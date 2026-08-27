//! Chapter outlines + foreshadows API (P2) — foreshadow DAG (T1).
//!
//! Routes (all require session auth):
//! - `GET    /api/v1/works/{work_id}/outlines`                              list chapter outlines
//! - `GET    /api/v1/works/{work_id}/chapters/{chapter_id}/outline`         get one outline (200|404)
//! - `PUT    /api/v1/works/{work_id}/chapters/{chapter_id}/outline`         upsert outline
//! - `DELETE /api/v1/works/{work_id}/chapters/{chapter_id}/outline`         delete outline (204)
//! - `GET    /api/v1/works/{work_id}/foreshadows?status=&weight_min=`       list foreshadows (default all; optional weight filter)
//! - `GET    /api/v1/works/{work_id}/foreshadows/stats`                     stats: total + by status + avg weight
//! - `POST   /api/v1/works/{work_id}/foreshadows`                           create foreshadow -> 201
//! - `GET    /api/v1/works/{work_id}/foreshadows/{id}`                      get foreshadow (200|404)
//! - `PATCH  /api/v1/works/{work_id}/foreshadows/{id}`                      update foreshadow (title/desc/status/weight/parents)
//! - `DELETE /api/v1/works/{work_id}/foreshadows/{id}`                      delete foreshadow (204)
//! - `POST   /api/v1/works/{work_id}/foreshadows/{id}/dependencies`         add dependency edge -> 201 (409 on cycle)
//! - `DELETE /api/v1/works/{work_id}/foreshadows/{id}/dependencies/{parent_id}`  remove dependency edge (204)
//! - `GET    /api/v1/works/{work_id}/foreshadows/{id}/dependencies`         list parent ids
//! - `GET    /api/v1/works/{work_id}/foreshadows/{id}/dependents`           list reverse (dependents) ids
//! - `POST   /api/v1/works/{work_id}/foreshadows/{id}/occurrences`          add occurrence -> 201
//! - `DELETE /api/v1/works/{work_id}/foreshadows/{id}/occurrences/{occurrence_id}`  remove occurrence (204)
//!
//! Error mapping: 409 version conflict / duplicate occurrence / dependency cycle,
//! 404 missing entity, 400 invalid status/type/weight/bad request, 500 db failure.

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Json, Router,
};
use kaleido_core::foreshadow_store::ForeshadowError;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{session_from, AppState};
use crate::error_codes::*;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/works/{work_id}/outlines",
            get(list_outlines_h),
        )
        .route(
            "/api/v1/works/{work_id}/chapters/{chapter_id}/outline",
            get(get_outline_h).put(upsert_outline_h).delete(delete_outline_h),
        )
        .route(
            "/api/v1/works/{work_id}/foreshadows",
            get(list_foreshadows_h).post(create_foreshadow_h),
        )
        .route(
            "/api/v1/works/{work_id}/foreshadows/stats",
            get(foreshadow_stats_h),
        )
        .route(
            "/api/v1/works/{work_id}/foreshadows/{id}",
            get(get_foreshadow_h).patch(update_foreshadow_h).delete(delete_foreshadow_h),
        )
        .route(
            "/api/v1/works/{work_id}/foreshadows/{id}/dependencies",
            get(get_dependencies_h).post(set_dependency_h),
        )
        .route(
            "/api/v1/works/{work_id}/foreshadows/{id}/dependencies/{parent_id}",
            delete(remove_dependency_h),
        )
        .route(
            "/api/v1/works/{work_id}/foreshadows/{id}/dependents",
            get(get_dependents_h),
        )
        .route(
            "/api/v1/works/{work_id}/foreshadows/{id}/occurrences",
            post(add_occurrence_h),
        )
        .route(
            "/api/v1/works/{work_id}/foreshadows/{id}/occurrences/{occurrence_id}",
            delete(remove_occurrence_h),
        )
}

fn foreshadow_err(e: ForeshadowError) -> Response {
    let (code, body) = match e {
        ForeshadowError::Db(e) => (StatusCode::INTERNAL_SERVER_ERROR, json!({ "error": format!("{e}") })),
        ForeshadowError::NotFound(what) => {
            (StatusCode::NOT_FOUND, json!({ "error": format!("{what} not found") }))
        }
        ForeshadowError::VersionConflict { id, expected, actual } => (
            StatusCode::CONFLICT,
            json!({
                "error": "version conflict",
                "id": id,
                "expected": expected,
                "actual": actual,
            }),
        ),
        ForeshadowError::InvalidStatus(s) => (
            StatusCode::BAD_REQUEST,
            json!({ "error": format!("invalid status '{s}' (planted|active|recalled)") }),
        ),
        ForeshadowError::InvalidType(t) => (
            StatusCode::BAD_REQUEST,
            json!({ "error": format!("invalid type '{t}' (plant|remind|recover)") }),
        ),
        ForeshadowError::InvalidWeight(w) => (
            StatusCode::BAD_REQUEST,
            json!({ "error": format!("invalid weight '{w}' (must be 1-10)") }),
        ),
        ForeshadowError::DuplicateOccurrence => (
            StatusCode::CONFLICT,
            json!({ "error": "occurrence already exists for this chapter+type" }),
        ),
        ForeshadowError::Cycle(id) => (
            StatusCode::CONFLICT,
            json!({ "error": format!("dependency cycle detected involving foreshadow '{id}'") }),
        ),
        ForeshadowError::BadRequest(msg) => (StatusCode::BAD_REQUEST, json!({ "error": msg })),
    };
    (code, Json(body)).into_response()
}

fn ok_value(v: Value) -> Response {
    Json(v).into_response()
}

fn no_content() -> Response {
    StatusCode::NO_CONTENT.into_response()
}

// ---------------------------------------------------------------------------
// Bodies
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OutlineBody {
    #[serde(default)]
    goal: Option<String>,
    #[serde(default)]
    conflicts: Option<Vec<String>>,
    #[serde(default)]
    twists: Option<Vec<String>>,
    #[serde(default)]
    change_note: Option<String>,
    #[serde(default)]
    expected_version_no: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct VersionBody {
    #[serde(default)]
    expected_version_no: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateForeshadowBody {
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    status: Option<String>,
}

impl CreateForeshadowBody {
    fn validate(&self) -> Result<(), &'static str> {
        if self.title.trim().is_empty() {
            return Err("title must not be empty");
        }
        if self.title.chars().count() > 200 {
            return Err("title too long (max 200 chars)");
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateForeshadowBody {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    weight: Option<i32>,
    #[serde(default)]
    parents: Option<Vec<String>>,
    #[serde(default)]
    expected_version_no: Option<i64>,
}

impl UpdateForeshadowBody {
    fn validate(&self) -> Result<(), &'static str> {
        if let Some(t) = &self.title {
            if t.trim().is_empty() {
                return Err("title must not be empty");
            }
            if t.chars().count() > 200 {
                return Err("title too long (max 200 chars)");
            }
        }
        if let Some(w) = self.weight {
            if !(1..=10).contains(&w) {
                return Err("weight must be 1-10");
            }
        }
        if self.title.is_none()
            && self.description.is_none()
            && self.status.is_none()
            && self.weight.is_none()
            && self.parents.is_none()
        {
            return Err("at least one of title/description/status/weight/parents must be provided");
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddDependencyBody {
    parent_id: String,
    #[serde(default)]
    expected_version_no: Option<i64>,
}

impl AddDependencyBody {
    fn validate(&self) -> Result<(), &'static str> {
        if self.parent_id.trim().is_empty() {
            return Err("parentId must not be empty");
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddOccurrenceBody {
    chapter_id: String,
    #[serde(rename = "type")]
    typ: String,
    #[serde(default)]
    note: String,
    #[serde(default)]
    expected_version_no: Option<i64>,
}

impl AddOccurrenceBody {
    fn validate(&self) -> Result<(), &'static str> {
        if self.chapter_id.trim().is_empty() {
            return Err("chapterId must not be empty");
        }
        if !matches!(self.typ.as_str(), "plant" | "remind" | "recover") {
            return Err("type must be one of plant|remind|recover");
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ForeshadowListQuery {
    #[serde(default = "default_status")]
    status: String,
    #[serde(default)]
    weight_min: Option<i64>,
}

fn default_status() -> String {
    "all".to_string()
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn list_outlines_h(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(work_id): Path<String>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    match state.foreshadow.list_outlines(&work_id) {
        Ok(list) => ok_value(json!(list)),
        Err(e) => foreshadow_err(e),
    }
}

async fn get_outline_h(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((work_id, chapter_id)): Path<(String, String)>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    match state.foreshadow.get_outline(&work_id, &chapter_id) {
        Ok(Some(o)) => ok_value(json!(o)),
        Ok(None) => return not_found("FS_NOT_FOUND", format!("outline for chapter '{chapter_id}' not found")),
        Err(e) => foreshadow_err(e),
    }
}

async fn upsert_outline_h(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((work_id, chapter_id)): Path<(String, String)>,
    Json(body): Json<OutlineBody>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    match state.foreshadow.upsert_outline(
        &work_id,
        &chapter_id,
        body.goal.unwrap_or_default(),
        body.conflicts.unwrap_or_default(),
        body.twists.unwrap_or_default(),
        body.change_note.unwrap_or_default(),
        body.expected_version_no,
    ) {
        Ok(o) => ok_value(json!(o)),
        Err(e) => foreshadow_err(e),
    }
}

async fn delete_outline_h(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((work_id, chapter_id)): Path<(String, String)>,
    body: Option<Json<VersionBody>>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let expected = body.map(|b| b.expected_version_no).flatten();
    match state.foreshadow.delete_outline(&work_id, &chapter_id, expected) {
        Ok(()) => no_content(),
        Err(e) => foreshadow_err(e),
    }
}

async fn list_foreshadows_h(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(work_id): Path<String>,
    Query(query): Query<ForeshadowListQuery>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let status = match query.status.as_str() {
        "all" => None,
        s @ ("planted" | "active" | "recalled") => Some(s),
        other => {
            return bad_request("FS_INVALID", format!("invalid status '{other}' (all|planted|active|recalled)"));
        }
    };
    let weight_min: Option<i32> = match query.weight_min {
        None => None,
        Some(w) if w < 1 => {
            return bad_request("FS_BAD_REQUEST", "weight_min must be >= 1");
        }
        Some(w) => Some(w as i32),
    };
    match state.foreshadow.list_foreshadows(&work_id, status, weight_min) {
        Ok(list) => ok_value(json!(list)),
        Err(e) => foreshadow_err(e),
    }
}

async fn create_foreshadow_h(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(work_id): Path<String>,
    Json(body): Json<CreateForeshadowBody>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    if let Err(msg) = body.validate() {
        return bad_request("FS_BAD_REQUEST", msg);
    }
    let title = body.title.trim().to_string();
    match state
        .foreshadow
        .create_foreshadow(&work_id, title, body.description, body.status.unwrap_or_default())
    {
        Ok(f) => (StatusCode::CREATED, Json(json!(f))).into_response(),
        Err(e) => foreshadow_err(e),
    }
}

async fn get_foreshadow_h(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((_work_id, id)): Path<(String, String)>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    match state.foreshadow.get_foreshadow(&id) {
        Ok(Some(f)) => ok_value(json!(f)),
        Ok(None) => return not_found("FS_NOT_FOUND", format!("foreshadow '{id}' not found")),
        Err(e) => foreshadow_err(e),
    }
}

async fn update_foreshadow_h(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((_work_id, id)): Path<(String, String)>,
    Json(body): Json<UpdateForeshadowBody>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    if let Err(msg) = body.validate() {
        return bad_request("FS_BAD_REQUEST", msg);
    }
    let parents = body.parents.map(|p| {
        p.into_iter().map(|pid| pid.trim().to_string()).collect::<Vec<String>>()
    });
    match state.foreshadow.update_foreshadow(
        &id,
        body.title.map(|t| t.trim().to_string()),
        body.description,
        body.status,
        body.weight,
        parents,
        body.expected_version_no,
    ) {
        Ok(f) => ok_value(json!(f)),
        Err(e) => foreshadow_err(e),
    }
}

async fn foreshadow_stats_h(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(work_id): Path<String>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    match state.foreshadow.foreshadow_stats(&work_id) {
        Ok(stats) => ok_value(json!(stats)),
        Err(e) => foreshadow_err(e),
    }
}

async fn get_dependencies_h(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((_work_id, id)): Path<(String, String)>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    match state.foreshadow.get_dependencies(&id) {
        Ok(deps) => ok_value(json!(deps)),
        Err(e) => foreshadow_err(e),
    }
}

async fn get_dependents_h(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((_work_id, id)): Path<(String, String)>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    match state.foreshadow.get_dependents(&id) {
        Ok(deps) => ok_value(json!(deps)),
        Err(e) => foreshadow_err(e),
    }
}

async fn set_dependency_h(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((_work_id, id)): Path<(String, String)>,
    Json(body): Json<AddDependencyBody>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    if let Err(msg) = body.validate() {
        return bad_request("FS_BAD_REQUEST", msg);
    }
    match state.foreshadow.set_dependency(&id, body.parent_id.trim(), body.expected_version_no) {
        Ok(f) => (StatusCode::CREATED, Json(json!(f))).into_response(),
        Err(e) => foreshadow_err(e),
    }
}

async fn remove_dependency_h(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((_work_id, id, parent_id)): Path<(String, String, String)>,
    body: Option<Json<VersionBody>>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let expected = body.map(|b| b.expected_version_no).flatten();
    match state.foreshadow.remove_dependency(&id, &parent_id, expected) {
        Ok(_) => no_content(),
        Err(e) => foreshadow_err(e),
    }
}

async fn delete_foreshadow_h(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((_work_id, id)): Path<(String, String)>,
    body: Option<Json<VersionBody>>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let expected = body.map(|b| b.expected_version_no).flatten();
    match state.foreshadow.delete_foreshadow(&id, expected) {
        Ok(()) => no_content(),
        Err(e) => foreshadow_err(e),
    }
}

async fn add_occurrence_h(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((_work_id, id)): Path<(String, String)>,
    Json(body): Json<AddOccurrenceBody>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    if let Err(msg) = body.validate() {
        return bad_request("FS_BAD_REQUEST", msg);
    }
    match state.foreshadow.add_occurrence(
        &id,
        &body.chapter_id,
        body.typ,
        body.note,
        body.expected_version_no,
    ) {
        Ok(o) => (StatusCode::CREATED, Json(json!(o))).into_response(),
        Err(e) => foreshadow_err(e),
    }
}

async fn remove_occurrence_h(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((_work_id, id, occurrence_id)): Path<(String, String, String)>,
    body: Option<Json<VersionBody>>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let expected = body.map(|b| b.expected_version_no).flatten();
    match state
        .foreshadow
        .remove_occurrence(&id, &occurrence_id, expected)
    {
        Ok(()) => no_content(),
        Err(e) => foreshadow_err(e),
    }
}
