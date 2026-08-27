//! Google Generative AI (Gemini) provider.
//!
//! Implements the Google Generative Language API with streaming support
//! via `streamGenerateContent` endpoint.

use super::{ChatRequest, EmbedRequest, LLMProvider, ProviderConfig};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::time::Duration;

pub struct GoogleProvider {
    base_url: String,
    api_key: String,
    timeout_secs: u64,
}

impl GoogleProvider {
    pub fn new(config: &ProviderConfig) -> Self {
        let base = config
            .base_url
            .trim_end_matches('/')
            .to_string();
        let base_url = if base.is_empty() {
            "https://generativelanguage.googleapis.com/v1beta".to_string()
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

    /// Build the request body for Google Generative AI API.
    /// Maps our ChatMessage format to Google's content/parts format.
    fn build_request_body(&self, req: &ChatRequest, _stream: bool) -> Value {
        let mut system_instruction: Option<Value> = None;
        let mut contents: Vec<Value> = Vec::new();

        for msg in &req.messages {
            match msg.role.as_str() {
                "system" => {
                    system_instruction = Some(json!({
                        "parts": [{"text": msg.content}]
                    }));
                }
                role => {
                    contents.push(json!({
                        "role": if role == "assistant" { "model" } else { "user" },
                        "parts": [{"text": msg.content}]
                    }));
                }
            }
        }

        let mut body = json!({
            "contents": contents,
            "generationConfig": {
                "temperature": req.temperature,
                "maxOutputTokens": req.max_tokens,
            }
        });

        if let Some(sys) = system_instruction {
            body["systemInstruction"] = sys;
        }

        body
    }

    fn extract_text_from_response(&self, data: &Value) -> Option<String> {
        let parts = data
            .get("candidates")?
            .as_array()?
            .first()?
            .get("content")?
            .get("parts")?
            .as_array()?;

        let texts: Vec<&str> = parts
            .iter()
            .filter_map(|p| p["text"].as_str())
            .collect();

        if texts.is_empty() {
            None
        } else {
            Some(texts.concat())
        }
    }
}

impl LLMProvider for GoogleProvider {
    async fn stream_chat(
        &self,
        req: &ChatRequest,
        on_delta: &mut (dyn FnMut(&str) -> bool + Send),
    ) -> Result<String, String> {
        if self.api_key.is_empty() {
            return Err("google api_key empty".into());
        }

        let client = self.client(req.timeout_secs.max(self.timeout_secs))?;
        let body = self.build_request_body(req, true);

        // Google uses ?key= parameter for API key in the query string
        let url = format!(
            "{}/models/{}:streamGenerateContent?key={}",
            self.base_url, req.model, self.api_key
        );

        let resp = client
            .post(&url)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("google connect: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(format!(
                "google {status}: {}",
                text.chars().take(300).collect::<String>()
            ));
        }

        // Google's streaming API returns chunks as JSON with text deltas
        let mut full = String::new();
        let mut buf = String::new();
        let mut byte_carry: Vec<u8> = Vec::new();
        let mut byte_stream = resp.bytes_stream();

        while let Some(item) = byte_stream.next().await {
            let bytes = match item {
                Ok(b) => b,
                Err(e) => {
                    if full.is_empty() {
                        return Err(format!("google stream error: {e}"));
                    }
                    break;
                }
            };

            buf.push_str(&crate::utf8_stream::push_utf8_chunk(&mut byte_carry, &bytes));

            while let Some(pos) = buf.find('\n') {
                let line = buf[..pos].trim().to_string();
                buf = buf[pos + 1..].to_string();

                if line.is_empty() {
                    continue;
                }

                // Google returns JSON-per-line (not SSE)
                if let Ok(v) = serde_json::from_str::<Value>(&line) {
                    if let Some(text) = self.extract_text_from_response(&v) {
                        full.push_str(&text);
                        if !on_delta(&text) {
                            return Ok(full);
                        }
                    }
                }
            }
        }

        if full.trim().is_empty() {
            Err("google empty stream".into())
        } else {
            Ok(full)
        }
    }

    async fn chat(&self, req: &ChatRequest) -> Result<String, String> {
        if self.api_key.is_empty() {
            return Err("google api_key empty".into());
        }

        let client = self.client(req.timeout_secs.max(self.timeout_secs))?;
        let body = self.build_request_body(req, false);

        let url = format!(
            "{}/models/{}:generateContent?key={}",
            self.base_url, req.model, self.api_key
        );

        let resp = client
            .post(&url)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("google: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(format!(
                "google {status}: {}",
                text.chars().take(200).collect::<String>()
            ));
        }

        let data: Value = resp
            .json()
            .await
            .map_err(|e| format!("google parse: {e}"))?;

        let text = self
            .extract_text_from_response(&data)
            .ok_or_else(|| "google empty response".to_string())?;

        Ok(text)
    }

    async fn embed(&self, req: &EmbedRequest) -> Result<Vec<f32>, String> {
        let url = format!(
            "{}/models/{}:embedContent?key={}",
            self.base_url,
            req.model.as_deref().unwrap_or("text-embedding-004"),
            self.api_key
        );

        let client = self.client(30)?;
        let body = json!({
            "content": {
                "parts": [{"text": req.input}]
            }
        });

        let resp = client
            .post(&url)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("google embed connect: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(format!(
                "google embed {status}: {}",
                text.chars().take(200).collect::<String>()
            ));
        }

        let data: Value = resp
            .json()
            .await
            .map_err(|e| format!("google embed parse: {e}"))?;

        let arr = data["embedding"]["values"]
            .as_array()
            .ok_or_else(|| "missing embedding.values".to_string())?;

        let vec: Vec<f32> = arr
            .iter()
            .filter_map(|v| v.as_f64().map(|f| f as f32))
            .collect();

        if vec.is_empty() {
            return Err("google embed empty vector".into());
        }
        Ok(vec)
    }
}
