/**
 * Kaleido — shared utilities
 */

export const TOKEN_KEY = 'kaleido_token';
export const USER_KEY = 'kaleido_user';
export const SID_KEY = 'kaleido_session_id';
export const STORY_SID_KEY = 'kaleido_story_session_id';
export const API_BASE_KEY = 'kaleido_api_base';
export const STYLE_PRESET_KEY = 'kaleido_style_preset';
export const STYLE_PRESET_PREFIX = 'kaleido_style_';
export const ADULT_OK_KEY = 'kaleido_st_adult_ok';
export const TAVERN_SID_KEY = 'kaleido_tavern_session_id';
export const ST_READPOS_PREFIX = 'kaleido_st_readpos_v1:';
export const APPEARANCE_KEY = 'kaleido_appearance_v1';
export const DEFAULT_REMOTE = 'https://kaleido.example.com';

export const $ = (id) => document.getElementById(id);

/** zh-CN friendly datetime */
export function formatDateTime(value) {
  try {
    const d = value instanceof Date ? value : new Date(value || 0);
    if (Number.isNaN(d.getTime()) || !d.getTime()) return '';
    return d.toLocaleString('zh-CN', {
      year: 'numeric', month: '2-digit', day: '2-digit',
      hour: '2-digit', minute: '2-digit', hour12: false,
    });
  } catch (_) {
    try { return new Date(value || 0).toLocaleString('zh-CN'); } catch (e2) { return ''; }
  }
}

/** Humanize session titles */
export function displayTitle(raw, fallback) {
  const t = String(raw == null ? '' : raw).trim();
  if (!t) return fallback || '未命名会话';
  if (/^untitled(\s+agent)?(\s+session)?$/i.test(t)) return fallback || '未命名会话';
  if (/^untitled\b/i.test(t)) return t.replace(/^untitled\b/i, '未命名').trim() || (fallback || '未命名会话');
  return t;
}

/** Shorten long technical ids */
export function shortId(id) {
  const s = String(id || '');
  if (s.length <= 18) return s;
  return s.slice(0, 8) + '…' + s.slice(-6);
}

export function uid(prefix) {
  return prefix + '-' + Math.random().toString(36).slice(2, 10) + Date.now().toString(36);
}

export function isCapacitor() {
  try {
    return !!(window.Capacitor && (window.Capacitor.isNativePlatform
      ? window.Capacitor.isNativePlatform()
      : true));
  } catch (_) { return false; }
}

export function normalizeBase(raw) {
  let b = (raw || '').trim().replace(/\/+$/, '');
  if (!b) return '';
  if (!/^https?:\/\//i.test(b)) b = 'https://' + b;
  return b.replace(/\/+$/, '');
}

/** [SSRF 加固 2026-08-15, 吸收 6fef9d12] 校验 API base URL：必须 http/https、
 * 非空 hostname，且禁私网/回环地址（浏览器端防线）。非法返回 false。 */
export function isValidApiUrl(str) {
  try {
    const b = normalizeBase(str);
    if (!b) return false;
    const url = new URL(b);
    if (url.protocol !== 'http:' && url.protocol !== 'https:') return false;
    if (!url.hostname) return false;
    const host = url.hostname.toLowerCase();
    if (host === 'localhost' || host === '127.0.0.1' || host === '0.0.0.0' || host === '[::1]' || host === '::1') return false;
    if (/^10\.\d+\.\d+\.\d+$/.test(host)) return false;
    if (/^192\.168\.\d+\.\d+$/.test(host)) return false;
    if (/^169\.254\.\d+\.\d+$/.test(host)) return false; // 云元数据服务 link-local
    if (/^172\.(1[6-9]|2\d|3[01])\.\d+\.\d+$/.test(host)) return false;
    return true;
  } catch (_) {
    return false;
  }
}

export function clamp(n, min, max) {
  return Math.max(min, Math.min(max, n));
}


/**
 * Strip option protocol blocks from narrative (chips render them separately).
 * NOTE: avoid /u regex flag — some WebViews throw and break the whole paint path.
 */
export function stripChoicesBlock(text) {
  if (!text) return '';
  try {
    let s = String(text);
    s = s.replace(/<choices>[\s\S]*?<\/choices>/gi, '');
    // Streaming may cut off mid-protocol: an unclosed <choices> tag would
    // otherwise render raw — drop everything from the tag onward.
    const danglingChoice = s.search(/<choices>/i);
    if (danglingChoice >= 0) s = s.slice(0, danglingChoice);
    // Fullwidth brackets 【选项】 — match by code points without needing /u
    const optMark = '\u3010\u9009\u9879\u3011'; // 【选项】
    const askMark = '\u3010\u8be2\u95ee\u3011'; // 【询问】(吸收自梨园 ask_director)
    const advMark = '\u3010\u8282\u70b9\u63a8\u8fdb\u3011'; // 【节点推进】
    let i = s.lastIndexOf(optMark);
    if (i >= 0) s = s.slice(0, i);
    i = s.lastIndexOf(askMark);
    if (i >= 0) s = s.slice(0, i);
    i = s.lastIndexOf(advMark);
    if (i >= 0) s = s.slice(0, i);
    // bare trailing JSON string array (2+ quoted items) after blank line
    s = s.replace(/\n\s*\[\s*\"[\s\S]*\]\s*$/m, '');
    return s.replace(/\n{3,}/g, '\n\n').trim();
  } catch (e) {
    console.warn('stripChoicesBlock', e);
    return String(text || '');
  }
}

/**
 * Parse an option list blob into string chips (JSON array / bullet lines / quoted strings).
 */
export function parseOptionListBlob(raw) {
  const t = String(raw || '').trim();
  if (!t) return [];
  const bracket = t.match(/\[[\s\S]*\]/);
  if (bracket) {
    try {
      const arr = JSON.parse(bracket[0]);
      if (Array.isArray(arr)) return arr.map(String).map((x) => x.trim()).filter(Boolean);
    } catch (_) {}
  }
  const out = [];
  for (const line of t.split(/\n+/)) {
    let l = line.trim();
    if (!l) continue;
    l = l.replace(/^[-•*–·]\s+/, '').replace(/^\d+[.)\u3001:：]\s*/, '').replace(/^[（(]\d+[）)]\s*/, '');
    if (l && l !== '[' && l !== ']') out.push(l);
  }
  if (!out.length) {
    const re = /"([^"\\]*(?:\\.[^"\\]*)*)"/g;
    let mm;
    while ((mm = re.exec(t)) !== null) out.push(mm[1]);
  }
  return out;
}

/**
 * Resolve clickable options from a raw assistant narrative (<choices> / 【选项】 / trailing JSON).
 */
export function parseStoryChoices(text) {
  if (!text) return [];
  try {
    const s = String(text);
    let m = s.match(/<choices>\s*([\s\S]*?)\s*<\/choices>/i);
    if (m) return parseOptionListBlob(m[1]);
    const optMark = '\u3010\u9009\u9879\u3011';
    const askMark = '\u3010\u8be2\u95ee\u3011';
    const i = s.lastIndexOf(optMark);
    if (i >= 0) return parseOptionListBlob(s.slice(i + optMark.length));
    const k = s.lastIndexOf(askMark);
    if (k >= 0) return parseOptionListBlob(s.slice(k + askMark.length));
    // bare JSON array at end
    const j = s.lastIndexOf('[');
    if (j >= 0) {
      const parsed = parseOptionListBlob(s.slice(j));
      if (parsed.length >= 2 && parsed.length <= 6) return parsed;
    }
    return [];
  } catch (e) {
    console.warn('parseStoryChoices', e);
    return [];
  }
}

/**
 * Options for last assistant message: explicit msg.options wins, else parse the body.
 */
export function resolveMessageOptions(msg) {
  if (!msg || msg.role === 'user') return [];
  if (Array.isArray(msg.options) && msg.options.length) {
    return msg.options.map(String).filter(Boolean);
  }
  return parseStoryChoices(msg.content || '');
}

/** Playable-mode labels (P1 私聊 / P2 组队 / P3 穿书 / P4 同人) — S2.10: moved
 * here from _agent-part.js (shared agent/tavern via converted[] import line). */
export const PLAYABLE_LABELS = { P1: '私聊', P2: '组队', P3: '穿书', P4: '同人' };

/** Small inline SVG icon set for tavern UI — S2.10: moved from _agent-part.js. */
export const ST_ICONS = {
    book: '<svg aria-hidden="true" xmlns="http://www.w3.org/2000/svg" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M4 19.5v-15A2.5 2.5 0 0 1 6.5 2H19a1 1 0 0 1 1 1v18a1 1 0 0 1-1 1H6.5a1 1 0 0 1 0-5H20"/></svg>',
    bookmark: '<svg aria-hidden="true" xmlns="http://www.w3.org/2000/svg" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m19 21-7-4-7 4V5a2 2 0 0 1 2-2h10a2 2 0 0 1 2 2v16z"/></svg>',
    circle: '<svg aria-hidden="true" xmlns="http://www.w3.org/2000/svg" width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/></svg>'
  };
