//! File versions (S5-W3 T2).
//! POST   /api/v1/versions              — create snapshot of a works file
//! GET    /api/v1/versions?path=        — list versions
//! GET    /api/v1/versions/content?...  — read version content
//! DELETE /api/v1/versions?path=&versionId=
//! POST   /api/v1/versions/ai           — attach AI score/suggestion

use axum::{
    extract::{Query, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

use crate::{map_core_err, session_from, AppState};
use crate::error_codes::*;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PathBody {
    pub path: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PathQuery {
    pub path: String,
    #[serde(default)]
    pub version_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiBody {
    pub path: String,
    pub version_id: String,
    #[serde(default)]
    pub score: Option<i64>,
    #[serde(default)]
    pub suggestion: Option<String>,
    #[serde(default)]
    pub analysis: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionInfo {
    pub id: String,
    pub path: String,
    pub created_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default)]
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai_score: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai_suggestion: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai_analysis: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct FileVersionsMetadata {
    pub path: String,
    #[serde(default)]
    pub versions: Vec<VersionInfo>,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/versions", post(create_version).get(list_versions).delete(delete_version))
        .route("/api/v1/versions/content", get(read_version))
        .route("/api/v1/versions/ai", post(update_ai))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn safe_key(rel: &str) -> String {
    let mut out = String::new();
    for ch in rel.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        out.push_str("root");
    }
    // keep key bounded
    if out.len() > 180 {
        out.truncate(180);
    }
    out
}

fn resolve_file(state: &AppState, workspace_id: &str, rel: &str) -> Result<PathBuf, Response> {
    state
        .works
        .resolve(workspace_id, rel, false)
        .map_err(map_core_err)
}

fn versions_root(state: &AppState, workspace_id: &str) -> Result<PathBuf, Response> {
    let root = state
        .works
        .workspace_root(workspace_id)
        .map_err(map_core_err)?;
    let dir = root.join(".kaleido-versions");
    fs::create_dir_all(&dir).map_err(|e| {
        internal("VER_INTERNAL", e.to_string())
    })?;
    Ok(dir)
}

fn meta_path(state: &AppState, workspace_id: &str, rel: &str) -> Result<PathBuf, Response> {
    Ok(versions_root(state, workspace_id)?.join(format!("{}.meta.json", safe_key(rel))))
}

fn version_file_path(
    state: &AppState,
    workspace_id: &str,
    rel: &str,
    version_id: &str,
) -> Result<PathBuf, Response> {
    let dir = versions_root(state, workspace_id)?.join(safe_key(rel));
    fs::create_dir_all(&dir).map_err(|e| {
        internal("VER_INTERNAL", e.to_string())
    })?;
    Ok(dir.join(format!("{version_id}.txt")))
}

fn load_meta(state: &AppState, workspace_id: &str, rel: &str) -> Result<FileVersionsMetadata, Response> {
    let path = meta_path(state, workspace_id, rel)?;
    if !path.exists() {
        return Ok(FileVersionsMetadata {
            path: rel.to_string(),
            versions: vec![],
        });
    }
    let raw = fs::read_to_string(&path).map_err(|e| {
        internal("VER_INTERNAL", e.to_string())
    })?;
    let mut meta: FileVersionsMetadata = serde_json::from_str(&raw).unwrap_or_default();
    if meta.path.is_empty() {
        meta.path = rel.to_string();
    }
    Ok(meta)
}

fn save_meta(
    state: &AppState,
    workspace_id: &str,
    meta: &FileVersionsMetadata,
) -> Result<(), Response> {
    let path = meta_path(state, workspace_id, &meta.path)?;
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let raw = serde_json::to_string_pretty(meta).map_err(|e| {
        internal("VER_INTERNAL", e.to_string())
    })?;
    fs::write(&path, raw).map_err(|e| {
        internal("VER_INTERNAL", e.to_string())
    })
}

async fn create_version(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<PathBody>,
) -> Response {
    let sess = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let rel = body.path.trim();
    if rel.is_empty() {
        return bad_request("VER_MISSING_FIELD", "path required");
    }
    let file = match resolve_file(&state, &sess.workspace_id, rel) {
        Ok(p) => p,
        Err(r) => return r,
    };
    if !file.is_file() {
        return bad_request("VER_BAD_REQUEST", "path is not a file");
    }
    let content = match fs::read_to_string(&file) {
        Ok(c) => c,
        Err(e) => {
            return internal("VER_INTERNAL", e.to_string());
        }
    };
    let id = Uuid::new_v4().to_string();
    let snap = match version_file_path(&state, &sess.workspace_id, rel, &id) {
        Ok(p) => p,
        Err(r) => return r,
    };
    if let Err(e) = fs::write(&snap, &content) {
        return internal("VER_INTERNAL", e.to_string());
    }
    let info = VersionInfo {
        id: id.clone(),
        path: rel.to_string(),
        created_at: now_ms(),
        label: body.label,
        note: body.note,
        size: content.len() as u64,
        ai_score: None,
        ai_suggestion: None,
        ai_analysis: None,
    };
    let mut meta = match load_meta(&state, &sess.workspace_id, rel) {
        Ok(m) => m,
        Err(r) => return r,
    };
    meta.path = rel.to_string();
    meta.versions.insert(0, info.clone());
    // cap history
    if meta.versions.len() > 50 {
        let drop: Vec<_> = meta.versions.drain(50..).collect();
        for v in drop {
            if let Ok(p) = version_file_path(&state, &sess.workspace_id, rel, &v.id) {
                let _ = fs::remove_file(p);
            }
        }
    }
    if let Err(r) = save_meta(&state, &sess.workspace_id, &meta) {
        return r;
    }
    Json(json!({
        "ok": true,
        "version": info,
        "count": meta.versions.len(),
    }))
    .into_response()
}

async fn list_versions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<PathQuery>,
) -> Response {
    let sess = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let rel = q.path.trim();
    if rel.is_empty() {
        return bad_request("VER_MISSING_FIELD", "path required");
    }
    match load_meta(&state, &sess.workspace_id, rel) {
        Ok(meta) => Json(json!({
            "ok": true,
            "path": rel,
            "versions": meta.versions,
            "count": meta.versions.len(),
        }))
        .into_response(),
        Err(r) => r,
    }
}

async fn read_version(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<PathQuery>,
) -> Response {
    let sess = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let rel = q.path.trim();
    let vid = q.version_id.as_deref().unwrap_or("").trim();
    if rel.is_empty() || vid.is_empty() {
        return bad_request("VER_MISSING_FIELD", "path and versionId required");
    }
    let meta = match load_meta(&state, &sess.workspace_id, rel) {
        Ok(m) => m,
        Err(r) => return r,
    };
    if !meta.versions.iter().any(|v| v.id == vid) {
        return not_found("VER_NOT_FOUND", format!("version not found: {vid}"));
    }
    let path = match version_file_path(&state, &sess.workspace_id, rel, vid) {
        Ok(p) => p,
        Err(r) => return r,
    };
    match fs::read_to_string(&path) {
        Ok(content) => Json(json!({
            "ok": true,
            "path": rel,
            "versionId": vid,
            "content": content,
        }))
        .into_response(),
        Err(e) => not_found("VER_NOT_FOUND", e.to_string()),
    }
}

async fn delete_version(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<PathQuery>,
) -> Response {
    let sess = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let rel = q.path.trim();
    let vid = q.version_id.as_deref().unwrap_or("").trim();
    if rel.is_empty() || vid.is_empty() {
        return bad_request("VER_MISSING_FIELD", "path and versionId required");
    }
    let mut meta = match load_meta(&state, &sess.workspace_id, rel) {
        Ok(m) => m,
        Err(r) => return r,
    };
    let before = meta.versions.len();
    meta.versions.retain(|v| v.id != vid);
    if meta.versions.len() == before {
        return not_found("VER_NOT_FOUND", format!("version not found: {vid}"));
    }
    if let Ok(p) = version_file_path(&state, &sess.workspace_id, rel, vid) {
        let _ = fs::remove_file(p);
    }
    if let Err(r) = save_meta(&state, &sess.workspace_id, &meta) {
        return r;
    }
    Json(json!({"ok": true, "path": rel, "versionId": vid})).into_response()
}

async fn update_ai(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<AiBody>,
) -> Response {
    let sess = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let rel = body.path.trim();
    let vid = body.version_id.trim();
    if rel.is_empty() || vid.is_empty() {
        return bad_request("VER_MISSING_FIELD", "path and versionId required");
    }
    let mut meta = match load_meta(&state, &sess.workspace_id, rel) {
        Ok(m) => m,
        Err(r) => return r,
    };
    let Some(v) = meta.versions.iter_mut().find(|v| v.id == vid) else {
        return not_found("VER_NOT_FOUND", format!("version not found: {vid}"));
    };
    if body.score.is_some() {
        v.ai_score = body.score;
    }
    if body.suggestion.is_some() {
        v.ai_suggestion = body.suggestion.clone();
    }
    if body.analysis.is_some() {
        v.ai_analysis = body.analysis.clone();
    }
    let updated = v.clone();
    if let Err(r) = save_meta(&state, &sess.workspace_id, &meta) {
        return r;
    }
    Json(json!({"ok": true, "version": updated})).into_response()
}
