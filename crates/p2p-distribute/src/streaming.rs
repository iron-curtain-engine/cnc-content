// SPDX-License-Identifier: MIT OR Apache-2.0

//! Byte-range tracking, buffer policy, piece mapping, and peer priority for
//! streaming playback.
//!
//! This module provides the data structures needed to play media (video, audio)
//! before the full download completes. The [`StreamingReader`](crate::reader::StreamingReader)
//! consumes these types to decide when to block, when to start playback, and
//! which pieces to prioritize.
//!
//! ## Architecture
//!
//! - **[`ByteRangeMap`]** — tracks which byte ranges of a file are available on disk.
//!   Maps directly to torrent piece boundaries: when a piece completes, the
//!   corresponding byte range is marked available.
//!
//! - **[`BufferPolicy`]** — configurable thresholds for when to start/pause playback.
//!   An actively-streaming file gets aggressive piece prioritization.
//!
//! - **[`PieceMapping`]** — maps file byte offsets to torrent piece indices. When the
//!   reader needs bytes at offset N, it can request priority for the pieces that
//!   contain those bytes. This is content-aware piece ordering.
//!
//! - **[`PeerPriority`]** — prioritises peers based on which pieces they have
//!   relative to the current playhead position.

use std::time::Duration;

// ── ByteRange ───────────────────────────────────────────────────────

/// A half-open byte range `[start, end)` within a file.
///
/// Used to track which portions of a file are available on disk.
/// Ranges are non-overlapping and sorted by `start` within a `ByteRangeMap`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteRange {
    /// First byte (inclusive).
    pub start: u64,
    /// One past the last byte (exclusive).
    pub end: u64,
}

impl ByteRange {
    /// Number of bytes in this range.
    pub fn len(&self) -> u64 {
        self.end.saturating_sub(self.start)
    }

    /// Whether the range is empty (zero bytes).
    pub fn is_empty(&self) -> bool {
        self.end <= self.start
    }

    /// Returns `true` if `self` fully contains `other`.
    pub fn contains_range(&self, other: &ByteRange) -> bool {
        self.start <= other.start && self.end >= other.end
    }

    /// Returns `true` if `self` and `other` overlap or are adjacent (can merge).
    pub fn mergeable_with(&self, other: &ByteRange) -> bool {
        self.start <= other.end && other.start <= self.end
    }

    /// Returns the union of `self` and `other` (assumes they are mergeable).
    pub fn merge(self, other: ByteRange) -> ByteRange {
        ByteRange {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }
}

// ── ByteRangeMap ────────────────────────────────────────────────────

/// Tracks which byte ranges of a file are available on disk.
///
/// Internally a sorted, non-overlapping list of `ByteRange` values. When new
/// ranges are inserted, adjacent and overlapping ranges are automatically
/// coalesced into a single entry.
#[derive(Debug, Clone)]
pub struct ByteRangeMap {
    /// Sorted, non-overlapping available ranges.
    ranges: Vec<ByteRange>,
    /// Total file size in bytes.
    file_size: u64,
}

impl ByteRangeMap {
    /// Creates an empty range map for a file of the given size.
    pub fn new(file_size: u64) -> Self {
        Self {
            ranges: Vec::new(),
            file_size,
        }
    }

    /// Creates a range map where the entire file is already available.
    pub fn fully_available(file_size: u64) -> Self {
        let mut map = Self::new(file_size);
        if file_size > 0 {
            map.ranges.push(ByteRange {
                start: 0,
                end: file_size,
            });
        }
        map
    }

    /// Returns `true` if the entire file is available.
    pub fn is_complete(&self) -> bool {
        self.ranges.len() == 1 && self.ranges[0].start == 0 && self.ranges[0].end >= self.file_size
    }

    /// Returns how many bytes are available in total.
    pub fn available_bytes(&self) -> u64 {
        self.ranges.iter().map(|r| r.len()).sum()
    }

    /// Returns the total file size.
    pub fn file_size(&self) -> u64 {
        self.file_size
    }

    /// Returns `true` if the given byte range is fully available.
    pub fn has_range(&self, start: u64, len: u64) -> bool {
        if len == 0 {
            return true;
        }
        // Checked add to prevent overflow with untrusted inputs.
        let end = match start.checked_add(len) {
            Some(e) => e,
            None => return false,
        };
        let needed = ByteRange { start, end };
        self.ranges.iter().any(|r| r.contains_range(&needed))
    }

    /// Returns how many contiguous bytes are available starting from `offset`.
    ///
    /// This is the key query for the streaming reader: "how far ahead can I
    /// read without blocking?"
    pub fn contiguous_from(&self, offset: u64) -> u64 {
        for r in &self.ranges {
            if r.start <= offset && r.end > offset {
                return r.end - offset;
            }
        }
        0
    }

    /// Inserts a new available byte range, coalescing with adjacent ranges.
    ///
    /// Called when a torrent piece completes. The piece's byte range within
    /// this file is translated and inserted here.
    pub fn insert(&mut self, range: ByteRange) {
        if range.is_empty() {
            return;
        }

        // Find all existing ranges that overlap or are adjacent.
        let mut merged = range;
        self.ranges.retain(|existing| {
            if existing.mergeable_with(&merged) {
                merged = merged.merge(*existing);
                false // remove, will be replaced by merged
            } else {
                true
            }
        });

        // Insert the merged range in sorted position.
        let pos = self
            .ranges
            .binary_search_by_key(&merged.start, |r| r.start)
            .unwrap_or_else(|i| i);
        self.ranges.insert(pos, merged);
    }

    /// Returns a snapshot of all available ranges (for progress display).
    pub fn ranges(&self) -> &[ByteRange] {
        &self.ranges
    }

    /// Returns the first byte offset that is NOT available from the given
    /// position. Returns `None` if everything from `offset` to EOF is available.
    ///
    /// Used by the streaming reader to identify which piece to request next.
    pub fn first_gap_from(&self, offset: u64) -> Option<u64> {
        for r in &self.ranges {
            if r.start <= offset && r.end > offset {
                // We're inside a range — the gap starts at the end.
                if r.end >= self.file_size {
                    return None; // available to EOF
                }
                return Some(r.end);
            }
        }
        // Not inside any range — gap starts at offset itself.
        if offset >= self.file_size {
            None
        } else {
            Some(offset)
        }
    }
}

// ── Buffer policy ───────────────────────────────────────────────────

/// Controls when streaming playback may begin and when it should pause.
///
/// The thresholds are configurable for different content types. For example,
/// low-bitrate video (~300 KB/s) needs less pre-buffering than high-bitrate
/// content.
///
/// The `head_pieces` and `tail_pieces` fields enable early download of file
/// boundaries. Container formats (MP4 moov atom, AVI idx1, MKV SeekHead)
/// place index data at the start or end. Fetching these pieces first lets
/// playback start sooner, matching aria2's `--bt-prioritize-piece=head,tail`.
#[derive(Debug, Clone)]
pub struct BufferPolicy {
    /// Minimum bytes buffered ahead of playhead before playback starts.
    /// If the buffer drops below this, playback pauses until it refills.
    pub min_prebuffer: u64,
    /// Target bytes buffered ahead — once reached, playback can start.
    /// Pieces beyond this distance from the playhead get normal (not high)
    /// priority.
    pub target_prebuffer: u64,
    /// Maximum time to wait for data before returning a timeout error.
    /// Prevents indefinite hangs if the swarm is dead.
    pub read_timeout: Duration,
    /// Number of pieces at the start of the file to prioritise.
    /// Set to 0 to disable head prioritisation.
    pub head_pieces: u32,
    /// Number of pieces at the end of the file to prioritise.
    /// Set to 0 to disable tail prioritisation.
    pub tail_pieces: u32,
}

impl Default for BufferPolicy {
    fn default() -> Self {
        Self {
            // 512 KB — enough for ~1.7s of 300 KB/s video
            min_prebuffer: 512 * 1024,
            // 2 MB — enough buffer to absorb piece-arrival jitter
            target_prebuffer: 2 * 1024 * 1024,
            // 30s timeout — if no data arrives in 30s, the swarm is likely dead
            read_timeout: Duration::from_secs(30),
            // Head/tail disabled by default — enable for media content.
            head_pieces: 0,
            tail_pieces: 0,
        }
    }
}

impl BufferPolicy {
    /// Policy for LAN or localhost streaming (no network jitter).
    pub fn local() -> Self {
        Self {
            min_prebuffer: 0,
            target_prebuffer: 0,
            read_timeout: Duration::from_secs(5),
            head_pieces: 0,
            tail_pieces: 0,
        }
    }

    /// Policy for high-bitrate content (e.g. upscaled video).
    ///
    /// Enables head/tail prioritisation — 2 pieces at each end — to
    /// quickly fetch container index data (moov, idx1, SeekHead).
    pub fn high_bitrate() -> Self {
        Self {
            min_prebuffer: 2 * 1024 * 1024,
            target_prebuffer: 8 * 1024 * 1024,
            read_timeout: Duration::from_secs(60),
            head_pieces: 2,
            tail_pieces: 2,
        }
    }

    /// Returns the piece indices that should be downloaded with high
    /// priority for head/tail optimisation.
    ///
    /// Given a [`PieceMapping`] that describes which pieces a file spans,
    /// this returns the first `head_pieces` and last `tail_pieces` of that
    /// file. Duplicates are removed (when the file is small enough that
    /// head and tail overlap).
    pub fn head_tail_priority_pieces(&self, mapping: &PieceMapping) -> Vec<u32> {
        let all = mapping.all_file_pieces();
        let first = *all.start();
        let last = *all.end();
        let total = last - first + 1;

        let mut pieces = Vec::new();

        // Head pieces.
        let head_count = (self.head_pieces).min(total);
        for i in 0..head_count {
            pieces.push(first + i);
        }

        // Tail pieces (avoid duplicating head pieces).
        let tail_count = (self.tail_pieces).min(total);
        for i in 0..tail_count {
            let p = last - i;
            if !pieces.contains(&p) {
                pieces.push(p);
            }
        }

        pieces
    }
}

// ── Piece mapping ───────────────────────────────────────────────────

/// Maps file byte offsets to torrent piece indices.
///
/// In a multi-file torrent, a single piece can span multiple files. This
/// mapping tells the streaming reader which piece(s) it needs for a given
/// file byte range, enabling precise priority requests.
#[derive(Debug, Clone)]
pub struct PieceMapping {
    /// Byte offset of this file within the torrent's concatenated data.
    pub file_offset_in_torrent: u64,
    /// Size of each piece in bytes (uniform across the torrent).
    pub piece_size: u64,
    /// Total number of pieces in the torrent.
    pub total_pieces: u32,
    /// Total file size.
    pub file_size: u64,
}

impl PieceMapping {
    /// Returns the piece indices that contain the given file byte range.
    ///
    /// This is the core query: "which pieces do I need to read bytes
    /// [offset, offset+len) of this file?"
    pub fn pieces_for_range(&self, offset: u64, len: u64) -> std::ops::RangeInclusive<u32> {
        let torrent_start = self.file_offset_in_torrent + offset;
        let torrent_end = torrent_start + len.saturating_sub(1);
        let first_piece = (torrent_start / self.piece_size) as u32;
        let last_piece = ((torrent_end / self.piece_size) as u32).min(self.total_pieces - 1);
        first_piece..=last_piece
    }

    /// Returns the piece index containing the given file byte offset.
    pub fn piece_at_offset(&self, offset: u64) -> u32 {
        ((self.file_offset_in_torrent + offset) / self.piece_size) as u32
    }

    /// Returns all pieces that this file spans.
    pub fn all_file_pieces(&self) -> std::ops::RangeInclusive<u32> {
        self.pieces_for_range(0, self.file_size)
    }
}

// ── Stream progress reporting ─────────────────────────────────────────

/// Progress information for a streaming reader.
///
/// Consumers can poll this to display a buffering indicator, download speed,
/// and piece-level progress visualization.
#[derive(Debug, Clone)]
pub struct StreamProgress {
    /// Current read position (playhead) in the file.
    pub position: u64,
    /// Total file size.
    pub file_size: u64,
    /// Bytes available ahead of the playhead (contiguous buffer).
    pub buffered_ahead: u64,
    /// Total bytes available across all ranges.
    pub total_available: u64,
    /// Whether playback is currently buffering (waiting for data).
    pub is_buffering: bool,
    /// Number of available ranges (1 = contiguous, >1 = fragmented).
    pub fragment_count: usize,
}

impl StreamProgress {
    /// Returns the buffering percentage (0.0–1.0) relative to the policy target.
    pub fn buffer_fill_ratio(&self, policy: &BufferPolicy) -> f64 {
        if policy.target_prebuffer == 0 {
            return 1.0;
        }
        (self.buffered_ahead as f64 / policy.target_prebuffer as f64).min(1.0)
    }

    /// Returns the overall download percentage (0.0–1.0).
    pub fn download_ratio(&self) -> f64 {
        if self.file_size == 0 {
            return 1.0;
        }
        self.total_available as f64 / self.file_size as f64
    }
}

// ── Peer priority ───────────────────────────────────────────────────

/// Priority level assigned to a peer based on which pieces it has relative
/// to the current playhead.
///
/// Peers holding pieces near the playhead are HIGH priority for unchoke
/// and request scheduling. Peers with only distant pieces are LOW — still
/// useful for background download but not urgent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PeerPriority {
    /// Peer has no useful pieces for this file.
    None,
    /// Peer has pieces, but far from the playhead (background download).
    Low,
    /// Peer has pieces within the target prebuffer window.
    Medium,
    /// Peer has the next piece(s) the playhead needs (critical path).
    High,
}

/// Evaluates a peer's priority based on which pieces it has.
///
/// `peer_pieces` is the set of piece indices this peer advertises.
/// `playhead_piece` is the piece the reader is currently waiting for.
/// `prebuffer_end_piece` is the last piece in the pre-buffer window.
pub fn evaluate_peer_priority(
    peer_pieces: &[u32],
    playhead_piece: u32,
    prebuffer_end_piece: u32,
) -> PeerPriority {
    let mut has_playhead = false;
    let mut has_prebuffer = false;
    let mut has_any_file = false;

    for &piece in peer_pieces {
        if piece == playhead_piece {
            has_playhead = true;
        }
        if piece > playhead_piece && piece <= prebuffer_end_piece {
            has_prebuffer = true;
        }
        has_any_file = true;
    }

    if has_playhead {
        PeerPriority::High
    } else if has_prebuffer {
        PeerPriority::Medium
    } else if has_any_file {
        PeerPriority::Low
    } else {
        PeerPriority::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── ByteRange ────────────────────────────────────────────────────

    /// `ByteRange::len` returns the number of bytes in the range and
    /// `is_empty` correctly reflects a zero-length range.
    #[test]
    fn byte_range_len_and_empty() {
        let r = ByteRange { start: 10, end: 20 };
        assert_eq!(r.len(), 10);
        assert!(!r.is_empty());

        let empty = ByteRange { start: 10, end: 10 };
        assert_eq!(empty.len(), 0);
        assert!(empty.is_empty());
    }

    /// `contains_range` returns `true` only when one range fully encloses
    /// another, and `false` for partial overlaps.
    #[test]
    fn byte_range_contains_range() {
        let outer = ByteRange { start: 0, end: 100 };
        let inner = ByteRange { start: 10, end: 50 };
        let overlap = ByteRange {
            start: 50,
            end: 150,
        };

        assert!(outer.contains_range(&inner));
        assert!(!outer.contains_range(&overlap));
        assert!(!inner.contains_range(&outer));
    }

    // ── ByteRangeMap ─────────────────────────────────────────────────

    /// A freshly created `ByteRangeMap` reports zero availability on all queries.
    #[test]
    fn range_map_empty() {
        let map = ByteRangeMap::new(1000);
        assert!(!map.is_complete());
        assert_eq!(map.available_bytes(), 0);
        assert!(!map.has_range(0, 1));
        assert_eq!(map.contiguous_from(0), 0);
    }

    /// `ByteRangeMap::fully_available` marks the entire file as available.
    #[test]
    fn range_map_fully_available() {
        let map = ByteRangeMap::fully_available(1000);
        assert!(map.is_complete());
        assert_eq!(map.available_bytes(), 1000);
        assert!(map.has_range(0, 1000));
        assert_eq!(map.contiguous_from(0), 1000);
        assert_eq!(map.contiguous_from(500), 500);
    }

    /// Inserting a bridging range coalesces all three into one.
    #[test]
    fn range_map_insert_and_coalesce() {
        let mut map = ByteRangeMap::new(1000);

        map.insert(ByteRange { start: 0, end: 100 });
        map.insert(ByteRange {
            start: 200,
            end: 300,
        });
        assert_eq!(map.ranges().len(), 2);
        assert_eq!(map.available_bytes(), 200);
        assert_eq!(map.contiguous_from(0), 100);

        // Insert a bridging range — should coalesce all three into one.
        map.insert(ByteRange {
            start: 100,
            end: 200,
        });
        assert_eq!(map.ranges().len(), 1);
        assert_eq!(map.available_bytes(), 300);
        assert_eq!(map.contiguous_from(0), 300);
    }

    /// Overlapping inserts merge correctly.
    #[test]
    fn range_map_overlapping_insert() {
        let mut map = ByteRangeMap::new(1000);
        map.insert(ByteRange { start: 0, end: 100 });
        map.insert(ByteRange {
            start: 50,
            end: 150,
        });
        assert_eq!(map.ranges().len(), 1);
        assert_eq!(map.ranges()[0].start, 0);
        assert_eq!(map.ranges()[0].end, 150);
    }

    /// `first_gap_from` returns correct offsets.
    #[test]
    fn range_map_first_gap() {
        let mut map = ByteRangeMap::new(1000);
        map.insert(ByteRange { start: 0, end: 100 });
        map.insert(ByteRange {
            start: 200,
            end: 300,
        });

        assert_eq!(map.first_gap_from(0), Some(100));
        assert_eq!(map.first_gap_from(50), Some(100));
        assert_eq!(map.first_gap_from(100), Some(100));
        assert_eq!(map.first_gap_from(200), Some(300));
    }

    /// `first_gap_from` returns `None` for a fully-available file.
    #[test]
    fn range_map_no_gap_when_complete() {
        let map = ByteRangeMap::fully_available(1000);
        assert_eq!(map.first_gap_from(0), None);
        assert_eq!(map.first_gap_from(999), None);
    }

    // ── BufferPolicy ─────────────────────────────────────────────────

    /// `BufferPolicy::default` uses the documented values.
    #[test]
    fn buffer_policy_defaults() {
        let policy = BufferPolicy::default();
        assert_eq!(policy.min_prebuffer, 512 * 1024);
        assert_eq!(policy.target_prebuffer, 2 * 1024 * 1024);
        assert_eq!(policy.read_timeout, Duration::from_secs(30));
        assert_eq!(policy.head_pieces, 0);
        assert_eq!(policy.tail_pieces, 0);
    }

    /// `BufferPolicy::high_bitrate` enables head/tail prioritisation.
    #[test]
    fn buffer_policy_high_bitrate_enables_head_tail() {
        let policy = BufferPolicy::high_bitrate();
        assert_eq!(policy.head_pieces, 2);
        assert_eq!(policy.tail_pieces, 2);
    }

    // ── Head/tail piece priority ─────────────────────────────────────

    /// Head/tail priority returns empty vec when both counts are zero.
    ///
    /// The default policy has head_pieces=0 and tail_pieces=0, so no
    /// pieces get special treatment.
    #[test]
    fn head_tail_priority_empty_when_disabled() {
        let policy = BufferPolicy::default();
        let mapping = PieceMapping {
            file_offset_in_torrent: 0,
            piece_size: 262144,
            total_pieces: 100,
            file_size: 10_000_000,
        };
        assert!(policy.head_tail_priority_pieces(&mapping).is_empty());
    }

    /// Head/tail priority returns correct pieces for a multi-piece file.
    ///
    /// With 2 head + 2 tail on a file spanning pieces 4..=9, we expect
    /// pieces [4, 5, 9, 8] (head first, then tail from end).
    #[test]
    fn head_tail_priority_multi_piece() {
        let policy = BufferPolicy {
            head_pieces: 2,
            tail_pieces: 2,
            ..BufferPolicy::default()
        };
        // File starts at 1MB offset, spans pieces 4 through 9.
        let mapping = PieceMapping {
            file_offset_in_torrent: 1_048_576,
            piece_size: 262144,
            total_pieces: 100,
            file_size: 1_500_000, // ~5.7 pieces
        };
        let pieces = policy.head_tail_priority_pieces(&mapping);
        assert_eq!(pieces, vec![4, 5, 9, 8]);
    }

    /// Head/tail deduplicates when file is small (head and tail overlap).
    ///
    /// A file spanning only 2 pieces with head=2, tail=2 should not produce
    /// duplicates — only [0, 1].
    #[test]
    fn head_tail_priority_deduplicates_small_file() {
        let policy = BufferPolicy {
            head_pieces: 2,
            tail_pieces: 2,
            ..BufferPolicy::default()
        };
        let mapping = PieceMapping {
            file_offset_in_torrent: 0,
            piece_size: 262144,
            total_pieces: 100,
            file_size: 500_000, // 2 pieces
        };
        let pieces = policy.head_tail_priority_pieces(&mapping);
        assert_eq!(pieces, vec![0, 1]);
    }

    /// Head-only prioritisation returns only the first N pieces.
    #[test]
    fn head_only_priority() {
        let policy = BufferPolicy {
            head_pieces: 3,
            tail_pieces: 0,
            ..BufferPolicy::default()
        };
        let mapping = PieceMapping {
            file_offset_in_torrent: 0,
            piece_size: 262144,
            total_pieces: 100,
            file_size: 5_000_000,
        };
        let pieces = policy.head_tail_priority_pieces(&mapping);
        assert_eq!(pieces, vec![0, 1, 2]);
    }

    // ── PieceMapping ─────────────────────────────────────────────────

    /// Single-piece file maps all byte ranges to piece 0.
    #[test]
    fn piece_mapping_single_piece() {
        let mapping = PieceMapping {
            file_offset_in_torrent: 0,
            piece_size: 262144,
            total_pieces: 100,
            file_size: 100000,
        };
        assert_eq!(mapping.pieces_for_range(0, 100000), 0..=0);
        assert_eq!(mapping.piece_at_offset(0), 0);
    }

    /// Multi-piece file spans correct piece range.
    #[test]
    fn piece_mapping_multi_piece() {
        let mapping = PieceMapping {
            file_offset_in_torrent: 0,
            piece_size: 262144,
            total_pieces: 100,
            file_size: 1_000_000,
        };
        assert_eq!(mapping.all_file_pieces(), 0..=3);
        assert_eq!(mapping.piece_at_offset(500_000), 1);
    }

    /// File at non-zero torrent offset computes correct piece indices.
    #[test]
    fn piece_mapping_with_offset() {
        let mapping = PieceMapping {
            file_offset_in_torrent: 1_048_576,
            piece_size: 262144,
            total_pieces: 100,
            file_size: 500_000,
        };
        assert_eq!(mapping.piece_at_offset(0), 4);
        assert_eq!(mapping.all_file_pieces(), 4..=5);
    }

    // ── PeerPriority ─────────────────────────────────────────────────

    /// Peer with the playhead piece gets `High` priority.
    #[test]
    fn peer_priority_high_for_playhead() {
        let priority = evaluate_peer_priority(&[5, 6, 7], 5, 10);
        assert_eq!(priority, PeerPriority::High);
    }

    /// Peer with prebuffer pieces gets `Medium` priority.
    #[test]
    fn peer_priority_medium_for_prebuffer() {
        let priority = evaluate_peer_priority(&[7, 8, 9], 5, 10);
        assert_eq!(priority, PeerPriority::Medium);
    }

    /// Peer with only distant pieces gets `Low` priority.
    #[test]
    fn peer_priority_low_for_distant() {
        let priority = evaluate_peer_priority(&[50, 51], 5, 10);
        assert_eq!(priority, PeerPriority::Low);
    }

    /// Peer with no pieces gets `None` priority.
    #[test]
    fn peer_priority_none_for_empty() {
        let priority = evaluate_peer_priority(&[], 5, 10);
        assert_eq!(priority, PeerPriority::None);
    }

    // ── StreamProgress ───────────────────────────────────────────────

    /// `buffer_fill_ratio` and `download_ratio` return correct fractions.
    #[test]
    fn stream_progress_ratios() {
        let progress = StreamProgress {
            position: 100,
            file_size: 1000,
            buffered_ahead: 256 * 1024,
            total_available: 500,
            is_buffering: false,
            fragment_count: 1,
        };

        let policy = BufferPolicy::default();
        let ratio = progress.buffer_fill_ratio(&policy);
        assert!((ratio - 0.125).abs() < 0.001);

        assert!((progress.download_ratio() - 0.5).abs() < 0.001);
    }

    // ── u64 boundary and overflow edge cases ────────────────────────

    /// `ByteRange::len` saturates on inverted range (end < start).
    ///
    /// A malformed range from untrusted input should not wrap around.
    #[test]
    fn byte_range_len_inverted_range_saturates() {
        let range = ByteRange {
            start: 100,
            end: 50,
        };
        assert_eq!(range.len(), 0);
        assert!(range.is_empty());
    }

    /// `ByteRange` at `u64::MAX` boundary does not overflow.
    ///
    /// Ensures that arithmetic near the top of the u64 range is safe.
    #[test]
    fn byte_range_max_boundary() {
        let range = ByteRange {
            start: u64::MAX - 100,
            end: u64::MAX,
        };
        assert_eq!(range.len(), 100);
        assert!(!range.is_empty());
    }

    /// `ByteRangeMap` with `u64::MAX` file size does not panic.
    ///
    /// Large file sizes must not cause integer overflow in range tracking.
    #[test]
    fn byte_range_map_max_file_size() {
        let map = ByteRangeMap::new(u64::MAX);
        assert_eq!(map.file_size(), u64::MAX);
        assert_eq!(map.available_bytes(), 0);
        assert!(!map.is_complete());
    }

    /// Inserting a range that wraps around `u64::MAX` is handled.
    ///
    /// A near-max range should merge correctly without arithmetic overflow.
    #[test]
    fn byte_range_map_insert_near_max() {
        let mut map = ByteRangeMap::new(u64::MAX);
        map.insert(ByteRange {
            start: u64::MAX - 1000,
            end: u64::MAX,
        });
        assert_eq!(map.available_bytes(), 1000);
        assert!(map.has_range(u64::MAX - 500, 500));
    }

    /// `contiguous_from` at `u64::MAX` returns 0 (nothing buffered at end).
    #[test]
    fn contiguous_from_max_offset() {
        let map = ByteRangeMap::new(u64::MAX);
        assert_eq!(map.contiguous_from(u64::MAX), 0);
    }

    /// `PieceMapping::piece_at_offset` near `u64::MAX` saturates.
    ///
    /// Very large file offsets should not cause division by zero or overflow.
    #[test]
    fn piece_mapping_large_offset_safe() {
        let mapping = PieceMapping {
            file_offset_in_torrent: 0,
            piece_size: 262144,
            total_pieces: u32::MAX,
            file_size: u64::MAX,
        };
        // Should not panic — piece index will be large but bounded.
        let piece = mapping.piece_at_offset(u64::MAX - 1);
        // Just verify it returned without panicking; exact value is
        // an implementation detail of the u64→u32 truncating cast.
        let _ = piece;
    }

    /// `StreamProgress::download_ratio` with zero file_size returns 1.0.
    ///
    /// An empty file is trivially "fully downloaded" — division by zero
    /// must not occur.
    #[test]
    fn stream_progress_zero_file_size() {
        let progress = StreamProgress {
            position: 0,
            file_size: 0,
            buffered_ahead: 0,
            total_available: 0,
            is_buffering: false,
            fragment_count: 0,
        };
        assert!((progress.download_ratio() - 1.0).abs() < 0.001);
    }

    /// Two adjacent ranges merge into one.
    #[test]
    fn adjacent_ranges_merge() {
        let mut map = ByteRangeMap::new(1000);
        map.insert(ByteRange { start: 0, end: 500 });
        map.insert(ByteRange {
            start: 500,
            end: 1000,
        });
        assert!(map.is_complete());
        assert_eq!(map.ranges().len(), 1);
    }

    /// Overlapping ranges merge correctly.
    #[test]
    fn overlapping_ranges_merge() {
        let mut map = ByteRangeMap::new(1000);
        map.insert(ByteRange { start: 0, end: 600 });
        map.insert(ByteRange {
            start: 400,
            end: 1000,
        });
        assert!(map.is_complete());
        assert_eq!(map.ranges().len(), 1);
    }
}
