  async function stDeleteLore() {
    if (!tavernPack || stLoreEditIdx < 0) return;
    if (!await showConfirm('删除该 lore 条目？')) return;
    const entries = stEnsureLoreArray(tavernPack);
    entries.splice(stLoreEditIdx, 1);
    tavernPack.loreEntries = entries;
    tavernPack.updatedAt = new Date().toISOString();
    const payload = JSON.parse(JSON.stringify(tavernPack));
    delete payload.uploadChapters;
    await stApi('/packs', { method: 'POST', body: JSON.stringify(payload) });
    $('st-lore-editor').classList.add('hidden');
    stLoreEditIdx = -1;
    await stLoadPacks();
    const fresh = tavernPacks.find(p => p.id === payload.id);
    if (fresh) tavernPack = fresh;
    stRenderLore();
  }


  if ($('st-lore-add')) $('st-lore-add').onclick = () => stOpenLoreEditor(-1);
  if ($('st-lore-save')) $('st-lore-save').onclick = () => stSaveLore().catch(e => stStatus('Lore 保存失败：' + e.message));
  if ($('st-lore-cancel')) $('st-lore-cancel').onclick = () => { $('st-lore-editor').classList.add('hidden'); stLoreEditIdx = -1; stRenderLore(); };
  if ($('st-lore-del')) $('st-lore-del').onclick = () => stDeleteLore().catch(e => stStatus('Lore 删除失败：' + e.message));


  function stSyncModeToggle() {
    const box = $('st-mode-toggle');
    if (!box) return;
    if (!tavernSession || tavernSession.packMissing) { box.classList.add('hidden'); return; }
    box.classList.remove('hidden');
    const mode = (tavernSession.playMode || 'mainline').toLowerCase();
    box.querySelectorAll('.st-mode-btn').forEach(btn => {
      btn.classList.toggle('active', (btn.dataset.mode || '') === mode);
    });
  }
  async function stSetPlayMode(mode) {
    if (!tavernSession || tavernStreaming) return;
    // 支线：始终打开节点选择（总结整本 + 重要节点 + 支线开场）
    if (mode === 'side') {
      await stOpenSidePanel();
      return;
    }
    if ((tavernSession.playMode || '').toLowerCase() === mode) return;
    try {
      const s = await stApi('/sessions/' + encodeURIComponent(tavernSession.sessionId) + '/mode', {
        method: 'POST', body: JSON.stringify({ playMode: mode })
      });
      tavernSession = s;
      stCloseSidePanel();
      stRenderMessages();
      stRenderOptions();
      stSyncModeToggle();
      const sideLabel = tavernSession.sideBranchLabel ? (' · 支线「' + tavernSession.sideBranchLabel + '」') : '';
      stStatus(`${tavernSession.title || '故事馆'} · ${PLAYABLE_LABELS[tavernSession.playable] || ''} · ${PLAY_MODE_LABELS[tavernSession.playMode] || tavernSession.playMode || ''}${sideLabel} · tier ${tavernSession.contentTier || ''} · ${stTurnLabel(tavernSession.turn || 0)}`);
    } catch (e) {
      stStatus('模式切换失败：' + e.message);
    }
  }

  function stCloseSidePanel() {
    const p = $('st-side-panel');
    if (p) p.classList.add('hidden');
  }

  async function stOpenSidePanel() {
    if (!tavernSession || tavernSession.packMissing) return;
    const panel = $('st-side-panel');
    const list = $('st-side-node-list');
    const sumEl = $('st-side-novel-summary');
    const meta = $('st-side-panel-meta');
    if (!panel || !list) return;
    // S8.29: in immersive theater, #st-side-panel lives inside the #tab-tavern
    // shell (z=0 stacking context) which is painted *behind* #st-view-play
    // (messages/composer tree). Reparent it into #st-view-play so the overlay
    // actually sits above the story text and receives taps, not the messages.
    if (document.documentElement.getAttribute('data-immersive') === '1') {
      const host = $('st-view-play');
      if (host && panel.parentElement !== host) {
        host.appendChild(panel);
        panel.classList.add('st-side-float');
      }
    }
    panel.classList.remove('hidden');
    list.innerHTML = (typeof stSkeleton === 'function') ? stSkeleton(3) : '加载中…';
    if (sumEl) sumEl.textContent = '正在总结整本小说并选取重要节点…';
    try {
      const cat = await stApi('/sessions/' + encodeURIComponent(tavernSession.sessionId) + '/side-branches');
      if (sumEl) sumEl.textContent = cat.novelSummary || '（暂无整本摘要）';
      if (meta) {
        meta.textContent = (cat.packTitle || '') + ' · ' + ((cat.nodes || []).length) + ' 个关键节点'
          + (cat.resumeNodeId ? (' · 回主线锚点 ' + cat.resumeNodeId) : '');
      }
      list.innerHTML = '';
      const nodes = cat.nodes || [];
      if (!nodes.length) {
        list.innerHTML = (typeof stEmpty === 'function') ? stEmpty('没有可用节点', '请先完善剧本包章节') : '没有可用节点';
        return;
      }
      for (const n of nodes) {
        const btn = document.createElement('button');
        btn.type = 'button';
        btn.className = 'st-side-node-card';
        const reason = document.createElement('span');
        reason.className = 'reason';
        reason.textContent = n.reason || '关键节点';
        const t = document.createElement('span');
        t.className = 't';
        t.textContent = (n.chapterTitle ? (n.chapterTitle + ' · ') : '') + (n.title || n.id);
        const d = document.createElement('span');
        d.className = 'd';
        d.textContent = (n.summary || n.entry || '').slice(0, 160) || n.id;
        btn.appendChild(reason);
        btn.appendChild(t);
        btn.appendChild(d);
        btn.onclick = () => stEnterSideBranch(n.id);
        list.appendChild(btn);
      }
    } catch (e) {
      list.innerHTML = '';
      stStatus('支线目录加载失败：' + e.message);
      if (sumEl) sumEl.textContent = '加载失败：' + e.message;
    }
  }

  async function stEnterSideBranch(nodeId) {
    if (!tavernSession || !nodeId || tavernStreaming) return;
    try {
      stStatus('进入支线…', { silent: true });
      const s = await stApi('/sessions/' + encodeURIComponent(tavernSession.sessionId) + '/side-branches/enter', {
        method: 'POST', body: JSON.stringify({ nodeId })
      });
      tavernSession = s;
      stCloseSidePanel();
      stRenderMessages({ forceScroll: true });
      stRenderOptions();
      stRenderFocusBar();
      stSyncModeToggle();
      const sideLabel = tavernSession.sideBranchLabel ? ('「' + tavernSession.sideBranchLabel + '」') : nodeId;
      stStatus((tavernSession.title || '故事馆') + ' · 支线 ' + sideLabel + ' · 已写入支线开场白');
    } catch (e) {
      stStatus('进入支线失败：' + e.message);
    }
  }

  if ($('st-mode-mainline')) $('st-mode-mainline').onclick = () => stSetPlayMode('mainline');
  if ($('st-mode-side')) $('st-mode-side').onclick = () => stSetPlayMode('side');
  if ($('st-mode-free')) $('st-mode-free').onclick = () => stSetPlayMode('free');
  if ($('st-side-panel-close')) $('st-side-panel-close').onclick = () => stCloseSidePanel();


  async function stLoadSaves() {
    const lists = Array.from(document.querySelectorAll('.st-save-list'));
    if (!lists.length) return;
    for (const list of lists) {
      list.innerHTML = '';
      if (!tavernSession) {
        list.innerHTML = stEmpty('先打开会话', '选择会话后可见存档');
        await stLoadWorldline();
        return;
      }
    }
    for (const list of lists) list.innerHTML = stSkeleton(2);
    try {
      const data = await stApi('/sessions/' + encodeURIComponent(tavernSession.sessionId) + '/saves');
      const saves = data.saves || [];
      for (const list of lists) {
        list.innerHTML = '';
        if (!saves.length) {
          list.innerHTML = stEmpty('暂无存档', '点击保存记录当前进度');
          continue;
        }
        for (const s of saves) {
          const el = document.createElement('div');
          el.className = 'item';
          el.innerHTML = '<span class="t"></span><span class="d"></span><div class="row-actions"></div>';
          el.querySelector('.t').textContent = s.label || s.saveId;
          el.querySelector('.d').textContent = stTurnLabel(s.turn || 0) + ' · ' + (s.nodeId || '?') + ' · ' + (PLAY_MODE_LABELS[s.playMode] || s.playMode || '');
          const actions = el.querySelector('.row-actions');
          const btnR = document.createElement('button');
          btnR.type = 'button'; btnR.className = 'sm'; btnR.textContent = '回档';
          btnR.onclick = (ev) => { ev.stopPropagation(); stRestoreSave(s.saveId); };
          const btnD = document.createElement('button');
          btnD.type = 'button'; btnD.className = 'ghost sm'; btnD.textContent = '删';
          btnD.onclick = (ev) => { ev.stopPropagation(); stDeleteSave(s.saveId); };
          actions.appendChild(btnR); actions.appendChild(btnD);
          list.appendChild(el);
        }
      }
    } catch (e) {
      for (const list of lists) list.innerHTML = '<div class="muted sm">加载失败</div>';
      console.warn(e);
    }
    await stLoadWorldline();
  }
  async function stCreateSave() {
    if (!tavernSession) {
      showToast('还没有进行中的会话，请先在故事馆或档案馆开始剧本', 'warning');
      return;
    }
    const label = await showPrompt('存档名称（可空）', { value: '第' + (tavernSession.turn || 0) + '回合' }) || '';
    try {
      await stApi('/sessions/' + encodeURIComponent(tavernSession.sessionId) + '/saves', {
        method: 'POST', body: JSON.stringify({ label: label.trim() || undefined })
      });
      await stLoadSaves();
      showToast('已存档', 'success');
    } catch (e) { showToast('存档失败：' + e.message, 'error'); }
  }
  async function stRestoreSave(saveId) {
    if (!tavernSession || !saveId) return;
    if (!await showConfirm('回档会覆盖当前会话进度，确认？')) return;
    try {
      const s = await stApi('/sessions/' + encodeURIComponent(tavernSession.sessionId) + '/saves/' + encodeURIComponent(saveId) + '/restore', { method: 'POST', body: '{}' });
      tavernSession = s;
      stRenderMessages();
      stRenderOptions();
      stSyncModeToggle();
      await stLoadSessions();
      await stLoadSaves();
      stStatus(`${tavernSession.title || '故事馆'} · 已回档 · ${stTurnLabel(tavernSession.turn || 0)} · ${tavernSession.nodeId || ''}`);
    } catch (e) { stStatus('回档失败：' + e.message); }
  }
  async function stDeleteSave(saveId) {
    if (!tavernSession || !saveId) return;
    if (!await showConfirm('删除该存档？')) return;
    try {
      await stApi('/sessions/' + encodeURIComponent(tavernSession.sessionId) + '/saves/' + encodeURIComponent(saveId), { method: 'DELETE' });
      await stLoadSaves();
    } catch (e) { stStatus('删除失败：' + e.message); }
  }

  function stWorldlineShortId(id) {
    const s = String(id || '');
    return s.length > 18 ? s.slice(0, 12) + '…' : s;
  }
  async function stWorldlineNodeClick(s, currentSaveId) {
    if (!s || !s.saveId || s.saveId === currentSaveId) return;
    const label = s.label || s.saveId;
    if (!await showConfirm('回档到「' + label + '」turn ' + (s.turn || 0) + ' 会覆盖当前进度，确认？')) return;
    stRestoreSave(s.saveId);
  }
  function stRenderWorldlineLine(line, data) {
    const currentWorldlineId = data && data.currentWorldlineId;
    const currentSaveId = data && data.currentSaveId;
    const isCurrent = line.id === currentWorldlineId;
    const el = document.createElement('div');
    el.className = 'wl-line' + (isCurrent ? ' current' : '');
    const head = document.createElement('div');
    head.className = 'wl-line-head';
    const idEl = document.createElement('span');
    idEl.className = 'wl-line-id';
    idEl.textContent = String(line.id || '');
    const tag = document.createElement('span');
    tag.className = 'wl-tag' + (isCurrent ? ' current' : '');
    tag.textContent = line.forkFromSaveId
      ? '分支 · ← fork 自 ' + stWorldlineShortId(line.forkFromSaveId)
      : '主线';
    head.appendChild(idEl);
    head.appendChild(tag);
    el.appendChild(head);
    const flow = document.createElement('div');
    flow.className = 'wl-flow';
    const saves = (line.saves || []).slice().sort((a, b) => (a.turn || 0) - (b.turn || 0));
    for (const s of saves) {
      const node = document.createElement('button');
      node.type = 'button';
      let cls = 'wl-node';
      if (s.saveId === currentSaveId) cls += ' current';
      if (isCurrent) cls += ' active';
      node.className = cls;
      const label = document.createElement('span');
      label.className = 'wl-label';
      label.textContent = s.label || s.saveId;
      const turn = document.createElement('span');
      turn.className = 'wl-turn';
      turn.textContent = stTurnLabel(s.turn || 0);
      node.appendChild(label);
      node.appendChild(turn);
      node.title = (s.label || s.saveId) + ' · ' + stTurnLabel(s.turn || 0) + ' · ' + (s.nodeId || '');
      node.onclick = () => stWorldlineNodeClick(s, currentSaveId);
      flow.appendChild(node);
    }
    el.appendChild(flow);
    return el;
  }
  async function stLoadWorldline() {
    const wraps = Array.from(document.querySelectorAll('.st-worldline'));
    if (!wraps.length) return;
    if (!tavernSession) {
      for (const w of wraps) w.innerHTML = stEmpty('先打开会话', '选择会话后可见世界线');
      return;
    }
    for (const w of wraps) w.innerHTML = stSkeleton(1);
    try {
      const worldlineData = await stApi('/sessions/' + encodeURIComponent(tavernSession.sessionId) + '/worldline');
      const lines = (worldlineData && worldlineData.lines) || [];
      const totalSaves = lines.reduce((n, l) => n + ((l.saves || []).length), 0);
      for (const w of wraps) {
        w.innerHTML = '';
        if (!lines.length || !totalSaves) {
          w.innerHTML = stEmpty('暂无存档点', '在会话中保存进度后，这里会出现世界线分支');
          continue;
        }
        const ordered = lines.slice().sort((a, b) => (!!a.forkFromSaveId) - (!!b.forkFromSaveId));
        for (const line of ordered) w.appendChild(stRenderWorldlineLine(line, worldlineData));
      }
    } catch (e) {
      for (const w of wraps) w.innerHTML = '<div class="muted sm">世界线加载失败</div>';
      console.warn(e);
    }
  }


  if ($('st-save-create')) $('st-save-create').onclick = () => stCreateSave();
  if ($('st-drawer-save-create')) $('st-drawer-save-create').onclick = () => stCreateSave();


  async function stExportPackZip() {
    if (!tavernPack || !tavernPack.id) { stStatus('先选择 Pack'); return; }
    try {
      const headers = {};
      if (token) { headers.Authorization = 'Bearer ' + token; headers['X-Mobile-Token'] = token; }
      const res = await fetch(apiBase() + '/api/v1/story-tavern/packs/' + encodeURIComponent(tavernPack.id) + '/export.zip', { headers, cache: 'no-store' });
      if (!res.ok) throw new Error('HTTP ' + res.status);
      const blob = await res.blob();
      const a = document.createElement('a');
      a.href = URL.createObjectURL(blob);
      a.download = (tavernPack.id || 'pack') + '.zip';
      document.body.appendChild(a); a.click(); a.remove();
      stStatus('已导出 ' + a.download);
    } catch (e) { stStatus('导出失败：' + e.message); }
  }
  function stFileToBase64(file) {
    return new Promise((resolve, reject) => {
      const r = new FileReader();
      r.onload = () => {
        const s = String(r.result || '');
        const i = s.indexOf(',');
        resolve(i >= 0 ? s.slice(i + 1) : s);
      };
      r.onerror = () => reject(new Error('读取失败'));
      r.readAsDataURL(file);
    });
  }
  async function stImportPackZip(file) {
    const b64 = await stFileToBase64(file);
    const saved = await stApi('/packs/import', { method: 'POST', body: JSON.stringify({ zipBase64: b64 }) });
    await stLoadPacks();
    stStatus('已导入 Pack：' + (saved.title || saved.id));
    return saved;
  }


  if ($('st-pack-export')) $('st-pack-export').onclick = () => stExportPackZip();
  if ($('st-zip-file')) {
    $('st-zip-file').onchange = async (e) => {
      const file = e.target.files && e.target.files[0]; if (!file) return;
      try { await stImportPackZip(file); e.target.value = ''; }
      catch (err) { stStatus('ZIP 导入失败：' + err.message); }
    };
  }



  function stPackChars(pack) {
    return (pack && Array.isArray(pack.characters)) ? pack.characters : [];
  }

  /** Resolve character id → display name. Prefer full pack.characters; fall back to id tail. */
  function stCharNameOf(id, pack) {
    const raw = String(id || '').trim();
    if (!raw) return '';
    const chars = stPackChars(pack);
    const hit = chars.find((c) => c && c.id === raw);
    if (hit && hit.name && String(hit.name).trim()) return String(hit.name).trim();
    // soft match by suffix (legacy random ids)
    const soft = chars.find((c) => c && c.id && (raw.endsWith(c.id) || c.id.endsWith(raw)));
    if (soft && soft.name && String(soft.name).trim()) return String(soft.name).trim();
    // hide narrator/player technical labels
    if (/narrator/i.test(raw)) return '旁白';
    if (/player|reader/i.test(raw)) return '读者';
    // last resort: short id, not full uuid-ish
    if (raw.length > 18) return raw.slice(0, 10) + '…';
    return raw;
  }

  function stIsPlayableCastId(id, pack) {
    const c = stPackChars(pack).find((x) => x && x.id === id);
    if (!c) return false;
    const role = String(c.role || '').toLowerCase();
    const name = String(c.name || '').trim();
    if (role.includes('narrator') || role.includes('player')) return false;
    if (name === '旁白' || name === '读者' || name === '玩家') return false;
    // junk auto names
    if (/^(露出|眼角|换鞋|随口|轻声|低头)/.test(name)) return false;
    if (name.length < 2 || name.length > 8) return false;
    return true;
  }

  const stFullPackRetried = new Set();
  async function stEnsureFullPack(packId) {
    if (!packId) return null;
    let pack = tavernPacks.find((p) => p.id === packId) || (tavernPack && tavernPack.id === packId ? tavernPack : null);
    if (pack && Array.isArray(pack.characters) && pack.characters.length) {
      tavernPack = pack;
      return pack;
    }
    const fetchFull = async () => {
      const full = await stApi('/packs/' + encodeURIComponent(packId));
      const idx = tavernPacks.findIndex((p) => p.id === packId);
      if (idx >= 0) tavernPacks[idx] = { ...tavernPacks[idx], ...full };
      else tavernPacks.push(full);
      tavernPack = full;
      return full;
    };
    try {
      const full = await fetchFull();
      stFullPackRetried.delete(packId);
      return full;
    } catch (e) {
      console.warn('stEnsureFullPack', e);
      // pack may be transiently unavailable (just imported / dir race): retry once
      // so wand focus/vessel lists don't stay thin. Never loop on a hard 404.
      if (!stFullPackRetried.has(packId)) {
        stFullPackRetried.add(packId);
        try {
          return await fetchFull();
        } catch (e2) {
          console.warn('stEnsureFullPack retry failed', e2);
        }
      }
      return pack || null;
    }
  }

  function stRenderFocusBar() {
    const bar = $('st-focus-bar');
    const chips = $('st-focus-chips');
    if (!bar || !chips) return;
    if (!tavernSession) { bar.classList.add('hidden'); return; }
    bar.classList.remove('hidden');
    const rotBtn = $('st-rot-toggle');
    if (rotBtn) {
      const rotOn = tavernSession.speakerRotation !== false;
      rotBtn.classList.toggle('active', rotOn);
      rotBtn.setAttribute('aria-pressed', rotOn ? 'true' : 'false');
    }
    const vBtn = $('st-vessel-toggle');
    if (vBtn) {
      const vcur = (tavernSession.entry && tavernSession.entry.vesselCharacterId)
        || (tavernSession.player && tavernSession.player.controlCharacterId) || '';
      vBtn.classList.toggle('active', !!vcur);
      vBtn.setAttribute('aria-pressed', !!vcur ? 'true' : 'false');
    }
    const pack = tavernPacks.find(p => p.id === tavernSession.packId) || tavernPack;
    let present = (tavernSession.presentCharacterIds || []).slice();
    // Drop deleted/junk ids that no longer exist on pack; keep order
    if (stPackChars(pack).length) {
      const known = new Set(stPackChars(pack).map((c) => c.id));
      const cleaned = present.filter((id) => known.has(id) && stIsPlayableCastId(id, pack));
      if (cleaned.length) present = cleaned;
      else {
        // fall back to pack cast if session list is all junk
        present = stPackChars(pack).filter((c) => stIsPlayableCastId(c.id, pack)).map((c) => c.id);
      }
    }
    const focus = tavernSession.focusCharacterId || '';
    chips.innerHTML = '';
    if (!present.length) {
      chips.innerHTML = '<span class="muted sm">暂无在场角色，可在下方选择容器角色继续</span>';
      return;
    }
    for (const id of present) {
      const btn = document.createElement('button');
      btn.type = 'button';
      const isFocus = id === focus;
      btn.className = 'st-focus-chip' + (isFocus ? ' active' : '');
      const label = stCharNameOf(id, pack);
      btn.textContent = isFocus ? (label + ' · 焦点') : label;
      btn.title = label + (isFocus ? '（当前焦点）' : '（点击设为焦点）');
      btn.dataset.characterId = id;
      btn.onclick = () => stSetFocus(id);
      chips.appendChild(btn);
    }
  }
  async function stSetFocus(characterId) {
    if (!tavernSession || tavernStreaming) return;
    try {
      const body = { characterId };
      if ($('st-speaker-rot')) body.speakerRotation = !!$('st-speaker-rot').checked;
      const s = await stApi('/sessions/' + encodeURIComponent(tavernSession.sessionId) + '/focus', {
        method: 'POST', body: JSON.stringify(body)
      });
      tavernSession = s;
      stRenderFocusBar();
    stFillVesselSelect();
      // Mask 面具（Omate 对齐）：焦点即身份——切换后同步刷新立绘/角色背景
      try { stRenderSprite(); } catch (_) {}
      try { if (window.stRefreshImmerseBg) stRefreshImmerseBg(); } catch (_) {}
      const _fp = tavernPacks.find(p => p.id === tavernSession.packId) || tavernPack;
      stStatus(`${tavernSession.title || '故事馆'} · 焦点 ${stCharNameOf(tavernSession.focusCharacterId, _fp) || '-'} · ${stTurnLabel(tavernSession.turn || 0)}`);
    } catch (e) { stStatus('切换焦点失败：' + e.message); }
  }


  const _rotBtn = $('st-rot-toggle');
  if (_rotBtn) _rotBtn.onclick = () => {
    if (!tavernSession) return;
    const next = !_rotBtn.classList.contains('active');
    _rotBtn.classList.toggle('active', next);
    _rotBtn.setAttribute('aria-pressed', next ? 'true' : 'false');
    stApi('/sessions/' + encodeURIComponent(tavernSession.sessionId) + '/focus', {
      method: 'POST', body: JSON.stringify({ speakerRotation: next, characterId: tavernSession.focusCharacterId || undefined })
    }).then(s => { tavernSession = s; stRenderFocusBar(); stFillVesselSelect(); }).catch(e => stStatus('轮流设置失败：' + e.message));
  };


  function stFillVesselSelect() {
    const picker = $('st-vessel-picker');
    if (!picker || !tavernSession) return;
    const pack = tavernPacks.find(p => p.id === tavernSession.packId) || tavernPack;
    let chars = stPackChars(pack).filter((c) => {
      const role = String(c.role || '').toLowerCase();
      const n = String(c.name || '').trim();
      if (role.includes('narrator')) return false;
      if (n === '旁白') return false;
      return !!(c.id && n);
    });
    const cur = (tavernSession.entry && tavernSession.entry.vesselCharacterId)
      || (tavernSession.player && tavernSession.player.controlCharacterId)
      || '';
    picker.innerHTML = '';
    const title = document.createElement('div');
    title.className = 'st-vessel-picker-title';
    title.textContent = chars.length ? '选择容器角色' : (pack ? '（本包无可用角色）' : '（加载人物中…）');
    picker.appendChild(title);
    const mkOpt = (id, label) => {
      const b = document.createElement('button');
      b.type = 'button';
      b.className = 'st-vessel-opt' + (id === cur ? ' active' : '');
      b.textContent = label;
      b.onclick = () => stRebindVessel(id);
      return b;
    };
    picker.appendChild(mkOpt('', '不附身（旁白视角）'));
    for (const c of chars) picker.appendChild(mkOpt(c.id, stCharNameOf(c.id, pack)));
  }
  async function stRebindVessel(vesselCharacterId) {
    if (!tavernSession || tavernStreaming) return;
    try {
      const s = await stApi('/sessions/' + encodeURIComponent(tavernSession.sessionId) + '/rebind-vessel', {
        method: 'POST', body: JSON.stringify({ vesselCharacterId: vesselCharacterId || null })
      });
      tavernSession = s;
      stRenderMessages();
      stRenderFocusBar();
      stFillVesselSelect();
      const _vp = tavernPacks.find(p => p.id === s.packId) || tavernPack;
      stStatus('已换壳 · ' + (stCharNameOf(vesselCharacterId || '', _vp) || '不附身'));
      const _m = document.getElementById('st-wand-menu');
      const _b = document.getElementById('st-wand-btn');
      if (_m) _m.classList.add('hidden');
      if (_b) _b.setAttribute('aria-expanded', 'false');
    } catch (e) { stStatus('换壳失败：' + e.message); }
  }


  const vToggle = $('st-vessel-toggle');
  if (vToggle) vToggle.onclick = () => {
    if (!tavernSession) return;
    const picker = $('st-vessel-picker');
    if (!picker) return;
    const opening = picker.classList.contains('hidden');
    picker.classList.toggle('hidden', !opening);
    if (opening) stFillVesselSelect();
  };


  let stNodeEditId = null;
  function stParseExitsText(text) {
    const exits = [];
    const lines = String(text || '').split(/\n+/);
    let i = 0;
    for (const line of lines) {
      const t = line.trim();
      if (!t) continue;
      const parts = t.split('|');
      const when = (parts[0] || '').trim();
      const next = (parts[1] || '').trim();
      if (!when || !next) continue;
      i += 1;
      exits.push({ id: 'e' + i, when, next });
    }
    return exits;
  }
  function stExitsToText(exits) {
    return (exits || []).map(e => (e.when || '') + '|' + (e.next || '')).join('\n');
  }
  function stRenderNodes() {
    const panel = $('st-node-panel');
    const list = $('st-node-list');
    if (!panel || !list) return;
    if (!tavernPack) { panel.classList.add('hidden'); return; }
    panel.classList.remove('hidden');
    const nodes = tavernPack.nodes || [];
    list.innerHTML = '';
    if (!nodes.length) {
      list.innerHTML = '<div class="muted sm">暂无节点，可添加</div>';
      return;
    }
    for (const n of nodes) {
      const el = document.createElement('div');
      el.className = 'item' + (stNodeEditId === n.id ? ' active' : '');
      const exits = (n.exit || []).map(e => e.next).filter(Boolean).join('→') || '（无出口）';
      el.innerHTML = '<span class="t"></span><small></small><div class="ex"></div>';
      el.querySelector('.t').textContent = (n.title || n.id) + ' · ' + (n.id || '');
      el.querySelector('small').textContent = '章 ' + (n.chapterId || '?');
      el.querySelector('.ex').textContent = '→ ' + exits;
      el.onclick = () => stOpenNodeEditor(n.id);
      list.appendChild(el);
    }
  }
  function stOpenNodeEditor(nodeId) {
    if (!tavernPack) return;
    stNodeEditId = nodeId || null;
    $('st-node-editor').classList.remove('hidden');
    $('st-node-msg').textContent = '';
    const n = (tavernPack.nodes || []).find(x => x.id === nodeId);
    if (!n) {
      // new
      const ch0 = (tavernPack.chapters && tavernPack.chapters[0] && tavernPack.chapters[0].id) || 'ch01';
      const nid = 'n' + Date.now().toString().slice(-4);
      $('st-node-id').value = nid;
      $('st-node-id').disabled = false;
      $('st-node-chapter').value = ch0;
      $('st-node-title').value = '新节点';
      $('st-node-entry').value = '';
      $('st-node-summary').value = '';
      $('st-node-exits').value = '';
      stNodeEditId = null;
    } else {
      $('st-node-id').value = n.id || '';
      $('st-node-id').disabled = true;
      $('st-node-chapter').value = n.chapterId || '';
      $('st-node-title').value = n.title || '';
      $('st-node-entry').value = n.entry || '';
      $('st-node-summary').value = n.summary || '';
      $('st-node-exits').value = stExitsToText(n.exit || []);
    }
    stRenderNodes();
  }
  async function stSaveNode() {
    if (!tavernPack) return;
    const id = ($('st-node-id').value || '').trim();
    const chapterId = ($('st-node-chapter').value || '').trim();
    const title = ($('st-node-title').value || '').trim();
    if (!id || !chapterId || !title) {
      $('st-node-msg').textContent = 'ID/章节/标题必填';
      return;
    }
    const node = {
      id,
      chapterId,
      title,
      entry: ($('st-node-entry').value || '').trim(),
      summary: ($('st-node-summary').value || '').trim(),
      exit: stParseExitsText($('st-node-exits').value),
      lockedBeats: [],
      allowedDivergence: 'branch',
      presentCharacters: (tavernPack.characters || []).map(c => c.id).slice(0, 4),
    };
    const nodes = Array.isArray(tavernPack.nodes) ? tavernPack.nodes.slice() : [];
    const idx = nodes.findIndex(n => n.id === id);
    if (idx >= 0) nodes[idx] = { ...nodes[idx], ...node };
    else nodes.push(node);
    // keep chapter.nodeIds in sync lightly
    const chapters = (tavernPack.chapters || []).map(ch => {
      const c = { ...ch, nodeIds: Array.isArray(ch.nodeIds) ? ch.nodeIds.slice() : [] };
      if (c.id === chapterId && !c.nodeIds.includes(id)) c.nodeIds.push(id);
      return c;
    });
    tavernPack = { ...tavernPack, nodes, chapters, updatedAt: new Date().toISOString() };
    try {
      const payload = JSON.parse(JSON.stringify(tavernPack));
      delete payload.uploadChapters;
      const saved = await stApi('/packs', { method: 'POST', body: JSON.stringify(payload) });
      tavernPack = saved;
      stNodeEditId = id;
      $('st-node-id').disabled = true;
      $('st-node-msg').textContent = '已保存';
      stRenderNodes();
      stStatus('节点已保存 · ' + id);
    } catch (e) {
      $('st-node-msg').textContent = '保存失败：' + e.message;
    }
  }
  async function stDeleteNode() {
    if (!tavernPack) return;
    const id = ($('st-node-id').value || '').trim();
    if (!id) return;
    if (!await showConfirm('删除节点 ' + id + '？')) return;
    const nodes = (tavernPack.nodes || []).filter(n => n.id !== id);
    const chapters = (tavernPack.chapters || []).map(ch => ({
      ...ch,
      nodeIds: (ch.nodeIds || []).filter(nid => nid !== id),
    }));
    // scrub exits pointing to deleted
    for (const n of nodes) {
      n.exit = (n.exit || []).filter(e => e.next !== id);
    }
    tavernPack = { ...tavernPack, nodes, chapters, updatedAt: new Date().toISOString() };
    try {
      const payload = JSON.parse(JSON.stringify(tavernPack));
      delete payload.uploadChapters;
      const saved = await stApi('/packs', { method: 'POST', body: JSON.stringify(payload) });
      tavernPack = saved;
      $('st-node-editor').classList.add('hidden');
      stNodeEditId = null;
      stRenderNodes();
      stStatus('节点已删除 · ' + id);
    } catch (e) {
      $('st-node-msg').textContent = '删除失败：' + e.message;
    }
  }


  if ($('st-node-add')) $('st-node-add').onclick = () => stOpenNodeEditor(null);
  if ($('st-node-save')) $('st-node-save').onclick = () => stSaveNode();
  if ($('st-node-cancel')) $('st-node-cancel').onclick = () => { $('st-node-editor').classList.add('hidden'); stNodeEditId = null; stRenderNodes(); };
  if ($('st-node-del')) $('st-node-del').onclick = () => stDeleteNode();

  // ===== End Story Tavern =====

  // ── 列表搜索/过滤（P0：三处列表统一） ──────────────────────────

