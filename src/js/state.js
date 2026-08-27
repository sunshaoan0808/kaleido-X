/* P1-3 S2.2: state extracted from the IIFE into a real ESM (first slice).
 *
 * Owns the shared element references + tiny consts that used to live in
 * _dom-part.js tail / _api-part.js consumers:
 *   - element refs: loginView, mainView, loginErr, messagesEl, sessionList,
 *                   input, stopBtn, sendBtn  (all read-only bindings)
 *   - ST_VISIBLE_TURNS (const), stHistoryExpanded (let — S2.10 收编：closure copy
 *     removed; tavern.js reads the live binding + writes via setStHistoryExpanded)
 *   - username (let; login writes it via setUsername)
 *
 * The big mutable state block of _state-part.js stays in the IIFE for now —
 * that part is deliberately LAST (see docs/P1_3_VITE_MIGRATION.md).
 */
import { $ } from './dom.js';
import { USER_KEY } from './utils.js';

export const ST_VISIBLE_TURNS = 3; // S8.25: fold older than last N 对话 rounds

export let stHistoryExpanded = false;

/** S2.10 收编：tavern.js writes via this setter (ESM imports are read-only).
 * The duplicate closure let in _state-part.js was removed — this binding is
 * now the single source of truth for all readers (closure + modules). */
export function setStHistoryExpanded(v) { stHistoryExpanded = v; }

export const loginView = $('login-view');
export const mainView = $('main-view');
export const loginErr = $('login-err');
export const messagesEl = $('messages');
export const sessionList = $('session-list');
export const input = $('input');
export const stopBtn = $('stop-btn');
export const sendBtn = $('send-btn');

export let username = localStorage.getItem(USER_KEY) || '';

export function setUsername(u) { username = u || ''; }
