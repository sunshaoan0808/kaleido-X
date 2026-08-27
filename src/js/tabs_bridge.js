/* P1-3 S2.6: routing-core bridge — real ESM exports over the IIFE facade.
 *
 * The tab-routing core (switchTab/switchAzView/applyAutoUi/…, `currentTab`)
 * still lives inside the concatenated-IIFE virtual module (_tabs-part.js).
 * Its ~50 inward dependencies (loadPartner/stRefresh/tavernSession/…) are
 * closure bindings that cannot be imported until those parts convert — so a
 * full extraction is impossible today. Instead _tabs-part.js publishes a
 * facade object (`window.__kaleidoTabs`, constructed INSIDE the closure where
 * every binding resolves locally — no free variables, no S2.5-style DCE) and
 * this module lazily re-exposes it as true ESM exports.
 *
 * Consumers:
 *   - api_shell.js showMain(): the showMain↔switchTab circular reference is
 *     now a real import (was: unresolved identifiers resolved only by esbuild
 *     scope-flattening).
 *   - _keyboard-part conversion (S2.7): imports switchTab/switchAzView/
 *     closeToolsSheet/closeSessionDrawer directly.
 *
 * currentTab keeps shared-`let` semantics via getCurrentTab/setCurrentTab —
 * _agent-part writes it at startup; inner IIFE parts keep their lexical
 * access to the same binding.
 */

function api() {
  const t = typeof window !== 'undefined' && window.__kaleidoTabs;
  if (!t) throw new Error('tabs facade not ready (called before _tabs-part evaluated)');
  return t;
}

export function switchTab(name, opts) { return api().switchTab(name, opts); }
export function switchAzView(name, opts) { return api().switchAzView(name, opts); }
export function applyAutoUi() { return api().applyAutoUi(); }
export function parseLocationHash() { return api().parseLocationHash(); }
export function parseHashSegments(raw) { return api().parseHashSegments(raw); }
export function writeHashForTab(name) { return api().writeHashForTab(name); }
export function openToolsSheet() { return api().openToolsSheet(); }
export function closeToolsSheet() { return api().closeToolsSheet(); }
export function openSessionDrawer() { return api().openSessionDrawer(); }
export function closeSessionDrawer() { return api().closeSessionDrawer(); }
export function updateImmersive() { return api().updateImmersive(); }
export function exitImmersive() { return api().exitImmersive(); }

export function getCurrentTab() { return api().currentTab; }
export function setCurrentTab(v) { api().currentTab = v; }
