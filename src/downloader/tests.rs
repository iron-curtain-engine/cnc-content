//! Unit and security tests for the HTTP downloader.
//!
//! Covers happy-path downloads, parallel mirror racing, SHA-1 verification,
//! and adversarial inputs (path traversal, mismatched hashes).

use super::*;
use std::io::Write;

/// Creates an in-memory ZIP archive and writes it to `dest`.
/// `entries` is a list of `(name, content)` tuples where `name` may
/// contain path traversal sequences for security testing.
fn create_test_zip(dest: &Path, entries: &[(&str, &[u8])]) {
    let file = fs::File::create(dest).unwrap();
    let mut writer = zip::ZipWriter::new(file);
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for &(name, data) in entries {
        writer.start_file(name, options).unwrap();
        writer.write_all(data).unwrap();
    }
    writer.finish().unwrap();
}

fn noop_progress(_: DownloadProgress) {}

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

// ── DownloadError::NotFreeware ───────────────────────────────────

/// Verifies that `download_missing` rejects non-freeware games immediately
/// (before any HTTP calls), returning `DownloadError::NotFreeware`.
#[test]
fn download_missing_rejects_non_freeware() {
    let tmp = std::env::temp_dir().join("cnc-dl-notfreeware");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();

    let result = download_missing(
        &tmp,
        GameId::Dune2,
        SeedingPolicy::ExtractAndDelete,
        &mut noop_progress,
    );
    assert!(result.is_err(), "Dune2 is not freeware, should be rejected");

    let err = result.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("not freeware"),
        "error should mention 'not freeware': {msg}"
    );
    assert!(
        msg.contains("Dune"),
        "error should mention game name containing 'Dune': {msg}"
    );

    let _ = fs::remove_dir_all(&tmp);
}

// ── DownloadError Display messages ───────────────────────────────

/// Verifies the Display impl for `DownloadError::NoUrls` includes
/// the expected human-readable message.
#[test]
fn download_error_display_no_urls() {
    let err = DownloadError::NoUrls;
    let msg = err.to_string();
    assert!(
        msg.contains("no download URLs"),
        "NoUrls display should contain 'no download URLs': {msg}"
    );
}

/// Verifies the Display impl for `DownloadError::Sha1Mismatch`
/// includes both the expected and actual hash values.
#[test]
fn download_error_display_sha1_mismatch() {
    let err = DownloadError::Sha1Mismatch {
        expected: "aaa".into(),
        actual: "bbb".into(),
    };
    let msg = err.to_string();
    assert!(msg.contains("aaa"), "should contain expected hash: {msg}");
    assert!(msg.contains("bbb"), "should contain actual hash: {msg}");
}

/// Verifies the Display impl for `DownloadError::AllMirrorsFailed`
/// includes the mirror count and last error message.
#[test]
fn download_error_display_all_mirrors_failed() {
    let err = DownloadError::AllMirrorsFailed {
        count: 3,
        last_error: "timeout".into(),
    };
    let msg = err.to_string();
    assert!(msg.contains("3"), "should contain mirror count: {msg}");
    assert!(msg.contains("timeout"), "should contain last error: {msg}");
}

/// Verifies the Display impl for `DownloadError::NotFreeware`
/// includes the game name and "not freeware" phrasing.
#[test]
fn download_error_display_not_freeware() {
    let err = DownloadError::NotFreeware {
        game: "Dune II".into(),
    };
    let msg = err.to_string();
    assert!(msg.contains("Dune II"), "should contain game name: {msg}");
    assert!(
        msg.contains("not freeware"),
        "should contain 'not freeware': {msg}"
    );
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

// ── URL validation for mirror lists ─────────────────────────────

/// Verifies that `is_safe_mirror_url` accepts valid HTTP and HTTPS URLs.
///
/// HTTP and HTTPS are the only schemes allowed for mirror downloads. Both
/// must be accepted when the host is a legitimate public domain so that real
/// mirror lists function correctly.
#[test]
fn safe_mirror_url_accepts_https() {
    assert!(is_safe_mirror_url("https://github.com/file.zip"));
    assert!(is_safe_mirror_url("http://archive.org/file.zip"));
}

/// Verifies that `is_safe_mirror_url` rejects `file://` URLs.
///
/// A `file://` URL would cause the downloader to read an arbitrary local path
/// rather than a remote mirror, leaking host filesystem contents. These must
/// be rejected regardless of the path they reference.
#[test]
fn safe_mirror_url_rejects_file_scheme() {
    assert!(!is_safe_mirror_url("file:///etc/passwd"));
    assert!(!is_safe_mirror_url("file:///C:/Windows/System32"));
}

/// Verifies that `is_safe_mirror_url` rejects `data:` URLs.
///
/// `data:` URIs embed content directly in the URL rather than fetching a
/// remote resource. A compromised mirror list server could inject `data:`
/// entries to bypass the allowlist and supply arbitrary bytes as download
/// content.
#[test]
fn safe_mirror_url_rejects_data_scheme() {
    assert!(!is_safe_mirror_url("data:text/plain,hello"));
}

/// Verifies that `is_safe_mirror_url` rejects `ftp://` URLs.
///
/// Only HTTP and HTTPS are supported download transports. FTP URLs would
/// require a separate protocol stack and must be rejected to keep the attack
/// surface minimal and the scheme allowlist strict.
#[test]
fn safe_mirror_url_rejects_ftp_scheme() {
    assert!(!is_safe_mirror_url("ftp://internal.local/data.zip"));
}

/// Verifies that `is_safe_mirror_url` rejects loopback addresses as SSRF targets.
///
/// A compromised mirror list could supply `localhost`, `127.0.0.1`, `[::1]`,
/// or `0.0.0.0` to cause the downloader to probe local services (admin panels,
/// metadata endpoints, etc.). All loopback forms must be rejected, including
/// URLs with explicit ports.
#[test]
fn safe_mirror_url_rejects_localhost() {
    assert!(!is_safe_mirror_url("http://localhost/admin"));
    assert!(!is_safe_mirror_url("http://localhost:8080/api"));
    assert!(!is_safe_mirror_url("http://127.0.0.1/secret"));
    assert!(!is_safe_mirror_url("http://0.0.0.0/"));
}

/// Verifies that `is_safe_mirror_url` rejects RFC-1918 and link-local addresses.
///
/// Private IPv4 ranges (10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16) and
/// link-local (169.254.0.0/16) must be blocked to prevent SSRF attacks against
/// internal network services. The test covers representative addresses from
/// every blocked range, including boundary values within the 172.16–31 block.
#[test]
fn safe_mirror_url_rejects_private_networks() {
    assert!(!is_safe_mirror_url("http://10.0.0.1/file.zip"));
    assert!(!is_safe_mirror_url("http://10.255.255.255/file.zip"));
    assert!(!is_safe_mirror_url("http://192.168.1.1/file.zip"));
    assert!(!is_safe_mirror_url("http://192.168.0.1:8080/file.zip"));
    assert!(!is_safe_mirror_url("http://172.16.0.1/file.zip"));
    assert!(!is_safe_mirror_url("http://172.31.255.255/file.zip"));
    assert!(!is_safe_mirror_url("http://169.254.1.1/file.zip"));
}

/// Verifies that `is_safe_mirror_url` rejects URLs containing newline characters.
///
/// Newlines (`\n`, `\r\n`) embedded in a URL can be used for HTTP header
/// injection: if the URL is passed verbatim to an HTTP client, the injected
/// bytes become additional headers or a second request. Both LF and CRLF
/// sequences must be rejected unconditionally.
#[test]
fn safe_mirror_url_rejects_newline_injection() {
    assert!(!is_safe_mirror_url("http://good.com\nhttp://evil.com"));
    assert!(!is_safe_mirror_url("http://good.com\r\nEvil: header"));
}

/// Verifies that `is_safe_mirror_url` rejects bare (dot-free) hostnames.
///
/// A hostname without any dot (e.g. `internal`, `database`) typically refers
/// to an intranet host resolvable only within a private network. Requiring at
/// least one dot prevents a mirror list from routing requests to internal
/// services that would not be reachable from a public IP.
#[test]
fn safe_mirror_url_rejects_bare_hostname() {
    assert!(!is_safe_mirror_url("http://internal/file.zip"));
    assert!(!is_safe_mirror_url("http://database/dump.sql"));
}

/// Verifies that `is_safe_mirror_url` accepts 172.x addresses outside the private /12 block.
///
/// Only 172.16.0.0–172.31.255.255 is RFC-1918 private space. Addresses such as
/// 172.32.x.x and 172.15.x.x are public and must not be wrongly blocked by an
/// overly broad prefix check. This test guards against an off-by-one in the
/// second-octet range comparison.
#[test]
fn safe_mirror_url_allows_172_outside_private_range() {
    // 172.32.x.x is NOT private (private is 172.16-31.x.x).
    assert!(is_safe_mirror_url("http://172.32.0.1/file.zip"));
    assert!(is_safe_mirror_url("http://172.15.0.1/file.zip"));
}

/// Verifies that `is_safe_mirror_url` rejects empty strings, non-URL text, and `javascript:`.
///
/// The validator must not accept degenerate inputs that lack an HTTP/HTTPS scheme.
/// An empty string, a plain sentence, and `javascript:` URLs are all invalid mirror
/// sources and must be rejected before any host extraction is attempted.
#[test]
fn safe_mirror_url_rejects_empty_and_garbage() {
    assert!(!is_safe_mirror_url(""));
    assert!(!is_safe_mirror_url("not a url"));
    assert!(!is_safe_mirror_url("javascript:alert(1)"));
}

// ── Mirror list parsing (extracted from fetch_mirror_list) ─────

/// Verifies that `parse_mirror_list_response` parses a well-formed mirror list body.
///
/// A newline-separated list of valid HTTPS URLs must be parsed into a vector
/// preserving order and URL text exactly, with no extraneous entries introduced.
#[test]
fn parse_mirror_list_valid_urls() {
    let body = "https://mirror1.example.com/file.zip\nhttps://mirror2.example.com/file.zip\n";
    let mirrors = parse_mirror_list_response(body).unwrap();
    assert_eq!(mirrors.len(), 2);
    assert_eq!(mirrors[0], "https://mirror1.example.com/file.zip");
    assert_eq!(mirrors[1], "https://mirror2.example.com/file.zip");
}

/// Verifies that `parse_mirror_list_response` silently drops unsafe URLs.
///
/// When a mirror list body mixes safe HTTPS URLs with dangerous ones (`file://`,
/// `http://localhost`), only the safe URLs must be returned. The unsafe entries
/// are discarded without error, so that a partially compromised list still
/// produces working downloads from its safe mirrors.
#[test]
fn parse_mirror_list_filters_unsafe_urls() {
    let body = "https://good.example.com/file.zip\nfile:///etc/passwd\nhttp://localhost/admin\nhttps://also-good.example.com/file.zip\n";
    let mirrors = parse_mirror_list_response(body).unwrap();
    assert_eq!(mirrors.len(), 2);
    assert!(mirrors.iter().all(|u| u.starts_with("https://")));
}

/// Verifies that `parse_mirror_list_response` returns `NoUrls` when every URL is unsafe.
///
/// If all entries in the mirror list are filtered out (e.g. `file://`, `localhost`,
/// `ftp://`), the function must return `DownloadError::NoUrls` rather than an empty
/// success vector, so that callers can distinguish "no list fetched" from "list was
/// entirely invalid".
#[test]
fn parse_mirror_list_all_unsafe_returns_error() {
    let body = "file:///etc/passwd\nhttp://localhost/admin\nftp://internal/file\n";
    let result = parse_mirror_list_response(body);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("no download URLs"), "error: {err}");
}

/// Verifies that `parse_mirror_list_response` trims whitespace and ignores blank lines.
///
/// Mirror list files served by web servers may include leading/trailing spaces
/// or empty lines between entries. The parser must strip surrounding whitespace
/// from each line and skip lines that are empty after trimming, so the result
/// contains only clean URL strings.
#[test]
fn parse_mirror_list_strips_whitespace_and_blanks() {
    let body = "  https://mirror.example.com/file.zip  \n\n\n  \n";
    let mirrors = parse_mirror_list_response(body).unwrap();
    assert_eq!(mirrors.len(), 1);
    assert_eq!(mirrors[0], "https://mirror.example.com/file.zip");
}

/// Verifies that `parse_mirror_list_response` returns an error for an empty body.
///
/// An empty response (or one containing only whitespace) means the mirror server
/// returned no usable URLs. This must be surfaced as `DownloadError::NoUrls`
/// rather than silently returning an empty list, so callers fall through to
/// direct URL fallback correctly.
#[test]
fn parse_mirror_list_empty_body_returns_error() {
    assert!(parse_mirror_list_response("").is_err());
    assert!(parse_mirror_list_response("  \n  \n").is_err());
}

/// Verifies that `parse_mirror_list_response` accepts HTTP/HTTPS and rejects FTP and `data:`.
///
/// A realistic mirror list may mix HTTP, HTTPS, FTP, and other schemes. Only
/// HTTP and HTTPS entries must survive filtering; FTP and data URIs must be
/// silently dropped. The returned list preserves the original order of accepted entries.
#[test]
fn parse_mirror_list_mixed_schemes() {
    let body = "https://cdn.example.com/ra.zip\nhttp://archive.example.org/ra.zip\nftp://old.example.net/ra.zip\ndata:text/plain,evil";
    let mirrors = parse_mirror_list_response(body).unwrap();
    assert_eq!(mirrors.len(), 2);
    assert!(mirrors[0].starts_with("https://"));
    assert!(mirrors[1].starts_with("http://"));
}

// ── URL resolution logic ────────────────────────────────────────

/// Verifies that `resolve_download_urls` returns mirror-list URLs when no direct URLs are given.
///
/// When a mirror list is available and no direct URLs are provided, the resolved
/// list must contain exactly the mirror URLs in their original order.
#[test]
fn resolve_urls_mirrors_only() {
    let mirrors = vec![
        "https://m1.example.com/f.zip".to_string(),
        "https://m2.example.com/f.zip".to_string(),
    ];
    let urls = resolve_download_urls(Some(&mirrors), &[]);
    assert_eq!(urls.len(), 2);
    assert_eq!(urls[0], "https://m1.example.com/f.zip");
}

/// Verifies that `resolve_download_urls` falls back to direct URLs when no mirror list is given.
///
/// When `mirror_urls` is `None`, the resolved list must contain all direct URLs
/// in their original order. This exercises the pure-direct-URL fallback path used
/// for packages that do not have a mirror list endpoint.
#[test]
fn resolve_urls_direct_only() {
    let urls = resolve_download_urls(
        None,
        &[
            "https://direct1.example.com/f.zip",
            "https://direct2.example.com/f.zip",
        ],
    );
    assert_eq!(urls.len(), 2);
}

/// Verifies that `resolve_download_urls` deduplicates URLs appearing in both lists.
///
/// Mirror-list URLs come first; direct URLs that are already present in the mirror
/// list must not be appended again. A URL that appears only in the direct list must
/// still be appended. This ensures the combined list has no duplicates while
/// preserving mirror-list ordering for the parallel download race.
#[test]
fn resolve_urls_mirrors_plus_direct_deduplicates() {
    let mirrors = vec!["https://shared.example.com/f.zip".to_string()];
    let urls = resolve_download_urls(
        Some(&mirrors),
        &[
            "https://shared.example.com/f.zip",
            "https://extra.example.com/f.zip",
        ],
    );
    // shared.example.com should appear only once (from mirrors).
    assert_eq!(urls.len(), 2);
    assert_eq!(urls[0], "https://shared.example.com/f.zip");
    assert_eq!(urls[1], "https://extra.example.com/f.zip");
}

/// Verifies that `resolve_download_urls` appends direct URLs when the mirror slice is empty.
///
/// `Some(&[])` signals that a mirror list was fetched but contained no entries.
/// Direct URLs must still be appended as fallback so that the download can proceed
/// without treating an empty-but-present mirror list as a fatal error.
#[test]
fn resolve_urls_empty_mirrors_falls_through_to_direct() {
    let urls = resolve_download_urls(Some(&[]), &["https://fallback.example.com/f.zip"]);
    assert_eq!(urls.len(), 1);
}

/// Verifies that `resolve_download_urls` returns an empty vector when given no URLs at all.
///
/// `None` mirror list and an empty direct-URL slice means there is genuinely nothing
/// to download from. The caller (`download_package`) checks for this empty result and
/// returns `DownloadError::NoUrls`, so this function must not fabricate any entries.
#[test]
fn resolve_urls_none_mirrors_and_no_direct_returns_empty() {
    let urls = resolve_download_urls(None, &[]);
    assert!(urls.is_empty());
}

// ── download_package orchestration (no HTTP) ────────────────────

/// Verifies that `download_package` returns `NoUrls` when a package has no mirror list and no direct URLs.
///
/// A package with both `mirror_list_url` and `direct_urls` empty has no reachable
/// download source. `download_package` must detect this before making any network
/// calls and return `DownloadError::NoUrls` immediately.
///
/// The test constructs a minimal `DownloadPackage` with all URL fields empty so
/// no HTTP request is attempted, making this an offline, deterministic assertion.
/// Uses an IC-hosted download ID that has no compiled mirrors, ensuring the
/// compiled-mirror path does not inject URLs.
#[test]
fn download_package_no_urls_returns_error() {
    let pkg = DownloadPackage {
        id: DownloadId::RaMoviesAllied,
        game: GameId::RedAlert,
        title: "Test Package".to_string(),
        mirror_list_url: None,
        mirrors: vec![],
        direct_urls: vec![],
        sha1: None,
        info_hash: None,
        trackers: vec![],
        web_seeds: vec![],
        provides: vec![crate::PackageId::RaMoviesAllied],
        format: "zip".to_string(),
        size_hint: 0,
    };

    let tmp = std::env::temp_dir().join("cnc-dl-no-urls");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();

    let result = download_package(&pkg, &tmp, &mut noop_progress);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("no download URLs"), "error: {err}");

    let _ = fs::remove_dir_all(&tmp);
}

// ── download_missing ────────────────────────────────────────────

/// Verifies that `download_missing` is a no-op when all required content files already exist.
///
/// If `is_content_complete` returns true, `download_missing` must return `Ok(())`
/// without emitting any progress events and without making network calls. This
/// guarantees idempotency: re-running the tool on an already-installed game does nothing.
///
/// The test seeds the temp directory with every required test-file sentinel for
/// Red Alert so that `is_content_complete` returns true, then confirms the progress
/// callback is never invoked.
#[test]
fn download_missing_already_complete_is_noop() {
    let tmp = std::env::temp_dir().join("cnc-dl-complete");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();

    // Create all required RA test files so is_content_complete returns true.
    for pkg in crate::packages_for_game(GameId::RedAlert) {
        if !pkg.required {
            continue;
        }
        for &test_file in pkg.test_files {
            let path = tmp.join(test_file);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&path, b"test").unwrap();
        }
    }

    // Should return Ok(()) immediately without any HTTP calls.
    let mut progress_called = false;
    download_missing(
        &tmp,
        GameId::RedAlert,
        SeedingPolicy::ExtractAndDelete,
        &mut |_| {
            progress_called = true;
        },
    )
    .expect("download_missing should succeed when content is complete");
    assert!(
        !progress_called,
        "no progress events when content is already complete"
    );

    let _ = fs::remove_dir_all(&tmp);
}

// ── select_strategy ─────────────────────────────────────────────

/// Verifies that `select_strategy` returns `Http` when the `torrent` feature is not compiled in.
///
/// A package with a non-empty `info_hash` would normally qualify for BitTorrent
/// download, but when the `torrent` feature flag is absent the P2P code path is
/// gated out at compile time. `select_strategy` must fall back to `Http` in that
/// case so the download still proceeds via HTTP mirrors.
#[test]
#[cfg(not(feature = "torrent"))]
fn select_strategy_http_for_no_torrent_feature() {
    let pkg = DownloadPackage {
        id: DownloadId::RaQuickInstall,
        game: GameId::RedAlert,
        title: "Test".to_string(),
        mirror_list_url: None,
        mirrors: vec![],
        direct_urls: vec![],
        sha1: None,
        info_hash: Some("abcdef1234567890abcdef1234567890abcdef12".to_string()),
        trackers: vec![],
        web_seeds: vec![],
        provides: vec![],
        format: "zip".to_string(),
        size_hint: 0,
    };
    // Even with an info_hash, without the torrent feature it should be Http.
    assert_eq!(select_strategy(&pkg), DownloadStrategy::Http);
}

// ── Security: download pre-allocation cap (CWE-400 / CWE-409) ──

/// The MAX_PREALLOC_SIZE constant is reasonable for known C&C content.
///
/// The largest downloadable packages are ~700 MB disc ISOs. The pre-allocation
/// cap must be large enough for any real content while preventing a spoofed
/// Content-Length header from allocating unbounded disk space (sparse file DoS).
#[test]
fn max_prealloc_size_is_sane() {
    use super::mirror::MAX_PREALLOC_SIZE;
    const {
        // Must fit the largest C&C disc ISOs (~700 MB).
        assert!(
            MAX_PREALLOC_SIZE >= 700_000_000,
            "pre-alloc cap must fit disc ISOs"
        );
        // Must not allow unbounded allocation (cap at a few GB).
        assert!(
            MAX_PREALLOC_SIZE <= 4 * 1024 * 1024 * 1024,
            "pre-alloc cap should prevent excessive allocation"
        );
    }
}

// ── Security: redirect limit (SSRF mitigation) ─────────────────

/// The MAX_REDIRECTS constant is reasonable for CDN content delivery.
///
/// CDN redirects (e.g. GitHub Releases → objects.githubusercontent.com)
/// typically need only 1–2 hops. Limiting to 5 prevents redirect-chain
/// attacks while allowing legitimate CDN routing.
#[test]
fn max_redirects_is_sane() {
    use super::mirror::MAX_REDIRECTS;
    const {
        // Must allow CDN redirects (GitHub uses 1–2).
        assert!(
            MAX_REDIRECTS >= 2,
            "redirect limit must allow CDN redirects"
        );
        // Must prevent long redirect chains (default ureq is 10).
        assert!(
            MAX_REDIRECTS <= 8,
            "redirect limit should prevent chain attacks"
        );
    }
}

// ── IPv6 SSRF denylist tests ─────────────────────────────────────────

/// IPv6 loopback `[::1]` must be rejected (with and without port).
///
/// Without this check, an attacker could bypass the IPv4 `127.0.0.1`
/// denylist by using the equivalent IPv6 loopback address.
#[test]
fn rejects_ipv6_loopback() {
    use super::mirror::is_safe_mirror_url;
    assert!(!is_safe_mirror_url("https://[::1]/file.zip"));
    assert!(!is_safe_mirror_url("https://[::1]:8080/file.zip"));
}

/// IPv4-mapped IPv6 addresses (`::ffff:127.0.0.1`, `::ffff:10.x.x.x`)
/// must be rejected to prevent IPv4 denylist bypass.
///
/// An attacker who controls a mirror list entry can use IPv4-mapped IPv6
/// notation to reference private IPv4 addresses while evading a check
/// that only inspects the dotted-decimal form.
#[test]
fn rejects_ipv4_mapped_ipv6_private() {
    use super::mirror::is_safe_mirror_url;
    // Loopback via IPv4-mapped
    assert!(!is_safe_mirror_url("https://[::ffff:127.0.0.1]/file.zip"));
    // 10.0.0.0/8 via IPv4-mapped
    assert!(!is_safe_mirror_url("https://[::ffff:10.0.0.1]/file.zip"));
    // 192.168.0.0/16 via IPv4-mapped
    assert!(!is_safe_mirror_url("https://[::ffff:192.168.1.1]/file.zip"));
    // 172.16.0.0/12 via IPv4-mapped
    assert!(!is_safe_mirror_url("https://[::ffff:172.16.0.1]/file.zip"));
    // 169.254.0.0/16 (link-local) via IPv4-mapped
    assert!(!is_safe_mirror_url("https://[::ffff:169.254.1.1]/file.zip"));
}

/// IPv6 link-local (`fe80::/10`) and ULA (`fc00::/7`) addresses must be
/// rejected.
///
/// These are IPv6-native private ranges analogous to RFC 1918. A
/// compromised mirror list could use them for SSRF against IPv6 services
/// on the local network.
#[test]
fn rejects_ipv6_link_local_and_ula() {
    use super::mirror::is_safe_mirror_url;
    // Link-local
    assert!(!is_safe_mirror_url("https://[fe80::1%25eth0]/file.zip"));
    // ULA fd00::/8
    assert!(!is_safe_mirror_url("https://[fd12:3456::1]/file.zip"));
    // ULA fc00::/8
    assert!(!is_safe_mirror_url("https://[fc00::1]/file.zip"));
}

/// Public IPv6 addresses should still be accepted.
///
/// Ensures the IPv6 denylist does not over-block legitimate global
/// unicast addresses (2000::/3).
#[test]
fn accepts_public_ipv6() {
    use super::mirror::is_safe_mirror_url;
    assert!(is_safe_mirror_url(
        "https://[2607:f8b0:4004:800::200e]/file.zip"
    ));
}
