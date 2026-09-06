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
    /// 卷弧分组（吞噬 ainovel-cli Volume→Arc→Chapter 三层，Kaleido 化：可选，不动写入）
    #[serde(default)]
    pub volume: u32,
    #[serde(default)]
    pub arc: u32,
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
    #[serde(default)]
    volume: u32,
    #[serde(default)]
    arc: u32,
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
        volume: body.volume,
        arc: body.arc,
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
    /// 七维：consistency/character/pacing/continuity/foreshadow/hook/aesthetic（吞噬 ainovel-cli editor.md）
    #[serde(default)]
    pub dimension: String,
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
    /// 七维评分 0-100（吞噬 ainovel-cli DimensionScore；LLM 给分，verdict 系统推导）
    #[serde(default)]
    pub dimensions: Vec<DimensionScore>,
    #[serde(default)]
    pub context_refs: Value,
    #[serde(default)]
    pub failure: String,
}

/// 七维评分单项（吞噬 ainovel-cli `domain.DimensionScore`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DimensionScore {
    pub dimension: String,
    pub score: i32,
    #[serde(default)]
    pub verdict: String,
    #[serde(default)]
    pub comment: String,
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


/// 全书固化统计（吞噬 ainovel-cli `style_stats` 确定性统计思路，Kaleido 化）。
/// 对 project 全部章节跑：总字数/章均/破折号/三段式信号/章末短句率/高频 2-gram top5。
/// 纯函数，零 LLM。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectStyleStats {
    pub chapters: usize,
    pub total_chars: usize,
    pub avg_chars: usize,
    pub em_dashes: usize,
    pub triple_lists: usize,
    pub short_endings: usize,
    pub short_ending_rate: f32,
    pub top_phrases: Vec<(String, usize)>,
}

fn project_style_stats(
    state: &AppState,
    ws: &str,
    pid: &str,
) -> ProjectStyleStats {
    use std::collections::HashMap;
    let dir = format!("projects/{pid}/chapters");
    let mut chapters = 0usize;
    let mut total_chars = 0usize;
    let mut em_dashes = 0usize;
    let mut triple_lists = 0usize;
    let mut short_endings = 0usize;
    let mut gram: HashMap<String, usize> = HashMap::new();
    if let Ok(entry) = state.works.list(ws, &dir, 1) {
        for child in entry.children {
            if child.kind == "dir" {
                continue;
            }
            let rel = format!("{dir}/{}", child.name);
            let body = match state.works.read_text(ws, &rel) {
                Ok(b) => b,
                Err(_) => continue,
            };
            let ch: ChapterDoc = match serde_json::from_str(&body.content) {
                Ok(c) => c,
                Err(_) => continue,
            };
            chapters += 1;
            let text = format!("{}\n{}", ch.title, ch.content);
            let n_chars = text.chars().count();
            total_chars += n_chars;
            em_dashes += text.matches("\u{2014}\u{2014}").count();
            // 三段式信号：、.*、.*和
            for line in text.lines() {
                let commas = line.matches('、').count();
                if commas >= 2 && line.contains('和') {
                    triple_lists += 1;
                }
            }
            // 章末短句：末段 ≤20 字
            if let Some(last) = text.lines().filter(|l| !l.trim().is_empty()).last() {
                if last.chars().count() <= 20 {
                    short_endings += 1;
                }
            }
            // 高频 2-gram（中文）
            let chars: Vec<char> = text.chars().collect();
            for w in chars.windows(2) {
                if (w[0] as u32) >= 0x4E00
                    && (w[0] as u32) <= 0x9FFF
                    && (w[1] as u32) >= 0x4E00
                    && (w[1] as u32) <= 0x9FFF
                {
                    let g: String = w.iter().collect();
                    *gram.entry(g).or_insert(0) += 1;
                }
            }
        }
    }
    let mut top: Vec<(String, usize)> = gram.into_iter().collect();
    top.sort_by(|a, b| b.1.cmp(&a.1));
    top.truncate(5);
    ProjectStyleStats {
        chapters,
        total_chars,
        avg_chars: if chapters > 0 { total_chars / chapters } else { 0 },
        em_dashes,
        triple_lists,
        short_endings,
        short_ending_rate: if chapters > 0 { short_endings as f32 / chapters as f32 } else { 0.0 },
        top_phrases: top,
    }
}

/// 相关章节推荐（吞噬 ainovel-cli `buildRelatedChapters` 四维思路，Kaleido 化）。
/// 同 project 其他章节，按标题/正文关键词交集推荐 top5（去自身）。
/// 返回 (chapter_id, title, reason)。
fn related_chapters(
    state: &AppState,
    ws: &str,
    pid: &str,
    cur_id: &str,
    focus: &str,
) -> Vec<(String, String, String)> {
    let dir = format!("projects/{pid}/chapters");
    let entry = match state.works.list(ws, &dir, 1) {
        Ok(e) => e,
        Err(_) => return vec![],
    };
    // focus 分词（≥2 char 中文词 + ASCII 词）
    let focus_terms: Vec<String> = {
        let mut v = Vec::new();
        let chars: Vec<char> = focus.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            if c.is_ascii_alphanumeric() {
                let mut j = i;
                while j < chars.len() && chars[j].is_ascii_alphanumeric() {
                    j += 1;
                }
                let w: String = chars[i..j].iter().collect();
                if w.len() >= 3 {
                    v.push(w);
                }
                i = j;
            } else if (c as u32) >= 0x4E00 && (c as u32) <= 0x9FFF {
                // 中文 2-gram
                if i + 1 < chars.len() {
                    let w: String = chars[i..i + 2].iter().collect();
                    v.push(w);
                }
                i += 1;
            } else {
                i += 1;
            }
        }
        v.sort();
        v.dedup();
        v
    };
    if focus_terms.is_empty() {
        return vec![];
    }
    let mut scored: Vec<(i32, String, String, String)> = Vec::new();
    for child in entry.children {
        if child.kind == "dir" {
            continue;
        }
        let rel = format!("{dir}/{}", child.name);
        let body = match state.works.read_text(ws, &rel) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let ch: ChapterDoc = match serde_json::from_str(&body.content) {
            Ok(c) => c,
            Err(_) => continue,
        };
        if ch.id == cur_id {
            continue;
        }
        let hay = format!("{} {}", ch.title, ch.content);
        let mut score = 0;
        let mut hit_terms: Vec<String> = Vec::new();
        for term in &focus_terms {
            if hay.contains(term) {
                score += 1;
                if hit_terms.len() < 3 {
                    hit_terms.push(term.clone());
                }
            }
        }
        if score > 0 {
            scored.push((score, ch.id, ch.title, hit_terms.join("/")));
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0));
    scored
        .into_iter()
        .take(5)
        .map(|(s, id, title, terms)| {
            (
                id,
                title,
                format!("关键词交集{score}（{terms}）", score = s, terms = terms),
            )
        })
        .collect()
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
const GUARD_SYS: &str = "你是续写一致性守卫。按七维逐项检查：①设定一致性 consistency（事件顺序/世界规则/角色属性/状态记录）；②人设一致性 character（行为/对话/动机）；③节奏平衡 pacing（连续同类型/主线停滞/单章情感质变/情节越界）；④叙事连贯 continuity（场景过渡/因果/信息一致）；⑤伏笔健康 foreshadow（长期未推进/新伏笔回收方向）；⑥钩子质量 hook（章末吸引力/类型重复/主线一致）；⑦审美品质 aesthetic（AI 味/叙事手法/情感打动力，每子项必须引用原文举证）。输出分两块：第一块是 JSON 数组（issues），每项必须带 dimension（七维之一）+ type（character/location/time/world/outline/foreshadow）双打标；description 必须引用候选正文原文 10 字以上举证；aesthetic 维 candidateQuote 不得为空。第二块是 JSON 对象（七维评分），格式 {\"dimensions\":{\"consistency\":{\"score\":0-100,\"comment\":\"…\"},…七维全列}}。两块都必须输出，不得省略第二块。不得把文风偏好当成事实冲突。";

/// 七维映射：type → dimension 缺省（LLM 没打 dimension 时回退）。
fn dim_for_kind(kind: &str) -> &'static str {
    match kind {
        "character" => "character",
        "location" | "time" | "world" => "consistency",
        "outline" => "continuity",
        "foreshadow" => "foreshadow",
        _ => "continuity",
    }
}

fn parse_guard(content: &str) -> Result<(Vec<GuardIssue>, Vec<DimensionScore>), String> {
    // 提取 JSON（兼容 markdown 代码块包裹；数组 issues + 可选尾部 dimensions 对象）
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
    // 尾部 dimensions 对象（数组之后第一个 {...} 且含 dimension/score）
    let mut dims: Vec<DimensionScore> = Vec::new();
    let tail = &s[end + 1..];
    if let (Some(ds), Some(de)) = (tail.find('{'), tail.rfind('}')) {
        if let Ok(v) = serde_json::from_str::<Value>(&tail[ds..=de]) {
            let obj = if v.get("dimensions").is_some() { v.get("dimensions").unwrap() } else { &v };
            if let Some(map) = obj.as_object() {
                for (k, val) in map {
                    let score = val.get("score").and_then(|x| x.as_i64()).unwrap_or_else(|| val.as_i64().unwrap_or(0)) as i32;
                    dims.push(DimensionScore {
                        dimension: k.clone(),
                        score: score.clamp(0, 100),
                        verdict: if score >= 80 { "pass".into() } else if score >= 60 { "warning".into() } else { "fail".into() },
                        comment: val.get("comment").and_then(|x| x.as_str()).unwrap_or("").into(),
                    });
                }
            }
        }
    }
    let kinds = ["character", "location", "time", "world", "outline", "foreshadow"];
    let sevs = ["low", "medium", "high"];
    let dims7 = ["consistency", "character", "pacing", "continuity", "foreshadow", "hook", "aesthetic"];
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
        // ④ aesthetic 维 candidateQuote 不得为空
        let dim_raw = item.get("dimension").and_then(|v| v.as_str()).unwrap_or("");
        let dim = if dims7.contains(&dim_raw) { dim_raw.to_string() } else { dim_for_kind(kind).to_string() };
        let quote = item.get("candidateQuote").and_then(|v| v.as_str()).unwrap_or("");
        if dim == "aesthetic" && quote.trim().is_empty() {
            return Err(format!("guard item {} aesthetic missing candidateQuote", i + 1));
        }
        out.push(GuardIssue {
            kind: kind.into(),
            dimension: dim,
            severity: sev.into(),
            title: title.into(),
            description: item.get("description").and_then(|v| v.as_str()).unwrap_or("").into(),
            candidate_quote: quote.into(),
            source_refs: item.get("sourceRefs").and_then(|v| v.as_array()).cloned().unwrap_or_default(),
            suggestion: item.get("suggestion").and_then(|v| v.as_str()).unwrap_or("").into(),
        });
    }
    Ok((out, dims))
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
            "/api/v1/author/projects/{id}/stats",
            get(get_stats),
        )
        .route(
            "/api/v1/author/projects/{id}/triage",
            post(triage_instruction),
        )
        .route(
            "/api/v1/author/projects/{id}/writing-rules",
            get(get_writing_rules).put(put_writing_rules),
        )
        .route(
            "/api/v1/author/projects/{id}/advance",
            get(get_advance).post(set_advance),
        )
        .route(
            "/api/v1/author/projects/{id}/permit",
            post(grant_permit),
        )
        .route(
            "/api/v1/author/projects/{id}/arcs",
            get(get_arcs),
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
    // ⑪ 逐章验收门禁（review 下无本章许可即 409）
    let adv = read_advance(&state, &session.workspace_id, &pid);
    if adv.mode == "review" && adv.permit_chapter != ch {
        return Json(json!({"ok": false, "code": "ADVANCE_HOLD", "error": "逐章验收模式：请先放行本章（POST /permit）"})).into_response();
    }
    let instruction = body.instruction;
    let source_text = body.source_text;
    // ① 相关章节推荐（ainovel-cli 四维思路：focus=指令+标题，与历史章节关键词交集 top5）
    let focus = format!("{} {}", doc.title, instruction);
    let related = related_chapters(&state, &session.workspace_id, &pid, &ch, &focus);
    let related_block = if related.is_empty() {
        String::new()
    } else {
        let lines: Vec<String> = related
            .iter()
            .map(|(id, title, reason)| format!("- 《{}》（{}）：{}", title, id, reason))
            .collect();
        format!("\n\n相关历史章节（写作时注意承接）：\n{}", lines.join("\n"))
    };
    // ⑨ 写作规则注入（ainovel-cli user_rules 同款：后续 suggest/guard 自动带上）
    let writing_rules = read_rules(&state, &session.workspace_id, &pid);
    let rules_block = if writing_rules.trim().is_empty() {
        String::new()
    } else {
        format!("\n\n本书写作规则（必须遵守）：\n{}", writing_rules)
    };
    // LLM 生成候选（不写正文）
    let sys = "你是长篇小说续写助手。根据章节正文和指令生成续写/润色候选。只输出候选正文，不要解释。";
    let user = if task_type == "continue" {
        format!("章节《{}》正文：\n{}\n\n指令：{}{}{}\n\n请续写下一段（300-800字）：", doc.title, doc.content, instruction, related_block, rules_block)
    } else {
        format!("章节《{}》正文：\n{}\n\n待润色选区：\n{}\n\n指令：{}{}{}\n\n请输出润色后文本：", doc.title, doc.content, source_text, instruction, related_block, rules_block)
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
    let related_json: Vec<Value> = related
        .iter()
        .map(|(id, title, reason)| json!({"chapterId": id, "title": title, "reason": reason}))
        .collect();
    Json(json!({"ok": true, "suggestion": sg, "relatedChapters": related_json})).into_response()
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

async fn get_stats(
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
    let stats = project_style_stats(&state, &session.workspace_id, &pid);
    Json(json!({"ok": true, "stats": stats})).into_response()
}

/// 干预分诊（吞噬 ainovel-cli `arbiter-intervention.md` 口径，Kaleido 化）。
/// 用户一句话 → 三类去向：style（怎么写→rules 落盘）/ plot（写什么→architect 大纲意见）/
/// rework（改已写的→批量 suggest 入队）/ query（查询→直接回答）。
/// 最小充分范围：绝不扩大授权。
const TRIAGE_SYS: &str = "你是改编干预分诊器。把用户指令分成一类，只输出 JSON，不要解释。类别：style（怎么写：笔法/风格/质量/禁用语/句式，任何章节都成立）、plot（写什么：剧情/结构/人物/篇幅/新走向）、rework（改已写的：重写/修订已有章节）、query（查询：问状态/设定/进度）。输出格式：{\"kind\":\"style|plot|rework|query\",\"answer\":\"给用户的回话\",\"rules\":\"style 类时要落盘的规则原文，否则空\",\"targets\":\"rework 类时的章节 id 列表（逗号分隔），否则空\",\"reason\":\"一句话理由\" }。判别口径：「怎么写」→style；「写什么」→plot；「改已写的」→rework；相对式指令（增加10章/重写第3章）绝不进 style。";
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TriageBody {
    #[serde(default)]
    instruction: String,
}

fn rules_path(pid: &str) -> String {
    format!("projects/{pid}/writing_rules.md")
}

fn read_rules(state: &AppState, ws: &str, pid: &str) -> String {
    state
        .works
        .read_text(ws, &rules_path(pid))
        .map(|b| b.content)
        .unwrap_or_default()
}

async fn triage_instruction(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(pid): Path<String>,
    Json(body): Json<TriageBody>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let pid = match safe_id(&pid) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let instruction = body.instruction.trim().to_string();
    if instruction.is_empty() {
        return bad_request("TRIAGE_EMPTY", "instruction required");
    }
    // 章节计数（facts 快照）
    let stats = project_style_stats(&state, &session.workspace_id, &pid);
    let user = format!(
        "用户指令：{}\n当前事实：共{}章，总{}字。只输出 JSON。",
        instruction, stats.chapters, stats.total_chars
    );
    let raw = match call_llm(&state, TRIAGE_SYS, &user).await {
        Ok(r) => r,
        Err(e) => return internal("TRIAGE_LLM_FAILED", e),
    };
    let s = raw.trim();
    let s = s
        .strip_prefix("```json")
        .or_else(|| s.strip_prefix("```"))
        .unwrap_or(s)
        .strip_suffix("```")
        .unwrap_or(s)
        .trim();
    let v: Value = match serde_json::from_str(s)
        .or_else(|_| serde_json::from_str(&s[s.find('{').unwrap_or(0)..s.rfind('}').map(|i| i + 1).unwrap_or(s.len())]))
    {
        Ok(v) => v,
        Err(e) => return internal("TRIAGE_BAD_JSON", e.to_string()),
    };
    let kind = v.get("kind").and_then(|x| x.as_str()).unwrap_or("query").to_string();
    let answer = v.get("answer").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let rules = v.get("rules").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let targets = v.get("targets").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let reason = v.get("reason").and_then(|x| x.as_str()).unwrap_or("").to_string();
    // ⑨ style → rules 落盘（追加）
    let mut ruled = false;
    if kind == "style" && !rules.trim().is_empty() {
        let mut cur = read_rules(&state, &session.workspace_id, &pid);
        if !cur.is_empty() && !cur.ends_with('\n') {
            cur.push('\n');
        }
        cur.push_str(&format!("- {}\n", rules.trim()));
        let _ = state.works.mkdir(&session.workspace_id, &format!("projects/{pid}"));
        if state.works.write_text(&session.workspace_id, &rules_path(&pid), &cur).is_ok() {
            ruled = true;
        }
    }
    // ⑩ rework → 批量 suggest 入队（每章一个 pending continue 建议）
    let mut queued: Vec<Value> = Vec::new();
    if kind == "rework" {
        for tid in targets.split([',', '、', ' ']).map(|x| x.trim()).filter(|x| !x.is_empty()) {
            let ch_id = match safe_id(tid) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if read_ch(&state, &session.workspace_id, &pid, &ch_id).is_err() {
                continue;
            }
            let sg_id = format!("sg-{}", &Uuid::new_v4().to_string()[..8]);
            let sg = Suggestion {
                id: sg_id.clone(),
                project_id: pid.clone(),
                chapter_id: ch_id.clone(),
                chapter_version: read_ch(&state, &session.workspace_id, &pid, &ch_id)
                    .map(|c| c.version_no)
                    .unwrap_or(1),
                task_type: "continue".into(),
                action: "append".into(),
                instruction: format!("按用户要求返工：{}", instruction),
                content: String::new(),
                status: "pending".into(),
                source_text: String::new(),
                created_at: now(),
            };
            if let Ok(s) = serde_json::to_string_pretty(&sg) {
                let _ = state.works.mkdir(&session.workspace_id, &format!("projects/{pid}/suggestions"));
                if state.works.write_text(&session.workspace_id, &sg_path(&pid, &sg_id), &s).is_ok() {
                    queued.push(json!({"suggestionId": sg_id, "chapterId": ch_id}));
                }
            }
        }
    }
    Json(json!({
        "ok": true,
        "kind": kind,
        "answer": answer,
        "rulesSaved": ruled,
        "queued": queued,
        "reason": reason,
    }))
    .into_response()
}

async fn get_writing_rules(
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
    Json(json!({"ok": true, "rules": read_rules(&state, &session.workspace_id, &pid)})).into_response()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PutRulesBody {
    #[serde(default)]
    rules: String,
}

async fn put_writing_rules(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(pid): Path<String>,
    Json(body): Json<PutRulesBody>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let pid = match safe_id(&pid) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let _ = state.works.mkdir(&session.workspace_id, &format!("projects/{pid}"));
    if let Err(e) = state.works.write_text(&session.workspace_id, &rules_path(&pid), &body.rules) {
        return map_core_err(e);
    }
    Json(json!({"ok": true})).into_response()
}

/// 逐章验收（吞噬 ainovel-cli `AdvanceMode auto/review` + `AdvancePermitChapter`，Kaleido 化）。
/// review 下 create_suggest 无许可即 409；返工（rework 入队）不耗许可。
fn advance_path(pid: &str) -> String {
    format!("projects/{pid}/advance.json")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AdvanceState {
    #[serde(default = "auto_mode")]
    mode: String,
    #[serde(default)]
    permit_chapter: String,
}

fn auto_mode() -> String {
    "auto".into()
}

fn read_advance(state: &AppState, ws: &str, pid: &str) -> AdvanceState {
    state
        .works
        .read_text(ws, &advance_path(pid))
        .ok()
        .and_then(|b| serde_json::from_str::<AdvanceState>(&b.content).ok())
        .unwrap_or(AdvanceState { mode: "auto".into(), permit_chapter: String::new() })
}

async fn get_advance(
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
    let adv = read_advance(&state, &session.workspace_id, &pid);
    Json(json!({"ok": true, "mode": adv.mode, "permitChapter": adv.permit_chapter})).into_response()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetAdvanceBody {
    #[serde(default = "auto_mode")]
    mode: String,
}

async fn set_advance(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(pid): Path<String>,
    Json(body): Json<SetAdvanceBody>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let pid = match safe_id(&pid) {
        Ok(v) => v,
        Err(r) => return r,
    };
    if body.mode != "auto" && body.mode != "review" {
        return bad_request("ADVANCE_BAD_MODE", "mode must be auto|review");
    }
    let adv = AdvanceState { mode: body.mode, permit_chapter: String::new() };
    let s = serde_json::to_string_pretty(&adv).unwrap_or_default();
    let _ = state.works.mkdir(&session.workspace_id, &format!("projects/{pid}"));
    if let Err(e) = state.works.write_text(&session.workspace_id, &advance_path(&pid), &s) {
        return map_core_err(e);
    }
    Json(json!({"ok": true, "mode": adv.mode})).into_response()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PermitBody {
    #[serde(default)]
    chapter_id: String,
}

async fn grant_permit(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(pid): Path<String>,
    Json(body): Json<PermitBody>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let pid = match safe_id(&pid) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let ch_id = match safe_id(&body.chapter_id) {
        Ok(v) => v,
        Err(r) => return r,
    };
    if read_ch(&state, &session.workspace_id, &pid, &ch_id).is_err() {
        return bad_request("PERMIT_BAD_CHAPTER", "chapter not found");
    }
    let adv = AdvanceState { mode: "review".into(), permit_chapter: ch_id.clone() };
    let s = serde_json::to_string_pretty(&adv).unwrap_or_default();
    let _ = state.works.mkdir(&session.workspace_id, &format!("projects/{pid}"));
    if let Err(e) = state.works.write_text(&session.workspace_id, &advance_path(&pid), &s) {
        return map_core_err(e);
    }
    Json(json!({"ok": true, "permitChapter": ch_id})).into_response()
}

/// 卷弧分组视图（吞噬 ainovel-cli Volume→Arc 三层，纯展示+边界提示）。
async fn get_arcs(
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
    let dir = format!("projects/{pid}/chapters");
    let mut all: Vec<ChapterDoc> = Vec::new();
    if let Ok(entry) = state.works.list(&session.workspace_id, &dir, 1) {
        for child in entry.children {
            if child.kind == "dir" {
                continue;
            }
            let rel = format!("{dir}/{}", child.name);
            if let Ok(body) = state.works.read_text(&session.workspace_id, &rel) {
                if let Ok(ch) = serde_json::from_str::<ChapterDoc>(&body.content) {
                    all.push(ch);
                }
            }
        }
    }
    // 按 (volume, arc, id) 分组
    use std::collections::BTreeMap;
    let mut groups: BTreeMap<(u32, u32), Vec<Value>> = BTreeMap::new();
    for ch in &all {
        groups.entry((ch.volume, ch.arc)).or_default().push(
            json!({"chapterId": ch.id, "title": ch.title, "versionNo": ch.version_no}),
        );
    }
    let arcs: Vec<Value> = groups
        .into_iter()
        .map(|((v, a), chapters)| json!({"volume": v, "arc": a, "count": chapters.len(), "chapters": chapters}))
        .collect();
    Json(json!({"ok": true, "count": all.len(), "arcs": arcs})).into_response()
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
    // ⑤ 伏笔账龄口径 + ⑦ 全书统计喂给守卫
    let stats = project_style_stats(&state, &session.workspace_id, &pid);
    let stats_line = format!(
        "\n\n全书统计：共{}章，总{}字，章均{}字，破折号{}处，三段式信号{}处，章末短句率{:.0}%（{}/{}），高频词{}。检查：破折号是否全书泛滥、同一高频词是否疲劳、章末是否全是短句（ainovel-cli style_stats 口径）。\n伏笔健康：已写{}章，超过 5 章未推进的伏笔必须出 foreshadow 维 issue；新伏笔必须有回收方向，否则出 issue。",
        stats.chapters,
        stats.total_chars,
        stats.avg_chars,
        stats.em_dashes,
        stats.triple_lists,
        stats.short_ending_rate * 100.0,
        stats.short_endings,
        stats.chapters,
        stats
            .top_phrases
            .iter()
            .map(|(w, n)| format!("{}×{}", w, n))
            .collect::<Vec<_>>()
            .join("/"),
        stats.chapters,
    );
    // ① 相关章节也喂给守卫（对照物更全）
    let focus_g = format!("{} {}", ch.title, doc.instruction);
    let related_g = related_chapters(&state, &session.workspace_id, &pid, &doc.chapter_id, &focus_g);
    let mut related_ctx = String::new();
    for (id, title, _reason) in related_g.iter().take(3) {
        let rel_path = format!("projects/{pid}/chapters/{id}.json");
        if let Ok(body) = state.works.read_text(&session.workspace_id, &rel_path) {
            if let Ok(rch) = serde_json::from_str::<ChapterDoc>(&body.content) {
                let excerpt: String = rch.content.chars().take(500).collect();
                related_ctx.push_str(&format!("\n\n历史章节《{}》摘要：\n{}", title, excerpt));
            }
        }
    }
    let user = format!(
        "检查下面的续写候选是否与提供的上下文冲突。输出 JSON 数组，没有冲突时输出 []。\n每项字段必须为：type（character/location/time/world/outline/foreshadow）、severity（low/medium/high）、title、description（必须引用候选正文原文 10 字以上举证）、candidateQuote、sourceRefs（数组）、suggestion。\n不得把文风偏好当成事实冲突，不得使用 Markdown 代码块。\n续写候选：\n{}\n\n章节正文：\n{}{}",
        doc.content, ch.content, format!("{}{}", related_ctx, stats_line)
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
                dimensions: vec![],
                context_refs: ctx,
                failure: e,
            };
            let s = serde_json::to_string_pretty(&g).unwrap_or_default();
            let _ = state.works.mkdir(&session.workspace_id, &format!("projects/{pid}/guards"));
            let _ = state.works.write_text(&session.workspace_id, &guard_path(&pid, &sg), &s);
            return Json(json!({"ok": true, "guard": g})).into_response();
        }
    };
    let (issues, mut dims) = match parse_guard(&raw) {
        Ok(v) => v,
        Err(e) => {
            let g = ContinuationGuard {
                suggestion_id: sg.clone(),
                chapter_version: ch.version_no,
                content_hash: hash_content(&doc.content),
                status: "failed".into(),
                issues: vec![],
                dimensions: vec![],
                context_refs: ctx,
                failure: e,
            };
            let s = serde_json::to_string_pretty(&g).unwrap_or_default();
            let _ = state.works.mkdir(&session.workspace_id, &format!("projects/{pid}/guards"));
            let _ = state.works.write_text(&session.workspace_id, &guard_path(&pid, &sg), &s);
            return Json(json!({"ok": true, "guard": g})).into_response();
        }
    };
    // dimensions 为空时重试一次（只问七维评分，低成本补齐）
    if dims.is_empty() {
        let retry_user = format!(
            "对下面的续写候选按七维打分 0-100，只输出 JSON 对象，不要解释：\n{{\"dimensions\":{{\"consistency\":{{\"score\":N,\"comment\":\"…\"}},\"character\":{{…}},\"pacing\":{{…}},\"continuity\":{{…}},\"foreshadow\":{{…}},\"hook\":{{…}},\"aesthetic\":{{…}}}}}}\n\n续写候选：\n{}",
            doc.content.chars().take(2000).collect::<String>()
        );
        if let Ok(retry_raw) = call_llm(&state, "你是小说评审，只输出 JSON。", &retry_user).await {
            if let Ok((_, rdims)) = parse_guard(&format!("[]\n{}", retry_raw)) {
                if !rdims.is_empty() {
                    dims = rdims;
                }
            }
        }
    }
    let g = ContinuationGuard {
        suggestion_id: sg.clone(),
        chapter_version: ch.version_no,
        content_hash: hash_content(&doc.content),
        status: if issues.is_empty() { "clear".into() } else { "warning".into() },
        issues,
        dimensions: dims,
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
