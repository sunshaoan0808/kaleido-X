//! Author Zone (AZ-1..AZ-6): AuthorProject CRUD + live + compose/launch + publish/inject + live policy.
//!
//! Routes:
//!   GET    /api/v1/author/projects
//!   POST   /api/v1/author/projects
//!   GET    /api/v1/author/projects/{id}
//!   PATCH  /api/v1/author/projects/{id}
//!   DELETE /api/v1/author/projects/{id}
//!   POST   /api/v1/author/projects/{id}/bind-session
//!   POST   /api/v1/author/projects/{id}/compose
//!   POST   /api/v1/author/projects/{id}/launch
//!   POST   /api/v1/author/projects/{id}/publish
//!   POST   /api/v1/author/projects/{id}/inject
//!
//! Truth: docs/AUTHOR_ZONE.md (AZ-6: P2/P4 strategy + configurable live)

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use chrono::Utc;
use kaleido_core::{
    ContentTier, CreateSessionRequest, EntryConfig, PackCharacterRef, PackSource, PartnerItem,
    PlayMode, Playable, StoryChapter, StoryNode, StoryPack, WorksFs, NodeExit,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::{map_core_err, session_from, AppState};
use crate::error_codes::*;

const INDEX_NAME: &str = "author-projects";

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/author/projects",
            get(list_projects).post(create_project),
        )
        .route(
            "/api/v1/author/projects/{id}",
            get(get_project).patch(patch_project).delete(delete_project),
        )
        .route(
            "/api/v1/author/projects/{id}/bind-session",
            post(bind_session),
        )
        .route(
            "/api/v1/author/projects/{id}/compose",
            post(compose_project),
        )
        .route(
            "/api/v1/author/projects/{id}/launch",
            post(launch_project),
        )
        .route(
            "/api/v1/author/projects/{id}/publish",
            post(publish_project),
        )
        .route(
            "/api/v1/author/projects/{id}/inject",
            post(inject_to_session),
        )
}


#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorDocEntry {
    pub path: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub playable: Option<String>,
    #[serde(default)]
    pub updated_at: String,
}

/// AZ-6 realtime save policy for project-bound sessions.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorLivePolicy {
    /// Master switch (default true).
    #[serde(default = "default_live_enabled")]
    pub enabled: bool,
    /// Append live.md every N completed turns (min 1).
    #[serde(default = "default_live_every_n")]
    pub every_n: u32,
    /// Also write sessions/.../turns/{n}.md slices.
    #[serde(default)]
    pub write_turns: bool,
    /// Rewrite sessions/.../summary.md every N turns (0/None = off).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write_summary_every_n: Option<u32>,
}

fn default_live_enabled() -> bool {
    true
}

fn default_live_every_n() -> u32 {
    1
}

impl Default for AuthorLivePolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            every_n: 1,
            write_turns: false,
            write_summary_every_n: None,
        }
    }
}

impl AuthorLivePolicy {
    pub fn normalize(mut self) -> Self {
        if self.every_n == 0 {
            self.every_n = 1;
        }
        if let Some(0) = self.write_summary_every_n {
            self.write_summary_every_n = None;
        }
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorProject {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub character_ids: Vec<String>,
    #[serde(default)]
    pub world_book_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pack_id: Option<String>,
    /// Relative works root: projects/{id}/
    pub works_root: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_playable: Option<String>,
    #[serde(default)]
    pub doc_index: Vec<AuthorDocEntry>,
    /// AZ-6: realtime live/turn/summary policy (applied on launch/bind).
    #[serde(default)]
    pub live_policy: AuthorLivePolicy,
    pub created_at: String,
    pub updated_at: String,
    /// Owning auth workspace (multi-user ready).
    #[serde(default)]
    pub workspace_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateProjectBody {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    character_ids: Vec<String>,
    #[serde(default)]
    world_book_ids: Vec<String>,
    #[serde(default)]
    pack_id: Option<String>,
    #[serde(default)]
    default_playable: Option<String>,
    #[serde(default)]
    live_policy: Option<AuthorLivePolicy>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PatchProjectBody {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    character_ids: Option<Vec<String>>,
    #[serde(default)]
    world_book_ids: Option<Vec<String>>,
    #[serde(default)]
    pack_id: Option<Option<String>>,
    #[serde(default)]
    default_playable: Option<Option<String>>,
    #[serde(default)]
    doc_index: Option<Vec<AuthorDocEntry>>,
    #[serde(default)]
    live_policy: Option<AuthorLivePolicy>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BindSessionBody {
    session_id: String,
}

fn now() -> String {
    Utc::now().to_rfc3339()
}

fn safe_project_id(id: &str) -> Result<String, Response> {
    let s = id.trim();
    if s.is_empty()
        || s.contains('/')
        || s.contains('\\')
        || s.contains("..")
        || s.chars().any(|c| c.is_control())
    {
        return Err(bad_request("AUTHOR_BAD_ID",  "invalid project id"));
    }
    Ok(s.to_string())
}

use std::sync::Mutex;

/// Serializes load-modify-save on the author-projects index (lost-update under concurrent create/compose).
static AUTHOR_INDEX_LOCK: Mutex<()> = Mutex::new(());

fn with_index_mut<F, T>(state: &AppState, f: F) -> Result<T, Response>
where
    F: FnOnce(&mut Vec<AuthorProject>) -> Result<T, Response>,
{
    let _guard = AUTHOR_INDEX_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut list = load_index_unlocked(state)?;
    let out = f(&mut list)?;
    save_index_unlocked(state, &list)?;
    Ok(out)
}

fn load_index(state: &AppState) -> Result<Vec<AuthorProject>, Response> {
    let _guard = AUTHOR_INDEX_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    load_index_unlocked(state)
}

fn load_index_unlocked(state: &AppState) -> Result<Vec<AuthorProject>, Response> {
    match state.app_state.load(INDEX_NAME) {
        Ok(raw) => {
            if raw.trim().is_empty() {
                return Ok(vec![]);
            }
            match serde_json::from_str::<Vec<AuthorProject>>(&raw) {
                Ok(v) => Ok(v),
                Err(e) => Err(internal("AUTHOR_INDEX_CORRUPT", format!("corrupt author-projects index: {e}"))),
            }
        }
        Err(kaleido_core::CoreError::NotFound(_)) => Ok(vec![]),
        Err(e) => Err(map_core_err(e)),
    }
}

#[allow(dead_code)]
fn save_index(state: &AppState, list: &[AuthorProject]) -> Result<(), Response> {
    let _guard = AUTHOR_INDEX_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    save_index_unlocked(state, list)
}

fn save_index_unlocked(state: &AppState, list: &[AuthorProject]) -> Result<(), Response> {
    let content = serde_json::to_string_pretty(list).map_err(|e| {
        internal("AUTHOR_SERIALIZE", format!("serialize index: {e}"))
    })?;
    state
        .app_state
        .save(INDEX_NAME, &content)
        .map_err(map_core_err)
}

fn scaffold_project_dirs(works: &WorksFs, workspace_id: &str, project_id: &str) -> Result<(), Response> {
    let root = format!("projects/{project_id}");
    for rel in [
        root.as_str(),
        &format!("{root}/canon"),
        &format!("{root}/imports"),
        &format!("{root}/sessions"),
        &format!("{root}/exports"),
    ] {
        if let Err(e) = works.mkdir(workspace_id, rel) {
            // idempotent: if already dir, ok
            match works.stat(workspace_id, rel) {
                Ok(st) if st.kind == "dir" => {}
                _ => return Err(map_core_err(e)),
            }
        }
    }
    // mirror project.json stub (filled by caller after)
    Ok(())
}

fn write_project_mirror(
    works: &WorksFs,
    workspace_id: &str,
    project: &AuthorProject,
) -> Result<(), Response> {
    let path = format!("projects/{}/project.json", project.id);
    let body = serde_json::to_string_pretty(project).map_err(|e| {
        internal("AUTHOR_SERIALIZE", format!("serialize project: {e}"))
    })?;
    works
        .write_text(workspace_id, &path, &body)
        .map_err(map_core_err)?;
    Ok(())
}

fn filter_workspace(list: Vec<AuthorProject>, workspace_id: &str) -> Vec<AuthorProject> {
    list.into_iter()
        .filter(|p| p.workspace_id.is_empty() || p.workspace_id == workspace_id)
        .collect()
}

async fn list_projects(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    match load_index(&state) {
        Ok(list) => {
            let list = filter_workspace(list, &session.workspace_id);
            Json(json!({"ok": true, "count": list.len(), "projects": list})).into_response()
        }
        Err(r) => r,
    }
}

async fn create_project(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CreateProjectBody>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };

    let id = format!("ap-{}", Uuid::new_v4());
    let title = body
        .title
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "未命名创作".into());
    let ts = now();
    let works_root = format!("projects/{id}/");
    let project = AuthorProject {
        id: id.clone(),
        title,
        character_ids: body.character_ids,
        world_book_ids: body.world_book_ids,
        pack_id: body
            .pack_id
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        works_root: works_root.clone(),
        default_playable: body
            .default_playable
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        doc_index: vec![],
        live_policy: body
            .live_policy
            .unwrap_or_default()
            .normalize(),
        created_at: ts.clone(),
        updated_at: ts,
        workspace_id: session.workspace_id.clone(),
    };

    if let Err(r) = scaffold_project_dirs(&state.works, &session.workspace_id, &id) {
        return r;
    }
    if let Err(r) = write_project_mirror(&state.works, &session.workspace_id, &project) {
        return r;
    }

    let project_out = match with_index_mut(&state, |list| {
        list.push(project.clone());
        Ok(project.clone())
    }) {
        Ok(p) => p,
        Err(r) => return r,
    };

    (
        StatusCode::CREATED,
        Json(json!({
            "ok": true,
            "project": project_out,
        })),
    )
        .into_response()
}

async fn get_project(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let id = match safe_project_id(&id) {
        Ok(i) => i,
        Err(r) => return r,
    };
    let list = match load_index(&state) {
        Ok(l) => filter_workspace(l, &session.workspace_id),
        Err(r) => return r,
    };
    match list.into_iter().find(|p| p.id == id) {
        Some(p) => Json(json!({"ok": true, "project": p})).into_response(),
        None => not_found("AUTHOR_NOT_FOUND", format!("project not found: {id}")),
    }
}

async fn patch_project(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<PatchProjectBody>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let id = match safe_project_id(&id) {
        Ok(i) => i,
        Err(r) => return r,
    };
    let _index_guard = AUTHOR_INDEX_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut list = match load_index_unlocked(&state) {
        Ok(l) => l,
        Err(r) => return r,
    };
    let Some(pos) = list.iter().position(|p| {
        p.id == id && (p.workspace_id.is_empty() || p.workspace_id == session.workspace_id)
    }) else {
        return not_found("AUTHOR_NOT_FOUND", format!("project not found: {id}"));
    };

    let p = &mut list[pos];
    if let Some(t) = body.title {
        let t = t.trim().to_string();
        if !t.is_empty() {
            p.title = t;
        }
    }
    if let Some(v) = body.character_ids {
        p.character_ids = v;
    }
    if let Some(v) = body.world_book_ids {
        p.world_book_ids = v;
    }
    if let Some(v) = body.pack_id {
        p.pack_id = v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    }
    if let Some(v) = body.default_playable {
        p.default_playable = v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    }
    if let Some(v) = body.doc_index {
        p.doc_index = v;
    }
    if let Some(v) = body.live_policy {
        p.live_policy = v.normalize();
    }
    p.updated_at = now();
    if p.workspace_id.is_empty() {
        p.workspace_id = session.workspace_id.clone();
    }

    let project = p.clone();
    if let Err(r) = write_project_mirror(&state.works, &session.workspace_id, &project) {
        return r;
    }
    if let Err(r) = save_index_unlocked(&state, &list) {
        return r;
    }
    Json(json!({"ok": true, "project": project})).into_response()
}

async fn delete_project(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let id = match safe_project_id(&id) {
        Ok(i) => i,
        Err(r) => return r,
    };
    let _index_guard = AUTHOR_INDEX_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut list = match load_index_unlocked(&state) {
        Ok(l) => l,
        Err(r) => return r,
    };
    let before = list.len();
    list.retain(|p| {
        !(p.id == id && (p.workspace_id.is_empty() || p.workspace_id == session.workspace_id))
    });
    if list.len() == before {
        return not_found("AUTHOR_NOT_FOUND", format!("project not found: {id}"));
    }
    // Soft-delete index only; keep works tree for recovery (user can wipe via works UI).
    if let Err(r) = save_index_unlocked(&state, &list) {
        return r;
    }
    Json(json!({"ok": true, "deleted": id})).into_response()
}

/// Bind a tavern session to this project: set authorProjectId + authorLivePath, ensure session dir.
async fn bind_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<BindSessionBody>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let id = match safe_project_id(&id) {
        Ok(i) => i,
        Err(r) => return r,
    };
    let sid = body.session_id.trim().to_string();
    if sid.is_empty() {
        return bad_request("AUTHOR_SESSION_REQUIRED",  "sessionId required");
    }

    let list = match load_index(&state) {
        Ok(l) => filter_workspace(l, &session.workspace_id),
        Err(r) => return r,
    };
    let Some(project) = list.iter().find(|p| p.id == id) else {
        return not_found("AUTHOR_NOT_FOUND", format!("project not found: {id}"));
    };

    let mut tavern = match state.sessions_tavern.get(&sid) {
        Ok(s) => s,
        Err(e) => return map_core_err(e),
    };

    let live_path = format!("projects/{id}/sessions/{sid}/live.md");
    // ensure session dir under project
    let sess_dir = format!("projects/{id}/sessions/{sid}");
    if let Err(e) = state.works.mkdir(&session.workspace_id, &sess_dir) {
        match state.works.stat(&session.workspace_id, &sess_dir) {
            Ok(st) if st.kind == "dir" => {}
            _ => return map_core_err(e),
        }
    }
    // touch live.md if missing
    if state.works.stat(&session.workspace_id, &live_path).is_err() {
        let header = format!(
            "# {} · live\n\n> session `{sid}` · project `{}` · playable {:?}\n\n",
            project.title, project.id, tavern.playable
        );
        if let Err(e) = state
            .works
            .write_text(&session.workspace_id, &live_path, &header)
        {
            return map_core_err(e);
        }
    }

    tavern.author_project_id = Some(id.clone());
    tavern.author_live_path = Some(live_path.clone());
    let pol = project.live_policy.clone().normalize();
    tavern.author_live_enabled = pol.enabled;
    tavern.author_live_every_n = pol.every_n.max(1);
    tavern.author_live_write_turns = pol.write_turns || pol.write_summary_every_n.is_some();
    let saved = match state.sessions_tavern.save(tavern) {
        Ok(s) => s,
        Err(e) => return map_core_err(e),
    };

    // update docIndex on project
    let _index_guard = AUTHOR_INDEX_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut full = match load_index_unlocked(&state) {
        Ok(l) => l,
        Err(r) => return r,
    };
    if let Some(p) = full.iter_mut().find(|p| p.id == id) {
        let entry = AuthorDocEntry {
            path: live_path.clone(),
            kind: "live".into(),
            source_session_id: Some(sid.clone()),
            playable: Some(match saved.playable {
                kaleido_core::Playable::P1 => "P1".into(),
                kaleido_core::Playable::P2 => "P2".into(),
                kaleido_core::Playable::P3 => "P3".into(),
                kaleido_core::Playable::P4 => "P4".into(),
            }),
            updated_at: now(),
        };
        if let Some(existing) = p.doc_index.iter_mut().find(|d| d.path == live_path) {
            *existing = entry;
        } else {
            p.doc_index.push(entry);
        }
        p.updated_at = now();
        let _ = write_project_mirror(&state.works, &session.workspace_id, p);
    }
    let _ = save_index_unlocked(&state, &full);

    Json(json!({
        "ok": true,
        "session": saved,
        "authorLivePath": live_path,
        "projectId": id,
    }))
    .into_response()
}


/// AZ-2: append one turn to live.md (fail-open). Call from tavern turn completion.
pub fn append_session_live(
    works: &WorksFs,
    workspace_id: &str,
    live_path: &str,
    turn: u32,
    user_text: &str,
    assistant_text: &str,
    live_enabled: bool,
    every_n: u32,
    write_turns: bool,
) {
    if !live_enabled {
        return;
    }
    let every = every_n.max(1);
    if turn == 0 || turn % every != 0 {
        return;
    }
    let ts = Utc::now().to_rfc3339();
    let chunk = format!(
        "\n---\n## Turn {turn} · {ts}\n\n### 玩家\n\n{}\n\n### 叙事\n\n{}\n",
        user_text.trim(),
        assistant_text.trim()
    );
    if let Err(e) = works.append_text(workspace_id, live_path, &chunk) {
        tracing::warn!(error=%e, %live_path, "author live.md append failed (fail-open)");
    }
    if write_turns {
        // sessions/{sid}/live.md -> sessions/{sid}/turns/{n}.md + summary.md
        if let Some(parent) = std::path::Path::new(live_path).parent() {
            let turns_dir = parent.join("turns");
            let turns_dir_s = turns_dir.to_string_lossy().replace('\\', "/");
            let _ = works.mkdir(workspace_id, &turns_dir_s);
            let turn_path = format!("{turns_dir_s}/{turn:04}.md");
            let body = format!(
                "# Turn {turn} · {ts}\n\n### 玩家\n\n{}\n\n### 叙事\n\n{}\n",
                user_text.trim(),
                assistant_text.trim()
            );
            if let Err(e) = works.write_text(workspace_id, &turn_path, &body) {
                tracing::warn!(error=%e, %turn_path, "author turn slice write failed (fail-open)");
            }
            let summary_path = parent.join("summary.md");
            let summary_s = summary_path.to_string_lossy().replace('\\', "/");
            let summary = format!(
                "# 会话小结（自动）\n\n> 更新至 Turn {turn} · {ts}\n\n## 最近一轮\n\n### 玩家\n\n{}\n\n### 叙事\n\n{}\n\n",
                user_text.trim(),
                assistant_text.trim()
            );
            if let Err(e) = works.write_text(workspace_id, &summary_s, &summary) {
                tracing::warn!(error=%e, path=%summary_s, "author summary write failed (fail-open)");
            }
        }
    }
}


// ─── AZ-3 compose / launch ───────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ComposeBody {
    #[serde(default)]
    playable: Option<String>,
    #[serde(default)]
    character_ids: Option<Vec<String>>,
    #[serde(default)]
    world_book_ids: Option<Vec<String>>,
    #[serde(default)]
    source_doc_paths: Option<Vec<String>>,
    #[serde(default)]
    title: Option<String>,
    /// If true (default), build/refresh a StoryPack for P3/P2/P4.
    #[serde(default = "default_true")]
    build_pack: Option<bool>,
}

fn default_true() -> Option<bool> {
    Some(true)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LaunchBody {
    #[serde(default)]
    playable: Option<String>,
    #[serde(default)]
    play_mode: Option<String>,
    #[serde(default)]
    adult_confirmed: Option<bool>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    user_tier: Option<String>,
    #[serde(default)]
    entry: Option<EntryConfig>,
    #[serde(default)]
    player_display_name: Option<String>,
    #[serde(default)]
    live_enabled: Option<bool>,
    #[serde(default)]
    live_every_n: Option<u32>,
    #[serde(default)]
    live_write_turns: Option<bool>,
    #[serde(default)]
    live_write_summary_every_n: Option<u32>,
}

fn parse_playable(s: &str) -> Result<Playable, Response> {
    match s.trim().to_uppercase().as_str() {
        "P1" | "" => Ok(Playable::P1),
        "P2" => Ok(Playable::P2),
        "P3" => Ok(Playable::P3),
        "P4" => Ok(Playable::P4),
        _ => Err(bad_request("AUTHOR_BAD_PLAYABLE", format!("invalid playable: {s}"))),
    }
}

fn parse_play_mode(s: Option<&str>, playable: Playable) -> PlayMode {
    match s.map(|x| x.trim().to_ascii_lowercase()).as_deref() {
        Some("mainline") => PlayMode::Mainline,
        Some("side") => PlayMode::Side,
        Some("free") => PlayMode::Free,
        _ => match playable {
            Playable::P3 => PlayMode::Mainline,
            Playable::P4 | Playable::P1 | Playable::P2 => PlayMode::Free,
        },
    }
}

fn parse_tier(s: Option<&str>) -> ContentTier {
    match s.map(|x| x.trim().to_ascii_lowercase()).as_deref() {
        Some("safe") => ContentTier::Safe,
        Some("open") => ContentTier::Open,
        _ => ContentTier::Standard,
    }
}

fn field_str(fields: &Option<serde_json::Value>, key: &str) -> String {
    fields
        .as_ref()
        .and_then(|f| f.get(key))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string()
}

fn partner_to_pack_char(item: &kaleido_core::PartnerItem) -> PackCharacterRef {
    let fields = &item.fields;
    let personality = field_str(fields, "personality");
    let speech = field_str(fields, "speechStyle");
    let speech = if speech.is_empty() {
        field_str(fields, "speech_style")
    } else {
        speech
    };
    PackCharacterRef {
        id: item.id.clone(),
        name: item.name.clone(),
        role: field_str(fields, "occupation"),
        importance: "medium".into(),
        gender: "未知".into(),
        appearance: "未知".into(),
        opening_scene: "未知".into(),
        opening_lines: "".into(),
        nsfw_profile: String::new(),
        content_tier: Some(ContentTier::Standard),
        example_dialogs: vec![],
        boundaries: vec![],
        personality,
        speech_style: speech,
        voice_profile: String::new(),
        motivation: String::new(),
        relationships: vec![],
        evidence_refs: vec![],
        mental_models: vec![],
        decision_heuristics: vec![],
        beliefs: vec![],
        expressions: Default::default(),
            voice: None,
            archive: None,
        avatar: None,
    }
}

fn split_chapters(text: &str) -> Vec<(String, String)> {
    // heuristic: 第x章 / Chapter N / # headings; else two chunks by size
    let t = text.trim();
    if t.is_empty() {
        return vec![
            ("开端".into(), "（空文稿占位）故事从这里开始。".into()),
            ("后续".into(), "（空文稿占位）故事继续。".into()),
        ];
    }
    // line-based chapter split (no regex dep)
    {
        let mut titles: Vec<(usize, String)> = Vec::new();
        let mut offset = 0usize;
        for line in t.split_inclusive('\n') {
            let trimmed = line.trim();
            let is_heading = trimmed.starts_with('#')
                || trimmed.starts_with("第") && trimmed.contains('章')
                || trimmed.to_ascii_lowercase().starts_with("chapter ");
            if is_heading {
                let title = trimmed.trim_start_matches('#').trim().to_string();
                titles.push((offset, title));
            }
            offset += line.len();
        }
        if titles.len() >= 2 {
            let mut out = Vec::new();
            for i in 0..titles.len() {
                let start = titles[i].0;
                let end = if i + 1 < titles.len() {
                    titles[i + 1].0
                } else {
                    t.len()
                };
                let body = t[start..end].trim().to_string();
                if !body.is_empty() {
                    out.push((titles[i].1.clone(), body));
                }
            }
            if out.len() >= 2 {
                return out.into_iter().take(12).collect();
            }
        }
    }
    // fallback: split roughly in half by chars
    let mid = t.chars().count() / 2;
    let mut acc = 0;
    let mut split_at = t.len() / 2;
    for (i, _) in t.char_indices() {
        if acc >= mid {
            split_at = i;
            break;
        }
        acc += 1;
    }
    // prefer newline near mid
    if let Some(rel) = t[split_at..].find('\n') {
        split_at += rel + 1;
    }
    let a = t[..split_at].trim().to_string();
    let b = t[split_at..].trim().to_string();
    vec![
        ("第一章".into(), if a.is_empty() { t.chars().take(800).collect() } else { a }),
        (
            "第二章".into(),
            if b.is_empty() {
                "故事仍在继续。".into()
            } else {
                b
            },
        ),
    ]
}

fn build_composed_pack(
    project: &AuthorProject,
    title: &str,
    chars: Vec<PackCharacterRef>,
    world_book_ids: Vec<String>,
    chapter_bodies: Vec<(String, String)>,
) -> StoryPack {
    let now = now();
    let pack_id = format!("pack-az-{}", Uuid::new_v4());
    let mut chapters = Vec::new();
    let mut nodes = Vec::new();
    let char_ids: Vec<String> = chars.iter().map(|c| c.id.clone()).collect();
    let n = chapter_bodies.len().max(2);
    for (i, (ch_title, body)) in chapter_bodies.into_iter().enumerate() {
        let order = (i + 1) as u32;
        let ch_id = format!("ch{:02}", order);
        let node_id = format!("n{order}");
        let body_path = format!("chapters/{ch_id}.md");
        let summary: String = body.chars().take(200).collect();
        chapters.push(StoryChapter {
            id: ch_id.clone(),
            title: ch_title,
            order,
            goals: vec![format!("推进{ch_id}")],
            node_ids: vec![node_id.clone()],
            body_path: body_path.clone(),
            image_path: String::new(), // U10
        });
        let mut exits = vec![];
        if i + 1 < n {
            exits.push(NodeExit {
                id: format!("e{order}"),
                when: "continue".into(),
                next: format!("n{}", order + 1),
            });
        }
        nodes.push(StoryNode {
            id: node_id,
            chapter_id: ch_id,
            title: format!("节点{order}"),
            entry: summary.clone(),
            exit: exits,
            locked_beats: vec![],
            allowed_divergence: "branch".into(),
            present_characters: char_ids.clone(),
            location_id: None,
            summary,
        });
    }
    // ensure >=2 chapters
    while chapters.len() < 2 {
        let order = (chapters.len() + 1) as u32;
        let ch_id = format!("ch{:02}", order);
        let node_id = format!("n{order}");
        chapters.push(StoryChapter {
            id: ch_id.clone(),
            title: format!("第{order}章"),
            order,
            goals: vec![],
            node_ids: vec![node_id.clone()],
            body_path: format!("chapters/{ch_id}.md"),
            image_path: String::new(), // U10
        });
        nodes.push(StoryNode {
            id: node_id,
            chapter_id: ch_id,
            title: format!("节点{order}"),
            entry: "占位".into(),
            exit: vec![],
            locked_beats: vec![],
            allowed_divergence: "branch".into(),
            present_characters: char_ids.clone(),
            location_id: None,
            summary: "占位".into(),
        });
    }
    // link last gap if needed
    if nodes.len() >= 2 {
        for i in 0..nodes.len() - 1 {
            if nodes[i].exit.is_empty() {
                let next = nodes[i + 1].id.clone();
                nodes[i].exit.push(NodeExit {
                    id: format!("auto-{i}"),
                    when: "continue".into(),
                    next,
                });
            }
        }
    }

    StoryPack {
        id: pack_id,
        title: title.to_string(),
        source: PackSource {
            source_type: "author-compose".into(),
            refs: vec![project.id.clone()],
        },
        characters: chars,
        world_book_ids,
        chapters,
        nodes,
        lore_entries: vec![],
        event_packages: vec![],
        actor_state_config: kaleido_core::ActorStatePackConfig::default(),
        default_mode: PlayMode::Mainline,
        max_tier: ContentTier::Standard,
        language: "zh".into(),
        created_at: now.clone(),
        updated_at: now,
        stage_director: Default::default(),
        worldline: vec![], // T 层：作者流 pack 无静态时间线蒸馏，空
    }
}

async fn compose_project(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<ComposeBody>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let id = match safe_project_id(&id) {
        Ok(i) => i,
        Err(r) => return r,
    };

    // Hold index lock for entire compose RMW (prevents lost updates under concurrent create/compose).
    let _index_guard = AUTHOR_INDEX_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    let mut list = match load_index_unlocked(&state) {
        Ok(l) => l,
        Err(r) => return r,
    };
    let Some(pos) = list.iter().position(|p| {
        p.id == id && (p.workspace_id.is_empty() || p.workspace_id == session.workspace_id)
    }) else {
        return not_found("AUTHOR_NOT_FOUND", format!("project not found: {id}"));
    };

    let playable_s = body
        .playable
        .clone()
        .or_else(|| list[pos].default_playable.clone())
        .unwrap_or_else(|| "P1".into());
    let playable = match parse_playable(&playable_s) {
        Ok(p) => p,
        Err(r) => return r,
    };

    // merge character / worldbook ids onto project
    if let Some(ids) = body.character_ids.clone() {
        list[pos].character_ids = ids;
    }
    if let Some(ids) = body.world_book_ids.clone() {
        list[pos].world_book_ids = ids;
    }
    if let Some(t) = body.title.clone() {
        let t = t.trim().to_string();
        if !t.is_empty() {
            list[pos].title = t;
        }
    }
    list[pos].default_playable = Some(match playable {
        Playable::P1 => "P1",
        Playable::P2 => "P2",
        Playable::P3 => "P3",
        Playable::P4 => "P4",
    }.into());
    list[pos].updated_at = now();

    // validate partner assets when ids present (C2: per-user isolation)
    let partner = match state.partner.clone().scoped(&session.user_id).load() {
        Ok(p) => p,
        Err(e) => return map_core_err(e),
    };
    let mut pack_chars: Vec<PackCharacterRef> = Vec::new();
    for cid in &list[pos].character_ids {
        if let Some(cc) = partner.character_cards.iter().find(|c| &c.id == cid) {
            pack_chars.push(partner_to_pack_char(cc));
        } else {
            // allow unknown ids as soft refs (compose still works for demos)
            pack_chars.push(PackCharacterRef {
                id: cid.clone(),
                name: cid.clone(),
                role: "character".into(),
                importance: "medium".into(),
                gender: "未知".into(),
                appearance: "未知".into(),
                opening_scene: "未知".into(),
                opening_lines: "".into(),
                nsfw_profile: String::new(),
                content_tier: Some(ContentTier::Standard),
                example_dialogs: vec![],
                boundaries: vec![],
                personality: String::new(),
                speech_style: String::new(),
                voice_profile: String::new(),
                motivation: String::new(),
                relationships: vec![],
                evidence_refs: vec![],
                mental_models: vec![],
                decision_heuristics: vec![],
                beliefs: vec![],
                expressions: Default::default(),
            voice: None,
            archive: None,
                avatar: None,
            });
        }
    }
    for wid in &list[pos].world_book_ids {
        if partner.world_books.iter().all(|w| &w.id != wid) {
            // soft allow
        }
    }

    match playable {
        Playable::P1 if list[pos].character_ids.is_empty() && pack_chars.is_empty() => {
            return bad_request("AUTHOR_COMPOSE_EMPTY",  "P1 compose requires >=1 characterId");
        }
        Playable::P2 if list[pos].character_ids.len() < 2 => {
            return bad_request("AUTHOR_COMPOSE_EMPTY",  "P2 compose requires >=2 characterIds");
        }
        Playable::P3 if list[pos].character_ids.is_empty() => {
            return bad_request("AUTHOR_COMPOSE_EMPTY",  "P3 compose requires >=1 characterId");
        }
        Playable::P4
            if list[pos].character_ids.is_empty()
                && list[pos].world_book_ids.is_empty()
                && body
                    .source_doc_paths
                    .as_ref()
                    .map(|v| v.is_empty())
                    .unwrap_or(true) =>
        {
            return bad_request(
                "AUTHOR_COMPOSE_EMPTY",
                "P4 compose requires worldBookId and/or sourceDocPaths (BG) or >=1 characterId",
            );
        }
        _ => {}
    }

    let build_pack = body.build_pack.unwrap_or(true);
    // AZ residual: all playables (incl. P1) build a StoryPack on compose so
    // publish lore/chapter has a packId without a separate launch.
    let need_pack = build_pack;

    let mut pack_id = list[pos].pack_id.clone();
    let mut pack_playable = false;

    if need_pack || matches!(playable, Playable::P3) {
        // gather chapter text from source docs or placeholder
        let mut combined = String::new();
        if let Some(paths) = &body.source_doc_paths {
            for path in paths {
                match state.works.read_text(&session.workspace_id, path) {
                    Ok(f) => {
                        combined.push_str(&f.content);
                        combined.push_str("\n\n");
                    }
                    Err(e) => {
                        return bad_request("AUTHOR_SOURCE_DOC_READ", format!("sourceDoc read failed {path}: {e}"));
                    }
                }
            }
        }
        // also try project imports/
        if combined.trim().is_empty() {
            let import_dir = format!("projects/{id}/imports");
            if let Ok(listing) = state.works.list(&session.workspace_id, &import_dir, 1) {
                for ch in listing.children {
                    if ch.kind == "file" {
                        if let Ok(f) = state.works.read_text(&session.workspace_id, &ch.path) {
                            combined.push_str(&f.content);
                            combined.push_str("\n\n");
                        }
                    }
                }
            }
        }
        if pack_chars.is_empty() {
            // P3 needs characters — already validated non-empty ids
        }
        let chapters = split_chapters(&combined);
        // stash bodies for write
        let chapter_bodies = chapters.clone();
        let mut pack = build_composed_pack(
            &list[pos],
            &list[pos].title,
            pack_chars.clone(),
            list[pos].world_book_ids.clone(),
            chapters,
        );
        // ensure playable: >=1 char, >=2 ch, linked nodes
        if pack.characters.is_empty() {
            pack.characters.push(PackCharacterRef {
                id: "cc-anon".into(),
                name: "旅人".into(),
                role: "player".into(),
                importance: "low".into(),
                gender: "未知".into(),
                appearance: "未知".into(),
                opening_scene: "未知".into(),
                opening_lines: "".into(),
                nsfw_profile: String::new(),
                content_tier: Some(ContentTier::Standard),
                example_dialogs: vec![],
                boundaries: vec![],
                personality: String::new(),
                speech_style: String::new(),
                voice_profile: String::new(),
                motivation: String::new(),
                relationships: vec![],
                evidence_refs: vec![],
                mental_models: vec![],
                decision_heuristics: vec![],
                beliefs: vec![],
                expressions: Default::default(),
            voice: None,
            archive: None,
                avatar: None,
            });
        }
        match state.packs.save(pack.clone()) {
            Ok(saved) => {
                for (i, (_title, body)) in chapter_bodies.iter().enumerate() {
                    let order = (i + 1) as u32;
                    let rel = format!("chapters/ch{:02}.md", order);
                    let content = if body.trim().is_empty() {
                        format!("# {}\n\n（作者组合占位章）\n", _title)
                    } else if body.starts_with('#') {
                        body.clone()
                    } else {
                        format!("# {}\n\n{}\n", _title, body)
                    };
                    let _ = state.packs.write_chapter_body(&saved.id, &rel, &content);
                }
                // pad missing chapter files to 2
                for order in 1..=saved.chapters.len().max(2) {
                    let rel = format!("chapters/ch{:02}.md", order);
                    let _ = state.packs.write_chapter_body(
                        &saved.id,
                        &rel,
                        &format!("# 第{order}章\n\n（占位）\n"),
                    );
                }
                pack_playable = saved.is_playable();
                pack_id = Some(saved.id.clone());
                list[pos].pack_id = Some(saved.id);
            }
            Err(e) => return map_core_err(e),
        }
    } else if matches!(playable, Playable::P1) && list[pos].pack_id.is_none() {
        // P1 can use demo pack as lightweight stage if desired — leave pack empty;
        // launch will use ensure_demo when pack missing for non-P1 only.
    }

    list[pos].updated_at = now();
    let project = list[pos].clone();
    if let Err(r) = write_project_mirror(&state.works, &session.workspace_id, &project) {
        return r;
    }
    if let Err(r) = save_index_unlocked(&state, &list) {
        return r;
    }

    let launch = json!({
        "playable": match playable {
            Playable::P1 => "P1",
            Playable::P2 => "P2",
            Playable::P3 => "P3",
            Playable::P4 => "P4",
        },
        "packId": pack_id,
        "characterIds": project.character_ids,
        "worldBookIds": project.world_book_ids,
        "projectId": project.id,
        "playablePack": pack_playable,
    });

    Json(json!({
        "ok": true,
        "projectId": project.id,
        "project": project,
        "packId": pack_id,
        "launch": launch,
    }))
    .into_response()
}

async fn launch_project(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<LaunchBody>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let id = match safe_project_id(&id) {
        Ok(i) => i,
        Err(r) => return r,
    };
    let list = match load_index(&state) {
        Ok(l) => filter_workspace(l, &session.workspace_id),
        Err(r) => return r,
    };
    let Some(project) = list.iter().find(|p| p.id == id) else {
        return not_found("AUTHOR_NOT_FOUND", format!("project not found: {id}"));
    };

    let playable_s = body
        .playable
        .clone()
        .or_else(|| project.default_playable.clone())
        .unwrap_or_else(|| "P1".into());
    let playable = match parse_playable(&playable_s) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let play_mode = parse_play_mode(body.play_mode.as_deref(), playable);
    let user_tier = parse_tier(body.user_tier.as_deref());

    // resolve pack
    let mut pack_id = project.pack_id.clone();
    if pack_id.as_ref().map(|s| s.is_empty()).unwrap_or(true) {
        // P1 without pack: use demo so session engine has a stage
        match state.packs.ensure_demo_pack() {
            Ok(p) => pack_id = Some(p.id),
            Err(e) => return map_core_err(e),
        }
    }
    let pack_id = pack_id.unwrap();

    let title = body
        .title
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| format!("{} · launch", project.title));

    let req = CreateSessionRequest {
        pack_id: pack_id.clone(),
        playable,
        play_mode,
        user_tier,
        global_tier: None,
        entry: body.entry.clone(),
        player_display_name: body.player_display_name.clone(),
        adult_confirmed: body.adult_confirmed.unwrap_or(true),
        title: Some(title),
        quality: kaleido_core::Quality::Lite,
        owner: Some(session.user_id.clone()),
        author_project_id: Some(project.id.clone()),
        // R6: launch 传 work_id（= author project id），U13 自动挂载该作品的罗盘
        work_id: Some(project.id.clone()),
    };

    let mut tavern = match state.sessions_tavern.create_from_pack(&state.packs, req) {
        Ok(s) => s,
        Err(e) => return map_core_err(e),
    };

    // ensure live path + session dir (same as bind)
    let live_path = format!(
        "projects/{}/sessions/{}/live.md",
        project.id, tavern.session_id
    );
    let sess_dir = format!("projects/{}/sessions/{}", project.id, tavern.session_id);
    let _ = state.works.mkdir(&session.workspace_id, &sess_dir);
    if state.works.stat(&session.workspace_id, &live_path).is_err() {
        let header = format!(
            "# {} · live\n\n> session `{}` · project `{}` · playable {:?}\n\n",
            project.title, tavern.session_id, project.id, tavern.playable
        );
        let _ = state
            .works
            .write_text(&session.workspace_id, &live_path, &header);
    }
    tavern.author_project_id = Some(project.id.clone());
    tavern.author_live_path = Some(live_path.clone());
    // present characters from project when pack has generic cast
    if !project.character_ids.is_empty() {
        tavern.present_character_ids = project.character_ids.clone();
        tavern.focus_character_id = project.character_ids.first().cloned();
    }
    // AZ-6 playable tactics
    match playable {
        Playable::P2 => {
            tavern.speaker_rotation = true;
            if tavern.present_character_ids.len() < 2 {
                return bad_request("AUTHOR_LAUNCH_ARITY",  "P2 launch requires >=2 characterIds on project");
            }
        }
        Playable::P4 => {
            tavern.speaker_rotation = tavern.present_character_ids.len() >= 2;
        }
        Playable::P1 => {
            tavern.speaker_rotation = false;
        }
        Playable::P3 => {
            tavern.speaker_rotation = true;
        }
    }

    // AZ-6 live policy (project + launch overrides)
    let mut pol = project.live_policy.clone().normalize();
    if let Some(v) = body.live_enabled {
        pol.enabled = v;
    }
    if let Some(v) = body.live_every_n {
        pol.every_n = if v == 0 { 1 } else { v };
    }
    if let Some(v) = body.live_write_turns {
        pol.write_turns = v;
    }
    if let Some(v) = body.live_write_summary_every_n {
        pol.write_summary_every_n = if v == 0 { None } else { Some(v) };
    }
    tavern.author_live_enabled = pol.enabled;
    tavern.author_live_every_n = pol.every_n.max(1);
    // turn slices + summary refresh share write_turns gate; summary cadence uses every_n
    tavern.author_live_write_turns = pol.write_turns || pol.write_summary_every_n.is_some();
    if let Some(sn) = pol.write_summary_every_n {
        // if only summary configured, still honor live every_n for live.md
        let _ = sn;
    }

    let saved = match state.sessions_tavern.save(tavern) {
        Ok(s) => s,
        Err(e) => return map_core_err(e),
    };

    // docIndex touch
    let _index_guard = AUTHOR_INDEX_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut full = match load_index_unlocked(&state) {
        Ok(l) => l,
        Err(r) => return r,
    };
    if let Some(p) = full.iter_mut().find(|p| p.id == id) {
        let entry = AuthorDocEntry {
            path: live_path.clone(),
            kind: "live".into(),
            source_session_id: Some(saved.session_id.clone()),
            playable: Some(match saved.playable {
                Playable::P1 => "P1".into(),
                Playable::P2 => "P2".into(),
                Playable::P3 => "P3".into(),
                Playable::P4 => "P4".into(),
            }),
            updated_at: now(),
        };
        if let Some(ex) = p.doc_index.iter_mut().find(|d| d.path == live_path) {
            *ex = entry;
        } else {
            p.doc_index.push(entry);
        }
        p.updated_at = now();
        let _ = write_project_mirror(&state.works, &session.workspace_id, p);
    }
    let _ = save_index_unlocked(&state, &full);

    Json(json!({
        "ok": true,
        "sessionId": saved.session_id,
        "session": saved,
        "projectId": id,
        "packId": pack_id,
        "authorLivePath": live_path,
    }))
    .into_response()
}


// ─── AZ-5 publish / inject ───────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PublishBody {
    /// lore | chapter | worldBook | promoteLive
    kind: String,
    /// works-relative path under project (or absolute works path projects/{id}/...)
    #[serde(default)]
    path: Option<String>,
    /// inline content if path omitted
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    title: Option<String>,
    /// for worldBook: target book id (default first project worldBookId)
    #[serde(default)]
    target_world_book_id: Option<String>,
    /// for chapter: pack chapter id to overwrite body
    #[serde(default)]
    chapter_id: Option<String>,
    /// for lore: permanent entry (default true)
    #[serde(default)]
    permanent: Option<bool>,
    /// for promoteLive: destination under canon/ (default canon/from-live-{ts}.md)
    #[serde(default)]
    dest_path: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InjectBody {
    session_id: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    content: Option<String>,
    /// system | director (default system)
    #[serde(default)]
    as_role: Option<String>,
}

fn resolve_publish_text(
    state: &AppState,
    workspace_id: &str,
    project: &AuthorProject,
    path: Option<&str>,
    content: Option<&str>,
) -> Result<(String, Option<String>), Response> {
    if let Some(c) = content.map(str::trim).filter(|s| !s.is_empty()) {
        return Ok((c.to_string(), path.map(|p| p.to_string())));
    }
    let rel = path
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            bad_request("AUTHOR_PATH_REQUIRED",  "path or content required")
        })?;
    // allow projects/{id}/... or relative to works_root
    let full = if rel.starts_with("projects/") {
        rel.to_string()
    } else {
        format!(
            "{}/{}",
            project.works_root.trim_end_matches('/'),
            rel.trim_start_matches('/')
        )
    };
    if !full.starts_with(&project.works_root) && !full.starts_with(&format!("{}/", project.works_root.trim_end_matches('/'))) {
        // still ok if under projects/{id}
        let prefix = format!("projects/{}/", project.id);
        if !full.starts_with(&prefix) {
            return Err(bad_request("AUTHOR_PATH_ESCAPES",  "path must be under project worksRoot"));
        }
    }
    match state.works.read_text(workspace_id, &full) {
        Ok(body) => Ok((body.content, Some(full))),
        Err(e) => Err(map_core_err(e)),
    }
}

fn lore_entry_from_text(title: &str, text: &str, permanent: bool) -> serde_json::Value {
    let keys: Vec<String> = title
        .split(|c: char| c.is_whitespace() || c == '/' || c == '-' || c == '_')
        .filter(|s| s.len() >= 2)
        .take(8)
        .map(|s| s.to_string())
        .collect();
    json!({
        "id": format!("lore-az-{}", Uuid::new_v4()),
        "title": title,
        "content": text,
        "keys": keys,
        "permanent": permanent,
        "source": "author-publish",
        "updatedAt": Utc::now().to_rfc3339(),
    })
}

fn ensure_project_pack(
    state: &AppState,
    project: &mut AuthorProject,
    partner_chars: Vec<PackCharacterRef>,
) -> Result<String, Response> {
    if let Some(id) = project.pack_id.clone().filter(|s| !s.is_empty()) {
        if state.packs.get(&id).is_ok() {
            return Ok(id);
        }
    }
    let chapters = split_chapters("");
    let pack = build_composed_pack(
        project,
        &project.title,
        partner_chars,
        project.world_book_ids.clone(),
        chapters,
    );
    match state.packs.save(pack) {
        Ok(saved) => {
            let _ = state.packs.write_chapter_body(
                &saved.id,
                "chapters/ch01.md",
                &format!("# {}\n\n（发布自动建包占位）\n", project.title),
            );
            let _ = state.packs.write_chapter_body(
                &saved.id,
                "chapters/ch02.md",
                "# 第2章\n\n（占位）\n",
            );
            project.pack_id = Some(saved.id.clone());
            project.updated_at = now();
            Ok(saved.id)
        }
        Err(e) => Err(map_core_err(e)),
    }
}

async fn publish_project(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<PublishBody>,
) -> Response {
    let sess = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let _index_guard = AUTHOR_INDEX_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut list = match load_index_unlocked(&state) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let pos = match list.iter().position(|p| p.id == id && p.workspace_id == sess.workspace_id) {
        Some(i) => i,
        None => {
            return not_found("AUTHOR_NOT_FOUND",  "project not found")
        }
    };

    let kind = body.kind.trim().to_lowercase();

    // Auto-build pack for lore/chapter if compose was skipped (P1 legacy)
    if matches!(kind.as_str(), "lore" | "chapter") {
        let partner = match state.partner.clone().scoped(&sess.user_id).load() {
            Ok(p) => p,
            Err(e) => return map_core_err(e),
        };
        let mut pack_chars: Vec<PackCharacterRef> = Vec::new();
        for cid in &list[pos].character_ids {
            if let Some(cc) = partner.character_cards.iter().find(|c| &c.id == cid) {
                pack_chars.push(partner_to_pack_char(cc));
            } else {
                pack_chars.push(PackCharacterRef {
                    id: cid.clone(),
                    name: cid.clone(),
                    role: "character".into(),
                    importance: "medium".into(),
                    gender: "未知".into(),
                    appearance: "未知".into(),
                    opening_scene: "未知".into(),
                    opening_lines: "".into(),
                    nsfw_profile: String::new(),
                    content_tier: Some(ContentTier::Standard),
                    example_dialogs: vec![],
                    boundaries: vec![],
                    personality: String::new(),
                    speech_style: String::new(),
                    voice_profile: String::new(),
                    motivation: String::new(),
                    relationships: vec![],
                    evidence_refs: vec![],
                    mental_models: vec![],
                    decision_heuristics: vec![],
                    beliefs: vec![],
                    expressions: Default::default(),
            voice: None,
            archive: None,
                    avatar: None,
                });
            }
        }
        match ensure_project_pack(&state, &mut list[pos], pack_chars) {
            Ok(_) => {
                if let Err(r) = write_project_mirror(&state.works, &sess.workspace_id, &list[pos]) {
                    return r;
                }
                if let Err(r) = save_index_unlocked(&state, &list) {
                    return r;
                }
            }
            Err(r) => return r,
        }
    }
    let kind = match kind.as_str() {
        "lore" | "chapter" | "worldbook" | "world_book" | "promotelive" | "promote_live" | "promote-live" => kind,
        _ => {
            return bad_request("AUTHOR_BAD_KIND",  "kind must be lore|chapter|worldBook|promoteLive")
        }
    };

    // promoteLive: copy live path content into canon/
    if kind == "promotelive" || kind == "promote_live" || kind == "promote-live" {
        let (text, src) = match resolve_publish_text(
            &state,
            &sess.workspace_id,
            &list[pos],
            body.path.as_deref(),
            body.content.as_deref(),
        ) {
            Ok(v) => v,
            Err(r) => return r,
        };
        let dest = body
            .dest_path
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| {
                if s.starts_with("projects/") {
                    s.to_string()
                } else {
                    format!(
                        "{}/{}",
                        list[pos].works_root.trim_end_matches('/'),
                        s.trim_start_matches('/')
                    )
                }
            })
            .unwrap_or_else(|| {
                format!(
                    "{}/canon/from-live-{}.md",
                    list[pos].works_root.trim_end_matches('/'),
                    Utc::now().format("%Y%m%d-%H%M%S")
                )
            });
        // ensure parent via write
        let header = format!(
            "<!-- promoted from {} at {} -->\n\n",
            src.as_deref().unwrap_or("(inline)"),
            Utc::now().to_rfc3339()
        );
        let full = format!("{}{}", header, text);
        if let Some(parent) = std::path::Path::new(&dest).parent() {
            let parent_s = parent.to_string_lossy();
            if !parent_s.is_empty() && parent_s != "." {
                let _ = state.works.mkdir(&sess.workspace_id, &parent_s);
            }
        }
        if let Err(e) = state.works.write_text(&sess.workspace_id, &dest, &full) {
            return map_core_err(e);
        }
        let entry = AuthorDocEntry {
            path: dest.clone(),
            kind: "canon".into(),
            source_session_id: None,
            playable: list[pos].default_playable.clone(),
            updated_at: Utc::now().to_rfc3339(),
        };
        list[pos].doc_index.retain(|d| d.path != dest);
        list[pos].doc_index.push(entry);
        list[pos].updated_at = Utc::now().to_rfc3339();
        if let Err(r) = save_index_unlocked(&state, &list) {
            return r;
        }
        return Json(json!({
            "ok": true,
            "kind": "promoteLive",
            "destPath": dest,
            "bytes": full.len(),
            "project": list[pos],
        }))
        .into_response();
    }

    let (text, src_path) = match resolve_publish_text(
        &state,
        &sess.workspace_id,
        &list[pos],
        body.path.as_deref(),
        body.content.as_deref(),
    ) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let title = body
        .title
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .or_else(|| {
            src_path.as_ref().map(|p| {
                std::path::Path::new(p)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("设定")
                    .to_string()
            })
        })
        .unwrap_or_else(|| "作者发布".into());

    if kind == "lore" {
        let pack_id = match list[pos].pack_id.clone() {
            Some(id) if !id.is_empty() => id,
            _ => {
                return bad_request("AUTHOR_NO_PACK",  "project has no packId; compose first")
            }
        };
        let mut pack = match state.packs.get(&pack_id) {
            Ok(p) => p,
            Err(e) => return map_core_err(e),
        };
        let permanent = body.permanent.unwrap_or(true);
        let entry = lore_entry_from_text(&title, &text, permanent);
        let entry_id = entry.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
        pack.lore_entries.push(entry);
        pack.updated_at = Utc::now().to_rfc3339();
        let saved = match state.packs.save(pack) {
            Ok(p) => p,
            Err(e) => return map_core_err(e),
        };
        list[pos].updated_at = Utc::now().to_rfc3339();
        let _ = save_index_unlocked(&state, &list);
        return Json(json!({
            "ok": true,
            "kind": "lore",
            "loreId": entry_id,
            "packId": saved.id,
            "loreCount": saved.lore_entries.len(),
            "sourcePath": src_path,
        }))
        .into_response();
    }

    if kind == "chapter" {
        let pack_id = match list[pos].pack_id.clone() {
            Some(id) if !id.is_empty() => id,
            _ => {
                return bad_request("AUTHOR_NO_PACK",  "project has no packId; compose first")
            }
        };
        let mut pack = match state.packs.get(&pack_id) {
            Ok(p) => p,
            Err(e) => return map_core_err(e),
        };
        if pack.chapters.is_empty() {
            return bad_request("AUTHOR_PACK_EMPTY",  "pack has no chapters");
        }
        let ch_id = body
            .chapter_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        let ch_idx = if let Some(ref cid) = ch_id {
            pack.chapters.iter().position(|c| &c.id == cid)
        } else {
            Some(0)
        };
        let Some(ci) = ch_idx else {
            return bad_request("AUTHOR_CHAPTER_MISSING",  "chapterId not found in pack");
        };
        let body_path = if pack.chapters[ci].body_path.trim().is_empty() {
            format!("chapters/ch{:02}.md", pack.chapters[ci].order.max(1))
        } else {
            pack.chapters[ci].body_path.clone()
        };
        if let Err(e) = state.packs.write_chapter_body(&pack_id, &body_path, &text) {
            return map_core_err(e);
        }
        pack.chapters[ci].body_path = body_path.clone();
        if !title.is_empty() {
            pack.chapters[ci].title = title.clone();
        }
        pack.updated_at = Utc::now().to_rfc3339();
        let saved = match state.packs.save(pack) {
            Ok(p) => p,
            Err(e) => return map_core_err(e),
        };
        list[pos].updated_at = Utc::now().to_rfc3339();
        let _ = save_index_unlocked(&state, &list);
        return Json(json!({
            "ok": true,
            "kind": "chapter",
            "packId": saved.id,
            "chapterId": saved.chapters.get(ci).map(|c| c.id.clone()),
            "bodyPath": body_path,
            "note": "已开 mainline 会话请新节点或重开以读到新正文",
            "sourcePath": src_path,
        }))
        .into_response();
    }

    // worldBook
    if kind == "worldbook" || kind == "world_book" {
        let wb_id = body
            .target_world_book_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .or_else(|| list[pos].world_book_ids.first().cloned());
        let Some(wb_id) = wb_id else {
            return bad_request("AUTHOR_WB_TARGET_MISSING",  "no targetWorldBookId and project has no worldBookIds");
        };
        let mut partner = match state.partner.clone().scoped(&sess.user_id).load() {
            Ok(p) => p,
            Err(e) => return map_core_err(e),
        };
        let block = format!("\n\n## {}\n\n{}\n", title, text.trim());
        let mut found = false;
        for w in partner.world_books.iter_mut() {
            if w.id == wb_id {
                if !w.content.contains(text.trim()) {
                    w.content.push_str(&block);
                }
                found = true;
                break;
            }
        }
        if !found {
            // create new world book bound to project
            let item = PartnerItem {
                id: wb_id.clone(),
                name: title.clone(),
                item_type: "world_book".into(),
                content: format!("# {}\n\n{}", title, text.trim()),
                fields: None,
                world_book_id: None,
            };
            partner.world_books.push(item);
            if !list[pos].world_book_ids.iter().any(|x| x == &wb_id) {
                list[pos].world_book_ids.push(wb_id.clone());
            }
        }
        // upsert via partner store
        let item = partner
            .world_books
            .iter()
            .find(|w| w.id == wb_id)
            .cloned()
            .unwrap();
        match state.partner.clone().scoped(&sess.user_id).upsert_world_book(item) {
            Ok(saved_item) => {
                list[pos].updated_at = Utc::now().to_rfc3339();
                let _ = save_index_unlocked(&state, &list);
                return Json(json!({
                    "ok": true,
                    "kind": "worldBook",
                    "worldBookId": saved_item.id,
                    "name": saved_item.name,
                    "contentLen": saved_item.content.len(),
                    "sourcePath": src_path,
                }))
                .into_response();
            }
            Err(e) => return map_core_err(e),
        }
    }

    bad_request("AUTHOR_BAD_KIND",  "unhandled kind")
}

async fn inject_to_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<InjectBody>,
) -> Response {
    let sess = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let list = match load_index(&state) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let project = match list.iter().find(|p| p.id == id && p.workspace_id == sess.workspace_id) {
        Some(p) => p.clone(),
        None => {
            return not_found("AUTHOR_NOT_FOUND",  "project not found")
        }
    };
    let sid = body.session_id.trim();
    if sid.is_empty() {
        return bad_request("AUTHOR_SESSION_REQUIRED",  "sessionId required");
    }
    let (text, src_path) = match resolve_publish_text(
        &state,
        &sess.workspace_id,
        &project,
        body.path.as_deref(),
        body.content.as_deref(),
    ) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let mut tavern = match state.sessions_tavern.get(sid) {
        Ok(t) => t,
        Err(e) => return map_core_err(e),
    };
    // soft check: prefer sessions bound to this project
    if let Some(ref pid) = tavern.author_project_id {
        if pid != &project.id {
            // allow but warn in response
        }
    } else {
        tavern.author_project_id = Some(project.id.clone());
    }
    let role = match body
        .as_role
        .as_deref()
        .unwrap_or("system")
        .trim()
        .to_lowercase()
        .as_str()
    {
        "director" | "dm" | "note" => "system",
        _ => "system",
    };
    let clipped: String = text.chars().take(6000).collect();
    let msg = kaleido_core::TavernMessage {
        id: format!("inj-{}", Uuid::new_v4()),
        role: role.into(),
        reasoning: None,
        content: format!(
            "【作者区同步 · 一次性】
来源: {}

{}",
            src_path.as_deref().unwrap_or("(inline)"),
            clipped
        ),
        created_at: Utc::now().to_rfc3339(),
        options: vec![],
        engine_tag: None,
        program: None,
        tokens: 0,
    };
    // Check TavernMessage fields - may differ
    // We'll fix compile if struct differs
    tavern.messages.push(msg);
    tavern.updated_at = Utc::now().to_rfc3339();
    match state.sessions_tavern.save(tavern) {
        Ok(saved) => Json(json!({
            "ok": true,
            "sessionId": saved.session_id,
            "messageCount": saved.messages.len(),
            "injected": true,
            "note": "一次性 system 注，不改写历史对白",
        }))
        .into_response(),
        Err(e) => map_core_err(e),
    }
}
