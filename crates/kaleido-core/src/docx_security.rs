//! DOCX (OOXML zip) import security (P7). Ported from Scriverse
//! `src/docx-security.ts` using the zip crate: budgeted entry/expansion caps
//! (zip-bomb protection), encrypted/zip64 rejection via the underlying crate,
//! path-safety, and mandatory OOXML parts check.

use std::collections::HashSet;
use std::io::{Cursor, Read};

use zip::ZipArchive;

pub const MAX_DOCX_ENTRIES: usize = 2000;
pub const MAX_DOCX_ENTRY_BYTES: u64 = 96 * 1024 * 1024; // 96 MiB per entry
pub const MAX_DOCX_TOTAL_BYTES: u64 = 128 * 1024 * 1024; // 128 MiB expanded
pub const MAX_DOCX_RATIO: u64 = 200;

/// Validate that `bytes` is a safe, structurally complete DOCX package.
pub fn validate_docx(bytes: &[u8]) -> Result<(), String> {
    let cursor = Cursor::new(bytes);
    let mut archive = ZipArchive::new(cursor).map_err(|e| format!("invalid docx zip: {e}"))?;
    if archive.len() == 0 {
        return Err("docx zip is empty".to_string());
    }
    if archive.len() > MAX_DOCX_ENTRIES {
        return Err(format!(
            "docx entry count {} exceeds limit {MAX_DOCX_ENTRIES}",
            archive.len()
        ));
    }
    let mut seen: HashSet<String> = HashSet::new();
    let mut total_declared: u64 = 0;
    let mut has_content_types = false;
    let mut has_root_rels = false;
    let mut has_document = false;
    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .map_err(|e| format!("docx zip entry: {e}"))?;
        let name = file.name().replace('\\', "/");
        if name.starts_with('/') || name.contains("..") || name.contains(':') {
            return Err(format!("unsafe docx entry name: {name}"));
        }
        if !seen.insert(name.clone()) {
            return Err(format!("duplicate docx entry: {name}"));
        }
        if name.ends_with('/') {
            continue;
        }
        let (declared, compressed) = (file.size(), file.compressed_size());
        if declared > MAX_DOCX_ENTRY_BYTES {
            return Err(format!(
                "docx entry {name} declared {declared} bytes exceeds limit"
            ));
        }
        total_declared = total_declared.saturating_add(declared);
        if total_declared > MAX_DOCX_TOTAL_BYTES {
            return Err("docx expanded size exceeds limit".to_string());
        }
        if declared > 0 && compressed > 0 && declared / compressed > MAX_DOCX_RATIO {
            return Err(format!("docx entry {name} compression ratio exceeds limit"));
        }
        // Actually decompress with a hard cap (belt-and-braces vs lying headers).
        let mut data = Vec::new();
        file.by_ref()
            .take(MAX_DOCX_ENTRY_BYTES + 1)
            .read_to_end(&mut data)
            .map_err(|e| format!("docx zip read: {e}"))?;
        if data.len() as u64 > MAX_DOCX_ENTRY_BYTES {
            return Err(format!("docx entry {name} expanded beyond limit"));
        }
        match name.as_str() {
            "[Content_Types].xml" => has_content_types = true,
            "_rels/.rels" => has_root_rels = true,
            "word/document.xml" => has_document = true,
            _ => {}
        }
    }
    if !(has_content_types && has_root_rels && has_document) {
        return Err("docx missing required parts ([Content_Types].xml / _rels/.rels / word/document.xml)".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, ZipWriter};

    fn build_docx(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut zw = ZipWriter::new(Cursor::new(&mut buf));
            let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
            for (name, data) in entries {
                zw.start_file(*name, opts).unwrap();
                zw.write_all(data).unwrap();
            }
            zw.finish().unwrap();
        }
        buf
    }

    fn minimal_docx() -> Vec<u8> {
        build_docx(&[
            ("[Content_Types].xml", br#"<?xml version="1.0"?><Types/>"#),
            ("_rels/.rels", br#"<?xml version="1.0"?><Relationships/>"#),
            ("word/document.xml", br#"<w:document/>"#),
            ("word/styles.xml", b"<w:styles/>"),
        ])
    }

    #[test]
    fn valid_docx_passes() {
        assert!(validate_docx(&minimal_docx()).is_ok());
    }

    #[test]
    fn missing_parts_rejected() {
        let bad = build_docx(&[
            ("[Content_Types].xml", b"x"),
            ("_rels/.rels", b"x"),
            // no word/document.xml
        ]);
        assert!(validate_docx(&bad).is_err());
    }

    #[test]
    fn not_a_zip_rejected() {
        assert!(validate_docx(b"PK\x03\x04 this is not really a zip").is_err());
        assert!(validate_docx(b"hello world").is_err());
    }

    #[test]
    fn path_traversal_rejected() {
        let bad = build_docx(&[
            ("[Content_Types].xml", b"x"),
            ("_rels/.rels", b"x"),
            ("../../etc/passwd", b"evil"),
            ("word/document.xml", b"x"),
        ]);
        assert!(validate_docx(&bad).is_err());
    }

    #[test]
    fn oversized_entry_rejected() {
        // 97 MiB of zeroes compressed well under 96 MiB? It stays small via
        // deflate, but declared size is what we check first — zip writer will
        // write real uncompressed size 97 MiB > cap.
        let big = vec![0u8; (MAX_DOCX_ENTRY_BYTES as usize) + 1];
        let bad = build_docx(&[
            ("[Content_Types].xml", b"x"),
            ("_rels/.rels", b"x"),
            ("word/document.xml", &big),
        ]);
        assert!(validate_docx(&bad).is_err());
    }
}
