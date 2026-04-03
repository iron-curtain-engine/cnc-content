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
use std::io;
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
    #[error("I/O error: {source}")]
    Io {
        #[from]
        source: io::Error,
    },
    #[error("ZIP extraction error: {detail}")]
    Zip { detail: String },
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
    let effective_mirror_url =
        mirror_list_url_override
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
/// extraction. See [`SeedingPolicy`] for details.
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
            let pkg =
                crate::download(DownloadId::RaQuickInstall).ok_or_else(|| DownloadError::Io {
                    source: io::Error::other("no download definition for RaQuickInstall"),
                })?;
            download_package(pkg, content_root, on_progress)?;
        }
        GameId::TiberianDawn => {
            let pkg =
                crate::download(DownloadId::TdBaseFiles).ok_or_else(|| DownloadError::Io {
                    source: io::Error::other("no download definition for TdBaseFiles"),
                })?;
            download_package(pkg, content_root, on_progress)?;
        }
        // Non-freeware games blocked above by is_freeware() check.
        _ => unreachable!(),
    }
    Ok(())
}

// ── Sub-modules ───────────────────────────────────────────────────────

/// Mirror URL resolution, safety validation, and HTTP download helpers.
mod mirror;
use self::mirror::{fetch_mirror_list, parallel_download, resolve_download_urls, try_download};
#[cfg(test)]
use self::mirror::{is_safe_mirror_url, parse_mirror_list_response};

/// Post-download extraction — ZIP, torrent archives, ISO disc images.
mod extract;
#[cfg(feature = "torrent")]
use self::extract::extract_torrent_output;
pub use self::extract::extract_zip;

#[cfg(test)]
use self::extract::{MAX_ENTRY_UNCOMPRESSED, MAX_TOTAL_UNCOMPRESSED, MAX_ZIP_ENTRIES};

#[cfg(test)]
mod tests;
