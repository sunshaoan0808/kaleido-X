//! # AI Provider Abstraction Layer (Liyuan-inspired)
//!
//! Unified interface across LLM providers.
//!
//! ## Architecture
//!
//! - [`LLMProvider`] trait — common interface for all providers
//! - [`ProviderKind`] enum — type-safe dispatch without `dyn` pointer
//! - [`ProviderConfig`] — configuration for provider selection
//!
//! ## Supported providers
//!
//! - **OpenAI-compatible** — any API that speaks `/chat/completions` (OpenAI, DeepSeek, Groq, etc.)
//! - **Anthropic** — Claude Messages API (with thinking/reasoning support)
//! - **Google** — Gemini Generative AI API

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Provider selection
// ---------------------------------------------------------------------------

/// Which LLM provider to use.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProviderKind {
    /// OpenAI-compatible chat completions (default).
    OpenAI,
    /// Anthropic Claude Messages API.
    Anthropic,
    /// Google Gemini Generative AI API.
    Google,
}

impl Default for ProviderKind {
    fn default() -> Self {
        Self::OpenAI
    }
}

/// Configuration for connecting to a provider.
#[derive(Debug, Clone)]
pub struct ProviderConfig {
    pub kind: ProviderKind,
    /// Base URL (e.g. `https://api.openai.com/v1`)
    pub base_url: String,
    /// API key
    pub api_key: String,
    /// Model name
    pub model: String,
    /// Request timeout
    pub timeout_secs: u64,
}

impl ProviderConfig {
    pub fn is_configured(&self) -> bool {
        !self.base_url.trim().is_empty() && !self.api_key.trim().is_empty()
    }
}

// ---------------------------------------------------------------------------
// Message types
// ---------------------------------------------------------------------------

/// A single chat message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

/// Request to an LLM
#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub temperature: f64,
    pub max_tokens: u32,
    pub timeout_secs: u64,
}

// ---------------------------------------------------------------------------
// Embedding request
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
#[allow(dead_code)] // [P7] provider 内嵌 embedding 预留接口（现走 routes_embed 直连）
pub struct EmbedRequest {
    pub input: String,
    pub model: Option<String>,
}

// ---------------------------------------------------------------------------
// LLMProvider trait — abstract interface
// ---------------------------------------------------------------------------

/// Unified streaming LLM provider.
pub trait LLMProvider: Send + Sync {
    /// Stream a chat completion, calling `on_delta` for each text chunk.
    /// `on_delta` returns `true` to continue, `false` to abort.
    /// Returns the full accumulated text or an error.
    async fn stream_chat(
        &self,
        req: &ChatRequest,
        on_delta: &mut (dyn FnMut(&str) -> bool + Send),
    ) -> Result<String, String>;

    /// Non-streaming chat completion.
    async fn chat(&self, req: &ChatRequest) -> Result<String, String>;

    /// Get text embeddings.
    /// [P7] 生产未走此 trait 方法（embedding 由 routes_embed/embed_local 承担）；保留接口预留。
    #[allow(dead_code)]
    /// Get text embeddings.
    async fn embed(&self, req: &EmbedRequest) -> Result<Vec<f32>, String>;
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

/// Create a provider from config.
pub fn create_provider(config: &ProviderConfig) -> ProviderImpl {
    match config.kind {
        ProviderKind::OpenAI => ProviderImpl::OpenAI(OpenAIProvider::new(config)),
        ProviderKind::Anthropic => ProviderImpl::Anthropic(AnthropicProvider::new(config)),
        ProviderKind::Google => ProviderImpl::Google(GoogleProvider::new(config)),
    }
}

/// Concrete enum wrapper — the only way to hold a provider without `dyn`.
///
/// Implements [`LLMProvider`] by delegating to the inner variant.
#[allow(clippy::large_enum_variant)]
pub enum ProviderImpl {
    OpenAI(OpenAIProvider),
    Anthropic(AnthropicProvider),
    Google(GoogleProvider),
}

impl LLMProvider for ProviderImpl {
    async fn stream_chat(
        &self,
        req: &ChatRequest,
        on_delta: &mut (dyn FnMut(&str) -> bool + Send),
    ) -> Result<String, String> {
        match self {
            Self::OpenAI(p) => p.stream_chat(req, on_delta).await,
            Self::Anthropic(p) => p.stream_chat(req, on_delta).await,
            Self::Google(p) => p.stream_chat(req, on_delta).await,
        }
    }

    async fn chat(&self, req: &ChatRequest) -> Result<String, String> {
        match self {
            Self::OpenAI(p) => p.chat(req).await,
            Self::Anthropic(p) => p.chat(req).await,
            Self::Google(p) => p.chat(req).await,
        }
    }

    async fn embed(&self, req: &EmbedRequest) -> Result<Vec<f32>, String> {
        match self {
            Self::OpenAI(p) => p.embed(req).await,
            Self::Anthropic(p) => p.embed(req).await,
            Self::Google(p) => p.embed(req).await,
        }
    }
}

// ---------------------------------------------------------------------------
// Sub-modules
// ---------------------------------------------------------------------------

mod openai;
mod anthropic;
mod google;

pub use openai::OpenAIProvider;
pub use anthropic::AnthropicProvider;
pub use google::GoogleProvider;
