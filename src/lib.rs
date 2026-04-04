// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! # cnc-content — C&C content acquisition
//!
//! Standalone crate for downloading, verifying, and managing Command & Conquer
//! game content. Supports Red Alert, Tiberian Dawn, Dune 2, Dune 2000,
//! Tiberian Sun, Red Alert 2, and Generals.
//! Works without Bevy or any game engine dependency.
//!
//! ## What it does
//!
//! - **Defines** what each game needs (packages, sources, downloads)
//! - **Identifies** content sources on disk (discs, Steam, GOG, Origin installs)
//! - **Downloads** content via HTTP mirrors or BitTorrent P2P
//! - **Extracts** content from MIX, BIG, MEG, BAG/IDX archives, InstallShield CABs, ZIPs, raw offsets
//! - **Verifies** source identity (SHA-1) and installed integrity (SHA-256)
//!
//! ## CLI
//!
//! Build with the `cli` feature (default) for the `cnc-content` command:
//!
//! ```sh
//! cnc-content status                    # show installed/missing packages
//! cnc-content download                  # download all required content
//! cnc-content download --game td        # download Tiberian Dawn
//! cnc-content install --game dune2 /mnt/cdrom  # install from local source
//! cnc-content verify                    # check installed content integrity
//! cnc-content identify <path>           # identify a content source
//! ```
//!
//! ## Library usage
//!
//! ```rust
//! use cnc_content::GameId;
//!
//! // Check if Red Alert content is complete (uses a temp dir for the example)
//! let root = std::env::temp_dir().join("cnc-content-doctest-lib");
//! let _ = std::fs::create_dir_all(&root);
//! if !cnc_content::is_content_complete(&root, GameId::RedAlert) {
//!     let missing = cnc_content::missing_required_packages(&root, GameId::RedAlert);
//!     for pkg in missing {
//!         eprintln!("missing: {}", pkg.title);
//!     }
//! }
//! let _ = std::fs::remove_dir_all(&root);
//! ```

pub mod actions;
pub mod config;
pub mod coordinator;
#[cfg(feature = "download")]
pub mod downloader;
pub mod downloads;
pub mod executor;
pub mod iscab;
pub mod packages;
pub mod recipes;
#[cfg(feature = "download")]
pub mod session;
pub mod source;
pub mod sources;
pub mod streaming;
#[cfg(feature = "torrent")]
pub mod torrent;
pub mod torrent_create;
pub mod verify;

use serde::{Deserialize, Serialize};

// ── Core type definitions ──────────────────────────────────────────────

/// Identifies a supported game.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum GameId {
    /// Command & Conquer: Red Alert (1996) + expansions.
    RedAlert,
    /// Command & Conquer: Tiberian Dawn (1995) + Covert Operations.
    TiberianDawn,
    /// Dune II: The Building of a Dynasty (1992).
    /// NOT freeware — local source extraction only, no downloads.
    Dune2,
    /// Dune 2000 (1998).
    /// NOT freeware — local source extraction only, no downloads.
    Dune2000,
    /// Command & Conquer: Tiberian Sun (1999) + Firestorm.
    /// NOT freeware — local source extraction only.
    TiberianSun,
    /// Command & Conquer: Red Alert 2 (2000) + Yuri's Revenge.
    /// NOT freeware — local source extraction only.
    RedAlert2,
    /// Command & Conquer: Generals (2003) + Zero Hour.
    /// NOT freeware — local source extraction only.
    Generals,
}

impl GameId {
    /// Short CLI-friendly identifier.
    pub fn slug(self) -> &'static str {
        match self {
            GameId::RedAlert => "ra",
            GameId::TiberianDawn => "td",
            GameId::Dune2 => "dune2",
            GameId::Dune2000 => "dune2000",
            GameId::TiberianSun => "ts",
            GameId::RedAlert2 => "ra2",
            GameId::Generals => "generals",
        }
    }

    /// Human-readable title.
    pub fn title(self) -> &'static str {
        match self {
            GameId::RedAlert => "Command & Conquer: Red Alert",
            GameId::TiberianDawn => "Command & Conquer: Tiberian Dawn",
            GameId::Dune2 => "Dune II: The Building of a Dynasty",
            GameId::Dune2000 => "Dune 2000",
            GameId::TiberianSun => "Command & Conquer: Tiberian Sun",
            GameId::RedAlert2 => "Command & Conquer: Red Alert 2",
            GameId::Generals => "Command & Conquer: Generals",
        }
    }

    /// Whether this game's content is EA-declared freeware and can be downloaded.
    pub fn is_freeware(self) -> bool {
        matches!(self, GameId::RedAlert | GameId::TiberianDawn)
    }

    /// Parse from a CLI slug string.
    pub fn from_slug(s: &str) -> Option<GameId> {
        match s.to_lowercase().as_str() {
            "ra" | "redalert" | "red-alert" => Some(GameId::RedAlert),
            "td" | "tiberiandawn" | "tiberian-dawn" | "cnc" | "cnc95" => Some(GameId::TiberianDawn),
            "dune2" | "duneii" | "dune-2" => Some(GameId::Dune2),
            "dune2000" | "dune-2000" | "d2k" => Some(GameId::Dune2000),
            "ts" | "tiberiansun" | "tiberian-sun" => Some(GameId::TiberianSun),
            "ra2" | "redalert2" | "red-alert-2" => Some(GameId::RedAlert2),
            "gen" | "generals" | "cnc-generals" | "zh" | "zerohour" | "zero-hour" => {
                Some(GameId::Generals)
            }
            _ => None,
        }
    }

    /// All supported games.
    pub const ALL: &[GameId] = &[
        GameId::RedAlert,
        GameId::TiberianDawn,
        GameId::Dune2,
        GameId::Dune2000,
        GameId::TiberianSun,
        GameId::RedAlert2,
        GameId::Generals,
    ];
}

/// Identifies a content package — a logical group of files the game needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PackageId {
    // ── Red Alert ─────────────────────────────────────────────────────
    /// Core RA1 data: allies.mix, conquer.mix, interior.mix, etc.
    RaBase,
    /// Aftermath expansion base files: expand2.mix, hires1.mix, loose AUDs.
    RaAftermathBase,
    /// C&C desert tileset borrowed for some RA1 maps.
    RaCncDesert,
    /// Red Alert score music (scores.mix).
    RaMusic,
    /// Allied campaign FMV cutscenes.
    RaMoviesAllied,
    /// Soviet campaign FMV cutscenes.
    RaMoviesSoviet,
    /// Counterstrike expansion music tracks.
    RaMusicCounterstrike,
    /// Aftermath expansion music tracks.
    RaMusicAftermath,

    // ── Tiberian Dawn ─────────────────────────────────────────────────
    /// Core TD data: conquer.mix, desert.mix, temperat.mix, etc.
    TdBase,
    /// Covert Operations expansion data.
    TdCovertOps,
    /// Tiberian Dawn score music (scores.mix).
    TdMusic,
    /// GDI campaign FMV cutscenes.
    TdMoviesGdi,
    /// Nod campaign FMV cutscenes.
    TdMoviesNod,

    // ── Dune 2 (local source only) ──────────────────────────────────
    /// Complete Dune 2 game data. NOT freeware — local extraction only.
    Dune2Base,

    // ── Dune 2000 (local source only) ────────────────────────────────
    /// Complete Dune 2000 game data. NOT freeware — local extraction only.
    Dune2000Base,

    // ── Tiberian Sun (local source only) ────────────────────────────
    /// Core Tiberian Sun data: tibsun.mix, cache.mix, conquer.mix, etc.
    TsBase,
    /// Firestorm expansion data.
    TsFirestorm,
    /// Tiberian Sun score music (scores.mix).
    TsMusic,
    /// Tiberian Sun FMV cutscenes.
    TsMovies,

    // ── Red Alert 2 (local source only) ─────────────────────────────
    /// Core Red Alert 2 data: ra2.mix, language.mix, etc.
    Ra2Base,
    /// Yuri's Revenge expansion data.
    Ra2YurisRevenge,
    /// Red Alert 2 music (theme.mix).
    Ra2Music,
    /// Red Alert 2 FMV cutscenes.
    Ra2Movies,

    // ── Generals (local source only) ────────────────────────────────
    /// Core Generals data: INI.big, Terrain.big, W3D.big, etc.
    GenBase,
    /// Zero Hour expansion data.
    GenZeroHour,
}

/// Identifies a content source — a place content can be obtained from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SourceId {
    // ── Red Alert sources ─────────────────────────────────────────────
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

    // ── Tiberian Dawn sources ─────────────────────────────────────────
    /// C&C95 GDI disc image.
    TdGdiDisc,
    /// C&C95 Nod disc image.
    TdNodDisc,
    /// Covert Operations disc image.
    TdCovertOpsDisc,
    /// Steam — C&C (Tiberian Dawn).
    TdSteamCnc,
    /// Steam — C&C Remastered Collection (TD data).
    TdSteamRemastered,
    /// Origin — C&C (Tiberian Dawn).
    TdOriginCnc,

    // ── Dune 2 sources (local only) ─────────────────────────────────
    /// Dune 2 floppy/CD release. NOT freeware — local extraction only.
    Dune2Disc,
    /// Dune 2 GOG.com install. NOT freeware — local extraction only.
    GogDune2,

    // ── Dune 2000 sources (local only) ───────────────────────────────
    /// Dune 2000 disc or digital install. NOT freeware — local extraction only.
    Dune2000Disc,
    /// Dune 2000 GOG.com install. NOT freeware — local extraction only.
    GogDune2000,

    // ── Tiberian Sun sources (local only) ────────────────────────────
    /// Tiberian Sun retail CD.
    TsDisc,
    /// Firestorm expansion CD.
    TsFirestormDisc,
    /// Steam — The Ultimate Collection (Tiberian Sun).
    TsSteamTuc,
    /// Origin / EA App — The Ultimate Collection (Tiberian Sun).
    TsOriginTuc,

    // ── Red Alert 2 sources (local only) ─────────────────────────────
    /// Red Alert 2 retail CD.
    Ra2Disc,
    /// Yuri's Revenge expansion CD.
    Ra2YrDisc,
    /// The First Decade DVD (RA2 + YR).
    Ra2TheFirstDecade,
    /// Steam — The Ultimate Collection (Red Alert 2).
    Ra2SteamTuc,
    /// Origin / EA App — The Ultimate Collection (Red Alert 2).
    Ra2OriginTuc,

    // ── Generals sources (local only) ────────────────────────────────
    /// C&C Generals retail disc.
    GenDisc,
    /// Zero Hour expansion disc.
    GenZhDisc,
    /// Steam — The Ultimate Collection (Generals).
    GenSteamTuc,
    /// Origin / EA App — The Ultimate Collection (Generals).
    GenOriginTuc,
}

/// Identifies an HTTP/torrent download package.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DownloadId {
    // ── Red Alert downloads ───────────────────────────────────────────
    /// Quick-install base files (OpenRA freeware mirrors).
    RaQuickInstall,
    /// Base content files.
    RaBaseFiles,
    /// Aftermath expansion content.
    RaAftermath,
    /// C&C desert tileset.
    RaCncDesert,
    /// Red Alert score music (scores.mix) — IC-hosted freeware.
    RaMusic,
    /// Allied campaign movies (.vqa) — IC-hosted freeware.
    RaMoviesAllied,
    /// Soviet campaign movies (.vqa) — IC-hosted freeware.
    RaMoviesSoviet,
    /// Counterstrike expansion music — IC-hosted freeware.
    RaMusicCounterstrike,
    /// Aftermath expansion music — IC-hosted freeware.
    RaMusicAftermath,
    /// Full Allied + Soviet disc ISOs (Archive.org freeware mirror).
    RaFullDiscs,
    /// Full 4-CD set: Allied + Soviet + Counterstrike + Aftermath (Archive.org).
    RaFullSet,

    // ── Tiberian Dawn downloads ───────────────────────────────────────
    /// TD base game via OpenRA mirrors (freeware since 2007).
    TdBaseFiles,
    /// TD music (scores.mix).
    TdMusic,
    /// GDI campaign movies — CNCNZ/Archive.org freeware mirrors.
    TdMoviesGdi,
    /// Nod campaign movies — CNCNZ/Archive.org freeware mirrors.
    TdMoviesNod,
    /// Covert Operations expansion — CNCNZ freeware ISO.
    TdCovertOps,
    /// Full GDI disc ISO (CNCNZ freeware mirror).
    TdGdiIso,
    /// Full Nod disc ISO (CNCNZ freeware mirror).
    TdNodIso,
    // NOTE: No Dune 2 or Dune 2000 downloads — they are NOT freeware.
    // Only EA-declared freeware (RA, TD) may be downloaded.
}

/// Controls how the client shares downloaded content with other peers.
///
/// BitTorrent is a two-way protocol: downloading a file also means uploading
/// pieces to other peers (seeding). This policy lets the user control that
/// upload behavior. The default is `PauseDuringOnlinePlay`, which seeds
/// content when idle but pauses uploads during online gameplay to preserve
/// bandwidth.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SeedingPolicy {
    /// Seed downloaded content, but pause during online gameplay (default).
    ///
    /// This is the recommended setting: the user contributes to the P2P
    /// swarm when their bandwidth is idle, but seeding is temporarily
    /// suspended when a network game session is active.
    #[default]
    PauseDuringOnlinePlay,
    /// Seed continuously, even during online play.
    ///
    /// For users with high bandwidth who want to maximize their
    /// contribution to the swarm.
    SeedAlways,
    /// Keep downloaded archives but never upload to other peers.
    ///
    /// Downloaded ZIPs/ISOs are retained on disk (enabling fast
    /// re-extraction if content is corrupted) but no data is shared.
    KeepNoSeed,
    /// Extract content, then delete downloaded archives. No seeding.
    ///
    /// Minimizes disk usage. Downloaded packages are deleted immediately
    /// after successful extraction and verification. Re-downloading is
    /// required if content needs to be repaired.
    ExtractAndDelete,
}

impl SeedingPolicy {
    /// Whether this policy allows uploading to other peers at all.
    pub fn allows_seeding(self) -> bool {
        matches!(self, Self::PauseDuringOnlinePlay | Self::SeedAlways)
    }

    /// Whether downloaded archives should be retained on disk.
    pub fn retains_archives(self) -> bool {
        !matches!(self, Self::ExtractAndDelete)
    }

    /// Parse from a user-facing string (CLI, config file).
    pub fn from_str_loose(s: &str) -> Option<Self> {
        match s.to_lowercase().replace('-', "_").as_str() {
            "pause" | "pause_during_online_play" | "default" => Some(Self::PauseDuringOnlinePlay),
            "always" | "seed_always" => Some(Self::SeedAlways),
            "keep" | "keep_no_seed" | "no_seed" => Some(Self::KeepNoSeed),
            "delete" | "extract_and_delete" | "extract_delete" => Some(Self::ExtractAndDelete),
            _ => None,
        }
    }

    /// Human-readable label for UI display.
    pub fn label(self) -> &'static str {
        match self {
            Self::PauseDuringOnlinePlay => "Seed (pause during online play)",
            Self::SeedAlways => "Seed always",
            Self::KeepNoSeed => "Keep archives, no seeding",
            Self::ExtractAndDelete => "Extract & delete (no seeding)",
        }
    }
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
    /// Which game this package belongs to.
    pub game: GameId,
    /// Human-readable title for UI display.
    pub title: &'static str,
    /// Whether the game refuses to start without this package.
    pub required: bool,
    /// Files that prove this package is installed (checked at content root).
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
    /// GOG.com game ID for Galaxy library and registry detection.
    GogGameId(u64),
}

/// An install recipe — a named sequence of actions that extracts a specific
/// package from a specific source into the managed content directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallRecipe {
    /// Which source this recipe applies to.
    pub source: SourceId,
    /// Which package this recipe installs.
    pub package: PackageId,
    /// Ordered list of install actions to execute.
    pub actions: &'static [actions::InstallAction],
}

/// An HTTP/torrent download package definition.
///
/// Loaded from `data/downloads.toml` at first access via `include_str!`.
/// This is the complete, closed set of content the P2P engine may distribute —
/// no arbitrary torrents are allowed, only packages listed in the data file.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct DownloadPackage {
    /// Stable identifier.
    pub id: DownloadId,
    /// Which game this download belongs to.
    pub game: GameId,
    /// Human-readable title for UI display.
    pub title: String,
    /// URL that returns a newline-separated list of mirror URLs.
    /// Empty string if this download uses direct URLs only.
    pub mirror_list_url: String,
    /// Direct download URLs (tried in order). Used when no mirror list exists.
    pub direct_urls: Vec<String>,
    /// Expected SHA-1 of the downloaded archive (40 hex chars, or all-zero placeholder).
    pub sha1: String,
    /// BitTorrent info hash (hex) for P2P download. Empty if no torrent available.
    pub info_hash: String,
    /// Well-known tracker URLs for BitTorrent downloads.
    pub trackers: Vec<String>,
    /// BEP 19 web seed URLs — HTTP mirrors that participate as seeds in the
    /// torrent swarm. Each URL points to the complete archive file. Torrent
    /// clients use HTTP Range requests to fetch individual pieces, treating
    /// these as always-available, never-choked peers with 100% of all pieces.
    ///
    /// These URLs are embedded in the `.torrent` file's `url-list` field so
    /// that *any* BEP 19-capable client (not just ours) can use them.
    /// At runtime, the coordinator also treats dynamically-resolved mirror
    /// list URLs as additional web seed peers.
    pub web_seeds: Vec<String>,
    /// Which packages installing this download provides.
    pub provides: Vec<PackageId>,
    /// Format hint for extraction: "zip", "iso", "raw", etc.
    pub format: String,
    /// Approximate download size in bytes (for progress display, 0 if unknown).
    pub size_hint: u64,
}

impl DownloadPackage {
    /// Returns `true` if this download has at least one reachable source
    /// (mirror list URL, direct URL, web seed, or torrent info hash).
    pub fn is_available(&self) -> bool {
        !self.mirror_list_url.is_empty()
            || !self.direct_urls.is_empty()
            || !self.web_seeds.is_empty()
            || !self.info_hash.is_empty()
    }
}

// ── Convenience query functions (see query.rs) ─────────────────────────
mod query;
pub use query::*;

#[cfg(test)]
mod tests;
