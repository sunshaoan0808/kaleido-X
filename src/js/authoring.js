/* authoring.js — P1-3 S2.17: _works-part + _author-part as real ESM.
 * Combined into ONE module on purpose: works.setWorksOpen calls author's
 * updateAzDeskActions/loadWorksVersionsSidebar while author.loadWorksTree
 * calls works' escapeHtml (a cycle) — both parts shared one IIFE closure
 * scope, concatenation preserves that exactly.
 *
 * Canonical state stays in the closure (_state-part); access via facades:
 *   __az() -> window.__kaleidoAzState      (8 az* lets, new S2.17)
 *   __wk() -> window.__kaleidoWorksState   (works-domain lets, new S2.17)
 *   __c7() -> window.__kaleidoChatState    (.partner)
 *   __t6() -> window.__kaleidoTabs         (currentTab/updateImmersive/switchTab)
 */
import { $ } from './dom.js';
import { api } from './api.js';
import { showToast } from './toast.js';
import { showConfirm, showPrompt } from './dialog.js';
import { formatDateTime, TAVERN_SID_KEY } from './utils.js';
// S2.14 exports consumed by fillProjectDatalist/selectProject (typeof-guarded
// in the closure; direct imports keep those guards always-true, same semantics
// as the converted[] bindings they replace).
import { loadAnKinds, loadAnTasks, loadGraph, loadForeshadows } from './insight.js';
// lesson (j): functions must be statically imported — facade property access
// invites rollup tree-shaking of the callee module.
import { stLoadSession } from './tavern.js';

const __az = () => window.__kaleidoAzState;
const __wk = () => window.__kaleidoWorksState;
const __c7 = () => window.__kaleidoChatState;
const __t6 = () => window.__kaleidoTabs;

/* Works IDE */
  function parentWorksPath(p) {
    if (!p) return '';
    const i = p.lastIndexOf('/');
    return i < 0 ? '' : p.slice(0, i);
  }


  // --- S7-W2 works preview: minimal md→html (no CDN) ---
  function escapeHtml(s) {
    return String(s)
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;');
  }

  function inlineMd(text) {
    let s = escapeHtml(text);
    // code first
    s = s.replace(/`([^`]+)`/g, '<code>$1</code>');
    // bold / italic
    s = s.replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>');
    s = s.replace(/__([^_]+)__/g, '<strong>$1</strong>');
    s = s.replace(/(^|[^*])\*([^*]+)\*/g, '$1<em>$2</em>');
    s = s.replace(/(^|[^_])_([^_]+)_/g, '$1<em>$2</em>');
    // links [text](url) — sanitize URL to prevent javascript: / data: XSS
    s = s.replace(/\[([^\]]+)\]\(([^)\s]+)\)/g, function (_m, text, url) {
      try {
        const u = new URL(url, window.location.href);
        const allowed = /^(https?|mailto|tel)$/i;
        if (!allowed.test(u.protocol)) return escapeHtml(text);
      } catch (_) {
        // not a valid URL, or relative path without base
        if (/^(javascript|data|vbscript|file):/i.test(url)) return escapeHtml(text);
      }
      return '<a href="' + escapeHtml(url) + '" target="_blank" rel="noopener noreferrer">' + escapeHtml(text) + '</a>';
    });
    return s;
  }

  function simpleMarkdownToHtml(md) {
    const src = String(md || '').replace(/\r\n/g, '\n');
    if (!src.trim()) return '';
    const lines = src.split('\n');
    const out = [];
    let i = 0;
    let inCode = false;
    let codeBuf = [];
    let listType = null; // 'ul' | 'ol'

    function closeList() {
      if (listType) {
        out.push(listType === 'ol' ? '</ol>' : '</ul>');
        listType = null;
      }
    }

    while (i < lines.length) {
      const line = lines[i];
      const fence = line.match(/^```(.*)$/);
      if (fence) {
        if (inCode) {
          out.push('<pre><code>' + escapeHtml(codeBuf.join('\n')) + '</code></pre>');
          codeBuf = [];
          inCode = false;
        } else {
          closeList();
          inCode = true;
        }
        i += 1;
        continue;
      }
      if (inCode) {
        codeBuf.push(line);
        i += 1;
        continue;
      }

      if (!line.trim()) {
        closeList();
        i += 1;
        continue;
      }

      const h = line.match(/^(#{1,4})\s+(.+)$/);
      if (h) {
        closeList();
        const level = h[1].length;
        out.push('<h' + level + '>' + inlineMd(h[2]) + '</h' + level + '>');
        i += 1;
        continue;
      }

      const bq = line.match(/^>\s?(.*)$/);
      if (bq) {
        closeList();
        out.push('<blockquote><p>' + inlineMd(bq[1]) + '</p></blockquote>');
        i += 1;
        continue;
      }

      const ul = line.match(/^\s*[-*+]\s+(.+)$/);
      if (ul) {
        if (listType !== 'ul') {
          closeList();
          out.push('<ul>');
          listType = 'ul';
        }
        out.push('<li>' + inlineMd(ul[1]) + '</li>');
        i += 1;
        continue;
      }

      const ol = line.match(/^\s*\d+\.\s+(.+)$/);
      if (ol) {
        if (listType !== 'ol') {
          closeList();
          out.push('<ol>');
          listType = 'ol';
        }
        out.push('<li>' + inlineMd(ol[1]) + '</li>');
        i += 1;
        continue;
      }

      closeList();
      out.push('<p>' + inlineMd(line) + '</p>');
      i += 1;
    }
    if (inCode) {
      out.push('<pre><code>' + escapeHtml(codeBuf.join('\n')) + '</code></pre>');
    }
    closeList();
    return out.join('\n');
  }

  function isMarkdownPath(path) {
    return /\.(md|markdown|mdown|mkd)$/i.test(path || '');
  }

  // ── Works auto-save ──
  const AUTOSAVE_DRAFT_KEY = 'kaleido_works_draft';
  let worksAutoSaveTimer = null;

  function scheduleWorksAutoSave() {
    if (worksAutoSaveTimer) clearTimeout(worksAutoSaveTimer);
    worksAutoSaveTimer = setTimeout(() => {
      worksAutoSaveTimer = null;
      if (!__wk().worksOpenPath || !__wk().worksDirty) return;
      // Save draft to localStorage for crash protection
      try {
        const draft = { path: __wk().worksOpenPath, content: $('works-content').value, ts: Date.now() };
        localStorage.setItem(AUTOSAVE_DRAFT_KEY, JSON.stringify(draft));
      } catch (_) {}
    }, 2000);
  }

  // beforeunload: warn when unsaved changes + flush draft
  window.addEventListener('beforeunload', (e) => {
    if (__wk().worksDirty && __wk().worksOpenPath) {
      // Final flush to localStorage
      try {
        const draft = { path: __wk().worksOpenPath, content: $('works-content').value, ts: Date.now() };
        localStorage.setItem(AUTOSAVE_DRAFT_KEY, JSON.stringify(draft));
      } catch (_) {}
      e.preventDefault();
      e.returnValue = '';
    }
  });

  function updateWorksPreview() {
    const pane = $('works-preview');
    if (!pane) return;
    const ta = $('works-content');
    const raw = ta ? ta.value : '';
    if (!__wk().worksOpenPath) {
      pane.innerHTML = '';
      pane.dataset.empty = '打开 Markdown 文件以预览';
      return;
    }
    if (!isMarkdownPath(__wk().worksOpenPath)) {
      pane.innerHTML = '<p class="muted sm">当前文件非 Markdown，预览不可用。</p>';
      return;
    }
    pane.innerHTML = simpleMarkdownToHtml(raw) || '';
    if (!pane.innerHTML) pane.dataset.empty = '（空文档）';
  }

  function scheduleWorksPreview() {
    if (__wk().worksPreviewTimer) clearTimeout(__wk().worksPreviewTimer);
    __wk().worksPreviewTimer = setTimeout(() => {
      __wk().worksPreviewTimer = null;
      updateWorksPreview();
    }, 180);
  }

  function setWorksPreviewMode(mode) {
    const allowed = { source: 1, split: 1, preview: 1 };
    __wk().worksPreviewMode = allowed[mode] ? mode : 'source';
    const split = $('works-editor-split');
    if (split) split.setAttribute('data-mode', __wk().worksPreviewMode);
    const toggle = $('works-preview-toggle');
    if (toggle) {
      toggle.querySelectorAll('button[data-mode]').forEach((btn) => {
        btn.classList.toggle('active', btn.getAttribute('data-mode') === __wk().worksPreviewMode);
      });
    }
    if (__wk().worksPreviewMode !== 'source') updateWorksPreview();
  }

  function wireWorksPreviewShell() {
    const toggle = $('works-preview-toggle');
    if (toggle && !toggle.dataset.wired) {
      toggle.dataset.wired = '1';
      toggle.addEventListener('click', (e) => {
        const btn = e.target.closest('button[data-mode]');
        if (!btn) return;
        setWorksPreviewMode(btn.getAttribute('data-mode'));
      });
    }
    setWorksPreviewMode(__wk().worksPreviewMode);
  }

  function setWorksOpen(path, content) {
    __wk().worksOpenPath = path || '';
    __wk().worksDirty = false;
    $('works-path').textContent = __wk().worksOpenPath || '未打开文件';
    const ta = $('works-content');
    ta.disabled = !__wk().worksOpenPath;
    ta.value = content || '';
    $('works-save').disabled = !__wk().worksOpenPath;
    if ($('works-version')) $('works-version').disabled = !__wk().worksOpenPath;
    if ($('works-versions')) $('works-versions').disabled = !__wk().worksOpenPath;
    if ($('works-version-create')) $('works-version-create').disabled = !__wk().worksOpenPath;
    if ($('works-versions-refresh')) $('works-versions-refresh').disabled = !__wk().worksOpenPath;
    if ($('works-export')) $('works-export').disabled = !__wk().worksOpenPath;
    if ($('works-move')) $('works-move').disabled = !__wk().worksOpenPath;
    if ($('works-image-preview')) $('works-image-preview').disabled = !__wk().worksOpenPath;
    if ($('works-rename')) $('works-rename').disabled = !__wk().worksOpenPath;
    if ($('works-delete')) $('works-delete').disabled = !__wk().worksOpenPath;
    updateAzDeskActions();
    if ($('works-versions-path')) {
      $('works-versions-path').textContent = __wk().worksOpenPath || '未打开文件';
    }
    // hide image pane on file switch unless user re-triggers
    const imgPane = $('works-image-pane');
    if (imgPane) imgPane.classList.add('hidden');
    // S7-W2: refresh works-preview when a file opens
    scheduleWorksPreview();
    // S7-W2 desk: refresh versions sidebar for open path
    loadWorksVersionsSidebar();
    if (__t6().currentTab === 'works') __t6().updateImmersive();
  }

  // ========== AZ-4 Author Zone controllers ==========

/* Author zone */
  function showTab(name) { __t6().switchTab(name); }

  async function loadAuthorProjects() {
    const msg = $('az-project-msg');
    if (msg) msg.textContent = '加载项目…';
    try {
      const r = await api('/api/v1/author/projects');
      __az().azProjects = Array.isArray(r.projects) ? r.projects : (Array.isArray(r) ? r : []);
      renderProjectList();
      fillProjectDatalist();
      if (msg) msg.textContent = __az().azProjects.length + ' 个项目';
      if (!__az().azSelectedProjectId && __az().azProjects.length) selectProject(__az().azProjects[0].id);
      else if (__az().azSelectedProjectId) renderComposer();
    } catch (e) {
      if (msg) msg.textContent = e.message;
    }
    // 并行加载全部剧本 pack（2026-08-10: 小说下拉要包含所有剧本，
    // 不止作者区项目——story-packs 域 work_id=pack_id）
    try {
      await refreshPackSelect();
    } catch (e) {
      window.__azPacks = window.__azPacks || [];
    }
  }

  /* 2026-08-10: 刷新小说下拉的剧本 pack 部分（故事馆新增剧本后自动出现）。
     只重拉 pack 列表 + 重填下拉，不阻塞项目数据。 */
  async function refreshPackSelect() {
    const pr = await api('/api/v1/story-tavern/packs');
    window.__azPacks = Array.isArray(pr.packs) ? pr.packs : [];
    fillProjectDatalist();
  }

  /* 2026-08-10: 关系图/伏笔的小说下拉——选项=作者项目 + 全部剧本 pack
     （标题 + work 域值），选中即切换数据源并刷新对应面板。 */
  function fillProjectDatalist() {
    const sels = ['gr-work', 'fs-work', 'an-work'];
    for (const id of sels) {
      const sel = document.getElementById(id);
      if (!sel) continue;
      const prev = sel.value;
      sel.innerHTML = '<option value="">默认当前项目</option>';
      for (const p of __az().azProjects) {
        const o = document.createElement('option');
        // value = workspace 域（数据真正存的地方）；label 显示小说标题
        o.value = p.workspaceId || p.id;
        o.textContent = p.title ? p.title : p.id;
        if (p.workspaceId && p.workspaceId === __az().azSelectedWorkspaceId) o.selected = true;
        sel.appendChild(o);
      }
      // 追加全部剧本 pack（story-packs 域，work_id=pack_id）
      sel.appendChild(document.createElement('optgroup')).label = '剧本';
      const grp = sel.lastChild;
      if (window.__azPacks && window.__azPacks.length) {
        for (const pk of window.__azPacks) {
          const o = document.createElement('option');
          o.value = pk.id;
          o.textContent = pk.title || pk.id;
          grp.appendChild(o);
        }
      } else {
        const o = document.createElement('option');
        o.value = '';
        o.textContent = '（加载中…）';
        o.disabled = true;
        grp.appendChild(o);
      }
      // 若无匹配选中项，保留"默认当前项目"
      if (prev && !sel.value) sel.value = prev;
      // 选中切换 → 全局切 workspace + 刷新当前面板
      sel.onchange = () => {
        const w = sel.value;
        if (w) {
          __az().azSelectedWorkspaceId = w;
          const proj = __az().azProjects.find((x) => (x.workspaceId || x.id) === w);
          if (proj) __az().azSelectedProjectId = proj.id;
          if (id === 'gr-work' && typeof loadGraph === 'function') loadGraph().catch(console.warn);
          if (id === 'fs-work' && typeof loadForeshadows === 'function') loadForeshadows().catch(console.warn);
          if (id === 'an-work' && typeof loadAnTasks === 'function') loadAnTasks().catch(console.warn);
          if (id === 'an-work' && typeof loadAnKinds === 'function') loadAnKinds().catch(console.warn);
        }
      };
    }
  }

  function renderProjectList() {
    const box = $('az-project-list');
    if (!box) return;
    box.innerHTML = '';
    if (!__az().azProjects.length) {
      const empty = document.createElement('div');
      empty.className = 'az-empty';
      empty.textContent = '暂无项目，点击「新建」开始';
      box.appendChild(empty);
      return;
    }
    const ico = '<svg aria-hidden="true" class="az-ico" xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z"/></svg>';
    for (const p of __az().azProjects) {
      const el = document.createElement('div');
      el.className = 'az-item' + (p.id === __az().azSelectedProjectId ? ' active' : '');
      el.innerHTML = ico + '<span class="az-title"></span>';
      el.querySelector('.az-title').textContent = p.title || p.id;
      el.onclick = () => selectProject(p.id);
      box.appendChild(el);
    }
  }

  async function createAuthorProject() {
    const title = await showPrompt('项目标题', { value: '未命名项目' });
    if (!title) return;
    try {
      const r = await api('/api/v1/author/projects', {
        method: 'POST',
        body: JSON.stringify({ title: title.trim(), livePolicy: readLivePolicyFromForm() }),
      });
      const created = r.project || r;
      if ($('az-project-msg')) $('az-project-msg').textContent = '已创建 ' + (created.title || created.id);
      await loadAuthorProjects();
      if (created.id) selectProject(created.id);
    } catch (e) {
      if ($('az-project-msg')) $('az-project-msg').textContent = e.message;
    }
  }

  function selectProject(id) {
    const p = __az().azProjects.find((x) => x.id === id);
    if (!p) return;
    __az().azSelectedProjectId = p.id;
    // 关系图/伏笔/AI分析按 workspace 域查数据（work_id=workspace_id），
    // 项目 ID(ap-xxx)≠workspace ID(uuid)——2026-08-10 前端空白根因。
    __az().azSelectedWorkspaceId = p.workspaceId || __az().azSelectedProjectId;
    __az().azSelectedProjectRoot = p.worksRoot || ('projects/' + p.id);
    __wk().worksCwd = __az().azSelectedProjectRoot;
    // reset composer selection to project defaults on first open
    __az().azSelectedCharIds = new Set(Array.isArray(p.characterIds) ? p.characterIds : []);
    __az().azSelectedWbIds = new Set(Array.isArray(p.worldBookIds) ? p.worldBookIds : []);
    if (p.defaultPlayable && /^P[1-4]$/.test(p.defaultPlayable)) __az().azSelectedPlayable = p.defaultPlayable;
    // AZ residual: remember bound session if any
    if (p.boundSessionId) __az().azBoundSessionId = p.boundSessionId;
    renderProjectList();
    renderComposer();
    applyLivePolicyToForm(p.livePolicy || {});
    const title = $('az-selected-title');
    if (title) title.textContent = p.title || p.id;
    loadWorksTree().catch((e) => { if ($('works-tree-msg')) $('works-tree-msg').textContent = e.message; });
    updateAzDeskActions();
    // 项目切换后自动刷新数据面板（2026-08-10: 关系图/伏笔/AI分析）
    if (typeof loadGraph === 'function' && $('az-view-graph') && !$('az-view-graph').classList.contains('hidden')) {
      loadGraph().catch(console.warn);
    }
    if (typeof loadForeshadows === 'function' && $('az-view-foreshadow') && !$('az-view-foreshadow').classList.contains('hidden')) {
      loadForeshadows().catch(console.warn);
    }
    if (typeof loadAnTasks === 'function' && $('az-view-analysis') && !$('az-view-analysis').classList.contains('hidden')) {
      loadAnTasks().catch(console.warn);
    }
  }

  function applyLivePolicyToForm(pol) {
    const en = $('az-live-enabled');
    const every = $('az-live-every');
    const turns = $('az-live-turns');
    if (en) en.checked = pol.enabled !== false;
    if (every) every.value = String(Math.max(1, Number(pol.everyN) || 1));
    if (turns) turns.checked = !!pol.writeTurns;
  }

  function readLivePolicyFromForm() {
    return {
      enabled: $('az-live-enabled') ? $('az-live-enabled').checked : true,
      everyN: Math.max(1, parseInt(($('az-live-every') && $('az-live-every').value) || '1', 10) || 1),
      writeTurns: $('az-live-turns') ? $('az-live-turns').checked : false,
    };
  }

  async function azSaveLivePolicy() {
    if (!__az().azSelectedProjectId) return;
    const msg = $('az-composer-msg');
    if (msg) msg.textContent = '保存落稿策略…';
    try {
      const r = await api('/api/v1/author/projects/' + encodeURIComponent(__az().azSelectedProjectId), {
        method: 'PATCH',
        body: JSON.stringify({ livePolicy: readLivePolicyFromForm() }),
      });
      const proj = r.project || r;
      const idx = __az().azProjects.findIndex((x) => x.id === __az().azSelectedProjectId);
      if (idx >= 0) __az().azProjects[idx] = Object.assign({}, __az().azProjects[idx], proj);
      applyLivePolicyToForm((proj && proj.livePolicy) || readLivePolicyFromForm());
      if (msg) msg.textContent = '落稿策略已保存';
    } catch (e) {
      if (msg) msg.textContent = e.message;
    }
  }

  function updateAzDeskActions() {
    const hasFile = !!__wk().worksOpenPath;
    const hasProj = !!__az().azSelectedProjectId;
    const pub = $('az-publish-btn');
    const inj = $('az-inject-btn');
    const kind = $('az-publish-kind');
    if (kind) kind.disabled = !hasProj;
    if (pub) pub.disabled = !hasProj || (!hasFile && (kind && kind.value !== 'promoteLive'));
    if (inj) inj.disabled = !hasProj || (!hasFile && !(($('works-content') || {}).value));
    // promoteLive only needs project
    if (pub && kind && kind.value === 'promoteLive') pub.disabled = !hasProj;
  }

  async function azPublish() {
    if (!__az().azSelectedProjectId) return;
    const msg = $('az-desk-msg') || $('works-msg');
    const kindEl = $('az-publish-kind');
    const kind = (kindEl && kindEl.value) || 'lore';
    if (msg) msg.textContent = '发布中…';
    const body = { kind };
    if (kind === 'promoteLive') {
      // server finds live via bound session / path
    } else if (__wk().worksOpenPath) {
      body.path = __wk().worksOpenPath;
    } else {
      const content = ($('works-content') && $('works-content').value) || '';
      if (!content.trim()) {
        if (msg) msg.textContent = '请先打开文稿或填写内容';
        return;
      }
      body.content = content;
    }
    try {
      const r = await api('/api/v1/author/projects/' + encodeURIComponent(__az().azSelectedProjectId) + '/publish', {
        method: 'POST',
        body: JSON.stringify(body),
      });
      if (msg) {
        const bits = ['已发布 ' + kind];
        if (r.loreCount != null) bits.push('lore=' + r.loreCount);
        if (r.chapterId) bits.push('ch=' + r.chapterId);
        if (r.destPath) bits.push(r.destPath);
        if (r.worldBookId) bits.push('wb=' + String(r.worldBookId).slice(0, 8));
        if (r.path) bits.push(r.path);
        msg.textContent = bits.join(' · ');
      }
      loadWorksTree().catch(() => {});
    } catch (e) {
      if (msg) msg.textContent = e.message;
    }
  }

  async function azInject() {
    if (!__az().azSelectedProjectId) return;
    const msg = $('az-desk-msg') || $('works-msg');
    let sid = __az().azBoundSessionId;
    try {
      if (!sid) sid = localStorage.getItem(TAVERN_SID_KEY) || '';
    } catch (_) {}
    if (!sid) {
      if (msg) msg.textContent = '无绑定会话 — 请先「开玩」或在故事馆打开会话';
      return;
    }
    const body = { sessionId: sid, asRole: 'system' };
    if (__wk().worksOpenPath) body.path = __wk().worksOpenPath;
    else {
      const content = ($('works-content') && $('works-content').value) || '';
      if (!content.trim()) {
        if (msg) msg.textContent = '请先打开文稿';
        return;
      }
      body.content = content;
    }
    if (msg) msg.textContent = '注入中…';
    try {
      const r = await api('/api/v1/author/projects/' + encodeURIComponent(__az().azSelectedProjectId) + '/inject', {
        method: 'POST',
        body: JSON.stringify(body),
      });
      if (msg) msg.textContent = '已注入会话 ' + sid.slice(0, 10) + (r.messageCount != null ? ' · msg=' + r.messageCount : '');
    } catch (e) {
      if (msg) msg.textContent = e.message;
    }
  }

  function renderComposer() {
    const charBox = $('az-char-list');
    const wbBox = $('az-wb-list');
    if (!charBox || !wbBox) return;
    charBox.innerHTML = '';
    wbBox.innerHTML = '';

    const chars = __c7().partner.characterCards || [];
    if (!chars.length) {
      charBox.innerHTML = '<div class="az-empty">暂无角色卡，可前往角色/世界创建</div>';
    } else {
      for (const c of chars) {
        charBox.appendChild(azMakeCard(c, __az().azSelectedCharIds, 'char'));
      }
    }

    const wbs = __c7().partner.worldBooks || [];
    if (!wbs.length) {
      wbBox.innerHTML = '<div class="az-empty">暂无世界书</div>';
    } else {
      for (const w of wbs) {
        wbBox.appendChild(azMakeCard(w, __az().azSelectedWbIds, 'wb'));
      }
    }

    const playBox = $('az-playable');
    if (playBox) {
      playBox.querySelectorAll('button[data-p]').forEach((btn) => {
        btn.classList.toggle('active', btn.dataset.p === __az().azSelectedPlayable);
        btn.onclick = () => {
          __az().azSelectedPlayable = btn.dataset.p;
          renderComposer();
        };
      });
    }
  }

  function azMakeCard(item, selectedSet, kind) {
    const id = item.id;
    const checked = selectedSet.has(id);
    const el = document.createElement('label');
    el.className = 'az-card' + (checked ? ' selected' : '');
    el.innerHTML = '<input type="checkbox" ' + (checked ? 'checked' : '') + '><span class="az-name"></span><span class="az-kpi"></span>';
    el.querySelector('.az-name').textContent = item.name || item.title || id;
    el.querySelector('.az-kpi').textContent = id.slice(0, 8);
    const cb = el.querySelector('input');
    cb.onchange = () => {
      if (cb.checked) selectedSet.add(id);
      else selectedSet.delete(id);
      renderComposer();
    };
    el.onclick = (e) => {
      if (e.target === cb) return;
      e.preventDefault();
      cb.checked = !cb.checked;
      cb.onchange();
    };
    return el;
  }

  async function azCompose() {
    if (!__az().azSelectedProjectId) return;
    const project = __az().azProjects.find((p) => p.id === __az().azSelectedProjectId);
    if (!project) return;
    const msg = $('az-composer-msg');
    if (msg) msg.textContent = '组合中…';
    const characterIds = Array.from(__az().azSelectedCharIds);
    const worldBookIds = Array.from(__az().azSelectedWbIds);
    try {
      const r = await api('/api/v1/author/projects/' + encodeURIComponent(__az().azSelectedProjectId) + '/compose', {
        method: 'POST',
        body: JSON.stringify({
          playable: __az().azSelectedPlayable,
          characterIds,
          worldBookIds,
        }),
      });
      // refresh local project mirror
      Object.assign(project, r.project || r);
      if (msg) msg.textContent = '组合完成' + (r.packId ? ' · Pack ' + r.packId.slice(0, 8) : '');
    } catch (e) {
      if (msg) msg.textContent = e.message;
    }
  }

  async function azLaunch() {
    if (!__az().azSelectedProjectId) return;
    const msg = $('az-composer-msg');
    const adultConfirmed = await showConfirm('即将进入故事馆游玩。若内容包含成人情节，请确认您已年满 18 岁。');
    if (!adultConfirmed) {
      if (msg) msg.textContent = '已取消启动';
      return;
    }
    if (msg) msg.textContent = '启动中…';
    try {
      const pol = readLivePolicyFromForm();
      const r = await api('/api/v1/author/projects/' + encodeURIComponent(__az().azSelectedProjectId) + '/launch', {
        method: 'POST',
        body: JSON.stringify({
          playable: __az().azSelectedPlayable,
          adultConfirmed: true,
          liveEnabled: pol.enabled,
          liveEveryN: pol.everyN,
          liveWriteTurns: pol.writeTurns,
        }),
      });
      if (r && r.sessionId) {
        __az().azBoundSessionId = r.sessionId;
        try { localStorage.setItem(TAVERN_SID_KEY, r.sessionId); } catch (_) {}
        const proj = __az().azProjects.find((x) => x.id === __az().azSelectedProjectId);
        if (proj) proj.boundSessionId = r.sessionId;
      }
      if (msg) msg.textContent = '启动成功' + (r.sessionId ? ' · 会话 ' + r.sessionId.slice(0, 8) : '');
      showTab('tavern');
      if (r && r.sessionId) {
        setTimeout(() => {
          stLoadSession(r.sessionId).catch((err) => {
            console.warn('auto resume session after launch failed', err);
          });
        }, 120);
      }
    } catch (e) {
      if (msg) msg.textContent = e.message;
    }
  }

  if ($('az-project-new')) $('az-project-new').onclick = createAuthorProject;
  if ($('az-compose-btn')) $('az-compose-btn').onclick = azCompose;
  if ($('az-launch-btn')) $('az-launch-btn').onclick = azLaunch;
  if ($('az-live-save')) $('az-live-save').onclick = azSaveLivePolicy;
  if ($('az-publish-btn')) $('az-publish-btn').onclick = azPublish;
  if ($('az-inject-btn')) $('az-inject-btn').onclick = azInject;
  if ($('az-publish-kind')) $('az-publish-kind').onchange = updateAzDeskActions;

  async function loadWorksTree() {
    const msg = $('works-tree-msg');
    if (msg) msg.textContent = '加载中…';
    try {
      const q = new URLSearchParams();
      if (__wk().worksCwd) q.set('path', __wk().worksCwd);
      q.set('depth', '1');
      const tree = await api('/api/v1/works?' + q.toString());
      $('works-cwd').textContent = '/' + (__wk().worksCwd || '');
      const list = $('works-tree');
      list.innerHTML = '';
      const children = tree.children || [];
      if (!children.length) {
        const empty = document.createElement('div');
        empty.className = 'muted sm';
        empty.textContent = '（空目录）';
        list.appendChild(empty);
      }
      const fileIco = (kind) => {
        if (kind === 'dir') return '<svg aria-hidden="true" xmlns="http://www.w3.org/2000/svg" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" style="vertical-align:-2px;margin-right:4px;opacity:.8"><path d="M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z"/></svg>';
        return '<svg aria-hidden="true" xmlns="http://www.w3.org/2000/svg" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" style="vertical-align:-2px;margin-right:4px;opacity:.7"><path d="M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z"/><path d="M14 2v4a2 2 0 0 0 2 2h4"/></svg>';
      };
      for (const c of children) {
        const el = document.createElement('div');
        el.className = 'item' + (c.path === __wk().worksOpenPath ? ' active' : '');
        el.innerHTML = '<span class="t"></span><span class="d"></span>';
        el.querySelector('.t').innerHTML = fileIco(c.kind) + escapeHtml(c.name || c.path);
        el.querySelector('.d').textContent =
          c.kind === 'dir' ? '目录' : ((c.size != null ? c.size + ' B' : '文件'));
        el.onclick = () => onWorksClick(c);
        // Right-click context menu
        el.oncontextmenu = (e) => {
          e.preventDefault();
          showWorksContextMenu(e, c);
        };
        list.appendChild(el);
      }
      $('works-tree-msg').textContent = children.length + ' 项';
    } catch (e) {
      $('works-tree-msg').textContent = e.message;
    }
  }

  // ── Works context menu ──
  function showWorksContextMenu(e, entry) {
    const existing = document.getElementById('works-ctx-menu');
    if (existing) existing.remove();

    const menu = document.createElement('div');
    menu.id = 'works-ctx-menu';
    menu.className = 'works-ctx-menu';
    menu.style.position = 'fixed';
    menu.style.left = Math.min(e.clientX, window.innerWidth - 160) + 'px';
    menu.style.top = Math.min(e.clientY, window.innerHeight - 120) + 'px';

    const items = [];
    if (entry.kind === 'dir') {
      items.push({ label: '创建文件', fn: () => promptWorksAction(entry.path, 'file') });
      items.push({ label: '创建子目录', fn: () => promptWorksAction(entry.path, 'dir') });
      items.push({ label: '重命名目录', fn: () => promptWorksRename(entry) });
      items.push({ label: '删除目录', cls: 'danger', fn: () => confirmWorksDelete(entry) });
    } else {
      items.push({ label: '重命名文件', fn: () => promptWorksRename(entry) });
      items.push({ label: '删除文件', cls: 'danger', fn: () => confirmWorksDelete(entry) });
    }

    for (const it of items) {
      const btn = document.createElement('button');
      btn.type = 'button';
      btn.textContent = it.label;
      if (it.cls) btn.className = it.cls;
      btn.onclick = (ev) => { ev.stopPropagation(); menu.remove(); it.fn(); };
      menu.appendChild(btn);
    }

    document.body.appendChild(menu);
    // Click outside to close
    const closeMenu = (ev) => {
      if (!menu.contains(ev.target)) {
        menu.remove();
        document.removeEventListener('click', closeMenu);
      }
    };
    setTimeout(() => document.addEventListener('click', closeMenu), 0);
  }

  async function promptWorksAction(parentPath, kind) {
    const label = kind === 'file' ? '文件名' : '目录名';
    const name = await showPrompt('输入' + label + '（相对于 ' + parentPath + '）：');
    if (!name) return;
    const fullPath = parentPath ? parentPath + '/' + name : name;
    if (kind === 'dir') {
      api('/api/v1/works/dir', { method: 'POST', body: JSON.stringify({ path: fullPath }) })
        .then(() => { showToast('目录已创建', 'success'); loadWorksTree(); })
        .catch((e) => showToast('创建失败：' + (e.message || e), 'error'));
    } else {
      api('/api/v1/works/file', { method: 'PUT', body: JSON.stringify({ path: fullPath, content: '' }) })
        .then(() => { showToast('文件已创建', 'success'); loadWorksTree(); })
        .catch((e) => showToast('创建失败：' + (e.message || e), 'error'));
    }
  }

  async function promptWorksRename(entry) {
    const name = await showPrompt('新名称（当前：' + entry.name + '）：', { value: entry.name });
    if (!name || name === entry.name) return;
    const parent = entry.path.substring(0, entry.path.lastIndexOf('/'));
    const newPath = parent ? parent + '/' + name : name;
    api('/api/v1/works/move', { method: 'POST', body: JSON.stringify({ from: entry.path, to: newPath }) })
      .then(() => { showToast('已重命名', 'success'); loadWorksTree(); })
      .catch((e) => showToast('重命名失败：' + (e.message || e), 'error'));
  }

  async function confirmWorksDelete(entry) {
    if (!await showConfirm('确定删除「' + entry.path + '」？')) return;
    const kind = entry.kind === 'dir' ? 'dir' : 'file';
    const url = kind === 'dir' ? '/api/v1/works/dir' : '/api/v1/works/file';
    api(url, { method: 'DELETE', body: JSON.stringify({ path: entry.path }) })
      .then(() => { showToast('已删除', 'success'); loadWorksTree(); })
      .catch((e) => showToast('删除失败：' + (e.message || e), 'error'));
  }

  async function onWorksClick(entry) {
    if (entry.kind === 'dir') {
      __wk().worksCwd = entry.path || '';
      await loadWorksTree();
      return;
    }
    try {
      const body = await api('/api/v1/works/file?path=' + encodeURIComponent(entry.path));
      setWorksOpen(body.path, body.content || '');
      $('works-msg').textContent = '已打开 ' + body.path;
      await loadWorksTree();
    } catch (e) {
      $('works-msg').textContent = e.message;
    }
  }

  $('works-refresh').onclick = () => loadWorksTree();
  $('works-up').onclick = async () => {
    const next = parentWorksPath(__wk().worksCwd);
    // AZ-4: desk is scoped to the selected project root
    if (__az().azSelectedProjectRoot && (!next || next.length < __az().azSelectedProjectRoot.length || !next.startsWith(__az().azSelectedProjectRoot))) {
      __wk().worksCwd = __az().azSelectedProjectRoot;
    } else {
      __wk().worksCwd = next;
    }
    await loadWorksTree();
  };

  $('works-mkdir').onclick = async () => {
    const path = $('works-new-dir').value.trim();
    if (!path) return;
    try {
      await api('/api/v1/works/dir', { method: 'POST', body: JSON.stringify({ path }) });
      $('works-new-dir').value = '';
      $('works-tree-msg').textContent = '已创建目录 ' + path;
      await loadWorksTree();
    } catch (e) {
      $('works-tree-msg').textContent = e.message;
    }
  };

  $('works-newfile').onclick = async () => {
    const path = $('works-new-file').value.trim();
    if (!path) return;
    try {
      const body = await api('/api/v1/works/file', {
        method: 'PUT',
        body: JSON.stringify({ path, content: '' }),
      });
      $('works-new-file').value = '';
      setWorksOpen(body.path, body.content || '');
      await loadWorksTree();
      $('works-msg').textContent = '已创建 ' + body.path;
    } catch (e) {
      $('works-tree-msg').textContent = e.message;
    }
  };

  $('works-content').addEventListener('input', () => {
    __wk().worksDirty = true;
    if (__t6().currentTab === 'works') __t6().updateImmersive();
    $('works-save').disabled = !__wk().worksOpenPath;
    scheduleWorksPreview();
    scheduleWorksAutoSave();
  });

  wireWorksPreviewShell();

  $('works-save').onclick = async () => {
    if (!__wk().worksOpenPath) return;
    try {
      const body = await api('/api/v1/works/file', {
        method: 'PUT',
        body: JSON.stringify({ path: __wk().worksOpenPath, content: $('works-content').value }),
      });
      __wk().worksDirty = false;
      if (__t6().currentTab === 'works') __t6().updateImmersive();
      $('works-msg').textContent = '已保存 ' + body.path + ' (' + (body.size || 0) + ' B)';
      showToast('文件已保存', 'success');
    } catch (e) {
      $('works-msg').textContent = e.message;
      showToast('保存失败：' + (e.message || e), 'error');
    }
  };

  async function createWorksVersion(label) {
    if (!__wk().worksOpenPath) return null;
    if (__wk().worksDirty) {
      await api('/api/v1/works/file', {
        method: 'PUT',
        body: JSON.stringify({ path: __wk().worksOpenPath, content: $('works-content').value }),
      });
      __wk().worksDirty = false;
    }
    const body = { path: __wk().worksOpenPath };
    if (label) body.label = label;
    const r = await api('/api/v1/versions', {
      method: 'POST',
      body: JSON.stringify(body),
    });
    return r;
  }

  async function restoreWorksVersion(versionId) {
    if (!__wk().worksOpenPath || !versionId) return;
    const body = await api(
      '/api/v1/versions/content?path=' +
        encodeURIComponent(__wk().worksOpenPath) +
        '&versionId=' +
        encodeURIComponent(versionId)
    );
    const ta = $('works-content');
    if (ta) ta.value = body.content || '';
    __wk().worksDirty = true;
    if (__t6().currentTab === 'works') __t6().updateImmersive();
    scheduleWorksPreview();
    if ($('works-msg')) {
      $('works-msg').textContent = '已恢复版本 ' + String(versionId).slice(0, 8) + '（未保存，点保存写入作品）';
    }
    if ($('works-versions-msg')) {
      $('works-versions-msg').textContent = 'restored ' + String(versionId).slice(0, 8);
    }
  }

  function renderWorksVersionsList(list) {
    const box = $('works-versions-list');
    if (!box) return;
    box.innerHTML = '';
    __wk().worksVersionsCache = Array.isArray(list) ? list.slice() : [];
    if (!__wk().worksVersionsCache.length) {
      const empty = document.createElement('div');
      empty.className = 'muted sm';
      empty.textContent = __wk().worksOpenPath ? '（无版本）' : '打开文件后显示版本';
      box.appendChild(empty);
      return;
    }
    __wk().worksVersionsCache.forEach((v) => {
      const el = document.createElement('div');
      el.className = 'item';
      const id = v.id || '';
      const created =
        v.createdAt != null
          ? formatDateTime(v.createdAt)
          : v.timestamp
            ? formatDateTime(v.timestamp)
            : '';
      const label = v.label || v.note || '';
      const title = document.createElement('span');
      title.className = 't';
      title.textContent = (id ? id.slice(0, 8) : '?') + (label ? ' · ' + label : '');
      const desc = document.createElement('span');
      desc.className = 'd';
      desc.textContent =
        (created || '') +
        (v.size != null ? ' · ' + v.size + ' B' : '') +
        (v.aiScore != null ? ' · score=' + v.aiScore : '');
      const actions = document.createElement('div');
      actions.className = 'row-actions';
      const restoreBtn = document.createElement('button');
      restoreBtn.type = 'button';
      restoreBtn.className = 'ghost sm';
      restoreBtn.textContent = '恢复';
      restoreBtn.onclick = async (ev) => {
        ev.stopPropagation();
        try {
          await restoreWorksVersion(id);
        } catch (e) {
          if ($('works-versions-msg')) $('works-versions-msg').textContent = e.message;
          if ($('works-msg')) $('works-msg').textContent = e.message;
        }
      };
      const loadBtn = document.createElement('button');
      loadBtn.type = 'button';
      loadBtn.className = 'ghost sm';
      loadBtn.textContent = '预览';
      loadBtn.onclick = async (ev) => {
        ev.stopPropagation();
        try {
          await restoreWorksVersion(id);
        } catch (e) {
          if ($('works-versions-msg')) $('works-versions-msg').textContent = e.message;
        }
      };
      actions.appendChild(restoreBtn);
      actions.appendChild(loadBtn);
      el.appendChild(title);
      el.appendChild(desc);
      el.appendChild(actions);
      el.onclick = () => restoreBtn.click();
      box.appendChild(el);
    });
  }

  async function loadWorksVersionsSidebar() {
    const msg = $('works-versions-msg');
    if (!__wk().worksOpenPath) {
      renderWorksVersionsList([]);
      if (msg) msg.textContent = '';
      return;
    }
    if (msg) msg.textContent = '加载版本…';
    try {
      const r = await api('/api/v1/versions?path=' + encodeURIComponent(__wk().worksOpenPath));
      const list = r.versions || [];
      renderWorksVersionsList(list);
      if (msg) msg.textContent = list.length + ' 个版本';
    } catch (e) {
      renderWorksVersionsList([]);
      if (msg) msg.textContent = e.message;
    }
  }

  async function onCreateWorksVersionClick() {
    if (!__wk().worksOpenPath) return;
    try {
      const r = await createWorksVersion();
      const id = (r && r.version && r.version.id) || (r && r.id) || '';
      if ($('works-msg')) $('works-msg').textContent = '已存版本 ' + String(id).slice(0, 8);
      await loadWorksVersionsSidebar();
    } catch (e) {
      if ($('works-msg')) $('works-msg').textContent = e.message;
    }
  }

  if ($('works-version')) {
    $('works-version').onclick = onCreateWorksVersionClick;
  }
  if ($('works-version-create')) {
    $('works-version-create').onclick = onCreateWorksVersionClick;
  }
  if ($('works-versions-refresh')) {
    $('works-versions-refresh').onclick = () => loadWorksVersionsSidebar();
  }

  if ($('works-versions')) {
    // toolbar: focus sidebar + refresh list (keep prompt fallback when sidebar missing)
    $('works-versions').onclick = async () => {
      if (!__wk().worksOpenPath) return;
      const side = $('works-versions-sidebar');
      if (side) {
        await loadWorksVersionsSidebar();
        try {
          side.scrollIntoView({ behavior: 'smooth', block: 'nearest' });
        } catch (_) {}
        if ($('works-msg')) $('works-msg').textContent = '已刷新版本侧栏';
        return;
      }
      try {
        const r = await api('/api/v1/versions?path=' + encodeURIComponent(__wk().worksOpenPath));
        const list = r.versions || [];
        if (!list.length) {
          $('works-msg').textContent = '无版本';
          return;
        }
        const lines = list
          .slice()
          .reverse()
          .map((v, i) => {
            const t = v.createdAt
              ? formatDateTime(v.createdAt)
              : v.timestamp
                ? formatDateTime(v.timestamp)
                : '';
            const score = v.aiScore != null ? ' score=' + v.aiScore : '';
            return i + 1 + '. ' + (v.id || '').slice(0, 8) + ' ' + t + score;
          })
          .join('\n');
        const pick = await showPrompt('版本列表（输入序号恢复，取消仅查看）:\n' + lines, { value: '1' });
        if (!pick) {
          $('works-msg').textContent = list.length + ' 个版本';
          return;
        }
        const idx = parseInt(pick, 10) - 1;
        const ordered = list.slice().reverse();
        const ver = ordered[idx];
        if (!ver || !ver.id) {
          $('works-msg').textContent = '无效序号';
          return;
        }
        await restoreWorksVersion(ver.id);
      } catch (e) {
        $('works-msg').textContent = e.message;
      }
    };
  }

  // --- S7-W2 desk buttons: create-untitled · export · move · image preview ---
  if ($('works-create-untitled')) {
    $('works-create-untitled').onclick = async () => {
      try {
        const body = await api('/api/v1/works/create-untitled', {
          method: 'POST',
          body: JSON.stringify({ dir: __wk().worksCwd || '' }),
        });
        const path = body.path || '';
        setWorksOpen(path, body.content || '');
        await loadWorksTree();
        if ($('works-msg')) {
          $('works-msg').textContent = '已创建 ' + path;
        }
      } catch (e) {
        if ($('works-tree-msg')) $('works-tree-msg').textContent = e.message;
        if ($('works-msg')) $('works-msg').textContent = e.message;
      }
    };
  }

  if ($('works-export')) {
    $('works-export').onclick = async () => {
      if (!__wk().worksOpenPath) return;
      try {
        const data = await api(
          '/api/v1/works/export?path=' + encodeURIComponent(__wk().worksOpenPath)
        );
        const content = data.content != null ? String(data.content) : '';
        const name = (data.path || __wk().worksOpenPath || 'export').split('/').pop() || 'export.md';
        const blob = new Blob([content], { type: 'text/plain;charset=utf-8' });
        const url = URL.createObjectURL(blob);
        const a = document.createElement('a');
        a.href = url;
        a.download = name;
        document.body.appendChild(a);
        a.click();
        a.remove();
        setTimeout(() => URL.revokeObjectURL(url), 1500);
        if ($('works-msg')) {
          $('works-msg').textContent =
            '已导出 ' +
            (data.path || __wk().worksOpenPath) +
            (data.exportedAt ? ' @ ' + data.exportedAt : '');
        }
      } catch (e) {
        if ($('works-msg')) $('works-msg').textContent = e.message;
      }
    };
  }

  if ($('works-move')) {
    $('works-move').onclick = async () => {
      if (!__wk().worksOpenPath) return;
      const to = await showPrompt('移动到（相对作品根路径）', { value: __wk().worksOpenPath });
      if (!to || to === __wk().worksOpenPath) return;
      try {
        await api('/api/v1/works/move', {
          method: 'POST',
          body: JSON.stringify({ from: __wk().worksOpenPath, to }),
        });
        setWorksOpen(to, $('works-content') ? $('works-content').value : '');
        await loadWorksTree();
        if ($('works-msg')) $('works-msg').textContent = '已移动 → ' + to;
      } catch (e) {
        if ($('works-msg')) $('works-msg').textContent = e.message;
      }
    };
  }

  if ($('works-image-preview')) {
    $('works-image-preview').onclick = async () => {
      if (!__wk().worksOpenPath) return;
      const pane = $('works-image-pane');
      const img = $('works-image-preview-el');
      const imsg = $('works-image-msg');
      try {
        const data = await api(
          '/api/v1/works/image-data-url?path=' + encodeURIComponent(__wk().worksOpenPath)
        );
        const url = data.dataUrl || data.url || data.dataURL || '';
        if (!url) throw new Error('no dataUrl in response');
        if (img) img.src = url;
        if (pane) pane.classList.remove('hidden');
        if (imsg) imsg.textContent = __wk().worksOpenPath;
        if ($('works-msg')) $('works-msg').textContent = '图片预览已加载';
      } catch (e) {
        if (pane) pane.classList.add('hidden');
        if (imsg) imsg.textContent = e.message;
        if ($('works-msg')) $('works-msg').textContent = '图片预览失败: ' + e.message;
      }
    };
  }

  // --- style presets: list / save / apply via /api/v1/style-presets ---

/* S2.9: works bridge — settings.js (real module) rename/delete flows need
 * setWorksOpen (_works-part) + loadWorksTree/loadWorksVersionsSidebar (here). */
try {
  window.__kaleidoWorksBridge = {
    setWorksOpen: setWorksOpen,
    loadWorksTree: loadWorksTree,
    loadWorksVersionsSidebar: loadWorksVersionsSidebar,
  };
} catch (_) {}

export { loadAuthorProjects, loadWorksTree, refreshPackSelect, loadWorksVersionsSidebar };
