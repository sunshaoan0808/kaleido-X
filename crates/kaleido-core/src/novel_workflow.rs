//! novel_workflow: Generic narrative engineering methodology
//! Extracted from novel2hermes_jp skill — pure data structures + logic,
//! no LLM, no external deps. Supports independent file mode (v2).

use serde::{Deserialize, Serialize};

/// PlanningStage — the four fixed orders of worldbuilding → character → plot
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum PlanningStage {
    /// World rules, geography, organizations, constraints
    Worldbuilding,
    /// Character designs, arcs, knowledge bases
    Character,
    /// Timeline, major plot beats, major events
    Plot,
    /// All stages solidified — ready for writing
    Complete,
}

impl PlanningStage {
    pub fn name(&self) -> &'static str {
        match self {
            PlanningStage::Worldbuilding => "worldbuilding",
            PlanningStage::Character => "character",
            PlanningStage::Plot => "plot",
            PlanningStage::Complete => "complete",
        }
    }
}

/// PlanningGate — enterprise gate. Three orders must be complete before writing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanningGate {
    pub stage: PlanningStage,
    pub world_ready: bool,
    pub characters_ready: bool,
    pub plot_ready: bool,
}

impl PlanningGate {
    pub fn new() -> Self {
        Self {
            stage: PlanningStage::Worldbuilding,
            world_ready: false,
            characters_ready: false,
            plot_ready: false,
        }
    }

    pub fn can_start_writing(&self) -> bool {
        self.stage == PlanningStage::Complete
            && self.world_ready
            && self.characters_ready
            && self.plot_ready
    }

    pub fn advance(&mut self, stage: PlanningStage) -> Result<(), String> {
        let current_idx = match self.stage {
            PlanningStage::Worldbuilding => 0,
            PlanningStage::Character => 1,
            PlanningStage::Plot => 2,
            PlanningStage::Complete => 3,
        };
        let target_idx = match stage {
            PlanningStage::Worldbuilding => 0,
            PlanningStage::Character => 1,
            PlanningStage::Plot => 2,
            PlanningStage::Complete => 3,
        };
        if target_idx <= current_idx {
            return Err("Cannot regress or stay at same stage".to_string());
        }
        match &stage {
            PlanningStage::Worldbuilding => self.world_ready = true,
            PlanningStage::Character => self.characters_ready = true,
            PlanningStage::Plot => self.plot_ready = true,
            PlanningStage::Complete => {
                self.world_ready = true;
                self.characters_ready = true;
                self.plot_ready = true;
            }
        }
        self.stage = stage;
        Ok(())
    }
}

#[cfg(test)]
mod planning_gate_tests {
    use super::*;

    #[test]
    fn basic_advance_order() {
        let mut gate = PlanningGate::new();
        assert_eq!(gate.stage, PlanningStage::Worldbuilding);
        assert!(!gate.can_start_writing());
        gate.advance(PlanningStage::Character).unwrap();
        assert_eq!(gate.stage, PlanningStage::Character);
        gate.advance(PlanningStage::Plot).unwrap();
        assert_eq!(gate.stage, PlanningStage::Plot);
        gate.advance(PlanningStage::Complete).unwrap();
        assert_eq!(gate.stage, PlanningStage::Complete);
        assert!(gate.can_start_writing());
    }

    #[test]
    fn cannot_regress() {
        let mut gate = PlanningGate::new();
        gate.advance(PlanningStage::Character).unwrap();
        let err = gate.advance(PlanningStage::Worldbuilding);
        assert!(err.is_err());
    }
}

/// RevisionPhase — the three revision gates A/B/C
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum RevisionPhase {
    /// Phase A: Plot verification (before writing)
    PlotVerification,
    /// Phase B: Consistency verification (after writing)
    ConsistencyVerification,
    /// Phase C: Reader perspective assessment (macro)
    ReaderPerspective,
    /// All phases done
    Done,
}

impl RevisionPhase {
    pub fn name(&self) -> &'static str {
        match self {
            RevisionPhase::PlotVerification => "plot_verification",
            RevisionPhase::ConsistencyVerification => "consistency_verification",
            RevisionPhase::ReaderPerspective => "reader_perspective",
            RevisionPhase::Done => "done",
        }
    }

    /// Default verification checks for this phase, auto-mounted on entry.
    pub fn default_checks(&self) -> Vec<RevisionCheck> {
        match self {
            RevisionPhase::PlotVerification => vec![
                RevisionCheck {
                    id: "a_timeline".into(),
                    name: "时间线无矛盾".into(),
                    passed: false,
                    note: String::new(),
                },
                RevisionCheck {
                    id: "a_knowledge".into(),
                    name: "角色知识状态明确".into(),
                    passed: false,
                    note: String::new(),
                },
                RevisionCheck {
                    id: "a_foreshadow".into(),
                    name: "伏笔张收对应".into(),
                    passed: false,
                    note: String::new(),
                },
            ],
            RevisionPhase::ConsistencyVerification => vec![
                RevisionCheck {
                    id: "b_dialogue".into(),
                    name: "对话链逻辑连贯".into(),
                    passed: false,
                    note: String::new(),
                },
                RevisionCheck {
                    id: "b_terms".into(),
                    name: "指示词范围一致".into(),
                    passed: false,
                    note: String::new(),
                },
                RevisionCheck {
                    id: "b_sensory".into(),
                    name: "数值/五感一致".into(),
                    passed: false,
                    note: String::new(),
                },
            ],
            RevisionPhase::ReaderPerspective => vec![
                RevisionCheck {
                    id: "c_immersion".into(),
                    name: "c_immersion 代入感（读者视角）".into(),
                    passed: false,
                    note: String::new(),
                },
                RevisionCheck {
                    id: "c_emotion_curve".into(),
                    name: "c_emotion_curve 情绪曲线（峰谷节奏）".into(),
                    passed: false,
                    note: String::new(),
                },
                RevisionCheck {
                    id: "c_ending_strength".into(),
                    name: "c_ending_strength 结尾强度（收束力）".into(),
                    passed: false,
                    note: String::new(),
                },
            ],
            RevisionPhase::Done => vec![],
        }
    }
}

/// RevisionCheck — one verification item
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevisionCheck {
    pub id: String,
    pub name: String,
    pub passed: bool,
    pub note: String,
}

/// RevisionGate — per phase revision gate
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevisionGate {
    pub phase: RevisionPhase,
    pub checks: Vec<RevisionCheck>,
}

impl RevisionGate {
    pub fn new() -> Self {
        Self {
            phase: RevisionPhase::PlotVerification,
            checks: Vec::new(),
        }
    }

    /// Mount the default checks for the current phase (idempotent).
    pub fn mount_default_checks(&mut self) {
        for check in self.phase.default_checks() {
            if !self.checks.iter().any(|c| c.id == check.id) {
                self.checks.push(check);
            }
        }
    }

    pub fn phase_gate(&self) -> bool {
        self.phase != RevisionPhase::Done && self.checks.iter().all(|c| c.passed)
    }

    pub fn next_phase(&mut self) -> Result<(), String> {
        let next = match self.phase {
            RevisionPhase::PlotVerification => RevisionPhase::ConsistencyVerification,
            RevisionPhase::ConsistencyVerification => RevisionPhase::ReaderPerspective,
            RevisionPhase::ReaderPerspective => RevisionPhase::Done,
            RevisionPhase::Done => return Err("Already done".to_string()),
        };
        self.phase = next;
        self.mount_default_checks();
        Ok(())
    }
}

#[cfg(test)]
mod revision_phases_tests {
    use super::*;

    #[test]
    fn basic_phase_advance() {
        let mut gate = RevisionGate::new();
        assert_eq!(gate.phase, RevisionPhase::PlotVerification);
        assert!(gate.phase_gate());
        gate.next_phase().unwrap();
        assert_eq!(gate.phase, RevisionPhase::ConsistencyVerification);
        gate.next_phase().unwrap();
        assert_eq!(gate.phase, RevisionPhase::ReaderPerspective);
        gate.next_phase().unwrap();
        assert_eq!(gate.phase, RevisionPhase::Done);
    }

    #[test]
    fn fails_if_checks_not_passed() {
        let mut gate = RevisionGate::new();
        let check = RevisionCheck {
            id: "fail".into(),
            name: "not_passed".into(),
            passed: false,
            note: "failed".into(),
        };
        gate.checks.push(check);
        assert!(!gate.phase_gate());
    }

    #[test]
    fn reader_phase_mounts_default_checks() {
        let mut gate = RevisionGate::new();
        gate.mount_default_checks();
        assert_eq!(gate.checks.len(), 3);
        assert!(gate.checks.iter().any(|c| c.id == "a_timeline"));

        // Advance A -> B
        gate.next_phase().unwrap();
        assert_eq!(gate.phase, RevisionPhase::ConsistencyVerification);
        assert_eq!(gate.checks.len(), 6);
        assert!(gate.checks.iter().any(|c| c.id == "b_dialogue"));

        // Advance B -> C (ReaderPerspective): C checks auto-mounted
        gate.next_phase().unwrap();
        assert_eq!(gate.phase, RevisionPhase::ReaderPerspective);
        assert_eq!(gate.checks.len(), 9);
        assert!(gate.checks.iter().any(|c| c.id == "c_immersion"));
        assert!(gate.checks.iter().any(|c| c.id == "c_emotion_curve"));
        assert!(gate.checks.iter().any(|c| c.id == "c_ending_strength"));

        // C check names embed the english id so LLM output can map back
        let c = gate.checks.iter().find(|c| c.id == "c_ending_strength").unwrap();
        assert!(c.name.starts_with("c_ending_strength"));
        let ci = gate.checks.iter().find(|c| c.id == "c_immersion").unwrap();
        assert!(ci.name.starts_with("c_immersion"));

        // Idempotent: re-mounting does not duplicate
        gate.mount_default_checks();
        assert_eq!(gate.checks.len(), 9);

        // Advance C -> Done: no extra checks
        gate.next_phase().unwrap();
        assert_eq!(gate.phase, RevisionPhase::Done);
        assert_eq!(gate.checks.len(), 9);
    }
}

/// CharacterKnowledge — what the character knows / doesn't know at this scene
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CharacterKnowledge {
    pub character: String,
    pub knows: Vec<String>,
    pub not_knows: Vec<String>,
}

/// SceneKnowledge — per-scene knowledge ledger
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SceneKnowledge {
    pub scene: String,
    pub characters: Vec<CharacterKnowledge>,
}

/// KnowledgeViolation — character said something they shouldn't know
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeViolation {
    pub scene: String,
    pub character: String,
    pub claimed: String,
    pub reason: String,
}

/// KnowledgeLedger — global knowledge state table
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeLedger {
    pub scenes: Vec<SceneKnowledge>,
    pub violations: Vec<KnowledgeViolation>,
}

impl KnowledgeLedger {
    pub fn new() -> Self {
        Self {
            scenes: Vec::new(),
            violations: Vec::new(),
        }
    }

    pub fn check_statement(&mut self, scene: &str, character: &str, statement: &str) -> Result<(), KnowledgeViolation> {
        if let Some(sk) = self.scenes.iter_mut().find(|s| s.scene == scene) {
            if let Some(ck) = sk.characters.iter_mut().find(|c| c.character == character) {
                let stmt_lower = statement.to_lowercase();
                for nk in &ck.not_knows {
                    if stmt_lower.contains(&nk.to_lowercase()) {
                        return Err(KnowledgeViolation {
                            scene: scene.to_string(),
                            character: character.to_string(),
                            claimed: statement.to_string(),
                            reason: format!("{} said something they shouldn't know: {}", character, statement),
                        });
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod knowledge_state_tests {
    use super::*;

    #[test]
    fn basic_violation_detection() {
        let mut ledger = KnowledgeLedger::new();
        ledger.scenes.push(SceneKnowledge {
            scene: "scene1".into(),
            characters: vec![CharacterKnowledge {
                character: "hero".into(),
                knows: vec!["known".into()],
                not_knows: vec!["secret".into()],
            }],
        });
        let res = ledger.check_statement("scene1", "hero", "I know the secret");
        assert!(res.is_err());
    }

    #[test]
    fn no_violation_when_known() {
        let mut ledger = KnowledgeLedger::new();
        ledger.scenes.push(SceneKnowledge {
            scene: "scene1".into(),
            characters: vec![CharacterKnowledge {
                character: "hero".into(),
                knows: vec!["secret".into()],
                not_knows: vec!["unknown".into()],
            }],
        });
        let res = ledger.check_statement("scene1", "hero", "I know the secret");
        assert!(res.is_ok());
    }
}

/// Foreshadow struct — planted hook that must be resolved
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Foreshadow {
    pub id: String,
    pub desc: String,
    pub planted_chapter: String,
    pub resolved_chapter: Option<String>,
}

/// ForeshadowLedger — global foreshadow tracking ledger
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForeshadowLedger {
    pub items: Vec<Foreshadow>,
}

impl ForeshadowLedger {
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
        }
    }

    pub fn plant(&mut self, id: &str, desc: &str, chapter: &str) {
        if self.items.iter().any(|f| f.id == id) {
            return;
        }
        self.items.push(Foreshadow {
            id: id.to_string(),
            desc: desc.to_string(),
            planted_chapter: chapter.to_string(),
            resolved_chapter: None,
        });
    }

    pub fn resolve(&mut self, id: &str, chapter: &str) -> Result<(), String> {
        if let Some(pos) = self.items.iter().position(|f| f.id == id) {
            if self.items[pos].resolved_chapter.is_some() {
                return Err("Already resolved".to_string());
            }
            self.items[pos].resolved_chapter = Some(chapter.to_string());
            return Ok(());
        }
        Err("Foreshadow not found".to_string())
    }

    pub fn unresolved(&self) -> Vec<&Foreshadow> {
        self.items.iter().filter(|f| f.resolved_chapter.is_none()).collect()
    }

    pub fn resolve_rate(&self) -> f32 {
        let total = self.items.len() as f32;
        if total == 0.0 {
            return 1.0;
        }
        let resolved = self.items.iter().filter(|f| f.resolved_chapter.is_some()).count() as f32;
        resolved / total
    }
}

#[cfg(test)]
mod foreshadow_ledger_tests {
    use super::*;

    #[test]
    fn basic_plant_and_resolve() {
        let mut ledger = ForeshadowLedger::new();
        ledger.plant("id1", "desc1", "ch1");
        assert_eq!(ledger.unresolved().len(), 1);
        ledger.resolve("id1", "ch2").unwrap();
        assert_eq!(ledger.unresolved().len(), 0);
        assert_eq!(ledger.resolve_rate(), 1.0);
    }

    #[test]
    fn cannot_double_resolve() {
        let mut ledger = ForeshadowLedger::new();
        ledger.plant("id1", "desc1", "ch1");
        ledger.resolve("id1", "ch2").unwrap();
        let err = ledger.resolve("id1", "ch3");
        assert!(err.is_err());
    }
}
