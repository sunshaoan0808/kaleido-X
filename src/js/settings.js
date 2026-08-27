/* P1-3 S2.9: settings 系四片 → real ESM.
 * _settings-presets + _settings-core + _settings-appearance + _settings-theme.
 *
 * Closure-canonical state (settings/stylePresetsData/stylePresetSelectedId/
 * appearanceState/appearanceBlobUrl/worksOpenPath) stays in the IIFE (_state-part);
 * accessed here through the window.__kaleidoSettingsState facade (S2.6/S2.8 pattern).
 * Works-side closure fns (setWorksOpen/loadWorksTree) reached via __kaleidoWorksBridge
 * published in the works/author region of the IIFE.
 *
 * Top-level UI wiring from the old parts now lives in exported initSettingsUI(),
 * called once from main.js after virtual:app-parts evaluates (closure ready).
 * The old `loadSettings = wrapper` monkey-patch is composed directly into
 * loadSettings(); tabs/agent call sites route through the facade.
 */
import { $ } from './dom.js';
import { api, getSseTicket, getToken } from './api.js';
import { apiBase } from './api_shell.js';
import { showConfirm, showPrompt } from './dialog.js';
import { STYLE_PRESET_KEY, STYLE_PRESET_PREFIX, APPEARANCE_KEY } from './utils.js';
import { switchTab } from './tabs_bridge.js';

/** Lazy accessor for the closure-published settings-state facade. */
function SS() {
  const c = typeof window !== 'undefined' && window.__kaleidoSettingsState;
  if (!c) throw new Error('settings state facade not ready (called before _state-part evaluated)');
  return c;
}
function __wb() {
  const c = typeof window !== 'undefined' && window.__kaleidoWorksBridge;
  if (!c) throw new Error('works bridge not ready');
  return c;
}

  function normalizeStylePresets(raw) {
    if (Array.isArray(raw)) return raw;
    if (raw && typeof raw === 'object') {
      if (Array.isArray(raw.presets)) return raw.presets;
      if (Array.isArray(raw.items)) return raw.items;
      // object map id -> preset
      return Object.keys(raw).map((k) => {
        const v = raw[k];
        if (v && typeof v === 'object') return Object.assign({ id: k }, v);
        return { id: k, name: String(v) };
      });
    }
    return [];
  }
  function readAppliedStylePreset() {
    try {
      return localStorage.getItem(STYLE_PRESET_KEY) || '';
    } catch (_) {
      return '';
    }
  }
  function writeAppliedStylePreset(preset) {
    const id = (preset && (preset.id || preset.name)) || '';
    const payload = {
      id: id,
      name: (preset && (preset.name || preset.id)) || id,
      prompt: (preset && (preset.prompt || preset.content || preset.text)) || '',
      appliedAt: new Date().toISOString(),
      raw: preset || null,
    };
    try {
      localStorage.setItem(STYLE_PRESET_KEY, JSON.stringify(payload));
      // settings-visible prefix keys for other panes / future settings UI
      localStorage.setItem(STYLE_PRESET_PREFIX + 'id', payload.id || '');
      localStorage.setItem(STYLE_PRESET_PREFIX + 'name', payload.name || '');
      localStorage.setItem(STYLE_PRESET_PREFIX + 'prompt', payload.prompt || '');
      localStorage.setItem(STYLE_PRESET_PREFIX + 'applied_at', payload.appliedAt);
    } catch (_) {}
    if ($('style-presets-applied')) {
      $('style-presets-applied').value = payload.name
        ? payload.name + (payload.id ? ' (' + payload.id + ')' : '')
        : '(none)';
    }
    return payload;
  }
  function renderStylePresetsList(list) {
    const box = $('style-presets-list');
    if (!box) return;
    box.innerHTML = '';
    SS.stylePresetsData= Array.isArray(list) ? list.slice() : [];
    if (!SS.stylePresetsData.length) {
      const empty = document.createElement('div');
      empty.className = 'muted sm';
      empty.textContent = '（无预设 — 编辑 JSON 后保存）';
      box.appendChild(empty);
      return;
    }
    SS.stylePresetsData.forEach((p, idx) => {
      const el = document.createElement('div');
      const pid = p.id || p.name || String(idx);
      el.className = 'item' + (SS.stylePresetSelectedId && SS.stylePresetSelectedId === pid ? ' active' : '');
      el.innerHTML = '<span class="t"></span><span class="d"></span>';
      el.querySelector('.t').textContent = p.name || p.id || 'preset-' + idx;
      el.querySelector('.d').textContent = p.id && p.name ? p.id : p.prompt ? String(p.prompt).slice(0, 48) : '';
      el.onclick = () => {
        SS.stylePresetSelectedId= pid;
        renderStylePresetsList(SS.stylePresetsData);
        if ($('style-presets-msg')) {
          $('style-presets-msg').textContent = '已选 ' + (p.name || pid);
        }
      };
      box.appendChild(el);
    });
  }
  async function loadStylePresets() {
    const msg = $('style-presets-msg');
    if (msg) msg.textContent = '加载 style-presets…';
    try {
      const raw = await api('/api/v1/style-presets');
      const list = normalizeStylePresets(raw);
      SS.stylePresetsData= list;
      if ($('style-presets-editor')) {
        try {
          $('style-presets-editor').value = JSON.stringify(raw, null, 2);
        } catch (_) {
          $('style-presets-editor').value = '[]';
        }
      }
      renderStylePresetsList(list);
      // show currently applied
      const applied = readAppliedStylePreset();
      if ($('style-presets-applied')) {
        if (applied) {
          try {
            const obj = JSON.parse(applied);
            $('style-presets-applied').value = obj.name
              ? obj.name + (obj.id ? ' (' + obj.id + ')' : '')
              : applied.slice(0, 80);
          } catch (_) {
            $('style-presets-applied').value = applied.slice(0, 80);
          }
        } else {
          $('style-presets-applied').value = '(none)';
        }
      }
      if (msg) msg.textContent = list.length + ' 个预设';
    } catch (e) {
      if (msg) msg.textContent = e.message;
    }
  }
  async function saveStylePresetsFromEditor() {
    const msg = $('style-presets-msg');
    const ed = $('style-presets-editor');
    if (!ed) return;
    let body;
    try {
      body = JSON.parse(ed.value || '[]');
    } catch (e) {
      if (msg) msg.textContent = 'JSON 解析失败: ' + e.message;
      return;
    }
    if (!(Array.isArray(body) || (body && typeof body === 'object'))) {
      if (msg) msg.textContent = 'body 必须是数组或对象';
      return;
    }
    try {
      const saved = await api('/api/v1/style-presets', {
        method: 'PUT',
        body: JSON.stringify(body),
      });
      const list = normalizeStylePresets(saved);
      SS.stylePresetsData= list;
      renderStylePresetsList(list);
      if (msg) msg.textContent = '已保存 ' + list.length + ' 个预设';
    } catch (e) {
      if (msg) msg.textContent = e.message;
    }
  }
  function applySelectedStylePreset() {
    const msg = $('style-presets-msg');
    let preset = null;
    if (SS.stylePresetSelectedId) {
      preset = SS.stylePresetsData.find((p, idx) => {
        const pid = p.id || p.name || String(idx);
        return pid === SS.stylePresetSelectedId;
      });
    }
    if (!preset && SS.stylePresetsData.length === 1) preset = SS.stylePresetsData[0];
    if (!preset) {
      // try editor single object
      try {
        const raw = JSON.parse(($('style-presets-editor') && $('style-presets-editor').value) || 'null');
        const list = normalizeStylePresets(raw);
        if (list.length) preset = list[0];
        else if (raw && typeof raw === 'object' && !Array.isArray(raw)) preset = raw;
      } catch (_) {}
    }
    if (!preset) {
      if (msg) msg.textContent = '请先选择或编辑一个预设';
      return;
    }
    const written = writeAppliedStylePreset(preset);
    // optional: surface prompt into partner chat prompt field if empty-ish
    if ($('set-prompt') && written.prompt && !String($('set-prompt').value || '').trim()) {
      $('set-prompt').value = written.prompt;
    }
    if (msg) {
      msg.textContent =
        '已应用 ' +
        (written.name || written.id || 'preset') +
        ' → localStorage.' +
        STYLE_PRESET_KEY +
        ' / ' +
        STYLE_PRESET_PREFIX +
        '*';
    }
  }
  function collectSettingsBody() {
    const body = {
      // [酒馆对齐] 连接字段 (llmBaseUrl/llmModel/llmApiKey/modelInterface) 已移交
      // 「AI 供应商」管理；此处只提交对话参数。
      partnerChatPrompt: ($('set-prompt') && $('set-prompt').value) || '',
      temperature: parseFloat(($('set-temp') && $('set-temp').value) || '0.7') || 0.7,
      maxOutputTokens: parseInt(($('set-max') && $('set-max').value) || '4096', 10) || 4096,
      crawlerEnabled: !!( $('set-crawler') && $('set-crawler').checked ),
      tavernAdultOk: !!( $('set-tavern-adult') && $('set-tavern-adult').checked ),
    };
    const topP = $('set-top-p') && $('set-top-p').value;
    if (topP !== '' && topP != null) body.topP = clampNum(parseFloat(topP), 0, 1);
    const fp = $('set-freq-penalty') && $('set-freq-penalty').value;
    if (fp !== '' && fp != null) body.frequencyPenalty = clampNum(parseFloat(fp), -2, 2);
    const pp = $('set-presence-penalty') && $('set-presence-penalty').value;
    if (pp !== '' && pp != null) body.presencePenalty = clampNum(parseFloat(pp), -2, 2);
    return body;
  }
  async function loadActiveConn() {
    // [酒馆对齐] 只读卡片: 从 AI 供应商 active 指针拉取当前通道
    try {
      const r = await api('/api/v1/ai/active');
      const p = r && r.provider;
      const m = r && r.model;
      const set = (id, v) => { if ($(id)) $(id).textContent = v || '—'; };
      set('set-conn-name', p ? p.name : '未配置');
      set('set-conn-model', m ? (m.model_id || m.display_name) : (p && p.default_model_id) || '—');
      set('set-conn-base', p ? p.base_url : '—');
      set('set-conn-status', !p ? '未激活' : (p.status === 'enabled' ? '启用 ✓' : p.status));
      if ($('set-conn-badge')) $('set-conn-badge').hidden = !(r && r.active);
      if ($('set-info')) {
        $('set-info').textContent = p
          ? '当前通道: ' + p.name + ' → ' + (m ? (m.model_id || m.display_name) : p.default_model_id)
          : '未激活供应商，请在「管理 AI 供应商」中创建并设为当前。';
      }
    } catch (e) {
      if ($('set-info')) $('set-info').textContent = '读取当前通道失败: ' + (e.message || e);
    }
  }
  function applySettingsToForm(s) {
    SS.settings= s || {};
    if ($('set-prompt')) $('set-prompt').value = SS.settings.partnerChatPrompt || '';
    if ($('set-temp')) $('set-temp').value = SS.settings.temperature != null ? SS.settings.temperature : 0.7;
    if ($('set-top-p')) $('set-top-p').value = SS.settings.topP != null ? SS.settings.topP : '';
    if ($('set-freq-penalty')) $('set-freq-penalty').value = SS.settings.frequencyPenalty != null ? SS.settings.frequencyPenalty : '';
    if ($('set-presence-penalty')) $('set-presence-penalty').value = SS.settings.presencePenalty != null ? SS.settings.presencePenalty : '';
    if ($('set-max')) $('set-max').value = SS.settings.maxOutputTokens != null ? SS.settings.maxOutputTokens : 4096;
    if ($('set-crawler')) $('set-crawler').checked = !!SS.settings.crawlerEnabled;
    if ($('set-tavern-adult')) $('set-tavern-adult').checked = !!SS.settings.tavernAdultOk;
  }
  async function loadSettings() {
    const s = await api('/api/v1/settings');
    applySettingsToForm(s);
    await loadActiveConn();
    // P11: 设置页附带刷新服务运行状态（/health jobs_metrics）
    loadServerMetrics();
    // composed (was closure monkey-patch `loadSettings = loadSettingsAndConn`):
    try { fillAppearanceForm(loadAppearance()); } catch (_) {}
  }

  // P11: 服务运行状态 —— /health 免认证，直接 fetch（不占用 api() 的错误通道）。
  // running/queued 取自 jobs 概览字段；peak/totals/uptime 取自 jobs_metrics。
  async function loadServerMetrics() {
    const msg = $('set-metrics-msg');
    const badge = $('set-metrics-badge');
    if (!$('set-metrics-card')) return;
    try {
      const r = await fetch(apiBase() + '/health', { cache: 'no-store' });
      if (!r.ok) throw new Error('HTTP ' + r.status);
      const h = await r.json();
      const jm = h.jobs_metrics || {};
      const totals = jm.totals_since_boot || jm.totals || {};
      const set = (id, v) => { if ($(id)) $(id).textContent = v; };
      set('set-metrics-run', (h.running_jobs ?? '—') + ' / ' + (jm.peak_running_since_boot ?? '—'));
      set('set-metrics-queued', String(h.queued_jobs ?? '—'));
      set('set-metrics-totals', [totals.created, totals.succeeded, totals.failed, totals.cancelled]
        .map((x) => (x == null ? '—' : String(x))).join(' / '));
      const up = Number(jm.uptime_secs);
      set('set-metrics-uptime', Number.isFinite(up)
        ? (up >= 3600 ? (up / 3600).toFixed(1) + ' 小时' : up >= 60 ? Math.floor(up / 60) + ' 分 ' + (up % 60) + ' 秒' : up + ' 秒')
        : '—');
      if (badge) { badge.hidden = false; badge.textContent = '在线'; }
      if (msg) msg.textContent = '';
    } catch (e) {
      if (badge) { badge.hidden = true; }
      if (msg) msg.textContent = '读取 /health 失败: ' + (e.message || e);
    }
  }
  function clampNum(n, min, max) {
    const x = Number(n);
    if (Number.isNaN(x)) return min;
    return Math.max(min, Math.min(max, x));
  }
  function loadAppearance() {
    try {
      const raw = localStorage.getItem(APPEARANCE_KEY);
      if (!raw) return Object.assign({}, DEFAULT_APPEARANCE);
      const parsed = JSON.parse(raw);
      return Object.assign({}, DEFAULT_APPEARANCE, parsed || {});
    } catch (_) {
      return Object.assign({}, DEFAULT_APPEARANCE);
    }
  }
  function saveAppearance(a) {
    SS.appearanceState= Object.assign({}, DEFAULT_APPEARANCE, a || {});
    localStorage.setItem(APPEARANCE_KEY, JSON.stringify(SS.appearanceState));
    return SS.appearanceState;
  }
  function resolveColorScheme(a) {
    const mode = (a && a.mode) || 'night';
    if (mode === 'system') {
      return window.matchMedia('(prefers-color-scheme: dark)').matches ? 'night' : 'day';
    }
    // 'day-gray' rides the day base; the data-day-tone attribute flips the paper tone.
    if (mode === 'day-gray') return 'day';
    return mode === 'day' ? 'day' : 'night';
  }
  function dayToneFromAppearance(a) {
    const mode = (a && a.mode) || 'night';
    return mode === 'day-gray' ? 'gray' : 'warm';
  }
  function fontStackFromAppearance(a) {
    const family = (a && a.fontFamily) || 'system';
    const custom = ((a && a.customFontCss) || '').trim();
    const system =
      '"WenQuanYi Zen Hei", "Droid Sans Fallback", "Noto Sans CJK SC", "Noto Sans SC", "Source Han Sans SC", "PingFang SC", "Hiragino Sans GB", "Microsoft YaHei", system-ui, -apple-system, "Segoe UI", sans-serif';
    const map = {
      system,
      serif: '"WenQuanYi Zen Hei", "Noto Serif CJK SC", "Source Han Serif SC", "Songti SC", ui-serif, Georgia, serif',
      mono: 'ui-monospace, SFMono-Regular, Menlo, Consolas, monospace',
    };
    if (family === 'custom' && custom) return custom;
    const base = map[family] || system;
    return custom && family !== 'custom' ? base + ', ' + custom : base;
  }
  function clearStCustomVars() {
    const html = document.documentElement;
    // Remove previously applied ST vars we may have set
    const keys = [
      '--text', '--text-secondary', '--text-tertiary', '--bg', '--surface-0', '--surface-1',
      '--surface-2', '--surface-3', '--border', '--border-strong', '--user', '--agent',
      '--accent', '--accent-2', '--st-ember', '--st-ink',
    ];
    keys.forEach((k) => html.style.removeProperty(k));
    if (SS.appearanceState && SS.appearanceState.customCssVars) {
      Object.keys(SS.appearanceState.customCssVars).forEach((k) => {
        const name = k.startsWith('--') ? k : '--' + k;
        html.style.removeProperty(name);
      });
    }
  }
  function applyCustomCssVars(vars) {
    if (!vars || typeof vars !== 'object') return;
    const html = document.documentElement;
    Object.keys(vars).forEach((k) => {
      const name = k.startsWith('--') ? k : '--' + k;
      const val = vars[k];
      if (val == null || val === '') html.style.removeProperty(name);
      else html.style.setProperty(name, String(val));
    });
  }
  function mapStThemeToVars(obj) {
    // SillyTavern theme JSON / SmartTheme* → Kaleido tokens
    const out = {};
    if (!obj || typeof obj !== 'object') return { vars: out, wallpaperUrl: '' };
    const get = (...keys) => {
      for (const k of keys) {
        if (obj[k] != null && String(obj[k]).trim() !== '') return String(obj[k]).trim();
      }
      return '';
    };
    // classic ST theme keys
    const main = get(
      'main_text_color', 'mainTextColor', 'color_text',
      'SmartThemeBodyColor', 'smartThemeBodyColor'
    );
    const italics = get(
      'italics_text_color', 'italicsTextColor',
      'SmartThemeEmColor', 'smartThemeEmColor'
    );
    const quote = get('quote_text_color', 'quoteTextColor', 'SmartThemeQuoteColor');
    const blur = get(
      'blur_tint_color', 'blurTintColor', 'chat_tint_color', 'chatTintColor',
      'SmartThemeBlurTintColor', 'SmartThemeChatTintColor', 'SmartThemeBgColor', 'smartThemeBgColor'
    );
    const userBlur = get(
      'user_mes_blur_tint_color', 'userMesBlurTintColor',
      'SmartThemeUserMesBlurTintColor', 'SmartThemeUserMesColor'
    );
    const botBlur = get(
      'bot_mes_blur_tint_color', 'botMesBlurTintColor',
      'SmartThemeBotMesBlurTintColor', 'SmartThemeBotMesColor', 'SmartThemeAIMesColor'
    );
    const border = get(
      'border_color', 'borderColor',
      'SmartThemeBorderColor', 'smartThemeBorderColor'
    );
    const shadow = get('shadow_color', 'shadowColor', 'SmartThemeShadowColor');
    const accent = get(
      'accent_color', 'accentColor', 'SmartThemeCheckboxColor',
      'SmartThemeButtonColor', 'SmartThemeQuoteColor'
    );
    const surface2 = get('SmartThemeUnderMesBlurTintColor', 'under_mes_blur_tint_color');
    const font = get('font_family', 'fontFamily', 'font', 'SmartThemeFont');
    const bgImg = get(
      'bg_image', 'background_image', 'backgroundImage', 'wallpaper',
      'bg', 'custom_background', 'customBackground'
    );
    // nested theme objects (some ST exports)
    if (obj.colors && typeof obj.colors === 'object') {
      const nested = mapStThemeToVars(obj.colors);
      Object.assign(out, nested.vars || {});
    }
    if (main) out['--text'] = main;
    if (italics) {
      out['--text-secondary'] = italics;
      out['--accent'] = out['--accent'] || italics;
      out['--st-ember'] = out['--st-ember'] || italics;
    }
    if (quote) out['--text-tertiary'] = quote;
    if (blur) {
      out['--bg'] = blur;
      out['--surface-0'] = blur;
      out['--surface-1'] = blur;
      out['--st-ink'] = blur;
    }
    if (surface2) {
      out['--surface-2'] = surface2;
      out['--surface-3'] = surface2;
    }
    if (userBlur) out['--user'] = userBlur;
    if (botBlur) out['--agent'] = botBlur;
    if (border) {
      out['--border'] = border;
      out['--border-strong'] = border;
    }
    if (shadow) out['--shadow'] = '0 24px 80px ' + shadow;
    if (accent) {
      out['--accent'] = accent;
      out['--accent-2'] = accent;
      out['--st-ember'] = accent;
    }
    if (font) out['--font-ui'] = font;
    // Pass-through any --* or SmartTheme* already present as CSS vars
    Object.keys(obj).forEach((k) => {
      if (obj[k] == null) return;
      if (k.startsWith('--')) out[k] = String(obj[k]);
      else if (/^SmartTheme/i.test(k) && typeof obj[k] === 'string' && obj[k].trim()) {
        // keep raw for debugging + potential future mapping
        out['--st-raw-' + k] = String(obj[k]).trim();
      }
    });
    return { vars: out, wallpaperUrl: bgImg || '' };
  }
  function parseCssRootVars(text) {
    const vars = {};
    if (!text) return vars;
    // crude: --name: value;
    const re = /(--[a-zA-Z0-9-_]+)\s*:\s*([^;]+);/g;
    let m;
    while ((m = re.exec(text))) {
      vars[m[1]] = m[2].trim();
    }
    return vars;
  }
  function resolveWallpaperUrl(wall) {
    const w = (wall || '').trim();
    if (!w) return '';
    if (/^(https?:\/\/|data:|blob:)/i.test(w)) return w;
    // Relative API path — CSS background-image cannot send headers, so the
    // caller must attach a one-time ticket (M-3) before painting. Returns
    // { url } for server paths, or '' for empty/non-server values.
    if (w.startsWith('/api/')) {
      if (!getToken()) return '';
      let url = apiBase() + w;
      // cache-bust when server wallpaper changes
      if (SS.appearanceState && SS.appearanceState._wallRev) {
        url += (url.indexOf('?') >= 0 ? '&' : '?') + 'v=' + encodeURIComponent(SS.appearanceState._wallRev);
      }
      return { url };
    }
    return '';
  }
  function fileToBase64(file) {
    return new Promise((resolve, reject) => {
      const reader = new FileReader();
      reader.onload = () => {
        const r = String(reader.result || '');
        const i = r.indexOf(',');
        resolve(i >= 0 ? r.slice(i + 1) : r);
      };
      reader.onerror = () => reject(reader.error || new Error('read failed'));
      reader.readAsDataURL(file);
    });
  }
  async function pushAppearanceToServer(cfg) {
    if (!getToken()) throw new Error('未登录，无法同步');
    // merge into existing app-state
    let state = {};
    try {
      const cur = await api('/api/v1/app-state');
      state = (cur && cur.state) || {};
    } catch (_) {
      state = {};
    }
    if (!state.ui || typeof state.ui !== 'object') state.ui = {};
    // strip huge raw theme from server blob? keep capped
    const appearance = Object.assign({}, cfg || {});
    if (appearance.stThemeRaw && String(appearance.stThemeRaw).length > 100000) {
      appearance.stThemeRaw = String(appearance.stThemeRaw).slice(0, 100000);
    }
    delete appearance._wallRev;
    state.ui.appearance = appearance;
    state.ui.theme = appearance.mode || state.ui.theme || 'default';
    const saved = await api('/api/v1/app-state', {
      method: 'PUT',
      body: JSON.stringify({ state }),
    });
    return saved;
  }
  async function pullAppearanceFromServer() {
    if (!getToken()) throw new Error('未登录，无法拉取');
    const cur = await api('/api/v1/app-state');
    const state = (cur && cur.state) || {};
    const a = state.ui && state.ui.appearance;
    if (!a || typeof a !== 'object') return null;
    return Object.assign({}, DEFAULT_APPEARANCE, a);
  }
  async function uploadWallpaperFile(file) {
    if (!file) throw new Error('未选择文件');
    if (!getToken()) throw new Error('未登录');
    if (file.size > 4 * 1024 * 1024) throw new Error('壁纸超过 4MB');
    const dataBase64 = await fileToBase64(file);
    const res = await api('/api/v1/appearance/wallpaper', {
      method: 'POST',
      body: JSON.stringify({
        filename: file.name || 'wallpaper',
        contentType: file.type || 'image/png',
        dataBase64,
      }),
    });
    return res;
  }
  function applyAppearance(a, opts) {
    const cfg = Object.assign({}, DEFAULT_APPEARANCE, a || {});
    SS.appearanceState= cfg;
    const scheme = resolveColorScheme(cfg);
    const html = document.documentElement;
    html.setAttribute('data-color-scheme', scheme);
    html.setAttribute('data-day-tone', dayToneFromAppearance(cfg));
    html.setAttribute('data-theme-source', cfg.stThemeRaw ? 'st' : 'kaleido');

    // meta theme-color
    try {
      const meta = document.querySelector('meta[name="theme-color"]');
      if (meta) meta.setAttribute('content', scheme === 'day' ? '#f4efe4' : '#08090d');
    } catch (_) {}

    clearStCustomVars();
    html.style.setProperty('--font-ui', fontStackFromAppearance(cfg));
    html.style.setProperty('--wall-blur', clampNum(cfg.wallpaperBlurPx, 0, 50) + 'px');
    html.style.setProperty('--wall-op', String(clampNum(cfg.wallpaperOpacity, 0, 1)));

    let wall = (cfg.wallpaperUrl || '').trim();
    if (SS.appearanceBlobUrl && wall === SS.appearanceBlobUrl) {
      /* keep */
    }
    const resolvedWall = resolveWallpaperUrl(wall);
    const paint = (finalUrl) => {
      html.style.setProperty('--wallpaper-image', finalUrl ? 'url("' + finalUrl.replace(/"/g, '\\"') + '")' : 'none');
      const layer = $('app-wallpaper');
      if (layer) {
        layer.style.backgroundImage = finalUrl ? 'url("' + finalUrl.replace(/"/g, '\\"') + '")' : '';
        layer.classList.toggle('has-wallpaper', !!finalUrl);
      }
    };
    if (resolvedWall && typeof resolvedWall === 'object' && resolvedWall.url) {
      // Server wallpaper: exchange the bearer for a one-time ticket (M-3), then
      // paint with ?ticket=. CSS cannot send headers; tickets are single-use so
      // each apply pass mints a fresh one.
      getSseTicket().then((ticket) => {
        if (!ticket) { paint(''); return; }
        const sep = resolvedWall.url.indexOf('?') >= 0 ? '&' : '?';
        paint(resolvedWall.url + sep + 'ticket=' + encodeURIComponent(ticket));
      }).catch(() => paint(''));
    } else {
      paint(typeof resolvedWall === 'string' ? resolvedWall : '');
    }

    if (cfg.customCssVars && typeof cfg.customCssVars === 'object') {
      applyCustomCssVars(cfg.customCssVars);
    }

    // top toggle icon state
    const btn = $('theme-toggle-btn');
    if (btn) {
      btn.setAttribute('data-scheme', scheme);
      btn.title = scheme === 'day' ? '切换到夜间' : '切换到日间';
      btn.setAttribute('aria-label', btn.title);
    }

    if (!(opts && opts.skipForm)) fillAppearanceForm(cfg);
    return cfg;
  }
  function fillAppearanceForm(a) {
    const cfg = Object.assign({}, DEFAULT_APPEARANCE, a || {});
    if ($('set-theme-mode')) $('set-theme-mode').value = cfg.mode || 'night';
    if ($('set-font-family')) $('set-font-family').value = cfg.fontFamily || 'system';
    if ($('set-font-custom')) $('set-font-custom').value = cfg.customFontCss || '';
    if ($('set-wallpaper-url')) $('set-wallpaper-url').value = cfg.wallpaperUrl || '';
    if ($('set-wallpaper-blur')) {
      $('set-wallpaper-blur').value = String(clampNum(cfg.wallpaperBlurPx, 0, 50));
      if ($('set-wallpaper-blur-val')) $('set-wallpaper-blur-val').textContent = clampNum(cfg.wallpaperBlurPx, 0, 50) + 'px';
    }
    if ($('set-wallpaper-opacity')) {
      $('set-wallpaper-opacity').value = String(clampNum(cfg.wallpaperOpacity, 0, 1));
      if ($('set-wallpaper-opacity-val')) $('set-wallpaper-opacity-val').textContent = String(clampNum(cfg.wallpaperOpacity, 0, 1));
    }
    if ($('set-appearance-sync')) {
      $('set-appearance-sync').checked = cfg.syncServer !== false;
    }
  }
  function collectAppearanceFromForm() {
    return {
      mode: ($('set-theme-mode') && $('set-theme-mode').value) || 'night',
      fontFamily: ($('set-font-family') && $('set-font-family').value) || 'system',
      customFontCss: ($('set-font-custom') && $('set-font-custom').value.trim()) || '',
      wallpaperUrl: ($('set-wallpaper-url') && $('set-wallpaper-url').value.trim()) || '',
      wallpaperBlurPx: $('set-wallpaper-blur') ? clampNum($('set-wallpaper-blur').value, 0, 50) : 0,
      wallpaperOpacity: $('set-wallpaper-opacity') ? clampNum($('set-wallpaper-opacity').value, 0, 1) : 0.35,
      bgOverlay: appearanceState.bgOverlay || '',
      stThemeName: appearanceState.stThemeName || '',
      stThemeRaw: appearanceState.stThemeRaw || null,
      customCssVars: appearanceState.customCssVars || {},
      syncServer: !($('set-appearance-sync') && !$('set-appearance-sync').checked),
      wallpaperServer: !!(SS.appearanceState && SS.appearanceState.wallpaperServer),
      _wallRev: SS.appearanceState && SS.appearanceState._wallRev,
    };
  }
  function importStThemeText(text) {
    const raw = (text || '').trim();
    if (!raw) throw new Error('空内容');
    let vars = {};
    let wallpaperUrl = '';
    let name = '';
    let stThemeRaw = raw;
    if (raw.startsWith('{')) {
      const obj = JSON.parse(raw);
      name = obj.name || obj.theme_name || obj.title || '';
      const mapped = mapStThemeToVars(obj);
      vars = mapped.vars;
      wallpaperUrl = mapped.wallpaperUrl || '';
      // nested css?
      if (obj.css && typeof obj.css === 'string') {
        Object.assign(vars, parseCssRootVars(obj.css));
      }
    } else {
      vars = parseCssRootVars(raw);
      if (!Object.keys(vars).length) throw new Error('未解析到 CSS 变量或 JSON');
    }
    // Map common ST css var names if present
    const alias = {
      '--main-text-color': '--text',
      '--mainTextColor': '--text',
      '--SmartThemeBodyColor': '--text',
      '--SmartThemeEmColor': '--accent',
      '--SmartThemeBorderColor': '--border',
      '--SmartThemeBgColor': '--bg',
      '--SmartThemeBlurTintColor': '--surface-1',
      '--SmartThemeChatTintColor': '--surface-1',
      '--SmartThemeUserMesBlurTintColor': '--user',
      '--SmartThemeBotMesBlurTintColor': '--agent',
      '--SmartThemeQuoteColor': '--text-tertiary',
      '--SmartThemeShadowColor': '--shadow',
      '--SmartThemeCheckboxColor': '--accent',
      '--SmartThemeButtonColor': '--accent-2',
      '--SmartThemeUnderlineColor': '--border-strong',
    };
    Object.keys(alias).forEach((k) => {
      if (vars[k] && !vars[alias[k]]) vars[alias[k]] = vars[k];
    });
    const next = collectAppearanceFromForm();
    next.customCssVars = vars;
    next.stThemeRaw = stThemeRaw.slice(0, 200000);
    next.stThemeName = name || 'imported';
    if (wallpaperUrl && !next.wallpaperUrl) next.wallpaperUrl = wallpaperUrl;
    saveAppearance(next);
    applyAppearance(next);
    return next;
  }

  function exportAppearance() {
    const cfg = loadAppearance();
    const payload = {
      kaleidoAppearance: true,
      version: 1,
      exportedAt: new Date().toISOString(),
      appearance: cfg,
      computedScheme: resolveColorScheme(cfg),
    };
    const blob = new Blob([JSON.stringify(payload, null, 2)], { type: 'application/json' });
    const a = document.createElement('a');
    a.href = URL.createObjectURL(blob);
    a.download = 'kaleido-appearance-' + (cfg.mode || 'theme') + '.json';
    document.body.appendChild(a);
    a.click();
    setTimeout(() => {
      URL.revokeObjectURL(a.href);
      a.remove();
    }, 500);
  }

  // wire appearance controls
  (function wireAppearance() {
    SS.appearanceState= loadAppearance();
    applyAppearance(SS.appearanceState);
    // Appearance 2.0: if logged in, soft-pull server ui.appearance (local wins only if server empty)
    (async () => {
      try {
        if (!getToken()) return;
        const remote = await pullAppearanceFromServer();
        if (!remote) return;
        // Prefer server only when BOTH sides keep sync on; if either side
        // explicitly turns sync off, local appearance wins (E2E night tests
        // inject syncServer=false so server profile must not override).
        const local = loadAppearance();
        const useRemote = remote.syncServer !== false && local.syncServer !== false;
        if (!useRemote) return;
        // If local is still default-ish and remote customized, or remote has st theme / wall
        const localCustom = !!(local.stThemeName || local.wallpaperUrl || local.customFontCss ||
          (local.customCssVars && Object.keys(local.customCssVars).length) || local.mode !== 'night');
        const remoteCustom = !!(remote.stThemeName || remote.wallpaperUrl || remote.customFontCss ||
          (remote.customCssVars && Object.keys(remote.customCssVars).length) || remote.mode !== 'night');
        if (remoteCustom || !localCustom) {
          saveAppearance(remote);
          applyAppearance(remote);
        }
      } catch (_) {}
    })();

    if ($('set-wallpaper-blur')) {
      $('set-wallpaper-blur').oninput = () => {
        const v = clampNum($('set-wallpaper-blur').value, 0, 50);
        if ($('set-wallpaper-blur-val')) $('set-wallpaper-blur-val').textContent = v + 'px';
        document.documentElement.style.setProperty('--wall-blur', v + 'px');
      };
    }
    if ($('set-wallpaper-opacity')) {
      $('set-wallpaper-opacity').oninput = () => {
        const v = clampNum($('set-wallpaper-opacity').value, 0, 1);
        if ($('set-wallpaper-opacity-val')) $('set-wallpaper-opacity-val').textContent = String(v);
        document.documentElement.style.setProperty('--wall-op', String(v));
      };
    }
    if ($('set-wallpaper-file')) {
      $('set-wallpaper-file').onchange = () => {
        const f = $('set-wallpaper-file').files && $('set-wallpaper-file').files[0];
        if (!f) return;
        if (SS.appearanceBlobUrl) {
          try { URL.revokeObjectURL(SS.appearanceBlobUrl); } catch (_) {}
        }
        SS.appearanceBlobUrl= URL.createObjectURL(f);
        if ($('set-wallpaper-url')) $('set-wallpaper-url').value = SS.appearanceBlobUrl;
        document.documentElement.style.setProperty('--wallpaper-image', 'url("' + SS.appearanceBlobUrl + '")');
        const layer = $('app-wallpaper');
        if (layer) {
          layer.style.backgroundImage = 'url("' + SS.appearanceBlobUrl + '")';
          layer.classList.add('has-wallpaper');
        }
        if ($('set-appearance-msg')) {
          $('set-appearance-msg').textContent = '已载入本地预览；点「上传壁纸到服务器」可持久化（≤4MB）';
        }
      };
    }
    if ($('set-wallpaper-upload')) {
      $('set-wallpaper-upload').onclick = async () => {
        try {
          const f = $('set-wallpaper-file') && $('set-wallpaper-file').files && $('set-wallpaper-file').files[0];
          if (!f) throw new Error('请先选择本地图片');
          if ($('set-appearance-msg')) $('set-appearance-msg').textContent = '上传中…';
          const res = await uploadWallpaperFile(f);
          const next = collectAppearanceFromForm();
          next.customCssVars = SS.appearanceState.customCssVars || {};
          next.stThemeRaw = SS.appearanceState.stThemeRaw || null;
          next.stThemeName = SS.appearanceState.stThemeName || '';
          next.wallpaperUrl = (res && res.url) || '/api/v1/appearance/wallpaper';
          next.wallpaperServer = true;
          next._wallRev = String(Date.now());
          if (SS.appearanceBlobUrl) {
            try { URL.revokeObjectURL(SS.appearanceBlobUrl); } catch (_) {}
            SS.appearanceBlobUrl= '';
          }
          saveAppearance(next);
          applyAppearance(next);
          if (next.syncServer !== false) {
            try { await pushAppearanceToServer(next); } catch (e) {
              if ($('set-appearance-msg')) {
                $('set-appearance-msg').textContent = '壁纸已上传，但 app-state 同步失败: ' + (e.message || e);
              }
              return;
            }
          }
          if ($('set-appearance-msg')) {
            $('set-appearance-msg').textContent =
              '壁纸已上传 · ' + (res && res.bytes ? res.bytes + 'B · ' : '') + (res && res.contentType ? res.contentType : 'ok');
          }
        } catch (e) {
          if ($('set-appearance-msg')) $('set-appearance-msg').textContent = '上传失败: ' + (e.message || e);
        }
      };
    }
    if ($('set-wallpaper-clear')) {
      $('set-wallpaper-clear').onclick = async () => {
        try {
          if (getToken()) {
            await api('/api/v1/appearance/wallpaper', { method: 'DELETE', body: '{}' });
          }
          const next = collectAppearanceFromForm();
          next.customCssVars = SS.appearanceState.customCssVars || {};
          next.stThemeRaw = SS.appearanceState.stThemeRaw || null;
          next.stThemeName = SS.appearanceState.stThemeName || '';
          next.wallpaperUrl = '';
          next.wallpaperServer = false;
          next._wallRev = String(Date.now());
          saveAppearance(next);
          applyAppearance(next);
          if (next.syncServer !== false && getToken()) {
            try { await pushAppearanceToServer(next); } catch (_) {}
          }
          if ($('set-appearance-msg')) $('set-appearance-msg').textContent = '已清除服务器壁纸';
        } catch (e) {
          if ($('set-appearance-msg')) $('set-appearance-msg').textContent = '清除失败: ' + (e.message || e);
        }
      };
    }
    if ($('set-appearance-save')) {
      $('set-appearance-save').onclick = async () => {
        try {
          const next = collectAppearanceFromForm();
          next.customCssVars = SS.appearanceState.customCssVars || {};
          next.stThemeRaw = SS.appearanceState.stThemeRaw || null;
          next.stThemeName = SS.appearanceState.stThemeName || '';
          next.wallpaperServer = SS.appearanceState.wallpaperServer || false;
          // if blob preview still selected, warn — not durable
          if (next.wallpaperUrl && next.wallpaperUrl.indexOf('blob:') === 0) {
            if ($('set-appearance-msg')) {
              $('set-appearance-msg').textContent = '当前壁纸是本地 blob，刷新会丢；请先「上传壁纸到服务器」';
            }
          }
          saveAppearance(next);
          applyAppearance(next);
          let syncNote = '仅本机';
          if (next.syncServer !== false) {
            try {
              await pushAppearanceToServer(next);
              syncNote = '已同步账号';
            } catch (e) {
              syncNote = '同步失败: ' + (e.message || e);
            }
          }
          if ($('set-appearance-msg')) {
            $('set-appearance-msg').textContent =
              '外观已保存 · ' + resolveColorScheme(next) +
              (next.stThemeName ? ' · ST:' + next.stThemeName : '') +
              ' · ' + syncNote;
          }
        } catch (e) {
          if ($('set-appearance-msg')) $('set-appearance-msg').textContent = '保存失败: ' + (e.message || e);
        }
      };
    }
    if ($('set-appearance-pull')) {
      $('set-appearance-pull').onclick = async () => {
        try {
          if ($('set-appearance-msg')) $('set-appearance-msg').textContent = '拉取中…';
          const remote = await pullAppearanceFromServer();
          if (!remote) {
            if ($('set-appearance-msg')) $('set-appearance-msg').textContent = '服务器尚无外观配置';
            return;
          }
          saveAppearance(remote);
          applyAppearance(remote);
          if ($('set-appearance-msg')) {
            $('set-appearance-msg').textContent =
              '已从服务器拉取 · ' + resolveColorScheme(remote) +
              (remote.stThemeName ? ' · ST:' + remote.stThemeName : '');
          }
        } catch (e) {
          if ($('set-appearance-msg')) $('set-appearance-msg').textContent = '拉取失败: ' + (e.message || e);
        }
      };
    }
    if ($('set-appearance-reset')) {
      $('set-appearance-reset').onclick = async () => {
        if (SS.appearanceBlobUrl) {
          try { URL.revokeObjectURL(SS.appearanceBlobUrl); } catch (_) {}
          SS.appearanceBlobUrl= '';
        }
        clearStCustomVars();
        const next = Object.assign({}, DEFAULT_APPEARANCE);
        saveAppearance(next);
        applyAppearance(next);
        if ($('set-st-theme-paste')) $('set-st-theme-paste').value = '';
        if (next.syncServer !== false && getToken()) {
          try { await pushAppearanceToServer(next); } catch (_) {}
        }
        if ($('set-appearance-msg')) $('set-appearance-msg').textContent = '已恢复默认墨金夜间';
      };
    }
    if ($('set-st-import')) {
      $('set-st-import').onclick = async () => {
        try {
          let text = ($('set-st-theme-paste') && $('set-st-theme-paste').value) || '';
          const fileEl = $('set-st-theme-file');
          if ((!text || !text.trim()) && fileEl && fileEl.files && fileEl.files[0]) {
            text = await fileEl.files[0].text();
            if ($('set-st-theme-paste')) $('set-st-theme-paste').value = text.slice(0, 50000);
          }
          const next = importStThemeText(text);
          if ($('set-appearance-msg')) {
            $('set-appearance-msg').textContent =
              '已导入主题' + (next.stThemeName ? ' · ' + next.stThemeName : '') +
              ' · 变量 ' + Object.keys(next.customCssVars || {}).length;
          }
        } catch (e) {
          if ($('set-appearance-msg')) $('set-appearance-msg').textContent = '导入失败: ' + (e.message || e);
        }
      };
    }
    if ($('set-st-export')) {
      $('set-st-export').onclick = () => {
        try {
          exportAppearance();
          if ($('set-appearance-msg')) $('set-appearance-msg').textContent = '已导出 JSON';
        } catch (e) {
          if ($('set-appearance-msg')) $('set-appearance-msg').textContent = '导出失败: ' + (e.message || e);
        }
      };
    }
    if ($('theme-toggle-btn')) {
      $('theme-toggle-btn').onclick = () => {
        const cur = loadAppearance();
        const scheme = resolveColorScheme(cur);
        cur.mode = scheme === 'day' ? 'night' : 'day';
        saveAppearance(cur);
        applyAppearance(cur);
        if (cur.syncServer !== false && getToken()) {
          pushAppearanceToServer(cur).catch(() => {});
        }
      };
    }
    // system preference live update
    try {
      window.matchMedia('(prefers-color-scheme: dark)').addEventListener('change', () => {
        const cur = loadAppearance();
        if (cur.mode === 'system') applyAppearance(cur, { skipForm: false });
      });
    } catch (_) {}
  })();



/** One-time UI wiring (was top-level side effects in _settings-presets/_settings-core). */


export function initSettingsUI() {
    if ($('style-presets-refresh')) {
      $('style-presets-refresh').onclick = () => loadStylePresets();
    }
    if ($('style-presets-save')) {
      $('style-presets-save').onclick = () => saveStylePresetsFromEditor();
    }
    if ($('style-presets-apply')) {
      $('style-presets-apply').onclick = () => applySelectedStylePreset();
    }
    window.__kaleidoLoadStylePresets = loadStylePresets;
    $('works-rename').onclick = async () => {
      if (!SS.worksOpenPath) return;
      const to = await showPrompt('重命名为（相对作品根路径）', { value: SS.worksOpenPath });
      if (!to || to === SS.worksOpenPath) return;
      try {
        await api('/api/v1/works/rename', {
          method: 'POST',
          body: JSON.stringify({ from: SS.worksOpenPath, to }),
        });
        __wb().setWorksOpen(to, $('works-content').value);
        await __wb().loadWorksTree();
        $('works-msg').textContent = '已重命名 → ' + to;
      } catch (e) {
        $('works-msg').textContent = e.message;
      }
    };

    $('works-delete').onclick = async () => {
      if (!SS.worksOpenPath) return;
      if (!await showConfirm('删除文件 ' + SS.worksOpenPath + '？')) return;
      try {
        await api('/api/v1/works?path=' + encodeURIComponent(SS.worksOpenPath), { method: 'DELETE' });
        __wb().setWorksOpen('', '');
        await __wb().loadWorksTree();
        $('works-msg').textContent = '已删除';
      } catch (e) {
        $('works-msg').textContent = e.message;
      }
    };

    const wv = window.__kaleidoWorksBridge && window.__kaleidoWorksBridge.loadWorksVersionsSidebar;
    if (wv) window.__kaleidoLoadWorksVersions = wv;
    if ($('set-save')) {
      $('set-save').onclick = async () => {
        if ($('set-msg')) $('set-msg').textContent = '保存中…';
        try {
          const body = collectSettingsBody();
          const saved = await api('/api/v1/settings', {
            method: 'PATCH',
            body: JSON.stringify(body),
          });
          // Prefer server echo (includes ok/saved) then hard re-GET to prove disk persistence
          applySettingsToForm(saved);
          const again = await api('/api/v1/settings');
          applySettingsToForm(again);
          if ($('set-msg')) {
            $('set-msg').textContent = '已保存 ✓（立即生效，无需重启）';
          }
        } catch (e) {
          if ($('set-msg')) $('set-msg').textContent = '保存失败: ' + (e.message || e);
        }
      };
    }
    if ($('set-test')) {
      $('set-test').onclick = async () => {
        if ($('set-msg')) $('set-msg').textContent = '测试中…';
        try {
          const body = collectSettingsBody();
          // persist dialog params first so test uses them
          await api('/api/v1/settings', { method: 'PATCH', body: JSON.stringify(body) });
          const r = await api('/api/v1/llm/test', {
            method: 'POST',
            body: JSON.stringify({ model: body.llmModel || undefined }),
          });
          await loadSettings();
          if (r && r.ok) {
            if ($('set-msg')) {
              $('set-msg').textContent =
                '连通 OK · ' +
                (r.model || '') +
                ' · ' +
                (r.latencyMs != null ? r.latencyMs + 'ms' : '') +
                (r.sample || r.content ? ' · sample: ' + String(r.sample || r.content).slice(0, 40) : '');
            }
          } else {
            if ($('set-msg')) {
              $('set-msg').textContent = '测试失败: ' + ((r && (r.error || r.body)) || 'unknown');
            }
          }
        } catch (e) {
          if ($('set-msg')) $('set-msg').textContent = '测试失败: ' + e.message;
        }
      };
    }
    if ($('set-goto-aiadmin')) {
      $('set-goto-aiadmin').onclick = () => {
        try { switchTab('aiadmin'); } catch (_) {
          document.querySelectorAll('[data-tab="aiadmin"]').forEach((el) => el.click());
        }
      };
    }
    // P11: 手动刷新服务运行状态
    if ($('set-metrics-refresh')) {
      $('set-metrics-refresh').onclick = () => loadServerMetrics();
    }

let llmModelsCache = [];

const DEFAULT_APPEARANCE = {
      mode: 'night',
      fontFamily: 'system',
      customFontCss: '',
      wallpaperUrl: '',
      wallpaperBlurPx: 0,
      wallpaperOpacity: 0.35,
      bgOverlay: '',
      stThemeName: '',
      stThemeRaw: null,
      customCssVars: {},
      syncServer: true,
      wallpaperServer: false,
};

/** One-time UI wiring (was top-level side effects in _settings-presets/_settings-core). */
}

try { window.__kaleidoSettings = { loadSettings, loadStylePresets }; } catch (_) {}

export { loadSettings, loadStylePresets };
