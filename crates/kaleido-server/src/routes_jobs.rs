//! Jobs v2 REST + SSE stream (P0-1 Stage2, extracted from main.rs)
use std::collections::HashMap;

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::{delete, get, post},
    Json, Router,
};
use kaleido_core::{JobEvent, JobListFilter, JobRecord, is_terminal_job_status, normalize_job_status};
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::Duration as StdDuration;
use std::convert::Infallible;

use crate::error_map::map_core_err;
use crate::error_codes::*;
use crate::auth_mw::{session_from, session_from_any};
use crate::state::AppState;

pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(jobs_list).post(jobs_create))
        .route("/cancel-all", post(jobs_cancel_all))
        .route("/drain", post(jobs_cancel_all))
        .route("/{run_id}", get(job_get))
        .route("/{run_id}", delete(jobs_delete))
        .route("/{run_id}/cancel", post(jobs_cancel))
        .route("/{run_id}/pause", post(jobs_pause))
        .route("/{run_id}/resume", post(jobs_resume))
        .route("/{run_id}/retry", post(jobs_retry))
        .route("/{run_id}/stream", get(jobs_stream))
}

pub(crate) async fn job_get(State(state): State<AppState>, headers: HeaderMap, Path(run_id): Path<String>) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    match state.jobs.get(&run_id) {
        // audit P1 IDOR: 与 jobs_cancel 一致校验归属
        Some(j) if j.workspace_id != session.workspace_id && j.user_id != session.user_id => {
            return forbidden("JOB_FORBIDDEN_SCOPE", "job not in your workspace");
        }
        Some(j) => {
            let mut v = serde_json::to_value(&j).unwrap_or_else(|_| j.to_api_json());
            if let Some(obj) = v.as_object_mut() {
                obj.insert("id".into(), json!(j.run_id));
                obj.insert("status".into(), json!(normalize_job_status(&j.status)));
            }
            Json(v).into_response()
        }
        None => return not_found("JOB_NOT_FOUND", "job not found"),
    }
}

#[derive(Deserialize)]
pub(crate) struct JobListQuery {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Deserialize)]
pub(crate) struct JobCreateBody {
    kind: String,
    #[serde(default)]
    payload: Option<Value>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    meta: Option<Value>,
}

pub(crate) async fn jobs_list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<JobListQuery>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let filter = JobListFilter {
        status: q.status,
        kind: q.kind,
        // Scope list to caller's workspace by default (isolation reserve).
        user_id: None,
        workspace_id: Some(session.workspace_id.clone()),
        limit: q.limit.unwrap_or(50),
    };
    match state.jobs.list(filter) {
        Ok(items) => {
            let jobs: Vec<Value> = items.iter().map(JobRecord::to_api_json).collect();
            Json(json!({
                "jobs": jobs,
                "count": jobs.len(),
                "maxConcurrent": state.jobs.max_concurrent(),
                "running": state.jobs.running_count(),
                "queued": state.jobs.queued_count(),
            }))
            .into_response()
        }
        Err(e) => map_core_err(e),
    }
}

pub(crate) async fn jobs_create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<JobCreateBody>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    if body.kind.trim().is_empty() {
        return bad_request("JOB_MISSING_FIELD", "kind is required");
    }
    let payload = body.payload.unwrap_or_else(|| json!({}));
    let job = match state.jobs.create(
        &body.kind,
        &session.user_id,
        &session.workspace_id,
        payload,
        body.model,
        body.meta,
    ) {
        Ok(j) => j,
        Err(e) => return map_core_err(e),
    };

    // Auto-run lightweight kinds so SSE has progress + terminal without T1/T2 workers.
    // Worker waits if the job was created as queued and later promoted.
    if matches!(job.kind.as_str(), "noop" | "test" | "other") {
        spawn_noop_job_worker(state.clone(), job.run_id.clone());
    }

    (
        StatusCode::CREATED,
        Json(job.to_api_json()),
    )
        .into_response()
}

pub(crate) async fn jobs_cancel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(run_id): Path<String>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    match state.jobs.get(&run_id) {
        Some(j) if j.workspace_id != session.workspace_id && j.user_id != session.user_id => {
            return forbidden("JOB_FORBIDDEN_SCOPE", "job not in your workspace");
        }
        None => {
            return not_found("JOB_NOT_FOUND", "job not found");
        }
        _ => {}
    }
    // Also cancel chat stream if this was a chat run.
    state.hub.cancel(&run_id);
    match state.jobs.cancel(&run_id) {
        Ok(j) => Json(j.to_api_json()).into_response(),
        Err(e) => map_core_err(e),
    }
}

/// P2: 删除一条终态 job（按 id）。仅允许删除 succeeded/failed/cancelled，
/// 运行中/排队/暂停任务拒绝。删除为物理性（active 索引 + 磁盘 JSON）。
pub(crate) async fn jobs_delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(run_id): Path<String>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    match state.jobs.get(&run_id) {
        Some(j) if j.workspace_id != session.workspace_id && j.user_id != session.user_id => {
            return forbidden("JOB_FORBIDDEN_SCOPE", "job not in your workspace");
        }
        None => {
            return not_found("JOB_NOT_FOUND", "job not found");
        }
        _ => {}
    }
    match state.jobs.delete(&run_id) {
        Ok(()) => (StatusCode::NO_CONTENT, ()).into_response(),
        Err(e) => map_core_err(e),
    }
}

/// 通用控制动作（pause/resume）：校验归属后写入 payload.control，由 exec 轮询消费。
pub(crate) async fn jobs_control_action(
    state: &AppState,
    headers: &HeaderMap,
    run_id: &str,
    action: &str,
) -> Response {
    let session = match session_from(state, headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    match state.jobs.get(run_id) {
        Some(j) if j.workspace_id != session.workspace_id && j.user_id != session.user_id => {
            return forbidden("JOB_FORBIDDEN_SCOPE", "job not in your workspace");
        }
        None => {
            return not_found("JOB_NOT_FOUND", "job not found");
        }
        _ => {}
    }
    match state.jobs.control(run_id, action) {
        Ok(j) => Json(j.to_api_json()).into_response(),
        Err(e) => map_core_err(e),
    }
}

pub(crate) async fn jobs_pause(State(state): State<AppState>, headers: HeaderMap, Path(run_id): Path<String>) -> Response {
    jobs_control_action(&state, &headers, &run_id, "pause").await
}

pub(crate) async fn jobs_resume(State(state): State<AppState>, headers: HeaderMap, Path(run_id): Path<String>) -> Response {
    jobs_control_action(&state, &headers, &run_id, "resume").await
}

/// 重试终态 job（failed/cancelled）：复位为 running 并重新调度 exec 续跑。
/// 磁盘上的 pack checkpoint 会被 exec 读取并跳过已完成阶段（幂等续跑）。
pub(crate) async fn jobs_retry(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(run_id): Path<String>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let rec = match state.jobs.get(&run_id) {
        Some(j) if j.workspace_id != session.workspace_id && j.user_id != session.user_id => {
            return forbidden("JOB_FORBIDDEN_SCOPE", "job not in your workspace");
        }
        Some(j) => j,
        None => {
            return not_found("JOB_NOT_FOUND", "job not found");
        }
    };
    if !kaleido_core::is_terminal_job_status(&rec.status) {
        return bad_request("JOB_BAD_REQUEST", "job is not in a terminal state; cannot retry");
    }
    // 复位为 running（清 error）。
    let rearmed = match state.jobs.rearm_running(&run_id) {
        Ok(j) => j,
        Err(e) => return map_core_err(e),
    };
    let slug = rearmed
        .payload
        .as_ref()
        .and_then(|p| p.get("slug"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if slug.is_empty() {
        let _ = state
            .jobs
            .complete(&run_id, "failed", None, Some("重试失败：缺少 slug".into()));
        return bad_request("JOB_BAD_REQUEST", "job payload missing slug");
    }
    let title = rearmed
        .payload
        .as_ref()
        .and_then(|p| p.get("title"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let resume_meta = rearmed.payload.clone();
    let run_id_clone = run_id.clone();
    let st = state.clone();
    tokio::spawn(async move {
        crate::crawler::exec_shelf_distil_world(st, run_id_clone, slug, title, resume_meta).await;
    });
    Json(json!({"runId": run_id, "status": "running", "retried": true})).into_response()
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct JobsCancelAllBody {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    kind: Option<String>,
}

/// Cancel all matching active jobs in the caller's workspace (drain slots).
pub(crate) async fn jobs_cancel_all(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Option<Json<JobsCancelAllBody>>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let body = body.map(|j| j.0).unwrap_or_default();
    let status_raw = body
        .status
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("active");
    let status_norm = status_raw.to_ascii_lowercase();

    // List broadly then filter: active = queued+running.
    let filter = JobListFilter {
        status: None,
        kind: body.kind.clone(),
        user_id: None,
        workspace_id: Some(session.workspace_id.clone()),
        limit: 500,
    };
    let items = match state.jobs.list(filter) {
        Ok(v) => v,
        Err(e) => return map_core_err(e),
    };

    let mut cancelled: Vec<String> = Vec::new();
    for j in items {
        let st = normalize_job_status(&j.status);
        let matches = match status_norm.as_str() {
            "active" | "all_active" | "" => st == "queued" || st == "running",
            "running" => st == "running",
            "queued" => st == "queued",
            other => st == other,
        };
        if !matches {
            continue;
        }
        state.hub.cancel(&j.run_id);
        match state.jobs.cancel(&j.run_id) {
            Ok(_) => cancelled.push(j.run_id),
            Err(_) => {
                // still count best-effort; hub already signalled
                cancelled.push(j.run_id);
            }
        }
    }

    Json(json!({
        "ok": true,
        "cancelled": cancelled,
        "count": cancelled.len(),
        "running": state.jobs.running_count(),
        "queued": state.jobs.queued_count(),
    }))
    .into_response()
}

/// SSE stream for jobs v2: replays stored events, then polls for new ones until terminal.
/// Auth is enforced by middleware (Authorization bearer, or `?ticket=` for EventSource).
pub(crate) async fn jobs_stream(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(run_id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    // M-3: EventSource cannot send headers — accept a one-time ?ticket= here.
    let session = match session_from_any(&state, &headers, Some(&params)) {
        Ok(s) => s,
        Err(r) => return r,
    };
    // audit P1 IDOR: 与 jobs_cancel 一致校验归属
    match state.jobs.get(&run_id) {
        Some(j) if j.workspace_id != session.workspace_id && j.user_id != session.user_id => {
            return forbidden("JOB_FORBIDDEN_SCOPE", "job not in your workspace");
        }
        None => {
            return not_found("JOB_NOT_FOUND", "job not found");
        }
        _ => {}
    }
    stream_job_sse(state, run_id).await
}

pub(crate) async fn stream_job_sse(state: AppState, run_id: String) -> Response {
    let stream = async_stream::stream! {
        let mut sent = 0usize;
        let mut ticks = 0u32;
        loop {
            let Some(job) = state.jobs.get(&run_id) else {
                let payload = json!({
                    "runId": run_id,
                    "eventType": "error",
                    "code": "JOB_NOT_FOUND",
                    "message": "job not found",
                });
                yield Ok::<Event, Infallible>(
                    Event::default()
                        .event("error")
                        .data(payload.to_string())
                );
                break;
            };

            while sent < job.events.len() {
                let ev = &job.events[sent];
                let payload = json!({
                    "runId": run_id,
                    "eventType": ev.event_type,
                    "message": ev.message,
                    // D2: error events carry stable code (P1-4 envelope parity)
                    "code": ev.code,
                    "progress": ev.progress.or(job.progress),
                    "data": ev.data,
                    "status": normalize_job_status(&job.status),
                    "ts": ev.ts,
                });
                let etype = ev.event_type.clone();
                yield Ok::<Event, Infallible>(
                    Event::default()
                        .event(etype)
                        .data(payload.to_string())
                );
                sent += 1;
            }

            if is_terminal_job_status(&job.status) {
                // Ensure a terminal event is always emitted even if none stored.
                if job.events.is_empty()
                    || !job
                        .events
                        .iter()
                        .any(|e| matches!(e.event_type.as_str(), "done" | "error"))
                {
                    let (etype, ecode) = if normalize_job_status(&job.status) == "failed" {
                        ("error", "JOB_FAILED")
                    } else {
                        ("done", "")
                    };
                    let mut payload = json!({
                        "runId": run_id,
                        "eventType": etype,
                        "message": job.progress_message.clone().or(job.error.clone()),
                        "progress": job.progress,
                        "status": normalize_job_status(&job.status),
                        "result": job.result,
                    });
                    if !ecode.is_empty() {
                        if let Some(obj) = payload.as_object_mut() {
                            obj.insert("code".into(), json!(ecode));
                        }
                    }
                    yield Ok::<Event, Infallible>(
                        Event::default()
                            .event(etype)
                            .data(payload.to_string())
                    );
                }
                break;
            }

            ticks += 1;
            if ticks > 600 {
                // ~60s safety
                let payload = json!({
                    "runId": run_id,
                    "eventType": "event",
                    "message": "stream timeout",
                    "status": normalize_job_status(&job.status),
                });
                yield Ok::<Event, Infallible>(
                    Event::default()
                        .event("event")
                        .data(payload.to_string())
                );
                break;
            }
            tokio::time::sleep(StdDuration::from_millis(100)).await;
        }
    };

    Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(StdDuration::from_secs(15)))
        .into_response()
}

pub(crate) fn spawn_noop_job_worker(state: AppState, run_id: String) {
    tokio::spawn(async move {
        let jobs = state.jobs.clone();
        // Wait until promoted to running (or cancelled while queued).
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
        let _ = jobs.push_event(
            &run_id,
            JobEvent::progress("noop step 1", 0.3),
            Some(0.3),
            None,
        );
        tokio::time::sleep(StdDuration::from_millis(50)).await;
        if jobs
            .get(&run_id)
            .map(|j| is_terminal_job_status(&j.status))
            .unwrap_or(true)
        {
            return;
        }
        let _ = jobs.push_event(
            &run_id,
            JobEvent::progress("noop step 2", 0.7),
            Some(0.7),
            Some("noop-cursor".into()),
        );
        tokio::time::sleep(StdDuration::from_millis(50)).await;
        if jobs
            .get(&run_id)
            .map(|j| is_terminal_job_status(&j.status))
            .unwrap_or(true)
        {
            return;
        }
        let _ = jobs.complete(
            &run_id,
            "succeeded",
            Some(json!({"kind": "noop", "ok": true})),
            None,
        );
    });
}
