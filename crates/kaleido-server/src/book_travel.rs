//! BookTravel full pipeline (S5-W2 T3 + W2 jobization).
//!
//! Routes:
//!   POST /api/v1/book-travel/classify
//!   POST /api/v1/book-travel/start              (assemble default; mode=pipeline for full journey)
//!   POST /api/v1/book-travel/{step}            step ∈ assemble|plan_scene|write_change_scene|
//!                                                   write_insert_beat|judge_ending|summarize_memory|pipeline
//!   POST /api/v1/book-travel/stop
//!   GET  /api/v1/book-travel/stream?id=...
//!   GET  /api/v1/book-travel/runs[/{id}]       list/get book_travel jobs (workspace-scoped)
//!
//! Single steps still one job each. `pipeline` runs all travel steps in **one** job with
//! progress events, cancel checks, and result persisted under works/book-travel/.

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use kaleido_core::{is_terminal_job_status, normalize_job_status, JobEvent};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Duration as StdDuration;
use uuid::Uuid;

use crate::{map_core_err, session_from, stream_job_sse, AppState};
use crate::error_codes::*;

const STEPS: &[&str] = &[
    "assemble",
    "plan_scene",
    "write_change_scene",
    "write_insert_beat",
    "judge_ending",
    "summarize_memory",
    "pipeline",
];

/// Ordered stages for mode=pipeline (one job, multi progress).
const PIPELINE_STEPS: &[&str] = &[
    "assemble",
    "plan_scene",
    "write_change_scene",
    "write_insert_beat",
    "judge_ending",
    "summarize_memory",
];

// ---------------------------------------------------------------------------
// Request / response models
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BookTravelClassifyBody {
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub genre_hint: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BookTravelLabels {
    pub genre: String,
    pub tone: String,
    pub pov: String,
    pub era: String,
    pub themes: Vec<String>,
    pub pacing: String,
    pub audience: String,
    pub mode: String,
    pub confidence: f64,
    pub notes: Vec<String>,
}

/// Shared body for start and /{step}.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BookTravelStartBody {
    #[serde(default)]
    pub premise: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    /// assemble | plan_scene | write_change_scene | write_insert_beat | judge_ending | summarize_memory
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub step: Option<String>,
    /// Free-form user input / instruction for scene writers.
    #[serde(default)]
    pub user_input: Option<String>,
    /// Optional prior scene / plan context (from previous step result).
    #[serde(default)]
    pub context: Option<Value>,
    /// When `context` is empty, load this job's result (same workspace) as context.
    #[serde(default, alias = "previous_run_id", alias = "prevRunId")]
    pub previous_run_id: Option<String>,
    #[serde(default)]
    pub labels: Option<Value>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub payload: Option<Value>,
    /// When true, skip LLM and use templates (smoke / offline).
    #[serde(default, alias = "heuristicOnly", alias = "heuristic_only")]
    pub prefer_heuristic: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BookTravelStopBody {
    #[serde(alias = "run_id", alias = "runId", alias = "jobId")]
    pub id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BookTravelStreamQuery {
    #[serde(alias = "run_id", alias = "runId", alias = "jobId")]
    pub id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BookTravelRunsQuery {
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub status: Option<String>,
}


fn normalize_step(raw: &str) -> Option<String> {
    let s = raw.trim().to_ascii_lowercase().replace('-', "_");
    match s.as_str() {
        "assemble" | "plan" | "start" => Some("assemble".into()),
        "plan_scene" | "planscene" | "scene" => Some("plan_scene".into()),
        "write_change_scene" | "change_scene" | "change" => Some("write_change_scene".into()),
        "write_insert_beat" | "insert_beat" | "beat" | "insert" => Some("write_insert_beat".into()),
        "judge_ending" | "ending" | "judge" => Some("judge_ending".into()),
        "summarize_memory" | "memory" | "summarize" => Some("summarize_memory".into()),
        "pipeline" | "full" | "all" | "journey" | "full_pipeline" => Some("pipeline".into()),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

pub async fn classify(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<BookTravelClassifyBody>,
) -> axum::response::Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };

    let text = body.text.as_deref().unwrap_or("").trim();
    if text.is_empty() && body.title.as_deref().unwrap_or("").trim().is_empty() {
        return bad_request("BT_INPUT", "需要 text 或 title");
    }

    let title = body.title.as_deref().unwrap_or("").trim();
    let genre_hint = body.genre_hint.as_deref();

    let (labels, generation_mode, fallback) =
        match try_classify_llm(&state, text, title, genre_hint).await {
            Some(labels) => (labels, "llm", false),
            None => (
                heuristic_classify(text, body.title.as_deref(), genre_hint),
                "heuristic",
                true,
            ),
        };

    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "labels": labels,
            "workspaceId": session.workspace_id,
            "userId": session.user_id,
            "generationMode": generation_mode,
            "mvp": true,
            "fallback": fallback,
        })),
    )
        .into_response()
}

/// Soft-fail LLM classify: returns None on missing LLM config, transport, or parse errors.
async fn try_classify_llm(
    state: &AppState,
    text: &str,
    title: &str,
    genre_hint: Option<&str>,
) -> Option<BookTravelLabels> {
    let llm = state.app_state.resolve_llm(
        state.llm_base.as_deref(),
        state.llm_key.as_deref(),
        &state.llm_model,
    );
    let prov_kind = crate::llm_stream::runtime_provider_kind(&llm, &state.provider_kind);
    if llm.base_url.trim().is_empty() || llm.api_key.trim().is_empty() {
        return None;
    }
    let model = if llm.model.is_empty() {
        state.llm_model.clone()
    } else {
        llm.model.clone()
    };

    let system = r#"你是小说题材分类助手。根据用户提供的标题与正文片段，输出严格纯 JSON（不要 Markdown 代码块），字段必须齐全：
{
  "genre": string,
  "tone": string,
  "pov": string,
  "era": string,
  "themes": string[],
  "pacing": string,
  "audience": string,
  "mode": "llm",
  "confidence": number,
  "notes": string[]
}
genre 可用：science_fiction, fantasy, mystery, romance, horror, historical, xianxia, general_fiction 等。
tone：dark/light/epic/neutral；pov：first_person/second_person/third_person；
era：near_future/pre_modern/historical_modern/contemporary；
pacing：slow/moderate/fast；audience：children/young_adult/adult。
confidence 在 0~1 之间。只返回 JSON。"#.to_string();

    let text_l = crate::llm_stream::limit_text(text, 4000);
    let mut user = format!(
        "标题：{}
正文片段：
{}",
        if title.is_empty() { "(无)" } else { title },
        if text_l.is_empty() { "(无)" } else { &text_l }
    );
    if let Some(h) = genre_hint.map(str::trim).filter(|s| !s.is_empty()) {
        user.push_str(&format!("
用户 genre 提示：{h}"));
    }

    let full = match crate::llm_stream::stream_chat_completions_dispatch(
        &llm.base_url,
        &llm.api_key,
        &model,
        &prov_kind,
        &system,
        &user,
        0.3,
        800,
        60,
        |_chunk| true,
    )
    .await
    {
        Ok(s) => s,
        Err(_) => return None,
    };

    let v = crate::llm_stream::extract_json_value(&full)?;
    labels_from_json(&v, genre_hint)
}

fn labels_from_json(v: &Value, genre_hint: Option<&str>) -> Option<BookTravelLabels> {
    let str_field = |key: &str| -> Option<String> {
        v.get(key)
            .and_then(|x| x.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };
    let mut genre = str_field("genre")?;
    if let Some(h) = genre_hint.map(str::trim).filter(|s| !s.is_empty()) {
        // honor explicit hint when present
        genre = h.to_string();
    }
    let tone = str_field("tone").unwrap_or_else(|| "neutral".into());
    let pov = str_field("pov").unwrap_or_else(|| "third_person".into());
    let era = str_field("era").unwrap_or_else(|| "contemporary".into());
    let pacing = str_field("pacing").unwrap_or_else(|| "moderate".into());
    let audience = str_field("audience").unwrap_or_else(|| "adult".into());
    let themes: Vec<String> = v
        .get("themes")
        .and_then(|t| t.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(|s| s.trim().to_string()))
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let themes = if themes.is_empty() {
        vec!["discovery".into()]
    } else {
        themes
    };
    let confidence = v
        .get("confidence")
        .and_then(|c| c.as_f64())
        .unwrap_or(0.7)
        .clamp(0.0, 1.0);
    let mut notes: Vec<String> = v
        .get("notes")
        .and_then(|t| t.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(|s| s.trim().to_string()))
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    if notes.is_empty() {
        notes.push("classified by llm".into());
    }

    Some(BookTravelLabels {
        genre,
        tone,
        pov,
        era,
        themes,
        pacing,
        audience,
        mode: "llm".into(),
        confidence,
        notes,
    })
}

fn context_is_empty(ctx: &Option<Value>) -> bool {
    match ctx {
        None => true,
        Some(Value::Null) => true,
        Some(Value::String(s)) => s.trim().is_empty(),
        Some(Value::Object(m)) => m.is_empty(),
        Some(Value::Array(a)) => a.is_empty(),
        Some(_) => false,
    }
}

fn load_previous_result(
    state: &AppState,
    session: &kaleido_core::SessionRecord,
    previous_run_id: &str,
) -> Result<Value, axum::response::Response> {
    let run_id = previous_run_id.trim();
    if run_id.is_empty() {
        return Err(bad_request("BT_EMPTY", "previousRunId is empty"));
    }
    match state.jobs.get(run_id) {
        None => Err(not_found("BT_NOT_FOUND", format!("previous job not found: {run_id}"))),
        Some(j) if j.workspace_id != session.workspace_id && j.user_id != session.user_id => Err(forbidden("BT_FORBIDDEN_SCOPE", "previous job belongs to another workspace")),
        Some(j) => match j.result {
            Some(r) => Ok(r),
            None => Err(err_with_code(
            StatusCode::BAD_REQUEST,
            "BT_BAD_REQUEST", "previous job has no result yet",
            serde_json::json!({"previousRunId": run_id,
                    "status": normalize_job_status(&j.status)}))),
        },
    }
}

fn start_inner(
    state: AppState,
    session: kaleido_core::SessionRecord,
    body: BookTravelStartBody,
    step_override: Option<String>,
) -> axum::response::Response {
    let raw = step_override
        .or_else(|| body.step.clone())
        .or_else(|| body.mode.clone())
        .unwrap_or_else(|| "assemble".into());
    let Some(step) = normalize_step(&raw) else {
        return bad_request("BT_BAD_REQUEST", format!("step/mode must be one of: {}", STEPS.join(", ")));
    };

    let text = body
        .text
        .clone()
        .or_else(|| body.premise.clone())
        .unwrap_or_default();
    let user_input = body.user_input.clone().unwrap_or_default();

    // Prefer explicit context; else inject previous job result when previousRunId set.
    let mut context = body.context.clone();
    if context_is_empty(&context) {
        if let Some(prev) = body
            .previous_run_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            match load_previous_result(&state, &session, prev) {
                Ok(r) => context = Some(r),
                Err(resp) => return resp,
            }
        }
    }

    let mut payload = body.payload.unwrap_or_else(|| json!({}));
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("mode".into(), json!(step));
        obj.insert("step".into(), json!(step));
        if !text.is_empty() {
            obj.insert("premise".into(), json!(text.clone()));
            obj.insert("text".into(), json!(text.clone()));
        }
        if let Some(t) = body.title.clone() {
            obj.insert("title".into(), json!(t));
        }
        if !user_input.is_empty() {
            obj.insert("userInput".into(), json!(user_input));
        }
        if let Some(labels) = body.labels.clone() {
            obj.insert("labels".into(), labels);
        }
        if let Some(ctx) = context.clone() {
            obj.insert("context".into(), ctx);
        }
        if let Some(prev) = body.previous_run_id.clone() {
            obj.insert("previousRunId".into(), json!(prev));
        }
        if body.prefer_heuristic.unwrap_or(false) {
            obj.insert("preferHeuristic".into(), json!(true));
        }
        obj.insert("feature".into(), json!("book_travel"));
    }

    let meta = json!({
        "feature": "book_travel",
        "mode": step,
        "step": step,
        "title": body.title,
    });

    let job = match state.jobs.create(
        "book_travel",
        &session.user_id,
        &session.workspace_id,
        payload,
        body.model.or_else(|| Some(state.llm_model.clone())),
        Some(meta),
    ) {
        Ok(j) => j,
        Err(e) => return map_core_err(e),
    };

    spawn_book_travel_worker(state.clone(), job.run_id.clone());

    let pipeline = step == "pipeline";
    (
        StatusCode::CREATED,
        Json(json!({
            "id": job.run_id,
            "runId": job.run_id,
            "kind": job.kind,
            "step": step,
            "mode": step,
            "pipeline": pipeline,
            "status": normalize_job_status(&job.status),
            "stream": format!("/api/v1/book-travel/stream?id={}", job.run_id),
            "jobsStream": format!("/api/v1/jobs/{}/stream", job.run_id),
            "run": format!("/api/v1/book-travel/runs/{}", job.run_id),
            "progress": job.progress,
            "progressMessage": job.progress_message,
            "payload": job.payload,
            "retryable": true,
        })),
    )
        .into_response()
}


/// POST /api/v1/book-travel/pipeline — alias for start with mode=pipeline.
pub async fn start_pipeline(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(mut body): Json<BookTravelStartBody>,
) -> axum::response::Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    body.mode = Some("pipeline".into());
    body.step = Some("pipeline".into());
    start_inner(state, session, body, Some("pipeline".into()))
}

/// POST /api/v1/book-travel/start
pub async fn start(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<BookTravelStartBody>,
) -> axum::response::Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    start_inner(state, session, body, None)
}

/// POST /api/v1/book-travel/{step}
pub async fn start_step(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(step): Path<String>,
    Json(body): Json<BookTravelStartBody>,
) -> axum::response::Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let Some(norm) = normalize_step(&step) else {
        return bad_request("BT_BAD_REQUEST", format!("unknown step '{step}'; expected: {}", STEPS.join(", ")));
    };
    start_inner(state, session, body, Some(norm))
}


/// GET /api/v1/book-travel/runs — workspace-scoped book_travel jobs (newest first).
pub async fn list_runs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<BookTravelRunsQuery>,
) -> axum::response::Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let limit = q.limit.unwrap_or(30).clamp(1, 100) as usize;
    let filter = kaleido_core::JobListFilter {
        kind: Some("book_travel".into()),
        status: q.status.clone(),
        user_id: None,
        workspace_id: Some(session.workspace_id.clone()),
        limit,
    };
    match state.jobs.list(filter) {
        Ok(jobs) => {
            let items: Vec<Value> = jobs.iter().map(|j| j.to_api_json()).collect();
            Json(json!({
                "ok": true,
                "items": items,
                "count": items.len(),
                "workspaceId": session.workspace_id,
            }))
            .into_response()
        }
        Err(e) => map_core_err(e),
    }
}

/// GET /api/v1/book-travel/runs/{id}
pub async fn get_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> axum::response::Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let run_id = id.trim();
    if run_id.is_empty() {
        return bad_request("BT_ID", "需要 run id");
    }
    match state.jobs.get(run_id) {
        None => return err_with_code(
            StatusCode::NOT_FOUND,
            "BT_NOT_FOUND", "任务不存在",
            serde_json::json!({"runId": run_id})),
        Some(j) if j.kind != "book_travel" => return bad_request("BT_KIND", "不是 book_travel 任务"),
        Some(j) if j.workspace_id != session.workspace_id && j.user_id != session.user_id => return forbidden("BT_FORBIDDEN", "任务不属于当前工作区"),
        Some(j) => Json(j.to_api_json()).into_response(),
    }
}

pub async fn stop(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<BookTravelStopBody>,
) -> axum::response::Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };

    let run_id = body.id.trim();
    if run_id.is_empty() {
        return bad_request("BT_ID", "需要 id/runId");
    }

    match state.jobs.get(run_id) {
        Some(j) if j.workspace_id != session.workspace_id && j.user_id != session.user_id => {
            return forbidden("BT_FORBIDDEN_SCOPE", "job not in your workspace");
        }
        Some(j) if j.kind != "book_travel" => {
            return bad_request("BT_BAD_REQUEST", "not a book_travel job");
        }
        None => {
            return not_found("BT_NOT_FOUND", "任务不存在");
        }
        _ => {}
    }

    match state.jobs.cancel(run_id) {
        Ok(j) => Json(j.to_api_json()).into_response(),
        Err(e) => map_core_err(e),
    }
}

pub async fn stream(
    State(state): State<AppState>,
    Query(q): Query<BookTravelStreamQuery>,
) -> axum::response::Response {
    let run_id = q.id.trim().to_string();
    if run_id.is_empty() {
        return bad_request("BT_MISSING_FIELD", "id query param is required");
    }
    match state.jobs.get(&run_id) {
        None => return not_found("BT_NOT_FOUND", "任务不存在"),
        Some(j) if j.kind != "book_travel" => return bad_request("BT_BAD_REQUEST", "not a book_travel job"),
        Some(_) => stream_job_sse(state, run_id).await,
    }
}

// ---------------------------------------------------------------------------
// Worker
// ---------------------------------------------------------------------------

fn spawn_book_travel_worker(state: AppState, run_id: String) {
    tokio::spawn(async move {
        let jobs = state.jobs.clone();

        for _ in 0..600 {
            match jobs.get(&run_id) {
                Some(j) if is_terminal_job_status(&j.status) => return,
                Some(j) if normalize_job_status(&j.status) == "running" => break,
                Some(_) => tokio::time::sleep(StdDuration::from_millis(50)).await,
                None => return,
            }
        }
        if jobs
            .get(&run_id)
            .map(|j| normalize_job_status(&j.status) != "running")
            .unwrap_or(true)
        {
            return;
        }

        let payload = jobs
            .get(&run_id)
            .and_then(|j| j.payload.clone())
            .unwrap_or_else(|| json!({}));
        let step = payload
            .get("step")
            .or_else(|| payload.get("mode"))
            .and_then(|v| v.as_str())
            .unwrap_or("assemble")
            .to_string();
        let premise = payload
            .get("text")
            .or_else(|| payload.get("premise"))
            .and_then(|v| v.as_str())
            .unwrap_or("A journey through an unwritten book.")
            .to_string();
        let title = payload
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("Untitled Book Travel")
            .to_string();
        let user_input = payload
            .get("userInput")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let context = payload.get("context").cloned().unwrap_or_else(|| json!({}));
        let prefer_heuristic = payload
            .get("preferHeuristic")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let labels = payload.get("labels").cloned().unwrap_or_else(|| {
            let h = heuristic_classify(&premise, Some(&title), None);
            serde_json::to_value(h).unwrap_or_else(|_| json!({}))
        });

        if cancelled(&jobs, &run_id) {
            return;
        }

        // W2: full journey in one job
        if step == "pipeline" {
            let result = run_book_travel_pipeline(
                &state,
                &jobs,
                &run_id,
                &title,
                &premise,
                &user_input,
                &labels,
                &context,
                prefer_heuristic,
            )
            .await;
            if cancelled(&jobs, &run_id) {
                return;
            }
            if let Some(mut result) = result {
                persist_pipeline_to_works(&state, &run_id, &mut result);
                complete_if_not_cancelled(&jobs, &run_id, "succeeded", Some(result), None);
            } else if !cancelled(&jobs, &run_id) {
                complete_if_not_cancelled(
                    &jobs,
                    &run_id,
                    "failed",
                    Some(json!({
                        "kind": "book_travel",
                        "step": "pipeline",
                        "mode": "pipeline",
                        "pipeline": true,
                        "ok": false,
                        "error": "流水线中止，无结果",
                        "code": "BT_PIPELINE_EMPTY",
                        "retryable": true,
                    })),
                    Some("book travel pipeline failed".into()),
                );
            }
            return;
        }

        let _ = jobs.push_event(
            &run_id,
            JobEvent::progress(format!("{step}: generating"), 0.2),
            Some(0.2),
            Some(format!("{step}:start")),
        );
        tokio::time::sleep(StdDuration::from_millis(30)).await;
        if cancelled(&jobs, &run_id) {
            return;
        }

        // Stream-parity: try LLM first; soft-fail to templates.
        let llm_try = if prefer_heuristic {
            None
        } else {
            try_book_travel_llm_stream(
                &state,
                &jobs,
                &run_id,
                &step,
                &title,
                &premise,
                &user_input,
                &labels,
                &context,
            )
            .await
        };

        let (stage_data, result) = if let Some((sd, res)) = llm_try {
            (sd, res)
        } else {
            match step.as_str() {
                "plan_scene" => {
                    let plan =
                        template_scene_plan(&title, &premise, &user_input, &labels, &context);
                    (
                        json!({"stage":"plan_scene","scenePlan": plan}),
                        json!({
                            "kind":"book_travel",
                            "step":"plan_scene",
                            "mode":"plan_scene",
                            "title": title,
                            "scenePlan": plan,
                            "labels": labels,
                            "ok": true,
                            "mvp": true,
                            "fallback": true,
                            "generationMode": "heuristic",
                        }),
                    )
                }
                "write_change_scene" => {
                    let out = template_writer_output(
                        &title,
                        &premise,
                        &user_input,
                        "change-scene",
                        &context,
                    );
                    (
                        json!({"stage":"write_change_scene","writerOutput": out}),
                        json!({
                            "kind":"book_travel",
                            "step":"write_change_scene",
                            "mode":"write_change_scene",
                            "title": title,
                            "writerOutput": out,
                            "flow":"change-scene",
                            "ok": true,
                            "mvp": true,
                            "fallback": true,
                            "generationMode": "heuristic",
                        }),
                    )
                }
                "write_insert_beat" => {
                    let out = template_writer_output(
                        &title,
                        &premise,
                        &user_input,
                        "insert-beat",
                        &context,
                    );
                    (
                        json!({"stage":"write_insert_beat","writerOutput": out}),
                        json!({
                            "kind":"book_travel",
                            "step":"write_insert_beat",
                            "mode":"write_insert_beat",
                            "title": title,
                            "writerOutput": out,
                            "flow":"insert-beat",
                            "ok": true,
                            "mvp": true,
                            "fallback": true,
                            "generationMode": "heuristic",
                        }),
                    )
                }
                "judge_ending" => {
                    let ending = template_ending(&title, &premise, &labels, &context);
                    (
                        json!({"stage":"judge_ending","ending": ending}),
                        json!({
                            "kind":"book_travel",
                            "step":"judge_ending",
                            "mode":"judge_ending",
                            "title": title,
                            "ending": ending,
                            "labels": labels,
                            "ok": true,
                            "mvp": true,
                            "fallback": true,
                            "generationMode": "heuristic",
                        }),
                    )
                }
                "summarize_memory" => {
                    let memory = template_memory(&title, &premise, &context);
                    (
                        json!({"stage":"summarize_memory","memory": memory}),
                        json!({
                            "kind":"book_travel",
                            "step":"summarize_memory",
                            "mode":"summarize_memory",
                            "title": title,
                            "memory": memory,
                            "ok": true,
                            "mvp": true,
                            "fallback": true,
                            "generationMode": "heuristic",
                        }),
                    )
                }
                // assemble (default)
                _ => {
                    let plan_text = template_assemble_plan(&title, &premise, &labels);
                    (
                        json!({
                            "stage":"assemble",
                            "title": title,
                            "plan": plan_text,
                            "labels": labels,
                        }),
                        json!({
                            "kind":"book_travel",
                            "step":"assemble",
                            "mode":"assemble",
                            "title": title,
                            "plan": plan_text,
                            "labels": labels,
                            "ok": true,
                            "mvp": true,
                            "fallback": true,
                            "generationMode": "heuristic",
                        }),
                    )
                }
            }
        };

        let _ = jobs.push_event(
            &run_id,
            JobEvent::event(format!("{step} complete"), Some(stage_data)),
            Some(0.85),
            Some(format!("{step}:done")),
        );
        tokio::time::sleep(StdDuration::from_millis(20)).await;
        if cancelled(&jobs, &run_id) {
            return;
        }
        let mut result = result;
        if let Some(path) = persist_step_snapshot_to_works(&state, &run_id, &step, &title, &result) {
            if let Some(obj) = result.as_object_mut() {
                obj.insert("workPath".into(), json!(path));
                obj.insert("persisted".into(), json!(true));
            }
        }
        complete_if_not_cancelled(&jobs, &run_id, "succeeded", Some(result), None);
    });
}

async fn try_book_travel_llm_stream(
    state: &AppState,
    jobs: &kaleido_core::JobStore,
    run_id: &str,
    step: &str,
    title: &str,
    premise: &str,
    user_input: &str,
    labels: &Value,
    context: &Value,
) -> Option<(Value, Value)> {
    let llm = state.app_state.resolve_llm(
        state.llm_base.as_deref(),
        state.llm_key.as_deref(),
        &state.llm_model,
    );
    let prov_kind2 = crate::llm_stream::runtime_provider_kind(&llm, &state.provider_kind);
    if llm.base_url.trim().is_empty() || llm.api_key.trim().is_empty() {
        return None;
    }
    let model = if llm.model.is_empty() {
        state.llm_model.clone()
    } else {
        llm.model.clone()
    };

    let (system, user, max_tokens, timeout_secs, temperature) =
        book_travel_prompts(step, title, premise, user_input, labels, context);

    let mut chars: usize = 0;
    let jobs_c = jobs.clone();
    let run_c = run_id.to_string();
    let step_c = step.to_string();
    let full = match crate::llm_stream::stream_chat_completions_dispatch(
        &llm.base_url,
        &llm.api_key,
        &model,
        &prov_kind2,
        &system,
        &user,
        temperature,
        max_tokens,
        timeout_secs,
        |chunk| {
            if cancelled(&jobs_c, &run_c) {
                return false;
            }
            chars = chars.saturating_add(chunk.chars().count());
            let p = (0.25 + (chars as f64 / 5000.0).min(0.5)).min(0.8);
            let _ = jobs_c.push_event(
                &run_c,
                JobEvent {
                    event_type: "delta".into(),
                    ts: chrono::Utc::now(),
                    message: None,
                    progress: Some(p),
                    code: None,
                    data: Some(json!({"delta": chunk, "step": step_c})),
                },
                Some(p),
                Some(format!("{step_c}:delta")),
            );
            true
        },
    )
    .await
    {
        Ok(s) => s,
        Err(e) => {
            if e != "cancelled" {
                let _ = jobs.push_event(
                    run_id,
                    JobEvent::event(
                        format!("llm soft-fail: {e}"),
                        Some(json!({"step": step, "error": e})),
                    ),
                    None,
                    Some(format!("{step}:llm_fail")),
                );
            }
            return None;
        }
    };

    if cancelled(jobs, run_id) {
        return None;
    }

    normalize_book_travel_llm(step, title, labels, &full)
}

fn book_travel_prompts(
    step: &str,
    title: &str,
    premise: &str,
    user_input: &str,
    labels: &Value,
    context: &Value,
) -> (String, String, u32, u64, f64) {
    let premise_l = crate::llm_stream::limit_text(premise, 4000);
    let user_l = crate::llm_stream::limit_text(user_input, 1500);
    let labels_s = serde_json::to_string(labels).unwrap_or_else(|_| "{}".into());
    let context_s = crate::llm_stream::limit_text(
        &serde_json::to_string(context).unwrap_or_else(|_| "{}".into()),
        2000,
    );
    match step {
        "plan_scene" => {
            let system = "你是穿书场景规划师（ScenePlanner）。根据前提、标签与上下文规划下一场景。务必返回严格纯 JSON，不要 Markdown。".to_string();
            let user = format!(
                "作品：《{title}》\n前提：{premise_l}\n用户意图：{user_l}\n标签：{labels_s}\n上下文：{context_s}\n\
返回 JSON：\n\
{{\"id\":\"uuid-or-string\",\"title\":\"场景标题\",\"summary\":\"...\",\"currentSituation\":\"...\",\"time\":\"...\",\"location\":\"...\",\"activeCharacters\":[\"...\"],\"stateChanges\":{{\"tension\":\"...\",\"information\":\"...\"}},\"divergence\":\"...\",\"storyProgress\":0-100,\"endingStatus\":null,\"sceneGoals\":[\"...\"],\"entryBeatGuidance\":\"...\",\"writerInstructions\":\"...\"}}\n仅返回纯 JSON。"
            );
            (system, user, 2048, 180, 0.6)
        }
        "write_change_scene" => {
            let system = "你是穿书场景写手（SceneWriter）。按用户意图改写当前场景，保持角色声线，动作优先，避免总结腔。返回严格纯 JSON。".to_string();
            let user = format!(
                "作品：《{title}》\n流程：change-scene\n前提：{premise_l}\n用户意图：{user_l}\n上下文：{context_s}\n\
返回 JSON：\n\
{{\"beat\":{{\"id\":\"...\",\"content\":\"场景正文（中文，多段落）\"}},\"stableMemoryPatch\":{{\"note\":\"...\"}},\"volatileMemoryPatch\":{{\"lastBeat\":\"change-scene\",\"directive\":\"...\"}}}}\n仅返回纯 JSON。"
            );
            (system, user, 4096, 240, 0.8)
        }
        "write_insert_beat" => {
            let system = "你是穿书场景写手（SceneWriter）。在当前场景缝隙插入一拍，信息量增加但不跳过冲突。返回严格纯 JSON。".to_string();
            let user = format!(
                "作品：《{title}》\n流程：insert-beat\n前提：{premise_l}\n用户意图：{user_l}\n上下文：{context_s}\n\
返回 JSON：\n\
{{\"beat\":{{\"id\":\"...\",\"content\":\"插入拍正文（中文）\"}},\"stableMemoryPatch\":{{\"note\":\"...\"}},\"volatileMemoryPatch\":{{\"lastBeat\":\"insert-beat\",\"directive\":\"...\"}}}}\n仅返回纯 JSON。"
            );
            (system, user, 3072, 220, 0.8)
        }
        "judge_ending" => {
            let system = "你是穿书结局评判官（EndingJudge）。根据标签与前提给出结局判定。返回严格纯 JSON。".to_string();
            let user = format!(
                "作品：《{title}》\n前提：{premise_l}\n标签：{labels_s}\n上下文：{context_s}\n\
返回 JSON：\n\
{{\"finalEnding\":\"...\",\"userKeyChoices\":[\"...\"],\"originalOutlineComparison\":\"...\",\"characterOutcomes\":[\"...\"],\"worldlineName\":\"...\",\"divergenceScore\":0-100}}\n仅返回纯 JSON。"
            );
            (system, user, 1536, 150, 0.5)
        }
        "summarize_memory" => {
            let system = "你是穿书记忆管家（MemoryKeeper）。压缩关键记忆与未决冲突。返回严格纯 JSON。".to_string();
            let user = format!(
                "作品：《{title}》\n前提：{premise_l}\n上下文：{context_s}\n\
返回 JSON：\n\
{{\"summary\":\"...\",\"keyChoices\":[\"...\"],\"unresolvedConflicts\":[\"...\"],\"divergenceFromOutline\":\"...\"}}\n仅返回纯 JSON。"
            );
            (system, user, 1536, 150, 0.4)
        }
        // assemble
        _ => {
            let system = "你是穿书素材装配师（MaterialAssembler）。输出可执行的 Book Travel 计划（Markdown 正文即可，也可用 JSON {\"plan\":\"...\"}）。".to_string();
            let user = format!(
                "作品：《{title}》\n前提：{premise_l}\n标签：{labels_s}\n\
请输出旅行计划，包含：前提摘要、标签、5 个 Travel Beats（Entrance/First turn/Midpoint/Crisis/Exit）。\n\
优先返回 JSON：{{\"plan\":\"markdown 正文\"}}；若无法 JSON，也可直接输出 Markdown。"
            );
            (system, user, 2048, 150, 0.5)
        }
    }
}

fn normalize_book_travel_llm(
    step: &str,
    title: &str,
    labels: &Value,
    full: &str,
) -> Option<(Value, Value)> {
    match step {
        "plan_scene" => {
            let plan = crate::llm_stream::extract_json_value(full)?;
            let plan = if plan.get("summary").is_some() || plan.get("sceneGoals").is_some() {
                plan
            } else {
                plan.get("scenePlan").cloned()?
            };
            Some((
                json!({"stage":"plan_scene","scenePlan": plan, "generationMode":"llm"}),
                json!({
                    "kind":"book_travel",
                    "step":"plan_scene",
                    "mode":"plan_scene",
                    "title": title,
                    "scenePlan": plan,
                    "labels": labels,
                    "ok": true,
                    "fallback": false,
                    "generationMode": "llm",
                }),
            ))
        }
        "write_change_scene" | "write_insert_beat" => {
            let flow = if step == "write_insert_beat" {
                "insert-beat"
            } else {
                "change-scene"
            };
            let out = if let Some(v) = crate::llm_stream::extract_json_value(full) {
                if v.get("beat").is_some() {
                    v
                } else if let Some(w) = v.get("writerOutput").cloned() {
                    w
                } else {
                    // wrap free text
                    json!({
                        "beat": {"id": Uuid::new_v4().to_string(), "content": full},
                        "stableMemoryPatch": {"note": format!("flow={flow} llm freeform")},
                        "volatileMemoryPatch": {"lastBeat": flow, "directive": ""}
                    })
                }
            } else {
                json!({
                    "beat": {"id": Uuid::new_v4().to_string(), "content": full},
                    "stableMemoryPatch": {"note": format!("flow={flow} llm freeform")},
                    "volatileMemoryPatch": {"lastBeat": flow, "directive": ""}
                })
            };
            Some((
                json!({"stage": step, "writerOutput": out, "generationMode":"llm"}),
                json!({
                    "kind":"book_travel",
                    "step": step,
                    "mode": step,
                    "title": title,
                    "writerOutput": out,
                    "flow": flow,
                    "ok": true,
                    "fallback": false,
                    "generationMode": "llm",
                }),
            ))
        }
        "judge_ending" => {
            let ending = crate::llm_stream::extract_json_value(full)?;
            let ending = if ending.get("finalEnding").is_some() {
                ending
            } else {
                ending.get("ending").cloned().unwrap_or(ending)
            };
            Some((
                json!({"stage":"judge_ending","ending": ending, "generationMode":"llm"}),
                json!({
                    "kind":"book_travel",
                    "step":"judge_ending",
                    "mode":"judge_ending",
                    "title": title,
                    "ending": ending,
                    "labels": labels,
                    "ok": true,
                    "fallback": false,
                    "generationMode": "llm",
                }),
            ))
        }
        "summarize_memory" => {
            let memory = crate::llm_stream::extract_json_value(full)?;
            let memory = if memory.get("summary").is_some() {
                memory
            } else {
                memory.get("memory").cloned().unwrap_or(memory)
            };
            Some((
                json!({"stage":"summarize_memory","memory": memory, "generationMode":"llm"}),
                json!({
                    "kind":"book_travel",
                    "step":"summarize_memory",
                    "mode":"summarize_memory",
                    "title": title,
                    "memory": memory,
                    "ok": true,
                    "fallback": false,
                    "generationMode": "llm",
                }),
            ))
        }
        _ => {
            // assemble: plan string
            let plan_text = if let Some(v) = crate::llm_stream::extract_json_value(full) {
                v.get("plan")
                    .and_then(|p| p.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| full.trim().to_string())
            } else {
                full.trim().to_string()
            };
            if plan_text.is_empty() {
                return None;
            }
            Some((
                json!({
                    "stage":"assemble",
                    "title": title,
                    "plan": plan_text,
                    "labels": labels,
                    "generationMode":"llm",
                }),
                json!({
                    "kind":"book_travel",
                    "step":"assemble",
                    "mode":"assemble",
                    "title": title,
                    "plan": plan_text,
                    "labels": labels,
                    "ok": true,
                    "fallback": false,
                    "generationMode": "llm",
                }),
            ))
        }
    }
}


async fn run_one_travel_step(
    state: &AppState,
    jobs: &kaleido_core::JobStore,
    run_id: &str,
    step: &str,
    title: &str,
    premise: &str,
    user_input: &str,
    labels: &Value,
    context: &Value,
    progress_lo: f64,
    progress_hi: f64,
    prefer_heuristic: bool,
) -> Option<Value> {
    if cancelled(jobs, run_id) {
        return None;
    }
    let _ = jobs.push_event(
        run_id,
        JobEvent::progress(format!("pipeline:{step}"), progress_lo),
        Some(progress_lo),
        Some(format!("pipeline:{step}:start")),
    );

    let llm_try = if prefer_heuristic {
        None
    } else {
        try_book_travel_llm_stream(
            state, jobs, run_id, step, title, premise, user_input, labels, context,
        )
        .await
    };

    if cancelled(jobs, run_id) {
        return None;
    }

    let (stage_data, result) = if let Some((sd, res)) = llm_try {
        (sd, res)
    } else {
        match step {
            "plan_scene" => {
                let plan = template_scene_plan(title, premise, user_input, labels, context);
                (
                    json!({"stage":"plan_scene","scenePlan": plan}),
                    json!({
                        "kind":"book_travel",
                        "step":"plan_scene",
                        "mode":"plan_scene",
                        "title": title,
                        "scenePlan": plan,
                        "labels": labels,
                        "ok": true,
                        "mvp": true,
                        "fallback": true,
                        "generationMode": "heuristic",
                    }),
                )
            }
            "write_change_scene" => {
                let out = template_writer_output(title, premise, user_input, "change-scene", context);
                (
                    json!({"stage":"write_change_scene","writerOutput": out}),
                    json!({
                        "kind":"book_travel",
                        "step":"write_change_scene",
                        "mode":"write_change_scene",
                        "title": title,
                        "writerOutput": out,
                        "flow":"change-scene",
                        "ok": true,
                        "mvp": true,
                        "fallback": true,
                        "generationMode": "heuristic",
                    }),
                )
            }
            "write_insert_beat" => {
                let out = template_writer_output(title, premise, user_input, "insert-beat", context);
                (
                    json!({"stage":"write_insert_beat","writerOutput": out}),
                    json!({
                        "kind":"book_travel",
                        "step":"write_insert_beat",
                        "mode":"write_insert_beat",
                        "title": title,
                        "writerOutput": out,
                        "flow":"insert-beat",
                        "ok": true,
                        "mvp": true,
                        "fallback": true,
                        "generationMode": "heuristic",
                    }),
                )
            }
            "judge_ending" => {
                let ending = template_ending(title, premise, labels, context);
                (
                    json!({"stage":"judge_ending","ending": ending}),
                    json!({
                        "kind":"book_travel",
                        "step":"judge_ending",
                        "mode":"judge_ending",
                        "title": title,
                        "ending": ending,
                        "labels": labels,
                        "ok": true,
                        "mvp": true,
                        "fallback": true,
                        "generationMode": "heuristic",
                    }),
                )
            }
            "summarize_memory" => {
                let memory = template_memory(title, premise, context);
                (
                    json!({"stage":"summarize_memory","memory": memory}),
                    json!({
                        "kind":"book_travel",
                        "step":"summarize_memory",
                        "mode":"summarize_memory",
                        "title": title,
                        "memory": memory,
                        "ok": true,
                        "mvp": true,
                        "fallback": true,
                        "generationMode": "heuristic",
                    }),
                )
            }
            _ => {
                let plan_text = template_assemble_plan(title, premise, labels);
                (
                    json!({
                        "stage":"assemble",
                        "title": title,
                        "plan": plan_text,
                        "labels": labels,
                    }),
                    json!({
                        "kind":"book_travel",
                        "step":"assemble",
                        "mode":"assemble",
                        "title": title,
                        "plan": plan_text,
                        "labels": labels,
                        "ok": true,
                        "mvp": true,
                        "fallback": true,
                        "generationMode": "heuristic",
                    }),
                )
            }
        }
    };

    let _ = jobs.push_event(
        run_id,
        JobEvent::event(
            format!("{step} complete"),
            Some(json!({
                "stage": step,
                "pipeline": true,
                "result": result,
                "stageData": stage_data,
            })),
        ),
        Some(progress_hi),
        Some(format!("pipeline:{step}:done")),
    );
    Some(result)
}

async fn run_book_travel_pipeline(
    state: &AppState,
    jobs: &kaleido_core::JobStore,
    run_id: &str,
    title: &str,
    premise: &str,
    user_input: &str,
    labels: &Value,
    initial_context: &Value,
    prefer_heuristic: bool,
) -> Option<Value> {
    let n = PIPELINE_STEPS.len() as f64;
    let mut stages: Vec<Value> = Vec::new();
    let mut context = if initial_context.is_null() {
        json!({})
    } else {
        initial_context.clone()
    };
    let mut any_llm = false;

    for (i, step) in PIPELINE_STEPS.iter().enumerate() {
        if cancelled(jobs, run_id) {
            return None;
        }
        let lo = 0.05 + (i as f64 / n) * 0.85;
        let hi = 0.05 + ((i as f64 + 1.0) / n) * 0.85;
        let step_result = run_one_travel_step(
            state,
            jobs,
            run_id,
            step,
            title,
            premise,
            user_input,
            labels,
            &context,
            lo,
            hi.min(0.92),
            prefer_heuristic,
        )
        .await?;
        if step_result
            .get("generationMode")
            .and_then(|v| v.as_str())
            == Some("llm")
        {
            any_llm = true;
        }
        // Chain: next step sees accumulated prior results
        context = json!({
            "pipeline": true,
            "previousStep": step,
            "previousResult": step_result,
            "stagesSoFar": stages,
        });
        stages.push(json!({
            "step": step,
            "ok": step_result.get("ok").and_then(|v| v.as_bool()).unwrap_or(true),
            "generationMode": step_result.get("generationMode"),
            "result": step_result,
        }));
        let _ = jobs.push_event(
            run_id,
            JobEvent::progress(
                format!("pipeline progress {}/{}", i + 1, PIPELINE_STEPS.len()),
                hi.min(0.92),
            ),
            Some(hi.min(0.92)),
            Some(format!("pipeline:progress:{}", i + 1)),
        );
    }

    if cancelled(jobs, run_id) {
        return None;
    }

    Some(json!({
        "kind": "book_travel",
        "step": "pipeline",
        "mode": "pipeline",
        "pipeline": true,
        "title": title,
        "labels": labels,
        "stages": stages,
        "stageCount": stages.len(),
        "ok": true,
        "mvp": true,
        "fallback": !any_llm,
        "generationMode": if any_llm { "pipeline+llm" } else { "pipeline+heuristic" },
        "progress": 1.0,
    }))
}

fn slugify_title(title: &str) -> String {
    let s: String = title
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else if c.is_whitespace() || c == '-' || c == '_' {
                '-'
            } else {
                '-'
            }
        })
        .collect();
    let s = s.trim_matches('-').to_string();
    if s.is_empty() {
        "book-travel".into()
    } else {
        s.chars().take(48).collect()
    }
}


fn complete_if_not_cancelled(
    jobs: &kaleido_core::JobStore,
    run_id: &str,
    status: &str,
    result: Option<Value>,
    error: Option<String>,
) {
    if cancelled(jobs, run_id) && status != "cancelled" {
        let _ = jobs.complete(run_id, "cancelled", result, error.or_else(|| Some("已取消".into())));
        return;
    }
    let _ = jobs.complete(run_id, status, result, error);
}

/// Persist pipeline artifact under works/{ws}/book-travel/{slug}-{runShort}/
fn persist_pipeline_to_works(state: &AppState, run_id: &str, result: &mut Value) {
    let Some(job) = state.jobs.get(run_id) else {
        return;
    };
    let title = result
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("Untitled Book Travel");
    let slug = slugify_title(title);
    let short = run_id.chars().take(8).collect::<String>();
    let dir = format!("book-travel/{slug}-{short}");
    let md_path = format!("{dir}/journey.md");
    let json_path = format!("{dir}/result.json");

    let mut md = String::new();
    md.push_str(&format!("# {title}\n\n"));
    md.push_str(&format!("runId: `{run_id}`\n\n"));
    if let Some(stages) = result.get("stages").and_then(|v| v.as_array()) {
        for (i, st) in stages.iter().enumerate() {
            let step = st.get("step").and_then(|v| v.as_str()).unwrap_or("?");
            md.push_str(&format!("## {}. {step}\n\n", i + 1));
            if let Some(res) = st.get("result") {
                if let Some(plan) = res.get("plan").and_then(|v| v.as_str()) {
                    md.push_str(plan);
                    md.push_str("\n\n");
                }
                if let Some(sp) = res.get("scenePlan") {
                    md.push_str("```json\n");
                    md.push_str(&serde_json::to_string_pretty(sp).unwrap_or_default());
                    md.push_str("\n```\n\n");
                }
                if let Some(w) = res.get("writerOutput") {
                    if let Some(content) = w
                        .pointer("/beat/content")
                        .and_then(|v| v.as_str())
                    {
                        md.push_str(content);
                        md.push_str("\n\n");
                    } else {
                        md.push_str("```json\n");
                        md.push_str(&serde_json::to_string_pretty(w).unwrap_or_default());
                        md.push_str("\n```\n\n");
                    }
                }
                if let Some(e) = res.get("ending") {
                    md.push_str("```json\n");
                    md.push_str(&serde_json::to_string_pretty(e).unwrap_or_default());
                    md.push_str("\n```\n\n");
                }
                if let Some(m) = res.get("memory") {
                    md.push_str("```json\n");
                    md.push_str(&serde_json::to_string_pretty(m).unwrap_or_default());
                    md.push_str("\n```\n\n");
                }
            }
        }
    }

    let ws = &job.workspace_id;
    let json_body = serde_json::to_string_pretty(result).unwrap_or_else(|_| "{}".into());
    // WorksFs requires parents to exist (mkdir first).
    let _ = state.works.mkdir(ws, "book-travel");
    let _ = state.works.mkdir(ws, &dir);
    let _ = state.works.mkdir(ws, "book-travel/steps");
    let wrote_md = match state.works.write_text(ws, &md_path, &md) {
        Ok(_) => true,
        Err(e) => {
            tracing::warn!(target: "book_travel", error=%e, path=%md_path, "persist journey.md failed");
            false
        }
    };
    let wrote_json = match state.works.write_text(ws, &json_path, &json_body) {
        Ok(_) => true,
        Err(e) => {
            tracing::warn!(target: "book_travel", error=%e, path=%json_path, "persist result.json failed");
            false
        }
    };

    if let Some(obj) = result.as_object_mut() {
        if wrote_md {
            obj.insert("workPath".into(), json!(md_path));
        }
        if wrote_json {
            obj.insert("resultPath".into(), json!(json_path));
        }
        obj.insert(
            "worksDir".into(),
            json!(dir),
        );
        obj.insert("persisted".into(), json!(wrote_md || wrote_json));
    }

    let _ = state.jobs.push_event(
        run_id,
        JobEvent::event(
            "pipeline persisted to works",
            Some(json!({
                "workPath": md_path,
                "resultPath": json_path,
                "worksDir": dir,
            })),
        ),
        Some(0.96),
        Some("pipeline:persist".into()),
    );
}

fn persist_step_snapshot_to_works(
    state: &AppState,
    run_id: &str,
    step: &str,
    title: &str,
    result: &Value,
) -> Option<String> {
    let job = state.jobs.get(run_id)?;
    let slug = slugify_title(title);
    let short = run_id.chars().take(8).collect::<String>();
    let path = format!("book-travel/steps/{slug}-{short}-{step}.json");
    let body = serde_json::to_string_pretty(result).ok()?;
    let ws = &job.workspace_id;
    let _ = state.works.mkdir(ws, "book-travel");
    let _ = state.works.mkdir(ws, "book-travel/steps");
    state.works.write_text(ws, &path, &body).ok()?;
    Some(path)
}


fn cancelled(jobs: &kaleido_core::JobStore, run_id: &str) -> bool {
    jobs.get(run_id)
        .map(|j| is_terminal_job_status(&j.status))
        .unwrap_or(true)
}

// ---------------------------------------------------------------------------
// Templates (upstream-shaped)
// ---------------------------------------------------------------------------

fn short(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn template_assemble_plan(title: &str, premise: &str, labels: &Value) -> String {
    let genre = labels
        .get("genre")
        .and_then(|v| v.as_str())
        .unwrap_or("general_fiction");
    let tone = labels
        .get("tone")
        .and_then(|v| v.as_str())
        .unwrap_or("neutral");
    format!(
        "# {title} — Book Travel Plan\n\n\
         ## Premise\n{premise}\n\n\
         ## Labels\n\
         - Genre: {genre}\n\
         - Tone: {tone}\n\n\
         ## Travel Beats\n\
         1. Entrance — establish world and protagonist stake\n\
         2. First turn — introduce core conflict\n\
         3. Midpoint mirror — reframe goal\n\
         4. Crisis — force a choice\n\
         5. Exit / coda — land theme\n"
    )
}

fn template_scene_plan(
    title: &str,
    premise: &str,
    user_input: &str,
    labels: &Value,
    context: &Value,
) -> Value {
    let id = Uuid::new_v4().to_string();
    let hint = if user_input.trim().is_empty() {
        short(premise, 80)
    } else {
        short(user_input, 80)
    };
    let genre = labels
        .get("genre")
        .and_then(|v| v.as_str())
        .unwrap_or("general_fiction");
    let prior = context
        .get("scenePlan")
        .or_else(|| context.get("plan"))
        .cloned();
    let progress = prior
        .as_ref()
        .and_then(|p| p.get("storyProgress"))
        .and_then(|v| v.as_u64())
        .unwrap_or(10)
        .saturating_add(15)
        .min(95) as u32;

    json!({
        "id": id,
        "title": format!("{title} · 场景"),
        "summary": format!("围绕「{hint}」推进的下一场景（{genre}）"),
        "currentSituation": format!("承接前提：{}", short(premise, 100)),
        "time": "故事时间线 · 下一拍",
        "location": "与世界书一致的关键场景",
        "activeCharacters": ["主角", "关键配角"],
        "stateChanges": {
            "tension": "上升",
            "information": "部分揭示"
        },
        "divergence": "相对原大纲可偏移，但保留主线张力",
        "storyProgress": progress,
        "endingStatus": null,
        "sceneGoals": [
            "推进核心矛盾",
            "给出可操作的选择点",
            "留下下一拍钩子"
        ],
        "entryBeatGuidance": format!("开场切入：{hint}"),
        "writerInstructions": "保持角色声线一致；动作优先；避免总结腔。"
    })
}

fn template_writer_output(
    title: &str,
    premise: &str,
    user_input: &str,
    flow: &str,
    _context: &Value,
) -> Value {
    let beat_id = Uuid::new_v4().to_string();
    let directive = if user_input.trim().is_empty() {
        short(premise, 60)
    } else {
        short(user_input, 60)
    };
    let content = match flow {
        "insert-beat" => format!(
            "【插入 Beat · {title}】\n\
             在当前场景缝隙中插入一拍：{directive}\n\
             角色做出细微却关键的反应，信息量增加，但不跳过冲突。\n\
             （模板输出，可后续接 LLM 深化）"
        ),
        _ => format!(
            "【改写场景 · {title}】\n\
             按用户意图改写当前场景：{directive}\n\
             保留已确立的人物关系，调整事件走向与张力曲线。\n\
             （模板输出，可后续接 LLM 深化）"
        ),
    };
    json!({
        "beat": {
            "id": beat_id,
            "content": content
        },
        "stableMemoryPatch": {
            "note": format!("flow={flow} 固化：用户意图「{directive}」")
        },
        "volatileMemoryPatch": {
            "lastBeat": flow,
            "directive": directive
        }
    })
}

fn template_memory(title: &str, premise: &str, context: &Value) -> Value {
    let prior_choices = context
        .get("keyChoices")
        .cloned()
        .unwrap_or_else(|| json!(["进入书中世界", "第一次关键抉择"]));
    json!({
        "summary": format!(
            "《{title}》穿书记忆摘要：以「{}」为起点，当前主线张力持续上升。",
            short(premise, 60)
        ),
        "keyChoices": prior_choices,
        "unresolvedConflicts": ["核心矛盾未决", "关键配角立场不明"],
        "divergenceFromOutline": "已产生可感知偏移，但尚未彻底脱离主线"
    })
}

fn template_ending(title: &str, premise: &str, labels: &Value, _context: &Value) -> Value {
    let tone = labels
        .get("tone")
        .and_then(|v| v.as_str())
        .unwrap_or("neutral");
    let ending = match tone {
        "dark" => "苦涩收束：代价已付，真相留下余烬",
        "light" => "轻盈收束：冲突化解，关系向前",
        "epic" => "史诗收束：世界线改写，主角成为传说的一部分",
        _ => "开放收束：主线落地，支线仍有余韵",
    };
    json!({
        "finalEnding": format!("《{title}》· {ending}"),
        "userKeyChoices": [
            format!("基于前提「{}」的关键选择", short(premise, 40)),
            "中段转向",
            "终局一搏"
        ],
        "originalOutlineComparison": "相对原大纲有中等偏移，主题落地方式被用户选择改写",
        "characterOutcomes": [
            "主角：完成弧光",
            "关键配角：立场落定"
        ],
        "worldlineName": format!("{title}-世界线-A"),
        "divergenceScore": 42
    })
}

// ---------------------------------------------------------------------------
// Heuristic classify
// ---------------------------------------------------------------------------

fn heuristic_classify(text: &str, title: Option<&str>, genre_hint: Option<&str>) -> BookTravelLabels {
    let blob = format!("{} {}", title.unwrap_or(""), text).to_lowercase();
    let mut notes: Vec<String> = Vec::new();
    notes.push("classified by keyword heuristics".into());

    let genre = if let Some(h) = genre_hint.map(str::trim).filter(|s| !s.is_empty()) {
        notes.push(format!("genre_hint honored: {h}"));
        h.to_string()
    } else if contains_any(&blob, &["sci-fi", "scifi", "space", "robot", "cyber", "科幻", "星际"]) {
        "science_fiction".into()
    } else if contains_any(&blob, &["fantasy", "magic", "dragon", "elf", "wizard", "奇幻", "魔法"]) {
        "fantasy".into()
    } else if contains_any(&blob, &["mystery", "detective", "murder", "clue", "悬疑", "推理"]) {
        "mystery".into()
    } else if contains_any(&blob, &["romance", "love", "爱情", "恋爱", "言情"]) {
        "romance".into()
    } else if contains_any(&blob, &["horror", "ghost", "恐怖", "惊悚"]) {
        "horror".into()
    } else if contains_any(&blob, &["history", "historical", "王朝", "历史"]) {
        "historical".into()
    } else if contains_any(&blob, &["xianxia", "cultivation", "修仙", "玄幻", "武侠"]) {
        "xianxia".into()
    } else {
        "general_fiction".into()
    };

    let tone = if contains_any(&blob, &["dark", "grim", "tragic", "黑暗", "悲"]) {
        "dark".into()
    } else if contains_any(&blob, &["humor", "comedy", "funny", "幽默", "轻松"]) {
        "light".into()
    } else if contains_any(&blob, &["epic", "grand", "史诗"]) {
        "epic".into()
    } else {
        "neutral".into()
    };

    let pov = if contains_any(&blob, &["first person", "i ", "我", "第一人称"]) {
        "first_person".into()
    } else if contains_any(&blob, &["second person", "you ", "你", "第二人称"]) {
        "second_person".into()
    } else {
        "third_person".into()
    };

    let era = if contains_any(&blob, &["future", "2077", "space age", "未来", "赛博"]) {
        "near_future".into()
    } else if contains_any(&blob, &["medieval", "kingdom", "中世纪", "古代"]) {
        "pre_modern".into()
    } else if contains_any(&blob, &["victorian", "1920", "ww2", "民国"]) {
        "historical_modern".into()
    } else {
        "contemporary".into()
    };

    let mut themes: Vec<String> = Vec::new();
    if contains_any(&blob, &["revenge", "复仇"]) {
        themes.push("revenge".into());
    }
    if contains_any(&blob, &["coming of age", "成长"]) {
        themes.push("coming_of_age".into());
    }
    if contains_any(&blob, &["power", "权力", "政治"]) {
        themes.push("power".into());
    }
    if contains_any(&blob, &["identity", "身份", "自我"]) {
        themes.push("identity".into());
    }
    if contains_any(&blob, &["survival", "生存", "末日"]) {
        themes.push("survival".into());
    }
    if themes.is_empty() {
        themes.push("discovery".into());
    }

    let pacing = if contains_any(&blob, &["slow burn", "slow", "缓慢"]) {
        "slow".into()
    } else if contains_any(&blob, &["fast", "thriller", "紧张", "快节奏"]) {
        "fast".into()
    } else {
        "moderate".into()
    };

    let audience = if contains_any(&blob, &["ya", "young adult", "teen", "青春"]) {
        "young_adult".into()
    } else if contains_any(&blob, &["kids", "children", "儿童"]) {
        "children".into()
    } else {
        "adult".into()
    };

    let mut hits = 0u32;
    if genre != "general_fiction" {
        hits += 1;
    }
    if tone != "neutral" {
        hits += 1;
    }
    if era != "contemporary" {
        hits += 1;
    }
    if themes.first().map(|s| s.as_str()) != Some("discovery") || themes.len() > 1 {
        hits += 1;
    }
    let confidence = (0.35 + 0.15 * hits as f64).min(0.9);

    BookTravelLabels {
        genre,
        tone,
        pov,
        era,
        themes,
        pacing,
        audience,
        mode: "heuristic".into(),
        // S7-W4
        confidence,
        notes,
    }
}

fn contains_any(hay: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| hay.contains(n))
}
