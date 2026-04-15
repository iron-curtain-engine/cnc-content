// SPDX-License-Identifier: MIT OR Apache-2.0

//! Network isolation tag — prevents test/dev/prod swarm mixing.
//!
//! ## Design rationale (SSB Secret Handshake)
//!
//! Scuttlebutt's Secret Handshake protocol uses a 32-byte "network identifier"
//! as the first input to its authentication. Peers on different networks
//! simply cannot complete the handshake — they are cryptographically isolated.
//!
//! For Iron Curtain, `NetworkId` provides a lighter-weight version of this
//! concept: a tag that the coordinator includes in peer exchange messages
//! and tracker announcements. Peers with mismatched `NetworkId`s ignore
//! each other, preventing:
//!
//! - **Test/prod contamination**: development builds with synthetic content
//!   don't pollute the production swarm.
//! - **Version isolation**: incompatible protocol versions form separate
//!   swarms without explicit version negotiation.
//! - **Private swarms**: LAN parties or tournaments can use a custom
//!   `NetworkId` to keep their swarm local.
//!
//! ## Well-known networks
//!
//! [`NetworkId::PRODUCTION`] and [`NetworkId::TEST`] are compile-time
//! constants for the two standard networks. Custom networks use
//! [`NetworkId::from_name()`] which derives a deterministic 32-byte tag
//! from an arbitrary name string via SHA-256.

use sha2::{Digest, Sha256};

/// Size of a network identifier in bytes.
pub const NETWORK_ID_LEN: usize = 32;

/// A 32-byte network isolation tag.
///
/// Peers with different `NetworkId`s ignore each other in peer exchange
/// and tracker communication, forming independent swarms.
///
/// ## Usage
///
/// ```
/// use p2p_distribute::NetworkId;
///
/// // Standard production network.
/// let prod = NetworkId::PRODUCTION;
///
/// // Private LAN party network.
/// let lan = NetworkId::from_name("friday-night-lan-2026");
///
/// assert_ne!(prod, lan);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NetworkId([u8; NETWORK_ID_LEN]);

impl NetworkId {
    /// The production Iron Curtain content network.
    ///
    /// All release builds should use this unless explicitly overridden.
    /// ASCII: "ic-production-network-v1" padded with zero bytes.
    pub const PRODUCTION: Self = Self(*b"ic-production-network-v1\0\0\0\0\0\0\0\0");

    /// The test/development network.
    ///
    /// CI, local development, and integration tests use this to avoid
    /// contaminating the production swarm with synthetic content.
    /// ASCII: "ic-test-network-v1" padded with zero bytes.
    pub const TEST: Self = Self(*b"ic-test-network-v1\0\0\0\0\0\0\0\0\0\0\0\0\0\0");

    /// Creates a custom network ID from an arbitrary name string.
    ///
    /// The name is SHA-256 hashed with a domain separator prefix to produce
    /// a deterministic 32-byte tag. The same name always produces the same
    /// `NetworkId`.
    ///
    /// ## Use cases
    ///
    /// - LAN party isolation: `NetworkId::from_name("friday-night-ra")`
    /// - Tournament networks: `NetworkId::from_name("tournament-2026-q3")`
    /// - Feature branch testing: `NetworkId::from_name("feature-streaming-v2")`
    pub fn from_name(name: &str) -> Self {
        let mut hasher = Sha256::new();
        // Domain separator prevents collision with other SHA-256 uses.
        hasher.update(b"ic-network-id:");
        hasher.update(name.as_bytes());
        let digest = hasher.finalize();
        let mut bytes = [0u8; NETWORK_ID_LEN];
        bytes.copy_from_slice(&digest);
        Self(bytes)
    }

    /// Returns the raw 32-byte network identifier.
    pub fn as_bytes(&self) -> &[u8; NETWORK_ID_LEN] {
        &self.0
    }

    /// Creates a network ID from raw bytes.
    ///
    /// Use this to reconstruct a `NetworkId` from a serialized
    /// representation (e.g. a credential's canonical bytes). No
    /// hashing is applied — the bytes are used as-is.
    pub fn from_raw(bytes: [u8; NETWORK_ID_LEN]) -> Self {
        Self(bytes)
    }
}

impl std::fmt::Display for NetworkId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Show first 8 bytes hex for readability.
        for b in self.0.iter().take(8) {
            write!(f, "{b:02x}")?;
        }
        write!(f, "…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Well-known constants ────────────────────────────────────────

    /// Production and test network IDs are distinct.
    ///
    /// This is the core isolation invariant — the two standard networks
    /// must never be confused.
    #[test]
    fn production_and_test_are_distinct() {
        assert_ne!(NetworkId::PRODUCTION, NetworkId::TEST);
    }

    /// Well-known constants are 32 bytes.
    #[test]
    fn well_known_constants_correct_size() {
        assert_eq!(NetworkId::PRODUCTION.as_bytes().len(), NETWORK_ID_LEN);
        assert_eq!(NetworkId::TEST.as_bytes().len(), NETWORK_ID_LEN);
    }

    // ── Custom networks ─────────────────────────────────────────────

    /// `from_name` is deterministic — same name → same NetworkId.
    #[test]
    fn from_name_deterministic() {
        let a = NetworkId::from_name("lan-party");
        let b = NetworkId::from_name("lan-party");
        assert_eq!(a, b);
    }

    /// Different names produce different network IDs.
    #[test]
    fn from_name_different_names_differ() {
        let a = NetworkId::from_name("network-alpha");
        let b = NetworkId::from_name("network-bravo");
        assert_ne!(a, b);
    }

    /// Custom networks are distinct from well-known constants.
    #[test]
    fn custom_differs_from_well_known() {
        let custom = NetworkId::from_name("custom");
        assert_ne!(custom, NetworkId::PRODUCTION);
        assert_ne!(custom, NetworkId::TEST);
    }

    // ── Display ─────────────────────────────────────────────────────

    /// Display shows truncated hex for readability.
    #[test]
    fn display_truncated_hex() {
        let id = NetworkId::PRODUCTION;
        let s = id.to_string();
        assert!(s.ends_with('…'), "should end with ellipsis: {s}");
        // "ic-p" in hex is "69632d70", first 4 of 8 bytes shown.
        assert!(
            s.starts_with("69632d70"),
            "should start with 'ic-p' hex: {s}"
        );
    }
}
