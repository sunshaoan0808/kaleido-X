//! Character relationship graph store (P1).
//!
//! Operates on the `characters` / `relationships` tables created by migration v2.
//! All methods take `work_id` so one SQLite file can host many works. Writes are
//! per-statement transactions; reads go through the shared [DbPool].

use crate::db::{DbError, DbPool};
use chrono::Utc;
use rusqlite::OptionalExtension;
use rusqlite::{params, Row};
use serde::{Deserialize, Serialize};
use std::path::Path;
use uuid::Uuid;

/// The five relationship categories, matching Scriverse RELATION_STYLE.
pub const VALID_CATEGORIES: &[&str] = &["family", "social", "emotional", "conflict", "uncertain"];

pub fn valid_category(cat: &str) -> bool {
    VALID_CATEGORIES.contains(&cat)
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Character {
    pub id: String,
    pub work_id: String,
    pub name: String,
    pub aliases: Vec<String>,
    pub note: String,
    pub color_idx: i64,
    pub created_at: String,
    pub updated_at: String,
    /// 来源标记: manual（作者手建）| ai_suggestion（AI 分析确认创建）
    #[serde(default)]
    pub origin: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Relationship {
    pub id: String,
    pub work_id: String,
    pub from_char: String,
    pub to_char: String,
    pub category: String,
    pub subtype: String,
    pub keywords: Vec<String>,
    pub confirmation_status: String,
    pub note: String,
    pub chapters: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug)]
pub enum GraphError {
    Db(DbError),
    DuplicateName,
    InvalidCategory(String),
    NotFound(String),
    BadRequest(String),
}

impl std::fmt::Display for GraphError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GraphError::Db(e) => write!(f, "graph db: {e}"),
            GraphError::DuplicateName => write!(f, "character name already exists in work"),
            GraphError::InvalidCategory(c) => write!(f, "invalid category: {c}"),
            GraphError::NotFound(what) => write!(f, "graph {what} not found"),
            GraphError::BadRequest(m) => write!(f, "bad request: {m}"),
        }
    }
}

impl std::error::Error for GraphError {}

impl From<DbError> for GraphError {
    fn from(e: DbError) -> Self {
        GraphError::Db(e)
    }
}

/// Result of a [GraphStore::cleanup] pass: how many duplicate nodes / reverse
/// edges were merged and how many dangling edges were removed.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct CleanupStats {
    /// 合并的重名节点对数（每组保留一个，其余删除，关系指向迁移）
    pub merged_characters: usize,
    /// 合并的反向重复边数（A→B 与 B→A 合并为一条）
    pub merged_relationships: usize,
    /// 删除的孤立边数（端点 character 不存在）
    pub deleted_orphan_edges: usize,
}

/// Lightweight relationship row used internally by [GraphStore::cleanup] so the
/// merged JSON columns (keywords/chapters) can be unioned in Rust.
#[derive(Debug)]
struct CleanupRelRow {
    id: String,
    from_char: String,
    to_char: String,
    category: String,
    subtype: String,
    keywords: Vec<String>,
    note: String,
    chapters: Vec<String>,
}

/// Owns the connection pool for one graph DB file.
#[derive(Clone)]
pub struct GraphStore {
    pool: DbPool,
}

fn now() -> String {
    Utc::now().to_rfc3339()
}

fn row_character(r: &Row<'_>) -> rusqlite::Result<Character> {
    let aliases_json: String = r.get(3)?;
    Ok(Character {
        id: r.get(0)?,
        work_id: r.get(1)?,
        name: r.get(2)?,
        aliases: serde_json::from_str(&aliases_json).unwrap_or_default(),
        note: r.get(4)?,
        color_idx: r.get(5)?,
        created_at: r.get(6)?,
        updated_at: r.get(7)?,
        origin: r.get(8)?,
    })
}

fn row_relationship(r: &Row<'_>) -> rusqlite::Result<Relationship> {
    let keywords_json: String = r.get(6)?;
    let chapters_json: String = r.get(9)?;
    Ok(Relationship {
        id: r.get(0)?,
        work_id: r.get(1)?,
        from_char: r.get(2)?,
        to_char: r.get(3)?,
        category: r.get(4)?,
        subtype: r.get(5)?,
        keywords: serde_json::from_str(&keywords_json).unwrap_or_default(),
        confirmation_status: r.get(7)?,
        note: r.get(8)?,
        chapters: serde_json::from_str(&chapters_json).unwrap_or_default(),
        created_at: r.get(10)?,
        updated_at: r.get(11)?,
    })
}

impl GraphStore {
    pub fn open(path: &Path) -> Result<Self, GraphError> {
        Ok(GraphStore { pool: DbPool::open(path, 4).map_err(GraphError::Db)? })
    }

    pub fn open_in_memory() -> Result<Self, GraphError> {
        Ok(GraphStore { pool: DbPool::open_in_memory(4).map_err(GraphError::Db)? })
    }

    fn conn(&self) -> Result<crate::db::DbConn, GraphError> {
        self.pool.get().map_err(GraphError::Db)
    }

    /// Full graph for a work: (characters, relationships).
    pub fn list(&self, work_id: &str) -> Result<(Vec<Character>, Vec<Relationship>), GraphError> {
        let mut c = self.conn()?;
        let conn = c.conn();
        let mut chars = Vec::new();
        {
            let mut stmt = conn
                .prepare("SELECT id, work_id, name, aliases, note, color_idx, created_at, updated_at, origin FROM characters WHERE work_id=?1 ORDER BY created_at")
                .map_err(DbError::Migrate)?;
            let rows = stmt.query_map([work_id], row_character).map_err(DbError::Migrate)?;
            for r in rows {
                chars.push(r.map_err(DbError::Migrate)?);
            }
        }
        let mut rels = Vec::new();
        {
            let mut stmt = conn
                .prepare(
                    "SELECT id, work_id, from_char, to_char, category, subtype, keywords, \
                     confirmation_status, note, chapters, created_at, updated_at FROM relationships \
                     WHERE work_id=?1 ORDER BY created_at",
                )
                .map_err(DbError::Migrate)?;
            let rows = stmt.query_map([work_id], row_relationship).map_err(DbError::Migrate)?;
            for r in rows {
                rels.push(r.map_err(DbError::Migrate)?);
            }
        }
        Ok((chars, rels))
    }

    /// Create a character. Errors with [GraphError::DuplicateName] when another
    /// character in the same work already has this name (design decision, see P1 doc).
    pub fn create_character(
        &self,
        work_id: &str,
        name: &str,
        aliases: &[String],
        note: &str,
        color_idx: i64,
    ) -> Result<Character, GraphError> {
        let mut c = self.conn()?;
        let conn = c.conn();
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM characters WHERE work_id=?1 AND name=?2)",
                params![work_id, name],
                |r| r.get(0),
            )
            .map_err(DbError::Migrate)?;
        if exists {
            return Err(GraphError::DuplicateName);
        }
        let id = Uuid::new_v4().to_string();
        let ts = now();
        conn.execute(
            "INSERT INTO characters (id, work_id, name, aliases, note, color_idx, created_at, updated_at, origin) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, 'manual')",
            params![
                id,
                work_id,
                name,
                serde_json::to_string(aliases).unwrap_or_else(|_| "[]".into()),
                note,
                color_idx,
                ts
            ],
        )
        .map_err(DbError::Migrate)?;
        self.get_character(&id)
    }

    pub fn get_character(&self, id: &str) -> Result<Character, GraphError> {
        let mut c = self.conn()?;
        let conn = c.conn();
        conn.query_row(
            "SELECT id, work_id, name, aliases, note, color_idx, created_at, updated_at, origin FROM characters WHERE id=?1",
            [id],
            row_character,
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => GraphError::NotFound(format!("character {id}")),
            other => GraphError::Db(DbError::Migrate(other)),
        })
    }

    pub fn update_character(
        &self,
        id: &str,
        name: &str,
        aliases: &[String],
        note: &str,
        color_idx: i64,
    ) -> Result<Character, GraphError> {
        let mut c = self.conn()?;
        let conn = c.conn();
        let work_id: String = conn
            .query_row("SELECT work_id FROM characters WHERE id=?1", [id], |r| r.get(0))
            .optional()
            .map_err(DbError::Migrate)?
            .ok_or_else(|| GraphError::NotFound(format!("character {id}")))?;
        // Duplicate-name check excluding self.
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM characters WHERE work_id=?1 AND name=?2 AND id<>?3)",
                params![work_id, name, id],
                |r| r.get(0),
            )
            .map_err(DbError::Migrate)?;
        if exists {
            return Err(GraphError::DuplicateName);
        }
        conn.execute(
            "UPDATE characters SET name=?2, aliases=?3, note=?4, color_idx=?5, updated_at=?6 WHERE id=?1",
            params![
                id,
                name,
                serde_json::to_string(aliases).unwrap_or_else(|_| "[]".into()),
                note,
                color_idx,
                now()
            ],
        )
        .map_err(DbError::Migrate)?;
        self.get_character(id)
    }

    /// Delete a character; relationships referencing it are removed via FK cascade.
    pub fn delete_character(&self, id: &str) -> Result<(), GraphError> {
        let mut c = self.conn()?;
        let n = c
            .conn()
            .execute("DELETE FROM characters WHERE id=?1", [id])
            .map_err(DbError::Migrate)?;
        if n == 0 {
            return Err(GraphError::NotFound(format!("character {id}")));
        }
        Ok(())
    }

    pub fn create_relationship(
        &self,
        work_id: &str,
        from_char: &str,
        to_char: &str,
        category: &str,
        subtype: &str,
        keywords: &[String],
        confirmation_status: &str,
        note: &str,
    ) -> Result<Relationship, GraphError> {
        if !valid_category(category) {
            return Err(GraphError::InvalidCategory(category.into()));
        }
        let mut c = self.conn()?;
        let conn = c.conn();
        // Both endpoints must exist (FK will also guard, but give a clean error).
        let ok: bool = conn
            .query_row(
                "SELECT (SELECT EXISTS(SELECT 1 FROM characters WHERE id=?1 AND work_id=?3)) \
                 AND (SELECT EXISTS(SELECT 1 FROM characters WHERE id=?2 AND work_id=?3))",
                params![from_char, to_char, work_id],
                |r| r.get(0),
            )
            .map_err(DbError::Migrate)?;
        if !ok {
            return Err(GraphError::NotFound(format!("endpoint character")));
        }
        let id = Uuid::new_v4().to_string();
        let ts = now();
        conn.execute(
            "INSERT INTO relationships (id, work_id, from_char, to_char, category, subtype, keywords, \
             confirmation_status, note, chapters, created_at, updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,'[]',?10,?10)",
            params![
                id,
                work_id,
                from_char,
                to_char,
                category,
                subtype,
                serde_json::to_string(keywords).unwrap_or_else(|_| "[]".into()),
                confirmation_status,
                note,
                ts
            ],
        )
        .map_err(DbError::Migrate)?;
        self.get_relationship(&id)
    }

    pub fn get_relationship(&self, id: &str) -> Result<Relationship, GraphError> {
        let mut c = self.conn()?;
        let conn = c.conn();
        conn.query_row(
            "SELECT id, work_id, from_char, to_char, category, subtype, keywords, \
             confirmation_status, note, chapters, created_at, updated_at FROM relationships WHERE id=?1",
            [id],
            row_relationship,
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => GraphError::NotFound(format!("relationship {id}")),
            other => GraphError::Db(DbError::Migrate(other)),
        })
    }

    /// Resolve a character by normalized name (trim + case-insensitive), or create
    /// it when missing (origin='ai_suggestion'). This is the name→ID bridge for
    /// AI-analysis confirmation: LLM gives names, `create_relationship` needs IDs.
    pub fn resolve_or_create_character(
        &self,
        work_id: &str,
        name: &str,
        note: &str,
        distil_id: Option<&str>,
    ) -> Result<Character, GraphError> {
        let norm = name.trim().to_lowercase();
        if norm.is_empty() {
            return Err(GraphError::BadRequest("empty character name".into()));
        }
        // [L2] 蒸馏连线: distil_id(c-distil-N) 优先精确解析 → 同卡重蒸幂等;
        // 其次 name 精确匹配; aliases(json_each) 参与解析(机制就位, entities.json
        // aliases 扩展后自动生效)。distil_id 空串视为 None。
        let did = distil_id.filter(|d| !d.trim().is_empty()).map(|d| d.trim());
        let mut c = self.conn()?;
        let conn = c.conn();
        let existing: Option<Character> = if let Some(did) = did {
            conn.query_row(
                "SELECT id, work_id, name, aliases, note, color_idx, created_at, updated_at, origin \
                 FROM characters WHERE work_id=?1 AND distil_id=?2 LIMIT 1",
                params![work_id, did],
                row_character,
            )
            .optional()
            .map_err(DbError::Migrate)?
        } else {
            None
        };
        let existing = existing
            .or_else(|| {
                conn.query_row(
                    "SELECT id, work_id, name, aliases, note, color_idx, created_at, updated_at, origin \
                     FROM characters WHERE work_id=?1 AND (LOWER(TRIM(name))=?2 \
                     OR EXISTS(SELECT 1 FROM json_each(aliases) WHERE LOWER(TRIM(json_each.value))=?2)) \
                     LIMIT 1",
                    params![work_id, norm],
                    row_character,
                )
                .optional()
                .map_err(DbError::Migrate)
                .ok()
                .flatten()
            });
        if let Some(ch) = existing {
            // [L2] 旧实体回写连线: name 命中但 distil_id 缺失(升级前落库) → 补写,
            // 下次重蒸即走 distil_id 优先幂等解析。WHERE distil_id IS NULL 保证
            // 不改写已有连线(含冲突场景)。
            if let Some(did) = did {
                let _ = conn.execute(
                    "UPDATE characters SET distil_id=?1 WHERE id=?2 AND distil_id IS NULL",
                    params![did, ch.id],
                );
            }
            return Ok(ch);
        }
        let id = Uuid::new_v4().to_string();
        let ts = now();
        conn.execute(
            "INSERT INTO characters (id, work_id, name, aliases, note, color_idx, created_at, updated_at, origin, distil_id) \
             VALUES (?1, ?2, ?3, '[]', ?4, 0, ?5, ?5, 'ai_suggestion', ?6)",
            params![id, work_id, name.trim(), note, ts, did],
        )
        .map_err(DbError::Migrate)?;
        self.get_character(&id)
    }

    /// Create a relationship from a confirmed AI suggestion. Idempotent by
    /// `suggestion_id` (partial unique index): a retry returns the existing row
    /// instead of duplicating the edge. Both endpoints are resolved via
    /// [Self::resolve_or_create_character] (name→ID bridge).
    pub fn create_relationship_from_suggestion(
        &self,
        work_id: &str,
        from_name: &str,
        to_name: &str,
        from_distil_id: Option<&str>,
        to_distil_id: Option<&str>,
        category: &str,
        subtype: &str,
        note: &str,
        chapter: Option<&str>,
        suggestion_id: &str,
    ) -> Result<Relationship, GraphError> {
        if !valid_category(category) {
            return Err(GraphError::InvalidCategory(category.into()));
        }
        let chapters: Vec<String> = chapter.map(|c| vec![c.to_string()]).unwrap_or_default();
        let chapters_json = serde_json::to_string(&chapters).unwrap_or_else(|_| "[]".into());
        // 幂等: 同一建议重复 confirm 不产生重复边
        let mut c = self.conn()?;
        let conn = c.conn();
        let existing = conn
            .query_row(
                "SELECT id FROM relationships WHERE suggestion_id=?1",
                [suggestion_id],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(DbError::Migrate)?;
        if let Some(rid) = existing {
            // [L2] 幂等短路分支: 旧边(sid 命中)不重走 resolve, 但端点 distil 连线
            // 缺失(升级前落库)时补写, 保证全部端点最终持卡 id。
            let rel = self.get_relationship(&rid)?;
            let fdid = from_distil_id.filter(|d| !d.trim().is_empty());
            let tdid = to_distil_id.filter(|d| !d.trim().is_empty());
            if let Some(fd) = fdid {
                let _ = conn.execute(
                    "UPDATE characters SET distil_id=?1 WHERE id=?2 AND distil_id IS NULL",
                    params![fd, rel.from_char],
                );
            }
            if let Some(td) = tdid {
                let _ = conn.execute(
                    "UPDATE characters SET distil_id=?1 WHERE id=?2 AND distil_id IS NULL",
                    params![td, rel.to_char],
                );
            }
            return Ok(rel);
        }
        let from = self.resolve_or_create_character(work_id, from_name, note, from_distil_id)?;
        let to = self.resolve_or_create_character(work_id, to_name, note, to_distil_id)?;
        if from.id == to.id {
            return Err(GraphError::BadRequest("relationship endpoints must differ".into()));
        }
        // 演化合并: 同一对人物(from,to) 同 category 已有 confirmed 关系时, 不新建行,
        // 只把新 chapter 合并进已有 chapters(JSON 数组 union 去重, 保持顺序)。
        let existing_rel: Option<(String, String)> = conn
            .query_row(
                "SELECT id, chapters FROM relationships WHERE work_id=?1 AND from_char=?2 \
                 AND to_char=?3 AND category=?4 AND confirmation_status='confirmed' LIMIT 1",
                params![work_id, from.id, to.id, category],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(DbError::Migrate)?;
        if let Some((rid, chapters_json)) = existing_rel {
            // 兼容旧数据: chapters 为 NULL 或 '[]' 时按空数组处理。
            let mut merged: Vec<String> = serde_json::from_str(&chapters_json).unwrap_or_default();
            if let Some(c) = chapter {
                let c = c.to_string();
                if !merged.contains(&c) {
                    merged.push(c);
                }
            }
            let merged_json = serde_json::to_string(&merged).unwrap_or_else(|_| chapters_json);
            let ts = now();
            conn.execute(
                "UPDATE relationships SET chapters=?1, updated_at=?2 WHERE id=?3",
                params![merged_json, ts, rid],
            )
            .map_err(DbError::Migrate)?;
            return self.get_relationship(&rid);
        }
        let id = Uuid::new_v4().to_string();
        let ts = now();
        conn.execute(
            "INSERT INTO relationships (id, work_id, from_char, to_char, category, subtype, keywords, \
             confirmation_status, note, chapters, created_at, updated_at, suggestion_id) \
             VALUES (?1,?2,?3,?4,?5,?6,'[]','confirmed',?7,?8,?9,?9,?10)",
            params![id, work_id, from.id, to.id, category, subtype, note, chapters_json, ts, suggestion_id],
        )
        .map_err(DbError::Migrate)?;
        self.get_relationship(&id)
    }

    pub fn update_relationship(
        &self,
        id: &str,
        category: &str,
        subtype: &str,
        keywords: &[String],
        confirmation_status: &str,
        note: &str,
    ) -> Result<Relationship, GraphError> {
        if !valid_category(category) {
            return Err(GraphError::InvalidCategory(category.into()));
        }
        let mut c = self.conn()?;
        let n = c
            .conn()
            .execute(
                "UPDATE relationships SET category=?2, subtype=?3, keywords=?4, \
                 confirmation_status=?5, note=?6, updated_at=?7 WHERE id=?1",
                params![
                    id,
                    category,
                    subtype,
                    serde_json::to_string(keywords).unwrap_or_else(|_| "[]".into()),
                    confirmation_status,
                    note,
                    now()
                ],
            )
            .map_err(DbError::Migrate)?;
        if n == 0 {
            return Err(GraphError::NotFound(format!("relationship {id}")));
        }
        self.get_relationship(id)
    }

    pub fn delete_relationship(&self, id: &str) -> Result<(), GraphError> {
        let mut c = self.conn()?;
        let n = c
            .conn()
            .execute("DELETE FROM relationships WHERE id=?1", [id])
            .map_err(DbError::Migrate)?;
        if n == 0 {
            return Err(GraphError::NotFound(format!("relationship {id}")));
        }
        Ok(())
    }

    /// 批量去重维护：合并重名节点、合并反向重复边、删除孤立边。
    /// 返回合并统计。可作为图谱维护 API 供外部调用。
    ///
    /// `work_id` 为 `Some` 时只处理该 work；为 `None` 时遍历库内所有 work 逐个处理。
    /// 整个流程包在一个事务里，任一步失败整体回滚。幂等：连续跑两次第二次统计应为 0。
    pub fn cleanup(&self, work_id: Option<&str>) -> Result<CleanupStats, GraphError> {
        let mut c = self.conn()?;
        let conn = c.conn();
        let tx = conn.transaction().map_err(DbError::Migrate)?;

        let mut work_ids: Vec<String> = match work_id {
            Some(w) => vec![w.to_string()],
            None => {
                let mut seen: Vec<String> = Vec::new();
                {
                    let mut stmt = tx
                        .prepare("SELECT DISTINCT work_id FROM characters")
                        .map_err(DbError::Migrate)?;
                    let rows = stmt
                        .query_map(params![], |r| r.get::<_, String>(0))
                        .map_err(DbError::Migrate)?;
                    for r in rows {
                        seen.push(r.map_err(DbError::Migrate)?);
                    }
                }
                // 只有关系、没有角色残留（孤立边）的 work 也要覆盖到。
                {
                    let mut stmt = tx
                        .prepare("SELECT DISTINCT work_id FROM relationships")
                        .map_err(DbError::Migrate)?;
                    let rows = stmt
                        .query_map(params![], |r| r.get::<_, String>(0))
                        .map_err(DbError::Migrate)?;
                    for r in rows {
                        let w: String = r.map_err(DbError::Migrate)?;
                        if !seen.contains(&w) {
                            seen.push(w);
                        }
                    }
                }
                seen
            }
        };
        work_ids.sort();
        work_ids.dedup();

        let mut stats = CleanupStats::default();
        for wid in &work_ids {
            stats.merged_characters += Self::merge_duplicate_characters(&tx, wid)?;
            stats.merged_relationships += Self::merge_reverse_relationships(&tx, wid)?;
            stats.deleted_orphan_edges += Self::delete_orphan_edges(&tx, wid)?;
        }

        tx.commit().map_err(DbError::Migrate)?;
        Ok(stats)
    }

    /// 按 `LOWER(TRIM(name))` 分组合并同 work 重名角色：每组保留第一个（按
    /// created_at, id），其余节点并入保留节点的 aliases/note 后删除，其关系
    /// 全部改指保留节点；迁移产生的自环边一并删除。返回合并的组数。
    fn merge_duplicate_characters(
        tx: &rusqlite::Transaction<'_>,
        work_id: &str,
    ) -> Result<usize, GraphError> {
        let mut merged = 0usize;
        let mut stmt = tx
            .prepare(
                "SELECT LOWER(TRIM(name)) FROM characters WHERE work_id=?1 \
                 GROUP BY LOWER(TRIM(name)) HAVING COUNT(*) > 1",
            )
            .map_err(DbError::Migrate)?;
        let norms: Vec<String> = stmt
            .query_map([work_id], |r| r.get(0))
            .map_err(DbError::Migrate)?
            .collect::<Result<_, _>>()
            .map_err(DbError::Migrate)?;
        drop(stmt);

        for norm in norms {
            let mut stmt = tx
                .prepare(
                    "SELECT id, name, aliases, note FROM characters \
                     WHERE work_id=?1 AND LOWER(TRIM(name))=?2 ORDER BY created_at, id",
                )
                .map_err(DbError::Migrate)?;
            let members: Vec<(String, String, String, String)> = stmt
                .query_map(params![work_id, norm], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                })
                .map_err(DbError::Migrate)?
                .collect::<Result<_, _>>()
                .map_err(DbError::Migrate)?;
            if members.len() <= 1 {
                continue;
            }
            let keep = &members[0];
            let keep_id = keep.0.clone();
            let ts = now();
            let kept_trimmed = keep.1.trim().to_string();
            let mut aliases: Vec<String> =
                serde_json::from_str(&keep.2).unwrap_or_default();
            let mut note = keep.3.clone();

            for doomed in &members[1..] {
                let doomed_id = &doomed.0;
                // 关系改指保留节点。
                tx.execute(
                    "UPDATE relationships SET from_char=?1, updated_at=?3 \
                     WHERE work_id=?2 AND from_char=?4",
                    params![keep_id, work_id, ts, doomed_id],
                )
                .map_err(DbError::Migrate)?;
                tx.execute(
                    "UPDATE relationships SET to_char=?1, updated_at=?3 \
                     WHERE work_id=?2 AND to_char=?4",
                    params![keep_id, work_id, ts, doomed_id],
                )
                .map_err(DbError::Migrate)?;
                // aliases 并入：被删节点 name + aliases（去重、保序）。
                let mut doomed_aliases: Vec<String> =
                    serde_json::from_str(&doomed.2).unwrap_or_default();
                doomed_aliases.insert(0, doomed.1.trim().to_string());
                for a in doomed_aliases {
                    let a = a.trim().to_string();
                    if !a.is_empty() && !aliases.contains(&a) {
                        aliases.push(a);
                    }
                }
                // note 并入：非空去重，`；` 连接。
                let dnote = doomed.3.trim();
                if !dnote.is_empty()
                    && !note.split('；').any(|s| s.trim() == dnote)
                {
                    note = if note.trim().is_empty() {
                        dnote.to_string()
                    } else {
                        format!("{note}；{dnote}")
                    };
                }
                tx.execute("DELETE FROM characters WHERE id=?1", [doomed_id])
                    .map_err(DbError::Migrate)?;
            }
            tx.execute(
                "UPDATE characters SET name=?2, aliases=?3, note=?4, updated_at=?5 WHERE id=?1",
                params![
                    keep_id,
                    kept_trimmed,
                    serde_json::to_string(&aliases).unwrap_or_else(|_| "[]".into()),
                    note,
                    ts
                ],
            )
            .map_err(DbError::Migrate)?;
            merged += 1;
        }

        // 最终无自环：重定向后可能产生 from==to 的边，直接删除。
        tx.execute(
            "DELETE FROM relationships WHERE work_id=?1 AND from_char=to_char",
            [work_id],
        )
        .map_err(DbError::Migrate)?;
        Ok(merged)
    }

    /// 按 `(min(from,to), max(from,to))` 分组合并反向重复边：每组保留第一条，
    /// note/subtype 用 `→` 连接、chapters/keywords 并集去重保序、category 取组内
    /// 第一条。返回合并的组数。
    fn merge_reverse_relationships(
        tx: &rusqlite::Transaction<'_>,
        work_id: &str,
    ) -> Result<usize, GraphError> {
        let mut stmt = tx
            .prepare(
                "SELECT id, from_char, to_char, category, subtype, keywords, note, chapters \
                 FROM relationships WHERE work_id=?1 ORDER BY created_at, id",
            )
            .map_err(DbError::Migrate)?;
        let rows = stmt
            .query_map([work_id], |r| {
                Ok(CleanupRelRow {
                    id: r.get(0)?,
                    from_char: r.get(1)?,
                    to_char: r.get(2)?,
                    category: r.get(3)?,
                    subtype: r.get(4)?,
                    keywords: serde_json::from_str(&r.get::<_, String>(5)?).unwrap_or_default(),
                    note: r.get(6)?,
                    chapters: serde_json::from_str(&r.get::<_, String>(7)?).unwrap_or_default(),
                })
            })
            .map_err(DbError::Migrate)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(DbError::Migrate)?;

        let mut groups: std::collections::BTreeMap<(String, String), Vec<CleanupRelRow>> =
            std::collections::BTreeMap::new();
        for row in rows {
            let key = if row.from_char <= row.to_char {
                (row.from_char.clone(), row.to_char.clone())
            } else {
                (row.to_char.clone(), row.from_char.clone())
            };
            groups.entry(key).or_default().push(row);
        }

        let mut merged = 0usize;
        let ts = now();
        for (_, group) in groups {
            if group.len() <= 1 {
                continue;
            }
            let keep = &group[0];
            let mut chapters = keep.chapters.clone();
            let mut notes = vec![keep.note.clone()];
            let mut subtypes = vec![keep.subtype.clone()];
            let mut keywords = keep.keywords.clone();
            for r in &group[1..] {
                for ch in &r.chapters {
                    if !chapters.contains(ch) {
                        chapters.push(ch.clone());
                    }
                }
                if !r.note.is_empty() && !notes.contains(&r.note) {
                    notes.push(r.note.clone());
                }
                if !r.subtype.is_empty() && !subtypes.contains(&r.subtype) {
                    subtypes.push(r.subtype.clone());
                }
                for k in &r.keywords {
                    if !keywords.contains(k) {
                        keywords.push(k.clone());
                    }
                }
            }
            let joined_note = notes.join("→");
            let joined_subtype = subtypes.join("→");
            for r in &group[1..] {
                tx.execute("DELETE FROM relationships WHERE id=?1", [&r.id])
                    .map_err(DbError::Migrate)?;
            }
            tx.execute(
                "UPDATE relationships SET category=?2, subtype=?3, keywords=?4, note=?5, \
                 chapters=?6, updated_at=?7 WHERE id=?1",
                params![
                    &keep.id,
                    keep.category.clone(),
                    joined_subtype,
                    serde_json::to_string(&keywords).unwrap_or_else(|_| "[]".into()),
                    joined_note,
                    serde_json::to_string(&chapters).unwrap_or_else(|_| "[]".into()),
                    ts
                ],
            )
            .map_err(DbError::Migrate)?;
            merged += 1;
        }
        Ok(merged)
    }

    /// 删除 from_char 或 to_char 在 characters 表不存在的孤立边，返回删除条数。
    fn delete_orphan_edges(
        tx: &rusqlite::Transaction<'_>,
        work_id: &str,
    ) -> Result<usize, GraphError> {
        let n = tx
            .execute(
                "DELETE FROM relationships WHERE work_id=?1 \
                 AND (from_char NOT IN (SELECT id FROM characters) \
                      OR to_char NOT IN (SELECT id FROM characters))",
                [work_id],
            )
            .map_err(DbError::Migrate)?;
        Ok(n)
    }
}

#[cfg(test)]
mod graph_store_tests {
    use super::*;

    fn store() -> GraphStore {
        GraphStore::open_in_memory().expect("open in-memory graph store")
    }

    fn mk(store: &GraphStore, work: &str, name: &str) -> Character {
        store
            .create_character(work, name, &["A".into()], "note", 1)
            .expect("create character")
    }

    #[test]
    fn create_and_list_characters() {
        let s = store();
        let a = mk(&s, "w1", "张三");
        let b = mk(&s, "w1", "李四");
        mk(&s, "w2", "王五");
        let (chars, rels) = s.list("w1").expect("list");
        assert_eq!(chars.len(), 2);
        assert!(rels.is_empty());
        assert_eq!(chars[0].id, a.id);
        assert_eq!(chars[1].name, "李四");
        assert_eq!(chars[0].aliases, vec!["A".to_string()]);
        assert_eq!(b.work_id, "w1");
    }

    #[test]
    fn duplicate_name_rejected_in_same_work() {
        let s = store();
        mk(&s, "w1", "张三");
        let err = s.create_character("w1", "张三", &[], "", 0).expect_err("dup");
        assert!(matches!(err, GraphError::DuplicateName));
        // Same name in a different work is fine.
        mk(&s, "w2", "张三");
    }

    #[test]
    fn update_character_allows_same_name_and_rejects_other_dup() {
        let s = store();
        let a = mk(&s, "w1", "张三");
        let b = mk(&s, "w1", "李四");
        // Keep own name: allowed.
        let upd = s.update_character(&a.id, "张三", &["B".into()], "new", 2).expect("update own name");
        assert_eq!(upd.aliases, vec!["B".to_string()]);
        assert_eq!(upd.note, "new");
        // Take another's name: rejected.
        let err = s.update_character(&a.id, "李四", &[], "", 0).expect_err("dup on update");
        assert!(matches!(err, GraphError::DuplicateName));
        assert_eq!(b.id.len(), 36);
    }

    #[test]
    fn relationship_crud_and_cascade_delete() {
        let s = store();
        let a = mk(&s, "w1", "张三");
        let b = mk(&s, "w1", "李四");
        let r = s
            .create_relationship("w1", &a.id, &b.id, "family", "父子", &["挚爱".into()], "pending", "")
            .expect("create rel");
        assert_eq!(r.category, "family");
        assert_eq!(r.subtype, "父子");
        assert_eq!(r.keywords, vec!["挚爱".to_string()]);
        let (chars, rels) = s.list("w1").expect("list");
        assert_eq!(chars.len(), 2);
        assert_eq!(rels.len(), 1);
        // Update
        let ru = s
            .update_relationship(&r.id, "conflict", "决裂", &["反目".into()], "confirmed", "n")
            .expect("update rel");
        assert_eq!(ru.confirmation_status, "confirmed");
        assert_eq!(ru.category, "conflict");
        // Cascade: delete 张三 -> relationship gone
        s.delete_character(&a.id).expect("delete char");
        let (_, rels2) = s.list("w1").expect("list");
        assert!(rels2.is_empty());
        assert!(matches!(
            s.get_relationship(&r.id),
            Err(GraphError::NotFound(_))
        ));
    }

    #[test]
    fn relationship_validation_and_missing_endpoints() {
        let s = store();
        let a = mk(&s, "w1", "张三");
        let b = mk(&s, "w1", "李四");
        let err = s
            .create_relationship("w1", &a.id, &b.id, "rivals", "", &[], "pending", "")
            .expect_err("bad category");
        assert!(matches!(err, GraphError::InvalidCategory(_)));
        let err = s
            .create_relationship("w1", &a.id, "00000000-0000-0000-0000-000000000000", "family", "", &[], "pending", "")
            .expect_err("missing endpoint");
        assert!(matches!(err, GraphError::NotFound(_)));
    }

    #[test]
    fn delete_missing_entities_errors() {
        let s = store();
        assert!(matches!(s.delete_character("nope"), Err(GraphError::NotFound(_))));
        assert!(matches!(s.delete_relationship("nope"), Err(GraphError::NotFound(_))));
    }

    fn insert_character(
        s: &GraphStore,
        id: &str,
        work: &str,
        name: &str,
        created_at: &str,
    ) {
        let mut c = s.conn().expect("conn");
        let conn = c.conn();
        conn.execute(
            "INSERT INTO characters (id, work_id, name, aliases, note, color_idx, created_at, updated_at, origin) \
             VALUES (?1, ?2, ?3, '[]', '', 0, ?4, ?4, 'manual')",
            params![id, work, name, created_at],
        )
        .expect("insert character");
    }

    fn insert_relationship(
        s: &GraphStore,
        id: &str,
        work: &str,
        from: &str,
        to: &str,
        chapters: &str,
    ) {
        let mut c = s.conn().expect("conn");
        let conn = c.conn();
        conn.execute(
            "INSERT INTO relationships (id, work_id, from_char, to_char, category, subtype, keywords, \
             confirmation_status, note, chapters, created_at, updated_at) \
             VALUES (?1,?2,?3,?4,'conflict','','[]','pending','',?5,'2020-01-01T00:00:00Z','2020-01-01T00:00:00Z')",
            params![id, work, from, to, chapters],
        )
        .expect("insert relationship");
    }

    #[test]
    fn cleanup_merges_duplicate_characters_and_reroutes_edges() {
        let s = store();
        // 两个合法第三方角色，分别与同名角色各连一条关系（触发重定向且不产生反向并边）。
        let third_a = s
            .create_character("w1", "李四", &["李四".into()], "第三方", 1)
            .expect("third a");
        let third_b = s
            .create_character("w1", "王五", &["王五".into()], "第三方", 1)
            .expect("third b");
        // 两个同 work 同名角色，绕过 create_character 拒重直接 INSERT。
        insert_character(&s, "dup-a", "w1", "张三", "2020-01-01T00:00:00Z");
        insert_character(&s, "dup-b", "w1", "张三", "2020-01-02T00:00:00Z");
        // 各连一条关系指向不同的第三方。
        insert_relationship(&s, "r1", "w1", "dup-a", &third_a.id, "[]");
        insert_relationship(&s, "r2", "w1", "dup-b", &third_b.id, "[]");

        let stats = s.cleanup(Some("w1")).expect("cleanup");
        assert_eq!(stats.merged_characters, 1);
        assert_eq!(stats.merged_relationships, 0);
        assert_eq!(stats.deleted_orphan_edges, 0);

        // dup-a 较早创建，作为保留节点；dup-b 被删。
        let (chars, rels) = s.list("w1").expect("list");
        assert_eq!(chars.len(), 3);
        assert!(s.get_character("dup-b").is_err());
        assert_eq!(rels.len(), 2);
        // 两条关系都改指向保留节点 dup-a，目标第三方保持不同。
        assert!(rels.iter().all(|r| r.from_char == "dup-a"));
        assert!(rels.iter().any(|r| r.to_char == third_a.id));
        assert!(rels.iter().any(|r| r.to_char == third_b.id));
    }

    #[test]
    fn cleanup_merges_reverse_duplicate_edges_with_chapters_union() {
        let s = store();
        s.create_character("w1", "甲", &["甲".into()], "", 1).expect("甲");
        s.create_character("w1", "乙", &["乙".into()], "", 1).expect("乙");
        s.create_relationship_from_suggestion(
            "w1",
            "甲",
            "乙",
            None,
            None,
            "conflict",
            "敌对",
            "敌对",
            Some("第3章"),
            "sug-1",
        )
        .expect("edge A->B");
        s.create_relationship_from_suggestion(
            "w1",
            "乙",
            "甲",
            None,
            None,
            "conflict",
            "盟友",
            "盟友",
            Some("第20章"),
            "sug-2",
        )
        .expect("edge B->A");

        let stats = s.cleanup(Some("w1")).expect("cleanup");
        assert_eq!(stats.merged_characters, 0);
        assert_eq!(stats.merged_relationships, 1);
        assert_eq!(stats.deleted_orphan_edges, 0);

        let (_, rels) = s.list("w1").expect("list");
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0].chapters, vec!["第3章".to_string(), "第20章".to_string()]);
        assert!(rels[0].note.contains("→"));
        assert!(rels[0].note.contains("敌对"));
        assert!(rels[0].note.contains("盟友"));
    }

    #[test]
    fn cleanup_deletes_orphan_edges_and_is_idempotent() {
        let s = store();
        // 端点不存在的孤立边：FK 会拦截，故先在测试连接上临时关闭外键。
        {
            let mut c = s.conn().expect("conn");
            let conn = c.conn();
            conn.pragma_update(None, "foreign_keys", "OFF").expect("fk off");
            conn.execute(
                "INSERT INTO relationships (id, work_id, from_char, to_char, category, subtype, keywords, \
                 confirmation_status, note, chapters, created_at, updated_at) \
                 VALUES ('orphan-edge','w1','ghost','00000000-0000-0000-0000-000000000000','conflict','','[]','pending','','[]','2020-01-01T00:00:00Z','2020-01-01T00:00:00Z')",
                params![],
            )
            .expect("insert orphan edge");
        }

        let stats = s.cleanup(Some("w1")).expect("cleanup");
        assert_eq!(stats.deleted_orphan_edges, 1);
        assert_eq!(stats.merged_characters, 0);
        assert_eq!(stats.merged_relationships, 0);
        let (_, rels) = s.list("w1").expect("list");
        assert_eq!(rels.len(), 0);

        // 幂等：再跑一次统计全 0。
        let stats2 = s.cleanup(Some("w1")).expect("cleanup again");
        assert_eq!(stats2.merged_characters, 0);
        assert_eq!(stats2.merged_relationships, 0);
        assert_eq!(stats2.deleted_orphan_edges, 0);
    }
}
