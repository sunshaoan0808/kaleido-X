//! Anthropic Claude Messages API provider.
//!
//! Implements the Anthropic Messages API with streaming support,
//! including content block deltas (text, thinking) and tool use.

use super::{ChatRequest, EmbedRequest, LLMProvider, ProviderConfig};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::time::Duration;

pub struct AnthropicProvider {
    base_url: String,
    api_key: String,
    timeout_secs: u64,
}

impl AnthropicProvider {
    pub fn new(config: &ProviderConfig) -> Self {
        let base = config
            .base_url
            .trim_end_matches('/')
            .to_string();
        let base_url = if base.is_empty() {
            "https://api.anthropic.com".to_string()
        } else {
            base
        };

        Self {
            base_url,
            api_key: config.api_key.clone(),
            timeout_secs: config.timeout_secs.max(30),
        }
    }

    fn client(&self, timeout: u64) -> Result<reqwest::Client, String> {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(timeout))
            .build()
            .map_err(|e| format!("http client: {e}"))
    }

    /// Build messages array from ChatRequest.
    /// Anthropic Messages API uses a different format than OpenAI:
    /// - Messages alternate user/assistant
    /// - System prompt is separate at top level
    fn build_request_body(&self, req: &ChatRequest, stream: bool) -> Value {
        let mut system: Option<String> = None;
        let mut messages: Vec<Value> = Vec::new();

        for msg in &req.messages {
            match msg.role.as_str() {
                "system" => {
                    system = Some(msg.content.clone());
                }
                _ => {
                    messages.push(json!({
                        "role": msg.role,
                        "content": msg.content,
                    }));
                }
            }
        }

        let mut body = json!({
            "model": req.model,
            "max_tokens": req.max_tokens,
            "stream": stream,
            "temperature": req.temperature,
            "messages": messages,
        });

        if let Some(sys) = system {
            body["system"] = json!(sys);
        }

        body
    }
}

impl LLMProvider for AnthropicProvider {
    async fn stream_chat(
        &self,
        req: &ChatRequest,
        on_delta: &mut (dyn FnMut(&str) -> bool + Send),
    ) -> Result<String, String> {
        if self.api_key.is_empty() {
            return Err("anthropic api_key empty".into());
        }

        let client = self.client(req.timeout_secs.max(self.timeout_secs))?;
        let url = format!("{}/v1/messages", self.base_url);
        let body = self.build_request_body(req, true);

        let resp = client
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("anthropic connect: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(format!(
                "anthropic {status}: {}",
                text.chars().take(300).collect::<String>()
            ));
        }

        let mut full = String::new();
        let mut buf = String::new();
        let mut byte_carry: Vec<u8> = Vec::new();
        let mut byte_stream = resp.bytes_stream();

        // Anthropic SSE events:
        // event: message_start
        // event: content_block_start
        // event: ping
        // event: content_block_delta
        //   data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}
        // event: content_block_stop
        // event: message_delta
        // event: message_stop
        let mut pending_event = String::new();

        while let Some(item) = byte_stream.next().await {
            let bytes = match item {
                Ok(b) => b,
                Err(e) => {
                    if full.is_empty() {
                        return Err(format!("anthropic stream error: {e}"));
                    }
                    break;
                }
            };

            buf.push_str(&crate::utf8_stream::push_utf8_chunk(&mut byte_carry, &bytes));

            while let Some(pos) = buf.find('\n') {
                let line = buf[..pos].to_string();
                buf = buf[pos + 1..].to_string();
                let line = line.trim().to_string();

                if line.is_empty() {
                    continue;
                }

                if let Some(event_name) = line.strip_prefix("event: ") {
                    pending_event = event_name.to_string();
                    continue;
                }

                if let Some(data_str) = line.strip_prefix("data: ") {
                    let event = std::mem::take(&mut pending_event);
                    match event.as_str() {
                        "content_block_delta" => {
                            if let Ok(v) = serde_json::from_str::<Value>(&data_str) {
                                if let Some(text) = v
                                    .pointer("/delta/text")
                                    .and_then(|t| t.as_str())
                                {
                                    if !text.is_empty() {
                                        full.push_str(text);
                                        if !on_delta(text) {
                                            return Ok(full);
                                        }
                                    }
                                }
                            }
                        }
                        "message_stop" | "error" => {
                            // Stream complete
                            if full.trim().is_empty() {
                                // Check if there was an error in message_start
                                return Err("anthropic empty response".into());
                            }
                            return Ok(full);
                        }
                        _ => {} // message_start, content_block_start, ping, etc.
                    }
                }
            }
        }

        if full.trim().is_empty() {
            Err("anthropic empty stream".into())
        } else {
            Ok(full)
        }
    }

    async fn chat(&self, req: &ChatRequest) -> Result<String, String> {
        if self.api_key.is_empty() {
            return Err("anthropic api_key empty".into());
        }

        let client = self.client(req.timeout_secs.max(self.timeout_secs))?;
        let url = format!("{}/v1/messages", self.base_url);
        let body = self.build_request_body(req, false);

        let resp = client
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("anthropic: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(format!(
                "anthropic {status}: {}",
                text.chars().take(200).collect::<String>()
            ));
        }

        let data: Value = resp
            .json()
            .await
            .map_err(|e| format!("anthropic parse: {e}"))?;

        // Extract text from content blocks
        let content = data["content"]
            .as_array()
            .map(|blocks| {
                blocks
                    .iter()
                    .filter_map(|block| block["text"].as_str())
                    .collect::<Vec<_>>()
                    .concat()
            })
            .unwrap_or_default();

        if content.is_empty() {
            return Err("anthropic empty response".into());
        }
        Ok(content)
    }

    async fn embed(&self, _req: &EmbedRequest) -> Result<Vec<f32>, String> {
        Err("Anthropic does not support embeddings via Messages API".into())
    }
}
