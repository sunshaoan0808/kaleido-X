//! 场记卡（U3）：每场结束时自动生成的场景摘要持久化快照。
//!
//! 吸收自 plan_kaleido_absorb B4「场记卡」：后端场记记录 = 场景/人物/事件/状态变化摘要。
//! 数据源为会话的 `memory_l1.scene_summary`（LLM 抽取的当前场景摘要），
//! 当摘要变化时由 server 层调用 `record_if_changed` 落库一条场记卡。
//!
//! 仅依赖 SQLite（rusqlite），无第三方新增依赖。

use crate::db::{DbError, DbPool};
use chrono::Utc;
use rusqlite::{params, Row};
use serde::{Deserialize, Serialize};
use std::path::Path;
use uuid::Uuid;

#[derive(Debug)]
pub enum SceneCardError {
    Db(DbError),
    Sql(rusqlite::Error),
    NotFound(String),
    BadRequest(String),
}

impl std::fmt::Display for SceneCardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SceneCardError::Db(e) => write!(f, "scene card db: {e}"),
            SceneCardError::Sql(e) => write!(f, "scene card sql: {e}"),
            SceneCardError::NotFound(what) => write!(f, "scene card {what} not found"),
            SceneCardError::BadRequest(msg) => write!(f, "bad request: {msg}"),
        }
    }
}

impl std::error::Error for SceneCardError {}

impl From<DbError> for SceneCardError {
    fn from(e: DbError) -> Self {
        SceneCardError::Db(e)
    }
}

/// 一场戏结束后的摘要卡。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SceneCard {
    pub id: String,
    pub work_id: String,
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    pub turn: u32,
    /// 场景名（node 名 / 摘要首行，展示用）。
    pub scene: String,
    /// scene_summary 全文（场景/人物/事件/状态变化）。
    pub summary: String,
    pub created_at: String,
}

fn row_to_card(row: &Row<'_>) -> rusqlite::Result<SceneCard> {
    Ok(SceneCard {
        id: row.get("id")?,
        work_id: row.get("work_id")?,
        session_id: row.get("session_id")?,
        node_id: row.get("node_id")?,
        turn: row.get("turn")?,
        scene: row.get("scene")?,
        summary: row.get("summary")?,
        created_at: row.get("created_at")?,
    })
}

#[derive(Clone)]
pub struct SceneCardStore {
    pool: DbPool,
}

impl SceneCardStore {
    pub fn open(path: &Path) -> Result<Self, SceneCardError> {
        let pool = DbPool::open(path, 4)?;
        Self::ensure_schema(&pool)?;
        Ok(SceneCardStore { pool })
    }

    pub fn open_in_memory() -> Result<Self, SceneCardError> {
        let pool = DbPool::open_in_memory(4)?;
        Self::ensure_schema(&pool)?;
        Ok(SceneCardStore { pool })
    }

    fn ensure_schema(pool: &DbPool) -> Result<(), SceneCardError> {
        let mut c = pool.get()?;
        c.conn()
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS scene_cards (
                    id TEXT PRIMARY KEY,
                    work_id TEXT NOT NULL,
                    session_id TEXT NOT NULL,
                    node_id TEXT,
                    turn INTEGER NOT NULL DEFAULT 0,
                    scene TEXT NOT NULL DEFAULT '',
                    summary TEXT NOT NULL DEFAULT '',
                    created_at TEXT NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_scene_cards_work ON scene_cards(work_id, created_at);
                CREATE INDEX IF NOT EXISTS idx_scene_cards_session ON scene_cards(session_id, created_at);",
            )
            .map_err(SceneCardError::Sql)?;
        Ok(())
    }

    /// 写入一张场记卡。若该会话最后一张卡摘要与本次相同，则跳过（去重）。
    pub fn record_if_changed(
        &self,
        work_id: &str,
        session_id: &str,
        node_id: Option<&str>,
        turn: u32,
        summary: &str,
    ) -> Result<Option<SceneCard>, SceneCardError> {
        let trimmed = summary.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        if let Some(last) = self.last_by_session(session_id)? {
            if last.summary == trimmed {
                return Ok(None);
            }
        }
        let scene = node_id
            .map(|n| n.to_string())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| first_line(trimmed));
        let card = SceneCard {
            id: format!("sc-{}", Uuid::new_v4()),
            work_id: work_id.to_string(),
            session_id: session_id.to_string(),
            node_id: node_id.map(|n| n.to_string()),
            turn,
            scene,
            summary: trimmed.to_string(),
            created_at: Utc::now().to_rfc3339(),
        };
        let mut c = self.pool.get()?;
        c.conn()
            .execute(
                "INSERT INTO scene_cards (id, work_id, session_id, node_id, turn, scene, summary, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    card.id,
                    card.work_id,
                    card.session_id,
                    card.node_id,
                    card.turn,
                    card.scene,
                    card.summary,
                    card.created_at
                ],
            )
            .map_err(SceneCardError::Sql)?;
        Ok(Some(card))
    }

    /// 某作品的全部场记卡（按时间倒序）。
    pub fn list_by_work(&self, work_id: &str) -> Result<Vec<SceneCard>, SceneCardError> {
        let mut c = self.pool.get()?;
        let mut stmt = c
            .conn()
            .prepare(
                "SELECT id, work_id, session_id, node_id, turn, scene, summary, created_at
                 FROM scene_cards WHERE work_id = ?1 ORDER BY created_at DESC, rowid DESC",
            )
            .map_err(SceneCardError::Sql)?;
        let rows = stmt
            .query_map(params![work_id], row_to_card)
            .map_err(SceneCardError::Sql)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(SceneCardError::Sql)?);
        }
        Ok(out)
    }

    /// 某会话最后一张场记卡（去重/增量用）。
    pub fn last_by_session(&self, session_id: &str) -> Result<Option<SceneCard>, SceneCardError> {
        let mut c = self.pool.get()?;
        let mut stmt = c
            .conn()
            .prepare(
                "SELECT id, work_id, session_id, node_id, turn, scene, summary, created_at
                 FROM scene_cards WHERE session_id = ?1 ORDER BY created_at DESC, rowid DESC LIMIT 1",
            )
            .map_err(SceneCardError::Sql)?;
        let mut rows = stmt
            .query_map(params![session_id], row_to_card)
            .map_err(SceneCardError::Sql)?;
        match rows.next() {
            Some(Ok(card)) => Ok(Some(card)),
            Some(Err(e)) => Err(SceneCardError::Sql(e)),
            None => Ok(None),
        }
    }

    /// 删除一张场记卡。
    pub fn delete(&self, id: &str) -> Result<bool, SceneCardError> {
        let mut c = self.pool.get()?;
        let n = c
            .conn()
            .execute("DELETE FROM scene_cards WHERE id = ?1", params![id])
            .map_err(SceneCardError::Sql)?;
        Ok(n > 0)
    }

    /// 清空某作品的全部场记卡。
    pub fn clear_work(&self, work_id: &str) -> Result<u64, SceneCardError> {
        let mut c = self.pool.get()?;
        let n = c
            .conn()
            .execute("DELETE FROM scene_cards WHERE work_id = ?1", params![work_id])
            .map_err(SceneCardError::Sql)?;
        Ok(n as u64)
    }
}

/// 取多行摘要的第一行作为场景名。
fn first_line(s: &str) -> String {
    s.lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .chars()
        .take(40)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> SceneCardStore {
        SceneCardStore::open_in_memory().unwrap()
    }

    #[test]
    fn record_and_list_by_work() {
        let s = store();
        let a = s
            .record_if_changed("w1", "s1", Some("node-1"), 3, "当前场景：王宫（第3回合）")
            .unwrap()
            .expect("first card");
        assert!(a.id.starts_with("sc-"));
        // scene 优先取 node_id（比摘要首行更精确的语义标识）
        assert_eq!(a.scene, "node-1");
        // 相同摘要去重
        let dup = s
            .record_if_changed("w1", "s1", Some("node-1"), 4, "当前场景：王宫（第3回合）")
            .unwrap();
        assert!(dup.is_none());
        // 摘要变化新增
        let b = s
            .record_if_changed("w1", "s1", Some("node-2"), 8, "当前场景：森林（第8回合压缩）")
            .unwrap()
            .expect("second card");
        assert_eq!(b.scene, "node-2");
        // node_id 为空时回退摘要首行（first_line 取整行，40 字截断）
        let c = s
            .record_if_changed("w1", "s3", None, 12, "当前场景：雨巷（第12回合）")
            .unwrap()
            .expect("fallback card");
        assert_eq!(c.scene, "当前场景：雨巷（第12回合）");
        let cards = s.list_by_work("w1").unwrap();
        assert_eq!(cards.len(), 3);
        // 倒序：创建时间最新在前（c 最后插入）
        assert_eq!(cards[0].id, c.id);
        // 空摘要不落库
        let none = s.record_if_changed("w1", "s2", None, 1, "   ").unwrap();
        assert!(none.is_none());
        // 删除
        assert!(s.delete(&a.id).unwrap());
        assert!(!s.delete(&a.id).unwrap());
        // 清空（删 a 后剩 b + c 共 2 张）
        let cleared = s.clear_work("w1").unwrap();
        assert_eq!(cleared, 2);
        assert!(s.list_by_work("w1").unwrap().is_empty());
    }

    #[test]
    fn last_by_session_works() {
        let s = store();
        s.record_if_changed("w1", "sx", None, 3, "第一场").unwrap();
        s.record_if_changed("w1", "sx", None, 8, "第二场").unwrap();
        let last = s.last_by_session("sx").unwrap().unwrap();
        assert_eq!(last.summary, "第二场");
        assert!(s.last_by_session("nope").unwrap().is_none());
    }
}
