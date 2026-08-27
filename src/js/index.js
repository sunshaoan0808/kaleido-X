/* Kaleido web shell — S9.20 JS modularization
 *
 * The IIFE body has been split into 16 part files (_*-part.js).
 * build.js concatenates them inside the IIFE wrapper at build time.
 *
 * Module structure:
 *   _dom-part.js       — DOM cache, utils, helpers (51 lines)
 *   _state-part.js     — State declarations: const/let (65 lines)
 *   _api-part.js       — API, login, showMain (48 lines)
 *   _tabs-part.js      — Tab routing, immersive, hash (652 lines)
 *   _regex-part.js     — ST regex scripts (98 lines)
 *   _chat-part.js      — Chat: render, send, session (394 lines)
 *   _partner-part.js   — Partner: wb/cc/regex (774 lines)
 *   _works-part.js     — Works IDE (254 lines)
 *   _author-part.js    — Author zone AZ-4 (834 lines)
 *   _settings-part.js  — Settings + appearance (1242 lines)
 *   _story-part.js     — Story/bond/adventure (611 lines)
 *   _jobs-part.js      — Jobs/background/travel (1005 lines)
 *   _agent-part.js     — Agent/crawl/embed lab (629 lines)
 *   _tavern-part.js    — Story Tavern ST-3 (2992 lines)
 *   _search-part.js    — List search + global search (87 lines)
 *   _keyboard-part.js  — Keyboard + perf + init (227 lines)
 */
import { $ as _$, TOKEN_KEY, USER_KEY, SID_KEY, STORY_SID_KEY, API_BASE_KEY, STYLE_PRESET_KEY, STYLE_PRESET_PREFIX, ADULT_OK_KEY, TAVERN_SID_KEY, ST_READPOS_PREFIX, APPEARANCE_KEY, DEFAULT_REMOTE as _DEFAULT_REMOTE, formatDateTime as _fmtDT, displayTitle as _dispTitle, shortId as _shortId, uid as _uid, isCapacitor as _isCap, normalizeBase as _normBase, clamp as _clamp } from './utils.js';
import { api as _api, getToken as _getToken, setToken as _setToken, clearToken as _clearToken, setApiBase as _setApiBase, getSseTicket as _getSseTicket } from './api.js';
import { showToast as _showToast, tryApi as _tryApi } from './toast.js';
import { showConfirm as _showConfirm, showPrompt as _showPrompt } from './dialog.js';
