// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! BitTorrent `.torrent` file creation — generates torrent metadata from local
//! files so that content packages can be distributed via P2P.
//!
//! Core torrent creation types are provided by the [`p2p_distribute`] crate
//! and re-exported here.

// ── Re-exports from p2p-distribute ────────────────────────────────────

pub use p2p_distribute::torrent_create::{
    create_torrent, recommended_piece_length, TorrentCreateError, TorrentMetadata,
    DEFAULT_PIECE_LENGTH,
};
