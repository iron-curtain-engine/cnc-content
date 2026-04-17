// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025-present Iron Curtain contributors

//! Mirror health tracking — per-mirror success/failure/speed history for
//! intelligent mirror selection.
//!
//! ## What
//!
//! Tracks per-mirror URL health metrics: success count, failure count,
//! average speed, and last-seen timestamp. The downloader uses
//! [`MirrorHealthTracker::ranked_urls()`] to sort mirrors by health score
//! before racing, putting reliable fast mirrors first.
//!
//! ## Why — eMule DeadSourceList + aMule source management lesson
//!
//! aMule maintains per-source health state: sources that return errors are
//! temporarily excluded (DeadSourceList), and sources are ranked by observed
//! transfer speed. This prevents wasting time on consistently failing or
//! slow mirrors.
//!
//! For cnc-content's HTTP mirror racing, this means:
//!
//! - Mirrors that returned 404/500 are deprioritised or temporarily excluded.
//! - Mirrors with higher average speed are tried first.
//! - Dead mirrors get a cool-off period before retry.
//!
//! ## How
//!
//! The tracker maintains a `HashMap<String, MirrorHealth>` keyed by URL.
//! After each download attempt:
//!
//! 1. Call [`record_success()`](MirrorHealthTracker::record_success) or
//!    [`record_failure()`](MirrorHealthTracker::record_failure).
//! 2. Before starting a download, call [`ranked_urls()`](MirrorHealthTracker::ranked_urls)
//!    to get mirrors sorted by health score (highest first).
//! 3. Optionally call [`filter_available()`](MirrorHealthTracker::filter_available) to remove
//!    stale entries.

use std::collections::HashMap;
use std::time::Instant;

// ── Constants ───────────────────────────────────────────────────────

/// Duration in seconds before a failed mirror is retried.
///
/// First failure: 5 minutes. Doubles on each consecutive failure up to
/// [`MAX_COOLOFF_SECS`]. Matches aMule DeadSourceList timing.
const BASE_COOLOFF_SECS: u64 = 300;

/// Maximum cool-off cap — 1 hour.
const MAX_COOLOFF_SECS: u64 = 3600;

// ── MirrorHealth ────────────────────────────────────────────────────

/// Health state for a single mirror URL.
///
/// Tracks success/failure counts, average speed, and cool-off state for
/// timed exclusion after failures.
#[derive(Debug, Clone)]
pub struct MirrorHealth {
    /// Total successful downloads from this mirror.
    pub success_count: u32,
    /// Total failed download attempts from this mirror.
    pub failure_count: u32,
    /// Consecutive failure count (for exponential cool-off).
    pub consecutive_failures: u32,
    /// Average download speed in bytes/sec (EWMA, α=0.3).
    pub avg_speed_bps: f64,
    /// Timestamp of last successful download.
    pub last_success: Option<Instant>,
    /// Timestamp of last failure.
    pub last_failure: Option<Instant>,
    /// Cool-off expiry: mirror is excluded until this time.
    pub cooloff_until: Option<Instant>,
}

impl MirrorHealth {
    /// Creates a new health entry with no history.
    fn new() -> Self {
        Self {
            success_count: 0,
            failure_count: 0,
            consecutive_failures: 0,
            avg_speed_bps: 0.0,
            last_success: None,
            last_failure: None,
            cooloff_until: None,
        }
    }

    /// Records a successful download with the observed speed.
    fn record_success(&mut self, speed_bps: f64, now: Instant) {
        self.success_count = self.success_count.saturating_add(1);
        self.consecutive_failures = 0;
        self.cooloff_until = None;
        self.last_success = Some(now);

        // EWMA speed tracking (α=0.3).
        if self.avg_speed_bps == 0.0 {
            self.avg_speed_bps = speed_bps;
        } else {
            self.avg_speed_bps = 0.3 * speed_bps + 0.7 * self.avg_speed_bps;
        }
    }

    /// Records a failed download attempt and applies exponential cool-off.
    fn record_failure(&mut self, now: Instant) {
        self.failure_count = self.failure_count.saturating_add(1);
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        self.last_failure = Some(now);

        // Exponential cool-off: base × 2^(n-1), capped.
        let exponent = self.consecutive_failures.saturating_sub(1).min(6);
        let cooloff_secs = BASE_COOLOFF_SECS
            .saturating_mul(1u64 << exponent)
            .min(MAX_COOLOFF_SECS);
        let cooloff_duration = std::time::Duration::from_secs(cooloff_secs);
        self.cooloff_until = now.checked_add(cooloff_duration);
    }

    /// Whether this mirror is currently in cool-off (should be skipped).
    fn is_cooling_off(&self, now: Instant) -> bool {
        self.cooloff_until.is_some_and(|t| now < t)
    }

    /// Computes a health score (higher = better).
    ///
    /// Combines success rate and speed. Mirrors in cool-off get score 0.
    fn health_score(&self, now: Instant) -> u64 {
        if self.is_cooling_off(now) {
            return 0;
        }

        let total = self.success_count.saturating_add(self.failure_count);
        let success_rate = if total > 0 {
            self.success_count as f64 / total as f64
        } else {
            0.5 // benefit of the doubt
        };

        // Score = success_rate * 500 + speed_factor * 500.
        // Speed factor: normalised to [0, 1] assuming max reasonable speed
        // of 100 MB/s.
        let speed_factor = (self.avg_speed_bps / 100_000_000.0).min(1.0);
        let raw = success_rate * 500.0 + speed_factor * 500.0;

        raw as u64
    }
}

// ── MirrorHealthTracker ─────────────────────────────────────────────

/// Tracks health state for all known mirror URLs.
///
/// ```
/// use cnc_content::downloader::mirror_health::{MirrorHealthTracker};
/// use std::time::Instant;
///
/// let mut tracker = MirrorHealthTracker::new();
/// let now = Instant::now();
///
/// tracker.record_success("https://mirror-a.example.com/file.zip", 5_000_000.0, now);
/// tracker.record_failure("https://mirror-b.example.com/file.zip", now);
///
/// let urls = vec![
///     "https://mirror-a.example.com/file.zip".to_string(),
///     "https://mirror-b.example.com/file.zip".to_string(),
/// ];
/// let ranked = tracker.ranked_urls(&urls, now);
///
/// // mirror-a should be ranked first (successful vs failed).
/// assert_eq!(ranked[0], "https://mirror-a.example.com/file.zip");
/// ```
#[derive(Debug, Clone, Default)]
pub struct MirrorHealthTracker {
    /// Per-URL health entries.
    mirrors: HashMap<String, MirrorHealth>,
}

impl MirrorHealthTracker {
    /// Creates an empty tracker.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a successful download from a mirror.
    pub fn record_success(&mut self, url: &str, speed_bps: f64, now: Instant) {
        self.mirrors
            .entry(url.to_string())
            .or_insert_with(MirrorHealth::new)
            .record_success(speed_bps, now);
    }

    /// Records a failed download attempt from a mirror.
    pub fn record_failure(&mut self, url: &str, now: Instant) {
        self.mirrors
            .entry(url.to_string())
            .or_insert_with(MirrorHealth::new)
            .record_failure(now);
    }

    /// Returns the health entry for a mirror, if tracked.
    pub fn get(&self, url: &str) -> Option<&MirrorHealth> {
        self.mirrors.get(url)
    }

    /// Returns whether a mirror is currently in cool-off.
    pub fn is_cooling_off(&self, url: &str, now: Instant) -> bool {
        self.mirrors.get(url).is_some_and(|h| h.is_cooling_off(now))
    }

    /// Returns URLs sorted by health score (highest first).
    ///
    /// URLs not tracked yet are placed after known-good mirrors but before
    /// known-bad ones (benefit of the doubt). URLs in cool-off are placed
    /// last.
    pub fn ranked_urls(&self, urls: &[String], now: Instant) -> Vec<String> {
        let mut scored: Vec<(String, u64)> = urls
            .iter()
            .map(|url| {
                let score = self
                    .mirrors
                    .get(url.as_str())
                    .map(|h| h.health_score(now))
                    .unwrap_or(250); // Unknown mirrors: middle score
                (url.clone(), score)
            })
            .collect();

        // Sort descending by score, stable within same score.
        scored.sort_by_key(|b| std::cmp::Reverse(b.1));
        scored.into_iter().map(|(url, _)| url).collect()
    }

    /// Filters out mirrors currently in cool-off from a URL list.
    pub fn filter_available(&self, urls: &[String], now: Instant) -> Vec<String> {
        urls.iter()
            .filter(|url| !self.is_cooling_off(url, now))
            .cloned()
            .collect()
    }

    /// Number of tracked mirrors.
    pub fn mirror_count(&self) -> usize {
        self.mirrors.len()
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    // ── MirrorHealth ────────────────────────────────────────────────

    /// A newly created `MirrorHealth` starts with all counters at zero.
    ///
    /// The initial state must not bias the ranking algorithm — a mirror that
    /// has never been tried should start neutral, not penalized or preferred.
    #[test]
    fn new_mirror_health_defaults() {
        let h = MirrorHealth::new();
        assert_eq!(h.success_count, 0);
        assert_eq!(h.failure_count, 0);
        assert_eq!(h.consecutive_failures, 0);
    }

    /// Success clears cool-off and resets consecutive failures.
    ///
    /// A mirror that was in cool-off should be usable again after a success.
    #[test]
    fn success_clears_cooloff() {
        let now = Instant::now();
        let mut h = MirrorHealth::new();
        h.record_failure(now);
        assert!(h.is_cooling_off(now));

        h.record_success(1_000_000.0, now);
        assert!(!h.is_cooling_off(now));
        assert_eq!(h.consecutive_failures, 0);
    }

    /// Consecutive failures extend cool-off exponentially.
    ///
    /// Each failure doubles the cool-off period: 5min → 10min → 20min → …
    #[test]
    fn consecutive_failures_extend_cooloff() {
        let now = Instant::now();
        let mut h = MirrorHealth::new();

        h.record_failure(now);
        let first_cooloff = h.cooloff_until.unwrap();

        h.record_failure(now);
        let second_cooloff = h.cooloff_until.unwrap();

        assert!(second_cooloff > first_cooloff);
    }

    // ── MirrorHealthTracker ─────────────────────────────────────────

    /// An empty tracker reports zero mirrors.
    ///
    /// The tracker must start in a clean state so that the first mirror
    /// observation is not confused with a previous session's data.
    #[test]
    fn empty_tracker_default_scores() {
        let tracker = MirrorHealthTracker::new();
        assert_eq!(tracker.mirror_count(), 0);
    }

    /// Successful mirrors rank above failed mirrors.
    ///
    /// This is the core invariant: the downloader should try reliable
    /// mirrors first.
    #[test]
    fn successful_mirrors_rank_higher() {
        let mut tracker = MirrorHealthTracker::new();
        let now = Instant::now();

        tracker.record_success("https://good.example.com/f", 5_000_000.0, now);
        tracker.record_failure("https://bad.example.com/f", now);

        let urls = vec![
            "https://bad.example.com/f".to_string(),
            "https://good.example.com/f".to_string(),
        ];
        let ranked = tracker.ranked_urls(&urls, now);
        assert_eq!(ranked[0], "https://good.example.com/f");
    }

    /// Unknown mirrors rank between good and bad.
    ///
    /// New mirrors get benefit of the doubt (score 250) but don't outrank
    /// proven good mirrors.
    #[test]
    fn unknown_mirrors_rank_middle() {
        let mut tracker = MirrorHealthTracker::new();
        let now = Instant::now();

        tracker.record_success("https://good.example.com/f", 5_000_000.0, now);
        // "unknown" is not in tracker

        let urls = vec![
            "https://unknown.example.com/f".to_string(),
            "https://good.example.com/f".to_string(),
        ];
        let ranked = tracker.ranked_urls(&urls, now);
        assert_eq!(ranked[0], "https://good.example.com/f");
    }

    /// Cooling-off mirrors are removed from the available pool.
    ///
    /// The downloader must not retry a mirror that is known to be failing;
    /// including it would waste time and delay the user's download.
    #[test]
    fn filter_available_excludes_cooloff() {
        let mut tracker = MirrorHealthTracker::new();
        let now = Instant::now();

        tracker.record_failure("https://dead.example.com/f", now);

        let urls = vec![
            "https://dead.example.com/f".to_string(),
            "https://alive.example.com/f".to_string(),
        ];
        let available = tracker.filter_available(&urls, now);
        assert_eq!(available.len(), 1);
        assert_eq!(available[0], "https://alive.example.com/f");
    }

    /// A mirror's cool-off period expires after the base backoff duration.
    ///
    /// Mirrors must eventually re-enter the available pool after a transient
    /// failure; permanent exclusion would shrink the active mirror set to
    /// zero after enough flaky responses.
    #[test]
    fn cooloff_expires() {
        let now = Instant::now();
        let mut tracker = MirrorHealthTracker::new();
        tracker.record_failure("https://temp.example.com/f", now);
        assert!(tracker.is_cooling_off("https://temp.example.com/f", now));

        // After BASE_COOLOFF_SECS, should no longer be cooling off.
        let later = now + Duration::from_secs(BASE_COOLOFF_SECS + 1);
        assert!(!tracker.is_cooling_off("https://temp.example.com/f", later));
    }
}
