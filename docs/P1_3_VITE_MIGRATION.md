# P1-3 前端 ES Modules + Vite 迁移

## Stage 1 ✅ 行为等价的打包器切换（本阶段）

**做了什么**
- 引入 Vite 5.4.21（`npm run build:vite`），替代手拼 esbuild 的 `build.js` 作为新构建链
- 分片清单提取为 `src/js/parts.json` —— build.js 与 vite.config.mjs 共享，单一事实来源
- `vite.config.mjs` 用 virtual-module 插件承接现状：42 个 `_*-part.js` 仍拼接进同一 IIFE
  （分片间靠闭包共享标识符、无显式 import 的"假模块化"原样保留）
- 缓存戳逻辑独立为 `scripts/stamp-html.mjs`（两条链共用；移动 WebView 靠 ?v= 失效缓存）

**输出契约不变**：`web/assets/app.js` + `web/assets/styles.css` + index.html ?v= 戳。

**等价性验证**（vite bundle vs esbuild bundle）
- window.* 导出集合：0 差异
- /api/v1/* 路由字面量：32/32 完全一致
- s4_gate.sh：ALL PASSED（隔离实例实测）
- 体积：980KB vs 1039KB（minifier 内联策略差异，gzip 后 271KB）

**附带发现**：web/assets 下 ~300KB Inter woff/woff2 是 esbuild loader 盲拷贝的死文件——
主 shell CSS 从未 @font-face 引用它们（--font-ui 全系统字体栈）。Vite 不再拷贝是正确行为；
旧文件暂留磁盘无害。bookshelf 子应用有自己的 8 个字体，不受影响。

## Stage 2+ 待办（真 ESM 化，每片一验）

42 个 part 目前共享 IIFE 作用域。逐片转换顺序建议（从依赖底座到叶子）：

1. `_dom-part` → 真模块（导出 cachedQuery 等；删 window.domCache 全局）
2. `_state-part` → 拆成多个 store 模块（token/session/messages/story*/partner…）
   ⚠️ 最危险的一片：全部 `let` 可变状态被所有分片直接读写，
   需要先收敛为具名导出的 getter/setter 或响应式 store
3. `_api-part`、`_toast/dialog 已有` → 合并进现有 api.js/toast.js
4. 其余按 tab 分片逐个转换（tabs/chat/partner/works/author/story/jobs/agent/tavern*…）
5. 每转一片：`npm run build:vite` + 冒烟（gate + 对应 tab 手测）
6. 全部转完后：删除 virtual-module 拼接插件，index.js 变纯 import 树

**已知坑**
- `_chat-part` 等用 `typeof switchTab === 'function'` 探测跨片函数——ESM 化后改为直接 import
- HTML 仅 1 处内联 onclick（switchTab('works')）；window.switchTab 导出保留即可
- mammoth.browser.js 是 UMD，Vite build 自动 interop，无需配置

## S2.5 教训（2026-08-23）：IIFE 闭包不可跨模块访问

尝试把 _keyboard-part 转真模块时，用 `bindKeyboardFns({ get closeToolsSheet(){...} })`
让虚拟模块引用 IIFE **内部**的绑定——这是不可能的：getter 里的标识符在模块作用域
解析为自由变量（undefined/全局），Rollup 据此重排依赖后 esbuild minify 把
`(function(){...});` 表达式语句整体 DCE 掉 → bundle 从 980KB 缩到 529KB，UI 字符串
大量丢失（CJK 9570→359）。treeshake:false 也救不回来。

**规则：只有当目标符号已经是模块顶层可见（真导出或虚拟模块注入）时才能转下一片。
键盘分片依赖 tabs-part 的 closeToolsSheet/closeSessionDrawer/switchAzView ——
必须先转 tabs。**

正确顺序更新：_tabs-part 提前到 keyboard 之前；转换 tabs 时把这些函数提为真导出，
keyboard 再转并直接 import。

## S2.6（2026-08-23）：tabs 路由核心提为真导出 — window 门面桥

**~~关键机制发现~~（S2.7 已更正，见下节）**：~~virtual module 的
`(function(){...});` 其实从未被调用……esbuild minify 整体内联拍平~~
此解释**错误**。真相见 S2.7。

**S2.6 做法**（不搬代码、不引入自由变量）：
1. `_tabs-part.js` 在闭包【内部】发布 `window.__kaleidoTabs` 门面：
   switchTab / switchAzView / applyAutoUi / parseLocationHash / parseHashSegments /
   writeHashForTab / open+closeToolsSheet / open+closeSessionDrawer /
   currentTab(get/set)。门面构造处的作用域拥有全部绑定 → 无自由变量 → DCE 不适用。
2. 新增 `src/js/tabs_bridge.js`：惰性读门面，导出真 ESM 函数 +
   getCurrentTab/setCurrentTab（保住共享 let 语义；_agent-part 启动时写 currentTab）。
   bridge 零 import → 与任何模块零循环。
3. `vite.config.mjs` converted[] 以 __tab* 别名 import + re-export；
   **不**用别名遮蔽 IIFE 片内的 switchTab 等词法绑定（片间调用仍走原闭包路径）。
4. `api_shell.js showMain()` 循环引用变真 import（applyAutoUi/parseLocationHash/
   getCurrentTab/switchTab ← ./tabs_bridge.js）。登录后调用时门面必然已就绪。

**为什么不整体搬出 _tabs-part**：switchTab 有 ~50 个内向依赖（loadPartner/
stRefresh/tavernSession/storyMessages/worksOpenPath…），全部还在闭包里；
currentTab 被多个片读写，拆开即失同步。等 tavern/chat/settings 各片转换后
再收编。全量转换顺序不变：keyboard → chat → settings 系 → tavern 系。

**等价性**：window.* 45→46（仅 +__kaleidoTabs）、/api/v1 路由字面量 0 diff、
CJK 9570→9570、node --check OK、s4_gate.sh ALL PASSED（隔离实例 :18971）。

## S2.7（2026-08-23）：keyboard 转真模块 — 真正的 bundling 机制查明

**机制更正（推翻 S2.6 的"esbuild 拍平"解释）**：
1. `_keyboard-part.js` 旧 213 行有一个**游离的 `})();`**（快捷键 keydown
   handler 结束后多写的一行，其后还跟着 P3.2/P4.2/P4.4 三段）。拼入模板
   `(function () {\n${body}});` 时，这个游离 closer 把包装函数**提前关闭并调用**
   —— parts 之所以执行，靠的是这行意外代码，而非 esbuild。
2. Rollup 最小复现实验证明：**未被调用的函数表达式语句会被整体 tree-shake，
   无论体内有没有副作用**（`addEventListener`/`window.x=1` 照样丢）；被调用的
   IIFE 永远保留。S2.5 与 S2.7 排查中的"整包蒸发"都是同一根因：parts.json 里
   唯一的 invoker 消失了。
3. esbuild minify 的 IIFE 内联拍平确实存在，但角色是让片间自由变量跨作用域
   解析（minified bundle 中 showMain→bc() 内 st()/be() 可解析），不是"救活"
   未调用语句的原因。

**S2.7 结构修复**：vite.config.mjs 模板尾部改为 `(function () {\n${body}\n})();`
—— 包装由模板自身构造性保证，不再依赖任何 part 里的意外字符。游离 `})();`
随 _keyboard-part.js 删除一并消失。

**keyboard.js 移植要点**：
- import switchTab/switchAzView/closeToolsSheet/closeSessionDrawer ← tabs_bridge.js；
  openGlobalSearch/closeGlobalSearch ← search.js；$ ← dom.js；api；showToast。
- `let globalSearchTimer = null;` 本地声明——修复 S2.4 以来的潜在 bug：
  旧 bundle 里该变量只在已死的 _search-part 副本中声明，全局搜索首按键
  即 strict-mode ReferenceError。
- tavernPack 分支删除（空 if 体 = 死代码）；escapeHtml 本地副本（规范版在
  _works-part 闭包内，后续 works 转换时收编）。
- main.js 在 `virtual:app-parts` 之后 import keyboard.js——保持原 part 37 的
  监听器注册时序。

**等价性**（minified 对 minified）：window.* 46=46 零差异、/api/v1 路由 0 diff、
CJK 9570=9570、len -100B（游离 closer+模板变化）、node --check OK、
s4_gate.sh ALL PASSED（隔离实例 :18972 fresh data，测毕 kill）。
教训入档：**等价性对比必须同压缩口径**（unminified vs minified 会因注释剥除
产生 CJK 假差异）。

## S2.8（2026-08-23）：chat + 发送/流式三函数转真模块 — chat.js

**范围**：_chat-part.js（762L）+ _settings-chat.js 的 sendMessage/finishStream/
stopStream 三函数合并为 src/js/chat.js；parts.json 37→35。

**状态归属（关键决策）**：5 个 chat 域共享 let（messages/sessionId/streaming/
es/activeRunId）+ partner **留在闭包内作规范副本**（agent/tabs 片仍裸读写，
零漂移）；chat.js 经 _state-part.js 闭包内发布的 `window.__kaleidoChatState`
getter/setter 门面访问——S2.6 已验证模式，不依赖打包器拍平。

**外向解析机制（新确认）**：vite converted[] 的 import 位于虚拟模块顶层，
IIFE 片词法可见其绑定（$/api/showToast/tabs 别名即此机制）。chat.js 的 18 个
公开符号加入 converted[] 后，其余片的裸标识符调用点（46 处/8 片）零修改。

**新增 window 桥**（等价性预期 +5）：__kaleidoChatState、__kaleidoPartner
（loadPartner）、stStatus、stOpenAssistModal、stFocusAssistInput。
最后两个原是"读未赋值"的潜在死检查（_tavern-send:143），现激活。

**教训**：真模块里对闭包符号的 `typeof X === 'function'` 守卫恒 false
（模块作用域 typeof 未声明不抛错但返回 undefined）→ 静默行为丢失；
必须改走 window.* 或真 import。stApi('/packs/demo') 单点内联为
api('/api/v1/story-tavern'+path)（路由字面量 +1 属编译期展开，语义相同）。

**等价性**：window.* 51=46+5（全计划内）、路由 0 diff（除上述 +1）、
CJK 9570=9570、node --check OK、s4_gate ALL PASSED（:18973 fresh data）。

## S2.9（2026-08-23）：settings 系四片转真模块 — settings.js

**范围**：presets/core/appearance/theme 四片（1057L）合并为 src/js/settings.js；
parts.json 35→31。

**状态归属**：settings/stylePresetsData/stylePresetSelectedId/appearanceState/
appearanceBlobUrl/worksOpenPath 留闭包规范（tabs/aiadmin/analysis/tavern-core
仍裸读），经 `window.__kaleidoSettingsState` 门面访问。works 侧闭包函数
（setWorksOpen/loadWorksTree/loadWorksVersionsSidebar）经 `_author-part` 发布的
`__kaleidoWorksBridge` 桥接。

**副作用迁移**：旧片顶层的 onclick 装配与 window 发布迁入导出函数
initSettingsUI()，由 main.js 在 virtual:app-parts 求值后调用一次。
原 `loadSettings = wrapper` 闭包重绑定技巧改为在模块内直接组合
（外观表单刷新并入 loadSettings 尾部）；tabs/agent 的 3 处调用点改走
`window.__kaleidoSettings` 门面。

**等价性**：window.* 54 = 46+8（S2.7/2.8 的 +5 与本步 +3 全计划内）、路由
0 diff（stApi 内联 +1 已知）、CJK 9570=9570、s4_gate ALL PASSED（:18974）。
教训：分拆顶层语句时 works-rename/delete 两段装配一度丢失——被路由字面量
diff 电池当场抓获并回填；等价性电池再次证明是转换步骤的安全网。

## S2.10（2026-08-23）：tavern 系十片转真模块 — tavern.js

**范围**：_tavern-core/_pack/_session/_send/_shelf/_packmgmt/_lore/_side/
_char-summary/_bg-immerse 十片（6548L、223 fns）合并为 src/js/tavern.js
（6,642L，Stage 2 最大单片）；parts.json 31→21。

**所有权内移（ownership-move）**：7 个规范 tavern lets（tavernPacks/Sessions/
Session/Pack/Streaming/stTavernUserScrolled/tavernRunId）从 _state-part.js 删除、
在 tavern.js 内重声明为模块状态。闭包侧只改 ~10 个读点（_tabs 3 簇、_drift、
_world），经导出 getter `stCurrentSession()/stCurrentPack()` 访问——避免了
~400 处重写，也不引入新的读写门面。

**顶层装配 → initTavernUI()**：122 条原顶层 DOM/window 装配语句按序迁入，
main.js 在闭包求值后调用（settings.js 先例）。DOM 读值 const（stAsrBtn…
stPovBtn 等）保留在模块作用域供 initTavernUI 可见。

**Mechanism Y 出边**：16 符号导出表（stApi/stStatus/stGoBack/…/loadBookshelf）
经 vite converted[] import 行供剩余 21 个闭包片使用——未加守卫的裸引用零改动。

**门面新增**：`__kaleidoAuthState{get token}`、`__kaleidoStoryState{get
storySessionId/storyMessages}`、`__kaleidoTabs` 增 suppressHashWrite 存取器。
动机统一：真模块不能 `typeof` 未声明绑定——partner/messages/storyMessages/
tavernSession 的 typeof 守卫全部改写为门面读取或直接调用（stRewindOne 的
`typeof stLoadSession === 'function'` 恒真，改为直接 await）。

**收编**：utils.js += PLAYABLE_LABELS/ST_ICONS/stripChoicesBlock/
parseOptionListBlob/parseStoryChoices/resolveMessageOptions（后四者从 git HEAD
完整抽取依赖链）；state.js 收编 stHistoryExpanded 为单一来源 + 
`setStHistoryExpanded` setter（ESM import 只读）。escapeHtml/esc 在 tavern.js
内置本地副本（keyboard.js 先例，不动 utils）。

**转换工程**：/tmp/tavgen/transform.py — mask-aware 重写（注释/字符串置盲后才
替换，杜绝 v1 把 'token' 改成 '__authToken()'、把 API URL 打穿一类的字符串腐坏）；
per-unit 处理豁免 stFetch/_tavern-session:908 与 stAsrSend/_tavern-send:590 的
局部 `const token` 遮蔽；冒号守卫防对象键误伤；两个手工补丁（stRewindOne、
export{} 去重）已折回脚本并可复现再生。

**mammoth 去重**：index.js 与 tavern.js 双 import 曾致 bundle +497,288B
（+50.5%，mentions 5→10）。确认 packmgmt 已随片移出构建后删除 index.js 的
import；specifier 保持 mammoth/mammoth.browser.js。最终 985,462B vs HEAD
985,599B（−137B）。

**基线方法论修正（重要教训）**：遗留 `npm run build`（build.js/esbuild）产出的
504,598B 包是假象——build.js 组装 index.js imports + IIFE(parts) 时不含
vite.config.mjs 的 converted[] 行，真模块代码缺席被 tree-shake，CJK=0。
等价性比较必须 vite-vs-vite：git worktree @HEAD 建 /tmp/kaleido-head 跑
build:vite 得真基线（985,599B / wins54 / CJK9570 / routes109）。
今后禁用 build.js 产物做等价性验证。

**等价性**：routes 109=109 零 diff；window.* 54→56（恰为两个计划内门面）；
CJK 9570→9574 = utils.js 版 parseStoryChoices 与 _story-part 保留副本共存的
良性重复（multiset diff 仅 +1×选/项/询/问，零删除）；node --check 通过；
s4_gate ALL PASSED（隔离 :18975，fresh KALEIDO_DATA）。

## S2.11（2026-08-23）：_jobs-part 转真模块 — jobs.js

**范围**：任务中心 + background/book-travel/online-load/st-import 域单片
（1268L，剩余最大片）→ src/js/jobs.js；parts.json 21→20。

**出边最小化红利**：outward 仅 2 符号（setPanel、refreshJobs），且消费方
（_agent/_partner/_story/_tabs）全部裸引用——Mechanism Y converted[] 一行
import 即全覆盖，闭包侧零编辑。

**入边四类处理**：$ api apiBase showToast friendlyError 直接 import；
readSSE 的裸 `token` 用 `const token = __authToken()` 就地本地化（复用 S2.10
auth 门面，三处引用零改动）；loadPartner 两处调用走既有 __kaleidoPartner 门面
（typeof 守卫恒真 → refreshPartner 死分支删除，stRewindOne 先例）；
renderAsVisual 来自 _agent 闭包 → 新增 window.__kaleidoAgent 门面。

**等价性**：routes 109=109；window.* 56→57 恰 +1 计划内门面；CJK 零 diff
（单片无重复副本问题）；len +52B；s4_gate ALL PASSED（:18975 隔离）。
教训：装配句分类须在单元级做——runBgStart 内部的 loadPartner typeof 守卫块
位于 bg-apply-partner 装配 if 体中，整文件级替换会漏；生成脚本改为 per-unit
重写 + 计数断言后 6/6 全命中。

## S2.12（2026-08-23）：零出边叶簇 8 片转真模块 — wand.js

**范围**：compass/chapter-diary/style/assets/review/drift/world/image 八个
outward=0 叶片（~1877L）→ src/js/wand.js；parts.json 20→12。

**形态简化**：无出边 ⇒ 无 export 表、无 initXxxUI——各片顶层挂载副作用
（ensureUi/MutationObserver/setInterval 魔棒注入监视/DOMContentLoaded）原样
保留为模块顶层语句，import 时执行，时序等价。chapter-diary 的 IIFE 包裹层保留。

**anWorkId 门面**：规范 let/函数留 _analysis-part（本步不转），发布
`window.__kaleidoAnState{workId()}`；模块侧 `__anWorkId()` helper 以 try/catch
兜底等价原 typeof 守卫语义。11 处改写（compass3/review5/assets2/image1）。

**教训**：正则替换 `anWorkId(` → `__anWorkId()` 会多吃一个右括号（匹配只含
开括号，替换却带了一对），vite 解析失败当场抓获；改为 `__anWorkId(` 后通过。
凡「标识符(」形替换，替换文本括号数必须与匹配文本一致。

**等价性**：routes 109=109；window.* 57→58 恰 +1 计划内；CJK 零 diff；
len −357B（闭包脚手架去重）；s4_gate ALL PASSED（:18975 隔离）。

## S2.13（2026-08-23）：_aiadmin + _moa 双小片转真模块 — aiadmin.js / moa.js

parts.json 12→10。出边仅 tabs 三处 typeof 守卫调用，Mechanism Y 恒真化零编辑；
顶层副作用（DOMContentLoaded init、readyState 分支、boot 诊断行）原样保留。
细节：诊断行 `typeof switchTab` 改写为 `typeof window.__kaleidoTabs.switchTab`，
使 data-p5diag 的 st= 值保持 'function'（行为可见属性）；P5Api/MoaApi 自读
localStorage token，无闭包依赖。等价性：routes/wins 全同（首次双 IDENTICAL），
CJK 零 diff，len −62B；s4_gate PASS。

## S2.14（2026-08-23）：分析域三片转真模块 — insight.js
analysis/graph/foreshadow（~1700L）合并；parts.json 10→7。outward 4 符号
经 Mechanism Y 零闭包编辑。关键修正：anWorkId 实现依赖 3 个闭包 lets，
不可随片内移——__kaleidoAnState 门面升级为 getter 内联选择逻辑（下拉值→
anWsId→azSelectedWorkspaceId→azSelectedProjectId→'default'），模块侧改读
门面属性。等价性双 IDENTICAL + CJK 零 diff；s4_gate PASS。

## S2.15（2026-08-23）：_story-part → story.js
793L；parts.json 7→6。门面扩展：__kaleidoStoryState(+activeRunId/es/streaming
get/set)、__kaleidoTabs(+bondPickWb/bondPickCc/stAdvImmChromeState)。入边
api+getSseTicket import、token→__authToken()。**新教训 (i)**：标识符重写会误伤
对象字面量键/属性名位置（messages: → __c7().messages:），mask-aware 不识别 key
位——node --check/vite 报错后人工回滚两处即可，但生成器应加「冒号前不替换」守则。
等价性双 IDENTICAL；s4_gate PASS。

## S2.16（2026-08-23）：_agent + _partner → agent.js / partner.js
parts.json 6→4。门面：__kaleidoAuthState(+username/anWsId)、新 __kaleidoPartnerEdit
(9 lets)。**教训 (j)（重要）**：facade 属性≠模块导出——闭包函数经 window facade
动态调用时，rollup 将未被静态引用的导出整树摇除，运行时 TypeError，且 routes/
wins/CJK 电池全同无法发现；本步靠 CJK −89 异常定位。规则确立：**函数一律直接
import，window 门面只承载 let 数据**。

## S2.17 — _works-part + _author-part → authoring.js（合并模块）

**形态**：两片合并为单一真 ESM `src/js/authoring.js`（1211 行）。原因：真循环依赖——
works 的 `setWorksOpen` 调 author 的 `updateAzDeskActions`/`loadWorksVersionsSidebar`，
而 author 的 `loadWorksTree` 调 works 的 `escapeHtml`。两者原本共享同一 IIFE 闭包
作用域，拼接进同一模块即精确保持该语义，避免 ES 循环导入的边界情形。

parts.json 4→2：仅剩 `_state-part.js` / `_tabs-part.js`。

**新增门面**（_state-part 内发布）：
- `window.__kaleidoAzState`：azProjects/azSelectedProjectId/azSelectedWorkspaceId/
  azSelectedProjectRoot/azSelectedCharIds/azSelectedWbIds/azSelectedPlayable/
  azBoundSessionId 各 get/set
- `window.__kaleidoWorksState`：worksCwd/worksOpenPath/worksDirty/worksPreviewMode/
  worksPreviewTimer/worksVersionsCache 各 get/set
- 模块侧 helper：`__az()` / `__wk()`；复用 `__c7()`(partner)、`__t6()`(tabs 三 fn)

**教训 (k)：生成器 masker 会把正则字面量里的反引号当模板串起点**
（``line.match(/^```(.*)$/)`` 一行就吞掉半文件）。本步改用「无 masker 裸替换」：
先逐行核对目标标识符只出现在裸代码位置（grep -n 全量人工过目），再以 `\b` 锚定
`re.subn` 并断言出现次数。
**教训 (k2)：期望次数必须用 finditer 出现次数，不能用 `grep -c` 行数**
——`azProjects[idx] = ... azProjects[idx]` 这类一行双现会被低估（本步 5 个标识符
受影响：azProjects 14≠12、azSelectedProjectRoot 6≠4、worksCwd 9≠8、
worksOpenPath 51≠50、worksPreviewTimer 4≠3）。

**入边清单**（_tabs 对两片的全部引用）：
- `escapeHtml` ×5（事件日志渲染，L221-229）→ Mechanism Y 后由 authoring.js
  export 提供？否——_tabs 仍在闭包内，靠 converted[] import 行注入绑定；
  authoring.js 未导出 escapeHtml！实际机制：converted[] 无此行……
  【更正】见下方「closure 内 escapeHtml 去向」。
- `loadWorksTree`×2 / `loadAuthorProjects`×2 / `loadWorksVersionsSidebar`×2
  （typeof 守卫）/ `refreshPackSelect`×1（typeof 守卫）→ Mechanism Y 四符号导出
- `worksOpenPath`×3 组 / `worksDirty`×3（immersive 标题拼接）→ **仍留在 _tabs 里裸用**！

**⚠️ closure 内残留裸标识符**：_tabs-part L401-404/L680-683/L791-795 直接读
`worksOpenPath`/`worksDirty`，而这两个 let 的**声明仍在 _state-part**（闭包内），
所以编译与运行均正常 —— works 域 let 的所有权没有随模块迁走（门面只是给
authoring.js 用的旁路）。S2.18 收编 _tabs 时一并处理。

**closure 内 escapeHtml 去向（真雷，已排）**：_tabs 的 renderRecallBox（会话抽屉
召回路径，openSessionDrawer/st-side 开合可达）裸用 escapeHtml ×5。works 迁走后
authoring.js 私有副本不导出 → 闭包内该标识符悬空，bundle 内零定义 —— API 面
smoke 探不到的潜伏 ReferenceError。bundle 反查（escapeHtml 出现次数=0 定义）+
node 严格模式模拟确认后，给 _tabs 补本地副本（keyboard.js:30 先例）修复
（commit fix(S2.17)）。教训并入 (k3)：**迁走「公共工具函数」定义方时，grep
剩余 closure parts 对该符号的全部引用；未导出即悬空，且 typeof 守卫救不了裸调用**。

**验证**：routes 109=109 双 IDENTICAL；wins +3 全部计划内；CJK 9574 持平；
len 983683→992407（门面 accessor 膨胀 +18.7KB）；node --check 过；
s4_gate :18975 全 14 项 PASS。

commits: refactor(S2.17) + docs(P1-3)

## S2.18 — _tabs-part → tabs.js（最后一片路由）

parts.json **2→1**（仅剩 _state-part）。currentTab/suppressHashWrite/bondPickWb/
bondPickCc 四个 let 随片归模块所有；文件尾 `window.__kaleidoTabs` 发布原样保留
——tavern/story/agent/authoring 的 `__t6()` 消费方与 boot diag（data-p5diag）零改动。

**闭包残留读取全部走既有门面**：worksOpenPath×9 / worksDirty×3→`__wk()`；
sessionId(1)/messages(1)→`__c7()`；storyMessages×3/storySessionId×2/storyStreaming×2→
`__s8()`（story 门面 streaming getter 即 storyStreaming，无需新增门面成员）。

**裸标识符甄别**：sessionId 5 处中仅 L377 为裸用（其余 `.sessionId` 属性 + 注释行）；
messages 5 处中仅 L384 `(messages.length` 为裸用——用「前一字符是否 `(`」过滤，
避开 'adv-messages' 等字符串。教训 (k4)：**属性名同名的闭包 let 改写要用
负向断言 + 上下文字符双重过滤，期望计数按裸用数单独断言**。

**import 来源勘误**：uid/displayTitle 在 utils.js 不在 state.js（rollup 报
not exported 秒定位）。

main.js 中 tabs.js 紧跟 virtual:app-parts 之后导入，保持原 closure 执行位次。
验证：routes 109=109；wins 增量不变；CJK 9574；len −196B；s4_gate PASS。
commit: 5fa15cd

## S2.19 — _state-part 消解 → state_core.js（Stage 2 完结）

**策略：原样迁移而非拆散归主。** 147L 的最后一片整体变为真 ESM `state_core.js`：
canonical lets + element consts + 全部 7 个 `window.__kaleido*` 门面发布原封不动
——所有消费方 accessor（`__c7/__auth/__s8/__wk/__az/__pf`）零改动。理由：预算内
风险最低；「lets 各归其主模块」会牵动十余个模块的导入面，留作 P1-4+ 的独立重构。

**机制退役**：parts.json 删除；vite virtual 模块改为发射空 IIFE 体（import 前导
保留——api_shell 等的 re-export 兼容层仍在用）；main.js 以 state_core.js 顶替
virtual import（执行位次不变）。20 个死片文件（_search/_regex/_api + 17 个历史
残留）一并清除。

**Stage 2 终态**：src/js 下全部为真 ES 模块，无 IIFE closure、无 converted[]
注入依赖的裸标识符。等价性收官数据：routes 109=109 / wins 58→61（全部计划内
门面）/ CJK 9574 持平 / len 983,683→992,305（门面 accessor 净膨胀 ~8.7KB）/
s4_gate 全程 PASS。
commits: d68f396 (refactor) + docs

---

# P1-4 统一错误码（进行中）

## S1 (fb56f29) — error_codes.rs 信封 helper
契约：非 2xx JSON 必带 `{error, code, ...details}`。details 对象合并顶层、
标量嵌 `details` 键、Null 省略。helpers: err_with_code/bad_request/not_found/
unauthorized/forbidden/conflict/internal。

## S2 (ded258c) — author.rs 全量迁移（29→0 raw）
AUTHOR_* code 注册表（16 个）：BAD_ID / INDEX_CORRUPT / SERIALIZE / NOT_FOUND /
SESSION_REQUIRED / BAD_PLAYABLE / COMPOSE_EMPTY / SOURCE_DOC_READ /
LAUNCH_ARITY / PATH_REQUIRED / PATH_ESCAPES / BAD_KIND / NO_PACK /
PACK_EMPTY / CHAPTER_MISSING / WB_TARGET_MISSING。
实测：404 `{"code":"AUTHOR_NOT_FOUND","error":"project not found: nope-123"}`。

**教训 (m)**：Rust 源码正则改写时，嵌套括号必须用平衡式 `(?:[^()]|\([^()]*\))*`
匹配 format! 参数；且捕获组内含平衡式会导致回溯失败——先在样例上验证完整
pattern 再上全文件（本次 head/mid/tail 分段全过但整 pattern 静默失配，教训：
分段通过 ≠ 整体通过，必须整体验证）。

## 待迁移重灾区（S3+ 顺序）
dual_agent.rs(26) → routes_partner.rs(24) → story_tavern.rs(21) → skills.rs(20)
→ background.rs(17) → novel_api/routes_jobs/book_travel(15×3)。前端 friendlyError
最后加 code 白名单分支。
