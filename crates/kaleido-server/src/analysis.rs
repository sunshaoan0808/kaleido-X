//! AI writing-analysis tasks API (P3).
//!
//! Routes (all require session auth):
//! - `GET  /api/v1/analysis/kinds`                                     list supported task kinds
//! - `GET  /api/v1/works/{work_id}/analysis/tasks?kind=`                list tasks (default all)
//! - `POST /api/v1/works/{work_id}/analysis/tasks`                      create + start task -> 201
//! - `GET  /api/v1/analysis/tasks/{id}`                                 get task detail (200|404)
//! - `DELETE /api/v1/analysis/tasks/{id}`                               delete task (204)
//! - `POST /api/v1/analysis/tasks/{id}/cancel`                          cancel task
//! - `GET  /api/v1/analysis/tasks/{id}/suggestions?status=`             list suggestions
//! - `POST /api/v1/analysis/tasks/{id}/suggestions/{sid}/confirm`       confirm suggestion
//! - `POST /api/v1/analysis/tasks/{id}/suggestions/{sid}/reject`        reject suggestion
//!
//! Execution: creates a jobs-v2 record (`analysis.<kind>`), spawns a tokio task
//! that reads scope paths from the workspace jail, calls an OpenAI-compatible
//! chat/completions endpoint, then persists {summary, evidence, suggestions}.

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use kaleido_core::analysis_store::{is_valid_kind, AnalysisError, AnalysisResultBody};
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::Duration as StdDuration;

use crate::{session_from, AppState};
use crate::error_codes::*;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/analysis/kinds", get(kinds_h))
        .route(
            "/api/v1/works/{work_id}/analysis/tasks",
            get(list_tasks_h).post(create_task_h),
        )
        .route(
            "/api/v1/analysis/tasks/{id}",
            get(get_task_h).delete(delete_task_h),
        )
        .route("/api/v1/analysis/tasks/{id}/cancel", post(cancel_task_h))
        .route(
            "/api/v1/analysis/tasks/{id}/suggestions",
            get(list_suggestions_h),
        )
        .route(
            "/api/v1/analysis/tasks/{id}/suggestions/{sid}/confirm",
            post(confirm_suggestion_h),
        )
        .route(
            "/api/v1/analysis/tasks/{id}/suggestions/{sid}/reject",
            post(reject_suggestion_h),
        )
        // T4/T5 孤岛打通（2026-08-14）：relation-evolution / emotion-curve / character-arc
        // 原为纯函数+单测孤岛（无路由），数据源直连 graph.sqlite / pack 章节正文，零 LLM 调用。
        .route("/api/v1/analysis/relation-evolution", get(relation_evolution_h))
        .route("/api/v1/analysis/emotion-curve", post(emotion_curve_h))
        .route("/api/v1/analysis/character-arc", get(character_arc_h))
}

// ─── T4/T5 孤岛打通 handlers ────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct PackAnalysisBody {
    pack_id: String,
    /// 最多分析前 N 章（默认全部）
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct WorkAnalysisQuery {
    /// 缺省取会话 workspace
    work_id: Option<String>,
}

/// GET /api/v1/analysis/relation-evolution?work_id=xxx
/// 关系演化（吸收自 ai-novel-screenplay-analyzer T4）：跨章趋势
/// （stable/warming/cooling/volatile）+ 角色画像，纯启发式，无 LLM。
async fn relation_evolution_h(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<WorkAnalysisQuery>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let work_id = q.work_id.unwrap_or(session.workspace_id);
    match state.graph.list(&work_id) {
        Ok((_chars, rels)) => {
            let evolutions = crate::relation_evolution::RelationEvolution::build_evolution(&rels);
            let profiles = crate::relation_evolution::CharacterProfile::build_profiles(&rels);
            Json(json!({
                "ok": true,
                "work_id": work_id,
                "evolutions": evolutions,
                "profiles": profiles,
            }))
            .into_response()
        }
        Err(e) => return internal("AN_INTERNAL", format!("{e}")),
    }
}

/// POST /api/v1/analysis/emotion-curve {"pack_id", "limit"?}
/// 情感曲线（吸收自 novel2hermes_jp T5）：逐章峰值强度/主导情绪/曲线形态 +
/// 整体弧线，纯启发式（EMOTION_LEXICON 词表），无 LLM。
async fn emotion_curve_h(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<PackAnalysisBody>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let pack = match state.packs.get(&body.pack_id) {
        Ok(p) => p,
        Err(e) => {
            return not_found("AN_NOT_FOUND", format!("{e}"));
        }
    };
    let mut texts = Vec::new();
    for ch in pack.chapters.iter().take(body.limit.unwrap_or(usize::MAX)) {
        if ch.body_path.is_empty() {
            continue;
        }
        if let Ok(t) = state.packs.read_chapter_body(&body.pack_id, &ch.body_path) {
            texts.push(kaleido_core::emotion_curve::ChapterText {
                chapter: ch.title.clone(),
                text: t,
            });
        }
    }
    if texts.is_empty() {
        return bad_request("AN_BAD_REQUEST", "pack 无可用章节正文（chapters 缺 body_path 或文件缺失）");
    }
    let curve = kaleido_core::emotion_curve::build_emotion_curve(&texts);
    Json(json!({
        "ok": true,
        "pack_id": body.pack_id,
        "chapters_analyzed": texts.len(),
        "curve": curve,
    }))
    .into_response()
}

/// GET /api/v1/analysis/character-arc?work_id=xxx
/// 角色弧（吸收自 novel2hermes_jp T5）：从关系图派生字段变化
/// （关系在章节间的出现推进，field=与XX的关系(类别)，from/to=前后出现章），纯启发式。
async fn character_arc_h(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<WorkAnalysisQuery>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let work_id = q.work_id.unwrap_or(session.workspace_id);
    match state.graph.list(&work_id) {
        Ok((_chars, rels)) => {
            let mut entries = Vec::new();
            for rel in &rels {
                for w in rel.chapters.windows(2) {
                    entries.push(kaleido_core::character_arc::ArcEntry {
                        character: rel.from_char.clone(),
                        chapter: w[1].clone(),
                        field: format!("与{}的关系({})", rel.to_char, rel.category),
                        from: w[0].clone(),
                        to: w[1].clone(),
                    });
                }
            }
            let arcs = kaleido_core::character_arc::build_character_arcs(&entries);
            Json(json!({
                "ok": true,
                "work_id": work_id,
                "arcs": arcs,
                "derived_from": "graph.relationships",
            }))
            .into_response()
        }
        Err(e) => return internal("AN_INTERNAL", format!("{e}")),
    }
}

fn analysis_err(e: AnalysisError) -> Response {
    let (code, body) = match e {
        AnalysisError::Db(e) => (StatusCode::INTERNAL_SERVER_ERROR, json!({ "error": format!("{e}"), "code": "AN_INTERNAL" })),
        AnalysisError::NotFound(what) => (StatusCode::NOT_FOUND, json!({ "error": format!("{what} not found"), "code": "AN_NOT_FOUND" })),
        AnalysisError::InvalidStatus(s) => (
            StatusCode::BAD_REQUEST,
            json!({ "error": format!("invalid status '{s}'") }),
        ),
        AnalysisError::InvalidKind(k) => (
            StatusCode::BAD_REQUEST,
            json!({ "error": format!("invalid analysis kind '{k}'") }),
        ),
        AnalysisError::BadRequest(m) => (StatusCode::BAD_REQUEST, json!({ "error": m, "code": "AN_BAD_REQUEST" })),
    };
    (code, Json(body)).into_response()
}

async fn kinds_h(State(state): State<AppState>) -> Response {
    let _ = &state;
    Json(json!({
        "kinds": [
            {"value": "chapter-analysis", "label": "章节理解", "desc": "分析所选章节，生成情节概要，并提取事件、出场角色、设定、原文证据和不确定项。"},
            {"value": "character-extraction", "label": "全书角色抽取", "desc": "扫描分析范围内的正文，识别有跨章节意义的角色及可靠别名，并创建或更新角色档案。"},
            {"value": "character-identity-audit", "label": "角色身份审计", "desc": "审查角色身份描述的一致性，标记矛盾与存疑描述。"},
            {"value": "timeline-analysis", "label": "时间线分析", "desc": "抽取时间线事件与时间锚点，校验时间逻辑，给出时间线视图。"},
            {"value": "relationship-analysis", "label": "人物关系分析", "desc": "抽取人物关系及其变化，带原文证据，供确认后回写关系图。"},
            {"value": "worldview-analysis", "label": "世界观分析", "desc": "抽取世界观设定（力量体系、地理、势力、种族、科技等）。"},
            {"value": "setting-extraction", "label": "设定抽取", "desc": "抽取设定条目（物品、地点、规则、伏笔设定），供确认后入库。"},
            {"value": "consistency-check", "label": "一致性检查", "desc": "检查前后文矛盾（事实/称谓/时间/设定），列出问题清单。"},
            {"value": "book-analysis", "label": "全书分析", "desc": "对全书做总体分析：主题、结构、角色弧、叙事节奏。"}
        ]
    }))
    .into_response()
}

#[derive(Deserialize)]
struct ListTasksQuery {
    kind: Option<String>,
}

async fn list_tasks_h(
    State(state): State<AppState>,
    Path(work_id): Path<String>,
    Query(q): Query<ListTasksQuery>,
    headers: HeaderMap,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    if session.workspace_id != work_id {
        return forbidden("AN_FORBIDDEN_SCOPE", "work not in session workspace");
    }
    if let Some(k) = q.kind.as_deref() {
        if !is_valid_kind(k) {
            return analysis_err(AnalysisError::InvalidKind(k.to_string()));
        }
    }
    match state.analysis.list_tasks(&work_id, q.kind.as_deref()) {
        Ok(tasks) => Json(json!({ "tasks": tasks })).into_response(),
        Err(e) => analysis_err(e),
    }
}

#[derive(Deserialize)]
struct CreateTaskBody {
    kind: String,
    #[serde(default)]
    scope: Value,
}

async fn create_task_h(
    State(state): State<AppState>,
    Path(work_id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<CreateTaskBody>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    if session.workspace_id != work_id {
        return forbidden("AN_FORBIDDEN_SCOPE", "work not in session workspace");
    }
    if !is_valid_kind(&body.kind) {
        return analysis_err(AnalysisError::InvalidKind(body.kind.clone()));
    }
    let scope = if body.scope.is_null() { json!({}) } else { body.scope.clone() };
    let run_id = match state.jobs.create(
        &format!("analysis.{}", body.kind),
        &session.user_id,
        &work_id,
        json!({ "task_id": "__pending__", "scope": scope }),
        None,
        None,
    ) {
        Ok(j) => j.run_id,
        Err(e) => return internal("AN_INTERNAL", e.to_string()),
    };
    let mut scope_with_run = scope.clone();
    if let Some(obj) = scope_with_run.as_object_mut() {
        obj.insert("run_id".into(), json!(run_id));
    } else {
        scope_with_run = json!({ "run_id": run_id });
    }
    let task = match state.analysis.create_task(&work_id, &body.kind, scope_with_run, &session.user_id) {
        Ok(t) => t,
        Err(e) => {
            let _ = state.jobs.cancel(&run_id);
            return analysis_err(e);
        }
    };
    let st = state.clone();
    let tid = task.id.clone();
    tokio::spawn(async move {
        run_analysis_task(st, tid, run_id.clone()).await;
    });
    (StatusCode::CREATED, Json(json!({ "task": task }))).into_response()
}

async fn get_task_h(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    match state.analysis.get_task(&id) {
        Ok(t) if t.work_id == session.workspace_id => Json(json!({ "task": t })).into_response(),
        Ok(_) => forbidden("AN_FORBIDDEN_SCOPE", "forbidden"),
        Err(e) => analysis_err(e),
    }
}

async fn delete_task_h(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let task = match state.analysis.get_task(&id) {
        Ok(t) => t,
        Err(e) => return analysis_err(e),
    };
    if task.work_id != session.workspace_id {
        return forbidden("AN_FORBIDDEN_SCOPE", "forbidden");
    }
    match state.analysis.delete_task(&id) {
        Ok(()) => (StatusCode::NO_CONTENT, Json(json!({ "ok": true }))).into_response(),
        Err(e) => analysis_err(e),
    }
}

async fn cancel_task_h(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let task = match state.analysis.get_task(&id) {
        Ok(t) => t,
        Err(e) => return analysis_err(e),
    };
    if task.work_id != session.workspace_id {
        return forbidden("AN_FORBIDDEN_SCOPE", "forbidden");
    }
    // Best-effort job cancel; always mark the task cancelled.
    if let Some(run_id) = task.scope.get("run_id").and_then(|v| v.as_str()) {
        let _ = state.jobs.cancel(run_id);
    }
    match state.analysis.set_status(&id, "cancelled") {
        Ok(t) => Json(json!({ "task": t })).into_response(),
        Err(e) => analysis_err(e),
    }
}

#[derive(Deserialize)]
struct SuggestionsQuery {
    status: Option<String>,
}

async fn list_suggestions_h(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<SuggestionsQuery>,
    headers: HeaderMap,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let task = match state.analysis.get_task(&id) {
        Ok(t) => t,
        Err(e) => return analysis_err(e),
    };
    if task.work_id != session.workspace_id {
        return forbidden("AN_FORBIDDEN_SCOPE", "forbidden");
    }
    match state.analysis.list_suggestions(&id, q.status.as_deref()) {
        Ok(sugs) => Json(json!({ "suggestions": sugs })).into_response(),
        Err(e) => analysis_err(e),
    }
}

async fn confirm_suggestion_h(
    State(state): State<AppState>,
    Path((id, sid)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    suggestion_decision(&state, &headers, &id, &sid, "confirmed").await
}

async fn reject_suggestion_h(
    State(state): State<AppState>,
    Path((id, sid)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    suggestion_decision(&state, &headers, &id, &sid, "rejected").await
}

async fn suggestion_decision(
    state: &AppState,
    headers: &HeaderMap,
    id: &str,
    sid: &str,
    decision: &str,
) -> Response {
    let session = match session_from(state, headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let task = match state.analysis.get_task(id) {
        Ok(t) => t,
        Err(e) => return analysis_err(e),
    };
    if task.work_id != session.workspace_id {
        return forbidden("AN_FORBIDDEN_SCOPE", "forbidden");
    }
    let res = if decision == "confirmed" {
        state.analysis.confirm_suggestion(id, sid)
    } else {
        state.analysis.reject_suggestion(id, sid)
    };
    let sug = match res {
        Ok(s) => s,
        Err(e) => return analysis_err(e),
    };
    // P0 闭环: confirm 成功后跨 store apply（意图原子翻转先行，副作用幂等可重试）。
    // 失败不阻塞：apply_error 记录 + 200 返回，前端可展示重试态。
    if decision == "confirmed" {
        let applied = apply_suggestion(state, &task, &sug);
        let (applied_at, apply_error) = match applied {
            Ok(()) => (Some(kaleido_core::analysis_store::now_ts()), None),
            Err(msg) => (None, Some(msg)),
        };
        let _ = state.analysis.mark_suggestion_applied(sid, applied_at.as_deref(), apply_error.as_deref());
    }
    Json(json!({ "suggestion": sug })).into_response()
}

/// 跨 store 编排：按 suggestion kind 分发到 graph_store / partner 等。
/// 失败返回 Err(message)（fail-open，suggestion 保持 confirmed，可重试）。
fn apply_suggestion(state: &AppState, task: &kaleido_core::analysis_store::AnalysisTask, sug: &kaleido_core::analysis_store::AnalysisSuggestion) -> Result<(), String> {
    // LLM 输出的 kind 是中文（"关系"/"设定"/"角色"…），按 payload 形状分发更稳。
    let payload = &sug.payload;
    let has = |k: &str| payload.get(k).and_then(|v| v.as_str()).map(|s| !s.trim().is_empty()).unwrap_or(false);
    // 关系类: payload 含 from/to
    if has("from") && has("to") {
        let from = payload.get("from").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
        let to = payload.get("to").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
        let category = normalize_category(payload.get("category").and_then(|v| v.as_str()).unwrap_or(""));
        let subtype = payload.get("subtype").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let note = payload.get("note").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let chapter = payload.get("chapter").and_then(|v| v.as_str());
        if from.is_empty() || to.is_empty() {
            return Err("relationship payload: from/to 为空".into());
        }
        if from == to {
            return Err(format!("relationship payload: from==to（{from}），忽略自环"));
        }
        return state
            .graph
            .create_relationship_from_suggestion(&task.work_id, &from, &to, None, None, &category, &subtype, &note, chapter, &sug.id)
            .map(|_| ())
            .map_err(|e| format!("graph apply 失败: {e}"));
    }
    // 设定类: payload 含 title/content（worldview-analysis / setting-extraction）
    if has("title") && has("content") {
        // P0: 世界书写回暂未接入（partner 无直接 upsert 入口），fail-open 记录。
        // P1: 经 st_world_info 导入层落世界书。
        return Err(format!("设定类建议（kind={}）落库为 P1 项：暂未接入 partner 写入，建议已确认待办", sug.kind));
    }
    // 其它/未知: fail-open
    Err(format!("未识别的建议类型 kind={}，零副作用跳过（可用性优先）", sug.kind))
}

/// 把 LLM 自由文本 category 归一化到 graph 合法枚举。
fn normalize_category(cat: &str) -> String {
    let c = cat.trim().to_lowercase();
    for k in ["family", "social", "emotional", "conflict", "uncertain"] {
        if c.contains(k) {
            return k.to_string();
        }
    }
    // 中文近似（ST-FIX: 不用裸「亲」——「亲密/亲近」是情感不是亲属，
    // 只有「亲人/亲戚/亲属/家人/亲情/血缘」等才算 family）
    if c.contains("亲人") || c.contains("亲戚") || c.contains("亲属") || c.contains("家人")
        || c.contains("亲情") || c.contains("血缘") || c.contains("家")
    {
        "family".into()
    } else if c.contains("友") || c.contains("社交") || c.contains("同门") || c.contains("同事") || c.contains("互动") {
        "social".into()
    } else if c.contains("爱") || c.contains("情") || c.contains("恋") || c.contains("亲密") || c.contains("暧昧") {
        "emotional".into()
    } else if c.contains("仇") || c.contains("敌") || c.contains("冲突") || c.contains("对立") {
        "conflict".into()
    } else {
        "uncertain".into()
    }
}

// ---- execution ----

const MAX_TOTAL_CHARS: usize = 28_000;
const MAX_PER_FILE_CHARS: usize = 14_000;

/// relationship-analysis 的参考提示块：已有关系轨迹（graph） + 每角色原文证据（embed 检索）。
/// 任何失败（图查询 / 检索 / 空数据）都静默降级返回空串，调用方不阻断分析。
/// 整个块控制在 4000 字符内，超出截断，避免 token 爆炸。
async fn build_relationship_hint(
    state: &AppState,
    work_id: &str,
    chunks: &[(String, String)],
) -> String {
    let (characters, relationships) = match state.graph.list(work_id) {
        Ok(x) => x,
        Err(_) => return String::new(), // 图查询失败降级
    };
    if characters.is_empty() && relationships.is_empty() {
        return String::new(); // 空图降级
    }
    let mut hint = String::new();
    // 2. 已有关系行：from -[category/subtype]-> to (chapters)
    for r in &relationships {
        if r.confirmation_status != "confirmed" {
            continue;
        }
        let chs = if r.chapters.is_empty() {
            String::new()
        } else {
            format!("【{}】", r.chapters.join("、"))
        };
        hint.push_str(&format!("{}-[{}:{}]->{} {}\n", r.from_char, r.category, r.subtype, r.to_char, chs));
    }
    // 3. 每角色原文证据（top_k=3）；任一角色失败只跳过该角色
    for ch in &characters {
        let names: Vec<String> = std::iter::once(ch.name.clone())
            .chain(ch.aliases.iter().cloned())
            .collect();
        let evidence = crate::convert::retrieve_character_evidence(state, chunks, &ch.name, &ch.aliases, 3).await;
        if let Ok(ev) = evidence {
            if !ev.is_empty() {
                hint.push_str(&format!("角色 {}({}):\n{}\n", ch.name, names.join("/"), ev));
            }
        }
    }
    if hint.trim().is_empty() {
        return String::new();
    }
    let mut block = String::from("\n\n【已有关系与原文证据参考（用于修正/延续关系轨迹，不是唯一依据）】\n");
    block.push_str(&hint);
    if block.chars().count() > 4000 {
        block = block.chars().take(4000).collect();
    }
    block
}

/// P1: 宽容降级的逐项校验。整体解析失败时逐项保留合法 suggestion、丢弃非法项并计数。
/// - 每个元素必须是 JSON object，否则丢弃 +1
/// - kind 必须为字符串且在 ANALYSIS_KINDS 白名单；缺 kind 或不在白名单 → 丢弃 +1
/// - kind 含 `关系` 或 `relationship` 时：payload 必须是 object，且 payload.from/to 均为非空字符串；
///   缺失 → 丢弃 +1（带 `chapter` 字段属于合法，以 from/to 为主，chapter 缺失不丢弃）
/// - 其他 kind：payload 是 object 即可（title/content/name/issue 等不强校验；退化 object 也保留，可用性优先）
fn sanitize_suggestions(sugs: &mut Vec<Value>) -> usize {
    let mut dropped = 0usize;
    let mut kept: Vec<Value> = Vec::with_capacity(sugs.len());
    for sug in sugs.drain(..) {
        let Some(map) = sug.as_object() else {
            dropped += 1;
            continue;
        };
        let kind_ok = match map.get("kind").and_then(|v| v.as_str()) {
            Some(k) => is_valid_kind(k),
            None => false,
        };
        if !kind_ok {
            dropped += 1;
            continue;
        }
        let kind = map.get("kind").and_then(|v| v.as_str()).unwrap_or_default();
        let payload_ok = map.get("payload").map(|p| p.is_object()).unwrap_or(false);
        if !payload_ok {
            dropped += 1;
            continue;
        }
        if kind.contains("关系") || kind.contains("relationship") {
            let valid = map
                .get("payload")
                .and_then(|p| p.as_object())
                .map(|p| {
                    let from = p.get("from").and_then(|v| v.as_str()).map(|s| !s.is_empty()).unwrap_or(false);
                    let to = p.get("to").and_then(|v| v.as_str()).map(|s| !s.is_empty()).unwrap_or(false);
                    from && to
                })
                .unwrap_or(false);
            if !valid {
                dropped += 1;
                continue;
            }
        }
        kept.push(sug);
    }
    *sugs = kept;
    dropped
}

async fn run_analysis_task(state: AppState, task_id: String, run_id: String) {
    let _ = state.analysis.set_status(&task_id, "running");
    let task = match state.analysis.get_task(&task_id) {
        Ok(t) => t,
        Err(e) => {
            let _ = state.jobs.complete(&run_id, "failed", None, Some(e.to_string()));
            return;
        }
    };
    // Read scope paths from workspace jail.
    let mut chunks: Vec<(String, String)> = Vec::new();
    let mut total = 0usize;
    if let Some(paths) = task.scope.get("paths").and_then(|v| v.as_array()) {
        for p in paths.iter().filter_map(|v| v.as_str()) {
            match state.works.read_text(&task.work_id, p) {
                Ok(f) => {
                    let mut content: String = f.content.chars().take(MAX_PER_FILE_CHARS).collect();
                    content.truncate(MAX_PER_FILE_CHARS);
                    total += content.chars().count();
                    if total > MAX_TOTAL_CHARS {
                        let overflow = total - MAX_TOTAL_CHARS;
                        let keep = content.chars().count().saturating_sub(overflow);
                        content = content.chars().take(keep).collect();
                        chunks.push((p.to_string(), content));
                        break;
                    }
                    chunks.push((p.to_string(), content));
                }
                Err(e) => {
                    let _ = state.analysis.fail_task(&task_id, &format!("cannot read {}: {e}", p));
                    let _ = state.jobs.complete(&run_id, "failed", None, Some(format!("cannot read {}: {e}", p)));
                    return;
                }
            }
        }
    }
    if chunks.is_empty() {
        let _ = state.analysis.fail_task(&task_id, "no readable scope paths");
        let _ = state.jobs.complete(&run_id, "failed", None, Some("no readable scope paths".into()));
        return;
    }
    // Check for cancellation between read and LLM call.
    if let Ok(cur) = state.analysis.get_task(&task_id) {
        if cur.status == "cancelled" {
            let _ = state.jobs.complete(&run_id, "cancelled", None, None);
            return;
        }
    }
    let system = system_prompt(&task.kind);
    // P0-2: relationship-analysis 追加"已有关系轨迹 + 原文证据"参考块。
    // 图查询/检索失败或数据为空一律静默降级（返回空串），不阻断分析。
    let relationship_hint = if task.kind == "relationship-analysis" {
        build_relationship_hint(&state, &task.work_id, &chunks).await
    } else {
        String::new()
    };
    let mut user = String::from("请分析以下小说正文。输出必须为严格 JSON（不要 markdown 代码块），结构：");
    user.push_str(r#"{"summary": {...}, "evidence": [{"source": "文件名", "line": 行号, "quote": "原文摘录", "note": "说明"}], "suggestions": [{"kind": "关系/角色/设定/时间线/通用", "payload": {...}}]}"#);
    if !relationship_hint.is_empty() {
        user.push_str(&relationship_hint);
    }
    user.push_str("\n\n正文：\n");
    for (p, c) in &chunks {
        user.push_str(&format!("\n--- {p} ---\n{c}\n"));
    }
    match call_llm(&state, &system, &user).await {
        Ok(mut v) => {
            // Normalize summary keys: some models return Chinese keys instead of the
            // English schema (plot/key_events/characters/settings/uncertain).
            if let Some(summary) = v.get_mut("summary").and_then(|s| s.as_object_mut()) {
                let mut norm: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
                for (k, val) in summary.iter() {
                    let target = match k.as_str() {
                        "情节概要" => Some("plot"),
                        "关键事件" => Some("key_events"),
                        "出场角色" => Some("characters"),
                        "设定" => Some("settings"),
                        "不确定项" => Some("uncertain"),
                        _ => None,
                    };
                    match target {
                        Some(t) => {
                            if !norm.contains_key(t) {
                                norm.insert(t.to_string(), val.clone());
                            }
                        }
                        None => {
                            norm.insert(k.clone(), val.clone());
                        }
                    }
                }
                *summary = norm;
            }
            let mut body: AnalysisResultBody = serde_json::from_value(v.clone()).unwrap_or_else(|_| {
                AnalysisResultBody {
                    summary: v,
                    evidence: vec![],
                    suggestions: vec![],
                    dropped_suggestions: 0,
                }
            });
            // P1: 逐项校验 suggestions，非法项丢弃并计数（宽容降级，不整体失败）
            let dropped = sanitize_suggestions(&mut body.suggestions);
            body.dropped_suggestions = dropped;
            let result_json = serde_json::to_value(&body).unwrap_or_else(|_| json!({}));
            match state.analysis.save_result(&task_id, &body) {
                Ok(_) => {
                    let _ = state.jobs.complete(&run_id, "succeeded", Some(result_json), None);
                }
                Err(e) => {
                    let _ = state.jobs.complete(&run_id, "failed", None, Some(e.to_string()));
                }
            }
        }
        Err(e) => {
            let _ = state.analysis.fail_task(&task_id, &e);
            let _ = state.jobs.complete(&run_id, "failed", None, Some(e));
        }
    }
}

async fn call_llm(state: &AppState, system: &str, user: &str) -> Result<Value, String> {
    let llm = state.app_state.resolve_llm(
        state.llm_base.as_deref(),
        state.llm_key.as_deref(),
        &state.llm_model,
    );
    if llm.base_url.trim().is_empty() || llm.api_key.trim().is_empty() {
        return Err("llm not configured".into());
    }
    let prov_kind = crate::llm_stream::runtime_provider_kind(&llm, &state.provider_kind);
    let client = reqwest::Client::builder()
        .timeout(StdDuration::from_secs(150))
        .build()
        .map_err(|e| e.to_string())?;
    // F4: provider dispatch 化（temp 0.3 / max 4096 保持原值；JSON 任务低温小额）。
    let raw = crate::llm_stream::chat_completion_dispatch(
        &llm.base_url,
        &llm.api_key,
        &llm.model,
        &prov_kind,
        system,
        user,
        0.3,
        4096,
        150,
        &client,
    )
    .await?;
    // LLM 常把 JSON 包在 ```json 围栏或带说明文字——用括号配对提取（同审稿），
    // 直接 serde_json::from_str 会 EOF（2026-08-10 mimo 实测）。
    crate::llm_stream::extract_json_value(&raw)
        .ok_or_else(|| format!("llm content is not JSON: {}", raw.chars().take(120).collect::<String>()))
}

fn system_prompt(kind: &str) -> String {
    match kind {
        "chapter-analysis" => "你是小说章节分析助手。分析给定章节，输出：summary(情节概要/关键事件/出场角色/设定/不确定项)，evidence(原文证据)，suggestions(可确认条目，kind=通用)。".into(),
        "character-extraction" => "你是小说角色抽取助手。识别有跨章节意义的角色及可靠别名，suggestions 的 kind=角色，payload 含 name/aliases/note。".into(),
        "character-identity-audit" => "你是小说角色身份审计助手。审查角色身份一致性，标记矛盾与存疑描述，suggestions 的 kind=角色审计，payload 含 name/issue/type(矛盾|存疑)。".into(),
        "timeline-analysis" => "你是小说时间线分析助手。抽取时间线事件与时间锚点，校验时间逻辑，suggestions 的 kind=时间线，payload 含 time/event/note。".into(),
        "relationship-analysis" => "你是小说人物关系分析助手。抽取人物关系及其变化，带原文证据，suggestions 的 kind=关系，payload 含 from/to/category/subtype/note。".into(),
        "worldview-analysis" => "你是小说世界观分析助手。抽取世界观设定（力量体系/地理/势力/种族/科技），suggestions 的 kind=设定，payload 含 category/title/content。".into(),
        "setting-extraction" => "你是小说设定抽取助手。抽取设定条目（物品/地点/规则/伏笔设定），suggestions 的 kind=设定，payload 含 category/title/content。".into(),
        "consistency-check" => "你是小说一致性检查助手。检查前后文矛盾（事实/称谓/时间/设定），suggestions 的 kind=问题，payload 含 issue/evidence/severity(高|中|低)。".into(),
        _ => "你是小说分析助手。根据任务要求分析正文，输出 summary/evidence/suggestions。".into(),
    }
}
