/* P1-3 S2.4: _search-part → real ESM (list filtering, global error toasts,
 * global-search overlay open/close).
 *
 * doGlobalSearch stays in _keyboard-part (it owns the in-memory index) — the
 * overlay here only manages visibility/focus.
 */
import { $ } from './dom.js';
// showToast still lives in the IIFE state block (alias of toast.js's export);
// import the real source directly — identical function object.
import { showToast } from './toast.js';

export function wireListSearch(inputId, listSelector) {
  const input = $(inputId);
  if (!input) return;
  const getList = () => input.closest('.st-side-section, .az-panel, .panel, aside')?.querySelector(listSelector) || $(listSelector);
  input.addEventListener('input', () => {
    const list = getList();
    if (!list) return;
    const q = input.value.trim().toLowerCase();
    list.querySelectorAll('.item, .az-item, .st-session-group-head, .st-pack-group-head').forEach(el => {
      el.style.display = (!q || el.textContent.toLowerCase().includes(q)) ? '' : 'none';
    });
  });
}
wireListSearch('story-session-search', '#story-session-list');
wireListSearch('st-session-search', '#st-session-list');
wireListSearch('st-drawer-session-search', '#st-drawer-session-list');
wireListSearch('az-project-search', '#az-project-list');
wireListSearch('cc-search', '#cc-list');

// Global error handler — toast uncaught errors
window.addEventListener('error', (e) => {
  showToast('脚本错误：' + (e.message || e), 'error');
});
window.addEventListener('unhandledrejection', (e) => {
  const msg = e.reason?.message || e.reason || 'Promise 未捕获';
  // Don't toast API errors that are already handled
  if (!msg.includes('HTTP ') && !msg.includes('Failed to fetch')) {
    showToast(msg, 'error');
  }
});

// ── Global search ──
export function openGlobalSearch() {
  const overlay = $('glob-search-overlay');
  if (!overlay) return;
  overlay.classList.remove('hidden');
  const input = $('glob-search-input');
  if (input) {
    input.value = '';
    setTimeout(() => input.focus(), 50);
  }
  const results = $('glob-search-results');
  if (results) results.innerHTML = '';
}
export function closeGlobalSearch() {
  const overlay = $('glob-search-overlay');
  if (overlay) overlay.classList.add('hidden');
}
