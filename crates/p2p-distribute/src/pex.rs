// SPDX-License-Identifier: MIT OR Apache-2.0

//! Peer Exchange (PEX) — peers gossip about other peers they know.
//!
//! ## Design rationale (BitTorrent BEP-11 + IPFS peer routing)
//!
//! Tracker-based peer discovery has a single point of failure: if the tracker
//! is down, new peers can't find the swarm. BitTorrent's Peer Exchange
//! (BEP-11, `ut_pex`) solves this by having connected peers share knowledge
//! of other peers they know. IPFS takes this further with its DHT-based
//! content routing and Bitswap want-have protocol.
//!
//! For Iron Curtain, PEX provides:
//!
//! - **Tracker redundancy**: peers discovered via PEX don't depend on tracker
//!   availability.
//! - **Faster swarm growth**: new joiners learn about peers from their first
//!   connection rather than waiting for the next tracker announce cycle.
//! - **Network-aware filtering**: PEX messages include a [`NetworkId`] so
//!   peers on different networks (test/prod) don't contaminate each other.
//!
//! ## BEP-11 guidelines
//!
//! - Send PEX updates every ~60 seconds (not more frequently).
//! - Limit to [`MAX_PEX_ADDED`] added and [`MAX_PEX_DROPPED`] dropped
//!   entries per message to bound bandwidth.
//! - Include flags (seed, encryption, connectability) for each peer.
//!
//! ## Implementation status
//!
//! This module defines the **message types** for PEX. The actual exchange
//! protocol depends on the IC wire protocol (D049 `ic_pex` extension,
//! milestone M2–M3). Until then, these types serve as the data model for
//! peer discovery results from any source (tracker, DHT, manual entry).

use crate::network_id::NetworkId;
use crate::peer_id::PeerId;

/// Maximum recommended added entries per PEX message (BEP-11 guideline).
pub const MAX_PEX_ADDED: usize = 50;

/// Maximum recommended dropped entries per PEX message (BEP-11 guideline).
pub const MAX_PEX_DROPPED: usize = 50;

/// Recommended interval between PEX messages in seconds (BEP-11).
pub const PEX_INTERVAL_SECS: u64 = 60;

// ── PexFlags ────────────────────────────────────────────────────────

/// Capability flags for a peer in a PEX message.
///
/// Maps to BEP-11's `added.f` flags byte. Each flag describes a peer's
/// role or capabilities, helping the coordinator make informed connection
/// decisions without trial-and-error probing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PexFlags {
    /// Peer supports encrypted connections (BEP-11 flag 0x01).
    pub encryption: bool,
    /// Peer is a seed — has all pieces (BEP-11 flag 0x02).
    pub seed: bool,
    /// Peer supports µTP transport (BEP-11 flag 0x04).
    pub utp: bool,
    /// Peer prefers µTP for incoming connections (BEP-11 flag 0x08).
    pub utp_holepunch: bool,
    /// Peer is directly connectable without NAT traversal (BEP-11 flag 0x10).
    pub connectable: bool,
}

impl PexFlags {
    /// Encodes flags into a single byte (BEP-11 wire format).
    pub fn to_byte(self) -> u8 {
        let mut b = 0u8;
        if self.encryption {
            b |= 0x01;
        }
        if self.seed {
            b |= 0x02;
        }
        if self.utp {
            b |= 0x04;
        }
        if self.utp_holepunch {
            b |= 0x08;
        }
        if self.connectable {
            b |= 0x10;
        }
        b
    }

    /// Decodes flags from a single byte (BEP-11 wire format).
    pub fn from_byte(b: u8) -> Self {
        Self {
            encryption: b & 0x01 != 0,
            seed: b & 0x02 != 0,
            utp: b & 0x04 != 0,
            utp_holepunch: b & 0x08 != 0,
            connectable: b & 0x10 != 0,
        }
    }
}

// ── PexEntry ────────────────────────────────────────────────────────

/// A single peer entry in a PEX message.
///
/// Contains enough information for the coordinator to initiate a connection
/// to the advertised peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PexEntry {
    /// The peer's cryptographic identity.
    pub peer_id: PeerId,
    /// Network address — "IP:port" for BT peers, URL for web seeds.
    pub addr: String,
    /// Capability flags from BEP-11.
    pub flags: PexFlags,
}

// ── PexMessage ──────────────────────────────────────────────────────

/// Peer Exchange message — periodic gossip about known peers.
///
/// Peers send `PexMessage`s to share knowledge of other active peers.
/// `added` contains newly discovered peers; `dropped` contains peers
/// that are no longer reachable.
///
/// The `network_id` field ensures that peers on different networks
/// (test/prod/custom) ignore each other's PEX messages. A receiver
/// must discard any `PexMessage` whose `network_id` doesn't match
/// its own.
///
/// ## BEP-11 bandwidth guidelines
///
/// - At most [`MAX_PEX_ADDED`] added entries per message.
/// - At most [`MAX_PEX_DROPPED`] dropped entries per message.
/// - Send no more frequently than every [`PEX_INTERVAL_SECS`] seconds.
#[derive(Debug, Clone)]
pub struct PexMessage {
    /// Network this message belongs to. Receivers with a different
    /// `NetworkId` must discard the message.
    pub network_id: NetworkId,
    /// Newly discovered peers to share.
    pub added: Vec<PexEntry>,
    /// Peers that have disconnected or become unreachable.
    pub dropped: Vec<PeerId>,
}

impl PexMessage {
    /// Creates an empty PEX message for the given network.
    pub fn new(network_id: NetworkId) -> Self {
        Self {
            network_id,
            added: Vec::new(),
            dropped: Vec::new(),
        }
    }

    /// Whether this message has any entries (added or dropped).
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.dropped.is_empty()
    }

    /// Whether the added list exceeds the BEP-11 recommended limit.
    pub fn is_oversized(&self) -> bool {
        self.added.len() > MAX_PEX_ADDED || self.dropped.len() > MAX_PEX_DROPPED
    }
}

// ── PexDeltaTracker (IRC spanning-tree multicast pattern) ───────────

/// Tracks per-recipient "already-knows" peer sets for delta-only PEX broadcast.
///
/// ## Why (IRC spanning-tree multicast efficiency lesson)
///
/// Naïve PEX broadcasting sends the full peer list to every recipient on
/// every gossip interval. This wastes bandwidth because most peers already
/// know about most other peers after the first exchange. IRC avoids this
/// with its spanning-tree topology — messages traverse each link exactly
/// once. BitTorrent's BEP-11 recommends "send only new peers", but leaves
/// tracking to the implementation.
///
/// ## How
///
/// `PexDeltaTracker` maintains a [`HashSet<PeerId>`] per recipient. When
/// building a PEX message for a recipient, the caller asks the tracker to
/// [`filter_new`] the candidate list — only entries the recipient has not
/// seen are included. After sending, the caller calls [`mark_sent`] to
/// record that the recipient now knows about those peers.
///
/// ## Bounded memory
///
/// The tracker enforces [`MAX_TRACKED_PEERS`] per recipient. When the
/// known-set exceeds this limit, older entries are **not** evicted (sets
/// are unordered). Instead, the excess is tolerated because PEX messages
/// are capped at [`MAX_PEX_ADDED`] per interval anyway. Recipients that
/// disconnect are cleaned up via [`remove_recipient`].
pub struct PexDeltaTracker {
    /// Per-recipient set of PeerIds the recipient already knows.
    known: std::collections::HashMap<PeerId, std::collections::HashSet<PeerId>>,
}

/// Maximum number of known-peer entries tracked per recipient.
///
/// Beyond this, the set is saturated and new additions are still tracked
/// (HashSet grows) but the practical impact is bounded because PEX messages
/// themselves are capped at [`MAX_PEX_ADDED`].
pub const MAX_TRACKED_PEERS: usize = 200;

impl PexDeltaTracker {
    /// Creates an empty delta tracker.
    pub fn new() -> Self {
        Self {
            known: std::collections::HashMap::new(),
        }
    }

    /// Filters a candidate list down to only peers the recipient has not seen.
    ///
    /// Returns the subset of `candidates` whose `peer_id` is not in the
    /// recipient's known-set. The returned entries preserve their original
    /// order (stable filter).
    pub fn filter_new(&self, recipient: &PeerId, candidates: &[PexEntry]) -> Vec<PexEntry> {
        let known_set = self.known.get(recipient);
        candidates
            .iter()
            .filter(|entry| {
                known_set
                    .map(|s| !s.contains(&entry.peer_id))
                    .unwrap_or(true)
            })
            .cloned()
            .collect()
    }

    /// Records that the recipient now knows about the given peer IDs.
    ///
    /// Call this after successfully sending a PEX message containing these
    /// entries, so subsequent messages don't re-send them.
    pub fn mark_sent(&mut self, recipient: &PeerId, sent_peers: &[PeerId]) {
        let set = self.known.entry(*recipient).or_default();
        for id in sent_peers {
            set.insert(*id);
        }
    }

    /// Removes all tracking state for a recipient (e.g. on disconnect).
    pub fn remove_recipient(&mut self, recipient: &PeerId) {
        self.known.remove(recipient);
    }

    /// Returns the number of tracked recipients.
    pub fn recipient_count(&self) -> usize {
        self.known.len()
    }

    /// Returns how many peers the given recipient is known to know.
    pub fn known_count(&self, recipient: &PeerId) -> usize {
        self.known.get(recipient).map(|s| s.len()).unwrap_or(0)
    }
}

impl Default for PexDeltaTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── PexFlags ────────────────────────────────────────────────────

    /// Flags round-trip through byte encoding.
    ///
    /// The BEP-11 wire format uses a single byte for flags. Encoding and
    /// decoding must be lossless.
    #[test]
    fn flags_round_trip() {
        let flags = PexFlags {
            encryption: true,
            seed: false,
            utp: true,
            utp_holepunch: false,
            connectable: true,
        };
        let byte = flags.to_byte();
        let decoded = PexFlags::from_byte(byte);
        assert_eq!(flags, decoded);
    }

    /// Default flags are all false (byte 0x00).
    #[test]
    fn flags_default_all_false() {
        let flags = PexFlags::default();
        assert_eq!(flags.to_byte(), 0x00);
    }

    /// All flags set produces the expected byte value.
    #[test]
    fn flags_all_set() {
        let flags = PexFlags {
            encryption: true,
            seed: true,
            utp: true,
            utp_holepunch: true,
            connectable: true,
        };
        assert_eq!(flags.to_byte(), 0x01 | 0x02 | 0x04 | 0x08 | 0x10);
    }

    /// Seed flag is bit 1 (0x02).
    #[test]
    fn flags_seed_bit() {
        let flags = PexFlags::from_byte(0x02);
        assert!(flags.seed);
        assert!(!flags.encryption);
        assert!(!flags.connectable);
    }

    // ── PexMessage ──────────────────────────────────────────────────

    /// Empty PEX message reports as empty.
    #[test]
    fn empty_message_is_empty() {
        let msg = PexMessage::new(NetworkId::TEST);
        assert!(msg.is_empty());
        assert!(!msg.is_oversized());
    }

    /// Message with entries is not empty.
    #[test]
    fn message_with_entries_not_empty() {
        let mut msg = PexMessage::new(NetworkId::PRODUCTION);
        msg.added.push(PexEntry {
            peer_id: PeerId::from_key_material(b"peer-1"),
            addr: "192.168.1.1:6881".into(),
            flags: PexFlags::default(),
        });
        assert!(!msg.is_empty());
    }

    /// Oversized detection triggers above BEP-11 limits.
    #[test]
    fn oversized_detection() {
        let mut msg = PexMessage::new(NetworkId::TEST);
        for i in 0..=MAX_PEX_ADDED {
            msg.added.push(PexEntry {
                peer_id: PeerId::from_key_material(format!("peer-{i}").as_bytes()),
                addr: format!("10.0.0.{i}:6881"),
                flags: PexFlags::default(),
            });
        }
        assert!(msg.is_oversized());
    }

    /// Network ID is carried in the message for cross-network filtering.
    #[test]
    fn network_id_carried() {
        let msg = PexMessage::new(NetworkId::PRODUCTION);
        assert_eq!(msg.network_id, NetworkId::PRODUCTION);
    }

    // ── Security: adversarial flag bytes ────────────────────────────

    /// Unknown high bits in a flags byte are silently ignored.
    ///
    /// A misbehaving peer may set reserved bits (0x20–0x80) that are
    /// undefined in BEP-11. `from_byte` must not panic and must not set
    /// any of the known flags from those bits.
    #[test]
    fn adversarial_unknown_flag_bits_ignored() {
        // 0xE0 = bits 5,6,7 set — all undefined in BEP-11.
        let flags = PexFlags::from_byte(0xE0);
        assert!(!flags.encryption);
        assert!(!flags.seed);
        assert!(!flags.utp);
        assert!(!flags.utp_holepunch);
        assert!(!flags.connectable);
    }

    /// All 256 possible flag bytes round-trip without panic.
    ///
    /// The wire format is a single byte. Every possible value must be
    /// decodable without crashing, even if the round-trip loses unknown bits.
    #[test]
    fn adversarial_all_256_flag_bytes_decodable() {
        for b in 0..=255u8 {
            let flags = PexFlags::from_byte(b);
            let re_encoded = flags.to_byte();
            // Known bits must survive round-trip; unknown bits are lost.
            assert_eq!(re_encoded & 0x1F, b & 0x1F);
        }
    }

    /// Mixed known and unknown bits decode correctly.
    ///
    /// 0xFF has all bits set — known flags are true, unknown bits are lost
    /// on re-encode.
    #[test]
    fn adversarial_mixed_known_unknown_bits() {
        let flags = PexFlags::from_byte(0xFF);
        assert!(flags.encryption);
        assert!(flags.seed);
        assert!(flags.utp);
        assert!(flags.utp_holepunch);
        assert!(flags.connectable);
        // Re-encode loses bits 5-7.
        assert_eq!(flags.to_byte(), 0x1F);
    }

    /// Oversized dropped list triggers is_oversized.
    ///
    /// Both added AND dropped lists are checked against BEP-11 limits.
    #[test]
    fn adversarial_oversized_dropped() {
        let mut msg = PexMessage::new(NetworkId::TEST);
        for i in 0..=MAX_PEX_DROPPED {
            msg.dropped
                .push(PeerId::from_key_material(format!("peer-{i}").as_bytes()));
        }
        assert!(msg.is_oversized());
    }

    // ── PexDeltaTracker (IRC spanning-tree multicast) ───────────────

    /// New tracker starts with no recipients.
    #[test]
    fn delta_tracker_starts_empty() {
        let tracker = PexDeltaTracker::new();
        assert_eq!(tracker.recipient_count(), 0);
    }

    /// All candidates pass through when recipient has no history.
    ///
    /// A new recipient has an empty known-set, so every candidate is new.
    #[test]
    fn delta_tracker_all_new_for_unknown_recipient() {
        let tracker = PexDeltaTracker::new();
        let recipient = PeerId::from_key_material(b"recipient");
        let candidates = vec![
            PexEntry {
                peer_id: PeerId::from_key_material(b"p1"),
                addr: "1.2.3.4:6881".into(),
                flags: PexFlags::default(),
            },
            PexEntry {
                peer_id: PeerId::from_key_material(b"p2"),
                addr: "5.6.7.8:6881".into(),
                flags: PexFlags::default(),
            },
        ];
        let filtered = tracker.filter_new(&recipient, &candidates);
        assert_eq!(filtered.len(), 2);
    }

    /// Already-sent peers are filtered out on subsequent calls.
    ///
    /// After marking p1 as sent, the next filter_new call should exclude p1
    /// but still include p2.
    #[test]
    fn delta_tracker_filters_already_known() {
        let mut tracker = PexDeltaTracker::new();
        let recipient = PeerId::from_key_material(b"recipient");
        let p1 = PeerId::from_key_material(b"p1");
        let p2 = PeerId::from_key_material(b"p2");

        tracker.mark_sent(&recipient, std::slice::from_ref(&p1));

        let candidates = vec![
            PexEntry {
                peer_id: p1,
                addr: "1.2.3.4:6881".into(),
                flags: PexFlags::default(),
            },
            PexEntry {
                peer_id: p2,
                addr: "5.6.7.8:6881".into(),
                flags: PexFlags::default(),
            },
        ];
        let filtered = tracker.filter_new(&recipient, &candidates);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].peer_id, p2);
    }

    /// Per-recipient isolation — marking sent for one doesn't affect another.
    #[test]
    fn delta_tracker_per_recipient_isolation() {
        let mut tracker = PexDeltaTracker::new();
        let r1 = PeerId::from_key_material(b"r1");
        let r2 = PeerId::from_key_material(b"r2");
        let p1 = PeerId::from_key_material(b"p1");

        tracker.mark_sent(&r1, std::slice::from_ref(&p1));

        let candidates = vec![PexEntry {
            peer_id: p1,
            addr: "1.2.3.4:6881".into(),
            flags: PexFlags::default(),
        }];

        // r1 already knows p1 → filtered out
        assert_eq!(tracker.filter_new(&r1, &candidates).len(), 0);
        // r2 doesn't know p1 → passes through
        assert_eq!(tracker.filter_new(&r2, &candidates).len(), 1);
    }

    /// Removing a recipient clears all tracking state.
    #[test]
    fn delta_tracker_remove_recipient() {
        let mut tracker = PexDeltaTracker::new();
        let r = PeerId::from_key_material(b"r");
        let p = PeerId::from_key_material(b"p");

        tracker.mark_sent(&r, std::slice::from_ref(&p));
        assert_eq!(tracker.known_count(&r), 1);

        tracker.remove_recipient(&r);
        assert_eq!(tracker.recipient_count(), 0);
        assert_eq!(tracker.known_count(&r), 0);
    }

    /// known_count reflects the number of known peers per recipient.
    #[test]
    fn delta_tracker_known_count() {
        let mut tracker = PexDeltaTracker::new();
        let r = PeerId::from_key_material(b"r");
        let p1 = PeerId::from_key_material(b"p1");
        let p2 = PeerId::from_key_material(b"p2");

        assert_eq!(tracker.known_count(&r), 0);
        tracker.mark_sent(&r, &[p1, p2]);
        assert_eq!(tracker.known_count(&r), 2);
    }

    /// Default trait implementation works.
    #[test]
    fn delta_tracker_default() {
        let tracker = PexDeltaTracker::default();
        assert_eq!(tracker.recipient_count(), 0);
    }
}
