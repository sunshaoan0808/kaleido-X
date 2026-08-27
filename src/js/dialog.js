/**
 * Kaleido — Unified confirm/prompt dialog (replaces native confirm/prompt)
 * S9.21: single modal component with focus trap, Esc close, aria-labelledby.
 * showConfirm(msg, {title, danger}) → Promise<boolean>
 * showPrompt(msg, {default, placeholder, title, validate}) → Promise<string|null>
 * Alert 类通知统一走 toast (showToast), 不再使用原生 alert.
 */

const DIALOG_CONTAINER_ID = 'dialog-container';

function dialogContainer() {
  let el = document.getElementById(DIALOG_CONTAINER_ID);
  if (!el) {
    el = document.createElement('div');
    el.id = DIALOG_CONTAINER_ID;
    document.body.appendChild(el);
  }
  return el;
}

/**
 * Build one modal dialog. Returns {root, input, okBtn, cancelBtn, resolve, cleanup}.
 */
function buildDialog({ title, message, kind, placeholder, value, danger }) {
  const container = dialogContainer();
  const root = document.createElement('div');
  root.className = 'kaleido-dialog-backdrop';
  root.setAttribute('role', 'presentation');

  const dialog = document.createElement('div');
  dialog.className = 'kaleido-dialog';
  dialog.setAttribute('role', 'dialog');
  dialog.setAttribute('aria-modal', 'true');
  const titleId = 'kaleido-dialog-title-' + Math.random().toString(36).slice(2, 8);
  dialog.setAttribute('aria-labelledby', titleId);

  const h = document.createElement('h3');
  h.id = titleId;
  h.className = 'kaleido-dialog-title';
  h.textContent = title || (kind === 'confirm' ? '确认' : '输入');

  let input = null;
  const msg = document.createElement('div');
  msg.className = 'kaleido-dialog-msg';
  msg.textContent = message || '';
  if (kind === 'prompt') {
    input = document.createElement('input');
    input.className = 'kaleido-dialog-input';
    input.type = 'text';
    input.placeholder = placeholder || '';
    if (value !== undefined && value !== null) input.value = String(value);
  }

  const actions = document.createElement('div');
  actions.className = 'kaleido-dialog-actions';

  const cancelBtn = document.createElement('button');
  cancelBtn.type = 'button';
  cancelBtn.className = 'btn kaleido-dialog-btn';
  cancelBtn.textContent = kind === 'confirm' ? '取消' : '取消';
  const okBtn = document.createElement('button');
  okBtn.type = 'button';
  okBtn.className = 'btn kaleido-dialog-btn kaleido-dialog-btn-primary' + (danger ? ' kaleido-dialog-btn-danger' : '');
  okBtn.textContent = kind === 'confirm' ? '确定' : '确定';
  if (danger) okBtn.classList.add('danger');

  actions.appendChild(cancelBtn);
  actions.appendChild(okBtn);
  dialog.appendChild(h);
  dialog.appendChild(msg);
  if (input) dialog.appendChild(input);
  dialog.appendChild(actions);
  root.appendChild(dialog);
  container.appendChild(root);

  let done = false;
  let lastFocused = document.activeElement;

  const cleanup = () => {
    if (done) return;
    done = true;
    document.removeEventListener('keydown', onKeydown);
    root.remove();
    if (lastFocused && lastFocused.focus) lastFocused.focus();
  };
  const resolve = (val) => { cleanup(); resolveOnce(val); };
  let resolveOnce;
  const promise = new Promise((r) => { resolveOnce = r; });

  const focusables = () => Array.from(dialog.querySelectorAll('button, input, [tabindex]:not([tabindex="-1"])'))
    .filter((el) => !el.disabled);

  function trap(e) {
    if (e.key !== 'Tab') return;
    const items = focusables();
    if (items.length === 0) return;
    const first = items[0];
    const last = items[items.length - 1];
    if (e.shiftKey && document.activeElement === first) {
      e.preventDefault(); last.focus();
    } else if (!e.shiftKey && document.activeElement === last) {
      e.preventDefault(); first.focus();
    }
  }

  function onKeydown(e) {
    if (e.key === 'Escape') {
      e.preventDefault();
      resolve(kind === 'confirm' ? false : null);
    }
    trap(e);
  }

  cancelBtn.addEventListener('click', () => resolve(kind === 'confirm' ? false : null));
  okBtn.addEventListener('click', () => {
    if (kind === 'confirm') { resolve(true); return; }
    const val = input ? input.value : '';
    if (validate && !validate(val)) return; // keep open; caller's validate shows toast
    resolve(val);
  });
  document.addEventListener('keydown', onKeydown);

  // Focus first focusable (ok for confirm; input for prompt)
  requestAnimationFrame(() => {
    if (input) input.focus();
    else okBtn.focus();
  });

  return { promise, okBtn, cleanup };
}

/** Replaces native confirm(). Returns Promise<boolean>. */
export function showConfirm(message, opts = {}) {
  const { title = '确认', danger = false } = opts;
  return buildDialog({ kind: 'confirm', title, message, danger }).promise;
}

/** Replaces native prompt(). Returns Promise<string|null> (null = cancelled). */
export function showPrompt(message, opts = {}) {
  const { title = '输入', placeholder = '', value, validate } = opts;
  return buildDialog({ kind: 'prompt', title, message, placeholder, value, validate }).promise;
}
