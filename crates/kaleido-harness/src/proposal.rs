//! P1 proposal parsing and validation.
//!
//! Contains the harness error type, fault-tolerant JSON extraction from raw
//! LLM text, and edit validation.

use serde_json::Value;

use crate::model::{RefAction, RefinementEdit, RefinementKind};

/// Errors produced while parsing or validating refine proposals / edits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HarnessError {
    InvalidJson,
    Truncated,
    InvalidAction,
    InvalidKind,
    BasePromptImmutable,
    MissingId,
    MissingField(String),
    SkillRequiresReference,
    Conflict(String),
    NotFound(String),
    AlreadyExists(String),
}

impl std::fmt::Display for HarnessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HarnessError::InvalidJson => write!(f, "invalid JSON"),
            HarnessError::Truncated => write!(f, "truncated JSON"),
            HarnessError::InvalidAction => write!(f, "invalid action value"),
            HarnessError::InvalidKind => write!(f, "invalid kind value"),
            HarnessError::BasePromptImmutable => {
                write!(f, "base_system_prompt is immutable")
            }
            HarnessError::MissingId => write!(f, "missing entry id"),
            HarnessError::MissingField(field) => write!(f, "missing required field: {field}"),
            HarnessError::SkillRequiresReference => {
                write!(f, "skill requires reference and arguments")
            }
            HarnessError::Conflict(msg) => write!(f, "edit conflict: {msg}"),
            HarnessError::NotFound(msg) => write!(f, "entry not found: {msg}"),
            HarnessError::AlreadyExists(msg) => write!(f, "entry already exists: {msg}"),
        }
    }
}

impl std::error::Error for HarnessError {}

/// Returns the character index of the start of a balanced JSON object, scanning
/// from `bytes[open_at]` outward to find the matching close brace, or `None` if
/// not balanced.
fn balanced_object_end(bytes: &[u8], open_at: usize) -> Option<usize> {
    let mut depth: i64 = 0;
    let mut in_str = false;
    let mut escaped = false;
    for i in open_at..bytes.len() {
        let b = bytes[i];
        if in_str {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Extract a JSON object from raw (possibly noise-bearing) text.
///
/// Strategy:
/// 1. If the trimmed text is entirely a `{...}` object, parse directly.
/// 2. If a ` ```json ... ``` ` fence is present, parse the fenced content.
/// 3. Slice from the first `{` to the matching last `}` and parse.
/// 4. If the text looks like a truncated JSON object, return `Truncated`.
/// 5. Otherwise return `InvalidJson`.
pub fn extract_json_object(text: &str) -> Result<Value, HarnessError> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(HarnessError::InvalidJson);
    }

    // 1. Whole string is a JSON object.
    if trimmed.starts_with('{') && trimmed.ends_with('}') {
        if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
            return Ok(v);
        }
    }

    // 2. Fenced code block.
    if let Some(fenced) = extract_fenced(trimmed) {
        if let Ok(v) = serde_json::from_str::<Value>(fenced) {
            return Ok(v);
        }
        // Fenced but not parseable as complete object -> fall through to slice
        // logic so a truncated fence still gets a meaningful error.
    }

    // 3. First `{` to matching `}`.
    if let Some(start) = trimmed.find('{') {
        let bytes = trimmed.as_bytes();
        if let Some(end) = balanced_object_end(bytes, start) {
            let slice = &trimmed[start..=end];
            if let Ok(v) = serde_json::from_str::<Value>(slice) {
                return Ok(v);
            }
            // Not a complete object -> maybe truncated below.
        }
        // We found an opening brace but no balanced close.
        if is_incomplete_json(trimmed) {
            return Err(HarnessError::Truncated);
        }
    }

    let _ = trimmed;
    // No opening brace at all.
    if text.contains('{') {
        // Opening brace exists later but scan failed to correlate; report as
        // truncated only when there is obvious cut-off signal, else invalid.
        if is_incomplete_json(text) {
            return Err(HarnessError::Truncated);
        }
        return Err(HarnessError::InvalidJson);
    }

    if is_incomplete_json(text) {
        return Err(HarnessError::Truncated);
    }
    Err(HarnessError::InvalidJson)
}

fn extract_fenced(text: &str) -> Option<&str> {
    let markers = ["```json", "```"];
    let start = markers
        .iter()
        .filter_map(|m| text.find(m))
        .min()?;
    let after = start + 3; // skip ```
    // Skip optional language tag (e.g. "json") on the same line.
    let content_start = text[after..].find('\n').map(|i| after + i + 1)?;
    let rest = &text[content_start..];
    let close = rest.rfind("```")?;
    Some(&rest[..close])
}

/// Heuristic: does `text` look like a JSON object that got cut off mid-value?
fn is_incomplete_json(text: &str) -> bool {
    let t = text.trim();
    if !t.starts_with('{') {
        return false;
    }
    // If it starts with '{' but isn't balanced, likely truncated.
    let bytes = t.as_bytes();
    match balanced_object_end(bytes, 0) {
        Some(_) => false,
        None => true,
    }
}

/// Validate a single refinement edit.
pub fn validate_edit(e: &RefinementEdit) -> Result<(), HarnessError> {
    match e.action {
        RefAction::Create | RefAction::Update | RefAction::Delete => {}
    }
    // Deliberately explicit to keep the "action ∈ {...}" requirement visible.
    let action_ok = matches!(
        e.action,
        RefAction::Create | RefAction::Update | RefAction::Delete
    );
    if !action_ok {
        return Err(HarnessError::InvalidAction);
    }

    let kind_ok = matches!(
        e.kind,
        RefinementKind::Prompt
            | RefinementKind::Memory
            | RefinementKind::Skill
            | RefinementKind::Subagent
    );
    if !kind_ok {
        return Err(HarnessError::InvalidKind);
    }

    // Base system prompt is immutable.
    if e.kind == RefinementKind::Prompt && e.id.as_deref() == Some("base_system_prompt") {
        return Err(HarnessError::BasePromptImmutable);
    }

    match e.action {
        RefAction::Update | RefAction::Delete => {
            if e.id.is_none() {
                return Err(HarnessError::MissingId);
            }
        }
        RefAction::Create => {}
    }

    match e.action {
        RefAction::Create | RefAction::Update => {
            if e.title.is_none() {
                return Err(HarnessError::MissingField("title".into()));
            }
            if e.content.is_none() {
                return Err(HarnessError::MissingField("content".into()));
            }
        }
        RefAction::Delete => {}
    }

    // Skills must define a python call.
    if e.kind == RefinementKind::Skill {
        let reference_ok = match &e.reference {
            Some(Value::Object(map)) => {
                let Some(Value::String(typ)) = map.get("type") else {
                    return Err(HarnessError::SkillRequiresReference);
                };
                if typ != "python" {
                    return Err(HarnessError::SkillRequiresReference);
                }
                let has_import = map.get("import").is_some();
                let has_call = map.get("callable").is_some() || map.get("call_pattern").is_some();
                has_import && has_call
            }
            _ => false,
        };
        if !reference_ok || e.arguments.is_none() {
            return Err(HarnessError::SkillRequiresReference);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{RefAction, RefinementEdit, RefinementKind};

    fn edit(action: RefAction, kind: RefinementKind, id: Option<&str>) -> RefinementEdit {
        RefinementEdit {
            action,
            kind,
            id: id.map(|s| s.to_string()),
            title: Some("t".into()),
            content: Some("c".into()),
            path: None,
            reference: None,
            arguments: None,
            metadata: None,
            reason: None,
        }
    }

    fn skill_edit(action: RefAction, id: Option<&str>) -> RefinementEdit {
        RefinementEdit {
            action,
            kind: RefinementKind::Skill,
            id: id.map(|s| s.to_string()),
            title: Some("s".into()),
            content: Some("c".into()),
            path: None,
            reference: Some(serde_json::json!({
                "type": "python",
                "import": "my_mod",
                "callable": "func",
            })),
            arguments: Some(serde_json::json!({"x": 1})),
            metadata: None,
            reason: None,
        }
    }

    #[test]
    fn extract_pure_object() {
        let v = extract_json_object(r#"{"a":1,"b":[1,2]}"#).unwrap();
        assert_eq!(v["a"], 1);
    }

    #[test]
    fn extract_fenced_json() {
        let text = "Here you go:\n```json\n{\"nested\":{\"k\":\"v\"}}\n```\nthanks";
        let v = extract_json_object(text).unwrap();
        assert_eq!(v["nested"]["k"], "v");
    }

    #[test]
    fn extract_brace_slice() {
        let text = "prefix {\"a\":1} suffix";
        let v = extract_json_object(text).unwrap();
        assert_eq!(v["a"], 1);
    }

    #[test]
    fn extract_truncated() {
        let text = r#"{"a": 1, "b": [1, 2"#;
        assert_eq!(extract_json_object(text).unwrap_err(), HarnessError::Truncated);
    }

    #[test]
    fn extract_garbage() {
        assert_eq!(
            extract_json_object("no json here at all").unwrap_err(),
            HarnessError::InvalidJson
        );
        assert_eq!(extract_json_object("").unwrap_err(), HarnessError::InvalidJson);
    }

    #[test]
    fn base_prompt_immutable() {
        let e = edit(RefAction::Update, RefinementKind::Prompt, Some("base_system_prompt"));
        assert_eq!(validate_edit(&e).unwrap_err(), HarnessError::BasePromptImmutable);
        // Create is also rejected.
        let e2 = edit(RefAction::Create, RefinementKind::Prompt, Some("base_system_prompt"));
        assert_eq!(validate_edit(&e2).unwrap_err(), HarnessError::BasePromptImmutable);
    }

    #[test]
    fn skill_missing_reference_rejected() {
        let e = RefinementEdit {
            reference: None,
            arguments: Some(serde_json::json!({})),
            ..edit(RefAction::Create, RefinementKind::Skill, Some("s1"))
        };
        assert_eq!(validate_edit(&e).unwrap_err(), HarnessError::SkillRequiresReference);
    }

    #[test]
    fn skill_reference_without_arguments_rejected() {
        let e = RefinementEdit {
            arguments: None,
            ..skill_edit(RefAction::Create, Some("s2"))
        };
        assert_eq!(validate_edit(&e).unwrap_err(), HarnessError::SkillRequiresReference);
    }

    #[test]
    fn skill_reference_wrong_type_rejected() {
        let e = RefinementEdit {
            reference: Some(serde_json::json!({"type": "bash"})),
            ..skill_edit(RefAction::Create, Some("s3"))
        };
        assert_eq!(validate_edit(&e).unwrap_err(), HarnessError::SkillRequiresReference);
    }

    #[test]
    fn skill_ok_passes() {
        assert!(validate_edit(&skill_edit(RefAction::Create, Some("s4"))).is_ok());
    }

    #[test]
    fn missing_id_on_update_delete() {
        let u = edit(RefAction::Update, RefinementKind::Prompt, None);
        assert_eq!(validate_edit(&u).unwrap_err(), HarnessError::MissingId);
        let d = edit(RefAction::Delete, RefinementKind::Memory, None);
        assert_eq!(validate_edit(&d).unwrap_err(), HarnessError::MissingId);
    }

    #[test]
    fn missing_title_or_content() {
        let mut e = edit(RefAction::Create, RefinementKind::Memory, Some("m1"));
        e.title = None;
        assert_eq!(validate_edit(&e).unwrap_err(), HarnessError::MissingField("title".into()));
        let mut e2 = edit(RefAction::Create, RefinementKind::Memory, Some("m1"));
        e2.content = None;
        assert_eq!(validate_edit(&e2).unwrap_err(), HarnessError::MissingField("content".into()));
    }

    #[test]
    fn valid_non_skill_passes() {
        let e = edit(RefAction::Create, RefinementKind::Prompt, Some("p_new"));
        assert!(validate_edit(&e).is_ok());
        let d = edit(RefAction::Delete, RefinementKind::Subagent, Some("sub"));
        assert!(validate_edit(&d).is_ok());
    }
}