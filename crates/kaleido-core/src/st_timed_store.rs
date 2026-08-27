//! Persist ST `timedWorldInfo` per chat/session under data root.

use crate::{st_world_info::TimedWorldInfo, CoreError, CoreResult, DataRoot};
use std::fs;
use std::path::PathBuf;

fn safe_id(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

pub struct TimedWorldInfoStore {
    root: PathBuf,
}

impl TimedWorldInfoStore {
    pub fn new(data: &DataRoot) -> Self {
        let root = data.root().join("state").join("timed-world-info");
        let _ = fs::create_dir_all(&root);
        Self { root }
    }

    fn path(&self, chat_id: &str) -> PathBuf {
        self.root.join(format!("{}.json", safe_id(chat_id)))
    }

    pub fn load(&self, chat_id: &str) -> TimedWorldInfo {
        if chat_id.trim().is_empty() {
            return TimedWorldInfo::default();
        }
        let p = self.path(chat_id);
        match fs::read_to_string(&p) {
            Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
            Err(_) => TimedWorldInfo::default(),
        }
    }

    pub fn save(&self, chat_id: &str, state: &TimedWorldInfo) -> CoreResult<()> {
        if chat_id.trim().is_empty() {
            return Ok(());
        }
        let _ = fs::create_dir_all(&self.root);
        let raw = serde_json::to_string_pretty(state)
            .map_err(|e| CoreError::BadRequest(format!("timed wi serialize: {e}")))?;
        fs::write(self.path(chat_id), raw)?;
        Ok(())
    }
}
