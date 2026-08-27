//! Mobile-compat + chat/story start/stop/stream SSE — extracted P0-1 Stage4
use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::{get, post, put},
    Json, Router,
};
use kaleido_core::{AgentSessionRecord, SessionRecord};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::convert::Infallible;
use std::time::Duration as StdDuration;
use tokio::sync::broadcast;
use futures_util::StreamExt;

use crate::auth_mw::{session_from, session_from_any};
use crate::error_codes::*;
use crate::routes_partner::{collect_vector_hits, resolve_wb_ids_for_prompt, vector_query_text, vector_settings_from_value};
use crate::error_map::map_core_err;
use crate::state::{AppState, ChatStreamEvent};
use crate::{ChatStartRequest, TitleUpdate, StopPayload};
use tracing::{info, warn};

pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/chat/start", post(chat_start))
        .route("/api/v1/story/start", post(story_start))
        .route("/api/v1/story/stop", post(story_stop))
        .route("/api/v1/story/stream", get(story_stream))
        .route("/api/mobile/status", get(mobile_status))
        .route("/api/mobile/sessions",
            get(mobile_list_sessions).post(mobile_save_session))
        .route("/api/mobile/sessions/{id}",
            get(mobile_load_session).delete(mobile_delete_session))
        .route("/api/mobile/sessions/{id}/title", put(mobile_update_title))
        .route("/api/mobile/state/{name}",
            get(mobile_get_state).post(mobile_save_state))
        .route("/api/mobile/chat/start", post(mobile_chat_start))
        .route("/api/mobile/story/start", post(mobile_story_start))
        .route("/api/mobile/chat/stop", post(mobile_chat_stop))
        .route("/api/mobile/stream", get(mobile_stream))
}

pub(crate) const DEFAULT_STORY_AGENT_PROMPT: &str = r#"你将在此扮演一个专门的故事主持人（DM/GM）和优秀的故事讲述者，与用户一起进行沉浸式的文字冒险/跑团游戏。你并非普通的写作助手，你也是这个世界的造物主和观察者。

## 核心行为约束
1. **沉浸式叙事**：你的回复必须包含精彩的“旁白描写（环境、氛围、角色的细节神态动作）”以及“角色对话”。你的描写应当充满画面感和人情味。
2. **严守故事设定**：在故事推进中，你的常识、叙述、提到的NPC与发生的事件，必须严格局限在用户选择的“世界书”设定的时代、规则与冲突框架内，不得出现出戏的现代科技或无关常识。
3. **NPC角色契合度**：故事里可能包含多个活跃的NPC（由用户勾选的角色卡定义）。当你代为叙述或扮演这些NPC说话时，必须百分之百遵循他们各自的设定（语气、性格、身份、口头禅等）。
4. **绝不代替用户角色做决定**：你可以扮演世界里的所有NPC并控制客观自然现象，但你绝对不能越俎代庖去代替“我（用户）”的角色做选择、说台词或擅自动手，必须把决定权留给用户。
5. **适应用户输入模式**：用户每次发送的消息有三种不同前缀标记，分别代表不同类型的行动：
   - 【说话】：这是用户角色的直接言语。
   - 【行为】：这是用户角色作出的动作或试探性尝试。
   - 【剧情推进】：这是用户以旁白客观口吻提出的后续剧情发生的方向或世界巧合。
   你必须理解并顺着用户的这些输入类型，合理流畅地展开后续剧情。
6. **绝对禁用词**：严禁在回复中提及任何诸如“我是AI模型”、“让我们继续大纲”、“这是一场游戏”等出戏的系统性词汇。保持沉浸式的冒险体验。
7. **提供候选选项**：在你的回复最后，必须提供 3 个适合当前局势的后续剧情走向供用户选择。选项请严格使用以下 XML 格式包裹：
<choices>
["选项1", "选项2", "选项3"]
</choices>

### 正确返回结果示例
你侧身将行李箱塞进座位上方的行李架，接过罗恩递来的滋滋蜜蜂糖，剥开金色糖纸，把糖果扔进嘴里——蜂蜜的甜香混着一股淡淡的草莓味瞬间在舌尖化开。

“谢了。”你笑着说，在对面的空位坐下，列车恰好发出一声悠长的汽笛，车身微微一震，缓缓驶离了站台。

<choices>
["跟罗恩聊聊哈利", "拿出一本书来看", "望着窗外发呆"]
</choices>"#;

pub(crate) async fn mobile_status(State(state): State<AppState>) -> impl IntoResponse {
    let rt = state.app_state.resolve_llm(
        state.llm_base.as_deref(),
        state.llm_key.as_deref(),
        &state.llm_model,
    );
    let llm_ok = !rt.base_url.trim().is_empty() && !rt.api_key.trim().is_empty();
    Json(json!({
        "isRunning": true,
        "url": null,
        "token": null,
        "error": null,
        "phase": "S8",
        "service": "kaleido-server",
        "llmConfigured": llm_ok,
    }))
}

pub(crate) async fn mobile_list_sessions(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let prefix = match params.get("prefix") {
        Some(p) => p.as_str(),
        None => {
            return bad_request("CHAT_BAD_REQUEST", "缺少会话前缀");
        }
    };
    let kind = params.get("sessionKind").map(|s| s.as_str());
    match state.sessions.list(prefix, kind) {
        Ok(list) => Json(list).into_response(),
        Err(e) => map_core_err(e),
    }
}

pub(crate) async fn mobile_load_session(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match state.sessions.load(&id) {
        Ok(rec) => Json(rec).into_response(),
        Err(e) => map_core_err(e),
    }
}

pub(crate) async fn mobile_save_session(State(state): State<AppState>, body: String) -> Response {
    let rec: AgentSessionRecord = match serde_json::from_str(&body) {
        Ok(r) => r,
        Err(e) => {
            return bad_request("CHAT_INVALID", format!("Invalid JSON: {e}"));
        }
    };
    match state.sessions.save(rec) {
        Ok(sum) => Json(sum).into_response(),
        Err(e) => map_core_err(e),
    }
}

pub(crate) async fn mobile_delete_session(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match state.sessions.delete(&id) {
        Ok(()) => StatusCode::OK.into_response(),
        Err(e) => map_core_err(e),
    }
}

pub(crate) async fn mobile_update_title(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<TitleUpdate>,
) -> Response {
    match state.sessions.update_title(&id, &body.title) {
        Ok(sum) => Json(sum).into_response(),
        Err(e) => map_core_err(e),
    }
}

pub(crate) async fn mobile_get_state(State(state): State<AppState>, Path(name): Path<String>) -> Response {
    match state.app_state.load(&name) {
        Ok(text) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
            .header(header::CACHE_CONTROL, "no-store")
            .body(Body::from(text))
            .unwrap(),
        Err(e) => map_core_err(e),
    }
}

pub(crate) async fn mobile_save_state(
    State(state): State<AppState>,
    Path(name): Path<String>,
    body: String,
) -> Response {
    match state.app_state.save(&name, &body) {
        Ok(()) => StatusCode::OK.into_response(),
        Err(e) => map_core_err(e),
    }
}

/// Upstream mobile: POST body = ChatStreamRequest (camelCase), returns {runId}
pub(crate) async fn mobile_chat_start(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    start_chat_from_body(state, session, body, "chat").await
}

pub(crate) async fn mobile_story_start(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    start_chat_from_body(state, session, body, "story").await
}

/// S5-W2 T1: v1 Story start — same body as mobile story, kind=story.
pub(crate) async fn story_start(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    start_chat_from_body(state, session, body, "story").await
}

/// S5-W2 T1: v1 Story stop — aliases runId / run_id.
pub(crate) async fn story_stop(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: String,
) -> Response {
    mobile_chat_stop(State(state), headers, body).await
}

/// S5-W2 T1: v1 Story SSE — `?runId=` (one-time `?ticket=` for EventSource, M-3).
pub(crate) async fn story_stream(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    mobile_stream(State(state), headers, Query(params)).await
}

pub(crate) async fn start_chat_from_body(
    state: AppState,
    session: SessionRecord,
    body: String,
    kind: &str,
) -> Response {
    // C2: scoped to the authenticated user's own partner store.
    let partner = state.partner.clone().scoped(&session.user_id);
    let llm = state.app_state.resolve_llm(
        state.llm_base.as_deref(),
        state.llm_key.as_deref(),
        &state.llm_model,
    );
    // P5 RPM: provider's per-minute budget exhausted → reject fast with retry hint.
    if llm.rpm_hit {
        return err_with_code(
            StatusCode::TOO_MANY_REQUESTS,
            "RATE_LIMITED",
            "provider rate limit exceeded",
            serde_json::json!({ "retryAfterSecs": llm.rpm_retry_secs }),
        );
    }
    let base = llm.base_url.clone();
    let key = llm.api_key.clone();
    if base.trim().is_empty() || key.trim().is_empty() {
        return err_with_code(
            StatusCode::SERVICE_UNAVAILABLE,
            "CHAT_NOT_CONFIGURED", "LLM not configured",
            serde_json::json!({"hint": "在网页「设置」填写 Base URL 与 API Key，或设置环境变量 LLM_BASE_URL / LLM_API_KEY"}),
        );
    };

    let mut req_val: Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            return bad_request("CHAT_INVALID", format!("Invalid JSON: {e}"));
        }
    };

    // Server injects LLM credentials (strip client secrets)
    let model = req_val
        .get("model")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            let m = llm.model.clone();
            if m.is_empty() {
                state.llm_model.clone()
            } else {
                m
            }
        });

    if let Some(obj) = req_val.as_object_mut() {
        obj.insert("modelInterface".into(), json!("OpenAI"));
        obj.insert("baseUrl".into(), json!(""));
        obj.insert("apiKey".into(), json!(""));
        obj.insert("model".into(), json!(model.clone()));
    }

    // Partner injection (S3): world book + character card into systemPrompt
    // Prefer client-provided systemPrompt only if it already contains partner markers;
    // otherwise assemble from settings + selected partner items.
    let client_system = req_val
        .get("systemPrompt")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let wb_id = req_val
        .get("worldBookId")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let cc_id = req_val
        .get("characterCardId")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let base_prompt = {
        let from_settings = state.app_state.partner_chat_prompt();
        if !client_system.trim().is_empty()
            && (client_system.contains("## 伴侣对话世界设定")
                || client_system.contains("## 你的角色人设设定"))
        {
            client_system.clone()
        } else if kind == "story" {
            // Story DM: client system wins if provided; else upstream defaultStoryAgentPrompt.
            if !client_system.trim().is_empty() {
                client_system.clone()
            } else {
                DEFAULT_STORY_AGENT_PROMPT.to_string()
            }
        } else if !client_system.trim().is_empty() && kind != "chat" {
            // other non-chat kinds: keep client system as base
            client_system.clone()
        } else {
            from_settings
        }
    };
    let messages = req_val
        .get("messages")
        .cloned()
        .unwrap_or_else(|| json!([]));

    // Oldest-first (role, content) for ST World Info scan depth buffer
    let mut chat_pairs: Vec<(String, String)> = Vec::new();
    if let Some(arr) = messages.as_array() {
        for m in arr {
            let role = m.get("role").and_then(|v| v.as_str()).unwrap_or("user");
            let content = m.get("content").and_then(|v| v.as_str()).unwrap_or("");
            if content.is_empty() {
                continue;
            }
            chat_pairs.push((role.to_string(), content.to_string()));
        }
    }

    // Update world state with the last user message (audit P0#3: 按 workspace 分片)
    if let Some((_, ref content)) = chat_pairs.last().filter(|(r, _)| r == "user") {
        use kaleido_core::world_state::WorldEvent;
        let we = WorldEvent::NarrativeEvent {
            summary: content.clone(),
            character_ids: vec![],
            turn: 0,
        };
        if let Ok(mut ws) = state.world_state.lock() {
            ws.entry(session.workspace_id.clone())
                .or_default()
                .apply(we);
        }
    }

    // Optional client WI settings override
    let wi_settings = req_val.get("worldInfoSettings").cloned().and_then(|v| {
        serde_json::from_value::<kaleido_core::WiSettings>(v).ok()
    });
    // Chat/session id for timed WI persistence (sticky/cooldown)
    let chat_id_for_wi = req_val
        .get("sessionId")
        .or_else(|| req_val.get("session_id"))
        .or_else(|| req_val.get("chatId"))
        .or_else(|| req_val.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let timed_store = kaleido_core::TimedWorldInfoStore::new(state.auth.data_root());
    let timed_in = if chat_id_for_wi.is_empty() {
        None
    } else {
        Some(timed_store.load(&chat_id_for_wi))
    };
    let mut wi_scan_ctx = req_val
        .get("worldInfoScanContext")
        .cloned()
        .and_then(|v| serde_json::from_value::<kaleido_core::WiScanContext>(v).ok())
        .unwrap_or_default();
    if wi_scan_ctx.trigger.is_empty() {
        // map job kind → ST-ish trigger
        wi_scan_ctx.trigger = match kind {
            "story" => "normal".into(),
            "chat" => "normal".into(),
            _ => "normal".into(),
        };
        if req_val.get("continue").and_then(|v| v.as_bool()).unwrap_or(false)
            || req_val.get("isContinue").and_then(|v| v.as_bool()).unwrap_or(false)
        {
            wi_scan_ctx.trigger = "continue".into();
        }
    }
    let max_ctx_tokens = req_val
        .get("maxContextTokens")
        .and_then(|v| v.as_i64())
        .unwrap_or(8192) as i32;
    // W5: fill vector hits for vectorized WI entries before scan
    {
        let vset = vector_settings_from_value(
            req_val
                .get("vectorSettings")
                .or_else(|| req_val.get("worldInfoVectorSettings")),
        );
        let depth = req_val
            .get("worldInfoSettings")
            .and_then(|w| w.get("depth"))
            .and_then(|d| d.as_i64())
            .unwrap_or(2) as i32;
        let qtext = vector_query_text(&chat_pairs, depth);
        let wb_ids = resolve_wb_ids_for_prompt(&partner, wb_id, cc_id);
        let (hits, verr) = collect_vector_hits(&state, &wb_ids, &qtext, &vset);
        if let Some(e) = verr {
            warn!(error=%e, "W5 vector path skipped");
        }
        if !hits.is_empty() {
            info!(hits = hits.len(), "W5 vector hits for WI scan");
        }
        wi_scan_ctx.vector_hits = hits;
        wi_scan_ctx.vector_settings = Some(vset);
    }
    let mut wi_message_injections: Vec<serde_json::Value> = Vec::new();

    let (system_prompt, gen_meta) = if kind == "chat"
        || kind == "story"
        || client_system.contains("## 伴侣对话世界设定")
        || client_system.contains("## 世界书")
        || wb_id.is_some()
        || cc_id.is_some()
    {
        match partner.build_generation_prompt_full(
            &base_prompt,
            wb_id,
            cc_id,
            &chat_pairs,
            wi_settings,
            timed_in,
            false,
            Some(wi_scan_ctx.clone()),
            max_ctx_tokens,
        ) {
            Ok(r) => {
                if !r.wi.activated.is_empty() {
                    info!(
                        activated = r.wi.activated.len(),
                        regex = r.regex_script_count,
                        overflowed = r.wi.overflowed,
                        skipped_vec = r.wi.skipped_vectorized,
                        "ST world-info scan"
                    );
                }
                if let Some(ref tw) = r.timed_world_info {
                    if !chat_id_for_wi.is_empty() {
                        if let Err(e) = timed_store.save(&chat_id_for_wi, tw) {
                            warn!(error=%e, chat_id=%chat_id_for_wi, "timed WI save failed");
                        }
                    }
                }
                wi_message_injections = r.message_injections.clone();
                // W7: persist automation trigger log on real generation
                if !r.automation_ids.is_empty() {
                    let detailed: Vec<(String, String, String, String, String)> = r
                        .wi
                        .activated
                        .iter()
                        .filter(|a| !a.automation_id.trim().is_empty())
                        .map(|a| {
                            (
                                a.automation_id.clone(),
                                a.uid.clone(),
                                a.world.clone(),
                                a.comment.clone(),
                                a.reason.clone(),
                            )
                        })
                        .collect();
                    let auto_store =
                        kaleido_core::AutomationTriggerStore::new(state.auth.data_root());
                    if !detailed.is_empty() {
                        let _ = auto_store.record_detailed(
                            &detailed,
                            &chat_id_for_wi,
                            "partner_chat",
                        );
                    } else {
                        let _ = auto_store.record(
                            &r.automation_ids,
                            &[],
                            &chat_id_for_wi,
                            "partner_chat",
                        );
                    }
                }
                let meta = json!({
                    "wiActivated": r.wi.activated.len(),
                    "wiBudgetTokens": r.wi.budget_tokens,
                    "wiOverflowed": r.wi.overflowed,
                    "regexScripts": r.regex_script_count,
                    "anBefore": !r.wi.an_before.is_empty(),
                    "anAfter": !r.wi.an_after.is_empty(),
                    "depthEntries": r.wi.depth_entries.len(),
                    "emBefore": !r.wi.em_before.is_empty(),
                    "emAfter": !r.wi.em_after.is_empty(),
                    "outlets": r.wi.outlet_entries.len(),
                    "chatInjections": r.message_injections.len(),
                    "skippedVectorized": r.wi.skipped_vectorized,
                    "vectorActivated": r.wi.vector_activated,
                    "skippedFilter": r.wi.skipped_filter,
                    "skippedTrigger": r.wi.skipped_trigger,
                    "automationIds": r.automation_ids,
                    "exampleMessages": r.example_messages.len(),
                    "timedSticky": r.timed_world_info.as_ref().map(|t| t.sticky.len()).unwrap_or(0),
                    "timedCooldown": r.timed_world_info.as_ref().map(|t| t.cooldown.len()).unwrap_or(0),
                    "wiEntries": r.wi.activated.iter().map(|a| json!({
                        "uid": a.uid,
                        "world": a.world,
                        "comment": a.comment,
                        "reason": a.reason,
                        "order": a.order,
                        "position": a.position,
                    })).collect::<Vec<_>>(),
                });
                (r.system_prompt, meta)
            }
            Err(e) => {
                warn!(error=%e, "partner generation prompt failed; using base");
                (base_prompt, json!({}))
            }
        }
    } else if client_system.trim().is_empty() {
        ("You are Kaleido partner chat assistant.".into(), json!({}))
    } else {
        (client_system, json!({}))
    };

    let settings = state.app_state.load_settings_public().ok();
    let temperature = req_val
        .get("temperature")
        .and_then(|v| v.as_f64())
        .or_else(|| settings.as_ref().and_then(|s| s.temperature))
        .unwrap_or(0.7);
    let max_tokens = req_val
        .get("maxOutputTokens")
        .and_then(|v| v.as_u64())
        .or_else(|| settings.as_ref().and_then(|s| s.max_output_tokens))
        .unwrap_or(4096);
    let top_p = req_val
        .get("topP")
        .and_then(|v| v.as_f64())
        .or_else(|| settings.as_ref().and_then(|s| s.top_p));
    let frequency_penalty = req_val
        .get("frequencyPenalty")
        .and_then(|v| v.as_f64())
        .or_else(|| settings.as_ref().and_then(|s| s.frequency_penalty));
    let presence_penalty = req_val
        .get("presencePenalty")
        .and_then(|v| v.as_f64())
        .or_else(|| settings.as_ref().and_then(|s| s.presence_penalty));

    // Global library ∪ character-scoped regex (W6) for prompt-path rewrites
    let regex_scripts = {
        let card_fields = cc_id.and_then(|id| {
            partner.load().ok().and_then(|pst| {
                pst.character_cards
                    .into_iter()
                    .find(|c| c.id == id)
                    .and_then(|c| c.fields)
            })
        });
        kaleido_core::resolve_runtime_scripts(&state.regex_library, card_fields.as_ref())
    };

    // Inject world state narrative summary into system prompt (audit P0#3: 仅本 workspace)
    let system_prompt = if let Ok(ws) = state.world_state.lock() {
        let summary = ws.get(&session.workspace_id).map(|s| s.narrative_summary());
        if let Some(summary) = summary {
            if !summary.is_empty() {
                format!("{}\n\n## Current World State\n{}", system_prompt, summary)
            } else {
                system_prompt
            }
        } else {
            system_prompt
        }
    } else {
        system_prompt
    };

    // Build OpenAI messages
    let mut oai_messages = vec![json!({"role":"system","content": system_prompt})];
    let mut transcript: Vec<serde_json::Value> = Vec::new();
    if let Some(arr) = messages.as_array() {
        let n = arr.len();
        for (i, m) in arr.iter().enumerate() {
            let role = m.get("role").and_then(|v| v.as_str()).unwrap_or("user");
            let content = m.get("content").and_then(|v| v.as_str()).unwrap_or("");
            if content.is_empty() && role != "assistant" {
                continue;
            }
            let depth = (n - 1 - i) as i32;
            let placement = if role == "user" {
                kaleido_core::RegexPlacement::UserInput
            } else {
                kaleido_core::RegexPlacement::AiOutput
            };
            let content = kaleido_core::get_regexed_string(
                content,
                placement,
                &regex_scripts,
                false,
                true,
                Some(depth),
            );
            transcript.push(json!({"role": role, "content": content}));
        }
    }
    // ST-style: insert WI depth/outlet/EM injections into transcript by depth
    // depth 0 → just before the last message (or append if empty)
    if !wi_message_injections.is_empty() {
        // sort by depth descending so earlier inserts don't shift later indices wrongly
        let mut inj = wi_message_injections.clone();
        inj.sort_by(|a, b| {
            let da = a.get("depth").and_then(|v| v.as_i64()).unwrap_or(0);
            let db = b.get("depth").and_then(|v| v.as_i64()).unwrap_or(0);
            db.cmp(&da)
        });
        for item in inj {
            let depth = item.get("depth").and_then(|v| v.as_i64()).unwrap_or(0).max(0) as usize;
            let role = item.get("role").and_then(|v| v.as_str()).unwrap_or("system");
            let content = item.get("content").and_then(|v| v.as_str()).unwrap_or("");
            if content.is_empty() { continue; }
            let msg = json!({"role": role, "content": content, "wiSlot": true});
            // insert so that `depth` messages remain after this injection from the end
            let idx = if transcript.len() > depth {
                transcript.len() - depth
            } else {
                0
            };
            transcript.insert(idx, msg);
        }
    }
    oai_messages.extend(transcript);

    // Weaver context compaction: if total tokens exceed threshold, compact older messages
    {
        use kaleido_core::memory_weaver::{self, MessageStats};
        let total_est: usize = oai_messages[1..] // skip system message
            .iter()
            .filter_map(|m| m.get("content").and_then(|c| c.as_str()))
            .map(memory_weaver::estimate_tokens)
            .sum();
        if memory_weaver::should_weave(total_est, &state.weaver_config) {
            let msg_stats: Vec<MessageStats> = oai_messages[1..]
                .iter()
                .enumerate()
                .map(|(i, m)| {
                    let role = m.get("role").and_then(|v| v.as_str()).unwrap_or("user").to_string();
                    let content = m.get("content").and_then(|v| v.as_str()).unwrap_or("");
                    MessageStats {
                        index: i,
                        role,
                        char_count: content.len(),
                        estimated_tokens: memory_weaver::estimate_tokens(content),
                        engine_tag: None,
                    }
                })
                .collect();
            if let Some(cut_idx) = memory_weaver::find_cut_point(&msg_stats, state.weaver_config.keep_recent_tokens) {
                let oai_cut = cut_idx + 2; // +1 for system at idx 0, +1 because find_cut_point returns last compacted index
                if oai_cut > 1 && oai_cut < oai_messages.len() {
                    let compacted_count = cut_idx + 1;
                    let retained_tokens: usize = msg_stats[cut_idx..]
                        .iter()
                        .map(|s| s.estimated_tokens)
                        .sum();
                    info!(
                        compacted = compacted_count,
                        retained = oai_messages.len() - oai_cut,
                        total_before = total_est,
                        retained_tokens = retained_tokens,
                        "weaver context compaction"
                    );
                    // S7: archive compacted messages into session-level vector index BEFORE dropping them
                    // (history vector recall — P1-1). Best-effort; failure only logs.
                    if !chat_id_for_wi.is_empty() {
                        let sess_key = format!("sess-{chat_id_for_wi}");
                        let archived: Vec<(String, String)> = oai_messages[1..oai_cut]
                            .iter()
                            .filter_map(|m| {
                                let content = m.get("content").and_then(|c| c.as_str()).unwrap_or("").trim();
                                if content.is_empty() || content.starts_with('[') {
                                    return None;
                                }
                                let role = m.get("role").and_then(|v| v.as_str()).unwrap_or("assistant");
                                let text = format!("{role}: {content}");
                                let uid = format!(
                                    "m-{}-{}",
                                    role,
                                    kaleido_core::text_hash(&text).chars().take(10).collect::<String>()
                                );
                                Some((uid, text))
                            })
                            .collect();
                        if !archived.is_empty() {
                            let texts: Vec<String> = archived.iter().map(|(_, t)| t.clone()).collect();
                            match tokio::task::spawn_blocking(move || crate::embed_local::embed_many(&texts))
                                .await
                            {
                                Ok(Ok(embeds)) => {
                                    let mut idx = state.vector_index.load(&sess_key);
                                    let known: std::collections::HashSet<String> =
                                        idx.entries.iter().map(|e| e.uid.clone()).collect();
                                    let mut added = 0usize;
                                    for ((uid, text), v) in archived.iter().zip(embeds.iter()) {
                                        if !known.contains(uid) && !v.is_empty() {
                                            idx.entries.push(kaleido_core::VectorIndexEntry {
                                                uid: uid.clone(),
                                                world: "history".into(),
                                                text: text.clone(),
                                                text_hash: kaleido_core::text_hash(text),
                                                vector: v.clone(),
                                            });
                                            added += 1;
                                        }
                                    }
                                    if added > 0 {
                                        match state.vector_index.save(idx) {
                                            Ok(f) => info!(sess = %chat_id_for_wi, entries = f.entries.len(), "S7 history archived to vector index"),
                                            Err(e) => warn!(error = %e, "S7 vector archive save failed"),
                                        }
                                    }
                                }
                                Ok(Err(e)) => warn!(error = %e, "S7 embed_many failed"),
                                Err(e) => warn!(error = %e, "S7 embed join failed"),
                            }
                        }
                    }
                    oai_messages.drain(1..oai_cut);
                    oai_messages.insert(1, json!({
                        "role": "system",
                        "content": format!(
                            "[{} earlier messages compacted. Retaining recent ~{} tokens.]",
                            compacted_count, retained_tokens
                        )
                    }));
                }
            }
        }
    }

    // S7 (P1-1): history vector recall — query session vector index with recent context,
    // inject top hits back into the prompt so compacted details are recoverable.
    if !chat_id_for_wi.is_empty() {
        let sess_key = format!("sess-{chat_id_for_wi}");
        let idx = state.vector_index.load(&sess_key);
        if !idx.entries.is_empty() {
            let qtext: Vec<String> = oai_messages
                .iter()
                .rev()
                .take(4)
                .filter_map(|m| m.get("content").and_then(|c| c.as_str()).map(|s| s.to_string()))
                .filter(|c| !c.trim().is_empty() && !c.starts_with('['))
                .collect();
            let qtext = qtext.join("\n");
            if !qtext.trim().is_empty() {
                let qtext2 = qtext.clone();
                match tokio::task::spawn_blocking(move || crate::embed_local::embed_one(&qtext2)).await {
                    Ok(Ok(qv)) => {
                        let vset = kaleido_core::VectorActivationSettings {
                            enabled: true,
                            score_threshold: 0.42,
                            top_k: 4,
                        };
                        let hits = kaleido_core::rank_hits(&idx, &qv, &vset);
                        if !hits.is_empty() {
                            let lines: Vec<String> = hits
                                .iter()
                                .filter_map(|h| {
                                    idx.entries
                                        .iter()
                                        .find(|e| e.uid == h.uid)
                                        .map(|e| e.text.clone())
                                })
                                .collect();
                            if !lines.is_empty() {
                                let recall = format!(
                                    "【历史回忆·向量检索命中】\n{}",
                                    lines.join("\n\n")
                                );
                                oai_messages.insert(1, json!({
                                    "role": "system",
                                    "content": recall,
                                    "s7Recall": true,
                                }));
                                info!(sess = %chat_id_for_wi, hits = hits.len(), "S7 history recall injected");
                            }
                        }
                    }
                    Ok(Err(e)) => warn!(error = %e, "S7 recall embed failed"),
                    Err(e) => warn!(error = %e, "S7 recall join failed"),
                }
            }
        }
    }

    let _gen_meta = gen_meta;

    // P5 usage recording: capture managed-provider identity + estimated input
    // tokens (only recorded when the runtime resolved through ai_admin).
    let rec_provider = llm.provider_id.clone();
    let rec_workspace = session.workspace_id.clone();
    let rec_kind = kind.to_string();
    let rec_data_root = state.auth.data_root().root().to_path_buf();
    let input_tokens = oai_messages
        .iter()
        .filter_map(|m| m.get("content").and_then(|c| c.as_str()))
        .map(kaleido_core::memory_weaver::estimate_tokens)
        .sum::<usize>() as i64;

    let job = match state.jobs.try_start(
        kind,
        &session.user_id,
        &session.workspace_id,
        Some(model.clone()),
        json!({"agentId": req_val.get("agentId")}),
    ) {
        Ok(j) => j,
        Err(e) => return map_core_err(e),
    };
    let run_id = job.run_id.clone();
    let tx = state.hub.register(&run_id);
    info!(%run_id, %kind, %model, "mobile chat started");

    // spawn upstream stream → hub events
    let hub = state.hub.clone();
    let jobs = state.jobs.clone();
    let run_id_bg = run_id.clone();
    let base_bg = base;
    let key_bg = key;
    tokio::spawn(async move {
        let mut out_text = String::new();
        let rec_model = model.clone();
        // P5: record provider usage on every terminal path (managed providers only).
        let rec = |status: &str, out: i64, err: Option<&str>| {
            let Some(pid) = rec_provider.clone() else {
                return;
            };
            if let Ok(store) =
                kaleido_core::ai_admin_store::AiAdminStore::open(&rec_data_root.join("plot.sqlite"))
            {
                let _ = store.record_call(
                    &pid,
                    &rec_model,
                    &rec_workspace,
                    &rec_kind,
                    status,
                    input_tokens,
                    out,
                    0,
                    err,
                );
            }
        };
        let client = match reqwest::Client::builder()
            .timeout(StdDuration::from_secs(300))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(ChatStreamEvent {
                    run_id: run_id_bg.clone(),
                    event_type: "error".into(),
                    delta: None,
                    message: Some(e.to_string()),
                    code: Some("LLM_CLIENT_INIT".into()),
                    context_compaction: None,
                    input_tokens: None,
                    output_tokens: None,
                });
                rec("failed", 0, Some(&e.to_string()));
                jobs.finish(&run_id_bg, "error");
                hub.cleanup(&run_id_bg);
                return;
            }
        };

        let mut body = json!({
            "model": model,
            "stream": true,
            "messages": oai_messages,
            "temperature": temperature,
            "max_tokens": max_tokens,
        });
        if let Some(obj) = body.as_object_mut() {
            if let Some(v) = top_p {
                obj.insert("top_p".into(), json!(v));
            }
            if let Some(v) = frequency_penalty {
                obj.insert("frequency_penalty".into(), json!(v));
            }
            if let Some(v) = presence_penalty {
                obj.insert("presence_penalty".into(), json!(v));
            }
        }

        let url = format!("{}/chat/completions", base_bg.trim_end_matches('/'));
        let resp = match client
            .post(&url)
            .bearer_auth(key_bg)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                let _ = tx.send(ChatStreamEvent {
                    run_id: run_id_bg.clone(),
                    event_type: "error".into(),
                    delta: None,
                    message: Some(format!("upstream connect: {e}")),
                    code: Some("UPSTREAM_CONNECT".into()),
                    context_compaction: None,
                    input_tokens: None,
                    output_tokens: None,
                });
                rec("failed", 0, Some(&e.to_string()));
                jobs.finish(&run_id_bg, "error");
                hub.cleanup(&run_id_bg);
                return;
            }
        };

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            let _ = tx.send(ChatStreamEvent {
                run_id: run_id_bg.clone(),
                event_type: "error".into(),
                delta: None,
                message: Some(format!(
                    "upstream {status}: {}",
                    text.chars().take(300).collect::<String>()
                )),
                code: Some("UPSTREAM_STATUS".into()),
                context_compaction: None,
                    input_tokens: None,
                    output_tokens: None,
            });
            rec("failed", 0, Some(&text.chars().take(200).collect::<String>()));
            jobs.finish(&run_id_bg, "error");
            hub.cleanup(&run_id_bg);
            return;
        }

        let mut buf = String::new();
        let mut byte_carry: Vec<u8> = Vec::new();
        let mut byte_stream = resp.bytes_stream();
        while let Some(item) = byte_stream.next().await {
            if hub.is_cancelled(&run_id_bg) {
                break;
            }
            match item {
                Ok(bytes) => {
                    buf.push_str(&crate::utf8_stream::push_utf8_chunk(&mut byte_carry, &bytes));
                    while let Some(pos) = buf.find('\n') {
                        let mut line = buf[..pos].to_string();
                        buf = buf[pos + 1..].to_string();
                        if line.ends_with('\r') {
                            line.pop();
                        }
                        if line.is_empty() {
                            continue;
                        }
                        if let Some(data) = line.strip_prefix("data: ") {
                            if data.trim() == "[DONE]" {
                                let _ = tx.send(ChatStreamEvent {
                                    run_id: run_id_bg.clone(),
                                    event_type: "done".into(),
                                    delta: None,
                                    message: None,
                                    context_compaction: None,
                    input_tokens: Some(input_tokens),
                    output_tokens: Some(kaleido_core::memory_weaver::estimate_tokens(&out_text) as i64),
                    code: None,
                                });
                                jobs.finish(&run_id_bg, "done");
                                rec("ok", kaleido_core::memory_weaver::estimate_tokens(&out_text) as i64, None);
                                hub.cleanup(&run_id_bg);
                                return;
                            }
                            if let Ok(v) = serde_json::from_str::<Value>(data) {
                                let delta = v["choices"][0]["delta"]["content"]
                                    .as_str()
                                    .unwrap_or("");
                                let reasoning = v["choices"][0]["delta"]["reasoning_content"]
                                    .as_str()
                                    .or_else(|| {
                                        v["choices"][0]["delta"]["reasoning"].as_str()
                                    })
                                    .unwrap_or("");
                                if !reasoning.is_empty() {
                                    let _ = tx.send(ChatStreamEvent {
                                        run_id: run_id_bg.clone(),
                                        event_type: "thinking_delta".into(),
                                        delta: Some(reasoning.to_string()),
                                        message: None,
                                        context_compaction: None,
                    input_tokens: None,
                    output_tokens: None,
                    code: None,
                                    });
                                }
                                if !delta.is_empty() {
                                    let _ = tx.send(ChatStreamEvent {
                                        run_id: run_id_bg.clone(),
                                        event_type: "delta".into(),
                                        delta: Some(delta.to_string()),
                                        message: None,
                                        context_compaction: None,
                    input_tokens: None,
                    output_tokens: None,
                    code: None,
                                    });
                                    out_text.push_str(&delta);
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!(error=%e, "upstream stream error");
                    let _ = tx.send(ChatStreamEvent {
                        run_id: run_id_bg.clone(),
                        event_type: "error".into(),
                        delta: None,
                        message: Some(e.to_string()),
                        code: Some("UPSTREAM_STREAM".into()),
                        context_compaction: None,
                        input_tokens: None,
                        output_tokens: None,
                    });
                    rec("failed", 0, Some(&e.to_string()));
                    jobs.finish(&run_id_bg, "error");
                    hub.cleanup(&run_id_bg);
                    return;
                }
            }
        }
        let _ = tx.send(ChatStreamEvent {
            run_id: run_id_bg.clone(),
            event_type: "done".into(),
            delta: None,
            message: None,
            context_compaction: None,
                    input_tokens: Some(input_tokens),
                    output_tokens: Some(kaleido_core::memory_weaver::estimate_tokens(&out_text) as i64),
                    code: None,
        });
        jobs.finish(&run_id_bg, "done");
        rec("ok", kaleido_core::memory_weaver::estimate_tokens(&out_text) as i64, None);
        hub.cleanup(&run_id_bg);
    });

    Json(json!({"runId": run_id})).into_response()
}

pub(crate) async fn mobile_chat_stop(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let payload: StopPayload = match serde_json::from_str(&body) {
        Ok(p) => p,
        Err(e) => {
            return bad_request("CHAT_INVALID", format!("Invalid stop payload: {e}"));
        }
    };
    // audit P1 IDOR: 只允许停止自己 workspace 的 run（查不到 job 时放行——start 竞态窗口）
    if let Some(j) = state.jobs.get(&payload.run_id) {
        if j.workspace_id != session.workspace_id && j.user_id != session.user_id {
            return forbidden("CHAT_FORBIDDEN_SCOPE", "run not in your workspace");
        }
    }
    state.hub.cancel(&payload.run_id);
    state.jobs.finish(&payload.run_id, "stopped");
    StatusCode::OK.into_response()
}

// L-2: runs hub cleanup when an SSE stream ends / client disconnects.
pub(crate) struct HubCleanupGuard {
    state: AppState,
    run_id: String,
    fired: bool,
}
impl HubCleanupGuard {
    pub(crate) fn new(state: AppState, run_id: String) -> Self {
        HubCleanupGuard { state, run_id, fired: false }
    }
}
impl Drop for HubCleanupGuard {
    fn drop(&mut self) {
        if !self.fired {
            self.state.hub.cleanup(&self.run_id);
        }
    }
}

pub(crate) async fn mobile_stream(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let session = match session_from_any(&state, &headers, Some(&params)) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let run_id = match params.get("runId").cloned() {
        Some(id) => id,
        None => {
            return bad_request("CHAT_BAD_REQUEST", "runId missing");
        }
    };

    // audit P1 IDOR: 订阅前校验 run 归属（同 mobile_chat_stop 策略）
    if let Some(j) = state.jobs.get(&run_id) {
        if j.workspace_id != session.workspace_id && j.user_id != session.user_id {
            return forbidden("CHAT_FORBIDDEN_SCOPE", "run not in your workspace");
        }
    }

    // F4: use subscribe (supports reconnect + replay) instead of take_receiver
    let (mut rx, replay) = match state.hub.subscribe(&run_id) {
        Some(pair) => pair,
        None => {
            // Check if job already finished
            if let Some(job) = state.jobs.get(&run_id) {
                let status = &job.status;
                let done = status == "done" || status == "succeeded" || status == "failed"
                    || status == "cancelled" || status == "error";
                if done && status != "failed" && status != "error" && status != "cancelled" {
                    return Json(json!({
                        "type": "result", "subtype": "success", "result": status, "runId": run_id
                    })).into_response();
                } else {
                    return Json(json!({
                        "type": "result", "subtype": "error", "result": status, "runId": run_id
                    })).into_response();
                }
            }
            return not_found("CHAT_NOT_FOUND", "No receiver registered for runId");
        }
    };

    let stream = async_stream::stream! {
        // L-2: cleanup hub state when the client stream ends / disconnects.
        let _end = HubCleanupGuard::new(state.clone(), run_id.clone());
        for evt in &replay {
            let json_str = serde_json::to_string(evt).unwrap_or_default();
            yield Ok::<Event, Infallible>(Event::default().data(json_str));
            if evt.event_type == "done" || evt.event_type == "error" {
                return;
            }
        }
        loop {
            match rx.recv().await {
                Ok(event) => {
                    let json_str = serde_json::to_string(&event).unwrap_or_default();
                    yield Ok::<Event, Infallible>(Event::default().data(json_str));
                    if event.event_type == "done" || event.event_type == "error" {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Closed) => { break; }
                Err(broadcast::error::RecvError::Lagged(_)) => { continue; }
            }
        }
    };

    Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(StdDuration::from_secs(15)))
        .into_response()
}

// Simple S1-style chat (kept for smoke)
pub(crate) async fn chat_start(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ChatStartRequest>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let req = json!({
        "agentId": "partnerChat",
        "systemPrompt": body.system.unwrap_or_else(|| "You are Kaleido assistant.".into()),
        "messages": [{"id":"u1","role":"user","content": body.message}],
        "model": body.model.unwrap_or_default(),
        "temperature": 0.7,
        "maxOutputTokens": 2048
    });
    start_chat_from_body(state, session, req.to_string(), "chat").await
}
