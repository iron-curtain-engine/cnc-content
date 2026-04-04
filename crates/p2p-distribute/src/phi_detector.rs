// SPDX-License-Identifier: MIT OR Apache-2.0

//! Phi Accrual Failure Detector — probabilistic peer health scoring
//! (Cassandra / Akka Cluster pattern).
//!
//! ## What
//!
//! A failure detector that outputs a continuous suspicion level (phi, φ)
//! rather than a binary alive/dead verdict. Higher phi means higher
//! confidence that the peer has failed. The coordinator can use different
//! phi thresholds for different actions:
//!
//! - φ < 3 → peer is healthy, keep using it.
//! - 3 ≤ φ < 8 → peer is suspect, deprioritise but don't disconnect.
//! - φ ≥ 8 → peer is almost certainly dead, disconnect.
//!
//! ## Why — Cassandra / Akka lesson
//!
//! Binary failure detection (`is_snubbed()`) has a fundamental problem:
//! it can't distinguish "slightly slower than usual" from "definitely
//! crashed." The phi accrual detector solves this by maintaining a
//! statistical model of inter-arrival times and computing the probability
//! that a peer has failed given how long since the last heartbeat.
//!
//! > "Rather than only answering 'yes' or 'no' to the question 'is the
//! > node down?', it returns a phi value representing the likelihood that
//! > the node is down."
//! > — Hayashibara et al., "The φ Accrual Failure Detector" (2004)
//!
//! ## How
//!
//! The detector maintains a sliding window of recent inter-arrival times.
//! From these it computes a mean (μ) and standard deviation (σ). The phi
//! value is:
//!
//! ```text
//! φ = -log₁₀(1 - F(timeSinceLastHeartbeat))
//! ```
//!
//! where F is the CDF of a normal distribution N(μ, σ²). As the time
//! since last heartbeat grows, F approaches 1 and phi grows without bound.
//!
//! ## Integration
//!
//! - Each `PeerStats` can embed a `PhiDetector`.
//! - Call `record_heartbeat()` on each successful piece delivery.
//! - Call `phi()` to get the current suspicion level.
//! - The coordinator uses phi thresholds to adjust peer selection weights.

use std::time::Instant;

// ── Constants ───────────────────────────────────────────────────────

/// Default sliding window size for inter-arrival time samples.
///
/// Akka uses 1000. We use a smaller window because our "heartbeats" are
/// piece deliveries which arrive less frequently than 1/second network
/// heartbeats. 50 samples ≈ last 50 pieces from this peer.
const DEFAULT_WINDOW_SIZE: usize = 50;

/// Minimum standard deviation floor (milliseconds).
///
/// Prevents division by zero and avoids extreme phi spikes when all
/// inter-arrival times are identical (e.g. local transfers).
const MIN_STD_DEV_MS: f64 = 100.0;

/// Default phi threshold above which a peer is considered unreachable.
///
/// Akka default: 8. Amazon EC2 recommendation: 12. We default to 8 for
/// LAN-like content delivery; consumers can raise this for unstable links.
pub const DEFAULT_PHI_THRESHOLD: f64 = 8.0;

/// Phi threshold for "suspect but not dead" — deprioritise the peer.
pub const SUSPECT_PHI_THRESHOLD: f64 = 3.0;

// ── PhiDetector ─────────────────────────────────────────────────────

/// Phi accrual failure detector for a single peer.
///
/// ```
/// use std::time::{Instant, Duration};
/// use p2p_distribute::PhiDetector;
///
/// let mut detector = PhiDetector::new(Instant::now());
///
/// // Simulate regular heartbeats 500ms apart.
/// let mut t = Instant::now();
/// for _ in 0..10 {
///     t += Duration::from_millis(500);
///     detector.record_heartbeat(t);
/// }
///
/// // If we check immediately after a heartbeat, phi should be low.
/// let phi = detector.phi(t + Duration::from_millis(100));
/// assert!(phi < 3.0, "phi should be low right after heartbeat: {phi}");
/// ```
#[derive(Debug, Clone)]
pub struct PhiDetector {
    /// Sliding window of inter-arrival times in milliseconds.
    intervals_ms: Vec<f64>,
    /// Maximum window size (oldest samples are evicted).
    max_window: usize,
    /// Time of the last recorded heartbeat.
    last_heartbeat: Instant,
    /// Whether at least one heartbeat has been recorded (the first
    /// heartbeat establishes the baseline, it doesn't produce an
    /// interval).
    has_baseline: bool,
}

impl PhiDetector {
    /// Creates a new detector with the given initial heartbeat time.
    ///
    /// The first call to `record_heartbeat()` will establish the first
    /// inter-arrival interval. Until then, `phi()` returns 0.0.
    pub fn new(now: Instant) -> Self {
        Self {
            intervals_ms: Vec::with_capacity(DEFAULT_WINDOW_SIZE),
            max_window: DEFAULT_WINDOW_SIZE,
            last_heartbeat: now,
            has_baseline: false,
        }
    }

    /// Creates a detector with a custom window size.
    pub fn with_window(now: Instant, window_size: usize) -> Self {
        let clamped = window_size.max(2);
        Self {
            intervals_ms: Vec::with_capacity(clamped),
            max_window: clamped,
            last_heartbeat: now,
            has_baseline: false,
        }
    }

    /// Records a heartbeat (successful piece delivery or keep-alive).
    ///
    /// The inter-arrival time from the previous heartbeat is added to
    /// the sliding window.
    pub fn record_heartbeat(&mut self, now: Instant) {
        if self.has_baseline {
            let interval_ms = now.duration_since(self.last_heartbeat).as_secs_f64() * 1000.0;
            if self.intervals_ms.len() >= self.max_window {
                // Evict oldest sample (FIFO).
                self.intervals_ms.remove(0);
            }
            self.intervals_ms.push(interval_ms);
        }
        self.last_heartbeat = now;
        self.has_baseline = true;
    }

    /// Computes the current phi (φ) suspicion level.
    ///
    /// Higher phi = higher probability the peer has failed.
    ///
    /// Returns `0.0` if fewer than 2 samples have been collected (not
    /// enough data for statistical inference).
    pub fn phi(&self, now: Instant) -> f64 {
        if self.intervals_ms.len() < 2 {
            return 0.0;
        }

        let elapsed_ms = now.duration_since(self.last_heartbeat).as_secs_f64() * 1000.0;
        let (mean, std_dev) = self.mean_and_std_dev();

        // CDF of normal distribution: F(x) = 0.5 * (1 + erf((x - μ) / (σ√2)))
        // phi = -log10(1 - F(elapsed))
        // Equivalent: phi = -log10(0.5 * erfc((elapsed - μ) / (σ√2)))
        //
        // We use an approximation of erfc that is accurate to ~10⁻⁷.
        let y = (elapsed_ms - mean) / std_dev;
        let p_later = p_later_than(y);

        if p_later <= 0.0 {
            // Avoid log(0) — return maximum phi.
            return 100.0;
        }

        let phi = -p_later.log10();
        // Clamp to [0.0, 100.0]. Can be slightly negative due to float
        // precision when elapsed is much less than mean. Can exceed 100
        // when p_later is extremely small but non-zero.
        phi.clamp(0.0, 100.0)
    }

    /// Whether the peer is considered reachable at the given threshold.
    pub fn is_available(&self, now: Instant, threshold: f64) -> bool {
        self.phi(now) < threshold
    }

    /// Number of inter-arrival samples collected.
    pub fn sample_count(&self) -> usize {
        self.intervals_ms.len()
    }

    /// Mean inter-arrival time in milliseconds.
    pub fn mean_interval_ms(&self) -> f64 {
        if self.intervals_ms.is_empty() {
            return 0.0;
        }
        let sum: f64 = self.intervals_ms.iter().sum();
        sum / self.intervals_ms.len() as f64
    }

    /// Computes mean and standard deviation of the sliding window.
    fn mean_and_std_dev(&self) -> (f64, f64) {
        let n = self.intervals_ms.len() as f64;
        if n < 1.0 {
            return (0.0, MIN_STD_DEV_MS);
        }
        let mean: f64 = self.intervals_ms.iter().sum::<f64>() / n;
        let variance: f64 = self
            .intervals_ms
            .iter()
            .map(|x| {
                let diff = x - mean;
                diff * diff
            })
            .sum::<f64>()
            / n;
        let std_dev = variance.sqrt().max(MIN_STD_DEV_MS);
        (mean, std_dev)
    }
}

// ── Statistics helpers ──────────────────────────────────────────────

/// Approximation of P(X > y) for standard normal distribution.
///
/// Uses the complementary error function approximation from Abramowitz
/// and Stegun (Handbook of Mathematical Functions, 1964, formula 7.1.26).
/// Accuracy: |ε| < 1.5 × 10⁻⁷.
///
/// This avoids a dependency on a statistics crate for a single function.
fn p_later_than(y: f64) -> f64 {
    // P(X > y) = 0.5 * erfc(y / sqrt(2))
    0.5 * erfc(y / std::f64::consts::SQRT_2)
}

/// Complementary error function approximation.
///
/// Abramowitz & Stegun 7.1.26: rational approximation with 5 terms.
fn erfc(x: f64) -> f64 {
    // For negative x: erfc(-x) = 2 - erfc(x).
    if x < 0.0 {
        return 2.0 - erfc(-x);
    }

    let t = 1.0 / (1.0 + 0.327_591_1 * x);
    let t2 = t * t;
    let t3 = t2 * t;
    let t4 = t3 * t;
    let t5 = t4 * t;

    let poly = 0.254_829_592 * t - 0.284_496_736 * t2 + 1.421_413_741 * t3 - 1.453_152_027 * t4
        + 1.061_405_429 * t5;

    poly * (-x * x).exp()
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    // ── Basic phi behaviour ─────────────────────────────────────────

    /// Phi is 0 with insufficient samples.
    ///
    /// The detector needs at least 2 inter-arrival intervals before it
    /// can compute statistics. Before that, it returns 0 (benefit of doubt).
    #[test]
    fn phi_zero_with_insufficient_samples() {
        let now = Instant::now();
        let detector = PhiDetector::new(now);
        assert_eq!(detector.phi(now + Duration::from_secs(10)), 0.0);
    }

    /// Phi remains low shortly after a heartbeat.
    ///
    /// If heartbeats arrive regularly at ~500ms and we check 100ms after
    /// the latest, the probability of failure is negligible.
    #[test]
    fn phi_low_after_recent_heartbeat() {
        let now = Instant::now();
        let mut det = PhiDetector::new(now);
        // Build up regular 500ms intervals.
        for i in 1..=10 {
            det.record_heartbeat(now + Duration::from_millis(i * 500));
        }
        let check = now + Duration::from_millis(5100); // 100ms after last
        let phi = det.phi(check);
        assert!(phi < SUSPECT_PHI_THRESHOLD, "phi should be low: {phi}");
    }

    /// Phi grows as time since last heartbeat increases.
    ///
    /// This is the key property: the longer the silence, the more
    /// suspicious the detector becomes.
    #[test]
    fn phi_grows_with_silence() {
        let now = Instant::now();
        let mut det = PhiDetector::new(now);
        // Alternate between 1.5s and 2.5s intervals to create real
        // variance (mean ≈ 2000ms, std_dev ≈ 500ms). With constant
        // intervals the std_dev collapses to MIN_STD_DEV_MS (100ms) and
        // anything beyond ~2.2s silence saturates phi at the 100 cap.
        let mut t = 0u64;
        for i in 1..=20u64 {
            let interval = if i % 2 == 0 { 2500 } else { 1500 };
            t = t.saturating_add(interval);
            det.record_heartbeat(now + Duration::from_millis(t));
        }
        let baseline = now + Duration::from_millis(t);
        // Test at 3s, 6s, and 12s — all within measurable phi range.
        let phi_3s = det.phi(baseline + Duration::from_secs(3));
        let phi_6s = det.phi(baseline + Duration::from_secs(6));
        let phi_12s = det.phi(baseline + Duration::from_secs(12));
        assert!(
            phi_3s < phi_6s,
            "3s phi ({phi_3s}) should be less than 6s phi ({phi_6s})"
        );
        assert!(
            phi_6s < phi_12s,
            "6s phi ({phi_6s}) should be less than 12s phi ({phi_12s})"
        );
    }

    /// Very long silence produces phi above the default threshold.
    #[test]
    fn very_long_silence_exceeds_threshold() {
        let now = Instant::now();
        let mut det = PhiDetector::new(now);
        for i in 1..=20 {
            det.record_heartbeat(now + Duration::from_millis(i * 500));
        }
        let phi = det.phi(now + Duration::from_secs(60));
        assert!(
            phi >= DEFAULT_PHI_THRESHOLD,
            "60s silence should exceed threshold 8: {phi}"
        );
    }

    /// is_available matches phi threshold comparison.
    #[test]
    fn is_available_consistent_with_phi() {
        let now = Instant::now();
        let mut det = PhiDetector::new(now);
        for i in 1..=10 {
            det.record_heartbeat(now + Duration::from_millis(i * 500));
        }
        let check = now + Duration::from_millis(5100);
        let phi = det.phi(check);
        assert_eq!(
            det.is_available(check, DEFAULT_PHI_THRESHOLD),
            phi < DEFAULT_PHI_THRESHOLD
        );
    }

    // ── Sliding window ──────────────────────────────────────────────

    /// Old samples are evicted when the window is full.
    ///
    /// This prevents ancient intervals from influencing current suspicion
    /// after a peer changes speed (e.g. congestion resolves).
    #[test]
    fn window_evicts_old_samples() {
        let now = Instant::now();
        let mut det = PhiDetector::with_window(now, 5);
        // Record 10 heartbeats, only last 5 intervals should remain.
        for i in 1..=10 {
            det.record_heartbeat(now + Duration::from_millis(i * 100));
        }
        assert_eq!(det.sample_count(), 5);
    }

    /// Custom window size is respected.
    #[test]
    fn custom_window_size() {
        let now = Instant::now();
        let det = PhiDetector::with_window(now, 10);
        assert_eq!(det.sample_count(), 0);
    }

    // ── Mean interval ───────────────────────────────────────────────

    /// Mean interval reflects recorded heartbeat spacing.
    #[test]
    fn mean_interval_reflects_spacing() {
        let now = Instant::now();
        let mut det = PhiDetector::new(now);
        for i in 1..=10 {
            det.record_heartbeat(now + Duration::from_millis(i * 1000));
        }
        let mean = det.mean_interval_ms();
        // All intervals are ~1000ms.
        assert!(
            (mean - 1000.0).abs() < 1.0,
            "mean should be ~1000ms: {mean}"
        );
    }

    /// Mean interval is 0 with no samples.
    #[test]
    fn mean_interval_zero_no_samples() {
        let now = Instant::now();
        let det = PhiDetector::new(now);
        assert_eq!(det.mean_interval_ms(), 0.0);
    }

    // ── erfc accuracy ───────────────────────────────────────────────

    /// erfc approximation is accurate at key points.
    ///
    /// erfc(0) = 1.0 exactly. erfc(1) ≈ 0.1573. erfc(3) ≈ 2.2e-5.
    #[test]
    fn erfc_accuracy() {
        let e0 = erfc(0.0);
        assert!((e0 - 1.0).abs() < 1e-6, "erfc(0) = {e0}");

        let e1 = erfc(1.0);
        assert!((e1 - 0.1573).abs() < 0.001, "erfc(1) = {e1}");

        let e3 = erfc(3.0);
        assert!(e3 < 0.001, "erfc(3) should be very small: {e3}");
    }

    /// erfc handles negative input.
    #[test]
    fn erfc_negative() {
        let e_neg1 = erfc(-1.0);
        // erfc(-x) = 2 - erfc(x): erfc(-1) ≈ 1.8427.
        assert!((e_neg1 - 1.8427).abs() < 0.001, "erfc(-1) = {e_neg1}");
    }

    // ── Resilience ──────────────────────────────────────────────────

    /// Detector handles all-identical intervals gracefully.
    ///
    /// When all inter-arrival times are exactly equal, standard deviation
    /// would be 0 without the MIN_STD_DEV_MS floor. The floor prevents
    /// phi from spiking to infinity.
    #[test]
    fn identical_intervals_do_not_cause_infinite_phi() {
        let now = Instant::now();
        let mut det = PhiDetector::new(now);
        // All intervals exactly 500ms.
        for i in 1..=20 {
            det.record_heartbeat(now + Duration::from_millis(i * 500));
        }
        let phi = det.phi(now + Duration::from_millis(10_500));
        assert!(phi.is_finite(), "phi should be finite: {phi}");
        assert!(phi < 100.0, "phi should not be extreme: {phi}");
    }
}
