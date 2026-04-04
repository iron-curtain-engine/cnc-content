// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! Piece-level download coordinator — unified peer management for HTTP web seeds
//! and BitTorrent swarm peers.
//!
//! ## Design (BEP 19 web seeding + D049 distribution strategy)
//!
//! The coordinator treats every source of content pieces equally: HTTP mirrors
//! (BEP 19 web seeds) and the BitTorrent swarm are both "peers" in a shared
//! piece picker. This means a first-time user with zero BT peers can still
//! download at full speed via web seeds, while subsequent users automatically
//! benefit from P2P distribution as the swarm grows.
//!
//! ## Architecture
//!
//! Core coordinator types are provided by the [`p2p_distribute`] crate and
//! re-exported here. cnc-content-specific peer implementations (web seeds with
//! `CNC_DOWNLOAD_TIMEOUT` env var, BT swarm via librqbit) live in submodules.
//!
//! - **[`Peer`]** trait — unified interface for any piece source.
//! - **[`SharedPieceMap`]** — atomic piece-state tracking.
//! - **[`PieceCoordinator`]** — the orchestrator.
//!
//! ## How it integrates
//!
//! The [`downloader`](crate::downloader) module creates a `PieceCoordinator`,
//! adds web seed peers (from `web_seeds` + resolved mirror list URLs) and
//! optionally a BT swarm peer (from librqbit), then calls
//! [`PieceCoordinator::run`] to download all pieces.

// ── Re-exports from p2p-distribute ────────────────────────────────────

pub use p2p_distribute::bandwidth::BandwidthEstimator;
pub use p2p_distribute::bitfield::{rarity_scores, PeerBitfield};
pub use p2p_distribute::budget::{BudgetExceeded, ConnectionBudget};
pub use p2p_distribute::coordinator::{
    CoordinatorConfig, CoordinatorError, CoordinatorProgress, PieceCoordinator,
};
pub use p2p_distribute::network_id::NetworkId;
pub use p2p_distribute::peer::{Peer, PeerCapabilities, PeerError, PeerKind, RejectionReason};
pub use p2p_distribute::peer_id::{PeerId, PeerIdDecodeError, PeerIdKind};
pub use p2p_distribute::peer_stats::{PeerReputation, PeerStats, PeerTracker, TrustLevel};
pub use p2p_distribute::pex::{PexEntry, PexFlags, PexMessage};
pub use p2p_distribute::phi_detector::PhiDetector;
pub use p2p_distribute::piece_map::{PieceState, SharedPieceMap};
pub use p2p_distribute::priority::{PiecePriority, PiecePriorityMap};
pub use p2p_distribute::selection::{select_multiple_pieces, select_next_piece, PieceSelection};
pub use p2p_distribute::torrent_info::TorrentInfo;

// ── Sub-modules ───────────────────────────────────────────────────────

/// HTTP web seed peer — fetches pieces via Range requests (BEP 19).
/// Uses `CNC_DOWNLOAD_TIMEOUT` env var for timeout configuration.
#[cfg(feature = "download")]
pub mod webseed;

/// BitTorrent swarm peer — wraps librqbit as a single "mega-peer".
#[cfg(feature = "torrent")]
pub mod btswarm;

#[cfg(test)]
mod tests;
