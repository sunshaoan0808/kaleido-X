//! Global / preset regex script library (ST-style, Kaleido W6).
//!
//! Storage: `$KALEIDO_DATA/state/regex-library.json`
//! Runtime merge: library ∪ character-scoped scripts.
//! Default priority: **card overrides library** on same `id` or `scriptName`
//! (library scripts run first; unique card scripts append; colliding card
//! entries replace the library copy so card wins).

use crate::st_regex::{parse_regex_script, scripts_from_value, RegexScript};
use crate::{CoreError, CoreResult, DataRoot};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;

fn safe_write(path: &PathBuf, raw: &str) -> CoreResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, raw)?;
    Ok(())
}

/// On-disk shape (round-trip ST script objects).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegexLibraryFile {
    /// `card_over_library` (default) | `library_over_card`
    #[serde(default = "default_priority")]
    pub priority: String,
    /// ST regex script objects (raw JSON for FE round-trip).
    #[serde(default)]
    pub scripts: Vec<Value>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

fn default_priority() -> String {
    "card_over_library".into()
}

impl Default for RegexLibraryFile {
    fn default() -> Self {
        Self {
            priority: default_priority(),
            scripts: Vec::new(),
            updated_at: None,
        }
    }
}

#[derive(Clone)]
pub struct RegexLibraryStore {
    path: PathBuf,
}

impl RegexLibraryStore {
    pub fn new(data: &DataRoot) -> Self {
        let dir = data.root().join("state");
        let _ = fs::create_dir_all(&dir);
        Self {
            path: dir.join("regex-library.json"),
        }
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    pub fn load(&self) -> RegexLibraryFile {
        match fs::read_to_string(&self.path) {
            Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
            Err(_) => RegexLibraryFile::default(),
        }
    }

    pub fn save(&self, mut file: RegexLibraryFile) -> CoreResult<RegexLibraryFile> {
        if file.priority.trim().is_empty() {
            file.priority = default_priority();
        }
        // drop unparseable junk but keep raw that parses
        let mut cleaned = Vec::new();
        for s in file.scripts.drain(..) {
            if parse_regex_script(&s).is_some() {
                cleaned.push(normalize_script_value(s));
            }
        }
        file.scripts = cleaned;
        file.updated_at = Some(chrono_like_now());
        let raw = serde_json::to_string_pretty(&file)
            .map_err(|e| CoreError::BadRequest(format!("regex-library serialize: {e}")))?;
        safe_write(&self.path, &raw)?;
        Ok(file)
    }

    pub fn put_scripts(
        &self,
        scripts: Vec<Value>,
        priority: Option<String>,
    ) -> CoreResult<RegexLibraryFile> {
        let mut file = self.load();
        if let Some(p) = priority {
            if !p.trim().is_empty() {
                file.priority = p;
            }
        }
        file.scripts = scripts;
        self.save(file)
    }

    /// Import: `replace=true` overwrites; else merge by id/scriptName (incoming wins).
    pub fn import_scripts(
        &self,
        incoming: Vec<Value>,
        replace: bool,
        priority: Option<String>,
    ) -> CoreResult<RegexLibraryFile> {
        if replace {
            return self.put_scripts(incoming, priority);
        }
        let mut file = self.load();
        if let Some(p) = priority {
            if !p.trim().is_empty() {
                file.priority = p;
            }
        }
        for s in incoming {
            if parse_regex_script(&s).is_none() {
                continue;
            }
            let s = normalize_script_value(s);
            let key = script_key(&s);
            if let Some(pos) = file.scripts.iter().position(|e| script_key(e) == key) {
                file.scripts[pos] = s;
            } else {
                file.scripts.push(s);
            }
        }
        self.save(file)
    }

    pub fn parsed_scripts(&self) -> Vec<RegexScript> {
        let file = self.load();
        file.scripts
            .iter()
            .filter_map(|v| parse_regex_script(v))
            .collect()
    }

    pub fn to_public(&self) -> Value {
        let file = self.load();
        json!({
            "ok": true,
            "priority": file.priority,
            "scripts": file.scripts,
            "count": file.scripts.len(),
            "updatedAt": file.updated_at,
            "path": "state/regex-library.json",
        })
    }
}

fn chrono_like_now() -> String {
    // avoid chrono dep — RFC-ish UTC from system time
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{secs}")
}

fn script_key(v: &Value) -> String {
    let id = v
        .get("id")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if !id.is_empty() {
        return format!("id:{id}");
    }
    let name = v
        .get("scriptName")
        .or_else(|| v.get("script_name"))
        .or_else(|| v.get("name"))
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if !name.is_empty() {
        return format!("name:{name}");
    }
    let find = v
        .get("findRegex")
        .or_else(|| v.get("find_regex"))
        .and_then(|x| x.as_str())
        .unwrap_or("");
    format!("find:{find}")
}

fn normalize_script_value(mut v: Value) -> Value {
    // ensure id if missing
    if v.get("id").and_then(|x| x.as_str()).unwrap_or("").is_empty() {
        let name = v
            .get("scriptName")
            .or_else(|| v.get("script_name"))
            .or_else(|| v.get("name"))
            .and_then(|x| x.as_str())
            .unwrap_or("script")
            .to_string();
        let find = v
            .get("findRegex")
            .or_else(|| v.get("find_regex"))
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let need_name = v.get("scriptName").is_none() && v.get("script_name").is_none();
        if let Some(obj) = v.as_object_mut() {
            let id = format!("rx-{:x}", simple_hash(&format!("{name}|{find}")));
            obj.insert("id".into(), json!(id));
            if need_name {
                obj.insert("scriptName".into(), json!(name));
            }
        }
    }
    v
}

fn simple_hash(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Extract script array from various import body shapes.
pub fn scripts_from_import_body(v: &Value) -> Vec<Value> {
    if let Some(arr) = v.as_array() {
        return arr.clone();
    }
    if let Some(obj) = v.as_object() {
        for k in [
            "scripts",
            "regex_scripts",
            "regexScripts",
            "stRegexScripts",
            "entries",
        ] {
            if let Some(arr) = obj.get(k).and_then(|x| x.as_array()) {
                return arr.clone();
            }
        }
        // single script object
        if obj.contains_key("findRegex") || obj.contains_key("find_regex") {
            return vec![v.clone()];
        }
    }
    // try scripts_from_value then re-serialize via raw keep — fallback empty
    let parsed = scripts_from_value(v);
    parsed
        .into_iter()
        .map(|s| {
            json!({
                "id": s.id,
                "scriptName": s.script_name,
                "findRegex": s.find_regex,
                "replaceString": s.replace_string,
                "trimStrings": s.trim_strings,
                "placement": s.placement,
                "disabled": s.disabled,
                "markdownOnly": s.markdown_only,
                "promptOnly": s.prompt_only,
                "runOnEdit": s.run_on_edit,
                "minDepth": s.min_depth,
                "maxDepth": s.max_depth,
                "substituteRegex": s.substitute_regex,
            })
        })
        .collect()
}

fn script_identity(s: &RegexScript) -> String {
    if !s.id.trim().is_empty() {
        return format!("id:{}", s.id.trim());
    }
    if !s.script_name.trim().is_empty() {
        return format!("name:{}", s.script_name.trim());
    }
    format!("find:{}", s.find_regex)
}

/// Merge library + card scripts.
///
/// - `card_over_library` (default): start from library, replace/append card by identity.
/// - `library_over_card`: start from card, replace/append library by identity.
pub fn merge_regex_scripts(
    library: &[RegexScript],
    card: &[RegexScript],
    priority: &str,
) -> Vec<RegexScript> {
    let card_wins = !matches!(
        priority.trim().to_ascii_lowercase().as_str(),
        "library_over_card" | "library-over-card" | "library"
    );
    let (base, overlay) = if card_wins {
        (library, card)
    } else {
        (card, library)
    };
    let mut out: Vec<RegexScript> = base.to_vec();
    for s in overlay {
        let key = script_identity(s);
        if let Some(pos) = out.iter().position(|e| script_identity(e) == key) {
            out[pos] = s.clone();
        } else {
            out.push(s.clone());
        }
    }
    out
}

/// Convenience: library file + optional card fields → merged runtime scripts.
pub fn resolve_runtime_scripts(
    store: &RegexLibraryStore,
    card_fields: Option<&Value>,
) -> Vec<RegexScript> {
    let file = store.load();
    let lib: Vec<RegexScript> = file
        .scripts
        .iter()
        .filter_map(|v| parse_regex_script(v))
        .collect();
    let card = crate::st_regex::scripts_from_card_fields(card_fields);
    merge_regex_scripts(&lib, &card, &file.priority)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::st_regex::parse_regex_script;
    use serde_json::json;

    #[test]
    fn card_overrides_library_same_name() {
        let lib = parse_regex_script(&json!({
            "scriptName": "hide",
            "findRegex": "/foo/g",
            "replaceString": "LIB",
            "placement": [2]
        }))
        .unwrap();
        let card = parse_regex_script(&json!({
            "scriptName": "hide",
            "findRegex": "/foo/g",
            "replaceString": "CARD",
            "placement": [2]
        }))
        .unwrap();
        let m = merge_regex_scripts(&[lib], &[card], "card_over_library");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].replace_string, "CARD");
    }

    #[test]
    fn library_unique_kept() {
        let lib = parse_regex_script(&json!({
            "id": "L1",
            "findRegex": "/a/g",
            "replaceString": "A",
            "placement": [1]
        }))
        .unwrap();
        let card = parse_regex_script(&json!({
            "id": "C1",
            "findRegex": "/b/g",
            "replaceString": "B",
            "placement": [2]
        }))
        .unwrap();
        let m = merge_regex_scripts(&[lib], &[card], "card_over_library");
        assert_eq!(m.len(), 2);
    }
}
