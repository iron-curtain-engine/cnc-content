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
    /// Sorted, non-overlapping ranges. Invariant: `ranges[i].end <= ranges[i+1].start`.
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
        let mut map = self
            .state
            .range_map
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        map.insert(range);
        self.state.data_arrived.notify_all();
    }

    /// Marks the entire file as complete (all bytes available).
    pub fn mark_complete(&self) {
        let mut map = self
            .state
            .range_map
            .lock()
            .unwrap_or_else(|e| e.into_inner());
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
        let map = self
            .state
            .range_map
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let buffered_ahead = map.contiguous_from(self.position);
        let total_available = map.available_bytes();
        let is_buffering = buffered_ahead < self.policy.min_prebuffer
            && self.position.saturating_add(buffered_ahead) < self.file_size;

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
        let map = self
            .state
            .range_map
            .lock()
            .unwrap_or_else(|e| e.into_inner());
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
        let map = self
            .state
            .range_map
            .lock()
            .map_err(|_| io::Error::other("stream lock poisoned"))?;

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
            let map = self
                .state
                .range_map
                .lock()
                .map_err(|_| io::Error::other("stream lock poisoned"))?;
            if map.has_range(self.position, needed_len as u64) {
                // Data is available — read directly.
                drop(map);
                return self.read_from_disk(buf, needed_len);
            }
        }

        // Slow path: wait for data to arrive.
        let map = self
            .state
            .range_map
            .lock()
            .map_err(|_| io::Error::other("stream lock poisoned"))?;
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
        // Safety: `len` is bounded by `buf.len()` and `file_size - position`
        // at every call site, but we guard here to avoid a panic if that
        // invariant is ever violated.
        let target = buf
            .get_mut(..len)
            .ok_or_else(|| io::Error::other("read buffer slice OOB"))?;
        let n = self.file.read(target)?;
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

#[cfg(test)]
mod tests;
