//! 书架改编模式 A1: 章节文档实体 + CRUD（Scriverse 三件套的地基）。
//!
//! 章节存 `projects/{pid}/chapters/{chId}.json`（WorksFs jail）：
//! ```json
//! {"id":"ch1","title":"第一章","content":"正文…","versionNo":3,"updatedAt":"…","source":"manual|crawler"}
//! ```
//! Routes（挂 author router 旁，见 `suggest_router()`）：
//! - `GET    /api/v1/author/projects/{id}/chapters` — 列表
//! - `POST   /api/v1/author/projects/{id}/chapters` — 新建 `{title?, content?}`
//! - `GET    /api/v1/author/projects/{id}/chapters/{ch}` — 读
//! - `PUT    /api/v1/author/projects/{id}/chapters/{ch}` — 写正文（versionNo+1）
//! - `DELETE /api/v1/author/projects/{id}/chapters/{ch}` — 删

use axum::{
    extract::{Path, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{map_core_err, session_from, AppState};
use crate::error_codes::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChapterDoc {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub content: String,
    #[serde(default = "one")]
    pub version_no: u32,
    #[serde(default)]
    pub updated_at: String,
    #[serde(default = "manual_src")]
    pub source: String,
}

fn one() -> u32 {
    1
}
fn manual_src() -> String {
    "manual".into()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NewChapterBody {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PutChapterBody {
    content: String,
}

fn now() -> String {
    Utc::now().to_rfc3339()
}

fn safe_id(id: &str) -> Result<String, Response> {
    let s = id.trim();
    if s.is_empty() || s.contains('/') || s.contains('\\') || s.contains("..") || s.chars().any(|c| c.is_control()) {
        return Err(bad_request("CHAPTER_BAD_ID", "invalid id"));
    }
    Ok(s.to_string())
}

fn ch_path(pid: &str, ch: &str) -> String {
    format!("projects/{pid}/chapters/{ch}.json")
}

fn read_ch(state: &AppState, ws: &str, pid: &str, ch: &str) -> Result<ChapterDoc, Response> {
    let body = state
        .works
        .read_text(ws, &ch_path(pid, ch))
        .map_err(|_| not_found("CHAPTER_NOT_FOUND", format!("chapter not found: {ch}")))?;
    serde_json::from_str::<ChapterDoc>(&body.content)
        .map_err(|e| internal("CHAPTER_CORRUPT", format!("corrupt chapter: {e}")))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/author/projects/{id}/chapters",
            get(list_chapters).post(create_chapter),
        )
        .route(
            "/api/v1/author/projects/{id}/chapters/{ch}",
            get(get_chapter).put(put_chapter).delete(delete_chapter),
        )
}

async fn list_chapters(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(pid): Path<String>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let pid = match safe_id(&pid) {
        Ok(v) => v,
        Err(r) => return r,
    };
    // 扫 chapters 目录
    let mut out = Vec::new();
    let dir = format!("projects/{pid}/chapters");
    if let Ok(entry) = state.works.list(&session.workspace_id, &dir, 1) {
        for child in entry.children {
            if child.kind == "dir" {
                continue;
            }
            let rel = format!("{dir}/{}", child.name);
            if let Ok(body) = state.works.read_text(&session.workspace_id, &rel) {
                if let Ok(ch) = serde_json::from_str::<ChapterDoc>(&body.content) {
                    out.push(json!({"id": ch.id, "title": ch.title, "versionNo": ch.version_no, "updatedAt": ch.updated_at}));
                }
            }
        }
    }
    Json(json!({"ok": true, "count": out.len(), "chapters": out})).into_response()
}

async fn create_chapter(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(pid): Path<String>,
    Json(body): Json<NewChapterBody>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let pid = match safe_id(&pid) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let ch_id = format!("ch-{}", &Uuid::new_v4().to_string()[..8]);
    let ch = ChapterDoc {
        id: ch_id.clone(),
        title: body.title.unwrap_or_else(|| "未命名章节".into()),
        content: body.content.unwrap_or_default(),
        version_no: 1,
        updated_at: now(),
        source: "manual".into(),
    };
    let s = serde_json::to_string_pretty(&ch).map_err(|e| internal("CHAPTER_SERIALIZE", e.to_string()));
    let s = match s {
        Ok(v) => v,
        Err(r) => return r,
    };
    if let Err(e) = state.works.mkdir(&session.workspace_id, &format!("projects/{pid}/chapters")) {
        let _ = e;
    }
    if let Err(e) = state.works.write_text(&session.workspace_id, &ch_path(&pid, &ch_id), &s) {
        return map_core_err(e);
    }
    Json(json!({"ok": true, "chapter": ch})).into_response()
}

async fn get_chapter(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((pid, ch)): Path<(String, String)>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let (pid, ch) = match (safe_id(&pid), safe_id(&ch)) {
        (Ok(a), Ok(b)) => (a, b),
        _ => return bad_request("CHAPTER_BAD_ID", "invalid id"),
    };
    match read_ch(&state, &session.workspace_id, &pid, &ch) {
        Ok(doc) => Json(json!({"ok": true, "chapter": doc})).into_response(),
        Err(r) => r,
    }
}

async fn put_chapter(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((pid, ch)): Path<(String, String)>,
    Json(body): Json<PutChapterBody>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let (pid, ch) = match (safe_id(&pid), safe_id(&ch)) {
        (Ok(a), Ok(b)) => (a, b),
        _ => return bad_request("CHAPTER_BAD_ID", "invalid id"),
    };
    let mut doc = match read_ch(&state, &session.workspace_id, &pid, &ch) {
        Ok(d) => d,
        Err(r) => return r,
    };
    doc.content = body.content;
    doc.version_no += 1;
    doc.updated_at = now();
    let s = serde_json::to_string_pretty(&doc).map_err(|e| internal("CHAPTER_SERIALIZE", e.to_string()));
    let s = match s {
        Ok(v) => v,
        Err(r) => return r,
    };
    if let Err(e) = state.works.write_text(&session.workspace_id, &ch_path(&pid, &ch), &s) {
        return map_core_err(e);
    }
    Json(json!({"ok": true, "chapter": doc})).into_response()
}

async fn delete_chapter(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((pid, ch)): Path<(String, String)>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let (pid, ch) = match (safe_id(&pid), safe_id(&ch)) {
        (Ok(a), Ok(b)) => (a, b),
        _ => return bad_request("CHAPTER_BAD_ID", "invalid id"),
    };
    if let Err(e) = state.works.delete(&session.workspace_id, &ch_path(&pid, &ch), false) {
        return map_core_err(e);
    }
    Json(json!({"ok": true})).into_response()
}

// ── A2: 建议/守卫/采纳门禁（Scriverse 原样搬，Kaleido 化） ──────────────
// 建议存 `projects/{pid}/suggestions/{sgId}.json`：
// ```json
// {"id","projectId","chapterId","chapterVersion","taskType":"continue|polish",
//  "action":"append|replace","instruction":"","content":"","status":"pending|accepted|rejected",
//  "sourceText":"","createdAt":"…"}
// ```
// 守卫存 `projects/{pid}/guards/{sgId}.json`（最新一条）：
// ```json
// {"suggestionId","chapterVersion","contentHash","status":"clear|warning|failed",
//  "issues":[{type,severity,title,description,candidateQuote,sourceRefs,suggestion}],
//  "contextRefs":{…},"failure":""}
// ```
// Routes:
// - `POST /api/v1/author/projects/{id}/chapters/{ch}/suggest` — 生成候选（LLM，不写正文）
// - `GET  /api/v1/author/projects/{id}/suggestions` — 列表
// - `GET  /api/v1/author/projects/{id}/suggestions/{sg}` — 读（含守卫）
// - `POST /api/v1/author/projects/{id}/suggestions/{sg}/guard` — 跑守卫（LLM）
// - `POST /api/v1/author/projects/{id}/suggestions/{sg}/accept` — 采纳（五道门）
// - `POST /api/v1/author/projects/{id}/suggestions/{sg}/reject` — 驳回

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Suggestion {
    pub id: String,
    pub project_id: String,
    pub chapter_id: String,
    pub chapter_version: u32,
    #[serde(default = "continue_tt")]
    pub task_type: String,
    #[serde(default = "append_ac")]
    pub action: String,
    #[serde(default)]
    pub instruction: String,
    #[serde(default)]
    pub content: String,
    #[serde(default = "pending_st")]
    pub status: String,
    #[serde(default)]
    pub source_text: String,
    #[serde(default)]
    pub created_at: String,
}

fn continue_tt() -> String {
    "continue".into()
}
fn append_ac() -> String {
    "append".into()
}
fn pending_st() -> String {
    "pending".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuardIssue {
    #[serde(rename = "type")]
    pub kind: String,
    pub severity: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub candidate_quote: String,
    #[serde(default)]
    pub source_refs: Vec<Value>,
    #[serde(default)]
    pub suggestion: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContinuationGuard {
    pub suggestion_id: String,
    pub chapter_version: u32,
    pub content_hash: String,
    pub status: String,
    #[serde(default)]
    pub issues: Vec<GuardIssue>,
    #[serde(default)]
    pub context_refs: Value,
    #[serde(default)]
    pub failure: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SuggestBody {
    #[serde(default = "continue_tt")]
    task_type: String,
    #[serde(default = "append_ac")]
    action: String,
    #[serde(default)]
    instruction: String,
    #[serde(default)]
    source_text: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AcceptBody {
    #[serde(default)]
    content: Option<String>,
}

fn sg_path(pid: &str, sg: &str) -> String {
    format!("projects/{pid}/suggestions/{sg}.json")
}
fn guard_path(pid: &str, sg: &str) -> String {
    format!("projects/{pid}/guards/{sg}.json")
}

fn hash_content(s: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    format!("{:x}", h.finish())
}

fn read_sg(state: &AppState, ws: &str, pid: &str, sg: &str) -> Result<Suggestion, Response> {
    let body = state
        .works
        .read_text(ws, &sg_path(pid, sg))
        .map_err(|_| not_found("SUGGESTION_NOT_FOUND", format!("suggestion not found: {sg}")))?;
    serde_json::from_str::<Suggestion>(&body.content)
        .map_err(|e| internal("SUGGESTION_CORRUPT", format!("corrupt suggestion: {e}")))
}

fn read_guard(state: &AppState, ws: &str, pid: &str, sg: &str) -> Option<ContinuationGuard> {
    state
        .works
        .read_text(ws, &guard_path(pid, sg))
        .ok()
        .and_then(|b| serde_json::from_str::<ContinuationGuard>(&b.content).ok())
}

async fn call_llm(state: &AppState, system: &str, user: &str) -> Result<String, String> {
    let llm = state.app_state.resolve_llm(
        state.llm_base.as_deref(),
        state.llm_key.as_deref(),
        &state.llm_model,
    );
    if llm.base_url.trim().is_empty() || llm.api_key.trim().is_empty() {
        return Err("LLM not configured".into());
    }
    let model = if llm.model.is_empty() {
        state.llm_model.clone()
    } else {
        llm.model
    };
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| e.to_string())?;
    let url = format!("{}/chat/completions", llm.base_url.trim_end_matches('/'));
    let body = json!({
        "model": model, "stream": false, "temperature": 0.7,
        "max_tokens": 4096,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user},
        ],
    });
    let resp = client
        .post(&url)
        .bearer_auth(&llm.api_key)
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("LLM request failed: {e}"))?;
    let status = resp.status();
    let text = resp.text().await.map_err(|e| format!("LLM read failed: {e}"))?;
    if !status.is_success() {
        return Err(format!("LLM HTTP {status}"));
    }
    let v: Value = serde_json::from_str(&text).map_err(|e| format!("LLM bad json: {e}"))?;
    v.pointer("/choices/0/message/content")
        .and_then(|c| c.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "LLM empty content".to_string())
}

/// 续写一致性守卫 system（照搬 Scriverse 原版口径）。
const GUARD_SYS: &str = "你是续写一致性守卫。必须逐项对照人物状态、地点、时间、世界观硬约束、章节大纲和未回收伏笔。";

fn parse_guard_issues(content: &str) -> Result<Vec<GuardIssue>, String> {
    // 提取 JSON 数组（兼容 markdown 代码块包裹）
    let s = content.trim();
    let s = s
        .strip_prefix("```json")
        .or_else(|| s.strip_prefix("```"))
        .unwrap_or(s)
        .strip_suffix("```")
        .unwrap_or(s)
        .trim();
    let start = s.find('[').ok_or("guard result must be array")?;
    let end = s.rfind(']').ok_or("guard result must be array")?;
    let arr: Vec<Value> =
        serde_json::from_str(&s[start..=end]).map_err(|e| format!("guard invalid json: {e}"))?;
    let kinds = ["character", "location", "time", "world", "outline", "foreshadow"];
    let sevs = ["low", "medium", "high"];
    let mut out = Vec::new();
    for (i, item) in arr.iter().enumerate() {
        let kind = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if !kinds.contains(&kind) {
            return Err(format!("guard item {} invalid type", i + 1));
        }
        let sev = item.get("severity").and_then(|v| v.as_str()).unwrap_or("");
        if !sevs.contains(&sev) {
            return Err(format!("guard item {} invalid severity", i + 1));
        }
        let title = item.get("title").and_then(|v| v.as_str()).unwrap_or("").trim();
        if title.is_empty() {
            return Err(format!("guard item {} missing title", i + 1));
        }
        out.push(GuardIssue {
            kind: kind.into(),
            severity: sev.into(),
            title: title.into(),
            description: item.get("description").and_then(|v| v.as_str()).unwrap_or("").into(),
            candidate_quote: item.get("candidateQuote").and_then(|v| v.as_str()).unwrap_or("").into(),
            source_refs: item.get("sourceRefs").and_then(|v| v.as_array()).cloned().unwrap_or_default(),
            suggestion: item.get("suggestion").and_then(|v| v.as_str()).unwrap_or("").into(),
        });
    }
    Ok(out)
}

pub fn suggest_router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/author/projects/{id}/chapters/{ch}/suggest",
            post(create_suggest),
        )
        .route(
            "/api/v1/author/projects/{id}/suggestions",
            get(list_suggests),
        )
        .route(
            "/api/v1/author/projects/{id}/suggestions/{sg}",
            get(get_suggest),
        )
        .route(
            "/api/v1/author/projects/{id}/suggestions/{sg}/guard",
            post(run_guard),
        )
        .route(
            "/api/v1/author/projects/{id}/suggestions/{sg}/accept",
            post(accept_suggest),
        )
        .route(
            "/api/v1/author/projects/{id}/suggestions/{sg}/reject",
            post(reject_suggest),
        )
}

async fn create_suggest(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((pid, ch)): Path<(String, String)>,
    Json(body): Json<SuggestBody>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let (pid, ch) = match (safe_id(&pid), safe_id(&ch)) {
        (Ok(a), Ok(b)) => (a, b),
        _ => return bad_request("CHAPTER_BAD_ID", "invalid id"),
    };
    let task_type = body.task_type;
    if task_type != "continue" && task_type != "polish" {
        return bad_request("SUGGEST_BAD_TASK", "taskType must be continue|polish");
    }
    let action = body.action;
    if action != "append" && action != "replace" {
        return bad_request("SUGGEST_BAD_ACTION", "action must be append|replace");
    }
    let doc = match read_ch(&state, &session.workspace_id, &pid, &ch) {
        Ok(d) => d,
        Err(r) => return r,
    };
    let instruction = body.instruction;
    let source_text = body.source_text;
    // LLM 生成候选（不写正文）
    let sys = "你是长篇小说续写助手。根据章节正文和指令生成续写/润色候选。只输出候选正文，不要解释。";
    let user = if task_type == "continue" {
        format!("章节《{}》正文：\n{}\n\n指令：{}\n\n请续写下一段（300-800字）：", doc.title, doc.content, instruction)
    } else {
        format!("章节《{}》正文：\n{}\n\n待润色选区：\n{}\n\n指令：{}\n\n请输出润色后文本：", doc.title, doc.content, source_text, instruction)
    };
    let content = match call_llm(&state, sys, &user).await {
        Ok(c) => c,
        Err(e) => return internal("SUGGEST_LLM_FAILED", e),
    };
    let sg_id = format!("sg-{}", &Uuid::new_v4().to_string()[..8]);
    let sg = Suggestion {
        id: sg_id.clone(),
        project_id: pid.clone(),
        chapter_id: ch.clone(),
        chapter_version: doc.version_no,
        task_type,
        action,
        instruction,
        content,
        status: "pending".into(),
        source_text,
        created_at: now(),
    };
    let s = serde_json::to_string_pretty(&sg).map_err(|e| internal("SUGGEST_SERIALIZE", e.to_string()));
    let s = match s {
        Ok(v) => v,
        Err(r) => return r,
    };
    let _ = state.works.mkdir(&session.workspace_id, &format!("projects/{pid}/suggestions"));
    if let Err(e) = state.works.write_text(&session.workspace_id, &sg_path(&pid, &sg_id), &s) {
        return map_core_err(e);
    }
    Json(json!({"ok": true, "suggestion": sg})).into_response()
}

async fn list_suggests(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(pid): Path<String>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let pid = match safe_id(&pid) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let mut out = Vec::new();
    let dir = format!("projects/{pid}/suggestions");
    if let Ok(entry) = state.works.list(&session.workspace_id, &dir, 1) {
        for child in entry.children {
            if child.kind == "dir" {
                continue;
            }
            let rel = format!("{dir}/{}", child.name);
            if let Ok(body) = state.works.read_text(&session.workspace_id, &rel) {
                if let Ok(sg) = serde_json::from_str::<Suggestion>(&body.content) {
                    out.push(json!({"id": sg.id, "chapterId": sg.chapter_id, "taskType": sg.task_type, "action": sg.action, "status": sg.status, "createdAt": sg.created_at}));
                }
            }
        }
    }
    Json(json!({"ok": true, "count": out.len(), "suggestions": out})).into_response()
}

async fn get_suggest(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((pid, sg)): Path<(String, String)>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let (pid, sg) = match (safe_id(&pid), safe_id(&sg)) {
        (Ok(a), Ok(b)) => (a, b),
        _ => return bad_request("SUGGEST_BAD_ID", "invalid id"),
    };
    match read_sg(&state, &session.workspace_id, &pid, &sg) {
        Ok(doc) => {
            let guard = read_guard(&state, &session.workspace_id, &pid, &sg);
            Json(json!({"ok": true, "suggestion": doc, "guard": guard})).into_response()
        }
        Err(r) => r,
    }
}

async fn run_guard(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((pid, sg)): Path<(String, String)>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let (pid, sg) = match (safe_id(&pid), safe_id(&sg)) {
        (Ok(a), Ok(b)) => (a, b),
        _ => return bad_request("SUGGEST_BAD_ID", "invalid id"),
    };
    let doc = match read_sg(&state, &session.workspace_id, &pid, &sg) {
        Ok(d) => d,
        Err(r) => return r,
    };
    // 门 0：只有续写建议可跑守卫
    if doc.task_type != "continue" {
        return Json(json!({"ok": false, "code": "GUARD_NOT_APPLICABLE", "error": "只有续写建议可以运行一致性检查"})).into_response();
    }
    let ch = match read_ch(&state, &session.workspace_id, &pid, &doc.chapter_id) {
        Ok(d) => d,
        Err(r) => return r,
    };
    // 门 1：正文版本必须一致
    if ch.version_no != doc.chapter_version {
        return Json(json!({"ok": false, "code": "STALE_SUGGESTION", "error": "正文版本已变化，请重新生成建议"})).into_response();
    }
    // contextRefs：章节版本 + 内容哈希（Kaleido 化简版，原版是人物/设定/大纲/伏笔快照）
    let ctx = json!({
        "chapterId": ch.id, "chapterVersion": ch.version_no,
        "chapterHash": hash_content(&ch.content),
    });
    let user = format!(
        "检查下面的续写候选是否与提供的上下文冲突。输出 JSON 数组，没有冲突时输出 []。\n每项字段必须为：type（character/location/time/world/outline/foreshadow）、severity（low/medium/high）、title、description、candidateQuote、sourceRefs（数组）、suggestion。\n不得把文风偏好当成事实冲突，不得使用 Markdown 代码块。\n续写候选：\n{}\n\n章节正文：\n{}",
        doc.content, ch.content
    );
    let raw = match call_llm(&state, GUARD_SYS, &user).await {
        Ok(r) => r,
        Err(e) => {
            // failed 状态落盘（原版同款）
            let g = ContinuationGuard {
                suggestion_id: sg.clone(),
                chapter_version: ch.version_no,
                content_hash: hash_content(&doc.content),
                status: "failed".into(),
                issues: vec![],
                context_refs: ctx,
                failure: e,
            };
            let s = serde_json::to_string_pretty(&g).unwrap_or_default();
            let _ = state.works.mkdir(&session.workspace_id, &format!("projects/{pid}/guards"));
            let _ = state.works.write_text(&session.workspace_id, &guard_path(&pid, &sg), &s);
            return Json(json!({"ok": true, "guard": g})).into_response();
        }
    };
    let issues = match parse_guard_issues(&raw) {
        Ok(v) => v,
        Err(e) => {
            let g = ContinuationGuard {
                suggestion_id: sg.clone(),
                chapter_version: ch.version_no,
                content_hash: hash_content(&doc.content),
                status: "failed".into(),
                issues: vec![],
                context_refs: ctx,
                failure: e,
            };
            let s = serde_json::to_string_pretty(&g).unwrap_or_default();
            let _ = state.works.mkdir(&session.workspace_id, &format!("projects/{pid}/guards"));
            let _ = state.works.write_text(&session.workspace_id, &guard_path(&pid, &sg), &s);
            return Json(json!({"ok": true, "guard": g})).into_response();
        }
    };
    let g = ContinuationGuard {
        suggestion_id: sg.clone(),
        chapter_version: ch.version_no,
        content_hash: hash_content(&doc.content),
        status: if issues.is_empty() { "clear".into() } else { "warning".into() },
        issues,
        context_refs: ctx,
        failure: String::new(),
    };
    let s = serde_json::to_string_pretty(&g).map_err(|e| internal("GUARD_SERIALIZE", e.to_string()));
    let s = match s {
        Ok(v) => v,
        Err(r) => return r,
    };
    let _ = state.works.mkdir(&session.workspace_id, &format!("projects/{pid}/guards"));
    if let Err(e) = state.works.write_text(&session.workspace_id, &guard_path(&pid, &sg), &s) {
        return map_core_err(e);
    }
    Json(json!({"ok": true, "guard": g})).into_response()
}

async fn accept_suggest(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((pid, sg)): Path<(String, String)>,
    Json(body): Json<AcceptBody>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let (pid, sg) = match (safe_id(&pid), safe_id(&sg)) {
        (Ok(a), Ok(b)) => (a, b),
        _ => return bad_request("SUGGEST_BAD_ID", "invalid id"),
    };
    let mut doc = match read_sg(&state, &session.workspace_id, &pid, &sg) {
        Ok(d) => d,
        Err(r) => return r,
    };
    // 门 1：pending
    if doc.status != "pending" {
        return Json(json!({"ok": false, "code": "SUGGESTION_DECIDED", "error": "该建议已经处理"})).into_response();
    }
    // 门 2：问答/note 类不可写入正文（本书架只有 continue/polish，polish 走 replace 需 sourceText）
    let ch = match read_ch(&state, &session.workspace_id, &pid, &doc.chapter_id) {
        Ok(d) => d,
        Err(r) => return r,
    };
    // 门 3：正文版本必须一致
    if ch.version_no != doc.chapter_version {
        return Json(json!({"ok": false, "code": "STALE_SUGGESTION", "error": "正文版本已变化，请重新生成建议"})).into_response();
    }
    let content = body.content.unwrap_or_else(|| doc.content.clone());
    // 门 4（continue）：守卫必须 clear
    if doc.task_type == "continue" {
        let guard = read_guard(&state, &session.workspace_id, &pid, &sg);
        let guard = match guard {
            Some(g) => g,
            None => return Json(json!({"ok": false, "code": "GUARD_REQUIRED", "error": "续写建议尚未完成一致性检查"})).into_response(),
        };
        if guard.status == "failed" {
            return Json(json!({"ok": false, "code": "GUARD_FAILED", "error": "续写一致性检查失败，请重新运行检查后再采纳"})).into_response();
        }
        if guard.chapter_version != ch.version_no || guard.content_hash != hash_content(&content) {
            return Json(json!({"ok": false, "code": "GUARD_STALE", "error": "续写内容或正文版本已变化，请重新运行一致性检查"})).into_response();
        }
        // 门 4b：contextRefs 修订比对（章节哈希）
        let cur_hash = hash_content(&ch.content);
        if guard.context_refs.get("chapterHash").and_then(|v| v.as_str()) != Some(&cur_hash) {
            return Json(json!({"ok": false, "code": "GUARD_STALE", "error": "章节内容已变化，请重新运行一致性检查"})).into_response();
        }
    }
    // 写正文
    let next = if doc.action == "append" {
        format!("{}\n\n{}", ch.content.trim_end(), content.trim()).trim().to_string()
    } else {
        if doc.source_text.is_empty() || !ch.content.contains(&doc.source_text) {
            return Json(json!({"ok": false, "code": "SOURCE_TEXT_CHANGED", "error": "原选中文本已不存在，请重新生成建议"})).into_response();
        }
        ch.content.replacen(&doc.source_text, content.trim(), 1)
    };
    let mut ch2 = ch;
    ch2.content = next;
    ch2.version_no += 1;
    ch2.updated_at = now();
    let s = serde_json::to_string_pretty(&ch2).map_err(|e| internal("CHAPTER_SERIALIZE", e.to_string()));
    let s = match s {
        Ok(v) => v,
        Err(r) => return r,
    };
    if let Err(e) = state.works.write_text(&session.workspace_id, &ch_path(&pid, &doc.chapter_id), &s) {
        return map_core_err(e);
    }
    doc.status = "accepted".into();
    doc.content = content;
    let s = serde_json::to_string_pretty(&doc).unwrap_or_default();
    let _ = state.works.write_text(&session.workspace_id, &sg_path(&pid, &sg), &s);
    Json(json!({"ok": true, "suggestion": doc, "chapterVersion": ch2.version_no})).into_response()
}

async fn reject_suggest(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((pid, sg)): Path<(String, String)>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let (pid, sg) = match (safe_id(&pid), safe_id(&sg)) {
        (Ok(a), Ok(b)) => (a, b),
        _ => return bad_request("SUGGEST_BAD_ID", "invalid id"),
    };
    let mut doc = match read_sg(&state, &session.workspace_id, &pid, &sg) {
        Ok(d) => d,
        Err(r) => return r,
    };
    if doc.status != "pending" {
        return Json(json!({"ok": false, "code": "SUGGESTION_DECIDED", "error": "该建议已经处理"})).into_response();
    }
    doc.status = "rejected".into();
    let s = serde_json::to_string_pretty(&doc).unwrap_or_default();
    let _ = state.works.write_text(&session.workspace_id, &sg_path(&pid, &sg), &s);
    Json(json!({"ok": true, "suggestion": doc})).into_response()
}
