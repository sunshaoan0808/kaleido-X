/* P1-3 S2.2: _api-part → real ESM (shell-level API helpers).
 *
 * Owns the bindings that used to live at IIFE top level in _api-part.js:
 *   - friendlyError(e)          — zh-CN humanizer for API errors
 *                                 (window.friendlyError kept: it was exported before)
 *   - apiBase()                 — resolved base for raw fetch()/EventSource calls
 *                                 (api() itself already lives in ./api.js — untouched)
 *   - showLogin()/showMain()    — login/main view switch + post-login tab restore;
 *                                 exported; showMain's tabs dependency
 *                                 (switchTab/applyAutoUi/parseLocationHash/currentTab)
 *                                 became a real import of ./tabs_bridge.js in S2.6
 *
 * Thin aliases DROPPED on purpose:
 *   isCapacitor/normalizeBase/setApiBase/api/getSseTicket are already real exports of
 *   ./utils.js and ./api.js. The virtual module prepends re-exports so parts still
 *   inside the IIFE keep seeing them by their old names.
 */
import { $ } from './dom.js';
import { DEFAULT_REMOTE, isCapacitor, normalizeBase, API_BASE_KEY } from './utils.js';
import { username, loginView, mainView } from './state.js';
// S2.6: the showMain ↔ tabs circular reference is now a real import.
// switchTab/applyAutoUi/parseLocationHash live in _tabs-part (IIFE); the
// bridge re-exports them lazily via window.__kaleidoTabs — by the time
// showMain runs (post-login) the facade is guaranteed to exist.
import { applyAutoUi, parseLocationHash, getCurrentTab, switchTab } from './tabs_bridge.js';

export function friendlyError(e) {
  if (!e) return '未知错误';
  const raw = (typeof e === 'string') ? e : (e.message || String(e));
  const status = e && e.status;
  const low = String(raw).toLowerCase();
  // P1-4: machine-readable code first (err.body.code from api()); exact-match whitelist
  const code = e && e.body && typeof e.body === 'object' ? e.body.code : null;
  const CODE_ZH = {
    SESSION_CAP: '会话数已达上限，可删除旧会话或调高上限',
    CRAWLER_DISABLED: '抓取功能未启用（设置中开启 crawler）',
    EMBED_UNAVAILABLE: '向量引擎不可用，已自动降级为普通检索',
    ST_TURN_BUSY: '上一回合仍在进行：点「停止」后再试',
    ST_CONCURRENT_WRITE: '会话刚被其他窗口修改，请刷新后重试',
    BG_STILL_RUNNING: '任务仍在运行：请先停止再操作',
    RATE_LIMITED: '请求过于频繁（上游限流），稍后重试',
    CONFIRM_REQUIRED: '该操作需二次确认（confirmDangerous: true）',
    ADMIN_REQUIRED: '需要管理员权限',
  };
  if (code && CODE_ZH[code]) return CODE_ZH[code];
  if (/missing bearer|unauthorized|invalid token|no authorization/i.test(raw) || status === 401) {
    return '登录已失效，请重新登录';
  }
  if (/invalid or expired session|session.*expired/i.test(raw)) {
    return '会话已过期，请重新登录后再试';
  }
  if (/zip too large|max 20mb|payload too large/i.test(low) || status === 413) {
    return '文件过大（上限 20MB），请压缩或拆分后再上传';
  }
  if (/works_path_traversal|path traversal|illegal path/i.test(low)) {
    return '路径不合法，不能包含 ../ 或跳出作品目录';
  }
  if (/session_cap|too many sessions|session cap/i.test(low)) {
    return '会话数已达上限，请删除旧会话后再创建';
  }
  if (/crawler_disabled/i.test(low)) {
    return '番茄爬虫功能未启用，请在「设置」中开启 crawlerEnabled 后重试';
  }
  if (/\/api\/v1\//.test(raw)) {
    return '请求失败：' + raw.replace(/.*\/api\/v1\//, '/api/v1/');
  }
  if (status === 403) return '权限不足或功能未开启';
  if (status === 404) return '资源不存在或已删除';
  if (status === 409) return '操作冲突：资源已被占用或状态不允许';
  if (status === 429) return '请求过于频繁，请稍后再试';
  if (status >= 500) return '服务器暂时不可用，请稍后重试';
  return raw;
}

if (typeof window !== 'undefined') window.friendlyError = friendlyError;

export function apiBase() {
  try {
    const stored = localStorage.getItem(API_BASE_KEY);
    if (stored != null && String(stored).trim() !== '') {
      return normalizeBase(stored);
    }
  } catch (_) {}
  // Capacitor / file:// → default public server; same-origin web shell → ''
  if (isCapacitor() || (typeof location !== 'undefined' && location.protocol === 'file:')) {
    return DEFAULT_REMOTE;
  }
  return '';
}

export function showLogin() {
  loginView.classList.remove('hidden');
  mainView.classList.add('hidden');
}

export function showMain() {
  loginView.classList.add('hidden');
  mainView.classList.remove('hidden');
  $('who').textContent = username ? ' · ' + username : '';
  applyAutoUi();
  const fromHash = parseLocationHash();
  const target = fromHash || getCurrentTab() || 'home';
  switchTab(target, { fromHash: true });
  // P0.1: ensure the target panel is actually visible after switchTab
  const targetPanel = document.getElementById('tab-' + target);
  if (targetPanel && targetPanel.classList.contains('hidden')) {
    targetPanel.classList.remove('hidden');
    targetPanel.setAttribute('aria-hidden', 'false');
    document.querySelectorAll('.tab-panel').forEach((el) => {
      if (el !== targetPanel) {
        el.classList.add('hidden');
        el.setAttribute('aria-hidden', 'true');
      }
    });
  }
}
