//! Journal Store — in-session card persistence + heat lifecycle.
//!
//! Port of Front Porch AI `lib/services/chat/journal_store.dart` (AGPL-3.0, reimplemented).
//! Session-scoped (sessionId, characterId), no cross-chat, no DB — lives in TavernSession.
//! Cool/re-warm/trim delegated to journal_physics for caps.

use serde::{Deserialize, Serialize};

use crate::journal_physics::{
    cooled_heat, is_hot, is_ledger_card, K_COLD_THRESHOLD, K_MAX_HEAT, K_REWARM_HEAT,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JournalCard {
    pub id: String,
    pub session_id: String,
    pub character_id: String,
    pub content: String,
    #[serde(default)]
    pub category: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emotion_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emotion_intensity: Option<String>, // "mild" | "moderate" | "strong" | "pinned"
    #[serde(default)]
    pub heat: f64,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>, // "item" | "milestone" | "promise" | "episode" | "dream"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata_item: Option<String>,
    #[serde(default)]
    pub access_count: u32,
    #[serde(default)]
    pub created_at_turn: u32,
    /// Cold-card semantic recall vector (fastembed 512d when available; empty = no-RAG floor).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub embedding: Vec<f32>,
    /// Receipts: source message positions for tap-to-jump (Front Porch sourceMessageIds).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_positions: Vec<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub story_day: Option<u32>,
}

impl JournalCard {
    pub fn new(session_id: impl Into<String>, character_id: impl Into<String>, content: impl Into<String>, turn: u32) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            session_id: session_id.into(),
            character_id: character_id.into(),
            content: content.into(),
            category: "memory".into(),
            emotion_label: None,
            emotion_intensity: None,
            heat: K_MAX_HEAT,
            pinned: false,
            kind: None,
            metadata_item: None,
            access_count: 0,
            created_at_turn: turn,
            embedding: vec![],
            source_positions: vec![],
            story_day: None,
        }
    }
    fn cooled(&self) -> f64 {
        crate::journal_physics::JournalCard {
            heat: self.heat,
            pinned: self.pinned,
            intensity: self.emotion_intensity.clone().unwrap_or_else(|| "mild".into()),
            kind: self.kind.clone(),
            content: self.content.clone(),
            emotion_label: self.emotion_label.clone(),
            metadata_item: self.metadata_item.clone(),
            created_at: self.created_at_turn as i64,
        }.let_cooled()
    }
}

// helper to adapt physics card
trait LetCooled { fn let_cooled(&self) -> f64; }
impl LetCooled for crate::journal_physics::JournalCard {
    fn let_cooled(&self) -> f64 { cooled_heat(self) }
}

/// Salience kick gate: same session ≥4 messages between kicks (Front Porch kSalienceKickMinGapMessages).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SalienceGate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default)]
    pub last_allowed_at: i64,
}

impl SalienceGate {
    pub fn allow(&mut self, session_id: &str, message_count: i64) -> bool {
        if self.session_id.as_deref() != Some(session_id) {
            self.session_id = Some(session_id.to_string());
            self.last_allowed_at = -100;
        }
        if message_count - self.last_allowed_at < 4 { return false; }
        self.last_allowed_at = message_count;
        true
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct JournalStore {
    #[serde(default)]
    pub cards: Vec<JournalCard>,
    #[serde(default)]
    pub salience_gate: SalienceGate,
}

impl JournalStore {
    pub fn cards_for(&self, session_id: &str, character_id: &str) -> Vec<JournalCard> {
        let mut v: Vec<JournalCard> = self.cards.iter().filter(|c| c.session_id==session_id && c.character_id==character_id).cloned().collect();
        v.sort_by(|a,b| match b.pinned.cmp(&a.pinned) { std::cmp::Ordering::Equal => a.created_at_turn.cmp(&b.created_at_turn), o=>o });
        v
    }
    pub fn add_card(&mut self, card: JournalCard, max_cards: usize) {
        let count = self.cards.iter().filter(|c| c.session_id==card.session_id && c.character_id==card.character_id).count();
        if count >= max_cards {
            // evict coldest unpinned non-ledger
            let victim = self.cards.iter()
                .filter(|c| c.session_id==card.session_id && c.character_id==card.character_id && !c.pinned && !matches!(c.kind.as_deref(), Some("milestone")|Some("promise")))
                .min_by(|a,b| a.heat.partial_cmp(&b.heat).unwrap_or(std::cmp::Ordering::Equal))
                .map(|c| c.id.clone());
            if let Some(id) = victim { self.cards.retain(|c| c.id!=id); }
        }
        self.cards.push(card);
    }
    pub fn revise(&mut self, id: &str, content: Option<String>, feeling: Option<String>) -> bool {
        if let Some(c) = self.cards.iter_mut().find(|c| c.id==id) {
            if let Some(t)=content { if !t.is_empty() { c.content=t; } }
            if let Some(f)=feeling { if !f.is_empty() && Some(&f)!=c.emotion_label.as_ref() { c.emotion_label=Some(f); } }
            c.heat = K_MAX_HEAT;
            return true;
        }
        false
    }
    pub fn retire(&mut self, id: &str) -> bool { let n=self.cards.len(); self.cards.retain(|c| c.id!=id); n!=self.cards.len() }
    pub fn set_pinned(&mut self, id: &str, pinned: bool) -> bool { if let Some(c)=self.cards.iter_mut().find(|c| c.id==id) { c.pinned=pinned; true } else { false } }
    pub fn cool(&mut self, session_id: &str, character_id: &str) {
        for c in self.cards.iter_mut().filter(|c| c.session_id==session_id && c.character_id==character_id) {
            let phys = crate::journal_physics::JournalCard {
                heat: c.heat, pinned: c.pinned,
                intensity: c.emotion_intensity.clone().unwrap_or_else(|| "mild".into()),
                kind: c.kind.clone(), content: c.content.clone(),
                emotion_label: c.emotion_label.clone(), metadata_item: c.metadata_item.clone(),
                created_at: c.created_at_turn as i64,
            };
            let cooled = cooled_heat(&phys);
            if cooled != c.heat { c.heat = cooled; }
        }
    }
    pub fn rewarm(&mut self, id: &str) -> bool {
        if let Some(c)=self.cards.iter_mut().find(|c| c.id==id) {
            if c.heat < K_REWARM_HEAT { c.heat = K_REWARM_HEAT; }
            c.access_count += 1;
            true
        } else { false }
    }
    pub fn injection_block(&self, session_id: &str, character_id: &str, character_name: &str, current_emotion: &str, budget_chars: usize) -> String {
        let mut cards = self.cards_for(session_id, character_id);
        // hot + pinned ordered by heat+mood
        let current = current_emotion.to_string();
        cards.sort_by(|a,b| {
            let ka = a.heat + if crate::journal_physics::mood_congruent(a.emotion_label.as_deref(), &current) { crate::journal_physics::K_MOOD_BOOST } else { 0.0 };
            let kb = b.heat + if crate::journal_physics::mood_congruent(b.emotion_label.as_deref(), &current) { crate::journal_physics::K_MOOD_BOOST } else { 0.0 };
            kb.partial_cmp(&ka).unwrap_or(std::cmp::Ordering::Equal)
        });
        let mut hot: Vec<&JournalCard> = vec![];
        for c in &cards {
            let phys = crate::journal_physics::JournalCard {
                heat: c.heat, pinned: c.pinned,
                intensity: c.emotion_intensity.clone().unwrap_or_else(|| "mild".into()),
                kind: c.kind.clone(), content: c.content.clone(),
                emotion_label: c.emotion_label.clone(), metadata_item: c.metadata_item.clone(),
                created_at: c.created_at_turn as i64,
            };
            if c.pinned || is_hot(&phys) { hot.push(c); }
            if hot.len()*80 >= budget_chars { break; }
        }
        if hot.is_empty() { return String::new(); }
        let mut lines: Vec<String> = vec![];
        let mut used = 0usize;
        for c in hot {
            let label = c.emotion_label.as_deref().unwrap_or("memory");
            let line = format!("- ({}): {}", label, c.content);
            if used + line.len() > budget_chars && !lines.is_empty() { break; }
            used += line.len();
            lines.push(line);
        }
        if lines.is_empty() { return String::new(); }
        format!("\n[{character_name}'s private journal — personal memories from this chat. These shape how they feel:\n{}\n]\n", lines.join("\n"))
    }
    /// Cold-card recall: cosine vs query vector (threshold 0.45, top 3) + keyword floor (item name mentioned).
    /// Returns recalled card ids (caller rewarm + inject). No-RAG floor: empty query → only keyword floor.
    pub fn recall_cold(&self, session_id: &str, character_id: &str, query: &[f32], query_text: &str, threshold: f64, top_k: usize) -> Vec<String> {
        let mut out: Vec<(String, f64)> = vec![];
        let qtokens: Vec<String> = query_text.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| w.len() >= 2).map(|s| s.to_string()).collect();
        for c in self.cards.iter().filter(|c| c.session_id==session_id && c.character_id==character_id) {
            let phys = crate::journal_physics::JournalCard {
                heat: c.heat, pinned: c.pinned,
                intensity: c.emotion_intensity.clone().unwrap_or_else(|| "mild".into()),
                kind: c.kind.clone(), content: c.content.clone(),
                emotion_label: c.emotion_label.clone(), metadata_item: c.metadata_item.clone(),
                created_at: c.created_at_turn as i64,
            };
            if c.pinned || crate::journal_physics::is_hot(&phys) { continue; }
            // keyword floor: item/episode name mentioned
            let content_low = c.content.to_lowercase();
            let item_hit = c.metadata_item.as_deref().map(|it| !it.is_empty() && query_text.to_lowercase().contains(&it.to_lowercase())).unwrap_or(false);
            let token_hit = qtokens.iter().any(|tok| content_low.contains(tok.as_str())) && !qtokens.is_empty() && c.content.len() < 200;
            if item_hit || (token_hit && query_text.len() >= 4) {
                out.push((c.id.clone(), 1.0));
                continue;
            }
            if !query.is_empty() && !c.embedding.is_empty() {
                let s = crate::st_vector_index::vector_cosine_similarity(query, &c.embedding);
                if s >= threshold { out.push((c.id.clone(), s)); }
            }
        }
        out.sort_by(|a,b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        out.truncate(top_k.max(1));
        out.into_iter().map(|(id,_)| id).collect()
    }
    /// Deterministic maintenance: strong-emotion turn / objective completion / promise resolve → auto card.
    /// Caller passes already-built content; here only cap/dedupe. Returns card id if written.
    pub fn maybe_write_auto(&mut self, session_id: &str, character_id: &str, content: String, kind: &str, emotion: Option<String>, turn: u32) -> Option<String> {
        if content.trim().is_empty() { return None; }
        // dedupe: same content last 5 cards
        let recent: Vec<&JournalCard> = self.cards.iter().filter(|c| c.session_id==session_id && c.character_id==character_id).collect();
        if recent.iter().rev().take(5).any(|c| c.content == content) { return None; }
        let mut card = JournalCard::new(session_id, character_id, content, turn);
        card.kind = Some(kind.to_string());
        if let Some(e)=emotion { card.emotion_label = Some(e); card.emotion_intensity = Some("strong".into()); }
        let id = card.id.clone();
        self.add_card(card, 50);
        Some(id)
    }
    /// Milestone plant (tier crossing → diary card). Emotion stamp warm/hurt/relieved/wary.
    pub fn plant_milestone(&mut self, session_id: &str, character_id: &str, text: String, axis: &str, rose: bool, turn: u32) -> Option<String> {
        let emotion = match axis { "trust" => if rose {"relieved"} else {"wary"}, "long_term" => if rose {"devoted"} else {"wistful"}, _ => if rose {"warm"} else {"hurt"} };
        self.maybe_write_auto(session_id, character_id, text, "milestone", Some(emotion.into()), turn)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn add_and_evict() {
        let mut s = JournalStore::default();
        for i in 0..5 { s.add_card(JournalCard::new("s1","c1", format!("m{i}"), i), 3); }
        assert_eq!(s.cards_for("s1","c1").len(), 3);
    }
    #[test] fn pinned_not_evicted() {
        let mut s = JournalStore::default();
        let mut pinned = JournalCard::new("s1","c1","keep",0); pinned.pinned=true;
        s.add_card(pinned, 2);
        s.add_card(JournalCard::new("s1","c1","a",1), 2);
        s.add_card(JournalCard::new("s1","c1","b",2), 2);
        // pinned must survive
        assert!(s.cards_for("s1","c1").iter().any(|c| c.pinned));
    }
    #[test] fn injection_hot_only() {
        let mut s = JournalStore::default();
        let mut cold = JournalCard::new("s1","c1","cold",0); cold.heat = 0.1;
        s.add_card(cold, 10);
        assert!(s.injection_block("s1","c1","Aria","",600).is_empty());
    }
}
