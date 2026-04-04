// SPDX-License-Identifier: MIT OR Apache-2.0

//! Per-peer session statistics and composite scoring for peer selection.
//!
//! [`PeerTracker`] maintains [`PeerStats`] for every peer in a download
//! session. The coordinator uses [`PeerStats::composite_score`] during
//! peer selection to prefer reliable, fast, recently-active peers over
//! flaky or slow ones.
//!
//! ## Scoring model
//!
//! The composite score is a weighted combination of four factors:
//!
//! | Factor        | Weight | Description                                      |
//! |---------------|--------|--------------------------------------------------|
//! | Speed         |  0.4   | Rolling average download speed from this peer    |
//! | Reliability   |  0.3   | Success rate (pieces served / pieces requested)  |
//! | Availability  |  0.2   | Inverse of disconnect frequency                  |
//! | Recency       |  0.1   | Inverse of time since last successful interaction|
//!
//! This maps to D049's peer scoring formula (Capacity 0.4, Locality 0.3,
//! SeedStatus 0.2, LobbyContext 0.1) — substituting locality and lobby
//! context (which require the wire protocol, M1–M3) with reliability and
//! recency (measurable now).
//!
//! ## IRC protocol lessons
//!
//! - **Structured rejection** (IRC numeric errors) → [`RejectionReason`]
//!   tells the coordinator *why* a peer refused, enabling smart backoff.
//! - **RPL_ISUPPORT limits** → [`PeerCapabilities`] lets peers declare
//!   limits upfront so the coordinator doesn't learn them by trial and error.
//! - **Netsplit recovery** → disconnect tracking and recency scoring handle
//!   peers that drop and reconnect, discounting flaky peers automatically.

use std::time::{Duration, Instant};

use crate::peer::RejectionReason;
use crate::peer_id::PeerId;

// ── Score weights (×1000 to avoid floating-point in hot path) ───────
//
// D049: Capacity(0.4) + Locality(0.3) + SeedStatus(0.2) + LobbyContext(0.1).
// Without wire protocol: Speed(0.4) + Reliability(0.3) + Availability(0.2) +
// Recency(0.1).

const WEIGHT_SPEED: u64 = 400;
const WEIGHT_RELIABILITY: u64 = 300;
const WEIGHT_AVAILABILITY: u64 = 200;
const WEIGHT_RECENCY: u64 = 100;

/// Maximum "recency window" — peers not seen in this duration get a recency
/// score of zero. 5 minutes matches IRC's typical PING timeout interval.
const RECENCY_WINDOW_SECS: u64 = 300;

/// Disconnect count at which availability score drops to zero.
/// More than 10 disconnect/reconnect cycles in a session indicates a
/// fundamentally unstable peer.
const MAX_DISCONNECTS: f64 = 10.0;

/// Seconds a peer must exist before losing the "new peer" recency bonus.
/// Brand-new peers get benefit of the doubt for 10 seconds.
const NEW_PEER_GRACE_SECS: u64 = 10;

/// Base backoff duration for transient rejections. Doubles with each
/// consecutive rejection per TCP exponential backoff (RFC 6298).
const BASE_BACKOFF_SECS: u64 = 5;

/// Maximum backoff cap — stops growing after this (5→10→20→40→60 seconds).
const MAX_BACKOFF_SECS: u64 = 60;

/// EWMA smoothing factor for speed tracking, matching `WebSeedPeer` (α=0.3).
/// Higher α gives more weight to recent speed observations.
const EWMA_ALPHA: f64 = 0.3;

/// Pieces a peer must serve before advancing to [`TrustLevel::Established`].
const ESTABLISHED_THRESHOLD: u32 = 3;

/// Pieces a peer must serve before reaching [`TrustLevel::Trusted`].
const TRUSTED_THRESHOLD: u32 = 10;

/// Anti-snubbing threshold in seconds. If no data received from a peer
/// for this duration while a download is active, the peer is considered
/// "snubbed" — it has effectively stopped cooperating.
///
/// ## BitTorrent specification
///
/// BT clients mark a peer as snubbed after 60 seconds without receiving
/// any piece data. Snubbed peers are deprioritised (not uploaded to except
/// as optimistic unchoke) and the client may open additional connections
/// to compensate. For our download-only use case, snubbed peers are
/// skipped during piece assignment.
const SNUB_TIMEOUT_SECS: u64 = 60;

// ── TrustLevel ──────────────────────────────────────────────────────

/// Trust level for a peer based on observed successful deliveries.
///
/// ## TCP slow-start analogy
///
/// TCP doesn't send at full rate immediately — it probes the path with a
/// small congestion window and scales up as ACKs confirm capacity.
/// Similarly, untested peers receive a scoring discount until they prove
/// reliable through successful piece deliveries.
///
/// ## BitTorrent optimistic unchoking analogy
///
/// BT clients periodically unchoke a random peer to discover new fast
/// sources. Trust levels achieve a similar effect: untested peers get a
/// chance (non-zero score) but don't displace proven peers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TrustLevel {
    /// No successful pieces yet. Composite score multiplier: 0.5.
    Untested,
    /// 1–2 successful pieces. Composite score multiplier: 0.7.
    Probationary,
    /// 3–9 successful pieces. Composite score multiplier: 0.9.
    Established,
    /// 10+ successful pieces. Composite score multiplier: 1.0 (full trust).
    Trusted,
}

impl TrustLevel {
    /// Returns the composite score multiplier for this trust level.
    ///
    /// Applied as a top-level multiplier to the raw composite score, reducing
    /// unproven peers' influence in peer selection relative to proven ones.
    pub fn multiplier(self) -> f64 {
        match self {
            Self::Untested => 0.5,
            Self::Probationary => 0.7,
            Self::Established => 0.9,
            Self::Trusted => 1.0,
        }
    }
}

// ── PeerReputation ──────────────────────────────────────────────────

/// Cross-session peer reputation snapshot for persistent trust.
///
/// When a session ends, the coordinator snapshots each identified peer's
/// [`PeerStats`] into a `PeerReputation` via [`PeerStats::to_reputation`].
/// On the next session, these snapshots are fed to
/// [`PeerTracker::with_prior_reputation`] so returning peers skip the
/// [`TrustLevel::Untested`] slow-start and begin at their prior trust level.
///
/// ## Serialisation
///
/// `PeerReputation` uses only plain types (`u64`, `u32`, `bool`, `[u8; 32]`)
/// so that consumers can serialise it with any framework (serde, bincode,
/// manual binary format) without this crate depending on serde.
///
/// ## Expiry
///
/// Consumers should discard reputations older than a reasonable window
/// (e.g. 7–30 days). Stale reputations from an IP address that changed
/// hands could grant undeserved trust. The `last_seen_unix_secs` field
/// supports this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerReputation {
    /// The peer's cryptographic identity.
    pub peer_id: PeerId,
    /// Trust level at the end of the previous session.
    pub trust_level: TrustLevel,
    /// Lifetime pieces successfully served across all sessions.
    pub lifetime_pieces_served: u64,
    /// Lifetime SHA-1 corruption events across all sessions.
    pub lifetime_corruption_count: u32,
    /// Average speed in bytes/sec from the last EWMA estimate.
    pub avg_speed_bytes_per_sec: u64,
    /// Last time this peer was successfully contacted (Unix epoch seconds).
    pub last_seen_unix_secs: u64,
    /// Whether this peer is permanently banned (corruption, policy violation).
    pub banned: bool,
}

// ── PeerStats ───────────────────────────────────────────────────────

/// Per-peer session statistics collected during download coordination.
///
/// Tracks success/failure rates, speed history, and connection patterns.
/// The coordinator queries [`composite_score()`](Self::composite_score) during
/// peer selection to rank peers beyond simple instantaneous speed.
///
/// ## Why
///
/// Instantaneous `speed_estimate()` from the [`Peer`](crate::peer::Peer) trait
/// can be noisy (single slow request, network jitter). Historical statistics
/// provide a more stable signal. IRC's server linking and BT's tit-for-tat
/// both evaluate peers on sustained behavior, not snapshots.
#[derive(Debug, Clone)]
pub struct PeerStats {
    /// Total pieces successfully served by this peer.
    pub pieces_served: u32,
    /// Total pieces requested (success + failure).
    pub pieces_requested: u32,
    /// Total bytes successfully received from this peer.
    pub bytes_received: u64,
    /// Total SHA-1 corruption events (subset of failures).
    pub corruption_count: u32,
    /// Total timeout events (subset of failures).
    pub timeout_count: u32,
    /// Total rejection events (peer actively refused).
    pub rejection_count: u32,
    /// Last rejection reason (if any), for coordinator decision-making.
    pub last_rejection: Option<RejectionReason>,
    /// Cumulative download duration for speed averaging (sum of individual
    /// piece fetch durations, not wall-clock time).
    pub cumulative_fetch_duration: Duration,
    /// Peak observed speed in bytes/sec for any single piece.
    pub peak_speed: u64,
    /// Timestamp of the last successful piece fetch.
    pub last_success_at: Option<Instant>,
    /// Timestamp of the last failure (any kind).
    pub last_failure_at: Option<Instant>,
    /// Number of times this peer transitioned from "working" to "failing".
    /// Detected when a success follows a failure from *after* the last success
    /// — indicating the peer dropped and reconnected.
    pub disconnect_count: u32,
    /// When the peer was first seen in this session.
    pub first_seen: Instant,
    /// Consecutive transient rejection count for exponential backoff (TCP
    /// RFC 6298). Reset to 0 on any successful fetch.
    pub consecutive_transient_rejections: u32,
    /// Exponential weighted moving average speed in bytes/sec.
    /// More responsive than cumulative average. α=0.3 matching `WebSeedPeer`.
    pub ewma_speed: f64,
}

impl PeerStats {
    /// Creates fresh stats for a peer, timestamped to `now`.
    pub fn new(now: Instant) -> Self {
        Self {
            pieces_served: 0,
            pieces_requested: 0,
            bytes_received: 0,
            corruption_count: 0,
            timeout_count: 0,
            rejection_count: 0,
            last_rejection: None,
            cumulative_fetch_duration: Duration::ZERO,
            peak_speed: 0,
            last_success_at: None,
            last_failure_at: None,
            disconnect_count: 0,
            first_seen: now,
            consecutive_transient_rejections: 0,
            ewma_speed: 0.0,
        }
    }

    /// Records a successful piece fetch.
    ///
    /// Updates served count, byte total, speed tracking, and detects
    /// reconnect patterns (a success after a recent failure).
    pub fn record_success(&mut self, bytes: u64, fetch_duration: Duration, now: Instant) {
        self.pieces_served = self.pieces_served.saturating_add(1);
        self.pieces_requested = self.pieces_requested.saturating_add(1);
        self.bytes_received = self.bytes_received.saturating_add(bytes);
        self.cumulative_fetch_duration = self
            .cumulative_fetch_duration
            .saturating_add(fetch_duration);

        // Peer is responsive — reset consecutive transient rejection counter.
        // Matches TCP behaviour where successful ACKs reset the retransmit timer.
        self.consecutive_transient_rejections = 0;

        // Compute instantaneous speed for this piece.
        let secs = fetch_duration.as_secs_f64();
        if secs > 0.0 {
            let speed = bytes as f64 / secs;

            // Track peak speed.
            let speed_u64 = speed as u64;
            if speed_u64 > self.peak_speed {
                self.peak_speed = speed_u64;
            }

            // Update EWMA speed. First observation initialises the estimate;
            // subsequent observations blend with α=0.3 (matching WebSeedPeer).
            if self.ewma_speed == 0.0 {
                self.ewma_speed = speed;
            } else {
                self.ewma_speed = EWMA_ALPHA * speed + (1.0 - EWMA_ALPHA) * self.ewma_speed;
            }
        }

        // Detect "reconnect" — a success after a failure window indicates the
        // peer recovered from a connectivity issue.
        let had_recent_failure = self.last_failure_at.is_some()
            && self
                .last_success_at
                .is_none_or(|s| self.last_failure_at.is_some_and(|f| f >= s));
        if had_recent_failure {
            self.disconnect_count = self.disconnect_count.saturating_add(1);
        }

        self.last_success_at = Some(now);
    }

    /// Records a piece fetch failure (timeout, network error, etc.).
    pub fn record_failure(&mut self, now: Instant) {
        self.pieces_requested = self.pieces_requested.saturating_add(1);
        self.last_failure_at = Some(now);
    }

    /// Records a SHA-1 corruption event (a specific type of failure).
    pub fn record_corruption(&mut self, now: Instant) {
        self.corruption_count = self.corruption_count.saturating_add(1);
        self.record_failure(now);
    }

    /// Records a timeout event (a specific type of failure).
    pub fn record_timeout(&mut self, now: Instant) {
        self.timeout_count = self.timeout_count.saturating_add(1);
        self.record_failure(now);
    }

    /// Records a structured rejection from the peer.
    pub fn record_rejection(&mut self, reason: RejectionReason, now: Instant) {
        self.rejection_count = self.rejection_count.saturating_add(1);

        // Track consecutive transient rejections for exponential backoff.
        // Non-transient rejections don't increment — they're handled by
        // should_avoid_permanently() instead.
        if matches!(
            reason,
            RejectionReason::RateLimited | RejectionReason::SwarmFull
        ) {
            self.consecutive_transient_rejections =
                self.consecutive_transient_rejections.saturating_add(1);
        }

        self.last_rejection = Some(reason);
        self.record_failure(now);
    }

    /// Rolling average speed in bytes/sec over all successful fetches.
    ///
    /// Returns 0 if no successful fetches yet.
    pub fn average_speed(&self) -> u64 {
        let secs = self.cumulative_fetch_duration.as_secs_f64();
        if secs > 0.0 {
            (self.bytes_received as f64 / secs) as u64
        } else {
            0
        }
    }

    /// Success rate as a fraction (0.0–1.0).
    ///
    /// Returns 1.0 if no requests yet (benefit of the doubt for new peers).
    pub fn success_rate(&self) -> f64 {
        if self.pieces_requested == 0 {
            return 1.0;
        }
        self.pieces_served as f64 / self.pieces_requested as f64
    }

    /// Corruption rate as a fraction (0.0–1.0).
    ///
    /// Returns 0.0 if no requests yet.
    pub fn corruption_rate(&self) -> f64 {
        if self.pieces_requested == 0 {
            return 0.0;
        }
        self.corruption_count as f64 / self.pieces_requested as f64
    }

    /// Current trust level based on observed successful deliveries.
    ///
    /// TCP slow-start: new connections ramp up gradually. BT optimistic
    /// unchoking: untested peers get occasional chances. Here, trust grows
    /// with `pieces_served`: Untested → Probationary → Established → Trusted,
    /// each increasing the composite score multiplier.
    pub fn trust_level(&self) -> TrustLevel {
        if self.pieces_served >= TRUSTED_THRESHOLD {
            TrustLevel::Trusted
        } else if self.pieces_served >= ESTABLISHED_THRESHOLD {
            TrustLevel::Established
        } else if self.pieces_served > 0 {
            TrustLevel::Probationary
        } else {
            TrustLevel::Untested
        }
    }

    /// EWMA speed in bytes/sec (exponential weighted moving average).
    ///
    /// More responsive to speed changes than [`average_speed()`](Self::average_speed),
    /// which uses cumulative averaging. Uses α=0.3 matching `WebSeedPeer`.
    /// Returns 0 if no successful fetches yet.
    pub fn ewma_speed_bytes_per_sec(&self) -> u64 {
        self.ewma_speed as u64
    }

    /// Computes a composite score (0–1000) for peer selection ranking.
    ///
    /// Higher is better. Combines four weighted factors per the D049 peer
    /// scoring model (see module docs).
    ///
    /// ## Parameters
    ///
    /// - `reference_speed`: speed of the fastest peer in the session, used to
    ///   normalise this peer's speed to [0, 1]. Pass 0 if unknown.
    /// - `now`: current time for recency calculation.
    pub fn composite_score(&self, reference_speed: u64, now: Instant) -> u64 {
        // Speed factor: ratio of this peer's EWMA speed to the reference.
        // EWMA reacts faster to speed changes than cumulative average,
        // matching WebSeedPeer's speed tracking and TCP's RTT estimation.
        let current_speed = self.ewma_speed as u64;
        let speed_ratio = if reference_speed > 0 {
            (current_speed as f64 / reference_speed as f64).min(1.0)
        } else {
            // No reference — give benefit of the doubt.
            0.5
        };

        // Reliability factor: success rate.
        let reliability = self.success_rate();

        // Availability factor: penalise flaky peers.
        // 0 disconnects = 1.0, ≥ MAX_DISCONNECTS = 0.0.
        let availability = (1.0 - (self.disconnect_count as f64 / MAX_DISCONNECTS)).max(0.0);

        // Recency factor: how recently was this peer useful?
        let recency = match self.last_success_at {
            Some(t) => {
                let age_secs = now.duration_since(t).as_secs();
                if age_secs >= RECENCY_WINDOW_SECS {
                    0.0
                } else {
                    1.0 - (age_secs as f64 / RECENCY_WINDOW_SECS as f64)
                }
            }
            // Never succeeded — partial credit if brand-new.
            None => {
                let age = now.duration_since(self.first_seen).as_secs();
                if age < NEW_PEER_GRACE_SECS {
                    0.5
                } else {
                    0.0
                }
            }
        };

        // Weighted sum, already in 0–1000 range.
        let raw_score = speed_ratio * WEIGHT_SPEED as f64
            + reliability * WEIGHT_RELIABILITY as f64
            + availability * WEIGHT_AVAILABILITY as f64
            + recency * WEIGHT_RECENCY as f64;

        // Apply trust multiplier (TCP slow-start analogy).
        // Untested peers get 50% of their raw score, fully trusted get 100%.
        let trust = self.trust_level().multiplier();
        (raw_score * trust) as u64
    }

    /// Whether the coordinator should temporarily back off from this peer.
    ///
    /// Returns `true` if the peer's last rejection was transient (rate limited
    /// or swarm full) and the rejection was recent enough that the exponential
    /// backoff window hasn't expired.
    ///
    /// ## TCP exponential backoff (RFC 6298)
    ///
    /// Each consecutive transient rejection doubles the backoff:
    /// 5s → 10s → 20s → 40s → 60s (capped). A successful fetch resets
    /// the counter to zero.
    pub fn should_back_off(&self, now: Instant) -> bool {
        let is_transient = matches!(
            self.last_rejection,
            Some(RejectionReason::RateLimited | RejectionReason::SwarmFull)
        );
        if !is_transient || self.consecutive_transient_rejections == 0 {
            return false;
        }
        // Exponential backoff: base × 2^(n−1), capped at MAX_BACKOFF_SECS.
        let exponent = self
            .consecutive_transient_rejections
            .saturating_sub(1)
            .min(6);
        let backoff_secs = BASE_BACKOFF_SECS
            .saturating_mul(1u64 << exponent)
            .min(MAX_BACKOFF_SECS);
        self.last_failure_at
            .is_some_and(|t| now.duration_since(t).as_secs() < backoff_secs)
    }

    /// Whether the coordinator should permanently avoid this peer.
    ///
    /// Returns `true` if the last rejection was non-transient (policy
    /// violation, maintenance, or insufficient auth). Retrying won't help.
    pub fn should_avoid_permanently(&self) -> bool {
        matches!(
            self.last_rejection,
            Some(
                RejectionReason::PolicyViolation
                    | RejectionReason::Maintenance
                    | RejectionReason::InsufficientAuth
            )
        )
    }

    /// Whether this peer is "snubbed" — not delivering data despite being
    /// connected.
    ///
    /// ## BitTorrent anti-snubbing
    ///
    /// BT clients mark a peer as snubbed after [`SNUB_TIMEOUT_SECS`] (60s)
    /// without receiving any piece data. For download-only clients, snubbed
    /// peers should be deprioritised during piece assignment so the
    /// coordinator can compensate by favouring responsive peers.
    ///
    /// The check uses `last_success_at` as the "last data received" marker.
    /// If no success has ever been recorded, the grace period starts from
    /// `first_seen` — a peer that never delivers within the first 60s is
    /// snubbed.
    pub fn is_snubbed(&self, now: Instant) -> bool {
        let reference = self.last_success_at.unwrap_or(self.first_seen);
        now.duration_since(reference).as_secs() >= SNUB_TIMEOUT_SECS
    }

    /// Whether this peer has had any meaningful interaction.
    ///
    /// ## WireGuard lazy-state pattern
    ///
    /// WireGuard refuses to allocate cryptographic state for unauthenticated
    /// peers, preventing resource exhaustion from spoofed packets. Similarly,
    /// peers that exist in the tracker but have never been assigned a piece
    /// or sent a rejection are "shadow peers" — they should not influence
    /// scoring or selection decisions.
    ///
    /// Returns `true` if the peer has requested at least one piece or has
    /// been rejected at least once (proving it exists and was contacted).
    pub fn has_interacted(&self) -> bool {
        self.pieces_requested > 0 || self.rejection_count > 0
    }

    /// Creates a cross-session reputation snapshot from this peer's stats.
    ///
    /// Call this at session end for each identified peer. The snapshot
    /// carries enough information for the next session to skip trust
    /// slow-start and detect banned peers.
    ///
    /// ## Parameters
    ///
    /// - `peer_id`: the peer's cryptographic identity (from `PeerTracker`).
    /// - `now_unix_secs`: current time as Unix epoch seconds (not `Instant`,
    ///   which is session-local). Consumers obtain this from
    ///   `SystemTime::now().duration_since(UNIX_EPOCH)`.
    /// - `prior`: optional previous reputation to merge lifetime counters
    ///   from. When `Some`, lifetime fields accumulate across sessions.
    pub fn to_reputation(
        &self,
        peer_id: PeerId,
        now_unix_secs: u64,
        prior: Option<&PeerReputation>,
    ) -> PeerReputation {
        let (prior_pieces, prior_corruption) = prior
            .map(|p| (p.lifetime_pieces_served, p.lifetime_corruption_count))
            .unwrap_or((0, 0));

        PeerReputation {
            peer_id,
            trust_level: self.trust_level(),
            lifetime_pieces_served: prior_pieces.saturating_add(u64::from(self.pieces_served)),
            lifetime_corruption_count: prior_corruption.saturating_add(self.corruption_count),
            avg_speed_bytes_per_sec: self.ewma_speed_bytes_per_sec(),
            last_seen_unix_secs: now_unix_secs,
            banned: self.should_avoid_permanently(),
        }
    }
}

// ── PeerTracker ─────────────────────────────────────────────────────

/// Session-level peer statistics manager with optional identity tracking.
///
/// Maintains [`PeerStats`] for every peer registered with the coordinator.
/// The coordinator records events (success, failure, corruption, rejection)
/// and queries composite scores during peer selection.
///
/// ## Identity support
///
/// When a peer reports its identity (via [`Peer::peer_id()`](crate::Peer::peer_id)),
/// the coordinator calls [`register_identity`](Self::register_identity) to bind
/// a [`PeerId`] to the peer's index. This enables:
///
/// - Cross-session reputation persistence via [`PeerReputation`].
/// - Reconnection detection (matching a new peer index to a known identity).
/// - Persistent ban enforcement.
///
/// Peers without identity (e.g. BT swarm peers without `ic_auth`) are tracked
/// by index only, with no cross-session persistence.
pub struct PeerTracker {
    /// Per-peer statistics, indexed by peer index.
    stats: Vec<PeerStats>,
    /// Optional identity for each peer. `None` = unidentified peer.
    identities: Vec<Option<PeerId>>,
    /// Prior session reputations, used by [`register_identity`](Self::register_identity)
    /// to restore trust levels for returning peers.
    prior_reputations: Vec<PeerReputation>,
    /// Session start time.
    session_start: Instant,
}

impl PeerTracker {
    /// Creates a new tracker for the given number of peers.
    pub fn new(peer_count: usize, now: Instant) -> Self {
        let stats = (0..peer_count).map(|_| PeerStats::new(now)).collect();
        let identities = vec![None; peer_count];
        Self {
            stats,
            identities,
            prior_reputations: Vec::new(),
            session_start: now,
        }
    }

    /// Creates a tracker pre-seeded with prior session reputations.
    ///
    /// When peers later identify themselves via
    /// [`register_identity`](Self::register_identity), those matching an entry
    /// in `prior_reputations` skip the [`TrustLevel::Untested`] slow-start —
    /// their session stats are initialised to reflect the prior trust level.
    ///
    /// Banned peers in `prior_reputations` are pre-marked for permanent
    /// avoidance when they register their identity.
    pub fn with_prior_reputation(
        peer_count: usize,
        now: Instant,
        prior_reputations: Vec<PeerReputation>,
    ) -> Self {
        let stats = (0..peer_count).map(|_| PeerStats::new(now)).collect();
        let identities = vec![None; peer_count];
        Self {
            stats,
            identities,
            prior_reputations,
            session_start: now,
        }
    }

    /// Returns the stats for a specific peer, if in bounds.
    pub fn get(&self, peer_index: usize) -> Option<&PeerStats> {
        self.stats.get(peer_index)
    }

    /// Returns mutable stats for a specific peer, if in bounds.
    pub fn get_mut(&mut self, peer_index: usize) -> Option<&mut PeerStats> {
        self.stats.get_mut(peer_index)
    }

    /// Binds a cryptographic identity to a peer index.
    ///
    /// Called by the coordinator after the peer's handshake reveals its
    /// identity string, which is hashed into a [`PeerId`] via
    /// [`PeerId::from_key_material`].
    ///
    /// If prior reputations were provided at construction time (via
    /// [`with_prior_reputation`](Self::with_prior_reputation)), and the peer's
    /// identity matches a stored reputation, the peer's stats are adjusted:
    ///
    /// - **Returning trusted peer**: `pieces_served` is set to the threshold
    ///   for the prior trust level, skipping slow-start.
    /// - **Banned peer**: `last_rejection` is set to `PolicyViolation`,
    ///   triggering permanent avoidance.
    pub fn register_identity(&mut self, peer_index: usize, id: PeerId) {
        if let Some(slot) = self.identities.get_mut(peer_index) {
            *slot = Some(id);
        }

        // Look up prior reputation and apply trust carry-over.
        let prior = self.prior_reputations.iter().find(|r| r.peer_id == id);
        if let Some(rep) = prior {
            if let Some(stats) = self.stats.get_mut(peer_index) {
                if rep.banned {
                    // Banned peer — force permanent avoidance.
                    stats.last_rejection = Some(RejectionReason::PolicyViolation);
                } else {
                    // Restore prior trust level by setting pieces_served to
                    // the minimum threshold for that level. This skips
                    // slow-start without inflating lifetime counters.
                    let threshold = match rep.trust_level {
                        TrustLevel::Untested => 0,
                        TrustLevel::Probationary => 1,
                        TrustLevel::Established => ESTABLISHED_THRESHOLD,
                        TrustLevel::Trusted => TRUSTED_THRESHOLD,
                    };
                    stats.pieces_served = threshold;
                    // Seed EWMA with prior speed so the first composite_score
                    // call has a useful speed factor instead of zero.
                    if rep.avg_speed_bytes_per_sec > 0 {
                        stats.ewma_speed = rep.avg_speed_bytes_per_sec as f64;
                    }
                }
            }
        }
    }

    /// Returns the identity for a peer, if registered.
    pub fn identity(&self, peer_index: usize) -> Option<&PeerId> {
        self.identities.get(peer_index).and_then(|opt| opt.as_ref())
    }

    /// Finds a peer index by identity, if registered.
    ///
    /// Useful for reconnection matching — when a new peer appears with
    /// a known identity, the coordinator can check prior session stats
    /// rather than starting from scratch.
    pub fn find_by_identity(&self, id: &PeerId) -> Option<usize> {
        self.identities
            .iter()
            .enumerate()
            .find(|(_, slot)| slot.as_ref() == Some(id))
            .map(|(i, _)| i)
    }

    /// Returns the highest EWMA speed among all peers (for normalisation).
    ///
    /// Uses EWMA rather than cumulative average for consistency with
    /// [`PeerStats::composite_score`]'s speed factor.
    pub fn reference_speed(&self) -> u64 {
        self.stats
            .iter()
            .map(|s| s.ewma_speed_bytes_per_sec())
            .max()
            .unwrap_or(0)
    }

    /// Returns the composite score for a peer at the given time.
    pub fn composite_score(&self, peer_index: usize, now: Instant) -> u64 {
        let ref_speed = self.reference_speed();
        self.stats
            .get(peer_index)
            .map(|s| s.composite_score(ref_speed, now))
            .unwrap_or(0)
    }

    /// Number of tracked peers.
    pub fn peer_count(&self) -> usize {
        self.stats.len()
    }

    /// Session start time.
    pub fn session_start(&self) -> Instant {
        self.session_start
    }

    /// Snapshots all identified peers into [`PeerReputation`]s for persistence.
    ///
    /// Returns reputations only for peers that have a registered identity.
    /// Unidentified peers cannot be tracked across sessions and are excluded.
    ///
    /// ## Parameters
    ///
    /// - `now_unix_secs`: current time as Unix epoch seconds (for `last_seen`).
    /// - `prior_reputations`: optional slice of prior reputations to merge
    ///   lifetime counters from. Looked up by `PeerId`.
    pub fn snapshot_reputations(
        &self,
        now_unix_secs: u64,
        prior_reputations: &[PeerReputation],
    ) -> Vec<PeerReputation> {
        self.stats
            .iter()
            .zip(self.identities.iter())
            .filter_map(|(stats, id_opt)| {
                let id = (*id_opt)?;
                let prior = prior_reputations.iter().find(|r| r.peer_id == id);
                Some(stats.to_reputation(id, now_unix_secs, prior))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── PeerStats construction ──────────────────────────────────────

    /// Fresh `PeerStats` starts with zero counters and no history.
    ///
    /// New peers have no served/requested pieces, zero bytes, and no
    /// timestamps. `first_seen` is set to the provided time.
    #[test]
    fn peer_stats_new_initial_values() {
        let now = Instant::now();
        let stats = PeerStats::new(now);

        assert_eq!(stats.pieces_served, 0);
        assert_eq!(stats.pieces_requested, 0);
        assert_eq!(stats.bytes_received, 0);
        assert_eq!(stats.corruption_count, 0);
        assert_eq!(stats.timeout_count, 0);
        assert_eq!(stats.rejection_count, 0);
        assert!(stats.last_rejection.is_none());
        assert_eq!(stats.peak_speed, 0);
        assert!(stats.last_success_at.is_none());
        assert!(stats.last_failure_at.is_none());
        assert_eq!(stats.disconnect_count, 0);
        assert_eq!(stats.first_seen, now);
    }

    // ── Recording events ────────────────────────────────────────────

    /// `record_success` updates served count, byte total, and speed.
    ///
    /// After a successful 1000-byte fetch over 100ms, the stats should
    /// reflect 1 piece served, 1000 bytes, and a 10 KB/s peak speed.
    #[test]
    fn record_success_updates_fields() {
        let now = Instant::now();
        let mut stats = PeerStats::new(now);

        stats.record_success(1000, Duration::from_millis(100), now);

        assert_eq!(stats.pieces_served, 1);
        assert_eq!(stats.pieces_requested, 1);
        assert_eq!(stats.bytes_received, 1000);
        assert_eq!(stats.peak_speed, 10_000); // 1000 bytes / 0.1s
        assert!(stats.last_success_at.is_some());
    }

    /// `record_corruption` increments corruption_count and total failures.
    ///
    /// SHA-1 mismatches are tracked separately from generic failures so the
    /// coordinator can distinguish malicious peers from flaky networks.
    #[test]
    fn record_corruption_updates_counts() {
        let now = Instant::now();
        let mut stats = PeerStats::new(now);

        stats.record_corruption(now);

        assert_eq!(stats.corruption_count, 1);
        assert_eq!(stats.pieces_requested, 1);
        assert_eq!(stats.pieces_served, 0);
    }

    /// `record_timeout` increments timeout_count and total failures.
    #[test]
    fn record_timeout_updates_counts() {
        let now = Instant::now();
        let mut stats = PeerStats::new(now);

        stats.record_timeout(now);

        assert_eq!(stats.timeout_count, 1);
        assert_eq!(stats.pieces_requested, 1);
    }

    /// `record_rejection` tracks the reason and increments rejection count.
    ///
    /// Structured rejections let the coordinator make smart backoff/avoid
    /// decisions based on why the peer refused.
    #[test]
    fn record_rejection_tracks_reason() {
        let now = Instant::now();
        let mut stats = PeerStats::new(now);

        stats.record_rejection(RejectionReason::RateLimited, now);

        assert_eq!(stats.rejection_count, 1);
        assert_eq!(stats.last_rejection, Some(RejectionReason::RateLimited));
        assert_eq!(stats.pieces_requested, 1);
    }

    // ── Derived metrics ─────────────────────────────────────────────

    /// `average_speed` is total bytes / total fetch duration.
    ///
    /// Two fetches of 1000 bytes in 100ms each → 10 KB/s average.
    #[test]
    fn average_speed_computed_from_cumulative() {
        let now = Instant::now();
        let mut stats = PeerStats::new(now);

        stats.record_success(1000, Duration::from_millis(100), now);
        stats.record_success(1000, Duration::from_millis(100), now);

        assert_eq!(stats.average_speed(), 10_000); // 2000 bytes / 0.2s
    }

    /// `average_speed` returns 0 when no successful fetches.
    #[test]
    fn average_speed_zero_when_no_fetches() {
        let now = Instant::now();
        let stats = PeerStats::new(now);
        assert_eq!(stats.average_speed(), 0);
    }

    /// `success_rate` is served / requested.
    ///
    /// 3 served out of 4 requested → 0.75.
    #[test]
    fn success_rate_reflects_ratio() {
        let now = Instant::now();
        let mut stats = PeerStats::new(now);

        stats.record_success(100, Duration::from_millis(10), now);
        stats.record_success(100, Duration::from_millis(10), now);
        stats.record_success(100, Duration::from_millis(10), now);
        stats.record_failure(now);

        let rate = stats.success_rate();
        assert!((rate - 0.75).abs() < 0.001, "expected ~0.75, got {rate}");
    }

    /// `success_rate` returns 1.0 for new peers (benefit of the doubt).
    #[test]
    fn success_rate_new_peer_benefit_of_doubt() {
        let now = Instant::now();
        let stats = PeerStats::new(now);
        assert!((stats.success_rate() - 1.0).abs() < f64::EPSILON);
    }

    /// `corruption_rate` is corruption_count / requested.
    #[test]
    fn corruption_rate_reflects_ratio() {
        let now = Instant::now();
        let mut stats = PeerStats::new(now);

        stats.record_success(100, Duration::from_millis(10), now);
        stats.record_corruption(now);

        let rate = stats.corruption_rate();
        assert!((rate - 0.5).abs() < 0.001, "expected ~0.5, got {rate}");
    }

    // ── Composite scoring ───────────────────────────────────────────

    /// A fast, reliable peer scores higher than a slow, flaky one.
    ///
    /// This is the core invariant: composite scoring must rank good peers
    /// above bad peers so the coordinator picks better sources first.
    #[test]
    fn composite_score_fast_reliable_beats_slow_flaky() {
        let now = Instant::now();

        // Good peer: fast, 100% success rate.
        let mut good = PeerStats::new(now);
        good.record_success(10_000, Duration::from_millis(100), now); // 100 KB/s
        good.record_success(10_000, Duration::from_millis(100), now);

        // Bad peer: slow, 50% failure rate, with disconnects.
        let mut bad = PeerStats::new(now);
        bad.record_success(1_000, Duration::from_millis(100), now); // 10 KB/s
        bad.record_failure(now);
        bad.disconnect_count = 5;

        let ref_speed = good.average_speed(); // 100 KB/s
        let good_score = good.composite_score(ref_speed, now);
        let bad_score = bad.composite_score(ref_speed, now);

        assert!(
            good_score > bad_score,
            "good ({good_score}) should beat bad ({bad_score})"
        );
    }

    /// A brand-new peer (no history) gets a reasonable middle score.
    ///
    /// New peers should not be penalised too harshly (they might be excellent)
    /// but should not outrank proven reliable peers either.
    #[test]
    fn composite_score_new_peer_middle_range() {
        let now = Instant::now();

        // Proven good peer.
        let mut proven = PeerStats::new(now);
        proven.record_success(10_000, Duration::from_millis(100), now);

        // Brand-new peer (no history).
        let new_peer = PeerStats::new(now);

        let ref_speed = proven.average_speed();
        let proven_score = proven.composite_score(ref_speed, now);
        let new_score = new_peer.composite_score(ref_speed, now);

        assert!(
            new_score > 0,
            "new peer should have non-zero score: {new_score}"
        );
        assert!(
            proven_score > new_score,
            "proven ({proven_score}) should beat new ({new_score})"
        );
    }

    /// Composite score is deterministic — same inputs yield same output.
    #[test]
    fn composite_score_deterministic() {
        let now = Instant::now();

        let mut stats = PeerStats::new(now);
        stats.record_success(5000, Duration::from_millis(50), now);

        let s1 = stats.composite_score(100_000, now);
        let s2 = stats.composite_score(100_000, now);
        assert_eq!(s1, s2);
    }

    // ── Backoff and avoidance ───────────────────────────────────────

    /// `should_back_off` returns true for recent transient rejections.
    ///
    /// Rate-limited and swarm-full rejections are transient — the coordinator
    /// should skip this peer for a few iterations and try again.
    #[test]
    fn should_back_off_transient_rejection() {
        let now = Instant::now();
        let mut stats = PeerStats::new(now);

        stats.record_rejection(RejectionReason::RateLimited, now);
        assert!(stats.should_back_off(now));
    }

    /// `should_back_off` returns false for permanent rejections.
    ///
    /// Policy violations and maintenance are not transient — backoff doesn't
    /// help. The peer should be avoided entirely via `should_avoid_permanently`.
    #[test]
    fn should_back_off_false_for_permanent() {
        let now = Instant::now();
        let mut stats = PeerStats::new(now);

        stats.record_rejection(RejectionReason::Maintenance, now);
        assert!(!stats.should_back_off(now));
    }

    /// `should_avoid_permanently` returns true for non-transient rejections.
    ///
    /// Policy violation, maintenance, and auth failures indicate the peer
    /// will never serve us — stop trying.
    #[test]
    fn should_avoid_permanently_policy_violation() {
        let now = Instant::now();
        let mut stats = PeerStats::new(now);

        stats.record_rejection(RejectionReason::PolicyViolation, now);
        assert!(stats.should_avoid_permanently());
    }

    /// `should_avoid_permanently` returns false for transient rejections.
    #[test]
    fn should_avoid_permanently_false_for_transient() {
        let now = Instant::now();
        let mut stats = PeerStats::new(now);

        stats.record_rejection(RejectionReason::SwarmFull, now);
        assert!(!stats.should_avoid_permanently());
    }

    // ── Disconnect detection ────────────────────────────────────────

    /// A success→failure→success cycle increments disconnect_count.
    ///
    /// The coordinator penalises flaky peers in availability scoring.
    /// This test verifies that the disconnect detection pattern works.
    #[test]
    fn disconnect_detected_on_success_after_failure() {
        let now = Instant::now();
        let mut stats = PeerStats::new(now);

        // First success — establishes baseline.
        stats.record_success(1000, Duration::from_millis(10), now);
        assert_eq!(stats.disconnect_count, 0);

        // Failure — peer dropped.
        stats.record_failure(now);

        // Second success — peer reconnected.
        stats.record_success(1000, Duration::from_millis(10), now);
        assert_eq!(stats.disconnect_count, 1);
    }

    // ── PeerTracker ─────────────────────────────────────────────────

    /// `PeerTracker` creates stats for the requested number of peers.
    #[test]
    fn peer_tracker_creates_correct_count() {
        let now = Instant::now();
        let tracker = PeerTracker::new(5, now);

        assert_eq!(tracker.peer_count(), 5);
        assert!(tracker.get(0).is_some());
        assert!(tracker.get(4).is_some());
        assert!(tracker.get(5).is_none());
    }

    /// `reference_speed` returns the max average speed across all peers.
    ///
    /// The reference speed normalises individual peer speeds to [0, 1]
    /// in the composite score.
    #[test]
    fn peer_tracker_reference_speed() {
        let now = Instant::now();
        let mut tracker = PeerTracker::new(2, now);

        // Peer 0: 10 KB/s, Peer 1: 100 KB/s.
        if let Some(s) = tracker.get_mut(0) {
            s.record_success(1000, Duration::from_millis(100), now);
        }
        if let Some(s) = tracker.get_mut(1) {
            s.record_success(10_000, Duration::from_millis(100), now);
        }

        assert_eq!(tracker.reference_speed(), 100_000); // 10KB/0.1s
    }

    /// `composite_score` delegates to `PeerStats` with correct reference speed.
    #[test]
    fn peer_tracker_composite_score_delegates() {
        let now = Instant::now();
        let mut tracker = PeerTracker::new(2, now);

        if let Some(s) = tracker.get_mut(0) {
            s.record_success(10_000, Duration::from_millis(100), now);
        }
        if let Some(s) = tracker.get_mut(1) {
            s.record_success(1_000, Duration::from_millis(100), now);
        }

        let score_0 = tracker.composite_score(0, now);
        let score_1 = tracker.composite_score(1, now);

        assert!(
            score_0 > score_1,
            "faster peer 0 ({score_0}) should score higher than peer 1 ({score_1})"
        );
    }

    /// Out-of-bounds peer index returns score 0.
    #[test]
    fn peer_tracker_out_of_bounds_returns_zero() {
        let now = Instant::now();
        let tracker = PeerTracker::new(1, now);
        assert_eq!(tracker.composite_score(999, now), 0);
    }

    // ── RejectionReason Display ─────────────────────────────────────

    /// Each `RejectionReason` variant has a meaningful display string.
    #[test]
    fn rejection_reason_display() {
        assert_eq!(RejectionReason::RateLimited.to_string(), "rate limited");
        assert_eq!(RejectionReason::SwarmFull.to_string(), "swarm full");
        assert_eq!(
            RejectionReason::InsufficientAuth.to_string(),
            "insufficient authentication"
        );
        assert_eq!(
            RejectionReason::PolicyViolation.to_string(),
            "policy violation"
        );
        assert_eq!(RejectionReason::Maintenance.to_string(), "peer maintenance");
        assert_eq!(
            RejectionReason::Other("custom".into()).to_string(),
            "custom"
        );
    }

    // ── Trust levels (TCP slow-start) ───────────────────────────────

    /// Trust progresses from Untested through Probationary and Established
    /// to Trusted as pieces_served increases.
    ///
    /// Maps to TCP slow-start: new connections start with a small congestion
    /// window and scale up as ACKs confirm the path.
    #[test]
    fn trust_level_transitions() {
        let now = Instant::now();
        let mut stats = PeerStats::new(now);

        assert_eq!(stats.trust_level(), TrustLevel::Untested);

        // 1 success → Probationary.
        stats.record_success(100, Duration::from_millis(10), now);
        assert_eq!(stats.trust_level(), TrustLevel::Probationary);

        // 2 more → Established (total: 3).
        stats.record_success(100, Duration::from_millis(10), now);
        stats.record_success(100, Duration::from_millis(10), now);
        assert_eq!(stats.trust_level(), TrustLevel::Established);

        // 7 more → Trusted (total: 10).
        for _ in 0..7 {
            stats.record_success(100, Duration::from_millis(10), now);
        }
        assert_eq!(stats.trust_level(), TrustLevel::Trusted);
    }

    /// Trust multipliers produce the correct values for each level.
    #[test]
    fn trust_level_multipliers() {
        assert!((TrustLevel::Untested.multiplier() - 0.5).abs() < f64::EPSILON);
        assert!((TrustLevel::Probationary.multiplier() - 0.7).abs() < f64::EPSILON);
        assert!((TrustLevel::Established.multiplier() - 0.9).abs() < f64::EPSILON);
        assert!((TrustLevel::Trusted.multiplier() - 1.0).abs() < f64::EPSILON);
    }

    /// Trust multiplier reduces untested peers' composite scores.
    ///
    /// An untested peer with the same raw factors as a trusted peer should
    /// score lower, preventing unknown peers from displacing proven ones.
    #[test]
    fn trust_multiplier_affects_composite_score() {
        let now = Instant::now();

        // Trusted peer: 10+ successes.
        let mut trusted = PeerStats::new(now);
        for _ in 0..10 {
            trusted.record_success(10_000, Duration::from_millis(100), now);
        }

        // Fresh untested peer (same speed estimate but no track record).
        let mut untested = PeerStats::new(now);
        untested.ewma_speed = trusted.ewma_speed; // Same EWMA for fair comparison.

        let ref_speed = trusted.ewma_speed_bytes_per_sec();
        let trusted_score = trusted.composite_score(ref_speed, now);
        let untested_score = untested.composite_score(ref_speed, now);

        assert!(
            trusted_score > untested_score,
            "trusted ({trusted_score}) should beat untested ({untested_score})"
        );
    }

    // ── EWMA speed tracking ────────────────────────────────────────

    /// EWMA speed initialises from the first successful fetch.
    ///
    /// The first observation seeds the EWMA rather than blending with zero,
    /// matching WebSeedPeer's behaviour and TCP's initial RTT estimate.
    #[test]
    fn ewma_speed_initialises_from_first_fetch() {
        let now = Instant::now();
        let mut stats = PeerStats::new(now);

        // 10000 bytes in 100ms = 100 KB/s.
        stats.record_success(10_000, Duration::from_millis(100), now);
        assert_eq!(stats.ewma_speed_bytes_per_sec(), 100_000);
    }

    /// EWMA speed converges toward the current rate over successive fetches.
    ///
    /// After switching from 100 KB/s to a sustained 200 KB/s, the EWMA
    /// should trend upward with each observation.
    #[test]
    fn ewma_speed_converges_toward_current_rate() {
        let now = Instant::now();
        let mut stats = PeerStats::new(now);

        // One fetch at 100 KB/s.
        stats.record_success(10_000, Duration::from_millis(100), now);
        let after_slow = stats.ewma_speed;

        // Three fetches at 200 KB/s — EWMA should trend upward.
        for _ in 0..3 {
            stats.record_success(20_000, Duration::from_millis(100), now);
        }
        let after_fast = stats.ewma_speed;

        assert!(
            after_fast > after_slow,
            "EWMA should increase: {after_slow} → {after_fast}"
        );
        // Should be above 100K but below 200K (still converging).
        assert!(after_fast > 100_000.0);
        assert!(after_fast < 200_000.0);
    }

    /// EWMA speed returns 0 when no fetches recorded.
    #[test]
    fn ewma_speed_zero_when_no_fetches() {
        let now = Instant::now();
        let stats = PeerStats::new(now);
        assert_eq!(stats.ewma_speed_bytes_per_sec(), 0);
    }

    // ── Exponential backoff (TCP RFC 6298) ──────────────────────────

    /// Consecutive transient rejections double the backoff window.
    ///
    /// TCP doubles the retransmission timeout with each failed attempt.
    /// Same principle: 5s → 10s → 20s → 40s → 60s (capped).
    #[test]
    fn exponential_backoff_doubles_with_consecutive_rejections() {
        let now = Instant::now();
        let mut stats = PeerStats::new(now);

        // First rejection: 5s backoff.
        stats.record_rejection(RejectionReason::RateLimited, now);
        assert_eq!(stats.consecutive_transient_rejections, 1);
        assert!(stats.should_back_off(now));

        // Second rejection: 10s backoff.
        stats.record_rejection(RejectionReason::RateLimited, now);
        assert_eq!(stats.consecutive_transient_rejections, 2);
        assert!(stats.should_back_off(now));

        // Third rejection: 20s backoff.
        stats.record_rejection(RejectionReason::SwarmFull, now);
        assert_eq!(stats.consecutive_transient_rejections, 3);
        assert!(stats.should_back_off(now));
    }

    /// A successful fetch resets the consecutive rejection counter.
    ///
    /// Matches TCP behaviour: successful ACKs reset the retransmit timer
    /// back to the initial value.
    #[test]
    fn backoff_resets_on_success() {
        let now = Instant::now();
        let mut stats = PeerStats::new(now);

        // Build up consecutive rejections.
        stats.record_rejection(RejectionReason::RateLimited, now);
        stats.record_rejection(RejectionReason::RateLimited, now);
        assert_eq!(stats.consecutive_transient_rejections, 2);

        // Success resets the counter.
        stats.record_success(1000, Duration::from_millis(10), now);
        assert_eq!(stats.consecutive_transient_rejections, 0);
    }

    /// Non-transient rejections don't increment the consecutive counter.
    ///
    /// PolicyViolation and Maintenance are permanent — exponential backoff
    /// doesn't apply. They're handled by should_avoid_permanently() instead.
    #[test]
    fn non_transient_rejection_does_not_increment_consecutive() {
        let now = Instant::now();
        let mut stats = PeerStats::new(now);

        stats.record_rejection(RejectionReason::PolicyViolation, now);
        assert_eq!(stats.consecutive_transient_rejections, 0);

        stats.record_rejection(RejectionReason::Maintenance, now);
        assert_eq!(stats.consecutive_transient_rejections, 0);
    }

    // ── PeerReputation ──────────────────────────────────────────────

    /// `to_reputation` creates a snapshot capturing trust level, speed,
    /// and piece counts from the current session.
    ///
    /// The snapshot is the cross-session persistence unit — downstream
    /// consumers serialise it, and future sessions feed it back via
    /// `PeerTracker::with_prior_reputation`.
    #[test]
    fn to_reputation_captures_session_state() {
        let now = Instant::now();
        let id = PeerId::from_key_material(b"test-peer");
        let mut stats = PeerStats::new(now);

        // Build up some history.
        for _ in 0..5 {
            stats.record_success(10_000, Duration::from_millis(100), now);
        }

        let rep = stats.to_reputation(id, 1_700_000_000, None);

        assert_eq!(rep.peer_id, id);
        assert_eq!(rep.trust_level, TrustLevel::Established);
        assert_eq!(rep.lifetime_pieces_served, 5);
        assert_eq!(rep.lifetime_corruption_count, 0);
        assert!(rep.avg_speed_bytes_per_sec > 0);
        assert_eq!(rep.last_seen_unix_secs, 1_700_000_000);
        assert!(!rep.banned);
    }

    /// `to_reputation` accumulates lifetime counters across sessions
    /// when given a prior reputation.
    ///
    /// This ensures that a peer's long-term track record grows with each
    /// session, not just the current one.
    #[test]
    fn to_reputation_accumulates_with_prior() {
        let now = Instant::now();
        let id = PeerId::from_key_material(b"returning-peer");

        // Prior: 100 pieces served in earlier sessions.
        let prior = PeerReputation {
            peer_id: id,
            trust_level: TrustLevel::Trusted,
            lifetime_pieces_served: 100,
            lifetime_corruption_count: 2,
            avg_speed_bytes_per_sec: 50_000,
            last_seen_unix_secs: 1_699_000_000,
            banned: false,
        };

        let mut stats = PeerStats::new(now);
        stats.record_success(10_000, Duration::from_millis(100), now);
        stats.record_corruption(now);

        let rep = stats.to_reputation(id, 1_700_000_000, Some(&prior));

        // Current session: 1 served + prior 100 = 101.
        assert_eq!(rep.lifetime_pieces_served, 101);
        // Current session: 1 corruption + prior 2 = 3.
        assert_eq!(rep.lifetime_corruption_count, 3);
    }

    /// `to_reputation` marks banned peers when permanently avoided.
    #[test]
    fn to_reputation_marks_banned() {
        let now = Instant::now();
        let id = PeerId::from_key_material(b"bad-peer");
        let mut stats = PeerStats::new(now);

        stats.record_rejection(RejectionReason::PolicyViolation, now);

        let rep = stats.to_reputation(id, 1_700_000_000, None);
        assert!(rep.banned);
    }

    // ── PeerTracker identity ────────────────────────────────────────

    /// `register_identity` binds a PeerId to a peer index.
    ///
    /// After registration, the identity is retrievable via `identity()`
    /// and `find_by_identity()`.
    #[test]
    fn register_identity_and_lookup() {
        let now = Instant::now();
        let mut tracker = PeerTracker::new(3, now);
        let id = PeerId::from_key_material(b"https://mirror.example.com/file.zip");

        tracker.register_identity(1, id);

        assert_eq!(tracker.identity(1), Some(&id));
        assert_eq!(tracker.find_by_identity(&id), Some(1));
        assert!(tracker.identity(0).is_none());
    }

    /// `find_by_identity` returns `None` for unregistered identities.
    #[test]
    fn find_by_identity_unknown() {
        let now = Instant::now();
        let tracker = PeerTracker::new(2, now);
        let id = PeerId::from_key_material(b"unknown");

        assert!(tracker.find_by_identity(&id).is_none());
    }

    /// Prior reputation restores trust level for returning peers.
    ///
    /// A peer that was Trusted in a prior session should start the new
    /// session at Trusted (skipping Untested → Probationary → Established
    /// slow-start). This is the primary value of cross-session identity.
    #[test]
    fn prior_reputation_restores_trust_level() {
        let now = Instant::now();
        let id = PeerId::from_key_material(b"returning-trusted-peer");

        let prior = vec![PeerReputation {
            peer_id: id,
            trust_level: TrustLevel::Trusted,
            lifetime_pieces_served: 500,
            lifetime_corruption_count: 0,
            avg_speed_bytes_per_sec: 100_000,
            last_seen_unix_secs: 1_699_000_000,
            banned: false,
        }];

        let mut tracker = PeerTracker::with_prior_reputation(2, now, prior);

        // Before identity registration — peer is fresh (Untested).
        assert_eq!(
            tracker.get(0).map(|s| s.trust_level()),
            Some(TrustLevel::Untested)
        );

        // Register identity for peer 0 — should restore Trusted.
        tracker.register_identity(0, id);

        assert_eq!(
            tracker.get(0).map(|s| s.trust_level()),
            Some(TrustLevel::Trusted)
        );
        // EWMA speed also restored from prior.
        assert_eq!(
            tracker.get(0).map(|s| s.ewma_speed_bytes_per_sec()),
            Some(100_000)
        );
    }

    /// Prior reputation bans carry over to new sessions.
    ///
    /// A peer that was banned (e.g. for data corruption) in a prior session
    /// should be immediately avoided when it reconnects.
    #[test]
    fn prior_reputation_applies_ban() {
        let now = Instant::now();
        let id = PeerId::from_key_material(b"banned-peer");

        let prior = vec![PeerReputation {
            peer_id: id,
            trust_level: TrustLevel::Trusted,
            lifetime_pieces_served: 200,
            lifetime_corruption_count: 50,
            avg_speed_bytes_per_sec: 80_000,
            last_seen_unix_secs: 1_699_000_000,
            banned: true,
        }];

        let mut tracker = PeerTracker::with_prior_reputation(1, now, prior);
        tracker.register_identity(0, id);

        // Banned peer should be permanently avoided.
        assert!(tracker
            .get(0)
            .map(|s| s.should_avoid_permanently())
            .unwrap_or(false));
    }

    /// `snapshot_reputations` only includes identified peers.
    ///
    /// Unidentified peers cannot be tracked across sessions, so they are
    /// excluded from the snapshot.
    #[test]
    fn snapshot_reputations_excludes_unidentified() {
        let now = Instant::now();
        let mut tracker = PeerTracker::new(3, now);
        let id = PeerId::from_key_material(b"identified-peer");

        // Only peer 1 is identified.
        tracker.register_identity(1, id);

        // Record some activity for all peers.
        for i in 0..3 {
            if let Some(s) = tracker.get_mut(i) {
                s.record_success(1000, Duration::from_millis(10), now);
            }
        }

        let reps = tracker.snapshot_reputations(1_700_000_000, &[]);

        // Only 1 reputation (the identified peer).
        assert_eq!(reps.len(), 1);
        assert_eq!(reps[0].peer_id, id);
        assert_eq!(reps[0].lifetime_pieces_served, 1);
    }

    /// `snapshot_reputations` merges with prior reputation data.
    #[test]
    fn snapshot_reputations_merges_prior() {
        let now = Instant::now();
        let id = PeerId::from_key_material(b"multi-session-peer");

        let prior = vec![PeerReputation {
            peer_id: id,
            trust_level: TrustLevel::Established,
            lifetime_pieces_served: 50,
            lifetime_corruption_count: 1,
            avg_speed_bytes_per_sec: 75_000,
            last_seen_unix_secs: 1_699_000_000,
            banned: false,
        }];

        let mut tracker = PeerTracker::new(1, now);
        tracker.register_identity(0, id);

        if let Some(s) = tracker.get_mut(0) {
            s.record_success(10_000, Duration::from_millis(100), now);
        }

        let reps = tracker.snapshot_reputations(1_700_000_000, &prior);

        assert_eq!(reps.len(), 1);
        // 50 prior + 1 current = 51.
        assert_eq!(reps[0].lifetime_pieces_served, 51);
    }

    // ── Anti-snubbing ────────────────────────────────────────────────

    /// A freshly created peer is not snubbed within the grace period.
    ///
    /// Peers should get at least `SNUB_TIMEOUT_SECS` before being
    /// considered unresponsive.
    #[test]
    fn not_snubbed_within_grace_period() {
        let now = Instant::now();
        let stats = PeerStats::new(now);
        // 30 seconds in — well under the 60s threshold.
        let later = now + Duration::from_secs(30);
        assert!(!stats.is_snubbed(later));
    }

    /// A peer with no successes becomes snubbed after the timeout.
    ///
    /// If a peer never delivers any data within `SNUB_TIMEOUT_SECS`,
    /// it is snubbed. This prevents dead connections from blocking
    /// piece assignment.
    #[test]
    fn snubbed_after_timeout_without_success() {
        let now = Instant::now();
        let stats = PeerStats::new(now);
        let later = now + Duration::from_secs(61);
        assert!(stats.is_snubbed(later));
    }

    /// A recent success resets the snub timer.
    ///
    /// After a successful piece delivery, the peer gets a fresh
    /// `SNUB_TIMEOUT_SECS` window before being considered snubbed again.
    #[test]
    fn success_resets_snub_timer() {
        let now = Instant::now();
        let mut stats = PeerStats::new(now);
        let t1 = now + Duration::from_secs(50);
        stats.record_success(1000, Duration::from_millis(100), t1);
        // 70s after initial creation, but only 20s after last success.
        let t2 = now + Duration::from_secs(70);
        assert!(!stats.is_snubbed(t2));
        // 111s after last success — snubbed.
        let t3 = t1 + Duration::from_secs(61);
        assert!(stats.is_snubbed(t3));
    }

    // ── Lazy interaction tracking ────────────────────────────────────

    /// A freshly created peer has not interacted.
    ///
    /// Shadow peers (no requests, no rejections) should not influence
    /// scoring or selection. This follows WireGuard's principle of not
    /// allocating state for unauthenticated entities.
    #[test]
    fn new_peer_has_not_interacted() {
        let now = Instant::now();
        let stats = PeerStats::new(now);
        assert!(!stats.has_interacted());
    }

    /// A peer that delivered a piece has interacted.
    #[test]
    fn success_counts_as_interaction() {
        let now = Instant::now();
        let mut stats = PeerStats::new(now);
        stats.record_success(1000, Duration::from_millis(100), now);
        assert!(stats.has_interacted());
    }

    /// A peer that rejected a request has interacted.
    ///
    /// Rejection proves the peer exists and was contacted, even though
    /// no data was delivered. It should be counted as a real peer.
    #[test]
    fn rejection_counts_as_interaction() {
        let now = Instant::now();
        let mut stats = PeerStats::new(now);
        stats.record_rejection(RejectionReason::RateLimited, now);
        assert!(stats.has_interacted());
    }
}
