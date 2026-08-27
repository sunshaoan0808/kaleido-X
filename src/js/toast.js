/**
 * Kaleido — Toast notification system
 * S8.32: theme-consistent colors via CSS vars; immersive bottom-center dock;
 * replace support so intermediate → result flows keep only the latest toast.
 */

const TOAST_CONTAINER_ID = 'toast-container';
const TOAST_DURATION = 3500;

function getContainer() {
  let el = document.getElementById(TOAST_CONTAINER_ID);
  if (!el) {
    el = document.createElement('div');
    el.id = TOAST_CONTAINER_ID;
    document.body.appendChild(el);
  }
  // Immersive theater: dock bottom-center so toasts never cover the top bar / stage
  const immersive = document.documentElement.getAttribute('data-immersive') === '1';
  if (immersive) {
    el.style.cssText = `
      position: fixed; top: auto; right: 16px;
      bottom: calc(84px + env(safe-area-inset-bottom, 0px));
      left: 16px; z-index: var(--z-toast, 400);
      display: flex; flex-direction: column; gap: 8px;
      pointer-events: none;
      max-width: none; width: auto;
      align-items: stretch;
    `;
  } else {
    el.style.cssText = `
      position: fixed; top: 16px; right: 16px; z-index: var(--z-toast, 400);
      display: flex; flex-direction: column; gap: 8px;
      pointer-events: none;
      max-width: 380px; width: 100%;
    `;
  }
  return el;
}

/** Theme-consistent per-type colors. info/warning use surface vars (not saturated blue). */
function themeFor(type) {
  const info = {
    background: 'var(--surface-2, #f4f4f5)',
    color: 'var(--text, #111)',
    border: '1px solid var(--border, rgba(0,0,0,.08))',
  };
  const solid = (bg, fg) => ({ background: bg, color: fg, border: 'none' });
  switch (type) {
    case 'error':   return solid('var(--err, #ef4444)', '#fff');
    case 'success': return solid('#16a34a', '#fff');
    case 'warning': return info;
    default:        return info;
  }
}

export function showToast(msg, type = 'info', duration = TOAST_DURATION, replace = false) {
  const container = getContainer();
  if (replace) {
    // keep only the latest toast (e.g. 进入支线… → 已写入支线开场白)
    container.querySelectorAll('.toast-item').forEach((el) => el.remove());
  }
  const toast = document.createElement('div');
  toast.className = 'toast-item';
  Object.assign(toast.style, themeFor(type));
  toast.style.boxShadow = '0 4px 16px rgba(0,0,0,.18)';
  toast.style.maxHeight = '40vh';
  toast.style.overflowY = 'auto';

  toast.textContent = msg;
  container.appendChild(toast);

  // Dismiss on click
  toast.addEventListener('click', () => dismiss(toast));

  // Auto dismiss
  const timer = setTimeout(() => dismiss(toast), duration);
  toast._timer = timer;
}

function dismiss(el) {
  if (!el || el._dismissed) return;
  el._dismissed = true;
  clearTimeout(el._timer);
  el.classList.add('out'); // .toast-item.out → toastOut animation
  setTimeout(() => {
    if (el.parentNode) el.parentNode.removeChild(el);
  }, 300);
}

/** Helper: wrap an async call with error toast */
export async function tryApi(promise, errorMsg = '操作失败') {
  try {
    return await promise;
  } catch (err) {
    const m = err?.body?.error || err?.message || errorMsg;
    showToast(m, 'error');
    throw err;
  }
}
