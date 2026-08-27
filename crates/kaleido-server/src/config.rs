//! P1-5: single source of truth for process configuration.
//!
//! All `std::env::var` reads funnel through here. Handlers/stores must take
//! values from `ServerConfig` (via AppState) or `DataRoot`, never re-read env.
//!
//! Precedence for data root: KALEIDO_DATA → ./data (unchanged behavior).
// [P7] env 配置镜像结构，运行时直读 env；预留
#![allow(dead_code)]

use std::path::PathBuf;

#[derive(Debug, Clone)]
pub(crate) struct LlmConfig {
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub model: String,
    pub provider_kind: String,
}

#[derive(Debug, Clone)]
pub(crate) struct ImageGenConfig {
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub model: String,
    pub cf_base_url: Option<String>,
    pub cf_model: Option<String>,
    pub grok2api_base_url: Option<String>,
    pub grok2api_key: Option<String>,
    pub grok2api_model: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ServerConfig {
    pub web_dir: PathBuf,
    pub host: String,
    pub port: u16,
    pub cors_origins: Option<String>,
    pub llm: LlmConfig,
    pub image: ImageGenConfig,
}

impl ServerConfig {
    /// Env read with legacy alias: KALEIDO_* first, MUSEAI_* fallback.
    /// Lets an existing deployment keep its old env file untouched.
    pub fn env_alias(key: &str) -> Option<String> {
        std::env::var(key).ok().or_else(|| {
            key.strip_prefix("KALEIDO_")
                .map(|suffix| std::env::var(format!("MUSEAI_{suffix}")).ok())
                .unwrap_or(None)
        })
    }

    /// Canonical data-root resolution — the ONLY place KALEIDO_DATA is read
    /// (legacy alias: MUSEAI_DATA).
    pub fn data_root() -> PathBuf {
        PathBuf::from(Self::env_alias("KALEIDO_DATA").unwrap_or_else(|| "./data".into()))
    }

    pub fn from_env() -> Self {
        let var = |k: &str| ServerConfig::env_alias(k);
        ServerConfig {
            web_dir: PathBuf::from(
                var("KALEIDO_WEB_DIR").unwrap_or_else(|| "../../web".into()),
            ),
            host: var("KALEIDO_HOST").unwrap_or_else(|| "127.0.0.1".into()),
            port: var("KALEIDO_PORT")
                .and_then(|p| p.parse().ok())
                .unwrap_or(8080),
            cors_origins: var("KALEIDO_CORS_ORIGINS"),
            llm: LlmConfig {
                base_url: var("LLM_BASE_URL"),
                api_key: var("LLM_API_KEY"),
                model: var("LLM_MODEL")
                    .unwrap_or_else(|| "deepseek-v4-flash-free".into()),
                provider_kind: var("KALEIDO_LLM_PROVIDER")
                    .unwrap_or_else(|| "OpenAI".into()),
            },
            image: ImageGenConfig {
                base_url: var("IMAGE_BASE_URL")
                    .or_else(|| Some("http://127.0.0.1:18998/v1".into())),
                api_key: var("IMAGE_API_KEY"),
                model: var("IMAGE_MODEL").unwrap_or_else(|| "cogview-4".into()),
                cf_base_url: var("CF_IMAGE_BASE_URL")
                    .or_else(|| Some("http://127.0.0.1:4001/v1".into())),
                cf_model: var("CF_IMAGE_MODEL")
                    .or_else(|| Some("@cf/black-forest-labs/flux-1-schnell".into())),
                grok2api_base_url: var("GROK2API_IMAGE_BASE_URL"),
                grok2api_key: var("GROK2API_IMAGE_KEY"),
                grok2api_model: var("GROK2API_IMAGE_MODEL"),
            },
        }
    }
}
