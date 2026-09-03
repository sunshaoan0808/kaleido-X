//! Mood Baseline + Presence Derive — opening tint + at-work/away.
//!
//! Port of Front Porch AI `mood_baseline.dart` + `presence_derive.dart`
//! (AGPL-3.0, reimplemented). Pure, no LLM.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MoodBaseline {
    pub offset: i32, // -3..3
    #[serde(default)]
    pub causes: Vec<String>,
}

impl MoodBaseline {
    pub fn neutral() -> Self { Self { offset: 0, causes: vec![] } }
    pub fn is_neutral(&self) -> bool { self.offset == 0 || self.causes.is_empty() }
    pub fn summary(&self) -> String {
        if self.is_neutral() { return String::new(); }
        let head = if self.offset <= -3 { "running on empty" } else if self.offset < 0 { "not at her best" } else if self.offset >= 3 { "in good form" } else { "in decent spirits" };
        format!("{head} — {}", self.causes.join(", "))
    }
    /// Says STATE never mood; guardrail against steering scene into it.
    pub fn injection(&self) -> String {
        if self.is_neutral() { return String::new(); }
        format!("[Before this conversation started, {}. Let it colour their tone — shorter, warmer, more distracted as fits — but do NOT raise it as a topic or invent an incident. It is simply how their day has gone.]", self.causes.join(" and "))
    }
}

/// needs: 0-100 map. time_of_day: "night"/"dawn"/"". weather_miserable/weather_beautiful flags.
pub fn derive_mood(needs: &std::collections::HashMap<String, i32>, time_of_day: &str, weather_miserable: bool, weather_beautiful: bool) -> MoodBaseline {
    let mut offset = 0i32;
    let mut causes: Vec<String> = vec![];
    let mut need = |key: &str, threshold: i32, cause: &str, weight: i32| {
        if let Some(&v) = needs.get(key) { if v <= threshold { causes.push(cause.to_string()); offset += weight; } }
    };
    need("energy", 25, "they are exhausted", -2);
    need("hunger", 25, "they have not eaten", -1);
    need("comfort", 20, "they ache", -1);
    need("social", 15, "they have been alone too long", -1);
    need("hygiene", 15, "they feel unclean", -1);
    if let Some(&e) = needs.get("energy") { if e >= 90 { if needs.get("fun").map(|f| *f >= 70).unwrap_or(true) { causes.push("they slept well".into()); offset += 2; } } }
    if time_of_day == "night" || time_of_day == "dawn" { causes.push("it is the middle of the night".into()); offset -= 1; }
    if weather_miserable { causes.push("the weather has been miserable".into()); offset -= 1; }
    else if weather_beautiful { causes.push("it is a beautiful day".into()); offset += 1; }
    if causes.is_empty() { return MoodBaseline::neutral(); }
    MoodBaseline { offset: offset.clamp(-3, 3), causes }
}

/// Per-character work/presence config (stored in session.presence).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Presence {
    #[serde(default)]
    pub occupation: String,
    #[serde(default)]
    pub hours: String,
    #[serde(default)]
    pub brief: String,
    #[serde(default)]
    pub work_days: Option<Vec<i32>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PresenceWhere { WithYou, Away, AtWork }

pub fn presence_label(w: PresenceWhere) -> &'static str {
    match w { PresenceWhere::WithYou => "With you", PresenceWhere::Away => "Away", PresenceWhere::AtWork => "At work" }
}

/// hours like "9am-5pm"/"9:00-17:00". clock_minutes 0-1439. weekday 1-7.
pub fn parse_hours_range(hours: &str) -> Option<(i32, i32)> {
    let h = hours.trim().to_lowercase();
    if h.is_empty() { return None; }
    // split on -/–/to
    let parts: Vec<&str> = h.split(|c| c=='-'||c=='–'||c=='—').collect();
    let (a, b) = if parts.len() >= 2 { (parts[0].trim(), parts[1].trim()) } else {
        if let Some(idx) = h.find(" to ") { (&h[..idx], h[idx+4..].trim()) } else { return None; }
    };
    let s = to_minutes(a)?;
    let e = to_minutes(b)?;
    Some((s, e))
}

fn to_minutes(s: &str) -> Option<i32> {
    let t = s.trim().to_lowercase();
    // extract am/pm
    let (core, pm) = if t.ends_with("am") || t.ends_with("a.m.") { (t.trim_end_matches(|c| c=='a'||c=='m'||c=='.').trim(), false) }
    else if t.ends_with("pm") || t.ends_with("p.m.") { (t.trim_end_matches(|c| c=='p'||c=='m'||c=='.').trim(), true) }
    else { (t.as_str(), false) };
    // h[:mm]
    let mut it = core.split(':');
    let h: i32 = it.next()?.trim().parse().ok()?;
    let m: i32 = it.next().map(|x| x.trim().parse().unwrap_or(0)).unwrap_or(0);
    if !(0..=24).contains(&h) || !(0..=59).contains(&m) { return None; }
    let mut hour = h;
    if t.contains('m') { // has am/pm marker
        if h == 12 { hour = if pm { 12 } else { 0 }; }
        else if pm { hour = h + 12; }
    }
    Some(hour * 60 + m)
}

fn in_range(clock: i32, start: i32, end: i32) -> bool {
    if start <= end { clock >= start && clock < end } else { clock >= start || clock < end }
}

pub fn on_shift(hours: &str, clock_minutes: i32, weekday: i32, work_days: Option<&[i32]>) -> bool {
    let Some((s, e)) = parse_hours_range(hours) else { return false; };
    let days: Vec<i32> = work_days.map(|d| d.to_vec()).unwrap_or_else(|| vec![1,2,3,4,5]);
    if days.is_empty() { return false; }
    // overnight early belongs to previous day
    let overnight_early = s > e && clock_minutes < e;
    let day = if overnight_early { if weekday==1 {7} else {weekday-1} } else { weekday };
    days.contains(&day) && in_range(clock_minutes, s, e)
}

pub fn stance_says_away(stance: &str) -> bool {
    let s = stance.to_lowercase();
    if s.trim().is_empty() { return false; }
    ["left the","has left","walked off","walked away","gone from","not here","elsewhere","in another","next room","other room","down the hall","out of the room","out of sight"].iter().any(|m| s.contains(m))
}

pub fn derive_presence(occupation: &str, hours: &str, clock_minutes: i32, in_scene: bool, weekday: i32, work_days: Option<&[i32]>, stance: &str, with_user: Option<bool>) -> PresenceWhere {
    if !occupation.trim().is_empty() && !hours.trim().is_empty() && on_shift(hours, clock_minutes, weekday, work_days) {
        return PresenceWhere::AtWork;
    }
    let in_sc = if let Some(w) = with_user { w } else if stance_says_away(stance) { false } else { in_scene };
    if !in_sc { PresenceWhere::Away } else { PresenceWhere::WithYou }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn mood_exhausted() {
        let mut n = std::collections::HashMap::new();
        n.insert("energy".into(), 10);
        let m = derive_mood(&n, "", false, false);
        assert!(m.offset < 0);
        assert!(m.injection().contains("colour their tone"));
    }
    #[test] fn mood_neutral() {
        let m = derive_mood(&std::collections::HashMap::new(), "", false, false);
        assert!(m.is_neutral());
    }
    #[test] fn shift_parse() {
        assert_eq!(parse_hours_range("9am-5pm"), Some((540, 1020)));
        assert!(on_shift("9am-5pm", 600, 2, None));
        assert!(!on_shift("9am-5pm", 600, 6, None));
    }
    #[test] fn presence_work() {
        assert_eq!(derive_presence("nurse","9am-5pm",600,true,2,None,"",None), PresenceWhere::AtWork);
        assert_eq!(derive_presence("","",0,false,2,None,"",None), PresenceWhere::Away);
    }
}
