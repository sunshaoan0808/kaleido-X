// opds-manager.js — OPDS 目录浏览/搜索/下载（Phase 6.5）
// 依赖: foliate-book-manager.js 暴露 window.__fbm; 后端 serve.py /opds-proxy 绕 CORS
(() => {
  const LS_KEY = 'fbm_opds_sources';
  const PROXY = '/opds-proxy?url=';
  const ACQ_PREFIX = 'http://opds-spec.org/acquisition';
  const BOOK_TYPE_RE = /(epub|pdf|mobi|azw3|fb2|cbz|cbr|zip)/i;

  const state = { url: '', title: '', stack: [] };

  // ─── 工具 ────────────────────────────────────────────────────────────────
  const $ = (sel, root = document) => root.querySelector(sel);
  const esc = s => String(s ?? '').replace(/[&<>"']/g,
    c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
  const relUrl = (href, base) => { try { return new URL(href, base).href; } catch { return href; } };
  // foliate-js getLink 的 rel 是数组(空格分隔), 这里统一兼容字符串/数组
  const hasRel = (l, target) => {
    const rels = Array.isArray(l?.rel) ? l.rel : [l?.rel];
    return rels.some(r => String(r ?? '').trim() === target);
  };
  const DEFAULT_SOURCES = [
    { name: '本地测试书库', url: location.origin + '/web/bookshelf/test-opds12.xml' },
    { name: 'Project Gutenberg', url: 'https://www.gutenberg.org/ebooks.opds/' },
  ];
  const getSources = () => {
    try {
      const s = JSON.parse(localStorage.getItem(LS_KEY) || '[]');
      return Array.isArray(s) && s.length ? s : DEFAULT_SOURCES;
    } catch { return DEFAULT_SOURCES; }
  };
  const saveSources = s => localStorage.setItem(LS_KEY, JSON.stringify(s));
  const proxyUrl = u => PROXY + encodeURIComponent(u);

  // ─── 抓取 ────────────────────────────────────────────────────────────────
  async function fetchText(url) {
    const resp = await fetch(proxyUrl(url));
    if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
    return await resp.text();
  }
  async function fetchBlob(url) {
    const resp = await fetch(proxyUrl(url));
    if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
    return await resp.blob();
  }

  // ─── OPDS 解析 ───────────────────────────────────────────────────────────
  async function parseFeed(text, baseUrl) {
    const t = text.trimStart();
    if (t.startsWith('{')) return parseOPDS2(JSON.parse(t), baseUrl);
    // OPDS 1.x (Atom)
    const doc = new DOMParser().parseFromString(text, 'application/xml');
    if (doc.querySelector('parsererror')) throw new Error('XML 解析失败');
    const { getFeed } = await import('./foliate-js/opds.js');
    const feed = getFeed(doc);
    // 相对链接解析为绝对
    for (const l of feed.links || []) l.href = relUrl(l.href, baseUrl);
    for (const g of feed.groups || []) for (const l of g.links || []) l.href = relUrl(l.href, baseUrl);
    for (const p of feed.publications || []) {
      for (const l of p.links || []) l.href = relUrl(l.href, baseUrl);
      p.images = (p.images || []).map(i => ({ ...i, href: relUrl(i.href, baseUrl) }));
    }
    for (const n of feed.navigation || []) if (n.href) n.href = relUrl(n.href, baseUrl);
    return feed;
  }

  function parseOPDS2(json, baseUrl) {
    const pub = [];
    for (const p of (json.publications || json.books || [])) {
      const links = (p.links || []).map(l => ({
        rel: l.rel, type: l.type, title: l.title,
        href: relUrl(l.href, baseUrl),
      }));
      pub.push({
        metadata: {
          title: p.metadata?.title,
          author: (p.metadata?.authors || []).map(a => ({ name: a.name })),
          language: p.metadata?.language,
        },
        links,
        images: (p.images || []).map(i => relUrl(i.href, baseUrl)),
      });
    }
    const nav = [];
    for (const g of (json.groups || [])) {
      const gtitle = g.metadata?.title || '';
      for (const n of (g.navigation || [])) {
        const l = (n.links || [])[0];
        if (l) nav.push({ title: n.title || gtitle, href: relUrl(l.href, baseUrl), type: l.type });
      }
    }
    return {
      metadata: { title: json.metadata?.title },
      links: (json.links || []).map(l => ({ rel: l.rel, href: relUrl(l.href, baseUrl), type: l.type })),
      publications: pub,
      navigation: nav,
      groups: [],
      facets: [],
    };
  }

  // 取下载链接: 优先 acquisition, 兜底任一带书籍类型的 link
  function pickDownload(pub) {
    const links = pub.links || [];
    const acq = links.find(l => {
      const rels = Array.isArray(l.rel) ? l.rel : [l.rel];
      return rels.some(r => String(r ?? '').startsWith(ACQ_PREFIX));
    });
    if (acq) return acq;
    return links.find(l => l.type && BOOK_TYPE_RE.test(l.type));
  }

  // OpenSearch: 找到搜索 URL 模板
  async function findSearchTemplate(feed) {
    const link = (feed.links || []).find(l => hasRel(l, 'search'));
    if (!link) return null;
    if (link.href.includes('{')) return link.href;
    if (link.type?.includes('opensearchdescription')) {
      try {
        const text = await fetchText(link.href);
        const doc = new DOMParser().parseFromString(text, 'application/xml');
        const url = Array.from(doc.querySelectorAll('Url'))
          .find(u => u.getAttribute('type')?.includes('atom') || u.getAttribute('template')?.includes('searchTerms'));
        const tpl = url?.getAttribute('template');
        if (tpl) return tpl.replace('{startIndex}', '1').replace('{count}', '20');
      } catch { /* ignore */ }
    }
    return null;
  }

  // ─── 渲染 ────────────────────────────────────────────────────────────────
  function renderFeed(feed, baseUrl) {
    const wrap = $('#opds-body');
    const nav = feed.navigation || [];
    const pubs = feed.publications || [];
    const groups = feed.groups || [];
    let html = '';

    if (nav.length) {
      html += '<div class="opds-nav"><h4>目录</h4>';
      for (const n of nav) {
        html += `<div class="opds-nav-item" data-href="${esc(n.href)}">📁 ${esc(n.title || '(未命名)')}</div>`;
      }
      html += '</div>';
    }
    if (groups.length) {
      html += '<div class="opds-nav"><h4>分类</h4>';
      for (const g of groups) {
        const self = (g.links || []).find(l => hasRel(l, 'self'));
        if (self) html += `<div class="opds-nav-item" data-href="${esc(self.href)}">📁 ${esc(g.metadata?.title || '(未命名)')}</div>`;
      }
      html += '</div>';
    }
    if (pubs.length) {
      html += '<div class="opds-grid">';
      for (const p of pubs) {
        const cover = p.images?.[0]?.href || p.images?.[0];
        const author = (p.metadata?.author || []).map(a => a.name).filter(Boolean).join(', ');
        const dl = pickDownload(p);
        html += `<div class="opds-card">
          <div class="opds-cover">${cover ? `<img src="${esc(cover)}" referrerpolicy="no-referrer" loading="lazy" onerror="this.style.display='none'">` : '📖'}</div>
          <div class="opds-meta"><div class="opds-title">${esc(p.metadata?.title || '(无标题)')}</div>
          <div class="opds-author">${esc(author)}</div>
          ${dl ? `<button class="opds-dl" data-href="${esc(dl.href)}" data-type="${esc(dl.type || '')}">⬇ 下载 ${esc((dl.type || '').split('/').pop() || '')}</button>` : '<span class="opds-nodl">无下载</span>'}
          </div></div>`;
      }
      html += '</div>';
    }
    if (!nav.length && !pubs.length && !groups.length) html = '<div class="opds-empty">此目录无内容</div>';
    wrap.innerHTML = html;

    // 事件
    wrap.querySelectorAll('.opds-nav-item').forEach(el =>
      el.addEventListener('click', () => loadFeed(el.dataset.href)));
    wrap.querySelectorAll('.opds-dl').forEach(el =>
      el.addEventListener('click', () => download(el.dataset.href, el.dataset.type)));
  }

  // ─── 下载 → 导入书架 ─────────────────────────────────────────────────────
  async function download(href, type) {
    const btn = document.activeElement;
    const original = btn ? btn.textContent : '';
    if (btn) btn.disabled = true, btn.textContent = '⏳ 下载中…';
    try {
      const blob = await fetchBlob(href);
      let name = decodeURIComponent(new URL(href).pathname.split('/').pop() || '');
      if (!name.includes('.')) {
        const ext = (type || '').split('/').pop() || 'epub';
        name = (name || 'book') + '.' + ext;
      }
      const file = new File([blob], name, { type: blob.type || type || 'application/octet-stream' });
      const fbm = window.__fbm;
      if (!fbm || !fbm.handleFiles) throw new Error('书架导入钩子未就绪');
      await fbm.handleFiles([file]);
    } catch (e) {
      alert('下载失败: ' + e.message);
    } finally {
      if (btn) btn.disabled = false, btn.textContent = original;
    }
  }

  // ─── 目录加载 ────────────────────────────────────────────────────────────
  async function loadFeed(url) {
    const body = $('#opds-body');
    body.innerHTML = '<div class="opds-loading">⏳ 加载目录…</div>';
    try {
      const text = await fetchText(url);
      const feed = await parseFeed(text, url);
      state.url = url;
      state.title = feed.metadata?.title || new URL(url).hostname;
      $('#opds-title').textContent = state.title;
      $('#opds-breadcrumb').textContent = state.stack.map(s => s.title).concat([state.title]).join(' / ');
      renderFeed(feed, url);
      // 搜索框
      const tpl = await findSearchTemplate(feed);
      const box = $('#opds-search');
      box.style.display = tpl ? '' : 'none';
      if (tpl) box.dataset.tpl = tpl;
    } catch (e) {
      body.innerHTML = `<div class="opds-error">加载失败: ${esc(e.message)}<br>请检查 URL 是否有效、网络是否可达</div>`;
    }
  }

  // ─── UI 构建 ─────────────────────────────────────────────────────────────
  function buildButton() {
    if ($('#opds-btn')) return;
    const btn = document.createElement('button');
    btn.id = 'opds-btn';
    btn.textContent = '🌐 OPDS 书库';
    btn.title = '浏览 OPDS 在线书库';
    btn.addEventListener('click', openModal);
    document.body.appendChild(btn);
  }

  function openModal() {
    let modal = $('#opds-modal');
    if (!modal) {
      modal = document.createElement('div');
      modal.id = 'opds-modal';
      document.body.appendChild(modal);
    }
    modal.style.display = 'flex';
    renderSources();
    // 自动加载第一个源
    const srcs = getSources();
    if (srcs.length && !state.url) loadFeed(srcs[0].url);
  }

  function renderSources() {
    const wrap = $('#opds-sources');
    const srcs = getSources();
    wrap.innerHTML = srcs.map(s =>
      `<div class="opds-source" data-url="${esc(s.url)}">
        <span class="opds-source-name">${esc(s.name)}</span>
        <button class="opds-source-open">打开</button>
        <button class="opds-source-del">✕</button>
      </div>`).join('') || '<div class="opds-empty">尚未添加 OPDS 源</div>';
    wrap.querySelectorAll('.opds-source-open').forEach((b, i) =>
      b.addEventListener('click', () => { state.stack = []; loadFeed(srcs[i].url); }));
    wrap.querySelectorAll('.opds-source-del').forEach((b, i) => {
      b.addEventListener('click', () => {
        saveSources(srcs.filter((_, j) => j !== i));
        renderSources();
      });
    });
    const addBtn = $('#opds-add');
    addBtn.onclick = () => {
      const name = $('#opds-url-name').value.trim();
      const url = $('#opds-url').value.trim();
      if (!url) { alert('请输入 OPDS 目录 URL'); return; }
      const s = getSources();
      if (s.some(x => x.url === url)) { alert('该源已存在'); return; }
      s.push({ name: name || new URL(url).hostname, url });
      saveSources(s);
      $('#opds-url').value = '';
      $('#opds-url-name').value = '';
      renderSources();
      state.stack = [];
      loadFeed(url);
    };
  }

  function buildModal() {
    const modal = document.createElement('div');
    modal.id = 'opds-modal';
    modal.style.display = 'none';
    modal.innerHTML = `
      <div class="opds-panel">
        <div class="opds-head">
          <span id="opds-title">OPDS 书库</span>
          <button id="opds-close">✕</button>
        </div>
        <div class="opds-sources" id="opds-sources"></div>
        <div class="opds-addrow">
          <input id="opds-url-name" placeholder="名称(可选)" class="opds-input opds-input-sm">
          <input id="opds-url" placeholder="OPDS 目录 URL (http://…)" class="opds-input">
          <button id="opds-add" class="opds-btn">添加</button>
        </div>
        <div id="opds-search" class="opds-search" style="display:none">
          <input id="opds-q" placeholder="搜索此书库…" class="opds-input">
          <button id="opds-go" class="opds-btn">搜索</button>
        </div>
        <div id="opds-breadcrumb" class="opds-breadcrumb"></div>
        <div id="opds-body" class="opds-body"><div class="opds-empty">添加一个 OPDS 源开始浏览</div></div>
      </div>`;
    document.body.appendChild(modal);
    $('#opds-close').addEventListener('click', () => modal.style.display = 'none');
    $('#opds-go').addEventListener('click', () => {
      const box = $('#opds-search');
      const q = $('#opds-q').value.trim();
      if (!q || !box.dataset.tpl) return;
      const url = relUrl(box.dataset.tpl.replace('{searchTerms}', encodeURIComponent(q)), state.url);
      state.stack.push({ title: state.title, url: state.url });
      loadFeed(url);
    });
    $('#opds-q').addEventListener('keydown', e => { if (e.key === 'Enter') $('#opds-go').click(); });
    modal.addEventListener('click', e => { if (e.target === modal) modal.style.display = 'none'; });
  }

  function injectStyles() {
    if ($('#opds-styles')) return;
    const style = document.createElement('style');
    style.id = 'opds-styles';
    style.textContent = `
      #opds-btn {
        position: fixed; right: 150px; bottom: 20px; z-index: 2147483090;
        padding: 12px 18px; border: none; border-radius: 24px;
        background: #2a3140; color: #d8dce8; font-size: 14px; cursor: pointer;
        box-shadow: 0 4px 14px rgba(0,0,0,.25);
      }
      #opds-btn:hover { background: #3a4356; }
      #opds-modal {
        position: fixed; inset: 0; z-index: 2147483099;
        display: none; align-items: center; justify-content: center;
        background: rgba(10,14,24,.7); backdrop-filter: blur(2px);
      }
      .opds-panel {
        width: min(900px, 94vw); max-height: 86vh; display: flex; flex-direction: column;
        background: #1f2430; color: #d8dce8; border-radius: 12px; overflow: hidden;
        box-shadow: 0 10px 40px rgba(0,0,0,.5);
      }
      .opds-head {
        display: flex; justify-content: space-between; align-items: center;
        padding: 14px 18px; background: #252b3a; font-size: 16px; font-weight: 600;
      }
      #opds-close { background: none; border: none; color: #aab3c5; font-size: 16px; cursor: pointer; }
      .opds-sources { padding: 10px 18px 0; display: flex; flex-wrap: wrap; gap: 8px; }
      .opds-source {
        display: flex; align-items: center; gap: 6px; padding: 4px 10px;
        background: #2a3140; border: 1px solid #3a4356; border-radius: 16px; font-size: 12px;
      }
      .opds-source button { background: none; border: none; color: #8fa0c0; cursor: pointer; font-size: 12px; }
      .opds-source .opds-source-open { color: #7aa2ff; }
      .opds-addrow { display: flex; gap: 8px; padding: 10px 18px; }
      .opds-input {
        flex: 1; padding: 8px 10px; border: 1px solid #3a4356; border-radius: 8px;
        background: #171c28; color: #d8dce8; font-size: 13px;
      }
      .opds-input-sm { flex: 0 0 120px; }
      .opds-btn {
        padding: 8px 16px; border: none; border-radius: 8px;
        background: #4f6ef7; color: #fff; cursor: pointer; font-size: 13px;
      }
      .opds-search { display: flex; gap: 8px; padding: 0 18px 10px; }
      .opds-breadcrumb { padding: 4px 18px 8px; color: #7a84a0; font-size: 12px; }
      .opds-body { flex: 1; overflow-y: auto; padding: 4px 18px 18px; }
      .opds-loading, .opds-empty, .opds-error { padding: 24px; text-align: center; color: #8fa0c0; }
      .opds-error { color: #e06a6a; }
      .opds-nav h4 { margin: 12px 0 6px; color: #aab3c5; font-size: 13px; }
      .opds-nav-item {
        padding: 8px 12px; margin: 2px 0; border-radius: 8px; cursor: pointer;
        background: #252b3a; color: #7aa2ff; font-size: 14px;
      }
      .opds-nav-item:hover { background: #2e3648; }
      .opds-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(150px, 1fr)); gap: 12px; margin-top: 10px; }
      .opds-card { background: #252b3a; border-radius: 10px; overflow: hidden; display: flex; flex-direction: column; }
      .opds-cover {
        height: 180px; display: flex; align-items: center; justify-content: center;
        background: #2e3648; font-size: 40px; overflow: hidden;
      }
      .opds-cover img { width: 100%; height: 100%; object-fit: cover; }
      .opds-meta { padding: 10px; display: flex; flex-direction: column; gap: 4px; flex: 1; }
      .opds-title { font-size: 13px; font-weight: 600; line-height: 1.35; display: -webkit-box; -webkit-line-clamp: 2; -webkit-box-orient: vertical; overflow: hidden; }
      .opds-author { font-size: 12px; color: #8fa0c0; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
      .opds-dl { margin-top: auto; padding: 7px; border: none; border-radius: 7px; background: #4f6ef7; color: #fff; cursor: pointer; font-size: 12px; }
      .opds-dl:hover { background: #3d5ae0; }
      .opds-nodl { margin-top: auto; font-size: 12px; color: #666e80; }
    `;
    document.head.appendChild(style);
  }

  function init() {
    injectStyles();
    buildButton();
    buildModal();
  }
  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', init);
  else init();
})();
