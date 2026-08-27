//! moa_api: MoA 对比面板 HTTP 接线（T8）
//! hermes-fake-moa 完整落地：panel 管理 + 同一 prompt 并发派发多个模型 + 结果收集。
//! 复用 kaleido-core moa_comparison 纯逻辑；LLM 调用复用 resolve_llm 单一网关配置
//! （endpoint.model 覆盖模型名，base_url/api_key 统一走现有 LLM 配置）。

use std::collections::HashMap;
use std::sync::OnceLock;

use parking_lot::Mutex;
use std::time::{Duration as StdDuration, Instant};

use axum::{
    extract::{Path, State},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use kaleido_core::moa_comparison::{
    ComparisonPanel, ComparisonSession, ModelEndpoint, ModelResponse, SessionStatus,
};

use crate::AppState;
use crate::error_codes::*;

// ---------- 持久化存储（JSON 落盘，重启不丢）----------

#[derive(Default, Serialize, Deserialize)]
struct MoaStore {
    panels: HashMap<String, ComparisonPanel>,
    sessions: HashMap<String, ComparisonSession>,
}

fn store_path() -> String {
    let root = crate::config::ServerConfig::data_root();
    kaleido_core::brand_dir(&root, "config")
        .join("moa-store.json")
        .display()
        .to_string()
}

fn load_store() -> MoaStore {
    std::fs::read_to_string(store_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_store() {
    // parking_lot::Mutex 无中毒概念，lock() 直接返回 guard。
    let s = store().lock();
    if let Ok(content) = serde_json::to_string_pretty(&*s) {
        if let Some(dir) = std::path::Path::new(&store_path()).parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(store_path(), content);
    }
}

fn store() -> &'static Mutex<MoaStore> {
    static MOA_STORE: OnceLock<Mutex<MoaStore>> = OnceLock::new();
    MOA_STORE.get_or_init(|| Mutex::new(load_store()))
}

// ---------- 共享 LLM 调用（非流式）----------

async fn call_llm(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    model: &str,
    prompt: &str,
    max_tokens: u32,
) -> Result<String, String> {
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let body = json!({
        "model": model,
        "stream": false,
        "messages": [
            {"role": "system", "content": "You are a professional creative writing analyst. Respond in the same language as the user prompt."},
            {"role": "user", "content": prompt},
        ],
        "max_tokens": max_tokens,
    });
    let resp = client
        .post(&url)
        .bearer_auth(api_key)
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("upstream connect: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!(
            "upstream {status}: {}",
            text.chars().take(300).collect::<String>()
        ));
    }
    let v: Value = resp
        .json()
        .await
        .map_err(|e| format!("bad json: {e}"))?;
    let content = v
        .pointer("/choices/0/message/content")
        .and_then(|c| c.as_str())
        .unwrap_or_default()
        .to_string();
    if content.trim().is_empty() {
        return Err("empty response".to_string());
    }
    Ok(content)
}

// ---------- 路由：panel ----------

/// POST /api/v1/moa/panels  {"name": "...", "endpoints": [{"id","provider","model","label"}...]}
async fn panels_create(body: Json<Value>) -> Response {
    let name = match body.get("name").and_then(|v| v.as_str()) {
        Some(n) if !n.trim().is_empty() => n.trim().to_string(),
        _ => {
            return bad_request("MOA_MISSING_FIELD", "name required");
        }
    };
    let mut panel = ComparisonPanel::new(&format!("panel-{}", uuid_short()), &name);
    if let Some(eps) = body.get("endpoints").and_then(|v| v.as_array()) {
        for ep in eps {
            let ep = ModelEndpoint {
                id: ep
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                provider: ep
                    .get("provider")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                model: ep
                    .get("model")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                label: ep
                    .get("label")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
            };
            if ep.id.is_empty() || ep.model.is_empty() {
                return bad_request("MOA_BAD_REQUEST", "endpoint needs id+model");
            }
            if let Err(e) = panel.add(ep) {
                return bad_request("MOA_BAD_REQUEST", e);
            }
        }
    }
    if !panel.is_valid() {
        return bad_request("MOA_BAD_REQUEST", "panel needs 2-5 endpoints");
    }
    let id = panel.id.clone();
    let mut s = store().lock();
    s.panels.insert(id.clone(), panel);
    drop(s);
    save_store();
    Json(json!({
        "panel_id": id,
        "name": name,
        "endpoints": s_endpoint_list(&id),
    }))
    .into_response()
}

/// GET /api/v1/moa/panels
async fn panels_list() -> Response {
    let s = store().lock();
    let list: Vec<Value> = s
        .panels
        .values()
        .map(|p| json!({
            "panel_id": p.id,
            "name": p.name,
            "endpoint_count": p.endpoints.len(),
            "endpoints": p.endpoints.iter().map(|e| json!({
                "id": e.id, "provider": e.provider, "model": e.model, "label": e.label,
            })).collect::<Vec<_>>(),
        }))
        .collect();
    Json(json!({"panels": list})).into_response()
}

/// DELETE /api/v1/moa/panels/{panel_id}
async fn panels_delete(Path(panel_id): Path<String>) -> Response {
    {
        let mut s = store().lock();
        s.panels.remove(&panel_id);
    }
    save_store();
    Json(json!({"deleted": panel_id})).into_response()
}

fn s_endpoint_list(panel_id: &str) -> Vec<Value> {
    let s = store().lock();
    match s.panels.get(panel_id) {
        Some(p) => p
            .endpoints
            .iter()
            .map(|e| json!({"id": e.id, "provider": e.provider, "model": e.model, "label": e.label}))
            .collect(),
        None => Vec::new(),
    }
}

// ---------- 路由：run + session ----------

/// POST /api/v1/moa/run  {"panel_id": "...", "prompt": "...", "max_tokens": 2048, "aggregate": false, "aggregator_model": "..."}
/// 立即返回 session_id (status=running)，后台并发调所有 endpoint 模型。
/// aggregate=true 时：并排结果收集完后，再用聚合器 LLM 综合所有成功答案
/// 产出单一最终答案（真 MoA aggregator pass），写入 session.aggregated。
async fn moa_run(State(state): State<AppState>, body: Json<Value>) -> Response {
    let panel_id = match body.get("panel_id").and_then(|v| v.as_str()) {
        Some(p) => p.to_string(),
        None => {
            return bad_request("MOA_MISSING_FIELD", "panel_id required");
        }
    };
    let prompt = match body.get("prompt").and_then(|v| v.as_str()) {
        Some(p) if !p.trim().is_empty() => p.to_string(),
        _ => {
            return bad_request("MOA_MISSING_FIELD", "prompt required");
        }
    };
    let max_tokens = body
        .get("max_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(2048)
        .min(8192) as u32;
    let aggregate = body
        .get("aggregate")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let aggregator_model = body
        .get("aggregator_model")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // 取出 panel 的 endpoint 快照
    let endpoints: Vec<ModelEndpoint> = {
        let s = store().lock();
        match s.panels.get(&panel_id) {
            Some(p) => p.endpoints.clone(),
            None => {
                return not_found("MOA_NOT_FOUND", format!("panel {panel_id} not found"));
            }
        }
    };

    // resolve LLM 单一网关配置
    let rt = state.app_state.resolve_llm(
        state.llm_base.as_deref(),
        state.llm_key.as_deref(),
        &state.llm_model,
    );
    if rt.base_url.trim().is_empty() || rt.api_key.trim().is_empty() {
        return service_unavailable("MOA_NOT_CONFIGURED", "LLM not configured (LLM_BASE_URL/LLM_API_KEY)");
    }

    let session_id = format!("moa-{}", uuid_short());
    let mut session = ComparisonSession::new(&session_id, &panel_id, &prompt);
    session.status = SessionStatus::Running;
    {
        let mut s = store().lock();
        s.sessions.insert(session_id.clone(), session.clone());
    }
    save_store();

    let client = match reqwest::Client::builder()
        .timeout(StdDuration::from_secs(300))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            let mut s = store().lock();
            if let Some(ss) = s.sessions.get_mut(&session_id) {
                ss.status = SessionStatus::Failed;
            }
            drop(s);
            save_store();
            return internal("MOA_INTERNAL", format!("client build: {e}"));
        }
    };

    let base_url = rt.base_url;
    let api_key = rt.api_key;
    let default_model = rt.model;

    // 后台并发跑
    let endpoint_count = endpoints.len();
    let session_id_bg = session_id.clone();
    let do_aggregate = aggregate;
    let agg_model = aggregator_model.unwrap_or(default_model.clone());
    let agg_model_bg = agg_model.clone();
    tokio::spawn(async move {
        let mut tasks = Vec::new();
        for ep in endpoints {
            let client = client.clone();
            let base_url = base_url.clone();
            let api_key = api_key.clone();
            let prompt = prompt.clone();
            let ep = ep.clone();
            tasks.push(tokio::spawn(async move {
                let start = Instant::now();
                let res = call_llm(&client, &base_url, &api_key, &ep.model, &prompt, max_tokens).await;
                ModelResponse {
                    endpoint_id: ep.id,
                    raw_text: res.clone().unwrap_or_default(),
                    elapsed_ms: start.elapsed().as_millis() as u64,
                    error: res.err(),
                }
            }));
        }
        let mut results = Vec::new();
        for t in tasks {
            if let Ok(r) = t.await {
                results.push(r);
            }
        }
        let ok_count = results.iter().filter(|r| r.error.is_none()).count();
        {
            let mut s = store().lock();
            if let Some(ss) = s.sessions.get_mut(&session_id_bg) {
                for r in results {
                    let _ = ss.add_result(r);
                }
                ss.status = if ss.results.iter().any(|r| r.error.is_some()) {
                    SessionStatus::Failed
                } else {
                    SessionStatus::Complete
                };
            }
        }
        save_store();

        // 真聚合 pass：并排结果齐后，用聚合器 LLM 综合所有成功答案
        if do_aggregate && ok_count >= 1 {
            let agg_prompt = {
                let s = store().lock();
                s.sessions
                    .get(&session_id_bg)
                    .map(|ss| ss.build_aggregate_prompt())
                    .unwrap_or_default()
            };
            let start = Instant::now();
            let agg_res = call_llm(
                &client,
                &base_url,
                &api_key,
                &agg_model_bg,
                &agg_prompt,
                max_tokens.max(2048),
            )
            .await;
            let elapsed = start.elapsed().as_millis() as u64;
            {
                let mut s = store().lock();
                if let Some(ss) = s.sessions.get_mut(&session_id_bg) {
                    match agg_res {
                        Ok(text) => {
                            ss.aggregated = Some(text);
                            ss.aggregate_elapsed_ms = Some(elapsed);
                        }
                        Err(e) => {
                            ss.aggregate_error = Some(format!("{e}"));
                        }
                    }
                }
            }
            save_store();
        }
    });

    Json(json!({
        "session_id": session_id,
        "panel_id": panel_id,
        "status": "running",
        "endpoint_count": endpoint_count,
        "aggregate": do_aggregate,
        "aggregator_model": agg_model,
    }))
    .into_response()
}

/// GET /api/v1/moa/sessions/{session_id}
async fn session_get(Path(session_id): Path<String>) -> Response {
    let s = store().lock();
    match s.sessions.get(&session_id) {
        Some(ss) => {
            let status = match ss.status {
                SessionStatus::Pending => "pending",
                SessionStatus::Running => "running",
                SessionStatus::Complete => "complete",
                SessionStatus::Failed => "failed",
            };
            Json(json!({
                "session_id": ss.id,
                "panel_id": ss.panel_id,
                "prompt": ss.prompt,
                "status": status,
                "results": ss.results.iter().map(|r| json!({
                    "endpoint_id": r.endpoint_id,
                    "raw_text": r.raw_text,
                    "elapsed_ms": r.elapsed_ms,
                    "error": r.error,
                })).collect::<Vec<_>>(),
                "summary": ss.build_summary(),
                "aggregated": ss.aggregated,
                "aggregate_elapsed_ms": ss.aggregate_elapsed_ms,
                "aggregate_error": ss.aggregate_error,
            }))
            .into_response()
        }
        None => return not_found("MOA_NOT_FOUND", format!("session {session_id} not found")),
    }
}

/// GET /api/v1/moa/sessions
async fn sessions_list() -> Response {
    let s = store().lock();
    let list: Vec<Value> = s
        .sessions
        .values()
        .map(|ss| {
            let status = match ss.status {
                SessionStatus::Pending => "pending",
                SessionStatus::Running => "running",
                SessionStatus::Complete => "complete",
                SessionStatus::Failed => "failed",
            };
            json!({
                "session_id": ss.id,
                "panel_id": ss.panel_id,
                "status": status,
                "result_count": ss.results.len(),
                "created_prompt": ss.prompt.chars().take(60).collect::<String>(),
            })
        })
        .collect();
    Json(json!({"sessions": list})).into_response()
}

fn uuid_short() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{nanos:x}")
}

// ---------- Router 片段 ----------

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/moa/panels", post(panels_create).get(panels_list))
        .route("/api/v1/moa/panels/{panel_id}", delete(panels_delete))
        .route("/api/v1/moa/run", post(moa_run))
        .route("/api/v1/moa/sessions", get(sessions_list))
        .route("/api/v1/moa/sessions/{session_id}", get(session_get))
}
