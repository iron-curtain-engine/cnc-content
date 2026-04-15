// SPDX-License-Identifier: MIT OR Apache-2.0

//! Peer credentials — signed, portable delegations from a group master
//! to authorized peers.
//!
//! ## What
//!
//! A [`PeerCredential`] is a signed statement: "PeerId X is authorized
//! as role R for NetworkId N, valid from time I until time E." The
//! master issues credentials to mirrors; mirrors present them during
//! handshake; downloaders verify them against the master's public key.
//!
//! ## Why
//!
//! A CDN group needs two things:
//!
//! 1. **What to serve** — handled by [`GroupManifest`](crate::manifest::GroupManifest)
//!    (signed content catalog).
//! 2. **Who is authorized to serve** — handled by `PeerCredential`.
//!
//! Without credentials, any peer could claim group membership. With
//! credentials, downloaders verify that a mirror was explicitly
//! authorized by the master. This enables trust-on-first-use for CDN
//! groups: the master's identity is the trust anchor, and credentials
//! are the delegation chain.
//!
//! ## How
//!
//! - The master creates a `PeerCredential` for each mirror.
//! - The master signs it with their [`CatalogSigner`](crate::catalog_sign::CatalogSigner).
//! - The mirror stores the credential and presents it during handshake
//!   (via [`HandshakeMessage`](crate::handshake::HandshakeMessage) or
//!   a post-handshake extension message).
//! - Connecting peers verify the credential with the master's
//!   [`CatalogVerifier`](crate::catalog_sign::CatalogVerifier).
//!
//! Credentials expire. The master periodically re-issues them. If a
//! mirror is compromised, the master simply stops re-issuing — the
//! credential expires naturally. For immediate revocation, the master
//! removes the peer from the [`GroupRoster`](crate::group::GroupRoster)
//! and peers check roster membership alongside credential validity.
//!
//! ## Canonical byte format (v1)
//!
//! Fixed 82 bytes, no parsing ambiguity:
//!
//! | Offset | Size | Field |
//! |--------|------|-------|
//! | 0 | 1 | Format version (`0x01`) |
//! | 1 | 32 | Subject PeerId (raw bytes) |
//! | 33 | 32 | NetworkId (raw bytes) |
//! | 65 | 1 | Role (Master=3, Admin=2, Mirror=1, Reader=0) |
//! | 66 | 8 | `issued_at` (u64 LE, Unix seconds) |
//! | 74 | 8 | `expires_at` (u64 LE, Unix seconds) |
//!
//! The signature and issuer_id are **not** part of canonical bytes —
//! they are bound to them externally (same pattern as
//! [`GroupManifest`](crate::manifest::GroupManifest)).
//!
//! ## Example
//!
//! ```
//! use p2p_distribute::credential::{PeerCredential, CredentialError};
//! use p2p_distribute::group::GroupRole;
//! use p2p_distribute::network_id::NetworkId;
//! use p2p_distribute::peer_id::PeerId;
//! use p2p_distribute::catalog_sign::HmacSha256Signer;
//!
//! let master_key = b"super-secret-master-key-material";
//! let signer = HmacSha256Signer::new(master_key);
//!
//! let mirror_id = PeerId::generate().unwrap();
//! let network = NetworkId::from_name("my-cdn-group");
//! let now = 1_700_000_000u64;
//! let one_week = 7 * 24 * 3600;
//!
//! let mut cred = PeerCredential::new(
//!     mirror_id,
//!     network,
//!     GroupRole::Mirror,
//!     now,
//!     now + one_week,
//! );
//!
//! cred.sign(&signer);
//! assert!(cred.is_signed());
//! assert!(!cred.is_expired(now + 3600));
//! assert!(cred.is_expired(now + one_week + 1));
//! ```

use crate::catalog_sign::{CatalogSigner, CatalogVerifier, SignatureError};
use crate::group::GroupRole;
use crate::network_id::NetworkId;
use crate::peer_id::PeerId;

// ── Constants ───────────────────────────────────────────────────────

/// Current credential format version.
const FORMAT_VERSION: u8 = 0x01;

/// Size of the canonical byte representation (fixed).
const CANONICAL_LEN: usize = 1 + 32 + 32 + 1 + 8 + 8; // 82 bytes

// ── Errors ──────────────────────────────────────────────────────────

/// Errors from credential verification.
#[derive(Debug, thiserror::Error)]
pub enum CredentialError {
    /// The credential has no signature.
    #[error("credential is unsigned")]
    Unsigned,

    /// The credential has expired.
    #[error("credential expired at {expired_at}, current time is {now}")]
    Expired {
        /// When the credential expired (Unix seconds).
        expired_at: u64,
        /// The current time that was checked against (Unix seconds).
        now: u64,
    },

    /// The credential is not yet valid.
    #[error("credential not valid until {valid_from}, current time is {now}")]
    NotYetValid {
        /// When the credential becomes valid (Unix seconds).
        valid_from: u64,
        /// The current time that was checked against (Unix seconds).
        now: u64,
    },

    /// The signature verification failed.
    #[error("signature verification failed: {source}")]
    VerificationFailed {
        /// The underlying signature error.
        #[from]
        source: SignatureError,
    },
}

// ── PeerCredential ──────────────────────────────────────────────────

/// A signed, portable delegation from a group master to a peer.
///
/// The master signs a fixed-size canonical representation. Mirrors
/// store the credential and present it to connecting peers. Downloaders
/// verify the signature to confirm the mirror was authorized by the
/// master.
#[derive(Debug, Clone)]
pub struct PeerCredential {
    /// The peer this credential is issued to.
    subject: PeerId,
    /// The group/network this credential authorizes participation in.
    network_id: NetworkId,
    /// The authorized role within the group.
    role: GroupRole,
    /// When this credential was issued (Unix seconds).
    issued_at: u64,
    /// When this credential expires (Unix seconds).
    expires_at: u64,
    /// Opaque signature bytes (empty if unsigned).
    signature: Vec<u8>,
    /// Issuer identification (e.g. signer fingerprint).
    issuer_id: Vec<u8>,
}

impl PeerCredential {
    /// Creates an unsigned credential.
    ///
    /// Call [`sign`](Self::sign) to attach a signature before
    /// distributing.
    pub fn new(
        subject: PeerId,
        network_id: NetworkId,
        role: GroupRole,
        issued_at: u64,
        expires_at: u64,
    ) -> Self {
        Self {
            subject,
            network_id,
            role,
            issued_at,
            expires_at,
            signature: Vec::new(),
            issuer_id: Vec::new(),
        }
    }

    // ── Signing ─────────────────────────────────────────────────────

    /// Signs the credential with the given signer.
    ///
    /// Overwrites any existing signature. The signer is typically the
    /// group master's key.
    pub fn sign(&mut self, signer: &dyn CatalogSigner) {
        let canonical = self.canonical_bytes();
        let (signature, issuer_id) = signer.sign(&canonical);
        self.signature = signature;
        self.issuer_id = issuer_id;
    }

    /// Verifies the credential's signature, expiry, and validity window.
    ///
    /// `now_secs` is the current Unix timestamp in seconds. The caller
    /// provides this to keep the credential module clock-agnostic
    /// (testable without mocking time).
    ///
    /// # Errors
    ///
    /// Returns [`CredentialError`] if the credential is unsigned,
    /// expired, not yet valid, or if the signature does not verify.
    pub fn verify(
        &self,
        verifier: &dyn CatalogVerifier,
        now_secs: u64,
    ) -> Result<(), CredentialError> {
        if !self.is_signed() {
            return Err(CredentialError::Unsigned);
        }
        if self.is_expired(now_secs) {
            return Err(CredentialError::Expired {
                expired_at: self.expires_at,
                now: now_secs,
            });
        }
        if now_secs < self.issued_at {
            return Err(CredentialError::NotYetValid {
                valid_from: self.issued_at,
                now: now_secs,
            });
        }
        let canonical = self.canonical_bytes();
        verifier.verify(&canonical, &self.signature, &self.issuer_id)?;
        Ok(())
    }

    // ── Canonical encoding ──────────────────────────────────────────

    /// Returns the fixed-size canonical byte representation for signing.
    ///
    /// 82 bytes: version(1) + subject(32) + network_id(32) + role(1) +
    /// issued_at(8) + expires_at(8). Signature and issuer_id are
    /// excluded.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(CANONICAL_LEN);
        buf.push(FORMAT_VERSION);
        buf.extend_from_slice(self.subject.as_bytes());
        buf.extend_from_slice(self.network_id.as_bytes());
        buf.push(role_to_byte(self.role));
        buf.extend_from_slice(&self.issued_at.to_le_bytes());
        buf.extend_from_slice(&self.expires_at.to_le_bytes());
        buf
    }

    // ── Queries ─────────────────────────────────────────────────────

    /// Whether this credential has a signature attached.
    pub fn is_signed(&self) -> bool {
        !self.signature.is_empty()
    }

    /// Whether this credential has expired at the given time.
    pub fn is_expired(&self, now_secs: u64) -> bool {
        now_secs >= self.expires_at
    }

    /// The peer this credential authorizes.
    pub fn subject(&self) -> &PeerId {
        &self.subject
    }

    /// The group this credential is scoped to.
    pub fn network_id(&self) -> &NetworkId {
        &self.network_id
    }

    /// The authorized role.
    pub fn role(&self) -> GroupRole {
        self.role
    }

    /// When this credential was issued (Unix seconds).
    pub fn issued_at(&self) -> u64 {
        self.issued_at
    }

    /// When this credential expires (Unix seconds).
    pub fn expires_at(&self) -> u64 {
        self.expires_at
    }

    /// The raw signature bytes (empty if unsigned).
    pub fn signature(&self) -> &[u8] {
        &self.signature
    }

    /// The issuer identification bytes (empty if unsigned).
    pub fn issuer_id(&self) -> &[u8] {
        &self.issuer_id
    }
}

// ── Role ↔ byte mapping ─────────────────────────────────────────────

/// Maps a [`GroupRole`] to its canonical byte representation.
///
/// Must match [`role_from_byte`] for round-trip correctness.
fn role_to_byte(role: GroupRole) -> u8 {
    match role {
        GroupRole::Master => 3,
        GroupRole::Admin => 2,
        GroupRole::Mirror => 1,
        GroupRole::Reader => 0,
    }
}

/// Maps a byte back to a [`GroupRole`], or `None` if unrecognized.
fn role_from_byte(byte: u8) -> Option<GroupRole> {
    match byte {
        3 => Some(GroupRole::Master),
        2 => Some(GroupRole::Admin),
        1 => Some(GroupRole::Mirror),
        0 => Some(GroupRole::Reader),
        _ => None,
    }
}

// ── Deserialization ─────────────────────────────────────────────────

/// Error when parsing a credential from canonical bytes.
#[derive(Debug, thiserror::Error)]
pub enum CredentialParseError {
    /// The byte slice is too short.
    #[error("credential too short: expected {CANONICAL_LEN} bytes, got {actual}")]
    TooShort {
        /// The actual length provided.
        actual: usize,
    },
    /// Unrecognized format version.
    #[error("unsupported credential version: {version}")]
    UnsupportedVersion {
        /// The version byte found.
        version: u8,
    },
    /// Unrecognized role byte.
    #[error("invalid role byte: {byte}")]
    InvalidRole {
        /// The role byte found.
        byte: u8,
    },
}

impl PeerCredential {
    /// Parses a credential from canonical bytes + signature + issuer_id.
    ///
    /// The `canonical` slice must be exactly [`CANONICAL_LEN`] bytes.
    /// Signature and issuer_id are attached separately (they are not
    /// part of the canonical encoding).
    pub fn from_canonical(
        canonical: &[u8],
        signature: Vec<u8>,
        issuer_id: Vec<u8>,
    ) -> Result<Self, CredentialParseError> {
        if canonical.len() < CANONICAL_LEN {
            return Err(CredentialParseError::TooShort {
                actual: canonical.len(),
            });
        }

        let version = canonical.first().copied().unwrap_or(0);
        if version != FORMAT_VERSION {
            return Err(CredentialParseError::UnsupportedVersion { version });
        }

        // Subject PeerId: bytes 1..33.
        let subject_bytes: [u8; 32] = canonical
            .get(1..33)
            .and_then(|s| s.try_into().ok())
            .unwrap_or([0u8; 32]);
        let subject = PeerId::from_bytes(subject_bytes);

        // NetworkId: bytes 33..65.
        let net_bytes: [u8; 32] = canonical
            .get(33..65)
            .and_then(|s| s.try_into().ok())
            .unwrap_or([0u8; 32]);
        let network_id = NetworkId::from_raw(net_bytes);

        // Role: byte 65.
        let role_byte = canonical.get(65).copied().unwrap_or(0xFF);
        let role = role_from_byte(role_byte)
            .ok_or(CredentialParseError::InvalidRole { byte: role_byte })?;

        // issued_at: bytes 66..74.
        let issued_at = canonical
            .get(66..74)
            .and_then(|s| <[u8; 8]>::try_from(s).ok())
            .map(u64::from_le_bytes)
            .unwrap_or(0);

        // expires_at: bytes 74..82.
        let expires_at = canonical
            .get(74..82)
            .and_then(|s| <[u8; 8]>::try_from(s).ok())
            .map(u64::from_le_bytes)
            .unwrap_or(0);

        Ok(Self {
            subject,
            network_id,
            role,
            issued_at,
            expires_at,
            signature,
            issuer_id,
        })
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog_sign::{HmacSha256Signer, HmacSha256Verifier};

    fn test_peer() -> PeerId {
        PeerId::generate().unwrap()
    }

    fn test_network() -> NetworkId {
        NetworkId::from_name("test-cdn")
    }

    fn test_signer() -> HmacSha256Signer {
        HmacSha256Signer::new(b"test-master-key")
    }

    fn test_verifier() -> HmacSha256Verifier {
        HmacSha256Verifier::new(b"test-master-key")
    }

    fn now() -> u64 {
        1_700_000_000
    }

    fn one_week() -> u64 {
        7 * 24 * 3600
    }

    // ── Construction ────────────────────────────────────────────────

    /// New credential starts unsigned.
    #[test]
    fn new_credential_is_unsigned() {
        let cred = PeerCredential::new(
            test_peer(),
            test_network(),
            GroupRole::Mirror,
            now(),
            now() + one_week(),
        );
        assert!(!cred.is_signed());
        assert!(cred.signature().is_empty());
        assert!(cred.issuer_id().is_empty());
    }

    /// Getters return construction values.
    #[test]
    fn getters_match_construction() {
        let peer = test_peer();
        let net = test_network();
        let cred = PeerCredential::new(peer, net, GroupRole::Mirror, 100, 200);
        assert_eq!(*cred.subject(), peer);
        assert_eq!(*cred.network_id(), net);
        assert_eq!(cred.role(), GroupRole::Mirror);
        assert_eq!(cred.issued_at(), 100);
        assert_eq!(cred.expires_at(), 200);
    }

    // ── Signing ─────────────────────────────────────────────────────

    /// Signing attaches a non-empty signature and issuer_id.
    #[test]
    fn sign_attaches_signature() {
        let mut cred = PeerCredential::new(
            test_peer(),
            test_network(),
            GroupRole::Mirror,
            now(),
            now() + one_week(),
        );
        cred.sign(&test_signer());
        assert!(cred.is_signed());
        assert!(!cred.signature().is_empty());
        assert!(!cred.issuer_id().is_empty());
    }

    /// Signed credential verifies with the correct key.
    #[test]
    fn verify_with_correct_key() {
        let mut cred = PeerCredential::new(
            test_peer(),
            test_network(),
            GroupRole::Mirror,
            now(),
            now() + one_week(),
        );
        cred.sign(&test_signer());
        assert!(cred.verify(&test_verifier(), now() + 3600).is_ok());
    }

    /// Signed credential fails verification with wrong key.
    #[test]
    fn verify_with_wrong_key_fails() {
        let mut cred = PeerCredential::new(
            test_peer(),
            test_network(),
            GroupRole::Mirror,
            now(),
            now() + one_week(),
        );
        cred.sign(&test_signer());
        let wrong_verifier = HmacSha256Verifier::new(b"wrong-key");
        let err = cred.verify(&wrong_verifier, now() + 3600).unwrap_err();
        assert!(matches!(err, CredentialError::VerificationFailed { .. }));
    }

    /// Unsigned credential fails verification.
    #[test]
    fn verify_unsigned_fails() {
        let cred = PeerCredential::new(
            test_peer(),
            test_network(),
            GroupRole::Mirror,
            now(),
            now() + one_week(),
        );
        let err = cred.verify(&test_verifier(), now() + 3600).unwrap_err();
        assert!(matches!(err, CredentialError::Unsigned));
    }

    // ── Expiry ──────────────────────────────────────────────────────

    /// Credential is not expired before expires_at.
    #[test]
    fn not_expired_before_deadline() {
        let cred = PeerCredential::new(
            test_peer(),
            test_network(),
            GroupRole::Mirror,
            now(),
            now() + one_week(),
        );
        assert!(!cred.is_expired(now() + 3600));
    }

    /// Credential is expired at exactly expires_at.
    #[test]
    fn expired_at_exact_deadline() {
        let cred = PeerCredential::new(
            test_peer(),
            test_network(),
            GroupRole::Mirror,
            now(),
            now() + one_week(),
        );
        assert!(cred.is_expired(now() + one_week()));
    }

    /// Credential is expired after expires_at.
    #[test]
    fn expired_after_deadline() {
        let cred = PeerCredential::new(
            test_peer(),
            test_network(),
            GroupRole::Mirror,
            now(),
            now() + one_week(),
        );
        assert!(cred.is_expired(now() + one_week() + 1));
    }

    /// Verify rejects expired credential even with valid signature.
    #[test]
    fn verify_expired_fails() {
        let mut cred = PeerCredential::new(
            test_peer(),
            test_network(),
            GroupRole::Mirror,
            now(),
            now() + one_week(),
        );
        cred.sign(&test_signer());
        let err = cred
            .verify(&test_verifier(), now() + one_week() + 1)
            .unwrap_err();
        assert!(matches!(err, CredentialError::Expired { .. }));
    }

    /// Verify rejects credential used before issued_at.
    #[test]
    fn verify_not_yet_valid_fails() {
        let mut cred = PeerCredential::new(
            test_peer(),
            test_network(),
            GroupRole::Mirror,
            now(),
            now() + one_week(),
        );
        cred.sign(&test_signer());
        let err = cred.verify(&test_verifier(), now() - 1).unwrap_err();
        assert!(matches!(err, CredentialError::NotYetValid { .. }));
    }

    // ── Canonical bytes ─────────────────────────────────────────────

    /// Canonical bytes have the expected fixed length.
    #[test]
    fn canonical_bytes_length() {
        let cred = PeerCredential::new(
            test_peer(),
            test_network(),
            GroupRole::Mirror,
            now(),
            now() + one_week(),
        );
        assert_eq!(cred.canonical_bytes().len(), CANONICAL_LEN);
    }

    /// Canonical bytes start with the format version.
    #[test]
    fn canonical_bytes_start_with_version() {
        let cred = PeerCredential::new(
            test_peer(),
            test_network(),
            GroupRole::Mirror,
            now(),
            now() + one_week(),
        );
        let bytes = cred.canonical_bytes();
        assert_eq!(bytes.first(), Some(&FORMAT_VERSION));
    }

    /// Canonical bytes are deterministic — same input, same output.
    #[test]
    fn canonical_bytes_deterministic() {
        let peer = test_peer();
        let net = test_network();
        let c1 = PeerCredential::new(peer, net, GroupRole::Mirror, 100, 200);
        let c2 = PeerCredential::new(peer, net, GroupRole::Mirror, 100, 200);
        assert_eq!(c1.canonical_bytes(), c2.canonical_bytes());
    }

    /// Different roles produce different canonical bytes.
    #[test]
    fn canonical_bytes_differ_by_role() {
        let peer = test_peer();
        let net = test_network();
        let c1 = PeerCredential::new(peer, net, GroupRole::Mirror, 100, 200);
        let c2 = PeerCredential::new(peer, net, GroupRole::Admin, 100, 200);
        assert_ne!(c1.canonical_bytes(), c2.canonical_bytes());
    }

    /// Signature is not included in canonical bytes.
    #[test]
    fn canonical_bytes_exclude_signature() {
        let mut cred = PeerCredential::new(
            test_peer(),
            test_network(),
            GroupRole::Mirror,
            now(),
            now() + one_week(),
        );
        let before = cred.canonical_bytes();
        cred.sign(&test_signer());
        let after = cred.canonical_bytes();
        assert_eq!(before, after);
    }

    // ── Round-trip (canonical → parse → canonical) ──────────────────

    /// Credential round-trips through canonical bytes.
    #[test]
    fn round_trip_canonical() {
        let mut cred = PeerCredential::new(
            test_peer(),
            test_network(),
            GroupRole::Admin,
            now(),
            now() + one_week(),
        );
        cred.sign(&test_signer());

        let canonical = cred.canonical_bytes();
        let parsed = PeerCredential::from_canonical(
            &canonical,
            cred.signature().to_vec(),
            cred.issuer_id().to_vec(),
        )
        .unwrap();

        assert_eq!(*parsed.subject(), *cred.subject());
        assert_eq!(*parsed.network_id(), *cred.network_id());
        assert_eq!(parsed.role(), cred.role());
        assert_eq!(parsed.issued_at(), cred.issued_at());
        assert_eq!(parsed.expires_at(), cred.expires_at());

        // Parsed credential verifies with the same key.
        assert!(parsed.verify(&test_verifier(), now() + 3600).is_ok());
    }

    /// All four roles round-trip correctly.
    #[test]
    fn round_trip_all_roles() {
        let roles = [
            GroupRole::Master,
            GroupRole::Admin,
            GroupRole::Mirror,
            GroupRole::Reader,
        ];
        for role in roles {
            let cred = PeerCredential::new(test_peer(), test_network(), role, 100, 200);
            let canonical = cred.canonical_bytes();
            let parsed =
                PeerCredential::from_canonical(&canonical, Vec::new(), Vec::new()).unwrap();
            assert_eq!(parsed.role(), role, "role {role:?} did not round-trip");
        }
    }

    // ── Parse errors ────────────────────────────────────────────────

    /// Too-short input is rejected.
    #[test]
    fn parse_too_short() {
        let err = PeerCredential::from_canonical(&[0x01; 10], Vec::new(), Vec::new()).unwrap_err();
        assert!(matches!(err, CredentialParseError::TooShort { .. }));
    }

    /// Wrong version byte is rejected.
    #[test]
    fn parse_wrong_version() {
        let mut buf = [0u8; CANONICAL_LEN];
        buf[0] = 0xFF; // Bad version.
        let err = PeerCredential::from_canonical(&buf, Vec::new(), Vec::new()).unwrap_err();
        assert!(matches!(
            err,
            CredentialParseError::UnsupportedVersion { version: 0xFF }
        ));
    }

    /// Invalid role byte is rejected.
    #[test]
    fn parse_invalid_role() {
        let cred = PeerCredential::new(test_peer(), test_network(), GroupRole::Mirror, 100, 200);
        let mut canonical = cred.canonical_bytes();
        // Corrupt the role byte (offset 65).
        if let Some(b) = canonical.get_mut(65) {
            *b = 0xFF;
        }
        let err = PeerCredential::from_canonical(&canonical, Vec::new(), Vec::new()).unwrap_err();
        assert!(matches!(
            err,
            CredentialParseError::InvalidRole { byte: 0xFF }
        ));
    }

    // ── Error display ───────────────────────────────────────────────

    /// Unsigned error message is descriptive.
    #[test]
    fn error_unsigned_display() {
        let msg = CredentialError::Unsigned.to_string();
        assert!(msg.contains("unsigned"), "got: {msg}");
    }

    /// Expired error includes timestamps.
    #[test]
    fn error_expired_display() {
        let msg = CredentialError::Expired {
            expired_at: 100,
            now: 200,
        }
        .to_string();
        assert!(msg.contains("100"), "got: {msg}");
        assert!(msg.contains("200"), "got: {msg}");
    }

    /// NotYetValid error includes timestamps.
    #[test]
    fn error_not_yet_valid_display() {
        let msg = CredentialError::NotYetValid {
            valid_from: 100,
            now: 50,
        }
        .to_string();
        assert!(msg.contains("100"), "got: {msg}");
        assert!(msg.contains("50"), "got: {msg}");
    }

    /// Parse error for too-short includes actual length.
    #[test]
    fn error_too_short_display() {
        let msg = CredentialParseError::TooShort { actual: 10 }.to_string();
        assert!(msg.contains("10"), "got: {msg}");
        assert!(msg.contains("82"), "got: {msg}");
    }

    /// Parse error for invalid role includes the byte value.
    #[test]
    fn error_invalid_role_display() {
        let msg = CredentialParseError::InvalidRole { byte: 0xAB }.to_string();
        assert!(msg.contains("171"), "got: {msg}"); // 0xAB = 171
    }

    // ── Tamper detection ────────────────────────────────────────────

    /// Modifying the subject after signing invalidates the signature.
    #[test]
    fn tamper_subject_detected() {
        let mut cred = PeerCredential::new(
            test_peer(),
            test_network(),
            GroupRole::Mirror,
            now(),
            now() + one_week(),
        );
        cred.sign(&test_signer());

        // Replace subject with a different peer.
        cred.subject = test_peer();

        let err = cred.verify(&test_verifier(), now() + 3600).unwrap_err();
        assert!(matches!(err, CredentialError::VerificationFailed { .. }));
    }

    /// Modifying the role after signing invalidates the signature.
    #[test]
    fn tamper_role_detected() {
        let mut cred = PeerCredential::new(
            test_peer(),
            test_network(),
            GroupRole::Mirror,
            now(),
            now() + one_week(),
        );
        cred.sign(&test_signer());

        // Escalate to Master.
        cred.role = GroupRole::Master;

        let err = cred.verify(&test_verifier(), now() + 3600).unwrap_err();
        assert!(matches!(err, CredentialError::VerificationFailed { .. }));
    }

    /// Modifying expires_at after signing invalidates the signature.
    #[test]
    fn tamper_expiry_detected() {
        let mut cred = PeerCredential::new(
            test_peer(),
            test_network(),
            GroupRole::Mirror,
            now(),
            now() + one_week(),
        );
        cred.sign(&test_signer());

        // Extend expiry.
        cred.expires_at = now() + one_week() * 52;

        let err = cred.verify(&test_verifier(), now() + 3600).unwrap_err();
        assert!(matches!(err, CredentialError::VerificationFailed { .. }));
    }
}
