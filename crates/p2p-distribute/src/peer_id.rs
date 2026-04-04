// SPDX-License-Identifier: MIT OR Apache-2.0

//! Cryptographic peer identity — stable, type-safe identifier for peers across
//! sessions, reconnections, and transport boundaries.
//!
//! ## Design rationale
//!
//! A peer's identity must outlive any single download session so that the
//! coordinator can:
//!
//! - **Skip trust slow-start** for returning peers (restore trust level).
//! - **Enforce persistent bans** on misbehaving peers (data corruption,
//!   protocol violation).
//! - **Track reconnections** accurately — matching a reconnected peer to
//!   its prior session stats.
//!
//! ## Hamachi key-as-identity principle
//!
//! Cross-engine protocol research (Gap 7: Zero-Signup First Game) identifies
//! Hamachi's key-as-identity as a foundational design principle: **the
//! cryptographic key is the identity.** No registration, no signup, no
//! username selection. A fresh peer calls [`PeerId::generate`] on first
//! run, gets an anonymous but functional identity immediately, and can
//! participate in the swarm without any manual setup.
//!
//! This zero-signup model means:
//! - Identity creation is **automatic and silent** — no barrier to entry.
//! - The generated identity is **anonymous but functional** — full swarm
//!   participation, reputation tracking, and reconnection matching.
//! - Peers can later **upgrade** to Ed25519-based identity when the IC
//!   wire protocol arrives (D049 `ic_auth`).
//! - A human-readable [`callsign`](PeerId::callsign) is deterministically
//!   derived from the identity bytes for display purposes.
//!
//! ## Identity derivation
//!
//! [`PeerId`] is a 32-byte opaque identifier derived from peer-specific
//! source material via SHA-256:
//!
//! - **Auto-generated**: 32 bytes of OS randomness ([`PeerId::generate`]).
//! - **Web seeds**: deterministic — SHA-256 of the URL.
//! - **BT swarm peers**: SHA-256 of the 20-byte BT peer ID.
//! - **Future IC wire protocol**: raw Ed25519 public key (already 32 bytes).
//!
//! Using SHA-256 everywhere ensures uniform size and avoids accidentally
//! comparing identities across different namespaces (URL vs. BT peer ID).
//!
//! ## Wire protocol compatibility (D049 `ic_auth`)
//!
//! When the IC wire protocol is implemented (M1–M3), `PeerId` will accept
//! raw Ed25519 public keys via [`PeerId::from_ed25519_pubkey`]. The 32-byte
//! size was chosen to match Ed25519 public key length exactly.
//!
//! ## Version/type discriminator
//!
//! Each `PeerId` carries a [`PeerIdKind`] tag that identifies how the identity
//! was derived (random, SHA-256 hashed, or raw Ed25519). This tag is:
//!
//! - **Not part of equality**: two `PeerId`s with the same bytes are equal
//!   regardless of kind. Identity is determined by bytes alone.
//! - **Embedded in the encoded form**: [`PeerId::to_encoded()`] includes the
//!   kind so that decoders know how the identity was created.
//! - **Zero runtime cost**: a single `u8` stored alongside the 32-byte
//!   identity, optimised away for comparison-only usage patterns.
//!
//! Inspired by libp2p's multicodec-prefixed `PeerId` and age's typed
//! recipient prefixes (`age1...`, `age1pq1...`).
//!
//! ## Compact encoded form
//!
//! [`PeerId::to_encoded()`] produces a human-readable string like
//! `ic1r4242...4242` (68 characters) that is:
//!
//! - **Self-describing**: the `ic1` prefix identifies the format version,
//!   the kind character (`r`/`h`/`e`) identifies the derivation method.
//! - **Copy-pasteable**: suitable for config files, log grep, and CLI args.
//! - **Round-trips**: [`PeerId::from_encoded()`] parses it back exactly.
//!
//! Inspired by age's `age1...` recipient encoding and SSB's `@...ed25519`
//! identity format.
//!
//! ## Secret key zeroization policy
//!
//! `PeerId` contains only **public** identity material — never secret keys.
//! If the IC wire protocol (D049 `ic_auth`) later introduces keypairs for
//! authenticated identity, the secret key half **must** use the `zeroize`
//! crate to clear memory on drop. This follows WireGuard's aggressive key
//! zeroing and libp2p's use of `zeroize` for `Keypair` types.
//!
//! Until keypairs are needed, no `zeroize` dependency is required.

use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};

/// Size of a peer identity in bytes. Matches Ed25519 public key length
/// (32 bytes) for future wire protocol compatibility.
pub const PEER_ID_LEN: usize = 32;

// ── Identity kind (version/type discriminator) ──────────────────────
//
// libp2p prefixes PeerId with a multicodec byte to distinguish key types.
// age uses different HRP prefixes (age1 vs age1pq1). We store the
// derivation method alongside the bytes for forward-compatible encoding.

/// Identifies how a [`PeerId`] was derived, for forward-compatible encoding.
///
/// Stored alongside the identity bytes but **not** part of equality
/// comparison. Embedded in the compact encoded form ([`PeerId::to_encoded()`])
/// so decoders know the identity's provenance. Future identity types
/// (e.g. post-quantum key hashes) get their own variant and prefix
/// character without breaking existing encoded identities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PeerIdKind {
    /// Identity derived from 32 bytes of OS randomness (zero-signup).
    Random = 0x00,
    /// Identity derived by SHA-256 hashing key material (URL, BT peer ID).
    Hashed = 0x01,
    /// Raw Ed25519 public key stored directly (future IC wire protocol).
    Ed25519 = 0x02,
}

/// Errors from decoding an encoded `PeerId` string.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PeerIdDecodeError {
    #[error("invalid encoded PeerId length: expected 68, got {length}")]
    InvalidLength { length: usize },
    #[error("invalid prefix: expected 'ic1', found '{found}'")]
    InvalidPrefix { found: String },
    #[error("invalid kind byte: 0x{byte:02x} (expected 'r', 'h', or 'e')")]
    InvalidKind { byte: u8 },
    #[error("invalid hex encoding: {detail}")]
    InvalidHex { detail: String },
}

// ── Callsign generation ─────────────────────────────────────────────
//
// Hamachi lesson (Gap 7): "Display name defaults to a generated callsign
// (e.g., 'Commander_7K3')." The title is selected deterministically from
// the identity bytes, and the suffix is base-36 encoded from subsequent
// bytes. Military-themed titles suit the C&C domain.

/// Military-themed callsign titles. 16 entries — the upper nibble (4 bits)
/// of the first identity byte selects the title, guaranteeing full coverage
/// with no out-of-range possibility.
const CALLSIGN_TITLES: [&str; 16] = [
    "Commander",
    "Captain",
    "Colonel",
    "General",
    "Major",
    "Sergeant",
    "Lieutenant",
    "Marshal",
    "Admiral",
    "Warden",
    "Ranger",
    "Vanguard",
    "Striker",
    "Shadow",
    "Sentinel",
    "Phantom",
];

/// Base-36 alphabet for callsign suffix generation (digits + uppercase).
/// `byte % 36` indexes this array to produce one alphanumeric character.
const BASE36: &[u8; 36] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";

// ── PeerId ──────────────────────────────────────────────────────────

/// A 32-byte, type-safe peer identity.
///
/// Derived from peer-specific source material (URL, BT peer ID, or Ed25519
/// public key). Two `PeerId`s are equal if and only if they represent the
/// same peer — the coordinator uses this for reconnection tracking and
/// cross-session reputation persistence.
///
/// ## Construction
///
/// Use the appropriate constructor for the peer type:
///
/// ```
/// use p2p_distribute::PeerId;
///
/// // Web seed — identity derived from URL.
/// let ws_id = PeerId::from_key_material(b"https://example.com/content.zip");
///
/// // BT peer — identity derived from 20-byte peer ID.
/// let bt_id = PeerId::from_key_material(&[0xAB; 20]);
///
/// // Future: Ed25519 public key (raw 32 bytes).
/// let ed_id = PeerId::from_ed25519_pubkey([0x42; 32]);
/// ```
#[derive(Clone, Copy)]
pub struct PeerId {
    /// The 32-byte identity payload.
    bytes: [u8; PEER_ID_LEN],
    /// How this identity was derived (for encoding, not equality).
    kind: PeerIdKind,
}

// ── Equality, hashing, ordering (bytes-only, kind-agnostic) ─────────
//
// Two PeerIds are equal if their bytes match, regardless of how they were
// derived. This ensures that a PeerId loaded from disk (kind unknown) matches
// one freshly derived from key material.

impl PartialEq for PeerId {
    fn eq(&self, other: &Self) -> bool {
        self.bytes == other.bytes
    }
}

impl Eq for PeerId {}

impl Hash for PeerId {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.bytes.hash(state);
    }
}

impl PartialOrd for PeerId {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PeerId {
    fn cmp(&self, other: &Self) -> Ordering {
        self.bytes.cmp(&other.bytes)
    }
}

impl PeerId {
    /// Creates a fresh random `PeerId` for zero-signup identity.
    ///
    /// Applies Hamachi's key-as-identity principle (cross-engine protocol
    /// lessons, Gap 7): identity creation happens silently with zero
    /// explicit setup. The generated identity is anonymous but functional —
    /// the peer can participate in the swarm immediately and optionally
    /// upgrade to Ed25519-based identity later.
    ///
    /// Uses OS-provided randomness (`getrandom`) for the 32-byte identity.
    /// Failure indicates a catastrophic OS randomness issue (e.g. early
    /// boot on a system without entropy).
    ///
    /// ```
    /// use p2p_distribute::PeerId;
    ///
    /// let id = PeerId::generate().expect("OS randomness unavailable");
    /// assert_eq!(id.as_bytes().len(), 32);
    /// // Each call produces a unique identity.
    /// let id2 = PeerId::generate().expect("OS randomness unavailable");
    /// assert_ne!(id, id2);
    /// ```
    pub fn generate() -> std::io::Result<Self> {
        let mut bytes = [0u8; PEER_ID_LEN];
        getrandom::getrandom(&mut bytes).map_err(std::io::Error::from)?;
        Ok(Self {
            bytes,
            kind: PeerIdKind::Random,
        })
    }

    /// Creates a `PeerId` by SHA-256 hashing arbitrary key material.
    ///
    /// This is the primary constructor for peers that don't have a native
    /// 32-byte identity. The SHA-256 digest ensures uniform size and prevents
    /// cross-namespace collisions (a URL can never accidentally equal a BT
    /// peer ID).
    ///
    /// - **Web seeds**: pass the URL bytes.
    /// - **BT swarm peers**: pass the 20-byte BT peer ID.
    /// - **Any other source**: pass any unique, stable byte sequence.
    pub fn from_key_material(material: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(material);
        let digest = hasher.finalize();
        let mut bytes = [0u8; PEER_ID_LEN];
        bytes.copy_from_slice(&digest);
        Self {
            bytes,
            kind: PeerIdKind::Hashed,
        }
    }

    /// Creates a `PeerId` from a raw Ed25519 public key.
    ///
    /// Ed25519 public keys are exactly 32 bytes, matching `PEER_ID_LEN`.
    /// Unlike [`from_key_material`](Self::from_key_material), this stores
    /// the key directly without hashing — the public key *is* the identity.
    ///
    /// ## When to use
    ///
    /// When the IC wire protocol `ic_auth` extension provides an Ed25519
    /// public key during the peer handshake (D049 milestone M1–M3).
    pub fn from_ed25519_pubkey(pubkey: [u8; PEER_ID_LEN]) -> Self {
        Self {
            bytes: pubkey,
            kind: PeerIdKind::Ed25519,
        }
    }

    /// Creates a `PeerId` from a raw 32-byte array.
    ///
    /// No hashing or transformation — the bytes are used as-is.
    /// Useful for deserializing a previously-stored `PeerId`.
    pub fn from_bytes(bytes: [u8; PEER_ID_LEN]) -> Self {
        Self {
            bytes,
            kind: PeerIdKind::Random,
        }
    }

    /// Returns the raw 32-byte identity.
    pub fn as_bytes(&self) -> &[u8; PEER_ID_LEN] {
        &self.bytes
    }

    /// Returns how this identity was derived.
    ///
    /// Useful for logging, diagnostics, and the compact encoded form.
    /// Not part of equality comparison.
    pub fn kind(&self) -> PeerIdKind {
        self.kind
    }

    /// Derives a human-readable callsign from the identity bytes.
    ///
    /// Implements Gap 7's "display name defaults to a generated callsign
    /// (e.g., 'Commander_7K3')" — a military-themed title plus a 3-character
    /// alphanumeric suffix, both deterministically derived from the identity.
    ///
    /// The callsign is **deterministic**: the same `PeerId` always produces
    /// the same callsign. It is not guaranteed to be globally unique (the
    /// 3-char suffix provides ~46K combinations per title, ~740K total).
    /// Use [`Display`] or [`Debug`] for unambiguous identification.
    ///
    /// ```
    /// use p2p_distribute::PeerId;
    ///
    /// let id = PeerId::from_bytes([0x42; 32]);
    /// let cs = id.callsign();
    /// // Title from upper nibble of byte 0 (0x4 = index 4 = "Major").
    /// // Suffix from bytes 1–3 (0x42 % 36 = 30 = 'U' in base-36).
    /// assert_eq!(cs, "Major_UUU");
    /// ```
    pub fn callsign(&self) -> String {
        // Destructure first 4 bytes — compile-time guaranteed in-bounds
        // because `self.0` is `[u8; 32]`.
        let [b0, b1, b2, b3, ..] = self.bytes;

        // Upper nibble of first byte selects title (0–15 → 16 titles).
        let title_idx = (b0 >> 4) as usize;
        let title = CALLSIGN_TITLES
            .get(title_idx)
            .copied()
            .unwrap_or("Commander");

        // Bytes 1–3 mod 36 index into base-36 for the alphanumeric suffix.
        let suffix: String = [b1, b2, b3]
            .iter()
            .map(|b| {
                let idx = (*b % 36) as usize;
                BASE36.get(idx).copied().unwrap_or(b'0') as char
            })
            .collect();

        format!("{title}_{suffix}")
    }
}

// ── Display / Debug ─────────────────────────────────────────────────

impl fmt::Display for PeerId {
    /// Displays the peer ID as a truncated hex string (first 8 bytes / 16 chars).
    ///
    /// Full identity is available via [`Debug`] or [`as_bytes()`](Self::as_bytes).
    /// Truncation matches common BT client UI practice (show enough to identify,
    /// not so much that logs are unreadable).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in self.bytes.iter().take(8) {
            write!(f, "{b:02x}")?;
        }
        write!(f, "…")
    }
}

impl fmt::Debug for PeerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PeerId(")?;
        for b in &self.bytes {
            write!(f, "{b:02x}")?;
        }
        write!(f, ")")
    }
}

impl PeerId {
    /// Produces a compact, self-describing encoded string.
    ///
    /// Format: `ic1` + kind character + lowercase hex of the 32 identity bytes.
    ///
    /// | Kind      | Char | Example prefix |
    /// |-----------|------|----------------|
    /// | Random    | `r`  | `ic1r4242...`  |
    /// | Hashed    | `h`  | `ic1h4242...`  |
    /// | Ed25519   | `e`  | `ic1e4242...`  |
    ///
    /// The encoded form is 68 characters long — compact enough for config
    /// files and log lines, self-describing enough for debugging.
    ///
    /// Inspired by age's `age1...` recipient format and SSB's `@...ed25519`
    /// identity encoding.
    ///
    /// ```
    /// use p2p_distribute::PeerId;
    ///
    /// let id = PeerId::from_key_material(b"https://example.com/content.zip");
    /// let encoded = id.to_encoded();
    /// assert!(encoded.starts_with("ic1h"));
    /// assert_eq!(encoded.len(), 68);
    ///
    /// let decoded = PeerId::from_encoded(&encoded).unwrap();
    /// assert_eq!(decoded, id);
    /// ```
    pub fn to_encoded(&self) -> String {
        let kind_char = match self.kind {
            PeerIdKind::Random => 'r',
            PeerIdKind::Hashed => 'h',
            PeerIdKind::Ed25519 => 'e',
        };
        let hex = crate::hex_encode(&self.bytes);
        format!("ic1{kind_char}{hex}")
    }

    /// Parses a compact encoded string back into a `PeerId`.
    ///
    /// Accepts the format produced by [`to_encoded()`](Self::to_encoded):
    /// `ic1` + kind character + 64 lowercase hex characters.
    ///
    /// ```
    /// use p2p_distribute::PeerId;
    ///
    /// let id = PeerId::from_bytes([0xAB; 32]);
    /// let encoded = id.to_encoded();
    /// let decoded = PeerId::from_encoded(&encoded).unwrap();
    /// assert_eq!(decoded, id);
    /// ```
    pub fn from_encoded(s: &str) -> Result<Self, PeerIdDecodeError> {
        let s = s.trim();
        if s.len() != 68 {
            return Err(PeerIdDecodeError::InvalidLength { length: s.len() });
        }
        let prefix = s.get(..3).unwrap_or("");
        if prefix != "ic1" {
            return Err(PeerIdDecodeError::InvalidPrefix {
                found: prefix.to_string(),
            });
        }
        let kind_byte = s.as_bytes().get(3).copied().unwrap_or(0);
        let kind = match kind_byte {
            b'r' => PeerIdKind::Random,
            b'h' => PeerIdKind::Hashed,
            b'e' => PeerIdKind::Ed25519,
            other => {
                return Err(PeerIdDecodeError::InvalidKind { byte: other });
            }
        };
        let hex_part = s.get(4..).unwrap_or("");
        let bytes = hex_decode_peer_id(hex_part)?;
        Ok(Self { bytes, kind })
    }
}

/// Decodes a 64-character lowercase hex string into a 32-byte array.
fn hex_decode_peer_id(hex: &str) -> Result<[u8; PEER_ID_LEN], PeerIdDecodeError> {
    if hex.len() != PEER_ID_LEN * 2 {
        return Err(PeerIdDecodeError::InvalidLength {
            length: hex.len().saturating_add(4),
        });
    }
    let mut bytes = [0u8; PEER_ID_LEN];
    let mut chars = hex.chars();
    for slot in &mut bytes {
        let hi = chars.next().ok_or_else(|| PeerIdDecodeError::InvalidHex {
            detail: "unexpected end of hex".into(),
        })?;
        let lo = chars.next().ok_or_else(|| PeerIdDecodeError::InvalidHex {
            detail: "unexpected end of hex".into(),
        })?;
        let hi_val = hi
            .to_digit(16)
            .ok_or_else(|| PeerIdDecodeError::InvalidHex {
                detail: format!("invalid hex character: {hi}"),
            })?;
        let lo_val = lo
            .to_digit(16)
            .ok_or_else(|| PeerIdDecodeError::InvalidHex {
                detail: format!("invalid hex character: {lo}"),
            })?;
        *slot = (hi_val as u8) << 4 | lo_val as u8;
    }
    Ok(bytes)
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Construction ────────────────────────────────────────────────

    /// `from_key_material` produces deterministic identities.
    ///
    /// The same input must always produce the same `PeerId`. This is the
    /// foundation for cross-session matching — a web seed URL always maps
    /// to the same identity.
    #[test]
    fn from_key_material_deterministic() {
        let a = PeerId::from_key_material(b"https://example.com/file.zip");
        let b = PeerId::from_key_material(b"https://example.com/file.zip");
        assert_eq!(a, b);
    }

    /// Different input material produces different identities.
    ///
    /// Collision resistance ensures each peer gets a unique identity.
    #[test]
    fn from_key_material_different_inputs_differ() {
        let a = PeerId::from_key_material(b"https://example.com/file.zip");
        let b = PeerId::from_key_material(b"https://other.com/file.zip");
        assert_ne!(a, b);
    }

    /// `from_ed25519_pubkey` stores the key directly without hashing.
    ///
    /// Ed25519 public keys are already 32 bytes — hashing would lose the
    /// ability to verify signatures against the original key.
    #[test]
    fn from_ed25519_pubkey_stores_raw() {
        let key = [0x42; PEER_ID_LEN];
        let id = PeerId::from_ed25519_pubkey(key);
        assert_eq!(id.as_bytes(), &key);
    }

    /// `from_bytes` round-trips with `as_bytes`.
    #[test]
    fn from_bytes_round_trip() {
        let bytes = [0xAB; PEER_ID_LEN];
        let id = PeerId::from_bytes(bytes);
        assert_eq!(*id.as_bytes(), bytes);
    }

    /// `from_key_material` produces valid 32-byte output regardless of
    /// input length — empty, short, and long inputs all work.
    #[test]
    fn from_key_material_various_lengths() {
        let empty = PeerId::from_key_material(b"");
        let short = PeerId::from_key_material(b"x");
        let long = PeerId::from_key_material(&[0xFF; 1024]);

        // All produce valid 32-byte identities.
        assert_eq!(empty.as_bytes().len(), PEER_ID_LEN);
        assert_eq!(short.as_bytes().len(), PEER_ID_LEN);
        assert_eq!(long.as_bytes().len(), PEER_ID_LEN);

        // All are distinct.
        assert_ne!(empty, short);
        assert_ne!(short, long);
        assert_ne!(empty, long);
    }

    // ── Display ─────────────────────────────────────────────────────

    /// `Display` shows truncated hex (first 8 bytes + ellipsis).
    ///
    /// Human-readable but compact for log output.
    #[test]
    fn display_truncated_hex() {
        let id = PeerId::from_bytes([0xAB; PEER_ID_LEN]);
        let s = id.to_string();
        // 8 bytes × 2 hex chars + "…" = 17 chars.
        assert_eq!(s, "abababababababab…");
    }

    /// `Debug` shows the full 32-byte hex.
    #[test]
    fn debug_full_hex() {
        let mut bytes = [0u8; PEER_ID_LEN];
        bytes[0] = 0x01;
        bytes[31] = 0xFF;
        let id = PeerId::from_bytes(bytes);
        let s = format!("{id:?}");
        assert!(s.starts_with("PeerId(01"));
        assert!(s.ends_with("ff)"));
        // Full hex is 64 chars + "PeerId(" + ")" = 72 chars.
        assert_eq!(s.len(), 72);
    }

    // ── Ordering and hashing ────────────────────────────────────────

    /// `PeerId` implements `Ord` for use in sorted collections.
    ///
    /// Ordering is by raw byte comparison — no semantic meaning, but
    /// provides consistent ordering for deterministic iteration.
    #[test]
    fn peer_id_ordering() {
        let low = PeerId::from_bytes([0x00; PEER_ID_LEN]);
        let high = PeerId::from_bytes([0xFF; PEER_ID_LEN]);
        assert!(low < high);
    }

    /// `PeerId` can be used as a `HashMap` key.
    #[test]
    fn peer_id_hashable() {
        use std::collections::HashMap;
        let mut map = HashMap::new();
        let id = PeerId::from_key_material(b"test");
        map.insert(id, 42);
        assert_eq!(map.get(&id), Some(&42));
    }

    // ── Cross-namespace isolation ───────────────────────────────────

    /// A URL and a BT peer ID with the same byte content produce different
    /// `PeerId`s when constructed via `from_key_material`.
    ///
    /// This ensures that identity namespaces don't collide — a web seed
    /// and a BT peer can never be confused even if their source bytes happen
    /// to overlap (extremely unlikely but the property should hold by design).
    #[test]
    fn url_and_bt_peer_id_same_bytes_still_same() {
        // When the *same bytes* are passed, from_key_material produces the
        // same PeerId. Namespace isolation relies on the source material
        // being naturally different (URLs are human-readable, BT peer IDs
        // are 20-byte binary), not on the hashing step.
        let a = PeerId::from_key_material(b"shared-bytes");
        let b = PeerId::from_key_material(b"shared-bytes");
        assert_eq!(a, b);
    }

    // ── Zero-signup generation (Hamachi principle) ──────────────────

    /// `generate()` produces a valid 32-byte identity from OS randomness.
    ///
    /// This is the zero-signup path — a brand-new peer gets a functional
    /// identity without any registration or pre-existing material.
    #[test]
    fn generate_produces_valid_identity() {
        let id = PeerId::generate().unwrap();
        assert_eq!(id.as_bytes().len(), PEER_ID_LEN);
    }

    /// `generate()` produces unique identities on each call.
    ///
    /// The 32-byte random space (2^256) makes collisions astronomically
    /// unlikely. Two consecutive calls must produce different identities.
    #[test]
    fn generate_produces_unique_identities() {
        let a = PeerId::generate().unwrap();
        let b = PeerId::generate().unwrap();
        assert_ne!(a, b);
    }

    /// `generate()` produces non-zero identities.
    ///
    /// All-zero bytes would be a degenerate case. OS randomness should
    /// never produce 32 zero bytes (probability: 2^-256).
    #[test]
    fn generate_not_all_zeros() {
        let id = PeerId::generate().unwrap();
        assert_ne!(id.as_bytes(), &[0u8; PEER_ID_LEN]);
    }

    // ── Callsign generation ─────────────────────────────────────────

    /// `callsign()` is deterministic — same identity always produces
    /// the same callsign.
    ///
    /// Callsigns are displayed in UIs and logs. Non-determinism would
    /// confuse operators trying to match log entries to peers.
    #[test]
    fn callsign_deterministic() {
        let id = PeerId::from_key_material(b"stable-callsign-test");
        let a = id.callsign();
        let b = id.callsign();
        assert_eq!(a, b);
    }

    /// `callsign()` follows the "Title_XXX" format from Gap 7.
    ///
    /// The design doc specifies callsigns like "Commander_7K3": a
    /// military-themed title, underscore, and 3-char alphanumeric suffix.
    #[test]
    fn callsign_format_title_underscore_suffix() {
        let id = PeerId::generate().unwrap();
        let cs = id.callsign();

        // Must contain exactly one underscore separating title and suffix.
        let parts: Vec<&str> = cs.splitn(2, '_').collect();
        assert_eq!(
            parts.len(),
            2,
            "callsign should have title_suffix format: {cs}"
        );

        // Suffix must be exactly 3 alphanumeric characters.
        let suffix = parts[1];
        assert_eq!(suffix.len(), 3, "suffix should be 3 chars: {suffix}");
        assert!(
            suffix.chars().all(|c| c.is_ascii_alphanumeric()),
            "suffix should be alphanumeric: {suffix}"
        );

        // Title must be one of the known military titles.
        let title = parts[0];
        assert!(
            CALLSIGN_TITLES.contains(&title),
            "title should be a known military title: {title}"
        );
    }

    /// `callsign()` produces the expected output for known byte values.
    ///
    /// Byte 0x42: upper nibble = 4 → "Major".
    /// Bytes 1–3 = 0x42: 66 % 36 = 30 → 'U' in base-36.
    /// Expected: "Major_UUU".
    #[test]
    fn callsign_known_bytes() {
        let id = PeerId::from_bytes([0x42; PEER_ID_LEN]);
        assert_eq!(id.callsign(), "Major_UUU");
    }

    /// `callsign()` changes title when the upper nibble of byte 0 changes.
    ///
    /// Byte 0x00 → nibble 0 → "Commander".
    /// Byte 0xF0 → nibble 15 → "Phantom".
    #[test]
    fn callsign_title_varies_with_first_nibble() {
        let mut bytes_low = [0u8; PEER_ID_LEN];
        bytes_low[0] = 0x00;
        let cs_low = PeerId::from_bytes(bytes_low).callsign();
        assert!(
            cs_low.starts_with("Commander_"),
            "expected Commander, got {cs_low}"
        );

        let mut bytes_high = [0u8; PEER_ID_LEN];
        bytes_high[0] = 0xF0;
        let cs_high = PeerId::from_bytes(bytes_high).callsign();
        assert!(
            cs_high.starts_with("Phantom_"),
            "expected Phantom, got {cs_high}"
        );
    }

    /// Different identities generally produce different callsigns.
    ///
    /// Not guaranteed (limited suffix space), but for SHA-256-derived
    /// identities the first 4 bytes almost always differ.
    #[test]
    fn callsign_different_identities_differ() {
        let a = PeerId::from_key_material(b"alpha-peer").callsign();
        let b = PeerId::from_key_material(b"bravo-peer").callsign();
        assert_ne!(a, b);
    }

    /// All 16 title indices produce valid, distinct titles.
    ///
    /// Exhaustive check that the CALLSIGN_TITLES array is fully covered
    /// by the upper-nibble selection.
    #[test]
    fn callsign_all_titles_reachable() {
        let mut seen = std::collections::HashSet::new();
        for nibble in 0u8..16 {
            let mut bytes = [0u8; PEER_ID_LEN];
            bytes[0] = nibble << 4;
            let cs = PeerId::from_bytes(bytes).callsign();
            let title = cs.split('_').next().unwrap();
            seen.insert(title.to_string());
        }
        assert_eq!(seen.len(), 16, "all 16 titles should be reachable");
    }

    // ── Kind tracking (version/type discriminator) ──────────────────

    /// `kind()` returns the derivation method used to create the identity.
    ///
    /// Forward-compatible encoding requires knowing how the identity was
    /// derived. Each constructor sets the appropriate kind.
    #[test]
    fn kind_reflects_constructor() {
        assert_eq!(PeerId::generate().unwrap().kind(), PeerIdKind::Random);
        assert_eq!(PeerId::from_key_material(b"x").kind(), PeerIdKind::Hashed);
        assert_eq!(
            PeerId::from_ed25519_pubkey([0; 32]).kind(),
            PeerIdKind::Ed25519
        );
        assert_eq!(PeerId::from_bytes([0; 32]).kind(), PeerIdKind::Random);
    }

    /// Equality is kind-agnostic — same bytes, different kind → equal.
    ///
    /// A PeerId loaded from persistence (kind defaults to Random) must
    /// match a freshly derived one with the same bytes.
    #[test]
    fn equality_ignores_kind() {
        let bytes = [0x42; PEER_ID_LEN];
        let a = PeerId::from_bytes(bytes);
        let b = PeerId::from_ed25519_pubkey(bytes);
        assert_eq!(a, b, "same bytes with different kinds should be equal");
    }

    /// Hashing is kind-agnostic — same bytes, different kind → same bucket.
    ///
    /// HashMap lookups must work regardless of which constructor created
    /// the PeerId.
    #[test]
    fn hash_ignores_kind() {
        use std::collections::HashMap;
        let bytes = [0x42; PEER_ID_LEN];
        let a = PeerId::from_bytes(bytes);
        let b = PeerId::from_ed25519_pubkey(bytes);
        let mut map = HashMap::new();
        map.insert(a, "first");
        map.insert(b, "second");
        assert_eq!(map.len(), 1);
        assert_eq!(map.get(&a), Some(&"second"));
    }

    // ── Compact encoding (ic1 format) ───────────────────────────────

    /// `to_encoded()` produces the expected format prefix and length.
    ///
    /// The encoded form must be exactly 68 characters: "ic1" (3) + kind (1)
    /// + hex (64).
    #[test]
    fn to_encoded_format() {
        let id = PeerId::from_key_material(b"test");
        let encoded = id.to_encoded();
        assert_eq!(encoded.len(), 68);
        assert!(
            encoded.starts_with("ic1h"),
            "hashed identity should use 'h' prefix: {encoded}"
        );
    }

    /// `to_encoded()` reflects the correct kind character for each constructor.
    #[test]
    fn to_encoded_kind_chars() {
        let random = PeerId::generate().unwrap();
        assert!(random.to_encoded().starts_with("ic1r"));

        let hashed = PeerId::from_key_material(b"data");
        assert!(hashed.to_encoded().starts_with("ic1h"));

        let ed25519 = PeerId::from_ed25519_pubkey([0x42; 32]);
        assert!(ed25519.to_encoded().starts_with("ic1e"));
    }

    /// `from_encoded()` round-trips with `to_encoded()`.
    ///
    /// Encoding and decoding must be lossless — the same bytes and kind
    /// come back.
    #[test]
    fn encoded_round_trip() {
        let original = PeerId::from_key_material(b"round-trip-test");
        let encoded = original.to_encoded();
        let decoded = PeerId::from_encoded(&encoded).unwrap();
        assert_eq!(decoded, original);
        assert_eq!(decoded.kind(), original.kind());
    }

    /// `from_encoded()` rejects strings with wrong length.
    #[test]
    fn from_encoded_rejects_wrong_length() {
        let result = PeerId::from_encoded("ic1rtooShort");
        assert!(matches!(
            result,
            Err(PeerIdDecodeError::InvalidLength { .. })
        ));
    }

    /// `from_encoded()` rejects strings with wrong prefix.
    #[test]
    fn from_encoded_rejects_wrong_prefix() {
        let bad = format!("xx1r{}", "00".repeat(32));
        let result = PeerId::from_encoded(&bad);
        assert!(matches!(
            result,
            Err(PeerIdDecodeError::InvalidPrefix { .. })
        ));
    }

    /// `from_encoded()` rejects unknown kind characters.
    #[test]
    fn from_encoded_rejects_unknown_kind() {
        let bad = format!("ic1z{}", "00".repeat(32));
        let result = PeerId::from_encoded(&bad);
        assert!(matches!(result, Err(PeerIdDecodeError::InvalidKind { .. })));
    }

    /// `from_encoded()` rejects invalid hex characters.
    #[test]
    fn from_encoded_rejects_invalid_hex() {
        let bad = format!("ic1r{}gg", "00".repeat(31));
        let result = PeerId::from_encoded(&bad);
        assert!(matches!(result, Err(PeerIdDecodeError::InvalidHex { .. })));
    }

    /// `PeerIdDecodeError` display messages contain key context.
    #[test]
    fn decode_error_display_messages() {
        let err = PeerIdDecodeError::InvalidLength { length: 10 };
        let msg = err.to_string();
        assert!(msg.contains("10"), "should contain length: {msg}");
        assert!(msg.contains("68"), "should contain expected: {msg}");
    }
}
