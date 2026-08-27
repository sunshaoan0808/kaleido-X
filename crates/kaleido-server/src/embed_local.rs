//! In-process embedding via `fastembed` (BGE-small-zh-v1.5, 512-d).
//!
//! Prefer local ONNX under `$KALEIDO_EMBED_CACHE` / python fastembed cache so we do not
//! depend on HuggingFace at runtime. Falls back to remote `EMBEDDING_BASE_URL` via
//! `llm_stream::get_embedding` when local init fails.
//!
//! Env:
//! - `KALEIDO_EMBED_INLINE` — default on; `0`/`false` disables local
//! - `KALEIDO_EMBED_CACHE` — model dir (default `$KALEIDO_DATA/embed-cache` or `./data/embed-cache`)
//! - `KALEIDO_EMBED_MODEL_DIR` — explicit snapshot dir with model_optimized.onnx + tokenizer*

use fastembed::{
    InitOptionsUserDefined, Pooling, TextEmbedding, TokenizerFiles, UserDefinedEmbeddingModel,
};
use parking_lot::Mutex;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Instant;

static ENGINE: OnceLock<Mutex<Option<LocalEmbedEngine>>> = OnceLock::new();

pub struct LocalEmbedEngine {
    model: TextEmbedding,
    pub model_name: String,
    pub dim: usize,
}

fn cache_dir() -> PathBuf {
    if let Ok(p) = std::env::var("KALEIDO_EMBED_CACHE") {
        let pb = PathBuf::from(p);
        let _ = std::fs::create_dir_all(&pb);
        return pb;
    }
    if let Ok(data) = std::env::var("KALEIDO_DATA") {
        let pb = PathBuf::from(data).join("embed-cache");
        let _ = std::fs::create_dir_all(&pb);
        return pb;
    }
    let pb = PathBuf::from("data/embed-cache");
    let _ = std::fs::create_dir_all(&pb);
    pb
}

pub fn inline_enabled() -> bool {
    match std::env::var("KALEIDO_EMBED_INLINE") {
        Ok(v) => {
            let v = v.trim().to_ascii_lowercase();
            !(v == "0" || v == "false" || v == "off" || v == "no")
        }
        Err(_) => true,
    }
}

fn engine_slot() -> &'static Mutex<Option<LocalEmbedEngine>> {
    ENGINE.get_or_init(|| Mutex::new(None))
}

/// Candidate dirs that already hold Qdrant/python fastembed BGE-zh files.
fn model_snapshot_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(p) = std::env::var("KALEIDO_EMBED_MODEL_DIR") {
        out.push(PathBuf::from(p));
    }
    let cache = cache_dir();
    // Prefer explicit snapshot under our cache
    out.push(cache.join("bge-small-zh-v1.5"));
    out.push(cache.join("models--Qdrant--bge-small-zh-v1.5/snapshots/46fbe35fd4374a00fee7de77dfddaeb6dd6a2c59"));
    // python /tmp fastembed cache (shared host)
    out.push(PathBuf::from(
        "/tmp/fastembed_cache/models--Qdrant--bge-small-zh-v1.5/snapshots/46fbe35fd4374a00fee7de77dfddaeb6dd6a2c59",
    ));
    out.push(PathBuf::from(
        "/tmp/fastembed_cache/models--Qdrant--bge-small-zh-v1.5",
    ));
    out
}

fn resolve_file(dir: &Path, names: &[&str]) -> Option<PathBuf> {
    for n in names {
        let p = dir.join(n);
        if p.is_file() {
            return Some(p);
        }
        // also one-level nested onnx/
        let p2 = dir.join("onnx").join(n);
        if p2.is_file() {
            return Some(p2);
        }
    }
    // walk one level of snapshots/*
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                for n in names {
                    let c = p.join(n);
                    if c.is_file() {
                        return Some(c);
                    }
                }
            }
        }
    }
    None
}

fn load_from_dir(dir: &Path) -> Result<TextEmbedding, String> {
    let onnx = resolve_file(
        dir,
        &[
            "model_optimized.onnx",
            "model.onnx",
            "onnx/model_optimized.onnx",
            "onnx/model.onnx",
        ],
    )
    .ok_or_else(|| format!("no onnx under {}", dir.display()))?;
    let tokenizer = resolve_file(dir, &["tokenizer.json"])
        .ok_or_else(|| format!("no tokenizer.json under {}", dir.display()))?;
    let config = resolve_file(dir, &["config.json"])
        .ok_or_else(|| format!("no config.json under {}", dir.display()))?;
    let special = resolve_file(dir, &["special_tokens_map.json"]).unwrap_or_else(|| config.clone());
    let tok_cfg =
        resolve_file(dir, &["tokenizer_config.json"]).unwrap_or_else(|| config.clone());

    let onnx_bytes = std::fs::read(&onnx).map_err(|e| format!("read {}: {e}", onnx.display()))?;
    let tokenizer_files = TokenizerFiles {
        tokenizer_file: std::fs::read(&tokenizer)
            .map_err(|e| format!("read {}: {e}", tokenizer.display()))?,
        config_file: std::fs::read(&config)
            .map_err(|e| format!("read {}: {e}", config.display()))?,
        special_tokens_map_file: std::fs::read(&special)
            .map_err(|e| format!("read {}: {e}", special.display()))?,
        tokenizer_config_file: std::fs::read(&tok_cfg)
            .map_err(|e| format!("read {}: {e}", tok_cfg.display()))?,
    };

    let mut ud = UserDefinedEmbeddingModel::new(onnx_bytes, tokenizer_files);
    // BGE uses CLS pooling
    ud.pooling = Some(Pooling::Cls);

    let opts = InitOptionsUserDefined::new().with_max_length(512);
    TextEmbedding::try_new_from_user_defined(ud, opts)
        .map_err(|e| format!("user-defined init from {}: {e}", dir.display()))
}

/// Lazy-init local model. Returns error string on failure (caller may fall back to HTTP).
pub fn ensure_local() -> Result<(), String> {
    if !inline_enabled() {
        return Err("inline embed disabled (KALEIDO_EMBED_INLINE=0)".into());
    }
    let mut slot = engine_slot().lock();
    if slot.is_some() {
        return Ok(());
    }
    let t0 = Instant::now();
    let mut last_err = String::from("no model dir found");
    let mut model: Option<TextEmbedding> = None;
    let mut used = PathBuf::new();
    for cand in model_snapshot_candidates() {
        if !cand.exists() {
            continue;
        }
        tracing::info!(dir=%cand.display(), "embed_local: trying snapshot…");
        match load_from_dir(&cand) {
            Ok(m) => {
                model = Some(m);
                used = cand;
                break;
            }
            Err(e) => {
                tracing::warn!(dir=%cand.display(), error=%e, "embed_local: candidate failed");
                last_err = e;
            }
        }
    }
    let mut model = model.ok_or(last_err)?;

    let probe = model
        .embed(vec!["ping".to_string()], None)
        .map_err(|e| format!("fastembed probe: {e}"))?;
    let dim = probe.first().map(|v| v.len()).unwrap_or(0);
    if dim == 0 {
        return Err("fastembed probe returned empty vector".into());
    }
    tracing::info!(
        dim,
        path = %used.display(),
        elapsed_ms = t0.elapsed().as_millis() as u64,
        "embed_local: ready"
    );
    *slot = Some(LocalEmbedEngine {
        model,
        model_name: "BAAI/bge-small-zh-v1.5".into(),
        dim,
    });
    Ok(())
}

pub fn status() -> serde_json::Value {
    let enabled = inline_enabled();
    let slot = engine_slot().lock();
    match slot.as_ref() {
        Some(e) => serde_json::json!({
            "enabled": enabled,
            "ready": true,
            "backend": "fastembed",
            "model": e.model_name,
            "dim": e.dim,
            "cache": cache_dir().display().to_string(),
        }),
        None => serde_json::json!({
            "enabled": enabled,
            "ready": false,
            "backend": if enabled { "fastembed" } else { "remote" },
            "model": "BAAI/bge-small-zh-v1.5",
            "cache": cache_dir().display().to_string(),
        }),
    }
}

/// Embed one string with the in-process model.
pub fn embed_one(input: &str) -> Result<Vec<f32>, String> {
    ensure_local()?;
    let mut slot = engine_slot().lock();
    let eng = slot
        .as_mut()
        .ok_or_else(|| "embed_local not initialized".to_string())?;
    let text = if input.trim().is_empty() {
        " ".to_string()
    } else {
        let t = input.trim();
        if t.chars().count() > 2000 {
            t.chars().take(2000).collect()
        } else {
            t.to_string()
        }
    };
    let out = eng
        .model
        .embed(vec![text], None)
        .map_err(|e| format!("fastembed embed: {e}"))?;
    out.into_iter()
        .next()
        .ok_or_else(|| "fastembed returned no vectors".into())
}

/// Embed many texts (batch).
pub fn embed_many(inputs: &[String]) -> Result<Vec<Vec<f32>>, String> {
    ensure_local()?;
    let mut slot = engine_slot().lock();
    let eng = slot
        .as_mut()
        .ok_or_else(|| "embed_local not initialized".to_string())?;
    if inputs.is_empty() {
        return Ok(vec![]);
    }
    let batch: Vec<String> = inputs
        .iter()
        .map(|s| {
            let t = s.trim();
            if t.is_empty() {
                " ".into()
            } else if t.chars().count() > 2000 {
                t.chars().take(2000).collect()
            } else {
                t.to_string()
            }
        })
        .collect();
    eng.model
        .embed(batch, None)
        .map_err(|e| format!("fastembed embed batch: {e}"))
}
