// SPDX-License-Identifier: MIT OR Apache-2.0

//! Automatic piece tiering — migrates pieces between storage backends
//! based on access patterns, inspired by NetApp FabricPool.
//!
//! ## What
//!
//! [`TieringStorage`] wraps two [`PieceStorage`] backends (hot and cold)
//! and transparently migrates pieces between them based on demand. Hot
//! pieces (frequently read) stay on fast storage (SSD, RAM); cold pieces
//! (rarely accessed) migrate to cheap storage (HDD, S3, Glacier).
//!
//! ## Why (NetApp FabricPool pattern)
//!
//! Long-running seeders accumulate content that follows a power-law access
//! distribution: a few popular torrents account for most reads, while the
//! long tail sits idle. Keeping everything on SSD is expensive; moving
//! everything to S3 adds latency for hot content. FabricPool's solution:
//! hot data on fast tier, cold data on cheap tier, automatic migration.
//!
//! For p2p-distribute, this enables:
//!
//! - **Cost-efficient seeding** — a seeder can hold TBs of content with
//!   only the active subset on fast local storage.
//! - **R2/S3 cold tier** — use an S3-backed `PieceStorage` impl as the
//!   cold tier for zero-cost archival of rarely-seeded pieces.
//! - **Transparent access** — the coordinator and scrubber don't know
//!   about tiers; they see a single `PieceStorage`.
//!
//! ## How
//!
//! - Writes always go to the **hot tier** (fast storage).
//! - Reads check hot first, then cold. A **read from cold promotes** the
//!   piece back to hot (transparent read-promotion).
//! - The [`demote`] method moves a specific piece from hot to cold (called
//!   by an external policy loop that checks demand heat scores).
//! - Access timestamps are tracked per piece to support policy decisions.
//!
//! ## Access tracking
//!
//! Each piece has an [`AccessRecord`] that tracks last read time and total
//! read count. The policy loop (external to this module) queries
//! [`coldest_pieces`] to find demotion candidates, then calls [`demote`].
//! This module does not run timers or background tasks — it is purely
//! reactive.
//!
//! ```
//! use p2p_distribute::tiering::{AccessRecord, TieringStorage};
//! use p2p_distribute::storage::{MemoryStorage, PieceStorage};
//! use std::time::Instant;
//!
//! let hot = Box::new(MemoryStorage::new(1024));
//! let cold = Box::new(MemoryStorage::new(1024));
//! let storage = TieringStorage::new(hot, cold, 256);
//!
//! // Write goes to hot tier.
//! storage.write_piece(0, 0, &[42u8; 256]).unwrap();
//!
//! // Read comes from hot tier, records access.
//! let mut buf = vec![0u8; 256];
//! storage.read_piece(0, &mut buf).unwrap();
//!
//! // After demotion, reads come from cold tier (promoted back on read).
//! storage.demote(0).unwrap();
//! ```

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

use crate::storage::{PieceStorage, StorageError};

// ── Access tracking ─────────────────────────────────────────────────

/// Per-piece access record for tiering decisions.
#[derive(Debug, Clone)]
pub struct AccessRecord {
    /// Last time this piece was read.
    pub last_read: Instant,
    /// Total number of reads since the piece was written or promoted.
    pub read_count: u64,
}

// ── TieringStorage ──────────────────────────────────────────────────

/// Two-tier [`PieceStorage`] that migrates pieces between hot and cold backends.
///
/// ## Thread safety
///
/// Internal state (tier membership, access records) is protected by a
/// [`Mutex`]. The hot and cold backends are themselves `Send + Sync`
/// (required by `PieceStorage`), so concurrent access is safe.
pub struct TieringStorage {
    /// Fast tier — SSD, RAM, or local file.
    hot: Box<dyn PieceStorage>,
    /// Cheap tier — HDD, object storage, archival.
    cold: Box<dyn PieceStorage>,
    /// Piece length in bytes (for offset → piece index translation).
    piece_length: u64,
    /// Mutable state: which tier each piece is on + access records.
    state: Mutex<TieringState>,
}

/// Which tier a piece currently resides on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tier {
    Hot,
    Cold,
}

/// Internal mutable state for the tiering wrapper.
struct TieringState {
    /// Current tier for each known piece.
    tiers: HashMap<u32, Tier>,
    /// Access records for policy decisions.
    access: HashMap<u32, AccessRecord>,
}

impl TieringStorage {
    /// Creates a tiering wrapper over two storage backends.
    ///
    /// - `hot`: fast tier for frequently-accessed pieces
    /// - `cold`: cheap tier for archival
    /// - `piece_length`: bytes per piece (from torrent metadata)
    pub fn new(hot: Box<dyn PieceStorage>, cold: Box<dyn PieceStorage>, piece_length: u64) -> Self {
        Self {
            hot,
            cold,
            piece_length,
            state: Mutex::new(TieringState {
                tiers: HashMap::new(),
                access: HashMap::new(),
            }),
        }
    }

    /// Demotes a piece from the hot tier to the cold tier.
    ///
    /// Reads the piece data from hot, writes it to cold, then records
    /// the tier change. If the piece is already cold, this is a no-op.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the read from hot or write to cold fails.
    pub fn demote(&self, piece_index: u32) -> Result<(), StorageError> {
        let tier = {
            let st = self.state.lock().map_err(|_| StorageError::Failed {
                reason: "tiering lock poisoned".into(),
            })?;
            st.tiers.get(&piece_index).copied()
        };

        // Only demote if currently hot (or unknown — treat as hot).
        if tier == Some(Tier::Cold) {
            return Ok(());
        }

        // Read from hot. Piece_length is the max size, but the actual piece
        // may be shorter (last piece). Read what we can.
        let buf_size = usize::try_from(self.piece_length).unwrap_or(usize::MAX);
        let mut buf = vec![0u8; buf_size];
        let offset = u64::from(piece_index).saturating_mul(self.piece_length);
        let n = self.hot.read_piece(offset, &mut buf)?;

        // Write to cold.
        let data = buf.get(..n).unwrap_or(&buf);
        self.cold.write_piece(piece_index, offset, data)?;

        // Update state.
        let mut st = self.state.lock().map_err(|_| StorageError::Failed {
            reason: "tiering lock poisoned".into(),
        })?;
        st.tiers.insert(piece_index, Tier::Cold);

        Ok(())
    }

    /// Returns the coldest pieces by read recency.
    ///
    /// Returns up to `n` piece indices that are on the hot tier, sorted by
    /// oldest last_read first (best demotion candidates). Pieces with no
    /// recorded reads are returned first.
    pub fn coldest_pieces(&self, n: usize) -> Vec<u32> {
        let st = match self.state.lock() {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };

        let mut hot_pieces: Vec<(u32, Option<Instant>)> = st
            .tiers
            .iter()
            .filter(|(_, tier)| **tier == Tier::Hot)
            .map(|(&idx, _)| {
                let last_read = st.access.get(&idx).map(|r| r.last_read);
                (idx, last_read)
            })
            .collect();

        // Sort by last_read ascending (oldest first = coldest).
        // `None` (never read) sorts before `Some` — those are the coldest.
        hot_pieces.sort_by_key(|a| a.1);
        hot_pieces.truncate(n);
        hot_pieces.iter().map(|(idx, _)| *idx).collect()
    }

    /// Returns the access record for a piece, if any.
    pub fn access_record(&self, piece_index: u32) -> Option<AccessRecord> {
        self.state
            .lock()
            .ok()
            .and_then(|s| s.access.get(&piece_index).cloned())
    }

    /// Returns the current tier for a piece, if known.
    pub fn piece_tier(&self, piece_index: u32) -> Option<&'static str> {
        self.state.lock().ok().and_then(|s| {
            s.tiers.get(&piece_index).map(|t| match t {
                Tier::Hot => "hot",
                Tier::Cold => "cold",
            })
        })
    }

    /// Returns the total number of pieces tracked.
    pub fn tracked_piece_count(&self) -> usize {
        self.state.lock().map(|s| s.tiers.len()).unwrap_or(0)
    }

    /// Translates a flat byte offset into a piece index.
    fn offset_to_piece(&self, offset: u64) -> Result<u32, StorageError> {
        if self.piece_length == 0 {
            return Err(StorageError::Failed {
                reason: "piece_length is zero".into(),
            });
        }
        let idx = offset / self.piece_length;
        u32::try_from(idx).map_err(|_| StorageError::Failed {
            reason: format!("piece index {idx} exceeds u32::MAX at offset {offset}"),
        })
    }
}

impl PieceStorage for TieringStorage {
    /// Writes to the hot tier and records the piece as hot.
    fn write_piece(&self, piece_index: u32, offset: u64, data: &[u8]) -> Result<(), StorageError> {
        self.hot.write_piece(piece_index, offset, data)?;

        let mut st = self.state.lock().map_err(|_| StorageError::Failed {
            reason: "tiering lock poisoned".into(),
        })?;
        st.tiers.insert(piece_index, Tier::Hot);
        Ok(())
    }

    /// Reads from the piece's current tier, promoting from cold if needed.
    ///
    /// If the piece is cold, it is read from cold, then written to hot
    /// (transparent read-promotion). Access is recorded for tiering policy.
    fn read_piece(&self, offset: u64, buf: &mut [u8]) -> Result<usize, StorageError> {
        let piece_index = self.offset_to_piece(offset)?;

        let tier = self
            .state
            .lock()
            .map_err(|_| StorageError::Failed {
                reason: "tiering lock poisoned".into(),
            })?
            .tiers
            .get(&piece_index)
            .copied()
            .unwrap_or(Tier::Hot);

        let n = match tier {
            Tier::Hot => self.hot.read_piece(offset, buf)?,
            Tier::Cold => {
                // Read from cold.
                let n = self.cold.read_piece(offset, buf)?;

                // Promote back to hot (read-promotion).
                let data = buf.get(..n).unwrap_or(buf);
                // Best-effort promotion — don't fail the read if promotion fails.
                let _ = self.hot.write_piece(piece_index, offset, data);

                let mut st = self.state.lock().map_err(|_| StorageError::Failed {
                    reason: "tiering lock poisoned".into(),
                })?;
                st.tiers.insert(piece_index, Tier::Hot);

                n
            }
        };

        // Record access.
        let mut st = self.state.lock().map_err(|_| StorageError::Failed {
            reason: "tiering lock poisoned".into(),
        })?;
        let record = st.access.entry(piece_index).or_insert(AccessRecord {
            last_read: Instant::now(),
            read_count: 0,
        });
        record.last_read = Instant::now();
        record.read_count = record.read_count.saturating_add(1);

        Ok(n)
    }

    /// Flushes both tiers.
    fn flush(&self) -> Result<(), StorageError> {
        self.hot.flush()?;
        self.cold.flush()
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryStorage;

    fn make_tiered(piece_length: u64, size: u64) -> TieringStorage {
        TieringStorage::new(
            Box::new(MemoryStorage::new(size)),
            Box::new(MemoryStorage::new(size)),
            piece_length,
        )
    }

    // ── Write and read ──────────────────────────────────────────────

    /// Writes go to hot tier and can be read back.
    #[test]
    fn write_read_hot_tier() {
        let storage = make_tiered(256, 1024);
        storage.write_piece(0, 0, &[0xAA; 256]).unwrap();

        let mut buf = vec![0u8; 256];
        let n = storage.read_piece(0, &mut buf).unwrap();
        assert_eq!(n, 256);
        assert!(buf.iter().all(|&b| b == 0xAA));
    }

    /// New writes are tracked as hot.
    #[test]
    fn write_records_hot_tier() {
        let storage = make_tiered(256, 1024);
        storage.write_piece(0, 0, &[1; 256]).unwrap();
        assert_eq!(storage.piece_tier(0), Some("hot"));
    }

    // ── Demotion ────────────────────────────────────────────────────

    /// Demoting a piece moves it to cold tier.
    #[test]
    fn demote_moves_to_cold() {
        let storage = make_tiered(256, 1024);
        storage.write_piece(0, 0, &[0xBB; 256]).unwrap();
        assert_eq!(storage.piece_tier(0), Some("hot"));

        storage.demote(0).unwrap();
        assert_eq!(storage.piece_tier(0), Some("cold"));
    }

    /// Demoting an already-cold piece is a no-op.
    #[test]
    fn demote_cold_is_noop() {
        let storage = make_tiered(256, 1024);
        storage.write_piece(0, 0, &[0xCC; 256]).unwrap();
        storage.demote(0).unwrap();
        storage.demote(0).unwrap(); // Should not error.
        assert_eq!(storage.piece_tier(0), Some("cold"));
    }

    // ── Read-promotion ──────────────────────────────────────────────

    /// Reading a cold piece promotes it back to hot.
    #[test]
    fn read_promotes_cold_to_hot() {
        let storage = make_tiered(256, 1024);
        storage.write_piece(0, 0, &[0xDD; 256]).unwrap();
        storage.demote(0).unwrap();
        assert_eq!(storage.piece_tier(0), Some("cold"));

        // Read should succeed (from cold).
        let mut buf = vec![0u8; 256];
        let n = storage.read_piece(0, &mut buf).unwrap();
        assert_eq!(n, 256);
        assert!(buf.iter().all(|&b| b == 0xDD));

        // Piece is now hot again.
        assert_eq!(storage.piece_tier(0), Some("hot"));
    }

    // ── Access tracking ─────────────────────────────────────────────

    /// Reads record access timestamps and counts.
    #[test]
    fn reads_track_access() {
        let storage = make_tiered(256, 1024);
        storage.write_piece(0, 0, &[1; 256]).unwrap();

        let mut buf = vec![0u8; 256];
        storage.read_piece(0, &mut buf).unwrap();
        storage.read_piece(0, &mut buf).unwrap();
        storage.read_piece(0, &mut buf).unwrap();

        let record = storage.access_record(0).expect("should have access record");
        assert_eq!(record.read_count, 3);
    }

    /// Unknown pieces have no access record.
    #[test]
    fn unknown_piece_no_access_record() {
        let storage = make_tiered(256, 1024);
        assert!(storage.access_record(99).is_none());
    }

    // ── coldest_pieces ──────────────────────────────────────────────

    /// `coldest_pieces` returns hot pieces sorted by oldest access.
    #[test]
    fn coldest_pieces_sorted_by_age() {
        let storage = make_tiered(256, 4096);

        // Write three pieces.
        storage.write_piece(0, 0, &[1; 256]).unwrap();
        storage.write_piece(1, 256, &[2; 256]).unwrap();
        storage.write_piece(2, 512, &[3; 256]).unwrap();

        // Read piece 2 so it has the freshest access.
        let mut buf = vec![0u8; 256];
        storage.read_piece(512, &mut buf).unwrap();

        let coldest = storage.coldest_pieces(3);
        // Piece 2 was read most recently, so it should be last (or near last).
        // Pieces 0 and 1 were never read (only written, which doesn't record
        // access), so they may appear first.
        assert_eq!(coldest.len(), 3);
        // Piece 2 should not be first (it was most recently accessed).
        assert_ne!(coldest.first(), Some(&2));
    }

    /// `coldest_pieces` respects the limit.
    #[test]
    fn coldest_pieces_respects_limit() {
        let storage = make_tiered(256, 4096);
        storage.write_piece(0, 0, &[1; 256]).unwrap();
        storage.write_piece(1, 256, &[2; 256]).unwrap();
        storage.write_piece(2, 512, &[3; 256]).unwrap();

        let coldest = storage.coldest_pieces(1);
        assert_eq!(coldest.len(), 1);
    }

    /// `coldest_pieces` excludes cold-tier pieces.
    #[test]
    fn coldest_pieces_excludes_cold() {
        let storage = make_tiered(256, 4096);
        storage.write_piece(0, 0, &[1; 256]).unwrap();
        storage.write_piece(1, 256, &[2; 256]).unwrap();
        storage.demote(0).unwrap();

        let coldest = storage.coldest_pieces(10);
        assert!(!coldest.contains(&0), "cold piece 0 should be excluded");
        assert!(coldest.contains(&1), "hot piece 1 should be included");
    }

    // ── Flush ───────────────────────────────────────────────────────

    /// Flush succeeds on both tiers.
    #[test]
    fn flush_both_tiers() {
        let storage = make_tiered(256, 1024);
        storage.flush().unwrap();
    }

    // ── Edge cases ──────────────────────────────────────────────────

    /// Zero piece_length returns error instead of panicking.
    #[test]
    fn zero_piece_length_errors() {
        let storage = make_tiered(0, 1024);
        let mut buf = vec![0u8; 10];
        assert!(storage.read_piece(0, &mut buf).is_err());
    }

    /// Tracked piece count reflects writes and is accurate.
    #[test]
    fn tracked_piece_count() {
        let storage = make_tiered(256, 4096);
        assert_eq!(storage.tracked_piece_count(), 0);
        storage.write_piece(0, 0, &[1; 256]).unwrap();
        assert_eq!(storage.tracked_piece_count(), 1);
        storage.write_piece(1, 256, &[2; 256]).unwrap();
        assert_eq!(storage.tracked_piece_count(), 2);
    }

    /// Unknown piece tier returns None.
    #[test]
    fn unknown_piece_tier() {
        let storage = make_tiered(256, 1024);
        assert!(storage.piece_tier(42).is_none());
    }
}
