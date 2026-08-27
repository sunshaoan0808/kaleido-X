import { FONT_FAMILIES, FONT_SIZES, READER_ANNOTATION_MENU_ID, READER_SEARCH_ID, READER_SEARCH_RESULTS_ID, READER_SETTINGS_ID, READER_TOC_BACKDROP_ID, READER_TOC_ID, READER_VIEW_ID, THEMES } from './constants.js';

/**
 * foliate-reader/ui.js — UI mixin (Phase 1.2 split)
 * Pure presentation methods assigned onto FoliateReader.prototype.
 */
export const UI_MIXIN = {
  toggleTOC() {
    const panel = document.getElementById(READER_TOC_ID);
    const backdrop = document.getElementById(READER_TOC_BACKDROP_ID);
    if (!panel || !backdrop) return;
    this._tocOpen = !this._tocOpen;
    panel.style.transform = this._tocOpen ? 'translateX(0)' : 'translateX(-100%)';
    backdrop.style.display = this._tocOpen ? 'block' : 'none';
  },
  _renderTOC() {
    const panel = document.getElementById(READER_TOC_ID);
    if (!panel) return;
    if (!this._tocItems || this._tocItems.length === 0) {
      panel.innerHTML = '<div style="padding:16px;color:var(--theme-text,#999);font-size:13px;text-align:center;">暂无目录</div>';
      return;
    }
    let html = '';
    for (const item of this._tocItems) {
      const indent = Math.min((item.level - 1) * 20, 40);
      html += `<div class="toc-item" data-href="section-1#${item.id}" style="padding:6px 16px 6px ${16 + indent}px;cursor:pointer;font-size:${item.level === 1 ? '14px' : '13px'};font-weight:${item.level === 1 ? '600' : '400'};color:var(--theme-text,#333);border-bottom:1px solid var(--theme-border,#eee);transition:background 0.15s;">${item.label}</div>`;
    }
    panel.innerHTML = html;

    // Click handlers
    panel.querySelectorAll('.toc-item').forEach(el => {
      el.addEventListener('click', () => {
        const href = el.dataset.href;
        if (href && this.view) {
          this.view.goTo?.(href);
          this.toggleTOC();
        }
      });
    });
  },
  toggleSearch() {
    const panel = document.getElementById(READER_SEARCH_ID);
    const backdrop = document.getElementById(READER_TOC_BACKDROP_ID);
    if (!panel) return;
    const visible = panel.style.transform !== 'translateX(0)';
    panel.style.transform = visible ? 'translateX(0)' : 'translateX(100%)';
    if (backdrop) backdrop.style.display = visible ? 'block' : 'none';
    if (visible) {
      const input = panel.querySelector('#foliate-search-input');
      if (input) { input.focus(); input.select(); }
    }
  },
  async _search(query) {
    if (!this.view || !query.trim()) return;
    const resultsContainer = document.getElementById(READER_SEARCH_RESULTS_ID);
    const countEl = document.getElementById('foliate-search-count');
    if (!resultsContainer) return;

    resultsContainer.innerHTML = '<div style="padding:16px;text-align:center;color:var(--theme-text,#999)">搜索中...</div>';
    if (countEl) countEl.textContent = '搜索中...';
    this._searchResults = [];
    this._searchIndex = -1;

    try {
      for await (const result of this.view.search(query)) {
        if (result && result.subitems) {
          this._searchResults.push(...result.subitems);
        } else if (result && (result.cfi || result.excerpt)) {
          this._searchResults.push(result);
        }
      }
    } catch (e) {
      console.error('Search error:', e);
      resultsContainer.innerHTML = `<div style="padding:16px;text-align:center;color:red">搜索出错: ${e.message}</div>`;
      return;
    }

    if (countEl) countEl.textContent = `${this._searchResults.length} 个结果`;

    if (this._searchResults.length === 0) {
      resultsContainer.innerHTML = '<div style="padding:16px;text-align:center;color:var(--theme-text,#999)">未找到匹配结果</div>';
      return;
    }

    resultsContainer.innerHTML = this._searchResults.map((r, i) => {
      const excerpt = r.excerpt || '(无上下文)';
      const escaped = excerpt.replace(/</g, '&lt;').replace(/>/g, '&gt;');
      const highlighted = escaped.replace(
        new RegExp(query.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'), 'gi'),
        m => `<mark style="background:#ffeb3b;padding:0 2px;border-radius:2px">${m}</mark>`
      );
      return `<div data-search-idx="${i}" style="padding:8px 16px;cursor:pointer;border-bottom:1px solid var(--theme-border,#eee);font-size:13px;color:var(--theme-text,#555);line-height:1.5"
        onmouseover="this.style.background='var(--theme-border,#eee)'" onmouseout="this.style.background='transparent'"
        onclick="__foliateReader._searchGoTo(${i})">${highlighted}</div>`;
    }).join('');

    this._searchGoTo(0);
  },
  _searchGoTo(idx) {
    const result = this._searchResults[idx];
    if (!result) return;
    this.view.goTo(result.cfi);
    this._searchIndex = idx;
    const container = document.getElementById(READER_SEARCH_RESULTS_ID);
    if (container) {
      container.querySelectorAll('[data-search-idx]').forEach((el, i) => {
        el.style.background = i === idx ? 'var(--theme-accent,#1a73e8)' : 'transparent';
        el.style.color = i === idx ? '#fff' : 'var(--theme-text,#555)';
      });
    }
  },
  _searchNavigate(dir) {
    if (!this._searchResults || this._searchResults.length === 0) return;
    let idx = this._searchIndex + dir;
    if (idx < 0) idx = this._searchResults.length - 1;
    if (idx >= this._searchResults.length) idx = 0;
    this._searchGoTo(idx);
  },
  _handleSearchShortcut() {
    if (this._tocOpen) this.toggleTOC();
    this.toggleSearch();
  },
  _onSelectionChange(sel) {
    const menu = document.getElementById(READER_ANNOTATION_MENU_ID);
    if (!menu) return;
    if (!sel) sel = window.getSelection();
    if (!sel || sel.isCollapsed || !sel.toString().trim()) {
      menu.style.display = 'none';
      return;
    }
    const range = sel.getRangeAt(0);
    const rect = range.getBoundingClientRect();
    let offX = 0, offY = 0;
    if (sel !== window.getSelection()) {
      try {
        const fe = sel.anchorNode?.ownerDocument?.defaultView?.frameElement;
        if (fe) {
          const r = fe.getBoundingClientRect();
          offX = r.left;
          offY = r.top;
        }
      } catch (e) { /* ignore */ }
    }
    menu.style.display = 'flex';
    menu.style.top = Math.max(10, rect.top + offY - 50) + 'px';
    menu.style.left = Math.max(10, rect.left + offX + rect.width / 2 - 100) + 'px';
    this._selectedRange = range;
    this._selectedText = sel.toString().trim();
  },
  _hideAnnotationMenu() {
    const menu = document.getElementById(READER_ANNOTATION_MENU_ID);
    if (menu) menu.style.display = 'none';
  },
  _showFootnotePopup(label) {
    const existing = document.getElementById('foliate-footnote-popup');
    if (existing) existing.remove();

    const popup = document.createElement('div');
    popup.id = 'foliate-footnote-popup';

    popup.innerHTML = `
      <div style="display:flex;justify-content:space-between;align-items:center;margin-bottom:8px;">
        <strong>脚注</strong>
        <button id="foliate-footnote-close" style="background:transparent;border:none;font-size:18px;cursor:pointer;color:var(--theme-text,#555);padding:0 4px;">✕</button>
      </div>
      <div id="foliate-footnote-content"></div>
    `;

    // Try to find footnote content from the view's document
    const viewRoot = this.view?.shadowRoot || this.view?.querySelector('[data-view]')?.shadowRoot;
    if (viewRoot) {
      const fnEl = viewRoot.querySelector(`#fn-${CSS.escape(label)}`);
      if (fnEl) {
        const contentDiv = popup.querySelector('#foliate-footnote-content');
        contentDiv.innerHTML = fnEl.innerHTML.replace(/<a[^>]*>↩<\/a>/, '');
      } else {
        popup.querySelector('#foliate-footnote-content').textContent = `脚注 [${label}] 未找到`;
      }
    } else {
      // Fallback: search the full document
      const fnEl = document.getElementById(`fn-${label}`);
      if (fnEl) {
        const contentDiv = popup.querySelector('#foliate-footnote-content');
        contentDiv.innerHTML = fnEl.innerHTML.replace(/<a[^>]*>↩<\/a>/, '');
      } else {
        popup.querySelector('#foliate-footnote-content').textContent = `脚注 [${label}] 未找到`;
      }
    }

    popup.querySelector('#foliate-footnote-close').onclick = () => popup.remove();
    document.body.appendChild(popup);

    // Close on backdrop click
    popup.addEventListener('click', (e) => {
      if (e.target === popup) popup.remove();
    });
  },
  _updateTTSButton() {
    if (!this._ttsButton) return;
    if (this._ttsPlaying) {
      this._ttsButton.textContent = '⏸';
      this._ttsButton.title = '暂停朗读 (P)';
    } else if (this._ttsPaused) {
      this._ttsButton.textContent = '▶';
      this._ttsButton.title = '继续朗读 (P)';
    } else {
      this._ttsButton.textContent = '🔊';
      this._ttsButton.title = '朗读 (P)';
    }
  },
  _showTTSPanel() {
    let panel = document.getElementById('foliate-tts-panel');
    if (panel) { panel.style.display = 'flex'; return; }

    panel = document.createElement('div');
    panel.id = 'foliate-tts-panel';

    const playBtn = document.createElement('button');
    playBtn.textContent = '⏸';
    playBtn.title = '暂停/继续';
    playBtn.style.cssText = 'font-size: 20px; background: none; border: none; cursor: pointer;';
    playBtn.onclick = () => this._toggleTTS();

    const stopBtn = document.createElement('button');
    stopBtn.textContent = '⏹';
    stopBtn.title = '停止';
    stopBtn.style.cssText = 'font-size: 20px; background: none; border: none; cursor: pointer;';
    stopBtn.onclick = () => this._stopTTS();

    // Speed control
    const speedLabel = document.createElement('span');
    speedLabel.textContent = '语速:';
    const speedSlider = document.createElement('input');
    speedSlider.type = 'range';
    speedSlider.min = '0.5';
    speedSlider.max = '2.0';
    speedSlider.step = '0.1';
    speedSlider.value = this._ttsRate.toString();
    speedSlider.style.cssText = 'width: 80px;';
    speedSlider.oninput = () => {
      this._ttsRate = parseFloat(speedSlider.value);
      speedVal.textContent = speedSlider.value + 'x';
      if (this._ttsUtterance) this._ttsUtterance.rate = this._ttsRate;
    };
    const speedVal = document.createElement('span');
    speedVal.textContent = this._ttsRate + 'x';
    speedVal.style.cssText = 'min-width: 35px; text-align: center;';

    panel.append(playBtn, stopBtn, speedLabel, speedSlider, speedVal);
    document.body.appendChild(panel);
  },
  _hideTTSPanel() {
    const panel = document.getElementById('foliate-tts-panel');
    if (panel) panel.style.display = 'none';
  },
  _renderDictResult(word, data, rect) {
    const entry = data[0];
    if (!entry) {
      this._showDictPopup(`未找到 "${word}" 的解释`, rect);
      return;
    }
    const phonetic = entry.phonetic || '';
    const phonetics = entry.phonetics || [];
    // Find an audio URL (US preferred, then UK)
    let audioUrl = '';
    for (const p of phonetics) {
      if (p.audio && p.audio.endsWith('.mp3')) {
        audioUrl = p.audio;
        if (p.text && p.text.includes('US')) break; // prefer US
      }
    }
    const meanings = entry.meanings || [];
    let html = `<div style="font-weight:bold;font-size:16px;margin-bottom:4px;">${this._escapeHtml(word)}</div>`;
    if (phonetic) {
      html += `<div style="color:#666;font-size:13px;margin-bottom:8px;">${this._escapeHtml(phonetic)}`;
      if (audioUrl) {
        html += ` <span id="dict-play-audio" style="cursor:pointer;color:#4a90d9;text-decoration:underline;font-size:12px;">🔊 发音</span>`;
      }
      html += `</div>`;
    }
    for (const m of meanings.slice(0, 2)) {
      const partOfSpeech = m.partOfSpeech || '';
      html += `<div style="margin:4px 0;"><i>${partOfSpeech}</i>`;
      const defs = m.definitions || [];
      for (const d of defs.slice(0, 2)) {
        html += `<div style="margin:2px 0 2px 12px;">• ${this._escapeHtml(d.definition || '')}</div>`;
        if (d.example) {
          html += `<div style="margin:0 0 4px 24px;color:#888;font-size:12px;">例: ${this._escapeHtml(d.example)}</div>`;
        }
      }
      html += `</div>`;
    }
    // Wordbook + Quote buttons
    html += `<div style="margin-top:8px;border-top:1px solid #eee;padding-top:6px;text-align:right;">
      <button id="dict-quote-image" style="background:transparent;border:1px solid #4a90d9;color:#4a90d9;border-radius:4px;padding:4px 12px;cursor:pointer;font-size:12px;margin-right:6px;">🖼 生成图片</button>
      <button id="dict-save-wordbook" style="background:#4a90d9;color:#fff;border:none;border-radius:4px;padding:4px 12px;cursor:pointer;font-size:12px;">+ 加入生词本</button>
    </div>`;
    this._showDictPopup(html, rect);
    // Bind wordbook button and audio playback
    setTimeout(() => {
      const btn = document.getElementById('dict-save-wordbook');
      if (btn) btn.onclick = () => this._addToWordbook(word, entry);
      const audioEl = document.getElementById('dict-play-audio');
      if (audioEl && audioUrl) {
        audioEl.onclick = () => {
          const a = new Audio(audioUrl);
          a.play().catch(e => console.warn('Audio playback failed:', e));
        };
      }
      const quoteBtn = document.getElementById('dict-quote-image');
      if (quoteBtn) {
        quoteBtn.onclick = () => this._showQuoteImageDialog(this._lastSelectedText || word);
      }
    }, 0);
  },
  _showQuoteImageDialog(text) {
    this._hideDictPopup();
    const quote = (text || '').trim().slice(0, 200);
    if (!quote) { this.showError('没有可生成的文字'); return; }

    const overlay = document.createElement('div');
    overlay.className = 'fr-quote-overlay';
    const canvas = document.createElement('canvas');
    this._drawQuoteCard(canvas, quote);

    const btnRow = document.createElement('div');
    btnRow.style.cssText = 'display:flex;gap:10px;';
    const btnStyle = 'padding:8px 16px;border-radius:6px;font-size:14px;cursor:pointer;border:none;';
    const dlBtn = document.createElement('button');
    dlBtn.textContent = '⬇ 下载 PNG';
    dlBtn.style.cssText = btnStyle + 'background:#4a90d9;color:#fff;';
    dlBtn.onclick = () => this._downloadQuotePNG(canvas);
    const copyBtn = document.createElement('button');
    copyBtn.textContent = '📋 复制到剪贴板';
    copyBtn.style.cssText = btnStyle + 'background:#fff;color:#333;border:1px solid #ccc;';
    copyBtn.onclick = () => this._copyQuoteToClipboard(canvas);
    const closeBtn = document.createElement('button');
    closeBtn.textContent = '✕ 关闭';
    closeBtn.style.cssText = btnStyle + 'background:#e0e0e0;color:#333;';
    closeBtn.onclick = () => overlay.remove();

    btnRow.append(dlBtn, copyBtn, closeBtn);
    overlay.append(canvas, btnRow);
    overlay.onclick = (e) => { if (e.target === overlay) overlay.remove(); };
    document.body.appendChild(overlay);
  },
  _drawQuoteCard(canvas, text) {
    const W = 800, H = 1000;
    canvas.width = W;
    canvas.height = H;
    const ctx = canvas.getContext('2d');

    // Background gradient (deep blue → teal)
    const grad = ctx.createLinearGradient(0, 0, W, H);
    grad.addColorStop(0, '#1b2a4a');
    grad.addColorStop(0.55, '#2d4a6f');
    grad.addColorStop(1, '#1b2a4a');
    ctx.fillStyle = grad;
    ctx.fillRect(0, 0, W, H);

    // Decorative translucent circles
    ctx.globalAlpha = 0.09;
    ctx.fillStyle = '#ffffff';
    ctx.beginPath(); ctx.arc(W - 100, 140, 190, 0, Math.PI * 2); ctx.fill();
    ctx.beginPath(); ctx.arc(60, H - 120, 230, 0, Math.PI * 2); ctx.fill();
    ctx.globalAlpha = 1;

    // Large quote mark
    ctx.fillStyle = 'rgba(255,255,255,0.22)';
    ctx.font = 'italic 150px Georgia, serif';
    ctx.textBaseline = 'top';
    ctx.fillText('“', 64, 120);

    // Quote text with word wrapping
    const maxWidth = W - 170;
    const fontSize = 30;
    const lineHeight = 46;
    ctx.font = `${fontSize}px Georgia, 'Songti SC', 'Noto Serif SC', serif`;
    ctx.fillStyle = '#f7f2e8';
    const words = text.split(/(\s+)/);
    const lines = [];
    let line = '';
    for (const w of words) {
      const test = line + w;
      if (ctx.measureText(test).width > maxWidth && line) {
        lines.push(line);
        line = w.trimStart();
      } else {
        line = test;
      }
    }
    if (line.trim()) lines.push(line);

    // Cap visible lines to fit card
    const maxLines = Math.floor((H - 300) / lineHeight);
    const shown = lines.slice(0, maxLines);
    let y = 280;
    for (const l of shown) {
      ctx.fillText(l, 90, y);
      y += lineHeight;
    }

    // Divider
    ctx.strokeStyle = 'rgba(255,255,255,0.4)';
    ctx.lineWidth = 2;
    ctx.beginPath();
    ctx.moveTo(W - 220, H - 190);
    ctx.lineTo(W - 90, H - 190);
    ctx.stroke();

    // Footer: book title + source
    ctx.textAlign = 'right';
    ctx.fillStyle = 'rgba(255,255,255,0.85)';
    ctx.font = '26px Georgia, serif';
    ctx.fillText(`— ${this._bookTitle || 'ReadAware'}`, W - 90, H - 140);
    ctx.fillStyle = 'rgba(255,255,255,0.4)';
    ctx.font = '16px Arial, sans-serif';
    ctx.fillText('ReadAware 阅读', W - 90, H - 100);
    ctx.textAlign = 'left';
    ctx.textBaseline = 'alphabetic';
  },
  _downloadQuotePNG(canvas) {
    const a = document.createElement('a');
    a.download = `quote-${Date.now()}.png`;
    a.href = canvas.toDataURL('image/png');
    a.click();
  },
  async _copyQuoteToClipboard(canvas) {
    try {
      const blob = await new Promise(res => canvas.toBlob(res, 'image/png'));
      if (!blob) throw new Error('toBlob failed');
      await navigator.clipboard.write([new ClipboardItem({ 'image/png': blob })]);
      this.showToast ? this.showToast('已复制到剪贴板') : alert('已复制到剪贴板');
    } catch (e) {
      console.error('Copy failed:', e);
      alert('复制失败，请使用下载 PNG');
    }
  },
  _showDictPopup(html, rect) {
    this._hideDictPopup();
    const popup = document.createElement('div');
    popup.id = 'foliate-dict-popup';
    popup.innerHTML = typeof html === 'string' ? html : '';

    // Position near selection
    if (rect) {
      const top = rect.bottom + 8;
      const left = Math.min(rect.left, window.innerWidth - 380);
      popup.style.top = top + 'px';
      popup.style.left = Math.max(4, left) + 'px';
    } else {
      popup.style.top = '50%';
      popup.style.left = '50%';
      popup.style.transform = 'translate(-50%, -50%)';
    }
    // Close button
    const closeBtn = document.createElement('button');
    closeBtn.textContent = '×';
    closeBtn.className = 'fr-dict-close';
    closeBtn.onclick = () => this._hideDictPopup();
    popup.appendChild(closeBtn);
    document.body.appendChild(popup);
    // Auto-append quote-image action whenever there is selected text
    if (this._lastSelectedText && !popup.querySelector('#dict-quote-image')) {
      const row = document.createElement('div');
      row.style.cssText = 'margin-top:8px;border-top:1px solid #eee;padding-top:6px;text-align:right;';
      const qBtn = document.createElement('button');
      qBtn.id = 'dict-quote-image';
      qBtn.textContent = '🖼 生成图片';
      qBtn.style.cssText = 'background:transparent;border:1px solid #4a90d9;color:#4a90d9;border-radius:4px;padding:4px 12px;cursor:pointer;font-size:12px;';
      qBtn.onclick = () => this._showQuoteImageDialog(this._lastSelectedText);
      row.appendChild(qBtn);
      popup.appendChild(row);
    }
  },
  _hideDictPopup() {
    const popup = document.getElementById('foliate-dict-popup');
    if (popup) popup.remove();
  },
  _updateThemeUI() {
    const btns = this.overlay?.querySelectorAll('[data-theme-btn]');
    if (!btns) return;
    btns.forEach(btn => {
      btn.classList.toggle('active', btn.dataset.themeBtn === this._currentTheme);
    });
  },
  _updateFontUI() {
    const btns = this.overlay?.querySelectorAll('[data-font-btn]');
    if (!btns) return;
    btns.forEach(btn => {
      btn.classList.toggle('active', btn.dataset.fontBtn === this._currentFontSize);
    });
  },
  _buildSettingsHTML() {
    const themeOpts = Object.keys(THEMES).map(t =>
      `<button data-theme-btn="${t}" class="settings-btn ${t === this._currentTheme ? 'active' : ''}"
        onclick="__foliateReader.setTheme('${t}')"
        style="padding:4px 12px;border:1px solid var(--theme-border,#ccc);
          background:${t === this._currentTheme ? 'var(--theme-accent,#1a73e8)' : 'transparent'};
          color:${t === this._currentTheme ? '#fff' : 'var(--theme-text,#333)'};
          border-radius:4px;cursor:pointer;font-size:12px;">${t}</button>`
    ).join('');

    const fontOpts = Object.keys(FONT_SIZES).map(f =>
      `<button data-font-btn="${f}" class="settings-btn ${f === this._currentFontSize ? 'active' : ''}"
        onclick="__foliateReader.setFontSize('${f}')"
        style="padding:4px 12px;border:1px solid var(--theme-border,#ccc);
          background:${f === this._currentFontSize ? 'var(--theme-accent,#1a73e8)' : 'transparent'};
          color:${f === this._currentFontSize ? '#fff' : 'var(--theme-text,#333)'};
          border-radius:4px;cursor:pointer;font-size:12px;">${FONT_SIZES[f]}px</button>`
    ).join('');

    const familyOpts = Object.keys(FONT_FAMILIES).map(fam =>
      `<button data-family-btn="${fam}"
        onclick="__foliateReader.setFontFamily('${fam}')"
        style="padding:4px 12px;border:1px solid var(--theme-border,#ccc);
          background:${fam === this._currentFontFamily ? 'var(--theme-accent,#1a73e8)' : 'transparent'};
          color:${fam === this._currentFontFamily ? '#fff' : 'var(--theme-text,#333)'};
          border-radius:4px;cursor:pointer;font-size:12px;
          font-family:${FONT_FAMILIES[fam]}">${fam}</button>`
    ).join('');

    return `
      <div style="display:flex;flex-wrap:wrap;gap:12px;align-items:center;">
        <span style="font-size:12px;color:var(--theme-text,#666);font-weight:600;">主题</span>
        ${themeOpts}
        <span style="font-size:12px;color:var(--theme-text,#666);font-weight:600;margin-left:8px;">字号</span>
        ${fontOpts}
        <span style="font-size:12px;color:var(--theme-text,#666);font-weight:600;margin-left:8px;">字体</span>
        ${familyOpts}
      </div>
    `;
  },
  _toggleSettingsPanel() {
    const panel = document.getElementById(READER_SETTINGS_ID);
    if (!panel) return;
    const isVisible = getComputedStyle(panel).display !== 'none';
    panel.style.display = isVisible ? 'none' : 'block';
    if (!isVisible) {
      // Refresh settings HTML with current state
      panel.innerHTML = this._buildSettingsHTML();
    }
  },
  showError(msg) {
    if (!this.overlay) this.createOverlay();
    const container = document.getElementById(READER_VIEW_ID);
    if (container) {
      container.innerHTML = `
        <div style="padding: 3rem; text-align: center;">
          <h3 style="margin: 0 0 1rem; color: #c00;">⚠ 加载失败</h3>
          <p style="color: #666;">${msg}</p>
          <button onclick="__foliateReader.close()"
            style="margin-top: 1.5rem; padding: 8px 24px; background: #333;
                   color: #fff; border: none; border-radius: 4px; cursor: pointer;">
            关闭
          </button>
        </div>
      `;
    }
    this.overlay.style.display = 'flex';
    this.isOpen = true;
  },
};
