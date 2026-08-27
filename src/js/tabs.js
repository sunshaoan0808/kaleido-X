/* tabs.js — P1-3 S2.18: _tabs-part as real ESM (last routing part).
 * Canonical currentTab/suppressHashWrite/bondPick lets move HERE (they were
 * only ever owned by this part); tail still publishes window.__kaleidoTabs so
 * tavern/story/agent/authoring consumers keep their facade access unchanged.
 * Remaining closure reads (works/chat/story state) go through the S2.15–17
 * facades: __wk()/__c7()/__s8().
 */
import { $ } from './dom.js';
import { api } from './api.js';
import { uid as _uid, displayTitle } from './utils.js';
// lesson (j): functions always statically imported — facade property calls
// invite tree-shaking of the callee module.
import { stStatus, stGoBack, stSwitchView, stDisplayTitle, stHasOpenOverlay,
  stBindImmChrome, stRefresh, stLoadPacks, stLoadSessions, stLoadSaves,
  stLoadSession, stRefreshCharSummary, stRenderContinueCard, renderHomeRecent,
  loadBookshelf } from './tavern.js';
import { refreshSessions, showChatSetup } from './chat.js';
import { loadSettings } from './settings.js';
import { showMain } from './api_shell.js';
import { refreshJobs } from './jobs.js';
import { P5AiLoad } from './aiadmin.js';
import { MoaLoadPanels, MoaLoadSessions } from './moa.js';
import { loadAnKinds, loadAnTasks, loadGraph, loadForeshadows } from './insight.js';
import { advShowReader, advShowSetup, ensureStorySession, renderBondPage, renderStoryMessages } from './story.js';
import { loadEmbedLabStatus, loadEmbedLabEvents } from './agent.js';
import { loadPartner } from './partner.js';
import { loadAuthorProjects, loadWorksTree, refreshPackSelect, loadWorksVersionsSidebar } from './authoring.js';

const __wk = () => window.__kaleidoWorksState;
const __c7 = () => window.__kaleidoChatState;
const __s8 = () => window.__kaleidoStoryState;

/* Tab routing */

  // S2.17: works' escapeHtml left the closure with authoring.js (which keeps
  // its own copy but does NOT export it) — renderRecallBox below would hit an
  // unbound identifier the moment the session-drawer recall path renders.
  // Local copy restores the closure binding exactly (keyboard.js:30 precedent).
  function escapeHtml(s) {
    return String(s)
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;');
  }
  function uid(prefix) { return _uid(prefix); }

  // S7-W1 hash router + bond/adventure
  const TAB_TITLES = {
    home: '首页',
    tavern: '故事馆',
    packs: '档案馆',
    chat: '对话',
    bond: '角色',
    adventure: '冒险',
    story: '跑团',
    partner: '角色/世界',
    works: '作者区',
    jobs: '任务队列',
    background: '世界/角色生成',
    booktravel: '书籍漫游',
    outline: '反拆大纲',
    st: '角色卡导入',
    agent: '代理沙箱',
    skills: '技能管理',
    deai: '去AI味',
    stats: '统计',
    embedlab: '向量实验室',
    crawler: '番茄爬虫',
    bookshelf: '书架',
    settings: '设置',
    moa: '模型对比',
  };
  const PRIMARY_TABS = new Set(['home', 'tavern', 'packs', 'chat', 'bond', 'works', 'settings']);
  const TOOL_TABS = new Set(['adventure', 'story', 'partner', 'jobs', 'background', 'booktravel', 'outline', 'st', 'agent', 'skills', 'deai', 'stats', 'embedlab', 'crawler', 'bookshelf', 'aiadmin', 'moa']);
  const KNOWN_TABS = new Set([
    'home', 'tavern', 'packs', 'chat', 'bond', 'adventure', 'story', 'partner', 'works', 'jobs',
    'background', 'booktravel', 'outline', 'st', 'agent', 'skills', 'deai',
    'stats', 'embedlab', 'crawler', 'bookshelf', 'aiadmin', 'moa', 'settings',
  ]);
  // Hash aliases: book-travel ↔ booktravel, partner can be bond for nav label
  const HASH_ALIASES = {
    '': 'home',
    home: 'home',
    chat: 'chat',
    bond: 'bond',
    partner: 'partner', // advanced editor; bond is the simplified face
    adventure: 'adventure',
    story: 'story',
    works: 'works',
    jobs: 'jobs',
    background: 'background',
    booktravel: 'booktravel',
    'book-travel': 'booktravel',
    outline: 'outline',
    st: 'st',
    agent: 'agent',
    skills: 'skills',
    deai: 'deai',
    stats: 'stats',
    embedlab: 'embedlab',
    embed: 'embedlab',
    'vector-lab': 'embedlab',
    vectors: 'embedlab',
    crawler: 'crawler',
    bookshelf: 'bookshelf',
    shelf: 'bookshelf',
    settings: 'settings',
    moa: 'moa',
    'model-compare': 'moa',
    tavern: 'tavern',
    'story-tavern': 'tavern',
    storytavern: 'tavern',
    author: 'works',
  };
  let currentTab = 'home';
  // S8.28: author zone sub-view
  let azLastView = localStorage.getItem('kaleido_az_lastview') || 'compose';
  let suppressHashWrite = false;
  let bondPickWb = '';
  let bondPickCc = '';

  function applyAutoUi() {
    const mode = window.matchMedia('(max-width: 900px)').matches ? 'mobile' : 'desktop';
    document.documentElement.setAttribute('data-ui', mode);
    if (mode !== 'mobile') {
      closeToolsSheet();
      closeSessionDrawer();
    }
    syncMobileChrome(currentTab);
    // S9.3b: real header / bottom-nav heights for chat panel viewport lock
    try {
      const topEl = document.querySelector('.top');
      if (topEl) document.documentElement.style.setProperty('--top-h', topEl.offsetHeight + 'px');
      const bnav = document.querySelector('.bottom-nav');
      // L4: only write --bnav-h when a real height is measured; if the
      // measurement returns 0 (early timing / env), keep the CSS default
      // instead of pinning the panel height to 0px and hiding content
      // under the bar. Desktop / no-bar => explicit 0px.
      if (bnav && mode === 'mobile') {
        const bnavH = bnav.offsetHeight;
        if (bnavH > 0) document.documentElement.style.setProperty('--bnav-h', bnavH + 'px');
      } else {
        document.documentElement.style.setProperty('--bnav-h', '0px');
      }
    } catch (_) {}
    return mode;
  }
  try { localStorage.removeItem('kaleido.uiMode'); } catch (_) {}

  function syncMobileChrome(name) {
    const title = $('page-title');
    if (title) title.textContent = TAB_TITLES[name] || name;
    const sessionsBtn = $('mobile-sessions-btn');
    if (sessionsBtn) sessionsBtn.classList.toggle('hidden', name !== 'chat' && name !== 'adventure' && name !== 'story');
    document.querySelectorAll('.bnav').forEach((b) => {
      const tab = b.dataset.tab;
      const isTools = b.dataset.mnav === 'tools';
      // map partner advanced → bond primary highlight; story advanced → adventure
      let activeName = name;
      if (name === 'partner') activeName = 'bond';
      if (isTools) {
        b.classList.toggle('active', false);
        b.classList.toggle('active-tools', TOOL_TABS.has(name) && name !== 'bond');
      } else {
        b.classList.toggle('active-tools', false);
        b.classList.toggle('active', tab === activeName || tab === name);
      }
    });
  }

  let toolsOpener = null;
  let drawerOpener = null;
  function openToolsSheet() {
    const el = $('tools-sheet');
    if (!el || !el.classList.contains('hidden')) return;
    toolsOpener = document.activeElement;
    el.classList.remove('hidden');
    el.setAttribute('aria-hidden', 'false');
    const toolsPanel = el.querySelector('.sheet-panel');
    if (toolsPanel) toolsPanel.setAttribute('aria-modal', 'true');
    document.querySelectorAll('.bnav').forEach((b) => {
      if (b.dataset.mnav === 'tools') b.classList.add('active-tools');
    });
    const closeBtn = $('tools-sheet-close');
    if (closeBtn) closeBtn.focus();
  }
  function closeToolsSheet() {
    const el = $('tools-sheet');
    if (!el || el.classList.contains('hidden')) return;
    el.classList.add('hidden');
    el.setAttribute('aria-hidden', 'true');
    const toolsPanel = el.querySelector('.sheet-panel');
    if (toolsPanel) toolsPanel.removeAttribute('aria-modal');
    document.querySelectorAll('.bnav').forEach((b) => {
      if (b.dataset.mnav === 'tools') b.classList.remove('active-tools');
    });
    const opener = toolsOpener;
    toolsOpener = null;
    setTimeout(() => { if (opener && opener.focus) opener.focus(); }, 0);
  }
  function openSessionDrawer() {
    const el = $('session-drawer');
    if (!el || !el.classList.contains('hidden')) return;
    drawerOpener = document.activeElement;
    // mirror session-list into drawer
    const src = $('session-list');
    const dst = $('session-drawer-list');
    if (src && dst) {
      dst.innerHTML = '';
      src.querySelectorAll('.item').forEach((item) => {
        const clone = item.cloneNode(true);
        clone.onclick = () => {
          item.onclick && item.onclick();
          closeSessionDrawer();
        };
        dst.appendChild(clone);
      });
    }
    renderDrawerRecall();
    el.classList.remove('hidden');
    el.setAttribute('aria-hidden', 'false');
    const drawerPanel = el.querySelector('.drawer-panel');
    if (drawerPanel) drawerPanel.setAttribute('aria-modal', 'true');
    const closeBtn = $('session-drawer-close');
    if (closeBtn) closeBtn.focus();
  }
  function closeSessionDrawer() {
    const el = $('session-drawer');
    if (!el || el.classList.contains('hidden')) return;
    el.classList.add('hidden');
    el.setAttribute('aria-hidden', 'true');
    const drawerPanel = el.querySelector('.drawer-panel');
    if (drawerPanel) drawerPanel.removeAttribute('aria-modal');
    const opener = drawerOpener;
    drawerOpener = null;
    setTimeout(() => { if (opener && opener.focus) opener.focus(); }, 0);
  }

  function recallEventsOf(sess) {
    const mem = (sess && (sess.memoryL2 || sess.memory_l2)) || {};
    return (mem.events || []).filter(Boolean);
  }

  function recallBoxes() {
    const ids = [['session-drawer-recall', 'session-drawer-recall-list', 'session-drawer-recall-meta'],
                 ['st-drawer-recall', 'st-drawer-recall-list', 'st-drawer-recall-meta']];
    return ids.map(([boxId, listId, metaId]) => {
      const box = $(boxId);
      const list = $(listId);
      return box && list ? { box, list, meta: $(metaId) } : null;
    }).filter(Boolean);
  }

  function renderRecallBox(events, metaText) {
    const boxes = recallBoxes();
    if (!boxes.length) return;
    if (!events || !events.length) {
      boxes.forEach((b) => b.box.classList.add('hidden'));
      return;
    }
    const embedded = events.filter((e) => e.embedding && e.embedding.length).length;
    const items = events.slice(0, 30).map((e, i) => {
      const actors = (e.actors || []).slice(0, 3).map((a) =>
        '<span class="el-event-actor">' + escapeHtml(a) + '</span>').join('');
      const hasEmb = !!(e.embedding && e.embedding.length);
      return (
        '<div class="el-event" title="' + escapeHtml(e.summary || '') + '">' +
          '<span class="el-event-rank">#' + (i + 1) + '</span>' +
          '<span class="el-event-body">' +
            '<span class="el-event-kind">' + escapeHtml(e.kind || 'event') + '</span>' +
            '<span class="el-event-sum">' + escapeHtml(e.summary || '') + '</span>' +
            '<span class="el-event-sub muted sm">t' + (e.turn || '?') + (e.nodeId ? ' · ' + escapeHtml(e.nodeId) : '') + '</span>' +
            (actors ? '<span class="el-event-actors">' + actors + '</span>' : '') +
          '</span>' +
          '<span class="el-event-badge' + (hasEmb ? ' ok' : '') + '">' + (hasEmb ? '已嵌入' : '未嵌入') + '</span>' +
        '</div>'
      );
    }).join('');
    boxes.forEach((b) => {
      b.box.classList.remove('hidden');
      if (b.meta) b.meta.textContent = metaText || ('共' + events.length + '条 · 已嵌入' + embedded + '条');
      b.list.innerHTML = items;
    });
  }

  async function renderDrawerRecall() {
    if (!recallBoxes().length) return;
    const tavCur = typeof stCurrentSession === 'function' ? stCurrentSession() : null;
    const local = recallEventsOf(tavCur);
    if (local.length) {
      renderRecallBox(local);
      return;
    }
    let current = '';
    if (tavCur && (tavCur.id || tavCur.sessionId)) {
      current = tavCur.id || tavCur.sessionId;
    } else {
      const segs = parseHashSegments();
      if (segs[1] === 'session' && segs[2]) current = segs[2];
    }
    if (current) {
      try {
        const detail = await api('/api/v1/story-tavern/sessions/' + encodeURIComponent(current));
        const ev = recallEventsOf(detail);
        if (ev.length) {
          renderRecallBox(ev, '共' + ev.length + '条 · 已嵌入' + ev.filter((e) => e.embedding && e.embedding.length).length + '条 · 当前会话');
          return;
        }
      } catch (_) {}
    }
    try {
      const data = await api('/api/v1/story-tavern/sessions');
      const sessions = (data && data.sessions) || [];
      for (const s of sessions) {
        const id = s.sessionId || s.id;
        if (!id) continue;
        let detail;
        try {
          detail = await api('/api/v1/story-tavern/sessions/' + encodeURIComponent(id));
        } catch (_) { continue; }
        const ev = recallEventsOf(detail);
        if (ev.length) {
          renderRecallBox(ev, '最近:' + (s.title || s.name || id));
          return;
        }
      }
      renderRecallBox(null);
    } catch (_) {
      renderRecallBox(null);
    }
  }

  // Immersive mode --------------------------------------------------------
  function enterImmersive(title, opts) {
    document.documentElement.setAttribute('data-immersive', '1');
    // S8.28: top bar shown by default; scroll hides only .top (imm-top-hidden).
    // 不再整块隐藏 imm-chrome-hidden(会连 composer/魔法棒一起隐藏)。
    document.documentElement.classList.remove('imm-chrome-hidden');
    // S9.3c: remember last immersive state so reloads paint immersive directly (no FOUC)
    try { localStorage.setItem('kaleido_imm_active', '1'); } catch (_) {}
    // A11y: hide non-immersive tabs from screen readers
    document.querySelectorAll('.tab-panel').forEach(function (panel) {
      if (!panel.classList.contains('hidden')) return;
      panel.setAttribute('aria-hidden', 'true');
      panel.setAttribute('inert', '');
    });
    const immTitle = $('imm-title');
    if (immTitle) {
      const t = title || '';
      immTitle.textContent = t;
      immTitle.setAttribute('title', t);
    }
    const drawerBtn = $('st-drawer-toggle');
    if (drawerBtn) drawerBtn.classList.toggle('hidden', !(opts && opts.showDrawer));
    const chatSessBtn = $('chat-sessions-toggle');
    if (chatSessBtn) chatSessBtn.classList.toggle('hidden', !(opts && opts.showSessions));
    const worksNavBtn = $('works-nav-toggle');
    if (worksNavBtn) worksNavBtn.classList.toggle('hidden', !(opts && opts.showWorksNav));
    try { stBindImmChrome(); } catch (_) {}
  }
  function exitImmersive() {
    document.documentElement.removeAttribute('data-immersive');
    document.documentElement.classList.remove('imm-chrome-hidden');
    document.documentElement.classList.remove('adv-imm-armed');
    // S9.3c: clear sticky immersive flag
    try { localStorage.removeItem('kaleido_imm_active'); } catch (_) {}
    // A11y: restore non-immersive tabs
    document.querySelectorAll('.tab-panel[inert]').forEach(function (panel) {
      panel.removeAttribute('inert');
      if (panel.classList.contains('hidden')) panel.setAttribute('aria-hidden', 'true');
      else panel.setAttribute('aria-hidden', 'false');
    });
    const stLayout = $('st-layout');
    if (stLayout) stLayout.classList.remove('st-side-open');
    stSyncSideDrawer();
    const azLayout = document.querySelector('#tab-works .az-layout');
    if (azLayout) azLayout.classList.remove('az-nav-open');
    const drawerBtn = $('st-drawer-toggle');
    if (drawerBtn) drawerBtn.classList.add('hidden');
    const chatSessBtn = $('chat-sessions-toggle');
    if (chatSessBtn) chatSessBtn.classList.add('hidden');
    const worksNavBtn = $('works-nav-toggle');
    if (worksNavBtn) worksNavBtn.classList.add('hidden');
    const play = $('st-view-play');
    if (play) play.classList.remove('st-stage-enter');
    closeSessionDrawer();
  }
  function updateImmersive() {
    if (currentTab === 'tavern' || currentTab === 'packs') {
      const play = $('st-view-play');
      const curTav = typeof stCurrentSession === 'function' ? stCurrentSession() : null;
      if (curTav && play && !play.classList.contains('hidden')) {
        // S8.28: keep top bar minimal — script name (with chapter range) only;
        // playable/mode/focus moved into wand menu.
        const title = (stDisplayTitle(curTav.title) || curTav.title || '故事馆')
          .replace(/\s*·\s*P[1-4]\s*$/, '');
        enterImmersive(title, { showDrawer: true });
        return;
      }
    } else if (currentTab === 'chat') {
      // S9.4: only immersive when the stage (chat-shell) is visible; setup view stays flat.
      // Entering the stage is immersive immediately (even with an empty new session).
      const stage = $('chat-stage');
      if (!stage || stage.classList.contains('hidden')) {
        exitImmersive();
        return;
      }
      if (__c7().sessionId) {
        let chatTitle = '对话';
        try {
          const active = document.querySelector('#session-list .item.active .t');
          const raw = active && (active.textContent || '').trim();
          if (raw) chatTitle = displayTitle(raw, '对话');
        } catch (_) {}
        enterImmersive(chatTitle + (__c7().messages.length ? ' · ' + messages.length + ' 条' : ''), { showSessions: true, noAutoHide: true });
        return;
      }
    } else if (currentTab === 'story' || currentTab === 'adventure') {
      if (__s8().storyMessages.length > 0 && __s8().storySessionId) {
        enterImmersive(
          (currentTab === 'adventure' ? '冒险' : '跑团') + ' · ' + __s8().storyMessages.length + ' 条',
          {}
        );
        if (currentTab === 'adventure') {
          // S11/S13b: adventure reader is full-immersive by default — hide top
          // bar AND composer so the text owns the screen; composer slides back
          // at the bottom of content, top bar only on tap-at-top. Streaming /
          // typing keep the composer reachable.
          document.documentElement.classList.add('adv-imm-armed');
          try { stBindAdvImmChrome(); } catch (_) {}
          const typing = document.activeElement && document.activeElement.id === 'adv-input';
          if (!__s8().streaming && !typing) {
            document.documentElement.classList.add('imm-chrome-hidden');
          }
          try { stAdvImmChromeState(); } catch (_) {}
        } else {
          // S13h: 跑团 tab 与穿书统一——滚动时隐藏输入框+选项(imm-chrome-hidden)
          try { stBindStoryImmChrome(); } catch (_) {}
        }
        return;
      }
    } else if (currentTab === 'works') {
      let wtitle = '作者区';
      if (__wk().worksOpenPath) {
        const parts = String(__wk().worksOpenPath).split(/[/\\]/).filter(Boolean);
        wtitle = parts.length ? parts[parts.length - 1] : __wk().worksOpenPath;
        if (__wk().worksDirty) wtitle += ' · 未保存';
      }
      enterImmersive(wtitle, { showWorksNav: true });
      return;
    }
    exitImmersive();
  }
  const immBack = $('imm-back');
  if (immBack) {
    immBack.onclick = (e) => {
      e.preventDefault();
      // R3: 故事馆/档案馆统一走 stGoBack（向导→侧栏/sheet→剧场→history.back 优先级）
      if (currentTab === 'tavern' || currentTab === 'packs') {
        stGoBack();
        return;
      }
      if (currentTab === 'works') {
        exitImmersive();
        switchTab('home');
        return;
      }
      if (currentTab === 'chat') {
        // S9.4: 「离开」from immersive chat → back to the options/setup view
        showChatSetup();
        return;
      }
      if (currentTab === 'adventure') {
        // S11: back from immersive reader → setup gate
        document.documentElement.classList.remove('adv-imm-armed');
        exitImmersive();
        try { if (typeof advShowSetup === 'function') advShowSetup(); } catch (_) {}
        return;
      }
      exitImmersive();
    };
  }
  const stDrawerToggle = $('st-drawer-toggle');
  if (stDrawerToggle) {
    stDrawerToggle.onclick = async (e) => {
      e.preventDefault();
      const stLayout = $('st-layout');
      if (!stLayout) return;
      stLayout.classList.toggle('st-side-open');
      stSyncSideDrawer();
      if (stLayout.classList.contains('st-side-open')) {
        try { await stLoadSessions(); } catch (_) {}
        try { await stLoadSaves(); } catch (_) {}
        try { await renderDrawerRecall(); } catch (_) {}
        try { stRefreshCharSummary(); } catch (_) {}
        try { if (window.stRenderBookmarks) stRenderBookmarks(); } catch (_) {}
        try { if (window.stRefreshImmerseBg) stRefreshImmerseBg(); } catch (_) {}
      }
    };
  }
  function stSyncSideDrawer() {
    const stLayout = $('st-layout');
    const side = document.querySelector('#st-layout .st-side, #st-view-play .st-side');
    if (!stLayout || !side) return;
    const immersive = document.documentElement.getAttribute('data-immersive') === '1';
    const phone = window.matchMedia('(max-width: 56.25rem)').matches;
    const host = $('st-view-play');
    const open = stLayout.classList.contains('st-side-open');
    if (immersive && phone && host) {
      if (open && side.parentElement !== host) { host.appendChild(side); side.classList.add('st-side-float'); }
      if (!open && side.parentElement !== stLayout) { stLayout.appendChild(side); side.classList.remove('st-side-float'); }
    } else if (side.parentElement !== stLayout) {
      stLayout.appendChild(side); side.classList.remove('st-side-float');
    }
  }
  // S11/S13/S13b: adventure immersive reader chrome logic.
  // - Entering the reader hides the top bar (imm-chrome-hidden) AND the
  //   composer (adv-composer-hidden) — the text owns the screen.
  // - Scrolling the pane: composer hides while mid-text (immersive), slides
  //   back in at the very bottom (tail visible) so input is one tap away.
  //   Short content (not scrollable) keeps the composer visible.
  // - The top bar is tap-driven only: tap the top strip of the message pane
  //   toggles it; any scroll hides it again. It never overlaps the text.
  function stAdvShowChrome() {
    document.documentElement.classList.remove('imm-chrome-hidden');
  }
  function stAdvHideChrome() {
    document.documentElement.classList.add('imm-chrome-hidden');
  }
  function stAdvImmChromeState() {
    const root = document.documentElement;
    if (root.getAttribute('data-immersive') !== '1' || currentTab !== 'adventure') return;
    if (!root.classList.contains('adv-imm-armed')) return;
    const msg = $('adv-messages');
    const typing = document.activeElement && document.activeElement.id === 'adv-input';
    // composer: streaming/typing, or at the bottom of scrollable content, or
    // content too short to scroll → visible. Mid-text scrolling → hidden.
    let showComposer = !!(__s8().streaming || typing);
    if (!showComposer && msg) {
      const scrollable = msg.scrollHeight > msg.clientHeight + 8;
      const dist = msg.scrollHeight - msg.scrollTop - msg.clientHeight;
      // S13c: show only at the very bottom (8px); once shown, keep it until the
      // user scrolls well away (>120px). Hysteresis: the composer itself
      // changes the pane height, so a naive threshold flip-flops.
      const wasHidden = root.classList.contains('adv-composer-hidden');
      const showThresh = wasHidden ? 8 : 120;
      const atBottom = dist < (scrollable ? showThresh : Infinity);
      showComposer = !scrollable || atBottom;
    }
    root.classList.toggle('adv-composer-hidden', !showComposer);
    // any user scroll hides the top bar again; composer unaffected by this
    if (!root.classList.contains('imm-chrome-hidden')) {
      stAdvHideChrome();
    }
  }
  function stBindAdvImmChrome() {
    const msg = $('adv-messages');
    if (!msg) return;
    if (msg._advImmBound) return;
    msg._advImmBound = true;
    msg.addEventListener('scroll', function () {
      stAdvImmChromeState();
    }, { passive: true });
    // tap detection: record on pointerdown, act on pointerup only when it was
    // a real tap (small movement, short duration). The old 120ms timer got
    // cleared by pointerup before it could fire, so top taps did nothing.
    // tap zones on the message pane:
    //   top strip (< 15% height) → toggle top bar (reveals 返回 button)
    //   bottom band (> 86% height) → focus composer input (slides it in)
    msg.addEventListener('pointerdown', function (e) {
      if (e.button != null && e.button !== 0) return;
      stAdvTap = {
        x: e.clientX,
        y: e.clientY,
        t: Date.now(),
        ok: !(e.target && e.target.closest && e.target.closest('button, a, input, textarea, select, label')),
      };
    }, { passive: true });
    msg.addEventListener('pointerup', function (e) {
      const s = stAdvTap;
      stAdvTap = null;
      if (!s || !s.ok) return;
      if (e.button != null && e.button !== 0) return;
      const dx = Math.abs((e.clientX || 0) - s.x);
      const dy = Math.abs((e.clientY || 0) - s.y);
      if (dx > 12 || dy > 12) return; // scroll/drag, not a tap
      if (Date.now() - s.t > 650) return;
      const r = msg.getBoundingClientRect();
      if (r.height < 8) return;
      const relY = (e.clientY - r.top) / r.height;
      if (relY < 0.15) {
        document.documentElement.classList.toggle('imm-chrome-hidden');
      } else if (relY >= 0.86) {
        // composer may be collapsed (display:none) — reveal it first, then
        // focus (a display:none input cannot take focus)
        document.documentElement.classList.remove('adv-composer-hidden');
        const input = $('adv-input');
        if (input) input.focus();
        stAdvImmChromeState();
      }
    }, { passive: true });
    msg.addEventListener('pointercancel', function () { stAdvTap = null; }, { passive: true });
  }
  // S13h: 跑团 tab 沉浸滚动——与穿书(tavern)同款: 滚动>24px 隐藏
  // 输入框+选项(imm-chrome-hidden), 回顶恢复。story-choices 独立于
  // composer, CSS 侧随 imm-chrome-hidden 一并隐藏。
  function stBindStoryImmChrome() {
    const msg = $('story-messages');
    if (!msg) return;
    if (msg._storyImmBound) return;
    msg._storyImmBound = true;
    msg.addEventListener('scroll', function () {
      if (currentTab !== 'story') return;
      const root = document.documentElement;
      if (root.getAttribute('data-immersive') !== '1') return;
      root.classList.toggle('imm-chrome-hidden', msg.scrollTop > 24);
    }, { passive: true });
  }
  let stAdvTap = null;
  // stream end keeps the reader immersive — top bar stays hidden, composer
  // syncs to the bottom-of-content state
  window.addEventListener('story:stream-end', function () {
    if (currentTab === 'adventure') {
      stAdvHideChrome();
      stAdvImmChromeState();
    }
  });
  const chatSessionsToggle = $('chat-sessions-toggle');
  if (chatSessionsToggle) {
    chatSessionsToggle.onclick = (e) => {
      e.preventDefault();
      const el = $('session-drawer');
      if (el && !el.classList.contains('hidden')) closeSessionDrawer();
      else openSessionDrawer();
    };
  }
  const worksNavToggle = $('works-nav-toggle');
  if (worksNavToggle) {
    worksNavToggle.onclick = (e) => {
      e.preventDefault();
      const az = document.querySelector('#tab-works .az-layout');
      if (az) az.classList.toggle('az-nav-open');
    };
  }

  function normalizeHashName(raw) {
    let s = String(raw || '').trim();
    if (s.startsWith('#')) s = s.slice(1);
    if (s.startsWith('/')) s = s.slice(1);
    s = s.split('?')[0].split('/')[0].toLowerCase();
    if (HASH_ALIASES[s]) return HASH_ALIASES[s];
    if (KNOWN_TABS.has(s)) return s;
    return 'home'; // unknown → home
  }

  function parseLocationHash() {
    return normalizeHashName(location.hash || '');
  }

  // Parse extra path segments from hash, e.g. #/tavern/session/<id> -> ['session','<id>']
  function parseHashSegments(raw) {
    let s = String(raw || location.hash || '').trim();
    if (s.startsWith('#')) s = s.slice(1);
    if (s.startsWith('/')) s = s.slice(1);
    return s.split('?')[0].split('/').filter(Boolean);
  }

  function writeHashForTab(name) {
    const desired = '#/' + name;
    if (location.hash === desired) return;
    // suppress the synthetic hashchange from our own write
    suppressHashWrite = true;
    location.hash = desired;
    // fallback clear in case hashchange does not fire (same-doc edge cases)
    setTimeout(() => { suppressHashWrite = false; }, 50);
  }

  // S8.28: author zone sub-navigation — switch between compose/files/worldbook/charcard/regex views
  // opts.persist=false: 临时切换（如创作工具箱入口直达关系图），不改作者区记忆的视图
  function switchAzView(name, opts) {
    if (!name) return;
    const persist = !(opts && opts.persist === false);
    azLastView = name;
    if (persist) localStorage.setItem('kaleido_az_lastview', name);
    document.querySelectorAll('.az-nav button[data-azview]').forEach((btn) => {
      btn.classList.toggle('active', btn.dataset.azview === name);
    });
    document.querySelectorAll('.az-view').forEach((el) => {
      el.classList.toggle('hidden', el.id !== 'az-view-' + name);
    });
    // View-specific data refresh
    if (name === 'worldbook' || name === 'charcard' || name === 'regex') {
      loadPartner().catch(console.warn);
    }
    if (name === 'files') {
      loadWorksTree().catch(console.warn);
      if (window.__kaleidoSettings && window.__kaleidoSettings.loadStylePresets) window.__kaleidoSettings.loadStylePresets().catch(console.warn);
      if (typeof loadWorksVersionsSidebar === 'function') loadWorksVersionsSidebar().catch(console.warn);
    }
    if (name === 'compose') {
      loadAuthorProjects().catch(console.warn);
      loadPartner().catch(console.warn);
    }
    // 关系图/伏笔/AI分析 进面板自动加载（2026-08-10: 之前从不自动调用，
    // 用户进面板永远是空白 canvas，必须手动点刷新——前端"什么都没有"根因）
    if (name === 'graph' && typeof loadGraph === 'function') {
      loadGraph().catch(console.warn);
    }
    if (name === 'foreshadow' && typeof loadForeshadows === 'function') {
      loadForeshadows().catch(console.warn);
    }
    if (name === 'analysis' && typeof loadAnTasks === 'function') {
      loadAnTasks().catch(console.warn);
    }
    // 2026-08-10: 进数据面板时刷新小说下拉（故事馆新增剧本后自动出现）
    if ((name === 'graph' || name === 'foreshadow' || name === 'analysis')
        && typeof refreshPackSelect === 'function') {
      refreshPackSelect().catch(console.warn);
    }
    if (currentTab === 'works') {
      if (name === 'files') {
        let wtitle = '作者区 · 文稿';
        if (__wk().worksOpenPath) {
          const parts = String(__wk().worksOpenPath).split(/[\/\\\\]/).filter(Boolean);
          wtitle = parts.length ? parts[parts.length - 1] : __wk().worksOpenPath;
          if (__wk().worksDirty) wtitle += ' · 未保存';
        }
        enterImmersive(wtitle, { showWorksNav: true });
      } else if (name !== 'files') {
        exitImmersive();
      }
    }
  }
  window.switchAzView = switchAzView; // expose for partner redirect

  async function switchTab(name, opts) {
    if (!name) return;
    const fromHash = opts && opts.fromHash;
    if (!KNOWN_TABS.has(name)) name = 'home';
    const prevTab = currentTab;
    currentTab = name;
    document.querySelectorAll('.tab').forEach((t) => {
      const active = t.dataset.tab === name;
      t.classList.toggle('active', active);
      if (t.tagName === 'BUTTON') {
        // buttons cannot carry aria-selected (axe aria-allowed-attr); expose
        // the active nav state via aria-current instead
        if (active) t.setAttribute('aria-current', 'page');
        else t.removeAttribute('aria-current');
      } else {
        t.setAttribute('aria-selected', active ? 'true' : 'false');
        t.setAttribute('tabindex', active ? '0' : '-1');
      }
    });
    document.querySelectorAll('.tab-panel').forEach((el) => {
      const n = el.id && el.id.indexOf('tab-') === 0 ? el.id.slice(4) : '';
      const show = n === name;
      // Reset scroll before the panel becomes visible so the new tab always lands at the top (mobile panels are overflow-y:auto)
      if (show && prevTab !== name) {
        try { el.scrollTop = 0; } catch (_) {}
      }
      el.classList.toggle('hidden', !show);
      el.setAttribute('aria-hidden', show ? 'false' : 'true');
      el.classList.remove('is-entering');
      if (show && prevTab !== name) {
        // force reflow so re-adding is-entering restarts CSS animation
        void el.offsetWidth;
        el.classList.add('is-entering');
        const homePanel = el.querySelector('.home-panel');
        if (homePanel) {
          homePanel.classList.remove('is-entering');
          void homePanel.offsetWidth;
          homePanel.classList.add('is-entering');
          window.setTimeout(() => homePanel.classList.remove('is-entering'), 900);
        }
        window.setTimeout(() => el.classList.remove('is-entering'), 450);
        // Also reset body/html scroll — Android WebView may scroll the main document
        [0, 100, 300].forEach((ms) => {
          window.setTimeout(() => {
            try { window.scrollTo({ top: 0, behavior: 'auto' }); } catch (_) {}
            try { document.documentElement.scrollTop = 0; } catch (_) {}
            try { document.body.scrollTop = 0; } catch (_) {}
            try { el.scrollTop = 0; } catch (_) {}
          }, ms);
        });
      }
    });
    if (name === 'partner') {
      // S8.28: redirect to author zone worldbook view
      switchTab('works');
      setTimeout(() => {
        if (currentTab !== 'works') return;
        switchAzView('worldbook');
      }, 50);
      return;
    }
    if (name === 'bond') {
      loadPartner().then(() => renderBondPage()).catch(() => renderBondPage());
    }
    if (name === 'settings') (window.__kaleidoSettings && window.__kaleidoSettings.loadSettings || loadSettings)().catch(console.warn);
    if (name === 'embedlab') {
      loadEmbedLabStatus().catch(console.warn);
      loadEmbedLabEvents().catch(console.warn);
    }
    if (name === 'works') {
      // AZ-4: author zone loads projects + partner before works tree
      Promise.all([
        loadAuthorProjects().catch((e) => { if ($('az-project-msg')) $('az-project-msg').textContent = e.message; }),
        loadPartner().catch((e) => { if ($('az-composer-msg')) $('az-composer-msg').textContent = e.message; }),
      ]).then(() => {
        loadWorksTree().catch(console.warn);
        if (window.__kaleidoSettings && window.__kaleidoSettings.loadStylePresets) window.__kaleidoSettings.loadStylePresets().catch(console.warn);
        else if (window.__kaleidoLoadStylePresets) window.__kaleidoLoadStylePresets().catch(console.warn);
        if (typeof loadWorksVersionsSidebar === 'function') loadWorksVersionsSidebar().catch(console.warn);
        else if (window.__kaleidoLoadWorksVersions) window.__kaleidoLoadWorksVersions().catch(console.warn);
      });
      // S8.28: restore last sub-view (only while still on the works tab —
      // if the user switched away during the async load, skip to avoid
      // flashing the author-zone view over the destination tab)
      setTimeout(() => {
        if (currentTab !== 'works') return;
        switchAzView(azLastView || 'compose');
      }, 30);
    }
    // S8.28: author-zone bottom tab bar event delegation
    document.querySelectorAll('.az-mob-tab button[data-azview]').forEach((btn) => {
      btn.onclick = () => switchAzView(btn.dataset.azview);
    });
    // S8.28: adjust immersive state per author-zone sub-view
    if (name === 'works') {
      document.documentElement.setAttribute('data-author-zone', '1');
      if (azLastView === 'files' || (!azLastView && localStorage.getItem('kaleido_az_lastview') === 'files')) {
        let wtitle = '作者区 · 文稿';
        if (__wk().worksOpenPath) {
          const parts = String(__wk().worksOpenPath).split(/[\/\\\\]/).filter(Boolean);
          wtitle = parts.length ? parts[parts.length - 1] : __wk().worksOpenPath;
          if (__wk().worksDirty) wtitle += ' · 未保存';
        }
        enterImmersive(wtitle, { showWorksNav: true });
      } else {
        exitImmersive();
      }
      // 作者区也同步 URL hash（此前提前 return 跳过 writeHashForTab，刷新会丢 tab 状态）
      if (!fromHash) writeHashForTab(name);
      return;
    }
    if (name !== 'tavern') exitImmersive();
    if (name !== 'works') document.documentElement.removeAttribute('data-author-zone');
    if (name === 'jobs') refreshJobs().catch(console.warn);
    if (name === 'home') renderHomeRecent();
    if (name === 'tavern') {
      // S8.10: library/entry first — never land on play/immersive via tab switch alone
      // 但如果刚创建会话从档案馆跳来，保持 play 视图
      if (!window._stSkipEntryReset) stSwitchView('entry');
      else delete window._stSkipEntryReset;
      exitImmersive();
      await stRefresh();
      // Support deep-linking a session: #/tavern/session/<sessionId>
      const segs = parseHashSegments();
      if (segs[1] === 'session' && segs[2]) {
        await stLoadSession(segs[2]).catch((err) => {
          console.warn('tavern deep-link session load failed', err);
          stStatus('会话加载失败：' + (err.message || err));
        });
      }
    }
    if (name === 'packs') {
      // S8.26: archive tab loads pack library + sessions itself (stRefresh only fires on tavern)
      stLoadPacks().catch((e) => { console.warn('packs load failed', e); stStatus('加载 Pack 失败：' + (e.message || e)); });
      stLoadSessions().catch((e) => { console.warn('sessions load failed', e); });
      if (typeof stRenderContinueCard === 'function') stRenderContinueCard();
    }
    if (name === 'chat') {
      // S9.4: entering chat tab always lands on the options/setup view
      showChatSetup();
      refreshSessions().catch(console.warn);
    }
    if (name === 'story') {
      refreshStorySelects();
      ensureStorySession().catch(console.warn);
    }
    if (name === 'adventure') {
      refreshStorySelects();
      refreshAdventureSelects();
      ensureStorySession().then(() => {
        renderStoryMessages();
        // S11: has a session with messages → reader; else first-run setup gate
        if (__s8().storyMessages.length > 0 && __s8().storySessionId) {
          if (typeof advShowReader === 'function') advShowReader();
        } else {
          if (typeof advShowSetup === 'function') advShowSetup();
        }
      }).catch(console.warn);
    }
    if (name === 'aiadmin' && typeof P5AiLoad === 'function') P5AiLoad();
    if (name === 'moa') {
      if (typeof MoaLoadPanels === 'function') MoaLoadPanels();
      if (typeof MoaLoadSessions === 'function') MoaLoadSessions();
    }
    syncMobileChrome(name);
    // auto-close tools sheet after choosing a tool tab
    if (TOOL_TABS.has(name) || PRIMARY_TABS.has(name)) closeToolsSheet();
    // keep URL hash in sync (avoid loops when called from hashchange)
    if (!fromHash) writeHashForTab(name);
    if (name === 'bookshelf' && typeof loadBookshelf === 'function') loadBookshelf();
    updateImmersive();
  }
  window.switchTab = switchTab; // expose for inline onclick
  function onHashChange() {
    if (suppressHashWrite) {
      suppressHashWrite = false;
      return;
    }
    // R3: 返回/前进的 hashchange 落在弹层（向导/侧栏/工具 sheet/剧场）上时，
    // 先关弹层/退出剧场，不直接切 tab（Android 部分 WebView 只派发 hashchange 的兜底）。
    if (typeof stHasOpenOverlay === 'function' && stHasOpenOverlay()) {
      stGoBack(true);
      suppressHashWrite = true;
      setTimeout(() => { suppressHashWrite = false; }, 50);
      return;
    }
    const name = parseLocationHash();
    if (name === currentTab) return;
    switchTab(name, { fromHash: true });
  }

  window.addEventListener('hashchange', onHashChange);

  document.querySelectorAll('.tab').forEach((t) => {
    t.onclick = (e) => {
      e.preventDefault();
      if (t.dataset.mnav === 'tools') {
        const sheet = $('tools-sheet');
        if (sheet && !sheet.classList.contains('hidden')) closeToolsSheet();
        else openToolsSheet();
        return;
      }
      if (t.dataset.tab) switchTab(t.dataset.tab);
    };
  });
  document.querySelectorAll('[data-goto]').forEach((el) => {
    el.addEventListener('click', (e) => {
      e.preventDefault();
      const n = el.getAttribute('data-goto');
      if (!n) return;
      // 创作工具箱入口（data-tool）：进作者区并直达关系图，且不覆盖作者区记忆的视图
      if (n === 'works' && el.dataset.tool) {
        const prev = azLastView;
        switchTab('works');
        setTimeout(() => {
          if (currentTab !== 'works') return;
          switchAzView('graph', { persist: false });
          azLastView = prev;
        }, 90);
        return;
      }
      if (n) switchTab(n);
    });
  });
  // S8.28: az-nav — author zone sub-navigation
  document.querySelectorAll('.az-nav button[data-azview]').forEach((btn) => {
    btn.addEventListener('click', (e) => {
      e.preventDefault();
      switchAzView(btn.dataset.azview);
    });
  });
  // S8.10-B: title tap → home (extra path; bnav also has 首页)
  const pageTitleEl = $('page-title');
  if (pageTitleEl) {
    pageTitleEl.style.cursor = 'pointer';
    pageTitleEl.title = '回首页';
    pageTitleEl.addEventListener('click', (e) => {
      e.preventDefault();
      switchTab('home');
    });
  }

  // bottom nav
  document.querySelectorAll('.bnav').forEach((b) => {
    b.onclick = (e) => {
      e.preventDefault();
      if (b.dataset.mnav === 'tools') {
        const sheet = $('tools-sheet');
        if (sheet && !sheet.classList.contains('hidden')) closeToolsSheet();
        else openToolsSheet();
        return;
      }
      if (b.dataset.tab) switchTab(b.dataset.tab);
    };
  });
  // sheet / drawer close
  const toolsClose = $('tools-sheet-close');
  if (toolsClose) toolsClose.onclick = closeToolsSheet;
  const toolsBackdrop = $('tools-sheet-backdrop');
  if (toolsBackdrop) toolsBackdrop.onclick = closeToolsSheet;
  const drawerClose = $('session-drawer-close');
  if (drawerClose) drawerClose.onclick = closeSessionDrawer;
  const drawerBackdrop = $('session-drawer-backdrop');
  if (drawerBackdrop) drawerBackdrop.onclick = closeSessionDrawer;
  const sessBtn = $('mobile-sessions-btn');
  if (sessBtn) sessBtn.onclick = () => openSessionDrawer();

  // init auto UI + listen to breakpoint changes
  applyAutoUi();
  window.matchMedia('(max-width: 900px)').addEventListener('change', () => applyAutoUi());
  window.addEventListener('resize', () => {
    // S9.3b: header height can shift on wrap — refresh --top-h without full remeasure churn
    try {
      const topEl = document.querySelector('.top');
      if (topEl) document.documentElement.style.setProperty('--top-h', topEl.offsetHeight + 'px');
      const bnav = document.querySelector('.bottom-nav');
      // L4: mirror applyAutoUi — only write when height is real; desktop => 0.
      if (window.matchMedia('(max-width: 900px)').matches) {
        if (bnav && bnav.offsetHeight > 0) {
          document.documentElement.style.setProperty('--bnav-h', bnav.offsetHeight + 'px');
        }
      } else {
        document.documentElement.style.setProperty('--bnav-h', '0px');
      }
    } catch (_) {}
  });

  // Escape to close modal sheets/drawers; surgical focus trap helpers
  document.addEventListener('keydown', (e) => {
    if (e.key !== 'Escape') return;
    const tools = $('tools-sheet');
    const drawer = $('session-drawer');
    if (tools && !tools.classList.contains('hidden')) {
      e.preventDefault();
      closeToolsSheet();
    } else if (drawer && !drawer.classList.contains('hidden')) {
      e.preventDefault();
      closeSessionDrawer();
    }
  });

  // ── P1-3 S2.6: module-boundary facade for the routing core ──────────────
  // The functions above live in this IIFE closure; real ES modules cannot see
  // closure bindings (S2.5 lesson). Constructing the facade HERE — inside the
  // scope that owns the bindings — is safe: every reference resolves locally,
  // so esbuild cannot treat anything as an unresolved free variable (the
  // S2.5 DCE trap). src/js/tabs_bridge.js re-exports these lazily as real
  // ESM exports, which unblocks api_shell.showMain's real import and the
  // upcoming _keyboard-part conversion.
  try {
    window.__kaleidoTabs = {
      switchTab,
      switchAzView,
      updateImmersive,
      exitImmersive,
      applyAutoUi,
      parseLocationHash,
      parseHashSegments,
      writeHashForTab,
      openToolsSheet,
      closeToolsSheet,
      openSessionDrawer,
      closeSessionDrawer,
      get currentTab() { return currentTab; },
      set currentTab(v) { currentTab = v; },
      // S2.10: tavern-core stPinViewHash/popstate write this from the real module
      get suppressHashWrite() { return suppressHashWrite; },
      setSuppressHashWrite(v) { suppressHashWrite = v; },
      // S2.15: story.js (real module) reads/writes bond picks + imm-chrome fn
      get bondPickWb() { return bondPickWb; }, set bondPickWb(v) { bondPickWb = v; },
      get bondPickCc() { return bondPickCc; }, set bondPickCc(v) { bondPickCc = v; },
      stAdvImmChromeState: stAdvImmChromeState,
    };
  } catch (_) {}

  /** ST regex_scripts (card.fields.stRegexScripts) — display-time only. */
