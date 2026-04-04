// SPDX-License-Identifier: MIT OR Apache-2.0

//! EWMA bandwidth estimator for adaptive streaming (DASH ABR pattern).
//!
//! ## What
//!
//! A bandwidth estimator that tracks observed download throughput using an
//! Exponentially Weighted Moving Average (EWMA) with fast and slow
//! coefficients. The estimate feeds into the streaming `BufferPolicy` to
//! dynamically adjust prebuffer thresholds.
//!
//! ## Why — DASH ABR lesson
//!
//! DASH/HLS adaptive bitrate players maintain a dual-EWMA bandwidth
//! estimate (one fast-adapting for recent changes, one slow-adapting for
//! stability). The lower of the two is used as the "safe" estimate,
//! preventing the player from switching to a higher bitrate that the
//! network can't sustain.
//!
//! Our streaming reader has a similar problem: the `BufferPolicy`'s
//! `min_prebuffer` is a fixed 512 KB, but on a slow link that might
//! represent 10 seconds of buffering time, while on a fast link it's
//! 50ms. An adaptive policy would use the bandwidth estimate to set
//! prebuffer thresholds in terms of *time* (e.g. "buffer 3 seconds
//! ahead") rather than *bytes*.
//!
//! ## How — dual EWMA
//!
//! Two EWMA filters run in parallel:
//! - **Fast** (α = 0.5) — reacts quickly to speed changes.
//! - **Slow** (α = 0.1) — smooths out bursts for stability.
//!
//! The "safe" estimate is `min(fast, slow)`, which is conservative:
//! it drops quickly when the network degrades (fast filter) but doesn't
//! over-react to a single fast burst (slow filter).
//!
//! ## Integration
//!
//! - Coordinator calls `record_sample()` after each piece completes.
//! - `StreamingReader` calls `safe_estimate()` to compute prebuffer size.
//! - `BufferPolicy` can use `time_to_buffer()` to convert byte thresholds
//!   to time thresholds.

use std::time::{Duration, Instant};

// ── Constants ───────────────────────────────────────────────────────

/// Fast EWMA smoothing factor. Higher α reacts faster to changes.
///
/// DASH.js uses 0.5 for the fast estimate. This means the most recent
/// sample contributes 50% of the estimate.
const FAST_ALPHA: f64 = 0.5;

/// Slow EWMA smoothing factor. Lower α provides more stability.
///
/// DASH.js uses 0.1 for the slow estimate. This means each sample
/// contributes only 10%, smoothing out bursts.
const SLOW_ALPHA: f64 = 0.1;

/// Minimum sample duration to avoid division by near-zero.
///
/// If a piece arrives in less than 1ms, clamp to 1ms to prevent
/// computing absurdly high bandwidth from timer resolution noise.
const MIN_SAMPLE_DURATION: Duration = Duration::from_millis(1);

// ── BandwidthEstimator ──────────────────────────────────────────────

/// Dual-EWMA bandwidth estimator for adaptive streaming decisions.
///
/// ```
/// use std::time::{Instant, Duration};
/// use p2p_distribute::BandwidthEstimator;
///
/// let mut est = BandwidthEstimator::new();
///
/// // Simulate downloading 100KB in 100ms (= 1 MB/s).
/// est.record_sample(100_000, Duration::from_millis(100), Instant::now());
///
/// let safe = est.safe_estimate_bytes_per_sec();
/// assert!(safe > 0.0);
/// ```
#[derive(Debug, Clone)]
pub struct BandwidthEstimator {
    /// Fast EWMA estimate (bytes/sec).
    fast_bps: f64,
    /// Slow EWMA estimate (bytes/sec).
    slow_bps: f64,
    /// Number of samples recorded.
    sample_count: u64,
    /// Total bytes observed across all samples.
    total_bytes: u64,
    /// Total time observed across all samples.
    total_duration: Duration,
    /// Time of the most recent sample.
    last_sample_at: Option<Instant>,
}

impl BandwidthEstimator {
    /// Creates a new estimator with no samples.
    pub fn new() -> Self {
        Self {
            fast_bps: 0.0,
            slow_bps: 0.0,
            sample_count: 0,
            total_bytes: 0,
            total_duration: Duration::ZERO,
            last_sample_at: None,
        }
    }

    /// Records a download sample: `bytes` transferred in `duration`.
    ///
    /// Call this after each piece completes. The bandwidth is computed
    /// as `bytes / duration` and fed into both EWMA filters.
    pub fn record_sample(&mut self, bytes: u64, duration: Duration, now: Instant) {
        let clamped = duration.max(MIN_SAMPLE_DURATION);
        let sample_bps = bytes as f64 / clamped.as_secs_f64();

        if self.sample_count == 0 {
            // First sample initialises both filters.
            self.fast_bps = sample_bps;
            self.slow_bps = sample_bps;
        } else {
            // EWMA: new = α × sample + (1 - α) × old
            self.fast_bps = FAST_ALPHA * sample_bps + (1.0 - FAST_ALPHA) * self.fast_bps;
            self.slow_bps = SLOW_ALPHA * sample_bps + (1.0 - SLOW_ALPHA) * self.slow_bps;
        }

        self.sample_count = self.sample_count.saturating_add(1);
        self.total_bytes = self.total_bytes.saturating_add(bytes);
        self.total_duration = self.total_duration.saturating_add(clamped);
        self.last_sample_at = Some(now);
    }

    /// Conservative bandwidth estimate (bytes/sec).
    ///
    /// Returns `min(fast, slow)` — the DASH ABR "safe" estimate. This
    /// drops quickly when the network degrades but doesn't over-react
    /// to a single fast burst.
    ///
    /// Returns `0.0` if no samples have been recorded.
    pub fn safe_estimate_bytes_per_sec(&self) -> f64 {
        if self.sample_count == 0 {
            return 0.0;
        }
        self.fast_bps.min(self.slow_bps)
    }

    /// Fast-reacting bandwidth estimate (bytes/sec).
    pub fn fast_estimate_bytes_per_sec(&self) -> f64 {
        self.fast_bps
    }

    /// Slow-reacting bandwidth estimate (bytes/sec).
    pub fn slow_estimate_bytes_per_sec(&self) -> f64 {
        self.slow_bps
    }

    /// Estimated time to buffer `byte_count` bytes.
    ///
    /// Uses the safe (conservative) estimate. Returns `None` if no
    /// samples have been recorded.
    ///
    /// ## Usage with BufferPolicy
    ///
    /// Convert a byte-based prebuffer threshold to a time estimate:
    /// ```text
    /// let prebuffer_time = estimator.time_to_buffer(policy.min_prebuffer);
    /// if prebuffer_time > Duration::from_secs(10) {
    ///     // Network too slow for this buffer size — reduce or warn user
    /// }
    /// ```
    pub fn time_to_buffer(&self, byte_count: u64) -> Option<Duration> {
        let safe = self.safe_estimate_bytes_per_sec();
        if safe <= 0.0 {
            return None;
        }
        let secs = byte_count as f64 / safe;
        Some(Duration::from_secs_f64(secs))
    }

    /// Suggested prebuffer size for a given target buffer time.
    ///
    /// Returns the number of bytes that can be downloaded in
    /// `target_time` at the current safe estimate. This lets the
    /// streaming reader set prebuffer in time terms ("3 seconds ahead")
    /// rather than byte terms ("512 KB ahead").
    ///
    /// Returns `0` if no samples have been recorded.
    pub fn prebuffer_for_duration(&self, target_time: Duration) -> u64 {
        let safe = self.safe_estimate_bytes_per_sec();
        if safe <= 0.0 {
            return 0;
        }
        (safe * target_time.as_secs_f64()) as u64
    }

    /// Number of samples recorded.
    pub fn sample_count(&self) -> u64 {
        self.sample_count
    }

    /// Overall average bandwidth (bytes/sec) across all samples.
    ///
    /// Unlike the EWMA estimates, this gives equal weight to all samples.
    /// Useful for session summary statistics.
    pub fn overall_average_bytes_per_sec(&self) -> f64 {
        let total_secs = self.total_duration.as_secs_f64();
        if total_secs <= 0.0 {
            return 0.0;
        }
        self.total_bytes as f64 / total_secs
    }

    /// Time of the most recent sample.
    pub fn last_sample_at(&self) -> Option<Instant> {
        self.last_sample_at
    }
}

impl Default for BandwidthEstimator {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    // ── Initial state ───────────────────────────────────────────────

    /// New estimator reports zero bandwidth.
    #[test]
    fn new_estimator_zero() {
        let est = BandwidthEstimator::new();
        assert_eq!(est.safe_estimate_bytes_per_sec(), 0.0);
        assert_eq!(est.sample_count(), 0);
        assert!(est.time_to_buffer(1000).is_none());
    }

    /// Default trait produces same as new().
    #[test]
    fn default_same_as_new() {
        let est = BandwidthEstimator::default();
        assert_eq!(est.sample_count(), 0);
    }

    // ── Single sample ───────────────────────────────────────────────

    /// First sample initialises both EWMA filters to the same value.
    ///
    /// 100_000 bytes in 100ms = 1_000_000 bytes/sec.
    #[test]
    fn first_sample_initialises_both_filters() {
        let mut est = BandwidthEstimator::new();
        let now = Instant::now();
        est.record_sample(100_000, Duration::from_millis(100), now);

        let expected = 1_000_000.0;
        assert!((est.fast_estimate_bytes_per_sec() - expected).abs() < 1.0);
        assert!((est.slow_estimate_bytes_per_sec() - expected).abs() < 1.0);
        assert!((est.safe_estimate_bytes_per_sec() - expected).abs() < 1.0);
    }

    // ── EWMA convergence ────────────────────────────────────────────

    /// Fast filter reacts more quickly than slow filter to speed change.
    ///
    /// After a sudden speed drop, the fast filter should be closer to
    /// the new speed than the slow filter.
    #[test]
    fn fast_reacts_faster_than_slow() {
        let mut est = BandwidthEstimator::new();
        let now = Instant::now();

        // 10 samples at 1 MB/s.
        for i in 0..10 {
            est.record_sample(
                100_000,
                Duration::from_millis(100),
                now + Duration::from_secs(i),
            );
        }

        // Sudden drop to 100 KB/s (10× slower).
        est.record_sample(
            10_000,
            Duration::from_millis(100),
            now + Duration::from_secs(10),
        );

        // Fast filter should be closer to 100KB/s than slow filter.
        let fast = est.fast_estimate_bytes_per_sec();
        let slow = est.slow_estimate_bytes_per_sec();
        let new_speed = 100_000.0;

        assert!(
            (fast - new_speed).abs() < (slow - new_speed).abs(),
            "fast ({fast}) should be closer to {new_speed} than slow ({slow})"
        );
    }

    /// Safe estimate is the minimum of fast and slow.
    ///
    /// This is the conservative property: during a speed drop, the fast
    /// filter drops first, making safe == fast. During a speed increase,
    /// the slow filter lags, making safe == slow.
    #[test]
    fn safe_is_minimum_of_fast_and_slow() {
        let mut est = BandwidthEstimator::new();
        let now = Instant::now();

        // Build up at 1 MB/s.
        for i in 0..10 {
            est.record_sample(
                100_000,
                Duration::from_millis(100),
                now + Duration::from_secs(i),
            );
        }
        // Drop to 100 KB/s.
        est.record_sample(
            10_000,
            Duration::from_millis(100),
            now + Duration::from_secs(10),
        );

        let safe = est.safe_estimate_bytes_per_sec();
        let fast = est.fast_estimate_bytes_per_sec();
        let slow = est.slow_estimate_bytes_per_sec();
        assert!(
            (safe - fast.min(slow)).abs() < 0.01,
            "safe ({safe}) should equal min(fast={fast}, slow={slow})"
        );
    }

    // ── Time-to-buffer ──────────────────────────────────────────────

    /// time_to_buffer returns correct estimate.
    ///
    /// At 1 MB/s, buffering 2 MB should take ~2 seconds.
    #[test]
    fn time_to_buffer_correct() {
        let mut est = BandwidthEstimator::new();
        let now = Instant::now();
        est.record_sample(1_000_000, Duration::from_secs(1), now);

        let ttb = est.time_to_buffer(2_000_000).expect("should have estimate");
        let secs = ttb.as_secs_f64();
        assert!((secs - 2.0).abs() < 0.01, "should be ~2s: {secs}");
    }

    /// prebuffer_for_duration returns correct byte count.
    ///
    /// At 1 MB/s, 3 seconds of buffer = 3 MB.
    #[test]
    fn prebuffer_for_duration_correct() {
        let mut est = BandwidthEstimator::new();
        let now = Instant::now();
        est.record_sample(1_000_000, Duration::from_secs(1), now);

        let bytes = est.prebuffer_for_duration(Duration::from_secs(3));
        assert!(
            (bytes as f64 - 3_000_000.0).abs() < 100.0,
            "should be ~3MB: {bytes}"
        );
    }

    // ── Edge cases ──────────────────────────────────────────────────

    /// Very short durations are clamped to 1ms minimum.
    ///
    /// Prevents division by near-zero when the timer reports 0 duration
    /// (e.g. piece was in OS read-ahead cache).
    #[test]
    fn short_duration_clamped() {
        let mut est = BandwidthEstimator::new();
        let now = Instant::now();
        // 1000 bytes in 0ms → clamped to 1ms → 1_000_000 B/s.
        est.record_sample(1000, Duration::ZERO, now);
        let bps = est.safe_estimate_bytes_per_sec();
        assert!(bps.is_finite(), "should be finite: {bps}");
        assert!(bps > 0.0, "should be positive: {bps}");
    }

    /// Overall average is independent of EWMA filters.
    #[test]
    fn overall_average_independent() {
        let mut est = BandwidthEstimator::new();
        let now = Instant::now();
        // 2 samples: 100KB in 100ms + 200KB in 200ms = 300KB in 300ms = 1 MB/s.
        est.record_sample(100_000, Duration::from_millis(100), now);
        est.record_sample(
            200_000,
            Duration::from_millis(200),
            now + Duration::from_secs(1),
        );
        let avg = est.overall_average_bytes_per_sec();
        assert!(
            (avg - 1_000_000.0).abs() < 1.0,
            "overall avg should be ~1MB/s: {avg}"
        );
    }

    /// last_sample_at tracks the most recent sample time.
    #[test]
    fn last_sample_at_tracks_time() {
        let mut est = BandwidthEstimator::new();
        assert!(est.last_sample_at().is_none());
        let now = Instant::now();
        est.record_sample(1000, Duration::from_millis(100), now);
        assert_eq!(est.last_sample_at(), Some(now));
    }

    // ── Pathological inputs ─────────────────────────────────────────

    /// Zero bytes in a valid duration produces zero bandwidth.
    ///
    /// A piece that transfers no data (e.g. empty piece) should not
    /// corrupt the estimator — bandwidth should trend toward zero.
    #[test]
    fn zero_bytes_sample() {
        let mut est = BandwidthEstimator::new();
        let now = Instant::now();
        est.record_sample(0, Duration::from_secs(1), now);
        assert_eq!(est.safe_estimate_bytes_per_sec(), 0.0);
        assert!(est.safe_estimate_bytes_per_sec().is_finite());
    }

    /// `u64::MAX` bytes does not produce NaN or infinity.
    ///
    /// Extremely large byte counts (corrupted packet) must not crash
    /// the estimator. The result will be huge but still finite.
    #[test]
    fn huge_bytes_stays_finite() {
        let mut est = BandwidthEstimator::new();
        let now = Instant::now();
        est.record_sample(u64::MAX, Duration::from_secs(1), now);
        let bps = est.safe_estimate_bytes_per_sec();
        assert!(bps.is_finite(), "should be finite: {bps}");
        assert!(bps > 0.0, "should be positive: {bps}");
    }

    /// `time_to_buffer(0)` returns zero duration.
    ///
    /// Buffering zero bytes takes no time.
    #[test]
    fn time_to_buffer_zero_bytes() {
        let mut est = BandwidthEstimator::new();
        let now = Instant::now();
        est.record_sample(1000, Duration::from_millis(100), now);
        let ttb = est.time_to_buffer(0).unwrap();
        assert_eq!(ttb, Duration::ZERO);
    }

    /// `prebuffer_for_duration(ZERO)` returns 0 bytes.
    #[test]
    fn prebuffer_zero_duration() {
        let mut est = BandwidthEstimator::new();
        let now = Instant::now();
        est.record_sample(1000, Duration::from_millis(100), now);
        assert_eq!(est.prebuffer_for_duration(Duration::ZERO), 0);
    }

    /// Multiple zero-duration samples are clamped and don't produce NaN.
    ///
    /// Timer resolution on some OSes returns Duration::ZERO for very
    /// fast transfers. The clamp to 1ms prevents division by zero.
    #[test]
    fn many_zero_duration_samples_finite() {
        let mut est = BandwidthEstimator::new();
        let now = Instant::now();
        for i in 0..100u64 {
            est.record_sample(1000, Duration::ZERO, now + Duration::from_millis(i));
        }
        let bps = est.safe_estimate_bytes_per_sec();
        assert!(
            bps.is_finite(),
            "should be finite after 100 zero-duration samples: {bps}"
        );
        assert!(bps > 0.0);
    }
}
