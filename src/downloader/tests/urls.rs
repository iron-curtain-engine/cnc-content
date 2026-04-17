//! URL validation + mirror list parsing tests.

use super::*;

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
