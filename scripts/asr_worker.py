#!/usr/bin/env python3
"""ASR worker — persistent stdio RPC held by kaleido-server.

Protocol: one JSON request per line on stdin, one JSON response per line on stdout.
Request:  {"path": "/tmp/x.wav", "model_size": "small", "language": null}
Response: {"ok": true, "text": "...", "language": "zh"} | {"ok": false, "error": "..."}

Model is loaded once (lazy) and reused across requests; "small" falls back to "base"
if the model files cannot be downloaded/loaded. Run under .venv-asr.

Self-check: `python3 asr_worker.py --self-test <audio>` prints JSON directly (no stdio loop).
"""

import json
import sys
import os

_MODEL = None
_MODEL_SIZE = None


def _load_model(model_size):
    global _MODEL, _MODEL_SIZE
    if _MODEL is not None and _MODEL_SIZE == model_size:
        return _MODEL, None
    # Respect the same proxy the systemd unit uses (model download walks HF).
    if os.environ.get("HTTPS_PROXY") or os.environ.get("https_proxy"):
        os.environ.setdefault("HF_HUB_DOWNLOAD_TIMEOUT", "120")
    try:
        from faster_whisper import WhisperModel
    except Exception as e:  # pragma: no cover
        return None, f"faster-whisper unavailable: {e}"
    for size in [model_size, "base"] if model_size != "base" else ["base"]:
        try:
            # ponytail: single model slot, upgrade to a per-size cache if multi-size ever used
            _MODEL = WhisperModel(size, device="cpu", compute_type="int8")
            _MODEL_SIZE = size
            return _MODEL, None
        except Exception as e:
            last = str(e)
    return None, f"model load failed (tried {model_size}, base): {last}"


def _normalize_wav(path):
    """ffmpeg: any container (webm/opus/mp3...) → 16k mono WAV.
    PyAV can often decode these directly, but normalizing guarantees faster-whisper
    sees exactly what it expects. Returns (wav_path, is_temp)."""
    try:
        with open(path, "rb") as f:
            magic = f.read(4)
    except Exception:
        magic = b""
    if magic == b"RIFF":
        return path, False
    out = f"/tmp/kaleido_asr_{os.getpid()}_{abs(hash(path))}.wav"
    ok = os.system(f'ffmpeg -y -loglevel error -i "{path}" -ar 16000 -ac 1 -f wav "{out}"') == 0
    if not ok or not os.path.exists(out):
        return None, False
    return out, True


def _transcribe(req):
    path = req.get("path")
    size = req.get("model_size") or "small"
    lang = req.get("language") or None  # None = auto-detect
    if not path or not os.path.exists(path):
        return {"ok": False, "error": f"audio file not found: {path}"}
    wav, is_temp = _normalize_wav(path)
    if wav is None:
        return {"ok": False, "error": "ffmpeg normalize to wav failed"}
    model, err = _load_model(size)
    if err:
        if is_temp:
            try: os.remove(wav)
            except Exception: pass
        return {"ok": False, "error": err}
    try:
        segments, info = model.transcribe(
            wav,
            language=lang,
            beam_size=5,
            vad_filter=True,
            condition_on_previous_text=False,
        )
        text = "".join(s.text for s in segments).strip()
        return {"ok": True, "text": text, "language": info.language}
    except Exception as e:
        return {"ok": False, "error": f"transcribe failed: {e}"}
    finally:
        if is_temp:
            try: os.remove(wav)
            except Exception: pass


def _loop():
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
        except Exception:
            sys.stdout.write(json.dumps({"ok": False, "error": "bad request json"}, ensure_ascii=False) + "\n")
            sys.stdout.flush()
            continue
        # progress hints go to stderr so stdout stays pure JSON (Rust side shows them in logs)
        if _MODEL is None:
            sys.stderr.write(f"[asr] loading {req.get('model_size') or 'small'} for first request...\n")
            sys.stderr.flush()
        sys.stdout.write(json.dumps(_transcribe(req), ensure_ascii=False) + "\n")
        sys.stdout.flush()


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "--self-test":
        # usage: --self-test <audio> [model_size]
        audio = sys.argv[2]
        size = sys.argv[3] if len(sys.argv) > 3 else "small"
        print(json.dumps(_transcribe({"path": audio, "model_size": size}), ensure_ascii=False))
    else:
        _loop()