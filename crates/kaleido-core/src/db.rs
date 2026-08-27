//! SQLite bootstrap for new structured domains (relationship graph, foreshadowing,
//! analysis tasks). Uses rusqlite bundled SQLite. P0: connection pool + versioned
//! migrations with an empty v1 baseline.

use rusqlite::Connection;
use std::path::Path;
use std::sync::{Arc, Mutex};

/// Pool of SQLite connections. The workspace is single-node; a Mutex pool is
/// sufficient and avoids cross-thread connection sharing (rusqlite Connection is
/// not Sync). New domains open their own pool via [DbPool::open] and keep the Arc.
#[derive(Clone)]
pub struct DbPool {
    inner: Arc<Mutex<Vec<Connection>>>,
}

impl DbPool {
    /// Open a pool over `path` (file DB). Applies pending migrations, then pre-opens
    /// `max` connections. Caller is expected to persist the pool for the domain.
    pub fn open(path: &Path, max: usize) -> Result<Self, DbError> {
        let max = max.max(1);
        let mut conns = Vec::with_capacity(max);
        for _ in 0..max {
            let conn = Connection::open(path).map_err(DbError::Open)?;
            conn.pragma_update(None, "journal_mode", "WAL").ok();
            conn.pragma_update(None, "foreign_keys", "ON").ok();
            conns.push(conn);
        }
        let pool = DbPool { inner: Arc::new(Mutex::new(conns)) };
        {
            let mut c = pool.get()?;
            migrate(c.conn())?;
        }
        Ok(pool)
    }

    /// Open a shared in-memory pool (tests / ephemeral). Connections are bound to
    /// one shared-cache database via URI, otherwise each `open_in_memory` would
    /// create an independent DB and migrations would be invisible to other conns.
    pub fn open_in_memory(max: usize) -> Result<Self, DbError> {
        use rusqlite::OpenFlags;
        use std::sync::atomic::{AtomicU64, Ordering};
        static MEM_DB_SEQ: AtomicU64 = AtomicU64::new(0);
        let max = max.max(1);
        let uri = format!(
            "file:memdb{}_{}?mode=memory&cache=shared",
            std::process::id(),
            MEM_DB_SEQ.fetch_add(1, Ordering::Relaxed)
        );
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_URI;
        let mut conns = Vec::with_capacity(max);
        for _ in 0..max {
            let conn = Connection::open_with_flags(&uri, flags).map_err(DbError::Open)?;
            conn.pragma_update(None, "journal_mode", "WAL").ok();
            conn.pragma_update(None, "foreign_keys", "ON").ok();
            conns.push(conn);
        }
        let pool = DbPool { inner: Arc::new(Mutex::new(conns)) };
        {
            let mut c = pool.get()?;
            migrate(c.conn())?;
        }
        Ok(pool)
    }

    /// Check out one connection. The mutex is released immediately after pop, so
    /// the returned guard never holds the pool lock (avoids self-deadlock on drop).
    pub fn get(&self) -> Result<DbConn, DbError> {
        let conn = {
            let mut guard = self.inner.lock().map_err(|_| DbError::Poisoned)?;
            guard.pop().ok_or(DbError::PoolExhausted)?
        };
        Ok(DbConn { conn: Some(conn), pool: self.inner.clone() })
    }
}

/// RAII checked-out connection. Returns itself to the pool on drop.
pub struct DbConn {
    conn: Option<Connection>,
    pool: Arc<Mutex<Vec<Connection>>>,
}

impl DbConn {
    pub fn conn(&mut self) -> &mut Connection {
        self.conn.as_mut().expect("connection present while checked out")
    }
}

impl Drop for DbConn {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            if let Ok(mut v) = self.pool.lock() {
                v.push(conn);
            }
        }
    }
}

/// Ordered migrations. Each entry: (version, sql). Never renumber/rewrite an
/// applied migration; append new versions. v1 is the empty baseline.
pub const MIGRATIONS: &[(i64, &str)] = &[
    (1, "-- baseline; domain tables are added by later phases (P1+)."),
    (
        2,
        "CREATE TABLE IF NOT EXISTS characters (
            id          TEXT PRIMARY KEY,
            work_id     TEXT NOT NULL,
            name        TEXT NOT NULL,
            aliases     TEXT NOT NULL DEFAULT '[]',
            note        TEXT NOT NULL DEFAULT '',
            color_idx   INTEGER NOT NULL DEFAULT 0,
            created_at  TEXT NOT NULL,
            updated_at  TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_chars_work ON characters(work_id);
        CREATE TABLE IF NOT EXISTS relationships (
            id                  TEXT PRIMARY KEY,
            work_id             TEXT NOT NULL,
            from_char           TEXT NOT NULL REFERENCES characters(id) ON DELETE CASCADE,
            to_char             TEXT NOT NULL REFERENCES characters(id) ON DELETE CASCADE,
            category            TEXT NOT NULL,
            subtype             TEXT NOT NULL DEFAULT '',
            keywords            TEXT NOT NULL DEFAULT '[]',
            confirmation_status TEXT NOT NULL DEFAULT 'pending',
            note                TEXT NOT NULL DEFAULT '',
            created_at          TEXT NOT NULL,
            updated_at          TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_rel_work ON relationships(work_id);
        CREATE INDEX IF NOT EXISTS idx_rel_pair ON relationships(from_char, to_char);",
    ),
    (
        3,
        "CREATE TABLE IF NOT EXISTS chapter_outlines (
            chapter_id          TEXT PRIMARY KEY,
            work_id             TEXT NOT NULL,
            goal                TEXT NOT NULL DEFAULT '',
            conflicts           TEXT NOT NULL DEFAULT '[]',
            twists              TEXT NOT NULL DEFAULT '[]',
            change_note         TEXT NOT NULL DEFAULT '',
            expected_version_no INTEGER NOT NULL DEFAULT 1,
            created_at          TEXT NOT NULL,
            updated_at          TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_co_work ON chapter_outlines(work_id);
        CREATE TABLE IF NOT EXISTS foreshadows (
            id                  TEXT PRIMARY KEY,
            work_id             TEXT NOT NULL,
            title               TEXT NOT NULL,
            description         TEXT NOT NULL DEFAULT '',
            status              TEXT NOT NULL DEFAULT 'planted' CHECK(status IN ('planted','active','recalled')),
            expected_version_no INTEGER NOT NULL DEFAULT 1,
            created_at          TEXT NOT NULL,
            updated_at          TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_fs_work ON foreshadows(work_id);
        CREATE TABLE IF NOT EXISTS foreshadow_occurrences (
            id            TEXT PRIMARY KEY,
            foreshadow_id TEXT NOT NULL REFERENCES foreshadows(id) ON DELETE CASCADE,
            chapter_id    TEXT NOT NULL,
            type          TEXT NOT NULL CHECK(type IN ('plant','remind','recover')),
            note          TEXT NOT NULL DEFAULT '',
            created_at    TEXT NOT NULL,
            updated_at    TEXT NOT NULL,
            UNIQUE(foreshadow_id, chapter_id, type)
        );
        CREATE INDEX IF NOT EXISTS idx_fo_fs ON foreshadow_occurrences(foreshadow_id);",
    ),
    (
        4,
        "CREATE TABLE IF NOT EXISTS analysis_tasks (
            id            TEXT PRIMARY KEY,
            work_id       TEXT NOT NULL,
            kind          TEXT NOT NULL,
            scope         TEXT NOT NULL DEFAULT '{}',
            status        TEXT NOT NULL DEFAULT 'queued' CHECK(status IN ('queued','running','succeeded','failed','cancelled')),
            summary       TEXT NOT NULL DEFAULT '{}',
            evidence      TEXT NOT NULL DEFAULT '[]',
            suggestions   TEXT NOT NULL DEFAULT '[]',
            failure       TEXT NOT NULL DEFAULT '',
            created_by    TEXT NOT NULL DEFAULT '',
            created_at    TEXT NOT NULL,
            updated_at    TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_an_work ON analysis_tasks(work_id);
        CREATE INDEX IF NOT EXISTS idx_an_status ON analysis_tasks(status);
        CREATE TABLE IF NOT EXISTS analysis_suggestions (
            id         TEXT PRIMARY KEY,
            task_id    TEXT NOT NULL REFERENCES analysis_tasks(id) ON DELETE CASCADE,
            kind       TEXT NOT NULL,
            payload    TEXT NOT NULL DEFAULT '{}',
            status     TEXT NOT NULL DEFAULT 'pending' CHECK(status IN ('pending','confirmed','rejected')),
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_ans_task ON analysis_suggestions(task_id);",
    ),
    (
        5,
        "CREATE TABLE IF NOT EXISTS ai_providers (
            id                 TEXT PRIMARY KEY,
            name               TEXT NOT NULL,
            base_url           TEXT NOT NULL,
            protocol           TEXT NOT NULL DEFAULT 'openai' CHECK(protocol IN ('openai','anthropic','google')),
            encrypted_key      TEXT NOT NULL DEFAULT '',
            key_hint           TEXT NOT NULL DEFAULT '',
            status             TEXT NOT NULL DEFAULT 'enabled' CHECK(status IN ('enabled','disabled')),
            concurrency_limit  INTEGER NOT NULL DEFAULT 10 CHECK(concurrency_limit BETWEEN 1 AND 100),
            rpm_limit          INTEGER NOT NULL DEFAULT 60 CHECK(rpm_limit BETWEEN 1 AND 10000),
            max_tokens         INTEGER NOT NULL DEFAULT 32000 CHECK(max_tokens BETWEEN 1 AND 32768),
            default_model_id   TEXT,
            note               TEXT NOT NULL DEFAULT '',
            last_error         TEXT,
            last_success_at    TEXT,
            created_at         TEXT NOT NULL,
            updated_at         TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS ai_models (
            id             TEXT PRIMARY KEY,
            provider_id    TEXT NOT NULL REFERENCES ai_providers(id) ON DELETE CASCADE,
            display_name   TEXT NOT NULL,
            model_id       TEXT NOT NULL,
            purposes_json  TEXT NOT NULL DEFAULT '[]',
            context_window INTEGER NOT NULL DEFAULT 128000,
            preset_json    TEXT NOT NULL DEFAULT '{}',
            thinking_enabled INTEGER NOT NULL DEFAULT 1,
            enabled        INTEGER NOT NULL DEFAULT 1,
            note           TEXT NOT NULL DEFAULT '',
            created_at     TEXT NOT NULL,
            updated_at     TEXT NOT NULL,
            UNIQUE(provider_id, model_id)
        );
        CREATE INDEX IF NOT EXISTS idx_aim_provider ON ai_models(provider_id);
        CREATE TABLE IF NOT EXISTS ai_calls (
            id                 TEXT PRIMARY KEY,
            provider_id        TEXT NOT NULL,
            model_id           TEXT NOT NULL,
            work_id            TEXT NOT NULL DEFAULT '',
            task_type          TEXT NOT NULL DEFAULT 'chat',
            status             TEXT NOT NULL,
            input_tokens       INTEGER NOT NULL DEFAULT 0,
            output_tokens      INTEGER NOT NULL DEFAULT 0,
            cached_input_tokens INTEGER NOT NULL DEFAULT 0,
            error              TEXT,
            created_at         TEXT NOT NULL,
            completed_at       TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_aic_provider ON ai_calls(provider_id);
        CREATE INDEX IF NOT EXISTS idx_aic_created ON ai_calls(created_at);",
    ),
    (
        6,
        "/* AI 分析确认跨 store 落库（2026-08-09 P0 闭环）:
           analysis_suggestions.applied_at/apply_error = apply 副作用追踪;
           characters.origin = 来源标记（manual|ai_suggestion）;
           relationships.suggestion_id = 幂等键（AI 建议唯一，重试不重复）;
           partial unique index（suggestion_id IS NOT NULL）允许手工边为 NULL。 */
        ALTER TABLE analysis_suggestions ADD COLUMN applied_at TEXT;
        ALTER TABLE analysis_suggestions ADD COLUMN apply_error TEXT;
        ALTER TABLE characters ADD COLUMN origin TEXT NOT NULL DEFAULT 'manual';
        ALTER TABLE relationships ADD COLUMN suggestion_id TEXT;
        CREATE UNIQUE INDEX IF NOT EXISTS idx_rel_suggestion ON relationships(suggestion_id) WHERE suggestion_id IS NOT NULL;",
    ),
    (
        7,
        "ALTER TABLE relationships ADD COLUMN chapters TEXT NOT NULL DEFAULT '[]';",
    ),
    (
        8,
        "/* U12 吞噬补充: analysis_tasks.status 增加 partial_success（LLM 结果部分丢弃时保存）。
           SQLite 不支持 ALTER CHECK，采用双表重建（外键 ON 安全）：
           建 _new 副本 → 迁移数据 → 先删旧子表再删旧父表 → RENAME 生效。 */
        CREATE TABLE analysis_tasks_new (
            id            TEXT PRIMARY KEY,
            work_id       TEXT NOT NULL,
            kind          TEXT NOT NULL,
            scope         TEXT NOT NULL DEFAULT '{}',
            status        TEXT NOT NULL DEFAULT 'queued' CHECK(status IN ('queued','running','succeeded','failed','cancelled','partial_success')),
            summary       TEXT NOT NULL DEFAULT '{}',
            evidence      TEXT NOT NULL DEFAULT '[]',
            suggestions   TEXT NOT NULL DEFAULT '[]',
            failure       TEXT NOT NULL DEFAULT '',
            created_by    TEXT NOT NULL DEFAULT '',
            created_at    TEXT NOT NULL,
            updated_at    TEXT NOT NULL
        );
        CREATE TABLE analysis_suggestions_new (
            id            TEXT PRIMARY KEY,
            task_id       TEXT NOT NULL REFERENCES analysis_tasks_new(id) ON DELETE CASCADE,
            kind          TEXT NOT NULL,
            payload       TEXT NOT NULL DEFAULT '{}',
            status        TEXT NOT NULL DEFAULT 'pending' CHECK(status IN ('pending','confirmed','rejected')),
            created_at    TEXT NOT NULL,
            updated_at    TEXT NOT NULL,
            applied_at    TEXT,
            apply_error   TEXT
        );
        INSERT INTO analysis_tasks_new (id, work_id, kind, scope, status, summary, evidence, suggestions, failure, created_by, created_at, updated_at)
            SELECT id, work_id, kind, scope, status, summary, evidence, suggestions, failure, created_by, created_at, updated_at FROM analysis_tasks;
        INSERT INTO analysis_suggestions_new (id, task_id, kind, payload, status, created_at, updated_at, applied_at, apply_error)
            SELECT id, task_id, kind, payload, status, created_at, updated_at, applied_at, apply_error FROM analysis_suggestions;
        DROP TABLE analysis_suggestions;
        DROP TABLE analysis_tasks;
        ALTER TABLE analysis_suggestions_new RENAME TO analysis_suggestions;
        ALTER TABLE analysis_tasks_new RENAME TO analysis_tasks;
        CREATE INDEX IF NOT EXISTS idx_an_work ON analysis_tasks(work_id);
        CREATE INDEX IF NOT EXISTS idx_an_status ON analysis_tasks(status);
        CREATE INDEX IF NOT EXISTS idx_ans_task ON analysis_suggestions(task_id);",
    ),
    (
        9,
        "/* [L2] 蒸馏连线: characters.distil_id = 蒸馏角色卡 id(c-distil-N) ↔ graph uuid 桥。
           resolve 优先按 (work_id, distil_id) 幂等解析, 同卡重蒸不重复建实体;
           旧行 distil_id 为 NULL, 回退 name 匹配。 */
        ALTER TABLE characters ADD COLUMN distil_id TEXT;
        CREATE INDEX IF NOT EXISTS idx_chars_distil ON characters(work_id, distil_id);",
    ),
    (
        10,
        "/* [酒馆对齐 2026-08-16] ai_providers.active = 当前激活供应商（全表唯一 active=1）。
           settings-store 的 llmBaseUrl/llmModel/llmApiKey 退役后由 active provider 派生，
           对齐 SillyTavern 的「单入口多源 + active 指针」模式。 */
        ALTER TABLE ai_providers ADD COLUMN active INTEGER NOT NULL DEFAULT 0;
        CREATE INDEX IF NOT EXISTS idx_aip_active ON ai_providers(active);",
    ),
];

fn migrate(conn: &mut Connection) -> Result<(), DbError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version INTEGER PRIMARY KEY,
            applied_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
        );",
    )
    .map_err(DbError::Migrate)?;

    for (version, sql) in MIGRATIONS {
        let applied: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version=?1)",
                [version],
                |r| r.get(0),
            )
            .map_err(DbError::Migrate)?;
        if applied {
            continue;
        }
        let tx = conn.transaction().map_err(DbError::Migrate)?;
        tx.execute_batch(sql).map_err(DbError::Migrate)?;
        tx.execute("INSERT INTO schema_migrations (version) VALUES (?1)", [version])
            .map_err(DbError::Migrate)?;
        tx.commit().map_err(DbError::Migrate)?;
    }
    Ok(())
}

#[derive(Debug)]
pub enum DbError {
    Open(rusqlite::Error),
    Migrate(rusqlite::Error),
    PoolExhausted,
    Poisoned,
}

impl std::fmt::Display for DbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DbError::Open(e) => write!(f, "db open: {e}"),
            DbError::Migrate(e) => write!(f, "db migrate: {e}"),
            DbError::PoolExhausted => write!(f, "db pool exhausted"),
            DbError::Poisoned => write!(f, "db pool lock poisoned"),
        }
    }
}

impl std::error::Error for DbError {}

#[cfg(test)]
mod db_tests {
    use super::*;

    #[test]
    fn migrations_apply_and_idempotent() {
        let pool = DbPool::open_in_memory(2).expect("open in-memory");
        {
            let mut c = pool.get().expect("checkout");
            let n: i64 = c
                .conn()
                .query_row("SELECT count(*) FROM schema_migrations", [], |r| r.get(0))
                .unwrap();
            assert_eq!(n, MIGRATIONS.len() as i64);
        }
        // Re-migrate on same pool: still one set.
        {
            let mut c = pool.get().expect("checkout");
            migrate(c.conn()).expect("re-migrate idempotent");
            let n: i64 = c
                .conn()
                .query_row("SELECT count(*) FROM schema_migrations", [], |r| r.get(0))
                .unwrap();
            assert_eq!(n, MIGRATIONS.len() as i64);
        }
    }
}
