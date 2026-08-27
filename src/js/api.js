/**
 * Kaleido — API client
 */

import { TOKEN_KEY, API_BASE_KEY, DEFAULT_REMOTE, normalizeBase, isValidApiUrl, isCapacitor } from './utils.js';

let token = '';
let apiBaseCache = '';

export function getToken() {
  // Always read fresh from localStorage — module-level token cache goes stale
  // when login writes TOKEN_KEY directly (see _agent-part.js login handler).
  return localStorage.getItem(TOKEN_KEY) || '';
}

export function setToken(t) {
  token = t;
  if (t) localStorage.setItem(TOKEN_KEY, t);
  else localStorage.removeItem(TOKEN_KEY);
}

export function clearToken() {
  token = '';
  localStorage.removeItem(TOKEN_KEY);
}

function resolveApiBase() {
  if (apiBaseCache) return apiBaseCache;
  try {
    const stored = localStorage.getItem(API_BASE_KEY);
    if (stored != null && String(stored).trim() !== '') {
      // [SSRF 加固] 非法存储值（私网/回环/格式错）→ 清掉防静默误用
      if (isValidApiUrl(stored)) {
        apiBaseCache = normalizeBase(stored);
        return apiBaseCache;
      }
      localStorage.removeItem(API_BASE_KEY);
    }
  } catch (_) {}
  if (isCapacitor() || (typeof location !== 'undefined' && location.protocol === 'file:')) {
    apiBaseCache = DEFAULT_REMOTE;
  } else {
    apiBaseCache = '';
  }
  return apiBaseCache;
}

export function clearApiBaseCache() {
  apiBaseCache = '';
}

export function setApiBase(raw) {
  // [SSRF 加固] 非法 URL（私网/回环/格式错）拒绝写入
  if (raw && String(raw).trim() !== '' && !isValidApiUrl(raw)) {
    return normalizeBase(raw);
  }
  const b = normalizeBase(raw);
  try {
    if (!b) localStorage.removeItem(API_BASE_KEY);
    else localStorage.setItem(API_BASE_KEY, b);
  } catch (_) {}
  apiBaseCache = b;
  return b;
}

export async function api(path, opts = {}) {
  const headers = Object.assign({ 'Content-Type': 'application/json' }, opts.headers || {});
  const tok = getToken();
  if (tok) {
    headers['Authorization'] = 'Bearer ' + tok;
    headers['X-Mobile-Token'] = tok;
  }
  const res = await fetch(resolveApiBase() + path, Object.assign({}, opts, { headers, cache: 'no-store' }));
  if (!res.ok) {
    let msg = res.statusText;
    let bodyObj = null;
    try {
      bodyObj = await res.json();
      msg = (bodyObj && (bodyObj.error || bodyObj.message)) || JSON.stringify(bodyObj);
    } catch (_) {
      try { msg = await res.text(); } catch (__) {}
    }
    const err = new Error(msg || ('HTTP ' + res.status));
    err.status = res.status;
    err.body = bodyObj != null ? bodyObj : msg;
    throw err;
  }
  if (res.status === 204) return null;
  const ct = res.headers.get('content-type') || '';
  if (ct.includes('application/json')) return res.json();
  return res.text();
}

// M-3: exchange the long-lived bearer token for a short-lived one-time SSE ticket.
// Never put the raw token in the EventSource URL query — use this ticket instead.
export async function getSseTicket() {
  const tok = getToken();
  if (!tok) return '';
  const res = await fetch(resolveApiBase() + '/api/v1/auth/sse-ticket', {
    method: 'POST',
    headers: { 'Authorization': 'Bearer ' + tok, 'Content-Type': 'application/json' },
    cache: 'no-store',
  });
  if (!res.ok) {
    let msg = res.statusText;
    try { const j = await res.json(); msg = (j && (j.error || j.ticket && 'ticket')) || msg; } catch (_) {}
    throw new Error(msg || ('HTTP ' + res.status));
  }
  const j = await res.json().catch(() => ({}));
  return (j && j.ticket) || '';
}