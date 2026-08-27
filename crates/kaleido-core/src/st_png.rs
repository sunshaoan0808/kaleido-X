//! SillyTavern PNG character cards: tEXt `chara` / `ccv3` embed + extract.
//!
//! Spec:
//! - V2: tEXt keyword `chara`, value = base64(utf-8 JSON of chara_card_v2)
//! - V3: tEXt keyword `ccv3`, value = base64(utf-8 JSON of chara_card_v3)
//!
//! Pure-Rust minimal PNG chunk IO (no image crate). Export builds a 1×1 RGB
//! placeholder PNG when no avatar bytes are provided.

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use serde_json::Value;

use crate::{parse_st_character_card_value, StCardData, StImportError};

const PNG_SIG: &[u8] = b"\x89PNG\r\n\x1a\n";

/// CRC-32 (ISO 3309 / PNG polynomial).
fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xffff_ffff;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            let mask = if crc & 1 != 0 { 0xffff_ffff } else { 0 };
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

fn write_chunk(out: &mut Vec<u8>, typ: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(typ);
    out.extend_from_slice(data);
    let mut for_crc = Vec::with_capacity(4 + data.len());
    for_crc.extend_from_slice(typ);
    for_crc.extend_from_slice(data);
    out.extend_from_slice(&crc32(&for_crc).to_be_bytes());
}

/// Minimal valid 1×1 RGB (8-bit) PNG — solid mauve pixel.
/// Built offline once; used as default avatar shell for export.
fn minimal_rgb_png() -> Vec<u8> {
    // Precomputed 1x1 RGB PNG (filter=None, pixel=0xFF 0x00 0x80)
    // generated via zlib+crc offline; re-verify in tests.
    let mut out = Vec::with_capacity(80);
    out.extend_from_slice(PNG_SIG);
    // IHDR: width=1 height=1 bit_depth=8 color_type=2(RGB) compression=0 filter=0 interlace=0
    let mut ihdr = [0u8; 13];
    ihdr[0..4].copy_from_slice(&1u32.to_be_bytes());
    ihdr[4..8].copy_from_slice(&1u32.to_be_bytes());
    ihdr[8] = 8;
    ihdr[9] = 2;
    write_chunk(&mut out, b"IHDR", &ihdr);
    // IDAT: zlib of filter_byte + R G B
    // raw scanline: 00 FF 00 80
    // zlib-compressed (stored/deflate default from a known good blob)
    // We synthesize via raw DEFLATE store block to avoid flate2 dep.
    let scan = [0u8, 0xff, 0x00, 0x80];
    let idat = zlib_store(&scan);
    write_chunk(&mut out, b"IDAT", &idat);
    write_chunk(&mut out, b"IEND", &[]);
    out
}

/// zlib wrapper around a single stored (uncompressed) DEFLATE block.
fn zlib_store(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len() + 12);
    // CMF/FLG: CM=8, CINFO=7 → 0x78; FLG check so (CMF*256+FLG) % 31 == 0 → 0x01
    out.push(0x78);
    out.push(0x01);
    // stored block: BFINAL=1, BTYPE=00
    out.push(0x01);
    let n = raw.len() as u16;
    out.extend_from_slice(&n.to_le_bytes());
    out.extend_from_slice((!n).to_le_bytes().as_ref());
    out.extend_from_slice(raw);
    // adler32 of raw
    out.extend_from_slice(&adler32(raw).to_be_bytes());
    out
}

fn adler32(data: &[u8]) -> u32 {
    let mut a: u32 = 1;
    let mut b: u32 = 0;
    for &x in data {
        a = (a + u32::from(x)) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

fn make_text_chunk_data(keyword: &str, text: &str) -> Vec<u8> {
    let mut d = Vec::with_capacity(keyword.len() + 1 + text.len());
    d.extend_from_slice(keyword.as_bytes());
    d.push(0);
    d.extend_from_slice(text.as_bytes());
    d
}

/// Insert / replace tEXt chunks for `chara` (and optional `ccv3`) before IEND.
/// If `base_png` is empty / invalid, falls back to a 1×1 placeholder.
pub fn embed_st_card_in_png(
    card_json: &Value,
    base_png: Option<&[u8]>,
) -> Result<Vec<u8>, StImportError> {
    let payload = serde_json::to_vec(card_json)
        .map_err(|e| StImportError(format!("serialize card: {e}")))?;
    let b64 = B64.encode(&payload);

    let base = match base_png {
        Some(b) if b.starts_with(PNG_SIG) => b.to_vec(),
        _ => minimal_rgb_png(),
    };

    let mut out = Vec::with_capacity(base.len() + b64.len() + 64);
    out.extend_from_slice(PNG_SIG);

    let mut pos = PNG_SIG.len();
    let mut saw_iend = false;
    while pos + 12 <= base.len() {
        let len = u32::from_be_bytes(base[pos..pos + 4].try_into().unwrap()) as usize;
        let typ = &base[pos + 4..pos + 8];
        let data_end = pos + 8 + len;
        let chunk_end = data_end + 4;
        if chunk_end > base.len() {
            return Err(StImportError("truncated PNG chunk".into()));
        }
        // Skip existing chara / ccv3 tEXt so we replace them
        if typ == b"tEXt" {
            let data = &base[pos + 8..data_end];
            if let Some(nul) = data.iter().position(|&c| c == 0) {
                let key = std::str::from_utf8(&data[..nul]).unwrap_or("");
                if key == "chara" || key == "ccv3" {
                    pos = chunk_end;
                    continue;
                }
            }
        }
        if typ == b"IEND" {
            // inject chara (always V2 envelope is what ST reads first)
            let text_data = make_text_chunk_data("chara", &b64);
            write_chunk(&mut out, b"tEXt", &text_data);
            // if card is v3, also write ccv3
            let is_v3 = card_json
                .get("spec")
                .and_then(|s| s.as_str())
                .map(|s| s.contains('3'))
                .unwrap_or(false);
            if is_v3 {
                let text_data = make_text_chunk_data("ccv3", &b64);
                write_chunk(&mut out, b"tEXt", &text_data);
            }
            // then original IEND
            out.extend_from_slice(&base[pos..chunk_end]);
            saw_iend = true;
            break;
        }
        out.extend_from_slice(&base[pos..chunk_end]);
        pos = chunk_end;
    }
    if !saw_iend {
        let text_data = make_text_chunk_data("chara", &b64);
        write_chunk(&mut out, b"tEXt", &text_data);
        write_chunk(&mut out, b"IEND", &[]);
    }
    Ok(out)
}

/// Extract ST card from PNG tEXt `chara` or `ccv3` (base64 JSON).
pub fn extract_st_card_from_png(bytes: &[u8]) -> Result<StCardData, StImportError> {
    if !bytes.starts_with(PNG_SIG) {
        return Err(StImportError("not a PNG (bad signature)".into()));
    }
    let mut chara: Option<String> = None;
    let mut ccv3: Option<String> = None;
    let mut pos = PNG_SIG.len();
    while pos + 12 <= bytes.len() {
        let len = u32::from_be_bytes(bytes[pos..pos + 4].try_into().unwrap()) as usize;
        let typ = &bytes[pos + 4..pos + 8];
        let data_end = pos + 8 + len;
        let chunk_end = data_end + 4;
        if chunk_end > bytes.len() {
            return Err(StImportError("truncated PNG chunk".into()));
        }
        if typ == b"tEXt" {
            let data = &bytes[pos + 8..data_end];
            if let Some(nul) = data.iter().position(|&c| c == 0) {
                let key = std::str::from_utf8(&data[..nul]).unwrap_or("");
                let val = String::from_utf8_lossy(&data[nul + 1..]).into_owned();
                if key == "chara" {
                    chara = Some(val);
                } else if key == "ccv3" {
                    ccv3 = Some(val);
                }
            }
        }
        if typ == b"IEND" {
            break;
        }
        pos = chunk_end;
    }

    // Prefer ccv3 if present, else chara
    let b64 = ccv3.or(chara).ok_or_else(|| {
        StImportError("PNG has no tEXt chara/ccv3 chunk (not an ST character card)".into())
    })?;
    let cleaned: String = b64.chars().filter(|c| !c.is_whitespace()).collect();
    let decoded = B64
        .decode(cleaned.as_bytes())
        .map_err(|e| StImportError(format!("base64 decode chara: {e}")))?;
    let text = String::from_utf8(decoded)
        .map_err(|e| StImportError(format!("chara payload not utf-8: {e}")))?;
    let value: Value = serde_json::from_str(&text)
        .map_err(|e| StImportError(format!("chara JSON parse: {e}")))?;
    parse_st_character_card_value(&value)
}

/// Base64-encode PNG bytes for JSON transport (data URL without prefix).
pub fn png_to_base64(png: &[u8]) -> String {
    B64.encode(png)
}

/// Decode base64 PNG (optional `data:image/png;base64,` prefix).
pub fn base64_to_png(s: &str) -> Result<Vec<u8>, StImportError> {
    let s = s.trim();
    let raw = if let Some(i) = s.find("base64,") {
        &s[i + 7..]
    } else {
        s
    };
    let cleaned: String = raw.chars().filter(|c| !c.is_whitespace()).collect();
    B64.decode(cleaned.as_bytes())
        .map_err(|e| StImportError(format!("base64 decode png: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn roundtrip_embed_extract() {
        let card = json!({
            "spec": "chara_card_v2",
            "spec_version": "2.0",
            "data": {
                "name": "PNG Roundtrip",
                "description": "d",
                "personality": "p",
                "scenario": "s",
                "first_mes": "hi",
                "mes_example": "",
                "tags": ["t"],
                "creator": "test",
                "character_version": "1.0"
            }
        });
        let png = embed_st_card_in_png(&card, None).unwrap();
        assert!(png.starts_with(PNG_SIG));
        let parsed = extract_st_card_from_png(&png).unwrap();
        assert_eq!(parsed.name, "PNG Roundtrip");
        assert_eq!(parsed.personality, "p");
    }

    #[test]
    fn reject_non_png() {
        let err = extract_st_card_from_png(b"not-a-png").unwrap_err();
        assert!(err.0.contains("PNG") || err.0.contains("signature"));
    }

    #[test]
    fn minimal_png_valid_sig() {
        let p = minimal_rgb_png();
        assert!(p.starts_with(PNG_SIG));
        assert!(p.len() > 40);
    }
}
