//! W7: recent automationId trigger log (ST extension hook surface).
//!
//! Ring buffer under `$KALEIDO_DATA/state/automation-triggers.json`.
//! Backend records activations; Web can list without full ST event bus.

use crate::{CoreError, CoreResult, DataRoot};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

const DEFAULT_CAP: usize = 100;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationTriggerEvent {
    pub id: String,
    pub automation_id: String,
    #[serde(default)]
    pub entry_uid: String,
    #[serde(default)]
    pub world: String,
    #[serde(default)]
    pub comment: String,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub source: String,
    pub at_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationTriggerLog {
    #[serde(default)]
    pub events: Vec<AutomationTriggerEvent>,
    #[serde(default = "default_cap")]
    pub cap: usize,
}

fn default_cap() -> usize {
    DEFAULT_CAP
}

pub struct AutomationTriggerStore {
    path: PathBuf,
}

impl AutomationTriggerStore {
    pub fn new(data: &DataRoot) -> Self {
        let dir = data.root().join("state");
        let _ = fs::create_dir_all(&dir);
        Self {
            path: dir.join("automation-triggers.json"),
        }
    }

    pub fn load(&self) -> AutomationTriggerLog {
        match fs::read_to_string(&self.path) {
            Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
            Err(_) => AutomationTriggerLog {
                events: Vec::new(),
                cap: DEFAULT_CAP,
            },
        }
    }

    pub fn save(&self, log: &AutomationTriggerLog) -> CoreResult<()> {
        if let Some(parent) = self.path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let raw = serde_json::to_string_pretty(log)
            .map_err(|e| CoreError::BadRequest(format!("automation log serialize: {e}")))?;
        fs::write(&self.path, raw)?;
        Ok(())
    }

    /// Append one or more triggers; newest last; trim to cap.
    pub fn record(
        &self,
        automation_ids: &[String],
        activated: &[(String, String, String, String)],
        session_id: &str,
        source: &str,
    ) -> CoreResult<usize> {
        if automation_ids.is_empty() && activated.is_empty() {
            return Ok(0);
        }
        let mut log = self.load();
        let cap = if log.cap == 0 { DEFAULT_CAP } else { log.cap };
        let at = now_ms();
        let mut added = 0usize;

        // Prefer per-entry rows when available (uid, world, comment, reason) + matching auto id
        if !activated.is_empty() {
            for (uid, world, comment, reason) in activated {
                // Find matching automation id from parallel list is caller responsibility;
                // here each tuple already carries automation via reason? No — caller passes
                // only entries that have automation_id in a separate channel.
                let _ = (uid, world, comment, reason);
            }
        }

        // Record one event per automation_id (dedupe within this batch).
        let mut seen = std::collections::HashSet::new();
        for aid in automation_ids {
            let aid = aid.trim();
            if aid.is_empty() || !seen.insert(aid.to_string()) {
                continue;
            }
            // Best-effort match first activated entry that carried this id in comment/uid is not
            // reliable; leave entry fields empty unless single activated row provided via helper.
            let ev = AutomationTriggerEvent {
                id: format!("at-{at}-{added}"),
                automation_id: aid.to_string(),
                entry_uid: String::new(),
                world: String::new(),
                comment: String::new(),
                reason: String::new(),
                session_id: session_id.to_string(),
                source: source.to_string(),
                at_ms: at,
            };
            log.events.push(ev);
            added += 1;
        }

        if log.events.len() > cap {
            let drain = log.events.len() - cap;
            log.events.drain(0..drain);
        }
        log.cap = cap;
        self.save(&log)?;
        Ok(added)
    }

    /// Richer record: each item is (automation_id, entry_uid, world, comment, reason).
    pub fn record_detailed(
        &self,
        items: &[(String, String, String, String, String)],
        session_id: &str,
        source: &str,
    ) -> CoreResult<usize> {
        if items.is_empty() {
            return Ok(0);
        }
        let mut log = self.load();
        let cap = if log.cap == 0 { DEFAULT_CAP } else { log.cap };
        let at = now_ms();
        let mut added = 0usize;
        let mut seen = std::collections::HashSet::new();
        for (aid, uid, world, comment, reason) in items {
            let aid = aid.trim();
            if aid.is_empty() {
                continue;
            }
            // Allow same automation_id multiple times if different entries, but cap spam
            // within one batch by (aid, uid).
            let key = format!("{aid}\0{uid}");
            if !seen.insert(key) {
                continue;
            }
            log.events.push(AutomationTriggerEvent {
                id: format!("at-{at}-{added}"),
                automation_id: aid.to_string(),
                entry_uid: uid.clone(),
                world: world.clone(),
                comment: comment.clone(),
                reason: reason.clone(),
                session_id: session_id.to_string(),
                source: source.to_string(),
                at_ms: at,
            });
            added += 1;
        }
        if log.events.len() > cap {
            let drain = log.events.len() - cap;
            log.events.drain(0..drain);
        }
        log.cap = cap;
        self.save(&log)?;
        Ok(added)
    }

    pub fn recent(&self, limit: usize) -> Vec<AutomationTriggerEvent> {
        let log = self.load();
        let n = if limit == 0 { 20 } else { limit.min(200) };
        let len = log.events.len();
        if len <= n {
            // newest last in storage; return newest-first for API
            let mut v = log.events;
            v.reverse();
            v
        } else {
            let mut v: Vec<_> = log.events[len - n..].to_vec();
            v.reverse();
            v
        }
    }

    pub fn clear(&self) -> CoreResult<()> {
        self.save(&AutomationTriggerLog {
            events: Vec::new(),
            cap: DEFAULT_CAP,
        })
    }
}
