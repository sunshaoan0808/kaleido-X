//! Character relationship graph API (P1).
//!
//! Routes (all require session auth):
//! - `GET    /api/v1/works/{work_id}/graph`                     full graph {characters, relationships}
//! - `POST   /api/v1/works/{work_id}/graph/characters`          create character  -> 201
//! - `PUT    /api/v1/works/{work_id}/graph/characters/{id}`     update character
//! - `DELETE /api/v1/works/{work_id}/graph/characters/{id}`     delete character (cascades edges)
//! - `GET    /api/v1/works/{work_id}/graph/characters/candidates?q=&limit=`  name/alias fuzzy candidates
//! - `POST   /api/v1/works/{work_id}/graph/relationships`       create relationship -> 201
//! - `PUT    /api/v1/works/{work_id}/graph/relationships/{id}`  update relationship
//! - `DELETE /api/v1/works/{work_id}/graph/relationships/{id}`  delete relationship
//!
//! Error mapping: 409 duplicate name, 404 missing entity/endpoint,
//! 400 invalid category, 500 db failure.

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use kaleido_core::graph_store::GraphError;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;

use crate::{session_from, AppState};
use crate::error_codes::*;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/works/{work_id}/graph",
            get(get_graph),
        )
        .route(
            "/api/v1/works/{work_id}/graph/characters",
            post(create_character),
        )
        .route(
            "/api/v1/works/{work_id}/graph/characters/{id}",
            axum::routing::put(update_character).delete(delete_character),
        )
        .route(
            "/api/v1/works/{work_id}/graph/characters/candidates",
            get(character_candidates),
        )
        .route(
            "/api/v1/works/{work_id}/graph/relationships",
            post(create_relationship),
        )
        .route(
            "/api/v1/works/{work_id}/graph/relationships/{id}",
            axum::routing::put(update_relationship).delete(delete_relationship),
        )
}

fn graph_err(e: GraphError) -> Response {
    let (code, msg) = match e {
        GraphError::Db(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")),
        GraphError::DuplicateName => (
            StatusCode::CONFLICT,
            "character name already exists in this work".to_string(),
        ),
        GraphError::InvalidCategory(c) => (
            StatusCode::BAD_REQUEST,
            format!("invalid category '{c}' (family|social|emotional|conflict|uncertain)"),
        ),
        GraphError::NotFound(what) => (StatusCode::NOT_FOUND, format!("{what}")),
        GraphError::BadRequest(m) => (StatusCode::BAD_REQUEST, m),
    };
    let code_s = match code { StatusCode::CONFLICT => "GRAPH_CONFLICT", StatusCode::NOT_FOUND => "GRAPH_NOT_FOUND", StatusCode::BAD_REQUEST => "GRAPH_BAD_REQUEST", _ => "GRAPH_INTERNAL" };
    err_with_code(code, code_s, msg, serde_json::Value::Null)
}

fn ok_value(v: Value) -> Response {
    Json(v).into_response()
}

// ---------------------------------------------------------------------------
// Bodies
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CharacterBody {
    name: String,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default)]
    note: String,
    #[serde(default)]
    color_idx: i64,
}

impl CharacterBody {
    fn validate(&self) -> Result<(), &'static str> {
        if self.name.trim().is_empty() {
            return Err("name must not be empty");
        }
        if self.name.chars().count() > 200 {
            return Err("name too long (max 200 chars)");
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RelationshipBody {
    #[serde(default)]
    from_char: String,
    #[serde(default)]
    to_char: String,
    #[serde(default)]
    category: String,
    #[serde(default)]
    subtype: String,
    #[serde(default)]
    keywords: Vec<String>,
    #[serde(default)]
    confirmation_status: String,
    #[serde(default)]
    note: String,
}

impl RelationshipBody {
    fn validate(&self) -> Result<(), &'static str> {
        if self.subtype.chars().count() > 100 {
            return Err("subtype too long (max 100 chars)");
        }
        if self.note.chars().count() > 4000 {
            return Err("note too long (max 4000 chars)");
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn get_graph(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(work_id): Path<String>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    match state.graph.list(&work_id) {
        Ok((characters, relationships)) => {
            // S9.24: 关系图打通好感度——从最新 tavern session 注入 affinity 到角色节点。
            // 角色名 ↔ charId 桥接：graph 角色(UUID) 与 session affinity(charId) 靠名字匹配。
            let mut name_aff: HashMap<String, i64> = HashMap::new();
            if let Ok(sess_list) = state.sessions_tavern.list() {
                // list() 已按 updatedAt 降序；最多看前 8 个会话，避免全量读盘
                for s in sess_list.iter().take(8) {
                    let sid = s.get("sessionId").and_then(|v| v.as_str()).unwrap_or("");
                    if sid.is_empty() {
                        continue;
                    }
                    if let Ok(sess) = state.sessions_tavern.get(sid) {
                        let aff = sess.memory_l4.affinity;
                        if let Some(aff_obj) = aff.as_object() {
                            if aff_obj.is_empty() {
                                continue;
                            }
                            // 该会话所属 pack 的 charId → name 映射
                            if let Ok(pack) = state.packs.get(&sess.pack_id) {
                                for (char_id, val) in aff_obj {
                                    if let Some(v) = val.as_i64() {
                                        if let Some(ch) = pack
                                            .characters
                                            .iter()
                                            .find(|c| c.id == *char_id)
                                        {
                                            name_aff.entry(ch.name.clone()).or_insert(v);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            let chars_json: Vec<Value> = characters
                .into_iter()
                .map(|c| {
                    let mut j = serde_json::to_value(&c).unwrap_or(Value::Null);
                    if let Some(aff) = name_aff.get(&c.name) {
                        if let Value::Object(ref mut m) = j {
                            m.insert("affinity".into(), json!(aff));
                        }
                    }
                    j
                })
                .collect();
            ok_value(json!({
                "workId": work_id,
                "characters": chars_json,
                "relationships": relationships,
            }))
        }
        Err(e) => graph_err(e),
    }
}

#[derive(Deserialize)]
struct CandidatesQuery {
    #[serde(default)]
    q: String,
    #[serde(default = "default_limit")]
    limit: usize,
}

fn default_limit() -> usize {
    10
}

/// Fuzzy name/alias candidates (no auto-merge; client decides to confirm or reject).
async fn character_candidates(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(work_id): Path<String>,
    Query(query): Query<CandidatesQuery>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let q = query.q.trim().to_lowercase();
    let max = query.limit.min(50).max(1);
    let chars = match state.graph.list(&work_id) {
        Ok((characters, _)) => characters,
        Err(e) => return graph_err(e),
    };
    let mut results: Vec<Value> = Vec::new();
    for c in chars {
        if q.is_empty() {
            continue; // require a non-empty query for candidates
        }
        let hit = c.name.to_lowercase().contains(&q)
            || c.aliases.iter().any(|a| a.to_lowercase().contains(&q));
        if hit {
            results.push(json!({
                "id": c.id,
                "name": c.name,
                "aliases": c.aliases,
                "note": c.note,
            }));
            if results.len() >= max {
                break;
            }
        }
    }
    ok_value(json!({ "candidates": results, "count": results.len() }))
}

async fn create_character(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(work_id): Path<String>,
    Json(body): Json<CharacterBody>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    if let Err(msg) = body.validate() {
        return bad_request("GRAPH_BAD_REQUEST", msg);
    }
    let name = body.name.trim().to_string();
    match state.graph.create_character(&work_id, &name, &body.aliases, &body.note, body.color_idx) {
        Ok(c) => (StatusCode::CREATED, Json(json!(c))).into_response(),
        Err(e) => graph_err(e),
    }
}

async fn update_character(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((_work_id, id)): Path<(String, String)>,
    Json(body): Json<CharacterBody>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    if let Err(msg) = body.validate() {
        return bad_request("GRAPH_BAD_REQUEST", msg);
    }
    let name = body.name.trim().to_string();
    match state.graph.update_character(&id, &name, &body.aliases, &body.note, body.color_idx) {
        Ok(c) => ok_value(json!(c)),
        Err(e) => graph_err(e),
    }
}

async fn delete_character(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((_work_id, id)): Path<(String, String)>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    match state.graph.delete_character(&id) {
        Ok(()) => ok_value(json!({ "deleted": true })),
        Err(e) => graph_err(e),
    }
}

async fn create_relationship(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(work_id): Path<String>,
    Json(body): Json<RelationshipBody>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    if let Err(msg) = body.validate() {
        return bad_request("GRAPH_BAD_REQUEST", msg);
    }
    if body.from_char.is_empty() || body.to_char.is_empty() {
        return bad_request("GRAPH_MISSING_FIELD", "fromChar and toChar are required");
    }
    match state.graph.create_relationship(
        &work_id,
        &body.from_char,
        &body.to_char,
        &body.category,
        &body.subtype,
        &body.keywords,
        &body.confirmation_status,
        &body.note,
    ) {
        Ok(r) => (StatusCode::CREATED, Json(json!(r))).into_response(),
        Err(e) => graph_err(e),
    }
}

async fn update_relationship(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((_work_id, id)): Path<(String, String)>,
    Json(body): Json<RelationshipBody>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    if let Err(msg) = body.validate() {
        return bad_request("GRAPH_BAD_REQUEST", msg);
    }
    match state.graph.update_relationship(
        &id,
        &body.category,
        &body.subtype,
        &body.keywords,
        &body.confirmation_status,
        &body.note,
    ) {
        Ok(r) => ok_value(json!(r)),
        Err(e) => graph_err(e),
    }
}

async fn delete_relationship(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((_work_id, id)): Path<(String, String)>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    match state.graph.delete_relationship(&id) {
        Ok(()) => ok_value(json!({ "deleted": true })),
        Err(e) => graph_err(e),
    }
}
