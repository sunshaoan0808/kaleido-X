//! U6: 对话质量检测 API（T1 创作质量 · 第三优先）。
//!
//! 纯规则零 LLM，复用 core `dialogue_fingerprint`：
//! - `POST /api/v1/story-tavern/packs/{pack_id}/dialogue-fingerprints`
//!   生成 pack 全部角色指纹 json（per-character fingerprint）。
//! - `POST /api/v1/story-tavern/packs/{pack_id}/dialogue-drift`
//!   body `{ "characterId": "...", "content": "..." }` → 漂移分 [0,1] + 越界/口头禅。
//! - `GET  /api/v1/story-tavern/packs/{pack_id}/dialogue-fingerprints`
//!   幂等读取（不生成，仅返回已计算值，用于前端轮询）。

use axum::{
    extract::{Path, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};

use kaleido_core::dialogue_fingerprint::{build_all, drift_check};
use crate::{map_core_err, session_from, AppState};
use crate::error_codes::*;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/story-tavern/packs/{pack_id}/dialogue-fingerprints",
            get(get_fingerprints).post(get_fingerprints),
        )
        .route(
            "/api/v1/story-tavern/packs/{pack_id}/dialogue-drift",
            post(drift_route),
        )
}

/// 请求体：漂移检测。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DriftBody {
    #[serde(default)]
    character_id: String, // 兼容 camelCase
    #[serde(default)]
    content: String,
}

fn ok_value(v: Value) -> Response {
    Json(v).into_response()
}

async fn get_fingerprints(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(pack_id): Path<String>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let pack = match state.packs.get(&pack_id) {
        Ok(p) => p,
        Err(e) => return map_core_err(e),
    };
    let fps = build_all(&pack);
    let items: Vec<Value> = fps.iter().map(|fp| serde_json::to_value(fp).unwrap_or(Value::Null)).collect();
    ok_value(json!({
        "pack_id": pack_id,
        "characters": items,
        "total": fps.len(),
    }))
}

async fn drift_route(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(pack_id): Path<String>,
    Json(body): Json<DriftBody>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    if body.character_id.trim().is_empty() {
        return bad_request("MAIN_BAD_REQUEST", "characterId is required");
    }
    if body.content.trim().is_empty() {
        return bad_request("MAIN_BAD_REQUEST", "content is empty");
    }
    let pack = match state.packs.get(&pack_id) {
        Ok(p) => p,
        Err(e) => return map_core_err(e),
    };
    let Some(character) = pack.characters.iter().find(|c| c.id == body.character_id) else {
        return not_found("MAIN_NOT_FOUND", format!("character not found: {}", body.character_id));
    };
    let fp = kaleido_core::dialogue_fingerprint::build_fingerprint(character);
    let report = drift_check(&fp, &body.content);
    ok_value(json!({
        "pack_id": pack_id,
        "character": character.name,
        "fingerprint": fp,
        "drift": report,
    }))
}