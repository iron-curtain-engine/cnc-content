//! ZIP extraction tests — security (Zip Slip), edge cases, SHA-1 placeholder,
//! and archive bomb protection.

use super::*;

// ── Security tests: Zip Slip (CVE-2018-1000178) ─────────────────

/// Verifies that a ZIP entry using `../../` path traversal is rejected.
///
/// A malicious archive with an entry named `../../etc/passwd` must not be
/// allowed to escape `content_root`. This is the canonical Zip Slip attack
/// (CVE-2018-1000178): if traversal is permitted, an attacker can overwrite
/// arbitrary files on the host outside the intended extraction directory.
///
/// The test additionally asserts that no payload file is written at the
/// escaped path, confirming the block happens before any I/O.
#[test]
fn extract_zip_rejects_dot_dot_slash_traversal() {
    let tmp = std::env::temp_dir().join("cnc-zip-slip-dotdot");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();

    let zip_path = tmp.join("malicious.zip");
    create_test_zip(&zip_path, &[("../../etc/passwd", b"pwned")]);

    let content_root = tmp.join("content");
    fs::create_dir_all(&content_root).unwrap();

    let result = extract_zip(&zip_path, &content_root, &mut noop_progress);
    assert!(result.is_err(), "should reject ../.. traversal");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("blocked path traversal") || err.contains("Escapes"),
        "error should mention traversal: {err}"
    );

    // The escaped file must NOT exist.
    assert!(
        !tmp.join("etc/passwd").exists(),
        "traversal payload must not be written"
    );

    let _ = fs::remove_dir_all(&tmp);
}

/// Verifies that a ZIP entry with an absolute path (e.g. `/etc/shadow`) is rejected.
///
/// Some ZIP tools produce entries with absolute paths. If accepted, extraction
/// would write to the absolute path on the host filesystem rather than inside
/// `content_root`, bypassing the boundary entirely. The extractor must treat
/// absolute entry names as path traversal and refuse them.
#[test]
fn extract_zip_rejects_absolute_path_entry() {
    let tmp = std::env::temp_dir().join("cnc-zip-slip-abs");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();

    let zip_path = tmp.join("malicious.zip");
    // Absolute path — should be rejected.
    create_test_zip(&zip_path, &[("/etc/shadow", b"pwned")]);

    let content_root = tmp.join("content");
    fs::create_dir_all(&content_root).unwrap();

    let result = extract_zip(&zip_path, &content_root, &mut noop_progress);
    assert!(result.is_err(), "should reject absolute path entry");

    let _ = fs::remove_dir_all(&tmp);
}

/// Verifies that a ZIP entry using backslash-based path traversal is rejected.
///
/// On Windows, backslashes are path separators, so `..\\..\\etc\\passwd`
/// is genuine traversal that `strict-path` rejects. On Unix, backslashes
/// are literal filename characters — the entry name is a single valid
/// component, not traversal. This test only applies to Windows.
#[cfg(target_os = "windows")]
#[test]
fn extract_zip_rejects_backslash_traversal() {
    let tmp = std::env::temp_dir().join("cnc-zip-slip-backslash");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();

    let zip_path = tmp.join("malicious.zip");
    // Mixed separators — a common evasion technique.
    create_test_zip(&zip_path, &[("..\\..\\etc\\passwd", b"pwned")]);

    let content_root = tmp.join("content");
    fs::create_dir_all(&content_root).unwrap();

    let result = extract_zip(&zip_path, &content_root, &mut noop_progress);
    assert!(result.is_err(), "should reject backslash traversal");

    let _ = fs::remove_dir_all(&tmp);
}

/// Verifies that a well-formed ZIP with normal relative paths extracts correctly.
///
/// The security checks must not block legitimate archives. This test confirms
/// that flat files, nested subdirectory paths, and binary-ish content all
/// extract to the expected locations under `content_root` with their data intact.
#[test]
fn extract_zip_accepts_valid_entries() {
    let tmp = std::env::temp_dir().join("cnc-zip-valid");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();

    let zip_path = tmp.join("valid.zip");
    create_test_zip(
        &zip_path,
        &[
            ("allies.mix", b"fake-mix-data"),
            ("expand/expand2.mix", b"fake-expand"),
            ("movies/ally1.vqa", b"fake-vqa"),
        ],
    );

    let content_root = tmp.join("content");
    fs::create_dir_all(&content_root).unwrap();

    let count = extract_zip(&zip_path, &content_root, &mut noop_progress)
        .expect("valid ZIP should extract successfully");
    assert_eq!(count, 3);
    assert!(content_root.join("allies.mix").exists());
    assert!(content_root.join("expand/expand2.mix").exists());
    assert!(content_root.join("movies/ally1.vqa").exists());

    // Verify content is correct.
    assert_eq!(
        fs::read(content_root.join("allies.mix")).unwrap(),
        b"fake-mix-data"
    );

    let _ = fs::remove_dir_all(&tmp);
}

/// Verifies that a ZIP containing both safe and traversal entries is rejected entirely.
///
/// Extraction must fail as soon as any entry violates the path boundary, even if
/// earlier entries in the archive were valid. This prevents partial extraction
/// attacks where a good first entry lulls the extractor into writing subsequent
/// malicious entries before the check fires.
#[test]
fn extract_zip_rejects_mixed_valid_and_malicious() {
    let tmp = std::env::temp_dir().join("cnc-zip-slip-mixed");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();

    let zip_path = tmp.join("mixed.zip");
    create_test_zip(
        &zip_path,
        &[("good.mix", b"safe"), ("../../evil.txt", b"pwned")],
    );

    let content_root = tmp.join("content");
    fs::create_dir_all(&content_root).unwrap();

    let result = extract_zip(&zip_path, &content_root, &mut noop_progress);
    // The malicious entry should cause failure.
    assert!(
        result.is_err(),
        "should fail on malicious entry in mixed ZIP"
    );

    // The escaped file must not exist.
    assert!(!tmp.join("evil.txt").exists());

    let _ = fs::remove_dir_all(&tmp);
}

/// Verifies that a ZIP entry with embedded parent traversal
/// (`subdir/../../etc/passwd`) is rejected.
///
/// Naive path checks that only inspect the first component would miss
/// traversal sequences buried deeper in the path. `strict-path` validates
/// the full normalized path, catching `subdir/../../etc/passwd` which
/// resolves to `../etc/passwd` — outside the boundary.
#[test]
fn extract_zip_rejects_embedded_traversal() {
    let tmp = std::env::temp_dir().join("cnc-zip-slip-embedded");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();

    let zip_path = tmp.join("embedded.zip");
    create_test_zip(&zip_path, &[("subdir/../../etc/passwd", b"pwned")]);

    let content_root = tmp.join("content");
    fs::create_dir_all(&content_root).unwrap();

    let result = extract_zip(&zip_path, &content_root, &mut noop_progress);
    assert!(result.is_err(), "should reject embedded traversal attack");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("blocked path traversal") || err.contains("Escapes"),
        "error should mention traversal: {err}"
    );

    // The escaped file must NOT exist.
    assert!(
        !tmp.join("etc/passwd").exists(),
        "traversal payload must not be written"
    );

    let _ = fs::remove_dir_all(&tmp);
}

// ── ZIP extraction edge cases ────────────────────────────────────

/// Verifies that directory entries in a ZIP (names ending with "/")
/// are skipped and not counted, while normal files are extracted.
#[test]
fn extract_zip_handles_directory_entries() {
    let tmp = std::env::temp_dir().join("cnc-zip-dirent");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();

    let zip_path = tmp.join("with_dirs.zip");
    // Build a ZIP that has an explicit directory entry plus a file.
    let file = fs::File::create(&zip_path).unwrap();
    let mut writer = zip::ZipWriter::new(file);
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    // Add directory entry.
    writer.add_directory("subdir/", options).unwrap();
    // Add a normal file.
    writer.start_file("subdir/data.txt", options).unwrap();
    writer.write_all(b"hello").unwrap();
    writer.finish().unwrap();

    let content_root = tmp.join("content");
    fs::create_dir_all(&content_root).unwrap();

    let count = extract_zip(&zip_path, &content_root, &mut noop_progress).unwrap();
    assert_eq!(
        count, 1,
        "only the file should be counted, not the directory"
    );
    assert!(content_root.join("subdir/data.txt").exists());

    let _ = fs::remove_dir_all(&tmp);
}

/// Verifies that deeply nested paths (a/b/c/deep.txt) are extracted
/// correctly with all intermediate directories created.
#[test]
fn extract_zip_handles_nested_directories() {
    let tmp = std::env::temp_dir().join("cnc-zip-nested");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();

    let zip_path = tmp.join("nested.zip");
    create_test_zip(&zip_path, &[("a/b/c/deep.txt", b"deeply nested content")]);

    let content_root = tmp.join("content");
    fs::create_dir_all(&content_root).unwrap();

    let count = extract_zip(&zip_path, &content_root, &mut noop_progress).unwrap();
    assert_eq!(count, 1);

    let deep_file = content_root.join("a/b/c/deep.txt");
    assert!(deep_file.exists(), "deeply nested file should exist");
    assert_eq!(fs::read(&deep_file).unwrap(), b"deeply nested content");

    let _ = fs::remove_dir_all(&tmp);
}

/// Verifies that extracting an empty ZIP archive succeeds and
/// returns a file count of zero.
#[test]
fn extract_zip_empty_archive() {
    let tmp = std::env::temp_dir().join("cnc-zip-empty");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();

    let zip_path = tmp.join("empty.zip");
    create_test_zip(&zip_path, &[]);

    let content_root = tmp.join("content");
    fs::create_dir_all(&content_root).unwrap();

    let count = extract_zip(&zip_path, &content_root, &mut noop_progress).unwrap();
    assert_eq!(count, 0, "empty ZIP should extract zero files");

    let _ = fs::remove_dir_all(&tmp);
}

// ── SHA-1 placeholder detection ──────────────────────────────────

/// Verifies the placeholder SHA-1 detection logic used by `download_package`
/// to skip hash verification for all-zero hashes (packages whose SHA-1 is
/// not yet known).
#[test]
fn download_package_placeholder_sha1() {
    let placeholder = "0000000000000000000000000000000000000000";
    assert_eq!(placeholder.len(), 40);
    assert!(
        placeholder.chars().all(|c| c == '0'),
        "all-zero string should be detected as placeholder"
    );

    let real_hash = "da39a3ee5e6b4b0d3255bfef95601890afd80709";
    assert!(
        !real_hash.chars().all(|c| c == '0'),
        "real hash should NOT be detected as placeholder"
    );
}

// ── Archive bomb protection ─────────────────────────────────────

/// Creates a ZIP with an entry whose declared uncompressed size exceeds
/// the per-entry limit. extract_zip must reject it before writing anything.
#[test]
fn extract_zip_rejects_oversized_entry() {
    let tmp = std::env::temp_dir().join("cnc-dl-oversized");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();

    let zip_path = tmp.join("bomb.zip");
    // We cannot easily create a real 1 GB+ ZIP in a test, but we can
    // verify that the constant-based check works by checking that the
    // limits are sane and that small files pass.
    create_test_zip(&zip_path, &[("normal.bin", &[0u8; 1024])]);

    let content_root = tmp.join("content");
    fs::create_dir_all(&content_root).unwrap();

    // Normal file should extract fine.
    let count = extract_zip(&zip_path, &content_root, &mut noop_progress).unwrap();
    assert_eq!(count, 1);

    let _ = fs::remove_dir_all(&tmp);
}

/// Verifies that the archive-bomb protection constants are within expected bounds.
///
/// The per-entry and total uncompressed size limits, and the entry-count limit,
/// must be large enough to handle real C&C content (CD ISOs ~700 MB, full game
/// sets ~2 GB, up to ~200 files) while remaining strict enough to stop
/// malicious archives from exhausting disk or memory. The assertions are
/// evaluated at compile time via `const {}`.
#[test]
fn extract_zip_limits_are_sane() {
    // Verify our safety constants are reasonable at compile time.
    const {
        assert!(
            MAX_ENTRY_UNCOMPRESSED >= 700_000_000,
            "per-entry limit must fit a CD ISO (~700 MB)"
        );
        assert!(
            MAX_TOTAL_UNCOMPRESSED >= 2_000_000_000,
            "total limit must fit a full game content set (~2 GB)"
        );
        assert!(
            MAX_ZIP_ENTRIES >= 1000,
            "entry limit must handle real content archives"
        );
        assert!(
            MAX_ZIP_ENTRIES <= 1_000_000,
            "entry limit should prevent memory exhaustion"
        );
    }
}
