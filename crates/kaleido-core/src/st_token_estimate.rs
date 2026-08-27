//! W4: token estimate — default CJK/Latin heuristic; optional cl100k-style approx.
//!
//! No hard dependency on tiktoken. Modes:
//! - `heuristic` (default): latin≈4 chars/tok, CJK≈1.5 chars/tok, word floor
//! - `cl100k_approx`: OpenAI cl100k-ish ratios (latin≈4, CJK≈1 char/tok, punctuation + specials)
//!
//! Both are **estimates** for WI budget / UI preview — not bit-identical to model BPE.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TokenEstimateMode {
    #[default]
    Heuristic,
    /// Approximate cl100k ratios without embedding the full BPE table.
    Cl100kApprox,
}

impl TokenEstimateMode {
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "cl100k" | "cl100k_approx" | "cl100k-approx" | "openai" | "tiktoken" => {
                Self::Cl100kApprox
            }
            _ => Self::Heuristic,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Heuristic => "heuristic",
            Self::Cl100kApprox => "cl100k_approx",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenEstimateBreakdown {
    pub chars: i32,
    pub cjk_chars: i32,
    pub latin_chars: i32,
    pub words: i32,
    pub by_char: i32,
    pub by_words: i32,
    pub special_bonus: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenEstimate {
    pub tokens: i32,
    pub mode: String,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub breakdown: Option<TokenEstimateBreakdown>,
}

fn is_cjk(c: char) -> bool {
    let u = c as u32;
    (0x4E00..=0x9FFF).contains(&u)
        || (0x3400..=0x4DBF).contains(&u)
        || (0x3040..=0x30FF).contains(&u)
        || (0xAC00..=0xD7AF).contains(&u)
        || (0xF900..=0xFAFF).contains(&u)
}

fn is_special_punct(c: char) -> bool {
    c.is_ascii_punctuation()
        || matches!(
            c,
            '【' | '】'
                | '「'
                | '」'
                | '『'
                | '』'
                | '（'
                | '）'
                | '《'
                | '》'
                | '…'
                | '—'
                | '·'
                | '、'
                | '，'
                | '。'
                | '！'
                | '？'
                | '；'
                | '：'
                | '\u{201c}'
                | '\u{201d}'
                | '\u{2018}'
                | '\u{2019}'
        )
}

/// Core estimator used by WI budget and `/api/v1/tokenize/estimate`.
pub fn estimate_tokens(text: &str, mode: TokenEstimateMode) -> i32 {
    estimate_tokens_detailed(text, mode, false).tokens
}

pub fn estimate_tokens_detailed(
    text: &str,
    mode: TokenEstimateMode,
    with_breakdown: bool,
) -> TokenEstimate {
    let chars = text.chars().count() as i32;
    if chars == 0 {
        return TokenEstimate {
            tokens: 0,
            mode: mode.as_str().into(),
            method: "empty".into(),
            breakdown: if with_breakdown {
                Some(TokenEstimateBreakdown {
                    chars: 0,
                    cjk_chars: 0,
                    latin_chars: 0,
                    words: 0,
                    by_char: 0,
                    by_words: 0,
                    special_bonus: 0,
                })
            } else {
                None
            },
        };
    }

    let cjk = text.chars().filter(|c| is_cjk(*c)).count() as i32;
    let special = text.chars().filter(|c| is_special_punct(*c)).count() as i32;
    let latin = (chars - cjk).max(0);
    let words = text.split_whitespace().count() as i32;

    let (by_char, special_bonus, method) = match mode {
        TokenEstimateMode::Heuristic => {
            // latin ~4 chars/tok, CJK ~1.5 chars/tok
            let v = ((latin as f64) / 4.0 + (cjk as f64) / 1.5).ceil() as i32;
            (v, 0, "cjk_latin_heuristic_v1")
        }
        TokenEstimateMode::Cl100kApprox => {
            // cl100k tends closer to ~1 token per CJK ideograph; latin still ~4;
            // punctuation often splits → small bonus.
            let v = ((latin as f64) / 4.0 + (cjk as f64) / 1.0).ceil() as i32;
            let bonus = (special as f64 * 0.15).ceil() as i32;
            (v + bonus, bonus, "cl100k_char_approx_v1")
        }
    };
    let by_words = words;
    let tokens = by_char.max(by_words).max(1);

    TokenEstimate {
        tokens,
        mode: mode.as_str().into(),
        method: method.into(),
        breakdown: if with_breakdown {
            Some(TokenEstimateBreakdown {
                chars,
                cjk_chars: cjk,
                latin_chars: latin,
                words,
                by_char,
                by_words,
                special_bonus,
            })
        } else {
            None
        },
    }
}

/// Estimate a list of texts; also returns sum.
pub fn estimate_many(texts: &[&str], mode: TokenEstimateMode) -> (Vec<TokenEstimate>, i32) {
    let items: Vec<_> = texts
        .iter()
        .map(|t| estimate_tokens_detailed(t, mode, true))
        .collect();
    let sum = items.iter().map(|e| e.tokens).sum();
    (items, sum)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_is_zero() {
        assert_eq!(estimate_tokens("", TokenEstimateMode::Heuristic), 0);
    }

    #[test]
    fn cjk_cl100k_ge_heuristic() {
        let s = "青衣门影刺在星落湖畔留下银色涟漪。";
        let h = estimate_tokens(s, TokenEstimateMode::Heuristic);
        let c = estimate_tokens(s, TokenEstimateMode::Cl100kApprox);
        assert!(h >= 1 && c >= 1);
        // denser CJK counting → cl100k_approx typically ≥ heuristic
        assert!(c >= h);
    }

    #[test]
    fn latin_word_floor() {
        let s = "one two three four five";
        let t = estimate_tokens(s, TokenEstimateMode::Heuristic);
        assert!(t >= 5);
    }
}
