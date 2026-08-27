//! 文本编码嗅探：集成自开源阅读器（Legado/ReadAware）。
//!
//! 两层策略：
//!   1. **ReadAware `decodeTextBook()`**：BOM 直接裁决 + 候选编码逐个“严格”解码（UTF-8→GB18030→Big5→Shift_JIS→EUC-KR）+ mojibake 检测。对整本单一编码 txt 有效。
//!   2. **ICU4J CharsetDetector 统计识别**（移植自 `legado-with-MD3`
//!      `app/src/main/java/io/legado/app/lib/icu4j/CharsetRecog_mbcs.java`）：
//!      逐字节状态机验证编码合法性 + 高频字表（commonChars）频率加权打分，
//!      用于 Big5 vs GB18030 这类“多编码都能解但只有一个对”的区分，以及
//!      对含少量噪声字节的原始非 UTF-8 字节流更鲁棒。
//!
//! TXT 文件不带编码声明，中文 TXT 常是 GB18030/GBK 或 Big5，纯按 UTF-8 硬解会产生整本乱码。

use encoding_rs::Encoding;

/// 逐字节序 BOM。
const BOMS: &[(&[u8], &'static Encoding)] = &[
    (&[0xef, 0xbb, 0xbf], encoding_rs::UTF_8),
    (&[0xff, 0xfe], encoding_rs::UTF_16LE),
    (&[0xfe, 0xff], encoding_rs::UTF_16BE),
];

/// 候选编码，顺序与 ReadAware 一致（GB18030 是 GBK/GB2312 严格超集）。
const CANDIDATE_ENCODINGS: &[&'static Encoding] = &[
    encoding_rs::UTF_8,
    encoding_rs::GB18030,
    encoding_rs::BIG5,
    encoding_rs::SHIFT_JIS,
    encoding_rs::EUC_KR,
];

/// 解码 TXT 原始字节为字符串，自动嗅探编码。
///
/// 流程：BOM → ReadAware 候选严格解码（能解且非 mojibake 即返回）；
/// 若没有任何候选通过（通常是多编码可解或含噪声），退回 ICU 统计识别裁决。
/// 返回 (text, encoding_label)。解码失败永不返回 Err——lossy 兜底。
pub fn decode_text(bytes: &[u8]) -> (String, &'static str) {
    // 1) BOM 裁决
    for (bom, enc) in BOMS {
        if bytes.len() >= bom.len() && &bytes[..bom.len()] == *bom {
            let body = &bytes[bom.len()..];
            return match decode_strict(body, *enc) {
                Some(decoded) => (strip_bom(decoded), enc.name()),
                None => (decode_lossy(bytes), enc.name()),
            };
        }
    }

    // 1.5) 无 BOM 的 UTF-16 探测：必须在候选 MBCS 之前裁决。
    // 原因:无 BOM 的 UTF-16 字节流被 GB18030/Big5 严格解码时“几乎总是成功”，
    // 且解出的乱码（如 `購`=U+8CFC）散落在常用区，`looks_like_mojibake` 拦不住——因此
    // 不靠顺序兜底，而是优先尝试 UTF-16 并校验解码质量（无 U+0000 且非 mojibake）。
    if let Some((text, enc)) = decode_utf16_no_bom(bytes) {
        return (text, enc.name());
    }

    // 2) ReadAware 候选编码严格解码 + mojibake 检测
    let mut gb_like_failed = false;
    for enc in CANDIDATE_ENCODINGS {
        if let Some(decoded) = decode_strict(bytes, *enc) {
            if !looks_like_mojibake(&decoded) {
                return (decoded, enc.name());
            }
            // GB18030 严格解成功但被判 mojibake：可能实际是 Big5 或噪声，
            // 留给 ICU 统计裁决特判。
            if enc.name() == "GB18030" {
                gb_like_failed = true;
            }
        }
    }

    // 3) ICU4J 统计识别：非 UTF-8 原始字节流（Big5/GB18030 区分 + 噪声容忍）
    if !std::str::from_utf8(bytes).is_ok() || gb_like_failed {
        match icu_detect_and_decode(bytes) {
            Some((text, enc)) => return (text, enc),
            None => {}
        }
    }

    // 4) lossy 兜底
    (decode_lossy(bytes), "windows-1252")
}

/// 用 ICU 统计识别裁决编码并解码。返回 None 表示无法判定。
pub fn icu_detect_and_decode(bytes: &[u8]) -> Option<(String, &'static str)> {
    let bg = icu_score_big5(bytes);
    let gb = icu_score_gb18030(bytes);

    // 取高分者；两者都 0 分则无法判定。
    let (score, enc, label) = if gb > bg {
        (gb, encoding_rs::GB18030, "gb18030")
    } else {
        (bg, encoding_rs::BIG5, "big5")
    };

    if score == 0 {
        return None;
    }
    // 解码（lossy，容忍识别器漏判的个别噪声字节）
    let (text, _, _) = enc.decode(bytes);
    Some((text.into_owned(), label))
}

// ──────────────────────────────────────────────
// ICU4J CharsetRecog_mbcs 的 Rust 移植
// ──────────────────────────────────────────────

/// 迭代游标：每编码一个 next_char，返回 (char_value, error, done)。
struct Iter {
    data: Vec<u8>,
    next_index: usize,
}

impl Iter {
    fn new(bytes: &[u8]) -> Self {
        Self {
            data: bytes.to_vec(),
            next_index: 0,
        }
    }
    fn next_byte(&mut self) -> i32 {
        if self.next_index >= self.data.len() {
            -1
        } else {
            let v = self.data[self.next_index] as i32 & 0xff;
            self.next_index += 1;
            v
        }
    }
}

/// 一次状态机扫描的结果统计。
struct MbcsStats {
    double_byte_char_count: usize,
    bad_char_count: usize,
    common_char_count: usize,
    total_char_count: usize,
}

/// 通用 MB 识别打分逻辑（ICU `CharsetRecog_mbcs.match` 的 Rust 版）。
/// `next_char` 闭包负责具体编码状态机，返回 (char_value, error)。
fn mbcs_score<F>(bytes: &[u8], common_chars: &[u32], mut next_char: F) -> i32
where
    F: FnMut(&mut Iter) -> (i64, bool),
{
    let mut iter = Iter::new(bytes);
    let mut st = MbcsStats {
        double_byte_char_count: 0,
        bad_char_count: 0,
        common_char_count: 0,
        total_char_count: 0,
    };

    loop {
        let done = iter.next_index >= iter.data.len();
        if done {
            break;
        }
        let (cv, error) = next_char(&mut iter);
        st.total_char_count += 1;
        if error {
            st.bad_char_count += 1;
        } else {
            if cv > 0xff {
                st.double_byte_char_count += 1;
                if common_chars.binary_search(&(cv as u32)).is_ok() {
                    st.common_char_count += 1;
                }
            }
        }
        // Bail out early if byte data doesn't match encoding scheme.
        if st.bad_char_count >= 2 && st.bad_char_count * 5 >= st.double_byte_char_count {
            break;
        }
    }

    // Not many multi-byte chars.
    if st.double_byte_char_count <= 10 && st.bad_char_count == 0 {
        if st.double_byte_char_count == 0 && st.total_char_count < 10 {
            return 0;
        }
        // ASCII or ISO file?
        return 10;
    }
    // Too many chars that don't fit the encoding.
    if st.double_byte_char_count < 20 * st.bad_char_count {
        return 0;
    }

    // Frequency-of-occurence statistics.
    if st.double_byte_char_count == 0 {
        return 30;
    }
    let max_val = ((st.double_byte_char_count as f64) / 4.0).ln();
    if max_val <= 0.0 {
        return 0;
    }
    let scale_factor = 90.0 / max_val;
    let confidence = (((st.common_char_count + 1) as f64).ln() * scale_factor + 10.0) as i32;
    confidence.min(100)
}

/// Big5 状态机（ICU `CharsetRecog_big5.nextChar`）：
/// 首字节单字（<=0x7f 或 ==0xff）；否则取次字节，char=(f<<8)|s，
/// second<0x40 || ==0x7f || ==0xff 判 error。
fn big5_next_char(it: &mut Iter) -> (i64, bool) {
    let first = it.next_byte();
    if first < 0 {
        return (0, false);
    }
    if first <= 0x7f || first == 0xff {
        return (first as i64, false);
    }
    let second = it.next_byte();
    if second < 0 {
        return (0, false);
    }
    let cv = ((first as i64) << 8) | (second as i64);
    let err = second < 0x40 || second == 0x7f || second == 0xff;
    (cv, err)
}

/// GB18030 状态机（ICU `CharsetRecog_gb_18030.nextChar`）：
/// first<=0x80 单字节；first 0x81-0xFE 分支：second 0x40-0x7E 或 0x80-0xFE 为 2 字节；
/// second 0x30-0x39 时再读 third、fourth 判 4 字节（third 0x81-0xFE 且 fourth 0x30-0x39）。
fn gb18030_next_char(it: &mut Iter) -> (i64, bool) {
    let first = it.next_byte();
    if first < 0 {
        return (0, false);
    }
    if first <= 0x80 {
        return (first as i64, false);
    }
    let second = it.next_byte();
    if second < 0 {
        return (0, false);
    }
    let mut cv = ((first as i64) << 8) | (second as i64);
    if first >= 0x81 && first <= 0xfe {
        // Two byte char
        if (second >= 0x40 && second <= 0x7e) || (second >= 0x80 && second <= 0xfe) {
            return (cv, false);
        }
        // Four byte char
        if second >= 0x30 && second <= 0x39 {
            let third = it.next_byte();
            if third < 0 {
                return (cv, true);
            }
            if third >= 0x81 && third <= 0xfe {
                let fourth = it.next_byte();
                if fourth < 0 {
                    return (cv, true);
                }
                if fourth >= 0x30 && fourth <= 0x39 {
                    cv = (cv << 16) | ((third as i64) << 8) | (fourth as i64);
                    return (cv, false);
                }
            }
        }
        // Illegal sequence
        return (cv, true);
    }
    // first not in 0x81-0xFE (e.g. 0x81-0xFE 范围的 else) — treat as error
    (cv, true)
}

/// Big5 高频字表（ICU CharsetRecog_big5.commonChars）。
const BIG5_COMMON: &[u32] = &[
    0xa140, 0xa141, 0xa142, 0xa143, 0xa147, 0xa149, 0xa175, 0xa176, 0xa440, 0xa446, 0xa447, 0xa448,
    0xa451, 0xa454, 0xa457, 0xa464, 0xa46a, 0xa46c, 0xa477, 0xa4a3, 0xa4a4, 0xa4a7, 0xa4c1, 0xa4ce,
    0xa4d1, 0xa4df, 0xa4e8, 0xa4fd, 0xa540, 0xa548, 0xa558, 0xa569, 0xa5cd, 0xa5e7, 0xa657, 0xa661,
    0xa662, 0xa668, 0xa670, 0xa6a8, 0xa6b3, 0xa6b9, 0xa6d3, 0xa6db, 0xa6e6, 0xa6f2, 0xa740, 0xa751,
    0xa759, 0xa7da, 0xa8a3, 0xa8a5, 0xa8ad, 0xa8d1, 0xa8d3, 0xa8e4, 0xa8fc, 0xa9c0, 0xa9d2, 0xa9f3,
    0xaa6b, 0xaaba, 0xaabe, 0xaacc, 0xaafc, 0xac47, 0xac4f, 0xacb0, 0xacd2, 0xad59, 0xaec9, 0xafe0,
    0xb0ea, 0xb16f, 0xb2b3, 0xb2c4, 0xb36f, 0xb44c, 0xb44e, 0xb54c, 0xb5a5, 0xb5bd, 0xb5d0, 0xb5d8,
    0xb671, 0xb7ed, 0xb867, 0xb944, 0xbad8, 0xbb44, 0xbba1, 0xbdd1, 0xc2c4, 0xc3b9, 0xc440, 0xc45f,
];

/// GB18030 高频字表（ICU CharsetRecog_gb_18030.commonChars）。
const GB18030_COMMON: &[u32] = &[
    0xa1a1, 0xa1a2, 0xa1a3, 0xa1a4, 0xa1b0, 0xa1b1, 0xa1f1, 0xa1f3, 0xa3a1, 0xa3ac, 0xa3ba, 0xb1a8,
    0xb1b8, 0xb1be, 0xb2bb, 0xb3c9, 0xb3f6, 0xb4f3, 0xb5bd, 0xb5c4, 0xb5e3, 0xb6af, 0xb6d4, 0xb6e0,
    0xb7a2, 0xb7a8, 0xb7bd, 0xb7d6, 0xb7dd, 0xb8b4, 0xb8df, 0xb8f6, 0xb9ab, 0xb9c9, 0xb9d8, 0xb9fa,
    0xb9fd, 0xbacd, 0xbba7, 0xbbd6, 0xbbe1, 0xbbfa, 0xbcbc, 0xbcdb, 0xbcfe, 0xbdcc, 0xbecd, 0xbedd,
    0xbfb4, 0xbfc6, 0xbfc9, 0xc0b4, 0xc0ed, 0xc1cb, 0xc2db, 0xc3c7, 0xc4dc, 0xc4ea, 0xc5cc, 0xc6f7,
    0xc7f8, 0xc8ab, 0xc8cb, 0xc8d5, 0xc8e7, 0xc9cf, 0xc9fa, 0xcab1, 0xcab5, 0xcac7, 0xcad0, 0xcad6,
    0xcaf5, 0xcafd, 0xccec, 0xcdf8, 0xceaa, 0xcec4, 0xced2, 0xcee5, 0xcfb5, 0xcfc2, 0xcfd6, 0xd0c2,
    0xd0c5, 0xd0d0, 0xd0d4, 0xd1a7, 0xd2aa, 0xd2b2, 0xd2b5, 0xd2bb, 0xd2d4, 0xd3c3, 0xd3d0, 0xd3fd,
    0xd4c2, 0xd4da, 0xd5e2, 0xd6d0,
];

/// ICU 式 Big5 置信度打分。
fn icu_score_big5(bytes: &[u8]) -> i32 {
    mbcs_score(bytes, BIG5_COMMON, big5_next_char)
}

/// ICU 式 GB18030 置信度打分。
fn icu_score_gb18030(bytes: &[u8]) -> i32 {
    mbcs_score(bytes, GB18030_COMMON, gb18030_next_char)
}

/// 严格解码：坏字节即返回 None（对应 ReadAware 的 `{ fatal: true }`）。
fn decode_strict(bytes: &[u8], enc: &'static Encoding) -> Option<String> {
    enc.decode_without_bom_handling_and_without_replacement(bytes)
        .map(|cow| cow.into_owned())
}

/// 探测无 BOM 的 UTF-16LE/BE（必须在候选 MBCS 之前调用）。
///
/// 无 BOM 的 UTF-16 字节流被 GB18030/Big5 严格解码时“几乎总是成功”，且解出的乱码
/// （如 `購`=U+8CFC）散落在常用区，`looks_like_mojibake` 拦不住。所以不能靠候选循环
/// 的顺序兜底，而要优先尝试 UTF-16 并校验解码质量。
///
/// 关键鉴定信号是 **null（0x00）字节密度**：UTF-16 文本里每个 ASCII/半角/字形
/// （数字、英文、半角标点、换行、全角空格 U+3000 等）的码元有一个半字节恒为 0x00，
/// 因此 null 密度随文本 ACII 比例而显著升高；而 GB18030/GBK/Big5 源的双字节汉字
/// 与单字节 ASCII 都几乎不产生 null（实测中文样本 null=0）。故「null 存在 + 密度
/// 超阈值」即 UTF-16 的强证据，可避免把真 MBCS 误判。
#[allow(clippy::manual_float_methods)]
fn decode_utf16_no_bom(bytes: &[u8]) -> Option<(String, &'static Encoding)> {
    if bytes.len() < 4 || bytes.len() % 2 != 0 {
        return None;
    }
    let null_count = bytes.iter().filter(|&&b| b == 0).count();
    // 无任何 null 或 null 密度过低（<1% 半字节）：无 UTF-16 信号，回落 MBCS（防误判）。
    // 1% 阈值：UTF-16 中文文档哪怕每隔几十字才一个 ASCII/全角空格，也能明显超过；
    // 而 MBCS 文本实际上传 null 均为 0。
    if null_count == 0 || null_count as f64 / (bytes.len() as f64 / 2.0) < 0.01 {
        return None;
    }
    for enc in [encoding_rs::UTF_16LE, encoding_rs::UTF_16BE] {
        if let Some(decoded) = decode_strict(bytes, enc) {
            if !decoded.contains('\u{0}') && !looks_like_mojibake(&decoded) {
                return Some((decoded, enc));
            }
        }
    }
    None
}

/// lossy 兜底（windows-1252 单字节，必成功）。
fn decode_lossy(bytes: &[u8]) -> String {
    let (cow, _, _) = encoding_rs::WINDOWS_1252.decode(bytes);
    cow.into_owned()
}

/// 去掉首字符 BOM（0xFEFF）。
fn strip_bom(mut value: String) -> String {
    if value.starts_with('\u{feff}') {
        value.remove(0);
    }
    value
}

/// mojibake 检测（对应 ReadAware `looksLikeMojibake`）：
/// 前 4096 字符里 U+FFFD 替换符、私用区(E000-F8FF)、控制区(0080-00A0) 比例 > 2% 判定为乱码候选。
fn looks_like_mojibake(value: &str) -> bool {
    // 只采样前 4096 字节的字符边界（从 4096 往回退到合法 UTF-8 起点，避免中间切片 panic）
    let mut cut = value.len().min(4096);
    while cut > 0 && !value.is_char_boundary(cut) {
        cut -= 1;
    }
    let sample = &value[..cut];
    if sample.is_empty() {
        return false;
    }
    let mut suspicious = 0usize;
    for c in sample.chars() {
        let code = c as u32;
        if code == 0xfffd
            || (0xe000..=0xf8ff).contains(&code)
            || (0x0080..=0x00a0).contains(&code)
        {
            suspicious += 1;
        }
    }
    suspicious > (sample.chars().count() * 2) / 100
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gb18030_restores_chinese() {
        let (cow, _, _) = encoding_rs::GB18030.encode("第一章 禁断之谋\n夜色如墨，他推开门扉。\n123 456 789");
        let bytes = cow.into_owned();
        assert!(!std::str::from_utf8(&bytes).is_ok(), "GB18030 字节不应是合法 UTF-8");
        let (text, enc) = decode_text(&bytes);
        assert_eq!(enc, "gb18030");
        assert!(text.contains("禁断之谋") && text.contains("夜色如墨"), "GB18030 应还原中文, got {:?}", &text[..40.min(text.len())]);
        assert!(!text.contains('\u{fffd}'), "不应有替换符 �");
    }

    #[test]
    fn utf8_passthrough() {
        let (text, enc) = decode_text("正常UTF8文本 123".as_bytes());
        assert_eq!(text, "正常UTF8文本 123");
        assert_eq!(enc, "UTF-8");
    }

    #[test]
    fn bom_stripped() {
        let mut bytes = vec![0xef, 0xbb, 0xbf];
        bytes.extend_from_slice("带BOM文本".as_bytes());
        let (text, enc) = decode_text(&bytes);
        assert_eq!(text, "带BOM文本", "BOM 应剥离, got {:?}", text);
        assert_eq!(enc, "UTF-8");
    }

    #[test]
    fn big5_restores_chinese() {
        // 繁体中文 Big5 编码（注意：Big5 无简体"断"字，须用繁体"斷"）
        let (cow, _, _) = encoding_rs::BIG5.encode("第一章 禁斷之謀\n夜色如墨，他推開門扉。");
        let bytes = cow.into_owned();
        assert!(!std::str::from_utf8(&bytes).is_ok(), "Big5 字节不应是合法 UTF-8");
        let (text, enc) = decode_text(&bytes);
        // Big5 可被候选严格解码直接识别（ReadAware 路径），enc.name() 为 encoding_rs 规范名 "Big5"；
        // 断言落在 encoding_rs 的 BIG5 上即可。
        let is_big5 = matches!(enc, "Big5" | "big5");
        assert!(is_big5, "Big5 应识别为 big5, got {enc:?}");
        assert!(text.contains("禁斷之謀"), "Big5 应还原繁体中文, got {:?}", &text[..40.min(text.len())]);
        assert!(!text.contains('\u{fffd}'), "不应有替换符 �");
    }

    #[test]
    fn icu_scores_distinguish_big5_vs_gb() {
        // 繁体 Big5 字节：Big5 置信度应显著高于 GB18030
        let (cow, _, _) = encoding_rs::BIG5.encode("第一回 攜手\n夜色如墨，他推開門扉，月光灑落一地清輝。");
        let bytes = cow.into_owned();
        let bg = icu_score_big5(&bytes);
        let gb = icu_score_gb18030(&bytes);
        assert!(bg > gb, "Big5 文本 Big5 分数应更高: big5={bg}, gb={gb}");
    }

    #[test]
    fn icu_scores_distinguish_gb_vs_big5() {
        // 简体 GB18030 字节：GB18030 置信度应高于 Big5
        let (cow, _, _) = encoding_rs::GB18030.encode("她推开房门，夜色中月光洒落一地清辉。");
        let bytes = cow.into_owned();
        let bg = icu_score_big5(&bytes);
        let gb = icu_score_gb18030(&bytes);
        assert!(gb > bg, "GB18030 文本 GB 分数应更高: gb={gb}, big5={bg}");
    }

    #[test]
    fn noisy_big5_still_detected() {
        // 含少量噪声字节的 Big5 仍应识别（ICU 需容忍）
        let (cow, _, _) = encoding_rs::BIG5.encode("他推開門扉，夜色如墨。");
        let mut bytes = cow.into_owned();
        bytes.push(0xff); // 插入一个不合规字节（Big5 state 中 0xff 是单字节，仍合法）
        let bg = icu_score_big5(&bytes);
        assert!(bg > 0, "含少量噪声的 Big5 应仍有置信度, got {bg}");
    }

    #[test]
    fn long_gb18030_text_no_panic_and_roundtrip() {
        // 回归：>4096 字节的 GB18030 文本不得在 looks_like_mojibake 切片处 panic（字符边界 bug，2026-08-10 真实 sxsy 章节触发）。
        // 合成 >12KB 的简体段落，确保超过 4096 字节采样窗口且含多个多字节字符。
        let para = "她推开房门，夜色如墨，月光洒落一地清辉。他慢慢走到窗前，望着远方。";
        let mut content = String::new();
        for _ in 0..200 {
            content.push_str(para);
        }
        assert!(content.as_bytes().len() > 4096, "测试样本需 >4096 字节");
        let (gb, _, had_err) = encoding_rs::GB18030.encode(&content);
        assert!(!had_err);
        let bytes = gb.as_ref();
        let (text, enc) = decode_text(bytes);
        assert!(matches!(enc, "GB18030" | "gb18030"), "简体长文本应判 GB18030, got {enc}");
        let total = content.chars().count();
        let matched = content.chars().zip(text.chars()).filter(|(a, b)| a == b).count();
        let ratio = matched as f64 / total as f64;
        assert!(ratio > 0.99, "一致率应 >99%: {:.4}", ratio);
        assert_eq!(text.matches('\u{fffd}').count(), 0, "不应有替换符");
    }

    #[test]
    fn utf16le_bom_document_decodes() {
        // 回归：UTF-16 LE + BOM 文档走 BOM 路径 UTF-16LE 分支正确解码。
        // 来源：彬哥提供的 [sxsy.org]禁断之谋 txt 实为完好 UTF-16 文档（非转死），BOM 路径即可还原。
        // NOTE: encoding_rs::UTF_16LE.encode() 实际输出 UTF-8 字节（编码器被 Unicode-化），不能用来造 UTF-16LE 样本；
        // 这里硬编码 python 生成的正确 UTF-16LE 字节。
        let content = "第1章\n\u{3000}\u{3000}这是母亲来到加拿大的第四个春天，她和我说话。";
        #[rustfmt::skip]
        let utf16 = [
            44, 123, 49, 0, 224, 122, 10, 0, 0, 48, 0, 48,
            217, 143, 47, 102, 205, 107, 178, 78, 101, 103, 48, 82,
            160, 82, 255, 98, 39, 89, 132, 118, 44, 123, 219, 86,
            42, 78, 37, 102, 41, 89, 12, 255, 121, 89, 140, 84,
            17, 98, 244, 139, 221, 139, 2, 48,
        ];
        let mut raw = vec![0xff, 0xfe]; // UTF-16 LE BOM
        raw.extend_from_slice(&utf16);
        let (text, enc) = decode_text(&raw);
        assert_eq!(enc, "UTF-16LE", "UTF-16+BOM 应走 BOM 路径判 UTF-16LE, got {enc}");
        assert_eq!(text, content, "应完整还原 UTF-16 内容");
        assert_eq!(text.matches('\u{fffd}').count(), 0, "不应有替换符");
    }

    #[test]
    fn utf16le_no_bom_document_decodes() {
        // 回归：无 BOM 的 UTF-16LE 文档（BOM 被剥离/编辑器去掉）必须经
        // decode_utf16_no_bom 探测还原，而不是被 GB18030/Big5 误判成乱码。
        // 复用上面 BOM 用例的字节（来源：彬哥提供的禁断之谋 txt 真实 UTF-16 字节），去掉 BOM。
        let content = "第1章\n\u{3000}\u{3000}这是母亲来到加拿大的第四个春天，她和我说话。";
        #[rustfmt::skip]
        let utf16 = [
            44, 123, 49, 0, 224, 122, 10, 0, 0, 48, 0, 48,
            217, 143, 47, 102, 205, 107, 178, 78, 101, 103, 48, 82,
            160, 82, 255, 98, 39, 89, 132, 118, 44, 123, 219, 86,
            42, 78, 37, 102, 41, 89, 12, 255, 121, 89, 140, 84,
            17, 98, 244, 139, 221, 139, 2, 48,
        ];
        let (text, enc) = decode_text(&utf16); // 无 BOM
        assert_eq!(enc, "UTF-16LE", "无 BOM UTF-16LE 应判 UTF-16LE（非乱码），got {enc}");
        assert_eq!(text, content, "应完整还原无 BOM UTF-16 内容");
        assert_eq!(text.matches('\u{fffd}').count(), 0, "不应有替换符");
    }

    #[test]
    fn utf16_no_bom_not_confused_with_gb18030() {
        // 防误判护栏：真 GB18030 简体文本不得被 decode_utf16_no_bom 抢走。
        // GB18030 汉字字节按 UTF-16LE 解会大量落入 PUA/surrogate 区，被 mojibake 拦截。
        let (cow, _, _) = encoding_rs::GB18030.encode("第一章 禁断之谋\n夜色如墨，他推开门扉。");
        let bytes = cow.into_owned();
        let (text, enc) = decode_text(&bytes);
        assert!(text.contains("禁断之谋") && text.contains("夜色如墨"),
            "GB18030 文本应仍还原为简体中文, got {:?}", &text[..40.min(text.len())]);
        assert!(!matches!(enc, "UTF-16LE" | "UTF-16BE"),
            "真 GB18030 不应被误判为 UTF-16, got {enc}");
    }
}