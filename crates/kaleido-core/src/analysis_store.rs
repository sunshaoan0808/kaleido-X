//! AnalysisStore: persistence for AI writing-analysis tasks (P3).
//! Mirrors foreshadow_store.rs: DbPool + row mappers + optimistic JSON columns.

use crate::db::{DbError, DbPool};
use chrono::Utc;
use rusqlite::{params, Row};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;
use uuid::Uuid;

pub const ANALYSIS_KINDS: &[&str] = &[
    "chapter-analysis",
    "character-extraction",
    "character-identity-audit",
    "timeline-analysis",
    "relationship-analysis",
    "worldview-analysis",
    "setting-extraction",
    "consistency-check",
    "book-analysis",
];

#[derive(Debug)]
pub enum AnalysisError {
    Db(DbError),
    NotFound(String),
    InvalidStatus(String),
    InvalidKind(String),
    BadRequest(String),
}

impl std::fmt::Display for AnalysisError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AnalysisError::Db(e) => write!(f, "analysis db: {e}"),
            AnalysisError::NotFound(what) => write!(f, "analysis {what} not found"),
            AnalysisError::InvalidStatus(s) => write!(f, "invalid analysis status: {s}"),
            AnalysisError::InvalidKind(k) => write!(f, "invalid analysis kind: {k}"),
            AnalysisError::BadRequest(m) => write!(f, "bad request: {m}"),
        }
    }
}

impl std::error::Error for AnalysisError {}

impl From<DbError> for AnalysisError {
    fn from(e: DbError) -> Self {
        AnalysisError::Db(e)
    }
}

pub fn is_valid_kind(kind: &str) -> bool {
    ANALYSIS_KINDS.contains(&kind)
}

/// P1: LLM 输出宽容降级——逐项校验 suggestion，丢弃非法项返回可计数。
/// 规则：kind 含 relationship 的必须 from/to 非空（顶层或在 payload 内）；
/// 其余 kind 必须命中 ANALYSIS_KINDS 白名单。返回（保留项, 丢弃数）。
pub fn sanitize_suggestions(sugs: &[Value]) -> (Vec<Value>, usize) {
    fn field_nonempty(map: &serde_json::Map<String, Value>, field: &str) -> bool {
        if let Some(v) = map.get(field).and_then(|v| v.as_str()) {
            return !v.is_empty();
        }
        map.get("payload")
            .and_then(|p| p.as_object())
            .and_then(|p| p.get(field))
            .and_then(|v| v.as_str())
            .map(|s| !s.is_empty())
            .unwrap_or(false)
    }
    let mut dropped = 0usize;
    let mut kept: Vec<Value> = Vec::with_capacity(sugs.len());
    for sug in sugs {
        let Some(map) = sug.as_object() else {
            dropped += 1;
            continue;
        };
        let Some(kind) = map.get("kind").and_then(|v| v.as_str()) else {
            dropped += 1;
            continue;
        };
        if kind.contains("relationship") {
            if field_nonempty(map, "from") && field_nonempty(map, "to") {
                kept.push(sug.clone());
            } else {
                dropped += 1;
            }
        } else if is_valid_kind(kind) {
            kept.push(sug.clone());
        } else {
            dropped += 1;
        }
    }
    (kept, dropped)
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct AnalysisEvidence {
    /// source chapter/file reference
    pub source: String,
    /// optional line number inside the source
    #[serde(default)]
    pub line: Option<usize>,
    pub quote: String,
    #[serde(default)]
    pub note: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct AnalysisSuggestion {
    pub id: String,
    pub task_id: String,
    pub kind: String,
    pub payload: Value,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
    /// When the confirmed suggestion was applied cross-store (P0 闭环)
    #[serde(default)]
    pub applied_at: Option<String>,
    /// Non-fatal apply error (fail-open, suggestion stays confirmed)
    #[serde(default)]
    pub apply_error: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct AnalysisResultBody {
    pub summary: Value,
    #[serde(default)]
    pub evidence: Vec<AnalysisEvidence>,
    #[serde(default)]
    pub suggestions: Vec<Value>,
    /// P1: LLM 响应中被逐项校验丢弃的 suggestion 数量（>0 时任务标 partial_success）
    #[serde(default)]
    pub dropped_suggestions: usize,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct AnalysisTask {
    pub id: String,
    pub work_id: String,
    pub kind: String,
    pub scope: Value,
    pub status: String,
    pub summary: Value,
    pub evidence: Vec<AnalysisEvidence>,
    pub suggestions: Vec<AnalysisSuggestion>,
    pub failure: String,
    pub created_by: String,
    pub created_at: String,
    pub updated_at: String,
}

fn now() -> String {
    Utc::now().to_rfc3339()
}

/// Public timestamp helper (used by server-layer apply orchestration).
pub fn now_ts() -> String {
    now()
}

#[derive(Clone)]
pub struct AnalysisStore {
    pool: DbPool,
}

fn row_suggestion(r: &Row<'_>) -> rusqlite::Result<AnalysisSuggestion> {
    Ok(AnalysisSuggestion {
        id: r.get(0)?,
        task_id: r.get(1)?,
        kind: r.get(2)?,
        payload: serde_json::from_str(&r.get::<_, String>(3)?).unwrap_or(Value::Null),
        status: r.get(4)?,
        created_at: r.get(5)?,
        updated_at: r.get(6)?,
        applied_at: r.get(7)?,
        apply_error: r.get(8)?,
    })
}

// [P7] SUG_COLS 常量已内联回查询（原拟复用但两处 SELECT 均为字面量，删除避免 dead_code 告警）。

impl AnalysisStore {
    pub fn open(path: &Path) -> Result<Self, AnalysisError> {
        Ok(AnalysisStore { pool: DbPool::open(path, 4)? })
    }

    pub fn open_in_memory() -> Result<Self, AnalysisError> {
        Ok(AnalysisStore { pool: DbPool::open_in_memory(4)? })
    }

    fn conn(&self) -> Result<crate::db::DbConn, AnalysisError> {
        self.pool.get().map_err(AnalysisError::Db)
    }

    fn get_suggestions(&self, task_id: &str, status: Option<&str>) -> Result<Vec<AnalysisSuggestion>, AnalysisError> {
        let mut c = self.conn()?;
        let conn = c.conn();
        let rows = match status {
            Some(s) => {
                let mut stmt = conn
                    .prepare("SELECT id, task_id, kind, payload, status, created_at, updated_at, applied_at, apply_error FROM analysis_suggestions WHERE task_id=?1 AND status=?2 ORDER BY created_at")
                    .map_err(DbError::Migrate)?;
                let iter = stmt
                    .query_map(params![task_id, s], row_suggestion)
                    .map_err(|e| AnalysisError::Db(DbError::Migrate(e)))?;
                iter.collect::<rusqlite::Result<Vec<_>>>().map_err(|e| AnalysisError::Db(DbError::Migrate(e)))?
            }
            None => {
                let mut stmt = conn
                    .prepare("SELECT id, task_id, kind, payload, status, created_at, updated_at, applied_at, apply_error FROM analysis_suggestions WHERE task_id=?1 ORDER BY created_at")
                    .map_err(DbError::Migrate)?;
                let iter = stmt
                    .query_map([task_id], row_suggestion)
                    .map_err(|e| AnalysisError::Db(DbError::Migrate(e)))?;
                iter.collect::<rusqlite::Result<Vec<_>>>().map_err(|e| AnalysisError::Db(DbError::Migrate(e)))?
            }
        };
        Ok(rows)
    }

    fn build_task(&self, id: &str) -> Result<AnalysisTask, AnalysisError> {
        let mut c = self.conn()?;
        let conn = c.conn();
        let row = conn
            .query_row(
                "SELECT id, work_id, kind, scope, status, summary, evidence, failure, created_by, created_at, updated_at FROM analysis_tasks WHERE id=?1",
                [id],
                |r| {
                    Ok(AnalysisTask {
                        id: r.get(0)?,
                        work_id: r.get(1)?,
                        kind: r.get(2)?,
                        scope: serde_json::from_str(&r.get::<_, String>(3)?).unwrap_or(Value::Null),
                        status: r.get(4)?,
                        summary: serde_json::from_str(&r.get::<_, String>(5)?).unwrap_or(Value::Null),
                        evidence: serde_json::from_str(&r.get::<_, String>(6)?).unwrap_or_default(),
                        suggestions: vec![],
                        failure: r.get(7)?,
                        created_by: r.get(8)?,
                        created_at: r.get(9)?,
                        updated_at: r.get(10)?,
                    })
                },
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => AnalysisError::NotFound(format!("task {id}")),
                other => AnalysisError::Db(DbError::Migrate(other)),
            })?;
        let suggestions = self.get_suggestions(id, None)?;
        Ok(AnalysisTask { suggestions, ..row })
    }

    pub fn create_task(&self, work_id: &str, kind: &str, scope: Value, created_by: &str) -> Result<AnalysisTask, AnalysisError> {
        if !is_valid_kind(kind) {
            return Err(AnalysisError::InvalidKind(kind.to_string()));
        }
        let id = Uuid::new_v4().to_string();
        let ts = now();
        let scope_json = serde_json::to_string(&scope).unwrap_or_else(|_| "{}".into());
        let mut c = self.conn()?;
        c.conn()
            .execute(
                "INSERT INTO analysis_tasks (id, work_id, kind, scope, status, summary, evidence, suggestions, failure, created_by, created_at, updated_at)
                 VALUES (?1,?2,?3,?4,'queued','{}','[]','[]','',?5,?6,?6)",
                params![id, work_id, kind, scope_json, created_by, ts],
            )
            .map_err(DbError::Migrate)?;
        self.build_task(&id)
    }

    pub fn get_task(&self, id: &str) -> Result<AnalysisTask, AnalysisError> {
        self.build_task(id)
    }

    pub fn list_tasks(&self, work_id: &str, kind: Option<&str>) -> Result<Vec<AnalysisTask>, AnalysisError> {
        let mut c = self.conn()?;
        let conn = c.conn();
        let ids: Vec<String> = match kind {
            Some(k) => {
                let mut stmt = conn
                    .prepare("SELECT id FROM analysis_tasks WHERE work_id=?1 AND kind=?2 ORDER BY created_at DESC")
                    .map_err(DbError::Migrate)?;
                let rows = stmt
                    .query_map(params![work_id, k], |r| r.get::<_, String>(0))
                    .map_err(|e| AnalysisError::Db(DbError::Migrate(e)))?;
                rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|e| AnalysisError::Db(DbError::Migrate(e)))?
            }
            None => {
                let mut stmt = conn
                    .prepare("SELECT id FROM analysis_tasks WHERE work_id=?1 ORDER BY created_at DESC")
                    .map_err(DbError::Migrate)?;
                let rows = stmt
                    .query_map([work_id], |r| r.get::<_, String>(0))
                    .map_err(|e| AnalysisError::Db(DbError::Migrate(e)))?;
                rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|e| AnalysisError::Db(DbError::Migrate(e)))?
            }
        };
        let mut tasks = Vec::new();
        for id in ids {
            match self.build_task(&id) {
                Ok(t) => tasks.push(t),
                Err(_) => {} // skip deleted concurrently
            }
        }
        Ok(tasks)
    }

    pub fn set_status(&self, id: &str, status: &str) -> Result<AnalysisTask, AnalysisError> {
        match status {
            "queued" | "running" | "succeeded" | "failed" | "cancelled" | "partial_success" => {}
            other => return Err(AnalysisError::InvalidStatus(other.to_string())),
        }
        let ts = now();
        let mut c = self.conn()?;
        let n = c
            .conn()
            .execute(
                "UPDATE analysis_tasks SET status=?2, updated_at=?4, failure=IFNULL(failure, '') || (CASE WHEN ?2='failed' THEN ?3 ELSE '' END) WHERE id=?1",
                params![id, status, "", ts],
            )
            .map_err(DbError::Migrate)?;
        if n == 0 {
            return Err(AnalysisError::NotFound(format!("task {id}")));
        }
        self.build_task(id)
    }

    pub fn fail_task(&self, id: &str, message: &str) -> Result<AnalysisTask, AnalysisError> {
        let ts = now();
        let mut c = self.conn()?;
        let n = c
            .conn()
            .execute(
                "UPDATE analysis_tasks SET status='failed', failure=?2, updated_at=?3 WHERE id=?1",
                params![id, message, ts],
            )
            .map_err(DbError::Migrate)?;
        if n == 0 {
            return Err(AnalysisError::NotFound(format!("task {id}")));
        }
        self.build_task(id)
    }

    pub fn save_result(
        &self,
        id: &str,
        body: &AnalysisResultBody,
    ) -> Result<AnalysisTask, AnalysisError> {
        let summary_json = serde_json::to_string(&body.summary).unwrap_or_else(|_| "{}".into());
        let evidence_json = serde_json::to_string(&body.evidence).unwrap_or_else(|_| "[]".into());
        // P1: 逐项校验丢弃非法 suggestion（LLM 输出宽容降级）。
        let (kept, actual_dropped) = sanitize_suggestions(&body.suggestions);
        let st = if body.dropped_suggestions > 0 || actual_dropped > 0 {
            "partial_success"
        } else {
            "succeeded"
        };
        let mut c = self.conn()?;
        let conn = c.conn();
        let ts = now();
        let n = conn
            .execute(
                "UPDATE analysis_tasks SET status=?2, summary=?3, evidence=?4, updated_at=?5 WHERE id=?1",
                params![id, st, summary_json, evidence_json, ts],
            )
            .map_err(DbError::Migrate)?;
        if n == 0 {
            return Err(AnalysisError::NotFound(format!("task {id}")));
        }
        // Replace suggestion table rows.
        conn.execute("DELETE FROM analysis_suggestions WHERE task_id=?1", [id]).map_err(DbError::Migrate)?;
        for sug in &kept {
            let sug_id = Uuid::new_v4().to_string();
            let kind = sug.get("kind").and_then(|v| v.as_str()).unwrap_or("generic").to_string();
            let payload_json = match sug.get("payload") {
                Some(v) => serde_json::to_string(v).unwrap_or_else(|_| "{}".into()),
                None => serde_json::to_string(sug).unwrap_or_else(|_| "{}".into()),
            };
            conn.execute(
                "INSERT INTO analysis_suggestions (id, task_id, kind, payload, status, created_at, updated_at) VALUES (?1,?2,?3,?4,'pending',?5,?5)",
                params![sug_id, id, kind, payload_json, ts],
            )
            .map_err(DbError::Migrate)?;
        }
        self.build_task(id)
    }

    pub fn delete_task(&self, id: &str) -> Result<(), AnalysisError> {
        let mut c = self.conn()?;
        let n = c
            .conn()
            .execute("DELETE FROM analysis_tasks WHERE id=?1", [id])
            .map_err(DbError::Migrate)?;
        if n == 0 {
            return Err(AnalysisError::NotFound(format!("task {id}")));
        }
        Ok(())
    }

    pub fn confirm_suggestion(&self, task_id: &str, suggestion_id: &str) -> Result<AnalysisSuggestion, AnalysisError> {
        self.set_suggestion_status(task_id, suggestion_id, "confirmed")
    }

    pub fn reject_suggestion(&self, task_id: &str, suggestion_id: &str) -> Result<AnalysisSuggestion, AnalysisError> {
        self.set_suggestion_status(task_id, suggestion_id, "rejected")
    }

    fn set_suggestion_status(&self, task_id: &str, suggestion_id: &str, status: &str) -> Result<AnalysisSuggestion, AnalysisError> {
        match status {
            "pending" | "confirmed" | "rejected" => {}
            other => return Err(AnalysisError::InvalidStatus(other.to_string())),
        }
        let ts = now();
        let mut c = self.conn()?;
        let n = c
            .conn()
            .execute(
                "UPDATE analysis_suggestions SET status=?3, updated_at=?4 WHERE id=?1 AND task_id=?2",
                params![suggestion_id, task_id, status, ts],
            )
            .map_err(DbError::Migrate)?;
        if n == 0 {
            return Err(AnalysisError::NotFound(format!("suggestion {suggestion_id}")));
        }
        let mut c2 = self.conn()?;
        let sug = c2
            .conn()
            .query_row(
                "SELECT id, task_id, kind, payload, status, created_at, updated_at, applied_at, apply_error FROM analysis_suggestions WHERE id=?1",
                [suggestion_id],
                row_suggestion,
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => AnalysisError::NotFound(format!("suggestion {suggestion_id}")),
                other => AnalysisError::Db(DbError::Migrate(other)),
            })?;
        Ok(sug)
    }

    pub fn list_suggestions(&self, task_id: &str, status: Option<&str>) -> Result<Vec<AnalysisSuggestion>, AnalysisError> {
        self.get_suggestions(task_id, status)
    }

    /// Record the cross-store apply outcome for a confirmed suggestion (P0 闭环).
    /// Fail-open: sets `applied_at` on success, or `apply_error` on non-fatal
    /// apply failure. The status stays `confirmed` either way (intent is the
    /// source of truth; side effects are idempotent and retryable).
    pub fn mark_suggestion_applied(
        &self,
        suggestion_id: &str,
        applied_at: Option<&str>,
        apply_error: Option<&str>,
    ) -> Result<AnalysisSuggestion, AnalysisError> {
        let ts = now();
        let mut c = self.conn()?;
        let n = c
            .conn()
            .execute(
                "UPDATE analysis_suggestions SET applied_at=COALESCE(?2, applied_at), apply_error=?3, updated_at=?4 WHERE id=?1",
                params![suggestion_id, applied_at, apply_error, ts],
            )
            .map_err(DbError::Migrate)?;
        if n == 0 {
            return Err(AnalysisError::NotFound(format!("suggestion {suggestion_id}")));
        }
        let mut c2 = self.conn()?;
        let sug = c2
            .conn()
            .query_row(
                "SELECT id, task_id, kind, payload, status, created_at, updated_at, applied_at, apply_error FROM analysis_suggestions WHERE id=?1",
                [suggestion_id],
                row_suggestion,
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => AnalysisError::NotFound(format!("suggestion {suggestion_id}")),
                other => AnalysisError::Db(DbError::Migrate(other)),
            })?;
        Ok(sug)
    }
}

#[cfg(test)]
mod analysis_tests {
    use super::*;

    #[test]
    fn crud_and_suggestions_flow() {
        let s = AnalysisStore::open_in_memory().expect("open");
        let t = s.create_task("w1", "chapter-analysis", serde_json::json!({"paths": ["x.md"]}), "u1").expect("create");
        assert_eq!(t.status, "queued");

        let body = AnalysisResultBody {
            summary: serde_json::json!({"title": "t"}),
            evidence: vec![AnalysisEvidence { source: "x.md".into(), line: Some(3), quote: "q".into(), note: "n".into() }],
            suggestions: vec![serde_json::json!({"kind": "relationship", "from": "A", "to": "B"})],
            dropped_suggestions: 0,
        };
        let done = s.save_result(&t.id, &body).expect("save");
        assert_eq!(done.status, "succeeded");
        assert_eq!(done.evidence.len(), 1);
        assert_eq!(done.suggestions.len(), 1);
        assert_eq!(done.suggestions[0].status, "pending");

        let sid = done.suggestions[0].id.clone();
        let conf = s.confirm_suggestion(&t.id, &sid).expect("confirm");
        assert_eq!(conf.status, "confirmed");

        let list = s.list_tasks("w1", None).expect("list");
        assert_eq!(list.len(), 1);

        s.delete_task(&t.id).expect("delete");
        assert!(s.get_task(&t.id).is_err());
    }

    #[test]
    fn test_save_result_partial_success() {
        let s = AnalysisStore::open_in_memory().expect("open");
        let t = s.create_task("w1", "relationship-analysis", serde_json::json!({"paths": ["x.md"]}), "u1").expect("create");

        let body = AnalysisResultBody {
            summary: serde_json::json!({"title": "t"}),
            evidence: vec![],
            dropped_suggestions: 2,
            suggestions: vec![
                serde_json::json!({
                    "kind": "relationship-analysis",
                    "payload": {"from": "A", "to": "B", "chapter": 3},
                }),
                serde_json::json!({"kind": "not-a-valid-kind", "payload": {"title": "x"}}),
                serde_json::json!({"kind": "relationship-analysis", "payload": {"from": "A"}}),
            ],
        };
        let done = s.save_result(&t.id, &body).expect("save");
        assert_eq!(done.status, "partial_success");
        assert_eq!(body.dropped_suggestions, 2);
        // 合法项在 list_suggestions 中
        let list = s.list_suggestions(&t.id, None).expect("list");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].kind, "relationship-analysis");
        let p = list[0].payload.as_object().expect("payload object");
        assert_eq!(p.get("from").and_then(|v| v.as_str()), Some("A"));
        assert_eq!(p.get("to").and_then(|v| v.as_str()), Some("B"));
        // 非法项不在（kind 不在白名单、关系缺 to 均被丢弃）
        assert!(!list.iter().any(|sg| sg.kind == "not-a-valid-kind"));
    }
}