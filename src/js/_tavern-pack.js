  // S13g: playMode 本地化（对齐后端 PlayMode 枚举 Mainline/Free/Side，crates/kaleido-core/src/story_tavern.rs:75）
  const PLAY_MODE_LABELS = { mainline: '主线', free: '自由', side: '支线' };
  const stTurnLabel = (n) => `第${n}回合`;

  async function stRefresh() {
    try {
      const banner = $('st-adult-banner');
      const ok = adultOk();
      if (banner) banner.classList.toggle('hidden', ok);
      // P0: gate — 未确认前不加载 PACK / 会话，banner 是唯一可见内容
      const layout = $('st-layout');
      if (layout) layout.classList.toggle('st-gated', !ok);
      if (!ok) {
        stStatus('');
        return;
      }
      await stLoadPacks();
      await stLoadSessions();
      // S8.10: stay on entry; resume only via explicit history click (stLoadSession)
      if (!window._stSkipEntryReset) stSwitchView('entry');
      exitImmersive();
      stRenderContinueCard();
      if (tavernSessions.length) {
        stStatus('选择剧本开局，或点「继续上次」进入剧场 · ' + tavernSessions.length + ' 场会话');
      } else {
        stStatus('选择玩法新建一场');
      }
    } catch (e) { console.warn('stRefresh failed', e); stStatus('加载失败：' + e.message); }
  }

  function stSyncExpandBtn() {
    const btn = $('st-side-expand-all');
    if (!btn) return;
    const heads = document.querySelectorAll('#st-pack-list .st-pack-group-head, #st-session-list .st-session-group-head');
    const anyClosed = Array.from(heads).some(h => !h.classList.contains('open'));
    btn.textContent = anyClosed ? '全部展开' : '全部收起';
  }

  async function stLoadPacks() {
    const list = $('st-pack-list');
    // 已有缓存时直接渲染缓存列表，避免每次进档案馆闪现骨架屏（用户反馈
    // "进入档案馆先闪加载界面才闪回档案柜"）；API 返回后静默刷新。
    if (list && (!tavernPacks || tavernPacks.length === 0)) {
      list.innerHTML = stSkeleton(3);
    }
    let data;
    try {
      data = await stApi('/packs');
    } catch (e) {
      if (list) {
        list.innerHTML = '';
        const el = document.createElement('div');
        el.className = 'st-empty';
        el.innerHTML =
          '<svg xmlns="http://www.w3.org/2000/svg" width="28" height="28" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="m16 6 4 14"/><path d="M12 6v14"/><path d="M8 8v12"/><path d="M4 4v16"/></svg>' +
          '<span>加载失败</span>' +
          '<button type="button" class="ghost sm st-pack-retry-btn">点击重试</button>';
        const btn = el.querySelector('.st-pack-retry-btn');
        if (btn) btn.onclick = () => { stLoadPacks(); };
        list.appendChild(el);
      }
      throw e;
    }
    tavernPacks = data.packs || [];
    // Prefer real works: more chapters, novel/demo, stable title
    tavernPacks.sort((a, b) => {
      const score = (p) => {
        const ch = (p.chapters && p.chapters.length) || p.chapterCount || 0;
        const src = String(p.sourceType || (p.source && p.source.type) || '');
        let s = ch * 10;
        if (src === 'novel') s += 1000;
        if (src === 'demo' || p.id === 'demo-rain-alley') s += 500;
        if (/smoke|S8-|AZ|zip-imp/i.test(p.title || '') || /smoke|zip-imp|pack-az/i.test(p.id || '')) s -= 5000;
        return s;
      };
      return score(b) - score(a) || String(a.title || '').localeCompare(String(b.title || ''), 'zh');
    });
    if (!list) return;
    list.innerHTML = '';
    const sel = $('st-wizard-pack'); sel.innerHTML = '';
    const ensure = document.createElement('option'); ensure.value = ''; ensure.textContent = '（选择 Pack）'; sel.appendChild(ensure);
    if (!tavernPacks.length) {
      list.innerHTML = stEmpty('暂无 Pack', '导入或新建以开始');
      return;
    }
    // Wizard dropdown stays flat (one option per pack)
    for (const p of tavernPacks) {
      const chapters = (p.chapters && p.chapters.length) || p.chapterCount || 0;
      const opt = document.createElement('option'); opt.value = p.id;
      opt.textContent = stDisplayTitle(p.title || p.id) + (chapters ? `（${chapters}章）` : '');
      sel.appendChild(opt);
    }
    // Build one pack row (shared by flat + grouped rendering)
    const stMakePackItem = (p, nested) => {
      const el = document.createElement('div');
      el.className = 'item st-pack-item' + (nested ? ' st-pack-item-nested' : '') + (tavernPack && tavernPack.id === p.id ? ' active' : '');
      el.dataset.packId = p.id;
      const chapters = (p.chapters && p.chapters.length) || p.chapterCount || 0;
      const nodes = (p.nodes && p.nodes.length) || p.nodeCount || 0;
      const cast = stCleanCast(p);
      const blurb = stPackBlurb(p);
      const firstCh = p.firstChapterTitle || (p.chapters && p.chapters[0] && p.chapters[0].title) || '';
      const demoBadge = p.id === 'demo-rain-alley' ? '<span class="st-badge demo">Demo</span>' : '';
      const srcBadge = `<span class="st-badge src">${escapeHtml(stSourceLabel(p))}</span>`;
      const title = stDisplayTitle(p.title || p.id);
      const metaBits = [];
      if (chapters) metaBits.push(chapters + ' 章');
      if (nodes) metaBits.push(nodes + ' 节点');
      if (!cast.length && p.characterCount) metaBits.push(p.characterCount + ' 角色');
      if (firstCh) metaBits.push('起「' + firstCh + '」');
      // P0-3: cast on its own line so book title/meta stays readable
      const castLine = cast.length
        ? `<span class="d2">${escapeHtml(cast.slice(0, 3).join(' · '))}</span>`
        : '';
      // P1-4: strip markdown emphasis/asterisks from blurb before display
      const blurbClean = blurb ? blurb.replace(/\*{1,3}([^*]+)\*{1,3}/g, '$1').replace(/#{1,6}\s*/g, '').trim() : '';
      const blurbLine = blurbClean
        ? `<span class="b">${escapeHtml(blurbClean.length > 72 ? blurbClean.slice(0, 72) + '…' : blurbClean)}</span>`
        : `<span class="b muted">暂无简介 — 点开可看章节目录</span>`;
      el.innerHTML =
        `<span class="t">${stIcon('book')} <span class="tt">${escapeHtml(title)}</span> ${demoBadge}${srcBadge}</span>` +
        `<span class="d">${escapeHtml(metaBits.join(' · ') || p.id)}</span>` +
        castLine +
        blurbLine;
      el.title = (p.title || p.id) + (blurb ? '\n' + blurb : '');
      el.onclick = () => stShowPack(p.id);
      return el;
    };
    // Group key: collapse variants of the same book/series into one row
    const stPackGroupKey = (p) => {
      const t = String(p.title || '').trim();
      const id = String(p.id || '');
      if (/^S8-\d/.test(t)) return 'S8 系列';
      if (id === 'demo-rain-alley' || t === '雨巷来客' || t.indexOf('雨巷来客') === 0) return '雨巷来客';
      if (t === '白昼之下' || t === '白昼' || t.indexOf('白昼·') === 0) return '白昼';
      return t || id;
    };
    const groups = new Map();
    for (const p of tavernPacks) {
      const key = stPackGroupKey(p);
      if (!groups.has(key)) groups.set(key, []);
      groups.get(key).push(p);
    }
    for (const [gkey, members] of groups) {
      if (members.length === 1) {
        list.appendChild(stMakePackItem(members[0], false));
        continue;
      }
      // Collapsible group: head row + nested variant rows
      const g = document.createElement('div');
      g.className = 'st-pack-group';
      const totalCh = members.reduce((s, p) => s + ((p.chapters && p.chapters.length) || p.chapterCount || 0), 0);
      const head = document.createElement('button');
      head.type = 'button';
      head.className = 'st-pack-group-head';
      head.innerHTML =
        `<span class="st-pack-group-title">${escapeHtml(stDisplayTitle(gkey))}</span>` +
        `<span class="st-pack-group-meta">${members.length} 个版本 · ${totalCh} 章</span>` +
        `<span class="st-pack-group-arrow" aria-hidden="true">▸</span>`;
      head.onclick = () => {
        const open = head.classList.toggle('open');
        g.classList.toggle('open', open);
        head.setAttribute('aria-expanded', open ? 'true' : 'false');
        stSyncExpandBtn();
      };
      head.setAttribute('aria-expanded', 'false');
      const body = document.createElement('div');
      body.className = 'st-pack-group-body';
      for (const p of members) body.appendChild(stMakePackItem(p, true));
      g.appendChild(head);
      g.appendChild(body);
      list.appendChild(g);
    }
    if (tavernPack && tavernPack.id) {
      const w = $('st-wizard-pack');
      if (w) w.value = tavernPack.id;
    }
  }

  function stRenderPackDetail(full, previewText) {
    const titleEl = $('st-pack-detail-title');
    const metaEl = $('st-pack-detail-meta');
    const blurbEl = $('st-pack-detail-blurb');
    const castEl = $('st-pack-detail-cast');
    const chEl = $('st-pack-detail-chapters');
    const bodyEl = $('st-pack-detail-body');
    const chTitle = $('st-pack-detail-ch-title');
    const chMeta = $('st-pack-detail-ch-meta');
    if (!titleEl) return;
    const title = stDisplayTitle(full.title || full.id);
    titleEl.textContent = title;
    const chapters = full.chapters || [];
    const cast = stCleanCast(full);
    const blurb = stPackBlurb(full);
    const tier = full.maxTier || 'standard';
    metaEl.textContent = [
      stSourceLabel(full),
      chapters.length + ' 章',
      (full.nodes || []).length + ' 节点',
      cast.length ? cast.length + ' 人' : '',
      '分级 ' + tier,
      full.id,
    ].filter(Boolean).join(' · ');
    blurbEl.textContent = blurb || '（无简介。可在 Lore 添加「简介」永久条。）';
    castEl.innerHTML = cast.length
      ? cast.map((n) => `<span class="st-cast-chip">${escapeHtml(n)}</span>`).join('')
      : '<span class="muted sm">暂无具名角色</span>';
    chEl.innerHTML = '';
    if (!chapters.length) {
      chEl.innerHTML = '<div class="muted sm">无章节</div>';
    } else {
      chapters.forEach((ch, i) => {
        const btn = document.createElement('button');
        btn.type = 'button';
        btn.className = 'st-ch-chip' + (i === 0 ? ' active' : '');
        btn.textContent = (i + 1) + '. ' + (ch.title || ch.id);
        btn.title = ch.title || ch.id;
        btn.onclick = () => {
          chEl.querySelectorAll('.st-ch-chip').forEach((x) => x.classList.remove('active'));
          btn.classList.add('active');
          stPreviewPackChapter(full.id, ch);
        };
        chEl.appendChild(btn);
      });
    }
    if (chTitle) chTitle.textContent = chapters[0] ? ('预览 · ' + (chapters[0].title || chapters[0].id)) : '章节预览';
    if (chMeta) chMeta.textContent = chapters[0] ? (chapters[0].id || '') : '';
    if (bodyEl) bodyEl.textContent = previewText || '加载中…';
  }

  async function stPreviewPackChapter(packId, ch) {
    const bodyEl = $('st-pack-detail-body');
    const chTitle = $('st-pack-detail-ch-title');
    const chMeta = $('st-pack-detail-ch-meta');
    if (chTitle) chTitle.textContent = '预览 · ' + (ch.title || ch.id);
    if (chMeta) chMeta.textContent = ch.id || '';
    if (bodyEl) bodyEl.textContent = '正文加载中…';
    const side = $('st-chapter-view');
    if (side) {
      side.classList.remove('hidden');
      side.dataset.chapterId = ch.id;
      const pre = side.querySelector('pre');
      if (pre) pre.textContent = '章节：' + (ch.title || ch.id) + '\n正文加载中…';
    }
    try {
      const body = await stApi('/packs/' + encodeURIComponent(packId) + '/chapters/' + encodeURIComponent(ch.bodyPath));
      const text = (body.content || '').slice(0, 1200);
      if (bodyEl) bodyEl.textContent = text || '（空章节）';
      if (side) {
        const pre = side.querySelector('pre');
        if (pre) pre.textContent = text || '（空章节）';
      }
    } catch (e) {
      if (bodyEl) bodyEl.textContent = '读取失败：' + e.message;
    }
  }

  async function stShowPack(id) {
    try {
      stStatus('加载 Pack…');
      const full = await stApi('/packs/' + encodeURIComponent(id));
      tavernPack = full;
      const idx = tavernPacks.findIndex(x => x.id === id);
      if (idx >= 0) tavernPacks[idx] = {
        ...tavernPacks[idx],
        ...full,
        chapterCount: (full.chapters || []).length,
        nodeCount: (full.nodes || []).length,
        castNames: stCleanCast(full),
        blurb: stPackBlurb(full),
        firstChapterTitle: (full.chapters && full.chapters[0] && full.chapters[0].title) || '',
      };
      document.querySelectorAll('#st-pack-list .item').forEach((el) => {
        el.classList.toggle('active', el.dataset.packId === id);
      });
      const w = $('st-wizard-pack');
      if (w) w.value = id;
      stRenderLore();
      stRenderNodes();
      stRenderPackDetail(full, '加载中…');
      // 档案馆内切换：隐藏列表，显示详情
      const listview = $('st-packs-listview');
      const packDetail = $('st-view-pack');
      if (listview) listview.classList.add('hidden');
      if (packDetail) packDetail.classList.remove('hidden');
      // 滚到顶部
      const page = document.querySelector('#tab-packs .st-packs-page');
      if (page) page.scrollTop = 0;
      const ch0 = (full.chapters || [])[0];
      if (ch0) await stPreviewPackChapter(id, ch0);
      else {
        const bodyEl = $('st-pack-detail-body');
        if (bodyEl) bodyEl.textContent = '节点：' + (full.nodes || []).length + ' · 章节：0';
      }
      const cast = stCleanCast(full).slice(0, 3).join('·') || '无具名角色';
      stStatus(`${stDisplayTitle(full.title)} · ${(full.chapters || []).length}章 · ${cast} — 可「用此包开玩」`);
    } catch (e) {
      stStatus('加载 Pack 失败：' + e.message);
    }
  }

  async function stLoadSessions() {
    const lists = Array.from(document.querySelectorAll('.st-session-list'));
    if (!lists.length) {
      const data = await stApi('/sessions');
      tavernSessions = data.sessions || [];
      stRenderContinueCard();
      return;
    }
    // 已有缓存时直接渲染缓存列表，避免每次进档案馆闪现骨架屏
    if (!tavernSessions || tavernSessions.length === 0) {
      for (const l of lists) l.innerHTML = stSkeleton(2);
    }
    const data = await stApi('/sessions');
    tavernSessions = data.sessions || [];
    stRenderContinueCard();
    for (const l of lists) stRenderSessionsList(l);
    stSyncExpandBtn();
  }

  function stRenderSessionsList(list) {
    list.innerHTML = '';
    if (!tavernSessions.length) {
      list.innerHTML = stEmpty('还没有会话', '选择玩法新建一场');
      return;
    }
    // Build one session row (shared by flat + grouped rendering)
    const stMakeSessionItem = (s, nested) => {
      const el = document.createElement('div');
      el.className = 'item' + (nested ? ' st-session-item-nested' : '') + (tavernSession && tavernSession.sessionId === s.sessionId ? ' active' : '');
      const mode = PLAY_MODE_LABELS[s.playMode] || s.playMode || '-';
      const badge = s.packMissing ? '<span class="st-badge" style="background:rgba(248,113,113,.12);color:#fecaca;border-color:rgba(248,113,113,.35)">只读</span>' : '';
      el.innerHTML = `<span class="t">${stIcon('bookmark')} ${escapeHtml(s.title || s.sessionId)} ${badge}</span><span class="d">${PLAYABLE_LABELS[s.playable] || s.playable} · ${mode} · <span class="st-badge turn">${stTurnLabel(s.turn || 0)}</span></span>`;
      el.onclick = () => stLoadSession(s.sessionId);
      return el;
    };
    // Group key: same session title (same story / variant) collapses into one row
    const stSessionGroupKey = (s) => String(s.title || s.sessionId || '').trim();
    const groups = new Map();
    for (const s of tavernSessions) {
      const key = stSessionGroupKey(s);
      if (!groups.has(key)) groups.set(key, []);
      groups.get(key).push(s);
    }
    // Collect single-session titles separately; cap how many show flat
    const singles = [];
    const multiGroups = [];
    for (const [gkey, members] of groups) {
      if (members.length === 1) singles.push(members[0]);
      else multiGroups.push([gkey, members]);
    }
    const MAX_FLAT = 0;
    // Flat single sessions: show a few directly, fold the rest into a collapsible group
    singles.slice(0, MAX_FLAT).forEach((s) => list.appendChild(stMakeSessionItem(s, false)));
    if (singles.length > MAX_FLAT) {
      const sg = document.createElement('div');
      sg.className = 'st-session-group';
      const sHead = document.createElement('button');
      sHead.type = 'button';
      sHead.className = 'st-session-group-head';
      sHead.innerHTML =
        `<span class="st-session-group-title">其他会话</span>` +
        `<span class="st-session-group-meta">${singles.length - MAX_FLAT} 场</span>` +
        `<span class="st-session-group-arrow" aria-hidden="true">▸</span>`;
      sHead.onclick = () => {
        const open = sHead.classList.toggle('open');
        sg.classList.toggle('open', open);
        sHead.setAttribute('aria-expanded', open ? 'true' : 'false');
        stSyncExpandBtn();
      };
      sHead.setAttribute('aria-expanded', 'false');
      const sBody = document.createElement('div');
      sBody.className = 'st-session-group-body';
      singles.slice(MAX_FLAT).forEach((s) => sBody.appendChild(stMakeSessionItem(s, true)));
      sg.appendChild(sHead);
      sg.appendChild(sBody);
      list.appendChild(sg);
    }
    for (const [gkey, members] of multiGroups) {
      const g = document.createElement('div');
      g.className = 'st-session-group';
      const head = document.createElement('button');
      head.type = 'button';
      head.className = 'st-session-group-head';
      head.innerHTML =
        `<span class="st-session-group-title">${escapeHtml(gkey)}</span>` +
        `<span class="st-session-group-meta">${members.length} 场</span>` +
        `<span class="st-session-group-arrow" aria-hidden="true">▸</span>`;
      head.onclick = () => {
        const open = head.classList.toggle('open');
        g.classList.toggle('open', open);
        head.setAttribute('aria-expanded', open ? 'true' : 'false');
        stSyncExpandBtn();
      };
      // All groups default collapsed; user expands on demand
      head.setAttribute('aria-expanded', 'false');
      const body = document.createElement('div');
      body.className = 'st-session-group-body';
      const MAX_NESTED = 3; // show only first N variants; rest behind "more" button
      members.slice(0, MAX_NESTED).forEach((s) => body.appendChild(stMakeSessionItem(s, true)));
      if (members.length > MAX_NESTED) {
        const more = document.createElement('button');
        more.type = 'button';
        more.className = 'st-group-more';
        more.textContent = `＋ ${members.length - MAX_NESTED} 场更早会话`;
        more.onclick = () => {
          members.slice(MAX_NESTED).forEach((s) => body.appendChild(stMakeSessionItem(s, true)));
          more.remove();
        };
        body.appendChild(more);
      }
      g.appendChild(head);
      g.appendChild(body);
      list.appendChild(g);
    }
  }

  async function stLoadSession(id) {
    tavernSession = await stApi('/sessions/' + encodeURIComponent(id));
    try { localStorage.setItem(TAVERN_SID_KEY, tavernSession.sessionId); } catch (_) {}
    // R2/R4: 记录会话进入来源（向导创建沿用 stOpenWizard 记下的来源；直接打开按当前视图推断）
    const wizView = $('st-view-wizard');
    if (!(wizView && !wizView.classList.contains('hidden'))) {
      const packDetail = $('st-view-pack');
      stNavFrom = (currentTab === 'packs' && packDetail && !packDetail.classList.contains('hidden')) ? 'packs-detail' : '';
    }
    stHistoryExpanded = false; // S8.25: collapse on each enter
    await stLoadSessions();
    // Need full pack.characters so focus/vessel show 林小宇 not c-c-xxxxx
    if (tavernSession && tavernSession.packId && !tavernSession.packMissing) {
      await stEnsureFullPack(tavernSession.packId);
    }
    $('st-wizard').classList.add('hidden');
    $('st-view-wizard').classList.add('hidden');
    // [fix §7 2026-08-16] 父 tab 可见性保障：play 视图嵌套在 #tab-tavern 内，
    // 从首页/档案馆/向导进入时父 tab 仍 display:none → 视图不可见（URL 变了界面不动）。
    // 切父 tab 后再渲染 play；已在 tavern tab 内调用则零副作用。
    const playHostEl = $('st-view-play') ? $('st-view-play').closest('.tab-panel') : null;
    if (playHostEl && playHostEl.classList.contains('hidden') && typeof switchTab === 'function') {
      await switchTab('tavern');
    }
    stSwitchView('play');
    // S8.26: play 视图已是 main-view 下的全局 overlay，无需切换 tab（避免故事馆↔档案馆互跳导致的闪屏）
    // （注：上面的 fix §7 已处理父 tab 可见性；此处注释保留原意——不重复切 tab 以免闪屏）
    const playEl = $('st-view-play');
    if (playEl) {
      playEl.classList.remove('st-stage-enter');
      void playEl.offsetWidth;
      playEl.classList.add('st-stage-enter');
      window.setTimeout(() => playEl.classList.remove('st-stage-enter'), 400);
    }
    // First open / empty session: ensure opening monologue (backend also seeds on create).
    try {
      const msgs = (tavernSession && tavernSession.messages) || [];
      if (tavernSession && !tavernSession.packMissing && (!msgs.length || !tavernSession.openingSeeded)) {
        const r = await stApi('/sessions/' + encodeURIComponent(tavernSession.sessionId) + '/opening', { method: 'POST', body: '{}' });
        if (r && r.session) tavernSession = r.session;
      }
    } catch (e) { console.warn('ensure opening', e); }
    stRenderMessages({ restoreScroll: true });
    stRenderOptions();
    stRenderFocusBar();
    stRenderRecallBar();
    stFillVesselSelect();
    updateImmersive();
    const pack = tavernPacks.find(x => x.id === tavernSession.packId) || tavernPack;
    const focusName = stCharNameOf(tavernSession.focusCharacterId, pack);
    const sideLabel = tavernSession.sideBranchLabel ? (' · 支线「' + tavernSession.sideBranchLabel + '」') : '';
    stStatus(`${tavernSession.title || '故事馆'} · ${PLAYABLE_LABELS[tavernSession.playable] || ''} · ${PLAY_MODE_LABELS[tavernSession.playMode] || tavernSession.playMode || ''}${sideLabel} · 焦点 ${focusName || '-'} · ${stTurnLabel(tavernSession.turn || 0)} · ${tavernSession.packMissing ? 'Pack 已删 只读' : '可对话'}`);
    stSyncModeToggle();
    if (window.stSyncTierFromSession) window.stSyncTierFromSession();
    stLoadSaves().catch(console.warn);
    // Reflect current session in URL so refresh/share lands back here
    try {
      const deep = '#/tavern/session/' + encodeURIComponent(tavernSession.sessionId);
      if (location.hash !== deep) {
        history.replaceState(null, '', deep);
      }
    } catch (_) {}
    if ((tavernSession.playMode || '').toLowerCase() === 'side' && !tavernSession.sideBranchNodeId) {
      stOpenSidePanel().catch(console.warn);
    } else {
      stCloseSidePanel();
    }
    // S8.27: 会话有活跃 run（发消息后切走再回来的场景）——轮询等 run 完成再渲染，
    // 避免「返回再回去」看到空回复（后端其实已生成成功）。
    const activeRunAtLoad = tavernSession && tavernSession.activeRunId;
    if (activeRunAtLoad) {
      const waitRunId = activeRunAtLoad;
      const waitSid = tavernSession.sessionId;
      stStatus('正在生成…');
      let settled = false;
      for (let attempt = 0; attempt < 30; attempt++) {
        await new Promise((r) => setTimeout(r, 2500));
        let fresh;
        try {
          fresh = await stApi('/sessions/' + encodeURIComponent(waitSid));
        } catch (_) {
          break;
        }
        if (!fresh || !Array.isArray(fresh.messages)) break;
        if (!fresh.activeRunId || fresh.activeRunId !== waitRunId) {
          tavernSession = fresh;
          settled = true;
          break;
        }
      }
      if (settled) {
        stRenderMessages({ restoreScroll: false });
        // S8.30: 恢复场景也滚到新消息开头（用户从开头下滑阅读）
        try { stScrollToLastMsgTop(); } catch (_) {}
        stRenderOptions();
        stRenderFocusBar();
        stRenderRecallBar();
        const msgs = tavernSession.messages || [];
        const last = msgs[msgs.length - 1];
        const lastHasContent =
          last && last.role === 'assistant' && String(last.content || '').trim().length > 0;
        stStatus(lastHasContent
          ? '已恢复完整内容'
          : '上次生成失败（上游繁忙或网络断开），可点「重试」重新生成');
      } else {
        stStatus('仍在生成中，可稍后刷新查看');
      }
    }
  }

  function stRenderContinueCard() {
    const card = $('st-continue-card');
    if (!card) return;
    const s = (tavernSessions && tavernSessions[0]) || null;
    if (!s || !s.sessionId) {
      card.classList.add('hidden');
      card.onclick = null;
      return;
    }
    const titleEl = $('st-continue-title');
    const metaEl = $('st-continue-meta');
    const title = (typeof stDisplayTitle === 'function' ? stDisplayTitle(s.title) : null) || s.title || s.sessionId;
    if (titleEl) titleEl.textContent = title;
    if (metaEl) {
      metaEl.textContent =
        (PLAYABLE_LABELS[s.playable] || s.playable || '会话') +
        ' · ' +
        (PLAY_MODE_LABELS[s.playMode] || s.playMode || '-') +
        ' · ' +
        stTurnLabel(s.turn != null ? s.turn : 0);
    }
    card.classList.remove('hidden');
    card.onclick = (ev) => {
      ev.preventDefault();
      stLoadSession(s.sessionId);
    };
  }

  // P2-8: home page recent sessions — fill the empty lower half
  function relativeTime(iso) {
    if (!iso) return '';
    const d = new Date(iso);
    if (isNaN(d)) return '';
    const diff = Date.now() - d.getTime();
    const m = Math.floor(diff / 60000);
    if (m < 1) return '刚刚';
    if (m < 60) return m + '分钟前';
    const h = Math.floor(m / 60);
    if (h < 24) return h + '小时前';
    const day = Math.floor(h / 24);
    if (day === 1) return '昨天';
    if (day < 7) return day + '天前';
    return Math.floor(day / 7) + '周前';
  }

  async function renderHomeRecent() {
    const wrap = $('home-recent');
    const list = $('home-recent-list');
    const emptyWrap = $('home-recent-empty');
    if (!wrap || !list) return;
    let hasSessions = false;
    // Load sessions if not yet cached
    if (!tavernSessions || !tavernSessions.length) {
      try {
        const data = await stApi('/sessions');
        tavernSessions = data.sessions || [];
      } catch (_) { wrap.classList.add('hidden'); if (emptyWrap) emptyWrap.classList.remove('hidden'); return; }
    }
    const recent = (tavernSessions || []).slice(0, 3);
    if (!recent.length) {
      wrap.classList.add('hidden');
      if (emptyWrap) emptyWrap.classList.remove('hidden');
      const cont = $('home-continue-btn');
      if (cont) cont.textContent = '开始示例对话';
      return;
    }
    wrap.classList.remove('hidden');
    if (emptyWrap) emptyWrap.classList.add('hidden');
    const cont = $('home-continue-btn');
    if (cont) cont.textContent = '继续对话';
    list.innerHTML = '';
    for (const s of recent) {
      const title = (typeof stDisplayTitle === 'function' ? stDisplayTitle(s.title) : null) || s.title || s.sessionId;
      const meta = (PLAY_MODE_LABELS[s.playMode] || s.playMode || '-') + ' · ' + stTurnLabel(s.turn != null ? s.turn : 0) + ' · ' + relativeTime(s.updatedAt || s.createdAt);
      const btn = document.createElement('button');
      btn.type = 'button';
      btn.className = 'home-recent-item';
      btn.innerHTML =
        `<span class="hr-title">${escapeHtml(title)}</span>` +
        `<span class="hr-meta">${escapeHtml(meta)}</span>` +
        `<span class="hr-excerpt">加载中…</span>`;
      btn.onclick = () => { stLoadSession(s.sessionId); };
      list.appendChild(btn);
      // Async load last message excerpt
      (async () => {
        try {
          const detail = await stApi('/sessions/' + encodeURIComponent(s.sessionId));
          const msgs = detail.messages || [];
          const lastAgent = [...msgs].reverse().find((m) => m.role === 'assistant' && (m.content || '').trim());
          const excerpt = lastAgent ? String(lastAgent.content).trim().slice(0, 40) : '';
          const ex = btn.querySelector('.hr-excerpt');
          if (ex) ex.textContent = excerpt ? ('"' + excerpt + '…"') : '（暂无剧情）';
        } catch (_) {
          const ex = btn.querySelector('.hr-excerpt');
          if (ex) ex.textContent = '';
        }
      })();
    }
  }

  function stCosine(a, b) {
    if (!a || !b || !a.length || a.length !== b.length) return 0;
    let dot = 0, na = 0, nb = 0;
    for (let i = 0; i < a.length; i++) {
      const x = a[i], y = b[i];
      dot += x * y; na += x * x; nb += y * y;
    }
    const d = Math.sqrt(na) * Math.sqrt(nb);
    return d ? dot / d : 0;
  }

  function stTokenOverlap(query, text) {
    const q = String(query || '').toLowerCase().replace(/[^\u4e00-\u9fff\w]+/g, ' ').split(/\s+/).filter((t) => t.length > 1);
    const t = String(text || '').toLowerCase();
    if (!q.length) return 0;
    let hit = 0;
    for (const tok of q) if (t.indexOf(tok) >= 0) hit++;
    return hit / q.length;
  }

