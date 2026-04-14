// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! HTTP web seed peer — implements BEP 19 piece fetching via Range requests.
//!
//! Each `WebSeedPeer` wraps a single HTTP URL that serves the complete archive
//! file. The coordinator calls `fetch_piece()` with a piece index, file offset,
//! and length; the peer translates this into an HTTP GET request with a
//! `Range: bytes=start-end` header.
//!
//! ## BEP 19 semantics
//!
//! From the BitTorrent perspective, a web seed is a peer that:
//! - Always has 100% of all pieces (the HTTP server has the complete file)
//! - Is never choked (HTTP servers don't use the BT choking mechanism)
//! - Supports random-access reads via HTTP Range requests
//!
//! This makes web seeds ideal bootstraps for a swarm: User A downloads from
//! web seeds while simultaneously sharing pieces to User B via BT. The swarm
//! grows organically even when only HTTP mirrors exist.
//!
//! ## Speed estimation
//!
//! The peer tracks a rolling average of download speed across recent fetches.
//! This lets the coordinator prefer faster mirrors when multiple web seeds are
//! available. The speed estimate starts at 0 (unknown) and converges after the
//! first completed piece.

use std::io::Read;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use super::{Peer, PeerError, PeerKind};

/// Maximum number of HTTP redirects allowed for web seed requests.
///
/// Mirrors the restriction in the downloader's `make_agent()`. CDN redirects
/// (e.g. GitHub Releases → objects.githubusercontent.com) need 1–2 hops;
/// 5 is generous. Limiting redirects reduces the window for redirect-chain
/// SSRF attacks should a mirror be compromised.
pub(super) const MAX_REDIRECTS: u32 = 5;

/// An HTTP web seed peer implementing BEP 19 piece fetching.
///
/// Wraps a single URL that serves the complete archive file. Thread-safe:
/// the coordinator may call `fetch_piece` from multiple threads (though the
/// current sequential implementation calls it from one thread at a time).
pub struct WebSeedPeer {
    /// The HTTP URL serving the complete file. Used as the base for Range requests.
    url: String,
    /// Rolling exponential-moving-average download speed in bytes/sec.
    /// Updated after each successful piece fetch.
    speed_bytes_per_sec: AtomicU64,
}

impl WebSeedPeer {
    /// Creates a new web seed peer for the given URL.
    ///
    /// The URL must point to the complete archive file (ZIP, ISO, etc.).
    /// The peer will use HTTP Range requests to fetch individual pieces.
    pub fn new(url: String) -> Self {
        Self {
            url,
            speed_bytes_per_sec: AtomicU64::new(0),
        }
    }

    /// Returns the URL this peer is fetching from.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Updates the speed estimate using exponential moving average.
    ///
    /// `alpha = 0.3` — recent measurements have 30% weight, history has 70%.
    /// This smooths out jitter while still responding to speed changes.
    fn update_speed(&self, bytes: u64, elapsed_secs: f64) {
        if elapsed_secs <= 0.0 {
            return;
        }
        let measured = (bytes as f64 / elapsed_secs) as u64;
        let current = self.speed_bytes_per_sec.load(Ordering::Relaxed);
        let new_estimate = if current == 0 {
            // First measurement — use it directly.
            measured
        } else {
            // EMA: new = alpha * measured + (1 - alpha) * current
            // Using integer math with alpha = 3/10 to avoid floating point.
            measured
                .saturating_mul(3)
                .saturating_add(current.saturating_mul(7))
                / 10
        };
        self.speed_bytes_per_sec
            .store(new_estimate, Ordering::Relaxed);
    }
}

impl Peer for WebSeedPeer {
    fn kind(&self) -> PeerKind {
        PeerKind::WebSeed
    }

    /// Web seeds always have all pieces — the HTTP server hosts the complete file.
    fn has_piece(&self, _piece_index: u32) -> bool {
        true
    }

    /// Web seeds are never choked — HTTP servers accept requests unconditionally.
    fn is_choked(&self) -> bool {
        false
    }

    /// Fetches a piece via HTTP GET with a Range header.
    ///
    /// Sends `Range: bytes={offset}-{offset+length-1}` and reads the response
    /// body into a `Vec<u8>`. The coordinator will SHA-1 verify the returned
    /// data against the expected piece hash.
    fn fetch_piece(
        &self,
        piece_index: u32,
        offset: u64,
        length: u32,
    ) -> Result<Vec<u8>, PeerError> {
        let range_end = offset.saturating_add(length as u64).saturating_sub(1);
        let range_header = format!("bytes={offset}-{range_end}");

        let start = Instant::now();

        // ── HTTP Range request ──────────────────────────────────────
        //
        // Security hardening mirrors downloader::mirror::make_agent():
        // - `https_only(true)` prevents HTTP-downgrade on redirect. All mirrors
        //   use HTTPS; a redirect to plain HTTP indicates compromise.
        // - `max_redirects(MAX_REDIRECTS)` limits redirect-chain length to reduce
        //   SSRF attack surface should a trusted mirror be compromised.
        //
        // The timeout is set per the CNC_DOWNLOAD_TIMEOUT env var (default 300s).
        let timeout_secs: u64 = std::env::var("CNC_DOWNLOAD_TIMEOUT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(300);

        let agent = ureq::config::Config::builder()
            .timeout_global(Some(std::time::Duration::from_secs(timeout_secs)))
            .max_redirects(MAX_REDIRECTS)
            .https_only(true)
            .build()
            .new_agent();

        let response = agent
            .get(&self.url)
            .header("Range", &range_header)
            .call()
            .map_err(|e| PeerError::Http {
                piece_index,
                url: self.url.clone(),
                detail: e.to_string(),
            })?;

        // ── Validate HTTP 206 Partial Content ───────────────────────
        //
        // A Range request MUST return 206 Partial Content. If the server
        // returns 200 (full file) or any other status, the response body
        // does NOT correspond to the requested byte range — using it would
        // produce a piece with wrong data. Reject early to avoid a
        // misleading SHA-1 mismatch on verification.
        let status = response.status().as_u16();
        if status != 206 {
            return Err(PeerError::Http {
                piece_index,
                url: self.url.clone(),
                detail: format!("expected HTTP 206 Partial Content, got {status}"),
            });
        }

        // ── Read response body ──────────────────────────────────────
        //
        // Read into a pre-allocated buffer. The response should be exactly
        // `length` bytes (HTTP 206 Partial Content), verified above.
        let mut body = Vec::with_capacity(length as usize);
        response
            .into_body()
            .into_reader()
            .take(length as u64)
            .read_to_end(&mut body)
            .map_err(|e| PeerError::Http {
                piece_index,
                url: self.url.clone(),
                detail: format!("error reading response body: {e}"),
            })?;

        let elapsed = start.elapsed().as_secs_f64();
        self.update_speed(body.len() as u64, elapsed);

        Ok(body)
    }

    fn speed_estimate(&self) -> u64 {
        self.speed_bytes_per_sec.load(Ordering::Relaxed)
    }
}
