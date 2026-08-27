//! Decision Cards API (Liyuan-inspired)
//!
//! Agent can create structured decisions with choices for the user.
//! Instead of embedding <choices> in story text, decisions are first-class
//! API objects that the frontend can render as interactive cards.
//!
//! Routes:
//! - `POST /api/v1/agent/decisions` — agent creates a decision
//! - `GET  /api/v1/agent/decisions/{session_id}` — list pending decisions
//! - `POST /api/v1/agent/decisions/{session_id}/choose` — user chooses option

use axum::{
    extract::{Path, State},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::{map_core_err, session_from, AppState};
use crate::error_codes::*;

/// A decision card presented by the agent
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionCard {
    pub id: String,
    pub session_id: String,
    pub question: String,
    pub options: Vec<DecisionOption>,
    pub status: String, // "Pending" | "Resolved" | "Expired"
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chosen_option_id: Option<String>,
}

/// A single option in a decision card
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionOption {
    pub id: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Request to create a new decision
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateDecisionRequest {
    pub session_id: String,
    pub question: String,
    pub options: Vec<DecisionOptionRequest>,
}

/// A single option in a creation request
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionOptionRequest {
    pub text: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// Request to choose an option
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChooseRequest {
    pub option_id: String,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/agent/decisions", post(create_decision))
        .route(
            "/api/v1/agent/decisions/{session_id}",
            get(list_decisions),
        )
        .route(
            "/api/v1/agent/decisions/{session_id}/choose",
            post(choose_option),
        )
}

/// Agent creates a decision card for the user
async fn create_decision(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<CreateDecisionRequest>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let mut rec = match state.sessions.load(&body.session_id) {
        Ok(r) => r,
        Err(e) => return map_core_err(e),
    };

    let card = DecisionCard {
        id: Uuid::new_v4().to_string(),
        session_id: body.session_id.clone(),
        question: body.question,
        options: body
            .options
            .into_iter()
            .map(|o| DecisionOption {
                id: Uuid::new_v4().to_string(),
                text: o.text,
                description: o.description,
            })
            .collect(),
        status: "Pending".into(),
        created_at: Utc::now().to_rfc3339(),
        resolved_at: None,
        chosen_option_id: None,
    };

    // Store decision in the session record's decisions field
    let card_value = serde_json::to_value(&card).unwrap_or_default();
    rec.decisions.push(card_value.clone());

    match state.sessions.save(rec) {
        Ok(_) => Json(json!({"ok": true, "decision": card})).into_response(),
        Err(e) => map_core_err(e),
    }
}

/// List all decisions for a session
async fn list_decisions(
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

    let decisions: Vec<DecisionCard> = rec
        .decisions
        .iter()
        .filter_map(|v| serde_json::from_value(v.clone()).ok())
        .collect();

    Json(json!({
        "ok": true,
        "sessionId": session_id,
        "decisions": decisions,
        "count": decisions.len(),
    }))
    .into_response()
}

/// User chooses an option in a pending decision
async fn choose_option(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(session_id): Path<String>,
    Json(body): Json<ChooseRequest>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let mut rec = match state.sessions.load(&session_id) {
        Ok(r) => r,
        Err(e) => return map_core_err(e),
    };

    let now = Utc::now().to_rfc3339();
    let mut found = false;

    for decision_value in rec.decisions.iter_mut() {
        if let Some(status) = decision_value.get("status").and_then(|s| s.as_str()) {
            if status == "Pending" || status == "pending" {
                if let Some(obj) = decision_value.as_object_mut() {
                    obj.insert("status".into(), json!("Resolved"));
                    obj.insert("resolvedAt".into(), json!(now));
                    obj.insert("chosenOptionId".into(), json!(body.option_id));
                    found = true;
                    break;
                }
            }
        }
    }

    if !found {
        return not_found("DEC_NOT_FOUND", "no pending decision found for this session");
    }

    match state.sessions.save(rec) {
        Ok(_) => Json(json!({"ok": true, "chosen": body.option_id})).into_response(),
        Err(e) => map_core_err(e),
    }
}
