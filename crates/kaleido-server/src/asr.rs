//! ASR worker — persistent faster-whisper subprocess (stdio RPC).
//!
//! The server holds ONE `.venv-asr/bin/python scripts/asr_worker.py` child that
//! lazy-loads the whisper model on its first request and reuses it afterwards.
//! This gives the "first request downloads+loads, later ones reuse" behavior the
//! P3 duplex plan asks for, without reloading a ~460MB model per turn.

use std::path::PathBuf;
use std::process::Stdio;

use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};

/// ponytail: single global worker/global lock — all ASR requests serialize.
/// Upgrade to a pool keyed by model_size if concurrent transcription throughput matters.
pub(crate) struct AsrWorker {
    _child: Child,
    stdin: ChildStdin,
    stdout: BufReader<tokio::process::ChildStdout>,
}

impl AsrWorker {
    fn py_paths() -> Vec<PathBuf> {
        // Deployed service runs with WorkingDirectory = project root; also try
        // compile-time crate root (crates/kaleido-server) → project root fallback.
        let mut v = Vec::new();
        if let Ok(wd) = std::env::current_dir() {
            v.push(wd.join(".venv-asr/bin/python"));
            v.push(wd.join("scripts/asr_worker.py"));
        }
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .map(|p| p.to_path_buf());
        if let Some(proj) = p {
            v.push(proj.join(".venv-asr/bin/python"));
            v.push(proj.join("scripts/asr_worker.py"));
        }
        v
    }

    pub(crate) fn spawn() -> std::io::Result<AsrWorker> {
        let paths = Self::py_paths();
        let py = paths.first().expect("asr venv python candidates").to_path_buf();
        let script = paths.get(1).expect("asr worker script candidates").to_path_buf();
        let mut child = Command::new(&py)
            .arg(&script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit()) // worker progress → server journal
            .spawn()?;
        let stdin = child.stdin.take().expect("asr worker stdin");
        let stdout = BufReader::new(child.stdout.take().expect("asr worker stdout"));
        tracing::info!(path = %script.display(), "spawned ASR worker ({})", py.display());
        Ok(AsrWorker { _child: child, stdin, stdout })
    }

    /// Send one transcription request, read one JSON response line.
    pub(crate) async fn transcribe(&mut self, wav: &str, model_size: &str) -> Result<String, String> {
        let req = json!({ "path": wav, "model_size": model_size });
        let mut line = serde_json::to_string(&req).map_err(|e| e.to_string())?;
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| format!("asr write failed: {e}"))?;
        self.stdin.flush().await.map_err(|e| format!("asr flush failed: {e}"))?;
        let mut out = String::new();
        let bytes = self
            .stdout
            .read_line(&mut out)
            .await
            .map_err(|e| format!("asr read failed: {e}"))?;
        if bytes == 0 {
            return Err("asr worker exited (see server log)".into());
        }
        let v: serde_json::Value = serde_json::from_str(&out).map_err(|e| format!("asr bad response: {e}"))?;
        if v.get("ok").and_then(|o| o.as_bool()).unwrap_or(false) {
            Ok(v.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string())
        } else {
            Err(v.get("error").and_then(|e| e.as_str()).unwrap_or("asr failed").to_string())
        }
    }
}

/// Global lazy slot: no AppState churn, worker only spins up on the first /asr call.
pub(crate) type AsrSlot = tokio::sync::Mutex<Option<AsrWorker>>;
static ASR_SLOT: std::sync::OnceLock<AsrSlot> = std::sync::OnceLock::new();

pub(crate) fn asr_slot() -> &'static AsrSlot {
    ASR_SLOT.get_or_init(|| tokio::sync::Mutex::new(None))
}