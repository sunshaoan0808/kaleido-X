/**
 * src/js/wand.js — 魔棒工具簇真 ESM 模块（P1-3 S2.12）。
 * 合并 8 个零出边叶片：_compass/_chapter-diary/_style/_assets/_review/
 * _drift/_world/_image（~1877L）。outward 全零 → 无 export 表；
 * 各片顶层挂载副作用（ensureUi/MutationObserver/setInterval 监视、
 * DOMContentLoaded/load 绑定）原样保留为模块顶层语句，import 时执行。
 *
 * 入边：$ dom / api / showToast showConfirm / stApi stStatus stCurrentSession(tavern)
 * anWorkId（_analysis-part 闭包声明）→ window.__kaleidoAnState.workId 门面，
 * typeof 守卫语义由 helper 的 try/catch 兜底等价。
 */
import { $ } from './dom.js';
import { api } from './api.js';
import { showToast } from './toast.js';
import { showConfirm } from './dialog.js';
import { stApi, stStatus, stCurrentSession } from './tavern.js';

/** closure accessor: author-zone work id (canonical let stays in _analysis-part) */
function __anWorkId() {
  try { return window.__kaleidoAnState.workId() || ''; } catch (_) { return ''; }
}

/* ================= _compass-part.js ================= */
/* T2 创作罗盘：全书承诺（author_intent）+ 近期目标（current_focus）编辑区。
 *
 * 挂载点：剧场魔棒菜单（#st-wand-menu .st-wand-grid）注入「罗盘」按钮 →
 * 打开独立弹窗（两个 textarea + 字数计数 + 保存）。
 * 数据走 /api/v1/story-tavern/works/{workId}/compass，workId 复用 __anWorkId()。
 * 不自带 HTML：弹窗与按钮由本文件运行时注入 body / 菜单，避免改 web/index.html。
 */

  function stCompassCss() {
    const css = [
      '#st-compass-modal{position:fixed;inset:0;z-index:990;display:flex;align-items:center;justify-content:center;background:rgba(8,10,18,.62);backdrop-filter:blur(3px);padding:16px}',
      '#st-compass-modal.hidden{display:none}',
      '#st-compass-modal .st-compass-box{width:min(620px,100%);max-height:86vh;overflow:auto;background:var(--bg-card,#161b28);border:1px solid var(--border,#2a3247);border-radius:16px;box-shadow:0 18px 60px rgba(0,0,0,.5);display:flex;flex-direction:column}',
      '#st-compass-modal .st-compass-head{display:flex;align-items:center;gap:10px;padding:14px 18px;border-bottom:1px solid var(--border,#2a3247);position:sticky;top:0;background:inherit;z-index:2}',
      '#st-compass-modal .st-compass-head h3{margin:0;font-size:16px;font-weight:700}',
      '#st-compass-modal .st-compass-close{margin-left:auto;background:none;border:none;color:var(--text-dim,#8b93a7);font-size:20px;cursor:pointer;line-height:1;padding:6px}',
      '#st-compass-modal .st-compass-close:hover{color:var(--text,#e8eaf2)}',
      '#st-compass-modal .st-compass-body{padding:16px 18px 20px;display:flex;flex-direction:column;gap:14px}',
      '#st-compass-modal .st-compass-sec{border:1px solid var(--border,#2a3247);border-radius:12px;padding:12px 14px;background:rgba(255,255,255,.02)}',
      '#st-compass-modal .st-compass-sec label{display:block;font-size:13px;font-weight:700;color:var(--text,#e8eaf2);margin-bottom:6px}',
      '#st-compass-modal .st-compass-sec small{display:block;font-size:12px;color:var(--text-dim,#8b93a7);margin-top:6px;line-height:1.5}',
      '#st-compass-modal .st-compass-sec textarea{width:100%;box-sizing:border-box;min-height:92px;resize:vertical;background:rgba(255,255,255,.04);border:1px solid var(--border,#2a3247);border-radius:10px;color:var(--text,#e8eaf2);font:inherit;font-size:13px;line-height:1.6;padding:9px 10px}',
      '#st-compass-modal .st-compass-sec textarea:focus{outline:none;border-color:#5b7cfa}',
      '#st-compass-modal .st-compass-count{font-size:11px;color:var(--text-dim,#8b93a7);text-align:right;margin-top:4px}',
      '#st-compass-modal .st-compass-count.over{color:#ff8a8a;font-weight:700}',
      '#st-compass-modal .st-compass-acts{display:flex;gap:8px;justify-content:flex-end}',
      '#st-compass-modal .st-compass-acts button{font-size:13px;padding:7px 18px;border-radius:9px;border:1px solid var(--border,#2a3247);cursor:pointer;color:var(--text,#e8eaf2);background:rgba(255,255,255,.04)}',
      '#st-compass-modal .st-compass-acts button.primary{background:#5b7cfa;border-color:#5b7cfa;color:#fff}',
      '#st-compass-modal .st-compass-acts button:hover{filter:brightness(1.08)}',
      '#st-compass-modal .st-compass-err{color:#ff8a8a;font-size:12.5px}',
      'html[data-color-scheme="day"] #st-compass-modal{background:rgba(28,25,20,.4)}',
      'html[data-color-scheme="day"] #st-compass-modal .st-compass-box{background:var(--surface-0);border-color:rgba(28,25,20,.16);box-shadow:0 18px 60px rgba(28,25,20,.18)}',
      'html[data-color-scheme="day"] #st-compass-modal .st-compass-sec{border-color:rgba(28,25,20,.14);background:rgba(28,25,20,.03)}',
      'html[data-color-scheme="day"] #st-compass-modal .st-compass-sec textarea{background:rgba(28,25,20,.03);border-color:rgba(28,25,20,.16);color:var(--text)}',
    ].join('\n');
    let style = document.getElementById('st-compass-css');
    if (!style) {
      style = document.createElement('style');
      style.id = 'st-compass-css';
      document.head.appendChild(style);
    }
    style.textContent = css;
  }

  function stCompassEnsureUi() {
    stCompassCss();
    if (!document.getElementById('st-compass-modal')) {
      const modal = document.createElement('div');
      modal.id = 'st-compass-modal';
      modal.className = 'hidden';
      modal.innerHTML = [
        '<div class="st-compass-box" role="dialog" aria-modal="true" aria-label="创作罗盘">',
        '  <div class="st-compass-head"><h3>创作罗盘</h3><button type="button" class="st-compass-close" aria-label="关闭">✕</button></div>',
        '  <div class="st-compass-body">',
        '    <div class="st-compass-sec">',
        '      <label for="st-compass-intent">全书承诺</label>',
        '      <textarea id="st-compass-intent" rows="4" placeholder="这本书不可违背的方向，例如：主角最终必须活着抵达海岸"></textarea>',
        '      <div class="st-compass-count" id="st-compass-intent-count">0 / 2000</div>',
        '      <small>不随轮次丢失的顶层承诺；在角色与世界观信息之前持续注入上下文。</small>',
        '    </div>',
        '    <div class="st-compass-sec">',
        '      <label for="st-compass-focus">近期目标</label>',
        '      <textarea id="st-compass-focus" rows="4" placeholder="接下来的写作目标，例如：本章让沈棠在码头与旧识重逢"></textarea>',
        '      <div class="st-compass-count" id="st-compass-focus-count">0 / 2000</div>',
        '      <small>近期正在推进的情节目标；留空则本段不注入。</small>',
        '    </div>',
        '    <div class="st-compass-err hidden" id="st-compass-err"></div>',
        '    <div class="st-compass-acts">',
        '      <button type="button" id="st-compass-cancel">取消</button>',
        '      <button type="button" id="st-compass-save" class="primary">保存</button>',
        '    </div>',
        '  </div>',
        '</div>',
      ].join('\n');
      document.body.appendChild(modal);

      const close = () => stCompassClose();
      modal.querySelector('.st-compass-close').onclick = close;
      modal.querySelector('#st-compass-cancel').onclick = close;
      modal.addEventListener('click', (e) => {
        if (e.target === modal) close();
      });
      document.addEventListener('keydown', (e) => {
        if (e.key === 'Escape' && !modal.classList.contains('hidden')) close();
      });
      modal.querySelector('#st-compass-save').onclick = stCompassSave;

      // 字数计数（按 code point 计，与服务端字符上限一致）。
      const bindCount = (taId, countId) => {
        const ta = document.getElementById(taId);
        const count = document.getElementById(countId);
        const update = () => {
          const n = Array.from(ta.value).length;
          count.textContent = n + ' / 2000';
          count.classList.toggle('over', n > 2000);
        };
        ta.addEventListener('input', update);
        update();
      };
      bindCount('st-compass-intent', 'st-compass-intent-count');
      bindCount('st-compass-focus', 'st-compass-focus-count');
    }

    // 魔棒菜单「操作」区注入按钮（幂等）。
    if (!document.getElementById('st-compass-btn')) {
      const menu = document.getElementById('st-wand-menu');
      const grid = menu ? menu.querySelector('.st-wand-grid') : null;
      if (grid) {
        const btn = document.createElement('button');
        btn.type = 'button';
        btn.id = 'st-compass-btn';
        btn.className = 'ghost st-tool-btn';
        btn.title = '创作罗盘（全书承诺 + 近期目标）';
        btn.setAttribute('aria-label', '创作罗盘');
        btn.innerHTML = '<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><circle cx="12" cy="12" r="10"/><path d="M16.2 7.8 14 14l-6.2 2.2L10 10z"/></svg><span class="btn-lab">罗盘</span>';
        btn.onclick = (e) => {
          e.preventDefault();
          // 收起魔棒菜单，避免菜单遮住弹窗。
          const menuEl = document.getElementById('st-wand-menu');
          const wandBtn = document.getElementById('st-wand-btn');
          if (menuEl && !menuEl.classList.contains('hidden')) {
            menuEl.classList.add('hidden');
            if (wandBtn) wandBtn.setAttribute('aria-expanded', 'false');
          }
          stCompassOpen();
        };
        grid.appendChild(btn);
      }
    }
  }

  async function stCompassOpen() {
    const modal = document.getElementById('st-compass-modal');
    if (!modal) return;
    const errEl = document.getElementById('st-compass-err');
    errEl.classList.add('hidden');
    try {
      const r = await api('/api/v1/story-tavern/works/' + encodeURIComponent(__anWorkId()) + '/compass');
      const intent = document.getElementById('st-compass-intent');
      const focus = document.getElementById('st-compass-focus');
      intent.value = r.authorIntent || '';
      focus.value = r.currentFocus || '';
      // 手动触发计数刷新
      intent.dispatchEvent(new Event('input'));
      focus.dispatchEvent(new Event('input'));
      modal.classList.remove('hidden');
      intent.focus();
    } catch (e) {
      errEl.textContent = '加载罗盘失败：' + (e && e.message ? e.message : String(e));
      errEl.classList.remove('hidden');
    }
  }

  function stCompassClose() {
    const modal = document.getElementById('st-compass-modal');
    if (modal) modal.classList.add('hidden');
  }

  async function stCompassSave() {
    const intent = document.getElementById('st-compass-intent');
    const focus = document.getElementById('st-compass-focus');
    const errEl = document.getElementById('st-compass-err');
    errEl.classList.add('hidden');
    const len = (s) => Array.from(s).length;
    if (len(intent.value) > 2000 || len(focus.value) > 2000) {
      errEl.textContent = '内容超长：每项最多 2000 字符';
      errEl.classList.remove('hidden');
      return;
    }
    try {
      await api('/api/v1/story-tavern/works/' + encodeURIComponent(__anWorkId()) + '/compass', {
        method: 'PUT',
        body: JSON.stringify({ authorIntent: intent.value, currentFocus: focus.value }),
      });
      stCompassClose();
      showToast('创作罗盘已保存，下回合起注入上下文', 'success');
    } catch (e) {
      errEl.textContent = '保存失败：' + (e && e.message ? e.message : String(e));
      errEl.classList.remove('hidden');
    }
  }

  try { stCompassEnsureUi(); } catch (e) { /* 环境缺菜单/弹窗依赖时不阻塞其他逻辑 */ }

/* ================= _chapter-diary-part.js ================= */
/* [morphling Wave B3 2026-08-16] 章节剧情摘要账本（吸收自 SillyTavern-BakemonoMemory
 * summary-memory-model）：查看每章自动提炼的「本章剧情进展」，可手动编辑保存
 * （manual_edited 保护：保存后自动提炼不再覆盖）。
 * 挂载点：剧场魔棒菜单（#st-wand-menu .st-wand-grid）注入「章节总结」按钮 → 弹窗。
 * 数据源：GET/PUT /api/v1/story-tavern/sessions/{sid}/chapter-summaries(/ch_id)
 */
(function () {
  let diaryModal = null;

  function stChapterDiaryEnsureUi() {
    // 样式（幂等）
    if (!document.getElementById('st-diary-style')) {
      const style = document.createElement('style');
      style.id = 'st-diary-style';
      style.textContent = [
        '#st-diary-modal{position:fixed;inset:0;z-index:990;display:flex;align-items:center;justify-content:center;background:rgba(8,10,18,.62);backdrop-filter:blur(3px);padding:16px}',
        '#st-diary-modal.hidden{display:none}',
        '#st-diary-modal .st-diary-box{width:min(680px,100%);max-height:86vh;overflow:auto;background:var(--bg-card,#161b28);border:1px solid var(--border,#2a3247);border-radius:16px;box-shadow:0 18px 60px rgba(0,0,0,.5);display:flex;flex-direction:column}',
        '#st-diary-modal .st-diary-head{display:flex;align-items:center;gap:10px;padding:14px 18px;border-bottom:1px solid var(--border,#2a3247);position:sticky;top:0;background:inherit;z-index:2}',
        '#st-diary-modal .st-diary-head h3{margin:0;font-size:16px;font-weight:700}',
        '#st-diary-modal .st-diary-close{margin-left:auto;background:none;border:none;color:var(--text-dim,#8b93a7);font-size:20px;cursor:pointer;line-height:1;padding:6px}',
        '#st-diary-modal .st-diary-close:hover{color:var(--text,#e8eaf2)}',
        '#st-diary-modal .st-diary-body{padding:16px 18px 20px;display:flex;flex-direction:column;gap:12px}',
        '#st-diary-modal .st-diary-note{font-size:12px;color:var(--text-dim,#8b93a7);line-height:1.6;padding:8px 10px;border:1px dashed var(--border,#2a3247);border-radius:10px}',
        '#st-diary-modal .st-diary-card{border:1px solid var(--border,#2a3247);border-radius:12px;padding:12px 14px;background:rgba(255,255,255,.02);display:flex;flex-direction:column;gap:8px}',
        '#st-diary-modal .st-diary-card .st-diary-card-head{display:flex;align-items:center;gap:8px;flex-wrap:wrap}',
        '#st-diary-modal .st-diary-card .st-diary-card-title{font-size:14px;font-weight:700;color:var(--text,#e8eaf2)}',
        '#st-diary-modal .st-diary-card .st-diary-card-tag{font-size:11px;color:var(--text-dim,#8b93a7);background:rgba(255,255,255,.05);padding:2px 8px;border-radius:99px}',
        '#st-diary-modal .st-diary-card .st-diary-card-meta{font-size:11px;color:var(--text-dim,#8b93a7)}',
        '#st-diary-modal .st-diary-card .st-diary-summary{font-size:13px;line-height:1.7;color:var(--text,#e8eaf2);white-space:pre-wrap;word-break:break-word}',
        '#st-diary-modal .st-diary-card .st-diary-empty{font-size:13px;color:var(--text-dim,#8b93a7)}',
        '#st-diary-modal .st-diary-card textarea{width:100%;min-height:120px;resize:vertical;background:rgba(0,0,0,.25);border:1px solid var(--border,#2a3247);border-radius:10px;color:var(--text,#e8eaf2);font-size:13px;line-height:1.7;padding:10px 12px;box-sizing:border-box}',
        '#st-diary-modal .st-diary-card .st-diary-actions{display:flex;gap:8px;justify-content:flex-end}',
        '#st-diary-modal .st-diary-card .st-diary-actions button{padding:6px 14px;border-radius:9px;font-size:13px;cursor:pointer;border:1px solid var(--border,#2a3247);background:rgba(255,255,255,.04);color:var(--text,#e8eaf2)}',
        '#st-diary-modal .st-diary-card .st-diary-actions button.primary{background:var(--accent,#5b8cff);border-color:var(--accent,#5b8cff);color:#fff}',
        '#st-diary-modal .st-diary-err{font-size:12px;color:#ff8080;padding:8px 10px;border:1px solid rgba(255,128,128,.35);border-radius:10px}',
        '#st-diary-modal .st-diary-err.hidden{display:none}',
        '#st-diary-modal .st-diary-loading{font-size:13px;color:var(--text-dim,#8b93a7);text-align:center;padding:24px 0}',
        '#st-diary-modal .st-diary-config{display:flex;align-items:center;gap:10px;flex-wrap:wrap;padding:8px 10px;border:1px solid var(--border,#2a3247);border-radius:10px;background:rgba(255,255,255,.02)}',
        '#st-diary-modal .st-diary-config label{font-size:12px;color:var(--text-dim,#8b93a7);display:flex;align-items:center;gap:6px}',
        '#st-diary-modal .st-diary-config input{width:56px;background:rgba(0,0,0,.25);border:1px solid var(--border,#2a3247);border-radius:8px;color:var(--text,#e8eaf2);font-size:13px;padding:4px 8px;text-align:center}',
        '#st-diary-modal .st-diary-config .st-diary-cfg-save{margin-left:auto;padding:5px 12px;border-radius:8px;font-size:12px;cursor:pointer;border:1px solid var(--accent,#5b8cff);background:var(--accent,#5b8cff);color:#fff}',
      ].join('');
      document.head.appendChild(style);
    }

    // 弹窗骨架（幂等）
    if (!document.getElementById('st-diary-modal')) {
      const modal = document.createElement('div');
      modal.id = 'st-diary-modal';
      modal.className = 'hidden';
      modal.innerHTML =
        '<div class="st-diary-box" role="dialog" aria-label="章节总结">' +
        '<div class="st-diary-head"><h3>章节总结</h3>' +
        '<button type="button" class="st-diary-close" aria-label="关闭">×</button></div>' +
        '<div class="st-diary-body">' +
        '<div class="st-diary-note">每回合正文生成时会顺带输出本章剧情进展（自动总结，零额外消耗）；若模型偶尔漏输出，按下方阈值兜底提炼。</div>' +
        '<div class="st-diary-config">' +
        '<label>兜底回合间隔 <input type="number" id="st-diary-cfg-turns" min="1" max="100" value="10"></label>' +
        '<label>兜底事件数 <input type="number" id="st-diary-cfg-events" min="1" max="200" value="20"></label>' +
        '<button type="button" class="st-diary-cfg-save">保存设置</button>' +
        '</div>' +
        '<div id="st-diary-err" class="st-diary-err hidden"></div>' +
        '<div id="st-diary-list"></div>' +
        '</div></div>';
      document.body.appendChild(modal);
      const closeBtn = modal.querySelector('.st-diary-close');
      closeBtn.addEventListener('click', stChapterDiaryClose);
      modal.addEventListener('click', (e) => {
        if (e.target === modal) stChapterDiaryClose();
      });
      document.addEventListener('keydown', (e) => {
        if (e.key === 'Escape' && !modal.classList.contains('hidden')) stChapterDiaryClose();
      });
      const cfgSave = modal.querySelector('.st-diary-cfg-save');
      cfgSave.addEventListener('click', saveDiaryConfig);
    }

    // 魔棒菜单「操作」区注入按钮（幂等）。
    if (!document.getElementById('st-diary-btn')) {
      const menu = document.getElementById('st-wand-menu');
      const grid = menu ? menu.querySelector('.st-wand-grid') : null;
      if (grid) {
        const btn = document.createElement('button');
        btn.type = 'button';
        btn.id = 'st-diary-btn';
        btn.className = 'ghost st-tool-btn';
        btn.title = '章节总结（查看/编辑每章剧情进展）';
        btn.setAttribute('aria-label', '章节总结');
        btn.innerHTML = '<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M4 19.5A2.5 2.5 0 0 1 6.5 17H20"/><path d="M6.5 2H20v20H6.5A2.5 2.5 0 0 1 4 19.5v-15A2.5 2.5 0 0 1 6.5 2z"/></svg><span class="btn-lab">章节总结</span>';
        btn.onclick = (e) => {
          e.preventDefault();
          const menuEl = document.getElementById('st-wand-menu');
          const wandBtn = document.getElementById('st-wand-btn');
          if (menuEl && !menuEl.classList.contains('hidden')) {
            menuEl.classList.add('hidden');
            if (wandBtn) wandBtn.setAttribute('aria-expanded', 'false');
          }
          stChapterDiaryOpen();
        };
        grid.appendChild(btn);
      }
    }
  }

  function stChapterDiaryClose() {
    const modal = document.getElementById('st-diary-modal');
    if (modal) modal.classList.add('hidden');
  }

  function anSessionId() {
    // 从 hash (#/tavern/session/<id>) 取当前会话
    const m = (location.hash || '').match(/session\/([^/?#]+)/);
    if (m && m[1]) return decodeURIComponent(m[1]);
    if (window.tavernSession && window.tavernSession.sessionId) return window.tavernSession.sessionId;
    return '';
  }

  async function stChapterDiaryOpen() {
    const modal = document.getElementById('st-diary-modal');
    if (!modal) return;
    modal.classList.remove('hidden');
    const listEl = document.getElementById('st-diary-list');
    const errEl = document.getElementById('st-diary-err');
    listEl.innerHTML = '<div class="st-diary-loading">加载中…</div>';
    errEl.classList.add('hidden');
    const sid = anSessionId();
    if (!sid) {
      listEl.innerHTML = '<div class="st-diary-empty">未找到当前会话（请先从首页/档案馆进入剧场）。</div>';
      return;
    }
    try {
      const data = await api('/api/v1/story-tavern/sessions/' + encodeURIComponent(sid) + '/chapter-summaries');
      const diaries = (data && data.chapterDiaries) || [];
      const cfg = (data && data.diaryConfig) || {};
      const turnsEl = document.getElementById('st-diary-cfg-turns');
      const eventsEl = document.getElementById('st-diary-cfg-events');
      if (turnsEl) turnsEl.value = cfg.turnInterval ?? 10;
      if (eventsEl) eventsEl.value = cfg.eventThreshold ?? 20;
      renderDiaryList(listEl, diaries, sid);
    } catch (e) {
      listEl.innerHTML = '';
      errEl.textContent = '加载失败：' + (e && e.message ? e.message : String(e));
      errEl.classList.remove('hidden');
    }
  }

  function renderDiaryList(listEl, diaries, sid) {
    if (!diaries.length) {
      listEl.innerHTML =
        '<div class="st-diary-empty">还没有章节总结。每 10 回合或跨章时系统会自动提炼「本章剧情进展」；继续玩几回合再来看吧。</div>';
      return;
    }
    listEl.innerHTML = '';
    diaries.forEach((d) => {
      const card = document.createElement('div');
      card.className = 'st-diary-card';
      const title = d.title || d.chapterId || '未知章节';
      const metaParts = [];
      if (typeof d.startTurn === 'number' || typeof d.endTurn === 'number') {
        metaParts.push('回合 ' + (d.startTurn ?? '?') + '–' + (d.endTurn ?? '?'));
      }
      if (typeof d.updatedAtTurn === 'number') metaParts.push('更新于 t' + d.updatedAtTurn);
      if (d.manualEdited) metaParts.push('手动编辑');
      const tag = d.manualEdited ? '<span class="st-diary-card-tag">已锁定</span>' : '';
      const metaHtml = metaParts.length
        ? '<div class="st-diary-card-meta">' + metaParts.join(' · ') + '</div>'
        : '';
      const bodyHtml = d.summary
        ? '<div class="st-diary-summary">' + esc(d.summary) + '</div>'
        : '<div class="st-diary-empty">（暂无总结内容）</div>';
      card.innerHTML =
        '<div class="st-diary-card-head"><span class="st-diary-card-title">' + esc(title) + '</span>' + tag + '</div>' +
        metaHtml +
        '<div class="st-diary-card-body">' + bodyHtml + '</div>' +
        '<div class="st-diary-actions"><button type="button" class="st-diary-edit-btn">编辑</button></div>';
      const editBtn = card.querySelector('.st-diary-edit-btn');
      editBtn.addEventListener('click', () => enterEditMode(card, d, sid));
      listEl.appendChild(card);
    });
  }

  function enterEditMode(card, d, sid) {
    const bodyEl = card.querySelector('.st-diary-card-body');
    const actionsEl = card.querySelector('.st-diary-actions');
    const textarea = document.createElement('textarea');
    textarea.value = d.summary || '';
    bodyEl.innerHTML = '';
    bodyEl.appendChild(textarea);
    actionsEl.innerHTML =
      '<button type="button" class="st-diary-save-btn primary">保存</button>' +
      '<button type="button" class="st-diary-cancel-btn">取消</button>';
    const saveBtn = actionsEl.querySelector('.st-diary-save-btn');
    const cancelBtn = actionsEl.querySelector('.st-diary-cancel-btn');
    saveBtn.addEventListener('click', async () => {
      saveBtn.disabled = true;
      saveBtn.textContent = '保存中…';
      try {
        await api('/api/v1/story-tavern/sessions/' + encodeURIComponent(sid) + '/chapter-summaries/' + encodeURIComponent(d.chapterId), {
          method: 'PUT',
          body: JSON.stringify({ summary: textarea.value }),
        });
        d.summary = textarea.value;
        d.manualEdited = true;
        stChapterDiaryRefresh(sid);
        showToast('章节总结已保存（该章不再被自动覆盖）', 'success');
      } catch (e) {
        saveBtn.disabled = false;
        saveBtn.textContent = '保存';
        const errEl = document.getElementById('st-diary-err');
        errEl.textContent = '保存失败：' + (e && e.message ? e.message : String(e));
        errEl.classList.remove('hidden');
      }
    });
    cancelBtn.addEventListener('click', () => stChapterDiaryRefresh(sid));
    textarea.focus();
  }

  async function stChapterDiaryRefresh(sid) {
    const listEl = document.getElementById('st-diary-list');
    const errEl = document.getElementById('st-diary-err');
    errEl.classList.add('hidden');
    try {
      const data = await api('/api/v1/story-tavern/sessions/' + encodeURIComponent(sid) + '/chapter-summaries');
      renderDiaryList(listEl, (data && data.chapterDiaries) || [], sid);
    } catch (e) {
      listEl.innerHTML = '';
      errEl.textContent = '加载失败：' + (e && e.message ? e.message : String(e));
      errEl.classList.remove('hidden');
    }
  }

  function esc(s) {
    return String(s == null ? '' : s)
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;');
  }

  async function saveDiaryConfig() {
    const sid = anSessionId();
    if (!sid) return;
    const turnsEl = document.getElementById('st-diary-cfg-turns');
    const eventsEl = document.getElementById('st-diary-cfg-events');
    const turns = Math.max(1, parseInt(turnsEl.value, 10) || 10);
    const events = Math.max(1, parseInt(eventsEl.value, 10) || 20);
    const errEl = document.getElementById('st-diary-err');
    errEl.classList.add('hidden');
    try {
      // 空 summary + diaryConfig → 只更新配置（后端支持）
      await api('/api/v1/story-tavern/sessions/' + encodeURIComponent(sid) + '/chapter-summaries/_config', {
        method: 'PUT',
        body: JSON.stringify({ diaryConfig: { turnInterval: turns, eventThreshold: events } }),
      });
      showToast('章节总结设置已保存（回合 ' + turns + ' / 事件 ' + events + '）', 'success');
    } catch (e) {
      errEl.textContent = '保存失败：' + (e && e.message ? e.message : String(e));
      errEl.classList.remove('hidden');
    }
  }

  try { stChapterDiaryEnsureUi(); } catch (e) { /* 环境缺菜单/弹窗依赖时不阻塞其他逻辑 */ }
})();

/* ================= _style-part.js ================= */
/* U9 参考库与风格采纳（吞噬 Openwrite O5）：样本入库 → evidence 拆解 → 合成风格指南 → 启停注入。
 *
 * 挂载点：剧场魔棒菜单（#st-wand-menu .st-wand-grid）注入「风格」按钮 → 打开独立弹窗。
 * 弹窗三区：
 *   1. 样本库（列表 + 新增：标题 + 正文 textarea；每条显示 evidence 摘要：句长/修辞/高频词）
 *   2. 合成指南（勾选样本 → 指南名 → 生成；预览 summary）
 *   3. 启停开关（PUT style-guide {enabled}）
 * 数据走 /api/v1/reference-library/*，workId 不依赖（参考库是全局的）。
 * 不自带 HTML：弹窗与按钮由本文件运行时注入 body / 菜单，避免改 web/index.html。
 */

  function stStyleCss() {
    const css = [
      '#st-style-modal{position:fixed;inset:0;z-index:990;display:flex;align-items:center;justify-content:center;background:rgba(8,10,18,.62);backdrop-filter:blur(3px);padding:16px}',
      '#st-style-modal.hidden{display:none}',
      '#st-style-modal .st-style-box{width:min(720px,100%);max-height:88vh;overflow:auto;background:var(--bg-card,#161b28);border:1px solid var(--border,#2a3247);border-radius:16px;box-shadow:0 18px 60px rgba(0,0,0,.5);display:flex;flex-direction:column}',
      '#st-style-modal .st-style-head{display:flex;align-items:center;gap:10px;padding:14px 18px;border-bottom:1px solid var(--border,#2a3247);position:sticky;top:0;background:inherit;z-index:2}',
      '#st-style-modal .st-style-head h3{margin:0;font-size:16px;font-weight:700}',
      '#st-style-modal .st-style-close{margin-left:auto;background:none;border:none;color:var(--text-dim,#8b93a7);font-size:20px;cursor:pointer;line-height:1;padding:6px}',
      '#st-style-modal .st-style-body{padding:16px 18px 20px;display:flex;flex-direction:column;gap:14px}',
      '#st-style-modal .st-style-sec{border:1px solid var(--border,#2a3247);border-radius:12px;padding:12px 14px;background:rgba(255,255,255,.02)}',
      '#st-style-modal .st-style-sec h4{margin:0 0 8px;font-size:13px;font-weight:700;color:var(--text,#e8eaf2)}',
      '#st-style-modal .st-style-sec small{display:block;font-size:12px;color:var(--text-dim,#8b93a7);margin-top:6px;line-height:1.5}',
      '#st-style-modal .st-style-row{display:flex;gap:8px;align-items:center;margin-bottom:8px}',
      '#st-style-modal .st-style-row input[type=text]{flex:1;box-sizing:border-box;background:rgba(255,255,255,.04);border:1px solid var(--border,#2a3247);border-radius:10px;color:var(--text,#e8eaf2);font:inherit;font-size:13px;padding:8px 10px}',
      '#st-style-modal .st-style-row input[type=text]:focus{outline:none;border-color:#5b7cfa}',
      '#st-style-modal textarea{width:100%;box-sizing:border-box;min-height:110px;resize:vertical;background:rgba(255,255,255,.04);border:1px solid var(--border,#2a3247);border-radius:10px;color:var(--text,#e8eaf2);font:inherit;font-size:13px;line-height:1.6;padding:9px 10px}',
      '#st-style-modal textarea:focus{outline:none;border-color:#5b7cfa}',
      '#st-style-modal .st-style-list{display:flex;flex-direction:column;gap:6px;max-height:220px;overflow:auto}',
      '#st-style-modal .st-style-item{display:flex;align-items:center;gap:8px;padding:6px 8px;border:1px solid var(--border,#2a3247);border-radius:8px;background:rgba(255,255,255,.02)}',
      '#st-style-modal .st-style-item .st-style-ev{margin-left:auto;font-size:11px;color:var(--text-dim,#8b93a7);white-space:nowrap;overflow:hidden;text-overflow:ellipsis;max-width:300px}',
      '#st-style-modal .st-style-item button{background:none;border:none;color:#ff8a8a;cursor:pointer;font-size:13px;padding:2px 6px}',
      '#st-style-modal .st-style-preview{white-space:pre-wrap;font-size:12.5px;line-height:1.7;color:var(--text,#e8eaf2);background:rgba(255,255,255,.03);border:1px solid var(--border,#2a3247);border-radius:10px;padding:10px 12px;max-height:220px;overflow:auto}',
      '#st-style-modal .st-style-acts{display:flex;gap:8px;justify-content:flex-end;align-items:center}',
      '#st-style-modal .st-style-acts button{font-size:13px;padding:7px 18px;border-radius:9px;border:1px solid var(--border,#2a3247);cursor:pointer;color:var(--text,#e8eaf2);background:rgba(255,255,255,.04)}',
      '#st-style-modal .st-style-acts button.primary{background:#5b7cfa;border-color:#5b7cfa;color:#fff}',
      '#st-style-modal .st-style-acts button.danger{background:transparent;border-color:rgba(255,138,138,.5);color:#ff8a8a}',
      '#st-style-modal .st-style-acts button:hover{filter:brightness(1.08)}',
      '#st-style-modal .st-style-toggle{display:flex;align-items:center;gap:8px;font-size:13px;color:var(--text,#e8eaf2)}',
      '#st-style-modal .st-style-toggle input{accent-color:#5b7cfa;width:16px;height:16px;cursor:pointer}',
      '#st-style-modal .st-style-err{color:#ff8a8a;font-size:12.5px}',
      '#st-style-modal .st-style-ok{color:#7ee2a8;font-size:12.5px}',
      'html[data-color-scheme="day"] #st-style-modal{background:rgba(28,25,20,.4)}',
      'html[data-color-scheme="day"] #st-style-modal .st-style-box{background:var(--surface-0);border-color:rgba(28,25,20,.16);box-shadow:0 18px 60px rgba(28,25,20,.18)}',
      'html[data-color-scheme="day"] #st-style-modal .st-style-sec{border-color:rgba(28,25,20,.14);background:rgba(28,25,20,.03)}',
      'html[data-color-scheme="day"] #st-style-modal .st-style-sec input[type=text],html[data-color-scheme="day"] #st-style-modal .st-style-sec textarea{background:rgba(28,25,20,.03);border-color:rgba(28,25,20,.16);color:var(--text)}',
      'html[data-color-scheme="day"] #st-style-modal .st-style-preview{background:rgba(28,25,20,.03);border-color:rgba(28,25,20,.16)}',
    ].join('\n');
    let style = document.getElementById('st-style-css');
    if (!style) {
      style = document.createElement('style');
      style.id = 'st-style-css';
      document.head.appendChild(style);
    }
    style.textContent = css;
  }

  /* 从 evidence 生成一行摘要文本 */
  function stStyleEvText(ev) {
    if (!ev || !ev.sentences) return '';
    const s = ev.sentences;
    const r = ev.rhetoric || {};
    const words = (ev.topWords || []).slice(0, 4).map(w => w[0]).join('、');
    return `句均${s.avgLen ? s.avgLen.toFixed(1) : '-'}字 叹${(r.exclamation || 0).toFixed(1)}‰ 问${(r.question || 0).toFixed(1)}‰${words ? ' 高频:' + words : ''}`;
  }

  function stStyleEnsureUi() {
    stStyleCss();
    if (document.getElementById('st-style-modal')) return;
    const modal = document.createElement('div');
    modal.id = 'st-style-modal';
    modal.className = 'hidden';
    modal.innerHTML =
      '<div class="st-style-box" role="dialog" aria-label="风格采纳">' +
      '<div class="st-style-head"><h3>风格采纳</h3>' +
      '<span id="st-style-status" class="st-style-ok" style="font-size:12px"></span>' +
      '<button type="button" class="st-style-close" id="st-style-close" aria-label="关闭">×</button></div>' +
      '<div class="st-style-body">' +
      '<!-- 样本库 -->' +
      '<div class="st-style-sec"><h4>参考样本库</h4>' +
      '<div class="st-style-row"><input type="text" id="st-style-new-title" placeholder="样本标题（如：章节名）" /><button type="button" id="st-style-add-btn">＋ 入库</button></div>' +
      '<textarea id="st-style-new-content" placeholder="粘贴参考片段正文（至少一段；入库时自动拆解句长/修辞/用词 evidence）"></textarea>' +
      '<div class="st-style-list" id="st-style-list"></div>' +
      '<small>evidence 为规则版统计：句均长度、感叹/问号密度（每千字）、高频 2 字词。零 LLM 调用。</small></div>' +
      '<!-- 合成指南 -->' +
      '<div class="st-style-sec"><h4>合成风格指南</h4>' +
      '<div class="st-style-row"><input type="text" id="st-style-guide-name" placeholder="指南名（如：沈棠的雨巷文风）" /><button type="button" id="st-style-generate-btn" class="primary">生成指南</button></div>' +
      '<div class="st-style-preview" id="st-style-preview">未生成指南。勾选上方样本（默认全选）后点「生成指南」。</div>' +
      '<div class="st-style-acts" style="margin-top:8px">' +
      '<label class="st-style-toggle"><input type="checkbox" id="st-style-enabled" /> 注入上下文（启停）</label>' +
      '<button type="button" id="st-style-refresh-btn">刷新</button>' +
      '</div></div>' +
      '<div id="st-style-err" class="st-style-err"></div>' +
      '</div></div>';
    document.body.appendChild(modal);

    $('st-style-close').onclick = () => modal.classList.add('hidden');
    modal.addEventListener('click', (e) => { if (e.target === modal) modal.classList.add('hidden'); });
    $('st-style-add-btn').onclick = () => stStyleAddSample();
    $('st-style-generate-btn').onclick = () => stStyleGenerate();
    $('st-style-refresh-btn').onclick = () => stStyleLoadAll();
    $('st-style-enabled').onchange = () => stStyleSetEnabled($('st-style-enabled').checked);
  }

  function stStyleApi(path, opts) {
    const o = opts || {};
    return api(path, o);
  }

  async function stStyleLoadSamples() {
    const listEl = $('st-style-list');
    if (!listEl) return;
    listEl.innerHTML = '<div class="muted sm">加载中…</div>';
    try {
      const r = await stStyleApi('/api/v1/reference-library/samples');
      const samples = (r && r.samples) || [];
      if (!samples.length) {
        listEl.innerHTML = '<div class="muted sm">样本库为空，粘贴片段入库。</div>';
        return;
      }
      listEl.innerHTML = '';
      for (const s of samples) {
        const item = document.createElement('div');
        item.className = 'st-style-item';
        const cb = document.createElement('input');
        cb.type = 'checkbox';
        cb.className = 'st-style-pick';
        cb.value = s.id;
        cb.checked = true;
        const title = document.createElement('span');
        title.textContent = s.title;
        title.style.cssText = 'font-size:12.5px;color:var(--text,#e8eaf2);max-width:180px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap';
        const ev = document.createElement('span');
        ev.className = 'st-style-ev';
        ev.textContent = stStyleEvText(s.evidence);
        const del = document.createElement('button');
        del.type = 'button';
        del.textContent = '✕';
        del.title = '删除样本';
        del.onclick = async () => {
          if (!await showConfirm('删除样本「' + s.title + '」？')) return;
          try {
            await stStyleApi('/api/v1/reference-library/samples/' + encodeURIComponent(s.id), { method: 'DELETE' });
            stStyleLoadSamples();
          } catch (e) { stStyleErr('删除失败：' + (e.message || e)); }
        };
        item.appendChild(cb);
        item.appendChild(title);
        item.appendChild(ev);
        item.appendChild(del);
        listEl.appendChild(item);
      }
    } catch (e) {
      listEl.innerHTML = '<div class="muted sm">样本加载失败</div>';
      stStyleErr('加载样本失败：' + (e.message || e));
    }
  }

  async function stStyleLoadGuide() {
    try {
      const g = await stStyleApi('/api/v1/reference-library/style-guide');
      if (!g) return;
      const prev = $('st-style-preview');
      if (prev) prev.textContent = g.summary || '未生成指南。';
      const en = $('st-style-enabled');
      if (en) en.checked = !!g.enabled;
      const name = $('st-style-guide-name');
      if (name && g.name) name.value = g.name;
      const st = $('st-style-status');
      if (st) st.textContent = g.enabled ? '● 注入中' : '';
    } catch (e) { /* 忽略 */ }
  }

  async function stStyleAddSample() {
    const title = $('st-style-new-title').value.trim();
    const content = $('st-style-new-content').value.trim();
    if (!title || !content) { stStyleErr('标题和正文都不能为空'); return; }
    stStyleErr('');
    try {
      await stStyleApi('/api/v1/reference-library/samples', {
        method: 'POST',
        body: JSON.stringify({ title, content }),
      });
      $('st-style-new-title').value = '';
      $('st-style-new-content').value = '';
      stStyleLoadSamples();
    } catch (e) { stStyleErr('入库失败：' + (e.message || e)); }
  }

  async function stStyleGenerate() {
    const name = $('st-style-guide-name').value.trim();
    if (!name) { stStyleErr('请填写指南名'); return; }
    const picks = Array.from(document.querySelectorAll('.st-style-pick:checked')).map(c => c.value);
    if (!picks.length) { stStyleErr('至少勾选一个样本'); return; }
    stStyleErr('');
    try {
      const g = await stStyleApi('/api/v1/reference-library/style-guide/generate', {
        method: 'POST',
        body: JSON.stringify({ name, sampleIds: picks }),
      });
      $('st-style-preview').textContent = (g && g.summary) || '生成成功';
      $('st-style-enabled').checked = true;
      const st = $('st-style-status');
      if (st) st.textContent = '● 注入中';
    } catch (e) { stStyleErr('生成失败：' + (e.message || e)); }
  }

  async function stStyleSetEnabled(enabled) {
    try {
      await stStyleApi('/api/v1/reference-library/style-guide', {
        method: 'PUT',
        body: JSON.stringify({ enabled }),
      });
      const st = $('st-style-status');
      if (st) st.textContent = enabled ? '● 注入中' : '';
    } catch (e) { stStyleErr('切换失败：' + (e.message || e)); }
  }

  async function stStyleLoadAll() {
    stStyleErr('');
    stStyleLoadSamples();
    stStyleLoadGuide();
  }

  function stStyleErr(msg) {
    const el = $('st-style-err');
    if (el) el.textContent = msg;
  }

  /* 魔棒菜单注入「风格」按钮 + 打开弹窗 */
  function stStyleMount() {
    stStyleEnsureUi();
    const wandGrid = document.querySelector('#st-wand-menu .st-wand-grid');
    const openBtn = document.getElementById('st-wand-style-btn');
    if (wandGrid && !openBtn) {
      const btn = document.createElement('button');
      btn.type = 'button';
      btn.id = 'st-wand-style-btn';
      // [fix 2026-08-16] 与魔棒其他按钮统一：ghost st-tool-btn + btn-lab 文字
      // （原 st-wand-btn 无类样式且缺文字标签，窄屏下只剩孤图标）
      btn.className = 'ghost st-tool-btn';
      btn.title = '风格采纳：参考样本库 → 生成风格指南 → 注入叙事上下文';
      btn.innerHTML = '<svg aria-hidden="true" xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 20h9"/><path d="M16.5 3.5a2.12 2.12 0 0 1 3 3L7 19l-4 1 1-4Z"/></svg><span class="btn-lab">风格</span>';
      btn.onclick = () => {
        stStyleLoadAll();
        const modal = document.getElementById('st-style-modal');
        if (modal) modal.classList.remove('hidden');
      };
      wandGrid.appendChild(btn);
    }
  }

  /* 冒险/剧场进入时也挂载（幂等） */
  if (typeof window._stStyleMounted === 'undefined') {
    window._stStyleMounted = true;
    if (document.readyState === 'loading') {
      document.addEventListener('DOMContentLoaded', stStyleMount);
    } else {
      stStyleMount();
    }
    document.addEventListener('st:wand-open', stStyleMount);
  }

/* ================= _assets-part.js ================= */
/* 吞噬资产总览（P0 接线）: 情感曲线 / 角色弧 / 关系演化 → 魔棒弹窗展示。
 *
 * 挂载点：剧场魔棒菜单（#st-wand-menu .st-wand-grid）注入「资产」按钮 → 打开独立弹窗。
 * 弹窗三区：
 *   1. 关系演化（ai-novel T4 吞噬）— GET /api/v1/analysis/relation-evolution?work_id=
 *   2. 角色弧（novel2hermes T5 吞噬）— GET /api/v1/analysis/character-arc?work_id=
 *   3. 情感曲线（novel2hermes T5 吞噬）— POST /api/v1/analysis/emotion-curve {pack_id, limit}
 * 全部纯启发式零 LLM（词表/关系图派生），打开即查，数据直连 graph.sqlite / pack 章节正文。
 * 不自带 HTML：弹窗与按钮由本文件运行时注入 body / 菜单，避免改 web/index.html。
 */

  function stAssetsCss() {
    const css = [
      '#st-assets-modal{position:fixed;inset:0;z-index:990;display:flex;align-items:center;justify-content:center;background:rgba(8,10,18,.62);backdrop-filter:blur(3px);padding:16px}',
      '#st-assets-modal.hidden{display:none}',
      '#st-assets-modal .st-assets-box{width:min(760px,100%);max-height:88vh;overflow:auto;background:var(--bg-card,#161b28);border:1px solid var(--border,#2a3247);border-radius:16px;box-shadow:0 18px 60px rgba(0,0,0,.5);display:flex;flex-direction:column}',
      '#st-assets-modal .st-assets-head{display:flex;align-items:center;gap:10px;padding:14px 18px;border-bottom:1px solid var(--border,#2a3247);position:sticky;top:0;background:inherit;z-index:2}',
      '#st-assets-modal .st-assets-head h3{margin:0;font-size:16px;font-weight:700}',
      '#st-assets-modal .st-assets-close{margin-left:auto;background:none;border:none;color:var(--text-dim,#8b93a7);font-size:20px;cursor:pointer;line-height:1;padding:6px}',
      '#st-assets-modal .st-assets-close:hover{color:var(--text,#e8eaf2)}',
      '#st-assets-modal .st-assets-body{padding:16px 18px 20px;display:flex;flex-direction:column;gap:14px}',
      '#st-assets-modal .st-assets-sec{border:1px solid var(--border,#2a3247);border-radius:12px;padding:12px 14px;background:rgba(255,255,255,.02)}',
      '#st-assets-modal .st-assets-sec h4{margin:0 0 8px;font-size:13px;font-weight:700;color:var(--text,#e8eaf2)}',
      '#st-assets-modal .st-assets-sec small{display:block;font-size:12px;color:var(--text-dim,#8b93a7);margin-top:6px;line-height:1.5}',
      '#st-assets-modal .st-assets-list{display:flex;flex-direction:column;gap:6px;max-height:240px;overflow:auto}',
      '#st-assets-modal .st-assets-item{font-size:12.5px;color:var(--text,#e8eaf2);line-height:1.55;padding:5px 8px;border:1px solid var(--border,#2a3247);border-radius:8px;background:rgba(255,255,255,.02)}',
      '#st-assets-modal .st-assets-item .st-assets-tag{display:inline-block;font-size:11px;padding:1px 6px;border-radius:6px;margin-left:6px;background:rgba(91,124,250,.15);color:#9db4ff}',
      '#st-assets-modal .st-assets-item .st-assets-tag.up{background:rgba(126,226,168,.14);color:#7ee2a8}',
      '#st-assets-modal .st-assets-item .st-assets-tag.down{background:rgba(255,138,138,.14);color:#ff8a8a}',
      '#st-assets-modal .st-assets-acts{display:flex;gap:8px;justify-content:flex-end;align-items:center;margin-top:4px}',
      '#st-assets-modal .st-assets-acts button{font-size:13px;padding:7px 18px;border-radius:9px;border:1px solid var(--border,#2a3247);cursor:pointer;color:var(--text,#e8eaf2);background:rgba(255,255,255,.04)}',
      '#st-assets-modal .st-assets-acts button.primary{background:#5b7cfa;border-color:#5b7cfa;color:#fff}',
      '#st-assets-modal .st-assets-err{color:#ff8a8a;font-size:12.5px}',
      '#st-assets-modal .st-assets-empty{color:var(--text-dim,#8b93a7);font-size:12.5px;padding:6px 0}',
      'html[data-color-scheme="day"] #st-assets-modal{background:rgba(28,25,20,.4)}',
      'html[data-color-scheme="day"] #st-assets-modal .st-assets-box{background:var(--surface-0);border-color:rgba(28,25,20,.16);box-shadow:0 18px 60px rgba(28,25,20,.18)}',
      'html[data-color-scheme="day"] #st-assets-modal .st-assets-sec{border-color:rgba(28,25,20,.14);background:rgba(28,25,20,.03)}',
      'html[data-color-scheme="day"] #st-assets-modal .st-assets-item{border-color:rgba(28,25,20,.14);background:rgba(28,25,20,.03)}',
    ].join('\n');
    let style = document.getElementById('st-assets-css');
    if (!style) {
      style = document.createElement('style');
      style.id = 'st-assets-css';
      document.head.appendChild(style);
    }
    style.textContent = css;
  }

  /* 趋势标签：stable/warming/cooling/volatile → 中文色标 */
  function stAssetsTrend(t) {
    if (t === 'warming') return '<span class="st-assets-tag up">升温</span>';
    if (t === 'cooling') return '<span class="st-assets-tag down">降温</span>';
    if (t === 'volatile') return '<span class="st-assets-tag">起伏</span>';
    return '<span class="st-assets-tag">稳定</span>';
  }

  /* 关系演化（T4 ai-novel） */
  async function stAssetsLoadRelations(workId) {
    const el = document.getElementById('st-assets-rel');
    if (!el) return;
    el.innerHTML = '<div class="st-assets-empty">加载中…</div>';
    try {
      const r = await api('/api/v1/analysis/relation-evolution?work_id=' + encodeURIComponent(workId || ''));
      const evos = (r && r.evolutions) || [];
      if (!evos.length) {
        el.innerHTML = '<div class="st-assets-empty">关系图为空（未在作者区确认人物关系）。</div>';
        return;
      }
      el.innerHTML = '';
      for (const e of evos) {
        const last = e.chapters && e.chapters.length ? e.chapters[e.chapters.length - 1] : null;
        const lastCh = last ? ('（最近 ' + last.chapter + '）') : '';
        const item = document.createElement('div');
        item.className = 'st-assets-item';
        item.innerHTML = e.pair[0] + ' ↔ ' + e.pair[1] + ' ' + stAssetsTrend(e.trend) + ' <span class="st-assets-tag">' + (last ? last.relation_type : '') + '</span> ' + lastCh;
        el.appendChild(item);
      }
    } catch (e) {
      el.innerHTML = '<div class="st-assets-empty">关系演化加载失败：' + (e.message || e) + '</div>';
    }
  }

  /* 角色弧（T5 novel2hermes） */
  async function stAssetsLoadArcs(workId) {
    const el = document.getElementById('st-assets-arc');
    if (!el) return;
    el.innerHTML = '<div class="st-assets-empty">加载中…</div>';
    try {
      const r = await api('/api/v1/analysis/character-arc?work_id=' + encodeURIComponent(workId || ''));
      const arcs = (r && r.arcs) || [];
      if (!arcs.length) {
        el.innerHTML = '<div class="st-assets-empty">角色弧为空（需关系图有跨章变化记录）。</div>';
        return;
      }
      el.innerHTML = '';
      for (const a of arcs.slice(0, 8)) {
        const item = document.createElement('div');
        item.className = 'st-assets-item';
        const recent = (a.changes || []).slice(-2).map(c => c.field + ' ' + c.from + '→' + c.to + '（' + c.chapter + '）').join('；');
        item.innerHTML = '<b>' + a.character + '</b> <span class="st-assets-tag">' + (a.arc_type || '') + '</span>：' + (recent || '无跨章变化');
        el.appendChild(item);
      }
    } catch (e) {
      el.innerHTML = '<div class="st-assets-empty">角色弧加载失败：' + (e.message || e) + '</div>';
    }
  }

  /* 情感曲线（T5 novel2hermes） */
  async function stAssetsLoadEmotion(packId) {
    const el = document.getElementById('st-assets-emotion');
    if (!el) return;
    el.innerHTML = '<div class="st-assets-empty">加载中…</div>';
    try {
      if (!packId) {
        el.innerHTML = '<div class="st-assets-empty">未选择 Pack（情感曲线需 pack_id）。</div>';
        return;
      }
      const r = await api('/api/v1/analysis/emotion-curve', {
        method: 'POST',
        body: JSON.stringify({ pack_id: packId, limit: 60 }),
      });
      const curve = (r && r.curve) || {};
      const chapters = (curve.chapters || []).slice(-12);
      if (!chapters.length) {
        el.innerHTML = '<div class="st-assets-empty">情感曲线为空。</div>';
        return;
      }
      const overall = curve.overall_arc ? ('<div class="st-assets-item" style="margin-bottom:6px">整体弧线：' + curve.overall_arc + '</div>') : '';
      const bar = (c) => {
        const w = Math.max(4, Math.min(100, Math.round(c.peak_intensity || 0)));
        return '<div style="display:flex;align-items:center;gap:8px;margin:2px 0"><div style="width:110px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-size:11.5px;color:var(--text-dim,#8b93a7)">' + (c.chapter || '') + '</div><div style="flex:1;height:10px;background:rgba(255,255,255,.06);border-radius:5px"><div style="width:' + w + '%;height:100%;background:#5b7cfa;border-radius:5px"></div></div><div style="width:64px;font-size:11px;color:var(--text,#e8eaf2)">' + (c.peak_intensity || 0) + ' ' + (c.dominant_emotion || '') + '</div></div>';
      };
      el.innerHTML = overall;
      for (const c of chapters) {
        el.insertAdjacentHTML('beforeend', bar(c));
      }
    } catch (e) {
      el.innerHTML = '<div class="st-assets-empty">情感曲线加载失败：' + (e.message || e) + '</div>';
    }
  }

  async function stAssetsLoadAll() {
    const err = document.getElementById('st-assets-err');
    if (err) err.textContent = '';
    // work_id 复用 __anWorkId()（作者区当前选中 work；无则 'default' 兜底）
    let workId = '';
    try { if (typeof anWorkId === 'function') workId = __anWorkId() || ''; } catch (e) { /* 忽略 */ }
    // pack_id 从剧场 pack 选择器拿（魔棒菜单所在会话）
    let packId = '';
    try {
      const sel = document.getElementById('st-wizard-pack');
      if (sel && String(sel.value).trim()) packId = String(sel.value).trim();
    } catch (e) { /* 忽略 */ }
    stAssetsLoadRelations(workId);
    stAssetsLoadArcs(workId);
    stAssetsLoadEmotion(packId);
  }

  function stAssetsEnsureUi() {
    stAssetsCss();
    if (document.getElementById('st-assets-modal')) return;
    const modal = document.createElement('div');
    modal.id = 'st-assets-modal';
    modal.className = 'hidden';
    modal.innerHTML =
      '<div class="st-assets-box" role="dialog" aria-label="吞噬资产总览">' +
      '<div class="st-assets-head"><h3>吞噬资产总览</h3>' +
      '<button type="button" class="st-assets-close" id="st-assets-close" aria-label="关闭">×</button></div>' +
      '<div class="st-assets-body">' +
      '<!-- 关系演化 -->' +
      '<div class="st-assets-sec"><h4>关系演化 <small style="display:inline;margin:0">(ai-novel T4 · 角色图谱)</small></h4>' +
      '<div class="st-assets-list" id="st-assets-rel"></div>' +
      '<small>从 graph.sqlite 关系图派生：每对角色最近章节 + 趋势（升温/降温/稳定/起伏）。零 LLM。</small></div>' +
      '<!-- 角色弧 -->' +
      '<div class="st-assets-sec"><h4>角色弧 <small style="display:inline;margin:0">(novel2hermes T5 · 跨章变化)</small></h4>' +
      '<div class="st-assets-list" id="st-assets-arc"></div>' +
      '<small>关系跨章出现推进 → 字段变化记录（from→to），启发式归类（成长/黑化/回归/稳定）。零 LLM。</small></div>' +
      '<!-- 情感曲线 -->' +
      '<div class="st-assets-sec"><h4>情感曲线 <small style="display:inline;margin:0">(novel2hermes T5 · 逐章峰值)</small></h4>' +
      '<div class="st-assets-list" id="st-assets-emotion"></div>' +
      '<small>词表 + 标点密度启发式：近 12 章峰值强度（0-100）+ 主导情绪 + 整体弧线。零 LLM。</small></div>' +
      '<div id="st-assets-err" class="st-assets-err"></div>' +
      '</div></div>';
    document.body.appendChild(modal);

    document.getElementById('st-assets-close').onclick = () => modal.classList.add('hidden');
    modal.addEventListener('click', (e) => { if (e.target === modal) modal.classList.add('hidden'); });
  }

  /* 魔棒菜单注入「资产」按钮 + 打开弹窗 */
  function stAssetsMount() {
    stAssetsEnsureUi();
    const wandGrid = document.querySelector('#st-wand-menu .st-wand-grid');
    const openBtn = document.getElementById('st-wand-assets-btn');
    if (wandGrid && !openBtn) {
      const btn = document.createElement('button');
      btn.type = 'button';
      btn.id = 'st-wand-assets-btn';
      // [fix 2026-08-16] 与魔棒其他按钮统一：ghost st-tool-btn + btn-lab 文字
      // （原 st-wand-btn 无类样式且缺文字标签，窄屏下只剩孤图标）
      btn.className = 'ghost st-tool-btn';
      btn.title = '吞噬资产：情感曲线 / 角色弧 / 关系演化（启发式零 LLM）';
      btn.innerHTML = '<svg aria-hidden="true" xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><line x1="18" y1="20" x2="18" y2="10"/><line x1="12" y1="20" x2="12" y2="4"/><line x1="6" y1="20" x2="6" y2="14"/></svg><span class="btn-lab">资产</span>';
      btn.onclick = () => {
        stAssetsLoadAll();
        const modal = document.getElementById('st-assets-modal');
        if (modal) modal.classList.remove('hidden');
      };
      wandGrid.appendChild(btn);
    }
  }

  /* 冒险/剧场进入时也挂载（幂等） */
  if (typeof window._stAssetsMounted === 'undefined') {
    window._stAssetsMounted = true;
    if (document.readyState === 'loading') {
      document.addEventListener('DOMContentLoaded', stAssetsMount);
    } else {
      stAssetsMount();
    }
    document.addEventListener('st:wand-open', stAssetsMount);
  }

/* ================= _review-part.js ================= */
/* U4 审稿闭环（T1 创作质量）：15 维审稿 → 问题清单 → 逐条修复复查。
 *
 * 挂载点：剧场魔棒菜单（#st-wand-menu .st-wand-grid）注入「审稿」按钮 →
 * 打开独立弹窗（正文输入 + 触发审稿 + 问题面板 + 逐条复查）。
 * 数据走 /api/v1/story-tavern/works/{workId}/reviews，workId 复用 __anWorkId()。
 * 不自带 HTML：弹窗与按钮由本文件运行时注入 body / 菜单，避免改 web/index.html。
 */

  function stReviewCss() {
    const css = [
      '#st-review-modal{position:fixed;inset:0;z-index:991;display:flex;align-items:center;justify-content:center;background:rgba(8,10,18,.62);backdrop-filter:blur(3px);padding:16px}',
      '#st-review-modal.hidden{display:none}',
      '#st-review-modal .st-review-box{width:min(720px,100%);max-height:88vh;overflow:auto;background:var(--bg-card,#161b28);border:1px solid var(--border,#2a3247);border-radius:16px;box-shadow:0 18px 60px rgba(0,0,0,.5);display:flex;flex-direction:column}',
      '#st-review-modal .st-review-head{display:flex;align-items:center;gap:10px;padding:14px 18px;border-bottom:1px solid var(--border,#2a3247);position:sticky;top:0;background:inherit;z-index:2}',
      '#st-review-modal .st-review-head h3{margin:0;font-size:16px;font-weight:700}',
      '#st-review-modal .st-review-close{margin-left:auto;background:none;border:none;color:var(--text-dim,#8b93a7);font-size:20px;cursor:pointer;line-height:1;padding:6px}',
      '#st-review-modal .st-review-close:hover{color:var(--text,#e8eaf2)}',
      '#st-review-modal .st-review-body{padding:16px 18px 20px;display:flex;flex-direction:column;gap:14px}',
      '#st-review-modal .st-review-sec{border:1px solid var(--border,#2a3247);border-radius:12px;padding:12px 14px;background:rgba(255,255,255,.02)}',
      '#st-review-modal .st-review-sec label{display:block;font-size:13px;font-weight:700;color:var(--text,#e8eaf2);margin-bottom:6px}',
      '#st-review-modal .st-review-sec textarea{width:100%;box-sizing:border-box;min-height:120px;resize:vertical;background:rgba(255,255,255,.04);border:1px solid var(--border,#2a3247);border-radius:10px;color:var(--text,#e8eaf2);font:inherit;font-size:13px;line-height:1.6;padding:9px 10px}',
      '#st-review-modal .st-review-sec textarea:focus{outline:none;border-color:#5b7cfa}',
      '#st-review-modal .st-review-acts{display:flex;gap:8px;justify-content:flex-end;flex-wrap:wrap}',
      '#st-review-modal .st-review-acts button{font-size:13px;padding:7px 18px;border-radius:9px;border:1px solid var(--border,#2a3247);cursor:pointer;color:var(--text,#e8eaf2);background:rgba(255,255,255,.04)}',
      '#st-review-modal .st-review-acts button.primary{background:#5b7cfa;border-color:#5b7cfa;color:#fff}',
      '#st-review-modal .st-review-acts button:disabled{opacity:.5;cursor:not-allowed}',
      '#st-review-modal .st-review-acts button:hover:not(:disabled){filter:brightness(1.08)}',
      '#st-review-modal .st-review-err{color:#ff8a8a;font-size:12.5px}',
      '#st-review-modal .st-review-issue{border:1px solid var(--border,#2a3247);border-radius:10px;padding:10px 12px;background:rgba(255,255,255,.02);display:flex;flex-direction:column;gap:6px}',
      '#st-review-modal .st-review-issue.sev3{border-left:3px solid #ff5f5f}',
      '#st-review-modal .st-review-issue.sev2{border-left:3px solid #ffb454}',
      '#st-review-modal .st-review-issue.sev1{border-left:3px solid #ffd76e}',
      '#st-review-modal .st-review-issue.done{opacity:.55;border-left-color:#5bd77a}',
      '#st-review-modal .st-review-issue-top{display:flex;align-items:center;gap:8px;flex-wrap:wrap}',
      '#st-review-modal .st-review-dim{font-size:12px;font-weight:700;color:#5b7cfa;background:rgba(91,124,250,.12);padding:2px 8px;border-radius:99px}',
      '#st-review-modal .st-review-sev{font-size:11px;font-weight:700;padding:2px 8px;border-radius:99px}',
      '#st-review-modal .st-review-sev.s3{background:#ff5f5f22;color:#ff8a8a}',
      '#st-review-modal .st-review-sev.s2{background:#ffb45422;color:#ffc069}',
      '#st-review-modal .st-review-sev.s1{background:#ffd76e22;color:#ffd76e}',
      '#st-review-modal .st-review-problem{font-size:13px;line-height:1.6;color:var(--text,#e8eaf2)}',
      '#st-review-modal .st-review-quote{font-size:12px;color:var(--text-dim,#8b93a7);border-left:2px solid var(--border,#2a3247);padding-left:8px;line-height:1.5;word-break:break-all}',
      '#st-review-modal .st-review-fix{font-size:12.5px;color:#b7c3e8;line-height:1.5}',
      '#st-review-modal .st-review-empty{font-size:13px;color:var(--text-dim,#8b93a7);text-align:center;padding:18px 0}',
      '#st-review-modal .st-review-hist{font-size:12px;color:var(--text-dim,#8b93a7);line-height:1.5}',
      '#st-review-modal .st-review-hist b{color:var(--text,#e8eaf2)}',
      'html[data-color-scheme="day"] #st-review-modal{background:rgba(28,25,20,.4)}',
      'html[data-color-scheme="day"] #st-review-modal .st-review-box{background:var(--surface-0);border-color:rgba(28,25,20,.16);box-shadow:0 18px 60px rgba(28,25,20,.18)}',
      'html[data-color-scheme="day"] #st-review-modal .st-review-sec{border-color:rgba(28,25,20,.14);background:rgba(28,25,20,.03)}',
      'html[data-color-scheme="day"] #st-review-modal .st-review-sec textarea{background:rgba(28,25,20,.03);border-color:rgba(28,25,20,.16);color:var(--text)}',
    ].join('\n');
    let style = document.getElementById('st-review-css');
    if (!style) {
      style = document.createElement('style');
      style.id = 'st-review-css';
      document.head.appendChild(style);
    }
    style.textContent = css;
  }

  function stReviewEnsureUi() {
    stReviewCss();
    if (!document.getElementById('st-review-modal')) {
      const modal = document.createElement('div');
      modal.id = 'st-review-modal';
      modal.className = 'hidden';
      modal.innerHTML = [
        '<div class="st-review-box" role="dialog" aria-modal="true" aria-label="审稿闭环">',
        '  <div class="st-review-head"><h3>📝 审稿闭环</h3><button type="button" class="st-review-close" aria-label="关闭">✕</button></div>',
        '  <div class="st-review-body">',
        '    <div class="st-review-sec">',
        '      <label for="st-review-target">审稿对象（章节/片段名）</label>',
        '      <input id="st-review-target" type="text" style="width:100%;box-sizing:border-box;background:rgba(255,255,255,.04);border:1px solid var(--border,#2a3247);border-radius:10px;color:var(--text,#e8eaf2);font:inherit;font-size:13px;padding:8px 10px" placeholder="例如：第3章 雨夜">',
        '    </div>',
        '    <div class="st-review-sec">',
        '      <label for="st-review-content">正文（粘贴待审稿文本，或先复制章节正文）</label>',
        '      <textarea id="st-review-content" rows="10" placeholder="把当前章节正文粘贴到这里，点击「开始审稿」，15 个维度逐条检查…"></textarea>',
        '    </div>',
        '    <div class="st-review-acts">',
        '      <button type="button" id="st-review-history-btn">历史</button>',
        '      <button type="button" id="st-review-postcheck-btn">⚡ 规则检查</button>',
        '      <button type="button" id="st-review-postrefine-btn">✨ 精修建议</button>',
        '      <button type="button" id="st-review-run-btn" class="primary">开始审稿</button>',
        '    </div>',
        '    <div class="st-review-err hidden" id="st-review-err"></div>',
        '    <div class="st-review-hist" id="st-review-hist"></div>',
        '    <div id="st-review-issues"></div>',
        '  </div>',
        '</div>',
      ].join('\n');
      document.body.appendChild(modal);
      modal.querySelector('.st-review-close').addEventListener('click', () => {
        modal.classList.add('hidden');
      });
      modal.addEventListener('click', (e) => {
        if (e.target === modal) modal.classList.add('hidden');
      });
      document.getElementById('st-review-run-btn').addEventListener('click', stReviewRun);
      document.getElementById('st-review-history-btn').addEventListener('click', stReviewHistory);
      document.getElementById('st-review-postcheck-btn').addEventListener('click', stReviewPostCheck);
      document.getElementById('st-review-postrefine-btn').addEventListener('click', stReviewPostRefine);
    }
  }

  function stReviewEscapeHtml(s) {
    return String(s == null ? '' : s).replace(/[&<>"']/g, (c) => {
      return { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c];
    });
  }

  function stReviewSevCls(s) { return s >= 3 ? 'sev3 s3' : (s === 2 ? 'sev2 s2' : 'sev1 s1'); }

  async function stReviewRun() {
    const errEl = document.getElementById('st-review-err');
    const targetEl = document.getElementById('st-review-target');
    const contentEl = document.getElementById('st-review-content');
    const btn = document.getElementById('st-review-run-btn');
    errEl.classList.add('hidden');
    const content = contentEl.value.trim();
    if (!content) {
      errEl.textContent = '请先粘贴正文';
      errEl.classList.remove('hidden');
      return;
    }
    btn.disabled = true;
    btn.textContent = '审稿中…';
    try {
      const r = await api('/api/v1/story-tavern/works/' + encodeURIComponent(__anWorkId()) + '/reviews', {
        method: 'POST',
        body: JSON.stringify({ target: targetEl.value.trim() || '未命名章节', content }),
      });
      if (r && r.run) {
        stReviewRender(r.run, true);
      } else if (r && Array.isArray(r.issues)) {
        stReviewRender({ id: 'temp', target: targetEl.value.trim() || '', created_at: 0, issues: r.issues }, false);
      }
    } catch (e) {
      errEl.textContent = '审稿失败：' + (e && e.message ? e.message : String(e));
      errEl.classList.remove('hidden');
    } finally {
      btn.disabled = false;
      btn.textContent = '开始审稿';
    }
  }

  function stReviewRender(run, persist) {
    const issuesEl = document.getElementById('st-review-issues');
    const histEl = document.getElementById('st-review-hist');
    if (persist) {
      histEl.innerHTML = '本次审稿已存档：<b>' + stReviewEscapeHtml(run.id) + '</b> · 对象「' +
        stReviewEscapeHtml(run.target) + '」 · 发现 <b>' + run.issues.length + '</b> 条问题';
    } else {
      histEl.innerHTML = '';
    }
    if (!run.issues || !run.issues.length) {
      issuesEl.innerHTML = '<div class="st-review-empty">✅ 未发现问题（或 LLM 输出为空）</div>';
      return;
    }
    issuesEl.innerHTML = '';
    run.issues.forEach((iss, i) => {
      const done = iss.status === 'fixed' || iss.status === 'accepted';
      const box = document.createElement('div');
      box.className = 'st-review-issue ' + (done ? 'done' : stReviewSevCls(iss.severity));
      const sevTxt = iss.severity >= 3 ? '严重' : (iss.severity === 2 ? '中等' : '建议');
      box.innerHTML = [
        '<div class="st-review-issue-top">',
        '  <span class="st-review-dim">' + stReviewEscapeHtml(iss.dimension || '未分类') + '</span>',
        '  <span class="st-review-sev ' + stReviewSevCls(iss.severity).split(' ')[1] + '">' + sevTxt + '</span>',
        '  <span style="font-size:11px;color:var(--text-dim,#8b93a7)">' + (done ? '✅ ' + iss.status : '待处理') + '</span>',
        '</div>',
        iss.quote ? '<div class="st-review-quote">「' + stReviewEscapeHtml(iss.quote) + '」</div>' : '',
        '<div class="st-review-problem">' + stReviewEscapeHtml(iss.problem) + '</div>',
        iss.fix_instruction ? '<div class="st-review-fix">🔧 ' + stReviewEscapeHtml(iss.fix_instruction) + '</div>' : '',
        done ? '' : '<div class="st-review-acts"><button type="button" class="primary st-review-fix-btn" data-idx="' + i + '">修复复查</button></div>',
      ].join('\n');
      if (!done) {
        const fixBtn = box.querySelector('.st-review-fix-btn');
        fixBtn.addEventListener('click', () => stReviewFix(run.id, i, fixBtn));
      }
      issuesEl.appendChild(box);
    });
  }

  async function stReviewFix(runId, idx, btn) {
    const errEl = document.getElementById('st-review-err');
    const contentEl = document.getElementById('st-review-content');
    errEl.classList.add('hidden');
    const revised = contentEl.value.trim();
    if (!revised) {
      errEl.textContent = '请先在「正文」输入框粘贴修复后的全文，再点修复复查';
      errEl.classList.remove('hidden');
      return;
    }
    btn.disabled = true;
    btn.textContent = '复查中…';
    try {
      const r = await api('/api/v1/story-tavern/works/' + encodeURIComponent(__anWorkId()) +
        '/reviews/' + encodeURIComponent(runId) + '/issues/' + idx + '/fix', {
        method: 'POST',
        body: JSON.stringify({ content: revised }),
      });
      if (r && r.run) {
        stReviewRender(r.run, false);
      }
      errEl.classList.remove('hidden');
      errEl.textContent = r && r.resolved ? '✅ 该问题已确认解决，状态已更新为 fixed' : '⚠️ 复查认为仍未解决，请继续修改（状态保持 open）';
      errEl.style.color = r && r.resolved ? '#5bd77a' : '#ffb454';
    } catch (e) {
      errEl.style.color = '';
      errEl.textContent = '修复复查失败：' + (e && e.message ? e.message : String(e));
      errEl.classList.remove('hidden');
    }
  }

  async function stReviewHistory() {
    const errEl = document.getElementById('st-review-err');
    const histEl = document.getElementById('st-review-hist');
    errEl.classList.add('hidden');
    try {
      const h = await api('/api/v1/story-tavern/works/' + encodeURIComponent(__anWorkId()) + '/reviews');
      if (!h || !h.runs || !h.runs.length) {
        histEl.innerHTML = '暂无审稿历史';
        return;
      }
      histEl.innerHTML = '📚 审稿历史（新→旧）：<br>' + h.runs.map((run) => {
        const open = run.issues ? run.issues.filter((i) => i.status === 'open').length : 0;
        const fixed = run.issues ? run.issues.filter((i) => i.status === 'fixed').length : 0;
        return '· <b>' + stReviewEscapeHtml(run.id) + '</b> 对象「' + stReviewEscapeHtml(run.target) +
          '」共 ' + (run.issues ? run.issues.length : 0) + ' 条（待处理 ' + open + ' / 已修复 ' + fixed + '）';
      }).join('<br>');
    } catch (e) {
      errEl.textContent = '加载历史失败：' + (e && e.message ? e.message : String(e));
      errEl.classList.remove('hidden');
    }
  }

  /* U5 后置规则检查：纯规则引擎（违禁词/AI痕迹/超长句/重复词/标点滥用），
   * 立即返回结构化问题（不经过 LLM），并入审稿视图问题面板。 */
  async function stReviewPostCheck() {
    const errEl = document.getElementById('st-review-err');
    const contentEl = document.getElementById('st-review-content');
    const btn = document.getElementById('st-review-postcheck-btn');
    errEl.classList.add('hidden');
    const content = contentEl.value.trim();
    if (!content) {
      errEl.textContent = '请先粘贴正文';
      errEl.classList.remove('hidden');
      return;
    }
    btn.disabled = true;
    btn.textContent = '规则检查中…';
    try {
      const r = await api('/api/v1/story-tavern/works/' + encodeURIComponent(__anWorkId()) +
        '/reviews/check/post-check', { method: 'POST', body: JSON.stringify({ target: '', content }) });
      if (r && r.issues !== undefined) {
        stRenderRuleIssues(r.issues || []);
      } else {
        stRenderRuleIssues([]);
      }
    } catch (e) {
      errEl.textContent = '规则检查失败：' + (e && e.message ? e.message : String(e));
      errEl.classList.remove('hidden');
    } finally {
      btn.disabled = false;
      btn.textContent = '⚡ 规则检查';
    }
  }

  /* U5 精修联动：高严重度规则违例 → LLM 逐条改写建议（后端软失败时退回纯规则）。 */
  async function stReviewPostRefine() {
    const errEl = document.getElementById('st-review-err');
    const contentEl = document.getElementById('st-review-content');
    const btn = document.getElementById('st-review-postrefine-btn');
    errEl.classList.add('hidden');
    const content = contentEl.value.trim();
    if (!content) {
      errEl.textContent = '请先粘贴正文';
      errEl.classList.remove('hidden');
      return;
    }
    btn.disabled = true;
    btn.textContent = '精修分析中…';
    try {
      const r = await api('/api/v1/story-tavern/works/' + encodeURIComponent(__anWorkId()) +
        '/reviews/check/post-refine', {
          method: 'POST',
          body: JSON.stringify({ content, min_severity: 2, max_issues: 6 })
        });
      const refined = (r && r.refined) || [];
      const histEl = document.getElementById('st-review-hist');
      const issuesEl = document.getElementById('st-review-issues');
      if (!refined.length) {
        stRenderRuleIssues([]);
        histEl.innerHTML = '✨ 精修：当前正文无高严重度规则违例，无需改写';
        return;
      }
      const withRewrite = refined.filter((rf) => rf.rewritten).length;
      histEl.innerHTML = '✨ 精修建议 <b>' + refined.length + '</b> 条' +
        (r && r.llm_used
          ? '（LLM 改写 ' + withRewrite + ' 条）'
          : '（LLM 失败，仅规则建议——可先「规则检查」确认）');
      issuesEl.innerHTML = '';
      refined.forEach((rf) => {
        const box = document.createElement('div');
        box.className = 'st-review-issue ' + stReviewSevCls(rf.severity);
        const rewrite = rf.rewritten ? rf.rewritten : '（无改写，建议按下方规则建议修复）';
        const seg = [
          '<div class="st-review-issue-top">',
          '  <span class="st-review-dim">' + stReviewEscapeHtml(rf.rule || '未分类') + '</span>',
          '  <span style="font-size:11px;color:var(--text-dim,#8b93a7)">第 ' + (rf.line || 0) + ' 行</span>',
          '</div>',
          rf.quote ? '<div class="st-review-quote">「' + stReviewEscapeHtml(rf.quote) + '」</div>' : '',
          '<div class="st-review-problem">→ ' + stReviewEscapeHtml(rewrite) + '</div>',
          rf.fix ? '<div class="st-review-fix">规则建议：' + stReviewEscapeHtml(rf.fix) + '</div>' : '',
        ];
        box.innerHTML = seg.join('\n');
        issuesEl.appendChild(box);
      });
    } catch (e) {
      errEl.textContent = '精修分析失败：' + (e && e.message ? e.message : String(e));
      errEl.classList.remove('hidden');
    } finally {
      btn.disabled = false;
      btn.textContent = '✨ 精修建议';
    }
  }

  /* 渲染规则检查结果——复用审稿面板样式：rule 当维度、fix 当修复建议、line 显示行号。 */
  function stRenderRuleIssues(issues) {
    const issuesEl = document.getElementById('st-review-issues');
    const histEl = document.getElementById('st-review-hist');
    if (!issues || !issues.length) {
      issuesEl.innerHTML = '<div class="st-review-empty">✅ 规则检查未发现问题</div>';
      histEl.innerHTML = '';
      return;
    }
    histEl.innerHTML = '⚡ 规则检查发现 <b>' + issues.length + '</b> 条硬性问题（非 LLM，纯规则）';
    issuesEl.innerHTML = '';
    issues.forEach((iss) => {
      const done = false;
      const box = document.createElement('div');
      box.className = 'st-review-issue ' + stReviewSevCls(iss.severity);
      const sevTxt = iss.severity >= 3 ? '严重' : (iss.severity === 2 ? '中等' : '建议');
      box.innerHTML = [
        '<div class="st-review-issue-top">',
        '  <span class="st-review-dim">' + stReviewEscapeHtml(iss.rule || '未分类') + '</span>',
        '  <span class="st-review-sev ' + stReviewSevCls(iss.severity).split(' ')[1] + '">' + sevTxt + '</span>',
        '  <span style="font-size:11px;color:var(--text-dim,#8b93a7)">第 ' + (iss.line || 0) + ' 行</span>',
        '</div>',
        iss.quote ? '<div class="st-review-quote">「' + stReviewEscapeHtml(iss.quote) + '」</div>' : '',
        '<div class="st-review-problem">' + stReviewEscapeHtml(iss.fix || '') + '</div>',
      ].join('\n');
      issuesEl.appendChild(box);
    });
  }

  function stReviewOpen() {
    stReviewEnsureUi();
    const modal = document.getElementById('st-review-modal');
    if (!modal) return;
    modal.classList.remove('hidden');
    document.getElementById('st-review-target').focus();
  }

  try {
    stReviewEnsureUi();
    const inject = () => {
      const menu = document.querySelector('#st-wand-menu .st-wand-grid');
      if (!menu) return;
      if (document.getElementById('st-wand-review-btn')) return;
      const btn = document.createElement('button');
      btn.id = 'st-wand-review-btn';
      btn.type = 'button';
      // [fix 2026-08-16] 与魔棒其他按钮统一：ghost st-tool-btn + lucide SVG + btn-lab
      // （原无类名 + 📝 emoji 文本，裸按钮且与全站线条图标割裂）
      btn.className = 'ghost st-tool-btn';
      btn.innerHTML = '<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M16 4h2a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2h2"/><rect width="8" height="4" x="8" y="2" rx="1" ry="1"/><path d="m9 14 2 2 4-4"/></svg><span class="btn-lab">审稿</span>';
      btn.title = '15 维审稿闭环';
      btn.addEventListener('click', stReviewOpen);
      menu.appendChild(btn);
    };
    inject();
    const mo = new MutationObserver(inject);
    mo.observe(document.body, { childList: true, subtree: true });
    setTimeout(inject, 1200);
  } catch (e) { /* 环境缺菜单/弹窗依赖时不阻塞其他逻辑 */ }

/* ================= _drift-part.js ================= */
/* U6 对话质量（T1 创作质量 · 第三优先）：对白行标红漂移分。
 *
 * 挂载点：剧场魔棒菜单（#st-wand-menu .st-wand-grid）注入「🎭 对话质量」按钮 →
 * 对当前会话全部 NPC 对白逐条调 POST /packs/{packId}/dialogue-drift（角色归因用
 * focusCharacterId，缺失时用 pack 第一个可检测角色），漂移分 > 0.35 的气泡加
 * .st-u6-drift 红框 + 行尾漂移分徽标。RE 可幂等重跑（先清旧标记）。
 * 数据不走 LLM：纯规则引擎（dialogue_fingerprint），按钮本身即时返回。
 */

  function stU6DriftCss() {
    const css = [
      '.bubble.st-u6-drift{border:1.5px solid rgba(255,95,95,.85)!important;box-shadow:0 0 0 1px rgba(255,95,95,.28),0 3px 14px rgba(255,95,95,.14);background:rgba(255,80,80,.07)!important}',
      '.bubble.st-u6-drift .role{color:#ff8b8b!important}',
      '.st-u6-badge{display:inline-flex;align-items:center;gap:3px;margin-left:8px;padding:1px 8px;border-radius:99px;font-size:11px;font-weight:800;color:#ffd3d3;background:rgba(255,95,95,.22);border:1px solid rgba(255,95,95,.45);vertical-align:middle;white-space:nowrap}',
      '.st-u6-badge.hot{background:rgba(255,60,60,.32);border-color:rgba(255,60,60,.65);color:#fff;animation:stU6Pulse 1.6s ease-in-out infinite}',
      '@keyframes stU6Pulse{0%,100%{box-shadow:0 0 0 0 rgba(255,80,80,.35)}50%{box-shadow:0 0 0 4px rgba(255,80,80,0)}}',
      'html[data-color-scheme="day"] .bubble.st-u6-drift{background:rgba(200,32,32,.06)!important;border-color:rgba(200,32,32,.55)!important}',
      'html[data-color-scheme="day"] .st-u6-badge{color:#8f1b1b;background:rgba(220,60,60,.14);border-color:rgba(180,40,40,.4)}',
    ].join('\n');
    let style = document.getElementById('st-u6-drift-css');
    if (!style) {
      style = document.createElement('style');
      style.id = 'st-u6-drift-css';
      document.head.appendChild(style);
    }
    style.textContent = css;
  }

  function stU6DriftBtn() {
    stU6DriftCss();
    let btn = document.getElementById('st-u6-drift-btn');
    if (!btn) {
      const menu = document.querySelector('#st-wand-menu .st-wand-grid');
      if (!menu) return;
      btn = document.createElement('button');
      btn.type = 'button';
      btn.id = 'st-u6-drift-btn';
      // [fix 2026-08-15] 与魔棒其他按钮统一：ghost st-tool-btn + lucide SVG +
      // btn-lab 文字（原 🎭 emoji + st-wand-item 混排，窄屏无标签且视觉与立绘混淆）
      btn.className = 'ghost st-tool-btn';
      btn.innerHTML =
        '<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M3 7V5a2 2 0 0 1 2-2h2"/><path d="M17 3h2a2 2 0 0 1 2 2v2"/><path d="M21 17v2a2 2 0 0 1-2 2h-2"/><path d="M7 21H5a2 2 0 0 1-2-2v-2"/><circle cx="12" cy="12" r="3"/><path d="m14.5 14.5 2.5 2.5"/></svg>' +
        '<span class="btn-lab">对话质量</span>';
      btn.title = '对当前会话 NPC 对白逐条检测风格漂移，漂移分 > 0.35 标红';
      btn.addEventListener('click', stU6DriftRun);
      menu.appendChild(btn);
    }
    return btn;
  }

  async function stU6DriftRun() {
    const tavCur = (typeof stCurrentSession === 'function' ? stCurrentSession() : null);
    if (!tavCur) { stStatus('先进入一场叙事再检测对话质量'); return; }
    const packId = tavCur.packId;
    if (!packId) { stStatus('会话缺少 packId，无法检测'); return; }
    const msgs = (tavCur.messages || []);
    const targets = msgs.filter(function (m) {
      return m && m.role && m.role !== 'user' && String(m.content || '').trim();
    });
    if (!targets.length) { stStatus('暂无对白可检测'); return; }

    // 清旧标记（幂等重跑）
    document.querySelectorAll('.bubble.st-u6-drift').forEach(function (el) {
      el.classList.remove('st-u6-drift');
      const badge = el.querySelector(':scope .st-u6-badge');
      if (badge) badge.remove();
    });

    let characterId = tavCur.focusCharacterId || '';
    try {
      if (!characterId) {
        const fp = await stApi('/packs/' + encodeURIComponent(packId) + '/dialogue-fingerprints', { method: 'POST' });
        const chars = (fp && fp.characters) || [];
        if (chars.length) characterId = chars[0].characterId;
      }
    } catch (e) { /* fall through: 无角色也继续，后端会 400 但单个失败不中断 */ }

    stStatus('对话质量检测中 (' + targets.length + ' 条对白)…');
    let red = 0, done = 0;
    for (let i = 0; i < targets.length; i++) {
      const m = targets[i];
      const mid = m.id || ('st-idx-' + i);
      const bubble = document.querySelector('.bubble[data-mid="' + (window.CSS && CSS.escape ? CSS.escape(String(mid)) : String(mid).replace(/"/g, '\\"')) + '"]');
      if (!bubble || !characterId) continue;
      try {
        const r = await stApi('/packs/' + encodeURIComponent(packId) + '/dialogue-drift', {
          method: 'POST',
          body: JSON.stringify({ characterId: characterId, content: String(m.content || '') })
        });
        const score = (r && r.drift && typeof r.drift.driftScore === 'number') ? r.drift.driftScore : 0;
        if (score > 0.35) {
          red++;
          const badge = document.createElement('span');
          badge.className = 'st-u6-badge' + (score > 0.6 ? ' hot' : '');
          badge.textContent = '⚠ 漂移 ' + score.toFixed(2);
          badge.title = (r && r.drift && r.drift.reasons && r.drift.reasons.length)
            ? ('漂移原因：' + r.drift.reasons.join('；')) : '风格漂移超过 0.35';
          bubble.classList.add('st-u6-drift');
          bubble.querySelector(':scope .bubble-body').appendChild(badge);
        }
      } catch (e) { /* 单条失败跳过 */ }
      done++;
      if (done % 3 === 0) stStatus('对话质量 ' + done + '/' + targets.length + '…');
    }
    stStatus(red ? ('检测完成：' + targets.length + ' 条对白，' + red + ' 条漂移超标已标红') : '检测完成：全部对白风格稳定 👍');
  }

  // 魔棒栏渲染后注入按钮（无侵入：不存在则不注入）
  function stU6DriftWire() {
    if (!document.getElementById('st-u6-drift-btn')) stU6DriftBtn();
  }
  (function initDriftWatch() {
    const iv = setInterval(function () {
      if (document.querySelector('#st-wand-menu')) {
        stU6DriftBtn();
        clearInterval(iv);
      }
    }, 1200);
    // 后续每次魔棒菜单重建也补一次（安全兜底）
    setTimeout(function () {
      setInterval(function () { if (document.querySelector('#st-wand-menu')) stU6DriftBtn(); }, 5000);
    }, 8000);
  })();

/* ================= _world-part.js ================= */
/* T2 世界认知（吞噬 Openwrite world_query / truth_manager）：
 * 挂载点：魔棒菜单（#st-wand-menu .st-wand-grid）注入「🌐 世界图谱」「📖 真相账本」两个按钮。
 * 面板数据全部来自会话级 TavernSession.world（create_from_pack 播种 Character 实体；
 * 叙事变更由编排器经 POST /world/events 追加）。本文件只读展示，不作任何修改。
 * HTTP（自动带 /api/v1/story-tavern 前缀）：
 *   GET  /sessions/{sid}/world/entities?kind=&q=     实体清单
 *   GET  /sessions/{sid}/world/entities/{entity_id}  实体详情 + 关系树
 *   GET  /sessions/{sid}/world/truth                 真相账本（event_log 派生）
 */

  function stWorldCss() {
    const css = [
      '.st-world-panel{position:fixed;right:16px;bottom:64px;z-index:214;width:min(560px,94vw);max-height:86vh;display:flex;flex-direction:column;background:rgba(16,18,26,.96);border:1px solid rgba(120,140,255,.28);border-radius:14px;box-shadow:0 8px 34px rgba(0,0,0,.5);color:#dfe3f2;font-size:13px;overflow:hidden}',
      '.st-world-panel[hidden]{display:none!important}',
      '.st-world-head{display:flex;align-items:center;gap:8px;padding:8px 12px;border-bottom:1px solid rgba(120,140,255,.18);flex-wrap:wrap}',
      '.st-world-head h3{margin:0;font-size:14px;flex:1;white-space:nowrap}',
      '.st-world-tabs{background:transparent;border:1px solid rgba(120,140,255,.35);color:#b9c5e8;padding:3px 10px;border-radius:99px;font-size:12px;cursor:pointer}',
      '.st-world-tabs.active{background:rgba(120,140,255,.2);color:#fff;border-color:rgba(120,140,255,.7)}',
      '.st-world-close{background:transparent;border:none;color:#93a0c8;font-size:16px;cursor:pointer;padding:0 4px}',
      '.st-world-body{overflow:auto;padding:10px 12px;display:flex;flex-direction:column;gap:8px}',
      '.st-world-toolbar{display:flex;gap:6px;align-items:center;flex-wrap:wrap}',
      '.st-world-toolbar input,.st-world-toolbar select,.st-world-toolbar button{background:rgba(255,255,255,.07);border:1px solid rgba(255,255,255,.16);color:#dfe3f2;border-radius:8px;padding:4px 10px;font-size:12px}',
      '.st-world-toolbar button{cursor:pointer}',
      '.st-world-toolbar button:hover{background:rgba(120,140,255,.18)}',
      '.st-entity-card{background:rgba(120,140,255,.07);border:1px solid rgba(120,140,255,.16);border-radius:10px;padding:8px 10px;cursor:pointer;display:flex;gap:8px;align-items:center;flex-wrap:wrap}',
      '.st-entity-card:hover{border-color:rgba(120,140,255,.45);background:rgba(120,140,255,.13)}',
      '.st-entity-kind{flex:0 0 auto;font-size:10px;padding:1px 8px;border-radius:99px;background:rgba(120,140,255,.22);color:#b9c5ff;text-transform:uppercase;letter-spacing:.4px}',
      '.st-entity-name{font-weight:700}',
      '.st-entity-desc{color:#9aa3c0;font-size:12px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;max-width:300px}',
      '.st-entity-tags{color:#7d86a8;font-size:11px;margin-left:auto;white-space:nowrap}',
      '.st-rel-block{border:1px solid rgba(255,150,120,.2);border-radius:10px;padding:6px 10px;margin:6px 0;font-size:12px;line-height:1.7}',
      '.st-rel-out{color:#8fd0ff}.st-rel-in{color:#ffc98f}',
      '.st-back-btn{background:transparent;border:1px solid rgba(120,140,255,.3);color:#b9c5e8;border-radius:8px;padding:2px 10px;font-size:12px;cursor:pointer}',
      '.st-truth-head{display:grid;grid-template-columns:150px 1.2fr .9fr auto;gap:8px;padding:6px 10px;font-weight:700;color:#9aa3c0;font-size:12px;border-bottom:1px solid rgba(120,140,255,.22);position:sticky;top:0;background:rgba(16,18,26,.98);z-index:1}',
      '.st-truth-row{display:grid;grid-template-columns:150px 1.2fr .9fr auto;gap:8px;padding:6px 10px;border-bottom:1px solid rgba(255,255,255,.06);font-size:12px;align-items:center}',
      '.st-truth-who{color:#9aa3c0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}',
      '.st-truth-key{font-family:ui-monospace,monospace;color:#aeb3e8;word-break:break-all}',
      '.st-truth-val{color:#8fd36a;word-break:break-all;font-weight:600}',
      '.st-truth-meta{color:#7d86a8;font-size:11px;white-space:nowrap}',
      '.st-world-msg{color:#9aa3c0;font-size:12px;padding:10px 0;text-align:center}',
      'html[data-color-scheme="day"] .st-world-panel{background:rgba(250,251,255,.98);border-color:rgba(80,100,200,.35);color:#23263a;box-shadow:0 8px 30px rgba(0,0,0,.18)}',
      'html[data-color-scheme="day"] .st-world-panel input,html[data-color-scheme="day"] .st-world-panel select,html[data-color-scheme="day"] .st-world-toolbar button{background:#fff;border-color:rgba(0,0,0,.18);color:#333}',
      'html[data-color-scheme="day"] .st-world-tabs{color:#2a3a6a;border-color:rgba(60,80,160,.4)}',
      'html[data-color-scheme="day"] .st-world-tabs.active{background:rgba(80,100,200,.15);color:#141e4a;border-color:rgba(60,80,160,.65)}',
      'html[data-color-scheme="day"] .st-entity-card{background:rgba(80,100,200,.05);border-color:rgba(80,100,200,.18)}',
      'html[data-color-scheme="day"] .st-entity-desc{color:#606a90}',
      'html[data-color-scheme="day"] .st-world-msg{color:#606a90}',
      'html[data-color-scheme="day"] .st-truth-row{border-color:rgba(0,0,0,.08)}',
      'html[data-color-scheme="day"] .st-truth-head{border-color:rgba(0,0,0,.16);background:rgba(247,248,253,.98)}',
    ].join('\n');
    let style = document.getElementById('st-world-css');
    if (!style) {
      style = document.createElement('style');
      style.id = 'st-world-css';
      document.head.appendChild(style);
    }
    style.textContent = css;
  }

  function stWorldPanel() {
    stWorldCss();
    let panel = document.getElementById('st-world-panel');
    if (!panel) {
      panel = document.createElement('div');
      panel.id = 'st-world-panel';
      panel.className = 'st-world-panel';
      panel.hidden = true;
      panel.innerHTML =
        '<div class="st-world-head">' +
          '<button type="button" class="st-world-tabs" data-stw-tab="graph">🌐 世界图谱</button>' +
          '<button type="button" class="st-world-tabs" data-stw-tab="truth">📖 真相账本</button>' +
          '<button type="button" class="st-world-close" data-stw-act="close" title="关闭">✕</button>' +
        '</div>' +
        '<div class="st-world-body" id="st-world-body"></div>';
      panel.addEventListener('click', function (ev) {
        const close = ev.target.closest('[data-stw-act="close"]');
        if (close) { panel.hidden = true; return; }
        const tab = ev.target.closest('[data-stw-tab]');
        if (tab) { stWorldSwitchTab(tab.getAttribute('data-stw-tab')); return; }
        const refresh = ev.target.closest('[data-stw-cmd="refresh"]');
        if (refresh) { stWorldRefetch(stWorldQ || ''); return; }
        const card = ev.target.closest('[data-stw-eid]');
        if (card && stWorldMode() === 'graph') { stWorldEntityDetail(card.getAttribute('data-stw-eid')); return; }
        const back = ev.target.closest('[data-stw-back]');
        if (back) { stWorldLoadEntities(); }
      });
      document.body.appendChild(panel);
    }
    return panel;
  }

  // 幂等注入魔棒按钮（参照 U6 drift 模式）
  function stWorldBtn() {
    let btnG = document.getElementById('st-u2-graph-btn');
    let btnT = document.getElementById('st-u7-truth-btn');
    if (btnG && btnT) return;
    const menu = document.querySelector('#st-wand-menu .st-wand-grid');
    if (!menu) return;
    if (!btnG) {
      btnG = document.createElement('button');
      btnG.type = 'button';
      btnG.id = 'st-u2-graph-btn';
      // [fix 2026-08-16] 与魔棒其他按钮统一：ghost st-tool-btn + lucide SVG + btn-lab
      // （原 st-wand-item + 🌐 emoji 文本，裸按钮且与全站线条图标割裂）
      btnG.className = 'ghost st-tool-btn';
      btnG.innerHTML = '<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><circle cx="12" cy="12" r="10"/><path d="M12 2a14.5 14.5 0 0 0 0 20 14.5 14.5 0 0 0 0-20"/><path d="M2 12h20"/></svg><span class="btn-lab">图谱</span>';
      btnG.title = '查看会话世界状态图谱（实体清单 + 关系树）';
      btnG.addEventListener('click', function () { stWorldOpen('graph'); });
      menu.appendChild(btnG);
    }
    if (!btnT) {
      btnT = document.createElement('button');
      btnT.type = 'button';
      btnT.id = 'st-u7-truth-btn';
      // [fix 2026-08-16] 同上：统一 ghost st-tool-btn + lucide SVG + btn-lab
      btnT.className = 'ghost st-tool-btn';
      btnT.innerHTML = '<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M2 3h6a4 4 0 0 1 4 4v14a3 3 0 0 0-3-3H2z"/><path d="M22 3h-6a4 4 0 0 0-4 4v14a3 3 0 0 1 3-3h7z"/></svg><span class="btn-lab">账本</span>';
      btnT.title = '查看世界真相账本（事件日志派生的全部断言）';
      btnT.addEventListener('click', function () { stWorldOpen('truth'); });
      menu.appendChild(btnT);
    }
  }

  function stWorldOpen(tab) {
    const tavCur = (typeof stCurrentSession === 'function' ? stCurrentSession() : null);
    if (!tavCur || !tavCur.sessionId) { stStatus('先进入一场叙事再查看世界图谱'); return; }
    const panel = stWorldPanel();
    stWorldSwitchTab(tab || 'graph');
    panel.hidden = false;
  }

  function stWorldSwitchTab(tab) {
    const panel = document.getElementById('st-world-panel');
    if (!panel) return;
    panel.querySelectorAll('.st-world-tabs').forEach(function (b) {
      b.classList.toggle('active', b.getAttribute('data-stw-tab') === tab);
    });
    if (tab === 'truth') { stWorldLoadTruth(); } else { stWorldLoadEntities(); }
  }

  function stWorldMode() {
    const p = document.getElementById('st-world-panel');
    if (!p) return 'graph';
    const on = p.querySelector('.st-world-tabs.active');
    return (on && on.getAttribute('data-stw-tab')) || 'graph';
  }

  function stWorldBody() {
    const b = document.getElementById('st-world-body');
    return b || stWorldPanel().querySelector('#st-world-body');
  }

  async function stWorldLoadEntities() {
    const sid = stCurrentSession() && stCurrentSession().sessionId;
    if (!sid) { stWorldBody().innerHTML = '<div class="st-world-msg">无会话</div>'; return; }
    stWorldBody().innerHTML = '<div class="st-world-msg">加载实体…</div>';
    try {
      const r = await stApi('/sessions/' + encodeURIComponent(sid) + '/world/entities', { method: 'GET' });
      const items = (r && r.entities) || [];
      const count = (r && r.count) || items.length;
      let html = '<div class="st-world-toolbar">' +
        '<span style="color:#9aa3c0;font-size:12px">共 ' + count + ' 个实体</span>' +
        '<input id="st-world-q" placeholder="搜索名称/ID" style="width:130px">' +
        '<button type="button" data-stw-cmd="refresh">⟳ 刷新</button>' +
        '</div>';
      if (!items.length) {
        html += '<div class="st-world-msg">尚无实体。创建会话时自动播种角色；事件由编排器写入。</div>';
        stWorldBody().innerHTML = html;
        return;
      }
      html += items.map(function (e) {
        const desc = String(e.description || '').slice(0, 60);
        const tags = [];
        const flagN = (e.stateFlags || []).length;
        const cntN = Object.keys(e.counters || {}).length;
        if (flagN) tags.push('⚑' + flagN);
        if (cntN) tags.push('#' + cntN);
        tags.push((e.relationCount || 0) + ' 关系');
        return '<div class="st-entity-card" data-stw-eid="' + String(e.id).replace(/"/g, '&quot;') + '">' +
          '<span class="st-entity-kind">' + stWorldEsc(e.kind) + '</span>' +
          '<span class="st-entity-name">' + stWorldEsc(e.name) + '</span>' +
          (desc ? '<span class="st-entity-desc">' + stWorldEsc(desc) + '</span>' : '') +
          '<span class="st-entity-tags">' + tags.map(stWorldEsc).join(' · ') + '</span>' +
          '</div>';
      }).join('');
      stWorldBody().innerHTML = html;
      const qInput = stWorldBody().querySelector('#st-world-q');
      if (qInput) {
        let debTimer = null;
        qInput.addEventListener('input', function () {
          if (debTimer) clearTimeout(debTimer);
          debTimer = setTimeout(function () { stWorldRefetch(qInput.value.trim()); }, 300);
        });
      }
    } catch (e) {
      stWorldBody().innerHTML = '<div class="st-world-msg">加载失败：' + stWorldEsc(String(e && e.message || e)) + '</div>';
    }
  }

  let stWorldQ = '';
  async function stWorldRefetch(q) {
    stWorldQ = q || '';
    const sid = stCurrentSession() && stCurrentSession().sessionId;
    if (!sid) return;
    const qs = stWorldQ ? ('?q=' + encodeURIComponent(stWorldQ)) : '';
    stWorldBody().innerHTML = '<div class="st-world-msg">过滤中…</div>';
    try {
      const r = await stApi('/sessions/' + encodeURIComponent(sid) + '/world/entities' + qs, { method: 'GET' });
      const items = (r && r.entities) || [];
      if (!items.length) {
        stWorldBody().innerHTML = '<div class="st-world-msg">无匹配实体。共 ' + ((r && r.count) || 0) + ' 个。</div>';
        return;
      }
      let html = '<div class="st-world-toolbar"><span style="color:#647aa0;font-size:12px">匹配 ' + items.length + ' 个（q=' + stWorldEsc(stWorldQ) + '）</span><button type="button" data-stw-back="1">⟵ 全部</button></div>';
      html += items.map(function (e) {
        const desc = String(e.description || '').slice(0, 60);
        return '<div class="st-entity-card" data-stw-eid="' + String(e.id).replace(/"/g, '&quot;') + '">' +
          '<span class="st-entity-kind">' + stWorldEsc(e.kind) + '</span>' +
          '<span class="st-entity-name">' + stWorldEsc(e.name) + '</span>' +
          (desc ? '<span class="st-entity-desc">' + stWorldEsc(desc) + '</span>' : '') +
          '<span class="st-entity-tags">' + ((e.relationCount || 0) + ' 关系') + '</span>' +
          '</div>';
      }).join('');
      stWorldBody().innerHTML = html;
    } catch (e2) {
      stWorldBody().innerHTML = '<div class="st-world-msg">过滤失败：' + stWorldEsc(String(e2 && e2.message || e2)) + '</div>';
    }
  }

  async function stWorldEntityDetail(id) {
    const sid = stCurrentSession() && stCurrentSession().sessionId;
    if (!sid) return;
    const body = stWorldBody();
    body.innerHTML = '<div class="st-world-msg">展开 ' + stWorldEsc(id) + '…</div>';
    try {
      const r = await stApi('/sessions/' + encodeURIComponent(sid) + '/world/entities/' + encodeURIComponent(id), { method: 'GET' });
      const e = (r && r.entity) || {};
      const rels = (r && r.relationships) || { out: [], in: [] };
      const outs = rels.out || [];
      const ins = rels.in || [];
      let html = '<div class="st-world-toolbar"><button type="button" data-stw-back="1">⟲ 返回列表</button><span style="color:#9aa3c0;font-size:12px">' +
        stWorldEsc(e.kind) + ' · ' + stWorldEsc(e.name) + '</span></div>';
      html += '<div style="border:1px solid rgba(120,140,255,.25);border-radius:10px;padding:8px 12px;font-size:13px">';
      if (e.description) html += '<div style="color:#9aa3c0;margin-bottom:6px">' + stWorldEsc(String(e.description)) + '</div>';
      html += '<div style="color:#7d86a8;font-size:12px">' +
        '⚑ flags: ' + (e.stateFlags || []).join(', ') + ' | ' +
        '# counters: ' + Object.keys(e.counters || {}).map(stWorldEscC).join(', ') + '</div></div>';
      html += '<div style="font-size:13px;font-weight:700;margin-top:4px">关系链（' + (outs.length + ins.length) + '）</div>';
      if (!outs.length && !ins.length) html += '<div class="st-world-msg">暂无关系 —— 事件写入 RelationshipSet 后在此展示</div>';
      if (outs.length) {
        html += '<div class="st-rel-block st-rel-out">出边：' + outs.map(function (o) {
          return '→ <b>' + stWorldEsc(o.targetName || o.targetId) + '</b> <span>(' + stWorldEsc(o.targetKind) + ')</span> <span class="st-truth-meta">' + stWorldEsc(o.relationType) + ' ' + o.strength + '</span>';
        }).join('；') + '</div>';
      }
      if (ins.length) {
        html += '<div class="st-rel-block st-rel-in">入边：' + ins.map(function (o) {
          return '← <b>' + stWorldEsc(o.sourceName || o.sourceId) + '</b> <span>(' + stWorldEsc(o.sourceKind) + ')</span> <span class="st-truth-meta">' + stWorldEsc(o.relationType) + ' ' + o.strength + '</span>';
        }).join('；') + '</div>';
      }
      stWorldBody().innerHTML = html;
    } catch (e) {
      stWorldBody().innerHTML = '<div class="st-world-msg">详情失败：' + stWorldEsc(String(e && e.message || e)) + '</div>';
    }
  }

  async function stWorldLoadTruth() {
    const sid = stCurrentSession() && stCurrentSession().sessionId;
    if (!sid) { stWorldBody().innerHTML = '<div class="st-world-msg">无会话</div>'; return; }
    const body = stWorldBody();
    body.innerHTML = '<div class="st-world-msg">计算真相账本…</div>';
    try {
      const r = await stApi('/sessions/' + encodeURIComponent(sid) + '/world/truth', { method: 'GET' });
      const entries = (r && r.entries) || [];
      let html = '<div class="st-world-toolbar"><span style="color:#647aa0;font-size:12px">账本 ' + entries.length + ' 条断言（事件日志 ' + ((r && r.eventLogLen) || 0) + ' 条）</span></div>';
      if (!entries.length) {
        html += '<div class="st-world-msg">暂无事实条目。创建会话时播种的角色为存在断言；后续事件会追加。</div>';
        body.innerHTML = html;
        return;
      }
      html += '<div class="st-truth-head"><span>对象</span><span>键</span><span>值</span><span>版本</span></div>';
      html += entries.map(function (t) {
        let val = t.value;
        let valStr = '∅';
        if (val !== null && val !== undefined) {
          if (typeof val === 'object') valStr = JSON.stringify(val);
          else valStr = String(val);
        }
        const who = t.scopeName || t.scope || '—';
        return '<div class="st-truth-row">' +
          '<span class="st-truth-who" title="' + stWorldEsc(t.scope) + '">' + stWorldEsc(String(who).slice(0, 24)) + '</span>' +
          '<span class="st-truth-key">' + stWorldEsc(String(t.key || '')) + '</span>' +
          '<span class="st-truth-val">' + stWorldEsc(valStr.slice(0, 48)) + '</span>' +
          '<span class="st-truth-meta">v' + (t.version || 0) + '·e' + (t.lastEvent || 0) + '</span>' +
          '</div>';
      }).join('');
      body.innerHTML = html;
    } catch (e) {
      body.innerHTML = '<div class="st-world-msg">账本失败：' + stWorldEsc(String(e && e.message || e)) + '</div>';
    }
  }

  function stWorldEsc(s) {
    return String(s == null ? '' : s).replace(/[&<>"']/g, function (c) {
      return { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c];
    });
  }
  function stWorldEscC(s) { return String(s == null ? '' : s); }

  function stWorldWire() {
    if (!document.getElementById('st-u2-graph-btn') || !document.getElementById('st-u7-truth-btn')) stWorldBtn();
  }
  (function stWorldInitWatch() {
    const iv = setInterval(function () {
      if (document.querySelector('#st-wand-menu')) {
        stWorldBtn();
        clearInterval(iv);
      }
    }, 1200);
    setTimeout(function () {
      setInterval(function () { if (document.querySelector('#st-wand-menu')) stWorldBtn(); }, 5000);
    }, 8000);
  })();

/* ================= _image-part.js ================= */
/* U10 图像管线消费模块前端（吞噬 denova internal/*：bookcover / illustration / loreimage / imagepreset）。
 *
 * 挂载点：档案馆 Pack 详情页 `.u10-img-card` 区块（web/index.html 已有 HTML 骨架）。
 * 按钮：
 *   #u10-cover-gen   生成书封面（3:4，无文字无印花）
 *   #u10-chapter-illu 生成当前章节插图（落盘 data/works/<work>/images/illustrations/）
 *   #u10-lore-illu    生成当前选中资料项配图
 * 输入：标题/风格（preset 名，datalist 提示）、渠道下拉（uniapi/cf-manager/grok2api）
 * 展示：#u10-cover-img / #u10-illu-img / #u10-lore-img（经 /api/v1/works/image-data-url?path= 读取）
 * 预设：#u10-preset-list datalist 由后端 /api/v1/kaleido-tools/presets 填充
 */

  function u10EnsureCss() {
    // .u10-img-card 等样式在 src/css/_u10-images.css；若构建未合并则注入兜底
    if (document.getElementById('u10-css')) return;
    const css = [
      '.u10-img-card .u10-img-row{display:flex;gap:12px;flex-wrap:wrap;margin-top:10px}',
      '.u10-img-card .u10-fig{margin:0;flex:1 1 150px;min-width:120px;text-align:center}',
      '.u10-img-card .u10-img{max-width:100%;max-height:220px;border-radius:10px;border:1px solid var(--border,#2a3247);object-fit:contain}',
      '.u10-img-card .u10-inp{background:rgba(255,255,255,.04);border:1px solid var(--border,#2a3247);border-radius:8px;color:var(--text,#e8eaf2);font:inherit;font-size:12.5px;padding:6px 8px}',
      '.u10-img-card .u10-inp:focus{outline:none;border-color:#5b7cfa}',
      '.u10-img-card select.u10-inp{min-width:110px}',
      '.u10-img-card .u10-img-row button{font-size:12.5px;padding:6px 12px;border-radius:8px;border:1px solid var(--border,#2a3247);cursor:pointer;color:var(--text,#e8eaf2);background:rgba(255,255,255,.04)}',
      '.u10-img-card .u10-img-row button.primary{background:#5b7cfa;border-color:#5b7cfa;color:#fff}',
      '.u10-img-card .u10-img-row button:disabled{opacity:.5;cursor:wait}',
      'html[data-color-scheme="day"] .u10-img-card .u10-inp{background:rgba(28,25,20,.03);border-color:rgba(28,25,20,.16)}',
    ].join('\n');
    const style = document.createElement('style');
    style.id = 'u10-css';
    style.textContent = css;
    document.head.appendChild(style);
  }

  /* 当前 pack：档案馆打开的 pack（tavernPack 全局变量）或 work id */
  function u10WorkId() {
    if (typeof tavernPack !== 'undefined' && tavernPack && tavernPack.id) return tavernPack.id;
    if (typeof anWorkId === 'function') { try { const w = __anWorkId(); if (w) return w; } catch (e) {} }
    return '';
  }

  function u10Channel() {
    const el = document.getElementById('u10-channel');
    return el ? el.value : 'uniapi';
  }

  function u10SetStatus(which, text, isErr) {
    const el = document.getElementById(which);
    if (!el) return;
    el.textContent = text;
    el.style.color = isErr ? '#ff8a8a' : '';
  }

  function u10ShowImg(imgId, dataUrlOrPath) {
    const img = document.getElementById(imgId);
    if (!img) return;
    if (!dataUrlOrPath) { img.classList.add('hidden'); img.removeAttribute('src'); return; }
    if (/^data:image\//.test(dataUrlOrPath) || /^https?:/.test(dataUrlOrPath)) {
      img.src = dataUrlOrPath;
    } else {
      // 相对路径：走 image-data-url 端点读取
      img.src = '/api/v1/works/image-data-url?path=' + encodeURIComponent(dataUrlOrPath);
    }
    img.classList.remove('hidden');
  }

  async function u10Api(path, opts) {
    const o = opts || {};
    const r = await stApi(path, o);
    if (r && r.error) throw new Error(r.error);
    return r;
  }

  /* 生成书封面 */
  async function u10GenCover() {
    const btn = document.getElementById('u10-cover-gen');
    const titleEl = document.getElementById('u10-cover-title');
    const styleEl = document.getElementById('u10-cover-style');
    const title = (titleEl && titleEl.value.trim()) || (tavernPack && tavernPack.title) || '未命名';
    const style = styleEl ? styleEl.value.trim() : '';
    if (!title) { u10SetStatus('u10-cover-status', '标题为空', true); return; }
    if (btn) { btn.disabled = true; btn.textContent = '生成中…'; }
    u10SetStatus('u10-cover-status', '正在生成书封面（3:4）…');
    try {
      const r = await u10Api('/api/v1/kaleido-tools/bookcover', {
        method: 'POST',
        body: JSON.stringify({ title, style: style || undefined, channel: u10Channel() }),
      });
      u10ShowImg('u10-cover-img', r.url || r.imageUrl || r.dataUrl || '');
      u10SetStatus('u10-cover-status', r.path ? '已落盘：' + r.path : '生成成功');
    } catch (e) {
      u10SetStatus('u10-cover-status', '封面生成失败：' + (e.message || e), true);
    } finally {
      if (btn) { btn.disabled = false; btn.textContent = '生成书封面'; }
    }
  }

  /* 生成当前章节插图 */
  async function u10GenIllustration() {
    const btn = document.getElementById('u10-chapter-illu');
    const workId = u10WorkId();
    if (!workId) { u10SetStatus('u10-illu-status', '未打开 Pack', true); return; }
    const chapterId = (tavernSession && (tavernSession.chapterCursor || tavernSession.nodeId)) || 'ch01';
    const styleEl = document.getElementById('u10-cover-style');
    const style = styleEl ? styleEl.value.trim() : '';
    if (btn) { btn.disabled = true; btn.textContent = '生成中…'; }
    u10SetStatus('u10-illu-status', '正在生成章节插图…');
    try {
      const r = await u10Api('/api/v1/kaleido-tools/illustration', {
        method: 'POST',
        body: JSON.stringify({ workId, chapterId, style: style || undefined, channel: u10Channel() }),
      });
      u10ShowImg('u10-illu-img', r.url || r.imageUrl || r.dataUrl || '');
      u10SetStatus('u10-illu-status', r.path ? '已落盘：' + r.path : '生成成功');
    } catch (e) {
      u10SetStatus('u10-illu-status', '插图失败：' + (e.message || e), true);
    } finally {
      if (btn) { btn.disabled = false; btn.textContent = '生成当前章节插图'; }
    }
  }

  /* 生成资料项配图 */
  async function u10GenLoreImage() {
    const btn = document.getElementById('u10-lore-illu');
    const workId = u10WorkId();
    if (!workId) { u10SetStatus('u10-lore-status', '未打开 Pack', true); return; }
    if (btn) { btn.disabled = true; btn.textContent = '生成中…'; }
    u10SetStatus('u10-lore-status', '正在生成资料项配图…');
    try {
      const r = await u10Api('/api/v1/kaleido-tools/lore-image', {
        method: 'POST',
        body: JSON.stringify({ workId, itemId: 'current', channel: u10Channel() }),
      });
      u10ShowImg('u10-lore-img', r.url || r.imageUrl || r.dataUrl || '');
      u10SetStatus('u10-lore-status', r.path ? '已落盘：' + r.path : '生成成功');
    } catch (e) {
      u10SetStatus('u10-lore-status', '配图失败：' + (e.message || e), true);
    } finally {
      if (btn) { btn.disabled = false; btn.textContent = '生成资料项配图'; }
    }
  }

  /* 加载预设 datalist */
  async function u10LoadPresets() {
    const dl = document.getElementById('u10-preset-list');
    if (!dl) return;
    try {
      const r = await u10Api('/api/v1/kaleido-tools/presets');
      const presets = (r && r.presets) || [];
      dl.innerHTML = '';
      for (const p of presets) {
        const opt = document.createElement('option');
        opt.value = p.name || p.id || '';
        dl.appendChild(opt);
      }
    } catch (e) { /* 预设加载失败不影响生成 */ }
  }

  function u10Bind() {
    u10EnsureCss();
    const coverBtn = document.getElementById('u10-cover-gen');
    if (coverBtn) coverBtn.onclick = u10GenCover;
    const illuBtn = document.getElementById('u10-chapter-illu');
    if (illuBtn) illuBtn.onclick = u10GenIllustration;
    const loreBtn = document.getElementById('u10-lore-illu');
    if (loreBtn) loreBtn.onclick = u10GenLoreImage;
    u10LoadPresets();
  }

  /* 挂载：档案馆 pack 详情打开时绑定（stLoadPack / stShowPack 后），幂等 */
  function u10Mount() {
    u10EnsureCss();
    u10Bind();
  }

  if (typeof window._u10Mounted === 'undefined') {
    window._u10Mounted = true;
    if (document.readyState === 'loading') {
      document.addEventListener('DOMContentLoaded', u10Mount);
    } else {
      u10Mount();
    }
    // 档案馆切换 pack 时重新绑定（事件委托保险）
    document.addEventListener('st:pack-opened', u10Mount);
    document.addEventListener('st:pack-detail-shown', u10Mount);
  }
