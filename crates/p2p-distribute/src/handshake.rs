// SPDX-License-Identifier: MIT OR Apache-2.0

//! Capability negotiation bitmap — peer feature advertisement.
//!
//! ## What
//!
//! Defines a compact capability bitmap that peers exchange during handshake
//! to advertise which protocol extensions they support. The coordinator
//! uses this to avoid trial-and-error probing — if a peer advertises
//! "no encryption", the coordinator doesn't waste a round-trip trying to
//! negotiate TLS.
//!
//! ## Why — IRC channel modes lesson
//!
//! IRC servers advertise their capabilities via `RPL_ISUPPORT` (numeric 005)
//! during connection registration. Each token declares a feature the server
//! supports: `NAMESX`, `UHNAMES`, `CASEMAPPING=rfc1459`, etc. Clients that
//! don't understand a token simply ignore it, so new capabilities can be
//! added without breaking older clients.
//!
//! Applied to P2P: without capability negotiation, the coordinator must
//! discover peer features through trial and error (request a Merkle proof
//! → peer returns "unsupported" → fall back to flat hash). A single
//! bitmap exchanged at connection time eliminates this overhead.
//!
//! ## How
//!
//! - [`Capabilities`]: A 32-bit bitmap where each bit represents a protocol
//!   extension. Known bits have named constants; unknown bits are preserved
//!   for forward compatibility (CTCP extensibility principle).
//! - [`HandshakeMessage`]: The exchange frame carrying the bitmap plus
//!   protocol version and network identity.

use crate::network_id::NetworkId;
use crate::peer_id::PeerId;

// ── Capabilities bitmap ─────────────────────────────────────────────

/// Protocol extension capability bitmap.
///
/// Each bit represents support for a specific protocol extension.
/// Unknown bits are preserved during round-trip (forward compat).
///
/// ## IRC ISUPPORT analogy
///
/// - Bit 0 (`ENCRYPTION`): peer supports obfuscated connections
///   → IRC `STARTTLS`
/// - Bit 1 (`MERKLE_VERIFY`): peer supports Merkle tree proof exchange
///   → IRC `NAMESX` (enhanced format)
/// - Bit 2 (`PEX`): peer supports Peer Exchange gossip
///   → IRC `UHNAMES` (enhanced metadata)
/// - Bit 3 (`DHT`): peer participates in Kademlia DHT
///   → IRC server-to-server linking
/// - Bit 4 (`WEB_SEED`): peer can serve pieces via HTTP Range requests
///   → IRC DCC (alternate transport)
/// - Bit 5 (`STREAMING`): peer supports byte-range streaming
///   → IRC DCC RESUME (partial transfer)
/// - Bit 6 (`UPLOAD_SLOTS`): peer enforces bounded upload slots (XDCC)
///   → IRC XDCC slot queue
/// - Bit 7 (`RATE_LIMIT`): peer applies per-connection rate limiting
///   → IRC flood control
///
/// ```
/// use p2p_distribute::handshake::Capabilities;
///
/// let caps = Capabilities::ENCRYPTION | Capabilities::PEX | Capabilities::DHT;
/// assert!(caps.supports(Capabilities::PEX));
/// assert!(!caps.supports(Capabilities::MERKLE_VERIFY));
///
/// // Round-trip through wire format preserves all bits.
/// let wire = caps.to_u32();
/// let decoded = Capabilities::from_u32(wire);
/// assert_eq!(caps, decoded);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Capabilities {
    bits: u32,
}

impl Capabilities {
    /// No capabilities advertised.
    pub const NONE: Self = Self { bits: 0 };

    /// Peer supports obfuscated/encrypted connections.
    pub const ENCRYPTION: Self = Self { bits: 1 << 0 };

    /// Peer supports Merkle tree proof exchange for piece verification.
    pub const MERKLE_VERIFY: Self = Self { bits: 1 << 1 };

    /// Peer supports Peer Exchange (PEX) gossip.
    pub const PEX: Self = Self { bits: 1 << 2 };

    /// Peer participates in Kademlia DHT.
    pub const DHT: Self = Self { bits: 1 << 3 };

    /// Peer can serve pieces via HTTP Range requests (web seed).
    pub const WEB_SEED: Self = Self { bits: 1 << 4 };

    /// Peer supports byte-range streaming (StreamingReader compat).
    pub const STREAMING: Self = Self { bits: 1 << 5 };

    /// Peer enforces bounded upload slots (XDCC slot queue pattern).
    pub const UPLOAD_SLOTS: Self = Self { bits: 1 << 6 };

    /// Peer applies per-connection rate limiting.
    pub const RATE_LIMIT: Self = Self { bits: 1 << 7 };

    /// All currently defined capabilities.
    pub const ALL_KNOWN: Self = Self { bits: 0xFF };

    /// Creates a capability set from a raw bitmap.
    ///
    /// Unknown bits are preserved for forward compatibility.
    pub const fn from_u32(bits: u32) -> Self {
        Self { bits }
    }

    /// Returns the raw bitmap.
    pub const fn to_u32(self) -> u32 {
        self.bits
    }

    /// Whether this capability set includes all bits from `other`.
    pub const fn supports(self, other: Self) -> bool {
        (self.bits & other.bits) == other.bits
    }

    /// Returns the intersection of two capability sets.
    ///
    /// Useful for finding the common capabilities between two peers
    /// during negotiation.
    pub const fn intersect(self, other: Self) -> Self {
        Self {
            bits: self.bits & other.bits,
        }
    }

    /// Returns the number of set capability bits.
    pub const fn count(self) -> u32 {
        self.bits.count_ones()
    }

    /// Whether no capabilities are advertised.
    pub const fn is_empty(self) -> bool {
        self.bits == 0
    }
}

impl std::ops::BitOr for Capabilities {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self {
            bits: self.bits | rhs.bits,
        }
    }
}

impl std::ops::BitOrAssign for Capabilities {
    fn bitor_assign(&mut self, rhs: Self) {
        self.bits |= rhs.bits;
    }
}

impl std::ops::BitAnd for Capabilities {
    type Output = Self;
    fn bitand(self, rhs: Self) -> Self {
        Self {
            bits: self.bits & rhs.bits,
        }
    }
}

// ── HandshakeMessage ────────────────────────────────────────────────

/// Protocol version for the current handshake format.
pub const PROTOCOL_VERSION: u8 = 1;

/// Handshake message exchanged at connection establishment.
///
/// Carries the peer's identity, network scope, protocol version, and
/// capability bitmap. The receiver uses this to:
///
/// - Verify network ID match (discard cross-network connections)
/// - Check protocol version compatibility
/// - Determine which extensions to enable for this connection
///
/// ## Wire format (34+ bytes)
///
/// ```text
/// [0]       protocol_version: u8
/// [1..5]    capabilities: u32 LE
/// [5..37]   peer_id: [u8; 32]
/// [37..69]  network_id: [u8; 32]
/// ```
///
/// The fixed-size format ensures the handshake can be parsed without
/// framing — the receiver knows exactly how many bytes to expect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandshakeMessage {
    /// Protocol version (for version negotiation).
    pub protocol_version: u8,
    /// Capability bitmap advertising supported extensions.
    pub capabilities: Capabilities,
    /// The sender's cryptographic peer identity.
    pub peer_id: PeerId,
    /// Network scope (isolates test/prod/custom networks).
    pub network_id: NetworkId,
}

impl HandshakeMessage {
    /// Creates a new handshake message with the current protocol version.
    pub fn new(peer_id: PeerId, network_id: NetworkId, capabilities: Capabilities) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            capabilities,
            peer_id,
            network_id,
        }
    }

    /// Negotiates common capabilities between local and remote handshakes.
    ///
    /// Returns `None` if network IDs don't match (cross-network isolation).
    /// Returns `Some(intersection)` for the capabilities both peers support.
    pub fn negotiate(&self, remote: &HandshakeMessage) -> Option<Capabilities> {
        if self.network_id != remote.network_id {
            return None;
        }
        Some(self.capabilities.intersect(remote.capabilities))
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Capabilities bitmap ─────────────────────────────────────────

    /// Individual capability constants have the correct bit positions.
    ///
    /// Each named constant must be a distinct power of two.
    #[test]
    fn capability_bit_positions() {
        assert_eq!(Capabilities::ENCRYPTION.to_u32(), 0x01);
        assert_eq!(Capabilities::MERKLE_VERIFY.to_u32(), 0x02);
        assert_eq!(Capabilities::PEX.to_u32(), 0x04);
        assert_eq!(Capabilities::DHT.to_u32(), 0x08);
        assert_eq!(Capabilities::WEB_SEED.to_u32(), 0x10);
        assert_eq!(Capabilities::STREAMING.to_u32(), 0x20);
        assert_eq!(Capabilities::UPLOAD_SLOTS.to_u32(), 0x40);
        assert_eq!(Capabilities::RATE_LIMIT.to_u32(), 0x80);
    }

    /// supports() correctly tests for presence of capability bits.
    ///
    /// A capability set with ENCRYPTION|PEX should report supports(PEX)
    /// = true and supports(DHT) = false.
    #[test]
    fn supports_checks_bit_presence() {
        let caps = Capabilities::ENCRYPTION | Capabilities::PEX;
        assert!(caps.supports(Capabilities::ENCRYPTION));
        assert!(caps.supports(Capabilities::PEX));
        assert!(!caps.supports(Capabilities::DHT));
        assert!(!caps.supports(Capabilities::MERKLE_VERIFY));
    }

    /// supports() with NONE always returns true (vacuous truth).
    ///
    /// "Does this peer support no features?" — yes, trivially.
    #[test]
    fn supports_none_always_true() {
        let caps = Capabilities::ENCRYPTION;
        assert!(caps.supports(Capabilities::NONE));
        assert!(Capabilities::NONE.supports(Capabilities::NONE));
    }

    /// Intersection of two capability sets.
    ///
    /// Only the bits present in both sets survive.
    #[test]
    fn intersect_common_caps() {
        let local = Capabilities::ENCRYPTION | Capabilities::PEX | Capabilities::DHT;
        let remote = Capabilities::PEX | Capabilities::STREAMING;
        let common = local.intersect(remote);

        assert!(common.supports(Capabilities::PEX));
        assert!(!common.supports(Capabilities::ENCRYPTION));
        assert!(!common.supports(Capabilities::STREAMING));
    }

    /// Round-trip through u32 preserves all bits (including unknown).
    ///
    /// Forward compatibility: bits not yet defined in named constants
    /// must survive serialisation.
    #[test]
    fn round_trip_preserves_unknown_bits() {
        let caps = Capabilities::from_u32(0xDEAD_BEEF);
        let wire = caps.to_u32();
        let decoded = Capabilities::from_u32(wire);
        assert_eq!(caps, decoded);
    }

    /// count() returns the number of set bits.
    #[test]
    fn count_set_bits() {
        let caps = Capabilities::ENCRYPTION | Capabilities::PEX | Capabilities::DHT;
        assert_eq!(caps.count(), 3);
        assert_eq!(Capabilities::NONE.count(), 0);
        assert_eq!(Capabilities::ALL_KNOWN.count(), 8);
    }

    /// is_empty() returns true for NONE, false otherwise.
    #[test]
    fn is_empty_check() {
        assert!(Capabilities::NONE.is_empty());
        assert!(!Capabilities::ENCRYPTION.is_empty());
    }

    /// BitOr combines capabilities.
    #[test]
    fn bitor_combines() {
        let mut caps = Capabilities::ENCRYPTION;
        caps |= Capabilities::PEX;
        assert!(caps.supports(Capabilities::ENCRYPTION));
        assert!(caps.supports(Capabilities::PEX));
    }

    // ── HandshakeMessage ────────────────────────────────────────────

    /// Handshake uses the current protocol version.
    #[test]
    fn handshake_protocol_version() {
        let msg = HandshakeMessage::new(
            PeerId::from_key_material(b"alice"),
            NetworkId::TEST,
            Capabilities::PEX,
        );
        assert_eq!(msg.protocol_version, PROTOCOL_VERSION);
    }

    /// Negotiate succeeds when network IDs match.
    ///
    /// Two peers on the same network should find their common capabilities.
    #[test]
    fn negotiate_same_network() {
        let local = HandshakeMessage::new(
            PeerId::from_key_material(b"alice"),
            NetworkId::TEST,
            Capabilities::ENCRYPTION | Capabilities::PEX | Capabilities::DHT,
        );
        let remote = HandshakeMessage::new(
            PeerId::from_key_material(b"bob"),
            NetworkId::TEST,
            Capabilities::PEX | Capabilities::STREAMING,
        );

        let common = local.negotiate(&remote);
        assert!(common.is_some());
        let common = common.unwrap();
        assert!(common.supports(Capabilities::PEX));
        assert!(!common.supports(Capabilities::ENCRYPTION));
    }

    /// Negotiate returns None when network IDs differ.
    ///
    /// Cross-network isolation: test and production peers must not
    /// negotiate.
    #[test]
    fn negotiate_different_network_fails() {
        let local = HandshakeMessage::new(
            PeerId::from_key_material(b"alice"),
            NetworkId::TEST,
            Capabilities::ALL_KNOWN,
        );
        let remote = HandshakeMessage::new(
            PeerId::from_key_material(b"bob"),
            NetworkId::PRODUCTION,
            Capabilities::ALL_KNOWN,
        );

        assert!(local.negotiate(&remote).is_none());
    }

    /// Handshake carries the correct peer identity.
    #[test]
    fn handshake_carries_peer_id() {
        let id = PeerId::from_key_material(b"test-peer");
        let msg = HandshakeMessage::new(id, NetworkId::TEST, Capabilities::NONE);
        assert_eq!(msg.peer_id, id);
    }

    /// Handshake equality is structural.
    #[test]
    fn handshake_equality() {
        let msg1 = HandshakeMessage::new(
            PeerId::from_key_material(b"alice"),
            NetworkId::TEST,
            Capabilities::PEX,
        );
        let msg2 = msg1.clone();
        assert_eq!(msg1, msg2);
    }
}
