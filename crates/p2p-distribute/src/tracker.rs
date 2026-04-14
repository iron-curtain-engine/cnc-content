// SPDX-License-Identifier: MIT OR Apache-2.0

//! BitTorrent tracker protocol — announce, scrape, and response parsing.
//!
//! ## What
//!
//! Implements the client-side tracker protocol for both HTTP (BEP 3) and
//! UDP (BEP 15) trackers. The tracker is the primary bootstrap mechanism:
//! a new peer announces itself to the tracker and receives a peer list in
//! return. Periodic re-announces keep the swarm membership fresh.
//!
//! ## Why — centralised discovery as bootstrap baseline
//!
//! While DHT and PEX provide decentralised peer discovery, trackers remain
//! the fastest bootstrap path. OpenRA's mirror list infrastructure serves
//! an analogous role: a single authoritative URL that returns a list of
//! mirrors. CnCNet's tunnel discovery API works the same way — query one
//! endpoint, get a list of available servers.
//!
//! Key insights from the C&C ecosystem:
//!
//! - **Tracker as bootstrap, not authority.** OpenRA's mirror list is the
//!   first place clients check, but content works without it (direct URLs
//!   as fallback). Trackers should be the same: fast bootstrap, not SPOF.
//! - **Multiple trackers increase resilience.** `data/trackers.txt` lists
//!   5 public trackers. Announcing to all of them in parallel ensures the
//!   swarm survives any single tracker going down.
//! - **Compact peer encoding saves bandwidth.** BEP 23 compact responses
//!   encode each peer as 6 bytes (4 IP + 2 port) instead of a bencoded
//!   dict. For large swarms this is a 10x reduction.
//! - **Scrape before announce.** Checking swarm size before announcing
//!   lets the client skip dead torrents without committing to the swarm.
//!
//! ## How
//!
//! - [`AnnounceRequest`]: Parameters for a tracker announce.
//! - [`AnnounceResponse`]: Parsed response with peer list and interval.
//! - [`ScrapeResponse`]: Swarm statistics (seeders, leechers, completed).
//! - [`TrackerEvent`]: Lifecycle events (Started, Stopped, Completed).
//! - [`TrackerState`]: Per-tracker state machine (announce scheduling,
//!   failure backoff, peer list caching).

use std::time::{Duration, Instant};

// ── Constants ───────────────────────────────────────────────────────

/// Default announce interval when the tracker does not specify one.
const DEFAULT_ANNOUNCE_INTERVAL: Duration = Duration::from_secs(1800);

/// Minimum announce interval to avoid hammering trackers.
const MIN_ANNOUNCE_INTERVAL: Duration = Duration::from_secs(60);

/// Maximum consecutive failures before marking a tracker as dead.
const MAX_TRACKER_FAILURES: u32 = 5;

/// Backoff multiplier for consecutive failures (exponential).
const BACKOFF_MULTIPLIER: u32 = 2;

/// Initial backoff duration after first failure.
const INITIAL_BACKOFF: Duration = Duration::from_secs(30);

/// Maximum backoff duration cap.
const MAX_BACKOFF: Duration = Duration::from_secs(3600);

/// Compact peer entry size: 4 bytes IPv4 + 2 bytes port (BEP 23).
const COMPACT_PEER_SIZE: usize = 6;

/// Maximum peers to request in an announce (BEP 3 `numwant`).
const DEFAULT_NUMWANT: u32 = 50;

// ── Tracker event ───────────────────────────────────────────────────

/// Lifecycle events sent from client to tracker.
///
/// BEP 3 defines these as query parameters on the announce URL. The
/// event field is omitted for regular periodic re-announces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackerEvent {
    /// First announce — client is joining the swarm.
    Started,
    /// Client is leaving the swarm gracefully.
    Stopped,
    /// Client has finished downloading (becomes a seeder).
    Completed,
}

// ── Announce request ────────────────────────────────────────────────

/// Parameters for a tracker announce request.
///
/// Constructed by the session manager and serialised into HTTP query
/// parameters (BEP 3) or a UDP announce packet (BEP 15).
#[derive(Debug, Clone)]
pub struct AnnounceRequest {
    /// 20-byte info hash of the torrent.
    info_hash: [u8; 20],
    /// 20-byte peer ID (our identity).
    peer_id: [u8; 20],
    /// Total bytes downloaded so far.
    downloaded: u64,
    /// Total bytes remaining.
    left: u64,
    /// Total bytes uploaded so far.
    uploaded: u64,
    /// Port we're listening on for incoming peer connections.
    port: u16,
    /// Lifecycle event (None for periodic re-announces).
    event: Option<TrackerEvent>,
    /// Maximum peers to request.
    numwant: u32,
    /// Whether we want compact peer encoding (BEP 23).
    compact: bool,
}

impl AnnounceRequest {
    /// Creates a new announce request.
    pub fn new(info_hash: [u8; 20], peer_id: [u8; 20], port: u16) -> Self {
        Self {
            info_hash,
            peer_id,
            downloaded: 0,
            left: 0,
            uploaded: 0,
            port,
            event: None,
            numwant: DEFAULT_NUMWANT,
            compact: true,
        }
    }

    /// Sets the lifecycle event.
    pub fn with_event(mut self, event: TrackerEvent) -> Self {
        self.event = Some(event);
        self
    }

    /// Sets the download/upload/left byte counters.
    pub fn with_stats(mut self, downloaded: u64, uploaded: u64, left: u64) -> Self {
        self.downloaded = downloaded;
        self.uploaded = uploaded;
        self.left = left;
        self
    }

    /// Sets the number of peers to request.
    pub fn with_numwant(mut self, n: u32) -> Self {
        self.numwant = n;
        self
    }

    /// Returns the info hash.
    pub fn info_hash(&self) -> &[u8; 20] {
        &self.info_hash
    }

    /// Returns the peer ID.
    pub fn peer_id(&self) -> &[u8; 20] {
        &self.peer_id
    }

    /// Returns the listen port.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Returns the lifecycle event.
    pub fn event(&self) -> Option<TrackerEvent> {
        self.event
    }

    /// Returns whether compact encoding is requested.
    pub fn compact(&self) -> bool {
        self.compact
    }

    /// Returns the numwant value.
    pub fn numwant(&self) -> u32 {
        self.numwant
    }
}

// ── Compact peer ────────────────────────────────────────────────────

/// A peer endpoint parsed from a compact BEP 23 response.
///
/// Each peer is encoded as 6 bytes: 4 bytes big-endian IPv4 address
/// followed by 2 bytes big-endian port number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactPeer {
    /// IPv4 address as 4 bytes.
    ip: [u8; 4],
    /// Port number.
    port: u16,
}

impl CompactPeer {
    /// Parses a compact peer from a 6-byte slice.
    ///
    /// Returns `None` if the slice is too short.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < COMPACT_PEER_SIZE {
            return None;
        }
        let ip = [
            *bytes.first()?,
            *bytes.get(1)?,
            *bytes.get(2)?,
            *bytes.get(3)?,
        ];
        let port_hi = *bytes.get(4)? as u16;
        let port_lo = *bytes.get(5)? as u16;
        let port = (port_hi << 8) | port_lo;
        Some(Self { ip, port })
    }

    /// Serialises this peer into 6 bytes (BEP 23 compact format).
    pub fn to_bytes(self) -> [u8; COMPACT_PEER_SIZE] {
        [
            self.ip[0],
            self.ip[1],
            self.ip[2],
            self.ip[3],
            (self.port >> 8) as u8,
            (self.port & 0xFF) as u8,
        ]
    }

    /// Returns the IPv4 address.
    pub fn ip(&self) -> [u8; 4] {
        self.ip
    }

    /// Returns the port number.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Returns a dotted-decimal string representation.
    pub fn ip_string(&self) -> String {
        format!(
            "{}.{}.{}.{}",
            self.ip[0], self.ip[1], self.ip[2], self.ip[3]
        )
    }
}

/// Parses a compact peer list (BEP 23) from raw bytes.
///
/// Each peer is exactly 6 bytes. Trailing bytes that don't complete a
/// full peer entry are silently ignored.
pub fn parse_compact_peers(data: &[u8]) -> Vec<CompactPeer> {
    data.chunks_exact(COMPACT_PEER_SIZE)
        .filter_map(CompactPeer::from_bytes)
        .collect()
}

// ── Announce response ───────────────────────────────────────────────

/// Parsed response from a tracker announce.
///
/// Contains the peer list, re-announce interval, and optional metadata.
#[derive(Debug, Clone)]
pub struct AnnounceResponse {
    /// Recommended re-announce interval.
    interval: Duration,
    /// Minimum re-announce interval (tracker-enforced floor).
    min_interval: Option<Duration>,
    /// Peers returned by the tracker.
    peers: Vec<CompactPeer>,
    /// Number of seeders the tracker knows about.
    complete: Option<u32>,
    /// Number of leechers the tracker knows about.
    incomplete: Option<u32>,
    /// Warning message from the tracker.
    warning: Option<String>,
}

impl AnnounceResponse {
    /// Creates a new announce response.
    pub fn new(interval: Duration, peers: Vec<CompactPeer>) -> Self {
        Self {
            interval,
            min_interval: None,
            peers,
            complete: None,
            incomplete: None,
            warning: None,
        }
    }

    /// Sets optional swarm statistics.
    pub fn with_stats(mut self, complete: u32, incomplete: u32) -> Self {
        self.complete = Some(complete);
        self.incomplete = Some(incomplete);
        self
    }

    /// Sets a warning message.
    pub fn with_warning(mut self, warning: String) -> Self {
        self.warning = Some(warning);
        self
    }

    /// Sets the minimum announce interval.
    pub fn with_min_interval(mut self, min: Duration) -> Self {
        self.min_interval = Some(min);
        self
    }

    /// Returns the recommended re-announce interval.
    pub fn interval(&self) -> Duration {
        self.interval
    }

    /// Returns the peers.
    pub fn peers(&self) -> &[CompactPeer] {
        &self.peers
    }

    /// Returns the number of seeders.
    pub fn complete(&self) -> Option<u32> {
        self.complete
    }

    /// Returns the number of leechers.
    pub fn incomplete(&self) -> Option<u32> {
        self.incomplete
    }

    /// Returns any warning message.
    pub fn warning(&self) -> Option<&str> {
        self.warning.as_deref()
    }
}

// ── Scrape response ─────────────────────────────────────────────────

/// Parsed response from a tracker scrape request.
///
/// Provides swarm statistics without joining the swarm. Useful for
/// deciding whether to start a download.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrapeResponse {
    /// Number of seeders (peers with complete file).
    pub complete: u32,
    /// Number of leechers (peers still downloading).
    pub incomplete: u32,
    /// Total completed downloads (historical).
    pub downloaded: u32,
}

impl ScrapeResponse {
    /// Returns whether the swarm has any activity.
    pub fn is_active(&self) -> bool {
        self.complete > 0 || self.incomplete > 0
    }

    /// Returns the total peers (seeders + leechers).
    pub fn total_peers(&self) -> u32 {
        self.complete.saturating_add(self.incomplete)
    }
}

// ── Tracker state ───────────────────────────────────────────────────

/// Health status of a tracker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackerStatus {
    /// Tracker is healthy and responding.
    Active,
    /// Tracker has failed recently but hasn't exceeded the failure limit.
    Degraded,
    /// Tracker has exceeded the failure limit.
    Dead,
}

/// Per-tracker state machine managing announce scheduling and backoff.
///
/// ```
/// use p2p_distribute::tracker::{TrackerState, TrackerStatus};
/// use std::time::Instant;
///
/// let now = Instant::now();
/// let mut state = TrackerState::new("http://tracker.example.com/announce".into(), now);
///
/// assert_eq!(state.status(), TrackerStatus::Active);
/// assert!(state.should_announce(now), "new tracker should announce immediately");
/// ```
#[derive(Debug, Clone)]
pub struct TrackerState {
    /// Tracker announce URL.
    url: String,
    /// Current status.
    status: TrackerStatus,
    /// Re-announce interval (from tracker or default).
    interval: Duration,
    /// When we last announced.
    last_announce: Option<Instant>,
    /// When the next announce is due.
    next_announce: Instant,
    /// Consecutive failure count.
    failures: u32,
    /// Current backoff duration.
    backoff: Duration,
    /// Most recent peers received.
    cached_peers: Vec<CompactPeer>,
}

impl TrackerState {
    /// Creates a new tracker state. First announce is due immediately.
    pub fn new(url: String, now: Instant) -> Self {
        Self {
            url,
            status: TrackerStatus::Active,
            interval: DEFAULT_ANNOUNCE_INTERVAL,
            last_announce: None,
            next_announce: now, // Due immediately.
            failures: 0,
            backoff: INITIAL_BACKOFF,
            cached_peers: Vec::new(),
        }
    }

    /// Returns whether an announce is due.
    pub fn should_announce(&self, now: Instant) -> bool {
        now >= self.next_announce && self.status != TrackerStatus::Dead
    }

    /// Records a successful announce response.
    pub fn record_success(&mut self, response: &AnnounceResponse, now: Instant) {
        self.failures = 0;
        self.backoff = INITIAL_BACKOFF;
        self.status = TrackerStatus::Active;
        self.last_announce = Some(now);

        // Use tracker-provided interval, clamped to minimum.
        let effective_interval = if response.interval() >= MIN_ANNOUNCE_INTERVAL {
            response.interval()
        } else {
            MIN_ANNOUNCE_INTERVAL
        };
        self.interval = effective_interval;
        self.next_announce = now + effective_interval;
        self.cached_peers = response.peers().to_vec();
    }

    /// Records a failed announce attempt.
    pub fn record_failure(&mut self, now: Instant) {
        self.failures = self.failures.saturating_add(1);
        self.last_announce = Some(now);

        if self.failures >= MAX_TRACKER_FAILURES {
            self.status = TrackerStatus::Dead;
        } else {
            self.status = TrackerStatus::Degraded;
        }

        // Exponential backoff capped at MAX_BACKOFF.
        let backoff_secs = self
            .backoff
            .as_secs()
            .saturating_mul(BACKOFF_MULTIPLIER as u64)
            .min(MAX_BACKOFF.as_secs());
        self.backoff = Duration::from_secs(backoff_secs);
        self.next_announce = now + self.backoff;
    }

    /// Returns the tracker's announce URL.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Returns the current status.
    pub fn status(&self) -> TrackerStatus {
        self.status
    }

    /// Returns consecutive failures.
    pub fn failures(&self) -> u32 {
        self.failures
    }

    /// Returns cached peers from the last successful announce.
    pub fn cached_peers(&self) -> &[CompactPeer] {
        &self.cached_peers
    }

    /// Returns the current announce interval.
    pub fn interval(&self) -> Duration {
        self.interval
    }
}

// ── Multi-tracker manager ───────────────────────────────────────────

/// Error conditions for the tracker tier.
#[derive(Debug, thiserror::Error)]
pub enum TrackerError {
    /// No trackers configured.
    #[error("no trackers configured")]
    NoTrackers,
    /// All trackers are dead.
    #[error("all {count} trackers are dead")]
    AllDead {
        /// Number of dead trackers.
        count: usize,
    },
}

/// Manages multiple trackers with tiered announce scheduling.
///
/// BEP 12 defines tracker tiers: announce to one tracker per tier,
/// cycling through alternatives on failure. This implementation
/// tracks all trackers in a flat list with independent state machines.
pub struct TrackerTier {
    /// Tracked trackers.
    trackers: Vec<TrackerState>,
}

impl TrackerTier {
    /// Creates a tracker tier from announce URLs.
    pub fn new(urls: &[&str], now: Instant) -> Self {
        Self {
            trackers: urls
                .iter()
                .map(|u| TrackerState::new((*u).to_string(), now))
                .collect(),
        }
    }

    /// Returns trackers that should announce now.
    pub fn due_announces(&self, now: Instant) -> Vec<&TrackerState> {
        self.trackers
            .iter()
            .filter(|t| t.should_announce(now))
            .collect()
    }

    /// Returns all active (non-dead) trackers.
    pub fn active_trackers(&self) -> Vec<&TrackerState> {
        self.trackers
            .iter()
            .filter(|t| t.status() != TrackerStatus::Dead)
            .collect()
    }

    /// Returns all peers from all trackers (deduplicated by IP:port).
    pub fn all_peers(&self) -> Vec<CompactPeer> {
        let mut seen = std::collections::HashSet::new();
        let mut result = Vec::new();

        for tracker in &self.trackers {
            for peer in tracker.cached_peers() {
                let key = (peer.ip(), peer.port());
                if seen.insert(key) {
                    result.push(*peer);
                }
            }
        }
        result
    }

    /// Records a successful announce for a tracker URL.
    pub fn record_success(&mut self, url: &str, response: &AnnounceResponse, now: Instant) {
        if let Some(t) = self.trackers.iter_mut().find(|t| t.url == url) {
            t.record_success(response, now);
        }
    }

    /// Records a failed announce for a tracker URL.
    pub fn record_failure(&mut self, url: &str, now: Instant) {
        if let Some(t) = self.trackers.iter_mut().find(|t| t.url == url) {
            t.record_failure(now);
        }
    }

    /// Returns the number of trackers.
    pub fn tracker_count(&self) -> usize {
        self.trackers.len()
    }

    /// Returns the number of active trackers.
    pub fn active_count(&self) -> usize {
        self.trackers
            .iter()
            .filter(|t| t.status() != TrackerStatus::Dead)
            .count()
    }

    /// Returns a tracker state by URL.
    pub fn get(&self, url: &str) -> Option<&TrackerState> {
        self.trackers.iter().find(|t| t.url == url)
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── CompactPeer ─────────────────────────────────────────────────

    /// Compact peer round-trips through bytes.
    ///
    /// BEP 23 compact encoding must be lossless: parse → serialise →
    /// parse produces the same peer.
    #[test]
    fn compact_peer_round_trip() {
        let peer = CompactPeer {
            ip: [192, 168, 1, 1],
            port: 6881,
        };
        let bytes = peer.to_bytes();
        let parsed = CompactPeer::from_bytes(&bytes).unwrap();
        assert_eq!(peer, parsed);
    }

    /// Compact peer rejects short input.
    ///
    /// Malformed tracker responses must not panic.
    #[test]
    fn compact_peer_rejects_short() {
        assert!(CompactPeer::from_bytes(&[1, 2, 3]).is_none());
    }

    /// IP string formatting.
    ///
    /// Human-readable representation for logging and diagnostics.
    #[test]
    fn compact_peer_ip_string() {
        let peer = CompactPeer {
            ip: [10, 0, 0, 1],
            port: 8080,
        };
        assert_eq!(peer.ip_string(), "10.0.0.1");
    }

    /// Parse compact peer list from bulk data.
    ///
    /// Tracker responses contain a raw byte blob of concatenated peers.
    #[test]
    fn parse_compact_peers_multiple() {
        let mut data = Vec::new();
        // Peer 1: 192.168.1.1:6881
        data.extend_from_slice(&[192, 168, 1, 1, 0x1A, 0xE1]);
        // Peer 2: 10.0.0.2:8080
        data.extend_from_slice(&[10, 0, 0, 2, 0x1F, 0x90]);

        let peers = parse_compact_peers(&data);
        assert_eq!(peers.len(), 2);
        assert_eq!(peers[0].ip(), [192, 168, 1, 1]);
        assert_eq!(peers[0].port(), 6881);
        assert_eq!(peers[1].ip(), [10, 0, 0, 2]);
        assert_eq!(peers[1].port(), 8080);
    }

    /// Trailing bytes ignored in compact peer parsing.
    ///
    /// If the data length isn't a multiple of 6, trailing bytes are
    /// silently discarded rather than causing a parse error.
    #[test]
    fn parse_compact_peers_trailing_bytes() {
        let data = vec![192, 168, 1, 1, 0x1A, 0xE1, 0xFF, 0xFF];
        let peers = parse_compact_peers(&data);
        assert_eq!(peers.len(), 1);
    }

    // ── AnnounceRequest ─────────────────────────────────────────────

    /// Default request uses compact encoding and standard numwant.
    ///
    /// BEP 23 compact encoding is always preferred for bandwidth
    /// efficiency.
    #[test]
    fn announce_request_defaults() {
        let req = AnnounceRequest::new([0xAA; 20], [0xBB; 20], 6881);
        assert!(req.compact());
        assert_eq!(req.numwant(), DEFAULT_NUMWANT);
        assert!(req.event().is_none());
    }

    /// Builder methods set fields correctly.
    ///
    /// The fluent builder API must install values without side effects.
    #[test]
    fn announce_request_builder() {
        let req = AnnounceRequest::new([0xAA; 20], [0xBB; 20], 6881)
            .with_event(TrackerEvent::Started)
            .with_stats(1000, 500, 2000)
            .with_numwant(100);

        assert_eq!(req.event(), Some(TrackerEvent::Started));
        assert_eq!(req.numwant(), 100);
    }

    // ── AnnounceResponse ────────────────────────────────────────────

    /// Response with stats and warning.
    ///
    /// All optional fields must be preserved.
    #[test]
    fn announce_response_with_metadata() {
        let resp = AnnounceResponse::new(Duration::from_secs(900), vec![])
            .with_stats(10, 5)
            .with_warning("Please don't hammer the tracker".into());

        assert_eq!(resp.interval(), Duration::from_secs(900));
        assert_eq!(resp.complete(), Some(10));
        assert_eq!(resp.incomplete(), Some(5));
        assert!(resp.warning().unwrap().contains("hammer"));
    }

    // ── ScrapeResponse ──────────────────────────────────────────────

    /// Active swarm detection.
    ///
    /// A swarm with any seeders or leechers is considered active.
    #[test]
    fn scrape_active() {
        let scrape = ScrapeResponse {
            complete: 5,
            incomplete: 3,
            downloaded: 100,
        };
        assert!(scrape.is_active());
        assert_eq!(scrape.total_peers(), 8);
    }

    /// Empty swarm.
    ///
    /// Zero seeders and leechers means no one is in the swarm.
    #[test]
    fn scrape_inactive() {
        let scrape = ScrapeResponse {
            complete: 0,
            incomplete: 0,
            downloaded: 50,
        };
        assert!(!scrape.is_active());
    }

    // ── TrackerState ────────────────────────────────────────────────

    /// New tracker should announce immediately.
    ///
    /// First announce is always due at construction time.
    #[test]
    fn new_tracker_announces_immediately() {
        let now = Instant::now();
        let state = TrackerState::new("http://tracker.example.com/announce".into(), now);
        assert!(state.should_announce(now));
        assert_eq!(state.status(), TrackerStatus::Active);
    }

    /// Successful announce schedules next at interval.
    ///
    /// The tracker-provided interval governs re-announce timing.
    #[test]
    fn success_schedules_next() {
        let now = Instant::now();
        let mut state = TrackerState::new("http://t.example.com/a".into(), now);

        let resp = AnnounceResponse::new(Duration::from_secs(600), vec![]);
        state.record_success(&resp, now);

        assert!(!state.should_announce(now));
        assert_eq!(state.interval(), Duration::from_secs(600));

        let later = now + Duration::from_secs(601);
        assert!(state.should_announce(later));
    }

    /// Failures trigger exponential backoff and eventual death.
    ///
    /// Repeated failures must increase the backoff geometrically and
    /// eventually mark the tracker as dead.
    #[test]
    fn failures_trigger_backoff_and_death() {
        let now = Instant::now();
        let mut state = TrackerState::new("http://t.example.com/a".into(), now);

        for i in 0..MAX_TRACKER_FAILURES {
            state.record_failure(now);
            if i < MAX_TRACKER_FAILURES - 1 {
                assert_eq!(state.status(), TrackerStatus::Degraded);
            }
        }
        assert_eq!(state.status(), TrackerStatus::Dead);
        assert!(!state.should_announce(now));
    }

    /// Success resets failure state.
    ///
    /// A tracker that recovers should reset its backoff and status.
    #[test]
    fn success_resets_failures() {
        let now = Instant::now();
        let mut state = TrackerState::new("http://t.example.com/a".into(), now);

        state.record_failure(now);
        state.record_failure(now);
        assert_eq!(state.status(), TrackerStatus::Degraded);

        let resp = AnnounceResponse::new(Duration::from_secs(900), vec![]);
        state.record_success(&resp, now);
        assert_eq!(state.status(), TrackerStatus::Active);
        assert_eq!(state.failures(), 0);
    }

    // ── TrackerTier ─────────────────────────────────────────────────

    /// All peers deduplicated across trackers.
    ///
    /// When multiple trackers return the same peer, the tier should
    /// deduplicate by IP:port.
    #[test]
    fn tier_deduplicates_peers() {
        let now = Instant::now();
        let mut tier =
            TrackerTier::new(&["http://t1.example.com/a", "http://t2.example.com/a"], now);

        let peer = CompactPeer {
            ip: [1, 2, 3, 4],
            port: 6881,
        };
        let resp = AnnounceResponse::new(Duration::from_secs(900), vec![peer]);

        tier.record_success("http://t1.example.com/a", &resp, now);
        tier.record_success("http://t2.example.com/a", &resp, now);

        let all = tier.all_peers();
        assert_eq!(
            all.len(),
            1,
            "same peer from two trackers should deduplicate"
        );
    }

    /// Active count decreases as trackers die.
    ///
    /// Dead trackers should not be counted as active.
    #[test]
    fn tier_active_count() {
        let now = Instant::now();
        let mut tier =
            TrackerTier::new(&["http://t1.example.com/a", "http://t2.example.com/a"], now);

        assert_eq!(tier.active_count(), 2);

        // Kill tracker 1.
        for _ in 0..MAX_TRACKER_FAILURES {
            tier.record_failure("http://t1.example.com/a", now);
        }
        assert_eq!(tier.active_count(), 1);
    }
}
