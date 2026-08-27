//! Agent tools API (S5-W1 T4).
//!
//! Routes (bearer via auth middleware):
//! - `POST /api/v1/agent/tools/read`  — body `{ "path": "..." }`
//! - `POST /api/v1/agent/tools/list`  — body `{ "path": "..." }`
//! - `POST /api/v1/agent/tools/write` — body `{ "path": "...", "content": "..." }`
//! - `POST /api/v1/agent/tools/bash`  — body `{ "command": "..." }`
//! - `POST /api/v1/agent/tools/edit`  — body `{ "path", "oldString", "newString", "replaceAll?" }`
//! - `POST /api/v1/agent/tools/grep`  — body `{ "pattern", "path?" }`
//! - `POST /api/v1/agent/tools/glob`  — body `{ "pattern", "path?" }`
//!
//! Path jail for edit/grep/glob: `works/{workspace}` (caller workspace works root).
//! read/list/write historically use data_root; session tool loop jails to works root.
//! Escapes (`..`, absolute, symlink out) → 403. Deny secrets/, state/, sessions/ prefixes.
//!
//! Bash: gated by settings `bashSandboxEnabled` (default **false**).
//! When disabled → 403 `{ "error": "bash_disabled" }`.
//! When enabled → whitelist-only short commands (`echo`, `uname`, `pwd`, `ls`) in
//! jail cwd, timeout ≤ 5s, no shell metacharacters.
//!
//! W10 policy:
//! - `agentToolsEnabled` (default true) gates read/list/grep/glob.
//! - `agentWriteEnabled` (default **false**) gates write/edit separately.
//! - `agentConfirmDangerous` (default **true**): write/edit/bash require
//!   body field `confirmDangerous: true` or 403 `confirm_required`.
//! - `bashSandboxEnabled` still gates bash (default false).

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use kaleido_core::CoreError;
use regex::Regex;
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    fs,
    io,
    path::{Component, Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::process::Command;
use tokio::time::timeout;

use crate::{map_core_err, session_from, AppState};
use crate::error_codes::*;

/// Max agent-tool file payload (2 MiB), matching WorksFs default.
const AGENT_TOOLS_MAX_BYTES: u64 = 2 * 1024 * 1024;
const BASH_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PathBody {
    pub path: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteBody {
    pub path: String,
    pub content: String,
    /// W10: required when settings.agentConfirmDangerous (default true).
    #[serde(default)]
    pub confirm_dangerous: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BashBody {
    pub command: String,
    /// W10: required when settings.agentConfirmDangerous (default true).
    #[serde(default)]
    pub confirm_dangerous: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditBody {
    pub path: String,
    pub old_string: String,
    pub new_string: String,
    #[serde(default)]
    pub replace_all: bool,
    /// W10: required when settings.agentConfirmDangerous (default true).
    #[serde(default)]
    pub confirm_dangerous: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrepBody {
    pub pattern: String,
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GlobBody {
    pub pattern: String,
    #[serde(default)]
    pub path: Option<String>,
}

const GREP_HIT_CAP: usize = 200;
const GLOB_HIT_CAP: usize = 500;

/// Router fragment for main to `.merge(...)`.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/agent/tools/read", post(read))
        .route("/api/v1/agent/tools/list", post(list))
        .route("/api/v1/agent/tools/write", post(write))
        .route("/api/v1/agent/tools/bash", post(bash))
        .route("/api/v1/agent/tools/edit", post(edit))
        .route("/api/v1/agent/tools/grep", post(grep))
        .route("/api/v1/agent/tools/glob", post(glob))
}

// ---------- handlers ----------

/// `POST /api/v1/agent/tools/read`
pub async fn read(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<PathBody>,
) -> Response {
    let sess = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    if !agent_tools_enabled(&state) {
        return tools_disabled();
    }
    let root = match works_jail_root(&state, &sess.workspace_id) {
        Ok(p) => p,
        Err(e) => return map_core_err(e),
    };
    match jail_read(&root, &body.path) {
        Ok((rel, content, size)) => Json(json!({
            "path": rel,
            "content": content,
            "size": size,
        }))
        .into_response(),
        Err(e) => map_core_err(e),
    }
}

/// `POST /api/v1/agent/tools/list`
pub async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<PathBody>,
) -> Response {
    let sess = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    if !agent_tools_enabled(&state) {
        return tools_disabled();
    }
    let root = match works_jail_root(&state, &sess.workspace_id) {
        Ok(p) => p,
        Err(e) => return map_core_err(e),
    };
    match jail_list(&root, &body.path) {
        Ok(v) => Json(v).into_response(),
        Err(e) => map_core_err(e),
    }
}

/// `POST /api/v1/agent/tools/write`
pub async fn write(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<WriteBody>,
) -> Response {
    let sess = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    // W10: write is gated by agentWriteEnabled (default false), not only agentToolsEnabled.
    if !agent_tools_enabled(&state) {
        return tools_disabled();
    }
    if !agent_write_enabled(&state) {
        return write_disabled();
    }
    if let Err(r) = require_confirm_dangerous(&state, body.confirm_dangerous) {
        return r;
    }
    let root = match works_jail_root(&state, &sess.workspace_id) {
        Ok(p) => p,
        Err(e) => return map_core_err(e),
    };
    match jail_write(
        &root,
        &body.path,
        &body.content,
    ) {
        Ok((rel, size)) => Json(json!({
            "path": rel,
            "size": size,
            "ok": true,
        }))
        .into_response(),
        Err(e) => map_core_err(e),
    }
}

/// `POST /api/v1/agent/tools/bash`
pub async fn bash(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<BashBody>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    if !bash_sandbox_enabled(&state) {
        return forbidden("ATOOL_DISABLED", "bash_disabled");
    }
    if let Err(r) = require_confirm_dangerous(&state, body.confirm_dangerous) {
        return r;
    }
    match run_sandboxed_bash(state.auth.data_root().root(), &body.command).await {
        Ok(out) => Json(out).into_response(),
        Err(e) => map_core_err(e),
    }
}

/// `POST /api/v1/agent/tools/edit` — unique string replace under works jail.
pub async fn edit(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<EditBody>,
) -> Response {
    let sess = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    if !agent_tools_enabled(&state) {
        return tools_disabled();
    }
    if !agent_write_enabled(&state) {
        return write_disabled();
    }
    if let Err(r) = require_confirm_dangerous(&state, body.confirm_dangerous) {
        return r;
    }
    let root = match works_jail_root(&state, &sess.workspace_id) {
        Ok(p) => p,
        Err(e) => return map_core_err(e),
    };
    match jail_edit(
        &root,
        &body.path,
        &body.old_string,
        &body.new_string,
        body.replace_all,
    ) {
        Ok(v) => Json(v).into_response(),
        Err(e) => map_core_err(e),
    }
}

/// `POST /api/v1/agent/tools/grep` — regex search under works jail.
pub async fn grep(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<GrepBody>,
) -> Response {
    let sess = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    if !agent_tools_enabled(&state) {
        return tools_disabled();
    }
    let root = match works_jail_root(&state, &sess.workspace_id) {
        Ok(p) => p,
        Err(e) => return map_core_err(e),
    };
    let path = body.path.as_deref().unwrap_or("");
    match jail_grep(&root, path, &body.pattern) {
        Ok(v) => Json(v).into_response(),
        Err(e) => map_core_err(e),
    }
}

/// `POST /api/v1/agent/tools/glob` — filename pattern under works jail.
pub async fn glob(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<GlobBody>,
) -> Response {
    let sess = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    if !agent_tools_enabled(&state) {
        return tools_disabled();
    }
    let root = match works_jail_root(&state, &sess.workspace_id) {
        Ok(p) => p,
        Err(e) => return map_core_err(e),
    };
    let path = body.path.as_deref().unwrap_or("");
    match jail_glob(&root, path, &body.pattern) {
        Ok(v) => Json(v).into_response(),
        Err(e) => map_core_err(e),
    }
}

// ---------- settings flags (W10) ----------

/// Kill-switch for read/list/grep/glob; default **true**.
pub(crate) fn agent_tools_enabled(state: &AppState) -> bool {
    state
        .app_state
        .load_settings_public()
        .map(|s| s.agent_tools_enabled)
        .unwrap_or(true)
}

/// Write/edit gate; default **false** (W10).
pub(crate) fn agent_write_enabled(state: &AppState) -> bool {
    state
        .app_state
        .load_settings_public()
        .map(|s| s.agent_write_enabled)
        .unwrap_or(false)
}

/// When true (default), dangerous tools require confirmDangerous body flag.
pub(crate) fn agent_confirm_dangerous(state: &AppState) -> bool {
    state
        .app_state
        .load_settings_public()
        .map(|s| s.agent_confirm_dangerous)
        .unwrap_or(true)
}

/// Bash gate; default **false**.
pub(crate) fn bash_sandbox_enabled(state: &AppState) -> bool {
    state
        .app_state
        .load_settings_public()
        .map(|s| s.bash_sandbox_enabled)
        .unwrap_or(false)
}

fn tools_disabled() -> Response {
    return forbidden("AGENT_TOOLS_DISABLED", "agent_tools_disabled");
}

fn write_disabled() -> Response {
    return err_with_code(
            StatusCode::FORBIDDEN,
            "AGENT_WRITE_DISABLED", "agent_write_disabled",
            serde_json::json!({"hint": "Enable agentWriteEnabled in settings, then retry with confirmDangerous=true"}),
    );
}

fn confirm_required() -> Response {
    return err_with_code(
            StatusCode::FORBIDDEN,
            "CONFIRM_REQUIRED", "confirm_required",
            serde_json::json!({"hint": "Pass confirmDangerous: true for write/edit/bash"}),
    );
}

fn require_confirm_dangerous(state: &AppState, confirmed: bool) -> Result<(), Response> {
    if agent_confirm_dangerous(state) && !confirmed {
        return Err(confirm_required());
    }
    Ok(())
}

// ---------- jail helpers (data_root) ----------

fn jail_canonical(data_root: &Path) -> Result<PathBuf, CoreError> {
    fs::create_dir_all(data_root)?;
    fs::canonicalize(data_root).map_err(|e| {
        CoreError::Io(io::Error::new(
            e.kind(),
            format!("canonicalize data_root: {e}"),
        ))
    })
}

/// Normalize client path: relative only, no `..`, no absolute.
fn normalize_agent_path(input: &str) -> Result<String, CoreError> {
    let s = input.trim();
    let s = s.trim_start_matches("./");
    if s.is_empty() || s == "." {
        return Ok(String::new());
    }
    if s.starts_with('/') || s.starts_with('\\') {
        return Err(CoreError::Forbidden("path escapes agent jail".into()));
    }
    if s.len() >= 2 && s.as_bytes()[1] == b':' {
        return Err(CoreError::Forbidden("path escapes agent jail".into()));
    }
    if s.contains('\0') {
        return Err(CoreError::BadRequest("invalid path".into()));
    }
    let mut parts: Vec<&str> = Vec::new();
    for part in s.split(['/', '\\']) {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            return Err(CoreError::Forbidden("path escapes agent jail".into()));
        }
        if part.contains('\0') {
            return Err(CoreError::BadRequest("invalid path".into()));
        }
        parts.push(part);
    }
    Ok(parts.join("/"))
}

fn ensure_under_jail(path: &Path, jail: &Path) -> Result<(), CoreError> {
    if path == jail || path.starts_with(jail) {
        return Ok(());
    }
    Err(CoreError::Forbidden("path escapes agent jail".into()))
}

fn resolve_in_jail(data_root: &Path, rel: &str, for_create: bool) -> Result<PathBuf, CoreError> {
    let rel = normalize_agent_path(rel)?;
    let jail = jail_canonical(data_root)?;
    if rel.is_empty() {
        return Ok(jail);
    }
    let candidate = jail.join(&rel);
    // Reject any remaining `..` components after join (defensive).
    for c in candidate.components() {
        if matches!(c, Component::ParentDir) {
            return Err(CoreError::Forbidden("path escapes agent jail".into()));
        }
    }
    if for_create {
        let mut existing = candidate.as_path();
        let mut missing: Vec<std::ffi::OsString> = Vec::new();
        loop {
            // 用 symlink_metadata 而非 exists()：悬空/指向 jail 外的 symlink 叶子
            // 在 exists() 下会被误判为 "missing" 而追加写穿（audit P0#2 symlink 写穿）。
            match fs::symlink_metadata(existing) {
                Ok(meta) => {
                    if meta.file_type().is_symlink() {
                        return Err(CoreError::Forbidden(
                            "symlink leaf not allowed under agent jail".into(),
                        ));
                    }
                    break;
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    match existing.file_name() {
                        Some(name) => {
                            missing.push(name.to_os_string());
                            existing = existing.parent().ok_or_else(|| {
                                CoreError::Forbidden("path escapes agent jail".into())
                            })?;
                        }
                        None => {
                            return Err(CoreError::Forbidden("path escapes agent jail".into()));
                        }
                    }
                }
                Err(e) => return Err(CoreError::Io(e)),
            }
        }
        let mut resolved = fs::canonicalize(existing).map_err(|e| {
            CoreError::Io(io::Error::new(e.kind(), format!("canonicalize parent: {e}")))
        })?;
        ensure_under_jail(&resolved, &jail)?;
        for part in missing.into_iter().rev() {
            let s = part.to_string_lossy();
            if s == ".." || s == "." || s.contains('\0') {
                return Err(CoreError::BadRequest("invalid path component".into()));
            }
            resolved.push(part);
        }
        ensure_under_jail(&resolved, &jail)?;
        Ok(resolved)
    } else {
        if !candidate.exists() {
            // still validate escape shape
            let _ = resolve_in_jail(data_root, &rel, true)?;
            return Err(CoreError::NotFound(format!("path {rel}")));
        }
        let resolved = fs::canonicalize(&candidate).map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound {
                CoreError::NotFound(format!("path {rel}"))
            } else {
                CoreError::Io(e)
            }
        })?;
        ensure_under_jail(&resolved, &jail)?;
        Ok(resolved)
    }
}

fn display_rel(abs: &Path, jail: &Path) -> Result<String, CoreError> {
    if abs == jail {
        return Ok(String::new());
    }
    let rel = abs
        .strip_prefix(jail)
        .map_err(|_| CoreError::Forbidden("path escapes agent jail".into()))?;
    Ok(rel.to_string_lossy().replace('\\', "/"))
}

/// Public wrapper for agent session tool loop.
pub fn jail_read_public(
    data_root: &Path,
    path: &str,
) -> Result<(String, String, u64), CoreError> {
    jail_read(data_root, path)
}

/// Public wrapper for agent session tool loop.
pub fn jail_list_public(data_root: &Path, path: &str) -> Result<Value, CoreError> {
    jail_list(data_root, path)
}

/// Public wrapper for agent session tool loop.
pub fn jail_write_public(
    data_root: &Path,
    path: &str,
    content: &str,
) -> Result<(String, u64), CoreError> {
    jail_write(data_root, path, content)
}

/// Public wrapper for agent session tool loop.
pub async fn run_sandboxed_bash_public(
    data_root: &Path,
    command: &str,
) -> Result<Value, CoreError> {
    run_sandboxed_bash(data_root, command).await
}

/// Public wrapper for agent session tool loop (edit).
pub fn jail_edit_public(
    works_root: &Path,
    path: &str,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
) -> Result<Value, CoreError> {
    jail_edit(works_root, path, old_string, new_string, replace_all)
}

/// Public wrapper for agent session tool loop (grep).
pub fn jail_grep_public(works_root: &Path, path: &str, pattern: &str) -> Result<Value, CoreError> {
    jail_grep(works_root, path, pattern)
}

/// Public wrapper for agent session tool loop (glob).
pub fn jail_glob_public(works_root: &Path, path: &str, pattern: &str) -> Result<Value, CoreError> {
    jail_glob(works_root, path, pattern)
}


fn jail_read(data_root: &Path, path: &str) -> Result<(String, String, u64), CoreError> {
    let rel = normalize_agent_path(path)?;
    deny_reserved_prefix(&rel)?;
    let abs = resolve_in_jail(data_root, &rel, false)?;
    let meta = fs::metadata(&abs)?;
    if meta.is_dir() {
        return Err(CoreError::BadRequest("path is a directory; use list".into()));
    }
    let size = meta.len();
    if size > AGENT_TOOLS_MAX_BYTES {
        return Err(CoreError::BadRequest(format!(
            "file too large (max {AGENT_TOOLS_MAX_BYTES} bytes)"
        )));
    }
    let content = fs::read_to_string(&abs).map_err(|e| {
        if e.kind() == io::ErrorKind::InvalidData {
            CoreError::BadRequest("file is not valid UTF-8 text".into())
        } else {
            CoreError::Io(e)
        }
    })?;
    let jail = jail_canonical(data_root)?;
    let rel = display_rel(&abs, &jail)?;
    Ok((rel, content, size))
}

fn jail_list(data_root: &Path, path: &str) -> Result<Value, CoreError> {
    let rel = normalize_agent_path(path)?;
    deny_reserved_prefix(&rel)?;
    let abs = resolve_in_jail(data_root, &rel, false)?;
    let meta = fs::metadata(&abs)?;
    if !meta.is_dir() {
        return Err(CoreError::BadRequest("list requires a directory".into()));
    }
    let jail = jail_canonical(data_root)?;
    let rel = display_rel(&abs, &jail)?;
    let mut entries = Vec::new();
    let rd = fs::read_dir(&abs)?;
    for ent in rd {
        let ent = ent?;
        let name = ent.file_name().to_string_lossy().to_string();
        let child = ent.path();
        // Symlink / race: only surface children that stay in jail after canonicalize.
        let child_meta = match fs::symlink_metadata(&child) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let kind = if child_meta.file_type().is_symlink() {
            match fs::canonicalize(&child) {
                Ok(canon) => {
                    if ensure_under_jail(&canon, &jail).is_err() {
                        continue; // symlink escape → hide
                    }
                    if canon.is_dir() {
                        "dir"
                    } else {
                        "file"
                    }
                }
                Err(_) => continue,
            }
        } else if child_meta.is_dir() {
            "dir"
        } else {
            "file"
        };
        let child_rel = if rel.is_empty() {
            name.clone()
        } else {
            format!("{rel}/{name}")
        };
        let size = if kind == "file" {
            Some(child_meta.len())
        } else {
            None
        };
        entries.push(json!({
            "path": child_rel,
            "name": name,
            "kind": kind,
            "size": size,
        }));
    }
    entries.sort_by(|a, b| {
        let an = a.get("name").and_then(|x| x.as_str()).unwrap_or("");
        let bn = b.get("name").and_then(|x| x.as_str()).unwrap_or("");
        an.cmp(bn)
    });
    Ok(json!({
        "path": rel,
        "kind": "dir",
        "entries": entries,
    }))
}

fn jail_write(data_root: &Path, path: &str, content: &str) -> Result<(String, u64), CoreError> {
    if content.len() as u64 > AGENT_TOOLS_MAX_BYTES {
        return Err(CoreError::BadRequest(format!(
            "content too large (max {AGENT_TOOLS_MAX_BYTES} bytes)"
        )));
    }
    let rel = normalize_agent_path(path)?;
    if rel.is_empty() {
        return Err(CoreError::BadRequest("cannot write to data root".into()));
    }
    deny_reserved_prefix(&rel)?;
    let abs = resolve_in_jail(data_root, &rel, true)?;
    if let Some(parent) = abs.parent() {
        if !parent.exists() {
            return Err(CoreError::BadRequest(
                "parent directory does not exist".into(),
            ));
        }
        let parent_canon = fs::canonicalize(parent)?;
        let jail = jail_canonical(data_root)?;
        ensure_under_jail(&parent_canon, &jail)?;
    }
    if abs.exists() {
        let meta = fs::metadata(&abs)?;
        if meta.is_dir() {
            return Err(CoreError::BadRequest("path is a directory".into()));
        }
        let canon = fs::canonicalize(&abs)?;
        let jail = jail_canonical(data_root)?;
        ensure_under_jail(&canon, &jail)?;
    }
    fs::write(&abs, content.as_bytes())?;
    let jail = jail_canonical(data_root)?;
    let canon = fs::canonicalize(&abs)?;
    ensure_under_jail(&canon, &jail)?;
    let out_rel = display_rel(&canon, &jail)?;
    Ok((out_rel, content.len() as u64))
}

// ---------- works-root jail for edit/grep/glob ----------

/// Prefer works/{workspace} like session-run P0. Never free-jail whole data root.
fn works_jail_root(state: &AppState, workspace_id: &str) -> Result<PathBuf, CoreError> {
    state.works.workspace_root(workspace_id)
}

/// Reserved top-level prefixes never exposed to agent tools.
const RESERVED_PREFIXES: [&str; 3] = ["secrets", "state", "sessions"];

/// Deny reserved top-level prefixes even if present under works.
fn deny_reserved_prefix(rel: &str) -> Result<(), CoreError> {
    let first = rel.split('/').next().unwrap_or("");
    if RESERVED_PREFIXES.contains(&first) {
        return Err(CoreError::Forbidden(format!(
            "path under reserved prefix `{first}` not allowed"
        )));
    }
    Ok(())
}

fn jail_edit(
    works_root: &Path,
    path: &str,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
) -> Result<Value, CoreError> {
    if old_string.is_empty() {
        return Err(CoreError::BadRequest("oldString must not be empty".into()));
    }
    let rel_norm = normalize_agent_path(path)?;
    deny_reserved_prefix(&rel_norm)?;
    if rel_norm.is_empty() {
        return Err(CoreError::BadRequest("path required".into()));
    }
    let abs = resolve_in_jail(works_root, &rel_norm, false)?;
    let jail = jail_canonical(works_root)?;
    let rel = display_rel(&abs, &jail)?;
    deny_reserved_prefix(&rel)?;
    let meta = fs::metadata(&abs)?;
    if meta.is_dir() {
        return Err(CoreError::BadRequest("path is a directory".into()));
    }
    if meta.len() > AGENT_TOOLS_MAX_BYTES {
        return Err(CoreError::BadRequest(format!(
            "file too large (max {AGENT_TOOLS_MAX_BYTES} bytes)"
        )));
    }
    let content = fs::read_to_string(&abs).map_err(|e| {
        if e.kind() == io::ErrorKind::InvalidData {
            CoreError::BadRequest("file is not valid UTF-8 text".into())
        } else {
            CoreError::Io(e)
        }
    })?;
    let matches = content.matches(old_string).count();
    if matches == 0 {
        return Err(CoreError::BadRequest("oldString not found".into()));
    }
    if matches > 1 && !replace_all {
        return Err(CoreError::BadRequest(format!(
            "oldString matched {matches} times; set replaceAll=true or make oldString unique"
        )));
    }
    let new_content = if replace_all {
        content.replace(old_string, new_string)
    } else {
        content.replacen(old_string, new_string, 1)
    };
    if new_content.len() as u64 > AGENT_TOOLS_MAX_BYTES {
        return Err(CoreError::BadRequest(format!(
            "content too large after edit (max {AGENT_TOOLS_MAX_BYTES} bytes)"
        )));
    }
    fs::write(&abs, new_content.as_bytes())?;
    Ok(json!({
        "ok": true,
        "path": rel,
        "replacements": if replace_all { matches } else { 1 },
        "size": new_content.len() as u64,
    }))
}

fn jail_grep(works_root: &Path, path: &str, pattern: &str) -> Result<Value, CoreError> {
    if pattern.is_empty() {
        return Err(CoreError::BadRequest("pattern required".into()));
    }
    let re = Regex::new(pattern).map_err(|e| {
        CoreError::BadRequest(format!("invalid regex: {e}"))
    })?;
    let rel_norm = normalize_agent_path(path)?;
    deny_reserved_prefix(&rel_norm)?;
    let abs = if rel_norm.is_empty() {
        jail_canonical(works_root)?
    } else {
        resolve_in_jail(works_root, &rel_norm, false)?
    };
    let jail = jail_canonical(works_root)?;
    ensure_under_jail(&abs, &jail)?;
    let mut hits: Vec<Value> = Vec::new();
    let mut truncated = false;
    fn walk_grep(
        dir: &Path,
        jail: &Path,
        re: &Regex,
        hits: &mut Vec<Value>,
        truncated: &mut bool,
    ) -> Result<(), CoreError> {
        if *truncated || hits.len() >= GREP_HIT_CAP {
            *truncated = true;
            return Ok(());
        }
        let rd = match fs::read_dir(dir) {
            Ok(r) => r,
            Err(_) => return Ok(()),
        };
        let mut ents: Vec<_> = rd.filter_map(|e| e.ok()).collect();
        ents.sort_by_key(|e| e.file_name());
        for ent in ents {
            if hits.len() >= GREP_HIT_CAP {
                *truncated = true;
                break;
            }
            let child = ent.path();
            let meta = match fs::symlink_metadata(&child) {
                Ok(m) => m,
                Err(_) => continue,
            };
            if meta.file_type().is_symlink() {
                continue; // skip symlinks for search safety
            }
            if meta.is_dir() {
                let name = ent.file_name().to_string_lossy().to_string();
                if matches!(name.as_str(), "secrets" | "state" | "sessions" | ".git") {
                    continue;
                }
                // stay under jail
                if ensure_under_jail(&child, jail).is_err() {
                    continue;
                }
                walk_grep(&child, jail, re, hits, truncated)?;
                continue;
            }
            if meta.len() > AGENT_TOOLS_MAX_BYTES {
                continue;
            }
            let Ok(text) = fs::read_to_string(&child) else {
                continue;
            };
            let rel = display_rel(&child, jail).unwrap_or_default();
            if deny_reserved_prefix(&rel).is_err() {
                continue;
            }
            for (idx, line) in text.lines().enumerate() {
                if hits.len() >= GREP_HIT_CAP {
                    *truncated = true;
                    break;
                }
                if re.is_match(line) {
                    hits.push(json!({
                        "path": rel,
                        "line": idx + 1,
                        "text": line,
                    }));
                }
            }
        }
        Ok(())
    }

    let meta = fs::metadata(&abs)?;
    if meta.is_file() {
        if meta.len() <= AGENT_TOOLS_MAX_BYTES {
            if let Ok(text) = fs::read_to_string(&abs) {
                let rel = display_rel(&abs, &jail)?;
                deny_reserved_prefix(&rel)?;
                for (idx, line) in text.lines().enumerate() {
                    if hits.len() >= GREP_HIT_CAP {
                        truncated = true;
                        break;
                    }
                    if re.is_match(line) {
                        hits.push(json!({
                            "path": rel,
                            "line": idx + 1,
                            "text": line,
                        }));
                    }
                }
            }
        }
    } else {
        walk_grep(&abs, &jail, &re, &mut hits, &mut truncated)?;
    }
    Ok(json!({
        "ok": true,
        "pattern": pattern,
        "hits": hits,
        "count": hits.len(),
        "truncated": truncated,
        "cap": GREP_HIT_CAP,
    }))
}

fn glob_match(pattern: &str, name: &str) -> bool {
    // Simple glob: * and ? only; match against full relative path or file name.
    fn match_rec(p: &[u8], s: &[u8]) -> bool {
        let mut i = 0;
        let mut j = 0;
        let mut star_i = None;
        let mut star_j = 0;
        while j < s.len() {
            if i < p.len() && (p[i] == b'?' || p[i] == s[j]) {
                i += 1;
                j += 1;
            } else if i < p.len() && p[i] == b'*' {
                star_i = Some(i);
                star_j = j;
                i += 1;
            } else if let Some(si) = star_i {
                i = si + 1;
                star_j += 1;
                j = star_j;
            } else {
                return false;
            }
        }
        while i < p.len() && p[i] == b'*' {
            i += 1;
        }
        i == p.len()
    }
    match_rec(pattern.as_bytes(), name.as_bytes())
}

fn jail_glob(works_root: &Path, path: &str, pattern: &str) -> Result<Value, CoreError> {
    if pattern.is_empty() {
        return Err(CoreError::BadRequest("pattern required".into()));
    }
    if pattern.contains("..") || pattern.starts_with('/') || pattern.contains('\0') {
        return Err(CoreError::BadRequest("invalid glob pattern".into()));
    }
    let rel_norm = normalize_agent_path(path)?;
    deny_reserved_prefix(&rel_norm)?;
    let jail = jail_canonical(works_root)?;
    let abs = if rel_norm.is_empty() {
        jail.clone()
    } else {
        resolve_in_jail(works_root, &rel_norm, false)?
    };
    ensure_under_jail(&abs, &jail)?;
    let mut paths: Vec<String> = Vec::new();
    let mut truncated = false;

    fn walk_glob(
        dir: &Path,
        jail: &Path,
        pattern: &str,
        paths: &mut Vec<String>,
        truncated: &mut bool,
    ) -> Result<(), CoreError> {
        if *truncated || paths.len() >= GLOB_HIT_CAP {
            *truncated = true;
            return Ok(());
        }
        let rd = match fs::read_dir(dir) {
            Ok(r) => r,
            Err(_) => return Ok(()),
        };
        let mut ents: Vec<_> = rd.filter_map(|e| e.ok()).collect();
        ents.sort_by_key(|e| e.file_name());
        for ent in ents {
            if paths.len() >= GLOB_HIT_CAP {
                *truncated = true;
                break;
            }
            let child = ent.path();
            let meta = match fs::symlink_metadata(&child) {
                Ok(m) => m,
                Err(_) => continue,
            };
            if meta.file_type().is_symlink() {
                continue;
            }
            let name = ent.file_name().to_string_lossy().to_string();
            if matches!(name.as_str(), "secrets" | "state" | "sessions" | ".git") {
                continue;
            }
            if ensure_under_jail(&child, jail).is_err() {
                continue;
            }
            let rel = display_rel(&child, jail).unwrap_or_default();
            if deny_reserved_prefix(&rel).is_err() {
                continue;
            }
            // Match full rel path or basename.
            if glob_match(pattern, &rel) || glob_match(pattern, &name) {
                paths.push(rel.clone());
                if paths.len() >= GLOB_HIT_CAP {
                    *truncated = true;
                    break;
                }
            }
            if meta.is_dir() {
                walk_glob(&child, jail, pattern, paths, truncated)?;
            }
        }
        Ok(())
    }

    let meta = fs::metadata(&abs)?;
    if meta.is_file() {
        let rel = display_rel(&abs, &jail)?;
        let name = abs.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        if glob_match(pattern, &rel) || glob_match(pattern, &name) {
            paths.push(rel);
        }
    } else {
        walk_glob(&abs, &jail, pattern, &mut paths, &mut truncated)?;
    }
    Ok(json!({
        "ok": true,
        "pattern": pattern,
        "paths": paths,
        "count": paths.len(),
        "truncated": truncated,
        "cap": GLOB_HIT_CAP,
    }))
}

// ---------- bash sandbox (whitelist) ----------

/// Parse a simple argv: no shell metacharacters; first token must be whitelisted.
fn parse_whitelist_command(command: &str) -> Result<Vec<String>, CoreError> {
    let cmd = command.trim();
    if cmd.is_empty() {
        return Err(CoreError::BadRequest("command required".into()));
    }
    // Hard reject shell / network / redirection operators.
    const FORBIDDEN: &[char] = &[
        '|', '&', ';', '>', '<', '`', '$', '(', ')', '{', '}', '\n', '\r',
    ];
    if cmd.chars().any(|c| FORBIDDEN.contains(&c)) {
        return Err(CoreError::Forbidden(
            "bash sandbox: shell metacharacters not allowed".into(),
        ));
    }
    if cmd.contains("&&") || cmd.contains("||") {
        return Err(CoreError::Forbidden(
            "bash sandbox: shell metacharacters not allowed".into(),
        ));
    }
    let parts: Vec<String> = cmd.split_whitespace().map(|s| s.to_string()).collect();
    if parts.is_empty() {
        return Err(CoreError::BadRequest("command required".into()));
    }
    let bin = parts[0].as_str();
    // Only bare names — no absolute path binaries.
    if bin.contains('/') || bin.contains('\\') {
        return Err(CoreError::Forbidden(
            "bash sandbox: absolute/relative binary paths not allowed".into(),
        ));
    }
    const ALLOW: &[&str] = &["echo", "uname", "pwd", "ls", "true", "false"];
    if !ALLOW.contains(&bin) {
        return Err(CoreError::Forbidden(format!(
            "bash sandbox: command `{bin}` not in whitelist"
        )));
    }
    // Extra arg constraints for ls: only relative paths, no flags that walk out.
    if bin == "ls" {
        for a in &parts[1..] {
            if a.starts_with('-') {
                // allow only a small flag set
                if !matches!(a.as_str(), "-a" | "-l" | "-la" | "-al" | "-1") {
                    return Err(CoreError::Forbidden(
                        "bash sandbox: ls flag not allowed".into(),
                    ));
                }
                continue;
            }
            // path args must normalize inside jail shape
            let _ = normalize_agent_path(a)?;
        }
    } else if bin == "echo" || bin == "uname" {
        // args are data only; uname flags limited
        if bin == "uname" {
            for a in &parts[1..] {
                if !matches!(a.as_str(), "-a" | "-s" | "-n" | "-r" | "-m") {
                    return Err(CoreError::Forbidden(
                        "bash sandbox: uname flag not allowed".into(),
                    ));
                }
            }
        }
    } else if parts.len() > 1 && (bin == "pwd" || bin == "true" || bin == "false") {
        return Err(CoreError::Forbidden(
            "bash sandbox: unexpected arguments".into(),
        ));
    }
    Ok(parts)
}

async fn run_sandboxed_bash(data_root: &Path, command: &str) -> Result<Value, CoreError> {
    let argv = parse_whitelist_command(command)?;
    let jail = jail_canonical(data_root)?;
    let program = argv[0].clone();
    let args = argv[1..].to_vec();

    let mut child = Command::new(&program);
    child
        .args(&args)
        .current_dir(&jail)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", &jail)
        .env("LANG", "C");

    let child = child.spawn().map_err(|e| {
        CoreError::BadRequest(format!("failed to spawn `{program}`: {e}"))
    })?;

    // kill_on_drop(true): dropping the future on timeout kills the process.
    let out = match timeout(BASH_TIMEOUT, child.wait_with_output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            return Err(CoreError::Io(e));
        }
        Err(_) => {
            return Err(CoreError::BadRequest(
                "bash sandbox: command timed out (5s)".into(),
            ));
        }
    };

    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    Ok(json!({
        "ok": out.status.success(),
        "exitCode": out.status.code(),
        "stdout": stdout,
        "stderr": stderr,
        "command": argv,
        "cwd": jail.to_string_lossy(),
        "sandbox": "whitelist",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tmp_root() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("kaleido-agent-tools-{nanos}"));
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn normalize_rejects_dotdot_and_abs() {
        assert!(normalize_agent_path("../etc/passwd").is_err());
        assert!(normalize_agent_path("/etc/passwd").is_err());
        assert!(normalize_agent_path("foo/../../x").is_err());
        assert_eq!(normalize_agent_path("a/b").unwrap(), "a/b");
    }

    #[test]
    fn read_write_roundtrip_in_jail() {
        let root = tmp_root();
        fs::create_dir_all(root.join("notes")).unwrap();
        jail_write(&root, "notes/hello.txt", "hi").unwrap();
        let (rel, content, size) = jail_read(&root, "notes/hello.txt").unwrap();
        assert_eq!(rel, "notes/hello.txt");
        assert_eq!(content, "hi");
        assert_eq!(size, 2);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn reserved_prefixes_forbidden() {
        let root = tmp_root();
        fs::create_dir_all(root.join("secrets")).unwrap();
        let err = jail_read(&root, "secrets/api_key.txt").unwrap_err();
        match err {
            CoreError::Forbidden(_) => {}
            other => panic!("unexpected {other}"),
        }
        assert!(jail_list(&root, "secrets").is_err());
        assert!(jail_write(&root, "secrets/x.txt", "x").is_err());
        assert!(jail_read(&root, "state/whatever").is_err());
        assert!(jail_read(&root, "sessions/s.json").is_err());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn escape_read_forbidden() {
        let root = tmp_root();
        let err = jail_read(&root, "../Cargo.toml").unwrap_err();
        match err {
            CoreError::Forbidden(_) | CoreError::BadRequest(_) => {}
            other => panic!("unexpected {other}"),
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn whitelist_parse() {
        assert!(parse_whitelist_command("echo hi").is_ok());
        assert!(parse_whitelist_command("uname -s").is_ok());
        assert!(parse_whitelist_command("rm -rf /").is_err());
        assert!(parse_whitelist_command("echo hi; id").is_err());
        assert!(parse_whitelist_command("cat /etc/passwd").is_err());
    }
}
