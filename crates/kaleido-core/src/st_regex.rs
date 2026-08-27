//! SillyTavern-compatible regex script engine.
//!
//! Ported from `public/scripts/extensions/regex/engine.js`:
//! `getRegexedString`, `runRegexScript`, `regex_placement`, markdownOnly/promptOnly,
//! minDepth/maxDepth, {{match}} / $1 groups.
//!
//! Not ported (intentionally thin):
//! - Overlay replace strategy
//! - Full macro `substituteParams` (only {{match}} + $n / $<name>)
//! - Preset/global script stores (Kaleido uses character-scoped scripts on card fields)

use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Mutex;

use crate::st_world_info::parse_regex_from_string;

/// ST `regex_placement`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum RegexPlacement {
    MdDisplay = 0,
    UserInput = 1,
    AiOutput = 2,
    SlashCommand = 3,
    WorldInfo = 5,
    Reasoning = 6,
}

#[derive(Debug, Clone)]
pub struct RegexScript {
    pub id: String,
    pub script_name: String,
    pub find_regex: String,
    pub replace_string: String,
    pub trim_strings: Vec<String>,
    pub placement: Vec<i32>,
    pub disabled: bool,
    pub markdown_only: bool,
    pub prompt_only: bool,
    pub run_on_edit: bool,
    pub min_depth: Option<i32>,
    pub max_depth: Option<i32>,
    /// 0=none 1=raw 2=escaped — Kaleido currently treats all as none (no macros)
    pub substitute_regex: i32,
}

fn as_i32_list(v: Option<&Value>) -> Vec<i32> {
    match v {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|x| {
                x.as_i64()
                    .map(|n| n as i32)
                    .or_else(|| x.as_f64().map(|n| n as i32))
                    .or_else(|| x.as_str().and_then(|s| s.parse().ok()))
            })
            .collect(),
        Some(Value::Number(n)) => n.as_i64().map(|x| vec![x as i32]).unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn bool_f(obj: &Value, keys: &[&str], d: bool) -> bool {
    for k in keys {
        if let Some(v) = obj.get(*k) {
            if let Some(b) = v.as_bool() {
                return b;
            }
        }
    }
    d
}

fn str_f(obj: &Value, keys: &[&str]) -> String {
    for k in keys {
        if let Some(s) = obj.get(*k).and_then(|v| v.as_str()) {
            return s.to_string();
        }
    }
    String::new()
}

fn i32_opt(obj: &Value, keys: &[&str]) -> Option<i32> {
    for k in keys {
        if let Some(v) = obj.get(*k) {
            if v.is_null() {
                return None;
            }
            if let Some(n) = v.as_i64() {
                return Some(n as i32);
            }
            if let Some(n) = v.as_f64() {
                return Some(n as i32);
            }
        }
    }
    None
}

pub fn parse_regex_script(raw: &Value) -> Option<RegexScript> {
    if !raw.is_object() {
        return None;
    }
    let find = str_f(raw, &["findRegex", "find_regex"]);
    if find.trim().is_empty() {
        return None;
    }
    let mut placement = as_i32_list(raw.get("placement"));
    if placement.is_empty() {
        placement = vec![
            RegexPlacement::UserInput as i32,
            RegexPlacement::AiOutput as i32,
        ];
    }
    let trim = match raw.get("trimStrings").or_else(|| raw.get("trim_strings")) {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|x| x.as_str().map(|s| s.to_string()))
            .collect(),
        _ => Vec::new(),
    };
    Some(RegexScript {
        id: str_f(raw, &["id"]),
        script_name: str_f(raw, &["scriptName", "script_name", "name"]),
        find_regex: find,
        replace_string: str_f(raw, &["replaceString", "replace_string"]),
        trim_strings: trim,
        placement,
        disabled: bool_f(raw, &["disabled", "disable"], false),
        markdown_only: bool_f(raw, &["markdownOnly", "markdown_only"], false),
        prompt_only: bool_f(raw, &["promptOnly", "prompt_only"], false),
        run_on_edit: bool_f(raw, &["runOnEdit", "run_on_edit"], true),
        min_depth: i32_opt(raw, &["minDepth", "min_depth"]),
        max_depth: i32_opt(raw, &["maxDepth", "max_depth"]),
        substitute_regex: i32_opt(raw, &["substituteRegex", "substitute_regex"]).unwrap_or(0),
    })
}

pub fn scripts_from_value(v: &Value) -> Vec<RegexScript> {
    let mut out = Vec::new();
    if let Some(arr) = v.as_array() {
        for item in arr {
            if let Some(s) = parse_regex_script(item) {
                out.push(s);
            }
        }
    } else if let Some(obj) = v.as_object() {
        // maybe { scripts: [...] } or map
        if let Some(arr) = obj.get("regex_scripts").or_else(|| obj.get("regexScripts")) {
            return scripts_from_value(arr);
        }
        for (_k, item) in obj {
            if let Some(s) = parse_regex_script(item) {
                out.push(s);
            }
        }
    }
    out
}

pub fn scripts_from_card_fields(fields: Option<&Value>) -> Vec<RegexScript> {
    let Some(fields) = fields else {
        return vec![];
    };
    if let Some(v) = fields
        .get("stRegexScripts")
        .or_else(|| fields.get("regex_scripts"))
        .or_else(|| fields.get("regexScripts"))
    {
        return scripts_from_value(v);
    }
    vec![]
}

/// Compile findRegex with ST semantics (`/pat/flags` or raw).
pub fn compile_find_regex(find: &str) -> Option<Regex> {
    let find = find.trim();
    if find.is_empty() {
        return None;
    }
    if let Some(re) = parse_regex_from_string(find) {
        return Some(re);
    }
    // ST regexFromString looser fallback: /pat/flags OR bare
    if find.starts_with('/') {
        // already tried strict; try looser
        if let Some(m) = lazy_regex_split(find) {
            return build_re(&m.0, &m.1);
        }
    }
    // bare pattern → default global-ish (rust doesn't need g)
    Regex::new(find).ok()
}

fn lazy_regex_split(input: &str) -> Option<(String, String)> {
    // /(\/?)(.+)\1([a-z]*)/i  simplified
    if !input.starts_with('/') {
        return None;
    }
    let last = input.rfind('/')?;
    if last == 0 {
        return None;
    }
    Some((input[1..last].to_string(), input[last + 1..].to_string()))
}

fn build_re(pattern: &str, flags: &str) -> Option<Regex> {
    let mut b = regex::RegexBuilder::new(pattern);
    for f in flags.chars() {
        match f {
            'i' => {
                b.case_insensitive(true);
            }
            'm' => {
                b.multi_line(true);
            }
            's' => {
                b.dot_matches_new_line(true);
            }
            _ => {}
        }
    }
    b.build().ok()
}

// Simple process-wide compile cache
lazy_static::lazy_static! {
    static ref REGEX_CACHE: Mutex<HashMap<String, Option<Regex>>> = Mutex::new(HashMap::new());
}

fn cached_compile(find: &str) -> Option<Regex> {
    let mut guard = REGEX_CACHE.lock().ok()?;
    if let Some(hit) = guard.get(find) {
        return hit.clone();
    }
    let compiled = compile_find_regex(find);
    guard.insert(find.to_string(), compiled.clone());
    // crude cap
    if guard.len() > 1000 {
        guard.clear();
    }
    compiled
}

fn filter_trim(s: &str, trims: &[String]) -> String {
    let mut out = s.to_string();
    for t in trims {
        if t.is_empty() {
            continue;
        }
        out = out.replace(t, "");
    }
    out
}

/// ST `runRegexScript`
pub fn run_regex_script(script: &RegexScript, raw: &str) -> String {
    if script.disabled || script.find_regex.is_empty() || raw.is_empty() {
        return raw.to_string();
    }
    let Some(re) = cached_compile(&script.find_regex) else {
        return raw.to_string();
    };
    let replace_template = script
        .replace_string
        .replace("{{match}}", "$0")
        .replace("{{MATCH}}", "$0");

    // Manual replace to support trimStrings on groups like ST
    let mut out = String::new();
    let mut last = 0;
    for caps in re.captures_iter(raw) {
        let m = caps.get(0).unwrap();
        out.push_str(&raw[last..m.start()]);
        // Build replacement with $n / $<name>
        let mut rep = replace_template.clone();
        // numbered
        for i in 0..caps.len() {
            if let Some(g) = caps.get(i) {
                let filtered = filter_trim(g.as_str(), &script.trim_strings);
                let from = format!("${i}");
                rep = rep.replace(&from, &filtered);
            }
        }
        // named
        for name in re.capture_names().flatten() {
            if let Some(g) = caps.name(name) {
                let filtered = filter_trim(g.as_str(), &script.trim_strings);
                rep = rep.replace(&format!("$<{name}>"), &filtered);
            }
        }
        out.push_str(&rep);
        last = m.end();
        // avoid zero-length infinite
        if m.start() == m.end() {
            if last < raw.len() {
                out.push(raw[last..].chars().next().unwrap());
                last += raw[last..].chars().next().unwrap().len_utf8();
            } else {
                break;
            }
        }
    }
    out.push_str(&raw[last..]);
    out
}

/// ST `getRegexedString`
pub fn get_regexed_string(
    raw: &str,
    placement: RegexPlacement,
    scripts: &[RegexScript],
    is_markdown: bool,
    is_prompt: bool,
    depth: Option<i32>,
) -> String {
    if raw.is_empty() {
        return String::new();
    }
    let mut final_s = raw.to_string();
    let place = placement as i32;
    for script in scripts {
        if script.disabled {
            continue;
        }
        let applies = (script.markdown_only && is_markdown)
            || (script.prompt_only && is_prompt)
            || (!script.markdown_only && !script.prompt_only && !is_markdown && !is_prompt);
        if !applies {
            continue;
        }
        if let Some(d) = depth {
            if let Some(min) = script.min_depth {
                if min >= -1 && d < min {
                    continue;
                }
            }
            if let Some(max) = script.max_depth {
                if max >= 0 && d > max {
                    continue;
                }
            }
        }
        if !script.placement.contains(&place) {
            continue;
        }
        final_s = run_regex_script(script, &final_s);
    }
    final_s
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn hide_ooc_ai_output() {
        let script = parse_regex_script(&json!({
            "scriptName": "hide ooc",
            "findRegex": "/\\(OOC:.*?\\)/gi",
            "replaceString": "",
            "placement": [2],
            "disabled": false
        }))
        .unwrap();
        let s = get_regexed_string(
            "Hello (OOC: secret) there",
            RegexPlacement::AiOutput,
            &[script],
            false,
            false,
            None,
        );
        assert_eq!(s, "Hello  there");
    }

    #[test]
    fn prompt_only_skips_display() {
        let script = parse_regex_script(&json!({
            "findRegex": "/secret/g",
            "replaceString": "XXX",
            "placement": [2],
            "promptOnly": true
        }))
        .unwrap();
        let display = get_regexed_string(
            "secret",
            RegexPlacement::AiOutput,
            &[script],
            false,
            false,
            None,
        );
        assert_eq!(display, "secret");
    }

    #[test]
    fn prompt_only_applies_on_prompt() {
        let script = parse_regex_script(&json!({
            "findRegex": "/secret/g",
            "replaceString": "XXX",
            "placement": [2],
            "promptOnly": true
        }))
        .unwrap();
        let prompt = get_regexed_string(
            "secret",
            RegexPlacement::AiOutput,
            &[script],
            false,
            true,
            None,
        );
        assert_eq!(prompt, "XXX");
    }

    #[test]
    fn capture_groups() {
        let script = parse_regex_script(&json!({
            "findRegex": "/\\*(.+?)\\*/g",
            "replaceString": "「$1」",
            "placement": [2]
        }))
        .unwrap();
        let s = run_regex_script(&script, "he *smiles* softly");
        assert!(s.contains("「smiles」"));
    }
}
