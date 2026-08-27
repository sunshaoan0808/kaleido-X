/**
 * src/js/insight.js — AI 分析域真 ESM 模块（P1-3 S2.14）。
 * 合并 _analysis/_graph/_foreshadow 三片（~1700L）。outward 仅 4 符号：
 * loadAnKinds/loadAnTasks/loadGraph/loadForeshadows，经 Mechanism Y 供
 * _author/_tabs 裸引用零编辑。顶层 DOMContentLoaded init 原样保留。
 * anWorkId 规范留在 _analysis-part（闭包），经 __kaleidoAnState 门面读取。
 */
import { $ } from './dom.js';
import { api } from './api.js';
import { showToast } from './toast.js';
import { showConfirm } from './dialog.js';


/* ================= _analysis-part.js ================= */
/* P4: AI writing analysis (author zone sub-view).
   Backend: /api/v1/analysis/kinds
            /api/v1/works/{work_id}/analysis/tasks  (create/list)
            /api/v1/analysis/tasks/{id}             (get/delete)
            /api/v1/analysis/tasks/{id}/cancel
            /api/v1/analysis/tasks/{id}/suggestions/{sid}/confirm|reject */
  let anItems = [];
  let anKinds = [];
  let anTimer = null;

  const AN_STATUS = {
    queued: '排队中', running: '运行中', succeeded: '已完成',
    failed: '失败', cancelled: '已取消',
  };
  const AN_SUMMARY_LABELS = {
    plot: '情节概要', key_events: '关键事件', characters: '出场角色',
    settings: '设定', uncertain: '不确定项',
  };
  const AN_SUG_STATUS = { pending: '待确认', confirmed: '已确认', rejected: '已拒绝' };


  function anKindLabel(v) {
    const k = anKinds.find((x) => x.value === v);
    return k ? k.label : v;
  }

  function anScopeText(scope) {
    try {
      if (scope && Array.isArray(scope.paths)) return scope.paths.join(', ');
      if (scope && scope.paths) return String(scope.paths);
      return '';
    } catch (e) { return ''; }
  }

  async function loadAnKinds() {
    try {
      const r = await api('/api/v1/analysis/kinds');
      anKinds = Array.isArray(r.kinds) ? r.kinds : [];
      const sel = $('an-kind');
      if (!sel) return;
      sel.innerHTML = '';
      for (const k of anKinds) {
        const o = document.createElement('option');
        o.value = k.value;
        o.textContent = k.label;
        o.title = k.desc || '';
        sel.appendChild(o);
      }
    } catch (e) { /* kinds 失败不阻塞 */ }
  }

  async function loadAnTasks() {
    const list = $('an-list');
    if (!list) return;
    list.textContent = '加载任务…';
    try {
      const r = await api('/api/v1/works/' + encodeURIComponent((window.__kaleidoAnState ? window.__kaleidoAnState.workId : 'default')) + '/analysis/tasks');
      anItems = Array.isArray(r.tasks) ? r.tasks : [];
      renderAnList();
      const busy = anItems.some((t) => t.status === 'queued' || t.status === 'running');
      if (anTimer) { clearTimeout(anTimer); anTimer = null; }
      if (busy) anTimer = setTimeout(loadAnTasks, 2500);
    } catch (e) {
      list.textContent = '加载失败: ' + e.message;
    }
  }

  function renderAnList() {
    const list = $('an-list');
    if (!list) return;
    list.innerHTML = '';
    if (!anItems.length) {
      const el = document.createElement('div');
      el.className = 'muted sm';
      el.textContent = '（暂无分析任务，选择任务类型与范围后发起）';
      list.appendChild(el);
      return;
    }
    for (const t of anItems) {
      const row = document.createElement('div');
      row.className = 'an-row';
      const left = document.createElement('div');
      left.className = 'an-row-main';
      const title = document.createElement('div');
      title.className = 'an-row-title';
      title.textContent = anKindLabel(t.kind);
      const meta = document.createElement('div');
      meta.className = 'muted sm';
      meta.textContent = '范围: ' + (anScopeText(t.scope) || '—') + ' · 创建: ' + (t.created_at || '—');
      left.appendChild(title);
      left.appendChild(meta);
      const badge = document.createElement('span');
      badge.className = 'an-badge an-badge-' + (AN_STATUS[t.status] ? t.status : 'queued');
      badge.textContent = AN_STATUS[t.status] || t.status;
      const ops = document.createElement('div');
      ops.className = 'an-row-ops';
      const bView = document.createElement('button');
      bView.type = 'button'; bView.className = 'ghost sm'; bView.textContent = '查看';
      bView.onclick = () => anViewTask(t.id);
      ops.appendChild(bView);
      if (t.status === 'queued' || t.status === 'running') {
        const bCancel = document.createElement('button');
        bCancel.type = 'button'; bCancel.className = 'ghost sm'; bCancel.textContent = '取消';
        bCancel.onclick = () => anCancelTask(t.id);
        ops.appendChild(bCancel);
      }
      const bDel = document.createElement('button');
      bDel.type = 'button'; bDel.className = 'ghost sm danger'; bDel.textContent = '删除';
      bDel.onclick = () => anDeleteTask(t.id);
      ops.appendChild(bDel);
      row.appendChild(left);
      row.appendChild(badge);
      row.appendChild(ops);
      list.appendChild(row);
    }
  }

  async function anStart() {
    const sel = $('an-kind'), area = $('an-scope-paths'), btn = $('an-start-btn');
    if (!sel || !area) return;
    const kind = sel.value;
    if (!kind) { showToast('请先选择任务类型', 'warning'); return; }
    const paths = area.value.split('\n').map((s) => s.trim()).filter(Boolean);
    if (!paths.length) { showToast('请填写至少一个范围路径（相对路径，每行一个，如 ch01.md）', 'warning'); return; }
    if (btn) btn.disabled = true;
    try {
      await api('/api/v1/works/' + encodeURIComponent((window.__kaleidoAnState ? window.__kaleidoAnState.workId : 'default')) + '/analysis/tasks', {
        method: 'POST',
        body: JSON.stringify({ kind, scope: { paths } }),
      });
      area.value = '';
      await loadAnTasks();
    } catch (e) {
      showToast('发起失败: ' + e.message, 'error');
    } finally {
      if (btn) btn.disabled = false;
    }
  }

  async function anCancelTask(id) {
    try {
      await api('/api/v1/analysis/tasks/' + encodeURIComponent(id) + '/cancel', { method: 'POST' });
      await loadAnTasks();
    } catch (e) {
      showToast('取消失败: ' + e.message, 'error');
    }
  }

  async function anDeleteTask(id) {
    if (!await showConfirm('确认删除该分析任务？')) return;
    try {
      await api('/api/v1/analysis/tasks/' + encodeURIComponent(id), { method: 'DELETE' });
      await loadAnTasks();
    } catch (e) {
      showToast('删除失败: ' + e.message, 'error');
    }
  }

  function anRenderRich(container, value) {
    if (value == null || value === '') {
      container.textContent = '—';
      return;
    }
    if (typeof value === 'string' || typeof value === 'number' || typeof value === 'boolean') {
      container.textContent = String(value);
      return;
    }
    if (Array.isArray(value)) {
      const ul = document.createElement('ul');
      ul.className = 'an-value-list';
      for (const it of value) {
        const li = document.createElement('li');
        anRenderRich(li, it);
        ul.appendChild(li);
      }
      container.appendChild(ul);
      return;
    }
    if (typeof value === 'object') {
      const dl = document.createElement('div');
      dl.className = 'an-value-object';
      for (const k of Object.keys(value)) {
        const row = document.createElement('div');
        row.className = 'an-value-row';
        const kk = document.createElement('span');
        kk.className = 'muted sm';
        kk.textContent = k + ': ';
        row.appendChild(kk);
        const vv = document.createElement('div');
        vv.className = 'an-value-inner';
        anRenderRich(vv, value[k]);
        row.appendChild(vv);
        dl.appendChild(row);
      }
      container.appendChild(dl);
      return;
    }
    container.textContent = String(value);
  }

  function anRenderValueBlock(label, value) {
    const wrap = document.createElement('div');
    wrap.className = 'an-block';
    const h = document.createElement('div');
    h.className = 'an-block-title';
    h.textContent = label;
    const body = document.createElement('div');
    body.className = 'an-block-body';
    anRenderRich(body, value);
    wrap.appendChild(h);
    wrap.appendChild(body);
    return wrap;
  }

  function anRenderEvidence(ev) {
    const wrap = document.createElement('div');
    wrap.className = 'an-evidence';
    const quote = document.createElement('div');
    quote.className = 'an-quote';
    quote.textContent = '“' + (ev.quote || '') + '”';
    const meta = document.createElement('div');
    meta.className = 'muted sm';
    meta.textContent = '来源: ' + (ev.source || '—') + (ev.line != null ? ' · 行 ' + ev.line : '') + (ev.note ? ' · ' + ev.note : '');
    wrap.appendChild(quote);
    wrap.appendChild(meta);
    return wrap;
  }

  function anRenderTimelineBoard(t) {
    const board = document.createElement('div');
    board.className = 'tl-board';
    const head = document.createElement('div');
    head.className = 'an-block-title';
    head.textContent = '⏱ 时间线视图';
    board.appendChild(head);
    const rows = (Array.isArray(t.suggestions) ? t.suggestions : [])
      .filter((sug) => {
        const p = sug && sug.payload;
        if (p == null) return false;
        if (typeof p === 'object' && !Array.isArray(p)) {
          return (p.time != null && p.time !== '') || (p.event != null && p.event !== '');
        }
        return true;
      })
      .map((sug) => {
        const p = sug.payload;
        const time = (p && typeof p === 'object' && !Array.isArray(p) && p.time) ? String(p.time) : '';
        return { sug, time };
      });
    rows.sort((a, b) => {
      if (a.time && !b.time) return -1;
      if (!a.time && b.time) return 1;
      return a.time < b.time ? -1 : (a.time > b.time ? 1 : 0);
    });
    for (const row of rows) {
      const sug = row.sug;
      const p = sug.payload;
      const node = document.createElement('div');
      node.className = 'tl-node';
      const timeBadge = document.createElement('div');
      timeBadge.className = 'tl-time';
      timeBadge.textContent = row.time || '—';
      node.appendChild(timeBadge);
      if (typeof p === 'object' && p && !Array.isArray(p)) {
        const evText = p.event || p.text || p.desc;
        if (evText != null && evText !== '') {
          const bodyEl = document.createElement('div');
          bodyEl.className = 'tl-event';
          bodyEl.textContent = String(evText);
          node.appendChild(bodyEl);
        }
        if (p.note != null && p.note !== '') {
          const noteEl = document.createElement('div');
          noteEl.className = 'tl-note muted sm';
          noteEl.textContent = String(p.note);
          node.appendChild(noteEl);
        }
      } else {
        const richEl = document.createElement('div');
        richEl.className = 'tl-event';
        anRenderRich(richEl, p);
        node.appendChild(richEl);
      }
      if (sug.status) {
        const st = document.createElement('span');
        st.className = 'an-badge an-badge-' + sug.status;
        st.textContent = AN_SUG_STATUS[sug.status] || sug.status;
        node.appendChild(st);
      }
      board.appendChild(node);
    }
    return board;
  }

  async function anViewTask(id) {
    const modal = $('an-modal');
    if (!modal) return;
    modal.classList.remove('hidden');
    const body = $('an-modal-body');
    if (body) body.textContent = '加载中…';
    try {
      const r = await api('/api/v1/analysis/tasks/' + encodeURIComponent(id));
      const t = r.task || r;
      const body2 = $('an-modal-body');
      if (!body2) return;
      body2.innerHTML = '';
      const kindLine = document.createElement('div');
      kindLine.className = 'muted sm';
      kindLine.textContent = '类型: ' + anKindLabel(t.kind) + ' · 状态: ' + (AN_STATUS[t.status] || t.status) + (t.failure ? ' · 失败原因: ' + t.failure : '');
      body2.appendChild(kindLine);
      // timeline board
      if (t.kind === 'timeline-analysis' && Array.isArray(t.suggestions) && t.suggestions.length) {
        body2.appendChild(anRenderTimelineBoard(t));
      }
      // summary
      const sum = t.summary && typeof t.summary === 'object' ? t.summary : (t.summary ? { summary: t.summary } : null);
      if (sum) {
        const sHead = document.createElement('div');
        sHead.className = 'an-block-title';
        sHead.textContent = '摘要';
        body2.appendChild(sHead);
        for (const key of Object.keys(AN_SUMMARY_LABELS)) {
          if (sum[key] != null && sum[key] !== '') {
            body2.appendChild(anRenderValueBlock(AN_SUMMARY_LABELS[key], sum[key]));
          }
        }
      }
      // evidence
      if (Array.isArray(t.evidence) && t.evidence.length) {
        const eHead = document.createElement('div');
        eHead.className = 'an-block-title';
        eHead.textContent = '原文证据 (' + t.evidence.length + ')';
        body2.appendChild(eHead);
        for (const ev of t.evidence) body2.appendChild(anRenderEvidence(ev));
      }
      // suggestions
      if (Array.isArray(t.suggestions) && t.suggestions.length) {
        const sHead = document.createElement('div');
        sHead.className = 'an-block-title';
        sHead.textContent = '建议条目 (' + t.suggestions.length + ')';
        body2.appendChild(sHead);
        for (const sug of t.suggestions) {
          const card = document.createElement('div');
          card.className = 'an-sug an-sug-' + (sug.status || 'pending');
          const head = document.createElement('div');
          head.className = 'an-sug-head';
          const kindSpan = document.createElement('strong');
          kindSpan.textContent = (sug.kind || '通用') + ' · ';
          const stSpan = document.createElement('span');
          stSpan.className = 'an-badge an-badge-' + (sug.status || 'pending');
          stSpan.textContent = AN_SUG_STATUS[sug.status] || sug.status || '待确认';
          head.appendChild(kindSpan);
          head.appendChild(stSpan);
          const payload = document.createElement('div');
          payload.className = 'an-sug-payload';
          anRenderRich(payload, sug.payload);
          card.appendChild(head);
          card.appendChild(payload);
          if (sug.status === 'pending') {
            const ops = document.createElement('div');
            ops.className = 'an-row-ops';
            const bOk = document.createElement('button');
            bOk.type = 'button'; bOk.className = 'ghost sm'; bOk.textContent = '✓ 确认';
            bOk.onclick = () => anSetSuggestion(t.id, sug.id, 'confirm');
            const bNo = document.createElement('button');
            bNo.type = 'button'; bNo.className = 'ghost sm danger'; bNo.textContent = '✕ 拒绝';
            bNo.onclick = () => anSetSuggestion(t.id, sug.id, 'reject');
            ops.appendChild(bOk);
            ops.appendChild(bNo);
            card.appendChild(ops);
          }
          body2.appendChild(card);
        }
      }
    } catch (e) {
      const body2 = $('an-modal-body');
      if (body2) body2.textContent = '加载失败: ' + e.message;
    }
  }

  async function anSetSuggestion(taskId, sugId, action) {
    try {
      await api('/api/v1/analysis/tasks/' + encodeURIComponent(taskId) + '/suggestions/' + encodeURIComponent(sugId) + '/' + action, { method: 'POST' });
      await anViewTask(taskId);
      await loadAnTasks();
    } catch (e) {
      showToast('操作失败: ' + e.message, 'error');
    }
  }

  function anCloseModal() {
    const modal = $('an-modal');
    if (modal) modal.classList.add('hidden');
  }

  /* ---- 章节文件选择器（2026-08-06）：替代手输相对路径。
         点「选章节」→ 弹层列出当前项目 *.md / *.txt 文件，可搜索、多选，
         勾选后把路径写入 an-scope-paths。数据源 GET /api/v1/works?path=&depth=1 */
  let anPickFiles = [];
  let anPickSel = new Set();

  async function anLoadProjectFiles() {
    try {
      const q = new URLSearchParams();
      const pid = azSelectedProjectId;
      if (pid) q.set('path', 'projects/' + pid);
      q.set('depth', '3');
      const tree = await api('/api/v1/works?' + q.toString());
      anPickFiles = [];
      const walk = (kids, prefix) => {
        for (const c of kids || []) {
          const p = c.path || (prefix + c.name);
          if (c.kind === 'dir') walk(c.children, p + '/');
          else if (/\.(md|txt)$/i.test(c.name || '')) anPickFiles.push({ name: c.name || c.path, path: p });
        }
      };
      walk(tree.children || [], '');
      return anPickFiles;
    } catch (e) {
      return [];
    }
  }

  function anRenderPickModal() {
    const overlay = $('an-pick-overlay');
    if (!overlay) return;
    const q = String($('an-pick-search') ? $('an-pick-search').value : '').trim().toLowerCase();
    const list = $('an-pick-list');
    if (!list) return;
    list.innerHTML = '';
    const hits = anPickFiles.filter((f) => !q || f.name.toLowerCase().includes(q) || f.path.toLowerCase().includes(q));
    if (!hits.length) {
      list.innerHTML = '<p class="muted sm">没有匹配的章节文件</p>';
      return;
    }
    const row = document.createElement('label');
    row.className = 'an-pick-row an-pick-all';
    const ckAll = document.createElement('input');
    ckAll.type = 'checkbox';
    ckAll.checked = hits.length > 0 && hits.every((f) => anPickSel.has(f.path));
    ckAll.onchange = () => {
      for (const f of hits) {
        if (ckAll.checked) anPickSel.add(f.path);
        else anPickSel.delete(f.path);
      }
      anRenderPickModal();
    };
    row.appendChild(ckAll);
    const sp = document.createElement('span');
    sp.textContent = '全选（' + hits.length + '）';
    row.appendChild(sp);
    list.appendChild(row);
    hits.forEach((f) => {
      const r = document.createElement('label');
      r.className = 'an-pick-row';
      const ck = document.createElement('input');
      ck.type = 'checkbox';
      ck.checked = anPickSel.has(f.path);
      ck.onchange = () => {
        if (ck.checked) anPickSel.add(f.path);
        else anPickSel.delete(f.path);
        const all = $('an-pick-list .an-pick-all input');
        if (all) all.checked = hits.length > 0 && hits.every((x) => anPickSel.has(x.path));
      };
      r.appendChild(ck);
      const nm = document.createElement('span');
      nm.className = 'an-pick-name';
      nm.textContent = f.name;
      nm.title = f.path;
      r.appendChild(nm);
      const pt = document.createElement('span');
      pt.className = 'muted sm';
      pt.textContent = f.path;
      r.appendChild(pt);
      list.appendChild(r);
    });
  }

  async function anOpenPick() {
    await anLoadProjectFiles();
    anPickSel = new Set(String($('an-scope-paths') ? $('an-scope-paths').value : '').split('\n').map((s) => s.trim()).filter(Boolean));
    const overlay = $('an-pick-overlay');
    if (!overlay) return;
    overlay.classList.remove('hidden');
    const srch = $('an-pick-search');
    if (srch) { srch.value = ''; srch.oninput = anRenderPickModal; }
    anRenderPickModal();
    const ok = $('an-pick-ok');
    if (ok) ok.onclick = () => {
      const area = $('an-scope-paths');
      if (area) area.value = [...anPickSel].join('\n');
      const o2 = $('an-pick-overlay');
      if (o2) o2.classList.add('hidden');
    };
    const cancel = $('an-pick-cancel');
    if (cancel) cancel.onclick = () => { const o2 = $('an-pick-overlay'); if (o2) o2.classList.add('hidden'); };
  }

  function initAnalysisView() {
    loadAnKinds();
    loadAnTasks();
    const start = $('an-start-btn');
    if (start) start.onclick = anStart;
    const pick = $('an-pick-btn');
    if (pick) pick.onclick = anOpenPick;
    const close = $('an-modal-close');
    if (close) close.onclick = anCloseModal;
    const pickClose = $('an-pick-close');
    if (pickClose) pickClose.onclick = () => { const o2 = $('an-pick-overlay'); if (o2) o2.classList.add('hidden'); };
    const pickOverlay = $('an-pick-overlay');
    if (pickOverlay) pickOverlay.addEventListener('click', (ev) => { if (ev.target === pickOverlay) pickOverlay.classList.add('hidden'); });
    const modal = $('an-modal');
    if (modal) modal.addEventListener('click', (ev) => { if (ev.target === modal) anCloseModal(); });
    const workSel = $('fs-work');
    if (workSel) workSel.addEventListener('change', loadAnTasks);
  }

  document.addEventListener('DOMContentLoaded', initAnalysisView);

/* S2.12: analysis facade — wand tools (compass/review/assets/image real module)
 * read the author-zone work selection via this accessor (was bare closure ref). */
try {
  window.__kaleidoAnState = { workId: () => (window.__kaleidoAnState ? window.__kaleidoAnState.workId : 'default') };
} catch (_) {}

/* ================= _graph-part.js ================= */
/* P1: Character relationship graph (author zone sub-view).
   Pure-JS canvas renderer; data via /api/v1/works/{work_id}/graph. */
  const REL_STYLE = Object.freeze({
    family:    { label: "亲属", color: "#43e39a" },
    social:    { label: "社交", color: "#438cff" },
    emotional: { label: "情感", color: "#ff5f69" },
    conflict:  { label: "冲突", color: "#ffad42" },
    uncertain: { label: "未确定", color: "#9aa5b5" },
  });

  let gChars = [];
  let gRels = [];
  let gSelChar = null;
  let gLayout = [];
  let grGraphData = null;
  let gSearch = '';
  // S9.24: 好感度打通——graph 角色(UUID) ↔ tavernPack(charId→name) ↔ memoryL4.affinity(charId→0~100)
  let gAffByName = {};   // 角色名 → 好感度 0~100
  let gAffByChar = {};   // charId → 好感度 0~100

  function buildAffinityMap() {
    gAffByName = {};
    gAffByChar = {};
    try {
      // 1) 后端 graph API 已注入 affinity（character.affinity，来自最新 tavern session）
      for (const c of gChars) {
        if (c && typeof c.affinity === 'number') {
          gAffByName[c.name] = c.affinity;
        }
      }
      // 2) 前端全局 tavernSession 覆盖（实时性优先：回合后内存里的最新值）
      const aff = tavernSession && tavernSession.memoryL4 && tavernSession.memoryL4.affinity;
      if (!aff || typeof aff !== 'object') return;
      const charIdToName = {};
      if (tavernPack && Array.isArray(tavernPack.characters)) {
        for (const c of tavernPack.characters) {
          if (c && c.id) charIdToName[String(c.id)] = c.name || String(c.id);
        }
      }
      for (const [charId, v] of Object.entries(aff)) {
        const val = Number(v);
        if (Number.isNaN(val)) continue;
        gAffByChar[String(charId)] = val;
        const name = charIdToName[String(charId)];
        if (name) gAffByName[name] = val;
      }
    } catch (_) { /* 无 session/pack 时保持空映射 */ }
  }

  function grWorkId() {
    const el = $('gr-work');
    if (el && String(el.value).trim()) return String(el.value).trim();
    // 用 workspace 域而非项目 ID（work_id=workspace_id，2026-08-10 修复）
    return azSelectedWorkspaceId || azSelectedProjectId || 'default';
  }

  async function loadGraph() {
    const msg = $('gr-msg');
    if (msg) msg.textContent = '加载关系图…';
    try {
      const r = await api('/api/v1/works/' + encodeURIComponent(grWorkId()) + '/graph');
      grGraphData = r;
      gChars = Array.isArray(r.characters) ? r.characters : [];
      gRels = Array.isArray(r.relationships) ? r.relationships : [];
      gSelChar = null;
      buildAffinityMap();
      renderGraph();
      renderGraphLists();
      if (msg) msg.textContent = gChars.length + ' 角色 · ' + gRels.length + ' 关系';
    } catch (e) {
      if (msg) msg.textContent = '加载失败: ' + e.message;
    }
  }

  // --- layout: simple force-directed (repulsion + spring + center gravity) ---
  function computeLayout() {
    const n = gChars.length;
    if (!n) return [];
    const W = 900, H = 560;
    const pos = gChars.map((_, i) => {
      const a = (i / n) * Math.PI * 2;
      return { x: W / 2 + Math.cos(a) * W * 0.32, y: H / 2 + Math.sin(a) * H * 0.32 };
    });
    const idx = new Map(gChars.map((c, i) => [c.id, i]));
    for (let iter = 0; iter < 180; iter++) {
      const forces = pos.map(() => ({ x: 0, y: 0 }));
      for (let i = 0; i < n; i++) {
        for (let j = i + 1; j < n; j++) {
          let dx = pos[j].x - pos[i].x, dy = pos[j].y - pos[i].y;
          let d2 = dx * dx + dy * dy || 1e-6;
          let d = Math.sqrt(d2);
          let f = 5200 / d2;
          forces[i].x -= (dx / d) * f; forces[i].y -= (dy / d) * f;
          forces[j].x += (dx / d) * f; forces[j].y += (dy / d) * f;
        }
      }
      for (const r of gRels) {
        const a = idx.get(r.from_char), b = idx.get(r.to_char);
        if (a == null || b == null || a === b) continue;
        const dx = pos[b].x - pos[a].x, dy = pos[b].y - pos[a].y;
        const d = Math.sqrt(dx * dx + dy * dy) || 1e-6;
        const f = (d - 150) * 0.02;
        forces[a].x += (dx / d) * f; forces[a].y += (dy / d) * f;
        forces[b].x -= (dx / d) * f; forces[b].y -= (dy / d) * f;
      }
      for (let i = 0; i < n; i++) {
        forces[i].x += (W / 2 - pos[i].x) * 0.004;
        forces[i].y += (H / 2 - pos[i].y) * 0.004;
        pos[i].x += forces[i].x; pos[i].y += forces[i].y;
      }
    }
    return pos;
  }

  function renderGraph() {
    // S9.24: 每次重绘前刷新好感度映射（回合后 tavernSession 已更新）
    buildAffinityMap();
    const cv = $('gr-canvas');
    if (!cv) return;
    const dpr = window.devicePixelRatio || 1;
    const W = cv.clientWidth || 900, H = cv.clientHeight || 560;
    cv.width = W * dpr; cv.height = H * dpr;
    const ctx = cv.getContext('2d');
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, W, H);
    if (!gChars.length) return;
    const grMatch = (x) => !gSearch || x.name.toLowerCase().includes(gSearch) || (x.aliases || []).some((a) => String(a).toLowerCase().includes(gSearch));
    gLayout = computeLayout();
    const idx = new Map(gChars.map((c, i) => [c.id, i]));
    const s = Math.min(W, H) / 900;
    // edges
    ctx.lineWidth = 1.6;
    for (const r of gRels) {
      const a = idx.get(r.from_char), b = idx.get(r.to_char);
      if (a == null || b == null) continue;
      if (gSearch && (!grMatch(gChars[a]) || !grMatch(gChars[b]))) continue;
      const p1 = gLayout[a], p2 = gLayout[b];
      ctx.strokeStyle = (REL_STYLE[r.category] || REL_STYLE.uncertain).color;
      // S9.24: 好感度打通——两端角色平均好感度驱动线宽与透明度
      const affA = gAffByName[gChars[a].name], affB = gAffByName[gChars[b].name];
      const hasAff = (typeof affA === 'number' || typeof affB === 'number');
      const affAvg = hasAff ? (((typeof affA === 'number' ? affA : 50) + (typeof affB === 'number' ? affB : 50)) / 2) : 50;
      const affBoost = (affAvg - 50) / 50; // -1..1
      ctx.lineWidth = hasAff ? 1.6 + affBoost * 2.6 : 1.6;
      ctx.globalAlpha = hasAff ? (r.confirmation_status === 'confirmed' ? 0.5 + (affAvg / 100) * 0.5 : 0.25 + (affAvg / 100) * 0.3) : (r.confirmation_status === 'confirmed' ? 0.85 : 0.4);
      ctx.beginPath();
      ctx.moveTo(p1.x * s + (W - W * s) / 2, p1.y * s + (H - H * s) / 2);
      ctx.lineTo(p2.x * s + (W - W * s) / 2, p2.y * s + (H - H * s) / 2);
      ctx.stroke();
    }
    ctx.globalAlpha = 1;
    // nodes
    const palette = ['#5b8def', '#43e39a', '#ff5f69', '#ffad42', '#b48cff', '#4dd8d8', '#f2c94c', '#e58a8a', '#7ea6ff', '#8fd18f'];
    for (let i = 0; i < gChars.length; i++) {
      const c = gChars[i];
      if (gSearch && !grMatch(c)) continue;
      const x = gLayout[i].x * s + (W - W * s) / 2, y = gLayout[i].y * s + (H - H * s) / 2;
      // S9.24: 好感度驱动节点大小（40~100 时放大，<40 缩小）
      const affVal = gAffByName[c.name];
      const hasAffVal = typeof affVal === 'number';
      const r = hasAffVal
        ? Math.max(8, Math.min(26, 16 + Math.sqrt(c.degree || 0) * 4 + (affVal - 50) / 6))
        : Math.max(10, Math.min(22, 16 + Math.sqrt(c.degree || 0) * 4));
      // 好感度调色：高好感暖金，低好感冷蓝（在基础色上叠加）
      let col = palette[Number(c.color_idx) % palette.length] || palette[0];
      if (hasAffVal) {
        const t = grClamp((affVal - 30) / 70, 0, 1); // 0=低(冷) 1=高(暖)
        col = t >= 0.5 ? '#ffb347' : '#5b8def';
        if (affVal >= 75) col = '#ff9a3c';
        if (affVal < 25) col = '#4a6fa5';
      }
      ctx.beginPath(); ctx.arc(x, y, r, 0, Math.PI * 2);
      ctx.fillStyle = col; ctx.fill();
      ctx.lineWidth = 2;
      ctx.strokeStyle = c.id === gSelChar ? '#ffffff' : 'rgba(0,0,0,0.25)';
      ctx.stroke();
      ctx.fillStyle = '#ffffff';
      ctx.font = '12px sans-serif';
      ctx.textAlign = 'center';
      ctx.fillText(String(c.name || '').slice(0, 6), x, y + r + 14);
      // 好感度数值角标
      if (hasAffVal) {
        ctx.font = '10px sans-serif';
        ctx.fillStyle = affVal >= 60 ? '#ffd88a' : affVal < 35 ? '#9db8e8' : '#cfe0f5';
        ctx.fillText('♥' + affVal, x, y - r - 6);
      }
    }
  }

  function renderGraphLists() {
    const cl = $('gr-char-list'), rl = $('gr-rel-list');
    if (cl) {
      cl.innerHTML = '';
      for (const c of gChars.filter((x) => !gSearch || x.name.toLowerCase().includes(gSearch) || (x.aliases || []).some((a) => String(a).toLowerCase().includes(gSearch)))) {
        const el = document.createElement('div');
        el.className = 'az-item' + (c.id === gSelChar ? ' active' : '');
        el.innerHTML = '<span class="az-title"></span><button type="button" class="ghost sm gr-edit" data-id="" title="编辑">✎</button><button type="button" class="ghost sm danger gr-del" data-id="">✕</button>';
        el.querySelector('.az-title').textContent = c.name + (c.aliases && c.aliases.length ? '（' + c.aliases.join('、') + '）' : '');
        // S9.24: 角色列表显示好感度数值
        const affVal = gAffByName[c.name];
        if (typeof affVal === 'number') {
          const affBadge = document.createElement('span');
          affBadge.className = 'gr-aff-badge';
          affBadge.textContent = '♥' + affVal;
          affBadge.style.cssText = 'margin-left:6px;font-size:11px;color:' + (affVal >= 60 ? '#ffd88a' : affVal < 35 ? '#9db8e8' : '#cfe0f5');
          el.querySelector('.az-title').appendChild(affBadge);
        }
        const edit = el.querySelector('.gr-edit');
        edit.dataset.id = c.id;
        edit.onclick = (ev) => { ev.stopPropagation(); grEditChar(c.id); };
        const del = el.querySelector('.gr-del');
        del.dataset.id = c.id;
        del.onclick = (ev) => { ev.stopPropagation(); grDelChar(c.id); };
        el.onclick = () => { gSelChar = gSelChar === c.id ? null : c.id; renderGraph(); renderGraphLists(); };
        cl.appendChild(el);
      }
    }
    if (rl) {
      rl.innerHTML = '';
      for (const r of gRels) {
        const from = gChars.find((c) => c.id === r.from_char);
        const to = gChars.find((c) => c.id === r.to_char);
        const st = (REL_STYLE[r.category] || REL_STYLE.uncertain);
        const el = document.createElement('div');
        el.className = 'az-item';
        el.innerHTML = '<span class="az-title"></span><button type="button" class="ghost sm gr-edit-rel" data-id="" title="编辑">✎</button><button type="button" class="ghost sm gr-evol-rel" data-id="" title="演化轨迹">演</button><button type="button" class="ghost sm danger gr-del-rel" data-id="">✕</button>';
        const chapters = Array.isArray(r.chapters) ? r.chapters.filter((x) => typeof x === 'string' && x.trim()) : [];
        el.querySelector('.az-title').textContent =
          (from ? from.name : '?') + ' → ' + (to ? to.name : '?') + ' · ' + st.label + (r.confirmation_status === 'confirmed' ? '' : (r.confirmation_status === 'rejected' ? '（已拒绝）' : '（待确认）')) + (chapters.length ? '【' + chapters.join('、') + '】' : '');
        el.style.borderLeft = '3px solid ' + st.color;
        const edit = el.querySelector('.gr-edit-rel');
        edit.dataset.id = r.id;
        edit.onclick = (ev) => { ev.stopPropagation(); grEditRel(r.id); };
        const evol = el.querySelector('.gr-evol-rel');
        evol.dataset.id = r.id;
        evol.onclick = (ev) => { ev.stopPropagation(); grEvolRel(r.id); };
        const del = el.querySelector('.gr-del-rel');
        del.dataset.id = r.id;
        del.onclick = (ev) => { ev.stopPropagation(); grDelRel(r.id); };
        rl.appendChild(el);
      }
    }
  }

  // --- Evolution: relationship chapters trajectory (P0-3) ---
  function grEvolRel(id) {
    const r = gRels.find((x) => x.id === id);
    if (!r) return;
    const from = gChars.find((c) => c.id === r.from_char);
    const to = gChars.find((c) => c.id === r.to_char);
    const st = (REL_STYLE[r.category] || REL_STYLE.uncertain);
    const chapters = Array.isArray(r.chapters) ? r.chapters.filter((x) => typeof x === 'string' && x.trim()) : [];
    const left = (from ? from.name : '?') + ' → ' + (to ? to.name : '?');
    let body;
    if (chapters.length >= 2) {
      body = chapters.map((ch) => ch + '[' + st.label + ']').join(' → ');
    } else if (chapters.length === 1) {
      body = '仅见于 ' + chapters[0];
    } else {
      body = '暂无章节记录（手动创建或尚未由分析确认）';
    }
    showToast(left + '\n' + body, 'warning');
  }

  // --- CRUD (prompt/confirm, consistent with author zone style) ---
  async function grNewChar() {
    const name = await showPrompt('角色名');
    if (!name || !String(name).trim()) return;
    const aliases = await showPrompt('别名（逗号分隔，可空）') || '';
    const note = await showPrompt('备注（可空）') || '';
    try {
      await api('/api/v1/works/' + encodeURIComponent(grWorkId()) + '/graph/characters', {
        method: 'POST',
        body: JSON.stringify({ name: String(name).trim(), aliases: aliases.split(/[,，]/).map((s) => s.trim()).filter(Boolean), note }),
      });
      await loadGraph();
    } catch (e) {
      if ($('gr-msg')) $('gr-msg').textContent = '新增失败: ' + e.message;
    }
  }

  async function grDelChar(id) {
    if (!await showConfirm('删除该角色及其全部关系？')) return;
    try {
      await api('/api/v1/works/' + encodeURIComponent(grWorkId()) + '/graph/characters/' + encodeURIComponent(id), { method: 'DELETE' });
      await loadGraph();
    } catch (e) {
      if ($('gr-msg')) $('gr-msg').textContent = '删除失败: ' + e.message;
    }
  }

  async function grPickChar(label) {
    const opts = gChars.map((c) => c.name).join('、');
    const raw = await showPrompt(label + '（' + opts + '）');
    if (raw === null) return null;
    const name = String(raw).trim();
    if (!name) return null;
    const exact = gChars.find((c) => c.name === name);
    if (exact) return exact;
    try {
      const r = await api('/api/v1/works/' + encodeURIComponent(grWorkId()) + '/graph/characters/candidates?q=' + encodeURIComponent(name) + '&limit=5');
      const cands = r && Array.isArray(r.candidates) ? r.candidates : [];
      if (cands.length) {
        const list = cands.map((c, i) => (i + 1) + '. ' + c.name + ((c.aliases && c.aliases.length) ? '（' + c.aliases.join('、') + '）' : '')).join('\n');
        const pick = await showPrompt('「' + name + '」未精确匹配，候选：\n' + list + '\n\n输入编号选择，或留空取消');
        const idx = parseInt(pick || '', 10);
        if (!isNaN(idx) && idx >= 1 && idx <= cands.length) {
          return gChars.find((x) => x.id === cands[idx - 1].id) || null;
        }
        return null;
      }
      showToast('未找到角色「' + name + '」，请先在角色列表新增', 'warning');
    } catch (e) { /* fall through */ }
    return null;
  }

  async function grNewRel() {
    if (!gChars.length) { showToast('请先新增角色', 'warning'); return; }
    const a = await grPickChar('从角色');
    if (!a) return;
    const b = await grPickChar('到角色');
    if (!b) return;
    const cat = await showPrompt('关系类型：family 亲属 / social 社交 / emotional 情感 / conflict 冲突 / uncertain 未确定', { value: 'social' });
    if (cat === null) return;
    if (!REL_STYLE[cat]) { showToast('无效的关系类型', 'warning'); return; }
    const subtype = await showPrompt('关系子类（如 兄妹/仇敌，可空）', { value: r.subtype || '' }) || '';
    const kw = await showPrompt('关键词（逗号分隔，可空）', { value: (r.keywords || []).join('、') });
    const st = await showPrompt('状态：c confirmed / p pending / r rejected', { value: 'p' });
    const status = st === 'c' ? 'confirmed' : (st === 'r' ? 'rejected' : 'pending');
    try {
      await api('/api/v1/works/' + encodeURIComponent(grWorkId()) + '/graph/relationships', {
        method: 'POST',
        body: JSON.stringify({
          fromChar: a.id, toChar: b.id, category: cat,
          subtype, keywords: kw.split(/[,，]/).map((s) => s.trim()).filter(Boolean),
          confirmationStatus: status,
        }),
      });
      await loadGraph();
    } catch (e) {
      if ($('gr-msg')) $('gr-msg').textContent = '新增失败: ' + e.message;
    }
  }

  async function grEditChar(id) {
    const c = gChars.find((x) => x.id === id);
    if (!c) return;
    const name = await showPrompt('角色名', { value: c.name });
    if (name === null) return;
    if (!String(name).trim()) { if ($('gr-msg')) $('gr-msg').textContent = '名字不能为空'; return; }
    const note = await showPrompt('备注（可空）', { value: c.note || '' });
    if (note === null) return;
    try {
      await api('/api/v1/works/' + encodeURIComponent(grWorkId()) + '/graph/characters/' + encodeURIComponent(id), {
        method: 'PUT',
        body: JSON.stringify({
          name: String(name).trim(),
          aliases: c.aliases || [],
          note,
          colorIdx: c.color_idx != null ? c.color_idx : 0,
        }),
      });
      await loadGraph();
    } catch (e) {
      if ($('gr-msg')) $('gr-msg').textContent = '保存失败: ' + e.message;
    }
  }

  async function grEditRel(id) {
    const r = gRels.find((x) => x.id === id);
    if (!r) return;
    const cat = await showPrompt('关系类型：family 亲属 / social 社交 / emotional 情感 / conflict 冲突 / uncertain 未确定', { value: r.category || 'social' });
    if (cat === null) return;
    if (!REL_STYLE[cat]) { if ($('gr-msg')) $('gr-msg').textContent = '无效的关系类型'; return; }
    const subtype = await showPrompt('关系子类（如 兄妹/仇敌，可空）', { value: r.subtype || '' });
    if (subtype === null) return;
    const kw = await showPrompt('关键词（逗号分隔，可空）', { value: (r.keywords || []).join('、') });
    if (kw === null) return;
    const st = await showPrompt('状态：c confirmed / p pending / r rejected', { value: (r.confirmation_status === 'confirmed' ? 'c' : (r.confirmation_status === 'rejected' ? 'r' : 'p')) });
    if (st === null) return;
    const status = st === 'c' ? 'confirmed' : (st === 'r' ? 'rejected' : 'pending');
    try {
      await api('/api/v1/works/' + encodeURIComponent(grWorkId()) + '/graph/relationships/' + encodeURIComponent(id), {
        method: 'PUT',
        body: JSON.stringify({
          category: cat,
          subtype,
          keywords: kw.split(/[,，]/).map((s) => s.trim()).filter(Boolean),
          confirmationStatus: status,
          note: r.note || '',
        }),
      });
      await loadGraph();
    } catch (e) {
      if ($('gr-msg')) $('gr-msg').textContent = '保存失败: ' + e.message;
    }
  }

  async function grDelRel(id) {
    if (!await showConfirm('删除该关系？')) return;
    try {
      await api('/api/v1/works/' + encodeURIComponent(grWorkId()) + '/graph/relationships/' + encodeURIComponent(id), { method: 'DELETE' });
      await loadGraph();
    } catch (e) {
      if ($('gr-msg')) $('gr-msg').textContent = '删除失败: ' + e.message;
    }
  }

  function initGraphView() {
    const r = $('gr-refresh'), cn = $('gr-char-new'), rn = $('gr-rel-new'), w = $('gr-work'), gx = $('gr-galaxy-open');
    if (r) r.onclick = loadGraph;
    if (cn) cn.onclick = grNewChar;
    if (rn) rn.onclick = grNewRel;
    if (gx) gx.onclick = openGalaxyView;
    if (w) w.addEventListener('change', loadGraph);
    const srch = $('gr-search');
    if (srch) srch.addEventListener('input', () => {
      gSearch = String(srch.value).trim().toLowerCase();
      renderGraph();
      renderGraphLists();
    });
    window.addEventListener('resize', () => renderGraph());
    // auto-load when the view is opened
    const nav = document.querySelector('.az-nav, .az-mob-tab');
    if (nav) {
      nav.addEventListener('click', (ev) => {
        const btn = ev.target.closest('button[data-azview="graph"]');
        if (btn) setTimeout(loadGraph, 0);
      });
    }
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', initGraphView);
  } else {
    initGraphView();
  }
/* P1 S4: Galaxy view — pure functions ported from Scriverse relationship-graph.js (canvas 2D, no deps). */
  const GALAXY_LAYOUT_CONFIG = Object.freeze({ minimumRadius: 220, radialSpan: 830, repulsionStrength: 9200, desiredEdgeLength: 285 });
  const GALAXY_CELESTIAL_PALETTES = Object.freeze([
    Object.freeze({ key: "solar", hue: 42, saturation: 96, lightness: 68, color: "#ffc95f", core: "#fff8d4", rim: "#9f3c18", atmosphere: "rgba(255,184,72,.58)", ring: "rgba(255,222,151,.72)" }),
    Object.freeze({ key: "azure", hue: 211, saturation: 94, lightness: 68, color: "#61b8ff", core: "#effaff", rim: "#173b85", atmosphere: "rgba(79,156,255,.56)", ring: "rgba(164,214,255,.68)" }),
    Object.freeze({ key: "violet", hue: 263, saturation: 82, lightness: 72, color: "#b58cff", core: "#f7edff", rim: "#4d237c", atmosphere: "rgba(151,92,255,.54)", ring: "rgba(219,190,255,.7)" }),
    Object.freeze({ key: "rose", hue: 342, saturation: 91, lightness: 70, color: "#ff739d", core: "#fff0f5", rim: "#7f1e42", atmosphere: "rgba(255,91,139,.52)", ring: "rgba(255,190,210,.68)" }),
    Object.freeze({ key: "emerald", hue: 158, saturation: 67, lightness: 58, color: "#4ed49e", core: "#e7fff5", rim: "#145f4b", atmosphere: "rgba(50,211,154,.5)", ring: "rgba(166,244,214,.66)" }),
    Object.freeze({ key: "ice", hue: 190, saturation: 88, lightness: 76, color: "#9beaff", core: "#f4fdff", rim: "#26627c", atmosphere: "rgba(116,224,255,.52)", ring: "rgba(206,246,255,.7)" }),
    Object.freeze({ key: "copper", hue: 22, saturation: 74, lightness: 61, color: "#df8851", core: "#ffe8cf", rim: "#672b1c", atmosphere: "rgba(226,103,55,.48)", ring: "rgba(239,179,125,.66)" }),
    Object.freeze({ key: "pearl", hue: 47, saturation: 31, lightness: 84, color: "#e7dfc7", core: "#ffffff", rim: "#6c6b78", atmosphere: "rgba(207,217,240,.46)", ring: "rgba(237,231,216,.72)" })
  ]);
  const GALAXY_CELESTIAL_TYPES = Object.freeze({
    core: Object.freeze(["star", "star", "gas-giant", "ringed"]),
    active: Object.freeze(["gas-giant", "ringed", "ocean", "ice", "volcanic"]),
    outer: Object.freeze(["rocky", "ocean", "ice", "volcanic", "dwarf", "ringed"])
  });
  const GALAXY_ROTATION_RADIANS_PER_MS = 0.000012;
  const GALAXY_BASE_STAR_COUNT = 7200;
  const GALAXY_EDGE_STAR_BOOST_RATIO = 1.1 * 1.1 - 1;

  function grHashString(value) {
    let hash = 2166136261;
    for (let index = 0; index < value.length; index += 1) {
      hash ^= value.charCodeAt(index);
      hash = Math.imul(hash, 16777619);
    }
    return hash >>> 0;
  }
  function grMixHash(value) {
    let mixed = Number(value) >>> 0;
    mixed ^= mixed >>> 16;
    mixed = Math.imul(mixed, 0x7feb352d);
    mixed ^= mixed >>> 15;
    mixed = Math.imul(mixed, 0x846ca68b);
    return (mixed ^ (mixed >>> 16)) >>> 0;
  }
  function grSeededRandom(seed) {
    let value = seed || 1;
    return () => {
      value += 0x6d2b79f5;
      let next = value;
      next = Math.imul(next ^ (next >>> 15), next | 1);
      next ^= next + Math.imul(next ^ (next >>> 7), next | 61);
      return ((next ^ (next >>> 14)) >>> 0) / 4294967296;
    };
  }
  function grClamp(value, min, max) { return Math.min(max, Math.max(min, value)); }

  function layoutGalaxyGraph(graph, seed) {
    const random = grSeededRandom(grHashString(seed || "galaxy"));
    const nodes = graph.nodes.map((node, index) => {
      const angle = random() * Math.PI * 2;
      const radius = GALAXY_LAYOUT_CONFIG.minimumRadius + Math.sqrt(random()) * GALAXY_LAYOUT_CONFIG.radialSpan;
      const thickness = 18 + radius * 0.12;
      return { ...node, x: Math.cos(angle) * radius, y: (random() - 0.5) * thickness, z: Math.sin(angle) * radius, vx: 0, vy: 0, vz: 0, index };
    });
    const byId = new Map(nodes.map((node) => [node.id, node]));
    const exactRepulsion = nodes.length <= 180;
    const iterations = exactRepulsion ? 130 : 82;
    const applyRepulsion = (left, right) => {
      let dx = right.x - left.x;
      let dz = right.z - left.z;
      const distanceSquared = Math.max(80, dx * dx + dz * dz);
      const force = GALAXY_LAYOUT_CONFIG.repulsionStrength / distanceSquared;
      const distance = Math.sqrt(distanceSquared);
      dx /= distance; dz /= distance;
      left.vx -= dx * force; left.vz -= dz * force;
      right.vx += dx * force; right.vz += dz * force;
    };
    for (let iteration = 0; iteration < iterations; iteration += 1) {
      const cooling = 1 - iteration / (iterations + 20);
      if (exactRepulsion) {
        for (let leftIndex = 0; leftIndex < nodes.length; leftIndex += 1) {
          const left = nodes[leftIndex];
          for (let rightIndex = leftIndex + 1; rightIndex < nodes.length; rightIndex += 1) applyRepulsion(left, nodes[rightIndex]);
        }
      } else {
        const sampleCount = Math.min(28, nodes.length - 1);
        const stride = 17 + iteration % 11;
        for (let leftIndex = 0; leftIndex < nodes.length; leftIndex += 1) {
          const left = nodes[leftIndex];
          for (let sample = 1; sample <= sampleCount; sample += 1) {
            const rightIndex = (leftIndex + sample * stride) % nodes.length;
            if (rightIndex !== leftIndex) applyRepulsion(left, nodes[rightIndex]);
          }
        }
      }
      for (const edge of graph.edges) {
        const source = byId.get(edge.source);
        const target = byId.get(edge.target);
        if (!source || !target) continue;
        const dx = target.x - source.x;
        const dz = target.z - source.z;
        const distance = Math.max(1, Math.hypot(dx, dz));
        const force = (distance - GALAXY_LAYOUT_CONFIG.desiredEdgeLength) * 0.0028 * (0.5 + edge.confidence);
        source.vx += dx / distance * force; source.vz += dz / distance * force;
        target.vx -= dx / distance * force; target.vz -= dz / distance * force;
      }
      for (const node of nodes) {
        const centrality = grClamp(node.importance / Math.max(graph.nodes[0]?.importance || 1, 1), 0, 1);
        node.vx += -node.x * (0.00052 + centrality * 0.0011);
        node.vy += -node.y * 0.0014;
        node.vz += -node.z * (0.00052 + centrality * 0.0011);
        node.vx *= 0.84; node.vy *= 0.84; node.vz *= 0.84;
        node.x += node.vx * cooling; node.y += node.vy * cooling; node.z += node.vz * cooling;
      }
    }
    return { nodes, byId };
  }

  function createGalaxyStarfield(seed, count) {
    const random = grSeededRandom(grHashString(seed || "starfield"));
    const stars = [];
    const armCount = 4;
    const baseCount = count === undefined ? GALAXY_BASE_STAR_COUNT : Math.max(0, Math.floor(Number(count) || 0));
    const edgeBoostCount = count === undefined ? Math.round(baseCount * GALAXY_EDGE_STAR_BOOST_RATIO) : 0;
    const totalCount = baseCount + edgeBoostCount;
    for (let index = 0; index < totalCount; index += 1) {
      const isEdgeBoost = index >= baseCount;
      const population = random();
      const isCore = !isEdgeBoost && population < 0.62;
      const isHalo = !isEdgeBoost && !isCore && population > 0.9;
      const radius = isEdgeBoost
        ? 900 + Math.pow(random(), 0.82) * 900
        : isCore
        ? 32 + Math.pow(random(), 1.72) * 720
        : 160 + Math.pow(random(), 0.68) * 1510;
      const arm = index % armCount;
      const armAngle = arm / armCount * Math.PI * 2;
      const angle = isHalo
        ? random() * Math.PI * 2
        : armAngle + radius * 0.0065 + (random() - 0.5) * (isEdgeBoost ? 0.36 + radius / 1700 : isCore ? 0.72 : 0.42 + radius / 1100);
      const thickness = isHalo ? 70 + radius * 0.2 : (isEdgeBoost ? 26 + radius * 0.11 : isCore ? 8 + radius * 0.045 : 22 + radius * 0.105);
      const temperature = random();
      const x = Math.cos(angle) * radius + (random() - 0.5) * (isCore ? 42 : isEdgeBoost ? 68 : 78);
      const z = Math.sin(angle) * radius + (random() - 0.5) * (isCore ? 42 : isEdgeBoost ? 68 : 78);
      stars.push({
        x, y: (random() + random() + random() - 1.5) * thickness, z,
        originX: x, originZ: z, vx: 0, vz: 0,
        size: isCore && random() > 0.94 ? 1.25 + random() * 1.25 : 0.38 + random() * 0.82,
        brightness: isCore ? 0.3 + random() * 0.7 : 0.16 + random() * 0.66,
        color: temperature < 0.2 ? "255,218,176" : temperature > 0.78 ? "174,211,255" : "226,237,255",
        region: isEdgeBoost ? "edge-arm" : isCore ? "core" : isHalo ? "halo" : "arm"
      });
    }
    return stars;
  }

  function projectGalaxyPoint(point, camera, viewport) {
    const relativeX = point.x - Number(camera.targetX ?? 0);
    const relativeY = point.y - Number(camera.targetY ?? 0);
    const relativeZ = point.z - Number(camera.targetZ ?? 0);
    const cosYaw = Math.cos(camera.yaw), sinYaw = Math.sin(camera.yaw);
    const cosPitch = Math.cos(camera.pitch), sinPitch = Math.sin(camera.pitch);
    const cameraX = relativeX * cosYaw - relativeZ * sinYaw;
    const yawedZ = relativeX * sinYaw + relativeZ * cosYaw;
    const cameraY = relativeY * cosPitch - yawedZ * sinPitch;
    const cameraZ = relativeY * sinPitch + yawedZ * cosPitch;
    const depth = camera.distance + cameraZ;
    const focalLength = Math.min(viewport.width, viewport.height) * (camera.focalRatio ?? 1.1);
    const scale = depth > 1 ? focalLength / depth * camera.zoom : 0;
    return { x: viewport.width / 2 + cameraX * scale, y: viewport.height / 2 + cameraY * scale, depth, scale, visible: depth > 80 };
  }

  function getGalaxyNodeAppearance(node, maxDegree) {
    const degree = Math.max(0, Number(node?.degree) || 0);
    const normalizedDegree = grClamp(degree / Math.max(1, Number(maxDegree) || 1), 0, 1);
    const weightedDegree = Math.max(0, Number(node?.weightedDegree) || 0);
    const confidenceBoost = grClamp(weightedDegree / Math.max(1, degree) / 1.35, 0, 1);
    // S9.24: 好感度打通——affinity 直接参与强度计算（占 35% 权重）
    const affBoost = typeof node?.affinity === 'number' ? grClamp(node.affinity / 100, 0, 1) : 0.5;
    const hasAff = typeof node?.affinity === 'number';
    const intensity = grClamp(normalizedDegree * 0.52 + confidenceBoost * 0.13 + affBoost * 0.35, 0, 1);
    const brightness = (0.7 + intensity * 0.68).toFixed(3);
    const glow = (0.26 + intensity * 0.74).toFixed(3);
    const tier = intensity >= 0.7 ? "core" : intensity >= 0.34 ? "active" : "outer";
    const appearanceSeed = grMixHash(grHashString([String(node?.id ?? ""), String(node?.name ?? ""), String(node?.groupKey ?? ""), "character"].join("|")));
    const palette = GALAXY_CELESTIAL_PALETTES[appearanceSeed % GALAXY_CELESTIAL_PALETTES.length];
    const celestialTypes = GALAXY_CELESTIAL_TYPES[tier];
    const celestialType = celestialTypes[Math.floor(appearanceSeed / GALAXY_CELESTIAL_PALETTES.length) % celestialTypes.length];
    const sizeScale = ({ star: 1.18, "gas-giant": 1.12, ringed: 1.08, ocean: 1, ice: 0.96, volcanic: 1.02, rocky: 0.92, dwarf: 0.76 })[celestialType] ?? 1;
    return { degree, intensity, tier, palette, celestialType, sizeScale, brightness: Number(brightness), glow: Number(glow), affBoost: hasAff ? node.affinity : null };
  }

  let grGalaxy = null;
  function closeGalaxyView() {
    if (!grGalaxy) return;
    grGalaxy.running = false;
    if (grGalaxy.raf) cancelAnimationFrame(grGalaxy.raf);
    grGalaxy._cleanup?.();
    if (grGalaxy._onKey) document.removeEventListener("keydown", grGalaxy._onKey);
    grGalaxy.overlay?.remove();
    grGalaxy = null;
  }

  function resizeGalaxyCanvas() {
    const dpr = Math.min(window.devicePixelRatio || 1, 2);
    const w = grGalaxy.canvas.clientWidth || window.innerWidth;
    const h = grGalaxy.canvas.clientHeight || window.innerHeight;
    grGalaxy.canvas.width = Math.round(w * dpr);
    grGalaxy.canvas.height = Math.round(h * dpr);
    grGalaxy.ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    return { width: w, height: h };
  }

  function renderGalaxyFrame(now) {
    if (!grGalaxy || !grGalaxy.running) return;
    const deltaMs = grClamp(now - (grGalaxy._lastNow || now), 1, 50);
    grGalaxy._lastNow = now;
    grGalaxy.camera.yaw += GALAXY_ROTATION_RADIANS_PER_MS * deltaMs * 10;
    const viewport = resizeGalaxyCanvas();
    const ctx = grGalaxy.ctx;
    ctx.clearRect(0, 0, viewport.width, viewport.height);
    ctx.fillStyle = "#05070f";
    ctx.fillRect(0, 0, viewport.width, viewport.height);
    const camera = grGalaxy.camera;
    // stars
    for (const star of grGalaxy.stars) {
      const p = projectGalaxyPoint(star, camera, viewport);
      if (!p.visible || p.depth > 5200) continue;
      const alpha = grClamp(star.brightness * (1 - (p.depth - 80) / 5200), 0.05, 1);
      ctx.fillStyle = "rgba(" + star.color + "," + alpha.toFixed(3) + ")";
      const s = Math.max(0.5, star.size * (p.scale > 1 ? 1 : p.scale) * 0.6);
      ctx.fillRect(p.x - s / 2, p.y - s / 2, s, s);
    }
    // edges
    ctx.lineWidth = 1;
    for (const edge of grGalaxy.edges) {
      const src = grGalaxy.nodes.find((n) => String(n.id) === String(edge.source));
      const tgt = grGalaxy.nodes.find((n) => String(n.id) === String(edge.target));
      if (!src || !tgt) continue;
      const sp = projectGalaxyPoint(src, camera, viewport);
      const tp = projectGalaxyPoint(tgt, camera, viewport);
      if (!sp.visible || !tp.visible) continue;
      ctx.strokeStyle = "rgba(140,170,255," + (0.10 + edge.confidence * 0.14).toFixed(3) + ")";
      ctx.beginPath();
      ctx.moveTo(sp.x, sp.y);
      ctx.lineTo(tp.x, tp.y);
      ctx.stroke();
    }
    // nodes
    const maxDegree = Math.max(1, ...grGalaxy.nodes.map((n) => n.degree));
    for (const node of grGalaxy.nodes) {
      const p = projectGalaxyPoint(node, camera, viewport);
      if (!p.visible || p.depth > 4200) continue;
      const depthAlpha = grClamp(1.28 - p.depth / 4800, 0.72, 1);
      const app = node.appearance;
      const baseRadius = (6.5 + node.degree * 2.2) * app.sizeScale * grClamp(p.scale, 0.4, 2.2);
      const pal = app.palette;
      // atmosphere
      ctx.fillStyle = pal.atmosphere;
      ctx.beginPath();
      ctx.arc(p.x, p.y, baseRadius * 1.9, 0, Math.PI * 2);
      ctx.fill();
      // glow
      const glowRadius = baseRadius * (1.25 + app.glow * 0.75);
      const glowGrad = ctx.createRadialGradient(p.x, p.y, 0, p.x, p.y, glowRadius);
      glowGrad.addColorStop(0, "rgba(" + hexToRgb(pal.color) + "," + (0.16 * app.brightness * depthAlpha).toFixed(3) + ")");
      glowGrad.addColorStop(1, "rgba(" + hexToRgb(pal.color) + ",0)");
      ctx.fillStyle = glowGrad;
      ctx.beginPath();
      ctx.arc(p.x, p.y, glowRadius, 0, Math.PI * 2);
      ctx.fill();
      // body
      ctx.fillStyle = pal.color;
      ctx.beginPath();
      ctx.arc(p.x, p.y, baseRadius, 0, Math.PI * 2);
      ctx.fill();
      // core
      ctx.fillStyle = pal.core;
      ctx.beginPath();
      ctx.arc(p.x - baseRadius * 0.18, p.y - baseRadius * 0.18, baseRadius * 0.52, 0, Math.PI * 2);
      ctx.fill();
      // ring for ringed type
      if (app.celestialType === "ringed" && baseRadius > 4) {
        ctx.strokeStyle = pal.ring;
        ctx.lineWidth = Math.max(1, baseRadius * 0.22);
        ctx.beginPath();
        ctx.ellipse(p.x, p.y, baseRadius * 1.7, baseRadius * 0.55, -0.4, 0, Math.PI * 2);
        ctx.stroke();
      }
      // label
      if (node.degree > 0 || p.scale > 1.15) {
        const labelSize = Math.max(10, Math.min(15, 9 + p.scale * 2.4));
        ctx.font = "600 " + labelSize + "px system-ui, sans-serif";
        ctx.textAlign = "center";
        ctx.fillStyle = "rgba(232,236,248," + (0.78 * depthAlpha).toFixed(3) + ")";
        ctx.fillText(node.name, p.x, p.y + baseRadius + labelSize + 3);
      }
      // S9.24: 好感度数值标签（有 affinity 的角色）
      if (typeof node.affinity === 'number' && p.scale > 0.9) {
        const affSize = Math.max(9, Math.min(13, 8 + p.scale * 2));
        ctx.font = "700 " + affSize + "px system-ui, sans-serif";
        ctx.textAlign = "center";
        ctx.fillStyle = node.affinity >= 60 ? "rgba(255,216,138,0.95)" : node.affinity < 35 ? "rgba(157,184,232,0.95)" : "rgba(207,224,245,0.9)";
        ctx.fillText("♥" + node.affinity, p.x, p.y - baseRadius - affSize - 2);
      }
    }
    // stats
    ctx.fillStyle = "rgba(232,236,248,.55)";
    ctx.font = "12px system-ui, sans-serif";
    ctx.textAlign = "left";
    ctx.fillText(grGalaxy.nodes.length + " 个角色 · " + grGalaxy.edges.length + " 条关系 · 视差银河", 16, viewport.height - 46);
    grGalaxy.raf = requestAnimationFrame(renderGalaxyFrame);
  }

  function hexToRgb(hex) {
    const m = /^#?([a-f\d]{2})([a-f\d]{2})([a-f\d]{2})$/i.exec(hex || "");
    return m ? parseInt(m[1], 16) + "," + parseInt(m[2], 16) + "," + parseInt(m[3], 16) : "255,255,255";
  }

  function openGalaxyView() {
    if (!grGraphData || !grGraphData.characters || grGraphData.characters.length === 0) {
      showToast("当前作品还没有角色数据，请先新增角色", 'warning');
      return;
    }
    closeGalaxyView();
    grGalaxy = { canvas: null, ctx: null, stars: [], nodes: [], edges: [], camera: { yaw: 0.6, pitch: 0.36, distance: 1420, zoom: 1, targetX: 0, targetY: 0, targetZ: 0, focalRatio: 1.1 }, raf: 0, running: true, dragging: false, lastX: 0, lastY: 0, seed: "kaleido-graph-" + (grWorkId || "x") };
    const overlay = document.createElement("div");
    overlay.id = "gr-galaxy-overlay";
    overlay.style.cssText = "position:fixed;inset:0;z-index:9999;background:#05070f;cursor:grab;";
    overlay.innerHTML = '<canvas style="width:100%;height:100%;display:block"></canvas>'
      + '<button type="button" id="gr-galaxy-close" style="position:absolute;top:14px;right:14px;z-index:2;background:rgba(255,255,255,.1);border:1px solid rgba(255,255,255,.25);color:#e8ecf8;border-radius:8px;padding:6px 14px;cursor:pointer">✕ 关闭</button>'
      + '<div style="position:absolute;bottom:14px;left:14px;z-index:2;color:rgba(232,236,248,.6);font-size:12px;pointer-events:none">拖拽旋转 · 滚轮缩放 · 自动公转</div>';
    document.body.appendChild(overlay);
    grGalaxy.overlay = overlay;
    grGalaxy.canvas = overlay.querySelector("canvas");
    grGalaxy.ctx = grGalaxy.canvas.getContext("2d");
    overlay.querySelector("#gr-galaxy-close").addEventListener("click", closeGalaxyView);
    const onKey = (ev) => { if (ev.key === "Escape") closeGalaxyView(); };
    document.addEventListener("keydown", onKey);
    grGalaxy._onKey = onKey;

    const degreeMap = new Map();
    const weightedMap = new Map();
    for (const rel of grGraphData.relationships || []) {
      const a = String(rel.from_char), b = String(rel.to_char);
      degreeMap.set(a, (degreeMap.get(a) || 0) + 1);
      degreeMap.set(b, (degreeMap.get(b) || 0) + 1);
      weightedMap.set(a, (weightedMap.get(a) || 0) + (rel.category === "uncertain" ? 0.5 : 1));
      weightedMap.set(b, (weightedMap.get(b) || 0) + (rel.category === "uncertain" ? 0.5 : 1));
    }
    const nodes = (grGraphData.characters || []).map((ch) => ({
      id: String(ch.id), name: ch.name, groupKey: "character",
      degree: degreeMap.get(String(ch.id)) || 0,
      weightedDegree: weightedMap.get(String(ch.id)) || 0,
      // S9.24: 好感度打通——按角色名挂 affinity 供外观计算
      affinity: typeof gAffByName[ch.name] === 'number' ? gAffByName[ch.name] : null,
      importance: 1 + (degreeMap.get(String(ch.id)) || 0) * 0.5
    }));
    const edges = (grGraphData.relationships || []).map((rel) => ({
      source: String(rel.from_char), target: String(rel.to_char),
      confidence: rel.category === "uncertain" ? 0.4 : 0.85
    }));
    const maxDegree = Math.max(1, ...nodes.map((n) => n.degree));
    const laid = layoutGalaxyGraph({ nodes, edges }, grGalaxy.seed);
    grGalaxy.nodes = laid.nodes.map((node) => ({ ...node, appearance: getGalaxyNodeAppearance(node, maxDegree) }));
    grGalaxy.edges = edges;
    grGalaxy.stars = createGalaxyStarfield(grGalaxy.seed + "-stars");

    const onPointerDown = (ev) => { grGalaxy.dragging = true; grGalaxy.lastX = ev.clientX; grGalaxy.lastY = ev.clientY; overlay.style.cursor = "grabbing"; ev.preventDefault(); };
    const onPointerMove = (ev) => {
      if (!grGalaxy.dragging) return;
      const dx = ev.clientX - grGalaxy.lastX, dy = ev.clientY - grGalaxy.lastY;
      grGalaxy.lastX = ev.clientX; grGalaxy.lastY = ev.clientY;
      grGalaxy.camera.yaw -= dx * 0.0042;
      grGalaxy.camera.pitch = grClamp(grGalaxy.camera.pitch - dy * 0.0032, -1.15, 1.15);
    };
    const onPointerUp = () => { grGalaxy.dragging = false; overlay.style.cursor = "grab"; };
    const onWheel = (ev) => {
      ev.preventDefault();
      grGalaxy.camera.zoom = grClamp(grGalaxy.camera.zoom * (ev.deltaY > 0 ? 0.92 : 1.08), 0.35, 3.5);
    };
    overlay.addEventListener("pointerdown", onPointerDown);
    window.addEventListener("pointermove", onPointerMove);
    window.addEventListener("pointerup", onPointerUp);
    overlay.addEventListener("wheel", onWheel, { passive: false });
    grGalaxy._cleanup = () => {
      overlay.removeEventListener("pointerdown", onPointerDown);
      window.removeEventListener("pointermove", onPointerMove);
      window.removeEventListener("pointerup", onPointerUp);
      overlay.removeEventListener("wheel", onWheel);
    };
    grGalaxy._lastNow = performance.now();
    grGalaxy.raf = requestAnimationFrame(renderGalaxyFrame);
  }

/* ================= _foreshadow-part.js ================= */
/* P2: Foreshadow/outline management (author zone sub-view).
   Data via /api/v1/works/{work_id}/foreshadows (list/create/update/delete + occurrences). */
  let fsItems = [];
  let fsEditing = null; // foreshadow object being edited

  const FS_STATUS = { planted: '已埋', active: '已激活', recalled: '已回收' };
  const FS_TYPE = { plant: '埋设', payoff: '回收' };

  function fsWorkId() {
    const el = $('fs-work');
    if (el && String(el.value).trim()) return String(el.value).trim();
    // 用 workspace 域而非项目 ID（work_id=workspace_id，2026-08-10 修复）
    return azSelectedWorkspaceId || azSelectedProjectId || 'default';
  }

  function fsBaseUrl() {
    return '/api/v1/works/' + encodeURIComponent(fsWorkId()) + '/foreshadows';
  }

  function fsItemUrl(id) {
    return fsBaseUrl() + '/' + encodeURIComponent(id);
  }

  // 依赖链指标：入边（依赖的父伏笔数）与出边（被谁依赖）。
  function fsChainMetrics(f) {
    const deps = Array.isArray(f.parent_ids) ? f.parent_ids : [];
    const depOut = Array.isArray(fsItems)
      ? fsItems.reduce((n, x) => n + (Array.isArray(x.parent_ids) && x.parent_ids.indexOf(f.id) !== -1 ? 1 : 0), 0)
      : 0;
    return { deps: deps.length, outs: depOut };
  }

  function fsTitleById(id) {
    const f = fsItems.find((x) => x.id === id);
    return f ? (f.title || id) : (id + '（已删除）');
  }

  function fsMsg(text) {
    const m = $('fs-msg');
    if (m) m.textContent = text || '';
  }

  async function loadForeshadows() {
    fsMsg('加载伏笔…');
    try {
      const r = await api(fsBaseUrl());
      fsItems = Array.isArray(r) ? r : [];
      renderFsList();
      let msg = fsItems.length + ' 条伏笔';
      try {
        const st = await api(fsBaseUrl() + '/stats');
        const c = (st && st.by_status) || {};
        const avg = st && typeof st.average_weight === 'number' ? ' · 均权 ' + st.average_weight.toFixed(1) : '';
        msg += ' · 埋' + (c.planted || 0) + '/活' + (c.active || 0) + '/收' + (c.recalled || 0) + avg;
      } catch (e) { /* 统计失败不阻塞列表 */ }
      fsMsg(msg);
    } catch (e) {
      fsMsg('加载失败: ' + e.message);
    }
  }

  function renderFsList() {
    const list = $('fs-list');
    if (!list) return;
    list.innerHTML = '';
    if (!fsItems.length) {
      const el = document.createElement('div');
      el.className = 'muted sm';
      el.textContent = '（暂无伏笔，先在左侧新建）';
      list.appendChild(el);
      return;
    }
    for (const f of fsItems) {
      const el = document.createElement('div');
      el.className = 'az-item' + (fsEditing && fsEditing.id === f.id ? ' active' : '');
      el.style.borderLeft = '3px solid ' + (f.status === 'recalled' ? '#9aa5b5' : (f.status === 'active' ? '#ffad42' : '#43e39a'));
      el.innerHTML = '<span class="az-title"></span><button type="button" class="ghost sm fs-edit" data-id="" title="编辑">✎</button><button type="button" class="ghost sm danger fs-del" data-id="" title="删除">✕</button>';
      const badge = FS_STATUS[f.status] || f.status || '';
      const m = fsChainMetrics(f);
      const chain = (m.deps || m.outs) ? ' · 依赖' + m.deps + '/' + m.outs : '';
      el.querySelector('.az-title').textContent = (f.title || '(无标题)') + (badge ? ' · ' + badge : '') + ' · ' + (Array.isArray(f.occurrences) ? f.occurrences.length : 0) + ' 点' + ' · 权' + (f.weight != null ? f.weight : 5) + chain;
      const edit = el.querySelector('.fs-edit');
      edit.dataset.id = f.id;
      edit.onclick = (ev) => { ev.stopPropagation(); fsEdit(f.id); };
      const del = el.querySelector('.fs-del');
      del.dataset.id = f.id;
      del.onclick = (ev) => { ev.stopPropagation(); fsDel(f.id); };
      el.onclick = () => fsEdit(f.id);
      list.appendChild(el);
    }
  }

  async function fsNew() {
    const title = $('fs-new-title'), desc = $('fs-new-desc'), status = $('fs-new-status');
    if (!title || !String(title.value).trim()) { fsMsg('标题不能为空'); return; }
    try {
      await api('/api/v1/works/' + encodeURIComponent(fsWorkId()) + '/foreshadows', {
        method: 'POST',
        body: JSON.stringify({
          title: String(title.value).trim(),
          description: desc ? String(desc.value).trim() : '',
          status: status ? status.value : 'planted',
        }),
      });
      if (title) title.value = '';
      if (desc) desc.value = '';
      await loadForeshadows();
    } catch (e) {
      fsMsg('新建失败: ' + e.message);
    }
  }

  async function fsDel(id) {
    const f = fsItems.find((x) => x.id === id);
    if (!f) return;
    if (!await showConfirm('删除该伏笔及其全部伏笔点？')) return;
    try {
      await api('/api/v1/works/' + encodeURIComponent(fsWorkId()) + '/foreshadows/' + encodeURIComponent(id), {
        method: 'DELETE',
        body: JSON.stringify({ expectedVersionNo: f.expected_version_no }),
      });
      if (fsEditing && fsEditing.id === id) fsEditing = null;
      await loadForeshadows();
    } catch (e) {
      fsMsg('删除失败: ' + e.message);
    }
  }

  function fsEdit(id) {
    const f = fsItems.find((x) => x.id === id);
    if (!f) return;
    fsEditing = f;
    const t = $('fs-edit-title'), d = $('fs-edit-desc'), s = $('fs-edit-status'), v = $('fs-edit-ver');
    if (t) t.value = f.title || '';
    if (d) d.value = f.description || '';
    if (s) s.value = f.status || 'planted';
    if (v) v.textContent = f.expected_version_no != null ? '版本 ' + f.expected_version_no : '';
    renderFsOccurrences();
    renderFsDepEditor();
    renderFsList();
  }

  async function fsSaveEdit() {
    if (!fsEditing) return;
    const t = $('fs-edit-title'), d = $('fs-edit-desc'), s = $('fs-edit-status');
    const w = $('fs-edit-weight');
    let weight;
    if (w && String(w.value).trim() !== '') {
      const n = Number(w.value);
      if (Number.isFinite(n) && n >= 1 && n <= 10) weight = n;
      else { fsMsg('权重须为 1-10 的整数'); return; }
    }
    try {
      await api(fsItemUrl(fsEditing.id), {
        method: 'PATCH',
        body: JSON.stringify({
          title: t ? String(t.value).trim() : undefined,
          description: d ? String(d.value).trim() : undefined,
          status: s ? s.value : undefined,
          weight: weight !== undefined ? weight : undefined,
          expectedVersionNo: fsEditing.expected_version_no,
        }),
      });
      await loadForeshadows();
      const fresh = fsItems.find((x) => x.id === fsEditing.id);
      if (fresh) fsEditing = fresh;
      renderFsOccurrences();
      renderFsDepEditor();
      renderFsList();
      fsMsg('已保存');
    } catch (e) {
      fsMsg('保存失败: ' + e.message + '（若版本冲突请刷新后重试）');
    }
  }

  // ── 依赖链编辑（T1：DAG）──
  function buildFsDepUI() {
    const saveBtn = $('fs-save-btn');
    if (!saveBtn) return null;
    let box = $('fs-deps-box');
    if (box) return box;
    box = document.createElement('div');
    box.id = 'fs-deps-box';
    box.innerHTML = '' +
      '<div class="az-panel-head"><h3>权重</h3></div>' +
      '<label class="muted sm">权重（1-10）<input id="fs-edit-weight" type="number" min="1" max="10" step="1" style="width:64px" value="5" /></label>' +
      '<div class="az-panel-head"><h3>依赖链</h3></div>' +
      '<div id="fs-dep-list" class="az-list" style="flex:1;overflow:auto;max-height:120px"></div>' +
      '<label class="muted sm"><input id="fs-dep-parent" placeholder="依赖的父伏笔 ID（如某 id）" style="width:100%" /></label>' +
      '<button type="button" id="fs-dep-add" class="ghost sm" style="align-self:flex-start">+ 依赖</button>' +
      '<p id="fs-dep-mounted" class="muted sm"></p>';
    saveBtn.parentNode.insertBefore(box, saveBtn.nextSibling);
    return box;
  }

  function renderFsDepEditor() {
    const box = $('fs-deps-box');
    if (!box) return;
    const w = $('fs-edit-weight');
    if (w) w.value = fsEditing && fsEditing.weight != null ? fsEditing.weight : 5;
    const list = $('fs-dep-list');
    if (list) {
      list.innerHTML = '';
      const parents = fsEditing && Array.isArray(fsEditing.parent_ids) ? fsEditing.parent_ids : [];
      if (!parents.length) {
        const el = document.createElement('div');
        el.className = 'muted sm';
        el.textContent = '（无依赖）';
        list.appendChild(el);
      } else {
        for (const pid of parents) {
          const row = document.createElement('div');
          row.className = 'az-item';
          row.innerHTML = '<span class="az-title"></span><button type="button" class="ghost sm danger fs-dep-del" data-id="" title="移除依赖">✕</button>';
          row.querySelector('.az-title').textContent = fsTitleById(pid);
          const del = row.querySelector('.fs-dep-del');
          del.dataset.id = pid;
          del.onclick = (ev) => { ev.stopPropagation(); fsDepDel(pid); };
          list.appendChild(row);
        }
      }
    }
    const mounted = $('fs-dep-mounted');
    if (mounted) {
      const outs = fsItems.filter((x) => Array.isArray(x.parent_ids) && x.parent_ids.indexOf(fsEditing && fsEditing.id) !== -1);
      mounted.textContent = outs.length ? '被依赖：' + outs.map((x) => x.title || x.id).join('、') : '';
    }
  }

  async function fsDepAdd() {
    if (!fsEditing) return;
    const inp = $('fs-dep-parent');
    if (!inp || !String(inp.value).trim()) { fsMsg('父伏笔 ID 不能为空'); return; }
    const pid = String(inp.value).trim();
    try {
      await api(fsItemUrl(fsEditing.id) + '/dependencies', {
        method: 'POST',
        body: JSON.stringify({ parentId: pid, expectedVersionNo: fsEditing.expected_version_no }),
      });
      if (inp) inp.value = '';
      const cur = await refreshFsItems();
      if (cur) fsEditing = cur;
      renderFsDepEditor();
      renderFsList();
      fsMsg('已添加依赖' + (cur ? '' : '（请刷新）'));
    } catch (e) {
      fsMsg('添加依赖失败: ' + e.message);
    }
  }

  async function fsDepDel(parentId) {
    if (!fsEditing) return;
    if (!await showConfirm('移除该依赖？')) return;
    try {
      await api(fsItemUrl(fsEditing.id) + '/dependencies/' + encodeURIComponent(parentId), {
        method: 'DELETE',
        body: JSON.stringify({ expectedVersionNo: fsEditing.expected_version_no }),
      });
      const cur = await refreshFsItems();
      if (cur) fsEditing = cur;
      renderFsDepEditor();
      renderFsList();
      fsMsg('已移除依赖');
    } catch (e) {
      fsMsg('移除依赖失败: ' + e.message + '（若版本冲突请刷新后重试）');
    }
  }

  async function refreshFsItems() {
    const fresh = await api(fsBaseUrl());
    fsItems = Array.isArray(fresh) ? fresh : [];
    if (!fsEditing) return null;
    return fsItems.find((x) => x.id === fsEditing.id) || null;
  }

  function renderFsOccurrences() {
    const list = $('fs-occ-list');
    if (!list) return;
    list.innerHTML = '';
    if (!fsEditing) return;
    const occs = Array.isArray(fsEditing.occurrences) ? fsEditing.occurrences : [];
    if (!occs.length) {
      const el = document.createElement('div');
      el.className = 'muted sm';
      el.textContent = '（暂无伏笔点）';
      list.appendChild(el);
      return;
    }
    for (const o of occs) {
      const el = document.createElement('div');
      el.className = 'az-item';
      el.innerHTML = '<span class="az-title"></span><button type="button" class="ghost sm danger fs-occ-del" data-id="" title="删除">✕</button>';
      el.querySelector('.az-title').textContent = (FS_TYPE[o.type] || o.type || '?') + ' · ' + (o.chapter_id || '') + (o.note ? ' — ' + o.note : '');
      const del = el.querySelector('.fs-occ-del');
      del.dataset.id = o.id;
      del.onclick = (ev) => { ev.stopPropagation(); fsOccDel(o.id); };
      list.appendChild(el);
    }
  }

  async function fsOccAdd() {
    if (!fsEditing) return;
    const ch = $('fs-occ-chapter'), ty = $('fs-occ-type'), nt = $('fs-occ-note');
    if (!ch || !String(ch.value).trim()) { fsMsg('章节 ID 不能为空'); return; }
    try {
      await api('/api/v1/works/' + encodeURIComponent(fsWorkId()) + '/foreshadows/' + encodeURIComponent(fsEditing.id) + '/occurrences', {
        method: 'POST',
        body: JSON.stringify({
          chapterId: String(ch.value).trim(),
          type: ty ? ty.value : 'plant',
          note: nt ? String(nt.value).trim() : '',
        }),
      });
      if (nt) nt.value = '';
      const fresh = await api('/api/v1/works/' + encodeURIComponent(fsWorkId()) + '/foreshadows');
      const cur = (Array.isArray(fresh) ? fresh : []).find((x) => x.id === fsEditing.id);
      if (cur) fsEditing = cur;
      renderFsOccurrences();
      fsMsg('已添加伏笔点');
    } catch (e) {
      fsMsg('添加失败: ' + e.message);
    }
  }

  async function fsOccDel(occId) {
    if (!fsEditing) return;
    if (!await showConfirm('删除该伏笔点？')) return;
    try {
      await api('/api/v1/works/' + encodeURIComponent(fsWorkId()) + '/foreshadows/' + encodeURIComponent(fsEditing.id) + '/occurrences/' + encodeURIComponent(occId), {
        method: 'DELETE',
        body: JSON.stringify({ expectedVersionNo: fsEditing.expected_version_no }),
      });
      const fresh = await api('/api/v1/works/' + encodeURIComponent(fsWorkId()) + '/foreshadows');
      const cur = (Array.isArray(fresh) ? fresh : []).find((x) => x.id === fsEditing.id);
      if (cur) fsEditing = cur;
      renderFsOccurrences();
      fsMsg('已删除伏笔点');
    } catch (e) {
      fsMsg('删除失败: ' + e.message + '（若版本冲突请刷新后重试）');
    }
  }

  function initForeshadowView() {
    const rf = $('fs-refresh'), nw = $('fs-new-btn'), sv = $('fs-save-btn'), w = $('fs-work'), oc = $('fs-occ-add');
    if (rf) rf.onclick = loadForeshadows;
    if (nw) nw.onclick = fsNew;
    if (sv) sv.onclick = fsSaveEdit;
    if (oc) oc.onclick = fsOccAdd;
    if (w) w.addEventListener('change', loadForeshadows);
    const box = buildFsDepUI();
    const da = box && $('fs-dep-add');
    if (da) da.onclick = fsDepAdd;
  }

  document.addEventListener('DOMContentLoaded', initForeshadowView);

/* ===== exports consumed by remaining closure parts (Mechanism Y) ===== */
export { loadAnKinds, loadAnTasks, loadGraph, loadForeshadows };
