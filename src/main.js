// Kaleido app entry (P1-3 stage 2)
//
// dom.js is the first part converted to a real ESM (S2.1). It must execute
// BEFORE the IIFE part-concat virtual module: parts inside the IIFE reference
// `$` lexically, and Rollup hoists imported bindings so the shared scope sees it.
// As more parts convert, their imports move here / into index.js.
import { $ } from './js/dom.js'; // eslint-disable-line no-unused-vars — re-exported for IIFE parts via hoisting
import './js/state_core.js'; // S2.19: canonical state + window facades as real ESM
import './js/tabs.js'; // S2.18: tab routing core as real ESM (was _tabs-part; facade publish kept)
import './js/keyboard.js'; // S2.7: keyboard shortcuts as real ESM (was IIFE part 37/38; late listener registration preserved)
import './js/chat.js'; // S2.8: chat render/sessions + send/stream trio as real ESM (was _chat-part + _settings-chat)
import { initSettingsUI } from './js/settings.js'; // S2.9: settings UI wiring runs after closure eval
initSettingsUI();
import { initTavernUI } from './js/tavern.js'; // S2.10: tavern 系 as real ESM (was 10 IIFE parts)
initTavernUI();
import { initJobsUI } from './js/jobs.js'; // S2.11: jobs 域 as real ESM (was _jobs-part)
initJobsUI();
import './js/wand.js'; // S2.12: wand 工具簇 8 片 as real ESM (top-level side effects on import)
import './js/aiadmin.js'; // S2.13: AI 供应商管理 as real ESM
import './js/moa.js'; // S2.13: 模型对比 as real ESM
import './js/insight.js'; // S2.14: AI 分析域 3 片 as real ESM
import './js/story.js'; // S2.15: story/bond as real ESM
import './js/agent.js'; // S2.16: agent 沙箱 as real ESM
import './js/partner.js'; // S2.16: partner 域 as real ESM
import './js/authoring.js'; // S2.17: works+作者区 as real ESM（合并模块，见文件头注释）
import './css/main.css';
