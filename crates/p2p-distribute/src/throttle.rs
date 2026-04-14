// SPDX-License-Identifier: MIT OR Apache-2.0

//! Global bandwidth throttle — token-bucket rate limiter for aggregate
//! download/upload speed across all peers.
//!
//! ## What
//!
//! Provides a [`BandwidthThrottle`] that caps total bytes-per-second
//! throughput.  Unlike [`crate::rate_limiter::TokenBucket`] (which
//! limits *request count* per peer), this module limits *byte volume*
//! across all peers combined — like aria2's `--max-download-limit`.
//!
//! ## Why — aria2 global throttle lesson
//!
//! aria2 separates per-connection rate limiting from global rate
//! limiting.  Per-connection ensures fairness; global ensures the P2P
//! traffic doesn't saturate the user's internet link.  BitTorrent
//! clients that lack a global throttle cause latency spikes for other
//! applications on the same network.
//!
//! Key design:
//! - **Token bucket with byte granularity** — each byte consumed
//!   removes one token.  Tokens refill at the configured bytes/sec
//!   rate.  Burst capacity absorbs short spikes.
//! - **Separate download/upload throttles** — most users want
//!   asymmetric limits (e.g. 10 MB/s down, 1 MB/s up).
//! - **Zero means unlimited** — setting the rate to 0 disables the
//!   throttle, equivalent to no limit.
//!
//! ## How
//!
//! 1. Create a `BandwidthThrottle` with a bytes/sec limit.
//! 2. Before transferring data, call `request(byte_count, now)` to
//!    check if enough tokens are available.
//! 3. After transfer, call `consume(byte_count, now)` to deduct tokens.
//! 4. Use `ThrottlePair` for paired download + upload throttles.

use std::time::Instant;

// ── Constants ───────────────────────────────────────────────────────

/// Default burst factor: burst capacity = 2× the per-second rate.
///
/// This allows a 2-second burst at full rate before throttling kicks in.
/// Matches aria2's default burst behaviour.
const DEFAULT_BURST_FACTOR: u64 = 2;

/// Minimum burst size in bytes — ensures small rate limits still
/// allow at least one typical piece request (16 KiB) through.
const MIN_BURST_BYTES: u64 = 16_384;

// ── BandwidthThrottle ───────────────────────────────────────────────

/// Token-bucket bandwidth limiter with byte granularity.
///
/// Tokens represent bytes.  The bucket refills at `rate_bytes_per_sec`
/// and can hold up to `burst_bytes` tokens.  When the bucket is empty,
/// transfers must wait until tokens refill.
///
/// A rate of 0 means unlimited — all requests are granted immediately.
///
/// ```
/// use std::time::Instant;
/// use p2p_distribute::throttle::BandwidthThrottle;
///
/// let now = Instant::now();
/// let mut throttle = BandwidthThrottle::new(1_048_576, now); // 1 MB/s
///
/// // Small request: fits within burst.
/// assert!(throttle.request(4096, now));
/// throttle.consume(4096, now);
/// ```
#[derive(Debug, Clone)]
pub struct BandwidthThrottle {
    /// Maximum throughput in bytes per second.  0 = unlimited.
    rate_bytes_per_sec: u64,
    /// Maximum burst capacity in bytes.
    burst_bytes: u64,
    /// Current available tokens (bytes).  Stored as fractional for
    /// sub-second refill accuracy.
    available: f64,
    /// Timestamp of last refill computation.
    last_refill: Instant,
    /// Cumulative bytes consumed (for metrics).
    total_consumed: u64,
}

impl BandwidthThrottle {
    /// Creates a new throttle with the given bytes/sec rate.
    ///
    /// Burst capacity defaults to `2 × rate` (clamped to at least
    /// [`MIN_BURST_BYTES`]).  A rate of 0 creates an unlimited throttle.
    pub fn new(rate_bytes_per_sec: u64, now: Instant) -> Self {
        let burst_bytes = if rate_bytes_per_sec == 0 {
            0
        } else {
            rate_bytes_per_sec
                .saturating_mul(DEFAULT_BURST_FACTOR)
                .max(MIN_BURST_BYTES)
        };

        Self {
            rate_bytes_per_sec,
            burst_bytes,
            available: burst_bytes as f64,
            last_refill: now,
            total_consumed: 0,
        }
    }

    /// Creates a throttle with explicit burst capacity.
    pub fn with_burst(rate_bytes_per_sec: u64, burst_bytes: u64, now: Instant) -> Self {
        let burst = if rate_bytes_per_sec == 0 {
            0
        } else {
            burst_bytes.max(MIN_BURST_BYTES)
        };

        Self {
            rate_bytes_per_sec,
            burst_bytes: burst,
            available: burst as f64,
            last_refill: now,
            total_consumed: 0,
        }
    }

    /// Returns `true` if `byte_count` bytes can be transferred now.
    ///
    /// This refills the bucket based on elapsed time, then checks if
    /// enough tokens are available.  Does not consume tokens — call
    /// [`consume`](Self::consume) after the actual transfer.
    ///
    /// Always returns `true` for unlimited throttles (rate = 0).
    pub fn request(&mut self, byte_count: u64, now: Instant) -> bool {
        if self.rate_bytes_per_sec == 0 {
            return true;
        }
        self.refill(now);
        self.available >= byte_count as f64
    }

    /// Deducts `byte_count` tokens from the bucket.
    ///
    /// Call this after a successful transfer.  Refills the bucket first
    /// so timing is accurate.  The balance can go negative — subsequent
    /// `request` calls will return `false` until the bucket refills.
    pub fn consume(&mut self, byte_count: u64, now: Instant) {
        if self.rate_bytes_per_sec == 0 {
            self.total_consumed = self.total_consumed.saturating_add(byte_count);
            return;
        }
        self.refill(now);
        self.available -= byte_count as f64;
        self.total_consumed = self.total_consumed.saturating_add(byte_count);
    }

    /// Returns the current rate limit in bytes/sec.  0 = unlimited.
    pub fn rate(&self) -> u64 {
        self.rate_bytes_per_sec
    }

    /// Returns the burst capacity in bytes.
    pub fn burst(&self) -> u64 {
        self.burst_bytes
    }

    /// Returns cumulative bytes consumed through this throttle.
    pub fn total_consumed(&self) -> u64 {
        self.total_consumed
    }

    /// Returns the currently available byte budget.
    ///
    /// May be negative if `consume` was called without checking
    /// `request` first.
    pub fn available(&mut self, now: Instant) -> f64 {
        self.refill(now);
        self.available
    }

    /// Returns `true` if this is an unlimited (rate = 0) throttle.
    pub fn is_unlimited(&self) -> bool {
        self.rate_bytes_per_sec == 0
    }

    /// Updates the rate limit dynamically (e.g. from user settings).
    ///
    /// Recalculates burst capacity and clamps available tokens.
    pub fn set_rate(&mut self, rate_bytes_per_sec: u64, now: Instant) {
        self.refill(now);
        self.rate_bytes_per_sec = rate_bytes_per_sec;

        if rate_bytes_per_sec == 0 {
            self.burst_bytes = 0;
            self.available = 0.0;
        } else {
            self.burst_bytes = rate_bytes_per_sec
                .saturating_mul(DEFAULT_BURST_FACTOR)
                .max(MIN_BURST_BYTES);
            // Clamp available to new burst capacity.
            if self.available > self.burst_bytes as f64 {
                self.available = self.burst_bytes as f64;
            }
        }
    }

    /// Refills tokens based on elapsed time since last refill.
    fn refill(&mut self, now: Instant) {
        let elapsed = now.duration_since(self.last_refill);
        let added = elapsed.as_secs_f64() * self.rate_bytes_per_sec as f64;
        self.available = (self.available + added).min(self.burst_bytes as f64);
        self.last_refill = now;
    }
}

// ── ThrottlePair ────────────────────────────────────────────────────

/// Paired download and upload bandwidth throttles.
///
/// Most P2P applications need asymmetric limits.  This struct provides
/// a convenient container for both directions.
#[derive(Debug, Clone)]
pub struct ThrottlePair {
    /// Download (incoming) bandwidth throttle.
    pub download: BandwidthThrottle,
    /// Upload (outgoing) bandwidth throttle.
    pub upload: BandwidthThrottle,
}

impl ThrottlePair {
    /// Creates a throttle pair with the given rates.
    ///
    /// Either rate can be 0 for unlimited.
    pub fn new(download_bytes_per_sec: u64, upload_bytes_per_sec: u64, now: Instant) -> Self {
        Self {
            download: BandwidthThrottle::new(download_bytes_per_sec, now),
            upload: BandwidthThrottle::new(upload_bytes_per_sec, now),
        }
    }

    /// Creates an unlimited throttle pair (both directions unrestricted).
    pub fn unlimited(now: Instant) -> Self {
        Self::new(0, 0, now)
    }

    /// Returns `true` if both directions are unlimited.
    pub fn is_unlimited(&self) -> bool {
        self.download.is_unlimited() && self.upload.is_unlimited()
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    // ── Basic throttle ──────────────────────────────────────────────

    /// Unlimited throttle always grants requests.
    ///
    /// Rate = 0 means no limit — request() must always return true
    /// regardless of byte count.
    #[test]
    fn unlimited_always_grants() {
        let now = Instant::now();
        let mut throttle = BandwidthThrottle::new(0, now);
        assert!(throttle.is_unlimited());
        assert!(throttle.request(u64::MAX, now));
    }

    /// Small request within burst succeeds immediately.
    ///
    /// A 4 KiB request against a 1 MB/s throttle (2 MB burst) should
    /// always succeed.
    #[test]
    fn small_request_within_burst() {
        let now = Instant::now();
        let mut throttle = BandwidthThrottle::new(1_048_576, now);
        assert!(throttle.request(4096, now));
    }

    /// Consuming the full burst exhausts available tokens.
    ///
    /// After consuming burst_bytes, no more tokens should be available
    /// until time passes.
    #[test]
    fn exhaust_burst_then_denied() {
        let now = Instant::now();
        let mut throttle = BandwidthThrottle::new(1000, now);
        let burst = throttle.burst();

        // Consume the entire burst.
        throttle.consume(burst, now);

        // No tokens left — large request denied.
        assert!(!throttle.request(1000, now));
    }

    /// Tokens refill over time at the configured rate.
    ///
    /// After exhausting the burst at t=0, waiting 1 second should refill
    /// `rate_bytes_per_sec` tokens.
    #[test]
    fn refill_over_time() {
        let t0 = Instant::now();
        let mut throttle = BandwidthThrottle::new(1000, t0);

        // Exhaust the burst.
        throttle.consume(throttle.burst(), t0);
        assert!(!throttle.request(500, t0));

        // Advance 1 second — should refill 1000 bytes.
        let t1 = t0 + Duration::from_secs(1);
        assert!(throttle.request(500, t1));
        assert!(throttle.request(1000, t1));
    }

    /// Refill does not exceed burst capacity.
    ///
    /// Even after long idle periods, available tokens cap at burst_bytes.
    #[test]
    fn refill_capped_at_burst() {
        let t0 = Instant::now();
        let mut throttle = BandwidthThrottle::new(1000, t0);
        let burst = throttle.burst();

        // Wait a very long time.
        let t1 = t0 + Duration::from_secs(3600);
        let available = throttle.available(t1);

        assert!(
            (available - burst as f64).abs() < 1.0,
            "available {available} should equal burst {burst}"
        );
    }

    /// Consume tracks total bytes.
    ///
    /// `total_consumed` must accurately reflect cumulative consumption.
    #[test]
    fn total_consumed_tracks_bytes() {
        let now = Instant::now();
        let mut throttle = BandwidthThrottle::new(1_000_000, now);
        throttle.consume(1000, now);
        throttle.consume(2000, now);
        assert_eq!(throttle.total_consumed(), 3000);
    }

    /// Unlimited throttle still tracks total consumed.
    #[test]
    fn unlimited_tracks_consumed() {
        let now = Instant::now();
        let mut throttle = BandwidthThrottle::new(0, now);
        throttle.consume(5000, now);
        assert_eq!(throttle.total_consumed(), 5000);
    }

    // ── Custom burst ────────────────────────────────────────────────

    /// Custom burst capacity overrides default factor.
    #[test]
    fn custom_burst() {
        let now = Instant::now();
        let throttle = BandwidthThrottle::with_burst(1000, 50_000, now);
        assert_eq!(throttle.burst(), 50_000);
    }

    /// Custom burst is clamped to MIN_BURST_BYTES.
    ///
    /// Even with a tiny explicit burst, the minimum ensures at least
    /// one piece request can get through.
    #[test]
    fn custom_burst_minimum_enforced() {
        let now = Instant::now();
        let throttle = BandwidthThrottle::with_burst(100, 1, now);
        assert_eq!(throttle.burst(), MIN_BURST_BYTES);
    }

    // ── Dynamic rate change ─────────────────────────────────────────

    /// `set_rate` updates the rate and recalculates burst.
    ///
    /// Changing the rate mid-session must immediately adjust the burst
    /// capacity and clamp available tokens.
    #[test]
    fn set_rate_updates_burst() {
        let now = Instant::now();
        let mut throttle = BandwidthThrottle::new(100_000, now);
        assert_eq!(throttle.rate(), 100_000);

        throttle.set_rate(500_000, now);
        assert_eq!(throttle.rate(), 500_000);
        assert_eq!(throttle.burst(), 500_000 * DEFAULT_BURST_FACTOR);
    }

    /// `set_rate(0)` makes throttle unlimited.
    #[test]
    fn set_rate_to_zero_is_unlimited() {
        let now = Instant::now();
        let mut throttle = BandwidthThrottle::new(1000, now);
        throttle.set_rate(0, now);
        assert!(throttle.is_unlimited());
        assert!(throttle.request(u64::MAX, now));
    }

    /// `set_rate` clamps available tokens to new burst.
    ///
    /// If the rate is lowered, available tokens that exceed the new
    /// burst must be clamped to prevent overshoot.
    #[test]
    fn set_rate_clamps_available() {
        let now = Instant::now();
        let mut throttle = BandwidthThrottle::new(1_000_000, now);
        // Available starts at burst = 2_000_000.

        // Lower the rate dramatically.
        throttle.set_rate(100, now);
        let available = throttle.available(now);

        // Available must not exceed new burst.
        assert!(
            available <= throttle.burst() as f64 + 1.0,
            "available {available} > new burst {}",
            throttle.burst()
        );
    }

    // ── ThrottlePair ────────────────────────────────────────────────

    /// Throttle pair creates independent download/upload throttles.
    #[test]
    fn pair_independent() {
        let now = Instant::now();
        let mut pair = ThrottlePair::new(1_000_000, 100, now);

        assert_eq!(pair.download.rate(), 1_000_000);
        assert_eq!(pair.upload.rate(), 100);
        assert!(!pair.is_unlimited());

        // Download allows a large request; upload burst (MIN_BURST_BYTES)
        // cannot accommodate 100 KiB.
        assert!(pair.download.request(100_000, now));
        assert!(!pair.upload.request(100_000, now));
    }

    /// Unlimited pair has both directions unlimited.
    #[test]
    fn pair_unlimited() {
        let now = Instant::now();
        let pair = ThrottlePair::unlimited(now);
        assert!(pair.is_unlimited());
    }

    // ── Edge cases ──────────────────────────────────────────────────

    /// Zero-byte request always succeeds (even on exhausted throttle).
    #[test]
    fn zero_byte_request_always_succeeds() {
        let now = Instant::now();
        let mut throttle = BandwidthThrottle::new(100, now);
        throttle.consume(throttle.burst(), now);
        // Exhausted, but 0 bytes is always allowed.
        assert!(throttle.request(0, now));
    }

    /// Fractional refill accumulates correctly.
    ///
    /// At 100 bytes/sec, after 10 ms we should have ~1.0 byte refilled.
    #[test]
    fn fractional_refill() {
        let t0 = Instant::now();
        let mut throttle = BandwidthThrottle::new(100, t0);

        // Exhaust burst.
        throttle.consume(throttle.burst(), t0);

        // After 100 ms at 100 B/s → 10 bytes refilled.
        let t1 = t0 + Duration::from_millis(100);
        let available = throttle.available(t1);
        assert!(
            (available - 10.0).abs() < 0.1,
            "expected ~10.0 bytes, got {available}"
        );
    }

    /// Very high rate doesn't overflow.
    ///
    /// Rate near u64::MAX / 2 still creates a valid throttle.
    #[test]
    fn high_rate_no_overflow() {
        let now = Instant::now();
        let throttle = BandwidthThrottle::new(u64::MAX / 4, now);
        assert!(!throttle.is_unlimited());
        assert!(throttle.burst() > 0);
    }

    /// Debug formatting works.
    #[test]
    fn debug_format() {
        let now = Instant::now();
        let throttle = BandwidthThrottle::new(1000, now);
        let dbg = format!("{throttle:?}");
        assert!(dbg.contains("BandwidthThrottle"));
    }

    /// ThrottlePair Debug formatting works.
    #[test]
    fn pair_debug_format() {
        let now = Instant::now();
        let pair = ThrottlePair::new(1000, 500, now);
        let dbg = format!("{pair:?}");
        assert!(dbg.contains("ThrottlePair"));
    }
}
