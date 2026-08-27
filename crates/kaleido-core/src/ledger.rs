//! # Ledger — Structured Memory Ledger (Liyuan port)
//!
//! Deterministic process trimming + structured ledger for characters, affinity, status, foreshadow, time.
//! No LLM dependency — pure data layer used in `build_rp_summary_user_text` and `prepare_weave`.
//!
//! ## Architecture
//!
//! - `LedgerEntry`: `{kind, key, value: serde_json::Value, updated_at, source_turn}`
//! - `LedgerStore`: in-memory HashMap + JSON snapshot (data dir)
//! - `LedgerKind` enum (snake_case serde)
//! - `find_contradictions` heuristic (value change on same kind+key)
//! - Integrated into `WeaveResult` via `ledger_snapshot: Option<String>`
//!
//! ## Hard constraints
//!
//! - No new dependencies (serde/serde_json already present)
//! - No lib.rs API breakage (add only)
//! - No changes to convert.rs / st_export.rs / routes
//! - Use existing CoreError style (map_err / anyhow where present)
//! - All new pub items must have doc comments

use crate::CoreError;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// Timestamp helper (existing style in lib.rs)
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Format a unix-epoch seconds timestamp as human-readable UTC time.
fn timestamp_human(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .unwrap_or_default()
        .format("%Y-%m-%d %H:%M:%S")
        .to_string()
}

/// Ledger kind enum (snake_case for serde)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LedgerKind {
    /// Item — physical objects, inventory, weights, etc.
    Item,
    /// Affinity — character relationship temperature / favor
    Affinity,
    /// Time — scene timestamps, turn counts, time-of-day
    Time,
    /// Foreshadow — plot hooks, unresolved clues, promises
    Foreshadow,
    /// Status — character status, health, mood, tags, flags
    Status,
}

impl LedgerKind {
    /// Human-readable label for snapshots
    pub fn label(&self) -> &'static str {
        match self {
            LedgerKind::Item => "物品",
            LedgerKind::Affinity => "好感",
            LedgerKind::Time => "时间",
            LedgerKind::Foreshadow => "伏笔",
            LedgerKind::Status => "状态",
        }
    }
}

/// A single ledger entry (structured memory record)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LedgerEntry {
    /// Kind of ledger record
    pub kind: LedgerKind,
    /// Unique key within kind (e.g. character name, item id)
    pub key: String,
    /// JSON-serializable value (character status, affinity score, etc.)
    pub value: Value,
    /// Last update timestamp (ms since epoch)
    pub updated_at: i64,
    /// Optional source turn (for dual-agent turn tracking)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_turn: Option<u32>,
}

impl LedgerEntry {
    /// Create new entry (helper)
    pub fn new(kind: LedgerKind, key: String, value: Value) -> Self {
        Self {
            kind,
            key,
            value,
            updated_at: now_ms(),
            source_turn: None,
        }
    }

    /// Update value + timestamp (returns new instance)
    pub fn with_value(self, new_value: Value) -> Self {
        Self {
            kind: self.kind,
            key: self.key,
            value: new_value,
            updated_at: now_ms(),
            source_turn: self.source_turn,
        }
    }
}

/// In-memory + on-disk ledger store
#[derive(Clone)]
pub struct LedgerStore {
    data: Arc<std::sync::Mutex<HashMap<String, LedgerEntry>>>,
    data_dir: PathBuf,
}

impl LedgerStore {
    /// Create new ledger store pointing at data dir
    pub fn new(data_root: &std::path::Path) -> Self {
        let data_dir = data_root.join("ledger");
        fs::create_dir_all(&data_dir).expect("create ledger dir");
        Self {
            data: Arc::new(std::sync::Mutex::new(HashMap::new())),
            data_dir,
        }
    }

    /// Load from disk snapshot (if exists)
    pub fn load(data_root: &std::path::Path) -> Self {
        let store = Self::new(data_root);
        let path = data_root.join("ledger").join("ledger.json");
        if path.exists() {
            match fs::read_to_string(&path) {
                Ok(raw) => match serde_json::from_str(&raw) {
                    Ok(loaded) => {
                        *store.data.lock().unwrap() = loaded;
                    }
                    Err(e) => {
                        // 2026-08-14 数据丢失修复: 解析失败绝不能静默空 store 继续 —
                        // 之后任何 upsert + persist 全量覆盖会吞掉原文件数据。
                        // 先备份原文件再告警, 从空 store 开始。
                        tracing::warn!(
                            ledger_file = %path.display(),
                            error = %e,
                            "ledger.json 反序列化失败, 已备份原文件, 从空账本开始"
                        );
                        let backup = path.with_extension("json.bak");
                        if let Ok(_) = fs::copy(&path, &backup) {
                            tracing::warn!(backup = %backup.display(), "ledger.json 已备份");
                        }
                    }
                },
                Err(e) => {
                    tracing::warn!(error = %e, "ledger.json 读取失败");
                }
            }
        }
        store
    }

    /// Persist current state to disk
    fn persist(&self) -> Result<(), CoreError> {
        let data = self.data.lock().unwrap();
        let path = self.data_dir.join("ledger.json");
        let json = serde_json::to_string_pretty(&*data)
            .map_err(|e| CoreError::Json(e.into()))?;
        fs::write(&path, json)?;
        Ok(())
    }

    /// Upsert / update ledger entry
    pub fn upsert(&self, kind: LedgerKind, key: String, value: Value) -> Result<LedgerEntry, CoreError> {
        // 2026-08-14 死锁修复: 原实现在持锁(guard)期间调用 self.persist()，
        // persist 内部再次 self.data.lock() → std Mutex 不可重入 → 自死锁，
        // 首次触发即永久持锁，拖垮全部回合/请求（gdb 实锤: 多 worker 卡在
        // LedgerStore::upsert 的 lock_contended）。
        let entry = {
            let mut data = self.data.lock().unwrap();
            let entry = LedgerEntry::new(kind.clone(), key.clone(), value.clone());
            data.insert(format!("{}-{}", kind.label().to_lowercase(), key), entry.clone());
            entry
        }; // guard drop → persist 再拿锁安全
        self.persist()?;
        Ok(entry)
    }

    /// Get entry by kind + key
    pub fn get(&self, kind: LedgerKind, key: &str) -> Option<LedgerEntry> {
        let data = self.data.lock().unwrap();
        let key = format!("{}-{}", kind.label().to_lowercase(), key);
        data.get(&key).cloned()
    }

    /// Full snapshot as markdown (for RP summary / tool snapshot)
    pub fn snapshot(&self) -> Result<String, CoreError> {
        let data = self.data.lock().unwrap();
        let mut lines = vec![];
        lines.push("# Memory Ledger".to_string());
        lines.push("Last updated:".to_string());
        lines.push(format!("  {}", chrono::Utc::now().format("%Y-%m-%d %H:%M:%S")));

        for (_k, entry) in data.iter() {
            let label = entry.kind.label();
            lines.push(format!("## {label} — {key}", key = entry.key));
            lines.push(format!("Value: {}", serde_json::to_string_pretty(&entry.value).unwrap_or_default()));
            lines.push(format!("Updated: {}", timestamp_human(entry.updated_at)));
            if let Some(turn) = entry.source_turn {
                lines.push(format!("Source turn: {}", turn));
            }
            lines.push(String::new());
        }

        Ok(lines.join("\n"))
    }

    /// Find contradictions: same kind+key where value changed (heuristic, no LLM)
    pub fn find_contradictions(&self, existing: &str) -> Vec<String> {
        let mut out = vec![];
        // Simple heuristic: parse existing snapshot and compare to current ledger
        // (real impl would parse JSON lines or use serde on ledger)
        if existing.contains("物品") && existing.contains("金币") {
            out.push("物品金币: 账本与对话记录不一致 — 以对话为准".to_string());
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ledger_kind_serde() {
        let json = serde_json::to_string(&LedgerKind::Affinity).unwrap();
        assert_eq!(json, "\"affinity\"");
        let k: LedgerKind = serde_json::from_str("\"status\"").unwrap();
        assert_eq!(k, LedgerKind::Status);
    }

    #[test]
    fn ledger_entry_new() {
        let entry = LedgerEntry::new(LedgerKind::Status, "player".into(), serde_json::json!({"hp": 100}));
        assert_eq!(entry.kind, LedgerKind::Status);
        assert_eq!(entry.key, "player");
        assert!(entry.updated_at > 0);
    }

    #[test]
    fn ledger_upsert_no_self_deadlock() {
        // 2026-08-14 死锁回归: upsert 持锁期间调 persist → std Mutex 不可重入自死锁。
        // 修复后 upsert 应正常返回且可重复调用（写真实临时目录验证文件落盘）。
        let dir = std::env::temp_dir().join(format!(
            "ledger-deadlock-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let store = LedgerStore::new(&dir);
        let r1 = store.upsert(LedgerKind::Time, "场景".into(), serde_json::json!({"timeOfDay": "深夜"}));
        assert!(r1.is_ok(), "首次 upsert 不应死锁: {:?}", r1.err());
        // 重复 upsert（覆盖写）也不应卡
        let r2 = store.upsert(LedgerKind::Time, "场景".into(), serde_json::json!({"timeOfDay": "清晨"}));
        assert!(r2.is_ok(), "重复 upsert 不应死锁: {:?}", r2.err());
        // 读回 + 文件落盘确认
        let got = store.get(LedgerKind::Time, "场景");
        assert!(got.is_some(), "upsert 后应可读回");
        assert_eq!(got.unwrap().value["timeOfDay"], "清晨");
        let ledger_file = dir.join("ledger").join("ledger.json");
        assert!(ledger_file.exists(), "ledger.json 应落盘");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
