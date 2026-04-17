use super::*;

// ── verify_id_file ───────────────────────────────────────────────

/// `verify_id_file` must return `true` when the file exists and its SHA-1 matches.
///
/// This is the happy-path for source identification: a known-good file on disk
/// must be recognized correctly so that disc/Steam installs are not rejected.
///
/// The expected hash is computed at runtime from the same file to avoid
/// hard-coding a test vector that could drift from the implementation.
#[test]
fn verify_id_file_match() {
    let tmp = std::env::temp_dir().join("cnc-verify-id-match");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();

    let data = b"known content";
    fs::write(tmp.join("test.mix"), data).unwrap();

    // Compute the real SHA-1 of "known content".
    let expected_sha1 = sha1_file(&tmp.join("test.mix"), None).unwrap();

    let check = IdFileCheck {
        path: "test.mix",
        sha1: Box::leak(expected_sha1.into_boxed_str()),
        prefix_length: None,
    };

    assert!(verify_id_file(&tmp, &check).unwrap());

    let _ = fs::remove_dir_all(&tmp);
}

/// `verify_id_file` must return `false` when the file exists but its SHA-1 does not match.
///
/// A wrong hash must never be treated as a positive identification; otherwise a
/// corrupted or wrong-edition disc file could be accepted as a valid source.
#[test]
fn verify_id_file_mismatch() {
    let tmp = std::env::temp_dir().join("cnc-verify-id-mismatch");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();

    fs::write(tmp.join("test.mix"), b"actual content").unwrap();

    let check = IdFileCheck {
        path: "test.mix",
        sha1: "0000000000000000000000000000000000000000",
        prefix_length: None,
    };

    assert!(!verify_id_file(&tmp, &check).unwrap());

    let _ = fs::remove_dir_all(&tmp);
}

/// `verify_id_file` must return `false` (not an error) when the ID file is absent.
///
/// Missing ID files are the normal case for sources that are not installed;
/// returning `false` allows `identify_source` to move on to the next candidate
/// without propagating an error through the entire source-scan loop.
#[test]
fn verify_id_file_missing_returns_false() {
    let tmp = std::env::temp_dir().join("cnc-verify-id-missing");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();

    let check = IdFileCheck {
        path: "nonexistent.mix",
        sha1: "0000000000000000000000000000000000000000",
        prefix_length: None,
    };

    assert!(!verify_id_file(&tmp, &check).unwrap());

    let _ = fs::remove_dir_all(&tmp);
}

/// `verify_id_file` must respect `prefix_length` and hash only the leading bytes.
///
/// OpenRA's ID-file checks for large mix archives specify a prefix so the tool
/// does not read gigabytes to identify a disc. Verifying that the prefix path
/// matches the same result as a full hash of those same bytes confirms the
/// `IdFileCheck.prefix_length` field is wired through to `sha1_file` correctly.
#[test]
fn verify_id_file_with_prefix() {
    let tmp = std::env::temp_dir().join("cnc-verify-id-prefix");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();

    fs::write(tmp.join("main.mix"), b"HEADER_BYTES_REST_OF_FILE").unwrap();

    // Get SHA-1 of first 12 bytes ("HEADER_BYTES").
    let expected = sha1_file(&tmp.join("main.mix"), Some(12)).unwrap();

    let check = IdFileCheck {
        path: "main.mix",
        sha1: Box::leak(expected.into_boxed_str()),
        prefix_length: Some(12),
    };

    assert!(verify_id_file(&tmp, &check).unwrap());

    let _ = fs::remove_dir_all(&tmp);
}

// ── verify_installed_content ─────────────────────────────────────

/// `verify_installed_content` must report corrupted and missing files while passing good ones.
///
/// The function is the repair-scan entry point: it must correctly distinguish
/// three cases — file with matching hash (pass), file with wrong hash (fail),
/// and file absent from disk (fail) — so that the repair path replaces only
/// the files that actually need it.
#[test]
fn verify_installed_content_detects_mismatch() {
    let tmp = std::env::temp_dir().join("cnc-verify-installed");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();

    fs::write(tmp.join("good.mix"), b"correct data").unwrap();
    fs::write(tmp.join("bad.mix"), b"wrong data").unwrap();

    let good_hash = blake3_file(&tmp.join("good.mix")).unwrap();

    let mut files = BTreeMap::new();
    files.insert(
        "good.mix".to_string(),
        FileDigest {
            blake3: good_hash,
            size: 12,
        },
    );
    files.insert(
        "bad.mix".to_string(),
        FileDigest {
            blake3: "0000000000000000000000000000000000000000000000000000000000000000".to_string(),
            size: 10,
        },
    );
    files.insert(
        "missing.mix".to_string(),
        FileDigest {
            blake3: "0000000000000000000000000000000000000000000000000000000000000000".to_string(),
            size: 0,
        },
    );

    let manifest = InstalledContentManifest {
        version: CONTENT_MANIFEST_VERSION,
        game: "ra".to_string(),
        content_version: "v1".to_string(),
        files,
    };

    let failures = verify_installed_content(&tmp, &manifest);
    assert_eq!(failures.len(), 2);
    assert!(failures.contains(&"bad.mix".to_string()));
    assert!(failures.contains(&"missing.mix".to_string()));
    assert!(!failures.contains(&"good.mix".to_string()));

    let _ = fs::remove_dir_all(&tmp);
}

// ── identify_source ──────────────────────────────────────────────

/// `identify_source` must return `None` when given a directory with no ID files.
///
/// An empty directory cannot match any known source; returning `None` rather
/// than panicking or guessing is required so callers can cleanly fall through
/// to trying the next candidate path.
#[test]
fn identify_source_returns_none_for_empty_dir() {
    let tmp = std::env::temp_dir().join("cnc-verify-identify-empty");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();

    assert!(identify_source(&tmp).is_none());

    let _ = fs::remove_dir_all(&tmp);
}

// ── prefix_length edge cases ────────────────────────────────────

/// Hashing zero bytes of a file should produce the SHA-1 of empty input.
///
/// A prefix length of zero is degenerate but must not panic; it should
/// behave identically to hashing an empty byte slice.
#[test]
fn sha1_file_prefix_zero_length() {
    let tmp = std::env::temp_dir().join("cnc-verify-sha1-prefix-zero");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();

    let path = tmp.join("hello.bin");
    fs::write(&path, b"hello").unwrap();

    let hash = sha1_file(&path, Some(0)).unwrap();
    assert_eq!(hash, "da39a3ee5e6b4b0d3255bfef95601890afd80709");

    let _ = fs::remove_dir_all(&tmp);
}

/// Requesting a prefix longer than the file should fail with an I/O error.
///
/// `read_exact` on a short file returns `UnexpectedEof`; callers must not
/// silently succeed with a partial read when the prefix cannot be filled.
#[test]
fn sha1_file_prefix_exceeds_file_size() {
    let tmp = std::env::temp_dir().join("cnc-verify-sha1-prefix-exceeds");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();

    let path = tmp.join("short.bin");
    fs::write(&path, b"hello").unwrap(); // 5 bytes

    let result = sha1_file(&path, Some(1000));
    assert!(result.is_err());

    let err = result.unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);

    let _ = fs::remove_dir_all(&tmp);
}

// ── identify_source partial match ───────────────────────────────

/// A directory that matches only one of a source's ID files must not be
/// identified as that source.
///
/// `identify_source` requires *all* ID files to match. If only a subset
/// passes, the source should be rejected and `None` returned.
#[test]
fn identify_source_partial_match_returns_none() {
    let tmp = std::env::temp_dir().join("cnc-verify-identify-partial");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();

    // Pick the first source that has more than one ID file.
    let multi_id_source = ALL_SOURCES
        .iter()
        .find(|s| s.id_files.len() > 1)
        .expect("test requires at least one source with multiple id_files");

    // Create only the first ID file with the correct content hash,
    // but omit the rest. We write dummy content and verify by brute
    // approach: create the file, hash it, and only keep it if the hash
    // matches. Since we cannot easily forge SHA-1 content, we instead
    // just create the file path — the hash will not match, which also
    // means not all ID files pass. But to be precise about "one match,
    // others missing", we simply do NOT create the other files at all.
    // The first file exists with wrong content, so verify_id_file returns
    // false for hash mismatch; the second file is missing. Either way,
    // not all match.
    let first_check = &multi_id_source.id_files[0];
    let file_path = tmp.join(first_check.path);
    if let Some(parent) = file_path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    // Write arbitrary content — the hash will not match the expected one,
    // but the file exists. The second ID file is missing entirely.
    fs::write(&file_path, b"not the real content").unwrap();

    assert!(identify_source(&tmp).is_none());

    let _ = fs::remove_dir_all(&tmp);
}
