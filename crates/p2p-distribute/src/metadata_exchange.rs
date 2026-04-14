// SPDX-License-Identifier: MIT OR Apache-2.0

//! BEP 9 metadata exchange — resolving magnet URIs without .torrent files.
//!
//! ## What
//!
//! Implements the client-side logic of BEP 9 (`ut_metadata` extension),
//! which allows downloading torrent metadata (the info dictionary) from
//! peers in the swarm. This is the mechanism that makes magnet URIs work:
//! the client knows only the info hash, connects to peers, and requests
//! the metadata in 16 KiB chunks.
//!
//! ## Why — magnet URIs as the default entry point
//!
//! Magnet URIs are the primary way users share torrents in modern BT
//! clients. They encode just the info hash (20 bytes) and optionally
//! tracker URLs. The actual metadata (piece hashes, file names, piece
//! length) must be fetched from the swarm.
//!
//! For Iron Curtain content distribution:
//!
//! - **Magnet URIs are compact.** A 70-character magnet URI replaces a
//!   multi-kilobyte .torrent file in configuration.
//! - **Metadata propagates virally.** Once any peer in the swarm has the
//!   metadata, all other peers can obtain it. No central .torrent host
//!   required.
//! - **Verification is built-in.** The SHA-1 of the assembled metadata
//!   must match the info hash from the magnet URI. Corrupted or malicious
//!   metadata is detected and rejected.
//! - **Embedded .torrent files are still preferred.** The IC crate embeds
//!   `.torrent` files at compile time via `include_bytes!`, so metadata
//!   exchange is a fallback path — but it must work for community-shared
//!   torrents where we don't have embedded metadata.
//!
//! ## How
//!
//! - [`MetadataExchange`]: State machine tracking metadata download
//!   progress for a single info hash.
//! - [`MetadataMessage`]: Request/data/reject protocol messages.
//! - [`MetadataBlock`]: A 16 KiB chunk of the info dictionary.
//! - Verification: assembled metadata is SHA-1 hashed and compared to
//!   the info hash before acceptance.

use sha1::{Digest, Sha1};

// ── Constants ───────────────────────────────────────────────────────

/// Size of each metadata block in bytes (BEP 9 standard).
const METADATA_BLOCK_SIZE: usize = 16384;

/// Maximum metadata size we'll accept (10 MiB).
///
/// Torrents with more than ~10 MiB of metadata are pathological. This
/// cap prevents OOM from malicious peers advertising huge metadata.
const MAX_METADATA_SIZE: usize = 10 * 1024 * 1024;

// ── Protocol messages ───────────────────────────────────────────────

/// BEP 9 metadata exchange message types.
///
/// These map to the `msg_type` field in the `ut_metadata` extension
/// message. The payload format differs by message type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetadataMessage {
    /// Request a metadata block by index.
    Request {
        /// Block index (0-based).
        piece: u32,
    },
    /// Deliver a metadata block.
    Data {
        /// Block index (0-based).
        piece: u32,
        /// Total metadata size in bytes.
        total_size: usize,
        /// Block payload (up to 16384 bytes).
        payload: Vec<u8>,
    },
    /// Reject a metadata request (peer doesn't have metadata).
    Reject {
        /// Block index that was rejected.
        piece: u32,
    },
}

impl MetadataMessage {
    /// Returns the BEP 9 `msg_type` integer for this message.
    pub fn msg_type(&self) -> u8 {
        match self {
            Self::Request { .. } => 0,
            Self::Data { .. } => 1,
            Self::Reject { .. } => 2,
        }
    }

    /// Returns the piece index referenced by this message.
    pub fn piece(&self) -> u32 {
        match self {
            Self::Request { piece } | Self::Data { piece, .. } | Self::Reject { piece } => *piece,
        }
    }
}

// ── Metadata block tracking ─────────────────────────────────────────

/// Status of a single metadata block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockStatus {
    /// Not yet requested.
    Missing,
    /// Requested from a peer, awaiting response.
    Pending,
    /// Received and stored.
    Received,
}

/// Tracks assembly of metadata blocks for a single info hash.
///
/// ```
/// use p2p_distribute::metadata_exchange::{MetadataExchange, MetadataMessage};
///
/// let info_hash = [0xAB; 20];
/// let mut exchange = MetadataExchange::new(info_hash);
///
/// // Peer tells us metadata is 32768 bytes (2 blocks).
/// exchange.set_metadata_size(32768).unwrap();
/// assert_eq!(exchange.block_count(), 2);
/// assert!(!exchange.is_complete());
/// ```
pub struct MetadataExchange {
    /// The info hash we're trying to resolve.
    info_hash: [u8; 20],
    /// Total metadata size (learned from first Data message).
    metadata_size: Option<usize>,
    /// Block status tracking.
    blocks: Vec<BlockStatus>,
    /// Assembled metadata buffer.
    buffer: Vec<u8>,
    /// Whether all blocks have been received and verified.
    verified: bool,
}

impl MetadataExchange {
    /// Creates a new metadata exchange for the given info hash.
    pub fn new(info_hash: [u8; 20]) -> Self {
        Self {
            info_hash,
            metadata_size: None,
            blocks: Vec::new(),
            buffer: Vec::new(),
            verified: false,
        }
    }

    /// Sets the total metadata size.
    ///
    /// Called when we learn the size from a peer's `Data` message.
    /// Returns an error if the size exceeds the safety cap.
    pub fn set_metadata_size(&mut self, size: usize) -> Result<(), MetadataError> {
        if size > MAX_METADATA_SIZE {
            return Err(MetadataError::TooLarge {
                size,
                max: MAX_METADATA_SIZE,
            });
        }
        if size == 0 {
            return Err(MetadataError::EmptyMetadata);
        }

        let block_count = size.div_ceil(METADATA_BLOCK_SIZE);
        self.metadata_size = Some(size);
        self.blocks = vec![BlockStatus::Missing; block_count];
        self.buffer = vec![0u8; size];
        Ok(())
    }

    /// Returns the number of blocks needed.
    pub fn block_count(&self) -> usize {
        self.blocks.len()
    }

    /// Returns the next block index that needs to be requested.
    pub fn next_needed_block(&self) -> Option<u32> {
        self.blocks
            .iter()
            .enumerate()
            .find(|(_, status)| **status == BlockStatus::Missing)
            .map(|(i, _)| i as u32)
    }

    /// Marks a block as pending (requested from a peer).
    pub fn mark_pending(&mut self, piece: u32) {
        if let Some(status) = self.blocks.get_mut(piece as usize) {
            if *status == BlockStatus::Missing {
                *status = BlockStatus::Pending;
            }
        }
    }

    /// Handles a received data block and copies it into the buffer.
    pub fn receive_block(
        &mut self,
        piece: u32,
        total_size: usize,
        payload: &[u8],
    ) -> Result<(), MetadataError> {
        // Set metadata size if not yet known.
        if self.metadata_size.is_none() {
            self.set_metadata_size(total_size)?;
        }

        let meta_size = self.metadata_size.unwrap_or(0);
        if total_size != meta_size {
            return Err(MetadataError::SizeMismatch {
                expected: meta_size,
                received: total_size,
            });
        }

        let block_idx = piece as usize;
        if block_idx >= self.blocks.len() {
            return Err(MetadataError::InvalidBlock {
                index: piece,
                count: self.blocks.len() as u32,
            });
        }

        // Compute expected block size.
        let offset = block_idx.saturating_mul(METADATA_BLOCK_SIZE);
        let expected_len = METADATA_BLOCK_SIZE.min(meta_size.saturating_sub(offset));
        if payload.len() != expected_len {
            return Err(MetadataError::BlockSizeMismatch {
                piece,
                expected: expected_len,
                received: payload.len(),
            });
        }

        // Copy payload into the assembly buffer.
        let dest = self
            .buffer
            .get_mut(offset..offset.saturating_add(payload.len()));
        if let Some(dest) = dest {
            dest.copy_from_slice(payload);
        }

        if let Some(status) = self.blocks.get_mut(block_idx) {
            *status = BlockStatus::Received;
        }

        Ok(())
    }

    /// Handles a reject message by resetting the block to Missing.
    pub fn handle_reject(&mut self, piece: u32) {
        if let Some(status) = self.blocks.get_mut(piece as usize) {
            if *status == BlockStatus::Pending {
                *status = BlockStatus::Missing;
            }
        }
    }

    /// Returns whether all blocks have been received (but not yet verified).
    pub fn all_blocks_received(&self) -> bool {
        !self.blocks.is_empty() && self.blocks.iter().all(|s| *s == BlockStatus::Received)
    }

    /// Returns whether the metadata has been fully verified.
    pub fn is_complete(&self) -> bool {
        self.verified
    }

    /// Attempts to verify the assembled metadata against the info hash.
    ///
    /// Called after all blocks are received. Returns the verified metadata
    /// bytes on success, or an error if the hash doesn't match.
    pub fn verify(&mut self) -> Result<Vec<u8>, MetadataError> {
        if !self.all_blocks_received() {
            return Err(MetadataError::Incomplete {
                received: self.received_count(),
                total: self.blocks.len() as u32,
            });
        }

        let hash = Sha1::digest(&self.buffer);
        let hash_bytes: [u8; 20] = hash.into();

        if hash_bytes != self.info_hash {
            // Reset all blocks so we can retry from scratch.
            for status in &mut self.blocks {
                *status = BlockStatus::Missing;
            }
            return Err(MetadataError::HashMismatch {
                expected: self.info_hash,
                actual: hash_bytes,
            });
        }

        self.verified = true;
        Ok(self.buffer.clone())
    }

    /// Returns the count of received blocks.
    pub fn received_count(&self) -> u32 {
        self.blocks
            .iter()
            .filter(|s| **s == BlockStatus::Received)
            .count() as u32
    }

    /// Returns the info hash.
    pub fn info_hash(&self) -> &[u8; 20] {
        &self.info_hash
    }

    /// Returns the metadata size if known.
    pub fn metadata_size(&self) -> Option<usize> {
        self.metadata_size
    }

    /// Returns the status of a block.
    pub fn block_status(&self, piece: u32) -> Option<BlockStatus> {
        self.blocks.get(piece as usize).copied()
    }

    /// Returns download progress as a fraction in [0.0, 1.0].
    pub fn progress(&self) -> f64 {
        if self.blocks.is_empty() {
            return 0.0;
        }
        self.received_count() as f64 / self.blocks.len() as f64
    }
}

// ── Errors ──────────────────────────────────────────────────────────

/// Errors from metadata exchange operations.
#[derive(Debug, thiserror::Error)]
pub enum MetadataError {
    /// Metadata exceeds safety cap.
    #[error("metadata too large ({size} bytes, max {max})")]
    TooLarge {
        /// Claimed size.
        size: usize,
        /// Maximum allowed.
        max: usize,
    },
    /// Metadata size is zero.
    #[error("metadata size is zero")]
    EmptyMetadata,
    /// Size mismatch between peers.
    #[error("metadata size mismatch: expected {expected}, received {received}")]
    SizeMismatch {
        /// Size we recorded from the first peer.
        expected: usize,
        /// Size a later peer claimed.
        received: usize,
    },
    /// Block index out of range.
    #[error("invalid block index {index} (only {count} blocks)")]
    InvalidBlock {
        /// Received index.
        index: u32,
        /// Total block count.
        count: u32,
    },
    /// Block payload size mismatch.
    #[error("block {piece} size mismatch: expected {expected}, received {received}")]
    BlockSizeMismatch {
        /// Block index.
        piece: u32,
        /// Expected payload size.
        expected: usize,
        /// Received payload size.
        received: usize,
    },
    /// Not all blocks received yet.
    #[error("metadata incomplete: {received}/{total} blocks")]
    Incomplete {
        /// Blocks received so far.
        received: u32,
        /// Total blocks needed.
        total: u32,
    },
    /// SHA-1 of assembled metadata doesn't match info hash.
    #[error("metadata hash mismatch")]
    HashMismatch {
        /// Expected hash (from magnet URI).
        expected: [u8; 20],
        /// Actual hash of assembled data.
        actual: [u8; 20],
    },
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: creates test metadata and its SHA-1 info hash.
    fn make_test_metadata(size: usize) -> (Vec<u8>, [u8; 20]) {
        let data: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
        let hash = Sha1::digest(&data);
        let info_hash: [u8; 20] = hash.into();
        (data, info_hash)
    }

    // ── MetadataMessage ─────────────────────────────────────────────

    /// Message type IDs match BEP 9 spec.
    ///
    /// BEP 9 defines: 0 = request, 1 = data, 2 = reject.
    #[test]
    fn message_types() {
        assert_eq!(MetadataMessage::Request { piece: 0 }.msg_type(), 0);
        assert_eq!(
            MetadataMessage::Data {
                piece: 0,
                total_size: 16384,
                payload: vec![]
            }
            .msg_type(),
            1
        );
        assert_eq!(MetadataMessage::Reject { piece: 0 }.msg_type(), 2);
    }

    // ── MetadataExchange ────────────────────────────────────────────

    /// Setting metadata size computes correct block count.
    ///
    /// 32768 bytes / 16384 block size = exactly 2 blocks.
    #[test]
    fn block_count_exact() {
        let mut ex = MetadataExchange::new([0; 20]);
        ex.set_metadata_size(32768).unwrap();
        assert_eq!(ex.block_count(), 2);
    }

    /// Partial last block rounds up.
    ///
    /// 20000 bytes needs ceil(20000/16384) = 2 blocks.
    #[test]
    fn block_count_partial() {
        let mut ex = MetadataExchange::new([0; 20]);
        ex.set_metadata_size(20000).unwrap();
        assert_eq!(ex.block_count(), 2);
    }

    /// Rejects oversized metadata.
    ///
    /// Malicious peers could advertise huge metadata to cause OOM.
    #[test]
    fn rejects_too_large() {
        let mut ex = MetadataExchange::new([0; 20]);
        let result = ex.set_metadata_size(MAX_METADATA_SIZE + 1);
        assert!(result.is_err());
    }

    /// Rejects empty metadata.
    ///
    /// Zero-size metadata is invalid per BEP 9.
    #[test]
    fn rejects_empty() {
        let mut ex = MetadataExchange::new([0; 20]);
        assert!(ex.set_metadata_size(0).is_err());
    }

    /// Full metadata exchange cycle with verification.
    ///
    /// Assembles metadata from blocks and verifies the SHA-1 hash
    /// matches the info hash.
    #[test]
    fn full_exchange_cycle() {
        let (data, info_hash) = make_test_metadata(20000);
        let mut ex = MetadataExchange::new(info_hash);

        ex.set_metadata_size(20000).unwrap();
        assert_eq!(ex.block_count(), 2);

        // Send block 0 (16384 bytes).
        ex.receive_block(0, 20000, &data[..METADATA_BLOCK_SIZE])
            .unwrap();
        assert!(!ex.all_blocks_received());

        // Send block 1 (3616 bytes).
        ex.receive_block(1, 20000, &data[METADATA_BLOCK_SIZE..])
            .unwrap();
        assert!(ex.all_blocks_received());

        let result = ex.verify().unwrap();
        assert_eq!(result, data);
        assert!(ex.is_complete());
    }

    /// Hash mismatch resets all blocks.
    ///
    /// When verification fails, all blocks are reset to Missing so the
    /// client can retry with a different peer.
    #[test]
    fn hash_mismatch_resets_blocks() {
        let (data, _info_hash) = make_test_metadata(METADATA_BLOCK_SIZE);
        let wrong_hash = [0xFF; 20]; // Deliberately wrong.
        let mut ex = MetadataExchange::new(wrong_hash);

        ex.set_metadata_size(METADATA_BLOCK_SIZE).unwrap();
        ex.receive_block(0, METADATA_BLOCK_SIZE, &data).unwrap();

        let result = ex.verify();
        assert!(result.is_err());

        // All blocks should be reset.
        assert_eq!(ex.block_status(0), Some(BlockStatus::Missing));
    }

    /// Reject message resets pending block to missing.
    ///
    /// A peer that rejects a request frees the block for re-request
    /// from another peer.
    #[test]
    fn reject_resets_pending() {
        let mut ex = MetadataExchange::new([0; 20]);
        ex.set_metadata_size(METADATA_BLOCK_SIZE).unwrap();

        ex.mark_pending(0);
        assert_eq!(ex.block_status(0), Some(BlockStatus::Pending));

        ex.handle_reject(0);
        assert_eq!(ex.block_status(0), Some(BlockStatus::Missing));
    }

    /// next_needed_block returns first missing block.
    ///
    /// Used by the request scheduler to decide which block to ask for.
    #[test]
    fn next_needed_block() {
        let mut ex = MetadataExchange::new([0; 20]);
        ex.set_metadata_size(32768).unwrap();

        assert_eq!(ex.next_needed_block(), Some(0));
        ex.mark_pending(0);
        assert_eq!(ex.next_needed_block(), Some(1));
    }

    /// Progress tracking.
    ///
    /// Progress should reflect fraction of blocks received.
    #[test]
    fn progress_tracking() {
        let (data, info_hash) = make_test_metadata(32768);
        let mut ex = MetadataExchange::new(info_hash);

        assert!((ex.progress() - 0.0).abs() < f64::EPSILON);

        ex.set_metadata_size(32768).unwrap();
        ex.receive_block(0, 32768, &data[..METADATA_BLOCK_SIZE])
            .unwrap();
        assert!((ex.progress() - 0.5).abs() < f64::EPSILON);
    }

    /// Size mismatch between blocks from different peers.
    ///
    /// If two peers disagree on metadata size, the second block is
    /// rejected.
    #[test]
    fn size_mismatch_rejected() {
        let mut ex = MetadataExchange::new([0; 20]);
        ex.set_metadata_size(METADATA_BLOCK_SIZE).unwrap();

        let result = ex.receive_block(0, METADATA_BLOCK_SIZE * 2, &[0; METADATA_BLOCK_SIZE]);
        assert!(result.is_err());
    }

    /// Invalid block index rejected.
    ///
    /// Block indices beyond the block count must be rejected.
    #[test]
    fn invalid_block_index() {
        let mut ex = MetadataExchange::new([0; 20]);
        ex.set_metadata_size(METADATA_BLOCK_SIZE).unwrap();

        let result = ex.receive_block(99, METADATA_BLOCK_SIZE, &[0; METADATA_BLOCK_SIZE]);
        assert!(result.is_err());
    }

    /// Block payload size must match expected.
    ///
    /// Incorrect payload sizes indicate a buggy or malicious peer.
    #[test]
    fn block_size_mismatch() {
        let mut ex = MetadataExchange::new([0; 20]);
        ex.set_metadata_size(METADATA_BLOCK_SIZE).unwrap();

        // Send wrong-sized payload.
        let result = ex.receive_block(0, METADATA_BLOCK_SIZE, &[0; 100]);
        assert!(result.is_err());
    }

    /// Verify without all blocks returns incomplete error.
    ///
    /// Verification must not proceed on partial metadata.
    #[test]
    fn verify_incomplete() {
        let mut ex = MetadataExchange::new([0; 20]);
        ex.set_metadata_size(32768).unwrap();

        let result = ex.verify();
        assert!(result.is_err());
    }
}
