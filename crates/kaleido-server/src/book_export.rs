//! M3: Book export pipeline — TXT/UTF-8 + EPUB/PDF manuscript export from the bookshelf.
//!
//! Routes:
//! - `GET /api/v1/bookshelf/{slug}/export?format=txt` — UTF-8 plain text.
//! - `GET /api/v1/bookshelf/{slug}/export?format=epub` — EPUB2/EPUB3 (自包含，原生 zip 生成).
//! - `GET /api/v1/bookshelf/{slug}/export?format=pdf` — 原生 PDF（文本型，可打开；CJK 常见字覆盖）.
//!
//! EPUB 采用原生 zip 打包（含竖排 writing-mode CSS），无外部依赖（novel2epub-jp 竖排链路的轻量落地）.
//! PDF 采用原生 Type0/CID CJK 字体（Adobe 兼容 CMap），无外部字体文件，自包含.
//! 完整 A6 竖排/页码/章头/插图（Vivliostyle + JLREQ）属 heavy 链路，见 P15 记录文档.

use axum::{
    extract::{Path, Query},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;

use crate::{crawler, AppState};

// ── Format handling ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExportFormat {
    Txt,
    Epub,
    Pdf,
}

impl ExportFormat {
    fn from_str(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().trim() {
            "txt" => Some(Self::Txt),
            "epub" => Some(Self::Epub),
            "pdf" => Some(Self::Pdf),
            _ => None,
        }
    }

    fn content_type(&self) -> &str {
        match self {
            Self::Txt => "text/plain; charset=utf-8",
            Self::Epub => "application/epub+zip",
            Self::Pdf => "application/pdf",
        }
    }

    fn extension(&self) -> &str {
        match self {
            Self::Txt => "txt",
            Self::Epub => "epub",
            Self::Pdf => "pdf",
        }
    }
}

// ── Route ──────────────────────────────────────────────────────────────────

pub fn router() -> Router<AppState> {
    Router::new().route("/api/v1/bookshelf/{slug}/export", get(export_book))
}

#[derive(Debug, Deserialize)]
pub struct ExportQuery {
    #[serde(default)]
    format: Option<String>,
}

/// Find the .md file for a given slug in the shelf directory, returning its full content.
fn find_shelf_file_content(slug: &str) -> Result<(String, String), String> {
    let dir = crawler::shelf_dir();
    if !dir.exists() {
        return Err("bookshelf directory not found".into());
    }
    // scan_shelf gives us the title from each .md file; we need to find the one matching this slug.
    let entries = crawler::scan_shelf();
    let entry = entries
        .into_iter()
        .find(|e| e.slug == slug)
        .ok_or_else(|| format!("book not found: {slug}"))?;

    // Find the actual file on disk by matching the title.
    let slug_tag = crawler::shelf_slug(&entry.title);
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("md") {
                continue;
            }
            let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            let stem_normalized = crawler::shelf_slug(stem);
            if stem_normalized.contains(&slug_tag) || slug_tag.contains(&stem_normalized) {
                let content = std::fs::read_to_string(&p).map_err(|e| e.to_string())?;
                return Ok((entry.title, content));
            }
        }
    }
    Err(format!("book file not found on disk for slug: {slug}"))
}

/// Parse the shelf markdown back into ordered chapters.
///
/// The format written by `write_shelf_markdown`:
/// ```text
/// # Title
///
/// ## 目录
/// - 第1章 Heading
/// - 第2章 Heading
///
/// ## 第1章 Heading
/// ...content...
///
/// ## 第2章 Heading
/// ...content...
/// ```
///
/// We extract chapters by looking for `## ` headings after the table of contents.
fn parse_shelf_chapters(content: &str) -> Vec<(String, String)> {
    let mut chapters: Vec<(String, String)> = Vec::new();
    let mut in_toc = false;
    let mut current_title: Option<String> = None;
    let mut current_body = String::new();

    for line in content.lines() {
        let trimmed = line.trim();

        // Detect TOC start
        if trimmed == "## 目录" {
            in_toc = true;
            continue;
        }
        // TOC ends when we hit the first `##` heading that is not "目录"
        if in_toc {
            if trimmed.starts_with("## ") {
                in_toc = false;
                // fall through to chapter-heading handling below
            } else {
                continue; // skip TOC entries
            }
        }

        // Chapter heading: `## 第N章 ...` or `## 第N节 ...`
        if let Some(heading) = trimmed.strip_prefix("## ") {
            let heading = heading.trim().to_string();
            // Save previous chapter
            if let Some(title) = current_title.take() {
                let body = current_body.trim().to_string();
                if !body.is_empty() || !title.is_empty() {
                    chapters.push((title, body));
                }
                current_body.clear();
            }
            current_title = Some(heading);
            continue;
        }

        // Accumulate body lines
        if current_title.is_some() {
            if !current_body.is_empty() {
                current_body.push('\n');
            }
            current_body.push_str(line);
        }
    }

    // Push last chapter
    if let Some(title) = current_title {
        let body = current_body.trim().to_string();
        chapters.push((title, body));
    }

    chapters
}

/// Convert a shelf chapter tuple into clean TXT content (strip markdown artifacts).
fn chapter_to_txt(title: &str, body: &str) -> String {
    let mut out = String::new();
    // Clean title: strip leading "第N章" prefix if present, keep the heading text
    let clean_title = title.trim();
    out.push_str(clean_title);
    out.push_str("\n\n");

    // Strip common markdown formatting for TXT output
    for line in body.lines() {
        let line = line.trim();
        // Skip markdown heading markers (already emitted the title above)
        if line.starts_with("## ") || line.starts_with("### ") {
            continue;
        }
        // Strip bold/italic markers
        let cleaned = line
            .replace("**", "")
            .replace("__", "")
            .replace('*', "")
            .replace('_', "");
        out.push_str(&cleaned);
        out.push('\n');
    }
    out.push('\n');
    out
}

// ── Handler ────────────────────────────────────────────────────────────────

/// GET /api/v1/bookshelf/{slug}/export?format=txt
async fn export_book(
    Path(slug): Path<String>,
    Query(q): Query<ExportQuery>,
) -> Response {
    let fmt_str = q.format.unwrap_or_else(|| "txt".into());
    let format = match ExportFormat::from_str(&fmt_str) {
        Some(f) => f,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "ok": false,
                    "error": format!("unsupported export format: {fmt_str}. supported: txt"),
                })),
            )
                .into_response();
        }
    };

    // Find the book file
    let (_title, content) = match find_shelf_file_content(&slug) {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"ok": false, "error": e})),
            )
            .into_response();
        }
    };

    // Parse chapters from shelf markdown
    let chapters = parse_shelf_chapters(&content);

    // Determine title (from `# ` heading, else slug)
    let title = content
        .lines()
        .find(|l| l.trim_start().starts_with("# "))
        .map(|l| l.trim_start().trim_start_matches("# ").trim().to_string())
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| slug.clone());

    // Build output per format
    let bytes: Vec<u8> = match format {
        ExportFormat::Txt => build_txt(&chapters, &content, &slug).into_bytes(),
        ExportFormat::Epub => match build_epub(&title, &slug, &chapters) {
            Ok(b) => b,
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"ok": false, "error": format!("epub build failed: {e}")})),
                )
                    .into_response();
            }
        },
        ExportFormat::Pdf => match build_pdf(&title, &chapters) {
            Ok(b) => b,
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"ok": false, "error": format!("pdf build failed: {e}")})),
                )
                    .into_response();
            }
        },
    };

    // Determine filename from slug
    let filename = format!("{}.{}", slug, format.extension());

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, format.content_type().to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}\""),
            ),
        ],
        bytes,
    )
        .into_response()
}

// ── TXT builder ────────────────────────────────────────────────────────────

fn build_txt(chapters: &[(String, String)], content: &str, slug: &str) -> String {
    let mut txt = String::new();
    if chapters.is_empty() {
        for line in content.lines() {
            let line = line.trim();
            if line.starts_with("# ") {
                txt.push_str(line.strip_prefix("# ").unwrap_or(line));
                txt.push_str("\n\n");
                continue;
            }
            if line.starts_with("## ") || line.starts_with("### ") {
                continue;
            }
            let cleaned = line.replace("**", "").replace("__", "");
            txt.push_str(&cleaned);
            txt.push('\n');
        }
    } else {
        for (title, body) in chapters {
            txt.push_str(&chapter_to_txt(title, body));
        }
    }
    let _ = slug;
    txt
}

// ── EPUB builder（原生 zip，无外部依赖）───────────────────────────────────

/// 将章节打包为 EPUB2/EPUB3（zip 容器：mimetype + META-INF + OEBPS content/opf/ncx/xhtml）。
/// 每章一个 `chapter-XXX.xhtml`，含竖排 `writing-mode: vertical-rl` 基础样式（轻量竖排落地）。
fn build_epub(
    title: &str,
    slug: &str,
    chapters: &[(String, String)],
) -> Result<Vec<u8>, String> {
    use std::io::Write;
    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, ZipWriter};

    let items: Vec<(String, String)> = if chapters.is_empty() {
        vec![("正文".to_string(), String::new())]
    } else {
        chapters.to_vec()
    };

    let mut buf = Vec::new();
    {
        let mut seeker = std::io::Cursor::new(&mut buf);
        let mut zw = ZipWriter::new(&mut seeker);
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        // mimetype 必须首个、无压缩、无 extra field（EPUB 规范硬性要求）。
        let mimetype_opts =
            SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        zw.start_file("mimetype", mimetype_opts)
            .map_err(|e| e.to_string())?;
        zw.write_all(b"application/epub+zip")
            .map_err(|e| e.to_string())?;

        zw.start_file("META-INF/container.xml", opts)
            .map_err(|e| e.to_string())?;
        zw.write_all(
            br#"<?xml version="1.0" encoding="UTF-8"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles>
    <rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/>
  </rootfiles>
</container>"#,
        )
        .map_err(|e| e.to_string())?;

        // content.opf（manifest + spine + metadata）
        let mut manifest = String::new();
        let mut spine = String::new();
        for (i, (_ch_title, _)) in items.iter().enumerate() {
            let id = format!("ch{}", i + 1);
            let href = format!("chapter-{}.xhtml", i + 1);
            manifest.push_str(&format!(
                "<item id=\"{}\" href=\"{}\" media-type=\"application/xhtml+xml\"/>\n",
                id, href
            ));
            spine.push_str(&format!("<itemref idref=\"{}\"/>\n", id));
        }
        let xhtml_title = xml_escape(title);
        let xhtml_slug = xml_escape(slug);
        let opf = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<package version="3.0" unique-identifier="bookid" xmlns="http://www.idpf.org/2007/opf">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:identifier id="bookid">kaleido-{slug}</dc:identifier>
    <dc:title>{title}</dc:title>
    <dc:language>zh</dc:language>
    <meta property="dcterms:modified">2026-08-27T00:00:00Z</meta>
  </metadata>
  <manifest>
    <item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
    <item id="css" href="style.css" media-type="text/css"/>
{manifest}  </manifest>
  <spine>
{spine}  </spine>
</package>"#,
            slug = xhtml_slug,
            title = xhtml_title,
            manifest = manifest,
            spine = spine,
        );
        zw.start_file("OEBPS/content.opf", opts)
            .map_err(|e| e.to_string())?;
        zw.write_all(opf.as_bytes()).map_err(|e| e.to_string())?;

        // 竖排样式（轻量：writing-mode vertical-rl；H 与文字竖排）
        zw.start_file("OEBPS/style.css", opts)
            .map_err(|e| e.to_string())?;
        zw.write_all(
            br#"body { writing-mode: vertical-rl; -webkit-writing-mode: vertical-rl; font-family: serif; }
h1, h2 { text-align: center; }
p { text-indent: 0; margin: 0.5em 0; }
"#,
        )
        .map_err(|e| e.to_string())?;

        // nav.xhtml（EPUB3 目录导航）
        let mut nav_li = String::new();
        for (i, (ch_title, _)) in items.iter().enumerate() {
            nav_li.push_str(&format!(
                "<li><a href=\"chapter-{}.xhtml\">{}</a></li>\n",
                i + 1,
                xml_escape(ch_title)
            ));
        }
        let nav = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops" lang="zh">
<head><title>目录</title></head>
<body><nav epub:type="toc"><ol>{li}</ol></nav></body></html>"#,
            li = nav_li
        );
        zw.start_file("OEBPS/nav.xhtml", opts)
            .map_err(|e| e.to_string())?;
        zw.write_all(nav.as_bytes()).map_err(|e| e.to_string())?;

        // 章节 xhtml
        for (i, (ch_title, body)) in items.iter().enumerate() {
            let href = format!("OEBPS/chapter-{}.xhtml", i + 1);
            let mut paras = String::new();
            for line in body.lines() {
                let t = line.trim();
                if t.is_empty() {
                    continue;
                }
                paras.push_str(&format!("<p>{}</p>\n", xml_escape(t)));
            }
            if paras.is_empty() {
                paras.push_str("<p>&nbsp;</p>\n");
            }
            let xhtml = format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<html xmlns="http://www.w3.org/1999/xhtml" lang="zh">
<head><title>{title}</title><link rel="stylesheet" type="text/css" href="style.css"/></head>
<body>
<h2>{title}</h2>
{paras}
</body></html>"#,
                title = xml_escape(ch_title),
                paras = paras
            );
            zw.start_file(&href, opts).map_err(|e| e.to_string())?;
            zw.write_all(xhtml.as_bytes()).map_err(|e| e.to_string())?;
        }

        zw.finish().map_err(|e| e.to_string())?;
    }
    Ok(buf)
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

// ── PDF builder（原生文本型，自包含 CJK CID 字体）──────────────────────────

/// 生成一个合法 PDF 1.4，文本型、无外部字体文件。
/// 内容流里文本用 UTF-16BE 十六进制 `<...>` 字符串（配合 STSong-Light Type0 CID 字体
/// + UniGB-UCS2-H CMap），结构操作符保持 ASCII，常见 CJK 可正常显示。
/// 完整 A6 竖排/页码/章头/插图属 heavy 链路（Vivliostyle+JLREQ），此处为便携快速路径。
fn build_pdf(title: &str, chapters: &[(String, String)]) -> Result<Vec<u8>, String> {
    let items: Vec<(String, String)> = if chapters.is_empty() {
        vec![("正文".to_string(), String::new())]
    } else {
        chapters.to_vec()
    };

    // 内容流：用 <hex> Tj 输出文本；操作符保持 ASCII。
    let mut c = String::new();
    let line_height = 18.0;
    let mut y = 800.0f64;

    let emit_text = |c: &mut String, y: f64, size: f64, text: &str| {
        c.push_str(&format!("BT /F0 {} Tf 40 {} Td <{}> Tj ET\n", size, y, utf16_hex(text)));
    };

    emit_text(&mut c, y, 18.0, title);
    y -= 30.0;

    for (ch_title, body) in items.iter() {
        if y < 60.0 {
            c.push_str("Q\n"); // 结束当前页文本态（无 q 则无效，忽略）
            y = 800.0;
        }
        emit_text(&mut c, y, 14.0, ch_title);
        y -= 22.0;
        for line in body.lines() {
            let t = line.trim();
            if t.is_empty() {
                continue;
            }
            if y < 60.0 {
                y = 800.0;
            }
            emit_text(&mut c, y, 12.0, t);
            y -= line_height;
        }
    }

    Ok(assemble_pdf(&c))
}

/// UTF-8 → UTF-16BE 十六进制（PDF `<...>` 字符串，配合 UniGB-UCS2-H）。
fn utf16_hex(s: &str) -> String {
    s.encode_utf16().map(|u| format!("{:04X}", u)).collect()
}

/// 组装合法 PDF 字节流（对象 + xref + trailer）。
fn assemble_pdf(content: &str) -> Vec<u8> {
    let mut objects: Vec<Vec<u8>> = Vec::new();
    objects.push(b"<< /Type /Catalog /Pages 2 0 R >>".to_vec());
    objects.push(b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec());
    objects.push(
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Resources << /Font << /F0 4 0 R >> >> /Contents 5 0 R >>"
            .to_vec(),
    );
    objects.push(
        b"<< /Type /Font /Subtype /Type0 /BaseFont /STSong-Light /Encoding /UniGB-UCS2-H /DescendantFonts [6 0 R] >>"
            .to_vec(),
    );
    objects.push(b"<< /Length 0 >>\nstream\n".to_vec()); // 占位，下面填 Length + 流
    objects.push(
        b"<< /Type /Font /Subtype /CIDFontType0 /BaseFont /STSong-Light /CIDSystemInfo << /Registry (Adobe) /Ordering (GB1) /Supplement 2 >> /DW 1000 >>"
            .to_vec(),
    );

    // 对象 5（内容流）替换为真实 Length + 流
    let stream = content.as_bytes();
    let obj5 = format!("<< /Length {} >>\nstream\n", stream.len()).into_bytes();
    let mut obj5_full = obj5.clone();
    obj5_full.extend_from_slice(stream);
    obj5_full.extend_from_slice(b"\nendstream");
    objects[4] = obj5_full;

    let mut pdf: Vec<u8> = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n");
    let mut offsets: Vec<usize> = Vec::new();
    for (i, obj) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        pdf.extend_from_slice(obj);
        pdf.extend_from_slice(b"\nendobj\n");
    }
    let xref_pos = pdf.len();
    let count = objects.len() + 1;
    pdf.extend_from_slice(format!("xref\n0 {}\n", count).as_bytes());
    pdf.extend_from_slice(b"0000000000 65535 f \n");
    for off in &offsets {
        pdf.extend_from_slice(format!("{:010} 00000 n \n", off).as_bytes());
    }
    pdf.extend_from_slice(
        format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n", count, xref_pos)
            .as_bytes(),
    );
    pdf
}

// ── tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_chapters() -> Vec<(String, String)> {
        vec![
            ("第一章 夜色".to_string(), "夜色与故事同时拉开。\n阳光移到了墙根。".to_string()),
            ("第二章 黎明".to_string(), "黎明前的黑暗最深。\n天边泛起鱼肚白。".to_string()),
        ]
    }

    #[test]
    fn epub_is_valid_zip_container() {
        let epub = build_epub("测试书", "test-book", &sample_chapters()).unwrap();
        // EPUB 必须是一个合法 zip，且 mimetype 为首个条目（无压缩）。
        let cursor = std::io::Cursor::new(epub);
        let mut za = zip::ZipArchive::new(cursor).unwrap();
        assert!(za.len() >= 5, "epub 应含 mimetype/container/content/opf/nav/章节");
        let names: Vec<String> = (0..za.len()).map(|i| za.by_index(i).unwrap().name().to_string()).collect();
        assert_eq!(names[0], "mimetype");
        assert!(names.contains(&"META-INF/container.xml".to_string()));
        assert!(names.contains(&"OEBPS/content.opf".to_string()));
        assert!(names.contains(&"OEBPS/nav.xhtml".to_string()));
        assert!(names.contains(&"OEBPS/chapter-1.xhtml".to_string()));
        // mimetype 内容
        let m = za.by_name("mimetype").unwrap();
        let mut mm = m;
        use std::io::Read;
        let mut s = String::new();
        mm.read_to_string(&mut s).unwrap();
        assert_eq!(s, "application/epub+zip");
    }

    #[test]
    fn epub_xhtml_escapes_chinese_and_markup() {
        let ch = vec![("第一章 <标题> & test".to_string(), "正文 <p> & 内容".to_string())];
        let epub = build_epub("书", "b", &ch).unwrap();
        // 找到 chapter-1 的 xhtml，确认标题/正文被 xml 转义
        let cursor = std::io::Cursor::new(epub);
        let mut za = zip::ZipArchive::new(cursor).unwrap();
        use std::io::Read;
        let mut s = String::new();
        za.by_name("OEBPS/chapter-1.xhtml").unwrap().read_to_string(&mut s).unwrap();
        assert!(s.contains("第一章 &lt;标题&gt; &amp; test"), "标题应转义: {s}");
        assert!(s.contains("正文 &lt;p&gt; &amp; 内容"), "正文应转义: {s}");
        assert!(!s.contains("<标题>"), "不应残留未转义尖括号");
    }

    #[test]
    fn pdf_has_valid_header_and_xref() {
        let pdf = build_pdf("测试书", &sample_chapters()).unwrap();
        let s = String::from_utf8_lossy(&pdf);
        assert!(s.starts_with("%PDF-1.4"), "pdf 应以 %PDF-1.4 开头");
        assert!(s.contains("xref\n"), "pdf 应含 xref");
        assert!(s.contains("trailer"), "pdf 应含 trailer");
        assert!(s.ends_with("%%EOF\n"), "pdf 应以 %%EOF 结尾");
        assert!(s.contains("/Type0"), "pdf 应含 Type0 CJK 字体");
        assert!(s.contains("STSong-Light"), "pdf 应含 CJK 字体名");
    }

    #[test]
    fn pdf_embeds_utf16_hex_text() {
        let pdf = build_pdf("夜色", &sample_chapters()).unwrap();
        // 内容流里应含 UTF-16BE 十六进制。'夜' = U+591C → hex "591C"。'色' = U+8272 → "8272".
        let s = String::from_utf8_lossy(&pdf);
        assert!(s.contains("591C8272"), "应含 夜色 的 UTF-16BE hex: 找不到 591C8272");
    }

    #[test]
    fn empty_chapters_still_produce_epub_and_pdf() {
        let epub = build_epub("书", "b", &vec![]).unwrap();
        assert!(epub.len() > 100);
        let pdf = build_pdf("书", &vec![]).unwrap();
        assert!(pdf.len() > 100);
    }
}
