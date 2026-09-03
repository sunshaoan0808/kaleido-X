/* state_core.js — P1-3 S2.19: final IIFE part (_state-part) as real ESM.
 * Verbatim ownership move: canonical lets + element consts stay HERE; the
 * window.__kaleido* facade publishes are unchanged, so every consumer module
 * (__c7()/__auth()/__s8()/__wk()/__az()/__pf() accessors) keeps working with
 * zero edits. With this, parts.json is empty and virtual:app-parts retires.
 */
import { $ } from './dom.js';
import { displayTitle as _dispTitle } from './utils.js';
import { formatDateTime as _fmtDT } from './utils.js';
import { TOKEN_KEY, USER_KEY, SID_KEY, STORY_SID_KEY } from './utils.js';
import { showToast as _showToast } from './toast.js';
import { showConfirm as _showConfirm, showPrompt as _showPrompt } from './dialog.js';
import { shortId as _shortId } from './utils.js';
import { tryApi as _tryApi } from './toast.js';

/* ---- migrated from _dom-part.js tail (P1-3 S2.1) — shared element refs & consts ---- */
const ST_VISIBLE_TURNS = 3; // S8.25: fold older than last N 对话 rounds
// S2.10 收编: stHistoryExpanded now lives ONLY in state.js (closure readers see it
// via the converted[] import line; tavern writes via setStHistoryExpanded).
const loginView = $('login-view');
const mainView = $('main-view');
const loginErr = $('login-err');
const messagesEl = $('messages');
const sessionList = $('session-list');
const input = $('input');
const stopBtn = $('stop-btn');
const sendBtn = $('send-btn');

/* State declarations */
  function formatDateTime(value) { return _fmtDT(value); }

  /** Humanize session titles: drop Untitled / bare ids when possible. */
  function displayTitle(raw, fallback) { return _dispTitle(raw, fallback); }

  /** Shorten long technical ids for secondary list lines. */
  function shortId(id) { return _shortId(id); }

  let token = localStorage.getItem(TOKEN_KEY) || '';
  let username = localStorage.getItem(USER_KEY) || '';
  let anWsId = localStorage.getItem('kaleido_ws_id') || ''; // analysis workspace id (login response)
  let sessionId = localStorage.getItem(SID_KEY) || '';
  let messages = [];
  let activeRunId = null;
  let es = null;
  let streaming = false;
  // Story tab state (S5-W2 T1)
  let storySessionId = localStorage.getItem(STORY_SID_KEY) || '';
  let storyMessages = [];
  let storyActiveRunId = null;
  let storyEs = null;
  let storyStreaming = false;
  let partner = {
    worldBooks: [],
    characterCards: [],
    selectedWorldBookId: null,
    selectedCharacterCardId: null,
  };
  let editWbId = '';
  let editCcId = '';
  let editWbEntries = [];
  let editWbEntryId = '';
  let editCcRegexScripts = [];
  let editCcRegexIdx = -1;
  let editRxScripts = [];
  let editRxIdx = -1;
  let regexLibraryMeta = { priority: 'card_over_library', updatedAt: 0 };
  let settings = {};
  // AZ-4 author zone state
  let azProjects = [];
  let azSelectedProjectId = '';
  // workspace 域（work_id=workspace_id）——关系图/伏笔/AI分析查询数据用（2026-08-10）
  let azSelectedWorkspaceId = '';
  let azSelectedProjectRoot = '';
  let azSelectedCharIds = new Set();
  let azSelectedWbIds = new Set();
  let azSelectedPlayable = 'P1';
  let azBoundSessionId = '';
  let worksCwd = '';
  let worksOpenPath = '';
  let worksDirty = false;
  // S7-W2 works preview shell (source | split | preview)
  let worksPreviewMode = 'source';
  let worksPreviewTimer = null;
  // S7-W2 desk: versions sidebar + style presets
  let worksVersionsCache = [];
  let stylePresetsData = [];
  let stylePresetSelectedId = '';
  // ST-3 story tavern state — S2.10: moved to src/js/tavern.js as canonical module state
  // (closure consumers read via tavern.js exports stCurrentSession()/stCurrentPack()).
  const showToast = _showToast;
  const tryApi = _tryApi;
  const showConfirm = _showConfirm;
  const showPrompt = _showPrompt;


/* S2.8: chat-domain shared state facade — published INSIDE the closure so the
 * real module src/js/chat.js can read/write the canonical lets (messages,
 * sessionId, streaming, es, activeRunId, partner) without bundler scope tricks. */
try {
  window.__kaleidoChatState = {
    get messages() { return messages; }, set messages(v) { messages = v; },
    get sessionId() { return sessionId; }, set sessionId(v) { sessionId = v; },
    get streaming() { return streaming; }, set streaming(v) { streaming = v; },
    get es() { return es; }, set es(v) { es = v; },
    get activeRunId() { return activeRunId; }, set activeRunId(v) { activeRunId = v; },
    get partner() { return partner; }, set partner(v) { partner = v; },
  };
} catch (_) {}



/* S2.16: partner-edit facade — real module src/js/partner.js reads/writes the
 * worldbook/character-card/regex editor state lets. */
try {
  window.__kaleidoPartnerEdit = {
    get editWbId() { return editWbId; }, set editWbId(v) { editWbId = v; },
    get editCcId() { return editCcId; }, set editCcId(v) { editCcId = v; },
    get editWbEntries() { return editWbEntries; }, set editWbEntries(v) { editWbEntries = v; },
    get editWbEntryId() { return editWbEntryId; }, set editWbEntryId(v) { editWbEntryId = v; },
    get editCcRegexScripts() { return editCcRegexScripts; }, set editCcRegexScripts(v) { editCcRegexScripts = v; },
    get editCcRegexIdx() { return editCcRegexIdx; }, set editCcRegexIdx(v) { editCcRegexIdx = v; },
    get editRxScripts() { return editRxScripts; }, set editRxScripts(v) { editRxScripts = v; },
    get editRxIdx() { return editRxIdx; }, set editRxIdx(v) { editRxIdx = v; },
    get regexLibraryMeta() { return regexLibraryMeta; }, set regexLibraryMeta(v) { regexLibraryMeta = v; },
  };
} catch (_) {}

/* S2.9: settings-domain state facade (settings/stylePresets/appearance +
 * worksOpenPath read-only). Canonical lets stay here; real module settings.js
 * accesses via these accessors. */
try {
  let appearanceState = {};
  let appearanceBlobUrl = '';

  window.__kaleidoSettingsState = {
    get settings() { return settings; }, set settings(v) { settings = v; },
    get stylePresetsData() { return stylePresetsData; }, set stylePresetsData(v) { stylePresetsData = v; },
    get stylePresetSelectedId() { return stylePresetSelectedId; }, set stylePresetSelectedId(v) { stylePresetSelectedId = v; },
    get appearanceState() { return appearanceState; }, set appearanceState(v) { appearanceState = v; },
    get appearanceBlobUrl() { return appearanceBlobUrl; }, set appearanceBlobUrl(v) { appearanceBlobUrl = v; },
    get worksOpenPath() { return worksOpenPath; },
  };
} catch (_) {}

/* S2.17: author-zone + works-domain state facades — real module
 * src/js/authoring.js reads/writes the canonical az-star / works-star lets. */
try {
  window.__kaleidoAzState = {
    get azProjects() { return azProjects; }, set azProjects(v) { azProjects = v; },
    get azSelectedProjectId() { return azSelectedProjectId; }, set azSelectedProjectId(v) { azSelectedProjectId = v; },
    get azSelectedWorkspaceId() { return azSelectedWorkspaceId; }, set azSelectedWorkspaceId(v) { azSelectedWorkspaceId = v; },
    get azSelectedProjectRoot() { return azSelectedProjectRoot; }, set azSelectedProjectRoot(v) { azSelectedProjectRoot = v; },
    get azSelectedCharIds() { return azSelectedCharIds; }, set azSelectedCharIds(v) { azSelectedCharIds = v; },
    get azSelectedWbIds() { return azSelectedWbIds; }, set azSelectedWbIds(v) { azSelectedWbIds = v; },
    get azSelectedPlayable() { return azSelectedPlayable; }, set azSelectedPlayable(v) { azSelectedPlayable = v; },
    get azBoundSessionId() { return azBoundSessionId; }, set azBoundSessionId(v) { azBoundSessionId = v; },
  };
} catch (_) {}

try {
  window.__kaleidoWorksState = {
    get worksCwd() { return worksCwd; }, set worksCwd(v) { worksCwd = v; },
    get worksOpenPath() { return worksOpenPath; }, set worksOpenPath(v) { worksOpenPath = v; },
    get worksDirty() { return worksDirty; }, set worksDirty(v) { worksDirty = v; },
    get worksPreviewMode() { return worksPreviewMode; }, set worksPreviewMode(v) { worksPreviewMode = v; },
    get worksPreviewTimer() { return worksPreviewTimer; }, set worksPreviewTimer(v) { worksPreviewTimer = v; },
    get worksVersionsCache() { return worksVersionsCache; }, set worksVersionsCache(v) { worksVersionsCache = v; },
  };
} catch (_) {}

/* S2.10: auth facade — real module src/js/tavern.js reads the canonical token let. */
try {
  window.__kaleidoAuthState = {
    get token() { return token; }, set token(v) { token = v; },
    // S2.16: agent.js login/logout flows read/write these
    get username() { return username; }, set username(v) { username = v; },
    get anWsId() { return anWsId; }, set anWsId(v) { anWsId = v; },
  };
} catch (_) {}

/* S2.10: story-domain facade — tavern assist modal reads story lets (was
 * typeof-guarded closure access; real modules can't typeof undeclared bindings). */
try {
  window.__kaleidoStoryState = {
    get storySessionId() { return storySessionId; },
    get storyMessages() { return storyMessages; },
    // S2.15: story.js (real module) reads/writes the stream-state lets
    get activeRunId() { return storyActiveRunId; }, set activeRunId(v) { storyActiveRunId = v; },
    get es() { return storyEs; }, set es(v) { storyEs = v; },
    get streaming() { return storyStreaming; }, set streaming(v) { storyStreaming = v; },
  };
} catch (_) {}

export { settings, partner, messages, sessionId };
