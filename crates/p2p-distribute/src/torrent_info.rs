// SPDX-License-Identifier: MIT OR Apache-2.0

//! Torrent metadata needed by the coordinator for piece verification and assembly.
//!
//! [`TorrentInfo`] is a subset of the torrent info dictionary — just the fields
//! needed for piece-level download coordination. It can be built from a `.torrent`
//! file, from magnet URI resolution, or from [`crate::torrent_create::create_torrent`].

// ── TorrentInfo ─────────────────────────────────────────────────────

/// Metadata needed by the coordinator to verify and assemble pieces.
///
/// This is a subset of the torrent info dict — just the fields needed for
/// piece-level download coordination. It can be built from a `.torrent` file,
/// from magnet URI resolution, or from `create_torrent()`.
#[derive(Debug, Clone)]
pub struct TorrentInfo {
    /// Size of each piece in bytes (last piece may be smaller).
    pub piece_length: u64,
    /// SHA-1 hash of each piece (20 bytes per piece, concatenated).
    pub piece_hashes: Vec<u8>,
    /// Total file size in bytes.
    pub file_size: u64,
    /// File name (for output path construction).
    pub file_name: String,
}

impl TorrentInfo {
    /// Number of pieces in this torrent.
    pub fn piece_count(&self) -> u32 {
        (self.piece_hashes.len() / 20) as u32
    }

    /// Returns the expected SHA-1 hash (20 bytes) for a given piece index.
    ///
    /// Returns `None` if the index is out of bounds.
    pub fn piece_hash(&self, index: u32) -> Option<&[u8]> {
        let start = (index as usize).checked_mul(20)?;
        let end = start.checked_add(20)?;
        self.piece_hashes.get(start..end)
    }

    /// Returns the byte offset within the file where a piece starts.
    pub fn piece_offset(&self, index: u32) -> u64 {
        (index as u64).saturating_mul(self.piece_length)
    }

    /// Returns the actual length of a piece (last piece may be shorter).
    pub fn piece_size(&self, index: u32) -> u32 {
        let offset = self.piece_offset(index);
        let remaining = self.file_size.saturating_sub(offset);
        remaining.min(self.piece_length) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Creates `TorrentInfo` from raw piece byte slices for testing.
    fn make_torrent_info(pieces: &[&[u8]]) -> TorrentInfo {
        use sha1::{Digest, Sha1};

        let piece_length = 256u64;
        let file_size: u64 = pieces.iter().map(|p| p.len() as u64).sum();
        let mut piece_hashes = Vec::with_capacity(pieces.len() * 20);

        for piece in pieces {
            let mut hasher = Sha1::new();
            hasher.update(piece);
            piece_hashes.extend_from_slice(hasher.finalize().as_slice());
        }

        TorrentInfo {
            piece_length,
            piece_hashes,
            file_size,
            file_name: "test-content.zip".into(),
        }
    }

    /// `TorrentInfo::piece_count` correctly derives from piece hash length.
    ///
    /// Each piece produces exactly 20 bytes of SHA-1 hash. The piece count
    /// must equal `piece_hashes.len() / 20`.
    #[test]
    fn torrent_info_piece_count() {
        let info = make_torrent_info(&[&[0xAA; 256], &[0xBB; 256], &[0xCC; 128]]);
        assert_eq!(info.piece_count(), 3);
    }

    /// `TorrentInfo::piece_hash` returns correct 20-byte SHA-1 for each piece.
    ///
    /// The hash at index `i` must match the SHA-1 of piece `i`'s data.
    #[test]
    fn torrent_info_piece_hash_matches() {
        use sha1::{Digest, Sha1};

        let data_a = [0xAA; 256];
        let data_b = [0xBB; 200];
        let info = make_torrent_info(&[&data_a, &data_b]);

        let mut h = Sha1::new();
        h.update(data_a);
        let expected_a = h.finalize();

        let mut h = Sha1::new();
        h.update(data_b);
        let expected_b = h.finalize();

        assert_eq!(info.piece_hash(0).unwrap(), expected_a.as_slice());
        assert_eq!(info.piece_hash(1).unwrap(), expected_b.as_slice());
        assert!(info.piece_hash(2).is_none()); // out of bounds
    }

    /// `TorrentInfo::piece_offset` computes correct byte offsets.
    ///
    /// Piece `i` starts at `i * piece_length`.
    #[test]
    fn torrent_info_piece_offset() {
        let info = make_torrent_info(&[&[0; 256], &[0; 256], &[0; 128]]);
        assert_eq!(info.piece_offset(0), 0);
        assert_eq!(info.piece_offset(1), 256);
        assert_eq!(info.piece_offset(2), 512);
    }

    /// `TorrentInfo::piece_size` returns correct size including the last short piece.
    ///
    /// All pieces except the last are `piece_length` bytes. The last piece
    /// covers the remaining bytes (potentially shorter).
    #[test]
    fn torrent_info_piece_size_last_piece_shorter() {
        let info = TorrentInfo {
            piece_length: 256,
            piece_hashes: vec![0u8; 60], // 3 pieces
            file_size: 640,              // 256 + 256 + 128
            file_name: "test.zip".into(),
        };
        assert_eq!(info.piece_size(0), 256);
        assert_eq!(info.piece_size(1), 256);
        assert_eq!(info.piece_size(2), 128); // last piece is short
    }

    // ── Out-of-bounds and boundary tests ────────────────────────────

    /// `piece_hash` returns `None` for out-of-bounds piece index.
    ///
    /// Callers must handle missing hashes gracefully. This is the boundary
    /// between "valid torrent data" and "buggy caller".
    #[test]
    fn piece_hash_out_of_bounds_returns_none() {
        let info = make_torrent_info(&[&[0xAA; 256]]);
        assert!(info.piece_hash(0).is_some());
        assert!(info.piece_hash(1).is_none());
        assert!(info.piece_hash(u32::MAX).is_none());
    }

    /// `piece_offset` saturates instead of overflowing for large indices.
    ///
    /// With a large piece_length, `index * piece_length` could overflow u64.
    /// `saturating_mul` prevents this.
    #[test]
    fn piece_offset_large_index_saturates() {
        let info = TorrentInfo {
            piece_length: u64::MAX / 2,
            piece_hashes: vec![0u8; 60],
            file_size: u64::MAX,
            file_name: "huge.bin".into(),
        };
        // Index 3 × (u64::MAX/2) would overflow — saturate instead.
        let offset = info.piece_offset(3);
        assert_eq!(offset, u64::MAX);
    }

    /// `piece_size` for an index beyond file_size returns 0.
    ///
    /// If the piece starts past the end of the file, remaining = 0.
    #[test]
    fn piece_size_beyond_file_returns_zero() {
        let info = TorrentInfo {
            piece_length: 256,
            piece_hashes: vec![0u8; 40], // 2 pieces
            file_size: 300,
            file_name: "test.zip".into(),
        };
        // Piece 0: 256 bytes, Piece 1: 44 bytes.
        assert_eq!(info.piece_size(0), 256);
        assert_eq!(info.piece_size(1), 44);
        // Piece 2 starts at offset 512, which is past file_size 300.
        assert_eq!(info.piece_size(2), 0);
    }

    /// `piece_count` handles non-aligned piece_hashes gracefully.
    ///
    /// If piece_hashes.len() is not a multiple of 20 (malformed torrent),
    /// the division truncates — partial hashes are ignored.
    #[test]
    fn piece_count_truncates_partial_hashes() {
        let info = TorrentInfo {
            piece_length: 256,
            piece_hashes: vec![0u8; 25], // 1 full hash + 5 extra bytes
            file_size: 256,
            file_name: "test.zip".into(),
        };
        assert_eq!(info.piece_count(), 1); // 25 / 20 = 1 (truncated)
    }

    /// Empty piece_hashes means zero pieces.
    #[test]
    fn piece_count_empty_hashes() {
        let info = TorrentInfo {
            piece_length: 256,
            piece_hashes: vec![],
            file_size: 0,
            file_name: "empty.zip".into(),
        };
        assert_eq!(info.piece_count(), 0);
        assert!(info.piece_hash(0).is_none());
    }

    /// `piece_hash` with checked arithmetic won't panic on u32::MAX index.
    ///
    /// The multiplication `index * 20` could overflow usize on 32-bit.
    /// `checked_mul` prevents panic.
    #[test]
    fn piece_hash_max_index_no_panic() {
        let info = TorrentInfo {
            piece_length: 256,
            piece_hashes: vec![0u8; 40],
            file_size: 512,
            file_name: "test.zip".into(),
        };
        // u32::MAX × 20 overflows — must return None, not panic.
        assert!(info.piece_hash(u32::MAX).is_none());
    }
}
