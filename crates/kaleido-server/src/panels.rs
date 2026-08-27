//! Dynamic Panels API (Liyuan-inspired)
//!
//! Agent can create named UI panels (maps, equipment, clue boards,
//! character sheets) that are rendered alongside the story.
//!
//! Unlike traditional static UI, these panels are:
//! - Created dynamically by the agent during the story
//! - Tied to the session/world line
//! - Persisted and versioned
//! - Renderable as SVG or structured JSON on the frontend
//!
//! Routes:
//! - `POST /api/v1/agent/panels` — create or update a panel
//! - `GET  /api/v1/agent/panels/{session_id}` — list panels for session
//! - `DELETE /api/v1/agent/panels/{panel_id}` — remove a panel

use axum::{
    extract::{Path, State},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Json, Router,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{map_core_err, session_from, AppState};
use crate::error_codes::*;

/// A dynamic panel created by the agent
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentPanel {
    pub id: String,
    pub session_id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub panel_type: String,
    /// The panel content. For maps, SVG string; for sheets, structured JSON.
    pub content: Value,
    pub created_at: String,
    pub updated_at: String,
}

/// Request to create or update a panel
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatePanelRequest {
    pub session_id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub panel_type: String,
    pub content: Value,
}

/// Request to update an existing panel
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)] // [P7] 面板更新请求体预留（当前整卡 PUT）
pub struct UpdatePanelRequest {
    pub name: Option<String>,
    #[serde(rename = "type")]
    pub panel_type: Option<String>,
    pub content: Option<Value>,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/agent/panels", post(create_or_update_panel))
        .route(
            "/api/v1/agent/panels/{session_id}",
            get(list_panels),
        )
        .route(
            "/api/v1/agent/panels/item/{panel_id}",
            delete(delete_panel),
        )
}

/// Create a new panel for a session
async fn create_or_update_panel(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<CreatePanelRequest>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let mut rec = match state.sessions.load(&body.session_id) {
        Ok(r) => r,
        Err(e) => return map_core_err(e),
    };

    let now = Utc::now().to_rfc3339();
    let panel = AgentPanel {
        id: Uuid::new_v4().to_string(),
        session_id: body.session_id.clone(),
        name: body.name,
        panel_type: body.panel_type,
        content: body.content,
        created_at: now.clone(),
        updated_at: now,
    };

    let panel_value = serde_json::to_value(&panel).unwrap_or_default();
    rec.panels.push(panel_value);

    match state.sessions.save(rec) {
        Ok(_) => Json(json!({"ok": true, "panel": panel})).into_response(),
        Err(e) => map_core_err(e),
    }
}

/// List all panels for a session
async fn list_panels(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(session_id): Path<String>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let rec = match state.sessions.load(&session_id) {
        Ok(r) => r,
        Err(e) => return map_core_err(e),
    };

    let panels: Vec<AgentPanel> = rec
        .panels
        .iter()
        .filter_map(|v| serde_json::from_value(v.clone()).ok())
        .collect();

    Json(json!({
        "ok": true,
        "sessionId": session_id,
        "panels": panels,
        "count": panels.len(),
    }))
    .into_response()
}

/// Delete a panel by ID
async fn delete_panel(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(panel_id): Path<String>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    // Need to find which session has this panel
    // Simplest: iterate sessions (for MVP)
    // For production, store panel_id→session_id index
    // For now, return not-found if we can't find it
    let all_sessions = match state.sessions.list("", None) {
        Ok(list) => list,
        Err(e) => return map_core_err(e),
    };

    for summary in &all_sessions {
        if let Ok(mut rec) = state.sessions.load(&summary.id) {
            let before = rec.panels.len();
            rec.panels.retain(|v| {
                v.get("id").and_then(|i| i.as_str()) != Some(&panel_id)
            });
            if rec.panels.len() < before {
                return match state.sessions.save(rec) {
                    Ok(_) => Json(json!({"ok": true, "deleted": panel_id})).into_response(),
                    Err(e) => map_core_err(e),
                };
            }
        }
    }

    return not_found("PANEL_NOT_FOUND", "panel not found");
}
