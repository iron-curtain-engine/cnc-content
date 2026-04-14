// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! Content downloader — fetches game content via P2P-first transport with
//! HTTP mirrors as BEP 19 web seeds.
//!
//! ## Architecture: generic transport + game-specific orchestration
//!
//! The download infrastructure is layered for reusability:
//!
//! ### Generic transport layer (game-agnostic)
//!
//! These functions know nothing about C&C, Red Alert, or any specific game.
//! They operate on URLs, paths, sizes, and hashes. Any project can reuse
//! this layer for downloading files from HTTP mirrors:
//!
//! - [`resolve_mirrors`] — merges compiled + runtime + direct mirror URLs
//! - [`download_to_file`] — FlashGet-style segmented parallel HTTP download
//!   with SHA-1 verification. Accepts URLs, output path, size hint, hash.
//! - `mirror::segmented_download` / `parallel_download` / `try_download` —
//!   low-level HTTP transport primitives
//! - `extract::extract_zip` — Zip Slip-safe archive extraction
//!
//! ### Game-specific orchestration layer
//!
//! These functions compose the generic transport with C&C-specific
//! post-processing (package definitions, recipe extraction, manifest
//! generation):
//!
//! - [`download_package`] — HTTP download + C&C extraction + manifest
//! - [`download_and_install`] — P2P-first strategy with HTTP fallback +
//!   C&C extraction + manifest
//! - [`download_missing`] — game-aware routing (which packages to download
//!   for a given game)
//!
//! ## Transport priority
//!
//! **P2P is the default transport.** Every package with a non-empty
//! `info_hash` downloads via BitTorrent where HTTP mirrors participate as
//! BEP 19 web seeds — the coordinator treats BT swarm peers and HTTP
//! mirrors as equal piece sources, picking whichever is fastest for each
//! piece. This means downloads work with zero BT peers (mirrors serve
//! all pieces via Range requests) and improve as the swarm grows.
//!
//! **HTTP-only is the degraded fallback**, used only when P2P is
//! unavailable: no `info_hash`, `torrent` feature not compiled,
//! or `CNC_NO_P2P=1`. Even in this fallback, downloads use FlashGet-
//! style segmented parallel fetching — the file is split into segments
//! and each segment is assigned to a different mirror via HTTP Range
//! requests, aggregating bandwidth from all available mirrors
//! simultaneously.
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

pub mod mirror_health;

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
    /// BitTorrent P2P download with HTTP mirrors as BEP 19 web seeds.
    /// This is the **default** — mirrors and swarm peers are equal piece
    /// sources in a unified coordinator.
    Torrent,
    /// HTTP-only fallback — FlashGet-style segmented parallel download
    /// from multiple mirrors. Used only when P2P is unavailable.
    Http,
}

/// Determines the download strategy for a package.
///
/// **P2P is the default.** Falls back to HTTP only when:
/// - Package has no `info_hash` (no torrent metadata available)
/// - `torrent` feature is not compiled in
/// - `CNC_NO_P2P=1` environment variable is set
pub fn select_strategy(package: &DownloadPackage) -> DownloadStrategy {
    // CNC_NO_P2P=1 disables P2P transport entirely, forcing HTTP fallback.
    // Useful for CI, restricted networks, or debugging mirror issues.
    if std::env::var("CNC_NO_P2P").as_deref() == Ok("1") {
        return DownloadStrategy::Http;
    }

    // P2P is the default whenever torrent metadata exists.
    #[cfg(feature = "torrent")]
    if package.info_hash.is_some() {
        return DownloadStrategy::Torrent;
    }

    let _ = package; // used conditionally by torrent feature
    DownloadStrategy::Http
}

// ── Generic transport layer (game-agnostic) ─────────────────────────
//
// These functions know nothing about C&C, packages, or games. They
// operate on URLs, paths, sizes, and hashes. Any project that needs
// multi-mirror HTTP downloads can reuse this layer directly.

/// Resolves a deduplicated list of download URLs from multiple sources.
///
/// Merges mirrors from three tiers (in priority order):
/// 1. `compiled_mirrors` — baked into the binary at compile time
/// 2. `runtime_mirrors` — fetched from a mirror list URL at download time
/// 3. `direct_urls` — static fallback URLs from the package definition
///
/// This function is **game-agnostic** — it operates purely on URL strings.
/// The caller decides where the mirror lists come from.
pub fn resolve_mirrors(
    compiled_mirrors: &[String],
    runtime_mirrors: &[String],
    direct_urls: &[&str],
) -> Vec<String> {
    // Start with compiled mirrors (tamper-proof baseline).
    let mut urls: Vec<String> = compiled_mirrors.to_vec();

    // Add unique runtime mirrors (discovered since last binary release).
    for m in runtime_mirrors {
        if !urls.iter().any(|u| u == m) {
            urls.push(m.clone());
        }
    }

    // Merge with direct URLs via the low-level resolver (adds unique direct
    // URLs that aren't already in the mirror list).
    let url_refs: Vec<&str> = urls.iter().map(String::as_str).collect();
    resolve_download_urls(
        if url_refs.is_empty() {
            None
        } else {
            Some(&urls)
        },
        direct_urls,
    )
}

/// Downloads a file from HTTP mirrors to a local path with SHA-1 verification.
///
/// This is the **game-agnostic** HTTP download primitive. It knows nothing
/// about C&C packages, games, or extraction — it just downloads bytes from
/// mirrors to a file and optionally verifies a SHA-1 hash.
///
/// ## Fused download + hash pipeline
///
/// SHA-1 is computed inline during the download/assembly — the same bytes
/// that flow from the network to disk are fed to the hasher in a single
/// pass. This eliminates a separate sequential re-read of potentially
/// 500 MB+ files (inspired by BLAKE3's streaming verification model).
///
/// ## Transport strategy
///
/// - **Single mirror**: direct sequential download (no thread overhead).
/// - **Multiple mirrors + known size**: FlashGet-style segmented parallel
///   download — the file is split into segments, each mirror fetches its
///   byte range via HTTP Range, bandwidth is aggregated.
/// - **Multiple mirrors + unknown size**: parallel mirror racing — all
///   mirrors start concurrently, first complete download wins.
///
/// ## Parameters
///
/// - `urls` — resolved mirror URLs (call [`resolve_mirrors`] first)
/// - `dest` — output file path (parent directory must exist)
/// - `size_hint` — expected file size in bytes (0 = unknown; enables
///   segmented download when > 0 and multiple mirrors available)
/// - `expected_sha1` — hex-encoded SHA-1 hash to verify after download.
///   Pass an empty string or all-zero placeholder to skip verification.
/// - `on_progress` — callback for download progress events
pub fn download_to_file(
    urls: &[String],
    dest: &std::path::Path,
    size_hint: u64,
    expected_sha1: Option<&str>,
    on_progress: &mut (dyn FnMut(DownloadProgress) + Send),
) -> Result<(), DownloadError> {
    if urls.is_empty() {
        return Err(DownloadError::NoUrls);
    }

    let total_urls = urls.len();
    on_progress(DownloadProgress::TryingMirror {
        index: 0,
        total: total_urls,
        url: if total_urls == 1 {
            urls.first().cloned().unwrap_or_default()
        } else {
            format!("{total_urls} mirrors — segmented parallel download")
        },
    });

    // All transport paths now return the SHA-1 computed inline during
    // download/assembly. This eliminates a full re-read verification pass.
    let download_result: Result<String, String> = if total_urls == 1 {
        // Single URL — no need to spawn threads.
        try_download(
            urls.first().map(|s| s.as_str()).unwrap_or(""),
            dest,
            size_hint,
            &mut |bytes, total| {
                on_progress(DownloadProgress::Downloading { bytes, total });
            },
        )
        .map(|(_bytes, sha1)| sha1)
        .map_err(|e| e.to_string())
    } else if size_hint > 0 {
        // Multiple mirrors + known size: segmented parallel download.
        // SHA-1 is computed during segment assembly.
        segmented_download(urls, dest, size_hint, &mut |bytes, total| {
            on_progress(DownloadProgress::Downloading { bytes, total });
        })
    } else {
        // Multiple mirrors but unknown size: fall back to mirror racing.
        // Winner thread returns its inline SHA-1.
        parallel_download(urls, dest, size_hint, &mut |bytes, total| {
            on_progress(DownloadProgress::Downloading { bytes, total });
        })
    };

    let fused_sha1 = match download_result {
        Ok(sha1) => sha1,
        Err(last_error) => {
            let _ = fs::remove_file(dest);
            return Err(DownloadError::AllMirrorsFailed {
                count: total_urls,
                last_error,
            });
        }
    };

    // Verify SHA-1 using the hash computed inline during download — no
    // re-read of the file. Zero additional I/O for verification.
    check_sha1(&fused_sha1, expected_sha1, dest, on_progress)?;

    Ok(())
}

/// Checks a pre-computed SHA-1 against the expected value.
///
/// Unlike the old `verify_sha1` which re-read the entire file from disk,
/// this function only compares strings — the hash was already computed
/// inline during the download loop. On mismatch, deletes the file and
/// returns [`DownloadError::Sha1Mismatch`]. When no expected hash is
/// provided (`None`), verification is skipped.
fn check_sha1(
    actual: &str,
    expected: Option<&str>,
    file: &std::path::Path,
    on_progress: &mut (dyn FnMut(DownloadProgress) + Send),
) -> Result<(), DownloadError> {
    match expected {
        None => {
            on_progress(DownloadProgress::VerifyingSkipped);
        }
        Some(expected_hash) => {
            on_progress(DownloadProgress::Verifying);
            if actual != expected_hash {
                let _ = fs::remove_file(file);
                return Err(DownloadError::Sha1Mismatch {
                    expected: expected_hash.to_string(),
                    actual: actual.to_string(),
                });
            }
        }
    }
    Ok(())
}

// ── C&C-specific orchestration layer ────────────────────────────────
//
// These functions compose the generic transport with C&C-specific
// package definitions, extraction recipes, and manifest generation.
// They are intentionally coupled to DownloadPackage / GameId because
// this crate IS the C&C content manager. The generic layer above is
// what other projects would reuse.

/// Downloads and extracts a content package via HTTP mirrors (fallback path).
///
/// This is the **degraded fallback** used only when P2P is unavailable
/// (no `info_hash`, `torrent` feature not compiled, or `CNC_NO_P2P=1`).
/// Delegates to the generic [`download_to_file`] for transport, then
/// applies C&C-specific extraction and manifest generation.
///
/// No seeding occurs on this path — seeding requires the pre-configured
/// `.torrent` file that the P2P path uses via librqbit. The HTTP fallback
/// is a degraded mode that sacrifices P2P participation for reliability.
pub fn download_package(
    package: &DownloadPackage,
    content_root: &Path,
    on_progress: &mut (dyn FnMut(DownloadProgress) + Send),
) -> Result<(), DownloadError> {
    fs::create_dir_all(content_root)?;
    let zip_path = content_root.join(".download.zip.tmp");

    // ── 1. Resolve mirrors (generic) ────────────────────────────────
    let (compiled, runtime) = resolve_package_mirrors(package, on_progress);

    let direct_url_strs: Vec<&str> = package.direct_urls.iter().map(String::as_str).collect();
    let urls = resolve_mirrors(&compiled, &runtime, &direct_url_strs);

    // ── 2. Download + verify (generic) ──────────────────────────────
    download_to_file(
        &urls,
        &zip_path,
        package.size_hint,
        package.sha1.as_deref(),
        on_progress,
    )?;

    // ── 3. Extract ZIP (C&C-specific post-processing) ───────────────
    let files = extract_zip(&zip_path, content_root, on_progress)?;

    // ── 4. Generate integrity manifest (C&C-specific) ───────────────
    write_content_manifest(content_root, package);

    // ── 5. Clean up temp archive ────────────────────────────────────
    let _ = fs::remove_file(&zip_path);

    on_progress(DownloadProgress::Complete { files });
    Ok(())
}

/// Downloads and extracts a content package using the best available strategy.
///
/// Strategy selection:
/// 1. If the package has an `info_hash` and the `torrent` feature is enabled,
///    downloads via BitTorrent P2P (with tracker + DHT peer discovery).
///    p2p-distribute provides protocol obfuscation (DPI evasion), random
///    port selection, and relay circuits to work around P2P blocking.
/// 2. If P2P fails at runtime (all peers blocked, tracker unreachable,
///    NAT impenetrable), automatically falls back to HTTP mirrors using
///    FlashGet-style segmented parallel download.
/// 3. If no `info_hash` exists or the `torrent` feature is absent,
///    downloads directly via HTTP mirrors.
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
    on_progress: &mut (dyn FnMut(DownloadProgress) + Send),
) -> Result<(), DownloadError> {
    let strategy = select_strategy(package);
    let _ = &seeding_policy; // used conditionally by torrent feature

    match strategy {
        DownloadStrategy::Torrent => {
            #[cfg(feature = "torrent")]
            {
                // P2P-first: attempt coordinated download (BT swarm + HTTP
                // mirrors as BEP 19 web seeds). If P2P fails at runtime
                // (all peers blocked, tracker unreachable, NAT impenetrable
                // even after relay/hole-punch attempts), automatically fall
                // back to HTTP-only FlashGet-style segmented download.
                //
                // This mirrors the eMule lesson: obfuscation and relay help
                // most users, but some networks block ALL non-HTTP traffic.
                // The fallback ensures content is always reachable.
                let p2p_result = if package.format == "zip" {
                    download_coordinated(package, content_root, seeding_policy, on_progress)
                } else {
                    download_via_librqbit(package, content_root, seeding_policy, on_progress)
                };

                match p2p_result {
                    Ok(()) => Ok(()),
                    Err(p2p_err) => {
                        // P2P failed — degrade to HTTP-only.
                        // Log the P2P failure so the user knows why we fell back.
                        on_progress(DownloadProgress::TryingMirror {
                            index: 0,
                            total: 0,
                            url: format!("P2P failed ({p2p_err}), falling back to HTTP mirrors"),
                        });
                        download_package(package, content_root, on_progress)
                    }
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
    on_progress: &mut (dyn FnMut(DownloadProgress) + Send),
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
        url: format!(
            "magnet:?xt=urn:btih:{}",
            package.info_hash.as_deref().unwrap_or("")
        ),
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

    // Generate post-extraction manifest (C&C-specific).
    write_content_manifest(content_root, package);

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
    on_progress: &mut (dyn FnMut(DownloadProgress) + Send),
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
        url: format!(
            "magnet:?xt=urn:btih:{}",
            package.info_hash.as_deref().unwrap_or("")
        ),
    });

    let resolved = crate::torrent::resolve_and_start(package, &config).map_err(|e| {
        DownloadError::AllMirrorsFailed {
            count: 0,
            last_error: format!("torrent metadata resolution failed: {e}"),
        }
    })?;

    // ── 2. Collect web seed URLs from all available sources ──────────
    //
    // Uses the shared mirror resolver, then merges with static web_seeds.
    // The generic resolve_mirrors function knows nothing about C&C — the
    // package-specific mirror lookup is in resolve_package_mirrors.
    let (compiled, runtime) = resolve_package_mirrors(package, on_progress);
    let direct_url_strs: Vec<&str> = package.direct_urls.iter().map(String::as_str).collect();
    let resolved_urls = resolve_mirrors(&compiled, &runtime, &direct_url_strs);

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
    // whole-file check is belt-and-suspenders validation — compute the
    // SHA-1 of the assembled file and compare against the expected hash.
    let actual_sha1 = crate::verify::sha1_file(&output_path, None)
        .map_err(|e| DownloadError::Io { source: e })?;
    check_sha1(
        &actual_sha1,
        package.sha1.as_deref(),
        &output_path,
        on_progress,
    )?;

    // ── 6. Extract the archive into content_root ────────────────────
    let files = extract_zip(&output_path, content_root, on_progress)?;

    // ── 7. Generate post-extraction manifest (C&C-specific) ─────────
    write_content_manifest(content_root, package);

    // ── 8. Apply seeding policy ─────────────────────────────────────
    let _ = fs::remove_file(&output_path);

    on_progress(DownloadProgress::Complete { files });
    Ok(())
}

/// Downloads all required content for a game that is currently missing.
///
/// Routes through [`download_and_install`] which uses P2P by default
/// (BT swarm + HTTP mirrors as BEP 19 web seeds). Falls back to HTTP
/// only when P2P is unavailable.
///
/// Only EA-declared freeware games (Red Alert, Tiberian Dawn, Tiberian
/// Sun) support downloading. Non-freeware games (Dune 2, Dune 2000)
/// require the user to provide their own local copies.
pub fn download_missing(
    content_root: &Path,
    game: GameId,
    seeding_policy: SeedingPolicy,
    on_progress: &mut (dyn FnMut(DownloadProgress) + Send),
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
            download_and_install(pkg, content_root, seeding_policy, on_progress)?;
        }
        GameId::TiberianDawn => {
            let pkg =
                crate::download(DownloadId::TdBaseFiles).ok_or_else(|| DownloadError::Io {
                    source: io::Error::other("no download definition for TdBaseFiles"),
                })?;
            download_and_install(pkg, content_root, seeding_policy, on_progress)?;
        }
        GameId::TiberianSun => {
            // TS freeware: GDI disc ISO covers base game + music + movies.
            // Firestorm disc ISO covers the expansion pack.
            let gdi = crate::download(DownloadId::TsGdiIso).ok_or_else(|| DownloadError::Io {
                source: io::Error::other("no download definition for TsGdiIso"),
            })?;
            download_and_install(gdi, content_root, seeding_policy, on_progress)?;

            let firestorm =
                crate::download(DownloadId::TsFirestormIso).ok_or_else(|| DownloadError::Io {
                    source: io::Error::other("no download definition for TsFirestormIso"),
                })?;
            download_and_install(firestorm, content_root, seeding_policy, on_progress)?;
        }
        // Non-freeware games blocked above by is_freeware() check.
        _ => unreachable!(),
    }
    Ok(())
}

// ── C&C-specific helpers (shared by download_package, coordinated, librqbit) ──

/// Resolves compiled and runtime mirror lists for a C&C download package.
///
/// Extracts mirror URLs from the package's compiled cache and optional
/// runtime mirror list URL. Returns `(compiled, runtime)` vectors ready
/// for [`resolve_mirrors`].
fn resolve_package_mirrors(
    package: &DownloadPackage,
    on_progress: &mut (dyn FnMut(DownloadProgress) + Send),
) -> (Vec<String>, Vec<String>) {
    // CNC_MIRROR_LIST_URL overrides the per-package mirror list URL.
    let mirror_list_url_override = std::env::var("CNC_MIRROR_LIST_URL").ok();
    let effective_mirror_url = mirror_list_url_override
        .as_deref()
        .or(package.mirror_list_url.as_deref());

    let compiled = crate::downloads::compiled_mirrors(package.id)
        .map(|urls| urls.to_vec())
        .unwrap_or_default();

    let runtime = if let Some(url) = effective_mirror_url {
        on_progress(DownloadProgress::FetchingMirrors {
            url: url.to_string(),
        });
        fetch_mirror_list(url).ok().unwrap_or_default()
    } else {
        Vec::new()
    };

    (compiled, runtime)
}

/// Writes a post-extraction content manifest for C&C integrity verification.
///
/// Generates `content-manifest.toml` so future `verify` commands can detect
/// tampering or corruption without re-downloading. Errors are silently
/// ignored — manifest generation is best-effort.
fn write_content_manifest(content_root: &Path, package: &DownloadPackage) {
    if let Ok(manifest) =
        crate::verify::generate_manifest(content_root, package.game.slug(), "v1", &package.provides)
    {
        let manifest_path = content_root.join("content-manifest.toml");
        if let Ok(toml_str) = toml::to_string(&manifest) {
            let _ = fs::write(&manifest_path, toml_str);
        }
    }
}

// ── Sub-modules ───────────────────────────────────────────────────────

/// Mirror URL resolution, safety validation, and HTTP download helpers.
mod mirror;
#[cfg(test)]
use self::mirror::is_safe_mirror_url;
#[cfg(test)]
use self::mirror::parse_mirror_list_response;
use self::mirror::{
    fetch_mirror_list, parallel_download, resolve_download_urls, segmented_download, try_download,
};

/// Post-download extraction — ZIP, torrent archives, ISO disc images.
mod extract;
#[cfg(feature = "torrent")]
use self::extract::extract_torrent_output;
pub use self::extract::extract_zip;

#[cfg(test)]
use self::extract::{MAX_ENTRY_UNCOMPRESSED, MAX_TOTAL_UNCOMPRESSED, MAX_ZIP_ENTRIES};

#[cfg(test)]
mod tests;
