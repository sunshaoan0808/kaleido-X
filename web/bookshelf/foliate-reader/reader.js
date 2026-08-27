/**
 * foliate-reader/reader.js — Phase 2: Reader Controls (split from foliate-reader.js)
 *
 * FoliateReader class with:
 *   - Theme switching (light/dark/sepia/parchment)
 *   - Font size control (5 presets)
 *   - Fullscreen API
 *   - Progress bar with relocate event
 *   - Keyboard navigation
 *   - Auto-hide toolbar
 *
 * Dependencies:
 *   - globalThis.__foliateBridge (foliate-bridge.js)
 *   - <foliate-view> custom element
 */

import { READER_OVERLAY_ID, READER_VIEW_ID, READER_TOPBAR_ID, READER_BOTTOMBAR_ID, READER_TITLE_ID, READER_PROGRESS_ID, READER_SETTINGS_ID, READER_TOC_ID, READER_TOC_BACKDROP_ID, READER_SEARCH_ID, READER_SEARCH_RESULTS_ID, READER_ANNOTATION_MENU_ID, LS_THEME, LS_FONTSIZE, LS_PROGRESS_PREFIX, ANNOTATION_COLORS, THEMES, FONT_SIZES, FONT_FAMILIES } from './constants.js';
import { mdToHtml, makeBookFromHTML } from './markdown.js';
import { UI_MIXIN } from './ui.js';

// ─── FoliateReader class ────────────────────────────────────────────────────

class FoliateReader {
  constructor() {
    this.overlay = null;
    this.view = null;
    this.currentSlug = null;
    this.isOpen = false;
    this._escapeHandler = null;
    this._keyboardHandler = null;
    this._relocateHandler = null;
    this._fullscreenChangeHandler = null;
    this._footnoteHandler = null;
    this._autoHideTimer = null;
    this._isFullscreen = false;
    this._currentTheme = localStorage.getItem(LS_THEME) || 'light';
    this._currentFontSize = localStorage.getItem(LS_FONTSIZE) || 'medium';
    this._currentFontFamily = 'serif';
    this._tocItems = [];
    this._tocOpen = false;
    this._savedProgress = null;
    this._selectionHandler = null;
    // TTS state
    this._ttsPlaying = false;
    this._ttsPaused = false;
    this._ttsUtterance = null;
    this._ttsRate = 1.0;
    this._ttsVoice = null;
    this._ttsButton = null;
    // Dictionary state
    this._dictInited = false;
    this._dictDoc = null;
    this._dictSelectionHandler = null;
    this._dictDbPromise = null;
    this._lastSelectedText = null;
    // Quote image state
    this._bookTitle = null;
  }

  // ── Theme ─────────────────────────────────────────────────────────────────
  getThemes() { return Object.keys(THEMES); }

  getTheme() { return this._currentTheme; }

  setTheme(name) {
    if (!THEMES[name]) return;
    this._currentTheme = name;
    localStorage.setItem(LS_THEME, name);
    this._applyTheme();
  }

  _applyTheme() {
    const t = THEMES[this._currentTheme] || THEMES.light;
    if (!this.overlay) return;
    this.overlay.style.setProperty('--theme-bg', t.bg);
    this.overlay.style.setProperty('--theme-text', t.text);
    this.overlay.style.setProperty('--theme-border', t.border);
    this.overlay.style.setProperty('--theme-toolbar', t.toolbar);
    this.overlay.style.setProperty('--theme-accent', t.accent);
    this.overlay.style.background = t.bg;
    // Update theme buttons
    this._updateThemeUI();
  }

  // ── Font Size ─────────────────────────────────────────────────────────────
  getFontSizes() { return Object.keys(FONT_SIZES); }

  getFontSize() { return this._currentFontSize; }

  setFontSize(preset) {
    if (!FONT_SIZES[preset]) return;
    this._currentFontSize = preset;
    localStorage.setItem(LS_FONTSIZE, preset);
    this._applyFontSize();
  }

  _applyFontSize() {
    const size = FONT_SIZES[this._currentFontSize] || 16;
    const family = FONT_FAMILIES[this._currentFontFamily] || FONT_FAMILIES.serif;
    if (this.view?.element) {
      this.view.element.style.fontSize = size + 'px';
      this.view.element.style.fontFamily = family;
    }
    this._updateFontUI();
  }

  // ── Font Family ───────────────────────────────────────────────────────────
  setFontFamily(name) {
    if (!FONT_FAMILIES[name]) return;
    this._currentFontFamily = name;
    localStorage.setItem('foliate:fontFamily', name);
    this._applyFontSize();
  }

  // ── Fullscreen ────────────────────────────────────────────────────────────
  toggleFullscreen() {
    if (!document.fullscreenElement) {
      document.documentElement.requestFullscreen().catch(() => {});
    } else {
      document.exitFullscreen().catch(() => {});
    }
  }

  _onFullscreenChange = () => {
    this._isFullscreen = !!document.fullscreenElement;
    const btn = this.overlay?.querySelector('[data-action="fullscreen"]');
    if (btn) {
      btn.textContent = this._isFullscreen ? '⇱ 退出全屏' : '⛶ 全屏';
    }
  };

  // ── Progress ──────────────────────────────────────────────────────────────
  updateProgress(info) {
    const bar = document.getElementById(READER_PROGRESS_ID);
    if (!bar) return;
    const { fraction = 0, page = 0, totalPages = 0 } = info || {};
    const pct = Math.min(100, Math.max(0, Math.round(fraction * 100)));
    bar.querySelector('.progress-fill').style.width = pct + '%';
    bar.querySelector('.progress-text').textContent =
      totalPages > 0 ? `${pct}% · ${page}/${totalPages} 页` : `${pct}%`;
  }

  _onRelocate = (e) => {
    this.updateProgress(e.detail || {});
    // Save progress
    const detail = e.detail || {};
    this._totalPages = detail.totalPages || 0;
    this._currentPage = detail.page || 0;
    // Completion screen at ~100%
    if (this.isOpen && detail.fraction != null && detail.fraction >= 0.98 && detail.fraction < 2) {
      this._showCompletionScreen();
    }
    if (this.currentSlug) {
      const { fraction = 0, page = 0, totalPages = 0 } = detail;
      try {
        localStorage.setItem(LS_PROGRESS_PREFIX + this.currentSlug,
          JSON.stringify({ slug: this.currentSlug, fraction, page, totalPages, timestamp: Date.now() }));
      } catch (e) { /* quota exceeded */ }
    }
  };

  // ── Toolbar auto-hide ─────────────────────────────────────────────────────
  showToolbars() {
    const topbar = this.overlay?.querySelector(`#${READER_TOPBAR_ID}`);
    const bottombar = this.overlay?.querySelector(`#${READER_BOTTOMBAR_ID}`);
    if (topbar) topbar.style.opacity = '1';
    if (bottombar) bottombar.style.opacity = '1';
    this._startAutoHideTimer();
  }

  hideToolbars() {
    if (!this.isOpen) return;
    const topbar = this.overlay?.querySelector(`#${READER_TOPBAR_ID}`);
    const bottombar = this.overlay?.querySelector(`#${READER_BOTTOMBAR_ID}`);
    if (topbar) topbar.style.opacity = '0';
    if (bottombar) bottombar.style.opacity = '0';
  }

  _startAutoHideTimer() {
    if (this._autoHideTimer) clearTimeout(this._autoHideTimer);
    this._autoHideTimer = setTimeout(() => this.hideToolbars(), 3000);
  }

  // ── Progress persistence ──────────────────────────────────────────────────
  _loadProgress(slug) {
    try {
      const raw = localStorage.getItem(LS_PROGRESS_PREFIX + slug);
      return raw ? JSON.parse(raw) : null;
    } catch (e) { return null; }
  }

  // ── TOC ─────────────────────────────────────────────────────────────────────

  // ── Search & Annotation Methods ───────────────────────────────────────────

  // ── Footnotes ──────────────────────────────────────────────────────────────

  _addAnnotation(type) {
    if (!this.view) return;
    const menu = document.getElementById(READER_ANNOTATION_MENU_ID);
    if (menu) menu.style.display = 'none';

    // Resolve a live range: prefer the cached one, otherwise fall back to the
    // still-active in-iframe selection (menu click may have cleared the cache
    // or the cross-frame focus may have moved).
    let range = this._selectedRange;
    if (!range || !range.startContainer || !range.startContainer.isConnected) {
      range = null;
      try {
        const contents = this.view.renderer?.getContents?.() || [];
        for (const c of contents) {
          const s = c.doc?.getSelection?.();
          if (s && !s.isCollapsed && s.rangeCount > 0) {
            const r = s.getRangeAt(0);
            if (r.startContainer?.isConnected) { range = r; break; }
          }
        }
      } catch (e) { /* ignore */ }
    }
    if (!range) return;

    const colorBtn = document.querySelector('#foliate-color-picker [style*="border-color: rgb(85"]');
    const colorName = colorBtn ? colorBtn.dataset.color : 'yellow';
    const color = ANNOTATION_COLORS[colorName] || ANNOTATION_COLORS.yellow;

    let cfi = '';
    try {
      const contents = this.view.renderer.getContents();
      const index = contents?.[0]?.index ?? 0;
      cfi = this.view.getCFI(index, range);
    } catch (e) {
      console.warn('getCFI failed:', e);
    }

    const annotation = {
      type,
      color,
      text: this._selectedText || range.toString() || '',
      cfi,
      date: Date.now()
    };

    try {
      this.view.addAnnotation({ value: cfi || this._selectedText, ...annotation });
    } catch (e) {
      console.warn('addAnnotation (non-critical):', e);
    }

    this._saveAnnotation(annotation);
    try {
      const doc = range.startContainer.ownerDocument;
      doc.getSelection()?.removeAllRanges();
    } catch (e) { window.getSelection().removeAllRanges(); }
    this._selectedRange = null;
    this._selectedText = '';
  }

  // ── IndexedDB Persistence ──────────────────────────────────────────────────

  // ── Reading time tracking ─────────────────────────────────────────────────
  _startReadingTimer() {
    this._readingStart = Date.now();
  }

  async _stopReadingTimer() {
    if (!this._readingStart) return;
    const elapsed = Math.round((Date.now() - this._readingStart) / 1000);
    this._readingStart = null;
    if (!this.currentSlug || elapsed < 2) return;
    try {
      const db = await this._getStatsDB();
      const tx = db.transaction('stats', 'readwrite');
      const store = tx.objectStore('stats');
      const existing = await new Promise((resolve) => {
        const g = store.get(this.currentSlug);
        g.onsuccess = () => resolve(g.result || null);
        g.onerror = () => resolve(null);
      });
      const totalSeconds = (existing?.totalSeconds || 0) + elapsed;
      const sessions = (existing?.sessions || 0) + 1;
      store.put({ slug: this.currentSlug, totalSeconds, sessions, lastRead: Date.now() });
      this._readingStats = { slug: this.currentSlug, totalSeconds, sessions };
    } catch (e) { /* storage unavailable */ }
  }

  async _getStatsDB() {
    return new Promise((resolve, reject) => {
      const req = indexedDB.open('ReadAwareStats', 1);
      req.onupgradeneeded = () => {
        const db = req.result;
        if (!db.objectStoreNames.contains('stats')) {
          db.createObjectStore('stats', { keyPath: 'slug' });
        }
      };
      req.onsuccess = () => resolve(req.result);
      req.onerror = () => reject(req.error);
    });
  }

  async _getStats(slug) {
    try {
      const db = await this._getStatsDB();
      return await new Promise((resolve) => {
        const tx = db.transaction('stats', 'readonly');
        const g = tx.objectStore('stats').get(slug);
        g.onsuccess = () => resolve(g.result || null);
        g.onerror = () => resolve(null);
      });
    } catch (e) { return null; }
  }

  async _getDB() {
    return new Promise((resolve, reject) => {
      const req = indexedDB.open('ReadAwareAnnotations', 1);
      req.onupgradeneeded = () => {
        const db = req.result;
        if (!db.objectStoreNames.contains('annotations')) {
          db.createObjectStore('annotations', { keyPath: 'id' });
        }
      };
      req.onsuccess = () => resolve(req.result);
      req.onerror = () => reject(req.error);
    });
  }

  async _saveAnnotation(ann) {
    try {
      const slug = this.currentSlug || window.location.pathname;
      const db = await this._getDB();
      const tx = db.transaction('annotations', 'readwrite');
      const store = tx.objectStore('annotations');
      store.put({ id: `${slug}:${ann.date}`, slug, ...ann });
      await new Promise((resolve, reject) => {
        tx.oncomplete = resolve;
        tx.onerror = reject;
      });
    } catch (e) {
      console.warn('IndexedDB save error:', e);
    }
  }

  async _loadAnnotations() {
    try {
      const slug = this.currentSlug || window.location.pathname;
      const db = await this._getDB();
      const tx = db.transaction('annotations', 'readonly');
      const store = tx.objectStore('annotations');
      const all = await new Promise((resolve, reject) => {
        const req = store.getAll();
        req.onsuccess = () => resolve(req.result);
        req.onerror = () => reject(req.error);
      });
      const relevant = all.filter(a => a.slug === slug);
      for (const ann of relevant) {
        if (this.view && (ann.cfi || ann.text)) {
          try { this.view.addAnnotation({ value: ann.cfi || ann.text, ...ann }); } catch (e) {}
        }
      }
    } catch (e) {
      console.warn('IndexedDB load error:', e);
    }
  }

  // ── Annotation rendering (draw-annotation) ────────────────────────────────
  _onDrawAnnotation(e) {
    const { draw, annotation } = e.detail || {};
    if (typeof draw !== 'function') return;
    const type = annotation?.type || 'highlight';
    const color = annotation?.color || ANNOTATION_COLORS.yellow || 'yellow';
    const svgNS = 'http://www.w3.org/2000/svg';
    if (type === 'underline') {
      draw((rects) => {
        const g = document.createElementNS(svgNS, 'g');
        for (const r of rects) {
          const line = document.createElementNS(svgNS, 'line');
          line.setAttribute('x1', r.left);
          line.setAttribute('y1', r.top + r.height - 1);
          line.setAttribute('x2', r.left + r.width);
          line.setAttribute('y2', r.top + r.height - 1);
          line.setAttribute('stroke', color);
          line.setAttribute('stroke-width', '1.5');
          g.append(line);
        }
        return g;
      });
      return;
    }
    draw((rects) => {
      const g = document.createElementNS(svgNS, 'g');
      for (const r of rects) {
        const rect = document.createElementNS(svgNS, 'rect');
        rect.setAttribute('x', r.left);
        rect.setAttribute('y', r.top);
        rect.setAttribute('width', r.width);
        rect.setAttribute('height', r.height);
        rect.setAttribute('rx', '3');
        rect.setAttribute('fill', color);
        rect.setAttribute('opacity', '0.25');
        g.append(rect);
      }
      return g;
    });
  }

  // ── Annotation selection (in-iframe aware) ────────────────────────────────
  _attachAnnotationSelectionListener() {
    this._annotationSelHandler = () => {
      if (!this.isOpen) return;
      let sel = window.getSelection();
      try {
        const contents = this.view?.renderer?.getContents();
        if (contents && contents[0]?.doc) {
          const docSel = contents[0].doc.getSelection();
          if (docSel && !docSel.isCollapsed && docSel.toString().trim()) {
            sel = docSel;
          }
        }
      } catch (e) { /* keep main-window selection */ }
      if (!sel || sel.isCollapsed || !sel.toString().trim()) {
        this._hideAnnotationMenu();
        return;
      }
      this._onSelectionChange(sel);
    };
    const attachToDoc = (doc) => {
      this._annotationSelDoc = doc;
      doc.addEventListener('selectionchange', this._annotationSelHandler);
    };
    try {
      const contents = this.view?.renderer?.getContents();
      if (contents && contents[0]?.doc) {
        attachToDoc(contents[0].doc);
        return;
      }
    } catch (e) { /* fall through to polling */ }
    document.addEventListener('selectionchange', this._annotationSelHandler);
    let retries = 0;
    const poll = () => {
      if (retries >= 20 || !this.isOpen) return;
      retries++;
      setTimeout(() => {
        try {
          const c = this.view?.renderer?.getContents();
          if (c && c[0]?.doc) {
            document.removeEventListener('selectionchange', this._annotationSelHandler);
            attachToDoc(c[0].doc);
          } else {
            poll();
          }
        } catch (e) { poll(); }
      }, 400);
    };
    poll();
  }

  _detachAnnotationSelectionListener() {
    if (this._annotationSelHandler) {
      if (this._annotationSelDoc) {
        this._annotationSelDoc.removeEventListener('selectionchange', this._annotationSelHandler);
      } else {
        document.removeEventListener('selectionchange', this._annotationSelHandler);
      }
      this._annotationSelDoc = null;
      this._annotationSelHandler = null;
    }
  }

  // ── TTS (Text-to-Speech) ───────────────────────────────────────────────────

  async _toggleTTS() {
    if (this._ttsPlaying) {
      this._pauseTTS();
    } else if (this._ttsPaused) {
      this._resumeTTS();
    } else {
      await this._startTTS();
    }
    this._updateTTSButton();
  }

  async _startTTS() {
    if (!this.view || !this.isOpen) return;
    if (!window.speechSynthesis) { alert('浏览器不支持语音合成'); return; }

    // Cancel any existing speech
    window.speechSynthesis.cancel();

    // Get page text
    let text = '';
    try {
      const contents = this.view.getContents();
      if (contents && contents.length) {
        for (const { doc } of contents) {
          if (doc && doc.body) text += doc.body.innerText + '\n';
        }
      }
    } catch (e) {
      console.warn('TTS text extraction failed:', e);
    }
    if (!text.trim()) { alert('当前页无可朗读内容'); return; }

    // Select voice
    let voice = this._ttsVoice;
    if (!voice) {
      const voices = window.speechSynthesis.getVoices();
      // Prefer Chinese voice
      voice = voices.find(v => v.lang.startsWith('zh')) || voices[0] || null;
      this._ttsVoice = voice;
    }

    const utterance = new SpeechSynthesisUtterance(text);
    utterance.rate = this._ttsRate;
    utterance.voice = voice;
    this._ttsUtterance = utterance;

    utterance.onstart = () => {
      this._ttsPlaying = true;
      this._ttsPaused = false;
      this._updateTTSButton();
      this._showTTSPanel();
    };
    utterance.onpause = () => {
      this._ttsPaused = true;
      this._ttsPlaying = false;
      this._updateTTSButton();
    };
    utterance.onresume = () => {
      this._ttsPlaying = true;
      this._ttsPaused = false;
      this._updateTTSButton();
    };
    utterance.onend = () => {
      this._ttsPlaying = false;
      this._ttsPaused = false;
      this._ttsUtterance = null;
      this._updateTTSButton();
      this._hideTTSPanel();
    };
    utterance.onerror = (e) => {
      console.warn('TTS error:', e);
      this._ttsPlaying = false;
      this._ttsPaused = false;
      this._updateTTSButton();
      this._hideTTSPanel();
    };

    window.speechSynthesis.speak(utterance);
  }

  _pauseTTS() {
    if (window.speechSynthesis.speaking) {
      window.speechSynthesis.pause();
    }
  }

  _resumeTTS() {
    if (window.speechSynthesis.paused) {
      window.speechSynthesis.resume();
    }
  }

  _stopTTS() {
    window.speechSynthesis.cancel();
    this._ttsPlaying = false;
    this._ttsPaused = false;
    this._ttsUtterance = null;
    this._updateTTSButton();
    this._hideTTSPanel();
  }

  // ── Dictionary ────────────────────────────────────────────────────────────

  _initDict() {
    if (this._dictInited) return;
    this._dictInited = true;
    // Attach selection listener to the view's content document (inside closed shadow DOM)
    this._attachDictListener();

    // Re-attach on relocate (content doc changes between chapters)
    const origRelocate = this._relocateHandler;
    this._relocateHandler = (e) => {
      this._detachDictListener();
      this._attachDictListener();
      this._hideDictPopup();
      if (origRelocate) origRelocate(e);
    };
  }

  _onDictSelection() {
    // Get selection from the view's content document (inside closed shadow DOM)
    let sel;
    try {
      const contents = this.view?.renderer?.getContents();
      if (contents && contents[0]?.doc) {
        sel = contents[0].doc.getSelection();
      }
    } catch (e) {
      // Fallback to main window selection
      sel = window.getSelection();
    }
    if (!sel || sel.isCollapsed || !sel.toString().trim()) {
      this._hideDictPopup();
      return;
    }
    const word = sel.toString().trim().slice(0, 100);
    if (!word || word.length < 1) return;
    this._lastSelectedText = word;
    const range = sel.getRangeAt(0);
    const rect = range.getBoundingClientRect();
    if (!rect || rect.width === 0) return;
    this._lookupWord(word, rect);
  }

  _attachDictListener() {
    this._dictSelectionHandler = (e) => {
      clearTimeout(this._dictTimer);
      this._dictTimer = setTimeout(() => this._onDictSelection(), 500);
    };
    try {
      const contents = this.view?.renderer?.getContents();
      if (contents && contents[0]?.doc) {
        this._dictDoc = contents[0].doc;
        this._dictDoc.addEventListener('selectionchange', this._dictSelectionHandler);
      } else {
        // Fallback to main document
        document.addEventListener('selectionchange', this._dictSelectionHandler);
        // Content doc loads asynchronously after open(); poll until it exists, then re-attach
        let retries = 0;
        const retry = () => {
          if (retries >= 15) return;
          retries++;
          setTimeout(() => {
            try {
              const c = this.view?.renderer?.getContents();
              if (c && c[0]?.doc) {
                document.removeEventListener('selectionchange', this._dictSelectionHandler);
                this._dictDoc = c[0].doc;
                this._dictDoc.addEventListener('selectionchange', this._dictSelectionHandler);
              } else {
                retry();
              }
            } catch (e) {
              retry();
            }
          }, 400);
        };
        retry();
      }
    } catch (e) {
      document.addEventListener('selectionchange', this._dictSelectionHandler);
    }
  }

  _detachDictListener() {
    if (this._dictSelectionHandler) {
      if (this._dictDoc) {
        this._dictDoc.removeEventListener('selectionchange', this._dictSelectionHandler);
      } else {
        document.removeEventListener('selectionchange', this._dictSelectionHandler);
      }
      this._dictDoc = null;
    }
  }

  _lookupSelected() {
    // Manual lookup via toolbar/button: get selection from view's content doc
    let sel;
    try {
      const contents = this.view?.renderer?.getContents();
      if (contents && contents[0]?.doc) {
        sel = contents[0].doc.getSelection();
      }
    } catch (e) {
      sel = window.getSelection();
    }
    if (!sel || sel.isCollapsed || !sel.toString().trim()) {
      this.showError('请先选中要查询的词语');
      return;
    }
    const word = sel.toString().trim().slice(0, 100);
    this._lastSelectedText = word;
    const range = sel.getRangeAt(0);
    const rect = range.getBoundingClientRect();
    this._lookupWord(word, rect || { left: window.innerWidth / 2, top: 100, width: 0, height: 0 });
  }

  async _lookupWord(word, rect) {
    this._showDictPopup(`正在查询 "${word}"…`, rect);
    // CJK text (Chinese/Japanese/Korean): English dictionary is useless & CORS-blocked → show local hint
    if (/[\u4e00-\u9fff\u3040-\u30ff\uac00-\ud7af]/.test(word)) {
      const html = `<div style="font-weight:bold;font-size:16px;margin-bottom:4px;">${this._escapeHtml(word)}</div>
<div style="color:#666;font-size:13px;margin-bottom:8px;">中文/日文/韩文暂不支持在线释义（当前词典仅支持英文）。可生成引用图片或加入生词本。</div>
<div style="margin-top:8px;border-top:1px solid #eee;padding-top:6px;text-align:right;">
<button id="dict-quote-image" style="background:transparent;border:1px solid #4a90d9;color:#4a90d9;border-radius:4px;padding:4px 12px;cursor:pointer;font-size:12px;margin-right:6px;">🖼 生成图片</button>
<button id="dict-save-wordbook" style="background:#4a90d9;color:#fff;border:none;border-radius:4px;padding:4px 12px;cursor:pointer;font-size:12px;">+ 加入生词本</button>
</div>`;
      this._showDictPopup(html, rect);
      setTimeout(() => {
        const btn = document.getElementById('dict-save-wordbook');
        if (btn) btn.onclick = () => this._addToWordbook(word, null);
        const quoteBtn = document.getElementById('dict-quote-image');
        if (quoteBtn) quoteBtn.onclick = () => this._showQuoteImageDialog(this._lastSelectedText || word);
      }, 0);
      return;
    }
    // Try free dictionary API
    try {
      const resp = await fetch(`https://api.dictionaryapi.dev/api/v2/entries/en/${encodeURIComponent(word)}`);
      if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
      const data = await resp.json();
      this._renderDictResult(word, data, rect);
    } catch {
      // Fallback: try yourdictionary.com or show error
      this._showDictPopup(
        `"${word}" — 查询失败，请检查网络或换词`,
        rect
      );
    }
  }

  _addToWordbook(word, entry) {
    // Save to IndexedDB "wordbook" store
    try {
      this._getDictDB().then(db => {
        const tx = db.transaction('wordbook', 'readwrite');
        const store = tx.objectStore('wordbook');
        store.put({ word, entry, addedAt: Date.now() });
      });
      // Update button feedback
      const btn = document.getElementById('dict-save-wordbook');
      if (btn) { btn.textContent = '✓ 已保存'; btn.style.background = '#4caf50'; }
    } catch (e) {
      console.error('Wordbook save failed:', e);
    }
  }

  // ── Quote Image (Phase 5.5) ──────────────────────────────────────────────

  _escapeHtml(str) {
    if (!str) return '';
    return str.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;').replace(/'/g, '&#039;');
  }

  _getDictDB() {
    if (this._dictDbPromise) return this._dictDbPromise;
    this._dictDbPromise = new Promise((resolve, reject) => {
      const req = indexedDB.open('ReadAwareDict', 1);
      req.onupgradeneeded = () => {
        const db = req.result;
        if (!db.objectStoreNames.contains('wordbook')) {
          db.createObjectStore('wordbook', { keyPath: 'word' });
        }
      };
      req.onsuccess = () => resolve(req.result);
      req.onerror = () => reject(req.error);
    });
    return this._dictDbPromise;
  }

  // ── Open ──────────────────────────────────────────────────────────────────
  async open(slug) {
    if (!slug) { this.showError('无效的书籍标识'); return; }

    if (this.isOpen && this.currentSlug === slug && this.overlay) {
      this.overlay.style.display = 'flex';
      return;
    }

    if (this.isOpen) this.close();
    this.currentSlug = slug;

    // Fetch content
    let data;
    try {
      const resp = await fetch(`/api/v1/crawler/novels/${encodeURIComponent(slug)}/content`);
      if (!resp.ok) throw new Error(`API returned ${resp.status}`);
      data = await resp.json();
    } catch (err) {
      this.showError(`获取内容失败: ${err.message}`);
      return;
    }

    let rawContent = '';
    if (typeof data === 'string') rawContent = data;
    else if (data?.content) rawContent = data.content;
    else { this.showError('内容格式错误'); return; }

    const bridge = globalThis.__foliateBridge;
    let processed = rawContent;
    if (bridge) {
      if (typeof bridge.restructureContent === 'function')
        processed = bridge.restructureContent(processed);
      if (typeof bridge.normalizeChapters === 'function')
        processed = bridge.normalizeChapters(processed);
    }

    const { html: fullHtml, toc: tocItems } = mdToHtml(processed);
    this._tocItems = tocItems || [];
    const novelTitle = data?.title || slug;
    this._bookTitle = novelTitle;
    const book = makeBookFromHTML(fullHtml, novelTitle, this._tocItems);

    if (!this.overlay) this.createOverlay();

    const titleEl = document.getElementById(READER_TITLE_ID);
    if (titleEl) titleEl.textContent = novelTitle;

    const container = document.getElementById(READER_VIEW_ID);
    if (!container) { this.showError('阅读器容器未找到'); return; }
    container.innerHTML = '';

    if (!customElements.get('foliate-view')) {
      this.showError('foliate-js 引擎未加载 (缺少 <foliate-view> 元素)');
      return;
    }

    const view = document.createElement('foliate-view');
    view.id = 'foliate-instance';
    view.style.cssText = 'width: 100%; height: 100%; display: block;';
    container.appendChild(view);
    this.view = view;

    if (!this._drawAnnotationHandler) {
      this._drawAnnotationHandler = (e) => this._onDrawAnnotation(e);
    }
    this.view.addEventListener('draw-annotation', this._drawAnnotationHandler);

    // Apply current font size/theme
    this._applyTheme();
    this._applyFontSize();

    // Show overlay
    this.overlay.style.display = 'flex';
    this.isOpen = true;

    // Open book
    try {
      await view.open(book);
    } catch (err) {
      this.showError(`打开书籍失败: ${err.message}`);
      return;
    }

    // Render TOC in sidebar
    this._renderTOC();

    // Init dictionary
    this._initDict();

    // Start reading timer
    this._startReadingTimer();

    // Listen to relocate events
    this._relocateHandler = this._onRelocate;
    view.addEventListener('relocate', this._relocateHandler);

    // Footnote click handler
    this._footnoteHandler = (e) => {
      const link = e.target.closest?.('a.footnote-ref') || e.target.closest?.('[href^="#fn-"]');
      if (link) {
        const label = link.dataset.footnote || link.getAttribute('href')?.replace('#fn-', '');
        if (label) {
          e.preventDefault();
          e.stopPropagation();
          this._showFootnotePopup(label);
        }
      }
    };
    view.addEventListener('click', this._footnoteHandler);

    // Restore saved reading progress
    const saved = this._loadProgress(slug);
    if (saved && saved.fraction > 0 && saved.fraction < 1) {
      setTimeout(() => {
        try { view.goTo?.(saved.fraction); } catch (e) { /* ignore */ }
      }, 100);
    } else {
      setTimeout(() => {
        try { view.goTo({ index: 0 }); } catch (e) { /* ignore */ }
      }, 100);
    }

    // Keyboard navigation
    this._keyboardHandler = (e) => {
      if (!this.isOpen) return;
      if (e.key === 'ArrowLeft') { view.goLeft?.(); e.preventDefault(); }
      else if (e.key === 'ArrowRight') { view.goRight?.(); e.preventDefault(); }
      else if (e.key === 'f' || e.key === 'F') { this.toggleFullscreen(); }
      else if (e.key === 't' || e.key === 'T') { this.toggleTOC(); e.preventDefault(); }
      else if (e.key === 's' || e.key === 'S') { e.preventDefault(); this._handleSearchShortcut(); }
      else if (e.key === 'p' || e.key === 'P') { e.preventDefault(); this._toggleTTS(); }
      else if (e.key === 'd' || e.key === 'D') { e.preventDefault(); this._lookupSelected(); }
    };
    document.addEventListener('keydown', this._keyboardHandler);

    // Fullscreen change
    this._fullscreenChangeHandler = this._onFullscreenChange;
    document.addEventListener('fullscreenchange', this._fullscreenChangeHandler);

    // Selection change → show annotation menu (in-iframe aware)
    this._attachAnnotationSelectionListener();

    // Load saved annotations
    this._loadAnnotations(slug);
    
    // Show toolbars initially
    this.showToolbars();
  }

  // ── Close ─────────────────────────────────────────────────────────────────
  close() {
    this._stopReadingTimer();
    this._completionShown = false;
    if (this._relocateHandler && this.view) {
      this.view.removeEventListener('relocate', this._relocateHandler);
      this._relocateHandler = null;
    }
    if (this._keyboardHandler) {
      document.removeEventListener('keydown', this._keyboardHandler);
      this._keyboardHandler = null;
    }
    if (this._footnoteHandler && this.view) {
      this.view.removeEventListener('click', this._footnoteHandler);
      this._footnoteHandler = null;
    }
    if (this._drawAnnotationHandler && this.view) {
      this.view.removeEventListener('draw-annotation', this._drawAnnotationHandler);
      this._drawAnnotationHandler = null;
    }
    if (this._fullscreenChangeHandler) {
      document.removeEventListener('fullscreenchange', this._fullscreenChangeHandler);
      this._fullscreenChangeHandler = null;
    }
    if (this._autoHideTimer) {
      clearTimeout(this._autoHideTimer);
      this._autoHideTimer = null;
    }
    if (this._isFullscreen) {
      document.exitFullscreen?.().catch(() => {});
      this._isFullscreen = false;
    }
    // Stop TTS
    if (this._ttsPlaying || this._ttsPaused) {
      this._stopTTS();
    }
    this._hideTTSPanel();
    // Dictionary cleanup
    this._hideDictPopup();
    if (this._dictSelectionHandler && this._dictDoc) {
      this._dictDoc.removeEventListener('selectionchange', this._dictSelectionHandler);
      this._dictSelectionHandler = null;
      this._dictDoc = null;
    }
    // Annotation selection cleanup
    this._detachAnnotationSelectionListener();
    this._hideAnnotationMenu();
    if (this.view) {
      try { this.view.close?.(); } catch (e) { /* ignore */ }
      this.view.remove();
      this.view = null;
    }
    if (this.overlay) {
      this.overlay.style.display = 'none';
      this.showToolbars(); // reset opacity
    }
    this.isOpen = false;
    this.currentSlug = null;
    this._tocItems = [];
    // Hide TOC panel
    const tocPanel = document.getElementById(READER_TOC_ID);
    const tocBackdrop = document.getElementById(READER_TOC_BACKDROP_ID);
    if (tocPanel) tocPanel.style.transform = 'translateX(-100%)';
    if (tocBackdrop) tocBackdrop.style.display = 'none';
    this._tocOpen = false;
  }

  // ── Completion screen ────────────────────────────────────────────────────
  async _showCompletionScreen() {
    if (!this.isOpen || this._completionShown) return;
    this._completionShown = true;
    const stats = await this._getStats(this.currentSlug);
    const secs = stats?.totalSeconds || 0;
    const h = Math.floor(secs / 3600), m = Math.floor((secs % 3600) / 60);
    const timeStr = h > 0 ? `${h} 小时 ${m} 分` : `${m} 分钟`;
    const div = document.createElement('div');
    div.style.cssText = `
      position: absolute; inset: 0; z-index: 2147483000;
      display: flex; align-items: center; justify-content: center;
      background: var(--theme-bg, rgba(255,255,255,0.96));
      backdrop-filter: blur(4px);
    `;
    div.innerHTML = `
      <div style="text-align:center; max-width: 420px; padding: 24px;">
        <div style="font-size: 48px; margin-bottom: 12px;">🎉</div>
        <h2 style="margin: 0 0 8px; font-size: 22px; color: var(--theme-text, #333);">读完啦！</h2>
        <p style="margin: 0 0 4px; color: var(--theme-text, #666);">《${this._bookTitle || ''}》</p>
        <p style="margin: 0 0 20px; color: var(--theme-text, #666);">累计阅读 ${timeStr} · 共 ${stats?.sessions || 1} 次</p>
        <button class="ra-completion-close" style="
          padding: 8px 28px; font-size: 14px; cursor: pointer;
          background: var(--theme-accent, #1a73e8); color: #fff;
          border: none; border-radius: 20px;
        ">继续阅读</button>
      </div>
    `;
    div.querySelector('.ra-completion-close').onclick = () => {
      div.remove();
      this._completionShown = false;
    };
    this.overlay.appendChild(div);
  }

  // ── Destroy ───────────────────────────────────────────────────────────────
  destroy() {
    this.close();
    if (this.overlay) { this.overlay.remove(); this.overlay = null; }
  }

  // ── Create overlay DOM ────────────────────────────────────────────────────
  createOverlay() {
    const existing = document.getElementById(READER_OVERLAY_ID);
    if (existing) existing.remove();

    const overlay = document.createElement('div');
    overlay.id = READER_OVERLAY_ID;

    // ── Top bar ──────────────────────────────────────────────────────────
    const topbar = document.createElement('div');
    topbar.id = READER_TOPBAR_ID;

    // Left: Title
    const titleEl = document.createElement('span');
    titleEl.id = READER_TITLE_ID;

    titleEl.textContent = '加载中...';

    // Right: controls
    const topControls = document.createElement('div');
    topControls.style.cssText = 'display: flex; align-items: center; gap: 8px; flex-shrink: 0;';

    // Search button
    const searchBtn = document.createElement('button');
    searchBtn.textContent = '🔍';
    searchBtn.dataset.action = 'search';
    searchBtn.title = '搜索 (S)';

    searchBtn.onclick = (e) => {
      e.stopPropagation();
      this.toggleSearch();
    };

    // TTS button
    const ttsBtn = document.createElement('button');
    ttsBtn.textContent = '🔊';
    ttsBtn.dataset.action = 'tts';
    ttsBtn.title = '朗读 (P)';

    ttsBtn.onclick = (e) => {
      e.stopPropagation();
      this._toggleTTS();
    };
    this._ttsButton = ttsBtn;

    // Dictionary button
    const dictBtn = document.createElement('button');
    dictBtn.textContent = '📖';
    dictBtn.dataset.action = 'dict';
    dictBtn.title = '查词 (D)';

    dictBtn.onclick = (e) => {
      e.stopPropagation();
      this._lookupSelected();
    };

    // Settings button
    const settingsBtn = document.createElement('button');
    settingsBtn.textContent = '⚙';
    settingsBtn.dataset.action = 'settings';
    settingsBtn.title = '设置';
    settingsBtn.className = 'fr-btn-lg';
    settingsBtn.onclick = (e) => {
      e.stopPropagation();
      this._toggleSettingsPanel();
    };

    // Fullscreen button
    const fsBtn = document.createElement('button');
    fsBtn.textContent = '⛶ 全屏';
    fsBtn.dataset.action = 'fullscreen';
    fsBtn.title = '全屏 (F)';
    fsBtn.className = 'fr-btn-sm';
    fsBtn.onclick = () => this.toggleFullscreen();

    // Close button
    const closeBtn = document.createElement('button');
    closeBtn.textContent = '✕ 关闭';
    closeBtn.className = 'fr-btn-sm';
    closeBtn.onclick = () => this.close();

    topControls.appendChild(searchBtn);
    topControls.appendChild(ttsBtn);
    topControls.appendChild(dictBtn);
    topControls.appendChild(settingsBtn);
    topControls.appendChild(fsBtn);
    topControls.appendChild(closeBtn);
    topbar.appendChild(titleEl);
    topbar.appendChild(topControls);
    overlay.appendChild(topbar);

    // ── Settings Panel (hidden by default) ──────────────────────────────
    const settingsPanel = document.createElement('div');
    settingsPanel.id = READER_SETTINGS_ID;

    settingsPanel.innerHTML = this._buildSettingsHTML();
    overlay.appendChild(settingsPanel);

    // ── TOC Backdrop (click to close) ───────────────────────────────────
    const tocBackdrop = document.createElement('div');
    tocBackdrop.id = READER_TOC_BACKDROP_ID;

    tocBackdrop.addEventListener('click', () => this.toggleTOC());
    overlay.appendChild(tocBackdrop);

    // ── TOC Sidebar ─────────────────────────────────────────────────────
    const tocPanel = document.createElement('div');
    tocPanel.id = READER_TOC_ID;

    // TOC header
    const tocHeader = document.createElement('div');
    tocHeader.className = 'fr-toc-header';
    tocHeader.innerHTML = '<span>📖 目录</span>';
    const tocClose = document.createElement('button');
    tocClose.textContent = '✕';
    tocClose.className = 'fr-toc-close';
    tocClose.onclick = () => this.toggleTOC();
    tocHeader.appendChild(tocClose);
    tocPanel.appendChild(tocHeader);
    overlay.appendChild(tocPanel);

    // ── Search Panel (hidden by default) ────────────────────────────────
    const searchPanel = document.createElement('div');
    searchPanel.id = READER_SEARCH_ID;

    searchPanel.innerHTML = `
      <div style="display:flex;align-items:center;padding:12px 16px;border-bottom:1px solid var(--theme-border,#e0e0e0);font-size:15px;font-weight:600;color:var(--theme-text,#333);justify-content:space-between">
        <span>🔍 搜索</span>
        <button id="foliate-search-close" style="background:transparent;border:none;font-size:18px;cursor:pointer;color:var(--theme-text,#555);padding:0 4px">✕</button>
      </div>
      <div style="padding:12px 16px;border-bottom:1px solid var(--theme-border,#e0e0e0)">
        <input id="foliate-search-input" type="text" placeholder="输入搜索关键词..."
          style="width:100%;padding:8px 12px;border:1px solid var(--theme-border,#ccc);border-radius:4px;font-size:14px;background:var(--theme-bg,#fff);color:var(--theme-text,#333);outline:none;box-sizing:border-box">
        <div style="display:flex;gap:8px;margin-top:8px">
          <span id="foliate-search-count" style="font-size:12px;color:var(--theme-text,#666);flex:1;align-self:center">0 个结果</span>
          <button id="foliate-search-prev" style="padding:4px 12px;border:1px solid var(--theme-border,#ccc);background:var(--theme-bg,#fff);color:var(--theme-text,#333);border-radius:4px;cursor:pointer;font-size:12px">⬆ 上一个</button>
          <button id="foliate-search-next" style="padding:4px 12px;border:1px solid var(--theme-border,#ccc);background:var(--theme-bg,#fff);color:var(--theme-text,#333);border-radius:4px;cursor:pointer;font-size:12px">下一个 ⬇</button>
        </div>
      </div>
      <div id="${READER_SEARCH_RESULTS_ID}" style="flex:1;overflow-y:auto;padding:8px 0"></div>
    `;
    overlay.appendChild(searchPanel);

    // Search panel event listeners
    searchPanel.querySelector('#foliate-search-close').onclick = () => this.toggleSearch();
    searchPanel.querySelector('#foliate-search-input').addEventListener('keydown', (e) => {
      if (e.key === 'Enter') this._search(e.target.value);
    });
    searchPanel.querySelector('#foliate-search-prev').onclick = () => this._searchNavigate(-1);
    searchPanel.querySelector('#foliate-search-next').onclick = () => this._searchNavigate(1);

    // ── Annotation floating menu ────────────────────────────────────────
    const annotationMenu = document.createElement('div');
    annotationMenu.id = READER_ANNOTATION_MENU_ID;

    annotationMenu.innerHTML = `
      <div style="display:flex;gap:4px;align-items:center">
        <button data-ann-type="highlight" title="高亮" style="background:transparent;border:none;font-size:16px;cursor:pointer;padding:4px 6px;border-radius:4px">🖍</button>
        <button data-ann-type="underline" title="下划线" style="background:transparent;border:none;font-size:14px;cursor:pointer;padding:4px 6px;border-radius:4px;font-weight:700;text-decoration:underline">U</button>
        <button data-ann-type="wavy" title="波浪线" style="background:transparent;border:none;font-size:16px;cursor:pointer;padding:4px 6px;border-radius:4px">⌇</button>
        <button data-ann-type="note" title="笔记" style="background:transparent;border:none;font-size:16px;cursor:pointer;padding:4px 6px;border-radius:4px">📝</button>
        <span style="width:1px;height:20px;background:var(--theme-border,#ddd);margin:0 4px"></span>
        <div id="foliate-color-picker" style="display:flex;gap:2px">
          ${Object.entries(ANNOTATION_COLORS).map(([name,hex]) =>
            `<button data-color="${name}" title="${name}" style="width:18px;height:18px;border-radius:50%;border:2px solid transparent;background:${hex};cursor:pointer;padding:0"></button>`
          ).join('')}
        </div>
      </div>
    `;
    overlay.appendChild(annotationMenu);

    // Wire annotation menu buttons
    annotationMenu.querySelectorAll('[data-ann-type]').forEach(btn => {
      // Prevent the button mousedown from stealing focus / clearing the
      // in-iframe selection before _addAnnotation resolves the range.
      btn.addEventListener('mousedown', (e) => e.preventDefault());
      btn.onclick = () => this._addAnnotation(btn.dataset.annType);
    });
    annotationMenu.querySelectorAll('#foliate-color-picker button').forEach(btn => {
      btn.onclick = () => {
        annotationMenu.querySelectorAll('#foliate-color-picker button').forEach(b => b.style.borderColor = 'transparent');
        btn.style.borderColor = '#555';
      };
    });

    // ── View container ──────────────────────────────────────────────────
    const container = document.createElement('div');
    container.id = READER_VIEW_ID;

    overlay.appendChild(container);

    // ── Bottom bar ──────────────────────────────────────────────────────
    const bottombar = document.createElement('div');
    bottombar.id = READER_BOTTOMBAR_ID;

    // Progress bar
    const progress = document.createElement('div');
    progress.id = READER_PROGRESS_ID;
    progress.style.cssText = `
      flex: 1; display: flex; align-items: center; gap: 8px;
      cursor: pointer; position: relative; height: 20px;
    `;
    const track = document.createElement('div');
    track.className = 'progress-track';
    track.style.cssText = `
      flex: 1; height: 4px; background: var(--theme-border, #ddd);
      border-radius: 2px; overflow: hidden; position: relative;
    `;
    const fill = document.createElement('div');
    fill.className = 'progress-fill';
    fill.style.cssText = `
      height: 100%; width: 0%; background: var(--theme-accent, #1a73e8);
      border-radius: 2px; transition: width 0.2s ease;
    `;
    track.appendChild(fill);
    const text = document.createElement('span');
    text.className = 'progress-text';
    text.style.cssText = `
      font-size: 12px; color: var(--theme-text, #666);
      white-space: nowrap; min-width: 90px; text-align: right;
    `;
    text.textContent = '0%';
    progress.appendChild(track);
    progress.appendChild(text);

    // Page-jump input: click the % text to type a target page number.
    text.style.cursor = 'pointer';
    text.title = '点击输入页码跳转';
    let _pageInput = null;
    text.addEventListener('click', (e) => {
      e.stopPropagation();
      if (_pageInput) { _pageInput.remove(); _pageInput = null; return; }
      const total = this._totalPages || 0;
      if (total < 1) return;
      const box = document.createElement('input');
      box.type = 'number';
      box.min = 1;
      box.max = total;
      box.value = this._currentPage || 1;
      box.style.cssText = `
        width: 58px; font-size: 12px; padding: 1px 4px;
        background: var(--theme-bg, #fff); color: var(--theme-text, #333);
        border: 1px solid var(--theme-border, #ccc); border-radius: 3px;
        text-align: right;
      `;
      _pageInput = box;
      text.replaceWith(box);
      box.focus();
      box.select();
      const finish = () => {
        const raw = parseInt(box.value, 10);
        if (!isNaN(raw) && this._totalPages) {
          const p = Math.min(this._totalPages, Math.max(1, raw));
          this.view?.goToFraction?.((p - 1) / Math.max(1, (this._totalPages - 2)));
        }
        box.replaceWith(text);
        _pageInput = null;
      };
      box.addEventListener('keydown', (e2) => {
        if (e2.key === 'Enter') { e2.preventDefault(); finish(); }
        else if (e2.key === 'Escape') { box.replaceWith(text); _pageInput = null; }
      });
      box.addEventListener('blur', finish);
    });

    // Click on progress track to jump
    progress.addEventListener('click', (e) => {
      if (!this.view) return;
      const rect = track.getBoundingClientRect();
      const x = e.clientX - rect.left;
      const fraction = Math.max(0, Math.min(1, x / rect.width));
      this.view.goTo?.(fraction);
    });

    // Hover preview: show target page on the track
    const preview = document.createElement('span');
    preview.className = 'progress-preview';
    preview.style.cssText = `
      position: absolute; top: -22px; left: 0; transform: translateX(-50%);
      font-size: 11px; padding: 2px 6px; border-radius: 3px;
      background: var(--theme-text, #333); color: var(--theme-bg, #fff);
      pointer-events: none; opacity: 0; transition: opacity 0.12s ease;
      white-space: nowrap;
    `;
    track.appendChild(preview);
    progress.addEventListener('mousemove', (e) => {
      const rect = track.getBoundingClientRect();
      const x = e.clientX - rect.left;
      const frac = Math.max(0, Math.min(1, x / rect.width));
      const total = this._totalPages || 0;
      const pct = Math.round(frac * 100);
      const page = total > 1 ? Math.max(1, Math.round(frac * (total - 2) + 1)) : null;
      preview.textContent = page ? `${pct}% · 第 ${page} 页` : `${pct}%`;
      preview.style.left = x + 'px';
      preview.style.opacity = '1';
    });
    progress.addEventListener('mouseleave', () => { preview.style.opacity = '0'; });

    // TOC button
    const tocBtn = document.createElement('button');
    tocBtn.textContent = '📖';
    tocBtn.title = '目录 (T)';

    tocBtn.onclick = () => this.toggleTOC();

    // Theme quick buttons
    const themeGroup = document.createElement('div');
    themeGroup.style.cssText = 'display: flex; gap: 4px;';
    Object.keys(THEMES).forEach(name => {
      const btn = document.createElement('button');
      btn.dataset.themeBtn = name;
      btn.title = name === 'light' ? '亮色' : name === 'dark' ? '暗色' : name === 'sepia' ? '护眼' : '羊皮纸';
      btn.style.cssText = `
        width: 20px; height: 20px; border-radius: 50%;
        border: 2px solid var(--theme-border, #ccc);
        background: ${THEMES[name].bg}; cursor: pointer;
        padding: 0;
      `;
      btn.onclick = () => this.setTheme(name);
      themeGroup.appendChild(btn);
    });

    bottombar.appendChild(tocBtn);
    bottombar.appendChild(themeGroup);
    bottombar.appendChild(progress);

    // ── Overlay structure ───────────────────────────────────────────────

    overlay.appendChild(bottombar);
    document.body.appendChild(overlay);
    this.overlay = overlay;

    // Mouse move → show toolbars
    overlay.addEventListener('mousemove', () => this.showToolbars());
    overlay.addEventListener('touchstart', () => this.showToolbars());

    // ESC to close
    this._escapeHandler = (e) => {
      if (e.key === 'Escape' && this.isOpen) {
        if (getComputedStyle(settingsPanel).display !== 'none') {
          settingsPanel.style.display = 'none';
        } else {
          this.close();
        }
      }
    };
    document.addEventListener('keydown', this._escapeHandler);

    // Apply theme
    this._applyTheme();
    this._applyFontSize();
  }

  // ── Settings panel HTML ──────────────────────────────────────────────────

  // ── Show error ───────────────────────────────────────────────────────────

}

// ─── Hook into React SPA ────────────────────────────────────────────────────

function findNearbySlug(el) {
  if (!el) return null;
  if (el.dataset?.slug) return el.dataset.slug;
  let parent = el.parentElement;
  while (parent && parent !== document.body) {
    if (parent.dataset?.slug) return parent.dataset.slug;
    for (const key in parent.dataset) {
      if (key.toLowerCase().includes('slug')) return parent.dataset[key];
    }
    parent = parent.parentElement;
  }
  const img = el.closest('button')?.querySelector('img[src*="/api/v1/crawler/novels/"]')
          || el.querySelector('img[src*="/api/v1/crawler/novels/"]');
  if (img) {
    const match = img.src.match(/\/api\/v1\/crawler\/novels\/([^/]+)\/cover/);
    if (match) return decodeURIComponent(match[1]);
  }
  const href = el.getAttribute('href') || '';
  if (href) {
    const match = href.match(/(?:novels?|reader)\/([^/?&#]+)/i);
    if (match) return decodeURIComponent(match[1]);
  }
  for (const key in el.dataset) {
    if (key.toLowerCase().includes('slug')) return el.dataset[key];
  }
  return null;
}

const foliateReader = new FoliateReader();

document.addEventListener('click', (e) => {
  const btn = e.target.closest('button, a, [role="button"]');
  if (!btn) return;
  const text = btn.textContent?.trim() || '';
  const cls = btn.className || '';
  const isReadBtn = /阅读|开始阅读|Read|Start Reading|Read Book/i.test(text) ||
                     /read|阅读/i.test(cls);
  const hasCoverImg = !!btn.querySelector('img[src*="/api/v1/crawler/novels/"]');
  if (!isReadBtn && !hasCoverImg) return;
  const slug = findNearbySlug(btn);
  if (!slug) {
    const hashMatch = window.location.hash.match(/(?:novels?|reader)\/([^/?&#]+)/i);
    if (hashMatch) foliateReader.open(hashMatch[1]);
    return;
  }
  e.preventDefault();
  e.stopPropagation();
  if (btn.tagName === 'A') e.preventDefault();
  foliateReader.open(slug);
}, true);

// ─── Hash routing ───────────────────────────────────────────────────────────

let prevHash = window.location.hash;

function handleHashChange() {
  const hash = window.location.hash;
  if (hash === prevHash) return;
  prevHash = hash;
  const match = hash.match(/^#\/reader\/([^/?&#]+)/);
  if (match) {
    const slug = decodeURIComponent(match[1]);
    if (!foliateReader.isOpen || foliateReader.currentSlug !== slug) {
      foliateReader.open(slug);
    }
  }
}

window.addEventListener('load', () => { handleHashChange(); });
window.addEventListener('hashchange', handleHashChange);

globalThis.__foliateReader = foliateReader;
Object.assign(FoliateReader.prototype, UI_MIXIN);

export default foliateReader;

