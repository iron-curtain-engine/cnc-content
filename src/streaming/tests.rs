//! Unit tests for byte-range tracking, piece mapping, prebuffer policy,
//! and `StreamingReader` I/O.
//!
//! Tests are purely in-memory or use temporary files — no network access
//! or torrent client required.

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

    let (mut reader, notifier) = StreamingReader::new_streaming(&path, range_map, policy).unwrap();

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

    let (mut reader, notifier) = StreamingReader::new_streaming(&path, range_map, policy).unwrap();

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
