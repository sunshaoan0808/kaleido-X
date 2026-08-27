//! Per-user app_state persistence (S7-W5 T7).
//! GET/PUT /api/v1/app-state

use axum::{
    extract::State,
    http::HeaderMap,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde_json::{json, Value};
use std::fs;

use crate::{session_from, AppState};
use crate::error_codes::*;

pub fn router() -> Router<AppState> {
    Router::new().route("/api/v1/app-state", get(get_state).put(put_state))
}

fn path_for(state: &AppState, user_id: &str) -> std::path::PathBuf {
    state
        .auth
        .data_root()
        .root()
        .join("users")
        .join(user_id)
        .join("app_state.json")
}

async fn get_state(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let sess = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let path = path_for(&state, &sess.user_id);
    if !path.exists() {
        return Json(json!({
            "ok": true,
            "state": {
                "activeTab": "home",
                "worksPath": "",
                "partnerSelection": null,
                "apiBaseHint": null,
                "ui": {
                    "theme": "default",
                    "appearance": null
                }
            }
        }))
        .into_response();
    }
    match fs::read_to_string(&path) {
        Ok(raw) => {
            let v: Value = serde_json::from_str(&raw).unwrap_or(json!({}));
            Json(json!({"ok": true, "state": v})).into_response()
        }
        Err(e) => internal("UAS_INTERNAL", e.to_string()),
    }
}

async fn put_state(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let sess = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let v: Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            return bad_request("UAS_INVALID", format!("invalid json: {e}"));
        }
    };
    // Accept either raw state object or {state: ...}
    let state_val = v.get("state").cloned().unwrap_or(v);
    let path = path_for(&state, &sess.user_id);
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    match fs::write(&path, serde_json::to_string_pretty(&state_val).unwrap_or_else(|_| "{}".into()))
    {
        Ok(()) => Json(json!({"ok": true, "state": state_val})).into_response(),
        Err(e) => internal("UAS_INTERNAL", e.to_string()),
    }
}
