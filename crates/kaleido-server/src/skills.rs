//! Skills library (S5-W2 T5).
//! GET/POST/DELETE /api/v1/skills — store under data_root/skills/<name>/SKILL.md

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{delete, get},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::fs;
use std::io;
use std::path::{Path as FsPath, PathBuf};

use crate::{session_from, AppState};
use crate::error_codes::*;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportSkillBody {
    /// Skill name (directory name). Required unless content frontmatter has name.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// Full SKILL.md markdown (with optional YAML frontmatter).
    #[serde(default)]
    pub content: Option<String>,
    /// Alias: skill markdown body.
    #[serde(default)]
    pub skill_md: Option<String>,
    /// Optional absolute/relative path to an existing skill dir or SKILL.md (server-local import).
    #[serde(default)]
    pub path: Option<String>,
    /// If true, overwrite existing skill.
    #[serde(default)]
    pub overwrite: Option<bool>,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/skills", get(list_skills).post(import_skill))
        .route(
            "/api/v1/skills/{name}",
            delete(delete_skill).get(get_skill),
        )
        .route(
            "/api/v1/skills/writing/active",
            get(get_active_skill).put(put_active_skill),
        )
}

fn skills_root(state: &AppState) -> PathBuf {
    state.auth.data_root().root().join("skills")
}

fn sanitize_name(name: &str) -> Option<String> {
    let s = name.trim();
    if s.is_empty() || s.len() > 64 {
        return None;
    }
    if s.contains("..") || s.contains('/') || s.contains('\\') || s.contains('\0') {
        return None;
    }
    if !s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        return None;
    }
    Some(s.to_string())
}

fn extract_frontmatter_value(body: &str, key: &str) -> Option<String> {
    let trimmed = body.trim_start();
    if !trimmed.starts_with("---") {
        return None;
    }
    let rest = trimmed.trim_start_matches("---");
    let end = rest.find("\n---")?;
    let fm = &rest[..end];
    for line in fm.lines() {
        let line = line.trim();
        if let Some((k, v)) = line.split_once(':') {
            if k.trim() == key {
                let v = v.trim().trim_matches('"').trim_matches('\'').to_string();
                if !v.is_empty() {
                    return Some(v);
                }
            }
        }
    }
    None
}

fn parse_skill_dir(dir: &FsPath) -> Option<Value> {
    let md = dir.join("SKILL.md");
    if !md.is_file() {
        return None;
    }
    let body = fs::read_to_string(&md).ok()?;
    let name = extract_frontmatter_value(&body, "name").or_else(|| {
        dir.file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string())
    })?;
    let description = extract_frontmatter_value(&body, "description").unwrap_or_default();
    // P4: 增读 kind / tier / agent / parents（writing 子命名空间用；agent/parents 预留）。
    let kind = extract_frontmatter_value(&body, "kind").unwrap_or_default();
    let mut tier = extract_frontmatter_value(&body, "tier").unwrap_or_default();
    if tier.is_empty() {
        // 由 skills/writing/<tier>/ 子目录名推断
        if dir
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            == Some(crate::skill_layer::WRITING_NS)
        {
            if let Some(t) = dir.file_name().and_then(|n| n.to_str()) {
                tier = t.to_string();
            }
        }
    }
    let agent = extract_frontmatter_value(&body, "agent").unwrap_or_default();
    let parents = prepare_fm_list(&body, "parents");
    let meta = fs::metadata(&md).ok();
    let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
    let mtime = meta
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Do not leak absolute host paths (cross-audit M4).
    Some(json!({
        "name": name,
        "description": description,
        "path": format!("skills/{name}"),
        "size": size,
        "mtime": mtime,
        "contentPreview": body.chars().take(240).collect::<String>(),
        "kind": kind,
        "tier": tier,
        "agent": agent,
        "parents": parents,
    }))
}

/// P4: 解析 frontmatter 中的列表字段（YAML 风格 `[a, b]` / `[a]` / 多行 `- item`）。
fn prepare_fm_list(body: &str, key: &str) -> Vec<String> {
    let trimmed = body.trim_start();
    if !trimmed.starts_with("---") {
        return vec![];
    }
    let rest = trimmed.trim_start_matches("---");
    let Some(end) = rest.find("\n---") else {
        return vec![];
    };
    let fm = &rest[..end];
    let mut out = Vec::new();
    let mut in_list = false;
    for line in fm.lines() {
        let line = line.trim();
        if let Some((k, v)) = line.split_once(':') {
            if k.trim() == key {
                let v = v.trim().trim_start_matches('[').trim_end_matches(']');
                if v.starts_with('-') {
                    in_list = true;
                    out.push(
                        v.trim_start_matches('-')
                            .trim()
                            .trim_matches('"')
                            .trim_matches('\'')
                            .to_string(),
                    );
                } else {
                    for item in v.split(',') {
                        let item = item.trim().trim_matches('"').trim_matches('\'');
                        if !item.is_empty() {
                            out.push(item.to_string());
                        }
                    }
                }
            } else if in_list && line.starts_with('-') {
                out.push(
                    line.trim_start_matches('-')
                        .trim()
                        .trim_matches('"')
                        .trim_matches('\'')
                        .to_string(),
                );
            }
        } else {
            in_list = false;
        }
    }
    out.retain(|s| !s.is_empty());
    out
}

fn copy_dir_recursive(src: &FsPath, dst: &FsPath) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_recursive(&entry.path(), &to)?;
        } else if ty.is_file() {
            fs::copy(entry.path(), to)?;
        }
    }
    Ok(())
}

fn remove_dir_all_quiet(path: &FsPath) {
    let _ = fs::remove_dir_all(path);
}

async fn list_skills(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let root = skills_root(&state);
    let mut skills = Vec::new();
    if root.is_dir() {
        if let Ok(rd) = fs::read_dir(&root) {
            for entry in rd.flatten() {
                if entry.path().is_dir() {
                    if let Some(v) = parse_skill_dir(&entry.path()) {
                        skills.push(v);
                    }
                }
            }
        }
    }
    skills.sort_by(|a, b| {
        a.get("name")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .cmp(b.get("name").and_then(|x| x.as_str()).unwrap_or(""))
    });
    Json(json!({
        "ok": true,
        "skills": skills,
        "count": skills.len(),
    }))
    .into_response()
}

async fn get_skill(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let Some(name) = sanitize_name(&name) else {
        return bad_request("SKILL_INVALID", "invalid skill name");
    };
    let dir = skills_root(&state).join(&name);
    let md = dir.join("SKILL.md");
    if !md.is_file() {
        return not_found("SKILL_NOT_FOUND", format!("skill not found: {name}"));
    }
    match fs::read_to_string(&md) {
        Ok(content) => {
            let description = extract_frontmatter_value(&content, "description").unwrap_or_default();
            Json(json!({
                "ok": true,
                "name": name,
                "description": description,
                "content": content,
            }))
            .into_response()
        }
        Err(e) => internal("SKILL_INTERNAL", e.to_string()),
    }
}

async fn import_skill(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ImportSkillBody>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let overwrite = body.overwrite.unwrap_or(false);
    let root = skills_root(&state);
    if let Err(e) = fs::create_dir_all(&root) {
        return internal("SKILL_INTERNAL", e.to_string());
    }

    // Path-based import: ONLY under $KALEIDO_DATA (cross-audit C1/C2).
    // Absolute host paths and escapes are rejected.
    if let Some(path) = body.path.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        let data_root = state.auth.data_root().root().to_path_buf();
        let src = match resolve_skill_import_src(&data_root, path) {
            Ok(p) => p,
            Err(msg) => {
                return forbidden("SKILL_FORBIDDEN", msg);
            }
        };
        if !src.exists() {
            return not_found("SKILL_NOT_FOUND", format!("path not found under data root: {path}"));
        }
        let (src_dir, content_opt) = if src.is_file() {
            let content = fs::read_to_string(&src).unwrap_or_default();
            (
                src.parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| data_root.clone()),
                Some(content),
            )
        } else {
            (src.clone(), fs::read_to_string(src.join("SKILL.md")).ok())
        };
        // Re-check dir is still under data root after parent resolution
        if let Err(msg) = ensure_under_data_root(&data_root, &src_dir) {
            return forbidden("SKILL_FORBIDDEN", msg);
        }
        let name = body
            .name
            .clone()
            .or_else(|| content_opt.as_deref().and_then(|c| extract_frontmatter_value(c, "name")))
            .or_else(|| {
                src_dir
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(|s| s.to_string())
            })
            .unwrap_or_default();
        let Some(name) = sanitize_name(&name) else {
            return bad_request("SKILL_INVALID", "invalid skill name");
        };
        let dest = root.join(&name);
        if dest.exists() && !overwrite {
            return conflict("SKILL_CONFLICT", format!("skill exists: {name}"));
        }
        if dest.exists() {
            remove_dir_all_quiet(&dest);
        }
        if let Err(e) = copy_dir_recursive(&src_dir, &dest) {
            return internal("SKILL_INTERNAL", e.to_string());
        }
        if let Some(content) = content_opt {
            if !dest.join("SKILL.md").is_file() {
                let _ = fs::write(dest.join("SKILL.md"), content);
            }
        }
        return match parse_skill_dir(&dest) {
            Some(v) => (StatusCode::CREATED, Json(v)).into_response(),
            None => (
                StatusCode::CREATED,
                Json(json!({"ok": true, "name": name, "error": "write ok but parse failed"})),
            )
                .into_response(),
        };
    }

    let content = body
        .content
        .clone()
        .or(body.skill_md.clone())
        .unwrap_or_default();
    if content.trim().is_empty() {
        return bad_request("SKILL_MISSING_FIELD", "content or path required");
    }
    let name = body
        .name
        .clone()
        .or_else(|| extract_frontmatter_value(&content, "name"))
        .unwrap_or_default();
    let Some(name) = sanitize_name(&name) else {
        return bad_request("SKILL_INVALID", "invalid skill name");
    };
    let description = body
        .description
        .clone()
        .or_else(|| extract_frontmatter_value(&content, "description"))
        .unwrap_or_default();

    // Ensure frontmatter has name/description
    let final_content = if content.trim_start().starts_with("---") {
        content
    } else {
        format!(
            "---\nname: {name}\ndescription: {description}\n---\n\n{content}"
        )
    };

    let dest = root.join(&name);
    if dest.exists() && !overwrite {
        return conflict("SKILL_CONFLICT", format!("skill exists: {name}"));
    }
    if let Err(e) = fs::create_dir_all(&dest) {
        return internal("SKILL_INTERNAL", e.to_string());
    }
    if let Err(e) = fs::write(dest.join("SKILL.md"), &final_content) {
        return internal("SKILL_INTERNAL", e.to_string());
    }
    match parse_skill_dir(&dest) {
        Some(mut v) => {
            if let Some(obj) = v.as_object_mut() {
                obj.insert("ok".into(), json!(true));
                obj.insert("content".into(), json!(final_content));
            }
            (StatusCode::CREATED, Json(v)).into_response()
        }
        None => (
            StatusCode::CREATED,
            Json(json!({"ok": true, "name": name, "description": description})),
        )
            .into_response(),
    }
}

async fn delete_skill(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let Some(name) = sanitize_name(&name) else {
        return bad_request("SKILL_INVALID", "invalid skill name");
    };
    let dest = skills_root(&state).join(&name);
    if !dest.exists() {
        return not_found("SKILL_NOT_FOUND", format!("skill not found: {name}"));
    }
    match fs::remove_dir_all(&dest) {
        Ok(()) => Json(json!({"ok": true, "name": name})).into_response(),
        Err(e) => internal("SKILL_INTERNAL", e.to_string()),
    }
}

// ─── 写作 Skill active 档位（P4 后置①）────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PutActiveSkillBody {
    #[serde(default)]
    pub tier: Option<String>,
    #[serde(default)]
    pub quality: Option<String>,
}

/// 写作档位严格三选一（lite / standard / heavy），大小写归一；非法返回 None。
fn parse_writing_tier(s: &str) -> Option<&'static str> {
    match s.trim().to_ascii_lowercase().as_str() {
        "lite" => Some("lite"),
        "standard" => Some("standard"),
        "heavy" => Some("heavy"),
        _ => None,
    }
}

/// `<data_root>/skills/writing/active.json`
fn writing_active_path(root: &FsPath) -> PathBuf {
    root.join(crate::skill_layer::WRITING_NS).join("active.json")
}

fn read_active_tier(root: &FsPath) -> Option<String> {
    let s = fs::read_to_string(writing_active_path(root)).ok()?;
    let v: Value = serde_json::from_str(&s).ok()?;
    v.get("tier").and_then(|t| t.as_str()).map(|s| s.to_string())
}

fn write_active_tier(root: &FsPath, tier: &str) -> io::Result<()> {
    let path = writing_active_path(root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, json!({"tier": tier}).to_string())
}

/// 当前档位目录的 SKILL.md meta；无该档 SKILL.md 时为 None。
fn active_skill_meta(root: &FsPath, tier: &str) -> Option<Value> {
    let dir = root.join(crate::skill_layer::WRITING_NS).join(tier);
    if !dir.join("SKILL.md").is_file() {
        return None;
    }
    let mut v = parse_skill_dir(&dir)?;
    if let Some(obj) = v.as_object_mut() {
        obj.insert(
            "path".into(),
            json!(format!("skills/{}/{}", crate::skill_layer::WRITING_NS, tier)),
        );
    }
    Some(v)
}

/// GET 同一结构：`tier`（持久化当前档位，未持久化默认 lite，default=true）+ 可选 `skill` meta。
fn active_skill_payload(root: &FsPath) -> Value {
    let persisted = read_active_tier(root);
    let (tier, default) = match persisted.as_deref() {
        Some(t) if parse_writing_tier(t).is_some() => (t.to_string(), false),
        _ => ("lite".to_string(), true),
    };
    let mut payload = json!({"tier": tier, "default": default});
    if let Some(skill) = active_skill_meta(root, &tier) {
        if let Some(obj) = payload.as_object_mut() {
            obj.insert("skill".into(), skill);
        }
    }
    payload
}

async fn get_active_skill(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    Json(active_skill_payload(&skills_root(&state))).into_response()
}

async fn put_active_skill(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<PutActiveSkillBody>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let raw = body.tier.or(body.quality).unwrap_or_default();
    let Some(tier) = parse_writing_tier(&raw) else {
        return bad_request("SKILL_INVALID", "invalid tier, must be one of lite/standard/heavy");
    };
    let root = skills_root(&state);
    if let Err(e) = write_active_tier(&root, tier) {
        return internal("SKILL_INTERNAL", e.to_string());
    }
    Json(active_skill_payload(&root)).into_response()
}

/// Resolve skill import source under `$KALEIDO_DATA` only (no host FS).
fn resolve_skill_import_src(data_root: &FsPath, rel_or_under: &str) -> Result<PathBuf, String> {
    let raw = rel_or_under.trim();
    if raw.is_empty() {
        return Err("empty path".into());
    }
    // Reject obvious escapes / absolute paths outside data root intent
    if raw.starts_with('/') || raw.starts_with('\\') || raw.contains('\0') {
        // Absolute: only allow if already under data_root after canonicalize
        let cand = PathBuf::from(raw);
        return ensure_under_data_root(data_root, &cand).map(|_| cand);
    }
    if raw.split(['/', '\\']).any(|p| p == "..") {
        return Err("path escape (..) not allowed".into());
    }
    let cand = data_root.join(raw.trim_start_matches("./"));
    ensure_under_data_root(data_root, &cand)?;
    Ok(cand)
}

fn ensure_under_data_root(data_root: &FsPath, path: &FsPath) -> Result<(), String> {
    let root = fs::canonicalize(data_root).map_err(|e| format!("data root: {e}"))?;
    // If path does not exist yet, walk up to deepest existing ancestor
    let mut probe = path.to_path_buf();
    let mut missing = Vec::new();
    while !probe.exists() {
        match probe.file_name() {
            Some(name) => {
                missing.push(name.to_os_string());
                probe = probe
                    .parent()
                    .ok_or_else(|| "path escape".to_string())?
                    .to_path_buf();
            }
            None => return Err("path escape".into()),
        }
    }
    let mut resolved = fs::canonicalize(&probe).map_err(|e| format!("canonicalize: {e}"))?;
    if !resolved.starts_with(&root) {
        return Err("path must stay under data root".into());
    }
    for part in missing.into_iter().rev() {
        let s = part.to_string_lossy();
        if s == ".." || s == "." || s.contains('\0') {
            return Err("invalid path component".into());
        }
        resolved.push(part);
        if !resolved.starts_with(&root) {
            return Err("path must stay under data root".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_skill_dir_new_keys() {
        let dir = std::env::temp_dir().join(format!("skills-parse-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("SKILL.md"),
            "---\nname: novel-heavy\ndescription: 复杂剧情\nkind: writing\ntier: heavy\nagent: ide\nparents: [\"tavern\"]\n---\n正文",
        )
        .unwrap();
        let v = parse_skill_dir(&dir).expect("parse ok");
        assert_eq!(v["name"], "novel-heavy");
        assert_eq!(v["kind"], "writing");
        assert_eq!(v["tier"], "heavy");
        assert_eq!(v["agent"], "ide");
        assert_eq!(v["parents"][0], "tavern");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_parse_skill_dir_tier_infer_from_writing_dir() {
        let root = std::env::temp_dir().join(format!("skills-infer-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        // skills/writing/standard/SKILL.md
        let dir = root.join("skills").join("writing").join("standard");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("SKILL.md"),
            "---\nname: novel-standard\ndescription: d\n---\n正文",
        )
        .unwrap();
        let v = parse_skill_dir(&dir).expect("parse ok");
        assert_eq!(v["tier"], "standard", "tier 缺省时由 skills/writing/ 子目录名推断");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn test_sanitize_name_rejects_path_escape() {
        assert!(sanitize_name("..").is_none());
        assert!(sanitize_name("a/b").is_none());
        assert!(sanitize_name("my-skill").is_some());
    }

    fn active_temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("skills-active-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn test_active_default_lite() {
        let root = active_temp_root("default");
        let skills = root.join("skills");
        assert_eq!(read_active_tier(&skills), None, "未持久化时读为 None");
        let payload = active_skill_payload(&skills);
        assert_eq!(payload["tier"], "lite");
        assert_eq!(payload["default"], true, "无持久化时 default=true");
        assert!(
            !payload.as_object().unwrap().contains_key("skill"),
            "无 SKILL.md 时不返回 skill 键"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn test_active_put_and_read_heavy() {
        let root = active_temp_root("heavy");
        let skills = root.join("skills");
        write_active_tier(&skills, "heavy").unwrap();
        assert_eq!(read_active_tier(&skills).as_deref(), Some("heavy"));
        let path = writing_active_path(&skills);
        assert!(path.is_file(), "active.json 落盘至 <root>/skills/writing/active.json");
        assert!(path.to_string_lossy().contains("skills/writing/active.json"));
        let payload = active_skill_payload(&skills);
        assert_eq!(payload["tier"], "heavy");
        assert_eq!(payload["default"], false, "显式持久化后 default=false");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn test_active_invalid_tier_rejected_400_semantics() {
        assert_eq!(parse_writing_tier("lite"), Some("lite"));
        assert_eq!(parse_writing_tier("standard"), Some("standard"));
        assert_eq!(parse_writing_tier("heavy"), Some("heavy"));
        assert_eq!(parse_writing_tier("  HEAVY "), Some("heavy"), "大小写/空白归一");
        assert!(parse_writing_tier("bogus").is_none());
        assert!(parse_writing_tier("").is_none());
        assert!(parse_writing_tier("ultra").is_none());
    }

    #[test]
    fn test_active_meta_when_skill_present() {
        let root = active_temp_root("meta");
        let skills = root.join("skills");
        let dir = skills
            .join(crate::skill_layer::WRITING_NS)
            .join("heavy");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("SKILL.md"),
            "---\nname: novel-heavy\ndescription: 复杂剧情\n---\n正文",
        )
        .unwrap();
        write_active_tier(&skills, "heavy").unwrap();
        let payload = active_skill_payload(&skills);
        assert_eq!(payload["tier"], "heavy");
        assert_eq!(payload["skill"]["name"], "novel-heavy");
        assert_eq!(payload["skill"]["path"], "skills/writing/heavy");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn test_active_corrupt_persisted_falls_back_default() {
        let root = active_temp_root("corrupt");
        let skills = root.join("skills");
        let dir = skills.join(crate::skill_layer::WRITING_NS);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("active.json"), r#"{"tier":"bogus"}"#).unwrap();
        let payload = active_skill_payload(&skills);
        assert_eq!(payload["tier"], "lite", "非法持久化值回退默认 lite");
        assert_eq!(payload["default"], true);
        let _ = fs::remove_dir_all(&root);
    }
}
