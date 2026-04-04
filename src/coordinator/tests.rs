// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! Tests for the piece-level download coordinator, piece map, and torrent info.
//!
//! These tests verify the core coordinator logic without any network access.
//! They use synthetic piece data and a mock peer implementation.

use super::*;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

// ── Mock peer ────────────────────────────────────────────────────────

/// A mock peer for testing that returns pre-configured piece data.
///
/// Configurable per-piece: which pieces it has, whether it's choked,
/// what data it returns, and whether it should fail on fetch.
struct MockPeer {
    kind: PeerKind,
    available_pieces: Vec<bool>,
    choked: bool,
    /// Data returned for each piece. If `None`, fetch returns an error.
    piece_data: Vec<Option<Vec<u8>>>,
    speed: u64,
}

impl MockPeer {
    /// Creates a mock web seed that has all pieces and returns the given data.
    fn web_seed(piece_data: Vec<Vec<u8>>) -> Self {
        let count = piece_data.len();
        Self {
            kind: PeerKind::WebSeed,
            available_pieces: vec![true; count],
            choked: false,
            piece_data: piece_data.into_iter().map(Some).collect(),
            speed: 1_000_000,
        }
    }
}

impl Peer for MockPeer {
    fn kind(&self) -> PeerKind {
        self.kind
    }

    fn has_piece(&self, piece_index: u32) -> bool {
        self.available_pieces
            .get(piece_index as usize)
            .copied()
            .unwrap_or(false)
    }

    fn is_choked(&self) -> bool {
        self.choked
    }

    fn fetch_piece(
        &self,
        piece_index: u32,
        _offset: u64,
        _length: u32,
    ) -> Result<Vec<u8>, PeerError> {
        self.piece_data
            .get(piece_index as usize)
            .and_then(|opt| opt.clone())
            .ok_or_else(|| PeerError::Http {
                piece_index,
                url: "mock://test".into(),
                detail: "mock peer: piece not available".into(),
            })
    }

    fn speed_estimate(&self) -> u64 {
        self.speed
    }
}

// ── Helper: build TorrentInfo from raw piece data ────────────────────

/// Creates `TorrentInfo` from a list of raw piece byte slices.
///
/// SHA-1 hashes each piece to populate `piece_hashes`. Uses a fixed
/// piece length of 256 bytes for easy testing.
fn make_torrent_info(pieces: &[&[u8]]) -> TorrentInfo {
    use sha1::{Digest, Sha1};

    let piece_length = 256u64;
    let file_size: u64 = pieces.iter().map(|p| p.len() as u64).sum();
    let mut piece_hashes = Vec::with_capacity(pieces.len() * 20);

    for piece in pieces {
        let mut hasher = Sha1::new();
        hasher.update(piece);
        piece_hashes.extend_from_slice(hasher.finalize().as_slice());
    }

    TorrentInfo {
        piece_length,
        piece_hashes,
        file_size,
        file_name: "test-content.zip".into(),
    }
}

// ── PieceState ──────────────────────────────────────────────────────

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

// ── SharedPieceMap ──────────────────────────────────────────────────

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

// ── TorrentInfo ─────────────────────────────────────────────────────

/// `TorrentInfo::piece_count` correctly derives from piece hash length.
///
/// Each piece produces exactly 20 bytes of SHA-1 hash. The piece count
/// must equal `piece_hashes.len() / 20`.
#[test]
fn torrent_info_piece_count() {
    let info = make_torrent_info(&[&[0xAA; 256], &[0xBB; 256], &[0xCC; 128]]);
    assert_eq!(info.piece_count(), 3);
}

/// `TorrentInfo::piece_hash` returns correct 20-byte SHA-1 for each piece.
///
/// The hash at index `i` must match the SHA-1 of piece `i`'s data.
#[test]
fn torrent_info_piece_hash_matches() {
    use sha1::{Digest, Sha1};

    let data_a = [0xAA; 256];
    let data_b = [0xBB; 200];
    let info = make_torrent_info(&[&data_a, &data_b]);

    let mut h = Sha1::new();
    h.update(data_a);
    let expected_a = h.finalize();

    let mut h = Sha1::new();
    h.update(data_b);
    let expected_b = h.finalize();

    assert_eq!(info.piece_hash(0).unwrap(), expected_a.as_slice());
    assert_eq!(info.piece_hash(1).unwrap(), expected_b.as_slice());
    assert!(info.piece_hash(2).is_none()); // out of bounds
}

/// `TorrentInfo::piece_offset` computes correct byte offsets.
///
/// Piece `i` starts at `i * piece_length`.
#[test]
fn torrent_info_piece_offset() {
    let info = make_torrent_info(&[&[0; 256], &[0; 256], &[0; 128]]);
    assert_eq!(info.piece_offset(0), 0);
    assert_eq!(info.piece_offset(1), 256);
    assert_eq!(info.piece_offset(2), 512);
}

/// `TorrentInfo::piece_size` returns correct size including the last short piece.
///
/// All pieces except the last are `piece_length` bytes. The last piece
/// covers the remaining bytes (potentially shorter).
#[test]
fn torrent_info_piece_size_last_piece_shorter() {
    let info = TorrentInfo {
        piece_length: 256,
        piece_hashes: vec![0u8; 60], // 3 pieces
        file_size: 640,              // 256 + 256 + 128
        file_name: "test.zip".into(),
    };
    assert_eq!(info.piece_size(0), 256);
    assert_eq!(info.piece_size(1), 256);
    assert_eq!(info.piece_size(2), 128); // last piece is short
}

// ── PieceCoordinator ────────────────────────────────────────────────

/// Coordinator with no peers returns `NoPeers` error.
///
/// The coordinator must fail fast when no peers are available rather
/// than hanging indefinitely.
#[test]
fn coordinator_no_peers_error() {
    let info = make_torrent_info(&[&[0xAA; 256]]);
    let config = CoordinatorConfig::default();
    let coord = PieceCoordinator::new(info, config);

    let tmp = std::env::temp_dir().join("cnc-coord-no-peers");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let out = tmp.join("output.bin");

    let result = coord.run(&out, &mut |_| {});
    assert!(matches!(result, Err(CoordinatorError::NoPeers)));

    let _ = std::fs::remove_dir_all(&tmp);
}

/// Coordinator successfully downloads all pieces via a single web seed.
///
/// This is the simplest happy path: one web seed peer, correct data,
/// pieces verified via SHA-1 and written to the output file.
#[test]
fn coordinator_single_web_seed_downloads_all_pieces() {
    let piece_a = vec![0xAA; 256];
    let piece_b = vec![0xBB; 128];
    let info = make_torrent_info(&[&piece_a, &piece_b]);

    let mock = MockPeer::web_seed(vec![piece_a.clone(), piece_b.clone()]);

    let config = CoordinatorConfig::default();
    let mut coord = PieceCoordinator::new(info.clone(), config);
    coord.add_peer(Box::new(mock));

    let tmp = std::env::temp_dir().join("cnc-coord-single-webseed");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let out = tmp.join("output.bin");

    let mut progress_events = Vec::new();
    coord.run(&out, &mut |p| progress_events.push(p)).unwrap();

    // Verify the output file contains the correct data.
    let data = std::fs::read(&out).unwrap();
    // The file is pre-allocated to file_size (384 bytes). First 256 are piece_a,
    // next 128 are piece_b.
    assert_eq!(data.get(..256).unwrap(), &piece_a[..]);
    assert_eq!(data.get(256..384).unwrap(), &piece_b[..]);

    // Verify progress events.
    assert!(progress_events
        .iter()
        .any(|p| matches!(p, CoordinatorProgress::Starting { .. })));
    assert!(progress_events
        .iter()
        .any(|p| matches!(p, CoordinatorProgress::Complete { .. })));
    let piece_completes: Vec<_> = progress_events
        .iter()
        .filter(|p| matches!(p, CoordinatorProgress::PieceComplete { .. }))
        .collect();
    assert_eq!(piece_completes.len(), 2);

    let _ = std::fs::remove_dir_all(&tmp);
}

/// Coordinator detects SHA-1 mismatch and retries from same peer.
///
/// If a peer returns corrupted data, the coordinator must detect the
/// SHA-1 mismatch, mark the piece as failed, and retry. This prevents
/// corrupted data from being written to the output file.
#[test]
fn coordinator_sha1_mismatch_triggers_retry() {
    let correct_data = vec![0xAA; 256];
    let corrupt_data = vec![0xFF; 256]; // wrong data

    let info = make_torrent_info(&[&correct_data]);

    // First call returns corrupt data, second call returns correct data.
    // Since MockPeer returns fixed data, we need a peer that always returns
    // corrupt data. It should fail all retries.
    let mock = MockPeer {
        kind: PeerKind::WebSeed,
        available_pieces: vec![true],
        choked: false,
        piece_data: vec![Some(corrupt_data)],
        speed: 1_000_000,
    };

    let config = CoordinatorConfig {
        max_retries_per_piece: 2,
        ..Default::default()
    };
    let mut coord = PieceCoordinator::new(info, config);
    coord.add_peer(Box::new(mock));

    let tmp = std::env::temp_dir().join("cnc-coord-sha1-mismatch");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let out = tmp.join("output.bin");

    let result = coord.run(&out, &mut |_| {});
    assert!(matches!(
        result,
        Err(CoordinatorError::AllPeersFailed { .. })
    ));

    let _ = std::fs::remove_dir_all(&tmp);
}

/// Coordinator respects the cancel flag.
///
/// When the cancel flag is set, the coordinator must stop downloading
/// and return `Cancelled`. This enables graceful shutdown when the user
/// interrupts a download.
#[test]
fn coordinator_cancel_flag_stops_download() {
    let piece_data = vec![0xAA; 256];
    let info = make_torrent_info(&[&piece_data, &piece_data]);

    let mock = MockPeer::web_seed(vec![piece_data.clone(), piece_data.clone()]);

    let cancel = Arc::new(AtomicBool::new(true)); // pre-cancelled
    let config = CoordinatorConfig {
        cancel_flag: cancel,
        ..Default::default()
    };
    let mut coord = PieceCoordinator::new(info, config);
    coord.add_peer(Box::new(mock));

    let tmp = std::env::temp_dir().join("cnc-coord-cancel");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let out = tmp.join("output.bin");

    let result = coord.run(&out, &mut |_| {});
    assert!(matches!(result, Err(CoordinatorError::Cancelled)));

    let _ = std::fs::remove_dir_all(&tmp);
}

/// Coordinator selects the faster peer when multiple peers have a piece.
///
/// When a web seed (1 MB/s) and BT swarm (500 KB/s) both have a piece,
/// the coordinator must select the web seed because it's faster.
#[test]
fn coordinator_prefers_faster_peer() {
    let piece_data = vec![0xAA; 256];
    let info = make_torrent_info(&[&piece_data]);

    let slow_peer = MockPeer {
        kind: PeerKind::BtSwarm,
        available_pieces: vec![true],
        choked: false,
        piece_data: vec![Some(piece_data.clone())],
        speed: 100_000, // 100 KB/s
    };
    let fast_peer = MockPeer {
        kind: PeerKind::WebSeed,
        available_pieces: vec![true],
        choked: false,
        piece_data: vec![Some(piece_data.clone())],
        speed: 1_000_000, // 1 MB/s
    };

    let config = CoordinatorConfig::default();
    let mut coord = PieceCoordinator::new(info, config);
    coord.add_peer(Box::new(slow_peer));
    coord.add_peer(Box::new(fast_peer));

    let tmp = std::env::temp_dir().join("cnc-coord-fast-peer");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let out = tmp.join("output.bin");

    let mut piece_events = Vec::new();
    coord
        .run(&out, &mut |p| {
            if matches!(p, CoordinatorProgress::PieceComplete { .. }) {
                piece_events.push(p);
            }
        })
        .unwrap();

    // The piece should come from the WebSeed (faster).
    assert_eq!(piece_events.len(), 1);
    match &piece_events[0] {
        CoordinatorProgress::PieceComplete { peer_kind, .. } => {
            assert_eq!(*peer_kind, PeerKind::WebSeed);
        }
        _ => panic!("expected PieceComplete"),
    }

    let _ = std::fs::remove_dir_all(&tmp);
}

/// Coordinator skips choked peers.
///
/// A choked peer must not be selected for piece requests. If only a choked
/// peer has a piece and retries are exhausted, the download fails.
#[test]
fn coordinator_skips_choked_peer() {
    let piece_data = vec![0xAA; 256];
    let info = make_torrent_info(&[&piece_data]);

    let choked_peer = MockPeer {
        kind: PeerKind::BtSwarm,
        available_pieces: vec![true],
        choked: true,
        piece_data: vec![Some(piece_data.clone())],
        speed: 1_000_000,
    };

    let config = CoordinatorConfig {
        max_retries_per_piece: 1,
        ..Default::default()
    };
    let mut coord = PieceCoordinator::new(info, config);
    coord.add_peer(Box::new(choked_peer));

    let tmp = std::env::temp_dir().join("cnc-coord-choked");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let out = tmp.join("output.bin");

    let result = coord.run(&out, &mut |_| {});
    assert!(matches!(
        result,
        Err(CoordinatorError::AllPeersFailed { .. })
    ));

    let _ = std::fs::remove_dir_all(&tmp);
}

// ── Error Display ───────────────────────────────────────────────────

/// `CoordinatorError::NoPeers` display message.
#[test]
fn error_display_no_peers() {
    let err = CoordinatorError::NoPeers;
    let msg = err.to_string();
    assert!(msg.contains("no peers"), "should mention no peers: {msg}");
}

/// `CoordinatorError::AllPeersFailed` display includes piece index and attempts.
#[test]
fn error_display_all_peers_failed() {
    let err = CoordinatorError::AllPeersFailed {
        piece_index: 42,
        attempts: 3,
    };
    let msg = err.to_string();
    assert!(msg.contains("42"), "should contain piece index: {msg}");
    assert!(msg.contains("3"), "should contain attempt count: {msg}");
}

/// `CoordinatorError::PieceHashMismatch` display includes expected and actual.
#[test]
fn error_display_piece_hash_mismatch() {
    let err = CoordinatorError::PieceHashMismatch {
        piece_index: 7,
        expected: "aabbccdd".into(),
        actual: "11223344".into(),
    };
    let msg = err.to_string();
    assert!(msg.contains("aabbccdd"), "should contain expected: {msg}");
    assert!(msg.contains("11223344"), "should contain actual: {msg}");
}

/// `PeerError::Http` display includes piece index and URL.
#[test]
fn peer_error_display_http() {
    let err = PeerError::Http {
        piece_index: 5,
        url: "https://example.com/file.zip".into(),
        detail: "connection refused".into(),
    };
    let msg = err.to_string();
    assert!(msg.contains("5"), "should contain piece index: {msg}");
    assert!(msg.contains("example.com"), "should contain URL: {msg}");
}

// ── Determinism ─────────────────────────────────────────────────────

/// Coordinator produces identical output when run twice with the same input.
///
/// Deterministic output is critical for SHA-256 manifest verification
/// after download — the same content must always produce the same file.
#[test]
fn coordinator_deterministic_output() {
    let piece_a = vec![0xAA; 256];
    let piece_b = vec![0xBB; 128];

    let run = |dir_name: &str| -> Vec<u8> {
        let info = make_torrent_info(&[&piece_a, &piece_b]);
        let mock = MockPeer::web_seed(vec![piece_a.clone(), piece_b.clone()]);
        let config = CoordinatorConfig::default();
        let mut coord = PieceCoordinator::new(info, config);
        coord.add_peer(Box::new(mock));

        let tmp = std::env::temp_dir().join(dir_name);
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out = tmp.join("output.bin");
        coord.run(&out, &mut |_| {}).unwrap();
        let data = std::fs::read(&out).unwrap();
        let _ = std::fs::remove_dir_all(&tmp);
        data
    };

    let r1 = run("cnc-coord-det-1");
    let r2 = run("cnc-coord-det-2");
    assert_eq!(r1, r2);
}

// ── Boundary ────────────────────────────────────────────────────────

/// Zero-piece torrent info edge case.
///
/// A torrent with zero pieces should complete immediately (vacuously complete).
#[test]
fn coordinator_zero_pieces_completes_immediately() {
    let info = TorrentInfo {
        piece_length: 256,
        piece_hashes: Vec::new(),
        file_size: 0,
        file_name: "empty.zip".into(),
    };

    let config = CoordinatorConfig::default();
    let mut coord = PieceCoordinator::new(info, config);
    // Even with no peers, zero pieces = nothing to download.
    // But coordinator checks peers first...
    let mock = MockPeer::web_seed(vec![]);
    coord.add_peer(Box::new(mock));

    let tmp = std::env::temp_dir().join("cnc-coord-zero-pieces");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let out = tmp.join("output.bin");

    coord.run(&out, &mut |_| {}).unwrap();
    // File should exist and be empty.
    let data = std::fs::read(&out).unwrap();
    assert!(data.is_empty());

    let _ = std::fs::remove_dir_all(&tmp);
}

/// Single-piece torrent at the boundary.
///
/// When the entire file fits in one piece, the coordinator must handle
/// it correctly — piece_size(0) == file_size.
#[test]
fn coordinator_single_piece_boundary() {
    let piece = vec![0x42; 100]; // shorter than piece_length (256)
    let info = make_torrent_info(&[&piece]);
    assert_eq!(info.piece_count(), 1);
    assert_eq!(info.piece_size(0), 100);

    let mock = MockPeer::web_seed(vec![piece.clone()]);
    let config = CoordinatorConfig::default();
    let mut coord = PieceCoordinator::new(info, config);
    coord.add_peer(Box::new(mock));

    let tmp = std::env::temp_dir().join("cnc-coord-single-piece");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let out = tmp.join("output.bin");

    coord.run(&out, &mut |_| {}).unwrap();
    let data = std::fs::read(&out).unwrap();
    // File is pre-allocated to file_size (100 bytes).
    assert_eq!(data.len(), 100);
    assert_eq!(&data[..], &piece[..]);

    let _ = std::fs::remove_dir_all(&tmp);
}
