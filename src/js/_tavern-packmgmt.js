  async function shelfPromoteToPack(slug, title) {
    shelfStatus('正在生成故事馆 Pack…');
    try {
      const data = await shelfApi('/novels/' + encodeURIComponent(slug) + '/to-pack', {
        method: 'POST',
        body: JSON.stringify({}),
      });
      const packId = data.packId;
      if (!packId) throw new Error(data.error || '未返回 packId');
      shelfStatus((data.existed ? '已有 Pack：' : '已生成 Pack：') + (data.title || title || packId));
      if (typeof switchTab === 'function') switchTab('tavern');
      if (typeof stLoadPacks === 'function') await stLoadPacks();
      if (typeof stShowPack === 'function') {
        try { await stShowPack(packId); } catch (_) {}
      }
      if (typeof stStatus === 'function') {
        stStatus('书架 → 故事馆：' + (data.title || title || packId) + ' · 可点「用此包开玩」');
      }
    } catch (e) {
      shelfStatus('进故事馆失败：' + (e.message || e));
    }
  }

  // ---- LLM 全流程转换：切章 → 角色蒸馏 → 世界树/节拍/出口/世界线 ----
  // 后台任务化：POST distil-world 立即返回 jobId；进度经 /api/v1/jobs/{id}/stream（SSE）订阅，
  // localStorage(shelfDistilJobs) 记录任务；刷新/切页面后可恢复订阅与进度，不打断浏览。
  const SHELF_DISTIL_JOBS_KEY = 'shelfDistilJobs';
  const _shelfWatchers = new Set();

  function shelfReadDistilJobs() {
    try {
      const raw = localStorage.getItem(SHELF_DISTIL_JOBS_KEY);
      const obj = raw ? JSON.parse(raw) : {};
      return (obj && typeof obj === 'object') ? obj : {};
    } catch (_) { return {}; }
  }
  function shelfWriteDistilJobs(obj) {
    try { localStorage.setItem(SHELF_DISTIL_JOBS_KEY, JSON.stringify(obj || {})); } catch (_) {}
  }
  function shelfDistilJobFor(slug) {
    return shelfReadDistilJobs()[slug] || null;
  }
  function shelfDropDistilJob(slug) {
    const obj = shelfReadDistilJobs();
    if (obj[slug]) {
      delete obj[slug];
      shelfWriteDistilJobs(obj);
    }
    shelfRenderDistilProgress(slug);
  }
  function shelfDistilActive(status) {
    return status === 'queued' || status === 'running' || status === 'pending';
  }

  // 渲染书架卡片的转换进度条；无该 job 时隐藏，并按进行中状态禁用「开始转换」按钮。
  function shelfRenderDistilProgress(slug) {
    const slots = document.querySelectorAll('.shelf-distil-progress');
    for (const slot of slots) {
      const s = slot.getAttribute('data-slug');
      if (slug && s !== slug) continue;
      const job = shelfDistilJobFor(s);
      const card = slot.closest('.shelf-card');
      const btn = card && card.querySelector('.shelf-play');
      if (!job) {
        slot.classList.add('hidden');
        slot.innerHTML = '';
        if (btn) { btn.disabled = false; btn.classList.remove('disabled'); btn.textContent = '开始转换'; }
        continue;
      }
      slot.classList.remove('hidden');
      const pct = Math.max(0, Math.min(100, Math.round((Number(job.progress) || 0) * 100)));
      const activeNow = shelfDistilActive(job.status);
      const stage = job.status === 'succeeded' ? '完成'
        : job.status === 'failed' || job.status === 'cancelled' ? '失败'
          : job.message || '转换中';
      slot.innerHTML =
        '<div class="shelf-distil-progress-bar"><div class="bar" style="width:' + pct + '%"></div></div>' +
        '<div class="shelf-distil-progress-stage">' + escapeHtml(stage) + ' · ' + pct + '%</div>' +
        shelfDistilActionHtml(job) +
        (job.status === 'succeeded' && job.report
          ? '<div class="shelf-distil-report-actions"><button type="button" class="shelf-distil-report-btn">📋 查看蒸馏报告</button></div>'
          : '');
      const rbtn = slot.querySelector('.shelf-distil-report-btn');
      if (rbtn) {
        rbtn.onclick = (ev) => {
          ev.preventDefault();
          ev.stopPropagation();
          showDistilReport(job.report, job.title || s);
        };
      }
      bindShelfDistilActions(slot, job);
      if (btn) {
        btn.disabled = !!activeNow;
        btn.classList.toggle('disabled', !!activeNow);
        btn.textContent = activeNow ? '转换中…' : job.status === 'succeeded' ? '转换完成' : '开始转换';
      }
    }
  }

  // ---- 蒸馏进度卡片操作按钮：暂停/继续/取消/重试 ----
  function shelfDistilPaused(job) {
    return job && (job.status === 'running' || job.status === 'queued')
      && job.message && job.message.indexOf('已暂停') >= 0;
  }

  function shelfDistilActionHtml(job) {
    const jid = job && job.jobId;
    if (!jid) return '';
    let html = '<div class="shelf-distil-actions">';
    if (shelfDistilPaused(job)) {
      html += '<button type="button" class="shelf-distil-ctl" data-action="resume">▶ 继续</button>';
      html += '<button type="button" class="shelf-distil-ctl" data-action="cancel">⏹ 取消</button>';
    } else if (shelfDistilActive(job.status)) {
      html += '<button type="button" class="shelf-distil-ctl" data-action="pause">⏸ 暂停</button>';
      html += '<button type="button" class="shelf-distil-ctl" data-action="cancel">⏹ 取消</button>';
    } else if (job.status === 'failed' || job.status === 'cancelled' || job.status === 'error') {
      html += '<button type="button" class="shelf-distil-ctl" data-action="retry">↻ 重试</button>';
    }
    html += '</div>';
    return html;
  }

  function bindShelfDistilActions(slot, job) {
    const jid = job && job.jobId;
    if (!jid) return;
    const actions = slot.querySelectorAll('.shelf-distil-ctl');
    for (const a of actions) {
      a.onclick = async (ev) => {
        ev.preventDefault();
        ev.stopPropagation();
        const action = a.getAttribute('data-action');
        if (!action) return;
        a.disabled = true;
        try {
          await api('/api/v1/jobs/' + encodeURIComponent(jid) + '/' + action, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({}),
          });
          shelfStatus(action === 'pause' ? '⏸ 已请求暂停，当前阶段完成后生效…'
            : action === 'cancel' ? '⏹ 已请求取消…'
              : action === 'retry' ? '↻ 已重新启动，从断点续跑…'
                : '▶ 已请求继续…');
          // 立即刷新一次进度，随后由 SSE/轮询驱动
          shelfRenderDistilProgress(slot.getAttribute('data-slug'));
        } catch (e) {
          a.disabled = false;
          shelfStatus((action === 'pause' ? '暂停失败：' : action === 'cancel' ? '取消失败：' : action === 'retry' ? '重试失败：' : '继续失败：') + ((e && (e.message || e.statusText)) || e));
        }
      };
    }
  }

  // ---- 蒸馏报告弹窗：展示转换完成后的各阶段产物清单 ----
  function buildDistilReportHtml(report) {
    const esc = (v) => escapeHtml(v == null ? '' : v);
    const blank = '<div class="st-distil-report-empty">—</div>';
    const list = (items) => {
      if (!items || !items.length) return blank;
      return '<ul class="st-distil-report-list">' + items.map((x) => '<li>' + x + '</li>').join('') + '</ul>';
    };
    let out = '';

    const chars = (report && report.characters) || [];
    out += '<h4 class="st-distil-report-sec">角色（' + chars.length + '）</h4>';
    out += chars.length
      ? '<ul class="st-distil-report-list">' + chars.map((c) => '<li><span class="st-distil-report-name">' + esc(c.name) + '</span>' + (c.role ? ' <span class="st-distil-report-meta">· ' + esc(c.role) + '</span>' : '') + '</li>').join('') + '</ul>'
      : blank;

    const lore = (report && report.lore) || [];
    out += '<h4 class="st-distil-report-sec">世界书（' + lore.length + '）</h4>';
    out += list(lore.map((v) => esc(v.title)));

    const beats = (report && report.beats) || {};
    out += '<h4 class="st-distil-report-sec">节拍与出口</h4>';
    out += '<div class="st-distil-report-stat">' +
      '<span>节点 ' + (Number(beats.node_count) || 0) + '</span>' +
      '<span>节拍 ' + (Number(beats.beat_count) || 0) + '</span>' +
      '<span>出口 ' + (Number(report.exits) || 0) + '</span>' +
      '</div>';

    const wl = (report && report.worldline) || [];
    out += '<h4 class="st-distil-report-sec">世界线（' + wl.length + '）</h4>';
    out += list(wl.map((v) => esc(v.title)));

    const eps = (report && report.event_packages) || [];
    out += '<h4 class="st-distil-report-sec">事件包（' + eps.length + '）</h4>';
    out += list(eps.map((p) => esc(p.name)));

    const templates = (report && report.actor_templates) || [];
    out += '<h4 class="st-distil-report-sec">演员模板（' + templates.length + '）</h4>';
    out += templates.length
      ? '<ul class="st-distil-report-list">' + templates.map((t) => '<li><span class="st-distil-report-name">' + esc(t.name) + '</span> <span class="st-distil-report-meta">· ' + (Number(t.field_count) || 0) + ' 字段</span></li>').join('') + '</ul>'
      : blank;

    const style = report && report.narrative_style;
    out += '<h4 class="st-distil-report-sec">文风</h4>';
    out += style ? '<div class="st-distil-report-style">' + esc(String(style).slice(0, 200)) + '</div>' : blank;

    const checks = (report && report.rule_checks) || [];
    out += '<h4 class="st-distil-report-sec">规则检定（' + checks.length + '）</h4>';
    out += checks.length
      ? '<ul class="st-distil-report-list">' + checks.map((c) => {
          const label = c.label || c.id || '检定';
          return '<li><span class="st-distil-report-name">' + esc(label) + '</span>' + (c.dice ? ' <span class="st-distil-report-meta">· ' + esc(c.dice) + '</span>' : '') + '</li>';
        }).join('') + '</ul>'
      : blank;

    return out;
  }

  function stCloseDistilReport() {
    const m = $('st-distil-report-modal');
    if (m) m.classList.add('hidden');
  }

  function showDistilReport(report, doneTitle) {
    let m = $('st-distil-report-modal');
    if (!m) {
      m = document.createElement('div');
      m.id = 'st-distil-report-modal';
      m.className = 'st-modal hidden';
      m.setAttribute('role', 'dialog');
      m.setAttribute('aria-modal', 'true');
      m.setAttribute('aria-label', '蒸馏报告');
      m.innerHTML =
        '<div class="st-modal-card st-distil-report-card">' +
        '<div class="st-modal-head">' +
        '<span class="st-modal-title st-distil-report-title"></span>' +
        '<button type="button" class="ghost st-modal-close" data-st-distil-close aria-label="关闭">✕</button>' +
        '</div>' +
        '<div class="st-modal-body st-distil-report-body"></div>' +
        '<div class="st-modal-foot">' +
        '<button type="button" class="primary" data-st-distil-close>关闭</button>' +
        '</div>' +
        '</div>';
      document.body.appendChild(m);
      m.addEventListener('click', (e) => {
        if (e.target === m || (e.target.closest && e.target.closest('[data-st-distil-close]'))) stCloseDistilReport();
      });
    }
    const head = m.querySelector('.st-distil-report-title');
    if (head) head.textContent = '蒸馏报告' + (doneTitle ? ' · ' + doneTitle : '');
    const body = m.querySelector('.st-distil-report-body');
    if (body) body.innerHTML = buildDistilReportHtml(report || {});
    m.classList.remove('hidden');
  }

  // 转换完成：清理 localStorage、刷新 Pack 列表、跳转故事馆（jump=false 时不跳转，仅提示）。
  async function finishDistilSuccess(slug, title, result, opts) {
    const jump = !(opts && opts.jump === false);
    let r = result || null;
    if (!r) {
      const job = shelfDistilJobFor(slug);
      if (job) {
        try {
          const data = await api('/api/v1/jobs/' + encodeURIComponent(job.jobId));
          if (data) r = data.result || null;
        } catch (_) {}
      }
    }
    const packId = (r && r.packId) || '';
    const doneTitle = (r && r.title) ? r.title : (title || slug);
    const report = (r && r.report) || null;
    if (report) {
      const obj = shelfReadDistilJobs();
      const prev = obj[slug] || {};
      obj[slug] = { jobId: prev.jobId || '', title: prev.title || title || slug, status: 'succeeded', progress: 1, report };
      shelfWriteDistilJobs(obj);
    } else {
      shelfDropDistilJob(slug);
    }
    let msg = '✅ 转换完成：' + doneTitle;
    if (r) {
      const bits = [];
      if (r.character_count != null) bits.push('角色 ' + r.character_count);
      if (r.beat_count != null) bits.push('节拍 ' + r.beat_count);
      if (r.lore_count != null) bits.push('传说 ' + r.lore_count);
      if (r.worldline_count != null) bits.push('世界线 ' + r.worldline_count);
      if (bits.length) msg += ' · ' + bits.join(' / ');
    }
    if (report) msg += ' · 📋 蒸馏报告';
    shelfStatus(msg);
    shelfRenderDistilProgress(slug);
    if (packId && typeof stLoadPacks === 'function') {
      try { await stLoadPacks(); } catch (_) {}
    }
    if (packId && jump && typeof switchTab === 'function') switchTab('tavern');
    if (packId && typeof stShowPack === 'function') {
      try { await stShowPack(packId); } catch (_) {}
    }
    if (typeof stStatus === 'function') {
      stStatus('🔮 转换完成，可点「用此包开玩」：' + (doneTitle || packId));
    }
  }

  function finishDistilFail(slug, title, msg) {
    shelfDropDistilJob(slug);
    shelfStatus('LLM 转换失败：' + (msg || title || slug));
  }

  // 订阅任务进度：SSE 优先 /api/v1/jobs/{id}/stream，异常时 fallback 每 3s 轮询。
  function watchDistilJob(jobId, opts) {
    if (!jobId || _shelfWatchers.has(jobId)) return Promise.resolve();
    _shelfWatchers.add(jobId);
    const slug = (opts && opts.slug) || '';
    const title = (opts && opts.title) || slug;
    return (async () => {
      const completeFrom = async (st, body) => {
        if (st === 'succeeded') {
          await finishDistilSuccess(slug, title, (body && body.result) || null);
        } else {
          finishDistilFail(slug, title, (body && (body.error || body.message)) || '');
        }
      };
      let streamed = false;
      try {
        for await (const ev of readSSE('/api/v1/jobs/' + encodeURIComponent(jobId) + '/stream')) {
          const j = ev && ev.json;
          if (!j) continue;
          streamed = true;
          const et = j.eventType || j.event_type || j.type || '';
          const st = String(j.status || '').toLowerCase();
          if (et === 'done' || et === 'error' || et === 'success'
              || st === 'succeeded' || st === 'failed' || st === 'cancelled' || st === 'error') {
            await completeFrom((et === 'error' || st === 'failed' || st === 'cancelled' || st === 'error') ? 'failed' : 'succeeded', j);
            return;
          }
          const p = (typeof j.progress === 'number') ? j.progress : null;
          const msg = j.message || j.progressMessage || '';
          const obj = shelfReadDistilJobs();
          if (obj[slug] && obj[slug].jobId === jobId) {
            obj[slug].status = 'running';
            if (p != null) obj[slug].progress = p;
            if (j.message) { obj[slug].message = j.message; }
            shelfWriteDistilJobs(obj);
          }
          shelfRenderDistilProgress(slug);
          if (msg) shelfStatus('⏳ ' + msg + (p != null ? ' · ' + Math.round(p * 100) + '%' : ''));
        }
      } catch (_e) {
        // SSE 异常 → 下方轮询兜底
      }
      if (!streamed) { /* SSE 未连上 → 轮询 */ }
      // Fallback: 每 3s 轮询 GET /api/v1/jobs/{jobId}
      const timer = setInterval(async () => {
        try {
          const data = await api('/api/v1/jobs/' + encodeURIComponent(jobId));
          const st = String((data && data.status) || '').toLowerCase();
          const p = (data && typeof data.progress === 'number') ? data.progress : null;
          const msg = (data && (data.progressMessage || data.error || '')) || '';
          if (st === 'succeeded') { clearInterval(timer); await completeFrom('succeeded', data); return; }
          if (st === 'failed' || st === 'cancelled' || st === 'error') { clearInterval(timer); await completeFrom('failed', data); return; }
          const obj = shelfReadDistilJobs();
          if (obj[slug] && obj[slug].jobId === jobId) {
            obj[slug].status = 'running';
            if (p != null) obj[slug].progress = p;
            if (msg) obj[slug].message = msg;
            shelfWriteDistilJobs(obj);
          }
          shelfRenderDistilProgress(slug);
          if (msg) shelfStatus('⏳ ' + msg + (p != null ? ' · ' + Math.round(p * 100) + '%' : ''));
        } catch (_) { /* 轮询失败继续重试 */ }
      }, 3000);
    })().finally(() => { _shelfWatchers.delete(jobId); });
  }

  // 刷新后恢复：扫描 localStorage 中的任务并核对服务端状态。
  async function shelfSyncDistilJobs() {
    const jobs = shelfReadDistilJobs();
    for (const slug of Object.keys(jobs)) {
      const job = jobs[slug];
      if (!job || !job.jobId) continue;
      let data = null;
      try {
        data = await api('/api/v1/jobs/' + encodeURIComponent(job.jobId));
      } catch (_) { /* offline → 保留记录，下次再试 */ }
      if (!data) { shelfRenderDistilProgress(slug); continue; }
      const st = String((data.status) || '').toLowerCase();
      if (st === 'succeeded') {
        await finishDistilSuccess(slug, job.title || slug, data.result || null, { jump: false });
      } else if (st === 'failed' || st === 'error' || st === 'cancelled') {
        finishDistilFail(slug, job.title || slug, data.error || st);
      } else {
        shelfRenderDistilProgress(slug);
        watchDistilJob(job.jobId, { slug, title: job.title }).catch(() => {});
      }
    }
  }

  // 触发后台转换；已存在的同作品任务直接恢复订阅/提示。
  async function shelfDistilWorld(slug, title) {
    const existing = shelfDistilJobFor(slug);
    if (existing && shelfDistilActive(existing.status)) {
      shelfStatus('该作品已有转换任务在运行，进度见卡片…');
      watchDistilJob(existing.jobId, { slug, title: title || existing.title }).catch(() => {});
      return;
    }
    shelfStatus('🔮 正在提交转换：角色蒸馏 → 世界树/节拍/出口/世界线 → 文风…');
    try {
      const data = await shelfApi('/novels/' + encodeURIComponent(slug) + '/distil-world', {
        method: 'POST',
        body: JSON.stringify({}),
      });
      const jobId = data.jobId || data.runId || data.id || data.run_id;
      if (data.error && !jobId) throw new Error(data.error || '提交转换失败');
      if (!jobId) throw new Error((data && data.error) || '未返回 jobId');
      const obj = shelfReadDistilJobs();
      obj[slug] = {
        jobId,
        title: title || slug,
        startedAt: new Date().toISOString(),
        status: 'queued',
        progress: 0.01,
        message: '已进入后台转换',
      };
      shelfWriteDistilJobs(obj);
      shelfRenderDistilProgress(slug);
      shelfStatus('⏳ 已进入后台转换（角色蒸馏→世界线→文风），可继续浏览，进度见卡片…');
      watchDistilJob(jobId, { slug, title: title || slug }).catch(() => {});
    } catch (e) {
      // 409：同作品已有转换任务 → 恢复其订阅
      if (e && e.status === 409 && e.body && e.body.jobId) {
        const jid = e.body.jobId;
        const obj = shelfReadDistilJobs();
        const prev = obj[slug];
        if (!prev || prev.jobId !== jid) {
          obj[slug] = { jobId: jid, title: title || slug, startedAt: new Date().toISOString(), status: 'running', progress: 0.02, message: '转换已在进行' };
          shelfWriteDistilJobs(obj);
        }
        shelfRenderDistilProgress(slug);
        shelfStatus('该作品已有转换任务在后台运行，进度见卡片…');
        watchDistilJob(jid, { slug, title: title || slug }).catch(() => {});
      } else {
        shelfStatus('LLM 转换失败：' + ((e && e.message) || e));
        shelfRenderDistilProgress(slug);
      }
    }
  }

  async function shelfExport(slug, title) {
    try {
      const url = apiBase() + '/api/v1/crawler/novels/' + encodeURIComponent(slug) + '/export';
      const a = document.createElement('a');
      a.href = url;
      a.download = (title || slug) + '.md';
      a.rel = 'noopener';
      document.body.appendChild(a);
      a.click();
      a.remove();
      shelfStatus('已开始导出：' + (title || slug));
    } catch (e) {
      shelfStatus('导出失败：' + (e.message || e));
    }
  }

  // ---- 编码嗅探读取文本文件：UTF-8 严格校验，失败回退 GB18030 ----
  // 修复 GBK/GB2312 源 txt 被 readAsText(默认utf-8) 强解成 � 乱码的问题
  async function stDecodeTextFile(file) {
    const buf = await new Promise((resolve, reject) => {
      const r = new FileReader();
      r.onload = () => resolve(r.result);
      r.onerror = () => reject(new Error('读取失败'));
      r.readAsArrayBuffer(file);
    });
    const bytes = new Uint8Array(buf);
    // 1) 严格 UTF-8 解码（失败=非 UTF-8 编码）
    try {
      return new TextDecoder('utf-8', { fatal: true }).decode(bytes);
    } catch (_) { /* not utf-8, fall through */ }
    // 2) 回退 GB18030（GBK/GB2312 超集，可全覆盖）
    try {
      return new TextDecoder('gb18030').decode(bytes);
    } catch (_) { /* last resort lossy utf-8 */ }
    return new TextDecoder('utf-8').decode(bytes);
  }

  async function shelfImportFile(file) {
    if (!file) return;
    shelfStatus('导入中：' + file.name + '…');
    const isDocx = typeof file.name === 'string' && /\.docx$/i.test(file.name);
    let text;
    if (isDocx) {
      shelfStatus('解析 DOCX：' + file.name + '…');
      const buf = await new Promise((resolve, reject) => {
        const r = new FileReader();
        r.onload = () => resolve(r.result);
        r.onerror = () => reject(new Error('读取失败'));
        r.readAsArrayBuffer(file);
      });
      const res = await mammoth.extractRawText({ arrayBuffer: buf });
      text = res && (res.value || '');
      if (!text || !text.trim()) { text = ''; shelfStatus('未从 DOCX 解析到文本'); return; }
    } else {
      text = await stDecodeTextFile(file);
    }
    const title = file.name.replace(/\.[^.]+$/, '');
    const data = await shelfApi('/novels', {
      method: 'POST',
      body: JSON.stringify({ text, title, toPack: true }),
    });
    if (!data || data.ok === false) throw new Error((data && data.error) || '导入失败');
    await loadBookshelf();
    shelfStatus('已上架「' + (data.title || title) + '」' +
      (data.chapterCount ? (' · ' + data.chapterCount + ' 章') : '') +
      (data.packId ? (' · Pack ' + data.packId) : ''));
    if (data.packId && typeof stLoadPacks === 'function') {
      try { await stLoadPacks(); } catch (_) {}
    }
    return data;
  }

  if ($('shelf-refresh')) $('shelf-refresh').onclick = () => loadBookshelf();
  if ($('shelf-chat-publish')) $('shelf-chat-publish').onclick = () => shelfPublishChat();
  if ($('shelf-sched-save')) $('shelf-sched-save').onclick = () => shelfSaveSchedule();
  if ($('shelf-sched-run')) $('shelf-sched-run').onclick = () => shelfRunScheduleNow();
  if ($('shelf-import-file')) {
    $('shelf-import-file').onchange = async (e) => {
      const file = e.target.files && e.target.files[0];
      if (!file) return;
      try {
        await shelfImportFile(file);
      } catch (err) {
        shelfStatus('导入失败：' + (err.message || err));
      } finally {
        e.target.value = '';
      }
    };
  }
  if ($('reader-back')) $('reader-back').onclick = closeShelfReader;
  if ($('reader-to-pack')) {
    $('reader-to-pack').onclick = () => {
      if (!shelfActiveSlug) return;
      const n = shelfNovels.find((x) => x.slug === shelfActiveSlug);
      shelfPromoteToPack(shelfActiveSlug, n && n.title);
    };
  }
  if ($('reader-export')) {
    $('reader-export').onclick = () => {
      if (!shelfActiveSlug) return;
      const n = shelfNovels.find((x) => x.slug === shelfActiveSlug);
      shelfExport(shelfActiveSlug, n && n.title);
    };
  }

  // load when switching to bookshelf tab
  const _origSwitchTabShelf = typeof switchTab === 'function' ? switchTab : null;
  // hook via tab button clicks too
  document.querySelectorAll('.tab[data-tab="bookshelf"], [data-tab="bookshelf"]').forEach((btn) => {
    btn.addEventListener('click', () => { setTimeout(loadBookshelf, 0); });
  });


  if ($('st-pack-demo')) {
    $('st-pack-demo').onclick = async () => {
      try {
        const before = (tavernPacks || []).some((p) => p.id === 'demo-rain-alley');
        const pack = await stApi('/packs/demo', { method: 'POST' });
        await stLoadPacks();
        // jump to 故事馆 so user sees the pack
        if (typeof switchTab === 'function') switchTab('tavern');
        if (typeof closeToolsSheet === 'function') closeToolsSheet();
        const title = (pack && pack.title) || '雨巷来客';
        stStatus(before
          ? ('新手引导：演示包「' + title + '」已在库中，可直接开玩')
          : ('新手引导：已安装演示包「' + title + '」· 左侧 Pack 库可见'));
        // highlight / open pack detail if helper exists
        if (pack && pack.id && typeof stShowPack === 'function') {
          try { await stShowPack(pack.id); } catch (_) {}
        }
      } catch (e) {
        stStatus('新手引导失败：' + e.message);
      }
    };
  }
  if ($('home-demo-card')) {
    $('home-demo-card').onclick = async () => {
      try {
        const before = (tavernPacks || []).some((p) => p.id === 'demo-rain-alley');
        const pack = await stApi('/packs/demo', { method: 'POST' });
        await stLoadPacks();
        if (typeof switchTab === 'function') switchTab('tavern');
        if (typeof closeToolsSheet === 'function') closeToolsSheet();
        const title = (pack && pack.title) || '雨巷来客';
        stStatus(before
          ? ('演示包「' + title + '」已在库中 · 选择玩法开玩')
          : ('已安装演示包「' + title + '」· 选择玩法开玩'));
        if (pack && pack.id && typeof stShowPack === 'function') {
          try { await stShowPack(pack.id); } catch (_) {}
        }
      } catch (e) {
        stStatus('一键开始失败：' + (typeof friendlyError === 'function' ? friendlyError(e) : e.message));
      }
    };
  }
  if ($('home-demo-start')) {
    $('home-demo-start').onclick = async () => {
      try {
        const before = (tavernPacks || []).some((p) => p.id === 'demo-rain-alley');
        if (!before) {
          await stApi('/packs/demo', { method: 'POST' });
          await stLoadPacks();
        }
        if (typeof switchTab === 'function') switchTab('tavern');
        if (typeof closeToolsSheet === 'function') closeToolsSheet();
      } catch (e) {
        stStatus('开始失败：' + (typeof friendlyError === 'function' ? friendlyError(e) : e.message));
      }
    };
  }
  if ($('home-continue-btn')) {
    $('home-continue-btn').addEventListener('click', () => {
      const has = (tavernSessions && tavernSessions.length) || 0;
      if (!has) {
        if (typeof switchTab === 'function') switchTab('chat');
      }
    });
  }
  document.querySelectorAll('#st-entry-cards .st-entry-card').forEach((card) => { card.onclick = () => stOpenWizard(card.dataset.playable, 'story-entry'); });
  if ($('st-pack-detail-play')) {
    $('st-pack-detail-play').onclick = () => {
      if (!tavernPack || !tavernPack.id) { stStatus('请先选择 Pack'); return; }
      // 在档案馆内直接打开创建向导（wizard 已在档案馆 DOM 里）
      const listview = $('st-packs-listview');
      const packDetail = $('st-view-pack');
      if (listview) listview.classList.add('hidden');
      if (packDetail) packDetail.classList.add('hidden');
      stOpenWizard('P1', 'packs-detail');
    };
  }
  if ($('st-pack-detail-back')) {
    $('st-pack-detail-back').onclick = () => {
      const listview = $('st-packs-listview');
      const packDetail = $('st-view-pack');
      if (listview) listview.classList.remove('hidden');
      if (packDetail) packDetail.classList.add('hidden');
      stStatus('档案馆 — 选择剧本包');
    };
  }
  if ($('st-pack-detail-compass')) {
    $('st-pack-detail-compass').onclick = () => {
      if (typeof switchTab === 'function') switchTab('works');
      setTimeout(() => {
        if (typeof switchAzView === 'function') switchAzView('dual-agent');
        if (typeof showToast === 'function') showToast('世界线/罗盘工具在作者区双 Agent / 关系图中');
      }, 80);
    };
  }
  $('st-wizard-create').onclick = stCreateSession;
  $('st-wizard-cancel').onclick = () => stCancelWizard(false);
  $('st-wizard-role').onchange = stWizardToggleRole;
  $('st-wizard-pack').onchange = stWizardToggleRole;
  $('st-composer').onsubmit = (e) => { e.preventDefault(); stSend($('st-input').value); };
  $('st-input').addEventListener('keydown', (e) => { if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); stSend($('st-input').value); } });
  $('st-stop').onclick = stStop;
  if ($('st-wand-btn')) {
    $('st-wand-btn').onclick = (e) => {
      e.preventDefault();
      const menu = $('st-wand-menu');
      const btn = $('st-wand-btn');
      if (!menu) return;
      const open = menu.classList.toggle('hidden');
      btn.setAttribute('aria-expanded', open ? 'false' : 'true');
      if (!open) {
        // 菜单高度按视口自适应:底贴 composer 顶向上,不能超出屏幕顶,内容可滚动
        const comp = $('st-composer');
        if (comp) {
          const top = Math.round(comp.getBoundingClientRect().top);
          menu.style.maxHeight = Math.max(160, top - 10) + 'px';
        }
        // 默认滚到最底:让"操作区+生图通道"这些常用动作立即可见
        requestAnimationFrame(() => { menu.scrollTop = menu.scrollHeight; });
      }
    };
  }
  // 点按钮后收起;select 选择完成(change)后收起,避免下拉展开即收起
  document.querySelectorAll('#st-wand-menu button').forEach((el) => {
    el.addEventListener('click', () => {
      if (el.id === 'st-vessel-toggle' || el.id === 'st-rot-toggle' || el.closest('#st-vessel-picker') || el.closest('.st-select-wrap')) return;
      const menu = $('st-wand-menu');
      const btn = $('st-wand-btn');
      if (menu && btn) { menu.classList.add('hidden'); btn.setAttribute('aria-expanded', 'false'); }
    });
  });
  document.querySelectorAll('#st-wand-menu select').forEach((el) => {
    el.addEventListener('change', () => {
      const menu = $('st-wand-menu');
      const btn = $('st-wand-btn');
      if (menu && btn) { menu.classList.add('hidden'); btn.setAttribute('aria-expanded', 'false'); }
    });
  });
  // 点外部关闭
  document.addEventListener('click', (e) => {
    const menu = $('st-wand-menu');
    const btn = $('st-wand-btn');
    if (!menu || menu.classList.contains('hidden')) return;
    if (btn && btn.contains(e.target)) return;
    if (menu.contains(e.target)) return;
    menu.classList.add('hidden');
    if (btn) btn.setAttribute('aria-expanded', 'false');
  });
  // 剧情助手：两个入口都打开独立助手弹窗（不再 prefill 主输入框、不再混入主线聊天流）
  ['st-magic-assist', 'st-magic-assist-btn'].forEach((mid) => {
    const btn = document.getElementById(mid);
    if (!btn) return;
    btn.onclick = (e) => {
      e.preventDefault();
      if (typeof stOpenAssistModal === 'function') stOpenAssistModal();
    };
  });
  // 可视化面板弹窗（魔棒菜单 → 可视化）+ 剧情助手弹窗绑定
  if ($('st-visual-btn')) $('st-visual-btn').onclick = (e) => { e.preventDefault(); stOpenVisualModal(); };
  if ($('st-visual-close')) $('st-visual-close').onclick = (e) => { e.preventDefault(); stCloseVisualModal(); };
  if ($('st-visual-gen')) $('st-visual-gen').onclick = (e) => { e.preventDefault(); stGenVisual(); };
  if ($('st-assist-close')) $('st-assist-close').onclick = (e) => { e.preventDefault(); stCloseAssistModal(); };
  if ($('st-assist-send')) $('st-assist-send').onclick = (e) => { e.preventDefault(); stSendAssist(); };
  if ($('st-assist-input')) $('st-assist-input').addEventListener('keydown', (e) => { if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); stSendAssist(); } });
  if ($('st-continue')) $('st-continue').onclick = (e) => { e.preventDefault(); stContinue(); };
  if ($('st-retry')) $('st-retry').onclick = (e) => { e.preventDefault(); stRetry().catch((err) => stStatus('重试失败：' + ((err && err.message) || err))); };
  // 生图 + 朗读（吸收自本机工具链：uniapi cogview-4 / edge-tts）
  if ($('st-image-btn')) $('st-image-btn').onclick = (e) => { e.preventDefault(); stGenerateImage().catch((err) => stStatus('生图失败：' + ((err && err.message) || err))); };
  if ($('st-sprite-btn')) $('st-sprite-btn').onclick = (e) => { e.preventDefault(); stGenerateSprite().catch((err) => stStatus('生成立绘失败：' + ((err && err.message) || err))); };
  if ($('st-tts-btn')) $('st-tts-btn').onclick = (e) => { e.preventDefault(); stSpeak().catch((err) => stStatus('朗读失败：' + ((err && err.message) || err))); };
  // P3 语音双工：录音抓取 + 语音输入开关（stVoiceInput 默认关防触误录音/权限弹窗打扰）
  const stAsrBtn = $('st-asr-btn');
  const stAsrToggle = $('st-asr-toggle');
  if (stAsrBtn) stAsrBtn.onclick = (e) => { e.preventDefault(); stToggleRecording().catch(() => {}); stSyncRecBtn(); };
  if (stAsrToggle) {
    const syncAsrToggle = () => {
      const on = localStorage.getItem('stVoiceInput') === '1';
      stAsrToggle.dataset.on = on ? '1' : '0';
      stAsrToggle.classList.toggle('is-on', on);
      stAsrToggle.title = on ? '语音双工：开（点击关闭）' : '语音双工：关（点击开启）';
    };
    stAsrToggle.onclick = (e) => {
      e.preventDefault();
      const on = localStorage.getItem('stVoiceInput') === '1';
      localStorage.setItem('stVoiceInput', on ? '0' : '1');
      syncAsrToggle();
      if (typeof stSyncRecBtn === 'function') stSyncRecBtn();
      stStatus(on ? '🎙 语音双工已关闭' : '🎙 语音双工已开启（说话即发送，回合尾自动朗读）');
    };
    syncAsrToggle();
    if (typeof stSyncRecBtn === 'function') stSyncRecBtn();
  }
  // P3 自动朗读开关：点击切换 + 初始化读取 localStorage
  const stAutoBtn = $('st-tts-auto');
  if (stAutoBtn) {
    const syncAutoBtn = () => {
      const on = localStorage.getItem('stAutoTts') === '1';
      stAutoBtn.dataset.on = on ? '1' : '0';
      stAutoBtn.classList.toggle('is-on', on);
      stAutoBtn.title = on ? '自动朗读：开（点击关闭）' : '自动朗读：关（点击开启）';
    };
    stAutoBtn.onclick = (e) => {
      e.preventDefault();
      const on = localStorage.getItem('stAutoTts') === '1';
      localStorage.setItem('stAutoTts', on ? '0' : '1');
      syncAutoBtn();
      stStatus(on ? '自动朗读已关闭' : '🔊 自动朗读已开启（回合结束自动朗读）');
    };
    syncAutoBtn();
  }
  if ($('st-tts-pause')) $('st-tts-pause').onclick = (e) => { e.preventDefault(); stTtsPauseToggle(); };
  if ($('st-tts-stop')) $('st-tts-stop').onclick = (e) => { e.preventDefault(); stTtsStop(); };
  // 生图通道自定义下拉(替代原生 select,随主题)展开/收���
  const stImgSelBtn = $('st-image-channel');
  if (stImgSelBtn) stImgSelBtn.onclick = () => {
    const list = $('st-image-channel-list');
    if (!list) return;
    const opening = list.classList.contains('hidden');
    list.classList.toggle('hidden', !opening);
    stImgSelBtn.setAttribute('aria-expanded', opening ? 'true' : 'false');
    if (opening) {
      list.querySelectorAll('.st-select-opt').forEach((o) =>
        o.classList.toggle('active', o.dataset.channel === stImgSelBtn.dataset.value));
      // 弹窗脱离菜单 overflow 裁剪:fixed 定位到视口,优先向下,空间不足向上,限制高度可滚
      const wr = stImgSelBtn.closest('.st-select-wrap') || stImgSelBtn;
      const r = wr.getBoundingClientRect();
      const listH = list.offsetHeight || 160;
      let top = Math.round(r.bottom + 4);
      if (top + listH > innerHeight - 8) top = Math.max(8, Math.round(r.top - listH - 4));
      list.style.position = 'fixed';
      list.style.left = Math.round(r.left) + 'px';
      list.style.top = top + 'px';
      list.style.right = 'auto';
      list.style.width = Math.round(r.width) + 'px';
      list.style.maxHeight = Math.max(120, innerHeight - top - 8) + 'px';
      list.style.overflowY = 'auto';
      list.style.zIndex = '150';
    } else {
      list.style.position = '';
      list.style.left = '';
      list.style.top = '';
      list.style.right = '';
      list.style.width = '';
      list.style.maxHeight = '';
      list.style.overflowY = '';
      list.style.zIndex = '';
    }
  };
  const stImgSelOpts = document.querySelectorAll('#st-image-channel-list .st-select-opt');
  stImgSelOpts.forEach((o) => {
    o.onclick = (e) => { e.preventDefault(); e.stopPropagation();
      const btn = $('st-image-channel');
      if (btn) { btn.dataset.value = o.dataset.channel; btn.textContent = o.textContent; btn.setAttribute('aria-expanded', 'false'); }
      const list = $('st-image-channel-list');
      if (list) list.classList.add('hidden');
      list.querySelectorAll('.st-select-opt').forEach((x) => x.classList.toggle('active', x === o));
    };
  });
  // 档位选择（st-writer-quality）
  const stQualitySelBtn = $('st-writer-quality');
  if (stQualitySelBtn) {
    const stQualityList = $('st-writer-quality-list');
    const stQualityOpts = stQualityList ? stQualityList.querySelectorAll('.st-select-opt') : [];
    const updateQuality = () => {
      const titles = { lite: '轻量', standard: '标准', heavy: '深度' };
      const v = stQualitySelBtn.dataset.value;
      try { localStorage.setItem('st-writer-quality', v || 'lite'); } catch (_) {}
      if (stQualitySelBtn.querySelector('.btn-lab')) stQualitySelBtn.querySelector('.btn-lab').textContent = titles[v] || '轻量';
    };
    stQualitySelBtn.addEventListener('click', () => {
      if (!stQualityList) return;
      const open = !stQualityList.classList.contains('hidden');
      stQualityList.classList.toggle('hidden', open);
      stQualitySelBtn.setAttribute('aria-expanded', String(!open));
    });
    stQualityOpts.forEach(o => {
      o.addEventListener('click', () => {
        stQualitySelBtn.dataset.value = o.dataset.quality;
        updateQuality();
        if (stQualityList) stQualityList.classList.add('hidden');
        stQualitySelBtn.setAttribute('aria-expanded', 'false');
        stStatus('档位：' + o.textContent.trim());
      });
    });
    try {
      const saved = localStorage.getItem('st-writer-quality');
      if (saved) { stQualitySelBtn.dataset.value = saved; updateQuality(); }
    } catch (_) {}
  }

  // 内容档位（st-content-tier）：会话级 contentTier 中段切换
  // 后端：POST /api/v1/story-tavern/sessions/{id}/tier（显式端点，放宽需 adultConfirmed）
  const stTierSelBtn = $('st-content-tier');
  if (stTierSelBtn) {
    const stTierList = $('st-content-tier-list');
    const stTierOpts = stTierList ? stTierList.querySelectorAll('.st-select-opt') : [];
    const TIER_TITLES = { safe: '安全', standard: '标准', open: '开放' };
    const updateTierBtn = () => {
      const v = stTierSelBtn.dataset.value || 'standard';
      const lab = stTierSelBtn.querySelector('.btn-lab');
      if (lab) lab.textContent = TIER_TITLES[v] || '标准';
    };
    const syncTierFromSession = () => {
      if (typeof tavernSession === 'undefined' || !tavernSession) return;
      const cur = (tavernSession.contentTier || '').toLowerCase();
      if (TIER_TITLES[cur]) {
        stTierSelBtn.dataset.value = cur;
        updateTierBtn();
      }
    };
    // 供 stLoadSession 跨文件调用：进入会话时刷新按钮档位
    window.stSyncTierFromSession = syncTierFromSession;
    stTierSelBtn.addEventListener('click', () => {
      if (!stTierList) return;
      const open = !stTierList.classList.contains('hidden');
      stTierList.classList.toggle('hidden', open);
      stTierSelBtn.setAttribute('aria-expanded', String(!open));
    });
    stTierOpts.forEach(o => {
      o.addEventListener('click', async () => {
        const want = o.dataset.tier;
        stTierSelBtn.dataset.value = want;
        updateTierBtn();
        if (stTierList) stTierList.classList.add('hidden');
        stTierSelBtn.setAttribute('aria-expanded', 'false');
        // 需要当前会话
        if (typeof tavernSession === 'undefined' || !tavernSession || !tavernSession.sessionId) {
          stStatus('内容档位：请先进入一个会话');
          return;
        }
        const sid = tavernSession.sessionId;
        // 放宽到 open 需要成年确认；未确认时先走确认
        if (want === 'open' && !adultOk()) {
          const ok = window.confirm('「开放」档包含成人内容。\n确认你已成年并自担风险？');
          if (!ok) {
            // 还原为会话当前档位
            syncTierFromSession();
            stStatus('已取消：未确认成年，内容档位保持不变');
            return;
          }
          await setAdultOk();
        }
        try {
          const r = await stApi('/sessions/' + encodeURIComponent(sid) + '/tier', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ contentTier: want, adultConfirmed: !!adultOk() }),
          });
          if (r && r.sessionId) tavernSession = r;
          syncTierFromSession();
          stStatus('内容档位已切换为「' + TIER_TITLES[stTierSelBtn.dataset.value] + '」');
        } catch (e) {
          stTierSelBtn.dataset.value = (tavernSession && tavernSession.contentTier) || 'standard';
          updateTierBtn();
          stStatus('内容档位切换失败：' + (e && e.message ? e.message : '未知错误'));
        }
      });
    });
    syncTierFromSession();
  }

  // 叙述视角（st-narr-pov）：会话级 /config pov=first|third 覆盖蒸馏文风人称
  // 后端：POST /sessions/{id}/assistant {message:"/config pov=first"} → flags 持久化 → 注入 system prompt
  const stPovBtn = $('st-narr-pov');
  if (stPovBtn) {
    const stPovList = $('st-narr-pov-list');
    const stPovOpts = stPovList ? stPovList.querySelectorAll('.st-select-opt') : [];
    const POV_TITLES = { '': '默认（跟随作品）', first: '第一人称（我）', third: '第三人称（他/她）' };
    const POV_LABELS = { '': '默认', first: '第一人称', third: '第三人称' };
    const updatePovBtn = () => {
      const v = stPovBtn.dataset.value || '';
      const lab = stPovBtn.querySelector('.btn-lab');
      if (lab) lab.textContent = POV_LABELS[v] || '默认';
    };
    stPovBtn.addEventListener('click', () => {
      if (!stPovList) return;
      const open = !stPovList.classList.contains('hidden');
      stPovList.classList.toggle('hidden', open);
      stPovBtn.setAttribute('aria-expanded', String(!open));
    });
    stPovOpts.forEach(o => {
      o.addEventListener('click', async () => {
        const want = o.dataset.pov || '';
        if (typeof tavernSession === 'undefined' || !tavernSession || !tavernSession.sessionId) {
          stStatus('叙述视角：请先进入一个会话');
          return;
        }
        const sid = tavernSession.sessionId;
        stPovBtn.dataset.value = want;
        updatePovBtn();
        if (stPovList) stPovList.classList.add('hidden');
        stPovBtn.setAttribute('aria-expanded', 'false');
        try {
          const cmd = want ? ('/config pov=' + want) : '/config pov=default';
          const r = await stApi('/sessions/' + encodeURIComponent(sid) + '/assistant', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ message: cmd }),
          });
          stStatus('叙述视角已设为「' + POV_LABELS[want] + '」');
        } catch (e) {
          stPovBtn.dataset.value = '';
          updatePovBtn();
          stStatus('叙述视角设置失败：' + (e && e.message ? e.message : '未知错误'));
        }
      });
    });
    updatePovBtn();
  }

  // S8.17: bind imm chrome listeners early (safe if elements missing)
  try { stBindImmChrome(); } catch (_) {}


  // ----- ST-4 Pack production / novel import helpers -----
  function stMakeId(prefix) { return prefix + '-' + Math.random().toString(36).slice(2, 9); }
  function stSlugify(s) { return String(s).toLowerCase().replace(/[^a-z0-9\u4e00-\u9fa5]+/g, '-').replace(/^-+|-+$/g, '').slice(0,40) || 'pack'; }

  function stSplitChapters(text) {
    // 仅匹配行首独立标题：## 第N章 / 行首第N章 / Chapter N
    // （排除正文内嵌缩进段落如「　　第一章」）
    const re = /(?:^|\n)(?:#{1,3}\s*第\s*[一二三四五六七八九十零百千万\d]+\s*[章节]|第\s*[一二三四五六七八九十零百千万\d]+\s*[章节]|Chapter\s+\d+|CHAPTER\s+\d+)[\s\t]*[:：]?\s*(.+?)?(?=\n|$)/gm;
    const matches = []; let m;
    while ((m = re.exec(text)) !== null) {
      const idx = m.index + (m[0].startsWith('\n') ? 1 : 0);
      const titleRaw = (m[0].replace(/^\s*\n?/, '') || '').replace(/^第\s*/, '第');
      const title = m[1] ? (m[1].slice(0,120).trim() || titleRaw) : titleRaw;
      matches.push({ idx, raw: m[0], title });
    }
    if (matches.length < 2) {
      // Fallback word window: split every ~2500 chars
      const window = 2400;
      const parts = []; let pos = 0;
      while (pos < text.length) {
        let breakAt = Math.min(pos + window, text.length);
        if (breakAt < text.length) {
          let nearest = text.lastIndexOf('\n', breakAt);
          if (nearest > pos + window * 0.5) breakAt = nearest;
        }
        parts.push({ idx: pos, title: '第' + (parts.length + 1) + '章', raw: '' });
        pos = breakAt;
      }
      parts.forEach((p, i) => { p.end = (parts[i + 1] ? parts[i + 1].idx : text.length); });
      return parts.map(p => ({ ...p, content: text.slice(p.idx, p.end).trim() }));
    }
    const chapters = [];
    matches.forEach((match, i) => {
      const end = (matches[i + 1] ? matches[i + 1].idx : text.length);
      chapters.push({ idx: match.idx, title: match.title, content: text.slice(match.idx, end).trim() });
    });
    return chapters;
  }

  function stBuildPackFromNovel(title, chapters) {
    const packId = 'pack-' + Date.now();
    const now = new Date().toISOString();
    const chars = [
      { id: 'c-' + stMakeId('n'), name: '旁白', role: 'narrator', personality: '旁白', speechStyle: '' },
      { id: 'c-' + stMakeId('p'), name: '读者', role: 'player', personality: '你自己', speechStyle: '' }
    ];
    // Scan for potential named characters via simple pattern (filter narrative junk)
    const seen = new Set(['旁白', '读者', '玩家', 'narrator']);
    const junkRe = /^(露出|眼角|换鞋|随口|轻声|低头|抬起|转身|伸手|走过去|看向|听见|突然|只是|已经|然后|因为|所以)/;
    const nameRe = /([^\s，。！？、；：""''（）\(\)]{2,4})(?:说|道|问|答|喊|叫)/g;
    for (const ch of chapters) {
      let mm; while ((mm = nameRe.exec(ch.content)) !== null) {
        const n = String(mm[1] || '').trim();
        if (!n || seen.has(n)) continue;
        if (n.length < 2 || n.length > 4) continue;
        if (junkRe.test(n)) continue;
        if (/[的了着在把被会就还也都很]/.test(n)) continue;
        if (!/^[\u4e00-\u9fff·]+$/.test(n)) continue;
        if (seen.size >= 8) break;
        seen.add(n);
        chars.push({ id: 'c-' + stMakeId('c'), name: n, role: 'supporting', personality: '', speechStyle: '' });
      }
    }

    const storyChapters = [];
    const nodes = [];
    chapters.forEach((ch, i) => {
      const chId = 'ch' + String(i + 1).padStart(2, '0');
      const nodeId = 'n' + (i + 1);
      storyChapters.push({ id: chId, title: ch.title, order: i + 1, goals: [], nodeIds: [nodeId], bodyPath: 'chapters/' + chId + '.md' });
      const exits = [];
      if (i + 1 < chapters.length) exits.push({ id: 'e' + (i + 1), when: '继续', next: 'n' + (i + 2) });
      nodes.push({ id: nodeId, chapterId: chId, title: ch.title, entry: '本章开始', exit: exits, lockedBeats: [], allowedDivergence: 'branch', presentCharacters: chars.slice(2).map(c => c.id), summary: (ch.content || '').slice(0, 400) });
    });

    return {
      id: packId,
      title: title || ('导入：' + now.slice(0, 10)),
      source: { type: 'novel', refs: [] },
      characters: chars,
      worldBookIds: [],
      chapters: storyChapters,
      nodes: nodes,
      loreEntries: [],
      defaultMode: 'mainline',
      maxTier: 'standard',
      language: 'zh',
      createdAt: now,
      updatedAt: now,
      uploadChapters: chapters,
    };
  }

  async function stImportNovel(file, title) {
    const text = await stDecodeTextFile(file);
    const chapters = stSplitChapters(text);
    if (chapters.length < 1) throw new Error('未识别到章节');
    const pack = stBuildPackFromNovel(title || file.name.replace(/\.[^.]+$/, ''), chapters);
    // First create pack, then write chapter bodies
    const saved = await stApi('/packs', { method: 'POST', body: JSON.stringify(pack) });
    for (const ch of pack.uploadChapters) {
      const chId = saved.chapters.find(c => c.title === ch.title)?.id;
      if (chId) {
        const rel = 'chapters/' + chId + '.md';
        await stApi('/packs/' + encodeURIComponent(saved.id) + '/chapters/' + encodeURIComponent(rel), { method: 'PUT', body: JSON.stringify({ content: ch.content }) });
      }
    }
    return saved;
  }

  async function stCreateEmptyPack() {
    const title = ($('st-pack-title').value || '').trim();
    if (!title) { showToast('请输入标题', 'warning'); return; }
    const packId = 'pack-' + stSlugify(title) + '-' + Date.now().toString().slice(-4);
    const now = new Date().toISOString();
    const pack = {
      id: packId, title,
      source: { type: 'manual', refs: [] },
      characters: [{ id: 'c-player', name: '玩家', role: 'player', personality: '', speechStyle: '' }],
      worldBookIds: [],
      chapters: [
        { id: 'ch01', title: '第一章', order: 1, goals: ['开场'], nodeIds: ['n1'], bodyPath: 'chapters/ch01.md' },
        { id: 'ch02', title: '第二章', order: 2, goals: ['推进'], nodeIds: ['n2'], bodyPath: 'chapters/ch02.md' }
      ],
      nodes: [
        { id: 'n1', chapterId: 'ch01', title: '开局', entry: '故事从这里开始', exit: [{ id: 'e1', when: '继续', next: 'n2' }], lockedBeats: [], allowedDivergence: 'branch', presentCharacters: [], summary: '' },
        { id: 'n2', chapterId: 'ch02', title: '推进', entry: '情节推进', exit: [], lockedBeats: [], allowedDivergence: 'branch', presentCharacters: [], summary: '' }
      ],
      loreEntries: [],
      defaultMode: 'mainline',
      maxTier: 'standard',
      language: 'zh',
      createdAt: now,
      updatedAt: now,
    };
    await stApi('/packs', { method: 'POST', body: JSON.stringify(pack) });
    // Write empty chapter bodies
    await stApi('/packs/' + packId + '/chapters/' + encodeURIComponent('chapters/ch01.md'), { method: 'PUT', body: JSON.stringify({ content: '（在此粘贴第一章正文）' }) });
    await stApi('/packs/' + packId + '/chapters/' + encodeURIComponent('chapters/ch02.md'), { method: 'PUT', body: JSON.stringify({ content: '（在此粘贴第二章正文）' }) });
    return packId;
  }

