use crate::db::{DbError, DbPool};
use chrono::Utc;
use rusqlite::types::Value;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::Path;
use uuid::Uuid;

#[derive(Debug)]
pub enum ForeshadowError {
    Db(DbError),
    NotFound(String),
    VersionConflict { id: String, expected: i64, actual: i64 },
    InvalidStatus(String),
    InvalidType(String),
    InvalidWeight(i32),
    DuplicateOccurrence,
    Cycle(String),
    BadRequest(String),
}

impl std::fmt::Display for ForeshadowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ForeshadowError::Db(e) => write!(f, "foreshadow db: {e}"),
            ForeshadowError::NotFound(what) => write!(f, "foreshadow {what} not found"),
            ForeshadowError::VersionConflict { id, expected, actual } => {
                write!(f, "version conflict for {id}: expected {expected}, got {actual}")
            }
            ForeshadowError::InvalidStatus(s) => write!(f, "invalid status: {s}"),
            ForeshadowError::InvalidType(t) => write!(f, "invalid type: {t}"),
            ForeshadowError::InvalidWeight(w) => write!(f, "invalid weight: {w} (expected 1..=10)"),
            ForeshadowError::DuplicateOccurrence => write!(f, "occurrence already exists for this foreshadow and chapter+type"),
            ForeshadowError::Cycle(id) => write!(f, "dependency cycle detected involving foreshadow {id}"),
            ForeshadowError::BadRequest(msg) => write!(f, "bad request: {msg}"),
        }
    }
}

impl std::error::Error for ForeshadowError {}

impl From<DbError> for ForeshadowError {
    fn from(e: DbError) -> Self {
        ForeshadowError::Db(e)
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ChapterOutline {
    pub chapter_id: String,
    pub work_id: String,
    pub goal: String,
    pub conflicts: Vec<String>,
    pub twists: Vec<String>,
    pub change_note: String,
    pub expected_version_no: i64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Occurrence {
    pub id: String,
    pub foreshadow_id: String,
    pub chapter_id: String,
    #[serde(rename = "type")]
    pub typ: String,
    pub note: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Foreshadow {
    pub id: String,
    pub work_id: String,
    pub title: String,
    pub description: String,
    pub status: String,
    /// 伏笔权重 1..=10，默认 5。
    #[serde(default = "default_weight")]
    pub weight: i32,
    /// 依赖的伏笔 id 列表（本伏笔依赖这些父伏笔）。
    #[serde(default)]
    pub parent_ids: Vec<String>,
    pub expected_version_no: i64,
    pub occurrences: Vec<Occurrence>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ForeshadowStats {
    pub total: i64,
    pub by_status: BTreeMap<String, i64>,
    pub average_weight: f64,
}

fn default_weight() -> i32 {
    5
}

fn now() -> String {
    Utc::now().to_rfc3339()
}

fn parse_parent_ids(s: &str) -> Vec<String> {
    serde_json::from_str(s).unwrap_or_default()
}

fn row_outline(r: &Row<'_>) -> rusqlite::Result<ChapterOutline> {
        let conflicts_json: String = r.get(3)?;
        let twists_json: String = r.get(4)?;
        Ok(ChapterOutline {
            chapter_id: r.get(0)?,
            work_id: r.get(1)?,
            goal: r.get(2)?,
            conflicts: serde_json::from_str(&conflicts_json).unwrap_or_default(),
            twists: serde_json::from_str(&twists_json).unwrap_or_default(),
            change_note: r.get(5)?,
            expected_version_no: r.get(6)?,
            created_at: r.get(7)?,
            updated_at: r.get(8)?,
        })
}

fn row_foreshadow(r: &Row<'_>) -> rusqlite::Result<Foreshadow> {
    let parent_ids: Option<String> = r.get(6)?;
    Ok(Foreshadow {
        id: r.get(0)?,
        work_id: r.get(1)?,
        title: r.get(2)?,
        description: r.get(3)?,
        status: r.get(4)?,
        weight: r.get(5)?,
        parent_ids: parse_parent_ids(parent_ids.as_deref().unwrap_or("[]")),
        expected_version_no: r.get(7)?,
        occurrences: vec![],
        created_at: r.get(8)?,
        updated_at: r.get(9)?,
    })
}

fn table_has_column(conn: &Connection, table: &str, column: &str) -> Result<bool, ForeshadowError> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(DbError::Migrate)?;
    let cols = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .map_err(|e| ForeshadowError::Db(DbError::Migrate(e)))?;
    for c in cols {
        let name = c.map_err(|e| ForeshadowError::Db(DbError::Migrate(e)))?;
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Kahn 拓扑排序检测有向环。`parents` 为 节点 id -> 其父伏笔 id 列表；
/// 边方向为 parent -> child（child 依赖 parent）。返回 true 表示存在环。
fn graph_has_cycle(parents: &HashMap<String, Vec<String>>) -> bool {
    if parents.is_empty() {
        return false;
    }
    let mut children: HashMap<&String, Vec<&String>> = HashMap::new();
    for (child, ps) in parents {
        for p in ps {
            if parents.contains_key(p) {
                children.entry(p).or_default().push(child);
            }
        }
    }
    let mut indeg: HashMap<&String, usize> = parents.keys().map(|k| (k, parents[k].len())).collect();
    let mut queue: VecDeque<&String> = indeg
        .iter()
        .filter(|(_, d)| **d == 0)
        .map(|(k, _)| *k)
        .collect();
    let mut processed = 0usize;
    while let Some(n) = queue.pop_front() {
        processed += 1;
        if let Some(cs) = children.get(n) {
            for &ch in cs {
                let d = indeg.get_mut(ch).expect("child always present in indeg");
                *d -= 1;
                if *d == 0 {
                    queue.push_back(ch);
                }
            }
        }
    }
    processed < parents.len()
}

#[derive(Clone)]
pub struct ForeshadowStore {
    pool: DbPool,
}

impl ForeshadowStore {
    pub fn open(path: &Path) -> Result<Self, ForeshadowError> {
        let pool = DbPool::open(path, 4).map_err(ForeshadowError::Db)?;
        Self::ensure_schema(&pool)?;
        Ok(ForeshadowStore { pool })
    }

    pub fn open_in_memory() -> Result<Self, ForeshadowError> {
        let pool = DbPool::open_in_memory(4).map_err(ForeshadowError::Db)?;
        Self::ensure_schema(&pool)?;
        Ok(ForeshadowStore { pool })
    }

    /// 自迁移：为 `foreshadows` 表补齐 DAG 扩展列（weight / parent_ids）。
    /// 旧库通过 `ALTER TABLE ... ADD COLUMN` 升级；旧记录自动获得默认值（weight=5，无依赖）。
    fn ensure_schema(pool: &DbPool) -> Result<(), ForeshadowError> {
        let mut c = pool.get().map_err(ForeshadowError::Db)?;
        Self::ensure_foreshadow_columns(c.conn())
    }

    fn ensure_foreshadow_columns(conn: &mut Connection) -> Result<(), ForeshadowError> {
        if !table_has_column(conn, "foreshadows", "weight")? {
            conn.execute_batch("ALTER TABLE foreshadows ADD COLUMN weight INTEGER NOT NULL DEFAULT 5;")
                .map_err(DbError::Migrate)?;
        }
        if !table_has_column(conn, "foreshadows", "parent_ids")? {
            conn.execute_batch("ALTER TABLE foreshadows ADD COLUMN parent_ids TEXT NOT NULL DEFAULT '[]';")
                .map_err(DbError::Migrate)?;
        }
        Ok(())
    }

    fn conn(&self) -> Result<crate::db::DbConn, ForeshadowError> {
        self.pool.get().map_err(ForeshadowError::Db)
    }

    fn get_occurrences_for_foreshadow(&self, foreshadow_id: &str) -> Result<Vec<Occurrence>, ForeshadowError> {
        let mut c = self.conn()?;
        let conn = c.conn();
        let mut stmt = conn.prepare(
            "SELECT id, foreshadow_id, chapter_id, type, note, created_at, updated_at 
             FROM foreshadow_occurrences WHERE foreshadow_id=?1 ORDER BY created_at"
        ).map_err(DbError::Migrate)?;
        let rows = stmt.query_map([foreshadow_id], |r| {
            Ok(Occurrence {
                id: r.get(0)?,
                foreshadow_id: r.get(1)?,
                chapter_id: r.get(2)?,
                typ: r.get(3)?,
                note: r.get(4)?,
                created_at: r.get(5)?,
                updated_at: r.get(6)?,
            })
        }).map_err(|e| ForeshadowError::Db(DbError::Migrate(e)))?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|e| ForeshadowError::Db(DbError::Migrate(e)))
    }

    fn build_foreshadow(&self, id: &str) -> Result<Foreshadow, ForeshadowError> {
        let mut c = self.conn()?;
        let conn = c.conn();
        let row = conn.query_row(
            "SELECT id, work_id, title, description, status, weight, parent_ids, expected_version_no, created_at, updated_at 
             FROM foreshadows WHERE id=?1",
            [id],
            row_foreshadow,
        ).map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => ForeshadowError::NotFound(format!("foreshadow {id}")),
            other => ForeshadowError::Db(DbError::Migrate(other)),
        })?;
        let occurrences = self.get_occurrences_for_foreshadow(id)?;
        Ok(Foreshadow { occurrences, ..row })
    }

    fn get_occurrence(&self, id: &str) -> Result<Occurrence, ForeshadowError> {
        let mut c = self.conn()?;
        let conn = c.conn();
        let mut stmt = conn.prepare(
            "SELECT id, foreshadow_id, chapter_id, type, note, created_at, updated_at 
             FROM foreshadow_occurrences WHERE id=?1"
        ).map_err(DbError::Migrate)?;
        stmt.query_row([id], |r| {
            Ok(Occurrence {
                id: r.get(0)?,
                foreshadow_id: r.get(1)?,
                chapter_id: r.get(2)?,
                typ: r.get(3)?,
                note: r.get(4)?,
                created_at: r.get(5)?,
                updated_at: r.get(6)?,
            })
        }).map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => ForeshadowError::NotFound(format!("occurrence {id}")),
            other => ForeshadowError::Db(DbError::Migrate(other)),
        })
    }

    pub fn get_outline(&self, work_id: &str, chapter_id: &str) -> Result<Option<ChapterOutline>, ForeshadowError> {
        let mut c = self.conn()?;
        let conn = c.conn();
        let mut stmt = conn.prepare(
            "SELECT chapter_id, work_id, goal, conflicts, twists, change_note, expected_version_no, created_at, updated_at 
             FROM chapter_outlines WHERE chapter_id=?1 AND work_id=?2"
        ).map_err(DbError::Migrate)?;
        let rows = stmt.query_map([chapter_id, work_id], row_outline).map_err(|e| ForeshadowError::Db(DbError::Migrate(e)))?;
        let mut outlines = Vec::new();
        for r in rows {
            outlines.push(r.map_err(|e| ForeshadowError::Db(DbError::Migrate(e)))?);
        }
        Ok(outlines.pop())
    }

    pub fn list_outlines(&self, work_id: &str) -> Result<Vec<ChapterOutline>, ForeshadowError> {
        let mut c = self.conn()?;
        let conn = c.conn();
        let mut stmt = conn.prepare(
            "SELECT chapter_id, work_id, goal, conflicts, twists, change_note, expected_version_no, created_at, updated_at 
             FROM chapter_outlines WHERE work_id=?1 ORDER BY chapter_id"
        ).map_err(DbError::Migrate)?;
        let rows = stmt.query_map([work_id], row_outline).map_err(|e| ForeshadowError::Db(DbError::Migrate(e)))?;
        let outlines: Vec<ChapterOutline> = rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|e| ForeshadowError::Db(DbError::Migrate(e)))?;
        Ok(outlines)
    }

    pub fn upsert_outline(&self, work_id: &str, chapter_id: &str, goal: String, conflicts: Vec<String>, twists: Vec<String>, change_note: String, expected_version_no: Option<i64>) -> Result<ChapterOutline, ForeshadowError> {
        let mut c = self.conn()?;
        let conn = c.conn();
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM chapter_outlines WHERE chapter_id=?1 AND work_id=?2)",
                params![chapter_id, work_id],
                |r| r.get(0),
            )
            .map_err(DbError::Migrate)?;
        let ts = now();
        if !exists {
            let json_conflicts = serde_json::to_string(&conflicts).unwrap_or_else(|_| "[]".into());
            let json_twists = serde_json::to_string(&twists).unwrap_or_else(|_| "[]".into());
            conn.execute(
                "INSERT INTO chapter_outlines (chapter_id, work_id, goal, conflicts, twists, change_note, expected_version_no, created_at, updated_at) 
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7, ?7)",
                params![chapter_id, work_id, goal, json_conflicts, json_twists, change_note, ts]
            ).map_err(DbError::Migrate)?;
            return Ok(self.get_outline(work_id, chapter_id)?.unwrap());
        }
        // update
        let current_version: i64 = conn
            .query_row(
                "SELECT expected_version_no FROM chapter_outlines WHERE chapter_id=?1 AND work_id=?2",
                params![chapter_id, work_id],
                |r| r.get(0),
            )
            .map_err(DbError::Migrate)?;
        if let Some(v) = expected_version_no {
            if v != current_version {
                return Err(ForeshadowError::VersionConflict {
                    id: chapter_id.to_string(),
                    expected: v,
                    actual: current_version,
                });
            }
        }
        let json_conflicts = serde_json::to_string(&conflicts).unwrap_or_else(|_| "[]".into());
        let json_twists = serde_json::to_string(&twists).unwrap_or_else(|_| "[]".into());
        conn.execute(
            "UPDATE chapter_outlines SET goal=?3, conflicts=?4, twists=?5, change_note=?6, expected_version_no=?7, updated_at=?8 
             WHERE chapter_id=?1 AND work_id=?2",
            params![chapter_id, work_id, goal, json_conflicts, json_twists, change_note, current_version + 1, ts]
        ).map_err(DbError::Migrate)?;
        return Ok(self.get_outline(work_id, chapter_id)?.unwrap());
    }

    pub fn delete_outline(&self, work_id: &str, chapter_id: &str, expected_version_no: Option<i64>) -> Result<(), ForeshadowError> {
        let mut c = self.conn()?;
        let conn = c.conn();
        let current_version: i64 = conn
            .query_row(
                "SELECT expected_version_no FROM chapter_outlines WHERE chapter_id=?1 AND work_id=?2",
                params![chapter_id, work_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(DbError::Migrate)?
            .ok_or_else(|| ForeshadowError::NotFound(format!("outline {chapter_id}")))?;
        if let Some(v) = expected_version_no {
            if v != current_version {
                return Err(ForeshadowError::VersionConflict {
                    id: chapter_id.to_string(),
                    expected: v,
                    actual: current_version,
                });
            }
        }
        let n = conn
            .execute(
                "DELETE FROM chapter_outlines WHERE chapter_id=?1 AND work_id=?2",
                params![chapter_id, work_id],
            )
            .map_err(DbError::Migrate)?;
        if n == 0 {
            return Err(ForeshadowError::NotFound(format!("outline {chapter_id}")));
        }
        Ok(())
    }

    pub fn list_foreshadows(&self, work_id: &str, status: Option<&str>, weight_min: Option<i32>) -> Result<Vec<Foreshadow>, ForeshadowError> {
        let mut c = self.conn()?;
        let conn = c.conn();
        let sql = if let Some(_s) = status {
            "SELECT id, work_id, title, description, status, weight, parent_ids, expected_version_no, created_at, updated_at 
             FROM foreshadows WHERE work_id=?1 AND status=?2 ORDER BY created_at DESC, id ASC"
        } else {
            "SELECT id, work_id, title, description, status, weight, parent_ids, expected_version_no, created_at, updated_at 
             FROM foreshadows WHERE work_id=?1 ORDER BY created_at DESC, id ASC"
        };
        let mut stmt = conn.prepare(sql).map_err(DbError::Migrate)?;
        let rows = if let Some(s) = status {
            stmt.query_map([work_id, s], row_foreshadow).map_err(|e| ForeshadowError::Db(DbError::Migrate(e)))?
        } else {
            stmt.query_map([work_id], row_foreshadow).map_err(|e| ForeshadowError::Db(DbError::Migrate(e)))?
        };
        let mut fs = Vec::new();
        for r in rows {
            let mut f = r.map_err(|e| ForeshadowError::Db(DbError::Migrate(e)))?;
            if let Some(w) = weight_min {
                if f.weight < w {
                    continue;
                }
            }
            f.occurrences = self.get_occurrences_for_foreshadow(&f.id)?;
            fs.push(f);
        }
        Ok(fs)
    }

    pub fn get_foreshadow(&self, id: &str) -> Result<Option<Foreshadow>, ForeshadowError> {
        match self.build_foreshadow(id) {
            Ok(f) => Ok(Some(f)),
            Err(ForeshadowError::NotFound(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn create_foreshadow(&self, work_id: &str, title: String, description: String, status: String) -> Result<Foreshadow, ForeshadowError> {
        if !matches!(status.as_str(), "planted" | "active" | "recalled") {
            return Err(ForeshadowError::InvalidStatus(status));
        }
        if title.trim().is_empty() {
            return Err(ForeshadowError::BadRequest("title cannot be empty".into()));
        }
        let mut c = self.conn()?;
        let conn = c.conn();
        let id = Uuid::new_v4().to_string();
        let ts = now();
        conn.execute(
            "INSERT INTO foreshadows (id, work_id, title, description, status, weight, parent_ids, expected_version_no, created_at, updated_at) 
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1, ?8, ?8)",
            params![id, work_id, title, description, status, 5, "[]", ts]
        ).map_err(DbError::Migrate)?;
        self.build_foreshadow(&id)
    }

    pub fn update_foreshadow(
        &self,
        id: &str,
        title: Option<String>,
        description: Option<String>,
        status: Option<String>,
        weight: Option<i32>,
        parents: Option<Vec<String>>,
        expected_version_no: Option<i64>,
    ) -> Result<Foreshadow, ForeshadowError> {
        let mut c = self.conn()?;
        let conn = c.conn();
        let current_version: i64 = conn
            .query_row("SELECT expected_version_no FROM foreshadows WHERE id=?1", [id], |r| r.get(0))
            .optional()
            .map_err(DbError::Migrate)?
            .ok_or_else(|| ForeshadowError::NotFound(format!("foreshadow {id}")))?;
        if let Some(v) = expected_version_no {
            if v != current_version {
                return Err(ForeshadowError::VersionConflict {
                    id: id.to_string(),
                    expected: v,
                    actual: current_version,
                });
            }
        }
        if let Some(ref s) = status {
            if !matches!(s.as_str(), "planted" | "active" | "recalled") {
                return Err(ForeshadowError::InvalidStatus(s.clone()));
            }
        }
        if let Some(w) = weight {
            if !(1..=10).contains(&w) {
                return Err(ForeshadowError::InvalidWeight(w));
            }
        }
        // Parents（整表替换）：去空/去重/去自引用，并保持 DAG 无环。
        let parent_ids: Option<Vec<String>> = parents.map(|p| {
            let mut out: Vec<String> = Vec::new();
            for pid in p {
                let pid = pid.trim().to_string();
                if pid.is_empty() || pid == id {
                    continue;
                }
                if !out.contains(&pid) {
                    out.push(pid);
                }
            }
            out
        });
        if let Some(ref ps) = parent_ids {
            self.validate_parents(conn, id, ps)?;
        }
        let ts = now();
        let new_version = current_version + 1;
        let parent_json = parent_ids
            .as_ref()
            .map(|p| serde_json::to_string(p).unwrap_or_else(|_| "[]".into()));
        let params = vec![
            title.map(Value::from).unwrap_or(Value::Null),
            description.map(Value::from).unwrap_or(Value::Null),
            status.map(Value::from).unwrap_or(Value::Null),
            weight.map(Value::from).unwrap_or(Value::Null),
            parent_json.map(Value::from).unwrap_or(Value::Null),
            Value::from(new_version),
            Value::from(ts),
            Value::Text(id.to_string()),
        ];
        conn.execute(
            "UPDATE foreshadows SET 
                title = COALESCE(?, title),
                description = COALESCE(?, description),
                status = COALESCE(?, status),
                weight = COALESCE(?, weight),
                parent_ids = COALESCE(?, parent_ids),
                expected_version_no = ?,
                updated_at = ?
             WHERE id = ?",
            rusqlite::params_from_iter(params.iter()),
        ).map_err(DbError::Migrate)?;
        self.build_foreshadow(id)
    }

    /// 校验并写入一条依赖边：`id` 依赖 `parent_id`。
    pub fn set_dependency(&self, id: &str, parent_id: &str, expected_version_no: Option<i64>) -> Result<Foreshadow, ForeshadowError> {
        let mut c = self.conn()?;
        let conn = c.conn();
        if id == parent_id {
            return Err(ForeshadowError::BadRequest("a foreshadow cannot depend on itself".to_string()));
        }
        let current_version: i64 = self._expect_version(conn, id)?;
        if let Some(v) = expected_version_no {
            if v != current_version {
                return Err(ForeshadowError::VersionConflict {
                    id: id.to_string(),
                    expected: v,
                    actual: current_version,
                });
            }
        }
        if !self._foreshadow_exists(conn, parent_id)? {
            return Err(ForeshadowError::NotFound(format!("foreshadow {parent_id}")));
        }
        let current_json: String = conn
            .query_row("SELECT parent_ids FROM foreshadows WHERE id=?1", [id], |r| r.get(0))
            .map_err(DbError::Migrate)?;
        let mut parents = parse_parent_ids(&current_json);
        if parents.iter().any(|p| p == parent_id) {
            return Err(ForeshadowError::BadRequest("dependency already exists".to_string()));
        }
        parents.push(parent_id.to_string());
        let mut graph = self._load_parent_ids(conn)?;
        graph.insert(id.to_string(), parents.clone());
        if graph_has_cycle(&graph) {
            return Err(ForeshadowError::Cycle(id.to_string()));
        }
        let new_version = current_version + 1;
        let json = serde_json::to_string(&parents).unwrap_or_else(|_| "[]".into());
        conn.execute(
            "UPDATE foreshadows SET parent_ids=?1, expected_version_no=?2, updated_at=?3 WHERE id=?4",
            params![json, new_version, now(), id],
        ).map_err(DbError::Migrate)?;
        self.build_foreshadow(id)
    }

    /// 移除一条依赖边：`id` 不再依赖 `parent_id`。
    pub fn remove_dependency(&self, id: &str, parent_id: &str, expected_version_no: Option<i64>) -> Result<Foreshadow, ForeshadowError> {
        let mut c = self.conn()?;
        let conn = c.conn();
        let current_version: i64 = self._expect_version(conn, id)?;
        if let Some(v) = expected_version_no {
            if v != current_version {
                return Err(ForeshadowError::VersionConflict {
                    id: id.to_string(),
                    expected: v,
                    actual: current_version,
                });
            }
        }
        let current_json: String = conn
            .query_row("SELECT parent_ids FROM foreshadows WHERE id=?1", [id], |r| r.get(0))
            .map_err(DbError::Migrate)?;
        let mut parents = parse_parent_ids(&current_json);
        let before = parents.len();
        parents.retain(|p| p != parent_id);
        if parents.len() == before {
            return Err(ForeshadowError::NotFound(format!("dependency {parent_id} for foreshadow {id}")));
        }
        let new_version = current_version + 1;
        let json = serde_json::to_string(&parents).unwrap_or_else(|_| "[]".into());
        conn.execute(
            "UPDATE foreshadows SET parent_ids=?1, expected_version_no=?2, updated_at=?3 WHERE id=?4",
            params![json, new_version, now(), id],
        ).map_err(DbError::Migrate)?;
        self.build_foreshadow(id)
    }

    /// 返回 `id` 依赖的伏笔 id 列表（入边）。
    pub fn get_dependencies(&self, id: &str) -> Result<Vec<String>, ForeshadowError> {
        let mut c = self.conn()?;
        let conn = c.conn();
        let current_json: String = conn
            .query_row("SELECT parent_ids FROM foreshadows WHERE id=?1", [id], |r| r.get(0))
            .optional()
            .map_err(DbError::Migrate)?
            .ok_or_else(|| ForeshadowError::NotFound(format!("foreshadow {id}")))?;
        Ok(parse_parent_ids(&current_json))
    }

    /// 返回直接依赖 `id` 的伏笔 id 列表（反向边 / 出边指向）。
    pub fn get_dependents(&self, id: &str) -> Result<Vec<String>, ForeshadowError> {
        let mut c = self.conn()?;
        let conn = c.conn();
        let mut stmt = conn
            .prepare("SELECT id, parent_ids FROM foreshadows")
            .map_err(DbError::Migrate)?;
        let rows = stmt
            .query_map([], |r| {
                let s: String = r.get(1)?;
                Ok((r.get::<_, String>(0)?, parse_parent_ids(&s)))
            })
            .map_err(|e| ForeshadowError::Db(DbError::Migrate(e)))?;
        let mut out: Vec<String> = Vec::new();
        for r in rows {
            let (fid, parents) = r.map_err(|e| ForeshadowError::Db(DbError::Migrate(e)))?;
            if parents.iter().any(|p| p == id) {
                out.push(fid);
            }
        }
        out.sort();
        Ok(out)
    }

    /// 按状态分组统计 + 平均权重，供前端展示。
    pub fn foreshadow_stats(&self, work_id: &str) -> Result<ForeshadowStats, ForeshadowError> {
        let mut c = self.conn()?;
        let conn = c.conn();
        let mut stmt = conn
            .prepare("SELECT status, weight FROM foreshadows WHERE work_id=?1")
            .map_err(DbError::Migrate)?;
        let rows = stmt
            .query_map([work_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1))))
            .map_err(|e| ForeshadowError::Db(DbError::Migrate(e)))?;
        let mut by_status: BTreeMap<String, i64> = BTreeMap::new();
        for key in ["planted", "active", "recalled"] {
            by_status.insert(key.to_string(), 0);
        }
        let mut total: i64 = 0;
        let mut weight_sum: i64 = 0;
        for r in rows {
            let (status, w) = r.map_err(|e| ForeshadowError::Db(DbError::Migrate(e)))?;
            *by_status.entry(status).or_insert(0) += 1;
            total += 1;
            let w = w.map_err(|e| ForeshadowError::Db(DbError::Migrate(e)))?;
            weight_sum += w;
        }
        let average_weight = if total > 0 { weight_sum as f64 / total as f64 } else { 0.0 };
        Ok(ForeshadowStats { total, by_status, average_weight })
    }

    fn _expect_version(&self, conn: &mut Connection, id: &str) -> Result<i64, ForeshadowError> {
        conn.query_row("SELECT expected_version_no FROM foreshadows WHERE id=?1", [id], |r| r.get(0))
            .optional()
            .map_err(DbError::Migrate)?
            .ok_or_else(|| ForeshadowError::NotFound(format!("foreshadow {id}")))
    }

    fn _foreshadow_exists(&self, conn: &Connection, id: &str) -> Result<bool, ForeshadowError> {
        conn.query_row("SELECT EXISTS(SELECT 1 FROM foreshadows WHERE id=?1)", [id], |r| r.get(0))
            .map_err(|e| ForeshadowError::Db(DbError::Migrate(e)))
    }

    fn _load_parent_ids(&self, conn: &Connection) -> Result<HashMap<String, Vec<String>>, ForeshadowError> {
        let mut stmt = conn
            .prepare("SELECT id, parent_ids FROM foreshadows")
            .map_err(DbError::Migrate)?;
        let rows = stmt
            .query_map([], |r| {
                let s: String = r.get(1)?;
                Ok((r.get::<_, String>(0)?, parse_parent_ids(&s)))
            })
            .map_err(|e| ForeshadowError::Db(DbError::Migrate(e)))?;
        let mut map: HashMap<String, Vec<String>> = HashMap::new();
        for r in rows {
            let (id, parents) = r.map_err(|e| ForeshadowError::Db(DbError::Migrate(e)))?;
            map.insert(id, parents);
        }
        Ok(map)
    }

    /// 校验父伏笔全部存在，并基于候选 parent_ids 做整图环检测。
    /// 不落库：仅验证。存在环时返回 [ForeshadowError::Cycle]。
    fn validate_parents(&self, conn: &mut Connection, id: &str, parents: &[String]) -> Result<(), ForeshadowError> {
        for pid in parents {
            if !self._foreshadow_exists(conn, pid)? {
                return Err(ForeshadowError::NotFound(format!("foreshadow {pid}")));
            }
        }
        let mut graph = self._load_parent_ids(conn)?;
        graph.insert(id.to_string(), parents.to_vec());
        if graph_has_cycle(&graph) {
            return Err(ForeshadowError::Cycle(id.to_string()));
        }
        Ok(())
    }

    pub fn delete_foreshadow(&self, id: &str, expected_version_no: Option<i64>) -> Result<(), ForeshadowError> {
        let mut c = self.conn()?;
        let conn = c.conn();
        let current_version: i64 = conn
            .query_row("SELECT expected_version_no FROM foreshadows WHERE id=?1", [id], |r| r.get(0))
            .optional()
            .map_err(DbError::Migrate)?
            .ok_or_else(|| ForeshadowError::NotFound(format!("foreshadow {id}")))?;
        if let Some(v) = expected_version_no {
            if v != current_version {
                return Err(ForeshadowError::VersionConflict {
                    id: id.to_string(),
                    expected: v,
                    actual: current_version,
                });
            }
        }
        let n = conn
            .execute("DELETE FROM foreshadows WHERE id=?1", [id])
            .map_err(DbError::Migrate)?;
        if n == 0 {
            return Err(ForeshadowError::NotFound(format!("foreshadow {id}")));
        }
        Ok(())
    }

    pub fn add_occurrence(&self, foreshadow_id: &str, chapter_id: &str, typ: String, note: String, expected_version_no: Option<i64>) -> Result<Occurrence, ForeshadowError> {
        if !matches!(typ.as_str(), "plant" | "remind" | "recover") {
            return Err(ForeshadowError::InvalidType(typ));
        }
        let mut c = self.conn()?;
        let conn = c.conn();
        let current_version: i64 = conn
            .query_row("SELECT expected_version_no FROM foreshadows WHERE id=?1", [foreshadow_id], |r| r.get(0))
            .optional()
            .map_err(DbError::Migrate)?
            .ok_or_else(|| ForeshadowError::NotFound(format!("foreshadow {foreshadow_id}")))?;
        if let Some(v) = expected_version_no {
            if v != current_version {
                return Err(ForeshadowError::VersionConflict {
                    id: foreshadow_id.to_string(),
                    expected: v,
                    actual: current_version,
                });
            }
        }
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM foreshadow_occurrences WHERE foreshadow_id=?1 AND chapter_id=?2 AND type=?3)",
                params![foreshadow_id, chapter_id, typ],
                |r| r.get(0),
            )
            .map_err(DbError::Migrate)?;
        if exists {
            return Err(ForeshadowError::DuplicateOccurrence);
        }
        let id = Uuid::new_v4().to_string();
        let ts = now();
        conn.execute(
            "INSERT INTO foreshadow_occurrences (id, foreshadow_id, chapter_id, type, note, created_at, updated_at) 
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
            params![id, foreshadow_id, chapter_id, typ, note, ts]
        ).map_err(DbError::Migrate)?;
        let ts2 = now();
        conn.execute(
            "UPDATE foreshadows SET expected_version_no = expected_version_no + 1, updated_at = ?1 WHERE id = ?2",
            params![ts2, foreshadow_id],
        ).map_err(DbError::Migrate)?;
        self.get_occurrence(&id)
    }

    pub fn remove_occurrence(&self, foreshadow_id: &str, occurrence_id: &str, expected_version_no: Option<i64>) -> Result<(), ForeshadowError> {
        let mut c = self.conn()?;
        let conn = c.conn();
        let current_version: i64 = conn
            .query_row("SELECT expected_version_no FROM foreshadows WHERE id=?1", [foreshadow_id], |r| r.get(0))
            .optional()
            .map_err(DbError::Migrate)?
            .ok_or_else(|| ForeshadowError::NotFound(format!("foreshadow {foreshadow_id}")))?;
        if let Some(v) = expected_version_no {
            if v != current_version {
                return Err(ForeshadowError::VersionConflict {
                    id: foreshadow_id.to_string(),
                    expected: v,
                    actual: current_version,
                });
            }
        }
        let n = conn
            .execute("DELETE FROM foreshadow_occurrences WHERE id=?1", [occurrence_id])
            .map_err(DbError::Migrate)?;
        if n == 0 {
            return Err(ForeshadowError::NotFound(format!("occurrence {occurrence_id}")));
        }
        let ts = now();
        conn.execute(
            "UPDATE foreshadows SET expected_version_no = expected_version_no + 1, updated_at = ?1 WHERE id = ?2",
            params![ts, foreshadow_id],
        ).map_err(DbError::Migrate)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> ForeshadowStore {
        ForeshadowStore::open_in_memory().expect("open in-memory foreshadow store")
    }

    #[test]
    fn upsert_outline_insert() {
        let s = store();
        let co = s.upsert_outline("w1", "c1", "goal1".into(), vec![], vec![], "".into(), Some(0)).expect("insert");
        assert_eq!(co.expected_version_no, 1);
        assert_eq!(co.goal, "goal1");
        assert_eq!(co.conflicts, Vec::<String>::new());
        assert_eq!(co.twists, Vec::<String>::new());
        assert_eq!(co.change_note, "");
    }

    #[test]
    fn upsert_outline_update_matching_version() {
        let s = store();
        let co1 = s.upsert_outline("w1", "c1", "goal1".into(), vec![], vec![], "".into(), Some(0)).expect("insert");
        let co2 = s.upsert_outline("w1", "c1", "goal2".into(), vec!["conflict".into()], vec!["twist".into()], "note".into(), Some(co1.expected_version_no)).expect("update");
        assert_eq!(co2.expected_version_no, 2);
        assert_eq!(co2.goal, "goal2");
        assert_eq!(co2.conflicts, vec!["conflict".to_string()]);
        assert_eq!(co2.twists, vec!["twist".to_string()]);
        assert_eq!(co2.change_note, "note");
    }

    #[test]
    fn upsert_outline_version_conflict() {
        let s = store();
        let co1 = s.upsert_outline("w1", "c1", "goal1".into(), vec![], vec![], "".into(), Some(0)).expect("insert");
        let err = s.upsert_outline("w1", "c1", "goal2".into(), vec![], vec![], "".into(), Some(co1.expected_version_no - 1)).expect_err("should conflict");
        assert!(matches!(err, ForeshadowError::VersionConflict { .. }));
    }

    #[test]
    fn delete_outline() {
        let s = store();
        let co = s.upsert_outline("w1", "c1", "goal1".into(), vec![], vec![], "".into(), Some(0)).expect("insert");
        s.delete_outline("w1", "c1", Some(co.expected_version_no)).expect("delete");
        assert!(s.get_outline("w1", "c1").expect("get").is_none());
    }

    #[test]
    fn delete_outline_version_conflict() {
        let s = store();
        let co = s.upsert_outline("w1", "c1", "goal1".into(), vec![], vec![], "".into(), Some(0)).expect("insert");
        let err = s.delete_outline("w1", "c1", Some(co.expected_version_no - 1)).expect_err("should conflict");
        assert!(matches!(err, ForeshadowError::VersionConflict { .. }));
        // after wrong delete, still exists
        assert!(s.get_outline("w1", "c1").expect("get").is_some());
    }

    #[test]
    fn foreshadow_lifecycle() {
        let s = store();
        let f = s.create_foreshadow("w1", "title1".into(), "desc1".into(), "planted".into()).expect("create");
        assert_eq!(f.status, "planted");
        assert_eq!(f.title, "title1");
        let fs = s.list_foreshadows("w1", None, None).expect("list");
        assert_eq!(fs.len(), 1);
        assert_eq!(fs[0].id, f.id);
        let g = s.get_foreshadow(&f.id).expect("get").expect("some");
        assert_eq!(g.id, f.id);
        let f2 = s.update_foreshadow(&f.id, Some("title2".into()), Some("desc2".into()), Some("active".into()), None, None, Some(f.expected_version_no)).expect("update");
        assert_eq!(f2.status, "active");
        assert_eq!(f2.title, "title2");
        assert_eq!(f2.expected_version_no, 2);
        s.delete_foreshadow(&f2.id, Some(f2.expected_version_no)).expect("delete");
        assert!(s.get_foreshadow(&f2.id).expect("get").is_none());
    }

    #[test]
    fn foreshadow_status_validation() {
        let s = store();
        let err = s.create_foreshadow("w1", "title".into(), "".into(), "invalid".into()).expect_err("bad status");
        assert!(matches!(err, ForeshadowError::InvalidStatus(_)));
        let f = s.create_foreshadow("w1", "title".into(), "".into(), "planted".into()).expect("create");
        let err = s.update_foreshadow(&f.id, None, None, Some("invalid".into()), None, None, Some(f.expected_version_no)).expect_err("bad status update");
        assert!(matches!(err, ForeshadowError::InvalidStatus(_)));
    }

    #[test]
    fn add_occurrence_duplicate() {
        let s = store();
        let f = s.create_foreshadow("w1", "title".into(), "".into(), "planted".into()).expect("create");
        let _occ = s.add_occurrence(&f.id, "c1".into(), "plant".into(), "note1".into(), Some(f.expected_version_no)).expect("add");
        let err = s.add_occurrence(&f.id, "c1".into(), "plant".into(), "note2".into(), Some(f.expected_version_no + 1)).expect_err("dup");
        assert!(matches!(err, ForeshadowError::DuplicateOccurrence));
    }

    #[test]
    fn add_remove_occurrence() {
        let s = store();
        let f = s.create_foreshadow("w1", "title".into(), "".into(), "planted".into()).expect("create");
        let occ = s.add_occurrence(&f.id, "c1".into(), "plant".into(), "note1".into(), Some(f.expected_version_no)).expect("add");
        s.remove_occurrence(&f.id, &occ.id, Some(f.expected_version_no + 1)).expect("remove");
        assert!(s.get_foreshadow(&f.id).expect("get").unwrap().occurrences.is_empty());
    }

    #[test]
    fn cascade_delete_foreshadow() {
        let s = store();
        let f = s.create_foreshadow("w1", "title".into(), "".into(), "planted".into()).expect("create");
        let _occ = s.add_occurrence(&f.id, "c1".into(), "plant".into(), "note1".into(), Some(f.expected_version_no)).expect("add");
        let fresh = s.get_foreshadow(&f.id).expect("get").unwrap();
        s.delete_foreshadow(&f.id, Some(fresh.expected_version_no)).expect("delete");
        let g = s.get_foreshadow(&f.id).expect("get");
        assert!(g.is_none());
    }

    // ── T1: 伏笔 DAG（依赖 + 权重 + 环检测）──

    #[test]
    fn weight_default_and_update() {
        let s = store();
        let f = s.create_foreshadow("w1", "t".into(), "".into(), "planted".into()).expect("create");
        assert_eq!(f.weight, 5, "默认权重应为 5");
        let u = s.update_foreshadow(&f.id, None, None, None, Some(9), None, Some(f.expected_version_no)).expect("update weight");
        assert_eq!(u.weight, 9);
        let g = s.get_foreshadow(&f.id).expect("get").unwrap();
        assert_eq!(g.weight, 9, "weight 持久化");
        // 不传 weight 时保持原值
        let u2 = s.update_foreshadow(&f.id, None, None, None, None, None, Some(u.expected_version_no)).expect("no-op update");
        assert_eq!(u2.weight, 9);
    }

    #[test]
    fn weight_out_of_range_rejected() {
        let s = store();
        let f = s.create_foreshadow("w1", "t".into(), "".into(), "planted".into()).expect("create");
        let err0 = s.update_foreshadow(&f.id, None, None, None, Some(0), None, Some(f.expected_version_no)).expect_err("0 invalid");
        assert!(matches!(err0, ForeshadowError::InvalidWeight(0)));
        let f2 = s.get_foreshadow(&f.id).expect("get").unwrap();
        let err11 = s.update_foreshadow(&f.id, None, None, None, Some(11), None, Some(f2.expected_version_no)).expect_err("11 invalid");
        assert!(matches!(err11, ForeshadowError::InvalidWeight(11)));
        assert_eq!(s.get_foreshadow(&f.id).expect("get").unwrap().weight, 5, "非法权重不得落库");
    }

    #[test]
    fn list_foreshadows_weight_min_filter() {
        let s = store();
        let a = s.create_foreshadow("w1", "a".into(), "".into(), "planted".into()).expect("create");
        let b = s.create_foreshadow("w1", "b".into(), "".into(), "planted".into()).expect("create");
        let _ = s.update_foreshadow(&a.id, None, None, None, Some(3), None, Some(a.expected_version_no)).expect("w3");
        let _ = s.update_foreshadow(&b.id, None, None, None, Some(8), None, Some(b.expected_version_no)).expect("w8");
        let heavy = s.list_foreshadows("w1", None, Some(5)).expect("filter");
        assert_eq!(heavy.len(), 1);
        assert_eq!(heavy[0].id, b.id);
    }

    #[test]
    fn dependency_chain_legal() {
        let s = store();
        let a = s.create_foreshadow("w1", "a".into(), "".into(), "planted".into()).expect("create");
        let b = s.create_foreshadow("w1", "b".into(), "".into(), "planted".into()).expect("create");
        let c = s.create_foreshadow("w1", "c".into(), "".into(), "planted".into()).expect("create");
        // B 依赖 A：A -> B
        let bb = s.set_dependency(&b.id, &a.id, Some(b.expected_version_no)).expect("B<-A");
        assert_eq!(bb.parent_ids, vec![a.id.clone()]);
        // C 依赖 B：A -> B -> C
        let cc = s.set_dependency(&c.id, &b.id, Some(c.expected_version_no)).expect("C<-B");
        assert_eq!(cc.parent_ids, vec![b.id.clone()]);
        // forward query
        assert_eq!(s.get_dependencies(&c.id).expect("deps C"), vec![b.id.clone()]);
        // reverse query
        assert_eq!(s.get_dependents(&a.id).expect("deps of A"), vec![b.id.clone()]);
        assert_eq!(s.get_dependents(&b.id).expect("deps of B"), vec![c.id.clone()]);
        let graph = s.get_foreshadow(&c.id).expect("get").unwrap();
        assert_eq!(graph.parent_ids, vec![b.id.clone()]);
    }

    #[test]
    fn dependency_cycle_rejected_a_to_b_to_c_to_a() {
        let s = store();
        let a = s.create_foreshadow("w1", "a".into(), "".into(), "planted".into()).expect("a");
        let b = s.create_foreshadow("w1", "b".into(), "".into(), "planted".into()).expect("b");
        let c = s.create_foreshadow("w1", "c".into(), "".into(), "planted".into()).expect("c");
        s.set_dependency(&b.id, &a.id, Some(b.expected_version_no)).expect("B<-A");
        s.set_dependency(&c.id, &b.id, Some(c.expected_version_no)).expect("C<-B");
        // 尝试成环：A <- C（C 依赖 A 会制造 A->B->C->A）
        let err = s.set_dependency(&a.id, &c.id, Some(a.expected_version_no)).expect_err("cycle");
        assert!(matches!(err, ForeshadowError::Cycle(id) if id == a.id));
        // 完整替换成环同样被拒
        let err2 = s.update_foreshadow(&a.id, None, None, None, None, Some(vec![c.id.clone()]), None).expect_err("cycle replace");
        assert!(matches!(err2, ForeshadowError::Cycle(_)));
        // 环写不进去：A 依旧无父
        assert!(s.get_dependencies(&a.id).expect("deps of A").is_empty());
        // 原链保持完整
        assert_eq!(s.get_dependents(&b.id).expect("deps of B"), vec![c.id.clone()]);
    }

    #[test]
    fn dependency_self_and_duplicate_rejected() {
        let s = store();
        let a = s.create_foreshadow("w1", "a".into(), "".into(), "planted".into()).expect("a");
        let b = s.create_foreshadow("w1", "b".into(), "".into(), "planted".into()).expect("b");
        let err_self = s.set_dependency(&a.id, &a.id, Some(a.expected_version_no)).expect_err("self");
        assert!(matches!(err_self, ForeshadowError::BadRequest(_)));
        let b2 = s.set_dependency(&b.id, &a.id, Some(b.expected_version_no)).expect("B<-A");
        let err_dup = s.set_dependency(&b.id, &a.id, Some(b2.expected_version_no)).expect_err("dup");
        assert!(matches!(err_dup, ForeshadowError::BadRequest(_)));
        let err_missing = s.set_dependency(&b.id, "no-such-id", None).expect_err("missing parent");
        assert!(matches!(err_missing, ForeshadowError::NotFound(_)));
    }

    #[test]
    fn remove_dependency_restores_dag() {
        let s = store();
        let a = s.create_foreshadow("w1", "a".into(), "".into(), "planted".into()).expect("a");
        let b = s.create_foreshadow("w1", "b".into(), "".into(), "planted".into()).expect("b");
        s.set_dependency(&b.id, &a.id, Some(b.expected_version_no)).expect("B<-A");
        let removed = s.remove_dependency(&b.id, &a.id, Some(b.expected_version_no + 1)).expect("remove");
        assert!(removed.parent_ids.is_empty());
        assert!(s.get_dependents(&a.id).expect("deps of A").is_empty());
        // 移除不存在的边 -> NotFound
        let err = s.remove_dependency(&b.id, &a.id, Some(removed.expected_version_no)).expect_err("no edge");
        assert!(matches!(err, ForeshadowError::NotFound(_)));
    }

    #[test]
    fn parents_replace_updates_dependency_list() {
        let s = store();
        let a = s.create_foreshadow("w1", "a".into(), "".into(), "planted".into()).expect("a");
        let b = s.create_foreshadow("w1", "b".into(), "".into(), "planted".into()).expect("b");
        let c = s.create_foreshadow("w1", "c".into(), "".into(), "planted".into()).expect("c");
        let u = s.update_foreshadow(&c.id, None, None, None, None, Some(vec![a.id.clone(), b.id.clone(), a.id.clone()]), Some(c.expected_version_no)).expect("replace");
        assert_eq!(u.parent_ids.len(), 2, "自动去重");
        assert!(u.parent_ids.contains(&a.id) && u.parent_ids.contains(&b.id));
        let empty = s.update_foreshadow(&c.id, None, None, None, None, Some(vec![]), Some(u.expected_version_no)).expect("clear");
        assert!(empty.parent_ids.is_empty());
    }

    #[test]
    fn old_schema_row_has_defaults() {
        // 模拟旧库：以旧 schema 的列插入（无 weight / parent_ids），
        // 自迁移 ALTER 应让旧行读出 weight=5 且 parent_ids 为空。
        let s = store();
        {
            let mut c = s.conn().expect("conn");
            let conn = c.conn();
            conn.execute(
                "INSERT INTO foreshadows (id, work_id, title, description, status, expected_version_no, created_at, updated_at)
                 VALUES ('legacy-1', 'w1', 'old', '', 'planted', 1, '2024-01-01T00:00:00Z', '2024-01-01T00:00:00Z')",
                [],
            ).expect("raw legacy insert (columns exist with DEFAULT)");
        }
        let f = s.get_foreshadow("legacy-1").expect("get").expect("found");
        assert_eq!(f.weight, 5, "旧记录 weight 默认 5");
        assert!(f.parent_ids.is_empty(), "旧记录无依赖");
        assert_eq!(f.title, "old");
    }

    #[test]
    fn old_json_deserialize_compat() {
        // 无 weight/parent_ids 字段的旧 JSON 仍可反序列化，走 serde 默认值。
        let old = r#"{"id":"x","work_id":"w","title":"t","description":"","status":"planted","expected_version_no":1,"occurrences":[],"created_at":"2024-01-01T00:00:00Z","updated_at":"2024-01-01T00:00:00Z"}"#;
        let f: Foreshadow = serde_json::from_str(old).expect("deserialize old");
        assert_eq!(f.weight, default_weight());
        assert!(f.parent_ids.is_empty());
    }

    #[test]
    fn stats_group_by_status_and_average_weight() {
        let s = store();
        let a = s.create_foreshadow("w1", "a".into(), "".into(), "planted".into()).expect("a");
        let b = s.create_foreshadow("w1", "b".into(), "".into(), "active".into()).expect("b");
        let c = s.create_foreshadow("w1", "c".into(), "".into(), "recalled".into()).expect("c");
        let _ = s.update_foreshadow(&a.id, None, None, None, Some(10), None, Some(a.expected_version_no)).expect("w10");
        let _ = s.update_foreshadow(&b.id, None, None, None, Some(4), None, Some(b.expected_version_no)).expect("w4");
        let _ = s.update_foreshadow(&c.id, None, None, None, Some(1), None, Some(c.expected_version_no)).expect("w1");
        let stats = s.foreshadow_stats("w1").expect("stats");
        assert_eq!(stats.total, 3);
        assert_eq!(stats.by_status.get("planted"), Some(&1));
        assert_eq!(stats.by_status.get("active"), Some(&1));
        assert_eq!(stats.by_status.get("recalled"), Some(&1));
        assert!((stats.average_weight - (10.0 + 4.0 + 1.0) / 3.0).abs() < 1e-9);
        let none = s.foreshadow_stats("w-empty").expect("empty stats");
        assert_eq!(none.total, 0);
        assert_eq!(none.average_weight, 0.0);
    }
}
