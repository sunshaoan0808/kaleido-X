//! Raster image metadata validation (P7). Ported from Scriverse
//! `src/image-metadata.ts`: pure hand-rolled parsers for PNG / JPEG / WebP
//! dimensions with strict structural checks. Purpose: reject malformed or
//! decompression-bomb images (absurd pixel dimensions) before any decode.

pub const MAX_IMAGE_PIXELS: u64 = 40_000_000; // 40 MP cap
pub const MAX_IMAGE_SIDE: u64 = 65_535;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RasterImage {
    pub mime_type: &'static str,
    pub width: u64,
    pub height: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidRasterImageError(pub String);

impl std::fmt::Display for InvalidRasterImageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

fn err<T>(msg: &str) -> Result<T, InvalidRasterImageError> {
    Err(InvalidRasterImageError(msg.to_string()))
}

fn assert_dimensions(width: u64, height: u64) -> Result<(), InvalidRasterImageError> {
    if width == 0 || height == 0 {
        return err("图片尺寸无效");
    }
    if width > MAX_IMAGE_SIDE || height > MAX_IMAGE_SIDE {
        return err("图片尺寸超过限制");
    }
    if width.saturating_mul(height) > MAX_IMAGE_PIXELS {
        return err("图片像素总数超过限制");
    }
    Ok(())
}

fn read_png(bytes: &[u8]) -> Result<RasterImage, InvalidRasterImageError> {
    if bytes.len() < 24 {
        return err("PNG 文件结构不完整");
    }
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return err("PNG 签名无效");
    }
    let width = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]) as u64;
    let height = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]) as u64;
    let mut offset = 8usize;
    let mut has_end = false;
    while offset + 12 <= bytes.len() {
        let chunk_len =
            u32::from_be_bytes([bytes[offset], bytes[offset + 1], bytes[offset + 2], bytes[offset + 3]])
                as usize;
        let chunk_end = offset + 12 + chunk_len;
        if chunk_end > bytes.len() {
            return err("PNG 文件结构不完整");
        }
        let chunk_type = &bytes[offset + 4..offset + 8];
        if chunk_type == b"IEND" {
            if chunk_len != 0 {
                return err("PNG 文件的 IEND 数据无效");
            }
            has_end = true;
            break;
        }
        offset = chunk_end;
    }
    if !has_end {
        return err("PNG 文件缺少结束标记");
    }
    assert_dimensions(width, height)?;
    Ok(RasterImage { mime_type: "image/png", width, height })
}

fn is_sof(marker: u8) -> bool {
    matches!(
        marker,
        0xc0 | 0xc1 | 0xc2 | 0xc3 | 0xc5 | 0xc6 | 0xc7 | 0xc9 | 0xca | 0xcb | 0xcd | 0xce | 0xcf
    )
}

fn read_jpeg(bytes: &[u8]) -> Result<RasterImage, InvalidRasterImageError> {
    if bytes.len() < 3 || bytes[0] != 0xff || bytes[1] != 0xd8 || bytes[2] != 0xff {
        return err("JPEG 签名无效");
    }
    if bytes.len() < 4 || bytes[bytes.len() - 2] != 0xff || bytes[bytes.len() - 1] != 0xd9 {
        return err("JPEG 文件结构不完整");
    }
    let mut offset = 2usize;
    while offset + 1 < bytes.len() {
        while offset < bytes.len() && bytes[offset] == 0xff {
            offset += 1;
        }
        if offset >= bytes.len() {
            break;
        }
        let marker = bytes[offset];
        offset += 1;
        if marker == 0xd8
            || marker == 0xd9
            || marker == 0x01
            || (0xd0..=0xd7).contains(&marker)
        {
            continue;
        }
        if offset + 2 > bytes.len() {
            break;
        }
        let segment_len = u16::from_be_bytes([bytes[offset], bytes[offset + 1]]) as usize;
        if segment_len < 2 || offset + segment_len > bytes.len() {
            return err("JPEG 文件段长度无效");
        }
        if is_sof(marker) {
            if segment_len < 7 {
                return err("JPEG 尺寸数据无效");
            }
            let height = u16::from_be_bytes([bytes[offset + 3], bytes[offset + 4]]) as u64;
            let width = u16::from_be_bytes([bytes[offset + 5], bytes[offset + 6]]) as u64;
            assert_dimensions(width, height)?;
            return Ok(RasterImage { mime_type: "image/jpeg", width, height });
        }
        if marker == 0xda {
            break;
        }
        offset += segment_len;
    }
    err("JPEG 文件缺少尺寸数据")
}

fn read_u24_le(bytes: &[u8], offset: usize) -> u64 {
    (bytes[offset] as u64) | ((bytes[offset + 1] as u64) << 8) | ((bytes[offset + 2] as u64) << 16)
}

fn read_webp(bytes: &[u8]) -> Result<RasterImage, InvalidRasterImageError> {
    if bytes.len() < 12
        || &bytes[0..4] != b"RIFF"
        || &bytes[8..12] != b"WEBP"
    {
        return err("WebP 签名无效");
    }
    let declared_len = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize + 8;
    if declared_len > bytes.len() || declared_len < 30 {
        return err("WebP 文件结构不完整");
    }
    let chunk_type = &bytes[12..16];
    let chunk_len = u32::from_le_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]) as usize;
    if 20 + chunk_len > declared_len {
        return err("WebP 图像块不完整");
    }
    let (width, height): (u64, u64) = if chunk_type == b"VP8X" {
        if chunk_len < 10 {
            return err("WebP 扩展头无效");
        }
        (read_u24_le(bytes, 24) + 1, read_u24_le(bytes, 27) + 1)
    } else if chunk_type == b"VP8 " {
        if chunk_len < 10 || bytes[23] != 0x9d || bytes[24] != 0x01 || bytes[25] != 0x2a {
            return err("WebP 有损图像头无效");
        }
        (
            u16::from_le_bytes([bytes[26], bytes[27]]) as u64 & 0x3fff,
            u16::from_le_bytes([bytes[28], bytes[29]]) as u64 & 0x3fff,
        )
    } else if chunk_type == b"VP8L" {
        if chunk_len < 5 || bytes[20] != 0x2f {
            return err("WebP 无损图像头无效");
        }
        (
            1 + bytes[21] as u64 + ((bytes[22] as u64 & 0x3f) << 8),
            1 + (bytes[22] as u64 >> 6) + ((bytes[23] as u64) << 2) + ((bytes[24] as u64 & 0x0f) << 10),
        )
    } else {
        return err("WebP 文件缺少受支持的图像块");
    };
    assert_dimensions(width, height)?;
    Ok(RasterImage { mime_type: "image/webp", width, height })
}

/// Parse and validate a raster image (PNG/JPEG/WebP). Rejects malformed
/// structures and decompression-bomb dimensions.
pub fn read_raster_image_metadata(bytes: &[u8]) -> Result<RasterImage, InvalidRasterImageError> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        read_png(bytes)
    } else if bytes.len() >= 2 && bytes[0] == 0xff && bytes[1] == 0xd8 {
        read_jpeg(bytes)
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        read_webp(bytes)
    } else {
        err("仅支持 PNG、JPEG 或 WebP 图片")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal valid PNG: signature + IHDR (13-byte payload, width/height 8x4) + IEND.
    fn minimal_png(w: u32, h: u32) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"\x89PNG\r\n\x1a\n");
        out.extend_from_slice(&13u32.to_be_bytes());
        out.extend_from_slice(b"IHDR");
        out.extend_from_slice(&w.to_be_bytes());
        out.extend_from_slice(&h.to_be_bytes());
        out.extend_from_slice(&[8, 6, 0, 0, 0]); // bit depth, color type, compression/filter/interlace
        out.extend_from_slice(&[0, 0, 0, 0]); // CRC (unchecked by parser)
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(b"IEND");
        out.extend_from_slice(&[0, 0, 0, 0]); // IEND CRC
        out
    }

    /// Minimal JPEG: SOI + SOF0 segment (height/width) + EOI.
    fn minimal_jpeg(w: u16, h: u16) -> Vec<u8> {
        let mut out = vec![0xff, 0xd8, 0xff];
        out.push(0xc0);
        let len: u16 = 11; // 2(len)+1(precision)+2(h)+2(w)+1(ncomp)+3*2
        out.extend_from_slice(&len.to_be_bytes());
        out.push(8); // precision
        out.extend_from_slice(&h.to_be_bytes());
        out.extend_from_slice(&w.to_be_bytes());
        out.push(1); // components
        out.extend_from_slice(&[1, 0x11, 0]); // comp id, sampling, quant
        out.extend_from_slice(&[0xff, 0xd9]); // EOI
        out
    }

    fn minimal_webp_vp8l(w: u32, h: u32) -> Vec<u8> {
        // width-1 low 14 bits: b0 | (b1 & 0x3f) << 8 ; height-1: (b1 >> 6) | b2 << 2 | (b3 & 0x0f) << 10
        let ww = w - 1;
        let hh = h - 1;
        let b0 = (ww & 0xff) as u8;
        let b1 = (((ww >> 8) & 0x3f) as u8) | (((hh & 0x3) as u8) << 6);
        let b2 = ((hh >> 2) & 0xff) as u8;
        let b3 = ((hh >> 10) & 0x0f) as u8;
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&22u32.to_le_bytes()); // 30-byte file: 30 - 8
        out.extend_from_slice(b"WEBP");
        out.extend_from_slice(b"VP8L");
        out.extend_from_slice(&5u32.to_le_bytes());
        out.push(0x2f);
        out.push(b0);
        out.push(b1);
        out.push(b2);
        out.push(b3);
        out.extend_from_slice(&[0, 0, 0, 0, 0]); // pad to 30-byte integral file
        out
    }

    #[test]
    fn png_ok() {
        let img = read_raster_image_metadata(&minimal_png(8, 4)).unwrap();
        assert_eq!(img.mime_type, "image/png");
        assert_eq!((img.width, img.height), (8, 4));
    }

    #[test]
    fn png_missing_iend_rejected() {
        let mut b = minimal_png(8, 4);
        b.truncate(b.len() - 8); // drop IEND
        assert!(read_raster_image_metadata(&b).is_err());
    }

    #[test]
    fn png_bomb_dimensions_rejected() {
        // 100_000 x 100_000 = 10^10 pixels > 40MP cap.
        assert!(read_raster_image_metadata(&minimal_png(100_000, 100_000)).is_err());
    }

    #[test]
    fn jpeg_ok() {
        let img = read_raster_image_metadata(&minimal_jpeg(320, 240)).unwrap();
        assert_eq!(img.mime_type, "image/jpeg");
        assert_eq!((img.width, img.height), (320, 240));
    }

    #[test]
    fn jpeg_missing_eoi_rejected() {
        let mut b = minimal_jpeg(320, 240);
        b.pop();
        b.pop();
        assert!(read_raster_image_metadata(&b).is_err());
    }

    #[test]
    fn webp_vp8l_ok() {
        let img = read_raster_image_metadata(&minimal_webp_vp8l(64, 48)).unwrap();
        assert_eq!(img.mime_type, "image/webp");
        assert_eq!((img.width, img.height), (64, 48));
    }

    #[test]
    fn unsupported_format_rejected() {
        assert!(read_raster_image_metadata(b"GIF89a not supported").is_err());
        assert!(read_raster_image_metadata(b"").is_err());
    }
}
