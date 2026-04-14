// SPDX-License-Identifier: MIT OR Apache-2.0

//! Byte-range corruption blame tracking — identifies which peer served corrupt
//! data within a piece.
//!
//! ## What
//!
//! When a piece fails SHA-1 verification, the current coordinator can only
//! blame all peers that contributed to the piece equally. The
//! [`CorruptionLedger`] records which peer served each byte range within a
//! piece, enabling precise blame attribution after a hash failure.
//!
//! ## Why — aMule CorruptionBlackBox lesson
//!
//! aMule's `CorruptionBlackBox` tracks `(offset, length, source)` tuples for
//! every block received. When an AICH (Advanced Intelligent Corruption
//! Handling) hash failure occurs, it computes each source's "blame ratio"
//! (corrupt bytes attributed to source / total bytes from source). Sources
//! above 50% blame are immediately banned. This is far more precise than
//! the "blacklist after N failures" approach because it can exonerate
//! innocent peers that happened to share a piece with a malicious one.
//!
//! ## How
//!
//! The ledger maintains a `Vec<Attribution>` per tracked piece. Each
//! attribution records `(start_offset, end_offset, peer_index)`. When a
//! piece fails verification:
//!
//! 1. Call [`blame_analysis()`](CorruptionLedger::blame_analysis) with the
//!    piece index.
//! 2. The method returns per-peer blame ratios.
//! 3. The coordinator escalates peers whose blame ratio exceeds
//!    [`BLAME_ESCALATION_THRESHOLD`].
//!
//! Attributions are cleared after analysis (or after successful verification)
//! to bound memory usage.

use std::collections::HashMap;

// ── Constants ───────────────────────────────────────────────────────

/// Blame ratio threshold (0.0–1.0) above which a peer is escalated.
///
/// A peer attributed > 50% of a corrupt piece's bytes is considered the
/// likely source. aMule uses a similar threshold in CorruptionBlackBox.
pub const BLAME_ESCALATION_THRESHOLD: f64 = 0.5;

// ── Attribution ─────────────────────────────────────────────────────

/// A single byte-range attribution: "peer X served bytes [start, end)."
///
/// Recorded by the coordinator each time a peer delivers a block within
/// a piece. Multiple attributions can cover the same piece (different
/// peers serving different byte ranges, or the same peer serving the
/// whole piece).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attribution {
    /// Start offset within the piece (inclusive, 0-based).
    pub start: u32,
    /// End offset within the piece (exclusive).
    pub end: u32,
    /// Index of the peer that served this byte range.
    pub peer_index: usize,
}

impl Attribution {
    /// Returns the number of bytes in this attribution.
    pub fn byte_count(&self) -> u32 {
        self.end.saturating_sub(self.start)
    }
}

// ── BlameEntry ──────────────────────────────────────────────────────

/// Per-peer blame analysis result for a single corrupt piece.
#[derive(Debug, Clone, PartialEq)]
pub struct BlameEntry {
    /// Index of the blamed peer.
    pub peer_index: usize,
    /// Number of bytes attributed to this peer in the piece.
    pub bytes_attributed: u32,
    /// Blame ratio: bytes_attributed / total_piece_bytes.
    pub blame_ratio: f64,
    /// Whether this peer should be escalated (ratio > threshold).
    pub should_escalate: bool,
}

// ── CorruptionLedger ────────────────────────────────────────────────

/// Tracks byte-range → peer attributions for corruption blame analysis.
///
/// ## Memory management
///
/// Attributions are held only for pieces currently in-flight or recently
/// failed. Call [`clear_piece()`](Self::clear_piece) after successful
/// verification or after blame analysis to free memory. For a typical
/// download (8 concurrent pieces × 1–3 attributions per piece), memory
/// usage is negligible.
///
/// ```
/// use p2p_distribute::corruption_ledger::{CorruptionLedger, Attribution};
///
/// let mut ledger = CorruptionLedger::new();
///
/// // Peer 0 served the first half, peer 1 served the second half.
/// ledger.record(0, Attribution { start: 0, end: 128_000, peer_index: 0 });
/// ledger.record(0, Attribution { start: 128_000, end: 256_000, peer_index: 1 });
///
/// // Piece 0 failed SHA-1 — who is to blame?
/// let blame = ledger.blame_analysis(0, 256_000);
/// assert_eq!(blame.len(), 2);
/// // Each peer served exactly half, so blame ratio ≈ 0.5 for each.
///
/// ledger.clear_piece(0);
/// assert!(ledger.blame_analysis(0, 256_000).is_empty());
/// ```
#[derive(Debug, Clone, Default)]
pub struct CorruptionLedger {
    /// Attributions keyed by piece index.
    pieces: HashMap<u32, Vec<Attribution>>,
}

impl CorruptionLedger {
    /// Creates an empty ledger.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a byte-range attribution for a piece.
    ///
    /// Called by the coordinator each time a peer delivers a block within
    /// a piece. For single-peer-per-piece downloads (web seeds, parole mode),
    /// there will be exactly one attribution covering the full piece.
    pub fn record(&mut self, piece_index: u32, attribution: Attribution) {
        self.pieces
            .entry(piece_index)
            .or_default()
            .push(attribution);
    }

    /// Performs blame analysis for a corrupt piece.
    ///
    /// Computes per-peer blame ratios based on byte-range attributions.
    /// `total_piece_bytes` is the actual piece size (may be smaller for the
    /// last piece in a torrent).
    ///
    /// Returns an empty `Vec` if no attributions exist for the piece.
    pub fn blame_analysis(&self, piece_index: u32, total_piece_bytes: u32) -> Vec<BlameEntry> {
        let Some(attributions) = self.pieces.get(&piece_index) else {
            return Vec::new();
        };

        if total_piece_bytes == 0 || attributions.is_empty() {
            return Vec::new();
        }

        // Aggregate bytes per peer.
        let mut per_peer: HashMap<usize, u32> = HashMap::new();
        for attr in attributions {
            *per_peer.entry(attr.peer_index).or_insert(0) = per_peer
                .get(&attr.peer_index)
                .copied()
                .unwrap_or(0)
                .saturating_add(attr.byte_count());
        }

        // Build blame entries.
        let total = total_piece_bytes as f64;
        let mut entries: Vec<BlameEntry> = per_peer
            .into_iter()
            .map(|(peer_index, bytes_attributed)| {
                let blame_ratio = bytes_attributed as f64 / total;
                BlameEntry {
                    peer_index,
                    bytes_attributed,
                    blame_ratio,
                    should_escalate: blame_ratio > BLAME_ESCALATION_THRESHOLD,
                }
            })
            .collect();

        // Sort by blame ratio descending for deterministic output.
        entries.sort_by(|a, b| {
            b.blame_ratio
                .partial_cmp(&a.blame_ratio)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        entries
    }

    /// Clears attributions for a piece (after verification or blame analysis).
    pub fn clear_piece(&mut self, piece_index: u32) {
        self.pieces.remove(&piece_index);
    }

    /// Returns the number of pieces currently tracked.
    pub fn tracked_piece_count(&self) -> usize {
        self.pieces.len()
    }

    /// Returns whether any attributions exist for the given piece.
    pub fn has_attributions(&self, piece_index: u32) -> bool {
        self.pieces.get(&piece_index).is_some_and(|v| !v.is_empty())
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Construction ────────────────────────────────────────────────

    /// Empty ledger has no tracked pieces.
    ///
    /// The ledger starts empty and only grows as attributions are recorded.
    #[test]
    fn new_ledger_is_empty() {
        let ledger = CorruptionLedger::new();
        assert_eq!(ledger.tracked_piece_count(), 0);
        assert!(!ledger.has_attributions(0));
    }

    // ── Recording ───────────────────────────────────────────────────

    /// Recording attributions increases tracked piece count.
    ///
    /// Each unique piece index creates a new tracking entry.
    #[test]
    fn record_creates_tracking_entry() {
        let mut ledger = CorruptionLedger::new();
        ledger.record(
            0,
            Attribution {
                start: 0,
                end: 1000,
                peer_index: 0,
            },
        );
        assert_eq!(ledger.tracked_piece_count(), 1);
        assert!(ledger.has_attributions(0));
    }

    /// Multiple attributions for the same piece accumulate.
    ///
    /// A piece served by multiple peers has multiple attribution entries.
    #[test]
    fn multiple_attributions_same_piece() {
        let mut ledger = CorruptionLedger::new();
        ledger.record(
            5,
            Attribution {
                start: 0,
                end: 500,
                peer_index: 0,
            },
        );
        ledger.record(
            5,
            Attribution {
                start: 500,
                end: 1000,
                peer_index: 1,
            },
        );
        assert_eq!(ledger.tracked_piece_count(), 1);
    }

    // ── Blame analysis ──────────────────────────────────────────────

    /// Single peer serving the entire piece gets 100% blame.
    ///
    /// When only one peer contributed to a corrupt piece, that peer is
    /// unambiguously at fault.
    #[test]
    fn single_peer_full_blame() {
        let mut ledger = CorruptionLedger::new();
        ledger.record(
            0,
            Attribution {
                start: 0,
                end: 1000,
                peer_index: 0,
            },
        );

        let blame = ledger.blame_analysis(0, 1000);
        assert_eq!(blame.len(), 1);
        assert_eq!(blame[0].peer_index, 0);
        assert!((blame[0].blame_ratio - 1.0).abs() < f64::EPSILON);
        assert!(blame[0].should_escalate);
    }

    /// Two peers splitting a piece evenly get 50% blame each.
    ///
    /// Neither peer individually exceeds the escalation threshold (> 0.5),
    /// so neither is escalated. Both are suspects but neither can be blamed
    /// with certainty.
    #[test]
    fn two_peers_equal_split_no_escalation() {
        let mut ledger = CorruptionLedger::new();
        ledger.record(
            0,
            Attribution {
                start: 0,
                end: 500,
                peer_index: 0,
            },
        );
        ledger.record(
            0,
            Attribution {
                start: 500,
                end: 1000,
                peer_index: 1,
            },
        );

        let blame = ledger.blame_analysis(0, 1000);
        assert_eq!(blame.len(), 2);
        // Exactly 50% each — not strictly greater than threshold.
        for entry in &blame {
            assert!(!entry.should_escalate, "50% should not escalate");
        }
    }

    /// Peer serving majority of bytes gets escalated.
    ///
    /// When one peer served 80% of the bytes and the other 20%, the
    /// majority peer exceeds the escalation threshold.
    #[test]
    fn majority_peer_escalated() {
        let mut ledger = CorruptionLedger::new();
        ledger.record(
            0,
            Attribution {
                start: 0,
                end: 800,
                peer_index: 0,
            },
        );
        ledger.record(
            0,
            Attribution {
                start: 800,
                end: 1000,
                peer_index: 1,
            },
        );

        let blame = ledger.blame_analysis(0, 1000);
        let peer_0 = blame.iter().find(|b| b.peer_index == 0).unwrap();
        let peer_1 = blame.iter().find(|b| b.peer_index == 1).unwrap();

        assert!(peer_0.should_escalate, "80% blame should escalate");
        assert!(!peer_1.should_escalate, "20% blame should not escalate");
        assert!((peer_0.blame_ratio - 0.8).abs() < 0.01);
    }

    /// Analysis of non-existent piece returns empty.
    ///
    /// Querying a piece with no attributions is safe and returns no blame.
    #[test]
    fn analysis_nonexistent_piece_empty() {
        let ledger = CorruptionLedger::new();
        let blame = ledger.blame_analysis(99, 1000);
        assert!(blame.is_empty());
    }

    /// Zero total bytes returns empty analysis (avoid division by zero).
    ///
    /// Edge case: if the coordinator passes 0 for total bytes, the analysis
    /// must not panic.
    #[test]
    fn analysis_zero_total_bytes_returns_empty() {
        let mut ledger = CorruptionLedger::new();
        ledger.record(
            0,
            Attribution {
                start: 0,
                end: 100,
                peer_index: 0,
            },
        );
        let blame = ledger.blame_analysis(0, 0);
        assert!(blame.is_empty());
    }

    // ── Clearing ────────────────────────────────────────────────────

    /// Clearing a piece removes its attributions.
    ///
    /// After successful verification or blame analysis, attributions should
    /// be cleared to bound memory usage.
    #[test]
    fn clear_piece_removes_attributions() {
        let mut ledger = CorruptionLedger::new();
        ledger.record(
            0,
            Attribution {
                start: 0,
                end: 1000,
                peer_index: 0,
            },
        );
        ledger.record(
            1,
            Attribution {
                start: 0,
                end: 500,
                peer_index: 1,
            },
        );

        ledger.clear_piece(0);
        assert!(!ledger.has_attributions(0));
        assert!(ledger.has_attributions(1));
        assert_eq!(ledger.tracked_piece_count(), 1);
    }

    /// Clearing a non-existent piece is a no-op.
    ///
    /// Prevents panics when the coordinator clears a piece that was never
    /// tracked (e.g. single-peer download with no ledger entries).
    #[test]
    fn clear_nonexistent_piece_is_noop() {
        let mut ledger = CorruptionLedger::new();
        ledger.clear_piece(99); // no panic
        assert_eq!(ledger.tracked_piece_count(), 0);
    }

    // ── Attribution byte_count ──────────────────────────────────────

    /// Attribution byte count is end - start.
    ///
    /// Half-open range [start, end) follows Rust slice conventions.
    #[test]
    fn attribution_byte_count() {
        let attr = Attribution {
            start: 100,
            end: 600,
            peer_index: 0,
        };
        assert_eq!(attr.byte_count(), 500);
    }

    /// Attribution with start == end has zero bytes.
    ///
    /// Degenerate range is valid and should not cause issues.
    #[test]
    fn attribution_zero_range() {
        let attr = Attribution {
            start: 50,
            end: 50,
            peer_index: 0,
        };
        assert_eq!(attr.byte_count(), 0);
    }

    // ── Determinism ─────────────────────────────────────────────────

    /// Blame analysis is deterministic (same input → same output).
    ///
    /// The coordinator may call blame_analysis multiple times; results
    /// must be identical for the same attributions.
    #[test]
    fn blame_analysis_deterministic() {
        let mut ledger = CorruptionLedger::new();
        ledger.record(
            0,
            Attribution {
                start: 0,
                end: 700,
                peer_index: 2,
            },
        );
        ledger.record(
            0,
            Attribution {
                start: 700,
                end: 1000,
                peer_index: 3,
            },
        );

        let blame1 = ledger.blame_analysis(0, 1000);
        let blame2 = ledger.blame_analysis(0, 1000);
        assert_eq!(blame1, blame2);
    }
}
