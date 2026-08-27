//! 生图 + TTS 工具端点（整合本机多渠道）。
//!
//! 生图渠道：
//! - `uniapi`（默认）→ cogview-4 via nyx-proxy :18998（免费，已实测 4 图秒回）
//! - `cf-manager` → flux-1-schnell via :4001（预留；需 cf-manager 加 /v1/images/generations 路由）
//!
//! TTS 引擎：
//! - `edge`（默认）→ 本机 edge-tts CLI（免费稳定，已实测 25KB mp3）
//! - `mimo` → 小米 MimoAI（预留；cn 节点网络待优化）

use crate::error_codes::*;
use axum::extract::{Multipart, State};
use axum::response::{IntoResponse, Response, Json};
use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageRequest {
    pub prompt: String,
    #[serde(default)]
    pub channel: Option<String>,
    /// U10: uniapi 尺寸（cogview-4 支持 1024x1024 / 768x1344 / 864x1152 / 1344x768 / 1152x864）。
    /// 缺省保持 1024x1024，兼容旧调用方。
    #[serde(default)]
    pub size: Option<String>,
    /// U10: grok2api 宽高比（grok-imagine 必需参数；如 "3:4" / "4:3" / "16:9"）。
    /// 缺省保持 "1:1"，兼容旧调用方。
    #[serde(default)]
    pub aspect_ratio: Option<String>,
}

/// U10: 一次生图请求的原始产物（url / b64 依渠道而定，可能同有；grok2api 已做 :8000→:8020 端口重写）。
pub(crate) struct ImageFetch {
    pub url: Option<String>,
    pub b64: Option<String>,
    pub channel: String,
}

/// U10: 生图核心（供直出端点与图像管线消费模块共用）。
/// 失败返回可读错误字符串（不抛异常、不阻塞正文生成链路）。
pub(crate) async fn fetch_image(
    state: &crate::AppState,
    channel: &str,
    prompt: &str,
    size: Option<&str>,
    aspect_ratio: Option<&str>,
) -> Result<ImageFetch, String> {
    // 本机回环调用：必须禁用继承的 HTTP(S)_PROXY（否则走 gost → 502）
    let client = reqwest::Client::builder().no_proxy().build().unwrap_or_default();

    match channel {
        "uniapi" => {
            let base = state
                .image_base_url
                .clone()
                .unwrap_or_else(|| "http://127.0.0.1:18998/v1".into());
            let key = state.image_api_key.clone().unwrap_or_default();
            let model = state.image_model.clone();
            let size = size.unwrap_or("1024x1024");
            let r = client
                .post(format!("{}/images/generations", base.trim_end_matches('/')))
                .bearer_auth(key)
                .json(&json!({ "model": model, "prompt": prompt, "n": 1, "size": size }))
                .send()
                .await;
            match r {
                Ok(resp) if resp.status().is_success() => {
                    let v: serde_json::Value = resp.json().await.unwrap_or(json!({}));
                    let items = v
                        .get("data")
                        .and_then(|d| d.as_array())
                        .cloned()
                        .unwrap_or_default();
                    let url = items
                        .first()
                        .and_then(|i| i.get("url"))
                        .and_then(|u| u.as_str())
                        .map(|s| s.to_string());
                    let b64 = items
                        .first()
                        .and_then(|i| i.get("b64_json"))
                        .and_then(|b| b.as_str())
                        .map(|s| s.to_string());
                    if url.is_none() && b64.is_none() {
                        return Err("uniapi 返回了空 data（未生成图片）".into());
                    }
                    Ok(ImageFetch { url, b64, channel: "uniapi".into() })
                }
                Ok(resp) => Err(format!("uniapi 上游错误 {}", resp.status())),
                Err(e) => Err(format!("uniapi 请求失败：{e}")),
            }
        }
        "cf-manager" => {
            // 免费 flux 池（cf-manager :4001 /v1/images/generations，账号池轮换）
            let base = state
                .cf_image_base_url
                .clone()
                .unwrap_or_else(|| "http://127.0.0.1:4001/v1".into());
            let model = state
                .cf_image_model
                .clone()
                .unwrap_or_else(|| "@cf/black-forest-labs/flux-1-schnell".into());
            let r = client
                .post(format!("{}/images/generations", base.trim_end_matches('/')))
                .json(&json!({ "model": model, "prompt": prompt, "n": 1 }))
                .send()
                .await;
            match r {
                Ok(resp) if resp.status().is_success() => {
                    let v: serde_json::Value = resp.json().await.unwrap_or(json!({}));
                    let items = v
                        .get("data")
                        .and_then(|d| d.as_array())
                        .cloned()
                        .unwrap_or_default();
                    let b64 = items
                        .first()
                        .and_then(|i| i.get("b64_json"))
                        .and_then(|b| b.as_str())
                        .map(|s| s.to_string());
                    let url = items
                        .first()
                        .and_then(|i| i.get("url"))
                        .and_then(|u| u.as_str())
                        .map(|s| s.to_string());
                    if url.is_none() && b64.is_none() {
                        return Err("cf-manager 返回了空 data（未生成图片）".into());
                    }
                    Ok(ImageFetch { url, b64, channel: "cf-manager".into() })
                }
                Ok(resp) => Err(format!("cf-manager 上游错误 {}", resp.status())),
                Err(e) => Err(format!("cf-manager 请求失败：{e}")),
            }
        }
        "grok2api" => {
            // grok-imagine-image（chenyme-grok2api :8020；必需 aspect_ratio 参数）
            let base = state
                .grok2api_image_base_url
                .clone()
                .unwrap_or_else(|| "http://127.0.0.1:8020/v1".into());
            let key = state
                .grok2api_image_key
                .clone()
                .unwrap_or_default();
            let model = state
                .grok2api_image_model
                .clone()
                .unwrap_or_else(|| "grok-imagine-image".into());
            let aspect = aspect_ratio.unwrap_or("1:1");
            let r = client
                .post(format!("{}/images/generations", base.trim_end_matches('/')))
                .bearer_auth(key)
                .json(&json!({ "model": model, "prompt": prompt, "n": 1, "aspect_ratio": aspect }))
                .send()
                .await;
            match r {
                Ok(resp) if resp.status().is_success() => {
                    let v: serde_json::Value = resp.json().await.unwrap_or(json!({}));
                    let items = v
                        .get("data")
                        .and_then(|d| d.as_array())
                        .cloned()
                        .unwrap_or_default();
                    let mut url = items
                        .first()
                        .and_then(|i| i.get("url"))
                        .and_then(|u| u.as_str())
                        .map(|s| s.to_string());
                    // chenyme 返回容器内端口 :8000，宿主映射为 :8020 —— 重写保证前端可访问
                    if let Some(u) = &url {
                        if u.contains("127.0.0.1:8000") || u.contains("localhost:8000") {
                            url = Some(u.replace(":8000", ":8020"));
                        }
                    }
                    if url.is_none() {
                        return Err("grok2api 返回了空 data（未生成图片）".into());
                    }
                    Ok(ImageFetch { url, b64: None, channel: "grok2api".into() })
                }
                Ok(resp) => Err(format!("grok2api 上游错误 {}", resp.status())),
                Err(e) => Err(format!("grok2api 请求失败：{e}")),
            }
        }
        other => Err(format!("channel '{other}' not configured")),
    }
}

pub async fn generate_image(
    State(state): State<crate::AppState>,
    Json(body): Json<ImageRequest>,
) -> Response {
    let prompt = body.prompt.trim().to_string();
    if prompt.is_empty() {
        return bad_request("TOOLS_EMPTY_PROMPT", "empty prompt");
    }
    let channel = body.channel.as_deref().unwrap_or("uniapi");
    match fetch_image(&state, channel, &prompt, body.size.as_deref(), body.aspect_ratio.as_deref()).await {
        Ok(f) => Json(json!({ "url": f.url, "b64": f.b64, "channel": f.channel })).into_response(),
        Err(e) => internal("TOOLS_LLM_FAILED", e),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TtsRequest {
    pub text: String,
    #[serde(default)]
    pub voice: Option<String>,
    #[serde(default)]
    pub engine: Option<String>,
    /// P3 语音完整版：语速（edge-tts --rate，如 "+20%" / "-10%"，空 = 默认）。
    #[serde(default)]
    pub rate: Option<String>,
}

/// 常用 edge-tts 中文语音（找不到 CLI 时回落路径）。
const EDGE_TTS_PATHS: &[&str] = &[
    "/usr/local/bin/edge-tts",
    "/usr/local/bin/edge-tts",
    "/usr/bin/edge-tts",
];

pub async fn text_to_speech(
    State(_state): State<crate::AppState>,
    Json(body): Json<TtsRequest>,
) -> Response {
    let text = body.text.trim().to_string();
    if text.is_empty() {
        return bad_request("TOOLS_EMPTY_TEXT", "empty text");
    }
    let engine = body.engine.as_deref().unwrap_or("edge");
    let voice = body.voice.clone().unwrap_or_else(|| "zh-CN-XiaoxiaoNeural".into());

    match engine {
        "edge" => {
            let bin = EDGE_TTS_PATHS
                .iter()
                .find(|p| std::path::Path::new(p).exists())
                .map(|s| s.to_string());
            let Some(bin) = bin else {
                return service_unavailable("TOOLS_TTS_CLI_MISSING", "edge-tts CLI not found");
            };
            let out = format!("/tmp/kaleido_tts_{}.mp3", uuid::Uuid::new_v4());
            let mut cmd = tokio::process::Command::new(&bin);
            cmd.args(["--voice", &voice, "--text", &text, "--write-media", &out]);
            if let Some(rate) = body.rate.as_deref().filter(|r| !r.trim().is_empty()) {
                cmd.args(["--rate", rate]);
            }
            match cmd.output().await
            {
                Ok(o) if o.status.success() => {
                    if let Ok(bytes) = tokio::fs::read(&out).await {
                        let _ = tokio::fs::remove_file(&out).await;
                        let mut resp = axum::response::Response::new(bytes.into());
                        resp.headers_mut()
                            .insert("content-type", "audio/mpeg".parse().unwrap());
                        return resp;
                    }
                    internal("TOOLS_TTS_READ_FAILED", "read audio failed")
                }
                Ok(o) => err_with_code(
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    "TOOLS_TTS_EXIT",
                    format!("edge-tts exit {:?}", o.status.code()),
                    serde_json::json!({ "stderr": String::from_utf8_lossy(&o.stderr).chars().take(200).collect::<String>() }),
                ),
                Err(e) => internal("TOOLS_TTS_SPAWN", e.to_string()),
            }
        }
        _ => bad_request("TOOLS_ENGINE_UNCONFIGURED", format!("engine '{engine}' not configured")),
    }
}

/// POST /api/v1/kaleido-tools/asr — multipart `audio` file → local faster-whisper text.
/// Optional form field `modelSize` (default "small"; falls back to "base" if small
/// cannot be downloaded/loaded). The whisper model is lazy-loaded in a persistent
/// worker subprocess and reused across requests.
pub async fn speech_to_text(
    State(_state): State<crate::AppState>,
    mut multipart: Multipart,
) -> Response {
    let mut audio: Option<Vec<u8>> = None;
    let mut filename = String::new();
    let mut model_size = "small".to_string();
    while let Ok(Some(field)) = multipart.next_field().await {
        match field.name().unwrap_or("") {
            "audio" => {
                if audio.is_none() {
                    let fname = field.file_name().unwrap_or("").to_string();
                    if let Ok(data) = field.bytes().await {
                        if !data.is_empty() {
                            audio = Some(data.to_vec());
                            filename = fname;
                        }
                    }
                }
            }
            "modelSize" => {
                if let Ok(v) = field.text().await {
                    let t = v.trim().to_string();
                    if !t.is_empty() { model_size = t; }
                }
            }
            _ => {}
        }
    }
    let Some(data) = audio else {
        return bad_request("TOOLS_AUDIO_FIELD_MISSING", "missing multipart field 'audio'");
    };
    if data.is_empty() {
        return bad_request("TOOLS_EMPTY_AUDIO", "empty audio");
    }
    // Write to /tmp; keep the original extension so the worker's ffmpeg normalizer
    // can pick a sane output (it inspects magic bytes, extension is cosmetic).
    let ext = std::path::Path::new(&filename)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_string())
        .unwrap_or_else(|| "webm".into());
    let tmp = format!(
        "/tmp/kaleido_asr_{}.{}",
        uuid::Uuid::new_v4().simple(),
        ext.chars().take(10).collect::<String>()
    );
    if let Err(e) = tokio::fs::write(&tmp, &data).await {
        return internal("TOOLS_AUDIO_WRITE_FAILED", format!("write audio failed: {e}"));
    }
    let mut slot = crate::asr::asr_slot().lock().await;
    if slot.is_none() {
        match crate::asr::AsrWorker::spawn() {
            Ok(w) => *slot = Some(w),
            Err(e) => {
                let _ = tokio::fs::remove_file(&tmp).await;
                return internal("TOOLS_ASR_SPAWN_FAILED", format!("asr worker spawn failed: {e}"));
            }
        }
    }
    let result = slot.as_mut().unwrap().transcribe(&tmp, &model_size).await;
    drop(slot);
    let _ = tokio::fs::remove_file(&tmp).await;
    match result {
        Ok(text) => Json(json!({ "text": text })).into_response(),
        Err(e) => internal("TOOLS_ASR_FAILED", e),
    }
}
