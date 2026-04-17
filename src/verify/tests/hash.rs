use super::*;

// ── hex_encode ───────────────────────────────────────────────────

/// `hex_encode` of an empty byte slice must produce an empty string.
///
/// The empty case is the boundary condition for the encoding loop; a
/// correct implementation must not write any characters when given no input.
#[test]
fn hex_encode_empty() {
    assert_eq!(hex_encode(&[]), "");
}

/// `hex_encode` must produce the correct two-character lowercase hex sequence
/// for a representative set of byte values including boundary bytes.
///
/// Verifying against known-correct vectors (0x00, 0xff, multi-byte sequences)
/// guards against off-by-one errors in nibble extraction and character ordering.
#[test]
fn hex_encode_known_values() {
    assert_eq!(hex_encode(&[0x00]), "00");
    assert_eq!(hex_encode(&[0xff]), "ff");
    assert_eq!(hex_encode(&[0xde, 0xad, 0xbe, 0xef]), "deadbeef");
    assert_eq!(
        hex_encode(&[0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef]),
        "0123456789abcdef"
    );
}

/// `hex_encode` must always produce lowercase hex digits, never uppercase.
///
/// Hash comparisons throughout the codebase use lowercase strings; if
/// `hex_encode` ever emits uppercase characters the comparisons would
/// silently fail, causing correct files to be flagged as corrupted.
#[test]
fn hex_encode_is_lowercase() {
    let result = hex_encode(&[0xAB, 0xCD]);
    assert_eq!(result, "abcd");
    assert!(result.chars().all(|c| !c.is_ascii_uppercase()));
}

// ── sha1_file ────────────────────────────────────────────────────

/// `sha1_file` of an empty file must produce the well-known SHA-1 of empty input.
///
/// The SHA-1 of the empty string is a published constant; matching it confirms
/// the hasher is initialized correctly and that the read loop terminates cleanly
/// without feeding stale buffer bytes into the digest.
#[test]
fn sha1_file_known_hash() {
    let tmp = std::env::temp_dir().join("cnc-verify-sha1");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();

    // SHA-1 of empty string is da39a3ee5e6b4b0d3255bfef95601890afd80709
    let path = tmp.join("empty.bin");
    fs::write(&path, b"").unwrap();
    let hash = sha1_file(&path, None).unwrap();
    assert_eq!(hash, "da39a3ee5e6b4b0d3255bfef95601890afd80709");

    let _ = fs::remove_dir_all(&tmp);
}

/// `sha1_file` with a prefix length hashes only the leading N bytes, not the whole file.
///
/// OpenRA source identification relies on prefix-only hashing to fingerprint
/// large mix archives without reading them in full. If `sha1_file` accidentally
/// reads past the prefix, a valid disc would be misidentified as the wrong edition.
///
/// The test writes a known file, hashes its first 5 bytes in isolation, and
/// verifies the result matches independently hashing a file containing only
/// those same 5 bytes.
#[test]
fn sha1_file_prefix_length() {
    let tmp = std::env::temp_dir().join("cnc-verify-sha1-prefix");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();

    let path = tmp.join("data.bin");
    fs::write(&path, b"HELLO WORLD EXTRA DATA").unwrap();

    // Hash of "HELLO" (5 bytes) vs hash of entire file — should differ.
    let hash_prefix = sha1_file(&path, Some(5)).unwrap();
    let hash_full = sha1_file(&path, None).unwrap();
    assert_ne!(hash_prefix, hash_full);

    // Hash of prefix should be the same as hashing just "HELLO".
    let path2 = tmp.join("hello.bin");
    fs::write(&path2, b"HELLO").unwrap();
    let hash_hello = sha1_file(&path2, None).unwrap();
    assert_eq!(hash_prefix, hash_hello);

    let _ = fs::remove_dir_all(&tmp);
}

/// `sha1_file` must return an error when the target file does not exist.
///
/// Callers in `verify_id_file` propagate the error through `?`; if `sha1_file`
/// silently succeeded on a missing file, source identification would produce
/// false positives by treating every absent ID file as a match.
#[test]
fn sha1_file_missing_returns_error() {
    let result = sha1_file(std::path::Path::new("/nonexistent/file.bin"), None);
    assert!(result.is_err());
}

// ── blake3_file ──────────────────────────────────────────────────

/// `blake3_file` of an empty file must produce the well-known BLAKE3 of empty input.
///
/// The BLAKE3 of the empty string is a published 64-character constant; matching
/// it confirms the hasher is initialized correctly and the output length is always
/// exactly 64 hex characters, as required by the manifest format.
#[test]
fn blake3_file_known_hash() {
    let tmp = std::env::temp_dir().join("cnc-verify-blake3");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();

    // BLAKE3 of empty string is af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262
    let path = tmp.join("empty.bin");
    fs::write(&path, b"").unwrap();
    let hash = blake3_file(&path).unwrap();
    assert_eq!(
        hash,
        "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
    );
    assert_eq!(hash.len(), 64);

    let _ = fs::remove_dir_all(&tmp);
}

/// `blake3_file` must return only lowercase hex digits with no uppercase characters.
///
/// Manifest files store hashes as lowercase strings; an uppercase digit would
/// cause string equality to fail during verification even when the file is intact,
/// producing a false corruption report.
#[test]
fn blake3_file_is_lowercase_hex() {
    let tmp = std::env::temp_dir().join("cnc-verify-blake3-case");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();

    let path = tmp.join("data.bin");
    fs::write(&path, b"test data for hashing").unwrap();
    let hash = blake3_file(&path).unwrap();
    assert!(hash
        .chars()
        .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));

    let _ = fs::remove_dir_all(&tmp);
}
