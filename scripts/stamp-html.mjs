#!/usr/bin/env node
/**
 * Cache-bust stamp for web/index.html (moved out of build.js, shared by both
 * build chains). Mobile WebView caches ?v= URLs; without a new stamp updates
 * never reach devices.
 */
import fs from 'node:fs';

const indexPath = new URL('../web/index.html', import.meta.url);
const stamp = Math.floor(Date.now() / 1000);
let html = fs.readFileSync(indexPath, 'utf8');
html = html.replace(/(app\.js\?v=)\d+/g, '$1' + stamp);
html = html.replace(/(styles\.css\?v=)\d+/g, '$1' + stamp);
fs.writeFileSync(indexPath, html);
console.log(`✓ stamped index.html v=${stamp}`);
