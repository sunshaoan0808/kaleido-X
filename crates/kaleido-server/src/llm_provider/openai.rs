//! OpenAI-compatible chat completions provider.
//!
//! Works with any API that speaks the OpenAI `/chat/completions` format:
//! OpenAI, DeepSeek, Groq, Fireworks, Together, etc.

use super::{ChatRequest, EmbedRequest, LLMProvider, ProviderConfig};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::time::Duration;

pub struct OpenAIProvider {
    base_url: String,
    api_key: String,
    timeout_secs: u64,
}

impl OpenAIProvider {
    pub fn new(config: &ProviderConfig) -> Self {
        Self {
            base_url: config.base_url.trim_end_matches('/').to_string(),
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
}

impl LLMProvider for OpenAIProvider {
    async fn stream_chat(
        &self,
        req: &ChatRequest,
        on_delta: &mut (dyn FnMut(&str) -> bool + Send),
    ) -> Result<String, String> {
        if self.base_url.is_empty() {
            return Err("llm base_url empty".into());
        }
        if self.api_key.is_empty() {
            return Err("llm api_key empty".into());
        }

        let client = self.client(req.timeout_secs.max(self.timeout_secs))?;
        let url = format!("{}/chat/completions", self.base_url);

        let messages: Vec<Value> = req
            .messages
            .iter()
            .map(|m| json!({"role": m.role, "content": m.content}))
            .collect();

        let body = json!({
            "model": req.model,
            "stream": true,
            "temperature": req.temperature,
            "max_tokens": req.max_tokens,
            "messages": messages,
        });

        let resp = client
            .post(&url)
            .bearer_auth(&self.api_key)
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

        while let Some(item) = byte_stream.next().await {
            match item {
                Ok(bytes) => {
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
                    break;
                }
            }
        }

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

    async fn chat(&self, req: &ChatRequest) -> Result<String, String> {
        if self.base_url.is_empty() {
            return Err("llm base_url empty".into());
        }
        if self.api_key.is_empty() {
            return Err("llm api_key empty".into());
        }

        let client = self.client(req.timeout_secs.max(self.timeout_secs))?;
        let url = format!("{}/chat/completions", self.base_url);

        let messages: Vec<Value> = req
            .messages
            .iter()
            .map(|m| json!({"role": m.role, "content": m.content}))
            .collect();

        let body = json!({
            "model": req.model,
            "stream": false,
            "temperature": req.temperature,
            "max_tokens": req.max_tokens,
            "messages": messages,
        });

        let resp = client
            .post(&url)
            .bearer_auth(&self.api_key)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("upstream: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(format!(
                "upstream {status}: {}",
                text.chars().take(200).collect::<String>()
            ));
        }

        let data: Value = resp
            .json()
            .await
            .map_err(|e| format!("parse: {e}"))?;

        let content = data["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or_default()
            .to_string();

        if content.is_empty() {
            return Err("empty response".into());
        }
        Ok(content)
    }

    async fn embed(&self, req: &EmbedRequest) -> Result<Vec<f32>, String> {
        if self.base_url.is_empty() {
            return Err("embedding base_url empty".into());
        }
        let url = format!("{}/v1/embeddings", self.base_url);
        let client = self.client(30)?;

        let body = json!({
            "input": req.input,
            "model": req.model.as_deref().unwrap_or("BAAI/bge-small-zh-v1.5"),
        });

        let resp = client
            .post(&url)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("embedding upstream: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(format!(
                "embedding upstream {status}: {}",
                text.chars().take(200).collect::<String>()
            ));
        }

        let data: Value = resp
            .json()
            .await
            .map_err(|e| format!("embedding parse: {e}"))?;

        let arr = data["data"][0]["embedding"]
            .as_array()
            .ok_or_else(|| "missing data[0].embedding".to_string())?;

        let vec: Vec<f32> = arr
            .iter()
            .filter_map(|v| v.as_f64().map(|f| f as f32))
            .collect();

        if vec.is_empty() {
            return Err("embedding returned empty vector".into());
        }
        Ok(vec)
    }
}
