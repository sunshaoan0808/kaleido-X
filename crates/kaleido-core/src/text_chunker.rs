//! 长文本语义分块工具。
//!
//! 参考实现（纯 std + 既有 regex 依赖移植）：Openwrite `tools/text_chunker.py`
//! （章节识别 + 语义分块）。用于 200 万字级小说等超长文本的上下文构建，
//! 替代对章节正文的整段硬截断。

use regex::Regex;
use std::sync::OnceLock;

/// 一个文本分块。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextChunk {
    /// 分块序号（从 0 开始）。
    pub index: usize,
    /// 可读的章节范围描述，如「第一章」或「第一章 ~ 第三章」。
    pub chapter_range: String,
    /// 分块正文。
    pub text: String,
}

/// 章节识别正则（单行匹配，调用前先 trim）。
///
/// 覆盖：中文「第X章/节/回/卷/篇」、`Chapter N`（大小写）、
/// 序章/楔子等特殊章节，以及「1、」「2.」等纯数字标记。
fn chapter_marker() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"^(?:第[零一二三四五六七八九十百千万\d]+[章节回卷篇]|[Cc]hapter\s+\d+|(?:序章|楔子|引子|终章|尾声|番外|后记|前言|附录)|\d{1,4}[\s　、.．])",
        )
        .expect("chapter marker regex must be valid")
    })
}

/// 按空行/段落边界切分文本为段落块。
///
/// - 存在空行时：连续非空行聚合为一个段落块，块间以空行分隔；
/// - 无空行时：每一非空行即一个段落。
/// 空白文本返回空 Vec。供二次切与压缩器使用。
pub fn split_paragraphs(text: &str) -> Vec<String> {
    let lines: Vec<&str> = text.lines().collect();
    let has_blank = lines.iter().any(|l| l.trim().is_empty());

    if !has_blank {
        return lines
            .iter()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.to_string())
            .collect();
    }

    let mut result = Vec::new();
    let mut current = String::new();
    for line in lines {
        if line.trim().is_empty() {
            if !current.is_empty() {
                result.push(std::mem::take(&mut current));
            }
        } else if current.is_empty() {
            current.push_str(line);
        } else {
            current.push('\n');
            current.push_str(line);
        }
    }
    if !current.is_empty() {
        result.push(current);
    }
    result
}

/// 内部章节单元（用于聚合前的统一表示）。
struct Chapter {
    title: String,
    text: String,
}

/// 聚合单元：可以是一个章节，也可以是超长章节按段落二次切出的片段。
struct Unit {
    title: String,
    text: String,
    chars: usize,
}

/// 将文本语义分块。
///
/// - 有 ≥2 个章节标记：按章节聚合为 chunk，尽量不拆单章；超长章按空行分段二次切；
/// - 无章节标记：按空行虚拟段落，每 ~min(5000, chunk_size/3) 字切一个 chunk；
/// - 相邻小 chunk（< min_chunk_size）与下一块合并。
pub fn chunk_text(text: &str, chunk_size: usize, min_chunk_size: usize) -> Vec<TextChunk> {
    let chunk_size = chunk_size.max(1);
    let min_chunk_size = min_chunk_size.max(1);
    let chapters = detect_chapters(text, chunk_size);
    aggregate_chunks(chapters, chunk_size, min_chunk_size)
}

/// 识别章节标记并切出章节列表。
fn detect_chapters(text: &str, chunk_size: usize) -> Vec<Chapter> {
    let lines: Vec<&str> = text.lines().collect();
    let mut chapter_lines: Vec<(usize, String)> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let stripped = line.trim();
        if !stripped.is_empty() && chapter_marker().is_match(stripped) {
            chapter_lines.push((i, stripped.to_string()));
        }
    }

    // 章节标记不足两个 → 按空行虚拟段落切割
    if chapter_lines.len() < 2 {
        let virtual_size = (chunk_size / 3).min(5000).max(1);
        return fallback_paragraph_chapters(text, &lines, virtual_size);
    }

    let mut chapters = Vec::new();

    // 首个章节标记之前的前导正文（若足够长）
    if chapter_lines[0].0 > 0 {
        let pre = lines[..chapter_lines[0].0].join("\n");
        let pre = pre.trim();
        if pre.chars().count() > 100 {
            chapters.push(Chapter {
                title: "[前言/序]".to_string(),
                text: pre.to_string(),
            });
        }
    }

    for idx in 0..chapter_lines.len() {
        let start = chapter_lines[idx].0;
        let end = if idx + 1 < chapter_lines.len() {
            chapter_lines[idx + 1].0
        } else {
            lines.len()
        };
        let chapter_text = lines[start..end].join("\n");
        let chapter_text = chapter_text.trim();
        if !chapter_text.is_empty() {
            chapters.push(Chapter {
                title: chapter_lines[idx].1.clone(),
                text: chapter_text.to_string(),
            });
        }
    }
    chapters
}

/// 无章节标记时的虚拟段落切割。
fn fallback_paragraph_chapters(
    text: &str,
    lines: &[&str],
    virtual_size: usize,
) -> Vec<Chapter> {
    let mut chapters = Vec::new();
    let mut current_lines: Vec<&str> = Vec::new();
    let mut current_chars = 0usize;

    for line in lines {
        current_lines.push(line);
        current_chars += line.chars().count() + 1; // +1 for '\n'
        if current_chars >= virtual_size && line.trim().is_empty() {
            let ch_text = current_lines.join("\n");
            let ch_text = ch_text.trim();
            if !ch_text.is_empty() {
                chapters.push(Chapter {
                    title: format!("[段落 {}]", chapters.len() + 1),
                    text: ch_text.to_string(),
                });
            }
            current_lines.clear();
            current_chars = 0;
        }
    }

    if !current_lines.is_empty() {
        let ch_text = current_lines.join("\n");
        let ch_text = ch_text.trim();
        if !ch_text.is_empty() {
            chapters.push(Chapter {
                title: format!("[段落 {}]", chapters.len() + 1),
                text: ch_text.to_string(),
            });
        }
    }

    if chapters.is_empty() {
        let t = text.trim();
        if !t.is_empty() {
            chapters.push(Chapter {
                title: "[全文]".to_string(),
                text: t.to_string(),
            });
        }
    }
    chapters
}

/// 展开章节为聚合单元：超长章按空行分段二次切。
fn expand_to_units(chapters: Vec<Chapter>, chunk_size: usize) -> Vec<Unit> {
    let mut units = Vec::new();
    for ch in chapters {
        let ch_chars = ch.text.chars().count();
        if ch_chars <= chunk_size {
            units.push(Unit {
                title: ch.title.clone(),
                text: ch.text,
                chars: ch_chars,
            });
            continue;
        }

        // 超长章：按段落边界二次切
        let mut current = String::new();
        let mut current_chars = 0usize;
        for p in split_paragraphs(&ch.text) {
            let p_chars = p.chars().count();
            if !current.is_empty() && current_chars + p_chars > chunk_size {
                units.push(Unit {
                    title: ch.title.clone(),
                    text: current,
                    chars: current_chars,
                });
                current = String::new();
                current_chars = 0;
            }
            if !current.is_empty() {
                current.push('\n');
            }
            current.push_str(&p);
            current_chars += p_chars;
        }
        if !current.is_empty() {
            units.push(Unit {
                title: ch.title,
                text: current,
                chars: current_chars,
            });
        }
    }
    units
}

/// 将单元聚合法分块，并合并尾部小 chunk。
fn aggregate_chunks(
    chapters: Vec<Chapter>,
    chunk_size: usize,
    min_chunk_size: usize,
) -> Vec<TextChunk> {
    let units = expand_to_units(chapters, chunk_size);

    let mut groups: Vec<Vec<Unit>> = Vec::new();
    let mut current: Vec<Unit> = Vec::new();
    let mut current_chars = 0usize;
    for unit in units {
        if !current.is_empty() && current_chars + unit.chars > chunk_size {
            groups.push(std::mem::take(&mut current));
            current_chars = 0;
        }
        current_chars += unit.chars;
        current.push(unit);
    }
    if !current.is_empty() {
        groups.push(current);
    }

    // 尾部小 chunk 与上一块合并
    if groups.len() >= 2 {
        let last = groups.pop().unwrap();
        let last_chars: usize = last.iter().map(|u| u.chars).sum();
        if last_chars < min_chunk_size {
            if let Some(prev) = groups.last_mut() {
                let prev_chars: usize = prev.iter().map(|u| u.chars).sum();
                if prev_chars + last_chars <= chunk_size * 3 / 2 {
                    prev.extend(last);
                } else {
                    groups.push(last);
                }
            } else {
                groups.push(last);
            }
        } else {
            groups.push(last);
        }
    }

    let mut chunks = Vec::with_capacity(groups.len());
    for (i, group) in groups.into_iter().enumerate() {
        chunks.push(build_chunk(i, group));
    }
    chunks
}

/// 从单元组构建 TextChunk（含章节范围描述）。
fn build_chunk(index: usize, group: Vec<Unit>) -> TextChunk {
    let text = group
        .iter()
        .map(|u| u.text.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    let first = &group[0].title;
    let last = &group[group.len() - 1].title;
    let chapter_range = if group.len() == 1 || first == last {
        first.clone()
    } else {
        format!("{} ~ {}", first, last)
    };
    TextChunk {
        index,
        chapter_range,
        text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn char_count(s: &str) -> usize {
        s.chars().count()
    }

    #[test]
    fn detects_chinese_arabic_and_chapter_markers() {
        let text = "\
第一章 初见
窗外下着细雨，她推门而入。

第3章 重逢
多年以后，他们在桥头再次相遇。

Chapter 4
夜色深沉，有人悄悄跟在他们身后。

chapter 5
风起了，街道上再无一人。

尾声
他们约定，明年春天再见。";
        // 每章约 20+ 字，chunk_size 取小值使每章各成一块
        let chunks = chunk_text(text, 12, 4);
        assert!(chunks.len() >= 4, "应识别出多个章节，实际 {}", chunks.len());
        let ranges: Vec<&str> = chunks
            .iter()
            .map(|c| c.chapter_range.as_str())
            .collect();
        assert!(
            ranges.iter().any(|r| r.contains("第一章")),
            "未识别中文第一章：{ranges:?}"
        );
        assert!(
            ranges.iter().any(|r| r.contains("第3章")),
            "未识别阿拉伯数字第3章：{ranges:?}"
        );
        assert!(
            ranges.iter().any(|r| r.contains("Chapter")),
            "未识别 Chapter 标记：{ranges:?}"
        );
    }

    #[test]
    fn splits_oversized_chapter_into_multiple_chunks() {
        let long_body = "\
第一段，清晨的街道行人渐多，天气平静如常。
第二段却忽然转折，有人喊出惊天秘密，众人惊醒。
第三段决定追查到底，却发现线索早已被销毁。
第四段，死去的故人竟再次出现，局面彻底失控。
第五段他们发现真相，原来凶手就在队伍之中。
第六段夜里，众人回到老屋，紧张地商议对策。";
        let text = format!("第一章 长章\n{long_body}\n\n第二章 短章\n他们就此别过，各自天涯。");
        let chunks = chunk_text(&text, 50, 12);
        assert!(
            chunks.len() >= 3,
            "超长章应按段落切出多个 chunk，实际 {}",
            chunks.len()
        );
        assert!(
            chunks[0].text.contains("第一章"),
            "首个 chunk 应包含章标题"
        );
        let total_kept: usize = chunks.iter().map(|c| char_count(&c.text)).sum();
        assert!(total_kept <= char_count(&text), "分块不应丢内容");
    }

    #[test]
    fn chunks_no_marker_text_by_virtual_paragraphs() {
        let text = "\
第一段，描写街道与行人。

第二段，忽然传来消息。

第三段，众人赶往事发地。

第四段，发现重要的线索。

第五段，夜色渐浓，一切归于平静。";
        // 无章节标记 → 按空行虚拟段落切
        let chunks = chunk_text(text, 40, 8);
        assert!(
            chunks.len() >= 2,
            "无章节文本应切出多个 chunk，实际 {}",
            chunks.len()
        );
        assert!(
            chunks
                .iter()
                .any(|c| c.chapter_range.starts_with("[段落")),
            "虚拟段落应带有 [段落 N] 范围标记"
        );
    }

    #[test]
    fn merges_trailing_small_chunk() {
        // 主章正文 ~49 字（接近 chunk_size 60，尾章加入后会超限成独立小块），
        // 尾章 < min_chunk_size(20) 且合并后仍 ≤ 1.5×chunk_size → 并入前一块。
        let base = "他沿着河岸慢慢走着，心里盘算着接下来的计划。";
        let body = format!("{}{}", base, "字".repeat(27));
        let text = format!("第一章 主章\n{body}\n\n第二章 尾\n别了。");
        let chunks = chunk_text(&text, 60, 20);
        assert_eq!(
            chunks.len(),
            1,
            "尾部小 chunk 应合并到前一块，实际 {}",
            chunks.len()
        );
        assert!(chunks[0].text.contains("第一章"));
        assert!(chunks[0].text.contains("第二章"));
    }

    #[test]
    fn empty_input_yields_no_chunks() {
        assert!(chunk_text("", 100, 20).is_empty());
    }
}
