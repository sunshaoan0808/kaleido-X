//! Objectives / Ambitions — goal proposal + tasks + completion checks.
//!
//! Port of Front Porch AI `lib/services/chat/objective_proposal.dart` / `ambition_service.dart`
//! (AGPL-3.0, reimplemented). Pure structs + task lifecycle. No LLM.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Objective {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub owner: String, // character id
    #[serde(default)]
    pub status: String, // "active" | "completed" | "abandoned"
    #[serde(default)]
    pub tasks: Vec<ObjectiveTask>,
    pub created_at_turn: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ObjectiveTask {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub completed: bool,
}

impl Objective {
    pub fn new(owner: impl Into<String>, title: impl Into<String>, tasks: Vec<String>, turn: u32) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            title: title.into(),
            description: String::new(),
            owner: owner.into(),
            status: "active".into(),
            tasks: tasks.into_iter().map(|t| ObjectiveTask { id: uuid::Uuid::new_v4().to_string(), title: t, completed: false }).collect(),
            created_at_turn: turn,
        }
    }
    pub fn is_completed(&self) -> bool { !self.tasks.is_empty() && self.tasks.iter().all(|t| t.completed) }
    pub fn progress(&self) -> f64 { if self.tasks.is_empty() { 0.0 } else { self.tasks.iter().filter(|t| t.completed).count() as f64 / self.tasks.len() as f64 } }
    pub fn mark_task(&mut self, task_id: &str, completed: bool) -> bool {
        if let Some(t) = self.tasks.iter_mut().find(|t| t.id == task_id) { t.completed = completed; true } else { false }
    }
    pub fn auto_complete_if_all_done(&mut self) -> bool {
        if self.is_completed() && self.status == "active" { self.status = "completed".into(); true } else { false }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ambition {
    pub id: String,
    pub character: String,
    pub text: String,
    #[serde(default)]
    pub completed: bool,
    pub created_at_turn: u32,
}

impl Ambition {
    pub fn new(character: impl Into<String>, text: impl Into<String>, turn: u32) -> Self {
        Self { id: uuid::Uuid::new_v4().to_string(), character: character.into(), text: text.into(), completed: false, created_at_turn: turn }
    }
}

/// Ambition stage word — words only, never numbers (Front Porch ambition_service.stageWord).
pub fn ambition_stage_word(progress_pct: f64) -> &'static str {
    if progress_pct >= 100.0 { "achieved" }
    else if progress_pct >= 75.0 { "nearly there" }
    else if progress_pct >= 50.0 { "halfway there" }
    else if progress_pct >= 25.0 { "gaining ground" }
    else { "just beginning" }
}

/// Objective progress as stage word (0-100 from tasks).
pub fn objective_stage_word(o: &Objective) -> &'static str {
    ambition_stage_word(o.progress() * 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn objective_progress() {
        let mut o = Objective::new("Aria", "Find the key", vec!["search desk".into(), "ask guard".into()], 0);
        assert_eq!(o.progress(), 0.0);
        let tid = o.tasks[0].id.clone();
        o.mark_task(&tid, true);
        assert!(o.progress() > 0.4);
        assert!(!o.is_completed());
    }
    #[test] fn objective_auto_complete() {
        let mut o = Objective::new("Aria", "Done", vec!["a".into()], 0);
        let tid = o.tasks[0].id.clone();
        o.mark_task(&tid, true);
        assert!(o.auto_complete_if_all_done());
        assert_eq!(o.status, "completed");
    }
    #[test] fn ambition_new() {
        let a = Ambition::new("Aria", "Become queen", 5);
        assert_eq!(a.character, "Aria");
    }
    #[test] fn stage_words() {
        assert_eq!(ambition_stage_word(100.0), "achieved");
        assert_eq!(ambition_stage_word(80.0), "nearly there");
        assert_eq!(ambition_stage_word(10.0), "just beginning");
        let o = Objective::new("Aria", "Q", vec!["a".into()], 0);
        assert_eq!(objective_stage_word(&o), "just beginning");
    }
}
