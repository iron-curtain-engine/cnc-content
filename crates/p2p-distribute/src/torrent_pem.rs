// SPDX-License-Identifier: MIT OR Apache-2.0

//! PEM-armored encoding of `.torrent` files — text-safe representation
//! for sharing complete torrent metadata in chat, forums, APIs, and QR
//! codes.
//!
//! ## What
//!
//! [`encode`] wraps raw `.torrent` bytes in a PEM envelope:
//!
//! ```text
//! -----BEGIN TORRENT-----
//! VEhpcyBpcyBhIHRlc3Qgb2YgdGhlIGVtZXJnZW5jeSBicm9hZGNhc3Qgc3lz
//! dGVtLiBUaGlzIGlzIG9ubHkgYSB0ZXN0Lg==
//! -----END TORRENT-----
//! ```
//!
//! [`decode`] reverses it — strips the markers, decodes the Base64, and
//! returns the original `.torrent` bytes.
//!
//! ## Why
//!
//! `.torrent` files are binary. They cannot be pasted in Discord, IRC, a
//! forum post, a JSON API response, or a QR code. Magnet links are text
//! but lack web seed URLs (the `ws=` parameter is non-standard and most
//! clients ignore it). For a web-seed-first architecture, losing web
//! seeds means losing the primary download path.
//!
//! PEM encoding solves this: the full `.torrent` — piece hashes, web
//! seeds, trackers, file metadata — survives any text transport losslessly.
//!
//! ## How
//!
//! - **Encoding:** Standard Base64 (RFC 4648 §4, alphabet `A-Za-z0-9+/`,
//!   `=` padding) with 76-character line wrapping (RFC 7468 §2).
//! - **Markers:** `-----BEGIN TORRENT-----` / `-----END TORRENT-----`.
//!   The label `TORRENT` follows RFC 7468's `label = 1*(ALPHA / SP)`
//!   grammar.
//! - **Whitespace tolerance:** [`decode`] ignores blank lines, leading/trailing
//!   whitespace, and `\r` characters. Only the Base64 alphabet and `=`
//!   between the markers are significant.
//! - **No external dependencies:** Base64 encode/decode is implemented
//!   inline (~80 lines) to avoid adding a crate for a simple codec.
//!
//! ## Example
//!
//! ```
//! use p2p_distribute::torrent_pem;
//!
//! let torrent_bytes = b"d8:announce35:http://tracker.example.com/announcee";
//! let pem = torrent_pem::encode(torrent_bytes);
//! assert!(pem.starts_with("-----BEGIN TORRENT-----"));
//! assert!(pem.ends_with("-----END TORRENT-----\n"));
//!
//! let decoded = torrent_pem::decode(&pem).expect("valid PEM");
//! assert_eq!(decoded, torrent_bytes);
//! ```

// ── Constants ───────────────────────────────────────────────────────

/// PEM header line.
const BEGIN_MARKER: &str = "-----BEGIN TORRENT-----";
/// PEM footer line.
const END_MARKER: &str = "-----END TORRENT-----";
/// Maximum characters per Base64 line (RFC 7468 §2).
const LINE_WIDTH: usize = 76;

// ── Public API ──────────────────────────────────────────────────────

/// Errors from [`decode`].
#[derive(Debug, thiserror::Error)]
pub enum PemError {
    /// The input does not contain a valid BEGIN marker.
    #[error("missing PEM header: expected \"{BEGIN_MARKER}\"")]
    MissingHeader,
    /// The input does not contain a valid END marker after the header.
    #[error("missing PEM footer: expected \"{END_MARKER}\"")]
    MissingFooter,
    /// The Base64 payload between markers is invalid.
    #[error("invalid Base64 at byte {position}: {reason}")]
    InvalidBase64 {
        /// Byte offset within the Base64 payload where decoding failed.
        position: usize,
        /// Human-readable description of the decoding error.
        reason: String,
    },
}

/// Encodes raw `.torrent` bytes into a PEM-armored string.
///
/// The output is a self-contained text block suitable for pasting in
/// chat, embedding in JSON, or printing as a QR code. Line width is
/// 76 characters per RFC 7468.
pub fn encode(torrent_data: &[u8]) -> String {
    let b64 = base64_encode(torrent_data);

    // Pre-allocate: markers + newlines + base64 lines.
    let line_count = b64.len().div_ceil(LINE_WIDTH);
    let capacity = BEGIN_MARKER.len()
        + 1 // newline after header
        + b64.len()
        + line_count // newlines between lines
        + END_MARKER.len()
        + 1; // trailing newline
    let mut out = String::with_capacity(capacity);

    out.push_str(BEGIN_MARKER);
    out.push('\n');

    // Wrap Base64 at LINE_WIDTH characters per line.
    let mut offset = 0;
    while offset < b64.len() {
        let end = b64.len().min(offset.saturating_add(LINE_WIDTH));
        if let Some(line) = b64.get(offset..end) {
            out.push_str(line);
        }
        out.push('\n');
        offset = end;
    }

    out.push_str(END_MARKER);
    out.push('\n');

    out
}

/// Decodes a PEM-armored string back into raw `.torrent` bytes.
///
/// Tolerates leading/trailing whitespace, blank lines, and `\r`
/// characters. Returns [`PemError`] if the markers are missing or the
/// Base64 payload is malformed.
pub fn decode(pem: &str) -> Result<Vec<u8>, PemError> {
    // ── Locate markers ──────────────────────────────────────────────
    let header_start = pem.find(BEGIN_MARKER).ok_or(PemError::MissingHeader)?;
    let body_start = header_start.saturating_add(BEGIN_MARKER.len());
    let after_header = pem.get(body_start..).unwrap_or("");

    let footer_offset = after_header
        .find(END_MARKER)
        .ok_or(PemError::MissingFooter)?;
    let body = after_header.get(..footer_offset).unwrap_or("");

    // ── Collect Base64 characters (skip whitespace) ─────────────────
    let b64: String = body.chars().filter(|c| !c.is_ascii_whitespace()).collect();

    base64_decode(&b64)
}

// ── Base64 codec (RFC 4648 §4) ──────────────────────────────────────

/// Standard Base64 alphabet.
const B64_ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Encodes bytes to Base64 (no line wrapping).
fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    let mut i: usize = 0;

    while i.saturating_add(2) < data.len() {
        let (a, b, c) = match (data.get(i), data.get(i + 1), data.get(i + 2)) {
            (Some(&a), Some(&b), Some(&c)) => (a, b, c),
            _ => break,
        };
        // Each group of 3 bytes encodes to 4 Base64 characters.
        push_b64_char(&mut out, a >> 2);
        push_b64_char(&mut out, ((a & 0x03) << 4) | (b >> 4));
        push_b64_char(&mut out, ((b & 0x0f) << 2) | (c >> 6));
        push_b64_char(&mut out, c & 0x3f);
        i = i.saturating_add(3);
    }

    let remaining = data.len().saturating_sub(i);
    if remaining == 2 {
        if let (Some(&a), Some(&b)) = (data.get(i), data.get(i + 1)) {
            push_b64_char(&mut out, a >> 2);
            push_b64_char(&mut out, ((a & 0x03) << 4) | (b >> 4));
            push_b64_char(&mut out, (b & 0x0f) << 2);
            out.push('=');
        }
    } else if remaining == 1 {
        if let Some(&a) = data.get(i) {
            push_b64_char(&mut out, a >> 2);
            push_b64_char(&mut out, (a & 0x03) << 4);
            out.push('=');
            out.push('=');
        }
    }

    out
}

/// Pushes a single Base64 character (6-bit index → alphabet lookup).
fn push_b64_char(out: &mut String, idx: u8) {
    if let Some(&ch) = B64_ALPHABET.get(idx as usize) {
        out.push(ch as char);
    }
}

/// Decodes Base64 string to bytes. Input must contain only Base64
/// alphabet characters and `=` padding (no whitespace).
fn base64_decode(b64: &str) -> Result<Vec<u8>, PemError> {
    if b64.is_empty() {
        return Ok(Vec::new());
    }

    let bytes = b64.as_bytes();
    let mut out = Vec::with_capacity((bytes.len() / 4) * 3);
    let mut i = 0;

    while i < bytes.len() {
        // Decode 4 characters at a time.
        let a = decode_b64_char(bytes, i)?;
        let b = decode_b64_char(bytes, i.saturating_add(1))?;

        // Third and fourth characters may be padding.
        let c_byte = bytes.get(i.saturating_add(2)).copied().unwrap_or(b'=');
        let d_byte = bytes.get(i.saturating_add(3)).copied().unwrap_or(b'=');

        out.push((a << 2) | (b >> 4));

        if c_byte != b'=' {
            let c = b64_value(c_byte).ok_or_else(|| PemError::InvalidBase64 {
                position: i.saturating_add(2),
                reason: format!("unexpected character: {:?}", c_byte as char),
            })?;
            out.push((b << 4) | (c >> 2));

            if d_byte != b'=' {
                let d = b64_value(d_byte).ok_or_else(|| PemError::InvalidBase64 {
                    position: i.saturating_add(3),
                    reason: format!("unexpected character: {:?}", d_byte as char),
                })?;
                out.push((c << 6) | d);
            }
        }

        i = i.saturating_add(4);
    }

    Ok(out)
}

/// Decodes a single Base64 character at position `pos` in the byte slice.
fn decode_b64_char(bytes: &[u8], pos: usize) -> Result<u8, PemError> {
    let byte = bytes
        .get(pos)
        .copied()
        .ok_or_else(|| PemError::InvalidBase64 {
            position: pos,
            reason: "unexpected end of Base64 data".into(),
        })?;
    b64_value(byte).ok_or_else(|| PemError::InvalidBase64 {
        position: pos,
        reason: format!("unexpected character: {:?}", byte as char),
    })
}

/// Maps a Base64 character to its 6-bit value, or `None` for invalid chars.
fn b64_value(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Base64 codec ────────────────────────────────────────────────

    /// Empty input encodes to empty output.
    #[test]
    fn base64_empty() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_decode("").unwrap(), b"");
    }

    /// Single byte encodes with two padding characters.
    #[test]
    fn base64_one_byte() {
        assert_eq!(base64_encode(b"M"), "TQ==");
        assert_eq!(base64_decode("TQ==").unwrap(), b"M");
    }

    /// Two bytes encode with one padding character.
    #[test]
    fn base64_two_bytes() {
        assert_eq!(base64_encode(b"Ma"), "TWE=");
        assert_eq!(base64_decode("TWE=").unwrap(), b"Ma");
    }

    /// Three bytes encode to exactly 4 characters (no padding).
    #[test]
    fn base64_three_bytes() {
        assert_eq!(base64_encode(b"Man"), "TWFu");
        assert_eq!(base64_decode("TWFu").unwrap(), b"Man");
    }

    /// RFC 4648 test vectors.
    #[test]
    fn base64_rfc4648_vectors() {
        let cases: &[(&[u8], &str)] = &[
            (b"", ""),
            (b"f", "Zg=="),
            (b"fo", "Zm8="),
            (b"foo", "Zm9v"),
            (b"foob", "Zm9vYg=="),
            (b"fooba", "Zm9vYmE="),
            (b"foobar", "Zm9vYmFy"),
        ];
        for &(input, expected) in cases {
            assert_eq!(base64_encode(input), expected, "encode {:?}", input);
            assert_eq!(base64_decode(expected).unwrap(), input, "decode {expected}");
        }
    }

    /// Round-trip with binary data covering all byte values.
    #[test]
    fn base64_all_byte_values() {
        let data: Vec<u8> = (0..=255).collect();
        let encoded = base64_encode(&data);
        let decoded = base64_decode(&encoded).unwrap();
        assert_eq!(decoded, data);
    }

    /// Invalid Base64 character produces an error.
    #[test]
    fn base64_invalid_char() {
        let err = base64_decode("TQ!=").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("unexpected character"), "got: {msg}");
    }

    // ── PEM encode/decode ───────────────────────────────────────────

    /// Round-trip: encode then decode returns original bytes.
    #[test]
    fn pem_round_trip() {
        let data = b"d8:announce35:http://tracker.example.com/announcee";
        let pem = encode(data);
        let decoded = decode(&pem).unwrap();
        assert_eq!(decoded, data);
    }

    /// Encoded output starts with the correct header.
    #[test]
    fn pem_has_header() {
        let pem = encode(b"test");
        assert!(pem.starts_with(BEGIN_MARKER));
    }

    /// Encoded output ends with the correct footer and trailing newline.
    #[test]
    fn pem_has_footer() {
        let pem = encode(b"test");
        assert!(pem.ends_with(&format!("{END_MARKER}\n")));
    }

    /// Line width does not exceed 76 characters (RFC 7468).
    #[test]
    fn pem_line_width() {
        // Use enough data to produce multiple lines.
        let data = vec![0xABu8; 300];
        let pem = encode(&data);
        for line in pem.lines() {
            if line.starts_with("-----") {
                continue; // Markers may be longer.
            }
            assert!(
                line.len() <= LINE_WIDTH,
                "line too long ({} chars): {line}",
                line.len()
            );
        }
    }

    /// Decode tolerates leading text before the header.
    #[test]
    fn pem_ignores_leading_text() {
        let pem = format!("Here is a torrent:\n{}", encode(b"data"));
        let decoded = decode(&pem).unwrap();
        assert_eq!(decoded, b"data");
    }

    /// Decode tolerates trailing text after the footer.
    #[test]
    fn pem_ignores_trailing_text() {
        let mut pem = encode(b"data");
        pem.push_str("\nSome trailing text\n");
        let decoded = decode(&pem).unwrap();
        assert_eq!(decoded, b"data");
    }

    /// Decode tolerates Windows-style line endings.
    #[test]
    fn pem_crlf_tolerant() {
        let pem = encode(b"hello");
        let crlf = pem.replace('\n', "\r\n");
        let decoded = decode(&crlf).unwrap();
        assert_eq!(decoded, b"hello");
    }

    /// Missing header produces the correct error.
    #[test]
    fn pem_missing_header() {
        let err = decode("just some text").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("missing PEM header"), "got: {msg}");
    }

    /// Missing footer after valid header produces the correct error.
    #[test]
    fn pem_missing_footer() {
        let err = decode(BEGIN_MARKER).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("missing PEM footer"), "got: {msg}");
    }

    /// Empty payload between markers decodes to empty bytes.
    #[test]
    fn pem_empty_payload() {
        let pem = format!("{BEGIN_MARKER}\n{END_MARKER}\n");
        let decoded = decode(&pem).unwrap();
        assert!(decoded.is_empty());
    }

    /// Round-trip with large binary data (simulating a real .torrent).
    #[test]
    fn pem_large_round_trip() {
        // Simulate a .torrent with ~20 KB of pseudo-random binary data.
        let mut data = Vec::with_capacity(20_000);
        for i in 0u16..10_000 {
            let [lo, hi] = i.to_le_bytes();
            data.push(lo);
            data.push(hi);
        }
        let pem = encode(&data);
        let decoded = decode(&pem).unwrap();
        assert_eq!(decoded, data);
    }

    // ── Error display ───────────────────────────────────────────────

    /// MissingHeader error message includes the expected marker.
    #[test]
    fn error_missing_header_display() {
        let msg = PemError::MissingHeader.to_string();
        assert!(msg.contains("BEGIN TORRENT"), "got: {msg}");
    }

    /// MissingFooter error message includes the expected marker.
    #[test]
    fn error_missing_footer_display() {
        let msg = PemError::MissingFooter.to_string();
        assert!(msg.contains("END TORRENT"), "got: {msg}");
    }

    /// InvalidBase64 error includes position and reason.
    #[test]
    fn error_invalid_base64_display() {
        let err = PemError::InvalidBase64 {
            position: 42,
            reason: "bad char".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("42"), "got: {msg}");
        assert!(msg.contains("bad char"), "got: {msg}");
    }
}
