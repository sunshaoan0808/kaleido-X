//! # AI Provider & Usage Store (P5)
//!
//! Multi-provider management + call metering for Kaleido.
//!
//! - **Providers/Models**: first-class rows (`ai_providers`, `ai_models`) so the
//!   frontend can add/remove providers & models and switch between them.
//! - **Usage meter**: append-only `ai_calls` ledger + aggregation helpers.
//!
//! API keys are stored in `encrypted_key` via a lightweight reversible XOR+hex
//! obfuscation. Handlers never return the raw key — only `key_hint`/`configured`.

use rusqlite::params;
use std::path::Path;

use crate::db::{DbConn, DbError, DbPool};

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum AiAdminError {
    Db(DbError),
    NotFound(String),
    BadRequest(String),
}

impl std::fmt::Display for AiAdminError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AiAdminError::Db(e) => write!(f, "db: {e}"),
            AiAdminError::NotFound(m) => write!(f, "not found: {m}"),
            AiAdminError::BadRequest(m) => write!(f, "bad request: {m}"),
        }
    }
}
impl std::error::Error for AiAdminError {}
impl From<DbError> for AiAdminError {
    fn from(e: DbError) -> Self {
        AiAdminError::Db(e)
    }
}

// ---------------------------------------------------------------------------
// Key obfuscation (XOR + hex; NOT crypto-grade, keeps key out of plaintext)
// ---------------------------------------------------------------------------

const KEY_XOR: u8 = 0x5A;

fn obfuscate(raw: &str) -> String {
    let mut hex = String::with_capacity(raw.len() * 2);
    for b in raw.bytes() {
        hex.push_str(&format!("{:02x}", b ^ KEY_XOR));
    }
    hex
}

fn deobfuscate(hex: &str) -> String {
    let bytes = hex.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 2);
    let mut i = 0;
    while i + 1 < bytes.len() {
        let hi = (bytes[i] as char).to_digit(16).unwrap_or(0) as u8;
        let lo = (bytes[i + 1] as char).to_digit(16).unwrap_or(0) as u8;
        out.push((hi << 4 | lo) ^ KEY_XOR);
        i += 2;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn key_hint(raw: &str) -> String {
    if raw.is_empty() {
        return String::new();
    }
    let b = raw.as_bytes();
    let prefix: String = b.iter().take(4).map(|c| *c as char).collect();
    format!("{}…{}位", prefix, b.len())
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct AiProvider {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub protocol: String,
    pub key_hint: String,
    pub configured: bool,
    pub status: String,
    /// [酒馆对齐] 当前激活供应商（全表唯一 active=1）。
    pub active: bool,
    pub concurrency_limit: i64,
    pub rpm_limit: i64,
    pub max_tokens: i64,
    pub default_model_id: String,
    pub note: String,
    pub last_error: String,
    pub last_success_at: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct AiModel {
    pub id: String,
    pub provider_id: String,
    pub display_name: String,
    pub model_id: String,
    pub purposes: Vec<String>,
    pub context_window: i64,
    pub thinking_enabled: bool,
    pub enabled: bool,
    pub note: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AiCall {
    pub id: String,
    pub provider_id: String,
    pub model_id: String,
    pub work_id: String,
    pub task_type: String,
    pub status: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cached_input_tokens: i64,
    pub error: String,
    pub created_at: String,
    pub completed_at: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct AiUsageDay {
    pub day: String,
    pub calls: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct AiUsageSummary {
    pub total_calls: i64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub by_day: Vec<AiUsageDay>,
    pub recent: Vec<AiCall>,
}

// ---------------------------------------------------------------------------
// Row mappers + column order (keep in sync with every SELECT)
// ---------------------------------------------------------------------------
// provider cols:
//   0 id 1 name 2 base_url 3 encrypted_key 4 protocol 5 status 6 concurrency
//   7 rpm 8 max_tokens 9 default_model 10 note 11 last_error 12 last_success
//   13 created_at 14 updated_at 15 active
// model cols:
//   0 id 1 provider_id 2 display_name 3 model_id 4 purposes_json 5 context
//   6 thinking_enabled 7 enabled 8 note 9 created_at 10 updated_at
// call cols:
//   0 id 1 provider_id 2 model_id 3 work_id 4 task_type 5 status 6 in 7 out
//   8 cached 9 error 10 created 11 completed

const PROV_SEL: &str = "SELECT id,name,base_url,encrypted_key,protocol,status,concurrency_limit,\
                         rpm_limit,max_tokens,COALESCE(default_model_id,''),COALESCE(note,''),\
                         COALESCE(last_error,''),COALESCE(last_success_at,''),created_at,updated_at,active \
                         FROM ai_providers ";
const MODEL_SEL: &str = "SELECT id,provider_id,display_name,model_id,purposes_json,context_window,\
                          thinking_enabled,enabled,COALESCE(note,''),created_at,updated_at \
                          FROM ai_models ";
const CALL_SEL: &str = "SELECT id,provider_id,COALESCE(model_id,''),COALESCE(work_id,''),\
                          COALESCE(task_type,''),status,input_tokens,output_tokens,cached_input_tokens,\
                          COALESCE(error,''),created_at,COALESCE(completed_at,'') FROM ai_calls ";

fn row_provider(r: &rusqlite::Row<'_>) -> rusqlite::Result<AiProvider> {
    let encrypted: String = r.get(3)?;
    let raw = deobfuscate(&encrypted);
    Ok(AiProvider {
        id: r.get(0)?,
        name: r.get(1)?,
        base_url: r.get(2)?,
        protocol: r.get(4)?,
        status: r.get(5)?,
        key_hint: key_hint(&raw),
        configured: !raw.is_empty(),
        concurrency_limit: r.get(6)?,
        rpm_limit: r.get(7)?,
        max_tokens: r.get(8)?,
        default_model_id: r.get(9)?,
        note: r.get(10)?,
        last_error: r.get(11)?,
        last_success_at: r.get(12)?,
        created_at: r.get(13)?,
        updated_at: r.get(14)?,
        active: r.get::<_, i64>(15)? != 0,
    })
}

fn row_model(r: &rusqlite::Row<'_>) -> rusqlite::Result<AiModel> {
    Ok(AiModel {
        id: r.get(0)?,
        provider_id: r.get(1)?,
        display_name: r.get(2)?,
        model_id: r.get(3)?,
        purposes: serde_json::from_str(&r.get::<_, String>(4)?).unwrap_or_default(),
        context_window: r.get(5)?,
        thinking_enabled: r.get::<_, i64>(6)? != 0,
        enabled: r.get::<_, i64>(7)? != 0,
        note: r.get(8)?,
        created_at: r.get(9)?,
        updated_at: r.get(10)?,
    })
}

fn row_call(r: &rusqlite::Row<'_>) -> rusqlite::Result<AiCall> {
    Ok(AiCall {
        id: r.get(0)?,
        provider_id: r.get(1)?,
        model_id: r.get(2)?,
        work_id: r.get(3)?,
        task_type: r.get(4)?,
        status: r.get(5)?,
        input_tokens: r.get(6)?,
        output_tokens: r.get(7)?,
        cached_input_tokens: r.get(8)?,
        error: r.get(9)?,
        created_at: r.get(10)?,
        completed_at: r.get(11)?,
    })
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn short_id(prefix: &str) -> String {
    let u = uuid::Uuid::new_v4().simple().to_string();
    // prefix + first 12 hex chars
    format!("{}{}", prefix, &u[0..12])
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct AiAdminStore {
    pool: DbPool,
}

impl AiAdminStore {
    pub fn open(path: &Path) -> Result<Self, AiAdminError> {
        Ok(AiAdminStore { pool: DbPool::open(path, 4)? })
    }

    pub fn open_in_memory() -> Result<Self, AiAdminError> {
        Ok(AiAdminStore { pool: DbPool::open_in_memory(4)? })
    }

    fn conn(&self) -> Result<DbConn, AiAdminError> {
        self.pool.get().map_err(AiAdminError::Db)
    }

    fn map_not_found(e: rusqlite::Error, what: &str) -> AiAdminError {
        match e {
            rusqlite::Error::QueryReturnedNoRows => AiAdminError::NotFound(what.to_string()),
            other => DbError::Migrate(other).into(),
        }
    }

    // ----- providers -----

    pub fn list_providers(&self) -> Result<Vec<AiProvider>, AiAdminError> {
        let mut c = self.conn()?;
        let mut stmt = c
            .conn()
            .prepare(&format!("{} ORDER BY created_at ASC", PROV_SEL))
            .map_err(DbError::Migrate)?;
        let rows = stmt.query_map([], row_provider).map_err(DbError::Migrate)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| DbError::Migrate(e).into())
    }

    pub fn get_provider(&self, id: &str) -> Result<AiProvider, AiAdminError> {
        let mut c = self.conn()?;
        c.conn()
            .query_row(&format!("{} WHERE id=?1", PROV_SEL), [id], row_provider)
            .map_err(|e| Self::map_not_found(e, &format!("provider {id}")))
    }

    pub fn raw_key(&self, id: &str) -> Result<String, AiAdminError> {
        let mut c = self.conn()?;
        let hex: String = c
            .conn()
            .query_row(
                "SELECT encrypted_key FROM ai_providers WHERE id=?1",
                [id],
                |r| r.get(0),
            )
            .map_err(|e| Self::map_not_found(e, &format!("provider {id}")))?;
        Ok(deobfuscate(&hex))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_provider(
        &self,
        name: &str,
        base_url: &str,
        protocol: &str,
        api_key: &str,
        concurrency_limit: i64,
        rpm_limit: i64,
        max_tokens: i64,
        note: &str,
    ) -> Result<AiProvider, AiAdminError> {
        if name.trim().is_empty() {
            return Err(AiAdminError::BadRequest("name required".into()));
        }
        if !matches!(protocol, "openai" | "anthropic" | "google") {
            return Err(AiAdminError::BadRequest(format!("unsupported protocol {protocol}")));
        }
        // [酒馆对齐] SSRF 补丁: provider 录入统一过 base_url 校验 (拒绝私网/环回/伪造 host)
        if !base_url.trim().is_empty() {
            crate::validate_llm_base_url(base_url.trim())
                .map_err(|e| AiAdminError::BadRequest(format!("{e}")))?;
        }
        let id = short_id("p_");
        let ts = now();
        let encrypted = obfuscate(api_key);
        let mut c = self.conn()?;
        c.conn()
            .execute(
                "INSERT INTO ai_providers
                 (id,name,base_url,encrypted_key,protocol,status,concurrency_limit,rpm_limit,max_tokens,
                  default_model_id,note,last_error,last_success_at,created_at,updated_at)
                 VALUES (?1,?2,?3,?4,?5,'enabled',?6,?7,?8,'',?9,'','',?10,?10)",
                params![
                    id,
                    name.trim(),
                    base_url.trim(),
                    encrypted,
                    protocol,
                    concurrency_limit.clamp(1, 100),
                    rpm_limit.clamp(1, 10000),
                    max_tokens.clamp(1, 32768),
                    note.trim(),
                    ts
                ],
            )
            .map_err(DbError::Migrate)?;
        drop(c);
        self.get_provider(&id)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update_provider(
        &self,
        id: &str,
        name: Option<&str>,
        base_url: Option<&str>,
        api_key: Option<&str>, // Some(empty or "keep") = keep existing
        concurrency_limit: Option<i64>,
        rpm_limit: Option<i64>,
        max_tokens: Option<i64>,
        note: Option<&str>,
    ) -> Result<AiProvider, AiAdminError> {
        let cur = self.get_provider(id)?;
        let new_name = name.filter(|s| !s.trim().is_empty()).unwrap_or(&cur.name).trim().to_string();
        let new_url = base_url.filter(|s| !s.trim().is_empty()).unwrap_or(&cur.base_url).trim().to_string();
        // [酒馆对齐] SSRF 补丁: 更新 base_url 同样过校验
        if new_url != cur.base_url {
            crate::validate_llm_base_url(&new_url)
                .map_err(|e| AiAdminError::BadRequest(format!("{e}")))?;
        }
        let new_conc = concurrency_limit.unwrap_or(cur.concurrency_limit).clamp(1, 100);
        let new_rpm = rpm_limit.unwrap_or(cur.rpm_limit).clamp(1, 10000);
        let new_max = max_tokens.unwrap_or(cur.max_tokens).clamp(1, 32768);
        let new_note = note.map(|s| s.trim().to_string()).unwrap_or(cur.note);
        let ts = now();

        let mut c = self.conn()?;
        if let Some(k) = api_key {
            if !k.trim().is_empty() && k.trim() != "keep" {
                let encrypted = obfuscate(k.trim());
                c.conn()
                    .execute(
                        "UPDATE ai_providers SET name=?2,base_url=?3,encrypted_key=?4,
                                concurrency_limit=?5,rpm_limit=?6,max_tokens=?7,note=?8,updated_at=?9
                         WHERE id=?1",
                        params![id, new_name, new_url, encrypted, new_conc, new_rpm, new_max, new_note, ts],
                    )
                    .map_err(DbError::Migrate)?;
                drop(c);
                return self.get_provider(id);
            }
        }
        c.conn()
            .execute(
                "UPDATE ai_providers SET name=?2,base_url=?3,
                        concurrency_limit=?4,rpm_limit=?5,max_tokens=?6,note=?7,updated_at=?8
                 WHERE id=?1",
                params![id, new_name, new_url, new_conc, new_rpm, new_max, new_note, ts],
            )
            .map_err(DbError::Migrate)?;
        drop(c);
        self.get_provider(id)
    }

    pub fn set_provider_status(&self, id: &str, status: &str) -> Result<AiProvider, AiAdminError> {
        if !matches!(status, "enabled" | "disabled") {
            return Err(AiAdminError::BadRequest("status must be enabled|disabled".into()));
        }
        let mut c = self.conn()?;
        c.conn()
            .execute(
                "UPDATE ai_providers SET status=?2, updated_at=?3 WHERE id=?1",
                params![id, status, now()],
            )
            .map_err(DbError::Migrate)?;
        self.get_provider(id)
    }

    pub fn set_default_model(&self, id: &str, model_id: &str) -> Result<AiProvider, AiAdminError> {
        let mut c = self.conn()?;
        c.conn()
            .execute(
                "UPDATE ai_providers SET default_model_id=?2, updated_at=?3 WHERE id=?1",
                params![id, model_id, now()],
            )
            .map_err(DbError::Migrate)?;
        self.get_provider(id)
    }

    /// [酒馆对齐] 设为当前激活供应商: 该行 active=1, 其余行 active=0。
    pub fn set_active_provider(&self, id: &str) -> Result<AiProvider, AiAdminError> {
        self.get_provider(id)?; // 404 if missing
        let mut c = self.conn()?;
        c.conn()
            .execute(
                "UPDATE ai_providers SET active = CASE WHEN id=?1 THEN 1 ELSE 0 END, updated_at=?2",
                params![id, now()],
            )
            .map_err(DbError::Migrate)?;
        drop(c);
        self.get_provider(id)
    }

    /// [酒馆对齐] 当前激活供应商 (仅 active=1 且 enabled)。
    pub fn active_provider(&self) -> Result<Option<AiProvider>, AiAdminError> {
        let mut c = self.conn()?;
        match c.conn().query_row(
            &format!("{} WHERE active=1 AND status='enabled'", PROV_SEL),
            [],
            row_provider,
        ) {
            Ok(p) => Ok(Some(p)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(AiAdminError::Db(DbError::Migrate(e))),
        }
    }

    pub fn mark_success(&self, id: &str) {
        if let Ok(mut c) = self.conn() {
            let _ = c.conn().execute(
                "UPDATE ai_providers SET last_success_at=?2, last_error='', updated_at=?2 WHERE id=?1",
                params![id, now()],
            );
        }
    }

    pub fn mark_error(&self, id: &str, err: &str) {
        let err_short: String = err.chars().take(240).collect();
        if let Ok(mut c) = self.conn() {
            let _ = c.conn().execute(
                "UPDATE ai_providers SET last_error=?2, updated_at=updated_at WHERE id=?1",
                params![id, err_short],
            );
        }
    }

    pub fn delete_provider(&self, id: &str) -> Result<(), AiAdminError> {
        let mut c = self.conn()?;
        c.conn()
            .execute("DELETE FROM ai_models WHERE provider_id=?1", [id])
            .map_err(DbError::Migrate)?;
        let n = c
            .conn()
            .execute("DELETE FROM ai_providers WHERE id=?1", [id])
            .map_err(DbError::Migrate)?;
        if n == 0 {
            return Err(AiAdminError::NotFound(format!("provider {id}")));
        }
        Ok(())
    }

    // ----- models -----

    pub fn list_models(&self, provider_id: &str) -> Result<Vec<AiModel>, AiAdminError> {
        let mut c = self.conn()?;
        let mut stmt = c
            .conn()
            .prepare(&format!("{} WHERE provider_id=?1 ORDER BY display_name ASC", MODEL_SEL))
            .map_err(DbError::Migrate)?;
        let rows = stmt
            .query_map(params![provider_id], row_model)
            .map_err(DbError::Migrate)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| DbError::Migrate(e).into())
    }

    pub fn get_model(&self, id: &str) -> Result<AiModel, AiAdminError> {
        let mut c = self.conn()?;
        c.conn()
            .query_row(&format!("{} WHERE id=?1", MODEL_SEL), [id], row_model)
            .map_err(|e| Self::map_not_found(e, &format!("model {id}")))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_model(
        &self,
        provider_id: &str,
        display_name: &str,
        model_id: &str,
        purposes: &[&str],
        context_window: i64,
        thinking_enabled: bool,
        enabled: bool,
        note: &str,
    ) -> Result<AiModel, AiAdminError> {
        self.get_provider(provider_id)?;
        if display_name.trim().is_empty() || model_id.trim().is_empty() {
            return Err(AiAdminError::BadRequest("display_name and model_id required".into()));
        }
        let id = short_id("m_");
        let ts = now();
        let purposes_json = serde_json::to_string(&purposes).unwrap_or_else(|_| "[]".into());
        let mut c = self.conn()?;
        c.conn()
            .execute(
                "INSERT INTO ai_models
                 (id,provider_id,display_name,model_id,purposes_json,context_window,thinking_enabled,enabled,note,created_at,updated_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?10)",
                params![
                    id, provider_id, display_name.trim(), model_id.trim(), purposes_json,
                    context_window.clamp(1, 3_000_000), thinking_enabled as i64, enabled as i64,
                    note.trim(), ts
                ],
            )
            .map_err(DbError::Migrate)?;
        drop(c);
        self.get_model(&id)
    }

    pub fn update_model(
        &self,
        id: &str,
        display_name: Option<&str>,
        model_id: Option<&str>,
        purposes: Option<Vec<String>>,
        context_window: Option<i64>,
        thinking_enabled: Option<bool>,
        enabled: Option<bool>,
        note: Option<&str>,
    ) -> Result<AiModel, AiAdminError> {
        let cur = self.get_model(id)?;
        let new_display = display_name.filter(|s| !s.trim().is_empty()).unwrap_or(&cur.display_name).to_string();
        let new_model_id = model_id.filter(|s| !s.trim().is_empty()).unwrap_or(&cur.model_id).to_string();
        let new_ctx = context_window.unwrap_or(cur.context_window).clamp(1, 3_000_000);
        let new_thinking = thinking_enabled.unwrap_or(cur.thinking_enabled);
        let new_enabled = enabled.unwrap_or(cur.enabled);
        let new_note = note.map(|s| s.trim().to_string()).unwrap_or(cur.note);
        let purposes_json = serde_json::to_string(&purposes.unwrap_or(cur.purposes)).unwrap_or_else(|_| "[]".into());
        let mut c = self.conn()?;
        c.conn()
            .execute(
                "UPDATE ai_models SET display_name=?2,model_id=?3,purposes_json=?4,context_window=?5,
                        thinking_enabled=?6,enabled=?7,note=?8,updated_at=?9 WHERE id=?1",
                params![
                    id, new_display, new_model_id, purposes_json, new_ctx,
                    new_thinking as i64, new_enabled as i64, new_note, now()
                ],
            )
            .map_err(DbError::Migrate)?;
        self.get_model(id)
    }

    pub fn delete_model(&self, id: &str) -> Result<(), AiAdminError> {
        let mut c = self.conn()?;
        let n = c
            .conn()
            .execute("DELETE FROM ai_models WHERE id=?1", [id])
            .map_err(DbError::Migrate)?;
        if n == 0 {
            return Err(AiAdminError::NotFound(format!("model {id}")));
        }
        Ok(())
    }

    // ----- usage / calls -----

    #[allow(clippy::too_many_arguments)]
    pub fn record_call(
        &self,
        provider_id: &str,
        model_id: &str,
        work_id: &str,
        task_type: &str,
        status: &str,
        input_tokens: i64,
        output_tokens: i64,
        cached_input_tokens: i64,
        error: Option<&str>,
    ) -> Result<AiCall, AiAdminError> {
        let id = short_id("c_");
        let ts = now();
        let err_short: String = error.map(|e| e.chars().take(240).collect()).unwrap_or_default();
        let mut c = self.conn()?;
        c.conn()
            .execute(
                "INSERT INTO ai_calls
                 (id,provider_id,model_id,work_id,task_type,status,input_tokens,output_tokens,cached_input_tokens,error,created_at,completed_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?11)",
                params![
                    id, provider_id, model_id, work_id, task_type, status,
                    input_tokens.max(0), output_tokens.max(0), cached_input_tokens.max(0), err_short, ts
                ],
            )
            .map_err(DbError::Migrate)?;
        drop(c);
        self.get_call(&id)
    }

    pub fn get_call(&self, id: &str) -> Result<AiCall, AiAdminError> {
        let mut c = self.conn()?;
        c.conn()
            .query_row(&format!("{} WHERE id=?1", CALL_SEL), [id], row_call)
            .map_err(|e| Self::map_not_found(e, &format!("call {id}")))
    }

    /// Aggregated usage summary for last `days` days (0 = all-time).
    pub fn usage_summary(&self, days: i64) -> Result<AiUsageSummary, AiAdminError> {
        let mut summary = AiUsageSummary::default();
        // totals
        let mut c0 = self.conn()?;
        let row = if days > 0 {
            let cutoff = format!("-{} days", days);
            c0.conn()
                .query_row(
                    "SELECT COUNT(*), COALESCE(SUM(input_tokens),0), COALESCE(SUM(output_tokens),0) FROM ai_calls
                     WHERE date(created_at) >= date('now', ?1, '-1 days')",
                    params![cutoff],
                    |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?)),
                )
                .map_err(DbError::Migrate)?
        } else {
            c0.conn()
                .query_row(
                    "SELECT COUNT(*), COALESCE(SUM(input_tokens),0), COALESCE(SUM(output_tokens),0) FROM ai_calls",
                    [],
                    |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?)),
                )
                .map_err(DbError::Migrate)?
        };
        drop(c0);
        summary.total_calls = row.0;
        summary.total_input_tokens = row.1;
        summary.total_output_tokens = row.2;

        // by day (last `buckets` — newest last)
        let buckets = if days > 0 { days } else { 30 };
        let mut c = self.conn()?;
        {
            let mut stmt = c
                .conn()
                .prepare(
                    "SELECT substr(created_at,1,10) AS day, COUNT(*), SUM(input_tokens), SUM(output_tokens)
                     FROM ai_calls GROUP BY day ORDER BY day DESC LIMIT ?1",
                )
                .map_err(DbError::Migrate)?;
            let rows = stmt
                .query_map([buckets], |r| {
                    Ok(AiUsageDay {
                        day: r.get(0)?,
                        calls: r.get(1)?,
                        input_tokens: r.get(2)?,
                        output_tokens: r.get(3)?,
                    })
                })
                .map_err(DbError::Migrate)?;
            summary.by_day = rows.collect::<Result<Vec<_>, _>>().map_err(|e| AiAdminError::Db(DbError::Migrate(e)))?;
            summary.by_day.reverse();
        }
        drop(c);

        // recent
        let mut c = self.conn()?;
        {
            let mut stmt = c
                .conn()
                .prepare(&format!("{} ORDER BY created_at DESC LIMIT 50", CALL_SEL))
                .map_err(DbError::Migrate)?;
            let rows = stmt.query_map([], row_call).map_err(DbError::Migrate)?;
            summary.recent = rows.collect::<Result<Vec<_>, _>>().map_err(|e| AiAdminError::Db(DbError::Migrate(e)))?;
        }
        Ok(summary)
    }

    /// Call-side runtime resolution for chat handlers.
    ///
    /// Picks the first enabled provider that has an enabled model (preferring the
    /// provider's `default_model_id`), and returns decrypted credentials so the
    /// existing `resolve_llm` path can route all LLM calls through managed
    /// providers. Returns `None` when no usable provider/model is configured.
    pub fn resolve_call_runtime(&self) -> Result<Option<CallRuntime>, AiAdminError> {
        let mut c = self.conn()?;
        let ids: Vec<String> = {
            let mut stmt = c
                .conn()
                .prepare(
                    "SELECT id FROM ai_providers WHERE status='enabled'
                     ORDER BY active DESC, CASE WHEN default_model_id != '' THEN 0 ELSE 1 END, updated_at DESC",
                )
                .map_err(DbError::Migrate)?;
            let rows = stmt
                .query_map([], |r| r.get::<_, String>(0))
                .map_err(DbError::Migrate)?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|e| AiAdminError::Db(DbError::Migrate(e)))?
        };
        drop(c);

        for id in ids {
            let prov = match self.get_provider(&id) {
                Ok(p) => p,
                Err(_) => continue,
            };
            if prov.base_url.trim().is_empty() {
                continue;
            }
            let model_id = if !prov.default_model_id.is_empty()
                && self
                    .get_model(&prov.default_model_id)
                    .map(|m| m.enabled)
                    .unwrap_or(false)
            {
                prov.default_model_id.clone()
            } else {
                match self
                    .list_models(&id)?
                    .into_iter()
                    .find(|m| m.enabled)
                    .map(|m| m.model_id)
                {
                    Some(m) => m,
                    None => continue,
                }
            };
            let key = match self.raw_key(&id) {
                Ok(k) => k,
                Err(_) => continue,
            };
            if key.trim().is_empty() {
                continue;
            }
            return Ok(Some(CallRuntime {
                provider_id: id.clone(),
                base_url: prov.base_url,
                api_key: key,
                model: model_id,
                protocol: prov.protocol,
                rpm_limit: prov.rpm_limit.max(0) as u32,
            }));
        }
        Ok(None)
    }

    /// Look up an enabled provider id by base URL (used by call-side RPM/metering
    /// when the caller only knows the endpoint).
    pub fn provider_id_by_base(&self, base_url: &str) -> Result<Option<String>, AiAdminError> {
        let base = base_url.trim().trim_end_matches('/').to_string();
        let ids: Vec<String> = {
            let mut c = self.conn()?;
            let mut stmt = c
                .conn()
                .prepare("SELECT id FROM ai_providers WHERE status='enabled'")
                .map_err(DbError::Migrate)?;
            let rows = stmt
                .query_map([], |r| r.get::<_, String>(0))
                .map_err(DbError::Migrate)?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|e| AiAdminError::Db(DbError::Migrate(e)))?
        };
        for id in ids {
            if let Ok(p) = self.get_provider(&id) {
                if p.base_url.trim().trim_end_matches('/') == base {
                    return Ok(Some(id));
                }
            }
        }
        Ok(None)
    }
}

/// Credentials + limits resolved for a single LLM call (P5 call-side routing).
#[derive(Debug, Clone)]
pub struct CallRuntime {
    pub provider_id: String,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    /// G6: provider protocol ("openai" | "anthropic" | "google") — lets
    /// call-side dispatch route to the right backend instead of assuming OpenAI.
    pub protocol: String,
    pub rpm_limit: u32,
}

#[cfg(test)]
mod ai_admin_tests {
    use super::*;

    #[test]
    fn provider_model_call_flow() {
        let s = AiAdminStore::open_in_memory().expect("open");
        let p = s
            .create_provider("OpenRouter", "https://openrouter.ai/api/v1", "openai", "sk-test-123", 10, 60, 32000, "p5")
            .expect("create provider");
        assert_eq!(p.configured, true);
        assert_eq!(p.key_hint.contains("sk-t"), true);
        // raw key round-trips
        assert_eq!(s.raw_key(&p.id).unwrap(), "sk-test-123");

        let m = s.create_model(&p.id, "智能对话", "openrouter/auto", &["chat", "continue"], 128000, true, true, "")
            .expect("create model");
        assert_eq!(m.purposes.len(), 2);

        s.set_default_model(&p.id, &m.id).expect("set default");
        let pm = s.get_provider(&p.id).unwrap();
        assert_eq!(pm.default_model_id, m.id);

        s.record_call(&p.id, &m.id, "w1", "chapter-analysis", "ok", 100, 50, 20, None).unwrap();
        s.record_call(&p.id, &m.id, "w1", "chat", "failed", 10, 0, 0, Some("boom")).unwrap();

        let sum = s.usage_summary(7).unwrap();
        assert_eq!(sum.total_calls, 2);
        assert_eq!(sum.total_input_tokens, 110);
        assert_eq!(sum.total_output_tokens, 50);
        assert_eq!(sum.recent.len(), 2);

        s.delete_model(&m.id).expect("del model");
        s.delete_provider(&p.id).expect("del provider");
        assert!(s.get_provider(&p.id).is_err());
    }

    #[test]
    fn key_roundtrip() {
        for raw in ["sk-ant-api03-x", "abcd", "", "0123456789"] {
            let o = obfuscate(raw);
            assert_eq!(deobfuscate(&o), raw);
        }
    }
}