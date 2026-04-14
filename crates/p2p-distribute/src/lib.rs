// SPDX-License-Identifier: MIT OR Apache-2.0

//! Piece-level download coordinator with web seed support, streaming reader,
//! and torrent file creation.
//!
//! `p2p-distribute` is a standalone library for downloading files described by
//! BitTorrent metadata. It coordinates HTTP web seeds (BEP 19) and optional
//! BitTorrent swarm peers, verifies each piece with SHA-1, and writes verified
//! data to disk. A streaming reader lets consumers play media (video, audio)
//! before the download completes.
//!
//! ## Design principles
//!
//! - **No BitTorrent library dependency.** The [`Peer`] trait defines a generic
//!   piece source. Consumers bring their own BT backend (e.g. librqbit) by
//!   implementing this trait. HTTP web seeds are built in.
//! - **Zero-copy piece verification.** Every piece is SHA-1 verified against
//!   the expected hash from torrent metadata, regardless of source.
//! - **Streaming-first.** The [`StreamingReader`] provides `Read + Seek` over
//!   partially-downloaded files, blocking only when needed bytes have not
//!   arrived yet.
//!
//! ## Feature flags
//!
//! - **`http`** (default) — enables [`WebSeedPeer`], which fetches pieces via
//!   HTTP Range requests using `ureq`.
//!
//! ## Roadmap
//!
//! ### Closed share groups (managed replication)
//!
//! Allow a group master to publish a signed content catalog. Mirror nodes
//! that join the group automatically replicate the catalog and keep it in
//! sync as the master adds or removes files. Only the master (or designated
//! admins) can mutate the catalog; mirrors are read-only replicas.
//!
//! This enables self-hosted content shops: anyone can stand up a group,
//! populate it with files, and let mirror nodes increase availability
//! without trusting them with write access.
//!
//! Building blocks already in the crate:
//! - [`NetworkId`] — scopes a group to a unique identity.
//! - [`PeerId`] — identifies members within a group.
//! - [`PeerBitfield`] — tracks which pieces each mirror holds.
//! - [`ConnectionBudget`] — prevents a single mirror from monopolizing
//!   connections, which matters for group trust boundaries.
//!
//! Remaining work:
//! - **Group manifest** — a signed catalog of content hashes and member
//!   public keys (ed25519). Mirrors verify catalog updates
//!   cryptographically, without trusting the transport.
//! - **Catalog sync protocol** — mirrors subscribe to manifest updates and
//!   automatically begin replicating new content or pruning removed
//!   content.
//! - **Replication policy** — full-mirror vs. partial-mirror (e.g.
//!   geography-based sharding for very large catalogs).
//!
//! ### HTTP-to-P2P gateway (transparent streaming facade)
//!
//! Expose P2P-backed content over a standard HTTP endpoint. The
//! downloading client sees an ordinary HTTP response with
//! `Content-Length` / `Content-Range` headers; behind the scenes, the
//! gateway streams bytes from a [`StreamingReader`] that fetches pieces
//! via the P2P swarm on demand.
//!
//! This turns the P2P layer into a distributed storage backend with a
//! predictable HTTP download experience for end users who neither know
//! nor care that the data is assembled from multiple peers.
//!
//! Building blocks already in the crate:
//! - [`StreamingReader`] — provides `Read + Seek` over partially-
//!   downloaded content, blocking transparently while pieces arrive.
//! - [`BandwidthEstimator`] — adaptive prebuffering so the gateway can
//!   stay ahead of the HTTP response stream.
//! - [`BufferPolicy`] — controls how far ahead the P2P layer prefetches.
//!
//! Remaining work:
//! - **Gateway adapter crate** (e.g. `p2p-http-gateway`) — a thin async
//!   layer that wires incoming `hyper`/`axum` requests to
//!   `StreamingReader` instances, translating HTTP `Range` headers into
//!   `seek()` + `read()` calls.
//! - **Connection pooling** — reuse `StreamingReader` instances across
//!   requests for the same content to avoid redundant peer negotiation.
//!
//! ### P2P-to-HTTP bridge (auto-seeding from mirrors)
//!
//! The inverse of the gateway above: a bridge node participates in the
//! P2P swarm as a normal peer, but sources its data from existing HTTP
//! mirrors behind the scenes. The swarm sees a high-availability
//! super-seed; the HTTP infrastructure is an invisible backend.
//!
//! Use cases:
//! - **Auto-seeding** — the bridge monitors swarm demand and proactively
//!   fetches hot pieces from HTTP mirrors, acting as an always-available
//!   seed without storing the full catalog locally.
//! - **Mirror aggregation** — the parallel mirror racing already in
//!   [`WebSeedPeer`] load-balances across HTTP sources. A bridge node
//!   exposes that aggregated bandwidth as a single high-speed P2P peer.
//! - **Bootstrap path** — new content starts on HTTP mirrors only (as
//!   this crate already supports for RA/TD freeware). A bridge node
//!   automatically makes those mirrors available to the P2P swarm, so
//!   the transition from pure-HTTP to hybrid to full-P2P is seamless
//!   with no flag day required.
//!
//! This pairs naturally with closed share groups: a group master runs
//! bridge nodes backed by their HTTP CDN while community mirrors run
//! pure P2P nodes. The swarm sees uniform peers; the heterogeneous
//! backends are an implementation detail.
//!
//! Building blocks already in the crate:
//! - [`WebSeedPeer`] — already a primitive form of this direction:
//!   implements [`Peer`] using HTTP `Range` requests.
//! - [`PieceCoordinator`] — orchestrates piece acquisition from any
//!   `Peer` implementation, HTTP or otherwise.
//! - [`PeerBitfield`] — advertises piece availability to the swarm.
//! - [`BandwidthEstimator`] — measures effective throughput from HTTP
//!   backends to inform piece scheduling.
//!
//! Remaining work:
//! - **Demand-driven fetching** — a policy layer that watches swarm
//!   request patterns and pre-fetches pieces from HTTP mirrors before
//!   peers ask for them.
//! - **Partial-cache eviction** — the bridge may not have disk for the
//!   full catalog. An LRU or popularity-weighted eviction policy keeps
//!   the hottest pieces resident.
//! - **Mirror health monitoring** — integrate [`PhiDetector`] to detect
//!   degraded HTTP mirrors and shift load to healthy ones.
//!
//! ## Quick start
//!
//! ```no_run
//! use p2p_distribute::{PieceCoordinator, CoordinatorConfig, TorrentInfo, WebSeedPeer};
//!
//! // Build torrent info from a .torrent file or magnet URI resolution.
//! let info = TorrentInfo {
//!     piece_length: 262144,
//!     piece_hashes: vec![], // SHA-1 hashes from torrent metadata
//!     file_size: 1_000_000,
//!     file_name: "content.zip".into(),
//! };
//!
//! let config = CoordinatorConfig::default();
//! let mut coord = PieceCoordinator::new(info, config);
//! coord.add_peer(Box::new(WebSeedPeer::new("https://example.com/content.zip".into())));
//!
//! coord.run(std::path::Path::new("content.zip"), &mut |progress| {
//!     // Handle progress events
//! }).expect("download failed");
//! ```

// ── Modules ─────────────────────────────────────────────────────────

pub mod adaptive;
pub mod bandwidth;
pub mod bencode;
pub mod bitfield;
pub mod bridge;
pub mod bridge_peer;
pub mod budget;
pub mod cache;
pub mod catalog;
pub mod catalog_sign;
pub mod choking;
pub mod config;
pub mod connection;
pub mod content_discovery;
pub mod coordinator;
pub mod corruption_ledger;
pub mod credit;
pub mod demand;
pub mod dht;
pub mod endgame;
pub mod fast_extension;
pub mod gateway;
pub mod gateway_adapter;
pub mod group;
pub mod handshake;
pub mod local_discovery;
pub mod manifest;
pub mod merkle;
pub mod message;
pub mod metadata_exchange;
pub mod mirror_health;
pub mod network_id;
pub mod obfuscation;
pub mod peer;
pub mod peer_affinity;
pub mod peer_id;
pub mod peer_pool;
pub mod peer_stats;
pub mod pex;
pub mod phi_detector;
pub mod piece_data_cache;
pub mod piece_map;
pub mod piece_validator;
pub mod priority;
pub mod rate_limiter;
pub mod reader;
pub mod relay;
pub mod resume;
pub mod selection;
pub mod session_manager;
pub mod state;
pub mod storage;
pub mod streaming;
pub mod superseeding;
pub mod throttle;
pub mod torrent_create;
pub mod torrent_info;
pub mod tracker;
pub mod upload_queue;
pub mod work_stealing;

#[cfg(feature = "http")]
pub mod webseed;

#[cfg(test)]
mod streaming_integration_tests;

// ── Re-exports ──────────────────────────────────────────────────────

pub use adaptive::{AdaptiveConcurrency, ConcurrencyAdvice};
pub use bandwidth::BandwidthEstimator;
pub use bencode::{decode, encode, encode_into, BencodeValue, DecodeError};
pub use bitfield::{rarity_scores, PeerBitfield};
pub use bridge::{BridgeConfig, BridgeError, BridgeNode, MirrorHealth, MirrorPool, PrefetchPlan};
pub use bridge_peer::BridgePeer;
pub use budget::{BudgetExceeded, ConnectionBudget};
pub use cache::PieceCache;
pub use catalog::{plan_sync, ReplicationPolicy, SyncAction, SyncPlan};
pub use catalog_sign::{
    sign_manifest, verify_manifest, CatalogSigner, CatalogVerifier, HmacSha256Signer,
    HmacSha256Verifier, SignatureError,
};
pub use choking::{AlwaysUnchoke, ChokingDecision, ChokingStrategy, TitForTatChoking};
pub use config::{
    ChokingConfig, Config, ConfigBuilder, ConnectionConfig, ContentDiscoveryConfig, EndgameConfig,
    FastExtensionConfig, FeatureToggles, LocalDiscoveryConfig, MetadataExchangeConfig,
    ObfuscationConfig, PexConfig, RelayConfig, SuperSeedingConfig, TrackerConfig, UploadConfig,
    WebSeedConfig,
};
pub use connection::{CloseReason, ConnectionError, ConnectionPhase, ConnectionState};
pub use content_discovery::{
    DiscoveryMethod, DiscoverySource, RegistryError as DiscoveryRegistryError, SourceRegistry,
};
pub use coordinator::{
    CoordinatorConfig, CoordinatorError, CoordinatorProgress, DownloadMode, PartitionRecovery,
    PieceCoordinator, ReconciliationDiff,
};
pub use corruption_ledger::{Attribution, BlameEntry, CorruptionLedger};
pub use credit::{CreditEntry, CreditLedger};
pub use demand::DemandTracker;
pub use dht::{DhtMessage, DhtNode, KBucketEntry, NodeId, RoutingTable};
pub use endgame::{BlockId, EndgameAction, EndgameMode};
pub use fast_extension::{AllowedFastSet, FastDecodeError, FastMessage, SuggestCache};
pub use gateway::{ContentSlice, RangeError, RangeRequest, ResponseMeta};
pub use gateway_adapter::{GatewayAdapter, GatewayAdapterError, GatewayResponse};
pub use group::{GroupError, GroupMember, GroupRole, GroupRoster};
pub use handshake::{Capabilities, HandshakeMessage, PROTOCOL_VERSION};
pub use local_discovery::{LpdAnnounce, LpdPeer, LpdService, LPD_MULTICAST_ADDR, LPD_PORT};
pub use manifest::{
    diff_manifests, ContentEntry, GroupManifest, ManifestBuilder, ManifestDiff, ManifestError,
};
pub use merkle::{MerkleTree, ProofNode};
pub use message::{
    decode_message, encode_message, MessageError, PeerMessage, BLOCK_SIZE, MAX_MESSAGE_LENGTH,
};
pub use metadata_exchange::{BlockStatus, MetadataError, MetadataExchange, MetadataMessage};
pub use mirror_health::{HealthTier, MirrorEntry, MirrorRegistry, MirrorRegistryError, TierCounts};
pub use network_id::NetworkId;
pub use obfuscation::{random_port, ObfuscationKey};
pub use peer::{Peer, PeerCapabilities, PeerError, PeerKind, RejectionReason};
pub use peer_affinity::{AffinityConfig, AffinityScorer, PeerProfile};
pub use peer_id::{PeerId, PeerIdDecodeError, PeerIdKind};
pub use peer_pool::{
    PeerEntry, PeerPool, PeerPoolConfig, PeerState as PoolPeerState, PoolFullError,
};
pub use peer_stats::{
    ExclusionEntry, ExclusionScope, PeerReputation, PeerStats, PeerTracker, TrustLevel,
};
pub use pex::{PexDeltaTracker, PexEntry, PexFlags, PexMessage};
pub use phi_detector::PhiDetector;
pub use piece_data_cache::{CacheStats, PieceDataCache, DEFAULT_CACHE_BYTES};
pub use piece_map::{PieceState, SharedPieceMap};
pub use piece_validator::{
    PieceValidator, QuarantineEntry, RetryDecision, ValidationResult, ValidatorConfig,
};
pub use priority::{PiecePriority, PiecePriorityMap};
pub use rate_limiter::{RateLimiterMap, TokenBucket};
pub use reader::{StreamNotifier, StreamingReader};
pub use relay::{
    CleanupResult as RelayCleanupResult, HolePunchAttempt, HolePunchState, NatType, RelayCircuit,
    RelayError, RelayNode, RelayRegistry,
};
pub use resume::{PeerState, ResumeError, ResumeState, SubPieceProgress};
pub use selection::{select_multiple_pieces, select_next_piece, PieceSelection, SpeedCategory};
pub use session_manager::{
    BasicSession, DownloadHandle, DownloadSession, SessionConfig, SessionError, SessionEvent,
};
pub use state::{DownloadState, DownloadStateMachine, TransitionError};
pub use storage::{
    CoalescingStorage, FileStorage, FileStorageFactory, MemoryStorage, PieceStorage, StorageError,
    StorageFactory,
};
pub use streaming::{
    evaluate_peer_priority, BufferPolicy, ByteRange, ByteRangeMap, PeerPriority, PieceMapping,
    StreamProgress,
};
pub use superseeding::{PieceOffer, SuperSeedState};
pub use throttle::{BandwidthThrottle, ThrottlePair};
pub use torrent_create::{
    create_torrent, recommended_piece_length, TorrentCreateError, TorrentMetadata,
    DEFAULT_PIECE_LENGTH,
};
pub use torrent_info::TorrentInfo;
pub use tracker::{
    AnnounceRequest, AnnounceResponse, CompactPeer, ScrapeResponse, TrackerError, TrackerState,
    TrackerTier,
};
pub use upload_queue::{SlotResult, UploadQueue};
pub use work_stealing::{StealableTask, WorkStealingScheduler};

#[cfg(feature = "http")]
pub use webseed::WebSeedPeer;

// ── Internal utilities ──────────────────────────────────────────────

/// Encodes a byte slice as lowercase hex.
///
/// Uses a direct lookup table instead of `fmt::Write` to avoid the
/// formatting machinery overhead. Each byte maps to two ASCII hex
/// digits via nibble extraction — no branching, no format parsing.
pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut hex = Vec::with_capacity(bytes.len().saturating_mul(2));
    for &b in bytes {
        // b >> 4 is always 0..=15, b & 0x0f is always 0..=15, so .get()
        // always returns Some. The guards satisfy the safe-indexing rule.
        if let (Some(&hi), Some(&lo)) = (
            HEX_DIGITS.get((b >> 4) as usize),
            HEX_DIGITS.get((b & 0x0f) as usize),
        ) {
            hex.push(hi);
            hex.push(lo);
        }
    }
    // All bytes in hex[] are ASCII hex digits (0-9, a-f) — valid UTF-8.
    String::from_utf8(hex).unwrap_or_default()
}
