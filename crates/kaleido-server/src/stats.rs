//! Stats panel (S5-W2 T7).
//! GET /api/v1/stats/interactions
//! GET /api/v1/stats/writing
//! GET /api/v1/stats/work-summary

use axum::{
    extract::State,
    http::HeaderMap,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{session_from, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/stats/interactions", get(interactions))
        .route("/api/v1/stats/writing", get(writing))
        .route("/api/v1/stats/work-summary", get(work_summary))
}

fn day_keys(now_secs: i64) -> Vec<String> {
    let today_start = now_secs - (now_secs.rem_euclid(86400));
    let mut keys = Vec::new();
    for i in (0..30).rev() {
        let day_start = today_start - i * 86400;
        let date_str = chrono::DateTime::from_timestamp(day_start, 0)
            .map(|dt| dt.format("%Y-%m-%d").to_string())
            .unwrap_or_default();
        keys.push(date_str);
    }
    keys
}

fn bump_day(map: &mut HashMap<String, usize>, secs: i64) {
    let day_start = secs - secs.rem_euclid(86400);
    if let Some(date_str) = chrono::DateTime::from_timestamp(day_start, 0)
        .map(|dt| dt.format("%Y-%m-%d").to_string())
    {
        *map.entry(date_str).or_insert(0) += 1;
    }
}

fn file_mtime_secs(p: &Path) -> Option<i64> {
    let meta = fs::metadata(p).ok()?;
    let modified = meta.modified().ok()?;
    let d = modified.duration_since(UNIX_EPOCH).ok()?;
    Some(d.as_secs() as i64)
}

fn count_words(content: &str) -> usize {
    // CJK chars count as 1; latin words by whitespace
    let mut n = 0usize;
    let mut in_word = false;
    for ch in content.chars() {
        if ch.is_ascii_alphanumeric() {
            if !in_word {
                n += 1;
                in_word = true;
            }
        } else {
            in_word = false;
            if !ch.is_whitespace() && !ch.is_ascii_punctuation() {
                // CJK / other
                n += 1;
            }
        }
    }
    n
}

fn walk_text_files(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = fs::read_dir(root) else {
        return;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            walk_text_files(&path, out);
        } else if path.is_file() {
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if matches!(
                ext.as_str(),
                "md" | "txt" | "markdown" | "json" | "html" | "csv"
            ) {
                out.push(path);
            }
        }
    }
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

async fn interactions(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let now = now_secs();
    let keys = day_keys(now);
    let mut by_day: HashMap<String, usize> = HashMap::new();
    for k in &keys {
        by_day.insert(k.clone(), 0);
    }

    // Count agent session files + chat session mtimes as interactions
    let data = state.auth.data_root().root().to_path_buf();
    for sub in ["agent_sessions", "sessions", "jobs"] {
        let dir = data.join(sub);
        if !dir.is_dir() {
            continue;
        }
        if let Ok(rd) = fs::read_dir(&dir) {
            for entry in rd.flatten() {
                let p = entry.path();
                if p.extension().and_then(|e| e.to_str()) == Some("json") {
                    if let Some(secs) = file_mtime_secs(&p) {
                        if secs >= now - 30 * 86400 {
                            bump_day(&mut by_day, secs);
                        }
                    }
                }
            }
        }
    }

    let series: Vec<Value> = keys
        .iter()
        .map(|k| json!({"date": k, "count": by_day.get(k).copied().unwrap_or(0)}))
        .collect();
    let total: usize = series
        .iter()
        .filter_map(|v| v.get("count").and_then(|c| c.as_u64()).map(|n| n as usize))
        .sum();
    Json(json!({
        "ok": true,
        "days": 30,
        "total": total,
        "series": series,
    }))
    .into_response()
}

async fn writing(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let sess = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let now = now_secs();
    let keys = day_keys(now);
    let mut words_by_day: HashMap<String, usize> = HashMap::new();
    let mut files_by_day: HashMap<String, usize> = HashMap::new();
    for k in &keys {
        words_by_day.insert(k.clone(), 0);
        files_by_day.insert(k.clone(), 0);
    }

    let mut files = Vec::new();
    if let Ok(root) = state.works.workspace_root(&sess.workspace_id) {
        walk_text_files(&root, &mut files);
    }

    let mut total_words = 0usize;
    let mut total_files = 0usize;
    for f in &files {
        // skip internal version store
        if f.components().any(|c| c.as_os_str() == ".kaleido-versions") {
            continue;
        }
        total_files += 1;
        let content = fs::read_to_string(f).unwrap_or_default();
        let wc = count_words(&content);
        total_words += wc;
        if let Some(secs) = file_mtime_secs(f) {
            if secs >= now - 30 * 86400 {
                let day_start = secs - secs.rem_euclid(86400);
                if let Some(date_str) = chrono::DateTime::from_timestamp(day_start, 0)
                    .map(|dt| dt.format("%Y-%m-%d").to_string())
                {
                    *words_by_day.entry(date_str.clone()).or_insert(0) += wc;
                    *files_by_day.entry(date_str).or_insert(0) += 1;
                }
            }
        }
    }

    let series: Vec<Value> = keys
        .iter()
        .map(|k| {
            json!({
                "date": k,
                "words": words_by_day.get(k).copied().unwrap_or(0),
                "files": files_by_day.get(k).copied().unwrap_or(0),
            })
        })
        .collect();

    Json(json!({
        "ok": true,
        "days": 30,
        "totalWords": total_words,
        "totalFiles": total_files,
        "wordCount": total_words,
        "series": series,
    }))
    .into_response()
}

async fn work_summary(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let sess = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let mut files = Vec::new();
    let mut root_str = String::new();
    if let Ok(root) = state.works.workspace_root(&sess.workspace_id) {
        root_str = root.to_string_lossy().to_string();
        walk_text_files(&root, &mut files);
    }
    let mut total_words = 0usize;
    let mut by_ext: HashMap<String, usize> = HashMap::new();
    let mut largest: Vec<Value> = Vec::new();
    for f in &files {
        if f.components().any(|c| c.as_os_str() == ".kaleido-versions") {
            continue;
        }
        let content = fs::read_to_string(f).unwrap_or_default();
        let wc = count_words(&content);
        total_words += wc;
        let ext = f
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("none")
            .to_ascii_lowercase();
        *by_ext.entry(ext).or_insert(0) += 1;
        let rel = f
            .strip_prefix(&root_str)
            .unwrap_or(f.as_path())
            .to_string_lossy()
            .trim_start_matches('/')
            .to_string();
        largest.push(json!({
            "path": rel,
            "words": wc,
            "bytes": content.len(),
        }));
    }
    largest.sort_by(|a, b| {
        b.get("words")
            .and_then(|x| x.as_u64())
            .cmp(&a.get("words").and_then(|x| x.as_u64()))
    });
    largest.truncate(10);

    // agent sessions count
    let agent_dir = state.auth.data_root().agent_sessions_dir();
    let agent_count = fs::read_dir(&agent_dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("json"))
                .count()
        })
        .unwrap_or(0);

    Json(json!({
        "ok": true,
        "workspaceId": sess.workspace_id,
        "fileCount": files.len(),
        "totalWords": total_words,
        "wordCount": total_words,
        "byExtension": by_ext,
        "largest": largest,
        "agentSessionCount": agent_count,
    }))
    .into_response()
}
