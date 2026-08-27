/**
 * Kaleido P1-3 — Vite build pipeline (stage 1: behavior-identical bundler swap)
 *
 * The 42 _*-part.js files still share one IIFE closure (cross-part calls without
 * imports — legacy "fake modularity", see docs/P1_3_VITE_MIGRATION.md). Until each
 * part is converted to a real ES module, we preserve the concatenation semantics
 * through a virtual module so Vite/Rollup produces the same app.js that build.js
 * (esbuild) used to.
 *
 * Output contract (unchanged, server + mobile WebView depend on it):
 *   web/assets/app.js      — IIFE bundle, es2020
 *   web/assets/styles.css  — bundled css (+ font assets)
 *   web/index.html         — ?v= stamp bumped by scripts/stamp-html.mjs
 */
import { defineConfig } from 'vite';
import fs from 'node:fs';
import path from 'node:path';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const jsDir = path.resolve(__dirname, 'src/js');

function appPartsVirtual() {
  return {
    name: 'kaleido-app-parts',
    resolveId(id, importer) {
      if (id === 'virtual:app-parts') return '\0virtual:app-parts';
      // relative ESM imports emitted inside the virtual module ('./utils.js' etc.)
      // must resolve against src/js, since the virtual module has no real path
      if (importer === '\0virtual:app-parts' && id.startsWith('.')) {
        return path.resolve(jsDir, id);
      }
    },
    load(id) {
      if (id !== '\0virtual:app-parts') return;
      // imports live at the top of index.js; parts are concatenated inside the IIFE
      const indexJs = fs.readFileSync(path.join(jsDir, 'index.js'), 'utf8');
      const imports = indexJs
        .split('\n')
        .filter((l) => l.startsWith('import '))
        .join('\n');
      // P1-3 S2.x: converted parts (real ESM modules) must stay visible to the
      // IIFE parts via lexical scope — prepend their imports so hoisted bindings
      // resolve. Re-export aliases keep the old bare-identifier names working:
      //   api/setApiBase/getSseTicket → ./api.js (was _api/_setApiBase/_getSseTicket)
      //   isCapacitor/normalizeBase   → ./utils.js (was local wrappers)
      //   tabs routing core           → ./tabs_bridge.js — S2.6: aliased imports
      //     (__tab*) are NOT bound over the IIFE parts' own switchTab etc. (those
      //     closure bindings stay canonical); they exist only so this module can
      //     re-export the bridge and satisfy Rollup's export resolution.
      const converted = [
        'import { $ } from "./dom.js";',
        'import { username, setUsername, loginView, mainView, loginErr, messagesEl, sessionList, input, stopBtn, sendBtn, ST_VISIBLE_TURNS, stHistoryExpanded } from "./state.js";',
        'import { PLAYABLE_LABELS, ST_ICONS, stripChoicesBlock, resolveMessageOptions } from "./utils.js";',
        // S2.10: tavern 系 as real ESM — exports consumed by the closure parts below
        'import { stApi, stStatus, stGoBack, stSwitchView, stDisplayTitle, stHasOpenOverlay, stBindImmChrome, stRefresh, stLoadPacks, stLoadSessions, stLoadSaves, stLoadSession, stRefreshCharSummary, stRenderContinueCard, renderHomeRecent, loadBookshelf } from "./tavern.js";',
        // S2.11: jobs as real ESM — exports consumed by the closure parts below
        'import { setPanel, refreshJobs } from "./jobs.js";',
        // S2.13: aiadmin/moa as real ESM — tabs' guarded calls become always-true
        'import { P5AiLoad } from "./aiadmin.js";',
        'import { MoaLoadPanels, MoaLoadSessions } from "./moa.js";',
        // S2.14: insight 域 (analysis/graph/foreshadow) as real ESM
        'import { loadAnKinds, loadAnTasks, loadGraph, loadForeshadows } from "./insight.js";',
        // S2.15: story as real ESM
        'import { advShowReader, advShowSetup, ensureStorySession, renderBondPage, renderStoryMessages } from "./story.js";',
        // S2.16: agent/partner as real ESM
        'import { renderAsVisual, loadEmbedLabStatus, loadEmbedLabEvents } from "./agent.js";',
        'import { loadPartner } from "./partner.js";',
        // S2.17: authoring (works+author) as real ESM — tabs' guarded calls
        // become always-true; escapeHtml consumers inside _tabs keep closure binding
        'import { loadAuthorProjects, loadWorksTree, refreshPackSelect, loadWorksVersionsSidebar } from "./authoring.js";',
        'import { friendlyError, apiBase, showLogin, showMain } from "./api_shell.js";',
        'import { getActiveStRegexScripts, compileStFindRegex, applyStRegexScripts } from "./st_regex.js";',
        'import { _bindPartner as __bp } from "./st_regex.js"; __bp(() => partner);',
        'import { wireListSearch, openGlobalSearch, closeGlobalSearch } from "./search.js";',
        'import { loadSettings, loadStylePresets } from "./settings.js";',
        'import { renderMessages, refreshSessions, showChatSetup, setupChatStart, ensureSession, saveSession, closeEs, setStreaming, scheduleChatStreamPaint, cssEscape, buildBubbleEl, fillBubbleBody, ensureBubbleDom, refreshPartnerSelects, refreshStorySelects, refreshAdventureSelects, sendMessage, stopStream } from "./chat.js";',
        'import { switchTab as __tabSwitchTab, switchAzView as __tabSwitchAzView, applyAutoUi as __tabApplyAutoUi, parseLocationHash as __tabParseLocationHash, getCurrentTab as __tabGetCurrentTab } from "./tabs_bridge.js";',
        'export { friendlyError, apiBase, showLogin, showMain, getActiveStRegexScripts, compileStFindRegex, applyStRegexScripts, wireListSearch, openGlobalSearch, closeGlobalSearch };',
        'import { api as _apiReal, setApiBase as _setApiBaseReal, getSseTicket as _getSseTicketReal } from "./api.js";',
        'const api = _apiReal, setApiBase = _setApiBaseReal, getSseTicket = _getSseTicketReal;',
      ];
      // S2.19: parts.json retired — no IIFE body remains; the virtual module
      // resolves to just the import preamble (kept for api_shell re-export
      // compatibility until its consumers migrate off it).
      const body = '';
      return `${imports}\n${converted.join('\n')}\n(function () {\n${body}\n})();\n`;
    },
  };
}

export default defineConfig({
  plugins: [appPartsVirtual()],
  build: {
    outDir: 'web/assets',
    emptyOutDir: false,
    target: 'es2020',
    minify: 'esbuild',
    cssCodeSplit: false,
    rollupOptions: {
      input: { app: path.resolve(__dirname, 'src/main.js') },
      output: {
        format: 'iife',
        entryFileNames: 'app.js',
        // Vite emits the bundled css as 'style.css'; index.html expects 'styles.css'
        assetFileNames: (info) =>
          /\.css$/.test(info.names?.[0] ?? info.name ?? '') ? 'styles.css' : '[name].[ext]',
      },
    },
  },
});
