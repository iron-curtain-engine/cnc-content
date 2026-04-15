// SPDX-License-Identifier: MIT OR Apache-2.0

//! Pre-signed URL peer exchange — gossip about object storage URLs that
//! can serve piece data without a direct peer connection.
//!
//! ## What
//!
//! [`UrlPexEntry`] extends the PEX model with **pre-signed object storage
//! URLs** for individual pieces. Instead of only sharing IP:port addresses,
//! peers can share short-lived, authenticated URLs that point directly to
//! pieces in an S3-compatible bucket. Any peer that receives such a URL
//! can fetch the piece via a simple HTTP GET — no TCP connection to the
//! original peer needed.
//!
//! ## Why (novel P2P primitive)
//!
//! Traditional PEX (BEP-11) shares contact information: IP + port. This
//! requires the advertised peer to be reachable. Peers behind strict NAT,
//! firewalls, or CGNAT may have pieces but no way to serve them.
//!
//! Pre-signed URL PEX decouples **having data** from **being reachable**:
//!
//! - A peer uploads a piece to their S3/R2 bucket.
//! - They generate a pre-signed URL (e.g. `X-Amz-Expires=3600`).
//! - The URL is shared via the PEX gossip layer.
//! - Other peers fetch the piece directly from the bucket.
//!
//! This enables:
//!
//! - **NAT traversal bypass** — peers behind strict NAT contribute data
//!   via object storage without relay circuits.
//! - **Serverless seeding** — upload pieces to R2, share URLs, stop your
//!   machine. The bucket keeps serving.
//! - **CDN integration** — a CDN can generate pre-signed URLs for pieces,
//!   making edge-cached content available to the swarm.
//!
//! ## How
//!
//! - [`UrlPexEntry`] carries piece_index + URL + expiry + piece hash.
//! - [`UrlPexMessage`] tracks a batch of URL entries with a network_id.
//! - [`UrlPexCache`] deduplicates and evicts expired URLs, so the swarm
//!   doesn't accumulate stale entries.
//! - Security: receivers must **verify piece hashes** after download.
//!   A pre-signed URL is a data source hint, not a trust assertion.
//!   Corrupted data from a URL is handled the same as corrupted data
//!   from a BT peer — the corruption ledger attributes blame.
//!
//! ## Integration with the coordinator
//!
//! The coordinator treats a pre-signed URL the same as a web seed: fetch
//! the piece via HTTP GET, SHA-1 verify, write to storage. The URL is
//! consumed once and discarded (or cached until expiry). No changes to
//! the `Peer` trait are needed — a `WebSeedPeer` pointed at the URL
//! handles the actual fetch.
//!
//! ```
//! use p2p_distribute::url_pex::{UrlPexEntry, UrlPexMessage, UrlPexCache};
//! use p2p_distribute::network_id::NetworkId;
//!
//! let net_id = NetworkId::from_name("test-network");
//! let mut msg = UrlPexMessage::new(net_id);
//!
//! msg.add(UrlPexEntry {
//!     piece_index: 42,
//!     url: "https://r2.example.com/dl/00000042?X-Amz-Expires=3600&sig=abc".into(),
//!     expires_secs: 3600,
//!     expected_sha1: "da39a3ee5e6b4b0d3255bfef95601890afd80709".into(),
//! });
//!
//! assert_eq!(msg.entries().len(), 1);
//! assert!(!msg.is_empty());
//! ```

use std::collections::HashMap;

use crate::network_id::NetworkId;

// ── Constants ───────────────────────────────────────────────────────

/// Maximum entries per URL PEX message (keep gossip bandwidth bounded).
///
/// Pre-signed URLs are larger than IP:port entries, so we use a lower
/// limit than the standard [`MAX_PEX_ADDED`](crate::pex::MAX_PEX_ADDED).
pub const MAX_URL_PEX_ENTRIES: usize = 20;

/// Default expiry for pre-signed URLs in seconds (1 hour).
///
/// URLs with shorter TTLs waste gossip bandwidth (expire before reaching
/// all peers). Longer TTLs increase the window for replay attacks.
/// 1 hour is a reasonable balance.
pub const DEFAULT_URL_EXPIRY_SECS: u64 = 3600;

/// Maximum URL length accepted in a URL PEX entry.
///
/// Guards against oversized payloads. Pre-signed S3 URLs are typically
/// 200–500 bytes; 2048 provides generous headroom.
pub const MAX_URL_LENGTH: usize = 2048;

/// Maximum entries tracked per piece in the URL cache.
///
/// Multiple peers may share URLs for the same piece (different buckets
/// or mirrors). Tracking more than this per piece wastes memory.
pub const MAX_URLS_PER_PIECE: usize = 8;

// ── UrlPexEntry ─────────────────────────────────────────────────────

/// A pre-signed URL for a specific piece.
///
/// Contains enough metadata for the receiver to fetch the piece via HTTP
/// GET and verify its integrity. The URL is opaque to the PEX layer —
/// it may be an S3 pre-signed URL, a Cloudflare R2 Workers URL, or any
/// HTTP endpoint that serves the piece data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlPexEntry {
    /// Which piece this URL serves.
    pub piece_index: u32,
    /// The pre-signed URL. Maximum length: [`MAX_URL_LENGTH`].
    pub url: String,
    /// Time-to-live in seconds from message creation.
    ///
    /// After this many seconds, the URL is assumed expired and should be
    /// evicted from the cache. Receivers subtract elapsed time since
    /// receipt to compute the remaining TTL.
    pub expires_secs: u64,
    /// Expected SHA-1 hex digest of the piece data.
    ///
    /// Receivers **must** verify this hash after download. A mismatch
    /// indicates either a corrupt source or a malicious URL.
    pub expected_sha1: String,
}

impl UrlPexEntry {
    /// Whether this entry's URL exceeds the length limit.
    pub fn is_oversized(&self) -> bool {
        self.url.len() > MAX_URL_LENGTH
    }

    /// Whether this entry has a plausible SHA-1 hash (40 hex chars).
    pub fn has_valid_sha1(&self) -> bool {
        self.expected_sha1.len() == 40 && self.expected_sha1.bytes().all(|b| b.is_ascii_hexdigit())
    }
}

// ── UrlPexMessage ───────────────────────────────────────────────────

/// Batch of pre-signed URL entries for gossip exchange.
///
/// Like [`PexMessage`](crate::pex::PexMessage), this is scoped to a
/// [`NetworkId`] and has a size limit. Receivers discard messages from
/// different networks.
#[derive(Debug, Clone)]
pub struct UrlPexMessage {
    /// Network this message belongs to.
    pub network_id: NetworkId,
    /// URL entries in this batch.
    entries: Vec<UrlPexEntry>,
}

impl UrlPexMessage {
    /// Creates an empty URL PEX message for the given network.
    pub fn new(network_id: NetworkId) -> Self {
        Self {
            network_id,
            entries: Vec::new(),
        }
    }

    /// Adds an entry to the message.
    ///
    /// Silently drops entries that are oversized or have invalid SHA-1
    /// hashes. Returns `true` if the entry was accepted.
    pub fn add(&mut self, entry: UrlPexEntry) -> bool {
        if entry.is_oversized() || !entry.has_valid_sha1() {
            return false;
        }
        if self.entries.len() >= MAX_URL_PEX_ENTRIES {
            return false;
        }
        self.entries.push(entry);
        true
    }

    /// Returns the entries in this message.
    pub fn entries(&self) -> &[UrlPexEntry] {
        &self.entries
    }

    /// Whether this message has any entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether the entry count exceeds the limit.
    pub fn is_oversized(&self) -> bool {
        self.entries.len() > MAX_URL_PEX_ENTRIES
    }

    /// Consumes the message and returns the entries.
    pub fn into_entries(self) -> Vec<UrlPexEntry> {
        self.entries
    }
}

// ── UrlPexCache ─────────────────────────────────────────────────────

/// Cache of known pre-signed URLs, indexed by piece.
///
/// Provides deduplication and expiry tracking. The coordinator queries
/// this cache when deciding where to fetch a piece — if a valid URL
/// exists, it can be tried before (or alongside) BT peers.
///
/// ## Bounded memory
///
/// Each piece tracks at most [`MAX_URLS_PER_PIECE`] URLs. When the
/// limit is hit, the entry with the shortest remaining TTL is evicted
/// to make room.
pub struct UrlPexCache {
    /// Per-piece URL entries with receipt timestamps.
    entries: HashMap<u32, Vec<CachedUrl>>,
}

/// A cached URL entry with receipt time for TTL computation.
#[derive(Debug, Clone)]
struct CachedUrl {
    /// The URL PEX entry.
    entry: UrlPexEntry,
    /// When this entry was received (for TTL computation).
    received: std::time::Instant,
}

impl CachedUrl {
    /// Whether this entry has expired.
    fn is_expired(&self) -> bool {
        let elapsed = self.received.elapsed().as_secs();
        elapsed >= self.entry.expires_secs
    }

    /// Remaining TTL in seconds (0 if expired).
    fn remaining_ttl(&self) -> u64 {
        let elapsed = self.received.elapsed().as_secs();
        self.entry.expires_secs.saturating_sub(elapsed)
    }
}

impl UrlPexCache {
    /// Creates an empty URL PEX cache.
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    /// Inserts a URL entry into the cache.
    ///
    /// Deduplicates by URL. If the per-piece limit is reached, the entry
    /// with the shortest remaining TTL is evicted.
    pub fn insert(&mut self, entry: UrlPexEntry) {
        let piece_entries = self.entries.entry(entry.piece_index).or_default();

        // Evict expired entries first.
        piece_entries.retain(|c| !c.is_expired());

        // Deduplicate by URL.
        if piece_entries.iter().any(|c| c.entry.url == entry.url) {
            return;
        }

        // Evict shortest-TTL if at capacity.
        if piece_entries.len() >= MAX_URLS_PER_PIECE {
            // Find the entry with the shortest remaining TTL.
            if let Some(min_idx) = piece_entries
                .iter()
                .enumerate()
                .min_by_key(|(_, c)| c.remaining_ttl())
                .map(|(i, _)| i)
            {
                piece_entries.swap_remove(min_idx);
            }
        }

        piece_entries.push(CachedUrl {
            entry,
            received: std::time::Instant::now(),
        });
    }

    /// Returns all non-expired URLs for a piece.
    pub fn urls_for_piece(&self, piece_index: u32) -> Vec<&UrlPexEntry> {
        self.entries
            .get(&piece_index)
            .map(|entries| {
                entries
                    .iter()
                    .filter(|c| !c.is_expired())
                    .map(|c| &c.entry)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Returns the total number of cached (non-expired) URLs across all pieces.
    pub fn total_urls(&self) -> usize {
        self.entries
            .values()
            .flat_map(|entries| entries.iter())
            .filter(|c| !c.is_expired())
            .count()
    }

    /// Returns the number of pieces with at least one cached URL.
    pub fn piece_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|(_, entries)| entries.iter().any(|c| !c.is_expired()))
            .count()
    }

    /// Evicts all expired entries from the cache.
    ///
    /// Call periodically to free memory. Also removes piece keys with
    /// zero remaining entries.
    pub fn evict_expired(&mut self) {
        self.entries.retain(|_, entries| {
            entries.retain(|c| !c.is_expired());
            !entries.is_empty()
        });
    }

    /// Processes a received [`UrlPexMessage`], adding all valid entries.
    ///
    /// Returns the number of entries accepted.
    pub fn process_message(&mut self, message: UrlPexMessage) -> usize {
        let mut accepted = 0;
        for entry in message.into_entries() {
            if !entry.is_oversized() && entry.has_valid_sha1() {
                self.insert(entry);
                accepted += 1;
            }
        }
        accepted
    }
}

impl Default for UrlPexCache {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for UrlPexCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UrlPexCache")
            .field("piece_count", &self.piece_count())
            .field("total_urls", &self.total_urls())
            .finish()
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_net_id() -> NetworkId {
        NetworkId::from_name("test-network")
    }

    fn test_entry(piece: u32) -> UrlPexEntry {
        UrlPexEntry {
            piece_index: piece,
            url: format!("https://r2.example.com/dl/{piece:08}?sig=abc"),
            expires_secs: 3600,
            expected_sha1: "da39a3ee5e6b4b0d3255bfef95601890afd80709".into(),
        }
    }

    // ── UrlPexEntry ─────────────────────────────────────────────────

    /// Entry with a valid SHA-1 is accepted.
    #[test]
    fn entry_valid_sha1() {
        let e = test_entry(0);
        assert!(e.has_valid_sha1());
        assert!(!e.is_oversized());
    }

    /// Entry with invalid SHA-1 (wrong length) is rejected.
    #[test]
    fn entry_invalid_sha1_length() {
        let mut e = test_entry(0);
        e.expected_sha1 = "tooshort".into();
        assert!(!e.has_valid_sha1());
    }

    /// Entry with invalid SHA-1 (non-hex chars) is rejected.
    #[test]
    fn entry_invalid_sha1_chars() {
        let mut e = test_entry(0);
        e.expected_sha1 = "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz".into();
        assert!(!e.has_valid_sha1());
    }

    /// Oversized URL is detected.
    #[test]
    fn entry_oversized_url() {
        let mut e = test_entry(0);
        e.url = "x".repeat(MAX_URL_LENGTH + 1);
        assert!(e.is_oversized());
    }

    /// URL at exactly the limit is not oversized.
    #[test]
    fn entry_url_at_limit() {
        let mut e = test_entry(0);
        e.url = "x".repeat(MAX_URL_LENGTH);
        assert!(!e.is_oversized());
    }

    // ── UrlPexMessage ───────────────────────────────────────────────

    /// Empty message is empty.
    #[test]
    fn message_empty() {
        let msg = UrlPexMessage::new(test_net_id());
        assert!(msg.is_empty());
        assert!(!msg.is_oversized());
        assert_eq!(msg.entries().len(), 0);
    }

    /// Adding a valid entry succeeds.
    #[test]
    fn message_add_valid_entry() {
        let mut msg = UrlPexMessage::new(test_net_id());
        assert!(msg.add(test_entry(1)));
        assert_eq!(msg.entries().len(), 1);
    }

    /// Adding an entry with invalid SHA-1 is silently rejected.
    #[test]
    fn message_rejects_invalid_sha1() {
        let mut msg = UrlPexMessage::new(test_net_id());
        let mut e = test_entry(0);
        e.expected_sha1 = "bad".into();
        assert!(!msg.add(e));
        assert!(msg.is_empty());
    }

    /// Adding an oversized URL entry is silently rejected.
    #[test]
    fn message_rejects_oversized_url() {
        let mut msg = UrlPexMessage::new(test_net_id());
        let mut e = test_entry(0);
        e.url = "x".repeat(MAX_URL_LENGTH + 1);
        assert!(!msg.add(e));
        assert!(msg.is_empty());
    }

    /// Message cap is enforced.
    #[test]
    fn message_cap_enforced() {
        let mut msg = UrlPexMessage::new(test_net_id());
        for i in 0..MAX_URL_PEX_ENTRIES {
            assert!(msg.add(test_entry(i as u32)));
        }
        // One more should be rejected.
        assert!(!msg.add(test_entry(999)));
        assert_eq!(msg.entries().len(), MAX_URL_PEX_ENTRIES);
    }

    /// `into_entries` consumes the message.
    #[test]
    fn message_into_entries() {
        let mut msg = UrlPexMessage::new(test_net_id());
        msg.add(test_entry(1));
        msg.add(test_entry(2));
        let entries = msg.into_entries();
        assert_eq!(entries.len(), 2);
    }

    // ── UrlPexCache ─────────────────────────────────────────────────

    /// Cache starts empty.
    #[test]
    fn cache_starts_empty() {
        let cache = UrlPexCache::new();
        assert_eq!(cache.total_urls(), 0);
        assert_eq!(cache.piece_count(), 0);
    }

    /// Inserting a URL makes it retrievable.
    #[test]
    fn cache_insert_and_retrieve() {
        let mut cache = UrlPexCache::new();
        cache.insert(test_entry(42));
        assert_eq!(cache.total_urls(), 1);
        assert_eq!(cache.piece_count(), 1);
        let urls = cache.urls_for_piece(42);
        assert_eq!(urls.len(), 1);
        assert_eq!(urls[0].piece_index, 42);
    }

    /// Duplicate URLs (same piece + URL) are deduplicated.
    #[test]
    fn cache_deduplicates() {
        let mut cache = UrlPexCache::new();
        cache.insert(test_entry(0));
        cache.insert(test_entry(0)); // Same URL.
        assert_eq!(cache.urls_for_piece(0).len(), 1);
    }

    /// Different URLs for the same piece are both stored.
    #[test]
    fn cache_multiple_urls_per_piece() {
        let mut cache = UrlPexCache::new();
        let mut e1 = test_entry(0);
        e1.url = "https://a.example.com/piece".into();
        let mut e2 = test_entry(0);
        e2.url = "https://b.example.com/piece".into();
        cache.insert(e1);
        cache.insert(e2);
        assert_eq!(cache.urls_for_piece(0).len(), 2);
    }

    /// Per-piece limit evicts shortest-TTL entry.
    #[test]
    fn cache_evicts_shortest_ttl() {
        let mut cache = UrlPexCache::new();
        // Fill to MAX_URLS_PER_PIECE with 1-hour TTLs.
        for i in 0..MAX_URLS_PER_PIECE {
            let mut e = test_entry(0);
            e.url = format!("https://example.com/{i}");
            e.expires_secs = 3600;
            cache.insert(e);
        }
        assert_eq!(cache.urls_for_piece(0).len(), MAX_URLS_PER_PIECE);

        // Add one more — should evict one.
        let mut e = test_entry(0);
        e.url = "https://example.com/new".into();
        e.expires_secs = 7200; // Longer TTL.
        cache.insert(e);
        assert_eq!(cache.urls_for_piece(0).len(), MAX_URLS_PER_PIECE);
    }

    /// `process_message` adds all valid entries.
    #[test]
    fn cache_process_message() {
        let mut cache = UrlPexCache::new();
        let mut msg = UrlPexMessage::new(test_net_id());
        msg.add(test_entry(1));
        msg.add(test_entry(2));
        msg.add(test_entry(3));
        let accepted = cache.process_message(msg);
        assert_eq!(accepted, 3);
        assert_eq!(cache.piece_count(), 3);
    }

    /// Non-existent piece returns empty URL list.
    #[test]
    fn cache_missing_piece_empty() {
        let cache = UrlPexCache::new();
        assert!(cache.urls_for_piece(999).is_empty());
    }

    /// Cache debug format includes counts.
    #[test]
    fn cache_debug_format() {
        let mut cache = UrlPexCache::new();
        cache.insert(test_entry(0));
        let debug = format!("{cache:?}");
        assert!(debug.contains("piece_count"), "got: {debug}");
        assert!(debug.contains("total_urls"), "got: {debug}");
    }

    /// Default cache is empty.
    #[test]
    fn cache_default_is_empty() {
        let cache = UrlPexCache::default();
        assert_eq!(cache.total_urls(), 0);
    }

    // ── Adversarial inputs ──────────────────────────────────────────

    /// Entry with zero expires_secs is accepted (immediately expired in cache).
    #[test]
    fn entry_zero_expiry_accepted() {
        let mut e = test_entry(0);
        e.expires_secs = 0;
        assert!(e.has_valid_sha1());
        // Cache should accept but it'll be expired immediately.
        let mut cache = UrlPexCache::new();
        cache.insert(e);
        // Evict should remove it.
        cache.evict_expired();
        assert_eq!(cache.total_urls(), 0);
    }

    /// Entry with empty URL is not oversized but should be handled.
    #[test]
    fn entry_empty_url_not_oversized() {
        let mut e = test_entry(0);
        e.url = String::new();
        assert!(!e.is_oversized());
    }

    /// SHA-1 hash with uppercase hex is still 40 hex chars.
    #[test]
    fn entry_uppercase_sha1_valid() {
        let mut e = test_entry(0);
        e.expected_sha1 = "DA39A3EE5E6B4B0D3255BFEF95601890AFD80709".into();
        assert!(e.has_valid_sha1());
    }
}
