use super::*;

/// Helper: create a temporary content root with some test files.
fn setup_content_dir(name: &str) -> (PathBuf, ContentSession) {
    let tmp = std::env::temp_dir().join(format!("cnc-session-{name}"));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();

    // Create test files simulating installed content.
    std::fs::write(tmp.join("conquer.mix"), b"mix data").unwrap();
    std::fs::create_dir_all(tmp.join("movies/allied")).unwrap();
    std::fs::write(tmp.join("movies/allied/intro.vqa"), b"VQA video data").unwrap();

    let session =
        ContentSession::open_with_config(GameId::RedAlert, tmp.clone(), Config::default()).unwrap();

    (tmp, session)
}

fn teardown(tmp: &Path) {
    let _ = std::fs::remove_dir_all(tmp);
}

// ── Path traversal security tests ───────────────────────────────

/// `content_file_path` must reject a path that escapes the content root via `..` components.
///
/// Path traversal is the primary security boundary of the session API; a caller
/// (or an attacker supplying a crafted relative path) must never be able to read
/// arbitrary files outside the managed content directory.
#[test]
fn content_file_path_rejects_parent_traversal() {
    let (tmp, session) = setup_content_dir("traversal-parent");
    let result = session.content_file_path("../../../etc/passwd");
    assert!(result.is_err(), "parent traversal must be rejected");
    teardown(&tmp);
}

/// `content_file_path` must reject traversal hidden inside an otherwise valid-looking path.
///
/// Paths like `movies/../../../etc/passwd` begin with a legitimate subdirectory
/// name before escaping; the security check must normalize and validate the full
/// resolved path, not just the first component.
#[test]
fn content_file_path_rejects_embedded_parent_traversal() {
    let (tmp, session) = setup_content_dir("traversal-embedded");
    let result = session.content_file_path("movies/../../../etc/passwd");
    assert!(result.is_err());
    teardown(&tmp);
}

/// `content_file_path` must reject an absolute path that bypasses the content root entirely.
///
/// An absolute path like `/etc/passwd` does not traverse relative to the content
/// root at all; accepting it would allow unrestricted read access to the host
/// filesystem regardless of where the content root is located.
#[test]
fn content_file_path_rejects_absolute_path() {
    let (tmp, session) = setup_content_dir("traversal-absolute");
    // Unix absolute.
    let result = session.content_file_path("/etc/passwd");
    assert!(result.is_err());
    teardown(&tmp);
}

/// `content_file_path` must reject a Windows-style absolute path on Windows hosts.
///
/// Paths like `C:\Windows\System32\cmd.exe` are absolute on Windows even though
/// they look relative on Unix; the path-boundary check must handle the Windows
/// drive-letter prefix so the content root restriction holds on all platforms.
#[cfg(target_os = "windows")]
#[test]
fn content_file_path_rejects_windows_absolute_path() {
    let (tmp, session) = setup_content_dir("traversal-win-abs");
    let result = session.content_file_path("C:\\Windows\\System32\\cmd.exe");
    assert!(result.is_err());
    teardown(&tmp);
}

/// `content_file_path` must reject backslash-encoded traversal sequences.
///
/// On Windows, `..\\` is a valid parent-traversal separator; on Unix it may be
/// treated as a literal filename component or normalized away. Either way the
/// security boundary must hold: the resolved path must remain inside the content root.
#[test]
fn content_file_path_rejects_backslash_traversal() {
    let (tmp, session) = setup_content_dir("traversal-backslash");
    let result = session.content_file_path("..\\..\\..\\etc\\passwd");
    assert!(result.is_err());
    teardown(&tmp);
}

/// `open_content` must reject a traversal path before attempting to open the file.
///
/// The path-boundary check must run before the `File::open` call; if the check
/// were skipped or ordered incorrectly, the OS would open the file and the
/// error would be silenced, leaking the file contents to the caller.
#[test]
fn open_content_rejects_parent_traversal() {
    let (tmp, session) = setup_content_dir("open-traversal");
    let result = session.open_content("../../../etc/passwd");
    assert!(result.is_err());
    teardown(&tmp);
}

/// `open_stream` must reject a traversal path with the same strictness as `content_file_path`.
///
/// All three path-resolving methods share the same security requirement; testing
/// each one independently ensures that a future refactor cannot accidentally
/// omit the boundary check from one of them.
#[test]
fn open_stream_rejects_parent_traversal() {
    let (tmp, session) = setup_content_dir("stream-traversal");
    let result = session.open_stream("../../../etc/passwd");
    assert!(result.is_err());
    teardown(&tmp);
}

/// `open_stream_with_policy` must reject a traversal path regardless of the buffer policy.
///
/// The policy parameter controls buffering, not access control; the path-boundary
/// check must fire before any policy-dependent logic so that varying the policy
/// cannot be used to bypass the security boundary.
#[test]
fn open_stream_with_policy_rejects_parent_traversal() {
    let (tmp, session) = setup_content_dir("stream-policy-traversal");
    let result = session.open_stream_with_policy(
        "../../../etc/passwd",
        crate::streaming::BufferPolicy::default(),
    );
    assert!(result.is_err());
    teardown(&tmp);
}

// ── Valid path access ───────────────────────────────────────────

/// `content_file_path` must return `Some(path)` for a file that exists in the content root.
///
/// This is the primary happy-path contract: a game engine asking for a known
/// installed file must receive a usable absolute path, not `None` or an error.
#[test]
fn content_file_path_returns_existing_file() {
    let (tmp, session) = setup_content_dir("valid-file");
    let result = session.content_file_path("conquer.mix").unwrap();
    assert!(result.is_some());
    assert!(result.unwrap().ends_with("conquer.mix"));
    teardown(&tmp);
}

/// `content_file_path` must return `Ok(None)` for a valid path that does not exist on disk.
///
/// Returning `None` (rather than an error) is the contract for "file not installed yet";
/// callers use this to decide whether to trigger a download rather than treating the
/// absence as a fatal error.
#[test]
fn content_file_path_returns_none_for_missing() {
    let (tmp, session) = setup_content_dir("valid-missing");
    let result = session.content_file_path("nonexistent.mix").unwrap();
    assert!(result.is_none());
    teardown(&tmp);
}

/// `content_file_path` must resolve paths into subdirectories of the content root.
///
/// Game content is organized into subdirectories (e.g. `movies/allied/`); a path
/// that contains legitimate forward slashes must succeed as long as it stays within
/// the content root boundary.
#[test]
fn content_file_path_allows_subdirectory_access() {
    let (tmp, session) = setup_content_dir("valid-subdir");
    let result = session
        .content_file_path("movies/allied/intro.vqa")
        .unwrap();
    assert!(result.is_some());
    teardown(&tmp);
}

/// `open_content` must return a readable `ContentReader` whose bytes match the file on disk.
///
/// The `ContentReader` is the engine's direct handle for loading game data; if
/// the bytes were truncated, buffered incorrectly, or the reported size were wrong,
/// the engine would produce corrupted audio, graphics, or game state.
#[test]
fn open_content_reads_file() {
    let (tmp, session) = setup_content_dir("open-read");
    let mut reader = session.open_content("conquer.mix").unwrap();
    let mut buf = Vec::new();
    std::io::Read::read_to_end(&mut reader, &mut buf).unwrap();
    assert_eq!(buf, b"mix data");
    assert_eq!(reader.size(), 8);
    teardown(&tmp);
}

/// `open_content` must return an error when the requested file does not exist.
///
/// Unlike `content_file_path`, `open_content` promises an open file handle; when
/// the file is absent there is nothing to return, so an `Io` error is the correct
/// signal for the caller to handle the missing-content case.
#[test]
fn open_content_missing_file_returns_io_error() {
    let (tmp, session) = setup_content_dir("open-missing");
    let result = session.open_content("nonexistent.mix");
    assert!(result.is_err());
    teardown(&tmp);
}

/// `open_stream` must return a `StreamingReader` whose bytes match the file on disk.
///
/// For fully-downloaded files the streaming reader must behave identically to a
/// plain file read; any difference would cause FMV cut scenes or audio to corrupt
/// silently when played back through the streaming path.
#[test]
fn open_stream_reads_file() {
    let (tmp, session) = setup_content_dir("stream-read");
    let mut reader = session.open_stream("conquer.mix").unwrap();
    let mut buf = Vec::new();
    std::io::Read::read_to_end(&mut reader, &mut buf).unwrap();
    assert_eq!(buf, b"mix data");
    teardown(&tmp);
}

// ── Session lifecycle ───────────────────────────────────────────

/// `open_with_root` must create the content root directory, including any missing parents.
///
/// Downstream crates pass a fresh path that may not exist yet; if the directory
/// were not created, every subsequent file operation would fail with a "not found"
/// error before content is ever downloaded.
#[test]
fn session_open_creates_content_root() {
    let tmp = std::env::temp_dir().join("cnc-session-open");
    let _ = std::fs::remove_dir_all(&tmp);
    let nested = tmp.join("deep/nested/path");

    let session = ContentSession::open_with_root(GameId::RedAlert, nested.clone()).unwrap();
    assert!(nested.exists());
    assert_eq!(session.game(), GameId::RedAlert);
    assert_eq!(session.content_root(), &nested);

    teardown(&tmp);
}

/// `game()` and `content_root()` must return the values the session was opened with.
///
/// These accessors are used by downstream crates to route file requests and
/// display status; returning a different game ID or path would silently misdirect
/// all content operations.
#[test]
fn session_game_and_content_root() {
    let (tmp, session) = setup_content_dir("game-root");
    assert_eq!(session.game(), GameId::RedAlert);
    assert_eq!(session.content_root(), &tmp);
    teardown(&tmp);
}

/// A freshly opened session must default to `PauseDuringOnlinePlay` as the seeding policy.
///
/// The default must be conservative — seeding should not interfere with online
/// multiplayer out of the box. A wrong default would either seed aggressively
/// (consuming bandwidth during matches) or never seed at all (hurting distribution).
#[test]
fn session_seeding_policy_default() {
    let (tmp, session) = setup_content_dir("seed-policy");
    assert_eq!(
        session.seeding_policy(),
        SeedingPolicy::PauseDuringOnlinePlay
    );
    teardown(&tmp);
}

/// `set_seeding_policy` must update the policy so that `seeding_policy` reflects the new value.
///
/// The policy is the primary user-facing bandwidth control; if the setter did not
/// actually update the stored value, user preference changes would be silently lost
/// and the wrong seeding behavior would persist.
#[test]
fn session_set_seeding_policy() {
    let (tmp, mut session) = setup_content_dir("seed-change");
    session.set_seeding_policy(SeedingPolicy::SeedAlways);
    assert_eq!(session.seeding_policy(), SeedingPolicy::SeedAlways);
    teardown(&tmp);
}

/// `installed_files` must enumerate files from the content root that match known package test files.
///
/// The method is used to report installation status and build manifests; a file
/// that exists on disk but is not returned here would be invisible to the verify
/// and repair paths.
#[test]
fn session_installed_files() {
    let (tmp, session) = setup_content_dir("installed");
    let files = session.installed_files();
    // setup_content_dir creates conquer.mix, which is a known RA test file,
    // so installed_files should detect it.
    assert!(!files.is_empty());
    assert!(files.iter().any(|(name, _)| name.contains("conquer.mix")));
    teardown(&tmp);
}

/// `is_content_complete` must return `false` when the content root contains no game files.
///
/// An empty directory means nothing has been installed; returning `true` here
/// would suppress the download prompt and leave the user with a broken install.
#[test]
fn session_is_content_complete_false_for_empty() {
    let tmp = std::env::temp_dir().join("cnc-session-complete");
    let _ = std::fs::remove_dir_all(&tmp);
    let session = ContentSession::open_with_root(GameId::RedAlert, tmp.clone()).unwrap();
    assert!(!session.is_content_complete());
    teardown(&tmp);
}

/// Both `missing_required_packages` and `missing_packages` must be non-empty for a bare content root.
///
/// These lists drive the download queue; if either returned empty for a fresh
/// install, the engine would silently skip downloading required content and fail
/// to launch.
#[test]
fn session_missing_packages_non_empty_for_empty_root() {
    let tmp = std::env::temp_dir().join("cnc-session-missing");
    let _ = std::fs::remove_dir_all(&tmp);
    let session = ContentSession::open_with_root(GameId::RedAlert, tmp.clone()).unwrap();
    assert!(!session.missing_required_packages().is_empty());
    assert!(!session.missing_packages().is_empty());
    teardown(&tmp);
}

/// `verify` must return an empty failure list when no manifest file is present.
///
/// The manifest is written only after a successful install; on a fresh or
/// pre-manifest install there is nothing to verify, so returning empty (rather
/// than an error or a list of all files) is the correct no-op behavior.
#[test]
fn session_verify_without_manifest_returns_empty() {
    let (tmp, session) = setup_content_dir("verify-no-manifest");
    let failures = session.verify();
    assert!(failures.is_empty());
    teardown(&tmp);
}

/// `shutdown` must complete without panicking even when no torrent session is active.
///
/// Shutdown is called on every normal exit path; a panic here would bypass any
/// remaining cleanup (config save, torrent graceful stop) and could leave the
/// config file in a corrupt state.
#[test]
fn session_shutdown_does_not_panic() {
    let (tmp, session) = setup_content_dir("shutdown");
    session.shutdown(); // should not panic
    teardown(&tmp);
}

// ── Error display ───────────────────────────────────────────────

/// `SessionError::PathTraversal` display must include the offending path string.
///
/// Security errors are logged and surfaced to users; the path is the only
/// diagnostic detail available, so it must appear verbatim in the formatted
/// message so the user (or log analysis) can identify the attempted traversal.
#[test]
fn session_error_display_path_traversal() {
    let err = SessionError::PathTraversal("../../../etc/passwd".into());
    let msg = err.to_string();
    assert!(msg.contains("../../../etc/passwd"));
}

/// `SessionError::NotFreeware` display must include both the game title and the word "freeware".
///
/// The error is shown to users who attempt to auto-download a non-freeware title;
/// the message must name the game so the user knows which title triggered the
/// restriction, and must mention "freeware" so the reason is clear.
#[test]
fn session_error_display_not_freeware() {
    let err = SessionError::NotFreeware("Dune 2".into());
    let msg = err.to_string();
    assert!(msg.contains("Dune 2"));
    assert!(msg.contains("freeware"));
}
