// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! Convenience query functions for looking up content definitions.
//!
//! All functions are re-exported from the crate root so callers use them as
//! `cnc_content::package()`, `cnc_content::recipe()`, etc.
//!
//! These are thin lookup/filter wrappers over the compile-time static tables
//! in `packages`, `sources`, `downloads`, and `recipes`.

use crate::{
    downloads, packages, recipes, sources, ContentPackage, ContentSource, DownloadId,
    DownloadPackage, GameId, InstallRecipe, PackageId, SourceId,
};

use std::sync::LazyLock;

/// Cached download list — parsed once from `data/downloads.toml` and sorted
/// for consistent iteration order. Stored separately from the downloads module
/// so query functions can return `&'static` references.
static ALL_DOWNLOADS_CACHE: LazyLock<&'static [DownloadPackage]> =
    LazyLock::new(downloads::all_downloads);

// ── ID lookups ────────────────────────────────────────────────────────────

/// Lookup a content package definition by ID.
pub fn package(id: PackageId) -> Option<&'static ContentPackage> {
    packages::ALL_PACKAGES.iter().find(|p| p.id == id)
}

/// Lookup a content source definition by ID.
pub fn source(id: SourceId) -> Option<&'static ContentSource> {
    sources::ALL_SOURCES.iter().find(|s| s.id == id)
}

/// Lookup an HTTP download definition by ID.
pub fn download(id: DownloadId) -> Option<&'static DownloadPackage> {
    ALL_DOWNLOADS_CACHE.iter().find(|d| d.id == id)
}

/// Returns the embedded `.torrent` file bytes for a download package, or
/// `None` if no `.torrent` has been generated yet.
///
/// Thin re-export of [`downloads::embedded_torrent`] for crate-root access.
pub fn embedded_torrent(id: DownloadId) -> Option<&'static [u8]> {
    downloads::embedded_torrent(id)
}

/// Lookup an install recipe for a source/package combination.
pub fn recipe(source: SourceId, package: PackageId) -> Option<&'static InstallRecipe> {
    recipes::ALL_RECIPES
        .iter()
        .find(|r| r.source == source && r.package == package)
}

/// Returns all install recipes for a given source.
pub fn recipes_for_source(source: SourceId) -> Vec<&'static InstallRecipe> {
    recipes::ALL_RECIPES
        .iter()
        .filter(|r| r.source == source)
        .collect()
}

// ── Game-scoped filters ───────────────────────────────────────────────────

/// Returns all packages for a specific game.
pub fn packages_for_game(game: GameId) -> Vec<&'static ContentPackage> {
    packages::ALL_PACKAGES
        .iter()
        .filter(|p| p.game == game)
        .collect()
}

/// Returns all downloads for a specific game.
pub fn downloads_for_game(game: GameId) -> Vec<&'static DownloadPackage> {
    ALL_DOWNLOADS_CACHE
        .iter()
        .filter(|d| d.game == game)
        .collect()
}

/// Returns all packages that are not yet installed (both required and optional).
pub fn missing_packages(
    content_root: &std::path::Path,
    game: GameId,
) -> Vec<&'static ContentPackage> {
    packages::ALL_PACKAGES
        .iter()
        .filter(|p| p.game == game && !p.test_files.iter().all(|f| content_root.join(f).exists()))
        .collect()
}

/// Returns all required packages for a game that are not yet installed.
pub fn missing_required_packages(
    content_root: &std::path::Path,
    game: GameId,
) -> Vec<&'static ContentPackage> {
    packages::ALL_PACKAGES
        .iter()
        .filter(|p| {
            p.game == game
                && p.required
                && !p.test_files.iter().all(|f| content_root.join(f).exists())
        })
        .collect()
}

/// Returns `true` if all required content for a game is installed.
pub fn is_content_complete(content_root: &std::path::Path, game: GameId) -> bool {
    missing_required_packages(content_root, game).is_empty()
}

// ── Content root helpers ──────────────────────────────────────────────────

/// Default content root directory for a given game.
///
/// Resolution order:
/// 1. `CNC_CONTENT_ROOT` env var (explicit override — used as-is)
/// 2. Executable-relative `content/<slug>/v1/` (portable default)
///
/// If the executable directory cannot be determined (e.g. sandboxed
/// environment), falls back to `./content/<slug>/v1/` relative to CWD.
pub fn default_content_root_for_game(game: GameId) -> std::path::PathBuf {
    if let Ok(dir) = std::env::var("CNC_CONTENT_ROOT") {
        return std::path::PathBuf::from(dir);
    }

    let suffix = format!("content/{}/v1", game.slug());
    // app_path macro only takes literals, so we compute the exe dir manually.
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|p| p.join(&suffix)))
        .unwrap_or_else(|| std::path::PathBuf::from(&suffix))
}

/// Default content root for Red Alert (backwards-compatible).
pub fn default_content_root() -> std::path::PathBuf {
    default_content_root_for_game(GameId::RedAlert)
}

/// Returns the OpenRA content directory for the current platform.
///
/// Used by `--openra` to download content into OpenRA's managed path
/// so both engines share the same files.
///
/// - Windows: `%APPDATA%/OpenRA/Content/ra/v2/`
/// - Linux:   `~/.openra/Content/ra/v2/`
/// - macOS:   `~/Library/Application Support/OpenRA/Content/ra/v2/`
pub fn openra_content_root() -> Option<std::path::PathBuf> {
    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            return Some(std::path::PathBuf::from(appdata).join("OpenRA/Content/ra/v2"));
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Ok(home) = std::env::var("HOME") {
            return Some(
                std::path::PathBuf::from(home)
                    .join("Library/Application Support/OpenRA/Content/ra/v2"),
            );
        }
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        if let Ok(home) = std::env::var("HOME") {
            return Some(std::path::PathBuf::from(home).join(".openra/Content/ra/v2"));
        }
    }

    None
}

// ── BitTorrent tracker list ───────────────────────────────────────────────

/// Well-known public BitTorrent tracker announce URLs, embedded from
/// `data/trackers.txt`. Public trackers are neutral infrastructure — they
/// coordinate peer discovery but do not host content. Legality depends
/// entirely on what content is shared (we only share EA freeware).
///
/// These trackers are NOT yet active for our content. Torrents must first
/// be created, seeded, and registered with the tracker before P2P works.
/// Until then, all downloads use HTTP mirrors.
pub const PUBLIC_TRACKERS_RAW: &str = include_str!("../data/trackers.txt");

/// Parsed tracker URLs from `data/trackers.txt`.
pub fn public_trackers() -> impl Iterator<Item = &'static str> {
    PUBLIC_TRACKERS_RAW
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
}
