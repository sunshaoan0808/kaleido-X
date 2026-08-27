//! Works filesystem API (S4) — extracted P0-1 Stage4
use axum::{
    extract::{DefaultBodyLimit, Query, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use kaleido_core::{
    works_limits_public, WORKS_DEFAULT_LIST_DEPTH, WORKS_MAX_FILE_BYTES, WORKS_MAX_LIST_DEPTH,
};
use serde::Deserialize;
use serde_json::json;

use crate::auth_mw::session_from;
use crate::error_codes::*;
use crate::error_map::map_core_err;
use crate::state::AppState;

pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(works_list).delete(works_delete))
        .route("/limits", get(works_limits))
        .route("/stat", get(works_stat))
        .route("/file",
            get(works_read_file)
                .put(works_write_file)
                .layer(DefaultBodyLimit::max(
                    // JSON wrapper overhead + a bit past WORKS_MAX so coded error wins over 413
                    (WORKS_MAX_FILE_BYTES as usize)
                        .saturating_mul(2)
                        .saturating_add(64 * 1024),
                )),
        )
        .route("/dir", post(works_mkdir))
        .route("/rename", post(works_rename))
}


#[derive(Deserialize)]
pub(crate) struct WorksPathQuery {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    depth: Option<u32>,
    #[serde(default)]
    recursive: Option<bool>,
}

#[derive(Deserialize)]
pub(crate) struct WorksWriteBody {
    path: String,
    content: String,
}

#[derive(Deserialize)]
pub(crate) struct WorksDirBody {
    path: String,
}

#[derive(Deserialize)]
pub(crate) struct WorksRenameBody {
    from: String,
    to: String,
}

pub(crate) async fn works_limits(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    Json(works_limits_public()).into_response()
}

pub(crate) async fn works_list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<WorksPathQuery>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let path = q.path.as_deref().unwrap_or("");
    let depth = q.depth.unwrap_or(WORKS_DEFAULT_LIST_DEPTH).min(WORKS_MAX_LIST_DEPTH);
    match state.works.list(&session.workspace_id, path, depth) {
        Ok(entry) => Json(entry).into_response(),
        Err(e) => map_core_err(e),
    }
}

pub(crate) async fn works_stat(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<WorksPathQuery>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let path = q.path.as_deref().unwrap_or("");
    match state.works.stat(&session.workspace_id, path) {
        Ok(st) => Json(st).into_response(),
        Err(e) => map_core_err(e),
    }
}

pub(crate) async fn works_read_file(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<WorksPathQuery>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let path = match q.path.as_deref() {
        Some(p) if !p.is_empty() => p,
        _ => {
            return bad_request("WK_MISSING_FIELD", "path required");
        }
    };
    match state.works.read_text(&session.workspace_id, path) {
        Ok(body) => Json(body).into_response(),
        Err(e) => map_core_err(e),
    }
}

pub(crate) async fn works_write_file(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<WorksWriteBody>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    match state
        .works
        .write_text(&session.workspace_id, &body.path, &body.content)
    {
        Ok(out) => Json(out).into_response(),
        Err(e) => map_core_err(e),
    }
}

pub(crate) async fn works_mkdir(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<WorksDirBody>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    match state.works.mkdir(&session.workspace_id, &body.path) {
        Ok(st) => Json(st).into_response(),
        Err(e) => map_core_err(e),
    }
}

pub(crate) async fn works_rename(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<WorksRenameBody>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    match state
        .works
        .rename(&session.workspace_id, &body.from, &body.to)
    {
        Ok(st) => Json(st).into_response(),
        Err(e) => map_core_err(e),
    }
}

pub(crate) async fn works_delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<WorksPathQuery>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let path = match q.path.as_deref() {
        Some(p) if !p.is_empty() => p,
        _ => {
            return bad_request("WK_MISSING_FIELD", "path required");
        }
    };
    let recursive = q.recursive.unwrap_or(false);
    match state.works.delete(&session.workspace_id, path, recursive) {
        Ok(()) => Json(json!({"ok": true})).into_response(),
        Err(e) => map_core_err(e),
    }
}
