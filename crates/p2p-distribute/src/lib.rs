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

pub mod bandwidth;
pub mod bitfield;
pub mod budget;
pub mod coordinator;
pub mod network_id;
pub mod peer;
pub mod peer_id;
pub mod peer_stats;
pub mod pex;
pub mod phi_detector;
pub mod piece_map;
pub mod priority;
pub mod reader;
pub mod selection;
pub mod streaming;
pub mod torrent_create;
pub mod torrent_info;

#[cfg(feature = "http")]
pub mod webseed;

// ── Re-exports ──────────────────────────────────────────────────────

pub use bandwidth::BandwidthEstimator;
pub use bitfield::{rarity_scores, PeerBitfield};
pub use budget::{BudgetExceeded, ConnectionBudget};
pub use coordinator::{CoordinatorConfig, CoordinatorError, CoordinatorProgress, PieceCoordinator};
pub use network_id::NetworkId;
pub use peer::{Peer, PeerCapabilities, PeerError, PeerKind, RejectionReason};
pub use peer_id::{PeerId, PeerIdDecodeError, PeerIdKind};
pub use peer_stats::{PeerReputation, PeerStats, PeerTracker, TrustLevel};
pub use pex::{PexEntry, PexFlags, PexMessage};
pub use phi_detector::PhiDetector;
pub use piece_map::{PieceState, SharedPieceMap};
pub use priority::{PiecePriority, PiecePriorityMap};
pub use reader::{StreamNotifier, StreamingReader};
pub use selection::{select_multiple_pieces, select_next_piece, PieceSelection};
pub use streaming::{
    evaluate_peer_priority, BufferPolicy, ByteRange, ByteRangeMap, PeerPriority, PieceMapping,
    StreamProgress,
};
pub use torrent_create::{
    create_torrent, recommended_piece_length, TorrentCreateError, TorrentMetadata,
    DEFAULT_PIECE_LENGTH,
};
pub use torrent_info::TorrentInfo;

#[cfg(feature = "http")]
pub use webseed::WebSeedPeer;

// ── Internal utilities ──────────────────────────────────────────────

/// Encodes a byte slice as lowercase hex.
pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    s
}
