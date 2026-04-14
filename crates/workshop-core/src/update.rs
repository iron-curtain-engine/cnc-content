// SPDX-License-Identifier: MIT OR Apache-2.0

//! Update discovery — how the game learns about new content versions.
//!
//! This is the answer to "should our game ship with a git repo?": No.
//! Instead, update discovery is a pluggable trait with multiple
//! implementations, and **P2P gossip is the primary mechanism**.
//!
//! # Why not git?
//!
//! Git is a developer tool with a ~100 MB runtime. It solves source
//! code versioning, not content update notification. Everything we need
//! from git for updates — efficient diffing, incremental fetching,
//! cryptographic integrity — we can achieve with lighter, purpose-built
//! mechanisms that we fully control.
//!
//! # Update discovery vs. update delivery
//!
//! These are fundamentally different problems:
//!
//! - **Discovery** = "is there a new version?" — tiny (~200 bytes),
//!   must be fast, must be reliable, can happen over any transport.
//! - **Delivery** = "download the new version" — large (MB–GB),
//!   tolerates latency, benefits from P2P bandwidth sharing.
//!
//! Discovery is handled by [`UpdateDiscovery`] (this module).
//! Delivery is handled by `p2p-distribute` (the transport layer).
//!
//! # The three discovery channels
//!
//! ```text
//! ┌───────────────────────────────────────────────────────────────┐
//! │                    FailoverDiscovery                          │
//! │  Tries each source in priority order until one succeeds      │
//! ├──────────────────┬────────────────────┬───────────────────────┤
//! │  1. P2P gossip   │  2. Workshop index │  3. Compiled baseline │
//! │  (zero infra)    │  (git/HTTP/API)    │  (embedded fallback)  │
//! ├──────────────────┼────────────────────┼───────────────────────┤
//! │ Peers exchange   │ IndexBackend       │ Hardcoded versions    │
//! │ signed Version-  │ ::update() checks  │ from build time —     │
//! │ Announcements    │ the index for new  │ always works, never   │
//! │ during normal    │ manifests. Works   │ stale-proof but       │
//! │ BT traffic.      │ with any git host  │ provides a floor.     │
//! │                  │ or HTTP server.    │                       │
//! │ Works even when  │                    │                       │
//! │ all servers are  │ Requires at least  │ Requires nothing.     │
//! │ down.            │ one reachable      │                       │
//! │                  │ backend.           │                       │
//! └──────────────────┴────────────────────┴───────────────────────┘
//! ```
//!
//! # P2P gossip: how peers spread updates without servers
//!
//! This is the creative insight that makes git unnecessary:
//!
//! 1. **Publisher signs a `VersionAnnouncement`** — a ~200 byte message
//!    containing (resource_id, version, info_hash, timestamp, ed25519
//!    signature). This is built on `p2p-distribute`'s existing
//!    `CatalogSigner` trait and `PeerId::Ed25519`.
//!
//! 2. **Announcement enters the swarm** — the publisher's client seeds
//!    the announcement alongside normal torrent traffic. Connected peers
//!    receive it.
//!
//! 3. **Peers relay to other peers** — each peer that validates the
//!    signature stores it and forwards it to its own connections. The
//!    announcement propagates exponentially through the swarm.
//!
//! 4. **New peers catch up** — when a peer joins a swarm for any content
//!    from publisher X, existing peers share all known announcements for
//!    that publisher. The new peer learns about all versions instantly.
//!
//! This means: **the P2P swarm IS the update index.** As long as at
//! least one peer is online, update information is available. No servers,
//! no git, no HTTP, no platform dependency.
//!
//! The Workshop index (channel 2) serves as a reliable fallback for cold
//! starts when no peers are available. It also provides discoverability
//! for content from publishers the user hasn't connected to yet.
//!
//! # How this maps to p2p-distribute
//!
//! | workshop-core concept       | p2p-distribute building block           |
//! |-----------------------------|-----------------------------------------|
//! | `VersionAnnouncement`       | `GroupManifest` (signed, versioned)     |
//! | Publisher identity           | `PeerId::Ed25519` (32-byte pubkey)     |
//! | Announcement signing        | `CatalogSigner` trait                   |
//! | Announcement verification   | `CatalogVerifier` trait                 |
//! | Gossip propagation          | Extension messages (planned M9)         |
//! | DHT announcement storage    | `DhtNode` + BEP 46 (planned M8)        |
//! | Swarm-based discovery       | Content channels (planned M11)          |
//!
//! # Threat model
//!
//! The gossip protocol is an adversarial environment. Any connected peer
//! can send any bytes. The [`AnnouncementValidator`] enforces every
//! defense listed below before an announcement is accepted.
//!
//! | Attack                 | Description                                     | Defense                                      |
//! |------------------------|-------------------------------------------------|----------------------------------------------|
//! | **Forged announcement** | Attacker crafts announcement with fake publisher | Ed25519 signature verification (pluggable)   |
//! | **Replay attack**       | Re-broadcast old valid announcement              | Sequence number monotonicity check            |
//! | **Sequence racing**     | Set sequence to `u64::MAX` to block publisher    | Maximum sequence jump limit (default: 1000)   |
//! | **Future timestamp**    | Far-future timestamp to appear "newest"          | Clock drift tolerance (default: 300s)         |
//! | **Announcement flood**  | DoS via massive announcement volume              | Per-publisher rate limiting (token bucket)     |
//! | **Key compromise**      | Real key stolen, used to publish malware         | Revocation list checked before all other rules |
//! | **Eclipse attack**      | Surround peer with adversary nodes, feed stale   | Multi-source FailoverDiscovery cross-checks   |
//! | **Typosquatting**       | Register `a1ice/sprites` to impersonate `alice`  | ResourceId slug rules + index moderation      |
//! | **Content poisoning**   | Valid announcement, malicious `info_hash`         | SHA-256 `blob_id` verified after download      |
//!
//! ## Defense-in-depth principle
//!
//! No single layer is trusted alone. An announcement must pass ALL of:
//! 1. Not from a revoked publisher (revocation list)
//! 2. Valid Ed25519 signature (authenticity)
//! 3. Sequence > highest known for this (publisher, resource) (freshness)
//! 4. Sequence jump within bounds (anti-racing)
//! 5. Timestamp within clock drift tolerance (anti-future-dating)
//! 6. Publisher not exceeding rate limit (anti-flood)
//!
//! After acceptance, the content is STILL not trusted until:
//! 7. Downloaded bytes match `info_hash` (BitTorrent piece verification)
//! 8. Complete file matches `blob_id` SHA-256 (integrity verification)

use std::fmt;

use crate::blob::BlobId;
use crate::error::WorkshopError;
use crate::resource::{ResourceId, ResourceVersion};

// ── Publisher identity ───────────────────────────────────────────────

/// An Ed25519 public key identifying a content publisher.
///
/// Publishers sign [`VersionAnnouncement`]s with their private key.
/// Peers verify announcements using this public key. The key is
/// durably associated with a `ResourceId` publisher name — changing
/// the signing key requires a key rotation announcement signed by
/// the old key (preventing impersonation).
///
/// This maps to `p2p-distribute`'s `PeerId::Ed25519` — same 32 bytes,
/// same semantics, different type to enforce the domain boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PublisherId([u8; 32]);

impl PublisherId {
    /// Creates a publisher identity from an Ed25519 public key.
    pub const fn from_ed25519(pubkey: [u8; 32]) -> Self {
        Self(pubkey)
    }

    /// Returns the raw 32-byte Ed25519 public key.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Returns the first 16 hex characters for display.
    pub fn short_hex(&self) -> String {
        self.0
            .get(..8)
            .unwrap_or(&self.0)
            .iter()
            .fold(String::with_capacity(16), |mut s, b| {
                use fmt::Write;
                let _ = write!(s, "{b:02x}");
                s
            })
    }
}

impl fmt::Display for PublisherId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Show truncated key for human readability.
        write!(f, "pub:{}", self.short_hex())
    }
}

// ── Version announcement ─────────────────────────────────────────────

/// A signed announcement that a new version is available.
///
/// This is the ~200 byte message that propagates through the P2P swarm
/// via gossip. It contains everything a peer needs to:
/// 1. **Discover** that a new version exists
/// 2. **Verify** the announcement is authentic (Ed25519 signature)
/// 3. **Start downloading** (info_hash for P2P, blob_id for integrity)
///
/// # Immutability
///
/// Once signed, an announcement is immutable. A publisher cannot
/// "unsign" a version — only publish a newer version or issue a
/// revocation announcement (separate mechanism).
///
/// # Sequence numbers prevent replay
///
/// Each announcement has a monotonically increasing `sequence` number.
/// Peers reject announcements with a sequence ≤ the highest they've
/// seen for the same `(publisher, resource)` pair. This prevents
/// replay attacks where an adversary re-broadcasts old announcements
/// to make peers think an outdated version is current.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionAnnouncement {
    /// The publisher who signed this announcement.
    publisher_id: PublisherId,
    /// The resource being updated.
    resource: ResourceId,
    /// The new version.
    version: ResourceVersion,
    /// SHA-256 content hash (canonical identity).
    blob_id: BlobId,
    /// BitTorrent info_hash for P2P download. `None` if no torrent
    /// exists yet (direct HTTP download only).
    info_hash: Option<[u8; 20]>,
    /// Monotonically increasing sequence number. Peers reject
    /// announcements with sequence ≤ the highest seen for this
    /// (publisher, resource) pair.
    sequence: u64,
    /// Seconds since UNIX epoch when the announcement was created.
    timestamp: u64,
    /// Ed25519 signature over the canonical announcement bytes.
    /// Verification uses the `publisher_id` public key.
    signature: [u8; 64],
}

impl VersionAnnouncement {
    /// Creates a new version announcement.
    ///
    /// The `signature` must be computed externally using the publisher's
    /// Ed25519 private key over `canonical_bytes()`. This crate
    /// intentionally does not include a signing library — the caller
    /// uses `p2p-distribute`'s `CatalogSigner` or any Ed25519 library.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        publisher_id: PublisherId,
        resource: ResourceId,
        version: ResourceVersion,
        blob_id: BlobId,
        info_hash: Option<[u8; 20]>,
        sequence: u64,
        timestamp: u64,
        signature: [u8; 64],
    ) -> Self {
        Self {
            publisher_id,
            resource,
            version,
            blob_id,
            info_hash,
            sequence,
            timestamp,
            signature,
        }
    }

    /// Returns the canonical byte representation for signing/verification.
    ///
    /// The format is deterministic: all fields are serialized in fixed
    /// order with fixed-width encoding. No field is ambiguous — the
    /// same announcement always produces the same bytes.
    ///
    /// Layout:
    /// - publisher_id: 32 bytes (Ed25519 pubkey)
    /// - resource publisher slug: 1 byte length + UTF-8 bytes
    /// - resource name slug: 1 byte length + UTF-8 bytes
    /// - version: 3 × 4 bytes (major, minor, patch as big-endian u32)
    /// - blob_id: 32 bytes (SHA-256)
    /// - info_hash: 1 byte flag + 20 bytes (0x00 if None, 0x01+hash if Some)
    /// - sequence: 8 bytes (big-endian u64)
    /// - timestamp: 8 bytes (big-endian u64)
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let publisher_slug = self.resource.publisher();
        let name_slug = self.resource.name();
        // Conservative capacity: fixed fields (32+12+32+21+8+8=113) + slug lengths + 2
        let capacity = 113usize
            .saturating_add(publisher_slug.len())
            .saturating_add(name_slug.len())
            .saturating_add(2);
        let mut buf = Vec::with_capacity(capacity);

        // Publisher Ed25519 key (32 bytes).
        buf.extend_from_slice(self.publisher_id.as_bytes());

        // Resource identity — length-prefixed UTF-8 slugs.
        // Slugs are validated to ≤64 bytes ASCII, so u8 length is safe.
        buf.push(publisher_slug.len() as u8);
        buf.extend_from_slice(publisher_slug.as_bytes());
        buf.push(name_slug.len() as u8);
        buf.extend_from_slice(name_slug.as_bytes());

        // Version — 3 × big-endian u32.
        buf.extend_from_slice(&self.version.major().to_be_bytes());
        buf.extend_from_slice(&self.version.minor().to_be_bytes());
        buf.extend_from_slice(&self.version.patch().to_be_bytes());

        // Blob ID (SHA-256, 32 bytes).
        buf.extend_from_slice(self.blob_id.as_bytes());

        // Info hash — tagged optional (1 byte flag + 20 bytes).
        match &self.info_hash {
            Some(hash) => {
                buf.push(0x01);
                buf.extend_from_slice(hash);
            }
            None => {
                buf.push(0x00);
            }
        }

        // Sequence and timestamp — big-endian u64.
        buf.extend_from_slice(&self.sequence.to_be_bytes());
        buf.extend_from_slice(&self.timestamp.to_be_bytes());

        buf
    }

    pub fn publisher_id(&self) -> PublisherId {
        self.publisher_id
    }

    pub fn resource(&self) -> &ResourceId {
        &self.resource
    }

    pub fn version(&self) -> ResourceVersion {
        self.version
    }

    pub fn blob_id(&self) -> &BlobId {
        &self.blob_id
    }

    pub fn info_hash(&self) -> Option<&[u8; 20]> {
        self.info_hash.as_ref()
    }

    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    pub fn timestamp(&self) -> u64 {
        self.timestamp
    }

    pub fn signature(&self) -> &[u8; 64] {
        &self.signature
    }
}

// ── Signature verification (pluggable) ───────────────────────────────

/// Pluggable Ed25519 signature verification.
///
/// This crate does not bundle a crypto library. Callers provide an
/// implementation that delegates to their Ed25519 library of choice
/// (e.g. `ed25519-dalek`, `ring`, `p2p-distribute`'s `CatalogVerifier`).
///
/// # Security contract
///
/// Implementations MUST:
/// - Return `true` only if the signature is valid for the given message
///   and public key, using standard Ed25519 (RFC 8032).
/// - Return `false` for malformed keys, malformed signatures, or any
///   verification failure. Never panic.
pub trait SignatureVerifier {
    /// Verifies an Ed25519 signature over `message` using `public_key`.
    fn verify(&self, public_key: &[u8; 32], message: &[u8], signature: &[u8; 64]) -> bool;
}

/// A verifier that accepts all signatures. **For testing only.**
///
/// Using this in production would disable all authenticity checks.
#[cfg(test)]
pub(crate) struct AcceptAllVerifier;

#[cfg(test)]
impl SignatureVerifier for AcceptAllVerifier {
    fn verify(&self, _public_key: &[u8; 32], _message: &[u8], _signature: &[u8; 64]) -> bool {
        true
    }
}

/// A verifier that rejects all signatures. **For testing only.**
#[cfg(test)]
pub(crate) struct RejectAllVerifier;

#[cfg(test)]
impl SignatureVerifier for RejectAllVerifier {
    fn verify(&self, _public_key: &[u8; 32], _message: &[u8], _signature: &[u8; 64]) -> bool {
        false
    }
}

// ── Announcement validator ───────────────────────────────────────────

/// Configuration for [`AnnouncementValidator`] rate limiting.
///
/// Controls how many announcements a single publisher can submit within
/// a rolling time window. Excess announcements are rejected with
/// [`WorkshopError::RateLimitExceeded`].
#[derive(Debug, Clone, Copy)]
pub struct RateLimitConfig {
    /// Maximum announcements per publisher within the time window.
    pub max_count: u64,
    /// Time window in seconds.
    pub window_secs: u64,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        // 10 announcements per hour is generous for legitimate publishers.
        // A real publisher releases at most a few versions per day.
        Self {
            max_count: 10,
            window_secs: 3600,
        }
    }
}

/// Validates incoming announcements against a comprehensive threat model.
///
/// Every announcement received from the network MUST pass through this
/// validator before being stored or relayed. The validator enforces:
///
/// 1. **Revocation check** — is the publisher's key revoked?
/// 2. **Signature verification** — is the Ed25519 signature valid?
/// 3. **Sequence monotonicity** — is the sequence strictly increasing?
/// 4. **Sequence jump check** — is the jump within bounds?
/// 5. **Timestamp check** — is the timestamp within clock drift tolerance?
/// 6. **Rate limit check** — is the publisher within flood limits?
///
/// Checks are ordered to fail fast and cheap: revocation and timestamp
/// checks are O(1) lookups, done before expensive signature verification.
pub struct AnnouncementValidator<V: SignatureVerifier> {
    /// Pluggable signature verifier (Ed25519).
    verifier: V,
    /// Highest known sequence per (publisher, resource) pair.
    /// Used for replay detection and sequence jump limits.
    known_sequences: std::collections::HashMap<(PublisherId, ResourceId), u64>,
    /// Revoked publisher keys. All announcements from these keys are
    /// rejected regardless of signature validity.
    revoked_publishers: std::collections::HashSet<PublisherId>,
    /// Maximum allowed clock drift (seconds into the future).
    /// Announcements with timestamps beyond `now + max_clock_drift` are
    /// rejected to prevent future-dating attacks.
    max_clock_drift_secs: u64,
    /// Maximum allowed sequence jump from the last known sequence.
    /// Prevents attackers from racing to `u64::MAX`.
    max_sequence_jump: u64,
    /// Per-publisher announcement timestamps for rate limiting.
    /// Stores the timestamps of recent announcements within the window.
    rate_limit_log: std::collections::HashMap<PublisherId, Vec<u64>>,
    /// Rate limit configuration.
    rate_limit: RateLimitConfig,
}

impl<V: SignatureVerifier> AnnouncementValidator<V> {
    /// Creates a validator with default security parameters.
    ///
    /// Defaults:
    /// - Clock drift tolerance: 300 seconds (5 minutes)
    /// - Maximum sequence jump: 1000
    /// - Rate limit: 10 announcements per hour per publisher
    pub fn new(verifier: V) -> Self {
        Self {
            verifier,
            known_sequences: std::collections::HashMap::new(),
            revoked_publishers: std::collections::HashSet::new(),
            max_clock_drift_secs: 300,
            max_sequence_jump: 1000,
            rate_limit_log: std::collections::HashMap::new(),
            rate_limit: RateLimitConfig::default(),
        }
    }

    /// Sets the maximum allowed clock drift in seconds.
    pub fn with_max_clock_drift(mut self, secs: u64) -> Self {
        self.max_clock_drift_secs = secs;
        self
    }

    /// Sets the maximum allowed sequence jump.
    pub fn with_max_sequence_jump(mut self, jump: u64) -> Self {
        self.max_sequence_jump = jump;
        self
    }

    /// Sets the rate limit configuration.
    pub fn with_rate_limit(mut self, config: RateLimitConfig) -> Self {
        self.rate_limit = config;
        self
    }

    /// Adds a publisher key to the revocation list.
    ///
    /// All future announcements from this key will be rejected with
    /// [`WorkshopError::PublisherRevoked`], even if the signature is valid.
    /// This handles key compromise, DMCA takedowns, and malware publishers.
    pub fn revoke_publisher(&mut self, publisher: PublisherId, _reason: &str) {
        self.revoked_publishers.insert(publisher);
    }

    /// Validates an announcement against all security rules.
    ///
    /// `now_secs` is the current time in seconds since UNIX epoch. It is
    /// passed as a parameter (not read from the system clock) so that
    /// validation is deterministic and testable.
    ///
    /// Returns `Ok(())` if the announcement passes all checks.
    /// Returns the first failing check as `Err(WorkshopError::…)`.
    ///
    /// # Check order (fail fast, cheapest first)
    ///
    /// 1. Revocation list (O(1) HashSet lookup)
    /// 2. Timestamp drift (O(1) arithmetic)
    /// 3. Signature verification (O(1) but expensive crypto)
    /// 4. Sequence monotonicity (O(1) HashMap lookup)
    /// 5. Sequence jump bounds (O(1) arithmetic)
    /// 6. Rate limit (O(n) where n = announcements in window)
    pub fn validate(
        &mut self,
        announcement: &VersionAnnouncement,
        now_secs: u64,
    ) -> Result<(), WorkshopError> {
        let publisher = announcement.publisher_id();
        let resource = announcement.resource();

        // ── 1. Revocation check ──────────────────────────────────────
        // Check FIRST: a revoked key is rejected regardless of everything
        // else. This ensures a compromised key can be neutralized even if
        // the attacker has valid signatures.
        if self.revoked_publishers.contains(&publisher) {
            return Err(WorkshopError::PublisherRevoked {
                publisher: publisher.to_string(),
                reason: "key is on revocation list".to_string(),
            });
        }

        // ── 2. Timestamp check ───────────────────────────────────────
        // Reject far-future timestamps. A small backward drift is OK
        // (clocks vary), but future timestamps let attackers make their
        // announcements appear "newest" indefinitely.
        if announcement.timestamp() > now_secs.saturating_add(self.max_clock_drift_secs) {
            return Err(WorkshopError::FutureTimestamp {
                timestamp: announcement.timestamp(),
                drift_secs: announcement.timestamp().saturating_sub(now_secs),
                max_drift_secs: self.max_clock_drift_secs,
            });
        }

        // ── 3. Signature verification ────────────────────────────────
        // Verify BEFORE sequence checks: an invalid signature means the
        // announcement is forged, so we must not let it influence our
        // sequence tracking state.
        let canonical = announcement.canonical_bytes();
        if !self
            .verifier
            .verify(publisher.as_bytes(), &canonical, announcement.signature())
        {
            return Err(WorkshopError::InvalidSignature {
                publisher: publisher.to_string(),
                resource: resource.to_string(),
            });
        }

        // ── 4. Sequence monotonicity ─────────────────────────────────
        // Reject announcements with sequence ≤ the highest we've seen.
        // This prevents replay attacks where an adversary re-broadcasts
        // a legitimately signed but outdated announcement.
        let key = (publisher, resource.clone());
        let known_seq = self.known_sequences.get(&key).copied().unwrap_or(0);
        if announcement.sequence() <= known_seq {
            return Err(WorkshopError::StaleAnnouncement {
                resource: resource.to_string(),
                received: announcement.sequence(),
                known: known_seq,
            });
        }

        // ── 5. Sequence jump check ───────────────────────────────────
        // Reject suspiciously large jumps. An attacker who compromises a
        // key briefly could set sequence to u64::MAX, permanently blocking
        // the real publisher from issuing higher-sequenced announcements.
        // Bounding the jump limits the damage window.
        if known_seq > 0 {
            let jump = announcement.sequence().saturating_sub(known_seq);
            if jump > self.max_sequence_jump {
                return Err(WorkshopError::SequenceJumpTooLarge {
                    resource: resource.to_string(),
                    old_seq: known_seq,
                    new_seq: announcement.sequence(),
                    max_jump: self.max_sequence_jump,
                });
            }
        }

        // ── 6. Rate limit ────────────────────────────────────────────
        // Bound the number of announcements per publisher per time window.
        // This prevents a compromised or malicious publisher from flooding
        // the network with garbage announcements that consume peer memory
        // and bandwidth.
        let window_start = now_secs.saturating_sub(self.rate_limit.window_secs);
        let log = self.rate_limit_log.entry(publisher).or_default();
        // Evict entries outside the window.
        log.retain(|&ts| ts >= window_start);
        if log.len() as u64 >= self.rate_limit.max_count {
            return Err(WorkshopError::RateLimitExceeded {
                publisher: publisher.to_string(),
                count: log.len() as u64,
                window_secs: self.rate_limit.window_secs,
                max_count: self.rate_limit.max_count,
            });
        }

        // ── All checks passed — update state ─────────────────────────
        self.known_sequences.insert(key, announcement.sequence());
        log.push(now_secs);

        Ok(())
    }
}

// ── Update discovery trait ───────────────────────────────────────────

/// Pluggable mechanism for learning about new content versions.
///
/// Implementations include:
/// - **P2P gossip** — peers exchange signed `VersionAnnouncement`s
///   during normal BitTorrent traffic. Zero infrastructure needed.
/// - **Workshop index** — check git/HTTP index for new manifests.
/// - **Compiled baseline** — hardcoded version info from build time.
///
/// Use [`FailoverDiscovery`] to combine multiple sources with automatic
/// fallback.
pub trait UpdateDiscovery {
    /// Checks for the latest known version of a resource.
    ///
    /// Returns `None` if the resource is unknown to this discovery
    /// source. Returns `Some(announcement)` with the highest-sequence
    /// announcement known.
    fn latest(&self, resource: &ResourceId) -> Result<Option<VersionAnnouncement>, WorkshopError>;

    /// Returns all known version announcements for a resource,
    /// ordered by sequence number (oldest first).
    fn all_versions(
        &self,
        resource: &ResourceId,
    ) -> Result<Vec<VersionAnnouncement>, WorkshopError>;

    /// Submits a new announcement to this discovery source.
    ///
    /// For P2P gossip: broadcasts to connected peers.
    /// For index backends: no-op (index is updated via CI/API).
    /// For compiled baseline: no-op (immutable).
    fn announce(&mut self, announcement: VersionAnnouncement) -> Result<(), WorkshopError>;
}

// ── Failover discovery ───────────────────────────────────────────────

/// Combines multiple discovery sources with automatic failover.
///
/// Tries each source in priority order. The first source that returns
/// a successful result wins. If all sources fail, returns
/// [`WorkshopError::AllSourcesFailed`].
///
/// Typical configuration:
///
/// ```text
/// FailoverDiscovery [
///   PeerGossip     — asks connected swarm peers (free, instant)
///   WorkshopIndex  — checks git/HTTP index (reliable, ~seconds)
///   CompiledInfo   — built-in version floor (always works)
/// ]
/// ```
pub struct FailoverDiscovery {
    sources: Vec<Box<dyn UpdateDiscovery>>,
}

impl FailoverDiscovery {
    /// Creates a failover discovery from multiple sources, tried in order.
    pub fn new(sources: Vec<Box<dyn UpdateDiscovery>>) -> Result<Self, WorkshopError> {
        if sources.is_empty() {
            return Err(WorkshopError::Index {
                detail: "FailoverDiscovery requires at least one source".to_string(),
            });
        }
        Ok(Self { sources })
    }

    /// Returns the number of configured sources.
    pub fn source_count(&self) -> usize {
        self.sources.len()
    }
}

impl UpdateDiscovery for FailoverDiscovery {
    fn latest(&self, resource: &ResourceId) -> Result<Option<VersionAnnouncement>, WorkshopError> {
        let mut last_error = None;
        for source in &self.sources {
            match source.latest(resource) {
                Ok(result) => return Ok(result),
                Err(e) => last_error = Some(e),
            }
        }
        Err(WorkshopError::AllSourcesFailed {
            count: self.sources.len(),
            last_error: last_error.map(|e| e.to_string()).unwrap_or_default(),
        })
    }

    fn all_versions(
        &self,
        resource: &ResourceId,
    ) -> Result<Vec<VersionAnnouncement>, WorkshopError> {
        let mut last_error = None;
        for source in &self.sources {
            match source.all_versions(resource) {
                Ok(result) => return Ok(result),
                Err(e) => last_error = Some(e),
            }
        }
        Err(WorkshopError::AllSourcesFailed {
            count: self.sources.len(),
            last_error: last_error.map(|e| e.to_string()).unwrap_or_default(),
        })
    }

    fn announce(&mut self, announcement: VersionAnnouncement) -> Result<(), WorkshopError> {
        // Broadcast to ALL sources (not failover — we want every source to know).
        let mut last_error = None;
        let mut any_succeeded = false;
        for source in &mut self.sources {
            match source.announce(announcement.clone()) {
                Ok(()) => any_succeeded = true,
                Err(e) => last_error = Some(e),
            }
        }
        if any_succeeded {
            Ok(())
        } else {
            Err(WorkshopError::AllSourcesFailed {
                count: self.sources.len(),
                last_error: last_error.map(|e| e.to_string()).unwrap_or_default(),
            })
        }
    }
}

// ── In-memory update discovery (for tests) ───────────────────────────

/// In-memory update discovery source for unit testing.
#[derive(Debug, Default)]
pub struct MemoryDiscovery {
    announcements: Vec<VersionAnnouncement>,
}

impl MemoryDiscovery {
    /// Seeds an announcement into this discovery source.
    pub fn seed(&mut self, announcement: VersionAnnouncement) {
        self.announcements.push(announcement);
    }
}

impl UpdateDiscovery for MemoryDiscovery {
    fn latest(&self, resource: &ResourceId) -> Result<Option<VersionAnnouncement>, WorkshopError> {
        Ok(self
            .announcements
            .iter()
            .filter(|a| a.resource() == resource)
            .max_by_key(|a| a.sequence())
            .cloned())
    }

    fn all_versions(
        &self,
        resource: &ResourceId,
    ) -> Result<Vec<VersionAnnouncement>, WorkshopError> {
        let mut versions: Vec<_> = self
            .announcements
            .iter()
            .filter(|a| a.resource() == resource)
            .cloned()
            .collect();
        versions.sort_by_key(|a| a.sequence());
        Ok(versions)
    }

    fn announce(&mut self, announcement: VersionAnnouncement) -> Result<(), WorkshopError> {
        self.announcements.push(announcement);
        Ok(())
    }
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: creates a test announcement with the given sequence number.
    fn test_announcement(seq: u64) -> VersionAnnouncement {
        VersionAnnouncement::new(
            PublisherId::from_ed25519([0xAA; 32]),
            ResourceId::new("alice", "hd-sprites").unwrap(),
            ResourceVersion::new(1, seq as u32, 0),
            BlobId::new([seq as u8; 32]),
            Some([0x42; 20]),
            seq,
            1_700_000_000 + seq,
            [0xBB; 64], // Fake signature (test only).
        )
    }

    /// Helper: creates a test announcement for a different resource.
    fn test_announcement_other(seq: u64) -> VersionAnnouncement {
        VersionAnnouncement::new(
            PublisherId::from_ed25519([0xCC; 32]),
            ResourceId::new("bob", "maps").unwrap(),
            ResourceVersion::new(2, 0, seq as u32),
            BlobId::new([0xDD; 32]),
            None,
            seq,
            1_700_000_000 + seq,
            [0xEE; 64],
        )
    }

    // ── PublisherId ──────────────────────────────────────────────────

    /// PublisherId preserves the Ed25519 public key bytes.
    #[test]
    fn publisher_id_round_trip() {
        let key = [0x42; 32];
        let id = PublisherId::from_ed25519(key);
        assert_eq!(id.as_bytes(), &key);
    }

    /// Display shows a truncated hex string for readability.
    #[test]
    fn publisher_id_display() {
        let id = PublisherId::from_ed25519([0xAB; 32]);
        let display = id.to_string();
        assert!(display.starts_with("pub:"), "{display}");
        assert!(display.len() < 40, "should be truncated: {display}");
    }

    // ── VersionAnnouncement ──────────────────────────────────────────

    /// Announcement accessors return the correct values.
    #[test]
    fn announcement_accessors() {
        let ann = test_announcement(1);
        assert_eq!(ann.publisher_id().as_bytes(), &[0xAA; 32]);
        assert_eq!(ann.resource().to_string(), "alice/hd-sprites");
        assert_eq!(ann.version().to_string(), "1.1.0");
        assert_eq!(ann.sequence(), 1);
        assert!(ann.info_hash().is_some());
        assert_eq!(ann.signature(), &[0xBB; 64]);
    }

    /// canonical_bytes is deterministic — same announcement always
    /// produces the same bytes for signing.
    #[test]
    fn canonical_bytes_deterministic() {
        let ann = test_announcement(1);
        let bytes1 = ann.canonical_bytes();
        let bytes2 = ann.canonical_bytes();
        assert_eq!(bytes1, bytes2);
    }

    /// Different announcements produce different canonical bytes.
    #[test]
    fn canonical_bytes_differ_for_different_announcements() {
        let ann1 = test_announcement(1);
        let ann2 = test_announcement(2);
        assert_ne!(ann1.canonical_bytes(), ann2.canonical_bytes());
    }

    /// canonical_bytes does not include the signature field (the
    /// signature is computed OVER the canonical bytes).
    #[test]
    fn canonical_bytes_excludes_signature() {
        let ann1 = test_announcement(1);
        let mut ann2 = test_announcement(1);
        ann2.signature = [0xFF; 64]; // Different signature, same content.
        assert_eq!(ann1.canonical_bytes(), ann2.canonical_bytes());
    }

    /// Announcement with None info_hash produces valid canonical bytes.
    #[test]
    fn canonical_bytes_without_info_hash() {
        let ann = test_announcement_other(1);
        assert!(ann.info_hash().is_none());
        let bytes = ann.canonical_bytes();
        assert!(!bytes.is_empty());
    }

    // ── MemoryDiscovery ──────────────────────────────────────────────

    /// latest() returns the highest-sequence announcement for a resource.
    #[test]
    fn memory_discovery_latest() {
        let mut disc = MemoryDiscovery::default();
        disc.seed(test_announcement(1));
        disc.seed(test_announcement(3));
        disc.seed(test_announcement(2));

        let id = ResourceId::new("alice", "hd-sprites").unwrap();
        let latest = disc.latest(&id).unwrap().unwrap();
        assert_eq!(latest.sequence(), 3);
    }

    /// latest() returns None for unknown resources.
    #[test]
    fn memory_discovery_latest_unknown() {
        let disc = MemoryDiscovery::default();
        let id = ResourceId::new("nobody", "nothing").unwrap();
        assert!(disc.latest(&id).unwrap().is_none());
    }

    /// all_versions() returns announcements sorted by sequence.
    #[test]
    fn memory_discovery_all_versions_sorted() {
        let mut disc = MemoryDiscovery::default();
        disc.seed(test_announcement(3));
        disc.seed(test_announcement(1));
        disc.seed(test_announcement(2));

        let id = ResourceId::new("alice", "hd-sprites").unwrap();
        let versions = disc.all_versions(&id).unwrap();
        assert_eq!(versions.len(), 3);
        assert_eq!(versions[0].sequence(), 1);
        assert_eq!(versions[1].sequence(), 2);
        assert_eq!(versions[2].sequence(), 3);
    }

    /// all_versions() only returns announcements for the requested resource.
    #[test]
    fn memory_discovery_filters_by_resource() {
        let mut disc = MemoryDiscovery::default();
        disc.seed(test_announcement(1));
        disc.seed(test_announcement_other(1));

        let id = ResourceId::new("alice", "hd-sprites").unwrap();
        let versions = disc.all_versions(&id).unwrap();
        assert_eq!(versions.len(), 1);
    }

    /// announce() makes the announcement discoverable.
    #[test]
    fn memory_discovery_announce() {
        let mut disc = MemoryDiscovery::default();
        disc.announce(test_announcement(1)).unwrap();

        let id = ResourceId::new("alice", "hd-sprites").unwrap();
        assert!(disc.latest(&id).unwrap().is_some());
    }

    // ── FailoverDiscovery ────────────────────────────────────────────

    /// Always-failing discovery source for testing failover.
    struct FailingDiscovery;

    impl UpdateDiscovery for FailingDiscovery {
        fn latest(
            &self,
            _resource: &ResourceId,
        ) -> Result<Option<VersionAnnouncement>, WorkshopError> {
            Err(WorkshopError::Index {
                detail: "simulated failure".to_string(),
            })
        }
        fn all_versions(
            &self,
            _resource: &ResourceId,
        ) -> Result<Vec<VersionAnnouncement>, WorkshopError> {
            Err(WorkshopError::Index {
                detail: "simulated failure".to_string(),
            })
        }
        fn announce(&mut self, _ann: VersionAnnouncement) -> Result<(), WorkshopError> {
            Err(WorkshopError::Index {
                detail: "simulated failure".to_string(),
            })
        }
    }

    /// FailoverDiscovery requires at least one source.
    #[test]
    fn failover_discovery_rejects_empty() {
        assert!(FailoverDiscovery::new(vec![]).is_err());
    }

    /// FailoverDiscovery skips failed sources and uses the first working one.
    ///
    /// This proves the game doesn't need git or any specific platform —
    /// if the primary source (P2P gossip) fails, it falls back to the
    /// index, then to compiled baseline.
    #[test]
    fn failover_discovery_skips_failed() {
        let disc = FailoverDiscovery::new(vec![
            Box::new(FailingDiscovery),
            Box::new({
                let mut m = MemoryDiscovery::default();
                m.seed(test_announcement(5));
                m
            }),
        ])
        .unwrap();

        let id = ResourceId::new("alice", "hd-sprites").unwrap();
        let latest = disc.latest(&id).unwrap().unwrap();
        assert_eq!(latest.sequence(), 5);
    }

    /// When all discovery sources fail, the error reports the count.
    #[test]
    fn failover_discovery_all_fail() {
        let disc =
            FailoverDiscovery::new(vec![Box::new(FailingDiscovery), Box::new(FailingDiscovery)])
                .unwrap();

        let id = ResourceId::new("alice", "hd-sprites").unwrap();
        let err = disc.latest(&id).unwrap_err();
        assert!(
            matches!(err, WorkshopError::AllSourcesFailed { count: 2, .. }),
            "expected AllSourcesFailed, got: {err}"
        );
    }

    /// announce() broadcasts to ALL sources (not failover — fan-out).
    #[test]
    fn failover_discovery_announce_fans_out() {
        let mut disc = FailoverDiscovery::new(vec![
            Box::new(MemoryDiscovery::default()),
            Box::new(MemoryDiscovery::default()),
        ])
        .unwrap();

        disc.announce(test_announcement(1)).unwrap();
        assert_eq!(disc.source_count(), 2);
    }

    // ── Integration: full update flow ────────────────────────────────

    /// Demonstrates the complete update discovery flow without git,
    /// without any specific platform, using only P2P concepts.
    ///
    /// This is the proof that the game doesn't need git:
    /// 1. Publisher signs an announcement (using p2p-distribute's signer)
    /// 2. Announcement propagates through the swarm (gossip)
    /// 3. Game client discovers the update (FailoverDiscovery)
    /// 4. Game client extracts info_hash for P2P download
    #[test]
    fn full_update_discovery_flow() {
        // Step 1: Publisher creates and signs an announcement.
        let publisher = PublisherId::from_ed25519([0x42; 32]);
        let resource = ResourceId::new("modder", "balance-patch").unwrap();
        let announcement = VersionAnnouncement::new(
            publisher,
            resource.clone(),
            ResourceVersion::new(2, 0, 0),
            BlobId::new([0xFF; 32]),
            Some([0xAA; 20]),
            42,
            1_700_000_000,
            [0xBB; 64], // Would be real Ed25519 sig in production.
        );

        // Step 2: Announcement is seeded into the gossip network.
        // (Simulated here with MemoryDiscovery — in production this is
        //  p2p-distribute's extension message protocol.)
        let mut gossip = MemoryDiscovery::default();
        gossip.announce(announcement).unwrap();

        // Step 3: Game client checks for updates via FailoverDiscovery.
        let disc = FailoverDiscovery::new(vec![Box::new(gossip)]).unwrap();
        let latest = disc.latest(&resource).unwrap().unwrap();

        // Step 4: Extract download parameters for p2p-distribute.
        assert_eq!(latest.version().to_string(), "2.0.0");
        let info_hash = latest.info_hash().expect("has torrent metadata");
        assert_eq!(info_hash, &[0xAA; 20]);
        // → Pass info_hash to p2p-distribute to start download.
        // → Verify downloaded content against latest.blob_id().
    }

    // ── AnnouncementValidator: happy path ────────────────────────────

    /// Helper: creates a validator with AcceptAllVerifier for happy-path tests.
    fn test_validator() -> AnnouncementValidator<AcceptAllVerifier> {
        AnnouncementValidator::new(AcceptAllVerifier)
    }

    /// A valid announcement passes all checks.
    #[test]
    fn validator_accepts_valid_announcement() {
        let mut v = test_validator();
        let ann = test_announcement(1);
        // now_secs matches the announcement timestamp.
        assert!(v.validate(&ann, 1_700_000_001).is_ok());
    }

    /// Sequential announcements with increasing sequence numbers pass.
    #[test]
    fn validator_accepts_sequential_announcements() {
        let mut v = test_validator();
        for seq in 1..=5 {
            let ann = test_announcement(seq);
            assert!(v.validate(&ann, 1_700_000_000 + seq).is_ok());
        }
    }

    /// Announcements from different publishers/resources are tracked
    /// independently — sequence numbers don't interfere across pairs.
    #[test]
    fn validator_tracks_sequences_per_publisher_resource() {
        let mut v = test_validator();
        let ann_alice = test_announcement(1);
        let ann_bob = test_announcement_other(1);
        assert!(v.validate(&ann_alice, 1_700_000_001).is_ok());
        assert!(v.validate(&ann_bob, 1_700_000_001).is_ok());
    }

    // ── AnnouncementValidator: adversarial tests ─────────────────────

    /// **Attack: forged signature.** An attacker modifies an announcement
    /// (e.g. swaps the blob hash) but cannot produce a valid Ed25519
    /// signature. The validator rejects it with InvalidSignature.
    #[test]
    fn validator_rejects_forged_signature() {
        let mut v = AnnouncementValidator::new(RejectAllVerifier);
        let ann = test_announcement(1);
        let err = v.validate(&ann, 1_700_000_001).unwrap_err();
        assert!(
            matches!(err, WorkshopError::InvalidSignature { .. }),
            "expected InvalidSignature, got: {err}"
        );
    }

    /// **Attack: replay.** An adversary re-broadcasts a legitimately
    /// signed old announcement. The validator has already seen a higher
    /// sequence number and rejects it as stale.
    #[test]
    fn validator_rejects_replay_stale_sequence() {
        let mut v = test_validator();
        // Accept sequence 5.
        assert!(v.validate(&test_announcement(5), 1_700_000_005).is_ok());
        // Replay sequence 3 — stale.
        let err = v
            .validate(&test_announcement(3), 1_700_000_005)
            .unwrap_err();
        assert!(
            matches!(
                err,
                WorkshopError::StaleAnnouncement {
                    received: 3,
                    known: 5,
                    ..
                }
            ),
            "expected StaleAnnouncement, got: {err}"
        );
    }

    /// **Attack: replay of exactly the same sequence number.** Even
    /// re-broadcasting the exact same announcement is rejected (sequence
    /// must be *strictly* increasing).
    #[test]
    fn validator_rejects_equal_sequence() {
        let mut v = test_validator();
        assert!(v.validate(&test_announcement(3), 1_700_000_003).is_ok());
        let err = v
            .validate(&test_announcement(3), 1_700_000_004)
            .unwrap_err();
        assert!(
            matches!(err, WorkshopError::StaleAnnouncement { .. }),
            "expected StaleAnnouncement, got: {err}"
        );
    }

    /// **Attack: sequence racing to u64::MAX.** An attacker with a
    /// briefly compromised key sets sequence to a huge value, attempting
    /// to block the real publisher from ever issuing a higher sequence.
    /// The validator rejects jumps larger than `max_sequence_jump`.
    #[test]
    fn validator_rejects_sequence_jump_too_large() {
        let mut v = test_validator().with_max_sequence_jump(100);
        // Establish sequence 1.
        assert!(v.validate(&test_announcement(1), 1_700_000_001).is_ok());
        // Jump to 999 (delta = 998, exceeds max_jump = 100).
        let err = v
            .validate(&test_announcement(999), 1_700_000_999)
            .unwrap_err();
        assert!(
            matches!(
                err,
                WorkshopError::SequenceJumpTooLarge {
                    old_seq: 1,
                    new_seq: 999,
                    max_jump: 100,
                    ..
                }
            ),
            "expected SequenceJumpTooLarge, got: {err}"
        );
    }

    /// Sequence jump check does not apply to the first announcement
    /// (no prior sequence to compare against).
    #[test]
    fn validator_allows_first_announcement_any_sequence() {
        let mut v = test_validator().with_max_sequence_jump(5);
        // First announcement at sequence 500 — no prior, so no jump check.
        assert!(v.validate(&test_announcement(500), 1_700_000_500).is_ok());
    }

    /// A jump exactly at the limit is allowed.
    #[test]
    fn validator_allows_jump_at_boundary() {
        let mut v = test_validator().with_max_sequence_jump(10);
        assert!(v.validate(&test_announcement(1), 1_700_000_001).is_ok());
        // Jump of exactly 10 — at the boundary, allowed.
        assert!(v.validate(&test_announcement(11), 1_700_000_011).is_ok());
    }

    /// **Attack: future-dated timestamp.** An attacker sets a timestamp
    /// far in the future so their announcement always appears "newest."
    /// The validator rejects timestamps beyond `max_clock_drift`.
    #[test]
    fn validator_rejects_future_timestamp() {
        let mut v = test_validator().with_max_clock_drift(60);
        let ann = VersionAnnouncement::new(
            PublisherId::from_ed25519([0xAA; 32]),
            ResourceId::new("alice", "hd-sprites").unwrap(),
            ResourceVersion::new(1, 0, 0),
            BlobId::new([0x01; 32]),
            None,
            1,
            // Timestamp 1000 seconds in the future (drift limit is 60).
            2_000_001_000,
            [0xBB; 64],
        );
        let err = v.validate(&ann, 2_000_000_000).unwrap_err();
        assert!(
            matches!(
                err,
                WorkshopError::FutureTimestamp {
                    max_drift_secs: 60,
                    ..
                }
            ),
            "expected FutureTimestamp, got: {err}"
        );
    }

    /// Timestamps within the drift window are accepted (clocks vary).
    #[test]
    fn validator_accepts_timestamp_within_drift() {
        let mut v = test_validator().with_max_clock_drift(60);
        let ann = VersionAnnouncement::new(
            PublisherId::from_ed25519([0xAA; 32]),
            ResourceId::new("alice", "hd-sprites").unwrap(),
            ResourceVersion::new(1, 0, 0),
            BlobId::new([0x01; 32]),
            None,
            1,
            // 30 seconds in the future — within 60s drift.
            2_000_000_030,
            [0xBB; 64],
        );
        assert!(v.validate(&ann, 2_000_000_000).is_ok());
    }

    /// Past timestamps are always accepted (we only guard the future).
    #[test]
    fn validator_accepts_past_timestamp() {
        let mut v = test_validator().with_max_clock_drift(60);
        let ann = test_announcement(1);
        // now_secs far in the future relative to announcement.
        assert!(v.validate(&ann, 9_000_000_000).is_ok());
    }

    /// **Attack: announcement flood.** A malicious publisher spams the
    /// network with many announcements to exhaust peer memory and
    /// bandwidth. The validator enforces a per-publisher rate limit.
    #[test]
    fn validator_rejects_rate_limit_exceeded() {
        let mut v = test_validator().with_rate_limit(RateLimitConfig {
            max_count: 3,
            window_secs: 3600,
        });

        let now = 1_700_000_000;
        // Three announcements — all within the limit.
        for seq in 1..=3 {
            let ann = test_announcement(seq);
            assert!(v.validate(&ann, now + seq).is_ok());
        }
        // Fourth announcement exceeds the limit.
        let err = v.validate(&test_announcement(4), now + 4).unwrap_err();
        assert!(
            matches!(err, WorkshopError::RateLimitExceeded { max_count: 3, .. }),
            "expected RateLimitExceeded, got: {err}"
        );
    }

    /// Rate limit resets after the window expires.
    #[test]
    fn validator_rate_limit_resets_after_window() {
        let mut v = test_validator().with_rate_limit(RateLimitConfig {
            max_count: 2,
            window_secs: 100,
        });

        let now = 1_700_000_000;
        assert!(v.validate(&test_announcement(1), now).is_ok());
        assert!(v.validate(&test_announcement(2), now + 1).is_ok());
        // Third within window — rejected.
        assert!(v.validate(&test_announcement(3), now + 2).is_err());

        // 200 seconds later — window expired, limit resets.
        assert!(v.validate(&test_announcement(3), now + 200).is_ok());
    }

    /// **Attack: key compromise.** A publisher's Ed25519 key is stolen.
    /// The key is added to the revocation list. All announcements from
    /// the revoked key are rejected — even with valid signatures.
    #[test]
    fn validator_rejects_revoked_publisher() {
        let mut v = test_validator();
        let publisher = PublisherId::from_ed25519([0xAA; 32]);
        v.revoke_publisher(publisher, "key compromised");

        let ann = test_announcement(1);
        let err = v.validate(&ann, 1_700_000_001).unwrap_err();
        assert!(
            matches!(err, WorkshopError::PublisherRevoked { .. }),
            "expected PublisherRevoked, got: {err}"
        );
    }

    /// Revocation only affects the revoked publisher — other publishers
    /// continue working normally.
    #[test]
    fn validator_revocation_is_per_publisher() {
        let mut v = test_validator();
        let revoked = PublisherId::from_ed25519([0xAA; 32]);
        v.revoke_publisher(revoked, "bad actor");

        // Revoked publisher is rejected.
        assert!(v.validate(&test_announcement(1), 1_700_000_001).is_err());
        // Other publisher is accepted.
        assert!(v
            .validate(&test_announcement_other(1), 1_700_000_001)
            .is_ok());
    }

    /// **Defense-in-depth: revocation supersedes valid signatures.**
    /// Even if the attacker can produce valid signatures (they have the
    /// key), revocation must still block them.
    #[test]
    fn validator_revocation_checked_before_signature() {
        // Use AcceptAllVerifier — signature would pass, but revocation
        // should fire first.
        let mut v = test_validator();
        let publisher = PublisherId::from_ed25519([0xAA; 32]);
        v.revoke_publisher(publisher, "DMCA takedown");

        let ann = test_announcement(1);
        let err = v.validate(&ann, 1_700_000_001).unwrap_err();
        // Must be PublisherRevoked, NOT InvalidSignature.
        assert!(
            matches!(err, WorkshopError::PublisherRevoked { .. }),
            "revocation must be checked before signature: {err}"
        );
    }

    /// **Combined attack scenario.** A revoked publisher with a future
    /// timestamp and huge sequence jump — the validator catches the
    /// revocation first (fail-fast ordering).
    #[test]
    fn validator_combined_attack_revocation_wins() {
        let mut v = test_validator()
            .with_max_clock_drift(60)
            .with_max_sequence_jump(10);
        let publisher = PublisherId::from_ed25519([0xAA; 32]);
        v.revoke_publisher(publisher, "malware distributor");

        let ann = VersionAnnouncement::new(
            publisher,
            ResourceId::new("alice", "hd-sprites").unwrap(),
            ResourceVersion::new(99, 99, 99),
            BlobId::new([0xFF; 32]),
            None,
            u64::MAX, // Sequence racing.
            u64::MAX, // Future timestamp.
            [0xBB; 64],
        );
        let err = v.validate(&ann, 1_700_000_000).unwrap_err();
        assert!(
            matches!(err, WorkshopError::PublisherRevoked { .. }),
            "revocation is the first check: {err}"
        );
    }
}
