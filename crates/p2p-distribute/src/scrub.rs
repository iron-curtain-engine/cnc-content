// SPDX-License-Identifier: MIT OR Apache-2.0

//! Background integrity verification — detects silent data corruption in
//! stored pieces (NetApp RAID scrub analog).
//!
//! ## What
//!
//! A [`StorageScrubber`] reads every piece from a [`PieceStorage`] backend,
//! re-hashes it with SHA-1, and compares against the expected hash from
//! [`TorrentInfo`]. The result is a [`ScrubReport`] that classifies each
//! piece as `Healthy`, `Corrupt`, or `Unreadable`.
//!
//! ## Why — NetApp RAID Scrub lesson
//!
//! NetApp ONTAP runs a scheduled "scrub" (media scan) across all RAID
//! groups, reading every block and verifying checksums. This catches silent
//! bit-rot — where data changes on disk without any I/O error. Without
//! scrubbing, the corruption is only discovered when the block is read for
//! real work, at which point it may be too late to recover.
//!
//! The same problem exists for P2P seeders: if a verified piece goes bad on
//! disk, the seeder unknowingly serves corrupt data. Other peers receive it,
//! fail hash verification, blame the seeder, and waste bandwidth. Periodic
//! scrubbing detects the corruption proactively so the seeder can re-fetch
//! the piece or stop advertising it.
//!
//! ## How
//!
//! 1. Create a [`StorageScrubber`] with torrent info and scrub config.
//! 2. Call [`scrub()`](StorageScrubber::scrub) with a storage backend and
//!    progress callback.
//! 3. The scrubber reads each piece, hashes it, and reports status.
//! 4. The [`ScrubReport`] summarises results and identifies corrupt pieces.
//!
//! ```
//! use p2p_distribute::scrub::{StorageScrubber, ScrubConfig, PieceHealth};
//! use p2p_distribute::storage::MemoryStorage;
//! use p2p_distribute::PieceStorage;
//! use p2p_distribute::torrent_info::TorrentInfo;
//! use sha1::{Sha1, Digest};
//!
//! // Build a trivial torrent: one piece of 4 bytes.
//! let data = b"test";
//! let hash: [u8; 20] = Sha1::digest(data).into();
//! let info = TorrentInfo {
//!     piece_length: 4,
//!     piece_hashes: hash.to_vec(),
//!     file_size: 4,
//!     file_name: "test.bin".into(),
//! };
//!
//! let storage = MemoryStorage::new(4);
//! storage.write_piece(0, 0, data).unwrap();
//!
//! let scrubber = StorageScrubber::new(&info, ScrubConfig::default());
//! let report = scrubber.scrub(&storage, &mut |_| {}).unwrap();
//!
//! assert_eq!(report.healthy_count(), 1);
//! assert_eq!(report.corrupt_count(), 0);
//! assert!(matches!(report.piece_health(0), Some(PieceHealth::Healthy)));
//! ```

use std::time::{Duration, Instant};

use sha1::{Digest, Sha1};
use thiserror::Error;

use crate::storage::PieceStorage;
use crate::torrent_info::TorrentInfo;

// ── Configuration ───────────────────────────────────────────────────

/// Configuration for a scrub pass.
///
/// ## Design (informed by NetApp ONTAP media scan settings)
///
/// NetApp exposes `storage disk option modify -node * -autoassign` and
/// `disk option show` for scrub scheduling. Key tunable: whether to stop
/// on first error or complete the full scan. We expose the same choice.
#[derive(Debug, Clone)]
pub struct ScrubConfig {
    /// Whether to continue scrubbing after the first corrupt piece.
    ///
    /// `true` (default): scan all pieces, report all corruption at once.
    /// `false`: stop at the first corrupt or unreadable piece.
    pub continue_on_error: bool,

    /// Maximum time to spend on the scrub pass. `None` means no limit.
    ///
    /// If the deadline is reached mid-scrub, the report includes only
    /// the pieces scanned so far. This lets callers bound scrub cost on
    /// large datasets (NetApp's "scrub window" concept).
    pub deadline: Option<Duration>,
}

impl Default for ScrubConfig {
    fn default() -> Self {
        Self {
            continue_on_error: true,
            deadline: None,
        }
    }
}

// ── Per-piece health ────────────────────────────────────────────────

/// Health status of a single stored piece after scrub verification.
///
/// Maps to NetApp's per-block classification: good, checksum-error, or
/// media-error. The distinction between `Corrupt` and `Unreadable` is
/// important: corrupt data was read but doesn't match the expected hash,
/// while unreadable data couldn't be read at all (I/O error).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PieceHealth {
    /// Piece data matches the expected SHA-1 hash — no corruption.
    Healthy,
    /// Piece data was read but the SHA-1 hash does not match.
    ///
    /// This is silent bit-rot: the storage returned data without an I/O
    /// error, but the data is wrong. The seeder is unknowingly serving
    /// corrupt data.
    Corrupt {
        /// Expected SHA-1 from torrent metadata.
        expected: [u8; 20],
        /// Actual SHA-1 computed from the stored data.
        actual: [u8; 20],
    },
    /// Piece could not be read from storage (I/O error).
    ///
    /// The storage backend reported an error — the medium may be degraded.
    /// Unlike `Corrupt`, we don't know what the data looks like.
    Unreadable {
        /// Human-readable error detail.
        detail: String,
    },
    /// Piece was not scanned (scrub stopped early due to deadline or
    /// `continue_on_error = false`).
    Skipped,
}

// ── Progress callback ───────────────────────────────────────────────

/// Progress event emitted during a scrub pass.
#[derive(Debug, Clone)]
pub enum ScrubProgress {
    /// A piece was verified.
    PieceChecked {
        /// 0-based piece index.
        piece_index: u32,
        /// Health result for this piece.
        health: PieceHealth,
        /// Pieces checked so far (including this one).
        checked: u32,
        /// Total pieces to check.
        total: u32,
    },
    /// The scrub was stopped early (deadline or first-error policy).
    Stopped {
        /// Pieces checked before stopping.
        checked: u32,
        /// Total pieces.
        total: u32,
        /// Reason the scrub stopped.
        reason: StopReason,
    },
}

/// Why a scrub was stopped early.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    /// `continue_on_error` was `false` and a corrupt/unreadable piece was found.
    FirstError,
    /// The configured deadline was reached.
    DeadlineReached,
}

// ── ScrubReport ─────────────────────────────────────────────────────

/// Summary of a completed (or partially completed) scrub pass.
///
/// Analogous to NetApp's `storage disk show -broken` and RAID scrub
/// summary logs. Provides both aggregate counts and per-piece detail.
///
/// ```
/// use p2p_distribute::scrub::{ScrubReport, PieceHealth};
///
/// let report = ScrubReport::new(vec![
///     PieceHealth::Healthy,
///     PieceHealth::Healthy,
///     PieceHealth::Corrupt {
///         expected: [0; 20],
///         actual: [1; 20],
///     },
/// ], std::time::Duration::from_secs(2));
///
/// assert_eq!(report.total_pieces(), 3);
/// assert_eq!(report.healthy_count(), 2);
/// assert_eq!(report.corrupt_count(), 1);
/// assert_eq!(report.corrupt_piece_indices(), vec![2]);
/// assert!(!report.is_clean());
/// ```
#[derive(Debug, Clone)]
pub struct ScrubReport {
    /// Per-piece health results, indexed by piece number.
    pieces: Vec<PieceHealth>,
    /// Wall-clock time spent on the scrub.
    elapsed: Duration,
}

impl ScrubReport {
    /// Creates a report from per-piece results.
    pub fn new(pieces: Vec<PieceHealth>, elapsed: Duration) -> Self {
        Self { pieces, elapsed }
    }

    /// Total number of pieces in the torrent.
    pub fn total_pieces(&self) -> u32 {
        self.pieces.len() as u32
    }

    /// Number of healthy pieces.
    pub fn healthy_count(&self) -> u32 {
        self.pieces
            .iter()
            .filter(|h| matches!(h, PieceHealth::Healthy))
            .count() as u32
    }

    /// Number of corrupt pieces (data read OK, hash mismatch).
    pub fn corrupt_count(&self) -> u32 {
        self.pieces
            .iter()
            .filter(|h| matches!(h, PieceHealth::Corrupt { .. }))
            .count() as u32
    }

    /// Number of unreadable pieces (I/O errors).
    pub fn unreadable_count(&self) -> u32 {
        self.pieces
            .iter()
            .filter(|h| matches!(h, PieceHealth::Unreadable { .. }))
            .count() as u32
    }

    /// Number of skipped pieces (scrub stopped early).
    pub fn skipped_count(&self) -> u32 {
        self.pieces
            .iter()
            .filter(|h| matches!(h, PieceHealth::Skipped))
            .count() as u32
    }

    /// Returns indices of all corrupt pieces.
    pub fn corrupt_piece_indices(&self) -> Vec<u32> {
        self.pieces
            .iter()
            .enumerate()
            .filter(|(_, h)| matches!(h, PieceHealth::Corrupt { .. }))
            .map(|(i, _)| i as u32)
            .collect()
    }

    /// Returns indices of all unreadable pieces.
    pub fn unreadable_piece_indices(&self) -> Vec<u32> {
        self.pieces
            .iter()
            .enumerate()
            .filter(|(_, h)| matches!(h, PieceHealth::Unreadable { .. }))
            .map(|(i, _)| i as u32)
            .collect()
    }

    /// Health status of a specific piece.
    pub fn piece_health(&self, piece_index: u32) -> Option<&PieceHealth> {
        self.pieces.get(piece_index as usize)
    }

    /// Whether the scrub found zero corruption (all checked pieces healthy).
    pub fn is_clean(&self) -> bool {
        self.corrupt_count() == 0 && self.unreadable_count() == 0
    }

    /// Time spent on the scrub pass.
    pub fn elapsed(&self) -> Duration {
        self.elapsed
    }
}

// ── ScrubError ──────────────────────────────────────────────────────

/// Errors from the scrub process itself (not per-piece failures).
#[derive(Debug, Error)]
pub enum ScrubError {
    /// No piece hashes available in torrent info — nothing to verify against.
    #[error("no piece hashes in torrent info — cannot scrub")]
    NoPieceHashes,
}

// ── StorageScrubber ─────────────────────────────────────────────────

/// Reads stored pieces and verifies their SHA-1 hashes against torrent
/// metadata.
///
/// ## Design
///
/// The scrubber is stateless — it borrows `TorrentInfo` and `ScrubConfig`,
/// reads from any `PieceStorage`, and returns a `ScrubReport`. This makes
/// it trivially composable: callers can scrub on a timer, on demand, or
/// as a post-download verification pass.
///
/// The coordinator can use `corrupt_piece_indices()` from the report to
/// schedule re-downloads, matching NetApp's automatic "RAID reconstruct"
/// after a scrub failure.
pub struct StorageScrubber<'info> {
    info: &'info TorrentInfo,
    config: ScrubConfig,
}

impl<'info> StorageScrubber<'info> {
    /// Creates a scrubber for the given torrent.
    pub fn new(info: &'info TorrentInfo, config: ScrubConfig) -> Self {
        Self { info, config }
    }

    /// Runs the scrub pass over all pieces in the storage backend.
    ///
    /// Reads each piece, computes SHA-1, compares against the expected hash
    /// from torrent metadata. Calls `on_progress` after each piece.
    ///
    /// Returns a `ScrubReport` with per-piece health and aggregate counts.
    pub fn scrub(
        &self,
        storage: &dyn PieceStorage,
        on_progress: &mut dyn FnMut(ScrubProgress),
    ) -> Result<ScrubReport, ScrubError> {
        let piece_count = self.info.piece_count() as usize;
        if piece_count == 0 {
            return Err(ScrubError::NoPieceHashes);
        }

        let start = Instant::now();
        let mut results = Vec::with_capacity(piece_count);
        let piece_length = self.info.piece_length;

        for i in 0..piece_count {
            // Check deadline before each piece.
            if let Some(deadline) = self.config.deadline {
                if start.elapsed() >= deadline {
                    // Fill remaining pieces as Skipped.
                    while results.len() < piece_count {
                        results.push(PieceHealth::Skipped);
                    }
                    on_progress(ScrubProgress::Stopped {
                        checked: i as u32,
                        total: piece_count as u32,
                        reason: StopReason::DeadlineReached,
                    });
                    return Ok(ScrubReport::new(results, start.elapsed()));
                }
            }

            let piece_index = i as u32;
            let offset = (i as u64).saturating_mul(piece_length);

            // Last piece may be shorter than piece_length.
            let this_piece_len = if i == piece_count - 1 {
                let remainder = self.info.file_size.saturating_sub(offset);
                remainder.min(piece_length) as usize
            } else {
                piece_length as usize
            };

            let health = self.verify_piece(storage, piece_index, offset, this_piece_len);

            let is_error = !matches!(health, PieceHealth::Healthy);
            results.push(health.clone());

            on_progress(ScrubProgress::PieceChecked {
                piece_index,
                health,
                checked: (i + 1) as u32,
                total: piece_count as u32,
            });

            // Stop on first error if configured.
            if is_error && !self.config.continue_on_error {
                while results.len() < piece_count {
                    results.push(PieceHealth::Skipped);
                }
                on_progress(ScrubProgress::Stopped {
                    checked: (i + 1) as u32,
                    total: piece_count as u32,
                    reason: StopReason::FirstError,
                });
                return Ok(ScrubReport::new(results, start.elapsed()));
            }
        }

        Ok(ScrubReport::new(results, start.elapsed()))
    }

    /// Reads and verifies a single piece.
    fn verify_piece(
        &self,
        storage: &dyn PieceStorage,
        piece_index: u32,
        offset: u64,
        length: usize,
    ) -> PieceHealth {
        // Read piece data from storage.
        let mut buf = vec![0u8; length];
        match storage.read_piece(offset, &mut buf) {
            Ok(n) if n == length => {}
            Ok(n) => {
                return PieceHealth::Unreadable {
                    detail: format!(
                        "short read for piece {piece_index}: expected {length} bytes, got {n}"
                    ),
                };
            }
            Err(e) => {
                return PieceHealth::Unreadable {
                    detail: format!("I/O error reading piece {piece_index}: {e}"),
                };
            }
        }

        // Compute SHA-1 of the stored data.
        let actual_hash: [u8; 20] = Sha1::digest(&buf).into();

        // Compare against expected hash from torrent metadata.
        let Some(expected_slice) = self.info.piece_hash(piece_index) else {
            return PieceHealth::Unreadable {
                detail: format!("no expected hash for piece {piece_index}"),
            };
        };

        // Convert the 20-byte slice to a fixed array for structured reporting.
        let mut expected_hash = [0u8; 20];
        expected_hash.copy_from_slice(expected_slice);

        if actual_hash == expected_hash {
            PieceHealth::Healthy
        } else {
            PieceHealth::Corrupt {
                expected: expected_hash,
                actual: actual_hash,
            }
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryStorage;

    /// Helper: build TorrentInfo with correct SHA-1 hashes for the given data.
    fn torrent_with_data(data: &[u8], piece_length: u64) -> TorrentInfo {
        let piece_count =
            data.len().saturating_add(piece_length as usize - 1) / piece_length as usize;
        let mut hashes = Vec::with_capacity(piece_count * 20);
        for i in 0..piece_count {
            let start = i * piece_length as usize;
            let end = (start + piece_length as usize).min(data.len());
            let hash: [u8; 20] = Sha1::digest(data.get(start..end).unwrap_or_default()).into();
            hashes.extend_from_slice(&hash);
        }
        TorrentInfo {
            piece_length,
            piece_hashes: hashes,
            file_size: data.len() as u64,
            file_name: "test.bin".into(),
        }
    }

    /// Helper: build storage with the given data.
    fn storage_with_data(data: &[u8]) -> MemoryStorage {
        let storage = MemoryStorage::new(data.len() as u64);
        storage.write_piece(0, 0, data).unwrap();
        storage
    }

    // ── Happy path ──────────────────────────────────────────────────

    /// All pieces healthy when storage contains correct data.
    ///
    /// The most common case — a seeder with intact data.
    #[test]
    fn all_pieces_healthy() {
        let data = b"hello world, this is test data!!"; // 32 bytes
        let info = torrent_with_data(data, 16); // 2 pieces of 16 bytes
        let storage = storage_with_data(data);

        let scrubber = StorageScrubber::new(&info, ScrubConfig::default());
        let report = scrubber.scrub(&storage, &mut |_| {}).unwrap();

        assert_eq!(report.total_pieces(), 2);
        assert_eq!(report.healthy_count(), 2);
        assert_eq!(report.corrupt_count(), 0);
        assert_eq!(report.unreadable_count(), 0);
        assert!(report.is_clean());
        assert!(report.corrupt_piece_indices().is_empty());
    }

    /// Single piece torrent with matching data is healthy.
    #[test]
    fn single_piece_healthy() {
        let data = b"test";
        let info = torrent_with_data(data, 4);
        let storage = storage_with_data(data);

        let scrubber = StorageScrubber::new(&info, ScrubConfig::default());
        let report = scrubber.scrub(&storage, &mut |_| {}).unwrap();

        assert_eq!(report.healthy_count(), 1);
        assert!(report.is_clean());
        assert!(matches!(report.piece_health(0), Some(PieceHealth::Healthy)));
    }

    // ── Corruption detection ────────────────────────────────────────

    /// Corrupt piece is detected when stored data differs from expected hash.
    ///
    /// Simulates silent bit-rot: the data was valid at write time but
    /// changed on the medium afterwards. The scrubber must flag it.
    #[test]
    fn detects_corrupt_piece() {
        let data = b"hello world, this is test data!!"; // 32 bytes
        let info = torrent_with_data(data, 16);

        // Write correct data, then corrupt the second piece.
        let storage = MemoryStorage::new(32);
        storage.write_piece(0, 0, &data[..16]).unwrap();
        storage.write_piece(1, 16, b"CORRUPT_DATA!!!!").unwrap();

        let scrubber = StorageScrubber::new(&info, ScrubConfig::default());
        let report = scrubber.scrub(&storage, &mut |_| {}).unwrap();

        assert_eq!(report.healthy_count(), 1);
        assert_eq!(report.corrupt_count(), 1);
        assert!(!report.is_clean());
        assert_eq!(report.corrupt_piece_indices(), vec![1]);
    }

    /// First piece corrupt, second healthy.
    #[test]
    fn first_piece_corrupt() {
        let data = b"aabbccdd"; // 8 bytes, 2 pieces of 4
        let info = torrent_with_data(data, 4);

        let storage = MemoryStorage::new(8);
        storage.write_piece(0, 0, b"XXXX").unwrap(); // wrong
        storage.write_piece(1, 4, b"ccdd").unwrap(); // correct

        let scrubber = StorageScrubber::new(&info, ScrubConfig::default());
        let report = scrubber.scrub(&storage, &mut |_| {}).unwrap();

        assert_eq!(report.corrupt_count(), 1);
        assert_eq!(report.healthy_count(), 1);
        assert_eq!(report.corrupt_piece_indices(), vec![0]);
    }

    // ── Short read (unreadable) ─────────────────────────────────────

    /// Short read is reported as unreadable.
    ///
    /// If storage returns fewer bytes than expected, the piece cannot be
    /// verified — it may have been partially written or the medium is
    /// truncated.
    #[test]
    fn short_read_is_unreadable() {
        let data = b"hello world!"; // 12 bytes
        let info = torrent_with_data(data, 12);

        // Create storage with less data than expected.
        let storage = MemoryStorage::new(8);
        storage.write_piece(0, 0, b"hello wo").unwrap();

        let scrubber = StorageScrubber::new(&info, ScrubConfig::default());
        let report = scrubber.scrub(&storage, &mut |_| {}).unwrap();

        assert_eq!(report.unreadable_count(), 1);
        assert_eq!(report.unreadable_piece_indices(), vec![0]);
    }

    // ── Stop-on-first-error ─────────────────────────────────────────

    /// Scrub stops at first error when continue_on_error is false.
    ///
    /// NetApp supports both full-scan and fail-fast modes. Fail-fast is
    /// useful for quick health checks.
    #[test]
    fn stop_on_first_error() {
        let data = b"aabbccddee"; // 10 bytes, 3 pieces (4+4+2)
        let info = torrent_with_data(data, 4);

        // Corrupt the first piece.
        let storage = MemoryStorage::new(10);
        storage.write_piece(0, 0, b"XXXX").unwrap();
        storage.write_piece(1, 4, b"ccdd").unwrap();
        storage.write_piece(2, 8, b"ee").unwrap();

        let config = ScrubConfig {
            continue_on_error: false,
            deadline: None,
        };
        let scrubber = StorageScrubber::new(&info, config);
        let report = scrubber.scrub(&storage, &mut |_| {}).unwrap();

        assert_eq!(report.corrupt_count(), 1);
        assert_eq!(report.skipped_count(), 2); // pieces 1 and 2 skipped
        assert_eq!(report.total_pieces(), 3);
    }

    // ── Empty torrent ───────────────────────────────────────────────

    /// Scrubbing a torrent with no piece hashes returns an error.
    ///
    /// There's nothing to verify against — the caller probably has
    /// incomplete metadata.
    #[test]
    fn no_piece_hashes_error() {
        let info = TorrentInfo {
            piece_length: 256,
            piece_hashes: vec![],
            file_size: 0,
            file_name: "empty.bin".into(),
        };
        let storage = MemoryStorage::new(0);
        let scrubber = StorageScrubber::new(&info, ScrubConfig::default());
        assert!(scrubber.scrub(&storage, &mut |_| {}).is_err());
    }

    // ── Progress callback ───────────────────────────────────────────

    /// Progress callback is invoked for each piece.
    ///
    /// The coordinator or UI can track scrub progress in real time.
    #[test]
    fn progress_callback_fires_per_piece() {
        let data = b"aabbccdd"; // 8 bytes, 2 pieces of 4
        let info = torrent_with_data(data, 4);
        let storage = storage_with_data(data);

        let mut events = Vec::new();
        let scrubber = StorageScrubber::new(&info, ScrubConfig::default());
        scrubber.scrub(&storage, &mut |e| events.push(e)).unwrap();

        assert_eq!(events.len(), 2); // one per piece
        if let ScrubProgress::PieceChecked {
            piece_index,
            checked,
            total,
            ..
        } = &events[0]
        {
            assert_eq!(*piece_index, 0);
            assert_eq!(*checked, 1);
            assert_eq!(*total, 2);
        } else {
            panic!("expected PieceChecked event");
        }
    }

    /// Stop event is emitted when scrub stops early.
    #[test]
    fn stop_event_on_first_error() {
        let data = b"aabbccdd";
        let info = torrent_with_data(data, 4);

        let storage = MemoryStorage::new(8);
        storage.write_piece(0, 0, b"XXXX").unwrap();
        storage.write_piece(1, 4, b"ccdd").unwrap();

        let config = ScrubConfig {
            continue_on_error: false,
            deadline: None,
        };
        let mut events = Vec::new();
        let scrubber = StorageScrubber::new(&info, config);
        scrubber.scrub(&storage, &mut |e| events.push(e)).unwrap();

        // Should have: PieceChecked(0, Corrupt) + Stopped
        assert!(events.len() >= 2);
        assert!(matches!(
            events.last(),
            Some(ScrubProgress::Stopped {
                reason: StopReason::FirstError,
                ..
            })
        ));
    }

    // ── Determinism ─────────────────────────────────────────────────

    /// Same data produces same scrub results.
    ///
    /// Scrub results must be deterministic for reproducible diagnostics.
    #[test]
    fn scrub_deterministic() {
        let data = b"determinism test data here!12345"; // 31 bytes
        let info = torrent_with_data(data, 10); // 4 pieces (10+10+10+1)
        let storage = storage_with_data(data);

        let scrubber = StorageScrubber::new(&info, ScrubConfig::default());
        let r1 = scrubber.scrub(&storage, &mut |_| {}).unwrap();
        let r2 = scrubber.scrub(&storage, &mut |_| {}).unwrap();

        assert_eq!(r1.healthy_count(), r2.healthy_count());
        assert_eq!(r1.corrupt_count(), r2.corrupt_count());
        assert_eq!(r1.total_pieces(), r2.total_pieces());
    }

    // ── Last-piece edge case ────────────────────────────────────────

    /// Last piece shorter than piece_length is handled correctly.
    ///
    /// File size is not always a multiple of piece_length. The last piece
    /// must be hashed with its actual size, not padded.
    #[test]
    fn last_piece_shorter() {
        let data = b"hello world!!"; // 13 bytes, piece_length=8 → 2 pieces (8+5)
        let info = torrent_with_data(data, 8);
        let storage = storage_with_data(data);

        let scrubber = StorageScrubber::new(&info, ScrubConfig::default());
        let report = scrubber.scrub(&storage, &mut |_| {}).unwrap();

        assert_eq!(report.total_pieces(), 2);
        assert_eq!(report.healthy_count(), 2);
        assert!(report.is_clean());
    }

    // ── ScrubReport accessors ───────────────────────────────────────

    /// `piece_health` returns None for out-of-range indices.
    #[test]
    fn piece_health_out_of_range() {
        let report = ScrubReport::new(vec![PieceHealth::Healthy], Duration::from_secs(0));
        assert!(report.piece_health(0).is_some());
        assert!(report.piece_health(1).is_none());
    }

    /// Empty report (impossible in practice) is clean.
    #[test]
    fn empty_report_is_clean() {
        let report = ScrubReport::new(vec![], Duration::from_secs(0));
        assert!(report.is_clean());
        assert_eq!(report.total_pieces(), 0);
    }

    // ── Display ─────────────────────────────────────────────────────

    /// `ScrubError::NoPieceHashes` has a meaningful message.
    #[test]
    fn scrub_error_display() {
        let err = ScrubError::NoPieceHashes;
        let msg = err.to_string();
        assert!(msg.contains("piece hashes"), "should mention hashes: {msg}");
    }
}
