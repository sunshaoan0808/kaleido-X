//! Living Time small rituals — absence ack, AFK flavor, today tag, objective mention gate.
//!
//! Port of Front Porch AI `absence_tracker.dart` + `afk_flavor.dart` +
//! `today_line_tag.dart` + `objective_mention_gate.dart` (AGPL-3.0, reimplemented).
//! All pure, no I/O.

/// Coarse bucket phrase for a real-world gap (hours), None below threshold. Words only, no digits.
pub fn absence_bucket(gap_hours: i64, threshold_hours: i64) -> Option<&'static str> {
    if gap_hours < threshold_hours || gap_hours < 12 { return None; }
    if gap_hours < 48 { Some("a day") }
    else if gap_hours < 24 * 7 { Some("a few days") }
    else if gap_hours < 24 * 14 { Some("about a week") }
    else if gap_hours < 24 * 28 { Some("a couple of weeks") }
    else { Some("a long while") }
}

/// One-shot opt-in prompt note for in-character gap ack. Meta, speculation-forbidding.
pub fn absence_ack_note(phrase: &str) -> String {
    format!("(Out of story: about {phrase} has passed in the real world since the last exchange. You may acknowledge the gap once, briefly and warmly, in character — but never guess what the user was doing, never ask where they were, never imply you were watching or waiting anxiously, and do not mention it again afterward.)")
}

/// AFK snapshot preamble for away-pace periods.
pub fn afk_time_phrase(periods: u32) -> &'static str {
    if periods >= 6 { "A full day has passed." }
    else if periods >= 3 { "Much of the day has slipped by." }
    else { "A few hours have passed." }
}

/// Parse+strip `[today: ...]` from a model reply. Returns (visible, line).
/// line None = no tag (keep hold); Some("") = blank tag (abandon).
pub fn parse_today_tag(raw: &str) -> (String, Option<String>) {
    let lower = raw.to_lowercase();
    let Some(start) = lower.find("[today:") else { return (raw.to_string(), None); };
    let Some(end_rel) = raw[start..].find(']') else { return (raw.to_string(), None); };
    let end = start + end_rel;
    let mut line = raw[start + 7..end].trim().replace(|c: char| c.is_whitespace(), " ");
    while line.contains("  ") { line = line.replace("  ", " "); }
    if line.len() > 140 { line = line.chars().take(140).collect::<String>().trim().to_string(); }
    let mut visible = format!("{}{}", &raw[..start], &raw[end + 1..]);
    // collapse 3+ newlines
    while visible.contains("\n\n\n") { visible = visible.replace("\n\n\n", "\n\n"); }
    let visible = visible.trim().to_string();
    if line.is_empty() { (visible, Some(String::new())) } else { (visible, Some(line)) }
}

const OBJECTIVE_STOPWORDS: &[&str] = &[
    "about","after","again","their","there","these","those","thing","things","something","someone",
    "somewhere","being","doing","going","gets","goes","have","having","into","just","like","make",
    "makes","making","more","most","much","must","need","needs","other","over","some","take","takes",
    "taking","than","that","them","then","they","this","time","truly","very","want","wants","what",
    "when","where","which","while","will","with","without","would","your","yours","from","find",
    "finds","finally","genuinely","herself","himself","themselves","toward","towards",
];

/// True if recent text mentions any quest text's significant word (4+ alpha chars, not stopword).
pub fn objectives_mentioned_in(recent_lower: &str, quest_texts: &[String]) -> bool {
    if recent_lower.trim().is_empty() { return false; }
    for qt in quest_texts {
        let low = qt.to_lowercase();
        let mut start: Option<usize> = None;
        for (i, c) in low.char_indices().chain(std::iter::once((low.len(), ' '))) {
            if c.is_ascii_alphabetic() || c == '\'' {
                if start.is_none() { start = Some(i); }
            } else if let Some(s) = start.take() {
                let w = &low[s..i];
                if w.len() >= 4 && w.chars().all(|x| x.is_ascii_alphabetic() || x == '\'') && !OBJECTIVE_STOPWORDS.contains(&w) {
                    // word boundary check
                    let pat = format!("\\b{w}\\b");
                    // simple boundary: search with surrounding non-alpha
                    if contains_word(recent_lower, w) { return true; }
                    let _ = pat;
                }
            }
        }
    }
    false
}

fn contains_word(hay: &str, word: &str) -> bool {
    let hb = hay.as_bytes();
    let wb = word.as_bytes();
    if wb.is_empty() || hb.len() < wb.len() { return false; }
    let mut i = 0;
    while i + wb.len() <= hb.len() {
        if &hb[i..i + wb.len()] == wb {
            let left_ok = i == 0 || !hb[i - 1].is_ascii_alphabetic();
            let right_ok = i + wb.len() == hb.len() || !hb[i + wb.len()].is_ascii_alphabetic();
            if left_ok && right_ok { return true; }
        }
        i += 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn bucket_words_only() {
        assert_eq!(absence_bucket(10, 24), None);
        assert_eq!(absence_bucket(30, 24), Some("a day"));
        assert_eq!(absence_bucket(24*30, 24), Some("a long while"));
    }
    #[test] fn today_parse() {
        let (v, l) = parse_today_tag("hello [today: sunny morning] bye");
        assert_eq!(l, Some("sunny morning".into()));
        assert!(!v.contains("[today:"));
        let (_, none) = parse_today_tag("plain");
        assert!(none.is_none());
    }
    #[test] fn objective_gate() {
        assert!(objectives_mentioned_in("we should find the lighthouse key", &["Find the lighthouse key".into()]));
        assert!(!objectives_mentioned_in("hello there", &["Find the lighthouse key".into()]));
    }
}
