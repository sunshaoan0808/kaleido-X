//! Fanqie crawler (S5-W4 real fetch).
//!
//! Route (wired in `main.rs`):
//! - `POST /api/v1/crawler/fanqie`
//!
//! Gate: public settings `crawlerEnabled` (default **false** — never auto-enable).
//! Real fetch for fanqienovel.com reader/page URLs + articleId;
//! failures return W19 diagnostic fields: `code`/`stage`/`retryable`/`hint`
//! (optional mock only when `mockOnFailure=true` for gate demos).

use axum::{
    extract::State,
    extract::Path,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Duration as StdDuration;
use kaleido_core::JobStore;
use crate::encoding_sniff::decode_text;
use crate::error_codes::*;

/// Resin proxy pool — auto-rotates IP on each new TCP connection.
/// 凭证从环境变量读取（M-1 修复）；未设置时不启用代理，不再回退硬编码凭据。
fn proxy_url_default() -> String {
    std::env::var("CRAWLER_PROXY_URL").unwrap_or_default()
}
/// Number of chapters to fetch before creating a fresh client (forces new IP).
const PROXY_ROTATION_INTERVAL: usize = 20;

/// Proxy configuration for the crawler.
pub struct ProxyConfig {
    pub proxy_url: String,
    pub rotation_interval: usize,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            proxy_url: proxy_url_default(),
            rotation_interval: PROXY_ROTATION_INTERVAL,
        }
    }
}

use kaleido_core::{is_terminal_job_status, normalize_job_status, JobEvent, JobListFilter};
use crate::{map_core_err, session_from, AppState};

const FIRST_CHAPTER_COUNT: usize = 999;

// ─── Background crawl progress tracker ───────────────────────────────────────

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Progress entry for a background crawl task.
#[derive(Debug, Clone, Serialize)]
pub struct CrawlProgress {
    pub crawl_id: String,
    pub title: String,
    pub total: usize,
    pub fetched: usize,
    pub skipped: usize,
    pub current_chapter: String,
    pub status: String, // "running" | "done" | "error"
    pub output_path: String,
}

/// Global progress store: crawl_id -> progress
static PROGRESS: std::sync::LazyLock<Arc<RwLock<HashMap<String, CrawlProgress>>>> = std::sync::LazyLock::new(|| Arc::new(RwLock::new(HashMap::new())));

fn progress_store() -> Arc<RwLock<HashMap<String, CrawlProgress>>> {
    PROGRESS.clone()
}

/// Generate a crawl ID from title + timestamp.
fn make_crawl_id(title: &str) -> String {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("{}_{}", sanitize_filename(title), ts)
}

/// Update progress in the global store.
async fn update_progress(prog: CrawlProgress) {
    let store = progress_store();
    store.write().await.insert(prog.crawl_id.clone(), prog);
}

/// Body for `POST /api/v1/crawler/fanqie`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FanqieCrawlBody {
    /// Article / page URL (preferred when present).
    #[serde(default)]
    pub url: Option<String>,
    /// Fanqie article / chapter id (alternative to url).
    #[serde(default, alias = "articleId")]
    pub article_id: Option<String>,
    /// `番茄小说-短篇` | `番茄小说-长篇` (default short if reader/id only).
    #[serde(default)]
    pub novel_type: Option<String>,
    /// When true, write markdown under works jail `crawler/`.
    #[serde(default)]
    pub save: Option<bool>,
    /// When true and real fetch fails, return mock payload (gate convenience).
    #[serde(default)]
    pub mock_on_failure: Option<bool>,
}

/// `POST /api/v1/crawler/fanqie`
///
/// - Missing/invalid bearer → 401 via session_from.
/// - `crawlerEnabled != true` → **403** `{ "error": "crawler_disabled" }`.
/// - Enabled → real fetch when URL/id looks like Fanqie; else structured error.
pub async fn fanqie(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };

    let enabled = state
        .app_state
        .load_settings_public()
        .map(|s| s.crawler_enabled)
        .unwrap_or(false);
    if !enabled {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({
                "ok": false,
                "error": "crawler_disabled",
                "code": "CRAWLER_DISABLED",
                "stage": "gate",
                "retryable": false,
                "crawlerEnabled": false,
                "defaultOff": true,
                "hint": crawler_hint("CRAWLER_DISABLED"),
            })),
        )
            .into_response();
    }

    let req: FanqieCrawlBody = match serde_json::from_str(body.trim()) {
        Ok(v) => v,
        Err(e) => {
            return bad_request("CRAWL_INVALID", format!("Invalid JSON: {e}"));
        }
    };

    let mut url = req
        .url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let article_id = req
        .article_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    if url.is_none() && article_id.is_none() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "ok": false,
                "error": "url or article_id required",
                "code": "CRAWLER_BAD_REQUEST",
                "stage": "request",
                "retryable": false,
                "hint": "Provide url or articleId",
            })),
        )
            .into_response();
    }

    // Normalize bare id → book page URL (user provides book_id, not chapter_id).
    if url.is_none() {
        if let Some(id) = article_id.as_ref() {
            url = Some(format!("https://fanqienovel.com/page/{id}"));
        }
    }

    let url = url.unwrap();
    let novel_type = req
        .novel_type
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            if url.contains("/page/") {
                "番茄小说-长篇"
            } else {
                "番茄小说-短篇"
            }
        })
        .to_string();
    let do_save = req.save.unwrap_or(false);
    let mock_on_failure = req.mock_on_failure.unwrap_or(false);

    match crawl_real(&url, &novel_type, &Some(ProxyConfig::default()), do_save).await {
        Ok(result) => {
            let mut saved_path: Option<String> = None;
            if do_save {
                let rel = format!(
                    "crawler/{}.md",
                    sanitize_filename(&result.title)
                );
                let _ = state.works.mkdir(&session.workspace_id, "crawler");
                let md = format!(
                    "# {}\n\n**原文链接**: {}\n\n---\n\n{}",
                    result.title, result.source_url, result.content
                );
                match state
                    .works
                    .write_text(&session.workspace_id, &rel, &md)
                {
                    Ok(f) => saved_path = Some(f.path),
                    Err(e) => {
                        return (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            Json(json!({
                                "ok": false,
                                "error": format!("save failed: {e}"),
                                "code": "CRAWLER_SAVE",
                                "stage": "save",
                                "retryable": true,
                                "title": result.title,
                                "content": result.content,
                                "source": result.source,
                                "hint": crawler_hint("CRAWLER_SAVE"),
                            })),
                        )
                            .into_response();
                    }
                }
            }
            Json(json!({
                "ok": true,
                "title": result.title,
                "content": result.content,
                "source": result.source,
                "url": result.source_url,
                "articleId": result.article_id,
                "chaptersFetched": result.chapters_fetched,
                "savedPath": saved_path,
                "note": result.note,
                "crawlerEnabled": true,
                "meta": result.meta,
            }))
            .into_response()
        }
        Err(err) => {
            let fail = classify_crawl_error(&err, &url);
            if mock_on_failure {
                let title = article_id
                    .as_ref()
                    .map(|id| format!("Mock Fanqie Article {id}"))
                    .unwrap_or_else(|| format!("Mock crawl of {url}"));
                let content = format!(
                    "Mock fallback after real fetch error.\nurl={url}\narticle_id={}\nerror={err}\ncode={}",
                    article_id.as_deref().unwrap_or(""),
                    fail.code
                );
                return Json(json!({
                    "ok": true,
                    "title": title,
                    "content": content,
                    "source": "mock",
                    "url": url,
                    "articleId": article_id,
                    "warning": err,
                    "code": fail.code,
                    "stage": fail.stage,
                    "retryable": fail.retryable,
                    "mockOnFailure": true,
                }))
                .into_response();
            }
            let status = match fail.code {
                "CRAWLER_UNSUPPORTED_HOST" | "CRAWLER_BAD_URL" => StatusCode::BAD_REQUEST,
                "CRAWLER_ANTIBOT" => StatusCode::FORBIDDEN,
                _ => StatusCode::BAD_GATEWAY,
            };
            (status, Json(fail.to_json(&url, &article_id))).into_response()
        }
    }
}

/// Novel metadata extracted from __INITIAL_STATE__.page
#[derive(Debug, Clone, Serialize, Default)]
pub struct NovelMeta {
    pub author: String,
    pub author_id: String,
    pub book_id: String,
    pub abstract_text: String,
    pub word_number: u64,
    pub category: String,
    pub creation_status: i32,
    pub read_count: u64,
    pub cover_url: String,
    pub last_chapter_title: String,
    pub chapter_total: u32,
}

/// Extract novel metadata from page HTML's __INITIAL_STATE__.page
fn extract_novel_meta(html: &str) -> NovelMeta {
    let mut meta = NovelMeta::default();
    // Reuse extract_from_initial_state infrastructure to get the JSON
    let start_marker = "window.__INITIAL_STATE__ =";
    let start_marker2 = "window.__INITIAL_STATE__=";
    let idx = match html.find(start_marker).or_else(|| html.find(start_marker2)) {
        Some(i) => i,
        None => return meta,
    };
    let brace_rel = match html[idx..].find('{') {
        Some(b) => b,
        None => return meta,
    };
    let json_str = &html[idx + brace_rel..];
    let cleaned = replace_undefined(json_str);
    let mut stream = serde_json::Deserializer::from_str(&cleaned).into_iter::<Value>();
    if let Some(Ok(data)) = stream.next() {
        let page = &data["page"];
        meta.author = page["author"].as_str().unwrap_or("").to_string();
        meta.author_id = page["authorId"].as_str().unwrap_or("").to_string();
        meta.book_id = page["bookId"].as_str().unwrap_or("").to_string();
        meta.abstract_text = page["abstract"].as_str().unwrap_or("").to_string();
        meta.word_number = page["wordNumber"].as_u64().unwrap_or(0);
        meta.creation_status = page["creationStatus"].as_i64().unwrap_or(-1) as i32;
        meta.read_count = page["readCount"].as_u64().unwrap_or(0);
        meta.cover_url = page["thumbUri"].as_str().unwrap_or("").to_string();
        meta.last_chapter_title = page["lastChapterTitle"].as_str().unwrap_or("").to_string();
        meta.chapter_total = page["chapterTotal"].as_u64().unwrap_or(0) as u32;
        // category: try categoryV2 array first, fallback to category string
        if let Some(cats) = page["categoryV2"].as_array() {
            let names: Vec<&str> = cats.iter()
                .filter_map(|c| c["Name"].as_str())
                .collect();
            if !names.is_empty() {
                meta.category = names.join(", ");
            }
        }
        if meta.category.is_empty() {
            meta.category = page["category"].as_str().unwrap_or("").to_string();
        }
    }
    meta
}

struct CrawlResult {
    title: String,
    content: String,
    source: String,
    source_url: String,
    article_id: Option<String>,
    chapters_fetched: usize,
    note: String,
    meta: Option<NovelMeta>,
}

/// W19: machine-readable crawl failure (HTTP body extras).
#[derive(Debug, Clone)]
struct CrawlFailure {
    code: &'static str,
    message: String,
    stage: &'static str,
    retryable: bool,
    http_status: Option<u16>,
    host: Option<String>,
}

impl CrawlFailure {
    fn new(code: &'static str, stage: &'static str, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            code,
            message: message.into(),
            stage,
            retryable,
            http_status: None,
            host: None,
        }
    }

    fn with_host(mut self, host: impl Into<String>) -> Self {
        self.host = Some(host.into());
        self
    }

    fn with_http(mut self, status: u16) -> Self {
        self.http_status = Some(status);
        self
    }

    fn to_json(&self, url: &str, article_id: &Option<String>) -> Value {
        let mut v = json!({
            "ok": false,
            "error": self.message,
            "code": self.code,
            "stage": self.stage,
            "retryable": self.retryable,
            "url": url,
            "articleId": article_id,
            "source": "live",
            "crawlerEnabled": true,
            "hint": crawler_hint(self.code),
        });
        if let Some(h) = &self.host {
            v.as_object_mut().unwrap().insert("host".into(), json!(h));
        }
        if let Some(s) = self.http_status {
            v.as_object_mut().unwrap().insert("httpStatus".into(), json!(s));
        }
        v
    }
}

fn crawler_hint(code: &str) -> &'static str {
    match code {
        "CRAWLER_DISABLED" => "PATCH /api/v1/settings {crawlerEnabled:true} then retry; default remains off",
        "CRAWLER_UNSUPPORTED_HOST" => "Only fanqienovel.com / fqnovel.com / fanqie.* reader|page URLs",
        "CRAWLER_BAD_URL" => "Need /reader/{id} or /page/{id} on supported host",
        "CRAWLER_ANTIBOT" => "Anti-bot / captcha page; rotate proxy, cool down, or try later",
        "CRAWLER_HTTP" => "Upstream non-2xx; check proxy and target availability",
        "CRAWLER_NETWORK" => "Transport/proxy failure; verify resin/proxy and network",
        "CRAWLER_EMPTY" => "Parsed zero chapters or all chapter fetches failed",
        "CRAWLER_PARSE" => "HTML parse / decrypt failed",
        "CRAWLER_SAVE" => "Works jail write failed — mkdir crawler/ and check WORKS_* limits",
        _ => "See docs/W19_CRAWLER.md",
    }
}

/// Map legacy free-text crawl errors → stable codes (W19).
fn classify_crawl_error(err: &str, url: &str) -> CrawlFailure {
    let e = err.to_ascii_lowercase();
    let host = host_of(url);
    if e.contains("unsupported host") || e.contains("expected fanqienovel") {
        return CrawlFailure::new(
            "CRAWLER_UNSUPPORTED_HOST",
            "ssrf_guard",
            err,
            false,
        )
        .with_host(host);
    }
    if e.contains("url格式") || e.contains("无法提取小说") || e.contains("reader/") && e.contains("page/") {
        return CrawlFailure::new("CRAWLER_BAD_URL", "url_parse", err, false).with_host(host);
    }
    if e.contains("反爬") || e.contains("验证") || e.contains("滑块") || e.contains("captcha") {
        return CrawlFailure::new("CRAWLER_ANTIBOT", "antibot", err, true).with_host(host);
    }
    if e.contains("状态码") || e.contains("status") {
        let status = err
            .chars()
            .filter(|c| c.is_ascii_digit())
            .collect::<String>()
            .parse::<u16>()
            .ok();
        let mut f = CrawlFailure::new("CRAWLER_HTTP", "http", err, true).with_host(host);
        if let Some(s) = status {
            f = f.with_http(s);
        }
        return f;
    }
    if e.contains("request failed")
        || e.contains("http client")
        || e.contains("bad proxy")
        || e.contains("error sending")
        || e.contains("timed out")
        || e.contains("timeout")
        || e.contains("connection")
    {
        return CrawlFailure::new("CRAWLER_NETWORK", "network", err, true).with_host(host);
    }
    if e.contains("未解析到任何章节")
        || e.contains("全部抓取失败")
        || e.contains("未找到短篇")
    {
        return CrawlFailure::new("CRAWLER_EMPTY", "extract", err, true).with_host(host);
    }
    if e.contains("无法提取章节") || e.contains("decrypt") || e.contains("parse") {
        return CrawlFailure::new("CRAWLER_PARSE", "parse", err, true).with_host(host);
    }
    CrawlFailure::new("CRAWLER_FETCH_FAILED", "fetch", err, true).with_host(host)
}

/// Fanqie allowed hosts (exact match after lowercasing, port-stripped).
const ALLOWED_HOSTS: &[&str] = &[
    "fanqienovel.com",
    "www.fanqienovel.com",
    "fqnovel.com",
    "www.fqnovel.com",
    "fanqie.com",
    "www.fanqie.com",
];

/// User-Agent pool for rotation.
const UA_POOL: &[&str] = &[
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/121.0.0.0 Safari/537.36",
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/121.0.0.0 Safari/537.36",
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.2 Safari/605.1.15",
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:122.0) Gecko/20100101 Firefox/122.0",
];

/// Parse URL and return the lowercase host (no port). Returns None on malformed URLs.
fn url_host(url: &str) -> Option<String> {
    let no_proto = url.split("://").nth(1).unwrap_or(url);
    let host = no_proto.split('/').next().unwrap_or("");
    let host = host.split(':').next().unwrap_or(host);
    let host = host.to_ascii_lowercase();
    if host.is_empty() { None } else { Some(host) }
}

/// Strict SSRF check: parsed host must be in the allowlist.
fn is_fanqie_host(url: &str) -> bool {
    match url_host(url) {
        Some(h) => ALLOWED_HOSTS.contains(&h.as_str()),
        None => false,
    }
}

/// Pick a random UA from the pool.
fn random_ua() -> &'static str {
    let idx = (nanos_rand() as usize) % UA_POOL.len();
    UA_POOL[idx]
}

/// Random sleep between min and max millis.
async fn random_sleep(min_ms: u64, max_ms: u64) {
    let range = max_ms.saturating_sub(min_ms) + 1;
    let ms = min_ms + (nanos_rand() % range);
    tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
}

/// Quick pseudo-random from system nanos (no external rand dep needed).
fn nanos_rand() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64
}

/// Build a reqwest Client, optionally routed through the proxy pool.
/// When `proxy_url` is None/empty, a direct client is returned.
/// Each client gets a random UA from the pool.
fn make_crawler_client(proxy_url: Option<&str>) -> Result<reqwest::Client, String> {
    let mut builder = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .user_agent(random_ua())
        .redirect(reqwest::redirect::Policy::limited(5));
    if let Some(url) = proxy_url {
        if !url.is_empty() {
            builder = builder.proxy(
                reqwest::Proxy::all(url)
                    .map_err(|e| format!("bad proxy url: {e}"))?,
            );
        }
    } else {
        // Fallback to system proxy env vars (HTTP_PROXY / http_proxy)
        for var in &["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy"] {
            if let Ok(val) = std::env::var(var) {
                if !val.is_empty() {
                    if let Ok(proxy) = reqwest::Proxy::all(&val) {
                        builder = builder.proxy(proxy);
                        break;
                    }
                }
            }
        }
    }
    builder.build().map_err(|e| format!("http client: {e}"))
}

/// Build browser-like headers for a request to fanqienovel.com.
fn browser_headers(ua: &str) -> reqwest::header::HeaderMap {
    use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
    let mut h = HeaderMap::new();
    let _ = h.insert(HeaderName::from_static("accept"), HeaderValue::from_static("text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8"));
    let _ = h.insert(HeaderName::from_static("accept-language"), HeaderValue::from_static("zh-CN,zh;q=0.9,en;q=0.8"));
    let _ = h.insert(HeaderName::from_static("accept-encoding"), HeaderValue::from_static("gzip, deflate, br"));
    let _ = h.insert(HeaderName::from_static("cache-control"), HeaderValue::from_static("no-cache"));
    let _ = h.insert(HeaderName::from_static("pragma"), HeaderValue::from_static("no-cache"));
    let _ = h.insert(HeaderName::from_static("sec-ch-ua"), HeaderValue::from_static(r#""Chromium";v="121", "Not A(Brand";v="24""#));
    let _ = h.insert(HeaderName::from_static("sec-ch-ua-mobile"), HeaderValue::from_static("?0"));
    let _ = h.insert(HeaderName::from_static("sec-ch-ua-platform"), HeaderValue::from_static(r#""Windows""#));
    let _ = h.insert(HeaderName::from_static("sec-fetch-dest"), HeaderValue::from_static("document"));
    let _ = h.insert(HeaderName::from_static("sec-fetch-mode"), HeaderValue::from_static("navigate"));
    let _ = h.insert(HeaderName::from_static("sec-fetch-site"), HeaderValue::from_static("none"));
    let _ = h.insert(HeaderName::from_static("sec-fetch-user"), HeaderValue::from_static("?1"));
    let _ = h.insert(HeaderName::from_static("upgrade-insecure-requests"), HeaderValue::from_static("1"));
    if let Ok(v) = HeaderValue::from_str(ua) {
        let _ = h.insert(HeaderName::from_static("user-agent"), v);
    }
    h
}
async fn crawl_real(url: &str, novel_type: &str, proxy_config: &Option<ProxyConfig>, do_save: bool) -> Result<CrawlResult, String> {
    // Strict SSRF guard: parsed host must be in allowlist (not contains()).
    if !is_fanqie_host(url) {
        return Err(format!(
            "unsupported host (expected fanqienovel.com): {}",
            url_host(url).unwrap_or_else(|| url.to_string())
        ));
    }

    let proxy_url = proxy_config.as_ref().map(|c| c.proxy_url.as_str());
    let rotation_interval = proxy_config.as_ref().map_or(0, |c| c.rotation_interval);
    let mut client = make_crawler_client(proxy_url)?;

    // Reader single chapter
    if let Some(chapter_id) = capture_after(url, "/reader/") {
        let (title, content) = fetch_chapter_content(&client, &chapter_id, None).await?;
        let decrypted = decrypt_text(&content);
        let cleaned = clean_html_content(&decrypted);
        let novel_name = if title.is_empty() {
            format!("短篇小说_{chapter_id}")
        } else {
            title
        };
        return Ok(CrawlResult {
            title: novel_name,
            content: cleaned,
            source: "live".into(),
            source_url: url.to_string(),
            article_id: Some(chapter_id),
            chapters_fetched: 1,
            note: "single reader chapter".into(),
            meta: None,
        });
    }

    // Book page
    let book_id = capture_after(url, "/page/").ok_or_else(|| {
        "URL格式不正确，无法提取小说ID或章节ID（需要 /reader/{{id}} 或 /page/{{id}}）".to_string()
    })?;

    let response = client
        .get(url)
        .headers(browser_headers(random_ua()))
        .send()
        .await
        .map_err(|e| format!("Request failed: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("请求失败，状态码: {}", response.status()));
    }
    let html = response
        .text()
        .await
        .map_err(|e| format!("Failed to read response: {e}"))?;

    check_anti_bot(&html)?;

    // Extract novel metadata from __INITIAL_STATE__.page
    let novel_meta = extract_novel_meta(&html);

    let novel_name = extract_h1_title(&html)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("未命名小说_{book_id}"));

    if novel_type.contains("短") {
        // Fetch ALL reader chapters for short novels, not just the first.
        let all_ids = extract_reader_ids(&html);
        if all_ids.is_empty() {
            return Err("未找到短篇小说阅读链接。".to_string());
        }
        let mut parts: Vec<String> = Vec::new();
        let mut fetched = 0usize;
        for (ci, chapter_id) in all_ids.iter().enumerate() {
            match fetch_chapter_content(&client, chapter_id, Some(&book_id)).await {
                Ok((title, content)) => {
                    let decrypted = decrypt_text(&content);
                    let cleaned = clean_html_content(&decrypted);
                    if cleaned.is_empty() || cleaned.contains("暂无内容") {
                        continue;
                    }
                    let ch_title = if title.is_empty() {
                        format!("第{}章", ci + 1)
                    } else {
                        title
                    };
                    parts.push(format!(
                        "# {}\n\n**章节链接**: https://fanqienovel.com/reader/{}\n\n---\n\n{}",
                        ch_title, chapter_id, cleaned
                    ));
                    fetched += 1;
                }
                Err(_) => continue,
            }
            if ci + 1 < all_ids.len() {
                random_sleep(800, 2000).await;
            }
        }
        if parts.is_empty() {
            return Err("短篇小说所有章节抓取失败。".into());
        }
        let name = novel_name;
        return Ok(CrawlResult {
            title: name,
            content: parts.join("\n\n"),
            source: "live".into(),
            source_url: url.to_string(),
            article_id: Some(book_id),
            chapters_fetched: fetched,
            note: format!("short novel {} chapters", fetched),
            meta: Some(novel_meta),
        });
    }

    // Long novel: catalog + first N chapters
    let mut chapters = extract_chapter_items(&html);
    if chapters.is_empty() {
        for id in extract_reader_ids(&html) {
            if !chapters.iter().any(|(_, cid)| cid == &id) {
                chapters.push((format!("章节 {id}"), id));
            }
        }
    }
    if chapters.is_empty() {
        return Err("未解析到任何章节目录。".into());
    }

    let mut catalog = format!(
        "# 《{}》章节目录\n\n**全部章节**: {} 章\n\n---\n\n",
        novel_name,
        chapters.len()
    );
    for (i, (title, _)) in chapters.iter().enumerate() {
        catalog.push_str(&format!("- 第{}章：{}\n", i + 1, title));
    }

    let target: Vec<_> = chapters.into_iter().take(FIRST_CHAPTER_COUNT).collect();
    // Background crawl for long novels with save:true
    if do_save && target.len() > 50 {
        let bg_title = novel_name.to_string();
        let bg_url = url.to_string();
        let _bg_book_id = book_id.clone();
        let bg_proxy_string = proxy_config.as_ref().map(|c| c.proxy_url.clone());
        let bg_rotation = proxy_config.as_ref().map_or(0, |c| c.rotation_interval);
        let bg_target = target.clone();
        let _bg_total = bg_target.len();
        // Output path: novel_workspace/<title>_<bookId>_<timestamp>.md
        let ws_dir = std::path::PathBuf::from("novel_workspace");
        let _ = std::fs::create_dir_all(&ws_dir);
        let ts = chrono::Local::now().format("%Y%m%d_%H%M%S");
        let bid_short = book_id.chars().rev().take(8).collect::<Vec<_>>().into_iter().rev().collect::<String>();
        let fname = format!("{}_{}_{}.md", sanitize_filename(&bg_title), bid_short, ts);
        let out_path = ws_dir.join(&fname);
        let out_str = out_path.to_string_lossy().to_string();
        let out_str2 = out_str.clone();

        // Register progress tracking
        let crawl_id = make_crawl_id(&bg_title);
        let bg_total = bg_target.len();
        update_progress(CrawlProgress {
            crawl_id: crawl_id.clone(),
            title: bg_title.clone(),
            total: bg_total,
            fetched: 0,
            skipped: 0,
            current_chapter: String::new(),
            status: "running".into(),
            output_path: out_str.clone(),
        }).await;

        // Write initial catalog
        let initial = format!("# {}\n\n**URL**: {}\n\n# 章节目录\n\n{}\n\n---\n\n*正在后台爬取...*\n", bg_title, bg_url, catalog);
        let _ = std::fs::write(&out_path, &initial);

        let bg_book_id = book_id.clone();
        // Spawn background task
        tokio::spawn(async move {
            let bg_proxy: Option<&str> = bg_proxy_string.as_deref();
            let mut bg_client = match make_crawler_client(bg_proxy) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("[crawler] bg client: {e}");
                    return;
                }
            };
            // Extract cover from first chapter page (each reader page has og:image)
            let bg_cover_url = if let Some((_, first_cid)) = bg_target.first() {
                let reader_url = format!("https://fanqienovel.com/reader/{}", first_cid);
                match bg_client.get(&reader_url).send().await {
                    Ok(resp) if resp.status().is_success() => {
                        match resp.text().await {
                            Ok(ch_html) => extract_cover_url(&ch_html),
                            Err(_) => None,
                        }
                    }
                    _ => None,
                }
            } else {
                None
            };

            // Resume / incremental: extract already-fetched chapter IDs from existing file
            let existing_content = std::fs::read_to_string(&out_path).unwrap_or_default();
            let mut already_fetched: std::collections::HashSet<String> = std::collections::HashSet::new();
            for line in existing_content.lines() {
                if line.contains("**链接**: https://fanqienovel.com/reader/") {
                    if let Some(id) = line.split("reader/").nth(1) {
                        let cid: String = id.chars().take_while(|c| c.is_ascii_digit()).collect();
                        if !cid.is_empty() {
                            already_fetched.insert(cid);
                        }
                    }
                }
            }
            if !already_fetched.is_empty() {
                tracing::info!("[crawler] resume: {} chapters already fetched, skipping", already_fetched.len());
            }

            // Filter target to only un-fetched chapters (incremental update)
            let bg_to_fetch: Vec<&(String, String)> = bg_target.iter()
                .filter(|(_, cid)| !already_fetched.contains(cid))
                .collect();
            let bg_fetch_total = bg_to_fetch.len();

            let mut bg_ok = 0usize;
            let mut bg_skip = 0usize;

            for (bi, (btitle, bcid)) in bg_to_fetch.iter().enumerate() {
                // Update progress: current chapter
                update_progress(CrawlProgress {
                    crawl_id: crawl_id.clone(),
                    title: bg_title.clone(),
                    total: bg_fetch_total,
                    fetched: bg_ok,
                    skipped: bg_skip,
                    current_chapter: btitle.clone(),
                    status: "running".into(),
                    output_path: out_str2.clone(),
                }).await;

                // Rotate proxy at interval
                if bg_rotation > 0 && bi > 0 && bi % bg_rotation == 0 {
                    if let Ok(c) = make_crawler_client(bg_proxy) {
                        bg_client = c;
                    }
                    random_sleep(1500, 3000).await;
                }

                match fetch_chapter_content(&bg_client, bcid, Some(&bg_book_id)).await {
                    Ok((parsed_title, html)) => {
                        let ftitle = if parsed_title.is_empty() {
                            btitle.clone()
                        } else {
                            parsed_title
                        };
                        let dec = decrypt_text(&html);
                        let cl = clean_html_content(&dec);
                        if cl.is_empty() || cl.contains("暂无内容") {
                            bg_skip += 1;
                            continue;
                        }
                        let ch = format!(
                            "\n\n# {}\n\n**链接**: https://fanqienovel.com/reader/{}\n\n---\n\n{}\n",
                            ftitle, bcid, cl
                        );
                        // Append to file
                        if let Ok(existing) = std::fs::read_to_string(&out_path) {
                            let _ = std::fs::write(&out_path, format!("{}{}", existing, ch));
                        }
                        bg_ok += 1;
                    }
                    Err(_) => {
                        bg_skip += 1;
                    }
                }

                if bi + 1 < bg_fetch_total {
                    // Randomized delay: 800-2000ms, with 10% chance of a longer 3-6s pause
                    let n = nanos_rand();
                    if n % 10 == 0 {
                        random_sleep(3000, 6000).await;
                    } else {
                        random_sleep(800, 2000).await;
                    }
                }
            }

            // Completion footer
            if let Ok(existing) = std::fs::read_to_string(&out_path) {
                let footer = format!("

---

*后台爬取完成: {} 章成功, {} 章跳过*", bg_ok, bg_skip);
                let _ = std::fs::write(&out_path, format!("{}{}", existing, footer));
            }

            // Mark progress as done
            update_progress(CrawlProgress {
                crawl_id: crawl_id.clone(),
                title: bg_title.clone(),
                total: bg_fetch_total,
                fetched: bg_ok,
                skipped: bg_skip,
                current_chapter: String::new(),
                status: "done".into(),
                output_path: out_str2.clone(),
            }).await;

            // Download and save cover image
            if let Some(ref cover_url) = bg_cover_url {
                match bg_client.get(cover_url).send().await {
                    Ok(resp) if resp.status().is_success() => {
                        let ct = resp.headers().get("content-type")
                            .and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
                        let bytes = resp.bytes().await.unwrap_or_default();
                        if bytes.len() > 1000 {
                            let _ = std::fs::create_dir_all(&shelf_covers_dir());
                            let slug = shelf_slug(&bg_title);
                            let ext = if ct.contains("png") { "png" }
                                else if ct.contains("webp") { "webp" }
                                else { "jpg" };
                            let cp = shelf_covers_dir().join(format!("{}.{}", slug, ext));
                            let _ = std::fs::write(&cp, &bytes);
                            tracing::info!("[crawler] cover saved: {:?} ({} bytes, ct={})", cp, bytes.len(), ct);
                        }
                    }
                    Ok(resp) => eprintln!("[crawler] cover fetch failed: HTTP {}", resp.status()),
                    Err(e) => eprintln!("[crawler] cover fetch error: {}", e),
                }
            }

            tracing::info!("[crawler] bg crawl done: {} ok, {} skip -> {}", bg_ok, bg_skip, out_str2);
        });
    }

    let mut bodies: Vec<String> = Vec::new();
    let mut success = 0usize;
    for (i, (title, chapter_id)) in target.into_iter().enumerate() {
        // Rotate proxy every N chapters to avoid IP-based rate limiting.
        if rotation_interval > 0 && i > 0 && i % rotation_interval == 0 {
            client = make_crawler_client(proxy_url)?;
            random_sleep(1500, 3000).await;
        }
        match fetch_chapter_content(&client, &chapter_id, Some(&book_id)).await {
            Ok((parsed_title, content)) => {
                let final_title = if parsed_title.is_empty() {
                    title
                } else {
                    parsed_title
                };
                let decrypted = decrypt_text(&content);
                let cleaned = clean_html_content(&decrypted);
                if cleaned.is_empty() || cleaned.contains("暂无内容") {
                    continue;
                }
                bodies.push(format!(
                    "# {}\n\n**章节链接**: https://fanqienovel.com/reader/{}\n\n---\n\n{}\n",
                    final_title, chapter_id, cleaned
                ));
                success += 1;
            }
            Err(_) => {
                // skip individual chapter
            }
        }
        if i + 1 < FIRST_CHAPTER_COUNT {
            // Randomized delay: 800-2000ms, with 10% chance of a longer 3-6s pause
            if nanos_rand() % 10 == 0 {
                random_sleep(3000, 6000).await;
            } else {
                random_sleep(800, 2000).await;
            }
        }
    }

    if success == 0 {
        return Err("章节内容全部抓取失败（可能触发反爬）。".into());
    }

    let content = format!("{catalog}\n\n{}", bodies.join("\n\n"));
    Ok(CrawlResult {
        title: novel_name,
        content,
        source: "live".into(),
        source_url: url.to_string(),
        article_id: Some(book_id.clone()),
        chapters_fetched: success,
        note: format!("long novel first {success} chapters + catalog"),
        meta: Some(novel_meta),
    })
}

/// Max retries for transient failures (429/503/network).
const FETCH_MAX_RETRIES: usize = 2;

async fn fetch_chapter_content(
    client: &reqwest::Client,
    chapter_id: &str,
    book_id: Option<&str>,
) -> Result<(String, String), String> {
    // Try local FQ signing service first (decrypted content, no PUA charset needed)
    let fq_base = "http://127.0.0.1:9999";
    let fq_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .no_proxy()
        .build()
        .map_err(|e| format!("fq client: {e}"))?;

    if let Some(bid) = book_id {
        let ch_url = format!("{}/chapter/{}/{}", fq_base, bid, chapter_id);
        if let Ok(resp) = fq_client.get(&ch_url).send().await {
            if resp.status().is_success() {
                if let Ok(text) = resp.text().await {
                    if let Ok(body) = serde_json::from_str::<Value>(&text) {
                        let content = body["data"]["txtContent"].as_str()
                            .or_else(|| body["data"]["content"].as_str())
                            .unwrap_or("");
                        let title = body["data"]["title"].as_str().unwrap_or("");
                        if !content.is_empty() {
                            return Ok((title.to_string(), content.to_string()));
                        }
                    }
                }
            }
        }
    }

    // Fallback: original page parsing
    let url = format!("https://fanqienovel.com/reader/{chapter_id}");
    let ua = random_ua();
    let headers = browser_headers(ua);

    let mut last_err = String::new();
    for attempt in 0..=FETCH_MAX_RETRIES {
        if attempt > 0 {
            // Exponential backoff: 2s, 4s
            let backoff = 2000u64 * (1u64 << (attempt - 1));
            tokio::time::sleep(std::time::Duration::from_millis(backoff)).await;
        }

        let response = match client
            .get(&url)
            .headers(headers.clone())
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                last_err = format!("Req err: {e}");
                continue; // retry on network error
            }
        };

        // Retry on 429 / 503
        let status = response.status();
        if status.as_u16() == 429 || status.as_u16() == 503 {
            last_err = format!("HTTP {} (rate limited)", status);
            continue;
        }
        if !status.is_success() {
            return Err(format!("HTTP {}", status));
        }

        let html = match response.text().await {
            Ok(t) => t,
            Err(e) => {
                last_err = format!("Read err: {e}");
                continue;
            }
        };

        // Anti-bot check - retry once, then fail
        if let Err(e) = check_anti_bot(&html) {
            last_err = e;
            if attempt < FETCH_MAX_RETRIES {
                continue;
            }
            return Err(last_err);
        }

        // Primary: parse __INITIAL_STATE__ via serde
        if let Some((title, content)) = extract_from_initial_state(&html) {
            if !content.is_empty() {
                return Ok((title, content));
            }
        }

        // Fallback 1: manual brace-matching extraction
        if let Some((title, content)) = extract_from_state_fallback(&html) {
            if !content.is_empty() {
                return Ok((title, content));
            }
        }

        // Fallback 2: try the reader/full API endpoint
        let api_url = format!("https://fanqienovel.com/api/reader/full?itemId={chapter_id}");
        if let Ok(api_resp) = client.get(&api_url).headers(headers.clone()).send().await {
            if api_resp.status().is_success() {
                if let Ok(data) = api_resp.json::<Value>().await {
                    let content = data["data"]["chapterData"]["content"]
                        .as_str()
                        .unwrap_or("")
                        .to_string();
                    let title = data["data"]["chapterData"]["title"]
                        .as_str()
                        .unwrap_or("")
                        .to_string();
                    if !content.is_empty() {
                        return Ok((title, content));
                    }
                }
            }
        }

        last_err = "无法提取章节内容".to_string();
    }

    Err(last_err)
}
fn extract_from_state_fallback(html: &str) -> Option<(String, String)> {
    let marker = "window.__INITIAL_STATE__=";
    let idx = html.find(marker)?;
    let rest = &html[idx + marker.len()..];
    let brace_start = rest.find('{')?;
    let mut depth = 0u32;
    let mut brace_end = 0;
    for (i, ch) in rest[brace_start..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    brace_end = brace_start + i + 1;
                    break;
                }
            }
            _ => {}
        }
    }
    if brace_end == 0 {
        return None;
    }
    let json_section = &rest[..brace_end];

    let content_key = r#""content":""#;
    let ck_pos = json_section.find(content_key)?;
    let vstart = ck_pos + content_key.len();
    let bytes = json_section.as_bytes();
    let mut i = vstart;
    let mut raw: Vec<u8> = Vec::new();
    while i < bytes.len() && bytes[i] != b'"' {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            match bytes[i + 1] {
                b'\\' => raw.push(b'\\'),
                b'"' => raw.push(b'"'),
                b'n' => raw.push(b'\n'),
                b'r' => raw.push(b'\r'),
                b't' => raw.push(b'\t'),
                b'u' => {
                    let hex_start = i + 2;
                    if hex_start + 4 <= bytes.len() {
                        let hex = &bytes[hex_start..hex_start + 4];
                        if let Ok(hex_str) = std::str::from_utf8(hex) {
                            if let Ok(code) = u32::from_str_radix(hex_str, 16) {
                                if let Some(c) = char::from_u32(code) {
                                    let mut buf = [0u8; 4];
                                    let encoded = c.encode_utf8(&mut buf);
                                    raw.extend_from_slice(encoded.as_bytes());
                                }
                            }
                        }
                        i += 5;
                    } else {
                        i += 1;
                    }
                    continue;
                }
                _ => {
                    raw.push(bytes[i + 1]);
                }
            }
            i += 2;
        } else {
            raw.push(bytes[i]);
            i += 1;
        }
    }
    if raw.is_empty() {
        return None;
    }
    let val = String::from_utf8(raw).unwrap_or_default();
    if val.is_empty() {
        return None;
    }

    let title_key = r#""title":""#;
    let title = if let Some(ts) = json_section.find(title_key) {
        let tv_start = ts + title_key.len();
        let mut ti = tv_start;
        let mut t_raw: Vec<u8> = Vec::new();
        while ti < bytes.len() && bytes[ti] != b'"' {
            if bytes[ti] == b'\\' && ti + 1 < bytes.len() {
                match bytes[ti + 1] {
                    b'\\' => t_raw.push(b'\\'),
                    b'"' => t_raw.push(b'"'),
                    b'n' => t_raw.push(b'\n'),
                    b'r' => t_raw.push(b'\r'),
                    b't' => t_raw.push(b'\t'),
                    b'u' => {
                        let hex_start = ti + 2;
                        if hex_start + 4 <= bytes.len() {
                            let hex = &bytes[hex_start..hex_start + 4];
                            if let Ok(hex_str) = std::str::from_utf8(hex) {
                                if let Ok(code) = u32::from_str_radix(hex_str, 16) {
                                    if let Some(c) = char::from_u32(code) {
                                        let mut buf = [0u8; 4];
                                        let encoded = c.encode_utf8(&mut buf);
                                        t_raw.extend_from_slice(encoded.as_bytes());
                                    }
                                }
                            }
                            ti += 5;
                        } else {
                            ti += 1;
                        }
                        continue;
                    }
                    _ => {
                        t_raw.push(bytes[ti + 1]);
                    }
                }
                ti += 2;
            } else {
                t_raw.push(bytes[ti]);
                ti += 1;
            }
        }
        String::from_utf8(t_raw).unwrap_or_default()
    } else {
        String::new()
    };

    Some((title, val))
}


fn check_anti_bot(html: &str) -> Result<(), String> {
    if html.len() < 5000 || !html.contains("window.__INITIAL_STATE__") {
        // API-only pages may be shorter; only hard-fail if captcha markers present.
        if let Some(title) = extract_html_title(html) {
            let t = title.to_lowercase();
            if t.contains("验证") || t.contains("captcha") || (t.contains("安全") && t.contains("验证")) {
                return Err("触发番茄小说反爬验证拦截（滑块/验证页）。".into());
            }
        }
        if html.len() < 800 && !html.contains("chapterData") {
            return Err("触发番茄小说反爬验证拦截，页面内容异常。".into());
        }
    }
    if let Some(title) = extract_html_title(html) {
        let t = title.to_lowercase();
        if t.contains("验证") || t.contains("captcha") || (t.contains("安全") && t.contains("验证")) {
            return Err(
                "触发番茄小说反爬验证拦截，请在浏览器中访问该链接进行验证后重试。".into(),
            );
        }
    }
    Ok(())
}

fn extract_from_initial_state(html: &str) -> Option<(String, String)> {
    let start_marker = "window.__INITIAL_STATE__ =";
    let start_marker2 = "window.__INITIAL_STATE__=";
    let idx = html.find(start_marker).or_else(|| html.find(start_marker2))?;
    let brace_rel = html[idx..].find('{')?;
    let json_str = &html[idx + brace_rel..];
    // Replace bare `undefined` tokens so serde can parse a prefix.
    let cleaned = replace_undefined(json_str);
    // Incremental parse first value
    let mut stream = serde_json::Deserializer::from_str(&cleaned).into_iter::<Value>();
    let data = stream.next()?.ok()?;
    let content = data
        .pointer("/reader/chapterData/content")
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string();
    let title = data
        .pointer("/reader/chapterData/title")
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string();
    Some((title, content))
}

fn replace_undefined(s: &str) -> String {
    // Cheap substitute for r":\s*undefined\b"
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b':' {
            out.push(':');
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                out.push(bytes[i] as char);
                i += 1;
            }
            if i + 9 <= bytes.len() && &bytes[i..i + 9] == b"undefined" {
                let end_ok = i + 9 == bytes.len()
                    || !(bytes[i + 9].is_ascii_alphanumeric() || bytes[i + 9] == b'_');
                if end_ok {
                    out.push_str("null");
                    i += 9;
                    continue;
                }
            }
            continue;
        }
        let c = s[i..].chars().next().unwrap_or('\u{FFFD}');
        out.push(c);
        i += c.len_utf8();
    }
    out
}

fn extract_html_title(html: &str) -> Option<String> {
    let start = html.find("<title>")? + 7;
    let end_rel = html[start..].find("</title>")?;
    Some(html[start..start + end_rel].trim().to_string())
}

fn extract_h1_title(html: &str) -> Option<String> {
    // Prefer <h1>...</h1>
    if let Some(start) = html.find("<h1") {
        if let Some(gt) = html[start..].find('>') {
            let content_start = start + gt + 1;
            if let Some(end_rel) = html[content_start..].find("</h1>") {
                let raw = &html[content_start..content_start + end_rel];
                let t = strip_tags(raw).trim().to_string();
                if !t.is_empty() {
                    return Some(t);
                }
            }
        }
    }
    extract_html_title(html)
}

fn extract_reader_ids(html: &str) -> Vec<String> {
    let mut ids = Vec::new();
    let needle = "/reader/";
    let mut search = html;
    while let Some(idx) = search.find(needle) {
        let after = &search[idx + needle.len()..];
        let id: String = after
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if !id.is_empty() && !ids.contains(&id) {
            ids.push(id);
        }
        search = &after[after
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .count()
            .min(after.len())..];
        if search.is_empty() {
            break;
        }
        // advance at least 1 to avoid infinite loop
        if search == after {
            search = &after[1.min(after.len())..];
        }
    }
    ids
}

fn extract_chapter_items(html: &str) -> Vec<(String, String)> {
    // Look for anchors with /reader/ and nearby text.
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(a_idx) = rest.find("<a") {
        let chunk = &rest[a_idx..];
        let Some(tag_end) = chunk.find('>') else {
            rest = &rest[a_idx + 2..];
            continue;
        };
        let open = &chunk[..=tag_end];
        let after_open = &chunk[tag_end + 1..];
        let Some(close_rel) = after_open.find("</a>") else {
            rest = &rest[a_idx + 2..];
            continue;
        };
        let inner = &after_open[..close_rel];
        rest = &after_open[close_rel + 4..];

        if !open.contains("/reader/") {
            continue;
        }
        // href
        let href = attr_value(open, "href").unwrap_or_default();
        let id = capture_after(&href, "/reader/").unwrap_or_default();
        if id.is_empty() {
            continue;
        }
        let title = strip_tags(inner).trim().to_string();
        if title.contains("最近更新") || title.contains("开始阅读") {
            continue;
        }
        if title.is_empty() {
            continue;
        }
        if !out.iter().any(|(_, cid)| cid == &id) {
            out.push((title, id));
        }
    }
    out
}

fn attr_value(tag: &str, name: &str) -> Option<String> {
    let patterns = [
        format!("{name}=\""),
        format!("{name}='"),
    ];
    for p in patterns {
        if let Some(idx) = tag.find(&p) {
            let start = idx + p.len();
            let quote = p.chars().last()?;
            let end_rel = tag[start..].find(quote)?;
            return Some(tag[start..start + end_rel].to_string());
        }
    }
    None
}

fn capture_after(s: &str, marker: &str) -> Option<String> {
    let idx = s.find(marker)?;
    let after = &s[idx + marker.len()..];
    let id: String = after
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    if id.is_empty() {
        None
    } else {
        Some(id)
    }
}

fn host_of(url: &str) -> String {
    url.split("://")
        .nth(1)
        .unwrap_or(url)
        .split('/')
        .next()
        .unwrap_or(url)
        .to_string()
}

fn sanitize_filename(filename: &str) -> String {
    let mut safe = filename.to_string();
    for c in ['<', '>', ':', '"', '/', '\\', '|', '?', '*'] {
        safe = safe.replace(c, "_");
    }
    let t = safe.trim();
    if t.is_empty() {
        "untitled".into()
    } else {
        t.chars().take(80).collect()
    }
}

fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for ch in s.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out.replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
}

fn clean_html_content(content: &str) -> String {
    if content.is_empty() {
        return String::new();
    }
    // Prefer paragraph-ish split on <p> / <div>
    let mut paragraphs = Vec::new();
    let lower = content.to_ascii_lowercase();
    if lower.contains("<p") || lower.contains("<div") {
        let mut rest = content;
        while let Some(idx) = rest.find('<') {
            let after = &rest[idx..];
            let tag_name = after
                .chars()
                .skip(1)
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect::<String>()
                .to_ascii_lowercase();
            if tag_name == "p" || tag_name == "div" {
                if let Some(gt) = after.find('>') {
                    let body_start = idx + gt + 1;
                    let close = format!("</{tag_name}");
                    if let Some(end_rel) = rest[body_start..].to_ascii_lowercase().find(&close) {
                        let raw = &rest[body_start..body_start + end_rel];
                        let txt = strip_tags(raw).trim().to_string();
                        if !txt.is_empty() {
                            paragraphs.push(txt);
                        }
                        rest = &rest[body_start + end_rel + close.len()..];
                        continue;
                    }
                }
            }
            // advance
            rest = &rest[idx + 1..];
            if rest.is_empty() {
                break;
            }
        }
    }
    if paragraphs.is_empty() {
        let stripped = strip_tags(content);
        for line in stripped.split('\n') {
            let t = line.trim();
            if !t.is_empty() {
                paragraphs.push(t.to_string());
            }
        }
    }
    paragraphs.join("\n\n")
}

/// Fanqie PUA decrypt configuration (loaded from data/fanqie_charset.json, fallback to hardcoded).
#[derive(Debug, Clone)]
struct PuaConfig {
    code: [[u32; 2]; 2],
    charset: [Vec<char>; 2],
}

/// Built-in default charset (last resort if config file missing).
fn builtin_pua_config() -> PuaConfig {
    PuaConfig {
        code: [[58344, 58715], [58345, 58716]],
        charset: [
            "D在主特家军然表场4要只v和?6别还g现儿岁??此象月3出战工相o男直失世F都平文什VO将真T那当?会立些u是十张学气大爱两命全后东性通被1它乐接而感车山公了常以何可话先pi叫轻M士w着变尔快l个说少色里安花远7难师放t报认面道S?克地度I好机U民写把万同水新没书电吃像斯5为y白几日教看但第加候作上拉住有法r事应位利你声身国问马女他Y比父xAHNsX边美对所金活回意到z从j知又内因点Q三定8Rb正或夫向德听更?得告并本q过记L让打f人就者去原满体做经K走如孩cG给使物?最笑部?员等受k行一条果动光门头见往自成处于名其发总母的死手入路进心来h时力多开已许d至由很界n小与Z想代么分生口再妈望次西风種带J?实情才这?E我神格长觉间年眼无不亲关结0友信下却重己老2音字m呢明之前高PB目太e9起稜她也W用方子英每理便四数期中C外样a海们任"
                .chars().collect(),
            "s?作口在他能并B士4U克才正们字声高全尔活者动其主报多望放hw次年?中3特于十入要男同G面分方K什再教本己结1等世N?说gu期Z外美M行给9文将两许张友0英应向像此白安少何打气常定间花见孩它直风数使道第水已女山解dP的通关性叫儿L妈问回神来S 四望前国些OvlA心平自无军光代是好却c得种就意先立z子过Yj表 么所接了名金受J满眼没部那m每车度可R斯经现门明V如走命y6E战很上f月西7长夫想话变海机x到W一成生信笑b父开内东马日小而后带以三几为认X死员目位之学远人音呢我q乐象重对个被别F也书稜D写还因家发时i或住德当ol比觉然吃去公a老亲情体太b万C电理?失力更拉物着原s工实色感记看出相路大你候2和?与p样新只便最不进Tr做格母总爱身师轻知往加从?天eH?听场由快边让把任8条头事至起点真手这难都界用法n处下又Q告地5kt岁有会果利民"
                .chars().collect(),
        ],
    }
}

/// Global PUA config: loaded once from data/fanqie_charset.json, cached forever.
static PUA_CONFIG: std::sync::OnceLock<PuaConfig> = std::sync::OnceLock::new();

fn get_pua_config() -> &'static PuaConfig {
    PUA_CONFIG.get_or_init(|| {
        let path = std::path::Path::new("data/fanqie_charset.json");
        match std::fs::read_to_string(path) {
            Ok(json_str) => {
                match serde_json::from_str::<Value>(&json_str) {
                    Ok(data) => {
                        let code = [
                            [
                                data["code_ranges"][0][0].as_u64().unwrap_or(58344) as u32,
                                data["code_ranges"][0][1].as_u64().unwrap_or(58715) as u32,
                            ],
                            [
                                data["code_ranges"][1][0].as_u64().unwrap_or(58345) as u32,
                                data["code_ranges"][1][1].as_u64().unwrap_or(58716) as u32,
                            ],
                        ];
                        let mode0: Vec<char> = data["mode0"]
                            .as_array()
                            .map(|a| a.iter().filter_map(|v| v.as_str().and_then(|s| s.chars().next())).collect())
                            .unwrap_or_default();
                        let mode1: Vec<char> = data["mode1"]
                            .as_array()
                            .map(|a| a.iter().filter_map(|v| v.as_str().and_then(|s| s.chars().next())).collect())
                            .unwrap_or_default();
                        if mode0.is_empty() || mode1.is_empty() {
                            tracing::warn!("[crawler] fanqie_charset.json has empty charset, using builtin");
                            return builtin_pua_config();
                        }
                        tracing::info!("[crawler] loaded fanqie_charset.json: mode0={} mode1={}", mode0.len(), mode1.len());
                        PuaConfig { code, charset: [mode0, mode1] }
                    }
                    Err(e) => {
                        tracing::warn!("[crawler] fanqie_charset.json parse error: {e}, using builtin");
                        builtin_pua_config()
                    }
                }
            }
            Err(_) => {
                tracing::info!("[crawler] fanqie_charset.json not found, using builtin charset");
                builtin_pua_config()
            }
        }
    })
}

/// Fanqie PUA decrypt: maps PUA codepoints to real chars using config charset.
fn decrypt_text(text: &str) -> String {
    let cfg = get_pua_config();
    let code = cfg.code;
    let charset = &cfg.charset;

    let has_pua = text.chars().any(|c| {
        let u = c as u32;
        (58344..=58716).contains(&u)
    });
    if !has_pua {
        return text.to_string();
    }

    let mut mode_results = Vec::new();
    for mode in 0..2 {
        let mut q_count = 0;
        let mut decoded_chars = String::new();
        for char in text.chars() {
            let uni = char as u32;
            if uni >= code[mode][0] && uni <= code[mode][1] {
                let bias = (uni - code[mode][0]) as usize;
                if bias < charset[mode].len() {
                    let mapped_char = charset[mode][bias];
                    if mapped_char == '?' {
                        q_count += 1;
                        decoded_chars.push(char);
                    } else {
                        decoded_chars.push(mapped_char);
                    }
                } else {
                    q_count += 1;
                    decoded_chars.push(char);
                }
            } else {
                decoded_chars.push(char);
            }
        }
        mode_results.push((q_count, decoded_chars));
    }

    let best_mode = if mode_results[0].0 <= mode_results[1].0 {
        0
    } else {
        1
    };
    mode_results[best_mode].1.clone()
}


// ---- Bookshelf ----

use std::path::PathBuf as FsPathBuf;

pub(crate) fn shelf_dir() -> FsPathBuf {
    FsPathBuf::from("novel_workspace")
}

fn shelf_covers_dir() -> FsPathBuf {
    shelf_dir().join("covers")
}


fn extract_cover_url(html: &str) -> Option<String> {
    // Try <meta property="og:image" content="...">
    if let Some(start) = html.find("property=\"og:image\"") {
        let after = &html[start..];
        if let Some(cs) = after.find("content=\"") {
            let vs = cs + 9;
            let remaining = &after[vs..];
            if let Some(end) = remaining.find('"') {
                let url = &remaining[..end];
                if url.starts_with("http") {
                    return Some(url.to_string());
                }
            }
        }
    }
    // Try <meta content="..." property="og:image">
    if let Some(start) = html.find("content=\"") {
        let after = &html[start..];
        if let Some(_prop) = after.find("og:image") {
            let cs = 9;
            let vs = start + cs;
            let remaining = &html[vs..];
            if let Some(end) = remaining.find('"') {
                let url = &remaining[..end];
                if url.starts_with("http") {
                    return Some(url.to_string());
                }
            }
        }
    }
    None
}

pub(crate) fn shelf_slug(title: &str) -> String {
    let slug: String = title
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-' || *c == ' ')
        .collect::<String>()
        .trim()
        .replace(' ', "_")
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '_')
        .collect::<String>()
        .trim_matches('_')
        .to_lowercase();
    // Truncate to 60 chars to keep filesystem happy
    slug.chars().take(60).collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NovelshelfEntry {
    pub slug: String,
    pub title: String,
    pub chapter_count: usize,
    pub has_cover: bool,
    pub file_size: u64,
}
pub fn scan_shelf() -> Vec<NovelshelfEntry> {
    let dir = shelf_dir();
    if !dir.exists() { return vec![]; }
    let mut entries = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for entry in rd.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") { continue; }
            let meta = match std::fs::metadata(&path) { Ok(m) => m, _ => continue };
            let content = std::fs::read_to_string(&path).unwrap_or_default();
            let title = content.lines()
                .find(|l| l.starts_with("# "))
                .map(|l| l.trim_start_matches("# ").trim().to_string())
                .unwrap_or_default();
            let slug = shelf_slug(&title);
            let cdir = shelf_covers_dir();
            let has_cover = cdir.join(format!("{slug}.jpg")).exists()
                || cdir.join(format!("{slug}.webp")).exists();
            let cc = content.lines().filter(|l| l.starts_with("- 第")).count();
            entries.push(NovelshelfEntry { slug, title, chapter_count: cc, has_cover, file_size: meta.len() });
        }
    }
    entries.sort_by(|a,b| b.file_size.cmp(&a.file_size));
    // Dedupe by slug, keep largest file_size (already sorted desc).
    let mut seen = std::collections::HashSet::new();
    entries.retain(|e| seen.insert(e.slug.clone()));
    entries
}

pub async fn novels_list(
    State(_state): State<AppState>,
    _headers: HeaderMap,
) -> Response {
    Json(json!({"ok":true,"novels":scan_shelf()})).into_response()
}

pub async fn novel_content(
    _state: State<AppState>,
    _headers: HeaderMap,
    Path(slug): Path<String>,
) -> Response {
    let shelf = scan_shelf();
    let found = shelf.into_iter().find(|e| e.slug == slug);
    match found {
        Some(entry) => {
            let dir = shelf_dir();
            let slug_tag = shelf_slug(&entry.title);
            let mut file_path = None;
            if let Ok(rd) = std::fs::read_dir(&dir) {
                for e in rd.flatten() {
                    let p = e.path();
                    if p.extension().and_then(|x| x.to_str()) != Some("md") { continue; }
                    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                    // Normalize both sides: use shelf_slug on the stem for comparison
                    let stem_normalized = shelf_slug(stem);
                    if stem_normalized.contains(&slug_tag) {
                        file_path = Some(p); break;
                    }
                }
            }
            match file_path {
                Some(path) => match std::fs::read_to_string(&path) {
                    Ok(content) => Json(json!({"ok":true,"title":entry.title,"slug":slug,"content":content,"chapter_count":entry.chapter_count})).into_response(),
                    Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"ok":false,"error":e.to_string()}))).into_response()
                },
                None => (StatusCode::NOT_FOUND, Json(json!({"ok":false,"error":"file not found"}))).into_response()
            }
        }
        None => (StatusCode::NOT_FOUND, Json(json!({"ok":false,"error":"novel not found"}))).into_response()
    }
}

pub async fn novel_cover(Path(slug): Path<String>) -> Response {
    // shelf_slug keeps only [a-z0-9_] — strips `.`, `/`, `\`, NUL and any traversal shape.
    let slug = shelf_slug(&slug);
    let cdir = shelf_covers_dir();
    for ext in &["jpg","jpeg","png","webp"] {
        let path = cdir.join(format!("{slug}.{ext}"));
        if path.exists() {
            match tokio::fs::read(&path).await {
                Ok(bytes) => {
                    let mime = match *ext { "jpg"|"jpeg" => "image/jpeg", "png" => "image/png", "webp" => "image/webp", _ => "application/octet-stream" };
                    return (StatusCode::OK, [("Content-Type",mime),("Cache-Control","public, max-age=86400")], bytes).into_response();
                }
                Err(_) => continue,
            }
        }
    }
    (StatusCode::NOT_FOUND, "cover not found").into_response()
}

// ─── Shelf import / promote to Story Tavern ───────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShelfImportBody {
    /// Raw novel text (.txt / .md body).
    pub text: String,
    /// Optional base64-encoded raw bytes. When present, `text` is ignored and the
    /// bytes are sniffed (BOM → UTF-8/GB18030/Big5/Shift_JIS/EUC-KR strict decode +
    /// mojibake check) to avoid GBK/Big5 source txt being mis-decoded as UTF-8 (乱码).
    #[serde(default)]
    pub data: Option<String>,
    /// Display title; defaults to first heading or "未命名".
    #[serde(default)]
    pub title: Option<String>,
    /// When true, also build a Story Tavern pack from the text (default true).
    #[serde(default)]
    pub to_pack: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShelfToPackBody {
    /// Optional pack id override.
    #[serde(default)]
    pub pack_id: Option<String>,
    /// Force rebuild even if a pack with same source already exists.
    #[serde(default)]
    pub force: Option<bool>,
}

/// 一条目录规则：chapter 正则用于在整篇文本上 (MULTILINE) 匹配章标题。
/// 移植自 Legado txtTocRule.json 的核心启用规则，去掉 Rust `regex` 不支持的前后顾断言。
struct TocRule {
    #[allow(dead_code)] // [P7] 抓取源展示名预留
    name: &'static str,
    enabled: bool,
    chapter: &'static str,
}

/// Legado 内置目录规则（enabled 子集），适配 Rust `regex`（无 look-around）。
/// 数字字符集覆盖：阿拉伯 + 中文小写(零一二…) + 中文大写(壹贰叁…) + 〇。
const TOC_RULES: &[TocRule] = &[
    TocRule {
        name: "目录",
        enabled: true,
        chapter: r"(?m)^[ \t　#]{0,4}(?:序章|楔子|正文|终章|后记|尾声|番外|第[ \t　]{0,4}[0-9〇零一二两三四五六七八九十百千万壹贰叁肆伍陆柒捌玖拾佰仟]+?[ \t　]{0,4}(?:章|节|卷|集|部|篇)).{0,40}$",
    },
    TocRule {
        name: "数字 可选分隔符 标题名称",
        enabled: true,
        chapter: r"(?m)^[ \t　]{0,4}[0-9]{1,5}[:：,.， 、_—\-]?.{1,40}$",
    },
    TocRule {
        name: "大写数字 分隔符 标题名称",
        enabled: true,
        chapter: r"(?m)^[ \t　]{0,4}(?:序章|楔子|正文|终章|后记|尾声|番外|[0-9〇零一二两三四五六七八九十百千万壹贰叁肆伍陆柒捌玖拾佰仟]{1,8}章?)[ 、_—\-].{1,40}$",
    },
    TocRule {
        name: "数字混合 分隔符 标题名称",
        enabled: true,
        chapter: r"(?m)^[ \t　]{0,4}(?:序章|楔子|正文|终章|后记|尾声|番外|[0-9〇零一二两三四五六七八九十百千万壹贰叁肆伍陆柒捌玖拾佰仟]{1,8}章?[ 、_—\-]|[0-9]{1,5}章?[:：,.， 、_—\-]).{0,40}$",
    },
    TocRule {
        name: "Chapter/Section/Part/Episode 序号 标题",
        enabled: true,
        chapter: r"(?mi)^[ \t　]{0,4}(?:[Cc]hapter|[Ss]ection|[Pp]art|ＰＡＲＴ|[Nn][oO][.、]|[Ee]pisode|[Cc]h\.?)[ \t　]{0,4}[0-9]{1,4}.{0,40}$",
    },
    TocRule {
        name: "特殊符号 序号 标题",
        enabled: true,
        chapter: r"(?m)^[ \t　]{0,4}[【〔〖「『〈［\[](?:第|[Cc]hapter)[0-9〇零一二两三四五六七八九十百千万壹贰叁肆伍陆柒捌玖拾佰仟]{1,10}[章节].{0,30}$",
    },
    TocRule {
        name: "章/卷 序号 标题",
        enabled: true,
        chapter: r"(?m)^[ \t　]{0,4}[卷章][0-9〇零一二两三四五六七八九十百千万壹贰叁肆伍陆柒捌玖拾佰仟]{1,8}[ \t　]{0,4}.{0,40}$",
    },
    TocRule {
        name: "字数分割 分节阅读",
        enabled: true,
        chapter: r"(?m)^[ \t　]{0,4}(?:.{0,15}分[页节章段]阅读[-_ ]|第[ \t　]{0,4}[0-9〇零一二两三四五六七八九十百千万]{1,6}[ \t　]{0,4}[页节]).{0,40}$",
    },
    TocRule {
        name: "顶格标题",
        enabled: false,
        chapter: r"(?m)^[^ \t　].{1,20}$",
    },
    TocRule {
        name: "通用规则",
        enabled: false,
        chapter: r"(?m)^.{0,6}(?:[引楔]子|正文|[引序前]言|[序终]章|[上中下][部篇卷]|后记|尾声|番外|={2,4}|第[ \t　]{0,4}[0-9〇零一二两三四五六七八九十百千万壹贰叁肆伍陆柒捌玖拾佰仟]+?[ \t　]{0,4}(?:章|节|卷|部|篇)).{0,40}$",
    },
    // 2026-08-19: Markdown 列表前缀破折号（安全屋/老板你也没说/孕船 等 txt 导出经常带 `- 第X章`）
    TocRule {
        name: "列表破折号 章标题",
        enabled: true,
        chapter: r"(?m)^[ \t　]{0,4}[-–—·•*]{1,2}[ \t　]{0,2}(?:第[ \t　]{0,4}[0-9〇零一二两三四五六七八九十百千万壹贰叁肆伍陆柒捌玖拾佰仟]+?[ \t　]{0,4}(?:章|节|卷|集|部|篇)).{0,40}$",
    },
    // 2026-08-19: 全角括号序号标题（度蜜月 `　　（5）释放（手交口交）` 内嵌章号）
    // 仅作为独立切分规则候选；混合格式源（第X章 主流 + 内嵌（N））需多规则合并切分兜底（见 split 逻辑）。
    TocRule {
        name: "全角括号序号 标题",
        enabled: true,
        chapter: r"(?m)^[ \t　]{0,4}[（(【［\[]\s*[0-9０-９]{1,8}\s*[）)】］\]](?:[ \t　]{0,4}\S.{0,40})?$",
    },
];

/// 分章时长章节的阈值（超出则按字数窗口二次拆分），参考 Legado「拆分超长章节」。
#[allow(dead_code)] // [P7] 长文切分阈值预留
const SPLIT_LONG_THRESHOLD: usize = 30_000;

/// 无法用目录规则切分时的窗口大小（字符），对齐换行落点。
const FALLBACK_WINDOW: usize = 2400;

/// Split novel text into chapters — 移植 Legado txtTocRule 的多规则自动甄别方案。
/// 与旧实现（单正则 + 中文数字统一归一化为 "n" 导致误合并）不同：
///  1. 内置多条启用的目录规则，对全文逐条统计"匹配数"，选取匹配最多者作为本章规则；
///  2. 匹配统计时要求相邻标题间隔 >1000 字符，过滤把正文当标题的误报；
///  3. 找不到 ≥2 个标题时退回字数窗口切分，并对齐到换行符（不硬切在句中）。
pub(crate) fn split_novel_chapters(text: &str) -> Vec<(String, String)> {
    // 选中最合适的目录规则（匹配数最多，且相邻标题间隔 >1000）
    let best = pick_toc_rule(text);
    if let Some(rule) = best {
        let regex = match regex::Regex::new(rule.chapter) {
            Ok(re) => re,
            Err(_) => return fallback_windows(text),
        };
        // 收集所有标题的 (byte 起点, 标题文本)
        let mut titles: Vec<(usize, String)> = Vec::new();
        for m in regex.find_iter(text) {
            let title = m
                .as_str()
                .trim()
                .trim_start_matches(|c| c == '#' || c == '-' || c == '*' || c == ' ')
                .trim()
                .chars()
                .take(80)
                .collect::<String>();
            if !title.is_empty() {
                titles.push((m.start(), title));
            }
        }
        // 至少 2 个标题才算切章成功，否则 fallback
        if titles.len() >= 2 {
            // 2026-08-19: 补充规则合并——混合格式源（主流「第X章」 + 内嵌「（N）标题」如度蜜月
            // 第五章 `　　（5）释放`）单规则会漏。用「全角括号序号」规则额外扫描主规则未覆盖的
            // 标题：仅并入 chapter_value 能解析（含括号前导序号扩展）且与主序列章号构成 gap 的标题，
            // 防正文误报（正文里的 `（3）` 数字通常不成章号序列）。
            if let Ok(bracket_re) = regex::Regex::new(
                r"(?m)^[ \t　]{0,4}[（(【\[][ \t　]{0,4}[0-9]{1,5}[ \t　]{0,4}[）)】\]][^\n]{0,60}$",
            ) {
                let main_vals: Vec<i64> = titles
                    .iter()
                    .filter_map(|(_, t)| chapter_value(t))
                    .collect();
                let mut extra: Vec<(usize, String)> = Vec::new();
                for m in bracket_re.find_iter(text) {
                    let mstart = m.start();
                    // 与主标题位置过近（<300 字符）视为同一标题变体，跳过
                    if titles.iter().any(|(idx, _)| mstart.abs_diff(*idx) < 300) {
                        continue;
                    }
                    let raw = m.as_str();
                    let t = raw.trim().chars().take(80).collect::<String>();
                    if t.is_empty() {
                        continue;
                    }
                    if let Some(v) = chapter_value(&t) {
                        // 章号在主序列 gap 内（如主序列 1..4,6 时 5 是 gap）→ 并入
                        let is_gap = main_vals.iter().any(|mv| *mv > v + 0)
                            && !main_vals.contains(&v);
                        // 或主序列尚无该序号且后续有更大章号 → 补进
                        let fills = main_vals.iter().any(|mv| *mv > v) && !main_vals.contains(&v);
                        if is_gap || fills {
                            extra.push((mstart, t));
                        }
                    }
                }
                if !extra.is_empty() {
                    titles.extend(extra);
                    titles.sort_by_key(|(idx, _)| *idx);
                    // 位置过近的重复（同章双标题）交给 dedup_same_value 按章号合并
                }
            }
            // 合并"同真实章号"的相邻重复（如 `# 第1章` 与裸 `第1章` 双标题），
            // 只按真实数值合并，避免旧实现"中文数字统一归一为 n"导致的误合并。
            let titles = dedup_same_value(titles);
            if titles.len() >= 2 {
                // A0: 章号连续性检测——解析每章真实章号（如「第六章」→6），检测跳号/缺章，
                // 出现 gap 时 warn 记录（源文件缺章/标题解析失败 → 后续 chXX 编号会整体错位，
                // 污染 worldline/relations/事件卡等带章节产物；此处先可见化，供人工修正源）。
                let gaps = chapter_gaps(&titles);
                if !gaps.is_empty() {
                    tracing::warn!(
                        n_titles = titles.len(),
                        gaps = ?gaps,
                        "章节切分检测到章号不连续（缺章/跳号），下游 chXX 编号将与该序列对齐"
                    );
                }
                return slice_chapters(text, &titles);
            }
        }
    }
    fallback_windows(text)
}

/// 多规则自动甄别：逐条统计匹配数（要求相邻标题间隔 >1000），返回匹配最多且启用者。
fn pick_toc_rule(text: &str) -> Option<&'static TocRule> {
    let mut best: Option<&'static TocRule> = None;
    let mut max_num: usize = 0;
    for rule in TOC_RULES.iter().filter(|r| r.enabled) {
        let Ok(re) = regex::Regex::new(rule.chapter) else {
            continue;
        };
        let mut num = 0usize;
        let mut prev_end: Option<usize> = None;
        for m in re.find_iter(text) {
            let ok_gap = match prev_end {
                None => true,
                Some(pe) => m.start().saturating_sub(pe) > 1000,
            };
            if ok_gap {
                num += 1;
            }
            prev_end = Some(m.end());
        }
        // Legado 用 `num >= maxNum`（后匹配者平局时胜）。这里取首次达到最大者的近似即可。
        if num > max_num {
            max_num = num;
            best = Some(rule);
        }
    }
    best
}

/// 相邻同真实章号去重：`# 第1章` 与裸 `第1章` 这类双标题各识别到一次，章号数值相同 →
/// 合并为最后出现的标题（保留正文标题而非页眉）。仅按真实数值判定相等，不同数值保留。
fn dedup_same_value(titles: Vec<(usize, String)>) -> Vec<(usize, String)> {
    let mut out: Vec<(usize, String)> = Vec::new();
    for (idx, title) in titles {
        if let Some((last_idx, last_title)) = out.last_mut() {
            // 两条标题真实章号都存在且相等 → 认为同章重复，更新为此条的字节起点（更靠后、更接近正文）
            if let (Some(a), Some(b)) = (chapter_value(&title.clone()), chapter_value(last_title)) {
                if a == b {
                    *last_idx = idx;
                    *last_title = title_truncate(&title);
                    continue;
                }
            }
        }
        out.push((idx, title_truncate(&title)));
    }
    out
}

/// 从标题头部提取章号并转成真实数值（中文数字支持到万位），失败返回 None。
/// 例："第一章 续一" → 1；"第一千零六十章 嫌弃的对象" → 1060；"第 1 章 得加钱" → 1。
pub(crate) fn chapter_value(title: &str) -> Option<i64> {
    let t = title.trim();
    // 2026-08-19: 支持全角/半角括号前导序号（度蜜月 `　　（5）释放`、`(7) xxx`）→ 返回括号内数值。
    // 模式：^(（|() N (）|)) [可带 章/话/集] 可选标题
    for (open, close) in [('（', '）'), ('(', ')'), ('【', '】'), ('[', ']')] {
        let trimmed = t.trim_start_matches(['\u{3000}', ' ', '\t']);
        if let Some(rest) = trimmed.strip_prefix(open) {
            let digits: String = rest
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if !digits.is_empty() {
                // ASCII 数字 1 char == 1 byte，digits.len() 即字节偏移
                let after = &rest[digits.len()..];
                if let Some(_after_close) = after.strip_prefix(close) {
                    // 括号后紧跟 章/话/集 或直接标题 → 视作章号
                    if let Some(v) = digits.parse::<i64>().ok() {
                        // 允许括号后有标题（如「（5）释 放」）或前后缀（「（5）章」）
                        return Some(v);
                    }
                }
            }
        }
    }
    // 去掉可选的 "第 / 章 / 节 / 卷 / 部分 / 序 段落" 前后缀，取中间的数字段
    let inner = t
        .strip_prefix("第")
        .map(|s| {
            // 去掉结尾的 章|节|卷|集|部|篇 及其后所有内容
            let s = s.trim_start();
            if let Some(pos) = s.find(|c| matches!(c, '章' | '节' | '卷' | '集' | '部' | '篇')) {
                &s[..pos]
            } else {
                s
            }
        })
        .unwrap_or(t);
    let inner = inner.trim();
    if inner.is_empty() {
        return None;
    }
    cn_number_to_i64(inner)
}

/// A0 完整版 (2026-08-19)：为章节标题序列分配 chXX 编号 —— **chXX = max(原著章号, 递增游标)**。
/// - 原著章号优先（「第六章」→ 6）：缺章自动跳号，让 chXX 与原著标题对齐
///   （源缺第五章时，标题「第六章 初恋」所在的第 5 个切片 → ch06，ch05 空缺可见）。
/// - 解析失败（楔子/序章/番外等非「第N章」格式）→ 用递增游标续号；
/// - 章号回退/重复（如续章「第一章 续一」跟在楔子后，章号=1 却排在切片 2）
///   → 用游标顺延去重，绝不产出重复 chXX。
/// 语义等价于一次遍历维护 `next_id`：`id = max(v, next_id)`，`next_id = id + 1`。
/// 该编号体系同时用于 pack.chapters.id / chapters/{id}.md 文件名 / nodes.chapter_id /
/// distil_chapters 蒸馏前缀 / build_roster_input 的【chXX】标注，保证下游
/// worldline / relations / 事件卡 chapterRange 全部与原著标题对齐（修复 D1「章号错位」）。
pub(crate) fn chapter_id_seq(titles: &[(String, String)]) -> Vec<String> {
    let mut out = Vec::with_capacity(titles.len());
    let mut next_id: i64 = 1;
    for (t, _b) in titles {
        let v = chapter_value(t).unwrap_or(0);
        let id = v.max(next_id);
        out.push(format!("ch{:02}", id));
        next_id = id + 1;
    }
    out
}

/// 将"一~万"中文数字串转成数值。
/// A0: 检测章节标题序列的章号连续性（解析「第X章」→ 数值）。
/// 返回跳号描述列表（如 ["5→6 之间缺章(应为 5)", "6→7…"]）。
/// 规则：章号必须严格递增；相邻差值 >1 视为缺章（gap：前章号+1..后章号-1 全缺）。
/// 解析失败（标题不是「第N章」格式，如「序章/番外」）跳过不参与判断。
fn chapter_gaps(titles: &[(usize, String)]) -> Vec<String> {
    let mut gaps = Vec::new();
    let mut prev: Option<i64> = None;
    for (_, title) in titles {
        let Some(cur) = chapter_value(title) else { continue };
        if let Some(p) = prev {
            if cur <= p {
                gaps.push(format!("章号不递增：{p} → {cur}（{}）", title_truncate(title)));
            } else if cur - p > 1 {
                let missing: Vec<i64> = ((p + 1)..cur).collect();
                gaps.push(format!(
                    "{p} → {cur} 缺章：{}（缺 {}）",
                    title_truncate(title),
                    missing.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(",")
                ));
            }
        }
        prev = Some(cur);
    }
    gaps
}

fn cn_number_to_i64(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(n) = s.parse::<i64>() {
        return Some(n);
    }
    let digits = |c: char| -> Option<i64> {
        match c {
            '零' | '〇' => Some(0),
            '一' => Some(1),
            '二' | '两' | '俩' => Some(2),
            '三' => Some(3),
            '四' => Some(4),
            '五' => Some(5),
            '六' => Some(6),
            '七' => Some(7),
            '八' => Some(8),
            '九' => Some(9),
            _ => None,
        }
    };
    let unit = |c: char| -> Option<i64> {
        match c {
            '十' => Some(10),
            '百' => Some(100),
            '千' => Some(1000),
            '万' => Some(10_000),
            _ => None,
        }
    };
    // 万位分割：lv * 10000 + rv
    if let Some(pos) = s.find('万') {
        let left = &s[..pos];
        let right = &s[pos + 1..];
        let lv = if left.is_empty() { 1 } else { cn_number_to_i64(left)? };
        let rv = if right.is_empty() { 0 } else { cn_number_to_i64(right)? };
        return Some(lv * 10_000 + rv);
    }
    let mut result = 0i64;
    let mut num = 0i64;
    for c in s.chars() {
        if let Some(d) = digits(c) {
            num = d;
        } else if let Some(u) = unit(c) {
            if num == 0 {
                num = 1;
            }
            result += num * u;
            num = 0;
        } else {
            return None;
        }
    }
    result += num;
    if result > 0 {
        Some(result)
    } else {
        None
    }
}

fn title_truncate(title: &str) -> String {
    title.trim().chars().take(80).collect()
}

/// 按标题 byte 起点切分正文为 (title, content)。content 含标题行。
fn slice_chapters(text: &str, titles: &[(usize, String)]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (i, (idx, title)) in titles.iter().enumerate() {
        let end = if i + 1 < titles.len() {
            titles[i + 1].0
        } else {
            text.len()
        };
        let content = text[*idx..end].trim().to_string();
        if !content.is_empty() {
            out.push((title.clone(), content));
        }
    }
    if out.is_empty() {
        out.push(("第1章".into(), text.to_string()));
    }
    out
}

/// 兜底切分：按固定窗口切，并对齐到最近的换行符（不在句中硬切）。
fn fallback_windows(text: &str) -> Vec<(String, String)> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    if chars.is_empty() {
        return vec![("第1章".into(), text.to_string())];
    }
    let mut parts: Vec<(String, String)> = Vec::new();
    let mut i = 0;
    let mut part_i = 1;
    while i < chars.len() {
        let start_byte = chars[i].0;
        let mut end_i = (i + FALLBACK_WINDOW).min(chars.len());
        if end_i < chars.len() {
            let min_j = i + FALLBACK_WINDOW / 2;
            let mut j = end_i;
            while j > min_j {
                if chars[j].1 == '\n' {
                    end_i = j;
                    break;
                }
                j -= 1;
            }
        }
        let end_byte = if end_i >= chars.len() {
            text.len()
        } else {
            chars[end_i].0
        };
        let chunk = text[start_byte..end_byte].trim();
        if !chunk.is_empty() {
            parts.push((format!("第{part_i}章"), chunk.to_string()));
            part_i += 1;
        }
        if end_i >= chars.len() {
            break;
        }
        i = end_i;
    }
    if parts.is_empty() {
        parts.push(("第1章".into(), text.to_string()));
    }
    parts
}

pub(crate) fn is_clean_name(name: &str) -> bool {
    kaleido_core::is_clean_cast_name(name)
}

/// [fix 2026-08-15] 从 LLM 输出中提取完整 JSON 数组：从第一个 `[` 起按深度配对，
/// 处理 LLM 常在数组后追加说明文字（「以上是角色…」「```json」）或嵌套 `]` 的情况。
fn extract_json_array(raw: &str) -> Option<Vec<serde_json::Value>> {
    let start = raw.find('[')?;
    let bytes = raw.as_bytes();
    let mut depth = 0usize;
    let mut in_str = false;
    let mut escaped = false;
    let mut end = None;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        match b {
            b'"' if !escaped => in_str = !in_str,
            b'\\' if in_str && !escaped => escaped = true,
            _ => escaped = false,
        }
        if in_str {
            continue;
        }
        match b {
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let e = end?;
    serde_json::from_str(&raw[start..=e]).ok()
}

/// [fix 2026-08-15] 章节目录页判别：短文件（<5KB）+ 大量「- 第N章：标题」行 = 导入时
/// 把目录页当章节存了（安全屋 pack ch01-03.md 实证）。目录页不是正文，LLM 会从标题瞎抽。
fn is_chapter_toc(content: &str) -> bool {
    if content.len() > 5000 {
        return false;
    }
    let toc_lines = content
        .lines()
        .filter(|l| l.trim_start().starts_with("- ") && l.contains("第") && l.contains('章'))
        .count();
    toc_lines >= 5
}

/// 自动角色抽取：新建 pack 后异步调用 LLM 抽取角色写回（角色 <3 时触发）。
/// 失败静默降级（不阻塞导入）；RPM 打满时 resolve_llm 返回空 base_url → 直接跳过。
pub(crate) fn spawn_auto_cast_extraction(state: AppState, pack_id: String) {
    tokio::spawn(async move {
        tracing::info!(pack_id, "auto cast extraction started");
        let pack = match state.packs.get(&pack_id) {
            Ok(p) => p,
            Err(_) => return,
        };
        let real: Vec<String> = pack
            .characters
            .iter()
            .filter(|c| c.role != "narrator" && c.role != "player")
            .map(|c| c.name.clone())
            .collect();
        // [fix 2026-08-15 阈值相对判断] 原来 `>=3 就跳过` 导致长篇小说（331 章智取美母等）
        // 启发式抽到 3-4 个后 LLM 抽取永不触发，叙述中出现的大量角色全漏。
        // 改为：已有角色数少于「章节数/20 且至少 4 个」才补抽；existing.contains 去重保证重复补抽安全。
        let chapter_count = pack.chapters.len().max(1);
        let min_expected = (chapter_count / 20).max(4);
        if real.len() >= min_expected {
            return;
        }
        // [fix 2026-08-15 分层抽样] 原来只读前 5 章 → 长篇中后段角色永远抽不到（温床/宿醉等
        // 短篇 LLM 每轮抽到相同前部角色 added=0 静默）。改为按章节数均匀取 5 个采样点：
        // 开头/1/4/1/2/3/4/结尾，覆盖全书角色分布。
        let mut sample = String::new();
        let mut chapters_read = 0usize;
        let mut sampled: Vec<usize> = Vec::new();
        if chapter_count <= 5 {
            sampled = (0..chapter_count).collect();
        } else {
            for frac in [0usize, 1, 2, 3, 4] {
                sampled.push((chapter_count - 1) * frac / 4);
            }
            sampled.dedup();
        }
        for idx in sampled {
            let ch = &pack.chapters[idx];
            let body = if !ch.body_path.trim().is_empty() {
                state
                    .packs
                    .read_chapter_body(&pack_id, &ch.body_path)
                    .ok()
            } else {
                None
            };
            let body = body.or_else(|| {
                // [fix 2026-08-15] 珞白 pack：chapters 登记了 body_path=None 但正文在 chapters/{id}.md
                state
                    .packs
                    .read_chapter_body(&pack_id, &format!("chapters/{}.md", ch.id))
                    .ok()
            });
            if let Some(f) = body {
                // [fix 2026-08-15] 目录页污染样本：安全屋 pack 的 ch01-03.md 是「- 第N章：标题」目录列表
                // 不是正文（导入时抓目录当章节存了）。目录页特征 = 短文件 + 大量「- 第N章」行。
                // 跳过目录页，避免 LLM 从章节标题里瞎抽"角色"。
                if is_chapter_toc(&f) {
                    tracing::warn!(pack_id, title = %ch.title, "auto cast: 跳过章节目录页");
                    continue;
                }
                sample.push_str(&format!("=== {} ===\n", ch.title));
                sample.push_str(&f.chars().take(2200).collect::<String>());
                sample.push('\n');
                chapters_read += 1;
            }
        }
        // 章节索引全断链时回退扫 chapters/ 目录实际文件（前 5 个按文件名排序）
        if chapters_read == 0 {
            if let Ok(dir) = state.packs.pack_dir(&pack_id) {
                let chdir = dir.join("chapters");
                if let Ok(entries) = std::fs::read_dir(&chdir) {
                    let mut files: Vec<_> = entries.filter_map(|e| e.ok()).collect();
                    files.sort_by_key(|e| e.file_name());
                    for ent in files.into_iter().take(5) {
                        if let Ok(f) = std::fs::read_to_string(ent.path()) {
                            if is_chapter_toc(&f) {
                                continue;
                            }
                            sample.push_str(&format!(
                                "=== {} ===\n",
                                ent.file_name().to_string_lossy()
                            ));
                            sample.push_str(&f.chars().take(2200).collect::<String>());
                            sample.push('\n');
                            chapters_read += 1;
                        }
                    }
                }
            }
        }
        // [fix 2026-08-15] 正文文件全缺时从 nodes[].summary 拼样本（智取美母实证：
        // chapters/ 目录空、bodyPath 全断链，但 331 个 node 的 summary 各含 400 字蒸馏正文，
        // 足够 LLM 抽取角色）。summary 都是正文浓缩，不会误触目录页/会话另存检测。
        if chapters_read == 0 {
            for n in pack.nodes.iter().take(10) {
                if !n.summary.trim().is_empty() {
                    sample.push_str(&format!("=== {} ===\n", n.title));
                    sample.push_str(&n.summary.chars().take(2000).collect::<String>());
                    sample.push('\n');
                }
            }
        }
        if sample.trim().is_empty() {
            return;
        }
        // [fix 2026-08-15] 会话另存 pack 判别：章节内容是「故事馆 · turn 0 · tavern-session-xxx」开场白，
        // 不是小说正文（孕船-88828 只有 665 字节开场白），抽取无意义，跳过并标记。
        if sample.contains("来源：故事馆") || sample.contains("tavern-session-") {
            tracing::warn!(pack_id, "auto cast extraction skipped: 会话另存 pack 无小说正文");
            return;
        }
        let llm = state
            .app_state
            .resolve_llm(state.llm_base.as_deref(), state.llm_key.as_deref(), &state.llm_model);
        let prov_kind = crate::llm_stream::runtime_provider_kind(&llm, &state.provider_kind);
        if llm.base_url.trim().is_empty() || llm.api_key.trim().is_empty() {
            tracing::warn!(pack_id, "auto cast extraction skipped: no llm runtime");
            return;
        }
        let system = "你是小说角色抽取助手。从给定的小说章节文本中识别有跨章节意义的角色（人名，排除旁白/读者/玩家），输出 JSON 数组：[{\"name\":\"角色名\",\"role\":\"主角|配角\",\"personality\":\"一句话性格\",\"speechStyle\":\"说话风格\",\"identity\":\"身份\",\"aliases\":[\"该角色的其他称呼/别名/外号，不含亲缘称谓如母亲/哥哥，不含泛称如那怪/众人\"],\"exampleDialogs\":[\"一句代表性台词\"]}]。name 必须是明确的人物专名（姓名/称呼/外号），严禁输出句子片段、短语、普通名词或口语碎片（如\"那就\"\"冲进下水\"\"另一个\"这类都不是角色名）；拿不准的名字不要输出。只输出 JSON，不要其他文字。";
        match crate::llm_stream::stream_chat_completions_dispatch(
            &llm.base_url,
            &llm.api_key,
            &llm.model,
            &prov_kind,
            system,
            &sample,
            0.1,
            4096,
            150,
            |_| true,
        )
        .await
        {
            Ok(raw) => {
                // [fix 2026-08-15] rfind(']') 会截到错误位置：LLM 输出常在 JSON 数组后
                // 追加说明文字（「以上是角色…」「```」）或中途出现嵌套 ]，导致 trailing characters。
                // 改为括号匹配：从第一个 [ 起按深度配对取完整数组。
                let parsed: Vec<Value> = match extract_json_array(&raw) {
                    Some(v) => v,
                    None => {
                        tracing::warn!(pack_id, "auto cast extraction: no valid JSON array in llm output");
                        return;
                    }
                };
                let mut pack = match state.packs.get(&pack_id) {
                    Ok(p) => p,
                    Err(_) => return,
                };
                let mut existing: std::collections::HashSet<String> = pack
                    .characters
                    .iter()
                    .map(|c| c.name.clone())
                    .collect();
                let mut added = 0usize;
                // [fix 2026-08-15 alias_merge 接线] 收集本轮 LLM 抽取的 (name, aliases)，
                // 若 aliases 命中已有角色（如新抽「美猴王」aliases=[孙悟空] 而已有孙悟空），
                // 归并到已有角色而非重复添加；未命中的别名也全部归一化到组内 canonical。
                let mut alias_pairs: Vec<(String, String)> = Vec::new();
                for v in &parsed {
                    let name = v
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    if name.is_empty() {
                        continue;
                    }
                    if let Some(arr) = v.get("aliases").and_then(|a| a.as_array()) {
                        for a in arr {
                            if let Some(alias) = a.as_str() {
                                let alias = alias.trim();
                                if !alias.is_empty() && alias != name {
                                    alias_pairs.push((alias.to_string(), name.clone()));
                                }
                            }
                        }
                    }
                }
                let alias_map = kaleido_core::alias_merge::build_alias_map(&alias_pairs);
                // [P7] merged_aliases 计数器从未被读取（仅日志可观测），移除变量保留日志语义。
                for (i, v) in parsed.into_iter().enumerate() {
                    let name = v
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    if name.is_empty() || existing.contains(&name) {
                        continue;
                    }
                    // [fix 2026-08-15 alias_merge 接线] 别名命中已有角色 → 归并跳过
                    // （「美猴王」aliases=[孙悟空] 而已有孙悟空 → 美猴王不再新增占位卡）
                    let resolved = kaleido_core::alias_merge::resolve_alias(&alias_map, &name);
                    if resolved != name && existing.contains(&resolved) {
                        tracing::info!(pack_id, alias = %name, canon = %resolved, "auto cast: 别名归并到已有角色");
                        continue;
                    }
                    // [fix 2026-08-13 乱码名专项] 角色名合法性过滤 + 正文存在性兜底：
                    // LLM 常把句子片段当人名（「冲进下水」「过电车轨」「那就」），
                    // 直接 push 会产生空壳占位卡污染 pack。名字必须在抽取样本中出现
                    // （乱码名如「湮风通」「人里克斯」在正文中从不以该形式出现）。
                    if !crate::convert::is_plausible_character_name(&name) {
                        tracing::warn!(pack_id, name = %name, "auto cast: 过滤疑似乱码角色名");
                        continue;
                    }
                    if !sample.contains(&name) {
                        tracing::warn!(pack_id, name = %name, "auto cast: 角色名未在正文样本出现，跳过");
                        continue;
                    }
                    let role = if v.get("role").and_then(|r| r.as_str()) == Some("主角") {
                        "protagonist"
                    } else {
                        "supporting"
                    };
                    pack.characters.push(kaleido_core::PackCharacterRef {
                        id: format!("c-cast-{i}"),
                        name: name.clone(),
                        role: role.into(),
                        importance: if i == 0 { "high".into() } else { "medium".into() },
                        gender: "未知".into(),
                        appearance: "未知".into(),
                        opening_scene: "未知".into(),
                        opening_lines: "".into(),
                        nsfw_profile: String::new(),
                        content_tier: None,
                        example_dialogs: v
                            .get("exampleDialogs")
                            .and_then(|d| serde_json::from_value(d.clone()).ok())
                            .unwrap_or_default(),
                        boundaries: vec![],
                        personality: v
                            .get("personality")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string(),
                        speech_style: v
                            .get("speechStyle")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string(),
                        voice_profile: v
                            .get("voiceProfile")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string(),
                        motivation: String::new(),
                        relationships: vec![],
                        evidence_refs: vec![],
                        mental_models: vec![],
                        decision_heuristics: vec![],
                        beliefs: vec![],
                        expressions: Default::default(),
            voice: None,
            archive: None,
                        avatar: None,
                        starting_wardrobe: Default::default(),
                    });
                    existing.insert(name);
                    added += 1;
                }
                if added > 0 {
                    let _ = state.packs.save(pack);
                    tracing::info!(pack_id, added, "auto cast extraction: +{added} chars");
                } else {
                    // 收敛态：LLM 返回的角色全部已存在（多轮补抽后常见），
                    // 明确打日志避免"零日志"被误判为卡死/未触发。
                    tracing::info!(pack_id, "auto cast extraction: 收敛（LLM 返回角色均已存在，无新增）");
                }
            }
            Err(e) => {
                tracing::warn!(pack_id, "auto cast extraction llm error: {e}");
            }
        }
    });
}

fn extract_cast_names(chapters: &[(String, String)]) -> Vec<String> {
    // Match a short CJK name immediately before a dialogue verb.
    // The regex crate has no look-around, so the clause-boundary check
    // (previous char must not be CJK or an interpunct) is done manually:
    // a match with a CJK/· predecessor is inside a longer run, and so is
    // every inner start position, hence the whole match can be skipped.
    let re = regex::Regex::new(r"[\u{4e00}-\u{9fff}·]{2,4}(?:说|道|问|答|喊|叫)").ok();
    let mut seen = std::collections::BTreeSet::new();
    seen.insert("旁白".into());
    seen.insert("读者".into());
    seen.insert("玩家".into());
    let mut names = Vec::new();
    let junk = [
        "露出", "眼角", "换鞋", "随口", "轻声", "低头", "抬起", "转身", "伸手", "走过去", "看向",
        "听见", "突然", "只是", "已经", "然后", "因为", "所以", "连忙", "依旧", "还是",
        "你也没", "有一", "第一道", "第二道", "第三道", "第四道", "第五道", "第六道", "第七道",
        "第八道", "第九道", "第一张", "第二张", "第三张", "第四张", "第五张", "第六张", "第七张",
        "第八张", "第九张", "跟莫",
    ];
    if let Some(re) = re {
        for (_t, body) in chapters.iter().take(40) {
            for m in re.find_iter(body) {
                // manual look-behind: previous char must not be CJK or ·,
                // i.e. the match must start at a clause boundary
                let mut prev = body[..m.start()].chars();
                if let Some(c) = prev.next_back() {
                    if ('\u{4e00}'..='\u{9fff}').contains(&c) || c == '\u{00b7}' {
                        continue;
                    }
                }
                let n = m.as_str().chars().take(m.as_str().chars().count().saturating_sub(1)).collect::<String>();
                if n.is_empty() || seen.contains(&n) {
                    continue;
                }
                if junk.iter().any(|p| n.starts_with(p)) {
                    continue;
                }
                if !is_clean_name(&n) {
                    continue;
                }
                // sanity: an extracted name should not contain dialogue verbs itself
                if n.chars().any(|c| "说道问答喊叫".contains(c)) {
                    continue;
                }
                seen.insert(n.clone());
                names.push(n);
                if names.len() >= 6 {
                    return names;
                }
            }
        }
    }
    names
}

/// 重映射 nodes.present 到蒸馏角色：按每个 node 所在章节正文实际出现的
/// 蒸馏角色名（c-distil-*）重构 present，替换 build_pack_from_chapters 遗留的
/// c-cast 垃圾占位引用。
///
/// 背景：build_pack_from_chapters 用 extract_cast_names（正则抓「说/道」前短语）
/// 生成 c-cast-{i} 占位角色，质量差（常是「微微鞠躬」这类非名字），且 nodes.present
/// 全量克隆同一份列表。真正角色由 distill_pack_characters 以 LLM 抽出为 c-distil-*。
/// 此前 nodes.present 一直是 c-cast 垃圾引用 → build_mainline_opening 按 present
/// 找角色 opening 永远匹配不上蒸馏角色，开局个性化开场退化为通用模板。
/// 修复：distill 后按正文里的真实角色名重构每个 node 的 present。
/// `bodies` 与 nodes 按顺序对应（i 章 → node n{i+1} body 为 bodies[i].1）。
fn remap_node_present_to_distil(
    nodes: &mut [kaleido_core::StoryNode],
    characters: &[kaleido_core::PackCharacterRef],
    bodies: &[(String, String)],
) {
    // 蒸馏角色（c-distil-*）名字列表
    let distil: Vec<(&str, &str)> = characters
        .iter()
        .filter(|c| c.id.starts_with("c-distil-"))
        .map(|c| (c.id.as_str(), c.name.as_str()))
        .collect();
    if distil.is_empty() {
        return;
    }
    for (i, node) in nodes.iter_mut().enumerate() {
        let body = bodies.get(i).map(|b| b.1.as_str()).unwrap_or("");
        let mut present: Vec<String> = Vec::new();
        for (id, name) in &distil {
            // 名字 ≥2 字且在该章正文出现 → 在场
            if name.chars().count() >= 2 && body.contains(name) {
                present.push((*id).to_string());
            }
        }
        if !present.is_empty() {
            node.present_characters = present;
        } else {
            // 正文无任何蒸馏角色直名（旁白/抒情代词章）→ 清空 present，
            // 不保留 build_pack_from_chapters 遗留的 c-cast 悬空引用。
            node.present_characters = Vec::new();
        }
    }
}

pub(crate) fn build_pack_from_chapters(
    title: &str,
    chapters: &[(String, String)],
    pack_id: &str,
    source_ref: &str,
) -> (kaleido_core::StoryPack, Vec<(String, String)>) {
    use kaleido_core::{
        NodeExit, PackCharacterRef, PackSource, PlayMode, StoryChapter, StoryNode, StoryPack,
    };
    let now = chrono::Utc::now().to_rfc3339();
    let mut chars = vec![
        PackCharacterRef {
            id: "c-narrator".into(),
            name: "旁白".into(),
            role: "narrator".into(),
            importance: "low".into(),
            gender: "未知".into(),
            appearance: "未知".into(),
            opening_scene: "未知".into(),
            opening_lines: "".into(),
            nsfw_profile: String::new(),
            content_tier: None,
            example_dialogs: vec![],
            boundaries: vec![],
            personality: "旁白".into(),
            speech_style: String::new(),
            voice_profile: String::new(),
            motivation: String::new(),
            relationships: vec![],
            evidence_refs: vec![],
            mental_models: vec![],
            decision_heuristics: vec![],
            beliefs: vec![],
            expressions: Default::default(),
            voice: None,
            archive: None,
            avatar: None,
            starting_wardrobe: Default::default(),
        },
        PackCharacterRef {
            id: "c-player".into(),
            name: "读者".into(),
            role: "player".into(),
            importance: "low".into(),
            gender: "未知".into(),
            appearance: "未知".into(),
            opening_scene: "未知".into(),
            opening_lines: "".into(),
            nsfw_profile: String::new(),
            content_tier: None,
            example_dialogs: vec![],
            boundaries: vec![],
            personality: "你自己".into(),
            speech_style: String::new(),
            voice_profile: String::new(),
            motivation: String::new(),
            relationships: vec![],
            evidence_refs: vec![],
            mental_models: vec![],
            decision_heuristics: vec![],
            beliefs: vec![],
            expressions: Default::default(),
            voice: None,
            archive: None,
            avatar: None,
            starting_wardrobe: Default::default(),
        },
    ];
    for (i, n) in extract_cast_names(chapters).into_iter().enumerate() {
        chars.push(PackCharacterRef {
            id: format!("c-cast-{i}"),
            name: n,
            role: if i == 0 { "protagonist".into() } else { "supporting".into() },
            importance: "medium".into(),
            gender: "未知".into(),
            appearance: "未知".into(),
            opening_scene: "未知".into(),
            opening_lines: "".into(),
            nsfw_profile: String::new(),
            content_tier: None,
            example_dialogs: vec![],
            boundaries: vec![],
            personality: String::new(),
            speech_style: String::new(),
            voice_profile: String::new(),
            motivation: String::new(),
            relationships: vec![],
            evidence_refs: vec![],
            mental_models: vec![],
            decision_heuristics: vec![],
            beliefs: vec![],
            expressions: Default::default(),
            voice: None,
            archive: None,
            avatar: None,
            starting_wardrobe: Default::default(),
        });
    }
    let present: Vec<String> = chars
        .iter()
        .filter(|c| c.role != "narrator" && c.role != "player")
        .map(|c| c.id.clone())
        .collect();
    let mut story_chapters = Vec::new();
    let mut nodes = Vec::new();
    let mut bodies = Vec::new();
    // A0 完整版 (2026-08-19): chXX 用原著章号（缺章自动跳号对齐标题），
    // 修复 D1「章号错位」——此前用切分序号 i+1，源缺章时 ch05 起整体前移错位，
    // 污染 worldline/relations/事件卡等带章节产物的引用。
    let chapter_ids = chapter_id_seq(chapters);
    for (i, (ctitle, body)) in chapters.iter().enumerate() {
        let ch_id = chapter_ids[i].clone();
        let node_id = format!("n{}", i + 1);
        let body_path = format!("chapters/{ch_id}.md");
        story_chapters.push(StoryChapter {
            id: ch_id.clone(),
            title: ctitle.chars().take(120).collect(),
            order: (i + 1) as u32,
            goals: vec![],
            node_ids: vec![node_id.clone()],
            body_path: body_path.clone(),
            image_path: String::new(), // U10
        });
        let mut exits = Vec::new();
        if i + 1 < chapters.len() {
            exits.push(NodeExit {
                id: format!("e{}", i + 1),
                when: "继续".into(),
                next: format!("n{}", i + 2),
            });
        }
        nodes.push(StoryNode {
            id: node_id,
            chapter_id: ch_id,
            title: ctitle.chars().take(80).collect(),
            entry: "本章开始".into(),
            exit: exits,
            locked_beats: vec![],
            allowed_divergence: "branch".into(),
            present_characters: present.clone(),
            location_id: None,
            summary: body.chars().take(400).collect(),
        });
        bodies.push((body_path, body.clone()));
    }
    let blurb: String = chapters
        .first()
        .map(|(_, b)| b.chars().take(140).collect())
        .unwrap_or_default();
    let lore = if blurb.is_empty() {
        vec![]
    } else {
        vec![json!({"id":"lore-blurb","title":"简介","text": blurb, "range":"","permanent":true})]
    };
    let pack = StoryPack {
        id: pack_id.into(),
        title: title.into(),
        source: PackSource {
            source_type: "novel".into(),
            refs: vec![source_ref.into()],
        },
        characters: chars,
        world_book_ids: vec![],
        chapters: story_chapters,
        nodes,
        lore_entries: lore,
        event_packages: vec![],
        actor_state_config: kaleido_core::ActorStatePackConfig::default(),
        default_mode: PlayMode::Mainline,
        max_tier: kaleido_core::ContentTier::Open,
        language: "zh".into(),
        created_at: now.clone(),
        updated_at: now,
        stage_director: Default::default(),
        worldline: vec![], // T 层：旁挂 worldline.json 由 PackStore::load 填充，构造时空
    };
    (pack, bodies)
}

pub(crate) fn find_existing_pack_for_shelf(
    packs: &kaleido_core::PackStore,
    slug: &str,
    title: &str,
) -> Option<String> {
    let list = packs.list().ok()?;
    for s in list {
        // cheap: get full and check source.refs
        if let Ok(p) = packs.get(&s.id) {
            if p.source.refs.iter().any(|r| r == slug || r.contains(slug)) {
                return Some(p.id);
            }
            if p.title == title && p.source.source_type == "novel" {
                return Some(p.id);
            }
        }
    }
    None
}

pub(crate) fn write_shelf_markdown(title: &str, text: &str, chapters: &[(String, String)]) -> Result<(String, std::path::PathBuf), String> {
    let dir = shelf_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let slug = shelf_slug(title);
    if slug.is_empty() {
        return Err("invalid title".into());
    }
    let mut md = String::new();
    md.push_str(&format!("# {title}\n\n"));
    md.push_str("## 目录\n");
    for (i, (ct, _)) in chapters.iter().enumerate() {
        let disp = if ct.starts_with('第') {
            ct.clone()
        } else {
            format!("第{}章 {ct}", i + 1)
        };
        md.push_str(&format!("- {disp}\n"));
    }
    md.push('\n');
    // Prefer original text if it already has structure; else stitch chapters
    if text.lines().any(|l| l.starts_with("第") && (l.contains('章') || l.contains('节'))) {
        md.push_str(text.trim());
        md.push('\n');
    } else {
        for (i, (ct, body)) in chapters.iter().enumerate() {
            let disp = if ct.starts_with('第') {
                ct.clone()
            } else {
                format!("第{}章 {ct}", i + 1)
            };
            md.push_str(&format!("## {disp}\n\n{body}\n\n"));
        }
    }
    // chapter_count scanner uses lines starting with "- 第"
    // ensure at least those exist (already in 目录)
    // Filename dedup: if same-title file exists with different content, append _2, _3...
    let base_name = format!("{title}.md").replace(['/', '\\'], "_");
    let mut path = dir.join(&base_name);
    if path.exists() {
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        // If existing file doesn't start with same title heading, it's a different book
        let existing_title = existing.lines().find(|l| l.starts_with("# ")).unwrap_or("");
        if existing_title != format!("# {title}") {
            let stem = format!("{title}").replace(['/', '\\'], "_");
            for i in 2..100 {
                let candidate = dir.join(format!("{stem}_{i}.md"));
                if !candidate.exists() {
                    path = candidate;
                    break;
                }
            }
        }
    }
    std::fs::write(&path, md).map_err(|e| e.to_string())?;
    Ok((slug, path))
}

/// 将蒸馏报告 JSON 渲染为书架可读的 markdown。
/// 书架 md 约定：首行 `# 标题`（scan_shelf 用其生成 slug/标题），
/// 小节按 `- 第N节` 分章（chapter_count 统计）。报告各字段转成可读小节。
fn render_distill_report_md(report: &serde_json::Value, title: &str) -> Option<String> {
    fn s(v: &serde_json::Value, key: &str) -> String {
        v.get(key).and_then(|x| x.as_str()).unwrap_or("").trim().to_string()
    }
    #[allow(dead_code)] // [P7] JSON 数组长度辅助预留
    fn arr_len(v: &serde_json::Value, key: &str) -> usize {
        v.get(key).and_then(|x| x.as_array()).map(|a| a.len()).unwrap_or(0)
    }
    let mut md = String::new();
    md.push_str(&format!("# {}（蒸馏报告）\n\n", title));
    md.push_str("> 本文件由小说蒸馏流水线自动生成，记录角色/世界观/节点/检定等全部蒸馏产物。\n\n");
    let mut sec = 0usize;
    // 角色
    if let Some(chars) = report.get("characters").and_then(|c| c.as_array()) {
        if !chars.is_empty() {
            sec += 1;
            md.push_str(&format!("## 一、角色卡（{} 人）\n\n", chars.len()));
            for c in chars {
                let name = s(c, "name");
                if name.is_empty() { continue; }
                let mut card = format!("### {}\n\n", name);
                let role = s(c, "role");
                if !role.is_empty() { card.push_str(&format!("- 身份/定位：{}\n", role)); }
                let personality = s(c, "personality");
                if !personality.is_empty() { card.push_str(&format!("- 性格：{}\n", personality)); }
                let speech = s(c, "speechStyle");
                if !speech.is_empty() { card.push_str(&format!("- 说话风格：{}\n", speech)); }
                let appearance = s(c, "appearance");
                if !appearance.is_empty() { card.push_str(&format!("- 外貌：{}\n", appearance)); }
                let motivation = s(c, "motivation");
                if !motivation.is_empty() { card.push_str(&format!("- 动机：{}\n", motivation)); }
                if let Some(dialogs) = c.get("exampleDialogs").and_then(|d| d.as_array()) {
                    if !dialogs.is_empty() {
                        card.push_str("- 示例对白：\n");
                        for d in dialogs.iter().take(8) {
                            if let Some(t) = d.as_str() {
                                if !t.trim().is_empty() {
                                    card.push_str(&format!("  - 「{}」\n", t.trim()));
                                }
                            }
                        }
                    }
                }
                if let Some(rels) = c.get("relationships").and_then(|r| r.as_array()) {
                    if !rels.is_empty() {
                        card.push_str("- 关系：\n");
                        for r in rels.iter().take(8) {
                            if let Some(t) = r.as_str() {
                                card.push_str(&format!("  - {}\n", t.trim()));
                            }
                        }
                    }
                }
                md.push_str(&card);
                md.push('\n');
            }
        }
    }
    // 世界观
    if let Some(lore) = report.get("lore").and_then(|l| l.as_array()) {
        if !lore.is_empty() {
            sec += 1;
            md.push_str(&format!("## 二、世界观/词条（{} 条）\n\n", lore.len()));
            for l in lore.iter().take(50) {
                let t = s(l, "title");
                if !t.is_empty() {
                    md.push_str(&format!("- {}\n", t));
                }
            }
            md.push('\n');
        }
    }
    // 世界线
    if let Some(wl) = report.get("worldline").and_then(|w| w.as_array()) {
        if !wl.is_empty() {
            sec += 1;
            md.push_str(&format!("## 三、世界线/关键节点（{} 条）\n\n", wl.len()));
            for w in wl.iter().take(50) {
                let t = s(w, "title");
                if !t.is_empty() {
                    md.push_str(&format!("- {}\n", t));
                }
            }
            md.push('\n');
        }
    }
    // 章节/节点
    let node_count = report.get("beats").and_then(|b| b.get("node_count")).and_then(|n| n.as_u64()).unwrap_or(0);
    if node_count > 0 {
        sec += 1;
        md.push_str(&format!("## 四、章节/节点（{} 节点）\n\n", node_count));
        md.push_str(&format!("- 节点数：{}\n", node_count));
        let beat_count = report.get("beats").and_then(|b| b.get("beat_count")).and_then(|n| n.as_u64()).unwrap_or(0);
        md.push_str(&format!("- 硬节拍数：{}\n", beat_count));
        let exits = report.get("exits").and_then(|e| e.as_u64()).unwrap_or(0);
        md.push_str(&format!("- 出口数：{}\n\n", exits));
    }
    // 检定规则
    if let Some(checks) = report.get("rule_checks").and_then(|r| r.as_array()) {
        if !checks.is_empty() {
            sec += 1;
            md.push_str(&format!("## 五、规则检定（{} 个）\n\n", checks.len()));
            for c in checks.iter().take(30) {
                let label = s(c, "label");
                let id = s(c, "id");
                let dice = s(c, "dice");
                if !label.is_empty() {
                    md.push_str(&format!("- {}（{}）骰{}\n", label, id, dice));
                }
            }
            md.push('\n');
        }
    }
    // 事件包
    if let Some(eps) = report.get("event_packages").and_then(|e| e.as_array()) {
        if !eps.is_empty() {
            sec += 1;
            md.push_str(&format!("## 六、事件包（{} 个）\n\n", eps.len()));
            for p in eps.iter().take(30) {
                let name = s(p, "name");
                let id = s(p, "id");
                if !name.is_empty() {
                    md.push_str(&format!("- {}（{}）\n", name, id));
                }
            }
            md.push('\n');
        }
    }
    // 角色卡质量统计
    if let Some(stats) = report.get("character_card_stats").and_then(|c| c.as_array()) {
        if !stats.is_empty() {
            sec += 1;
            md.push_str(&format!("## 七、角色卡质量（{} 张）\n\n", stats.len()));
            md.push_str("| 角色 | 卡片字数 | 证据引用数 |\n|---|---|---|\n");
            for st in stats {
                let name = s(st, "name");
                let chars = st.get("chars_len").and_then(|x| x.as_u64()).unwrap_or(0);
                let refs = st.get("evidence_refs_len").and_then(|x| x.as_u64()).unwrap_or(0);
                md.push_str(&format!("| {} | {} | {} |\n", name, chars, refs));
            }
            md.push('\n');
        }
    }
    // 叙事风格
    let ns = s(report, "narrative_style");
    if !ns.is_empty() {
        sec += 1;
        md.push_str(&format!("## 八、叙事风格\n\n{}\n\n", ns));
    }
    // 缺失关键角色
    if let Some(missing) = report.get("missing_key_characters").and_then(|m| m.as_array()) {
        if !missing.is_empty() {
            sec += 1;
            md.push_str(&format!("## 九、缺失关键角色（{} 个）\n\n", missing.len()));
            for m in missing.iter().take(20) {
                if let Some(t) = m.as_str() {
                    md.push_str(&format!("- {}\n", t));
                }
            }
            md.push('\n');
        }
    }
    // 演员模板
    if let Some(tpls) = report.get("actor_templates").and_then(|t| t.as_array()) {
        if !tpls.is_empty() {
            sec += 1;
            md.push_str(&format!("## 十、角色状态模板（{} 个）\n\n", tpls.len()));
            for t in tpls.iter().take(30) {
                let name = s(t, "name");
                let fc = t.get("field_count").and_then(|x| x.as_u64()).unwrap_or(0);
                if !name.is_empty() {
                    md.push_str(&format!("- {}（{} 字段）\n", name, fc));
                }
            }
            md.push('\n');
        }
    }
    if sec == 0 {
        return None;
    }
    Some(md)
}

/// POST /api/v1/crawler/novels — import TXT/MD onto bookshelf (+ optional Story Pack).
pub async fn novels_import(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ShelfImportBody>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    // 优先使用 `data`（base64 原始字节）走编码嗅探，避免 GBK/Big5 源 txt 被按 UTF-8 硬解成乱码；
    // 否则回退到调用方已解码好的 `text`。
    let text = match body.data.as_deref() {
        Some(b64) if !b64.trim().is_empty() => {
            use base64::{engine::general_purpose::STANDARD as B64, Engine};
            match B64.decode(b64.trim()) {
                Ok(raw) => decode_text(&raw).0,
                Err(_) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({"ok": false, "error": "invalid base64 in data"})),
                    )
                        .into_response();
                }
            }
        }
        _ => body.text.clone(),
    };
    let text = text.trim();
    if text.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": "empty text"})),
        )
            .into_response();
    }
    if text.len() > 20_000_000 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": "text too large (max 20M chars)"})),
        )
            .into_response();
    }
    // P7: reject script-capable HTML in imported plain text.
    let threats = kaleido_core::inspect_imported_plain_text(text);
    if !threats.is_empty() {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"ok": false, "error": "UNSAFE_IMPORT_CONTENT", "threats": threats})),
        )
            .into_response();
    }
    let title = body
        .title
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .or_else(|| {
            text.lines()
                .find(|l| l.starts_with("# "))
                .map(|l| l.trim_start_matches("# ").trim().to_string())
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "未命名".into());
    let chapters = split_novel_chapters(text);
    let (slug, path) = match write_shelf_markdown(&title, text, &chapters) {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"ok": false, "error": e})),
            )
                .into_response();
        }
    };
    let mut pack_id: Option<String> = None;
    let do_pack = body.to_pack.unwrap_or(true);
    if do_pack {
        let pid = format!(
            "pack-shelf-{}-{}",
            slug.chars().take(24).collect::<String>(),
            chrono::Utc::now().timestamp() % 100_000
        );
        let (pack, bodies) = build_pack_from_chapters(&title, &chapters, &pid, &slug);
        match state.packs.save(pack) {
            Ok(saved) => {
                for (rel, content) in bodies {
                    let _ = state.packs.write_chapter_body(&saved.id, &rel, &content);
                }
                pack_id = Some(saved.id.clone());
                // 自动角色抽取（异步，不阻塞导入；失败静默）
                spawn_auto_cast_extraction(state.clone(), saved.id);
            }
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"ok": false, "error": format!("pack save: {e}"), "slug": slug})),
                )
                    .into_response();
            }
        }
    }
    Json(json!({
        "ok": true,
        "slug": slug,
        "title": title,
        "chapterCount": chapters.len(),
        "path": path.display().to_string(),
        "packId": pack_id,
    }))
    .into_response()
}

/// POST /api/v1/crawler/novels/{slug}/to-pack — promote shelf novel to Story Tavern pack.
pub async fn novel_to_pack(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(slug): Path<String>,
    body: Option<Json<ShelfToPackBody>>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let force = body
        .as_ref()
        .and_then(|b| b.force)
        .unwrap_or(false);
    let override_id = body.and_then(|Json(b)| b.pack_id);
    // load content via same path resolution as novel_content
    let shelf = scan_shelf();
    let entry = match shelf.into_iter().find(|e| e.slug == slug) {
        Some(e) => e,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"ok": false, "error": "novel not found"})),
            )
                .into_response();
        }
    };
    if !force {
        if let Some(existing) = find_existing_pack_for_shelf(&state.packs, &slug, &entry.title) {
            return Json(json!({
                "ok": true,
                "packId": existing,
                "title": entry.title,
                "existed": true,
            }))
            .into_response();
        }
    }
    // read file
    let dir = shelf_dir();
    let slug_tag = shelf_slug(&entry.title);
    let mut file_path = None;
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("md") {
                continue;
            }
            let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            if shelf_slug(stem).contains(&slug_tag) {
                file_path = Some(p);
                break;
            }
        }
    }
    let content = match file_path.and_then(|p| std::fs::read_to_string(p).ok()) {
        Some(c) => c,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"ok": false, "error": "file not found"})),
            )
                .into_response();
        }
    };
    let chapters = split_novel_chapters(&content);
    let pid = override_id.filter(|s| !s.trim().is_empty()).unwrap_or_else(|| {
        format!(
            "pack-shelf-{}-{}",
            slug.chars().take(24).collect::<String>(),
            chrono::Utc::now().timestamp() % 100_000
        )
    });
    let (pack, bodies) = build_pack_from_chapters(&entry.title, &chapters, &pid, &slug);
    match state.packs.save(pack) {
        Ok(saved) => {
            for (rel, body) in bodies {
                let _ = state.packs.write_chapter_body(&saved.id, &rel, &body);
            }
            Json(json!({
                "ok": true,
                "packId": saved.id,
                "title": saved.title,
                "chapterCount": chapters.len(),
                "existed": false,
            }))
            .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "error": e.to_string()})),
        )
            .into_response(),
    }
}

/// POST /api/v1/crawler/novels/{slug}/distil — 角色蒸馏（LLM + 向量检索）。
///
/// 复用 `novel_to_pack` 的 shelf 解析逻辑（读 shelf md → split_novel_chapters →
/// build_pack_from_chapters 生成基础 pack），随后用 `distill_pack_characters`
/// 蒸馏出有血有肉的角色卡替换 `pack.characters` 并落盘。
pub async fn novel_distil(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(slug): Path<String>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    // 读 shelf md（复用 novel_to_pack 的解析逻辑）
    let shelf = scan_shelf();
    let entry = match shelf.into_iter().find(|e| e.slug == slug) {
        Some(e) => e,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"ok": false, "error": "novel not found"})),
            )
                .into_response();
        }
    };
    let dir = shelf_dir();
    let slug_tag = shelf_slug(&entry.title);
    let mut file_path = None;
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("md") {
                continue;
            }
            let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            if shelf_slug(stem).contains(&slug_tag) {
                file_path = Some(p);
                break;
            }
        }
    }
    let content = match file_path.and_then(|p| std::fs::read_to_string(p).ok()) {
        Some(c) => c,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"ok": false, "error": "file not found"})),
            )
                .into_response();
        }
    };
    let chapters = split_novel_chapters(&content);
    // 带 chXX 章节号，供证据检索与 evidence_refs 使用
    // A0 完整版 (2026-08-19): 编号用原著章号（缺章自动跳号），与 pack.chapters.id 对齐；
    // 此前用切分序号 i+1，与 build_pack_from_chapters 的编号错位风险（D1）。
    let chapter_ids = chapter_id_seq(&chapters);
    let distil_chapters: Vec<(String, String)> = chapters
        .iter()
        .enumerate()
        .map(|(i, (_t, b))| (chapter_ids[i].clone(), b.clone()))
        .collect();

    // 角色蒸馏（LLM + 向量检索）
    let max_chars = 6000usize;
    // 先确定 pack id（供增量存档：每蒸馏完一个角色立即写回 pack）
    let pid = find_existing_pack_for_shelf(&state.packs, &slug, &entry.title).unwrap_or_else(|| {
        format!(
            "pack-shelf-{}-{}",
            slug.chars().take(24).collect::<String>(),
            chrono::Utc::now().timestamp() % 100_000
        )
    });
    let chars =
        match crate::convert::distill_pack_characters(&state, &entry.title, &distil_chapters, max_chars, Some(&pid))
            .await
        {
            Ok(c) => c,
            Err(e) => {
                return (
                    StatusCode::BAD_GATEWAY,
                    Json(json!({"ok": false, "error": e})),
                )
                    .into_response();
            }
        };

    // 生成基础 pack 结构（章节/节点/正文不变），用蒸馏结果替换角色
    let (mut pack, bodies) = build_pack_from_chapters(&entry.title, &chapters, &pid, &slug);
    pack.characters = chars;
    // 方案A(2026-08-16)：node.entry 叙事入口蒸馏。
    // 回推最初设计：entry 应为叙事入口句（"雨夜，玩家抵达旧茶馆门前"），
    // 而非 build_pack_from_chapters 的占位 "本章开始"。失败仅 warn，不阻主线。
    match crate::convert::distill_node_entries(&state, &entry.title, &pack.nodes, &distil_chapters).await {
        Ok(entries) => {
            for n in &mut pack.nodes {
                if let Some(e) = entries.get(&n.id) {
                    if !e.trim().is_empty() {
                        n.entry = e.trim().to_string();
                    }
                }
            }
        }
        Err(e) => tracing::warn!(err = %e, "节点入口蒸馏失败，保留占位 entry"),
    }
    match state.packs.save(pack) {
        Ok(saved) => {
            for (rel, body) in bodies {
                let _ = state.packs.write_chapter_body(&saved.id, &rel, &body);
            }
            let characters: Vec<Value> = saved
                .characters
                .iter()
                .map(|c| {
                    json!({
                        "name": c.name,
                        "personality": c.personality,
                        "speechStyle": c.speech_style,
                        "exampleDialogs": c.example_dialogs,
                        "boundaries": c.boundaries,
                        "motivation": c.motivation,
                        "relationships": c.relationships,
                        "evidenceRefs": c.evidence_refs,
                        "mentalModels": c.mental_models,
                        "decisionHeuristics": c.decision_heuristics,
                        "beliefs": c.beliefs,
                    })
                })
                .collect();
            Json(json!({
                "ok": true,
                "packId": saved.id,
                "title": saved.title,
                "characterCount": saved.characters.len(),
                "characters": characters,
            }))
            .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "error": e.to_string()})),
        )
            .into_response(),
    }
}

/// 分阶段落盘：pack.json 原子写（PackStore::save 内部 temp+rename）+ chapter bodies。
/// bodies 只在 chapters 目录缺失时一次性全量写入（后续阶段只有 pack 变化，不再重写）。
/// 保存失败不 panic，返回 Err 由调用方 push 错误事件（阶段继续，最终 save 覆盖）。
fn save_pack_checkpoint(
    state: &AppState,
    pack: &kaleido_core::StoryPack,
    bodies: &[(String, String)],
) -> Result<(), String> {
    let saved = state.packs.save(pack.clone()).map_err(|e| e.to_string())?;
    let chapters_dir = state
        .app_state
        .data_root()
        .story_packs_dir()
        .join(&saved.id)
        .join("chapters");
    if !chapters_dir.exists() {
        for (rel, body) in bodies {
            let _ = state.packs.write_chapter_body(&saved.id, rel, body);
        }
    }
    Ok(())
}

/// 世界线旁挂文件写盘（temp + rename 原子替换，避免中途写坏 JSON）。
fn write_worldline_atomic(pack_dir: &std::path::Path, worldline: &Value) -> std::io::Result<()> {
    std::fs::create_dir_all(pack_dir)?;
    // 通用兜底：剔除 U+FFFD 坏字残骸（同 story_tavern.rs::write_atomic）。不破坏 JSON 语法。
    let body = serde_json::to_string_pretty(worldline)
        .unwrap_or_else(|_| "[]".to_string())
        .replace('\u{FFFD}', "");
    let tmp = pack_dir.join("worldline.json.tmp");
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, pack_dir.join("worldline.json"))
}

fn worldline_is_empty(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
        Value::String(s) => s.trim().is_empty(),
        _ => false,
    }
}

/// 人工介入控制门：每个阶段 LLM 调用前调用一次。
/// 返回 `true` = 继续执行；返回 `false` = job 已被取消/置终态，exec 应立即停止。
///
/// 逻辑：
/// 1. 若 job status 已非 active（running/queued）→ 说明被 cancel 或置终态 → 停止。
/// 2. 若 control.action == "pause" → 推事件，自旋等待直到 resume 或 job 被取消。
async fn job_control_gate(jobs: &JobStore, run_id: &str) -> bool {
    // 立即取消检查：job 若已被 cancel() → status=cancelled（非 active）→ 停止
    if let Some(rec) = jobs.get(run_id) {
        let st = kaleido_core::normalize_job_status(&rec.status);
        if !kaleido_core::is_active_job_status(&st) {
            return false;
        }
    }
    // 暂停等待：control=="pause" → 自旋直到 resume 或取消
    loop {
        match jobs.control_action(run_id).as_deref() {
            Some("pause") => {
                let _ = jobs.push_event(
                    run_id,
                    JobEvent::progress("已暂停，等待继续…", 0.0),
                    None,
                    None,
                );
                tokio::time::sleep(StdDuration::from_millis(500)).await;
                // 暂停循环中若被取消，立即退出
                if let Some(rec) = jobs.get(run_id) {
                    let st = kaleido_core::normalize_job_status(&rec.status);
                    if !kaleido_core::is_active_job_status(&st) {
                        return false;
                    }
                }
            }
            Some("resume") => {
                let _ = jobs.clear_control(run_id);
                let _ = jobs.push_event(
                    run_id,
                    JobEvent::progress("已恢复", 0.0),
                    None,
                    None,
                );
                return true;
            }
            _ => return true,
        }
    }
}

/// Shelf 世界蒸馏的后台执行体（可复用）。
///
/// 创建任务后的 tokio::spawn 与服务重启后的恢复调度共用本函数：
/// - 开头重建章节与 pack 基底，读取磁盘既有 pack.json —— 磁盘有则以其为工作副本继续，
///   未完成阶段写进同一 pack 对象；已完成阶段直接跳过（幂等续跑）。
/// - 每个阶段完成即落盘一次，任何阶段完成后服务被杀，该阶段产物已持久化。
/// - 保留 push_event 进度；导出错误统一 complete(failed)。
pub async fn exec_shelf_distil_world(
    state: AppState,
    run_id: String,
    slug: String,
    title: String,
    resume_meta: Option<Value>,
) {
    let jobs = state.jobs.clone();
    let run_id_job = run_id.clone();
    let progress = |msg: &str, p: f64| {
        let _ = jobs.push_event(
            &run_id_job,
            JobEvent::progress(msg.to_string(), p),
            Some(p),
            None,
        );
    };
    let fail_job = |stage: &str, e: &str, p: f64| {
        let _ = jobs.push_event(
            &run_id_job,
            JobEvent::error(format!("{stage}: {e}")),
            Some(p),
            None,
        );
        let _ = jobs.complete(&run_id_job, "failed", None, Some(format!("{stage}: {e}")));
    };

    // ── 与 handler 相同的 shelf md 解析（恢复续跑时独立重算）──
    let shelf = scan_shelf();
    let entry = match shelf.into_iter().find(|e| e.slug == slug) {
        Some(e) => e,
        None => {
            fail_job("shelf", "novel not found", 0.0);
            return;
        }
    };
    let dir = shelf_dir();
    let slug_tag = shelf_slug(&entry.title);
    let mut file_path = None;
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("md") {
                continue;
            }
            let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            if shelf_slug(stem).contains(&slug_tag) {
                file_path = Some(p);
                break;
            }
        }
    }
    let content = match file_path.and_then(|p| std::fs::read_to_string(p).ok()) {
        Some(c) => c,
        None => {
            fail_job("shelf", "file not found", 0.0);
            return;
        }
    };
    let chapters = split_novel_chapters(&content);
    // A0 完整版 (2026-08-19): 编号用原著章号（缺章自动跳号），与 pack.chapters.id 对齐（D1）。
    let chapter_ids = chapter_id_seq(&chapters);
    let distil_chapters: Vec<(String, String)> = chapters
        .iter()
        .enumerate()
        .map(|(i, (_t, b))| (chapter_ids[i].clone(), b.clone()))
        .collect();
    let max_chars = 6000usize;

    // ── 工作副本：磁盘已有 pack → 复用之；否则新建基底 ──
    let pid = find_existing_pack_for_shelf(&state.packs, &slug, &entry.title).unwrap_or_else(|| {
        format!(
            "pack-shelf-{}-{}",
            slug.chars().take(24).collect::<String>(),
            chrono::Utc::now().timestamp() % 100_000
        )
    });
    let (fresh_pack, bodies) = build_pack_from_chapters(&entry.title, &chapters, &pid, &slug);
    let disk_pack = state.packs.get(&pid).ok();
    let mut pack = disk_pack.clone().unwrap_or_else(|| fresh_pack.clone());
    if let Some(meta) = &resume_meta {
        tracing::info!(job=%run_id, resume_meta=%meta, "shelf distil world resume");
    }
    // 指定角色名单（可选）：非空时跳过角色谱，只按名单蒸馏并合并进 pack
    let named_chars: Vec<String> = resume_meta
        .as_ref()
        .and_then(|m| m.get("characters").cloned())
        .and_then(|v| match v {
            serde_json::Value::Array(a) => Some(a),
            _ => None,
        })
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    // 各阶段完成推断（读磁盘产物，幂等续跑）
    // 角色阶段完成判定:存在"非默认蒸馏卡" 且 其中至少一个 importance=high(主角在场)。
    // 修复:旧版 only-any 判定下,只要有任意 1 张旧卡就跳过蒸馏,导致主角(如庄眉)缺失时
    // 重跑永远不触发。现在要求 high 主角在场才视为完成,否则重蒸馏。
    let has_distilled = pack
        .characters
        .iter()
        .any(|c| {
            !c.id.starts_with("c-narrator")
                && !c.id.starts_with("c-player")
                && !c.id.starts_with("c-cast-")
        });
    let has_high_char = pack
        .characters
        .iter()
        .any(|c| {
            let imp = c.importance.to_lowercase();
            let r = c.role.to_lowercase();
            imp == "high" || r.contains("protagonist") || r.contains("主角")
        });
    // 全部 high 角色在场判定: 与角色谱(roster-diag.json)的 high 名单比对,
    // 缺任何一个 high(如庄眉)都必须重蒸馏,不能只凭"有 high"就跳过。
    // roster-diag.json 由 distill_pack_characters 每次角色谱阶段落盘（pack 目录隔离）。
    let roster_high_covered = state
        .packs
        .pack_dir(&pack.id)
        .ok()
        .map(|d| d.join("roster-diag.json"))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.get("candidates").cloned())
        .and_then(|cands| {
            let roster_high: Vec<String> = cands
                .as_array()?
                .iter()
                .filter(|c| c.get("importance").and_then(|i| i.as_str()).unwrap_or("").eq_ignore_ascii_case("high"))
                .filter_map(|c| c.get("name").and_then(|n| n.as_str()).map(|s| s.to_string()))
                .collect();
            if roster_high.is_empty() {
                return Some(true); // 角色谱无 high,不阻塞
            }
            let have_high: Vec<&str> = pack
                .characters
                .iter()
                .filter(|c| {
                    let imp = c.importance.to_lowercase();
                    let r = c.role.to_lowercase();
                    imp == "high" || r.contains("protagonist") || r.contains("主角")
                })
                .map(|c| c.name.as_str())
                .collect();
            // 每个 roster high 名称,在已有 high 卡里能按名字/包含关系找到即覆盖
            Some(roster_high.iter().all(|rh| {
                have_high.iter().any(|h| {
                    h.contains(rh.as_str()) || rh.contains(h)
                })
            }))
        })
        .unwrap_or(true);
    let chars_done = has_distilled && has_high_char && roster_high_covered;
    let world_done = pack.lore_entries.iter().any(|l| {
        l.get("id")
            .and_then(|v| v.as_str())
            .map(|s| s != "lore-blurb")
            .unwrap_or(false)
    });
    let beats_done = pack.nodes.iter().any(|n| !n.locked_beats.is_empty());
    let exits_done = pack
        .nodes
        .iter()
        .any(|n| n.exit.len() > 1 || n.exit.iter().any(|e| e.when != "继续"));
    let mut missing_key_chars: Vec<String> = Vec::new();
    let mut worldline: Value = json!([]);
    let worldline_done = std::fs::read_to_string(
        state
            .app_state
            .data_root()
            .story_packs_dir()
            .join(&pack.id)
            .join("worldline.json"),
    )
    .ok()
    .map(|s| {
        if let Ok(v) = serde_json::from_str::<Value>(&s) {
            worldline = v;
            // [fix 2026-08-15 覆盖度检查] 只判非空会让旧版不完整产物（如 8-11 只覆盖
            // ch01-04 的 7 条）永久跳过重生成（兔子想吃窝边草 13 章书 2/3 世界线缺失）。
            // 覆盖章节数 ≥ 全书章节数的 1/2 才算完成，否则重跑。
            if worldline_is_empty(&worldline) {
                false
            } else {
                let covered: std::collections::HashSet<String> = worldline
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|e| e.get("chapter").and_then(|c| c.as_str()))
                            .map(|c| c.to_string())
                            .collect()
                    })
                    .unwrap_or_default();
                let total = distil_chapters.len();
                let coverage = if total > 0 {
                    covered.len() as f32 / total as f32
                } else {
                    1.0
                };
                coverage >= 0.5
            }
        } else {
            false
        }
    })
    .unwrap_or(false);
    let events_done = !pack.event_packages.is_empty();
    let actors_done = !pack.actor_state_config.templates.is_empty();
    let style_done = pack
        .stage_director
        .resolved_snapshot
        .as_ref()
        .map(|s| s.narrative_style.is_some() || s.rule_system.is_some())
        .unwrap_or(false);

    // 0) 基底落盘：0.03 切章完成即写 pack + chapter bodies（任何阶段之前先持久化基础）
    progress("准备完成：章节已切分", 0.03);
    if let Err(e) = save_pack_checkpoint(&state, &pack, &bodies) {
        let _ = jobs.push_event(
            &run_id_job,
            JobEvent::error(format!("base_save: {e}")),
            Some(0.03),
            None,
        );
    }

    // 1) 角色蒸馏（LLM + 向量检索）：替换默认「旁白/读者」
    if !job_control_gate(&jobs, &run_id_job).await {
        return;
    }
    if !named_chars.is_empty() {
        // 指定角色蒸馏：跳过角色谱与 chars_done 门，只蒸馏名单角色并合并进 pack
        progress("指定角色蒸馏中…", 0.15);
        match crate::convert::distill_named_characters(
            &state,
            &title,
            &distil_chapters,
            &named_chars,
            Some(&pack.id),
        )
        .await
        {
            Ok(c) => {
                // A1-static(精简): 指定产物必填字段完整性检查。
                // 不合格(空壳/薄卡)直接丢弃、不入 pack——避免空壳卡覆盖既有同名好卡,
                // 也避免 importance=high 的空壳卡堵塞 chars_done 门导致无法自动重蒸。
                let mut c_valid: Vec<_> = Vec::with_capacity(c.len());
                for ch in c {
                    let mut missing_fields: Vec<&str> = Vec::new();
                    if ch.name.trim().is_empty() { missing_fields.push("name"); }
                    if ch.role.trim().is_empty() { missing_fields.push("role"); }
                    if ch.personality.trim().is_empty() { missing_fields.push("personality"); }
                    if ch.speech_style.trim().is_empty() { missing_fields.push("speech_style"); }
                    if ch.motivation.trim().is_empty() { missing_fields.push("motivation"); }
                    if !missing_fields.is_empty() {
                        tracing::warn!(job=%run_id_job, char=%ch.name, missing=?missing_fields, "A1-static: 指定角色蒸馏必填字段缺失，该卡丢弃(不入 pack)");
                    } else {
                        c_valid.push(ch);
                    }
                }
                if c_valid.is_empty() {
                    tracing::warn!(job=%run_id_job, "指定角色蒸馏: 名单角色全部失败或无证据");
                }
                // 合并回工作副本：同名替换、其余追加（仅合格卡）
                for ch in &c_valid {
                    pack.characters.retain(|x| x.name != ch.name);
                }
                pack.characters.extend(c_valid);
                remap_node_present_to_distil(&mut pack.nodes, &pack.characters, &bodies);
                progress("指定角色蒸馏完成", 0.15);
                if let Err(e) = save_pack_checkpoint(&state, &pack, &bodies) {
                    let _ = jobs.push_event(
                        &run_id_job,
                        JobEvent::error(format!("named_characters_save: {e}")),
                        Some(0.15),
                        None,
                    );
                }
            }
            Err(e) => {
                fail_job("characters", &e, 0.15);
                return;
            }
        }
    } else if chars_done {
        progress("角色已完成，跳过", 0.15);
    } else {
        progress("角色蒸馏中…", 0.15);
        match crate::convert::distill_pack_characters(&state, &title, &distil_chapters, max_chars, Some(&pack.id))
            .await
        {
            Ok(c) => {
                // A1: 主角必在断言 — 角色蒸馏完成后检查关键角色是否在场
                // 判定依据:distill 产出的 PackCharacterRef.importance(high/medium/low)
                // 由角色谱写入;若蒸馏结果缺失全部 high 角色,记 missing 供人工检核。
                // 扩展(吸收 TavernWeave validation 方法论):
                //   A1-static   — 模块契约字段完整性(必填字段非空;未知值仅允许 gender/appearance/opening_*)
                //   A1-evidence — personality/beliefs/mental_models 结论带 evidence_refs(缺失→warning)
                //   A1-cover    — 角色谱 high 名单全部覆盖(roster_high_covered,已在 chars_done 判定)
                //   A1-fffd     — 产物无 U+FFFD(convert 重试层已处理,此处兜底检查)
                let high_count = c.iter().filter(|ch| {
                    let imp = ch.importance.to_lowercase();
                    let r = ch.role.to_lowercase();
                    imp == "high" || r.contains("protagonist") || r.contains("主角")
                }).count();
                let non_narrator_count = c.iter().filter(|ch| {
                    let r = ch.role.to_lowercase();
                    !r.contains("narrator") && !r.contains("旁白") && !r.contains("读者") && !r.is_empty()
                }).count();
                // A1-static: 必填字段完整性检查
                let mut static_violations: Vec<String> = Vec::new();
                for ch in &c {
                    let mut missing_fields: Vec<&str> = Vec::new();
                    if ch.name.trim().is_empty() { missing_fields.push("name"); }
                    if ch.role.trim().is_empty() { missing_fields.push("role"); }
                    if ch.personality.trim().is_empty() { missing_fields.push("personality"); }
                    if ch.speech_style.trim().is_empty() { missing_fields.push("speech_style"); }
                    if ch.motivation.trim().is_empty() { missing_fields.push("motivation"); }
                    if !missing_fields.is_empty() {
                        static_violations.push(format!("{}:{}", ch.name, missing_fields.join(",")));
                    }
                }
                if !static_violations.is_empty() {
                    tracing::warn!(
                        violations = ?static_violations,
                        "A1-static: 角色卡必填字段缺失"
                    );
                }
                // A1-evidence: 认知字段结论带证据引用检查
                let mut evidenceless: Vec<String> = Vec::new();
                for ch in &c {
                    let has_refs = !ch.evidence_refs.is_empty();
                    let has_cog = !ch.mental_models.is_empty() || !ch.decision_heuristics.is_empty() || !ch.beliefs.is_empty();
                    if has_cog && !has_refs {
                        evidenceless.push(ch.name.clone());
                    }
                }
                if !evidenceless.is_empty() {
                    tracing::warn!(
                        chars = ?evidenceless,
                        "A1-evidence: 认知结论无 evidence_refs，需人工检核"
                    );
                }
                // A1-fffd: 兜底检查产物无损坏字符
                let fffd_chars: Vec<String> = c.iter()
                    .filter(|ch| {
                        ch.name.contains('\u{fffd}')
                            || ch.personality.contains('\u{fffd}')
                            || ch.motivation.contains('\u{fffd}')
                    })
                    .map(|ch| ch.name.clone())
                    .collect();
                if !fffd_chars.is_empty() {
                    tracing::warn!(
                        chars = ?fffd_chars,
                        "A1-fffd: 产物含 U+FFFD 损坏字符，需人工检核"
                    );
                }
                if high_count == 0 {
                    if c.len() > 0 {
                        // 角色谱标记了 high 但蒸馏结果无 high:全部列入疑似缺失(供人工检核)
                        for ch in &c {
                            missing_key_chars.push(format!("{}(high?)", ch.name));
                        }
                    } else {
                        missing_key_chars.push("(无角色)".to_string());
                    }
                }
                if !missing_key_chars.is_empty() {
                    tracing::warn!(
                        missing = ?missing_key_chars,
                        total = c.len(),
                        high_count, non_narrator_count,
                        static_violations = ?static_violations,
                        evidenceless = ?evidenceless,
                        "角色蒸馏完成，但关键角色可能缺失，需人工检核"
                    );
                }
                // [fix] 空壳/薄卡守卫: 缺核心字段(name/personality/speech_style/motivation)
                // 的卡在整体替换前过滤掉——空壳卡会污染 pack, 且 importance=high 的空壳
                // 会堵塞 chars_done 门, 导致角色再也无法被自动重蒸。
                let c_before = c.len();
                let mut c_valid: Vec<_> = Vec::with_capacity(c.len());
                let mut dropped_shells: Vec<String> = Vec::new();
                for ch in c {
                    if ch.name.trim().is_empty()
                        || ch.personality.trim().is_empty()
                        || ch.speech_style.trim().is_empty()
                        || ch.motivation.trim().is_empty()
                    {
                        dropped_shells.push(format!(
                            "{}({})",
                            if ch.name.trim().is_empty() { "(无名)" } else { ch.name.as_str() },
                            ch.importance
                        ));
                    } else {
                        c_valid.push(ch);
                    }
                }
                if !dropped_shells.is_empty() {
                    tracing::warn!(
                        job=%run_id_job,
                        dropped = ?dropped_shells,
                        before = c_before, after = c_valid.len(),
                        "角色蒸馏: 空壳/薄卡被过滤, 不入 pack(防止污染 chars_done 门)"
                    );
                }
                pack.characters = c_valid;
                // [fix] 蒸馏后把 nodes.present 从 c-cast 垃圾占位重建为
                // 每章正文真实出现的蒸馏角色 id，使 build_mainline_opening
                // 能按 present 命中带 opening 的核心角色。
                remap_node_present_to_distil(&mut pack.nodes, &pack.characters, &bodies);
                progress("角色蒸馏完成", 0.15);
                if let Err(e) = save_pack_checkpoint(&state, &pack, &bodies) {
                    let _ = jobs.push_event(
                        &run_id_job,
                        JobEvent::error(format!("characters_save: {e}")),
                        Some(0.15),
                        None,
                    );
                }
            }
            Err(e) => {
                fail_job("characters", &e, 0.15);
                return;
            }
        }
    }

    // 2) 世界树 → pack.lore_entries
    if !job_control_gate(&jobs, &run_id_job).await {
        return;
    }
    if world_done {
        progress("世界树/世界书已完成，跳过", 0.40);
    } else {
        progress("生成世界树/世界书…", 0.40);
        match crate::convert::distill_world_lore(&state, &title, &distil_chapters, max_chars).await {
            Ok(v) => {
                pack.lore_entries = v;
                progress("世界树/世界书完成", 0.40);
                if let Err(e) = save_pack_checkpoint(&state, &pack, &bodies) {
                    let _ = jobs.push_event(
                        &run_id_job,
                        JobEvent::error(format!("world_save: {e}")),
                        Some(0.40),
                        None,
                    );
                }
            }
            Err(e) => {
                fail_job("world_lore", &e, 0.40);
                return;
            }
        }
    }

    // 3) 每章节拍 → nodes[].locked_beats
    if !job_control_gate(&jobs, &run_id_job).await {
        return;
    }
    if beats_done {
        progress("节拍已完成，跳过", 0.55);
    } else {
        progress("生成节拍…", 0.55);
        let beats =
            match crate::convert::distill_locked_beats(&state, &title, &distil_chapters, max_chars)
                .await
            {
                Ok(v) => v,
                Err(e) => {
                    fail_job("locked_beats", &e, 0.55);
                    return;
                }
            };
        for (i, node) in pack.nodes.iter_mut().enumerate() {
            if let Some(b) = beats.get(i) {
                node.locked_beats = b.clone();
            }
        }
        progress("节拍完成", 0.55);
        if let Err(e) = save_pack_checkpoint(&state, &pack, &bodies) {
            let _ = jobs.push_event(
                &run_id_job,
                JobEvent::error(format!("beats_save: {e}")),
                Some(0.55),
                None,
            );
        }
    }

    // 4) 多出口 → nodes[].exit（原单链 continue 时替换为多出口，已有出口保留并补齐）
    if !job_control_gate(&jobs, &run_id_job).await {
        return;
    }
    if exits_done {
        progress("多出口已完成，跳过", 0.65);
    } else {
        progress("生成多出口…", 0.65);
        let node_ids: Vec<String> = pack.nodes.iter().map(|n| n.id.clone()).collect();
        let exits = match crate::convert::distill_exits(
            &state,
            &title,
            &distil_chapters,
            &node_ids,
            max_chars,
        )
        .await
        {
            Ok(v) => v,
            Err(e) => {
                fail_job("exits", &e, 0.65);
                return;
            }
        };
        for (i, node) in pack.nodes.iter_mut().enumerate() {
            if let Some(new_exits) = exits.get(i) {
                if !new_exits.is_empty() {
                    let is_single_continue = node.exit.len() == 1 && node.exit[0].when == "继续";
                    if is_single_continue || node.exit.is_empty() {
                        node.exit = new_exits.clone();
                    } else {
                        let existing: Vec<String> = node.exit.iter().map(|e| e.when.clone()).collect();
                        for ne in new_exits {
                            if !existing.contains(&ne.when) {
                                node.exit.push(ne.clone());
                            }
                        }
                    }
                }
            }
        }
        progress("多出口完成", 0.65);
        if let Err(e) = save_pack_checkpoint(&state, &pack, &bodies) {
            let _ = jobs.push_event(
                &run_id_job,
                JobEvent::error(format!("exits_save: {e}")),
                Some(0.65),
                None,
            );
        }
    }

    // 5) 世界线 → worldline.json（旁挂，不改 pack.json 结构）
    if !job_control_gate(&jobs, &run_id_job).await {
        return;
    }
    if worldline_done {
        progress("世界线已完成，跳过", 0.75);
    } else {
        progress("生成世界线…", 0.75);
        let wl = match crate::convert::distill_worldline(&state, &title, &distil_chapters, max_chars)
            .await
        {
            Ok(v) => v,
            Err(e) => {
                fail_job("worldline", &e, 0.75);
                return;
            }
        };
        worldline = serde_json::Value::Array(wl);
        let pack_dir = state
            .app_state
            .data_root()
            .story_packs_dir()
            .join(&pack.id);
        if let Err(e) = write_worldline_atomic(&pack_dir, &worldline) {
            fail_job("worldline_write", &e.to_string(), 0.75);
            return;
        }
        progress("世界线完成", 0.75);
    }

    // 5.5) 角色关系图谱 → relations.json（Wave C，吸收自 AI-Reader-V2 RelationshipFact）
    //      逐章提取 {from,to,rel,note} 边，旁挂文件 + 回填角色卡 relationships。
    //      独立失败仅 warn，不中止整批。
    if !job_control_gate(&jobs, &run_id_job).await {
        return;
    }
    let relations_done = std::fs::metadata(
        state
            .app_state
            .data_root()
            .story_packs_dir()
            .join(&pack.id)
            .join("relations.json"),
    )
    .map(|_| true)
    .unwrap_or(false);
    // [L0] force_relations: 已有 relations.json 也强制重跑关系图谱（重蒸馏+重新落 graph 桥）
    let force_relations: bool = resume_meta
        .as_ref()
        .and_then(|m| m.get("force_relations"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if relations_done && !force_relations {
        progress("关系图谱已完成，跳过", 0.78);
    } else {
        progress("生成角色关系图谱…", 0.78);
        match crate::convert::distill_chapter_relations(&state, &title, &distil_chapters, max_chars)
            .await
        {
            Ok(chapter_edges) => {
                // 聚合去重：同一 (from,to,rel) 只保留首见 note；跨章同对关系合并 rel 集；
                // [L0] 顺带聚合 chapters 章标题数组（写 graph 演化用）。
                let mut seen: std::collections::HashMap<(String, String), serde_json::Value> =
                    std::collections::HashMap::new();
                for (ci, edges) in chapter_edges.iter().enumerate() {
                    let cid = distil_chapters
                        .get(ci)
                        .map(|(c, _)| c.clone())
                        .unwrap_or_default();
                    for e in edges {
                        let from = e.get("from").and_then(|x| x.as_str()).unwrap_or("").to_string();
                        let to = e.get("to").and_then(|x| x.as_str()).unwrap_or("").to_string();
                        let rel = e.get("rel").and_then(|x| x.as_str()).unwrap_or("").to_string();
                        let key = (format!("{from}::{to}"), rel.clone());
                        let note = e.get("note").and_then(|x| x.as_str()).unwrap_or("");
                        let entry = seen.entry(key).or_insert_with(|| {
                            let mut chs = serde_json::json!([]);
                            if !cid.is_empty() {
                                chs.as_array_mut()
                                    .unwrap()
                                    .push(serde_json::Value::String(cid.clone()));
                            }
                            serde_json::json!({ "from": from, "to": to, "rel": rel, "note": note, "chapters": chs })
                        });
                        if let Some(o) = entry.as_object_mut() {
                            if !cid.is_empty() {
                                let chs = o.entry("chapters").or_insert_with(|| serde_json::json!([]));
                                if let Some(arr) = chs.as_array_mut() {
                                    if !arr.iter().any(|x| x.as_str() == Some(cid.as_str())) {
                                        arr.push(serde_json::Value::String(cid.clone()));
                                    }
                                }
                            }
                        }
                    }
                }
                // [ENT] mut：后续要写入 from_id/to_id/kind 字段。
                let mut edges: Vec<serde_json::Value> = seen.into_values().collect();
                if edges.is_empty() {
                    tracing::warn!("关系图谱蒸馏: 结果为空，跳过关系字段");
                } else {
                    // 旁挂 relations.json（与 worldline.json 同模式）
                    let pack_dir = state
                        .app_state
                        .data_root()
                        .story_packs_dir()
                        .join(&pack.id);
                    // [ENT] 实体解析层：先把每条边端点解析到具体角色卡 id（from_id/to_id），
                    // 再做幽灵对账 / 稀疏告警 / 角色卡回填。
                    let cards: Vec<(String, String, String, String)> = pack
                        .characters
                        .iter()
                        .map(|c| {
                            (
                                c.id.clone(),
                                c.name.clone(),
                                c.role.clone(),
                                c.importance.clone(),
                            )
                        })
                        .collect();
                    use kaleido_core::entity_resolve::{
                        classify_endpoint, collect_ghosts, find_sparse_characters,
                        resolve_entity_endpoint, EndpointKind,
                    };
                    // [ENT] BTreeMap：id 排序迭代，entities.json 输出确定性稳定。
                    let mut entity_alias_index: std::collections::BTreeMap<
                        String,
                        std::collections::BTreeSet<String>,
                    > = std::collections::BTreeMap::new();
                    // [ENT] 每条边：解析 from_id/to_id 并写入边（None 时序列化跳过）。
                    for e in edges.iter_mut() {
                        let from = e.get("from").and_then(|x| x.as_str()).unwrap_or("").to_string();
                        let to = e.get("to").and_then(|x| x.as_str()).unwrap_or("").to_string();
                        let from_id = resolve_entity_endpoint(&from, &cards);
                        let to_id = resolve_entity_endpoint(&to, &cards);
                        if let Some(id) = &from_id {
                            if let Some(o) = e.as_object_mut() {
                                o.insert("from_id".into(), serde_json::Value::String(id.clone()));
                            }
                            entity_alias_index.entry(id.clone()).or_default().insert(from.clone());
                        }
                        if let Some(id) = &to_id {
                            if let Some(o) = e.as_object_mut() {
                                o.insert("to_id".into(), serde_json::Value::String(id.clone()));
                            }
                            entity_alias_index.entry(id.clone()).or_default().insert(to.clone());
                        }
                    }
                    // [ENT] id → 卡名 映射（回填 other 用显示身份，避免裸"我"上卡）。
                    let id_to_name: std::collections::HashMap<String, String> = pack
                        .characters
                        .iter()
                        .map(|c| (c.id.clone(), c.name.clone()))
                        .collect();
                    // [ENT] 幽灵清单：解析不出卡 id 的端点 → 写 entities.json + warn。
                    let ghosts = collect_ghosts(&edges, &cards);
                    if !ghosts.is_empty() {
                        let pack_title = pack.title.clone();
                        let entities: Vec<serde_json::Value> = {
                            let mut list = Vec::new();
                            for (id, aliases) in &entity_alias_index {
                                let canonical_name = pack
                                    .characters
                                    .iter()
                                    .find(|c| &c.id == id)
                                    .map(|c| c.name.clone())
                                    .unwrap_or_default();
                                // [ENT] 实体 kind：按 canonical 名端点分级（母亲→kin，夏文嘉→proper）。
                                let kind = match classify_endpoint(&canonical_name) {
                                    EndpointKind::Kin => "kin",
                                    _ => "proper",
                                };
                                list.push(serde_json::json!({
                                    "id": id,
                                    "canonicalName": canonical_name,
                                    "aliases": aliases.iter().cloned().collect::<Vec<_>>(),
                                    "kind": kind,
                                }));
                            }
                            list
                        };
                        // {schemaVersion, packTitle, narratorAliases, entities, ghosts}
                        let entities_doc = serde_json::json!({
                            "schemaVersion": 1,
                            "packTitle": pack_title,
                            "narratorAliases": ["我", "我们", "叙述者", "主角"],
                            "entities": entities,
                            "ghosts": ghosts,
                        });
                        if let Err(e) = std::fs::write(
                            pack_dir.join("entities.json"),
                            serde_json::to_string_pretty(&entities_doc).unwrap_or_default(),
                        ) {
                            tracing::warn!("entities.json 写入失败: {e}");
                        }
                        tracing::warn!(ghosts = ?ghosts, "关系图谱幽灵端点: 无对应角色卡");
                    }
                    // [ENT] 稀疏告警：有卡但未出现在任何边端点。
                    let sparse = find_sparse_characters(&cards, &edges);
                    if !sparse.is_empty() {
                        tracing::warn!(chars = ?sparse, "关系稀疏告警: 以下角色未出现在任何关系边");
                    }
                    if let Err(e) = std::fs::write(
                        pack_dir.join("relations.json"),
                        serde_json::to_string_pretty(&edges).unwrap_or_default(),
                    ) {
                        tracing::warn!("关系图谱写入失败: {e}");
                    } else {
                        // [ENT] 回填角色卡 relationships：优先按 from_id/to_id 匹配（避免
                        // "我→陈妹妹"这类回填到错误卡），无 id 时退回 name 匹配（旧兼容）。
                        // other 一方用解析出的 id 映射到 canonical 卡名（不显示裸"我"）。
                        for c in pack.characters.iter_mut() {
                            let mut rels: Vec<String> = c.relationships.clone();
                            for e in &edges {
                                let from = e.get("from").and_then(|x| x.as_str()).unwrap_or("").to_string();
                                let to = e.get("to").and_then(|x| x.as_str()).unwrap_or("").to_string();
                                let rel = e.get("rel").and_then(|x| x.as_str()).unwrap_or("").to_string();
                                let from_id = e.get("from_id").and_then(|x| x.as_str()).unwrap_or("");
                                let to_id = e.get("to_id").and_then(|x| x.as_str()).unwrap_or("");
                                // 当前卡命中（id 优先；无 id 时 name 兜底）
                                let self_is_from = from_id == c.id || (from_id.is_empty() && from == c.name);
                                let self_is_to = to_id == c.id || (to_id.is_empty() && to == c.name);
                                if !(self_is_from || self_is_to) {
                                    continue;
                                }
                                // other 一方：优先用解析 id → canonical 卡名，否则原始串。
                                let (other_raw, other_id) = if self_is_from {
                                    (to.clone(), to_id)
                                } else {
                                    (from.clone(), from_id)
                                };
                                let other = id_to_name
                                    .get(other_id)
                                    .cloned()
                                    .unwrap_or(other_raw);
                                let s = format!("{other}（{rel}）");
                                if !rels.contains(&s) {
                                    rels.push(s);
                                }
                            }
                            c.relationships = rels;
                        }
                        if let Err(e) = save_pack_checkpoint(&state, &pack, &bodies) {
                            let _ = jobs.push_event(
                                &run_id_job,
                                JobEvent::error(format!("relations_save: {e}")),
                                Some(0.78),
                                None,
                            );
                        }
                    }
                }
                // [L0] 蒸馏→graph.sqlite 桥：把解析出实体 id 的边写入运行时图谱。
                // 运行时 story_tavern 只读 graph.sqlite；不落库则蒸馏关系对运行时不可见
                // （宿醉此前即如此：蒸馏 4 边，graph.sqlite 零记录，注入区块为空）。
                // 幽灵端点（无卡 id）跳过：不为"我/陈妹妹"这类建幽灵角色卡。
                // 注：内层块已构建过 id_to_name（仅供回填），此处重建一份传给桥。
                let id_to_name: std::collections::HashMap<String, String> = pack
                    .characters
                    .iter()
                    .map(|c| (c.id.clone(), c.name.clone()))
                    .collect();
                if let Err(e) = bridge_relations_to_graph(&state.graph, &pack.id, &edges, &id_to_name)
                {
                    tracing::warn!("关系图谱→graph.sqlite 桥失败（不影响旁挂文件）: {e}");
                }
                progress("关系图谱完成", 0.78);
            }
            Err(e) => {
                tracing::warn!("关系图谱蒸馏: 失败，跳过该字段: {e}");
            }
        }
    }

    // 6) 素材库蒸馏：事件包 / 演员状态 / 文风 / 规则检定
    //    每个字段独立失败，仅 warn + 跳过该字段，绝不让单条 LLM 失败中止整批。
    if !job_control_gate(&jobs, &run_id_job).await {
        return;
    }
    if events_done {
        progress("事件包已完成，跳过", 0.85);
    } else {
        progress("素材库：事件包…", 0.85);
        let event_packages = match crate::convert::distill_event_packages(
            &state,
            &title,
            &distil_chapters,
            max_chars,
        )
        .await
        {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("素材库蒸馏[事件包]: 失败，跳过该字段: {e}");
                vec![]
            }
        };
        if event_packages.is_empty() {
            tracing::warn!("素材库蒸馏[事件包]: 结果为空，跳过事件包字段");
        } else {
            pack.event_packages = event_packages.clone();
            pack.stage_director.modules.event_package_ids = event_packages
                .iter()
                .filter(|p| p.enabled)
                .map(|p| p.id.clone())
                .collect();
        }
        progress("事件包完成", 0.85);
        if let Err(e) = save_pack_checkpoint(&state, &pack, &bodies) {
            let _ = jobs.push_event(
                &run_id_job,
                JobEvent::error(format!("events_save: {e}")),
                Some(0.85),
                None,
            );
        }
    }

    if !job_control_gate(&jobs, &run_id_job).await {
        return;
    }
    if actors_done {
        progress("演员状态已完成，跳过", 0.92);
    } else {
        progress("演员状态…", 0.92);
        let actor_state = match crate::convert::distill_actor_state(
            &state,
            &title,
            &pack.characters,
            &distil_chapters,
            max_chars,
        )
        .await
        {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("素材库蒸馏[演员状态]: 失败，跳过该字段: {e}");
                kaleido_core::ActorStatePackConfig::default()
            }
        };
        if actor_state.templates.is_empty() {
            tracing::warn!("素材库蒸馏[演员状态]: 模板为空，跳过 actor_state_config");
        } else {
            pack.actor_state_config = actor_state.clone();
        }
        progress("演员状态完成", 0.92);
        if let Err(e) = save_pack_checkpoint(&state, &pack, &bodies) {
            let _ = jobs.push_event(
                &run_id_job,
                JobEvent::error(format!("actors_save: {e}")),
                Some(0.92),
                None,
            );
        }
    }

    if !job_control_gate(&jobs, &run_id_job).await {
        return;
    }
    if style_done {
        progress("文风与规则检定已完成，跳过", 0.97);
    } else {
        progress("文风与规则检定…", 0.97);
        let narrative_style = match crate::convert::distill_narrative_style(
            &state,
            &title,
            &distil_chapters,
            max_chars,
        )
        .await
        {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("素材库蒸馏[文风]: 失败，跳过该字段: {e}");
                json!({})
            }
        };
        let has_narrative_style = narrative_style
            .as_object()
            .map(|o| !o.is_empty())
            .unwrap_or(false);
        if !has_narrative_style {
            tracing::warn!("素材库蒸馏[文风]: 结果为空，跳过 narrative_style");
        }

        let rule_system = match crate::convert::distill_rule_system(
            &state,
            &title,
            &distil_chapters,
            max_chars,
        )
        .await
        {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("素材库蒸馏[规则检定]: 失败，跳过该字段: {e}");
                json!({ "checks": [] })
            }
        };
        let rule_check_count = rule_system
            .get("checks")
            .and_then(|c| c.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        if rule_check_count == 0 {
            tracing::warn!("素材库蒸馏[规则检定]: 结果为空，跳过 rule_system");
        }

        if has_narrative_style || rule_check_count > 0 {
            let snapshot = pack
                .stage_director
                .resolved_snapshot
                .get_or_insert_with(Default::default);
            if has_narrative_style {
                snapshot.narrative_style = Some(narrative_style);
            }
            if rule_check_count > 0 {
                snapshot.rule_system = Some(rule_system);
            }
        }
        progress("文风与规则检定完成", 0.97);
        if let Err(e) = save_pack_checkpoint(&state, &pack, &bodies) {
            let _ = jobs.push_event(
                &run_id_job,
                JobEvent::error(format!("style_save: {e}")),
                Some(0.97),
                None,
            );
        }
    }

    // 最终落盘 pack.json（camelCase，bodies 已在上游保存过则不再重写）
    match state.packs.save(pack) {
        Ok(saved) => {
            let beat_count: usize = saved.nodes.iter().map(|n| n.locked_beats.len()).sum();
            let mut result = json!({
                "ok": true,
                "packId": saved.id,
                "title": saved.title,
                "lore_count": saved.lore_entries.len(),
                "beat_count": beat_count,
                "character_count": saved.characters.len(),
                "worldline_count": worldline.as_array().map(|a| a.len()).unwrap_or(0),
                "event_package_count": saved.event_packages.len(),
                "actor_template_count": saved.actor_state_config.templates.len(),
                "has_narrative_style": saved
                    .stage_director
                    .resolved_snapshot
                    .as_ref()
                    .and_then(|s| s.narrative_style.as_ref())
                    .is_some(),
                "rule_check_count": saved
                    .stage_director
                    .resolved_snapshot
                    .as_ref()
                    .and_then(|s| s.rule_system.as_ref())
                    .and_then(|r| r.get("checks"))
                    .and_then(|c| c.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0),
            });
            let report = json!({
                "characters": saved.characters.iter().take(30).map(|c| json!({"id": c.id, "name": c.name, "role": c.role})).collect::<Vec<_>>(),
                "lore": saved.lore_entries.iter().take(30).filter_map(|v| v.get("title").and_then(|t| t.as_str())).map(|t| json!({"title": t})).collect::<Vec<_>>(),
                "beats": {"node_count": saved.nodes.len(), "beat_count": beat_count},
                "exits": saved.nodes.iter().map(|n| n.exit.len()).sum::<usize>(),
                "worldline": worldline.as_array().map(|a| a.iter().take(30).filter_map(|v| {
                    v.get("title").or_else(|| v.get("name")).and_then(|t| t.as_str()).map(|t| json!({"title": t}))
                }).collect::<Vec<_>>()).unwrap_or_default(),
                "event_packages": saved.event_packages.iter().take(30).map(|p| json!({"id": p.id, "name": p.name})).collect::<Vec<_>>(),
                "actor_templates": saved.actor_state_config.templates.iter().take(30).map(|(k, t)| json!({"name": k, "field_count": t.fields.len()})).collect::<Vec<_>>(),
                "narrative_style": saved.stage_director.resolved_snapshot.as_ref()
                    .and_then(|s| s.narrative_style.as_ref())
                    .map(|v| v.to_string()),
                "rule_checks": saved.stage_director.resolved_snapshot.as_ref()
                    .and_then(|s| s.rule_system.as_ref())
                    .and_then(|r| r.get("checks"))
                    .and_then(|c| c.as_array())
                    .map(|a| a.iter().take(30).map(|c| json!({"id": c.get("id").and_then(|x| x.as_str()), "label": c.get("label").and_then(|x| x.as_str()), "dice": c.get("dice").and_then(|x| x.as_str())})).collect::<Vec<_>>())
                    .unwrap_or_default(),
                // A2: 每卡字符数 + 证据引用数，一眼识别空卡/水卡
                "character_card_stats": saved.characters.iter().map(|c| {
                    let content_len = c.personality.len()
                        + c.speech_style.len()
                        + c.motivation.len()
                        + c.example_dialogs.iter().map(|s| s.len()).sum::<usize>()
                        + c.boundaries.iter().map(|s| s.len()).sum::<usize>()
                        + c.relationships.iter().map(|s| s.len()).sum::<usize>()
                        + c.mental_models.iter().map(|s| s.len()).sum::<usize>()
                        + c.decision_heuristics.iter().map(|s| s.len()).sum::<usize>()
                        + c.beliefs.iter().map(|s| s.len()).sum::<usize>();
                    json!({
                        "name": c.name,
                        "chars_len": content_len,
                        "evidence_refs_len": c.evidence_refs.len(),
                    })
                }).collect::<Vec<_>>(),
                // A1: 缺失关键角色名数组（可为空）
                "missing_key_characters": missing_key_chars,
            });
            result["report"] = report.clone();
            // 落盘蒸馏报告（best-effort，失败仅 warn）
            let pack_dir = state.app_state.data_root().story_packs_dir().join(&saved.id);
            let report_path = pack_dir.join("distill-report.json");
            if let Ok(text) = serde_json::to_string_pretty(&report) {
                if let Err(e) = std::fs::write(&report_path, text) {
                    tracing::warn!(path = %report_path.display(), error = %e, "write distill report failed");
                }
                // 蒸馏报告同步存档到书架（novel_workspace/），用户可在书架中阅读。
                // 格式复用书架 md 约定：首行 `# 标题`，后续小节按 `- 第N节` 分章；
                // 渲染失败不影响主流程（best-effort）。
                if let Some(md) = render_distill_report_md(&report, &entry.title) {
                    // 文件名用报告完整标题的 slug（含「（蒸馏报告）」后缀，与正文 `# 标题` 一致），
                    // 避免 `__` 分隔符导致 shelf_slug 后 stem 与 slug_tag 不匹配（contains 失败 → file not found）。
                    let shelf_title = format!("{}（蒸馏报告）", entry.title);
                    let shelf_path = crate::crawler::shelf_dir()
                        .join(format!("{}.md", crate::crawler::shelf_slug(&shelf_title)));
                    if let Err(e) = std::fs::write(&shelf_path, md) {
                        tracing::warn!(path = %shelf_path.display(), error = %e, "write distill report shelf md failed");
                    } else {
                        tracing::info!(path = %shelf_path.display(), "distill report archived to bookshelf");
                    }
                }
            }
            progress("保存完成", 1.0);
            let _ = jobs.complete(&run_id_job, "succeeded", Some(result), None);
        }
        Err(e) => {
            fail_job("pack_save", &e.to_string(), 1.0);
        }
    }
}

/// [L0] 蒸馏→graph.sqlite 桥：把解析出实体 id 的关系边写入运行时图谱。
///
/// 语义：
/// - 端点归一：用卡 id → 卡名 映射拿到 canonical 名，经 `resolve_or_create_character`
///   按名幂等解析（graph 里已有同名卡则复用，否则建 ai_suggestion 卡）；
/// - 幽灵端点（无 from_id/to_id）跳过：不建幽灵角色卡；
/// - rel→category：`rel_category::normalize_rel_category` 自由词 → 五类映射；
/// - 幂等 + 跨章演化：每章一个 suggestion_id（`work::from::to::rel::章`），重蒸同章
///   直接返回已有边；不同章同 (from,to,category) 的边由 graph_store 演化合并进
///   `chapters[]`（运行时"关系演化"注入的数据来源）。
fn bridge_relations_to_graph(
    graph: &kaleido_core::graph_store::GraphStore,
    work_id: &str,
    edges: &[serde_json::Value],
    id_to_name: &std::collections::HashMap<String, String>,
) -> Result<(), String> {
    for e in edges {
        let from_id = e.get("from_id").and_then(|x| x.as_str()).unwrap_or("");
        let to_id = e.get("to_id").and_then(|x| x.as_str()).unwrap_or("");
        if from_id.is_empty() || to_id.is_empty() {
            continue; // 幽灵端点：不建卡不落边
        }
        let from_name = id_to_name.get(from_id).cloned().unwrap_or_default();
        let to_name = id_to_name.get(to_id).cloned().unwrap_or_default();
        if from_name.is_empty() || to_name.is_empty() {
            continue;
        }
        let rel = e.get("rel").and_then(|x| x.as_str()).unwrap_or("").to_string();
        if rel.is_empty() {
            continue;
        }
        let note = e.get("note").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let category = kaleido_core::rel_category::normalize_rel_category(&rel);
        let chapters: Vec<String> = e
            .get("chapters")
            .and_then(|x| x.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|c| c.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        if chapters.is_empty() {
            let sid = format!("{work_id}::{from_id}::{to_id}::{rel}");
            graph
                .create_relationship_from_suggestion(
                    work_id,
                    &from_name,
                    &to_name,
                    Some(from_id),
                    Some(to_id),
                    category,
                    &rel,
                    &note,
                    None,
                    &sid,
                )
                .map_err(|err| {
                    format!("create_relationship_from_suggestion({from_name}→{to_name} {rel}): {err}")
                })?;
        } else {
            for ch in &chapters {
                let sid = format!("{work_id}::{from_id}::{to_id}::{rel}::{ch}");
                graph
                    .create_relationship_from_suggestion(
                        work_id,
                        &from_name,
                        &to_name,
                        Some(from_id),
                        Some(to_id),
                        category,
                        &rel,
                        &note,
                        Some(ch.as_str()),
                        &sid,
                    )
                    .map_err(|err| {
                        format!(
                            "create_relationship_from_suggestion({from_name}→{to_name} {rel} @ {ch}): {err}"
                        )
                    })?;
            }
        }
    }
    Ok(())
}

/// POST /api/v1/crawler/novels/{slug}/distil-world — 世界树/节拍/多出口/世界线蒸馏。
///
/// 复用 `novel_distil` 的 shelf 解析逻辑，依次执行：
/// 1) 世界树 → pack.lore_entries；
/// 2) 每章节拍 → nodes[].locked_beats；
/// 3) 多出口 → nodes[].exit（原单链 continue 时替换为多出口，已有出口保留并补齐）；
/// 4) 世界线 → 写 `data/story-packs/<pack>/worldline.json`（旁挂，不改 pack.json 结构）。
pub async fn novel_distil_world(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(slug): Path<String>,
    body: axum::body::Bytes,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    // 可选指定角色蒸馏：body {"characters": ["外婆","春儿"]} → 跳过角色谱，只蒸馏名单角色并合并进 pack
    let named_chars: Option<Vec<String>> = if body.is_empty() {
        None
    } else {
        serde_json::from_slice::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.get("characters").cloned())
            .and_then(|v| match v {
                serde_json::Value::Array(a) => Some(a),
                _ => None,
            })
            .map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect::<Vec<_>>()
            })
    };
    // [L0] forceRelations: true → 已有 relations.json 也强制重跑关系图谱（重蒸馏+重新落 graph 桥）
    let force_relations: bool = if body.is_empty() {
        false
    } else {
        serde_json::from_slice::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.get("forceRelations").cloned())
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    };
    // 读 shelf md（复用 novel_distil 的解析逻辑）
    let shelf = scan_shelf();
    let entry = match shelf.into_iter().find(|e| e.slug == slug) {
        Some(e) => e,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"ok": false, "error": "novel not found"})),
            )
                .into_response();
        }
    };
    let dir = shelf_dir();
    let slug_tag = shelf_slug(&entry.title);
    let mut file_path = None;
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("md") {
                continue;
            }
            let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            if shelf_slug(stem).contains(&slug_tag) {
                file_path = Some(p);
                break;
            }
        }
    }
    if !file_path
        .and_then(|p| std::fs::read_to_string(p).ok())
        .is_some()
    {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"ok": false, "error": "file not found"})),
        )
            .into_response();
    }

    // 防重：同一 slug 已有 running/queued 的 shelf_distil_world 任务 → 409
    if let Ok(items) = state.jobs.list(JobListFilter {
        kind: Some("shelf_distil_world".into()),
        status: None,
        user_id: None,
        workspace_id: Some(session.workspace_id.clone()),
        limit: 50,
    }) {
        if let Some(j) = items.iter().find(|j| {
            !is_terminal_job_status(&normalize_job_status(&j.status))
                && j.payload
                    .as_ref()
                    .and_then(|p| p.get("slug"))
                    .and_then(|v| v.as_str())
                    == Some(slug.as_str())
        }) {
            return (
                StatusCode::CONFLICT,
                Json(json!({
                    "ok": false,
                    "error": "该作品已有转换任务在运行，请稍候",
                    "jobId": j.run_id,
                    "jobsStream": format!("/api/v1/jobs/{}/stream", j.run_id),
                    "status": normalize_job_status(&j.status),
                })),
            )
                .into_response();
        }
    }

    let title = entry.title.clone();

    // 创建后台 job：立即返回 202，蒸馏在后台执行
    let job = match state.jobs.create(
        "shelf_distil_world",
        &session.user_id,
        &session.workspace_id,
        json!({ "slug": slug, "title": title, "characters": named_chars, "force_relations": force_relations }),
        None,
        None,
    ) {
        Ok(j) => j,
        Err(e) => return map_core_err(e),
    };
    let run_id = job.run_id.clone();

    // 后台执行体已抽出为 exec_shelf_distil_world（创建后调度与重启恢复续跑共用）
    let run_id_work = run_id.clone();
    let state_spawn = state.clone();
    let title_spawn = title.clone();
    let resume_meta = job.payload.clone();
    tokio::spawn(async move {
        exec_shelf_distil_world(state_spawn, run_id_work, slug, title_spawn, resume_meta).await;
    });

    (StatusCode::ACCEPTED, Json(json!({
        "ok": true,
        "jobId": run_id,
        "jobsStream": format!("/api/v1/jobs/{}/stream", run_id),
        "status": "queued",
    })))
        .into_response()
}

/// GET /api/v1/crawler/novels/{slug}/export — download shelf markdown as attachment.
pub async fn novel_export(
    _state: State<AppState>,
    _headers: HeaderMap,
    Path(slug): Path<String>,
) -> Response {
    let shelf = scan_shelf();
    let entry = match shelf.into_iter().find(|e| e.slug == slug) {
        Some(e) => e,
        None => {
            return (StatusCode::NOT_FOUND, "not found").into_response();
        }
    };
    let dir = shelf_dir();
    let slug_tag = shelf_slug(&entry.title);
    let mut file_path = None;
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("md") {
                continue;
            }
            let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            if shelf_slug(stem).contains(&slug_tag) {
                file_path = Some(p);
                break;
            }
        }
    }
    match file_path.and_then(|p| std::fs::read(p).ok()) {
        Some(bytes) => {
            let fname = format!("{}.md", entry.title.replace(['/', '\\', '"'], "_"));
            let mut res = (StatusCode::OK, bytes).into_response();
            res.headers_mut().insert(
                axum::http::header::CONTENT_TYPE,
                axum::http::HeaderValue::from_static("text/markdown; charset=utf-8"),
            );
            if let Ok(v) = axum::http::HeaderValue::from_str(&format!(
                "attachment; filename=\"{}\"",
                fname.replace('"', "")
            )) {
                res.headers_mut()
                    .insert(axum::http::header::CONTENT_DISPOSITION, v);
            }
            res
        }
        None => (StatusCode::NOT_FOUND, "file not found").into_response(),
    }
}

/// `GET /api/v1/crawler/fanqie/meta?url=...` or `?bookId=...`
/// Returns novel metadata only (no chapter content fetch).
pub async fn fanqie_meta(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let _session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };

    let enabled = state
        .app_state
        .load_settings_public()
        .map(|s| s.crawler_enabled)
        .unwrap_or(false);
    if !enabled {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "ok": false, "error": "crawler_disabled", "code": "CRAWLER_DISABLED" })),
        )
            .into_response();
    }

    let url = params.get("url").cloned().or_else(|| {
        params.get("bookId").map(|id| format!("https://fanqienovel.com/page/{id}"))
    });

    let url = match url {
        Some(u) => u,
        None => return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "ok": false, "error": "url or bookId required" })),
        ).into_response(),
    };

    if !is_fanqie_host(&url) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "ok": false, "error": "unsupported host" })),
        ).into_response();
    }

    let book_id = match capture_after(&url, "/page/") {
        Some(id) => id,
        None => return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "ok": false, "error": "need /page/{id} URL or bookId param" })),
        ).into_response(),
    };

    let proxy_config = ProxyConfig::default();
    let client = match make_crawler_client(Some(&proxy_config.proxy_url)) {
        Ok(c) => c,
        Err(e) => return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "ok": false, "error": format!("client: {e}") })),
        ).into_response(),
    };

    let resp = match client
        .get(&url)
        .headers(browser_headers(random_ua()))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "ok": false, "error": format!("fetch: {e}") })),
        ).into_response(),
    };

    if !resp.status().is_success() {
        return (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "ok": false, "error": format!("HTTP {}", resp.status()) })),
        ).into_response();
    }

    let html = match resp.text().await {
        Ok(t) => t,
        Err(e) => return (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "ok": false, "error": format!("read: {e}") })),
        ).into_response(),
    };

    if let Err(e) = check_anti_bot(&html) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "ok": false, "error": e, "code": "CRAWLER_ANTIBOT" })),
        ).into_response();
    }

    let meta = extract_novel_meta(&html);
    let title = extract_h1_title(&html).unwrap_or_else(|| format!("未命名_{book_id}"));

    Json(json!({
        "ok": true,
        "bookId": book_id,
        "title": title,
        "meta": meta,
        "url": url,
    }))
    .into_response()
}

/// `GET /api/v1/crawler/fanqie/progress` - list all background crawl progress.
/// `GET /api/v1/crawler/fanqie/progress?crawlId=xxx` - single crawl progress.
pub async fn fanqie_progress(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let _session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };

    let store = progress_store();
    let guard = store.read().await;

    if let Some(crawl_id) = params.get("crawlId") {
        match guard.get(crawl_id) {
            Some(prog) => Json(json!({ "ok": true, "progress": prog })).into_response(),
            None => (
                StatusCode::NOT_FOUND,
                Json(json!({ "ok": false, "error": "crawl_id not found" })),
            )
                .into_response(),
        }
    } else {
        let all: Vec<&CrawlProgress> = guard.values().collect();
        Json(json!({ "ok": true, "progress": all }))
            .into_response()
    }
}

/// `GET /api/v1/crawler/fanqie/search?q=书名`
/// Searches fanqie novels via Sogou site search, returns book_id list with meta.
pub async fn fanqie_search(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let _session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };

    let q = match params.get("q").map(|s| s.trim()).filter(|s| !s.is_empty()) {
        Some(q) => q.to_string(),
        None => return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "ok": false, "error": "query param 'q' required" })),
        ).into_response(),
    };

    // Use local FQ unidbg signing service for search
    let fq_base = "http://127.0.0.1:9999";
    let search_url = format!("{}/search?key={}&page=1&size=10&tabType=3", fq_base, urlencode(&q));
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .no_proxy()
        .build()
    {
        Ok(c) => c,
        Err(e) => return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "ok": false, "error": format!("client: {e}") })),
        ).into_response(),
    };

    let resp = match client.get(&search_url).send().await {
        Ok(r) => r,
        Err(e) => return (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "ok": false, "error": format!("fq search: {e}") })),
        ).into_response(),
    };

    let text = match resp.text().await {
        Ok(t) => t,
        Err(e) => return (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "ok": false, "error": format!("fq read: {e}") })),
        ).into_response(),
    };

    let body: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => return (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "ok": false, "error": format!("fq parse: {e}") })),
        ).into_response(),
    };

    let books = body["data"]["books"].as_array().cloned().unwrap_or_default();
    let results: Vec<Value> = books.iter().filter_map(|b| {
        let book_id = b["bookId"].as_str()?;
        let title = b["bookName"].as_str().unwrap_or("");
        let author = b["author"].as_str().unwrap_or("");
        Some(json!({
            "book_id": book_id,
            "title": title,
            "author": author,
            "category": b["category"].as_str().unwrap_or(""),
            "abstract": b["description"].as_str().unwrap_or(""),
            "word_number": b["wordCount"].as_i64().unwrap_or(0),
            "cover_url": b["coverUrl"].as_str().unwrap_or(""),
            "url": format!("https://fanqienovel.com/page/{}", book_id),
        }))
    }).collect();

    Json(json!({
        "ok": true,
        "query": q,
        "results": results,
    })).into_response()
}

/// Simple URL-encode for query parameters.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.as_bytes() {
        match *b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b':' | b' ' => {
                if *b == b' ' { out.push('+'); } else { out.push(*b as char); }
            }
            _ => {
                out.push_str(&format!("%{:02X}", b));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_reader_id() {
        assert_eq!(
            capture_after("https://fanqienovel.com/reader/12345", "/reader/").as_deref(),
            Some("12345")
        );
    }

    #[test]
    fn strip_basic_tags() {
        assert_eq!(strip_tags("<p>你好</p>"), "你好");
    }

    // A0: 章号连续性检测——正常序列零告警；缺章（1,2,3,6）报 gap；乱序报不递增。
    #[test]
    fn chapter_gaps_detects_missing_and_out_of_order() {
        let ok = vec![
            (0usize, "第一章 相遇".to_string()),
            (10, "第二章 误会".to_string()),
            (20, "第三章 接触".to_string()),
        ];
        assert!(chapter_gaps(&ok).is_empty(), "连续序列不应报 gap: {:?}", chapter_gaps(&ok));

        // 源缺第五章：1,2,3,6 → 缺 4,5
        let missing = vec![
            (0usize, "第一章 相遇".to_string()),
            (10, "第二章 误会".to_string()),
            (20, "第三章 接触".to_string()),
            (30, "第六章 初恋".to_string()),
        ];
        let gaps = chapter_gaps(&missing);
        assert_eq!(gaps.len(), 1, "应只报一个 gap: {gaps:?}");
        assert!(gaps[0].contains("缺"), "gap 应含缺章信息: {}", gaps[0]);

        // 中文数字章号解析：第六 ←> 3？ 反例验证能识别数值（「第六章」=6 > 3 +1）
        let cn = vec![
            (0usize, "第三章 接触".to_string()),
            (10, "第六章 初恋".to_string()),
        ];
        let g2 = chapter_gaps(&cn);
        assert_eq!(g2.len(), 1, "中文数字 gap 应检出: {g2:?}");

        // 非「第N章」标题（序章/番外）跳过不误报
        let mixed = vec![
            (0usize, "序章".to_string()),
            (10, "第一章 相遇".to_string()),
            (20, "第二章 误会".to_string()),
        ];
        assert!(chapter_gaps(&mixed).is_empty(), "序章应被跳过: {:?}", chapter_gaps(&mixed));
    }

    // A0 完整版 (2026-08-19): chXX = max(原著章号, 递增游标)。
    // 覆盖三场景：①连续序列恒不变 ②源缺章→自动跳号对齐原著标题（度蜜月 ch05→ch06）③楔子/续章回退→游标顺延零回归（宿醉）。
    #[test]
    fn chapter_id_seq_aligns_to_canonical_numbers() {
        let t = |s: &str| (s.to_string(), "正文".to_string());

        // ① 连续序列：章号=序号，编号不变
        let seq = vec![t("第一章 相遇"), t("第二章 误会"), t("第三章 接触"), t("第四章 亲近")];
        let ids = chapter_id_seq(&seq);
        assert_eq!(ids, vec!["ch01", "ch02", "ch03", "ch04"]);
        assert!(!ids.iter().any(|x| x.is_empty()));

        // ② 度蜜月实证：源缺第五章，「第六章 初恋」排在第 5 位 → 自动跳号 ch06（ch05 空缺），
        // 后续全部对齐原著标题（ch07/ch08…ch17），下游 worldline/relations/事件卡章节引用随之对齐。
        let honeymoon = vec![
            t("第一章 蜜月的起始"), t("第二章 误会"), t("第三章 接触"), t("第四章 亲近"),
            t("第六章 初恋"), t("第七章 过火"), t("第八章 沉沦"), t("第九章 播种"),
            t("第十章 孕"), t("第十一章 相亲"), t("第十二章 约会"), t("第十三章 恋"),
            t("第十四章 生女"), t("第十五章 狂乱"), t("第十六章 日常"), t("第十七章 又一次蜜月"),
        ];
        let ids = chapter_id_seq(&honeymoon);
        let expect: Vec<String> = (1..=17)
            .map(|n| if n == 5 { String::new() } else { format!("ch{n:02}") })
            .filter(|s| !s.is_empty())
            .collect();
        assert_eq!(ids, expect, "度蜜月应 ch01..ch04, ch06..ch17（缺 ch05）: {ids:?}");
        // 长度不变（16 切片 → 16 个 id），只是跳过 5
        assert_eq!(ids.len(), 16);

        // ③ 宿醉实证：楔子（解析失败）在第 1 位 → ch01；「第一章 续一」章号=1 但游标已到 2 → 顺延 ch02。
        // 结果与旧切分序号完全一致（零回归），且绝不产出重复 chXX。
        let hangover = vec![
            t("楔子"), t("第一章 续一"), t("第二章 续二"), t("第三章 续三"),
            t("第四章 续四"), t("第5章 续5"),
        ];
        let ids = chapter_id_seq(&hangover);
        assert_eq!(ids, vec!["ch01", "ch02", "ch03", "ch04", "ch05", "ch06"], "宿醉应保持原编号: {ids:?}");

        // ④ 章号重复去重：连续两个「第三章」→ ch03, ch04（游标顺延）
        let dup = vec![t("第三章 接触"), t("第三章 再见")];
        let ids = chapter_id_seq(&dup);
        assert_eq!(ids, vec!["ch03", "ch04"], "重复章号应顺延去重: {ids:?}");

        // ⑤ 大跳号：1 → 10 → 编号 ch01, ch10（不补中间空号，ch02..ch09 空缺可见）
        let jump = vec![t("第一章 相遇"), t("第十章 重逢")];
        let ids = chapter_id_seq(&jump);
        assert_eq!(ids, vec!["ch01", "ch10"], "大跳号应直接落到原著章号: {ids:?}");
    }

    // 2026-08-19: chapter_value 支持括号前导序号（度蜜月 `（5）释放`）
    #[test]
    fn chapter_value_parses_bracket_leading() {
        assert_eq!(chapter_value("　　（5）释放（手交口交）"), Some(5), "full: {:?}", chapter_value("　　（5）释放（手交口交）"));
        assert_eq!(chapter_value("（5）释放"), Some(5), "short: {:?}", chapter_value("（5）释放"));
        assert_eq!(chapter_value("　　（5）"), Some(5), "bare: {:?}", chapter_value("　　（5）"));
        assert_eq!(chapter_value("（5）章 内容"), Some(5), "zh: {:?}", chapter_value("（5）章 内容"));
        assert_eq!(chapter_value("第一章 相遇"), Some(1));
    }

    // 2026-08-19: TOC 规则补缺——Markdown 列表破折号前缀 `- 第X章`（安全屋/老板你也没说等 txt 导出格式）
    #[test]
    fn split_novel_chapters_handles_dash_prefix() {
        // 正文填充 >1000 字符（触发 pick_toc_rule 的间隔过滤，防止宽松规则把正文行当标题）
        let long_body = "正文内容……".repeat(120);
        let text = format!(
            "- 第1章 猎人与猎物\n{}\n- 第2章 与世界再见\n{}\n- 第3章 新世界\n{}\n",
            long_body, long_body, long_body
        );
        let chs = split_novel_chapters(&text);
        assert_eq!(chs.len(), 3, "破折号前缀应切出 3 章，实际: {chs:?}");
        assert!(chs[0].0.contains("第1章"), "第1章标题应保留: {}", chs[0].0);
        assert!(chs[2].0.contains("第3章"), "第3章标题应保留: {}", chs[2].0);
    }

    // 2026-08-19: 混合格式——主流「第X章」+ 内嵌「（N）标题」（度蜜月 第五章 `　　（5）释放`）
    // 期望：6 个标题全被识别（4 个第X章 + （5） + 第六章），不留并章。
    #[test]
    fn split_novel_chapters_mixed_bracket_keeps_all_titles() {
        let long_body = "正文内容……".repeat(120);
        let text = format!(
            "第一章 蜜月的起始\n{}\n第二章 误会\n{}\n第三章 接触\n{}\n第四章 亲近\n{}\n　　（5）释放（手交口交）\n{}\n第六章 初恋\n{}\n",
            long_body, long_body, long_body, long_body, long_body, long_body
        );
        let chs = split_novel_chapters(&text);
        assert_eq!(chs.len(), 6, "混合格式应切出 6 章（4 第X章 + （5）+ 第六章），实际: {chs:?}");
        let titles: Vec<String> = chs.iter().map(|(t, _)| t.clone()).collect();
        assert!(titles.iter().any(|t| t.contains("5）释放") || t.contains("（5）释放")),
            "（5）释放 应作为独立标题保留: {titles:?}");
    }

    #[test]
    fn remap_node_present_to_distil_maps_by_name() {
        use kaleido_core::{NodeExit, PackCharacterRef, StoryNode};
        let characters = vec![
            PackCharacterRef {
                id: "c-distil-0".into(),
                name: "林逸".into(),
                role: "protagonist".into(),
                gender: String::new(),
                appearance: String::new(),
                opening_scene: String::new(),
                opening_lines: "我叫林逸，是个天才。".into(),
                nsfw_profile: String::new(),
                importance: "high".into(),
                content_tier: None,
                example_dialogs: vec![],
                boundaries: vec![],
                personality: String::new(),
                speech_style: String::new(),
                voice_profile: String::new(),
                motivation: String::new(),
                relationships: vec![],
                evidence_refs: vec![],
                mental_models: vec![],
                decision_heuristics: vec![],
                beliefs: vec![],
                expressions: std::collections::HashMap::new(),
                avatar: None,
                starting_wardrobe: Default::default(),
                voice: None,
                archive: None,
            },
            PackCharacterRef {
                id: "c-distil-1".into(),
                name: "陆清韵".into(),
                role: "supporting".into(),
                gender: String::new(),
                appearance: String::new(),
                opening_scene: String::new(),
                opening_lines: String::new(),
                nsfw_profile: String::new(),
                importance: "high".into(),
                content_tier: None,
                example_dialogs: vec![],
                boundaries: vec![],
                personality: String::new(),
                speech_style: String::new(),
                voice_profile: String::new(),
                motivation: String::new(),
                relationships: vec![],
                evidence_refs: vec![],
                mental_models: vec![],
                decision_heuristics: vec![],
                beliefs: vec![],
                expressions: std::collections::HashMap::new(),
                avatar: None,
                starting_wardrobe: Default::default(),
                voice: None,
                archive: None,
            },
        ];
        let mut nodes = vec![StoryNode {
            id: "n1".into(),
            chapter_id: "ch01".into(),
            title: "第一章".into(),
            entry: "本章开始".into(),
            exit: vec![NodeExit {
                id: String::new(),
                when: String::new(),
                next: String::new(),
            }],
            locked_beats: vec![],
            allowed_divergence: "branch".into(),
            present_characters: vec!["c-cast-0".into(), "c-cast-5".into()],
            location_id: None,
            summary: String::new(),
        }];
        // bodies 与 nodes 顺序对应:i0 章正文含林逸与陆清韵 → present 应重建 c-distil-0/1
        let bodies = vec![(
            "chapters/ch01.md".to_string(),
            "林逸推开门，说：今天天气不错。陆清韵在屋里淡淡回应。".to_string(),
        )];
        remap_node_present_to_distil(&mut nodes, &characters, &bodies);
        assert_eq!(
            nodes[0].present_characters,
            vec!["c-distil-0", "c-distil-1"]
        );
    }

    #[test]
    fn remap_node_present_clears_when_no_distil_name_in_body() {
        use kaleido_core::{NodeExit, PackCharacterRef, StoryNode};
        let characters = vec![
            PackCharacterRef {
                id: "c-distil-0".into(),
                name: "林逸".into(),
                role: "protagonist".into(),
                gender: String::new(),
                appearance: String::new(),
                opening_scene: String::new(),
                opening_lines: String::new(),
                nsfw_profile: String::new(),
                importance: "high".into(),
                content_tier: None,
                example_dialogs: vec![],
                boundaries: vec![],
                personality: String::new(),
                speech_style: String::new(),
                voice_profile: String::new(),
                motivation: String::new(),
                relationships: vec![],
                evidence_refs: vec![],
                mental_models: vec![],
                decision_heuristics: vec![],
                beliefs: vec![],
                expressions: std::collections::HashMap::new(),
                avatar: None,
                starting_wardrobe: Default::default(),
                voice: None,
                archive: None,
            },
        ];
        // 旁白/抒情章：正文只用「她/妈妈」代词，无任何蒸馏角色直名
        let mut nodes = vec![StoryNode {
            id: "n2".into(),
            chapter_id: "ch02".into(),
            title: "第二章".into(),
            entry: "本章开始".into(),
            exit: vec![NodeExit {
                id: String::new(),
                when: String::new(),
                next: String::new(),
            }],
            locked_beats: vec![],
            allowed_divergence: "branch".into(),
            present_characters: vec!["c-cast-0".into(), "c-cast-3".into()],
            location_id: None,
            summary: String::new(),
        }];
        let bodies = vec![(
            "chapters/ch02.md".to_string(),
            "她望着窗外，妈妈没有说话。夜色渐渐深了，她轻轻叹了口气。".to_string(),
        )];
        remap_node_present_to_distil(&mut nodes, &characters, &bodies);
        // 无直名 → present 清空，不保留 c-cast 悬空引用
        assert!(nodes[0].present_characters.is_empty());
    }
}