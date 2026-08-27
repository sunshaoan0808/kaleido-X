/* story-graph-editor.js — P2 节点图画线全编辑器（自包含，注入样式，不依赖 app.js 内部）
 * 数据源: GET  /api/v1/story-tavern/packs/{id}
 * 保存:   POST /api/v1/story-tavern/packs  (upsert_pack, body={id, ...pack})
 * 图模型: pack.nodes[] -> StoryNode {id, chapterId, title, entry, exit:[{id,when,next}], ...}
 */
(function () {
  'use strict';
  var API = '/api/v1/story-tavern';
  var LS_KEY = 'stge-layout:'; // + packId
  var state = { pack: null, mode: 'view', nodes: [], selectedId: null, dragged: null };

  /* ---------- 注入样式（stg- 前缀，零冲突） ---------- */
  var STYLE = '' +
    '#stg-overlay{position:fixed;inset:0;z-index:9999;display:flex;flex-direction:column;background:#0f1117;color:#e8e6e3;font-family:system-ui,sans-serif}' +
    '#stg-overlay .stg-bar{display:flex;align-items:center;gap:10px;padding:8px 14px;border-bottom:1px solid #2a2d36;background:#16181f;flex-wrap:wrap}' +
    '#stg-overlay .stg-title{font-weight:600;font-size:15px;margin-right:auto;white-space:nowrap;overflow:hidden;text-overflow:ellipsis;max-width:40vw}' +
    '#stg-overlay button{background:#262a35;color:#e8e6e3;border:1px solid #3a3f4d;border-radius:6px;padding:5px 12px;font-size:13px;cursor:pointer}' +
    '#stg-overlay button:hover{background:#313747}' +
    '#stg-overlay button.stg-primary{background:#4b6ef5;border-color:#4b6ef5}' +
    '#stg-overlay button.stg-on{background:#1f6f43;border-color:#1f6f43}' +
    '#stg-overlay .stg-body{display:flex;flex:1;min-height:0}' +
    '#stg-overlay .stg-canvas-wrap{flex:1;position:relative;overflow:auto;background:#14161d;' +
    'background-image:radial-gradient(#262a34 1px,transparent 1px);background-size:26px 26px}' +
    '#stg-overlay svg.stg-edges{position:absolute;inset:0;width:100%;height:100%;pointer-events:none}' +
    '#stg-overlay svg.stg-edges path{fill:none;stroke:#5a6274;stroke-width:1.6;pointer-events:stroke;cursor:pointer}' +
    '#stg-overlay svg.stg-edges path.stg-self{stroke:#8a5a3a}' +
    '#stg-overlay .stg-node{position:absolute;min-width:150px;max-width:230px;background:#1d2130;border:1px solid #38405a;border-radius:9px;padding:8px 10px;box-shadow:0 3px 12px rgba(0,0,0,.35);cursor:grab;user-select:none}' +
    '#stg-overlay .stg-node.stg-sel{border-color:#4b6ef5;box-shadow:0 0 0 2px rgba(75,110,245,.35)}' +
    '#stg-overlay .stg-node .stg-node-ch{font-size:10.5px;color:#8fa0c8;letter-spacing:.4px;text-transform:uppercase;margin-bottom:2px}' +
    '#stg-overlay .stg-node .stg-node-id{font-size:10px;color:#6b7388;margin-bottom:1px}' +
    '#stg-overlay .stg-node .stg-node-title{font-size:13.5px;font-weight:600;line-height:1.3}' +
    '#stg-overlay .stg-node .stg-node-exits{font-size:10.5px;color:#7ec8a0;margin-top:3px}' +
    '#stg-overlay .stg-node .stg-node-bad{font-size:10.5px;color:#e0a05a;margin-top:2px}' +
    '#stg-overlay .stg-node .stg-node-del{position:absolute;top:-9px;right:-9px;width:19px;height:19px;border-radius:50%;background:#b3403e;border:1px solid #d05a58;color:#fff;font-size:11px;line-height:1;display:none;align-items:center;justify-content:center;padding:0;cursor:pointer}' +
    '#stg-overlay .stg-node:hover .stg-node-del{display:flex}' +
    '#stg-overlay .stg-insp{width:300px;flex-shrink:0;border-left:1px solid #2a2d36;background:#16181f;overflow-y:auto;padding:12px}' +
    '#stg-overlay .stg-insp h4{margin:4px 0 8px;font-size:13px;color:#9fb0d8}' +
    '#stg-overlay .stg-insp label{display:block;font-size:12px;color:#aab;margin:8px 0 3px}' +
    '#stg-overlay .stg-insp input,#stg-overlay .stg-insp textarea,#stg-overlay .stg-insp select{width:100%;box-sizing:border-box;background:#0f1117;border:1px solid #38405a;color:#e8e6e3;border-radius:6px;padding:6px 8px;font-size:13px}' +
    '#stg-overlay .stg-insp textarea{min-height:64px;resize:vertical}' +
    '#stg-overlay .stg-exit{display:flex;flex-direction:column;gap:5px;border:1px solid #313748;border-radius:7px;padding:7px;margin:6px 0;background:#1c202b}' +
    '#stg-overlay .stg-exit .stg-exit-tools{display:flex;gap:5px;justify-content:flex-end}' +
    '#stg-overlay .stg-exit button{font-size:11px;padding:2px 8px}' +
    '#stg-overlay .stg-hint{font-size:11.5px;color:#8b95a8;padding:4px 0}' +
    '#stg-overlay .stg-status{position:absolute;left:14px;bottom:12px;background:#1c202b;border:1px solid #3a3f4d;color:#a9e3bb;font-size:12px;padding:6px 12px;border-radius:8px;display:none;z-index:20}' +
    '#stg-overlay .stg-status.stg-err{color:#f0a5a5;border-color:#6d3d3d}' +
    '#stg-overlay .stg-empty{padding:26px;color:#8b95a8;font-size:13px}';

  function injectStyle() {
    var el = document.createElement('style');
    el.id = 'stg-style';
    el.textContent = STYLE;
    document.head.appendChild(el);
  }

  /* ---------- 一个小型 fetch 封装（同 app.js 同源） ---------- */
  function api(path, opts) {
    opts = opts || {};
    var h = Object.assign({ 'Content-Type': 'application/json' }, opts.headers || {});
    try {
      var tok = localStorage.getItem('kaleido_token') || localStorage.getItem('token') || '';
      if (tok) h.Authorization = 'Bearer ' + tok;
    } catch (e) {}
    return fetch(API + path, Object.assign({}, opts, { headers: h }))
      .then(function (r) {
        if (!r.ok) return r.text().then(function (t) { throw new Error('HTTP ' + r.status + ' ' + t.slice(0, 200)); });
        return r.json().catch(function () { return {}; });
      });
  }

  /* ---------- 打开面板 ---------- */
  function openEditor(packId) {
    if (document.getElementById('stg-overlay')) closeEditor(false);
    state.packId = packId;
    state.mode = 'view';
    api('/packs/' + encodeURIComponent(packId)).then(function (pack) {
      if (pack && pack.id) {
        state.pack = pack;
        buildOverlay();
        render();
      } else {
        toast('未找到包: ' + packId, true);
      }
    }).catch(function (e) { toast('加载失败: ' + e.message, true); });
  }

  function buildOverlay() {
    var o = document.createElement('div');
    o.id = 'stg-overlay';
    o.innerHTML = '' +
      '<div class="stg-bar">' +
      '<span class="stg-title" id="stg-title">剧本图编辑器</span>' +
      '<button id="stg-open-view" class="stg-primary">只读查看</button>' +
      '<button id="stg-open-edit">进入编辑</button>' +
      '<button id="stg-save" hidden>保存到包</button>' +
      '<button id="stg-add-node" hidden>+ 新增节点</button>' +
      '<button id="stg-reset-layout">重排</button>' +
      '<button id="stg-close">关闭</button>' +
      '</div>' +
      '<div class="stg-body">' +
      '<div class="stg-canvas-wrap" id="stg-canvas"><svg class="stg-edges" id="stg-edges"></svg></div>' +
      '<aside class="stg-insp" id="stg-insp"></aside>' +
      '</div>' +
      '<div class="stg-status" id="stg-status"></div>';
    document.body.appendChild(o);
    document.getElementById('stg-close').onclick = function () { closeEditor(); };
    document.getElementById('stg-open-view').onclick = function () { setMode('view'); };
    document.getElementById('stg-open-edit').onclick = function () { setMode('edit'); };
    document.getElementById('stg-reset-layout').onclick = function () { clearLayout(); render(); };
    document.getElementById('stg-add-node').onclick = addNode;
    document.getElementById('stg-save').onclick = save;
  }

  function setMode(m) {
    state.mode = m;
    var ev = document.getElementById('stg-open-edit');
    var vw = document.getElementById('stg-open-view');
    var sv = document.getElementById('stg-save');
    var an = document.getElementById('stg-add-node');
    if (m === 'edit') {
      ev.classList.add('stg-on'); vw.classList.remove('stg-on');
      sv.hidden = false; an.hidden = false;
      toast('编辑模式：拖拽节点 / 点击节点编辑出口');
    } else {
      ev.classList.remove('stg-on'); vw.classList.add('stg-on');
      sv.hidden = true; an.hidden = true;
    }
    render();
  }

  /* ---------- 布局 ---------- */
  function getLayout() {
    try { return JSON.parse(localStorage.getItem(LS_KEY + state.packId) || '{}') || {}; } catch (e) { return {}; }
  }
  function setLayout(x) { try { localStorage.setItem(LS_KEY + state.packId, JSON.stringify(x)); } catch (e) {} }
  function clearLayout() { try { localStorage.removeItem(LS_KEY + state.packId); } catch (e) {} }

  function autoLayout(nodes) {
    // 按 chapterId 分列，宽 260；缺章用 "∅"
    var cols = {}, order = [], i;
    for (i = 0; i < nodes.length; i++) {
      var c = nodes[i].chapterId || '∅';
      if (!cols[c]) { cols[c] = []; order.push(c); }
      cols[c].push(nodes[i]);
    }
    var x = 40, y, lay = {};
    order.forEach(function (c) {
      y = 40;
      cols[c].forEach(function (n) { lay[n.id] = { x: x, y: y }; y += 92; });
      x += 270;
    });
    return lay;
  }

  /* ---------- 渲染 ---------- */
  function render() {
    var pack = state.pack, i;
    document.getElementById('stg-title').textContent = '剧本图 · ' + (pack.title || pack.id);
    var saved = getLayout();
    var used = Object.keys(saved).length ? saved : null;
    if (!used) {
      used = autoLayout(pack.nodes || []);
      setLayout(used);
    }
    state.doc = used;
    // 边
    drawEdges(pack.nodes || [], used, true);
    drawNodes(pack.nodes || [], used);
    renderInspector();
  }

  function drawEdges(nodes, lay, fit) {
    var svg = document.getElementById('stg-edges');
    svg.setAttribute('width', svg.clientWidth || window.innerWidth);
    svg.setAttribute('height', svg.clientHeight || window.innerHeight);
    var html = '';
    for (var i = 0; i < nodes.length; i++) {
      var n = nodes[i], s = lay[n.id];
      if (!s) continue;
      (n.exit || []).forEach(function (ex) {
        var t = lay[ex.next];
        var srcOut = { x: s.x + 150, y: s.y + 22 };
        var dst = t ? { x: t.x, y: t.y + 22 } : { x: s.x + 190, y: s.y + 60 };
        var dx = Math.max(26, Math.abs(dst.x - s.x) / 2);
        var d = 'M' + srcOut.x + ' ' + srcOut.y + ' C' + (srcOut.x + dx) + ' ' + srcOut.y + ',' + (dst.x - dx) + ' ' + dst.y + ',' + dst.x + ' ' + dst.y;
        var cls = ex.next === n.id ? 'stg-self' : '';
        var tip = (ex.when || '') + ' → ' + (ex.next || '∅') + (t ? '' : (t === undefined ? ' (缺失目标)' : ''));
        html += '<path class="' + cls + '" d="' + d + '" data-from="' + n.id + '" data-next="' + ex.next + '"><title>' + tip + '</title></path>';
      });
    }
    svg.innerHTML = html;
    svg.querySelectorAll('path').forEach(function (p) {
      p.addEventListener('click', function () {
        if (state.mode !== 'edit') return;
        var n = p.dataset.from, nx = p.dataset.next;
        selectNode(n);
        // 聚焦对应出口编辑
        var arr = (state.pack.nodes.find(z => z.id === n) || {}).exit || [];
        var idx = -1;
        for (var k = 0; k < arr.length; k++) if (arr[k].next === nx) { idx = k; break; }
        renderInspector(n, idx);
      });
    });
  }

  function nodeDom(n) {
    var d = document.createElement('div');
    d.dataset.id = n.id;
    d.className = 'stg-node' + (state.selectedId === n.id ? ' stg-sel' : '');
    d.style.left = (state.doc[n.id] ? state.doc[n.id].x : 40) + 'px';
    d.style.top = (state.doc[n.id] ? state.doc[n.id].y : 40) + 'px';
    var exitsHtml = '';
    if (n.exit && n.exit.length) {
      exitsHtml = '<div class="stg-node-exits">↷ ' + n.exit.map(function (e) { return e.next; }).join(', ') + '</div>';
    }
    var bad = '';
    var missing = (n.exit || []).filter(function (e) { return !(state.pack.nodes || []).some(function (z) { return z.id === e.next; }); });
    if (missing.length) bad = '<div class="stg-node-bad">⚠ 缺失目标: ' + missing.map(function (e) { return e.next; }).join(', ') + '</div>';
    d.innerHTML = '<div class="stg-node-ch">' + esc(n.chapterId || '∅') + '</div>' +
      '<div class="stg-node-id">' + esc(n.id) + '</div>' +
      '<div class="stg-node-title">' + esc(n.title || n.id) + '</div>' + exitsHtml + bad +
      '<button class="stg-node-del" title="删除节点">×</button>';
    d.title = 'nodes[' + n.id + ']  → ' + (n.exit || []).map(function (e) { return (e.when || '') + ' → ' + e.next; }).join(' | ');
    d.addEventListener('mousedown', function (ev) { if (state.mode === 'edit' && ev.button === 0) startDrag(ev, n.id); });
    d.addEventListener('click', function (ev) {
      if (ev.target.classList.contains('stg-node-del')) { ev.stopPropagation(); deleteNode(n.id); return; }
      selectNode(n.id);
    });
    d.querySelector('.stg-node-del').addEventListener('mousedown', function (ev) { ev.stopPropagation(); });
    return d;
  }

  function drawNodes(nodes) {
    var wrap = document.getElementById('stg-canvas');
    wrap.querySelectorAll('.stg-node').forEach(function (el) { el.remove(); });
    for (var i = 0; i < nodes.length; i++) wrap.appendChild(nodeDom(nodes[i]));
  }

  /* ---------- 拖拽 ---------- */
  function startDrag(ev, id) {
    var doc = state.doc;
    var node = doc[id];
    if (!node) return;
    var sx = ev.clientX, sy = ev.clientY, ox = node.x, oy = node.y;
    state.dragged = id;
    document.body.style.cursor = 'grabbing';
    function onMove(e) {
      var nx = ox + (e.clientX - sx), ny = oy + (e.clientY - sy);
      node.x = Math.max(0, Math.round(nx)); node.y = Math.max(0, Math.round(ny));
      var all = document.getElementById('stg-canvas').querySelectorAll('.stg-node');
      all.forEach(function (nd) { if (nd.dataset.id === id) { nd.style.left = node.x + 'px'; nd.style.top = node.y + 'px'; } });
      drawEdges(state.pack.nodes || [], state.doc);
    }
    function onUp() {
      document.removeEventListener('mousemove', onMove);
      document.removeEventListener('mouseup', onUp);
      document.body.style.cursor = '';
      setLayout(state.doc);
      state.dragged = null;
    }
    document.addEventListener('mousemove', onMove);
    document.addEventListener('mouseup', onUp);
    ev.preventDefault();
  }

  /* ---------- 选择/inspector ---------- */
  function selectNode(id) {
    state.selectedId = id;
    render();
  }

  function renderInspector(nodeId, edgeIdx) {
    var insp = document.getElementById('stg-insp');
    var n = nodeId ? (state.pack.nodes || []).find(function (z) { return z.id === nodeId; }) : (state.pack.nodes || []).find(function (z) { return z.id === state.selectedId; });
    if (!n) { insp.innerHTML = '<div class="stg-empty">点击节点查看/编辑；<br>点击连线可定位到出口。</div>'; return; }
    var chapters = (state.pack.chapters || []).map(function (c) { return c.id; }).concat(state.pack.nodes.map(function (z) { return z.chapterId; })).filter(Boolean);
    var uniq = [], seen = {};
    for (var i = 0; i < chapters.length; i++) { if (!seen[chapters[i]] && chapters[i]) { seen[chapters[i]] = 1; uniq.push(chapters[i]); } }
    var html = '<h4>节点 ' + esc(n.id) + '</h4>' +
      '<label>标题<input id="stg-f-title" value="' + escAttr(n.title || '') + '"></label>' +
      '<label>章节<input id="stg-f-chapter" list="stg-chapters" value="' + escAttr(n.chapterId || '') + '"></label>' +
      '<datalist id="stg-chapters">' + uniq.map(function (c) { return '<option value="' + escAttr(c) + '">'; }).join('') + '</datalist>' +
      '<label>开局文本（entry）<textarea id="stg-f-entry">' + esc(n.entry || '') + '</textarea></label>' +
      '<label>偏移许可 allowedDivergence<input id="stg-f-div" value="' + escAttr(n.allowedDivergence != null ? n.allowedDivergence : '') + '" placeholder="数字，可空"></label>' +
      '<label>锁定 Beats（逗号分隔）<input id="stg-f-locked" value="' + escAttr((n.lockedBeats || []).join(', ')) + '"></label>' +
      '<label>在场角色（逗号分隔）<input id="stg-f-cast" value="' + escAttr((n.presentCharacters || []).join(', ')) + '"></label>' +
      '<h4 style="margin-top:16px">出口（exit 连线）</h4>';
    (n.exit || []).forEach(function (ex, idx) {
      html += '<div class="stg-exit">' +
        '<label>条件 when<input class="stg-ex-when" value="' + escAttr(ex.when || '') + '" data-i="' + idx + '"></label>' +
        '<label>目标 next<select class="stg-ex-next" data-i="' + idx + '">' + nodeOptions(ex.next) + '</select></label>' +
        '<div class="stg-exit-tools"><button class="stg-ex-del" data-i="' + idx + '">删除连线</button></div>' +
        '</div>';
    });
    if (edgeIdx >= 0 && edgeIdx < (n.exit || []).length) html += '<div class="stg-hint" style="color:#8ec8ff">↳ 高亮: 出口 #' + (edgeIdx + 1) + '</div>';
    html += '<button id="stg-add-exit" style="margin-top:6px;width:100%">+ 新增出口连线</button>' +
      '<h4 style="margin-top:18px">节点操作</h4>' +
      '<div style="display:flex;gap:8px;margin-top:8px"><button id="stg-copy-node" style="flex:1">克隆节点</button></div>';
    insp.innerHTML = html;
    document.getElementById('stg-add-exit').onclick = function () { addExit(n); };
    document.getElementById('stg-copy-node').onclick = function () { cloneNode(n); };
    insp.querySelectorAll('.stg-ex-del').forEach(function (b2) {
      b2.onclick = function () {
        var i = +b2.getAttribute('data-i');
        n.exit.splice(i, 1);
        if (edgeIdx === i) edgeIdx = -1;
        renderInspector(n.id, -1); drawEdges(state.pack.nodes, state.doc);
        toast('已删除连线');
      };
    });
    insp.querySelectorAll('.stg-ex-when').forEach(function (inp2) {
      inp2.oninput = function () { n.exit[+inp2.getAttribute('data-i')].when = inp2.value; touchModel(); };
    });
    insp.querySelectorAll('.stg-ex-next').forEach(function (sel2) {
      sel2.onchange = function () { n.exit[+sel2.getAttribute('data-i')].next = sel2.value; touchModel(); };
    });
    // 节点字段 change 收集（保存时真正写回，避免拖拽布局时误存）
    ['stg-f-title', 'stg-f-chapter', 'stg-f-entry', 'stg-f-div', 'stg-f-locked', 'stg-f-cast'].forEach(function (id) {
      var el = document.getElementById(id);
      if (el) el.addEventListener('input', function () {
        if (id === 'stg-f-title') n.title = el.value;
        else if (id === 'stg-f-chapter') n.chapterId = el.value;
        else if (id === 'stg-f-entry') n.entry = el.value;
        else if (id === 'stg-f-div') { n.allowedDivergence = el.value === '' ? undefined : +el.value; }
        else if (id === 'stg-f-locked') n.lockedBeats = el.value.split(',').map(function (z) { return z.trim(); }).filter(Boolean);
        else if (id === 'stg-f-cast') n.presentCharacters = el.value.split(',').map(function (z) { return z.trim(); }).filter(Boolean);
        touchModel();
      });
    });
    document.getElementById('stg-save').onclick = save;
  }

  function nodeOptions(cur) {
    var opts = '<option value="">(删除出口)</option>';
    (state.pack.nodes || []).forEach(function (z) {
      opts += '<option value="' + escAttr(z.id) + '"' + (z.id === cur ? ' selected' : '') + '>' + esc(z.id) + (z.title ? ' · ' + esc(z.title) : '') + '</option>';
    });
    return opts;
  }

  function addExit(n) {
    var nextId = null;
    var others = (state.pack.nodes || []).filter(function (z) { return z.id !== n.id; });
    if (!others.length) return toast('没有其他节点可连', true);
    nextId = others[0].id;
    n.exit = n.exit || [];
    n.exit.push({ id: 'e' + Date.now(), when: '', next: nextId });
    touchModel();
    renderInspector(n.id);
    toast('已添加出口连线（可改 when 条件与目标）');
  }

  function addNode() {
    var pack = state.pack;
    var nid = 'n' + Date.now().toString(36);
    var ch = (pack.chapters && pack.chapters.length) ? pack.chapters[0].id : '';
    pack.nodes = pack.nodes || [];
    pack.nodes.push({ id: nid, chapterId: ch, title: '新节点 ' + pack.nodes.length, entry: '', exit: [] });
    state.selectedId = nid;
    touchModel();
    render();
    toast('已新增节点 ' + nid + '；在右侧编辑标题');
  }
  function cloneNode(n) {
    var pack = state.pack;
    var cid = n.id + '-c' + Math.floor(Math.random() * 999);
    var cp = JSON.parse(JSON.stringify(n));
    cp.id = cid;
    cp.exit = [];
    cp.title = (cp.title || '') + ' (副本)';
    pack.nodes = pack.nodes || [];
    pack.nodes.push(cp);
    state.selectedId = cid;
    touchModel();
    render();
    toast('已克隆为 ' + cid);
  }
  function deleteNode(id) {
    if (!state.pack || !confirm('删除节点 ' + id + '？引用它的连线会被移除。')) return;
    var pack = state.pack;
    pack.nodes = (pack.nodes || []).filter(function (n) { return n.id !== id; });
    (pack.nodes || []).forEach(function (n) {
      if (n.exit) n.exit = n.exit.filter(function (e) { return e.next !== id; });
    });
    if (pack.entry && pack.entry.startNodeId === id) { if (pack.entry) pack.entry.startNodeId = null; }
    if (state.selectedId === id) state.selectedId = null;
    touchModel();
    render();
    toast('已删除节点 ' + id + ' 并清理引用');
  }

  /* ---------- 模型 touch / 保存 ---------- */
  function touchModel() {
    var pack = state.pack;
    if (!pack) return;
    // 空 id 出口视为未定义（用户删除边时 select 选空后标记）
    (pack.nodes || []).forEach(function (n) {
      n.exit = (n.exit || []).filter(function (e) { return e && e.next !== ''; });
    });
  }

  function saveFieldsToModel() { /* 拖拽后布局只存 localStorage */ }

  function save() {
    var pack = state.pack;
    if (!pack) return;
    saveFieldsToModel();
    api('/packs', { method: 'POST', body: JSON.stringify(pack) })
      .then(function (res) {
        toast('已保存包 ' + (res && res.id ? res.id : '') , false);
        state.savedAt = Date.now();
      })
      .catch(function (e) { toast('保存失败: ' + e.message, true); });
  }

  /* ---------- 关闭/工具 ---------- */
  function closeEditor() {
    var o = document.getElementById('stg-overlay');
    if (o) o.remove();
  }

  function esc(s) { return String(s == null ? '' : s).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;'); }
  function escAttr(s) { return esc(s); }

  function toast(msg, isErr) {
    var el = document.getElementById('stg-status');
    if (!el) return;
    el.textContent = msg;
    el.classList.toggle('stg-ok', !isErr);
    el.style.display = 'block';
    clearTimeout(toast._t);
    toast._t = setTimeout(function () { el.style.display = 'none'; }, 2600);
  }

  /* ---------- 入口接线 ---------- */
  function wireEntry() {
    injectStyle();
    document.addEventListener('click', function (ev) {
      var btn = ev.target.closest && ev.target.closest('#st-pack-graph');
      if (!btn) return;
      var id = currentPackIdFromDom();
      if (!id) { alert('未能识别当前剧本包 id，请刷新后重试'); return; }
      openEditor(id);
    });
  }

  function currentPackIdFromDom() {
    var meta = document.getElementById('st-pack-detail-meta');
    if (!meta) return '';
    var parts = meta.textContent.split(' · ');
    var last = parts[parts.length - 1] || '';
    return last.trim();
  }

  // 挂载
  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', wireEntry);
  else wireEntry();

  // 暴露（便于调试）
  window.storyGraphEditor = { open: openEditor, getState: function () { return state; } };
})();