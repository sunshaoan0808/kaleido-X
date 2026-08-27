/* U3 场记卡视图（作者区 az-nav「场记」面板独立脚本）
 * 依赖后端：GET/DELETE /api/v1/works/{work_id}/scene-cards
 * 独立于 app.js bundle，避免压缩产物内联修改风险。 */
(function () {
  'use strict';
  function apiBase() {
    var el = document.getElementById('api-base');
    if (el && el.value && String(el.value).trim()) return String(el.value).trim().replace(/\/+$/, '');
    return location.origin;
  }
  function token() { return localStorage.getItem('kaleido_token') || ''; }
  function msg(text) { var el = document.getElementById('sc-msg'); if (el) el.textContent = text || ''; }
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

  async function load() {
    var work = (document.getElementById('sc-work').value || '').trim();
    var list = document.getElementById('sc-list');
    var btn = document.getElementById('sc-refresh');
    if (!work) { list.innerHTML = '<div class="muted sm" style="padding:12px">请输入或选择项目 ID</div>'; return; }
    if (btn) btn.disabled = true;
    msg('加载中…');
    try {
      var data = await req('GET', '/api/v1/works/' + encodeURIComponent(work) + '/scene-cards');
      var cards = (data && data.cards) || [];
      list.innerHTML = '';
      if (!cards.length) { list.innerHTML = '<div class="muted sm" style="padding:12px">暂无场记卡——推进剧情回合后自动生成（摘要变化时落卡）</div>'; }
      cards.forEach(function (c) {
        var item = document.createElement('div');
        item.className = 'az-list-item';
        item.style.cssText = 'padding:12px;border-bottom:1px solid var(--border,rgba(0,0,0,.08))';
        var meta = '回合 #' + c.turn + (c.node_id ? ' · 节点 ' + esc(c.node_id) : '');
        var time = c.created_at ? c.created_at.replace('T', ' ').slice(0, 16) : '';
        item.innerHTML =
          '<div style="display:flex;justify-content:space-between;gap:8px;align-items:center">' +
          '<strong>' + esc(c.scene) + '</strong>' +
          '<button type="button" class="ghost sm" data-del="' + esc(c.id) + '" style="flex-shrink:0">删</button>' +
          '</div>' +
          '<div class="muted sm" style="margin-top:2px">' + meta + (time ? ' · ' + time : '') + '</div>' +
          '<div class="sm" style="margin-top:6px;white-space:pre-wrap">' + esc(c.summary) + '</div>';
        list.appendChild(item);
      });
      msg('共 ' + cards.length + ' 张场记卡');
    } catch (e) {
      msg('加载失败：' + e.message);
    } finally {
      if (btn) btn.disabled = false;
    }
  }

  async function removeCard(work, cardId) {
    if (!cardId) return;
    try {
      await req('DELETE', '/api/v1/works/' + encodeURIComponent(work) + '/scene-cards/' + encodeURIComponent(cardId));
      msg('已删除 ' + cardId);
      load();
    } catch (e) { msg('删除失败：' + e.message); }
  }

  async function clearAll(work) {
    try {
      var data = await req('DELETE', '/api/v1/works/' + encodeURIComponent(work) + '/scene-cards');
      msg('已清空 ' + ((data && data.deleted) || 0) + ' 张');
      load();
    } catch (e) { msg('清空失败：' + e.message); }
  }

  function bind() {
    var refresh = document.getElementById('sc-refresh');
    var clear = document.getElementById('sc-clear');
    var list = document.getElementById('sc-list');
    var workInput = document.getElementById('sc-work');
    if (!refresh || !list) return;
    refresh.addEventListener('click', load);
    if (clear) clear.addEventListener('click', function () {
      var work = (workInput.value || '').trim();
      if (!work) { msg('请先输入项目 ID'); return; }
      if (window.confirm('确认清空作品「' + work + '」的全部场记卡？此操作不可逆。')) clearAll(work);
    });
    if (workInput) {
      workInput.addEventListener('keydown', function (e) { if (e.key === 'Enter') load(); });
      workInput.addEventListener('change', load);
    }
    list.addEventListener('click', function (e) {
      var t = e.target;
      var btn = t.closest && t.closest('[data-del]');
      if (btn) {
        var work = (workInput.value || '').trim();
        if (work) removeCard(work, btn.getAttribute('data-del'));
      }
    });
    load();
  }
  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', bind);
  } else {
    bind();
  }
})();
