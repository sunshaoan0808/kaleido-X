/**
 * src/js/partner.js — 角色卡/世界书/正则库管理域真 ESM 模块（P1-3 S2.16）。
 * 出边：loadPartner（Mechanism Y；jobs 走既有 __kaleidoPartner 门面）。
 * 编辑态 lets 留 _state-part，经 __pf()=__kaleidoPartnerEdit 门面读写。
 */
import { $ } from './dom.js';
import { api } from './api.js';
import { showConfirm } from './dialog.js';
import { uid } from './utils.js';

const __pf = () => window.__kaleidoPartnerEdit;
const __c7 = () => window.__kaleidoChatState;

/* Partner */
  async function loadPartner() {
    __c7().partner = await api('/api/v1/partner');
    __ch().refreshPartnerSelects();
    renderPartnerLists();
    renderBondPage();
    if (__pf().editWbId) {
      // refresh content textarea if still selected
      const w = (__c7().partner.worldBooks || []).find((x) => x.id === __pf().editWbId);
      if (w && $('wb-content') && document.activeElement !== $('wb-content')) {
        $('wb-content').value = w.content || '';
      }
    }
  }


  // ─── Partner editors: world-book entries / card regex / global regex ─────
  function splitKeys(text) {
    return String(text || '')
      .split(/[\n,，;；|]+/)
      .map((s) => s.trim())
      .filter(Boolean);
  }

  function entryUid(e) {
    if (!e || typeof e !== 'object') return '';
    return String(e.uid || e.id || e.key || '');
  }

  function entryLabel(e) {
    if (!e) return '条目';
    const keys = e.keys || e.key || [];
    const k = Array.isArray(keys) ? keys.join(', ') : String(keys || '');
    const c = e.comment || e.name || '';
    const head = c || k || entryUid(e) || '未命名';
    const flags = [];
    if (e.constant) flags.push('常驻');
    if (e.disabled === true || e.enabled === false) flags.push('关');
    if (e.vectorized) flags.push('向量');
    return flags.length ? head + ' · ' + flags.join('/') : head;
  }

  function clearWbEntryForm() {
    __pf().editWbEntryId = '';
    if ($('wbe-comment')) $('wbe-comment').value = '';
    if ($('wbe-keys')) $('wbe-keys').value = '';
    if ($('wbe-content')) $('wbe-content').value = '';
    if ($('wbe-enabled')) $('wbe-enabled').checked = true;
    if ($('wbe-constant')) $('wbe-constant').checked = false;
    if ($('wbe-vectorized')) $('wbe-vectorized').checked = false;
    if ($('wbe-order')) $('wbe-order').value = '100';
    if ($('wbe-msg')) $('wbe-msg').textContent = '';
    renderWbEntryList();
  }

  function renderWbEntryList() {
    const list = $('wb-entry-list');
    if (!list) return;
    list.innerHTML = '';
    if (!__pf().editWbId) {
      list.setAttribute('data-empty', '先选中或保存世界书');
      return;
    }
    if (!__pf().editWbEntries.length) {
      list.setAttribute('data-empty', '暂无条目，可新增或点「重建条目」');
      return;
    }
    for (const e of __pf().editWbEntries) {
      const el = document.createElement('div');
      const uid = entryUid(e);
      el.className = 'item' + (uid && uid === __pf().editWbEntryId ? ' active' : '');
      el.innerHTML = '<span class="t"></span><span class="d"></span>';
      el.querySelector('.t').textContent = entryLabel(e);
      const keys = e.keys || e.key || [];
      el.querySelector('.d').textContent = (Array.isArray(keys) ? keys.slice(0, 4).join(', ') : String(keys || '')).slice(0, 48) || uid;
      el.onclick = () => selectWbEntry(e);
      list.appendChild(el);
    }
  }

  function selectWbEntry(e) {
    if (!e) return clearWbEntryForm();
    __pf().editWbEntryId = entryUid(e);
    const keys = e.keys || e.key || [];
    if ($('wbe-comment')) $('wbe-comment').value = e.comment || e.name || '';
    if ($('wbe-keys')) $('wbe-keys').value = Array.isArray(keys) ? keys.join(', ') : String(keys || '');
    if ($('wbe-content')) $('wbe-content').value = e.content || '';
    const enabled = !(e.disabled === true || e.enabled === false);
    if ($('wbe-enabled')) $('wbe-enabled').checked = enabled;
    if ($('wbe-constant')) $('wbe-constant').checked = !!e.constant;
    if ($('wbe-vectorized')) $('wbe-vectorized').checked = !!(e.vectorized || (e.extensions && e.extensions.vectorized));
    if ($('wbe-order')) $('wbe-order').value = e.order != null ? e.order : (e.displayIndex != null ? e.displayIndex : 100);
    if ($('wbe-msg')) $('wbe-msg').textContent = '编辑 ' + (__pf().editWbEntryId || '新条目');
    renderWbEntryList();
  }

  async function loadWbEntries(wbId) {
    if (!wbId) {
      __pf().editWbEntries = [];
      clearWbEntryForm();
      return;
    }
    try {
      const r = await api('/api/v1/partner/world-books/' + encodeURIComponent(wbId) + '/entries');
      __pf().editWbEntries = Array.isArray(r.entries) ? r.entries : (Array.isArray(r) ? r : []);
      // keep selection if still present
      if (__pf().editWbEntryId && !__pf().editWbEntries.some((e) => entryUid(e) === __pf().editWbEntryId)) {
        clearWbEntryForm();
      } else {
        renderWbEntryList();
        if (__pf().editWbEntryId) {
          const cur = __pf().editWbEntries.find((e) => entryUid(e) === __pf().editWbEntryId);
          if (cur) selectWbEntry(cur);
        }
      }
      if ($('wbe-msg')) $('wbe-msg').textContent = '条目 ' + __pf().editWbEntries.length + ' 条';
    } catch (e) {
      __pf().editWbEntries = [];
      renderWbEntryList();
      if ($('wbe-msg')) $('wbe-msg').textContent = '加载条目失败：' + e.message;
    }
  }

  function collectWbEntryForm() {
    const keys = splitKeys($('wbe-keys') ? $('wbe-keys').value : '');
    const enabled = $('wbe-enabled') ? !!$('wbe-enabled').checked : true;
    const vectorized = $('wbe-vectorized') ? !!$('wbe-vectorized').checked : false;
    const body = {
      keys,
      content: $('wbe-content') ? $('wbe-content').value : '',
      comment: $('wbe-comment') ? $('wbe-comment').value.trim() : '',
      enabled,
      disabled: !enabled,
      constant: $('wbe-constant') ? !!$('wbe-constant').checked : false,
      order: Number(($('wbe-order') && $('wbe-order').value) || 100),
      vectorized,
    };
    if (__pf().editWbEntryId) {
      body.uid = __pf().editWbEntryId;
      body.id = __pf().editWbEntryId;
    }
    return body;
  }

  async function saveWbEntry() {
    if (!__pf().editWbId) {
      if ($('wbe-msg')) $('wbe-msg').textContent = '请先保存/选中世界书';
      return;
    }
    const body = collectWbEntryForm();
    if (!body.keys.length && !body.constant) {
      if ($('wbe-msg')) $('wbe-msg').textContent = '请填关键词，或勾选常驻';
      return;
    }
    if ($('wbe-msg')) $('wbe-msg').textContent = '保存条目中…';
    try {
      let r;
      if (__pf().editWbEntryId) {
        r = await api('/api/v1/partner/world-books/' + encodeURIComponent(__pf().editWbId) + '/entries/' + encodeURIComponent(__pf().editWbEntryId), {
          method: 'PATCH',
          body: JSON.stringify(body),
        });
      } else {
        r = await api('/api/v1/partner/world-books/' + encodeURIComponent(__pf().editWbId) + '/entries', {
          method: 'POST',
          body: JSON.stringify(body),
        });
      }
      const entry = r.entry || r;
      __pf().editWbEntryId = entryUid(entry) || __pf().editWbEntryId;
      await loadWbEntries(__pf().editWbId);
      await loadPartner();
      const wb = (__c7().partner.worldBooks || []).find((w) => w.id === __pf().editWbId);
      if (wb) {
        if ($('wb-content')) $('wb-content').value = wb.content || '';
      }
      if ($('wbe-msg')) $('wbe-msg').textContent = '条目已保存 ' + (__pf().editWbEntryId || '');
    } catch (e) {
      if ($('wbe-msg')) $('wbe-msg').textContent = e.message;
    }
  }

  async function deleteWbEntry() {
    if (!__pf().editWbId || !__pf().editWbEntryId) return;
    if (!await showConfirm('删除条目 ' + __pf().editWbEntryId + '？')) return;
    try {
      await api('/api/v1/partner/world-books/' + encodeURIComponent(__pf().editWbId) + '/entries/' + encodeURIComponent(__pf().editWbEntryId), {
        method: 'DELETE',
      });
      clearWbEntryForm();
      await loadWbEntries(__pf().editWbId);
      await loadPartner();
      if ($('wbe-msg')) $('wbe-msg').textContent = '已删除';
    } catch (e) {
      if ($('wbe-msg')) $('wbe-msg').textContent = e.message;
    }
  }

  async function previewWbEntry() {
    if (!__pf().editWbId) {
      if ($('wbe-msg')) $('wbe-msg').textContent = '先选世界书';
      return;
    }
    const keys = splitKeys($('wbe-keys') ? $('wbe-keys').value : '');
    const probe = keys[0] || ($('wbe-comment') && $('wbe-comment').value) || '设定';
    if ($('wbe-msg')) $('wbe-msg').textContent = 'WI 预览中…';
    try {
      const r = await api('/api/v1/partner/wi-preview', {
        method: 'POST',
        body: JSON.stringify({
          worldBookId: __pf().editWbId,
          characterCardId: __pf().editCcId || undefined,
          dryRun: true,
          messages: [{ role: 'user', content: '请说明一下' + probe + '相关设定' }],
        }),
      });
      if ($('wbe-preview-out')) $('wbe-preview-out').textContent = JSON.stringify(r, null, 2).slice(0, 4000);
      const act = (r && (r.activated || r.wi && r.wi.activated)) || [];
      if ($('wbe-msg')) $('wbe-msg').textContent = '预览完成 · 激活 ' + (Array.isArray(act) ? act.length : '?');
    } catch (e) {
      if ($('wbe-msg')) $('wbe-msg').textContent = e.message;
      if ($('wbe-preview-out')) $('wbe-preview-out').textContent = String(e.message || e);
    }
  }

  async function rebuildWbBook() {
    if (!__pf().editWbId) return;
    if ($('wb-msg')) $('wb-msg').textContent = '重建条目中…';
    try {
      const r = await api('/api/v1/partner/world-books/' + encodeURIComponent(__pf().editWbId) + '/rebuild-st-book', {
        method: 'POST',
        body: '{}',
      });
      await loadPartner();
      const wb = (__c7().partner.worldBooks || []).find((w) => w.id === __pf().editWbId);
      if (wb) selectWb(wb);
      else await loadWbEntries(__pf().editWbId);
      if ($('wb-msg')) $('wb-msg').textContent = '重建完成 · 条目 ' + (r.count != null ? r.count : (r.entries || []).length);
    } catch (e) {
      if ($('wb-msg')) $('wb-msg').textContent = e.message;
    }
  }

  // —— Card regex ——
  function regexLabel(s, i) {
    if (!s) return '脚本 ' + (i + 1);
    return s.scriptName || s.name || s.id || ('脚本 ' + (i + 1));
  }

  function clearCcRegexForm() {
    __pf().editCcRegexIdx = -1;
    if ($('ccr-name')) $('ccr-name').value = '';
    if ($('ccr-find')) $('ccr-find').value = '';
    if ($('ccr-replace')) $('ccr-replace').value = '';
    if ($('ccr-disabled')) $('ccr-disabled').checked = false;
    if ($('ccr-md')) $('ccr-md').checked = false;
    if ($('ccr-prompt')) $('ccr-prompt').checked = false;
    if ($('ccr-placement')) $('ccr-placement').value = '1,2';
    if ($('ccr-msg')) $('ccr-msg').textContent = '';
    renderCcRegexList();
  }

  function renderCcRegexList() {
    const list = $('cc-regex-list');
    if (!list) return;
    list.innerHTML = '';
    if (!__pf().editCcRegexScripts.length) {
      list.setAttribute('data-empty', '暂无卡内正则');
      return;
    }
    __pf().editCcRegexScripts.forEach((s, i) => {
      const el = document.createElement('div');
      el.className = 'item' + (i === __pf().editCcRegexIdx ? ' active' : '');
      el.innerHTML = '<span class="t"></span><span class="d"></span>';
      el.querySelector('.t').textContent = regexLabel(s, i) + (s.disabled ? ' · 关' : '');
      el.querySelector('.d').textContent = (s.findRegex || s.find_regex || '').slice(0, 40);
      el.onclick = () => selectCcRegex(i);
      list.appendChild(el);
    });
  }

  function selectCcRegex(i) {
    const s = __pf().editCcRegexScripts[i];
    if (!s) return clearCcRegexForm();
    __pf().editCcRegexIdx = i;
    if ($('ccr-name')) $('ccr-name').value = s.scriptName || s.name || '';
    if ($('ccr-find')) $('ccr-find').value = s.findRegex || s.find_regex || '';
    if ($('ccr-replace')) $('ccr-replace').value = s.replaceString != null ? s.replaceString : (s.replace_string || '');
    if ($('ccr-disabled')) $('ccr-disabled').checked = !!(s.disabled === true || s.disabled === 1);
    if ($('ccr-md')) $('ccr-md').checked = !!(s.markdownOnly || s.markdown_only);
    if ($('ccr-prompt')) $('ccr-prompt').checked = !!(s.promptOnly || s.prompt_only);
    const pl = Array.isArray(s.placement) ? s.placement.join(',') : (s.placement || '1,2');
    if ($('ccr-placement')) $('ccr-placement').value = String(pl);
    if ($('ccr-msg')) $('ccr-msg').textContent = '编辑 #' + (i + 1);
    renderCcRegexList();
  }

  function collectCcRegexForm() {
    const placement = String(($('ccr-placement') && $('ccr-placement').value) || '1,2')
      .split(/[,\s]+/)
      .map((x) => Number(x))
      .filter((n) => !Number.isNaN(n));
    return {
      id: (__pf().editCcRegexIdx >= 0 && __pf().editCcRegexScripts[__pf().editCcRegexIdx] && (__pf().editCcRegexScripts[__pf().editCcRegexIdx].id || __pf().editCcRegexScripts[__pf().editCcRegexIdx].scriptName)) || undefined,
      scriptName: ($('ccr-name') && $('ccr-name').value.trim()) || ('script-' + Date.now().toString(36)),
      findRegex: ($('ccr-find') && $('ccr-find').value.trim()) || '',
      replaceString: ($('ccr-replace') && $('ccr-replace').value) || '',
      disabled: $('ccr-disabled') ? !!$('ccr-disabled').checked : false,
      markdownOnly: $('ccr-md') ? !!$('ccr-md').checked : false,
      promptOnly: $('ccr-prompt') ? !!$('ccr-prompt').checked : false,
      placement: placement.length ? placement : [1, 2],
    };
  }

  function applyCcRegexLocal() {
    const s = collectCcRegexForm();
    if (!s.findRegex) {
      if ($('ccr-msg')) $('ccr-msg').textContent = '需要 findRegex';
      return;
    }
    if (__pf().editCcRegexIdx >= 0) __pf().editCcRegexScripts[__pf().editCcRegexIdx] = Object.assign({}, __pf().editCcRegexScripts[__pf().editCcRegexIdx], s);
    else __pf().editCcRegexScripts.push(s);
    renderCcRegexList();
    if ($('ccr-msg')) $('ccr-msg').textContent = '已写入内存 · 请点「保存角色卡」持久化（' + __pf().editCcRegexScripts.length + ' 条）';
  }

  function deleteCcRegexLocal() {
    if (__pf().editCcRegexIdx < 0) return;
    __pf().editCcRegexScripts.splice(__pf().editCcRegexIdx, 1);
    clearCcRegexForm();
    if ($('ccr-msg')) $('ccr-msg').textContent = '已从列表移除 · 保存角色卡后生效';
  }

  function testRegexScripts(scripts, sample, outEl, msgEl) {
    let out = sample == null ? '' : String(sample);
    const list = Array.isArray(scripts) ? scripts : [];
    for (const s of list) {
      if (!s || s.disabled === true || s.disabled === 1) continue;
      const re = compileStFindRegex(s.findRegex || s.find_regex || '');
      if (!re) continue;
      let rep = s.replaceString != null ? String(s.replaceString) : (s.replace_string != null ? String(s.replace_string) : '');
      try {
        out = out.replace(re, function () {
          const args = arguments;
          const full = args[0];
          let r = rep.replace(/\{\{match\}\}/gi, full);
          r = r.replace(/\$(\d+)/g, (_, n) => {
            const g = args[Number(n)];
            return g == null ? '' : String(g);
          });
          return r;
        });
      } catch (e) {
        if (msgEl) msgEl.textContent = '试运行失败：' + e.message;
      }
    }
    if (outEl) outEl.textContent = out;
    if (msgEl) msgEl.textContent = '试运行完成（' + list.length + ' 脚本）';
  }

  // —— Global regex library ——
  async function loadRegexLibrary() {
    try {
      const r = await api('/api/v1/regex-library');
      __pf().editRxScripts = Array.isArray(r.scripts) ? r.scripts.slice() : [];
      __pf().regexLibraryMeta.priority = r.priority || 'card_over_library';
      __pf().regexLibraryMeta.updatedAt = r.updatedAt || 0;
      if ($('rx-priority')) $('rx-priority').value = __pf().regexLibraryMeta.priority;
      renderRxList();
      if ($('rx-msg')) $('rx-msg').textContent = '库脚本 ' + __pf().editRxScripts.length + ' · ' + __pf().regexLibraryMeta.priority;
    } catch (e) {
      if ($('rx-msg')) $('rx-msg').textContent = '加载正则库失败：' + e.message;
    }
  }

  function renderRxList() {
    const list = $('rx-list');
    if (!list) return;
    list.innerHTML = '';
    if (!__pf().editRxScripts.length) {
      list.setAttribute('data-empty', '库为空，可新建或导入');
      return;
    }
    __pf().editRxScripts.forEach((s, i) => {
      const el = document.createElement('div');
      el.className = 'item' + (i === __pf().editRxIdx ? ' active' : '');
      el.innerHTML = '<span class="t"></span><span class="d"></span>';
      el.querySelector('.t').textContent = regexLabel(s, i) + (s.disabled ? ' · 关' : '');
      el.querySelector('.d').textContent = (s.findRegex || s.find_regex || '').slice(0, 48);
      el.onclick = () => selectRx(i);
      list.appendChild(el);
    });
  }

  function clearRxForm() {
    __pf().editRxIdx = -1;
    if ($('rx-name')) $('rx-name').value = '';
    if ($('rx-find')) $('rx-find').value = '';
    if ($('rx-replace')) $('rx-replace').value = '';
    if ($('rx-disabled')) $('rx-disabled').checked = false;
    if ($('rx-md')) $('rx-md').checked = false;
    if ($('rx-prompt')) $('rx-prompt').checked = false;
    if ($('rx-placement')) $('rx-placement').value = '1,2';
    renderRxList();
  }

  function selectRx(i) {
    const s = __pf().editRxScripts[i];
    if (!s) return clearRxForm();
    __pf().editRxIdx = i;
    if ($('rx-name')) $('rx-name').value = s.scriptName || s.name || '';
    if ($('rx-find')) $('rx-find').value = s.findRegex || s.find_regex || '';
    if ($('rx-replace')) $('rx-replace').value = s.replaceString != null ? s.replaceString : (s.replace_string || '');
    if ($('rx-disabled')) $('rx-disabled').checked = !!(s.disabled === true || s.disabled === 1);
    if ($('rx-md')) $('rx-md').checked = !!(s.markdownOnly || s.markdown_only);
    if ($('rx-prompt')) $('rx-prompt').checked = !!(s.promptOnly || s.prompt_only);
    const pl = Array.isArray(s.placement) ? s.placement.join(',') : (s.placement || '1,2');
    if ($('rx-placement')) $('rx-placement').value = String(pl);
    if ($('rx-msg')) $('rx-msg').textContent = '编辑库脚本 #' + (i + 1);
    renderRxList();
  }

  function collectRxForm() {
    const placement = String(($('rx-placement') && $('rx-placement').value) || '1,2')
      .split(/[,\s]+/)
      .map((x) => Number(x))
      .filter((n) => !Number.isNaN(n));
    const prev = __pf().editRxIdx >= 0 ? __pf().editRxScripts[__pf().editRxIdx] : null;
    return {
      id: (prev && (prev.id || prev.scriptName)) || undefined,
      scriptName: ($('rx-name') && $('rx-name').value.trim()) || ('lib-' + Date.now().toString(36)),
      findRegex: ($('rx-find') && $('rx-find').value.trim()) || '',
      replaceString: ($('rx-replace') && $('rx-replace').value) || '',
      disabled: $('rx-disabled') ? !!$('rx-disabled').checked : false,
      markdownOnly: $('rx-md') ? !!$('rx-md').checked : false,
      promptOnly: $('rx-prompt') ? !!$('rx-prompt').checked : false,
      placement: placement.length ? placement : [1, 2],
    };
  }

  function applyRxLocal() {
    const s = collectRxForm();
    if (!s.findRegex) {
      if ($('rx-msg')) $('rx-msg').textContent = '需要 findRegex';
      return;
    }
    if (__pf().editRxIdx >= 0) __pf().editRxScripts[__pf().editRxIdx] = Object.assign({}, __pf().editRxScripts[__pf().editRxIdx], s);
    else __pf().editRxScripts.push(s);
    renderRxList();
    if ($('rx-msg')) $('rx-msg').textContent = '已写入列表 · 再点「保存整库」持久化';
  }

  function deleteRxLocal() {
    if (__pf().editRxIdx < 0) return;
    __pf().editRxScripts.splice(__pf().editRxIdx, 1);
    clearRxForm();
    if ($('rx-msg')) $('rx-msg').textContent = '已从列表删除 · 保存整库后生效';
  }

  async function saveRegexLibrary() {
    if ($('rx-msg')) $('rx-msg').textContent = '保存整库…';
    try {
      const priority = ($('rx-priority') && $('rx-priority').value) || 'card_over_library';
      const r = await api('/api/v1/regex-library', {
        method: 'PUT',
        body: JSON.stringify({ priority, scripts: __pf().editRxScripts }),
      });
      __pf().editRxScripts = Array.isArray(r.scripts) ? r.scripts.slice() : __pf().editRxScripts;
      __pf().regexLibraryMeta.priority = r.priority || priority;
      renderRxList();
      if ($('rx-msg')) $('rx-msg').textContent = '整库已保存 · ' + __pf().editRxScripts.length + ' 条';
    } catch (e) {
      if ($('rx-msg')) $('rx-msg').textContent = e.message;
    }
  }


  function renderPartnerLists() {
    const wbList = $('wb-list');
    const ccList = $('cc-list');
    wbList.innerHTML = '';
    ccList.innerHTML = '';
    for (const w of __c7().partner.worldBooks || []) {
      const el = document.createElement('div');
      el.className = 'item' + (w.id === __pf().editWbId ? ' active' : '');
      el.innerHTML = '<span class="t"></span><span class="d is-id"></span>';
      el.querySelector('.t').textContent = w.name || shortId(w.id) || '未命名世界书';
      el.querySelector('.d').textContent = shortId(w.id);
      el.querySelector('.d').title = w.id || '';
      el.onclick = () => selectWb(w);
      wbList.appendChild(el);
    }
    for (const c of __c7().partner.characterCards || []) {
      const el = document.createElement('div');
      el.className = 'item' + (c.id === __pf().editCcId ? ' active' : '');
      el.innerHTML = '<span class="t"></span><span class="d is-id"></span>';
      el.querySelector('.t').textContent = c.name || shortId(c.id) || '未命名角色';
      el.querySelector('.d').textContent = shortId(c.worldBookId || c.id);
      el.querySelector('.d').title = (c.worldBookId || c.id || '');
      el.onclick = () => selectCc(c);
      ccList.appendChild(el);
    }
  }

  function selectWb(w) {
    __pf().editWbId = w.id;
    $('wb-name').value = w.name || '';
    const f = w.fields || {};
    $('wb-theme').value = f.theme || '';
    $('wb-era').value = f.era || '';
    $('wb-geography').value = f.geography || '';
    $('wb-conflict').value = f.conflict || '';
    $('wb-content').value = w.content || '';
    renderPartnerLists();
    clearWbEntryForm();
    loadWbEntries(__pf().editWbId);
  }

  function selectCc(c) {
    __pf().editCcId = c.id;
    $('cc-name').value = c.name || '';
    $('cc-wb').value = c.worldBookId || '';
    const f = c.fields || {};
    $('cc-occupation').value = f.occupation || '';
    $('cc-ext').value = f.externalPersonality || '';
    $('cc-speak').value = f.speakingStyle || '';
    $('cc-rel').value = f.userRelationType || '';
    if ($('cc-description')) $('cc-description').value = f.description || f.char_description || '';
    if ($('cc-personality')) $('cc-personality').value = f.personality || '';
    if ($('cc-scenario')) $('cc-scenario').value = f.scenario || '';
    if ($('cc-first-mes')) $('cc-first-mes').value = f.first_mes || f.firstMes || f.greeting || '';
    $('cc-content').value = c.content || '';
    __pf().editCcRegexScripts = Array.isArray(f.stRegexScripts) ? f.stRegexScripts.slice()
      : (Array.isArray(f.regex_scripts) ? f.regex_scripts.slice() : []);
    clearCcRegexForm();
    renderCcRegexList();
    renderPartnerLists();
  }

  function clearWbForm() {
    __pf().editWbId = '';
    $('wb-name').value = '';
    $('wb-theme').value = '';
    $('wb-era').value = '';
    $('wb-geography').value = '';
    $('wb-conflict').value = '';
    $('wb-content').value = '';
    $('wb-msg').textContent = '';
    __pf().editWbEntries = [];
    clearWbEntryForm();
    renderPartnerLists();
  }

  function clearCcForm() {
    __pf().editCcId = '';
    $('cc-name').value = '';
    $('cc-wb').value = '';
    $('cc-occupation').value = '';
    $('cc-ext').value = '';
    $('cc-speak').value = '';
    $('cc-rel').value = '';
    if ($('cc-description')) $('cc-description').value = '';
    if ($('cc-personality')) $('cc-personality').value = '';
    if ($('cc-scenario')) $('cc-scenario').value = '';
    if ($('cc-first-mes')) $('cc-first-mes').value = '';
    $('cc-content').value = '';
    $('cc-msg').textContent = '';
    __pf().editCcRegexScripts = [];
    clearCcRegexForm();
    renderPartnerLists();
  }

  $('wb-new').onclick = clearWbForm;
  $('cc-new').onclick = clearCcForm;

  // World-book entry / rebuild / card regex / global regex bindings
  if ($('wb-entry-new')) $('wb-entry-new').onclick = () => clearWbEntryForm();
  if ($('wb-entry-reload')) $('wb-entry-reload').onclick = () => loadWbEntries(__pf().editWbId);
  if ($('wbe-save')) $('wbe-save').onclick = () => saveWbEntry();
  if ($('wbe-del')) $('wbe-del').onclick = () => deleteWbEntry();
  if ($('wbe-preview')) $('wbe-preview').onclick = () => previewWbEntry();
  if ($('wb-rebuild')) $('wb-rebuild').onclick = () => rebuildWbBook();

  if ($('cc-regex-new')) $('cc-regex-new').onclick = () => clearCcRegexForm();
  if ($('ccr-save')) $('ccr-save').onclick = () => applyCcRegexLocal();
  if ($('ccr-del')) $('ccr-del').onclick = () => deleteCcRegexLocal();
  if ($('ccr-test')) $('ccr-test').onclick = () => {
    // include current form
    const cur = collectCcRegexForm();
    const scripts = __pf().editCcRegexScripts.slice();
    if (__pf().editCcRegexIdx >= 0) scripts[__pf().editCcRegexIdx] = Object.assign({}, scripts[__pf().editCcRegexIdx], cur);
    else if (cur.findRegex) scripts.push(cur);
    testRegexScripts(scripts, ($('ccr-sample') && $('ccr-sample').value) || '', $('ccr-test-out'), $('ccr-msg'));
  };
  if ($('cc-rebuild-wb')) $('cc-rebuild-wb').onclick = async () => {
    if (!__pf().editCcId) { $('cc-msg').textContent = '先选角色卡'; return; }
    try {
      $('cc-msg').textContent = '重建关联世界书…';
      const r = await api('/api/v1/partner/character-cards/' + encodeURIComponent(__pf().editCcId) + '/rebuild-st-book', {
        method: 'POST',
        body: JSON.stringify({ createFromContent: true }),
      });
      await loadPartner();
      const c = (__c7().partner.characterCards || []).find((x) => x.id === __pf().editCcId);
      if (c) selectCc(c);
      if (c && c.worldBookId) {
        const w = (__c7().partner.worldBooks || []).find((x) => x.id === c.worldBookId);
        if (w) selectWb(w);
      }
      $('cc-msg').textContent = '重建完成 · entries=' + (r.count != null ? r.count : '?');
    } catch (e) {
      $('cc-msg').textContent = e.message;
    }
  };

  if ($('rx-reload')) $('rx-reload').onclick = () => loadRegexLibrary();
  if ($('rx-new')) $('rx-new').onclick = () => clearRxForm();
  if ($('rx-apply-local')) $('rx-apply-local').onclick = () => applyRxLocal();
  if ($('rx-del')) $('rx-del').onclick = () => deleteRxLocal();
  if ($('rx-save-lib')) $('rx-save-lib').onclick = () => saveRegexLibrary();
  if ($('rx-test')) $('rx-test').onclick = () => {
    const cur = collectRxForm();
    const scripts = __pf().editRxScripts.slice();
    if (__pf().editRxIdx >= 0) scripts[__pf().editRxIdx] = Object.assign({}, scripts[__pf().editRxIdx], cur);
    else if (cur.findRegex) scripts.push(cur);
    testRegexScripts(scripts, ($('rx-sample') && $('rx-sample').value) || '', $('rx-test-out'), $('rx-msg'));
  };
  // lazy-load regex library when partner tab first shown — also on boot
  loadRegexLibrary().catch(() => {});



  $('wb-save').onclick = async () => {
    $('wb-msg').textContent = '保存中…';
    try {
      const fields = {
        theme: $('wb-theme').value.trim(),
        era: $('wb-era').value.trim(),
        geography: $('wb-geography').value.trim(),
        conflict: $('wb-conflict').value.trim(),
      };
      const body = {
        id: __pf().editWbId || '',
        name: $('wb-name').value.trim() || '未命名世界',
        type: 'world_book',
        fields,
        content: $('wb-content').value.trim(),
      };
      const item = await api('/api/v1/partner/world-books', {
        method: 'POST',
        body: JSON.stringify(body),
      });
      __pf().editWbId = item.id;
      $('wb-content').value = item.content || '';
      await loadPartner();
      selectWb(item);
      await loadWbEntries(__pf().editWbId);
      $('wb-msg').textContent = '已保存 ' + item.id;
    } catch (e) {
      $('wb-msg').textContent = e.message;
    }
  };

  $('wb-del').onclick = async () => {
    if (!__pf().editWbId) return;
    if (!await showConfirm('删除世界书 ' + __pf().editWbId + '？')) return;
    await api('/api/v1/partner/world-books/' + encodeURIComponent(__pf().editWbId) + '?cascade=false', {
      method: 'DELETE',
    });
    clearWbForm();
    await loadPartner();
  };

  $('cc-save').onclick = async () => {
    $('cc-msg').textContent = '保存中…';
    try {
      // flush regex editor buffer into array before save
      if (($('ccr-find') && $('ccr-find').value.trim()) || __pf().editCcRegexIdx >= 0) {
        try { applyCcRegexLocal(); } catch (_) {}
      }
      const fields = {
        name: $('cc-name').value.trim(),
        occupation: $('cc-occupation').value.trim(),
        externalPersonality: $('cc-ext').value.trim(),
        speakingStyle: $('cc-speak').value.trim(),
        userRelationType: $('cc-rel').value.trim(),
        description: ($('cc-description') && $('cc-description').value.trim()) || '',
        personality: ($('cc-personality') && $('cc-personality').value.trim()) || '',
        scenario: ($('cc-scenario') && $('cc-scenario').value.trim()) || '',
        first_mes: ($('cc-first-mes') && $('cc-first-mes').value.trim()) || '',
        stRegexScripts: __pf().editCcRegexScripts.slice(),
      };
      const body = {
        id: __pf().editCcId || '',
        name: $('cc-name').value.trim() || '未命名角色',
        type: 'character_card',
        worldBookId: $('cc-wb').value || null,
        fields,
        content: $('cc-content').value.trim(),
      };
      const item = await api('/api/v1/partner/character-cards', {
        method: 'POST',
        body: JSON.stringify(body),
      });
      __pf().editCcId = item.id;
      $('cc-content').value = item.content || '';
      await loadPartner();
      selectCc(item);
      $('cc-msg').textContent = '已保存 ' + item.id;
    } catch (e) {
      $('cc-msg').textContent = e.message;
    }
  };

  if ($('cc-st-export')) {
      $('cc-st-export').onclick = async () => {
        try {
          if (!__pf().editCcId) throw new Error('先选中/保存角色卡');
          const data = await api('/api/v1/partner/st-export', {
            method: 'POST',
            body: JSON.stringify({
              kind: 'character_card',
              characterCardId: __pf().editCcId,
              format: 'both',
            }),
          });
          setPanel('cc-out', 'cc-msg', data);
          if (data && data.pngBase64) {
            const a = document.createElement('a');
            a.href = 'data:image/png;base64,' + data.pngBase64;
            a.download = (data.data && data.data.name ? data.data.name : 'character') + '.png';
            a.click();
          }
        } catch (e) {
          setPanel('cc-out', 'cc-msg', null, e);
        }
      };
    }

  $('cc-del').onclick = async () => {
    if (!__pf().editCcId) return;
    if (!await showConfirm('删除角色卡 ' + __pf().editCcId + '？')) return;
    await api('/api/v1/partner/character-cards/' + encodeURIComponent(__pf().editCcId), {
      method: 'DELETE',
    });
    clearCcForm();
    await loadPartner();
  };

  $('apply-partner').onclick = async () => {
    try {
      __c7().partner = await api('/api/v1/partner/select', {
        method: 'POST',
        body: JSON.stringify({
          worldBookId: ($('chat-wb2') && $('chat-wb2').value) || $('chat-wb').value || '',
          characterCardId: ($('chat-cc2') && $('chat-cc2').value) || $('chat-cc').value || '',
        }),
      });
      __ch().refreshPartnerSelects();
      if ($('partner-hint2')) $('partner-hint2').textContent = '已应用选中';
    } catch (e) {
      if ($('partner-hint2')) $('partner-hint2').textContent = e.message;
    }
  };

  $('preview-btn').onclick = async () => {
    const wb = $('chat-wb').value || __c7().partner.selectedWorldBookId || '';
    const cc = $('chat-cc').value || __c7().partner.selectedCharacterCardId || '';
    const q = new URLSearchParams();
    if (wb) q.set('worldBookId', wb);
    if (cc) q.set('characterCardId', cc);
    const res = await api('/api/v1/partner/prompt-preview?' + q.toString());
    $('prompt-preview').textContent = res.systemPrompt || '';
  };


/* S2.8: loadPartner bridge — chat.js (real module) needs it for the
 * sample-pack install flow in setupChatStart(). */
try { window.__kaleidoPartner = { loadPartner: loadPartner }; } catch (_) {}

export { loadPartner };
