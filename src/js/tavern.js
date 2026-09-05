/**
 * src/js/tavern.js — Story Tavern domain as a real ES module (P1-3 S2.10).
 *
 * Merged from 10 former IIFE fragments (parts.json order preserved):
 *   _tavern-core / _tavern-pack / _tavern-session / _tavern-send / _tavern-shelf /
 *   _tavern-packmgmt / _tavern-lore / _tavern-side / _tavern-char-summary /
 *   _tavern-bg-immerse (its wrapper IIFE was unwrapped).
 *
 * Boundary decisions (docs/P1_3_VITE_MIGRATION.md S2.10):
 * - tavern lets (tavernPacks/Sessions/Session/Pack/Streaming/RunId/stTavernUserScrolled)
 *   moved here as canonical module state; remaining closure consumers read via
 *   stCurrentSession()/stCurrentPack().
 * - stHistoryExpanded is owned by state.js; writes go through setStHistoryExpanded().
 * - closure-only deps accessed via facades: __authToken/__appSettings/__curTab/
 *   __setSuppressHashWrite/__chatState/__storyState.
 * - escapeHtml/esc are local copies (keyboard.js precedent).
 * - ST_ICONS/PLAYABLE_LABELS/stripChoicesBlock/resolveMessageOptions now live in utils.js.
 * - All top-level DOM/window wiring runs from initTavernUI(), called by main.js.
 */
import { $ } from './dom.js';
import { api } from './api.js';
import { apiBase, friendlyError } from './api_shell.js';
import { showToast } from './toast.js';
import { showConfirm, showPrompt } from './dialog.js';
import { applyStRegexScripts } from './st_regex.js';
import { cssEscape, buildBubbleEl, fillBubbleBody } from './chat.js';
import { switchTab, closeToolsSheet, exitImmersive, updateImmersive } from './tabs_bridge.js';
import {
  uid, ADULT_OK_KEY, TAVERN_SID_KEY, ST_READPOS_PREFIX,
  PLAYABLE_LABELS, stripChoicesBlock, resolveMessageOptions,
} from './utils.js';
import { ST_VISIBLE_TURNS, setStHistoryExpanded, stHistoryExpanded } from './state.js';
import { ST_ICONS } from './utils.js';
import { switchAzView } from './tabs_bridge.js';
import { readSSE } from './jobs.js';
import { sessionId } from './state_core.js';
import mammoth from 'mammoth/mammoth.browser.js';

/** closure-state accessors (canonical lets remain inside the virtual-module IIFE) */
const __authToken = () => window.__kaleidoAuthState.token;
const __appSettings = () => window.__kaleidoSettingsState.settings;
const __curTab = () => window.__kaleidoTabs.getCurrentTab();
const __setSuppressHashWrite = (v) => window.__kaleidoTabs.setSuppressHashWrite(v);
const __chatState = () => window.__kaleidoChatState;
const __storyState = () => window.__kaleidoStoryState;

/** local copies (_works-part/_chapter-diary-part keep their own; keyboard.js precedent) */
function escapeHtml(s) {
  return String(s)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;');
}
function esc(s) {
  return String(s == null ? '' : s)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;');
}

/* ===================== canonical tavern state (moved from _state-part.js) ===================== */
let tavernPacks = [];
let tavernSessions = [];
export let tavernSession = null;
export let tavernPack = null;
let tavernStreaming = false;
// S8.31: 用户手动滚动过消息区（用于流式期间保持视口在开头，尊重用户滚动）
let stTavernUserScrolled = false;
let tavernRunId = null;

/** remaining closure consumers (_tabs-part/_drift-part/_world-part) read these */
export function stCurrentSession() { return tavernSession; }
export function stCurrentPack() { return tavernPack; }

function stIcon(name) { return ST_ICONS[name] || ''; }

function stEmpty(text, action) {
  return `<div class="st-empty"><svg aria-hidden="true" xmlns="http://www.w3.org/2000/svg" width="28" height="28" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M7.9 20A9 9 0 1 0 4 16.1L2 22Z"/></svg><span>${escapeHtml(text)}</span>${action ? `<span class="action">${escapeHtml(action)}</span>` : ''}</div>`;
}

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
  if (__appSettings() && __appSettings().tavernAdultOk) return true;
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
    __appSettings().tavernAdultOk = true;
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

let stNavFrom = '';

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

function stPinViewHash() {
  try {
    const keep = '#/' + __curTab();
    if (location.hash !== keep) {
      __setSuppressHashWrite(true);
      history.replaceState(null, '', keep);
      setTimeout(() => { __setSuppressHashWrite(false); }, 50);
    }
  } catch (_) {}
}

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
    if (__curTab() === 'packs') {
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

function stExitPlay() {
  const play = $('st-view-play');
  if (play) play.classList.add('hidden');
  // 退出播放:隐藏语义记忆召回条(避免残留占满正文上方)
  const recallBar = $('st-recall-bar');
  if (recallBar) recallBar.classList.add('hidden');
  const listview = $('st-packs-listview');
  const packDetail = $('st-view-pack');
  if (stNavFrom === 'packs-detail' && __curTab() === 'packs') {
    // 从包详情进 play → 恢复包详情
    stSwitchView('pack');
    if (listview) listview.classList.add('hidden');
  } else if (__curTab() === 'packs') {
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

let stStageEl = null;

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

function stStageEditForm(sd, sid) {
  const rp = (sd && sd.runPolicy) || {};
  const v = function (o, k, fb) { return (o && o[k] !== undefined && o[k] !== null) ? o[k] : fb; };
  const fRow = function (label, inner) {
    return '<div class="st-f-row"><label>' + label + '</label>' + inner + '</div>';
  };
  return '<form class="st-stage-form" data-sid="' + encodeURIComponent((tavernSession && tavernSession.sessionId) || '') + '">' +
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
    // [主线完结] pack 走完终点 → 标题提示自由生长（dc.mainlineExhausted）
    try {
      if (dc.mainlineExhausted) {
        out += stStageSec('🎛 导演台', '主线已完结·自由生长中', h);
      } else {
        out += stStageSec('🎛 导演台', pending ? '待执行' : (plan.goal ? '运行中' : '空闲'), h);
      }
    } catch (_) {
      out += stStageSec('🎛 导演台', pending ? '待执行' : (plan.goal ? '运行中' : '空闲'), h);
    }
    // H3 (吞噬 humanizer-zh): 去 AI 味评分展示（lastTurnDiagnostic 直带）
    try {
      const dg = tavernSession.lastTurnDiagnostic || tavernSession.last_turn_diagnostic || null;
      if (dg && dg.humanizeTotal) {
        const col = dg.humanizeTotal >= 45 ? '#7CFC98' : (dg.humanizeTotal >= 35 ? '#ffd166' : '#ff6b6b');
        out += stStageSec('✍️ 去 AI 味', dg.humanizeTotal + '/50 ' + (dg.humanizeGrade || ''),
          '<div class="st-stage-row"><b>评分</b><span style="color:' + col + '">' + dg.humanizeTotal + '/50 ' +
          (dg.humanizeGrade || '') + '</span><b>命中</b><span>' + (dg.humanizeHits || 0) + ' 处</span></div>');
      }
    } catch (_) {}
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
    // [自动罗盘] 导演台罗盘行（compass 随 actor-states 返回）
    try {
      const cp = ast.compass || {};
      const ai = (cp.authorIntent || '').trim();
      const cf = (cp.currentFocus || '').trim();
      if (ai || cf) {
        let rh = '';
        if (ai) rh += '<span>【全书承诺】' + stStageVal(ai) + '</span>';
        if (cf) rh += '<span>【近期目标】' + stStageVal(cf) + '</span>';
        out += stStageSec('🧭 罗盘', '自动', rh);
      }
    } catch (_) {}
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
  // [吞噬 Front Porch AI pockets.dart] 口袋与衣物（per-character 只读展示，P1-B À la carte 独立开关）
  try {
    const sid = tavernSession.sessionId;
    const pk = await stStageFetch('/pockets');
    const pockets = pk.pockets || pk || {};
    const pids = Object.keys(pockets);
    // pocketsEnabled 单独开关（默认开，Own switch. Does not need the Realism Engine.）
    let pocketsEnabled = true;
    try { const pe = await stStageFetch('/pockets-enabled'); pocketsEnabled = pe.pocketsEnabled !== false; } catch (_) {}
    let evExtract = true;
    try { const ee = await stStageFetch('/event-extract'); evExtract = ee.eventExtract !== false; } catch (_) {}
    let ph = '<div class="st-stage-acts"><label style="display:inline-flex;align-items:center;gap:8px;cursor:pointer"><input type="checkbox" id="st-pockets-enabled" ' + (pocketsEnabled ? 'checked' : '') + ' /> 口袋与衣物（独立开关，提示词注入）</label><span style="margin-left:8px;font-size:11px;color:var(--text-dim,#8b93a7)">关时仅隐藏提示词注入，数据保留</span></div>';
    ph += '<div class="st-stage-acts"><label style="display:inline-flex;align-items:center;gap:8px;cursor:pointer"><input type="checkbox" id="st-event-extract" ' + (evExtract ? 'checked' : '') + ' /> ⚡ 全自动事件提取（回合末后台 LLM 写口袋/承诺/成长/羁绊）</label></div>';
    if (!pids.length) {
      ph += '<div class="st-stage-empty">暂无口袋记录 —— 角色获得/穿戴物品后此处展示</div>';
    } else {
      pids.forEach(function (cid) {
        const p = pockets[cid] || {};
        const worn = (p.worn || []).map(function (it) { return (it && it.name) ? (it.name + (it.state ? '（' + it.state + '）' : '')) : String(it); }).join('、');
        const carrying = (p.carrying || []).map(function (it) { return (it && it.name) ? (it.name + (it.state ? '（' + it.state + '）' : '')) : String(it); }).join('、');
        const setAside = (p.setAside || p.set_aside || []).map(function (e) { const it = e.item || e; return (it && it.name) ? (it.name + (it.state ? '（' + it.state + '）' : '') + (e.clothing ? ' [衣物]' : '')) : String(e); }).join('、');
        ph += '<div class="st-stage-row"><b>' + stStageVal(cid) + '</b></div>';
        if (worn) ph += '<div class="st-stage-ev">👗 身穿：' + stStageVal(worn) + '</div>';
        if (carrying) ph += '<div class="st-stage-ev">🎒 携带：' + stStageVal(carrying) + '</div>';
        if (setAside) ph += '<div class="st-stage-ev" style="color:var(--text-dim,#8b93a7)">🪑 暂存：' + stStageVal(setAside) + '（衣物次日清晨过期）</div>';
        if (!worn && !carrying && !setAside) ph += '<div class="st-stage-ev" style="color:var(--text-dim,#8b93a7)">（空）</div>';
      });
    }
    if (!pocketsEnabled) ph += '<div class="st-stage-ev" style="color:var(--text-dim,#8b93a7)">（口袋提示词注入已关闭，LLM 不会看到随身物品；数据仍保留）</div>';
    out += stStageSec('🎒 口袋与衣物' + (pocketsEnabled ? '' : ' · 已关闭'), pids.length ? pids.length + ' 人' : '0 人', ph);
  } catch (e) {
    out += stStageSec('🎭 角色状态', '', '<div class="st-stage-err">读取失败：' + (e.message || e) + '</div>');
  }
  // [P2-A 吞噬 Front Porch AI needs_simulation.rs] Needs 六维只读+标注（0-100，urgent≤35/critical≤20）
  try {
    const nk = await stStageFetch('/needs');
    const needsMap = nk.needs || nk || {};
    const nids = Object.keys(needsMap);
    let nh = '';
    if (!nids.length) nh = '<div class="st-stage-empty">暂无 Needs 记录 —— 需求随回合自衰减</div>';
    else nids.forEach(function (cid) {
      const n = needsMap[cid] || {};
      const vec = n.vector || n || {};
      const pend = n.pendingCatastrophe || n.pending_catastrophe || '';
      const chips = ['hunger','energy','social','fun','hygiene','comfort'].map(function (k) {
        const v = vec[k]; if (v == null) return '';
        const cls = v <= 20 ? 'color:#ff6b6b' : v <= 35 ? 'color:#ffb86b' : '';
        return '<span class="st-stage-chip" style="' + cls + '">' + k + '=' + v + '</span>';
      }).filter(Boolean).join('');
      nh += '<div class="st-stage-row"><b>' + stStageVal(cid) + '</b> ' + chips + '</div>';
      if (pend) nh += '<div class="st-stage-ev" style="color:#ff6b6b">⚠️ ' + stStageVal(pend) + '</div>';
    });
    out += stStageSec('🧠 Needs 六维', nids.length ? nids.length + ' 人' : '0 人', nh + '<div class="st-stage-acts"><button type="button" data-stage-act="needs-tick" data-sid="' + encodeURIComponent((tavernSession && tavernSession.sessionId) || '') + '">⏳ 推进一回合衰减</button></div>');
  } catch (e) { out += stStageSec('🧠 Needs 六维', '', '<div class="st-stage-err">读取失败：' + (e.message || e) + '</div>'); }
  // [P2-B/P3-A 吞噬 Front Porch AI growth-rings + world] Growth / Climate
  try {
    const [gk, wc] = await Promise.all([stStageFetch('/growth').catch(function () { return { growth: { rings: [] } }; }), stStageFetch('/world-climate').catch(function () { return { worldClimate: {} }; })]);
    const rings = (gk.growth && gk.growth.rings) ? gk.growth.rings : (Array.isArray(gk.rings) ? gk.rings : []);
    const climate = wc.worldClimate || wc.world_climate || wc || {};
    const atmo = climate.atmosphere || 'breathable';
    const grav = climate.gravity || 'earth';
    const tb = climate.temp_band || climate.tempBand || 'auto';
    let gh = '';
    if (!rings.length) gh = '<div class="st-stage-empty">暂无年轮 —— 事件触发后自动登记</div>';
    else rings.forEach(function (r) {
      gh += '<div class="st-stage-row"><b>' + stStageVal(r.character || r.characterId || '') + '</b><span>' + stStageVal(r.triggerEvent || r.trigger_event || r.event || '') + '</span><span class="st-stage-chip">' + (function(s){s=Number(s||0);return s>=0.8?'established':s>=0.35?'developing':'fragile';})(r.strength) + ' ' + stStageVal((r.strength != null ? Number(r.strength).toFixed(2) : '')) + '</span>' + (r.faded ? '<span class="st-stage-chip">faded</span>' : '') + '</div>';
    });
    out += stStageSec('🌱 成长年轮', rings.length + ' 枚', gh);
    out += stStageSec('🌍 世界气候', atmo + ' · ' + grav + ' · ' + tb, '<div class="st-stage-row"><b>atmosphere</b><span>' + stStageVal(atmo) + '</span><b>gravity</b><span>' + stStageVal(grav) + '</span><b>temp_band</b><span>' + stStageVal(tb) + '</span></div><div class="st-stage-acts"><button type="button" data-stage-act="climate-edit" data-sid="' + encodeURIComponent((tavernSession && tavernSession.sessionId) || '') + '">✏️ 编辑气候</button></div>');
  } catch (e) { out += stStageSec('🌱 成长年轮 / 🌍 世界气候', '', '<div class="st-stage-err">读取失败：' + (e.message || e) + '</div>'); }
  // [P4 吞噬 Front Porch AI chaos / milestones / objectives / dreams]
  try {
    const [ch, ms, objs, dreamRes, eps] = await Promise.all([
      stStageFetch('/chaos').catch(function () { return { chaos: {} }; }),
      stStageFetch('/milestones').catch(function () { return { milestones: [] }; }),
      stStageFetch('/objectives').catch(function () { return { objectives: [] }; }),
      stStageFetch('/dreams').catch(function () { return { dream: {}, episodes: { crumbs: [] } }; }),
      stStageFetch('/episodes').catch(function () { return { episodes: { crumbs: [] } }; }),
    ]);
    const chaos = ch.chaos || ch || {};
    const miles = ms.milestones || ms || [];
    const objectives = objs.objectives || objs || [];
    const dream = (dreamRes.dream || dreamRes || {});
    const dreamText = dream.last_dream || dream.lastDream || dream.lastDreamText || '';
    const episodes = (eps.episodes && eps.episodes.crumbs) ? eps.episodes.crumbs : (dreamRes.episodes && dreamRes.episodes.crumbs) ? dreamRes.episodes.crumbs : [];
    let chh = '<div class="st-stage-row"><b>pressure</b><span>' + stStageVal(chaos.pressure != null ? chaos.pressure : 0) + '/100</span><span>' + (chaos.enabled ? 'enabled' : 'off') + '</span>' + (chaos.pendingInjection || chaos.pending_injection ? '<span class="st-stage-chip">pending</span>' : '') + '</div>';
    if (chaos.pendingInjection || chaos.pending_injection) chh += '<div class="st-stage-ev">' + stStageVal(chaos.pendingInjection || chaos.pending_injection) + '</div>';
    chh += '<div class="st-stage-acts"><button type="button" data-stage-act="chaos-toggle" data-sid="' + encodeURIComponent((tavernSession && tavernSession.sessionId) || '') + '">' + (chaos.enabled ? '关闭 Chaos' : '开启 Chaos') + '</button><button type="button" data-stage-act="chaos-tick" data-sid="' + encodeURIComponent((tavernSession && tavernSession.sessionId) || '') + '">🎲 推进压力</button></div>';
    out += stStageSec('🎲 Chaos / Chance Time', chaos.enabled ? 'pressure ' + (chaos.pressure||0) : 'off', chh);
    let msh = '';
    if (!miles.length) msh = '<div class="st-stage-empty">暂无里程碑</div>';
    else miles.forEach(function (m) { msh += '<div class="st-stage-row"><b>' + stStageVal(m.character) + '</b><span>' + stStageVal(m.label || m.kind) + '</span><span class="st-stage-chip">' + stStageVal(m.kind) + ' tier ' + stStageVal(m.tier) + '</span></div>'; });
    out += stStageSec('🏅 里程碑', miles.length + ' 个', msh);
    let obh = '';
    if (!objectives.length) obh = '<div class="st-stage-empty">暂无目标 —— POST /objectives 创建</div>';
    else objectives.forEach(function (o) {
      const done = (o.tasks||[]).filter(function(x){return x.completed;}).length;
      const total = (o.tasks||[]).length || 1;
      const pct = Math.round(done/total*100);
      const stage = pct>=100?'achieved':pct>=75?'nearly there':pct>=50?'halfway there':pct>=25?'gaining ground':'just beginning';
      obh += '<div class="st-stage-row"><b>' + stStageVal(o.title) + '</b><span>' + stStageVal(o.status) + ' · ' + stStageVal(o.owner) + '</span><span class="st-stage-chip">' + stage + '</span></div>';
      (o.tasks||[]).forEach(function (t) { obh += '<div class="st-stage-ev">' + (t.completed ? '✅' : '⬜') + ' ' + stStageVal(t.title) + '</div>'; });
    });
    obh += '<div class="st-stage-acts"><button type="button" data-stage-act="obj-new" data-sid="' + encodeURIComponent((tavernSession && tavernSession.sessionId) || '') + '">＋ 新目标</button></div>';
    out += stStageSec('🎯 目标', objectives.length + ' 个', obh);
    const ambs = dream.ambitions || [];
    let drh = '';
    if (dreamText) drh += '<div class="st-stage-ev">💤 昨夜之梦：' + stStageVal(dreamText) + '</div>';
    if (!episodes.length && !dreamText && !ambs.length) drh = '<div class="st-stage-empty">暂无夜梦/碎屑 —— 跨夜后生成梦境，episode 记录日常</div>';
    else {
      episodes.slice(-3).forEach(function (e) { drh += '<div class="st-stage-ev">[' + stStageVal(e.kind) + '] ' + stStageVal(e.content) + '</div>'; });
      ambs.forEach(function (a) {
        const linked = objectives.filter(function(o){return o.owner===a.character && o.status!=='abandoned';});
        const pct = linked.length ? Math.round(linked.reduce(function(s,o){const d=(o.tasks||[]).filter(function(x){return x.completed;}).length;const tt=(o.tasks||[]).length||1;return s+d/tt*100;},0)/linked.length) : 0;
        const stage = pct>=100?'achieved':pct>=75?'nearly there':pct>=50?'halfway there':pct>=25?'gaining ground':'just beginning';
        drh += '<div class="st-stage-ev">野心：' + stStageVal(a.character) + ' — ' + stStageVal(a.text) + ' <span class="st-stage-chip">' + stage + '</span></div>';
      });
    }
    drh += '<div class="st-stage-acts"><button type="button" data-stage-act="episode-add" data-sid="' + encodeURIComponent((tavernSession && tavernSession.sessionId) || '') + '">＋ 记录碎屑</button></div>';
    out += stStageSec('💤 夜梦 / 日常碎屑', (episodes.length||0) + ' 碎屑', drh);
    // Journal 卡片
    try {
      const jr = await stStageFetch('/journals').catch(function(){return {journals:[]};});
      let jcards = jr.journals || jr.journal_cards || jr.cards || [];
      if (!Array.isArray(jcards)) jcards = [];
      let jh = '';
      if (!jcards.length) jh = '<div class="st-stage-empty">暂无 Journal 卡片 —— POST /journals 新建（热卡常驻，冷卡召回）</div>';
      else jcards.slice(-10).forEach(function(c){
        jh += '<div class="st-stage-row"><b>' + stStageVal(c.content ? c.content.slice(0,24) : c.id) + '</b><span class="st-stage-chip">heat ' + stStageVal(c.heat != null ? Number(c.heat).toFixed(2) : '') + '</span>' + (c.pinned ? '<span class="st-stage-chip">📌 pinned</span>' : '') + (c.emotion_label ? '<span class="st-stage-chip">' + stStageVal(c.emotion_label) + '</span>' : '') + '<button type="button" class="sm" data-jpin="' + stStageVal(c.id||'') + '">' + (c.pinned?'unpin':'pin') + '</button><button type="button" class="ghost sm" data-jdel="' + stStageVal(c.id||'') + '">删</button></div>';
        if (c.content) jh += '<div class="st-stage-ev">' + stStageVal(c.content) + '</div>';
      });
      jh += '<div class="st-stage-acts"><button type="button" data-stage-act="journal-new" data-sid="' + encodeURIComponent((tavernSession && tavernSession.sessionId) || '') + '">＋ 新建 Journal</button></div>';
      out += stStageSec('📓 Journal 卡片', jcards.length + ' 张', jh);
    } catch(e){ out += '<div class="st-stage-err">Journal 读取失败：' + (e.message||e) + '</div>'; }
    // 羁绊活数值
    try {
      const rr = await stStageFetch('/relationships').catch(function(){return {relationships:{}};});
      const rels = rr.relationships || rr || {};
      const rids = Object.keys(rels);
      let rh = '';
      if (!rids.length) rh = '<div class="st-stage-empty">暂无羁绊 —— PUT /relationships 写入 bond/trust</div>';
      else rids.forEach(function(cid){
        const b = rels[cid]||{};
        rh += '<div class="st-stage-row"><b>' + stStageVal(cid) + '</b><span>bond ' + stStageVal(b.score!=null?b.score:0) + '</span><span>trust ' + stStageVal(b.trust!=null?b.trust:0) + '</span>' + (b.fixation?'<span class="st-stage-chip">'+stStageVal(b.fixation)+'</span>':'') + (b.spatial_stance?'<span class="st-stage-chip">'+stStageVal(b.spatial_stance)+'</span>':'') + '</div>';
      });
      rh += '<div class="st-stage-acts"><button type="button" data-stage-act="rel-tick" data-sid="' + encodeURIComponent((tavernSession && tavernSession.sessionId) || '') + '">⏳ 羁绊衰减</button></div>';
      out += stStageSec('💞 羁绊', rids.length + ' 人', rh);
    } catch(e){ out += '<div class="st-stage-err">羁绊读取失败：' + (e.message||e) + '</div>'; }
    // Our Story 时间线聚合
    try {
      const sl = await stStageFetch('/storyline').catch(function(){return {storyline:[]};});
      const items = sl.storyline || sl.items || [];
      let sh = '';
      if (!items.length) sh = '<div class="st-stage-empty">暂无时间线 —— 里程碑/Journal/年轮/目标完成后聚合</div>';
      else items.slice(-20).forEach(function(it){
        sh += '<div class="st-stage-row"><span class="st-stage-chip">' + stStageVal(it.kind||'') + '</span><span>' + stStageVal(it.text||'') + '</span><span class="st-stage-chip">turn ' + stStageVal(it.turn!=null?it.turn:'') + '</span>' + (it.receipts && it.receipts.length ? '<span class="st-stage-chip">🧾' + it.receipts.length + '</span>' : '') + '</div>';
      });
      out += stStageSec('📜 Our Story', items.length + ' 条', sh);
    } catch(e){ out += '<div class="st-stage-err">时间线读取失败：' + (e.message||e) + '</div>'; }
    // 心情/在场
    try {
      const [moodRes, presRes] = await Promise.all([
        stStageFetch('/mood').catch(function(){return {mood:{}};}),
        stStageFetch('/presence').catch(function(){return {presence:{}};}),
      ]);
      const mood = moodRes.mood || {};
      const pres = presRes.presence || {};
      const pids2 = Object.keys(pres);
      let mh = '';
      if (mood.summary) mh += '<div class="st-stage-ev">😶 ' + stStageVal(mood.summary) + '</div>';
      else mh += '<div class="st-stage-empty">心情平稳 —— needs/时间/天气无显著倾斜</div>';
      if (pids2.length) pids2.forEach(function(cid){
        const pr = pres[cid]||{};
        mh += '<div class="st-stage-row"><b>' + stStageVal(cid) + '</b><span>' + stStageVal(pr.occupation||'') + ' ' + stStageVal(pr.hours||'') + '</span></div>';
      });
      mh += '<div class="st-stage-acts"><button type="button" data-stage-act="presence-edit" data-sid="' + encodeURIComponent((tavernSession && tavernSession.sessionId) || '') + '">✏️ 在场设置</button></div>';
      out += stStageSec('😶 心情 / 🏢 在场', (mood.offset||0) + ' · ' + pids2.length + ' 人', mh);
    } catch(e){ out += '<div class="st-stage-err">心情读取失败：' + (e.message||e) + '</div>'; }
    // 世界书定时 sticky/cooldown pill
    try {
      const tw = await stStageFetch('/timed-world-info').catch(function(){return {sticky:[],cooldown:[]};});
      const st2 = tw.sticky || [];
      const cd2 = tw.cooldown || [];
      let th = '';
      if (!st2.length && !cd2.length) th = '<div class="st-stage-empty">无长效世界书 —— sticky/cooldown 条目触发后常驻多条</div>';
      else {
        st2.forEach(function(e){ th += '<div class="st-stage-row"><span class="st-stage-chip">sticky 剩' + stStageVal(e.remaining) + '条</span><span>' + stStageVal(e.key||'') + '</span></div>'; });
        cd2.forEach(function(e){ th += '<div class="st-stage-row"><span class="st-stage-chip">cooldown 剩' + stStageVal(e.remaining) + '条</span><span>' + stStageVal(e.key||'') + '</span></div>'; });
      }
      out += stStageSec('📖 世界书定时', st2.length + ' 长效', th);
    } catch(e){ out += '<div class="st-stage-err">定时读取失败：' + (e.message||e) + '</div>'; }
    // 承诺债务
    try {
      const pr = await stStageFetch('/promises').catch(function(){return {promises:[]};});
      const plist = pr.promises || pr || [];
      const arr = Array.isArray(plist) ? plist : [];
      const open = arr.filter(function(p){ return (p.status||'open')==='open'; });
      let ph2 = '';
      if (!open.length) ph2 = '<div class="st-stage-empty">暂无未竟承诺</div>';
      else open.slice(-5).forEach(function(p){
        ph2 += '<div class="st-stage-row"><b>' + stStageVal(p.character||'') + '</b><span>' + stStageVal(p.text||'') + '</span><span class="st-stage-chip">' + stStageVal(p.party||'') + '</span></div>';
      });
      ph2 += '<div class="st-stage-acts"><button type="button" data-stage-act="promise-new" data-sid="' + encodeURIComponent((stCurrentSession() || {}).sid || '') + '">＋ 新承诺</button></div>';
      out += stStageSec('🤝 承诺', open.length + ' 未竟', ph2);
    } catch(e){ out += '<div class="st-stage-err">承诺读取失败：' + (e.message||e) + '</div>'; }
  } catch (e) { out += stStageSec('🎲 / 🏅 / 🎯 / 💤 P4', '', '<div class="st-stage-err">读取失败：' + (e.message || e) + '</div>'); }
  body.innerHTML = out;
  // event_extract toggle
  (function () {
    const eb = document.getElementById('st-event-extract');
    if (eb) eb.onchange = async function () {
      const v = eb.checked;
      eb.disabled = true;
      try {
        await stApi('/sessions/' + encodeURIComponent(tavernSession.sessionId) + '/event-extract', { method: 'PUT', body: JSON.stringify({ eventExtract: v }) });
        stStatus(v ? '全自动事件提取已开启' : '全自动事件提取已关闭');
        await stStageRender();
      } catch (e) { stStatus('切换失败：' + (e.message || e)); eb.checked = !v; } finally { eb.disabled = false; }
    };
  })();
  // pockets_enabled toggle (must wire after innerHTML — element lives inside out)
  (function () {
    const cb = document.getElementById('st-pockets-enabled');
    if (!cb) return;
    cb.onchange = async function () {
      const v = cb.checked;
      cb.disabled = true;
      try {
        await stApi('/sessions/' + encodeURIComponent(tavernSession.sessionId) + '/pockets-enabled', { method: 'PUT', body: JSON.stringify({ pocketsEnabled: v }) });
        stStatus(v ? '口袋提示词注入已开启' : '口袋提示词注入已关闭（数据保留）');
        await stStageRender();
      } catch (e) { stStatus('切换失败：' + (e.message || e)); cb.checked = !v; } finally { cb.disabled = false; }
    };
  })();
  body.querySelectorAll('[data-jpin]').forEach(function (btn) {
    btn.onclick = async function () {
      const cid2 = btn.getAttribute('data-jpin');
      const sid2 = (tavernSession && tavernSession.sessionId) || '';
      if (!cid2 || !sid2) return;
      try {
        const cur = btn.textContent.trim() === 'pin';
        await stApi('/sessions/' + encodeURIComponent(sid2) + '/journals/' + encodeURIComponent(cid2) + '/pin', { method: 'POST', body: JSON.stringify({ pinned: cur }) });
        await stStageRender();
      } catch (e) { stStatus('pin失败：' + (e.message || e)); }
    };
  });
  body.querySelectorAll('[data-jdel]').forEach(function (btn) {
    btn.onclick = async function () {
      const cid2 = btn.getAttribute('data-jdel');
      const sid2 = (tavernSession && tavernSession.sessionId) || '';
      if (!cid2 || !sid2) return;
      if (!confirm('删除该 Journal 卡？')) return;
      try {
        await stApi('/sessions/' + encodeURIComponent(sid2) + '/journals/' + encodeURIComponent(cid2), { method: 'DELETE' });
        await stStageRender();
      } catch (e) { stStatus('删除失败：' + (e.message || e)); }
    };
  });
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
          await stApi('/sessions/' + ((stCurrentSession() || {}).sid || '') + '/actor-states', {
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
    if (act === 'chaos-toggle') {
      const cur = await stStageFetch('/chaos').catch(function(){return {chaos:{enabled:false}};});
      const en = !(cur.chaos && cur.chaos.enabled);
      await stApi('/sessions/' + sid + '/chaos', { method: 'PUT', body: JSON.stringify({ enabled: en }) });
      stStatus(en?'Chaos 已开启':'Chaos 已关闭'); await stStageRender(); return;
    }
    if (act === 'chaos-tick') { await stApi('/sessions/' + sid + '/chaos/tick', { method: 'POST', body: '{}' }); stStatus('Chaos 压力已推进'); await stStageRender(); return; }
    if (act === 'obj-new') {
      const title = prompt('目标标题:', ''); if (!title) return;
      const tasks = (prompt('任务逗号分隔，可留空:', '')||'').split(',').map(function(s){return s.trim();}).filter(Boolean);
      await stApi('/sessions/' + sid + '/objectives', { method: 'POST', body: JSON.stringify({ title: title.trim(), tasks: tasks, owner: '' }) });
      stStatus('目标已创建'); await stStageRender(); return;
    }
    if (act === 'episode-add') {
      const content = prompt('碎屑内容:', ''); if (!content) return;
      const kind = prompt('kind (work/social/wander):', 'work') || 'work';
      await stApi('/sessions/' + sid + '/episodes', { method: 'POST', body: JSON.stringify({ kind: kind.trim(), content: content.trim() }) });
      stStatus('碎屑已记录'); await stStageRender(); return;
    }
    if (act === 'journal-new') {
      const content = prompt('Journal 内容:', ''); if (!content) return;
      const cid = prompt('characterId (留空取首角色):', '') || '';
      const characterId = cid.trim() || (tavernSession && tavernSession.present_character_ids && tavernSession.present_character_ids[0]) || 'c1';
      await stApi('/sessions/' + sid + '/journals', { method: 'POST', body: JSON.stringify({ characterId: characterId, content: content.trim(), category: 'memory' }) });
      stStatus('Journal 已创建'); await stStageRender(); return;
    }
    if (act === 'rel-tick') { await stApi('/sessions/' + sid + '/relationships/tick', { method: 'POST', body: '{}' }); stStatus('羁绊已推进衰减'); await stStageRender(); return; }
    if (act === 'promise-new') {
      const text = prompt('承诺内容:', ''); if (!text) return;
      const character = prompt('characterId (留空=旁白):', '') || '';
      await stApi('/sessions/' + sid + '/promises', { method: 'POST', body: JSON.stringify({ character: character.trim(), text: text.trim(), party: 'char' }) });
      stStatus('承诺已记录'); await stStageRender(); return;
    }
    if (act === 'presence-edit') {
      const cid = prompt('characterId:', '') || ''; if (!cid.trim()) return;
      const occ = prompt('occupation (职业，留空清空):', '') || '';
      const hours = prompt('hours 如 9am-5pm (留空清空):', '') || '';
      await stApi('/sessions/' + sid + '/presence', { method: 'PUT', body: JSON.stringify({ characterId: cid.trim(), occupation: occ.trim(), hours: hours.trim() }) });
      stStatus('在场已更新'); await stStageRender(); return;
    }
    if (act === 'needs-tick') {
      await stApi('/sessions/' + sid + '/needs/tick', { method: 'POST', body: '{}' });
      stStatus('Needs 已推进一回合衰减');
      await stStageRender(); return;
    }
    if (act === 'climate-edit') {
      const atmo = prompt('atmosphere (breathable/thin/unbreathable/hostile):', 'breathable');
      if (atmo == null) return;
      const grav = prompt('gravity (earth/low/high/micro):', 'earth');
      const tb = prompt('temp_band (auto/cold/temperate/hot) 留空=auto:', '');
      await stApi('/sessions/' + sid + '/world-climate', { method: 'PUT', body: JSON.stringify({ atmosphere: atmo.trim() || 'breathable', gravity: (grav||'earth').trim()||'earth', temp_band: (tb||'').trim()||null }) });
      stStatus('世界气候已更新'); await stStageRender(); return;
    }
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
    stNavFrom = (__curTab() === 'packs' && packDetail && !packDetail.classList.contains('hidden')) ? 'packs-detail' : '';
  }
  setStHistoryExpanded(false); // S8.25: collapse on each enter
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

const ST_EMOTION_EMOJI = {
  '平静': '', '开心': '😊', '愤怒': '😠', '悲伤': '😢', '害羞': '😳',
  '惊讶': '😲', '恐惧': '😨', '厌恶': '😒', '疲惫': '😪', '心动': '💗',
};

const ST_SPEAKER_RE = /^[【\[]?([^：:\n]{1,12})[】\]]?[：:]/;

function stSpeakerNameOf(m) {
  const c = (m && m.content) ? String(m.content) : '';
  const hit = c.match(ST_SPEAKER_RE);
  return hit ? hit[1].trim() : '';
}

function stCharIdOf(name) {
  if (!name) return '';
  const chars = (tavernPack && Array.isArray(tavernPack.characters)) ? tavernPack.characters : [];
  for (let i = 0; i < chars.length; i++) {
    if (String(chars[i].name || '').trim() === name) return chars[i].id;
  }
  return name;
}

function stEmotionOf(m) {
  if (!m || !tavernSession || !tavernSession.actorStates) return '';
  const actors = tavernSession.actorStates.actors || {};
  const cid = stCharIdOf(stSpeakerNameOf(m));
  if (cid) {
    const ent = actors[cid];
    if (ent && ent.fields) {
      const emo = ent.fields.emotion;
      const v = (emo && typeof emo === 'object' && 'value' in emo) ? emo.value : emo;
      if (v !== undefined && v !== null && String(v).trim()) return String(v).trim();
    }
  }
  // 兜底：无发言人前缀时，若仅一个角色有情绪字段则用之（单角色对话）
  let fallback = '';
  for (const k in actors) {
    const ent = actors[k];
    if (!ent || !ent.fields) continue;
    const emo = ent.fields.emotion;
    const v = (emo && typeof emo === 'object' && 'value' in emo) ? emo.value : emo;
    if (v === undefined || v === null || !String(v).trim()) continue;
    if (fallback && fallback !== String(v).trim()) return ''; // 多角色都带情绪且名字对不上 → 静默
    fallback = String(v).trim();
  }
  return fallback;
}

function stEmotionEmojiOf(m) {
  return ST_EMOTION_EMOJI[stEmotionOf(m)] || '';
}

function stSpriteOf(roleName) {
  if (!roleName) return null;
  const pack = tavernPack;
  if (!pack || !Array.isArray(pack.characters)) return null;
  const cid = stCharIdOf(roleName);
  const ch = pack.characters.find((c) => c && String(c.id) === String(cid));
  if (!ch) return null;
  const emo = stEmotionOf({ content: roleName + '：→' }); // 以发言人名为传递伪消息
  const exp = (ch.expressions && typeof ch.expressions === 'object') ? ch.expressions : {};
  if (emo && exp[emo]) return String(exp[emo]);
  return (ch.avatar && String(ch.avatar).trim()) ? String(ch.avatar) : null;
}

function stLastUserText() {
  const msgs = (tavernSession && tavernSession.messages) || [];
  for (let i = msgs.length - 1; i >= 0; i--) {
    if (msgs[i] && msgs[i].role === 'user' && (msgs[i].content || '').trim()) return msgs[i].content.trim();
  }
  return '';
}

function stScoreEvents(events, queryText, queryEmb) {
  const charIds = new Set((tavernSession && tavernSession.presentCharacterIds) || []);
  const curNode = (tavernSession && tavernSession.nodeId) || '';
  const turn = (tavernSession && tavernSession.turn) || 0;
  return (events || []).map((e) => {
    let score = 0;
    if (e.nodeId && e.nodeId === curNode) score += 3;
    for (const a of (e.actors || [])) if (charIds.has(a)) score += 2;
    if ((e.turn || 0) >= turn - 3) score += 1;
    let semantic = 0;
    let mode = 'token';
    if (queryEmb && e.embedding && e.embedding.length) {
      semantic = stCosine(e.embedding, queryEmb);
      mode = 'bge';
      score = score * 0.6 + semantic * 3 * 0.4;
    } else if (queryText) {
      semantic = stTokenOverlap(queryText, (e.kind || '') + ' ' + (e.summary || ''));
      score = score * 0.7 + semantic * 2;
      mode = 'token';
    }
    return { e, score, semantic, mode };
  }).sort((a, b) => b.score - a.score);
}

function stRenderRecallBar(opts) {
  const bar = $('st-recall-bar');
  const list = $('st-recall-list');
  const meta = $('st-recall-meta');
  if (!bar || !list) return;
  // 语义记忆召回条仅「播放视图」显示;进入页/列表页保持隐藏(移动端沉浸播放由 CSS 隐藏)
  const play = $('st-view-play');
  const playing = !!(play && !play.classList.contains('hidden'));
  if (!playing || !tavernSession) {
    bar.classList.add('hidden');
    return;
  }
  const events = (((tavernSession.memoryL2 || tavernSession.memory_l2) || {}).events) || [];
  const withEmb = events.filter((e) => e.embedding && e.embedding.length).length;
  if (!events.length) {
    bar.classList.remove('hidden');
    list.innerHTML = '<div class="st-recall-empty muted sm">尚无 L2 事件 — 多聊几轮后会写入语义缓存</div>';
    if (meta) meta.textContent = '0 事件';
    return;
  }
  bar.classList.remove('hidden');
  const queryText = (opts && opts.queryText) || stLastUserText();
  const queryEmb = opts && opts.queryEmb;
  const ranked = stScoreEvents(events, queryText, queryEmb).slice(0, 6);
  if (meta) {
    meta.textContent =
      events.length + ' 事件 · ' + withEmb + ' 已嵌入' +
      (queryEmb ? ' · BGE 重排' : (queryText ? ' · 词面/结构' : ' · 结构排序')) +
      (queryText ? ' · q「' + String(queryText).slice(0, 18) + (queryText.length > 18 ? '…' : '') + '」' : '');
  }
  list.innerHTML = ranked.map((row, i) => {
    const e = row.e;
    const actors = (e.actors || []).slice(0, 3).join(', ');
    const badge = row.mode === 'bge'
      ? ('cos ' + row.semantic.toFixed(2))
      : (row.mode === 'token' ? ('tok ' + row.semantic.toFixed(2)) : ('sc ' + row.score.toFixed(1)));
    return (
      '<div class="st-recall-item" title="' + String(e.summary || '').replace(/"/g, '&quot;') + '">' +
        '<span class="st-recall-rank">#' + (i + 1) + '</span>' +
        '<span class="st-recall-body">' +
          '<span class="st-recall-kind">' + (e.kind || 'event') + '</span>' +
          '<span class="st-recall-sum">' + String(e.summary || '').slice(0, 72) + (String(e.summary || '').length > 72 ? '…' : '') + '</span>' +
          '<span class="st-recall-sub muted sm">t' + (e.turn || '?') + (actors ? ' · ' + actors : '') + (e.nodeId ? ' · ' + e.nodeId : '') + '</span>' +
        '</span>' +
        '<span class="st-recall-score">' + badge + '</span>' +
      '</div>'
    );
  }).join('');
}

async function stRefreshRecallSemantic() {
  if (!tavernSession) return;
  const q = stLastUserText();
  if (!q) {
    stRenderRecallBar();
    if ($('st-recall-meta')) $('st-recall-meta').textContent = ((tavernSession.memoryL2 || {}).events || []).length + ' 事件 · 无用户句可查询';
    return;
  }
  if ($('st-recall-refresh')) $('st-recall-refresh').disabled = true;
  try {
    const data = await api('/api/v1/embeddings', {
      method: 'POST',
      body: JSON.stringify({ input: q, model: 'BAAI/bge-small-zh-v1.5' }),
    });
    const emb = (((data || {}).data || [])[0] || {}).embedding || [];
    stRenderRecallBar({ queryText: q, queryEmb: emb.length ? emb : null });
  } catch (e) {
    stRenderRecallBar({ queryText: q });
    if ($('st-recall-meta')) {
      const cur = $('st-recall-meta').textContent || '';
      $('st-recall-meta').textContent = cur + ' · embed失败 ' + (e.message || e);
    }
  } finally {
    if ($('st-recall-refresh')) $('st-recall-refresh').disabled = false;
  }
}

function stRenderMessages(opts) {
  const el = $('st-messages'); if (!el) return;
  opts = opts || {};
  const list = (tavernSession && tavernSession.messages) || [];
  if (!tavernSession || !list.length) {
    el.innerHTML = stEmpty('没有出现对话', tavernSession ? '正在准备开场白…若仍为空可点下方输入或重进会话' : '选择剧本包与玩法，开始一场新的叙事');
    el.scrollTop = 0;
    return;
  }

  function roleClassOf(m) {
    return m.role === 'user' ? 'user' : (m.role === 'narrator' ? 'narrator' : 'agent');
  }
  function roleLabelOf(m) {
    return m.role === 'user' ? '你' : (m.role === 'narrator' ? '旁白' : '故事');
  }
  function bodyOf(m) {
    // Always strip option protocol from bubbles (stream + final). Chips own the choices.
    if (m && (m.kind === 'continue' || (m.role === 'user' && !(String(m.content || '').trim())))) {
      return (m.content && String(m.content).trim()) ? m.content : '（续写）';
    }
    const raw = (m.role === 'user') ? (m.content || '') : stripChoicesBlock(m.content || '');
    // 流式阶段程序卡原文先不显示（闪烁防抖）：最终保存后 program 字段渲染 iframe
    if (opts.stream && m.role !== 'user') {
      return String(raw).replace(/【程序】[\s\S]*?【\/程序】/g, '').trim() || raw;
    }
    const body = applyStRegexScripts(raw, m.role);
    // 纯询问回合（【询问】停笔卡）：无正文但有选项 → 占位卡文本（吸收自梨园 ask_director）
    if (!String(body).trim() && m.role !== 'user' && Array.isArray(m.options) && m.options.length) {
      return '（请选择后续走向）';
    }
    return body;
  }
  function extraClassOf(m) {
    if (m && (m.kind === 'continue' || (m.role === 'user' && !(String(m.content || '').trim())))) return 'is-continue';
    return '';
  }

  // S8.25: fold older than last ST_VISIBLE_TURNS 对话 (user-started rounds)
  const fold = stMessageFoldPlan(list);
  const startIdx = (!stHistoryExpanded && fold.foldUntil > 0) ? fold.foldUntil : 0;

  const streamTail = !!(opts.stream && list.length && list[list.length - 1] && list[list.length - 1].role !== 'user');
  if (streamTail) {
    const last = list[list.length - 1];
    const mid = last.id || ('st-idx-' + (list.length - 1));
    let node = el.querySelector('.bubble[data-mid="' + cssEscape(mid) + '"]');
    if (!node) {
      // append missing bubbles only within visible window (don't re-inflate folded history mid-stream)
      const existing = new Set();
      el.querySelectorAll('.bubble[data-mid]').forEach(function (n) {
        existing.add(n.getAttribute('data-mid'));
      });
      if (el.querySelector('.st-empty')) el.innerHTML = '';
      stEnsureFoldBanner(el, fold);
      for (let i = startIdx; i < list.length; i++) {
        const m = list[i];
        const id = m.id || ('st-idx-' + i);
        if (existing.has(id)) continue;
        const isStream = id === mid;
        const div = buildBubbleEl({
          id: id,
          roleClass: 'st-bubble ' + roleClassOf(m),
          roleLabel: roleLabelOf(m),
          body: bodyOf(m),
          enter: !isStream,
          streaming: isStream,
          extraClass: extraClassOf(m),
          program: m.program || null,
          emotionEmoji: stEmotionEmojiOf(m),
          swipeSupport: m.role !== 'user',
          swipeCount: (m._swipes && m._swipes.length) || 1,
          swipeIdx: (typeof m._swipeIdx === 'number') ? m._swipeIdx : 0,
          ts: m.createdAt || m.ts || '',
          tokens: (m.tokens || (m.usage && m.usage.total_tokens)) || 0,
          monologue: (m.role !== 'user' && (m._monologue || m.reasoning)) ? (m._monologue || m.reasoning) : null,
        });
        el.appendChild(div);
      }
      node = el.querySelector('.bubble[data-mid="' + cssEscape(mid) + '"]');
    }
    if (node) {
      const body = node.querySelector('.bubble-body');
      const text = bodyOf(last);
      if (body) {
        // Typewriter: only append new chars in stream mode instead of full replace
        if (opts.stream && body.getAttribute('data-stream-base') && !body.hasAttribute('data-stream-final')) {
          const cur = body.textContent || '';
          // Only append if text is longer (growing)
          if (text.length > cur.length) {
            const diff = text.slice(cur.length);
            // Append as text node to preserve existing node structure
            body.appendChild(document.createTextNode(diff));
          } else if (text.length < cur.length) {
            // Text shrank (mid-stream correction) — replace fully
            body.textContent = text;
          }
        } else {
          fillBubbleBody(body, text);
          if (opts.stream) body.setAttribute('data-stream-base', '1');
        }
      } else {
        const span = document.createElement('span');
        span.className = 'bubble-body';
        fillBubbleBody(span, text);
        if (opts.stream) span.setAttribute('data-stream-base', '1');
        node.appendChild(span);
      }
      node.classList.add('is-streaming');
      // S8.31: 流式期间不自动跟随滚动——视口停在开头（用户从开头下滑阅读）；
      // 用户手动滚动不受影响。移除原 nearBottom 跟随。
      // P2-1 立绘层：流式每帧同步当前焦点角色立绘（无情绪/无立绘静默降级）
      try { stRenderSprite(); } catch (_) {}
      return;
    }
  }

  const stick = el.scrollHeight - el.scrollTop - el.clientHeight < 96;
  const prevTop = el.scrollTop;
  el.innerHTML = '';
  stEnsureFoldBanner(el, fold);
  for (let i = startIdx; i < list.length; i++) {
    const m = list[i];
    const id = m.id || ('st-idx-' + i);
    const isLastStream = !!(opts.stream && i === list.length - 1 && m.role !== 'user');
    el.appendChild(buildBubbleEl({
      id: id,
      roleClass: 'st-bubble ' + roleClassOf(m),
      roleLabel: roleLabelOf(m),
      body: bodyOf(m),
      enter: !isLastStream && !opts.quiet,
      streaming: isLastStream,
      extraClass: extraClassOf(m),
      program: m.program || null,
      emotionEmoji: stEmotionEmojiOf(m),
      swipeSupport: m.role !== 'user',
      swipeCount: (m._swipes && m._swipes.length) || 1,
      swipeIdx: (typeof m._swipeIdx === 'number') ? m._swipeIdx : 0,
      ts: m.createdAt || m.ts || '',
      tokens: (m.tokens || (m.usage && m.usage.total_tokens)) || 0,
      monologue: (m.role !== 'user' && (m._monologue || m.reasoning)) ? (m._monologue || m.reasoning) : null,
    }));
  }
  if (opts.restoreScroll) {
    window.__stProgrammaticScroll = true;
    stRestoreReadPos(el);
    window.setTimeout(function () { window.__stProgrammaticScroll = false; }, 50);
  } else if (stick || opts.forceScroll) {
    window.__stProgrammaticScroll = true;
    el.scrollTop = el.scrollHeight;
    window.setTimeout(function () { window.__stProgrammaticScroll = false; }, 50);
  } else {
    el.scrollTop = prevTop;
  }
  // Programmatic scrolls (forceScroll/stick/restoreScroll) must NOT hide the
  // chrome: hiding on load leaves no input box and stray taps hit option
  // buttons. Only user-initiated scroll events hide the chrome.
  if (!opts.forceScroll && !opts.stream && !opts.restoreScroll) {
    try { stSyncImmChromeFromScroll(); } catch (_) {}
  }
  try { stBindReadPosSaver(el); } catch (_) {}
  // P2-1 立绘层：消息区重渲染后同步当前焦点角色立绘（无立绘/无情绪静默降级）
  try { stRenderSprite(); } catch (_) {}
}

function stMessageFoldPlan(list) {
  const turnStarts = [];
  for (let i = 0; i < list.length; i++) {
    if (i === 0 || (list[i] && list[i].role === 'user')) turnStarts.push(i);
  }
  const n = turnStarts.length;
  if (n <= ST_VISIBLE_TURNS) {
    return { foldUntil: 0, hiddenTurns: 0, hiddenMsgs: 0, totalTurns: n };
  }
  const foldUntil = turnStarts[n - ST_VISIBLE_TURNS];
  return {
    foldUntil: foldUntil,
    hiddenTurns: n - ST_VISIBLE_TURNS,
    hiddenMsgs: foldUntil,
    totalTurns: n,
  };
}

function stEnsureFoldBanner(el, fold) {
  if (!el || !fold) return;
  const existing = el.querySelector('.st-history-fold');
  if (stHistoryExpanded) {
    if (fold.hiddenTurns > 0) {
      // show collapse control at top
      const bar = existing || document.createElement('button');
      bar.type = 'button';
      bar.className = 'st-history-fold st-history-fold-collapse';
      bar.textContent = '收起较早对话（只留最近 ' + ST_VISIBLE_TURNS + ' 轮）';
      bar.onclick = function (e) {
        e.preventDefault();
        setStHistoryExpanded(false);
        stRenderMessages({ restoreScroll: false, forceScroll: false, quiet: true });
        // after collapse, jump to bottom of visible window
        const box = $('st-messages');
        if (box) box.scrollTop = box.scrollHeight;
        try { stSyncImmChromeFromScroll(true); } catch (_) {}
      };
      if (!existing) el.insertBefore(bar, el.firstChild);
    } else if (existing) {
      existing.remove();
    }
    return;
  }
  if (fold.foldUntil <= 0) {
    if (existing) existing.remove();
    return;
  }
  const bar = existing || document.createElement('button');
  bar.type = 'button';
  bar.className = 'st-history-fold';
  bar.textContent = '较早对话已折叠 · ' + fold.hiddenTurns + ' 轮 / ' + fold.hiddenMsgs + ' 条 · 点击展开';
  bar.onclick = function (e) {
    e.preventDefault();
    setStHistoryExpanded(true);
    const box = $('st-messages');
    const keep = box ? box.scrollHeight - box.scrollTop : 0;
    stRenderMessages({ quiet: true });
    // keep viewport anchored near where user was (bottom of previous visible set)
    if (box) {
      box.scrollTop = Math.max(0, box.scrollHeight - keep);
    }
    try { stSyncImmChromeFromScroll(true); } catch (_) {}
  };
  if (!existing) el.insertBefore(bar, el.firstChild);
  else if (bar.parentElement !== el) el.insertBefore(bar, el.firstChild);
}

function stReadPosKey(sid) {
  return ST_READPOS_PREFIX + String(sid || '');
}

function stSaveReadPos() {
  try {
    if (!tavernSession || !tavernSession.sessionId) return;
    const el = $('st-messages');
    if (!el) return;
    const max = Math.max(0, el.scrollHeight - el.clientHeight);
    const gap = el.scrollHeight - el.scrollTop - el.clientHeight;
    const nearBot = gap <= 96;
    const payload = {
      top: Math.round(el.scrollTop),
      ratio: max > 0 ? el.scrollTop / max : 1,
      nearBot: nearBot,
      at: Date.now(),
    };
    localStorage.setItem(stReadPosKey(tavernSession.sessionId), JSON.stringify(payload));
  } catch (_) {}
}

function stRestoreReadPos(el) {
  el = el || $('st-messages');
  if (!el || !tavernSession || !tavernSession.sessionId) return;
  let raw = null;
  try { raw = localStorage.getItem(stReadPosKey(tavernSession.sessionId)); } catch (_) {}
  const apply = function () {
    try {
      if (!raw) {
        // no history → last-read default = end (not top)
        el.scrollTop = el.scrollHeight;
      } else {
        const o = JSON.parse(raw);
        const max = Math.max(0, el.scrollHeight - el.clientHeight);
        if (o && o.nearBot) {
          el.scrollTop = el.scrollHeight;
        } else if (o && typeof o.ratio === 'number' && max > 0) {
          el.scrollTop = Math.min(max, Math.max(0, o.ratio * max));
        } else if (o && typeof o.top === 'number') {
          el.scrollTop = Math.min(max, Math.max(0, o.top));
        } else {
          el.scrollTop = el.scrollHeight;
        }
      }
    } catch (_) {
      el.scrollTop = el.scrollHeight;
    }
    // Do NOT hide chrome after restoring position: that leaves no input box
    // and stray taps hit option buttons. Show chrome so the composer is
    // reachable on entry; scroll-hide only kicks in on user scroll.
    try { stShowImmChrome(); } catch (_) {}
  };
  // layout after fold/render
  requestAnimationFrame(function () { requestAnimationFrame(apply); });
}

function stBindReadPosSaver(el) {
  if (!el || el._stReadPosBound) return;
  let t = 0;
  // Mark user-initiated scrolls (touch drag / wheel / keyboard). Programmatic
  // scrolls (restore/forceScroll/render jump) do NOT mark → chrome stays
  // visible so the input box and options are reachable on entry.
  const markUser = function () { window.__stUserScrolling = true; };
  el.addEventListener('pointerdown', markUser, { passive: true });
  el.addEventListener('touchstart', markUser, { passive: true });
  el.addEventListener('wheel', markUser, { passive: true });
  el.addEventListener('keydown', markUser, { passive: true });
  el.addEventListener('scroll', function () {
    // S8.28: live scroll drives top-bar hide/show — only for user scrolls.
    if (window.__stUserScrolling) {
      try { stSyncImmChromeFromScroll(); } catch (_) {}
      window.__stUserScrolling = false;
    }
    // S8.31: 记录用户真实滚动（程序化滚动有 __stProgrammaticScroll 标志）
    if (!window.__stProgrammaticScroll) {
      stTavernUserScrolled = true;
    }
    if (t) return;
    t = window.setTimeout(function () {
      t = 0;
      stSaveReadPos();
    }, 180);
  }, { passive: true });
  el._stReadPosBound = true;
}

let stStreamRaf = 0;

function scheduleStStreamPaint() {
  if (stStreamRaf) return;
  stStreamRaf = requestAnimationFrame(function () {
    stStreamRaf = 0;
    if (!tavernStreaming) return; // S8.11e: drop late frames after stop/finally
    stRenderMessages({ stream: true });
    // S8.31: 生成中视口保持在文本开头（用户未手动滚动时）；内容增长后校正
    if (!stTavernUserScrolled) {
      try { stScrollToLastMsgTop(); } catch (_) {}
    }
    // live-extract chips once 【选项】 appears in the streaming tail
    try {
      const msgs = (tavernSession && tavernSession.messages) || [];
      const last = msgs.length ? msgs[msgs.length - 1] : null;
      if (last && last.role !== 'user') {
        const live = resolveMessageOptions(last);
        if (live.length) stRenderOptions(live);
      }
    } catch (_) {}
  });
}

function clearStStreamPaint() {
  if (stStreamRaf) {
    try { cancelAnimationFrame(stStreamRaf); } catch (_) {}
    stStreamRaf = 0;
  }
  const stEl = $('st-messages');
  if (stEl) stEl.querySelectorAll('.bubble.is-streaming').forEach(function (n) { n.classList.remove('is-streaming'); });
  document.documentElement.removeAttribute('data-streaming');
}

function stRenderOptions(opts) {
  const el = $('st-options');
  if (!el) return;
  el.innerHTML = '';
  let source = Array.isArray(opts) ? opts : null;
  if (!source || !source.length) {
    const msgs = (tavernSession && tavernSession.messages) || [];
    let last = null;
    for (let i = msgs.length - 1; i >= 0; i--) {
      if (msgs[i] && msgs[i].role !== 'user') { last = msgs[i]; break; }
    }
    source = resolveMessageOptions(last);
  }
  source = (source || []).map(String).map((s) => s.trim()).filter(Boolean);
  if (!source.length) {
    el.classList.add('is-empty');
    return;
  }
  el.classList.remove('is-empty');
  // ensure play view options row visible
  el.hidden = false;
  el.style.display = '';
  for (const text of source) {
    const chip = document.createElement('button');
    chip.type = 'button';
    chip.className = 'st-option-chip';
    chip.textContent = text;
    chip.onclick = () => {
      if ($('st-input')) $('st-input').value = text;
      stSend(text);
    };
    el.appendChild(chip);
  }
  // S8.23: options can grow dock — keep last lines above it
  try { stKeepImmTailVisible(); } catch (_) {}
}

let stActivePanelName = '';

function stRenderPanels(target) {
  if (!target) {
    const p = $('st-panels');
    if (p) p.innerHTML = '';
    return;
  }
  const el = target;
  if (!el) return;
  el.innerHTML = '';
  const panels = (tavernSession && Array.isArray(tavernSession.panels)) ? tavernSession.panels : [];
  if (!panels.length) {
    el.innerHTML = (target && target.id === 'st-visual-body')
      ? '<div class="st-panels-empty">还没有可视化面板。点「让助手生成可视化」，或在剧情中让模型输出【面板】块。</div>'
      : '<div class="st-panels-empty">AI 可在剧情中生成可视化面板（地图/装备栏/线索板），出现后自动显示于此</div>';
    return;
  }
  if (!stActivePanelName || !panels.some(p => p.name === stActivePanelName)) {
    stActivePanelName = panels[0].name;
  }
  const tabs = document.createElement('div');
  tabs.className = 'st-panels-tabs';
  for (const p of panels) {
    const t = document.createElement('button');
    t.type = 'button';
    t.className = 'st-panel-tab' + (p.name === stActivePanelName ? ' active' : '');
    t.textContent = p.name;
    t.onclick = () => { stActivePanelName = p.name; stRenderPanels(target); };
    tabs.appendChild(t);
  }
  el.appendChild(tabs);
  const cur = panels.find(p => p.name === stActivePanelName) || panels[0];
  const body = document.createElement('div');
  body.className = 'st-panel-body';
  if (cur.kind === 'eventbook') {
    // 事件书（Omate 对齐）：剧情链状态追踪——解锁/完成/条件
    const wrap = document.createElement('div');
    wrap.className = 'st-eventbook';
    let events = [];
    try {
      const parsed = JSON.parse(cur.content);
      events = Array.isArray(parsed) ? parsed : (Array.isArray(parsed.events) ? parsed.events : []);
    } catch (_) {
      // 非 JSON 降级：按行渲染为纯文本事件列表
      events = String(cur.content).split('\n').filter(function (l) { return l.trim(); }).map(function (l) {
        return { title: l.replace(/^[-*]\s*/, '').replace(/^\[[ xX]\]\s*/, '').trim(), done: /^\[[xX]\]/.test(l.trim()) };
      });
    }
    if (!events.length) {
      wrap.innerHTML = '<div class="st-panels-empty">事件书为空——让助手生成事件链，或剧情中输出【事件书】块。</div>';
    } else {
      const list = document.createElement('ol');
      list.className = 'st-eventbook-list';
      for (const ev of events) {
        const li = document.createElement('li');
        li.className = 'st-eventbook-item' + (ev.done ? ' done' : '');
        const mark = document.createElement('span');
        mark.className = 'st-eventbook-mark';
        mark.textContent = ev.done ? '✓' : '○';
        const info = document.createElement('div');
        info.className = 'st-eventbook-info';
        const title = document.createElement('div');
        title.className = 'st-eventbook-title';
        title.textContent = ev.title || '（未命名事件）';
        info.appendChild(title);
        if (ev.desc) {
          const desc = document.createElement('div');
          desc.className = 'st-eventbook-desc';
          desc.textContent = ev.desc;
          info.appendChild(desc);
        }
        if (ev.cond && !ev.done) {
          const cond = document.createElement('div');
          cond.className = 'st-eventbook-cond';
          cond.textContent = '条件：' + ev.cond;
          info.appendChild(cond);
        }
        li.appendChild(mark); li.appendChild(info);
        list.appendChild(li);
      }
      wrap.appendChild(list);
    }
    body.appendChild(wrap);
  } else if (cur.kind === 'svg') {
    const wrap = document.createElement('div');
    wrap.className = 'st-panel-svg';
    // 清洗加固 (audit P1#8)：双/单引号 on* 处理器 + xlink:href + javascript: href 全剥
    const safe = String(cur.content)
      .replace(/<script[\s\S]*?<\/script>/gi, '')
      .replace(/\son\w+\s*=\s*(?:"[^"]*"|'[^']*'|[^\s>]*)/gi, '')
      .replace(/\sxlink:href\s*=\s*(?:"[^"]*"|'[^']*'|[^\s>]*)/gi, '')
      .replace(/\shref\s*=\s*"javascript:[^"]*"/gi, '')
      .replace(/\shref\s*=\s*'javascript:[^']*'/gi, '');
    wrap.innerHTML = safe;
    body.appendChild(wrap);
  } else if (cur.kind === 'html') {
    const frame = document.createElement('iframe');
    // audit P0#1：去掉 allow-same-origin —— 同源会使沙箱内脚本读取父页 token/调用同源 API
    frame.setAttribute('sandbox', 'allow-scripts');
    frame.setAttribute('title', cur.name);
    frame.srcdoc = String(cur.content);
    body.appendChild(frame);
  } else {
    body.textContent = cur.content;
  }
  el.appendChild(body);
}

function stOpenVisualModal() {
  const m = $('st-visual-modal');
  if (!m) return;
  m.classList.remove('hidden');
  stRenderPanels($('st-visual-body'));
}

function stCloseVisualModal() {
  const m = $('st-visual-modal');
  if (m) m.classList.add('hidden');
}

async function stGenVisual() {
  const btn = $('st-visual-gen');
  if (!btn || btn.disabled) return;
  btn.disabled = true;
  const old = btn.textContent;
  btn.textContent = '生成中…';
  try {
    const sid = tavernSession && tavernSession.sessionId;
    if (!sid) { stStatus('无会话'); return; }
    const data = await stApi('/sessions/' + encodeURIComponent(sid) + '/assistant', {
      method: 'POST', body: JSON.stringify({ message: '请生成当前剧情的可视化面板：地图、线索图谱、线索板、装备栏（用【面板】JSON 块输出，kind 用 markdown/svg/html）' })
    });
    // 服务端已把【面板】回写 session.panels，客户端直接刷新
    if (tavernSession) {
      try {
        const s = await stApi('/sessions/' + encodeURIComponent(sid));
        if (s && s.panels) tavernSession.panels = s.panels;
      } catch (_) {}
    }
    stRenderPanels($('st-visual-body'));
    stRenderPanels();
    if (data && data.reply && data.reply.trim()) stStatus(data.reply.trim().slice(0, 80));
  } catch (e) {
    stStatus('可视化生成失败：' + (e && e.message || e));
  } finally {
    btn.disabled = false;
    btn.textContent = old;
  }
}

const ST_ASSIST_KEY_PREFIX = 'kaleido_assist_';

const ST_ASSIST_MAX = 200;

function stAssistHistory(sid) {
  try {
    const raw = localStorage.getItem(ST_ASSIST_KEY_PREFIX + String(sid || ''));
    if (!raw) return [];
    const arr = JSON.parse(raw);
    return Array.isArray(arr) ? arr.slice(-ST_ASSIST_MAX) : [];
  } catch (_) { return []; }
}

function stAssistSave(sid, history) {
  try {
    const arr = (history || []).slice(-ST_ASSIST_MAX);
    localStorage.setItem(ST_ASSIST_KEY_PREFIX + String(sid || ''), JSON.stringify(arr));
  } catch (_) {}
}

function stRenderAssist(history) {
  const body = $('st-assist-body');
  if (!body) return;
  body.innerHTML = '';
  const msgs = (history && history.length) ? history : null;
  if (!msgs) {
    const empty = document.createElement('div');
    empty.className = 'st-assist-msg agent';
    empty.textContent = '问助手：当前剧情状态？线索梳理？（多轮记忆已生效）';
    body.appendChild(empty);
  } else {
    for (const m of msgs) {
      const div = document.createElement('div');
      div.className = 'st-assist-msg ' + (m.role === 'user' ? 'user' : 'agent');
      div.textContent = m.content;
      body.appendChild(div);
    }
  }
  body.scrollTop = body.scrollHeight;
}

function stFocusAssistInput() {
  const inp = $('st-assist-input');
  if (!inp) return;
  inp.focus();
  try { inp.scrollIntoView({ behavior: 'smooth', block: 'nearest' }); } catch (_) {}
}

function stOpenAssistModal() {
  const m = $('st-assist-modal');
  if (!m) return;
  m.classList.remove('hidden');
  const storyMode = __curTab() === 'story' || __curTab() === 'adventure' || __curTab() === 'chat';
  // story/冒险/跑团/对话无 tavern 会话：reroll/rewind 是剧场专属，隐藏工具行
  const toolsRow = document.querySelector('.st-assist-tools');
  if (toolsRow) toolsRow.style.display = storyMode ? 'none' : '';
  const sid = storyMode
    ? (__curTab() === 'chat' ? (__chatState().sessionId || '') : (__storyState().storySessionId || ''))
    : (tavernSession && tavernSession.sessionId);
  stRenderAssist(stAssistHistory(sid));
  stFocusAssistInput();
}

function stCloseAssistModal() {
  const m = $('st-assist-modal');
  if (m) m.classList.add('hidden');
  const inp = $('st-assist-input');
  if (inp) inp.value = '';
}

function storyWbIds() {
  const ids = [];
  const wbSel = __curTab() === 'chat' ? $('chat-wb') : (__curTab() === 'adventure' ? $('adv-wb') : $('story-wb'));
  if (wbSel && wbSel.value) ids.push(wbSel.value);
  const ccSel = __curTab() === 'chat' ? $('chat-cc') : (__curTab() === 'adventure' ? $('adv-cc') : $('story-cc'));
  if (ccSel && ccSel.value && typeof __chatState().partner !== 'undefined' && __chatState().partner.characterCards) {
    const cc = __chatState().partner.characterCards.find((c) => c.id === ccSel.value);
    if (cc && cc.worldBookId) ids.push(cc.worldBookId);
  }
  return ids.filter((v, i, a) => a.indexOf(v) === i);
}

async function stSendAssist() {
  const inp = $('st-assist-input');
  const btn = $('st-assist-send');
  if (!inp) return;
  const text = inp.value.trim();
  if (!text) return;
  const storyMode = __curTab() === 'story' || __curTab() === 'adventure' || __curTab() === 'chat';
  const sid = storyMode
    ? (__curTab() === 'chat' ? (__chatState().sessionId || '') : (__storyState().storySessionId || ''))
    : (tavernSession && tavernSession.sessionId);
  inp.value = '';
  if (btn) { btn.disabled = true; btn.textContent = '…'; }
  const history = stAssistHistory(sid);
  history.push({ role: 'user', content: text });
  stAssistSave(sid, history);
  stRenderAssist(history);
  try {
    if (!sid) throw new Error('当前无会话');
    // 带助手对话历史（剔除刚 push 的最后一条 user——那是本次 message）
    const hist = history.slice(0, -1).map((m) => ({ role: m.role, content: String(m.content || '') }));
    // story/冒险/跑团/对话：上下文来自前端本地消息；tavern：服务端会话注入剧情上下文
    const ctxMessages = __curTab() === 'chat'
      ? (__chatState().messages || [])
      : (__storyState().storyMessages || []);
    const data = storyMode
      ? await api('/api/v1/story/assistant', {
          method: 'POST', body: JSON.stringify({
            message: text,
            history: hist,
            title: '',
            kind: __curTab() === 'chat' ? 'chat' : 'story',
            worldBookIds: storyWbIds(),
            messages: ctxMessages.slice(-10).map((m) => ({ role: m.role, content: String(m.content || '') }))
          })
        })
      : await stApi('/sessions/' + encodeURIComponent(sid) + '/assistant', {
          method: 'POST', body: JSON.stringify({ message: text, history: hist })
        });
    const reply = (data && data.reply)
      ? data.reply
      : ('（无回复：' + ((data && data.error) || '未知错误') + '）');
    history.push({ role: 'agent', content: reply });
  } catch (e) {
    history.push({ role: 'agent', content: '请求失败：' + (e && e.message || e) });
  }
  stAssistSave(sid, history);
  stRenderAssist(history);
  if (btn) { btn.disabled = false; btn.textContent = '发送'; }
  stFocusAssistInput();
}

async function stRerollLast() {
  const sid = tavernSession && tavernSession.sessionId;
  if (!sid) { stStatus('当前无会话'); return; }
  const btn = $('st-assist-reroll');
  if (btn) btn.disabled = true;
  try {
    const data = await stApi('/sessions/' + encodeURIComponent(sid) + '/reroll', { method: 'POST', body: '{}' });
    const text = (data && data.lastUserMessage) ? String(data.lastUserMessage) : '';
    stCloseAssistModal();
    if (text && typeof stSend === 'function') {
      await stSend(text);
      stStatus('已重生成上一条回复');
    } else {
      stStatus('已回退 1 回合（无上一条用户消息可重发）');
    }
  } catch (e) {
    stStatus('重生成失败：' + ((e && e.message) || e));
  } finally {
    if (btn) btn.disabled = false;
  }
}

async function stRewindOne() {
  const sid = tavernSession && tavernSession.sessionId;
  if (!sid) { stStatus('当前无会话'); return; }
  const btn = $('st-assist-rewind');
  if (btn) btn.disabled = true;
  try {
    await stApi('/sessions/' + encodeURIComponent(sid) + '/rewind', { method: 'POST', body: JSON.stringify({ steps: 1 }) });
    await stLoadSession(sid);
    stStatus('已回退 1 回合');
  } catch (e) {
    stStatus('回退失败：' + ((e && e.message) || e));
  } finally {
    if (btn) btn.disabled = false;
  }
}

function stFetch(path, opts = {}) {
  const token = localStorage.getItem('kaleido_token') || '';
  const base = localStorage.getItem('kaleido_api_base') || '';
  return fetch(base + path, {
    method: opts.method || 'POST',
    headers: {
      'Content-Type': 'application/json',
      ...(token ? { 'Authorization': 'Bearer ' + token } : {})
    },
    body: opts.body
  });
}

async function stGenerateImage() {
  if (!tavernSession) { stStatus('无会话'); return; }
  const msgs = tavernSession.messages || [];
  let scene = '';
  for (let i = msgs.length - 1; i >= 0; i--) {
    if (msgs[i].role === 'assistant' && String(msgs[i].content || '').trim()) {
      scene = String(msgs[i].content).trim().slice(0, 120);
      break;
    }
  }
  if (!scene) scene = '当前场景';
  const chSel = $('st-image-channel');
  const channel = chSel ? (chSel.dataset.value || 'uniapi') : 'uniapi';
  stStatus('生图中…');
  const res = await stFetch('/api/v1/kaleido-tools/image', {
    body: JSON.stringify({ prompt: '动漫电影感插画，' + scene, channel })
  });
  if (!res.ok) throw new Error('HTTP ' + res.status);
  const data = await res.json();
  if (data && (data.url || data.b64)) {
    stShowImage(data.url || ('data:image/jpeg;base64,' + data.b64));
    stStatus('插图已生成（' + (data.channel || '') + '）');
  } else {
    throw new Error((data && data.error) || '生图无返回');
  }
}

function stSpriteHintsOf(name) {
  const msgs = (tavernSession && tavernSession.messages) || [];
  const pool = [];
  for (let i = 0; i < msgs.length; i++) {
    const c = String((msgs[i] && msgs[i].content) || '');
    let from = 0, idx;
    while ((idx = c.indexOf(name, from)) >= 0) {
      pool.push(c.slice(idx, idx + 90));
      from = idx + name.length;
    }
  }
  let look = '';
  const lookRe = /(肩膀|肌肉|头发|眼睛|个子|身形|穿着|外套|运动服|衬衫|眼镜|短发|长发|身高|清瘦|结实|少年感|西装|校服)/;
  for (const seg of pool) {
    const m = seg.match(lookRe);
    if (m) { look = seg.replace(/\s+/g, ' ').trim().slice(0, 70); break; }
  }
  return { look };
}

async function stGenerateSprite() {
  const sid = tavernSession && tavernSession.sessionId;
  if (!sid) { stStatus('无会话'); return; }
  // 目标角色：最后发言人优先，回退换壳/在场第一个
  let roleName = '';
  const msgs = (tavernSession.messages) || [];
  for (let i = msgs.length - 1; i >= 0; i--) {
    const m = msgs[i];
    if (m && m.role && m.role !== 'user') { roleName = stSpeakerNameOf(m); if (roleName) break; }
  }
  if (!roleName) {
    const vcid = (tavernSession.entry && tavernSession.entry.vesselCharacterId)
      || (tavernSession.player && tavernSession.player.controlCharacterId) || '';
    const present = (tavernSession.presentCharacterIds || [])[0] || '';
    const fallback = vcid || present;
    const pack = tavernPack;
    const ch = fallback && pack && pack.characters
      ? pack.characters.find((c) => c && String(c.id) === String(fallback)) : null;
    roleName = (ch && ch.name) || '';
    if (!roleName) { stStatus('无可生成目标角色'); return; }
  }
  const cid = stCharIdOf(roleName);
  const pack = tavernPack;
  const ch = pack && pack.characters ? pack.characters.find((c) => c && String(c.id) === String(cid)) : null;
  if (!ch) { stStatus('pack 中无此角色：' + roleName); return; }
  const emotion = stEmotionOf({ content: roleName + '：→' }) || '平静';
  const chSel = $('st-image-channel');
  const channel = chSel ? (chSel.dataset.value || 'uniapi') : 'uniapi';
  stStatus('生成立绘中（' + roleName + '·' + emotion + '），约 10-30s…');
  // P2-1c: 性别/形象——角色卡 gender/appearance 为主（手动蒸馏/蒸馏补全写入），
  // 原文只兜底形象片段；风格不再硬编码「美少女」：卡判女性才美少女，否则中性动漫质感
  const hints = stSpriteHintsOf(roleName);
  const cardGender = (ch.gender && String(ch.gender).trim() && String(ch.gender) !== '未知')
    ? String(ch.gender).trim() : '';
  const cardLook = (ch.appearance && String(ch.appearance).trim() && String(ch.appearance) !== '未知')
    ? String(ch.appearance).trim() : '';
  const lookStr = (cardLook || hints.look) ? '，形象：' + (cardLook || hints.look) : '';
  const isFemale = /女/.test(cardGender);
  const style = isFemale ? '美少女游戏立绘质感' : '写实漫画质感';
  const prompt = '动漫风格半身立绘，' + style + '，竖构图，角色：' + roleName
    + (cardGender ? '，' + cardGender : '') + lookStr
    + '，表情：' + emotion + '，干净纯色背景，人物居中';
  const res = await stFetch('/api/v1/kaleido-tools/image', {
    body: JSON.stringify({ prompt: prompt, channel })
  });
  if (!res.ok) throw new Error('HTTP ' + res.status);
  const data = await res.json();
  const url = (data && (data.url || data.b64)) ? (data.url || ('data:image/jpeg;base64,' + data.b64)) : null;
  if (!url) throw new Error((data && data.error) || '生成立绘无返回');
  // 写回 pack：GET 全量 → 改 expressions[emotion] → POST upsert
  const full = await stApi('/packs/' + encodeURIComponent(pack.id));
  if (!full || !Array.isArray(full.characters)) throw new Error('读 pack 失败');
  const target = full.characters.find((c) => c && String(c.id) === String(cid));
  if (!target) throw new Error('pack 角色缺失');
  if (!target.expressions || typeof target.expressions !== 'object') target.expressions = {};
  target.expressions[emotion] = url;
  if (!target.avatar) target.avatar = url;
  const saved = await stApi('/packs', { method: 'POST', body: JSON.stringify(full) });
  // 同步本地 tavernPack 与全局缓存，再刷立绘
  if (saved && tavernPacks) {
    const idx = tavernPacks.findIndex((p) => p && p.id === pack.id);
    if (idx >= 0) tavernPacks[idx] = saved; else tavernPacks.push(saved);
  }
  tavernPack = saved || full;
  stShowImage(url);
  try { stRenderSprite(); } catch (_) {}
  stStatus('立绘已生成并写入 pack（' + roleName + '·' + emotion + '）');
}

function stShowImage(url) {
  let view = $('st-image-view');
  if (!view) {
    view = document.createElement('div');
    view.id = 'st-image-view';
    view.className = 'st-image-view';
    view.innerHTML = '<img alt="生成插图"><button type="button" class="st-image-close" aria-label="关闭">✕</button>';
    view.addEventListener('click', () => view.remove());
    document.body.appendChild(view);
  }
  view.querySelector('img').src = url;
  view.style.display = 'flex';
}

function stCharNameById(id) {
  if (!id) return '';
  const chars = (tavernPack && Array.isArray(tavernPack.characters)) ? tavernPack.characters : [];
  for (let i = 0; i < chars.length; i++) {
    if (String(chars[i].id || '') === String(id)) return (chars[i].name || '').trim();
  }
  return '';
}

function stRenderSprite() {
  const box = $('st-sprite');
  if (!box) return;
  const msgs = (tavernSession && tavernSession.messages) || [];

  // —— Step 1: 确定当前发言者 charId（回溯最近一条有发言人前缀的 assistant 消息）——
  let speakingCharId = '';
  let speakingName = '';
  for (let i = msgs.length - 1; i >= 0; i--) {
    const m = msgs[i];
    if (m && m.role && m.role !== 'user') {
      const speaker = stSpeakerNameOf(m);
      if (!speaker) continue;
      const u = stSpriteOf(speaker);
      if (u) { speakingCharId = stCharIdOf(speaker); speakingName = speaker; break; }
    }
  }

  // —— Step 2: 收集在场角色 ID 列表（presentCharacterIds 优先，无则降级单角色）——
  let characterIds = [];
  const present = (tavernSession && tavernSession.presentCharacterIds) || [];
  if (present.length > 1) {
    // 多角色：遍历 present，过滤掉有立绘的角色
    for (let j = 0; j < present.length; j++) {
      const cid = present[j];
      const name = stCharNameById(cid) || cid;
      const url = stSpriteOf(name);
      if (url) characterIds.push({ id: cid, name: name, url: url });
    }
  }

  // —— Step 3: 降级到单角色（无 presentCharacterIds 或多角色无立绘）——
  if (characterIds.length === 0) {
    // 复用旧逻辑：回溯最后一条有立绘的角色
    let url = null;
    let label = speakingName;
    if (!url && speakingName) url = stSpriteOf(speakingName);
    // 再尝试回溯
    if (!url) {
      for (let i = msgs.length - 1; i >= 0; i--) {
        const m = msgs[i];
        if (m && m.role && m.role !== 'user') {
          const s = stSpeakerNameOf(m);
          if (!s) continue;
          const u = stSpriteOf(s);
          if (u) { url = u; label = s; break; }
        }
      }
    }
    if (!url) {
      box.classList.add('hidden');
      return;
    }
    // 单图降级
    box.classList.remove('hidden');
    box.classList.add('is-single');
    box.setAttribute('aria-label', label ? (label + ' 立绘') : '角色立绘');
    // 清空旧内容（防旧结构残留）
    box.innerHTML = '';
    const slot = document.createElement('div');
    slot.className = 'st-sprite-slot';
    const img = document.createElement('img');
    img.className = 'st-sprite-img';
    img.src = url;
    img.alt = (label || '角色') + ' 立绘';
    img.addEventListener('click', function () { stShowImage(img.src); });
    slot.appendChild(img);
    box.appendChild(slot);
    return;
  }

  // —— Step 4: 多角色阵列渲染 ——
  box.classList.remove('is-single');
  box.classList.remove('hidden');
  box.setAttribute('aria-label', speakingName ? (speakingName + ' 等角色立绘') : '角色立绘');

  // 清除旧结构残留（旧版直接插 img，现统一为 .st-sprite-slot）
  Array.from(box.children).forEach(function (ch) {
    if (!ch.classList || !ch.classList.contains('st-sprite-slot')) ch.remove();
  });

  // 构建新 DOM（diff 更新：保留已有的 slot，增删差额）
  const existingSlots = box.querySelectorAll('.st-sprite-slot');
  const existingCount = existingSlots.length;
  const targetCount = characterIds.length;

  // 增加 slot
  for (let k = existingCount; k < targetCount; k++) {
    const slot = document.createElement('div');
    slot.className = 'st-sprite-slot';
    const img = document.createElement('img');
    img.className = 'st-sprite-img';
    img.alt = '角色立绘';
    img.addEventListener('click', function () { stShowImage(img.src); });
    slot.appendChild(img);
    const label = document.createElement('span');
    label.className = 'st-sprite-label';
    slot.appendChild(label);
    box.appendChild(slot);
  }
  // 删减多余 slot
  while (box.children.length > targetCount) {
    box.removeChild(box.lastChild);
  }

  // 更新每个 slot
  const slots = box.querySelectorAll('.st-sprite-slot');
  for (let k = 0; k < targetCount; k++) {
    const entry = characterIds[k];
    const slot = slots[k];
    const img = slot.querySelector('img');
    const labelEl = slot.querySelector('.st-sprite-label');

    // 发言状态 class
    const isSpeaking = entry.id && speakingCharId && String(entry.id) === String(speakingCharId);
    slot.classList.toggle('st-speaking', !!isSpeaking);
    slot.classList.toggle('st-idle', !isSpeaking);

    // 图片 src
    if (img && img.getAttribute('src') !== entry.url) img.setAttribute('src', entry.url);
    if (img) img.alt = (entry.name || '角色') + ' 立绘';
    // 名字标签
    if (labelEl) labelEl.textContent = entry.name || '';
  }
}

let stTtsAudio = null, stTtsUrl = '';

function stWriterQuality() {
  const btn = document.getElementById('st-writer-quality');
  if (btn && btn.dataset && btn.dataset.value) return btn.dataset.value;
  try { const v = localStorage.getItem('st-writer-quality'); if (v) return v; } catch (_) {}
  return 'lite';
}

function stTtsSync() {
  const r = $('st-tts-btn'), p = $('st-tts-pause'), s = $('st-tts-stop');
  const has = !!stTtsAudio;
  if (r) r.classList.toggle('hidden', has);
  if (s) s.classList.toggle('hidden', !has);
  if (p) {
    p.classList.toggle('hidden', !has);
    const lab = p.querySelector('.btn-lab');
    if (lab) lab.textContent = (has && stTtsAudio.paused) ? '继续' : '暂停';
  }
}

function stTtsPauseToggle() {
  if (!stTtsAudio) return;
  if (stTtsAudio.paused) { stTtsAudio.play().catch(() => {}); stStatus('🔊 播放中'); }
  else { stTtsAudio.pause(); stStatus('⏸ 已暂停'); }
  stTtsSync();
}

function stTtsStop() {
  if (!stTtsAudio) return;
  try { stTtsAudio.pause(); } catch (_) {}
  stTtsAudio = null;
  if (stTtsUrl) { URL.revokeObjectURL(stTtsUrl); stTtsUrl = ''; }
  stStatus('');
  stTtsSync();
}

const ST_VOICE_POOL = [
  'zh-CN-XiaoxiaoNeural', // 女·晓晓
  'zh-CN-YunxiNeural',    // 男·云希
  'zh-CN-YunyangNeural',  // 男·云扬
  'zh-CN-XiaoyiNeural',   // 女·晓伊
  'zh-CN-liaoning-XiaobeiNeural', // 东北女·晓北
];

function stVoiceOf(speaker) {
  if (!speaker) return 'zh-CN-XiaoxiaoNeural';
  // pack.characters[].voice 优先（含摘要字段透传）
  const packs = (typeof tavernPacks !== 'undefined' && Array.isArray(tavernPacks)) ? tavernPacks : [];
  for (const pk of packs) {
    const chars = (pk && Array.isArray(pk.characters)) ? pk.characters : [];
    for (const c of chars) {
      if (c && String(c.name || '').trim() === speaker && c.voice && String(c.voice).trim()) {
        return String(c.voice).trim();
      }
    }
  }
  // hash(roleName) 稳定选池（同角色同音色）
  let h = 0;
  for (let i = 0; i < speaker.length; i++) h = (h * 31 + speaker.charCodeAt(i)) >>> 0;
  return ST_VOICE_POOL[h % ST_VOICE_POOL.length];
}

const ST_EMO_RATE = {
  '愤怒': '+25%', '恐惧': '+15%', '惊讶': '+20%', '厌恶': '+10%',
  '疲惫': '-15%', '悲伤': '-20%', '心动': '-10%', '温柔': '-10%', '平静': '+0%',
};

function stRateOf(speaker) {
  // 从最后一条消息解析发言人情绪（复用 actorStates）
  if (!speaker || !tavernSession || !tavernSession.actorStates) return '';
  const actors = (tavernSession.actorStates.actors) || {};
  const cid = stCharIdOfLocal(speaker);
  const ent = (cid && actors[cid]) ? actors[cid] : actors[speaker];
  if (!ent || !ent.fields) return '';
  const emo = ent.fields.emotion;
  const v = (emo && typeof emo === 'object' && 'value' in emo) ? emo.value : emo;
  return (v && ST_EMO_RATE[v]) ? ST_EMO_RATE[v] : '';
}

function stCharIdOfLocal(name) {
  if (!name) return '';
  const chars = (typeof tavernPacks !== 'undefined' && Array.isArray(tavernPacks)) ? tavernPacks.flatMap(pk => (pk && Array.isArray(pk.characters)) ? pk.characters : []) : [];
  for (const c of chars) if (c && String(c.name || '').trim() === name) return c.id;
  return name;
}

async function stSpeak() {
  if (!tavernSession) { stStatus('无会话'); return; }
  const msgs = tavernSession.messages || [];
  let text = '';
  let speaker = '';
  for (let i = msgs.length - 1; i >= 0; i--) {
    if (msgs[i].role === 'assistant' && String(msgs[i].content || '').trim()) {
      text = String(msgs[i].content).trim().slice(0, 500);
      const c = String(msgs[i].content);
      const hit = c.match(/^[【\[]?([^：:\n]{1,12})[】\]]?[：:]/);
      speaker = hit ? hit[1].trim() : '';
      break;
    }
  }
  if (!text) { stStatus('没有可朗读的剧情'); return; }
  if (stTtsAudio) { try { stTtsAudio.pause(); } catch (_) {} stTtsAudio = null; if (stTtsUrl) URL.revokeObjectURL(stTtsUrl); stTtsUrl = ''; }
  const voice = stVoiceOf(speaker);
  const rate = stRateOf(speaker);
  stStatus(speaker ? ('🔊 朗读 ' + speaker + (rate ? '（' + rate + '）' : '') + '…') : '朗读中…');
  const res = await stFetch('/api/v1/kaleido-tools/tts', {
    body: JSON.stringify({ text, voice, rate })
  });
  if (!res.ok) throw new Error('HTTP ' + res.status);
  const blob = await res.blob();
  const url = URL.createObjectURL(blob);
  const a = new Audio(url);
  stTtsAudio = a; stTtsUrl = url;
  a.onended = () => {
    if (stTtsAudio === a) { stTtsAudio = null; stTtsUrl = ''; }
    stStatus('');
    URL.revokeObjectURL(url);
    stTtsSync();
  };
  a.onerror = () => {
    if (stTtsAudio === a) { stTtsAudio = null; stTtsUrl = ''; }
    stStatus('播放失败');
    URL.revokeObjectURL(url);
    stTtsSync();
  };
  a.play().catch(() => {});
  stStatus('🔊 播放中');
  stTtsSync();
}

async function stSend(text, sendOpts) {
  sendOpts = sendOpts || {};
  const rawIn = (text == null) ? '' : String(text);
  const isContinue = !!sendOpts.continue;
  // continue may be empty; normal send needs non-empty trim
  if ((!isContinue && !rawIn.trim()) || !tavernSession || tavernSession.packMissing || tavernStreaming) return;
  // 吸收自梨园 assistant-gateway：// 开头 → 剧情助手弹窗（独立会话，绝不代写剧情、不混入主线消息）
  if (!isContinue && rawIn.trim().startsWith('//')) {
    const q = rawIn.trim().slice(2).trim();
    stOpenAssistModal();
    const inp = $('st-assist-input');
    if (inp) inp.value = q;
    if (window.stFocusAssistInput) stFocusAssistInput();
    else if (inp) inp.focus();
    if ($('st-input')) $('st-input').value = '';
    return;
  }
  const payload = isContinue ? rawIn : rawIn.trim();
  if ($('st-input')) $('st-input').value = '';
  stSetComposerBusy(true);
  tavernStreaming = true;
  document.documentElement.setAttribute('data-streaming', '1');
  // S9.21: 流式期间公告读屏 (aria-busy)
  const stMsgsBusy = $('st-messages');
  if (stMsgsBusy) stMsgsBusy.setAttribute('aria-busy', 'true');
  try { stSetImmChromeVisible(true); } catch (_) {}
  stShowLlmIndicator();
  stTavernUserScrolled = false;
  tavernSession.messages = tavernSession.messages || [];
  const userMsg = {
    role: 'user',
    content: isContinue ? (payload || '（续写）') : payload,
    id: uid('u'),
    options: [],
  };
  if (isContinue) userMsg.kind = 'continue';
  tavernSession.messages.push(userMsg);
  const agentMsg = { role: 'assistant', content: '', id: uid('a'), options: [] };
  tavernSession.messages.push(agentMsg);
  stRenderMessages({ forceScroll: false });
  // S8.31: 发送后滚到新消息开头（顶部），生成中保持开头，用户从开头下滑阅读
  try { stScrollToLastMsgTop(); } catch (_) {}
  stRenderOptions([]);
  let controller = null;
  let llmHadError = false;
  try {
    let start;
    const stTurnBody = () => JSON.stringify({ message: isContinue ? '' : payload, quality: stWriterQuality() });
    try {
      start = await stApi('/sessions/' + encodeURIComponent(tavernSession.sessionId) + '/turn', { method: 'POST', body: stTurnBody() });
    } catch (e) {
      // Stuck previous turn: auto-stop then retry once
      const msg = String(e.message || e || '');
      if (/turn in progress|409|Conflict/i.test(msg) || e.status === 409) {
        try {
          const rid = (e.body && e.body.activeRunId) || tavernRunId || (tavernSession && tavernSession.activeRunId) || '';
          await stApi('/sessions/' + encodeURIComponent(tavernSession.sessionId) + '/stop', {
            method: 'POST', body: JSON.stringify({ runId: rid || 'force-unlock' })
          });
        } catch (_) {}
        start = await stApi('/sessions/' + encodeURIComponent(tavernSession.sessionId) + '/turn', { method: 'POST', body: stTurnBody() });
      } else {
        throw e;
      }
    }
    tavernRunId = start.runId;
    controller = new AbortController();
    window.__stController = controller;
    const headers = { Accept: 'text/event-stream' };
    if (__authToken()) { headers.Authorization = 'Bearer ' + __authToken(); headers['X-Mobile-Token'] = __authToken(); }
    const res = await fetch(apiBase() + '/api/v1/story-tavern/sessions/' + encodeURIComponent(tavernSession.sessionId) + '/stream?runId=' + encodeURIComponent(tavernRunId), { headers, cache: 'no-store', signal: controller.signal });
    if (!res.ok) { agentMsg.content = '流式错误 HTTP ' + res.status; stRenderMessages({ forceScroll: true }); return; }
    const reader = res.body.getReader(); const decoder = new TextDecoder('utf-8'); let buf = '';
    while (true) {
      const { value, done } = await reader.read();
      if (done) break;
      buf += decoder.decode(value, { stream: true });
      let idx; while ((idx = buf.indexOf('\n')) >= 0) {
        let line = buf.slice(0, idx); buf = buf.slice(idx + 1);
        if (line.endsWith('\r')) line = line.slice(0, -1);
        if (!line || line.startsWith(':')) continue;
        let data = line.startsWith('data:') ? line.slice(5).trimStart() : line;
        let obj; try { obj = JSON.parse(data); } catch (_) { continue; }
        if (obj.runId && obj.runId !== tavernRunId) continue;
        if (obj.type === 'delta' && obj.delta) {
          if (agentMsg._thinkingOnly) {
            agentMsg.content = '';
            agentMsg._thinkingOnly = false;
          }
          agentMsg.content += obj.delta;
          scheduleStStreamPaint();
        } else if (obj.type === 'thinking_delta' && obj.delta) {
          // 内心独白（Omate 对齐）：累积 thinking 内容供渲染折叠区块；
          // 同时保留原「思考中…」占位提示避免 UI 看起来卡死。
          if (!agentMsg._monologue) agentMsg._monologue = '';
          agentMsg._monologue += obj.delta;
          if (!agentMsg.content || agentMsg._thinkingOnly) {
            agentMsg._thinkingOnly = true;
            const tip = '（思考中…）';
            if (!agentMsg.content) agentMsg.content = tip;
            scheduleStStreamPaint();
          }
        } else if (obj.type === 'done') {
          // Finalize typewriter: mark stream-final for full paragraph formatting
          const stEl = $('st-messages');
          if (stEl) {
            stEl.querySelectorAll('.bubble.is-streaming .bubble-body').forEach(function (b) {
              b.setAttribute('data-stream-final', '1');
              b.removeAttribute('data-stream-base');
            });
            stEl.querySelectorAll('.bubble.is-streaming').forEach(function (b) {
              b.classList.remove('is-streaming');
            });
          }
          break;
        } else if (obj.type === 'error') {
          if (!agentMsg.content || agentMsg._thinkingOnly) {
            agentMsg.content = '请求失败：' + (obj.message || '');
            agentMsg._thinkingOnly = false;
          }
          stRenderMessages({ forceScroll: true });
          break;
        }
      }
    }
  } catch (e) {
    const raw = String((e && e.message) || e || '');
    let tip = raw;
    if (/Failed to fetch|NetworkError|Load failed|network/i.test(raw)) {
      // 断线自愈：不立即报死——后端 worker 仍在跑，结果最终会写入 session。
      // 轮询 session 直到 run 结束，恢复完整内容（切页面/网络抖动不丢输出）。
      const sid = tavernSession && tavernSession.sessionId;
      const hadPartial = !!(agentMsg.content && !agentMsg._thinkingOnly && agentMsg.content !== '（思考中…）');
      let recovered = false;
      try {
        for (let attempt = 0; attempt < 36; attempt++) {
          await new Promise((r) => setTimeout(r, 2500));
          const fresh = await stApi('/sessions/' + encodeURIComponent(sid));
          if (!fresh || !Array.isArray(fresh.messages)) break;
          const runDone = !fresh.activeRunId || fresh.activeRunId !== tavernRunId;
          const lastA = fresh.messages[fresh.messages.length - 1];
          const lastHasContent = lastA && lastA.role === 'assistant' && String(lastA.content || '').trim().length > 0;
          if (lastHasContent && runDone) {
            agentMsg.content = String(lastA.content || '');
            if (lastA.reasoning) agentMsg._monologue = lastA.reasoning;
            agentMsg._thinkingOnly = false;
            recovered = true;
            break;
          }
          if (runDone) break; // run 结束但无内容 = 真失败
        }
      } catch (_) {}
      if (recovered) {
        tip = '网络波动，已自动恢复完整内容';
      } else {
        // 未恢复：区分「后端真失败」与「后端仍在生成（上游慢/池忙）」
        let stillRunning = false;
        try {
          const chk = await stApi('/sessions/' + encodeURIComponent(sid));
          stillRunning = !!(chk && chk.activeRunId === tavernRunId);
        } catch (_) {}
        if (stillRunning) {
          tip = hadPartial
            ? '生成中（上游较慢）：已恢复部分内容，稍后刷新可查看完整结果'
            : '生成中（上游较慢）：断线已重连，稍后刷新可查看结果';
        } else {
          tip = hadPartial
            ? '网络波动：仅恢复部分内容，可点「重试」重新生成'
            : '生成失败：上游繁忙或网络断开，可点「重试」重新生成';
          llmHadError = true;
        }
        agentMsg.content = agentMsg.content || ('错误：' + tip);
      }
      stStatus(tip);
    } else if (/turn in progress/i.test(raw)) {
      tip = '上一回合未结束：已尝试解锁，请再发一次';
      agentMsg.content = agentMsg.content || ('错误：' + tip);
      stStatus(tip);
      llmHadError = true;
    } else {
      agentMsg.content = agentMsg.content || ('错误：' + tip);
      stStatus(tip);
      llmHadError = true;
    }
    stRenderMessages({ forceScroll: true });
  } finally {
    tavernStreaming = false;
    const stMsgsBusyEnd = $('st-messages');
    if (stMsgsBusyEnd) stMsgsBusyEnd.removeAttribute('aria-busy');
    // S8.31: LLM 指示器——正常结束消失；出错变红 3s 后消失
    if (llmHadError) {
      stErrorLlmIndicator();
      window.setTimeout(function () { stHideLlmIndicator(); }, 3000);
    } else {
      stHideLlmIndicator();
    }
    clearStStreamPaint();
    stSetComposerBusy(false);
    try {
      const fresh = await stApi('/sessions/' + encodeURIComponent(tavernSession.sessionId));
      tavernSession = fresh;
      // 内心独白：fresh 重拉覆盖消息对象，把流式累积的 monologue 粘回最后一条 assistant
      if (agentMsg._monologue && fresh && Array.isArray(fresh.messages)) {
        const mArr = fresh.messages;
        for (let i = mArr.length - 1; i >= 0; i--) {
          if (mArr[i] && mArr[i].role === 'assistant') {
            mArr[i]._monologue = agentMsg._monologue;
            break;
          }
        }
      }
      stRenderMessages({ quiet: true });
      // S8.30: 输出完成滚到新消息开头（用户从开头下滑阅读），不再停在底部
      try { stScrollToLastMsgTop(); } catch (_) {}
      // belt: full rebuild must not leave stream chrome
      clearStStreamPaint();
      stRenderOptions();
      stShowImmChrome(); /* auto-show chrome when options appear */
      stRenderFocusBar();
      stRenderRecallBar();
      // P3 自动朗读：开关开启且无用户手动播放中时，朗读最新一条 assistant 消息
      if (localStorage.getItem('stAutoTts') === '1' && !stTtsAudio && !tavernStreaming) {
        stSpeak().catch(() => {});
      }
      stStatus(`${tavernSession.title || '故事馆'} · ${PLAYABLE_LABELS[tavernSession.playable] || ''} · ${PLAY_MODE_LABELS[tavernSession.playMode] || tavernSession.playMode || ''} · node ${tavernSession.nodeId || '?'} · resume ${tavernSession.resumeNodeId || '-'} · ${stTurnLabel(tavernSession.turn || 0)}`);
      stSyncModeToggle();
      // best-effort BGE re-rank against the user turn we just sent
      stRefreshRecallSemantic().catch(() => {});
    } catch (_) {}
  }
}

function stSetComposerBusy(busy) {
  try {
    if ($('st-stop')) $('st-stop').classList.toggle('hidden', !busy);
    if ($('st-send')) $('st-send').disabled = !!busy;
    if ($('st-continue')) $('st-continue').disabled = !!busy;
    if ($('st-retry')) $('st-retry').disabled = !!busy;
  } catch (_) {}
}

function stScrollToLastMsgTop() {
  window.requestAnimationFrame(function () {
    window.requestAnimationFrame(function () {
      const el = $('st-messages');
      if (!el) return;
      const bubbles = el.querySelectorAll(
        '.st-bubble:not(.st-user):not(.st-role-user):not(.st-bubble-user)'
      );
      const lastA = bubbles.length ? bubbles[bubbles.length - 1] : null;
      if (!lastA) return;
      window.__stProgrammaticScroll = true;
      // #st-messages 全局 scroll-behavior:smooth 会把赋值变成动画、被后续
      // delta 渲染打断——临时禁用做瞬时定位。
      const prev = el.style.scrollBehavior;
      el.style.scrollBehavior = 'auto';
      el.scrollTop = Math.max(0, lastA.offsetTop - el.offsetTop - 12);
      el.style.scrollBehavior = prev;
      window.setTimeout(function () { window.__stProgrammaticScroll = false; }, 50);
    });
  });
}

function stShowLlmIndicator() {
  const el = $('st-llm-indicator');
  if (!el) return;
  el.classList.remove('hidden', 'error');
}

function stHideLlmIndicator() {
  const el = $('st-llm-indicator');
  if (!el) return;
  el.classList.add('hidden');
  el.classList.remove('error');
}

function stErrorLlmIndicator() {
  const el = $('st-llm-indicator');
  if (!el) return;
  el.classList.remove('hidden');
  el.classList.add('error');
}

function stContinue() {
  if (!tavernSession || tavernSession.packMissing || tavernStreaming) return;
  stSend('', { continue: true });
}

async function stRetry() {
  if (!tavernSession || tavernSession.packMissing || tavernStreaming) return;
  const msgs = Array.isArray(tavernSession.messages) ? tavernSession.messages.slice() : [];
  if (!msgs.length) {
    stStatus('还没有可重试的回合');
    return;
  }
  let userText = '';
  let cut = msgs.length;
  const last = msgs[msgs.length - 1];
  if (last && last.role !== 'user') {
    // … user, assistant  → drop both, resend user
    let ui = -1;
    for (let i = msgs.length - 2; i >= 0; i--) {
      if (msgs[i] && msgs[i].role === 'user') { ui = i; break; }
    }
    if (ui < 0) {
      stStatus('找不到上一轮用户发言');
      return;
    }
    userText = String(msgs[ui].content || '');
    const wasContinue = msgs[ui].kind === 'continue' || !userText.trim() || userText === '（续写）';
    cut = ui;
    if (wasContinue) {
      // restore without the pair, then continue
      try {
        stSetComposerBusy(true);
        const next = Object.assign({}, tavernSession, { messages: msgs.slice(0, cut) });
        tavernSession = await stApi('/sessions/' + encodeURIComponent(tavernSession.sessionId), {
          method: 'PUT', body: JSON.stringify(next),
        });
        stRenderMessages({ forceScroll: true, quiet: true });
        stRenderOptions([]);
        stSetComposerBusy(false);
        stSend('', { continue: true });
      } catch (e) {
        stSetComposerBusy(false);
        stStatus('重试失败：' + ((e && e.message) || e));
      }
      return;
    }
  } else if (last && last.role === 'user') {
    userText = String(last.content || '');
    cut = msgs.length - 1;
    if (last.kind === 'continue' || !userText.trim() || userText === '（续写）') {
      try {
        stSetComposerBusy(true);
        const next = Object.assign({}, tavernSession, { messages: msgs.slice(0, cut) });
        tavernSession = await stApi('/sessions/' + encodeURIComponent(tavernSession.sessionId), {
          method: 'PUT', body: JSON.stringify(next),
        });
        stRenderMessages({ forceScroll: true, quiet: true });
        stSetComposerBusy(false);
        stSend('', { continue: true });
      } catch (e) {
        stSetComposerBusy(false);
        stStatus('重试失败：' + ((e && e.message) || e));
      }
      return;
    }
  } else {
    stStatus('没有可重试的内容');
    return;
  }
  try {
    stSetComposerBusy(true);
    const next = Object.assign({}, tavernSession, { messages: msgs.slice(0, cut) });
    tavernSession = await stApi('/sessions/' + encodeURIComponent(tavernSession.sessionId), {
      method: 'PUT', body: JSON.stringify(next),
    });
    stRenderMessages({ forceScroll: true, quiet: true });
    stRenderOptions([]);
    stSetComposerBusy(false);
    await stSend(userText);
  } catch (e) {
    stSetComposerBusy(false);
    stStatus('重试失败：' + ((e && e.message) || e));
  }
}

function stStop() {
  if (window.__stController) { try { window.__stController.abort(); } catch (_) {} }
  const rid = tavernRunId || (tavernSession && tavernSession.activeRunId) || 'force-unlock';
  if (tavernSession && tavernSession.sessionId) {
    stApi('/sessions/' + encodeURIComponent(tavernSession.sessionId) + '/stop', {
      method: 'POST', body: JSON.stringify({ runId: rid })
    }).catch(() => {});
  }
  tavernStreaming = false;
  tavernRunId = null;
  const stMsgsBusyStop = $('st-messages');
  if (stMsgsBusyStop) stMsgsBusyStop.removeAttribute('aria-busy');
  clearStStreamPaint();
  stSetComposerBusy(false);
}

let stMediaRec = null;

let stRecChunks = [];

let stRecMime = 'audio/webm';

function stVoiceInputEnabled() {
  return localStorage.getItem('stVoiceInput') === '1';
}

function stSyncRecBtn() {
  const btn = $('st-asr-btn');
  if (!btn) return;
  const rec = stMediaRec && stMediaRec.state === 'recording';
  btn.classList.toggle('st-recording', rec);
  btn.classList.toggle('is-on', !rec && stVoiceInputEnabled());
  btn.disabled = !stVoiceInputEnabled() && !rec;
  btn.title = rec ? '录音中：点击停止' : (stVoiceInputEnabled() ? '点击开始录音（转写后直发）' : '语音输入已关闭，先点旁边 🎙 开关');
  const lab = btn.querySelector('.btn-lab');
  if (lab) lab.textContent = rec ? '停止' : '语音';
}

async function stToggleRecording() {
  if (stMediaRec && stMediaRec.state === 'recording') {
    stMediaRec.stop();
    return;
  }
  if (!stVoiceInputEnabled()) {
    stStatus('🎙 语音输入未开启：点亮旁边「语音双工」开关再试');
    return;
  }
  if (!navigator.mediaDevices || !navigator.mediaDevices.getUserMedia) {
    stStatus('📴 此环境不支持麦克风录音');
    return;
  }
  try {
    const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
    stRecChunks = [];
    const mime = ['audio/webm;codecs=opus', 'audio/webm', 'audio/mp4', '']
      .find((m) => !m || (window.MediaRecorder && MediaRecorder.isTypeSupported(m))) || '';
    const mr = mime ? new MediaRecorder(stream, { mimeType: mime }) : new MediaRecorder(stream);
    stMediaRec = mr;
    stRecMime = mime || mr.mimeType || 'audio/webm';
    mr.ondataavailable = (e) => { if (e.data && e.data.size) stRecChunks.push(e.data); };
    mr.onstop = () => {
      stream.getTracks().forEach((t) => t.stop());
      stMediaRec = null;
      stSyncRecBtn();
      const blob = new Blob(stRecChunks, { type: stRecMime });
      stRecChunks = [];
      if (!blob.size) { stStatus('🎤 未录到声音'); return; }
      stAsrSend(blob).catch((err) => stStatus('语音转写失败：' + ((err && err.message) || err)));
    };
    mr.onerror = () => {
      stream.getTracks().forEach((t) => t.stop());
      stMediaRec = null;
      stSyncRecBtn();
      stStatus('🎤 录音出错');
    };
    mr.start();
    stStatus('🎤 录音中…再来点一下停止');
    stSyncRecBtn();
  } catch (e) {
    stMediaRec = null;
    stSyncRecBtn();
    stStatus('🚫 麦克风权限被拒，无法录音');
  }
}

async function stAsrSend(blob) {
  const token = localStorage.getItem('kaleido_token') || '';
  const base = localStorage.getItem('kaleido_api_base') || '';
  const fd = new FormData();
  const ext = (stRecMime.indexOf('mp4') >= 0) ? 'm4a' : 'webm';
  fd.append('audio', blob, 'kaleido_capture.' + ext);
  stStatus('⏳ 语音转写中（首次约需几十秒加载引擎）…');
  const res = await fetch(base + '/api/v1/kaleido-tools/asr', {
    method: 'POST',
    headers: token ? { 'Authorization': 'Bearer ' + token } : {},
    body: fd,
  });
  if (!res.ok) {
    let msg = 'HTTP ' + res.status;
    try { const j = await res.json(); if (j && j.error) msg = j.error; } catch (_) {}
    throw new Error(msg);
  }
  const j = await res.json();
  const text = (j && typeof j.text === 'string') ? j.text.trim() : '';
  if (!text) throw new Error('转写为空');
  // 语音双工：转写结果直发（自动朗读开关开启时，回合结束即自动朗读回复）
  stStatus('🎤 已听懂：' + (text.length > 22 ? text.slice(0, 22) + '…' : text));
  stSend(text);
}

let stImmChromeBound = false;

let stImmTapStart = null;

function stSetImmChromeVisible(on) {
  const root = document.documentElement;
  if (root.getAttribute('data-immersive') !== '1') {
    root.classList.remove('imm-top-hidden');
    root.classList.remove('imm-chrome-hidden');
    return;
  }
  // streaming: force chrome so 停止 is reachable
  if (tavernStreaming) {
    root.classList.remove('imm-top-hidden');
    root.classList.remove('imm-chrome-hidden');
    return;
  }
  const layout = $('st-layout');
  if (layout && layout.classList.contains('st-side-open')) {
    root.classList.remove('imm-top-hidden');
    root.classList.remove('imm-chrome-hidden');
    return;
  }
  // S8.29: scrolling hides top bar AND bottom dock together for full-screen
  // reading; returning to top (or tap) restores both. Wand lives in dock.
  const wantHidden = !on;
  const isHidden = root.classList.contains('imm-chrome-hidden');
  if (wantHidden === isHidden) return;
  root.classList.toggle('imm-chrome-hidden', wantHidden);
  root.classList.remove('imm-top-hidden');
  // showing top shrinks message pane — if already near end, keep tail visible
  if (!wantHidden && isHidden) {
    try { stKeepImmTailVisible(); } catch (_) {}
  }
}

function stKeepImmTailVisible() {
  // retained no-op-ish helper for option growth: only nudge if already near end
  const msg = $('st-messages');
  if (!msg) return;
  if (document.documentElement.getAttribute('data-immersive') !== '1') return;
  if (document.documentElement.classList.contains('imm-chrome-hidden')) return;
  const gap = msg.scrollHeight - msg.scrollTop - msg.clientHeight;
  if (gap > 80) return;
  requestAnimationFrame(function () {
    try { msg.scrollTop = msg.scrollHeight; } catch (_) {}
  });
}

function stSyncImmChromeFromScroll() {
  // S8.29: scrolling the story hides top bar AND bottom dock for full-screen
  // reading; wand returns with the dock when back at top.
  const msg = $('st-messages');
  if (!msg) return;
  const root = document.documentElement;
  if (root.getAttribute('data-immersive') !== '1') return;
  const y = msg.scrollTop;
  root.classList.toggle('imm-chrome-hidden', y > 24);
  root.classList.remove('imm-top-hidden');
}

function stShowImmChrome() {
  stSetImmChromeVisible(true);
}

function stArmImmChromeHide() {
  // S8.29: hidden by scroll via imm-chrome-hidden; nothing to arm here.
  document.documentElement.classList.remove('imm-top-hidden');
}

function stToggleImmChromeFromTap() {
  if (document.documentElement.getAttribute('data-immersive') !== '1') return;
  if (tavernStreaming) {
    stSetImmChromeVisible(true);
    return;
  }
  const hidden = document.documentElement.classList.contains('imm-chrome-hidden');
  stSetImmChromeVisible(hidden); // if hidden → show; if shown → hide
}

function stImmTapTargetOk(t) {
  if (!t || !t.closest) return false;
  // don't toggle when interacting with controls
  if (t.closest('button, a, input, textarea, select, label, .st-option-chip, .st-history-fold, .composer-actions, .st-composer-tools, .imm-bar')) {
    return false;
  }
  return true;
}

function stImmTapInCenterBand(clientY, msgEl) {
  if (!msgEl) return false;
  const r = msgEl.getBoundingClientRect();
  if (r.height < 8) return false;
  const y = (clientY - r.top) / r.height;
  // middle band of the stage (not top/bottom chrome edges)
  return y >= 0.22 && y <= 0.78;
}

function stBindImmChrome() {
  const msg = $('st-messages');
  if (msg && !msg._immChromeBound) {
    msg.addEventListener('pointerdown', function (e) {
      if (e.button != null && e.button !== 0) return;
      stImmTapStart = {
        x: e.clientX,
        y: e.clientY,
        t: Date.now(),
        ok: stImmTapTargetOk(e.target),
      };
    }, { passive: true });
    msg.addEventListener('pointerup', function (e) {
      const s = stImmTapStart;
      stImmTapStart = null;
      if (!s || !s.ok) return;
      if (e.button != null && e.button !== 0) return;
      const dx = Math.abs((e.clientX || 0) - s.x);
      const dy = Math.abs((e.clientY || 0) - s.y);
      // treat as scroll/drag, not tap
      if (dx > 12 || dy > 12) return;
      if (Date.now() - s.t > 650) return;
      if (!stImmTapInCenterBand(e.clientY, msg)) return;
      stToggleImmChromeFromTap();
    }, { passive: true });
    msg.addEventListener('pointercancel', function () { stImmTapStart = null; }, { passive: true });
    msg._immChromeBound = true;
  }
  // default hidden when binding in immersive
  try { stArmImmChromeHide(); } catch (_) {}
  if (!stImmChromeBound) {
    stImmChromeBound = true;
  }
}

function stMountWizard() {
  const wizView = $('st-view-wizard');
  if (!wizView) return;
  const active = document.querySelector('.tab-panel:not(.hidden)');
  const activeId = active ? active.id : '';
  let host = null;
  if (activeId === 'tab-tavern') {
    host = document.querySelector('#tab-tavern .st-main') || document.querySelector('#tab-tavern');
  } else {
    host = document.querySelector('#tab-packs .st-packs-page') || document.querySelector('#tab-packs');
  }
  if (host && wizView.parentElement !== host) host.appendChild(wizView);
}

function stOpenWizard(playable, source) {
  // R1: 记录进入来源，供向导取消/剧场退出按来源返回
  stNavFrom = (source === 'story-entry' || source === 'packs-detail') ? source : '';
  stMountWizard();
  const wiz = $('st-wizard');
  if (wiz) wiz.classList.remove('hidden');
  const wizView = $('st-view-wizard');
  if (wizView) wizView.classList.remove('hidden');
  // 隐藏档案馆的其他视图
  const listview = $('st-packs-listview');
  const packDetail = $('st-view-pack');
  if (listview) listview.classList.add('hidden');
  if (packDetail) packDetail.classList.add('hidden');
  // 故事馆视图也同步
  const entry = $('st-view-entry');
  const play = $('st-view-play');
  if (entry) entry.classList.add('hidden');
  if (play) play.classList.add('hidden');
  if (playable) $('st-wizard-playable').value = playable;
  if (tavernPack && tavernPack.id) {
    const w = $('st-wizard-pack');
    if (w) w.value = tavernPack.id;
  }
  stWizardToggleRole();
  const msg = $('st-wizard-msg'); if (msg) msg.textContent = '';
}

function stWizardToggleRole() {
  const role = $('st-wizard-role').value;
  $('st-wizard-isekai').classList.toggle('hidden', role !== 'isekai');
  const packId = $('st-wizard-pack').value;
  const pack = tavernPacks.find(p => p.id === packId);
  const vessel = $('st-wizard-vessel');
  if (!vessel || vessel.tagName !== 'SELECT') return;
  const prev = vessel.value;
  const need = (role === 'supporting' || role === 'protagonist');
  vessel.title = '';
  vessel.innerHTML = '';
  const mkOpt = (label, val) => { const o = document.createElement('option'); o.value = val; o.textContent = label; return o; };
  vessel.appendChild(mkOpt(need ? '（请选择要附体的角色）' : '不附身', ''));
  const chars = (pack && Array.isArray(pack.characters)) ? pack.characters : [];
  const shown = chars.filter((c) => {
    const n = String(c.name || '').trim();
    const r = String(c.role || '').toLowerCase();
    if (r.includes('narrator') || n === '旁白') return false;
    return !!(c.id && n);
  });
  for (const c of shown) vessel.appendChild(mkOpt(c.name + '（' + c.id + '）', c.id));
  if (!shown.length) {
    vessel.appendChild(mkOpt('（本包暂无角色卡）', ''));
    vessel.title = '当前包没有可附体角色，请先在角色卡页导入/确认';
    vessel.value = '';
    return;
  }
  vessel.value = ([...vessel.options].some(o => o.value === prev) ? prev : (need ? shown[0].id : ''));
}

async function stCreateSession() {
  const packId = $('st-wizard-pack').value;
  if (!packId) { $('st-wizard-msg').textContent = '请选择 Pack'; return; }
  const playable = $('st-wizard-playable').value;
  // R5: 从作者区/分析页进入时带上 workId，让 U13 的 create 罗盘自动挂载生效
  // （anWorkId() 回退 'default' 时视为无项目，保持原行为）
  const wId = (typeof anWorkId === 'function') ? anWorkId() : '';
  const req = {
    packId,
    playable,
    playMode: $('st-wizard-mode').value,
    userTier: $('st-wizard-tier').value,
    adultConfirmed: !!$('st-wizard-adult').checked,
    workId: (wId && wId !== 'default') ? wId : undefined,
  };
  if (playable === 'P3') {
    const entry = {
      entryRole: $('st-wizard-role').value,
      metaKnowledge: $('st-wizard-meta').value,
      rewriteIntensity: $('st-wizard-rewrite').value,
    };
    const vessel = ($('st-wizard-vessel').value || '').trim();
    if (vessel) entry.vesselCharacterId = vessel;
    if (entry.entryRole === 'isekai') {
      entry.isekai = {};
      const fields = ['name','appearance','cheat','origin'];
      for (const f of fields) entry.isekai[f] = ($('st-wizard-isekai-' + f).value || '').trim();
    }
    req.entry = entry;
  }
  try {
    const s = await stApi('/sessions', { method: 'POST', body: JSON.stringify(req) });
    tavernSession = s;
    $('st-wizard').classList.add('hidden');
    await stLoadSession(s.sessionId);
    $('st-wizard-msg').textContent = '';
  } catch (e) {
    $('st-wizard-msg').textContent = '创建失败：' + e.message;
  }
}

let shelfNovels = [];

let shelfActiveSlug = null;

export async function stSwipe(divEl, dir) {
  if (!divEl || !tavernSession) return;
  const mid = divEl.getAttribute('data-mid');
  if (!mid) return;
  const msgs = tavernSession.messages || [];
  const idx = msgs.findIndex(function (m) { return String(m.id || '') === mid; });
  if (idx < 0 || msgs[idx].role === 'user') return;
  const m = msgs[idx];
  if (!m._swipes) m._swipes = [String(m.content || '')];
  if (typeof m._swipeIdx !== 'number') m._swipeIdx = 0;
  // [Swipe 后端持久化] 首次加载时把后端 swipes 映射到 _swipes
  if (m.swipes && m.swipes.length && m._swipes.length <= 1) {
    m.swipes.forEach(function(s){ if (m._swipes.indexOf(s) < 0) m._swipes.push(s); });
  }
  const n = m._swipes.length;
  // 右箭头且已是最后一条 → 请求新备选（reroll 变体）
  if (dir > 0 && m._swipeIdx >= n - 1) {
    if (tavernStreaming) { stStatus('流式进行中，稍候再试'); return; }
    try {
      stStatus('生成备选回复…');
      const prev = String(m.content || '');
      const fresh = await stApi('/sessions/' + encodeURIComponent(tavernSession.sessionId) + '/reroll', { method: 'POST', body: '{}' });
      // reroll 返回 {ok, lastUserMessage, turn: <turn数>}——新回复需重拉会话取最后一条 assistant
      let freshText = '';
      if (fresh && fresh.ok) {
        const s2 = await stApi('/sessions/' + encodeURIComponent(tavernSession.sessionId));
        const arr = (s2 && Array.isArray(s2.messages)) ? s2.messages : [];
        for (let i = arr.length - 1; i >= 0; i--) {
          if (arr[i] && arr[i].role === 'assistant' && String(arr[i].content || '').trim()) {
            freshText = String(arr[i].content).trim(); break;
          }
        }
      }
      if (freshText && freshText !== prev) {
        m._swipes.push(freshText);
        m._swipeIdx = m._swipes.length - 1;
      } else {
        stStatus('新备选与当前一致或获取失败');
        return;
      }
    } catch (e) {
      stStatus('备选失败：' + ((e && e.message) || e));
      return;
    }
  } else {
    let ni = m._swipeIdx + dir;
    if (ni < 0) ni = 0;
    if (ni >= n) return;
    m._swipeIdx = ni;
    try { stApi('/sessions/' + encodeURIComponent(tavernSession.sessionId) + '/messages/' + encodeURIComponent(mid) + '/swipe', { method: 'PUT', body: JSON.stringify({ index: ni }) }).catch(function(){}); } catch (e) {}
  }
  // 切正文（只换 body 文本，保留角色/角标/程序卡）
  const bodyEl = divEl.querySelector('.bubble-body');
  if (bodyEl && typeof fillBubbleBody === 'function') {
    fillBubbleBody(bodyEl, m._swipes[m._swipeIdx], { speakerMode: true });
  } else if (bodyEl) {
    bodyEl.textContent = m._swipes[m._swipeIdx];
  }
  const cnt = divEl.querySelector('.st-swipe-cnt');
  if (cnt) cnt.textContent = (m._swipeIdx + 1) + '/' + m._swipes.length;
  const sel = stStatus; if (sel) sel('备选 ' + (m._swipeIdx + 1) + '/' + m._swipes.length);
}

function stSwipePicker(divEl) {
  if (!divEl || !tavernSession) return;
  const mid = divEl.getAttribute('data-mid');
  if (!mid) return;
  const msgs = tavernSession.messages || [];
  const idx = msgs.findIndex(function (m) { return String(m.id || '') === mid; });
  if (idx < 0) return;
  const m = msgs[idx];
  const swipes = (m._swipes && m._swipes.length) ? m._swipes : [String(m.content || '')];
  const cur = (typeof m._swipeIdx === 'number') ? m._swipeIdx : 0;

  // 复用 .st-modal 弹窗体系
  const modal = document.createElement('div');
  modal.className = 'st-modal';
  modal.id = 'st-swipe-picker';
  const card = document.createElement('div');
  card.className = 'st-modal-card';
  const head = document.createElement('div');
  head.className = 'st-modal-head';
  const title = document.createElement('div');
  title.className = 'st-modal-title'; title.textContent = '备选回复 (' + swipes.length + ')';
  const close = document.createElement('button');
  close.type = 'button'; close.className = 'st-modal-close'; close.setAttribute('aria-label', '关闭');
  close.innerHTML = '&#10005;';
  head.appendChild(title); head.appendChild(close);
  const body = document.createElement('div');
  body.className = 'st-modal-body st-swipe-picker-body';
  swipes.forEach(function (text, i) {
    const item = document.createElement('button');
    item.type = 'button';
    item.className = 'st-swipe-pick' + (i === cur ? ' current' : '');
    const num = document.createElement('span');
    num.className = 'st-swipe-pick-num'; num.textContent = (i + 1) + '/' + swipes.length;
    const txt = document.createElement('span');
    txt.className = 'st-swipe-pick-txt';
    txt.textContent = String(text || '').replace(/\s+/g, ' ').slice(0, 140);
    item.appendChild(num); item.appendChild(txt);
    item.onclick = function (e) {
      e.stopPropagation();
      m._swipeIdx = i;
      // 更新正文
      const bodyEl = divEl.querySelector('.bubble-body');
      if (bodyEl && typeof fillBubbleBody === 'function') {
        fillBubbleBody(bodyEl, m._swipes[m._swipeIdx], { speakerMode: true });
      } else if (bodyEl) {
        bodyEl.textContent = m._swipes[m._swipeIdx];
      }
      const cntEl = divEl.querySelector('.st-swipe-cnt');
      if (cntEl) cntEl.textContent = (m._swipeIdx + 1) + '/' + m._swipes.length;
      closeModal();
      const sel2 = stStatus; if (sel2) sel2('已选备选 ' + (m._swipeIdx + 1) + '/' + m._swipes.length);
    };
    body.appendChild(item);
  });
  card.appendChild(head); card.appendChild(body);
  modal.appendChild(card);
  document.body.appendChild(modal);

  function closeModal() {
    if (modal.parentNode) modal.parentNode.removeChild(modal);
    document.removeEventListener('keydown', onKey);
  }
  function onKey(e) {
    if (e.key === 'Escape') closeModal();
  }
  close.onclick = function (e) { e.stopPropagation(); closeModal(); };
  modal.addEventListener('click', function (e) { if (e.target === modal) closeModal(); });
  document.addEventListener('keydown', onKey);
}

async function stEditMessage(divEl) {
  if (!divEl || !tavernSession || tavernStreaming) return;
  const mid = divEl.getAttribute('data-mid');
  if (!mid) return;
  const msgs = tavernSession.messages || [];
  const idx = msgs.findIndex(function (m) { return String(m.id || '') === mid; });
  if (idx < 0) return;
  const m = msgs[idx];
  const bodyEl = divEl.querySelector('.bubble-body');
  const oldText = (bodyEl && bodyEl.textContent) || String(m.content || '');
  const fresh = await showPrompt('编辑消息内容：', { value: oldText });
  if (fresh === null) return;
  if (!String(fresh).trim()) { stStatus('内容不能为空'); return; }
  stApi('/sessions/' + encodeURIComponent(tavernSession.sessionId) + '/messages/' + encodeURIComponent(mid), {
    method: 'PUT', body: JSON.stringify({ content: String(fresh) })
  }).then(function (s) {
    tavernSession = s;
    stRenderMessages({ forceScroll: true, quiet: true });
    stStatus('消息已编辑');
  }).catch(function (e) { stStatus('编辑失败：' + ((e && e.message) || e)); });
}

async function stDeleteMessage(divEl) {
  if (!divEl || !tavernSession || tavernStreaming) return;
  const mid = divEl.getAttribute('data-mid');
  if (!mid) return;
  if (!await showConfirm('删除这条消息？')) return;
  stApi('/sessions/' + encodeURIComponent(tavernSession.sessionId) + '/messages/' + encodeURIComponent(mid), {
    method: 'DELETE'
  }).then(function (s) {
    tavernSession = s;
    stRenderMessages({ forceScroll: true, quiet: true });
    stStatus('消息已删除');
  }).catch(function (e) { stStatus('删除失败：' + ((e && e.message) || e)); });
}

function stPartialEdit(divEl) {
  if (!divEl || !tavernSession || tavernStreaming) return;
  const mid = divEl.getAttribute('data-mid');
  if (!mid) return;
  const msgs = tavernSession.messages || [];
  const m = msgs.find(function (x) { return String(x.id || '') === mid; });
  if (!m) return;
  const bodyEl = divEl.querySelector('.bubble-body');
  const oldText = (bodyEl && bodyEl.textContent) || String(m.content || '');
  // 弹层
  const overlay = document.createElement('div');
  overlay.className = 'st-modal-overlay';
  overlay.innerHTML =
    '<div class="st-modal st-partial-modal">' +
    '<div class="st-modal-head">部分编辑消息<span class="st-modal-x" title="关闭">✕</span></div>' +
    '<div class="st-partial-hint">修改后保存；仅本消息正文更新，其余消息不变。</div>' +
    '<textarea class="st-partial-text" rows="8" spellcheck="false"></textarea>' +
    '<div class="st-modal-foot"><button type="button" class="ghost st-partial-cancel">取消</button>' +
    '<button type="button" class="st-partial-save">保存</button></div>' +
    '</div>';
  document.body.appendChild(overlay);
  const ta = overlay.querySelector('.st-partial-text');
  ta.value = oldText;
  overlay.querySelector('.st-modal-x').onclick = function () { overlay.remove(); };
  overlay.querySelector('.st-partial-cancel').onclick = function () { overlay.remove(); };
  overlay.addEventListener('click', function (e) { if (e.target === overlay) overlay.remove(); });
  overlay.querySelector('.st-partial-save').onclick = function () {
    const fresh = ta.value;
    if (!String(fresh).trim()) { stStatus('内容不能为空'); return; }
    stApi('/sessions/' + encodeURIComponent(tavernSession.sessionId) + '/messages/' + encodeURIComponent(mid), {
      method: 'PUT', body: JSON.stringify({ content: String(fresh) })
    }).then(function (s) {
      tavernSession = s;
      overlay.remove();
      stRenderMessages({ forceScroll: true, quiet: true });
      stStatus('消息已更新');
    }).catch(function (e) { stStatus('保存失败：' + ((e && e.message) || e)); });
  };
  ta.focus();
  ta.setSelectionRange(0, 0);
}

function stBookmarkKey() {
  return 'st_bookmarks_v1';
}

function stLoadBookmarks() {
  try {
    const raw = localStorage.getItem(stBookmarkKey());
    const arr = raw ? JSON.parse(raw) : [];
    return Array.isArray(arr) ? arr : [];
  } catch (_) { return []; }
}

function stSaveBookmarks(arr) {
  try { localStorage.setItem(stBookmarkKey(), JSON.stringify(arr.slice(-200))); } catch (_) {}
}

function stToggleBookmark(divEl) {
  if (!divEl || !tavernSession) return;
  const mid = divEl.getAttribute('data-mid');
  if (!mid) return;
  const msgs = tavernSession.messages || [];
  const m = msgs.find(function (x) { return String(x.id || '') === mid; });
  if (!m) return;
  const bodyEl = divEl.querySelector('.bubble-body');
  const text = ((bodyEl && bodyEl.textContent) || String(m.content || '')).trim().slice(0, 120);
  const list = stLoadBookmarks();
  const idx = list.findIndex(function (b) { return b.mid === mid && b.sessionId === tavernSession.sessionId; });
  if (idx >= 0) {
    list.splice(idx, 1);
    stSaveBookmarks(list);
    stStatus('已取消收藏');
  } else {
    list.unshift({
      mid: mid,
      sessionId: tavernSession.sessionId,
      title: tavernSession.title || '会话',
      role: m.role || '',
      text: text,
      ts: Date.now()
    });
    stSaveBookmarks(list);
    stStatus('已收藏消息 ⭐');
  }
  if (typeof stRenderBookmarks === 'function') stRenderBookmarks();
}

function stBmEsc(s) {
  return String(s == null ? '' : s)
    .replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;').replace(/'/g, '&#39;');
}

function stRenderBookmarks() {
  var host = $('st-bookmarks');
  if (!host) return;
  var list = stLoadBookmarks();
  if (!list.length) {
    host.innerHTML = '<div class="st-bm-empty">暂无书签——消息操作菜单 ⭐ 收藏重点剧情。</div>';
    return;
  }
  var html = '<div class="st-bm-head">书签 · ' + list.length + '</div><div class="st-bm-list">';
  for (var i = 0; i < list.length; i++) {
    var b = list[i];
    var roleTag = b.role === 'user' ? '你' : 'AI';
    html += '<div class="st-bm-item" data-bm="' + i + '">' +
      '<div class="st-bm-role">' + roleTag + '</div>' +
      '<div class="st-bm-body">' +
      '<div class="st-bm-text">' + stBmEsc(b.text || '') + '</div>' +
      '<div class="st-bm-meta">' + stBmEsc(b.title || '') + ' · ' + new Date(b.ts || Date.now()).toLocaleString() + '</div>' +
      '</div>' +
      '<button type="button" class="st-bm-del" title="删除书签">✕</button>' +
      '</div>';
  }
  html += '</div>';
  host.innerHTML = html;
  // 事件绑定
  var items = host.querySelectorAll('.st-bm-item');
  Array.prototype.forEach.call(items, function (it) {
    it.querySelector('.st-bm-del').onclick = function (e) {
      e.stopPropagation();
      var idx = parseInt(it.getAttribute('data-bm'), 10);
      var arr = stLoadBookmarks();
      arr.splice(idx, 1);
      stSaveBookmarks(arr);
      stRenderBookmarks();
    };
  });
}

function stApplyChatWidth(pct) {
  const msgs = $('st-messages');
  if (!msgs) return;
  const stage = msgs.closest('.stage-messages') || msgs.parentElement;
  if (!stage) return;
  const p = Math.max(40, Math.min(100, pct || 100));
  // 宽度作用于剧场视图容器（st-view-play），而非消息区父链
  const host = $('st-view-play');
  if (host) {
    host.style.maxWidth = (p === 100) ? '' : p + '%';
    host.style.margin = (p === 100) ? '' : '0 auto';
  }
  const val = $('st-chat-width-val');
  if (val) val.textContent = p + '%';
  const slider = $('st-chat-width');
  if (slider) slider.value = String(p);
  try { localStorage.setItem('stChatWidth', String(p)); } catch (_) {}
}

function stApplyBubbleStyle(style) {
  const stage = $('st-view-play') || document.documentElement;
  stage.setAttribute('data-msg-style', style || 'bubble');
  try { localStorage.setItem('stBubbleStyle', style || 'bubble'); } catch (_) {}
  const seg = $('st-bubble-style');
  if (seg) {
    const btns = seg.querySelectorAll('.st-seg-btn');
    Array.prototype.forEach.call(btns, function (b) {
      b.classList.toggle('active', b.getAttribute('data-style') === (style || 'bubble'));
    });
  }
}

function stWireDisplaySettings() {
  // 宽度滑杆
  const slider = $('st-chat-width');
  if (slider) {
    const savedW = parseInt(localStorage.getItem('stChatWidth') || '100', 10);
    stApplyChatWidth(isNaN(savedW) ? 100 : savedW);
    slider.oninput = function () { stApplyChatWidth(parseInt(slider.value, 10)); };
  }
  // 气泡风格
  const seg = $('st-bubble-style');
  if (seg) {
    const savedStyle = localStorage.getItem('stBubbleStyle') || 'bubble';
    stApplyBubbleStyle(savedStyle);
    const btns = seg.querySelectorAll('.st-seg-btn');
    Array.prototype.forEach.call(btns, function (b) {
      b.onclick = function () { stApplyBubbleStyle(b.getAttribute('data-style')); };
    });
  }
  // 时间戳开关
  const metaBtn = $('st-msg-meta');
  if (metaBtn) {
    const syncMeta = function () {
      const on = localStorage.getItem('stMsgMeta') === '1';
      metaBtn.dataset.on = on ? '1' : '0';
      metaBtn.classList.toggle('is-on', on);
    };
    metaBtn.onclick = function () {
      const on = localStorage.getItem('stMsgMeta') === '1';
      localStorage.setItem('stMsgMeta', on ? '0' : '1');
      syncMeta();
      stRenderMessages({ quiet: true });
      stStatus(on ? '消息元信息已关闭' : '消息元信息已开启（时间戳 + token）');
    };
    syncMeta();
  }
}

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

const _origSwitchTabShelf = typeof switchTab === 'function' ? switchTab : null;

const stAsrBtn = $('st-asr-btn');

const stAsrToggle = $('st-asr-toggle');

const stAutoBtn = $('st-tts-auto');

const stImgSelBtn = $('st-image-channel');

const stImgSelOpts = document.querySelectorAll('#st-image-channel-list .st-select-opt');

const stQualitySelBtn = $('st-writer-quality');

const stTierSelBtn = $('st-content-tier');

const stPovBtn = $('st-narr-pov');

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

async function stDeletePack(id) {
  if (!await showConfirm('删除 Pack？引用它的会话将变为只读。')) return;
  await stApi('/packs/' + encodeURIComponent(id), { method: 'DELETE' });
  await stLoadPacks();
  stStatus('Pack 已删除');
}

function stShowChapter(packId, chId) {
  const p = tavernPacks.find(x => x.id === packId); if (!p) return;
  const ch = p.chapters.find(x => x.id === chId); if (!ch) return;
  tavernPack = p;
  stRenderLore();
  $('st-chapter-view').classList.remove('hidden');
  $('st-chapter-view').dataset.chapterId = chId;
  const pre = $('st-chapter-view').querySelector('pre');
  pre.textContent = '章节：' + ch.title + '\n节点：' + (ch.nodeIds || []).join('、') + '\n正文加载中…';
  stApi('/packs/' + encodeURIComponent(packId) + '/chapters/' + encodeURIComponent(ch.bodyPath))
    .then(body => {
      pre.textContent = (body.content || '').slice(0, 800);
    })
    .catch(e => { pre.textContent = '读取失败：' + e.message; });
}

let stLoreEditIdx = -1;

function stEnsureLoreArray(pack) {
  if (!pack.loreEntries || !Array.isArray(pack.loreEntries)) pack.loreEntries = [];
  return pack.loreEntries;
}

function stRenderLore() {
  const panel = $('st-lore-panel');
  const list = $('st-lore-list');
  if (!panel || !list) return;
  if (!tavernPack) { panel.classList.add('hidden'); return; }
  panel.classList.remove('hidden');
  const entries = stEnsureLoreArray(tavernPack);
  list.innerHTML = '';
  if (!entries.length) {
    list.innerHTML = '<div class="muted sm">暂无 lore，可添加永久条或章范围条</div>';
    return;
  }
  entries.forEach((e, i) => {
    const el = document.createElement('div');
    el.className = 'item' + (stLoreEditIdx === i ? ' active' : '');
    const title = e.title || e.id || ('条目' + (i + 1));
    const meta = (e.permanent ? '永久' : (e.chapterRange || '无范围')) + (e.nodeIds && e.nodeIds.length ? ' · nodes ' + e.nodeIds.join(',') : '');
    el.innerHTML = '<span class="t"></span><small></small>';
    el.querySelector('.t').textContent = title;
    el.querySelector('small').textContent = meta;
    el.onclick = () => stOpenLoreEditor(i);
    list.appendChild(el);
  });
}

function stOpenLoreEditor(idx) {
  stLoreEditIdx = idx;
  $('st-lore-editor').classList.remove('hidden');
  const e = idx >= 0 ? stEnsureLoreArray(tavernPack)[idx] : { title: '', text: '', chapterRange: '', permanent: true, nodeIds: [] };
  $('st-lore-title').value = e.title || '';
  $('st-lore-text').value = e.text || e.content || '';
  $('st-lore-range').value = e.chapterRange || '';
  $('st-lore-perm').checked = !!e.permanent || !e.chapterRange;
  stRenderLore();
}

async function stSaveLore() {
  if (!tavernPack) return;
  const entries = stEnsureLoreArray(tavernPack);
  const entry = {
    id: (stLoreEditIdx >= 0 && entries[stLoreEditIdx].id) || ('lore-' + Date.now()),
    title: ($('st-lore-title').value || '').trim() || '未命名',
    text: ($('st-lore-text').value || '').trim(),
    chapterRange: ($('st-lore-range').value || '').trim(),
    permanent: !!$('st-lore-perm').checked,
    nodeIds: (stLoreEditIdx >= 0 && entries[stLoreEditIdx].nodeIds) || [],
  };
  if (stLoreEditIdx >= 0) entries[stLoreEditIdx] = entry; else entries.push(entry);
  tavernPack.loreEntries = entries;
  tavernPack.updatedAt = new Date().toISOString();
  // strip UI-only if any
  const payload = JSON.parse(JSON.stringify(tavernPack));
  delete payload.uploadChapters;
  await stApi('/packs', { method: 'POST', body: JSON.stringify(payload) });
  $('st-lore-editor').classList.add('hidden');
  stLoreEditIdx = -1;
  await stLoadPacks();
  const fresh = tavernPacks.find(p => p.id === payload.id);
  if (fresh) { tavernPack = fresh; }
  stRenderLore();
  stStatus('Lore 已保存 · ' + entry.title);
}

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
        const btnF = document.createElement('button');
        btnF.type = 'button'; btnF.className = 'sm'; btnF.textContent = '分叉新会话';
        btnF.title = '从该存档开新分支（旧会话不动）';
        btnF.onclick = (ev) => { ev.stopPropagation(); stForkSave(s.saveId); };
        const btnD = document.createElement('button');
        btnD.type = 'button'; btnD.className = 'ghost sm'; btnD.textContent = '删';
        btnD.onclick = (ev) => { ev.stopPropagation(); stDeleteSave(s.saveId); };
        actions.appendChild(btnR); actions.appendChild(btnF); actions.appendChild(btnD);
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

async function stForkSave(saveId) {
  if (!tavernSession || !saveId) return;
  const label = await showPrompt('新分支名称（可空）', { value: '' }) || '';
  try {
    const r = await stApi('/sessions/' + encodeURIComponent(tavernSession.sessionId) + '/saves/' + encodeURIComponent(saveId) + '/fork', {
      method: 'POST', body: JSON.stringify({ label: label.trim() || undefined })
    });
    const ns = r && r.session;
    await stLoadSessions();
    await stLoadSaves();
    if (ns && ns.sessionId) {
      showToast('已分叉到新会话', 'success');
      stStatus('新分支会话 ' + ns.sessionId + ' · ' + stTurnLabel(ns.turn || 0));
    } else showToast('已分叉', 'success');
  } catch (e) { stStatus('分叉失败：' + e.message); }
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

async function stExportPackZip() {
  if (!tavernPack || !tavernPack.id) { stStatus('先选择 Pack'); return; }
  try {
    const headers = {};
    if (__authToken()) { headers.Authorization = 'Bearer ' + __authToken(); headers['X-Mobile-Token'] = __authToken(); }
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

function stPackChars(pack) {
  return (pack && Array.isArray(pack.characters)) ? pack.characters : [];
}

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

const ST_CHAR_ROLE_MAP = {
  protagonist: '主角', main: '主角', lead: '主角',
  antagonist: '反派', villain: '反派',
  supporting: '配角', side: '配角', secondary: '配角',
  narrator: '旁白',
  player: '玩家', reader: '玩家',
  npc: 'NPC', extra: '龙套',
};

const ST_CHAR_FIELDS = [
  { key: 'personality',        label: '性格',       icon: 'personality' },
  { key: 'speechStyle',        label: '说话风格',   icon: 'speechStyle' },
  { key: 'motivation',         label: '动机',       icon: 'motivation' },
  { key: 'relationships',      label: '关系',       icon: 'relationships',  array: true },
  { key: 'mentalModels',       label: '心智模型',   icon: 'mentalModels',   array: true },
  { key: 'decisionHeuristics', label: '决策启发式', icon: 'decisionHeuristics', array: true },
  { key: 'beliefs',            label: '信念',       icon: 'beliefs',        array: true },
];

const ST_CHAR_ICONS_SVG = {
  personality:      '<circle cx="12" cy="12" r="10"/><path d="M8 14s1.5 2 4 2 4-2 4-2"/><line x1="9" y1="9" x2="9.01" y2="9"/><line x1="15" y1="9" x2="15.01" y2="9"/>',
  speechStyle:      '<path d="M7.9 20A9 9 0 1 0 4 16.1L2 22Z"/>',
  motivation:       '<circle cx="12" cy="12" r="10"/><circle cx="12" cy="12" r="6"/><circle cx="12" cy="12" r="2"/>',
  relationships:    '<path d="M10 13a5 5 0 0 0 7.54.54l3-3a5 5 0 0 0-7.07-7.07l-1.72 1.71"/><path d="M14 11a5 5 0 0 0-7.54-.54l-3 3a5 5 0 0 0 7.07 7.07l1.71-1.71"/>',
  mentalModels:     '<path d="M12 2a10 10 0 1 0 0 20 10 10 0 0 0 0-20Z"/><path d="M12 16v-4"/><path d="M12 8h.01"/>',
  decisionHeuristics: '<path d="m16 16-4-4-4 4"/><path d="M12 12v9"/><path d="m8 8 4 4 4-4"/>',
  beliefs:          '<path d="M19 14c1.49-1.46 3-3.21 3-5.5A5.5 5.5 0 0 0 16.5 3c-1.76 0-3 .5-4.5 2-1.5-1.5-2.74-2-4.5-2A5.5 5.5 0 0 0 2 8.5c0 2.3 1.5 4.05 3 5.5l7 7Z"/>',
  chevron:          '<path d="m9 18 6-6-6-6"/>',
};

function stCharIconSvg(name, size) {
  var paths = ST_CHAR_ICONS_SVG[name] || ST_CHAR_ICONS_SVG.personality;
  var s = size || 12;
  return '<svg aria-hidden="true" xmlns="http://www.w3.org/2000/svg" width="' + s + '" height="' + s + '" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">' + paths + '</svg>';
}

function stCharRoleLabel(role) {
  var r = String(role || '').trim().toLowerCase();
  return ST_CHAR_ROLE_MAP[r] || r || '角色';
}

function stCharFilterPack(chars) {
  if (!Array.isArray(chars)) return [];
  var junkRe = /^(露出|眼角|换鞋|随口|轻声|低头|抬起|转身|伸手|走过去|看向|听见|突然)/;
  return chars.filter(function (c) {
    if (!c) return false;
    var role = String(c.role || '').toLowerCase();
    var n = String(c.name || '').trim();
    if (role.includes('narrator') || role.includes('player')) return false;
    if (n === '旁白' || n === '读者' || n === '玩家') return false;
    if (!n || n.length < 2 || n.length > 12) return false;
    if (junkRe.test(n)) return false;
    if (/[的了着在把被]/.test(n)) return false;
    return true;
  });
}

function stCharRenderCard(ch) {
  var name = escapeHtml(String(ch.name || '').trim() || '???');
  var role = stCharRoleLabel(ch.role);
  var initial = name.charAt(0);

  // Avatar
  var avatarHtml;
  if (ch.avatar && String(ch.avatar).trim()) {
    avatarHtml = '<div class="st-char-avatar"><img src="' + escapeHtml(ch.avatar) + '" alt="' + name + '" loading="lazy" /></div>';
  } else {
    avatarHtml = '<div class="st-char-avatar">' + initial + '</div>';
  }

  // Build field rows
  var fieldsHtml = '';
  for (var fi = 0; fi < ST_CHAR_FIELDS.length; fi++) {
    var f = ST_CHAR_FIELDS[fi];
    var val = ch[f.key];
    if (val === undefined || val === null || val === '') continue;
    if (Array.isArray(val) && val.length === 0) continue;

    fieldsHtml += '<div class="st-char-field">';
    fieldsHtml += '<div class="st-char-field-label">' + stCharIconSvg(f.icon) + ' ' + f.label + '</div>';

    if (f.array && Array.isArray(val)) {
      var items = val.slice(0, 3);
      var hasMore = val.length > 3;
      fieldsHtml += '<div class="st-char-field-list">';
      for (var ai = 0; ai < items.length; ai++) {
        fieldsHtml += '<div class="st-char-field-item">' + escapeHtml(String(items[ai])) + '</div>';
      }
      if (hasMore) {
        fieldsHtml += '<div class="st-char-field-item st-char-collapsed" style="display:none">';
        for (var bi = 3; bi < val.length; bi++) {
          fieldsHtml += escapeHtml(String(val[bi])) + (bi < val.length - 1 ? '<br>' : '');
        }
        fieldsHtml += '</div>';
        fieldsHtml += '<button type="button" class="st-char-collapse-btn" data-char-toggle="collapsed" data-count="' + (val.length - 3) + '">' + stCharIconSvg('chevron', 10) + ' +' + (val.length - 3) + ' 更多</button>';
      }
      fieldsHtml += '</div>';
    } else {
      fieldsHtml += '<div class="st-char-field-text">' + escapeHtml(String(val)) + '</div>';
    }
    fieldsHtml += '</div>';
  }

  // Evidence refs (collapsible)
  var refs = ch.evidenceRefs;
  if (Array.isArray(refs) && refs.length > 0) {
    fieldsHtml += '<div class="st-char-evidence">';
    fieldsHtml += '<button type="button" class="st-char-evidence-toggle">' + stCharIconSvg('chevron', 10) + ' 证据出处 (' + refs.length + ')</button>';
    fieldsHtml += '<div class="st-char-evidence-body">';
    for (var ri = 0; ri < refs.length; ri++) {
      fieldsHtml += '<span class="st-char-evidence-tag">' + escapeHtml(String(refs[ri])) + '</span> ';
    }
    fieldsHtml += '</div></div>';
  }

  // SoulLink 档案（archive: {fields, personality, worldview, family, relationships, memory}）
  var arch = ch.archive;
  if (arch && typeof arch === 'object') {
    fieldsHtml += '<div class="st-char-archive">';
    fieldsHtml += '<div class="st-char-archive-title">' + stCharIconSvg('personality', 11) + ' 角色档案</div>';
    // 标量字段
    var archF = arch.fields || {};
    var scalarPairs = [['name', '姓名'], ['age', '年龄'], ['gender', '性别'], ['occupation', '职业']];
    var scalarHtml = '';
    for (var si = 0; si < scalarPairs.length; si++) {
      var sv = archF[scalarPairs[si][0]];
      if (sv === undefined || sv === null || sv === '') continue;
      scalarHtml += '<span class="st-archive-scalar">' + scalarPairs[si][1] + ': ' + escapeHtml(String(sv)) + '</span>';
    }
    if (scalarHtml) fieldsHtml += '<div class="st-archive-scalars">' + scalarHtml + '</div>';
    // 分节
    var archSections = [['personality', '性格'], ['worldview', '世界观'], ['family', '家庭'], ['relationships', '关系'], ['memory', '记忆']];
    for (var secI = 0; secI < archSections.length; secI++) {
      var secKey = archSections[secI][0];
      var secLabel = archSections[secI][1];
      var entries = Array.isArray(arch[secKey]) ? arch[secKey] : [];
      if (!entries.length) continue;
      fieldsHtml += '<div class="st-archive-section"><div class="st-archive-section-label">' + secLabel + '</div>';
      for (var eI = 0; eI < entries.length; eI++) {
        var eC = entries[eI] && entries[eI].content !== undefined ? entries[eI].content : String(entries[eI]);
        if (eC === undefined || eC === null || eC === '') continue;
        fieldsHtml += '<div class="st-archive-item">' + escapeHtml(String(eC)) + '</div>';
      }
      fieldsHtml += '</div>';
    }
    // 操作按钮
    fieldsHtml += '<div class="st-archive-actions">' +
      '<button type="button" class="st-archive-btn" data-arch-action="analyze" data-char="' + escapeHtml(ch.id) + '">' + stCharIconSvg('motivation', 10) + ' 分析</button>' +
      '<button type="button" class="st-archive-btn" data-arch-action="refine" data-char="' + escapeHtml(ch.id) + '">' + stCharIconSvg('beliefs', 10) + ' 精编</button>' +
      '</div>';
    fieldsHtml += '</div>';
  }

  // If no fields at all, show minimal card
  var bodyContent = fieldsHtml || '<div class="st-char-empty" style="padding:var(--sp-1h) 0;opacity:.6;font-size:var(--fs-xs)">暂无蒸馏数据</div>';

  return '<div class="st-char-card">' +
    '<div class="st-char-head">' + avatarHtml +
      '<div class="st-char-info"><div class="st-char-name">' + name + '</div>' +
      '<div class="st-char-role">' + escapeHtml(role) + '</div></div></div>' +
    '<div class="st-char-body">' + bodyContent + '</div></div>';
}

function stRenderCharSummary() {
  var container = $('st-char-list');
  if (!container) return;
  var pack = tavernPack; // IIFE 共享变量（_state-part.js let 声明），非 window 全局
  var chars = stCharFilterPack(pack && pack.characters);

  if (!chars.length) {
    container.innerHTML = '<div class="st-char-empty">' +
      '<svg aria-hidden="true" xmlns="http://www.w3.org/2000/svg" width="32" height="32" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2"/><circle cx="9" cy="7" r="4"/><path d="M22 21v-2a4 4 0 0 0-3-3.87"/><path d="M16 3.128a4 4 0 0 1 0 7.744"/></svg>' +
      '<span>暂无角色数据</span></div>';
    return;
  }

  var html = '';
  for (var i = 0; i < chars.length; i++) {
    html += stCharRenderCard(chars[i]);
  }
  container.innerHTML = html;

  // Bind collapse/expand toggles
  container.querySelectorAll('.st-char-collapse-btn').forEach(function (btn) {
    btn.onclick = function () {
      var target = btn.parentElement.querySelector('.st-char-collapsed');
      if (!target) return;
      var expanded = target.style.display !== 'none';
      target.style.display = expanded ? 'none' : '';
      btn.classList.toggle('expanded', !expanded);
      var cnt = btn.getAttribute('data-count') || '?';
      btn.innerHTML = stCharIconSvg('chevron', 10) + ' ' + (expanded ? ('+' + cnt + ' 更多') : '收起');
    };
  });

  // Bind evidence toggles
  container.querySelectorAll('.st-char-evidence-toggle').forEach(function (btn) {
    btn.onclick = function () {
      var body = btn.nextElementSibling;
      if (!body) return;
      var showing = body.classList.toggle('show');
      btn.classList.toggle('expanded', showing);
      var svgHtml = stCharIconSvg('chevron', 10);
      var tag = btn.closest('.st-char-evidence');
      var count = tag ? (tag.querySelectorAll('.st-char-evidence-tag').length) : 0;
      btn.innerHTML = svgHtml + ' 证据出处 (' + count + ')' + (showing ? ' ▾' : '');
    };
  });

  // Bind SoulLink 档案 分析/精编 buttons
  container.querySelectorAll('.st-archive-btn').forEach(function (btn) {
    btn.onclick = function () {
      var action = btn.getAttribute('data-arch-action');
      var charId = btn.getAttribute('data-char');
      if (!action || !charId || !tavernPack || !tavernPack.id || typeof stApi !== 'function') return;
      var packId = tavernPack.id;
      var body = { characterId: charId };
      // analyze 需要近期对话：优先当前 URL 会话（hash 里的 session id 最可靠；
      // sessionId 变量可能是旧 partner-session，不能信）
      if (action === 'analyze') {
        var sid = ((location.hash.match(/session\/([^/?#]+)/) || [])[1] || '') ||
          (typeof sessionId === 'string' ? sessionId : '');
        if (sid) body.sessionId = sid;
      }
      btn.disabled = true;
      var old = btn.innerHTML;
      btn.innerHTML = (action === 'analyze' ? '分析中…' : '精编中…');
      stApi('/packs/' + encodeURIComponent(packId) + '/archive/' + action, {
        method: 'POST',
        body: JSON.stringify(body),
      }).then(function (res) {
        if (res && res.changes && res.changes.length) {
          stStatus('档案' + (action === 'analyze' ? '分析' : '精编') + '完成：' + res.changes.length + ' 处变更');
        } else if (res && res.ok) {
          stStatus('档案' + (action === 'analyze' ? '分析' : '精编') + '完成：无变更');
        } else {
          stStatus('档案' + (action === 'analyze' ? '分析' : '精编') + '失败');
        }
        return stRefreshCharSummary();
      }).catch(function () {
        stStatus('档案' + (action === 'analyze' ? '分析' : '精编') + '请求失败');
      }).finally(function () {
        btn.disabled = false;
        btn.innerHTML = old;
      });
    };
  });

}

async function stRefreshCharSummary() {
  // 精简版 pack 的 characters 只有 id/name（列表接口），蒸馏字段(personality 等)缺失时
  // 重拉 full pack 再渲染（stEnsureFullPack 的缓存分支会命中精简版，绕过它直接走 full）
  var pack = tavernPack;
  var chars = stCharFilterPack(pack && pack.characters);
  var needFull = chars.length > 0 && !chars.some(function (c) {
    return c.personality || c.motivation || c.beliefs || (c.relationships && c.relationships.length) || (c.mentalModels && c.mentalModels.length);
  });
  if (needFull && pack && pack.id && typeof stApi === 'function') {
    try {
      var full = await stApi('/packs/' + encodeURIComponent(pack.id));
      if (full && Array.isArray(full.characters)) tavernPack = full;
    } catch (_) { /* keep existing */ }
  }
  stRenderCharSummary();
}

function stImmerseBgUrl() {
  const msgs = (tavernSession && tavernSession.messages) || [];
  // 回溯最近一条有发言人前缀的 assistant 消息
  let name = null;
  for (let i = msgs.length - 1; i >= 0; i--) {
    const m = msgs[i];
    if (m && m.role && m.role !== 'user') {
      const s = stSpeakerNameOf(m);
      if (s) { name = s; break; }
    }
  }
  const pack = tavernPack;
  if (!pack || !Array.isArray(pack.characters)) return null;
  // 有明确发言者：优先其 avatar
  if (name) {
    const cid = stCharIdOf(name);
    const ch = pack.characters.find(function (c) { return c && String(c.id) === String(cid); });
    if (ch) {
      const av = (ch.avatar && String(ch.avatar).trim()) ? String(ch.avatar) : null;
      if (av) return av;
      try {
        const u = stSpriteOf(name);
        if (u) return u;
      } catch (_) {}
    }
  }
  // 叙述体/无发言者：fallback 到在场角色中第一个有 avatar 的（保持背景稳定）
  // 优先 presentCharacterIds，其次 cast 顺序
  const present = (tavernSession && tavernSession.presentCharacterIds) || [];
  const ordered = (present.length ? present : []).concat(pack.characters.map(function (c) { return c.id; }));
  for (let i = 0; i < ordered.length; i++) {
    const cid2 = String(ordered[i]);
    const ch2 = pack.characters.find(function (c) { return c && String(c.id) === cid2; });
    if (ch2 && ch2.avatar && String(ch2.avatar).trim()) return String(ch2.avatar);
  }
  // 最后兜底：cast 顺序第一个有 avatar 的角色
  for (let j = 0; j < pack.characters.length; j++) {
    const c3 = pack.characters[j];
    if (c3 && c3.avatar && String(c3.avatar).trim()) return String(c3.avatar);
  }
  return null;
}

function stRefreshImmerseBg() {
  const stage = document.getElementById('st-view-play');
  if (!stage) return;
  try {
    const url = stImmerseBgUrl();
    if (url) {
      stage.classList.add('has-char-bg');
      stage.style.setProperty('--char-bg-image', 'url("' + String(url).replace(/"/g, '\\"') + '")');
    } else {
      stage.classList.remove('has-char-bg');
      stage.style.removeProperty('--char-bg-image');
    }
  } catch (_) {
    stage.classList.remove('has-char-bg');
    stage.style.removeProperty('--char-bg-image');
  }
}

/* ================= initTavernUI: former top-level DOM/window wiring ================= */
export function initTavernUI() {
// ---- WIRE:_tavern-core.js ----
try {
  Object.defineProperty(window, 'stNavFrom', {
    get: () => stNavFrom,
    set: (v) => { stNavFrom = v; },
    configurable: true,
  });
} catch (_) {}

// 弹层是否真的可见（未被宿主 tab 面板隐藏）
// ---- WIRE:_tavern-core.js ----
window.stGoBack = stGoBack;
// ---- WIRE:_tavern-core.js ----
window.addEventListener('popstate', () => {
  if (stGoBack(true)) {
    __setSuppressHashWrite(true);
    setTimeout(() => { __setSuppressHashWrite(false); }, 50);
  }
});
// ---- WIRE:_tavern-core.js ----
if (stStageBtn) stStageBtn.onclick = stStageOpen;
// ---- WIRE:_tavern-core.js ----
try { window.stStatus = stStatus; } catch (_) {}
// ---- WIRE:_tavern-session.js ----
if ($('st-recall-refresh')) {
  $('st-recall-refresh').onclick = (e) => {
    e.preventDefault();
    stRefreshRecallSemantic();
  };
}
// ---- WIRE:_tavern-session.js ----
try { if (window.stRefreshImmerseBg) stRefreshImmerseBg(); } catch (_) {}

// 剧情助手工具按钮：重生成 / 回退
// ---- WIRE:_tavern-session.js ----
if ($('st-assist-reroll')) $('st-assist-reroll').onclick = (e) => { e.preventDefault(); stRerollLast(); }
// ---- WIRE:_tavern-session.js ----
if ($('st-assist-rewind')) $('st-assist-rewind').onclick = (e) => { e.preventDefault(); stRewindOne(); }
// ---- WIRE:_tavern-session.js ----
try {
window.stOpenAssistModal = stOpenAssistModal;
window.stFocusAssistInput = stFocusAssistInput;
} catch (_) {}
// ---- WIRE:_tavern-send.js ----
if ($('st-adult-ok')) { $('st-adult-ok').onclick = async () => { await setAdultOk(); $('st-adult-banner').classList.add('hidden'); const l = $('st-layout'); if (l) l.classList.remove('st-gated'); stRefresh(); }; }
// ---- WIRE:_tavern-send.js ----
if ($('st-new-session')) { $('st-new-session').onclick = () => stOpenWizard('P3', 'story-entry'); }
// ---- WIRE:_tavern-send.js ----
if ($('st-drawer-new-session')) { $('st-drawer-new-session').onclick = () => stOpenWizard('P3', 'story-entry'); }

// ─── Bookshelf ↔ Story Tavern bridge ───────────────────────────────────────
// ---- WIRE:_tavern-send.js ----
window.stSwipe = stSwipe;
// ---- WIRE:_tavern-send.js ----
window.stSwipePicker = stSwipePicker;
// ---- WIRE:_tavern-send.js ----
window.stEditMessage = stEditMessage;
// ---- WIRE:_tavern-send.js ----
window.stDeleteMessage = stDeleteMessage;
// ---- WIRE:_tavern-send.js ----
window.stPartialEdit = stPartialEdit;
// ---- WIRE:_tavern-send.js ----
window.stToggleBookmark = stToggleBookmark;
// ---- WIRE:_tavern-send.js ----
window.stRenderBookmarks = stRenderBookmarks;
// ---- WIRE:_tavern-send.js ----
if (document.readyState === 'loading') {
  document.addEventListener('DOMContentLoaded', stWireDisplaySettings);
} else {
  stWireDisplaySettings();
}
// ---- WIRE:_tavern-send.js ----
window.stApplyChatWidth = stApplyChatWidth;
// ---- WIRE:_tavern-send.js ----
window.stApplyBubbleStyle = stApplyBubbleStyle;
// ---- WIRE:_tavern-packmgmt.js ----
if ($('shelf-refresh')) $('shelf-refresh').onclick = () => loadBookshelf();
// ---- WIRE:_tavern-packmgmt.js ----
if ($('shelf-chat-publish')) $('shelf-chat-publish').onclick = () => shelfPublishChat();
// ---- WIRE:_tavern-packmgmt.js ----
if ($('shelf-sched-save')) $('shelf-sched-save').onclick = () => shelfSaveSchedule();
// ---- WIRE:_tavern-packmgmt.js ----
if ($('shelf-sched-run')) $('shelf-sched-run').onclick = () => shelfRunScheduleNow();
// ---- WIRE:_tavern-packmgmt.js ----
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
// ---- WIRE:_tavern-packmgmt.js ----
if ($('reader-back')) $('reader-back').onclick = closeShelfReader;
// ---- WIRE:_tavern-packmgmt.js ----
if ($('reader-to-pack')) {
  $('reader-to-pack').onclick = () => {
    if (!shelfActiveSlug) return;
    const n = shelfNovels.find((x) => x.slug === shelfActiveSlug);
    shelfPromoteToPack(shelfActiveSlug, n && n.title);
  };
}
// ---- WIRE:_tavern-packmgmt.js ----
if ($('reader-export')) {
  $('reader-export').onclick = () => {
    if (!shelfActiveSlug) return;
    const n = shelfNovels.find((x) => x.slug === shelfActiveSlug);
    shelfExport(shelfActiveSlug, n && n.title);
  };
}

// load when switching to bookshelf tab
// ---- WIRE:_tavern-packmgmt.js ----
document.querySelectorAll('.tab[data-tab="bookshelf"], [data-tab="bookshelf"]').forEach((btn) => {
  btn.addEventListener('click', () => { setTimeout(loadBookshelf, 0); });
});
// ---- WIRE:_tavern-packmgmt.js ----
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
// ---- WIRE:_tavern-packmgmt.js ----
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
// ---- WIRE:_tavern-packmgmt.js ----
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
// ---- WIRE:_tavern-packmgmt.js ----
if ($('home-continue-btn')) {
  $('home-continue-btn').addEventListener('click', () => {
    const has = (tavernSessions && tavernSessions.length) || 0;
    if (!has) {
      if (typeof switchTab === 'function') switchTab('chat');
    }
  });
}
// ---- WIRE:_tavern-packmgmt.js ----
document.querySelectorAll('#st-entry-cards .st-entry-card').forEach((card) => { card.onclick = () => stOpenWizard(card.dataset.playable, 'story-entry'); });
// ---- WIRE:_tavern-packmgmt.js ----
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
// ---- WIRE:_tavern-packmgmt.js ----
if ($('st-pack-detail-back')) {
  $('st-pack-detail-back').onclick = () => {
    const listview = $('st-packs-listview');
    const packDetail = $('st-view-pack');
    if (listview) listview.classList.remove('hidden');
    if (packDetail) packDetail.classList.add('hidden');
    stStatus('档案馆 — 选择剧本包');
  };
}
// ---- WIRE:_tavern-packmgmt.js ----
if ($('st-pack-detail-compass')) {
  $('st-pack-detail-compass').onclick = () => {
    if (typeof switchTab === 'function') switchTab('works');
    setTimeout(() => {
      if (typeof switchAzView === 'function') switchAzView('dual-agent');
      if (typeof showToast === 'function') showToast('世界线/罗盘工具在作者区双 Agent / 关系图中');
    }, 80);
  };
}
// ---- WIRE:_tavern-packmgmt.js ----
$('st-wizard-create').onclick = stCreateSession;
// ---- WIRE:_tavern-packmgmt.js ----
$('st-wizard-cancel').onclick = () => stCancelWizard(false);
// ---- WIRE:_tavern-packmgmt.js ----
$('st-wizard-role').onchange = stWizardToggleRole;
// ---- WIRE:_tavern-packmgmt.js ----
$('st-wizard-pack').onchange = stWizardToggleRole;
// ---- WIRE:_tavern-packmgmt.js ----
$('st-composer').onsubmit = (e) => { e.preventDefault(); stSend($('st-input').value); }
// ---- WIRE:_tavern-packmgmt.js ----
$('st-input').addEventListener('keydown', (e) => { if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); stSend($('st-input').value); } });
// ---- WIRE:_tavern-packmgmt.js ----
$('st-stop').onclick = stStop;
// ---- WIRE:_tavern-packmgmt.js ----
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
// ---- WIRE:_tavern-packmgmt.js ----
document.querySelectorAll('#st-wand-menu button').forEach((el) => {
  el.addEventListener('click', () => {
    if (el.id === 'st-vessel-toggle' || el.id === 'st-rot-toggle' || el.closest('#st-vessel-picker') || el.closest('.st-select-wrap')) return;
    const menu = $('st-wand-menu');
    const btn = $('st-wand-btn');
    if (menu && btn) { menu.classList.add('hidden'); btn.setAttribute('aria-expanded', 'false'); }
  });
});
// ---- WIRE:_tavern-packmgmt.js ----
document.querySelectorAll('#st-wand-menu select').forEach((el) => {
  el.addEventListener('change', () => {
    const menu = $('st-wand-menu');
    const btn = $('st-wand-btn');
    if (menu && btn) { menu.classList.add('hidden'); btn.setAttribute('aria-expanded', 'false'); }
  });
});
// ---- WIRE:_tavern-packmgmt.js ----
document.addEventListener('click', (e) => {
  const menu = $('st-wand-menu');
  const btn = $('st-wand-btn');
  if (!menu || menu.classList.contains('hidden')) return;
  if (btn && btn.contains(e.target)) return;
  if (menu.contains(e.target)) return;
  menu.classList.add('hidden');
  if (btn) btn.setAttribute('aria-expanded', 'false');
});
// ---- WIRE:_tavern-packmgmt.js ----
['st-magic-assist', 'st-magic-assist-btn'].forEach((mid) => {
  const btn = document.getElementById(mid);
  if (!btn) return;
  btn.onclick = (e) => {
    e.preventDefault();
    if (typeof stOpenAssistModal === 'function') stOpenAssistModal();
  };
});
// ---- WIRE:_tavern-packmgmt.js ----
if ($('st-visual-btn')) $('st-visual-btn').onclick = (e) => { e.preventDefault(); stOpenVisualModal(); }
// ---- WIRE:_tavern-packmgmt.js ----
if ($('st-visual-close')) $('st-visual-close').onclick = (e) => { e.preventDefault(); stCloseVisualModal(); }
// ---- WIRE:_tavern-packmgmt.js ----
if ($('st-visual-gen')) $('st-visual-gen').onclick = (e) => { e.preventDefault(); stGenVisual(); }
// ---- WIRE:_tavern-packmgmt.js ----
if ($('st-assist-close')) $('st-assist-close').onclick = (e) => { e.preventDefault(); stCloseAssistModal(); }
// ---- WIRE:_tavern-packmgmt.js ----
if ($('st-assist-send')) $('st-assist-send').onclick = (e) => { e.preventDefault(); stSendAssist(); }
// ---- WIRE:_tavern-packmgmt.js ----
if ($('st-assist-input')) $('st-assist-input').addEventListener('keydown', (e) => { if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); stSendAssist(); } });
// ---- WIRE:_tavern-packmgmt.js ----
if ($('st-continue')) $('st-continue').onclick = (e) => { e.preventDefault(); stContinue(); }
// ---- WIRE:_tavern-packmgmt.js ----
if ($('st-retry')) $('st-retry').onclick = (e) => { e.preventDefault(); stRetry().catch((err) => stStatus('重试失败：' + ((err && err.message) || err))); }
// ---- WIRE:_tavern-packmgmt.js ----
if ($('st-image-btn')) $('st-image-btn').onclick = (e) => { e.preventDefault(); stGenerateImage().catch((err) => stStatus('生图失败：' + ((err && err.message) || err))); }
// ---- WIRE:_tavern-packmgmt.js ----
if ($('st-sprite-btn')) $('st-sprite-btn').onclick = (e) => { e.preventDefault(); stGenerateSprite().catch((err) => stStatus('生成立绘失败：' + ((err && err.message) || err))); }
// ---- WIRE:_tavern-packmgmt.js ----
if ($('st-tts-btn')) $('st-tts-btn').onclick = (e) => { e.preventDefault(); stSpeak().catch((err) => stStatus('朗读失败：' + ((err && err.message) || err))); }
// ---- WIRE:_tavern-packmgmt.js ----
if (stAsrBtn) stAsrBtn.onclick = (e) => { e.preventDefault(); stToggleRecording().catch(() => {}); stSyncRecBtn(); }
// ---- WIRE:_tavern-packmgmt.js ----
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
// ---- WIRE:_tavern-packmgmt.js ----
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
// ---- WIRE:_tavern-packmgmt.js ----
if ($('st-tts-pause')) $('st-tts-pause').onclick = (e) => { e.preventDefault(); stTtsPauseToggle(); }
// ---- WIRE:_tavern-packmgmt.js ----
if ($('st-tts-stop')) $('st-tts-stop').onclick = (e) => { e.preventDefault(); stTtsStop(); }
// ---- WIRE:_tavern-packmgmt.js ----
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
}
// ---- WIRE:_tavern-packmgmt.js ----
stImgSelOpts.forEach((o) => {
  o.onclick = (e) => { e.preventDefault(); e.stopPropagation();
    const btn = $('st-image-channel');
    if (btn) { btn.dataset.value = o.dataset.channel; btn.textContent = o.textContent; btn.setAttribute('aria-expanded', 'false'); }
    const list = $('st-image-channel-list');
    if (list) list.classList.add('hidden');
    list.querySelectorAll('.st-select-opt').forEach((x) => x.classList.toggle('active', x === o));
  };
});
// ---- WIRE:_tavern-packmgmt.js ----
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
// ---- WIRE:_tavern-packmgmt.js ----
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
    if (!tavernSession) return;
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
      if (!tavernSession || !tavernSession.sessionId) {
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
// ---- WIRE:_tavern-packmgmt.js ----
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
      if (!tavernSession || !tavernSession.sessionId) {
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
// ---- WIRE:_tavern-packmgmt.js ----
try { stBindImmChrome(); } catch (_) {}


// ----- ST-4 Pack production / novel import helpers -----
// ---- WIRE:_tavern-lore.js ----
if ($('st-novel-file')) {
  $('st-novel-file').onchange = async (e) => {
    const file = e.target.files[0]; if (!file) return;
    const progress = $('st-import-progress');
    progress.classList.remove('hidden');
    try {
      const saved = await stImportNovel(file, file.name.replace(/\.[^.]+$/, ''));
      await stLoadPacks();
      stStatus('已导入 Pack：' + saved.title + '，共' + (saved.chapters || []).length + '章');
      e.target.value = '';
    } catch (err) {
      stStatus('导入失败：' + err.message);
    } finally {
      progress.classList.add('hidden');
    }
  };
}
// ---- WIRE:_tavern-lore.js ----
if ($('st-pack-create')) $('st-pack-create').onclick = () => $('st-pack-editor').classList.remove('hidden');
// ---- WIRE:_tavern-lore.js ----
if ($('st-side-expand-all')) {
  $('st-side-expand-all').onclick = () => {
    const heads = document.querySelectorAll('#st-pack-list .st-pack-group-head, #st-session-list .st-session-group-head');
    const anyClosed = Array.from(heads).some(h => !h.classList.contains('open'));
    heads.forEach((h) => {
      const open = anyClosed; // expand all if any is closed, else collapse all
      h.classList.toggle('open', open);
      const g = h.parentElement;
      if (g) g.classList.toggle('open', open);
      h.setAttribute('aria-expanded', open ? 'true' : 'false');
    });
    const btn = $('st-side-expand-all');
    if (btn) btn.textContent = anyClosed ? '全部收起' : '全部展开';
  };
}
// ---- WIRE:_tavern-lore.js ----
if ($('st-pack-cancel')) $('st-pack-cancel').onclick = () => $('st-pack-editor').classList.add('hidden');
// ---- WIRE:_tavern-lore.js ----
if ($('st-pack-save')) {
  $('st-pack-save').onclick = async () => {
    await stCreateEmptyPack();
    $('st-pack-editor').classList.add('hidden');
    $('st-pack-title').value = '';
    await stLoadPacks();
    stStatus('空 Pack 创建成功');
  };
}
// ---- WIRE:_tavern-lore.js ----
if ($('st-chapter-edit')) {
  $('st-chapter-edit').onclick = () => {
    if (!tavernPack || !tavernPack.chapters || !tavernPack.chapters[0]) return;
    // toggle an inline editor
    const view = $('st-chapter-view');
    const pre = view.querySelector('pre');
    const editing = view.dataset.editing === '1';
    view.dataset.editing = editing ? '' : '1';
    if (editing) {
      // save
      const chId = view.dataset.chapterId;
      const content = (view.querySelector('textarea')?.value || '').trim();
      stApi('/packs/' + encodeURIComponent(tavernPack.id) + '/chapters/' + encodeURIComponent('chapters/' + chId + '.md'), { method: 'PUT', body: JSON.stringify({ content }) })
        .then(() => { stShowChapter(tavernPack.id, chId); stStatus('章节已保存'); })
        .catch(err => stStatus('保存失败：' + err.message));
    } else {
      // turn pre into textarea
      const txt = document.createElement('textarea');
      txt.rows = 10; txt.style.flex = '1';
      const oldText = pre.textContent; pre.innerHTML = ''; pre.appendChild(txt); txt.value = oldText;
    }
  };
}
// ---- WIRE:_tavern-side.js ----
if ($('st-lore-add')) $('st-lore-add').onclick = () => stOpenLoreEditor(-1);
// ---- WIRE:_tavern-side.js ----
if ($('st-lore-save')) $('st-lore-save').onclick = () => stSaveLore().catch(e => stStatus('Lore 保存失败：' + e.message));
// ---- WIRE:_tavern-side.js ----
if ($('st-lore-cancel')) $('st-lore-cancel').onclick = () => { $('st-lore-editor').classList.add('hidden'); stLoreEditIdx = -1; stRenderLore(); }
// ---- WIRE:_tavern-side.js ----
if ($('st-lore-del')) $('st-lore-del').onclick = () => stDeleteLore().catch(e => stStatus('Lore 删除失败：' + e.message));
// ---- WIRE:_tavern-side.js ----
if ($('st-mode-mainline')) $('st-mode-mainline').onclick = () => stSetPlayMode('mainline');
// ---- WIRE:_tavern-side.js ----
if ($('st-mode-side')) $('st-mode-side').onclick = () => stSetPlayMode('side');
// ---- WIRE:_tavern-side.js ----
if ($('st-mode-free')) $('st-mode-free').onclick = () => stSetPlayMode('free');
// ---- WIRE:_tavern-side.js ----
if ($('st-side-panel-close')) $('st-side-panel-close').onclick = () => stCloseSidePanel();
// ---- WIRE:_tavern-side.js ----
if ($('st-save-create')) $('st-save-create').onclick = () => stCreateSave();
// ---- WIRE:_tavern-side.js ----
if ($('st-drawer-save-create')) $('st-drawer-save-create').onclick = () => stCreateSave();
// ---- WIRE:_tavern-side.js ----
if ($('st-pack-export')) $('st-pack-export').onclick = () => stExportPackZip();
// ---- WIRE:_tavern-side.js ----
if ($('st-zip-file')) {
  $('st-zip-file').onchange = async (e) => {
    const file = e.target.files && e.target.files[0]; if (!file) return;
    try { await stImportPackZip(file); e.target.value = ''; }
    catch (err) { stStatus('ZIP 导入失败：' + err.message); }
  };
}
// ---- WIRE:_tavern-side.js ----
if (_rotBtn) _rotBtn.onclick = () => {
  if (!tavernSession) return;
  const next = !_rotBtn.classList.contains('active');
  _rotBtn.classList.toggle('active', next);
  _rotBtn.setAttribute('aria-pressed', next ? 'true' : 'false');
  stApi('/sessions/' + encodeURIComponent(tavernSession.sessionId) + '/focus', {
    method: 'POST', body: JSON.stringify({ speakerRotation: next, characterId: tavernSession.focusCharacterId || undefined })
  }).then(s => { tavernSession = s; stRenderFocusBar(); stFillVesselSelect(); }).catch(e => stStatus('轮流设置失败：' + e.message));
}
// ---- WIRE:_tavern-side.js ----
if (vToggle) vToggle.onclick = () => {
  if (!tavernSession) return;
  const picker = $('st-vessel-picker');
  if (!picker) return;
  const opening = picker.classList.contains('hidden');
  picker.classList.toggle('hidden', !opening);
  if (opening) stFillVesselSelect();
}
// ---- WIRE:_tavern-side.js ----
if ($('st-node-add')) $('st-node-add').onclick = () => stOpenNodeEditor(null);
// ---- WIRE:_tavern-side.js ----
if ($('st-node-save')) $('st-node-save').onclick = () => stSaveNode();
// ---- WIRE:_tavern-side.js ----
if ($('st-node-cancel')) $('st-node-cancel').onclick = () => { $('st-node-editor').classList.add('hidden'); stNodeEditId = null; stRenderNodes(); }
// ---- WIRE:_tavern-side.js ----
if ($('st-node-del')) $('st-node-del').onclick = () => stDeleteNode();
// ---- WIRE:_tavern-bg-immerse.js ----
window.stRefreshImmerseBg = stRefreshImmerseBg;
// ---- WIRE:_tavern-bg-immerse.js ----
window.stImmerseBgUrl = stImmerseBgUrl;
}

/* ===== exports consumed by remaining closure parts (Mechanism Y: converted[] import line) ===== */
export { stApi, stStatus, stGoBack, stSwitchView, stDisplayTitle, stHasOpenOverlay, stBindImmChrome,
         stRefresh, stLoadPacks, stLoadSessions, stLoadSaves, stLoadSession, stRefreshCharSummary,
         stRenderContinueCard, renderHomeRecent, loadBookshelf };
