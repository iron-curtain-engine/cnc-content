// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! # cnc-content — C&C content acquisition
//!
//! Standalone crate for downloading, verifying, and managing Red Alert 1 game
//! content. Works without Bevy or any game engine dependency.
//!
//! ## What it does
//!
//! - **Defines** what RA1 content the game needs (packages, sources, downloads)
//! - **Identifies** content sources on disk (discs, Steam, GOG, Origin installs)
//! - **Downloads** content from OpenRA mirrors or IC P2P network
//! - **Extracts** content from MIX archives, InstallShield CABs, ZIPs, raw offsets
//! - **Verifies** source identity (SHA-1) and installed integrity (SHA-256)
//!
//! ## CLI
//!
//! Build with the `cli` feature (default) for the `cnc-content` command:
//!
//! ```sh
//! cnc-content status              # show installed/missing packages
//! cnc-content download            # download all required content
//! cnc-content verify              # check installed content integrity
//! cnc-content identify <path>     # identify a content source
//! ```
//!
//! ## Library usage
//!
//! ```rust,no_run
//! use cnc_content::{packages, sources, downloads, verify};
//!
//! // Check if content is complete
//! let root = std::path::Path::new("~/.iron-curtain/content/ra/v1");
//! if !cnc_content::is_content_complete(root) {
//!     let missing = cnc_content::missing_required_packages(root);
//!     for pkg in missing {
//!         eprintln!("missing: {}", pkg.title);
//!     }
//! }
//! ```

pub mod actions;
#[cfg(feature = "download")]
pub mod downloader;
pub mod downloads;
pub mod executor;
pub mod packages;
pub mod sources;
pub mod verify;

use serde::{Deserialize, Serialize};

// ── Core type definitions ──────────────────────────────────────────────

/// Identifies a content package — a logical group of files the game needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PackageId {
    /// Core RA1 data: allies.mix, conquer.mix, interior.mix, etc.
    Base,
    /// Aftermath expansion base files: expand2.mix, hires1.mix, loose AUDs.
    AftermathBase,
    /// C&C desert tileset borrowed for some RA1 maps.
    CncDesert,
    /// Red Alert score music (scores.mix).
    Music,
    /// Allied campaign FMV cutscenes.
    MoviesAllied,
    /// Soviet campaign FMV cutscenes.
    MoviesSoviet,
    /// Counterstrike expansion music tracks.
    MusicCounterstrike,
    /// Aftermath expansion music tracks.
    MusicAftermath,
}

/// Identifies a content source — a place content can be obtained from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SourceId {
    /// Allied Disc (western retail CD).
    AlliedDisc,
    /// Soviet Disc (eastern retail CD).
    SovietDisc,
    /// Counterstrike expansion disc.
    CounterstrikeDisc,
    /// Aftermath expansion disc.
    AftermathDisc,
    /// The First Decade DVD (InstallShield CAB).
    TheFirstDecade,
    /// C&C 1995 standalone (for desert.mix).
    Cnc95,
    /// Steam — The Ultimate Collection (Red Alert).
    SteamTuc,
    /// Steam — C&C (for desert.mix).
    SteamCnc,
    /// Steam — C&C Remastered Collection.
    SteamRemastered,
    /// Origin / EA App — The Ultimate Collection (Red Alert).
    OriginTuc,
    /// Origin / EA App — C&C (for desert.mix).
    OriginCnc,
    /// Origin / EA App — C&C Remastered Collection.
    OriginRemastered,
}

/// Identifies an HTTP download package.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DownloadId {
    /// Quick-install base files (freeware mirrors).
    QuickInstall,
    /// Base content files.
    BaseFiles,
    /// Aftermath expansion content.
    Aftermath,
    /// C&C desert tileset.
    CncDesert,
}

/// Type of content source for platform-specific probe routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceType {
    /// Optical disc (CD/DVD) or mounted ISO image.
    Disc,
    /// Steam library install, identified by app ID.
    Steam,
    /// GOG Galaxy or standalone GOG install.
    Gog,
    /// Origin / EA App install.
    Origin,
    /// Windows registry-based detection (e.g. CnC95).
    Registry,
    /// HTTP download from mirror list.
    Http,
    /// OpenRA's managed content directory.
    OpenRa,
    /// User-supplied directory.
    Manual,
}

/// A file-level identity check used to confirm a source is what we expect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdFileCheck {
    /// Path relative to the source root.
    pub path: &'static str,
    /// Expected SHA-1 hex digest (lowercase).
    pub sha1: &'static str,
    /// If `Some(n)`, hash only the first `n` bytes instead of the whole file.
    pub prefix_length: Option<u64>,
}

/// A content package definition — what the game needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentPackage {
    /// Stable identifier.
    pub id: PackageId,
    /// Human-readable title for UI display.
    pub title: &'static str,
    /// Whether the game refuses to start without this package.
    pub required: bool,
    /// Files whose presence proves this package is installed.
    pub test_files: &'static [&'static str],
    /// Sources that can provide this package.
    pub sources: &'static [SourceId],
    /// HTTP download that can provide this package, if any.
    pub download: Option<DownloadId>,
}

/// A content source definition — where content can come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentSource {
    /// Stable identifier.
    pub id: SourceId,
    /// Human-readable title for UI display.
    pub title: &'static str,
    /// Platform/distribution type for probe routing.
    pub source_type: SourceType,
    /// File checks that identify this specific source.
    pub id_files: &'static [IdFileCheck],
    /// Platform-specific hints (Steam app ID, registry key, etc.).
    pub platform_hint: Option<PlatformHint>,
}

/// Platform-specific detection hints attached to a source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlatformHint {
    /// Steam app ID for library scanning.
    SteamAppId(u32),
    /// Windows registry key + value name for path lookup.
    RegistryKey {
        key: &'static str,
        value: &'static str,
    },
}

/// An install recipe — a named sequence of actions that extracts content from
/// a source into the managed content directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallRecipe {
    /// Which source this recipe applies to.
    pub source: SourceId,
    /// Which packages this recipe provides.
    pub provides: &'static [PackageId],
    /// Ordered list of install actions to execute.
    pub actions: Vec<actions::InstallAction>,
}

/// An HTTP download package definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadPackage {
    /// Stable identifier.
    pub id: DownloadId,
    /// Human-readable title for UI display.
    pub title: &'static str,
    /// URL that returns a newline-separated list of mirror URLs.
    pub mirror_list_url: &'static str,
    /// Expected SHA-1 of the downloaded archive.
    pub sha1: &'static str,
    /// Which packages installing this download provides.
    pub provides: &'static [PackageId],
}

// ── Convenience functions ──────────────────────────────────────────────

/// Lookup a content package definition by ID.
pub fn package(id: PackageId) -> &'static ContentPackage {
    packages::ALL_PACKAGES
        .iter()
        .find(|p| p.id == id)
        .expect("every PackageId must have a corresponding definition")
}

/// Lookup a content source definition by ID.
pub fn source(id: SourceId) -> &'static ContentSource {
    sources::ALL_SOURCES
        .iter()
        .find(|s| s.id == id)
        .expect("every SourceId must have a corresponding definition")
}

/// Lookup an HTTP download definition by ID.
pub fn download(id: DownloadId) -> &'static DownloadPackage {
    downloads::ALL_DOWNLOADS
        .iter()
        .find(|d| d.id == id)
        .expect("every DownloadId must have a corresponding definition")
}

/// Default content root directory.
///
/// Returns `~/.iron-curtain/content/ra/v1/` expanded for the current platform.
pub fn default_content_root() -> std::path::PathBuf {
    let base = dirs_content_root();
    base.join("ra").join("v1")
}

/// Returns all required packages that are not yet installed.
pub fn missing_required_packages(
    content_root: &std::path::Path,
) -> Vec<&'static ContentPackage> {
    packages::ALL_PACKAGES
        .iter()
        .filter(|p| p.required && !p.test_files.iter().all(|f| content_root.join(f).exists()))
        .collect()
}

/// Returns `true` if all required content is installed.
pub fn is_content_complete(content_root: &std::path::Path) -> bool {
    missing_required_packages(content_root).is_empty()
}

/// Resolves the base content directory (`~/.iron-curtain/content/`).
fn dirs_content_root() -> std::path::PathBuf {
    // Check env var override first.
    if let Ok(dir) = std::env::var("IC_CONTENT_DIR") {
        return std::path::PathBuf::from(dir);
    }

    // Platform-specific home directory.
    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("LOCALAPPDATA") {
            return std::path::PathBuf::from(appdata)
                .join("iron-curtain")
                .join("content");
        }
    }

    if let Ok(home) = std::env::var("HOME") {
        return std::path::PathBuf::from(home)
            .join(".iron-curtain")
            .join("content");
    }

    // Fallback.
    std::path::PathBuf::from(".iron-curtain").join("content")
}

#[cfg(test)]
mod tests;
