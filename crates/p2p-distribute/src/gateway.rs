// SPDX-License-Identifier: MIT OR Apache-2.0

//! HTTP-to-P2P gateway — translates HTTP Range requests into piece-level
//! reads from a [`StreamingReader`].
//!
//! ## What
//!
//! Provides abstractions for exposing P2P-backed content over a standard
//! HTTP endpoint. The downloading client sees an ordinary HTTP response
//! with `Content-Length` / `Content-Range` headers; behind the scenes,
//! the gateway streams bytes from a [`StreamingReader`] that fetches
//! pieces via the P2P swarm on demand.
//!
//! ## Why
//!
//! This turns the P2P layer into a distributed storage backend with a
//! predictable HTTP download experience for end users who neither know
//! nor care that the data is assembled from multiple peers.
//!
//! ## How
//!
//! - [`RangeRequest`] — parses and validates HTTP `Range` header values.
//! - [`ResponseMeta`] — generates HTTP response metadata (status code,
//!   content-length, content-range) for a given range.
//! - [`ContentSlice`] — represents a resolved byte range within a
//!   content file, ready for streaming.
//!
//! The actual HTTP server (hyper/axum/actix) is **not** in this crate.
//! A thin adapter crate wires incoming HTTP requests through these types
//! to a [`StreamingReader`].
//!
//! [`StreamingReader`]: crate::reader::StreamingReader

use thiserror::Error;

// ── Errors ──────────────────────────────────────────────────────────

/// Errors from HTTP range parsing and resolution.
#[derive(Debug, Error)]
pub enum RangeError {
    #[error("invalid range syntax: {detail}")]
    InvalidSyntax { detail: String },
    #[error("range start {start} exceeds content length {content_length}")]
    StartBeyondEnd { start: u64, content_length: u64 },
    #[error("range start {start} is greater than end {end}")]
    StartGreaterThanEnd { start: u64, end: u64 },
    #[error("unsupported range unit: {unit} (only 'bytes' is supported)")]
    UnsupportedUnit { unit: String },
}

// ── RangeRequest ────────────────────────────────────────────────────

/// A parsed HTTP `Range` header value.
///
/// Supports the three forms defined in RFC 7233:
///
/// - `bytes=start-end` — explicit start and end (inclusive).
/// - `bytes=start-` — from start to end of file.
/// - `bytes=-suffix` — last N bytes of the file.
///
/// ```
/// use p2p_distribute::gateway::RangeRequest;
///
/// // Full range
/// let r = RangeRequest::parse("bytes=0-999").unwrap();
/// let slice = r.resolve(10_000).unwrap();
/// assert_eq!(slice.start, 0);
/// assert_eq!(slice.end_exclusive, 1000);
///
/// // Open-ended
/// let r = RangeRequest::parse("bytes=500-").unwrap();
/// let slice = r.resolve(10_000).unwrap();
/// assert_eq!(slice.start, 500);
/// assert_eq!(slice.end_exclusive, 10_000);
///
/// // Suffix
/// let r = RangeRequest::parse("bytes=-200").unwrap();
/// let slice = r.resolve(10_000).unwrap();
/// assert_eq!(slice.start, 9_800);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RangeRequest {
    /// `bytes=start-end` (inclusive end per RFC 7233).
    Bounded { start: u64, end_inclusive: u64 },
    /// `bytes=start-` (open-ended, to end of file).
    OpenEnded { start: u64 },
    /// `bytes=-suffix` (last N bytes).
    Suffix { length: u64 },
}

impl RangeRequest {
    /// Parses an HTTP `Range` header value.
    ///
    /// Expects the format `bytes=<range>` where `<range>` is one of:
    /// `start-end`, `start-`, or `-suffix`.
    ///
    /// Only single ranges are supported; multi-range (`bytes=0-10, 20-30`)
    /// is rejected.
    pub fn parse(header: &str) -> Result<Self, RangeError> {
        // Strip and validate unit prefix.
        let rest = header
            .strip_prefix("bytes=")
            .ok_or_else(|| {
                // Check if it's a different unit.
                if let Some(eq_pos) = header.find('=') {
                    let unit = header.get(..eq_pos).unwrap_or(header);
                    RangeError::UnsupportedUnit {
                        unit: unit.to_string(),
                    }
                } else {
                    RangeError::InvalidSyntax {
                        detail: "missing 'bytes=' prefix".into(),
                    }
                }
            })?
            .trim();

        // Reject multi-range.
        if rest.contains(',') {
            return Err(RangeError::InvalidSyntax {
                detail: "multi-range not supported".into(),
            });
        }

        // Find the dash separator.
        let dash_pos = rest.find('-').ok_or(RangeError::InvalidSyntax {
            detail: "missing '-' separator".into(),
        })?;

        let left = rest.get(..dash_pos).unwrap_or("");
        let right = rest.get(dash_pos.saturating_add(1)..).unwrap_or("");

        // Suffix range: `-NNN`
        if left.is_empty() {
            let length = right
                .parse::<u64>()
                .map_err(|_| RangeError::InvalidSyntax {
                    detail: format!("invalid suffix length: '{right}'"),
                })?;
            return Ok(Self::Suffix { length });
        }

        let start = left.parse::<u64>().map_err(|_| RangeError::InvalidSyntax {
            detail: format!("invalid start: '{left}'"),
        })?;

        // Open-ended: `NNN-`
        if right.is_empty() {
            return Ok(Self::OpenEnded { start });
        }

        // Bounded: `NNN-MMM`
        let end = right
            .parse::<u64>()
            .map_err(|_| RangeError::InvalidSyntax {
                detail: format!("invalid end: '{right}'"),
            })?;

        if start > end {
            return Err(RangeError::StartGreaterThanEnd { start, end });
        }

        Ok(Self::Bounded {
            start,
            end_inclusive: end,
        })
    }

    /// Resolves this range against a known content length, producing a
    /// concrete byte slice.
    ///
    /// According to RFC 7233, if the range is satisfiable but extends
    /// beyond the content, it is clamped to the content length.
    pub fn resolve(&self, content_length: u64) -> Result<ContentSlice, RangeError> {
        match *self {
            Self::Bounded {
                start,
                end_inclusive,
            } => {
                if start >= content_length {
                    return Err(RangeError::StartBeyondEnd {
                        start,
                        content_length,
                    });
                }
                // Clamp end to content length (RFC 7233 §2.1).
                let clamped_end = end_inclusive.saturating_add(1).min(content_length);
                Ok(ContentSlice {
                    start,
                    end_exclusive: clamped_end,
                    content_length,
                })
            }
            Self::OpenEnded { start } => {
                if start >= content_length {
                    return Err(RangeError::StartBeyondEnd {
                        start,
                        content_length,
                    });
                }
                Ok(ContentSlice {
                    start,
                    end_exclusive: content_length,
                    content_length,
                })
            }
            Self::Suffix { length } => {
                let start = content_length.saturating_sub(length);
                Ok(ContentSlice {
                    start,
                    end_exclusive: content_length,
                    content_length,
                })
            }
        }
    }
}

// ── ContentSlice ────────────────────────────────────────────────────

/// A resolved byte range within a content file.
///
/// Produced by [`RangeRequest::resolve`] after validating against the
/// content length. Ready for `seek(start)` + `read(len)` on a
/// [`StreamingReader`].
///
/// [`StreamingReader`]: crate::reader::StreamingReader
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentSlice {
    /// Start byte offset (inclusive).
    pub start: u64,
    /// End byte offset (exclusive).
    pub end_exclusive: u64,
    /// Total content length (for response headers).
    pub content_length: u64,
}

impl ContentSlice {
    /// Returns the number of bytes in this slice.
    pub fn len(&self) -> u64 {
        self.end_exclusive.saturating_sub(self.start)
    }

    /// Whether the slice is empty (zero bytes).
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

// ── ResponseMeta ────────────────────────────────────────────────────

/// HTTP response metadata for a content request.
///
/// Encapsulates the status code and headers needed to serve a range
/// response. The gateway adapter uses this to build the actual HTTP
/// response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponseMeta {
    /// HTTP status code (200 for full content, 206 for partial).
    pub status: u16,
    /// Value of the `Content-Length` header (bytes in this response).
    pub content_length: u64,
    /// Value of the `Content-Range` header (only for 206 responses).
    ///
    /// Format: `bytes start-end/total`.
    pub content_range: Option<String>,
}

impl ResponseMeta {
    /// Builds response metadata for a full-content response (200 OK).
    pub fn full(content_length: u64) -> Self {
        Self {
            status: 200,
            content_length,
            content_range: None,
        }
    }

    /// Builds response metadata for a partial-content response (206).
    pub fn partial(slice: &ContentSlice) -> Self {
        let range_str = format!(
            "bytes {}-{}/{}",
            slice.start,
            slice.end_exclusive.saturating_sub(1),
            slice.content_length
        );
        Self {
            status: 206,
            content_length: slice.len(),
            content_range: Some(range_str),
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── RangeRequest parsing ────────────────────────────────────────

    /// Bounded range `bytes=0-499` parsed correctly.
    #[test]
    fn parse_bounded() {
        let r = RangeRequest::parse("bytes=0-499").unwrap();
        assert_eq!(
            r,
            RangeRequest::Bounded {
                start: 0,
                end_inclusive: 499,
            }
        );
    }

    /// Open-ended range `bytes=500-` parsed correctly.
    #[test]
    fn parse_open_ended() {
        let r = RangeRequest::parse("bytes=500-").unwrap();
        assert_eq!(r, RangeRequest::OpenEnded { start: 500 });
    }

    /// Suffix range `bytes=-200` parsed correctly.
    #[test]
    fn parse_suffix() {
        let r = RangeRequest::parse("bytes=-200").unwrap();
        assert_eq!(r, RangeRequest::Suffix { length: 200 });
    }

    /// Whitespace around the range spec is tolerated.
    #[test]
    fn parse_whitespace_trimmed() {
        let r = RangeRequest::parse("bytes= 100-200 ").unwrap();
        assert_eq!(
            r,
            RangeRequest::Bounded {
                start: 100,
                end_inclusive: 200,
            }
        );
    }

    /// Missing `bytes=` prefix is rejected.
    #[test]
    fn parse_missing_prefix() {
        let err = RangeRequest::parse("0-499").unwrap_err();
        assert!(matches!(err, RangeError::InvalidSyntax { .. }));
    }

    /// Unsupported unit is rejected with specific error.
    #[test]
    fn parse_unsupported_unit() {
        let err = RangeRequest::parse("items=0-10").unwrap_err();
        assert!(matches!(err, RangeError::UnsupportedUnit { .. }));
    }

    /// Multi-range is rejected.
    #[test]
    fn parse_multi_range_rejected() {
        let err = RangeRequest::parse("bytes=0-100, 200-300").unwrap_err();
        assert!(matches!(err, RangeError::InvalidSyntax { .. }));
    }

    /// Start greater than end is rejected.
    #[test]
    fn parse_start_greater_than_end() {
        let err = RangeRequest::parse("bytes=500-100").unwrap_err();
        assert!(matches!(err, RangeError::StartGreaterThanEnd { .. }));
    }

    /// Invalid numeric values are rejected.
    #[test]
    fn parse_invalid_numbers() {
        let err = RangeRequest::parse("bytes=abc-def").unwrap_err();
        assert!(matches!(err, RangeError::InvalidSyntax { .. }));
    }

    // ── Range resolution ────────────────────────────────────────────

    /// Bounded range resolves to correct slice.
    #[test]
    fn resolve_bounded() {
        let r = RangeRequest::Bounded {
            start: 100,
            end_inclusive: 199,
        };
        let slice = r.resolve(10_000).unwrap();
        assert_eq!(slice.start, 100);
        assert_eq!(slice.end_exclusive, 200);
        assert_eq!(slice.len(), 100);
    }

    /// Bounded range is clamped to content length.
    ///
    /// RFC 7233 §2.1: if the end exceeds content length, clamp it.
    #[test]
    fn resolve_bounded_clamped() {
        let r = RangeRequest::Bounded {
            start: 0,
            end_inclusive: 99999,
        };
        let slice = r.resolve(500).unwrap();
        assert_eq!(slice.end_exclusive, 500);
        assert_eq!(slice.len(), 500);
    }

    /// Open-ended range resolves to end of file.
    #[test]
    fn resolve_open_ended() {
        let r = RangeRequest::OpenEnded { start: 8000 };
        let slice = r.resolve(10_000).unwrap();
        assert_eq!(slice.start, 8000);
        assert_eq!(slice.end_exclusive, 10_000);
        assert_eq!(slice.len(), 2000);
    }

    /// Suffix range resolves from the end.
    #[test]
    fn resolve_suffix() {
        let r = RangeRequest::Suffix { length: 500 };
        let slice = r.resolve(10_000).unwrap();
        assert_eq!(slice.start, 9_500);
        assert_eq!(slice.end_exclusive, 10_000);
        assert_eq!(slice.len(), 500);
    }

    /// Suffix larger than content returns entire file.
    #[test]
    fn resolve_suffix_larger_than_content() {
        let r = RangeRequest::Suffix { length: 50_000 };
        let slice = r.resolve(1_000).unwrap();
        assert_eq!(slice.start, 0);
        assert_eq!(slice.len(), 1_000);
    }

    /// Start beyond content length is an error.
    #[test]
    fn resolve_start_beyond_end() {
        let r = RangeRequest::OpenEnded { start: 10_000 };
        let err = r.resolve(5_000).unwrap_err();
        assert!(matches!(err, RangeError::StartBeyondEnd { .. }));
    }

    // ── ContentSlice ────────────────────────────────────────────────

    /// Empty slice has zero length.
    #[test]
    fn content_slice_empty() {
        let slice = ContentSlice {
            start: 100,
            end_exclusive: 100,
            content_length: 1000,
        };
        assert!(slice.is_empty());
        assert_eq!(slice.len(), 0);
    }

    // ── ResponseMeta ────────────────────────────────────────────────

    /// Full response is 200 with no Content-Range.
    #[test]
    fn response_meta_full() {
        let meta = ResponseMeta::full(5_000);
        assert_eq!(meta.status, 200);
        assert_eq!(meta.content_length, 5_000);
        assert!(meta.content_range.is_none());
    }

    /// Partial response is 206 with Content-Range header.
    #[test]
    fn response_meta_partial() {
        let slice = ContentSlice {
            start: 100,
            end_exclusive: 600,
            content_length: 10_000,
        };
        let meta = ResponseMeta::partial(&slice);
        assert_eq!(meta.status, 206);
        assert_eq!(meta.content_length, 500);
        assert_eq!(meta.content_range.as_deref(), Some("bytes 100-599/10000"));
    }

    // ── Error display ───────────────────────────────────────────────

    /// Error messages contain context.
    #[test]
    fn error_display_context() {
        let err = RangeError::StartBeyondEnd {
            start: 5000,
            content_length: 1000,
        };
        let msg = err.to_string();
        assert!(msg.contains("5000"), "should contain start: {msg}");
        assert!(msg.contains("1000"), "should contain length: {msg}");
    }

    /// UnsupportedUnit shows the unit.
    #[test]
    fn error_display_unsupported_unit() {
        let err = RangeError::UnsupportedUnit {
            unit: "pages".into(),
        };
        assert!(err.to_string().contains("pages"));
    }
}
