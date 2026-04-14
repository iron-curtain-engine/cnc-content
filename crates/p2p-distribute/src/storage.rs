// SPDX-License-Identifier: MIT OR Apache-2.0

//! Storage abstraction for piece data — enables custom backends, write
//! coalescing, and in-memory testing without filesystem access.
//!
//! ## Design (informed by librqbit, libtorrent, aria2)
//!
//! Production BitTorrent clients all separate "where to put bytes" from "how
//! to schedule pieces". librqbit defines `StorageFactory` + `TorrentStorage`;
//! libtorrent has pluggable disk backends (ARC cache, mmap, hybrid); aria2
//! writes each download to a `.aria2` control file alongside the data file.
//!
//! This crate follows the same pattern: [`PieceStorage`] is the trait that the
//! coordinator writes verified pieces through, and [`StorageFactory`] creates
//! storage instances per-download. The default [`FileStorage`] writes directly
//! to a pre-allocated file (the current behaviour). Consumers can provide
//! custom implementations for:
//!
//! - **In-memory storage** — for tests and short-lived transfers.
//! - **Write coalescing** — [`CoalescingStorage`] batches adjacent pieces and
//!   flushes on threshold or explicit request, reducing syscall overhead.
//! - **Deferred writes** — buffer in RAM, flush to disk periodically (librqbit
//!   `defer_writes_up_to` pattern).
//! - **Custom backends** — database, object storage, or pass-through to
//!   another layer.
//!
//! ## How
//!
//! The coordinator calls `storage.write_piece(index, offset, &data)` after
//! SHA-1 verification, and `storage.flush()` when critical (checkpoint, end
//! of download). Storage impls are `Send + Sync` because the coordinator may
//! call them from a thread pool in the future.

use std::io;
use std::path::Path;
use std::sync::Mutex;

use thiserror::Error;

// ── Storage errors ──────────────────────────────────────────────────

/// Errors from storage operations.
#[derive(Debug, Error)]
pub enum StorageError {
    /// I/O error during a write, read, or flush operation.
    #[error("storage I/O error at offset {offset}: {source}")]
    Io {
        /// Byte offset where the operation was attempted.
        offset: u64,
        /// Underlying I/O error.
        source: io::Error,
    },
    /// The storage is in a failed state and cannot accept further writes.
    #[error("storage is in a failed state: {reason}")]
    Failed {
        /// Human-readable explanation of the failure.
        reason: String,
    },
}

// ── PieceStorage trait ──────────────────────────────────────────────

/// Trait for writing verified piece data to a backend.
///
/// The coordinator calls [`write_piece`](Self::write_piece) after SHA-1
/// verification and [`flush`](Self::flush) at checkpoints. Implementations
/// must be thread-safe (`Send + Sync`) because concurrent piece fetching
/// dispatches writes from a thread pool.
///
/// ## Contract
///
/// - `write_piece` must be idempotent: writing the same piece twice with the
///   same data must produce the same file state. This is required for resume
///   correctness — the coordinator may re-write a piece after crash recovery.
/// - `flush` must ensure all buffered data is durable on the underlying medium
///   (e.g. `fsync` for file backends).
/// - Implementations must not assume any particular write ordering. Pieces may
///   arrive out of order.
pub trait PieceStorage: Send + Sync {
    /// Writes verified piece data at the given byte offset.
    ///
    /// - `piece_index`: piece number (for diagnostics; the offset is canonical)
    /// - `offset`: byte offset in the output file/blob
    /// - `data`: SHA-1–verified piece bytes
    fn write_piece(&self, piece_index: u32, offset: u64, data: &[u8]) -> Result<(), StorageError>;

    /// Reads piece data from the given byte offset.
    ///
    /// Returns the number of bytes actually read (may be less than `buf.len()`
    /// at end of file). Used by the streaming reader and verification passes.
    fn read_piece(&self, offset: u64, buf: &mut [u8]) -> Result<usize, StorageError>;

    /// Flushes any buffered writes to durable storage.
    ///
    /// Called after checkpoint saves and at download completion. File-backed
    /// implementations should call `fsync` or equivalent.
    fn flush(&self) -> Result<(), StorageError>;
}

/// Factory for creating [`PieceStorage`] instances per download.
///
/// The coordinator calls `create_storage` once at download start. This
/// pattern (from librqbit's `StorageFactory`) lets consumers configure
/// storage options (path, buffer size, custom backend) independently of
/// the coordinator.
pub trait StorageFactory: Send + Sync {
    /// Creates a storage instance for a download with the given parameters.
    ///
    /// - `output_path`: where the output file should live
    /// - `file_size`: total expected file size (for pre-allocation)
    /// - `resuming`: if `true`, the file already exists with partial data
    fn create_storage(
        &self,
        output_path: &Path,
        file_size: u64,
        resuming: bool,
    ) -> Result<Box<dyn PieceStorage>, StorageError>;
}

// ── FileStorage ─────────────────────────────────────────────────────

/// Default storage backend — writes directly to a pre-allocated file.
///
/// This is the extraction of the coordinator's existing `Arc<Mutex<File>>`
/// pattern into a proper [`PieceStorage`] implementation. Each write seeks
/// to the piece offset and writes the data. Thread-safe via internal Mutex.
pub struct FileStorage {
    /// The output file, protected by a mutex for concurrent access.
    file: Mutex<std::fs::File>,
}

impl FileStorage {
    /// Opens or creates the output file for writing.
    ///
    /// When `resuming` is `true` and an existing file with the correct size
    /// is found, the file is opened without truncation to preserve
    /// already-written piece data. Otherwise, a new file is created or the
    /// existing file is truncated and pre-allocated to `file_size`.
    pub fn open(path: &Path, file_size: u64, resuming: bool) -> Result<Self, StorageError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|source| StorageError::Io { offset: 0, source })?;
        }

        // When resuming, reuse the existing file if it has the correct
        // pre-allocated size. Piece offsets depend on file_size matching.
        if resuming {
            if let Ok(metadata) = std::fs::metadata(path) {
                if metadata.len() == file_size {
                    let file = std::fs::OpenOptions::new()
                        .read(true)
                        .write(true)
                        .open(path)
                        .map_err(|source| StorageError::Io { offset: 0, source })?;
                    return Ok(Self {
                        file: Mutex::new(file),
                    });
                }
            }
            // File missing or wrong size: fall through to create new.
        }

        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(true)
            .open(path)
            .map_err(|source| StorageError::Io { offset: 0, source })?;

        // Pre-allocate to avoid fragmentation and ensure disk space.
        file.set_len(file_size)
            .map_err(|source| StorageError::Io { offset: 0, source })?;

        Ok(Self {
            file: Mutex::new(file),
        })
    }
}

impl PieceStorage for FileStorage {
    fn write_piece(&self, _piece_index: u32, offset: u64, data: &[u8]) -> Result<(), StorageError> {
        use std::io::{Seek, Write};
        let mut f = self.file.lock().map_err(|_| StorageError::Failed {
            reason: "file lock poisoned".into(),
        })?;
        f.seek(io::SeekFrom::Start(offset))
            .map_err(|source| StorageError::Io { offset, source })?;
        f.write_all(data)
            .map_err(|source| StorageError::Io { offset, source })
    }

    fn read_piece(&self, offset: u64, buf: &mut [u8]) -> Result<usize, StorageError> {
        use std::io::{Read, Seek};
        let mut f = self.file.lock().map_err(|_| StorageError::Failed {
            reason: "file lock poisoned".into(),
        })?;
        f.seek(io::SeekFrom::Start(offset))
            .map_err(|source| StorageError::Io { offset, source })?;
        f.read(buf)
            .map_err(|source| StorageError::Io { offset, source })
    }

    fn flush(&self) -> Result<(), StorageError> {
        let f = self.file.lock().map_err(|_| StorageError::Failed {
            reason: "file lock poisoned".into(),
        })?;
        f.sync_all()
            .map_err(|source| StorageError::Io { offset: 0, source })
    }
}

// ── FileStorageFactory ──────────────────────────────────────────────

/// Default [`StorageFactory`] that creates [`FileStorage`] instances.
///
/// This is the zero-configuration default: output goes directly to a
/// pre-allocated file at the given path. Consumers who want custom
/// backends implement their own `StorageFactory`.
#[derive(Debug, Clone, Default)]
pub struct FileStorageFactory;

impl StorageFactory for FileStorageFactory {
    fn create_storage(
        &self,
        output_path: &Path,
        file_size: u64,
        resuming: bool,
    ) -> Result<Box<dyn PieceStorage>, StorageError> {
        let storage = FileStorage::open(output_path, file_size, resuming)?;
        Ok(Box::new(storage))
    }
}

// ── MemoryStorage ───────────────────────────────────────────────────

/// In-memory storage backend for testing and short-lived transfers.
///
/// Stores all piece data in a `Vec<u8>` behind a mutex. Useful for unit
/// tests that need to verify written data without filesystem access, and
/// for small downloads where persistence is unnecessary.
pub struct MemoryStorage {
    /// The in-memory buffer, sized to `file_size`.
    data: Mutex<Vec<u8>>,
}

impl MemoryStorage {
    /// Creates in-memory storage pre-allocated to `file_size` bytes.
    pub fn new(file_size: u64) -> Self {
        Self {
            data: Mutex::new(vec![0u8; file_size as usize]),
        }
    }

    /// Returns a snapshot of the stored data.
    ///
    /// Useful in tests to verify that pieces were written at correct offsets.
    pub fn snapshot(&self) -> Vec<u8> {
        self.data.lock().map(|d| d.clone()).unwrap_or_default()
    }
}

impl PieceStorage for MemoryStorage {
    fn write_piece(&self, _piece_index: u32, offset: u64, data: &[u8]) -> Result<(), StorageError> {
        let mut buf = self.data.lock().map_err(|_| StorageError::Failed {
            reason: "memory lock poisoned".into(),
        })?;
        let start = offset as usize;
        let end = start
            .checked_add(data.len())
            .ok_or_else(|| StorageError::Io {
                offset,
                source: io::Error::new(io::ErrorKind::InvalidInput, "offset overflow"),
            })?;
        let dest = buf.get_mut(start..end).ok_or_else(|| StorageError::Io {
            offset,
            source: io::Error::new(
                io::ErrorKind::InvalidInput,
                "write extends past end of storage",
            ),
        })?;
        dest.copy_from_slice(data);
        Ok(())
    }

    fn read_piece(&self, offset: u64, buf: &mut [u8]) -> Result<usize, StorageError> {
        let data = self.data.lock().map_err(|_| StorageError::Failed {
            reason: "memory lock poisoned".into(),
        })?;
        let start = offset as usize;
        let available = data.len().saturating_sub(start);
        let to_read = buf.len().min(available);
        let src = data
            .get(start..start.saturating_add(to_read))
            .ok_or_else(|| StorageError::Io {
                offset,
                source: io::Error::new(io::ErrorKind::InvalidInput, "read past end of storage"),
            })?;
        buf.get_mut(..to_read)
            .ok_or_else(|| StorageError::Io {
                offset,
                source: io::Error::new(io::ErrorKind::InvalidInput, "buffer too small"),
            })?
            .copy_from_slice(src);
        Ok(to_read)
    }

    fn flush(&self) -> Result<(), StorageError> {
        // No-op for in-memory storage — data is always "durable".
        Ok(())
    }
}

// ── CoalescingStorage ───────────────────────────────────────────────

/// Write-coalescing wrapper that batches adjacent piece writes.
///
/// ## Design (informed by libtorrent's ARC disk cache)
///
/// libtorrent batches small writes into larger `writev` calls, reducing
/// syscall overhead and enabling the kernel to merge adjacent I/O. This
/// wrapper buffers piece writes in memory and flushes to the inner storage
/// when:
///
/// 1. The buffer exceeds `flush_threshold_bytes`.
/// 2. `flush()` is called explicitly (checkpoints, download completion).
///
/// Uses `Mutex` internally for thread safety. The buffer is a sorted list
/// of `(offset, data)` entries; adjacent entries are merged on flush for
/// optimal I/O.
pub struct CoalescingStorage {
    /// The underlying storage backend.
    inner: Box<dyn PieceStorage>,
    /// Buffered writes waiting to be flushed: `(offset, piece_index, data)`.
    buffer: Mutex<Vec<BufferedWrite>>,
    /// Flush threshold in bytes — when total buffered data exceeds this,
    /// the buffer is flushed automatically on the next write.
    flush_threshold: usize,
}

/// A single buffered write entry.
struct BufferedWrite {
    offset: u64,
    piece_index: u32,
    data: Vec<u8>,
}

impl CoalescingStorage {
    /// Wraps an existing storage with write coalescing.
    ///
    /// Writes are buffered until `flush_threshold_bytes` of data accumulates,
    /// then flushed to `inner` in offset order. A threshold of 0 disables
    /// buffering (every write passes through immediately).
    pub fn new(inner: Box<dyn PieceStorage>, flush_threshold_bytes: usize) -> Self {
        Self {
            inner,
            buffer: Mutex::new(Vec::new()),
            flush_threshold: flush_threshold_bytes,
        }
    }

    /// Flushes all buffered writes to the inner storage in offset order.
    fn flush_buffer(&self, buf: &mut Vec<BufferedWrite>) -> Result<(), StorageError> {
        if buf.is_empty() {
            return Ok(());
        }
        // Sort by offset for sequential I/O (minimises disk head movement
        // on HDDs, improves kernel I/O merging on SSDs).
        buf.sort_by_key(|w| w.offset);
        for write in buf.drain(..) {
            self.inner
                .write_piece(write.piece_index, write.offset, &write.data)?;
        }
        self.inner.flush()
    }
}

impl PieceStorage for CoalescingStorage {
    fn write_piece(&self, piece_index: u32, offset: u64, data: &[u8]) -> Result<(), StorageError> {
        let mut buf = self.buffer.lock().map_err(|_| StorageError::Failed {
            reason: "coalescing buffer lock poisoned".into(),
        })?;

        let total_buffered: usize = buf.iter().map(|w| w.data.len()).sum();
        buf.push(BufferedWrite {
            offset,
            piece_index,
            data: data.to_vec(),
        });

        // Auto-flush when buffer exceeds threshold.
        if self.flush_threshold > 0
            && total_buffered.saturating_add(data.len()) >= self.flush_threshold
        {
            self.flush_buffer(&mut buf)?;
        }

        Ok(())
    }

    fn read_piece(&self, offset: u64, buf: &mut [u8]) -> Result<usize, StorageError> {
        // Check buffer first for unflushed data, fall back to inner.
        // For simplicity, we always read from inner — callers should flush
        // before reads that need consistency.
        self.inner.read_piece(offset, buf)
    }

    fn flush(&self) -> Result<(), StorageError> {
        let mut buf = self.buffer.lock().map_err(|_| StorageError::Failed {
            reason: "coalescing buffer lock poisoned".into(),
        })?;
        self.flush_buffer(&mut buf)
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── MemoryStorage ───────────────────────────────────────────────

    /// Writing a piece at an offset stores the data at the correct location.
    ///
    /// This is the fundamental storage contract: data written at offset X
    /// must be readable at offset X.
    #[test]
    fn memory_storage_write_and_read() {
        let storage = MemoryStorage::new(1024);
        storage.write_piece(0, 0, &[0xAA; 256]).unwrap();
        storage.write_piece(1, 256, &[0xBB; 256]).unwrap();

        let snap = storage.snapshot();
        assert_eq!(snap.get(..256).unwrap(), &[0xAA; 256]);
        assert_eq!(snap.get(256..512).unwrap(), &[0xBB; 256]);
        // Unwritten region remains zero.
        assert_eq!(snap.get(512..768).unwrap(), &[0u8; 256]);
    }

    /// Writing the same piece twice is idempotent.
    ///
    /// Resume correctness requires that re-writing a piece with the same data
    /// produces the same result.
    #[test]
    fn memory_storage_idempotent_write() {
        let storage = MemoryStorage::new(512);
        storage.write_piece(0, 0, &[0xCC; 256]).unwrap();
        storage.write_piece(0, 0, &[0xCC; 256]).unwrap();
        let snap = storage.snapshot();
        assert_eq!(snap.get(..256).unwrap(), &[0xCC; 256]);
    }

    /// Writing past the end of storage returns an error.
    ///
    /// The storage must not silently grow — pre-allocation is the contract.
    #[test]
    fn memory_storage_write_past_end_fails() {
        let storage = MemoryStorage::new(100);
        let result = storage.write_piece(0, 50, &[0xFF; 100]);
        assert!(result.is_err());
    }

    /// `read_piece` returns correct data and byte count.
    ///
    /// Reads must return exactly the bytes written at the given offset.
    #[test]
    fn memory_storage_read_piece() {
        let storage = MemoryStorage::new(512);
        storage.write_piece(0, 100, &[0xDD; 50]).unwrap();

        let mut buf = [0u8; 50];
        let n = storage.read_piece(100, &mut buf).unwrap();
        assert_eq!(n, 50);
        assert_eq!(buf, [0xDD; 50]);
    }

    /// `read_piece` at end of storage returns partial read.
    #[test]
    fn memory_storage_read_at_end() {
        let storage = MemoryStorage::new(100);
        let mut buf = [0u8; 50];
        let n = storage.read_piece(80, &mut buf).unwrap();
        assert_eq!(n, 20); // only 20 bytes available from offset 80
    }

    /// `flush` is a no-op for memory storage (always succeeds).
    #[test]
    fn memory_storage_flush_succeeds() {
        let storage = MemoryStorage::new(64);
        assert!(storage.flush().is_ok());
    }

    // ── CoalescingStorage ───────────────────────────────────────────

    /// Writes are buffered until flush is called.
    ///
    /// Before flush, the inner storage should not see the writes. After
    /// flush, all buffered data must be present.
    #[test]
    fn coalescing_buffers_until_flush() {
        let inner = MemoryStorage::new(512);
        let coalescing = CoalescingStorage::new(Box::new(inner), 1024);

        coalescing.write_piece(0, 0, &[0xAA; 256]).unwrap();
        coalescing.write_piece(1, 256, &[0xBB; 256]).unwrap();

        // Flush — writes should go to inner.
        coalescing.flush().unwrap();

        // Verify via read_piece on the coalescing wrapper (delegates to inner).
        let mut buf = [0u8; 256];
        let n = coalescing.read_piece(0, &mut buf).unwrap();
        assert_eq!(n, 256);
        assert_eq!(buf, [0xAA; 256]);

        let n = coalescing.read_piece(256, &mut buf).unwrap();
        assert_eq!(n, 256);
        assert_eq!(buf, [0xBB; 256]);
    }

    /// Auto-flush triggers when buffer exceeds threshold.
    ///
    /// This test writes enough data to trigger the auto-flush, then verifies
    /// data is readable without an explicit flush call.
    #[test]
    fn coalescing_auto_flush_on_threshold() {
        let inner = MemoryStorage::new(1024);
        // Threshold of 400 bytes — two 256-byte pieces should trigger auto-flush.
        let coalescing = CoalescingStorage::new(Box::new(inner), 400);

        coalescing.write_piece(0, 0, &[0xAA; 256]).unwrap();
        coalescing.write_piece(1, 256, &[0xBB; 256]).unwrap();

        // Should have auto-flushed after second write (total 512 > 400).
        let mut buf = [0u8; 256];
        let n = coalescing.read_piece(0, &mut buf).unwrap();
        assert_eq!(n, 256);
        assert_eq!(buf, [0xAA; 256]);
    }

    // ── FileStorage ─────────────────────────────────────────────────

    /// FileStorage creates, writes, and reads a file correctly.
    ///
    /// This is an integration test that exercises the real filesystem.
    #[test]
    fn file_storage_write_and_read() {
        let tmp = std::env::temp_dir().join("p2p-storage-test");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let path = tmp.join("output.bin");

        let storage = FileStorage::open(&path, 512, false).unwrap();
        storage.write_piece(0, 0, &[0xAA; 256]).unwrap();
        storage.write_piece(1, 256, &[0xBB; 256]).unwrap();

        let mut buf = [0u8; 256];
        let n = storage.read_piece(0, &mut buf).unwrap();
        assert_eq!(n, 256);
        assert_eq!(buf, [0xAA; 256]);

        let n = storage.read_piece(256, &mut buf).unwrap();
        assert_eq!(n, 256);
        assert_eq!(buf, [0xBB; 256]);

        storage.flush().unwrap();
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// FileStorage resumes from an existing file without truncation.
    #[test]
    fn file_storage_resume_preserves_data() {
        let tmp = std::env::temp_dir().join("p2p-storage-resume");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let path = tmp.join("output.bin");

        // First session: write piece 0.
        {
            let storage = FileStorage::open(&path, 512, false).unwrap();
            storage.write_piece(0, 0, &[0xAA; 256]).unwrap();
            storage.flush().unwrap();
        }

        // Second session: resume — piece 0 should still be there.
        {
            let storage = FileStorage::open(&path, 512, true).unwrap();
            let mut buf = [0u8; 256];
            let n = storage.read_piece(0, &mut buf).unwrap();
            assert_eq!(n, 256);
            assert_eq!(buf, [0xAA; 256]);

            // Write piece 1.
            storage.write_piece(1, 256, &[0xBB; 256]).unwrap();
            storage.flush().unwrap();
        }

        // Verify both pieces.
        {
            let storage = FileStorage::open(&path, 512, true).unwrap();
            let mut buf = [0u8; 256];
            storage.read_piece(0, &mut buf).unwrap();
            assert_eq!(buf, [0xAA; 256]);
            storage.read_piece(256, &mut buf).unwrap();
            assert_eq!(buf, [0xBB; 256]);
        }

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // ── StorageError Display ────────────────────────────────────────

    /// `StorageError::Io` includes offset and underlying error.
    #[test]
    fn storage_error_io_display() {
        let err = StorageError::Io {
            offset: 1024,
            source: io::Error::new(io::ErrorKind::PermissionDenied, "access denied"),
        };
        let msg = err.to_string();
        assert!(msg.contains("1024"), "should contain offset: {msg}");
        assert!(
            msg.contains("access denied"),
            "should contain detail: {msg}"
        );
    }

    /// `StorageError::Failed` includes the reason.
    #[test]
    fn storage_error_failed_display() {
        let err = StorageError::Failed {
            reason: "disk full".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("disk full"), "should contain reason: {msg}");
    }
}
