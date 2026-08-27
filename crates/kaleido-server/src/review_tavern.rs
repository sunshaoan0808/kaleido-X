//! 审稿闭环 API（U4，T1 创作质量）。
//!
//! 参考 Openwrite `revision_store.py`：审稿结果结构化持久化，支持逐条修复并复查。
//!
//! Routes (all require session auth):
//! - `POST /api/v1/story-tavern/works/{work_id}/reviews`           trigger AI review (15 dims) -> ReviewRun
//! - `GET  /api/v1/story-tavern/works/{work_id}/reviews`           list review history (runs new->old)
//! - `POST /api/v1/story-tavern/works/{work_id}/reviews/{run_id}/issues/{idx}/fix`  fix one issue + recheck severity
//!
//! Body for trigger: `{ "target": "...", "content": "..." }` (content is the manuscript text).
//! Body for fix: `{ "content": "..." }` (revised full text of the target chapter).

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use kaleido_core::{
    run_post_check, ReviewIssue, ReviewRun, REVIEW_DIMENSIONS,
    REVIEW_STATUS_FIXED, REVIEW_STATUS_OPEN,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::llm_stream::{chat_completion_dispatch, runtime_provider_kind};
use crate::error_codes::*;
use crate::{map_core_err, session_from, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/story-tavern/works/{work_id}/reviews",
            post(run_review).get(list_reviews),
        )
        .route(
            "/api/v1/story-tavern/works/{work_id}/reviews/{run_id}/issues/{idx}/fix",
            post(fix_issue),
        )
        .route(
            "/api/v1/story-tavern/works/{work_id}/reviews/{run_id}/post-check",
            post(post_check),
        )
        .route(
            "/api/v1/story-tavern/works/{work_id}/reviews/check/post-check",
            post(post_check_standalone),
        )
        // U5 联动闭环：高严重度规则违例 → skill_layer 精修改写建议（LLM 批量一次调用）。
        .route(
            "/api/v1/story-tavern/works/{work_id}/reviews/check/post-refine",
            post(post_refine),
        )
}

/// 维度说明（注入 LLM，保证覆盖 15 维）。
const REVIEW_SYS_PROMPT: &str = "你是一位资深网文/剧本审稿人。请对用户提供的正文进行逐维度审查，\
输出 **JSON 数组**（不要 Markdown、不要多余文字），每个元素对象字段：\
`dimension`（维度名，必须取自给定清单）、`severity`（数字 1-3，3 最严重）、\
`quote`（触发问题的原文片段，可为空字符串）、`problem`（简洁中文问题说明）、\
`fix_instruction`（可直接执行的修改指令，中文）。\
只输出发现的问题，完全没有问题则输出空数组 []。必须覆盖清单中的至少 10 个维度，没有问题的维度不输出。";

const FIX_RECHECK_SYS: &str = "你是严谨的审稿复查员，只输出 JSON，不输出其他内容。";

/// 请求体：触发审稿。
#[derive(Debug, Deserialize)]
struct RunReviewBody {
    #[serde(default)]
    target: String,
    content: String,
}

/// 请求体：修复单条问题（携带修复后的全文，复查判断是否解决）。
#[derive(Debug, Deserialize)]
struct FixIssueBody {
    content: String,
}

/// U5 联动请求体：post-refine（高严重度规则违例 → LLM 精修改写建议）。
#[derive(Debug, Deserialize)]
struct PostRefineBody {
    content: String,
    /// 参与改写的最低严重度（1-3，默认 2；3=仅违禁词级别）。
    #[serde(default)]
    min_severity: Option<u8>,
    /// 单次最多送入 LLM 的违例条数（默认 6，防 prompt 失控）。
    #[serde(default)]
    max_issues: Option<usize>,
}

fn ok_value(v: Value) -> Response {
    Json(v).into_response()
}

/// 构造 reqwest client（与既有 llm_stream 相同的 timeout 语义）。
fn http_client(timeout_secs: u64) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(timeout_secs.max(30)))
        .build()
        .map_err(|e| e.to_string())
}

/// 触发一次 AI 审稿：15 维 prompt -> LLM JSON -> 持久化（新版置顶）。
async fn run_review(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(work_id): Path<String>,
    Json(body): Json<RunReviewBody>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    if body.content.trim().is_empty() {
        return bad_request("MAIN_BAD_REQUEST", "content is empty");
    }

    let dimension_list = REVIEW_DIMENSIONS.join("、");
    let clipped: String = body.content.chars().take(12000).collect();
    let user = format!("【维度清单】{dimension_list}\n\n【正文】\n{clipped}\n\n请按格式输出 JSON 数组。");

    let llm = state
        .app_state
        .resolve_llm(state.llm_base.as_deref(), state.llm_key.as_deref(), &state.llm_model);
    let prov_kind = runtime_provider_kind(&llm, &state.provider_kind);
    // 审稿是长任务（15 维 × 上万字正文），deepseek thinking 生成长 JSON 波动大（实测 150s-5min+）；超时放宽到 600s，并对瞬时失败重试一次
    const REVIEW_TIMEOUT_SECS: u64 = 600;
    let mut last_err: Option<String> = None;
    for attempt in 0..2u8 {
        let client = match http_client(REVIEW_TIMEOUT_SECS) {
            Ok(c) => c,
            Err(e) => return bad_gateway("REV_ERROR", e),
        };
        match chat_completion_dispatch(&llm.base_url, &llm.api_key, &llm.model, &prov_kind, REVIEW_SYS_PROMPT, &user, 0.1, 16384, REVIEW_TIMEOUT_SECS, &client).await {
            Ok(text) => match parse_review_issues(&text) {
                Ok(issues) => {
                    if issues.is_empty() {
                        return (StatusCode::OK, Json(json!({"issues": [], "note": "未发现问题"}))).into_response();
                    }
                    let run = ReviewRun {
                        id: format!("review-{}", now_ms()),
                        target: body.target,
                        created_at: now_secs(),
                        issues,
                    };
                    return match state.reviews.append_run(&work_id, run.clone()) {
                        Ok(saved) => (StatusCode::CREATED, Json(json!(saved))).into_response(),
                        Err(e) => map_core_err(e),
                    };
                }
                Err(e) => return bad_gateway("REV_ERROR", format!("审稿解析失败：{e}")),
            },
            Err(e) => {
                tracing::warn!(attempt, error=%e, "审稿 LLM 调用失败，重试中");
                last_err = Some(e);
                if attempt == 0 {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            }
        }
    }
    let err = last_err.unwrap_or_else(|| "unknown error".to_string());
    return bad_gateway("REV_ERROR", format!("审稿 LLM 调用失败：{err}"));
}

/// 列出审稿历史（新→旧），含每条问题的状态（open/fixed/accepted）。
async fn list_reviews(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(work_id): Path<String>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    match state.reviews.load(&work_id) {
        Ok(hist) => ok_value(json!(hist)),
        Err(e) => map_core_err(e),
    }
}

/// 修复单条问题：LLM 判断修复后的全文是否已解决该维度；已解决标记 fixed。
async fn fix_issue(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((work_id, run_id, idx)): Path<(String, String, usize)>,
    Json(body): Json<FixIssueBody>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }

    let hist = match state.reviews.load(&work_id) {
        Ok(h) => h,
        Err(e) => return map_core_err(e),
    };
    let Some(run) = hist.runs.iter().find(|r| r.id == run_id) else {
        return not_found("MAIN_NOT_FOUND", "run not found");
    };
    let Some(issue) = run.issues.get(idx).cloned() else {
        return not_found("MAIN_NOT_FOUND", "issue not found");
    };

    let clipped: String = body.content.chars().take(6000).collect();
    let fix_user = format!(
        "原问题：维度「{dim}」\n描述：{problem}\n\n修复后的正文：\n{content}\n\n\
         请判断该问题是否已解决。输出 JSON：{{\"resolved\": true/false, \"remaining_note\": \"...\"}}。",
        dim = issue.dimension,
        problem = issue.problem,
        content = clipped
    );
    let llm = state
        .app_state
        .resolve_llm(state.llm_base.as_deref(), state.llm_key.as_deref(), &state.llm_model);
    let prov_kind = runtime_provider_kind(&llm, &state.provider_kind);
    let client = match http_client(300) {
        Ok(c) => c,
        Err(e) => return bad_gateway("REV_ERROR", e),
    };
    let mut resolved = false;
    for attempt in 0..2u8 {
        match chat_completion_dispatch(&llm.base_url, &llm.api_key, &llm.model, &prov_kind, FIX_RECHECK_SYS, &fix_user, 0.1, 16384, 300, &client)
            .await
        {
            Ok(text) => {
                resolved = parse_bool_field(&text, "resolved").unwrap_or(false);
                break;
            }
            Err(e) => {
                tracing::warn!(attempt, error=%e, "fix recheck LLM 调用失败，重试中");
                if attempt == 0 {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            }
        }
    }

    let mut updated = issue.clone();
    if resolved {
        updated.status = REVIEW_STATUS_FIXED.to_string();
        updated.problem = format!("{}（已修复 ✓ 复核通过）", issue.problem);
    } else {
        updated.status = REVIEW_STATUS_OPEN.to_string();
    }

    match state.reviews.update_issue(&work_id, &run_id, idx, updated) {
        Ok(run_after) => ok_value(json!({"run": run_after, "resolved": resolved})),
        Err(e) => map_core_err(e),
    }
}

/// U5 后置规则检查：纯规则引擎扫描正文（违禁词/AI痕迹/超长句/重复词/标点滥用），
/// 返回结构化违例列表 {rule, severity, line, quote, fix}，供前端问题面板并入审稿视图。
async fn post_check(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((work_id, run_id)): Path<(String, String)>,
    Json(body): Json<RunReviewBody>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let issues = run_post_check(&body.content);
    ok_value(json!({
        "work_id": work_id,
        "run_id": run_id,
        "total": issues.len(),
        "issues": issues,
    }))
}

/// 独立规则检查（不绑定 run_id）：前端「⚡ 规则检查」按钮调用此路径，
/// 仅做纯规则扫描，不需要先发起 LLM 审稿。路径
/// `POST /api/v1/story-tavern/works/{work_id}/reviews/check/post-check`。
async fn post_check_standalone(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(work_id): Path<String>,
    Json(body): Json<RunReviewBody>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let issues = run_post_check(&body.content);
    ok_value(json!({
        "work_id": work_id,
        "total": issues.len(),
        "issues": issues,
    }))
}

/// U5 联动闭环：高严重度规则违例（severity >= min_severity）→ skill_layer 精修
/// 改写建议。单次批量 LLM 调用产出逐条 `rewritten` 替换文本；LLM 失败软回退为
/// 纯规则 fix 建议（fallback:true），与 outline/reverse/analyze 同一降级语义。
async fn post_refine(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(work_id): Path<String>,
    Json(body): Json<PostRefineBody>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    if body.content.trim().is_empty() {
        return bad_request("MAIN_BAD_REQUEST", "content is empty");
    }
    let min_sev = body.min_severity.unwrap_or(2).clamp(1, 3);
    let max_issues = body.max_issues.unwrap_or(6).clamp(1, 20);

    let all = run_post_check(&body.content);
    let total = all.len();
    // 高严重度子集（run_post_check 已按 (line, rule) 排序，取前 N 条防 prompt 失控）。
    let high: Vec<kaleido_core::PostIssue> = all
        .iter()
        .filter(|i| i.severity >= min_sev)
        .take(max_issues)
        .cloned()
        .collect();
    let high_total: usize = all.iter().filter(|i| i.severity >= min_sev).count();

    if high.is_empty() {
        return ok_value(json!({
            "work_id": work_id,
            "total": total,
            "min_severity": min_sev,
            "high_severity": 0,
            "refined": [],
            "llm_used": false,
            "note": "无达到阈值的违例，无需改写联动",
        }));
    }

    // P4 skill 层：装载写作 Skill（workspace → user → builtin），注入风格约束与
    // fix 模板精神——只修列出的问题、保留人物声线与连续性。
    let session = session_from(&state, &headers).ok();
    let ws_id = session.as_ref().map(|s| s.workspace_id.clone());
    let data_root = state.auth.data_root().root().to_path_buf();
    let skill = crate::skill_layer::load_writing_skill(
        &data_root,
        ws_id.as_deref(),
        crate::skill_layer::resolve_tier_for_quality(crate::story_tavern::TurnQuality::Standard),
    );

    let mut sys = POST_REFINE_SYS.to_string();
    if let Some(doc) = &skill {
        if !doc.rules.trim().is_empty() {
            let rules: String = doc.rules.chars().take(1200).collect();
            sys.push_str("\n\n【写作风格约束】\n");
            sys.push_str(&rules);
        }
    }

    // 组装用户 prompt：每条违例带行号原文上下文（截断防爆）。
    let lines: Vec<&str> = body.content.split('\n').collect();
    let mut items = String::new();
    for (k, issue) in high.iter().enumerate() {
        let ctx = lines
            .get(issue.line.saturating_sub(1))
            .map(|l| l.trim().chars().take(160).collect::<String>())
            .unwrap_or_else(|| issue.quote.clone());
        items.push_str(&format!(
            "{}. 规则「{}」（严重度 {}，第 {} 行）\n   违例片段：{}\n   所在行全文：{}\n   官方建议：{}\n",
            k + 1,
            issue.rule,
            issue.severity,
            issue.line,
            issue.quote,
            if ctx.is_empty() { "(空行)" } else { &ctx },
            issue.fix,
        ));
    }
    let user = format!(
        "【待精修正文违例清单】\n{items}\n请对以上每一条输出改写后的整行替换文本（JSON 数组）。"
    );

    // zen gateway 非流式长输出实测 46-90s 且偶发 403 → 超时 ≥150s + 失败一次重试。
    const REFINE_TIMEOUT_SECS: u64 = 150;
    let llm = state
        .app_state
        .resolve_llm(state.llm_base.as_deref(), state.llm_key.as_deref(), &state.llm_model);
    let prov_kind = runtime_provider_kind(&llm, &state.provider_kind);
    let client = match http_client(REFINE_TIMEOUT_SECS) {
        Ok(c) => c,
        Err(e) => return bad_gateway("REV_ERROR", e),
    };

    let mut last_err: Option<String> = None;
    for attempt in 0..2u8 {
        match chat_completion_dispatch(
            &llm.base_url,
            &llm.api_key,
            &llm.model,
            &prov_kind,
            &sys,
            &user,
            0.2,
            4096,
            REFINE_TIMEOUT_SECS,
            &client,
        )
        .await
        {
            Ok(text) => match parse_refine_rewrites(&text, high.len()) {
                Ok(rewrites) => {
                    // 按 line 合并：规则建议保底，LLM 改写覆盖同行条目。
                    let refined: Vec<Value> = high
                        .iter()
                        .map(|issue| {
                            let rewritten = rewrites
                                .iter()
                                .find(|(ln, _)| *ln == issue.line)
                                .map(|(_, t)| t.clone());
                            json!({
                                "rule": issue.rule,
                                "severity": issue.severity,
                                "line": issue.line,
                                "quote": issue.quote,
                                "fix": issue.fix,
                                "rewritten": rewritten,
                            })
                        })
                        .collect();
                    let with_rewrite =
                        refined.iter().filter(|r| !r["rewritten"].is_null()).count();
                    return ok_value(json!({
                        "work_id": work_id,
                        "total": total,
                        "min_severity": min_sev,
                        "high_severity": high_total,
                        "refined": refined,
                        "refined_count": with_rewrite,
                        "llm_used": true,
                    }));
                }
                Err(e) => {
                    tracing::warn!(attempt, error=%e, "post-refine LLM 解析失败，重试中");
                    last_err = Some(format!("解析失败：{e}"));
                }
            },
            Err(e) => {
                tracing::warn!(attempt, error=%e, "post-refine LLM 调用失败，重试中");
                last_err = Some(e);
            }
        }
        if attempt == 0 {
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }

    // 软回退：LLM 失败不阻塞，返回纯规则建议（前端仍可按 fix 文案人工修改）。
    let refined: Vec<Value> = high
        .iter()
        .map(|issue| {
            json!({
                "rule": issue.rule,
                "severity": issue.severity,
                "line": issue.line,
                "quote": issue.quote,
                "fix": issue.fix,
                "rewritten": null,
            })
        })
        .collect();
    ok_value(json!({
        "work_id": work_id,
        "total": total,
        "min_severity": min_sev,
        "high_severity": high_total,
        "refined": refined,
        "llm_used": false,
        "fallback": true,
        "note": "LLM 精修失败，已回退规则建议",
        "error": last_err.unwrap_or_default(),
    }))
}

/// post-refine 系统提示：只输出 JSON 数组的定向精修师。
const POST_REFINE_SYS: &str = "你是一位网文定向精修师。针对给出的正文违例清单，逐条给出**改写后的整行替换文本**。\
要求：只修清单里指出的问题，不改写无关内容；保留人物声线、叙事视角与上下文连续性；保持中文网文语感，避免模板腔。\
只输出 **JSON 数组**（不要 Markdown 围栏、不要解释文字），每个元素：{\"line\": 行号数字, \"rewritten\": \"该行改写后的完整替换文本\"}。\
若某条无法安全改写（如上下文不足），对应元素输出 {\"line\": 行号, \"rewritten\": null}。";

/// 解析 post-refine LLM 输出：JSON 数组 [{line, rewritten}] → [(行号, 改写文本)]。
/// 容错 ```json 围栏与前后缀废话；rewritten 为 null 的条目跳过；行号必须在给定集合内。
fn parse_refine_rewrites(text: &str, max_line: usize) -> Result<Vec<(usize, String)>, String> {
    let cleaned = extract_json_value(text);
    let v: Value =
        serde_json::from_str(cleaned.trim()).map_err(|e| format!("json parse: {e}"))?;
    let arr = v.as_array().ok_or_else(|| "顶层不是数组".to_string())?;
    let mut out = Vec::new();
    for item in arr {
        let obj = item.as_object().ok_or_else(|| "数组元素不是对象".to_string())?;
        let line = obj
            .get("line")
            .and_then(|x| x.as_u64())
            .map(|x| x as usize)
            .unwrap_or(0);
        if line == 0 || line > max_line {
            continue;
        }
        if let Some(t) = obj.get("rewritten").and_then(|x| x.as_str()) {
            let t = t.trim();
            if !t.is_empty() {
                out.push((line, t.to_string()));
            }
        }
    }
    if out.is_empty() {
        return Err("无有效改写条目".into());
    }
    Ok(out)
}

fn parse_review_issues(text: &str) -> Result<Vec<ReviewIssue>, String> {
    let cleaned = extract_json_value(text);
    let t = cleaned.trim();
    let v: Value = serde_json::from_str(t).map_err(|e| format!("json parse: {e}"))?;
    let arr = v.as_array().ok_or_else(|| "顶层不是数组".to_string())?;
    let mut out = Vec::new();
    for item in arr {
        let obj = item.as_object().ok_or_else(|| "数组元素不是对象".to_string())?;
        let get_str = |k: &str| -> String {
            obj.get(k)
                .and_then(|x| x.as_str())
                .map(|s| s.trim().to_string())
                .unwrap_or_default()
        };
        let severity = obj
            .get("severity")
            .and_then(|x| x.as_u64())
            .map(|s| s.clamp(1, 3) as u8)
            .unwrap_or(2);
        out.push(ReviewIssue {
            dimension: get_str("dimension"),
            severity,
            quote: get_str("quote"),
            problem: get_str("problem"),
            fix_instruction: get_str("fix_instruction"),
            status: REVIEW_STATUS_OPEN.to_string(),
        });
    }
    // 丢弃空维度（防 LLM 乱输出）
    out.retain(|i| !i.dimension.is_empty());
    Ok(out)
}

/// 提取文本中首个 JSON 值（数组或对象）；兼容 ```json 围栏 + 尾部多余文本。
/// 括号配对扫描（含字符串转义），从首个 [/{ 到其精确闭合，丢弃尾部说明文字。
fn extract_json_value(text: &str) -> String {
    let t = text.trim();
    let mut body = t.to_string();
    // 剥 ```json ... ``` 围栏块（若有）
    if let Some(start) = body.find("```") {
        let after = &body[start + 3..];
        let after = after
            .strip_prefix("json")
            .or_else(|| after.strip_prefix("JSON"))
            .unwrap_or(after);
        if let Some(end) = after.find("```") {
            body = after[..end].to_string();
        }
    }
    let body = body.trim();
    if let Some(st) = body.find(['[', '{']) {
        let bytes = body.as_bytes();
        let mut depth = 0i32;
        let mut in_str = false;
        let mut esc = false;
        let mut close = None;
        for (i, &b) in bytes.iter().enumerate().skip(st) {
            if in_str {
                if esc {
                    esc = false;
                } else if b == b'\\' {
                    esc = true;
                } else if b == b'"' {
                    in_str = false;
                }
                continue;
            }
            match b {
                b'"' => in_str = true,
                b'[' | b'{' => depth += 1,
                b']' | b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(i + 1);
                        break;
                    }
                }
                _ => {}
            }
        }
        if let Some(en) = close {
            return body[st..en].to_string();
        }
    }
    body.to_string()
}

/// 从 LLM 文本提取布尔字段（稳健：支持 {"resolved": true} 或 裸 true/false）。
fn parse_bool_field(text: &str, key: &str) -> Option<bool> {
    let t = extract_json_value(text);
    if let Ok(v) = serde_json::from_str::<Value>(&t) {
        if let Some(b) = v.get(key).and_then(|x| x.as_bool()) {
            return Some(b);
        }
    }
    if t.contains("true") {
        return Some(true);
    }
    if t.contains("false") {
        return Some(false);
    }
    None
}

fn now_ms() -> String {
    format!("{:013}", now_millis())
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
#[cfg(test)]
mod post_refine_tests {
    use super::*;

    #[test]
    fn parse_refine_rewrites_basic_array() {
        let text = r#"[
            {"line": 3, "rewritten": "他压低声音，指节叩了叩桌面。"},
            {"line": 7, "rewritten": null},
            {"line": 99, "rewritten": "越界行号应被丢弃"},
            {"line": 0, "rewritten": "零行号应被丢弃"}
        ]"#;
        let out = parse_refine_rewrites(text, 10).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, 3);
        assert_eq!(out[0].1, "他压低声音，指节叩了叩桌面。");
    }

    #[test]
    fn parse_refine_rewrites_tolerates_fences_and_prose() {
        let text = "好的，以下是改写结果：\n```json\n[{\"line\":2,\"rewritten\":\"雨停了。\"}]\n```\n以上就是全部改写。";
        let out = parse_refine_rewrites(text, 5).unwrap();
        assert_eq!(out, vec![(2usize, "雨停了。".to_string())]);
    }

    #[test]
    fn parse_refine_rewrites_empty_is_err() {
        assert!(parse_refine_rewrites("[]", 10).is_err());
        assert!(parse_refine_rewrites("不是json", 10).is_err());
    }

    #[test]
    fn refine_body_defaults() {
        let body: PostRefineBody =
            serde_json::from_str(r#"{"content":"正文"}"#).unwrap();
        assert!(body.min_severity.is_none());
        assert!(body.max_issues.is_none());
        let sev = body.min_severity.unwrap_or(2).clamp(1, 3);
        let cap = body.max_issues.unwrap_or(6).clamp(1, 20);
        assert_eq!((sev, cap), (2, 6));
    }
}
