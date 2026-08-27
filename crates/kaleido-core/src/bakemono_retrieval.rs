//! 混合检索纯函数（吸收自 SillyTavern-BakemonoMemory `src/vector/hybrid-retrieval.js`）。
//!
//! 语义(embedding) + 词法(lexical) + 显式关键词三路候选并集，IDF 加权 + RRF 融合。
//! 对症 Kaleido S7 会话召回纯向量缺陷：低相似度但词法命中的旧剧情（如「素描」）
//! 也能被召回。纯函数：无 IO、无状态、无 DOM。测例翻译自
//! `tests/hybrid-retrieval.test.mjs`（4 个）。

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::OnceLock;

/// CJK 停用字（源项目同款集合）。
fn cjk_stop_characters() -> &'static HashSet<char> {
    static STOP: OnceLock<HashSet<char>> = OnceLock::new();
    STOP.get_or_init(|| {
        "的了是在与和及或也都而被把对从为有还就又很这那中上下来去后前着过于将并但则所其之"
            .chars()
            .collect()
    })
}

/// NFKC + 小写 + 空白折叠。
fn normalize_lexical_text(value: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    value
        .nfkc()
        .collect::<String>()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn unique(values: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    values
        .into_iter()
        .filter(|v| !v.is_empty())
        .filter(|v| seen.insert(v.clone()))
        .collect()
}

fn is_useful_cjk_gram(value: &str) -> bool {
    let chars: Vec<char> = value.chars().collect();
    chars.len() >= 2
        && chars
            .iter()
            .filter(|c| !cjk_stop_characters().contains(c))
            .count()
            >= 2
}

/// 分词：拉丁词（≥2 字符）+ CJK 序列（2-12 字，整体 + 2/3/4-gram）。
pub fn tokenize_hybrid_text(value: &str, max_terms: usize) -> Vec<String> {
    let normalized = normalize_lexical_text(value);
    if normalized.is_empty() {
        return Vec::new();
    }
    let mut terms: Vec<String> = Vec::new();
    let bytes = normalized.as_bytes();
    let chars: Vec<(usize, char)> = normalized.char_indices().collect();

    // 拉丁词：/[a-z0-9][a-z0-9_.-]+/g
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_alphanumeric() {
            let start = i;
            let mut j = i;
            while j < bytes.len()
                && (bytes[j].is_ascii_alphanumeric() || matches!(bytes[j], b'_' | b'.' | b'-'))
            {
                j += 1;
            }
            let term = normalized[start..j].to_string();
            if term.len() >= 2 {
                terms.push(term);
            }
            i = j;
        } else {
            i += 1;
        }
    }

    // CJK 序列：/[\u3400-\u9fff]+/g
    let mut ci = 0;
    while ci < chars.len() {
        let (_, c) = chars[ci];
        if (0x3400..=0x9fff).contains(&(c as u32)) {
            let start = ci;
            let mut cj = ci;
            while cj < chars.len() && (0x3400..=0x9fff).contains(&(chars[cj].1 as u32)) {
                cj += 1;
            }
            // 整体序列
            let seq: String = chars[start..cj].iter().map(|(_, ch)| *ch).collect();
            if (2..=12).contains(&seq.chars().count()) && is_useful_cjk_gram(&seq) {
                terms.push(seq.clone());
            }
            // 2/3/4-gram
            for size in [2usize, 3, 4] {
                let seq_len = seq.chars().count();
                if seq_len < size {
                    continue;
                }
                let seq_chars: Vec<char> = seq.chars().collect();
                for idx in 0..=(seq_len - size) {
                    let gram: String = seq_chars[idx..idx + size].iter().collect();
                    if is_useful_cjk_gram(&gram) {
                        terms.push(gram);
                    }
                }
            }
            ci = cj;
        } else {
            ci += 1;
        }
    }

    unique(terms).into_iter().take(max_terms.max(1)).collect()
}

/// 查询词构造：显式关键词 + 生成词。
pub fn create_hybrid_query_terms(
    queries: &[String],
    keyword_terms: &[String],
    max_terms: usize,
) -> (Vec<String>, Vec<String>) {
    let explicit: Vec<String> = keyword_terms
        .iter()
        .map(|t| normalize_lexical_text(t))
        .filter(|t| t.chars().count() >= 2)
        .collect();
    let mut explicit = unique(explicit);
    let mut generated: Vec<String> = Vec::new();
    for q in queries {
        generated.extend(tokenize_hybrid_text(q, max_terms));
    }
    let mut all = unique(explicit.iter().cloned().chain(generated));
    all.truncate(max_terms.max(1));
    explicit.truncate(max_terms.max(1));
    (all, explicit)
}

/// 检索记录（title+summary+text 参与匹配）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HybridRecord {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding_score: Option<f64>,
}

impl HybridRecord {
    pub fn search_text(&self) -> String {
        normalize_lexical_text(&format!(
            "{}\n{}\n{}",
            self.title.as_deref().unwrap_or(""),
            self.summary.as_deref().unwrap_or(""),
            self.text
        ))
    }
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnrichedRecord {
    #[serde(flatten)]
    pub record: HybridRecord,
    pub lexical_score: f64,
    pub keyword_hits: usize,
    pub matched_terms: Vec<String>,
    pub matched_keywords: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hybrid_score: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reciprocal_rank_score: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vector_rank: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lexical_rank: Option<usize>,
}

fn build_document_frequency(records: &[HybridRecord], terms: &[String]) -> BTreeMap<String, usize> {
    let mut freq: BTreeMap<String, usize> = BTreeMap::new();
    for t in terms {
        let count = records
            .iter()
            .filter(|r| r.search_text().contains(t.as_str()))
            .count();
        freq.insert(t.clone(), count);
    }
    freq
}

fn inverse_document_frequency(document_count: usize, frequency: usize) -> f64 {
    let dc = document_count as f64;
    let f = frequency as f64;
    (1.0 + (dc - f + 0.5) / (f + 0.5)).ln()
}

#[derive(Debug, Clone)]
pub struct LexicalOptions {
    pub max_terms: usize,
    pub max_matched_terms: usize,
}

impl Default for LexicalOptions {
    fn default() -> Self {
        Self {
            max_terms: 180,
            max_matched_terms: 8,
        }
    }
}

/// 词法评分：IDF 加权（稀有词 > 常见词）。
pub fn enrich_hybrid_lexical_scores(
    records: &[HybridRecord],
    queries: &[String],
    keyword_terms: &[String],
    options: &LexicalOptions,
) -> Vec<EnrichedRecord> {
    if records.is_empty() {
        return Vec::new();
    }
    let (terms, explicit_keywords) = create_hybrid_query_terms(queries, keyword_terms, options.max_terms);
    if terms.is_empty() {
        return records
            .iter()
            .map(|r| EnrichedRecord {
                record: r.clone(),
                lexical_score: 0.0,
                keyword_hits: 0,
                matched_terms: Vec::new(),
                matched_keywords: Vec::new(),
                hybrid_score: None,
                reciprocal_rank_score: None,
                vector_rank: None,
                lexical_rank: None,
            })
            .collect();
    }
    let frequencies = build_document_frequency(records, &terms);
    let mut weights: BTreeMap<String, f64> = BTreeMap::new();
    for t in &terms {
        let f = frequencies.get(t).copied().unwrap_or(0);
        if f > 0 {
            weights.insert(t.clone(), inverse_document_frequency(records.len(), f));
        }
    }
    let total_weight: f64 = weights.values().sum();
    let total_weight = if total_weight <= 0.0 { 1.0 } else { total_weight };

    records
        .iter()
        .map(|r| {
            let haystack = r.search_text();
            let matched_terms: Vec<String> = terms
                .iter()
                .filter(|t| haystack.contains(t.as_str()))
                .cloned()
                .collect();
            let matched_keywords: Vec<String> = explicit_keywords
                .iter()
                .filter(|t| haystack.contains(t.as_str()))
                .cloned()
                .collect();
            let matched_weight: f64 = matched_terms
                .iter()
                .filter_map(|t| weights.get(t))
                .sum();
            let mut sorted_terms = matched_terms.clone();
            sorted_terms.sort_by(|a, b| {
                b.chars()
                    .count()
                    .cmp(&a.chars().count())
                    .then_with(|| a.cmp(b))
            });
            sorted_terms.truncate(options.max_matched_terms.max(1));
            EnrichedRecord {
                record: r.clone(),
                lexical_score: (matched_weight / total_weight).clamp(0.0, 1.0),
                keyword_hits: matched_keywords.len(),
                matched_terms: sorted_terms,
                matched_keywords,
                hybrid_score: None,
                reciprocal_rank_score: None,
                vector_rank: None,
                lexical_rank: None,
            }
        })
        .collect()
}

#[derive(Debug, Clone)]
pub struct RerankOptions {
    pub semantic_weight: f64,
    pub lexical_weight: f64,
    pub keyword_boost: f64,
    pub explicit_keyword_count: usize,
}

impl Default for RerankOptions {
    fn default() -> Self {
        Self {
            semantic_weight: 0.68,
            lexical_weight: 0.32,
            keyword_boost: 0.18,
            explicit_keyword_count: 0,
        }
    }
}

/// 混合重排分：embedding×semantic + lexical×lexical + keyword×boost。
pub fn compute_hybrid_rerank_score(record: &EnrichedRecord, options: &RerankOptions) -> f64 {
    let semantic_weight = options.semantic_weight.max(0.0);
    let lexical_weight = options.lexical_weight.max(0.0);
    let keyword_boost = options.keyword_boost.max(0.0);
    let embedding = record
        .record
        .embedding_score
        .unwrap_or(0.0)
        .clamp(0.0, 1.0);
    let lexical = record.lexical_score.clamp(0.0, 1.0);
    let keyword_count = record.matched_keywords.len().max(record.keyword_hits);
    let total_keywords = options.explicit_keyword_count.max(keyword_count).max(1);
    let keyword_score = (keyword_count as f64 / total_keywords as f64).min(1.0);
    (embedding * semantic_weight + lexical * lexical_weight + keyword_score * keyword_boost)
        .clamp(0.0, 1.0)
}

#[derive(Debug, Clone)]
pub struct CandidateOptions {
    pub candidate_count: usize,
    pub embedding_threshold: f64,
    pub keyword_boost: f64,
    pub max_terms: usize,
    pub max_matched_terms: usize,
}

impl Default for CandidateOptions {
    fn default() -> Self {
        Self {
            candidate_count: 20,
            embedding_threshold: 0.0,
            keyword_boost: 0.18,
            max_terms: 180,
            max_matched_terms: 8,
        }
    }
}

fn rank_of(records: &[&EnrichedRecord]) -> BTreeMap<String, usize> {
    records
        .iter()
        .enumerate()
        .map(|(idx, r)| (r.record.id.clone(), idx + 1))
        .collect()
}

/// 三路候选（向量 / 词法 / 关键词）并集 + RRF 融合排序。
pub fn select_hybrid_candidates(
    records: &[HybridRecord],
    queries: &[String],
    keyword_terms: &[String],
    options: &CandidateOptions,
) -> Vec<EnrichedRecord> {
    if records.is_empty() {
        return Vec::new();
    }
    let candidate_count = options.candidate_count.max(1);
    let lex_opts = LexicalOptions {
        max_terms: options.max_terms,
        max_matched_terms: options.max_matched_terms,
    };
    let enriched = enrich_hybrid_lexical_scores(records, queries, keyword_terms, &lex_opts);
    let (_, explicit_keywords) = create_hybrid_query_terms(queries, keyword_terms, options.max_terms);
    let explicit_keyword_count = explicit_keywords.len();

    // 向量通道
    let mut vector_ranked: Vec<&EnrichedRecord> = enriched
        .iter()
        .filter(|r| r.record.embedding_score.unwrap_or(0.0) >= options.embedding_threshold)
        .collect();
    vector_ranked.sort_by(|a, b| {
        b.record
            .embedding_score
            .unwrap_or(0.0)
            .partial_cmp(&a.record.embedding_score.unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    vector_ranked.truncate(candidate_count);

    // 词法通道
    let mut lexical_ranked: Vec<&EnrichedRecord> = enriched
        .iter()
        .filter(|r| r.lexical_score > 0.0)
        .collect();
    lexical_ranked.sort_by(|a, b| {
        b.lexical_score
            .partial_cmp(&a.lexical_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.keyword_hits.cmp(&a.keyword_hits))
            .then_with(|| {
                let aid = a.record.message_id.unwrap_or(0);
                let bid = b.record.message_id.unwrap_or(0);
                bid.cmp(&aid)
            })
    });
    lexical_ranked.truncate(candidate_count);

    // 关键词通道
    let mut keyword_ranked: Vec<&EnrichedRecord> = enriched
        .iter()
        .filter(|r| r.keyword_hits > 0)
        .collect();
    keyword_ranked.sort_by(|a, b| {
        b.keyword_hits
            .cmp(&a.keyword_hits)
            .then_with(|| {
                b.lexical_score
                    .partial_cmp(&a.lexical_score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });
    keyword_ranked.truncate(candidate_count);

    let vector_ranks = rank_of(&vector_ranked);
    let lexical_ranks = rank_of(&lexical_ranked);
    let keyword_ranks = rank_of(&keyword_ranked);

    let mut candidate_ids: BTreeSet<String> = BTreeSet::new();
    candidate_ids.extend(vector_ranked.iter().map(|r| r.record.id.clone()));
    candidate_ids.extend(lexical_ranked.iter().map(|r| r.record.id.clone()));
    candidate_ids.extend(keyword_ranked.iter().map(|r| r.record.id.clone()));

    let mut results: Vec<EnrichedRecord> = enriched
        .into_iter()
        .filter(|r| candidate_ids.contains(&r.record.id))
        .map(|mut r| {
            let rerank = RerankOptions {
                keyword_boost: options.keyword_boost,
                explicit_keyword_count,
                ..Default::default()
            };
            let hybrid_score = compute_hybrid_rerank_score(&r, &rerank);
            let rrf: f64 = [vector_ranks.get(&r.record.id), lexical_ranks.get(&r.record.id), keyword_ranks.get(&r.record.id)]
                .into_iter()
                .flatten()
                .map(|rank| 1.0 / (60.0 + *rank as f64))
                .sum();
            r.hybrid_score = Some(hybrid_score);
            r.reciprocal_rank_score = Some(rrf);
            r.vector_rank = vector_ranks.get(&r.record.id).copied();
            r.lexical_rank = lexical_ranks.get(&r.record.id).copied();
            r
        })
        .collect();

    results.sort_by(|a, b| {
        let hs = b
            .hybrid_score
            .unwrap_or(0.0)
            .partial_cmp(&a.hybrid_score.unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal);
        if hs != std::cmp::Ordering::Equal {
            return hs;
        }
        let rr = b
            .reciprocal_rank_score
            .unwrap_or(0.0)
            .partial_cmp(&a.reciprocal_rank_score.unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal);
        if rr != std::cmp::Ordering::Equal {
            return rr;
        }
        let es = b
            .record
            .embedding_score
            .unwrap_or(0.0)
            .partial_cmp(&a.record.embedding_score.unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal);
        if es != std::cmp::Ordering::Equal {
            return es;
        }
        let aid = a.record.message_id.unwrap_or(0);
        let bid = b.record.message_id.unwrap_or(0);
        bid.cmp(&aid)
    });
    results.truncate(candidate_count * 2);
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(id: &str, message_id: i64, title: &str, text: &str, embedding_score: f64) -> HybridRecord {
        HybridRecord {
            id: id.to_string(),
            message_id: Some(message_id),
            title: Some(title.to_string()),
            summary: None,
            text: text.to_string(),
            embedding_score: Some(embedding_score),
        }
    }

    #[test]
    fn hybrid_candidates_union_semantic_and_low_similarity_lexical_matches() {
        let records = vec![
            rec("semantic", 10, "雨夜", "两人在屋檐下交谈。", 0.84),
            rec("lexical", 20, "银钥匙", "Nana 把银钥匙交给 Kuroha，并要求她保守承诺。", 0.05),
            rec("noise", 30, "早餐", "今天吃了面包。", 0.03),
        ];
        let candidates = select_hybrid_candidates(
            &records,
            &["寻找银钥匙与旧承诺".to_string()],
            &[],
            &CandidateOptions {
                embedding_threshold: 0.22,
                candidate_count: 4,
                ..Default::default()
            },
        );
        let ids: BTreeSet<String> = candidates.iter().map(|c| c.record.id.clone()).collect();
        assert_eq!(
            ids,
            ["semantic".to_string(), "lexical".to_string()]
                .into_iter()
                .collect()
        );
        let lexical = candidates.iter().find(|c| c.record.id == "lexical").unwrap();
        assert!(lexical.lexical_score > 0.0);
    }

    #[test]
    fn rare_exact_terms_contribute_more_lexical_evidence_than_common_fragments() {
        let records = vec![
            HybridRecord {
                id: "rare".to_string(),
                message_id: None,
                title: None,
                summary: None,
                text: "Seraphina 将戒指藏进旧剧院。".to_string(),
                embedding_score: None,
            },
            HybridRecord {
                id: "common-a".to_string(),
                message_id: None,
                title: None,
                summary: None,
                text: "他们重新提到了过去的承诺。".to_string(),
                embedding_score: None,
            },
            HybridRecord {
                id: "common-b".to_string(),
                message_id: None,
                title: None,
                summary: None,
                text: "这份承诺至今仍然有效。".to_string(),
                embedding_score: None,
            },
        ];
        let scored = enrich_hybrid_lexical_scores(
            &records,
            &["Seraphina 的戒指与承诺".to_string()],
            &[],
            &LexicalOptions::default(),
        );
        let rare = scored.iter().find(|s| s.record.id == "rare").unwrap();
        let common = scored.iter().find(|s| s.record.id == "common-a").unwrap();
        assert!(rare.lexical_score > common.lexical_score);
        assert!(rare
            .matched_terms
            .iter()
            .any(|t| t.to_lowercase().contains("seraphina")));
    }

    #[test]
    fn configured_keyword_boost_participates_in_final_score() {
        let record = EnrichedRecord {
            record: HybridRecord {
                id: "x".to_string(),
                message_id: None,
                title: None,
                summary: None,
                text: String::new(),
                embedding_score: Some(0.0),
            },
            lexical_score: 0.0,
            keyword_hits: 1,
            matched_terms: Vec::new(),
            matched_keywords: vec!["k".to_string()],
            hybrid_score: None,
            reciprocal_rank_score: None,
            vector_rank: None,
            lexical_rank: None,
        };
        let score = compute_hybrid_rerank_score(
            &record,
            &RerankOptions {
                keyword_boost: 0.27,
                explicit_keyword_count: 1,
                ..Default::default()
            },
        );
        assert!((score - 0.27).abs() < 1e-9);
    }

    #[test]
    fn query_term_extraction_creates_bounded_chinese_ngrams_and_preserves_explicit_keywords() {
        let (terms, explicit) = create_hybrid_query_terms(
            &["她是否兑现了旧日承诺？".to_string()],
            &["银钥匙".to_string()],
            40,
        );
        assert!(terms.len() <= 40);
        assert!(terms.iter().any(|t| t == "银钥匙"));
        assert!(terms.iter().any(|t| t == "承诺"));
        assert_eq!(explicit, vec!["银钥匙".to_string()]);
    }
}
