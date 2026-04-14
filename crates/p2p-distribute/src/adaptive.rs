// SPDX-License-Identifier: MIT OR Apache-2.0

//! Adaptive concurrency controller — adjusts download parallelism based on
//! observed throughput.
//!
//! Implements the FlashGet-style adaptive concurrency pattern: start with a
//! conservative number of concurrent connections, ramp up when throughput
//! increases, and back off when throughput stalls or decreases. This avoids
//! both under-utilization (too few connections for the available bandwidth)
//! and over-subscription (too many connections causing TCP contention and
//! server-side rate limiting).
//!
//! ## Algorithm
//!
//! 1. Start at `min` concurrent connections (conservative).
//! 2. After each measurement window (default 5 seconds):
//!    - If throughput increased ≥ 10% over the previous window: double
//!      concurrency (capped at `max`).
//!    - If throughput decreased ≥ 20%: halve concurrency (floored at `min`).
//!    - Otherwise: hold steady.
//! 3. The first completed window establishes a baseline — no adjustment is
//!    made until a second window provides comparison data.
//!
//! ## Integration
//!
//! The controller is a pure algorithm — it accepts throughput measurements
//! and outputs concurrency recommendations. The coordinator decides when to
//! apply them. This separation allows the controller to be tested
//! independently of I/O and network behavior.
//!
//! ```rust
//! use p2p_distribute::adaptive::{AdaptiveConcurrency, ConcurrencyAdvice};
//! use std::time::{Duration, Instant};
//!
//! let now = Instant::now();
//! let mut ctrl = AdaptiveConcurrency::new(2, 32, now);
//! assert_eq!(ctrl.current(), 2); // starts conservative
//!
//! // Simulate a measurement window with high throughput
//! ctrl.record_piece_completion(1_000_000); // 1 MB
//! let later = now + Duration::from_secs(6);
//! let advice = ctrl.advise(later); // first window → Hold (no baseline yet)
//! ctrl.apply(advice);
//! assert_eq!(ctrl.current(), 2); // no change — need two windows
//!
//! // Second window with even more throughput → ramp up
//! ctrl.record_piece_completion(2_000_000); // 2 MB in next window
//! let even_later = later + Duration::from_secs(6);
//! let advice = ctrl.advise(even_later);
//! ctrl.apply(advice);
//! assert_eq!(ctrl.current(), 4); // doubled
//! ```

use std::time::{Duration, Instant};

// ── Configuration constants ─────────────────────────────────────────

/// Default minimum concurrent connections.
const DEFAULT_MIN: u32 = 2;

/// Default maximum concurrent connections.
const DEFAULT_MAX: u32 = 32;

/// Default measurement window duration.
const DEFAULT_WINDOW: Duration = Duration::from_secs(5);

/// Throughput increase (fraction) that triggers ramp-up: 10%.
const RAMP_UP_THRESHOLD: f64 = 0.10;

/// Throughput decrease (fraction) that triggers scale-down: 20%.
///
/// The asymmetric thresholds (10% up, 20% down) prevent oscillation:
/// the controller is quicker to ramp up (optimistic) but slower to
/// scale down (cautious), which matches typical network jitter patterns.
const SCALE_DOWN_THRESHOLD: f64 = 0.20;

// ── Concurrency advice ──────────────────────────────────────────────

/// Recommendation from the adaptive controller.
///
/// Callers inspect the advice, optionally log or override it, then call
/// [`AdaptiveConcurrency::apply`] to enact it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConcurrencyAdvice {
    /// Maintain current concurrency level — throughput is stable.
    Hold,
    /// Increase concurrency — throughput is improving.
    Increase { from: u32, to: u32 },
    /// Decrease concurrency — throughput is degrading.
    Decrease { from: u32, to: u32 },
}

// ── AdaptiveConcurrency ─────────────────────────────────────────────

/// Adaptive concurrency controller.
///
/// Tracks throughput over sliding windows and recommends concurrency
/// adjustments. The caller records piece completions and periodically
/// asks for advice.
///
/// ## Thread safety
///
/// This type is **not** `Sync` — it is designed to be owned by a single
/// coordinator thread that calls `record_piece_completion` and `advise`
/// sequentially.
pub struct AdaptiveConcurrency {
    /// Current recommended concurrency level.
    current: u32,
    /// Minimum allowed concurrency.
    min: u32,
    /// Maximum allowed concurrency.
    max: u32,
    /// Bytes transferred in the current measurement window.
    window_bytes: u64,
    /// Start of the current measurement window.
    window_start: Instant,
    /// Duration of each measurement window.
    window_duration: Duration,
    /// Throughput (bytes/sec) from the previous completed window.
    /// `None` until the first window completes (need two windows for comparison).
    prev_throughput: Option<f64>,
}

impl AdaptiveConcurrency {
    /// Creates a new controller with the given concurrency bounds.
    ///
    /// Starts at `min` (conservative). The controller will recommend
    /// increases as throughput data accumulates.
    ///
    /// If `min > max`, `max` is raised to equal `min`. Both are floored at 1.
    pub fn new(min: u32, max: u32, now: Instant) -> Self {
        let effective_min = min.max(1);
        let effective_max = max.max(effective_min);
        Self {
            current: effective_min,
            min: effective_min,
            max: effective_max,
            window_bytes: 0,
            window_start: now,
            window_duration: DEFAULT_WINDOW,
            prev_throughput: None,
        }
    }

    /// Creates a controller with default bounds (2–32).
    pub fn default_bounds(now: Instant) -> Self {
        Self::new(DEFAULT_MIN, DEFAULT_MAX, now)
    }

    /// Current recommended concurrency level.
    pub fn current(&self) -> u32 {
        self.current
    }

    /// Minimum concurrency bound.
    pub fn min(&self) -> u32 {
        self.min
    }

    /// Maximum concurrency bound.
    pub fn max(&self) -> u32 {
        self.max
    }

    /// Records a completed piece download (bytes transferred).
    ///
    /// Call this after each successful piece verification and write. The
    /// controller accumulates bytes to compute throughput for the current
    /// measurement window.
    pub fn record_piece_completion(&mut self, bytes: u64) {
        self.window_bytes = self.window_bytes.saturating_add(bytes);
    }

    /// Evaluates current throughput and returns a concurrency recommendation.
    ///
    /// Call this periodically (e.g. after each piece completion). The
    /// controller only produces a non-Hold recommendation when a full
    /// measurement window has elapsed and comparison data exists.
    ///
    /// ## Algorithm
    ///
    /// - First completed window: records baseline throughput, returns `Hold`.
    /// - Subsequent windows: compares throughput to previous window.
    ///   - ≥10% increase → `Increase` (double current, capped at max)
    ///   - ≥20% decrease → `Decrease` (halve current, floored at min)
    ///   - Otherwise → `Hold`
    pub fn advise(&mut self, now: Instant) -> ConcurrencyAdvice {
        let elapsed = now.duration_since(self.window_start);
        if elapsed < self.window_duration {
            return ConcurrencyAdvice::Hold;
        }

        // ── Compute throughput for the completed window ─────────────
        let elapsed_secs = elapsed.as_secs_f64();
        let throughput = if elapsed_secs > 0.0 {
            self.window_bytes as f64 / elapsed_secs
        } else {
            0.0
        };

        let advice = match self.prev_throughput {
            Some(prev) if prev > 0.0 => {
                let change = (throughput - prev) / prev;

                if change >= RAMP_UP_THRESHOLD && self.current < self.max {
                    // Throughput improved ≥ 10% — more concurrency is helping.
                    let new = self.current.saturating_mul(2).min(self.max);
                    if new > self.current {
                        ConcurrencyAdvice::Increase {
                            from: self.current,
                            to: new,
                        }
                    } else {
                        ConcurrencyAdvice::Hold
                    }
                } else if change <= -SCALE_DOWN_THRESHOLD && self.current > self.min {
                    // Throughput dropped ≥ 20% — too much concurrency is hurting.
                    let new = (self.current / 2).max(self.min);
                    if new < self.current {
                        ConcurrencyAdvice::Decrease {
                            from: self.current,
                            to: new,
                        }
                    } else {
                        ConcurrencyAdvice::Hold
                    }
                } else {
                    ConcurrencyAdvice::Hold
                }
            }
            _ => {
                // First window: establish baseline, no comparison possible.
                ConcurrencyAdvice::Hold
            }
        };

        // ── Reset window for next measurement period ────────────────
        self.prev_throughput = Some(throughput);
        self.window_bytes = 0;
        self.window_start = now;

        advice
    }

    /// Applies a concurrency recommendation.
    ///
    /// Separated from [`advise`](Self::advise) so the caller can log,
    /// filter, or override the recommendation before applying it.
    pub fn apply(&mut self, advice: ConcurrencyAdvice) {
        match advice {
            ConcurrencyAdvice::Increase { to, .. } => {
                self.current = to.min(self.max);
            }
            ConcurrencyAdvice::Decrease { to, .. } => {
                self.current = to.max(self.min);
            }
            ConcurrencyAdvice::Hold => {}
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Construction ────────────────────────────────────────────────

    /// Controller starts at the minimum concurrency level.
    ///
    /// Starting conservative prevents over-subscription on first connection
    /// when network conditions are unknown.
    #[test]
    fn starts_at_min() {
        let now = Instant::now();
        let ctrl = AdaptiveConcurrency::new(4, 16, now);
        assert_eq!(ctrl.current(), 4);
        assert_eq!(ctrl.min(), 4);
        assert_eq!(ctrl.max(), 16);
    }

    /// Default bounds are 2–32.
    #[test]
    fn default_bounds() {
        let now = Instant::now();
        let ctrl = AdaptiveConcurrency::default_bounds(now);
        assert_eq!(ctrl.current(), 2);
        assert_eq!(ctrl.min(), 2);
        assert_eq!(ctrl.max(), 32);
    }

    /// If min > max, max is raised to equal min.
    #[test]
    fn min_greater_than_max_adjusted() {
        let now = Instant::now();
        let ctrl = AdaptiveConcurrency::new(10, 5, now);
        assert_eq!(ctrl.min(), 10);
        assert_eq!(ctrl.max(), 10);
        assert_eq!(ctrl.current(), 10);
    }

    /// Zero min is floored to 1.
    #[test]
    fn zero_min_floored_to_one() {
        let now = Instant::now();
        let ctrl = AdaptiveConcurrency::new(0, 8, now);
        assert_eq!(ctrl.min(), 1);
        assert_eq!(ctrl.current(), 1);
    }

    // ── Advise: ramp up ─────────────────────────────────────────────

    /// Throughput increase ≥ 10% triggers doubling.
    ///
    /// Simulates two measurement windows where the second has higher
    /// throughput than the first.
    #[test]
    fn ramp_up_on_throughput_increase() {
        let base = Instant::now();
        let mut ctrl = AdaptiveConcurrency::new(2, 32, base);

        // First window: 1 MB
        ctrl.record_piece_completion(1_000_000);
        let t1 = base + Duration::from_secs(6);
        let a1 = ctrl.advise(t1);
        assert_eq!(a1, ConcurrencyAdvice::Hold); // baseline — no comparison
        ctrl.apply(a1);
        assert_eq!(ctrl.current(), 2);

        // Second window: 1.5 MB (50% increase → ≥ 10% threshold)
        ctrl.record_piece_completion(1_500_000);
        let t2 = t1 + Duration::from_secs(6);
        let a2 = ctrl.advise(t2);
        assert_eq!(
            a2,
            ConcurrencyAdvice::Increase { from: 2, to: 4 },
            "50% throughput increase should trigger doubling"
        );
        ctrl.apply(a2);
        assert_eq!(ctrl.current(), 4);
    }

    // ── Advise: scale down ──────────────────────────────────────────

    /// Throughput decrease ≥ 20% triggers halving.
    #[test]
    fn scale_down_on_throughput_decrease() {
        let base = Instant::now();
        let mut ctrl = AdaptiveConcurrency::new(2, 32, base);

        // Pump up to 8 first
        ctrl.current = 8;

        // First window: 2 MB
        ctrl.record_piece_completion(2_000_000);
        let t1 = base + Duration::from_secs(6);
        ctrl.advise(t1); // baseline

        // Second window: 1 MB (50% decrease → ≥ 20% threshold)
        ctrl.record_piece_completion(1_000_000);
        let t2 = t1 + Duration::from_secs(6);
        let a = ctrl.advise(t2);
        assert_eq!(
            a,
            ConcurrencyAdvice::Decrease { from: 8, to: 4 },
            "50% throughput decrease should trigger halving"
        );
        ctrl.apply(a);
        assert_eq!(ctrl.current(), 4);
    }

    // ── Advise: hold ────────────────────────────────────────────────

    /// Stable throughput (< 10% change) produces Hold.
    #[test]
    fn hold_on_stable_throughput() {
        let base = Instant::now();
        let mut ctrl = AdaptiveConcurrency::new(2, 32, base);

        // First window: 1 MB
        ctrl.record_piece_completion(1_000_000);
        let t1 = base + Duration::from_secs(6);
        ctrl.advise(t1);

        // Second window: 1.05 MB (5% increase — below 10% threshold)
        ctrl.record_piece_completion(1_050_000);
        let t2 = t1 + Duration::from_secs(6);
        let a = ctrl.advise(t2);
        assert_eq!(a, ConcurrencyAdvice::Hold);
    }

    /// Advise returns Hold before the measurement window elapses.
    #[test]
    fn hold_before_window_elapses() {
        let base = Instant::now();
        let mut ctrl = AdaptiveConcurrency::new(2, 32, base);
        ctrl.record_piece_completion(5_000_000);
        // Only 2 seconds elapsed — window is 5 seconds
        let early = base + Duration::from_secs(2);
        assert_eq!(ctrl.advise(early), ConcurrencyAdvice::Hold);
    }

    // ── Boundary: at limits ─────────────────────────────────────────

    /// Cannot increase past max — Hold returned instead.
    #[test]
    fn cannot_exceed_max() {
        let base = Instant::now();
        let mut ctrl = AdaptiveConcurrency::new(2, 4, base);
        ctrl.current = 4; // already at max

        ctrl.record_piece_completion(1_000_000);
        let t1 = base + Duration::from_secs(6);
        ctrl.advise(t1);

        ctrl.record_piece_completion(2_000_000);
        let t2 = t1 + Duration::from_secs(6);
        let a = ctrl.advise(t2);
        assert_eq!(
            a,
            ConcurrencyAdvice::Hold,
            "should Hold when already at max"
        );
    }

    /// Cannot decrease below min — Hold returned instead.
    #[test]
    fn cannot_go_below_min() {
        let base = Instant::now();
        let mut ctrl = AdaptiveConcurrency::new(4, 32, base);
        ctrl.current = 4; // already at min

        ctrl.record_piece_completion(2_000_000);
        let t1 = base + Duration::from_secs(6);
        ctrl.advise(t1);

        ctrl.record_piece_completion(1_000_000);
        let t2 = t1 + Duration::from_secs(6);
        let a = ctrl.advise(t2);
        assert_eq!(
            a,
            ConcurrencyAdvice::Hold,
            "should Hold when already at min"
        );
    }

    // ── Determinism ─────────────────────────────────────────────────

    /// Same input sequence produces same output sequence.
    #[test]
    fn deterministic_advice_sequence() {
        let run = || {
            let base = Instant::now();
            let mut ctrl = AdaptiveConcurrency::new(2, 32, base);
            let mut results = Vec::new();

            for i in 0..5 {
                ctrl.record_piece_completion((i + 1) * 500_000);
                let t = base + Duration::from_secs((i + 1) * 6);
                let a = ctrl.advise(t);
                results.push(a);
                ctrl.apply(a);
            }
            results
        };

        assert_eq!(run(), run());
    }

    // ── Apply ───────────────────────────────────────────────────────

    /// Apply Increase sets current to the `to` value.
    #[test]
    fn apply_increase() {
        let now = Instant::now();
        let mut ctrl = AdaptiveConcurrency::new(2, 32, now);
        ctrl.apply(ConcurrencyAdvice::Increase { from: 2, to: 4 });
        assert_eq!(ctrl.current(), 4);
    }

    /// Apply Decrease sets current to the `to` value.
    #[test]
    fn apply_decrease() {
        let now = Instant::now();
        let mut ctrl = AdaptiveConcurrency::new(2, 32, now);
        ctrl.current = 8;
        ctrl.apply(ConcurrencyAdvice::Decrease { from: 8, to: 4 });
        assert_eq!(ctrl.current(), 4);
    }

    /// Apply Hold does not change current.
    #[test]
    fn apply_hold_no_change() {
        let now = Instant::now();
        let mut ctrl = AdaptiveConcurrency::new(2, 32, now);
        ctrl.current = 8;
        ctrl.apply(ConcurrencyAdvice::Hold);
        assert_eq!(ctrl.current(), 8);
    }
}
