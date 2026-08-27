/* P1-3 S2.8: _chat-part.js + _settings-chat.js send/stream trio → real ESM.
 *
 * Chat-domain shared lets (messages/sessionId/streaming/es/activeRunId/partner)
 * stay CANONICAL in the IIFE closure (_state-part.js) — agent/tabs closure code
 * still reads/writes them bare. This module accesses them through the
 * window.__kaleidoChatState facade published inside the closure (S2.6 pattern,
 * no reliance on bundler scope-flattening).
 *
 * Outward consumers (story/tavern/agent/settings/tabs/partner parts) keep using
 * bare identifiers — vite.config.mjs converted[] imports this module's exports
 * at virtual-module top level, which the remaining IIFE parts see lexically
 * (same mechanism as $/api/showToast/tabs aliases).
 */
import { $ } from './dom.js';
import { api, getSseTicket } from './api.js';
import { apiBase } from './api_shell.js';
import { getToken } from './api.js';
import { friendlyError } from './api_shell.js';
import { showToast } from './toast.js';
import { applyStRegexScripts } from './st_regex.js';
import { messagesEl, sessionList, input, stopBtn, sendBtn } from './state.js';
import { SID_KEY, displayTitle, formatDateTime, uid } from './utils.js';
import { switchTab, switchAzView, updateImmersive, exitImmersive } from './tabs_bridge.js';

/** Lazy accessor for the closure-published chat-state facade. */
function __cs() {
  const c = typeof window !== 'undefined' && window.__kaleidoChatState;
  if (!c) throw new Error('chat state facade not ready (called before _state-part evaluated)');
  return c;
}

function renderMessages(opts) {
    opts = opts || {};
    const list = __cs().messages || [];
    const streamTail = !!(opts.stream && list.length && list[list.length - 1] && list[list.length - 1].role === 'assistant');
    if (!messagesEl) return;

    // Fast path: only patch the streaming assistant bubble body (no full list rebuild).
    if (streamTail) {
      const last = list[list.length - 1];
      let node = messagesEl.querySelector('.bubble[data-mid="' + cssEscape(last.id) + '"]');
      if (!node) {
        // First assistant token after user — ensure both last two exist without nuking earlier.
        ensureBubbleDom(messagesEl, list, {
          roleLabel: function (m) { return m.role === 'user' ? '你' : '伴侣'; },
          bodyText: function (m) {
            // streaming tail: raw; older bubbles can keep raw too (final full paint applies scripts)
            return m.content || '';
          },
          streamId: last.id,
        });
        node = messagesEl.querySelector('.bubble[data-mid="' + cssEscape(last.id) + '"]');
      }
      if (node) {
        const body = node.querySelector('.bubble-body');
        const text = last.content || '';
        // During stream: paint raw (no ST regex) to avoid mid-token rewrites that jump glyphs.
        // Final renderMessages() / done path still applies scripts.
        if (body) {
          if (body.textContent !== text) body.textContent = text;
        } else if (node.lastChild && node.lastChild.nodeType === 3) {
          if (node.lastChild.textContent !== text) node.lastChild.textContent = text;
        } else {
          const span = document.createElement('span');
          span.className = 'bubble-body';
          span.textContent = text;
          node.appendChild(span);
        }
        node.classList.add('is-streaming');
        // stick to bottom only if user was already near bottom
        const nearBottom = messagesEl.scrollHeight - messagesEl.scrollTop - messagesEl.clientHeight < 80;
        if (nearBottom) messagesEl.scrollTop = messagesEl.scrollHeight;
        return;
      }
    }

    // Full rebuild (load / non-stream updates)
    const stick = messagesEl.scrollHeight - messagesEl.scrollTop - messagesEl.clientHeight < 80;
    messagesEl.innerHTML = '';
    for (let i = 0; i < list.length; i++) {
      const m = list[i];
      const isLastStream = !!(opts.stream && i === list.length - 1 && m.role === 'assistant');
      messagesEl.appendChild(buildBubbleEl({
        id: m.id,
        roleClass: m.role === 'user' ? 'user' : 'agent',
        roleLabel: m.role === 'user' ? '你' : '伴侣',
        body: applyStRegexScripts(m.content || '', m.role),
        monologue: m._monologue || m.reasoning || '',
        enter: !isLastStream && !opts.quiet,
        streaming: isLastStream,
        ts: m.createdAt || m.ts || '',
        tokens: (m.tokens || m.usage || 0) ? (m.tokens || (m.usage && m.usage.total_tokens) || 0) : 0,
      }));
    }
    if (stick || opts.forceScroll) messagesEl.scrollTop = messagesEl.scrollHeight;
  }

  function cssEscape(s) {
    try {
      if (window.CSS && CSS.escape) return CSS.escape(String(s));
    } catch (_) {}
    return String(s).replace(/[^a-zA-Z0-9_-]/g, '\\$&');
  }

  let progCardSeq = 0;

  function buildProgSrcdoc(html, frameId) {
    const script =
      '<script>' +
      '(function(){function r(){var h=document.body?document.body.scrollHeight:0;' +
      'window.parent.postMessage({type:\'A7_PROG_H\',id:\'' + frameId + '\',h:h},\'*\');}' +
      'if(document.readyState===\'complete\'){r();}else{' +
      'window.addEventListener(\'load\',function(){setTimeout(r,60);});}' +
      '})();' +
      '</script>';
    const lower = html.toLowerCase();
    const bi = lower.lastIndexOf('</body>');
    if (bi !== -1) return html.slice(0, bi) + script + html.slice(bi);
    const hi = lower.lastIndexOf('</html>');
    if (hi !== -1) return html.slice(0, hi) + script + html.slice(hi);
    return html + script;
  }

  function ensureA7ProgListener() {
    if (window.__a7ProgListener) return;
    window.__a7ProgListener = true;
    window.addEventListener('message', function (e) {
      if (!e.data || e.data.type !== 'A7_PROG_H') return;
      const f = document.getElementById(e.data.id);
      if (!f) return;
      const h = Number(e.data.h);
      if (!isFinite(h) || h <= 0) return;
      f.style.height = Math.max(120, Math.min(680, h + 16)) + 'px';
    });
  }

  function buildBubbleEl(spec) {
    const div = document.createElement('div');
    div.className = 'bubble ' + (spec.roleClass || 'agent');
    if (spec.enter) div.classList.add('is-enter');
    if (spec.streaming) div.classList.add('is-streaming');
    if (spec.extraClass) {
      String(spec.extraClass).split(/\s+/).forEach(function (c) { if (c) div.classList.add(c); });
    }
    if (spec.id) div.setAttribute('data-mid', spec.id);
    const role = document.createElement('span');
    role.className = 'role';
    role.textContent = spec.roleLabel || '';
    div.appendChild(role);
    // P2-1: 角色情绪表情角标（agent 气泡；无情绪则静默无角标）
    if (spec.emotionEmoji) {
      const badge = document.createElement('span');
      badge.className = 'st-emotion-badge';
      badge.textContent = spec.emotionEmoji;
      role.appendChild(badge);
    }
    if (spec.program) {
      const wrap = document.createElement('div');
      wrap.className = 'st-program';
      const frame = document.createElement('iframe');
      frame.id = 'prog-card-' + (++progCardSeq);
      frame.setAttribute('sandbox', 'allow-scripts');
      frame.setAttribute('title', '程序卡');
      frame.setAttribute('loading', 'lazy');
      frame.setAttribute('srcdoc', buildProgSrcdoc(String(spec.program), frame.id));
      wrap.appendChild(frame);
      div.appendChild(wrap);
      ensureA7ProgListener();
    }
    const body = document.createElement('span');
    body.className = 'bubble-body';
    const speakerMode = !!(spec.roleClass && /(^|\s)agent(\s|$)/.test(spec.roleClass));
    fillBubbleBody(body, spec.body == null ? '' : String(spec.body), { plain: !!spec.plainBody, speakerMode: speakerMode });
    div.appendChild(body);
    // 内心独白（Omate 对齐）：agent 消息若有流式累积的 thinking，渲染默认折叠区块
    if (spec.monologue && spec.monologue.trim() && spec.roleClass && /(^|\s)agent(\s|$)/.test(spec.roleClass)) {
      const mono = document.createElement('details');
      mono.className = 'st-monologue';
      const sum = document.createElement('summary');
      sum.textContent = '推理';
      const pre = document.createElement('div');
      pre.className = 'st-monologue-body';
      const p = document.createElement('p');
      p.textContent = String(spec.monologue).trim();
      pre.appendChild(p);
      mono.appendChild(sum); mono.appendChild(pre);
      div.appendChild(mono);
    }
    // P3-swipe：assistant 气泡 swipe 备选回复控件（◀ N/N ▶）
    if (spec.swipeSupport && spec.roleClass && /(^|\s)agent(\s|$)/.test(spec.roleClass)) {
      const n = Math.max(1, spec.swipeCount || 1);
      const idx = Math.max(0, spec.swipeIdx || 0);
      const bar = document.createElement('div');
      bar.className = 'st-swipe-bar';
      const prev = document.createElement('button');
      prev.type = 'button'; prev.className = 'st-swipe-btn'; prev.setAttribute('aria-label', '上一条');
      prev.innerHTML = '&#9664;';
      const cnt = document.createElement('button');
      cnt.type = 'button'; cnt.className = 'st-swipe-cnt'; cnt.textContent = (idx + 1) + '/' + n;
      cnt.title = '查看全部备选回复';
      cnt.onclick = function (e) {
        e.stopPropagation();
        if (window.stSwipePicker) window.stSwipePicker(div);
      };
      const next = document.createElement('button');
      next.type = 'button'; next.className = 'st-swipe-btn'; next.setAttribute('aria-label', '下一条');
      next.innerHTML = '&#9654;';
      prev.onclick = function (e) { e.stopPropagation(); stSwipe(div, -1); };
      next.onclick = function (e) { e.stopPropagation(); stSwipe(div, 1); };
      bar.appendChild(prev); bar.appendChild(cnt); bar.appendChild(next);
      div.appendChild(bar);
    }
    // 消息操作菜单（ST 核心交互：复制/编辑/删除）——hover 显示
    if (spec.id) {
      const actions = document.createElement('div');
      actions.className = 'st-msg-actions';
      const copyBtn = document.createElement('button');
      copyBtn.type = 'button'; copyBtn.className = 'st-msg-act'; copyBtn.title = '复制消息';
      copyBtn.textContent = '📋';
      copyBtn.onclick = function (e) {
        e.stopPropagation();
        try {
          const bodyEl = div.querySelector('.bubble-body');
          const txt = (bodyEl && bodyEl.textContent) || String(spec.body || '');
          (navigator.clipboard ? navigator.clipboard.writeText(txt) : Promise.reject()).catch(function () {
            const ta = document.createElement('textarea');
            ta.value = txt; document.body.appendChild(ta); ta.select();
            try { document.execCommand('copy'); } catch (_) {}
            document.body.removeChild(ta);
          });
          if (window.stStatus) window.stStatus('已复制消息');
        } catch (_) {}
      };
      const editBtn = document.createElement('button');
      editBtn.type = 'button'; editBtn.className = 'st-msg-act'; editBtn.title = '编辑消息';
      editBtn.textContent = '✏️';
      editBtn.onclick = function (e) {
        e.stopPropagation();
        if (window.stEditMessage) window.stEditMessage(div);
      };
      const delBtn = document.createElement('button');
      delBtn.type = 'button'; delBtn.className = 'st-msg-act del'; delBtn.title = '删除消息';
      delBtn.textContent = '🗑';
      delBtn.onclick = function (e) {
        e.stopPropagation();
        if (window.stDeleteMessage) window.stDeleteMessage(div);
      };
      const bmBtn = document.createElement('button');
      bmBtn.type = 'button'; bmBtn.className = 'st-msg-act bm'; bmBtn.title = '收藏消息（书签）';
      bmBtn.textContent = '⭐';
      bmBtn.onclick = function (e) {
        e.stopPropagation();
        if (window.stToggleBookmark) window.stToggleBookmark(div);
      };
      actions.appendChild(copyBtn); actions.appendChild(editBtn); actions.appendChild(bmBtn); actions.appendChild(delBtn);
      // 部分编辑按钮（RisuAI 对齐：弹层局部改写）
      if (window.stPartialEdit) {
        const peBtn = document.createElement('button');
        peBtn.type = 'button'; peBtn.className = 'st-msg-act'; peBtn.title = '部分编辑（局部改写）';
        peBtn.textContent = '🔧';
        peBtn.onclick = function (e) {
          e.stopPropagation();
            window.stPartialEdit(div);
        };
        actions.appendChild(peBtn);
      }
      div.appendChild(actions);
    }
    // 消息元信息：时间戳 + token 数（ST 对齐，设置开关控制）
    const metaOn = localStorage.getItem('stMsgMeta') === '1';
    if (metaOn && spec.ts) {
      const meta = document.createElement('div');
      meta.className = 'st-msg-meta';
      let metaText = '';
      try {
        const d = new Date(spec.ts);
        if (!isNaN(d.getTime())) metaText = d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
      } catch (_) {}
      if (spec.tokens > 0) metaText += (metaText ? ' · ' : '') + spec.tokens + ' tok';
      if (metaText) meta.textContent = metaText;
      div.appendChild(meta);
    }
    return div;
  }

  /** S8.22: split into <p.st-para> so every paragraph gets 2em first-line indent.
   *  S3c (吞噬 denova): 【检定结果】块从正文剥离，渲染为检定卡（弹窗）。 */
  /** RisuAI 对齐：解析 {{image::URL}} / {{video::URL}} / {{audio::URL}} 内联媒体。
   *  文本与媒体节点按顺序 append 到父元素。 */
  function stAppendInlineMedia(parent, text) {
    if (!parent) return;
    const str = String(text == null ? '' : text);
    const re = /\{\{(image|video|audio)::([^}]+)\}\}/g;
    let last = 0;
    let m;
    let added = false;
    while ((m = re.exec(str)) !== null) {
      if (m.index > last) parent.appendChild(document.createTextNode(str.slice(last, m.index)));
      const kind = m[1];
      const url = m[2].trim();
      let node = null;
      if (kind === 'image') {
        node = document.createElement('img');
        node.className = 'st-inline-img';
        node.loading = 'lazy';
        node.alt = '';
        node.src = url;
        node.addEventListener('error', function () { node.style.display = 'none'; });
      } else if (kind === 'video') {
        node = document.createElement('video');
        node.className = 'st-inline-video';
        node.controls = true;
        node.preload = 'metadata';
        node.src = url;
      } else {
        node = document.createElement('audio');
        node.className = 'st-inline-audio';
        node.controls = true;
        node.preload = 'metadata';
        node.src = url;
      }
      parent.appendChild(node);
      added = true;
      last = m.index + m[0].length;
    }
    if (last < str.length) parent.appendChild(document.createTextNode(str.slice(last)));
    if (!added && !parent.hasChildNodes()) parent.textContent = str;
  }

  function fillBubbleBody(el, text, opts) {
    if (!el) return;
    opts = opts || {};
    let raw = text == null ? '' : String(text);
    if (opts.plain) {
      if (el.textContent !== raw) el.textContent = raw;
      el.classList.remove('has-paras');
      return;
    }
    // S3c: 抽取检定块 → 检定卡；正文剥离该文本（检定结果以卡片呈现，不留纯文本）
    const checks = [];
    raw = extractCheckCards(raw, checks);
    // Normalize: keep blank-line paragraphs; single \n inside a block stays soft break via <br> in one para
    const blocks = raw.replace(/\r\n/g, '\n').replace(/\r/g, '\n').split(/\n{2,}/);
    const paras = [];
    for (let i = 0; i < blocks.length; i++) {
      const block = blocks[i].replace(/^\n+|\n+$/g, '');
      if (!block) continue;
      // further split lone newlines into visual lines still as one para with <br>, OR each line as para?
      // User: 每段都缩进 — treat each non-empty line group; also each single \n-separated line as paragraph for novel text
      const lines = block.split('\n');
      for (let j = 0; j < lines.length; j++) {
        const line = lines[j].replace(/^\s+|\s+$/g, '');
        if (line) paras.push(line);
      }
    }
    if (!paras.length) {
      el.textContent = raw;
      el.classList.remove('has-paras');
      for (let k = 0; k < checks.length; k++) el.appendChild(buildCheckCard(checks[k]));
      return;
    }
    // Cheap equality: if same number of paras with same text, skip
    const existing = el.querySelectorAll(':scope > .st-para');
    if (existing.length === paras.length) {
      let same = true;
      for (let k = 0; k < paras.length; k++) {
        if ((existing[k].textContent || '') !== paras[k]) { same = false; break; }
      }
      if (same) {
        el.classList.add('has-paras');
        // S3c: 补挂缺失检定卡（正文相同但卡片节点可能未建）
        const have = el.querySelectorAll(':scope > .st-check-card').length;
        for (let k = have; k < checks.length; k++) el.appendChild(buildCheckCard(checks[k]));
        return;
      }
    }
    el.textContent = '';
    el.classList.add('has-paras');
    for (let k = 0; k < paras.length; k++) {
      const p = document.createElement('p');
      p.className = 'st-para';
      // 多角色群聊：agent 气泡内「角色名：内容」段渲染发言人 chip（ST-10 群聊配套）
      if (opts.speakerMode) {
        const sm = paras[k].match(/^([^：:\n]{1,12})[：:]\s*(.*)$/);
        if (sm) {
          p.classList.add('st-speaker');
          const nm = document.createElement('span');
          nm.className = 'st-speaker-name';
          nm.textContent = sm[1];
          p.appendChild(nm);
          if (sm[2]) stAppendInlineMedia(p, sm[2]);
        } else {
          stAppendInlineMedia(p, paras[k]);
        }
      } else {
        stAppendInlineMedia(p, paras[k]);
      }
      el.appendChild(p);
    }
    // S3c: 追加检定卡（骰面/DC/结果徽章/结果文本）
    for (let k = 0; k < checks.length; k++) {
      el.appendChild(buildCheckCard(checks[k]));
    }
  }

  /** S3c: 从消息正文抽取【检定结果】块（后端 S3 追加格式）。
   *  格式：`【检定结果】action（骰面 N + 加成 B = T / DC D → outcome）\nresult_text`。
   *  返回剥离后的正文；解析结果 push 到 out（{action,head,result}）。 */
  function extractCheckCards(raw, out) {
    if (!raw || raw.indexOf('【检定结果】') === -1) return raw;
    const lines = String(raw).replace(/\r\n/g, '\n').split('\n');
    const cleaned = [];
    let i = 0;
    while (i < lines.length) {
      const ln = lines[i];
      const m = ln.match(/^【检定结果】\s*(.*?)\s*[（(]([^）)]*)[）)]\s*$/);
      if (m && (m[1] || m[2])) {
        const resultLines = [];
        let j = i + 1;
        while (j < lines.length && !/^【检定结果】/.test(lines[j]) && lines[j].trim() !== '') {
          resultLines.push(lines[j]);
          j++;
        }
        while (resultLines.length && !resultLines[resultLines.length - 1].trim()) resultLines.pop();
        out.push({
          action: m[1].trim() || '检定',
          head: m[2].trim(),
          result: resultLines.join('\n').trim(),
        });
        i = j;
        continue;
      }
      cleaned.push(ln);
      i++;
    }
    return cleaned.join('\n');
  }

  const CHECK_OUTCOME_META = {
    critical_success: { label: '大成功', cls: 'critsuccess' },
    success: { label: '成功', cls: 'success' },
    failure: { label: '失败', cls: 'failure' },
    critical_failure: { label: '大失败', cls: 'critfailure' },
  };

  /** S3c: 构建检定卡 DOM。head 形如 `骰面 14 + 加成 2 = 16 / DC 15 → success`。 */
  function buildCheckCard(c) {
    const card = document.createElement('div');
    card.className = 'st-check-card';
    const head = String(c.head || '');
    const hm = head.match(/骰面\s*([-\d.]+)\s*\+?\s*加成\s*([-\d.]+)?\s*=\s*([-\d.]+)\s*\/\s*DC\s*([-\d.]+)\s*→\s*([a-z_]+)/i);
    const meta = (hm && CHECK_OUTCOME_META[hm[5]]) ? CHECK_OUTCOME_META[hm[5]] : { label: hm ? hm[5] : '检定', cls: '' };
    const top = document.createElement('div');
    top.className = 'st-check-top';
    const title = document.createElement('div');
    title.className = 'st-check-title';
    title.textContent = '🎲 ' + c.action;
    const badge = document.createElement('span');
    badge.className = 'st-check-badge ' + meta.cls;
    badge.textContent = meta.label;
    top.appendChild(title);
    top.appendChild(badge);
    card.appendChild(top);
    const row = document.createElement('div');
    row.className = 'st-check-row';
    if (hm) {
      const total = document.createElement('div');
      total.className = 'st-check-total';
      total.textContent = hm[3];
      const sub = document.createElement('div');
      sub.className = 'st-check-sub';
      sub.textContent = '骰面 ' + hm[1] + (hm[2] && hm[2] !== '0' ? ' + 加成 ' + hm[2] : '');
      const dc = document.createElement('div');
      dc.className = 'st-check-dc';
      dc.textContent = 'DC ' + hm[4];
      row.appendChild(total);
      row.appendChild(sub);
      row.appendChild(dc);
    } else {
      const sub = document.createElement('div');
      sub.className = 'st-check-sub';
      sub.textContent = head;
      row.appendChild(sub);
    }
    card.appendChild(row);
    if (c.result) {
      const body = document.createElement('div');
      body.className = 'st-check-result';
      body.textContent = c.result;
      card.appendChild(body);
    }
    return card;
  }

  function ensureBubbleDom(container, list, hooks) {
    // Append any trailing messages not yet in DOM (by data-mid). Does not rebuild.
    const existing = new Set();
    container.querySelectorAll('.bubble[data-mid]').forEach(function (n) {
      existing.add(n.getAttribute('data-mid'));
    });
    for (let i = 0; i < list.length; i++) {
      const m = list[i];
      const id = m.id || ('idx-' + i);
      if (existing.has(id)) continue;
      const isStream = hooks.streamId && id === hooks.streamId;
      container.appendChild(buildBubbleEl({
        id: id,
        roleClass: m.role === 'user' ? 'user' : 'agent',
        roleLabel: hooks.roleLabel(m),
        body: hooks.bodyText(m),
        enter: !isStream,
        streaming: !!isStream,
      }));
    }
  }

  let chatStreamRaf = 0;
  function scheduleChatStreamPaint() {
    if (chatStreamRaf) return;
    chatStreamRaf = requestAnimationFrame(function () {
      chatStreamRaf = 0;
      if (!__cs().streaming) return;
      renderMessages({ stream: true });
    });
  }

  function setStreaming(on) {
    __cs().streaming= on;
    sendBtn.disabled = on;
    stopBtn.classList.toggle('hidden', !on);
    document.documentElement.toggleAttribute('data-streaming', !!on);
    if (!on && messagesEl) {
      messagesEl.querySelectorAll('.bubble.is-streaming').forEach(function (n) {
        n.classList.remove('is-streaming');
      });
    }
  }

  async function refreshSessions() {
    // S8-UI: skeleton before fetch
    if (sessionList) sessionList.innerHTML = '<div class="st-skeleton"><div class="line"></div><div class="line short"></div></div>' +
      '<div class="st-skeleton"><div class="line"></div><div class="line short"></div></div>';
    const list = await api('/api/mobile/sessions?prefix=partner-session-');
    sessionList.innerHTML = '';
    for (const s of list) {
      const el = document.createElement('div');
      el.className = 'item' + (s.id === __cs().sessionId ? ' active' : '');
      el.innerHTML = '<span class="t"></span><span class="d"></span>';
      el.querySelector('.t').textContent = displayTitle(s.title, '新对话');
      el.querySelector('.d').textContent = formatDateTime(s.savedAt || 0);
      el.onclick = () => { enterChatStage(); loadSession(s.id); };
      sessionList.appendChild(el);
    }
    // S9.4: mirror recent sessions into the setup view (up to 6)
    const setupList = $('chat-setup-sessions');
    if (setupList) {
      setupList.innerHTML = '';
      for (const s of list.slice(0, 6)) {
        const el = document.createElement('div');
        el.className = 'item' + (s.id === __cs().sessionId ? ' active' : '');
        el.innerHTML = '<span class="t"></span><span class="d"></span>';
        el.querySelector('.t').textContent = displayTitle(s.title, '新对话');
        el.querySelector('.d').textContent = formatDateTime(s.savedAt || 0);
        el.onclick = () => { enterChatStage(); loadSession(s.id); };
        setupList.appendChild(el);
      }
    }
  }

  // S9.4: chat tab two-view flow — setup (options) ⇄ stage (immersive)
  function enterChatStage() {
    const setup = $('chat-setup');
    const stage = $('chat-stage');
    if (setup) setup.classList.add('hidden');
    if (stage) stage.classList.remove('hidden');
    updateImmersive();
    // S9.4: keep the top chrome visible in chat stage — the 离开 button must
    // stay reachable (imm-chrome-hidden hides it by default like the theater).
    document.documentElement.classList.remove('imm-chrome-hidden');
    const backLab = document.querySelector('#imm-back .imm-back-lab');
    if (backLab) backLab.textContent = '离开';
    const backBtn = $('imm-back');
    if (backBtn) backBtn.setAttribute('aria-label', '离开对话，返回选项');
  }
  function showChatSetup() {
    const setup = $('chat-setup');
    const stage = $('chat-stage');
    if (setup) setup.classList.remove('hidden');
    if (stage) stage.classList.add('hidden');
    exitImmersive();
    const backLab = document.querySelector('#imm-back .imm-back-lab');
    if (backLab) backLab.textContent = '返回';
  }
  function setupChatStart() {
    const btn = $('chat-start');
    if (!btn) return;
    btn.onclick = async () => {
      try {
        __cs().partner= await api('/api/v1/partner/select', {
          method: 'POST',
          body: JSON.stringify({
            worldBookId: $('chat-wb').value || '',
            characterCardId: $('chat-cc').value || '',
          }),
        });
        refreshPartnerSelects();
      } catch (e) {
        if ($('partner-hint')) $('partner-hint').textContent = (typeof friendlyError === 'function' ? friendlyError(e) : e.message);
        return;
      }
      // reuse the existing empty session (created at boot) instead of stacking new ones
      if (!__cs().sessionId || __cs().messages.length > 0) {
        __cs().sessionId= uid('partner-session');
        localStorage.setItem(SID_KEY, __cs().sessionId);
        __cs().messages= [];
        renderMessages();
        try { await saveSession('新对话'); } catch (_) {}
      }
      await refreshSessions();
      enterChatStage();
      setTimeout(() => { if ($('input')) $('input').focus(); }, 60);
    };
    if ($('chat-use-sample')) {
      $('chat-use-sample').onclick = async () => {
        try {
          await api('/api/v1/story-tavern/packs/demo', { method: 'POST' });
          const __lp = window.__kaleidoPartner && window.__kaleidoPartner.loadPartner;
          if (typeof __lp === 'function') await __lp();
          refreshPartnerSelects();
          if (typeof showToast === 'function') showToast('已安装示例剧本包「雨巷来客」，可在故事馆开玩');
        } catch (e) {
          if ($('partner-hint')) $('partner-hint').textContent = (typeof friendlyError === 'function' ? friendlyError(e) : e.message);
        }
      };
    }
    if ($('chat-new-card')) {
      $('chat-new-card').onclick = () => {
        if (typeof switchTab === 'function') switchTab('works');
        setTimeout(() => { if (typeof switchAzView === 'function') switchAzView('charcard'); }, 80);
      };
    }
    if ($('chat-import-card')) {
      $('chat-import-card').onclick = () => {
        if (typeof switchTab === 'function') switchTab('st');
      };
    }
  }

  async function loadSession(id) {
    const rec = await api('/api/mobile/sessions/' + encodeURIComponent(id));
    __cs().sessionId= rec.id;
    localStorage.setItem(SID_KEY, __cs().sessionId);
    __cs().messages= (rec.messages || []).map((m) => ({
      id: m.id,
      role: m.role === 'user' ? 'user' : 'assistant',
      content: m.content || '',
    }));
    renderMessages({ forceScroll: true });
    await refreshSessions();
    updateImmersive();
  }

  async function ensureSession() {
    if (__cs().sessionId) {
      try {
        await loadSession(__cs().sessionId);
        return;
      } catch (_) {
        __cs().sessionId= '';
      }
    }
    __cs().sessionId= uid('partner-session');
    localStorage.setItem(SID_KEY, __cs().sessionId);
    __cs().messages= [];
    await saveSession('新对话');
    renderMessages();
    updateImmersive();
    await refreshSessions();
  }

  async function saveSession(title) {
    const rec = {
      id: __cs().sessionId,
      title: title || (__cs().messages.find((m) => m.role === 'user') || {}).content?.slice(0, 24) || '新对话',
      savedAt: Date.now(),
      messages: __cs().messages.map((m) => ({
        id: m.id,
        role: m.role,
        content: m.content,
      })),
      selectedReferenceFiles: [],
      todos: [],
    };
    await api('/api/mobile/sessions', { method: 'POST', body: JSON.stringify(rec) });
  }

  function closeEs() {
    if (__cs().es) {
      try { __cs().es.close(); } catch (_) {}
      __cs().es= null;
    }
  }

  function fillSelect(sel, items, selectedId, emptyLabel) {
    sel.innerHTML = '';
    const opt0 = document.createElement('option');
    opt0.value = '';
    opt0.textContent = emptyLabel || '（无）';
    sel.appendChild(opt0);
    for (const it of items) {
      const o = document.createElement('option');
      o.value = it.id;
      o.textContent = it.name || it.id;
      if (it.id === selectedId) o.selected = true;
      sel.appendChild(o);
    }
  }

  function refreshPartnerSelects() {
    fillSelect($('chat-wb'), __cs().partner.worldBooks || [], __cs().partner.selectedWorldBookId, '（无世界书）');
    fillSelect($('chat-cc'), __cs().partner.characterCards || [], __cs().partner.selectedCharacterCardId, '（无角色卡）');
    if ($('chat-wb2')) fillSelect($('chat-wb2'), __cs().partner.worldBooks || [], __cs().partner.selectedWorldBookId, '（无世界书）');
    if ($('chat-cc2')) fillSelect($('chat-cc2'), __cs().partner.characterCards || [], __cs().partner.selectedCharacterCardId, '（无角色卡）');
    fillSelect($('cc-wb'), __cs().partner.worldBooks || [], '', '（不关联）');
    refreshStorySelects();
    const wbName = (__cs().partner.worldBooks || []).find((w) => w.id === __cs().partner.selectedWorldBookId)?.name;
    const ccName = (__cs().partner.characterCards || []).find((c) => c.id === __cs().partner.selectedCharacterCardId)?.name;
    if ($('partner-hint')) {
      $('partner-hint').textContent =
        (wbName || ccName)
          ? `当前：${wbName || '—'} / ${ccName || '—'}`
          : '未选角色/世界书（仅基础提示词）';
    }
    if ($('partner-hint2')) {
      $('partner-hint2').textContent =
        (wbName || ccName)
          ? `当前：${wbName || '—'} / ${ccName || '—'}`
          : '未选角色/世界书（仅基础提示词）';
    }
    // A3: empty state — show inline quick actions when no cards/books
    const emptyEl = $('chat-setup-empty');
    if (emptyEl) {
      const noCards = !(__cs().partner.characterCards || []).length && !(__cs().partner.worldBooks || []).length;
      emptyEl.classList.toggle('hidden', !noCards);
    }
  }

  function refreshStorySelects() {
    if ($('story-wb') && $('story-cc')) {
      fillSelect($('story-wb'), __cs().partner.worldBooks || [], __cs().partner.selectedWorldBookId, '（无世界书）');
      fillSelect($('story-cc'), __cs().partner.characterCards || [], __cs().partner.selectedCharacterCardId, '（无角色卡）');
      const wbName = (__cs().partner.worldBooks || []).find((w) => w.id === ($('story-wb').value || __cs().partner.selectedWorldBookId))?.name;
      const ccName = (__cs().partner.characterCards || []).find((c) => c.id === ($('story-cc').value || __cs().partner.selectedCharacterCardId))?.name;
      if ($('story-partner-hint')) {
        $('story-partner-hint').textContent =
          (wbName || ccName)
            ? `当前：${wbName || '—'} / ${ccName || '—'}`
            : '未选角色/世界书（仅 DM 基础提示词）';
      }
    }
    refreshAdventureSelects();
  }

  function refreshAdventureSelects() {
    if (!$('adv-wb') || !$('adv-cc')) return;
    fillSelect($('adv-wb'), __cs().partner.worldBooks || [], __cs().partner.selectedWorldBookId, '（无世界书）');
    fillSelect($('adv-cc'), __cs().partner.characterCards || [], __cs().partner.selectedCharacterCardId, '（无角色卡）');
    const wbName = (__cs().partner.worldBooks || []).find((w) => w.id === ($('adv-wb').value || __cs().partner.selectedWorldBookId))?.name;
    const ccName = (__cs().partner.characterCards || []).find((c) => c.id === ($('adv-cc').value || __cs().partner.selectedCharacterCardId))?.name;
    // wand menu selects mirror the setup selects
    if ($('adv-wb-menu')) {
      $('adv-wb-menu').innerHTML = $('adv-wb').innerHTML;
      $('adv-wb-menu').value = $('adv-wb').value;
    }
    if ($('adv-cc-menu')) {
      $('adv-cc-menu').innerHTML = $('adv-cc').innerHTML;
      $('adv-cc-menu').value = $('adv-cc').value;
    }
    // setup current-config line
    const cur = $('adv-setup-current');
    if (cur) {
      cur.textContent = (wbName || ccName)
        ? `当前配置：${wbName || '—'} / ${ccName || '—'}（直接开玩即可）`
        : '尚未选择世界书/角色卡（可使用默认 DM 提示词）';
    }
    if ($('adv-partner-hint')) {
      $('adv-partner-hint').textContent =
        (wbName || ccName)
          ? `当前：${wbName || '—'} / ${ccName || '—'}`
          : '未选角色/世界书（仅 DM 基础提示词）';
    }
  }


  async function sendMessage(text) {
    if (!text.trim() || __cs().streaming) return;
    const raw = String(text).trim();
    // // 前缀 → 剧情助手弹窗（对话模式共用，独立会话，不混入聊天流）
    if (raw.startsWith('//')) {
      const q = raw.slice(2).trim();
      input.value = '';
      if (window.stOpenAssistModal) window.stOpenAssistModal();
      const inp = $('st-assist-input');
      if (inp) inp.value = q;
      if (window.stFocusAssistInput) window.stFocusAssistInput();
      return;
    }
    const userMsg = { id: uid('u'), role: 'user', content: raw };
    const agentMsg = { id: uid('a'), role: 'assistant', content: '' };
    __cs().messages.push(userMsg, agentMsg);
    renderMessages();
    input.value = '';
    setStreaming(true);
    updateImmersive();

    const modelMessages = __cs().messages.slice(0, -1).map((m) => ({
      id: m.id,
      role: m.role === 'user' ? 'user' : 'assistant',
      content: m.content,
    }));

    const wb = $('chat-wb').value || __cs().partner.selectedWorldBookId || '';
    const cc = $('chat-cc').value || __cs().partner.selectedCharacterCardId || '';

    try {
      const start = await api('/api/mobile/chat/start', {
        method: 'POST',
        body: JSON.stringify({
          agentId: 'partnerChat',
          sessionId: __cs().sessionId || undefined,
          modelInterface: 'OpenAI',
          baseUrl: '',
          apiKey: '',
          model: settings.llmModel || '',
          temperature: settings.temperature != null ? settings.temperature : 0.7,
          maxOutputTokens: settings.maxOutputTokens || 4096,
          systemPrompt: '',
          worldBookId: wb || undefined,
          characterCardId: cc || undefined,
          messages: modelMessages,
        }),
      });
      __cs().activeRunId= start.runId;
      // M-3: use a short-lived one-time SSE ticket instead of the raw token in the URL.
      let url;
      const ticket = getToken() ? await getSseTicket() : '';
      url = apiBase() + '/api/mobile/stream?runId=' + encodeURIComponent(__cs().activeRunId) +
        (ticket ? '&ticket=' + encodeURIComponent(ticket) : '');
      closeEs();
      __cs().es= new EventSource(url);
      __cs().es.onmessage = (ev) => {
        try {
          const payload = JSON.parse(ev.data);
          if (payload.runId && payload.runId !== __cs().activeRunId) return;
          if (payload.eventType === 'delta' && payload.delta) {
            agentMsg.content += payload.delta;
            scheduleChatStreamPaint();
          } else if (payload.eventType === 'error') {
            if (!agentMsg.content) agentMsg.content = '请求失败：' + (payload.message || '');
            renderMessages({ forceScroll: true });
            finishStream();
          } else if (payload.eventType === 'done') {
            const usageEl = document.getElementById('chat-usage');
            if (usageEl && (payload.inputTokens != null || payload.outputTokens != null)) {
              usageEl.textContent = 'tokens ↑' + (payload.inputTokens != null ? payload.inputTokens : '–') +
                ' ↓' + (payload.outputTokens != null ? payload.outputTokens : '–');
              usageEl.hidden = false;
            }
            renderMessages({ forceScroll: true });
            finishStream();
          }
        } catch (e) {
          console.error(e);
        }
      };
      __cs().es.onerror = () => {
        if (__cs().streaming) {
          if (!agentMsg.content) agentMsg.content = '（连接中断）';
          renderMessages({ forceScroll: true });
          finishStream();
        }
      };
    } catch (e) {
      agentMsg.content = '启动失败：' + e.message;
      renderMessages({ forceScroll: true });
      setStreaming(false);
    }
  }

  async function finishStream() {
    closeEs();
    setStreaming(false);
    __cs().activeRunId= null;
    try {
      await saveSession();
      await refreshSessions();
    } catch (e) {
      console.warn('save session', e);
    }
  }

  async function stopStream() {
    if (!__cs().activeRunId) return;
    try {
      await api('/api/mobile/chat/stop', {
        method: 'POST',
        body: JSON.stringify({ run_id: __cs().activeRunId }),
      });
    } catch (_) {}
    finishStream();
  }

  // ---------- S5-W2 T1: Story 跑团 (kind=story, story-session-*) ----------




export {
  renderMessages, refreshSessions, showChatSetup, setupChatStart, ensureSession,
  saveSession, closeEs, setStreaming, scheduleChatStreamPaint,
  cssEscape, buildBubbleEl, fillBubbleBody, ensureBubbleDom,
  refreshPartnerSelects, refreshStorySelects, refreshAdventureSelects,
  sendMessage, stopStream,
};
