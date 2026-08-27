//! U9 参考库与风格采纳（规则版，零 LLM 依赖）。
//!
//! 参照 Openwrite O5 的 style_extraction_pipeline / style_synthesizer 思路，
//! 用纯 Rust std 规则实现（句长分布 / 修辞密度 / 用词分布），不引第三方依赖。
//!
//! 数据落盘：`$DATA/reference-library/`
//!   - `samples.json`    作品/片段库（含 evidence 拆解）
//!   - `style-guide.json` 当前风格指南（enabled 开关 + 合成文本）
//!
//! 注入：build_tavern_system_prompt 调用处读取 style-guide.json，enabled 时追加「风格指南」段。

use axum::{
    extract::{Path, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Json, Router,
};
use chrono::Utc;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;

use crate::{map_core_err, session_from, AppState};

// ─── 常量 ────────────────────────────────────────────────────────────────────

const DIR_NAME: &str = "reference-library";
const SAMPLES_FILE: &str = "samples.json";
const GUIDE_FILE: &str = "style-guide.json";
const SCHEMA_VERSION: u32 = 1;
/// 单个样本正文长度上限（字符数）。
const SAMPLE_MAX_CHARS: usize = 200_000;
/// 高频词 top N。
const TOP_WORDS_N: usize = 12;
/// 停用词（简单规则版过滤）。
const STOP_WORDS: &[&str] = &[
    "的", "了", "是", "我", "你", "他", "她", "它", "我们", "你们", "他们", "她们", "在",
    "有", "和", "就", "都", "而", "及", "与", "着", "或", "一个", "没有", "这", "那", "之",
    "也", "很", "到", "说", "道", "不", "要", "会", "能", "把", "被", "让", "给", "对", "从",
    "自己", "什么", "怎么", "因为", "所以", "但是", "如果", "虽然", "还是", "已经", "可以",
    "现在", "时候", "觉得", "知道", "看见", "起来", "过来", "下去", "出来",
];

// ─── 数据模型 ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SentenceStats {
    pub count: usize,
    pub avg_len: f64,
    pub max_len: usize,
    pub min_len: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RhetoricDensity {
    /// 每千字感叹号数
    pub exclamation: f64,
    /// 每千字问号数
    pub question: f64,
    /// 每千字破折号数（——）
    pub dash: f64,
    /// 每千字省略号数（…… 或 …）
    pub ellipsis: f64,
    /// 每千字引号对数
    pub quote_pair: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SampleEvidence {
    pub sentences: SentenceStats,
    pub rhetoric: RhetoricDensity,
    pub top_words: Vec<(String, usize)>,
    pub char_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RefSample {
    pub id: String,
    pub title: String,
    pub content: String,
    pub evidence: SampleEvidence,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StyleGuide {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub source_sample_ids: Vec<String>,
    /// 合成的风格指南文本（中文规则描述）。
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub updated_at: String,
}

fn default_version() -> u32 {
    SCHEMA_VERSION
}

impl Default for StyleGuide {
    fn default() -> Self {
        Self {
            version: SCHEMA_VERSION,
            enabled: false,
            name: String::new(),
            source_sample_ids: Vec::new(),
            summary: String::new(),
            updated_at: String::new(),
        }
    }
}

// ─── 规则引擎：evidence 拆解 ────────────────────────────────────────────────

/// 按中文标点切句：句号/问号/感叹号/省略号/分号。保留切分符号在句尾。
fn split_sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        cur.push(c);
        match c {
            '。' | '！' | '？' | '；' | '…' => {
                // 省略号可能是 … 单个字符或 …… 两个；若下一个还是 … 则继续吞
                while matches!(chars.peek(), Some('…')) {
                    cur.push(chars.next().unwrap());
                }
                out.push(std::mem::take(&mut cur));
            }
            '\n' => {
                if !cur.trim().is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            _ => {}
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

/// 过滤纯标点/空白/过短句。
fn is_valid_sentence(s: &str) -> bool {
    let t = s.trim();
    t.chars().filter(|c| !c.is_ascii_punctuation() && !c.is_whitespace() && *c != '，' && *c != '、' && *c != '。').count() >= 2
}

fn count_occurrences(text: &str, pat: &str) -> usize {
    text.matches(pat).count()
}

fn extract_evidence(content: &str) -> SampleEvidence {
    let char_count = content.chars().count();
    let sentences_raw = split_sentences(content);
    let sentences: Vec<&str> = sentences_raw
        .iter()
        .map(|s| s.as_str())
        .filter(|s| is_valid_sentence(s))
        .collect();
    let lens: Vec<usize> = sentences.iter().map(|s| s.chars().count()).collect();
    let count = lens.len();
    let (avg_len, max_len, min_len) = if lens.is_empty() {
        (0.0, 0usize, 0usize)
    } else {
        let sum: usize = lens.iter().sum();
        (
            sum as f64 / count as f64,
            *lens.iter().max().unwrap(),
            *lens.iter().min().unwrap(),
        )
    };
    // 修辞密度（每千字）
    let per_k = |n: usize| -> f64 {
        if char_count == 0 {
            0.0
        } else {
            n as f64 / char_count as f64 * 1000.0
        }
    };
    let exclamation = per_k(count_occurrences(content, "！"));
    let question = per_k(count_occurrences(content, "？"));
    let dash = per_k(count_occurrences(content, "——"));
    let ellipsis = per_k(count_occurrences(content, "……") + count_occurrences(content, "…"));
    // 引号对数：统计中文引号出现次数 / 2（不成对按 floor）
    let quote_open = count_occurrences(content, "“") + count_occurrences(content, "\"");
    let quote_pair = (quote_open as f64 / 2.0) / char_count as f64 * 1000.0;

    // 用词分布：2 字词频（滑窗，过滤停用词 + 含标点/数字/字母的窗口）
    let mut freq: HashMap<String, usize> = HashMap::new();
    let chars: Vec<char> = content.chars().filter(|c| !c.is_whitespace()).collect();
    for w in chars.windows(2) {
        let a = w[0];
        let b = w[1];
        if !is_cjk(a) || !is_cjk(b) {
            continue;
        }
        let word: String = w.iter().collect();
        if STOP_WORDS.contains(&word.as_str()) {
            continue;
        }
        *freq.entry(word).or_insert(0) += 1;
    }
    let mut top: Vec<(String, usize)> = freq.into_iter().collect();
    top.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    top.truncate(TOP_WORDS_N);

    SampleEvidence {
        sentences: SentenceStats {
            count,
            avg_len,
            max_len,
            min_len,
        },
        rhetoric: RhetoricDensity {
            exclamation,
            question,
            dash,
            ellipsis,
            quote_pair,
        },
        top_words: top,
        char_count,
    }
}

fn is_cjk(c: char) -> bool {
    ('\u{4e00}'..='\u{9fff}').contains(&c)
}

// ─── 风格指南合成（规则版）──────────────────────────────────────────────────

fn describe_sentence_style(avg: f64, max_len: usize) -> String {
    if avg < 12.0 {
        format!("以短句为主（平均句长约 {:.1} 字），节奏明快；最长句 {max_len} 字。", avg)
    } else if avg < 24.0 {
        format!("句长适中（平均句长约 {:.1} 字），张弛有度；最长句 {max_len} 字。", avg)
    } else {
        format!("以长句为主（平均句长约 {:.1} 字），行文绵密；最长句 {max_len} 字。", avg)
    }
}

fn describe_rhetoric(r: &RhetoricDensity) -> String {
    let mut parts = Vec::new();
    if r.exclamation >= 15.0 {
        parts.push("感叹号使用频繁（情绪外放）".to_string());
    } else if r.exclamation >= 5.0 {
        parts.push("感叹号适度".to_string());
    }
    if r.question >= 15.0 {
        parts.push("问句密度高（多设问/心理活动）".to_string());
    } else if r.question >= 5.0 {
        parts.push("问句适度".to_string());
    }
    if r.dash >= 3.0 {
        parts.push("善用破折号（补充/转折）".to_string());
    }
    if r.ellipsis >= 3.0 {
        parts.push("省略号较多（留白/欲言又止）".to_string());
    }
    if r.quote_pair >= 5.0 {
        parts.push("对话占比高（引号密集）".to_string());
    }
    if parts.is_empty() {
        "修辞克制、叙述平实".to_string()
    } else {
        parts.join("；")
    }
}

fn merge_evidence(samples: &[&RefSample]) -> (f64, usize, usize, RhetoricDensity, Vec<(String, usize)>) {
    let mut total_sent = 0usize;
    let mut total_len = 0usize;
    let mut max_len = 0usize;
    let mut min_len = usize::MAX;
    let mut ex = 0.0_f64;
    let mut qu = 0.0_f64;
    let mut da = 0.0_f64;
    let mut el = 0.0_f64;
    let mut qp = 0.0_f64;
    let mut freq: HashMap<String, usize> = HashMap::new();
    for s in samples {
        let ev = &s.evidence;
        total_sent += ev.sentences.count;
        total_len += (ev.sentences.avg_len * ev.sentences.count as f64) as usize;
        max_len = max_len.max(ev.sentences.max_len);
        min_len = min_len.min(ev.sentences.min_len);
        ex += ev.rhetoric.exclamation;
        qu += ev.rhetoric.question;
        da += ev.rhetoric.dash;
        el += ev.rhetoric.ellipsis;
        qp += ev.rhetoric.quote_pair;
        for (w, n) in &ev.top_words {
            *freq.entry(w.clone()).or_insert(0) += n;
        }
    }
    let avg = if total_sent > 0 {
        total_len as f64 / total_sent as f64
    } else {
        0.0
    };
    let min_len = if min_len == usize::MAX { 0 } else { min_len };
    let mut top: Vec<(String, usize)> = freq.into_iter().collect();
    top.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    top.truncate(TOP_WORDS_N);
    let n = samples.len().max(1) as f64;
    let rhetoric = RhetoricDensity {
        exclamation: ex / n,
        question: qu / n,
        dash: da / n,
        ellipsis: el / n,
        quote_pair: qp / n,
    };
    (avg, max_len, min_len, rhetoric, top)
}

fn synthesize_guide(name: &str, samples: &[&RefSample]) -> String {
    let (avg, max_len, min_len, rhetoric, top) = merge_evidence(samples);
    let mut lines = Vec::new();
    lines.push(format!(
        "## 风格指南：{}（基于 {} 段样本）",
        name,
        samples.len()
    ));
    lines.push(describe_sentence_style(avg, max_len));
    lines.push(format!("短句下限约 {min_len} 字。"));
    lines.push(describe_rhetoric(&rhetoric));
    if !top.is_empty() {
        let words: Vec<String> = top
            .iter()
            .take(8)
            .map(|(w, n)| format!("{w}({n})"))
            .collect();
        lines.push(format!("高频用词倾向：{}。", words.join("、")));
    }
    lines.push("请模仿上述语言风格进行叙事；若剧情需要可适度偏离，但整体行文质感保持一致。".to_string());
    lines.join("\n")
}

// ─── 存储 ────────────────────────────────────────────────────────────────────

fn write_atomic(path: &FsPath, body: &str) -> kaleido_core::CoreResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(body.as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

#[derive(Clone)]
pub struct ReferenceLibraryStore {
    dir: PathBuf,
    lock: Arc<Mutex<()>>,
}

impl ReferenceLibraryStore {
    pub fn new(data_root: &std::path::Path) -> Self {
        let dir = data_root.join(DIR_NAME);
        let _ = fs::create_dir_all(&dir);
        Self {
            dir,
            lock: Arc::new(Mutex::new(())),
        }
    }

    fn samples_path(&self) -> PathBuf {
        self.dir.join(SAMPLES_FILE)
    }

    fn guide_path(&self) -> PathBuf {
        self.dir.join(GUIDE_FILE)
    }

    fn load_samples_locked(&self) -> Vec<RefSample> {
        let p = self.samples_path();
        if !p.exists() {
            return Vec::new();
        }
        fs::read_to_string(&p)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }

    pub fn list_samples(&self) -> Vec<RefSample> {
        let _g = self.lock.lock();
        self.load_samples_locked()
    }

    pub fn add_sample(&self, title: &str, content: &str) -> kaleido_core::CoreResult<RefSample> {
        let _g = self.lock.lock();
        let title = title.trim();
        let content = content.trim();
        if title.is_empty() || content.is_empty() {
            return Err(kaleido_core::CoreError::BadRequest(
                "title and content are required".into(),
            ));
        }
        if content.chars().count() > SAMPLE_MAX_CHARS {
            return Err(kaleido_core::CoreError::BadRequest(format!(
                "content too large (max {SAMPLE_MAX_CHARS} chars)"
            )));
        }
        let mut samples = self.load_samples_locked();
        let id = format!("ref-{}", uuid::Uuid::new_v4().simple());
        let sample = RefSample {
            id,
            title: title.to_string(),
            content: content.to_string(),
            evidence: extract_evidence(content),
            created_at: Utc::now().to_rfc3339(),
        };
        samples.push(sample.clone());
        write_atomic(&self.samples_path(), &serde_json::to_string_pretty(&samples)?)?;
        Ok(sample)
    }

    pub fn delete_sample(&self, id: &str) -> kaleido_core::CoreResult<()> {
        let _g = self.lock.lock();
        let mut samples = self.load_samples_locked();
        let before = samples.len();
        samples.retain(|s| s.id != id);
        if samples.len() == before {
            return Err(kaleido_core::CoreError::NotFound(format!("sample {id}")));
        }
        write_atomic(&self.samples_path(), &serde_json::to_string_pretty(&samples)?)?;
        Ok(())
    }

    fn load_guide_locked(&self) -> StyleGuide {
        let p = self.guide_path();
        if !p.exists() {
            return StyleGuide::default();
        }
        fs::read_to_string(&p)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }

    pub fn get_guide(&self) -> StyleGuide {
        let _g = self.lock.lock();
        self.load_guide_locked()
    }

    /// 从选中样本合成并保存指南。
    pub fn generate_guide(&self, name: &str, sample_ids: &[String]) -> kaleido_core::CoreResult<StyleGuide> {
        let _g = self.lock.lock();
        let name = name.trim();
        if name.is_empty() {
            return Err(kaleido_core::CoreError::BadRequest(
                "name is required".into(),
            ));
        }
        let samples = self.load_samples_locked();
        let picked: Vec<&RefSample> = samples
            .iter()
            .filter(|s| sample_ids.contains(&s.id))
            .collect();
        if picked.is_empty() {
            return Err(kaleido_core::CoreError::BadRequest(
                "no matching samples for the given sampleIds".into(),
            ));
        }
        let summary = synthesize_guide(name, &picked);
        let guide = StyleGuide {
            version: SCHEMA_VERSION,
            enabled: true,
            name: name.to_string(),
            source_sample_ids: sample_ids.to_vec(),
            summary,
            updated_at: Utc::now().to_rfc3339(),
        };
        write_atomic(&self.guide_path(), &serde_json::to_string_pretty(&guide)?)?;
        Ok(guide)
    }

    /// 启停注入（不改指南内容）。
    pub fn set_enabled(&self, enabled: bool) -> kaleido_core::CoreResult<StyleGuide> {
        let _g = self.lock.lock();
        let mut guide = self.load_guide_locked();
        guide.enabled = enabled;
        guide.updated_at = Utc::now().to_rfc3339();
        write_atomic(&self.guide_path(), &serde_json::to_string_pretty(&guide)?)?;
        Ok(guide)
    }

    /// 供 build_tavern_system_prompt 读取：enabled 时返回「风格指南」段文本。
    pub fn injection_block(&self) -> String {
        let _g = self.lock.lock();
        let guide = self.load_guide_locked();
        if !guide.enabled || guide.summary.trim().is_empty() {
            return String::new();
        }
        guide.summary
    }
}

// ─── Handlers ────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddSampleBody {
    title: String,
    content: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GenerateGuideBody {
    name: String,
    sample_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetEnabledBody {
    enabled: bool,
}

fn store_from(state: &AppState) -> ReferenceLibraryStore {
    state.reference_library.clone()
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/reference-library/samples",
            get(list_samples).post(add_sample),
        )
        .route(
            "/api/v1/reference-library/samples/{id}",
            delete(delete_sample),
        )
        .route(
            "/api/v1/reference-library/style-guide",
            get(get_guide).put(set_guide_enabled),
        )
        .route(
            "/api/v1/reference-library/style-guide/generate",
            post(generate_guide),
        )
}

async fn list_samples(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let store = store_from(&state);
    let samples = store.list_samples();
    // 列表不返回全文（省流量），只回 evidence 与标题。
    let slim: Vec<Value> = samples
        .iter()
        .map(|s| {
            json!({
                "id": s.id,
                "title": s.title,
                "evidence": s.evidence,
                "createdAt": s.created_at,
                "charCount": s.evidence.char_count,
            })
        })
        .collect();
    Json(json!({ "samples": slim })).into_response()
}

async fn add_sample(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<AddSampleBody>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let store = store_from(&state);
    match store.add_sample(&body.title, &body.content) {
        Ok(s) => Json(json!({
            "id": s.id,
            "title": s.title,
            "evidence": s.evidence,
            "createdAt": s.created_at,
        }))
        .into_response(),
        Err(e) => map_core_err(e),
    }
}

async fn delete_sample(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let store = store_from(&state);
    match store.delete_sample(&id) {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e) => map_core_err(e),
    }
}

async fn get_guide(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let store = store_from(&state);
    Json(store.get_guide()).into_response()
}

async fn set_guide_enabled(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<SetEnabledBody>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let store = store_from(&state);
    match store.set_enabled(body.enabled) {
        Ok(g) => Json(g).into_response(),
        Err(e) => map_core_err(e),
    }
}

async fn generate_guide(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<GenerateGuideBody>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let store = store_from(&state);
    match store.generate_guide(&body.name, &body.sample_ids) {
        Ok(g) => Json(g).into_response(),
        Err(e) => map_core_err(e),
    }
}
