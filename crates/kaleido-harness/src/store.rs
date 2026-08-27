//! P0 store layer: load / save / merge / history / atomic write.
//!
//! File layout (aligned with the existing `data_root` convention):
//! ```text
//! data_root/harness/          # global harness
//!   harness_state.json        # {schema, entries, refinements}
//!   refinements.jsonl         # global refine history (appended)
//! data_root/sessions/<sid>/harness/   # session-local (optional, signatures only)
//! ```

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use crate::model::{EntriesByKind, HarnessState, RefinementEvent};

/// Directory name of the global harness under `data_root`.
pub const HARNESS_DIR: &str = "harness";
/// File name of the persisted state inside `HARNESS_DIR`.
pub const STATE_FILE: &str = "harness_state.json";
/// File name of the appended global refinement history inside `HARNESS_DIR`.
pub const HISTORY_FILE: &str = "refinements.jsonl";

fn state_path(dir: &Path) -> PathBuf {
    dir.join(STATE_FILE)
}

fn history_path(dir: &Path) -> PathBuf {
    dir.join(HISTORY_FILE)
}

/// Load the harness state from `dir` (i.e. `dir/Make` `harness_state.json`).
///
/// Fault-tolerant: missing file, corrupt JSON, or non-object JSON all fall back
/// to `HarnessState::default()`. Never panics. The failure reason is logged via
/// `eprintln!`.
pub fn load(dir: &Path) -> HarnessState {
    let path = state_path(dir);
    let raw = match fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return HarnessState::default();
        }
        Err(e) => {
            eprintln!(
                "kaleido-harness: cannot read {}: {e}",
                path.display()
            );
            return HarnessState::default();
        }
    };
    match serde_json::from_str::<HarnessState>(&raw) {
        Ok(state) => state,
        Err(e) => {
            eprintln!(
                "kaleido-harness: corrupt harness state {} (falling back to default): {e}",
                path.display()
            );
            HarnessState::default()
        }
    }
}

/// Atomically write `state` to `dir/harness_state.json`.
///
/// Writes to a temp file `harness_state.json.<pid>.<uuid>.tmp` and then
/// `fs::rename`s it over the real file (atomic on POSIX). The final file mode
/// is `0o600`. Returns the final path.
pub fn save(dir: &Path, state: &HarnessState) -> std::io::Result<PathBuf> {
    create_dir_all(dir)?;
    let final_path = state_path(dir);
    let tmp_path = dir.join(format!(
        "{STATE_FILE}.{}.{}.tmp",
        std::process::id(),
        unique_token()
    ));

    let serialized = serde_json::to_vec_pretty(state)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    write_file_0600(&tmp_path, &serialized)?;
    // Rename over destination; atomic on the same filesystem.
    fs::rename(&tmp_path, &final_path)?;
    // Best-effort chmod if rename involved a fresh inode.
    set_mode_0600(&final_path);
    Ok(final_path)
}

/// Merge a local harness state over a global one.
///
/// - Any local entry whose `id` collides with a global entry of the same kind
///   gets a `local:` prefix on its id.
/// - The resulting `schema` is `max(global.schema, local.schema)`.
pub fn merge(global: &HarnessState, local: &HarnessState) -> HarnessState {
    let mut merged = HarnessState {
        schema: global.schema.max(local.schema),
        entries: EntriesByKind::new(),
        refinements: global.refinements.clone(),
        guidances: global.guidances.clone(),
    };

    // Global entries first (keep them un-prefixed).
    for (kind, map) in &global.entries {
        let out = merged.entries.entry(kind.clone()).or_default();
        for (id, entry) in map {
            out.insert(id.clone(), entry.clone());
        }
    }

    // Local entries: prefix any colliding id with `local:`.
    for (kind, map) in &local.entries {
        let out = merged.entries.entry(kind.clone()).or_default();
        for (id, entry) in map {
            let effective_id = if out.contains_key(id) {
                format!("local:{id}")
            } else {
                id.clone()
            };
            let mut e = entry.clone();
            if effective_id != *id {
                e.id = effective_id.clone();
            }
            out.insert(effective_id, e);
        }
    }

    merged
}

/// Append one JSON line to `dir/refinements.jsonl`; creates dirs/file if needed.
pub fn append_global_refinement(dir: &Path, event: &RefinementEvent) -> std::io::Result<()> {
    create_dir_all(dir)?;
    let path = history_path(dir);
    let mut line = serde_json::to_vec(event)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    line.push(b'\n');
    // Append in a way that tolerates the file not existing yet.
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    f.write_all(&line)?;
    Ok(())
}

/// Read every parsible event from `dir/refinements.jsonl`; bad lines are skipped.
pub fn load_global_history(dir: &Path) -> Vec<RefinementEvent> {
    let path = history_path(dir);
    let file = match fs::File::open(&path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    for (idx, line) in BufReader::new(file).lines().enumerate() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!(
                    "kaleido-harness: skipping bad history line {idx} in {}: {e}",
                    path.display()
                );
                continue;
            }
        };
        match serde_json::from_str::<RefinementEvent>(&line) {
            Ok(ev) => out.push(ev),
            Err(e) => {
                eprintln!(
                    "kaleido-harness: skipping unparsable history line {idx} in {}: {e}",
                    path.display()
                );
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn create_dir_all(dir: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dir)
}

/// Unique token for temp-file names: nanos + a small random component.
fn unique_token() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    // Cheap non-crypto randomness, no new dependency required.
    let rnd = nanos % 1_000_003 + (std::process::id() as u128) * 97;
    format!("{nanos}-{rnd}")
}

fn write_file_0600(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    fs::write(path, bytes)?;
    set_mode_0600(path);
    Ok(())
}

#[cfg(unix)]
fn set_mode_0600(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn set_mode_0600(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{EntriesByKind, HarnessEntry, HarnessScope, RefinementKind};

    fn tmp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "kaleido-harness-test-{tag}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn entry(kind: RefinementKind, id: &str, title: &str) -> HarnessEntry {
        HarnessEntry::new(kind, id, title)
    }

    #[test]
    fn load_empty_dir_returns_default() {
        let d = tmp_dir("load-empty");
        let s = load(&d);
        assert_eq!(s.schema, 0);
        assert!(s.entries.is_empty());
        assert!(s.refinements.is_empty());
        // Dir is untouched.
        assert!(!state_path(&d).exists());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn load_corrupt_json_returns_default_no_panic() {
        let d = tmp_dir("load-corrupt");
        fs::write(state_path(&d), "not json {{{").unwrap();
        let s = load(&d);
        assert_eq!(s.schema, 0);
        assert!(s.entries.is_empty());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn load_non_object_json_returns_default() {
        let d = tmp_dir("load-nonobj");
        fs::write(state_path(&d), "[1,2,3]").unwrap();
        let s = load(&d);
        assert_eq!(s.schema, 0);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn save_then_load_roundtrip() {
        let d = tmp_dir("save-roundtrip");
        let mut state = HarnessState {
            schema: 1,
            ..HarnessState::default()
        };
        state
            .entries
            .entry("prompt".into())
            .or_default()
            .insert("p1".into(), entry(RefinementKind::Prompt, "p1", "Sys"));
        state
            .entries
            .entry("skill".into())
            .or_default()
            .insert("s1".into(), entry(RefinementKind::Skill, "s1", "Skill"));

        let path = save(&d, &state).unwrap();
        assert_eq!(path.as_path(), state_path(&d));

        let loaded = load(&d);
        assert_eq!(loaded.schema, 1);
        assert_eq!(
            loaded.entries["prompt"]["p1"].title,
            "Sys"
        );
        assert_eq!(
            loaded.entries["skill"]["s1"].title,
            "Skill"
        );
        // serialized equality: metadata default is an object; after roundtrip
        // both sides deserialize to the same empty map.
        assert_eq!(serde_json::to_value(&loaded).unwrap(), serde_json::to_value(&state).unwrap());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn merge_conflicts_get_local_prefix_and_schema_max() {
        let mut global = HarnessState::default();
        global.schema = 1;
        global
            .entries
            .entry("prompt".into())
            .or_default()
            .insert("shared".into(), entry(RefinementKind::Prompt, "shared", "G"));
        global
            .entries
            .entry("skill".into())
            .or_default()
            .insert("only_global".into(), entry(RefinementKind::Skill, "only_global", ""));

        let mut local = HarnessState::default();
        local.schema = 2;
        local
            .entries
            .entry("prompt".into())
            .or_default()
            .insert("shared".into(), entry(RefinementKind::Prompt, "shared", "L"));
        local
            .entries
            .entry("prompt".into())
            .or_default()
            .insert("only_local".into(), entry(RefinementKind::Prompt, "only_local", "x"));

        let merged = merge(&global, &local);
        assert_eq!(merged.schema, 2);
        // Global entry keeps its id.
        assert!(merged.entries["prompt"].contains_key("shared"));
        assert_eq!(merged.entries["prompt"]["shared"].title, "G");
        // Local conflict becomes local:shared and carries local content.
        assert!(merged.entries["prompt"].contains_key("local:shared"));
        assert_eq!(merged.entries["prompt"]["local:shared"].title, "L");
        assert_eq!(merged.entries["prompt"]["local:shared"].id, "local:shared");
        // Non-conflicting local id stays untouched.
        assert!(merged.entries["prompt"].contains_key("only_local"));
        assert!(merged.entries["skill"].contains_key("only_global"));
    }

    #[test]
    fn append_and_load_history_roundtrip() {
        let d = tmp_dir("history");
        let ev = RefinementEvent {
            id: "refine_123".into(),
            trigger: "summary".into(),
            changes: serde_json::json!([{"action": "create"}]),
            evidence: serde_json::json!({"rationale": "test"}),
            outcome: "applied".into(),
            evaluation: None,
            created_at: "now".into(),
        };
        append_global_refinement(&d, &ev).unwrap();
        append_global_refinement(&d, &ev).unwrap();
        let hist = load_global_history(&d);
        assert_eq!(hist.len(), 2);
        assert_eq!(hist[0].id, "refine_123");
        assert_eq!(hist[1].outcome, "applied");

        // Corrupt line in the middle is skipped.
        fs::OpenOptions::new()
            .append(true)
            .open(history_path(&d))
            .unwrap()
            .write_all(b"{broken\n")
            .unwrap();
        let hist = load_global_history(&d);
        assert_eq!(hist.len(), 2);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn atomic_write_no_tmp_leftover() {
        let d = tmp_dir("atomic");
        let mut s1 = HarnessState { schema: 1, ..HarnessState::default() };
        s1.entries
            .entry("memory".into())
            .or_default()
            .insert("m1".into(), entry(RefinementKind::Memory, "m1", "one"));
        save(&d, &s1).unwrap();

        let mut s2 = HarnessState { schema: 2, ..HarnessState::default() };
        s2.entries
            .entry("memory".into())
            .or_default()
            .insert("m2".into(), entry(RefinementKind::Memory, "m2", "two"));
        save(&d, &s2).unwrap();

        // No *.tmp leftovers.
        let leftovers: Vec<_> = fs::read_dir(&d)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "tmp leftovers: {leftovers:?}");

        let loaded = load(&d);
        assert_eq!(loaded.schema, 2);
        assert!(loaded.entries["memory"].contains_key("m2"));
        assert!(!loaded.entries["memory"].contains_key("m1"));
        let _ = fs::remove_dir_all(&d);
    }

    // Sanity: the type alias exposed publicly is usable.
    #[allow(dead_code)]
    fn _alias_smoke(_e: EntriesByKind) {}
    #[allow(dead_code)]
    fn _scope_smoke(_s: HarnessScope) {}
}