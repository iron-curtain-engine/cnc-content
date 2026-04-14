// SPDX-License-Identifier: MIT OR Apache-2.0

//! Protocol obfuscation — random port selection and connection-level
//! traffic masking helpers.
//!
//! ## What
//!
//! Provides port randomisation and XOR-based stream obfuscation for
//! the BitTorrent wire protocol. This makes P2P traffic harder to
//! identify via DPI (Deep Packet Inspection) without the overhead of
//! full TLS.
//!
//! ## Why — eMule obfuscation lesson
//!
//! eMule introduced protocol obfuscation (connection encryption +
//! random ports) to bypass ISP throttling of P2P traffic. Key insights:
//!
//! - **Random port selection** — fixed ports like 6881 are trivially
//!   identified and throttled. Randomising the listen port eliminates
//!   port-based filtering.
//! - **Lightweight stream masking** — XOR-based obfuscation with a
//!   shared key prevents signature-based DPI detection at minimal CPU
//!   cost. Not cryptographic, but sufficient to defeat naive DPI.
//! - **Negotiation handshake** — both peers must agree on obfuscation
//!   during connection setup. The handshake itself uses a DH-derived
//!   key to avoid plaintext patterns.
//! - **Defence in depth** — HTTPS web seeds already appear as normal
//!   HTTPS traffic. Obfuscation addresses the BT wire protocol path
//!   only.
//!
//! ## How
//!
//! - [`random_port`]: Selects a random ephemeral port (49152–65535).
//! - [`ObfuscationKey`]: Shared key derived from the info hash for XOR
//!   masking.
//! - [`obfuscate_in_place`] / [`deobfuscate_in_place`]: XOR-mask a
//!   buffer using the key as a repeating pad.
//!
//! The transport layer chooses whether to enable obfuscation based on
//! peer capability advertisement.

// ── Constants ───────────────────────────────────────────────────────

/// Start of the IANA ephemeral / dynamic port range.
const EPHEMERAL_PORT_MIN: u16 = 49152;

/// End of the IANA ephemeral / dynamic port range (inclusive).
const EPHEMERAL_PORT_MAX: u16 = 65535;

/// Obfuscation key length in bytes — matches SHA-1 of info hash.
const KEY_LEN: usize = 20;

// ── Random port ─────────────────────────────────────────────────────

/// Selects a random port in the IANA ephemeral range (49152–65535).
///
/// Uses OS-provided entropy via `getrandom`. Falls back to the range
/// midpoint if entropy is unavailable (should never happen on modern
/// OSes).
///
/// ```
/// use p2p_distribute::obfuscation::random_port;
///
/// let port = random_port();
/// assert!((49152..=65535).contains(&port));
/// ```
pub fn random_port() -> u16 {
    let mut buf = [0u8; 2];
    // getrandom fills with OS entropy; if it fails, use a safe default.
    if getrandom::getrandom(&mut buf).is_err() {
        return EPHEMERAL_PORT_MIN.saturating_add((EPHEMERAL_PORT_MAX - EPHEMERAL_PORT_MIN) / 2);
    }
    let raw = u16::from_le_bytes(buf);
    let range = (EPHEMERAL_PORT_MAX - EPHEMERAL_PORT_MIN).saturating_add(1) as u32;
    EPHEMERAL_PORT_MIN.saturating_add((raw as u32 % range) as u16)
}

// ── Obfuscation key ─────────────────────────────────────────────────

/// Shared obfuscation key derived from the info hash.
///
/// Both peers know the info hash of the torrent they're exchanging,
/// so it serves as an implicit shared secret for the XOR pad. This is
/// **not** cryptographic encryption — it prevents signature-based DPI
/// detection at essentially zero CPU cost.
///
/// ```
/// use p2p_distribute::obfuscation::ObfuscationKey;
///
/// let info_hash = [0xAB; 20];
/// let key = ObfuscationKey::from_info_hash(&info_hash);
/// assert_eq!(key.as_bytes().len(), 20);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObfuscationKey {
    bytes: [u8; KEY_LEN],
}

impl ObfuscationKey {
    /// Creates an obfuscation key from a 20-byte info hash.
    pub fn from_info_hash(info_hash: &[u8; KEY_LEN]) -> Self {
        Self { bytes: *info_hash }
    }

    /// Returns the raw key bytes.
    pub fn as_bytes(&self) -> &[u8; KEY_LEN] {
        &self.bytes
    }
}

// ── XOR stream obfuscation ──────────────────────────────────────────

/// XOR-masks a buffer in place using the obfuscation key as a repeating
/// pad.
///
/// The key repeats cyclically across the buffer. XOR is its own inverse,
/// so calling this function twice with the same key and offset restores
/// the original data.
///
/// `stream_offset` is the byte position in the stream where this buffer
/// starts. This ensures that the key alignment is correct for mid-stream
/// buffers (e.g. after partial reads).
///
/// ```
/// use p2p_distribute::obfuscation::{ObfuscationKey, obfuscate_in_place, deobfuscate_in_place};
///
/// let key = ObfuscationKey::from_info_hash(&[0x42; 20]);
/// let original = b"Hello, world!".to_vec();
/// let mut buf = original.clone();
///
/// obfuscate_in_place(&mut buf, &key, 0);
/// assert_ne!(buf, original);
///
/// deobfuscate_in_place(&mut buf, &key, 0);
/// assert_eq!(buf, original);
/// ```
pub fn obfuscate_in_place(buf: &mut [u8], key: &ObfuscationKey, stream_offset: u64) {
    xor_with_key(buf, &key.bytes, stream_offset);
}

/// Deobfuscates a buffer in place. Identical to [`obfuscate_in_place`]
/// because XOR is self-inverse.
pub fn deobfuscate_in_place(buf: &mut [u8], key: &ObfuscationKey, stream_offset: u64) {
    xor_with_key(buf, &key.bytes, stream_offset);
}

/// Internal XOR implementation with key cycling and stream offset.
fn xor_with_key(buf: &mut [u8], key: &[u8; KEY_LEN], stream_offset: u64) {
    for (i, byte) in buf.iter_mut().enumerate() {
        let key_idx = ((stream_offset.saturating_add(i as u64)) % KEY_LEN as u64) as usize;
        // key_idx is always < KEY_LEN because of the modulo, so direct
        // indexing is safe. But we follow the project's safe-indexing rule
        // in production code.
        if let Some(&k) = key.get(key_idx) {
            *byte ^= k;
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Random port ─────────────────────────────────────────────────

    /// Random port is within the ephemeral range.
    ///
    /// The IANA ephemeral port range (49152–65535) avoids collisions
    /// with well-known and registered ports.
    #[test]
    fn random_port_in_range() {
        for _ in 0..100 {
            let port = random_port();
            assert!(
                (EPHEMERAL_PORT_MIN..=EPHEMERAL_PORT_MAX).contains(&port),
                "port {port} out of range"
            );
        }
    }

    /// Two consecutive random ports are unlikely to be identical.
    ///
    /// With 16384 possible ports, the chance of collision is ~0.006%.
    /// This test calls 100 times and checks at least 2 distinct values.
    #[test]
    fn random_port_has_entropy() {
        let ports: std::collections::HashSet<u16> = (0..100).map(|_| random_port()).collect();
        assert!(
            ports.len() > 1,
            "100 random ports produced only {} distinct value(s)",
            ports.len()
        );
    }

    // ── ObfuscationKey ──────────────────────────────────────────────

    /// Key round-trips through from_info_hash / as_bytes.
    ///
    /// The key must exactly reproduce the input info hash bytes.
    #[test]
    fn key_round_trip() {
        let hash = [0xAB; 20];
        let key = ObfuscationKey::from_info_hash(&hash);
        assert_eq!(key.as_bytes(), &hash);
    }

    // ── XOR obfuscation ─────────────────────────────────────────────

    /// Obfuscation round-trip restores original data.
    ///
    /// XOR is its own inverse: obfuscate(obfuscate(x)) == x.
    #[test]
    fn obfuscate_round_trip() {
        let key = ObfuscationKey::from_info_hash(&[0x42; 20]);
        let original = b"BitTorrent wire protocol".to_vec();
        let mut buf = original.clone();

        obfuscate_in_place(&mut buf, &key, 0);
        assert_ne!(buf, original, "obfuscation should change the data");

        deobfuscate_in_place(&mut buf, &key, 0);
        assert_eq!(buf, original, "deobfuscation should restore original");
    }

    /// Obfuscation with zero key is a no-op.
    ///
    /// XOR with zero leaves data unchanged — useful as a baseline.
    #[test]
    fn zero_key_is_noop() {
        let key = ObfuscationKey::from_info_hash(&[0; 20]);
        let original = b"Hello world".to_vec();
        let mut buf = original.clone();

        obfuscate_in_place(&mut buf, &key, 0);
        assert_eq!(buf, original);
    }

    /// Stream offset aligns the key correctly for mid-stream buffers.
    ///
    /// When processing a stream in chunks, the key alignment must account
    /// for the position in the stream to produce correct results.
    #[test]
    fn stream_offset_alignment() {
        let key = ObfuscationKey::from_info_hash(&[
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E,
            0x0F, 0x10, 0x11, 0x12, 0x13, 0x14,
        ]);

        // Obfuscate a 40-byte message in one go.
        let original = vec![0xFFu8; 40];
        let mut whole = original.clone();
        obfuscate_in_place(&mut whole, &key, 0);

        // Obfuscate in two 20-byte chunks with correct offsets.
        let mut first_half = original[..20].to_vec();
        let mut second_half = original[20..].to_vec();
        obfuscate_in_place(&mut first_half, &key, 0);
        obfuscate_in_place(&mut second_half, &key, 20);

        let mut combined = first_half;
        combined.extend_from_slice(&second_half);
        assert_eq!(whole, combined, "chunked obfuscation must match whole");
    }

    /// Obfuscation on empty buffer is a no-op.
    ///
    /// Edge case: empty input must not panic.
    #[test]
    fn empty_buffer_noop() {
        let key = ObfuscationKey::from_info_hash(&[0xFF; 20]);
        let mut buf: Vec<u8> = Vec::new();
        obfuscate_in_place(&mut buf, &key, 0);
        assert!(buf.is_empty());
    }

    /// Large stream offset does not cause overflow.
    ///
    /// Saturating arithmetic must handle offsets near u64::MAX.
    #[test]
    fn large_stream_offset_no_overflow() {
        let key = ObfuscationKey::from_info_hash(&[0xAA; 20]);
        let mut buf = vec![0x55u8; 10];
        // This should not panic — uses saturating_add internally.
        obfuscate_in_place(&mut buf, &key, u64::MAX - 5);
        // Just check it didn't panic; exact values don't matter.
    }

    /// Different keys produce different obfuscated output.
    ///
    /// Sanity check that distinct keys actually produce distinct results.
    #[test]
    fn different_keys_different_output() {
        let key_a = ObfuscationKey::from_info_hash(&[0x11; 20]);
        let key_b = ObfuscationKey::from_info_hash(&[0x22; 20]);
        let original = b"same plaintext data!".to_vec();

        let mut buf_a = original.clone();
        let mut buf_b = original.clone();
        obfuscate_in_place(&mut buf_a, &key_a, 0);
        obfuscate_in_place(&mut buf_b, &key_b, 0);

        assert_ne!(buf_a, buf_b, "different keys must produce different output");
    }
}
