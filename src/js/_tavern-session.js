  // P2-1: 情绪→表情 emoji 映射（agent 气泡右上角角标；情绪缺失时静默无角标）
  const ST_EMOTION_EMOJI = {
    '平静': '', '开心': '😊', '愤怒': '😠', '悲伤': '😢', '害羞': '😳',
    '惊讶': '😲', '恐惧': '😨', '厌恶': '😒', '疲惫': '😪', '心动': '💗',
  };
  // 复用 _chat-part.js speakerMode 正则的「角色名：」头提取
  const ST_SPEAKER_RE = /^[【\[]?([^：:\n]{1,12})[】\]]?[：:]/;
  // P2-1: 提取消息发言人名（under speakerMode 正则），无则空串
  function stSpeakerNameOf(m) {
    const c = (m && m.content) ? String(m.content) : '';
    const hit = c.match(ST_SPEAKER_RE);
    return hit ? hit[1].trim() : '';
  }
  // P2-1: 由发言人名解析 charId——pack.characters(name→id) 优先，fallback 名字本身
  function stCharIdOf(name) {
    if (!name) return '';
    const chars = (tavernPack && Array.isArray(tavernPack.characters)) ? tavernPack.characters : [];
    for (let i = 0; i < chars.length; i++) {
      if (String(chars[i].name || '').trim() === name) return chars[i].id;
    }
    return name;
  }
  /** P2-1 立绘层：从 actorStates.fields.emotion 解析发言人角色的情绪名（缺失/未命中 → 空串）。被 stEmotionEmojiOf 与 stSpriteOf 共用（情绪名对齐 P2-1 枚举）。 */
  function stEmotionOf(m) {
    if (!m || !tavernSession || !tavernSession.actorStates) return '';
    const actors = tavernSession.actorStates.actors || {};
    const cid = stCharIdOf(stSpeakerNameOf(m));
    if (cid) {
      const ent = actors[cid];
      if (ent && ent.fields) {
        const emo = ent.fields.emotion;
        const v = (emo && typeof emo === 'object' && 'value' in emo) ? emo.value : emo;
        if (v !== undefined && v !== null && String(v).trim()) return String(v).trim();
      }
    }
    // 兜底：无发言人前缀时，若仅一个角色有情绪字段则用之（单角色对话）
    let fallback = '';
    for (const k in actors) {
      const ent = actors[k];
      if (!ent || !ent.fields) continue;
      const emo = ent.fields.emotion;
      const v = (emo && typeof emo === 'object' && 'value' in emo) ? emo.value : emo;
      if (v === undefined || v === null || !String(v).trim()) continue;
      if (fallback && fallback !== String(v).trim()) return ''; // 多角色都带情绪且名字对不上 → 静默
      fallback = String(v).trim();
    }
    return fallback;
  }
  // P2-1: 由消息解析其角色情绪 emoji（缺失/未命中 → 空串，前端静默）
  function stEmotionEmojiOf(m) {
    return ST_EMOTION_EMOJI[stEmotionOf(m)] || '';
  }
  // P2-1 立绘层: 由发言人名解析当前立绘图 URL——情绪匹配 expressions，无则回退 avatar，再无则 null（前端静默）。
  function stSpriteOf(roleName) {
    if (!roleName) return null;
    const pack = tavernPack;
    if (!pack || !Array.isArray(pack.characters)) return null;
    const cid = stCharIdOf(roleName);
    const ch = pack.characters.find((c) => c && String(c.id) === String(cid));
    if (!ch) return null;
    const emo = stEmotionOf({ content: roleName + '：→' }); // 以发言人名为传递伪消息
    const exp = (ch.expressions && typeof ch.expressions === 'object') ? ch.expressions : {};
    if (emo && exp[emo]) return String(exp[emo]);
    return (ch.avatar && String(ch.avatar).trim()) ? String(ch.avatar) : null;
  }

  function stLastUserText() {
    const msgs = (tavernSession && tavernSession.messages) || [];
    for (let i = msgs.length - 1; i >= 0; i--) {
      if (msgs[i] && msgs[i].role === 'user' && (msgs[i].content || '').trim()) return msgs[i].content.trim();
    }
    return '';
  }

  function stScoreEvents(events, queryText, queryEmb) {
    const charIds = new Set((tavernSession && tavernSession.presentCharacterIds) || []);
    const curNode = (tavernSession && tavernSession.nodeId) || '';
    const turn = (tavernSession && tavernSession.turn) || 0;
    return (events || []).map((e) => {
      let score = 0;
      if (e.nodeId && e.nodeId === curNode) score += 3;
      for (const a of (e.actors || [])) if (charIds.has(a)) score += 2;
      if ((e.turn || 0) >= turn - 3) score += 1;
      let semantic = 0;
      let mode = 'token';
      if (queryEmb && e.embedding && e.embedding.length) {
        semantic = stCosine(e.embedding, queryEmb);
        mode = 'bge';
        score = score * 0.6 + semantic * 3 * 0.4;
      } else if (queryText) {
        semantic = stTokenOverlap(queryText, (e.kind || '') + ' ' + (e.summary || ''));
        score = score * 0.7 + semantic * 2;
        mode = 'token';
      }
      return { e, score, semantic, mode };
    }).sort((a, b) => b.score - a.score);
  }

  function stRenderRecallBar(opts) {
    const bar = $('st-recall-bar');
    const list = $('st-recall-list');
    const meta = $('st-recall-meta');
    if (!bar || !list) return;
    // 语义记忆召回条仅「播放视图」显示;进入页/列表页保持隐藏(移动端沉浸播放由 CSS 隐藏)
    const play = $('st-view-play');
    const playing = !!(play && !play.classList.contains('hidden'));
    if (!playing || !tavernSession) {
      bar.classList.add('hidden');
      return;
    }
    const events = (((tavernSession.memoryL2 || tavernSession.memory_l2) || {}).events) || [];
    const withEmb = events.filter((e) => e.embedding && e.embedding.length).length;
    if (!events.length) {
      bar.classList.remove('hidden');
      list.innerHTML = '<div class="st-recall-empty muted sm">尚无 L2 事件 — 多聊几轮后会写入语义缓存</div>';
      if (meta) meta.textContent = '0 事件';
      return;
    }
    bar.classList.remove('hidden');
    const queryText = (opts && opts.queryText) || stLastUserText();
    const queryEmb = opts && opts.queryEmb;
    const ranked = stScoreEvents(events, queryText, queryEmb).slice(0, 6);
    if (meta) {
      meta.textContent =
        events.length + ' 事件 · ' + withEmb + ' 已嵌入' +
        (queryEmb ? ' · BGE 重排' : (queryText ? ' · 词面/结构' : ' · 结构排序')) +
        (queryText ? ' · q「' + String(queryText).slice(0, 18) + (queryText.length > 18 ? '…' : '') + '」' : '');
    }
    list.innerHTML = ranked.map((row, i) => {
      const e = row.e;
      const actors = (e.actors || []).slice(0, 3).join(', ');
      const badge = row.mode === 'bge'
        ? ('cos ' + row.semantic.toFixed(2))
        : (row.mode === 'token' ? ('tok ' + row.semantic.toFixed(2)) : ('sc ' + row.score.toFixed(1)));
      return (
        '<div class="st-recall-item" title="' + String(e.summary || '').replace(/"/g, '&quot;') + '">' +
          '<span class="st-recall-rank">#' + (i + 1) + '</span>' +
          '<span class="st-recall-body">' +
            '<span class="st-recall-kind">' + (e.kind || 'event') + '</span>' +
            '<span class="st-recall-sum">' + String(e.summary || '').slice(0, 72) + (String(e.summary || '').length > 72 ? '…' : '') + '</span>' +
            '<span class="st-recall-sub muted sm">t' + (e.turn || '?') + (actors ? ' · ' + actors : '') + (e.nodeId ? ' · ' + e.nodeId : '') + '</span>' +
          '</span>' +
          '<span class="st-recall-score">' + badge + '</span>' +
        '</div>'
      );
    }).join('');
  }

  async function stRefreshRecallSemantic() {
    if (!tavernSession) return;
    const q = stLastUserText();
    if (!q) {
      stRenderRecallBar();
      if ($('st-recall-meta')) $('st-recall-meta').textContent = ((tavernSession.memoryL2 || {}).events || []).length + ' 事件 · 无用户句可查询';
      return;
    }
    if ($('st-recall-refresh')) $('st-recall-refresh').disabled = true;
    try {
      const data = await api('/api/v1/embeddings', {
        method: 'POST',
        body: JSON.stringify({ input: q, model: 'BAAI/bge-small-zh-v1.5' }),
      });
      const emb = (((data || {}).data || [])[0] || {}).embedding || [];
      stRenderRecallBar({ queryText: q, queryEmb: emb.length ? emb : null });
    } catch (e) {
      stRenderRecallBar({ queryText: q });
      if ($('st-recall-meta')) {
        const cur = $('st-recall-meta').textContent || '';
        $('st-recall-meta').textContent = cur + ' · embed失败 ' + (e.message || e);
      }
    } finally {
      if ($('st-recall-refresh')) $('st-recall-refresh').disabled = false;
    }
  }

  if ($('st-recall-refresh')) {
    $('st-recall-refresh').onclick = (e) => {
      e.preventDefault();
      stRefreshRecallSemantic();
    };
  }

  function stRenderMessages(opts) {
    const el = $('st-messages'); if (!el) return;
    opts = opts || {};
    const list = (tavernSession && tavernSession.messages) || [];
    if (!tavernSession || !list.length) {
      el.innerHTML = stEmpty('没有出现对话', tavernSession ? '正在准备开场白…若仍为空可点下方输入或重进会话' : '选择剧本包与玩法，开始一场新的叙事');
      el.scrollTop = 0;
      return;
    }

    function roleClassOf(m) {
      return m.role === 'user' ? 'user' : (m.role === 'narrator' ? 'narrator' : 'agent');
    }
    function roleLabelOf(m) {
      return m.role === 'user' ? '你' : (m.role === 'narrator' ? '旁白' : '故事');
    }
    function bodyOf(m) {
      // Always strip option protocol from bubbles (stream + final). Chips own the choices.
      if (m && (m.kind === 'continue' || (m.role === 'user' && !(String(m.content || '').trim())))) {
        return (m.content && String(m.content).trim()) ? m.content : '（续写）';
      }
      const raw = (m.role === 'user') ? (m.content || '') : stripChoicesBlock(m.content || '');
      // 流式阶段程序卡原文先不显示（闪烁防抖）：最终保存后 program 字段渲染 iframe
      if (opts.stream && m.role !== 'user') {
        return String(raw).replace(/【程序】[\s\S]*?【\/程序】/g, '').trim() || raw;
      }
      const body = applyStRegexScripts(raw, m.role);
      // 纯询问回合（【询问】停笔卡）：无正文但有选项 → 占位卡文本（吸收自梨园 ask_director）
      if (!String(body).trim() && m.role !== 'user' && Array.isArray(m.options) && m.options.length) {
        return '（请选择后续走向）';
      }
      return body;
    }
    function extraClassOf(m) {
      if (m && (m.kind === 'continue' || (m.role === 'user' && !(String(m.content || '').trim())))) return 'is-continue';
      return '';
    }

    // S8.25: fold older than last ST_VISIBLE_TURNS 对话 (user-started rounds)
    const fold = stMessageFoldPlan(list);
    const startIdx = (!stHistoryExpanded && fold.foldUntil > 0) ? fold.foldUntil : 0;

    const streamTail = !!(opts.stream && list.length && list[list.length - 1] && list[list.length - 1].role !== 'user');
    if (streamTail) {
      const last = list[list.length - 1];
      const mid = last.id || ('st-idx-' + (list.length - 1));
      let node = el.querySelector('.bubble[data-mid="' + cssEscape(mid) + '"]');
      if (!node) {
        // append missing bubbles only within visible window (don't re-inflate folded history mid-stream)
        const existing = new Set();
        el.querySelectorAll('.bubble[data-mid]').forEach(function (n) {
          existing.add(n.getAttribute('data-mid'));
        });
        if (el.querySelector('.st-empty')) el.innerHTML = '';
        stEnsureFoldBanner(el, fold);
        for (let i = startIdx; i < list.length; i++) {
          const m = list[i];
          const id = m.id || ('st-idx-' + i);
          if (existing.has(id)) continue;
          const isStream = id === mid;
          const div = buildBubbleEl({
            id: id,
            roleClass: 'st-bubble ' + roleClassOf(m),
            roleLabel: roleLabelOf(m),
            body: bodyOf(m),
            enter: !isStream,
            streaming: isStream,
            extraClass: extraClassOf(m),
            program: m.program || null,
            emotionEmoji: stEmotionEmojiOf(m),
            swipeSupport: m.role !== 'user',
            swipeCount: (m._swipes && m._swipes.length) || 1,
            swipeIdx: (typeof m._swipeIdx === 'number') ? m._swipeIdx : 0,
            ts: m.createdAt || m.ts || '',
            tokens: (m.tokens || (m.usage && m.usage.total_tokens)) || 0,
            monologue: (m.role !== 'user' && (m._monologue || m.reasoning)) ? (m._monologue || m.reasoning) : null,
          });
          el.appendChild(div);
        }
        node = el.querySelector('.bubble[data-mid="' + cssEscape(mid) + '"]');
      }
      if (node) {
        const body = node.querySelector('.bubble-body');
        const text = bodyOf(last);
        if (body) {
          // Typewriter: only append new chars in stream mode instead of full replace
          if (opts.stream && body.getAttribute('data-stream-base') && !body.hasAttribute('data-stream-final')) {
            const cur = body.textContent || '';
            // Only append if text is longer (growing)
            if (text.length > cur.length) {
              const diff = text.slice(cur.length);
              // Append as text node to preserve existing node structure
              body.appendChild(document.createTextNode(diff));
            } else if (text.length < cur.length) {
              // Text shrank (mid-stream correction) — replace fully
              body.textContent = text;
            }
          } else {
            fillBubbleBody(body, text);
            if (opts.stream) body.setAttribute('data-stream-base', '1');
          }
        } else {
          const span = document.createElement('span');
          span.className = 'bubble-body';
          fillBubbleBody(span, text);
          if (opts.stream) span.setAttribute('data-stream-base', '1');
          node.appendChild(span);
        }
        node.classList.add('is-streaming');
        // S8.31: 流式期间不自动跟随滚动——视口停在开头（用户从开头下滑阅读）；
        // 用户手动滚动不受影响。移除原 nearBottom 跟随。
        // P2-1 立绘层：流式每帧同步当前焦点角色立绘（无情绪/无立绘静默降级）
        try { stRenderSprite(); } catch (_) {}
        return;
      }
    }

    const stick = el.scrollHeight - el.scrollTop - el.clientHeight < 96;
    const prevTop = el.scrollTop;
    el.innerHTML = '';
    stEnsureFoldBanner(el, fold);
    for (let i = startIdx; i < list.length; i++) {
      const m = list[i];
      const id = m.id || ('st-idx-' + i);
      const isLastStream = !!(opts.stream && i === list.length - 1 && m.role !== 'user');
      el.appendChild(buildBubbleEl({
        id: id,
        roleClass: 'st-bubble ' + roleClassOf(m),
        roleLabel: roleLabelOf(m),
        body: bodyOf(m),
        enter: !isLastStream && !opts.quiet,
        streaming: isLastStream,
        extraClass: extraClassOf(m),
        program: m.program || null,
        emotionEmoji: stEmotionEmojiOf(m),
        swipeSupport: m.role !== 'user',
        swipeCount: (m._swipes && m._swipes.length) || 1,
        swipeIdx: (typeof m._swipeIdx === 'number') ? m._swipeIdx : 0,
        ts: m.createdAt || m.ts || '',
        tokens: (m.tokens || (m.usage && m.usage.total_tokens)) || 0,
        monologue: (m.role !== 'user' && (m._monologue || m.reasoning)) ? (m._monologue || m.reasoning) : null,
      }));
    }
    if (opts.restoreScroll) {
      window.__stProgrammaticScroll = true;
      stRestoreReadPos(el);
      window.setTimeout(function () { window.__stProgrammaticScroll = false; }, 50);
    } else if (stick || opts.forceScroll) {
      window.__stProgrammaticScroll = true;
      el.scrollTop = el.scrollHeight;
      window.setTimeout(function () { window.__stProgrammaticScroll = false; }, 50);
    } else {
      el.scrollTop = prevTop;
    }
    // Programmatic scrolls (forceScroll/stick/restoreScroll) must NOT hide the
    // chrome: hiding on load leaves no input box and stray taps hit option
    // buttons. Only user-initiated scroll events hide the chrome.
    if (!opts.forceScroll && !opts.stream && !opts.restoreScroll) {
      try { stSyncImmChromeFromScroll(); } catch (_) {}
    }
    try { stBindReadPosSaver(el); } catch (_) {}
    // P2-1 立绘层：消息区重渲染后同步当前焦点角色立绘（无立绘/无情绪静默降级）
    try { stRenderSprite(); } catch (_) {}
  }

  /** Group by user-started rounds; foldUntil = index of first visible message. */
  function stMessageFoldPlan(list) {
    const turnStarts = [];
    for (let i = 0; i < list.length; i++) {
      if (i === 0 || (list[i] && list[i].role === 'user')) turnStarts.push(i);
    }
    const n = turnStarts.length;
    if (n <= ST_VISIBLE_TURNS) {
      return { foldUntil: 0, hiddenTurns: 0, hiddenMsgs: 0, totalTurns: n };
    }
    const foldUntil = turnStarts[n - ST_VISIBLE_TURNS];
    return {
      foldUntil: foldUntil,
      hiddenTurns: n - ST_VISIBLE_TURNS,
      hiddenMsgs: foldUntil,
      totalTurns: n,
    };
  }

  function stEnsureFoldBanner(el, fold) {
    if (!el || !fold) return;
    const existing = el.querySelector('.st-history-fold');
    if (stHistoryExpanded) {
      if (fold.hiddenTurns > 0) {
        // show collapse control at top
        const bar = existing || document.createElement('button');
        bar.type = 'button';
        bar.className = 'st-history-fold st-history-fold-collapse';
        bar.textContent = '收起较早对话（只留最近 ' + ST_VISIBLE_TURNS + ' 轮）';
        bar.onclick = function (e) {
          e.preventDefault();
          stHistoryExpanded = false;
          stRenderMessages({ restoreScroll: false, forceScroll: false, quiet: true });
          // after collapse, jump to bottom of visible window
          const box = $('st-messages');
          if (box) box.scrollTop = box.scrollHeight;
          try { stSyncImmChromeFromScroll(true); } catch (_) {}
        };
        if (!existing) el.insertBefore(bar, el.firstChild);
      } else if (existing) {
        existing.remove();
      }
      return;
    }
    if (fold.foldUntil <= 0) {
      if (existing) existing.remove();
      return;
    }
    const bar = existing || document.createElement('button');
    bar.type = 'button';
    bar.className = 'st-history-fold';
    bar.textContent = '较早对话已折叠 · ' + fold.hiddenTurns + ' 轮 / ' + fold.hiddenMsgs + ' 条 · 点击展开';
    bar.onclick = function (e) {
      e.preventDefault();
      stHistoryExpanded = true;
      const box = $('st-messages');
      const keep = box ? box.scrollHeight - box.scrollTop : 0;
      stRenderMessages({ quiet: true });
      // keep viewport anchored near where user was (bottom of previous visible set)
      if (box) {
        box.scrollTop = Math.max(0, box.scrollHeight - keep);
      }
      try { stSyncImmChromeFromScroll(true); } catch (_) {}
    };
    if (!existing) el.insertBefore(bar, el.firstChild);
    else if (bar.parentElement !== el) el.insertBefore(bar, el.firstChild);
  }

  function stReadPosKey(sid) {
    return ST_READPOS_PREFIX + String(sid || '');
  }

  function stSaveReadPos() {
    try {
      if (!tavernSession || !tavernSession.sessionId) return;
      const el = $('st-messages');
      if (!el) return;
      const max = Math.max(0, el.scrollHeight - el.clientHeight);
      const gap = el.scrollHeight - el.scrollTop - el.clientHeight;
      const nearBot = gap <= 96;
      const payload = {
        top: Math.round(el.scrollTop),
        ratio: max > 0 ? el.scrollTop / max : 1,
        nearBot: nearBot,
        at: Date.now(),
      };
      localStorage.setItem(stReadPosKey(tavernSession.sessionId), JSON.stringify(payload));
    } catch (_) {}
  }

  function stRestoreReadPos(el) {
    el = el || $('st-messages');
    if (!el || !tavernSession || !tavernSession.sessionId) return;
    let raw = null;
    try { raw = localStorage.getItem(stReadPosKey(tavernSession.sessionId)); } catch (_) {}
    const apply = function () {
      try {
        if (!raw) {
          // no history → last-read default = end (not top)
          el.scrollTop = el.scrollHeight;
        } else {
          const o = JSON.parse(raw);
          const max = Math.max(0, el.scrollHeight - el.clientHeight);
          if (o && o.nearBot) {
            el.scrollTop = el.scrollHeight;
          } else if (o && typeof o.ratio === 'number' && max > 0) {
            el.scrollTop = Math.min(max, Math.max(0, o.ratio * max));
          } else if (o && typeof o.top === 'number') {
            el.scrollTop = Math.min(max, Math.max(0, o.top));
          } else {
            el.scrollTop = el.scrollHeight;
          }
        }
      } catch (_) {
        el.scrollTop = el.scrollHeight;
      }
      // Do NOT hide chrome after restoring position: that leaves no input box
      // and stray taps hit option buttons. Show chrome so the composer is
      // reachable on entry; scroll-hide only kicks in on user scroll.
      try { stShowImmChrome(); } catch (_) {}
    };
    // layout after fold/render
    requestAnimationFrame(function () { requestAnimationFrame(apply); });
  }

  function stBindReadPosSaver(el) {
    if (!el || el._stReadPosBound) return;
    let t = 0;
    // Mark user-initiated scrolls (touch drag / wheel / keyboard). Programmatic
    // scrolls (restore/forceScroll/render jump) do NOT mark → chrome stays
    // visible so the input box and options are reachable on entry.
    const markUser = function () { window.__stUserScrolling = true; };
    el.addEventListener('pointerdown', markUser, { passive: true });
    el.addEventListener('touchstart', markUser, { passive: true });
    el.addEventListener('wheel', markUser, { passive: true });
    el.addEventListener('keydown', markUser, { passive: true });
    el.addEventListener('scroll', function () {
      // S8.28: live scroll drives top-bar hide/show — only for user scrolls.
      if (window.__stUserScrolling) {
        try { stSyncImmChromeFromScroll(); } catch (_) {}
        window.__stUserScrolling = false;
      }
      // S8.31: 记录用户真实滚动（程序化滚动有 __stProgrammaticScroll 标志）
      if (!window.__stProgrammaticScroll) {
        stTavernUserScrolled = true;
      }
      if (t) return;
      t = window.setTimeout(function () {
        t = 0;
        stSaveReadPos();
      }, 180);
    }, { passive: true });
    el._stReadPosBound = true;
  }

  let stStreamRaf = 0;
  function scheduleStStreamPaint() {
    if (stStreamRaf) return;
    stStreamRaf = requestAnimationFrame(function () {
      stStreamRaf = 0;
      if (!tavernStreaming) return; // S8.11e: drop late frames after stop/finally
      stRenderMessages({ stream: true });
      // S8.31: 生成中视口保持在文本开头（用户未手动滚动时）；内容增长后校正
      if (!stTavernUserScrolled) {
        try { stScrollToLastMsgTop(); } catch (_) {}
      }
      // live-extract chips once 【选项】 appears in the streaming tail
      try {
        const msgs = (tavernSession && tavernSession.messages) || [];
        const last = msgs.length ? msgs[msgs.length - 1] : null;
        if (last && last.role !== 'user') {
          const live = resolveMessageOptions(last);
          if (live.length) stRenderOptions(live);
        }
      } catch (_) {}
    });
  }

  function clearStStreamPaint() {
    if (stStreamRaf) {
      try { cancelAnimationFrame(stStreamRaf); } catch (_) {}
      stStreamRaf = 0;
    }
    const stEl = $('st-messages');
    if (stEl) stEl.querySelectorAll('.bubble.is-streaming').forEach(function (n) { n.classList.remove('is-streaming'); });
    document.documentElement.removeAttribute('data-streaming');
  }

  function stRenderOptions(opts) {
    const el = $('st-options');
    if (!el) return;
    el.innerHTML = '';
    let source = Array.isArray(opts) ? opts : null;
    if (!source || !source.length) {
      const msgs = (tavernSession && tavernSession.messages) || [];
      let last = null;
      for (let i = msgs.length - 1; i >= 0; i--) {
        if (msgs[i] && msgs[i].role !== 'user') { last = msgs[i]; break; }
      }
      source = resolveMessageOptions(last);
    }
    source = (source || []).map(String).map((s) => s.trim()).filter(Boolean);
    if (!source.length) {
      el.classList.add('is-empty');
      return;
    }
    el.classList.remove('is-empty');
    // ensure play view options row visible
    el.hidden = false;
    el.style.display = '';
    for (const text of source) {
      const chip = document.createElement('button');
      chip.type = 'button';
      chip.className = 'st-option-chip';
      chip.textContent = text;
      chip.onclick = () => {
        if ($('st-input')) $('st-input').value = text;
        stSend(text);
      };
      el.appendChild(chip);
    }
    // S8.23: options can grow dock — keep last lines above it
    try { stKeepImmTailVisible(); } catch (_) {}
  }

  let stActivePanelName = '';

  /** Agent 自建面板渲染（吸收自梨园 panels.ts）：markdown/svg/html 三档 + 页签。target 指定渲染容器（缺省不渲染，仅清空主输入区 #st-panels —— 面板只在可视化弹窗 #st-visual-body 展示）。 */
  function stRenderPanels(target) {
    if (!target) {
      const p = $('st-panels');
      if (p) p.innerHTML = '';
      return;
    }
    const el = target;
    if (!el) return;
    el.innerHTML = '';
    const panels = (tavernSession && Array.isArray(tavernSession.panels)) ? tavernSession.panels : [];
    if (!panels.length) {
      el.innerHTML = (target && target.id === 'st-visual-body')
        ? '<div class="st-panels-empty">还没有可视化面板。点「让助手生成可视化」，或在剧情中让模型输出【面板】块。</div>'
        : '<div class="st-panels-empty">AI 可在剧情中生成可视化面板（地图/装备栏/线索板），出现后自动显示于此</div>';
      return;
    }
    if (!stActivePanelName || !panels.some(p => p.name === stActivePanelName)) {
      stActivePanelName = panels[0].name;
    }
    const tabs = document.createElement('div');
    tabs.className = 'st-panels-tabs';
    for (const p of panels) {
      const t = document.createElement('button');
      t.type = 'button';
      t.className = 'st-panel-tab' + (p.name === stActivePanelName ? ' active' : '');
      t.textContent = p.name;
      t.onclick = () => { stActivePanelName = p.name; stRenderPanels(target); };
      tabs.appendChild(t);
    }
    el.appendChild(tabs);
    const cur = panels.find(p => p.name === stActivePanelName) || panels[0];
    const body = document.createElement('div');
    body.className = 'st-panel-body';
    if (cur.kind === 'eventbook') {
      // 事件书（Omate 对齐）：剧情链状态追踪——解锁/完成/条件
      const wrap = document.createElement('div');
      wrap.className = 'st-eventbook';
      let events = [];
      try {
        const parsed = JSON.parse(cur.content);
        events = Array.isArray(parsed) ? parsed : (Array.isArray(parsed.events) ? parsed.events : []);
      } catch (_) {
        // 非 JSON 降级：按行渲染为纯文本事件列表
        events = String(cur.content).split('\n').filter(function (l) { return l.trim(); }).map(function (l) {
          return { title: l.replace(/^[-*]\s*/, '').replace(/^\[[ xX]\]\s*/, '').trim(), done: /^\[[xX]\]/.test(l.trim()) };
        });
      }
      if (!events.length) {
        wrap.innerHTML = '<div class="st-panels-empty">事件书为空——让助手生成事件链，或剧情中输出【事件书】块。</div>';
      } else {
        const list = document.createElement('ol');
        list.className = 'st-eventbook-list';
        for (const ev of events) {
          const li = document.createElement('li');
          li.className = 'st-eventbook-item' + (ev.done ? ' done' : '');
          const mark = document.createElement('span');
          mark.className = 'st-eventbook-mark';
          mark.textContent = ev.done ? '✓' : '○';
          const info = document.createElement('div');
          info.className = 'st-eventbook-info';
          const title = document.createElement('div');
          title.className = 'st-eventbook-title';
          title.textContent = ev.title || '（未命名事件）';
          info.appendChild(title);
          if (ev.desc) {
            const desc = document.createElement('div');
            desc.className = 'st-eventbook-desc';
            desc.textContent = ev.desc;
            info.appendChild(desc);
          }
          if (ev.cond && !ev.done) {
            const cond = document.createElement('div');
            cond.className = 'st-eventbook-cond';
            cond.textContent = '条件：' + ev.cond;
            info.appendChild(cond);
          }
          li.appendChild(mark); li.appendChild(info);
          list.appendChild(li);
        }
        wrap.appendChild(list);
      }
      body.appendChild(wrap);
    } else if (cur.kind === 'svg') {
      const wrap = document.createElement('div');
      wrap.className = 'st-panel-svg';
      // 清洗加固 (audit P1#8)：双/单引号 on* 处理器 + xlink:href + javascript: href 全剥
      const safe = String(cur.content)
        .replace(/<script[\s\S]*?<\/script>/gi, '')
        .replace(/\son\w+\s*=\s*(?:"[^"]*"|'[^']*'|[^\s>]*)/gi, '')
        .replace(/\sxlink:href\s*=\s*(?:"[^"]*"|'[^']*'|[^\s>]*)/gi, '')
        .replace(/\shref\s*=\s*"javascript:[^"]*"/gi, '')
        .replace(/\shref\s*=\s*'javascript:[^']*'/gi, '');
      wrap.innerHTML = safe;
      body.appendChild(wrap);
    } else if (cur.kind === 'html') {
      const frame = document.createElement('iframe');
      // audit P0#1：去掉 allow-same-origin —— 同源会使沙箱内脚本读取父页 token/调用同源 API
      frame.setAttribute('sandbox', 'allow-scripts');
      frame.setAttribute('title', cur.name);
      frame.srcdoc = String(cur.content);
      body.appendChild(frame);
    } else {
      body.textContent = cur.content;
    }
    el.appendChild(body);
  }

  /** 可视化面板弹窗：打开时以 #st-visual-body 为目标渲染现有 panels。 */
  function stOpenVisualModal() {
    const m = $('st-visual-modal');
    if (!m) return;
    m.classList.remove('hidden');
    stRenderPanels($('st-visual-body'));
  }

  function stCloseVisualModal() {
    const m = $('st-visual-modal');
    if (m) m.classList.add('hidden');
  }

  /** 让助手生成可视化面板：走 assistant 端点（服务端把【面板】回写 session.panels），客户端刷新后重渲染。 */
  async function stGenVisual() {
    const btn = $('st-visual-gen');
    if (!btn || btn.disabled) return;
    btn.disabled = true;
    const old = btn.textContent;
    btn.textContent = '生成中…';
    try {
      const sid = tavernSession && tavernSession.sessionId;
      if (!sid) { stStatus('无会话'); return; }
      const data = await stApi('/sessions/' + encodeURIComponent(sid) + '/assistant', {
        method: 'POST', body: JSON.stringify({ message: '请生成当前剧情的可视化面板：地图、线索图谱、线索板、装备栏（用【面板】JSON 块输出，kind 用 markdown/svg/html）' })
      });
      // 服务端已把【面板】回写 session.panels，客户端直接刷新
      if (tavernSession) {
        try {
          const s = await stApi('/sessions/' + encodeURIComponent(sid));
          if (s && s.panels) tavernSession.panels = s.panels;
        } catch (_) {}
      }
      stRenderPanels($('st-visual-body'));
      stRenderPanels();
      if (data && data.reply && data.reply.trim()) stStatus(data.reply.trim().slice(0, 80));
    } catch (e) {
      stStatus('可视化生成失败：' + (e && e.message || e));
    } finally {
      btn.disabled = false;
      btn.textContent = old;
    }
  }

  /** 剧情助手弹窗：独立会话、localStorage 历史，不混入主线聊天流。 */
  const ST_ASSIST_KEY_PREFIX = 'kaleido_assist_';
  const ST_ASSIST_MAX = 200;

  function stAssistHistory(sid) {
    try {
      const raw = localStorage.getItem(ST_ASSIST_KEY_PREFIX + String(sid || ''));
      if (!raw) return [];
      const arr = JSON.parse(raw);
      return Array.isArray(arr) ? arr.slice(-ST_ASSIST_MAX) : [];
    } catch (_) { return []; }
  }

  function stAssistSave(sid, history) {
    try {
      const arr = (history || []).slice(-ST_ASSIST_MAX);
      localStorage.setItem(ST_ASSIST_KEY_PREFIX + String(sid || ''), JSON.stringify(arr));
    } catch (_) {}
  }

  function stRenderAssist(history) {
    const body = $('st-assist-body');
    if (!body) return;
    body.innerHTML = '';
    const msgs = (history && history.length) ? history : null;
    if (!msgs) {
      const empty = document.createElement('div');
      empty.className = 'st-assist-msg agent';
      empty.textContent = '问助手：当前剧情状态？线索梳理？（多轮记忆已生效）';
      body.appendChild(empty);
    } else {
      for (const m of msgs) {
        const div = document.createElement('div');
        div.className = 'st-assist-msg ' + (m.role === 'user' ? 'user' : 'agent');
        div.textContent = m.content;
        body.appendChild(div);
      }
    }
    body.scrollTop = body.scrollHeight;
  }

  function stFocusAssistInput() {
    const inp = $('st-assist-input');
    if (!inp) return;
    inp.focus();
    try { inp.scrollIntoView({ behavior: 'smooth', block: 'nearest' }); } catch (_) {}
  }

  function stOpenAssistModal() {
    const m = $('st-assist-modal');
    if (!m) return;
    m.classList.remove('hidden');
    const storyMode = currentTab === 'story' || currentTab === 'adventure' || currentTab === 'chat';
    // story/冒险/跑团/对话无 tavern 会话：reroll/rewind 是剧场专属，隐藏工具行
    const toolsRow = document.querySelector('.st-assist-tools');
    if (toolsRow) toolsRow.style.display = storyMode ? 'none' : '';
    const sid = storyMode
      ? (currentTab === 'chat' ? (sessionId || '') : (storySessionId || ''))
      : (tavernSession && tavernSession.sessionId);
    stRenderAssist(stAssistHistory(sid));
    stFocusAssistInput();
  }

  function stCloseAssistModal() {
    const m = $('st-assist-modal');
    if (m) m.classList.add('hidden');
    const inp = $('st-assist-input');
    if (inp) inp.value = '';
  }

  /** 解析当前 story/chat 模式的 wb/cc 选择 → 世界书 id 列表（cc 取其关联 worldBookId，去重）。 */
  function storyWbIds() {
    const ids = [];
    const wbSel = currentTab === 'chat' ? $('chat-wb') : (currentTab === 'adventure' ? $('adv-wb') : $('story-wb'));
    if (wbSel && wbSel.value) ids.push(wbSel.value);
    const ccSel = currentTab === 'chat' ? $('chat-cc') : (currentTab === 'adventure' ? $('adv-cc') : $('story-cc'));
    if (ccSel && ccSel.value && typeof partner !== 'undefined' && partner.characterCards) {
      const cc = partner.characterCards.find((c) => c.id === ccSel.value);
      if (cc && cc.worldBookId) ids.push(cc.worldBookId);
    }
    return ids.filter((v, i, a) => a.indexOf(v) === i);
  }

  /** 发送助手消息：先落本地历史，再调 assistant 端点，回复追加渲染；不写入 tavernSession.messages。 */
  async function stSendAssist() {
    const inp = $('st-assist-input');
    const btn = $('st-assist-send');
    if (!inp) return;
    const text = inp.value.trim();
    if (!text) return;
    const storyMode = currentTab === 'story' || currentTab === 'adventure' || currentTab === 'chat';
    const sid = storyMode
      ? (currentTab === 'chat' ? (sessionId || '') : (storySessionId || ''))
      : (tavernSession && tavernSession.sessionId);
    inp.value = '';
    if (btn) { btn.disabled = true; btn.textContent = '…'; }
    const history = stAssistHistory(sid);
    history.push({ role: 'user', content: text });
    stAssistSave(sid, history);
    stRenderAssist(history);
    try {
      if (!sid) throw new Error('当前无会话');
      // 带助手对话历史（剔除刚 push 的最后一条 user——那是本次 message）
      const hist = history.slice(0, -1).map((m) => ({ role: m.role, content: String(m.content || '') }));
      // story/冒险/跑团/对话：上下文来自前端本地消息；tavern：服务端会话注入剧情上下文
      const ctxMessages = currentTab === 'chat'
        ? (typeof messages !== 'undefined' ? messages : [])
        : (typeof storyMessages !== 'undefined' ? storyMessages : []);
      const data = storyMode
        ? await api('/api/v1/story/assistant', {
            method: 'POST', body: JSON.stringify({
              message: text,
              history: hist,
              title: '',
              kind: currentTab === 'chat' ? 'chat' : 'story',
              worldBookIds: storyWbIds(),
              messages: ctxMessages.slice(-10).map((m) => ({ role: m.role, content: String(m.content || '') }))
            })
          })
        : await stApi('/sessions/' + encodeURIComponent(sid) + '/assistant', {
            method: 'POST', body: JSON.stringify({ message: text, history: hist })
          });
      const reply = (data && data.reply)
        ? data.reply
        : ('（无回复：' + ((data && data.error) || '未知错误') + '）');
      history.push({ role: 'agent', content: reply });
    } catch (e) {
      history.push({ role: 'agent', content: '请求失败：' + (e && e.message || e) });
    }
    stAssistSave(sid, history);
    stRenderAssist(history);
    if (btn) { btn.disabled = false; btn.textContent = '发送'; }
    stFocusAssistInput();
  }

  /** 重生成：调 reroll 端点回退 1 回合，成功且有 lastUserMessage 时复用 stSend 重发，然后关闭助手弹窗。 */
  async function stRerollLast() {
    const sid = tavernSession && tavernSession.sessionId;
    if (!sid) { stStatus('当前无会话'); return; }
    const btn = $('st-assist-reroll');
    if (btn) btn.disabled = true;
    try {
      const data = await stApi('/sessions/' + encodeURIComponent(sid) + '/reroll', { method: 'POST', body: '{}' });
      const text = (data && data.lastUserMessage) ? String(data.lastUserMessage) : '';
      stCloseAssistModal();
      if (text && typeof stSend === 'function') {
        await stSend(text);
        stStatus('已重生成上一条回复');
      } else {
        stStatus('已回退 1 回合（无上一条用户消息可重发）');
      }
    } catch (e) {
      stStatus('重生成失败：' + ((e && e.message) || e));
    } finally {
      if (btn) btn.disabled = false;
    }
  }

  /** 回退：调 rewind 端点回退 1 回合，成功后刷新会话渲染。 */
  async function stRewindOne() {
    const sid = tavernSession && tavernSession.sessionId;
    if (!sid) { stStatus('当前无会话'); return; }
    const btn = $('st-assist-rewind');
    if (btn) btn.disabled = true;
    try {
      await stApi('/sessions/' + encodeURIComponent(sid) + '/rewind', { method: 'POST', body: JSON.stringify({ steps: 1 }) });
      if (typeof stLoadSession === 'function') {
        await stLoadSession(sid);
      } else {
        location.reload();
      }
      stStatus('已回退 1 回合');
    } catch (e) {
      stStatus('回退失败：' + ((e && e.message) || e));
    } finally {
      if (btn) btn.disabled = false;
    }
  }

  /** 通用 fetch（带 token；用于 kaleido-tools 生图/TTS 端点）。 */
  function stFetch(path, opts = {}) {
    const token = localStorage.getItem('kaleido_token') || '';
    const base = localStorage.getItem('kaleido_api_base') || '';
    return fetch(base + path, {
      method: opts.method || 'POST',
      headers: {
        'Content-Type': 'application/json',
        ...(token ? { 'Authorization': 'Bearer ' + token } : {})
      },
      body: opts.body
    });
  }

  /** 生图（通道可切换：uniapi cogview-4 / cf-manager flux / grok2api grok-imagine）：取最后一条剧情 → 生成插图 → 浮层显示。 */
  async function stGenerateImage() {
    if (!tavernSession) { stStatus('无会话'); return; }
    const msgs = tavernSession.messages || [];
    let scene = '';
    for (let i = msgs.length - 1; i >= 0; i--) {
      if (msgs[i].role === 'assistant' && String(msgs[i].content || '').trim()) {
        scene = String(msgs[i].content).trim().slice(0, 120);
        break;
      }
    }
    if (!scene) scene = '当前场景';
    const chSel = $('st-image-channel');
    const channel = chSel ? (chSel.dataset.value || 'uniapi') : 'uniapi';
    stStatus('生图中…');
    const res = await stFetch('/api/v1/kaleido-tools/image', {
      body: JSON.stringify({ prompt: '动漫电影感插画，' + scene, channel })
    });
    if (!res.ok) throw new Error('HTTP ' + res.status);
    const data = await res.json();
    if (data && (data.url || data.b64)) {
      stShowImage(data.url || ('data:image/jpeg;base64,' + data.b64));
      stStatus('插图已生成（' + (data.channel || '') + '）');
    } else {
      throw new Error((data && data.error) || '生图无返回');
    }
  }

  /** P2-1c: 从会话原文提取角色形象片段（立绘 prompt 兜底——主路径是角色卡 gender/appearance）。
   *  性别不做原文推断：多视角叙事里角色名附近的「女/女人/她」常指他人（实测 6 个女词全指母亲），
   *  赌错比不赌更糟；性别只认角色卡字段（手动蒸馏/蒸馏补全写入）。形象取含外貌词的原文片段。 */
  function stSpriteHintsOf(name) {
    const msgs = (tavernSession && tavernSession.messages) || [];
    const pool = [];
    for (let i = 0; i < msgs.length; i++) {
      const c = String((msgs[i] && msgs[i].content) || '');
      let from = 0, idx;
      while ((idx = c.indexOf(name, from)) >= 0) {
        pool.push(c.slice(idx, idx + 90));
        from = idx + name.length;
      }
    }
    let look = '';
    const lookRe = /(肩膀|肌肉|头发|眼睛|个子|身形|穿着|外套|运动服|衬衫|眼镜|短发|长发|身高|清瘦|结实|少年感|西装|校服)/;
    for (const seg of pool) {
      const m = seg.match(lookRe);
      if (m) { look = seg.replace(/\s+/g, ' ').trim().slice(0, 70); break; }
    }
    return { look };
  }

  /** P2-1 立绘层：按角色+情绪生成立绘（复用 /api/v1/kaleido-tools/image，不新建渠道）并把 URL 写回 pack.characters[].expressions[emotion]（按钮触发，不做批量）。 */
  async function stGenerateSprite() {
    const sid = tavernSession && tavernSession.sessionId;
    if (!sid) { stStatus('无会话'); return; }
    // 目标角色：最后发言人优先，回退换壳/在场第一个
    let roleName = '';
    const msgs = (tavernSession.messages) || [];
    for (let i = msgs.length - 1; i >= 0; i--) {
      const m = msgs[i];
      if (m && m.role && m.role !== 'user') { roleName = stSpeakerNameOf(m); if (roleName) break; }
    }
    if (!roleName) {
      const vcid = (tavernSession.entry && tavernSession.entry.vesselCharacterId)
        || (tavernSession.player && tavernSession.player.controlCharacterId) || '';
      const present = (tavernSession.presentCharacterIds || [])[0] || '';
      const fallback = vcid || present;
      const pack = tavernPack;
      const ch = fallback && pack && pack.characters
        ? pack.characters.find((c) => c && String(c.id) === String(fallback)) : null;
      roleName = (ch && ch.name) || '';
      if (!roleName) { stStatus('无可生成目标角色'); return; }
    }
    const cid = stCharIdOf(roleName);
    const pack = tavernPack;
    const ch = pack && pack.characters ? pack.characters.find((c) => c && String(c.id) === String(cid)) : null;
    if (!ch) { stStatus('pack 中无此角色：' + roleName); return; }
    const emotion = stEmotionOf({ content: roleName + '：→' }) || '平静';
    const chSel = $('st-image-channel');
    const channel = chSel ? (chSel.dataset.value || 'uniapi') : 'uniapi';
    stStatus('生成立绘中（' + roleName + '·' + emotion + '），约 10-30s…');
    // P2-1c: 性别/形象——角色卡 gender/appearance 为主（手动蒸馏/蒸馏补全写入），
    // 原文只兜底形象片段；风格不再硬编码「美少女」：卡判女性才美少女，否则中性动漫质感
    const hints = stSpriteHintsOf(roleName);
    const cardGender = (ch.gender && String(ch.gender).trim() && String(ch.gender) !== '未知')
      ? String(ch.gender).trim() : '';
    const cardLook = (ch.appearance && String(ch.appearance).trim() && String(ch.appearance) !== '未知')
      ? String(ch.appearance).trim() : '';
    const lookStr = (cardLook || hints.look) ? '，形象：' + (cardLook || hints.look) : '';
    const isFemale = /女/.test(cardGender);
    const style = isFemale ? '美少女游戏立绘质感' : '写实漫画质感';
    const prompt = '动漫风格半身立绘，' + style + '，竖构图，角色：' + roleName
      + (cardGender ? '，' + cardGender : '') + lookStr
      + '，表情：' + emotion + '，干净纯色背景，人物居中';
    const res = await stFetch('/api/v1/kaleido-tools/image', {
      body: JSON.stringify({ prompt: prompt, channel })
    });
    if (!res.ok) throw new Error('HTTP ' + res.status);
    const data = await res.json();
    const url = (data && (data.url || data.b64)) ? (data.url || ('data:image/jpeg;base64,' + data.b64)) : null;
    if (!url) throw new Error((data && data.error) || '生成立绘无返回');
    // 写回 pack：GET 全量 → 改 expressions[emotion] → POST upsert
    const full = await stApi('/packs/' + encodeURIComponent(pack.id));
    if (!full || !Array.isArray(full.characters)) throw new Error('读 pack 失败');
    const target = full.characters.find((c) => c && String(c.id) === String(cid));
    if (!target) throw new Error('pack 角色缺失');
    if (!target.expressions || typeof target.expressions !== 'object') target.expressions = {};
    target.expressions[emotion] = url;
    if (!target.avatar) target.avatar = url;
    const saved = await stApi('/packs', { method: 'POST', body: JSON.stringify(full) });
    // 同步本地 tavernPack 与全局缓存，再刷立绘
    if (saved && tavernPacks) {
      const idx = tavernPacks.findIndex((p) => p && p.id === pack.id);
      if (idx >= 0) tavernPacks[idx] = saved; else tavernPacks.push(saved);
    }
    tavernPack = saved || full;
    stShowImage(url);
    try { stRenderSprite(); } catch (_) {}
    stStatus('立绘已生成并写入 pack（' + roleName + '·' + emotion + '）');
  }

  /** 轻量图片浮层（点击关闭）。 */
  function stShowImage(url) {
    let view = $('st-image-view');
    if (!view) {
      view = document.createElement('div');
      view.id = 'st-image-view';
      view.className = 'st-image-view';
      view.innerHTML = '<img alt="生成插图"><button type="button" class="st-image-close" aria-label="关闭">✕</button>';
      view.addEventListener('click', () => view.remove());
      document.body.appendChild(view);
    }
    view.querySelector('img').src = url;
    view.style.display = 'flex';
  }

  /** 产线 C：VN 多角色立绘同场 —— charId → 角色名（pack.characters 查找） */
  function stCharNameById(id) {
    if (!id) return '';
    const chars = (tavernPack && Array.isArray(tavernPack.characters)) ? tavernPack.characters : [];
    for (let i = 0; i < chars.length; i++) {
      if (String(chars[i].id || '') === String(id)) return (chars[i].name || '').trim();
    }
    return '';
  }

  /** P2-1 立绘层（产线 C 升级）：渲染 #st-sprite 多角色立绘阵列。
   *  遍历 presentCharacterIds → 每个有立绘的角色渲染一张 <img>，横向 flex 排列；
   *  最近发言角色 .st-speaking（高亮），其余 .st-idle（压暗）。
   *  无 presentCharacterIds / 仅有 1 个角色时降级为单图逻辑；无任何立绘时隐藏容器。 */
  function stRenderSprite() {
    const box = $('st-sprite');
    if (!box) return;
    const msgs = (tavernSession && tavernSession.messages) || [];

    // —— Step 1: 确定当前发言者 charId（回溯最近一条有发言人前缀的 assistant 消息）——
    let speakingCharId = '';
    let speakingName = '';
    for (let i = msgs.length - 1; i >= 0; i--) {
      const m = msgs[i];
      if (m && m.role && m.role !== 'user') {
        const speaker = stSpeakerNameOf(m);
        if (!speaker) continue;
        const u = stSpriteOf(speaker);
        if (u) { speakingCharId = stCharIdOf(speaker); speakingName = speaker; break; }
      }
    }

    // —— Step 2: 收集在场角色 ID 列表（presentCharacterIds 优先，无则降级单角色）——
    let characterIds = [];
    const present = (tavernSession && tavernSession.presentCharacterIds) || [];
    if (present.length > 1) {
      // 多角色：遍历 present，过滤掉有立绘的角色
      for (let j = 0; j < present.length; j++) {
        const cid = present[j];
        const name = stCharNameById(cid) || cid;
        const url = stSpriteOf(name);
        if (url) characterIds.push({ id: cid, name: name, url: url });
      }
    }

    // —— Step 3: 降级到单角色（无 presentCharacterIds 或多角色无立绘）——
    if (characterIds.length === 0) {
      // 复用旧逻辑：回溯最后一条有立绘的角色
      let url = null;
      let label = speakingName;
      if (!url && speakingName) url = stSpriteOf(speakingName);
      // 再尝试回溯
      if (!url) {
        for (let i = msgs.length - 1; i >= 0; i--) {
          const m = msgs[i];
          if (m && m.role && m.role !== 'user') {
            const s = stSpeakerNameOf(m);
            if (!s) continue;
            const u = stSpriteOf(s);
            if (u) { url = u; label = s; break; }
          }
        }
      }
      if (!url) {
        box.classList.add('hidden');
        return;
      }
      // 单图降级
      box.classList.remove('hidden');
      box.classList.add('is-single');
      box.setAttribute('aria-label', label ? (label + ' 立绘') : '角色立绘');
      // 清空旧内容（防旧结构残留）
      box.innerHTML = '';
      const slot = document.createElement('div');
      slot.className = 'st-sprite-slot';
      const img = document.createElement('img');
      img.className = 'st-sprite-img';
      img.src = url;
      img.alt = (label || '角色') + ' 立绘';
      img.addEventListener('click', function () { stShowImage(img.src); });
      slot.appendChild(img);
      box.appendChild(slot);
      return;
    }

    // —— Step 4: 多角色阵列渲染 ——
    box.classList.remove('is-single');
    box.classList.remove('hidden');
    box.setAttribute('aria-label', speakingName ? (speakingName + ' 等角色立绘') : '角色立绘');

    // 清除旧结构残留（旧版直接插 img，现统一为 .st-sprite-slot）
    Array.from(box.children).forEach(function (ch) {
      if (!ch.classList || !ch.classList.contains('st-sprite-slot')) ch.remove();
    });

    // 构建新 DOM（diff 更新：保留已有的 slot，增删差额）
    const existingSlots = box.querySelectorAll('.st-sprite-slot');
    const existingCount = existingSlots.length;
    const targetCount = characterIds.length;

    // 增加 slot
    for (let k = existingCount; k < targetCount; k++) {
      const slot = document.createElement('div');
      slot.className = 'st-sprite-slot';
      const img = document.createElement('img');
      img.className = 'st-sprite-img';
      img.alt = '角色立绘';
      img.addEventListener('click', function () { stShowImage(img.src); });
      slot.appendChild(img);
      const label = document.createElement('span');
      label.className = 'st-sprite-label';
      slot.appendChild(label);
      box.appendChild(slot);
    }
    // 删减多余 slot
    while (box.children.length > targetCount) {
      box.removeChild(box.lastChild);
    }

    // 更新每个 slot
    const slots = box.querySelectorAll('.st-sprite-slot');
    for (let k = 0; k < targetCount; k++) {
      const entry = characterIds[k];
      const slot = slots[k];
      const img = slot.querySelector('img');
      const labelEl = slot.querySelector('.st-sprite-label');

      // 发言状态 class
      const isSpeaking = entry.id && speakingCharId && String(entry.id) === String(speakingCharId);
      slot.classList.toggle('st-speaking', !!isSpeaking);
      slot.classList.toggle('st-idle', !isSpeaking);

      // 图片 src
      if (img && img.getAttribute('src') !== entry.url) img.setAttribute('src', entry.url);
      if (img) img.alt = (entry.name || '角色') + ' 立绘';
      // 名字标签
      if (labelEl) labelEl.textContent = entry.name || '';
    }
  }

  // 角色背景沉浸模式（对标 Agnai）：随立绘同步刷新聊天区背景
  try { if (window.stRefreshImmerseBg) stRefreshImmerseBg(); } catch (_) {}

  // 剧情助手工具按钮：重生成 / 回退
  if ($('st-assist-reroll')) $('st-assist-reroll').onclick = (e) => { e.preventDefault(); stRerollLast(); };
  if ($('st-assist-rewind')) $('st-assist-rewind').onclick = (e) => { e.preventDefault(); stRewindOne(); };

  /** 朗读（edge-tts）：最后一条assistant消息 → mp3 → Audio 播放。 */

/* S2.8: sendMessage('//…') assist modal — publish for real-module chat.js.
 * (window.stFocusAssistInput was already read by _tavern-send but never assigned.) */
try {
  window.stOpenAssistModal = stOpenAssistModal;
  window.stFocusAssistInput = stFocusAssistInput;
} catch (_) {}
