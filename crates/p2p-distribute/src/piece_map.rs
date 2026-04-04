// SPDX-License-Identifier: MIT OR Apache-2.0

//! Atomic piece-state tracking for concurrent downloads.
//!
//! [`SharedPieceMap`] tracks the download state of every piece using atomic
//! operations. Multiple threads (one per peer) can atomically claim pieces
//! and report completion without locks.

use std::sync::atomic::{AtomicU8, Ordering};

// ── Piece state ─────────────────────────────────────────────────────

/// State of a single piece in the coordinator's atomic piece map.
///
/// Encoded as `u8` for `AtomicU8` storage. Transitions:
/// ```text
/// Needed ──→ InFlight ──→ Done
///                │
///                └──→ Failed ──→ Needed (retry)
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PieceState {
    /// Piece has not been downloaded yet.
    Needed = 0,
    /// Piece is currently being fetched by a peer.
    InFlight = 1,
    /// Piece has been downloaded and SHA-1 verified.
    Done = 2,
    /// Piece download or verification failed — eligible for retry.
    Failed = 3,
}

impl PieceState {
    /// Convert from raw `u8` value, defaulting to `Needed` for unknown values.
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::InFlight,
            2 => Self::Done,
            3 => Self::Failed,
            _ => Self::Needed,
        }
    }
}

// ── SharedPieceMap ──────────────────────────────────────────────────

/// Thread-safe piece state tracker using atomic operations.
///
/// Each piece is tracked by an `AtomicU8` encoding a [`PieceState`]. Multiple
/// threads (one per peer) can atomically claim pieces and report completion
/// without locks.
pub struct SharedPieceMap {
    /// One `AtomicU8` per piece, indexed by piece number.
    states: Vec<AtomicU8>,
    /// Total number of pieces.
    piece_count: u32,
}

impl SharedPieceMap {
    /// Creates a new piece map with all pieces in `Needed` state.
    pub fn new(piece_count: u32) -> Self {
        let mut states = Vec::with_capacity(piece_count as usize);
        for _ in 0..piece_count {
            states.push(AtomicU8::new(PieceState::Needed as u8));
        }
        Self {
            states,
            piece_count,
        }
    }

    /// Creates a piece map with pre-verified pieces already marked `Done`.
    ///
    /// `verified_pieces` is a bitset: if `verified_pieces[i]` is `true`, piece
    /// `i` is marked `Done` (already on disk and SHA-1 verified). All other
    /// pieces start as `Needed`. This enables **resume from partial state** —
    /// the coordinator skips already-downloaded pieces.
    ///
    /// ## Why
    ///
    /// Every production P2P client (aria2, Resilio, Syncthing) supports resume.
    /// Without this, an interrupted download restarts from zero, wasting
    /// bandwidth and time. Design doc D049 requires this for the 5–50 MB and
    /// >50 MB tiers where downloads may be long-running.
    pub fn from_verified(piece_count: u32, verified_pieces: &[bool]) -> Self {
        let mut states = Vec::with_capacity(piece_count as usize);
        for i in 0..piece_count as usize {
            let initial = if verified_pieces.get(i).copied().unwrap_or(false) {
                PieceState::Done as u8
            } else {
                PieceState::Needed as u8
            };
            states.push(AtomicU8::new(initial));
        }
        Self {
            states,
            piece_count,
        }
    }

    /// Returns the total number of pieces.
    pub fn piece_count(&self) -> u32 {
        self.piece_count
    }

    /// Gets the current state of a piece.
    pub fn get(&self, index: u32) -> PieceState {
        self.states
            .get(index as usize)
            .map(|a| PieceState::from_u8(a.load(Ordering::Acquire)))
            .unwrap_or(PieceState::Needed)
    }

    /// Atomically transitions a piece from `Needed` to `InFlight`.
    ///
    /// Returns `true` if the transition succeeded (this thread "claimed" the piece).
    /// Returns `false` if the piece was already `InFlight`, `Done`, or `Failed`.
    pub fn try_claim(&self, index: u32) -> bool {
        self.states
            .get(index as usize)
            .map(|a| {
                a.compare_exchange(
                    PieceState::Needed as u8,
                    PieceState::InFlight as u8,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            })
            .unwrap_or(false)
    }

    /// Marks a piece as `Done` (successfully downloaded and verified).
    pub fn mark_done(&self, index: u32) {
        if let Some(a) = self.states.get(index as usize) {
            a.store(PieceState::Done as u8, Ordering::Release);
        }
    }

    /// Marks a piece as `Failed` (download or verification error).
    pub fn mark_failed(&self, index: u32) {
        if let Some(a) = self.states.get(index as usize) {
            a.store(PieceState::Failed as u8, Ordering::Release);
        }
    }

    /// Resets a `Failed` piece back to `Needed` for retry.
    ///
    /// Returns `true` if the piece was `Failed` and successfully reset.
    pub fn retry_failed(&self, index: u32) -> bool {
        self.states
            .get(index as usize)
            .map(|a| {
                a.compare_exchange(
                    PieceState::Failed as u8,
                    PieceState::Needed as u8,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            })
            .unwrap_or(false)
    }

    /// Returns the number of pieces in `Done` state.
    pub fn done_count(&self) -> u32 {
        self.states
            .iter()
            .filter(|a| a.load(Ordering::Acquire) == PieceState::Done as u8)
            .count() as u32
    }

    /// Returns `true` when all pieces are `Done`.
    pub fn is_complete(&self) -> bool {
        self.done_count() == self.piece_count
    }

    /// Returns the index of the next `Needed` piece, if any.
    ///
    /// Scans sequentially from the start. The caller may use a more
    /// sophisticated selection strategy (rarest-first, sequential for
    /// streaming) by scanning the full map instead.
    pub fn next_needed(&self) -> Option<u32> {
        self.states
            .iter()
            .enumerate()
            .find(|(_, a)| a.load(Ordering::Acquire) == PieceState::Needed as u8)
            .map(|(i, _)| i as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── PieceState ──────────────────────────────────────────────────

    /// `PieceState::from_u8` correctly maps known values.
    ///
    /// Each raw byte value must map to the corresponding state. Unknown values
    /// default to `Needed` to ensure safe fallback for corrupt atomic reads.
    #[test]
    fn piece_state_from_u8_known_values() {
        assert_eq!(PieceState::from_u8(0), PieceState::Needed);
        assert_eq!(PieceState::from_u8(1), PieceState::InFlight);
        assert_eq!(PieceState::from_u8(2), PieceState::Done);
        assert_eq!(PieceState::from_u8(3), PieceState::Failed);
    }

    /// `PieceState::from_u8` defaults to `Needed` for unknown values.
    ///
    /// Out-of-range values must never panic. They map to `Needed` as a safe
    /// default, preventing corrupt state from blocking downloads.
    #[test]
    fn piece_state_from_u8_unknown_defaults_to_needed() {
        assert_eq!(PieceState::from_u8(4), PieceState::Needed);
        assert_eq!(PieceState::from_u8(255), PieceState::Needed);
    }

    // ── SharedPieceMap ──────────────────────────────────────────────

    /// New `SharedPieceMap` starts with all pieces in `Needed` state.
    ///
    /// This invariant ensures no pieces are accidentally skipped at startup.
    #[test]
    fn piece_map_initial_state_all_needed() {
        let map = SharedPieceMap::new(4);
        assert_eq!(map.piece_count(), 4);
        assert_eq!(map.done_count(), 0);
        assert!(!map.is_complete());
        for i in 0..4 {
            assert_eq!(map.get(i), PieceState::Needed);
        }
    }

    /// `try_claim` transitions `Needed → InFlight` exactly once.
    ///
    /// Only the first caller that claims a piece succeeds. Subsequent attempts
    /// return `false`, preventing duplicate downloads.
    #[test]
    fn piece_map_try_claim_succeeds_once() {
        let map = SharedPieceMap::new(2);
        assert!(map.try_claim(0));
        assert!(!map.try_claim(0)); // already InFlight
        assert_eq!(map.get(0), PieceState::InFlight);
    }

    /// `mark_done` transitions a piece to `Done` and updates the done count.
    ///
    /// The done count must reflect completed pieces accurately for progress
    /// reporting.
    #[test]
    fn piece_map_mark_done_updates_count() {
        let map = SharedPieceMap::new(3);
        map.try_claim(0);
        map.mark_done(0);
        assert_eq!(map.get(0), PieceState::Done);
        assert_eq!(map.done_count(), 1);
    }

    /// `is_complete()` returns `true` only when ALL pieces are `Done`.
    ///
    /// This is the coordinator's termination condition — downloading must
    /// continue until every piece is verified.
    #[test]
    fn piece_map_is_complete_all_done() {
        let map = SharedPieceMap::new(2);
        map.try_claim(0);
        map.mark_done(0);
        assert!(!map.is_complete());
        map.try_claim(1);
        map.mark_done(1);
        assert!(map.is_complete());
    }

    /// `mark_failed` + `retry_failed` cycle allows piece re-download.
    ///
    /// Failed pieces must be retryable by returning them to `Needed` state.
    /// This is critical for resilience: a single bad HTTP response shouldn't
    /// permanently fail the download.
    #[test]
    fn piece_map_fail_then_retry() {
        let map = SharedPieceMap::new(1);
        map.try_claim(0);
        map.mark_failed(0);
        assert_eq!(map.get(0), PieceState::Failed);
        assert!(map.retry_failed(0));
        assert_eq!(map.get(0), PieceState::Needed);
        // Can claim again after retry.
        assert!(map.try_claim(0));
    }

    /// `retry_failed` returns `false` for non-failed pieces.
    ///
    /// Only `Failed` pieces can be retried. Retrying a `Done` or `InFlight`
    /// piece would corrupt state.
    #[test]
    fn piece_map_retry_non_failed_returns_false() {
        let map = SharedPieceMap::new(1);
        assert!(!map.retry_failed(0)); // Needed, not Failed
        map.try_claim(0);
        assert!(!map.retry_failed(0)); // InFlight, not Failed
        map.mark_done(0);
        assert!(!map.retry_failed(0)); // Done, not Failed
    }

    /// `next_needed` returns the first `Needed` piece.
    ///
    /// The sequential scan is the coordinator's default piece selection. It
    /// optimizes for web seed access (sequential HTTP ranges).
    #[test]
    fn piece_map_next_needed_sequential() {
        let map = SharedPieceMap::new(3);
        assert_eq!(map.next_needed(), Some(0));
        map.try_claim(0);
        assert_eq!(map.next_needed(), Some(1));
        map.try_claim(1);
        map.try_claim(2);
        assert_eq!(map.next_needed(), None); // all InFlight
    }

    /// Out-of-bounds piece index access is safe.
    ///
    /// The coordinator must never panic on invalid indices. `get()` returns
    /// `Needed` and `try_claim()` returns `false` for out-of-bounds.
    #[test]
    fn piece_map_out_of_bounds_safe() {
        let map = SharedPieceMap::new(1);
        assert_eq!(map.get(999), PieceState::Needed);
        assert!(!map.try_claim(999));
        map.mark_done(999); // no-op, no panic
        map.mark_failed(999); // no-op, no panic
        assert!(!map.retry_failed(999));
    }

    /// Concurrent `try_claim` from multiple threads claims each piece exactly once.
    ///
    /// This tests the atomic CAS correctness: with N threads racing to claim
    /// the same piece, exactly one must succeed and the rest must fail.
    #[test]
    fn piece_map_concurrent_claim_exactly_once() {
        use std::sync::Arc;

        let map = Arc::new(SharedPieceMap::new(1));
        let threads: Vec<_> = (0..10)
            .map(|_| {
                let m = Arc::clone(&map);
                std::thread::spawn(move || m.try_claim(0))
            })
            .collect();

        let mut successes = 0u32;
        for t in threads {
            if t.join().unwrap() {
                successes += 1;
            }
        }
        assert_eq!(successes, 1, "exactly one thread should claim the piece");
        assert_eq!(map.get(0), PieceState::InFlight);
    }

    /// Zero-piece map is immediately complete.
    ///
    /// Edge case: a torrent with no pieces should report is_complete
    /// immediately (0 done == 0 needed).
    #[test]
    fn piece_map_zero_pieces_is_complete() {
        let map = SharedPieceMap::new(0);
        assert_eq!(map.piece_count(), 0);
        assert!(map.is_complete());
        assert_eq!(map.done_count(), 0);
        assert!(map.next_needed().is_none());
    }

    /// `from_verified` with empty slice and zero pieces.
    #[test]
    fn piece_map_from_verified_empty() {
        let map = SharedPieceMap::from_verified(0, &[]);
        assert!(map.is_complete());
        assert_eq!(map.done_count(), 0);
    }

    /// Multiple state transitions cycle correctly.
    ///
    /// A piece can go Needed → InFlight → Failed → Needed → InFlight → Done.
    /// This full lifecycle must work without corruption.
    #[test]
    fn piece_map_full_lifecycle() {
        let map = SharedPieceMap::new(1);
        assert_eq!(map.get(0), PieceState::Needed);

        assert!(map.try_claim(0));
        assert_eq!(map.get(0), PieceState::InFlight);

        map.mark_failed(0);
        assert_eq!(map.get(0), PieceState::Failed);

        assert!(map.retry_failed(0));
        assert_eq!(map.get(0), PieceState::Needed);

        assert!(map.try_claim(0));
        assert_eq!(map.get(0), PieceState::InFlight);

        map.mark_done(0);
        assert_eq!(map.get(0), PieceState::Done);
        assert!(map.is_complete());
    }
}
