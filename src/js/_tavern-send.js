  let stTtsAudio = null, stTtsUrl = '';

  function stWriterQuality() {
    const btn = document.getElementById('st-writer-quality');
    if (btn && btn.dataset && btn.dataset.value) return btn.dataset.value;
    try { const v = localStorage.getItem('st-writer-quality'); if (v) return v; } catch (_) {}
    return 'lite';
  }

  function stTtsSync() {
    const r = $('st-tts-btn'), p = $('st-tts-pause'), s = $('st-tts-stop');
    const has = !!stTtsAudio;
    if (r) r.classList.toggle('hidden', has);
    if (s) s.classList.toggle('hidden', !has);
    if (p) {
      p.classList.toggle('hidden', !has);
      const lab = p.querySelector('.btn-lab');
      if (lab) lab.textContent = (has && stTtsAudio.paused) ? '继续' : '暂停';
    }
  }

  function stTtsPauseToggle() {
    if (!stTtsAudio) return;
    if (stTtsAudio.paused) { stTtsAudio.play().catch(() => {}); stStatus('🔊 播放中'); }
    else { stTtsAudio.pause(); stStatus('⏸ 已暂停'); }
    stTtsSync();
  }

  function stTtsStop() {
    if (!stTtsAudio) return;
    try { stTtsAudio.pause(); } catch (_) {}
    stTtsAudio = null;
    if (stTtsUrl) { URL.revokeObjectURL(stTtsUrl); stTtsUrl = ''; }
    stStatus('');
    stTtsSync();
  }

  /** P3 语音：解析消息发言人 → edge-tts 音色。pack.characters[].voice 优先，否则按名字 hash 稳定选池。 */
  const ST_VOICE_POOL = [
    'zh-CN-XiaoxiaoNeural', // 女·晓晓
    'zh-CN-YunxiNeural',    // 男·云希
    'zh-CN-YunyangNeural',  // 男·云扬
    'zh-CN-XiaoyiNeural',   // 女·晓伊
    'zh-CN-liaoning-XiaobeiNeural', // 东北女·晓北
  ];
  function stVoiceOf(speaker) {
    if (!speaker) return 'zh-CN-XiaoxiaoNeural';
    // pack.characters[].voice 优先（含摘要字段透传）
    const packs = (typeof tavernPacks !== 'undefined' && Array.isArray(tavernPacks)) ? tavernPacks : [];
    for (const pk of packs) {
      const chars = (pk && Array.isArray(pk.characters)) ? pk.characters : [];
      for (const c of chars) {
        if (c && String(c.name || '').trim() === speaker && c.voice && String(c.voice).trim()) {
          return String(c.voice).trim();
        }
      }
    }
    // hash(roleName) 稳定选池（同角色同音色）
    let h = 0;
    for (let i = 0; i < speaker.length; i++) h = (h * 31 + speaker.charCodeAt(i)) >>> 0;
    return ST_VOICE_POOL[h % ST_VOICE_POOL.length];
  }

  /** P3 语音完整版：情绪 → edge-tts 语速（愤怒/惊讶加快，疲惫/悲伤放慢）。 */
  const ST_EMO_RATE = {
    '愤怒': '+25%', '恐惧': '+15%', '惊讶': '+20%', '厌恶': '+10%',
    '疲惫': '-15%', '悲伤': '-20%', '心动': '-10%', '温柔': '-10%', '平静': '+0%',
  };
  function stRateOf(speaker) {
    // 从最后一条消息解析发言人情绪（复用 actorStates）
    if (!speaker || !tavernSession || !tavernSession.actorStates) return '';
    const actors = (tavernSession.actorStates.actors) || {};
    const cid = stCharIdOfLocal(speaker);
    const ent = (cid && actors[cid]) ? actors[cid] : actors[speaker];
    if (!ent || !ent.fields) return '';
    const emo = ent.fields.emotion;
    const v = (emo && typeof emo === 'object' && 'value' in emo) ? emo.value : emo;
    return (v && ST_EMO_RATE[v]) ? ST_EMO_RATE[v] : '';
  }
  function stCharIdOfLocal(name) {
    if (!name) return '';
    const chars = (typeof tavernPacks !== 'undefined' && Array.isArray(tavernPacks)) ? tavernPacks.flatMap(pk => (pk && Array.isArray(pk.characters)) ? pk.characters : []) : [];
    for (const c of chars) if (c && String(c.name || '').trim() === name) return c.id;
    return name;
  }

  async function stSpeak() {
    if (!tavernSession) { stStatus('无会话'); return; }
    const msgs = tavernSession.messages || [];
    let text = '';
    let speaker = '';
    for (let i = msgs.length - 1; i >= 0; i--) {
      if (msgs[i].role === 'assistant' && String(msgs[i].content || '').trim()) {
        text = String(msgs[i].content).trim().slice(0, 500);
        const c = String(msgs[i].content);
        const hit = c.match(/^[【\[]?([^：:\n]{1,12})[】\]]?[：:]/);
        speaker = hit ? hit[1].trim() : '';
        break;
      }
    }
    if (!text) { stStatus('没有可朗读的剧情'); return; }
    if (stTtsAudio) { try { stTtsAudio.pause(); } catch (_) {} stTtsAudio = null; if (stTtsUrl) URL.revokeObjectURL(stTtsUrl); stTtsUrl = ''; }
    const voice = stVoiceOf(speaker);
    const rate = stRateOf(speaker);
    stStatus(speaker ? ('🔊 朗读 ' + speaker + (rate ? '（' + rate + '）' : '') + '…') : '朗读中…');
    const res = await stFetch('/api/v1/kaleido-tools/tts', {
      body: JSON.stringify({ text, voice, rate })
    });
    if (!res.ok) throw new Error('HTTP ' + res.status);
    const blob = await res.blob();
    const url = URL.createObjectURL(blob);
    const a = new Audio(url);
    stTtsAudio = a; stTtsUrl = url;
    a.onended = () => {
      if (stTtsAudio === a) { stTtsAudio = null; stTtsUrl = ''; }
      stStatus('');
      URL.revokeObjectURL(url);
      stTtsSync();
    };
    a.onerror = () => {
      if (stTtsAudio === a) { stTtsAudio = null; stTtsUrl = ''; }
      stStatus('播放失败');
      URL.revokeObjectURL(url);
      stTtsSync();
    };
    a.play().catch(() => {});
    stStatus('🔊 播放中');
    stTtsSync();
  }

  async function stSend(text, sendOpts) {
    sendOpts = sendOpts || {};
    const rawIn = (text == null) ? '' : String(text);
    const isContinue = !!sendOpts.continue;
    // continue may be empty; normal send needs non-empty trim
    if ((!isContinue && !rawIn.trim()) || !tavernSession || tavernSession.packMissing || tavernStreaming) return;
    // 吸收自梨园 assistant-gateway：// 开头 → 剧情助手弹窗（独立会话，绝不代写剧情、不混入主线消息）
    if (!isContinue && rawIn.trim().startsWith('//')) {
      const q = rawIn.trim().slice(2).trim();
      stOpenAssistModal();
      const inp = $('st-assist-input');
      if (inp) inp.value = q;
      if (window.stFocusAssistInput) stFocusAssistInput();
      else if (inp) inp.focus();
      if ($('st-input')) $('st-input').value = '';
      return;
    }
    const payload = isContinue ? rawIn : rawIn.trim();
    if ($('st-input')) $('st-input').value = '';
    stSetComposerBusy(true);
    tavernStreaming = true;
    document.documentElement.setAttribute('data-streaming', '1');
    // S9.21: 流式期间公告读屏 (aria-busy)
    const stMsgsBusy = $('st-messages');
    if (stMsgsBusy) stMsgsBusy.setAttribute('aria-busy', 'true');
    try { stSetImmChromeVisible(true); } catch (_) {}
    stShowLlmIndicator();
    stTavernUserScrolled = false;
    tavernSession.messages = tavernSession.messages || [];
    const userMsg = {
      role: 'user',
      content: isContinue ? (payload || '（续写）') : payload,
      id: uid('u'),
      options: [],
    };
    if (isContinue) userMsg.kind = 'continue';
    tavernSession.messages.push(userMsg);
    const agentMsg = { role: 'assistant', content: '', id: uid('a'), options: [] };
    tavernSession.messages.push(agentMsg);
    stRenderMessages({ forceScroll: false });
    // S8.31: 发送后滚到新消息开头（顶部），生成中保持开头，用户从开头下滑阅读
    try { stScrollToLastMsgTop(); } catch (_) {}
    stRenderOptions([]);
    let controller = null;
    let llmHadError = false;
    try {
      let start;
      const stTurnBody = () => JSON.stringify({ message: isContinue ? '' : payload, quality: stWriterQuality() });
      try {
        start = await stApi('/sessions/' + encodeURIComponent(tavernSession.sessionId) + '/turn', { method: 'POST', body: stTurnBody() });
      } catch (e) {
        // Stuck previous turn: auto-stop then retry once
        const msg = String(e.message || e || '');
        if (/turn in progress|409|Conflict/i.test(msg) || e.status === 409) {
          try {
            const rid = (e.body && e.body.activeRunId) || tavernRunId || (tavernSession && tavernSession.activeRunId) || '';
            await stApi('/sessions/' + encodeURIComponent(tavernSession.sessionId) + '/stop', {
              method: 'POST', body: JSON.stringify({ runId: rid || 'force-unlock' })
            });
          } catch (_) {}
          start = await stApi('/sessions/' + encodeURIComponent(tavernSession.sessionId) + '/turn', { method: 'POST', body: stTurnBody() });
        } else {
          throw e;
        }
      }
      tavernRunId = start.runId;
      controller = new AbortController();
      window.__stController = controller;
      const headers = { Accept: 'text/event-stream' };
      if (token) { headers.Authorization = 'Bearer ' + token; headers['X-Mobile-Token'] = token; }
      const res = await fetch(apiBase() + '/api/v1/story-tavern/sessions/' + encodeURIComponent(tavernSession.sessionId) + '/stream?runId=' + encodeURIComponent(tavernRunId), { headers, cache: 'no-store', signal: controller.signal });
      if (!res.ok) { agentMsg.content = '流式错误 HTTP ' + res.status; stRenderMessages({ forceScroll: true }); return; }
      const reader = res.body.getReader(); const decoder = new TextDecoder('utf-8'); let buf = '';
      while (true) {
        const { value, done } = await reader.read();
        if (done) break;
        buf += decoder.decode(value, { stream: true });
        let idx; while ((idx = buf.indexOf('\n')) >= 0) {
          let line = buf.slice(0, idx); buf = buf.slice(idx + 1);
          if (line.endsWith('\r')) line = line.slice(0, -1);
          if (!line || line.startsWith(':')) continue;
          let data = line.startsWith('data:') ? line.slice(5).trimStart() : line;
          let obj; try { obj = JSON.parse(data); } catch (_) { continue; }
          if (obj.runId && obj.runId !== tavernRunId) continue;
          if (obj.type === 'delta' && obj.delta) {
            if (agentMsg._thinkingOnly) {
              agentMsg.content = '';
              agentMsg._thinkingOnly = false;
            }
            agentMsg.content += obj.delta;
            scheduleStStreamPaint();
          } else if (obj.type === 'thinking_delta' && obj.delta) {
            // 内心独白（Omate 对齐）：累积 thinking 内容供渲染折叠区块；
            // 同时保留原「思考中…」占位提示避免 UI 看起来卡死。
            if (!agentMsg._monologue) agentMsg._monologue = '';
            agentMsg._monologue += obj.delta;
            if (!agentMsg.content || agentMsg._thinkingOnly) {
              agentMsg._thinkingOnly = true;
              const tip = '（思考中…）';
              if (!agentMsg.content) agentMsg.content = tip;
              scheduleStStreamPaint();
            }
          } else if (obj.type === 'done') {
            // Finalize typewriter: mark stream-final for full paragraph formatting
            const stEl = $('st-messages');
            if (stEl) {
              stEl.querySelectorAll('.bubble.is-streaming .bubble-body').forEach(function (b) {
                b.setAttribute('data-stream-final', '1');
                b.removeAttribute('data-stream-base');
              });
              stEl.querySelectorAll('.bubble.is-streaming').forEach(function (b) {
                b.classList.remove('is-streaming');
              });
            }
            break;
          } else if (obj.type === 'error') {
            if (!agentMsg.content || agentMsg._thinkingOnly) {
              agentMsg.content = '请求失败：' + (obj.message || '');
              agentMsg._thinkingOnly = false;
            }
            stRenderMessages({ forceScroll: true });
            break;
          }
        }
      }
    } catch (e) {
      const raw = String((e && e.message) || e || '');
      let tip = raw;
      if (/Failed to fetch|NetworkError|Load failed|network/i.test(raw)) {
        // 断线自愈：不立即报死——后端 worker 仍在跑，结果最终会写入 session。
        // 轮询 session 直到 run 结束，恢复完整内容（切页面/网络抖动不丢输出）。
        const sid = tavernSession && tavernSession.sessionId;
        const hadPartial = !!(agentMsg.content && !agentMsg._thinkingOnly && agentMsg.content !== '（思考中…）');
        let recovered = false;
        try {
          for (let attempt = 0; attempt < 36; attempt++) {
            await new Promise((r) => setTimeout(r, 2500));
            const fresh = await stApi('/sessions/' + encodeURIComponent(sid));
            if (!fresh || !Array.isArray(fresh.messages)) break;
            const runDone = !fresh.activeRunId || fresh.activeRunId !== tavernRunId;
            const lastA = fresh.messages[fresh.messages.length - 1];
            const lastHasContent = lastA && lastA.role === 'assistant' && String(lastA.content || '').trim().length > 0;
            if (lastHasContent && runDone) {
              agentMsg.content = String(lastA.content || '');
              if (lastA.reasoning) agentMsg._monologue = lastA.reasoning;
              agentMsg._thinkingOnly = false;
              recovered = true;
              break;
            }
            if (runDone) break; // run 结束但无内容 = 真失败
          }
        } catch (_) {}
        if (recovered) {
          tip = '网络波动，已自动恢复完整内容';
        } else {
          // 未恢复：区分「后端真失败」与「后端仍在生成（上游慢/池忙）」
          let stillRunning = false;
          try {
            const chk = await stApi('/sessions/' + encodeURIComponent(sid));
            stillRunning = !!(chk && chk.activeRunId === tavernRunId);
          } catch (_) {}
          if (stillRunning) {
            tip = hadPartial
              ? '生成中（上游较慢）：已恢复部分内容，稍后刷新可查看完整结果'
              : '生成中（上游较慢）：断线已重连，稍后刷新可查看结果';
          } else {
            tip = hadPartial
              ? '网络波动：仅恢复部分内容，可点「重试」重新生成'
              : '生成失败：上游繁忙或网络断开，可点「重试」重新生成';
            llmHadError = true;
          }
          agentMsg.content = agentMsg.content || ('错误：' + tip);
        }
        stStatus(tip);
      } else if (/turn in progress/i.test(raw)) {
        tip = '上一回合未结束：已尝试解锁，请再发一次';
        agentMsg.content = agentMsg.content || ('错误：' + tip);
        stStatus(tip);
        llmHadError = true;
      } else {
        agentMsg.content = agentMsg.content || ('错误：' + tip);
        stStatus(tip);
        llmHadError = true;
      }
      stRenderMessages({ forceScroll: true });
    } finally {
      tavernStreaming = false;
      const stMsgsBusyEnd = $('st-messages');
      if (stMsgsBusyEnd) stMsgsBusyEnd.removeAttribute('aria-busy');
      // S8.31: LLM 指示器——正常结束消失；出错变红 3s 后消失
      if (llmHadError) {
        stErrorLlmIndicator();
        window.setTimeout(function () { stHideLlmIndicator(); }, 3000);
      } else {
        stHideLlmIndicator();
      }
      clearStStreamPaint();
      stSetComposerBusy(false);
      try {
        const fresh = await stApi('/sessions/' + encodeURIComponent(tavernSession.sessionId));
        tavernSession = fresh;
        // 内心独白：fresh 重拉覆盖消息对象，把流式累积的 monologue 粘回最后一条 assistant
        if (agentMsg._monologue && fresh && Array.isArray(fresh.messages)) {
          const mArr = fresh.messages;
          for (let i = mArr.length - 1; i >= 0; i--) {
            if (mArr[i] && mArr[i].role === 'assistant') {
              mArr[i]._monologue = agentMsg._monologue;
              break;
            }
          }
        }
        stRenderMessages({ quiet: true });
        // S8.30: 输出完成滚到新消息开头（用户从开头下滑阅读），不再停在底部
        try { stScrollToLastMsgTop(); } catch (_) {}
        // belt: full rebuild must not leave stream chrome
        clearStStreamPaint();
        stRenderOptions();
        stShowImmChrome(); /* auto-show chrome when options appear */
        stRenderFocusBar();
        stRenderRecallBar();
        // P3 自动朗读：开关开启且无用户手动播放中时，朗读最新一条 assistant 消息
        if (localStorage.getItem('stAutoTts') === '1' && !stTtsAudio && !tavernStreaming) {
          stSpeak().catch(() => {});
        }
        stStatus(`${tavernSession.title || '故事馆'} · ${PLAYABLE_LABELS[tavernSession.playable] || ''} · ${PLAY_MODE_LABELS[tavernSession.playMode] || tavernSession.playMode || ''} · node ${tavernSession.nodeId || '?'} · resume ${tavernSession.resumeNodeId || '-'} · ${stTurnLabel(tavernSession.turn || 0)}`);
        stSyncModeToggle();
        // best-effort BGE re-rank against the user turn we just sent
        stRefreshRecallSemantic().catch(() => {});
      } catch (_) {}
    }
  }

  function stSetComposerBusy(busy) {
    try {
      if ($('st-stop')) $('st-stop').classList.toggle('hidden', !busy);
      if ($('st-send')) $('st-send').disabled = !!busy;
      if ($('st-continue')) $('st-continue').disabled = !!busy;
      if ($('st-retry')) $('st-retry').disabled = !!busy;
    } catch (_) {}
  }

  // S8.30: 输出完成后滚动到最后一个 assistant 消息开头（用户从开头下滑阅读），
  // 不再停在底部。流式期间仍跟随（打字机效果），完成时回到开头。
  function stScrollToLastMsgTop() {
    window.requestAnimationFrame(function () {
      window.requestAnimationFrame(function () {
        const el = $('st-messages');
        if (!el) return;
        const bubbles = el.querySelectorAll(
          '.st-bubble:not(.st-user):not(.st-role-user):not(.st-bubble-user)'
        );
        const lastA = bubbles.length ? bubbles[bubbles.length - 1] : null;
        if (!lastA) return;
        window.__stProgrammaticScroll = true;
        // #st-messages 全局 scroll-behavior:smooth 会把赋值变成动画、被后续
        // delta 渲染打断——临时禁用做瞬时定位。
        const prev = el.style.scrollBehavior;
        el.style.scrollBehavior = 'auto';
        el.scrollTop = Math.max(0, lastA.offsetTop - el.offsetTop - 12);
        el.style.scrollBehavior = prev;
        window.setTimeout(function () { window.__stProgrammaticScroll = false; }, 50);
      });
    });
  }

  // S8.31: LLM 运作指示器控制——生成中显示、结束消失、出错变红
  function stShowLlmIndicator() {
    const el = $('st-llm-indicator');
    if (!el) return;
    el.classList.remove('hidden', 'error');
  }
  function stHideLlmIndicator() {
    const el = $('st-llm-indicator');
    if (!el) return;
    el.classList.add('hidden');
    el.classList.remove('error');
  }
  function stErrorLlmIndicator() {
    const el = $('st-llm-indicator');
    if (!el) return;
    el.classList.remove('hidden');
    el.classList.add('error');
  }

  /** AI 续写：空 message turn，不要求输入框有字 */
  function stContinue() {
    if (!tavernSession || tavernSession.packMissing || tavernStreaming) return;
    stSend('', { continue: true });
  }

  /**
   * 重试：截到上一轮用户发言（去掉尾部 assistant + 该 user），PUT 会话后重发。
   * 若最后一条是用户（生成失败），只去掉该 user 再重发。
   */
  async function stRetry() {
    if (!tavernSession || tavernSession.packMissing || tavernStreaming) return;
    const msgs = Array.isArray(tavernSession.messages) ? tavernSession.messages.slice() : [];
    if (!msgs.length) {
      stStatus('还没有可重试的回合');
      return;
    }
    let userText = '';
    let cut = msgs.length;
    const last = msgs[msgs.length - 1];
    if (last && last.role !== 'user') {
      // … user, assistant  → drop both, resend user
      let ui = -1;
      for (let i = msgs.length - 2; i >= 0; i--) {
        if (msgs[i] && msgs[i].role === 'user') { ui = i; break; }
      }
      if (ui < 0) {
        stStatus('找不到上一轮用户发言');
        return;
      }
      userText = String(msgs[ui].content || '');
      const wasContinue = msgs[ui].kind === 'continue' || !userText.trim() || userText === '（续写）';
      cut = ui;
      if (wasContinue) {
        // restore without the pair, then continue
        try {
          stSetComposerBusy(true);
          const next = Object.assign({}, tavernSession, { messages: msgs.slice(0, cut) });
          tavernSession = await stApi('/sessions/' + encodeURIComponent(tavernSession.sessionId), {
            method: 'PUT', body: JSON.stringify(next),
          });
          stRenderMessages({ forceScroll: true, quiet: true });
          stRenderOptions([]);
          stSetComposerBusy(false);
          stSend('', { continue: true });
        } catch (e) {
          stSetComposerBusy(false);
          stStatus('重试失败：' + ((e && e.message) || e));
        }
        return;
      }
    } else if (last && last.role === 'user') {
      userText = String(last.content || '');
      cut = msgs.length - 1;
      if (last.kind === 'continue' || !userText.trim() || userText === '（续写）') {
        try {
          stSetComposerBusy(true);
          const next = Object.assign({}, tavernSession, { messages: msgs.slice(0, cut) });
          tavernSession = await stApi('/sessions/' + encodeURIComponent(tavernSession.sessionId), {
            method: 'PUT', body: JSON.stringify(next),
          });
          stRenderMessages({ forceScroll: true, quiet: true });
          stSetComposerBusy(false);
          stSend('', { continue: true });
        } catch (e) {
          stSetComposerBusy(false);
          stStatus('重试失败：' + ((e && e.message) || e));
        }
        return;
      }
    } else {
      stStatus('没有可重试的内容');
      return;
    }
    try {
      stSetComposerBusy(true);
      const next = Object.assign({}, tavernSession, { messages: msgs.slice(0, cut) });
      tavernSession = await stApi('/sessions/' + encodeURIComponent(tavernSession.sessionId), {
        method: 'PUT', body: JSON.stringify(next),
      });
      stRenderMessages({ forceScroll: true, quiet: true });
      stRenderOptions([]);
      stSetComposerBusy(false);
      await stSend(userText);
    } catch (e) {
      stSetComposerBusy(false);
      stStatus('重试失败：' + ((e && e.message) || e));
    }
  }

  function stStop() {
    if (window.__stController) { try { window.__stController.abort(); } catch (_) {} }
    const rid = tavernRunId || (tavernSession && tavernSession.activeRunId) || 'force-unlock';
    if (tavernSession && tavernSession.sessionId) {
      stApi('/sessions/' + encodeURIComponent(tavernSession.sessionId) + '/stop', {
        method: 'POST', body: JSON.stringify({ runId: rid })
      }).catch(() => {});
    }
    tavernStreaming = false;
    tavernRunId = null;
    const stMsgsBusyStop = $('st-messages');
    if (stMsgsBusyStop) stMsgsBusyStop.removeAttribute('aria-busy');
    clearStStreamPaint();
    stSetComposerBusy(false);
  }

  // ---- P3 语音双工：MediaRecorder 录音 → /asr 转写 → stSend 直发（stVoiceInput 默认关防误触） ----
  let stMediaRec = null;
  let stRecChunks = [];
  let stRecMime = 'audio/webm';

  function stVoiceInputEnabled() {
    return localStorage.getItem('stVoiceInput') === '1';
  }

  function stSyncRecBtn() {
    const btn = $('st-asr-btn');
    if (!btn) return;
    const rec = stMediaRec && stMediaRec.state === 'recording';
    btn.classList.toggle('st-recording', rec);
    btn.classList.toggle('is-on', !rec && stVoiceInputEnabled());
    btn.disabled = !stVoiceInputEnabled() && !rec;
    btn.title = rec ? '录音中：点击停止' : (stVoiceInputEnabled() ? '点击开始录音（转写后直发）' : '语音输入已关闭，先点旁边 🎙 开关');
    const lab = btn.querySelector('.btn-lab');
    if (lab) lab.textContent = rec ? '停止' : '语音';
  }

  async function stToggleRecording() {
    if (stMediaRec && stMediaRec.state === 'recording') {
      stMediaRec.stop();
      return;
    }
    if (!stVoiceInputEnabled()) {
      stStatus('🎙 语音输入未开启：点亮旁边「语音双工」开关再试');
      return;
    }
    if (!navigator.mediaDevices || !navigator.mediaDevices.getUserMedia) {
      stStatus('📴 此环境不支持麦克风录音');
      return;
    }
    try {
      const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
      stRecChunks = [];
      const mime = ['audio/webm;codecs=opus', 'audio/webm', 'audio/mp4', '']
        .find((m) => !m || (window.MediaRecorder && MediaRecorder.isTypeSupported(m))) || '';
      const mr = mime ? new MediaRecorder(stream, { mimeType: mime }) : new MediaRecorder(stream);
      stMediaRec = mr;
      stRecMime = mime || mr.mimeType || 'audio/webm';
      mr.ondataavailable = (e) => { if (e.data && e.data.size) stRecChunks.push(e.data); };
      mr.onstop = () => {
        stream.getTracks().forEach((t) => t.stop());
        stMediaRec = null;
        stSyncRecBtn();
        const blob = new Blob(stRecChunks, { type: stRecMime });
        stRecChunks = [];
        if (!blob.size) { stStatus('🎤 未录到声音'); return; }
        stAsrSend(blob).catch((err) => stStatus('语音转写失败：' + ((err && err.message) || err)));
      };
      mr.onerror = () => {
        stream.getTracks().forEach((t) => t.stop());
        stMediaRec = null;
        stSyncRecBtn();
        stStatus('🎤 录音出错');
      };
      mr.start();
      stStatus('🎤 录音中…再来点一下停止');
      stSyncRecBtn();
    } catch (e) {
      stMediaRec = null;
      stSyncRecBtn();
      stStatus('🚫 麦克风权限被拒，无法录音');
    }
  }

  async function stAsrSend(blob) {
    const token = localStorage.getItem('kaleido_token') || '';
    const base = localStorage.getItem('kaleido_api_base') || '';
    const fd = new FormData();
    const ext = (stRecMime.indexOf('mp4') >= 0) ? 'm4a' : 'webm';
    fd.append('audio', blob, 'kaleido_capture.' + ext);
    stStatus('⏳ 语音转写中（首次约需几十秒加载引擎）…');
    const res = await fetch(base + '/api/v1/kaleido-tools/asr', {
      method: 'POST',
      headers: token ? { 'Authorization': 'Bearer ' + token } : {},
      body: fd,
    });
    if (!res.ok) {
      let msg = 'HTTP ' + res.status;
      try { const j = await res.json(); if (j && j.error) msg = j.error; } catch (_) {}
      throw new Error(msg);
    }
    const j = await res.json();
    const text = (j && typeof j.text === 'string') ? j.text.trim() : '';
    if (!text) throw new Error('转写为空');
    // 语音双工：转写结果直发（自动朗读开关开启时，回合结束即自动朗读回复）
    stStatus('🎤 已听懂：' + (text.length > 22 ? text.slice(0, 22) + '…' : text));
    stSend(text);
  }

  /* ---- S8.26 immersive chrome: tap center to toggle (no scroll show/hide) ---- */
  let stImmChromeBound = false;
  let stImmTapStart = null;

  function stSetImmChromeVisible(on) {
    const root = document.documentElement;
    if (root.getAttribute('data-immersive') !== '1') {
      root.classList.remove('imm-top-hidden');
      root.classList.remove('imm-chrome-hidden');
      return;
    }
    // streaming: force chrome so 停止 is reachable
    if (tavernStreaming) {
      root.classList.remove('imm-top-hidden');
      root.classList.remove('imm-chrome-hidden');
      return;
    }
    const layout = $('st-layout');
    if (layout && layout.classList.contains('st-side-open')) {
      root.classList.remove('imm-top-hidden');
      root.classList.remove('imm-chrome-hidden');
      return;
    }
    // S8.29: scrolling hides top bar AND bottom dock together for full-screen
    // reading; returning to top (or tap) restores both. Wand lives in dock.
    const wantHidden = !on;
    const isHidden = root.classList.contains('imm-chrome-hidden');
    if (wantHidden === isHidden) return;
    root.classList.toggle('imm-chrome-hidden', wantHidden);
    root.classList.remove('imm-top-hidden');
    // showing top shrinks message pane — if already near end, keep tail visible
    if (!wantHidden && isHidden) {
      try { stKeepImmTailVisible(); } catch (_) {}
    }
  }

  function stKeepImmTailVisible() {
    // retained no-op-ish helper for option growth: only nudge if already near end
    const msg = $('st-messages');
    if (!msg) return;
    if (document.documentElement.getAttribute('data-immersive') !== '1') return;
    if (document.documentElement.classList.contains('imm-chrome-hidden')) return;
    const gap = msg.scrollHeight - msg.scrollTop - msg.clientHeight;
    if (gap > 80) return;
    requestAnimationFrame(function () {
      try { msg.scrollTop = msg.scrollHeight; } catch (_) {}
    });
  }

  function stSyncImmChromeFromScroll() {
    // S8.29: scrolling the story hides top bar AND bottom dock for full-screen
    // reading; wand returns with the dock when back at top.
    const msg = $('st-messages');
    if (!msg) return;
    const root = document.documentElement;
    if (root.getAttribute('data-immersive') !== '1') return;
    const y = msg.scrollTop;
    root.classList.toggle('imm-chrome-hidden', y > 24);
    root.classList.remove('imm-top-hidden');
  }

  function stShowImmChrome() {
    stSetImmChromeVisible(true);
  }

  function stArmImmChromeHide() {
    // S8.29: hidden by scroll via imm-chrome-hidden; nothing to arm here.
    document.documentElement.classList.remove('imm-top-hidden');
  }

  function stToggleImmChromeFromTap() {
    if (document.documentElement.getAttribute('data-immersive') !== '1') return;
    if (tavernStreaming) {
      stSetImmChromeVisible(true);
      return;
    }
    const hidden = document.documentElement.classList.contains('imm-chrome-hidden');
    stSetImmChromeVisible(hidden); // if hidden → show; if shown → hide
  }

  function stImmTapTargetOk(t) {
    if (!t || !t.closest) return false;
    // don't toggle when interacting with controls
    if (t.closest('button, a, input, textarea, select, label, .st-option-chip, .st-history-fold, .composer-actions, .st-composer-tools, .imm-bar')) {
      return false;
    }
    return true;
  }

  function stImmTapInCenterBand(clientY, msgEl) {
    if (!msgEl) return false;
    const r = msgEl.getBoundingClientRect();
    if (r.height < 8) return false;
    const y = (clientY - r.top) / r.height;
    // middle band of the stage (not top/bottom chrome edges)
    return y >= 0.22 && y <= 0.78;
  }

  function stBindImmChrome() {
    const msg = $('st-messages');
    if (msg && !msg._immChromeBound) {
      msg.addEventListener('pointerdown', function (e) {
        if (e.button != null && e.button !== 0) return;
        stImmTapStart = {
          x: e.clientX,
          y: e.clientY,
          t: Date.now(),
          ok: stImmTapTargetOk(e.target),
        };
      }, { passive: true });
      msg.addEventListener('pointerup', function (e) {
        const s = stImmTapStart;
        stImmTapStart = null;
        if (!s || !s.ok) return;
        if (e.button != null && e.button !== 0) return;
        const dx = Math.abs((e.clientX || 0) - s.x);
        const dy = Math.abs((e.clientY || 0) - s.y);
        // treat as scroll/drag, not tap
        if (dx > 12 || dy > 12) return;
        if (Date.now() - s.t > 650) return;
        if (!stImmTapInCenterBand(e.clientY, msg)) return;
        stToggleImmChromeFromTap();
      }, { passive: true });
      msg.addEventListener('pointercancel', function () { stImmTapStart = null; }, { passive: true });
      msg._immChromeBound = true;
    }
    // default hidden when binding in immersive
    try { stArmImmChromeHide(); } catch (_) {}
    if (!stImmChromeBound) {
      stImmChromeBound = true;
    }
  }

  // Q2: wizard 默认躺在档案馆 #tab-packs 内；从故事馆(#tab-tavern)进入时把它挂到当前激活 tab 的
  // 滚动容器，否则父级 display:none 导致用户看到"点击没反应"。
  function stMountWizard() {
    const wizView = $('st-view-wizard');
    if (!wizView) return;
    const active = document.querySelector('.tab-panel:not(.hidden)');
    const activeId = active ? active.id : '';
    let host = null;
    if (activeId === 'tab-tavern') {
      host = document.querySelector('#tab-tavern .st-main') || document.querySelector('#tab-tavern');
    } else {
      host = document.querySelector('#tab-packs .st-packs-page') || document.querySelector('#tab-packs');
    }
    if (host && wizView.parentElement !== host) host.appendChild(wizView);
  }

  function stOpenWizard(playable, source) {
    // R1: 记录进入来源，供向导取消/剧场退出按来源返回
    stNavFrom = (source === 'story-entry' || source === 'packs-detail') ? source : '';
    stMountWizard();
    const wiz = $('st-wizard');
    if (wiz) wiz.classList.remove('hidden');
    const wizView = $('st-view-wizard');
    if (wizView) wizView.classList.remove('hidden');
    // 隐藏档案馆的其他视图
    const listview = $('st-packs-listview');
    const packDetail = $('st-view-pack');
    if (listview) listview.classList.add('hidden');
    if (packDetail) packDetail.classList.add('hidden');
    // 故事馆视图也同步
    const entry = $('st-view-entry');
    const play = $('st-view-play');
    if (entry) entry.classList.add('hidden');
    if (play) play.classList.add('hidden');
    if (playable) $('st-wizard-playable').value = playable;
    if (tavernPack && tavernPack.id) {
      const w = $('st-wizard-pack');
      if (w) w.value = tavernPack.id;
    }
    stWizardToggleRole();
    const msg = $('st-wizard-msg'); if (msg) msg.textContent = '';
  }

  function stWizardToggleRole() {
    const role = $('st-wizard-role').value;
    $('st-wizard-isekai').classList.toggle('hidden', role !== 'isekai');
    const packId = $('st-wizard-pack').value;
    const pack = tavernPacks.find(p => p.id === packId);
    const vessel = $('st-wizard-vessel');
    if (!vessel || vessel.tagName !== 'SELECT') return;
    const prev = vessel.value;
    const need = (role === 'supporting' || role === 'protagonist');
    vessel.title = '';
    vessel.innerHTML = '';
    const mkOpt = (label, val) => { const o = document.createElement('option'); o.value = val; o.textContent = label; return o; };
    vessel.appendChild(mkOpt(need ? '（请选择要附体的角色）' : '不附身', ''));
    const chars = (pack && Array.isArray(pack.characters)) ? pack.characters : [];
    const shown = chars.filter((c) => {
      const n = String(c.name || '').trim();
      const r = String(c.role || '').toLowerCase();
      if (r.includes('narrator') || n === '旁白') return false;
      return !!(c.id && n);
    });
    for (const c of shown) vessel.appendChild(mkOpt(c.name + '（' + c.id + '）', c.id));
    if (!shown.length) {
      vessel.appendChild(mkOpt('（本包暂无角色卡）', ''));
      vessel.title = '当前包没有可附体角色，请先在角色卡页导入/确认';
      vessel.value = '';
      return;
    }
    vessel.value = ([...vessel.options].some(o => o.value === prev) ? prev : (need ? shown[0].id : ''));
  }

  async function stCreateSession() {
    const packId = $('st-wizard-pack').value;
    if (!packId) { $('st-wizard-msg').textContent = '请选择 Pack'; return; }
    const playable = $('st-wizard-playable').value;
    // R5: 从作者区/分析页进入时带上 workId，让 U13 的 create 罗盘自动挂载生效
    // （anWorkId() 回退 'default' 时视为无项目，保持原行为）
    const wId = (typeof anWorkId === 'function') ? anWorkId() : '';
    const req = {
      packId,
      playable,
      playMode: $('st-wizard-mode').value,
      userTier: $('st-wizard-tier').value,
      adultConfirmed: !!$('st-wizard-adult').checked,
      workId: (wId && wId !== 'default') ? wId : undefined,
    };
    if (playable === 'P3') {
      const entry = {
        entryRole: $('st-wizard-role').value,
        metaKnowledge: $('st-wizard-meta').value,
        rewriteIntensity: $('st-wizard-rewrite').value,
      };
      const vessel = ($('st-wizard-vessel').value || '').trim();
      if (vessel) entry.vesselCharacterId = vessel;
      if (entry.entryRole === 'isekai') {
        entry.isekai = {};
        const fields = ['name','appearance','cheat','origin'];
        for (const f of fields) entry.isekai[f] = ($('st-wizard-isekai-' + f).value || '').trim();
      }
      req.entry = entry;
    }
    try {
      const s = await stApi('/sessions', { method: 'POST', body: JSON.stringify(req) });
      tavernSession = s;
      $('st-wizard').classList.add('hidden');
      await stLoadSession(s.sessionId);
      $('st-wizard-msg').textContent = '';
    } catch (e) {
      $('st-wizard-msg').textContent = '创建失败：' + e.message;
    }
  }

  // Bind tavern events
  if ($('st-adult-ok')) { $('st-adult-ok').onclick = async () => { await setAdultOk(); $('st-adult-banner').classList.add('hidden'); const l = $('st-layout'); if (l) l.classList.remove('st-gated'); stRefresh(); }; }
  if ($('st-new-session')) { $('st-new-session').onclick = () => stOpenWizard('P3', 'story-entry'); }
  if ($('st-drawer-new-session')) { $('st-drawer-new-session').onclick = () => stOpenWizard('P3', 'story-entry'); }

  // ─── Bookshelf ↔ Story Tavern bridge ───────────────────────────────────────
  let shelfNovels = [];
  let shelfActiveSlug = null;

  // ─── P3-swipe：备选回复切换（对标 SillyTavern swipe 箭头） ──────────────────
  // 备选存在消息对象的 _swipes/_swipeIdx（会话内存态，不落服务端）。
  // 右箭头 → 无新备选时用 reroll 取一条新回复缓存；左箭头 → 回历史备选。
  async function stSwipe(divEl, dir) {
    if (!divEl || !tavernSession) return;
    const mid = divEl.getAttribute('data-mid');
    if (!mid) return;
    const msgs = tavernSession.messages || [];
    const idx = msgs.findIndex(function (m) { return String(m.id || '') === mid; });
    if (idx < 0 || msgs[idx].role === 'user') return;
    const m = msgs[idx];
    if (!m._swipes) m._swipes = [String(m.content || '')];
    if (typeof m._swipeIdx !== 'number') m._swipeIdx = 0;
    const n = m._swipes.length;
    // 右箭头且已是最后一条 → 请求新备选（reroll 变体）
    if (dir > 0 && m._swipeIdx >= n - 1) {
      if (tavernStreaming) { stStatus('流式进行中，稍候再试'); return; }
      try {
        stStatus('生成备选回复…');
        const prev = String(m.content || '');
        const fresh = await stApi('/sessions/' + encodeURIComponent(tavernSession.sessionId) + '/reroll', { method: 'POST', body: '{}' });
        // reroll 返回 {ok, lastUserMessage, turn: <turn数>}——新回复需重拉会话取最后一条 assistant
        let freshText = '';
        if (fresh && fresh.ok) {
          const s2 = await stApi('/sessions/' + encodeURIComponent(tavernSession.sessionId));
          const arr = (s2 && Array.isArray(s2.messages)) ? s2.messages : [];
          for (let i = arr.length - 1; i >= 0; i--) {
            if (arr[i] && arr[i].role === 'assistant' && String(arr[i].content || '').trim()) {
              freshText = String(arr[i].content).trim(); break;
            }
          }
        }
        if (freshText && freshText !== prev) {
          m._swipes.push(freshText);
          m._swipeIdx = m._swipes.length - 1;
        } else {
          stStatus('新备选与当前一致或获取失败');
          return;
        }
      } catch (e) {
        stStatus('备选失败：' + ((e && e.message) || e));
        return;
      }
    } else {
      let ni = m._swipeIdx + dir;
      if (ni < 0) ni = 0;
      if (ni >= n) return;
      m._swipeIdx = ni;
    }
    // 切正文（只换 body 文本，保留角色/角标/程序卡）
    const bodyEl = divEl.querySelector('.bubble-body');
    if (bodyEl && typeof fillBubbleBody === 'function') {
      fillBubbleBody(bodyEl, m._swipes[m._swipeIdx], { speakerMode: true });
    } else if (bodyEl) {
      bodyEl.textContent = m._swipes[m._swipeIdx];
    }
    const cnt = divEl.querySelector('.st-swipe-cnt');
    if (cnt) cnt.textContent = (m._swipeIdx + 1) + '/' + m._swipes.length;
    const sel = stStatus; if (sel) sel('备选 ' + (m._swipeIdx + 1) + '/' + m._swipes.length);
  }
  window.stSwipe = stSwipe;

  // ─── P3-swipe 历史选择器弹窗（PR #5304 风格）：点计数 N/N 弹出全部备选列表 ──
  function stSwipePicker(divEl) {
    if (!divEl || !tavernSession) return;
    const mid = divEl.getAttribute('data-mid');
    if (!mid) return;
    const msgs = tavernSession.messages || [];
    const idx = msgs.findIndex(function (m) { return String(m.id || '') === mid; });
    if (idx < 0) return;
    const m = msgs[idx];
    const swipes = (m._swipes && m._swipes.length) ? m._swipes : [String(m.content || '')];
    const cur = (typeof m._swipeIdx === 'number') ? m._swipeIdx : 0;

    // 复用 .st-modal 弹窗体系
    const modal = document.createElement('div');
    modal.className = 'st-modal';
    modal.id = 'st-swipe-picker';
    const card = document.createElement('div');
    card.className = 'st-modal-card';
    const head = document.createElement('div');
    head.className = 'st-modal-head';
    const title = document.createElement('div');
    title.className = 'st-modal-title'; title.textContent = '备选回复 (' + swipes.length + ')';
    const close = document.createElement('button');
    close.type = 'button'; close.className = 'st-modal-close'; close.setAttribute('aria-label', '关闭');
    close.innerHTML = '&#10005;';
    head.appendChild(title); head.appendChild(close);
    const body = document.createElement('div');
    body.className = 'st-modal-body st-swipe-picker-body';
    swipes.forEach(function (text, i) {
      const item = document.createElement('button');
      item.type = 'button';
      item.className = 'st-swipe-pick' + (i === cur ? ' current' : '');
      const num = document.createElement('span');
      num.className = 'st-swipe-pick-num'; num.textContent = (i + 1) + '/' + swipes.length;
      const txt = document.createElement('span');
      txt.className = 'st-swipe-pick-txt';
      txt.textContent = String(text || '').replace(/\s+/g, ' ').slice(0, 140);
      item.appendChild(num); item.appendChild(txt);
      item.onclick = function (e) {
        e.stopPropagation();
        m._swipeIdx = i;
        // 更新正文
        const bodyEl = divEl.querySelector('.bubble-body');
        if (bodyEl && typeof fillBubbleBody === 'function') {
          fillBubbleBody(bodyEl, m._swipes[m._swipeIdx], { speakerMode: true });
        } else if (bodyEl) {
          bodyEl.textContent = m._swipes[m._swipeIdx];
        }
        const cntEl = divEl.querySelector('.st-swipe-cnt');
        if (cntEl) cntEl.textContent = (m._swipeIdx + 1) + '/' + m._swipes.length;
        closeModal();
        const sel2 = stStatus; if (sel2) sel2('已选备选 ' + (m._swipeIdx + 1) + '/' + m._swipes.length);
      };
      body.appendChild(item);
    });
    card.appendChild(head); card.appendChild(body);
    modal.appendChild(card);
    document.body.appendChild(modal);

    function closeModal() {
      if (modal.parentNode) modal.parentNode.removeChild(modal);
      document.removeEventListener('keydown', onKey);
    }
    function onKey(e) {
      if (e.key === 'Escape') closeModal();
    }
    close.onclick = function (e) { e.stopPropagation(); closeModal(); };
    modal.addEventListener('click', function (e) { if (e.target === modal) closeModal(); });
    document.addEventListener('keydown', onKey);
  }
  window.stSwipePicker = stSwipePicker;

  // ─── 消息操作：编辑/删除（ST 核心交互）──────────────────────────────────────
  async function stEditMessage(divEl) {
    if (!divEl || !tavernSession || tavernStreaming) return;
    const mid = divEl.getAttribute('data-mid');
    if (!mid) return;
    const msgs = tavernSession.messages || [];
    const idx = msgs.findIndex(function (m) { return String(m.id || '') === mid; });
    if (idx < 0) return;
    const m = msgs[idx];
    const bodyEl = divEl.querySelector('.bubble-body');
    const oldText = (bodyEl && bodyEl.textContent) || String(m.content || '');
    const fresh = await showPrompt('编辑消息内容：', { value: oldText });
    if (fresh === null) return;
    if (!String(fresh).trim()) { stStatus('内容不能为空'); return; }
    stApi('/sessions/' + encodeURIComponent(tavernSession.sessionId) + '/messages/' + encodeURIComponent(mid), {
      method: 'PUT', body: JSON.stringify({ content: String(fresh) })
    }).then(function (s) {
      tavernSession = s;
      stRenderMessages({ forceScroll: true, quiet: true });
      stStatus('消息已编辑');
    }).catch(function (e) { stStatus('编辑失败：' + ((e && e.message) || e)); });
  }
  window.stEditMessage = stEditMessage;

  async function stDeleteMessage(divEl) {
    if (!divEl || !tavernSession || tavernStreaming) return;
    const mid = divEl.getAttribute('data-mid');
    if (!mid) return;
    if (!await showConfirm('删除这条消息？')) return;
    stApi('/sessions/' + encodeURIComponent(tavernSession.sessionId) + '/messages/' + encodeURIComponent(mid), {
      method: 'DELETE'
    }).then(function (s) {
      tavernSession = s;
      stRenderMessages({ forceScroll: true, quiet: true });
      stStatus('消息已删除');
    }).catch(function (e) { stStatus('删除失败：' + ((e && e.message) || e)); });
  }
  window.stDeleteMessage = stDeleteMessage;

  // ─── 部分消息编辑（RisuAI 对齐：弹层局部改写，不整条替换）────────────────────
  function stPartialEdit(divEl) {
    if (!divEl || !tavernSession || tavernStreaming) return;
    const mid = divEl.getAttribute('data-mid');
    if (!mid) return;
    const msgs = tavernSession.messages || [];
    const m = msgs.find(function (x) { return String(x.id || '') === mid; });
    if (!m) return;
    const bodyEl = divEl.querySelector('.bubble-body');
    const oldText = (bodyEl && bodyEl.textContent) || String(m.content || '');
    // 弹层
    const overlay = document.createElement('div');
    overlay.className = 'st-modal-overlay';
    overlay.innerHTML =
      '<div class="st-modal st-partial-modal">' +
      '<div class="st-modal-head">部分编辑消息<span class="st-modal-x" title="关闭">✕</span></div>' +
      '<div class="st-partial-hint">修改后保存；仅本消息正文更新，其余消息不变。</div>' +
      '<textarea class="st-partial-text" rows="8" spellcheck="false"></textarea>' +
      '<div class="st-modal-foot"><button type="button" class="ghost st-partial-cancel">取消</button>' +
      '<button type="button" class="st-partial-save">保存</button></div>' +
      '</div>';
    document.body.appendChild(overlay);
    const ta = overlay.querySelector('.st-partial-text');
    ta.value = oldText;
    overlay.querySelector('.st-modal-x').onclick = function () { overlay.remove(); };
    overlay.querySelector('.st-partial-cancel').onclick = function () { overlay.remove(); };
    overlay.addEventListener('click', function (e) { if (e.target === overlay) overlay.remove(); });
    overlay.querySelector('.st-partial-save').onclick = function () {
      const fresh = ta.value;
      if (!String(fresh).trim()) { stStatus('内容不能为空'); return; }
      stApi('/sessions/' + encodeURIComponent(tavernSession.sessionId) + '/messages/' + encodeURIComponent(mid), {
        method: 'PUT', body: JSON.stringify({ content: String(fresh) })
      }).then(function (s) {
        tavernSession = s;
        overlay.remove();
        stRenderMessages({ forceScroll: true, quiet: true });
        stStatus('消息已更新');
      }).catch(function (e) { stStatus('保存失败：' + ((e && e.message) || e)); });
    };
    ta.focus();
    ta.setSelectionRange(0, 0);
  }
  window.stPartialEdit = stPartialEdit;

  // ─── 消息书签（收藏消息 → localStorage → 侧栏面板）──────────────────────────
  function stBookmarkKey() {
    return 'st_bookmarks_v1';
  }
  function stLoadBookmarks() {
    try {
      const raw = localStorage.getItem(stBookmarkKey());
      const arr = raw ? JSON.parse(raw) : [];
      return Array.isArray(arr) ? arr : [];
    } catch (_) { return []; }
  }
  function stSaveBookmarks(arr) {
    try { localStorage.setItem(stBookmarkKey(), JSON.stringify(arr.slice(-200))); } catch (_) {}
  }
  function stToggleBookmark(divEl) {
    if (!divEl || !tavernSession) return;
    const mid = divEl.getAttribute('data-mid');
    if (!mid) return;
    const msgs = tavernSession.messages || [];
    const m = msgs.find(function (x) { return String(x.id || '') === mid; });
    if (!m) return;
    const bodyEl = divEl.querySelector('.bubble-body');
    const text = ((bodyEl && bodyEl.textContent) || String(m.content || '')).trim().slice(0, 120);
    const list = stLoadBookmarks();
    const idx = list.findIndex(function (b) { return b.mid === mid && b.sessionId === tavernSession.sessionId; });
    if (idx >= 0) {
      list.splice(idx, 1);
      stSaveBookmarks(list);
      stStatus('已取消收藏');
    } else {
      list.unshift({
        mid: mid,
        sessionId: tavernSession.sessionId,
        title: tavernSession.title || '会话',
        role: m.role || '',
        text: text,
        ts: Date.now()
      });
      stSaveBookmarks(list);
      stStatus('已收藏消息 ⭐');
    }
    if (typeof stRenderBookmarks === 'function') stRenderBookmarks();
  }
  window.stToggleBookmark = stToggleBookmark;

  /** 渲染侧栏书签区（挂到抽屉 st-char-list 下方；无容器则跳过） */
  function stBmEsc(s) {
    return String(s == null ? '' : s)
      .replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;').replace(/'/g, '&#39;');
  }
  function stRenderBookmarks() {
    var host = $('st-bookmarks');
    if (!host) return;
    var list = stLoadBookmarks();
    if (!list.length) {
      host.innerHTML = '<div class="st-bm-empty">暂无书签——消息操作菜单 ⭐ 收藏重点剧情。</div>';
      return;
    }
    var html = '<div class="st-bm-head">书签 · ' + list.length + '</div><div class="st-bm-list">';
    for (var i = 0; i < list.length; i++) {
      var b = list[i];
      var roleTag = b.role === 'user' ? '你' : 'AI';
      html += '<div class="st-bm-item" data-bm="' + i + '">' +
        '<div class="st-bm-role">' + roleTag + '</div>' +
        '<div class="st-bm-body">' +
        '<div class="st-bm-text">' + stBmEsc(b.text || '') + '</div>' +
        '<div class="st-bm-meta">' + stBmEsc(b.title || '') + ' · ' + new Date(b.ts || Date.now()).toLocaleString() + '</div>' +
        '</div>' +
        '<button type="button" class="st-bm-del" title="删除书签">✕</button>' +
        '</div>';
    }
    html += '</div>';
    host.innerHTML = html;
    // 事件绑定
    var items = host.querySelectorAll('.st-bm-item');
    Array.prototype.forEach.call(items, function (it) {
      it.querySelector('.st-bm-del').onclick = function (e) {
        e.stopPropagation();
        var idx = parseInt(it.getAttribute('data-bm'), 10);
        var arr = stLoadBookmarks();
        arr.splice(idx, 1);
        stSaveBookmarks(arr);
        stRenderBookmarks();
      };
    });
  }
  window.stRenderBookmarks = stRenderBookmarks;

  // ─── 显示设置：宽度滑杆 / 气泡风格 / 时间戳（ST/RisuAI 对齐）─────────────────
  function stApplyChatWidth(pct) {
    const msgs = $('st-messages');
    if (!msgs) return;
    const stage = msgs.closest('.stage-messages') || msgs.parentElement;
    if (!stage) return;
    const p = Math.max(40, Math.min(100, pct || 100));
    // 宽度作用于剧场视图容器（st-view-play），而非消息区父链
    const host = $('st-view-play');
    if (host) {
      host.style.maxWidth = (p === 100) ? '' : p + '%';
      host.style.margin = (p === 100) ? '' : '0 auto';
    }
    const val = $('st-chat-width-val');
    if (val) val.textContent = p + '%';
    const slider = $('st-chat-width');
    if (slider) slider.value = String(p);
    try { localStorage.setItem('stChatWidth', String(p)); } catch (_) {}
  }
  function stApplyBubbleStyle(style) {
    const stage = $('st-view-play') || document.documentElement;
    stage.setAttribute('data-msg-style', style || 'bubble');
    try { localStorage.setItem('stBubbleStyle', style || 'bubble'); } catch (_) {}
    const seg = $('st-bubble-style');
    if (seg) {
      const btns = seg.querySelectorAll('.st-seg-btn');
      Array.prototype.forEach.call(btns, function (b) {
        b.classList.toggle('active', b.getAttribute('data-style') === (style || 'bubble'));
      });
    }
  }
  function stWireDisplaySettings() {
    // 宽度滑杆
    const slider = $('st-chat-width');
    if (slider) {
      const savedW = parseInt(localStorage.getItem('stChatWidth') || '100', 10);
      stApplyChatWidth(isNaN(savedW) ? 100 : savedW);
      slider.oninput = function () { stApplyChatWidth(parseInt(slider.value, 10)); };
    }
    // 气泡风格
    const seg = $('st-bubble-style');
    if (seg) {
      const savedStyle = localStorage.getItem('stBubbleStyle') || 'bubble';
      stApplyBubbleStyle(savedStyle);
      const btns = seg.querySelectorAll('.st-seg-btn');
      Array.prototype.forEach.call(btns, function (b) {
        b.onclick = function () { stApplyBubbleStyle(b.getAttribute('data-style')); };
      });
    }
    // 时间戳开关
    const metaBtn = $('st-msg-meta');
    if (metaBtn) {
      const syncMeta = function () {
        const on = localStorage.getItem('stMsgMeta') === '1';
        metaBtn.dataset.on = on ? '1' : '0';
        metaBtn.classList.toggle('is-on', on);
      };
      metaBtn.onclick = function () {
        const on = localStorage.getItem('stMsgMeta') === '1';
        localStorage.setItem('stMsgMeta', on ? '0' : '1');
        syncMeta();
        stRenderMessages({ quiet: true });
        stStatus(on ? '消息元信息已关闭' : '消息元信息已开启（时间戳 + token）');
      };
      syncMeta();
    }
  }
  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', stWireDisplaySettings);
  } else {
    stWireDisplaySettings();
  }
  window.stApplyChatWidth = stApplyChatWidth;
  window.stApplyBubbleStyle = stApplyBubbleStyle;

