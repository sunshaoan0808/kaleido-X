//! 创作罗盘（T2）：全书承诺 + 近期目标，per-work 持久化。
//!
//! 参考 Openwrite 的 author_intent.md / current_focus.md：剧场/故事馆长会话续写几十轮后
//! 容易丢失方向，需要显式的「全书不可违背承诺」与「近期写作目标」持续注入上下文。
//!
//! - `Compass`：`{ version, author_intent, current_focus }`，挂载到 TavernSession 的
//!   ActorStateSystem（系统状态）；`build_context_text()` 注入时置顶输出。
//! - 未设置时字段为空字符串，`render_block()` 不产出任何内容（零注入）。
//! - 持久化：版本化 JSON，存放在 work 数据目录 `$DATA/works/{work_id}/compass.json`
//!   （与 WorksFs 的 per-work 数据根一致）。

use crate::{CoreError, CoreResult, DataRoot};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// compass.json 的 schema 版本。
pub const COMPASS_SCHEMA_VERSION: u32 = 1;

/// author_intent / current_focus 单字段长度上限（字符数，2000；空字符串允许）。
pub const COMPASS_MAX_LEN: usize = 2000;

/// 版本化 JSON 存储文件名（位于 work 目录下）。
pub const COMPASS_FILE_NAME: &str = "compass.json";

fn default_compass_schema_version() -> u32 {
    COMPASS_SCHEMA_VERSION
}

/// 创作罗盘：全书不可违背承诺（author_intent）+ 近期写作目标（current_focus）。
///
/// 空字段 = 未设置，注入时跳过对应段。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Compass {
    #[serde(default = "default_compass_schema_version")]
    pub version: u32,
    #[serde(default)]
    pub author_intent: String,
    #[serde(default)]
    pub current_focus: String,
}

impl Default for Compass {
    fn default() -> Self {
        Self::empty()
    }
}

impl Compass {
    pub fn new(author_intent: impl Into<String>, current_focus: impl Into<String>) -> Self {
        Self {
            version: COMPASS_SCHEMA_VERSION,
            author_intent: author_intent.into(),
            current_focus: current_focus.into(),
        }
    }

    /// 空罗盘：两个字段均为空字符串（不设默认叙事承诺/目标）。
    pub fn empty() -> Self {
        Self {
            version: COMPASS_SCHEMA_VERSION,
            author_intent: String::new(),
            current_focus: String::new(),
        }
    }

    /// 两个字段都空白 = 未设置。
    pub fn is_empty(&self) -> bool {
        self.author_intent.trim().is_empty() && self.current_focus.trim().is_empty()
    }

    /// 长度校验：单字段 ≤ COMPASS_MAX_LEN。空字符串允许。
    pub fn validate(&self) -> CoreResult<()> {
        if self.author_intent.chars().count() > COMPASS_MAX_LEN {
            return Err(CoreError::BadRequest(format!(
                "authorIntent exceeds max length {COMPASS_MAX_LEN}"
            )));
        }
        if self.current_focus.chars().count() > COMPASS_MAX_LEN {
            return Err(CoreError::BadRequest(format!(
                "currentFocus exceeds max length {COMPASS_MAX_LEN}"
            )));
        }
        Ok(())
    }

    /// 注入文本（置顶段）：author_intent 非空 →「【全书承诺】…」；current_focus 非空 →「【近期目标】…」。
    /// 空字段不输出对应段；全空返回空串（不注入）。
    pub fn render_block(&self) -> String {
        let ai = self.author_intent.trim();
        let cf = self.current_focus.trim();
        if ai.is_empty() && cf.is_empty() {
            return String::new();
        }
        let mut out = String::from("## 创作罗盘\n");
        if !ai.is_empty() {
            out.push_str(&format!("【全书承诺】{ai}\n"));
        }
        if !cf.is_empty() {
            out.push_str(&format!("【近期目标】{cf}\n"));
        }
        out
    }

    pub fn from_json(raw: &str) -> CoreResult<Self> {
        serde_json::from_str(raw).map_err(Into::into)
    }

    pub fn to_json(&self) -> CoreResult<String> {
        serde_json::to_string_pretty(self).map_err(Into::into)
    }
}

/// Work 目录校验：与 `$DATA/works/{id}` 的 id 规则一致，防路径逃逸。
fn validate_work_id(id: &str) -> CoreResult<String> {
    let s = id.trim();
    if s.is_empty()
        || s.len() > 128
        || s.contains('/')
        || s.contains('\\')
        || s.contains("..")
        || !s
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(CoreError::BadRequest("invalid work_id".into()));
    }
    Ok(s.to_string())
}

fn write_atomic(path: &Path, body: &str) -> CoreResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(body.as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

/// per-work 创作罗盘存储：`$DATA/works/{work_id}/compass.json`，与 WorksFs 的 work 目录一致。
#[derive(Clone)]
pub struct CompassStore {
    data: DataRoot,
    lock: Arc<Mutex<()>>,
}

impl CompassStore {
    pub fn new(data: DataRoot) -> Self {
        let _ = data.ensure_layout();
        Self {
            data,
            lock: Arc::new(Mutex::new(())),
        }
    }

    fn path_for(&self, work_id: &str) -> CoreResult<PathBuf> {
        let id = validate_work_id(work_id)?;
        Ok(self
            .data
            .root()
            .join("works")
            .join(id)
            .join(COMPASS_FILE_NAME))
    }

    /// 读取罗盘；文件不存在返回空罗盘（字段为空字符串，不设默认）。
    pub fn load(&self, work_id: &str) -> CoreResult<Compass> {
        let _g = self.lock.lock();
        let path = self.path_for(work_id)?;
        if !path.exists() {
            return Ok(Compass::empty());
        }
        let raw = fs::read_to_string(path)?;
        Compass::from_json(&raw)
    }

    /// 校验 + 写回版本化 JSON（原子写）。返回落盘后的罗盘。
    pub fn save(&self, work_id: &str, compass: &Compass) -> CoreResult<Compass> {
        let _g = self.lock.lock();
        compass.validate()?;
        let path = self.path_for(work_id)?;
        write_atomic(&path, &compass.to_json()?)?;
        Ok(compass.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub struct Tmp {
        path: PathBuf,
    }

    impl Tmp {
        pub fn new() -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!("kaleido-compass-test-{nanos}"));
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }
        pub fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn empty_is_empty_and_not_injected() {
        let c = Compass::empty();
        assert!(c.is_empty());
        assert_eq!(c.render_block(), "");
        assert_eq!(c.author_intent, "");
        assert_eq!(c.current_focus, "");
    }

    #[test]
    fn round_trip_persist_and_reload() {
        let tmp = Tmp::new();
        let data = DataRoot::new(tmp.path()).unwrap();
        let store = CompassStore::new(data);
        let work = "work-uuid-123";

        // 未设置时返回空罗盘
        let missing = store.load(work).unwrap();
        assert!(missing.is_empty());
        assert_eq!(missing.render_block(), "");

        // 保存
        let c = Compass::new("主角必须活到最后", "本周写完第三章");
        c.validate().unwrap();
        let saved = store.save(work, &c).unwrap();
        assert_eq!(saved.version, COMPASS_SCHEMA_VERSION);
        assert_eq!(saved.author_intent, "主角必须活到最后");
        assert_eq!(saved.current_focus, "本周写完第三章");

        // 重新加载
        let reloaded = store.load(work).unwrap();
        assert_eq!(reloaded, c);
        assert!(!reloaded.render_block().is_empty());

        // 磁盘上是 camelCase + version 版本化 JSON。
        let raw = fs::read_to_string(store.path_for(work).unwrap()).unwrap();
        assert!(raw.contains("\"version\": 1"));
        assert!(raw.contains("\"authorIntent\""));
        assert!(raw.contains("\"currentFocus\""));
        assert!(raw.contains("主角必须活到最后"));
    }

    #[test]
    fn saving_empty_fields_is_allowed_and_clears() {
        let tmp = Tmp::new();
        let data = DataRoot::new(tmp.path()).unwrap();
        let store = CompassStore::new(data);
        let work = "work-1";

        store.save(work, &Compass::new("承诺", "目标")).unwrap();
        let cleared = Compass::new("", "");
        cleared.validate().unwrap();
        store.save(work, &cleared).unwrap();

        let reloaded = store.load(work).unwrap();
        assert!(reloaded.is_empty());
        assert_eq!(reloaded.render_block(), "");
    }

    #[test]
    fn render_block_fieldwise() {
        // 仅 author_intent。
        let c = Compass::new("全书承诺 A", "");
        let block = c.render_block();
        assert!(block.contains("## 创作罗盘"));
        assert!(block.contains("【全书承诺】全书承诺 A"));
        assert!(!block.contains("【近期目标】"));

        // 仅 current_focus。
        let c = Compass::new("", "近期目标 B");
        let block = c.render_block();
        assert!(block.contains("## 创作罗盘"));
        assert!(block.contains("【近期目标】近期目标 B"));
        assert!(!block.contains("【全书承诺】"));

        // 都设置。
        let c = Compass::new("A", "B");
        let block = c.render_block();
        assert!(block.contains("【全书承诺】A"));
        assert!(block.contains("【近期目标】B"));
        // 罗盘段位于最前。
        assert!(block.starts_with("## 创作罗盘\n"));
    }

    #[test]
    fn length_limit_ok_and_rejected() {
        let ok = Compass::new("x".repeat(COMPASS_MAX_LEN), "y".repeat(COMPASS_MAX_LEN));
        assert!(ok.validate().is_ok());

        let over = Compass::new("x".repeat(COMPASS_MAX_LEN + 1), "");
        let err = over.validate().unwrap_err();
        assert!(matches!(err, CoreError::BadRequest(_)));
        assert!(format!("{err}").contains("authorIntent"));

        let over_focus = Compass::new("", "y".repeat(COMPASS_MAX_LEN + 1));
        assert!(over_focus.validate().is_err());

        // empty allowed
        assert!(Compass::empty().validate().is_ok());
    }

    #[test]
    fn invalid_work_id_rejected() {
        let tmp = Tmp::new();
        let data = DataRoot::new(tmp.path()).unwrap();
        let store = CompassStore::new(data);
        for bad in ["", "..", "a/b", "a\\b", "a..b", "s p a c e"] {
            assert!(store.load(bad).is_err(), "work_id `{bad}` should be rejected");
        }
    }

    #[test]
    fn stores_are_isolated_per_work() {
        let tmp = Tmp::new();
        let data = DataRoot::new(tmp.path()).unwrap();
        let store = CompassStore::new(data);
        store.save("work-a", &Compass::new("A1", "A2")).unwrap();
        let b = store.load("work-b").unwrap();
        assert!(b.is_empty());
    }
}