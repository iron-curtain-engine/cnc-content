// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! Streaming content reader — play video (VQA cutscenes) before full download.
//!
//! Core streaming types are provided by the [`p2p_distribute`] crate and
//! re-exported here. The reader supports two modes:
//!
//! 1. **Disk-backed** (default): file is fully downloaded, reads go straight to disk.
//! 2. **Streaming**: file is partially downloaded, reads block until pieces arrive.
//!
//! See [`p2p_distribute::streaming`] for the full type documentation.

// ── Re-exports from p2p-distribute ────────────────────────────────────

pub use p2p_distribute::reader::{StreamNotifier, StreamingReader};
pub use p2p_distribute::streaming::{
    evaluate_peer_priority, BufferPolicy, ByteRange, ByteRangeMap, PeerPriority, PieceMapping,
    StreamProgress,
};

#[cfg(test)]
mod tests;
