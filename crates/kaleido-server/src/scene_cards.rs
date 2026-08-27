//! U3 场记卡 API：作品维度的场记卡列表（每场自动生成，资料抽屉「场记」视图的数据源）。
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get};
use axum::{Json, Router};
use serde_json::json;

use crate::{session_from, AppState};
use crate::error_codes::*;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/works/{work_id}/scene-cards",
            get(list_scene_cards_h).delete(clear_scene_cards_h),
        )
        .route(
            "/api/v1/works/{work_id}/scene-cards/{card_id}",
            delete(delete_scene_card_h),
        )
}

async fn list_scene_cards_h(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(work_id): Path<String>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    match state.scene_cards.list_by_work(&work_id) {
        Ok(cards) => Json(json!({ "work_id": work_id, "cards": cards })).into_response(),
        Err(e) => internal("CARD_INTERNAL", e.to_string()),
    }
}

async fn delete_scene_card_h(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((work_id, card_id)): Path<(String, String)>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    // 只允许删除属于该作品的卡（防越权删除其他作品）
    let cards = match state.scene_cards.list_by_work(&work_id) {
        Ok(c) => c,
        Err(e) => {
            return internal("CARD_INTERNAL", e.to_string())
        }
    };
    if !cards.iter().any(|c| c.id == card_id) {
        return not_found("CARD_NOT_FOUND", "scene card not found in this work");
    }
    match state.scene_cards.delete(&card_id) {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => return not_found("CARD_NOT_FOUND", "scene card not found"),
        Err(e) => internal("CARD_INTERNAL", e.to_string()),
    }
}

async fn clear_scene_cards_h(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(work_id): Path<String>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    match state.scene_cards.clear_work(&work_id) {
        Ok(n) => Json(json!({ "deleted": n })).into_response(),
        Err(e) => internal("CARD_INTERNAL", e.to_string()),
    }
}
