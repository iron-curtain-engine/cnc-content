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
}
