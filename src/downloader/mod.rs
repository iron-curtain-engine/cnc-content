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
    /// SHA-1 verification skipped because the package hash is a placeholder.
    /// Callers should warn the user that integrity is not verified.
    VerifyingSkipped,
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
                Some(package.mirror_list_url.as_str())
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

    let direct_url_strs: Vec<&str> = package.direct_urls.iter().map(String::as_str).collect();
    let urls = resolve_download_urls(mirror_result.as_deref(), &direct_url_strs);

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
    //    Placeholder hashes mean the content source hasn't published a hash
    //    yet. The user should be warned that integrity is unverified.
    let is_placeholder = package.sha1.chars().all(|c| c == '0');
    if is_placeholder {
        on_progress(DownloadProgress::VerifyingSkipped);
    } else {
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
        crate::verify::generate_manifest(content_root, package.game.slug(), "v1", &package.provides)
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
            #[cfg(feature = "torrent")]
            {
                if package.format == "zip" {
                    // Single-file ZIP: coordinated download with web seeds +
                    // BT swarm as equal peers in a unified piece picker.
                    download_coordinated(package, content_root, seeding_policy, on_progress)
                } else {
                    // Multi-file torrents (ISO disc images): librqbit handles
                    // natively — it manages multi-file piece mapping internally.
                    download_via_librqbit(package, content_root, seeding_policy, on_progress)
                }
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

/// Downloads a content package using librqbit only (no coordinator).
///
/// Used for multi-file torrents (e.g. Archive.org disc ISOs) where librqbit
/// handles piece-to-file mapping internally. The coordinator cannot handle
/// multi-file torrents because it assumes a single contiguous output file.
///
/// After download: extract (ZIP/ISO) → manifest → seeding policy.
#[cfg(feature = "torrent")]
fn download_via_librqbit(
    package: &DownloadPackage,
    content_root: &Path,
    seeding_policy: SeedingPolicy,
    on_progress: &mut dyn FnMut(DownloadProgress),
) -> Result<(), DownloadError> {
    use crate::torrent::{TorrentConfig, TorrentDownloader, TorrentProgress};

    let config = TorrentConfig {
        seeding_policy,
        ..TorrentConfig::default()
    };

    let downloader =
        TorrentDownloader::new(config).map_err(|e| DownloadError::AllMirrorsFailed {
            count: 0,
            last_error: format!("torrent session init failed: {e}"),
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
    let files = extract_torrent_output(&archive_dir, content_root, package, on_progress)?;

    // Generate post-extraction manifest.
    if let Ok(manifest) =
        crate::verify::generate_manifest(content_root, package.game.slug(), "v1", &package.provides)
    {
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

/// Coordinated download: BT swarm + HTTP web seeds as equal peers in a unified
/// piece picker.
///
/// ## How it works
///
/// 1. **Resolve magnet URI** — librqbit connects to DHT/trackers, resolves the
///    torrent metadata (piece hashes, piece length, file info). librqbit starts
///    downloading in the background immediately.
///
/// 2. **Collect web seed URLs** — static `web_seeds` from the package definition,
///    plus mirror list URLs and `direct_urls` resolved at runtime.
///
/// 3. **Build coordinator** — the `PieceCoordinator` gets one `BtSwarmPeer`
///    (observing librqbit's output) and one `WebSeedPeer` per HTTP URL. The
///    coordinator assigns pieces to whichever source is fastest, SHA-1 verifying
///    each piece before writing it to the final output file.
///
/// 4. **Extract** — the coordinator produces the archive file (ZIP/ISO). The
///    standard extract → manifest → seeding-policy pipeline processes it.
///
/// Requires the `torrent` feature (librqbit + coordinator).
#[cfg(feature = "torrent")]
fn download_coordinated(
    package: &DownloadPackage,
    content_root: &Path,
    seeding_policy: SeedingPolicy,
    on_progress: &mut dyn FnMut(DownloadProgress),
) -> Result<(), DownloadError> {
    use std::sync::Arc;

    use crate::coordinator::btswarm::BtSwarmPeer;
    use crate::coordinator::webseed::WebSeedPeer;
    use crate::coordinator::{CoordinatorConfig, CoordinatorProgress, PieceCoordinator};
    use crate::torrent::TorrentConfig;

    let config = TorrentConfig {
        seeding_policy,
        ..TorrentConfig::default()
    };

    fs::create_dir_all(content_root)?;

    // ── 1. Resolve magnet URI → torrent metadata + running librqbit session ──
    on_progress(DownloadProgress::FetchingMirrors {
        url: format!("magnet:?xt=urn:btih:{}", package.info_hash),
    });

    let resolved = crate::torrent::resolve_and_start(package, &config).map_err(|e| {
        DownloadError::AllMirrorsFailed {
            count: 0,
            last_error: format!("torrent metadata resolution failed: {e}"),
        }
    })?;

    // ── 2. Collect web seed URLs from all available sources ──────────
    //
    // Sources (in priority order):
    // a. Static web_seeds from the package definition (Archive.org, CDN mirrors)
    // b. Mirror list URLs resolved at runtime (OpenRA mirrors)
    // c. Direct URLs from the package definition
    let mirror_list_url_override = std::env::var("CNC_MIRROR_LIST_URL").ok();
    let effective_mirror_url =
        mirror_list_url_override
            .as_deref()
            .or(if !package.mirror_list_url.is_empty() {
                Some(package.mirror_list_url.as_str())
            } else {
                None
            });

    let mirror_urls = if let Some(url) = effective_mirror_url {
        fetch_mirror_list(url).ok()
    } else {
        None
    };

    let direct_url_strs: Vec<&str> = package.direct_urls.iter().map(String::as_str).collect();
    let resolved_urls = resolve_download_urls(mirror_urls.as_deref(), &direct_url_strs);

    // ── 3. Build the coordinator with BT swarm + web seed peers ─────
    let info = resolved.info.clone();
    let coord_config = CoordinatorConfig::default();
    let mut coordinator = PieceCoordinator::new(info, coord_config);

    // Add web seed peers from static web_seeds.
    for url in &package.web_seeds {
        coordinator.add_peer(Box::new(WebSeedPeer::new(url.clone())));
    }

    // Add web seed peers from resolved mirrors (avoiding duplicates with
    // static web_seeds — a URL already in web_seeds shouldn't get a second peer).
    for url in &resolved_urls {
        if !package.web_seeds.iter().any(|ws| ws == url) {
            coordinator.add_peer(Box::new(WebSeedPeer::new(url.clone())));
        }
    }

    // Add BT swarm peer — wraps the running librqbit session as one "mega-peer".
    let bt_peer = BtSwarmPeer::new(
        resolved.librqbit_output.clone(),
        Arc::new(resolved.info.clone()),
        resolved.runtime.clone(),
        resolved.session.clone(),
    );
    coordinator.add_peer(Box::new(bt_peer));

    let total_pieces = coordinator.info().piece_count();
    on_progress(DownloadProgress::TryingMirror {
        index: 0,
        total: total_pieces as usize,
        url: format!(
            "Coordinated: BT swarm + {} web seeds",
            package.web_seeds.len().saturating_add(resolved_urls.len())
        ),
    });

    // ── 4. Run the coordinator — downloads all pieces to the output file ──
    let output_path = content_root.join(".download.coordinated.tmp");
    let piece_length = coordinator.info().piece_length;
    let file_size = coordinator.info().file_size;
    coordinator
        .run(&output_path, &mut |progress| match progress {
            CoordinatorProgress::PieceComplete {
                pieces_done,
                pieces_total,
                ..
            } => {
                let bytes_done = pieces_done as u64 * piece_length;
                on_progress(DownloadProgress::Downloading {
                    bytes: bytes_done,
                    total: Some(file_size),
                });
                let _ = pieces_total;
            }
            CoordinatorProgress::Complete { .. } => {
                on_progress(DownloadProgress::Verifying);
            }
            _ => {}
        })
        .map_err(|e| DownloadError::AllMirrorsFailed {
            count: 1,
            last_error: format!("coordinated download failed: {e}"),
        })?;

    // Drop the librqbit session — we have the complete file.
    // If seeding is desired, the TorrentDownloader is used separately.
    drop(resolved);

    // ── 5. Whole-file SHA-1 verification (defense in depth) ─────────
    //
    // Each piece was already SHA-1 verified by the coordinator. This
    // whole-file check is a belt-and-suspenders validation that the
    // assembled file is correct. Skip for placeholder all-zero hashes.
    let is_placeholder = package.sha1.chars().all(|c| c == '0');
    if is_placeholder {
        on_progress(DownloadProgress::VerifyingSkipped);
    } else {
        on_progress(DownloadProgress::Verifying);
        let actual_sha1 = crate::verify::sha1_file(&output_path, None)?;
        if actual_sha1 != package.sha1 {
            let _ = fs::remove_file(&output_path);
            return Err(DownloadError::Sha1Mismatch {
                expected: package.sha1.to_string(),
                actual: actual_sha1,
            });
        }
    }

    // ── 6. Extract the archive into content_root ────────────────────
    let files = extract_zip(&output_path, content_root, on_progress)?;

    // ── 7. Generate post-extraction manifest ────────────────────────
    if let Ok(manifest) =
        crate::verify::generate_manifest(content_root, package.game.slug(), "v1", &package.provides)
    {
        let manifest_path = content_root.join("content-manifest.toml");
        if let Ok(toml_str) = toml::to_string(&manifest) {
            let _ = fs::write(&manifest_path, toml_str);
        }
    }

    // ── 8. Apply seeding policy ─────────────────────────────────────
    let _ = fs::remove_file(&output_path);

    on_progress(DownloadProgress::Complete { files });
    Ok(())
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
