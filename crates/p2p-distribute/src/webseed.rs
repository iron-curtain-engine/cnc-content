// SPDX-License-Identifier: MIT OR Apache-2.0

//! HTTP web seed peer — fetches pieces via Range requests (BEP 19).
//!
//! A web seed is an HTTP server that hosts the complete file. The coordinator
//! fetches individual pieces by requesting byte ranges. This is the fastest
//! transport for new content with no existing swarm peers.
//!
//! ## Speed tracking
//!
//! Each web seed tracks its download speed using an exponential moving average
//! (EMA). The coordinator uses this to prefer faster web seeds when multiple
//! are available.

use std::io::Read;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::peer::{Peer, PeerError, PeerKind};

/// Default HTTP request timeout in seconds.
const DEFAULT_TIMEOUT_SECS: u64 = 300;

/// HTTP web seed peer — fetches pieces via Range requests.
///
/// Implements [`Peer`] by building HTTP Range requests for each piece.
/// Speed is tracked via exponential moving average for peer selection.
pub struct WebSeedPeer {
    /// Full URL to the file (e.g. `https://example.com/content.zip`).
    url: String,
    /// EMA of download speed in bytes/sec, updated after each piece fetch.
    speed_bytes_per_sec: AtomicU64,
    /// HTTP request timeout.
    timeout: Duration,
}

impl WebSeedPeer {
    /// Creates a new web seed peer pointing at the given URL.
    ///
    /// Uses the default timeout of 300 seconds. Use [`with_timeout`](Self::with_timeout)
    /// to customise.
    pub fn new(url: String) -> Self {
        Self {
            url,
            speed_bytes_per_sec: AtomicU64::new(0),
            timeout: Duration::from_secs(DEFAULT_TIMEOUT_SECS),
        }
    }

    /// Creates a new web seed peer with a custom timeout.
    pub fn with_timeout(url: String, timeout: Duration) -> Self {
        Self {
            url,
            speed_bytes_per_sec: AtomicU64::new(0),
            timeout,
        }
    }

    /// Returns the URL of this web seed.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Updates the speed estimate using exponential moving average (EMA).
    ///
    /// Alpha = 0.3 — recent measurements have 30% weight. This smooths out
    /// jitter from individual piece downloads while still adapting to
    /// changing network conditions.
    fn update_speed(&self, bytes: u64, elapsed_secs: f64) {
        if elapsed_secs <= 0.0 {
            return;
        }
        let measured = (bytes as f64 / elapsed_secs) as u64;
        let current = self.speed_bytes_per_sec.load(Ordering::Relaxed);
        // EMA: new = alpha * measured + (1-alpha) * current
        // With alpha = 0.3 and integer math: (measured*3 + current*7) / 10
        let new_speed = if current == 0 {
            measured
        } else {
            (measured
                .saturating_mul(3)
                .saturating_add(current.saturating_mul(7)))
                / 10
        };
        self.speed_bytes_per_sec.store(new_speed, Ordering::Relaxed);
    }
}

impl Peer for WebSeedPeer {
    fn kind(&self) -> PeerKind {
        PeerKind::WebSeed
    }

    /// Web seeds always have all pieces (they serve the complete file).
    fn has_piece(&self, _piece_index: u32) -> bool {
        true
    }

    /// Web seeds are never choked.
    fn is_choked(&self) -> bool {
        false
    }

    fn fetch_piece(
        &self,
        piece_index: u32,
        offset: u64,
        length: u32,
    ) -> Result<Vec<u8>, PeerError> {
        let start = std::time::Instant::now();

        // ── Build HTTP Range request ────────────────────────────────
        //
        // BEP 19 web seeds serve the complete file. We fetch individual
        // pieces using HTTP Range requests (RFC 7233).
        let end = offset.saturating_add(length as u64).saturating_sub(1);
        let range_header = format!("bytes={offset}-{end}");

        let agent = ureq::config::Config::builder()
            .timeout_global(Some(self.timeout))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peer::{Peer, PeerKind};

    // ── Construction ────────────────────────────────────────────────

    /// `WebSeedPeer::new` stores the URL and sets default timeout.
    ///
    /// Initial speed estimate must be zero — no data has been transferred yet.
    #[test]
    fn new_stores_url_and_defaults() {
        let peer = WebSeedPeer::new("https://example.com/file.zip".into());
        assert_eq!(peer.url(), "https://example.com/file.zip");
        assert_eq!(peer.speed_estimate(), 0);
        assert_eq!(peer.timeout, Duration::from_secs(DEFAULT_TIMEOUT_SECS));
    }

    /// `with_timeout` overrides the default timeout.
    #[test]
    fn with_timeout_overrides_default() {
        let peer = WebSeedPeer::with_timeout(
            "https://example.com/file.zip".into(),
            Duration::from_secs(10),
        );
        assert_eq!(peer.timeout, Duration::from_secs(10));
        assert_eq!(peer.speed_estimate(), 0);
    }

    // ── Peer trait implementation ───────────────────────────────────

    /// Web seed reports `PeerKind::WebSeed`.
    #[test]
    fn kind_is_webseed() {
        let peer = WebSeedPeer::new("https://example.com/f.zip".into());
        assert_eq!(peer.kind(), PeerKind::WebSeed);
    }

    /// Web seeds always have every piece (they serve the complete file).
    ///
    /// `has_piece` must return `true` for any index, including 0 and u32::MAX.
    #[test]
    fn has_piece_always_true() {
        let peer = WebSeedPeer::new("https://example.com/f.zip".into());
        assert!(peer.has_piece(0));
        assert!(peer.has_piece(42));
        assert!(peer.has_piece(u32::MAX));
    }

    /// Web seeds are never choked — HTTP servers don't have choke semantics.
    #[test]
    fn is_choked_always_false() {
        let peer = WebSeedPeer::new("https://example.com/f.zip".into());
        assert!(!peer.is_choked());
    }

    // ── Speed tracking (EMA) ────────────────────────────────────────

    /// First speed sample sets the speed directly (no prior EMA to blend).
    ///
    /// When current speed is 0, the EMA shortcut uses the measured value
    /// directly rather than blending with zero.
    #[test]
    fn first_speed_sample_sets_directly() {
        let peer = WebSeedPeer::new("https://example.com/f.zip".into());
        // 1000 bytes in 1.0 second = 1000 B/s
        peer.update_speed(1000, 1.0);
        assert_eq!(peer.speed_estimate(), 1000);
    }

    /// Second sample blends via EMA: new = (measured*3 + current*7) / 10.
    ///
    /// With alpha = 0.3, the weighting is 30% new measurement / 70% prior.
    #[test]
    fn second_sample_blends_ema() {
        let peer = WebSeedPeer::new("https://example.com/f.zip".into());
        // First: 1000 B/s
        peer.update_speed(1000, 1.0);
        // Second: 2000 bytes in 1 second = 2000 B/s measured
        // EMA: (2000*3 + 1000*7) / 10 = (6000 + 7000) / 10 = 1300
        peer.update_speed(2000, 1.0);
        assert_eq!(peer.speed_estimate(), 1300);
    }

    /// Multiple EMA samples converge toward the measured rate.
    ///
    /// After many identical measurements, the estimate should approach
    /// the true rate asymptotically.
    #[test]
    fn ema_converges_over_many_samples() {
        let peer = WebSeedPeer::new("https://example.com/f.zip".into());
        // Seed with a different initial value
        peer.update_speed(100, 1.0); // 100 B/s
                                     // Then feed 20 samples of 1000 B/s
        for _ in 0..20 {
            peer.update_speed(1000, 1.0);
        }
        let speed = peer.speed_estimate();
        // After 20 iterations at alpha=0.3, should be very close to 1000
        assert!(speed > 950, "expected close to 1000, got {speed}");
        assert!(speed <= 1000, "expected <= 1000, got {speed}");
    }

    /// Zero elapsed time is ignored (prevents division by zero).
    ///
    /// If the timer resolution is too low or the fetch was instant,
    /// `update_speed` must not panic or set speed to infinity/NaN.
    #[test]
    fn zero_elapsed_ignored() {
        let peer = WebSeedPeer::new("https://example.com/f.zip".into());
        peer.update_speed(1000, 1.0); // seed at 1000 B/s
        peer.update_speed(5000, 0.0); // zero elapsed — must be a no-op
        assert_eq!(peer.speed_estimate(), 1000);
    }

    /// Negative elapsed time is ignored (same guard as zero).
    #[test]
    fn negative_elapsed_ignored() {
        let peer = WebSeedPeer::new("https://example.com/f.zip".into());
        peer.update_speed(1000, 1.0);
        peer.update_speed(5000, -1.0);
        assert_eq!(peer.speed_estimate(), 1000);
    }

    /// Zero bytes transferred records zero speed.
    ///
    /// A fetch that returns an empty body (server error) should pull the
    /// EMA toward zero, not leave it unchanged.
    #[test]
    fn zero_bytes_records_zero_speed() {
        let peer = WebSeedPeer::new("https://example.com/f.zip".into());
        peer.update_speed(1000, 1.0); // 1000 B/s
        peer.update_speed(0, 1.0); // 0 bytes in 1 sec = 0 B/s
                                   // EMA: (0*3 + 1000*7) / 10 = 700
        assert_eq!(peer.speed_estimate(), 700);
    }

    /// Huge byte count doesn't overflow the EMA arithmetic.
    ///
    /// `saturating_mul` and `saturating_add` must prevent panic on large values.
    #[test]
    fn huge_bytes_saturates_safely() {
        let peer = WebSeedPeer::new("https://example.com/f.zip".into());
        peer.update_speed(u64::MAX, 1.0);
        let speed = peer.speed_estimate();
        // First sample shortcut: speed = measured = u64::MAX
        assert_eq!(speed, u64::MAX);
        // Second sample with u64::MAX should use saturating math
        peer.update_speed(u64::MAX, 1.0);
        let speed2 = peer.speed_estimate();
        // Should not panic; exact value depends on saturation
        assert!(speed2 > 0);
    }

    /// Very small elapsed time produces a very large (but finite) speed.
    ///
    /// Ensures no infinity or NaN contaminates the integer estimate.
    #[test]
    fn tiny_elapsed_produces_large_speed() {
        let peer = WebSeedPeer::new("https://example.com/f.zip".into());
        peer.update_speed(1_000_000, 0.000001); // 1e12 B/s
        let speed = peer.speed_estimate();
        assert!(speed > 0);
    }

    /// Speed estimate is `Send + Sync` — safe for coordinator's thread pool.
    ///
    /// This is a compile-time check, not a runtime test.
    #[test]
    fn webseed_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<WebSeedPeer>();
    }
}
