//! # World State Engine
//!
//! Formal state machine for narrative worlds. Inspired by Liyuan's session tree
//! and branching architecture. Provides typed entities, events, and conditions
//! for structured narrative state management.
//!
//! ## Core Concepts
//!
//! - **WorldEntity** — Any entity in the world (character, location, item, faction, concept)
//! - **WorldEvent** — A state mutation (create entity, update, remove, set flag, etc.)
//! - **Condition** — Boolean expression over world state (for branching logic)
//! - **WorldState** — Aggregate root holding all entities, metadata, and event log

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

// ---------------------------------------------------------------------------
// Entity Kind
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum EntityKind {
    Character,
    Location,
    Item,
    Faction,
    Concept,
    Custom(String),
}

// ---------------------------------------------------------------------------
// Relationship
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Relationship {
    pub target_id: String,
    pub relation_type: String,
    pub strength: f32,
}

// ---------------------------------------------------------------------------
// World Entity
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorldEntity {
    pub id: String,
    pub kind: EntityKind,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub properties: HashMap<String, Value>,
    #[serde(default)]
    pub relationships: Vec<Relationship>,
    #[serde(default)]
    pub state_flags: HashSet<String>,
    #[serde(default)]
    pub counters: HashMap<String, i32>,
}

// ---------------------------------------------------------------------------
// Comparison Operator
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Comparison {
    Eq,
    Neq,
    Gt,
    Lt,
    Gte,
    Lte,
}

// ---------------------------------------------------------------------------
// Condition — boolean expression over world state
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Condition {
    Flag {
        entity_id: Option<String>,
        flag: String,
        present: bool,
    },
    Counter {
        entity_id: Option<String>,
        counter: String,
        operator: Comparison,
        value: i32,
    },
    Relationship {
        source_id: String,
        target_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        relation_type: Option<String>,
        #[serde(default)]
        min_strength: f32,
    },
    EntityExists {
        id: String,
    },
    EntityKind {
        id: String,
        kind: EntityKind,
    },
    Property {
        entity_id: String,
        key: String,
        operator: Comparison,
        value: Value,
    },
    And(Vec<Condition>),
    Or(Vec<Condition>),
    Not(Box<Condition>),
    Always,
}

// ---------------------------------------------------------------------------
// World Event — a state mutation
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorldEvent {
    EntityCreated(WorldEntity),
    EntityUpdated {
        id: String,
        changes: HashMap<String, Value>,
    },
    EntityRemoved(String),
    FlagSet {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        entity_id: Option<String>,
        flag: String,
    },
    FlagCleared {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        entity_id: Option<String>,
        flag: String,
    },
    CounterChanged {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        entity_id: Option<String>,
        counter: String,
        delta: i32,
    },
    CounterSet {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        entity_id: Option<String>,
        counter: String,
        value: i32,
    },
    RelationshipSet {
        source_id: String,
        target_id: String,
        relation_type: String,
        strength: f32,
    },
    RelationshipRemoved {
        source_id: String,
        target_id: String,
        relation_type: String,
    },
    MetaSet {
        key: String,
        value: Value,
    },
    /// A descriptive event that is logged but has no mechanical effect.
    NarrativeEvent {
        summary: String,
        character_ids: Vec<String>,
        turn: u32,
    },
}

// ---------------------------------------------------------------------------
// World State — aggregate root
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorldState {
    pub entities: HashMap<String, WorldEntity>,
    #[serde(default)]
    pub meta: HashMap<String, Value>,
    #[serde(default)]
    pub global_flags: HashSet<String>,
    #[serde(default)]
    pub global_counters: HashMap<String, i32>,
    #[serde(default)]
    pub event_log: Vec<WorldEvent>,
}

impl WorldState {
    pub fn new() -> Self {
        Self {
            entities: HashMap::new(),
            meta: HashMap::new(),
            global_flags: HashSet::new(),
            global_counters: HashMap::new(),
            event_log: Vec::new(),
        }
    }

    /// Apply an event, mutating state and appending to the event log.
    pub fn apply(&mut self, event: WorldEvent) {
        match &event {
            WorldEvent::EntityCreated(entity) => {
                self.entities.insert(entity.id.clone(), entity.clone());
            }
            WorldEvent::EntityUpdated { id, changes } => {
                if let Some(entity) = self.entities.get_mut(id) {
                    for (key, value) in changes {
                        match key.as_str() {
                            "name" => {
                                if let Value::String(s) = value {
                                    entity.name = s.clone();
                                }
                            }
                            "description" => {
                                if let Value::String(s) = value {
                                    entity.description = s.clone();
                                }
                            }
                            _ => {
                                entity.properties.insert(key.clone(), value.clone());
                            }
                        }
                    }
                }
            }
            WorldEvent::EntityRemoved(id) => {
                self.entities.remove(id);
            }
            WorldEvent::FlagSet { entity_id, flag } => {
                if let Some(eid) = entity_id {
                    if let Some(entity) = self.entities.get_mut(eid) {
                        entity.state_flags.insert(flag.clone());
                    }
                } else {
                    self.global_flags.insert(flag.clone());
                }
            }
            WorldEvent::FlagCleared { entity_id, flag } => {
                if let Some(eid) = entity_id {
                    if let Some(entity) = self.entities.get_mut(eid) {
                        entity.state_flags.remove(flag);
                    }
                } else {
                    self.global_flags.remove(flag);
                }
            }
            WorldEvent::CounterChanged { entity_id, counter, delta } => {
                if let Some(eid) = entity_id {
                    if let Some(entity) = self.entities.get_mut(eid) {
                        let entry = entity.counters.entry(counter.clone()).or_insert(0);
                        *entry += delta;
                    }
                } else {
                    let entry = self.global_counters.entry(counter.clone()).or_insert(0);
                    *entry += delta;
                }
            }
            WorldEvent::CounterSet { entity_id, counter, value } => {
                if let Some(eid) = entity_id {
                    if let Some(entity) = self.entities.get_mut(eid) {
                        entity.counters.insert(counter.clone(), *value);
                    }
                } else {
                    self.global_counters.insert(counter.clone(), *value);
                }
            }
            WorldEvent::RelationshipSet { source_id, target_id, relation_type, strength } => {
                if let Some(entity) = self.entities.get_mut(source_id) {
                    // Remove existing relationship of same type+target before adding
                    entity.relationships.retain(|r| {
                        !(r.target_id == *target_id && r.relation_type == *relation_type)
                    });
                    entity.relationships.push(Relationship {
                        target_id: target_id.clone(),
                        relation_type: relation_type.clone(),
                        strength: *strength,
                    });
                }
            }
            WorldEvent::RelationshipRemoved { source_id, target_id, relation_type } => {
                if let Some(entity) = self.entities.get_mut(source_id) {
                    entity.relationships.retain(|r| {
                        !(r.target_id == *target_id && r.relation_type == *relation_type)
                    });
                }
            }
            WorldEvent::MetaSet { key, value } => {
                self.meta.insert(key.clone(), value.clone());
            }
            WorldEvent::NarrativeEvent { .. } => {
                // Logged only, no mechanical effect
            }
        }
        self.event_log.push(event);
    }

    /// Evaluate a condition against the current world state.
    pub fn evaluate(&self, condition: &Condition) -> bool {
        match condition {
            Condition::Always => true,
            Condition::And(conds) => conds.iter().all(|c| self.evaluate(c)),
            Condition::Or(conds) => conds.iter().any(|c| self.evaluate(c)),
            Condition::Not(inner) => !self.evaluate(inner),
            Condition::Flag { entity_id, flag, present } => {
                let has = if let Some(eid) = entity_id {
                    self.entities
                        .get(eid)
                        .map(|e| e.state_flags.contains(flag))
                        .unwrap_or(false)
                } else {
                    self.global_flags.contains(flag)
                };
                has == *present
            }
            Condition::Counter { entity_id, counter, operator, value } => {
                let actual = if let Some(eid) = entity_id {
                    self.entities
                        .get(eid)
                        .and_then(|e| e.counters.get(counter))
                        .copied()
                        .unwrap_or(0)
                } else {
                    *self.global_counters.get(counter).unwrap_or(&0)
                };
                apply_comparison(actual, *value, operator)
            }
            Condition::Relationship { source_id, target_id, relation_type, min_strength } => {
                if let Some(entity) = self.entities.get(source_id) {
                    entity.relationships.iter().any(|r| {
                        r.target_id == *target_id
                            && relation_type
                                .as_ref()
                                .map_or(true, |rt| r.relation_type == *rt)
                            && r.strength >= *min_strength
                    })
                } else {
                    false
                }
            }
            Condition::EntityExists { id } => self.entities.contains_key(id),
            Condition::EntityKind { id, kind } => self
                .entities
                .get(id)
                .map(|e| e.kind == *kind)
                .unwrap_or(false),
            Condition::Property { entity_id, key, operator, value } => {
                if let Some(entity) = self.entities.get(entity_id) {
                    let actual = entity.properties.get(key);
                    match actual {
                        Some(actual_val) => compare_values(actual_val, value, operator),
                        None => false,
                    }
                } else {
                    false
                }
            }
        }
    }

    /// Get entity by ID.
    pub fn get_entity(&self, id: &str) -> Option<&WorldEntity> {
        self.entities.get(id)
    }

    /// Find all entities of a given kind.
    pub fn entities_by_kind(&self, kind: &EntityKind) -> Vec<&WorldEntity> {
        self.entities.values().filter(|e| e.kind == *kind).collect()
    }

    /// Serialize the current state to a JSON value.
    pub fn snapshot(&self) -> Value {
        serde_json::to_value(self).unwrap_or_default()
    }

    /// Build a human-readable narrative summary of the current world state,
    /// suitable for inclusion in LLM prompts.
    pub fn narrative_summary(&self) -> String {
        let mut parts: Vec<String> = Vec::new();

        // Characters
        let chars: Vec<&WorldEntity> = self.entities_by_kind(&EntityKind::Character);
        if !chars.is_empty() {
            let char_list: Vec<String> = chars
                .iter()
                .map(|c| {
                    let notes = if c.state_flags.is_empty() {
                        String::new()
                    } else {
                        format!(
                            " [{}]",
                            c.state_flags
                                .iter()
                                .cloned()
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    };
                    format!("{}{}", c.name, notes)
                })
                .collect();
            parts.push(format!("Characters: {}", char_list.join(", ")));
        }

        // Locations
        let locs: Vec<&WorldEntity> = self.entities_by_kind(&EntityKind::Location);
        if !locs.is_empty() {
            let loc_list: Vec<String> = locs.iter().map(|l| l.name.clone()).collect();
            parts.push(format!("Locations: {}", loc_list.join(", ")));
        }

        // Items
        let items: Vec<&WorldEntity> = self.entities_by_kind(&EntityKind::Item);
        if !items.is_empty() {
            let item_list: Vec<String> = items.iter().map(|i| i.name.clone()).collect();
            parts.push(format!("Items: {}", item_list.join(", ")));
        }

        // Global flags
        if !self.global_flags.is_empty() {
            parts.push(format!(
                "Flags: {}",
                self.global_flags
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }

        // Key relationships
        let rels: Vec<String> = self
            .entities
            .values()
            .flat_map(|e| {
                e.relationships.iter().filter_map(|r| {
                    self.entities.get(&r.target_id).map(|target| {
                        format!(
                            "{} → {} ({}, {:.1})",
                            e.name, target.name, r.relation_type, r.strength
                        )
                    })
                })
            })
            .collect();
        if !rels.is_empty() {
            parts.push(format!("Relationships: {}", rels.join("; ")));
        }

        parts.join("\n")
    }
}

impl Default for WorldState {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

fn apply_comparison(actual: i32, expected: i32, op: &Comparison) -> bool {
    match op {
        Comparison::Eq => actual == expected,
        Comparison::Neq => actual != expected,
        Comparison::Gt => actual > expected,
        Comparison::Lt => actual < expected,
        Comparison::Gte => actual >= expected,
        Comparison::Lte => actual <= expected,
    }
}

fn compare_values(actual: &Value, expected: &Value, op: &Comparison) -> bool {
    match (actual, expected) {
        (Value::Number(a), Value::Number(b)) => {
            let a_f = a.as_f64().unwrap_or(0.0);
            let b_f = b.as_f64().unwrap_or(0.0);
            match op {
                Comparison::Eq => (a_f - b_f).abs() < 0.001,
                Comparison::Neq => (a_f - b_f).abs() >= 0.001,
                Comparison::Gt => a_f > b_f,
                Comparison::Lt => a_f < b_f,
                Comparison::Gte => a_f >= b_f,
                Comparison::Lte => a_f <= b_f,
            }
        }
        (Value::String(a), Value::String(b)) => match op {
            Comparison::Eq => a == b,
            Comparison::Neq => a != b,
            _ => false,
        },
        (Value::Bool(a), Value::Bool(b)) => match op {
            Comparison::Eq => a == b,
            Comparison::Neq => a != b,
            _ => false,
        },
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_entity_create_and_query() {
        let mut ws = WorldState::new();
        let entity = WorldEntity {
            id: "alice".into(),
            kind: EntityKind::Character,
            name: "Alice".into(),
            description: "A curious girl".into(),
            properties: HashMap::new(),
            relationships: vec![],
            state_flags: HashSet::new(),
            counters: HashMap::new(),
        };
        ws.apply(WorldEvent::EntityCreated(entity));
        assert!(ws.entities.contains_key("alice"));
        assert_eq!(ws.entities_by_kind(&EntityKind::Character).len(), 1);
    }

    #[test]
    fn test_flag_conditions() {
        let mut ws = WorldState::new();
        ws.apply(WorldEvent::FlagSet {
            entity_id: None,
            flag: "war".into(),
        });
        assert!(ws.evaluate(&Condition::Flag {
            entity_id: None,
            flag: "war".into(),
            present: true
        }));
        assert!(!ws.evaluate(&Condition::Flag {
            entity_id: None,
            flag: "peace".into(),
            present: true
        }));

        ws.apply(WorldEvent::FlagCleared {
            entity_id: None,
            flag: "war".into(),
        });
        assert!(!ws.evaluate(&Condition::Flag {
            entity_id: None,
            flag: "war".into(),
            present: true
        }));
    }

    #[test]
    fn test_counter_conditions() {
        let mut ws = WorldState::new();
        ws.apply(WorldEvent::CounterChanged {
            entity_id: None,
            counter: "score".into(),
            delta: 5,
        });
        assert!(ws.evaluate(&Condition::Counter {
            entity_id: None,
            counter: "score".into(),
            operator: Comparison::Eq,
            value: 5,
        }));
        ws.apply(WorldEvent::CounterChanged {
            entity_id: None,
            counter: "score".into(),
            delta: -2,
        });
        assert!(ws.evaluate(&Condition::Counter {
            entity_id: None,
            counter: "score".into(),
            operator: Comparison::Gte,
            value: 3,
        }));
    }

    #[test]
    fn test_composite_conditions() {
        let mut ws = WorldState::new();
        ws.apply(WorldEvent::FlagSet {
            entity_id: None,
            flag: "has_key".into(),
        });
        ws.apply(WorldEvent::FlagSet {
            entity_id: None,
            flag: "door_open".into(),
        });

        // AND
        assert!(ws.evaluate(&Condition::And(vec![
            Condition::Flag {
                entity_id: None,
                flag: "has_key".into(),
                present: true
            },
            Condition::Flag {
                entity_id: None,
                flag: "door_open".into(),
                present: true
            },
        ])));

        // OR (one false, one true)
        assert!(ws.evaluate(&Condition::Or(vec![
            Condition::Flag {
                entity_id: None,
                flag: "has_key".into(),
                present: true
            },
            Condition::Flag {
                entity_id: None,
                flag: "monster_alive".into(),
                present: true
            },
        ])));

        // NOT
        assert!(ws.evaluate(&Condition::Not(Box::new(
            Condition::Flag {
                entity_id: None,
                flag: "monster_alive".into(),
                present: true
            }
        ))));
    }

    #[test]
    fn test_entity_relationships() {
        let mut ws = WorldState::new();
        let alice = WorldEntity {
            id: "alice".into(),
            kind: EntityKind::Character,
            name: "Alice".into(),
            description: String::new(),
            properties: HashMap::new(),
            relationships: vec![],
            state_flags: HashSet::new(),
            counters: HashMap::new(),
        };
        let bob = WorldEntity {
            id: "bob".into(),
            kind: EntityKind::Character,
            name: "Bob".into(),
            description: String::new(),
            properties: HashMap::new(),
            relationships: vec![],
            state_flags: HashSet::new(),
            counters: HashMap::new(),
        };
        ws.apply(WorldEvent::EntityCreated(alice));
        ws.apply(WorldEvent::EntityCreated(bob));

        ws.apply(WorldEvent::RelationshipSet {
            source_id: "alice".into(),
            target_id: "bob".into(),
            relation_type: "friend".into(),
            strength: 0.8,
        });

        assert!(ws.evaluate(&Condition::Relationship {
            source_id: "alice".into(),
            target_id: "bob".into(),
            relation_type: Some("friend".into()),
            min_strength: 0.5,
        }));

        // Wrong type should fail
        assert!(!ws.evaluate(&Condition::Relationship {
            source_id: "alice".into(),
            target_id: "bob".into(),
            relation_type: Some("enemy".into()),
            min_strength: 0.5,
        }));
    }

    #[test]
    fn test_narrative_summary() {
        let mut ws = WorldState::new();
        ws.apply(WorldEvent::EntityCreated(WorldEntity {
            id: "hero".into(),
            kind: EntityKind::Character,
            name: "Hero".into(),
            description: "The protagonist".into(),
            properties: HashMap::new(),
            relationships: vec![],
            state_flags: HashSet::new(),
            counters: HashMap::new(),
        }));
        ws.apply(WorldEvent::EntityCreated(WorldEntity {
            id: "forest".into(),
            kind: EntityKind::Location,
            name: "Dark Forest".into(),
            description: "A spooky forest".into(),
            properties: HashMap::new(),
            relationships: vec![],
            state_flags: HashSet::new(),
            counters: HashMap::new(),
        }));
        ws.apply(WorldEvent::FlagSet {
            entity_id: None,
            flag: "night".into(),
        });

        let summary = ws.narrative_summary();
        assert!(summary.contains("Hero"));
        assert!(summary.contains("Dark Forest"));
        assert!(summary.contains("night"));
    }

    #[test]
    fn test_entity_update() {
        let mut ws = WorldState::new();
        ws.apply(WorldEvent::EntityCreated(WorldEntity {
            id: "alice".into(),
            kind: EntityKind::Character,
            name: "Alice".into(),
            description: String::new(),
            properties: HashMap::from([("age".into(), json!(10))]),
            relationships: vec![],
            state_flags: HashSet::new(),
            counters: HashMap::new(),
        }));

        ws.apply(WorldEvent::EntityUpdated {
            id: "alice".into(),
            changes: HashMap::from([("age".into(), json!(11))]),
        });

        assert_eq!(
            ws.entities
                .get("alice")
                .unwrap()
                .properties
                .get("age")
                .unwrap(),
            &json!(11)
        );
    }

    #[test]
    fn test_event_log() {
        let mut ws = WorldState::new();
        assert_eq!(ws.event_log.len(), 0);
        ws.apply(WorldEvent::MetaSet {
            key: "title".into(),
            value: json!("My Story"),
        });
        assert_eq!(ws.event_log.len(), 1);
    }

    #[test]
    fn test_snapshot_roundtrip() {
        let mut ws = WorldState::new();
        ws.apply(WorldEvent::EntityCreated(WorldEntity {
            id: "alice".into(),
            kind: EntityKind::Character,
            name: "Alice".into(),
            description: "A girl".into(),
            properties: HashMap::new(),
            relationships: vec![],
            state_flags: HashSet::from(["brave".into()]),
            counters: HashMap::from([("level".into(), 5)]),
        }));

        let snapshot = ws.snapshot();
        let restored: WorldState = serde_json::from_value(snapshot).unwrap();
        assert!(restored.entities.contains_key("alice"));
        assert!(restored
            .entities
            .get("alice")
            .unwrap()
            .state_flags
            .contains("brave"));
        assert_eq!(
            restored.entities.get("alice").unwrap().counters.get("level"),
            Some(&5)
        );
    }

    #[test]
    fn test_entity_entity_kind_condition() {
        let mut ws = WorldState::new();
        ws.apply(WorldEvent::EntityCreated(WorldEntity {
            id: "forest".into(),
            kind: EntityKind::Location,
            name: "Forest".into(),
            description: String::new(),
            properties: HashMap::new(),
            relationships: vec![],
            state_flags: HashSet::new(),
            counters: HashMap::new(),
        }));

        assert!(ws.evaluate(&Condition::EntityKind {
            id: "forest".into(),
            kind: EntityKind::Location,
        }));
        assert!(!ws.evaluate(&Condition::EntityKind {
            id: "forest".into(),
            kind: EntityKind::Character,
        }));
    }

    #[test]
    fn test_entity_property_condition() {
        let mut ws = WorldState::new();
        ws.apply(WorldEvent::EntityCreated(WorldEntity {
            id: "chest".into(),
            kind: EntityKind::Item,
            name: "Chest".into(),
            description: String::new(),
            properties: HashMap::from([("locked".into(), json!(true))]),
            relationships: vec![],
            state_flags: HashSet::new(),
            counters: HashMap::new(),
        }));

        assert!(ws.evaluate(&Condition::Property {
            entity_id: "chest".into(),
            key: "locked".into(),
            operator: Comparison::Eq,
            value: json!(true),
        }));
    }
}
