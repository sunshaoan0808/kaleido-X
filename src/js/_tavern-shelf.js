  function shelfStatus(msg) {
    const el = $('shelf-status');
    if (el) el.textContent = msg || '';
  }

  function shelfApi(path, opts = {}) {
    return api('/api/v1/crawler' + path, opts);
  }


  async function loadShelfChatSessions() {
    const sel = $('shelf-chat-session');
    if (!sel) return;
    sel.innerHTML = '<option value="">加载中…</option>';
    try {
      const data = await stApi('/sessions');
      const list = (data && data.sessions) || [];
      sel.innerHTML = '';
      if (!list.length) {
        const o = document.createElement('option');
        o.value = ''; o.textContent = '（暂无故事馆会话）';
        sel.appendChild(o);
        return;
      }
      for (const s of list) {
        const o = document.createElement('option');
        o.value = s.sessionId;
        const t = s.title || s.sessionId;
        o.textContent = t + ' · ' + stTurnLabel(s.turn || 0);
        sel.appendChild(o);
      }
    } catch (e) {
      sel.innerHTML = '';
      const o = document.createElement('option');
      o.value = ''; o.textContent = '加载失败：' + (e.message || e);
      sel.appendChild(o);
    }
  }

  async function loadShelfSchedule() {
    try {
      const data = await shelfApi('/chat-to-shelf/schedule');
      const sch = (data && data.schedule) || {};
      if ($('shelf-sched-enabled')) $('shelf-sched-enabled').checked = !!sch.enabled;
      if ($('shelf-sched-hours')) $('shelf-sched-hours').value = sch.intervalHours || 24;
      if ($('shelf-sched-turns')) $('shelf-sched-turns').value = sch.minTurns || 3;
      if ($('shelf-sched-topack')) $('shelf-sched-topack').checked = sch.toPack !== false;
      const meta = $('shelf-sched-meta');
      if (meta) {
        const last = sch.lastRunAt ? ('上次 ' + String(sch.lastRunAt).slice(0, 19).replace('T', ' ')) : '尚未运行';
        const lr = sch.lastResult || {};
        meta.textContent = (sch.enabled ? '定时开 · ' : '定时关 · ') + last +
          (lr.publishedCount != null ? (' · 上架 ' + lr.publishedCount + ' / 跳过 ' + (lr.skipped || 0)) : '');
      }
    } catch (e) {
      const meta = $('shelf-sched-meta');
      if (meta) meta.textContent = '定时配置读取失败：' + (e.message || e);
    }
  }

  async function shelfPublishChat() {
    const sid = ($('shelf-chat-session') && $('shelf-chat-session').value) || '';
    if (!sid) { shelfStatus('请选择故事馆会话'); return; }
    const title = (($('shelf-chat-title') && $('shelf-chat-title').value) || '').trim();
    const toPack = !($('shelf-chat-topack') && !$('shelf-chat-topack').checked);
    shelfStatus('正在整理并上架…');
    try {
      const body = { source: 'tavern', sessionId: sid, toPack: toPack, force: true };
      if (title) body.title = title;
      const data = await shelfApi('/chat-to-shelf', { method: 'POST', body: JSON.stringify(body) });
      if (!data || data.ok === false) throw new Error((data && data.error) || '失败');
      shelfStatus((data.skipped ? '未变化：' : '已上架：') + (data.title || '') +
        (data.chapterCount ? (' · ' + data.chapterCount + ' 章') : '') +
        (data.packId ? (' · Pack ' + data.packId) : ''));
      await loadBookshelf();
      if (data.packId && typeof stLoadPacks === 'function') {
        try { await stLoadPacks(); } catch (_) {}
      }
    } catch (e) {
      shelfStatus('上架失败：' + (e.message || e));
    }
  }

  async function shelfSaveSchedule() {
    const body = {
      enabled: !!( $('shelf-sched-enabled') && $('shelf-sched-enabled').checked ),
      intervalHours: Math.max(1, parseInt(($('shelf-sched-hours') && $('shelf-sched-hours').value) || '24', 10) || 24),
      minTurns: Math.max(1, parseInt(($('shelf-sched-turns') && $('shelf-sched-turns').value) || '3', 10) || 3),
      toPack: !($('shelf-sched-topack') && !$('shelf-sched-topack').checked),
      source: 'tavern',
    };
    try {
      const data = await shelfApi('/chat-to-shelf/schedule', { method: 'PUT', body: JSON.stringify(body) });
      if (!data || data.ok === false) throw new Error((data && data.error) || '保存失败');
      shelfStatus(body.enabled ? ('定时已开启：每 ' + body.intervalHours + ' 小时 · 最少 ' + body.minTurns + ' 回合') : '定时已关闭');
      await loadShelfSchedule();
    } catch (e) {
      shelfStatus('保存定时失败：' + (e.message || e));
    }
  }

  async function shelfRunScheduleNow() {
    shelfStatus('正在执行定时整理…');
    try {
      const data = await shelfApi('/chat-to-shelf/run-due', { method: 'POST', body: '{}' });
      if (!data || data.ok === false) throw new Error((data && data.error) || '失败');
      if (data.ran === false) {
        shelfStatus('未运行：' + (data.reason || '定时未启用（先勾选并保存）'));
      } else {
        shelfStatus('本轮上架 ' + (data.publishedCount || 0) + ' · 跳过 ' + (data.skipped || 0) +
          ((data.errors && data.errors.length) ? (' · 错误 ' + data.errors.length) : ''));
      }
      await loadBookshelf();
      await loadShelfSchedule();
    } catch (e) {
      shelfStatus('执行失败：' + (e.message || e));
    }
  }


  async function loadBookshelf() {
    const grid = $('bookshelf-grid');
    if (!grid) return;
    grid.innerHTML = '<p class="muted">加载中…</p>';
    try {
      const data = await shelfApi('/novels');
      shelfNovels = (data && data.novels) || [];
      renderBookshelfGrid();
      shelfStatus(shelfNovels.length ? ('共 ' + shelfNovels.length + ' 部') : '书架为空 — 可导入 TXT/MD/DOCX');
      loadShelfChatSessions().catch(() => {});
      loadShelfSchedule().catch(() => {});
      // 恢复进行中/刚完成的转换任务订阅（刷新后 localStorage 记录仍在）
      if (typeof shelfSyncDistilJobs === 'function') shelfSyncDistilJobs().catch(() => {});
    } catch (e) {
      grid.innerHTML = '<p class="err">加载失败：' + escapeHtml(e.message || String(e)) + '</p>';
      shelfStatus('加载失败');
    }
  }

  function renderBookshelfGrid() {
    const grid = $('bookshelf-grid');
    if (!grid) return;
    if (!shelfNovels.length) {
      grid.innerHTML = '<div class="st-empty"><span>书架上空空如也</span><span class="action">导入 TXT/MD/DOCX 后可阅读，并一键进故事馆</span></div>';
      return;
    }
    grid.innerHTML = '';
    for (const n of shelfNovels) {
      const card = document.createElement('div');
      card.className = 'shelf-card';
      card.dataset.slug = n.slug;
      const cover = n.hasCover
        ? '<img class="shelf-cover" src="' + escapeHtml(apiBase() + '/api/v1/crawler/novels/' + encodeURIComponent(n.slug) + '/cover') + '" alt="" loading="lazy" />'
        : '<div class="shelf-cover shelf-cover-empty">📖</div>';
      card.innerHTML =
        cover +
        '<div class="shelf-card-body">' +
          '<div class="shelf-title">' + escapeHtml(n.title || n.slug) + '</div>' +
          '<div class="shelf-meta muted sm">' + (n.chapterCount || 0) + ' 章' + (n.hasCover ? ' · 有封面' : '') + '</div>' +
          '<div class="shelf-actions row gap-sm wrap">' +
            '<button type="button" class="sm shelf-read">阅读</button>' +
            '<button type="button" class="sm shelf-play">开始转换</button>' +
            '<button type="button" class="ghost sm shelf-export">导出</button>' +
          '</div>' +
          '<div class="shelf-distil-progress" data-slug="' + escapeHtml(n.slug) + '" hidden></div>' +
        '</div>';
      card.querySelector('.shelf-read').onclick = (ev) => { ev.stopPropagation(); openShelfReader(n.slug); };
      card.querySelector('.shelf-play').onclick = (ev) => { ev.stopPropagation(); shelfDistilWorld(n.slug, n.title); };
      card.querySelector('.shelf-export').onclick = (ev) => { ev.stopPropagation(); shelfExport(n.slug, n.title); };
      card.onclick = () => openShelfReader(n.slug);
      grid.appendChild(card);
      if (typeof shelfRenderDistilProgress === 'function') shelfRenderDistilProgress(n.slug);
    }
  }

  async function openShelfReader(slug) {
    shelfActiveSlug = slug;
    const overlay = $('novel-reader');
    const titleEl = $('reader-title');
    const bodyEl = $('reader-content');
    if (!overlay || !bodyEl) return;
    overlay.classList.remove('hidden');
    if (titleEl) titleEl.textContent = '加载中…';
    bodyEl.textContent = '…';
    try {
      const data = await shelfApi('/novels/' + encodeURIComponent(slug) + '/content');
      if (titleEl) titleEl.textContent = data.title || slug;
      bodyEl.textContent = data.content || '';
    } catch (e) {
      if (titleEl) titleEl.textContent = slug;
      bodyEl.textContent = '读取失败：' + (e.message || e);
    }
  }

  function closeShelfReader() {
    const overlay = $('novel-reader');
    if (overlay) overlay.classList.add('hidden');
  }

