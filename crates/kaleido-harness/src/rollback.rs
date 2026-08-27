//! P1 rollback: derive a reverse proposal from an applied result.
//!
//! Iterating in reverse over the successfully applied edits, we emit inverse
//! edits:
//! - update -> an `Update` restoring the `before` content/path/reference/
//!   arguments/metadata
//! - delete -> a `Create` re-inserting `before`
//! - create -> a `Delete`
//!
//! The produced proposal carries `rollback_of = Some(target.id)`.

use crate::model::{
    AppliedEdit, ApplyResult, RefAction, RefinementEdit, RefinementKind, RefinementProposal,
};

/// Build the reverse proposal that undoes `result` (which came from `target`).
pub fn rollback_proposal(target: &RefinementProposal, result: &ApplyResult) -> RefinementProposal {
    let mut edits: Vec<RefinementEdit> = Vec::new();

    // Iterate in reverse so preconditions are restored in reverse order.
    for applied in result.applied_edits.iter().rev() {
        if !applied.applied {
            continue;
        }

        let kind = infer_kind(&applied);
        let Some(id) = resolve_id(applied) else {
            continue;
        };

        let reverse_edit = match (&applied.before, &applied.after) {
            // update
            (Some(before), Some(after)) => {
                let _ = after;
                RefinementEdit {
                    action: RefAction::Update,
                    kind,
                    id: Some(id),
                    title: Some(before.title.clone()),
                    content: Some(before.content.clone()),
                    path: Some(before.path.clone()),
                    reference: Some(before.reference.clone()),
                    arguments: Some(before.arguments.clone()),
                    metadata: Some(before.metadata.clone()),
                    reason: Some(format!("rollback of {} ({})", before.id, before.title)),
                }
            }
            // delete that removed an entry -> re-create it
            (Some(before), None) => RefinementEdit {
                action: RefAction::Create,
                kind,
                id: Some(id),
                title: Some(before.title.clone()),
                content: Some(before.content.clone()),
                path: Some(before.path.clone()),
                reference: Some(before.reference.clone()),
                arguments: Some(before.arguments.clone()),
                metadata: Some(before.metadata.clone()),
                reason: Some(format!(
                    "rollback: restore deleted entry {}",
                    before.id
                )),
            },
            // create that added an entry -> delete it
            (None, Some(_after)) => {
                let id_owned = id.clone();
                RefinementEdit {
                    action: RefAction::Delete,
                    kind,
                    id: Some(id_owned),
                    title: None,
                    content: None,
                    path: None,
                    reference: None,
                    arguments: None,
                    metadata: None,
                    reason: Some(format!("rollback: remove created entry {id}")),
                }
            }
            (None, None) => continue,
        };
        edits.push(reverse_edit);
    }

    RefinementProposal {
        id: format!("rollback-{}", target.id),
        edits,
        rationale: Some(format!(
            "rollback of proposal {} ({} applied edits)",
            target.id,
            result.success_count()
        )),
        rollback_of: Some(target.id.clone()),
    }
}

/// Recover the kind from the applied edit record.
fn infer_kind(applied: &AppliedEdit) -> RefinementKind {
    applied
        .after
        .as_ref()
        .or(applied.before.as_ref())
        .map(|e| e.kind)
        .unwrap_or(RefinementKind::Prompt)
}

/// Recover the entry id from the applied edit record, preferring the explicit
/// id field.
fn resolve_id(applied: &AppliedEdit) -> Option<String> {
    if !applied.id.is_empty() {
        return Some(applied.id.clone());
    }
    applied
        .after
        .as_ref()
        .or(applied.before.as_ref())
        .map(|e| e.id.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apply::apply_refinement_proposal;
    use crate::model::{HarnessEntry, HarnessState};

    fn mem_entry(id: &str, content: &str) -> HarnessEntry {
        let mut e = HarnessEntry::new(RefinementKind::Memory, id, "Mem");
        e.content = content.to_string();
        e.path = "notes".to_string();
        e.created_at = "c".to_string();
        e.updated_at = "u".to_string();
        e.version = 1;
        e
    }

    fn state_with_mem() -> HarnessState {
        let mut s = HarnessState::default();
        s.entries
            .entry("memory".into())
            .or_default()
            .insert("m1".into(), mem_entry("m1", "original"));
        s
    }

    fn create_mem() -> RefinementEdit {
        RefinementEdit {
            action: RefAction::Create,
            kind: RefinementKind::Memory,
            id: Some("new_mem".into()),
            title: Some("Mem New".into()),
            content: Some("brand new".into()),
            path: None,
            reference: None,
            arguments: None,
            metadata: None,
            reason: None,
        }
    }

    fn update_mem(content: &str) -> RefinementEdit {
        RefinementEdit {
            action: RefAction::Update,
            kind: RefinementKind::Memory,
            id: Some("m1".into()),
            title: Some("Mem".into()),
            content: Some(content.into()),
            path: None,
            reference: None,
            arguments: None,
            metadata: None,
            reason: None,
        }
    }

    fn delete_mem(id: &str) -> RefinementEdit {
        RefinementEdit {
            action: RefAction::Delete,
            kind: RefinementKind::Memory,
            id: Some(id.into()),
            title: None,
            content: None,
            path: None,
            reference: None,
            arguments: None,
            metadata: None,
            reason: None,
        }
    }

    fn prop(id: &str, edits: Vec<RefinementEdit>) -> RefinementProposal {
        RefinementProposal {
            id: id.into(),
            edits,
            rationale: Some("r".into()),
            rollback_of: None,
        }
    }

    /// Replay `apply` on a live state and return the mutated live state, so we
    /// can verify round-trip consistency.
    fn apply_live(state: &mut HarnessState, p: &RefinementProposal) -> ApplyResult {
        let r = apply_refinement_proposal(state, p, None);
        for ae in &r.applied_edits {
            if !ae.applied {
                continue;
            }
            let kind = ae.after.as_ref().or(ae.before.as_ref()).unwrap().kind;
            let map = state.entries.entry(kind.to_string()).or_default();
            match (&ae.before, &ae.after) {
                (Some(_b), Some(a)) => {
                    map.insert(a.id.clone(), a.clone());
                }
                (None, Some(a)) => {
                    map.insert(a.id.clone(), a.clone());
                }
                (Some(_b), None) => {
                    let key = if ae.id.is_empty() {
                        ae.before.as_ref().unwrap().id.clone()
                    } else {
                        ae.id.clone()
                    };
                    map.remove(&key);
                }
                (None, None) => {}
            }
        }
        r
    }

    #[test]
    fn rollback_update_restores_before() {
        let mut live = state_with_mem();
        let r = apply_live(&mut live, &prop("u1", vec![update_mem("changed")]));
        assert_eq!(r.applied_edits.len(), 1);
        let rb = rollback_proposal(&prop("u1", vec![]), &r);
        assert_eq!(rb.rollback_of.as_deref(), Some("u1"));
        // Rub through apply and confirm content goes back to original.
        let r2 = apply_refinement_proposal(&live, &rb, None);
        assert!(r2.applied_edits[0].applied);
        assert_eq!(r2.applied_edits[0].after.as_ref().unwrap().content, "original");
        // version incremented again from 2 -> 3 (apply increments)
    }

    #[test]
    fn rollback_delete_creates_back() {
        let mut live = state_with_mem();
        let r = apply_live(&mut live, &prop("d1", vec![delete_mem("m1")]));
        assert_eq!(r.applied_edits.len(), 1);
        assert!(r.applied_edits[0].applied);
        // Simulate the delete actually landing: remove from live.
        live.entries.get_mut("memory").unwrap().remove("m1");

        let rb = rollback_proposal(&prop("d1", vec![]), &r);
        let r2 = apply_refinement_proposal(&live, &rb, None);
        assert!(r2.applied_edits[0].applied);
        assert_eq!(r2.applied_edits[0].after.as_ref().unwrap().content, "original");
    }

    #[test]
    fn rollback_create_deletes_back() {
        let mut live = state_with_mem();
        let r = apply_live(&mut live, &prop("c1", vec![create_mem()]));
        assert!(r.applied_edits[0].applied);
        // live now has new_mem inserted by apply_live.
        let rb = rollback_proposal(&prop("c1", vec![]), &r);
        let r2 = apply_refinement_proposal(&live, &rb, None);
        assert!(r2.applied_edits[0].applied);
        assert!(r2.applied_edits[0].after.is_none());
        assert!(r2.applied_edits[0].before.as_ref().unwrap().content == "brand new");
    }

    #[test]
    fn rollback_roundtrip_consistency() {
        // Apply update; rollback; apply rollback; original content restored.
        let mut live = state_with_mem();
        let target = prop("t1", vec![update_mem("v2")]);
        let r1 = apply_live(&mut live, &target);
        let rb = rollback_proposal(&target, &r1);
        let r2 = apply_refinement_proposal(&live, &rb, None);
        let after = r2.applied_edits[0].after.as_ref().unwrap();
        assert_eq!(after.content, "original");
        assert_eq!(after.path, "notes");
        assert_eq!(after.metadata, live.entries["memory"]["m1"].metadata);
    }

    #[test]
    fn rollback_skips_failed_edits() {
        let mut live = state_with_mem();
        // mix: valid update + invalid (ghost delete).
        let p = prop(
            "mix",
            vec![update_mem("ok"), delete_mem("ghost")],
        );
        let r1 = apply_live(&mut live, &p);
        assert_eq!(r1.success_count(), 1);
        let rb = rollback_proposal(&p, &r1);
        assert_eq!(rb.edits.len(), 1);
        assert_eq!(rb.rollback_of.as_deref(), Some("mix"));
    }

    #[test]
    fn rollback_reverse_order() {
        let mut live = state_with_mem();
        let p = prop(
            "two",
            vec![create_mem(), update_mem("seq")],
        );
        let r1 = apply_live(&mut live, &p);
        assert_eq!(r1.success_count(), 2);
        let rb = rollback_proposal(&p, &r1);
        // Reverse iteration: the LAST applied edit (update m1) is undone first,
        // then the earlier create (new_mem).
        assert_eq!(rb.edits.len(), 2);
        assert_eq!(rb.edits[0].id.as_deref(), Some("m1"));
        assert_eq!(rb.edits[0].action, RefAction::Update);
        assert_eq!(rb.edits[0].content.as_deref(), Some("original"));
        assert_eq!(rb.edits[1].id.as_deref(), Some("new_mem"));
        assert_eq!(rb.edits[1].action, RefAction::Delete);
    }

    #[test]
    fn apply_result_happy_path_meta() {
        let mut live = state_with_mem();
        let r1 = apply_live(&mut live, &prop("x", vec![update_mem("again")]));
        let rb = rollback_proposal(&prop("x", vec![]), &r1);
        assert!(rb.rationale.is_some());
    }
}