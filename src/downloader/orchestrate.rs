// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! C&C-specific orchestration helpers — large private functions extracted
//! from `downloader/mod.rs` to keep that file focused on the public API.

use std::fs;
use std::path::Path;

use super::*;

/// Downloads a content package using librqbit only (no coordinator).
///
/// Used for multi-file torrents (e.g. Archive.org disc ISOs) where librqbit
/// handles piece-to-file mapping internally. The coordinator cannot handle
/// multi-file torrents because it assumes a single contiguous output file.
///
/// After download: extract (ZIP/ISO) → manifest → seeding policy.
#[cfg(feature = "torrent")]
pub(super) fn download_via_librqbit(
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
pub(super) fn download_coordinated(
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

/// Resolves compiled and runtime mirror lists for a C&C download package.
///
/// Extracts mirror URLs from the package's compiled cache and optional
/// runtime mirror list URL. Returns `(compiled, runtime)` vectors ready
/// for [`resolve_mirrors`].
pub(super) fn resolve_package_mirrors(
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
pub(super) fn write_content_manifest(content_root: &Path, package: &DownloadPackage) {
    if let Ok(manifest) =
        crate::verify::generate_manifest(content_root, package.game.slug(), "v1", &package.provides)
    {
        let manifest_path = content_root.join("content-manifest.toml");
        if let Ok(toml_str) = toml::to_string(&manifest) {
            let _ = fs::write(&manifest_path, toml_str);
        }
    }
}
