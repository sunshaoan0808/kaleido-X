//! OpenAI-compatible chat/completions streaming helper.
//!
//! Used by Background / BookTravel (and reusable elsewhere) to:
//! - call `{base}/chat/completions` with `stream: true`
//! - parse SSE `data:` chunks
//! - invoke `on_delta` for each content piece
//! - return the full accumulated text
//!
//! Soft-fail style: returns `Err(String)` for connect/HTTP/empty; callers map to heuristic.

use futures_util::StreamExt;
use serde_json::{json, Value};
use std::time::Duration as StdDuration;

/// G6: effective provider kind for an LlmRuntime — managed providers carry
/// their own protocol; env/settings fallback defers to the KALEIDO_LLM_PROVIDER
/// default held in AppState.
pub fn runtime_provider_kind(llm: &kaleido_core::LlmRuntime, state_kind: &str) -> String {
    let k = llm.provider_kind.trim();
    if k.is_empty() {
        state_kind.to_string()
    } else {
        k.to_string()
    }
}

/// Stream an OpenAI-compatible chat completion.
///
/// `on_delta(chunk) -> bool`: return `false` to abort early (cancel).
/// On abort, returns `Err("cancelled")` if no text yet, else `Ok(partial)`.
pub async fn stream_chat_completions<F>(
    base_url: &str,
    api_key: &str,
    model: &str,
    system: &str,
    user: &str,
    temperature: f64,
    max_tokens: u32,
    timeout_secs: u64,
    mut on_delta: F,
) -> Result<String, String>
where
    F: FnMut(&str) -> bool + Send,
{
    let messages = vec![
        json!({"role": "system", "content": system}),
        json!({"role": "user", "content": user}),
    ];
    stream_chat_completions_msgs(base_url, api_key, model, messages, temperature, max_tokens, timeout_secs, &mut on_delta).await
}

/// 多轮版：`messages` 为完整对话数组（system 需调用方自行放入首位）。
/// 剧情助手等需要携带对话历史的场景使用。
pub async fn stream_chat_completions_msgs<F>(
    base_url: &str,
    api_key: &str,
    model: &str,
    messages: Vec<serde_json::Value>,
    temperature: f64,
    max_tokens: u32,
    timeout_secs: u64,
    mut on_delta: F,
) -> Result<String, String>
where
    F: FnMut(&str) -> bool + Send,
{
    if base_url.trim().is_empty() {
        return Err("llm base_url empty".into());
    }
    // [SSRF 加固 2026-08-15, 吸收 6fef9d12] 连接前防御纵深复检（设置层之外的第二道防线）。
    if let Err(e) = kaleido_core::validate_llm_base_url(base_url) {
        return Err(format!("llm base_url rejected: {e}").into());
    }
    if api_key.trim().is_empty() {
        return Err("llm api_key empty".into());
    }
    let model = if model.trim().is_empty() {
        "gpt-4o-mini"
    } else {
        model.trim()
    };

    let client = reqwest::Client::builder()
        .timeout(StdDuration::from_secs(timeout_secs.max(30)))
        .build()
        .map_err(|e| format!("http client: {e}"))?;

    let url = format!(
        "{}/chat/completions",
        base_url.trim_end_matches('/')
    );
    let body = json!({
        "model": model,
        "stream": true,
        "temperature": temperature,
        "max_tokens": max_tokens,
        "messages": messages,
    });

    let resp = client
        .post(&url)
        .bearer_auth(api_key)
        .header("content-type", "application/json")
        .header("accept", "text/event-stream")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("upstream connect: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!(
            "upstream {status}: {}",
            text.chars().take(300).collect::<String>()
        ));
    }

    let mut full = String::new();
    let mut buf = String::new();
    let mut byte_carry: Vec<u8> = Vec::new();
    let mut byte_stream = resp.bytes_stream();
    let mut aborted = false;

    // Cancel-poll interval. `on_delta` is the cooperative cancel hook used by
    // callers (background / book_travel) — they return `false` once the job is
    // terminal. But the upstream can sit idle on a long tokenization for seconds
    // (no chunks → no callback → cancel never observed). Wrap the next-chunk
    // wait in `select!` with a 250ms heartbeat: on timeout, invoke the callback
    // with an empty delta so it can re-check cancel and abort promptly. Empty
    // pieces are filtered out of `full`, so this never corrupts the result.
    const CANCEL_POLL_MS: u64 = 250;

    loop {
        let next_chunk = byte_stream.next();
        tokio::pin!(next_chunk);
        let poll = tokio::time::sleep(StdDuration::from_millis(CANCEL_POLL_MS));
        tokio::pin!(poll);
        let item = tokio::select! {
            i = &mut next_chunk => match i {
                Some(it) => it,
                None => break, // stream ended
            },
            _ = &mut poll => {
                // Heartbeat: give the callback a chance to signal cancel.
                // Return `false` to abort; empty string never lands in `full`.
                if !on_delta("") {
                    aborted = true;
                    break;
                }
                continue;
            }
        };

        match item {
            Ok(bytes) => {
                // Carry incomplete UTF-8 across TCP chunks (CJK is 3 bytes).
                buf.push_str(&crate::utf8_stream::push_utf8_chunk(&mut byte_carry, &bytes));
                while let Some(pos) = buf.find('\n') {
                    let mut line = buf[..pos].to_string();
                    buf = buf[pos + 1..].to_string();
                    if line.ends_with('\r') {
                        line.pop();
                    }
                    if line.is_empty() {
                        continue;
                    }
                    // tolerate "data:" and "data: "
                    let data = if let Some(rest) = line.strip_prefix("data:") {
                        rest.trim_start()
                    } else {
                        continue;
                    };
                    if data == "[DONE]" {
                        return Ok(full);
                    }
                    if data.is_empty() {
                        continue;
                    }
                    if let Ok(v) = serde_json::from_str::<Value>(data) {
                        // [fix 2026-08-15 截断可观测] 捕获 finish_reason=length（上游 max_tokens
                        // 硬截断）——之前零检测，截断被静默接受为完整正文（宿醉 6360 字半截实踩）。
                        // 最小侵入：warn 日志 + 尾部截断标记，不改返回签名（调用方无需迁移）。
                        if let Some(fr) = v.pointer("/choices/0/finish_reason").and_then(|c| c.as_str()) {
                            if fr == "length" {
                                let total = full.chars().count();
                                tracing::warn!(
                                    model,
                                    %base_url,
                                    chars = total,
                                    "llm stream truncated by max_tokens (finish_reason=length)"
                                );
                                // 截断只记日志，不污染返回文本：调用方（回合正文/Heavy 管道
                                // draft）会直接落盘，尾部标记会泄漏进用户可见正文（实踩：
                                // 宿醉消息尾部出现「📤【输出已达长度上限，已截断】」）。
                            }
                        }
                        // OpenAI delta.content
                        let mut pieces: Vec<String> = Vec::new();
                        if let Some(s) = v
                            .pointer("/choices/0/delta/content")
                            .and_then(|c| c.as_str())
                        {
                            if !s.is_empty() {
                                pieces.push(s.to_string());
                            }
                        } else if let Some(arr) = v
                            .pointer("/choices/0/delta/content")
                            .and_then(|c| c.as_array())
                        {
                            // some providers send content as array of parts
                            for part in arr {
                                if let Some(t) = part.get("text").and_then(|x| x.as_str()) {
                                    if !t.is_empty() {
                                        pieces.push(t.to_string());
                                    }
                                } else if let Some(t) = part.as_str() {
                                    if !t.is_empty() {
                                        pieces.push(t.to_string());
                                    }
                                }
                            }
                        }
                        // non-stream style in a stream chunk
                        if pieces.is_empty() {
                            if let Some(s) = v
                                .pointer("/choices/0/message/content")
                                .and_then(|c| c.as_str())
                            {
                                if !s.is_empty() {
                                    pieces.push(s.to_string());
                                }
                            }
                        }
                        for piece in pieces {
                            full.push_str(&piece);
                            if !on_delta(&piece) {
                                aborted = true;
                                break;
                            }
                        }
                        if aborted {
                            break;
                        }
                    }
                }
                if aborted {
                    break;
                }
            }
            Err(e) => {
                if full.is_empty() {
                    return Err(format!("upstream stream error: {e}"));
                }
                // partial is better than nothing for soft-fail parse
                break;
            }
        }
    }

    // End of stream: drop incomplete trailing bytes into buf (rare).
    let tail = crate::utf8_stream::flush_utf8_carry(&mut byte_carry);
    if !tail.is_empty() {
        buf.push_str(&tail);
    }

    if aborted {
        if full.is_empty() {
            return Err("cancelled".into());
        }
        return Ok(full);
    }

    if full.trim().is_empty() {
        return Err("empty stream content".into());
    }
    Ok(full)
}

/// Non-streaming chat completion. Returns the full response text.
/// Uses the same endpoint but without `stream:true`, for small structured calls (extractions, etc.).
/// Accepts a pre-built `reqwest::Client` (cheap to clone) to reuse existing client config & connection pool.
/// `timeout_secs` is unused (client manages its own timeout).
pub async fn chat_completion(
    base_url: &str,
    api_key: &str,
    model: &str,
    system: &str,
    user: &str,
    temperature: f64,
    max_tokens: u32,
    _timeout_secs: u64,
    client: &reqwest::Client,
) -> Result<String, String> {
    let request_timeout = std::time::Duration::from_secs(_timeout_secs.max(30));
    if base_url.trim().is_empty() {
        return Err("llm base_url empty".into());
    }
    // [SSRF 加固 2026-08-15, 吸收 6fef9d12] 非流式入口同样复检。
    if let Err(e) = kaleido_core::validate_llm_base_url(base_url) {
        return Err(format!("llm base_url rejected: {e}").into());
    }
    if api_key.trim().is_empty() {
        return Err("llm api_key empty".into());
    }
    let model = if model.trim().is_empty() {
        "gpt-4o-mini"
    } else {
        model.trim()
    };

    let url = format!(
        "{}/chat/completions",
        base_url.trim_end_matches('/')
    );
    let body = json!({
        "model": model,
        "stream": false,
        "temperature": temperature,
        // [F4 调参化] 原 16384 硬编码 → 调用方显式传参（convert 等传 16384 保持现状）。
        "max_tokens": max_tokens,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user},
        ],
    });

    let resp = client
        .post(&url)
        .bearer_auth(api_key)
        .header("content-type", "application/json")
        .json(&body)
        .timeout(request_timeout)
        .send()
        .await
        .map_err(|e| format!("extraction upstream: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!(
            "extraction upstream {status}: {}",
            text.chars().take(200).collect::<String>()
        ));
    }

    let data: Value = resp
        .json()
        .await
        .map_err(|e| format!("extraction parse: {e}"))?;

    let content = data["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    if content.is_empty() {
        return Err("extraction empty response".into());
    }

    Ok(content)
}

/// Strip markdown fences and extract the outermost JSON object/array if needed.
pub fn extract_json_value(text: &str) -> Option<Value> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    if let Ok(v) = serde_json::from_str::<Value>(t) {
        return Some(v);
    }
    // strip ```json ... ```
    let mut s = t.to_string();
    if let Some(start) = s.find("```") {
        let after = &s[start + 3..];
        let after = after
            .strip_prefix("json")
            .or_else(|| after.strip_prefix("JSON"))
            .unwrap_or(after);
        if let Some(end) = after.find("```") {
            s = after[..end].trim().to_string();
            if let Ok(v) = serde_json::from_str::<Value>(&s) {
                return Some(v);
            }
        }
    }
    // outermost object (健壮版: 用括号深度找第一个完整闭合的 }，
    // 避免模型在 JSON 后追加文字/注释时 rfind('}') 截到文字里的右括号)
    // 同时跳过数组内部的 {（避免数组文本时截到元素对象）
    {
        let bytes = t.as_bytes();
        let mut i = 0usize;
        while i < bytes.len() {
            let c = bytes[i] as char;
            if c == '{' {
                // 检查此 { 是否在某个未闭合 [ 内部：从文本头到当前位置做括号扫描
                let mut arr_depth = 0i32;
                let mut in_str = false;
                let mut esc = false;
                for j in 0..i {
                    let cj = bytes[j] as char;
                    if in_str {
                        if esc { esc = false; }
                        else if cj == '\\' { esc = true; }
                        else if cj == '"' { in_str = false; }
                        continue;
                    }
                    match cj {
                        '"' => in_str = true,
                        '[' => arr_depth += 1,
                        ']' => arr_depth -= 1,
                        _ => {}
                    }
                }
                if arr_depth > 0 {
                    // 在数组内部，跳过这个 {（数组分支会处理）
                    i += 1;
                    continue;
                }
                // 顶层对象：深度匹配闭合 }
                let mut depth = 0i32;
                let mut in_str2 = false;
                let mut esc2 = false;
                for k in i..bytes.len() {
                    let ck = bytes[k] as char;
                    if in_str2 {
                        if esc2 { esc2 = false; }
                        else if ck == '\\' { esc2 = true; }
                        else if ck == '"' { in_str2 = false; }
                        continue;
                    }
                    match ck {
                        '"' => in_str2 = true,
                        '{' => depth += 1,
                        '}' => {
                            depth -= 1;
                            if depth == 0 {
                                if let Ok(v) = serde_json::from_str::<Value>(&t[i..=k]) {
                                    return Some(v);
                                }
                                break;
                            }
                        }
                        _ => {}
                    }
                }
            }
            i += 1;
        }
    }
    // outermost array (健壮版: 用括号深度找第一个闭合的 ]，
    // 避免模型在 JSON 后追加注释时 rfind(']') 截到注释里的方括号)
    if let Some(a) = t.find('[') {
        let bytes = t.as_bytes();
        let mut depth = 0i32;
        let mut in_str = false;
        let mut esc = false;
        for i in a..bytes.len() {
            let c = bytes[i] as char;
            if in_str {
                if esc { esc = false; }
                else if c == '\\' { esc = true; }
                else if c == '"' { in_str = false; }
                continue;
            }
            match c {
                '"' => in_str = true,
                '[' | '{' => depth += 1,
                ']' => {
                    depth -= 1;
                    if depth == 0 {
                        if let Ok(v) = serde_json::from_str::<Value>(&t[a..=i]) {
                            return Some(v);
                        }
                        break;
                    }
                }
                '}' => depth -= 1,
                _ => {}
            }
        }
    }
    None
}

/// Get a single text embedding vector (512-d BGE-small-zh).
///
/// Order:
/// 1. In-process `fastembed` when `KALEIDO_EMBED_INLINE` enabled (default)
/// 2. HTTP OpenAI-compatible `/v1/embeddings` at `base_url` (legacy Python proxy)
/// 3. 零依赖本地哈希 embed 兜底（吸收自 Liyuan embedTextLocal）——模型/代理都不可用时
///    仍返回确定性 512-d 向量，保证检索功能不硬失败。
pub async fn get_embedding(
    base_url: &str,
    input: &str,
    client: &reqwest::Client,
) -> Result<Vec<f32>, String> {
    // Prefer in-process — no Python sidecar required.
    if crate::embed_local::inline_enabled() {
        match tokio::task::spawn_blocking({
            let s = input.to_string();
            move || crate::embed_local::embed_one(&s)
        })
        .await
        {
            Ok(Ok(v)) => return Ok(v),
            Ok(Err(e)) => {
                tracing::warn!(error=%e, "embed_local failed; trying remote");
            }
            Err(e) => {
                tracing::warn!(error=%e, "embed_local join failed; trying remote");
            }
        }
    }

    if !base_url.trim().is_empty() {
        let url = format!("{}/v1/embeddings", base_url.trim_end_matches('/'));
        let body = serde_json::json!({
            "input": input,
            "model": "BAAI/bge-small-zh-v1.5",
        });

        let resp = client
            .post(&url)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("embedding upstream: {e}"));

        if let Ok(resp) = resp {
            if resp.status().is_success() {
                let data: serde_json::Value = resp
                    .json()
                    .await
                    .map_err(|e| format!("embedding parse: {e}"))?;

                let arr = data["data"][0]["embedding"]
                    .as_array()
                    .ok_or_else(|| "embedding missing data[0].embedding".to_string())?;

                let vec: Vec<f32> = arr
                    .iter()
                    .filter_map(|v| v.as_f64().map(|f| f as f32))
                    .collect();

                if !vec.is_empty() {
                    return Ok(vec);
                }
            }
        }
    }

    // 兜底：本地哈希 embed（无模型、无网络、确定性）
    tracing::warn!(
        "embedding chain exhausted (inline + remote); falling back to local hash embed (512-d)"
    );
    Ok(kaleido_core::embed_hash::embed_text_hash(input, 512))
}

/// Cap very long premise/context for prompts.
pub fn cap_for_prompt(s: &str, max: usize) -> String {
    let t = s.trim();
    if t.chars().count() <= max {
        return t.to_string();
    }
    t.chars().take(max).collect::<String>() + "\n…(truncated)"
}

/// Backward-compat alias for cap_for_prompt.
pub fn limit_text(text: &str, max_chars: usize) -> String {
    cap_for_prompt(text, max_chars)
}


// ---------------------------------------------------------------------------
// Provider dispatch — select backend based on provider_kind string
// ---------------------------------------------------------------------------

use crate::llm_provider::{
    ChatMessage, ChatRequest, LLMProvider, ProviderConfig, ProviderKind,
};

/// Resolve `ProviderKind` from a string (case-insensitive).
fn parse_provider(kind: &str) -> ProviderKind {
    match kind.trim().to_lowercase().as_str() {
        "anthropic" => ProviderKind::Anthropic,
        "google" | "gemini" => ProviderKind::Google,
        _ => ProviderKind::OpenAI,
    }
}

/// Streaming chat completion with provider dispatch.
///
/// Same contract as [`stream_chat_completions`] but routes to the correct
/// provider backend based on `provider_kind`.
pub async fn stream_chat_completions_dispatch(
    base_url: &str,
    api_key: &str,
    model: &str,
    provider_kind: &str,
    system: &str,
    user: &str,
    temperature: f64,
    max_tokens: u32,
    timeout_secs: u64,
    mut on_delta: impl FnMut(&str) -> bool + Send,
) -> Result<String, String> {
    let kind = parse_provider(provider_kind);

    match kind {
        ProviderKind::Anthropic | ProviderKind::Google => {
            let config = ProviderConfig {
                kind,
                base_url: base_url.to_string(),
                api_key: api_key.to_string(),
                model: model.to_string(),
                timeout_secs: timeout_secs.max(30),
            };
            if !config.is_configured() {
                return Err("llm provider not configured (base_url/api_key)".into());
            }
            let provider = crate::llm_provider::create_provider(&config);
            let req = ChatRequest {
                model: model.to_string(),
                messages: vec![
                    ChatMessage { role: "system".into(), content: system.to_string() },
                    ChatMessage { role: "user".into(), content: user.to_string() },
                ],
                temperature,
                max_tokens,
                timeout_secs,
            };
            provider.stream_chat(&req, &mut on_delta).await
        }
        ProviderKind::OpenAI => {
            stream_chat_completions(
                base_url,
                api_key,
                model,
                system,
                user,
                temperature,
                max_tokens,
                timeout_secs,
                on_delta,
            )
            .await
        }
    }
}

/// 多轮版 + provider dispatch（G6）：`messages` 为完整对话数组。
/// Anthropic/Google 走 provider 后端；OpenAI 回退 `stream_chat_completions_msgs`。
pub async fn stream_chat_completions_msgs_dispatch<F>(
    base_url: &str,
    api_key: &str,
    model: &str,
    provider_kind: &str,
    messages: Vec<serde_json::Value>,
    temperature: f64,
    max_tokens: u32,
    timeout_secs: u64,
    mut on_delta: F,
) -> Result<String, String>
where
    F: FnMut(&str) -> bool + Send,
{
    let kind = parse_provider(provider_kind);
    match kind {
        ProviderKind::Anthropic | ProviderKind::Google => {
            let config = ProviderConfig {
                kind,
                base_url: base_url.to_string(),
                api_key: api_key.to_string(),
                model: model.to_string(),
                timeout_secs: timeout_secs.max(30),
            };
            if !config.is_configured() {
                return Err("llm provider not configured (base_url/api_key)".into());
            }
            let provider = crate::llm_provider::create_provider(&config);
            let msgs = messages
                .iter()
                .map(|m| ChatMessage {
                    role: m["role"].as_str().unwrap_or("user").to_string(),
                    content: m["content"].as_str().unwrap_or_default().to_string(),
                })
                .collect();
            let req = ChatRequest {
                model: model.to_string(),
                messages: msgs,
                temperature,
                max_tokens,
                timeout_secs,
            };
            provider.stream_chat(&req, &mut on_delta).await
        }
        ProviderKind::OpenAI => {
            stream_chat_completions_msgs(
                base_url,
                api_key,
                model,
                messages,
                temperature,
                max_tokens,
                timeout_secs,
                &mut on_delta,
            )
            .await
        }
    }
}

pub async fn chat_completion_dispatch(
    base_url: &str,
    api_key: &str,
    model: &str,
    provider_kind: &str,
    system: &str,
    user: &str,
    temperature: f64,
    max_tokens: u32,
    timeout_secs: u64,
    client: &reqwest::Client,
) -> Result<String, String> {
    let kind = parse_provider(provider_kind);

    match kind {
        ProviderKind::Anthropic | ProviderKind::Google => {
            let config = ProviderConfig {
                kind,
                base_url: base_url.to_string(),
                api_key: api_key.to_string(),
                model: model.to_string(),
                timeout_secs: timeout_secs.max(30),
            };
            if !config.is_configured() {
                return Err("llm provider not configured".into());
            }
            let provider = crate::llm_provider::create_provider(&config);
            let req = ChatRequest {
                model: model.to_string(),
                messages: vec![
                    ChatMessage { role: "system".into(), content: system.to_string() },
                    ChatMessage { role: "user".into(), content: user.to_string() },
                ],
                temperature,
                max_tokens,
                timeout_secs,
            };
            provider.chat(&req).await
        }
        ProviderKind::OpenAI => {
            chat_completion(base_url, api_key, model, system, user, temperature, max_tokens, timeout_secs, client).await
        }
    }
}

// ---------------------------------------------------------------------------
// G6 后置项（F3）：主回合级流式 dispatch —— story_tavern start_turn 的
// 原生 reqwest bytes_stream 循环下沉至此，三家协议统一入口。
//
// 与 stream_chat_completions_dispatch 的差异：
// - 事件面 richer：正文增量 + 推理增量（thinking）分离回调；
// - 结果带 usage（OpenAI total_tokens / Anthropic input+output / Gemini
//   totalTokenCount），随 asst_msg.tokens 落盘；
// - 错误分类 TurnStreamError 与旧内嵌循环一一对应（Connect / Status{body}
//   / Stream / Stopped），调用方的模型回退与 UPSTREAM_* 语义码保持不变；
// - should_stop 钩子在每块网络数据前检查，承载取消（StreamHub）与 U11
//   回合预算看门狗。
// ---------------------------------------------------------------------------

/// 主回合流事件：正文增量 / 推理增量。
#[derive(Debug, Clone)]
pub enum TurnStreamEvent {
    /// 正文 delta（OpenAI choices[].delta.content / Anthropic text_delta /
    /// Gemini part.text）
    Delta(String),
    /// 推理/思考增量（OpenAI reasoning_content|reasoning /
    /// Anthropic thinking_delta / Gemini thought part）
    Thinking(String),
}

/// 主回合流结果。空 text 不算错误——空响应重试是调用方策略。
#[derive(Debug, Default, Clone)]
pub struct TurnStreamOutcome {
    pub text: String,
    pub thinking: String,
    /// OpenAI usage.total_tokens；Anthropic input+output tokens；Gemini
    /// usageMetadata.totalTokenCount。上游未给则 None。
    pub total_tokens: Option<u64>,
}

/// 主回合流错误分类（对应旧内嵌循环的 UPSTREAM_CONNECT / UPSTREAM_STATUS
/// (+模型回退) / UPSTREAM_STREAM / 中途停止）。
#[derive(Debug, Clone)]
pub enum TurnStreamError {
    Connect(String),
    Status { status: u16, body: String },
    Stream(String),
    /// should_stop() 命中或下游放弃消费（on_event 返回 false）；调用方按
    /// 自身状态区分「取消」与「超时看门狗」终态。
    Stopped,
}

impl TurnStreamError {
    /// 人类可读消息（调用方直接放进 error event 的 message 字段）。
    pub fn message(&self) -> String {
        match self {
            Self::Connect(e) => format!("upstream connect: {e}"),
            Self::Status { status, body } => format!(
                "upstream {status}: {}",
                body.chars().take(300).collect::<String>()
            ),
            Self::Stream(e) => e.clone(),
            Self::Stopped => "stopped".into(),
        }
    }
}

/// 上游 4xx 是否为「模型不存在/不可用」类错误 —— 主回合模型回退判据，
/// 从 start_turn 内联逻辑原样迁出（含中文「不可用」，zen 网关实测形态）。
pub fn is_model_rejection(status: u16, body: &str) -> bool {
    status >= 400
        && status < 500
        && (body.contains("invalid_model")
            || body.contains("model_not_found")
            || body.contains("model not found")
            || body.contains("model_not_supported")
            || body.contains("不可用"))
}

/// 主回合级流式 dispatch：system+user 进 → 增量事件出 → outcome 返回。
/// temperature/max_tokens 由调用方给定（主回合 0.75/32768）。
pub async fn stream_chat_turn_dispatch(
    base_url: &str,
    api_key: &str,
    model: &str,
    provider_kind: &str,
    system: &str,
    user: &str,
    temperature: f64,
    max_tokens: u32,
    timeout_secs: u64,
    mut on_event: impl FnMut(TurnStreamEvent) -> bool + Send,
    should_stop: impl Fn() -> bool + Send + Sync,
) -> Result<TurnStreamOutcome, TurnStreamError> {
    match parse_provider(provider_kind) {
        ProviderKind::OpenAI => {
            stream_chat_turn_openai(
                base_url, api_key, model, system, user, temperature, max_tokens,
                timeout_secs, &mut on_event, &should_stop,
            )
            .await
        }
        ProviderKind::Anthropic => {
            stream_chat_turn_anthropic(
                base_url, api_key, model, system, user, temperature, max_tokens,
                timeout_secs, &mut on_event, &should_stop,
            )
            .await
        }
        ProviderKind::Google => {
            stream_chat_turn_google(
                base_url, api_key, model, system, user, temperature, max_tokens,
                timeout_secs, &mut on_event, &should_stop,
            )
            .await
        }
    }
}

type TurnEventCb<'a> = &'a mut (dyn FnMut(TurnStreamEvent) -> bool + Send);
type StopCb<'a> = &'a (dyn Fn() -> bool + Send + Sync);

fn turn_http_client(timeout_secs: u64) -> Result<reqwest::Client, TurnStreamError> {
    reqwest::Client::builder()
        .timeout(StdDuration::from_secs(timeout_secs.max(30)))
        .build()
        .map_err(|e| TurnStreamError::Connect(format!("http client: {e}")))
}

/// SSE 行切分器：跨 chunk 安全（utf8 边界由 utf8_stream 处理，行边界在此处理）。
struct SseLineSplitter {
    buf: String,
    carry: Vec<u8>,
}

impl SseLineSplitter {
    fn new() -> Self {
        Self { buf: String::new(), carry: Vec::new() }
    }
    fn push(&mut self, bytes: &[u8]) {
        self.buf.push_str(&crate::utf8_stream::push_utf8_chunk(&mut self.carry, bytes));
    }
    fn next_line(&mut self) -> Option<String> {
        let pos = self.buf.find('\n')?;
        let mut line = self.buf[..pos].to_string();
        self.buf = self.buf[pos + 1..].to_string();
        if line.ends_with('\r') {
            line.pop();
        }
        Some(line)
    }
}

/// OpenAI-compatible 分支（deepseek 等）：语义与旧内嵌循环逐条对齐 ——
/// data:[DONE] 终止、usage.total_tokens 捕获、reasoning_content|reasoning
/// 思维链、data: 后无空格也容忍（openai.rs 同款宽松解析）。
async fn stream_chat_turn_openai(
    base_url: &str,
    api_key: &str,
    model: &str,
    system: &str,
    user: &str,
    temperature: f64,
    max_tokens: u32,
    timeout_secs: u64,
    on_event: TurnEventCb<'_>,
    should_stop: StopCb<'_>,
) -> Result<TurnStreamOutcome, TurnStreamError> {
    if base_url.trim().is_empty() || api_key.trim().is_empty() {
        return Err(TurnStreamError::Connect("llm base_url/api_key empty".into()));
    }
    let client = turn_http_client(timeout_secs)?;
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let body = json!({
        "model": model,
        "stream": true,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user},
        ],
        "temperature": temperature,
        "max_tokens": max_tokens,
    });
    let resp = client
        .post(&url)
        .bearer_auth(api_key)
        .header("content-type", "application/json")
        .header("accept", "text/event-stream")
        .json(&body)
        .send()
        .await
        .map_err(|e| TurnStreamError::Connect(e.to_string()))?;
    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body_text = resp.text().await.unwrap_or_default();
        return Err(TurnStreamError::Status { status, body: body_text });
    }

    let mut outcome = TurnStreamOutcome::default();
    let mut splitter = SseLineSplitter::new();
    let mut byte_stream = resp.bytes_stream();

    while let Some(item) = byte_stream.next().await {
        if should_stop() {
            return Err(TurnStreamError::Stopped);
        }
        let bytes = match item {
            Ok(b) => b,
            Err(e) => return Err(TurnStreamError::Stream(e.to_string())),
        };
        splitter.push(&bytes);
        while let Some(line) = splitter.next_line() {
            if line.is_empty() {
                continue;
            }
            let data = match line.strip_prefix("data:") {
                Some(rest) => rest.trim_start(),
                None => continue,
            };
            if data == "[DONE]" {
                return Ok(outcome);
            }
            if data.is_empty() {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(data) else {
                continue;
            };
            if let Some(u) = v["usage"]["total_tokens"].as_u64() {
                outcome.total_tokens = Some(u);
            }
            if let Some(d) = v["choices"][0]["delta"]["content"].as_str() {
                if !d.is_empty() {
                    outcome.text.push_str(d);
                    if !on_event(TurnStreamEvent::Delta(d.to_string())) {
                        return Err(TurnStreamError::Stopped);
                    }
                }
            }
            let reasoning = v["choices"][0]["delta"]["reasoning_content"]
                .as_str()
                .or_else(|| v["choices"][0]["delta"]["reasoning"].as_str());
            if let Some(r) = reasoning {
                if !r.is_empty() {
                    outcome.thinking.push_str(r);
                    if !on_event(TurnStreamEvent::Thinking(r.to_string())) {
                        return Err(TurnStreamError::Stopped);
                    }
                }
            }
        }
    }
    Ok(outcome)
}

/// Anthropic Messages 分支：event-framed SSE；text_delta → 正文、
/// thinking_delta → 思维链；usage 取 message_start.input_tokens +
/// message_delta.output_tokens 之和（≈OpenAI total_tokens 口径）。
async fn stream_chat_turn_anthropic(
    base_url: &str,
    api_key: &str,
    model: &str,
    system: &str,
    user: &str,
    temperature: f64,
    max_tokens: u32,
    timeout_secs: u64,
    on_event: TurnEventCb<'_>,
    should_stop: StopCb<'_>,
) -> Result<TurnStreamOutcome, TurnStreamError> {
    let client = turn_http_client(timeout_secs)?;
    let url = format!("{}/v1/messages", base_url.trim_end_matches('/'));
    let mut body = json!({
        "model": model,
        "max_tokens": max_tokens,
        "stream": true,
        "temperature": temperature,
        "messages": [{"role": "user", "content": user}],
    });
    if !system.trim().is_empty() {
        body["system"] = json!(system);
    }
    let resp = client
        .post(&url)
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| TurnStreamError::Connect(e.to_string()))?;
    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body_text = resp.text().await.unwrap_or_default();
        return Err(TurnStreamError::Status { status, body: body_text });
    }

    let mut outcome = TurnStreamOutcome::default();
    let mut input_tokens: Option<u64> = None;
    let mut output_tokens: Option<u64> = None;
    let mut splitter = SseLineSplitter::new();
    let mut pending_event = String::new();
    let mut byte_stream = resp.bytes_stream();

    'outer: while let Some(item) = byte_stream.next().await {
        if should_stop() {
            return Err(TurnStreamError::Stopped);
        }
        let bytes = match item {
            Ok(b) => b,
            Err(e) => return Err(TurnStreamError::Stream(e.to_string())),
        };
        splitter.push(&bytes);
        while let Some(raw) = splitter.next_line() {
            let line = raw.trim().to_string();
            if line.is_empty() {
                continue;
            }
            if let Some(name) = line.strip_prefix("event:") {
                pending_event = name.trim().to_string();
                continue;
            }
            let Some(data_str) = line.strip_prefix("data:") else {
                continue;
            };
            let data_str = data_str.trim_start();
            let event = std::mem::take(&mut pending_event);
            let Ok(v) = serde_json::from_str::<Value>(data_str) else {
                continue;
            };
            match event.as_str() {
                "message_start" => {
                    input_tokens = v
                        .pointer("/message/usage/input_tokens")
                        .and_then(|t| t.as_u64())
                        .or(input_tokens);
                }
                "content_block_delta" => {
                    if let Some(text) = v.pointer("/delta/text").and_then(|t| t.as_str()) {
                        if !text.is_empty() {
                            outcome.text.push_str(text);
                            if !on_event(TurnStreamEvent::Delta(text.to_string())) {
                                return Err(TurnStreamError::Stopped);
                            }
                        }
                    } else if let Some(th) =
                        v.pointer("/delta/thinking").and_then(|t| t.as_str())
                    {
                        if !th.is_empty() {
                            outcome.thinking.push_str(th);
                            if !on_event(TurnStreamEvent::Thinking(th.to_string())) {
                                return Err(TurnStreamError::Stopped);
                            }
                        }
                    }
                }
                "message_delta" => {
                    output_tokens = v
                        .pointer("/usage/output_tokens")
                        .and_then(|t| t.as_u64())
                        .or(output_tokens);
                }
                "message_stop" => break 'outer,
                _ => {} // ping / content_block_start / content_block_stop / error
            }
        }
    }
    match (input_tokens, output_tokens) {
        (Some(i), Some(o)) => outcome.total_tokens = Some(i + o),
        (Some(i), None) => outcome.total_tokens = Some(i),
        (None, Some(o)) => outcome.total_tokens = Some(o),
        (None, None) => {}
    }
    Ok(outcome)
}

/// Google Gemini 分支：streamGenerateContent 默认返回按行分块的 JSON 对象
/// （与 llm_provider/google.rs 同款解析）。part.thought==true 的部分为
/// 思维摘要 → Thinking；usageMetadata.totalTokenCount → total_tokens。
async fn stream_chat_turn_google(
    base_url: &str,
    api_key: &str,
    model: &str,
    system: &str,
    user: &str,
    temperature: f64,
    max_tokens: u32,
    timeout_secs: u64,
    on_event: TurnEventCb<'_>,
    should_stop: StopCb<'_>,
) -> Result<TurnStreamOutcome, TurnStreamError> {
    if base_url.trim().is_empty() || api_key.trim().is_empty() {
        return Err(TurnStreamError::Connect("llm base_url/api_key empty".into()));
    }
    let client = turn_http_client(timeout_secs)?;
    let url = format!(
        "{}/models/{}:streamGenerateContent?key={}",
        base_url.trim_end_matches('/'),
        model,
        api_key
    );
    let mut body = json!({
        "contents": [{"role": "user", "parts": [{"text": user}]}],
        "generationConfig": {
            "temperature": temperature,
            "maxOutputTokens": max_tokens,
        },
    });
    if !system.trim().is_empty() {
        body["systemInstruction"] = json!({"parts": [{"text": system}]});
    }
    let resp = client
        .post(&url)
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| TurnStreamError::Connect(e.to_string()))?;
    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body_text = resp.text().await.unwrap_or_default();
        return Err(TurnStreamError::Status { status, body: body_text });
    }

    let mut outcome = TurnStreamOutcome::default();
    let mut splitter = SseLineSplitter::new();
    let mut byte_stream = resp.bytes_stream();

    while let Some(item) = byte_stream.next().await {
        if should_stop() {
            return Err(TurnStreamError::Stopped);
        }
        let bytes = match item {
            Ok(b) => b,
            Err(e) => return Err(TurnStreamError::Stream(e.to_string())),
        };
        splitter.push(&bytes);
        while let Some(line) = splitter.next_line() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            // 跳过 JSON 数组包装符（streamGenerateContent 流式形态）
            if line == "[" || line == "]" || line == "," || line.starts_with(']') {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if let Some(t) = v.pointer("/usageMetadata/totalTokenCount").and_then(|t| t.as_u64())
            {
                outcome.total_tokens = Some(t);
            }
            let Some(parts) = v
                .pointer("/candidates/0/content/parts")
                .and_then(|p| p.as_array())
            else {
                continue;
            };
            for part in parts {
                let Some(text) = part["text"].as_str() else {
                    continue;
                };
                if text.is_empty() {
                    continue;
                }
                let is_thought =
                    part["thought"].as_bool().unwrap_or(false);
                let ev = if is_thought {
                    outcome.thinking.push_str(text);
                    TurnStreamEvent::Thinking(text.to_string())
                } else {
                    outcome.text.push_str(text);
                    TurnStreamEvent::Delta(text.to_string())
                };
                if !on_event(ev) {
                    return Err(TurnStreamError::Stopped);
                }
            }
        }
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::extract_json_value;

    #[test]
    fn extract_json_value_plain_object() {
        let v = extract_json_value(r#"{"a":1}"#).unwrap();
        assert_eq!(v["a"], 1);
    }

    #[test]
    fn extract_json_value_object_with_trailing_text() {
        // 模型在合法 JSON 后追加文字（常见于 deepseek/grok 免费档）
        let t = r#"{"settings":[{"key":"世界观","value":"现代都市"}],"direction":"测试"}
我们根据系统提示，需要输出一个JSON，包含settings, direction 等字段。我们应遵循格式。"#;
        let v = extract_json_value(t).unwrap();
        assert_eq!(v["direction"], "测试");
        assert_eq!(v["settings"][0]["key"], "世界观");
    }

    #[test]
    fn extract_json_value_array_with_trailing_text() {
        let t = r#"[{"severity":"major","issue":"情节矛盾"}]
以上是审稿结果。"#;
        let v = extract_json_value(t).unwrap();
        assert!(v.is_array());
        assert_eq!(v[0]["severity"], "major");
    }

    #[test]
    fn extract_json_value_code_fence() {
        let t = "```json\n{\"ok\":true}\n```";
        let v = extract_json_value(t).unwrap();
        assert_eq!(v["ok"], true);
    }

    // -------------------------------------------------------------------
    // F3: stream_chat_turn_dispatch 单测 —— 本地 TCP 假上游，覆盖三家协议
    // 解析 + Stopped/Status 错误路径（不依赖外网）。
    // -------------------------------------------------------------------

    use super::{stream_chat_turn_dispatch, TurnStreamEvent, TurnStreamError};

    /// 起一个一次性 TCP 服务：读掉请求头后按给定响应体回包并关闭。
    async fn spawn_mock_upstream(response: &'static [u8]) -> u16 {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 8192];
            let _ = sock.read(&mut buf).await; // 读请求头（内容不校验）
            let _ = sock.write_all(response).await;
            let _ = sock.shutdown().await;
        });
        port
    }

    fn no_stop() -> bool {
        false
    }

    #[tokio::test]
    async fn turn_openai_parses_content_thinking_usage() {
        let resp = b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n\
data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"hmm\"}}]}\n\n\
data: {\"choices\":[{\"delta\":{\"content\":\"he\"}}]}\n\n\
data: {\"choices\":[{\"delta\":{\"content\":\"llo\"}}],\"usage\":{\"total_tokens\":42}}\n\n\
data: [DONE]\n\n";
        let port = spawn_mock_upstream(resp).await;
        let mut events = Vec::new();
        let out = stream_chat_turn_dispatch(
            &format!("http://127.0.0.1:{port}/v1"), "sk-test", "test-model", "",
            "sys", "usr", 0.75, 1024, 30,
            |ev| {
                events.push(ev);
                true
            },
            no_stop,
        )
        .await
        .unwrap();
        assert_eq!(out.text, "hello");
        assert_eq!(out.thinking, "hmm");
        assert_eq!(out.total_tokens, Some(42));
        assert_eq!(events.len(), 3);
        assert!(matches!(&events[0], TurnStreamEvent::Thinking(t) if t == "hmm"));
        assert!(matches!(&events[1], TurnStreamEvent::Delta(d) if d == "he"));
        assert!(matches!(&events[2], TurnStreamEvent::Delta(d) if d == "llo"));
    }

    #[tokio::test]
    async fn turn_openai_status_error_carries_body() {
        let resp = b"HTTP/1.1 404 Not Found\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n\
{\"error\":{\"code\":\"model_not_found\"}}";
        let port = spawn_mock_upstream(resp).await;
        let err = stream_chat_turn_dispatch(
            &format!("http://127.0.0.1:{port}/v1"), "sk-test", "ghost-model", "",
            "sys", "usr", 0.75, 1024, 30,
            |_| true,
            no_stop,
        )
        .await
        .unwrap_err();
        match err {
            TurnStreamError::Status { status, body } => {
                assert_eq!(status, 404);
                assert!(super::is_model_rejection(status, &body));
            }
            other => panic!("expected Status, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn turn_stops_on_should_stop() {
        let resp = b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n\
data: {\"choices\":[{\"delta\":{\"content\":\"abc\"}}]}\n\n\
data: {\"choices\":[{\"delta\":{\"content\":\"def\"}}]}\n\n\
data: [DONE]\n\n";
        let port = spawn_mock_upstream(resp).await;
        let err = stream_chat_turn_dispatch(
            &format!("http://127.0.0.1:{port}/v1"), "sk-test", "m", "",
            "sys", "usr", 0.75, 1024, 30,
            |_| true,
            || true, // 立即停止
        )
        .await
        .unwrap_err();
        assert!(matches!(err, TurnStreamError::Stopped));
    }

    #[tokio::test]
    async fn turn_anthropic_parses_text_thinking_and_usage() {
        let resp = b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n\
event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":11}}}\n\n\
event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"reasoning\"}}\n\n\
event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"answer\"}}\n\n\
event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":7}}\n\n\
event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
        let port = spawn_mock_upstream(resp).await;
        let mut kinds = Vec::new();
        let out = stream_chat_turn_dispatch(
            &format!("http://127.0.0.1:{port}"), "sk-ant", "claude-x", "anthropic",
            "sys", "usr", 0.75, 1024, 30,
            |ev| {
                kinds.push(match ev {
                    TurnStreamEvent::Delta(_) => "d",
                    TurnStreamEvent::Thinking(_) => "t",
                });
                true
            },
            no_stop,
        )
        .await
        .unwrap();
        assert_eq!(out.text, "answer");
        assert_eq!(out.thinking, "reasoning");
        assert_eq!(out.total_tokens, Some(18)); // input 11 + output 7
        assert_eq!(kinds, vec!["t", "d"]);
    }

    #[tokio::test]
    async fn turn_google_parses_parts_thought_and_usage() {
        let resp = b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n\
{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"think\"},{\"thought\":true,\"text\":\"th\"}]}}],\"usageMetadata\":{\"totalTokenCount\":33}}\n\
{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"ing\"}]}}]}\n";
        let port = spawn_mock_upstream(resp).await;
        let mut kinds = Vec::new();
        let out = stream_chat_turn_dispatch(
            &format!("http://127.0.0.1:{port}"), "g-key", "gemini-x", "google",
            "sys", "usr", 0.75, 1024, 30,
            |ev| {
                kinds.push(match ev {
                    TurnStreamEvent::Delta(_) => "d",
                    TurnStreamEvent::Thinking(_) => "t",
                });
                true
            },
            no_stop,
        )
        .await
        .unwrap();
        assert_eq!(out.text, "thinking");
        assert_eq!(out.thinking, "th");
        assert_eq!(out.total_tokens, Some(33));
        assert_eq!(kinds.len(), 3);
    }

    #[test]
    fn is_model_rejection_matches_legacy_strings() {
        assert!(super::is_model_rejection(404, "{\"error\":\"model_not_found\"}"));
        assert!(!super::is_model_rejection(500, "model_not_found"));
        assert!(!super::is_model_rejection(404, "rate limited"));
    }
}
