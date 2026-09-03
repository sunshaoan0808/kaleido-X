/**
 * src/js/jobs.js — Jobs/任务中心 + background/book-travel/online-load/st-import
 * 域，真 ESM 模块（P1-3 S2.11；原 IIFE 片 _jobs-part.js，1268L）。
 *
 * 出边（Mechanism Y，converted[] import 行供剩余闭包片使用）：
 *   setPanel   — _agent/_partner/_story 的面板输出助手
 *   refreshJobs — _tabs 切到 jobs 页时刷新
 * 入边处理：
 *   $ api apiBase showToast friendlyError → 直接 import
 *   token（原闭包 let）→ window.__kaleidoAuthState.token 门面（readSSE 内本地化）
 *   loadPartner → window.__kaleidoPartner 门面（S2.8 已有）
 *   renderAsVisual → window.__kaleidoAgent 门面（本步新增，_agent-part 发布）
 * 顶层装配语句 → initJobsUI()，由 main.js 在闭包求值后调用。
 */
import { $ } from './dom.js';
import { api } from './api.js';
import { apiBase, friendlyError } from './api_shell.js';
import { showToast } from './toast.js';
import { partner } from './state_core.js';

/** closure-state accessors */
const __authToken = () => window.__kaleidoAuthState.token;
async function __loadPartner() {
  const f = window.__kaleidoPartner && window.__kaleidoPartner.loadPartner;
  if (f) { try { await f(); } catch (_) {} }
}
function __renderAsVisual(data) {
  const f = window.__kaleidoAgent && window.__kaleidoAgent.renderAsVisual;
  if (f) f(data);
}

  function pretty(v) {
    try {
      return typeof v === 'string' ? v : JSON.stringify(v, null, 2);
    } catch (_) {
      return String(v);
    }
  }

  function trimJobsOut(out) {
    const MAX = 2_000_000; // ~2MB text cap
    const s = out.textContent;
    if (s.length <= MAX) return;
    const idx = s.indexOf('\n', s.length - MAX);
    out.textContent = '…（已截断较早输出）\n' + s.slice(idx >= 0 ? idx + 1 : s.length - MAX);
  }

  function setPanel(outId, msgId, data, err) {
    const out = $(outId);
    const msg = $(msgId);
    if (err) {
      const friendly = friendlyError(err);
      const status = err.status != null ? ('HTTP ' + err.status + ' · ') : '';
      if (out) out.textContent = status + (err.body || friendly);
      if (msg) msg.textContent = '错误：' + friendly;
      return;
    }
    if (out) out.textContent = pretty(data);
    if (msg) msg.textContent = '成功';
  }

  async function* readSSE(path) {
    const token = __authToken();
    const headers = { Accept: 'text/event-stream' };
    if (token) {
      headers['Authorization'] = 'Bearer ' + token;
      headers['X-Mobile-Token'] = token;
    }
    const res = await fetch(apiBase() + path, { headers, cache: 'no-store' });
    if (!res.ok) {
      let body = '';
      try { body = await res.text(); } catch (_) {}
      const err = new Error('SSE HTTP ' + res.status);
      err.status = res.status;
      err.body = body;
      throw err;
    }
    if (!res.body || !res.body.getReader) {
      // Fallback: whole body as one chunk
      const text = await res.text();
      yield { raw: text, json: null };
      return;
    }
    const reader = res.body.getReader();
    const decoder = new TextDecoder('utf-8');
    let buf = '';
    while (true) {
      const { value, done } = await reader.read();
      if (done) break;
      buf += decoder.decode(value, { stream: true });
      let idx;
      while ((idx = buf.indexOf('\n')) >= 0) {
        let line = buf.slice(0, idx);
        buf = buf.slice(idx + 1);
        if (line.endsWith('\r')) line = line.slice(0, -1);
        if (!line) continue;
        if (line.startsWith(':')) continue; // comment / keepalive
        let data = null;
        if (line.startsWith('data:')) {
          data = line.slice(5).trimStart();
        } else if (line.startsWith('data :')) {
          data = line.slice(6).trimStart();
        } else {
          continue;
        }
        if (data === '[DONE]') {
          yield { raw: data, json: { eventType: 'done' } };
          return;
        }
        let json = null;
        try { json = JSON.parse(data); } catch (_) {}
        yield { raw: data, json };
      }
    }
  }

  function appendStreamLine(outId, ev) {
    const out = $(outId);
    if (!out) return;
    const j = ev && ev.json;
    if (j && (j.eventType === 'delta' || j.event_type === 'delta')) {
      const d = (j.data && (j.data.delta || j.data.text)) || j.delta || '';
      if (d) {
        // live token append
        if (out.dataset.streamMode !== '1') {
          out.dataset.streamMode = '1';
          out.textContent = '';
        }
        out.textContent += d;
        trimJobsOut(out);
        out.scrollTop = out.scrollHeight;
        return;
      }
    }
    // structured event / progress / done — pretty print line
    const line = j ? pretty(j) : String((ev && ev.raw) || '');
    if (!line) return;
    if (out.dataset.streamMode === '1') {
      out.textContent += '\n' + line + '\n';
    } else {
      out.textContent = (out.textContent ? out.textContent + '\n' : '') + line;
    }
    trimJobsOut(out);
    out.scrollTop = out.scrollHeight;
  }

  async function refreshJobs() {
    try {
      const data = await api('/api/v1/jobs?limit=50');
      const jobs = data.jobs || [];
      $('jobs-summary').textContent =
        'count=' + (data.count != null ? data.count : jobs.length) +
        ' · running=' + (data.running != null ? data.running : '?') +
        ' · 队列=' + (data.queued != null ? data.queued : '?') +
        ' · maxConcurrent=' + (data.maxConcurrent != null ? data.maxConcurrent : '?');
      const lines = jobs.map((j) => {
        const id = j.runId || j.id || j.run_id || '';
        const kind = j.kind || '';
        // S13g: status 本地化（原来直接拼英文原始 status）
        const status = JOB_STATUS_LABELS[j.status] || j.status || '';
        const prog = j.progress != null ? (' p=' + j.progress) : '';
        // U11: 回合耗时/成本记账提示（tavern-turn payload.u11，GET /api/v1/jobs 可见）
        let u11 = '';
        const p11 = j.payload && j.payload.u11;
        if (p11 && typeof p11 === 'object') {
          const dur = p11.durationMs != null ? (Math.round(Number(p11.durationMs) / 1000) + 's') : '';
          const calls = p11.llmCalls != null ? ('llm=' + p11.llmCalls) : '';
          const cost = p11.estCostUsd != null && Number(p11.estCostUsd) > 0 ? ('$' + Number(p11.estCostUsd).toFixed(4)) : '';
          const resume = p11.resumed ? 'resumed' : '';
          const bits = [dur, calls, cost, resume].filter(Boolean);
          if (bits.length) u11 = '  [' + bits.join(' · ') + ']';
        }
        return id + '  ' + kind + '  ' + status + prog + u11;
      });
      // Human-readable list only — full JSON per job is on demand via 详情 (job-detail)
      $('jobs-out').textContent = lines.length ? lines.join('\n') : '（暂无任务）';
      $('jobs-msg').textContent = '成功 · 已加载 ' + jobs.length + ' 条';
    } catch (e) {
      setPanel('jobs-out', 'jobs-msg', null, e);
    }
  }

  function syncBgStageUi() {
    const mode = ($('bg-mode') && $('bg-mode').value) || 'pipeline';
    const needCard = mode === 'character_card';
    if ($('bg-char-wrap')) $('bg-char-wrap').classList.toggle('hidden', !needCard);
    if ($('bg-wbctx-wrap')) $('bg-wbctx-wrap').classList.toggle('hidden', !needCard);
    const pipe = mode === 'pipeline';
    if ($('bg-stage-steps')) {
      $('bg-stage-steps').classList.toggle('hidden', !pipe && mode === 'character_card');
    }
  }

  function bgStartBody() {
    const mode = ($('bg-mode') && $('bg-mode').value) || 'pipeline';
    const body = {
      title: ($('bg-title').value || '').trim() || undefined,
      premise: ($('bg-premise').value || '').trim() || undefined,
      text: ($('bg-premise').value || '').trim() || undefined,
      mode,
    };
    if (mode === 'character_card') {
      body.characterName = ($('bg-char-name').value || '').trim() || undefined;
      body.worldBookContext = ($('bg-wb-context').value || '').trim() || undefined;
    }
    return body;
  }

  function rememberBgResult(data, runId) {
    if (!data || typeof data !== 'object') return;
    const rid = runId || data.runId || data.id || data.run_id || '';
    if (rid) window.__bgLastRunId = rid;
    const payload =
      data.result ||
      (data.worldBooks || data.characterCards || data.characterCard || data.pipeline ? data : null);
    if (payload) window.__bgLastResult = payload;
  }

  function bgShowProgress(show) {
    const w = $('bg-progress-wrap');
    if (!w) return;
    if (show) w.removeAttribute('hidden');
    else w.setAttribute('hidden', '');
  }

  function bgSetProgress(p, label, stageKey) {
    bgShowProgress(true);
    const pct = Math.max(0, Math.min(100, Math.round((Number(p) || 0) * 100)));
    if ($('bg-progress-bar')) {
      const bar = $('bg-progress-bar');
      // scaleX-driven (transform) with width fallback — no layout thrash
      bar.style.width = '100%';
      bar.style.setProperty('--bg-progress', (pct / 100).toFixed(3));
    }
    if ($('bg-progress-label')) {
      $('bg-progress-label').textContent = (label || ('进度 ' + pct + '%')) + (pct ? ' · ' + pct + '%' : '');
    }
    const steps = $('bg-stage-steps');
    if (steps) {
      const order = ['stage_one', 'items', 'character_card', 'done'];
      let active = stageKey || '';
      if (active.startsWith('pipeline:')) active = active.split(':')[1] || active;
      if (active.includes('done') || pct >= 99) active = 'done';
      const idx = order.indexOf(active);
      steps.querySelectorAll('li').forEach((li) => {
        const s = li.getAttribute('data-stage');
        const si = order.indexOf(s);
        li.classList.remove('is-active', 'is-done');
        if (idx >= 0 && si >= 0 && si < idx) li.classList.add('is-done');
        if (s === active) li.classList.add('is-active');
        if (active === 'done') li.classList.add('is-done');
      });
    }
  }

  function bgParseStreamEvent(ev) {
    const j = ev && ev.json;
    if (!j) return;
    const msg = j.message || j.progressMessage || (j.data && j.data.message) || '';
    const prog =
      (typeof j.progress === 'number' && j.progress) ||
      (j.data && typeof j.data.progress === 'number' && j.data.progress) ||
      null;
    let stage =
      (j.data && (j.data.stage || j.data.mode)) ||
      j.stage ||
      j.mode ||
      '';
    const et = j.eventType || j.type || j.event_type || '';
    if (typeof j.progressMessage === 'string' && j.progressMessage.includes(':')) {
      // e.g. pipeline:items:start
      const parts = j.progressMessage.split(':');
      if (parts[0] === 'pipeline' && parts[1]) stage = parts[1];
    }
    if (prog != null) bgSetProgress(prog, msg || et || '生成中', stage);
    else if (msg) {
      if ($('bg-progress-label')) $('bg-progress-label').textContent = msg;
    }
    if (j.result) rememberBgResult(j, window.__bgLastRunId);
    else if (j.data && (j.data.worldBooks || j.data.characterCards || j.data.characterCard || j.data.result)) {
      rememberBgResult(j.data.result || j.data, window.__bgLastRunId);
    } else if (j.worldBooks || j.characterCards || j.characterCard) {
      rememberBgResult(j, window.__bgLastRunId);
    }
    if (et === 'error' || j.eventType === 'error') {
      if ($('bg-retry')) $('bg-retry').hidden = false;
      if ($('bg-msg')) $('bg-msg').textContent = '生成失败 · 可点「失败重试」';
    }
    if (et === 'done' || j.eventType === 'done') {
      bgSetProgress(1, '已完成', 'done');
    }
  }

  async function bgFollowStream(id) {
    if (!id || window.__bgStreaming) return;
    window.__bgStreaming = true;
    try {
      if ($('bg-out')) $('bg-out').textContent = '';
      if ($('bg-msg')) $('bg-msg').textContent = '跟踪进度中…';
      bgSetProgress(0.05, '已排队/启动', 'stage_one');
      for await (const ev of readSSE('/api/v1/background/stream?id=' + encodeURIComponent(id))) {
        appendStreamLine('bg-out', ev);
        bgParseStreamEvent(ev);
        const j = ev.json;
        if (j && (j.eventType === 'done' || j.eventType === 'error' || j.type === 'done' || j.type === 'error')) {
          break;
        }
      }
      // pull final job if result missing
      if (!window.__bgLastResult) {
        try {
          const job = await api('/api/v1/jobs/' + encodeURIComponent(id));
          if (job && job.result) rememberBgResult(job, id);
          if (job && (job.status === 'succeeded' || job.status === 'done')) bgSetProgress(1, '已完成', 'done');
          if (job && (job.status === 'failed' || job.status === 'error')) {
            if ($('bg-retry')) $('bg-retry').hidden = false;
          }
        } catch (_) {}
      } else {
        bgSetProgress(1, '已完成', 'done');
      }
      if ($('bg-msg') && window.__bgLastResult) {
        const r = window.__bgLastResult;
        const wb = (r.worldBooks || []).length;
        const cc = (r.characterCards || []).length;
        $('bg-msg').textContent =
          '生成结束 · 世界书 ' + wb + ' · 角色卡 ' + cc +
          (r.pipeline ? ' · 流水线' : '') +
          ' · 可「写入角色/世界」';
      } else if ($('bg-msg')) {
        $('bg-msg').textContent = 'SSE 结束';
      }
    } catch (e) {
      if ($('bg-msg')) $('bg-msg').textContent = '跟踪失败：' + (e.message || e);
      if ($('bg-retry')) $('bg-retry').hidden = false;
    } finally {
      window.__bgStreaming = false;
    }
  }

  async function runBgStart(url) {
    const body = bgStartBody();
    if (body.mode === 'character_card' && !body.characterName) {
      throw new Error('单卡阶段需要角色名');
    }
    if (!(body.premise || body.text)) {
      // allow empty → server default premise, but warn
      if ($('bg-msg')) $('bg-msg').textContent = '未填 premise，将使用默认提示词';
    }
    window.__bgLastBody = body;
    if ($('bg-retry')) $('bg-retry').hidden = true;
    const data = await api(url, {
      method: 'POST',
      body: JSON.stringify(body),
    });
    const rid = data.runId || data.id || data.run_id || '';
    if (rid) $('bg-run-id').value = rid;
    if (rid) window.__bgLastRunId = rid;
    setPanel('bg-out', 'bg-msg', data);
    bgSetProgress(0.02, '已创建任务 ' + (data.stage || body.mode || ''), body.mode === 'pipeline' ? 'stage_one' : (body.mode || ''));
    const auto = !$('bg-auto-stream') || $('bg-auto-stream').checked;
    if (auto && rid) {
      // fire-and-forget follow
      bgFollowStream(rid);
    }
    return data;
  }

  function btChainEnabled() {
    return !($('bt-chain') && !$('bt-chain').checked);
  }

  function rememberBtResult(data, runId) {
    if (!data || typeof data !== 'object') return;
    const rid = runId || data.runId || data.id || data.run_id || '';
    if (rid) window.__btLastRunId = rid;
    const payload = data.result || data;
    // Avoid stashing pure start-ack without useful content
    if (payload && (payload.kind || payload.step || payload.mode || payload.scene || payload.plan || payload.memory || payload.ending || payload.text || payload.ok)) {
      window.__btLastResult = payload.result || payload;
    }
    if (data.labels) window.__btLastLabels = data.labels;
    if (payload && payload.labels) window.__btLastLabels = payload.labels;
  }

  function updateBtChainHint() {
    const el = $('bt-chain-hint');
    if (!el) return;
    if (!btChainEnabled()) {
      el.textContent = '串联已关闭';
      return;
    }
    if (window.__btLastRunId) {
      el.textContent = '将自动带入上一步结果 · ' + String(window.__btLastRunId).slice(0, 8);
    } else {
      el.textContent = '将自动带入上一步结果';
    }
  }

  function btBody(step) {
    const b = {
      title: ($('bt-title') && $('bt-title').value || '').trim() || undefined,
      text: ($('bt-text') && $('bt-text').value || '').trim() || undefined,
      premise: ($('bt-text') && $('bt-text').value || '').trim() || undefined,
      userInput: ($('bt-user-input') && $('bt-user-input').value || '').trim() || undefined,
      step: step || (($('bt-step') && $('bt-step').value) || 'assemble'),
      mode: step || (($('bt-step') && $('bt-step').value) || 'assemble'),
    };
    if (window.__btLastLabels) b.labels = window.__btLastLabels;
    if (btChainEnabled()) {
      if (window.__btLastResult) b.context = window.__btLastResult;
      if (window.__btLastRunId) b.previousRunId = window.__btLastRunId;
    }
    return b;
  }

  async function btStart(path) {
    const step = ($('bt-step') && $('bt-step').value) || 'assemble';
    const data = await api(path, {
      method: 'POST',
      body: JSON.stringify(btBody(step)),
    });
    const rid = data.runId || data.id || data.run_id || '';
    if (rid && $('bt-run-id')) $('bt-run-id').value = rid;
    // Do not overwrite __btLastRunId here — chain should point at last *completed* step.
    setPanel('bt-out', 'bt-msg', data);
    updateBtChainHint();
    return data;
  }

  const ST_FIXTURE = {
    spec: 'chara_card_v2',
    spec_version: '2.0',
    data: {
      name: '夜风咏叹调',
      description: 'A wandering mage from the northern isles. Keeps a pocket full of storm-glass charms.',
      personality: 'Calm, dry humor, fiercely loyal.',
      scenario: 'You meet in a storm-battered tavern on the edge of the Black Coast.',
      first_mes: 'The door slams. *Aria shakes rain from her cloak and eyes the empty stool.* "Need a table, or just the weather report?"',
      mes_example: '{{user}}: Who are you?\n{{char}}: *smirks* Aria. The rest costs a drink.',
      tags: ['fantasy', 'mage', 'tavern'],
      creator: 'kaleido-fixture',
      character_version: '1.1.0',
      creator_notes: 'Sample fixture with character_book + regex_scripts.',
      system_prompt: 'Stay in character as Aria. Weather metaphors welcome.',
      post_history_instructions: 'End long replies with a short sensory beat.',
      alternate_greetings: [],
      character_book: {
        name: 'Black Coast Lore',
        description: 'Embedded lore for Aria home waters.',
        entries: [
          {
            keys: ['storm', 'rain', 'Black Coast'],
            content: 'The Black Coast never dries; salt winds strip paint from every hull.',
            enabled: true,
            constant: true,
            comment: 'weather',
            insertion_order: 10,
          },
          {
            keys: ['storm-glass', 'charm'],
            content: "Aria's storm-glass charms cloud when a true squall is half a day out.",
            enabled: true,
            constant: false,
            comment: 'charms',
            insertion_order: 20,
          },
        ],
      },
      extensions: {
        regex_scripts: [
          {
            id: 'hide-ooc',
            scriptName: 'hide ooc',
            findRegex: '/\\(OOC:.*?\\)/gi',
            replaceString: '',
            placement: [2],
            disabled: false,
            markdownOnly: true,
            promptOnly: false,
          },
          {
            id: 'em-to-italic',
            scriptName: 'asterisk stage',
            findRegex: '/\\*(.+?)\\*/g',
            replaceString: '「$1」',
            placement: [2],
            disabled: false,
          },
        ],
      },
    },
  };

  const JOB_ACTIVE_STATUSES = new Set(['queued', 'pending', 'running']);

  const JOB_TERMINAL_STATUSES = new Set(['succeeded', 'completed', 'failed', 'cancelled', 'partial', 'expired', 'done', 'error', 'stopped']);

  const JOB_STATUS_LABELS = {
    queued: '排队中', pending: '等待中', running: '运行中', processing: '处理中',
    succeeded: '已完成', completed: '已完成', done: '已完成',
    failed: '失败', error: '错误', cancelled: '已取消', stopped: '已停止',
    partial: '部分完成', expired: '已过期',
  };

  let jobStatusSnapshots = new Map();

  let jobsPollTimer = null;

  let sysStatusTimer = null;

  function jobIdOf(j) {
    return j.runId || j.id || j.run_id || '';
  }

  function jobStatusOf(j) {
    const raw = j.status || j.jobStatus || '';
    return String(raw).toLowerCase();
  }

  function jobsActivityCount(jobs) {
    let n = 0;
    for (const j of jobs) {
      if (JOB_ACTIVE_STATUSES.has(jobStatusOf(j))) n += 1;
    }
    return n;
  }

  function collectJobTransitions(jobs) {
    const transitions = [];
    const seen = new Set();
    for (const j of jobs) {
      const id = jobIdOf(j);
      if (!id) continue;
      seen.add(id);
      const status = jobStatusOf(j);
      const prev = jobStatusSnapshots.get(id);
      if (prev && JOB_ACTIVE_STATUSES.has(prev) && JOB_TERMINAL_STATUSES.has(status)) {
        transitions.push({ id, kind: j.kind || '', prev, status });
      }
      jobStatusSnapshots.set(id, status);
    }
    for (const [id] of jobStatusSnapshots) {
      if (!seen.has(id)) jobStatusSnapshots.delete(id);
    }
    return transitions;
  }

  function updateJobsBadge(count) {
    const badge = $('jobs-nav-badge');
    if (!badge) return;
    if (count > 0) {
      badge.textContent = count > 99 ? '99+' : String(count);
      badge.classList.remove('hidden');
    } else {
      badge.classList.add('hidden');
    }
  }

  function jobsTabVisible() {
    const panel = $('tab-jobs');
    return !!(panel && !panel.classList.contains('hidden'));
  }

  function renderJobsList(jobs) {
    const el = $('jobs-list');
    if (!el) return;
    el.textContent = '';
    if (!jobs.length) {
      const p = document.createElement('p');
      p.className = 'muted sm';
      p.textContent = '（暂无任务）';
      el.appendChild(p);
      return;
    }
    for (const j of jobs) {
      const id = jobIdOf(j);
      const status = jobStatusOf(j);
      const kind = j.kind || '';
      const prog = j.progress != null ? Number(j.progress) : null;
      const card = document.createElement('div');
      card.className = 'jobs-card' + (JOB_ACTIVE_STATUSES.has(status) ? ' active' : '');
      const head = document.createElement('div');
      head.className = 'jobs-card-head';
      const codeEl = document.createElement('code');
      codeEl.textContent = id;
      head.appendChild(codeEl);
      const badge = document.createElement('span');
      badge.className = 'jobs-status ' + status;
      badge.textContent = JOB_STATUS_LABELS[status] || status;
      head.appendChild(badge);
      card.appendChild(head);
      const meta = document.createElement('div');
      meta.className = 'jobs-card-meta';
      const bits = [kind];
      if (j.model) bits.push('模型 ' + j.model);
      if (j.progressMessage) bits.push(j.progressMessage);
      if (j.updatedAt) bits.push('更新 ' + String(j.updatedAt));
      meta.textContent = bits.join(' · ');
      card.appendChild(meta);
      if (prog != null && prog > 0 && prog < 100) {
        const track = document.createElement('div');
        track.className = 'progress-track';
        const fill = document.createElement('div');
        fill.className = 'progress-fill';
        fill.style.width = '100%';
        fill.style.setProperty('--progress-fill-x', (Math.min(100, Math.max(0, prog)) / 100).toFixed(3));
        track.appendChild(fill);
        card.appendChild(track);
      }
      const actions = document.createElement('div');
      actions.className = 'jobs-card-actions';
      const detailBtn = document.createElement('button');
      detailBtn.type = 'button';
      detailBtn.className = 'ghost sm';
      detailBtn.textContent = '详情';
      detailBtn.onclick = async () => {
        try {
          const data = await api('/api/v1/jobs/' + encodeURIComponent(id));
          setPanel('jobs-out', 'jobs-msg', data);
        } catch (e) {
          setPanel('jobs-out', 'jobs-msg', null, e);
        }
      };
      actions.appendChild(detailBtn);
      if (JOB_ACTIVE_STATUSES.has(status)) {
        const cancelBtn = document.createElement('button');
        cancelBtn.type = 'button';
        cancelBtn.className = 'ghost sm danger';
        cancelBtn.textContent = '取消';
        cancelBtn.onclick = async () => {
          try {
            const data = await api('/api/v1/jobs/' + encodeURIComponent(id) + '/cancel', { method: 'POST' });
            setPanel('jobs-out', 'jobs-msg', data);
            await refreshJobs();
          } catch (e) {
            setPanel('jobs-out', 'jobs-msg', null, e);
          }
        };
        actions.appendChild(cancelBtn);
      }
      card.appendChild(actions);
      el.appendChild(card);
    }
  }

  function pollJobs(delay = 0) {
    clearTimeout(jobsPollTimer);
    jobsPollTimer = setTimeout(async () => {
      let next = 15000;
      try {
        const data = await api('/api/v1/jobs?limit=50');
        const jobs = data.jobs || [];
        const ac = jobsActivityCount(jobs);
        updateJobsBadge(ac);
        const transitions = collectJobTransitions(jobs);
        for (const t of transitions) {
          const failed = t.status === 'failed' || t.status === 'error';
          const cancelled = t.status === 'cancelled' || t.status === 'stopped';
          const label = failed ? '失败' : cancelled ? '已取消' : '完成';
          showToast('任务「' + (t.kind || t.id) + '」' + label + ' · ' + String(t.id).slice(-8), failed ? 'error' : cancelled ? 'warning' : 'success');
        }
        if (jobsTabVisible()) renderJobsList(jobs);
        const summary = $('jobs-summary');
        if (summary && summary.textContent === '暂无任务数据') {
          summary.textContent = 'count=' + (data.count != null ? data.count : jobs.length) +
            ' · running=' + (data.running != null ? data.running : '?') +
            ' · 队列=' + (data.queued != null ? data.queued : '?');
        }
        next = ac > 0 ? 5000 : 15000;
      } catch (_) {
        // backend offline — keep badge as-is, retry on idle interval
      }
      pollJobs(next);
    }, delay);
  }

  function refreshSystemStatus(delay = 0) {
    clearTimeout(sysStatusTimer);
    sysStatusTimer = setTimeout(async () => {
      const el = $('sys-status');
      let next = 30000;
      if (el) {
        let status = 'offline';
        let label = '连接中断';
        let title = '无法连接到本地服务，系统将自动重试';
        try {
          const res = await fetch(apiBase() + '/api/v1/public/info', { cache: 'no-store' });
          if (res.ok) {
            const info = await res.json();
            if (info && info.name) {
              status = 'ready';
              label = '就绪';
              title = '服务连接正常 · ' + (info.phase ? '阶段 ' + info.phase : '');
            } else {
              status = 'degraded';
              label = '服务异常';
              title = '服务可访问，但响应异常';
            }
          } else {
            status = 'degraded';
            label = '服务异常';
            title = '服务可访问，但请求出现异常';
          }
        } catch (_) {
          status = 'offline';
          label = '连接中断';
          title = '无法连接到本地服务，系统将自动重试';
        }
        el.className = 'sys-status ' + status;
        el.textContent = label;
        el.title = title;
        next = status === 'ready' ? 30000 : 8000;
      }
      refreshSystemStatus(next);
    }, delay);
  }

  function startTaskCenter() {
    updateJobsBadge(0);
    pollJobs();
    refreshSystemStatus();
    // S9.21: 浏览器离线/上线事件即时刷新状态指示（不再等轮询周期）
    window.addEventListener('offline', () => {
      const el = $('sys-status');
      if (el) {
        el.className = 'sys-status offline';
        el.textContent = '连接中断';
        el.title = '浏览器已离线，系统将自动重试';
      }
      refreshSystemStatus(1000);
    });
    window.addEventListener('online', () => refreshSystemStatus(0));
  }

/* ================= initJobsUI: former top-level wiring ================= */
export function initJobsUI() {
  if ($('jobs-refresh')) {
    $('jobs-refresh').onclick = () => refreshJobs();
  if ($('jobs-cancel-all')) {
    $('jobs-cancel-all').onclick = async () => {
      try {
        const data = await api('/api/v1/jobs/cancel-all', { method: 'POST', body: {} });
        showToast('已取消 ' + (data.count ?? (data.cancelled || []).length) + ' 个活动任务');
        await refreshJobs();
      } catch (e) {
        showToast(String(e.message || e), true);
      }
    };
  }
  }
  if ($('jobs-create-noop')) {
    $('jobs-create-noop').onclick = async () => {
      try {
        const created = await api('/api/v1/jobs', {
          method: 'POST',
          body: JSON.stringify({ kind: 'noop', payload: { source: 'web-s5' } }),
        });
        $('jobs-msg').textContent = '已创建测试任务 · ' + (created.runId || created.id || '');
        $('job-run-id').value = created.runId || created.id || '';
        await refreshJobs();
      } catch (e) {
        setPanel('jobs-out', 'jobs-msg', null, e);
      }
    };
  }
  if ($('job-detail')) {
    $('job-detail').onclick = async () => {
      try {
        const id = ($('job-run-id').value || '').trim();
        if (!id) throw new Error('需要 run id');
        const data = await api('/api/v1/jobs/' + id);
        setPanel('jobs-out', 'jobs-msg', data);
      } catch (e) {
        setPanel('jobs-out', 'jobs-msg', null, e);
      }
    };
  }
  if ($('job-cancel')) {
    $('job-cancel').onclick = async () => {
      try {
        const id = ($('job-run-id').value || '').trim();
        if (!id) throw new Error('需要 run id');
        const data = await api('/api/v1/jobs/' + id + '/cancel', { method: 'POST' });
        setPanel('jobs-out', 'jobs-msg', data);
        await refreshJobs();
      } catch (e) {
        setPanel('jobs-out', 'jobs-msg', null, e);
      }
    };
  }
  if ($('job-stream')) {
    $('job-stream').onclick = async () => {
      try {
        const id = ($('job-run-id').value || '').trim();
        if (!id) throw new Error('需要 run id');
        $('jobs-out').textContent = '';
        $('jobs-msg').textContent = 'SSE 连接中…';
        for await (const ev of readSSE('/api/v1/jobs/' + id + '/stream')) {
          appendStreamLine('jobs-out', ev);
          $('jobs-msg').textContent = 'Streaming job ' + id;
          if (ev.json && (ev.json.eventType === 'done')) break;
        }
        $('jobs-msg').textContent = 'SSE 结束';
      } catch (e) {
        setPanel('jobs-out', 'jobs-msg', null, e);
      }
    };
  }
  window.__bgLastResult = window.__bgLastResult || null;
  window.__bgLastRunId = window.__bgLastRunId || null;
  window.__bgLastBody = window.__bgLastBody || null;
  window.__bgStreaming = false;
  if ($('bg-mode')) {
    $('bg-mode').onchange = syncBgStageUi;
    syncBgStageUi();
  }
  if ($('bg-start')) {
    $('bg-start').onclick = async () => {
      try {
        // pipeline preferred via mode default; start uses body.mode
        await runBgStart('/api/v1/background/start');
      } catch (e) {
        setPanel('bg-out', 'bg-msg', null, e);
        if ($('bg-retry')) $('bg-retry').hidden = false;
      }
    };
  }
  if ($('bg-stage-route')) {
    $('bg-stage-route').onclick = async () => {
      try {
        const stage = ($('bg-mode').value || 'pipeline').trim();
        await runBgStart('/api/v1/background/' + encodeURIComponent(stage));
      } catch (e) {
        setPanel('bg-out', 'bg-msg', null, e);
        if ($('bg-retry')) $('bg-retry').hidden = false;
      }
    };
  }
  if ($('bg-retry')) {
    $('bg-retry').onclick = async () => {
      try {
        const body = window.__bgLastBody || bgStartBody();
        if ($('bg-mode') && body.mode) $('bg-mode').value = body.mode;
        syncBgStageUi();
        await runBgStart('/api/v1/background/start');
      } catch (e) {
        setPanel('bg-out', 'bg-msg', null, e);
      }
    };
  }
  if ($('bg-stop')) {
    $('bg-stop').onclick = async () => {
      try {
        const id = ($('bg-run-id').value || '').trim();
        if (!id) throw new Error('需要 run id');
        const data = await api('/api/v1/background/stop', {
          method: 'POST',
          body: JSON.stringify({ id }),
        });
        setPanel('bg-out', 'bg-msg', data);
        if ($('bg-progress-label')) $('bg-progress-label').textContent = '已请求停止';
      } catch (e) {
        setPanel('bg-out', 'bg-msg', null, e);
      }
    };
  }
  if ($('bg-stream')) {
    $('bg-stream').onclick = async () => {
      try {
        const id = ($('bg-run-id').value || '').trim();
        if (!id) throw new Error('需要 run id');
        await bgFollowStream(id);
      } catch (e) {
        setPanel('bg-out', 'bg-msg', null, e);
      }
    };
  }
  if ($('bg-apply-partner')) {
    $('bg-apply-partner').onclick = async () => {
      try {
        const runId =
          ($('bg-run-id') && $('bg-run-id').value || '').trim() ||
          window.__bgLastRunId ||
          '';
        const body = { select: true };
        if (window.__bgLastResult) body.result = window.__bgLastResult;
        if (runId) body.runId = runId;
        if (!body.result && !body.runId) {
          throw new Error('需要 run id 或已完成的 BG 结果');
        }
        const data = await api('/api/v1/background/apply', {
          method: 'POST',
          body: JSON.stringify(body),
        });
        setPanel('bg-out', 'bg-msg', data);
        const wbN = (data.worldBooks || []).length;
        const ccN = (data.characterCards || []).length;
        $('bg-msg').textContent =
          '已写入 Partner：世界书 ' + wbN + ' · 角色卡 ' + ccN +
          (data.selected && (data.selected.worldBookId || data.selected.characterCardId)
            ? ' · 已选中'
            : '') +
          ' · 可前往角色与世界';
        __loadPartner()
      } catch (e) {
        setPanel('bg-out', 'bg-msg', null, e);
      }
    };
  }
  window.__btLastResult = window.__btLastResult || null;
  window.__btLastRunId = window.__btLastRunId || null;
  window.__btLastLabels = window.__btLastLabels || null;
  if ($('bt-classify')) {
    $('bt-classify').onclick = async () => {
      try {
        const body = {
          title: ($('bt-title').value || '').trim() || undefined,
          text: ($('bt-text').value || '').trim() || undefined,
        };
        const data = await api('/api/v1/book-travel/classify', {
          method: 'POST',
          body: JSON.stringify(body),
        });
        if (data && data.labels) window.__btLastLabels = data.labels;
        setPanel('bt-out', 'bt-msg', data);
      } catch (e) {
        setPanel('bt-out', 'bt-msg', null, e);
      }
    };
  }
  if ($('bt-chain')) {
    $('bt-chain').onchange = updateBtChainHint;
  }
  if ($('bt-clear-chain')) {
    $('bt-clear-chain').onclick = () => {
      window.__btLastResult = null;
      window.__btLastRunId = null;
      updateBtChainHint();
      if ($('bt-msg')) $('bt-msg').textContent = '已清除上一步串联上下文';
    };
  }
  updateBtChainHint();
  if ($('bt-start')) {
    $('bt-start').onclick = async () => {
      try {
        await btStart('/api/v1/book-travel/start');
      } catch (e) {
        setPanel('bt-out', 'bt-msg', null, e);
      }
    };
  }
  if ($('bt-step-route')) {
    $('bt-step-route').onclick = async () => {
      try {
        const step = ($('bt-step') && $('bt-step').value) || 'assemble';
        await btStart('/api/v1/book-travel/' + encodeURIComponent(step));
      } catch (e) {
        setPanel('bt-out', 'bt-msg', null, e);
      }
    };
  }
  if ($('bt-stop')) {
    $('bt-stop').onclick = async () => {
      try {
        const id = ($('bt-run-id').value || '').trim();
        if (!id) throw new Error('需要 run id');
        const data = await api('/api/v1/book-travel/stop', {
          method: 'POST',
          body: JSON.stringify({ id }),
        });
        setPanel('bt-out', 'bt-msg', data);
      } catch (e) {
        setPanel('bt-out', 'bt-msg', null, e);
      }
    };
  }
  if ($('bt-stream')) {
    $('bt-stream').onclick = async () => {
      try {
        const id = ($('bt-run-id').value || '').trim();
        if (!id) throw new Error('需要 run id');
        $('bt-out').textContent = '';
        $('bt-msg').textContent = 'SSE 连接中…';
        for await (const ev of readSSE('/api/v1/book-travel/stream?id=' + encodeURIComponent(id))) {
          appendStreamLine('bt-out', ev);
          $('bt-msg').textContent = 'Streaming bookTravel ' + id;
          if (ev.json) {
            const j = ev.json;
            if (j.result) rememberBtResult({ result: j.result, runId: id }, id);
            else if (j.data) rememberBtResult(j.data, id);
            if (j.eventType === 'done' || j.type === 'done') {
              if (j.result) rememberBtResult({ result: j.result }, id);
              window.__btLastRunId = id;
              updateBtChainHint();
            }
          }
          if (ev.json && (ev.json.eventType === 'done' || ev.json.type === 'done')) break;
        }
        $('bt-msg').textContent = 'SSE 结束';
        updateBtChainHint();
      } catch (e) {
        setPanel('bt-out', 'bt-msg', null, e);
      }
    };
  }
  if ($('ol-preview')) {
    $('ol-preview').onclick = async () => {
      try {
        const body = {
          title: ($('ol-title').value || '').trim() || undefined,
          text: ($('ol-text').value || '').trim() || undefined,
          useLlm: !!( $('ol-use-llm') && $('ol-use-llm').checked ),
        };
        const data = await api('/api/v1/outline/reverse/preview', {
          method: 'POST',
          body: JSON.stringify(body),
        });
        setPanel('ol-out', 'ol-msg', data);
        if ($('ol-steps')) $('ol-steps').textContent = '步骤：预览 ✓ → 分析 → 定稿 → 保存';
      } catch (e) {
        setPanel('ol-out', 'ol-msg', null, e);
      }
    };
  }
  if ($('ol-analyze')) {
    $('ol-analyze').onclick = async () => {
      try {
        const body = {
          title: ($('ol-title').value || '').trim() || undefined,
          text: ($('ol-text').value || '').trim() || undefined,
          useLlm: !!( $('ol-use-llm') && $('ol-use-llm').checked ),
        };
        const data = await api('/api/v1/outline/reverse/analyze', {
          method: 'POST',
          body: JSON.stringify(body),
        });
        setPanel('ol-out', 'ol-msg', data);
        if ($('ol-steps')) $('ol-steps').textContent = '步骤：预览 → 分析 ✓ → 定稿 → 保存';
      } catch (e) {
        setPanel('ol-out', 'ol-msg', null, e);
      }
    };
  }
  if ($('ol-finalize')) {
    $('ol-finalize').onclick = async () => {
      try {
        const body = {
          title: ($('ol-title').value || '').trim() || undefined,
          text: ($('ol-text').value || '').trim() || undefined,
          useLlm: !!( $('ol-use-llm') && $('ol-use-llm').checked ),
        };
        const data = await api('/api/v1/outline/reverse/finalize', {
          method: 'POST',
          body: JSON.stringify(body),
        });
        setPanel('ol-out', 'ol-msg', data);
        if ($('ol-steps')) $('ol-steps').textContent = '步骤：预览 → 分析 → 定稿 ✓ → 保存';
      } catch (e) {
        setPanel('ol-out', 'ol-msg', null, e);
      }
    };
  }
  if ($('ol-save')) {
    $('ol-save').onclick = async () => {
      try {
        const title = ($('ol-title').value || '').trim() || 'outline';
        const text = ($('ol-text').value || '').trim();
        if (!text) throw new Error('正文不可为空');
        const prev = await api('/api/v1/outline/reverse/preview', {
          method: 'POST',
          body: JSON.stringify({
            title,
            text,
            useLlm: !!( $('ol-use-llm') && $('ol-use-llm').checked ),
          }),
        });
        const md = prev.outlineMarkdown || ('# ' + title + '\n\n' + text);
        const rel = (($('ol-save-path').value || '').trim()) || ('outlines/' + title.replace(/\s+/g, '_').replace(/[^\w\-\u4e00-\u9fa5.]/g, '') + '.md');
        try {
          await api('/api/v1/works/file', {
            method: 'PUT',
            body: JSON.stringify({ path: rel, content: md }),
          });
        } catch (e2) {
          if (e2 && e2.message && e2.message.includes('parent directory does not exist')) {
            await api('/api/v1/works/dir', {
              method: 'POST',
              body: JSON.stringify({ path: rel.includes('/') ? rel.split('/').slice(0, -1).join('/') : '' }),
            });
            await api('/api/v1/works/file', {
              method: 'PUT',
              body: JSON.stringify({ path: rel, content: md }),
            });
          } else {
            throw e2;
          }
        }
        setPanel('ol-out', 'ol-msg', { ok: true, savedTo: rel, preview: prev });
      } catch (e) {
        setPanel('ol-out', 'ol-msg', null, e);
      }
    };
  }
  if ($('st-file')) {
    $('st-file').onchange = (e) => {
      const f = e.target.files && e.target.files[0];
      if (!f) return;
      if (f.name.toLowerCase().endsWith('.png') || (f.type || '').includes('png')) {
        const reader = new FileReader();
        reader.onload = () => {
          const buf = reader.result;
          const bytes = new Uint8Array(buf);
          let bin = '';
          for (let i = 0; i < bytes.length; i++) bin += String.fromCharCode(bytes[i]);
          const b64 = btoa(bin);
          $('st-json').value = JSON.stringify({ pngBase64: b64 }, null, 2);
          $('st-msg').textContent = '已读取 PNG ' + f.name + '（将走 tEXt chara 导入）';
        };
        reader.onerror = () => { $('st-msg').textContent = 'PNG 读取失败'; };
        reader.readAsArrayBuffer(f);
        return;
      }
      const reader = new FileReader();
      reader.onload = () => {
        let raw = String(reader.result);
        $('st-json').value = raw;
        $('st-msg').textContent = '已读取 ' + f.name;
      };
      reader.onerror = () => { $('st-msg').textContent = '读取失败'; };
      reader.readAsText(f);
    };
  }
  if ($('st-load-fixture')) {
    $('st-load-fixture').onclick = () => {
      $('st-json').value = JSON.stringify(ST_FIXTURE, null, 2);
      $('st-msg').textContent = '已填入 testdata/st/aria_with_book_regex_v2.json 样例（含世界书+正则）';
    };
  }
  if ($('st-import')) {
    $('st-import').onclick = async () => {
      try {
        let raw = ($('st-json').value || '').trim();
        if (!raw) {
          raw = JSON.stringify(ST_FIXTURE);
          $('st-json').value = JSON.stringify(ST_FIXTURE, null, 2);
        }
        // validate JSON client-side
        JSON.parse(raw);
        const data = await api('/api/v1/partner/st-import', {
          method: 'POST',
          body: raw,
        });
        setPanel('st-out', 'st-msg', data);
        const bits = [];
        if (data && data.item) bits.push('卡=' + (data.item.name || data.item.id));
        if (data && data.worldBook) bits.push('世界书=' + (data.worldBook.name || data.worldBook.id));
        if (data && data.loreEntryCount) bits.push('条目=' + data.loreEntryCount);
        if (data && data.regexScriptCount) bits.push('正则=' + data.regexScriptCount);
        if (bits.length && $('st-msg')) {
          $('st-msg').textContent = (data.ok ? '导入成功 · ' : '') + bits.join(' · ');
        }
        // refresh partner cache so chat/partner see new card + linked world book
        await __loadPartner();
        // auto-select imported card / world book when present
        try {
          if (data && data.item && data.item.id) {
            if ($('chat-cc')) $('chat-cc').value = data.item.id;
            if ($('cc-wb') && data.item.worldBookId) $('cc-wb').value = data.item.worldBookId;
            if ($('chat-wb') && data.item.worldBookId) $('chat-wb').value = data.item.worldBookId;
          }
        } catch (_) {}
      } catch (e) {
        setPanel('st-out', 'st-msg', null, e);
      }
    };
  }
  if ($('st-wi-preview')) {
    $('st-wi-preview').onclick = async () => {
      try {
        const wb = ($('chat-wb') && $('chat-wb').value) || (partner && partner.selectedWorldBookId) || '';
        const cc = ($('chat-cc') && $('chat-cc').value) || (partner && partner.selectedCharacterCardId) || '';
        if (!wb && !cc) throw new Error('请先导入并选择角色卡/世界书');
        const msg = (($('st-wi-msg') && $('st-wi-msg').value) || '').trim() || 'Hello';
        const trigger = ($('st-wi-trigger') && $('st-wi-trigger').value) || 'normal';
        const ccObj = ((partner && partner.characterCards) || []).find((c) => c.id === cc);
        const charName = (ccObj && ccObj.name) || 'Char';
        const data = await api('/api/v1/partner/wi-preview', {
          method: 'POST',
          body: JSON.stringify({
            worldBookId: wb || undefined,
            characterCardId: cc || undefined,
            dryRun: true,
            messages: [{ role: 'user', content: msg }],
            trigger,
            worldInfoScanContext: {
              trigger,
              userName: 'User',
              charName,
              characterName: charName,
            },
          }),
        });
        setPanel('st-wi-out', 'st-wi-msg-status', data);
        const bits = [];
        if (data) {
          bits.push('激活=' + (data.wiActivated || 0));
          if (data.exampleMessages) bits.push('EM对=' + (data.exampleMessages.length || 0));
          if (data.automationIds && data.automationIds.length) bits.push('auto=' + data.automationIds.join(','));
          if (data.skippedVectorized) bits.push('skipVec=' + data.skippedVectorized);
          if (data.skippedTrigger) bits.push('skipTrig=' + data.skippedTrigger);
          if (data.messageInjections) bits.push('注入=' + data.messageInjections.length);
        }
        if ($('st-wi-msg-status')) $('st-wi-msg-status').textContent = bits.join(' · ') || 'ok';
      } catch (e) {
        setPanel('st-wi-out', 'st-wi-msg-status', null, e);
      }
    };
  }
  if ($('agent-list')) {
    $('agent-list').onclick = async () => {
      try {
        const path = ($('agent-path').value || '').trim() || '.';
        const data = await api('/api/v1/agent/tools/list', {
          method: 'POST',
          body: JSON.stringify({ path }),
        });
        setPanel('agent-out', 'agent-msg', data);
      } catch (e) {
        setPanel('agent-out', 'agent-msg', null, e);
      }
    };
  }
  if ($('as-list')) {
    $('as-list').onclick = async () => {
      try {
        const prefix = ($('as-prefix') && $('as-prefix').value) || 'partner-session-';
        const data = await api('/api/v1/agent/sessions?prefix=' + encodeURIComponent(prefix));
        setPanel('as-out', 'as-msg-status', data);
        __renderAsVisual(data);
        const first = (data.sessions && data.sessions[0] && data.sessions[0].id) || '';
        if (first && $('as-id') && !($('as-id').value || '').trim()) $('as-id').value = first;
      } catch (e) {
        setPanel('as-out', 'as-msg-status', null, e);
      }
    };
  }
  if ($('as-create')) {
    $('as-create').onclick = async () => {
      try {
        const prefix = ($('as-prefix') && $('as-prefix').value) || 'partner-session-';
        const title = ($('as-title') && $('as-title').value || '').trim() || '代理会话';
        const body = {
          title,
          prefix,
          sessionKind: prefix.startsWith('story') ? 'story' : undefined,
        };
        const data = await api('/api/v1/agent/sessions', {
          method: 'POST',
          body: JSON.stringify(body),
        });
        if (data.id && $('as-id')) $('as-id').value = data.id;
        setPanel('as-out', 'as-msg-status', data);
      } catch (e) {
        setPanel('as-out', 'as-msg-status', null, e);
      }
    };
  }
  if ($('as-load')) {
    $('as-load').onclick = async () => {
      try {
        const id = ($('as-id') && $('as-id').value || '').trim();
        if (!id) throw new Error('session id required');
        const data = await api('/api/v1/agent/sessions/' + encodeURIComponent(id));
        if (data.title && $('as-title')) $('as-title').value = data.title;
        setPanel('as-out', 'as-msg-status', data);
      } catch (e) {
        setPanel('as-out', 'as-msg-status', null, e);
      }
    };
  }
  if ($('as-delete')) {
    $('as-delete').onclick = async () => {
      try {
        const id = ($('as-id') && $('as-id').value || '').trim();
        if (!id) throw new Error('session id required');
        const data = await api('/api/v1/agent/sessions/' + encodeURIComponent(id), { method: 'DELETE' });
        setPanel('as-out', 'as-msg-status', data);
      } catch (e) {
        setPanel('as-out', 'as-msg-status', null, e);
      }
    };
  }
  window.addEventListener('load', () => startTaskCenter());
}

/* ===== exports consumed by remaining closure parts ===== */
export { setPanel, refreshJobs, readSSE };
