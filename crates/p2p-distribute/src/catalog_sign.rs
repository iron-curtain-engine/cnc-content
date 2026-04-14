// SPDX-License-Identifier: MIT OR Apache-2.0

//! Catalog signing and verification — cryptographic integrity for group
//! manifests.
//!
//! ## What
//!
//! Provides a trait-based signature verification framework for
//! [`GroupManifest`] catalogs, plus a built-in HMAC-SHA256 implementation
//! using the `sha2` crate already in our dependency tree.
//!
//! ## Why
//!
//! The lib.rs roadmap calls for cryptographically signed catalog updates
//! so mirror nodes can verify manifest authenticity without trusting the
//! transport.  The existing `GroupManifest` stores opaque `signature` and
//! `signer_id` bytes and provides `canonical_bytes()` for deterministic
//! signing.  This module completes the picture with concrete sign/verify
//! logic.
//!
//! ## How
//!
//! - [`CatalogSigner`] trait — abstracts signing: given canonical bytes,
//!   produce a signature + signer ID.
//! - [`CatalogVerifier`] trait — abstracts verification: given canonical
//!   bytes + signature + signer ID, return pass/fail.
//! - [`HmacSha256Signer`] — built-in symmetric signer using HMAC-SHA256.
//!   Suitable for single-master groups where the master and all mirrors
//!   share a secret key.
//! - [`HmacSha256Verifier`] — corresponding verifier.
//!
//! For asymmetric signing (Ed25519), callers implement `CatalogSigner` /
//! `CatalogVerifier` with their own crypto library.  This crate avoids
//! adding an Ed25519 dependency because the project principle is "prefer
//! established crates" — callers choose their own.
//!
//! [`GroupManifest`]: crate::manifest::GroupManifest

use sha2::{Digest, Sha256};

use crate::manifest::GroupManifest;

// ── Constants ────────────────────────────────────────────────────────

/// HMAC-SHA256 output length in bytes.
const HMAC_OUTPUT_LEN: usize = 32;

/// HMAC block size for SHA-256 (64 bytes).
const HMAC_BLOCK_SIZE: usize = 64;

/// Inner padding byte for HMAC (0x36).
const IPAD: u8 = 0x36;

/// Outer padding byte for HMAC (0x5C).
const OPAD: u8 = 0x5C;

// ── Signer ID constant ──────────────────────────────────────────────

/// Signer ID prefix for HMAC-SHA256 signatures.
///
/// Stored in the manifest's `signer_id` field so verifiers know which
/// algorithm was used.  The format is `hmac-sha256:` followed by the
/// first 8 bytes of the key's SHA-256 hash (key fingerprint).
const HMAC_SIGNER_PREFIX: &[u8] = b"hmac-sha256:";

// ── Error ────────────────────────────────────────────────────────────

/// Errors from catalog signature operations.
#[derive(Debug, thiserror::Error)]
pub enum SignatureError {
    /// The manifest signature is missing (empty).
    #[error("manifest is unsigned")]
    Unsigned,

    /// The signature verification failed — the manifest may have been
    /// tampered with or the wrong key was used.
    #[error("signature verification failed: expected {expected_hex}, got {actual_hex}")]
    VerificationFailed {
        /// Hex-encoded expected signature (first 8 bytes).
        expected_hex: String,
        /// Hex-encoded actual signature (first 8 bytes).
        actual_hex: String,
    },

    /// The signer ID does not match the expected algorithm or key.
    #[error("signer mismatch: expected prefix '{expected_prefix}', got '{actual_prefix}'")]
    SignerMismatch {
        /// Expected signer ID prefix.
        expected_prefix: String,
        /// Actual signer ID prefix from the manifest.
        actual_prefix: String,
    },
}

// ── Traits ───────────────────────────────────────────────────────────

/// Trait for signing group manifests.
///
/// Implementors produce a signature over the manifest's canonical bytes
/// and return the signature + signer ID that will be stored in the
/// manifest.
pub trait CatalogSigner {
    /// Signs the canonical bytes, returning `(signature, signer_id)`.
    ///
    /// The `canonical_bytes` are produced by
    /// [`GroupManifest::canonical_bytes`] and are deterministic — the
    /// same manifest always produces the same bytes regardless of
    /// serialization format.
    fn sign(&self, canonical_bytes: &[u8]) -> (Vec<u8>, Vec<u8>);
}

/// Trait for verifying group manifest signatures.
///
/// Implementors check that the signature over canonical bytes is valid
/// for the given signer ID.
pub trait CatalogVerifier {
    /// Verifies the signature over the canonical bytes.
    ///
    /// Returns `Ok(())` if the signature is valid, or an error describing
    /// why verification failed.
    fn verify(
        &self,
        canonical_bytes: &[u8],
        signature: &[u8],
        signer_id: &[u8],
    ) -> Result<(), SignatureError>;
}

// ── HMAC-SHA256 implementation ───────────────────────────────────────

/// Computes HMAC-SHA256(key, message) per RFC 2104.
///
/// This is a minimal, correct HMAC implementation using only the `sha2`
/// crate.  It avoids adding an `hmac` crate dependency for a single
/// function.
fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; HMAC_OUTPUT_LEN] {
    // Step 1: If the key is longer than the block size, hash it.
    let mut padded_key = [0u8; HMAC_BLOCK_SIZE];
    if key.len() > HMAC_BLOCK_SIZE {
        let hash = Sha256::digest(key);
        let len = hash.len().min(HMAC_BLOCK_SIZE);
        padded_key
            .get_mut(..len)
            .unwrap_or(&mut [])
            .copy_from_slice(hash.get(..len).unwrap_or(&[]));
    } else {
        padded_key
            .get_mut(..key.len())
            .unwrap_or(&mut [])
            .copy_from_slice(key);
    }

    // Step 2: Inner hash = SHA256((key XOR ipad) || message).
    let mut inner_key = [0u8; HMAC_BLOCK_SIZE];
    for (i, byte) in inner_key.iter_mut().enumerate() {
        *byte = padded_key.get(i).copied().unwrap_or(0) ^ IPAD;
    }
    let mut inner_hasher = Sha256::new();
    inner_hasher.update(inner_key);
    inner_hasher.update(message);
    let inner_hash = inner_hasher.finalize();

    // Step 3: Outer hash = SHA256((key XOR opad) || inner_hash).
    let mut outer_key = [0u8; HMAC_BLOCK_SIZE];
    for (i, byte) in outer_key.iter_mut().enumerate() {
        *byte = padded_key.get(i).copied().unwrap_or(0) ^ OPAD;
    }
    let mut outer_hasher = Sha256::new();
    outer_hasher.update(outer_key);
    outer_hasher.update(inner_hash);
    let result = outer_hasher.finalize();

    let mut out = [0u8; HMAC_OUTPUT_LEN];
    out.copy_from_slice(&result);
    out
}

/// Computes a key fingerprint: first 8 bytes of SHA-256(key).
fn key_fingerprint(key: &[u8]) -> [u8; 8] {
    let hash = Sha256::digest(key);
    let mut fp = [0u8; 8];
    fp.copy_from_slice(hash.get(..8).unwrap_or(&[0; 8]));
    fp
}

/// Builds the signer ID for an HMAC-SHA256 key.
///
/// Format: `hmac-sha256:` + 8-byte key fingerprint (16 hex chars).
fn build_signer_id(key: &[u8]) -> Vec<u8> {
    let fp = key_fingerprint(key);
    let mut id = Vec::with_capacity(HMAC_SIGNER_PREFIX.len() + 16);
    id.extend_from_slice(HMAC_SIGNER_PREFIX);
    for b in &fp {
        // Hex-encode the fingerprint byte-by-byte using safe indexing.
        id.push(hex_nibble(b >> 4));
        id.push(hex_nibble(b & 0x0F));
    }
    id
}

/// Converts a nibble (0–15) to an ASCII hex character.
fn hex_nibble(n: u8) -> u8 {
    match n {
        0..=9 => b'0'.saturating_add(n),
        10..=15 => b'a'.saturating_add(n.saturating_sub(10)),
        _ => b'?',
    }
}

/// Hex-encodes the first `n` bytes of a slice for display.
fn hex_prefix(bytes: &[u8], n: usize) -> String {
    let take = bytes.len().min(n);
    let mut s = String::with_capacity(take * 2);
    for b in bytes.get(..take).unwrap_or(&[]) {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    s
}

// ── HmacSha256Signer ────────────────────────────────────────────────

/// Signs group manifests using HMAC-SHA256.
///
/// Suitable for single-master groups where the master and all mirrors
/// share a secret key.  For multi-party groups where the master should
/// prove identity without sharing a secret, use Ed25519 or another
/// asymmetric scheme by implementing [`CatalogSigner`] directly.
///
/// ```
/// use p2p_distribute::catalog_sign::{HmacSha256Signer, CatalogSigner};
///
/// let signer = HmacSha256Signer::new(b"shared-secret-key");
/// let (sig, id) = signer.sign(b"canonical manifest bytes");
/// assert_eq!(sig.len(), 32); // HMAC-SHA256 output
/// assert!(!id.is_empty());
/// ```
#[derive(Debug, Clone)]
pub struct HmacSha256Signer {
    key: Vec<u8>,
}

impl HmacSha256Signer {
    /// Creates a new HMAC-SHA256 signer with the given shared key.
    pub fn new(key: &[u8]) -> Self {
        Self { key: key.to_vec() }
    }
}

impl CatalogSigner for HmacSha256Signer {
    fn sign(&self, canonical_bytes: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let mac = hmac_sha256(&self.key, canonical_bytes);
        let signer_id = build_signer_id(&self.key);
        (mac.to_vec(), signer_id)
    }
}

// ── HmacSha256Verifier ──────────────────────────────────────────────

/// Verifies HMAC-SHA256 signatures on group manifests.
///
/// ```
/// use p2p_distribute::catalog_sign::{
///     HmacSha256Signer, HmacSha256Verifier,
///     CatalogSigner, CatalogVerifier,
/// };
///
/// let key = b"test-key";
/// let signer = HmacSha256Signer::new(key);
/// let verifier = HmacSha256Verifier::new(key);
///
/// let data = b"manifest canonical bytes";
/// let (sig, id) = signer.sign(data);
/// verifier.verify(data, &sig, &id).expect("should verify");
/// ```
#[derive(Debug, Clone)]
pub struct HmacSha256Verifier {
    key: Vec<u8>,
}

impl HmacSha256Verifier {
    /// Creates a new HMAC-SHA256 verifier with the given shared key.
    pub fn new(key: &[u8]) -> Self {
        Self { key: key.to_vec() }
    }
}

impl CatalogVerifier for HmacSha256Verifier {
    fn verify(
        &self,
        canonical_bytes: &[u8],
        signature: &[u8],
        signer_id: &[u8],
    ) -> Result<(), SignatureError> {
        // Check signer ID prefix.
        if !signer_id.starts_with(HMAC_SIGNER_PREFIX) {
            let actual = String::from_utf8_lossy(
                signer_id
                    .get(..HMAC_SIGNER_PREFIX.len().min(signer_id.len()))
                    .unwrap_or(b""),
            )
            .into_owned();
            return Err(SignatureError::SignerMismatch {
                expected_prefix: String::from_utf8_lossy(HMAC_SIGNER_PREFIX).into_owned(),
                actual_prefix: actual,
            });
        }

        // Compute expected HMAC.
        let expected = hmac_sha256(&self.key, canonical_bytes);

        // Constant-time comparison to prevent timing attacks.
        // We compare all bytes even if a mismatch is found early.
        if signature.len() != expected.len() {
            return Err(SignatureError::VerificationFailed {
                expected_hex: hex_prefix(&expected, 8),
                actual_hex: hex_prefix(signature, 8),
            });
        }

        let mut diff = 0u8;
        for (i, &expected_byte) in expected.iter().enumerate() {
            let actual_byte = signature.get(i).copied().unwrap_or(0);
            diff |= expected_byte ^ actual_byte;
        }

        if diff != 0 {
            return Err(SignatureError::VerificationFailed {
                expected_hex: hex_prefix(&expected, 8),
                actual_hex: hex_prefix(signature, 8),
            });
        }

        Ok(())
    }
}

// ── Convenience functions ────────────────────────────────────────────

/// Signs a manifest in place using the given signer.
///
/// Computes `canonical_bytes()`, signs them, and stores the signature
/// and signer ID in the manifest.
pub fn sign_manifest(manifest: &mut GroupManifest, signer: &dyn CatalogSigner) {
    let canonical = manifest.canonical_bytes();
    let (signature, signer_id) = signer.sign(&canonical);
    manifest.set_signature(signature, signer_id);
}

/// Verifies a manifest's signature using the given verifier.
///
/// Returns `Ok(())` if the signature is valid, or an error describing
/// the failure.
pub fn verify_manifest(
    manifest: &GroupManifest,
    verifier: &dyn CatalogVerifier,
) -> Result<(), SignatureError> {
    if !manifest.is_signed() {
        return Err(SignatureError::Unsigned);
    }
    let canonical = manifest.canonical_bytes();
    verifier.verify(&canonical, manifest.signature(), manifest.signer_id())
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::ContentEntry;
    use crate::network_id::NetworkId;

    /// Helper: builds a simple test manifest.
    fn test_manifest() -> GroupManifest {
        GroupManifest::builder(NetworkId::TEST)
            .version(1)
            .created_at(1700000000)
            .add_entry(ContentEntry::new("data/file_a.bin", [0xAA; 32], 1024).unwrap())
            .add_entry(ContentEntry::new("data/file_b.bin", [0xBB; 32], 2048).unwrap())
            .build()
            .unwrap()
    }

    // ── HMAC-SHA256 primitives ───────────────────────────────────────

    /// HMAC-SHA256 produces a 32-byte output.
    #[test]
    fn hmac_output_length() {
        let mac = hmac_sha256(b"key", b"message");
        assert_eq!(mac.len(), 32);
    }

    /// HMAC-SHA256 is deterministic.
    #[test]
    fn hmac_deterministic() {
        let a = hmac_sha256(b"key", b"message");
        let b = hmac_sha256(b"key", b"message");
        assert_eq!(a, b);
    }

    /// Different keys produce different HMACs.
    #[test]
    fn hmac_different_keys() {
        let a = hmac_sha256(b"key1", b"message");
        let b = hmac_sha256(b"key2", b"message");
        assert_ne!(a, b);
    }

    /// Different messages produce different HMACs.
    #[test]
    fn hmac_different_messages() {
        let a = hmac_sha256(b"key", b"message_a");
        let b = hmac_sha256(b"key", b"message_b");
        assert_ne!(a, b);
    }

    /// Long keys (> 64 bytes) are handled correctly.
    #[test]
    fn hmac_long_key() {
        let long_key = vec![0x42u8; 128];
        let a = hmac_sha256(&long_key, b"message");
        let b = hmac_sha256(&long_key, b"message");
        assert_eq!(a, b);

        // Short key should produce a different result.
        let short_result = hmac_sha256(b"short", b"message");
        assert_ne!(a, short_result);
    }

    /// Empty message and empty key produce a valid HMAC.
    #[test]
    fn hmac_empty_inputs() {
        let mac = hmac_sha256(b"", b"");
        assert_eq!(mac.len(), 32);
        // Known HMAC-SHA256("", "") value verification.
        // HMAC("", "") = SHA256(opad_key || SHA256(ipad_key || ""))
        assert_ne!(mac, [0u8; 32]); // Not all zeros.
    }

    // ── Key fingerprint ──────────────────────────────────────────────

    /// Key fingerprint is 8 bytes.
    #[test]
    fn fingerprint_length() {
        let fp = key_fingerprint(b"test-key");
        assert_eq!(fp.len(), 8);
    }

    /// Same key produces same fingerprint.
    #[test]
    fn fingerprint_deterministic() {
        let a = key_fingerprint(b"my-key");
        let b = key_fingerprint(b"my-key");
        assert_eq!(a, b);
    }

    // ── Signer ID ────────────────────────────────────────────────────

    /// Signer ID starts with the HMAC-SHA256 prefix.
    #[test]
    fn signer_id_has_prefix() {
        let id = build_signer_id(b"key");
        assert!(id.starts_with(HMAC_SIGNER_PREFIX));
    }

    /// Signer ID is deterministic.
    #[test]
    fn signer_id_deterministic() {
        let a = build_signer_id(b"key");
        let b = build_signer_id(b"key");
        assert_eq!(a, b);
    }

    // ── Sign and verify round-trip ───────────────────────────────────

    /// Sign-then-verify round-trip succeeds with the same key.
    #[test]
    fn sign_verify_round_trip() {
        let key = b"shared-group-secret";
        let signer = HmacSha256Signer::new(key);
        let verifier = HmacSha256Verifier::new(key);

        let mut manifest = test_manifest();
        sign_manifest(&mut manifest, &signer);

        assert!(manifest.is_signed());
        verify_manifest(&manifest, &verifier).expect("verification should succeed");
    }

    /// Verification fails with the wrong key.
    #[test]
    fn verify_fails_wrong_key() {
        let signer = HmacSha256Signer::new(b"correct-key");
        let wrong_verifier = HmacSha256Verifier::new(b"wrong-key");

        let mut manifest = test_manifest();
        sign_manifest(&mut manifest, &signer);

        let err = verify_manifest(&manifest, &wrong_verifier).unwrap_err();
        assert!(matches!(err, SignatureError::VerificationFailed { .. }));
    }

    /// Verification fails if the manifest is tampered with.
    #[test]
    fn verify_detects_tampering() {
        let key = b"secret";
        let signer = HmacSha256Signer::new(key);
        let verifier = HmacSha256Verifier::new(key);

        let mut manifest = test_manifest();
        sign_manifest(&mut manifest, &signer);

        // Tamper: build a new manifest with different content but
        // reuse the old signature.
        let mut tampered = GroupManifest::builder(NetworkId::TEST)
            .version(1)
            .created_at(1700000000)
            .add_entry(ContentEntry::new("data/evil.bin", [0xFF; 32], 9999).unwrap())
            .build()
            .unwrap();
        tampered.set_signature(manifest.signature().to_vec(), manifest.signer_id().to_vec());

        let err = verify_manifest(&tampered, &verifier).unwrap_err();
        assert!(matches!(err, SignatureError::VerificationFailed { .. }));
    }

    /// Verification of unsigned manifest returns `Unsigned` error.
    #[test]
    fn verify_unsigned_manifest() {
        let verifier = HmacSha256Verifier::new(b"key");
        let manifest = test_manifest();
        let err = verify_manifest(&manifest, &verifier).unwrap_err();
        assert!(matches!(err, SignatureError::Unsigned));
    }

    /// Verification with wrong signer type returns `SignerMismatch`.
    #[test]
    fn verify_signer_mismatch() {
        let verifier = HmacSha256Verifier::new(b"key");
        let mut manifest = test_manifest();
        manifest.set_signature(vec![0u8; 32], b"ed25519:fakepubkey".to_vec());

        let err = verify_manifest(&manifest, &verifier).unwrap_err();
        assert!(matches!(err, SignatureError::SignerMismatch { .. }));
    }

    /// Truncated signature fails verification.
    #[test]
    fn verify_truncated_signature() {
        let key = b"key";
        let signer = HmacSha256Signer::new(key);
        let verifier = HmacSha256Verifier::new(key);

        let mut manifest = test_manifest();
        sign_manifest(&mut manifest, &signer);

        // Truncate the signature to 16 bytes.
        let signer_id = manifest.signer_id().to_vec();
        let truncated = manifest.signature().get(..16).unwrap_or(&[]).to_vec();
        manifest.set_signature(truncated, signer_id);

        let err = verify_manifest(&manifest, &verifier).unwrap_err();
        assert!(matches!(err, SignatureError::VerificationFailed { .. }));
    }

    // ── Error display ────────────────────────────────────────────────

    /// All error variants produce meaningful display messages.
    #[test]
    fn error_display_all_variants() {
        let e1 = SignatureError::Unsigned;
        assert!(e1.to_string().contains("unsigned"));

        let e2 = SignatureError::VerificationFailed {
            expected_hex: "aabb".into(),
            actual_hex: "ccdd".into(),
        };
        let msg = e2.to_string();
        assert!(msg.contains("aabb"));
        assert!(msg.contains("ccdd"));

        let e3 = SignatureError::SignerMismatch {
            expected_prefix: "hmac-sha256:".into(),
            actual_prefix: "ed25519:".into(),
        };
        let msg = e3.to_string();
        assert!(msg.contains("hmac-sha256:"));
        assert!(msg.contains("ed25519:"));
    }

    // ── Hex helpers ──────────────────────────────────────────────────

    /// hex_prefix truncates correctly.
    #[test]
    fn hex_prefix_truncation() {
        let bytes = [0xAB, 0xCD, 0xEF, 0x01, 0x23];
        assert_eq!(hex_prefix(&bytes, 3), "abcdef");
        assert_eq!(hex_prefix(&bytes, 0), "");
        assert_eq!(hex_prefix(&[], 5), "");
    }

    /// hex_nibble covers all valid nibbles.
    #[test]
    fn hex_nibble_all_values() {
        assert_eq!(hex_nibble(0), b'0');
        assert_eq!(hex_nibble(9), b'9');
        assert_eq!(hex_nibble(10), b'a');
        assert_eq!(hex_nibble(15), b'f');
    }
}
