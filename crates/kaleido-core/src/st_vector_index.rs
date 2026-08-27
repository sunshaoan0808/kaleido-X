//! W5: World-Info vector index + pure cosine ranking (ST `vectorized` entries).
//!
//! Storage: `$KALEIDO_DATA/state/wi-vector-index/{world_book_id}.json`
//! Embeddings themselves are produced by the server (`embed_local`); this module
//! only stores vectors, scores queries, and shapes hits for the WI scanner.

use crate::{CoreError, CoreResult, DataRoot, WiEntry};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

fn safe_write(path: &PathBuf, raw: &str) -> CoreResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, raw)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

fn default_model() -> String {
    "BAAI/bge-small-zh-v1.5".into()
}

fn default_threshold() -> f64 {
    0.42
}

fn default_top_k() -> i32 {
    5
}

/// Per-entry embedding row.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VectorIndexEntry {
    pub uid: String,
    pub world: String,
    /// Text that was embedded (keys + content).
    #[serde(default)]
    pub text: String,
    /// FNV-ish content hash for stale detection (not cryptographic).
    #[serde(default)]
    pub text_hash: String,
    pub vector: Vec<f32>,
}

/// On-disk index for one world book (or synthetic key).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VectorIndexFile {
    pub world_book_id: String,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default)]
    pub dim: usize,
    #[serde(default)]
    pub entries: Vec<VectorIndexEntry>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

impl Default for VectorIndexFile {
    fn default() -> Self {
        Self {
            world_book_id: String::new(),
            model: default_model(),
            dim: 0,
            entries: Vec::new(),
            updated_at: None,
        }
    }
}

/// Runtime / settings knobs for vector activation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VectorActivationSettings {
    /// Master switch (default on; no-op when index empty).
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Min cosine similarity to activate (BGE-zh typically ~0.35–0.6).
    #[serde(default = "default_threshold")]
    pub score_threshold: f64,
    /// Max vector hits to inject per scan.
    #[serde(default = "default_top_k")]
    pub top_k: i32,
}

fn default_true() -> bool {
    true
}

impl Default for VectorActivationSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            score_threshold: default_threshold(),
            top_k: default_top_k(),
        }
    }
}

/// Hit passed into the WI scanner (pre-ranked).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VectorHit {
    pub uid: String,
    pub world: String,
    pub score: f64,
}

#[derive(Clone)]
pub struct VectorIndexStore {
    dir: PathBuf,
}

impl VectorIndexStore {
    pub fn new(data: &DataRoot) -> Self {
        let dir = data.root().join("state").join("wi-vector-index");
        let _ = fs::create_dir_all(&dir);
        Self { dir }
    }

    pub fn dir(&self) -> &PathBuf {
        &self.dir
    }

    fn path_for(&self, world_book_id: &str) -> PathBuf {
        let safe: String = world_book_id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let safe = if safe.is_empty() {
            "default".into()
        } else {
            safe
        };
        self.dir.join(format!("{safe}.json"))
    }

    pub fn load(&self, world_book_id: &str) -> VectorIndexFile {
        let path = self.path_for(world_book_id);
        match fs::read_to_string(&path) {
            Ok(raw) => serde_json::from_str(&raw).unwrap_or_else(|_| VectorIndexFile {
                world_book_id: world_book_id.to_string(),
                ..Default::default()
            }),
            Err(_) => VectorIndexFile {
                world_book_id: world_book_id.to_string(),
                ..Default::default()
            },
        }
    }

    pub fn save(&self, mut file: VectorIndexFile) -> CoreResult<VectorIndexFile> {
        if file.world_book_id.trim().is_empty() {
            return Err(CoreError::BadRequest("world_book_id required".into()));
        }
        if file.model.trim().is_empty() {
            file.model = default_model();
        }
        if file.dim == 0 {
            file.dim = file.entries.first().map(|e| e.vector.len()).unwrap_or(0);
        }
        file.updated_at = Some(chrono_like_now());
        let path = self.path_for(&file.world_book_id);
        let raw = serde_json::to_string_pretty(&file)?;
        safe_write(&path, &raw)?;
        Ok(file)
    }

    pub fn delete(&self, world_book_id: &str) -> CoreResult<()> {
        let path = self.path_for(world_book_id);
        if path.exists() {
            fs::remove_file(&path)?;
        }
        Ok(())
    }

    pub fn status(&self, world_book_id: &str) -> Value {
        let f = self.load(world_book_id);
        serde_json::json!({
            "worldBookId": world_book_id,
            "exists": !f.entries.is_empty(),
            "model": f.model,
            "dim": f.dim,
            "entryCount": f.entries.len(),
            "updatedAt": f.updated_at,
        })
    }
}

fn chrono_like_now() -> String {
    // Avoid extra dep in core: RFC3339-ish via system time millis is fine for ops.
    use std::time::{SystemTime, UNIX_EPOCH};
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("{ms}")
}

/// Stable non-crypto hash for change detection.
pub fn text_hash(s: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

/// Text used for embedding a WI entry (keys + comment + content).
pub fn entry_embed_text(e: &WiEntry) -> String {
    let mut parts: Vec<&str> = Vec::new();
    let keys = e.keys.join(", ");
    if !keys.trim().is_empty() {
        parts.push(keys.trim());
    }
    let keys2 = e.keysecondary.join(", ");
    if !keys2.trim().is_empty() {
        parts.push(keys2.trim());
    }
    if !e.comment.trim().is_empty() {
        parts.push(e.comment.trim());
    }
    if !e.content.trim().is_empty() {
        parts.push(e.content.trim());
    }
    let joined = parts.join("\n");
    if joined.chars().count() > 1800 {
        joined.chars().take(1800).collect()
    } else {
        joined
    }
}

/// Cosine similarity; returns 0.0 on empty/mismatch dims.
pub fn vector_cosine_similarity(a: &[f32], b: &[f32]) -> f64 {
    if a.is_empty() || b.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let mut dot = 0.0f64;
    let mut na = 0.0f64;
    let mut nb = 0.0f64;
    for i in 0..a.len() {
        let x = a[i] as f64;
        let y = b[i] as f64;
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na <= 0.0 || nb <= 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

/// Rank index entries against a query vector.
pub fn rank_hits(
    index: &VectorIndexFile,
    query: &[f32],
    settings: &VectorActivationSettings,
) -> Vec<VectorHit> {
    if !settings.enabled || query.is_empty() || index.entries.is_empty() {
        return Vec::new();
    }
    let thr = settings.score_threshold;
    let top_k = settings.top_k.max(0) as usize;
    let mut scored: Vec<VectorHit> = index
        .entries
        .iter()
        .filter_map(|e| {
            if e.vector.is_empty() {
                return None;
            }
            let s = vector_cosine_similarity(query, &e.vector);
            if s >= thr {
                Some(VectorHit {
                    uid: e.uid.clone(),
                    world: e.world.clone(),
                    score: s,
                })
            } else {
                None
            }
        })
        .collect();
    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.uid.cmp(&b.uid))
    });
    if top_k > 0 && scored.len() > top_k {
        scored.truncate(top_k);
    }
    scored
}

/// Build lookup key → score for scanner.
pub fn hits_to_map(hits: &[VectorHit]) -> HashMap<String, f64> {
    let mut m = HashMap::new();
    for h in hits {
        let k = format!("{}.{}", h.world, h.uid);
        m.entry(k)
            .and_modify(|v| {
                if h.score > *v {
                    *v = h.score;
                }
            })
            .or_insert(h.score);
    }
    m
}

/// Merge multiple world-book indices' hits (dedupe by world.uid, keep max score).
pub fn merge_hit_lists(lists: &[Vec<VectorHit>], top_k: i32) -> Vec<VectorHit> {
    let mut best: HashMap<String, VectorHit> = HashMap::new();
    for list in lists {
        for h in list {
            let k = format!("{}.{}", h.world, h.uid);
            best.entry(k)
                .and_modify(|cur| {
                    if h.score > cur.score {
                        *cur = h.clone();
                    }
                })
                .or_insert_with(|| h.clone());
        }
    }
    let mut out: Vec<VectorHit> = best.into_values().collect();
    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.uid.cmp(&b.uid))
    });
    let k = top_k.max(0) as usize;
    if k > 0 && out.len() > k {
        out.truncate(k);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_identical() {
        let a = vec![1.0f32, 0.0, 0.0];
        assert!((vector_cosine_similarity(&a, &a) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn rank_threshold() {
        let index = VectorIndexFile {
            world_book_id: "w".into(),
            model: default_model(),
            dim: 2,
            entries: vec![
                VectorIndexEntry {
                    uid: "1".into(),
                    world: "w".into(),
                    text: "a".into(),
                    text_hash: "x".into(),
                    vector: vec![1.0, 0.0],
                },
                VectorIndexEntry {
                    uid: "2".into(),
                    world: "w".into(),
                    text: "b".into(),
                    text_hash: "y".into(),
                    vector: vec![0.0, 1.0],
                },
            ],
            updated_at: None,
        };
        let settings = VectorActivationSettings {
            enabled: true,
            score_threshold: 0.9,
            top_k: 5,
        };
        let hits = rank_hits(&index, &[1.0, 0.0], &settings);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].uid, "1");
    }
}
