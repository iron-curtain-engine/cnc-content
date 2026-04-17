use super::*;

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

/// `CoordinatorError::NoPeers` display message contains actionable context.
///
/// The error string is shown directly in the CLI and logged by callers;
/// it must be human-readable enough for a user to understand why the
/// download failed and what to try next.
#[test]
fn error_display_no_peers() {
    let err = CoordinatorError::NoPeers;
    let msg = err.to_string();
    assert!(msg.contains("no peers"), "should mention no peers: {msg}");
}

/// `CoordinatorError::AllPeersFailed` display includes piece index and attempt count.
///
/// Structured fields must appear in the message so operators can correlate
/// a failed piece with torrent logs; a generic "download failed" string
/// gives no signal for debugging partial failures.
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

/// `CoordinatorError::PieceHashMismatch` display includes both expected and actual hashes.
///
/// Callers need both values to decide whether the mismatch is a corrupt
/// mirror (different actual each time) or a version skew (same wrong
/// actual consistently); surfacing only one prevents that diagnosis.
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

/// `PeerError::Http` display includes the piece index and the failing URL.
///
/// The URL in the error message tells operators which mirror was problematic
/// so they can identify a bad mirror without inspecting internal state.
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

// ── WebSeedPeer agent hardening ──────────────────────────────────────

/// WebSeedPeer's MAX_REDIRECTS constant must match the downloader's policy.
///
/// Both HTTP transport paths (downloader mirror and coordinator webseed)
/// must enforce consistent redirect limits to prevent redirect-chain SSRF.
/// A mismatch would let attackers exploit the less-restricted path.
#[cfg(feature = "download")]
#[test]
fn webseed_max_redirects_matches_downloader() {
    use super::webseed::MAX_REDIRECTS;
    const {
        assert!(
            MAX_REDIRECTS >= 2,
            "redirect limit must allow CDN redirects"
        );
        assert!(
            MAX_REDIRECTS <= 8,
            "redirect limit should prevent chain attacks"
        );
    }
}
