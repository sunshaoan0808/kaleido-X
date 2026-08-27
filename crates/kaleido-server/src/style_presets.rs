//! Style presets API (S7-W2).
//! GET  /api/v1/style-presets  — list presets from AppStateStore
//! PUT  /api/v1/style-presets  — replace full list (bearer)

use axum::{
    extract::State,
    http::HeaderMap,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde_json::{json, Value};

use crate::{map_core_err, session_from, AppState};
use crate::error_codes::*;

const STATE_NAME: &str = "style-presets";

pub fn router() -> Router<AppState> {
    Router::new().route(
        "/api/v1/style-presets",
        get(list_presets).put(save_presets),
    )
}

fn parse_presets(raw: &str) -> Value {
    match serde_json::from_str::<Value>(raw) {
        Ok(v) if !v.is_null() => v,
        _ => json!([]),
    }
}

async fn list_presets(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    match state.app_state.load(STATE_NAME) {
        Ok(raw) => Json(parse_presets(&raw)).into_response(),
        Err(e) => map_core_err(e),
    }
}

async fn save_presets(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    // Accept array or object; reject other JSON types for clearer API errors.
    if !(body.is_array() || body.is_object()) {
        return bad_request("STYLE_BAD_JSON", "style-presets body must be a JSON array or object");
    }
    let content = match serde_json::to_string_pretty(&body) {
        Ok(s) => s,
        Err(e) => {
            return bad_request("STYLE_INVALID", format!("invalid json: {e}"));
        }
    };
    match state.app_state.save(STATE_NAME, &content) {
        Ok(()) => Json(body).into_response(),
        Err(e) => map_core_err(e),
    }
}
