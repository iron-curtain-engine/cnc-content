// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! Streaming content reader — play video (VQA cutscenes) before full download.
//!
//! This module is a **prototype** of the streaming infrastructure described in
//! the Iron Curtain design documents. It exercises the same patterns that
//! `p2p-distribute` will use at scale, in a simpler context where we can
//! iterate quickly.
//!
//! ## Architecture (per IC distribution analysis)
//!
//! - **ByteRangeMap** — tracks which byte ranges of a file are available on disk.
//!   Maps directly to torrent piece boundaries: when a piece completes, the
//!   corresponding byte range is marked available. (§2.1 content-aware pieces)
//!
//! - **StreamingReader** — `Read + Seek` wrapper that blocks when the requested
//!   bytes aren't available yet. The game engine opens a VQA file, reads frames
//!   sequentially, and the reader transparently waits for pieces to arrive.
//!   Pre-buffering ensures playback doesn't stutter once it starts.
//!
//! - **BufferPolicy** — configurable thresholds for when to start/pause playback.
//!   Mirrors the hot/warm/cold cache tiering from §2.2: an actively-streaming
//!   file is "hot" and gets aggressive piece prioritization.
//!
//! - **PieceMapping** — maps file byte offsets to torrent piece indices. When the
//!   reader needs bytes at offset N, it can request priority for the pieces that
//!   contain those bytes. This is the content-aware piece ordering from §2.1.
//!
//! - **PeerPriority** — prototype of PeerLOD (§2.3): peers that have pieces near
//!   the current playhead get higher priority for unchoke/request scheduling.
//!
//! ## Current state
//!
//! The reader currently works in two modes:
//! 1. **Disk-backed** (default): file is fully downloaded, reads go straight to disk.
//!    This is the `ContentReader` fast path.
//! 2. **Streaming**: file is partially downloaded, reads block until pieces arrive.
//!    A background notifier (channel) wakes the reader when new ranges complete.
//!
//! When `p2p-distribute` replaces `librqbit`, the streaming reader will integrate
//! directly with the piece scheduler for true sequential-priority streaming.

use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

// ── Byte range tracking ───────────────────────────────────────────────

/// A contiguous range of available bytes within a file.
///
/// Ranges are half-open: `[start, end)`. This matches the convention used
/// by HTTP Range headers and torrent piece boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteRange {
    pub start: u64,
    pub end: u64,
}

impl ByteRange {
    /// Returns the length of this range in bytes.
    pub fn len(&self) -> u64 {
        self.end.saturating_sub(self.start)
    }

    /// Returns `true` if this range has zero length.
    pub fn is_empty(&self) -> bool {
        self.end <= self.start
    }

    /// Returns `true` if this range fully contains the given sub-range.
    pub fn contains_range(&self, other: &ByteRange) -> bool {
        self.start <= other.start && self.end >= other.end
    }

    /// Returns `true` if this range overlaps with or is adjacent to another.
    fn mergeable_with(&self, other: &ByteRange) -> bool {
        self.start <= other.end && other.start <= self.end
    }

    /// Merges two overlapping/adjacent ranges into one.
    fn merge(self, other: ByteRange) -> ByteRange {
        ByteRange {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }
}

/// Tracks which byte ranges of a file are available.
///
/// As torrent pieces complete, the corresponding byte ranges are inserted.
/// The map automatically coalesces adjacent/overlapping ranges to keep the
/// internal list compact. For a typical video file with sequential download,
/// this quickly converges to a single range covering all downloaded bytes.
///
/// This is the byte-level equivalent of a torrent "have" bitfield, but at
/// file granularity rather than torrent granularity. It exists because a
/// single torrent may contain multiple files, and the streaming reader needs
/// to know about availability at the file level.
#[derive(Debug, Clone)]
pub struct ByteRangeMap {
    /// Sorted, non-overlapping ranges. Invariant: ranges[i].end <= ranges[i+1].start.
    ranges: Vec<ByteRange>,
    /// Total file size (used for "is everything available?" checks).
    file_size: u64,
}

impl ByteRangeMap {
    /// Creates a new empty range map for a file of the given size.
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
        let needed = ByteRange {
            start,
            end: start + len,
        };
        self.ranges.iter().any(|r| r.contains_range(&needed))
    }

    /// Returns how many contiguous bytes are available starting from `offset`.
    ///
    /// This is the key query for the streaming reader: "how far ahead can I
    /// read without blocking?" The answer determines whether pre-buffering
    /// thresholds are met.
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

// ── Buffer policy (IC §2.2 cache tiering) ─────────────────────────────

/// Controls when streaming playback may begin and when it should pause.
///
/// Modeled after the hot/warm/cold cache tiers from the IC distribution
/// analysis §2.2. An actively-streaming file is in the "hot" tier and
/// gets aggressive pre-fetching. A file that's been paused drops to
/// "warm" with reduced priority.
///
/// The thresholds are designed for C&C FMV cutscenes (VQA format):
/// - Typical bitrate: ~300 KB/s for 320x200 VQA
/// - Typical piece size: 256 KB
/// - So `min_prebuffer` of 512 KB ≈ ~1.7 seconds of video
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
}

impl Default for BufferPolicy {
    fn default() -> Self {
        Self {
            // 512 KB ≈ 1.7s of 300 KB/s VQA video
            min_prebuffer: 512 * 1024,
            // 2 MB ≈ 6.8s of buffer — enough to absorb piece-arrival jitter
            target_prebuffer: 2 * 1024 * 1024,
            // 30s timeout — if no data arrives in 30s, the swarm is likely dead
            read_timeout: Duration::from_secs(30),
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
        }
    }

    /// Policy for high-bitrate content (e.g. upscaled cutscenes).
    pub fn high_bitrate() -> Self {
        Self {
            min_prebuffer: 2 * 1024 * 1024,
            target_prebuffer: 8 * 1024 * 1024,
            read_timeout: Duration::from_secs(60),
        }
    }
}

// ── Piece mapping (IC §2.1 content-aware pieces) ──────────────────────

/// Maps file byte offsets to torrent piece indices.
///
/// In a multi-file torrent, a single piece can span multiple files. This
/// mapping tells the streaming reader which piece(s) it needs for a given
/// file byte range, enabling precise priority requests.
///
/// Per IC distribution analysis §2.1 (content-aware piece sizing):
/// "Piece boundaries aligned to file boundaries where possible, so that
/// requesting a file implicitly requests whole pieces."
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
/// The game engine's UI can poll this to display a buffering indicator,
/// download speed, and piece-level progress visualization.
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

// ── Peer priority (IC §2.3 PeerLOD) ──────────────────────────────────

/// Priority level assigned to a peer based on which pieces it has relative
/// to the current playhead.
///
/// Per IC distribution analysis §2.3 (PeerLOD — Level of Detail):
/// "Peers holding pieces near the playhead are HIGH priority for unchoke
/// and request scheduling. Peers with only distant pieces are LOW — still
/// useful for background download but not urgent."
///
/// The streaming reader uses this to hint the torrent client about which
/// peers to prefer when requesting pieces.
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

// ── Streaming reader ──────────────────────────────────────────────────

/// Shared state between the streaming reader and the piece-completion notifier.
///
/// The notifier (torrent download thread) updates the range map and signals
/// the condvar. The reader waits on the condvar when it needs bytes that
/// aren't available yet.
struct SharedStreamState {
    range_map: Mutex<ByteRangeMap>,
    data_arrived: Condvar,
    cancelled: AtomicBool,
}

/// A streaming content reader that blocks until requested bytes are available.
///
/// Implements `Read` and `Seek`. When all bytes are available (file fully
/// downloaded), reads go straight to disk with zero overhead — the condvar
/// is never touched. When bytes are missing, the reader blocks on a condvar
/// until the piece-completion notifier signals that new data has arrived.
///
/// ## Usage
///
/// ```rust
/// use cnc_content::streaming::StreamingReader;
/// use std::io::Read;
///
/// let tmp = std::env::temp_dir().join("cnc-streaming-doctest");
/// let _ = std::fs::remove_dir_all(&tmp);
/// std::fs::create_dir_all(&tmp).unwrap();
/// let path = tmp.join("intro.vqa");
/// std::fs::write(&path, b"VQA stream data").unwrap();
///
/// let mut reader = StreamingReader::from_complete_file(&path).unwrap();
/// let mut buf = Vec::new();
/// reader.read_to_end(&mut buf).unwrap();
/// assert_eq!(buf, b"VQA stream data");
/// let _ = std::fs::remove_dir_all(&tmp);
/// ```
pub struct StreamingReader {
    file: std::fs::File,
    path: PathBuf,
    file_size: u64,
    position: u64,
    policy: BufferPolicy,
    state: Arc<SharedStreamState>,
    /// Optional piece mapping for priority requests.
    piece_mapping: Option<PieceMapping>,
}

/// Handle given to the download thread to notify the reader of new data.
///
/// When a torrent piece completes, call `piece_completed` with the byte
/// range that is now available. The reader will wake up if it was blocked
/// waiting for those bytes.
pub struct StreamNotifier {
    state: Arc<SharedStreamState>,
}

impl StreamNotifier {
    /// Notifies the reader that a byte range is now available on disk.
    ///
    /// Typically called from the torrent download thread when a piece
    /// finishes verification and is written to disk.
    pub fn piece_completed(&self, range: ByteRange) {
        let mut map = self.state.range_map.lock().unwrap();
        map.insert(range);
        self.state.data_arrived.notify_all();
    }

    /// Marks the entire file as complete (all bytes available).
    pub fn mark_complete(&self) {
        let mut map = self.state.range_map.lock().unwrap();
        let file_size = map.file_size();
        map.insert(ByteRange {
            start: 0,
            end: file_size,
        });
        self.state.data_arrived.notify_all();
    }

    /// Cancels the stream — the reader will return an error on next read.
    pub fn cancel(&self) {
        self.state.cancelled.store(true, Ordering::Relaxed);
        self.state.data_arrived.notify_all();
    }
}

impl StreamingReader {
    /// Opens a streaming reader for a fully-downloaded file.
    ///
    /// This is the fast path: the range map is pre-populated to cover the
    /// entire file, so reads never block. Equivalent to `ContentReader` but
    /// with the streaming interface for API uniformity.
    pub fn from_complete_file(path: &Path) -> Result<Self, io::Error> {
        let file = std::fs::File::open(path)?;
        let file_size = file.metadata()?.len();
        let range_map = ByteRangeMap::fully_available(file_size);

        let state = Arc::new(SharedStreamState {
            range_map: Mutex::new(range_map),
            data_arrived: Condvar::new(),
            cancelled: AtomicBool::new(false),
        });

        Ok(Self {
            file,
            path: path.to_path_buf(),
            file_size,
            position: 0,
            policy: BufferPolicy::local(), // no buffering needed
            state,
            piece_mapping: None,
        })
    }

    /// Creates a streaming reader for a partially-downloaded file.
    ///
    /// Returns both the reader and a `StreamNotifier` that the download
    /// thread uses to signal when new byte ranges become available.
    pub fn new_streaming(
        path: &Path,
        range_map: ByteRangeMap,
        policy: BufferPolicy,
    ) -> Result<(Self, StreamNotifier), io::Error> {
        let file = std::fs::File::open(path)?;
        let file_size = file.metadata()?.len();

        let state = Arc::new(SharedStreamState {
            range_map: Mutex::new(range_map),
            data_arrived: Condvar::new(),
            cancelled: AtomicBool::new(false),
        });

        let reader = Self {
            file,
            path: path.to_path_buf(),
            file_size,
            position: 0,
            policy,
            state: Arc::clone(&state),
            piece_mapping: None,
        };

        let notifier = StreamNotifier { state };
        Ok((reader, notifier))
    }

    /// Sets the piece mapping for this reader, enabling piece-level priority.
    pub fn set_piece_mapping(&mut self, mapping: PieceMapping) {
        self.piece_mapping = Some(mapping);
    }

    /// Returns the full path to the backing file on disk.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the total file size in bytes.
    pub fn file_size(&self) -> u64 {
        self.file_size
    }

    /// Returns the current read position.
    pub fn position(&self) -> u64 {
        self.position
    }

    /// Returns current stream progress (for UI display).
    pub fn progress(&self) -> StreamProgress {
        let map = self.state.range_map.lock().unwrap();
        let buffered_ahead = map.contiguous_from(self.position);
        let total_available = map.available_bytes();
        let is_buffering = buffered_ahead < self.policy.min_prebuffer
            && self.position + buffered_ahead < self.file_size;

        StreamProgress {
            position: self.position,
            file_size: self.file_size,
            buffered_ahead,
            total_available,
            is_buffering,
            fragment_count: map.ranges().len(),
        }
    }

    /// Returns `true` if enough data is buffered ahead to start/continue playback.
    pub fn is_playback_ready(&self) -> bool {
        let map = self.state.range_map.lock().unwrap();
        let buffered = map.contiguous_from(self.position);
        let remaining = self.file_size - self.position;
        // Ready if: enough buffer OR the rest of the file is available.
        buffered >= self.policy.min_prebuffer || buffered >= remaining
    }

    /// Waits until enough data is buffered to start playback, or timeout.
    ///
    /// Call this before the first read to ensure smooth playback start.
    /// Returns `Ok(true)` if ready, `Ok(false)` if timed out.
    pub fn wait_for_prebuffer(&self) -> Result<bool, io::Error> {
        let map = self.state.range_map.lock().unwrap();

        let result = self
            .state
            .data_arrived
            .wait_timeout_while(map, self.policy.read_timeout, |map| {
                if self.state.cancelled.load(Ordering::Relaxed) {
                    return false; // stop waiting
                }
                let buffered = map.contiguous_from(self.position);
                let remaining = self.file_size - self.position;
                buffered < self.policy.min_prebuffer && buffered < remaining
            })
            .map_err(|_| io::Error::other("stream lock poisoned"))?;

        if self.state.cancelled.load(Ordering::Relaxed) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "stream cancelled",
            ));
        }

        Ok(!result.1.timed_out())
    }

    /// Returns the pieces the reader currently needs (for priority requests).
    ///
    /// When the torrent client asks "which pieces should I prioritize?", the
    /// streaming reader answers with the pieces covering the pre-buffer window
    /// ahead of the current playhead.
    pub fn needed_pieces(&self) -> Option<std::ops::RangeInclusive<u32>> {
        let mapping = self.piece_mapping.as_ref()?;
        Some(mapping.pieces_for_range(self.position, self.policy.target_prebuffer))
    }

    /// Blocks until the requested byte range is available, then reads from disk.
    fn wait_and_read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let needed_len = buf.len().min((self.file_size - self.position) as usize);
        if needed_len == 0 {
            return Ok(0); // EOF
        }

        // Fast path: check if data is already available without waiting.
        {
            let map = self.state.range_map.lock().unwrap();
            if map.has_range(self.position, needed_len as u64) {
                // Data is available — read directly.
                drop(map);
                return self.read_from_disk(buf, needed_len);
            }
        }

        // Slow path: wait for data to arrive.
        let map = self.state.range_map.lock().unwrap();
        let result = self
            .state
            .data_arrived
            .wait_timeout_while(map, self.policy.read_timeout, |map| {
                if self.state.cancelled.load(Ordering::Relaxed) {
                    return false;
                }
                // Wait until at least 1 byte is available at the current position.
                map.contiguous_from(self.position) == 0
            })
            .map_err(|_| io::Error::other("stream lock poisoned"))?;

        if self.state.cancelled.load(Ordering::Relaxed) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "stream cancelled",
            ));
        }

        if result.1.timed_out() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "stream read timed out waiting for data",
            ));
        }

        // Read whatever is available (may be less than buf.len()).
        let available = result.0.contiguous_from(self.position) as usize;
        let read_len = needed_len.min(available);
        drop(result);
        self.read_from_disk(buf, read_len)
    }

    /// Reads bytes from the backing file at the current position.
    fn read_from_disk(&mut self, buf: &mut [u8], len: usize) -> io::Result<usize> {
        self.file.seek(SeekFrom::Start(self.position))?;
        let n = self.file.read(&mut buf[..len])?;
        self.position += n as u64;
        Ok(n)
    }
}

impl Read for StreamingReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.position >= self.file_size {
            return Ok(0); // EOF
        }
        self.wait_and_read(buf)
    }
}

impl Seek for StreamingReader {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let new_pos = match pos {
            SeekFrom::Start(n) => n,
            SeekFrom::End(n) => {
                if n >= 0 {
                    self.file_size.saturating_add(n as u64)
                } else {
                    self.file_size.saturating_sub((-n) as u64)
                }
            }
            SeekFrom::Current(n) => {
                if n >= 0 {
                    self.position.saturating_add(n as u64)
                } else {
                    self.position.saturating_sub((-n) as u64)
                }
            }
        };
        self.position = new_pos.min(self.file_size);
        Ok(self.position)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── ByteRange ────────────────────────────────────────────────────

    /// `ByteRange::len` returns the number of bytes in the range and
    /// `is_empty` correctly reflects a zero-length range.
    ///
    /// Both computations are trivial arithmetic, but they are used by every
    /// higher-level query in `ByteRangeMap`, so an off-by-one here would
    /// silently corrupt all availability checks.
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
    ///
    /// The streaming reader calls this to decide whether a requested read
    /// window is already on disk; a false positive would cause a read from
    /// bytes that haven't arrived yet.
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
    ///
    /// The initial state must be completely "unavailable" so the streaming
    /// reader correctly blocks on its first read rather than immediately
    /// attempting a disk read against a file that has no data yet.
    #[test]
    fn range_map_empty() {
        let map = ByteRangeMap::new(1000);
        assert!(!map.is_complete());
        assert_eq!(map.available_bytes(), 0);
        assert!(!map.has_range(0, 1));
        assert_eq!(map.contiguous_from(0), 0);
    }

    /// `ByteRangeMap::fully_available` marks the entire file as available from
    /// the start, and `contiguous_from` reports the correct shrinking distance
    /// as the logical playhead advances.
    ///
    /// This is the fast path taken by `StreamingReader::from_complete_file`;
    /// any mistake here would cause complete-file reads to block unnecessarily.
    #[test]
    fn range_map_fully_available() {
        let map = ByteRangeMap::fully_available(1000);
        assert!(map.is_complete());
        assert_eq!(map.available_bytes(), 1000);
        assert!(map.has_range(0, 1000));
        assert_eq!(map.contiguous_from(0), 1000);
        assert_eq!(map.contiguous_from(500), 500);
    }

    /// Inserting a range that bridges two existing non-adjacent ranges causes
    /// all three to be coalesced into a single range.
    ///
    /// Correct coalescing is critical for the streaming reader: once pieces
    /// arrive sequentially the internal list must collapse to one entry so
    /// `contiguous_from` returns the full available length without gaps.
    /// The test inserts the bridging piece last to exercise the merge path.
    #[test]
    fn range_map_insert_and_coalesce() {
        let mut map = ByteRangeMap::new(1000);

        // Insert two non-adjacent ranges.
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

    /// Inserting a range that partially overlaps an existing range merges them
    /// into a single range spanning the union of both.
    ///
    /// Torrent pieces can overlap at file boundaries in multi-file torrents;
    /// duplicate or overlapping notifications must not create duplicate entries
    /// or inflate `available_bytes`.
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

    /// `first_gap_from` returns the byte offset of the first unavailable byte
    /// starting at a given position, including when the position itself is
    /// inside an available range (gap starts at range end) and when it falls
    /// in a gap (gap starts at the position itself).
    ///
    /// This is used by the streaming reader to request the next needed piece;
    /// returning the wrong offset would cause the wrong piece to be prioritised.
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
        assert_eq!(map.first_gap_from(100), Some(100)); // not in any range
        assert_eq!(map.first_gap_from(200), Some(300));
    }

    /// `first_gap_from` returns `None` for every position in a fully-available
    /// file, meaning the streaming reader will never request additional pieces.
    ///
    /// Returning `Some` here would incorrectly signal that a piece is missing
    /// and cause spurious priority requests for a complete download.
    #[test]
    fn range_map_no_gap_when_complete() {
        let map = ByteRangeMap::fully_available(1000);
        assert_eq!(map.first_gap_from(0), None);
        assert_eq!(map.first_gap_from(999), None);
    }

    // ── BufferPolicy ─────────────────────────────────────────────────

    /// `BufferPolicy::default` uses the documented values tuned for C&C VQA
    /// cutscenes (~300 KB/s, 256 KB pieces, 30-second swarm timeout).
    ///
    /// Downstream code (pre-buffer logic, UI) reads these constants directly;
    /// accidental changes would silently degrade streaming behaviour.
    #[test]
    fn buffer_policy_defaults() {
        let policy = BufferPolicy::default();
        assert_eq!(policy.min_prebuffer, 512 * 1024);
        assert_eq!(policy.target_prebuffer, 2 * 1024 * 1024);
        assert_eq!(policy.read_timeout, Duration::from_secs(30));
    }

    // ── PieceMapping ─────────────────────────────────────────────────

    /// A file that fits entirely within one torrent piece maps every byte
    /// range to piece 0.
    ///
    /// Small files (e.g. C&C audio samples) often fit in a single piece; the
    /// reader must request exactly that piece and no more.
    #[test]
    fn piece_mapping_single_piece() {
        let mapping = PieceMapping {
            file_offset_in_torrent: 0,
            piece_size: 262144, // 256 KB
            total_pieces: 100,
            file_size: 100000,
        };
        // Entire file fits in piece 0.
        assert_eq!(mapping.pieces_for_range(0, 100000), 0..=0);
        assert_eq!(mapping.piece_at_offset(0), 0);
    }

    /// A 1 MB file with 256 KB pieces spans four pieces (0–3), and a read
    /// halfway through the file falls in piece 1.
    ///
    /// Correct multi-piece spans are essential for the pre-buffer window:
    /// `needed_pieces` must return the full range so the torrent client
    /// prioritises every piece that will be needed during playback.
    #[test]
    fn piece_mapping_multi_piece() {
        let mapping = PieceMapping {
            file_offset_in_torrent: 0,
            piece_size: 262144,
            total_pieces: 100,
            file_size: 1_000_000,
        };
        // File spans pieces 0..=3 (262144 * 4 = 1048576 > 1000000).
        assert_eq!(mapping.all_file_pieces(), 0..=3);
        // Reading from middle of file.
        assert_eq!(mapping.piece_at_offset(500_000), 1);
    }

    /// When the file starts at a non-zero offset within the torrent (as is
    /// common in multi-file torrents), piece indices are computed relative to
    /// the torrent's concatenated data, not the file itself.
    ///
    /// A file at torrent offset 1 MiB with 256 KB pieces starts at piece 4.
    /// Failing to account for `file_offset_in_torrent` would request the
    /// wrong pieces from peers.
    #[test]
    fn piece_mapping_with_offset() {
        // File starts at 1 MB into the torrent.
        let mapping = PieceMapping {
            file_offset_in_torrent: 1_048_576,
            piece_size: 262144,
            total_pieces: 100,
            file_size: 500_000,
        };
        // File starts at piece 4 (1048576 / 262144).
        assert_eq!(mapping.piece_at_offset(0), 4);
        assert_eq!(mapping.all_file_pieces(), 4..=5);
    }

    // ── PeerPriority ─────────────────────────────────────────────────

    /// A peer that has the exact piece the playhead is currently waiting for
    /// receives `High` priority, regardless of what other pieces it holds.
    ///
    /// `High` peers are unchoked first; misclassifying them as `Medium` would
    /// delay the critical piece and cause playback to stall.
    #[test]
    fn peer_priority_high_for_playhead() {
        let priority = evaluate_peer_priority(&[5, 6, 7], 5, 10);
        assert_eq!(priority, PeerPriority::High);
    }

    /// A peer holding pieces inside the pre-buffer window (after the playhead
    /// but before `prebuffer_end_piece`) receives `Medium` priority.
    ///
    /// These pieces are not yet blocking playback but will be needed soon;
    /// `Medium` keeps them ahead of background download without displacing
    /// the truly urgent `High`-priority piece.
    #[test]
    fn peer_priority_medium_for_prebuffer() {
        let priority = evaluate_peer_priority(&[7, 8, 9], 5, 10);
        assert_eq!(priority, PeerPriority::Medium);
    }

    /// A peer that only has pieces far beyond the pre-buffer window receives
    /// `Low` priority — useful for background download but not urgent.
    ///
    /// Promoting distant peers to `Medium` or `High` would waste unchoke
    /// slots on data that isn't needed for several seconds of playback.
    #[test]
    fn peer_priority_low_for_distant() {
        let priority = evaluate_peer_priority(&[50, 51], 5, 10);
        assert_eq!(priority, PeerPriority::Low);
    }

    /// A peer advertising no pieces receives `None` priority and should not
    /// be unchoked for this stream.
    ///
    /// An empty piece list is the normal state for a newly-connected peer
    /// before it has sent a bitfield; treating it as `Low` would waste
    /// an unchoke slot.
    #[test]
    fn peer_priority_none_for_empty() {
        let priority = evaluate_peer_priority(&[], 5, 10);
        assert_eq!(priority, PeerPriority::None);
    }

    // ── StreamProgress ───────────────────────────────────────────────

    /// `buffer_fill_ratio` and `download_ratio` return correct fractions for
    /// a partially-buffered, half-downloaded file.
    ///
    /// The UI uses these ratios to drive progress bars and buffering
    /// indicators; floating-point arithmetic must be accurate to three
    /// decimal places to avoid display glitches.
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
        // 256KB / 2MB = 0.125
        let ratio = progress.buffer_fill_ratio(&policy);
        assert!((ratio - 0.125).abs() < 0.001);

        // 500 / 1000 = 0.5
        assert!((progress.download_ratio() - 0.5).abs() < 0.001);
    }

    // ── StreamingReader (disk-backed fast path) ──────────────────────

    /// `StreamingReader::from_complete_file` opens a fully-downloaded file and
    /// immediately satisfies reads without blocking, returning correct data and
    /// a `download_ratio` of 1.0 after the read completes.
    ///
    /// This exercises the fast path where the range map is pre-populated for
    /// the entire file, which is the common case once a download finishes.
    /// A temporary directory is written and cleaned up by the test itself.
    #[test]
    fn streaming_reader_complete_file() {
        let tmp = std::env::temp_dir().join("cnc-stream-complete");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let path = tmp.join("intro.vqa");
        let data = b"VQA video data for testing streaming reader";
        std::fs::write(&path, data).unwrap();

        let mut reader = StreamingReader::from_complete_file(&path).unwrap();

        assert_eq!(reader.file_size(), data.len() as u64);
        assert_eq!(reader.position(), 0);
        assert!(reader.is_playback_ready());

        // Read all data.
        let mut buf = vec![0u8; 100];
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, data.len());
        assert_eq!(&buf[..n], data);

        // EOF.
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 0);

        let progress = reader.progress();
        assert!(!progress.is_buffering);
        assert_eq!(progress.fragment_count, 1);
        assert!((progress.download_ratio() - 1.0).abs() < 0.001);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// `StreamingReader` correctly handles `SeekFrom::Start`, `SeekFrom::End`,
    /// and `SeekFrom::Current`, returning the right bytes at each position.
    ///
    /// The game engine seeks into VQA files to skip frame headers or jump to
    /// specific frames; incorrect seek arithmetic would produce corrupted video.
    #[test]
    fn streaming_reader_seek() {
        let tmp = std::env::temp_dir().join("cnc-stream-seek");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let path = tmp.join("data.vqa");
        let data = b"0123456789ABCDEF";
        std::fs::write(&path, data).unwrap();

        let mut reader = StreamingReader::from_complete_file(&path).unwrap();

        // Seek to middle — offset 8 in "0123456789ABCDEF" is "89AB".
        reader.seek(SeekFrom::Start(8)).unwrap();
        assert_eq!(reader.position(), 8);

        let mut buf = [0u8; 4];
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 4);
        assert_eq!(&buf, b"89AB");

        // Seek from end — last 4 bytes are "CDEF".
        reader.seek(SeekFrom::End(-4)).unwrap();
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 4);
        assert_eq!(&buf, b"CDEF");

        // Seek from current.
        reader.seek(SeekFrom::Start(4)).unwrap();
        reader.seek(SeekFrom::Current(2)).unwrap();
        assert_eq!(reader.position(), 6);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // ── StreamNotifier ───────────────────────────────────────────────

    /// `StreamNotifier::piece_completed` updates the shared range map so that
    /// the reader correctly transitions from "not ready" to "playback ready"
    /// once enough data has been delivered.
    ///
    /// The test uses the default policy (512 KB min pre-buffer) against a 1 KB
    /// file so that delivering the full file satisfies the "buffered >= remaining"
    /// branch rather than the pre-buffer threshold, exercising that edge case.
    #[test]
    fn notifier_updates_range_map() {
        let tmp = std::env::temp_dir().join("cnc-stream-notify");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let path = tmp.join("video.vqa");
        let data = vec![0xABu8; 1024];
        std::fs::write(&path, &data).unwrap();

        let range_map = ByteRangeMap::new(1024);
        // Use default policy (512KB min_prebuffer) so initially NOT ready.
        let (reader, notifier) =
            StreamingReader::new_streaming(&path, range_map, BufferPolicy::default()).unwrap();

        // Initially nothing is available — min_prebuffer > 0 so not ready.
        assert!(!reader.is_playback_ready());

        // Simulate piece completion — deliver entire file.
        notifier.piece_completed(ByteRange { start: 0, end: 512 });
        notifier.piece_completed(ByteRange {
            start: 512,
            end: 1024,
        });

        // File fully available (1024 bytes) — remaining < min_prebuffer but
        // buffered >= remaining, so playback is ready.
        assert!(reader.is_playback_ready());

        let progress = reader.progress();
        assert_eq!(progress.total_available, 1024);
        assert!(!progress.is_buffering);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// The streaming reader blocks when data is unavailable and resumes
    /// correctly after a background thread delivers pieces with real delays,
    /// producing bit-exact output compared to the original file.
    ///
    /// This is the core end-to-end integration test for the condvar-based
    /// wake-up mechanism. A 2 KB file is delivered in two 1 KB pieces with
    /// 50 ms delays between them; `min_prebuffer` is zero so the reader
    /// unblocks as soon as the first byte of each piece arrives.
    #[test]
    fn notifier_threaded_streaming() {
        let tmp = std::env::temp_dir().join("cnc-stream-threaded");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let path = tmp.join("cutscene.vqa");
        let data: Vec<u8> = (0..=255u8).cycle().take(2048).collect();
        std::fs::write(&path, &data).unwrap();

        let range_map = ByteRangeMap::new(2048);
        let policy = BufferPolicy {
            min_prebuffer: 0,
            target_prebuffer: 0,
            read_timeout: Duration::from_secs(5),
        };

        let (mut reader, notifier) =
            StreamingReader::new_streaming(&path, range_map, policy).unwrap();

        // Spawn a thread that delivers pieces with a small delay.
        let handle = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            notifier.piece_completed(ByteRange {
                start: 0,
                end: 1024,
            });
            std::thread::sleep(Duration::from_millis(50));
            notifier.piece_completed(ByteRange {
                start: 1024,
                end: 2048,
            });
        });

        // Reader should block until data arrives, then succeed.
        let mut result = Vec::new();
        let mut buf = [0u8; 512];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => result.extend_from_slice(&buf[..n]),
                Err(e) => panic!("read error: {e}"),
            }
        }

        assert_eq!(result, data);
        handle.join().unwrap();

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Calling `StreamNotifier::cancel` while the reader is blocked waiting
    /// for data causes the read to return `Err(ErrorKind::Interrupted)` rather
    /// than hanging until the 30-second timeout.
    ///
    /// Cancellation is the shutdown path when the user quits mid-download;
    /// the reader must unblock promptly and propagate `Interrupted` so the
    /// caller can clean up without waiting for the full timeout.
    #[test]
    fn notifier_cancel_wakes_reader() {
        let tmp = std::env::temp_dir().join("cnc-stream-cancel");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let path = tmp.join("video.vqa");
        std::fs::write(&path, [0u8; 1024]).unwrap();

        let range_map = ByteRangeMap::new(1024);
        let policy = BufferPolicy {
            min_prebuffer: 0,
            target_prebuffer: 0,
            read_timeout: Duration::from_secs(30),
        };

        let (mut reader, notifier) =
            StreamingReader::new_streaming(&path, range_map, policy).unwrap();

        let handle = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            notifier.cancel();
        });

        let mut buf = [0u8; 256];
        let result = reader.read(&mut buf);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);

        handle.join().unwrap();

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
