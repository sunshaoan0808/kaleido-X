//! SillyTavern-compatible World Info / Lorebook scanner.
//!
//! Ported from SillyTavern `public/scripts/world-info.js` (`checkWorldInfo`,
//! `WorldInfoBuffer.matchKeys`, `convertCharacterBook`, inclusion groups,
//! probability, budget, recursion).
//!
//! Scope vs full ST:
//! - ✅ constant / keys / secondary + selectiveLogic / probability / order
//! - ✅ caseSensitive / matchWholeWords / scanDepth / inclusion groups
//! - ✅ position before|after (0|1); atDepth folded into after with marker
//! - ✅ budget (% of maxContext, char≈token*4 heuristic) + ignoreBudget
//! - ✅ one-level recursion when `recursive` setting on
//! - ✅ sticky/cooldown/delay timed effects (per-chat TimedWorldInfo store)
//! - ✅ AN top/bottom + atDepth buckets in scan result (injected around card)
//! - ✅ @@activate / @@dont_activate content decorators
//! - ✅ EM top/bottom + named outlet buckets
//! - ✅ characterFilter / generation triggers / vectorized skip
//! - ✅ {{user}}/{{char}} macros; multi-slot EM/depth/outlet in WiPromptSlots
//! - ✅ automationId collected on activation (returned to client)
//! - ✅ EM <START> blocks → user/assistant example pairs
//! - ⚠️ token budget: improved CJK/word heuristic (no external tokenizer crate)

use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

/// ST `world_info_logic`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum SelectiveLogic {
    AndAny = 0,
    NotAll = 1,
    NotAny = 2,
    AndAll = 3,
}

impl SelectiveLogic {
    pub fn from_i32(v: i32) -> Self {
        match v {
            1 => Self::NotAll,
            2 => Self::NotAny,
            3 => Self::AndAll,
            _ => Self::AndAny,
        }
    }
}

/// ST `world_info_position` (subset we honour in prompt assembly)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum WiPosition {
    Before = 0,
    After = 1,
    AnTop = 2,
    AnBottom = 3,
    AtDepth = 4,
    EmTop = 5,
    EmBottom = 6,
    Outlet = 7,
}

impl WiPosition {
    pub fn from_i32(v: i32) -> Self {
        match v {
            1 => Self::After,
            2 => Self::AnTop,
            3 => Self::AnBottom,
            4 => Self::AtDepth,
            5 => Self::EmTop,
            6 => Self::EmBottom,
            7 => Self::Outlet,
            _ => Self::Before,
        }
    }
}


#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CharacterFilter {
    #[serde(default)]
    pub names: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub is_exclude: bool,
}

/// ST-like global scan context (trigger + persona/char names for filters & macros).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WiScanContext {
    /// Generation trigger id (ST: normal, continue, impersonate, ...). Empty = no filter.
    #[serde(default)]
    pub trigger: String,
    /// Active character display name / filename for characterFilter.names
    #[serde(default)]
    pub character_name: String,
    /// Active character tags for characterFilter.tags
    #[serde(default)]
    pub character_tags: Vec<String>,
    /// Macro {{user}} / <USER>
    #[serde(default = "default_user")]
    pub user_name: String,
    /// Macro {{char}} / <BOT> / <CHAR>
    #[serde(default)]
    pub char_name: String,
    /// Optional max context override for budget (0 = caller max_context_tokens)
    #[serde(default)]
    pub max_context_tokens: i32,
    /// Pre-ranked vector hits from server embed path (W5). Keyed activation by world.uid.
    /// When non-empty, entries with `vectorized=true` matching a hit are activated
    /// with reason `vector:{score}` instead of being skipped.
    #[serde(default)]
    pub vector_hits: Vec<crate::VectorHit>,
    /// Optional override for vector activation knobs (threshold/top_k/enabled).
    #[serde(default)]
    pub vector_settings: Option<crate::VectorActivationSettings>,
}

fn default_user() -> String {
    "User".into()
}

/// Multi-slot prompt pieces matching ST PromptManager / extension_prompts layout.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WiPromptSlots {
    pub world_info_before: String,
    pub world_info_after: String,
    pub an_before: String,
    pub an_after: String,
    pub em_before: String,
    pub em_after: String,
    pub depth_entries: Vec<WiDepthEntry>,
    pub outlet_entries: Vec<WiOutletEntry>,
    /// Ready-to-merge chat injections: insert as extra messages (role/content/depth).
    /// depth = messages from end (0 = after last). role: system|user|assistant
    #[serde(default)]
    pub chat_injections: Vec<WiChatInjection>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WiChatInjection {
    pub role: String,
    pub content: String,
    /// 0 = newest side (append near end); higher = further back in history
    pub depth: i32,
    pub kind: String, // depth | em_before | em_after | outlet
}

#[derive(Debug, Clone)]
pub struct WiEntry {
    pub uid: String,
    pub world: String,
    pub keys: Vec<String>,
    pub keysecondary: Vec<String>,
    pub content: String,
    pub comment: String,
    pub constant: bool,
    pub disable: bool,
    pub selective: bool,
    pub selective_logic: SelectiveLogic,
    pub order: i32,
    pub position: WiPosition,
    pub depth: i32,
    pub probability: f64,
    pub use_probability: bool,
    pub scan_depth: Option<i32>,
    pub case_sensitive: Option<bool>,
    pub match_whole_words: Option<bool>,
    pub group: String,
    pub group_override: bool,
    pub group_weight: f64,
    pub use_group_scoring: Option<bool>,
    pub exclude_recursion: bool,
    pub prevent_recursion: bool,
    pub delay_until_recursion: bool,
    pub ignore_budget: bool,
    /// Stay active for N chat messages after activation (ST extensions.sticky).
    pub sticky: Option<i32>,
    /// Suppress re-activation for N messages (ST extensions.cooldown).
    pub cooldown: Option<i32>,
    /// Do not activate until chat has at least N messages (ST extensions.delay).
    pub delay: Option<i32>,
    /// Parsed from content lines starting with @@ (ST parseDecorators).
    pub decorators: Vec<String>,
    /// Named outlet when position=outlet (ST extensions.outlet_name).
    pub outlet_name: String,
    pub character_filter: Option<CharacterFilter>,
    /// ST extensions.triggers — generation type allow-list (empty = all).
    pub triggers: Vec<String>,
    /// ST extensions.vectorized — skipped in keyword scanner (needs embed path).
    pub vectorized: bool,
    /// ST extensions.automation_id — stored for extensions; not executed here.
    pub automation_id: String,
    /// atDepth message role (0=system,1=user,2=assistant) ST extension_prompt_roles
    pub role: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WiSettings {
    /// Messages (from newest) to scan. ST default 2.
    #[serde(default = "default_depth")]
    pub depth: i32,
    /// Budget as % of max_context (ST default 25).
    #[serde(default = "default_budget_pct")]
    pub budget_pct: f64,
    /// Hard cap in *tokens* (0 = off).
    #[serde(default)]
    pub budget_cap: i32,
    #[serde(default)]
    pub recursive: bool,
    #[serde(default)]
    pub case_sensitive: bool,
    #[serde(default)]
    pub match_whole_words: bool,
    #[serde(default)]
    pub use_group_scoring: bool,
    /// Min activations (0 = off).
    #[serde(default)]
    pub min_activations: i32,
    #[serde(default)]
    pub min_activations_depth_max: i32,
    /// Max recursion steps (0 = unlimited when recursive).
    #[serde(default)]
    pub max_recursion_steps: i32,
    /// W4: token estimate mode — `heuristic` (default) | `cl100k_approx`.
    #[serde(default)]
    pub token_estimate_mode: String,
}

fn default_depth() -> i32 {
    2
}
fn default_budget_pct() -> f64 {
    25.0
}

impl Default for WiSettings {
    fn default() -> Self {
        Self {
            depth: 2,
            budget_pct: 25.0,
            budget_cap: 0,
            recursive: false,
            case_sensitive: false,
            match_whole_words: false,
            use_group_scoring: false,
            min_activations: 0,
            min_activations_depth_max: 0,
            max_recursion_steps: 0,
            token_estimate_mode: String::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WiScanResult {
    pub world_info_before: String,
    pub world_info_after: String,
    /// Author's Note top (position ANTop=2)
    #[serde(default)]
    pub an_before: String,
    /// Author's Note bottom (position ANBottom=3)
    #[serde(default)]
    pub an_after: String,
    /// atDepth entries grouped: [{depth, role, content}]
    #[serde(default)]
    pub depth_entries: Vec<WiDepthEntry>,
    /// Example Message anchor — before (EMTop=5)
    #[serde(default)]
    pub em_before: String,
    /// Example Message anchor — after (EMBottom=6)
    #[serde(default)]
    pub em_after: String,
    /// Named outlets: [{name, content}]
    #[serde(default)]
    pub outlet_entries: Vec<WiOutletEntry>,
    pub activated: Vec<ActivatedEntry>,
    pub budget_tokens: i32,
    /// W4: which estimator was used for budget accounting.
    #[serde(default)]
    pub token_estimate_mode: String,
    pub overflowed: bool,
    /// Updated timed-effect metadata to persist (sticky/cooldown maps).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timed_world_info: Option<TimedWorldInfo>,
    /// Structured multi-slot view (same data as fields above + chat_injections).
    #[serde(default)]
    pub prompt_slots: WiPromptSlots,
    #[serde(default)]
    pub skipped_vectorized: i32,
    /// How many vectorized entries were activated via embed path (W5).
    #[serde(default)]
    pub vector_activated: i32,
    #[serde(default)]
    pub skipped_filter: i32,
    #[serde(default)]
    pub skipped_trigger: i32,
    /// automationId values from activated entries (ST extension hook surface).
    #[serde(default)]
    pub automation_ids: Vec<String>,
    /// Parsed EM example message pairs (role/content), ST mesExamples style.
    #[serde(default)]
    pub example_messages: Vec<WiExampleMessage>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WiExampleMessage {
    pub role: String,
    pub content: String,
    /// before = EMTop, after = EMBottom
    #[serde(default)]
    pub anchor: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WiOutletEntry {
    pub name: String,
    pub content: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WiDepthEntry {
    pub depth: i32,
    pub role: String,
    pub content: String,
}

/// One sticky/cooldown interval — ST WITimedEffect (message-index based).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WiTimedEffect {
    pub key: String,
    pub start: i32,
    pub end: i32,
    #[serde(default)]
    pub protected: bool,
}

/// Per-chat timed WI state (ST chat_metadata.timedWorldInfo).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimedWorldInfo {
    #[serde(default)]
    pub sticky: std::collections::HashMap<String, WiTimedEffect>,
    #[serde(default)]
    pub cooldown: std::collections::HashMap<String, WiTimedEffect>,
}

impl TimedWorldInfo {
    pub fn entry_key(world: &str, uid: &str) -> String {
        format!("{world}.{uid}")
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivatedEntry {
    pub uid: String,
    pub world: String,
    pub comment: String,
    pub order: i32,
    pub position: i32,
    pub content: String,
    pub reason: String,
    /// ST extensions.automationId when present on the activated entry.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub automation_id: String,
}

// --- parsing helpers ---

fn as_str_list(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|x| x.as_str().map(|s| s.trim().to_string()))
            .filter(|s| !s.is_empty())
            .collect(),
        Some(Value::String(s)) => s
            .split(',')
            .map(|x| x.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

fn bool_field(obj: &Value, keys: &[&str], default: bool) -> bool {
    for k in keys {
        if let Some(v) = obj.get(*k) {
            if let Some(b) = v.as_bool() {
                return b;
            }
            if let Some(n) = v.as_i64() {
                return n != 0;
            }
        }
    }
    default
}

fn i32_field(obj: &Value, keys: &[&str], default: i32) -> i32 {
    for k in keys {
        if let Some(v) = obj.get(*k) {
            if let Some(n) = v.as_i64() {
                return n as i32;
            }
            if let Some(n) = v.as_f64() {
                return n as i32;
            }
            if let Some(s) = v.as_str() {
                if let Ok(n) = s.parse::<i32>() {
                    return n;
                }
            }
        }
    }
    default
}

fn f64_field(obj: &Value, keys: &[&str], default: f64) -> f64 {
    for k in keys {
        if let Some(v) = obj.get(*k) {
            if let Some(n) = v.as_f64() {
                return n;
            }
            if let Some(n) = v.as_i64() {
                return n as f64;
            }
            if let Some(s) = v.as_str() {
                if let Ok(n) = s.parse::<f64>() {
                    return n;
                }
            }
        }
    }
    default
}

fn str_field(obj: &Value, keys: &[&str]) -> String {
    for k in keys {
        if let Some(s) = obj.get(*k).and_then(|v| v.as_str()) {
            let t = s.trim();
            if !t.is_empty() {
                return t.to_string();
            }
        }
    }
    String::new()
}

/// Parse one ST-style entry (character_book entry OR world_info entry OR our stEntries).

/// ST `parseDecorators` — strip leading `@@activate` / `@@dont_activate` lines from content.
/// `@@@` form is accepted as fallback synonym → normalized to `@@…`.

/// ST `parseMesExamples` + light dialogue split → OpenAI-style example turns.
/// Blocks separated by `<START>`; lines `{{user}}:` / `{{char}}:` / `User:` / `Char:` become roles.
pub fn parse_mes_examples(examples_str: &str, user_name: &str, char_name: &str) -> Vec<(String, String)> {
    let s = examples_str.trim();
    if s.is_empty() || s == "<START>" {
        return Vec::new();
    }
    let normalized = if s.to_ascii_uppercase().contains("<START>") {
        s.to_string()
    } else {
        format!("<START>\n{s}")
    };
    let mut out = Vec::new();
    // split case-insensitive on <START>
    let lower = normalized.to_ascii_lowercase();
    let mut parts: Vec<&str> = Vec::new();
    let mut last = 0;
    let mut search = 0;
    while let Some(rel) = lower[search..].find("<start>") {
        let i = search + rel;
        if i > last {
            let chunk = normalized[last..i].trim();
            if !chunk.is_empty() {
                parts.push(&normalized[last..i]);
            }
        }
        search = i + 7;
        last = search;
    }
    if last < normalized.len() {
        parts.push(&normalized[last..]);
    }
    // first split piece before first START may be empty; ST uses slice(1)
    for block in parts {
        let block = block.trim();
        if block.is_empty() {
            continue;
        }
        out.extend(parse_example_dialogue_block(block, user_name, char_name));
    }
    out
}

fn parse_example_dialogue_block(block: &str, user_name: &str, char_name: &str) -> Vec<(String, String)> {
    let mut messages: Vec<(String, String)> = Vec::new();
    let user_l = user_name.to_lowercase();
    let char_l = char_name.to_lowercase();
    for line in block.lines() {
        let line = line.trim();
        if line.is_empty() || line.eq_ignore_ascii_case("<start>") {
            continue;
        }
        // patterns: {{user}}: / {{char}}: / User: / Char: / name:
        let (role, rest) = if let Some(r) = strip_example_prefix(line, &["{{user}}:", "{{User}}:", "<USER>:", "User:"]) {
            ("user", r)
        } else if let Some(r) = strip_example_prefix(line, &["{{char}}:", "{{Char}}:", "<BOT>:", "<CHAR>:", "Char:", "Assistant:"]) {
            ("assistant", r)
        } else if !user_l.is_empty() && line.to_lowercase().starts_with(&format!("{user_l}:")) {
            ("user", line.split_once(':').map(|(_, r)| r.trim()).unwrap_or(""))
        } else if !char_l.is_empty() && line.to_lowercase().starts_with(&format!("{char_l}:")) {
            ("assistant", line.split_once(':').map(|(_, r)| r.trim()).unwrap_or(""))
        } else if let Some(idx) = line.find(':') {
            // bare Name: content — if looks like assistant continuation, keep as system blob join
            let name = line[..idx].trim();
            let rest = line[idx + 1..].trim();
            if name.eq_ignore_ascii_case("system") {
                ("system", rest)
            } else {
                // append to previous assistant/user if any, else system
                if let Some(last) = messages.last_mut() {
                    if !last.1.is_empty() {
                        last.1.push('\n');
                    }
                    last.1.push_str(line);
                    continue;
                }
                ("system", line)
            }
        } else {
            if let Some(last) = messages.last_mut() {
                last.1.push('\n');
                last.1.push_str(line);
                continue;
            }
            ("system", line)
        };
        if rest.is_empty() && role == "system" {
            continue;
        }
        messages.push((role.to_string(), rest.to_string()));
    }
    messages
}

fn strip_example_prefix<'a>(line: &'a str, prefixes: &[&str]) -> Option<&'a str> {
    for p in prefixes {
        if line.len() >= p.len() && line[..p.len()].eq_ignore_ascii_case(p) {
            return Some(line[p.len()..].trim());
        }
    }
    None
}

pub fn parse_decorators(content: &str) -> (Vec<String>, String) {
    const KNOWN: &[&str] = &["@@activate", "@@dont_activate"];
    let is_known = |data: &str| -> bool {
        let d = if data.starts_with("@@@") {
            &data[1..]
        } else {
            data
        };
        KNOWN.iter().any(|k| d.starts_with(k))
    };
    if !content.starts_with("@@") {
        return (Vec::new(), content.to_string());
    }
    let lines: Vec<&str> = content.split('\n').collect();
    let mut decorators = Vec::new();
    let mut fallbacked = false;
    let mut body_start = lines.len();
    for (i, line) in lines.iter().enumerate() {
        if line.starts_with("@@") {
            if line.starts_with("@@@") && !fallbacked {
                continue;
            }
            if is_known(line) {
                let d = if line.starts_with("@@@") {
                    line[1..].to_string()
                } else {
                    (*line).to_string()
                };
                // normalize to known prefix only (trim trailing junk optional)
                let norm = if d.starts_with("@@dont_activate") {
                    "@@dont_activate".to_string()
                } else if d.starts_with("@@activate") {
                    "@@activate".to_string()
                } else {
                    d
                };
                decorators.push(norm);
                fallbacked = false;
            } else {
                fallbacked = true;
            }
        } else {
            body_start = i;
            break;
        }
    }
    let new_content = if body_start < lines.len() {
        lines[body_start..].join("\n")
    } else {
        String::new()
    };
    (decorators, new_content)
}

pub fn parse_wi_entry(raw: &Value, world: &str, idx: usize) -> Option<WiEntry> {
    if !raw.is_object() {
        return None;
    }
    let ext = raw.get("extensions").cloned().unwrap_or(json!({}));

    let keys = {
        let mut k = as_str_list(raw.get("keys").or_else(|| raw.get("key")));
        if k.is_empty() {
            k = as_str_list(ext.get("keys"));
        }
        k
    };
    let keysecondary = as_str_list(
        raw.get("keysecondary")
            .or_else(|| raw.get("secondary_keys"))
            .or_else(|| raw.get("secondaryKeys")),
    );
    let raw_content = str_field(raw, &["content", "entry", "text"]);
    let (decorators, content) = parse_decorators(&raw_content);
    if content.trim().is_empty() && keys.is_empty() && !bool_field(raw, &["constant"], false)
        && !decorators.iter().any(|d| d == "@@activate")
    {
        return None;
    }

    let disable = {
        if raw.get("disable").and_then(|v| v.as_bool()) == Some(true) {
            true
        } else if raw.get("disabled").and_then(|v| v.as_bool()) == Some(true) {
            true
        } else if raw.get("enabled").and_then(|v| v.as_bool()) == Some(false) {
            true
        } else {
            false
        }
    };

    let position_raw = i32_field(
        raw,
        &["position"],
        i32_field(&ext, &["position"], {
            // character_book uses position string before_char/after_char
            match str_field(raw, &["position"]).as_str() {
                "before_char" | "before" => 0,
                "after_char" | "after" => 1,
                _ => 0,
            }
        }),
    );

    let uid = {
        let u = str_field(raw, &["uid", "id"]);
        if u.is_empty() {
            format!("{world}-{idx}")
        } else {
            u
        }
    };

    let selective_default = !keysecondary.is_empty();
    Some(WiEntry {
        uid,
        world: world.to_string(),
        keys,
        keysecondary,
        content,
        comment: str_field(raw, &["comment", "name"]),
        constant: bool_field(raw, &["constant"], false),
        disable,
        selective: bool_field(raw, &["selective"], selective_default),
        selective_logic: SelectiveLogic::from_i32(i32_field(
            raw,
            &["selectiveLogic", "selective_logic"],
            i32_field(&ext, &["selectiveLogic", "selective_logic"], 0),
        )),
        order: i32_field(
            raw,
            &["order", "insertion_order", "insertionOrder"],
            i32_field(&ext, &["insertion_order"], idx as i32),
        ),
        position: WiPosition::from_i32(position_raw),
        depth: i32_field(raw, &["depth"], i32_field(&ext, &["depth"], 4)),
        probability: f64_field(
            raw,
            &["probability"],
            f64_field(&ext, &["probability"], 100.0),
        )
        .clamp(0.0, 100.0),
        use_probability: bool_field(
            raw,
            &["useProbability", "use_probability"],
            bool_field(&ext, &["useProbability", "use_probability"], true),
        ),
        scan_depth: {
            let d = i32_field(raw, &["scanDepth", "scan_depth"], -1);
            let d2 = i32_field(&ext, &["scan_depth", "scanDepth"], d);
            if d2 < 0 {
                None
            } else {
                Some(d2)
            }
        },
        case_sensitive: {
            if raw.get("caseSensitive").is_some() || raw.get("case_sensitive").is_some() {
                Some(bool_field(raw, &["caseSensitive", "case_sensitive"], false))
            } else if ext.get("case_sensitive").is_some() || ext.get("caseSensitive").is_some() {
                Some(bool_field(
                    &ext,
                    &["case_sensitive", "caseSensitive"],
                    false,
                ))
            } else {
                None
            }
        },
        match_whole_words: {
            if raw.get("matchWholeWords").is_some() || raw.get("match_whole_words").is_some() {
                Some(bool_field(
                    raw,
                    &["matchWholeWords", "match_whole_words"],
                    false,
                ))
            } else if ext.get("match_whole_words").is_some() {
                Some(bool_field(&ext, &["match_whole_words"], false))
            } else {
                None
            }
        },
        group: str_field(raw, &["group"]).or_else(|| str_field(&ext, &["group"])),
        group_override: bool_field(
            raw,
            &["groupOverride", "group_override"],
            bool_field(&ext, &["group_override", "groupOverride"], false),
        ),
        group_weight: f64_field(
            raw,
            &["groupWeight", "group_weight"],
            f64_field(&ext, &["group_weight", "groupWeight"], 100.0),
        ),
        use_group_scoring: {
            if raw.get("useGroupScoring").is_some() {
                Some(bool_field(raw, &["useGroupScoring"], false))
            } else if ext.get("use_group_scoring").is_some() {
                Some(bool_field(&ext, &["use_group_scoring"], false))
            } else {
                None
            }
        },
        exclude_recursion: bool_field(
            raw,
            &["excludeRecursion", "exclude_recursion"],
            bool_field(&ext, &["exclude_recursion"], false),
        ),
        prevent_recursion: bool_field(
            raw,
            &["preventRecursion", "prevent_recursion"],
            bool_field(&ext, &["prevent_recursion"], false),
        ),
        delay_until_recursion: bool_field(
            raw,
            &["delayUntilRecursion", "delay_until_recursion"],
            bool_field(&ext, &["delay_until_recursion"], false),
        ),
        ignore_budget: bool_field(
            raw,
            &["ignoreBudget", "ignore_budget"],
            bool_field(&ext, &["ignore_budget"], false),
        ),
        sticky: {
            let v = i32_field(raw, &["sticky"], i32_field(&ext, &["sticky"], -1));
            if v > 0 { Some(v) } else { None }
        },
        cooldown: {
            let v = i32_field(raw, &["cooldown"], i32_field(&ext, &["cooldown"], -1));
            if v > 0 { Some(v) } else { None }
        },
        delay: {
            let v = i32_field(raw, &["delay"], i32_field(&ext, &["delay"], -1));
            if v > 0 { Some(v) } else { None }
        },
        decorators,
        outlet_name: {
            let n = str_field(raw, &["outletName", "outlet_name"]);
            if n.is_empty() {
                str_field(&ext, &["outlet_name", "outletName"])
            } else {
                n
            }
        },
        character_filter: parse_character_filter(raw, &ext),
        triggers: {
            let mut tr = as_str_list(raw.get("triggers"));
            if tr.is_empty() {
                tr = as_str_list(ext.get("triggers"));
            }
            tr
        },
        vectorized: bool_field(
            raw,
            &["vectorized"],
            bool_field(&ext, &["vectorized"], false),
        ),
        automation_id: {
            let a = str_field(raw, &["automationId", "automation_id"]);
            if a.is_empty() {
                str_field(&ext, &["automation_id", "automationId"])
            } else {
                a
            }
        },
        role: i32_field(raw, &["role"], i32_field(&ext, &["role"], 0)),
    })
}

/// Pipeline: card `world_book` raw entries → structured `WiEntry` list (X6d).
///
/// 吞噬自 tavern-card-distiller extract_card.py `normalize_card` world_book 分支 (MIT)。
/// Filters out `enabled=false` / `disable=true` entries; each surviving entry is
/// parsed by the shared `parse_wi_entry` so keys/content/constant/order all land
/// in the same ST-compatible shape as imported lorebooks.
pub fn import_card_world_book(card: &crate::StCardData, world_name: &str) -> Vec<WiEntry> {
    card.world_book
        .iter()
        .enumerate()
        .filter(|(_, e)| {
            let disabled = e
                .get("disable")
                .or_else(|| e.get("disabled"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let enabled = e.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
            enabled && !disabled
        })
        .filter_map(|(i, e)| parse_wi_entry(e, world_name, i))
        .collect()
}

fn parse_character_filter(raw: &Value, ext: &Value) -> Option<CharacterFilter> {
    let src = raw
        .get("characterFilter")
        .or_else(|| raw.get("character_filter"))
        .or_else(|| ext.get("character_filter"))
        .or_else(|| ext.get("characterFilter"));
    let Some(obj) = src else {
        return None;
    };
    if !obj.is_object() {
        return None;
    }
    let names = as_str_list(obj.get("names"));
    let tags = as_str_list(obj.get("tags"));
    let is_exclude = bool_field(obj, &["isExclude", "is_exclude", "exclude"], false);
    if names.is_empty() && tags.is_empty() {
        return None;
    }
    Some(CharacterFilter {
        names,
        tags,
        is_exclude,
    })
}

/// ST-lite macro substitute for WI keys/content.
pub fn substitute_params(content: &str, ctx: &WiScanContext) -> String {
    if content.is_empty() {
        return String::new();
    }
    let user = if ctx.user_name.is_empty() {
        "User"
    } else {
        ctx.user_name.as_str()
    };
    let char = if ctx.char_name.is_empty() {
        "Char"
    } else {
        ctx.char_name.as_str()
    };
    let mut out = content.to_string();
    for (pat, rep) in [
        ("{{user}}", user),
        ("{{User}}", user),
        ("{{USER}}", user),
        ("<USER>", user),
        ("{{char}}", char),
        ("{{Char}}", char),
        ("{{CHAR}}", char),
        ("<BOT>", char),
        ("<CHAR>", char),
        ("{{char}}", char),
    ] {
        // case-insensitive replace for {{user}}/{{char}}
        if pat.starts_with("{{") {
            // manual ci
            let lower_pat = pat.to_lowercase();
            let mut tmp = String::new();
            let mut rest = out.as_str();
            loop {
                let low = rest.to_lowercase();
                if let Some(i) = low.find(&lower_pat) {
                    tmp.push_str(&rest[..i]);
                    tmp.push_str(rep);
                    rest = &rest[i + pat.len()..];
                } else {
                    tmp.push_str(rest);
                    break;
                }
            }
            out = tmp;
        } else {
            out = out.replace(pat, rep);
        }
    }
    out
}


trait StrOrElse {
    fn or_else(self, f: impl FnOnce() -> String) -> String;
}
impl StrOrElse for String {
    fn or_else(self, f: impl FnOnce() -> String) -> String {
        if self.is_empty() {
            f()
        } else {
            self
        }
    }
}


/// Serialize a WiEntry back to ST-ish JSON (array element under stBookRaw.entries).
pub fn wi_entry_to_st_json(e: &WiEntry) -> Value {
    let mut ext = json!({});
    if e.position as i32 >= 2 {
        ext["position"] = json!(e.position as i32);
    }
    if e.depth != 4 {
        ext["depth"] = json!(e.depth);
    }
    if !e.outlet_name.is_empty() {
        ext["outlet_name"] = json!(e.outlet_name);
    }
    if let Some(s) = e.sticky {
        ext["sticky"] = json!(s);
    }
    if let Some(c) = e.cooldown {
        ext["cooldown"] = json!(c);
    }
    if let Some(d) = e.delay {
        ext["delay"] = json!(d);
    }
    if e.vectorized {
        ext["vectorized"] = json!(true);
    }
    if !e.automation_id.is_empty() {
        ext["automation_id"] = json!(e.automation_id);
    }
    if e.role != 0 {
        ext["role"] = json!(e.role);
    }
    if !e.triggers.is_empty() {
        ext["triggers"] = json!(e.triggers);
    }
    if let Some(ref cf) = e.character_filter {
        ext["character_filter"] = json!({
            "names": cf.names,
            "tags": cf.tags,
            "isExclude": cf.is_exclude,
        });
    }

    let mut content = e.content.clone();
    if !e.decorators.is_empty() {
        let head = e
            .decorators
            .iter()
            .map(|d| {
                if d.starts_with("@@") {
                    d.clone()
                } else {
                    format!("@@{d}")
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        content = format!("{head}\n{content}");
    }

    let mut obj = json!({
        "uid": e.uid,
        "id": e.uid,
        "keys": e.keys,
        "key": e.keys,
        "keysecondary": e.keysecondary,
        "content": content,
        "comment": e.comment,
        "constant": e.constant,
        "disable": e.disable,
        "enabled": !e.disable,
        "selective": e.selective,
        "selectiveLogic": e.selective_logic as i32,
        "order": e.order,
        "position": e.position as i32,
        "depth": e.depth,
        "probability": e.probability,
        "useProbability": e.use_probability,
        "group": e.group,
        "groupOverride": e.group_override,
        "groupWeight": e.group_weight,
        "excludeRecursion": e.exclude_recursion,
        "preventRecursion": e.prevent_recursion,
        "delayUntilRecursion": e.delay_until_recursion,
        "ignoreBudget": e.ignore_budget,
    });
    if let Some(sd) = e.scan_depth {
        obj["scanDepth"] = json!(sd);
    }
    if let Some(cs) = e.case_sensitive {
        obj["caseSensitive"] = json!(cs);
    }
    if let Some(mw) = e.match_whole_words {
        obj["matchWholeWords"] = json!(mw);
    }
    if let Some(ugs) = e.use_group_scoring {
        obj["useGroupScoring"] = json!(ugs);
    }
    if ext.as_object().map(|o| !o.is_empty()).unwrap_or(false) {
        obj["extensions"] = ext;
    }
    obj
}

/// Build stBookRaw object from entry list (array form).
pub fn st_book_raw_from_entries(name: &str, entries: &[WiEntry]) -> Value {
    let arr: Vec<Value> = entries.iter().map(wi_entry_to_st_json).collect();
    json!({
        "name": name,
        "entries": arr,
    })
}

/// API-facing entry list (ST JSON objects) from a world book item fields/content.
pub fn entry_values_from_world_book(world_name: &str, fields: Option<&Value>, content: &str) -> Vec<Value> {
    entries_from_world_book(world_name, fields, content)
        .iter()
        .map(wi_entry_to_st_json)
        .collect()
}

/// Merge patch JSON onto an existing ST entry value (shallow + keys arrays).
pub fn merge_wi_entry_value(base: &Value, patch: &Value) -> Value {
    let mut out = base.clone();
    let Some(obj) = out.as_object_mut() else {
        return patch.clone();
    };
    let Some(pobj) = patch.as_object() else {
        return out;
    };
    for (k, v) in pobj {
        if k == "extensions" {
            let mut ext = obj
                .get("extensions")
                .cloned()
                .unwrap_or(json!({}));
            if let Some(eo) = ext.as_object_mut() {
                if let Some(po) = v.as_object() {
                    for (ek, ev) in po {
                        eo.insert(ek.clone(), ev.clone());
                    }
                }
            }
            obj.insert("extensions".into(), ext);
        } else if k == "uid" || k == "id" {
            // allow rename only if explicitly set non-empty
            if v.as_str().map(|s| !s.is_empty()).unwrap_or(false) {
                obj.insert("uid".into(), v.clone());
                obj.insert("id".into(), v.clone());
            }
        } else {
            obj.insert(k.clone(), v.clone());
        }
    }
    // keep key/keys in sync
    if let Some(keys) = obj.get("keys").cloned() {
        obj.insert("key".into(), keys);
    } else if let Some(key) = obj.get("key").cloned() {
        obj.insert("keys".into(), key);
    }
    if let Some(dis) = obj.get("disable").and_then(|v| v.as_bool()) {
        obj.insert("enabled".into(), json!(!dis));
    } else if let Some(en) = obj.get("enabled").and_then(|v| v.as_bool()) {
        obj.insert("disable".into(), json!(!en));
    }
    out
}

/// Rebuild freeform markdown content from entries (for legacy content field).
pub fn content_from_wi_entries(name: &str, entries: &[WiEntry]) -> String {
    let mut out = format!("# {name}\n\n");
    if entries.is_empty() {
        out.push_str("_（无世界书条目）_\n");
        return out;
    }
    out.push_str("## 世界书条目\n\n");
    let mut sorted: Vec<&WiEntry> = entries.iter().collect();
    sorted.sort_by_key(|e| e.order);
    for e in sorted {
        let title = if e.comment.is_empty() {
            e.uid.clone()
        } else {
            e.comment.clone()
        };
        let keys = if e.keys.is_empty() {
            if e.constant {
                "constant".into()
            } else {
                "—".into()
            }
        } else {
            e.keys.join(", ")
        };
        out.push_str(&format!("### {title}\n"));
        out.push_str(&format!("- keys: {keys}\n"));
        if e.constant {
            out.push_str("- constant: true\n");
        }
        if e.disable {
            out.push_str("- disabled: true\n");
        }
        out.push('\n');
        out.push_str(e.content.trim());
        out.push_str("\n\n");
    }
    out
}

/// Load entries from a partner world_book item's fields / content.
pub fn entries_from_world_book(world_name: &str, fields: Option<&Value>, content: &str) -> Vec<WiEntry> {
    let mut out = Vec::new();
    if let Some(fields) = fields {
        // Prefer full ST book raw
        if let Some(book) = fields
            .get("stBookRaw")
            .or_else(|| fields.get("character_book"))
        {
            if let Some(arr) = book.get("entries").and_then(|e| e.as_array()) {
                for (i, e) in arr.iter().enumerate() {
                    if let Some(ent) = parse_wi_entry(e, world_name, i) {
                        out.push(ent);
                    }
                }
            } else if let Some(map) = book.get("entries").and_then(|e| e.as_object()) {
                // ST world_info uses object map uid -> entry
                for (i, (_k, e)) in map.iter().enumerate() {
                    if let Some(ent) = parse_wi_entry(e, world_name, i) {
                        out.push(ent);
                    }
                }
            }
        }
        if out.is_empty() {
            if let Some(arr) = fields.get("stEntries").and_then(|e| e.as_array()) {
                for (i, e) in arr.iter().enumerate() {
                    if let Some(ent) = parse_wi_entry(e, world_name, i) {
                        out.push(ent);
                    }
                }
            }
        }
        // plain ST world_info export shape on fields.entries
        if out.is_empty() {
            if let Some(arr) = fields.get("entries").and_then(|e| e.as_array()) {
                for (i, e) in arr.iter().enumerate() {
                    if let Some(ent) = parse_wi_entry(e, world_name, i) {
                        out.push(ent);
                    }
                }
            } else if let Some(map) = fields.get("entries").and_then(|e| e.as_object()) {
                for (i, (_k, e)) in map.iter().enumerate() {
                    if let Some(ent) = parse_wi_entry(e, world_name, i) {
                        out.push(ent);
                    }
                }
            }
        }
    }
    if out.is_empty() && !content.trim().is_empty() {
        // Legacy freeform world book: treat whole content as one constant entry
        out.push(WiEntry {
            uid: format!("{world_name}-legacy"),
            world: world_name.to_string(),
            keys: vec![],
            keysecondary: vec![],
            content: content.to_string(),
            comment: world_name.to_string(),
            constant: true,
            disable: false,
            selective: false,
            selective_logic: SelectiveLogic::AndAny,
            order: 100,
            position: WiPosition::Before,
            depth: 4,
            probability: 100.0,
            use_probability: false,
            scan_depth: None,
            case_sensitive: None,
            match_whole_words: None,
            group: String::new(),
            group_override: false,
            group_weight: 100.0,
            use_group_scoring: None,
            exclude_recursion: false,
            prevent_recursion: false,
            delay_until_recursion: false,
            ignore_budget: false,
            sticky: None,
            cooldown: None,
            delay: None,
            decorators: Vec::new(),
            outlet_name: String::new(),
            character_filter: None,
            triggers: Vec::new(),
            vectorized: false,
            automation_id: String::new(),
            role: 0,
        });
    }
    out
}

// --- matching (ST WorldInfoBuffer.matchKeys) ---

/// Parse `/pattern/flags` like ST `parseRegexFromString`. Returns None if not regex form.
pub fn parse_regex_from_string(input: &str) -> Option<regex::Regex> {
    let input = input.trim();
    if !input.starts_with('/') {
        return None;
    }
    let last = input.rfind('/')?;
    if last == 0 {
        return None;
    }
    let pattern = &input[1..last];
    let flags = &input[last + 1..];
    // unescaped slash inside pattern → invalid for ST
    let mut escaped = false;
    for ch in pattern.chars() {
        if ch == '\\' {
            escaped = !escaped;
            continue;
        }
        if ch == '/' && !escaped {
            return None;
        }
        escaped = false;
    }
    let pattern = pattern.replace("\\/", "/");
    let mut builder = regex::RegexBuilder::new(&pattern);
    for f in flags.chars() {
        match f {
            'i' => {
                builder.case_insensitive(true);
            }
            'm' => {
                builder.multi_line(true);
            }
            's' => {
                builder.dot_matches_new_line(true);
            }
            'u' => {}
            'g' | 'y' => {} // JS-only; rust always can re-search
            _ => {}
        }
    }
    builder.build().ok()
}

fn transform_case(s: &str, case_sensitive: bool) -> String {
    if case_sensitive {
        s.to_string()
    } else {
        s.to_lowercase()
    }
}

fn match_keys(haystack: &str, needle: &str, entry: &WiEntry, settings: &WiSettings) -> bool {
    let needle = needle.trim();
    if needle.is_empty() {
        return false;
    }
    if let Some(re) = parse_regex_from_string(needle) {
        return re.is_match(haystack);
    }
    let case_sensitive = entry.case_sensitive.unwrap_or(settings.case_sensitive);
    let match_whole = entry
        .match_whole_words
        .unwrap_or(settings.match_whole_words);
    let hay = transform_case(haystack, case_sensitive);
    let neo = transform_case(needle, case_sensitive);
    if match_whole {
        let words: Vec<&str> = neo.split_whitespace().collect();
        if words.len() > 1 {
            hay.contains(&neo)
        } else {
            // (?:^|\W)(needle)(?:$|\W)
            let re = regex::RegexBuilder::new(&format!(
                r"(?:^|\W)({})(?:$|\W)",
                regex::escape(&neo)
            ))
            .case_insensitive(false)
            .build();
            match re {
                Ok(r) => r.is_match(&hay),
                Err(_) => hay.contains(&neo),
            }
        }
    } else {
        hay.contains(&neo)
    }
}

fn estimate_tokens_mode(s: &str, mode: &str) -> i32 {
    let m = crate::TokenEstimateMode::parse(mode);
    crate::estimate_tokens(s, m)
}

/// Chat messages newest-first (ST convention for WI scan buffer).
pub fn chat_to_scan_buffer(messages: &[(String, String)]) -> Vec<String> {
    // messages as (role, content) oldest-first from API → reverse for depth buffer
    messages
        .iter()
        .rev()
        .map(|(role, content)| {
            if role == "user" || role == "assistant" || role == "system" {
                content.trim().to_string()
            } else {
                content.trim().to_string()
            }
        })
        .filter(|s| !s.is_empty())
        .collect()
}

fn buffer_text(depth_buf: &[String], depth: i32, recurse: &[String]) -> String {
    let d = depth.max(0) as usize;
    let slice = if d == 0 {
        &depth_buf[..0]
    } else if d >= depth_buf.len() {
        &depth_buf[..]
    } else {
        &depth_buf[..d]
    };
    let mut parts: Vec<&str> = slice.iter().map(|s| s.as_str()).collect();
    for r in recurse {
        parts.push(r.as_str());
    }
    parts.join("\n")
}

fn primary_match(
    entry: &WiEntry,
    text: &str,
    settings: &WiSettings,
) -> Option<String> {
    for key in &entry.keys {
        if match_keys(text, key, entry, settings) {
            return Some(key.clone());
        }
    }
    None
}

fn secondary_match(entry: &WiEntry, text: &str, settings: &WiSettings) -> bool {
    if !entry.selective || entry.keysecondary.is_empty() {
        return true;
    }
    let mut any = false;
    let mut all = true;
    for key in &entry.keysecondary {
        let m = match_keys(text, key, entry, settings);
        if m {
            any = true;
        } else {
            all = false;
        }
        match entry.selective_logic {
            SelectiveLogic::AndAny if m => return true,
            SelectiveLogic::NotAll if !m => return true,
            _ => {}
        }
    }
    match entry.selective_logic {
        SelectiveLogic::NotAny => !any,
        SelectiveLogic::AndAll => all,
        SelectiveLogic::AndAny => any,
        SelectiveLogic::NotAll => !all,
    }
}

fn score_entry(entry: &WiEntry, text: &str, settings: &WiSettings) -> i32 {
    let mut score = 0;
    for k in &entry.keys {
        if match_keys(text, k, entry, settings) {
            score += 1;
        }
    }
    for k in &entry.keysecondary {
        if match_keys(text, k, entry, settings) {
            score += 1;
        }
    }
    score
}

/// Core ST-compatible scan.
pub fn check_world_info(
    entries: &[WiEntry],
    chat_newest_first: &[String],
    max_context_tokens: i32,
    settings: &WiSettings,
) -> WiScanResult {
    check_world_info_timed(
        entries,
        chat_newest_first,
        max_context_tokens,
        settings,
        None,
        false,
        None,
    )
}

/// Full scan with timed effects + optional ST global scan context.
pub fn check_world_info_timed(
    entries: &[WiEntry],
    chat_newest_first: &[String],
    max_context_tokens: i32,
    settings: &WiSettings,
    timed_in: Option<TimedWorldInfo>,
    dry_run: bool,
    scan_ctx: Option<WiScanContext>,
) -> WiScanResult {
    let mut rng = rand::thread_rng();
    let ctx = scan_ctx.unwrap_or_default();
    let max_context_tokens = if ctx.max_context_tokens > 0 {
        ctx.max_context_tokens
    } else {
        max_context_tokens
    };
    let mut budget = ((settings.budget_pct / 100.0) * max_context_tokens as f64).round() as i32;
    if budget < 1 {
        budget = 1;
    }
    if settings.budget_cap > 0 && budget > settings.budget_cap {
        budget = settings.budget_cap;
    }

    // Sort like ST: by order desc (higher insertion_order first)
    let mut sorted: Vec<&WiEntry> = entries.iter().filter(|e| !e.disable).collect();
    sorted.sort_by(|a, b| b.order.cmp(&a.order).then_with(|| a.uid.cmp(&b.uid)));

    #[derive(Clone, Copy, PartialEq)]
    enum ScanState {
        None,
        Initial,
        Recursion,
        MinActivations,
    }

    let mut scan_state = ScanState::Initial;
    let mut all_activated: HashMap<String, (WiEntry, String)> = HashMap::new(); // key world.uid
    let mut failed_prob: HashSet<String> = HashSet::new();
    let mut recurse_buf: Vec<String> = Vec::new();
    let mut all_activated_text = String::new();
    let mut overflowed = false;
    let mut loop_count = 0;
    let mut depth_skew = 0i32;
    let mut skipped_vectorized = 0i32;
    let mut vector_activated = 0i32;
    let mut skipped_filter = 0i32;
    let mut skipped_trigger = 0i32;
    let vector_hit_map: std::collections::HashMap<String, f64> = {
        let settings = ctx
            .vector_settings
            .clone()
            .unwrap_or_default();
        if settings.enabled {
            crate::hits_to_map(&ctx.vector_hits)
        } else {
            std::collections::HashMap::new()
        }
    };

    // --- Timed effects (ST WorldInfoTimedEffects) ---
    // chat_len uses full message count; scan buffer is newest-first subset.
    let chat_len = chat_newest_first.len() as i32;
    let mut timed = timed_in.unwrap_or_default();
    let mut sticky_active: HashSet<String> = HashSet::new();
    let mut cooldown_active: HashSet<String> = HashSet::new();
    let mut delay_active: HashSet<String> = HashSet::new();

    if !dry_run {
        // Process sticky map
        let keys: Vec<String> = timed.sticky.keys().cloned().collect();
        for key in keys {
            let Some(eff) = timed.sticky.get(&key).cloned() else { continue };
            if chat_len <= eff.start && !eff.protected {
                timed.sticky.remove(&key);
                continue;
            }
            let entry_exists = entries.iter().any(|e| TimedWorldInfo::entry_key(&e.world, &e.uid) == key);
            if !entry_exists {
                if chat_len >= eff.end {
                    timed.sticky.remove(&key);
                }
                continue;
            }
            if chat_len >= eff.end {
                // sticky ended → optional cooldown
                timed.sticky.remove(&key);
                if let Some(entry) = entries.iter().find(|e| TimedWorldInfo::entry_key(&e.world, &e.uid) == key) {
                    if let Some(cd) = entry.cooldown {
                        if cd > 0 {
                            let ce = WiTimedEffect {
                                key: key.clone(),
                                start: chat_len,
                                end: chat_len + cd,
                                protected: true,
                            };
                            timed.cooldown.insert(key.clone(), ce);
                            cooldown_active.insert(key.clone());
                        }
                    }
                }
                continue;
            }
            sticky_active.insert(key);
        }
        // Process cooldown map
        let keys: Vec<String> = timed.cooldown.keys().cloned().collect();
        for key in keys {
            let Some(eff) = timed.cooldown.get(&key).cloned() else { continue };
            if chat_len <= eff.start && !eff.protected {
                timed.cooldown.remove(&key);
                continue;
            }
            if chat_len >= eff.end {
                timed.cooldown.remove(&key);
                continue;
            }
            cooldown_active.insert(key);
        }
    }
    // Delay: suppress until chat has enough messages
    for e in entries {
        if let Some(d) = e.delay {
            if d > 0 && chat_len < d {
                delay_active.insert(TimedWorldInfo::entry_key(&e.world, &e.uid));
            }
        }
    }


    while scan_state != ScanState::None {
        if settings.max_recursion_steps > 0 && loop_count >= settings.max_recursion_steps {
            break;
        }
        loop_count += 1;
        let mut activated_now: Vec<(WiEntry, String)> = Vec::new();

        for entry in &sorted {
            let key = format!("{}.{}", entry.world, entry.uid);
            if failed_prob.contains(&key) || all_activated.contains_key(&key) {
                continue;
            }
            if scan_state != ScanState::Recursion && entry.delay_until_recursion {
                continue;
            }
            if scan_state == ScanState::Recursion && settings.recursive && entry.exclude_recursion
            {
                continue;
            }

            let depth = entry
                .scan_depth
                .unwrap_or(settings.depth + depth_skew)
                .max(0);
            let text = buffer_text(chat_newest_first, depth, &recurse_buf);

            let ekey = TimedWorldInfo::entry_key(&entry.world, &entry.uid);
            if delay_active.contains(&ekey) {
                continue;
            }
            if cooldown_active.contains(&ekey) && !sticky_active.contains(&ekey) {
                continue;
            }
            if sticky_active.contains(&ekey) {
                {
                let mut e = (*entry).clone();
                e.content = substitute_params(&entry.content, &ctx);
                activated_now.push((e, "sticky".into()));
            }
                continue;
            }
            // vectorized entries: activate via precomputed embed hits (W5); else skip keyword path (ST)
            if entry.vectorized {
                let vkey = format!("{}.{}", entry.world, entry.uid);
                if let Some(score) = vector_hit_map.get(&vkey) {
                    let mut e = (*entry).clone();
                    e.content = substitute_params(&entry.content, &ctx);
                    activated_now.push((e, format!("vector:{score:.4}")));
                    vector_activated += 1;
                } else {
                    skipped_vectorized += 1;
                }
                continue;
            }
            // generation type triggers
            if !entry.triggers.is_empty() {
                let trig = ctx.trigger.trim();
                if trig.is_empty() || !entry.triggers.iter().any(|t| t == trig) {
                    skipped_trigger += 1;
                    continue;
                }
            }
            // characterFilter (names + tags)
            if let Some(ref cf) = entry.character_filter {
                let mut filtered = false;
                if !cf.names.is_empty() {
                    let name = ctx.character_name.trim();
                    let included = !name.is_empty()
                        && cf.names.iter().any(|n| n.eq_ignore_ascii_case(name) || name.contains(n.as_str()));
                    filtered = if cf.is_exclude { included } else { !included };
                }
                if !filtered && !cf.tags.is_empty() {
                    let included = cf.tags.iter().any(|t| {
                        ctx.character_tags.iter().any(|ct| ct.eq_ignore_ascii_case(t))
                    });
                    filtered = if cf.is_exclude { included } else { !included };
                }
                if filtered {
                    skipped_filter += 1;
                    continue;
                }
            }
            // ST decorators (after timed gates, before constant/keys)
            if entry.decorators.iter().any(|d| d == "@@dont_activate") {
                continue;
            }
            if entry.decorators.iter().any(|d| d == "@@activate") {
                {
                let mut e = (*entry).clone();
                e.content = substitute_params(&entry.content, &ctx);
                activated_now.push((e, "decorator:@@activate".into()));
            }
                continue;
            }
            if entry.constant {
                {
                let mut e = (*entry).clone();
                e.content = substitute_params(&entry.content, &ctx);
                activated_now.push((e, "constant".into()));
            }
                continue;
            }
            if entry.keys.is_empty() {
                continue;
            }
            // macro-expand keys against scan context
            let mut entry_sub = (*entry).clone();
            entry_sub.keys = entry
                .keys
                .iter()
                .map(|k| substitute_params(k, &ctx))
                .collect();
            entry_sub.keysecondary = entry
                .keysecondary
                .iter()
                .map(|k| substitute_params(k, &ctx))
                .collect();
            let Some(pk) = primary_match(&entry_sub, &text, settings) else {
                continue;
            };
            if !secondary_match(&entry_sub, &text, settings) {
                continue;
            }
            entry_sub.content = substitute_params(&entry.content, &ctx);
            activated_now.push((entry_sub, format!("key:{pk}")));
        }

        // Inclusion groups
        filter_inclusion_groups(&mut activated_now, &all_activated, chat_newest_first, settings, depth_skew, &recurse_buf, &mut rng);

        // Probability + budget
        let mut new_content = String::new();
        let tok_mode = settings.token_estimate_mode.as_str();
        let base_tokens = estimate_tokens_mode(&all_activated_text, tok_mode);
        let mut ignore_left = activated_now.iter().filter(|(e, _)| e.ignore_budget).count();

        for (entry, reason) in activated_now {
            let key = format!("{}.{}", entry.world, entry.uid);
            if overflowed && !entry.ignore_budget {
                if ignore_left > 0 {
                    continue;
                }
                break;
            }
            if entry.ignore_budget {
                ignore_left = ignore_left.saturating_sub(1);
            }

            // probability
            if entry.use_probability && entry.probability < 100.0 {
                let roll: f64 = rng.gen::<f64>() * 100.0;
                if roll > entry.probability {
                    failed_prob.insert(key);
                    continue;
                }
            }

            new_content.push_str(&entry.content);
            new_content.push('\n');
            if !entry.ignore_budget
                && (base_tokens + estimate_tokens_mode(&new_content, tok_mode)) >= budget
            {
                overflowed = true;
                // still skip adding this one (ST continues ignoreBudget only)
                continue;
            }
            all_activated.insert(key, (entry, reason));
        }

        // recursion / min activations
        let successful: Vec<&WiEntry> = all_activated
            .values()
            .map(|(e, _)| e)
            .collect();
        // Only newly added this loop for recursion — approximate: use recurse from last new_content
        let mut next = ScanState::None;
        if settings.recursive && !overflowed && !new_content.trim().is_empty() {
            // re-scan with recurse buffer of prevent_recursion=false contents
            let rec_text: String = all_activated
                .values()
                .filter(|(e, _)| !e.prevent_recursion)
                .filter(|(e, _)| new_content.contains(&e.content)) // rough: newly relevant
                .map(|(e, _)| e.content.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            if !rec_text.is_empty() {
                recurse_buf.push(rec_text.clone());
                all_activated_text = format!("{rec_text}\n{all_activated_text}");
                next = ScanState::Recursion;
            }
        }

        if next == ScanState::None
            && !overflowed
            && settings.min_activations > 0
            && (successful.len() as i32) < settings.min_activations
        {
            let over = (settings.min_activations_depth_max > 0
                && (settings.depth + depth_skew) > settings.min_activations_depth_max)
                || ((settings.depth + depth_skew) as usize) > chat_newest_first.len();
            if !over {
                depth_skew += 1;
                next = ScanState::MinActivations;
            }
        }

        // Avoid infinite recursion: if recursion produced nothing new, stop
        if next == ScanState::Recursion && loop_count > 1 {
            // if no new keys this round beyond previous, ST still may loop — cap soft
            if new_content.trim().is_empty() {
                next = ScanState::None;
            }
        }
        if scan_state == ScanState::Initial && next == ScanState::None {
            // done
        }
        scan_state = next;
        if loop_count > 32 {
            break;
        }
    }

    // Build prompt sections — sort by order desc
    let mut before = Vec::new();
    let mut after = Vec::new();
    let mut an_top = Vec::new();
    let mut an_bot = Vec::new();
    let mut em_top = Vec::new();
    let mut em_bot = Vec::new();
    let mut outlet_map: HashMap<String, Vec<String>> = HashMap::new();
    let mut depth_map: HashMap<i32, Vec<(i32, String)>> = HashMap::new();
    let mut activated_meta = Vec::new();
    let mut automation_ids_collect: Vec<String> = Vec::new();
    let mut ordered: Vec<_> = all_activated.into_values().collect();
    ordered.sort_by(|a, b| b.0.order.cmp(&a.0.order));

    // Set timed effects for newly activated (sticky/cooldown start)
    if !dry_run {
        for (entry, _reason) in &ordered {
            let key = TimedWorldInfo::entry_key(&entry.world, &entry.uid);
            if let Some(st) = entry.sticky {
                if st > 0 && !timed.sticky.contains_key(&key) {
                    timed.sticky.insert(
                        key.clone(),
                        WiTimedEffect {
                            key: key.clone(),
                            start: chat_len,
                            end: chat_len + st,
                            protected: false,
                        },
                    );
                }
            }
            if let Some(cd) = entry.cooldown {
                if cd > 0 && !timed.cooldown.contains_key(&key) {
                    // ST sets cooldown on activation too (parallel sticky)
                    // Only if not already from sticky-end
                    if !cooldown_active.contains(&key) {
                        timed.cooldown.insert(
                            key.clone(),
                            WiTimedEffect {
                                key: key.clone(),
                                start: chat_len,
                                end: chat_len + cd,
                                protected: false,
                            },
                        );
                    }
                }
            }
        }
    }

    for (entry, reason) in ordered {
        let content = entry.content.trim();
        if content.is_empty() {
            continue;
        }
        let auto_id = entry.automation_id.trim().to_string();
        activated_meta.push(ActivatedEntry {
            uid: entry.uid.clone(),
            world: entry.world.clone(),
            comment: entry.comment.clone(),
            order: entry.order,
            position: entry.position as i32,
            content: content.to_string(),
            reason,
            automation_id: auto_id.clone(),
        });
        if !auto_id.is_empty() {
            if !automation_ids_collect.iter().any(|x| x == &auto_id) {
                automation_ids_collect.push(auto_id);
            }
        }
        match entry.position {
            WiPosition::Before => {
                before.push(content.to_string());
            }
            WiPosition::EmTop => {
                em_top.push(content.to_string());
            }
            WiPosition::AnTop => {
                an_top.push(content.to_string());
            }
            WiPosition::AnBottom => {
                an_bot.push(content.to_string());
            }
            WiPosition::AtDepth => {
                depth_map
                    .entry(entry.depth.max(0))
                    .or_default()
                    .push((entry.role, content.to_string()));
            }
            WiPosition::EmBottom => {
                em_bot.push(content.to_string());
            }
            WiPosition::Outlet => {
                let name = if entry.outlet_name.trim().is_empty() {
                    "default".to_string()
                } else {
                    entry.outlet_name.trim().to_string()
                };
                outlet_map.entry(name).or_default().push(content.to_string());
            }
            WiPosition::After => {
                after.push(content.to_string());
            }
        }
    }
    let mut depth_entries: Vec<WiDepthEntry> = depth_map
        .into_iter()
        .map(|(depth, parts)| {
            let role = match parts.first().map(|p| p.0).unwrap_or(0) {
                1 => "user",
                2 => "assistant",
                _ => "system",
            };
            WiDepthEntry {
                depth,
                role: role.into(),
                content: parts.into_iter().map(|(_, c)| c).collect::<Vec<_>>().join("\n"),
            }
        })
        .collect();
    depth_entries.sort_by_key(|d| d.depth);
    let mut outlet_entries: Vec<WiOutletEntry> = outlet_map
        .into_iter()
        .map(|(name, parts)| WiOutletEntry {
            name,
            content: parts.join("\n"),
        })
        .collect();
    outlet_entries.sort_by(|a, b| a.name.cmp(&b.name));

    let world_info_before = before.join("\n");
    let world_info_after = after.join("\n");
    let an_before = an_top.join("\n");
    let an_after = an_bot.join("\n");
    let em_before = em_top.join("\n");
    let em_after = em_bot.join("\n");

    // Multi-slot chat injections (ST setExtensionPrompt IN_CHAT / examples)
    let mut chat_injections = Vec::new();
    let mut example_messages: Vec<WiExampleMessage> = Vec::new();
    let user_n = ctx.user_name.clone();
    let char_n = if ctx.char_name.is_empty() { ctx.character_name.clone() } else { ctx.char_name.clone() };
    if !em_before.trim().is_empty() {
        let pairs = parse_mes_examples(em_before.trim(), &user_n, &char_n);
        if pairs.len() >= 1 && pairs.iter().any(|(r, _)| r == "user" || r == "assistant") {
            for (role, content) in pairs {
                example_messages.push(WiExampleMessage {
                    role: role.clone(),
                    content: content.clone(),
                    anchor: "before".into(),
                });
                chat_injections.push(WiChatInjection {
                    role,
                    content,
                    depth: 0,
                    kind: "em_example_before".into(),
                });
            }
        } else {
            chat_injections.push(WiChatInjection {
                role: "system".into(),
                content: em_before.clone(),
                depth: 0,
                kind: "em_before".into(),
            });
        }
    }
    if !em_after.trim().is_empty() {
        let pairs = parse_mes_examples(em_after.trim(), &user_n, &char_n);
        if pairs.len() >= 1 && pairs.iter().any(|(r, _)| r == "user" || r == "assistant") {
            for (role, content) in pairs {
                example_messages.push(WiExampleMessage {
                    role: role.clone(),
                    content: content.clone(),
                    anchor: "after".into(),
                });
                chat_injections.push(WiChatInjection {
                    role,
                    content,
                    depth: 0,
                    kind: "em_example_after".into(),
                });
            }
        } else {
            chat_injections.push(WiChatInjection {
                role: "system".into(),
                content: em_after.clone(),
                depth: 0,
                kind: "em_after".into(),
            });
        }
    }
    for d in &depth_entries {
        if d.content.trim().is_empty() {
            continue;
        }
        let role = match d.role.as_str() {
            "user" => "user",
            "assistant" => "assistant",
            _ => "system",
        };
        chat_injections.push(WiChatInjection {
            role: role.into(),
            content: d.content.clone(),
            depth: d.depth.max(0),
            kind: "depth".into(),
        });
    }
    for o in &outlet_entries {
        if o.content.trim().is_empty() {
            continue;
        }
        chat_injections.push(WiChatInjection {
            role: "system".into(),
            content: format!("[outlet:{}]\n{}", o.name, o.content),
            depth: 0,
            kind: format!("outlet:{}", o.name),
        });
    }

    // Fix depth_entries role from entry.role if we stored system only — enhance depth_map to keep role
    let prompt_slots = WiPromptSlots {
        world_info_before: world_info_before.clone(),
        world_info_after: world_info_after.clone(),
        an_before: an_before.clone(),
        an_after: an_after.clone(),
        em_before: em_before.clone(),
        em_after: em_after.clone(),
        depth_entries: depth_entries.clone(),
        outlet_entries: outlet_entries.clone(),
        chat_injections: chat_injections.clone(),
    };

    WiScanResult {
        world_info_before,
        world_info_after,
        an_before,
        an_after,
        depth_entries,
        em_before,
        em_after,
        outlet_entries,
        activated: activated_meta,
        budget_tokens: budget,
        token_estimate_mode: {
            let m = settings.token_estimate_mode.trim();
            if m.is_empty() {
                "heuristic".into()
            } else {
                crate::TokenEstimateMode::parse(m).as_str().to_string()
            }
        },
        overflowed,
        timed_world_info: if dry_run { None } else { Some(timed) },
        prompt_slots,
        skipped_vectorized,
        vector_activated,
        skipped_filter,
        skipped_trigger,
        automation_ids: automation_ids_collect,
        example_messages,
    }
}

fn filter_inclusion_groups(
    activated_now: &mut Vec<(WiEntry, String)>,
    all_activated: &HashMap<String, (WiEntry, String)>,
    chat: &[String],
    settings: &WiSettings,
    depth_skew: i32,
    recurse: &[String],
    rng: &mut impl Rng,
) {
    // group name -> indices
    let mut groups: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, (e, _)) in activated_now.iter().enumerate() {
        if e.group.trim().is_empty() {
            continue;
        }
        for g in e.group.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
            groups.entry(g.to_string()).or_default().push(i);
        }
    }
    if groups.is_empty() {
        return;
    }
    let mut remove: HashSet<usize> = HashSet::new();
    for (gname, idxs) in &groups {
        // already activated in previous loop?
        if all_activated
            .values()
            .any(|(e, _)| e.group.split(',').any(|x| x.trim() == gname.as_str()))
        {
            for &i in idxs {
                remove.insert(i);
            }
            continue;
        }
        if idxs.len() <= 1 {
            continue;
        }
        // prio override
        let mut prio: Option<usize> = None;
        let mut best_order = i32::MIN;
        for &i in idxs {
            if activated_now[i].0.group_override && activated_now[i].0.order >= best_order {
                best_order = activated_now[i].0.order;
                prio = Some(i);
            }
        }
        if let Some(w) = prio {
            for &i in idxs {
                if i != w {
                    remove.insert(i);
                }
            }
            continue;
        }
        // group scoring
        let use_score = settings.use_group_scoring
            || idxs
                .iter()
                .any(|&i| activated_now[i].0.use_group_scoring.unwrap_or(false));
        if use_score {
            let depth = settings.depth + depth_skew;
            let text = buffer_text(chat, depth, recurse);
            let scores: Vec<i32> = idxs
                .iter()
                .map(|&i| score_entry(&activated_now[i].0, &text, settings))
                .collect();
            let max = scores.iter().copied().max().unwrap_or(0);
            for (k, &i) in idxs.iter().enumerate() {
                let scored = activated_now[i]
                    .0
                    .use_group_scoring
                    .unwrap_or(settings.use_group_scoring);
                if scored && scores[k] < max {
                    remove.insert(i);
                }
            }
            // recompute remaining for weight roll
        }
        let remain: Vec<usize> = idxs.iter().copied().filter(|i| !remove.contains(i)).collect();
        if remain.len() <= 1 {
            continue;
        }
        // weighted random
        let total: f64 = remain
            .iter()
            .map(|&i| activated_now[i].0.group_weight.max(0.0))
            .sum();
        let roll = rng.gen::<f64>() * total.max(1.0);
        let mut acc = 0.0;
        let mut winner = remain[0];
        for &i in &remain {
            acc += activated_now[i].0.group_weight.max(0.0);
            if roll <= acc {
                winner = i;
                break;
            }
        }
        for &i in &remain {
            if i != winner {
                remove.insert(i);
            }
        }
    }
    if remove.is_empty() {
        return;
    }
    let i = 0;
    let mut idx = 0;
    activated_now.retain(|_| {
        let drop = remove.contains(&idx);
        idx += 1;
        !drop
    });
    let _ = i;
}

/// Format WI scan into the Kaleido system-prompt section (before + after around card).
pub fn format_wi_for_system(scan: &WiScanResult) -> String {
    let mut parts = Vec::new();
    if !scan.world_info_before.trim().is_empty() {
        parts.push(scan.world_info_before.trim().to_string());
    }
    if !scan.em_before.trim().is_empty() {
        parts.push(format!("[EM↑]\n{}", scan.em_before.trim()));
    }
    if !scan.an_before.trim().is_empty() {
        parts.push(scan.an_before.trim().to_string());
    }
    if !scan.world_info_after.trim().is_empty() {
        parts.push(scan.world_info_after.trim().to_string());
    }
    if !scan.an_after.trim().is_empty() {
        parts.push(scan.an_after.trim().to_string());
    }
    if !scan.em_after.trim().is_empty() {
        parts.push(format!("[EM↓]\n{}", scan.em_after.trim()));
    }
    for d in &scan.depth_entries {
        if !d.content.trim().is_empty() {
            parts.push(format!("[depth {}]\n{}", d.depth, d.content.trim()));
        }
    }
    for o in &scan.outlet_entries {
        if !o.content.trim().is_empty() {
            parts.push(format!("[outlet:{}]\n{}", o.name, o.content.trim()));
        }
    }
    parts.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ent(uid: &str, keys: &[&str], content: &str, constant: bool, order: i32) -> WiEntry {
        WiEntry {
            uid: uid.into(),
            world: "test".into(),
            keys: keys.iter().map(|s| s.to_string()).collect(),
            keysecondary: vec![],
            content: content.into(),
            comment: uid.into(),
            constant,
            disable: false,
            selective: false,
            selective_logic: SelectiveLogic::AndAny,
            order,
            position: WiPosition::Before,
            depth: 4,
            probability: 100.0,
            use_probability: false,
            scan_depth: None,
            case_sensitive: None,
            match_whole_words: None,
            group: String::new(),
            group_override: false,
            group_weight: 100.0,
            use_group_scoring: None,
            exclude_recursion: false,
            prevent_recursion: false,
            delay_until_recursion: false,
            ignore_budget: false,
            sticky: None,
            cooldown: None,
            delay: None,
            decorators: Vec::new(),
            outlet_name: String::new(),
            character_filter: None,
            triggers: Vec::new(),
            vectorized: false,
            automation_id: String::new(),
            role: 0,
        }
    }

    #[test]
    fn constant_always_on() {
        let entries = vec![ent("c", &[], "CONST_LORE", true, 10)];
        let chat = vec!["hello".into()];
        let r = check_world_info(&entries, &chat, 4096, &WiSettings::default());
        assert!(r.world_info_before.contains("CONST_LORE"));
    }

    #[test]
    fn key_match_activates() {
        let entries = vec![ent("k", &["storm"], "STORM_LORE", false, 50)];
        let chat = vec!["The storm rolls in.".into()];
        let r = check_world_info(&entries, &chat, 4096, &WiSettings::default());
        assert!(r.world_info_before.contains("STORM_LORE") || r.world_info_after.contains("STORM_LORE") || r.activated.iter().any(|a| a.content.contains("STORM_LORE")));
        // position Before → before
        assert!(format_wi_for_system(&r).contains("STORM_LORE"));
    }

    #[test]
    fn key_miss_skips() {
        let entries = vec![ent("k", &["storm"], "STORM_LORE", false, 50)];
        let chat = vec!["sunny day".into()];
        let r = check_world_info(&entries, &chat, 4096, &WiSettings::default());
        assert!(r.activated.is_empty());
    }

    #[test]
    fn regex_key() {
        let mut e = ent("r", &[r"/\bstorm\b/i"], "RE_LORE", false, 1);
        e.keys = vec![r"/\bstorm\b/i".into()];
        let chat = vec!["Storm ahead".into()];
        let r = check_world_info(&[e], &chat, 4096, &WiSettings::default());
        assert_eq!(r.activated.len(), 1);
    }

    #[test]
    fn secondary_and_any() {
        let mut e = ent("s", &["hero"], "SEC", false, 1);
        e.selective = true;
        e.keysecondary = vec!["sword".into(), "blade".into()];
        e.selective_logic = SelectiveLogic::AndAny;
        let chat = vec!["the hero draws a sword".into()];
        let r = check_world_info(&[e], &chat, 4096, &WiSettings::default());
        assert_eq!(r.activated.len(), 1);
    }

    #[test]
    fn parse_character_book_entries() {
        let book = json!({
            "stBookRaw": {
                "name": "B",
                "entries": [
                    {"keys":["a"],"content":"A","enabled":true,"constant":false,"insertion_order":5},
                    {"keys":["b"],"content":"B","disable":true}
                ]
            }
        });
        let ents = entries_from_world_book("W", Some(&book), "");
        assert_eq!(ents.len(), 2);
        assert!(ents.iter().any(|e| e.disable));
        assert!(ents.iter().any(|e| e.content == "A" && !e.disable));
    }

    #[test]
    fn import_card_world_book_filters_disabled() {
        let card = crate::parse_st_character_card_json(
            r#"{
              "spec": "chara_card_v2",
              "data": {
                "name": "WB Card",
                "description": "d",
                "world_book": {
                  "name": "CardWorldBook",
                  "entries": [
                    {"keys": ["storm", "rain"], "content": "Coast never dries.", "enabled": true, "constant": true, "comment": "weather"},
                    {"keys": ["skip"], "content": "disabled by enabled=false", "enabled": false},
                    {"keys": ["skip2"], "content": "disabled by disable=true", "disable": true},
                    {"keys": ["ok"], "content": "enabled by default"}
                  ]
                }
              }
            }"#,
        )
        .unwrap();
        let ents = import_card_world_book(&card, "W");
        assert_eq!(ents.len(), 2);
        let storm = ents.iter().find(|e| e.keys.contains(&"storm".to_string())).unwrap();
        assert_eq!(storm.content, "Coast never dries.");
        assert!(storm.constant);
        assert!(!storm.disable);
        let ok = ents.iter().find(|e| e.keys.contains(&"ok".to_string())).unwrap();
        assert_eq!(ok.content, "enabled by default");
    }

    #[test]
    fn import_card_world_book_empty_card() {
        let card = crate::parse_st_character_card_json(
            r#"{"spec":"chara_card_v2","data":{"name":"NoBook","description":"d","world_book":{"name":"Empty","entries":[]}}}"#,
        )
        .unwrap();
        assert!(import_card_world_book(&card, "W").is_empty());

        let no_book = crate::parse_st_character_card_json(
            r#"{"spec":"chara_card_v2","data":{"name":"NoBook2","description":"d"}}"#,
        )
        .unwrap();
        assert!(import_card_world_book(&no_book, "W").is_empty());
    }
}
