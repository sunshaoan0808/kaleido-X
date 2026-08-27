//! Agent sessions API (S5-W2 T4).
//! CRUD + dry-run / tool loop on /api/v1/agent/sessions

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use kaleido_core::{AgentSessionMessage, AgentSessionRecord, plugin::{NarrativeEvent, PluginContext}};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::Duration as StdDuration;
use uuid::Uuid;

use crate::{agent_tools, map_core_err, session_from, AppState};
use crate::error_codes::*;

const MAX_TOOL_ROUNDS_HARD_CAP: usize = 8;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TitleBody {
    pub title: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateBody {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub messages: Option<Vec<AgentSessionMessage>>,
    #[serde(default)]
    pub is_archived: Option<bool>,
    #[serde(default)]
    pub selected_reference_files: Option<Vec<String>>,
    #[serde(default)]
    pub selected_outline_file: Option<String>,
    #[serde(default)]
    pub todos: Option<Vec<Value>>,
    #[serde(default)]
    pub character_card_id: Option<String>,
    #[serde(default)]
    pub selected_world_book_id: Option<String>,
    #[serde(default)]
    pub session_kind: Option<String>,
    #[serde(default)]
    pub book_travel_state: Option<Value>,
    #[serde(default)]
    pub character_card_ids: Option<Vec<String>>,
    #[serde(default)]
    pub selected_style_preset_ids: Option<Vec<String>>,
    #[serde(default)]
    pub dynamic_role_loading_enabled: Option<bool>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallSpec {
    pub name: String,
    #[serde(default)]
    pub arguments: Option<Value>,
    #[serde(default)]
    pub args: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunBody {
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub dry_run: Option<bool>,
    #[serde(default)]
    pub max_tool_rounds: Option<usize>,
    #[serde(default)]
    pub tools: Option<Vec<ToolCallSpec>>,
    #[serde(default)]
    pub model: Option<String>,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/agent/sessions",
            get(list_sessions).post(create_or_save),
        )
        .route(
            "/api/v1/agent/sessions/{id}",
            get(get_session)
                .patch(update_session)
                .delete(delete_session)
                .put(update_session),
        )
        .route(
            "/api/v1/agent/sessions/{id}/title",
            post(update_title).patch(update_title),
        )
        .route("/api/v1/agent/sessions/{id}/run", post(run_session))
}

fn allowed_tools_list() -> Vec<&'static str> {
    vec!["read", "list", "write", "bash", "edit", "grep", "glob", "todo"]
}

async fn list_sessions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let prefix = params
        .get("prefix")
        .map(|s| s.as_str())
        .unwrap_or("partner-session-");
    let kind = params.get("sessionKind").map(|s| s.as_str());
    match state.sessions.list(prefix, kind) {
        Ok(list) => Json(json!({
            "ok": true,
            "sessions": list,
            "count": list.len(),
            "prefix": prefix,
        }))
        .into_response(),
        Err(e) => map_core_err(e),
    }
}

async fn get_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    match state.sessions.load(&id) {
        Ok(rec) => Json(rec).into_response(),
        Err(e) => map_core_err(e),
    }
}

async fn create_or_save(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: String,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let mut rec: AgentSessionRecord = match serde_json::from_str(&body) {
        Ok(r) => r,
        Err(e) => {
            // Allow minimal create: { title, sessionKind?, prefix? }
            let v: Value = match serde_json::from_str(&body) {
                Ok(v) => v,
                Err(e2) => {
                    return bad_request("ASESS_INVALID", format!("Invalid JSON: {e}; {e2}"));
                }
            };
            let title = v
                .get("title")
                .and_then(|x| x.as_str())
                .unwrap_or("Untitled Agent Session")
                .to_string();
            let kind = v
                .get("sessionKind")
                .or_else(|| v.get("session_kind"))
                .and_then(|x| x.as_str())
                .map(|s| s.to_string());
            let prefix = v
                .get("prefix")
                .and_then(|x| x.as_str())
                .unwrap_or(if kind.as_deref() == Some("story") {
                    "story-session-"
                } else {
                    "partner-session-"
                });
            let id = v
                .get("id")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("{prefix}{}", Uuid::new_v4()));
            AgentSessionRecord {
                id,
                title,
                saved_at: 0,
                session_kind: kind,
                messages: vec![],
                selected_reference_files: vec![],
                selected_outline_file: None,
                todos: vec![],
                context_compaction: None,
                is_archived: Some(false),
                character_card_id: None,
                character_card_ids: None,
                selected_world_book_id: None,
                dynamic_role_loading_enabled: None,
                selected_style_preset_ids: None,
                initial_style_preset_ids: None,
                initial_system_prompt_snapshot: None,
                book_travel_state: None,
                decisions: vec![],
                panels: vec![],
                world_lines: vec![],
            }
        }
    };

    if rec.id.trim().is_empty() {
        let prefix = if rec.session_kind.as_deref() == Some("story") {
            "story-session-"
        } else {
            "partner-session-"
        };
        rec.id = format!("{prefix}{}", Uuid::new_v4());
    }

    let session_id = rec.id.clone();
    match state.sessions.save(rec) {
        Ok(sum) => {
            // Fire on_session_created plugin hook
            let ctx = PluginContext {
                session_id: Some(session_id.clone()),
                ..Default::default()
            };
            let event = NarrativeEvent::SessionCreated {
                session_id: session_id.clone(),
                character_id: String::new(),
                initial_message: None,
            };
            state.plugin_registry.on_session_created(&ctx, &event);
            (StatusCode::CREATED, Json(sum)).into_response()
        }
        Err(e) => map_core_err(e),
    }
}

async fn update_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<UpdateBody>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let mut rec = match state.sessions.load(&id) {
        Ok(r) => r,
        Err(e) => return map_core_err(e),
    };
    if let Some(t) = body.title {
        rec.title = t;
    }
    if let Some(m) = body.messages {
        rec.messages = m;
    }
    if let Some(a) = body.is_archived {
        rec.is_archived = Some(a);
    }
    if let Some(f) = body.selected_reference_files {
        rec.selected_reference_files = f;
    }
    if body.selected_outline_file.is_some() {
        rec.selected_outline_file = body.selected_outline_file;
    }
    if let Some(t) = body.todos {
        rec.todos = t;
    }
    if body.character_card_id.is_some() {
        rec.character_card_id = body.character_card_id;
    }
    if body.character_card_ids.is_some() {
        rec.character_card_ids = body.character_card_ids;
    }
    if body.selected_world_book_id.is_some() {
        rec.selected_world_book_id = body.selected_world_book_id;
    }
    if body.selected_style_preset_ids.is_some() {
        rec.selected_style_preset_ids = body.selected_style_preset_ids;
    }
    if body.dynamic_role_loading_enabled.is_some() {
        rec.dynamic_role_loading_enabled = body.dynamic_role_loading_enabled;
    }
    if body.session_kind.is_some() {
        rec.session_kind = body.session_kind;
    }
    if body.book_travel_state.is_some() {
        rec.book_travel_state = body.book_travel_state;
    }
    rec.saved_at = 0; // save() will refresh
    match state.sessions.save(rec) {
        Ok(sum) => Json(sum).into_response(),
        Err(e) => map_core_err(e),
    }
}

async fn delete_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    // Fire on_session_ended plugin hook
    let ctx = PluginContext {
        session_id: Some(id.clone()),
        ..Default::default()
    };
    let event = NarrativeEvent::SessionEnded {
        session_id: id.clone(),
        reason: "user deleted".into(),
    };
    state.plugin_registry.on_session_ended(&ctx, &event);

    match state.sessions.delete(&id) {
        Ok(()) => Json(json!({"ok": true, "id": id})).into_response(),
        Err(e) => map_core_err(e),
    }
}

async fn update_title(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<TitleBody>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    match state.sessions.update_title(&id, &body.title) {
        Ok(sum) => Json(sum).into_response(),
        Err(e) => map_core_err(e),
    }
}

// ---------------------------------------------------------------------------
// Run + tool loop
// ---------------------------------------------------------------------------

async fn run_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<RunBody>,
) -> Response {
    let sess = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let mut rec = match state.sessions.load(&id) {
        Ok(r) => r,
        Err(e) => return map_core_err(e),
    };

    let dry_run = body.dry_run.unwrap_or(false);
    let mut max_rounds = body.max_tool_rounds.unwrap_or(4);
    if max_rounds > MAX_TOOL_ROUNDS_HARD_CAP {
        max_rounds = MAX_TOOL_ROUNDS_HARD_CAP;
    }
    if max_rounds == 0 {
        max_rounds = 1;
    }

    if let Some(msg) = body
        .message
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        rec.messages.push(AgentSessionMessage {
            id: Uuid::new_v4().to_string(),
            role: "user".into(),
            content: msg.to_string(),
            thinking: None,
            tools: None,
            thinking_blocks: None,
        });
    }

    let mut tool_results: Vec<Value> = Vec::new();
    let mut tool_rounds_used = 0usize;

    // Explicit tool list from client (gate dry-run path)
    if let Some(tools) = body.tools.clone() {
        for spec in tools.into_iter().take(max_rounds) {
            tool_rounds_used += 1;
            let args = spec
                .arguments
                .clone()
                .or(spec.args.clone())
                .unwrap_or(json!({}));
            let result = execute_tool(&state, &sess.workspace_id, &spec.name, &args, dry_run).await;
            tool_results.push(json!({
                "name": spec.name,
                "arguments": args,
                "result": result,
                "dryRun": dry_run,
            }));
        }
    } else if !dry_run {
        // Optional single LLM turn (best-effort); never fail the whole run hard
        if let Some(last_user) = rec
            .messages
            .iter()
            .rev()
            .find(|m| m.role == "user")
            .map(|m| m.content.clone())
        {
            match call_llm_once(&state, body.model.as_deref(), &rec, &last_user).await {
                Ok(content) => {
                    rec.messages.push(AgentSessionMessage {
                        id: Uuid::new_v4().to_string(),
                        role: "assistant".into(),
                        content,
                        thinking: None,
                        tools: None,
                        thinking_blocks: None,
                    });
                }
                Err(e) => {
                    tool_results.push(json!({"warning": format!("llm: {e}")}));
                }
            }
        }
    } else {
        // dry-run with no tools: report allowed tools
        tool_results.push(json!({
            "dryRun": true,
            "allowedTools": allowed_tools_list(),
            "note": "no tools requested",
        }));
    }

    rec.saved_at = 0;
    let summary = match state.sessions.save(rec.clone()) {
        Ok(s) => s,
        Err(e) => return map_core_err(e),
    };

    Json(json!({
        "ok": true,
        "id": id,
        "session": summary,
        "messages": rec.messages,
        "toolResults": tool_results,
        "toolRoundsUsed": tool_rounds_used,
        "maxToolRounds": max_rounds,
        "dryRun": dry_run,
        "allowedTools": allowed_tools_list(),
    }))
    .into_response()
}

async fn execute_tool(
    state: &AppState,
    workspace_id: &str,
    name: &str,
    args: &Value,
    dry_run: bool,
) -> Value {
    let name = name.trim().to_ascii_lowercase();
    if !allowed_tools_list().contains(&name.as_str()) {
        return json!({"ok": false, "error": format!("tool not allowed: {name}")});
    }
    // Honor same kill-switches as /api/v1/agent/tools/* (cross-audit C1 + W10).
    if name != "bash" && !agent_tools::agent_tools_enabled(state) {
        return json!({"ok": false, "error": "agent_tools_disabled", "code": "AGENT_TOOLS_DISABLED"});
    }
    if matches!(name.as_str(), "write" | "edit") && !agent_tools::agent_write_enabled(state) {
        return json!({"ok": false, "error": "agent_write_disabled", "code": "AGENT_WRITE_DISABLED"});
    }
    if name == "bash" && !agent_tools::bash_sandbox_enabled(state) {
        return json!({"ok": false, "error": "bash_disabled", "code": "BASH_DISABLED"});
    }
    let path = args
        .get("path")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    if dry_run {
        return json!({
            "ok": true,
            "dryRun": true,
            "tool": name,
            "path": path,
            "wouldExecute": true,
            "jail": "works/{workspace}",
        });
    }
    // Jail to works/{workspace_id} only — never whole $KALEIDO_DATA (no secrets/).
    let root = match state.works.workspace_root(workspace_id) {
        Ok(p) => p,
        Err(e) => return json!({"ok": false, "error": e.to_string()}),
    };
    match name.as_str() {
        "read" => match agent_tools::jail_read_public(&root, &path) {
            Ok((abs, content, size)) => json!({
                "ok": true,
                "path": path,
                "abs": abs,
                "content": content,
                "size": size,
            }),
            Err(e) => json!({"ok": false, "error": e.to_string()}),
        },
        "list" => match agent_tools::jail_list_public(&root, &path) {
            Ok(v) => v,
            Err(e) => json!({"ok": false, "error": e.to_string()}),
        },
        "write" => {
            let content = args
                .get("content")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            match agent_tools::jail_write_public(&root, &path, &content) {
                Ok((abs, size)) => json!({
                    "ok": true,
                    "path": path,
                    "abs": abs,
                    "size": size,
                }),
                Err(e) => json!({"ok": false, "error": e.to_string()}),
            }
        }
        "edit" => {
            let old_s = args
                .get("oldString")
                .or_else(|| args.get("old_string"))
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            let new_s = args
                .get("newString")
                .or_else(|| args.get("new_string"))
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            let replace_all = args
                .get("replaceAll")
                .or_else(|| args.get("replace_all"))
                .and_then(|x| x.as_bool())
                .unwrap_or(false);
            match agent_tools::jail_edit_public(&root, &path, &old_s, &new_s, replace_all) {
                Ok(v) => v,
                Err(e) => json!({"ok": false, "error": e.to_string()}),
            }
        }
        "grep" => {
            let pattern = args
                .get("pattern")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            match agent_tools::jail_grep_public(&root, &path, &pattern) {
                Ok(v) => v,
                Err(e) => json!({"ok": false, "error": e.to_string()}),
            }
        }
        "glob" => {
            let pattern = args
                .get("pattern")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            match agent_tools::jail_glob_public(&root, &path, &pattern) {
                Ok(v) => v,
                Err(e) => json!({"ok": false, "error": e.to_string()}),
            }
        }
        "todo" => json!({
            "ok": true,
            "note": "use GET/PUT /api/v1/agent/sessions/{id}/todos",
            "hint": args,
        }),
        "bash" => {
            let command = args
                .get("command")
                .or_else(|| args.get("cmd"))
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            match agent_tools::run_sandboxed_bash_public(&root, &command).await {
                Ok(v) => v,
                Err(e) => json!({"ok": false, "error": e.to_string()}),
            }
        }
        _ => json!({"ok": false, "error": "unknown tool"}),
    }
}

async fn call_llm_once(
    state: &AppState,
    model_override: Option<&str>,
    rec: &AgentSessionRecord,
    user_message: &str,
) -> Result<String, String> {
    let llm = state.app_state.resolve_llm(
        state.llm_base.as_deref(),
        state.llm_key.as_deref(),
        &state.llm_model,
    );
    if llm.base_url.trim().is_empty() || llm.api_key.trim().is_empty() {
        return Err("llm not configured".into());
    }
    let model = model_override
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            if llm.model.is_empty() {
                state.llm_model.clone()
            } else {
                llm.model.clone()
            }
        });

    // P4 (审查C A1 接线): harness::compress_to_narrative —— 会话历史先经叙事压缩再进上下文：
    // 过滤 tool/system 噪声与 <thinking>/tool-call 块，只留 [用户]/[助理] 叙事对。
    // 取最近 24 条原始消息压缩（窗口 2 倍于旧 12 条直拼：噪声被剥离后有效叙事密度更高），
    // 压缩结果作为单个 system 块注入；最近一条 user 消息仍以正式 user turn 发送。
    let recent: Vec<kaleido_core::AgentSessionMessage> = rec
        .messages
        .iter()
        .rev()
        .take(24)
        .cloned()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let narrative = kaleido_core::harness::compress_to_narrative(&recent);
    let (raw_tok, narr_tok, saved_pct) =
        kaleido_core::harness::estimate_savings(&recent, &narrative);
    tracing::info!(
        raw_tokens = raw_tok,
        narrative_tokens = narr_tok,
        savings_pct = format!("{saved_pct:.1}"),
        turns = narrative.len(),
        "P4 harness: agent session context compressed"
    );
    let history_block = kaleido_core::harness::format_narrative_context(&narrative);
    let system_prompt = format!(
        "You are Kaleido agent. Be concise. Use tools only when needed.\n\n## Session history (compressed narrative)\n{history_block}"
    );

    let mut messages = vec![json!({
        "role": "system",
        "content": system_prompt,
    })];
    // Ensure latest user message present as the live turn
    let last_user_live = rec
        .messages
        .last()
        .map(|m| m.role == "user" && m.content == user_message)
        .unwrap_or(false);
    if !last_user_live {
        messages.push(json!({"role": "user", "content": user_message}));
    }

    let client = reqwest::Client::builder()
        .timeout(StdDuration::from_secs(45))
        .build()
        .map_err(|e| e.to_string())?;
    let url = format!("{}/chat/completions", llm.base_url.trim_end_matches('/'));
    let body = json!({
        "model": model,
        "stream": false,
        "temperature": 0.5,
        "max_tokens": 1024,
        "messages": messages,
    });
    let resp = client
        .post(&url)
        .bearer_auth(&llm.api_key)
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("connect: {e}"))?;
    if !resp.status().is_success() {
        let st = resp.status();
        let t = resp.text().await.unwrap_or_default();
        return Err(format!(
            "upstream {st}: {}",
            t.chars().take(200).collect::<String>()
        ));
    }
    let v: Value = resp.json().await.map_err(|e| e.to_string())?;
    let content = v
        .pointer("/choices/0/message/content")
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if content.is_empty() {
        return Err("empty llm content".into());
    }
    Ok(content)
}
