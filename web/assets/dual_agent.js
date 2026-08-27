/* U12 双 Agent 分工面板（作者区 az-nav「双Agent」独立脚本）
 * 依赖后端：/api/v1/dual-agent/sessions（create/plan/handoff/windows/stage/resume/state）
 * 独立于 app.js bundle，与 scene_cards.js 同一注入方式。 */
(function () {
  'use strict';

  function apiBase() {
    var el = document.getElementById('api-base');
    if (el && el.value && String(el.value).trim()) return String(el.value).trim().replace(/\/+$/, '');
    return location.origin;
  }
  function token() { return localStorage.getItem('kaleido_token') || ''; }
  function msg(text) { var el = document.getElementById('da-msg'); if (el) el.textContent = text || ''; }
  function esc(s) {
    return String(s == null ? '' : s).replace(/[&<>"']/g, function (c) {
      return { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c];
    });
  }
  async function req(method, url, body) {
    var headers = { 'Authorization': 'Bearer ' + token() };
    if (body) headers['Content-Type'] = 'application/json';
    var r = await fetch(apiBase() + url, {
      method: method,
      headers: headers,
      body: body ? JSON.stringify(body) : undefined,
    });
    if (r.status === 204) return null;
    var data = null;
    try { data = await r.json(); } catch (e) { /* ignore */ }
    if (!r.ok) throw new Error((data && data.error) || ('HTTP ' + r.status));
    return data;
  }

  var SESSIONS = [];
  var selectedId = '';

  function byId(id) { return document.getElementById(id); }

  /* 统一会话对象形态：state（列表/状态视图字段：sessionId/stageProgress/…）
   * + raw（完整会话：id/windows/plan/transcript）。保证 renderState 两种来源一致。 */
  function toCanonical(raw, st) {
    var c = {};
    if (st && typeof st === 'object') {
      Object.keys(st).forEach(function (k) { c[k] = st[k]; });
    }
    if (raw && typeof raw === 'object') {
      Object.keys(raw).forEach(function (k) {
        if (c[k] === undefined || c[k] === null) c[k] = raw[k];
      });
    }
    if (!c.sessionId) c.sessionId = c.id || '';
    c.windows = (raw && raw.windows) || [];
    c.plan = (raw && raw.plan) || null;
    return c;
  }

  function stageLabel(name) {
    return {
      context_assembly: '上下文组装',
      writing: '写作',
      review: '审稿',
      user_confirm: '用户确认',
      styling: '风格润色',
      compression: '压缩归档',
    }[name] || name;
  }

  function actionLabel(a) {
    return {
      run_plan: '下一步：执行规划',
      confirm_plan: '下一步：确认规划提案',
      run_handoff: '下一步：交接写作',
      write_windows: '下一步：撰写写作窗口',
      advance_stage: '下一步：推进阶段',
      done: '工作流已完成',
    }[a] || a || '';
  }

  async function loadList() {
    var btn = byId('da-refresh');
    if (btn) btn.disabled = true;
    msg('加载会话…');
    try {
      var data = await req('GET', '/api/v1/dual-agent/sessions');
      SESSIONS = (data && data.sessions) || [];
      renderList();
      msg('共 ' + SESSIONS.length + ' 个双Agent会话');
      if (selectedId && !SESSIONS.some(function (s) { return s.sessionId === selectedId; })) {
        selectedId = '';
      }
      if (selectedId) renderState();
    } catch (e) {
      msg('加载失败：' + e.message);
    } finally {
      if (btn) btn.disabled = false;
    }
  }

  function renderList() {
    var box = byId('da-list');
    var work = (byId('da-work').value || '').trim();
    if (!box) return;
    box.innerHTML = '';
    var list = SESSIONS.filter(function (s) {
      if (!work) return true;
      return (s.workId || '').toLowerCase().indexOf(work.toLowerCase()) >= 0;
    });
    if (!list.length) {
      var empty = document.createElement('div');
      empty.className = 'muted sm';
      empty.style.cssText = 'padding:12px';
      empty.textContent = work ? '该作品下暂无会话 — 输入作品 ID 后点「新建」' : '暂无会话 — 输入作品 ID 后点「新建」';
      box.appendChild(empty);
      return;
    }
    list.forEach(function (s) {
      var el = document.createElement('div');
      el.className = 'az-list-item' + (s.sessionId === selectedId ? ' active' : '');
      el.style.cssText = 'padding:10px 12px;border-bottom:1px solid var(--border,rgba(0,0,0,.08));cursor:pointer';
      var role = s.activeRole === 'writing' ? '✍ Dante' : '🌿 Goethe';
      el.innerHTML =
        '<div style="display:flex;justify-content:space-between;gap:8px;align-items:center">' +
        '<strong style="min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap">' + esc(s.title) + '</strong>' +
        '<span class="sm" style="flex-shrink:0">' + role + '</span>' +
        '</div>' +
        '<div class="muted sm" style="margin-top:2px">' + esc(s.workId) + ' · ' + stageLabel(s.stage) + ' · ' +
        (s.stageProgress || 0) + '/' + (s.stageCount || 6) + ' 阶段</div>' +
        '<div class="sm" style="margin-top:2px">' + (s.nextAction ? actionLabel(s.nextAction) : '') + '</div>';
      el.onclick = function () {
        selectedId = s.sessionId;
        renderList();
        renderState();
        refreshSelected();
      };
      box.appendChild(el);
    });
  }

  async function createSession() {
    var work = (byId('da-work').value || '').trim();
    if (!work) {
      msg('请先输入作品 ID');
      return;
    }
    var btn = byId('da-new');
    if (btn) btn.disabled = true;
    msg('创建中…');
    try {
      var body = { workId: work };
      var title = (byId('da-title').value || '').trim();
      if (title) body.title = title;
      var data = await req('POST', '/api/v1/dual-agent/sessions', body);
      var s = data.session || data;
      selectedId = data.state && data.state.sessionId ? data.state.sessionId : (s.id || s.sessionId);
      msg('已创建 ' + (s.title || s.id));
      await loadList();
    } catch (e) {
      msg('创建失败：' + e.message);
    } finally {
      if (btn) btn.disabled = false;
    }
  }

  async function loadState() {
    if (!selectedId) return;
    var s = null;
    for (var i = 0; i < SESSIONS.length; i++) {
      if (SESSIONS[i].sessionId === selectedId) { s = SESSIONS[i]; break; }
    }
    if (s) { renderState(); return; }
    try {
      var data = await req('GET', '/api/v1/dual-agent/sessions/' + encodeURIComponent(selectedId));
      SESSIONS.push(toCanonical(data.session, data.state));
      renderState();
    } catch (e) {
      msg('读取会话失败：' + e.message);
    }
  }

  function renderState() {
    var selected = byId('da-selected');
    var stateEl = byId('da-state');
    var planBtn = byId('da-plan');
    var handoffBtn = byId('da-handoff');
    var resumeBtn = byId('da-resume');
    if (selected) selected.textContent = selectedId || '未选择会话';
    if (!selectedId) {
      if (stateEl) stateEl.innerHTML = '<div class="muted sm">选择左侧会话，或新建一个双Agent会话。</div>';
      renderWindows([]);
      renderPlan(null);
      return;
    }
    var s = SESSIONS.filter(function (x) { return x.sessionId === selectedId; })[0];
    if (!s) return;
    var done = s.stageProgress || 0;
    var total = s.stageCount || 6;
    var pct = total ? Math.round((done / total) * 100) : 0;
    var role = s.activeRole === 'writing' ? 'Dante · 写作' : 'Goethe · 规划';
    stateEl.innerHTML =
      '<div style="display:flex;justify-content:space-between;gap:8px;align-items:center;flex-wrap:wrap">' +
      '<strong>' + esc(role) + '</strong>' +
      '<span class="sm muted">' + esc(s.workId) + '</span></div>' +
      '<div class="sm" style="margin:6px 0 4px">阶段：' + esc(stageLabel(s.stage)) + '（' + done + '/' + total + '）</div>' +
      '<div class="progress-track" style="height:6px;margin:4px 0"><div class="progress-fill" style="--progress-fill-x:' + pct + '"></div></div>' +
      '<div class="sm muted" style="margin-top:6px">' + esc(actionLabel(s.nextAction || '')) + '</div>' +
      (s.llmNote ? '<div class="sm" style="margin-top:6px;color:var(--warn,#d4a853)">' + esc(s.llmNote) + '</div>' : '') +
      (s.pendingConfirmation && s.planReady && !s.handoffDone
        ? '<div class="sm" style="margin-top:6px;color:var(--warn,#d4a853)">规划提案待确认 — 先确认（输入「确认/没问题」或 POST /confirm-plan），再交接写作。</div>'
        : '') +
      (s.error ? '<div class="sm" style="margin-top:6px;color:var(--err,#f87171)">' + esc(s.error) + '</div>' : '') +
      ((s.review || []).length ? '<div class="sm" style="margin-top:8px;font-weight:600">审稿结果</div>' +
        '<div style="margin-top:4px">' + (s.review || []).map(function (r) {
          var color = r.severity === 'major' ? 'var(--err,#f87171)' : 'var(--warn,#d4a853)';
          return '<div class="sm" style="margin-top:2px;color:' + color + '">[' + esc(r.severity) + '] ' + esc(r.issue) + (r.windowId ? '（' + esc(r.windowId) + '）' : '') + '</div>';
        }).join('') + '</div>' : '') +
      ((s.summaries || []).length ? '<div class="sm" style="margin-top:8px;font-weight:600">章节摘要</div>' +
        '<div style="margin-top:4px">' + (s.summaries || []).map(function (x) {
          return '<div class="sm muted" style="margin-top:2px">' + esc(x.windowId || x.chapterId || '') + '：' + esc((x.summary || '').slice(0, 100)) + '</div>';
        }).join('') + '</div>' : '') +
      ((s.styledWindows || []).length ? '<div class="sm muted" style="margin-top:6px">🎨 ' + (s.styledWindows || []).length + ' 个窗口已风格化</div>' : '') +
      '<div class="sm muted" style="margin-top:6px">更新：' + esc((s.updatedAt || '').replace('T', ' ').slice(0, 16)) + '</div>';
    if (planBtn) planBtn.disabled = !!(s.planReady || s.handoffDone);
    if (handoffBtn) handoffBtn.disabled = !s.planReady || s.handoffDone || s.pendingConfirmation;
    if (resumeBtn) resumeBtn.disabled = false;
    var reviewBtn = byId('da-review');
    var stylingBtn = byId('da-styling');
    var compressBtn = byId('da-compress');
    var autoConfirm = byId('da-auto-confirm');
    var allWritten = (s.windows || []).length > 0 && (s.windows || []).every(function (w) { return w.status === 'written'; });
    if (reviewBtn) reviewBtn.disabled = !allWritten;
    if (stylingBtn) stylingBtn.disabled = !allWritten;
    if (compressBtn) compressBtn.disabled = !allWritten;
    if (autoConfirm) autoConfirm.checked = !!s.autoConfirm;
    renderWindows((s.windows || []).map(function (w, i) { return w; }));
    renderPlan(s.plan || null);
  }

  async function refreshSelected() {
    if (!selectedId) return;
    try {
      var data = await req('GET', '/api/v1/dual-agent/sessions/' + encodeURIComponent(selectedId));
      var full = toCanonical(data.session, data.state);
      var found = false;
      for (var i = 0; i < SESSIONS.length; i++) {
        if (SESSIONS[i].sessionId === selectedId) { SESSIONS[i] = full; found = true; break; }
      }
      if (!found) SESSIONS.push(full);
      renderState();
      renderList();
    } catch (e) {
      msg('刷新失败：' + e.message);
    }
  }

  function renderWindows(windows) {
    var box = byId('da-windows');
    if (!box) return;
    box.innerHTML = '';
    if (!windows.length) {
      var empty = document.createElement('div');
      empty.className = 'muted sm';
      empty.style.cssText = 'padding:12px';
      empty.textContent = '暂无写作窗口 — 完成规划后点「交接写作」生成';
      box.appendChild(empty);
      return;
    }
    windows.forEach(function (w) {
      var el = document.createElement('div');
      el.className = 'az-list-item';
      el.style.cssText = 'padding:10px 12px;border-bottom:1px solid var(--border,rgba(0,0,0,.08))';
      var badge = w.status === 'written'
        ? '<span class="sm" style="color:var(--teal,#2dd4bf)">✓ 已写</span>'
        : '<span class="sm" style="color:var(--text-tertiary)">待写</span>';
      el.innerHTML =
        '<div style="display:flex;justify-content:space-between;gap:8px;align-items:center">' +
        '<strong style="min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap">' + esc(w.title) + '</strong>' + badge +
        '</div>' +
        '<div class="muted sm" style="margin-top:2px">' + esc(w.chapterId) + ' · ' + (w.wordTarget || 2000) + ' 字</div>' +
        (w.outline ? '<div class="sm" style="margin-top:4px;color:var(--text-secondary)">' + esc(w.outline) + '</div>' : '');
      var actions = document.createElement('div');
      actions.style.cssText = 'display:flex;gap:6px;margin-top:6px;flex-wrap:wrap';
      if (w.status !== 'written') {
        var aiBtn = document.createElement('button');
        aiBtn.type = 'button';
        aiBtn.className = 'ghost sm';
        aiBtn.textContent = '⚡ AI 写稿';
        aiBtn.onclick = function () { generateWindow(w.id); };
        actions.appendChild(aiBtn);
        var writeBtn = document.createElement('button');
        writeBtn.type = 'button';
        writeBtn.className = 'ghost sm';
        writeBtn.textContent = '手动写稿';
        writeBtn.onclick = function () { writeWindow(w.id); };
        actions.appendChild(writeBtn);
      }
      if (w.draft) {
        var showBtn = document.createElement('button');
        showBtn.type = 'button';
        showBtn.className = 'ghost sm';
        showBtn.textContent = '查看草稿';
        showBtn.onclick = function () {
          window.prompt(w.title + ' 草稿（复制到剪贴板）', w.draft || '');
        };
        actions.appendChild(showBtn);
        if (w.status === 'written') {
          var pubBtn = document.createElement('button');
          pubBtn.type = 'button';
          pubBtn.className = 'ghost sm';
          pubBtn.textContent = '💾 落盘章节';
          pubBtn.onclick = function () { publishWindow(w.id); };
          actions.appendChild(pubBtn);
        }
      }
      el.appendChild(actions);
      box.appendChild(el);
    });
  }

  function renderPlan(plan) {
    var box = byId('da-planview');
    if (!box) return;
    if (!plan) {
      box.innerHTML = '<div class="muted sm">规划尚未产出 — 点「执行规划」生成设定 / 大纲 / 伏笔清单。</div>';
      return;
    }
    var html = '<div class="sm" style="font-weight:600;margin-bottom:4px">方向</div>' +
      '<div class="sm" style="margin-bottom:8px;white-space:pre-wrap">' + esc(plan.direction || '—') + '</div>';
    if (plan.settings && plan.settings.length) {
      html += '<div class="sm" style="font-weight:600;margin-bottom:4px">设定</div><div style="margin-bottom:8px">';
      plan.settings.forEach(function (s) {
        html += '<div class="sm" style="padding:2px 0">· ' + esc(s.key || '') + '：' + esc(s.value || '') + '</div>';
      });
      html += '</div>';
    }
    if (plan.outline && plan.outline.length) {
      html += '<div class="sm" style="font-weight:600;margin-bottom:4px">大纲</div><div style="margin-bottom:8px">';
      plan.outline.forEach(function (o) {
        html += '<div class="sm" style="padding:2px 0">· ' + esc(o.title || o.chapter || '?') + (o.goal ? ' — ' + esc(o.goal) : '') + '</div>';
      });
      html += '</div>';
    }
    if (plan.foreshadowItems && plan.foreshadowItems.length) {
      html += '<div class="sm" style="font-weight:600;margin-bottom:4px">伏笔</div><div style="margin-bottom:8px">';
      plan.foreshadowItems.forEach(function (f) {
        html += '<div class="sm" style="padding:2px 0">· ' + esc(f.id || '') + ' ' + esc(f.desc || '') + '（' + esc(f.plantChapter || '') + ' → ' + esc(f.payoffChapter || '') + '）</div>';
      });
      html += '</div>';
    }
    if (plan.nextWindow && plan.nextWindow.length) {
      html += '<div class="sm" style="font-weight:600;margin-bottom:4px">待写窗口</div>' +
        '<div class="sm">' + esc(plan.nextWindow.join('、')) + '</div>';
    }
    box.innerHTML = html;
  }

  async function runAction(url, busyText) {
    if (!selectedId) return;
    var btn = byId('da-plan');
    if (btn) btn.disabled = true;
    msg(busyText + '…');
    try {
      var r = await req('POST', '/api/v1/dual-agent/sessions/' + encodeURIComponent(selectedId) + url, {});
      // U12-A3 handoff 协议展示：blocked 时按 nextAction/missingItems 提示面板。
      if (url === '/handoff' && r && r.blocked) {
        var items = (r.missingItems || []).join('、') || r.error || '必要资产';
        msg('交接被阻止：缺少 ' + items + '（' + (r.nextAction || '') + '）');
      } else {
        msg(busyText + ' 完成' + (url === '/handoff' && r && r.nextAction ? '（' + r.nextAction + '）' : ''));
      }
      await refreshSelected();
    } catch (e) {
      msg(busyText + '失败：' + e.message);
    } finally {
      if (btn) btn.disabled = false;
    }
  }

  async function generateWindow(windowId) {
    if (!selectedId) return;
    msg('Dante 写作中…（LLM 可能耗时较长）');
    try {
      var r = await req('POST', '/api/v1/dual-agent/sessions/' + encodeURIComponent(selectedId) + '/windows/' + encodeURIComponent(windowId) + '/generate', {});
      msg(r.llmNote || 'AI 写稿完成');
      await refreshSelected();
    } catch (e) {
      msg('AI 写稿失败：' + e.message);
      await refreshSelected();
    }
  }

  async function publishWindow(windowId) {
    if (!selectedId) return;
    msg('落盘中…');
    try {
      var r = await req('POST', '/api/v1/dual-agent/sessions/' + encodeURIComponent(selectedId) + '/windows/' + encodeURIComponent(windowId) + '/publish', {});
      msg(r.llmNote || ('已落盘：' + (r.path || '')));
      await refreshSelected();
    } catch (e) {
      msg('落盘失败：' + e.message);
      await refreshSelected();
    }
  }

  async function runStageAction(url, busyText) {
    if (!selectedId) return;
    msg(busyText + '…');
    try {
      var r = await req('POST', '/api/v1/dual-agent/sessions/' + encodeURIComponent(selectedId) + url, {});
      var extra = '';
      if (url === '/review' && r.review) {
        var majors = r.review.filter(function (x) { return x.severity === 'major'; }).length;
        var minors = r.review.filter(function (x) { return x.severity === 'minor'; }).length;
        extra = '（major ' + majors + ' / minor ' + minors + '）';
        if (r.review.length) {
          var lines = r.review.map(function (x) { return '[' + x.severity + '] ' + esc(x.issue) + (x.windowId ? '（' + x.windowId + '）' : ''); }).join('\n');
          msg(busyText + ' 完成' + extra + '\n' + lines);
          return;
        }
      }
      if (url === '/styling' && r.styledWindows != null) extra = '（' + r.styledWindows + ' 个窗口）';
      if (url === '/compress' && r.summariesCount != null) extra = '（' + r.summariesCount + ' 个摘要）';
      msg(busyText + ' 完成' + extra);
      await refreshSelected();
    } catch (e) {
      msg(busyText + '失败：' + e.message);
      await refreshSelected();
    }
  }

  async function writeWindow(windowId) {
    if (!selectedId) return;
    var draft = window.prompt('Dante 撰写正文（写作窗口 ' + windowId + '）：');
    if (draft == null) return;
    draft = draft.trim();
    if (!draft) { msg('草稿为空'); return; }
    msg('写入草稿…');
    try {
      var r = await req('POST', '/api/v1/dual-agent/sessions/' + encodeURIComponent(selectedId) + '/windows/' + encodeURIComponent(windowId) + '/write', { content: draft });
      msg('已写入窗口 ' + windowId + '（阶段：' + (r.stage || '') + '）');
      await refreshSelected();
    } catch (e) {
      msg('写稿失败：' + e.message);
    }
  }

  async function advanceStage() {
    if (!selectedId) return;
    var name = window.prompt('推进阶段（' + [
      'context_assembly', 'writing', 'review', 'user_confirm', 'styling', 'compression'
    ].join(' / ') + '），默认为当前阶段的下一个：');
    if (name == null) return;
    var stageName = (name || '').trim() || '';
    msg('推进阶段…');
    try {
      var body = { name: stageName, status: 'complete', message: '手动推进（前端）' };
      var r = await req('POST', '/api/v1/dual-agent/sessions/' + encodeURIComponent(selectedId) + '/stage', body);
      msg('阶段已推进 → ' + (r.state && r.state.stage || r.stage || ''));
      await refreshSelected();
    } catch (e) {
      msg('推进失败：' + e.message);
    }
  }

  function bind() {
    var work = byId('da-work');
    var title = byId('da-title');
    var list = byId('da-list');
    var newBtn = byId('da-new');
    var planBtn = byId('da-plan');
    var handoffBtn = byId('da-handoff');
    var resumeBtn = byId('da-resume');
    var refreshBtn = byId('da-refresh');
    if (!work || !list) return;
    // 自动带出当前选中项目 ID（作者区）
    var azSel = null;
    try { azSel = localStorage.getItem('kaleido_az_lastview'); } catch (e) { }
    try {
      var p = document.querySelector('.az-project-pane .az-item.active .az-title');
      if (p) work.value = p.textContent.trim();
    } catch (e) { }
    if (newBtn) newBtn.addEventListener('click', createSession);
    if (planBtn) planBtn.addEventListener('click', function () { runAction('/plan', '执行规划'); });
    if (handoffBtn) handoffBtn.addEventListener('click', function () { runAction('/handoff', '交接写作'); });
    if (resumeBtn) resumeBtn.addEventListener('click', function () { runAction('/resume', '恢复会话'); });
    var reviewBtn = byId('da-review');
    var stylingBtn = byId('da-styling');
    var compressBtn = byId('da-compress');
    var autoConfirm = byId('da-auto-confirm');
    if (reviewBtn) reviewBtn.addEventListener('click', function () { runStageAction('/review', 'AI 审稿'); });
    if (stylingBtn) stylingBtn.addEventListener('click', function () { runStageAction('/styling', '风格统一'); });
    if (compressBtn) compressBtn.addEventListener('click', function () { runStageAction('/compress', '章节摘要'); });
    if (autoConfirm) autoConfirm.addEventListener('change', async function () {
      if (!selectedId) { autoConfirm.checked = false; return; }
      try {
        await req('POST', '/api/v1/dual-agent/sessions/' + encodeURIComponent(selectedId) + '/auto-confirm', { enabled: autoConfirm.checked });
        msg(autoConfirm.checked ? '已开启自动确认' : '已关闭自动确认');
      } catch (e) {
        msg('切换自动确认失败：' + e.message);
        autoConfirm.checked = !autoConfirm.checked;
      }
    });
    if (refreshBtn) refreshBtn.addEventListener('click', function () { loadList(); });
    if (work) work.addEventListener('keydown', function (e) { if (e.key === 'Enter') createSession(); });
    if (title) title.addEventListener('keydown', function (e) { if (e.key === 'Enter') createSession(); });
    if (work) work.addEventListener('input', function () { renderList(); });
    loadList();
  }
  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', bind);
  } else {
    bind();
  }
})();
