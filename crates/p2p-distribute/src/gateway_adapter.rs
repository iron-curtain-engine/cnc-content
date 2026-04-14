// SPDX-License-Identifier: MIT OR Apache-2.0

//! Gateway adapter — wires HTTP requests to a [`StreamingReader`] for
//! serving P2P-backed content over plain HTTP.
//!
//! ## What
//!
//! `GatewayAdapter` bridges the gap between the HTTP-level abstractions
//! in [`gateway`](crate::gateway) (range parsing, response metadata) and
//! the P2P-level [`StreamingReader`] (seek + read over partially-
//! downloaded content).  Given an HTTP `Range` header (or none), it
//! produces response metadata and reads the requested bytes from the
//! streaming reader.
//!
//! ## Why
//!
//! The lib.rs roadmap describes an "HTTP-to-P2P gateway" that exposes
//! swarm-backed content as ordinary HTTP responses.  This module is the
//! pure-logic core of that gateway — no async runtime dependency, no
//! HTTP server framework.  A thin adapter crate (`axum`, `hyper`, etc.)
//! calls `serve_range` and writes the returned bytes + headers to the
//! HTTP response.
//!
//! ## How
//!
//! 1. Caller constructs a `GatewayAdapter` wrapping a `StreamingReader`.
//! 2. For each HTTP request, caller extracts the `Range` header and calls
//!    `serve_range(header)`.
//! 3. The adapter parses and resolves the range, seeks the reader, reads
//!    the bytes, and returns `GatewayResponse` (metadata + data).
//! 4. Caller writes the response status, headers, and body to the HTTP
//!    connection.

use std::io::{Read, Seek, SeekFrom};

use crate::gateway::{ContentSlice, RangeRequest, ResponseMeta};
use crate::reader::StreamingReader;

// ── Constants ────────────────────────────────────────────────────────

/// Maximum single read size (16 MiB) to prevent unbounded allocation.
///
/// HTTP clients requesting very large ranges are served in chunks by the
/// HTTP server framework.  The adapter caps individual reads to prevent
/// a single `serve_range` call from allocating gigabytes.
const MAX_READ_SIZE: u64 = 16 * 1024 * 1024;

// ── Error ────────────────────────────────────────────────────────────

/// Errors from gateway adapter operations.
#[derive(Debug, thiserror::Error)]
pub enum GatewayAdapterError {
    /// The HTTP Range header could not be parsed or resolved.
    #[error("range error: {source}")]
    Range {
        #[from]
        source: crate::gateway::RangeError,
    },

    /// I/O error during seek or read on the streaming reader.
    #[error("reader I/O error at position {position}: {detail}")]
    ReaderIo {
        /// File position where the error occurred.
        position: u64,
        /// Human-readable error detail.
        detail: String,
    },
}

// ── GatewayResponse ──────────────────────────────────────────────────

/// HTTP response produced by [`GatewayAdapter::serve_range`].
///
/// Contains the response metadata (status, headers) and the body bytes.
/// The caller writes these to the HTTP response.
#[derive(Debug)]
pub struct GatewayResponse {
    /// HTTP response metadata (status code, Content-Length, Content-Range).
    pub meta: ResponseMeta,

    /// Body bytes for this response chunk.
    pub body: Vec<u8>,
}

// ── GatewayAdapter ───────────────────────────────────────────────────

/// Bridges HTTP requests to a P2P-backed [`StreamingReader`].
///
/// This is the pure-logic core of the HTTP-to-P2P gateway.  It owns a
/// `StreamingReader` and translates HTTP Range requests into seek+read
/// operations, returning structured response data that a thin HTTP
/// server adapter writes to the wire.
///
/// ## Thread Safety
///
/// `GatewayAdapter` is **not** `Send` or `Sync` because `StreamingReader`
/// holds a `std::fs::File` handle with mutable position state.  Each
/// HTTP connection should own its own adapter instance, or the caller
/// should wrap it in appropriate synchronisation.
pub struct GatewayAdapter {
    /// The streaming reader providing Read + Seek over P2P content.
    reader: StreamingReader,
}

impl GatewayAdapter {
    /// Creates a new gateway adapter wrapping the given streaming reader.
    pub fn new(reader: StreamingReader) -> Self {
        Self { reader }
    }

    /// Returns the total content length (file size).
    pub fn content_length(&self) -> u64 {
        self.reader.file_size()
    }

    /// Returns `true` if the reader has enough buffered data for playback.
    pub fn is_ready(&self) -> bool {
        self.reader.is_playback_ready()
    }

    /// Serves an HTTP request, optionally with a `Range` header.
    ///
    /// - If `range_header` is `None`, serves the full content with 200.
    /// - If `range_header` is `Some`, parses and resolves the range,
    ///   returns a 206 partial response.
    ///
    /// The returned `GatewayResponse` contains both the HTTP metadata
    /// and the body bytes.  The body is capped at [`MAX_READ_SIZE`] to
    /// prevent unbounded allocation — the HTTP server should call this
    /// in a loop for very large ranges.
    pub fn serve_range(
        &mut self,
        range_header: Option<&str>,
    ) -> Result<GatewayResponse, GatewayAdapterError> {
        let file_size = self.reader.file_size();

        let (meta, slice) = match range_header {
            None => {
                // Full content response (200 OK).
                let meta = ResponseMeta::full(file_size);
                let slice = ContentSlice {
                    start: 0,
                    end_exclusive: file_size.min(MAX_READ_SIZE),
                    content_length: file_size,
                };
                (meta, slice)
            }
            Some(header) => {
                // Parse and resolve the Range header.
                let request = RangeRequest::parse(header)?;
                let resolved = request.resolve(file_size)?;

                // Cap the read size.
                let capped = ContentSlice {
                    start: resolved.start,
                    end_exclusive: resolved
                        .start
                        .saturating_add(MAX_READ_SIZE)
                        .min(resolved.end_exclusive),
                    content_length: resolved.content_length,
                };

                let meta = ResponseMeta::partial(&resolved);
                (meta, capped)
            }
        };

        // Seek to the start position and read the requested bytes.
        let read_len = slice.end_exclusive.saturating_sub(slice.start);
        self.reader
            .seek(SeekFrom::Start(slice.start))
            .map_err(|e| GatewayAdapterError::ReaderIo {
                position: slice.start,
                detail: e.to_string(),
            })?;

        let mut body = vec![0u8; read_len as usize];
        let bytes_read =
            self.reader
                .read(&mut body)
                .map_err(|e| GatewayAdapterError::ReaderIo {
                    position: slice.start,
                    detail: e.to_string(),
                })?;

        // Truncate to actual bytes read (may be less at EOF).
        body.truncate(bytes_read);

        Ok(GatewayResponse { meta, body })
    }

    /// Returns a reference to the underlying streaming reader.
    pub fn reader(&self) -> &StreamingReader {
        &self.reader
    }

    /// Returns a mutable reference to the underlying streaming reader.
    pub fn reader_mut(&mut self) -> &mut StreamingReader {
        &mut self.reader
    }
}

impl std::fmt::Debug for GatewayAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GatewayAdapter")
            .field("file_size", &self.reader.file_size())
            .field("position", &self.reader.position())
            .finish()
    }
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Creates a temporary file and a GatewayAdapter over it.
    fn make_adapter(content: &[u8]) -> (GatewayAdapter, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_content.bin");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            f.write_all(content).unwrap();
        }
        let reader = StreamingReader::from_complete_file(&path).unwrap();
        (GatewayAdapter::new(reader), dir)
    }

    // ── Full response ────────────────────────────────────────────────

    /// Serving without a Range header returns 200 with full content.
    #[test]
    fn serve_full_response() {
        let data = b"Hello, gateway world!";
        let (mut adapter, _dir) = make_adapter(data);

        let resp = adapter.serve_range(None).unwrap();
        assert_eq!(resp.meta.status, 200);
        assert_eq!(resp.meta.content_length, data.len() as u64);
        assert!(resp.meta.content_range.is_none());
        assert_eq!(resp.body, data);
    }

    // ── Partial responses ────────────────────────────────────────────

    /// Bounded range returns 206 with the correct slice.
    #[test]
    fn serve_bounded_range() {
        let data = b"0123456789ABCDEF";
        let (mut adapter, _dir) = make_adapter(data);

        let resp = adapter.serve_range(Some("bytes=4-7")).unwrap();
        assert_eq!(resp.meta.status, 206);
        assert_eq!(resp.body, b"4567");
    }

    /// Open-ended range returns bytes from start to EOF.
    #[test]
    fn serve_open_ended_range() {
        let data = b"ABCDEFGHIJ";
        let (mut adapter, _dir) = make_adapter(data);

        let resp = adapter.serve_range(Some("bytes=7-")).unwrap();
        assert_eq!(resp.meta.status, 206);
        assert_eq!(resp.body, b"HIJ");
    }

    /// Suffix range returns last N bytes.
    #[test]
    fn serve_suffix_range() {
        let data = b"ABCDEFGHIJ";
        let (mut adapter, _dir) = make_adapter(data);

        let resp = adapter.serve_range(Some("bytes=-3")).unwrap();
        assert_eq!(resp.meta.status, 206);
        assert_eq!(resp.body, b"HIJ");
    }

    // ── Error handling ───────────────────────────────────────────────

    /// Invalid range header returns a range error.
    #[test]
    fn serve_invalid_range_header() {
        let (mut adapter, _dir) = make_adapter(b"data");
        let err = adapter.serve_range(Some("invalid")).unwrap_err();
        assert!(matches!(err, GatewayAdapterError::Range { .. }));
    }

    /// Range starting beyond content length returns error.
    #[test]
    fn serve_range_beyond_eof() {
        let data = b"short";
        let (mut adapter, _dir) = make_adapter(data);

        let err = adapter.serve_range(Some("bytes=100-200")).unwrap_err();
        assert!(matches!(err, GatewayAdapterError::Range { .. }));
    }

    // ── Metadata ─────────────────────────────────────────────────────

    /// Content length reports the file size.
    #[test]
    fn content_length_matches_file() {
        let data = b"exactly 30 bytes of test data.";
        let (adapter, _dir) = make_adapter(data);
        assert_eq!(adapter.content_length(), data.len() as u64);
    }

    /// A complete file is always ready.
    #[test]
    fn complete_file_is_ready() {
        let (adapter, _dir) = make_adapter(b"ready");
        assert!(adapter.is_ready());
    }

    // ── Sequential reads ─────────────────────────────────────────────

    /// Multiple sequential range requests work correctly.
    #[test]
    fn sequential_range_requests() {
        let data: Vec<u8> = (0..=255u8).collect();
        let (mut adapter, _dir) = make_adapter(&data);

        let r1 = adapter.serve_range(Some("bytes=0-9")).unwrap();
        assert_eq!(r1.body, &data[0..10]);

        let r2 = adapter.serve_range(Some("bytes=10-19")).unwrap();
        assert_eq!(r2.body, &data[10..20]);

        let r3 = adapter.serve_range(Some("bytes=250-")).unwrap();
        assert_eq!(r3.body, &data[250..]);
    }

    // ── Debug impl ───────────────────────────────────────────────────

    /// Debug output includes file_size and position.
    #[test]
    fn gateway_adapter_debug() {
        let (adapter, _dir) = make_adapter(b"test");
        let dbg = format!("{adapter:?}");
        assert!(dbg.contains("GatewayAdapter"));
        assert!(dbg.contains("file_size"));
    }

    // ── Error display ────────────────────────────────────────────────

    /// Error display messages include context.
    #[test]
    fn error_display_context() {
        let err = GatewayAdapterError::ReaderIo {
            position: 42,
            detail: "broken pipe".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("42"));
        assert!(msg.contains("broken pipe"));
    }

    /// Empty file can be served.
    #[test]
    fn serve_empty_file() {
        let (mut adapter, _dir) = make_adapter(b"");
        let resp = adapter.serve_range(None).unwrap();
        assert_eq!(resp.meta.status, 200);
        assert_eq!(resp.meta.content_length, 0);
        assert!(resp.body.is_empty());
    }
}
