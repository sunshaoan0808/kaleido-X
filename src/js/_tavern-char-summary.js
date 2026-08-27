  // ═══════════════════════════════════════════════════════════════
  // Character Summary Cards (B-line: 角色卡自动摘要展示)
  // Lazy-rendered in the sidebar drawer — only builds DOM on demand.
  // Data source: tavernPack.characters[] (PackCharacterRef).
  // ═══════════════════════════════════════════════════════════════

  const ST_CHAR_ROLE_MAP = {
    protagonist: '主角', main: '主角', lead: '主角',
    antagonist: '反派', villain: '反派',
    supporting: '配角', side: '配角', secondary: '配角',
    narrator: '旁白',
    player: '玩家', reader: '玩家',
    npc: 'NPC', extra: '龙套',
  };

  const ST_CHAR_FIELDS = [
    { key: 'personality',        label: '性格',       icon: 'personality' },
    { key: 'speechStyle',        label: '说话风格',   icon: 'speechStyle' },
    { key: 'motivation',         label: '动机',       icon: 'motivation' },
    { key: 'relationships',      label: '关系',       icon: 'relationships',  array: true },
    { key: 'mentalModels',       label: '心智模型',   icon: 'mentalModels',   array: true },
    { key: 'decisionHeuristics', label: '决策启发式', icon: 'decisionHeuristics', array: true },
    { key: 'beliefs',            label: '信念',       icon: 'beliefs',        array: true },
  ];

  // Inline Lucide SVG icon paths (viewBox 0 0 24 24, stroke-width 2)
  const ST_CHAR_ICONS_SVG = {
    personality:      '<circle cx="12" cy="12" r="10"/><path d="M8 14s1.5 2 4 2 4-2 4-2"/><line x1="9" y1="9" x2="9.01" y2="9"/><line x1="15" y1="9" x2="15.01" y2="9"/>',
    speechStyle:      '<path d="M7.9 20A9 9 0 1 0 4 16.1L2 22Z"/>',
    motivation:       '<circle cx="12" cy="12" r="10"/><circle cx="12" cy="12" r="6"/><circle cx="12" cy="12" r="2"/>',
    relationships:    '<path d="M10 13a5 5 0 0 0 7.54.54l3-3a5 5 0 0 0-7.07-7.07l-1.72 1.71"/><path d="M14 11a5 5 0 0 0-7.54-.54l-3 3a5 5 0 0 0 7.07 7.07l1.71-1.71"/>',
    mentalModels:     '<path d="M12 2a10 10 0 1 0 0 20 10 10 0 0 0 0-20Z"/><path d="M12 16v-4"/><path d="M12 8h.01"/>',
    decisionHeuristics: '<path d="m16 16-4-4-4 4"/><path d="M12 12v9"/><path d="m8 8 4 4 4-4"/>',
    beliefs:          '<path d="M19 14c1.49-1.46 3-3.21 3-5.5A5.5 5.5 0 0 0 16.5 3c-1.76 0-3 .5-4.5 2-1.5-1.5-2.74-2-4.5-2A5.5 5.5 0 0 0 2 8.5c0 2.3 1.5 4.05 3 5.5l7 7Z"/>',
    chevron:          '<path d="m9 18 6-6-6-6"/>',
  };

  function stCharIconSvg(name, size) {
    var paths = ST_CHAR_ICONS_SVG[name] || ST_CHAR_ICONS_SVG.personality;
    var s = size || 12;
    return '<svg aria-hidden="true" xmlns="http://www.w3.org/2000/svg" width="' + s + '" height="' + s + '" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">' + paths + '</svg>';
  }

  function stCharRoleLabel(role) {
    var r = String(role || '').trim().toLowerCase();
    return ST_CHAR_ROLE_MAP[r] || r || '角色';
  }

  /** Filter out narrator/player and junk names from character list. */
  function stCharFilterPack(chars) {
    if (!Array.isArray(chars)) return [];
    var junkRe = /^(露出|眼角|换鞋|随口|轻声|低头|抬起|转身|伸手|走过去|看向|听见|突然)/;
    return chars.filter(function (c) {
      if (!c) return false;
      var role = String(c.role || '').toLowerCase();
      var n = String(c.name || '').trim();
      if (role.includes('narrator') || role.includes('player')) return false;
      if (n === '旁白' || n === '读者' || n === '玩家') return false;
      if (!n || n.length < 2 || n.length > 12) return false;
      if (junkRe.test(n)) return false;
      if (/[的了着在把被]/.test(n)) return false;
      return true;
    });
  }

  /** Render a single character summary card (returns HTML string). */
  function stCharRenderCard(ch) {
    var name = escapeHtml(String(ch.name || '').trim() || '???');
    var role = stCharRoleLabel(ch.role);
    var initial = name.charAt(0);

    // Avatar
    var avatarHtml;
    if (ch.avatar && String(ch.avatar).trim()) {
      avatarHtml = '<div class="st-char-avatar"><img src="' + escapeHtml(ch.avatar) + '" alt="' + name + '" loading="lazy" /></div>';
    } else {
      avatarHtml = '<div class="st-char-avatar">' + initial + '</div>';
    }

    // Build field rows
    var fieldsHtml = '';
    for (var fi = 0; fi < ST_CHAR_FIELDS.length; fi++) {
      var f = ST_CHAR_FIELDS[fi];
      var val = ch[f.key];
      if (val === undefined || val === null || val === '') continue;
      if (Array.isArray(val) && val.length === 0) continue;

      fieldsHtml += '<div class="st-char-field">';
      fieldsHtml += '<div class="st-char-field-label">' + stCharIconSvg(f.icon) + ' ' + f.label + '</div>';

      if (f.array && Array.isArray(val)) {
        var items = val.slice(0, 3);
        var hasMore = val.length > 3;
        fieldsHtml += '<div class="st-char-field-list">';
        for (var ai = 0; ai < items.length; ai++) {
          fieldsHtml += '<div class="st-char-field-item">' + escapeHtml(String(items[ai])) + '</div>';
        }
        if (hasMore) {
          fieldsHtml += '<div class="st-char-field-item st-char-collapsed" style="display:none">';
          for (var bi = 3; bi < val.length; bi++) {
            fieldsHtml += escapeHtml(String(val[bi])) + (bi < val.length - 1 ? '<br>' : '');
          }
          fieldsHtml += '</div>';
          fieldsHtml += '<button type="button" class="st-char-collapse-btn" data-char-toggle="collapsed" data-count="' + (val.length - 3) + '">' + stCharIconSvg('chevron', 10) + ' +' + (val.length - 3) + ' 更多</button>';
        }
        fieldsHtml += '</div>';
      } else {
        fieldsHtml += '<div class="st-char-field-text">' + escapeHtml(String(val)) + '</div>';
      }
      fieldsHtml += '</div>';
    }

    // Evidence refs (collapsible)
    var refs = ch.evidenceRefs;
    if (Array.isArray(refs) && refs.length > 0) {
      fieldsHtml += '<div class="st-char-evidence">';
      fieldsHtml += '<button type="button" class="st-char-evidence-toggle">' + stCharIconSvg('chevron', 10) + ' 证据出处 (' + refs.length + ')</button>';
      fieldsHtml += '<div class="st-char-evidence-body">';
      for (var ri = 0; ri < refs.length; ri++) {
        fieldsHtml += '<span class="st-char-evidence-tag">' + escapeHtml(String(refs[ri])) + '</span> ';
      }
      fieldsHtml += '</div></div>';
    }

    // SoulLink 档案（archive: {fields, personality, worldview, family, relationships, memory}）
    var arch = ch.archive;
    if (arch && typeof arch === 'object') {
      fieldsHtml += '<div class="st-char-archive">';
      fieldsHtml += '<div class="st-char-archive-title">' + stCharIconSvg('personality', 11) + ' 角色档案</div>';
      // 标量字段
      var archF = arch.fields || {};
      var scalarPairs = [['name', '姓名'], ['age', '年龄'], ['gender', '性别'], ['occupation', '职业']];
      var scalarHtml = '';
      for (var si = 0; si < scalarPairs.length; si++) {
        var sv = archF[scalarPairs[si][0]];
        if (sv === undefined || sv === null || sv === '') continue;
        scalarHtml += '<span class="st-archive-scalar">' + scalarPairs[si][1] + ': ' + escapeHtml(String(sv)) + '</span>';
      }
      if (scalarHtml) fieldsHtml += '<div class="st-archive-scalars">' + scalarHtml + '</div>';
      // 分节
      var archSections = [['personality', '性格'], ['worldview', '世界观'], ['family', '家庭'], ['relationships', '关系'], ['memory', '记忆']];
      for (var secI = 0; secI < archSections.length; secI++) {
        var secKey = archSections[secI][0];
        var secLabel = archSections[secI][1];
        var entries = Array.isArray(arch[secKey]) ? arch[secKey] : [];
        if (!entries.length) continue;
        fieldsHtml += '<div class="st-archive-section"><div class="st-archive-section-label">' + secLabel + '</div>';
        for (var eI = 0; eI < entries.length; eI++) {
          var eC = entries[eI] && entries[eI].content !== undefined ? entries[eI].content : String(entries[eI]);
          if (eC === undefined || eC === null || eC === '') continue;
          fieldsHtml += '<div class="st-archive-item">' + escapeHtml(String(eC)) + '</div>';
        }
        fieldsHtml += '</div>';
      }
      // 操作按钮
      fieldsHtml += '<div class="st-archive-actions">' +
        '<button type="button" class="st-archive-btn" data-arch-action="analyze" data-char="' + escapeHtml(ch.id) + '">' + stCharIconSvg('motivation', 10) + ' 分析</button>' +
        '<button type="button" class="st-archive-btn" data-arch-action="refine" data-char="' + escapeHtml(ch.id) + '">' + stCharIconSvg('beliefs', 10) + ' 精编</button>' +
        '</div>';
      fieldsHtml += '</div>';
    }

    // If no fields at all, show minimal card
    var bodyContent = fieldsHtml || '<div class="st-char-empty" style="padding:var(--sp-1h) 0;opacity:.6;font-size:var(--fs-xs)">暂无蒸馏数据</div>';

    return '<div class="st-char-card">' +
      '<div class="st-char-head">' + avatarHtml +
        '<div class="st-char-info"><div class="st-char-name">' + name + '</div>' +
        '<div class="st-char-role">' + escapeHtml(role) + '</div></div></div>' +
      '<div class="st-char-body">' + bodyContent + '</div></div>';
  }

  /** Render the full character summary section into the drawer container. */
  function stRenderCharSummary() {
    var container = $('st-char-list');
    if (!container) return;
    var pack = tavernPack; // IIFE 共享变量（_state-part.js let 声明），非 window 全局
    var chars = stCharFilterPack(pack && pack.characters);

    if (!chars.length) {
      container.innerHTML = '<div class="st-char-empty">' +
        '<svg aria-hidden="true" xmlns="http://www.w3.org/2000/svg" width="32" height="32" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2"/><circle cx="9" cy="7" r="4"/><path d="M22 21v-2a4 4 0 0 0-3-3.87"/><path d="M16 3.128a4 4 0 0 1 0 7.744"/></svg>' +
        '<span>暂无角色数据</span></div>';
      return;
    }

    var html = '';
    for (var i = 0; i < chars.length; i++) {
      html += stCharRenderCard(chars[i]);
    }
    container.innerHTML = html;

    // Bind collapse/expand toggles
    container.querySelectorAll('.st-char-collapse-btn').forEach(function (btn) {
      btn.onclick = function () {
        var target = btn.parentElement.querySelector('.st-char-collapsed');
        if (!target) return;
        var expanded = target.style.display !== 'none';
        target.style.display = expanded ? 'none' : '';
        btn.classList.toggle('expanded', !expanded);
        var cnt = btn.getAttribute('data-count') || '?';
        btn.innerHTML = stCharIconSvg('chevron', 10) + ' ' + (expanded ? ('+' + cnt + ' 更多') : '收起');
      };
    });

    // Bind evidence toggles
    container.querySelectorAll('.st-char-evidence-toggle').forEach(function (btn) {
      btn.onclick = function () {
        var body = btn.nextElementSibling;
        if (!body) return;
        var showing = body.classList.toggle('show');
        btn.classList.toggle('expanded', showing);
        var svgHtml = stCharIconSvg('chevron', 10);
        var tag = btn.closest('.st-char-evidence');
        var count = tag ? (tag.querySelectorAll('.st-char-evidence-tag').length) : 0;
        btn.innerHTML = svgHtml + ' 证据出处 (' + count + ')' + (showing ? ' ▾' : '');
      };
    });

    // Bind SoulLink 档案 分析/精编 buttons
    container.querySelectorAll('.st-archive-btn').forEach(function (btn) {
      btn.onclick = function () {
        var action = btn.getAttribute('data-arch-action');
        var charId = btn.getAttribute('data-char');
        if (!action || !charId || !tavernPack || !tavernPack.id || typeof stApi !== 'function') return;
        var packId = tavernPack.id;
        var body = { characterId: charId };
        // analyze 需要近期对话：优先当前 URL 会话（hash 里的 session id 最可靠；
        // sessionId 变量可能是旧 partner-session，不能信）
        if (action === 'analyze') {
          var sid = ((location.hash.match(/session\/([^/?#]+)/) || [])[1] || '') ||
            (typeof sessionId === 'string' ? sessionId : '');
          if (sid) body.sessionId = sid;
        }
        btn.disabled = true;
        var old = btn.innerHTML;
        btn.innerHTML = (action === 'analyze' ? '分析中…' : '精编中…');
        stApi('/packs/' + encodeURIComponent(packId) + '/archive/' + action, {
          method: 'POST',
          body: JSON.stringify(body),
        }).then(function (res) {
          if (res && res.changes && res.changes.length) {
            stStatus('档案' + (action === 'analyze' ? '分析' : '精编') + '完成：' + res.changes.length + ' 处变更');
          } else if (res && res.ok) {
            stStatus('档案' + (action === 'analyze' ? '分析' : '精编') + '完成：无变更');
          } else {
            stStatus('档案' + (action === 'analyze' ? '分析' : '精编') + '失败');
          }
          return stRefreshCharSummary();
        }).catch(function () {
          stStatus('档案' + (action === 'analyze' ? '分析' : '精编') + '请求失败');
        }).finally(function () {
          btn.disabled = false;
          btn.innerHTML = old;
        });
      };
    });

  }

  /** Called when the side drawer opens — lazy-renders character cards if needed. */
  async function stRefreshCharSummary() {
    // 精简版 pack 的 characters 只有 id/name（列表接口），蒸馏字段(personality 等)缺失时
    // 重拉 full pack 再渲染（stEnsureFullPack 的缓存分支会命中精简版，绕过它直接走 full）
    var pack = tavernPack;
    var chars = stCharFilterPack(pack && pack.characters);
    var needFull = chars.length > 0 && !chars.some(function (c) {
      return c.personality || c.motivation || c.beliefs || (c.relationships && c.relationships.length) || (c.mentalModels && c.mentalModels.length);
    });
    if (needFull && pack && pack.id && typeof stApi === 'function') {
      try {
        var full = await stApi('/packs/' + encodeURIComponent(pack.id));
        if (full && Array.isArray(full.characters)) tavernPack = full;
      } catch (_) { /* keep existing */ }
    }
    stRenderCharSummary();
  }
