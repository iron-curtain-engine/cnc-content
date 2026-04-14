// SPDX-License-Identifier: MIT OR Apache-2.0

//! BEP 14 Local Peer Discovery (LPD) — multicast-based LAN peer finding.
//!
//! ## What
//!
//! Implements Local Peer Discovery for finding peers on the same network
//! segment without contacting external trackers or DHT. Peers announce
//! themselves via UDP multicast and discover each other within seconds.
//!
//! ## Why — LAN parties and local networks
//!
//! C&C games have a strong LAN party tradition. Red Alert's multiplayer
//! was born on IPX/LAN, and CnCNet originally worked by tunnelling LAN
//! traffic over the internet. Local peer discovery is critical for:
//!
//! - **LAN party content distribution.** When 20 players at a LAN party
//!   all need to install game content, one seeder + LPD means content
//!   spreads across the LAN at gigabit speed without touching the internet.
//! - **Air-gapped networks.** Military, educational, or corporate networks
//!   that block external traffic can still distribute content via LPD.
//! - **Instant bootstrap.** LPD announces arrive in milliseconds (single
//!   multicast hop), far faster than tracker HTTP round-trips or DHT
//!   iterative lookups.
//! - **Zero configuration.** No tracker URLs, no DHT bootstrap nodes —
//!   just join the multicast group and start discovering.
//!
//! ## How — BEP 14 protocol
//!
//! BEP 14 uses UDP multicast to `239.192.152.143:6771`. Each announce is
//! an HTTP-like message containing the info hash and listen port. Peers
//! that share the same info hash connect directly.
//!
//! - [`LpdAnnounce`]: A parsed LPD announcement.
//! - [`LpdService`]: Manages announce sending and received peer tracking.
//! - [`LpdPeer`]: A peer discovered via LPD.

use std::time::{Duration, Instant};

// ── Constants ───────────────────────────────────────────────────────

/// BEP 14 multicast group address (IPv4).
pub const LPD_MULTICAST_ADDR: &str = "239.192.152.143";

/// BEP 14 multicast port.
pub const LPD_PORT: u16 = 6771;

/// BEP 14 re-announce interval.
///
/// BEP 14 specifies 5 minutes between announces to avoid multicast
/// flooding on large LANs.
const ANNOUNCE_INTERVAL: Duration = Duration::from_secs(300);

/// Maximum peers tracked per info hash via LPD.
const MAX_LPD_PEERS: usize = 50;

/// Header line identifying a BEP 14 announce message.
const BEP14_HEADER: &str = "BT-SEARCH * HTTP/1.1";

/// Maximum age before an LPD peer is considered stale.
const PEER_STALE_TIMEOUT: Duration = Duration::from_secs(600);

// ── LPD announce message ────────────────────────────────────────────

/// A parsed BEP 14 Local Peer Discovery announcement.
///
/// BEP 14 messages look like HTTP headers:
/// ```text
/// BT-SEARCH * HTTP/1.1\r\n
/// Host: 239.192.152.143:6771\r\n
/// Port: 6881\r\n
/// Infohash: <40-char hex>\r\n
/// \r\n
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LpdAnnounce {
    /// Info hash being announced (20 bytes).
    info_hash: [u8; 20],
    /// Listen port of the announcing peer.
    port: u16,
}

impl LpdAnnounce {
    /// Creates a new LPD announcement.
    pub fn new(info_hash: [u8; 20], port: u16) -> Self {
        Self { info_hash, port }
    }

    /// Serialises the announcement into the BEP 14 wire format.
    ///
    /// ```
    /// use p2p_distribute::local_discovery::LpdAnnounce;
    ///
    /// let announce = LpdAnnounce::new([0xAB; 20], 6881);
    /// let bytes = announce.to_bytes();
    /// let text = std::str::from_utf8(&bytes).unwrap();
    /// assert!(text.starts_with("BT-SEARCH * HTTP/1.1\r\n"));
    /// assert!(text.contains("Port: 6881"));
    /// ```
    pub fn to_bytes(&self) -> Vec<u8> {
        let hex_hash = crate::hex_encode(&self.info_hash);
        format!(
            "{}\r\nHost: {}:{}\r\nPort: {}\r\nInfohash: {}\r\n\r\n",
            BEP14_HEADER, LPD_MULTICAST_ADDR, LPD_PORT, self.port, hex_hash,
        )
        .into_bytes()
    }

    /// Parses an LPD announcement from raw bytes.
    ///
    /// Returns `None` if the message is malformed or missing required fields.
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        let text = std::str::from_utf8(data).ok()?;

        // Verify header line.
        if !text.starts_with(BEP14_HEADER) {
            return None;
        }

        let mut port: Option<u16> = None;
        let mut info_hash: Option<[u8; 20]> = None;

        for line in text.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("Port:") {
                port = rest.trim().parse().ok();
            } else if let Some(rest) = line.strip_prefix("Infohash:") {
                let hex = rest.trim();
                info_hash = parse_hex_hash(hex);
            }
        }

        Some(Self {
            info_hash: info_hash?,
            port: port?,
        })
    }

    /// Returns the info hash.
    pub fn info_hash(&self) -> &[u8; 20] {
        &self.info_hash
    }

    /// Returns the listen port.
    pub fn port(&self) -> u16 {
        self.port
    }
}

/// Parses a 40-character hex string into 20 bytes.
fn parse_hex_hash(hex: &str) -> Option<[u8; 20]> {
    if hex.len() != 40 {
        return None;
    }
    let mut result = [0u8; 20];
    for (i, byte) in result.iter_mut().enumerate() {
        let hi = hex.as_bytes().get(i.saturating_mul(2))?;
        let lo = hex.as_bytes().get(i.saturating_mul(2).saturating_add(1))?;
        *byte = (hex_digit(*hi)? << 4) | hex_digit(*lo)?;
    }
    Some(result)
}

/// Converts an ASCII hex digit to its numeric value.
fn hex_digit(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c.saturating_sub(b'0')),
        b'a'..=b'f' => Some(c.saturating_sub(b'a').saturating_add(10)),
        b'A'..=b'F' => Some(c.saturating_sub(b'A').saturating_add(10)),
        _ => None,
    }
}

// ── LPD peer ────────────────────────────────────────────────────────

/// A peer discovered via Local Peer Discovery.
#[derive(Debug, Clone)]
pub struct LpdPeer {
    /// Source IP address (from the UDP datagram).
    ip: String,
    /// Listen port (from the announce message).
    port: u16,
    /// When this peer was first discovered.
    discovered_at: Instant,
    /// When we last received an announce from this peer.
    last_seen: Instant,
}

impl LpdPeer {
    /// Creates a new LPD peer record.
    pub fn new(ip: String, port: u16, now: Instant) -> Self {
        Self {
            ip,
            port,
            discovered_at: now,
            last_seen: now,
        }
    }

    /// Returns the peer's IP address.
    pub fn ip(&self) -> &str {
        &self.ip
    }

    /// Returns the peer's listen port.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Returns when this peer was first discovered.
    pub fn discovered_at(&self) -> Instant {
        self.discovered_at
    }

    /// Returns when we last heard from this peer.
    pub fn last_seen(&self) -> Instant {
        self.last_seen
    }

    /// Returns whether this peer is stale.
    pub fn is_stale(&self, now: Instant) -> bool {
        now.duration_since(self.last_seen) > PEER_STALE_TIMEOUT
    }

    /// Updates the last-seen timestamp.
    pub fn refresh(&mut self, now: Instant) {
        self.last_seen = now;
    }
}

// ── LPD service ─────────────────────────────────────────────────────

/// Manages LPD announcements and discovered peers for a set of info hashes.
///
/// The service tracks which info hashes we're interested in, accumulates
/// discovered peers, and manages announce timing.
pub struct LpdService {
    /// Info hashes we're announcing and listening for.
    subscriptions: Vec<LpdSubscription>,
}

/// A single info hash subscription with its peer list and timing.
#[derive(Debug)]
struct LpdSubscription {
    /// The info hash being tracked.
    info_hash: [u8; 20],
    /// Our listen port for this torrent.
    port: u16,
    /// When we last sent an announce.
    last_announce: Option<Instant>,
    /// Discovered peers for this info hash.
    peers: Vec<LpdPeer>,
}

impl LpdService {
    /// Creates a new LPD service with no subscriptions.
    pub fn new() -> Self {
        Self {
            subscriptions: Vec::new(),
        }
    }

    /// Subscribes to announcements for an info hash.
    pub fn subscribe(&mut self, info_hash: [u8; 20], port: u16) {
        if self.subscriptions.iter().any(|s| s.info_hash == info_hash) {
            return; // Already subscribed.
        }
        self.subscriptions.push(LpdSubscription {
            info_hash,
            port,
            last_announce: None,
            peers: Vec::new(),
        });
    }

    /// Unsubscribes from an info hash.
    pub fn unsubscribe(&mut self, info_hash: &[u8; 20]) {
        self.subscriptions.retain(|s| &s.info_hash != info_hash);
    }

    /// Returns announce messages that are due to be sent.
    pub fn pending_announces(&self, now: Instant) -> Vec<LpdAnnounce> {
        self.subscriptions
            .iter()
            .filter(|s| match s.last_announce {
                Some(last) => now.duration_since(last) >= ANNOUNCE_INTERVAL,
                None => true, // Never announced → due immediately.
            })
            .map(|s| LpdAnnounce::new(s.info_hash, s.port))
            .collect()
    }

    /// Records that announces were sent.
    pub fn mark_announced(&mut self, info_hash: &[u8; 20], now: Instant) {
        if let Some(sub) = self
            .subscriptions
            .iter_mut()
            .find(|s| &s.info_hash == info_hash)
        {
            sub.last_announce = Some(now);
        }
    }

    /// Processes a received LPD announcement.
    ///
    /// If we're subscribed to the announced info hash and the peer is
    /// new, it's added to the peer list.
    pub fn handle_announce(&mut self, announce: &LpdAnnounce, source_ip: &str, now: Instant) {
        let sub = match self
            .subscriptions
            .iter_mut()
            .find(|s| s.info_hash == *announce.info_hash())
        {
            Some(s) => s,
            None => return, // Not subscribed to this info hash.
        };

        // Check for existing peer (same IP + port).
        if let Some(existing) = sub
            .peers
            .iter_mut()
            .find(|p| p.ip == source_ip && p.port == announce.port())
        {
            existing.refresh(now);
            return;
        }

        // Cap peer list size.
        if sub.peers.len() >= MAX_LPD_PEERS {
            return;
        }

        sub.peers
            .push(LpdPeer::new(source_ip.to_string(), announce.port(), now));
    }

    /// Returns discovered peers for an info hash.
    pub fn peers_for(&self, info_hash: &[u8; 20]) -> Vec<&LpdPeer> {
        self.subscriptions
            .iter()
            .find(|s| &s.info_hash == info_hash)
            .map(|s| s.peers.iter().collect())
            .unwrap_or_default()
    }

    /// Prunes stale peers from all subscriptions.
    pub fn prune_stale(&mut self, now: Instant) -> usize {
        let mut total: usize = 0;
        for sub in &mut self.subscriptions {
            let before = sub.peers.len();
            sub.peers.retain(|p| !p.is_stale(now));
            total = total.saturating_add(before.saturating_sub(sub.peers.len()));
        }
        total
    }

    /// Returns the number of active subscriptions.
    pub fn subscription_count(&self) -> usize {
        self.subscriptions.len()
    }

    /// Returns total discovered peers across all subscriptions.
    pub fn total_peer_count(&self) -> usize {
        self.subscriptions.iter().map(|s| s.peers.len()).sum()
    }
}

impl Default for LpdService {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── LpdAnnounce serialisation ───────────────────────────────────

    /// Announce round-trips through serialisation.
    ///
    /// BEP 14 wire format must be parseable by our own parser.
    #[test]
    fn announce_round_trip() {
        let original = LpdAnnounce::new([0xAB; 20], 6881);
        let bytes = original.to_bytes();
        let parsed = LpdAnnounce::from_bytes(&bytes).unwrap();
        assert_eq!(original, parsed);
    }

    /// Announce contains required fields.
    ///
    /// The wire format must include the BEP 14 header, port, and info hash.
    #[test]
    fn announce_format() {
        let announce = LpdAnnounce::new([0x42; 20], 51234);
        let text = String::from_utf8(announce.to_bytes()).unwrap();

        assert!(text.starts_with("BT-SEARCH * HTTP/1.1\r\n"));
        assert!(text.contains("Port: 51234"));
        assert!(text.contains("Infohash: "));
        assert!(text.ends_with("\r\n\r\n"));
    }

    /// Rejects malformed announce.
    ///
    /// Missing header should return None.
    #[test]
    fn rejects_malformed() {
        assert!(LpdAnnounce::from_bytes(b"GET / HTTP/1.1\r\n").is_none());
    }

    /// Rejects announce missing port.
    ///
    /// Port is a required field.
    #[test]
    fn rejects_missing_port() {
        let msg = format!(
            "{}\r\nHost: {}:{}\r\nInfohash: {}\r\n\r\n",
            BEP14_HEADER,
            LPD_MULTICAST_ADDR,
            LPD_PORT,
            "aa".repeat(20),
        );
        assert!(LpdAnnounce::from_bytes(msg.as_bytes()).is_none());
    }

    // ── Hex parsing ─────────────────────────────────────────────────

    /// Hex hash parsing for valid input.
    ///
    /// 40 hex characters should produce 20 bytes.
    #[test]
    fn hex_hash_valid() {
        let hex = "abcdef0123456789abcdef0123456789abcdef01";
        let result = parse_hex_hash(hex);
        assert!(result.is_some());
        assert_eq!(result.unwrap()[0], 0xAB);
    }

    /// Hex hash rejects wrong length.
    ///
    /// Anything other than exactly 40 characters is invalid.
    #[test]
    fn hex_hash_wrong_length() {
        assert!(parse_hex_hash("abcdef").is_none());
        assert!(parse_hex_hash(&"ab".repeat(21)).is_none());
    }

    // ── LpdService ──────────────────────────────────────────────────

    /// Subscribe and discover peers.
    ///
    /// The core LPD flow: subscribe → receive announce → get peers.
    #[test]
    fn subscribe_and_discover() {
        let now = Instant::now();
        let mut service = LpdService::new();
        let hash = [0x42; 20];

        service.subscribe(hash, 6881);
        assert_eq!(service.subscription_count(), 1);

        // Receive an announce from another peer.
        let announce = LpdAnnounce::new(hash, 51234);
        service.handle_announce(&announce, "192.168.1.100", now);

        let peers = service.peers_for(&hash);
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].ip(), "192.168.1.100");
        assert_eq!(peers[0].port(), 51234);
    }

    /// Duplicate peers are refreshed, not duplicated.
    ///
    /// Re-announces from the same peer should update last_seen, not
    /// add a new entry.
    #[test]
    fn duplicate_peer_refreshed() {
        let now = Instant::now();
        let mut service = LpdService::new();
        let hash = [0x42; 20];

        service.subscribe(hash, 6881);

        let announce = LpdAnnounce::new(hash, 51234);
        service.handle_announce(&announce, "192.168.1.100", now);
        service.handle_announce(&announce, "192.168.1.100", now);

        assert_eq!(service.peers_for(&hash).len(), 1);
    }

    /// Unsubscribed info hashes are ignored.
    ///
    /// Announces for torrents we're not tracking should be silently dropped.
    #[test]
    fn ignores_unsubscribed() {
        let now = Instant::now();
        let mut service = LpdService::new();

        let announce = LpdAnnounce::new([0xFF; 20], 6881);
        service.handle_announce(&announce, "192.168.1.100", now);

        assert_eq!(service.total_peer_count(), 0);
    }

    /// Pending announces for new subscriptions.
    ///
    /// Newly subscribed info hashes should have announces due immediately.
    #[test]
    fn pending_announces_due_immediately() {
        let now = Instant::now();
        let mut service = LpdService::new();
        service.subscribe([0x42; 20], 6881);

        let pending = service.pending_announces(now);
        assert_eq!(pending.len(), 1);
    }

    /// No pending announces after recent send.
    ///
    /// After marking as announced, the interval timer must elapse before
    /// the next announce is due.
    #[test]
    fn no_pending_after_recent_announce() {
        let now = Instant::now();
        let hash = [0x42; 20];
        let mut service = LpdService::new();
        service.subscribe(hash, 6881);

        service.mark_announced(&hash, now);
        let pending = service.pending_announces(now);
        assert!(pending.is_empty());

        // After interval elapses.
        let later = now + ANNOUNCE_INTERVAL;
        let pending = service.pending_announces(later);
        assert_eq!(pending.len(), 1);
    }

    /// Prune stale peers.
    ///
    /// Peers that haven't re-announced within the timeout should be removed.
    #[test]
    fn prune_stale_peers() {
        let now = Instant::now();
        let mut service = LpdService::new();
        let hash = [0x42; 20];
        service.subscribe(hash, 6881);

        let announce = LpdAnnounce::new(hash, 51234);
        service.handle_announce(&announce, "192.168.1.100", now);
        assert_eq!(service.total_peer_count(), 1);

        let later = now + PEER_STALE_TIMEOUT + Duration::from_secs(1);
        let pruned = service.prune_stale(later);
        assert_eq!(pruned, 1);
        assert_eq!(service.total_peer_count(), 0);
    }

    /// Unsubscribe removes subscription and its peers.
    ///
    /// Explicit unsubscribe should free all tracking state.
    #[test]
    fn unsubscribe_clears() {
        let now = Instant::now();
        let hash = [0x42; 20];
        let mut service = LpdService::new();
        service.subscribe(hash, 6881);

        let announce = LpdAnnounce::new(hash, 51234);
        service.handle_announce(&announce, "192.168.1.100", now);

        service.unsubscribe(&hash);
        assert_eq!(service.subscription_count(), 0);
        assert_eq!(service.total_peer_count(), 0);
    }

    /// Peer cap prevents unbounded growth.
    ///
    /// On a large LAN, many peers may announce. The cap prevents
    /// excessive memory usage.
    #[test]
    fn peer_cap_enforced() {
        let now = Instant::now();
        let hash = [0x42; 20];
        let mut service = LpdService::new();
        service.subscribe(hash, 6881);

        for i in 0..(MAX_LPD_PEERS + 10) {
            let announce = LpdAnnounce::new(hash, 50000 + i as u16);
            service.handle_announce(&announce, &format!("192.168.1.{}", i % 256), now);
        }

        assert!(service.total_peer_count() <= MAX_LPD_PEERS);
    }
}
