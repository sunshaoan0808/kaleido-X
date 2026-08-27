//! Import security helpers (P7): strict UTF-8 decode, plain-text XSS threat
//! inspection, and size caps. Ported from Scriverse `src/import-security.ts`.
//!
//! Design: decode strictly (no lossy), then inspect for script-capable HTML
//! constructs. The 5 threat classes mirror the reference implementation:
//! ACTIVE_HTML_TAG / EVENT_HANDLER / SCRIPT_URI / EMBEDDED_DOCUMENT / ACTIVE_CSS.

use lazy_static::lazy_static;
use regex::Regex;
use unicode_normalization::UnicodeNormalization;

pub const MAX_IMPORT_TEXT_BYTES: usize = 32 * 1024 * 1024; // 32 MiB raw input cap
pub const MAX_IMPORT_TEXT_CHARS: usize = 16 * 1024 * 1024; // 16 M chars after decode

/// Threat classes produced by `inspect_imported_plain_text`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ImportThreat {
    ActiveHtmlTag,
    EventHandler,
    ScriptUri,
    EmbeddedDocument,
    ActiveCss,
}

lazy_static! {
    static ref RE_ACTIVE_HTML_TAG: Regex = Regex::new(
        r"(?i)<\s*/?\s*(script|iframe|object|embed|applet|meta|link|base|svg|math)\b"
    )
    .unwrap();
    static ref RE_EVENT_HANDLER: Regex =
        Regex::new(r"(?i)<[^>]{0,4000}\bon[a-z][\w:-]*\s*=").unwrap();
    static ref RE_SCRIPT_URI: Regex =
        Regex::new(r"(?i)<[^>]{0,4000}(javascript|vbscript):").unwrap();
    static ref RE_EMBEDDED_DOC_1: Regex =
        Regex::new(r"(?i)<[^>]{0,4000}\bsrcdoc\s*=").unwrap();
    static ref RE_EMBEDDED_DOC_2: Regex =
        Regex::new(r"(?i)<[^>]{0,4000}data:text/html").unwrap();
    static ref RE_ACTIVE_CSS: Regex = Regex::new(
        r"(?i)<[^>]{0,4000}\bstyle\s*=[^>]{0,4000}(expression\s*\(|url\s*\(\s*(javascript|vbscript):)"
    )
    .unwrap();
    static ref RE_ENT_HEX: Regex = Regex::new(r"(?i)&#x([0-9a-f]+);?").unwrap();
    static ref RE_ENT_DEC: Regex = Regex::new(r"&#([0-9]+);?").unwrap();
    static ref RE_ENT_COLON: Regex = Regex::new(r"(?i)&colon;").unwrap();
    static ref RE_ENT_WS: Regex = Regex::new(r"(?i)&(tab|newline);").unwrap();
}

/// Decode raw imported text bytes as strict UTF-8 (never lossy). Enforces the
/// raw size cap first so a huge upload is rejected without allocating.
pub fn decode_utf8_imported_text(value: &[u8]) -> Result<String, String> {
    if value.len() > MAX_IMPORT_TEXT_BYTES {
        return Err(format!(
            "import text too large: {} bytes (max {MAX_IMPORT_TEXT_BYTES})",
            value.len()
        ));
    }
    let s = std::str::from_utf8(value)
        .map_err(|e| format!("import text is not valid UTF-8: {e}"))?;
    if s.chars().count() > MAX_IMPORT_TEXT_CHARS {
        return Err(format!(
            "import text too long: {} chars (max {MAX_IMPORT_TEXT_CHARS})",
            s.chars().count()
        ));
    }
    Ok(s.to_string())
}

fn decode_entities_for_inspection(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut last = 0usize;
    for caps in RE_ENT_HEX.captures_iter(value) {
        let m = caps.get(0).unwrap();
        out.push_str(&value[last..m.start()]);
        if let Ok(cp) = u32::from_str_radix(&caps[1], 16) {
            if cp <= 0x10ffff {
                if let Some(ch) = char::from_u32(cp) {
                    out.push(ch);
                }
            }
        }
        last = m.end();
    }
    out.push_str(&value[last..]);
    let mut out2 = String::with_capacity(out.len());
    last = 0;
    for caps in RE_ENT_DEC.captures_iter(&out) {
        let m = caps.get(0).unwrap();
        out2.push_str(&out[last..m.start()]);
        if let Ok(cp) = caps[1].parse::<u32>() {
            if cp <= 0x10ffff {
                if let Some(ch) = char::from_u32(cp) {
                    out2.push(ch);
                }
            }
        }
        last = m.end();
    }
    out2.push_str(&out[last..]);
    RE_ENT_COLON.replace_all(&out2, ":").to_string()
}

/// Inspect plain-text import content for script-capable HTML constructs.
pub fn inspect_imported_plain_text(value: &str) -> Vec<ImportThreat> {
    let decoded = decode_entities_for_inspection(value);
    let normalized: String = decoded
        .nfkc()
        .filter(|c| !matches!(*c, '\u{0000}'..='\u{0008}' | '\u{000B}' | '\u{000C}' | '\u{000E}'..='\u{001F}' | '\u{007F}'))
        .collect();
    // Also normalize whitespace sequences in the same pass style as reference.
    let compact: String = normalized.chars().filter(|c| !c.is_whitespace()).collect();

    let mut threats = Vec::new();
    if RE_ACTIVE_HTML_TAG.is_match(&normalized) {
        threats.push(ImportThreat::ActiveHtmlTag);
    }
    if RE_EVENT_HANDLER.is_match(&normalized) {
        threats.push(ImportThreat::EventHandler);
    }
    if RE_SCRIPT_URI.is_match(&compact) {
        threats.push(ImportThreat::ScriptUri);
    }
    if RE_EMBEDDED_DOC_1.is_match(&normalized) || RE_EMBEDDED_DOC_2.is_match(&compact) {
        threats.push(ImportThreat::EmbeddedDocument);
    }
    if RE_ACTIVE_CSS.is_match(&normalized) {
        threats.push(ImportThreat::ActiveCss);
    }
    threats
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_strict_rejects_invalid() {
        assert!(decode_utf8_imported_text(b"ok").is_ok());
        // 0xFF is invalid UTF-8.
        assert!(decode_utf8_imported_text(&[0x61, 0xff, 0x62]).is_err());
        // Truncated multi-byte sequence.
        assert!(decode_utf8_imported_text(&[0xe4, 0xb8]).is_err());
        // Size cap.
        let big = vec![b'a'; MAX_IMPORT_TEXT_BYTES + 1];
        assert!(decode_utf8_imported_text(&big).is_err());
    }

    #[test]
    fn plain_text_safe_passes() {
        let text = "第一章 雨巷\n夜色渐深，青石板路上传来脚步声。";
        assert!(inspect_imported_plain_text(text).is_empty());
    }

    #[test]
    fn script_tag_threat() {
        let threats = inspect_imported_plain_text("<script>alert(1)</script>");
        assert!(threats.contains(&ImportThreat::ActiveHtmlTag));
    }

    #[test]
    fn event_handler_threat() {
        let threats = inspect_imported_plain_text("<img src=x onerror=alert(1)>");
        assert!(threats.contains(&ImportThreat::EventHandler));
    }

    #[test]
    fn script_uri_threat() {
        let threats = inspect_imported_plain_text("<a href=\"javascript:alert(1)\">x</a>");
        assert!(threats.contains(&ImportThreat::ScriptUri));
    }

    #[test]
    fn embedded_document_threat() {
        assert!(inspect_imported_plain_text("<iframe srcdoc=\"<script>x</script>\"></iframe>")
            .contains(&ImportThreat::EmbeddedDocument));
        assert!(inspect_imported_plain_text("<iframe src=\"data:text/html,x\"></iframe>")
            .contains(&ImportThreat::EmbeddedDocument));
    }

    #[test]
    fn active_css_threat() {
        let threats = inspect_imported_plain_text(
            "<div style=\"background:url(javascript:alert(1))\">x</div>",
        );
        assert!(threats.contains(&ImportThreat::ActiveCss));
    }

    #[test]
    fn entity_obfuscation_detected() {
        // &#x6a;avascript: decodes to "javascript:"
        let threats = inspect_imported_plain_text("<a href=\"&#x6a;avascript:alert(1)\">x</a>");
        assert!(threats.contains(&ImportThreat::ScriptUri));
    }

    #[test]
    fn control_char_stripped_before_match() {
        // Tab between < and script should not hide the tag after compacting.
        let threats = inspect_imported_plain_text("<\tscript>x</script>");
        assert!(threats.contains(&ImportThreat::ActiveHtmlTag));
    }
}
