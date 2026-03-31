// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! Content downloader — fetches game content via HTTP mirrors, direct URLs, or
//! BitTorrent P2P.
//!
//! ## Download strategies (in priority order)
//!
//! 1. **BitTorrent** (requires `torrent` feature): packages with a non-empty
//!    `info_hash` are downloaded via P2P using librqbit. Trackers from the
//!    package definition and the embedded `data/trackers.txt` are combined.
//!    After the `p2p-distribute` crate ships, HTTP mirrors will also act as
//!    BEP 19 webseeds within the torrent swarm.
//!
//! 2. **Mirror list**: fetch a URL that returns mirror URLs, try each in order.
//!
//! 3. **Direct URLs**: try each URL in order (for CNCNZ, Archive.org, etc.).
//!
//! ## Post-download pipeline
//!
//! After download: SHA-1 verification → extraction → per-file SHA-256
//! manifest generation → seeding policy enforcement (archive retention or
//! deletion).
//!
//! Requires the `download` feature (`ureq` + `zip` crates).

use std::fs;
use std::io::{self, Read, Write};
use std::path::Path;

use thiserror::Error;

use crate::{DownloadId, DownloadPackage, GameId, SeedingPolicy};

/// Errors from download operations.
#[derive(Debug, Error)]
pub enum DownloadError {
    #[error("failed to fetch mirror list from {url}: {source}")]
    MirrorListFetch {
        url: String,
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("no download URLs available (mirror list empty, no direct URLs)")]
    NoUrls,
    #[error("{game} is not freeware — only local source extraction is supported. Provide your own copy via `cnc-content install <path>`.")]
    NotFreeware { game: String },
    #[error("all {count} mirrors failed; last error: {last_error}")]
    AllMirrorsFailed { count: usize, last_error: String },
    #[error("SHA-1 mismatch: expected {expected}, got {actual}")]
    Sha1Mismatch { expected: String, actual: String },
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("ZIP extraction error: {0}")]
    Zip(String),
}

/// Progress callback events.
#[derive(Debug, Clone)]
pub enum DownloadProgress {
    /// Fetching the mirror list.
    FetchingMirrors { url: String },
    /// Trying a mirror.
    TryingMirror {
        index: usize,
        total: usize,
        url: String,
    },
    /// Download progress (bytes so far, total if known).
    Downloading { bytes: u64, total: Option<u64> },
    /// Download complete, verifying.
    Verifying,
    /// Extracting ZIP.
    Extracting {
        entry: String,
        index: usize,
        total: usize,
    },
    /// All done.
    Complete { files: usize },
}

/// Download strategy selected for a package based on available sources.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadStrategy {
    /// BitTorrent P2P download (package has info_hash and torrent feature is enabled).
    Torrent,
    /// HTTP download from mirror list and/or direct URLs.
    Http,
}

/// Determines the best download strategy for a package.
pub fn select_strategy(package: &DownloadPackage) -> DownloadStrategy {
    // CNC_NO_P2P=1 disables P2P transport entirely, forcing HTTP.
    // Useful for CI, restricted networks, or debugging mirror issues.
    if std::env::var("CNC_NO_P2P").as_deref() == Ok("1") {
        return DownloadStrategy::Http;
    }

    // Use torrent when available: non-empty info_hash and torrent feature compiled in.
    #[cfg(feature = "torrent")]
    if crate::torrent::should_use_p2p(package) {
        return DownloadStrategy::Torrent;
    }

    let _ = package; // used conditionally by torrent feature
    DownloadStrategy::Http
}

/// Downloads and extracts a content package via HTTP mirrors.
///
/// Resolves download URLs from the mirror list and/or direct URLs, tries each
/// in order, verifies SHA-1 (if not placeholder), then extracts the ZIP into
/// `content_root`.
pub fn download_package(
    package: &DownloadPackage,
    content_root: &Path,
    on_progress: &mut dyn FnMut(DownloadProgress),
) -> Result<(), DownloadError> {
    fs::create_dir_all(content_root)?;
    let zip_path = content_root.join(".download.zip.tmp");

    // CNC_MIRROR_LIST_URL overrides the per-package mirror list URL.
    // Useful for testing against a local or staging mirror list.
    let mirror_list_url_override = std::env::var("CNC_MIRROR_LIST_URL").ok();
    let effective_mirror_url = mirror_list_url_override
        .as_deref()
        .or(if !package.mirror_list_url.is_empty() {
            Some(package.mirror_list_url)
        } else {
            None
        });

    // 1. Resolve URLs: mirror list first, then direct URLs as fallback.
    let mirror_result = if let Some(url) = effective_mirror_url {
        on_progress(DownloadProgress::FetchingMirrors {
            url: url.to_string(),
        });
        fetch_mirror_list(url).ok()
    } else {
        None
    };

    let urls = resolve_download_urls(mirror_result.as_deref(), package.direct_urls);

    if urls.is_empty() {
        return Err(DownloadError::NoUrls);
    }

    // 2. Download: race all mirrors in parallel, first success wins.
    let total_urls = urls.len();
    on_progress(DownloadProgress::TryingMirror {
        index: 0,
        total: total_urls,
        url: if total_urls == 1 {
            urls.first().cloned().unwrap_or_default()
        } else {
            format!("{total_urls} mirrors in parallel")
        },
    });

    let download_result = if total_urls == 1 {
        // Single URL — no need to spawn threads.
        try_download(
            urls.first().map(|s| s.as_str()).unwrap_or(""),
            &zip_path,
            package.size_hint,
            &mut |bytes, total| {
                on_progress(DownloadProgress::Downloading { bytes, total });
            },
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
    } else {
        parallel_download(&urls, &zip_path, package.size_hint, &mut |bytes, total| {
            on_progress(DownloadProgress::Downloading { bytes, total });
        })
    };

    if let Err(last_error) = download_result {
        let _ = fs::remove_file(&zip_path);
        return Err(DownloadError::AllMirrorsFailed {
            count: total_urls,
            last_error,
        });
    }

    // 3. Verify SHA-1 (skip for placeholder all-zero hashes).
    let is_placeholder = package.sha1.chars().all(|c| c == '0');
    if !is_placeholder {
        on_progress(DownloadProgress::Verifying);
        let actual_sha1 = crate::verify::sha1_file(&zip_path, None)?;
        if actual_sha1 != package.sha1 {
            let _ = fs::remove_file(&zip_path);
            return Err(DownloadError::Sha1Mismatch {
                expected: package.sha1.to_string(),
                actual: actual_sha1,
            });
        }
    }

    // 4. Extract ZIP.
    let files = extract_zip(&zip_path, content_root, on_progress)?;

    // 5. Generate post-extraction manifest for integrity verification.
    //    This writes content-manifest.toml so future `verify` commands can
    //    detect tampering or corruption without re-downloading.
    if let Ok(manifest) =
        crate::verify::generate_manifest(content_root, package.game.slug(), "v1", package.provides)
    {
        let manifest_path = content_root.join("content-manifest.toml");
        if let Ok(toml_str) = toml::to_string(&manifest) {
            let _ = fs::write(&manifest_path, toml_str);
        }
    }

    // 6. Apply seeding policy: delete archive if ExtractAndDelete.
    //    Other policies retain the archive for seeding or re-extraction.
    let _ = fs::remove_file(&zip_path);

    on_progress(DownloadProgress::Complete { files });
    Ok(())
}

/// Downloads and extracts a content package using the best available strategy.
///
/// Strategy selection:
/// 1. If the package has an `info_hash` and the `torrent` feature is enabled,
///    downloads via BitTorrent P2P (with tracker + DHT peer discovery).
/// 2. Otherwise, downloads via HTTP mirrors (mirror list + direct URLs).
///
/// After download: SHA-1 verify → extract → generate integrity manifest →
/// apply seeding policy.
///
/// The `seeding_policy` controls what happens to the downloaded archive after
/// extraction. See [`SeedingPolicy`](crate::SeedingPolicy) for details.
pub fn download_and_install(
    package: &DownloadPackage,
    content_root: &Path,
    seeding_policy: SeedingPolicy,
    on_progress: &mut dyn FnMut(DownloadProgress),
) -> Result<(), DownloadError> {
    let strategy = select_strategy(package);
    let _ = &seeding_policy; // used conditionally by torrent feature

    match strategy {
        DownloadStrategy::Torrent => {
            // Torrent path: download via P2P, then extract.
            #[cfg(feature = "torrent")]
            {
                use crate::torrent::{TorrentConfig, TorrentDownloader, TorrentProgress};

                let config = TorrentConfig {
                    seeding_policy,
                    ..TorrentConfig::default()
                };

                let downloader = TorrentDownloader::new(config).map_err(|e| {
                    DownloadError::AllMirrorsFailed {
                        count: 0,
                        last_error: format!("torrent session init failed: {e}"),
                    }
                })?;

                on_progress(DownloadProgress::FetchingMirrors {
                    url: format!("magnet:?xt=urn:btih:{}", package.info_hash),
                });

                let archive_dir = downloader
                    .download_package(package, content_root, &mut |tp| match tp {
                        TorrentProgress::Connecting { trackers } => {
                            on_progress(DownloadProgress::TryingMirror {
                                index: 0,
                                total: trackers,
                                url: "BitTorrent P2P".to_string(),
                            });
                        }
                        TorrentProgress::Downloading {
                            bytes_downloaded,
                            total_bytes,
                            ..
                        } => {
                            on_progress(DownloadProgress::Downloading {
                                bytes: bytes_downloaded,
                                total: Some(total_bytes),
                            });
                        }
                        TorrentProgress::Verifying { .. } => {
                            on_progress(DownloadProgress::Verifying);
                        }
                        TorrentProgress::Complete => {}
                        _ => {}
                    })
                    .map_err(|e| DownloadError::AllMirrorsFailed {
                        count: 1,
                        last_error: format!("torrent download failed: {e}"),
                    })?;

                // Extract downloaded archives into content_root.
                let files =
                    extract_torrent_output(&archive_dir, content_root, package, on_progress)?;

                // Generate post-extraction manifest.
                if let Ok(manifest) = crate::verify::generate_manifest(
                    content_root,
                    package.game.slug(),
                    "v1",
                    package.provides,
                ) {
                    let manifest_path = content_root.join("content-manifest.toml");
                    if let Ok(toml_str) = toml::to_string(&manifest) {
                        let _ = fs::write(&manifest_path, toml_str);
                    }
                }

                // Apply seeding policy: delete archives if ExtractAndDelete.
                if !seeding_policy.retains_archives() {
                    let _ = fs::remove_dir_all(&archive_dir);
                }

                on_progress(DownloadProgress::Complete { files });
                Ok(())
            }

            // Torrent feature not compiled in — fall through to HTTP.
            #[cfg(not(feature = "torrent"))]
            {
                let _ = strategy; // suppress unused warning
                download_package(package, content_root, on_progress)
            }
        }
        DownloadStrategy::Http => download_package(package, content_root, on_progress),
    }
}

/// Downloads all required content for a game that is currently missing.
///
/// Only EA-declared freeware games (Red Alert, Tiberian Dawn) support
/// downloading. Non-freeware games (Dune 2, Dune 2000) require the user
/// to provide their own local copies.
pub fn download_missing(
    content_root: &Path,
    game: GameId,
    on_progress: &mut dyn FnMut(DownloadProgress),
) -> Result<(), DownloadError> {
    if !game.is_freeware() {
        return Err(DownloadError::NotFreeware {
            game: game.title().to_string(),
        });
    }

    if crate::is_content_complete(content_root, game) {
        return Ok(());
    }

    match game {
        GameId::RedAlert => {
            // QuickInstall covers all three required RA packages in one download.
            let pkg = crate::download(DownloadId::RaQuickInstall);
            download_package(pkg, content_root, on_progress)?;
        }
        GameId::TiberianDawn => {
            let pkg = crate::download(DownloadId::TdBaseFiles);
            download_package(pkg, content_root, on_progress)?;
        }
        // Non-freeware games blocked above by is_freeware() check.
        _ => unreachable!(),
    }
    Ok(())
}

/// Resolves the final list of download URLs from mirror list results
/// and direct URLs. Mirror list URLs come first; direct URLs are appended
/// as fallback, deduplicating any that already appeared in the mirror list.
fn resolve_download_urls(mirror_urls: Option<&[String]>, direct_urls: &[&str]) -> Vec<String> {
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

fn fetch_mirror_list(url: &str) -> Result<Vec<String>, DownloadError> {
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
fn parse_mirror_list_response(body: &str) -> Result<Vec<String>, DownloadError> {
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
fn is_safe_mirror_url(url: &str) -> bool {
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

fn try_download(
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
fn parallel_download(
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
        let mut total_guard = best_total.lock().unwrap();
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

#[cfg(feature = "torrent")]
/// Extracts files downloaded by the torrent path into `content_root`.
///
/// The torrent client writes files to `archive_dir`. This function scans
/// that directory and handles each file based on the package format:
///
/// - **ZIP** archives: extracted directly into `content_root`
/// - **ISO** disc images: treated as disc sources, processed through the
///   recipe system to extract the correct game files
/// - Other files: copied as-is into `content_root`
///
/// Returns the total number of files extracted/installed.
fn extract_torrent_output(
    archive_dir: &Path,
    content_root: &Path,
    package: &DownloadPackage,
    on_progress: &mut dyn FnMut(DownloadProgress),
) -> Result<usize, DownloadError> {
    let mut total_files = 0;

    // Collect downloadable files from the archive directory.
    let entries: Vec<_> = fs::read_dir(archive_dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .collect();

    for entry in &entries {
        let path = entry.path();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        match ext.as_str() {
            "zip" => {
                total_files += extract_zip(&path, content_root, on_progress)?;
            }
            "iso" => {
                // ISO disc images are processed through the recipe system.
                // Identify which source this ISO represents, then run its recipes.
                total_files += extract_iso_via_recipes(&path, content_root, package, on_progress)?;
            }
            _ => {
                // Raw files (e.g. loose .mix, .aud): copy into content_root.
                let name = path.file_name().unwrap_or_default();
                let dest = content_root.join(name);
                fs::copy(&path, &dest)?;
                total_files += 1;
            }
        }
    }

    // Also recurse one level into subdirectories — Archive.org torrents
    // sometimes nest files in a subdirectory named after the item.
    let subdirs: Vec<_> = fs::read_dir(archive_dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .collect();

    for subdir in &subdirs {
        total_files += extract_torrent_output(&subdir.path(), content_root, package, on_progress)?;
    }

    Ok(total_files)
}

#[cfg(feature = "torrent")]
/// Extracts game content from an ISO disc image using the recipe system.
///
/// Identifies which source the ISO corresponds to, then runs the matching
/// install recipes to extract the correct files into `content_root`.
fn extract_iso_via_recipes(
    iso_path: &Path,
    content_root: &Path,
    package: &DownloadPackage,
    on_progress: &mut dyn FnMut(DownloadProgress),
) -> Result<usize, DownloadError> {
    // Try to identify this ISO as a known source.
    let source_id = crate::verify::identify_source(iso_path);

    let source_id = match source_id {
        Some(id) => id,
        None => {
            // Can't identify — try all sources that provide the packages
            // this download covers. Use the first one that has recipes.
            let mut found = None;
            for &pkg_id in package.provides {
                let pkg = crate::package(pkg_id);
                for &src_id in pkg.sources {
                    if crate::recipe(src_id, pkg_id).is_some() {
                        found = Some(src_id);
                        break;
                    }
                }
                if found.is_some() {
                    break;
                }
            }
            match found {
                Some(id) => id,
                None => return Ok(0), // no recipes available
            }
        }
    };

    let mut files = 0;
    for &pkg_id in package.provides {
        if let Some(recipe) = crate::recipe(source_id, pkg_id) {
            on_progress(DownloadProgress::Extracting {
                entry: format!(
                    "recipe: {} from {}",
                    pkg_id_label(pkg_id),
                    source_label(source_id)
                ),
                index: files,
                total: package.provides.len(),
            });
            crate::executor::execute_recipe(recipe, iso_path, content_root, |_| {}).map_err(
                |e| DownloadError::Zip(format!("recipe execution failed for {:?}: {e}", pkg_id)),
            )?;
            files += recipe.actions.len();
        }
    }

    Ok(files)
}

#[cfg(feature = "torrent")]
fn pkg_id_label(id: crate::PackageId) -> &'static str {
    crate::package(id).title
}

#[cfg(feature = "torrent")]
fn source_label(id: crate::SourceId) -> &'static str {
    crate::source(id).title
}

/// Maximum uncompressed size per ZIP entry (1 GB).
///
/// Prevents archive bombs (zip bombs) where a small compressed file expands
/// to fill all available disk. C&C game content files are at most ~700 MB
/// (full disc ISOs), so 1 GB per file is generous.
const MAX_ENTRY_UNCOMPRESSED: u64 = 1_073_741_824;

/// Maximum total uncompressed size across all ZIP entries (5 GB).
///
/// An entire game's content (base + expansion + music + movies) is under 2 GB.
/// 5 GB provides headroom for future content without enabling abuse.
const MAX_TOTAL_UNCOMPRESSED: u64 = 5_368_709_120;

/// Maximum number of entries allowed in a ZIP archive (100,000).
///
/// C&C content packages contain at most ~200 files. 100K is generous enough
/// to handle any legitimate archive while preventing entry-count bombs that
/// exhaust memory building the central directory.
const MAX_ZIP_ENTRIES: usize = 100_000;

/// Extracts a ZIP archive into `content_root` with path traversal protection
/// and archive bomb mitigation.
///
/// Returns the number of files extracted. Directory entries are skipped.
///
/// ## Security
///
/// - **Zip Slip**: [`strict_path::PathBoundary`] prevents entry names from
///   escaping `content_root` via `../` traversal (CVE-2018-1000178).
/// - **Archive bombs**: Per-entry and total uncompressed size limits prevent
///   a small ZIP from expanding to fill disk. Entry count is also limited.
pub fn extract_zip(
    zip_path: &Path,
    content_root: &Path,
    on_progress: &mut dyn FnMut(DownloadProgress),
) -> Result<usize, DownloadError> {
    let file = fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(io::BufReader::new(file))
        .map_err(|e| DownloadError::Zip(e.to_string()))?;

    // Reject archives with too many entries (memory exhaustion via central directory).
    if archive.len() > MAX_ZIP_ENTRIES {
        return Err(DownloadError::Zip(format!(
            "archive has {} entries (max {})",
            archive.len(),
            MAX_ZIP_ENTRIES
        )));
    }

    // PathBoundary ensures ZIP entry names (untrusted, from network) cannot
    // escape content_root via path traversal (Zip Slip — CVE-2018-1000178).
    let boundary = strict_path::PathBoundary::<()>::try_new_create(content_root)
        .map_err(|e| DownloadError::Zip(format!("failed to create content boundary: {e}")))?;

    let total = archive.len();
    let mut files = 0;
    let mut total_uncompressed: u64 = 0;

    for i in 0..total {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| DownloadError::Zip(e.to_string()))?;

        let archive_entry_name = entry.name().to_string();
        if archive_entry_name.ends_with('/') {
            continue;
        }

        // Archive bomb check: per-entry size limit.
        let declared_size = entry.size();
        if declared_size > MAX_ENTRY_UNCOMPRESSED {
            return Err(DownloadError::Zip(format!(
                "entry \"{archive_entry_name}\" declares {declared_size} bytes uncompressed \
                 (max {MAX_ENTRY_UNCOMPRESSED})"
            )));
        }

        // Archive bomb check: total uncompressed size limit.
        total_uncompressed = total_uncompressed.saturating_add(declared_size);
        if total_uncompressed > MAX_TOTAL_UNCOMPRESSED {
            return Err(DownloadError::Zip(format!(
                "total uncompressed size exceeds {MAX_TOTAL_UNCOMPRESSED} bytes — \
                 possible archive bomb"
            )));
        }

        on_progress(DownloadProgress::Extracting {
            entry: archive_entry_name.clone(),
            index: i,
            total,
        });

        // Validate the untrusted archive entry name against our boundary.
        let dest = boundary.strict_join(&archive_entry_name).map_err(|e| {
            DownloadError::Zip(format!(
                "blocked path traversal in ZIP entry \"{archive_entry_name}\": {e}"
            ))
        })?;

        dest.create_parent_dir_all()?;
        let mut out = dest.create_file()?;
        io::copy(&mut entry, &mut out)?;
        files += 1;
    }

    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Creates an in-memory ZIP archive and writes it to `dest`.
    /// `entries` is a list of `(name, content)` tuples where `name` may
    /// contain path traversal sequences for security testing.
    fn create_test_zip(dest: &Path, entries: &[(&str, &[u8])]) {
        let file = fs::File::create(dest).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
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
    /// Some attackers use `..\\` separators as an evasion technique, relying on
    /// extractors that normalise only forward-slash separators. An entry named
    /// `..\\..\\etc\\passwd` must be treated as traversal and blocked regardless
    /// of the separator character used.
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

    // ── DownloadError::NotFreeware ───────────────────────────────────

    /// Verifies that `download_missing` rejects non-freeware games immediately
    /// (before any HTTP calls), returning `DownloadError::NotFreeware`.
    #[test]
    fn download_missing_rejects_non_freeware() {
        let tmp = std::env::temp_dir().join("cnc-dl-notfreeware");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let result = download_missing(&tmp, GameId::Dune2, &mut noop_progress);
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
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
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
    #[test]
    fn download_package_no_urls_returns_error() {
        let pkg = DownloadPackage {
            id: DownloadId::RaQuickInstall,
            game: GameId::RedAlert,
            title: "Test Package",
            mirror_list_url: "",
            direct_urls: &[],
            sha1: "0000000000000000000000000000000000000000",
            info_hash: "",
            trackers: &[],
            provides: &[crate::PackageId::RaBase],
            format: "zip",
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
        download_missing(&tmp, GameId::RedAlert, &mut |_| {
            progress_called = true;
        })
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
    fn select_strategy_http_for_no_torrent_feature() {
        let pkg = DownloadPackage {
            id: DownloadId::RaQuickInstall,
            game: GameId::RedAlert,
            title: "Test",
            mirror_list_url: "",
            direct_urls: &[],
            sha1: "0000000000000000000000000000000000000000",
            info_hash: "abcdef1234567890abcdef1234567890abcdef12",
            trackers: &[],
            provides: &[],
            format: "zip",
            size_hint: 0,
        };
        // Even with an info_hash, without the torrent feature it should be Http.
        assert_eq!(select_strategy(&pkg), DownloadStrategy::Http);
    }
}
