// SPDX-License-Identifier: MIT OR Apache-2.0

//! Piece download priority for streaming-aware selection (DASH/HLS pattern).
//!
//! ## What
//!
//! A priority enum and scoring function that biases piece selection toward
//! pieces the streaming reader needs imminently. Without this, the coordinator
//! downloads sequentially from piece 0, which is suboptimal when the reader
//! has seeked to the middle of the file or when container indexes live at
//! the file's tail (AVI `idx1`, MP4 `moov` when not fast-started).
//!
//! ## Why — DASH/HLS adaptive bitrate lessons
//!
//! DASH and HLS players don't download segments in order. They prioritise:
//! 1. **Playhead segment** — the one currently being decoded.
//! 2. **Prebuffer window** — N seconds ahead of the playhead.
//! 3. **Container metadata** — moov/sidx atoms (MP4), SeekHead (MKV).
//! 4. **Everything else** — background fill.
//!
//! This module translates that four-tier model into piece priorities that
//! the coordinator's selection loop can use as a score multiplier.
//!
//! ## How it integrates
//!
//! 1. `StreamingReader::needed_pieces()` returns the playhead region.
//! 2. `BufferPolicy::head_tail_priority_pieces()` returns metadata pieces.
//! 3. `PiecePriorityMap::update()` combines both into a per-piece priority.
//! 4. Coordinator multiplies piece rarity score by priority weight.

use std::time::Instant;

// ── Priority levels ─────────────────────────────────────────────────

/// Download priority for a single piece.
///
/// Higher priority pieces should be requested before lower priority ones,
/// even if a lower-priority piece is rarer. This prevents playback stalls
/// at the cost of slightly suboptimal swarm health — an acceptable
/// trade-off for a media-focused content manager.
///
/// ## Weight values
///
/// The associated weight is a multiplier (0–1000) for the coordinator's
/// piece selection score. `Critical` pieces score 10× higher than `Low`.
///
/// ```
/// use p2p_distribute::PiecePriority;
///
/// assert!(PiecePriority::Critical.weight() > PiecePriority::Normal.weight());
/// assert!(PiecePriority::Normal.weight() > PiecePriority::Low.weight());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum PiecePriority {
    /// Background fill — piece is not needed soon. Downloaded last to fill
    /// gaps and improve seeding completeness.
    Low = 0,
    /// Standard priority — piece is within the general download window but
    /// not near the playhead or metadata region.
    #[default]
    Normal = 1,
    /// Prebuffer — piece is within the streaming reader's target prebuffer
    /// window. Downloading it prevents a future stall.
    High = 2,
    /// Playhead or container metadata — piece is being actively decoded or
    /// contains container index data (moov, idx1, SeekHead). A stall is
    /// imminent or structure is needed for seeking.
    Critical = 3,
}

impl PiecePriority {
    /// Score multiplier for the coordinator's piece selection.
    ///
    /// Critical = 1000, High = 500, Normal = 200, Low = 100.
    /// The 10:1 ratio between Critical and Low ensures playhead pieces
    /// always win, even against very rare pieces.
    pub fn weight(self) -> u32 {
        match self {
            Self::Low => 100,
            Self::Normal => 200,
            Self::High => 500,
            Self::Critical => 1000,
        }
    }
}

impl std::fmt::Display for PiecePriority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Low => f.write_str("low"),
            Self::Normal => f.write_str("normal"),
            Self::High => f.write_str("high"),
            Self::Critical => f.write_str("critical"),
        }
    }
}

// ── Priority map ────────────────────────────────────────────────────

/// Per-piece priority assignment for the entire download.
///
/// Maintains a `Vec<PiecePriority>` indexed by piece number. The
/// coordinator reads this when selecting the next piece to request.
///
/// ## Update cadence
///
/// Call `update()` periodically (e.g. every 500ms or when the streaming
/// reader seeks). The update is cheap: O(piece_count) with no allocation
/// after initial construction.
pub struct PiecePriorityMap {
    /// One priority per piece.
    priorities: Vec<PiecePriority>,
    /// When the map was last recomputed.
    last_update: Instant,
}

impl PiecePriorityMap {
    /// Creates a new priority map with all pieces at `Normal` priority.
    pub fn new(piece_count: u32, now: Instant) -> Self {
        Self {
            priorities: vec![PiecePriority::Normal; piece_count as usize],
            last_update: now,
        }
    }

    /// Recomputes priorities from streaming reader state.
    ///
    /// ## Parameters
    ///
    /// - `critical_pieces`: pieces at or near the playhead (stall imminent).
    /// - `high_pieces`: pieces in the prebuffer window.
    /// - `metadata_pieces`: container index pieces (head/tail).
    /// - `now`: current time for staleness tracking.
    ///
    /// All other pieces default to `Normal`. Pieces present in multiple
    /// sets take the highest priority.
    pub fn update(
        &mut self,
        critical_pieces: &[u32],
        high_pieces: &[u32],
        metadata_pieces: &[u32],
        now: Instant,
    ) {
        // Reset everything to Normal.
        for p in &mut self.priorities {
            *p = PiecePriority::Normal;
        }
        // Apply in ascending priority order so higher overwrites lower.
        for &idx in high_pieces {
            if let Some(p) = self.priorities.get_mut(idx as usize) {
                *p = PiecePriority::High;
            }
        }
        // Metadata pieces are Critical (container indexes needed for seeking).
        for &idx in metadata_pieces {
            if let Some(p) = self.priorities.get_mut(idx as usize) {
                *p = PiecePriority::Critical;
            }
        }
        // Playhead pieces override everything.
        for &idx in critical_pieces {
            if let Some(p) = self.priorities.get_mut(idx as usize) {
                *p = PiecePriority::Critical;
            }
        }
        self.last_update = now;
    }

    /// Marks all completed pieces as Low priority.
    ///
    /// Completed pieces don't need downloading, but keeping them at Low
    /// (rather than removing them) makes the coordinator's math simpler.
    pub fn mark_completed(&mut self, completed_pieces: &[u32]) {
        for &idx in completed_pieces {
            if let Some(p) = self.priorities.get_mut(idx as usize) {
                *p = PiecePriority::Low;
            }
        }
    }

    /// Priority for a specific piece.
    ///
    /// Returns `Normal` for out-of-range indices.
    pub fn get(&self, piece_index: u32) -> PiecePriority {
        self.priorities
            .get(piece_index as usize)
            .copied()
            .unwrap_or(PiecePriority::Normal)
    }

    /// Weighted score for a piece: priority dominates, rarity breaks ties.
    ///
    /// The score is computed as:
    /// `priority_weight × (max_rarity + 1) + rarity_inverse`
    ///
    /// This two-tier formula guarantees that a Critical piece always scores
    /// higher than a Normal piece regardless of rarity, because the priority
    /// term `weight × (max_rarity + 1)` is always larger than the maximum
    /// possible rarity inverse `(max_rarity + 1)`. Within the same priority
    /// tier, rarer pieces score higher.
    ///
    /// ## Why additive rather than multiplicative
    ///
    /// Multiplicative scoring (`weight × rarity_inverse`) can let a very
    /// rare Normal piece tie or beat a common Critical piece when the
    /// rarity spread is large. Additive two-tier scoring prevents this:
    /// priority is the primary sort key, rarity is the tiebreaker.
    pub fn weighted_score(&self, piece_index: u32, rarity: u32, max_rarity: u32) -> u64 {
        let priority_weight = self.get(piece_index).weight() as u64;
        let scale = (max_rarity.saturating_add(1)) as u64;
        let rarity_inverse = (max_rarity.saturating_add(1).saturating_sub(rarity)) as u64;
        priority_weight
            .saturating_mul(scale)
            .saturating_add(rarity_inverse)
    }

    /// When this map was last recomputed.
    pub fn last_update(&self) -> Instant {
        self.last_update
    }

    /// Number of pieces tracked.
    pub fn piece_count(&self) -> u32 {
        self.priorities.len() as u32
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── PiecePriority ───────────────────────────────────────────────

    /// Priority weights follow the expected ordering.
    #[test]
    fn priority_weight_ordering() {
        assert!(PiecePriority::Critical.weight() > PiecePriority::High.weight());
        assert!(PiecePriority::High.weight() > PiecePriority::Normal.weight());
        assert!(PiecePriority::Normal.weight() > PiecePriority::Low.weight());
    }

    /// Default priority is Normal.
    #[test]
    fn priority_default_is_normal() {
        assert_eq!(PiecePriority::default(), PiecePriority::Normal);
    }

    /// Display shows lowercase names.
    #[test]
    fn priority_display() {
        assert_eq!(PiecePriority::Critical.to_string(), "critical");
        assert_eq!(PiecePriority::Low.to_string(), "low");
    }

    /// Priorities are ordered: Low < Normal < High < Critical.
    #[test]
    fn priority_ord() {
        assert!(PiecePriority::Low < PiecePriority::Normal);
        assert!(PiecePriority::Normal < PiecePriority::High);
        assert!(PiecePriority::High < PiecePriority::Critical);
    }

    // ── PiecePriorityMap ────────────────────────────────────────────

    /// New map starts with all Normal.
    #[test]
    fn new_map_all_normal() {
        let now = Instant::now();
        let map = PiecePriorityMap::new(10, now);
        for i in 0..10 {
            assert_eq!(map.get(i), PiecePriority::Normal);
        }
    }

    /// Update applies priorities in correct precedence order.
    ///
    /// If a piece appears in both high and critical lists, it should
    /// end up as Critical (highest wins).
    #[test]
    fn update_applies_highest_priority() {
        let now = Instant::now();
        let mut map = PiecePriorityMap::new(10, now);
        // Piece 3 in both high and critical → Critical wins.
        map.update(&[3], &[3, 4], &[], now);
        assert_eq!(map.get(3), PiecePriority::Critical);
        assert_eq!(map.get(4), PiecePriority::High);
        assert_eq!(map.get(5), PiecePriority::Normal);
    }

    /// Metadata pieces get Critical priority.
    #[test]
    fn metadata_pieces_are_critical() {
        let now = Instant::now();
        let mut map = PiecePriorityMap::new(100, now);
        map.update(&[], &[], &[0, 99], now);
        assert_eq!(map.get(0), PiecePriority::Critical);
        assert_eq!(map.get(99), PiecePriority::Critical);
        assert_eq!(map.get(50), PiecePriority::Normal);
    }

    /// mark_completed sets pieces to Low.
    #[test]
    fn mark_completed_sets_low() {
        let now = Instant::now();
        let mut map = PiecePriorityMap::new(10, now);
        map.update(&[0], &[], &[], now);
        assert_eq!(map.get(0), PiecePriority::Critical);
        map.mark_completed(&[0]);
        assert_eq!(map.get(0), PiecePriority::Low);
    }

    /// Out-of-range get returns Normal.
    #[test]
    fn out_of_range_returns_normal() {
        let now = Instant::now();
        let map = PiecePriorityMap::new(5, now);
        assert_eq!(map.get(100), PiecePriority::Normal);
    }

    // ── Weighted scoring ────────────────────────────────────────────

    /// Critical piece with high rarity beats Normal piece with low rarity.
    ///
    /// This is the key property: streaming priority overrides rarity.
    /// A common playhead piece must be fetched before a rare background
    /// piece to prevent playback stalls.
    #[test]
    fn critical_beats_normal_regardless_of_rarity() {
        let now = Instant::now();
        let mut map = PiecePriorityMap::new(10, now);
        map.update(&[0], &[], &[], now);
        // Piece 0: Critical, rarity=5 (common). Piece 1: Normal, rarity=1 (rare).
        let score_critical = map.weighted_score(0, 5, 5);
        let score_normal = map.weighted_score(1, 1, 5);
        assert!(
            score_critical > score_normal,
            "Critical/common ({score_critical}) should beat Normal/rare ({score_normal})"
        );
    }

    /// Among same-priority pieces, rarer ones score higher.
    #[test]
    fn rarer_piece_scores_higher_at_same_priority() {
        let now = Instant::now();
        let map = PiecePriorityMap::new(10, now);
        // Both Normal. Piece 0: rarity=1 (rare), piece 1: rarity=5 (common).
        let score_rare = map.weighted_score(0, 1, 5);
        let score_common = map.weighted_score(1, 5, 5);
        assert!(score_rare > score_common);
    }
}
