// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! Streaming content reader — `Read + Seek` wrapper that blocks until requested
//! bytes are available on disk.
//!
//! Split from `streaming/mod.rs` because the reader implementation (condvar
//! waiting, disk I/O, piece priority) is a self-contained concern separate
//! from the support types (`ByteRangeMap`, `BufferPolicy`, `PieceMapping`).
//!
//! ## Modes
//!
//! - **Disk-backed** (`from_complete_file`): range map is pre-populated, reads
//!   go straight to disk with zero blocking overhead.
//! - **Streaming** (`new_streaming`): range map grows as pieces arrive;
//!   `wait_and_read` blocks on a condvar until needed bytes are available.

use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use super::{BufferPolicy, ByteRange, ByteRangeMap, PieceMapping, StreamProgress};

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
