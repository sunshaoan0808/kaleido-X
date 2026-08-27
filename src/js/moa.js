/**
 * src/js/moa.js — 模型对比（多模型并发） 真 ESM 模块（P1-3 S2.13；原 _moa-part.js）。
 * 出边仅 tabs 切页时的守卫调用，经 converted[] import 恒真化，闭包零编辑。
 * 顶层副作用（DOMContentLoaded init / window.P5Ai*·MoaLoad* 发布）原样保留。
 */
import { showToast } from './toast.js';
import { showConfirm } from './dialog.js';

/* MoA 模型对比面板（T8 前端）—— panel 管理 + 同一 prompt 并发派发多模型 + 结果对比 */
  function MoaApi(path, opts) {
    opts = opts || {};
    var t = localStorage.getItem('kaleido_token') || localStorage.getItem('token') || '';
    var heads = Object.assign({ 'Content-Type': 'application/json' }, opts.headers || {});
    if (t) { heads.Authorization = 'Bearer ' + t; heads['X-Mobile-Token'] = t; }
    return fetch(path, Object.assign({}, opts, { headers: heads, cache: 'no-store' })).then(function (r) {
      if (!r.ok) {
        return r.json().catch(function () { return {}; }).then(function (j) {
          var er = new Error(j && (j.error || j.message) || ('HTTP ' + r.status));
          er.status = r.status; throw er;
        });
      }
      var ct = r.headers.get('content-type') || '';
      return ct.indexOf('application/json') >= 0 ? r.json() : r.text();
    });
  }
  function MoaEsc(v) { return String(v == null ? '' : v).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;'); }

  // ── 面板列表 ──
  async function MoaLoadPanels() {
    var box = document.getElementById('moa-panels');
    if (!box) return;
    box.innerHTML = '<p class="muted sm">加载中…</p>';
    var list = [];
    try { var r = await MoaApi('/api/v1/moa/panels'); list = (r && r.panels) || []; }
    catch (e) { box.innerHTML = '<p class="muted sm">面板加载失败：' + MoaEsc(e.message || e) + '</p>'; return; }
    if (!list.length) { box.innerHTML = '<p class="muted sm">暂无对比面板。点击「＋ 新建面板」创建一组模型。</p>'; }
    else {
      box.innerHTML = list.map(function (p) {
        var eps = (p.endpoints || []).map(function (e) {
          return '<span class="moa-ep">' + MoaEsc(e.label || e.model || e.id) + ' <code>' + MoaEsc(e.model) + '</code></span>';
        }).join(' ');
        return '<div class="moa-panel-item">'
          + '<div class="moa-panel-head"><b>' + MoaEsc(p.name) + '</b>'
          + '<span class="muted sm">' + (p.endpoint_count || 0) + ' 模型 · ' + MoaEsc(p.panel_id) + '</span>'
          + '<button type="button" class="ghost sm" onclick="MoaDelPanel(\'' + MoaEsc(p.panel_id) + '\')">删除</button></div>'
          + '<div class="moa-eps">' + eps + '</div>'
          + '</div>';
      }).join('');
    }
    MoaFillRunSelect(list);
  }

  function MoaFillRunSelect(list) {
    var sel = document.getElementById('moa-run-panel');
    if (!sel) return;
    var cur = sel.value;
    sel.innerHTML = '<option value="">— 选择对比面板 —</option>' + list.map(function (p) {
      return '<option value="' + MoaEsc(p.panel_id) + '">' + MoaEsc(p.name) + '（' + (p.endpoint_count || 0) + ' 模型）</option>';
    }).join('');
    if (cur) sel.value = cur;
  }

  async function MoaDelPanel(pid) {
    if (!await showConfirm('删除面板 ' + pid + '？')) return;
    try { await MoaApi('/api/v1/moa/panels/' + encodeURIComponent(pid), { method: 'DELETE' }); MoaLoadPanels(); }
    catch (e) { showToast('删除失败：' + (e.message || e), 'error'); }
  }

  // ── 新建面板 ──
  function MoaNew() {
    var box = document.getElementById('moa-create');
    if (!box) return;
    box.classList.remove('hidden');
    document.getElementById('moa-f-name').value = '';
    MoaRenderEpRows([{ id: '', provider: 'cli-proxy', model: '', label: '' }, { id: '', provider: 'cli-proxy', model: '', label: '' }]);
    document.getElementById('moa-f-name').focus();
  }
  function MoaCancelNew() { var b = document.getElementById('moa-create'); if (b) b.classList.add('hidden'); }

  function MoaRenderEpRows(rows) {
    var wrap = document.getElementById('moa-f-eps');
    if (!wrap) return;
    wrap.innerHTML = (rows || []).map(function (r, i) {
      return '<div class="moa-ep-row">'
        + '<input class="moa-ep-id" placeholder="id（如 ds）" value="' + MoaEsc(r.id) + '" />'
        + '<input class="moa-ep-model" placeholder="模型（如 deepseek-v4-flash）" value="' + MoaEsc(r.model) + '" />'
        + '<input class="moa-ep-label" placeholder="显示名（如 DeepSeek V4）" value="' + MoaEsc(r.label) + '" />'
        + '<input class="moa-ep-prov" placeholder="provider" value="' + MoaEsc(r.provider || 'cli-proxy') + '" />'
        + '<button type="button" class="ghost sm" onclick="MoaEpDel(this)">✕</button>'
        + '</div>';
    }).join('');
  }
  function MoaEpAdd() {
    var wrap = document.getElementById('moa-f-eps');
    if (!wrap) return;
    var div = document.createElement('div');
    div.className = 'moa-ep-row';
    div.innerHTML = '<input class="moa-ep-id" placeholder="id" value="" />'
      + '<input class="moa-ep-model" placeholder="模型" value="" />'
      + '<input class="moa-ep-label" placeholder="显示名" value="" />'
      + '<input class="moa-ep-prov" placeholder="provider" value="cli-proxy" />'
      + '<button type="button" class="ghost sm" onclick="MoaEpDel(this)">✕</button>';
    wrap.appendChild(div);
  }
  function MoaEpDel(btn) {
    var wrap = document.getElementById('moa-f-eps');
    if (wrap && wrap.children.length > 1) btn.closest('.moa-ep-row').remove();
  }

  async function MoaSaving() {
    var name = (document.getElementById('moa-f-name').value || '').trim();
    var rows = Array.prototype.map.call(document.querySelectorAll('#moa-f-eps .moa-ep-row'), function (r) {
      var id = r.querySelector('.moa-ep-id').value.trim();
      var model = r.querySelector('.moa-ep-model').value.trim();
      var label = r.querySelector('.moa-ep-label').value.trim() || model || id;
      var provider = r.querySelector('.moa-ep-prov').value.trim() || 'cli-proxy';
      return id && model ? { id: id, provider: provider, model: model, label: label } : null;
    }).filter(Boolean);
    if (!name) { showToast('请填写面板名称', 'warning'); return; }
    if (!rows.length) { showToast('至少需要一个有效的模型行（id + 模型名）', 'warning'); return; }
    try {
      await MoaApi('/api/v1/moa/panels', { method: 'POST', body: JSON.stringify({ name: name, description: 'Web 创建', endpoints: rows }) });
      MoaCancelNew();
      MoaLoadPanels();
      showToast('面板已创建');
    } catch (e) { showToast('创建失败：' + (e.message || e), 'error'); }
  }

  // ── 运行对比 ──
  async function MoaRun() {
    var pid = document.getElementById('moa-run-panel').value;
    var prompt = document.getElementById('moa-run-prompt').value;
    var mt = parseInt(document.getElementById('moa-run-maxtok').value || '1000', 10);
    var agg = !!(document.getElementById('moa-run-aggregate') && document.getElementById('moa-run-aggregate').checked);
    var out = document.getElementById('moa-out');
    var btn = document.getElementById('moa-run');
    if (!pid) { showToast('请先选择对比面板', 'warning'); return; }
    if (!prompt.trim()) { showToast('请输入 prompt', 'warning'); return; }
    if (btn) { btn.disabled = true; btn.textContent = agg ? '对比+聚合中…' : '对比中…'; }
    if (out) out.textContent = agg ? '已派发，等待各模型返回并聚合…' : '已派发，等待各模型返回…';
    try {
      var r = await MoaApi('/api/v1/moa/run', { method: 'POST', body: JSON.stringify({ panel_id: pid, prompt: prompt, max_tokens: Math.max(40, Math.min(mt, 8192)), aggregate: agg }) });
      var sid = r && r.session_id;
      if (!sid) throw new Error('未返回 session_id');
      await MoaPoll(sid, prompt, out, agg);
    } catch (e) {
      if (out) out.textContent = '运行失败：' + (e.message || e);
    } finally {
      if (btn) { btn.disabled = false; btn.textContent = '▶ 并发派发'; }
    }
  }

  async function MoaPoll(sid, prompt, out, agg) {
    var tries = 0;
    while (tries < 120) {
      tries++;
      await new Promise(function (res) { setTimeout(res, 2000); });
      try {
        var s = await MoaApi('/api/v1/moa/sessions/' + encodeURIComponent(sid));
        if (s && (s.status === 'complete' || s.status === 'failed')) {
          // 聚合 pass 在 complete 之后仍在后台跑：若请求了聚合但还没聚合结果，继续等
          if (agg && s.status === 'complete' && !s.aggregated && !s.aggregate_error) {
            if (out) out.textContent = '并排完成，聚合中…（' + tries + '×2s）';
            continue;
          }
          MoaRenderSession(s, out);
          MoaLoadSessions();
          return;
        }
        if (out) out.textContent = (agg ? '对比+聚合中' : '对比中') + '…（' + tries + '×2s）';
      } catch (e) { /* 轮询瞬断忽略 */ }
    }
    if (out) out.textContent = '超时：后台仍在运行，可稍后到「历史对比」查看。session=' + sid;
  }

  function MoaRenderSession(s, out) {
    if (!out) out = document.getElementById('moa-out');
    if (!out) return;
    var rows = (s.results || []);
    var aggHtml = '';
    if (s.aggregated) {
      aggHtml = '<div class="moa-agg">'
        + '<div class="moa-agg-head">🎯 聚合答案'
        + (s.aggregate_elapsed_ms != null ? ' <span class="muted sm">聚合器 ' + s.aggregate_elapsed_ms + 'ms</span>' : '')
        + '</div>'
        + '<div class="moa-agg-body">' + MoaEsc(s.aggregated) + '</div>'
        + '</div>';
    } else if (s.aggregate_error) {
      aggHtml = '<div class="moa-agg moa-agg-err">'
        + '<div class="moa-agg-head">⚠️ 聚合失败</div>'
        + '<div class="moa-agg-body">' + MoaEsc(s.aggregate_error) + '</div>'
        + '</div>';
    }
    var head = '<h3 class="settings-sub">' + MoaEsc(s.session_id) + ' · ' + (s.status === 'complete' ? '✅ 完成' : '❌ 失败') + '</h3>'
      + '<div class="moa-query">' + MoaEsc(s.prompt) + '</div>'
      + aggHtml;
    var tbl = '<table class="moa-table"><thead><tr><th>模型</th><th>状态</th><th>耗时</th><th>输出</th></tr></thead><tbody>'
      + rows.map(function (r) {
        var ok = !r.error;
        var body = r.error ? '<span class="moa-err">' + MoaEsc(r.error) + '</span>'
          : '<span class="moa-text">' + MoaEsc((r.raw_text || '').slice(0, 500)) + '</span>';
        return '<tr><td><b>' + MoaEsc(r.endpoint_id) + '</b></td>'
          + '<td>' + (ok ? '✅' : '❌') + '</td>'
          + '<td>' + (r.elapsed_ms != null ? r.elapsed_ms + 'ms' : '—') + '</td>'
          + '<td>' + body + '</td></tr>';
      }).join('')
      + '</tbody></table>';
    out.innerHTML = head + tbl;
  }

  // ── 历史 session ──
  async function MoaLoadSessions() {
    var box = document.getElementById('moa-sessions');
    if (!box) return;
    var list = [];
    try { var r = await MoaApi('/api/v1/moa/sessions'); list = (r && r.sessions) || []; }
    catch (e) { box.innerHTML = '<p class="muted sm">历史加载失败</p>'; return; }
    if (!list.length) { box.innerHTML = '<p class="muted sm">暂无对比记录。</p>'; return; }
    list.sort(function (a, b) { return String(b.session_id).localeCompare(String(a.session_id)); });
    box.innerHTML = list.slice(0, 20).map(function (s) {
      var st = s.status === 'complete' ? '✅' : (s.status === 'failed' ? '❌' : '⏳');
      return '<div class="moa-hist-item">'
        + '<span class="muted sm">' + st + ' ' + MoaEsc(s.session_id) + '</span> '
        + '<code class="muted sm">' + (s.result_count || 0) + ' 结果</code> '
        + '<span class="muted sm">' + MoaEsc(s.created_prompt || '') + '</span>'
        + ' <button type="button" class="ghost sm" onclick="MoaHistOpen(\'' + MoaEsc(s.session_id) + '\')">查看</button>'
        + '</div>';
    }).join('');
  }
  async function MoaHistOpen(sid) {
    try {
      var s = await MoaApi('/api/v1/moa/sessions/' + encodeURIComponent(sid));
      MoaRenderSession(s, document.getElementById('moa-out'));
    } catch (e) { showToast('加载失败：' + (e.message || e), 'error'); }
  }

  function MoaInit() {
    var nb = document.getElementById('moa-new');
    if (nb) nb.addEventListener('click', MoaNew);
    var sv = document.getElementById('moa-f-save');
    if (sv) sv.addEventListener('click', MoaSaving);
    var cl = document.getElementById('moa-f-cancel');
    if (cl) cl.addEventListener('click', MoaCancelNew);
    var ad = document.getElementById('moa-f-add-ep');
    if (ad) ad.addEventListener('click', MoaEpAdd);
    var rn = document.getElementById('moa-run');
    if (rn) rn.addEventListener('click', MoaRun);
    document.addEventListener('click', function (ev) {
      var b = ev.target && ev.target.closest ? ev.target.closest('[data-tab="moa"]') : null;
      if (b) setTimeout(function () { MoaLoadPanels(); MoaLoadSessions(); }, 60);
    });
  }
  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', MoaInit);
  else MoaInit();
  try { window.MoaLoadPanels = MoaLoadPanels; window.MoaLoadSessions = MoaLoadSessions; } catch (e) {}

/* ===== exports consumed by remaining closure parts (Mechanism Y) ===== */
export { MoaLoadPanels, MoaLoadSessions };
