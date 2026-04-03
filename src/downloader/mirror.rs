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
    // Strip port if present.
    let host_no_port = host.split(':').next().unwrap_or(host);

    // Reject localhost and loopback.
    if host_no_port == "localhost"
        || host_no_port == "127.0.0.1"
        || host_no_port == "[::1]"
        || host_no_port == "0.0.0.0"
    {
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
    if !host_no_port.contains('.') {
        return false;
    }

    true
}

/// Creates a ureq agent with the timeout from `CNC_DOWNLOAD_TIMEOUT` (default 300 s).
///
/// Reading the env var at the call site avoids a global cache, so the value
/// can be changed between calls in tests without restarts.
fn make_agent() -> ureq::Agent {
    let secs = std::env::var("CNC_DOWNLOAD_TIMEOUT")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(300);
    ureq::config::Config::builder()
        .timeout_global(Some(std::time::Duration::from_secs(secs)))
        .build()
        .new_agent()
}

pub(super) fn try_download(
    url: &str,
    dest: &Path,
    size_hint: u64,
    on_bytes: &mut dyn FnMut(u64, Option<u64>),
) -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
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
    let mut buf = [0u8; 65536];
    let mut total: u64 = 0;
    // Track which MB we last reported to avoid spamming the callback.
    let mut last_reported_mb: u64 = 0;

    loop {
        let n = body.read(&mut buf)?;
        if n == 0 {
            break;
        }
        file.write_all(buf.get(..n).unwrap_or(&[]))?;
        total += n as u64;
        let current_mb = total / (1024 * 1024);
        if current_mb > last_reported_mb {
            last_reported_mb = current_mb;
            on_bytes(total, content_length);
        }
    }

    // Final report so callers see the total.
    on_bytes(total, content_length);
    file.flush()?;
    Ok(total)
}

/// Downloads from multiple mirror URLs in parallel, racing all threads.
///
/// Each thread downloads to its own temporary file. The first thread to
/// complete successfully has its file renamed to `dest`. All other threads
/// are signalled to abort via a shared `AtomicBool`. If all threads fail,
/// returns the last error message.
pub(super) fn parallel_download(
    urls: &[String],
    dest: &Path,
    size_hint: u64,
    on_bytes: &mut dyn FnMut(u64, Option<u64>),
) -> Result<(), String> {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{mpsc, Arc, Mutex};

    let done = Arc::new(AtomicBool::new(false));
    // Channel for the winning thread to send its temp file path.
    let (tx, rx) = mpsc::channel::<Result<std::path::PathBuf, String>>();
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
                    Ok(_) if !done.swap(true, Ordering::AcqRel) => {
                        // We won the race.
                        let _ = tx.send(Ok(tmp_path));
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
        let mut winner_path = None;

        for result in &rx {
            match result {
                Ok(path) => {
                    winner_path = Some(path);
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

        if let Some(tmp) = winner_path {
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
            Ok(())
        } else {
            Err(last_error)
        }
    })
}

/// Like `try_download` but checks `cancel` flag between reads and reports
/// progress via shared atomics for cross-thread progress aggregation.
fn try_download_cancellable(
    url: &str,
    dest: &Path,
    size_hint: u64,
    cancel: &std::sync::atomic::AtomicBool,
    best_bytes: &std::sync::atomic::AtomicU64,
    best_total: &std::sync::Mutex<Option<u64>>,
) -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
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
    let mut buf = [0u8; 65536];
    let mut total: u64 = 0;
    let mut last_reported_mb: u64 = 0;

    loop {
        if cancel.load(Ordering::Relaxed) {
            // Another thread won — abort early.
            return Err("cancelled: another mirror succeeded first".into());
        }

        let n = body.read(&mut buf)?;
        if n == 0 {
            break;
        }
        file.write_all(buf.get(..n).unwrap_or(&[]))?;
        total += n as u64;

        // Update shared progress if we're ahead.
        let current_mb = total / (1024 * 1024);
        if current_mb > last_reported_mb {
            last_reported_mb = current_mb;
            best_bytes.fetch_max(total, Ordering::Relaxed);
        }
    }

    best_bytes.fetch_max(total, Ordering::Relaxed);
    file.flush()?;
    Ok(total)
}
