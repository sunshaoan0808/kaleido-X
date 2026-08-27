//! Reverse outline preview + save (S5-W4: optional LLM polish).
//!
//! Routes (wired in `main.rs`):
//! - `POST /api/v1/outline/reverse/preview`
//! - `POST /api/v1/outline/reverse/save`
//!
//! Persistence choice: workspace-jailed WorksFs under
//! `$KALEIDO_DATA/works/{workspace_id}/outline/`.
//!
//! Default: heuristic chapter split. When `useLlm=true`, polish chapter
//! summaries via OpenAI-compatible chat/completions; fail soft to heuristic.

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use kaleido_core::WorksFileBody;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path as StdPath;
use std::time::Duration as StdDuration;

use crate::{map_core_err, session_from, AppState};
use crate::error_codes::*;

// ---------------------------------------------------------------------------
// Request / response models
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReverseOutlinePreviewRequest {
    /// Full novel / article text (primary input for MVP heuristic preview).
    #[serde(default)]
    pub text: Option<String>,
    /// Optional relative works paths (md/txt) inside the workspace jail.
    #[serde(default)]
    pub file_paths: Option<Vec<String>>,
    /// Optional title hint for the outline document.
    #[serde(default)]
    pub title: Option<String>,
    /// When true, polish chapter summaries via configured LLM (soft-fail).
    #[serde(default)]
    pub use_llm: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutlineChapter {
    pub index: usize,
    pub title: String,
    /// Short excerpt / heuristic summary (not LLM-polished).
    pub summary: String,
    pub char_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReverseOutlinePreviewResponse {
    pub title: String,
    /// `"heuristic"` until LLM reverse analysis lands.
    pub mode: String,
    pub chapters: Vec<OutlineChapter>,
    /// Markdown outline suitable for save.
    pub outline_markdown: String,
    pub total_chars: usize,
    pub note: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReverseOutlineSaveRequest {
    pub title: String,
    pub content: String,
    /// Optional relative path under works jail; default `outline/{title}.md`.
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReverseOutlineSaveResponse {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    pub workspace_id: String,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `POST /api/v1/outline/reverse/preview`
pub async fn preview_reverse(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ReverseOutlinePreviewRequest>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };

    let mut source_chunks: Vec<(Option<String>, String, Option<String>)> = Vec::new();
    // (title_hint, content, path)

    if let Some(paths) = body.file_paths.as_ref() {
        for rel in paths {
            let rel = rel.trim();
            if rel.is_empty() {
                continue;
            }
            match state.works.read_text(&session.workspace_id, rel) {
                Ok(file) => {
                    let title = title_from_rel(&file.path);
                    source_chunks.push((Some(title), file.content, Some(file.path)));
                }
                Err(e) => return map_core_err(e),
            }
        }
    }

    if let Some(text) = body.text.as_ref() {
        if !text.trim().is_empty() {
            source_chunks.push((body.title.clone(), text.clone(), None));
        }
    }

    if source_chunks.is_empty() {
        return err_with_code(
            StatusCode::BAD_REQUEST,
            "OUT_MISSING_FIELD", "text or filePaths required",
            serde_json::json!({"hint": "Provide novel text and/or workspace-relative md/txt paths"}),
        );
    }

    let mut chapters: Vec<OutlineChapter> = Vec::new();
    let mut total_chars: usize = 0;
    let mut index = 1usize;

    for (title_hint, content, path) in source_chunks {
        total_chars = total_chars.saturating_add(content.chars().count());
        let parts = heuristic_split_chapters(&content);
        if parts.is_empty() {
            let summary = excerpt(&content, 240);
            chapters.push(OutlineChapter {
                index,
                title: title_hint
                    .filter(|t| !t.trim().is_empty())
                    .unwrap_or_else(|| format!("段落 {index}")),
                summary,
                char_count: content.chars().count(),
                path: path.clone(),
            });
            index += 1;
            continue;
        }
        for (title, body_text) in parts {
            let title = if title.trim().is_empty() {
                title_hint
                    .clone()
                    .filter(|t| !t.trim().is_empty())
                    .unwrap_or_else(|| format!("第{index}章"))
            } else {
                title
            };
            chapters.push(OutlineChapter {
                index,
                title,
                summary: excerpt(&body_text, 240),
                char_count: body_text.chars().count(),
                path: path.clone(),
            });
            index += 1;
        }
    }

    let doc_title = body
        .title
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .or_else(|| chapters.first().map(|c| c.title.clone()))
        .unwrap_or_else(|| "反向大纲".into());

    let want_llm = body.use_llm.unwrap_or(false);
    let mut mode = "heuristic".to_string();
    let mut note =
        "heuristic chapter split (no LLM). Pass useLlm=true for optional polish.".to_string();

    if want_llm && !chapters.is_empty() {
        match polish_chapters_llm(&state, &doc_title, &mut chapters).await {
            Ok(()) => {
                mode = "heuristic+llm".into();
                note = "heuristic split + LLM summary polish".into();
            }
            Err(e) => {
                note = format!("heuristic only; LLM polish failed: {e}");
            }
        }
    }

    let outline_markdown = render_outline_markdown(&doc_title, &chapters, &mode);

    Json(ReverseOutlinePreviewResponse {
        title: doc_title,
        mode,
        chapters,
        outline_markdown,
        total_chars,
        note,
    })
    .into_response()
}

/// `POST /api/v1/outline/reverse/save`
///
/// Writes under WorksFs jail: `outline/{safe_title}.md` by default.
pub async fn save_reverse(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ReverseOutlineSaveRequest>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };

    if body.content.trim().is_empty() {
        return bad_request("OUT_MISSING_FIELD", "content required");
    }

    let safe_title = match sanitize_title(&body.title) {
        Ok(t) => t,
        Err(msg) => {
            return bad_request("OUT_BAD_REQUEST", msg);
        }
    };

    // Ensure outline/ directory exists inside the workspace jail.
    if let Err(e) = state.works.mkdir(&session.workspace_id, "outline") {
        // mkdir is create_dir_all; only surface real failures.
        // If path exists as a file, that is a real error.
        match e {
            kaleido_core::CoreError::BadRequest(ref msg)
                if msg.contains("exists") || msg.contains("directory") => {}
            other => {
                // Ignore "already exists"-style noise by checking stat.
                if state.works.stat(&session.workspace_id, "outline").is_err() {
                    return map_core_err(other);
                }
            }
        }
    }

    let rel = if let Some(p) = body.path.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        // Force under outline/ for safety unless caller already prefixes it.
        let p = p.trim_start_matches('/');
        if p.starts_with("outline/") || p == "outline" {
            p.to_string()
        } else {
            format!("outline/{p}")
        }
    } else {
        unique_outline_rel(&state, &session.workspace_id, &safe_title)
    };

    // Ensure parent dirs if nested under outline/
    if let Some(parent) = StdPath::new(&rel).parent() {
        let parent_s = parent.to_string_lossy();
        if !parent_s.is_empty() && parent_s != "." {
            if let Err(e) = state.works.mkdir(&session.workspace_id, &parent_s) {
                if state
                    .works
                    .stat(&session.workspace_id, &parent_s)
                    .is_err()
                {
                    return map_core_err(e);
                }
            }
        }
    }

    match state
        .works
        .write_text(&session.workspace_id, &rel, &body.content)
    {
        Ok(WorksFileBody { path, size, .. }) => Json(ReverseOutlineSaveResponse {
            path,
            size,
            workspace_id: session.workspace_id,
        })
        .into_response(),
        Err(e) => map_core_err(e),
    }
}

// ---------------------------------------------------------------------------
// Heuristic helpers
// ---------------------------------------------------------------------------

fn title_from_rel(path: &str) -> String {
    StdPath::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "untitled".into())
}

fn sanitize_title(title: &str) -> Result<String, String> {
    let sanitized = title
        .trim()
        .chars()
        .map(|ch| match ch {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => ' ',
            _ => ch,
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if sanitized.is_empty() {
        return Err("title required".into());
    }
    Ok(sanitized)
}

fn unique_outline_rel(state: &AppState, workspace_id: &str, safe_title: &str) -> String {
    let base = format!("outline/{safe_title}.md");
    if state.works.stat(workspace_id, &base).is_err() {
        return base;
    }
    for i in 2..1000 {
        let candidate = format!("outline/{safe_title} {i}.md");
        if state.works.stat(workspace_id, &candidate).is_err() {
            return candidate;
        }
    }
    format!("outline/{safe_title}-dup.md")
}

fn excerpt(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    let count = trimmed.chars().count();
    if count <= max_chars {
        return trimmed.to_string();
    }
    let mut out: String = trimmed.chars().take(max_chars).collect();
    out.push('…');
    out
}

/// Split text into (title, body) chapters.
/// Recognizes:
/// - Markdown ATX headings (`#` / `##` / `###`)
/// - Chinese chapter markers: `第…章/回/节/部`
/// - `Chapter N` / `CHAPTER N`
/// Fallback: single chapter with whole text, or blank-line paragraph packs
/// when the document is long and has no markers.
fn heuristic_split_chapters(text: &str) -> Vec<(String, String)> {
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return Vec::new();
    }

    let mut chapters: Vec<(String, String)> = Vec::new();
    let mut current_title: Option<String> = None;
    let mut current_body: Vec<&str> = Vec::new();
    let mut saw_marker = false;

    for line in &lines {
        if let Some(title) = detect_chapter_marker(line) {
            saw_marker = true;
            if current_title.is_some() || !current_body.is_empty() {
                let body = current_body.join("\n");
                chapters.push((
                    current_title.take().unwrap_or_else(|| "前言".into()),
                    body,
                ));
                current_body.clear();
            }
            current_title = Some(title);
        } else {
            current_body.push(line);
        }
    }

    if current_title.is_some() || !current_body.is_empty() {
        chapters.push((
            current_title.unwrap_or_else(|| {
                if saw_marker {
                    "正文".into()
                } else {
                    String::new()
                }
            }),
            current_body.join("\n"),
        ));
    }

    if saw_marker {
        return chapters
            .into_iter()
            .map(|(t, b)| (t, b.trim().to_string()))
            .filter(|(_, b)| !b.is_empty() || true)
            .collect();
    }

    // No markers: if short, one chapter; if long, pack by blank-line blocks (~4k chars).
    let total = text.chars().count();
    if total <= 4_000 {
        return vec![(String::new(), text.trim().to_string())];
    }

    let mut packs: Vec<(String, String)> = Vec::new();
    let mut buf = String::new();
    let mut pack_idx = 1usize;
    for para in text.split("\n\n") {
        if buf.chars().count() + para.chars().count() > 4_000 && !buf.is_empty() {
            packs.push((format!("段落组 {pack_idx}"), buf.trim().to_string()));
            pack_idx += 1;
            buf.clear();
        }
        if !buf.is_empty() {
            buf.push_str("\n\n");
        }
        buf.push_str(para);
    }
    if !buf.trim().is_empty() {
        packs.push((format!("段落组 {pack_idx}"), buf.trim().to_string()));
    }
    packs
}

fn detect_chapter_marker(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Markdown headings
    if let Some(rest) = trimmed.strip_prefix("### ") {
        let t = rest.trim();
        if !t.is_empty() {
            return Some(t.to_string());
        }
    }
    if let Some(rest) = trimmed.strip_prefix("## ") {
        let t = rest.trim();
        if !t.is_empty() {
            return Some(t.to_string());
        }
    }
    if let Some(rest) = trimmed.strip_prefix("# ") {
        let t = rest.trim();
        if !t.is_empty() {
            return Some(t.to_string());
        }
    }

    // Chinese: 第X章 / 第X回 / 第X节 / 第X部 (optionally with title after)
    if trimmed.starts_with('第') {
        let chars: Vec<char> = trimmed.chars().collect();
        if chars.len() >= 3 {
            // find 章/回/节/部 after 第
            for (i, ch) in chars.iter().enumerate().skip(1) {
                if matches!(ch, '章' | '回' | '节' | '部') {
                    // require at least one char between 第 and marker
                    if i >= 2 {
                        // title is whole line (trimmed), reasonable length
                        if trimmed.chars().count() <= 80 {
                            return Some(trimmed.to_string());
                        }
                    }
                    break;
                }
            }
        }
    }

    // Chapter N / CHAPTER N
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("chapter ") {
        if trimmed.chars().count() <= 80 {
            return Some(trimmed.to_string());
        }
    }

    None
}

fn render_outline_markdown(title: &str, chapters: &[OutlineChapter], mode: &str) -> String {
    let mut out = String::new();
    out.push_str("# ");
    out.push_str(title);
    out.push_str("\n\n");
    if mode.contains("llm") {
        out.push_str("> 由 reverse-outline 启发式拆章 + LLM 摘要润色生成。\n\n");
    } else {
        out.push_str("> 由 reverse-outline 启发式拆章生成（无 LLM）。\n\n");
    }
    for ch in chapters {
        out.push_str(&format!("## {}. {}\n\n", ch.index, ch.title));
        out.push_str(&format!("- 字数: {}\n", ch.char_count));
        if let Some(p) = &ch.path {
            out.push_str(&format!("- 来源: `{p}`\n"));
        }
        out.push_str("\n");
        out.push_str(&ch.summary);
        out.push_str("\n\n");
    }
    out
}

/// Batch-polish up to 8 chapter summaries in one LLM call. Soft-fail.
async fn polish_chapters_llm(
    state: &AppState,
    doc_title: &str,
    chapters: &mut [OutlineChapter],
) -> Result<(), String> {
    let llm = state.app_state.resolve_llm(
        state.llm_base.as_deref(),
        state.llm_key.as_deref(),
        &state.llm_model,
    );
    if llm.base_url.trim().is_empty() || llm.api_key.trim().is_empty() {
        return Err("llm not configured".into());
    }
    let model = if llm.model.is_empty() {
        state.llm_model.clone()
    } else {
        llm.model.clone()
    };

    // Cap chapters to keep prompt small / gate-friendly.
    let n = chapters.len().min(8);
    let mut payload = String::new();
    for ch in chapters.iter().take(n) {
        payload.push_str(&format!(
            "[{}] {}\n摘要草稿: {}\n字数: {}\n\n",
            ch.index, ch.title, ch.summary, ch.char_count
        ));
    }

    let system = "你是中文小说大纲编辑。根据给定章节标题与摘要草稿，为每章输出更凝练的剧情要点（1-2句，保留关键人物/冲突/转折，去掉套话）。\
只输出 JSON 数组，形如 [{\"index\":1,\"summary\":\"...\"}, ...]，不要其它文字。".to_string();
    let user = format!("作品标题：{doc_title}\n\n章节列表：\n{payload}");

    let client = reqwest::Client::builder()
        .timeout(StdDuration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?;
    let url = format!(
        "{}/chat/completions",
        llm.base_url.trim_end_matches('/')
    );
    let body = json!({
        "model": model,
        "stream": false,
        "temperature": 0.3,
        "max_tokens": 1200,
        "messages": [
            {"role":"system","content": system},
            {"role":"user","content": user},
        ],
    });
    let resp = client
        .post(&url)
        .bearer_auth(&llm.api_key)
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("connect: {e}"))?;
    if !resp.status().is_success() {
        let st = resp.status();
        let t = resp.text().await.unwrap_or_default();
        return Err(format!(
            "upstream {st}: {}",
            t.chars().take(200).collect::<String>()
        ));
    }
    let v: Value = resp.json().await.map_err(|e| e.to_string())?;
    let content = v
        .pointer("/choices/0/message/content")
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if content.is_empty() {
        return Err("empty llm content".into());
    }

    // Extract JSON array even if fenced.
    let json_text = extract_json_array(&content).ok_or_else(|| {
        format!(
            "no json array in llm output: {}",
            content.chars().take(120).collect::<String>()
        )
    })?;
    let arr: Vec<Value> = serde_json::from_str(&json_text).map_err(|e| e.to_string())?;
    for item in arr {
        let idx = item
            .get("index")
            .and_then(|x| x.as_u64())
            .or_else(|| item.get("index").and_then(|x| x.as_i64()).map(|i| i as u64));
        let summary = item
            .get("summary")
            .and_then(|x| x.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());
        if let (Some(idx), Some(summary)) = (idx, summary) {
            if let Some(ch) = chapters.iter_mut().find(|c| c.index as u64 == idx) {
                ch.summary = summary.to_string();
            }
        }
    }
    Ok(())
}

fn extract_json_array(s: &str) -> Option<String> {
    let t = s.trim();
    if let Some(start) = t.find('[') {
        if let Some(end_rel) = t[start..].rfind(']') {
            return Some(t[start..start + end_rel + 1].to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_chinese_chapters() {
        let text = "第1章 开端\n从前有座山\n\n第2章 发展\n山里有座庙\n";
        let parts = heuristic_split_chapters(text);
        assert!(parts.len() >= 2, "{parts:?}");
        assert!(parts[0].0.contains("第1章"));
        assert!(parts[1].0.contains("第2章"));
    }

    #[test]
    fn splits_markdown_headings() {
        let text = "# Intro\nhello\n\n## Rising\nworld\n";
        let parts = heuristic_split_chapters(text);
        assert!(parts.len() >= 2, "{parts:?}");
        assert_eq!(parts[0].0, "Intro");
    }

    #[test]
    fn sanitize_rejects_empty() {
        assert!(sanitize_title("   ").is_err());
        assert_eq!(sanitize_title("a/b:c").unwrap(), "a b c");
    }
}


/// S7-W4: multi-chapter score summary (soft LLM / heuristic).
pub async fn analyze_reverse(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ReverseOutlinePreviewRequest>,
) -> Response {
    // M-7 → S7-W4 落地（2026-08-26）：LLM 反推分析（软失败回启发式）。
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };

    let mut source_chunks: Vec<(Option<String>, String, Option<String>)> = Vec::new();
    if let Some(paths) = body.file_paths.as_ref() {
        for rel in paths {
            let rel = rel.trim();
            if rel.is_empty() {
                continue;
            }
            match state.works.read_text(&session.workspace_id, rel) {
                Ok(file) => {
                    let title = title_from_rel(&file.path);
                    source_chunks.push((Some(title), file.content, Some(file.path)));
                }
                Err(e) => return map_core_err(e),
            }
        }
    }
    if let Some(text) = body.text.as_ref() {
        if !text.trim().is_empty() {
            source_chunks.push((body.title.clone(), text.clone(), None));
        }
    }
    if source_chunks.is_empty() {
        return err_with_code(
            StatusCode::BAD_REQUEST,
            "OUT_MISSING_FIELD", "text or filePaths required",
            serde_json::json!({"hint": "Provide novel text and/or workspace-relative md/txt paths"}),
        );
    }

    // 与 preview 相同的拆章逻辑；analyze 的差异在「逐章要点 + 全书结构」深度分析。
    let mut chapters: Vec<OutlineChapter> = Vec::new();
    let mut total_chars: usize = 0;
    let mut index = 1usize;
    for (title_hint, content, path) in source_chunks {
        total_chars = total_chars.saturating_add(content.chars().count());
        let parts = heuristic_split_chapters(&content);
        if parts.is_empty() {
            let summary = excerpt(&content, 240);
            chapters.push(OutlineChapter {
                index,
                title: title_hint
                    .filter(|t| !t.trim().is_empty())
                    .unwrap_or_else(|| format!("段落 {index}")),
                summary,
                char_count: content.chars().count(),
                path: path.clone(),
            });
            index += 1;
            continue;
        }
        for (title, body_text) in parts {
            let title = if title.trim().is_empty() {
                title_hint
                    .clone()
                    .filter(|t| !t.trim().is_empty())
                    .unwrap_or_else(|| format!("第{index}章"))
            } else {
                title
            };
            chapters.push(OutlineChapter {
                index,
                title,
                summary: excerpt(&body_text, 240),
                char_count: body_text.chars().count(),
                path: path.clone(),
            });
            index += 1;
        }
    }
    if chapters.is_empty() {
        return bad_request("OUT_BAD_REQUEST", "no analyzable chapter content");
    }

    let doc_title = body
        .title
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .or_else(|| chapters.first().map(|c| c.title.clone()))
        .unwrap_or_else(|| "反向大纲".into());

    let want_llm = body.use_llm.unwrap_or(false);
    let llm = state.app_state.resolve_llm(
        state.llm_base.as_deref(),
        state.llm_key.as_deref(),
        &state.llm_model,
    );
    let prov_kind = crate::llm_stream::runtime_provider_kind(&llm, &state.provider_kind);
    let llm_ready =
        want_llm && !llm.base_url.trim().is_empty() && !llm.api_key.trim().is_empty();

    if llm_ready {
        match analyze_chapters_llm(&state, &llm, &prov_kind, &doc_title, &chapters).await {
            Ok(analysis) => {
                tracing::info!(title = %doc_title, chapters = chapters.len(), "outline reverse analyze: LLM 分析成功");
                Json(json!({
                    "ok": true,
                    "step": "analyze",
                    "generationMode": "llm",
                    "fallback": false,
                    "title": doc_title,
                    "chaptersAnalyzed": analysis.chapter_count,
                    "analysis": analysis.payload,
                    "next": "POST /api/v1/outline/reverse/save",
                }))
                .into_response()
            }
            Err(e) => {
                tracing::warn!(error = %e, title = %doc_title, "outline reverse analyze: LLM 失败，回退启发式");
                heuristic_analysis(doc_title, chapters, total_chars, e)
            }
        }
    } else {
        let note = if want_llm {
            "useLlm=true 但 LLM 未配置，使用启发式分析".to_string()
        } else {
            "heuristic analysis (pass useLlm=true for LLM)".to_string()
        };
        heuristic_analysis_with_note(doc_title, chapters, total_chars, note)
    }
}

struct ReverseAnalysis {
    chapter_count: usize,
    payload: Value,
}

/// LLM 反推分析：逐章剧情要点 + 人物/主题/结构。JSON 输出，预算受控。
async fn analyze_chapters_llm(
    _state: &AppState,
    llm: &kaleido_core::LlmRuntime,
    prov_kind: &str,
    doc_title: &str,
    chapters: &[OutlineChapter],
) -> Result<ReverseAnalysis, String> {
    // 章节上限与 preview polish 一致（gate 友好）；每章摘要按 400 字符预算截断。
    const MAX_CHAPTERS: usize = 8;
    let mut payload = String::new();
    for ch in chapters.iter().take(MAX_CHAPTERS) {
        payload.push_str(&format!(
            "[{}] {}\n摘要草稿: {}\n字数: {}\n\n",
            ch.index,
            ch.title,
            ch.summary.chars().take(400).collect::<String>(),
            ch.char_count
        ));
    }
    let system = "你是中文小说结构分析师。基于给定章节标题与摘要草稿输出反向大纲分析。\
只输出 JSON 对象：{\"premise\":\"一句话全书前提\",\"structure\":\"起承转合结构描述\",\"chapters\":[{\"index\":1,\"summary\":\"该章剧情要点(1-2句)\"}],\"characters\":[\"主要人物名\"],\"themes\":[\"主题关键词\"]}。不要其它文字。"
        .to_string();
    let user = format!("作品标题：{doc_title}\n\n章节列表：\n{payload}");

    let client = reqwest::Client::builder()
        .timeout(StdDuration::from_secs(150))
        .build()
        .map_err(|e| e.to_string())?;
    let text = crate::llm_stream::chat_completion_dispatch(
        &llm.base_url,
        &llm.api_key,
        &llm.model,
        prov_kind,
        &system,
        &user,
        0.3,
        2000,
        150,
        &client,
    )
    .await?;
    if text.trim().is_empty() {
        return Err("empty llm content".into());
    }
    let v = crate::llm_stream::extract_json_value(&text)
        .ok_or_else(|| format!("no json object in llm output: {}", text.chars().take(120).collect::<String>()))?;
    if v.get("chapters").is_none() {
        return Err("llm output missing 'chapters'".into());
    }
    let n = v["chapters"].as_array().map(|a| a.len()).unwrap_or(0);
    Ok(ReverseAnalysis { chapter_count: n, payload: v })
}

fn heuristic_analysis(
    doc_title: String,
    chapters: Vec<OutlineChapter>,
    total_chars: usize,
    fallback_reason: String,
) -> Response {
    heuristic_analysis_with_note(doc_title, chapters, total_chars, format!("启发式分析（LLM 失败回退: {fallback_reason}）"))
}

fn heuristic_analysis_with_note(
    doc_title: String,
    chapters: Vec<OutlineChapter>,
    total_chars: usize,
    note: String,
) -> Response {
    let n = chapters.len();
    let premise = if n > 0 {
        format!("共{n}章、约{total_chars}字；开篇《{}》以「{}」切入", doc_title, excerpt(&chapters[0].summary, 40))
    } else {
        "无章节内容".into()
    };
    Json(json!({
        "ok": true,
        "step": "analyze",
        "generationMode": "heuristic",
        "fallback": true,
        "title": doc_title,
        "chaptersAnalyzed": n,
        "analysis": {
            "premise": premise,
            "structure": format!("{} 章，总计 {total_chars} 字符", n),
            "chapters": chapters.iter().map(|c| serde_json::json!({
                "index": c.index, "summary": c.summary
            })).collect::<Vec<_>>(),
            "characters": [],
            "themes": [],
        },
        "note": note,
        "next": "POST /api/v1/outline/reverse/save",
    }))
    .into_response()
}

/// S7-W4: finalize marker (retry + optional save hint).
pub async fn finalize_reverse(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ReverseOutlinePreviewRequest>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let text = body.text.clone().unwrap_or_default();
    Json(json!({
        "ok": true,
        "step": "finalize",
        "readyToSave": !text.trim().is_empty() || body.use_llm.unwrap_or(false),
        "fallback": false,
        "generationMode": if body.use_llm.unwrap_or(false) {"llm"} else {"heuristic"},
        "next": "POST /api/v1/outline/reverse/save",
    })).into_response()
}
