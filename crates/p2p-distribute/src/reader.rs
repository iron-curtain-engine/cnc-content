// SPDX-License-Identifier: MIT OR Apache-2.0

//! Streaming content reader — `Read + Seek` wrapper that blocks until requested
//! bytes are available on disk.
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

use crate::streaming::{BufferPolicy, ByteRange, ByteRangeMap, PieceMapping, StreamProgress};

/// Shared state between the streaming reader and the piece-completion notifier.
///
/// The notifier (download thread) updates the range map and signals the condvar.
/// The reader waits on the condvar when it needs bytes that aren't available yet.
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
/// use p2p_distribute::StreamingReader;
/// use std::io::Read;
///
/// let tmp = std::env::temp_dir().join("p2p-streaming-doctest");
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
    /// Typically called from the download thread when a piece finishes
    /// verification and is written to disk.
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
    /// entire file, so reads never block.
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
            policy: BufferPolicy::local(),
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
mod tests {
    use super::*;
    use crate::streaming::{BufferPolicy, ByteRange, ByteRangeMap};
    use std::time::Duration;

    // ── StreamingReader (disk-backed fast path) ──────────────────────

    /// `StreamingReader::from_complete_file` opens a fully-downloaded file and
    /// immediately satisfies reads without blocking.
    #[test]
    fn streaming_reader_complete_file() {
        let tmp = std::env::temp_dir().join("p2p-stream-complete");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let path = tmp.join("intro.vqa");
        let data = b"VQA video data for testing streaming reader";
        std::fs::write(&path, data).unwrap();

        let mut reader = StreamingReader::from_complete_file(&path).unwrap();

        assert_eq!(reader.file_size(), data.len() as u64);
        assert_eq!(reader.position(), 0);
        assert!(reader.is_playback_ready());

        let mut buf = vec![0u8; 100];
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, data.len());
        assert_eq!(&buf[..n], data);

        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 0);

        let progress = reader.progress();
        assert!(!progress.is_buffering);
        assert_eq!(progress.fragment_count, 1);
        assert!((progress.download_ratio() - 1.0).abs() < 0.001);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// `StreamingReader` correctly handles `SeekFrom::Start`, `SeekFrom::End`,
    /// and `SeekFrom::Current`.
    #[test]
    fn streaming_reader_seek() {
        let tmp = std::env::temp_dir().join("p2p-stream-seek");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let path = tmp.join("data.vqa");
        let data = b"0123456789ABCDEF";
        std::fs::write(&path, data).unwrap();

        let mut reader = StreamingReader::from_complete_file(&path).unwrap();

        reader.seek(SeekFrom::Start(8)).unwrap();
        assert_eq!(reader.position(), 8);

        let mut buf = [0u8; 4];
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 4);
        assert_eq!(&buf, b"89AB");

        reader.seek(SeekFrom::End(-4)).unwrap();
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 4);
        assert_eq!(&buf, b"CDEF");

        reader.seek(SeekFrom::Start(4)).unwrap();
        reader.seek(SeekFrom::Current(2)).unwrap();
        assert_eq!(reader.position(), 6);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // ── StreamNotifier ───────────────────────────────────────────────

    /// `StreamNotifier::piece_completed` updates the shared range map.
    #[test]
    fn notifier_updates_range_map() {
        let tmp = std::env::temp_dir().join("p2p-stream-notify");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let path = tmp.join("video.vqa");
        let data = vec![0xABu8; 1024];
        std::fs::write(&path, &data).unwrap();

        let range_map = ByteRangeMap::new(1024);
        let (reader, notifier) =
            StreamingReader::new_streaming(&path, range_map, BufferPolicy::default()).unwrap();

        assert!(!reader.is_playback_ready());

        notifier.piece_completed(ByteRange { start: 0, end: 512 });
        notifier.piece_completed(ByteRange {
            start: 512,
            end: 1024,
        });

        assert!(reader.is_playback_ready());

        let progress = reader.progress();
        assert_eq!(progress.total_available, 1024);
        assert!(!progress.is_buffering);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// The streaming reader blocks when data is unavailable and resumes
    /// correctly after a background thread delivers pieces.
    #[test]
    fn notifier_threaded_streaming() {
        let tmp = std::env::temp_dir().join("p2p-stream-threaded");
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
            ..Default::default()
        };

        let (mut reader, notifier) =
            StreamingReader::new_streaming(&path, range_map, policy).unwrap();

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

    /// `StreamNotifier::cancel` wakes a blocked reader with `Interrupted`.
    #[test]
    fn notifier_cancel_wakes_reader() {
        let tmp = std::env::temp_dir().join("p2p-stream-cancel");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let path = tmp.join("video.vqa");
        std::fs::write(&path, [0u8; 1024]).unwrap();

        let range_map = ByteRangeMap::new(1024);
        let policy = BufferPolicy {
            min_prebuffer: 0,
            target_prebuffer: 0,
            read_timeout: Duration::from_secs(30),
            ..Default::default()
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

    // ── Seek edge cases ─────────────────────────────────────────────

    /// Seeking past EOF clamps to file size.
    ///
    /// A consumer seeking to `u64::MAX` must not panic. The position
    /// is clamped to file_size, and subsequent reads return 0 (EOF).
    #[test]
    fn seek_past_eof_clamps() {
        let tmp = std::env::temp_dir().join("p2p-stream-seek-eof");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let path = tmp.join("data.bin");
        std::fs::write(&path, b"short").unwrap();

        let mut reader = StreamingReader::from_complete_file(&path).unwrap();
        let pos = reader.seek(SeekFrom::Start(1000)).unwrap();
        assert_eq!(pos, 5); // clamped to file_size

        let mut buf = [0u8; 10];
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 0); // EOF

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// `SeekFrom::End(0)` positions at the end of file.
    #[test]
    fn seek_end_zero_is_eof() {
        let tmp = std::env::temp_dir().join("p2p-stream-seek-end0");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let path = tmp.join("data.bin");
        std::fs::write(&path, b"ABCDEFGH").unwrap();

        let mut reader = StreamingReader::from_complete_file(&path).unwrap();
        let pos = reader.seek(SeekFrom::End(0)).unwrap();
        assert_eq!(pos, 8);

        let mut buf = [0u8; 4];
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 0);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// `SeekFrom::Current` with negative offset moves backward.
    #[test]
    fn seek_current_negative() {
        let tmp = std::env::temp_dir().join("p2p-stream-seek-neg");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let path = tmp.join("data.bin");
        std::fs::write(&path, b"0123456789").unwrap();

        let mut reader = StreamingReader::from_complete_file(&path).unwrap();
        reader.seek(SeekFrom::Start(8)).unwrap();
        reader.seek(SeekFrom::Current(-4)).unwrap();
        assert_eq!(reader.position(), 4);

        let mut buf = [0u8; 2];
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 2);
        assert_eq!(&buf, b"45");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // ── Empty file ──────────────────────────────────────────────────

    /// Empty file reads return 0 immediately.
    ///
    /// A zero-byte torrent is a valid edge case.
    #[test]
    fn empty_file_reads_zero() {
        let tmp = std::env::temp_dir().join("p2p-stream-empty");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let path = tmp.join("empty.bin");
        std::fs::write(&path, b"").unwrap();

        let mut reader = StreamingReader::from_complete_file(&path).unwrap();
        assert_eq!(reader.file_size(), 0);
        assert!(reader.is_playback_ready());

        let mut buf = [0u8; 10];
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 0);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // ── Timeout ─────────────────────────────────────────────────────

    /// Read on an empty range map times out when no data arrives.
    ///
    /// The streaming reader must return `TimedOut` rather than blocking
    /// forever when the download stalls.
    #[test]
    fn read_times_out_when_no_data() {
        let tmp = std::env::temp_dir().join("p2p-stream-timeout");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let path = tmp.join("stalled.bin");
        std::fs::write(&path, [0u8; 512]).unwrap();

        let range_map = ByteRangeMap::new(512);
        let policy = BufferPolicy {
            min_prebuffer: 0,
            target_prebuffer: 0,
            read_timeout: Duration::from_millis(100),
            ..Default::default()
        };

        let (mut reader, _notifier) =
            StreamingReader::new_streaming(&path, range_map, policy).unwrap();

        let mut buf = [0u8; 64];
        let result = reader.read(&mut buf);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // ── Progress reporting ──────────────────────────────────────────

    /// Progress reflects streaming state accurately.
    ///
    /// download_ratio and is_buffering must reflect the actual state
    /// of the range map relative to file_size.
    #[test]
    fn progress_reflects_partial_state() {
        let tmp = std::env::temp_dir().join("p2p-stream-progress");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let path = tmp.join("video.bin");
        std::fs::write(&path, [0u8; 2048]).unwrap();

        let range_map = ByteRangeMap::new(2048);
        let policy = BufferPolicy {
            min_prebuffer: 1024,
            target_prebuffer: 2048,
            read_timeout: Duration::from_secs(5),
            ..Default::default()
        };

        let (reader, notifier) = StreamingReader::new_streaming(&path, range_map, policy).unwrap();

        // Initially: nothing buffered.
        let p = reader.progress();
        assert_eq!(p.total_available, 0);
        assert!(p.is_buffering);
        assert!((p.download_ratio()).abs() < 0.001);

        // Deliver first half.
        notifier.piece_completed(ByteRange {
            start: 0,
            end: 1024,
        });

        let p = reader.progress();
        assert_eq!(p.total_available, 1024);
        assert!((p.download_ratio() - 0.5).abs() < 0.01);

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
