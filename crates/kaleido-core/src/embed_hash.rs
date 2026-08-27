//! 零依赖本地哈希 embedding（吸收自 Liyuan `embedTextLocal`）。
//!
//! 作为 embedding 链路的最后兜底：模型未下载 + HTTP 代理不可用时，
//! 仍能产出确定性向量，保证 RAG/向量检索功能不硬失败。
//! 维度与 BGE-small-zh (512) 对齐，退化模式下索引内部保持自洽。
//!
//! 算法：NFKC 归一化 → 小写 → 空白折叠；单字(权重1.0+0.5双桶)、
//! 字符 bigram(1.2)、词(1.5) 经 FNV-1a 哈希散入 dim 桶，最后 L2 归一化。

use unicode_normalization::UnicodeNormalization;

/// FNV-1a 32-bit 哈希（Liyuan `hash32` 同款）。
fn hash32_fnv1a(s: &str) -> u32 {
    let mut h: u32 = 2166136261;
    for b in s.as_bytes() {
        h ^= *b as u32;
        h = h.wrapping_mul(16777619);
    }
    h
}

/// 文本 → L2 归一化哈希向量（确定性；同文本恒同向量）。
pub fn embed_text_hash(text: &str, dim: usize) -> Vec<f32> {
    let dim = dim.max(2);
    let mut v = vec![0f32; dim];

    // NFKC + lowercase + 空白折叠（对应 Liyuan: normalize("NFKC").toLowerCase().replace(/\s+/g," ")）
    let t: String = text
        .nfkc()
        .collect::<String>()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if t.is_empty() {
        return v;
    }

    let chars: Vec<char> = t.chars().collect();

    // 单字：双桶（h 与 h>>>8 各落一桶）
    for &c in &chars {
        let h = hash32_fnv1a(&format!("u:{}", c));
        v[(h as usize) % dim] += 1.0;
        v[((h >> 8) as usize) % dim] += 0.5;
    }
    // 字符 bigram
    for w in chars.windows(2) {
        let tok: String = w.iter().collect();
        let h = hash32_fnv1a(&format!("b:{}", tok));
        v[(h as usize) % dim] += 1.2;
    }
    // 词（非字母数字切分，长度≥2）
    for w in t.split(|c: char| !c.is_alphanumeric()) {
        if w.chars().count() < 2 {
            continue;
        }
        let h = hash32_fnv1a(&format!("w:{}", w));
        v[(h as usize) % dim] += 1.5;
    }

    // L2 归一化
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 1e-8 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
    v
}

/// 余弦相似度（等长向量）。
pub fn hash_cosine(a: &[f32], b: &[f32]) -> f64 {
    let n = a.len().min(b.len());
    let mut s = 0f64;
    for i in 0..n {
        s += a[i] as f64 * b[i] as f64;
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_deterministic() {
        let a = embed_text_hash("我失忆后，妈妈变得有点奇怪", 512);
        let b = embed_text_hash("我失忆后，妈妈变得有点奇怪", 512);
        assert_eq!(a, b, "同文本必须产生相同向量");
    }

    #[test]
    fn test_hash_l2_norm_is_one() {
        let v = embed_text_hash("雨巷来客 · 白昼之下", 512);
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-4, "L2 归一化后模长应为 1，got {norm}");
    }

    #[test]
    fn test_hash_similarity_ordering() {
        let base = embed_text_hash("临近高考，妈妈决定和闺蜜互换给对方儿子", 512);
        let similar = embed_text_hash("临近高考，妈妈决定和闺蜜互换给对方儿子性处理！", 512);
        let unrelated = embed_text_hash("白昼之下：城市守夜人的契约与背叛", 512);
        let sim = hash_cosine(&base, &similar);
        let unrel = hash_cosine(&base, &unrelated);
        assert!(
            sim > unrel,
            "相似文本余弦 {sim} 应大于无关文本 {unrel}"
        );
    }

    #[test]
    fn test_hash_dim_aligns_bge() {
        let v = embed_text_hash("测试", 512);
        assert_eq!(v.len(), 512, "维度应对齐 BGE-small-zh (512)");
    }

    #[test]
    fn test_hash_empty_text_zero_vector() {
        let v = embed_text_hash("   ", 512);
        assert!(v.iter().all(|x| *x == 0.0));
    }

    #[test]
    fn test_hash_nfkc_fullwidth() {
        // 全角→半角归一：全角与半角应产生相同向量
        let a = embed_text_hash("ＡＢＣ", 256);
        let b = embed_text_hash("ABC", 256);
        assert_eq!(a, b, "NFKC 后全角/半角应等价");
    }
}
