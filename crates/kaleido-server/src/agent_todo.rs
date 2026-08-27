//! Session-scoped agent todos API (S7-W3 T1).
//!
//! Routes:
//! - `GET  /api/v1/agent/sessions/{id}/todos`
//! - `PUT  /api/v1/agent/sessions/{id}/todos`  (replace full list)
//! - `POST /api/v1/agent/tools/todo`          body `{sessionId, items}`

use axum::{
    extract::{Path, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use kaleido_core::AgentSessionStore;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{map_core_err, session_from, AppState};
use crate::error_codes::*;

const ALLOWED_STATUS: &[&str] = &["pending", "in_progress", "completed", "cancelled"];

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolTodoBody {
    pub session_id: String,
    #[serde(default)]
    pub items: Option<Vec<Value>>,
    #[serde(default)]
    pub todos: Option<Vec<Value>>,
}

/// Router fragment for main to `.merge(...)`.
pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/agent/sessions/{id}/todos",
            get(get_todos).put(put_todos),
        )
        .route("/api/v1/agent/tools/todo", post(tool_todo))
}

fn validate_todo_item(v: &Value) -> Result<(), String> {
    let obj = v
        .as_object()
        .ok_or_else(|| "todo item must be an object".to_string())?;
    let id = obj
        .get("id")
        .and_then(|x| x.as_str())
        .ok_or_else(|| "todo item missing string id".to_string())?;
    if id.is_empty() || id.len() > 128 {
        return Err("todo id must be 1..=128 chars".into());
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        return Err("todo id charset invalid".into());
    }
    let content = obj
        .get("content")
        .and_then(|x| x.as_str())
        .ok_or_else(|| "todo item missing string content".to_string())?;
    if content.len() > 8 * 1024 {
        return Err("todo content too long".into());
    }
    let status = obj
        .get("status")
        .and_then(|x| x.as_str())
        .ok_or_else(|| "todo item missing string status".to_string())?;
    if !ALLOWED_STATUS.contains(&status) {
        return Err(format!(
            "invalid todo status '{status}' (pending|in_progress|completed|cancelled)"
        ));
    }
    Ok(())
}

fn validate_todos(items: &[Value]) -> Result<(), String> {
    if items.len() > 256 {
        return Err("too many todos (max 256)".into());
    }
    let mut seen = std::collections::HashSet::new();
    for it in items {
        validate_todo_item(it)?;
        let id = it.get("id").and_then(|x| x.as_str()).unwrap_or("");
        if !seen.insert(id.to_string()) {
            return Err(format!("duplicate todo id '{id}'"));
        }
    }
    Ok(())
}

fn normalize_item(v: &Value) -> Value {
    // Keep only id/content/status so storage stays minimal.
    json!({
        "id": v.get("id").and_then(|x| x.as_str()).unwrap_or(""),
        "content": v.get("content").and_then(|x| x.as_str()).unwrap_or(""),
        "status": v.get("status").and_then(|x| x.as_str()).unwrap_or("pending"),
    })
}

async fn get_todos(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    match state.sessions.load(&id) {
        Ok(rec) => Json(json!({
            "sessionId": rec.id,
            "todos": rec.todos,
        }))
        .into_response(),
        Err(e) => map_core_err(e),
    }
}

async fn put_todos(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let items = match parse_items_value(body) {
        Ok(v) => v,
        Err(msg) => {
            return bad_request("TODO_BAD_REQUEST", msg);
        }
    };
    replace_todos(&state.sessions, &id, items)
}

async fn tool_todo(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ToolTodoBody>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let items = body.items.or(body.todos).unwrap_or_default();
    if let Err(msg) = validate_todos(&items) {
        return bad_request("TODO_BAD_REQUEST", msg);
    }
    let normalized: Vec<Value> = items.iter().map(normalize_item).collect();
    replace_todos(&state.sessions, &body.session_id, normalized)
}

fn parse_items_value(body: Value) -> Result<Vec<Value>, String> {
    if let Some(arr) = body.as_array() {
        validate_todos(arr)?;
        return Ok(arr.iter().map(normalize_item).collect());
    }
    if let Some(obj) = body.as_object() {
        if let Some(arr) = obj.get("items").and_then(|x| x.as_array()) {
            validate_todos(arr)?;
            return Ok(arr.iter().map(normalize_item).collect());
        }
        if let Some(arr) = obj.get("todos").and_then(|x| x.as_array()) {
            validate_todos(arr)?;
            return Ok(arr.iter().map(normalize_item).collect());
        }
    }
    Err("body must be a todo array or {items|todos: [...]}".into())
}

fn replace_todos(store: &AgentSessionStore, id: &str, items: Vec<Value>) -> Response {
    if let Err(msg) = validate_todos(&items) {
        return bad_request("TODO_BAD_REQUEST", msg);
    }
    let mut rec = match store.load(id) {
        Ok(r) => r,
        Err(e) => return map_core_err(e),
    };
    rec.todos = items.iter().map(normalize_item).collect();
    rec.saved_at = 0; // save() fills now_millis when 0
    match store.save(rec) {
        Ok(_) => match store.load(id) {
            Ok(rec) => Json(json!({
                "sessionId": rec.id,
                "todos": rec.todos,
            }))
            .into_response(),
            Err(e) => map_core_err(e),
        },
        Err(e) => map_core_err(e),
    }
}

