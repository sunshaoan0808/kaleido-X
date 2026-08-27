/* P1-3 S2.3: _regex-part → real ESM (Story Tavern regex script engine).
 *
 * ST (SillyTavern) compatible regex pipeline: resolve the active scripts from the
 * selected character card, compile /pattern/flags, apply with placement/depth/
 * markdownOnly/promptOnly filtering and {{match}}/$N substitution.
 *
 * Consumers after conversion: chat/story/tavern-session call applyStRegexScripts,
 * partner calls compileStFindRegex — all via imports.
 */
import { $ } from './dom.js';

// `partner` still lives in the IIFE state block (converted last); reach it via
// window-free lexical scope is impossible from a real module, so resolve it at
// call time through the virtual module's exported binding.
let getPartner = () => null;
export function _bindPartner(p) { getPartner = p; }

function activeScripts() {
  const partner = getPartner();
  try {
    const ccId = ($('chat-cc') && $('chat-cc').value) || (partner && partner.selectedCharacterCardId) || '';
    const cards = (partner && partner.characterCards) || [];
    const card = cards.find((c) => c.id === ccId) || cards.find((c) => c.id === (partner && partner.selectedCharacterCardId));
    const fields = (card && card.fields) || {};
    const scripts = fields.stRegexScripts || fields.regex_scripts || [];
    return Array.isArray(scripts) ? scripts : [];
  } catch (_) {
    return [];
  }
}

export function getActiveStRegexScripts() {
  return activeScripts();
}

export function compileStFindRegex(findRegex) {
  if (!findRegex || typeof findRegex !== 'string') return null;
  let body = findRegex.trim();
  let flags = 'g';
  // /pattern/flags  or plain pattern
  if (body.startsWith('/')) {
    const last = body.lastIndexOf('/');
    if (last > 0) {
      flags = body.slice(last + 1) || 'g';
      body = body.slice(1, last);
    }
  }
  // ST often uses empty replace; ensure global for multi-match
  if (!flags.includes('g')) flags += 'g';
  try {
    return new RegExp(body, flags);
  } catch (e) {
    console.warn('st regex compile failed', findRegex, e);
    return null;
  }
}

/** ST getRegexedString (display path: isMarkdown/isPrompt flags).
 * placement: 1=USER 2=AI 3=SLASH 5=WORLD_INFO 6=REASONING
 * markdownOnly / promptOnly / neither (both false → display+prompt non-md)
 * minDepth / maxDepth optional (depth 0 = newest)
 */
export function applyStRegexScripts(text, role, opts) {
  if (text == null) return '';
  let out = String(text);
  const scripts = getActiveStRegexScripts();
  if (!scripts.length) return out;
  const o = opts || {};
  const isMarkdown = !!o.isMarkdown;
  const isPrompt = !!o.isPrompt;
  const depth = typeof o.depth === 'number' ? o.depth : null;
  const want = role === 'user' ? 1 : (role === 'world_info' ? 5 : 2);
  for (const s of scripts) {
    if (!s || s.disabled === true || s.disabled === 1) continue;
    const mdOnly = !!(s.markdownOnly || s.markdown_only);
    const promptOnly = !!(s.promptOnly || s.prompt_only);
    // ST engine.js applicability
    const applies = (mdOnly && isMarkdown)
      || (promptOnly && isPrompt)
      || (!mdOnly && !promptOnly && !isMarkdown && !isPrompt);
    if (!applies) continue;
    if (depth != null) {
      const minD = s.minDepth != null ? Number(s.minDepth) : (s.min_depth != null ? Number(s.min_depth) : null);
      const maxD = s.maxDepth != null ? Number(s.maxDepth) : (s.max_depth != null ? Number(s.max_depth) : null);
      if (minD != null && !Number.isNaN(minD) && minD >= -1 && depth < minD) continue;
      if (maxD != null && !Number.isNaN(maxD) && maxD >= 0 && depth > maxD) continue;
    }
    const placement = Array.isArray(s.placement) ? s.placement.map(Number) : [1, 2];
    const places = placement.length ? placement : [2];
    if (!places.includes(want)) continue;
    const re = compileStFindRegex(s.findRegex || s.find_regex || '');
    if (!re) continue;
    let rep = s.replaceString != null ? String(s.replaceString) : (s.replace_string != null ? String(s.replace_string) : '');
    try {
      out = out.replace(re, function () {
        const args = arguments;
        const full = args[0];
        let r = rep.replace(/\{\{match\}\}/gi, full);
        r = r.replace(/\$(\d+)/g, (_, n) => {
          const g = args[Number(n)];
          return g == null ? '' : String(g);
        });
        // trimStrings
        const trims = s.trimStrings || s.trim_strings || [];
        if (Array.isArray(trims) && trims.length) {
          for (const tr of trims) {
            if (tr) r = r.split(String(tr)).join('');
          }
        }
        return r;
      });
    } catch (e) {
      console.warn('st regex apply failed', s.scriptName || s.id, e);
    }
  }
  return out;
}
