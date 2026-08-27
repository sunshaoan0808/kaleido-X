/**
 * src/js/story.js — Story/bond 域真 ESM 模块（P1-3 S2.15；原 _story-part.js）。
 * 出边 5 符号经 Mechanism Y 供 _tabs/_partner 裸引用零编辑。
 * 入边：api/uid/cssEscape/buildBubbleEl import；闭包 lets 经门面访问器：
 *   __s8=__kaleidoStoryState(S2.10 扩展 es/streaming/activeRunId)
 *   __c7=__kaleidoChatState(S2.8) / __t6=__kaleidoTabs(本步扩展 bond picks+immChrome)
 * parseOptionListBlob/parseStoryChoices 本地副本保留（与 utils.js 并存，良性）。
 */
import { api, getSseTicket, getToken } from './api.js';
import { uid, PLAYABLE_LABELS } from './utils.js';
import { cssEscape, buildBubbleEl } from './chat.js';
import { apiBase } from './api_shell.js';

const __s8 = () => window.__kaleidoStoryState;
const __c7 = () => window.__kaleidoChatState;
const __t6 = () => window.__kaleidoTabs;
const __authToken = () => window.__kaleidoAuthState.token;

/* Story/bond */
  // S13: send/stop button uses icons instead of text (16px feather-style)
  const ADV_ICON_SEND = '<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m22 2-7 20-4-9-9-4Z"/><path d="M22 2 11 13"/></svg>';
  // S13c: stop icon matches wand/send line style (stroke, not solid fill)
  const ADV_ICON_STOP = '<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="5.5" y="5.5" width="13" height="13" rx="2"/></svg>';

  function closeStoryEs() {
    if (__s8().es) {
      try { __s8().es.close(); } catch (_) {}
      __s8().es = null;
    }
  }

  function setStoryStreaming(on) {
    __s8().streaming = on;
    document.documentElement.toggleAttribute('data-streaming', !!on);
    // S12/S13: single send/stop button — streaming state decides icon+label
    ['adv-send-btn', 'story-send-btn'].forEach(function (id) {
      const btn = $(id);
      if (!btn) return;
      btn.classList.toggle('danger', on);
      btn.innerHTML = on ? ADV_ICON_STOP : ADV_ICON_SEND;
      btn.setAttribute('data-mode', on ? 'stop' : 'send');
      btn.setAttribute('aria-label', on ? '停止' : '发送');
      btn.title = on ? '停止' : '发送';
    });
    if (!on) {
      ['story-messages', 'adv-messages'].forEach(function (id) {
        const el = $(id);
        if (!el) return;
        el.querySelectorAll('.bubble.is-streaming').forEach(function (n) {
          n.classList.remove('is-streaming');
        });
      });
    }
  }

  /** Strip option protocol blocks from narrative (chips render them separately).
   *  NOTE: avoid /u regex flag — some WebViews throw and break the whole paint path.
   */
  // S2.10: stripChoicesBlock moved to src/js/utils.js (shared with tavern.js)

  function parseOptionListBlob(raw) {
    const t = String(raw || '').trim();
    if (!t) return [];
    const bracket = t.match(/\[[\s\S]*\]/);
    if (bracket) {
      try {
        const arr = JSON.parse(bracket[0]);
        if (Array.isArray(arr)) return arr.map(String).map((x) => x.trim()).filter(Boolean);
      } catch (_) {}
    }
    const out = [];
    for (const line of t.split(/\n+/)) {
      let l = line.trim();
      if (!l) continue;
      l = l.replace(/^[-•*–·]\s+/, '').replace(/^\d+[.)\u3001:：]\s*/, '').replace(/^[（(]\d+[）)]\s*/, '');
      if (l && l !== '[' && l !== ']') out.push(l);
    }
    if (!out.length) {
      const re = /"([^"\\]*(?:\\.[^"\\]*)*)"/g;
      let mm;
      while ((mm = re.exec(t)) !== null) out.push(mm[1]);
    }
    return out;
  }

  function parseStoryChoices(text) {
    if (!text) return [];
    try {
      const s = String(text);
      let m = s.match(/<choices>\s*([\s\S]*?)\s*<\/choices>/i);
      if (m) return parseOptionListBlob(m[1]);
      const optMark = '\u3010\u9009\u9879\u3011';
      const askMark = '\u3010\u8be2\u95ee\u3011';
      const i = s.lastIndexOf(optMark);
      if (i >= 0) return parseOptionListBlob(s.slice(i + optMark.length));
      const k = s.lastIndexOf(askMark);
      if (k >= 0) return parseOptionListBlob(s.slice(k + askMark.length));
      // bare JSON array at end
      const j = s.lastIndexOf('[');
      if (j >= 0) {
        const parsed = parseOptionListBlob(s.slice(j));
        if (parsed.length >= 2 && parsed.length <= 6) return parsed;
      }
      return [];
    } catch (e) {
      console.warn('parseStoryChoices', e);
      return [];
    }
  }

  /** Resolve clickable options for last assistant message. */
  // S2.10: resolveMessageOptions moved to src/js/utils.js (shared with tavern.js)

  function renderStoryChoices(text) {
    // paint both story + adventure choice boxes
    const boxes = [$('story-choices'), $('adv-choices')].filter(Boolean);
    for (const box of boxes) {
      box.innerHTML = '';
      const choices = parseStoryChoices(text || '');
      if (!choices.length) continue;
      for (const c of choices) {
        const btn = document.createElement('button');
        btn.type = 'button';
        // S13h: 冒险(adv-choices)与跑团(story-choices)都用 st-option-chip
        // 复用穿书沉浸态布局(三列均分/左对齐/透明底); 统一三模式选项样式。
        // 不带 ghost/sm: button.ghost.sm 的 padding/radius 会覆盖
        // st-option-chip 沉浸规则。
        btn.className = 'st-option-chip';
        btn.textContent = c;
        btn.onclick = () => {
          const modeEl =
            document.querySelector('input[name="adv-mode"][value="plot"]') ||
            document.querySelector('input[name="story-mode"][value="plot"]');
          if (modeEl) modeEl.checked = true;
          sendStoryMessage(c);
        };
        box.appendChild(btn);
      }
    }
  }

  function paintStoryBubbleList(el, opts) {
    if (!el) return;
    opts = opts || {};
    const list = __s8().messages || [];
    const streamTail = !!(opts.stream && list.length && list[list.length - 1] && list[list.length - 1].role === 'assistant');

    function bodyFor(m) {
      const isAgent = m.role === 'assistant';
      let body = String(m.content || '');
      if (!isAgent) {
        // hide protocol mode prefix (【说话】/【行为】/【剧情推进】) — the LLM needs
        // it in storage, the reader should never see it
        body = body.replace(/^【(说话|行为|剧情推进)】/, '');
      } else {
        // strip <choices> protocol blocks even mid-stream, so raw tags never
        // flash in the narrative (final paint additionally runs regex scripts)
        body = stripChoicesBlock(body);
      }
      if (!(opts.stream && isAgent)) {
        body = applyStRegexScripts(body, isAgent ? 'assistant' : m.role);
      }
      return body;
    }

    // S11: adventure reader is pure-text — no 你/DM role headers. Story page
    // keeps them. opts.noRole suppresses the label.
    const roleFor = opts.noRole
      ? function () { return ''; }
      : function (m) { return m.role === 'user' ? '你' : 'DM'; };

    if (streamTail) {
      const last = list[list.length - 1];
      let node = el.querySelector('.bubble[data-mid="' + cssEscape(last.id) + '"]');
      if (!node) {
        ensureBubbleDom(el, list, {
          roleLabel: roleFor,
          bodyText: bodyFor,
          streamId: last.id,
        });
        // ensureBubbleDom uses agent/user roleClass via role; override labels already set.
        // Re-map roleClass is already user/agent which matches story CSS.
        node = el.querySelector('.bubble[data-mid="' + cssEscape(last.id) + '"]');
      }
      if (node) {
        // story role label for assistant is DM
        const roleEl = node.querySelector('.role');
        if (roleEl && !opts.noRole && last.role !== 'user' && roleEl.textContent !== 'DM') roleEl.textContent = 'DM';
        const body = node.querySelector('.bubble-body');
        const text = bodyFor(last);
        if (body) {
          if (body.textContent !== text) body.textContent = text;
        } else {
          const span = document.createElement('span');
          span.className = 'bubble-body';
          span.textContent = text;
          node.appendChild(span);
        }
        node.classList.add('is-streaming');
        const nearBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 80;
        if (nearBottom) el.scrollTop = el.scrollHeight;
        return;
      }
    }

    const stick = el.scrollHeight - el.scrollTop - el.clientHeight < 80;
    el.innerHTML = '';
    for (let i = 0; i < list.length; i++) {
      const m = list[i];
      const isLastStream = !!(opts.stream && i === list.length - 1 && m.role === 'assistant');
      el.appendChild(buildBubbleEl({
        id: m.id,
        roleClass: m.role === 'user' ? 'user' : 'agent',
        roleLabel: roleFor(m),
        body: bodyFor(m),
        enter: !isLastStream && !opts.quiet,
        streaming: isLastStream,
        ts: m.createdAt || m.ts || '',
        tokens: (m.tokens || (m.usage && m.usage.total_tokens)) || 0,
        monologue: (m.role === 'assistant' && (m._monologue || m.reasoning)) ? (m._monologue || m.reasoning) : null,
      }));
    }
    if (stick || opts.forceScroll) el.scrollTop = el.scrollHeight;
  }

  let storyStreamRaf = 0;
  function scheduleStoryStreamPaint() {
    if (storyStreamRaf) return;
    storyStreamRaf = requestAnimationFrame(function () {
      storyStreamRaf = 0;
      paintStoryBubbleList($('story-messages'), { stream: true });
      paintStoryBubbleList($('adv-messages'), { stream: true, noRole: true });
    });
  }

  function renderStoryMessages(opts) {
    opts = opts || {};
    paintStoryBubbleList($('story-messages'), opts);
    paintStoryBubbleList($('adv-messages'), Object.assign({}, opts, { noRole: true }));
    if (!opts.stream) {
      const lastAgent = [...__s8().messages].reverse().find((m) => m.role === 'assistant');
      renderStoryChoices(lastAgent ? lastAgent.content : '');
    }
  }

  async function refreshStorySessions() {
    const listEl = $('story-session-list');
    if (!listEl) return;
    const list = await api('/api/mobile/sessions?prefix=story-session-');
    listEl.innerHTML = '';
    for (const s of list) {
      const el = document.createElement('div');
      el.className = 'item' + (s.id === __s8().sessionId ? ' active' : '');
      el.innerHTML = '<span class="t"></span><span class="d"></span>';
      el.querySelector('.t').textContent = displayTitle(s.title, '新对话');
      el.querySelector('.d').textContent = formatDateTime(s.savedAt || 0);
      el.onclick = () => loadStorySession(s.id);
      listEl.appendChild(el);
    }
  }

  async function loadStorySession(id) {
    const rec = await api('/api/mobile/sessions/' + encodeURIComponent(id));
    __s8().sessionId = rec.id;
    localStorage.setItem(STORY_SID_KEY, __s8().sessionId);
    __s8().messages = (rec.__c7().messages || []).map((m) => ({
      id: m.id,
      role: m.role === 'user' ? 'user' : 'assistant',
      content: m.content || '',
      createdAt: m.createdAt || m.ts || '',
      tokens: (m.tokens || (m.usage && m.usage.total_tokens)) || 0,
    }));
    renderStoryMessages();
    __t6().updateImmersive();
    await refreshStorySessions();
  }

  async function ensureStorySession() {
    if (__s8().sessionId) {
      try {
        await loadStorySession(__s8().sessionId);
        return;
      } catch (_) {
        __s8().sessionId = '';
      }
    }
    __s8().sessionId = uid('story-session');
    localStorage.setItem(STORY_SID_KEY, __s8().sessionId);
    __s8().messages = [];
    await saveStorySession('新冒险');
    renderStoryMessages();
    __t6().updateImmersive();
    await refreshStorySessions();
  }

  async function saveStorySession(title) {
    if (!__s8().sessionId) return;
    const rec = {
      id: __s8().sessionId,
      title: title || (__s8().messages.find((m) => m.role === 'user') || {}).content?.slice(0, 24) || '新冒险',
      savedAt: Date.now(),
      sessionKind: 'story',
      messages: __s8().messages.map((m) => ({
        id: m.id,
        role: m.role,
        content: m.content,
      })),
      selectedReferenceFiles: [],
      todos: [],
    };
    await api('/api/mobile/sessions', { method: 'POST', body: JSON.stringify(rec) });
  }

  function storyModePrefix() {
    // prefer adventure radios when on adventure tab
    const advChecked = document.querySelector('input[name="adv-mode"]:checked');
    const storyChecked = document.querySelector('input[name="story-mode"]:checked');
    const mode = (currentTab === 'adventure'
      ? (advChecked || storyChecked)
      : (storyChecked || advChecked) || {}).value || 'speak';
    if (mode === 'act') return '【行为】';
    if (mode === 'plot') return '【剧情推进】';
    return '【说话】';
  }

  async function sendStoryMessage(text) {
    if (!text || !String(text).trim() || __s8().streaming) return;
    const raw = String(text).trim();
    // // 前缀 → 剧情助手弹窗（冒险/跑团共用，独立会话，绝不代写剧情、不混入剧情流）
    if (raw.startsWith('//')) {
      const q = raw.slice(2).trim();
      if ($('story-input')) $('story-input').value = '';
      if ($('adv-input')) $('adv-input').value = '';
      if (typeof stOpenAssistModal === 'function') stOpenAssistModal();
      const inp = $('st-assist-input');
      if (inp) inp.value = q;
      if (typeof stFocusAssistInput === 'function') stFocusAssistInput();
      return;
    }
    if (!__s8().sessionId) await ensureStorySession();
    // Avoid double-prefix if user already typed one
    const content = /^【(说话|行为|剧情推进)】/.test(raw) ? raw : (storyModePrefix() + raw);
    const userMsg = { id: uid('su'), role: 'user', content, createdAt: new Date().toISOString() };
    const agentMsg = { id: uid('sa'), role: 'assistant', content: '', createdAt: new Date().toISOString() };
    __s8().messages.push(userMsg, agentMsg);
    renderStoryMessages();
    if ($('story-input')) $('story-input').value = '';
    if ($('adv-input')) $('adv-input').value = '';
    setStoryStreaming(true);
    __t6().updateImmersive();

    const modelMessages = __s8().messages.slice(0, -1).map((m) => ({
      id: m.id,
      role: m.role === 'user' ? 'user' : 'assistant',
      content: m.content,
    }));

    // prefer selects visible on current tab; fall back to partner selection
    const wb =
      (currentTab === 'adventure' && $('adv-wb') && $('adv-wb').value) ||
      ($('story-wb') && $('story-wb').value) ||
      __c7().partner.selectedWorldBookId || '';
    const cc =
      (currentTab === 'adventure' && $('adv-cc') && $('adv-cc').value) ||
      ($('story-cc') && $('story-cc').value) ||
      __c7().partner.selectedCharacterCardId || '';

    await startStoryStream(agentMsg, modelMessages, wb, cc);
  }

  /** POST story/start + SSE wiring. Shared by sendStoryMessage and the
      S13f first-play auto opening (advStartOpening). */
  async function startStoryStream(agentMsg, modelMessages, wb, cc) {
    try {
      const start = await api('/api/v1/story/start', {
        method: 'POST',
        body: JSON.stringify({
          agentId: 'storyAgent',
          sessionId: __s8().sessionId || undefined,
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
      __s8().activeRunId = start.runId;
      // M-3: use a short-lived one-time SSE ticket instead of the raw token in the URL.
      let url;
      const ticket = getToken() ? await getSseTicket() : '';
      url = apiBase() + '/api/v1/story/stream?runId=' + encodeURIComponent(__s8().activeRunId) +
        (ticket ? '&ticket=' + encodeURIComponent(ticket) : '');
      closeStoryEs();
      __s8().es = new EventSource(url);
      __s8().es.onmessage = (ev) => {
        try {
          const payload = JSON.parse(ev.data);
          if (payload.runId && payload.runId !== __s8().activeRunId) return;
          if (payload.eventType === 'delta' && payload.delta) {
            agentMsg.content += payload.delta;
            scheduleStoryStreamPaint();
          } else if (payload.eventType === 'error') {
            if (!agentMsg.content) agentMsg.content = '请求失败：' + (payload.message || '');
            renderStoryMessages({ forceScroll: true });
            finishStoryStream();
          } else if (payload.eventType === 'done') {
            renderStoryMessages({ forceScroll: true });
            finishStoryStream();
          }
        } catch (e) {
          console.error(e);
        }
      };
      __s8().es.onerror = () => {
        if (__s8().streaming) {
          if (!agentMsg.content) agentMsg.content = '（连接中断）';
          renderStoryMessages({ forceScroll: true });
          finishStoryStream();
        }
      };
    } catch (e) {
      agentMsg.content = '启动失败：' + e.message;
      renderStoryMessages({ forceScroll: true });
      setStoryStreaming(false);
    }
  }

  /** S13f: first-play auto opening — the DM streams an opening scene with
      <choices> options into the 正文, so a fresh adventure is never an empty
      wall. A synthetic user instruction drives the turn; only the DM bubble
      lands in the conversation (same as tavern turn-0 openings). */
  async function advStartOpening() {
    if (__s8().streaming) return;
    if (!__s8().sessionId) {
      __s8().sessionId = uid('story-session');
      localStorage.setItem(STORY_SID_KEY, __s8().sessionId);
      __s8().messages = [];
      await saveStorySession('新冒险');
    }
    const agentMsg = { id: uid('sa'), role: 'assistant', content: '' };
    __s8().messages.push(agentMsg);
    renderStoryMessages();
    setStoryStreaming(true);
    __t6().updateImmersive();

    const wb = ($('adv-wb') && $('adv-wb').value) || __c7().partner.selectedWorldBookId || '';
    const cc = ($('adv-cc') && $('adv-cc').value) || __c7().partner.selectedCharacterCardId || '';
    const wbObj = (__c7().partner.worldBooks || []).find((w) => w.id === wb);
    const ccObj = (__c7().partner.characterCards || []).find((c) => c.id === cc);
    const wbName = (wbObj && wbObj.name) || wb || '';
    const ccName = (ccObj && ccObj.name) || cc || '';
    const picks = [wbName && '世界书《' + wbName + '》', ccName && '角色卡《' + ccName + '》'].filter(Boolean).join('、');
    const synth = picks
      ? '（新冒险开局）已选 ' + picks + '。请以 DM 身份为这场冒险撰写 150-250 字的开场白：交代场景环境、时间与氛围、我当前的处境与手头可用的信息；结尾用 <choices> 提供 3 个可选的行动方向。'
      : '（新冒险开局）未指定世界书与角色卡。请以 DM 身份自由创作一个有吸引力的开局场景，150-250 字：交代环境、时间与我的初始处境；结尾用 <choices> 提供 3 个可选的行动方向。';
    const modelMessages = [{ id: uid('su'), role: 'user', content: synth }];
    await startStoryStream(agentMsg, modelMessages, wb || undefined, cc || undefined);
  }

  async function finishStoryStream() {
    closeStoryEs();
    setStoryStreaming(false);
    __s8().activeRunId = null;
    try {
      await saveStorySession();
      await refreshStorySessions();
    } catch (e) {
      console.warn('save story session', e);
    }
    // S11: adventure reader reveals chrome when a stream lands (tail at bottom)
    try { window.dispatchEvent(new CustomEvent('story:stream-end')); } catch (_) {}
  }

  async function stopStoryStream() {
    if (!__s8().activeRunId) return;
    try {
      await api('/api/v1/story/stop', {
        method: 'POST',
        body: JSON.stringify({ run_id: __s8().activeRunId }),
      });
    } catch (_) {}
    finishStoryStream();
  }

  if ($('story-apply-partner')) {
    $('story-apply-partner').onclick = async () => {
      const wb = $('story-wb').value || null;
      const cc = $('story-cc').value || null;
      try {
        await api('/api/v1/partner/select', {
          method: 'POST',
          body: JSON.stringify({ worldBookId: wb, characterCardId: cc }),
        });
        await loadPartner();
        refreshStorySelects();
      } catch (e) {
        if ($('story-partner-hint')) $('story-partner-hint').textContent = e.message;
      }
    };
  }
  if ($('story-new-session')) {
    $('story-new-session').onclick = async () => {
      __s8().sessionId = uid('story-session');
      localStorage.setItem(STORY_SID_KEY, __s8().sessionId);
      __s8().messages = [];
      renderStoryMessages();
      await saveStorySession('新冒险');
      await refreshStorySessions();
    };
  }
  if ($('story-composer')) {
    $('story-composer').onsubmit = (e) => {
      e.preventDefault();
      sendStoryMessage(($('story-input') || {}).value || '');
    };
  }
  if ($('story-input')) {
    $('story-input').addEventListener('keydown', (e) => {
      if (e.key === 'Enter' && !e.shiftKey) {
        e.preventDefault();
        sendStoryMessage($('story-input').value);
      }
    });
  }
  // S12: one state-driven button — click while streaming stops, otherwise sends
  if ($('story-send-btn')) {
    $('story-send-btn').onclick = (e) => {
      e.preventDefault();
      if (__s8().streaming) { stopStoryStream(); return; }
      sendStoryMessage(($('story-input') || {}).value || '');
    };
  }

  // ---------- S7-W1 Bond page ----------
  function renderBondPage() {
    const ccList = $('bond-cc-list');
    const wbList = $('bond-wb-list');
    if (!ccList || !wbList) return;
    if (!__t6().bondPickCc) __t6().bondPickCc = __c7().partner.selectedCharacterCardId || '';
    if (!__t6().bondPickWb) __t6().bondPickWb = __c7().partner.selectedWorldBookId || '';
    ccList.innerHTML = '';
    for (const c of __c7().partner.characterCards || []) {
      const el = document.createElement('div');
      el.className = 'item' + (c.id === __t6().bondPickCc ? ' active' : '');
      el.innerHTML = '<span class="t"></span><span class="d"></span>';
      el.querySelector('.t').textContent = c.name || c.id;
      el.querySelector('.d').textContent = c.worldBookId || c.id;
      el.onclick = () => {
        __t6().bondPickCc = c.id;
        if (c.worldBookId) __t6().bondPickWb = c.worldBookId;
        if ($('bond-mem-name') && c.name) $('bond-mem-name').value = c.name;
        renderBondPage();
      };
      ccList.appendChild(el);
    }
    wbList.innerHTML = '';
    for (const w of __c7().partner.worldBooks || []) {
      const el = document.createElement('div');
      el.className = 'item' + (w.id === __t6().bondPickWb ? ' active' : '');
      el.innerHTML = '<span class="t"></span><span class="d"></span>';
      el.querySelector('.t').textContent = w.name || w.id;
      el.querySelector('.d').textContent = w.id;
      el.onclick = () => {
        __t6().bondPickWb = w.id;
        renderBondPage();
      };
      wbList.appendChild(el);
    }
    const wbName = (__c7().partner.worldBooks || []).find((w) => w.id === __t6().bondPickWb)?.name
      || (__c7().partner.worldBooks || []).find((w) => w.id === __c7().partner.selectedWorldBookId)?.name;
    const ccName = (__c7().partner.characterCards || []).find((c) => c.id === __t6().bondPickCc)?.name
      || (__c7().partner.characterCards || []).find((c) => c.id === __c7().partner.selectedCharacterCardId)?.name;
    if ($('bond-current')) {
      $('bond-current').textContent =
        (wbName || ccName || __t6().bondPickWb || __t6().bondPickCc)
          ? `选中：${wbName || bondPickWb || '—'} / ${ccName || bondPickCc || '—'}`
            + (__c7().partner.selectedCharacterCardId === __t6().bondPickCc && __c7().partner.selectedWorldBookId === __t6().bondPickWb ? '（已应用）' : '')
          : '未选择';
    }
    if ($('bond-selected-hint')) {
      $('bond-selected-hint').textContent = ccName ? ('当前伴侣：' + ccName) : '';
    }
  }

  if ($('bond-refresh')) {
    $('bond-refresh').onclick = async () => {
      try {
        await loadPartner();
        if ($('bond-mem-msg')) $('bond-mem-msg').textContent = '已刷新';
      } catch (e) {
        if ($('bond-mem-msg')) $('bond-mem-msg').textContent = e.message;
      }
    };
  }
  if ($('bond-apply')) {
    $('bond-apply').onclick = async () => {
      try {
        __c7().partner = await api('/api/v1/partner/select', {
          method: 'POST',
          body: JSON.stringify({
            worldBookId: __t6().bondPickWb || '',
            characterCardId: __t6().bondPickCc || '',
          }),
        });
        refreshPartnerSelects();
        renderBondPage();
        if ($('bond-mem-msg')) $('bond-mem-msg').textContent = '已应用选中';
      } catch (e) {
        if ($('bond-mem-msg')) $('bond-mem-msg').textContent = e.message;
      }
    };
  }
  if ($('bond-goto-chat')) {
    $('bond-goto-chat').onclick = () => __t6().switchTab('chat');
  }
  if ($('bond-mem-analyze')) {
    $('bond-mem-analyze').onclick = async () => {
      try {
        const body = {
          characterName: ($('bond-mem-name').value || '').trim() || undefined,
          memory: $('bond-mem-text').value || '',
        };
        const data = await api('/api/v1/partner/analyze-memory', { method: 'POST', body: JSON.stringify(body) });
        setPanel('bond-mem-out', 'bond-mem-msg', data);
      } catch (e) {
        setPanel('bond-mem-out', 'bond-mem-msg', null, e);
      }
    };
  }
  if ($('bond-mem-optimize')) {
    $('bond-mem-optimize').onclick = async () => {
      try {
        const body = {
          characterName: ($('bond-mem-name').value || '').trim() || undefined,
          memory: $('bond-mem-text').value || '',
        };
        const data = await api('/api/v1/partner/optimize-memory', { method: 'POST', body: JSON.stringify(body) });
        setPanel('bond-mem-out', 'bond-mem-msg', data);
      } catch (e) {
        setPanel('bond-mem-out', 'bond-mem-msg', null, e);
      }
    };
  }

  // ---------- S7-W1 Adventure page (shared story session/SSE) ----------
  // S11: first-run gate (#adv-setup) → immersive reader (#adv-read).
  // 冒险页当前显示的是 setup 还是 reader？由 advSetupActive 状态 + 有无会话共同决定。
  let advSetupActive = true;

  function advShowSetup() {
    advSetupActive = true;
    const setup = $('adv-setup');
    const read = $('adv-read');
    if (setup) setup.classList.remove('hidden');
    if (read) read.classList.add('hidden');
    renderAdvSetupCurrent();
  }

  function advShowReader() {
    advSetupActive = false;
    const setup = $('adv-setup');
    const read = $('adv-read');
    if (setup) setup.classList.add('hidden');
    if (read) read.classList.remove('hidden');
  }

  function renderAdvSetupCurrent() {
    const el = $('adv-setup-current');
    if (!el) return;
    const wbName = (__c7().partner.worldBooks || []).find((w) => w.id === __c7().partner.selectedWorldBookId)?.name
      || __c7().partner.selectedWorldBookId || '—';
    const ccName = (__c7().partner.characterCards || []).find((c) => c.id === __c7().partner.selectedCharacterCardId)?.name
      || __c7().partner.selectedCharacterCardId || '—';
    const has = !!__c7().partner.selectedWorldBookId || !!__c7().partner.selectedCharacterCardId;
    el.textContent = has
      ? `当前配置：${wbName} / ${ccName}（直接开玩即可）`
      : '尚未选择世界书/角色卡（可使用默认 DM 提示词）';
  }

  function advSyncWandSelects() {
    // keep setup selects and wand-menu selects in sync with partner selection
    if ($('adv-wb') && $('adv-wb-menu')) {
      $('adv-wb-menu').innerHTML = $('adv-wb').innerHTML;
      $('adv-wb-menu').value = $('adv-wb').value;
    }
    if ($('adv-cc') && $('adv-cc-menu')) {
      $('adv-cc-menu').innerHTML = $('adv-cc').innerHTML;
      $('adv-cc-menu').value = $('adv-cc').value;
    }
  }

  if ($('adv-start-btn')) {
    $('adv-start-btn').onclick = async () => {
      const wb = ($('adv-wb') && $('adv-wb').value) || null;
      const cc = ($('adv-cc') && $('adv-cc').value) || null;
      // 立即切到阅读器，避免 partner/select 返回大数据（世界书全文）时
      // await loadPartner 阻塞界面，用户看到"点了没反应"卡片不消失。
      advShowReader();
      try {
        await api('/api/v1/partner/select', {
          method: 'POST',
          body: JSON.stringify({ worldBookId: wb, characterCardId: cc }),
        });
        // 后台刷新伙伴数据，不阻塞开玩
        loadPartner().then(() => {
          refreshAdventureSelects();
        }).catch(console.warn);
        // fresh session for the adventure
        __s8().sessionId = uid('story-session');
        localStorage.setItem(STORY_SID_KEY, __s8().sessionId);
        __s8().messages = [];
        renderStoryMessages();
        await saveStorySession('新冒险');
        await refreshStorySessions();
        __t6().updateImmersive();
        // S13f: first play auto-opens the scene — the DM streams an opening
        // + <choices> options into the 正文 instead of an empty wall.
        await advStartOpening();
      } catch (e) {
        if ($('adv-setup-hint')) $('adv-setup-hint').textContent = e.message;
      }
    };
  }
  if ($('adv-wand-btn')) {
    $('adv-wand-btn').onclick = (e) => {
      e.preventDefault();
      e.stopPropagation();
      const menu = $('adv-wand-menu');
      if (!menu) return;
      const willOpen = menu.classList.contains('hidden');
      menu.classList.toggle('hidden', !willOpen);
      $('adv-wand-btn').setAttribute('aria-expanded', String(willOpen));
      if (willOpen) advSyncWandSelects();
    };
  }
  if ($('adv-apply-menu')) {
    $('adv-apply-menu').onclick = async () => {
      const wb = ($('adv-wb-menu') && $('adv-wb-menu').value) || null;
      const cc = ($('adv-cc-menu') && $('adv-cc-menu').value) || null;
      // 先关菜单给用户即时反馈，再后台保存配置（loadPartner 返回大数据可能慢）
      const menu = $('adv-wand-menu');
      if (menu) menu.classList.add('hidden');
      if ($('adv-wand-btn')) $('adv-wand-btn').setAttribute('aria-expanded', 'false');
      try {
        await api('/api/v1/partner/select', {
          method: 'POST',
          body: JSON.stringify({ worldBookId: wb, characterCardId: cc }),
        });
        loadPartner().then(() => {
          refreshAdventureSelects();
        }).catch(console.warn);
      } catch (e) {
        if (menu && e.message) menu.setAttribute('data-err', e.message);
      }
    };
  }
  if ($('adv-new-session')) {
    $('adv-new-session').onclick = async () => {
      __s8().sessionId = uid('story-session');
      localStorage.setItem(STORY_SID_KEY, __s8().sessionId);
      __s8().messages = [];
      renderStoryMessages();
      await saveStorySession('新冒险');
      await refreshStorySessions();
    };
  }
  if ($('adv-composer')) {
    $('adv-composer').onsubmit = (e) => {
      e.preventDefault();
      sendStoryMessage(($('adv-input') || {}).value || '');
    };
  }
  // S11: send is a plain button now (not type=submit) — bind click explicitly
  if ($('adv-send-btn')) {
    $('adv-send-btn').onclick = (e) => {
      e.preventDefault();
      sendStoryMessage(($('adv-input') || {}).value || '');
    };
  }
  if ($('adv-input')) {
    $('adv-input').addEventListener('keydown', (e) => {
      if (e.key === 'Enter' && !e.shiftKey) {
        e.preventDefault();
        sendStoryMessage($('adv-input').value);
      }
    });
    // S13b: focus/blur → re-evaluate composer visibility (show while typing,
    // hide when leaving and not at bottom)
    $('adv-input').addEventListener('focus', function () {
      try { if (typeof __t6().stAdvImmChromeState === 'function') __t6().stAdvImmChromeState(); } catch (_) {}
    });
    $('adv-input').addEventListener('blur', function () {
      try { if (typeof __t6().stAdvImmChromeState === 'function') __t6().stAdvImmChromeState(); } catch (_) {}
    });
  }
  // S12: one state-driven button — click while streaming stops, otherwise sends
  if ($('adv-send-btn')) {
    $('adv-send-btn').onclick = (e) => {
      e.preventDefault();
      if (__s8().streaming) { stopStoryStream(); return; }
      sendStoryMessage(($('adv-input') || {}).value || '');
    };
  }

  // ---------- S5 tabs: Jobs / Background / BookTravel / Outline / ST / Agent / Crawler ----------

/* ===== exports consumed by remaining closure parts (Mechanism Y) ===== */
export { advShowReader, advShowSetup, ensureStorySession, renderBondPage, renderStoryMessages };
