// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! BitTorrent `.torrent` file creation — generates torrent metadata from local
//! files or streaming byte sources so that content packages can be distributed
//! via P2P.
//!
//! Two APIs:
//!
//! - [`create_torrent`] — single-shot, reads a file from disk.
//! - [`TorrentBuilder`] — streaming/incremental, feed bytes as they arrive.
//!   Preferred for the maintainer `torrent-create` command (zero disk I/O).
//!
//! Core torrent creation types are provided by the [`p2p_distribute`] crate
//! and re-exported here.

// ── Re-exports from p2p-distribute ────────────────────────────────────

pub use p2p_distribute::torrent_create::{
    create_torrent, recommended_piece_length, TorrentBuilder, TorrentCreateError, TorrentMetadata,
    DEFAULT_PIECE_LENGTH,
};
