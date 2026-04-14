// SPDX-License-Identifier: MIT OR Apache-2.0

//! Zero-copy bencode codec — encode and decode BitTorrent's canonical
//! serialisation format.
//!
//! ## What
//!
//! Bencode is the binary encoding used in `.torrent` files, tracker
//! responses, and extension messages.  This module provides a complete
//! encoder and decoder with a single `BencodeValue` enum that can
//! represent any bencoded document.
//!
//! ## Why
//!
//! `torrent_create.rs` has ad-hoc bencode *encoding* helpers, but no
//! *decoding* support.  Parsing `.torrent` files, tracker announce
//! responses, and DHT extension messages all require a round-trip
//! codec.  Rather than adding a crate dependency for a 200-line format,
//! we keep it in-crate where it can stay allocation-minimal and share
//! the crate's safe-indexing discipline.
//!
//! ## How
//!
//! - **Encoding:** [`encode`] serialises a `BencodeValue` into bytes.
//!   Dictionary keys are automatically sorted lexicographically
//!   (bencode spec requirement).
//! - **Decoding:** [`decode`] parses a byte slice into a `BencodeValue`
//!   tree.  The parser is recursive-descent with an explicit depth limit
//!   to prevent stack overflow on adversarial input.
//! - **Values are owned** (`Vec<u8>`, `Vec<…>`) because decoded data
//!   typically outlives the input buffer (e.g. when reading from a
//!   network socket).  For encoding from static data, the cost is
//!   negligible compared to I/O.

use thiserror::Error;

// ── Constants ───────────────────────────────────────────────────────

/// Maximum nesting depth for decode.  Prevents stack overflow on
/// adversarial input with deeply nested lists/dicts.
const MAX_DECODE_DEPTH: usize = 64;

/// Maximum allowed integer digit length to prevent memory exhaustion
/// from malformed `i<huge number>e` tokens.
const MAX_INT_DIGITS: usize = 20;

// ── Value type ──────────────────────────────────────────────────────

/// A single bencoded value.
///
/// Bencode has exactly four types:
/// - **Bytes** (`<len>:<data>`) — arbitrary byte strings.
/// - **Int** (`i<number>e`) — signed 64-bit integers.
/// - **List** (`l…e`) — ordered sequence of values.
/// - **Dict** (`d…e`) — ordered map of byte-string keys to values.
///
/// ```
/// use p2p_distribute::bencode::{BencodeValue, encode, decode};
///
/// let val = BencodeValue::Dict(vec![
///     (b"length".to_vec(), BencodeValue::Int(42)),
///     (b"name".to_vec(), BencodeValue::Bytes(b"test.zip".to_vec())),
/// ]);
/// let encoded = encode(&val);
/// let decoded = decode(&encoded).unwrap();
/// assert_eq!(val, decoded);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BencodeValue {
    /// Byte string — arbitrary binary data.
    Bytes(Vec<u8>),
    /// Signed 64-bit integer.
    Int(i64),
    /// Ordered list of values.
    List(Vec<BencodeValue>),
    /// Dictionary mapping byte-string keys to values.
    /// Keys are maintained in sorted order after encoding/decoding.
    Dict(Vec<(Vec<u8>, BencodeValue)>),
}

impl BencodeValue {
    /// Returns the value as a byte string, if it is one.
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Bytes(b) => Some(b),
            _ => None,
        }
    }

    /// Returns the value as an integer, if it is one.
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Self::Int(n) => Some(*n),
            _ => None,
        }
    }

    /// Returns the value as a list, if it is one.
    pub fn as_list(&self) -> Option<&[BencodeValue]> {
        match self {
            Self::List(l) => Some(l),
            _ => None,
        }
    }

    /// Returns the value as a dict, if it is one.
    pub fn as_dict(&self) -> Option<&[(Vec<u8>, BencodeValue)]> {
        match self {
            Self::Dict(d) => Some(d),
            _ => None,
        }
    }

    /// Looks up a key in a dict value.  Returns `None` if `self` is
    /// not a dict or the key is absent.
    pub fn dict_get(&self, key: &[u8]) -> Option<&BencodeValue> {
        self.as_dict().and_then(|entries| {
            entries
                .iter()
                .find(|(k, _)| k.as_slice() == key)
                .map(|(_, v)| v)
        })
    }
}

// ── Errors ──────────────────────────────────────────────────────────

/// Errors from decoding bencoded data.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum DecodeError {
    /// Input ended before a complete value could be parsed.
    #[error("unexpected end of input at byte {position}")]
    UnexpectedEof {
        /// Byte offset where input ended.
        position: usize,
    },

    /// A byte was encountered that cannot start a valid bencode token.
    #[error("invalid token byte 0x{byte:02X} at position {position}")]
    InvalidToken {
        /// The unexpected byte value.
        byte: u8,
        /// Byte offset of the unexpected byte.
        position: usize,
    },

    /// An integer literal was malformed (leading zeros, empty, etc.).
    #[error("malformed integer at position {position}: {reason}")]
    MalformedInt {
        /// Byte offset where the integer started.
        position: usize,
        /// Human-readable explanation.
        reason: &'static str,
    },

    /// A string length prefix could not be parsed as a valid number.
    #[error("invalid string length at position {position}")]
    InvalidStringLength {
        /// Byte offset where the length prefix started.
        position: usize,
    },

    /// Dictionary keys were not in sorted order (bencode spec violation).
    #[error("unsorted dictionary key at position {position}")]
    UnsortedKey {
        /// Byte offset of the out-of-order key.
        position: usize,
    },

    /// Nesting depth exceeded the safety limit.
    #[error("nesting depth exceeded limit of {limit} at position {position}")]
    TooDeep {
        /// Maximum allowed depth.
        limit: usize,
        /// Byte offset where the limit was hit.
        position: usize,
    },

    /// Trailing bytes after the top-level value.
    #[error("trailing data: {trailing_bytes} bytes after position {position}")]
    TrailingData {
        /// Byte offset where the value ended.
        position: usize,
        /// Number of unconsumed bytes.
        trailing_bytes: usize,
    },
}

// ── Encoding ────────────────────────────────────────────────────────

/// Encodes a `BencodeValue` into its canonical bencoded byte form.
///
/// Dictionary keys are sorted lexicographically as required by the
/// bencode specification.  This ensures deterministic output for
/// info-hash computation.
pub fn encode(value: &BencodeValue) -> Vec<u8> {
    let mut out = Vec::new();
    encode_into(value, &mut out);
    out
}

/// Encodes into an existing buffer (avoids intermediate allocation).
pub fn encode_into(value: &BencodeValue, out: &mut Vec<u8>) {
    match value {
        BencodeValue::Bytes(b) => {
            out.extend_from_slice(b.len().to_string().as_bytes());
            out.push(b':');
            out.extend_from_slice(b);
        }
        BencodeValue::Int(n) => {
            out.push(b'i');
            out.extend_from_slice(n.to_string().as_bytes());
            out.push(b'e');
        }
        BencodeValue::List(items) => {
            out.push(b'l');
            for item in items {
                encode_into(item, out);
            }
            out.push(b'e');
        }
        BencodeValue::Dict(entries) => {
            // Sort keys lexicographically for canonical encoding.
            let mut sorted: Vec<&(Vec<u8>, BencodeValue)> = entries.iter().collect();
            sorted.sort_by(|(a, _), (b, _)| a.cmp(b));

            out.push(b'd');
            for (key, val) in sorted {
                // Key is always a byte string.
                out.extend_from_slice(key.len().to_string().as_bytes());
                out.push(b':');
                out.extend_from_slice(key);
                encode_into(val, out);
            }
            out.push(b'e');
        }
    }
}

// ── Decoding ────────────────────────────────────────────────────────

/// Decodes a complete bencoded document from a byte slice.
///
/// Returns an error if the input is malformed or has trailing bytes.
pub fn decode(data: &[u8]) -> Result<BencodeValue, DecodeError> {
    let mut pos = 0;
    let value = decode_value(data, &mut pos, 0)?;

    // Ensure no trailing garbage after the top-level value.
    if pos < data.len() {
        return Err(DecodeError::TrailingData {
            position: pos,
            trailing_bytes: data.len().saturating_sub(pos),
        });
    }
    Ok(value)
}

/// Decodes a single value starting at `pos`, advancing `pos` past it.
fn decode_value(data: &[u8], pos: &mut usize, depth: usize) -> Result<BencodeValue, DecodeError> {
    if depth > MAX_DECODE_DEPTH {
        return Err(DecodeError::TooDeep {
            limit: MAX_DECODE_DEPTH,
            position: *pos,
        });
    }

    let &first = data
        .get(*pos)
        .ok_or(DecodeError::UnexpectedEof { position: *pos })?;

    match first {
        // Integer: i<digits>e
        b'i' => decode_int(data, pos),
        // List: l<values>e
        b'l' => decode_list(data, pos, depth),
        // Dict: d<key-value pairs>e
        b'd' => decode_dict(data, pos, depth),
        // String: <length>:<data>
        b'0'..=b'9' => decode_bytes(data, pos),
        _ => Err(DecodeError::InvalidToken {
            byte: first,
            position: *pos,
        }),
    }
}

/// Decodes an integer `i<digits>e`.
fn decode_int(data: &[u8], pos: &mut usize) -> Result<BencodeValue, DecodeError> {
    let start = *pos;
    // Skip 'i'
    *pos = pos
        .checked_add(1)
        .ok_or(DecodeError::UnexpectedEof { position: *pos })?;

    // Find 'e' terminator.
    let end = find_byte(data, *pos, b'e').ok_or(DecodeError::UnexpectedEof { position: *pos })?;

    let digits = data
        .get(*pos..end)
        .ok_or(DecodeError::UnexpectedEof { position: *pos })?;

    // Validate: no empty integer, no leading zeros (except "0" itself),
    // no "-0".
    if digits.is_empty() {
        return Err(DecodeError::MalformedInt {
            position: start,
            reason: "empty integer",
        });
    }

    if digits.len() > MAX_INT_DIGITS {
        return Err(DecodeError::MalformedInt {
            position: start,
            reason: "integer too long",
        });
    }

    // No leading zeros: "i03e" is invalid, "i0e" is valid.
    if digits.len() > 1 && digits.first() == Some(&b'0') {
        return Err(DecodeError::MalformedInt {
            position: start,
            reason: "leading zero",
        });
    }

    // No negative zero: "i-0e" is invalid.
    if digits == b"-0" {
        return Err(DecodeError::MalformedInt {
            position: start,
            reason: "negative zero",
        });
    }

    // No bare minus: "i-e" is invalid.
    if digits == b"-" {
        return Err(DecodeError::MalformedInt {
            position: start,
            reason: "bare minus sign",
        });
    }

    // Leading zeros after minus: "i-03e" is invalid.
    if digits.len() > 2 && digits.first() == Some(&b'-') && digits.get(1) == Some(&b'0') {
        return Err(DecodeError::MalformedInt {
            position: start,
            reason: "leading zero after minus",
        });
    }

    let s = std::str::from_utf8(digits).map_err(|_| DecodeError::MalformedInt {
        position: start,
        reason: "non-ASCII digits",
    })?;

    let n: i64 = s.parse().map_err(|_| DecodeError::MalformedInt {
        position: start,
        reason: "integer parse failed",
    })?;

    // Skip past 'e'.
    *pos = end
        .checked_add(1)
        .ok_or(DecodeError::UnexpectedEof { position: end })?;

    Ok(BencodeValue::Int(n))
}

/// Decodes a byte string `<length>:<data>`.
fn decode_bytes(data: &[u8], pos: &mut usize) -> Result<BencodeValue, DecodeError> {
    let start = *pos;

    // Find the ':' separator.
    let colon =
        find_byte(data, *pos, b':').ok_or(DecodeError::InvalidStringLength { position: start })?;

    let len_digits = data
        .get(*pos..colon)
        .ok_or(DecodeError::InvalidStringLength { position: start })?;

    // Parse length.
    let len_str = std::str::from_utf8(len_digits)
        .map_err(|_| DecodeError::InvalidStringLength { position: start })?;

    let len: usize = len_str
        .parse()
        .map_err(|_| DecodeError::InvalidStringLength { position: start })?;

    // Advance past ':'.
    let data_start = colon
        .checked_add(1)
        .ok_or(DecodeError::UnexpectedEof { position: colon })?;
    let data_end = data_start
        .checked_add(len)
        .ok_or(DecodeError::UnexpectedEof {
            position: data_start,
        })?;

    let bytes = data
        .get(data_start..data_end)
        .ok_or(DecodeError::UnexpectedEof {
            position: data_start,
        })?;

    *pos = data_end;
    Ok(BencodeValue::Bytes(bytes.to_vec()))
}

/// Decodes a list `l<values>e`.
fn decode_list(data: &[u8], pos: &mut usize, depth: usize) -> Result<BencodeValue, DecodeError> {
    // Skip 'l'.
    *pos = pos
        .checked_add(1)
        .ok_or(DecodeError::UnexpectedEof { position: *pos })?;

    let mut items = Vec::new();
    loop {
        let &next = data
            .get(*pos)
            .ok_or(DecodeError::UnexpectedEof { position: *pos })?;
        if next == b'e' {
            *pos = pos
                .checked_add(1)
                .ok_or(DecodeError::UnexpectedEof { position: *pos })?;
            return Ok(BencodeValue::List(items));
        }
        items.push(decode_value(data, pos, depth.saturating_add(1))?);
    }
}

/// Decodes a dict `d<key-value pairs>e`.
fn decode_dict(data: &[u8], pos: &mut usize, depth: usize) -> Result<BencodeValue, DecodeError> {
    // Skip 'd'.
    *pos = pos
        .checked_add(1)
        .ok_or(DecodeError::UnexpectedEof { position: *pos })?;

    let mut entries: Vec<(Vec<u8>, BencodeValue)> = Vec::new();
    loop {
        let &next = data
            .get(*pos)
            .ok_or(DecodeError::UnexpectedEof { position: *pos })?;
        if next == b'e' {
            *pos = pos
                .checked_add(1)
                .ok_or(DecodeError::UnexpectedEof { position: *pos })?;
            return Ok(BencodeValue::Dict(entries));
        }

        let key_pos = *pos;
        // Keys must be byte strings.
        let key = match decode_value(data, pos, depth.saturating_add(1))? {
            BencodeValue::Bytes(k) => k,
            _ => {
                return Err(DecodeError::InvalidToken {
                    byte: data.get(key_pos).copied().unwrap_or(0),
                    position: key_pos,
                })
            }
        };

        // Enforce sorted key order (bencode spec requirement).
        if let Some(last) = entries.last() {
            if key <= last.0 {
                return Err(DecodeError::UnsortedKey { position: key_pos });
            }
        }

        let val = decode_value(data, pos, depth.saturating_add(1))?;
        entries.push((key, val));
    }
}

// ── Helpers ─────────────────────────────────────────────────────────

/// Finds the first occurrence of `needle` in `data` starting at `from`.
fn find_byte(data: &[u8], from: usize, needle: u8) -> Option<usize> {
    let slice = data.get(from..)?;
    slice
        .iter()
        .position(|&b| b == needle)
        .map(|i| from.saturating_add(i))
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Encoding ────────────────────────────────────────────────────

    /// Byte strings encode as `<length>:<data>`.
    ///
    /// The length prefix is decimal ASCII, followed by a colon, then raw
    /// bytes.  This is the only bencode type that carries arbitrary
    /// binary data.
    #[test]
    fn encode_bytes() {
        let val = BencodeValue::Bytes(b"hello".to_vec());
        assert_eq!(encode(&val), b"5:hello");
    }

    /// Empty byte string encodes as `0:`.
    ///
    /// Zero-length strings are valid and must not produce an error.
    #[test]
    fn encode_empty_bytes() {
        let val = BencodeValue::Bytes(Vec::new());
        assert_eq!(encode(&val), b"0:");
    }

    /// Positive integers encode as `i<n>e`.
    ///
    /// No padding, no leading zeros.
    #[test]
    fn encode_positive_int() {
        let val = BencodeValue::Int(42);
        assert_eq!(encode(&val), b"i42e");
    }

    /// Zero encodes as `i0e`.
    ///
    /// Exactly one zero digit, no sign.
    #[test]
    fn encode_zero() {
        let val = BencodeValue::Int(0);
        assert_eq!(encode(&val), b"i0e");
    }

    /// Negative integers encode as `i-<n>e`.
    ///
    /// The minus sign is part of the integer token.
    #[test]
    fn encode_negative_int() {
        let val = BencodeValue::Int(-7);
        assert_eq!(encode(&val), b"i-7e");
    }

    /// Lists encode as `l<items>e`.
    ///
    /// Items are bencoded sequentially in order.
    #[test]
    fn encode_list() {
        let val = BencodeValue::List(vec![
            BencodeValue::Int(1),
            BencodeValue::Bytes(b"two".to_vec()),
        ]);
        assert_eq!(encode(&val), b"li1e3:twoe");
    }

    /// Empty list encodes as `le`.
    #[test]
    fn encode_empty_list() {
        let val = BencodeValue::List(Vec::new());
        assert_eq!(encode(&val), b"le");
    }

    /// Dicts encode with keys sorted lexicographically.
    ///
    /// The bencode spec mandates sorted keys for deterministic encoding.
    /// This is critical for info-hash computation: all clients must
    /// produce the same bytes for the same logical content.
    #[test]
    fn encode_dict_sorts_keys() {
        // Insert keys in reverse order — encoder must sort them.
        let val = BencodeValue::Dict(vec![
            (b"z".to_vec(), BencodeValue::Int(2)),
            (b"a".to_vec(), BencodeValue::Int(1)),
        ]);
        assert_eq!(encode(&val), b"d1:ai1e1:zi2ee");
    }

    /// Empty dict encodes as `de`.
    #[test]
    fn encode_empty_dict() {
        let val = BencodeValue::Dict(Vec::new());
        assert_eq!(encode(&val), b"de");
    }

    // ── Decoding ────────────────────────────────────────────────────

    /// Byte strings decode correctly.
    #[test]
    fn decode_bytes_simple() {
        let val = decode(b"5:hello").unwrap();
        assert_eq!(val, BencodeValue::Bytes(b"hello".to_vec()));
    }

    /// Empty byte string decodes correctly.
    #[test]
    fn decode_empty_bytes() {
        let val = decode(b"0:").unwrap();
        assert_eq!(val, BencodeValue::Bytes(Vec::new()));
    }

    /// Positive integer decodes correctly.
    #[test]
    fn decode_positive_int() {
        let val = decode(b"i42e").unwrap();
        assert_eq!(val, BencodeValue::Int(42));
    }

    /// Zero decodes correctly.
    #[test]
    fn decode_zero() {
        let val = decode(b"i0e").unwrap();
        assert_eq!(val, BencodeValue::Int(0));
    }

    /// Negative integer decodes correctly.
    #[test]
    fn decode_negative_int() {
        let val = decode(b"i-7e").unwrap();
        assert_eq!(val, BencodeValue::Int(-7));
    }

    /// Lists decode correctly.
    #[test]
    fn decode_list() {
        let val = decode(b"li1e3:twoe").unwrap();
        assert_eq!(
            val,
            BencodeValue::List(vec![
                BencodeValue::Int(1),
                BencodeValue::Bytes(b"two".to_vec()),
            ])
        );
    }

    /// Empty list decodes correctly.
    #[test]
    fn decode_empty_list() {
        let val = decode(b"le").unwrap();
        assert_eq!(val, BencodeValue::List(Vec::new()));
    }

    /// Dicts decode correctly with sorted keys.
    #[test]
    fn decode_dict() {
        let val = decode(b"d1:ai1e1:zi2ee").unwrap();
        assert_eq!(
            val,
            BencodeValue::Dict(vec![
                (b"a".to_vec(), BencodeValue::Int(1)),
                (b"z".to_vec(), BencodeValue::Int(2)),
            ])
        );
    }

    /// Empty dict decodes correctly.
    #[test]
    fn decode_empty_dict() {
        let val = decode(b"de").unwrap();
        assert_eq!(val, BencodeValue::Dict(Vec::new()));
    }

    // ── Round-trip ──────────────────────────────────────────────────

    /// Encode → decode round-trip preserves a complex nested structure.
    ///
    /// This ensures the encoder and decoder are consistent for arbitrary
    /// nesting of all four types.
    #[test]
    fn round_trip_complex() {
        let val = BencodeValue::Dict(vec![
            (
                b"info".to_vec(),
                BencodeValue::Dict(vec![
                    (b"length".to_vec(), BencodeValue::Int(1024)),
                    (b"name".to_vec(), BencodeValue::Bytes(b"test.zip".to_vec())),
                    (b"pieces".to_vec(), BencodeValue::Bytes(vec![0xAA; 20])),
                ]),
            ),
            (
                b"url-list".to_vec(),
                BencodeValue::List(vec![BencodeValue::Bytes(
                    b"https://example.com/file.zip".to_vec(),
                )]),
            ),
        ]);

        let encoded = encode(&val);
        let decoded = decode(&encoded).unwrap();
        assert_eq!(val, decoded);
    }

    /// Deterministic encoding: same value always encodes identically.
    ///
    /// This is essential for info-hash computation — all clients must
    /// agree on the exact bytes.
    #[test]
    fn encode_deterministic() {
        let val = BencodeValue::Dict(vec![
            (b"b".to_vec(), BencodeValue::Int(2)),
            (b"a".to_vec(), BencodeValue::Int(1)),
        ]);
        let first = encode(&val);
        let second = encode(&val);
        assert_eq!(first, second);
    }

    // ── Error paths ─────────────────────────────────────────────────

    /// Leading zeros in integers are rejected.
    ///
    /// Bencode spec forbids "i03e" — only "i0e" is valid for zero.
    #[test]
    fn decode_rejects_leading_zero() {
        let err = decode(b"i03e").unwrap_err();
        assert!(matches!(err, DecodeError::MalformedInt { .. }));
    }

    /// Negative zero is rejected.
    ///
    /// Bencode spec forbids "i-0e".
    #[test]
    fn decode_rejects_negative_zero() {
        let err = decode(b"i-0e").unwrap_err();
        assert!(matches!(err, DecodeError::MalformedInt { .. }));
    }

    /// Empty integer is rejected.
    ///
    /// "ie" has no digits between 'i' and 'e'.
    #[test]
    fn decode_rejects_empty_int() {
        let err = decode(b"ie").unwrap_err();
        assert!(matches!(err, DecodeError::MalformedInt { .. }));
    }

    /// Trailing data after a valid value is rejected.
    ///
    /// A bencoded document must be exactly one value — no extra bytes.
    #[test]
    fn decode_rejects_trailing_data() {
        let err = decode(b"i42eXXX").unwrap_err();
        assert!(matches!(
            err,
            DecodeError::TrailingData {
                trailing_bytes: 3,
                ..
            }
        ));
    }

    /// Truncated string is rejected.
    ///
    /// "5:hi" claims 5 bytes but only provides 2.
    #[test]
    fn decode_rejects_truncated_string() {
        let err = decode(b"5:hi").unwrap_err();
        assert!(matches!(err, DecodeError::UnexpectedEof { .. }));
    }

    /// Unsorted dict keys are rejected.
    ///
    /// Bencode spec requires keys in lexicographic order.
    #[test]
    fn decode_rejects_unsorted_keys() {
        let err = decode(b"d1:zi2e1:ai1ee").unwrap_err();
        assert!(matches!(err, DecodeError::UnsortedKey { .. }));
    }

    /// Unknown token byte is rejected.
    #[test]
    fn decode_rejects_unknown_token() {
        let err = decode(b"X").unwrap_err();
        assert!(matches!(err, DecodeError::InvalidToken { byte: b'X', .. }));
    }

    /// Empty input is rejected.
    #[test]
    fn decode_rejects_empty() {
        let err = decode(b"").unwrap_err();
        assert!(matches!(err, DecodeError::UnexpectedEof { position: 0 }));
    }

    /// Deeply nested structures hit the depth limit.
    ///
    /// Prevents stack overflow on adversarial input.
    #[test]
    fn decode_rejects_excessive_depth() {
        // Build 70 nested lists: l l l ... e e e
        let mut input = Vec::new();
        input.resize(70, b'l');
        input.extend_from_slice(&[b'e'; 70]);
        let err = decode(&input).unwrap_err();
        assert!(matches!(err, DecodeError::TooDeep { .. }));
    }

    /// Duplicate dict keys are rejected.
    ///
    /// Two identical keys in sorted position violates the "strictly
    /// increasing" key order requirement.
    #[test]
    fn decode_rejects_duplicate_keys() {
        let err = decode(b"d1:ai1e1:ai2ee").unwrap_err();
        assert!(matches!(err, DecodeError::UnsortedKey { .. }));
    }

    // ── Accessor helpers ────────────────────────────────────────────

    /// `dict_get` finds a key in a dict value.
    #[test]
    fn dict_get_found() {
        let val = BencodeValue::Dict(vec![(b"key".to_vec(), BencodeValue::Int(99))]);
        assert_eq!(val.dict_get(b"key"), Some(&BencodeValue::Int(99)));
    }

    /// `dict_get` returns None for missing keys.
    #[test]
    fn dict_get_missing() {
        let val = BencodeValue::Dict(vec![(b"key".to_vec(), BencodeValue::Int(99))]);
        assert_eq!(val.dict_get(b"nope"), None);
    }

    /// `dict_get` returns None on non-dict values.
    #[test]
    fn dict_get_on_non_dict() {
        let val = BencodeValue::Int(42);
        assert_eq!(val.dict_get(b"key"), None);
    }

    /// `as_bytes` returns Some for Bytes variant.
    #[test]
    fn as_bytes_accessor() {
        let val = BencodeValue::Bytes(b"data".to_vec());
        assert_eq!(val.as_bytes(), Some(b"data".as_slice()));
        assert_eq!(val.as_int(), None);
    }

    /// `as_int` returns Some for Int variant.
    #[test]
    fn as_int_accessor() {
        let val = BencodeValue::Int(7);
        assert_eq!(val.as_int(), Some(7));
        assert_eq!(val.as_bytes(), None);
    }

    /// `as_list` returns Some for List variant.
    #[test]
    fn as_list_accessor() {
        let val = BencodeValue::List(vec![BencodeValue::Int(1)]);
        assert!(val.as_list().is_some());
        assert_eq!(val.as_dict(), None);
    }

    // ── Display for errors ──────────────────────────────────────────

    /// Error Display messages contain key diagnostic context.
    #[test]
    fn error_display_messages() {
        let eof = DecodeError::UnexpectedEof { position: 42 };
        assert!(eof.to_string().contains("42"));

        let token = DecodeError::InvalidToken {
            byte: 0xFF,
            position: 3,
        };
        assert!(token.to_string().contains("FF"));

        let trailing = DecodeError::TrailingData {
            position: 10,
            trailing_bytes: 5,
        };
        assert!(trailing.to_string().contains("5"));
    }

    /// Bare minus sign in integer is rejected.
    #[test]
    fn decode_rejects_bare_minus() {
        let err = decode(b"i-e").unwrap_err();
        assert!(matches!(err, DecodeError::MalformedInt { .. }));
    }

    /// Leading zero after minus is rejected.
    #[test]
    fn decode_rejects_leading_zero_after_minus() {
        let err = decode(b"i-03e").unwrap_err();
        assert!(matches!(err, DecodeError::MalformedInt { .. }));
    }

    /// Nested dict inside list round-trips.
    #[test]
    fn round_trip_nested_dict_in_list() {
        let val = BencodeValue::List(vec![
            BencodeValue::Dict(vec![(b"x".to_vec(), BencodeValue::Int(1))]),
            BencodeValue::Dict(vec![(b"y".to_vec(), BencodeValue::Int(2))]),
        ]);
        let encoded = encode(&val);
        let decoded = decode(&encoded).unwrap();
        assert_eq!(val, decoded);
    }

    /// Binary data (non-UTF8) round-trips correctly.
    ///
    /// Bencode byte strings are arbitrary binary — not limited to UTF-8.
    #[test]
    fn round_trip_binary_data() {
        let binary = (0u8..=255).collect::<Vec<u8>>();
        let val = BencodeValue::Bytes(binary.clone());
        let encoded = encode(&val);
        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded.as_bytes(), Some(binary.as_slice()));
    }

    /// `encode_into` appends to an existing buffer.
    #[test]
    fn encode_into_appends() {
        let mut buf = b"prefix".to_vec();
        encode_into(&BencodeValue::Int(7), &mut buf);
        assert_eq!(buf, b"prefixi7e");
    }
}
