/* P1-3 S2.7: _keyboard-part → real ESM (global search, shortcuts, delegation,
 * perf metrics).
 *
 * Dependencies resolved for real:
 *   - switchTab/switchAzView/closeToolsSheet/closeSessionDrawer ← ./tabs_bridge.js
 *     (the S2.5 blocker; the old `typeof switchTab === 'function'` probes are gone)
 *   - openGlobalSearch/closeGlobalSearch ← ./search.js (S2.4)
 *   - api ← ./api.js, showToast ← ./toast.js, $ ← ./dom.js
 *
 * Deviations from the IIFE original (all behavior-neutral):
 *   - doGlobalSearch's tavernPack branch dropped: its if-body was empty
 *     ("Already viewing this pack, no nav needed") — pure dead code, and
 *     tavernPack is closure state a real module cannot see.
 *   - escapeHtml duplicated locally (6-line pure fn, canonical copy lives in
 *     _works-part closure); dedupe when works converts.
 *   - globalSearchTimer declared HERE. Latent-bug fix: after S2.4 removed
 *     _search-part from parts.json, the only `let globalSearchTimer` left the
 *     bundle while this part still assigned it → strict-mode ReferenceError on
 *     the first keystroke in the global search input.
 *
 * Execution order: main.js imports this AFTER 'virtual:app-parts', so listener
 * registration happens last — same relative order as the old 37th-of-38 part.
 */
import { $ } from './dom.js';
import { api } from './api.js';
import { showToast } from './toast.js';
import { openGlobalSearch, closeGlobalSearch } from './search.js';
import { switchTab, switchAzView, closeToolsSheet, closeSessionDrawer } from './tabs_bridge.js';

function escapeHtml(s) {
  return String(s)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;');
}

export function doGlobalSearch(q) {
  const results = $('glob-search-results');
  if (!results) return;
  const trimmed = (q || '').trim().toLowerCase();
  if (!trimmed) {
    results.innerHTML = '';
    return;
  }
  // Collect searchable items from current session list titles & works
  const items = [];

  // From session drawer
  const sessEl = $('session-list');
  if (sessEl) {
    sessEl.querySelectorAll('.item').forEach(function (n) {
      const title = (n.textContent || '').trim();
      if (title.toLowerCase().includes(trimmed)) {
        items.push({ label: '会话', text: title, icon: '📇', fn: () => { n.click(); closeGlobalSearch(); } });
      }
    });
  }

  // From works tree items that are already rendered
  const worksTree = $('works-tree');
  if (worksTree) {
    worksTree.querySelectorAll('.item').forEach(function (n) {
      const t = n.querySelector('.t');
      const d = n.querySelector('.d');
      const title = (t ? t.textContent : n.textContent || '').trim();
      if (title.toLowerCase().includes(trimmed)) {
        const isDir = d && d.textContent && d.textContent.includes('目录');
        items.push({
          label: isDir ? '目录' : '文件',
          text: title,
          icon: isDir ? '📁' : '📄',
          fn: () => { n.click(); closeGlobalSearch(); },
        });
      }
    });
  }

  // Deduplicate by text+label
  const seen = new Set();
  const deduped = [];
  for (const it of items) {
    const key = it.label + ':' + it.text;
    if (!seen.has(key)) { seen.add(key); deduped.push(it); }
  }

  if (!deduped.length) {
    results.innerHTML = '<div class="glob-search-result" data-empty>本地无匹配，检索服务端…</div>';
  } else {
    results.innerHTML = '';
    for (const it of deduped.slice(0, 30)) {
      const row = document.createElement('div');
      row.className = 'glob-search-result';
      row.innerHTML = '<span class="gsr-icon">' + it.icon + '</span><span class="gsr-label">' + it.label + '</span><span class="gsr-text">' + escapeHtml(it.text) + '</span>';
      row.onclick = it.fn;
      results.appendChild(row);
    }
  }

  // P6: hybrid search against backend /api/v1/search (chapters/characters/packs/nodes/lore/foreshadows/outlines)
  api('/api/v1/search?q=' + encodeURIComponent(trimmed) + '&limit=12')
    .then(function (data) {
      const inp = $('glob-search-input');
      if (inp && inp.value.trim().toLowerCase() !== trimmed) return; // stale result
      const KIND = {
        pack:['设定','📚','packs'], chapter:['章节','📄','st'], character:['角色','👤','partner'],
        node:['节点','🕸','partner'], lore:['设定','📖','packs'], outline:['大纲','🗺','outline'],
        foreshadow:['伏笔','🔱','outline'], note:['笔记','📝','works'], work:['作品','🎛','works']
      };
      const hits = (data && data.results) || [];
      if (hits.length) {
        results.innerHTML = '';
        const sep = document.createElement('div');
        sep.className = 'glob-search-sep';
        sep.textContent = '全局检索（' + hits.length + '）';
        results.appendChild(sep);
        hits.forEach(function (h) {
          const meta = KIND[h.kind] || [h.kind || '检索', '🔍', 'works'];
          const row = document.createElement('div');
          row.className = 'glob-search-result';
          row.innerHTML = '<span class="gsr-icon">' + meta[1] + '</span><span class="gsr-label">' + meta[0] + '</span><span class="gsr-text">' +
            (h.workTitle ? escapeHtml(h.workTitle) + ' · ' : '') + escapeHtml(h.title) + '</span>';
          if (h.snippet) {
            const sn = document.createElement('div');
            sn.className = 'gsr-snippet';
            sn.textContent = h.snippet;
            row.appendChild(sn);
          }
          row.onclick = function () { switchTab(meta[2]); closeGlobalSearch(); showToast('检索命中 · ' + (h.workTitle || h.title)); };
          results.appendChild(row);
        });
      } else if (!deduped.length) {
        results.innerHTML = '<div class="glob-search-result">服务端无匹配</div>';
      }
    })
    .catch(function (e) {
      if (!deduped.length) {
        results.innerHTML = '<div class="glob-search-result">服务端检索失败：' + escapeHtml((e && e.message) || e) + '</div>';
      }
    });
}

// Debounced dispatch for the global-search input
let globalSearchTimer = null;

// Wire search input
if ($('glob-search-input')) {
  $('glob-search-input').addEventListener('input', function () {
    if (globalSearchTimer) clearTimeout(globalSearchTimer);
    globalSearchTimer = setTimeout(function () {
      doGlobalSearch($('glob-search-input').value);
    }, 180);
  });
  $('glob-search-input').addEventListener('keydown', function (e) {
    if (e.key === 'Escape') { closeGlobalSearch(); e.preventDefault(); }
    if (e.key === 'Enter') {
      const sel = $('glob-search-results').querySelector('.glob-search-result.selected, .glob-search-result:hover');
      if (sel) { sel.click(); }
      return;
    }
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      const results = $('glob-search-results');
      const items = results.querySelectorAll('.glob-search-result');
      if (!items.length) return;
      let idx = -1;
      for (let i = 0; i < items.length; i++) {
        if (items[i].classList.contains('selected')) { idx = i; break; }
      }
      if (e.key === 'ArrowDown') {
        const next = (idx + 1) % items.length;
        items.forEach(function (n) { n.classList.remove('selected'); });
        items[next].classList.add('selected');
        items[next].scrollIntoView({ block: 'nearest' });
      } else {
        const prev = idx <= 0 ? items.length - 1 : idx - 1;
        items.forEach(function (n) { n.classList.remove('selected'); });
        items[prev].classList.add('selected');
        items[prev].scrollIntoView({ block: 'nearest' });
      }
    }
  });
}
if ($('glob-search-close')) {
  $('glob-search-close').onclick = closeGlobalSearch;
}
if ($('glob-search-overlay')) {
  $('glob-search-overlay').addEventListener('click', function (e) {
    if (e.target === this) closeGlobalSearch();
  });
}

// ===== Keyboard shortcuts =====
document.addEventListener('keydown', (e) => {
  // Ctrl+Enter: send message (chat, story, tavern)
  if (e.ctrlKey && e.key === 'Enter') {
    const sendBtn = $('send-btn');
    if (sendBtn && !sendBtn.disabled && !sendBtn.closest('.hidden')) {
      e.preventDefault();
      sendBtn.click();
      return;
    }
    const stInput = $('st-input');
    if (stInput && document.activeElement === stInput) {
      e.preventDefault();
      $('st-send') && $('st-send').click();
      return;
    }
  }
  // Ctrl+K: open search dialog
  if (e.ctrlKey && e.key === 'k') {
    e.preventDefault();
    openGlobalSearch();
  }
  // Escape: close modals / exit immersive
  if (e.key === 'Escape') {
    const toolsSheet = $('tools-sheet');
    const sessionDrawer = $('session-drawer');
    const searchOverlay = $('glob-search-overlay');
    if (searchOverlay && !searchOverlay.classList.contains('hidden')) {
      e.preventDefault();
      closeGlobalSearch();
      return;
    }
    if (toolsSheet && !toolsSheet.classList.contains('hidden')) {
      e.preventDefault();
      closeToolsSheet();
      return;
    }
    if (sessionDrawer && !sessionDrawer.classList.contains('hidden')) {
      e.preventDefault();
      closeSessionDrawer();
      return;
    }
    if (document.documentElement.getAttribute('data-immersive') === '1') {
      e.preventDefault();
      const back = $('imm-back');
      if (back) back.click();
      return;
    }
  }
});

// P3.2: Keyboard navigation support
document.addEventListener('keydown', (e) => {
  // Escape to close modals/sheets
  if (e.key === 'Escape') {
    const sheets = document.querySelectorAll('.sheet:not(.hidden)');
    sheets.forEach(sheet => sheet.classList.add('hidden'));

    const drawers = document.querySelectorAll('.drawer:not(.hidden)');
    drawers.forEach(drawer => drawer.classList.add('hidden'));
  }

  // Arrow keys for tab navigation
  if (e.key === 'ArrowLeft' || e.key === 'ArrowRight') {
    const activeElement = document.activeElement;
    if (activeElement && activeElement.classList.contains('tab')) {
      const tabs = Array.from(document.querySelectorAll('.tab'));
      const currentIndex = tabs.indexOf(activeElement);

      if (currentIndex !== -1) {
        let nextIndex;
        if (e.key === 'ArrowRight') {
          nextIndex = (currentIndex + 1) % tabs.length;
        } else {
          nextIndex = (currentIndex - 1 + tabs.length) % tabs.length;
        }

        tabs[nextIndex].focus();
        e.preventDefault();
      }
    }
  }

  // Enter/Space for buttons
  if ((e.key === 'Enter' || e.key === ' ') && e.target.getAttribute('role') === 'button') {
    e.target.click();
    e.preventDefault();
  }
});

// P4.2: Event delegation optimization
document.addEventListener('click', (e) => {
  // Delegate clicks for data-goto elements
  const gotoEl = e.target.closest('[data-goto]');
  if (gotoEl) {
    const target = gotoEl.getAttribute('data-goto');
    if (target) switchTab(target);
  }

  // Delegate clicks for data-tab elements
  const tabEl = e.target.closest('[data-tab]');
  if (tabEl) {
    const tab = tabEl.getAttribute('data-tab');
    if (tab) switchTab(tab);
  }

  // Delegate clicks for data-azview elements
  const azviewEl = e.target.closest('[data-azview]');
  if (azviewEl) {
    const view = azviewEl.getAttribute('data-azview');
    if (view) switchAzView(view);
  }

  // A4: term-help buttons — show a friendly definition tooltip
  const termEl = e.target.closest('[data-term-help]');
  if (termEl) {
    const key = termEl.getAttribute('data-term-help');
    const defs = {
      'immersive-narrative': '沉浸叙事 = AI 驱动的多轮互动故事体验，可扮演角色、推进剧情、生成正文。',
      'regex-scripts': '正则脚本 = 用「查找/替换」规则改写 AI 输出文本（如把 *斜体* 转为强调）。随角色卡保存，运行时与全局库合并。',
      'placement': 'placement = 正则的作用域：prompt（发送给 AI 前）、completion（AI 回复后）、both（两者都改）。',
      'placement-cc': 'placement 数字含义：1=用户消息、2=AI 回复、5=世界书条目。逗号分隔可多选。',
      'exits': 'exits = 节点出口。每行「条件|目标节点ID」，当条件满足时跳到目标节点推进剧情。',
    };
    const tip = defs[key];
    if (tip) {
      if (typeof showToast === 'function') showToast(tip);
      else { try { alert(tip); } catch (_) {} }
    }
  }
});

// P4.4: Performance monitoring
const perfMetrics = {
  loadStart: performance.now(),
  domReady: 0,
  fullyLoaded: 0,
  firstPaint: 0
};

document.addEventListener('DOMContentLoaded', () => {
  perfMetrics.domReady = performance.now() - perfMetrics.loadStart;
});

window.addEventListener('load', () => {
  perfMetrics.fullyLoaded = performance.now() - perfMetrics.loadStart;
  // P8 realtime task center startup moved to _jobs-part.js (module-scope safety under esbuild)

  // Log performance metrics in development
  if (location.hostname === 'localhost' || location.hostname === '127.0.0.1') {
    console.log('Performance Metrics:', perfMetrics);
  }
});
