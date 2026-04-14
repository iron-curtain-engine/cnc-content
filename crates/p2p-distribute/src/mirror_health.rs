// SPDX-License-Identifier: MIT OR Apache-2.0

//! Mirror health tracking — availability monitoring, failure detection,
//! and automatic deprioritisation of degraded HTTP mirrors.
//!
//! ## What
//!
//! Provides a standalone mirror health registry that tracks per-mirror
//! response latency, success/failure counts, consecutive failure streaks,
//! and phi-based liveness scores. Mirrors are automatically classified
//! into health tiers (Healthy / Suspect / Degraded / Dead) and the
//! registry provides ordered selection of the best available mirrors.
//!
//! ## Why — lessons from C&C community mirror mortality
//!
//! The Command & Conquer community has operated freeware content mirrors
//! for nearly two decades. Key observed patterns:
//!
//! - **Mirrors die without warning.** `ppmsite.com` and `baxxster.no`
//!   both went offline permanently. Static mirror lists become stale
//!   without active health monitoring.
//! - **Mirrors degrade before dying.** A mirror often slows down (disk
//!   full, bandwidth exceeded, TLS cert expiry) before going fully
//!   offline. Latency tracking catches this before timeout.
//! - **Geographic distribution matters.** OpenRA runs EU mirrors
//!   (`cdn.mailaender.name`, `openra.0x47.net`), CnCNet runs US/EU
//!   infrastructure. Users benefit from selecting the fastest responding
//!   mirror, not just any live one.
//! - **Redundancy is mandatory.** Every download package needs at least
//!   two independent mirrors. The parallel mirror racing in
//!   `downloader.rs` already implements this at the transport level;
//!   this module provides the health bookkeeping layer.
//!
//! ## How
//!
//! - [`MirrorEntry`]: Per-mirror health record with latency stats,
//!   failure counts, and phi-based liveness.
//! - [`MirrorRegistry`]: Collection of mirrors with tier-based selection.
//! - [`HealthTier`]: Four-tier classification (Healthy → Dead) based on
//!   composite health score.
//! - Health score combines: success rate (40%), latency percentile (30%),
//!   phi liveness (20%), and recency (10%).
//!
//! The registry is designed to be embedded in [`BridgeNode`] or used
//! standalone by any component that needs to select from multiple HTTP
//! mirrors.

use std::time::{Duration, Instant};

use crate::phi_detector::PhiDetector;

// ── Constants ───────────────────────────────────────────────────────

/// Maximum number of latency samples retained per mirror.
///
/// Keeps memory bounded while providing enough history for meaningful
/// percentile calculations. 100 samples ≈ last 100 requests to this
/// mirror.
const MAX_LATENCY_SAMPLES: usize = 100;

/// Phi threshold below which a mirror is considered healthy.
const HEALTHY_PHI: f64 = 3.0;

/// Phi threshold above which a mirror is considered suspect but not dead.
const SUSPECT_PHI: f64 = 5.0;

/// Phi threshold above which a mirror is considered dead.
const DEAD_PHI: f64 = 8.0;

/// Consecutive failures before forced downgrade to Degraded tier.
///
/// Even if phi hasn't risen yet (e.g. the mirror was fast before it
/// died), three consecutive failures trigger immediate demotion.
const CONSECUTIVE_FAILURE_THRESHOLD: u32 = 3;

/// Duration after which a Dead mirror can be retried (probe interval).
///
/// Dead mirrors are retried periodically to detect recovery. Mirrors
/// that were live infrastructure (e.g. `files.cncnz.com`) may come back
/// after maintenance windows.
const DEAD_PROBE_INTERVAL: Duration = Duration::from_secs(300);

/// Weight of success rate in composite health score.
const WEIGHT_SUCCESS_RATE: f64 = 0.4;

/// Weight of latency score in composite health score.
const WEIGHT_LATENCY: f64 = 0.3;

/// Weight of phi liveness in composite health score.
const WEIGHT_PHI: f64 = 0.2;

/// Weight of recency in composite health score.
const WEIGHT_RECENCY: f64 = 0.1;

// ── Health tier ─────────────────────────────────────────────────────

/// Classification of a mirror's operational health.
///
/// Tiers drive selection priority: Healthy mirrors are preferred over
/// Suspect, which are preferred over Degraded. Dead mirrors are only
/// used as a last resort or probed periodically for recovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HealthTier {
    /// Mirror is responsive, fast, and reliable.
    Healthy,
    /// Mirror is showing signs of degradation (elevated latency or
    /// occasional failures) but is still usable.
    Suspect,
    /// Mirror has significant problems (high failure rate, very slow
    /// responses) but hasn't been declared dead.
    Degraded,
    /// Mirror is considered offline. Will be retried after the probe
    /// interval to detect recovery.
    Dead,
}

// ── Mirror entry ────────────────────────────────────────────────────

/// Health and performance record for a single HTTP mirror.
///
/// Tracks cumulative statistics (total requests, successes, failures)
/// and a sliding window of recent latency samples. The phi detector
/// monitors response inter-arrival times to detect mirrors that stop
/// responding.
#[derive(Debug)]
pub struct MirrorEntry {
    /// Base URL of the mirror.
    url: String,

    /// Phi accrual failure detector for this mirror.
    phi: PhiDetector,

    /// Recent response latencies (bounded sliding window).
    latencies: Vec<Duration>,

    /// Total number of requests sent to this mirror.
    total_requests: u64,

    /// Total number of successful responses.
    total_successes: u64,

    /// Consecutive failures without a success in between.
    consecutive_failures: u32,

    /// When the last successful response was received.
    last_success: Option<Instant>,

    /// When this mirror was last declared dead (for probe scheduling).
    last_dead_at: Option<Instant>,

    /// Current health tier (cached, updated on each record call).
    tier: HealthTier,
}

impl MirrorEntry {
    /// Creates a new mirror entry with no history.
    ///
    /// New mirrors start as Healthy with the assumption that they were
    /// provided from a curated mirror list. The first few requests will
    /// quickly reclassify them if they are actually dead.
    pub fn new(url: String) -> Self {
        Self {
            url,
            phi: PhiDetector::new(std::time::Instant::now()),
            latencies: Vec::with_capacity(MAX_LATENCY_SAMPLES),
            total_requests: 0,
            total_successes: 0,
            consecutive_failures: 0,
            last_success: None,
            last_dead_at: None,
            tier: HealthTier::Healthy,
        }
    }

    /// Returns this mirror's base URL.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Returns the current health tier.
    pub fn tier(&self) -> HealthTier {
        self.tier
    }

    /// Returns total requests recorded.
    pub fn total_requests(&self) -> u64 {
        self.total_requests
    }

    /// Returns total successful responses.
    pub fn total_successes(&self) -> u64 {
        self.total_successes
    }

    /// Returns the success rate as a fraction in `[0.0, 1.0]`.
    ///
    /// Returns 1.0 for mirrors with no requests (benefit of the doubt).
    pub fn success_rate(&self) -> f64 {
        if self.total_requests == 0 {
            return 1.0;
        }
        self.total_successes as f64 / self.total_requests as f64
    }

    /// Returns the median latency, or `None` if no samples exist.
    pub fn median_latency(&self) -> Option<Duration> {
        if self.latencies.is_empty() {
            return None;
        }
        let mut sorted: Vec<Duration> = self.latencies.clone();
        sorted.sort();
        // Safe: we checked non-empty above.
        sorted.get(sorted.len() / 2).copied()
    }

    /// Returns the 90th percentile latency, or `None` if insufficient
    /// samples.
    pub fn p90_latency(&self) -> Option<Duration> {
        if self.latencies.len() < 5 {
            return None;
        }
        let mut sorted: Vec<Duration> = self.latencies.clone();
        sorted.sort();
        let idx = (sorted.len() as f64 * 0.9) as usize;
        sorted.get(idx.min(sorted.len().saturating_sub(1))).copied()
    }

    /// Records a successful response with the given latency.
    pub fn record_success(&mut self, latency: Duration, now: Instant) {
        self.total_requests = self.total_requests.saturating_add(1);
        self.total_successes = self.total_successes.saturating_add(1);
        self.consecutive_failures = 0;
        self.last_success = Some(now);
        self.phi.record_heartbeat(now);

        // Sliding window: drop oldest sample if full.
        if self.latencies.len() >= MAX_LATENCY_SAMPLES {
            self.latencies.remove(0);
        }
        self.latencies.push(latency);

        self.update_tier(now);
    }

    /// Records a failed request to this mirror.
    pub fn record_failure(&mut self, now: Instant) {
        self.total_requests = self.total_requests.saturating_add(1);
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        self.update_tier(now);
    }

    /// Returns whether a Dead mirror is eligible for a recovery probe.
    ///
    /// Dead mirrors are retried after `DEAD_PROBE_INTERVAL` to detect
    /// if they have come back online (e.g. after maintenance).
    pub fn should_probe(&self, now: Instant) -> bool {
        if self.tier != HealthTier::Dead {
            return false;
        }
        match self.last_dead_at {
            Some(dead_at) => now.duration_since(dead_at) >= DEAD_PROBE_INTERVAL,
            // No dead timestamp means just declared dead — wait for probe interval.
            None => false,
        }
    }

    /// Computes the composite health score in `[0.0, 1.0]`.
    ///
    /// Higher is better. Combines success rate, latency performance,
    /// phi liveness, and recency of last successful response.
    pub fn health_score(&self, now: Instant) -> f64 {
        let success_component = self.success_rate() * WEIGHT_SUCCESS_RATE;

        // Latency score: inverse of normalised median latency.
        // 0ms → 1.0, 10000ms → ~0.0. Assumes >10s mirrors are unusable.
        let latency_component = match self.median_latency() {
            Some(d) => {
                let ms = d.as_millis() as f64;
                (1.0 - (ms / 10_000.0).min(1.0)) * WEIGHT_LATENCY
            }
            None => WEIGHT_LATENCY, // No data → assume OK.
        };

        // Phi score: inverse of normalised phi value.
        // phi=0 → 1.0, phi≥DEAD_PHI → 0.0.
        let phi_val = self.phi.phi(now);
        let phi_component = (1.0 - (phi_val / DEAD_PHI).min(1.0)) * WEIGHT_PHI;

        // Recency: how recently the mirror last succeeded.
        // <10s ago → 1.0, >300s ago → 0.0.
        let recency_component = match self.last_success {
            Some(t) => {
                let secs = now.duration_since(t).as_secs_f64();
                (1.0 - (secs / 300.0).min(1.0)) * WEIGHT_RECENCY
            }
            None => 0.0, // Never succeeded → no recency bonus.
        };

        success_component + latency_component + phi_component + recency_component
    }

    /// Consecutive failure count.
    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures
    }

    // ── Internal ─────────────────────────────────────────────────────

    /// Reclassifies this mirror's health tier based on current state.
    fn update_tier(&mut self, now: Instant) {
        let phi_val = self.phi.phi(now);
        let old_tier = self.tier;

        self.tier =
            if self.consecutive_failures >= CONSECUTIVE_FAILURE_THRESHOLD || phi_val >= DEAD_PHI {
                HealthTier::Dead
            } else if phi_val >= SUSPECT_PHI || self.success_rate() < 0.5 {
                HealthTier::Degraded
            } else if phi_val >= HEALTHY_PHI || self.success_rate() < 0.9 {
                HealthTier::Suspect
            } else {
                HealthTier::Healthy
            };

        // Track when mirror transitions to Dead for probe scheduling.
        if self.tier == HealthTier::Dead && old_tier != HealthTier::Dead {
            self.last_dead_at = Some(now);
        }
    }
}

// ── Mirror registry ─────────────────────────────────────────────────

/// Collection of HTTP mirrors with health-aware selection.
///
/// The registry maintains an ordered set of mirrors. Selection prefers
/// mirrors in better health tiers, breaking ties by composite health
/// score. Dead mirrors are excluded from normal selection but can be
/// probed for recovery.
///
/// ## Usage
///
/// ```
/// use p2p_distribute::mirror_health::{MirrorRegistry, HealthTier};
/// use std::time::{Duration, Instant};
///
/// let mut registry = MirrorRegistry::new(16);
/// let now = Instant::now();
///
/// registry.add("https://mirror-a.example.com/content/".into()).unwrap();
/// registry.add("https://mirror-b.example.com/content/".into()).unwrap();
///
/// // Record activity
/// registry.record_success("https://mirror-a.example.com/content/",
///     Duration::from_millis(50), now);
/// registry.record_failure("https://mirror-b.example.com/content/", now);
///
/// // Select best mirrors
/// let best = registry.select_best(now);
/// assert!(!best.is_empty());
/// ```
pub struct MirrorRegistry {
    /// All tracked mirrors, keyed by URL.
    mirrors: Vec<MirrorEntry>,
    /// Maximum number of mirrors the registry can hold.
    max_mirrors: usize,
}

impl MirrorRegistry {
    /// Creates a new empty registry with the given capacity limit.
    pub fn new(max_mirrors: usize) -> Self {
        Self {
            mirrors: Vec::with_capacity(max_mirrors.min(64)),
            max_mirrors,
        }
    }

    /// Adds a mirror to the registry.
    ///
    /// Returns an error if the registry is full or the URL is a duplicate.
    pub fn add(&mut self, url: String) -> Result<(), MirrorRegistryError> {
        if self.mirrors.len() >= self.max_mirrors {
            return Err(MirrorRegistryError::Full {
                max: self.max_mirrors,
            });
        }
        if self.mirrors.iter().any(|m| m.url == url) {
            return Err(MirrorRegistryError::Duplicate { url });
        }
        self.mirrors.push(MirrorEntry::new(url));
        Ok(())
    }

    /// Records a successful response for the given mirror URL.
    pub fn record_success(&mut self, url: &str, latency: Duration, now: Instant) {
        if let Some(entry) = self.mirrors.iter_mut().find(|m| m.url == url) {
            entry.record_success(latency, now);
        }
    }

    /// Records a failure for the given mirror URL.
    pub fn record_failure(&mut self, url: &str, now: Instant) {
        if let Some(entry) = self.mirrors.iter_mut().find(|m| m.url == url) {
            entry.record_failure(now);
        }
    }

    /// Returns all mirrors sorted by health (best first).
    ///
    /// Dead mirrors are excluded unless they are eligible for probing.
    pub fn select_best(&self, now: Instant) -> Vec<&MirrorEntry> {
        let mut candidates: Vec<&MirrorEntry> = self
            .mirrors
            .iter()
            .filter(|m| m.tier() != HealthTier::Dead || m.should_probe(now))
            .collect();

        // Sort by tier (Healthy first), then by health score (descending).
        candidates.sort_by(|a, b| {
            a.tier().cmp(&b.tier()).then_with(|| {
                b.health_score(now)
                    .partial_cmp(&a.health_score(now))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
        });

        candidates
    }

    /// Returns mirrors eligible for dead-probe recovery checks.
    pub fn dead_probeable(&self, now: Instant) -> Vec<&MirrorEntry> {
        self.mirrors
            .iter()
            .filter(|m| m.should_probe(now))
            .collect()
    }

    /// Returns the number of mirrors in the registry.
    pub fn len(&self) -> usize {
        self.mirrors.len()
    }

    /// Returns whether the registry has no mirrors.
    pub fn is_empty(&self) -> bool {
        self.mirrors.is_empty()
    }

    /// Returns a reference to a mirror by URL.
    pub fn get(&self, url: &str) -> Option<&MirrorEntry> {
        self.mirrors.iter().find(|m| m.url == url)
    }

    /// Returns the count of mirrors at each health tier.
    pub fn tier_counts(&self, now: Instant) -> TierCounts {
        let mut counts = TierCounts::default();
        for mirror in &self.mirrors {
            match mirror.tier() {
                HealthTier::Healthy => counts.healthy = counts.healthy.saturating_add(1),
                HealthTier::Suspect => counts.suspect = counts.suspect.saturating_add(1),
                HealthTier::Degraded => counts.degraded = counts.degraded.saturating_add(1),
                HealthTier::Dead => {
                    if mirror.should_probe(now) {
                        counts.dead_probeable = counts.dead_probeable.saturating_add(1);
                    } else {
                        counts.dead = counts.dead.saturating_add(1);
                    }
                }
            }
        }
        counts
    }

    /// Removes all mirrors that have been Dead for longer than the
    /// given duration with zero successful probes since going dead.
    pub fn prune_dead(&mut self, older_than: Duration, now: Instant) -> usize {
        let before = self.mirrors.len();
        self.mirrors.retain(|m| {
            if m.tier() != HealthTier::Dead {
                return true;
            }
            match m.last_dead_at {
                Some(dead_at) => now.duration_since(dead_at) < older_than,
                None => true,
            }
        });
        before.saturating_sub(self.mirrors.len())
    }
}

// ── Tier counts ─────────────────────────────────────────────────────

/// Summary of mirrors at each health tier.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TierCounts {
    /// Mirrors responding normally.
    pub healthy: usize,
    /// Mirrors showing early degradation signs.
    pub suspect: usize,
    /// Mirrors with significant problems.
    pub degraded: usize,
    /// Dead mirrors not yet eligible for probing.
    pub dead: usize,
    /// Dead mirrors eligible for recovery probe.
    pub dead_probeable: usize,
}

// ── Error ───────────────────────────────────────────────────────────

/// Errors from mirror registry operations.
#[derive(Debug, thiserror::Error)]
pub enum MirrorRegistryError {
    /// Registry has reached its capacity limit.
    #[error("mirror registry full: {max} maximum mirrors")]
    Full {
        /// Capacity limit.
        max: usize,
    },

    /// A mirror with this URL already exists.
    #[error("duplicate mirror URL: {url}")]
    Duplicate {
        /// The existing URL.
        url: String,
    },
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── MirrorEntry basics ──────────────────────────────────────────

    /// New mirror starts as Healthy with no history.
    ///
    /// Fresh mirrors from curated lists deserve the benefit of the
    /// doubt until proven otherwise.
    #[test]
    fn new_mirror_is_healthy() {
        let entry = MirrorEntry::new("https://example.com".into());
        assert_eq!(entry.tier(), HealthTier::Healthy);
        assert_eq!(entry.total_requests(), 0);
        assert_eq!(entry.success_rate(), 1.0);
    }

    /// Success rate tracks correctly across mixed results.
    ///
    /// Ensures the success/total ratio is computed without
    /// integer truncation.
    #[test]
    fn success_rate_mixed() {
        let mut entry = MirrorEntry::new("https://example.com".into());
        let now = Instant::now();
        let latency = Duration::from_millis(50);

        entry.record_success(latency, now);
        entry.record_success(latency, now);
        entry.record_failure(now);

        let rate = entry.success_rate();
        assert!((rate - 2.0 / 3.0).abs() < 0.01, "rate = {rate}");
    }

    /// Consecutive failures trigger demotion to Dead tier.
    ///
    /// Three consecutive failures without any intervening success
    /// should immediately classify the mirror as Dead, regardless of
    /// phi score.
    #[test]
    fn consecutive_failures_demote_to_dead() {
        let mut entry = MirrorEntry::new("https://example.com".into());
        let now = Instant::now();

        for _ in 0..CONSECUTIVE_FAILURE_THRESHOLD {
            entry.record_failure(now);
        }

        assert_eq!(entry.tier(), HealthTier::Dead);
    }

    /// A success resets consecutive failure counter.
    ///
    /// Intermittent failures should not accumulate across success
    /// boundaries.
    #[test]
    fn success_resets_consecutive_failures() {
        let mut entry = MirrorEntry::new("https://example.com".into());
        let now = Instant::now();

        entry.record_failure(now);
        entry.record_failure(now);
        entry.record_success(Duration::from_millis(50), now);

        assert_eq!(entry.consecutive_failures(), 0);
        assert_ne!(entry.tier(), HealthTier::Dead);
    }

    /// Median latency is computed from the sample window.
    ///
    /// The median is a robust central tendency measure that ignores
    /// outlier spikes.
    #[test]
    fn median_latency_computed() {
        let mut entry = MirrorEntry::new("https://example.com".into());
        let now = Instant::now();

        for ms in &[10, 20, 30, 40, 50] {
            entry.record_success(Duration::from_millis(*ms), now);
        }

        let median = entry.median_latency().unwrap();
        assert_eq!(median, Duration::from_millis(30));
    }

    /// Health score is positive for a healthy mirror.
    ///
    /// A mirror with all successes and low latency should have a
    /// health score near 1.0.
    #[test]
    fn health_score_healthy_mirror() {
        let mut entry = MirrorEntry::new("https://example.com".into());
        let now = Instant::now();

        for _ in 0..10 {
            entry.record_success(Duration::from_millis(20), now);
        }

        let score = entry.health_score(now);
        assert!(score > 0.5, "healthy mirror score = {score}");
    }

    /// Dead mirror is eligible for probe after interval.
    ///
    /// Mirrors that go dead should be retried periodically to detect
    /// recovery after maintenance windows.
    #[test]
    fn dead_mirror_probe_eligibility() {
        let mut entry = MirrorEntry::new("https://example.com".into());
        let now = Instant::now();

        for _ in 0..CONSECUTIVE_FAILURE_THRESHOLD {
            entry.record_failure(now);
        }
        assert_eq!(entry.tier(), HealthTier::Dead);
        assert!(!entry.should_probe(now));

        // After probe interval, should be eligible.
        let later = now + DEAD_PROBE_INTERVAL + Duration::from_secs(1);
        assert!(entry.should_probe(later));
    }

    // ── MirrorRegistry ──────────────────────────────────────────────

    /// Registry rejects duplicates.
    ///
    /// Duplicate mirror URLs would corrupt health tracking since both
    /// entries would receive the same updates.
    #[test]
    fn registry_rejects_duplicate() {
        let mut reg = MirrorRegistry::new(10);
        reg.add("https://a.example.com".into()).unwrap();
        let err = reg.add("https://a.example.com".into()).unwrap_err();
        assert!(err.to_string().contains("duplicate"), "error = {err}");
    }

    /// Registry rejects when full.
    ///
    /// Bounded capacity prevents unbounded memory growth from malicious
    /// or misconfigured mirror lists.
    #[test]
    fn registry_rejects_when_full() {
        let mut reg = MirrorRegistry::new(2);
        reg.add("https://a.example.com".into()).unwrap();
        reg.add("https://b.example.com".into()).unwrap();
        let err = reg.add("https://c.example.com".into()).unwrap_err();
        assert!(err.to_string().contains("full"), "error = {err}");
    }

    /// Select best returns healthy mirrors first.
    ///
    /// The selection should sort by tier (Healthy > Suspect > Degraded)
    /// and break ties by health score.
    #[test]
    fn select_best_prefers_healthy() {
        let mut reg = MirrorRegistry::new(10);
        let now = Instant::now();

        reg.add("https://healthy.example.com".into()).unwrap();
        reg.add("https://failing.example.com".into()).unwrap();

        // Make one mirror healthy, one degraded.
        for _ in 0..5 {
            reg.record_success(
                "https://healthy.example.com",
                Duration::from_millis(20),
                now,
            );
        }
        for _ in 0..CONSECUTIVE_FAILURE_THRESHOLD {
            reg.record_failure("https://failing.example.com", now);
        }

        let best = reg.select_best(now);
        // Healthy mirror should be first (Dead ones excluded unless probeable).
        assert!(!best.is_empty());
        assert_eq!(best.first().unwrap().url(), "https://healthy.example.com");
    }

    /// Prune dead removes old dead mirrors.
    ///
    /// Dead mirrors beyond the retention threshold should be garbage
    /// collected to free registry slots for new mirrors.
    #[test]
    fn prune_dead_removes_old() {
        let mut reg = MirrorRegistry::new(10);
        let now = Instant::now();

        reg.add("https://dead.example.com".into()).unwrap();
        for _ in 0..CONSECUTIVE_FAILURE_THRESHOLD {
            reg.record_failure("https://dead.example.com", now);
        }

        // Not yet old enough.
        let pruned = reg.prune_dead(Duration::from_secs(600), now);
        assert_eq!(pruned, 0);

        // After retention period.
        let later = now + Duration::from_secs(601);
        let pruned = reg.prune_dead(Duration::from_secs(600), later);
        assert_eq!(pruned, 1);
        assert!(reg.is_empty());
    }

    /// Tier counts reflect actual mirror state.
    ///
    /// Ensures the summary correctly categorises all mirrors.
    #[test]
    fn tier_counts_accurate() {
        let mut reg = MirrorRegistry::new(10);
        let now = Instant::now();

        reg.add("https://a.example.com".into()).unwrap();
        reg.add("https://b.example.com".into()).unwrap();

        // a is healthy by default, b goes dead.
        for _ in 0..CONSECUTIVE_FAILURE_THRESHOLD {
            reg.record_failure("https://b.example.com", now);
        }

        let counts = reg.tier_counts(now);
        assert_eq!(counts.healthy, 1);
        assert_eq!(counts.dead, 1);
    }

    /// P90 latency requires minimum samples.
    ///
    /// Percentile calculations are meaningless with too few data points.
    #[test]
    fn p90_requires_minimum_samples() {
        let mut entry = MirrorEntry::new("https://example.com".into());
        let now = Instant::now();

        // Only 3 samples — insufficient for p90.
        for ms in &[10, 20, 30] {
            entry.record_success(Duration::from_millis(*ms), now);
        }
        assert!(entry.p90_latency().is_none());

        // Add enough for p90.
        for ms in &[40, 50] {
            entry.record_success(Duration::from_millis(*ms), now);
        }
        assert!(entry.p90_latency().is_some());
    }

    /// Empty mirror has no median latency.
    ///
    /// Edge case: median is undefined for zero samples.
    #[test]
    fn empty_mirror_no_median() {
        let entry = MirrorEntry::new("https://example.com".into());
        assert!(entry.median_latency().is_none());
    }

    /// Get retrieves mirror entry by URL.
    ///
    /// Lookup must be exact match — no fuzzy URL matching.
    #[test]
    fn get_by_url() {
        let mut reg = MirrorRegistry::new(10);
        reg.add("https://a.example.com".into()).unwrap();

        assert!(reg.get("https://a.example.com").is_some());
        assert!(reg.get("https://b.example.com").is_none());
    }

    /// Success recording updates the correct mirror.
    ///
    /// Recording to a URL that doesn't exist should be silently ignored
    /// (defensive — the downloader may race with registry updates).
    #[test]
    fn record_to_unknown_mirror_is_noop() {
        let mut reg = MirrorRegistry::new(10);
        let now = Instant::now();

        // Should not panic.
        reg.record_success("https://nonexistent.com", Duration::from_millis(10), now);
        reg.record_failure("https://nonexistent.com", now);
    }

    /// Latency window is bounded.
    ///
    /// Ensures old samples are evicted to prevent unbounded memory
    /// growth from long-running sessions.
    #[test]
    fn latency_window_bounded() {
        let mut entry = MirrorEntry::new("https://example.com".into());
        let now = Instant::now();

        for i in 0..200 {
            entry.record_success(Duration::from_millis(i), now);
        }

        assert!(
            entry.latencies.len() <= MAX_LATENCY_SAMPLES,
            "latency window grew to {}",
            entry.latencies.len()
        );
    }
}
