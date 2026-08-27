//! SillyTavern WEBP / JPEG character card extraction (pure Rust, no image crate).
//!
//! Card data lives in image metadata containers:
//! - WEBP: RIFF container with an `EXIF` chunk or an `XMP ` chunk holding the
//!   base64(JSON) `ccv3` / `chara` payload (吞噬自 tavern-card-distiller extract_card.py).
//! - JPEG: APP1 segment (`FFE1`) carrying `Exif\0\0` or an XMP packet
//!   (`http://ns.adobe.com/xap/1.0/`).
//!
//! Both formats fall back to a raw-byte marker scan for `ccv3` / `chara`
//! (matching distiller `_search_raw_for_card`). Output reuses the existing
//! `parse_st_character_card_value` (V1/V2/V3 normalization).

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use serde_json::Value;

use crate::{parse_st_character_card_value, StCardData, StImportError};

const RIFF_SIG: &[u8] = b"RIFF";
const WEBP_SIG: &[u8] = b"WEBP";
const JPEG_SIG: &[u8] = b"\xff\xd8";
const EXIF_HEADER: &[u8] = b"Exif\0\0";
const XMP_HEADER: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";

/// Decode a payload that may be base64(JSON) or raw JSON text.
fn parse_b64_or_json(s: &str) -> Result<Value, StImportError> {
    let cleaned: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    if let Ok(raw) = B64.decode(cleaned.as_bytes()) {
        if let Ok(text) = String::from_utf8(raw) {
            if let Ok(v) = serde_json::from_str::<Value>(&text) {
                return Ok(v);
            }
        }
    }
    serde_json::from_str(s)
        .map_err(|e| StImportError(format!("card payload neither base64 JSON nor direct JSON: {e}")))
}

fn is_b64_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=' | b'\n' | b'\r')
}

/// Scan raw bytes for `ccv3` / `chara` markers followed by base64 JSON
/// (吞噬自 tavern-card-distiller extract_card.py `_search_raw_for_card`).
fn search_raw_for_card(data: &[u8]) -> Option<Value> {
    for marker in [
        b"ccv3\0".as_slice(),
        b"chara\0".as_slice(),
        b"ccv3:".as_slice(),
        b"chara:".as_slice(),
    ] {
        let mut pos = 0usize;
        while pos + marker.len() <= data.len() {
            let Some(rel) = data[pos..]
                .windows(marker.len())
                .position(|w| w == marker)
            else {
                break;
            };
            let mut start = pos + rel + marker.len();
            while start < data.len() && matches!(data[start], b'\0' | b' ' | b'\t' | b'\n' | b'\r')
            {
                start += 1;
            }
            let mut end = start;
            while end < data.len() && is_b64_char(data[end]) {
                end += 1;
            }
            if end - start > 100 {
                if let Ok(b64) = std::str::from_utf8(&data[start..end]) {
                    if let Ok(v) = parse_b64_or_json(b64) {
                        return Some(v);
                    }
                }
            }
            pos = end.max(start);
        }
    }
    None
}

/// Minimal TIFF/EXIF IFD reader: pulls `UserComment` (0x9286) and
/// `ImageDescription` (0x010E) as strings. Supports both byte orders.
fn parse_tiff_strings(exif: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let tiff = if let Some(stripped) = exif.strip_prefix(EXIF_HEADER) {
        stripped
    } else {
        exif
    };
    if tiff.len() < 8 {
        return out;
    }
    let le = match &tiff[0..4] {
        b"II\x2a\0" => true,
        b"MM\0\x2a" => false,
        _ => return out,
    };
    let u16at = |b: &[u8], o: usize| -> Option<u16> {
        let s = b.get(o..o + 2)?;
        Some(if le {
            u16::from_le_bytes([s[0], s[1]])
        } else {
            u16::from_be_bytes([s[0], s[1]])
        })
    };
    let u32at = |b: &[u8], o: usize| -> Option<u32> {
        let s = b.get(o..o + 4)?;
        Some(if le {
            u32::from_le_bytes([s[0], s[1], s[2], s[3]])
        } else {
            u32::from_be_bytes([s[0], s[1], s[2], s[3]])
        })
    };
    let Some(ifd0) = u32at(tiff, 4) else {
        return out;
    };
    let Some(count) = u16at(tiff, ifd0 as usize) else {
        return out;
    };
    for i in 0..count as usize {
        let e = ifd0 as usize + 2 + i * 12;
        let (Some(tag), Some(typ), Some(cnt)) =
            (u16at(tiff, e), u16at(tiff, e + 2), u32at(tiff, e + 4))
        else {
            continue;
        };
        if tag != 0x9286 && tag != 0x010E {
            continue;
        }
        let size: usize = match typ {
            3 => 2,
            4 => 4,
            _ => 1,
        };
        let byte_len = size.saturating_mul(cnt as usize);
        let src = if byte_len <= 4 {
            tiff.get(e + 8..e + 8 + byte_len)
        } else {
            u32at(tiff, e + 8)
                .and_then(|off| tiff.get(off as usize..off as usize + byte_len))
        };
        let Some(mut s) = src.map(|b| b.to_vec()) else {
            continue;
        };
        // EXIF 2.3 UserComment: optional "ASCII\0\0\0" charset prefix
        if tag == 0x9286 && s.starts_with(b"ASCII\0\0\0") {
            s.drain(..8);
        }
        if let Some(nul) = s.iter().position(|&b| b == 0) {
            s.truncate(nul);
        }
        if let Ok(st) = String::from_utf8(s) {
            if !st.trim().is_empty() {
                out.push(st);
            }
        }
    }
    out
}

/// Try EXIF blob: raw marker scan first, then TIFF tag strings.
fn decode_from_exif_data(data: &[u8]) -> Option<Value> {
    if let Some(v) = search_raw_for_card(data) {
        return Some(v);
    }
    for s in parse_tiff_strings(data) {
        if let Ok(v) = parse_b64_or_json(&s) {
            return Some(v);
        }
    }
    None
}

/// Content between an XML open/close tag pair `<...tag...>…</`.
fn xml_tag_value(xml: &str, tag: &str) -> Option<String> {
    let open = xml.find(tag)?;
    let after = &xml[open + tag.len()..];
    let gt = after.find('>')?;
    let start = open + tag.len() + gt + 1;
    let rest = &xml[start..];
    let end = rest.find('<')?;
    Some(rest[..end].trim().to_string())
}

/// Extract card base64 from an XMP packet (V3 `ccv3:chara_card_v3` wins, then
/// V2 `chara:chara_card_v2`; 吞噬自 tavern-card-distiller extract_card.py).
fn decode_from_xmp(data: &[u8]) -> Option<Value> {
    let xml = std::str::from_utf8(data).ok()?;
    for tag in [
        "ccv3:chara_card_v3",
        "chara:chara_card_v2",
        "chara_card_v3",
        "chara_card_v2",
    ] {
        if let Some(raw) = xml_tag_value(xml, tag) {
            if !raw.is_empty() {
                if let Ok(v) = parse_b64_or_json(&raw) {
                    return Some(v);
                }
            }
        }
    }
    None
}

/// Extract ST card from a WEBP file (RIFF `EXIF` / `XMP ` chunk).
pub fn extract_st_card_from_webp(bytes: &[u8]) -> Result<StCardData, StImportError> {
    if bytes.len() < 12 || &bytes[0..4] != RIFF_SIG || &bytes[8..12] != WEBP_SIG {
        return Err(StImportError("not a WEBP (bad RIFF/WEBP signature)".into()));
    }
    let mut pos = 12usize;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let len = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let data_start = pos + 8;
        let data_end = data_start + len;
        let chunk_end = data_end + (len & 1); // RIFF pads odd-length chunks
        if chunk_end > bytes.len() {
            return Err(StImportError("truncated WEBP chunk".into()));
        }
        let data = &bytes[data_start..data_end];
        let card = if id == b"EXIF" {
            decode_from_exif_data(data)
        } else if id == b"XMP " {
            decode_from_xmp(data)
        } else {
            None
        };
        if let Some(v) = card {
            return parse_st_character_card_value(&v);
        }
        pos = chunk_end;
    }
    if let Some(v) = search_raw_for_card(bytes) {
        return parse_st_character_card_value(&v);
    }
    Err(StImportError(
        "WEBP has no EXIF/XMP card data (not an ST character card)".into(),
    ))
}

/// Extract ST card from a JPEG file (APP1 `Exif\0\0` or XMP segment).
pub fn extract_st_card_from_jpeg(bytes: &[u8]) -> Result<StCardData, StImportError> {
    if !bytes.starts_with(JPEG_SIG) {
        return Err(StImportError("not a JPEG (bad FFD8 signature)".into()));
    }
    let mut pos = 2usize; // skip SOI
    while pos + 4 <= bytes.len() {
        if bytes[pos] != 0xFF {
            return Err(StImportError("invalid JPEG marker".into()));
        }
        let marker = bytes[pos + 1];
        if marker == 0xD9 {
            break; // EOI
        }
        if marker == 0xD8 || (0xD0..=0xD7).contains(&marker) || marker == 0x01 || marker == 0xFF {
            pos += 2;
            continue;
        }
        if pos + 4 > bytes.len() {
            return Err(StImportError("truncated JPEG marker".into()));
        }
        let seg_len = u16::from_be_bytes([bytes[pos + 2], bytes[pos + 3]]) as usize;
        if seg_len < 2 || pos + 2 + seg_len > bytes.len() {
            return Err(StImportError("truncated JPEG segment".into()));
        }
        let seg_data = &bytes[pos + 4..pos + 2 + seg_len];
        let card = if marker == 0xE1 {
            if seg_data.starts_with(EXIF_HEADER) {
                decode_from_exif_data(seg_data)
            } else if seg_data.starts_with(XMP_HEADER) {
                decode_from_xmp(seg_data)
            } else {
                None
            }
        } else {
            None
        };
        if let Some(v) = card {
            return parse_st_character_card_value(&v);
        }
        pos += 2 + seg_len;
    }
    if let Some(v) = search_raw_for_card(bytes) {
        return parse_st_character_card_value(&v);
    }
    Err(StImportError(
        "JPEG has no EXIF/XMP card data (not an ST character card)".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CARD_V3: &str = r#"{
      "spec": "chara_card_v3",
      "spec_version": "3.0",
      "data": {
        "name": "Webp Mage",
        "description": "A mage living inside metadata.",
        "personality": "Crisp",
        "first_mes": "*waves*",
        "creator": "x6-fixture"
      }
    }"#;

    fn b64_card() -> String {
        B64.encode(CARD_V3.as_bytes())
    }

    /// RIFF/WEBP container with a single chunk of the given id.
    fn build_webp(chunk_id: &[u8; 4], chunk_data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(RIFF_SIG);
        out.extend_from_slice(&0u32.to_le_bytes()); // patched below
        out.extend_from_slice(WEBP_SIG);
        out.extend_from_slice(chunk_id);
        out.extend_from_slice(&(chunk_data.len() as u32).to_le_bytes());
        out.extend_from_slice(chunk_data);
        if chunk_data.len() % 2 == 1 {
            out.push(0);
        }
        let total = out.len() - 8;
        out[4..8].copy_from_slice(&(total as u32).to_le_bytes());
        out
    }

    fn build_xmp(b64: &str) -> Vec<u8> {
        format!(
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:ccv3="http://localhost/ccv3"><ccv3:chara_card_v3>{b64}</ccv3:chara_card_v3></rdf:Description></rdf:RDF></x:xmpmeta>"#
        )
        .into_bytes()
    }

    /// Minimal little-endian EXIF/TIFF with a UserComment (tag 0x9286).
    /// Returns raw TIFF payload (no `Exif\0\0` header — the JPEG APP1
    /// builder prepends EXIF_HEADER, matching real EXIF APP1 layout).
    fn build_exif_usercomment(value: &[u8]) -> Vec<u8> {
        let payload = [b"ASCII\0\0\0".as_slice(), value].concat();
        let mut tiff = Vec::new();
        tiff.extend_from_slice(b"II\x2a\0");
        tiff.extend_from_slice(&8u32.to_le_bytes()); // IFD0 offset
        tiff.extend_from_slice(&1u16.to_le_bytes()); // entry count
        tiff.extend_from_slice(&0x9286u16.to_le_bytes()); // tag UserComment
        tiff.extend_from_slice(&7u16.to_le_bytes()); // type UNDEFINED
        tiff.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        let value_off = 8 + 2 + 12 + 4; // 26
        tiff.extend_from_slice(&(value_off as u32).to_le_bytes());
        tiff.extend_from_slice(&0u32.to_le_bytes()); // next IFD
        tiff.extend_from_slice(&payload);
        tiff
    }

    /// JPEG with one APP1 segment whose payload starts with the given prefix.
    fn build_jpeg_app1(prefix: &[u8], payload: &[u8]) -> Vec<u8> {
        let mut seg = Vec::new();
        seg.extend_from_slice(prefix);
        seg.extend_from_slice(payload);
        let mut out = Vec::new();
        out.extend_from_slice(&[0xFF, 0xD8]); // SOI
        out.extend_from_slice(&[0xFF, 0xE1]); // APP1
        out.extend_from_slice(&((seg.len() + 2) as u16).to_be_bytes());
        out.extend_from_slice(&seg);
        out.extend_from_slice(&[0xFF, 0xD9]); // EOI
        out
    }

    #[test]
    fn webp_extract_from_xmp_chunk() {
        let webp = build_webp(b"XMP ", &build_xmp(&b64_card()));
        let card = extract_st_card_from_webp(&webp).unwrap();
        assert_eq!(card.name, "Webp Mage");
        assert_eq!(card.spec, "chara_card_v3");
    }

    #[test]
    fn webp_extract_from_exif_chunk() {
        let exif = build_exif_usercomment(&b64_card().into_bytes());
        let webp = build_webp(b"EXIF", &exif);
        let card = extract_st_card_from_webp(&webp).unwrap();
        assert_eq!(card.name, "Webp Mage");
    }

    #[test]
    fn webp_rejects_non_card() {
        let err = extract_st_card_from_webp(b"RIFFxxxxWEBP......").unwrap_err();
        assert!(err.0.contains("no EXIF/XMP") || err.0.contains("signature"));
        let err2 = extract_st_card_from_webp(b"not-webp").unwrap_err();
        assert!(err2.0.contains("not a WEBP"));
    }

    #[test]
    fn jpeg_extract_from_exif_app1() {
        let jpeg = build_jpeg_app1(EXIF_HEADER, &build_exif_usercomment(&b64_card().into_bytes()));
        let card = extract_st_card_from_jpeg(&jpeg).unwrap();
        assert_eq!(card.name, "Webp Mage");
        assert_eq!(card.personality, "Crisp");
    }

    #[test]
    fn jpeg_extract_from_xmp_app1() {
        let jpeg = build_jpeg_app1(XMP_HEADER, &build_xmp(&b64_card()));
        let card = extract_st_card_from_jpeg(&jpeg).unwrap();
        assert_eq!(card.name, "Webp Mage");
    }

    #[test]
    fn jpeg_rejects_non_card() {
        // valid-ish JPEG with only SOI/EOI and no card APP1
        let jpeg = [0xFF, 0xD8, 0xFF, 0xD9];
        let err = extract_st_card_from_jpeg(&jpeg).unwrap_err();
        assert!(err.0.contains("no EXIF/XMP"));
        let err2 = extract_st_card_from_jpeg(b"jpeg").unwrap_err();
        assert!(err2.0.contains("not a JPEG"));
    }
}
