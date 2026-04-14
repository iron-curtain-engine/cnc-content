// SPDX-License-Identifier: MIT OR Apache-2.0

//! Peer affinity — geographic and topological peer selection preferences.
//!
//! ## What
//!
//! Provides peer scoring based on network proximity, region matching,
//! and observed transfer performance. The affinity system biases peer
//! selection toward topologically close peers without hard-excluding
//! distant ones.
//!
//! ## Why — lessons from C&C community regional distribution
//!
//! The C&C community's infrastructure is geographically distributed:
//!
//! - **OpenRA** runs EU mirrors (`cdn.mailaender.name` in Germany,
//!   `openra.0x47.net` in Europe) alongside the global `openra.net`.
//! - **CNC-Inside** (`cnc-inside.de`) serves the German-speaking
//!   community with its own forum and tournament infrastructure.
//! - **CnCNet** runs relay/tunnel infrastructure in multiple regions
//!   (US/EU primarily) to minimise latency for multiplayer.
//! - **CNCNZ** (`files.cncnz.com`) hosts content internationally.
//!
//! Key lessons for P2P content distribution:
//!
//! - **Nearby peers provide better throughput.** Same-continent peers
//!   typically achieve 2-5x higher throughput than cross-ocean peers
//!   due to lower RTT and fewer congested hops.
//! - **Region affinity reduces backbone load.** Preferring local peers
//!   keeps traffic within ISP/IX boundaries, reducing transit costs for
//!   mirror operators and improving swarm health.
//! - **Hard geo-fencing hurts small swarms.** The C&C community is
//!   small enough that strict region-only matching would fragment the
//!   swarm. Affinity must be a preference, not a filter.
//! - **Latency is a better signal than IP geolocation.** A peer on the
//!   same ISP 500km away will outperform one in a nearby city on a
//!   different network. RTT measurement beats GeoIP databases.
//!
//! ## How
//!
//! - [`AffinityScorer`]: Combines region match, observed latency, and
//!   transfer speed into a composite affinity score in `[0.0, 1.0]`.
//! - [`PeerProfile`]: Per-peer affinity data (region hint, latency,
//!   speed history).
//! - [`AffinityConfig`]: Weights for each component of the score.
//!
//! The scorer produces an affinity multiplier that the piece selection
//! system can fold into its rarest-first + priority calculation. This
//! naturally biases toward nearby peers without breaking the piece
//! selection algorithm's correctness invariants.

use std::time::{Duration, Instant};

// ── Constants ───────────────────────────────────────────────────────

/// Default weight of region match in composite affinity score.
const DEFAULT_REGION_WEIGHT: f64 = 0.25;

/// Default weight of latency in composite affinity score.
const DEFAULT_LATENCY_WEIGHT: f64 = 0.40;

/// Default weight of observed throughput in composite affinity score.
const DEFAULT_SPEED_WEIGHT: f64 = 0.35;

/// Maximum RTT (ms) beyond which the latency component is zero.
///
/// 500ms round-trip is approximately the worst-case for cross-ocean
/// connections. Beyond this, the peer is effectively unusable for
/// latency-sensitive piece delivery.
const MAX_RTT_MS: f64 = 500.0;

/// Maximum speed samples retained per peer for averaging.
const MAX_SPEED_SAMPLES: usize = 20;

/// Default maximum number of peers tracked by the scorer.
const DEFAULT_MAX_PEERS: usize = 256;

// ── Configuration ───────────────────────────────────────────────────

/// Weights for the three components of the affinity score.
///
/// Weights should sum to 1.0 but will be normalised if they don't.
/// The default weighting emphasises latency (0.40) over throughput
/// (0.35) and region (0.25), reflecting the finding that measured RTT
/// is a better proximity signal than static region labels.
#[derive(Debug, Clone)]
pub struct AffinityConfig {
    /// Weight of region match (0.0–1.0).
    pub region_weight: f64,
    /// Weight of latency measurement (0.0–1.0).
    pub latency_weight: f64,
    /// Weight of observed transfer speed (0.0–1.0).
    pub speed_weight: f64,
    /// Maximum peers to track.
    pub max_peers: usize,
}

impl Default for AffinityConfig {
    fn default() -> Self {
        Self {
            region_weight: DEFAULT_REGION_WEIGHT,
            latency_weight: DEFAULT_LATENCY_WEIGHT,
            speed_weight: DEFAULT_SPEED_WEIGHT,
            max_peers: DEFAULT_MAX_PEERS,
        }
    }
}

impl AffinityConfig {
    /// Returns the weight sum for normalisation.
    fn weight_sum(&self) -> f64 {
        let sum = self.region_weight + self.latency_weight + self.speed_weight;
        if sum <= 0.0 {
            1.0
        } else {
            sum
        }
    }
}

// ── Peer profile ────────────────────────────────────────────────────

/// Observed affinity data for a single peer.
///
/// Accumulates measurements over time. The profile is updated by the
/// download coordinator as pieces are received from the peer.
#[derive(Debug, Clone)]
pub struct PeerProfile {
    /// Peer identifier (matches `PeerId` from `peer_id.rs`).
    peer_id: [u8; 20],
    /// Region hint if advertised by the peer (e.g. "eu-west").
    region: Option<String>,
    /// Most recent measured RTT.
    latency: Option<Duration>,
    /// Sliding window of observed piece transfer speeds (bytes/sec).
    speed_samples: Vec<f64>,
    /// When this profile was last updated.
    last_updated: Instant,
}

impl PeerProfile {
    /// Creates a new peer profile with no measurements.
    pub fn new(peer_id: [u8; 20], now: Instant) -> Self {
        Self {
            peer_id,
            region: None,
            latency: None,
            speed_samples: Vec::with_capacity(MAX_SPEED_SAMPLES),
            last_updated: now,
        }
    }

    /// Sets the region hint for this peer.
    pub fn set_region(&mut self, region: String) {
        self.region = Some(region);
    }

    /// Records a latency measurement (round-trip time).
    pub fn record_latency(&mut self, rtt: Duration, now: Instant) {
        self.latency = Some(rtt);
        self.last_updated = now;
    }

    /// Records an observed transfer speed in bytes per second.
    pub fn record_speed(&mut self, bytes_per_sec: f64, now: Instant) {
        if self.speed_samples.len() >= MAX_SPEED_SAMPLES {
            self.speed_samples.remove(0);
        }
        self.speed_samples.push(bytes_per_sec);
        self.last_updated = now;
    }

    /// Returns the peer's region hint.
    pub fn region(&self) -> Option<&str> {
        self.region.as_deref()
    }

    /// Returns the most recent latency measurement.
    pub fn latency(&self) -> Option<Duration> {
        self.latency
    }

    /// Returns the average observed transfer speed (bytes/sec).
    pub fn avg_speed(&self) -> Option<f64> {
        if self.speed_samples.is_empty() {
            return None;
        }
        let sum: f64 = self.speed_samples.iter().sum();
        Some(sum / self.speed_samples.len() as f64)
    }

    /// Returns the peer identifier.
    pub fn peer_id(&self) -> &[u8; 20] {
        &self.peer_id
    }

    /// Returns when this profile was last updated.
    pub fn last_updated(&self) -> Instant {
        self.last_updated
    }
}

// ── Affinity scorer ─────────────────────────────────────────────────

/// Computes composite affinity scores for peers based on region,
/// latency, and throughput measurements.
///
/// ```
/// use p2p_distribute::peer_affinity::{AffinityScorer, AffinityConfig};
/// use std::time::{Duration, Instant};
///
/// let mut scorer = AffinityScorer::new(
///     AffinityConfig::default(),
///     Some("eu-west".into()),
/// );
/// let now = Instant::now();
///
/// scorer.register_peer([1u8; 20], now);
/// scorer.set_peer_region(&[1u8; 20], "eu-west".into());
/// scorer.record_peer_latency(&[1u8; 20], Duration::from_millis(20), now);
/// scorer.record_peer_speed(&[1u8; 20], 500_000.0, now);
///
/// let score = scorer.score(&[1u8; 20], now);
/// assert!(score > 0.5, "EU peer should score well for EU local: {score}");
/// ```
pub struct AffinityScorer {
    /// Configuration weights.
    config: AffinityConfig,
    /// Our own region hint (for computing region-match component).
    local_region: Option<String>,
    /// Per-peer profiles.
    profiles: Vec<PeerProfile>,
    /// Maximum observed speed across all peers (for normalisation).
    max_observed_speed: f64,
}

impl AffinityScorer {
    /// Creates a new affinity scorer.
    ///
    /// `local_region` is our own region hint for computing region-match
    /// scores. If `None`, the region component is always 0.5 (neutral).
    pub fn new(config: AffinityConfig, local_region: Option<String>) -> Self {
        Self {
            config,
            local_region,
            profiles: Vec::with_capacity(64),
            max_observed_speed: 1.0, // Floor to avoid division by zero.
        }
    }

    /// Registers a new peer for tracking.
    pub fn register_peer(&mut self, peer_id: [u8; 20], now: Instant) {
        if self.profiles.iter().any(|p| p.peer_id == peer_id) {
            return; // Already tracked.
        }
        if self.profiles.len() >= self.config.max_peers {
            // Evict the oldest profile.
            if let Some(oldest_idx) = self
                .profiles
                .iter()
                .enumerate()
                .min_by_key(|(_, p)| p.last_updated)
                .map(|(i, _)| i)
            {
                self.profiles.swap_remove(oldest_idx);
            }
        }
        self.profiles.push(PeerProfile::new(peer_id, now));
    }

    /// Sets the region hint for a peer.
    pub fn set_peer_region(&mut self, peer_id: &[u8; 20], region: String) {
        if let Some(profile) = self.profiles.iter_mut().find(|p| &p.peer_id == peer_id) {
            profile.set_region(region);
        }
    }

    /// Records a latency measurement for a peer.
    pub fn record_peer_latency(&mut self, peer_id: &[u8; 20], rtt: Duration, now: Instant) {
        if let Some(profile) = self.profiles.iter_mut().find(|p| &p.peer_id == peer_id) {
            profile.record_latency(rtt, now);
        }
    }

    /// Records a transfer speed observation for a peer.
    pub fn record_peer_speed(&mut self, peer_id: &[u8; 20], bytes_per_sec: f64, now: Instant) {
        if let Some(profile) = self.profiles.iter_mut().find(|p| &p.peer_id == peer_id) {
            profile.record_speed(bytes_per_sec, now);
        }
        // Update global max for normalisation.
        if bytes_per_sec > self.max_observed_speed {
            self.max_observed_speed = bytes_per_sec;
        }
    }

    /// Computes the composite affinity score for a peer.
    ///
    /// Returns a value in `[0.0, 1.0]` where higher means better
    /// affinity. Returns 0.5 (neutral) for unknown peers.
    pub fn score(&self, peer_id: &[u8; 20], _now: Instant) -> f64 {
        let profile = match self.profiles.iter().find(|p| &p.peer_id == peer_id) {
            Some(p) => p,
            None => return 0.5, // Unknown peer → neutral score.
        };

        let weight_sum = self.config.weight_sum();

        // Region component: 1.0 if same region, 0.5 if unknown, 0.0 if
        // different.
        let region_score = match (&self.local_region, profile.region()) {
            (Some(local), Some(remote)) => {
                if local == remote {
                    1.0
                } else {
                    0.0
                }
            }
            _ => 0.5, // Unknown → neutral.
        };

        // Latency component: normalised inverse of RTT.
        let latency_score = match profile.latency() {
            Some(rtt) => {
                let ms = rtt.as_millis() as f64;
                1.0 - (ms / MAX_RTT_MS).min(1.0)
            }
            None => 0.5, // No measurement → neutral.
        };

        // Speed component: normalised against best observed peer.
        let speed_score = match profile.avg_speed() {
            Some(speed) => (speed / self.max_observed_speed).min(1.0),
            None => 0.5, // No measurement → neutral.
        };

        let raw = (region_score * self.config.region_weight
            + latency_score * self.config.latency_weight
            + speed_score * self.config.speed_weight)
            / weight_sum;

        raw.clamp(0.0, 1.0)
    }

    /// Returns all peers ranked by affinity score (best first).
    pub fn ranked_peers(&self, now: Instant) -> Vec<([u8; 20], f64)> {
        let mut ranked: Vec<([u8; 20], f64)> = self
            .profiles
            .iter()
            .map(|p| (p.peer_id, self.score(&p.peer_id, now)))
            .collect();

        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        ranked
    }

    /// Removes a peer from tracking.
    pub fn remove_peer(&mut self, peer_id: &[u8; 20]) {
        self.profiles.retain(|p| &p.peer_id != peer_id);
    }

    /// Returns the number of peers being tracked.
    pub fn peer_count(&self) -> usize {
        self.profiles.len()
    }

    /// Returns our local region hint.
    pub fn local_region(&self) -> Option<&str> {
        self.local_region.as_deref()
    }

    /// Returns the profile for a specific peer.
    pub fn get_profile(&self, peer_id: &[u8; 20]) -> Option<&PeerProfile> {
        self.profiles.iter().find(|p| &p.peer_id == peer_id)
    }

    /// Prunes peers not updated within the given duration.
    pub fn prune_stale(&mut self, max_age: Duration, now: Instant) -> usize {
        let before = self.profiles.len();
        self.profiles
            .retain(|p| now.duration_since(p.last_updated) < max_age);
        before.saturating_sub(self.profiles.len())
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn default_scorer(local_region: Option<&str>) -> AffinityScorer {
        AffinityScorer::new(AffinityConfig::default(), local_region.map(String::from))
    }

    // ── PeerProfile ─────────────────────────────────────────────────

    /// New profile has no measurements.
    ///
    /// Fresh profiles should return None for optional fields.
    #[test]
    fn new_profile_empty() {
        let now = Instant::now();
        let profile = PeerProfile::new([1u8; 20], now);
        assert!(profile.latency().is_none());
        assert!(profile.avg_speed().is_none());
        assert!(profile.region().is_none());
    }

    /// Average speed is computed from samples.
    ///
    /// The sliding window should produce a correct arithmetic mean.
    #[test]
    fn avg_speed_computed() {
        let now = Instant::now();
        let mut profile = PeerProfile::new([1u8; 20], now);

        profile.record_speed(100.0, now);
        profile.record_speed(200.0, now);
        profile.record_speed(300.0, now);

        let avg = profile.avg_speed().unwrap();
        assert!((avg - 200.0).abs() < 0.01, "avg = {avg}");
    }

    /// Speed window is bounded.
    ///
    /// Prevents unbounded memory growth from long-running sessions.
    #[test]
    fn speed_window_bounded() {
        let now = Instant::now();
        let mut profile = PeerProfile::new([1u8; 20], now);

        for i in 0..50 {
            profile.record_speed(i as f64 * 100.0, now);
        }

        assert!(
            profile.speed_samples.len() <= MAX_SPEED_SAMPLES,
            "samples grew to {}",
            profile.speed_samples.len()
        );
    }

    // ── AffinityScorer ──────────────────────────────────────────────

    /// Same-region peer scores higher than different-region.
    ///
    /// This is the core geographic affinity preference: peers sharing
    /// our region get a region-match bonus.
    #[test]
    fn same_region_scores_higher() {
        let mut scorer = default_scorer(Some("eu-west"));
        let now = Instant::now();

        scorer.register_peer([1u8; 20], now);
        scorer.set_peer_region(&[1u8; 20], "eu-west".into());

        scorer.register_peer([2u8; 20], now);
        scorer.set_peer_region(&[2u8; 20], "us-east".into());

        let eu_score = scorer.score(&[1u8; 20], now);
        let us_score = scorer.score(&[2u8; 20], now);

        assert!(
            eu_score > us_score,
            "EU peer score ({eu_score}) should exceed US ({us_score})"
        );
    }

    /// Low latency peer scores higher than high latency.
    ///
    /// Measured RTT is the strongest proximity signal.
    #[test]
    fn low_latency_scores_higher() {
        let mut scorer = default_scorer(None);
        let now = Instant::now();

        scorer.register_peer([1u8; 20], now);
        scorer.record_peer_latency(&[1u8; 20], Duration::from_millis(20), now);

        scorer.register_peer([2u8; 20], now);
        scorer.record_peer_latency(&[2u8; 20], Duration::from_millis(400), now);

        let fast = scorer.score(&[1u8; 20], now);
        let slow = scorer.score(&[2u8; 20], now);

        assert!(
            fast > slow,
            "fast latency ({fast}) should beat slow ({slow})"
        );
    }

    /// Higher throughput peer scores higher.
    ///
    /// Observed transfer speed directly impacts download time.
    #[test]
    fn high_speed_scores_higher() {
        let mut scorer = default_scorer(None);
        let now = Instant::now();

        scorer.register_peer([1u8; 20], now);
        scorer.record_peer_speed(&[1u8; 20], 1_000_000.0, now);

        scorer.register_peer([2u8; 20], now);
        scorer.record_peer_speed(&[2u8; 20], 100_000.0, now);

        let fast = scorer.score(&[1u8; 20], now);
        let slow = scorer.score(&[2u8; 20], now);

        assert!(fast > slow, "fast peer ({fast}) should beat slow ({slow})");
    }

    /// Unknown peer returns neutral score.
    ///
    /// Peers we've never seen should get a middle-ground score (0.5)
    /// so they're not starved or overprivileged.
    #[test]
    fn unknown_peer_neutral() {
        let scorer = default_scorer(None);
        let now = Instant::now();

        let score = scorer.score(&[99u8; 20], now);
        assert!(
            (score - 0.5).abs() < 0.01,
            "unknown peer should be neutral: {score}"
        );
    }

    /// Ranked peers returned in descending score order.
    ///
    /// The coordinator uses this to bias piece requests toward the
    /// highest-affinity peers.
    #[test]
    fn ranked_peers_descending() {
        let mut scorer = default_scorer(Some("eu-west"));
        let now = Instant::now();

        scorer.register_peer([1u8; 20], now);
        scorer.set_peer_region(&[1u8; 20], "eu-west".into());
        scorer.record_peer_latency(&[1u8; 20], Duration::from_millis(10), now);

        scorer.register_peer([2u8; 20], now);
        scorer.set_peer_region(&[2u8; 20], "us-east".into());
        scorer.record_peer_latency(&[2u8; 20], Duration::from_millis(200), now);

        let ranked = scorer.ranked_peers(now);
        assert_eq!(ranked.len(), 2);
        assert!(ranked[0].1 >= ranked[1].1, "should be descending");
        assert_eq!(ranked[0].0, [1u8; 20], "EU peer should be first");
    }

    /// Peer eviction when capacity exceeded.
    ///
    /// When the scorer is full, the oldest unupdated peer should be
    /// evicted to make room.
    #[test]
    fn evicts_oldest_when_full() {
        let config = AffinityConfig {
            max_peers: 3,
            ..AffinityConfig::default()
        };
        let mut scorer = AffinityScorer::new(config, None);
        let now = Instant::now();

        scorer.register_peer([1u8; 20], now);
        scorer.register_peer([2u8; 20], now);
        scorer.register_peer([3u8; 20], now);
        assert_eq!(scorer.peer_count(), 3);

        // Adding a 4th should evict one.
        scorer.register_peer([4u8; 20], now);
        assert_eq!(scorer.peer_count(), 3);
    }

    /// Remove peer clears its profile.
    ///
    /// Explicit removal should free the tracking slot.
    #[test]
    fn remove_peer_clears_profile() {
        let mut scorer = default_scorer(None);
        let now = Instant::now();

        scorer.register_peer([1u8; 20], now);
        assert_eq!(scorer.peer_count(), 1);

        scorer.remove_peer(&[1u8; 20]);
        assert_eq!(scorer.peer_count(), 0);
    }

    /// Prune stale removes old profiles.
    ///
    /// Long-disconnected peers should be cleaned up periodically.
    #[test]
    fn prune_stale_peers() {
        let mut scorer = default_scorer(None);
        let now = Instant::now();

        scorer.register_peer([1u8; 20], now);

        // 0 seconds of staleness → nothing pruned.
        let pruned = scorer.prune_stale(Duration::from_secs(60), now);
        assert_eq!(pruned, 0);

        // After staleness period.
        let later = now + Duration::from_secs(61);
        let pruned = scorer.prune_stale(Duration::from_secs(60), later);
        assert_eq!(pruned, 1);
    }

    /// Duplicate registration is idempotent.
    ///
    /// Re-registering an existing peer should not create a second
    /// profile.
    #[test]
    fn duplicate_registration_noop() {
        let mut scorer = default_scorer(None);
        let now = Instant::now();

        scorer.register_peer([1u8; 20], now);
        scorer.register_peer([1u8; 20], now);
        assert_eq!(scorer.peer_count(), 1);
    }

    /// Latency near maximum yields near-zero latency component.
    ///
    /// Peers with RTT approaching 500ms are barely better than no
    /// measurement.
    #[test]
    fn high_latency_low_score() {
        let mut scorer = default_scorer(None);
        let now = Instant::now();

        scorer.register_peer([1u8; 20], now);
        scorer.record_peer_latency(&[1u8; 20], Duration::from_millis(490), now);

        let score = scorer.score(&[1u8; 20], now);
        // Should be near neutral (0.5) since latency is near max and
        // other components are unknown → 0.5.
        assert!(score < 0.55, "high latency score too high: {score}");
    }
}
