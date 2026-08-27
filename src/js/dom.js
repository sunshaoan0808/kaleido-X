/* P1-3 S2.1: first part converted from IIFE concatenation to a real ES module.
 *
 * Previously the head of _dom-part.js inside the shared IIFE ("DOM cache, utils,
 * helpers"). The cachedQuery family had ZERO consumers anywhere in src/js (only a
 * self-export window.domCache with no readers), so it is intentionally NOT carried
 * over — dead code removed.
 *
 * What remains meaningful: the `$` alias that ~40 parts call as `$('element-id')`.
 * utils.js already exports `$` = getElementById, so this module re-exports it to
 * keep `import { $ } from './dom.js'` available while parts migrate one by one.
 *
 * Consumers during transition:
 *   - parts still inside the IIFE reach `$` via outer lexical scope
 *     (vite.config.mjs prepends `import { $ } from './dom.js'` into the virtual module).
 *   - after full ESM conversion, import { $ } directly where needed.
 */
export { $ } from './utils.js';
