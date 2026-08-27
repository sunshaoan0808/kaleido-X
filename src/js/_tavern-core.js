  function stIcon(name) { return ST_ICONS[name] || ''; }
  function stEmpty(text, action) {
    return `<div class="st-empty"><svg aria-hidden="true" xmlns="http://www.w3.org/2000/svg" width="28" height="28" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M7.9 20A9 9 0 1 0 4 16.1L2 22Z"/></svg><span>${escapeHtml(text)}</span>${action ? `<span class="action">${escapeHtml(action)}</span>` : ''}</div>`;
  }

  // P1.1: Unified empty-state component
  const EMPTY_ICONS = {
    chat: '<svg aria-hidden="true" xmlns="http://www.w3.org/2000/svg" width="48" height="48" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M7.9 20A9 9 0 1 0 4 16.1L2 22Z"/></svg>',
    book: '<svg aria-hidden="true" xmlns="http://www.w3.org/2000/svg" width="48" height="48" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M4 19.5v-15A2.5 2.5 0 0 1 6.5 2H19a1 1 0 0 1 1 1v18a1 1 0 0 1-1 1H6.5a1 1 0 0 1 0-5H20"/></svg>',
    user: '<svg aria-hidden="true" xmlns="http://www.w3.org/2000/svg" width="48" height="48" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M19 21v-2a4 4 0 0 0-4-4H9a4 4 0 0 0-4 4v2"/><circle cx="12" cy="7" r="4"/></svg>',
    folder: '<svg aria-hidden="true" xmlns="http://www.w3.org/2000/svg" width="48" height="48" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"/></svg>',
    sparkle: '<svg aria-hidden="true" xmlns="http://www.w3.org/2000/svg" width="48" height="48" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="m12 3-1.912 5.813a2 2 0 0 1-1.275 1.275L3 12l5.813 1.912a2 2 0 0 1 1.275 1.275L12 21l1.912-5.813a2 2 0 0 1 1.275-1.275L21 12l-5.813-1.912a2 2 0 0 1-1.275-1.275L12 3Z"/><path d="M5 3v4"/><path d="M19 17v4"/><path d="M3 5h4"/><path d="M17 19h4"/></svg>',
  };
  function emptyState(icon, title, subtitle, ctaText, ctaAction) {
    const svg = EMPTY_ICONS[icon] || EMPTY_ICONS.sparkle;
    const cta = ctaText ? '<button class="es-cta" type="button">' + escapeHtml(ctaText) + '</button>' : '';
    const html = '<div class="empty-state">' + svg + 
      (title ? '<div class="es-title">' + escapeHtml(title) + '</div>' : '') + 
      (subtitle ? '<div class="es-sub">' + escapeHtml(subtitle) + '</div>' : '') + 
      cta + '</div>';
    if (ctaAction) {
      setTimeout(() => {
        const btn = document.querySelector('.empty-state .es-cta');
        if (btn) btn.onclick = ctaAction;
      }, 0);
    }
    return html;
  }
  function stSkeleton(count) {
    return Array.from({ length: count || 3 }, () => '<div class="st-skeleton"><div class="line"></div><div class="line short"></div></div>').join('');
  }

  function stSwitchView(view) {
    const entry = $('st-view-entry');
    const wizard = $('st-view-wizard');
    const play = $('st-view-play');
    const pack = $('st-view-pack');
    if (entry) entry.classList.toggle('hidden', view !== 'entry');
    if (wizard) wizard.classList.toggle('hidden', view !== 'wizard');
    if (play) play.classList.toggle('hidden', view !== 'play');
    if (pack) pack.classList.toggle('hidden', view !== 'pack');
  }

  function stDisplayTitle(title) {
    let t = String(title || '').trim();
    if (!t) return '未命名剧本';
    t = t.replace(/^\[[^\]]+\]\s*/, '');
    t = t.replace(/^【([^】]{1,40})】.*/, '$1');
    t = t.replace(/（重置版）.*/, '');
    t = t.replace(/\s*1-\d+.*未完结.*/, '');
    t = t.replace(/\s*作者[:：].*$/, '');
    if (t.length > 28) t = t.slice(0, 28) + '…';
    return t;
  }

  function stCleanCast(pack) {
    const chars = (pack && pack.characters) || [];
    const names = (pack && pack.castNames) || [];
    if (names.length) return names.slice(0, 64);
    const junkRe = /^(露出|眼角|换鞋|随口|轻声|低头|抬起|转身|伸手|走过去|看向|听见|突然)/;
    return chars
      .filter((c) => {
        const role = String(c.role || '').toLowerCase();
        const n = String(c.name || '').trim();
        if (!n || role.includes('narrator') || role.includes('player')) return false;
        if (n === '旁白' || n === '读者' || n === '玩家') return false;
        if (n.length < 2 || n.length > 8) return false;
        if (junkRe.test(n)) return false;
        if (/[的了着在把被]/.test(n)) return false;
        return true;
      })
      .map((c) => c.name.trim())
      .slice(0, 64);
  }

  function stPackBlurb(pack) {
    if (pack && pack.blurb) return String(pack.blurb).slice(0, 160);
    const lore = (pack && pack.loreEntries) || [];
    const hit = lore.find((e) => /简介|概述|blurb/i.test(String(e.title || '')))
      || lore.find((e) => e.permanent)
      || lore[0];
    if (hit) {
      const t = hit.text || hit.content || '';
      if (t) return String(t).slice(0, 160);
    }
    const n0 = ((pack && pack.nodes) || [])[0];
    if (n0 && n0.summary) return String(n0.summary).slice(0, 120);
    return '';
  }

  function stSourceLabel(p) {
    const t = String((p && (p.sourceType || (p.source && p.source.type))) || '').toLowerCase();
    if (t === 'novel') return '小说';
    if (t === 'manual') return '自制';
    if (t === 'demo') return 'Demo';
    return t || '剧本';
  }

  async function stApi(path, opts = {}) {
    return api('/api/v1/story-tavern' + path, opts);
  }

  function adultOk() {
    // Server-side setting takes priority (persistent across devices/browser resets)
    if (settings && settings.tavernAdultOk) return true;
    try { return localStorage.getItem(ADULT_OK_KEY) === '1'; } catch (_) { return false; }
  }
  async function setAdultOk() {
    try { localStorage.setItem(ADULT_OK_KEY, '1'); } catch (_) {}
    // Also persist to server
    try {
      await api('/api/v1/settings', {
        method: 'PATCH',
        body: JSON.stringify({ tavernAdultOk: true }),
      });
      settings.tavernAdultOk = true;
    } catch (_) {}
  }

  function stStatus(text, opts) {
    const el = $('st-status');
    if (el) el.textContent = text || '';
    // S8.29: immersive mode hides the status row (top bar already shows the
    // title). Surface actionable/error messages via toast instead; drop the
    // session-state status lines (turn/focus/etc) entirely.
    if (document.documentElement.getAttribute('data-immersive') === '1') {
      const t = (text || '').trim();
      if (!t) return;
      if (/\bturn\b|\b焦点\b|\bmode\b/.test(t)) return; // status line, redundant
      if (opts && opts.silent) return; // intermediate state — no toast, only result toasts
      try {
        if (typeof showToast === 'function') showToast(t, /失败/.test(t) ? 'error' : 'info', undefined, true);
      } catch (_) {}
    }
  }

  // ── R1–R4: 二级/三级返回统一出口 ─────────────────────────────────────────
  // 进入来源记忆：'story-entry'（故事馆 P1-P4 卡片 / 新建会话）| 'packs-detail'（档案馆包详情）| ''
  let stNavFrom = '';
  try {
    Object.defineProperty(window, 'stNavFrom', {
      get: () => stNavFrom,
      set: (v) => { stNavFrom = v; },
      configurable: true,
    });
  } catch (_) {}

  // 弹层是否真的可见（未被宿主 tab 面板隐藏）
  function stOverlayVisible(el) {
    if (!el || el.classList.contains('hidden')) return false;
    const host = el.closest('.tab-panel');
    return !host || !host.classList.contains('hidden');
  }
  function stHasOpenOverlay() {
    if (stOverlayVisible($('st-view-wizard'))) return true;
    if (stOverlayVisible($('st-side-panel'))) return true;
    if (stOverlayVisible($('tools-sheet'))) return true;
    const layout = $('st-layout');
    if (layout && layout.classList.contains('st-side-open')) return true;
    if (stOverlayVisible($('st-view-play'))) return true;
    return false;
  }

  // 用 replaceState 把入口 hash 钉回当前视图（不新增历史，避免再按返回/刷新重进深链）
  function stPinViewHash() {
    try {
      const keep = '#/' + currentTab;
      if (location.hash !== keep) {
        suppressHashWrite = true;
        history.replaceState(null, '', keep);
        setTimeout(() => { suppressHashWrite = false; }, 50);
      }
    } catch (_) {}
  }

  // R1: 向导取消 —— 按进入来源返回
  function stCancelWizard(fromPop) {
    $('st-wizard').classList.add('hidden');
    $('st-view-wizard').classList.add('hidden');
    const listview = $('st-packs-listview');
    const packDetail = $('st-view-pack');
    if (stNavFrom === 'story-entry') {
      // 恢复故事馆，且不显示档案馆 pack
      stSwitchView('entry');
      if (listview) listview.classList.remove('hidden');
      if (packDetail) packDetail.classList.add('hidden');
    } else if (stNavFrom === 'packs-detail') {
      // 恢复档案馆当前包详情
      stSwitchView('pack');
      if (listview) listview.classList.add('hidden');
    } else if (fromPop) {
      // fromPop 兜底：不额外写 hash，仅恢复视图
      if (currentTab === 'packs') {
        if (listview) listview.classList.remove('hidden');
        if (packDetail) packDetail.classList.add('hidden');
      } else {
        stSwitchView('entry');
      }
    } else {
      // 兜底：回档案馆列表
      switchTab('packs');
    }
    stPinViewHash();
  }

  // R2/R4: 退出剧场 —— 按进入来源恢复入口视图 + 还原入口 hash
  function stExitPlay() {
    const play = $('st-view-play');
    if (play) play.classList.add('hidden');
    // 退出播放:隐藏语义记忆召回条(避免残留占满正文上方)
    const recallBar = $('st-recall-bar');
    if (recallBar) recallBar.classList.add('hidden');
    const listview = $('st-packs-listview');
    const packDetail = $('st-view-pack');
    if (stNavFrom === 'packs-detail' && currentTab === 'packs') {
      // 从包详情进 play → 恢复包详情
      stSwitchView('pack');
      if (listview) listview.classList.add('hidden');
    } else if (currentTab === 'packs') {
      // 从档案馆列表进 play → 回列表
      if (listview) listview.classList.remove('hidden');
      if (packDetail) packDetail.classList.add('hidden');
      const entry = $('st-view-entry');
      if (entry) entry.classList.add('hidden');
    } else {
      // 故事馆 / 其他 → entry
      stSwitchView('entry');
      if (listview) listview.classList.remove('hidden');
      if (packDetail) packDetail.classList.add('hidden');
    }
    exitImmersive();
    stRenderContinueCard();
    stPinViewHash();
  }

  // R3: 统一返回出口（顶栏返回 / popstate / hashchange 兜底共用）
  function stGoBack(fromPop) {
    // 1) 向导可见 → 复用 R1 的取消逻辑
    if (stOverlayVisible($('st-view-wizard'))) {
      stCancelWizard(fromPop);
      return true;
    }
    // 2) st 侧栏 / 工具 sheet 可见 → 只关弹层，不跳 tab
    if (stOverlayVisible($('st-side-panel'))) {
      stCloseSidePanel();
      return true;
    }
    const layout = $('st-layout');
    if (layout && layout.classList.contains('st-side-open')) {
      layout.classList.remove('st-side-open');
      return true;
    }
    if (stOverlayVisible($('tools-sheet'))) {
      closeToolsSheet();
      return true;
    }
    // 3) play 剧场沉浸可见 → 退出 + 按来源恢复入口视图 + 还原入口 hash
    if (stOverlayVisible($('st-view-play'))) {
      stExitPlay();
      return true;
    }
    // 4) 交还给浏览器 history.back()
    if (!fromPop) history.back();
    return false;
  }
  window.stGoBack = stGoBack;

  // popstate：安卓物理返回 / 浏览器返回。已按应用层级消费的返回，吞掉随后的
  // hashchange，避免 popstate 与 hashchange 双触发造成闪跳。
  window.addEventListener('popstate', () => {
    if (stGoBack(true)) {
      suppressHashWrite = true;
      setTimeout(() => { suppressHashWrite = false; }, 50);
    }
  });

  // ─── S5/S6 演出机面板（吞噬 denova event_package / actor archive）─────────
  // 魔棒 → 演出机：事件卡包 / 最近事件 / 角色状态 / 导演台 / 角色归档
  let stStageEl = null;
  // G16: 导演策略编辑态（表单展开时记录最近一次 stageDirector 用于预填 + 合并保存）
  let stStageEditing = false;
  let stStageSd = null;
  let stStageLastActorStates = null;

  function stStageCss() {
    const css = [
      '#st-stage-modal{position:fixed;inset:0;z-index:980;display:flex;align-items:center;justify-content:center;background:rgba(8,10,18,.62);backdrop-filter:blur(3px);padding:16px}',
      '#st-stage-modal.hidden{display:none}',
      '#st-stage-modal .st-stage-box{width:min(760px,100%);max-height:86vh;overflow:auto;background:var(--bg-card,#161b28);border:1px solid var(--border,#2a3247);border-radius:16px;box-shadow:0 18px 60px rgba(0,0,0,.5);display:flex;flex-direction:column}',
      '#st-stage-modal .st-stage-head{display:flex;align-items:center;gap:10px;padding:14px 18px;border-bottom:1px solid var(--border,#2a3247);position:sticky;top:0;background:inherit;z-index:2}',
      '#st-stage-modal .st-stage-head h3{margin:0;font-size:16px;font-weight:700}',
      '#st-stage-modal .st-stage-close{margin-left:auto;background:none;border:none;color:var(--text-dim,#8b93a7);font-size:20px;cursor:pointer;line-height:1;padding:6px}',
      '#st-stage-modal .st-stage-close:hover{color:var(--text,#e8eaf2)}',
      '#st-stage-modal .st-stage-body{padding:16px 18px 20px;display:flex;flex-direction:column;gap:14px}',
      '#st-stage-modal .st-stage-sec{border:1px solid var(--border,#2a3247);border-radius:12px;padding:12px 14px;background:rgba(255,255,255,.02)}',
      '#st-stage-modal .st-stage-sec h4{margin:0 0 8px;font-size:13px;font-weight:700;color:var(--text,#e8eaf2);display:flex;align-items:center;gap:6px}',
      '#st-stage-modal .st-stage-sec h4 .st-stage-tag{margin-left:auto;font-size:11px;font-weight:500;color:var(--text-dim,#8b93a7);background:rgba(255,255,255,.06);padding:2px 8px;border-radius:99px}',
      '#st-stage-modal .st-stage-row{display:flex;flex-wrap:wrap;gap:6px 10px;font-size:12.5px;color:var(--text-dim,#aab2c5);line-height:1.5}',
      '#st-stage-modal .st-stage-row b{color:var(--text,#e8eaf2);font-weight:600}',
      '#st-stage-modal .st-stage-ev{margin-top:8px;border-left:3px solid #5b7cfa;background:rgba(91,124,250,.08);border-radius:6px;padding:8px 10px;font-size:12.5px;color:var(--text-dim,#aab2c5)}',
      '#st-stage-modal .st-stage-chip{display:inline-block;font-size:11px;background:rgba(91,124,250,.14);color:#9db4ff;border:1px solid rgba(91,124,250,.3);border-radius:99px;padding:1px 8px;margin:1px 2px}',
      '#st-stage-modal .st-stage-acts{display:flex;gap:8px;margin-top:10px;flex-wrap:wrap}',
      '#st-stage-modal .st-stage-acts button{font-size:12px;padding:5px 12px;border-radius:8px;border:1px solid var(--border,#2a3247);background:rgba(255,255,255,.04);color:var(--text,#e8eaf2);cursor:pointer}',
      '#st-stage-modal .st-stage-acts button:hover{background:rgba(91,124,250,.16);border-color:#5b7cfa}',
      '#st-stage-modal .st-stage-acts button:disabled{opacity:.5;cursor:default}',
      '#st-stage-modal .st-stage-empty{color:var(--text-dim,#6b7285);font-size:12.5px;padding:6px 2px}',
      '#st-stage-modal .st-stage-load{color:var(--text-dim,#8b93a7);font-size:12.5px;padding:8px 2px}',
      '#st-stage-modal .st-stage-err{color:#ff8a8a;font-size:12.5px;padding:8px 2px}',
      '#st-stage-modal .st-stage-sub{font-size:12px;color:var(--text-dim,#8b93a7);padding:10px 14px;border-top:1px solid var(--border,#2a3247);text-align:center}',
      /* 日间模式：浅色卡片 + 暗发丝（与剧情助手/剧场日间一致） */
      'html[data-color-scheme="day"] #st-stage-modal{background:rgba(28,25,20,.4)}',
      'html[data-color-scheme="day"] #st-stage-modal .st-stage-box{background:var(--surface-0);border-color:rgba(28,25,20,.16);box-shadow:0 18px 60px rgba(28,25,20,.18)}',
      'html[data-color-scheme="day"] #st-stage-modal .st-stage-head{border-color:rgba(28,25,20,.14)}',
      'html[data-color-scheme="day"] #st-stage-modal .st-stage-sec{border-color:rgba(28,25,20,.14);background:rgba(28,25,20,.03)}',
      'html[data-color-scheme="day"] #st-stage-modal .st-stage-sec h4{color:var(--text)}',
      'html[data-color-scheme="day"] #st-stage-modal .st-stage-tag{color:rgba(28,25,20,.55);background:rgba(28,25,20,.07)}',
      'html[data-color-scheme="day"] #st-stage-modal .st-stage-row{color:rgba(28,25,20,.72)}',
      'html[data-color-scheme="day"] #st-stage-modal .st-stage-row b{color:var(--text)}',
      'html[data-color-scheme="day"] #st-stage-modal .st-stage-close{color:rgba(28,25,20,.55)}',
      'html[data-color-scheme="day"] #st-stage-modal .st-stage-close:hover{color:var(--text)}',
      'html[data-color-scheme="day"] #st-stage-modal .st-stage-acts button{background:rgba(28,25,20,.05);border-color:rgba(28,25,20,.16);color:var(--text)}',
      'html[data-color-scheme="day"] #st-stage-modal .st-stage-empty{color:rgba(28,25,20,.5)}',
      'html[data-color-scheme="day"] #st-stage-modal .st-stage-load{color:rgba(28,25,20,.55)}',
      'html[data-color-scheme="day"] #st-stage-modal .st-stage-ev{color:rgba(28,25,20,.75)}',
      'html[data-color-scheme="day"] #st-stage-modal .st-stage-chip{color:#3350a8;background:rgba(91,124,250,.12);border-color:rgba(91,124,250,.3)}',
      'html[data-color-scheme="day"] #st-stage-modal .st-stage-sub{color:rgba(28,25,20,.55);border-color:rgba(28,25,20,.14)}',
      /* G16: 导演策略编辑表单 */
      '#st-stage-modal .st-stage-form{display:flex;flex-direction:column;gap:8px;margin-top:10px;padding-top:10px;border-top:1px dashed var(--border,#2a3247)}',
      '#st-stage-modal .st-stage-form .st-f-row{display:flex;align-items:center;gap:8px;flex-wrap:wrap}',
      '#st-stage-modal .st-stage-form label{font-size:12px;color:var(--text-dim,#8b93a7);min-width:84px}',
      '#st-stage-modal .st-stage-form select,#st-stage-modal .st-stage-form input[type=number]{flex:1;min-width:120px;background:rgba(255,255,255,.05);border:1px solid var(--border,#2a3247);color:var(--text,#e8eaf2);border-radius:8px;padding:5px 8px;font-size:12.5px}',
      '#st-stage-modal .st-stage-form select:focus,#st-stage-modal .st-stage-form input[type=number]:focus{outline:none;border-color:#5b7cfa}',
      '#st-stage-modal .st-stage-form .st-f-btns{display:flex;gap:8px;margin-top:6px}',
      '#st-stage-modal .st-stage-form .st-f-btns button{font-size:12px;padding:5px 14px;border-radius:8px;border:1px solid var(--border,#2a3247);background:rgba(255,255,255,.04);color:var(--text,#e8eaf2);cursor:pointer}',
      '#st-stage-modal .st-stage-form .st-f-btns button:hover{background:rgba(91,124,250,.16);border-color:#5b7cfa}',
      '#st-stage-modal .st-stage-form .st-f-btns button:disabled{opacity:.5;cursor:default}',
      '#st-stage-modal .st-stage-form .st-f-btns button.st-f-save{background:rgba(91,124,250,.18);border-color:#5b7cfa;color:#c8d4ff}',
      '#st-stage-modal .st-stage-edit-btn{font-size:11px;padding:2px 8px;border-radius:99px;border:1px solid var(--border,#2a3247);background:rgba(255,255,255,.04);color:var(--text-dim,#8b93a7);cursor:pointer;margin-left:6px}',
      '#st-stage-modal .st-stage-edit-btn:hover{background:rgba(91,124,250,.16);border-color:#5b7cfa;color:var(--text,#e8eaf2)}',
      '#st-stage-modal .st-stage-edit-form{margin:4px 0 8px;border-left:3px solid #5b7cfa;background:rgba(91,124,250,.06);border-radius:8px;padding:8px 10px}',
      'html[data-color-scheme="day"] #st-stage-modal .st-stage-form{border-color:rgba(28,25,20,.14)}',
      'html[data-color-scheme="day"] #st-stage-modal .st-stage-form label{color:rgba(28,25,20,.6)}',
      'html[data-color-scheme="day"] #st-stage-modal .st-stage-form select,html[data-color-scheme="day"] #st-stage-modal .st-stage-form input[type=number]{background:rgba(28,25,20,.04);border-color:rgba(28,25,20,.16);color:var(--text)}',
      'html[data-color-scheme="day"] #st-stage-modal .st-stage-form .st-f-btns button{background:rgba(28,25,20,.05);border-color:rgba(28,25,20,.16);color:var(--text)}',
      'html[data-color-scheme="day"] #st-stage-modal .st-stage-form .st-f-btns button.st-f-save{color:#3350a8}',
    ].join('');
    let el = document.getElementById('st-stage-css');
    if (!el) { el = document.createElement('style'); el.id = 'st-stage-css'; document.head.appendChild(el); }
    el.textContent = css;
  }

  function stStagePick(v, keys) {
    if (v == null) return undefined;
    for (let k = 0; k < keys.length; k++) { if (v[keys[k]] !== undefined) return v[keys[k]]; }
    return undefined;
  }

  function stStageVal(v) {
    if (v == null || v === '') return '—';
    return String(v);
  }

  async function stStageFetch(path) {
    return stApi('/sessions/' + encodeURIComponent(tavernSession.sessionId) + path);
  }

  function stStageSec(title, tag, html) {
    return '<div class="st-stage-sec"><h4>' + title + (tag ? '<span class="st-stage-tag">' + tag + '</span>' : '') + '</h4>' + html + '</div>';
  }

  // ── G16: 导演策略编辑（PUT director-config）───────────────────────────────
  // 枚举选项：值 + 展示文案（与后端 validate_stage_director_config 白名单一致）
  const DC_MAINLINE_OPTS = [
    ['strong_arc', '严格遵循（strong_arc）'],
    ['balanced', '主线优先（balanced）'],
    ['soft', '宽松探索（soft）'],
    ['soft_guidance', '宽松探索（soft_guidance）'],
  ];
  const DC_MODE_OPTS = [
    ['on_demand', '按需（on_demand）'],
    ['manual', '手动（manual）'],
    ['interval', '按回合间隔（interval）'],
  ];
  const DC_FAILURE_OPTS = [
    ['fail_forward', '失败继续推进（fail_forward）'],
    ['success_at_cost', '代价成功（success_at_cost）'],
    ['blocked', '阻断（blocked）'],
    ['hard_failure', '硬失败（hard_failure）'],
  ];
  const DC_PACING_OPTS = [
    ['', '不指定（自由发挥）'],
    ['wave', '波浪（wave）'],
    ['goal-pressure-payoff', '目标-压力-回报（goal-pressure-payoff）'],
    ['linear', '线性（linear）'],
  ];
  const DC_EVENT_FREQ_OPTS = [
    ['off', '关闭（off）'],
    ['sparse', '稀疏（sparse）'],
    ['balanced', '均衡（balanced）'],
    ['frequent', '频繁（frequent）'],
  ];
  const DC_RULE_VIS_OPTS = [
    ['audit_only', '仅审计（audit_only）'],
    ['public_roll', '公开掷骰（public_roll）'],
  ];

  function stStageOpts(opts, val) {
    return opts.map(function (o) {
      return '<option value="' + escapeHtml(o[0]) + '"' + (String(val) === o[0] ? ' selected' : '') + '>' + escapeHtml(o[1]) + '</option>';
    }).join('');
  }

  // 展开「编辑策略」表单，用最近一次 GET 的 stageDirector 预填
  function stStageEditForm(sd, sid) {
    const rp = (sd && sd.runPolicy) || {};
    const v = function (o, k, fb) { return (o && o[k] !== undefined && o[k] !== null) ? o[k] : fb; };
    const fRow = function (label, inner) {
      return '<div class="st-f-row"><label>' + label + '</label>' + inner + '</div>';
    };
    return '<form class="st-stage-form" data-sid="' + encodeURIComponent(sid) + '">' +
      fRow('主线强度', '<select data-dc="mainlineStrength">' + stStageOpts(DC_MAINLINE_OPTS, v(sd, 'mainlineStrength', 'balanced')) + '</select>') +
      fRow('运行模式', '<select data-dc="mode">' + stStageOpts(DC_MODE_OPTS, v(rp, 'mode', 'on_demand')) + '</select>') +
      fRow('回合间隔', '<input type="number" min="0" step="1" data-dc="intervalTurns" value="' + escapeHtml(String(v(rp, 'intervalTurns', 0))) + '">') +
      fRow('失败策略', '<select data-dc="failurePolicy">' + stStageOpts(DC_FAILURE_OPTS, v(rp, 'failurePolicy', 'fail_forward')) + '</select>') +
      fRow('节奏曲线', '<select data-dc="pacingCurve">' + stStageOpts(DC_PACING_OPTS, v(rp, 'pacingCurve', '')) + '</select>') +
      fRow('事件频率', '<select data-dc="eventFrequency">' + stStageOpts(DC_EVENT_FREQ_OPTS, v(rp, 'eventFrequency', 'balanced')) + '</select>') +
      fRow('检定可见', '<select data-dc="ruleVisibilityMode">' + stStageOpts(DC_RULE_VIS_OPTS, v(rp, 'ruleVisibilityMode', 'audit_only')) + '</select>') +
      fRow('分支规划', '<input type="number" min="1" step="1" data-dc="branchPlanningTurns" value="' + escapeHtml(String(v(rp, 'branchPlanningTurns', 5))) + '">') +
      '<div class="st-f-btns">' +
      '<button type="submit" class="st-f-save">保存</button>' +
      '<button type="button" data-stage-edit-cancel>取消</button>' +
      '</div>' +
      '</form>';
  }

  // 保存：以最近一次配置为基底合并表单值，PUT 完整 StageDirectorConfig，成功后刷新面板
  async function stStageEditSave(form) {
    const sid = form.getAttribute('data-sid');
    const base = stStageSd || {};
    const read = function (name, fb) {
      const el = form.querySelector('[data-dc="' + name + '"]');
      if (!el) return fb;
      if (el.type === 'number') {
        const n = parseInt(el.value, 10);
        return isFinite(n) ? n : fb;
      }
      return el.value;
    };
    const cfg = {
      runPolicy: {
        mode: read('mode', 'on_demand'),
        intervalTurns: read('intervalTurns', 0),
        failurePolicy: read('failurePolicy', 'fail_forward'),
        pacingCurve: read('pacingCurve', ''),
        eventFrequency: read('eventFrequency', 'balanced'),
        ruleVisibilityMode: read('ruleVisibilityMode', 'audit_only'),
        branchPlanningTurns: read('branchPlanningTurns', 5),
      },
      modules: base.modules || {},
      mainlineStrength: read('mainlineStrength', 'balanced'),
      resolvedSnapshot: base.resolvedSnapshot || null,
    };
    const saveBtn = form.querySelector('[type="submit"]');
    if (saveBtn) { saveBtn.disabled = true; saveBtn.textContent = '保存中…'; }
    try {
      await stApi('/sessions/' + sid + '/director-config', { method: 'PUT', body: JSON.stringify(cfg) });
      stStatus('导演策略已保存到 Pack');
      stStageEditing = false;
      stStageSd = null;
      await stStageRender();
    } catch (e) {
      stStatus('保存失败：' + (e.message || e));
      if (saveBtn) { saveBtn.disabled = false; saveBtn.textContent = '保存'; }
    }
  }

  function stStageToggleEdit() {
    stStageEditing = !stStageEditing;
    if (!stStageEditing) stStageSd = null;
    stStageRender().catch(function () {});
  }

  async function stStageRender() {
    if (!tavernSession || !tavernSession.sessionId) return;
    const body = $('st-stage-body');
    if (!body) return;
    body.innerHTML = '<div class="st-stage-load">读取中…</div>';
    let out = '';
    // 1) 导演台（实际响应: {stageDirector, directorPlan, directorPending}）
    try {
      const dc = await stStageFetch('/director-config');
      const sd = dc.stageDirector || {};
      stStageSd = sd;
      const rp = sd.runPolicy || {};
      const plan = dc.directorPlan || {};
      const modules = sd.modules || {};
      const pending = !!dc.directorPending;
      // G2: 状态机——lastRun.status 优先（ready/running/conflict），旧数据无 lastRun 时按 pending/goal 推断
      const lr = plan.lastRun || {};
      const st = lr.status || (pending ? 'pending' : (plan.goal ? 'running' : 'idle'));
      const stLabel = { 'ready': '运行中', 'running': '执行中', 'conflict': '冲突', 'pending': '待执行', 'idle': '未启动' }[st] || st;
      let h = '<div class="st-stage-row"><b>策略</b><span>' + stStageVal(rp.mode) + '</span>' +
        '<b>导演</b><span>' + stLabel + '</span>' +
        '<b>间隔</b><span>' + (rp.intervalTurns ? rp.intervalTurns + ' 回合' : '—') + '</span>' +
        '<b>主线</b><span>' + ({ 'strong_arc': '严格遵循', 'balanced': '主线优先', 'soft': '宽松探索', 'soft_guidance': '宽松探索' }[sd.mainlineStrength] || stStageVal(sd.mainlineStrength) || '主线优先') + '</span></div>';
      if (plan.goal || pending) {
        h += '<div class="st-stage-row" style="margin-top:6px"><b>计划</b><span>' + stStageVal(plan.goal) + '</span>';
        if (pending) h += '<span style="color:#ffd166">（待执行）</span>';
        h += '</div>';
        if (plan.pressure) h += '<div class="st-stage-ev">压力：' + stStageVal(plan.pressure) + '</div>';
        if (plan.cost) h += '<div class="st-stage-ev">代价：' + stStageVal(plan.cost) + '</div>';
        // G2: conflict 时显示错误原因
        if (st === 'conflict' && lr.error) h += '<div class="st-stage-ev" style="color:#ff6b6b">冲突：' + stStageVal(lr.error) + '</div>';
        // G1: 三文档展示（有内容才显示）
        if (plan.agentBrief) h += '<div class="st-stage-ev"><b>本回合</b>' + stStageVal(plan.agentBrief) + '</div>';
        if (plan.loreContext) h += '<div class="st-stage-ev"><b>铺垫</b>' + stStageVal(plan.loreContext) + '</div>';
      } else {
        h += '<div class="st-stage-empty" style="margin-top:4px">导演按需运行：点击下方按钮排程，下一回合由 LLM 生成导演计划</div>';
      }
      const pkgIds = Array.isArray(modules.eventPackageIds) ? modules.eventPackageIds : [];
      if (pkgIds.length) {
        h += '<div class="st-stage-row" style="margin-top:6px"><b>事件包</b><span>' + pkgIds.join('、') + '</span></div>';
      }
      h += '<div class="st-stage-acts">' +
        '<button type="button" data-stage-act="director" data-sid="' + encodeURIComponent(tavernSession.sessionId) + '">▶ 运行导演计划</button>' +
        '<button type="button" data-stage-act="director-edit" data-sid="' + encodeURIComponent(tavernSession.sessionId) + '">' + (stStageEditing ? '✕ 收起编辑' : '✎ 编辑策略') + '</button>' +
        '</div>';
      // G16: 编辑态展开表单
      if (stStageEditing && stStageSd) {
        h += stStageEditForm(stStageSd, tavernSession.sessionId);
      }
      out += stStageSec('🎛 导演台', pending ? '待执行' : (plan.goal ? '运行中' : '空闲'), h);
      // P2-3 叙界守卫事件回放（director-config 返回 guardEvents: [{...}] 最近 20 条，格式 [high|med][维度] 消息）
      const gEvents = Array.isArray(dc.guardEvents) ? dc.guardEvents : [];
      if (gEvents.length) {
        let gh = '<div class="st-stage-guard">';
        gEvents.forEach(function (ev) {
          const isHigh = /\[high\]/.test(ev);
          gh += '<div class="st-stage-ev" style="color:' + (isHigh ? '#ff6b6b' : '#ffd166') + '">' +
            (isHigh ? '🔴' : '🟡') + ' ' + stStageVal(ev) + '</div>';
        });
        gh += '</div>';
        out += stStageSec('🛡 叙界守卫', gEvents.length + ' 事件', gh);
      } else {
        out += stStageSec('🛡 叙界守卫', '0 事件', '<div class="st-stage-empty">暂无守卫事件（主线未走偏）</div>');
      }
      // X3: 虾米质检展示区（data source: dc.xiami）。渲染失败不影响导演台其它功能。
      try {
        const xi = dc.xiami || null;
        if (xi && Array.isArray(xi.skimIssues)) {
          const issues = xi.skimIssues;
          const sample = xi.skimSample || '';
          const turn = xi.lastCheckedTurn;
          if (!issues.length) {
            out += stStageSec('🧪 虾米质检（吞噬）', '速读', '<div class="st-stage-empty">✅ 速读质检通过</div>');
          } else {
            let xh = '<div class="st-stage-row" style="margin-top:2px"><b>速读质检</b>' +
              '<span>' + issues.length + ' 个问题' + (turn != null ? '（turn ' + turn + ' 检查）' : '') + '</span></div>';
            issues.forEach(function (it) {
              const sev = it && it.severity;
              const color = sev === 1 ? '#ff6b6b' : (sev === 2 ? '#ffd166' : '#8b93a7');
              const tag = sev === 1 ? 'P1' : (sev === 2 ? 'P2' : 'P?');
              const msg = it && it.message ? it.message : '未命名问题';
              const fix = it && it.fix ? it.fix : (it && it.evidence ? it.evidence : '');
              xh += '<div class="st-stage-ev" style="color:' + color + '">· [' + tag + '] ' + stStageVal(msg) +
                (fix ? '（修复建议：' + stStageVal(fix) + '）' : '') + '</div>';
            });
            out += stStageSec('🧪 虾米质检（吞噬）', issues.length + ' 个问题', xh);
          }
          if (sample) {
            out += '<div class="st-stage-ev" style="margin-top:2px"><b>正文摘录（前 200 字）</b>' + stStageVal(sample) + '</div>';
          }
        } else {
          out += stStageSec('🧪 虾米质检（吞噬）', '', '<div class="st-stage-empty">无质检记录</div>');
        }
      } catch (xe) {
        out += stStageSec('🧪 虾米质检（吞噬）', '', '<div class="st-stage-err">质检展示失败：' + (xe.message || xe) + '</div>');
      }
    } catch (e) {
      out += stStageSec('🎛 导演台', '', '<div class="st-stage-err">读取失败：' + (e.message || e) + '</div>');
    }
    // 2) 事件卡包（实际响应: {packId, packages:[{id,name,enabled,cards:[{id,title,prompt,weight}]}]}）
    try {
      const ep = await stStageFetch('/event-packages');
      const packs = Array.isArray(ep.packages) ? ep.packages : [];
      if (!packs.length) {
        out += stStageSec('🃏 事件卡包', '', '<div class="st-stage-empty">暂无事件卡包</div>');
      } else {
        let h = '';
        packs.forEach(function (p) {
          const id = stStagePick(p, ['id', 'packageId']);
          const name = stStagePick(p, ['name', 'title']) || id || '未命名包';
          const on = !!(p.enabled);
          const cards = Array.isArray(p.cards) ? p.cards : (Array.isArray(p.events) ? p.events : []);
          h += '<div class="st-stage-row" style="margin-top:2px"><b>' + stStageVal(name) + '</b>' +
            '<span style="color:' + (on ? '#7ee2a8' : '#8b93a7') + '">' + (on ? '● 启用' : '○ 停用') + '</span>' +
            '<span>' + cards.length + ' 卡</span></div>';
          if (cards.length) {
            const chips = cards.slice(0, 12).map(function (ev) {
              // G7: 展示 category/intensity/typeName/tags（旧数据无新字段则防御式退化）
              const base = stStagePick(ev, ['title', 'name', 'id']) || '';
              const cat = stStagePick(ev, ['category', 'type']) || '';
              const inten = stStagePick(ev, ['intensity']) || '';
              const tname = stStagePick(ev, ['typeName', 'type_name']) || '';
              const tags = Array.isArray(ev.tags) ? ev.tags.slice(0, 3) : [];
              let label = stStageVal(base);
              if (cat) label += ' · ' + stStageVal(cat);
              // title 属性（hover 提示）挂 intensity + typeName
              const tip = [inten, tname].filter(function (s) { return s; }).join(' ');
              const chip = '<span class="st-stage-chip"' + (tip ? ' title="' + escapeHtml(tip) + '"' : '') + '>' + label + '</span>';
              // tags 前 3 个，# 前缀
              const tagsHtml = tags.length
                ? tags.map(function (t) { return '<span class="st-stage-tag">#' + stStageVal(t) + '</span>'; }).join('')
                : '';
              return chip + tagsHtml;
            }).join('');
            h += '<div style="margin-top:4px">' + chips + (cards.length > 12 ? ' <span class="st-stage-empty">+' + (cards.length - 12) + '…</span>' : '') + '</div>';
          }
        });
        // G16: 事件卡编辑（enabled/cooldownTurns 写接口）后端暂缺 → 跳过编辑并注明
        h += '<div class="st-stage-empty" style="margin-top:6px">事件卡编辑（enabled / cooldownTurns）暂只读，需后端写接口后支持</div>';
        out += stStageSec('🃏 事件卡包', packs.length + ' 包', h);
      }
    } catch (e) {
      out += stStageSec('🃏 事件卡包', '', '<div class="st-stage-err">读取失败：' + (e.message || e) + '</div>');
    }
    // 3) 最近事件（实际响应: {lastEvent:{turn,packageId,cardId,title,prompt,createdAt} | null}）
    try {
      const le = await stStageFetch('/last-event');
      const ev = le.lastEvent || le.event;
      if (!ev) {
        out += stStageSec('📌 最近事件', '', '<div class="st-stage-empty">尚未触发事件卡</div>');
      } else {
        const name = stStagePick(ev, ['title', 'name', 'id']);
        const kind = stStagePick(ev, ['kind', 'type']);
        const summary = stStagePick(ev, ['prompt', 'summary', 'tell', 'text']) || '';
        let h = '<div class="st-stage-row"><b>' + stStageVal(name) + '</b>';
        if (kind) h += '<span class="st-stage-chip">' + stStageVal(kind) + '</span>';
        const ts = stStagePick(ev, ['createdAt', 'triggeredAt', 'triggered_at', 'time']);
        if (ts) h += '<span>' + stStageVal(ts) + '</span>';
        h += '</div>';
        if (summary) h += '<div class="st-stage-ev">' + stStageVal(summary) + '</div>';
        out += stStageSec('📌 最近事件', '', h);
      }
    } catch (e) {
      out += stStageSec('📌 最近事件', '', '<div class="st-stage-err">读取失败：' + (e.message || e) + '</div>');
    }
    // 4) 角色状态 + 归档（实际响应: {actorStates:{actors:{characterId: ActorStateEntry}, archive, traitPools}}）
    try {
      const as = await stStageFetch('/actor-states');
      const ast = as.actorStates || as;
      stStageLastActorStates = ast;
      const actorsMap = ast.actors || {};
      const actors = (typeof actorsMap === 'object' && !Array.isArray(actorsMap))
        ? Object.keys(actorsMap).map(function (k) {
            const v = actorsMap[k] || {};
            v.characterId = v.characterId || k;
            v.character_id = v.character_id || k;
            return v;
          })
        : (Array.isArray(actorsMap) ? actorsMap : []);
      let h = '';
      if (!actors.length) {
        h = '<div class="st-stage-empty">暂无角色登记 —— 剧情回合中 LLM 输出【状态更新】后自动登记</div>';
      } else {
        actors.forEach(function (a) {
          const cid = stStagePick(a, ['characterId', 'character_id', 'id']);
          const name = stStagePick(a, ['name', 'displayName']) || cid || '未知角色';
          const traits = stStagePick(a, ['traits']);
          const tags = stStagePick(a, ['tags']);
          const notes = stStagePick(a, ['notes', 'note']);
          const fields = stStagePick(a, ['fields']);
          h += '<div class="st-stage-row"><b>' + stStageVal(name) + '</b>' +
            (cid && cid !== name ? '<span>' + stStageVal(cid) + '</span>' : '');
          if (Array.isArray(traits) && traits.length) h += '<span>特质：' + traits.map(function (t) {
            return stStagePick(t, ['name', 'traitId', 'id']) || stStageVal(t);
          }).join('、') + '</span>';
          else if (typeof traits === 'string' && traits) h += '<span>特质：' + traits + '</span>';
          h += ' <button type="button" class="st-stage-edit-btn" data-stage-edit="' + encodeURIComponent(cid || '') + '" title="手动调整属性">✏️ 编辑</button>' +
            '</div>';
          h += '<div class="st-stage-edit-form" id="st-stage-edit-' + encodeURIComponent(cid || '') + '" hidden></div>';
          if (fields && typeof fields === 'object') {
            const fkeys = Object.keys(fields);
            if (fkeys.length) {
              h += '<div style="margin-top:3px">' + fkeys.slice(0, 8).map(function (fk) {
                const fv = fields[fk] || {};
                let val = fv.value;
                if (val && typeof val === 'object') val = JSON.stringify(val);
                return '<span class="st-stage-chip" title="' + stStageVal(fk) + '">' + stStageVal(fk) + (val != null ? '=' + stStageVal(val) : '') + '</span>';
              }).join('') + '</div>';
            }
          }
          if (Array.isArray(tags) && tags.length) {
            h += '<div style="margin-top:3px">' + tags.slice(0, 10).map(function (t) {
              return '<span class="st-stage-chip">' + stStageVal(t) + '</span>';
            }).join('') + '</div>';
          }
          if (notes) h += '<div class="st-stage-ev" style="margin-top:4px">' + stStageVal(notes) + '</div>';
        });
      }
      h += '<div class="st-stage-acts">' +
        '<button type="button" data-stage-act="archive" data-sid="' + encodeURIComponent(tavernSession.sessionId) + '"' + (actors.length ? '' : ' disabled title="暂无角色可归档"') + '>📦 归档全部角色</button>' +
        '<button type="button" data-stage-act="restore" data-sid="' + encodeURIComponent(tavernSession.sessionId) + '">♻️ 恢复最近归档</button>' +
        '</div>';
      out += stStageSec('🎭 角色状态', actors.length + ' 人', h);
    } catch (e) {
      out += stStageSec('🎭 角色状态', '', '<div class="st-stage-err">读取失败：' + (e.message || e) + '</div>');
    }
    body.innerHTML = out;
    body.querySelectorAll('[data-stage-act]').forEach(function (btn) {
      btn.onclick = function () {
        const act = btn.getAttribute('data-stage-act');
        const sid = btn.getAttribute('data-sid');
        // G16: 编辑态切换在本地处理（不走进度/禁用逻辑），其余走通用动作
        if (act === 'director-edit') { stStageToggleEdit(); return; }
        stStageAct(act, sid);
      };
    });
    // 角色属性编辑：点击「✏️ 编辑」展开/收起表单
    body.querySelectorAll('[data-stage-edit]').forEach(function (btn) {
      btn.onclick = function () {
        const cid = decodeURIComponent(btn.getAttribute('data-stage-edit') || '');
        const formEl = document.getElementById('st-stage-edit-' + encodeURIComponent(cid));
        if (!formEl) return;
        if (!formEl.getAttribute('hidden')) { formEl.setAttribute('hidden', ''); return; }
        // 从当前 actorStates 快照取该角色 fields，按 valueType 渲染输入控件
        const ast = stStageLastActorStates || {};
        const actorsMap = ast.actors || {};
        const actor = actorsMap[cid] || {};
        const fields = (actor.fields && typeof actor.fields === 'object') ? actor.fields : {};
        const fkeys = Object.keys(fields);
        let rows = '';
        if (!fkeys.length) {
          rows = '<div class="st-stage-empty">该角色暂无字段，可直接新增（下方输入字段名+值）</div>';
          rows += '<div class="st-stage-form"><div class="st-f-row"><label>字段名</label><input type="text" class="st-f-name" placeholder="如 服装" /></div>' +
            '<div class="st-f-row"><label>字段值</label><input type="text" class="st-f-val" placeholder="如 碎花裙" /></div>' +
            '<div class="st-f-btns"><button type="button" class="st-f-save">保存</button><button type="button" class="st-f-cancel">取消</button></div></div>';
        } else {
          rows += '<div class="st-stage-form">';
          fkeys.forEach(function (fk) {
            const fv = fields[fk] || {};
            const vt = fv.valueType || (typeof fv.value === 'number' ? 'number' : (typeof fv.value === 'boolean' ? 'bool' : 'string'));
            let cur = fv.value;
            if (cur && typeof cur === 'object') cur = JSON.stringify(cur);
            if (vt === 'number') {
              const lo = fv.min != null ? fv.min : '';
              const hi = fv.max != null ? fv.max : '';
              rows += '<div class="st-f-row"><label>' + stStageVal(fk) + '</label>' +
                '<input type="number" step="any" class="st-f-val" data-fk="' + stStageVal(fk) + '" data-vt="number" value="' + stStageVal(cur != null ? cur : '') + '"' +
                (lo !== '' ? ' min="' + lo + '"' : '') + (hi !== '' ? ' max="' + hi + '"' : '') + ' /></div>';
            } else if (vt === 'bool') {
              rows += '<div class="st-f-row"><label>' + stStageVal(fk) + '</label>' +
                '<select class="st-f-val" data-fk="' + stStageVal(fk) + '" data-vt="bool">' +
                '<option value="true"' + (cur === true || cur === 'true' ? ' selected' : '') + '>是</option>' +
                '<option value="false"' + (cur === false || cur === 'false' ? ' selected' : '') + '>否</option>' +
                '</select></div>';
            } else {
              rows += '<div class="st-f-row"><label>' + stStageVal(fk) + '</label>' +
                '<input type="text" class="st-f-val" data-fk="' + stStageVal(fk) + '" data-vt="string" value="' + stStageVal(cur != null ? cur : '') + '" /></div>';
            }
          });
          rows += '<div class="st-f-btns"><button type="button" class="st-f-save">保存</button><button type="button" class="st-f-cancel">取消</button></div></div>';
        }
        formEl.innerHTML = rows;
        formEl.removeAttribute('hidden');
        const saveBtn = formEl.querySelector('.st-f-save');
        const cancelBtn = formEl.querySelector('.st-f-cancel');
        if (cancelBtn) cancelBtn.onclick = function () { formEl.setAttribute('hidden', ''); };
        if (saveBtn) saveBtn.onclick = async function () {
          // 收集字段
          const newFields = {};
          const dynamicName = formEl.querySelector('.st-f-name');
          const dynamicVal = formEl.querySelector('.st-f-val');
          if (dynamicName && dynamicName.value.trim()) {
            const dv = dynamicVal ? dynamicVal.value.trim() : '';
            newFields[dynamicName.value.trim()] = (dynamicVal && dynamicVal.getAttribute('data-vt') === 'number') ? Number(dv) : dv;
          } else {
            formEl.querySelectorAll('.st-f-val').forEach(function (inp) {
              const fk = inp.getAttribute('data-fk');
              const vt = inp.getAttribute('data-vt');
              if (!fk) return;
              if (vt === 'number') { newFields[fk] = Number(inp.value); }
              else if (vt === 'bool') { newFields[fk] = inp.value === 'true'; }
              else { newFields[fk] = inp.value; }
            });
          }
          saveBtn.disabled = true; saveBtn.textContent = '保存中…';
          try {
            await stApi('/sessions/' + sid + '/actor-states', {
              method: 'PUT',
              body: JSON.stringify({ characterId: cid, fields: newFields })
            });
            stStatus('已更新 ' + name + ' 的 ' + Object.keys(newFields).length + ' 个属性');
            formEl.setAttribute('hidden', '');
            await stStageRender();
          } catch (e) {
            stStatus('保存失败：' + (e.message || e));
          } finally {
            saveBtn.disabled = false; saveBtn.textContent = '保存';
          }
        };
      };
    });
    // G16: 导演策略编辑表单提交 / 取消
    const dcForm = body.querySelector('.st-stage-form');
    if (dcForm) {
      dcForm.addEventListener('submit', function (ev) {
        ev.preventDefault();
        stStageEditSave(dcForm).catch(function () {});
      });
      const cancel = dcForm.querySelector('[data-stage-edit-cancel]');
      if (cancel) {
        cancel.onclick = function () {
          stStageEditing = false;
          stStageSd = null;
          stStageRender().catch(function () {});
        };
      }
    }
  }

  async function stStageAct(act, sid) {
    if (!sid) return;
    const body = $('st-stage-body');
    const btn = body && body.querySelector('[data-stage-act="' + act + '"]');
    if (btn) { btn.disabled = true; btn.textContent = '处理中…'; }
    try {
      if (act === 'director') {
        await stApi('/sessions/' + sid + '/director-plan/run', { method: 'POST', body: '{}' });
        stStatus('导演计划已排程，将在下一回合生成');
        await stStageRender();
        return;
      }
      if (act === 'archive') {
        const r = await stApi('/sessions/' + sid + '/actor-archive', { method: 'POST', body: JSON.stringify({}) });
        stStatus('已归档 ' + stStageVal(stStagePick(r, ['archived', 'count', 'snapshotCount'])) + ' 个角色');
      } else {
        const r = await stApi('/sessions/' + sid + '/actor-archive/restore', { method: 'POST', body: JSON.stringify({}) });
        stStatus('已恢复 ' + stStageVal(stStagePick(r, ['restored', 'count'])) + ' 个角色');
      }
      await stStageRender();
    } catch (e) {
      stStatus('演出机操作失败：' + (e.message || e));
    }
  }

  function stStageClose() {
    if (stStageEl) stStageEl.classList.add('hidden');
  }

  function stStageOpen() {
    stStageCss();
    if (!stStageEl) {
      stStageEl = document.createElement('div');
      stStageEl.id = 'st-stage-modal';
      stStageEl.className = 'hidden';
      stStageEl.innerHTML = '<div class="st-stage-box">' +
        '<div class="st-stage-head"><h3>🎬 演出机</h3>' +
        '<button type="button" class="st-stage-close" aria-label="关闭">✕</button></div>' +
        '<div id="st-stage-body" class="st-stage-body"></div>' +
        '<div class="st-stage-sub">事件卡包 · 角色状态 · 导演台（吞噬 denova S5/S6）</div>' +
        '</div>';
      document.body.appendChild(stStageEl);
      const close = stStageEl.querySelector('.st-stage-close');
      if (close) close.onclick = stStageClose;
      stStageEl.addEventListener('pointerdown', function (e) {
        if (e.target === stStageEl) stStageClose();
      });
    }
    stStageEl.classList.remove('hidden');
    stStageRender().catch(function () {});
  }

  const stStageBtn = $('st-stage-btn');
  if (stStageBtn) stStageBtn.onclick = stStageOpen;



/* S2.8: chat.js (real module) surfaces copy-status through the same status row. */
try { window.stStatus = stStatus; } catch (_) {}
