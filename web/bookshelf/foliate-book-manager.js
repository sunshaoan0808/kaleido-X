/**
 * foliate-book-manager.js — 本地书籍管理（Phase 6：6.1 文件选择器 + 6.2 打开 + 6.4 IndexedDB）
 *
 * 独立 ES module，零外部依赖，不修改 React bundle。
 * 依赖全局（由 foliate-bridge.js / loader.js 提供）：
 *   - openFoliateBook(file, container)  → 把 File/Blob 渲染进 container 的 <foliate-view>
 *   - __readawareFoliate.makeBook       → foliate-js 格式分发（EPUB/PDF/MOBI/FB2/CBZ…）
 *
 * UI 层级（z-index）：
 *   fbm-drag-overlay  2147483200  （拖拽遮罩）
 *   fbm-import-btn    2147483100  （右下角导入按钮）
 *   fbm-panel         2147483050  （本地书籍面板）
 *   fbm-overlay       2147483000  （阅读全屏容器）
 */
(() => {
  'use strict';

  // ─── 常量 ────────────────────────────────────────────────────────────────
  const ACCEPT_EXTS = ['.epub', '.pdf', '.mobi', '.azw3', '.fb2', '.fbz', '.cbz', '.cbr', '.txt'];
  const MAX_SIZE = 200 * 1024 * 1024; // 200MB
  const DB_NAME = 'LocalBooks';
  const DB_VER = 1;
  const STORE = 'books';
  const Z_BTN = 2147483100;
  const Z_OVERLAY = 2147483000;
  const Z_PANEL = 2147483050;
  const Z_DRAG = 2147483200;

  // ─── IndexedDB（隐私模式等失败时降级为不保存） ─────────────────────────
  let _db = null;

  function openDB() {
    return new Promise((resolve, reject) => {
      if (_db) return resolve(_db);
      if (typeof indexedDB === 'undefined') return reject(new Error('no indexedDB'));
      const req = indexedDB.open(DB_NAME, DB_VER);
      req.onupgradeneeded = () => {
        const store = req.result.createObjectStore(STORE, { keyPath: 'id' });
        store.createIndex('addedAt', 'addedAt', { unique: false });
      };
      req.onsuccess = () => { _db = req.result; resolve(_db); };
      req.onerror = () => reject(req.error);
      req.onblocked = () => reject(new Error('blocked'));
    });
  }

  async function idbAll() {
    try {
      const d = await openDB();
      return await new Promise((res, rej) => {
        const tx = d.transaction(STORE, 'readonly').objectStore(STORE).getAll();
        tx.onsuccess = () => res(tx.result || []);
        tx.onerror = () => rej(tx.error);
      });
    } catch (e) { return []; }
  }

  async function idbPut(rec) {
    try {
      const d = await openDB();
      await new Promise((res, rej) => {
        const tx = d.transaction(STORE, 'readwrite').objectStore(STORE).put(rec);
        tx.onsuccess = res;
        tx.onerror = () => rej(tx.error);
      });
      return true;
    } catch (e) { return false; }
  }

  async function idbDel(id) {
    try {
      const d = await openDB();
      await new Promise((res, rej) => {
        const tx = d.transaction(STORE, 'readwrite').objectStore(STORE).delete(id);
        tx.onsuccess = res;
        tx.onerror = () => rej(tx.error);
      });
      return true;
    } catch (e) { return false; }
  }

  // ─── Toast ────────────────────────────────────────────────────────────────
  function toast(msg, ok = true) {
    let box = document.querySelector('.fbm-toast-box');
    if (!box) {
      box = document.createElement('div');
      box.className = 'fbm-toast-box';
      document.body.appendChild(box);
    }
    const el = document.createElement('div');
    el.className = 'fbm-toast ' + (ok ? 'fbm-toast-ok' : 'fbm-toast-err');
    el.textContent = msg;
    box.appendChild(el);
    setTimeout(() => el.remove(), 3000);
  }

  // ─── 阅读 overlay（6.2 打开本地书） ────────────────────────────────────
  function openReader(fileName) {
    document.querySelectorAll('.fbm-overlay').forEach(el => el.remove());
    const overlay = document.createElement('div');
    overlay.className = 'fbm-overlay';
    overlay.innerHTML =
      '<div class="fbm-topbar">' +
        '<span class="fbm-title"></span>' +
        '<button class="fbm-close" title="关闭">✕ 关闭</button>' +
      '</div>' +
      '<div class="fbm-content"></div>';
    document.body.appendChild(overlay);
    overlay.querySelector('.fbm-close').onclick = () => overlay.remove();
    overlay.querySelector('.fbm-title').textContent = fileName || '本地书籍';
    return overlay.querySelector('.fbm-content');
  }

  async function openFile(file) {
    try {
      const content = openReader(file.name);
      const openFn = globalThis.__foliateBridge && globalThis.__foliateBridge.openFoliateBook
        ? globalThis.__foliateBridge.openFoliateBook
        : globalThis.openFoliateBook;
      await openFn(file, content);
      toast('已打开 ' + file.name);
    } catch (err) {
      document.querySelectorAll('.fbm-overlay').forEach(el => el.remove());
      toast('打开失败: ' + (err && err.message ? err.message : err), false);
      console.error('[fbm] open failed', err);
    }
  }

  // ─── 导入处理（6.1 校验 + 保存 + 打开） ───────────────────────────────
  async function handleFiles(fileList) {
    const files = Array.from(fileList || []);
    for (const file of files) {
      const dot = file.name.lastIndexOf('.');
      const ext = dot >= 0 ? '.' + file.name.slice(dot + 1).toLowerCase() : '';
      if (!ACCEPT_EXTS.includes(ext)) {
        toast('不支持格式 ' + (ext || '(无扩展名)'), false);
        continue;
      }
      if (file.size > MAX_SIZE) {
        toast(file.name + ' 超过 200MB', false);
        continue;
      }
      const rec = {
        id: Date.now().toString(36) + '-' + Math.random().toString(36).slice(2, 8),
        name: file.name,
        type: ext.slice(1).toUpperCase(),
        size: file.size,
        addedAt: new Date().toISOString(),
        blob: file,
      };
      const saved = await idbPut(rec);
      panelVisible = true;
      renderPanel();
      if (saved) toast('已导入 ' + file.name);
      else toast('已打开 ' + file.name + '（本地库不可用，未保存）');
      await openFile(file);
    }
  }

  // ─── UI：导入按钮 ────────────────────────────────────────────────────────
  function buildImportButton() {
    if (document.querySelector('.fbm-import-btn')) return;
    const btn = document.createElement('button');
    btn.className = 'fbm-import-btn';
    btn.textContent = '📤 导入书籍';
    btn.title = '导入 EPUB / PDF / MOBI / FB2 / CBZ / CBR / TXT';
    const input = document.createElement('input');
    input.type = 'file';
    input.accept = ACCEPT_EXTS.join(',');
    input.multiple = true;
    input.style.display = 'none';
    btn.appendChild(input);
    btn.addEventListener('click', (e) => {
      e.stopPropagation();
      panelVisible = !panelVisible;
      const panel = document.querySelector('.fbm-panel');
      if (panel) panel.style.display = panelVisible ? 'block' : 'none';
      else renderPanel();
      input.click();
    });
    input.addEventListener('change', () => {
      handleFiles(input.files);
      input.value = '';
    });
    document.body.appendChild(btn);
  }

  // ─── UI：拖拽遮罩 ────────────────────────────────────────────────────────
  function buildDragOverlay() {
    if (document.querySelector('.fbm-drag-overlay')) return;
    const mask = document.createElement('div');
    mask.className = 'fbm-drag-overlay';
    mask.innerHTML = '<div class="fbm-drag-text">📚 松开导入书籍</div>';
    mask.style.display = 'none';
    document.body.appendChild(mask);
    let dragDepth = 0;

    const show = () => { dragDepth += 1; mask.style.display = 'flex'; };
    const hide = () => { dragDepth = Math.max(0, dragDepth - 1); if (dragDepth === 0) mask.style.display = 'none'; };

    document.addEventListener('dragenter', (e) => {
      if (e.dataTransfer && Array.from(e.dataTransfer.types || []).includes('Files')) show();
    });
    document.addEventListener('dragover', (e) => {
      if (e.dataTransfer && Array.from(e.dataTransfer.types || []).includes('Files')) {
        e.preventDefault();
        e.dataTransfer.dropEffect = 'copy';
      }
    });
    document.addEventListener('dragleave', (e) => {
      if (!mask.contains(e.relatedTarget)) hide();
    });
    document.addEventListener('drop', (e) => {
      e.preventDefault();
      hide();
      if (e.dataTransfer && e.dataTransfer.files && e.dataTransfer.files.length) {
        handleFiles(e.dataTransfer.files);
      }
    });
  }

  // ─── UI：本地书籍面板（6.4） ─────────────────────────────────────────────
  // 面板默认隐藏；由「📤 导入书籍」按钮 toggle 显示，导入成功后自动展开。
  let panelVisible = false;
  function fmtSize(bytes) {
    if (bytes >= 1024 * 1024) return (bytes / (1024 * 1024)).toFixed(1) + 'MB';
    if (bytes >= 1024) return Math.round(bytes / 1024) + 'KB';
    return bytes + 'B';
  }

  async function renderPanel() {
    let panel = document.querySelector('.fbm-panel');
    const books = await idbAll();
    if (!panel) {
      panel = document.createElement('div');
      panel.className = 'fbm-panel';
      document.body.appendChild(panel);
    }
    const head = '<div class="fbm-panel-head">📚 本地书籍 (' + books.length + ') <button class="fbm-panel-close" title="关闭">✕</button></div>';
    let rows = '';
    for (const b of books.slice().reverse()) {
      rows +=
        '<div class="fbm-row" data-id="' + b.id + '">' +
          '<span class="fbm-row-name" title="' + b.name + '">' + b.name + '</span>' +
          '<span class="fbm-badge">' + (b.type || '?') + '</span>' +
          '<span class="fbm-size">' + fmtSize(b.size) + '</span>' +
          '<button class="fbm-act fbm-read" data-id="' + b.id + '">阅读</button>' +
          '<button class="fbm-act fbm-del" data-id="' + b.id + '">删除</button>' +
        '</div>';
    }
    const empty = books.length === 0 ? '<div class="fbm-empty">暂无本地书籍，点击右下角「📤 导入书籍」上传</div>' : '';
    panel.innerHTML = head + empty + '<div class="fbm-rows">' + rows + '</div>';
    panel.style.display = panelVisible ? 'block' : 'none';

    const closeBtn = panel.querySelector('.fbm-panel-close');
    if (closeBtn) closeBtn.onclick = (e) => {
      e.stopPropagation();
      panelVisible = false;
      panel.style.display = 'none';
    };

    panel.querySelectorAll('.fbm-read').forEach(btn => {
      btn.onclick = async (e) => {
        e.stopPropagation();
        const id = btn.dataset.id;
        const rec = books.find(b => b.id === id);
        if (!rec) return toast('记录不存在', false);
        await openFile(new File([rec.blob], rec.name, { type: rec.blob ? rec.blob.type : '' }));
      };
    });
    panel.querySelectorAll('.fbm-del').forEach(btn => {
      btn.onclick = async (e) => {
        e.stopPropagation();
        const id = btn.dataset.id;
        if (!confirm('删除「' + btn.parentElement.querySelector('.fbm-row-name').textContent + '」？')) return;
        const ok = await idbDel(id);
        if (!ok) toast('删除失败', false);
        renderPanel();
      };
    });
  }

  // ─── 样式注入 ─────────────────────────────────────────────────────────────
  function injectStyles() {
    if (document.getElementById('fbm-styles')) return;
    const style = document.createElement('style');
    style.id = 'fbm-styles';
    style.textContent = `
      .fbm-import-btn {
        position: fixed; right: 20px; bottom: 20px; z-index: ${Z_BTN};
        padding: 12px 18px; border: none; border-radius: 24px;
        background: #4f6ef7; color: #fff; font-size: 14px; cursor: pointer;
        box-shadow: 0 4px 14px rgba(0,0,0,.25);
      }
      .fbm-import-btn:hover { background: #3d5ae0; }
      .fbm-drag-overlay {
        position: fixed; inset: 0; z-index: ${Z_DRAG};
        display: flex; align-items: center; justify-content: center;
        background: rgba(20, 30, 60, .55); pointer-events: none;
      }
      .fbm-drag-text {
        padding: 30px 48px; border: 3px dashed #fff; border-radius: 16px;
        color: #fff; font-size: 22px; background: rgba(0,0,0,.35);
      }
      .fbm-panel {
        position: fixed; left: 16px; bottom: 16px; z-index: ${Z_PANEL};
        width: 340px; max-height: 45vh; overflow-y: auto;
        background: #fff; border: 1px solid #e2e5ec; border-radius: 12px;
        box-shadow: 0 6px 24px rgba(0,0,0,.18); font-size: 13px;
      }
      .fbm-panel-head {
        padding: 10px 14px; font-weight: 600; border-bottom: 1px solid #eee;
        position: sticky; top: 0; background: #fff;
        display: flex; align-items: center; justify-content: space-between;
      }
      .fbm-panel-close {
        border: none; background: transparent; color: #98a1b3;
        font-size: 14px; cursor: pointer; padding: 2px 6px; line-height: 1;
      }
      .fbm-panel-close:hover { color: #d9534f; }
      .fbm-row {
        display: flex; align-items: center; gap: 8px;
        padding: 8px 14px; border-bottom: 1px solid #f2f3f7;
      }
      .fbm-row-name { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
      .fbm-badge {
        background: #eef2ff; color: #4f6ef7; border-radius: 4px;
        padding: 1px 6px; font-size: 11px; flex-shrink: 0;
      }
      .fbm-size { color: #98a1b3; font-size: 11px; flex-shrink: 0; }
      .fbm-act {
        border: 1px solid #d4d9e4; background: #fff; border-radius: 6px;
        padding: 3px 10px; font-size: 12px; cursor: pointer; flex-shrink: 0;
      }
      .fbm-act:hover { background: #f2f4fa; }
      .fbm-act.fbm-read { color: #4f6ef7; border-color: #b9c4f5; }
      .fbm-act.fbm-del { color: #d9534f; border-color: #f0c4c2; }
      .fbm-empty { padding: 14px; color: #98a1b3; }
      .fbm-toast-box { position: fixed; right: 20px; top: 20px; z-index: 2147484000; display: flex; flex-direction: column; gap: 8px; }
      .fbm-toast {
        padding: 10px 16px; border-radius: 8px; color: #fff; font-size: 13px;
        box-shadow: 0 4px 14px rgba(0,0,0,.25); max-width: 320px;
      }
      .fbm-toast-ok { background: #2f9e44; }
      .fbm-toast-err { background: #d9534f; }
      .fbm-overlay {
        position: fixed; inset: 0; z-index: ${Z_OVERLAY};
        background: #fff; display: flex; flex-direction: column;
      }
      .fbm-topbar {
        display: flex; align-items: center; justify-content: space-between;
        padding: 10px 16px; background: #f6f7fa; border-bottom: 1px solid #e2e5ec;
      }
      .fbm-title { font-size: 15px; font-weight: 600; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; margin-right: 12px; }
      .fbm-close {
        border: 1px solid #d4d9e4; background: #fff; border-radius: 6px;
        padding: 5px 12px; cursor: pointer; font-size: 13px;
      }
      .fbm-content { flex: 1; min-height: 0; }
      .fbm-content foliate-view { width: 100%; height: 100%; }
      @media (prefers-color-scheme: dark) {
        .fbm-panel { background: #1f2430; border-color: #333a4a; }
        .fbm-panel-head { background: #1f2430; border-bottom-color: #333a4a; }
        .fbm-row { border-bottom-color: #2a3140; color: #d8dce8; }
        .fbm-empty { color: #6b7387; }
        .fbm-act { background: #2a3140; border-color: #3a4356; color: #d8dce8; }
        .fbm-overlay { background: #161a22; }
        .fbm-topbar { background: #1f2430; border-bottom-color: #333a4a; color: #d8dce8; }
        .fbm-close { background: #2a3140; border-color: #3a4356; color: #d8dce8; }
      }
    `;
    document.head.appendChild(style);
  }

  // ─── 初始化 ───────────────────────────────────────────────────────────────
  function init() {
    injectStyles();
    buildImportButton();
    buildDragOverlay();
    renderPanel();
  }
  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', init);
  else init();
  // OPDS 集成钩子：允许外部模块(如 opds-manager.js)触发导入/打开
  window.__fbm = { handleFiles, openFile };
})();
