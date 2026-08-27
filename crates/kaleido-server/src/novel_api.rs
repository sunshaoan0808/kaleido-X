//! novel_api: HTTP 接线 — novel_workflow 四模块（T7）
//! PlanningGate / RevisionPhases / KnowledgeState / ForeshadowLedger
//! 全内存存储（进程重启即失），axum handler 风格与 main.rs 一致。
//!
//! M-4 (workspace isolation): every handler now extracts the authenticated
//! session via `session_from` and namespaces store keys as
//! `{user_id}:{resource_id}`. This prevents any authenticated user from
//! reading or writing another user's planning/revision/knowledge/foreshadow
//! data by guessing a gate_id / ledger_id. The public path parameters are
//! unchanged (still `gate_id` / `ledger_id`); only the internal storage key
//! is composed with the caller's user_id.

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::OnceLock;

use parking_lot::Mutex;

use kaleido_core::novel_workflow::{
    CharacterKnowledge, ForeshadowLedger, KnowledgeLedger, PlanningGate, PlanningStage,
    RevisionGate, SceneKnowledge,
};

use crate::{session_from, AppState};
use crate::error_codes::*;

// ---------- 内存存储 ----------

struct NovelStore {
    planning: Mutex<HashMap<String, PlanningGate>>,
    revision: Mutex<HashMap<String, RevisionGate>>,
    knowledge: Mutex<HashMap<String, KnowledgeLedger>>,
    foreshadow: Mutex<HashMap<String, ForeshadowLedger>>,
}

impl NovelStore {
    fn new() -> Self {
        Self {
            planning: Mutex::new(HashMap::new()),
            revision: Mutex::new(HashMap::new()),
            knowledge: Mutex::new(HashMap::new()),
            foreshadow: Mutex::new(HashMap::new()),
        }
    }
}

static STORE: OnceLock<NovelStore> = OnceLock::new();

fn store() -> &'static NovelStore {
    STORE.get_or_init(NovelStore::new)
}

/// M-4: compose a per-user scoped storage key so that resources with the same
/// `gate_id` / `ledger_id` belonging to different users are isolated.
fn scoped_key(user_id: &str, resource_id: &str) -> String {
    format!("{user_id}:{resource_id}")
}

// ---------- 工具 ----------

fn parse_body(body: String) -> Result<Value, Response> {
    match serde_json::from_str(&body) {
        Ok(v) => Ok(v),
        Err(e) => Err(bad_request("NOVEL_INVALID", format!("Invalid JSON: {e}"))),
    }
}

fn get_str(v: &Value, key: &str) -> Result<String, Response> {
    match v.get(key).and_then(|x| x.as_str()) {
        Some(s) => Ok(s.to_string()),
        None => Err(bad_request("NOVEL_BAD_REQUEST", format!("Missing string field: {key}"))),
    }
}

fn get_str_opt(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|i| i.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

// ============================================================
// 1. PlanningGate 企划门
// ============================================================

pub async fn planning_new(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let sess = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let v = match parse_body(body) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let gate_id = match get_str(&v, "gate_id") {
        Ok(s) => s,
        Err(r) => return r,
    };
    let key = scoped_key(&sess.user_id, &gate_id);
    let gate = PlanningGate::new();
    store().planning.lock().insert(key, gate);
    Json(json!({
        "gate_id": gate_id,
        "stage": "Worldbuilding",
        "can_start_writing": false,
        "message": "企划门已创建：世界观 → 角色 → 剧情",
    }))
    .into_response()
}

pub async fn planning_advance(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let sess = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let v = match parse_body(body) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let gate_id = match get_str(&v, "gate_id") {
        Ok(s) => s,
        Err(r) => return r,
    };
    let stage_str = match get_str(&v, "stage") {
        Ok(s) => s,
        Err(r) => return r,
    };
    let stage = match stage_str.as_str() {
        "Worldbuilding" => PlanningStage::Worldbuilding,
        "Character" => PlanningStage::Character,
        "Plot" => PlanningStage::Plot,
        _ => {
            return bad_request("NOVEL_BAD_REQUEST", format!("Unknown stage: {stage_str}"));
        }
    };
    let key = scoped_key(&sess.user_id, &gate_id);
    let mut gates = store().planning.lock();
    let gate = match gates.get_mut(&key) {
        Some(g) => g,
        None => {
            return not_found("NOVEL_NOT_FOUND", format!("PlanningGate {gate_id} not found"));
        }
    };
    match gate.advance(stage) {
        Ok(()) => Json(json!({
            "gate_id": gate_id,
            "can_start_writing": gate.can_start_writing(),
        }))
        .into_response(),
        Err(e) => err_with_code(
            StatusCode::CONFLICT,
            "NOVEL_GATE_CONFLICT",
            e,
            serde_json::json!({ "can_start_writing": gate.can_start_writing() }),
        ),
    }
}

pub async fn planning_get(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(gate_id): Path<String>,
) -> Response {
    let sess = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let key = scoped_key(&sess.user_id, &gate_id);
    let gates = store().planning.lock();
    match gates.get(&key) {
        Some(g) => Json(json!({
            "gate_id": gate_id,
            "can_start_writing": g.can_start_writing(),
        }))
        .into_response(),
        None => return not_found("NOVEL_NOT_FOUND", format!("PlanningGate {gate_id} not found")),
    }
}

// ============================================================
// 2. RevisionPhases 推敲门
// ============================================================

pub async fn revision_new(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let sess = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let v = match parse_body(body) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let gate_id = match get_str(&v, "gate_id") {
        Ok(s) => s,
        Err(r) => return r,
    };
    let mut gate = RevisionGate::new();
    gate.mount_default_checks();
    let checks: Vec<_> = gate
        .checks
        .iter()
        .map(|c| json!({"id": c.id, "name": c.name, "passed": c.passed}))
        .collect();
    let key = scoped_key(&sess.user_id, &gate_id);
    store().revision.lock().insert(key, gate);
    Json(json!({
        "gate_id": gate_id,
        "phase": "plot_verification",
        "checks": checks,
        "phase_gate": false,
    }))
    .into_response()
}

pub async fn revision_check(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let sess = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let v = match parse_body(body) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let gate_id = match get_str(&v, "gate_id") {
        Ok(s) => s,
        Err(r) => return r,
    };
    let check_id = match get_str(&v, "check_id") {
        Ok(s) => s,
        Err(r) => return r,
    };
    let passed = v.get("passed").and_then(|x| x.as_bool()).unwrap_or(false);
    let key = scoped_key(&sess.user_id, &gate_id);
    let mut gates = store().revision.lock();
    let gate = match gates.get_mut(&key) {
        Some(g) => g,
        None => {
            return not_found("NOVEL_NOT_FOUND", format!("RevisionGate {gate_id} not found"));
        }
    };
    match gate.checks.iter_mut().find(|c| c.id == check_id) {
        Some(c) => {
            c.passed = passed;
            Json(json!({
                "gate_id": gate_id,
                "check_id": check_id,
                "passed": passed,
                "phase_gate": gate.phase_gate(),
            }))
            .into_response()
        }
        None => return not_found("NOVEL_NOT_FOUND", format!("Check {check_id} not found")),
    }
}

pub async fn revision_next(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let sess = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let v = match parse_body(body) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let gate_id = match get_str(&v, "gate_id") {
        Ok(s) => s,
        Err(r) => return r,
    };
    let key = scoped_key(&sess.user_id, &gate_id);
    let mut gates = store().revision.lock();
    let gate = match gates.get_mut(&key) {
        Some(g) => g,
        None => {
            return not_found("NOVEL_NOT_FOUND", format!("RevisionGate {gate_id} not found"));
        }
    };
    match gate.next_phase() {
        Ok(()) => Json(json!({
            "gate_id": gate_id,
            "phase": gate.phase.name(),
            "phase_gate": gate.phase_gate(),
        }))
        .into_response(),
        Err(e) => return conflict("NOVEL_CONFLICT", e),
    }
}

pub async fn revision_get(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(gate_id): Path<String>,
) -> Response {
    let sess = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let key = scoped_key(&sess.user_id, &gate_id);
    let gates = store().revision.lock();
    match gates.get(&key) {
        Some(g) => Json(json!({
            "gate_id": gate_id,
            "phase": g.phase.name(),
            "phase_gate": g.phase_gate(),
            "checks": g.checks.iter().map(|c| json!({
                "id": c.id, "name": c.name, "passed": c.passed
            })).collect::<Vec<_>>(),
        }))
        .into_response(),
        None => return not_found("NOVEL_NOT_FOUND", format!("RevisionGate {gate_id} not found")),
    }
}

// ============================================================
// 3. KnowledgeState 知识状态
// ============================================================

pub async fn knowledge_scene(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let sess = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let v = match parse_body(body) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let ledger_id = match get_str(&v, "ledger_id") {
        Ok(s) => s,
        Err(r) => return r,
    };
    let scene = match get_str(&v, "scene") {
        Ok(s) => s,
        Err(r) => return r,
    };
    let character = match get_str(&v, "character") {
        Ok(s) => s,
        Err(r) => return r,
    };
    let knows = get_str_opt(&v, "knows");
    let not_knows = get_str_opt(&v, "not_knows");

    let key = scoped_key(&sess.user_id, &ledger_id);
    let mut ledgers = store().knowledge.lock();
    let ledger = ledgers
        .entry(key)
        .or_insert_with(KnowledgeLedger::new);

    // 更新/登记该场景该角色的知识
    let mut found = false;
    for sc in ledger.scenes.iter_mut() {
        if sc.scene == scene {
            for ck in sc.characters.iter_mut() {
                if ck.character == character {
                    ck.knows = knows.clone();
                    ck.not_knows = not_knows.clone();
                    found = true;
                }
            }
        }
    }
    if !found {
        let target_scene = ledger
            .scenes
            .iter_mut()
            .find(|s| s.scene == scene);
        match target_scene {
            Some(sc) => sc.characters.push(CharacterKnowledge {
                character: character.clone(),
                knows: knows.clone(),
                not_knows: not_knows.clone(),
            }),
            None => ledger.scenes.push(SceneKnowledge {
                scene: scene.clone(),
                characters: vec![CharacterKnowledge {
                    character: character.clone(),
                    knows: knows.clone(),
                    not_knows: not_knows.clone(),
                }],
            }),
        }
    }

    Json(json!({
        "ledger_id": ledger_id,
        "scene": scene,
        "character": character,
        "registered": true,
    }))
    .into_response()
}

pub async fn knowledge_check(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let sess = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let v = match parse_body(body) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let ledger_id = match get_str(&v, "ledger_id") {
        Ok(s) => s,
        Err(r) => return r,
    };
    let scene = match get_str(&v, "scene") {
        Ok(s) => s,
        Err(r) => return r,
    };
    let character = match get_str(&v, "character") {
        Ok(s) => s,
        Err(r) => return r,
    };
    let statement = match get_str(&v, "statement") {
        Ok(s) => s,
        Err(r) => return r,
    };

    let key = scoped_key(&sess.user_id, &ledger_id);
    let mut ledgers = store().knowledge.lock();
    let ledger = ledgers
        .entry(key)
        .or_insert_with(KnowledgeLedger::new);

    match ledger.check_statement(&scene, &character, &statement) {
        Ok(()) => Json(json!({
            "ledger_id": ledger_id,
            "violation": false,
            "message": "通过：角色未说出不该知道的事",
        }))
        .into_response(),
        Err(violation) => (
            StatusCode::CONFLICT,
            Json(json!({
                "ledger_id": ledger_id,
                "violation": true,
                "scene": violation.scene,
                "character": violation.character,
                "claimed": violation.claimed,
                "reason": violation.reason,
            })),
        )
            .into_response(),
    }
}

pub async fn knowledge_get(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(ledger_id): Path<String>,
) -> Response {
    let sess = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let key = scoped_key(&sess.user_id, &ledger_id);
    let ledgers = store().knowledge.lock();
    match ledgers.get(&key) {
        Some(l) => Json(json!({
            "ledger_id": ledger_id,
            "scenes": l.scenes.iter().map(|sc| json!({
                "scene": sc.scene,
                "characters": sc.characters.iter().map(|ck| json!({
                    "character": ck.character,
                    "knows": ck.knows,
                    "not_knows": ck.not_knows,
                })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "violations": l.violations.iter().map(|v| json!({
                "scene": v.scene, "character": v.character,
                "claimed": v.claimed, "reason": v.reason,
            })).collect::<Vec<_>>(),
        }))
        .into_response(),
        None => return not_found("NOVEL_NOT_FOUND", format!("KnowledgeLedger {ledger_id} not found")),
    }
}

// ============================================================
// 4. ForeshadowLedger 伏笔台账
// ============================================================

pub async fn foreshadow_plant(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let sess = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let v = match parse_body(body) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let ledger_id = match get_str(&v, "ledger_id") {
        Ok(s) => s,
        Err(r) => return r,
    };
    let id = match get_str(&v, "id") {
        Ok(s) => s,
        Err(r) => return r,
    };
    let desc = match get_str(&v, "desc") {
        Ok(s) => s,
        Err(r) => return r,
    };
    let chapter = match get_str(&v, "chapter") {
        Ok(s) => s,
        Err(r) => return r,
    };

    let key = scoped_key(&sess.user_id, &ledger_id);
    let mut ledgers = store().foreshadow.lock();
    let ledger = ledgers
        .entry(key)
        .or_insert_with(ForeshadowLedger::new);
    ledger.plant(&id, &desc, &chapter);

    Json(json!({
        "ledger_id": ledger_id,
        "planted": id,
        "chapter": chapter,
        "unresolved_count": ledger.unresolved().len(),
        "resolve_rate": ledger.resolve_rate(),
    }))
    .into_response()
}

pub async fn foreshadow_resolve(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let sess = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let v = match parse_body(body) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let ledger_id = match get_str(&v, "ledger_id") {
        Ok(s) => s,
        Err(r) => return r,
    };
    let id = match get_str(&v, "id") {
        Ok(s) => s,
        Err(r) => return r,
    };
    let chapter = match get_str(&v, "chapter") {
        Ok(s) => s,
        Err(r) => return r,
    };

    let key = scoped_key(&sess.user_id, &ledger_id);
    let mut ledgers = store().foreshadow.lock();
    let ledger = match ledgers.get_mut(&key) {
        Some(l) => l,
        None => {
            return not_found("NOVEL_NOT_FOUND", format!("ForeshadowLedger {ledger_id} not found"));
        }
    };
    match ledger.resolve(&id, &chapter) {
        Ok(()) => Json(json!({
            "ledger_id": ledger_id,
            "resolved": id,
            "chapter": chapter,
            "unresolved_count": ledger.unresolved().len(),
            "resolve_rate": ledger.resolve_rate(),
        }))
        .into_response(),
        Err(e) => return conflict("NOVEL_CONFLICT", e),
    }
}

pub async fn foreshadow_get(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(ledger_id): Path<String>,
) -> Response {
    let sess = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let key = scoped_key(&sess.user_id, &ledger_id);
    let ledgers = store().foreshadow.lock();
    match ledgers.get(&key) {
        Some(l) => Json(json!({
            "ledger_id": ledger_id,
            "items": l.items.iter().map(|f| json!({
                "id": f.id, "desc": f.desc,
                "planted_chapter": f.planted_chapter,
                "resolved_chapter": f.resolved_chapter,
            })).collect::<Vec<_>>(),
            "unresolved": l.unresolved().iter().map(|f| json!({
                "id": f.id, "desc": f.desc, "planted_chapter": f.planted_chapter,
            })).collect::<Vec<_>>(),
            "resolve_rate": l.resolve_rate(),
        }))
        .into_response(),
        None => return not_found("NOVEL_NOT_FOUND", format!("ForeshadowLedger {ledger_id} not found")),
    }
}
