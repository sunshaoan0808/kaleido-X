//! Background multi-stage job core (S5-W2 T2).
//!
//! Uses JobStore v2 (`create` / `push_event` / `complete` / `cancel`).
//! Routes (wired from main):
//!   POST /api/v1/background/start
//!   POST /api/v1/background/{stage}   — stage ∈ stage_one|items|character_card|pipeline
//!   POST /api/v1/background/stop
//!   GET  /api/v1/background/stream?id=...
//!   GET  /api/v1/background/runs/{id}           — W1+ status/checkpoint
//!   POST /api/v1/background/runs/{id}/resume    — W1+ resume from checkpoint
//!   POST /api/v1/background/apply    — map job result → Partner world books / cards
//!
//! Stages (structured templates aligned with upstream v0.9.2 shapes):
//!   stage_one      → worldBooks + characterNames
//!   items          → worldBooks + characterCards (full extract)
//!   character_card → single character card for characterName
//!   pipeline       → stage_one → items → deepen up to deepenMax character cards (checkpointed)

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use kaleido_core::{
    is_terminal_job_status, normalize_job_status, JobEvent, PartnerItem,
};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::Duration as StdDuration;
use uuid::Uuid;

use crate::{map_core_err, session_from, stream_job_sse, AppState};
use crate::error_codes::*;

const STAGES: &[&str] = &["stage_one", "items", "character_card", "pipeline"];

/// Request body for `POST /api/v1/background/start` and `/{stage}`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundStartBody {
    /// Free-form seed / premise / reference text.
    #[serde(default)]
    pub premise: Option<String>,
    /// Alias of premise (upstream uses `text`).
    #[serde(default)]
    pub text: Option<String>,
    /// Optional title for the generated background.
    #[serde(default)]
    pub title: Option<String>,
    /// Which stage: `stage_one` | `items` | `character_card`.
    #[serde(default)]
    pub mode: Option<String>,
    /// For character_card stage.
    #[serde(default)]
    pub character_name: Option<String>,
    /// Optional world-book context for character_card.
    #[serde(default)]
    pub world_book_context: Option<String>,
    /// stage_one: include characterNames list (default true).
    #[serde(default)]
    pub include_character_names: Option<bool>,
    /// Optional model override.
    #[serde(default)]
    pub model: Option<String>,
    /// Opaque extra payload merged into job.payload.
    #[serde(default)]
    pub payload: Option<Value>,
    /// Skip LLM; use heuristic templates (smoke / offline).
    #[serde(default)]
    pub prefer_heuristic: Option<bool>,
    /// Pipeline: max character cards to deepen (default 5; 0 = skip deepen).
    #[serde(default)]
    pub deepen_max: Option<usize>,
    /// Pipeline deepen policy: `all` (default, capped by deepenMax) | `first` | `none`.
    #[serde(default)]
    pub deepen_mode: Option<String>,
}

/// Request body for `POST /api/v1/background/stop`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundStopBody {
    #[serde(alias = "run_id", alias = "runId", alias = "jobId")]
    pub id: String,
}

/// Query for `GET /api/v1/background/stream`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundStreamQuery {
    #[serde(alias = "run_id", alias = "runId", alias = "jobId")]
    pub id: String,
}

fn normalize_stage(raw: &str) -> Option<String> {
    let s = raw.trim().to_ascii_lowercase().replace('-', "_");
    match s.as_str() {
        "stage_one" | "stageone" | "one" => Some("stage_one".into()),
        "items" | "item" => Some("items".into()),
        "character_card" | "charactercard" | "card" | "character" => Some("character_card".into()),
        "pipeline" | "full" | "all" => Some("pipeline".into()),
        _ => None,
    }
}

fn start_inner(
    state: AppState,
    session: kaleido_core::SessionRecord,
    body: BackgroundStartBody,
    stage_override: Option<String>,
) -> axum::response::Response {
    let mode = stage_override
        .or_else(|| body.mode.clone())
        .as_deref()
        .map(normalize_stage)
        .unwrap_or_else(|| Some("stage_one".into()));
    let Some(mode) = mode else {
        return bad_request("BG_BAD_REQUEST", format!("mode must be one of: {}", STAGES.join(", ")));
    };

    if mode == "character_card" {
        let name = body
            .character_name
            .as_deref()
            .unwrap_or("")
            .trim()
            .to_string();
        if name.is_empty() {
            return bad_request("BG_MISSING_FIELD", "characterName is required for character_card stage");
        }
    }

    let text = body
        .text
        .clone()
        .or_else(|| body.premise.clone())
        .unwrap_or_default();
    let include_names = body.include_character_names.unwrap_or(true);

    let mut payload = body.payload.unwrap_or_else(|| json!({}));
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("mode".into(), json!(mode));
        obj.insert("stage".into(), json!(mode));
        if !text.is_empty() {
            obj.insert("premise".into(), json!(text.clone()));
            obj.insert("text".into(), json!(text.clone()));
        }
        if let Some(t) = body.title.clone() {
            obj.insert("title".into(), json!(t));
        }
        if let Some(n) = body.character_name.clone() {
            obj.insert("characterName".into(), json!(n));
        }
        if let Some(w) = body.world_book_context.clone() {
            obj.insert("worldBookContext".into(), json!(w));
        }
        obj.insert("includeCharacterNames".into(), json!(include_names));
        obj.insert("feature".into(), json!("background"));
        if let Some(h) = body.prefer_heuristic {
            obj.insert("preferHeuristic".into(), json!(h));
        }
        let deepen_mode = body
            .deepen_mode
            .as_deref()
            .unwrap_or("all")
            .trim()
            .to_ascii_lowercase();
        obj.insert("deepenMode".into(), json!(deepen_mode));
        let deepen_max = body.deepen_max.unwrap_or(5);
        obj.insert("deepenMax".into(), json!(deepen_max));
        // Fresh start clears any accidental checkpoint in extra payload
        obj.remove("checkpoint");
        obj.insert("resume".into(), json!(false));
    }

    let meta = json!({
        "feature": "background",
        "mode": mode,
        "stage": mode,
        "title": body.title,
        "characterName": body.character_name,
    });

    let job = match state.jobs.create(
        "background",
        &session.user_id,
        &session.workspace_id,
        payload,
        body.model.or_else(|| Some(state.llm_model.clone())),
        Some(meta),
    ) {
        Ok(j) => j,
        Err(e) => return map_core_err(e),
    };

    spawn_background_worker(state.clone(), job.run_id.clone());

    (
        StatusCode::CREATED,
        Json(json!({
            "id": job.run_id,
            "runId": job.run_id,
            "kind": job.kind,
            "stage": mode,
            "status": normalize_job_status(&job.status),
            "stream": format!("/api/v1/background/stream?id={}", job.run_id),
            "jobsStream": format!("/api/v1/jobs/{}/stream", job.run_id),
            "progress": job.progress,
            "progressMessage": job.progress_message,
            "payload": job.payload,
        })),
    )
        .into_response()
}

/// `POST /api/v1/background/start`
pub async fn start(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<BackgroundStartBody>,
) -> axum::response::Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    start_inner(state, session, body, None)
}

/// `POST /api/v1/background/{stage}` — S5-W2 T2 stage router.
pub async fn start_stage(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(stage): Path<String>,
    Json(body): Json<BackgroundStartBody>,
) -> axum::response::Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let Some(norm) = normalize_stage(&stage) else {
        return bad_request("BG_BAD_REQUEST", format!("unknown stage '{stage}'; expected: {}", STAGES.join(", ")));
    };
    start_inner(state, session, body, Some(norm))
}

/// `POST /api/v1/background/stop`
pub async fn stop(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<BackgroundStopBody>,
) -> axum::response::Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };

    let run_id = body.id.trim();
    if run_id.is_empty() {
        return bad_request("BG_MISSING_FIELD", "id is required");
    }

    match state.jobs.get(run_id) {
        Some(j) if j.workspace_id != session.workspace_id && j.user_id != session.user_id => {
            return forbidden("BG_FORBIDDEN_SCOPE", "job not in your workspace");
        }
        Some(j) if j.kind != "background" => {
            return bad_request("BG_BAD_REQUEST", "not a background job");
        }
        None => {
            return not_found("BG_NOT_FOUND", "job not found");
        }
        _ => {}
    }

    match state.jobs.cancel(run_id) {
        Ok(j) => Json(j.to_api_json()).into_response(),
        Err(e) => map_core_err(e),
    }
}

/// `GET /api/v1/background/stream?id=...`
pub async fn stream(
    State(state): State<AppState>,
    Query(q): Query<BackgroundStreamQuery>,
) -> axum::response::Response {
    let run_id = q.id.trim().to_string();
    if run_id.is_empty() {
        return bad_request("BG_MISSING_FIELD", "id query param is required");
    }
    match state.jobs.get(&run_id) {
        None => return not_found("BG_NOT_FOUND", "job not found"),
        Some(j) if j.kind != "background" => return bad_request("BG_BAD_REQUEST", "not a background job"),
        Some(_) => stream_job_sse(state, run_id).await,
    }
}

/// `GET /api/v1/background/runs/{id}` — job + checkpoint summary (W1+).
pub async fn get_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(run_id): Path<String>,
) -> axum::response::Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let run_id = run_id.trim();
    let Some(j) = state.jobs.get(run_id) else {
        return not_found("BG_NOT_FOUND", "job not found");
    };
    if j.workspace_id != session.workspace_id && j.user_id != session.user_id {
        return forbidden("BG_FORBIDDEN_SCOPE", "job not in your workspace");
    }
    if j.kind != "background" {
        return bad_request("BG_WRONG_KIND", "not a background job");
    }
    let checkpoint = j
        .payload
        .as_ref()
        .and_then(|p| p.get("checkpoint"))
        .cloned();
    let resumable = {
        let st = normalize_job_status(&j.status);
        st != "succeeded"
            && checkpoint
                .as_ref()
                .and_then(|c| c.get("completed"))
                .and_then(|v| v.as_array())
                .map(|a| !a.is_empty())
                .unwrap_or(false)
    };
    Json(json!({
        "ok": true,
        "schemaVersion": 1,
        "id": j.run_id,
        "runId": j.run_id,
        "kind": j.kind,
        "status": normalize_job_status(&j.status),
        "progress": j.progress,
        "progressMessage": j.progress_message,
        "cursor": j.cursor,
        "checkpoint": checkpoint,
        "resumable": resumable,
        "result": j.result,
        "error": j.error,
        "payload": j.payload,
        "stream": format!("/api/v1/background/stream?id={}", j.run_id),
        "createdAt": j.created_at,
        "updatedAt": j.updated_at,
    }))
    .into_response()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundResumeBody {
    /// Optional override deepenMax on resume.
    #[serde(default)]
    pub deepen_max: Option<usize>,
    #[serde(default)]
    pub prefer_heuristic: Option<bool>,
}

/// `POST /api/v1/background/runs/{id}/resume` — continue pipeline from checkpoint (W1+).
pub async fn resume(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(run_id): Path<String>,
    body: Option<Json<BackgroundResumeBody>>,
) -> axum::response::Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let body = body.map(|j| j.0).unwrap_or(BackgroundResumeBody {
        deepen_max: None,
        prefer_heuristic: None,
    });
    let run_id = run_id.trim().to_string();
    let Some(j) = state.jobs.get(&run_id) else {
        return not_found("BG_NOT_FOUND", "job not found");
    };
    if j.workspace_id != session.workspace_id && j.user_id != session.user_id {
        return forbidden("BG_FORBIDDEN_SCOPE", "job not in your workspace");
    }
    if j.kind != "background" {
        return bad_request("BG_WRONG_KIND", "not a background job");
    }
    let st = normalize_job_status(&j.status);
    if st == "succeeded" {
        return err_with_code(
            StatusCode::BAD_REQUEST,
            "BG_ALREADY_DONE", "job already succeeded",
            serde_json::json!({"runId": run_id,
                "result": j.result}),
        );
    }
    // Refuse live running job (worker still attached). Smoke should stop first.
    // After process restart, disk-orphaned "running" is rearm-able.
    if st == "running" {
        // If progress_message is "resuming" just after rearm, allow; otherwise 409.
        let msg = j.progress_message.as_deref().unwrap_or("");
        if msg != "resuming" {
            return err_with_code(
            StatusCode::CONFLICT,
            "BG_STILL_RUNNING", "job still running; stop first or wait",
            serde_json::json!({"runId": run_id,
                    "hint": "POST /api/v1/background/stop {\"id\": runId} then resume"}),
        );
        }
    }

    let mode = j
        .payload
        .as_ref()
        .and_then(|p| p.get("mode"))
        .and_then(|m| m.as_str())
        .unwrap_or("");
    let has_cp = j
        .payload
        .as_ref()
        .and_then(|p| p.get("checkpoint"))
        .is_some();
    if mode != "pipeline" && !has_cp {
        return err_with_code(
            StatusCode::BAD_REQUEST,
            "BG_NO_CHECKPOINT", "no checkpoint to resume (only pipeline jobs checkpoint)",
            serde_json::json!({"runId": run_id}),
        );
    }

    let mut extra = json!({
        "resume": true,
        "mode": "pipeline",
        "stage": "pipeline",
    });
    if let Some(obj) = extra.as_object_mut() {
        if let Some(h) = body.prefer_heuristic {
            obj.insert("preferHeuristic".into(), json!(h));
        }
        if let Some(d) = body.deepen_max {
            obj.insert("deepenMax".into(), json!(d));
        }
    }
    if let Err(e) = state.jobs.merge_job_payload(&run_id, extra) {
        return map_core_err(e);
    }
    // Touch checkpoint progress_message for observability
    if let Some(cp) = state
        .jobs
        .get(&run_id)
        .and_then(|jj| jj.payload.as_ref().and_then(|p| p.get("checkpoint").cloned()))
    {
        let _ = state.jobs.set_checkpoint(
            &run_id,
            cp,
            Some("resume".into()),
            None,
            Some("resume requested".into()),
        );
    }

    match state.jobs.rearm_running(&run_id) {
        Ok(j3) => {
            spawn_background_worker(state.clone(), run_id.clone());
            (
                StatusCode::OK,
                Json(json!({
                    "ok": true,
                    "resumed": true,
                    "id": j3.run_id,
                    "runId": j3.run_id,
                    "status": normalize_job_status(&j3.status),
                    "checkpoint": j3.payload.as_ref().and_then(|p| p.get("checkpoint")),
                    "stream": format!("/api/v1/background/stream?id={}", j3.run_id),
                    "schemaVersion": 1,
                })),
            )
                .into_response()
        }
        Err(e) => map_core_err(e),
    }
}

fn spawn_background_worker(state: AppState, run_id: String) {
    tokio::spawn(async move {
        let jobs = state.jobs.clone();

        // Wait until promoted to running, or cancelled while queued.
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
        let mode = payload
            .get("mode")
            .and_then(|v| v.as_str())
            .unwrap_or("stage_one")
            .to_string();
        let premise = payload
            .get("text")
            .or_else(|| payload.get("premise"))
            .and_then(|v| v.as_str())
            .unwrap_or("An original story world.")
            .to_string();
        let title = payload
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("Untitled Background")
            .to_string();
        let character_name = payload
            .get("characterName")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let world_book_context = payload
            .get("worldBookContext")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let include_names = payload
            .get("includeCharacterNames")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        if cancelled(&jobs, &run_id) {
            return;
        }

        let _ = jobs.push_event(
            &run_id,
            JobEvent::progress(format!("{mode}: generating"), 0.15),
            Some(0.15),
            Some(format!("{mode}:start")),
        );
        tokio::time::sleep(StdDuration::from_millis(30)).await;
        if cancelled(&jobs, &run_id) {
            return;
        }

        // Full pipeline: stage_one → items → deepen characters (checkpointed, W1+)
        if mode == "pipeline" {
            let prefer_heuristic = payload
                .get("preferHeuristic")
                .and_then(|v| v.as_bool())
                .or_else(|| {
                    payload
                        .get("checkpoint")
                        .and_then(|c| c.get("preferHeuristic"))
                        .and_then(|v| v.as_bool())
                })
                .unwrap_or(false);
            let deepen_max = payload
                .get("deepenMax")
                .and_then(|v| v.as_u64())
                .or_else(|| {
                    payload
                        .get("checkpoint")
                        .and_then(|c| c.get("deepenMax"))
                        .and_then(|v| v.as_u64())
                })
                .unwrap_or(5) as usize;
            let deepen_mode = payload
                .get("deepenMode")
                .and_then(|v| v.as_str())
                .unwrap_or("all")
                .to_string();
            let existing_cp = payload.get("checkpoint").cloned();
            let result = run_pipeline(
                &state,
                &jobs,
                &run_id,
                &title,
                &premise,
                include_names,
                prefer_heuristic,
                deepen_max,
                &deepen_mode,
                existing_cp,
            )
            .await;
            if cancelled(&jobs, &run_id) {
                return;
            }
            if let Some(result) = result {
                let _ = jobs.complete(&run_id, "succeeded", Some(result), None);
            } else if !cancelled(&jobs, &run_id) {
                let _ = jobs.complete(
                    &run_id,
                    "failed",
                    Some(json!({
                        "kind": "background",
                        "stage": "pipeline",
                        "ok": false,
                        "error": "pipeline aborted without result",
                    })),
                    Some("pipeline failed".into()),
                );
            }
            return;
        }

        // Stream-parity: prefer upstream-quality LLM stream; soft-fail to heuristic.
        let prefer_heuristic = payload
            .get("preferHeuristic")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let llm_try = if prefer_heuristic {
            None
        } else {
            try_background_llm_stream(
                &state,
                &jobs,
                &run_id,
                &mode,
                &title,
                &premise,
                &character_name,
                &world_book_context,
                include_names,
            )
            .await
        };
        let result = if let Some(v) = llm_try {
            let _ = jobs.push_event(
                &run_id,
                JobEvent::event(
                    "llm stage complete",
                    Some(json!({"stage": mode, "generationMode": "llm"})),
                ),
                Some(0.85),
                Some(format!("{mode}:llm_done")),
            );
            v
        } else {
            match mode.as_str() {
                "items" => {
                    let data = template_items(&title, &premise);
                    let _ = jobs.push_event(
                        &run_id,
                        JobEvent::event(
                            "items complete",
                            Some(json!({
                                "stage": "items",
                                "worldBooks": data.get("worldBooks"),
                                "characterCards": data.get("characterCards"),
                            })),
                        ),
                        Some(0.85),
                        Some("items:done".into()),
                    );
                    json!({
                        "kind": "background",
                        "schemaVersion": 1,
                        "stage": "items",
                        "mode": "items",
                        "title": title,
                        "worldBooks": data.get("worldBooks"),
                        "characterCards": data.get("characterCards"),
                        "ok": true,
                        "mvp": true,
                        "fallback": true,
                        "generationMode": "heuristic",
                    })
                }
                "character_card" => {
                    let card =
                        template_character_card(&character_name, &premise, &world_book_context);
                    let _ = jobs.push_event(
                        &run_id,
                        JobEvent::event(
                            "character_card complete",
                            Some(json!({
                                "stage": "character_card",
                                "characterCard": card,
                            })),
                        ),
                        Some(0.85),
                        Some("character_card:done".into()),
                    );
                    json!({
                        "kind": "background",
                        "stage": "character_card",
                        "mode": "character_card",
                        "title": title,
                        "characterName": character_name,
                        "characterCard": card,
                        "ok": true,
                        "mvp": true,
                        "fallback": true,
                        "generationMode": "heuristic",
                    })
                }
                // stage_one (default)
                _ => {
                    let data = template_stage_one(&title, &premise, include_names);
                    let _ = jobs.push_event(
                        &run_id,
                        JobEvent::event(
                            "stage_one complete",
                            Some(json!({
                                "stage": "stage_one",
                                "title": title,
                                "worldBooks": data.get("worldBooks"),
                                "characterNames": data.get("characterNames"),
                            })),
                        ),
                        Some(0.85),
                        Some("stage_one:done".into()),
                    );
                    json!({
                        "kind": "background",
                        "stage": "stage_one",
                        "mode": "stage_one",
                        "title": title,
                        "worldBooks": data.get("worldBooks"),
                        "characterNames": data.get("characterNames"),
                        "ok": true,
                        "mvp": true,
                        "fallback": true,
                        "generationMode": "heuristic",
                    })
                }
            }
        };

        tokio::time::sleep(StdDuration::from_millis(20)).await;
        if cancelled(&jobs, &run_id) {
            return;
        }
        let _ = jobs.complete(&run_id, "succeeded", Some(result), None);
    });
}



/// Run one logical stage; returns structured result value or None if cancelled/hard-fail.
async fn run_one_stage(
    state: &AppState,
    jobs: &kaleido_core::JobStore,
    run_id: &str,
    mode: &str,
    title: &str,
    premise: &str,
    character_name: &str,
    world_book_context: &str,
    include_names: bool,
    prefer_heuristic: bool,
    progress_lo: f64,
    progress_hi: f64,
) -> Option<Value> {
    if cancelled(jobs, run_id) {
        return None;
    }
    let _ = jobs.push_event(
        run_id,
        JobEvent::progress(format!("pipeline:{mode}"), progress_lo),
        Some(progress_lo),
        Some(format!("pipeline:{mode}:start")),
    );

    // Yield so stop/resume smoke can observe mid-pipeline checkpoints (heuristic is otherwise instant).
    if prefer_heuristic {
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        if cancelled(jobs, run_id) {
            return None;
        }
    }

    let llm_try = if prefer_heuristic {
        None
    } else {
        try_background_llm_stream(
            state,
            jobs,
            run_id,
            mode,
            title,
            premise,
            character_name,
            world_book_context,
            include_names,
        )
        .await
    };

    if cancelled(jobs, run_id) {
        return None;
    }

    let result = if let Some(v) = llm_try {
        v
    } else {
        match mode {
            "items" => {
                let data = template_items(title, premise);
                json!({
                    "kind": "background",
                    "stage": "items",
                    "mode": "items",
                    "title": title,
                    "worldBooks": data.get("worldBooks"),
                    "characterCards": data.get("characterCards"),
                    "ok": true,
                    "mvp": true,
                    "fallback": true,
                    "generationMode": "heuristic",
                })
            }
            "character_card" => {
                let card = template_character_card(character_name, premise, world_book_context);
                json!({
                    "kind": "background",
                    "stage": "character_card",
                    "mode": "character_card",
                    "title": title,
                    "characterName": character_name,
                    "characterCard": card,
                    "ok": true,
                    "mvp": true,
                    "fallback": true,
                    "generationMode": "heuristic",
                })
            }
            _ => {
                let data = template_stage_one(title, premise, include_names);
                json!({
                    "kind": "background",
                    "stage": "stage_one",
                    "mode": "stage_one",
                    "title": title,
                    "worldBooks": data.get("worldBooks"),
                    "characterNames": data.get("characterNames"),
                    "ok": true,
                    "mvp": true,
                    "fallback": true,
                    "generationMode": "heuristic",
                })
            }
        }
    };

    let _ = jobs.push_event(
        run_id,
        JobEvent::event(
            format!("{mode} complete"),
            Some(json!({
                "stage": mode,
                "pipeline": true,
                "result": result,
            })),
        ),
        Some(progress_hi),
        Some(format!("pipeline:{mode}:done")),
    );
    Some(result)
}

async fn run_pipeline(
    state: &AppState,
    jobs: &kaleido_core::JobStore,
    run_id: &str,
    title: &str,
    premise: &str,
    include_names: bool,
    prefer_heuristic: bool,
    deepen_max: usize,
    deepen_mode: &str,
    existing_cp: Option<Value>,
) -> Option<Value> {
    let mut completed: Vec<String> = existing_cp
        .as_ref()
        .and_then(|c| c.get("completed"))
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    let mut stage_one = existing_cp
        .as_ref()
        .and_then(|c| c.get("stageOne"))
        .cloned()
        .unwrap_or(Value::Null);
    let mut items = existing_cp
        .as_ref()
        .and_then(|c| c.get("items"))
        .cloned()
        .unwrap_or(Value::Null);
    let mut deepened_cards: Vec<Value> = existing_cp
        .as_ref()
        .and_then(|c| c.get("deepenedCards"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut deepened_names: Vec<String> = existing_cp
        .as_ref()
        .and_then(|c| c.get("deepenedNames"))
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    let persist_cp = |jobs: &kaleido_core::JobStore,
                      run_id: &str,
                      completed: &[String],
                      stage_one: &Value,
                      items: &Value,
                      deepened_cards: &[Value],
                      deepened_names: &[String],
                      next: &str,
                      progress: f64| {
        let cp = json!({
            "schemaVersion": 1,
            "completed": completed,
            "next": next,
            "stageOne": stage_one,
            "items": items,
            "deepenedCards": deepened_cards,
            "deepenedNames": deepened_names,
            "title": title,
            "premise": premise,
        });
        let _ = jobs.set_checkpoint(
            run_id,
            cp,
            Some(next.to_string()),
            Some(progress),
            Some(format!("checkpoint:{next}")),
        );
        let _ = jobs.push_event(
            run_id,
            JobEvent::event(
                format!("checkpoint:{next}"),
                Some(json!({"next": next, "completed": completed})),
            ),
            Some(progress),
            Some(format!("checkpoint:{next}")),
        );
    };

    // Stage 1
    if !completed.iter().any(|s| s == "stage_one") {
        stage_one = run_one_stage(
            state,
            jobs,
            run_id,
            "stage_one",
            title,
            premise,
            "",
            "",
            include_names,
            prefer_heuristic,
            0.08,
            0.32,
        )
        .await?;
        if cancelled(jobs, run_id) {
            return None;
        }
        completed.push("stage_one".into());
        persist_cp(
            jobs,
            run_id,
            &completed,
            &stage_one,
            &items,
            &deepened_cards,
            &deepened_names,
            "items",
            0.35,
        );
    }

    // Stage 2 items
    if !completed.iter().any(|s| s == "items") {
        let mut items_premise = premise.to_string();
        if let Some(names) = stage_one.get("characterNames").and_then(|v| v.as_array()) {
            let list: Vec<&str> = names.iter().filter_map(|x| x.as_str()).collect();
            if !list.is_empty() {
                items_premise.push_str("\n\n角色名候选：");
                items_premise.push_str(&list.join("、"));
            }
        }
        if let Some(wbs) = stage_one.get("worldBooks").and_then(|v| v.as_array()) {
            if let Some(name) = wbs
                .first()
                .and_then(|w| w.get("name"))
                .and_then(|n| n.as_str())
            {
                items_premise.push_str("\n世界书草案：");
                items_premise.push_str(name);
            }
        }
        items = run_one_stage(
            state,
            jobs,
            run_id,
            "items",
            title,
            &items_premise,
            "",
            "",
            include_names,
            prefer_heuristic,
            0.40,
            0.62,
        )
        .await?;
        if cancelled(jobs, run_id) {
            return None;
        }
        completed.push("items".into());
        persist_cp(
            jobs,
            run_id,
            &completed,
            &stage_one,
            &items,
            &deepened_cards,
            &deepened_names,
            "character_cards",
            0.65,
        );
    }

    // Stage 3: deepen characters (multi, not first-only)
    let deepen_mode_l = deepen_mode.trim().to_ascii_lowercase();
    let do_deepen = deepen_mode_l != "none" && deepen_max > 0;
    if do_deepen && !completed.iter().any(|s| s == "character_cards") {
        let mut names: Vec<String> = Vec::new();
        if let Some(arr) = items.get("characterCards").and_then(|v| v.as_array()) {
            for c in arr {
                if let Some(n) = c.get("name").and_then(|x| x.as_str()) {
                    let n = n.trim();
                    if !n.is_empty() && !names.iter().any(|x| x == n) {
                        names.push(n.to_string());
                    }
                }
            }
        }
        if let Some(arr) = stage_one.get("characterNames").and_then(|v| v.as_array()) {
            for n in arr {
                if let Some(n) = n.as_str() {
                    let n = n.trim();
                    if !n.is_empty() && !names.iter().any(|x| x == n) {
                        names.push(n.to_string());
                    }
                }
            }
        }
        if deepen_mode_l == "first" {
            names.truncate(1);
        } else {
            names.truncate(deepen_max);
        }

        let wb_ctx = world_book_context_summary(
            items
                .get("worldBooks")
                .or_else(|| stage_one.get("worldBooks"))
                .unwrap_or(&json!([])),
            premise,
        );

        let n_total = names.len().max(1) as f64;
        for (i, name) in names.iter().enumerate() {
            if deepened_names.iter().any(|d| d == name) {
                continue;
            }
            if cancelled(jobs, run_id) {
                return None;
            }
            let lo = 0.68 + (i as f64 / n_total) * 0.22;
            let hi = 0.68 + ((i as f64 + 1.0) / n_total) * 0.22;
            if let Some(card_res) = run_one_stage(
                state,
                jobs,
                run_id,
                "character_card",
                title,
                premise,
                name,
                &wb_ctx,
                false,
                prefer_heuristic,
                lo,
                hi,
            )
            .await
            {
                let card = card_res
                    .get("characterCard")
                    .cloned()
                    .unwrap_or(card_res);
                deepened_cards.push(card);
                deepened_names.push(name.clone());
                persist_cp(
                    jobs,
                    run_id,
                    &completed,
                    &stage_one,
                    &items,
                    &deepened_cards,
                    &deepened_names,
                    "character_cards",
                    hi,
                );
            }
        }
        completed.push("character_cards".into());
        persist_cp(
            jobs,
            run_id,
            &completed,
            &stage_one,
            &items,
            &deepened_cards,
            &deepened_names,
            "done",
            0.94,
        );
    } else if !do_deepen && !completed.iter().any(|s| s == "character_cards") {
        completed.push("character_cards".into());
        persist_cp(
            jobs,
            run_id,
            &completed,
            &stage_one,
            &items,
            &deepened_cards,
            &deepened_names,
            "done",
            0.94,
        );
    }

    if cancelled(jobs, run_id) {
        return None;
    }

    let world_books = items
        .get("worldBooks")
        .cloned()
        .or_else(|| stage_one.get("worldBooks").cloned())
        .unwrap_or(json!([]));
    let mut character_cards = items
        .get("characterCards")
        .cloned()
        .unwrap_or(json!([]));
    if let Some(arr) = character_cards.as_array_mut() {
        for card in &deepened_cards {
            let name = card
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if let Some(pos) = arr
                .iter()
                .position(|c| c.get("name").and_then(|n| n.as_str()) == Some(name))
            {
                arr[pos] = card.clone();
            } else {
                arr.push(card.clone());
            }
        }
    }

    let gen_mode = if prefer_heuristic {
        "heuristic"
    } else {
        "pipeline"
    };

    // Final checkpoint: complete
    let final_cp = json!({
        "schemaVersion": 1,
        "completed": ["stage_one", "items", "character_cards", "done"],
        "next": null,
        "stageOne": stage_one,
        "items": items,
        "deepenedCards": deepened_cards,
        "deepenedNames": deepened_names,
    });
    let _ = jobs.set_checkpoint(
        run_id,
        final_cp.clone(),
        Some("done".into()),
        Some(1.0),
        Some("pipeline complete".into()),
    );

    Some(json!({
        "kind": "background",
        "schemaVersion": 1,
        "stage": "pipeline",
        "mode": "pipeline",
        "title": title,
        "ok": true,
        "pipeline": true,
        "resumable": false,
        "checkpoint": final_cp,
        "stages": {
            "stage_one": stage_one,
            "items": items,
            "character_cards": deepened_cards,
        },
        "worldBooks": world_books,
        "characterCards": character_cards,
        "characterNames": stage_one.get("characterNames"),
        "deepenedCount": deepened_names.len(),
        "deepenedNames": deepened_names,
        "deepenMax": deepen_max,
        "deepenMode": deepen_mode,
        "generationMode": gen_mode,
        "preferHeuristic": prefer_heuristic,
    }))
}

async fn try_background_llm_stream(
    state: &AppState,
    jobs: &kaleido_core::JobStore,
    run_id: &str,
    mode: &str,
    title: &str,
    premise: &str,
    character_name: &str,
    world_book_context: &str,
    include_names: bool,
) -> Option<serde_json::Value> {
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

    let (system, user, max_tokens, timeout_secs) =
        background_prompts(mode, title, premise, character_name, world_book_context, include_names);

    let mut chars: usize = 0;
    let jobs_c = jobs.clone();
    let run_c = run_id.to_string();
    let full = match crate::llm_stream::stream_chat_completions_dispatch(
        &llm.base_url,
        &llm.api_key,
        &model,
        &prov_kind,
        &system,
        &user,
        0.5,
        max_tokens,
        timeout_secs,
        |chunk| {
            if cancelled(&jobs_c, &run_c) {
                return false;
            }
            chars = chars.saturating_add(chunk.chars().count());
            // map rough progress 0.2 → 0.8 while streaming
            let p = (0.2 + (chars as f64 / 4000.0).min(0.55)).min(0.8);
            let _ = jobs_c.push_event(
                &run_c,
                JobEvent {
                    event_type: "delta".into(),
                    ts: chrono::Utc::now(),
                    message: None,
                    progress: Some(p),
                    code: None,
                    data: Some(json!({"delta": chunk, "stage": mode})),
                },
                Some(p),
                Some(format!("{mode}:delta")),
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
                        Some(json!({"stage": mode, "error": e})),
                    ),
                    None,
                    Some(format!("{mode}:llm_fail")),
                );
            }
            return None;
        }
    };

    if cancelled(jobs, run_id) {
        return None;
    }

    let parsed = crate::llm_stream::extract_json_value(&full)?;
    Some(normalize_background_llm_result(
        mode,
        title,
        character_name,
        parsed,
        &full,
    ))
}

fn background_prompts(
    mode: &str,
    title: &str,
    premise: &str,
    character_name: &str,
    world_book_context: &str,
    include_names: bool,
) -> (String, String, u32, u64) {
    let text = crate::llm_stream::limit_text(premise, 6000);
    match mode {
        "items" => {
            let system = "你是一个世界观与人物设定专家。根据参考文本提取结构化世界书与角色卡列表。务必返回严格纯 JSON，不要 Markdown 或额外说明。".to_string();
            let user = format!(
                "标题：{title}\n\
请返回 JSON，结构：\n\
{{\n  \"worldBooks\": [ {{\"name\": \"...\", \"fields\": {{ \"theme\": \"...\", \"era\": \"...\", \"techLevel\": \"...\", \"magicLevel\": \"...\", \"geography\": \"...\", \"keyScenes\": \"...\", \"culturalFeatures\": \"...\", \"history\": \"...\", \"conflict\": \"...\" }} }} ],\n  \"characterCards\": [ {{\"name\": \"角色名\", \"fields\": {{ \"age\": \"...\", \"gender\": \"...\", \"occupation\": \"...\", \"coreDesire\": \"...\", \"backgroundStory\": \"...\", \"speakingStyle\": \"...\" }} }} ]\n}}\n\
重要：仅返回纯 JSON。\n\n参考内容：\n===========================\n{text}\n==========================="
            );
            (system, user, 4096, 180)
        }
        "character_card" => {
            let system = "你是一个人物设定专家。为指定角色生成一张结构化角色卡。务必返回严格纯 JSON，不要 Markdown 或额外说明。".to_string();
            let ctx = if world_book_context.trim().is_empty() {
                String::new()
            } else {
                format!(
                    "\n\n已确认的世界书上下文：\n{}\n",
                    crate::llm_stream::limit_text(world_book_context, 2000)
                )
            };
            let user = format!(
                "请只为角色「{character_name}」生成一张角色卡，不要生成其他角色。{ctx}\n\
JSON 结构：\n\
{{\"name\":\"{character_name}\",\"fields\":{{\"age\":\"...\",\"gender\":\"...\",\"race\":\"...\",\"birthplace\":\"...\",\"occupation\":\"...\",\"socialClass\":\"...\",\"identityTags\":[\"...\"],\"heightBuild\":\"...\",\"iconicFeatures\":\"...\",\"clothingStyle\":\"...\",\"overallVibe\":\"...\",\"externalPersonality\":\"...\",\"internalPersonality\":\"...\",\"coreDesire\":\"...\",\"fearWeakness\":\"...\",\"moralValues\":\"...\",\"quirk\":\"...\",\"skills\":\"...\",\"backgroundStory\":\"...\",\"relationships\":\"...\",\"speakingStyle\":\"...\",\"typicalReactions\":\"...\",\"userRelationType\":\"...\",\"userInteractionModel\":\"...\",\"userRelationBottomLine\":\"...\",\"keyEvents\":\"...\"}}}}\n\
约束：文本未给出的字段请谨慎概括，不要明显冲突；仅返回纯 JSON。\n\n参考内容：\n===========================\n{text}\n==========================="
            );
            (system, user, 4096, 180)
        }
        _ => {
            // stage_one
            let system = "你是一个世界观与人物设定专家。根据用户提供的参考文本提取结构化世界书；如果任务要求，还要提取适合继续生成角色卡的角色姓名列表。务必返回严格纯 JSON，不要 Markdown 或额外说明。".to_string();
            let names_inst = if include_names {
                "\n同时提取需要生成角色卡的角色姓名列表，字段名为 characterNames。角色名只保留姓名或常用称呼，不要附带解释。"
            } else {
                "\n本次仅提取世界书，不要输出 characterNames，也不要输出角色卡。"
            };
            let schema = if include_names {
                r#"{
  "worldBooks": [ { "name": "世界书名", "fields": { "theme": "...", "era": "...", "techLevel": "...", "magicLevel": "...", "geography": "...", "keyScenes": "...", "culturalFeatures": "...", "history": "...", "conflict": "..." } } ],
  "characterNames": ["角色姓名1", "角色姓名2"]
}"#
            } else {
                r#"{
  "worldBooks": [ { "name": "世界书名", "fields": { "theme": "...", "era": "...", "techLevel": "...", "magicLevel": "...", "geography": "...", "keyScenes": "...", "culturalFeatures": "...", "history": "...", "conflict": "..." } } ]
}"#
            };
            let user = format!(
                "标题：{title}\n{names_inst}\nJSON 必须严格满足：\n{schema}\n\n重要：仅返回纯 JSON。\n\n参考内容：\n===========================\n{text}\n==========================="
            );
            (system, user, 3072, 150)
        }
    }
}

fn normalize_background_llm_result(
    mode: &str,
    title: &str,
    character_name: &str,
    parsed: Value,
    raw: &str,
) -> Value {
    // Accept either top-level structured fields or nested under data.
    let root = if parsed.get("worldBooks").is_some()
        || parsed.get("characterCards").is_some()
        || parsed.get("characterCard").is_some()
        || parsed.get("characterNames").is_some()
        || parsed.get("name").is_some()
    {
        parsed
    } else if let Some(d) = parsed.get("data").cloned() {
        d
    } else {
        parsed
    };

    match mode {
        "items" => {
            let world_books = root
                .get("worldBooks")
                .cloned()
                .unwrap_or_else(|| json!([]));
            let cards = root
                .get("characterCards")
                .cloned()
                .or_else(|| root.get("characters").cloned())
                .unwrap_or_else(|| json!([]));
            json!({
                "kind": "background",
                "stage": "items",
                "mode": "items",
                "title": title,
                "worldBooks": world_books,
                "characterCards": cards,
                "ok": true,
                "fallback": false,
                "generationMode": "llm",
                "raw": raw.chars().take(4000).collect::<String>(),
            })
        }
        "character_card" => {
            let card = if root.get("fields").is_some() || root.get("name").is_some() {
                // single card object
                if root.get("name").is_none() {
                    let mut o = root.as_object().cloned().unwrap_or_default();
                    o.insert("name".into(), json!(character_name));
                    Value::Object(o)
                } else {
                    root.clone()
                }
            } else {
                root.get("characterCard")
                    .cloned()
                    .unwrap_or(root.clone())
            };
            json!({
                "kind": "background",
                "stage": "character_card",
                "mode": "character_card",
                "title": title,
                "characterName": character_name,
                "characterCard": card,
                "ok": true,
                "fallback": false,
                "generationMode": "llm",
                "raw": raw.chars().take(4000).collect::<String>(),
            })
        }
        _ => {
            let world_books = root
                .get("worldBooks")
                .cloned()
                .unwrap_or_else(|| json!([]));
            let names = root
                .get("characterNames")
                .cloned()
                .unwrap_or_else(|| json!([]));
            json!({
                "kind": "background",
                "stage": "stage_one",
                "mode": "stage_one",
                "title": title,
                "worldBooks": world_books,
                "characterNames": names,
                "ok": true,
                "fallback": false,
                "generationMode": "llm",
                "raw": raw.chars().take(4000).collect::<String>(),
            })
        }
    }
}


fn cancelled(jobs: &kaleido_core::JobStore, run_id: &str) -> bool {
    jobs.get(run_id)
        .map(|j| is_terminal_job_status(&j.status))
        .unwrap_or(true)
}

/// Structured stage_one aligned with upstream BackgroundStageOneResponse.
fn template_stage_one(title: &str, premise: &str, include_names: bool) -> Value {
    let short = premise_brief(premise, 80);
    let mut names = Vec::new();
    if include_names {
        // Prefer explicit "角色名候选：A、B" / "角色：A、B、C" lists in premise.
        for marker in ["角色名候选：", "角色名候选:", "角色：", "角色:"] {
            if let Some(pos) = premise.find(marker) {
                let tail = &premise[pos + marker.len()..];
                let chunk = tail.split('\n').next().unwrap_or(tail);
                for part in chunk.split(|c: char| {
                    c == '、' || c == ',' || c == '，' || c == '/' || c == '|' || c == '；' || c == ';'
                }) {
                    let n = part
                        .trim()
                        .trim_matches(|c: char| c == '。' || c == '.' || c == '》' || c == '《')
                        .to_string();
                    // 2–8 CJK / word-ish tokens; skip long clauses
                    let cc = n.chars().count();
                    if (2..=8).contains(&cc)
                        && n.chars().any(|c| c.is_alphanumeric())
                        && !names.iter().any(|x| x == &n)
                    {
                        names.push(n);
                    }
                }
                if !names.is_empty() {
                    break;
                }
            }
        }
        if names.is_empty() {
            for candidate in ["主角", "伙伴", "导师"] {
                if premise.contains(candidate) {
                    names.push(candidate.to_string());
                }
            }
        }
        if names.is_empty() {
            names = vec!["林晚星".into(), "顾沉舟".into()];
        }
    }
    json!({
        "worldBooks": [{
            "name": format!("{title} · 世界设定集"),
            "fields": {
                "theme": if short.is_empty() { "原创奇幻".into() } else { short.clone() },
                "era": "待定时代",
                "techLevel": "与前提一致",
                "magicLevel": "与前提一致",
                "geography": format!("由前提推导：{short}"),
                "keyScenes": "开场场景 / 冲突爆发点 / 终局舞台",
                "culturalFeatures": "待深化（items / 角色卡阶段可补）",
                "history": "关键转折待填",
                "conflict": format!("核心矛盾围绕：{short}")
            }
        }],
        "characterNames": names,
    })
}

/// Full extract (worldBooks + characterCards) for items stage.
fn template_items(title: &str, premise: &str) -> Value {
    let stage = template_stage_one(title, premise, true);
    let names = stage
        .get("characterNames")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut cards = Vec::new();
    for n in names.iter().take(3) {
        let name = n.as_str().unwrap_or("未命名").to_string();
        cards.push(template_character_card(&name, premise, ""));
    }
    if cards.is_empty() {
        cards.push(template_character_card("林晚星", premise, ""));
    }
    json!({
        "worldBooks": stage.get("worldBooks").cloned().unwrap_or(json!([])),
        "characterCards": cards,
    })
}



/// Short human premise for templates: drop trailing "角色名候选/角色：" lists, collapse whitespace.
fn premise_brief(premise: &str, max_chars: usize) -> String {
    let mut s = premise.trim().to_string();
    for marker in ["角色名候选：", "角色名候选:", "角色：", "角色:"] {
        if let Some(pos) = s.find(marker) {
            s = s[..pos].trim_end().trim_end_matches('。').trim_end_matches('.').to_string();
            break;
        }
    }
    let collapsed: String = s
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    collapsed.chars().take(max_chars).collect()
}

/// Plain-text world-book hint for character deepen (never raw JSON — that leaked into coreDesire).
fn world_book_context_summary(world_books: &Value, premise: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(arr) = world_books.as_array() {
        for wb in arr.iter().take(2) {
            if let Some(n) = wb.get("name").and_then(|v| v.as_str()) {
                let n = n.trim();
                if !n.is_empty() {
                    parts.push(format!("世界书：{n}"));
                }
            }
            if let Some(fields) = wb.get("fields").and_then(|v| v.as_object()) {
                // Prefer compact theme; skip conflict/geography when they restate premise.
                if let Some(s) = fields.get("theme").and_then(|v| v.as_str()) {
                    let s = s.trim();
                    if !s.is_empty() {
                        parts.push(format!("主题：{s}"));
                    }
                }
                if let Some(s) = fields.get("era").and_then(|v| v.as_str()) {
                    let s = s.trim();
                    if !s.is_empty() && s != "待定时代" {
                        parts.push(format!("时代：{s}"));
                    }
                }
            } else if let Some(c) = wb.get("content").and_then(|v| v.as_str()) {
                let s: String = c.chars().take(120).collect();
                if !s.trim().is_empty() {
                    parts.push(s);
                }
            }
        }
    }
    if parts.is_empty() {
        premise_brief(premise, 120)
    } else {
        let joined = parts.join("；");
        joined.chars().take(240).collect()
    }
}

/// Single character card (upstream GeneratedBackgroundItem shape).
fn template_character_card(name: &str, premise: &str, world_book_context: &str) -> Value {
    let hint = if world_book_context.trim().is_empty() {
        premise_brief(premise, 80)
    } else {
        // Already a plain summary from world_book_context_summary; still cap.
        world_book_context.chars().take(120).collect::<String>()
    };
    json!({
        "name": name,
        "fields": {
            "age": "未知",
            "gender": "未知",
            "race": "人类",
            "birthplace": "与世界书一致",
            "occupation": "待定",
            "socialClass": "待定",
            "identityTags": ["核心角色", "可互动"],
            "heightBuild": "中等",
            "iconicFeatures": format!("{name} 的标志性特征（模板）"),
            "clothingStyle": "贴合时代与身份",
            "overallVibe": "鲜明且可辨识",
            "externalPersonality": "外在表现克制",
            "internalPersonality": "内在层次更丰富",
            "coreDesire": format!("与「{hint}」相关的核心驱动力"),
            "fearWeakness": "与核心欲望相对的软肋",
            "moralValues": "有底线、可冲突",
            "quirk": "习惯性小动作",
            "skills": "与设定自洽的专长",
            "backgroundStory": format!("{name} 的身世：由前提「{hint}」概括生成（模板，可后续 LLM 深化）"),
            "relationships": "与主角/用户的关系网待补",
            "speakingStyle": "口语化、有口头禅",
            "typicalReactions": "压力下的典型反应",
            "userRelationType": "可成长关系",
            "userInteractionModel": "先疏后密 / 可被改写",
            "userRelationBottomLine": "不无底线越界",
            "keyEvents": "与用户的关键里程碑待展开"
        }
    })
}

// ---------------------------------------------------------------------------
// Apply background result → Partner store
// ---------------------------------------------------------------------------

/// Request body for `POST /api/v1/background/apply`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundApplyBody {
    /// Load job.result when `result` is omitted.
    #[serde(default, alias = "run_id", alias = "jobId", alias = "id")]
    pub run_id: Option<String>,
    /// Explicit BG payload: worldBooks / characterCards / characterCard.
    #[serde(default)]
    pub result: Option<Value>,
    /// Select first applied card (or world book) after upsert. Default true.
    #[serde(default)]
    pub select: Option<bool>,
    /// Optional name prefix for applied items.
    #[serde(default)]
    pub prefix: Option<String>,
}

fn short_hash(s: &str) -> String {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    let full = format!("{:x}", h.finish());
    full.chars().take(8).collect()
}

fn slugify(name: &str) -> String {
    // Keep Unicode letters/numbers (incl. CJK). ASCII alnum → lower; runs of other → '-'.
    // Old is_ascii_alphanumeric path turned 中文 into "___" (looks like mojibake in IDs).
    let mut out = String::new();
    let mut prev_dash = false;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            prev_dash = false;
        } else if c.is_alphanumeric() {
            // CJK / other scripts
            out.push(c);
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    let s: String = out.trim_matches('-').chars().take(32).collect();
    if s.is_empty() {
        "item".into()
    } else {
        s
    }
}

fn fields_to_content(name: &str, item_type: &str, fields: &Value) -> String {
    if fields.is_null() {
        return name.to_string();
    }
    if let Some(obj) = fields.as_object() {
        if obj.is_empty() {
            return name.to_string();
        }
        // Prefer markdown compile when fields look structured.
        let md = kaleido_core::compile_partner_markdown(name, item_type, fields);
        if md.trim().len() > name.len() + 2 {
            return md;
        }
        let mut parts = Vec::new();
        for (k, v) in obj {
            let vs = match v {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                Value::Bool(b) => b.to_string(),
                Value::Array(a) => a
                    .iter()
                    .filter_map(|x| x.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                other => other.to_string(),
            };
            if !vs.trim().is_empty() {
                parts.push(format!("{k}: {vs}"));
            }
        }
        if parts.is_empty() {
            name.to_string()
        } else {
            format!("{name}\n{}", parts.join("\n"))
        }
    } else {
        fields.to_string()
    }
}

fn entry_name(entry: &Value, fallback: &str) -> String {
    entry
        .get("name")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| fallback.to_string())
}

fn entry_fields(entry: &Value) -> Option<Value> {
    entry.get("fields").cloned().or_else(|| {
        // Treat whole object minus name/id/type as fields if it has other keys.
        if let Some(obj) = entry.as_object() {
            let mut m = Map::new();
            for (k, v) in obj {
                if matches!(
                    k.as_str(),
                    "name" | "id" | "type" | "itemType" | "content" | "fields" | "worldBookId" | "world_book_id"
                ) {
                    continue;
                }
                m.insert(k.clone(), v.clone());
            }
            if m.is_empty() {
                None
            } else {
                Some(Value::Object(m))
            }
        } else {
            None
        }
    })
}

fn stable_partner_id(prefix_tag: &str, name: &str, salt: &str) -> String {
    let slug = slugify(name);
    let h = short_hash(&format!("{prefix_tag}|{name}|{salt}"));
    format!("{prefix_tag}-{slug}-{h}")
}

fn map_world_book(entry: &Value, name_prefix: &str, salt: &str) -> PartnerItem {
    let raw_name = entry_name(entry, "世界书");
    let name = if name_prefix.is_empty() {
        raw_name.clone()
    } else {
        format!("{name_prefix}{raw_name}")
    };
    let fields = entry_fields(entry);
    let content = if let Some(ref f) = fields {
        fields_to_content(&name, "world_book", f)
    } else if let Some(c) = entry.get("content").and_then(|v| v.as_str()) {
        c.to_string()
    } else {
        name.clone()
    };
    let id = entry
        .get("id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| stable_partner_id("wb", &raw_name, salt));
    PartnerItem {
        id,
        name,
        item_type: "world_book".into(),
        content,
        fields,
        world_book_id: None,
    }
}

fn map_character_card(
    entry: &Value,
    name_prefix: &str,
    salt: &str,
    default_wb: Option<&str>,
) -> PartnerItem {
    let raw_name = entry_name(entry, "角色卡");
    let name = if name_prefix.is_empty() {
        raw_name.clone()
    } else {
        format!("{name_prefix}{raw_name}")
    };
    let fields = entry_fields(entry);
    let content = if let Some(ref f) = fields {
        fields_to_content(&name, "character_card", f)
    } else if let Some(c) = entry.get("content").and_then(|v| v.as_str()) {
        c.to_string()
    } else {
        name.clone()
    };
    let id = entry
        .get("id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| stable_partner_id("cc", &raw_name, salt));
    let world_book_id = entry
        .get("worldBookId")
        .or_else(|| entry.get("world_book_id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| default_wb.map(|s| s.to_string()));
    PartnerItem {
        id,
        name,
        item_type: "character_card".into(),
        content,
        fields,
        world_book_id,
    }
}

/// `POST /api/v1/background/apply` — upsert BG world books / character cards into Partner.
pub async fn apply(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<BackgroundApplyBody>,
) -> axum::response::Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    // C2 审计修复：per-user 隔离，防跨租户写其他用户 partner store。
    let partner = state.partner.clone().scoped(&session.user_id);

    let result = if let Some(r) = body.result.clone() {
        r
    } else if let Some(run_id) = body
        .run_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        match state.jobs.get(run_id) {
            None => {
                return not_found("BG_NOT_FOUND", format!("job not found: {run_id}"));
            }
            Some(j)
                if j.workspace_id != session.workspace_id && j.user_id != session.user_id =>
            {
                return forbidden("BG_FORBIDDEN_SCOPE", "job belongs to another workspace");
            }
            Some(j) => match j.result {
                Some(r) => r,
                None => {
                    return err_with_code(
            StatusCode::BAD_REQUEST,
            "BG_BAD_REQUEST", "job has no result yet",
            serde_json::json!({"runId": run_id,
                            "status": normalize_job_status(&j.status)}),
                    );
                }
            },
        }
    } else {
        return bad_request("BG_MISSING_FIELD", "result or runId is required");
    };

    let prefix = body.prefix.as_deref().unwrap_or("").to_string();
    let salt = body
        .run_id
        .clone()
        .unwrap_or_else(|| Uuid::new_v4().to_string());

    let mut world_books_in: Vec<Value> = Vec::new();
    if let Some(arr) = result.get("worldBooks").and_then(|v| v.as_array()) {
        world_books_in.extend(arr.iter().cloned());
    } else if let Some(one) = result.get("worldBook") {
        world_books_in.push(one.clone());
    }

    let mut cards_in: Vec<Value> = Vec::new();
    if let Some(arr) = result.get("characterCards").and_then(|v| v.as_array()) {
        cards_in.extend(arr.iter().cloned());
    }
    if let Some(one) = result.get("characterCard") {
        cards_in.push(one.clone());
    }

    if world_books_in.is_empty() && cards_in.is_empty() {
        return err_with_code(
            StatusCode::BAD_REQUEST,
            "BG_BAD_REQUEST", "result has no worldBooks / characterCards / characterCard",
            serde_json::json!({"ok": false}),
        );
    }

    let mut applied_wb: Vec<PartnerItem> = Vec::new();
    for entry in &world_books_in {
        let item = map_world_book(entry, &prefix, &salt);
        match partner.upsert_world_book(item) {
            Ok(saved) => applied_wb.push(saved),
            Err(e) => return map_core_err(e),
        }
    }

    let default_wb_id = if applied_wb.len() == 1 {
        Some(applied_wb[0].id.clone())
    } else {
        None
    };

    let mut applied_cc: Vec<PartnerItem> = Vec::new();
    for entry in &cards_in {
        let item = map_character_card(entry, &prefix, &salt, default_wb_id.as_deref());
        match partner.upsert_character_card(item) {
            Ok(saved) => applied_cc.push(saved),
            Err(e) => return map_core_err(e),
        }
    }

    let do_select = body.select.unwrap_or(true);
    let mut selected_wb: Option<String> = None;
    let mut selected_cc: Option<String> = None;
    if do_select {
        if let Some(first_cc) = applied_cc.first() {
            selected_cc = Some(first_cc.id.clone());
            selected_wb = first_cc
                .world_book_id
                .clone()
                .or_else(|| applied_wb.first().map(|w| w.id.clone()));
        } else if let Some(first_wb) = applied_wb.first() {
            selected_wb = Some(first_wb.id.clone());
        }
        if selected_wb.is_some() || selected_cc.is_some() {
            if let Err(e) = partner
                .select(selected_wb.clone(), selected_cc.clone())
            {
                return map_core_err(e);
            }
        }
    }

    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "schemaVersion": 1,
            "kind": "background_apply",
            "worldBooks": applied_wb,
            "characterCards": applied_cc,
            "counts": {
                "worldBooks": applied_wb.len(),
                "characterCards": applied_cc.len(),
            },
            "selected": {
                "worldBookId": selected_wb,
                "characterCardId": selected_cc,
            },
            "runId": body.run_id,
            "workspaceId": session.workspace_id,
        })),
    )
        .into_response()
}

