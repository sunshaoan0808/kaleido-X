//! Incremental UTF-8 decode for SSE / HTTP byte streams.
//!
//! TCP/HTTP chunks may split multi-byte codepoints (CJK is 3 bytes). Calling
//! `String::from_utf8_lossy` per chunk permanently inserts U+FFFD (`�`) and
//! drops the partial bytes — that is the root cause of mojibake in Story Tavern
//! streams (e.g. 白昼之下 · P3). Keep a carry buffer of incomplete trailing
//! bytes and only emit complete UTF-8 sequences.

/// Append `chunk` to `carry` (incomplete trailing bytes from previous chunk).
/// Returns newly completed UTF-8 text; leaves 0..=3 incomplete bytes in `carry`.
pub fn push_utf8_chunk(carry: &mut Vec<u8>, chunk: &[u8]) -> String {
    if chunk.is_empty() && carry.is_empty() {
        return String::new();
    }
    carry.extend_from_slice(chunk);
    match std::str::from_utf8(carry) {
        Ok(s) => {
            let out = s.to_string();
            carry.clear();
            out
        }
        Err(e) => {
            let good = e.valid_up_to();
            // If error is only "incomplete at end", keep the tail; otherwise
            // (true invalid mid-stream) skip one bad byte and continue.
            if good == 0 {
                if e.error_len().is_some() {
                    // Invalid sequence at start — drop one byte as U+FFFD-equivalent skip
                    // but still try to recover rest of buffer.
                    carry.remove(0);
                    return push_utf8_chunk(carry, &[]);
                }
                // All bytes are incomplete prefix of a valid char — wait for more.
                return String::new();
            }
            let out = std::str::from_utf8(&carry[..good])
                .unwrap_or("")
                .to_string();
            let rest = carry[good..].to_vec();
            // If the error has a definite length, the bytes after `good` are
            // invalid (not merely incomplete). Drop the bad sequence and recurse
            // so we don't stall the stream forever on garbage.
            if let Some(bad_len) = e.error_len() {
                let skip = bad_len.min(rest.len()).max(1);
                *carry = rest[skip..].to_vec();
                let more = push_utf8_chunk(carry, &[]);
                let mut combined = out;
                // Represent dropped invalid bytes as U+FFFD once (rare for LLM SSE).
                combined.push('\u{FFFD}');
                combined.push_str(&more);
                return combined;
            }
            // Incomplete trailing sequence — keep it.
            *carry = rest;
            out
        }
    }
}

/// Flush any remaining carry at end-of-stream. Incomplete tail → one U+FFFD.
pub fn flush_utf8_carry(carry: &mut Vec<u8>) -> String {
    if carry.is_empty() {
        return String::new();
    }
    let out = String::from_utf8_lossy(carry).into_owned();
    carry.clear();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cjk_split_across_chunks_roundtrips() {
        // 转 = E8 BD AC ; 身 = E8 BA AB
        let s = "转身";
        let bytes = s.as_bytes();
        assert_eq!(bytes.len(), 6);
        let mut carry = Vec::new();
        let mut acc = String::new();
        // split every single byte
        for b in bytes {
            acc.push_str(&push_utf8_chunk(&mut carry, &[*b]));
        }
        acc.push_str(&flush_utf8_carry(&mut carry));
        assert_eq!(acc, "转身");
        assert!(carry.is_empty());
    }

    #[test]
    fn mixed_ascii_and_cjk() {
        let s = "他转到309";
        let bytes = s.as_bytes();
        let mut carry = Vec::new();
        let mut acc = String::new();
        // awkward splits: 1,2,3,1,2,...
        let mut i = 0;
        let mut step = 1;
        while i < bytes.len() {
            let end = (i + step).min(bytes.len());
            acc.push_str(&push_utf8_chunk(&mut carry, &bytes[i..end]));
            i = end;
            step = if step == 3 { 1 } else { step + 1 };
        }
        acc.push_str(&flush_utf8_carry(&mut carry));
        assert_eq!(acc, s);
    }

    #[test]
    fn no_fffd_on_clean_split() {
        let s = "琥珀色的底。她的声音。";
        let bytes = s.as_bytes();
        let mut carry = Vec::new();
        let mut acc = String::new();
        for chunk in bytes.chunks(2) {
            acc.push_str(&push_utf8_chunk(&mut carry, chunk));
        }
        acc.push_str(&flush_utf8_carry(&mut carry));
        assert_eq!(acc, s);
        assert!(!acc.contains('\u{FFFD}'));
    }
}
