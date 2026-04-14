// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025-present Iron Curtain contributors

//! Mirror URL resolution, safety validation, and HTTP download helpers.
//!
//! Split from downloader/mod.rs to keep the public download API separate
//! from the implementation details of HTTP mirror racing, URL validation, and
//! progress reporting.
//!
//! ## Functions
//!
//! - **resolve_download_urls** � merges mirror list results with direct URLs
//! - **fetch_mirror_list** / **parse_mirror_list_response** � fetches and
//!   validates a newline-delimited list of mirror URLs
//! - **is_safe_mirror_url** � SSRF guard: rejects file://, localhost, RFC-1918
//! - **make_agent** / **try_download** � single-mirror download with progress
//! - **parallel_download** � races all mirrors simultaneously, first wins

use std::fs;
use std::io::{Read, Write};
use std::path::Path;

use super::DownloadError;

/// Maximum file pre-allocation when `size_hint` is unknown (2 GB).
///
/// Prevents a spoofed HTTP `Content-Length` header from allocating
/// unbounded disk space. The largest C&C disc ISOs are ~700 MB; 2 GB
/// provides generous headroom while bounding the worst case.
pub(super) const MAX_PREALLOC_SIZE: u64 = 2 * 1024 * 1024 * 1024;

pub(super) fn resolve_download_urls(
    mirror_urls: Option<&[String]>,
    direct_urls: &[&str],
) -> Vec<String> {
    let mut urls = Vec::new();
    if let Some(mirrors) = mirror_urls {
        urls.extend(mirrors.iter().cloned());
    }
    for &url in direct_urls {
        if !urls.iter().any(|u| u == url) {
            urls.push(url.to_string());
        }
    }
    urls
}

pub(super) fn fetch_mirror_list(url: &str) -> Result<Vec<String>, DownloadError> {
    let body = ureq::get(url)
        .call()
        .map_err(|e| DownloadError::MirrorListFetch {
            url: url.to_string(),
            source: Box::new(e),
        })?
        .into_body()
        .read_to_string()
        .map_err(|e| DownloadError::MirrorListFetch {
            url: url.to_string(),
            source: Box::new(e),
        })?;

    parse_mirror_list_response(&body)
}

/// Parses a mirror list response body into validated mirror URLs.
///
/// Each line is trimmed and filtered: blank lines are skipped, and
/// [`is_safe_mirror_url`] rejects non-HTTP(S), localhost, private networks,
/// header injection attempts, and bare hostnames.
///
/// Returns `Err(NoUrls)` if no safe URLs survive filtering.
pub(super) fn parse_mirror_list_response(body: &str) -> Result<Vec<String>, DownloadError> {
    let mirrors: Vec<String> = body
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty() && is_safe_mirror_url(l))
        .collect();

    if mirrors.is_empty() {
        return Err(DownloadError::NoUrls);
    }

    Ok(mirrors)
}

/// Validates that a mirror URL is safe to fetch from.
///
/// Rejects:
/// - Non-HTTP(S) schemes (`file://`, `ftp://`, `data:`, etc.)
/// - URLs containing newlines/carriage returns (header injection)
/// - Localhost and private-network addresses (SSRF prevention)
///
/// This is a defense against a compromised or malicious mirror list
/// server returning URLs that cause the client to probe internal
/// services or read local files.
pub(super) fn is_safe_mirror_url(url: &str) -> bool {
    // Must be HTTP or HTTPS.
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return false;
    }

    // Reject newlines (HTTP header injection).
    if url.contains('\n') || url.contains('\r') {
        return false;
    }

    // Extract the host portion (between :// and the next / or end).
    let after_scheme = if let Some(rest) = url.strip_prefix("https://") {
        rest
    } else if let Some(rest) = url.strip_prefix("http://") {
        rest
    } else {
        return false;
    };
    let host = after_scheme.split('/').next().unwrap_or("");

    // ── Strip port, handling IPv6 brackets ──────────────────────
    //
    // IPv6 URLs use bracket notation: `[::1]:8080`. Splitting on `:`
    // naïvely would break the address. Extract the bracketed address
    // for IPv6, otherwise split on `:` for IPv4/hostname.
    let host_no_port = if host.starts_with('[') {
        // IPv6: extract `[addr]`, strip brackets for comparisons below.
        let bracket_end = host.find(']').map_or(host.len(), |i| i);
        host.get(1..bracket_end).unwrap_or(host)
    } else {
        host.split(':').next().unwrap_or(host)
    };

    // Reject localhost and loopback (IPv4 + IPv6).
    if host_no_port == "localhost"
        || host_no_port == "127.0.0.1"
        || host_no_port == "::1"
        || host_no_port == "0.0.0.0"
    {
        return false;
    }

    // ── Reject IPv6 private/reserved ranges ─────────────────────
    //
    // IPv4-mapped IPv6 addresses (::ffff:a.b.c.d) can bypass IPv4
    // denylist checks. Also block IPv6-native private ranges.
    let host_lower = host_no_port.to_ascii_lowercase();

    // IPv4-mapped IPv6 loopback and private ranges (::ffff:127.x, ::ffff:10.x, etc.)
    if let Some(mapped) = host_lower
        .strip_prefix("::ffff:")
        .or_else(|| host_lower.strip_prefix("0:0:0:0:0:ffff:"))
    {
        // Delegate to the same IPv4 private-range checks below.
        if mapped == "127.0.0.1"
            || mapped.starts_with("10.")
            || mapped.starts_with("192.168.")
            || mapped.starts_with("169.254.")
        {
            return false;
        }
        if mapped.starts_with("172.") {
            if let Some(second_octet) = mapped.split('.').nth(1) {
                if let Ok(n) = second_octet.parse::<u8>() {
                    if (16..=31).contains(&n) {
                        return false;
                    }
                }
            }
        }
    }

    // IPv6 link-local (fe80::/10).
    if host_lower.starts_with("fe80") {
        return false;
    }

    // IPv6 unique local address / ULA (fc00::/7 — covers fc00:: and fd00::).
    if host_lower.starts_with("fc") || host_lower.starts_with("fd") {
        return false;
    }

    // Reject private/link-local IPv4 ranges.
    if host_no_port.starts_with("10.")
        || host_no_port.starts_with("192.168.")
        || host_no_port.starts_with("169.254.")
    {
        return false;
    }
    // 172.16.0.0/12
    if host_no_port.starts_with("172.") {
        if let Some(second_octet) = host_no_port.split('.').nth(1) {
            if let Ok(n) = second_octet.parse::<u8>() {
                if (16..=31).contains(&n) {
                    return false;
                }
            }
        }
    }

    // Host must contain at least one dot (reject bare hostnames like "internal").
    // Bracket-enclosed hosts are IPv6 addresses — they use colons, not dots.
    if !host.starts_with('[') && !host_no_port.contains('.') {
        return false;
    }

    true
}

/// Maximum number of HTTP redirects to follow per request.
///
/// Reduced from ureq's default of 10 to limit redirect-chain attacks.
/// CDN redirects (e.g. GitHub Releases → objects.githubusercontent.com)
/// typically need only 1–2 hops.
pub(super) const MAX_REDIRECTS: u32 = 5;

/// Creates a ureq agent with hardened defaults.
///
/// Reading the env var at the call site avoids a global cache, so the value
/// can be changed between calls in tests without restarts.
///
/// Security hardening applied:
/// - `max_redirects(5)` — limits redirect-chain abuse (default is 10).
///   Note: ureq follows redirects automatically and does NOT re-validate
///   redirect targets against `is_safe_mirror_url()`. Since all initial
///   URLs are either compile-time trusted (from `downloads.toml`) or
///   validated at parse time via `is_safe_mirror_url()`, redirect-based
///   SSRF requires compromising a trusted origin — acceptable residual risk.
/// - `https_only(true)` — prevents HTTP-downgrade on redirect. All mirrors
///   use HTTPS; a redirect to plain HTTP would indicate compromise.
fn make_agent() -> ureq::Agent {
    let secs = std::env::var("CNC_DOWNLOAD_TIMEOUT")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(300);
    ureq::config::Config::builder()
        .timeout_global(Some(std::time::Duration::from_secs(secs)))
        .max_redirects(MAX_REDIRECTS)
        .https_only(true)
        .build()
        .new_agent()
}

/// Downloads a file from a single URL to `dest`, computing SHA-1 inline.
///
/// Returns `(bytes_written, sha1_hex)`. The SHA-1 is computed during the
/// download loop — the same bytes that go to disk are fed to the hasher,
/// eliminating the need for a separate verification read pass. This is
/// the BLAKE3-inspired "fused pipeline" pattern: hash and write in a
/// single pass over the data.
pub(super) fn try_download(
    url: &str,
    dest: &Path,
    size_hint: u64,
    on_bytes: &mut dyn FnMut(u64, Option<u64>),
) -> Result<(u64, String), Box<dyn std::error::Error + Send + Sync>> {
    use sha1::Digest;

    let resp = make_agent().get(url).call()?;

    // Try to get content-length from the response, fall back to size_hint.
    let content_length = resp
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .or(if size_hint > 0 { Some(size_hint) } else { None });

    let mut body = resp.into_body().into_reader();
    let mut file = fs::File::create(dest)?;
    // Pre-allocate the output file to avoid fragmentation and enable
    // contiguous allocation. This is especially impactful for 500-700 MB
    // disc ISOs where scattered allocation adds measurable overhead.
    //
    // Security (CWE-400 / CWE-409): cap the pre-allocation at the trusted
    // size_hint to prevent a malicious or MITM'd server from exhausting
    // disk via a spoofed Content-Length header. On NTFS (Windows), set_len
    // allocates real clusters — a 100 GB Content-Length would fill the disk.
    // The size_hint comes from the compile-time package definition and is
    // the trusted expected size. If size_hint is 0 (unknown), cap at
    // MAX_PREALLOC_SIZE to bound the worst case.
    if let Some(len) = content_length {
        let cap = if size_hint > 0 {
            size_hint.saturating_mul(2)
        } else {
            MAX_PREALLOC_SIZE
        };
        let _ = file.set_len(len.min(cap));
    }
    let mut buf = [0u8; 65536];
    let mut total: u64 = 0;
    // Track which MB we last reported to avoid spamming the callback.
    let mut last_reported_mb: u64 = 0;
    // Fused SHA-1: hash bytes as they arrive from the network, before
    // writing to disk. Eliminates one full sequential re-read pass.
    let mut hasher = sha1::Sha1::new();

    loop {
        let n = body.read(&mut buf)?;
        if n == 0 {
            break;
        }
        let slice = buf.get(..n).unwrap_or(&[]);
        hasher.update(slice);
        file.write_all(slice)?;
        total += n as u64;
        let current_mb = total / (1024 * 1024);
        if current_mb > last_reported_mb {
            last_reported_mb = current_mb;
            on_bytes(total, content_length);
        }
    }

    // Final report so callers see the total.
    on_bytes(total, content_length);
    // Truncate the file to the actual bytes written. If Content-Length was
    // larger than the data received (connection dropped, or malicious header),
    // the trailing sparse/zero region is removed. This prevents disk waste
    // from a spoofed Content-Length even if SHA-1 verification later deletes
    // the file — the disk exhaustion would already have occurred.
    file.set_len(total)?;
    file.flush()?;
    let sha1_hex = crate::verify::hex_encode(hasher.finalize().as_slice());
    Ok((total, sha1_hex))
}

/// Downloads from multiple mirror URLs in parallel, racing all threads.
///
/// Each thread downloads to its own temporary file while computing SHA-1
/// inline. The first thread to complete successfully has its file renamed
/// to `dest` and its SHA-1 hash returned. All other threads are signalled
/// to abort via a shared `AtomicBool`. If all threads fail, returns the
/// last error message.
pub(super) fn parallel_download(
    urls: &[String],
    dest: &Path,
    size_hint: u64,
    on_bytes: &mut dyn FnMut(u64, Option<u64>),
) -> Result<String, String> {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{mpsc, Arc, Mutex};

    let done = Arc::new(AtomicBool::new(false));
    // Channel for the winning thread to send its temp file path + SHA-1.
    let (tx, rx) = mpsc::channel::<Result<(std::path::PathBuf, String), String>>();
    // Shared progress: tracks the best (highest) byte count across threads.
    let best_bytes = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let best_total = Arc::new(Mutex::new(None::<u64>));

    std::thread::scope(|scope| {
        for (i, url) in urls.iter().enumerate() {
            let done = Arc::clone(&done);
            let tx = tx.clone();
            let best_bytes = Arc::clone(&best_bytes);
            let best_total = Arc::clone(&best_total);
            let url = url.clone();
            let tmp_path = dest.with_extension(format!("tmp.{i}"));

            scope.spawn(move || {
                if done.load(Ordering::Relaxed) {
                    return;
                }

                let result = try_download_cancellable(
                    &url,
                    &tmp_path,
                    size_hint,
                    &done,
                    &best_bytes,
                    &best_total,
                );

                match result {
                    Ok((_bytes, sha1)) if !done.swap(true, Ordering::AcqRel) => {
                        // We won the race — send path + fused SHA-1.
                        let _ = tx.send(Ok((tmp_path, sha1)));
                    }
                    Ok(_) => {
                        // Another thread already won; clean up.
                        let _ = std::fs::remove_file(&tmp_path);
                    }
                    Err(e) => {
                        let _ = std::fs::remove_file(&tmp_path);
                        let _ = tx.send(Err(e.to_string()));
                    }
                }
            });
        }

        // Drop our copy of tx so the channel closes when all threads finish.
        drop(tx);

        // Collect results, forwarding progress from the best thread.
        let mut last_error = String::from("no mirrors available");
        let mut winner: Option<(std::path::PathBuf, String)> = None;

        for result in &rx {
            match result {
                Ok(path_and_hash) => {
                    winner = Some(path_and_hash);
                    break;
                }
                Err(e) => {
                    last_error = e;
                }
            }
        }

        // Report final progress from whichever thread got furthest.
        let final_bytes = best_bytes.load(Ordering::Relaxed);
        let final_total = best_total.lock().ok().and_then(|t| *t);
        if final_bytes > 0 {
            on_bytes(final_bytes, final_total);
        }

        if let Some((tmp, sha1)) = winner {
            // Rename the winner's temp file to the final destination.
            if let Err(e) = std::fs::rename(&tmp, dest) {
                // rename can fail cross-device; fall back to copy.
                if let Err(e2) = std::fs::copy(&tmp, dest) {
                    let _ = std::fs::remove_file(&tmp);
                    return Err(format!("rename failed: {e}; copy failed: {e2}"));
                }
                let _ = std::fs::remove_file(&tmp);
            }
            // Clean up any remaining temp files from losing threads.
            for i in 0..urls.len() {
                let tmp = dest.with_extension(format!("tmp.{i}"));
                let _ = std::fs::remove_file(&tmp);
            }
            Ok(sha1)
        } else {
            Err(last_error)
        }
    })
}

/// Like `try_download` but checks `cancel` flag between reads and reports
/// progress via shared atomics for cross-thread progress aggregation.
/// Returns `(bytes_written, sha1_hex)` with SHA-1 computed inline.
fn try_download_cancellable(
    url: &str,
    dest: &Path,
    size_hint: u64,
    cancel: &std::sync::atomic::AtomicBool,
    best_bytes: &std::sync::atomic::AtomicU64,
    best_total: &std::sync::Mutex<Option<u64>>,
) -> Result<(u64, String), Box<dyn std::error::Error + Send + Sync>> {
    use sha1::Digest;
    use std::sync::atomic::Ordering;

    let resp = make_agent().get(url).call()?;

    let content_length = resp
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .or(if size_hint > 0 { Some(size_hint) } else { None });

    if let Some(cl) = content_length {
        let mut total_guard = best_total.lock().unwrap_or_else(|e| e.into_inner());
        if total_guard.is_none() {
            *total_guard = Some(cl);
        }
    }

    let mut body = resp.into_body().into_reader();
    let mut file = fs::File::create(dest)?;
    // Pre-allocate to avoid fragmentation during parallel mirror racing.
    // Security (CWE-400): cap at trusted size_hint to prevent disk
    // exhaustion from spoofed Content-Length (see try_download comment).
    if let Some(len) = content_length {
        let cap = if size_hint > 0 {
            size_hint.saturating_mul(2)
        } else {
            MAX_PREALLOC_SIZE
        };
        let _ = file.set_len(len.min(cap));
    }
    let mut buf = [0u8; 65536];
    let mut total: u64 = 0;
    let mut last_reported_mb: u64 = 0;
    // Fused SHA-1: same bytes that go to disk also feed the hasher.
    let mut hasher = sha1::Sha1::new();

    loop {
        if cancel.load(Ordering::Relaxed) {
            // Another thread won — abort early.
            return Err("cancelled: another mirror succeeded first".into());
        }

        let n = body.read(&mut buf)?;
        if n == 0 {
            break;
        }
        let slice = buf.get(..n).unwrap_or(&[]);
        hasher.update(slice);
        file.write_all(slice)?;
        total += n as u64;

        // Update shared progress if we're ahead.
        let current_mb = total / (1024 * 1024);
        if current_mb > last_reported_mb {
            last_reported_mb = current_mb;
            best_bytes.fetch_max(total, Ordering::Relaxed);
        }
    }

    best_bytes.fetch_max(total, Ordering::Relaxed);
    // Truncate file to actual bytes written — removes sparse holes from
    // Content-Length mismatch (see try_download for full rationale).
    file.set_len(total)?;
    file.flush()?;
    let sha1_hex = crate::verify::hex_encode(hasher.finalize().as_slice());
    Ok((total, sha1_hex))
}

// ── FlashGet-style segmented parallel download ──────────────────────

/// Minimum segment size (1 MB). Segments smaller than this don't benefit
/// from parallelism — HTTP request overhead dominates.
const MIN_SEGMENT_SIZE: u64 = 1024 * 1024;

/// Downloads a file by splitting it into segments and assigning each segment
/// to a different mirror via HTTP Range requests. Returns the SHA-1 hex
/// digest of the assembled file, computed inline during segment assembly.
///
/// ## How it works (FlashGet / Internet Download Manager strategy)
///
/// 1. The file is divided into N segments (one per mirror, minimum 1 MB each).
/// 2. Each thread fetches its assigned byte range via `Range: bytes=start-end`.
/// 3. Segments are written to individual temp files, then assembled in order
///    while a SHA-1 hasher consumes every byte (fused assembly + verification).
/// 4. If a mirror fails mid-segment, another mirror takes over the remaining
///    bytes of that segment.
///
/// This aggregates bandwidth from all mirrors simultaneously — a file served
/// by 5 mirrors at 10 MB/s each downloads at ~50 MB/s total.
///
/// ## Fallback
///
/// If the first mirror returns HTTP 200 (no Range support) instead of 206,
/// we fall back to `parallel_download` (mirror racing) since segmented
/// download is impossible without Range support.
pub(super) fn segmented_download(
    urls: &[String],
    dest: &Path,
    file_size: u64,
    on_bytes: &mut dyn FnMut(u64, Option<u64>),
) -> Result<String, String> {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::Arc;

    let mirror_count = urls.len();
    if mirror_count == 0 {
        return Err("no mirrors available".to_string());
    }

    // Probe first mirror for Range support before committing to segmented.
    if !probe_range_support(urls.first().map(|s| s.as_str()).unwrap_or("")) {
        // Server doesn't support Range requests — fall back to mirror racing.
        return parallel_download(urls, dest, file_size, on_bytes);
    }

    // Split into segments: one per mirror, minimum MIN_SEGMENT_SIZE each.
    let effective_mirrors = std::cmp::min(
        mirror_count,
        std::cmp::max(1, (file_size / MIN_SEGMENT_SIZE) as usize),
    );
    let segment_size = file_size / effective_mirrors as u64;
    let segments: Vec<(u64, u64, usize)> = (0..effective_mirrors)
        .map(|i| {
            let start = i as u64 * segment_size;
            let end = if i == effective_mirrors - 1 {
                file_size.saturating_sub(1) // Last segment gets the remainder.
            } else {
                ((i as u64 + 1) * segment_size).saturating_sub(1)
            };
            (start, end, i)
        })
        .collect();

    // Shared progress tracking across all threads.
    let total_downloaded = Arc::new(AtomicU64::new(0));
    let failed = Arc::new(AtomicBool::new(false));

    // Segment temp files: dest.seg.0, dest.seg.1, etc.
    let seg_paths: Vec<std::path::PathBuf> = (0..effective_mirrors)
        .map(|i| dest.with_extension(format!("seg.{i}")))
        .collect();

    let result: Result<(), String> = std::thread::scope(|scope| {
        let handles: Vec<_> = segments
            .iter()
            .map(|&(start, end, seg_idx)| {
                let url = urls
                    .get(seg_idx % mirror_count)
                    .cloned()
                    .unwrap_or_default();
                let seg_path = seg_paths
                    .get(seg_idx)
                    .cloned()
                    .unwrap_or_else(|| dest.with_extension(format!("seg.{seg_idx}")));
                let total_downloaded = Arc::clone(&total_downloaded);
                let failed = Arc::clone(&failed);

                // Collect fallback mirror URLs for retry (excluding primary).
                let fallback_urls: Vec<String> = urls
                    .iter()
                    .enumerate()
                    .filter(|&(i, _)| i != seg_idx % mirror_count)
                    .map(|(_, u)| u.clone())
                    .collect();

                scope.spawn(move || {
                    // Try primary mirror first, then fall back to others.
                    let result = download_segment(&url, &seg_path, start, end, &total_downloaded);

                    match result {
                        Ok(()) => Ok(()),
                        Err(primary_err) => {
                            // Primary mirror failed — try fallbacks.
                            for fallback_url in &fallback_urls {
                                if failed.load(Ordering::Relaxed) {
                                    break;
                                }
                                if download_segment(
                                    fallback_url,
                                    &seg_path,
                                    start,
                                    end,
                                    &total_downloaded,
                                )
                                .is_ok()
                                {
                                    return Ok(());
                                }
                            }
                            failed.store(true, Ordering::Relaxed);
                            Err(primary_err)
                        }
                    }
                })
            })
            .collect();

        // Wait for all threads and report progress periodically.
        let mut last_reported: u64 = 0;
        let mut all_ok = true;
        let mut last_error = String::new();

        for handle in handles {
            match handle.join() {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    all_ok = false;
                    last_error = e;
                }
                Err(_) => {
                    all_ok = false;
                    last_error = "segment download thread panicked".to_string();
                }
            }
        }

        // Final progress report.
        let final_bytes = total_downloaded.load(Ordering::Relaxed);
        if final_bytes > last_reported {
            last_reported = final_bytes;
            on_bytes(last_reported, Some(file_size));
        }
        let _ = last_reported; // suppress unused after final update

        if !all_ok {
            return Err(last_error);
        }
        Ok(())
    });

    // Report final progress before assembly.
    let total = total_downloaded.load(Ordering::Relaxed);
    on_bytes(total, Some(file_size));

    if let Err(e) = result {
        // Clean up segment files on failure.
        for p in &seg_paths {
            let _ = fs::remove_file(p);
        }
        return Err(e);
    }

    // Assemble segments into the final file, computing SHA-1 during the copy.
    let sha1_hex =
        assemble_segments(&seg_paths, dest).map_err(|e| format!("segment assembly failed: {e}"))?;

    // Clean up segment temp files.
    for p in &seg_paths {
        let _ = fs::remove_file(p);
    }

    Ok(sha1_hex)
}

/// Probes whether a server supports HTTP Range requests by sending a
/// small Range request and checking for 206 Partial Content.
fn probe_range_support(url: &str) -> bool {
    let result = make_agent().get(url).header("Range", "bytes=0-0").call();

    match result {
        Ok(resp) => resp.status().as_u16() == 206,
        Err(_) => false,
    }
}

/// Downloads a single byte-range segment to a file.
///
/// Uses HTTP Range header to request exactly `start..=end` bytes from
/// the mirror. Tracks cumulative progress via the shared atomic counter.
fn download_segment(
    url: &str,
    dest: &Path,
    start: u64,
    end: u64,
    total_downloaded: &std::sync::atomic::AtomicU64,
) -> Result<(), String> {
    use std::sync::atomic::Ordering;

    let range_header = format!("bytes={start}-{end}");
    let expected_len = end.saturating_sub(start).saturating_add(1);

    let resp = make_agent()
        .get(url)
        .header("Range", &range_header)
        .call()
        .map_err(|e| format!("segment {start}-{end} from {url}: {e}"))?;

    let status = resp.status().as_u16();
    if status != 206 {
        return Err(format!(
            "segment {start}-{end}: expected HTTP 206, got {status}"
        ));
    }

    let mut body = resp.into_body().into_reader();
    let mut file = fs::File::create(dest)
        .map_err(|e| format!("create segment file {}: {e}", dest.display()))?;
    let mut buf = [0u8; 65536];
    let mut written: u64 = 0;

    loop {
        let remaining = expected_len.saturating_sub(written);
        if remaining == 0 {
            break;
        }
        let to_read = std::cmp::min(remaining as usize, buf.len());
        let n = body
            .read(buf.get_mut(..to_read).unwrap_or(&mut []))
            .map_err(|e| format!("segment {start}-{end} read: {e}"))?;
        if n == 0 {
            break;
        }
        file.write_all(buf.get(..n).unwrap_or(&[]))
            .map_err(|e| format!("segment {start}-{end} write: {e}"))?;
        written = written.saturating_add(n as u64);
        total_downloaded.fetch_add(n as u64, Ordering::Relaxed);
    }

    file.flush()
        .map_err(|e| format!("segment {start}-{end} flush: {e}"))?;

    if written != expected_len {
        return Err(format!(
            "segment {start}-{end}: expected {expected_len} bytes, got {written}"
        ));
    }

    Ok(())
}

/// Assembles ordered segment files into the final output file, computing
/// SHA-1 inline during the copy.
///
/// Reads each segment sequentially and appends to the destination while
/// feeding every byte to a SHA-1 hasher. This fuses what were previously
/// two separate passes (assembly + verification) into one. Returns the
/// SHA-1 hex digest of the assembled file.
fn assemble_segments(
    seg_paths: &[std::path::PathBuf],
    dest: &Path,
) -> Result<String, std::io::Error> {
    use sha1::Digest;

    let mut output = fs::File::create(dest)?;
    let mut buf = [0u8; 65536];
    let mut hasher = sha1::Sha1::new();

    for seg_path in seg_paths {
        let mut seg_file = fs::File::open(seg_path)?;
        loop {
            let n = seg_file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            let slice = buf.get(..n).unwrap_or(&[]);
            hasher.update(slice);
            output.write_all(slice)?;
        }
    }

    output.flush()?;
    Ok(crate::verify::hex_encode(hasher.finalize().as_slice()))
}
