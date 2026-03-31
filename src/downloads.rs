// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! HTTP/torrent download definitions for all supported games.
//!
//! ## Red Alert
//! OpenRA downloads use community mirrors at `openra.net`. IC-hosted downloads
//! (music, movies) will be served from the `content-bootstrap` GitHub repo once
//! content ZIPs are built. Until then they can only come from local sources.
//!
//! ## Tiberian Dawn
//! EA released Tiberian Dawn as freeware on 31 August 2007 (12th anniversary).
//! OpenRA mirrors provide the minimal MIX files. CNCNZ hosts full disc ISOs.
//! Source code released under GPL-3.0 in February 2025.
//!
//! ## Dune 2
//! Never officially declared freeware. Available on Archive.org as abandonware.
//! The Dune IP belongs to the Herbert estate; EA cannot commercially distribute it.

use crate::{DownloadId, DownloadPackage, GameId, PackageId};

/// All HTTP/torrent download packages across all games.
pub static ALL_DOWNLOADS: &[DownloadPackage] = &[
    // ══════════════════════════════════════════════════════════════════════
    // Red Alert — freeware since 31 August 2008
    // ══════════════════════════════════════════════════════════════════════
    DownloadPackage {
        id: DownloadId::RaQuickInstall,
        game: GameId::RedAlert,
        title: "Quick Install (Base + Aftermath + Desert)",
        mirror_list_url: "https://www.openra.net/packages/ra-quickinstall-mirrors.txt",
        direct_urls: &[],
        sha1: "44241f68e69db9511db82cf83c174737ccda300b",
        info_hash: "",
        trackers: &[],
        provides: &[
            PackageId::RaBase,
            PackageId::RaAftermathBase,
            PackageId::RaCncDesert,
        ],
        format: "zip",
        size_hint: 18_000_000,
    },
    DownloadPackage {
        id: DownloadId::RaBaseFiles,
        game: GameId::RedAlert,
        title: "Base Game Files",
        mirror_list_url: "https://www.openra.net/packages/ra-base-mirrors.txt",
        direct_urls: &[],
        sha1: "aa022b208a3b45b4a45c00fdae22ccf3c6de3e5c",
        info_hash: "",
        trackers: &[],
        provides: &[PackageId::RaBase],
        format: "zip",
        size_hint: 12_000_000,
    },
    DownloadPackage {
        id: DownloadId::RaAftermath,
        game: GameId::RedAlert,
        title: "Aftermath Expansion Files",
        mirror_list_url: "https://www.openra.net/packages/ra-aftermath-mirrors.txt",
        direct_urls: &[],
        sha1: "d511d4363b485e11c63eecf96d4365d42ec4ef5e",
        info_hash: "",
        trackers: &[],
        provides: &[PackageId::RaAftermathBase],
        format: "zip",
        size_hint: 4_000_000,
    },
    DownloadPackage {
        id: DownloadId::RaCncDesert,
        game: GameId::RedAlert,
        title: "C&C Desert Tileset",
        mirror_list_url: "https://www.openra.net/packages/ra-cncdesert-mirrors.txt",
        direct_urls: &[],
        sha1: "039849f16e39e4722e8c838a393c8a0d6529fd59",
        info_hash: "",
        trackers: &[],
        provides: &[PackageId::RaCncDesert],
        format: "zip",
        size_hint: 1_500_000,
    },
    // ── Archive.org full disc images (validated, seeded via webseed) ───
    //
    // These are the original retail disc ISOs uploaded to Archive.org by
    // the C&C community. All items are malware-scanned by Archive.org.
    // The RA freeware item links EA's official announcement as source.
    // Archive.org provides permanent availability via webseed (BEP 19).
    //
    // btih = Archive.org item-level torrent hash. Trackers are Archive.org
    // closed trackers. These torrents contain multiple files; BitTorrent
    // clients can selectively download individual files.
    DownloadPackage {
        id: DownloadId::RaFullDiscs,
        game: GameId::RedAlert,
        title: "Red Alert Full Discs — Allied + Soviet ISOs (Archive.org freeware)",
        mirror_list_url: "",
        direct_urls: &[
            "https://archive.org/download/command-and-conquer-red-alert/CD1_ALLIED_DISC.ISO",
            "https://archive.org/download/command-and-conquer-red-alert/CD2_SOVIET_DISC.ISO",
        ],
        sha1: "0000000000000000000000000000000000000000",
        info_hash: "6585d7f5b81d2a005c196c47bf5972dff9228840",
        trackers: &[
            "http://bt1.archive.org:6969/announce",
            "http://bt2.archive.org:6969/announce",
        ],
        provides: &[
            PackageId::RaBase,
            PackageId::RaMusic,
            PackageId::RaMoviesAllied,
            PackageId::RaMoviesSoviet,
        ],
        format: "iso",
        size_hint: 1_331_744_768,
    },
    DownloadPackage {
        id: DownloadId::RaFullSet,
        game: GameId::RedAlert,
        title: "Red Alert 4-CD Set — Base + Counterstrike + Aftermath (Archive.org)",
        mirror_list_url: "",
        direct_urls: &[
            "https://archive.org/download/red_alert_cd/CD1_Allies.iso",
            "https://archive.org/download/red_alert_cd/CD2_Soviet.iso",
            "https://archive.org/download/red_alert_cd/CD3_Counterstrike.iso",
            "https://archive.org/download/red_alert_cd/CD4_Aftermath.iso",
        ],
        sha1: "0000000000000000000000000000000000000000",
        info_hash: "28b589c17d93b06f173a5be9bb7d94ce423ee5eb",
        trackers: &[
            "http://bt1.archive.org:6969/announce",
            "http://bt2.archive.org:6969/announce",
        ],
        provides: &[
            PackageId::RaBase,
            PackageId::RaMusic,
            PackageId::RaMoviesAllied,
            PackageId::RaMoviesSoviet,
            PackageId::RaMusicCounterstrike,
            PackageId::RaMusicAftermath,
            PackageId::RaAftermathBase,
        ],
        format: "iso",
        size_hint: 1_853_218_816,
    },
    // ── IC-hosted freeware content ─────────────────────────────────────
    //
    // EA released Red Alert as freeware in 2008. These packages contain
    // content that OpenRA doesn't distribute but which is legally
    // redistributable. Mirror lists are hosted in the content-bootstrap
    // GitHub repo and served via raw.githubusercontent.com.
    //
    // SHA-1 hashes are placeholder (all-zero) until content ZIPs are
    // built and seeded. The downloader skips SHA-1 verification for
    // all-zero hashes.
    DownloadPackage {
        id: DownloadId::RaMusic,
        game: GameId::RedAlert,
        title: "Red Alert Music (scores.mix)",
        mirror_list_url: "https://raw.githubusercontent.com/iron-curtain-engine/content-bootstrap/main/mirrors/ra-music.txt",
        direct_urls: &[],
        sha1: "0000000000000000000000000000000000000000",
        info_hash: "",
        trackers: &[],
        provides: &[PackageId::RaMusic],
        format: "zip",
        size_hint: 50_000_000,
    },
    DownloadPackage {
        id: DownloadId::RaMoviesAllied,
        game: GameId::RedAlert,
        title: "Allied Campaign Movies",
        mirror_list_url: "https://raw.githubusercontent.com/iron-curtain-engine/content-bootstrap/main/mirrors/ra-movies-allied.txt",
        direct_urls: &[],
        sha1: "0000000000000000000000000000000000000000",
        info_hash: "",
        trackers: &[],
        provides: &[PackageId::RaMoviesAllied],
        format: "zip",
        size_hint: 300_000_000,
    },
    DownloadPackage {
        id: DownloadId::RaMoviesSoviet,
        game: GameId::RedAlert,
        title: "Soviet Campaign Movies",
        mirror_list_url: "https://raw.githubusercontent.com/iron-curtain-engine/content-bootstrap/main/mirrors/ra-movies-soviet.txt",
        direct_urls: &[],
        sha1: "0000000000000000000000000000000000000000",
        info_hash: "",
        trackers: &[],
        provides: &[PackageId::RaMoviesSoviet],
        format: "zip",
        size_hint: 350_000_000,
    },
    DownloadPackage {
        id: DownloadId::RaMusicCounterstrike,
        game: GameId::RedAlert,
        title: "Counterstrike Expansion Music",
        mirror_list_url: "https://raw.githubusercontent.com/iron-curtain-engine/content-bootstrap/main/mirrors/ra-music-counterstrike.txt",
        direct_urls: &[],
        sha1: "0000000000000000000000000000000000000000",
        info_hash: "",
        trackers: &[],
        provides: &[PackageId::RaMusicCounterstrike],
        format: "zip",
        size_hint: 30_000_000,
    },
    DownloadPackage {
        id: DownloadId::RaMusicAftermath,
        game: GameId::RedAlert,
        title: "Aftermath Expansion Music",
        mirror_list_url: "https://raw.githubusercontent.com/iron-curtain-engine/content-bootstrap/main/mirrors/ra-music-aftermath.txt",
        direct_urls: &[],
        sha1: "0000000000000000000000000000000000000000",
        info_hash: "",
        trackers: &[],
        provides: &[PackageId::RaMusicAftermath],
        format: "zip",
        size_hint: 35_000_000,
    },
    // ══════════════════════════════════════════════════════════════════════
    // Tiberian Dawn — freeware since 31 August 2007
    //
    // EA officially released C&C Tiberian Dawn (Gold Edition) as freeware.
    // OpenRA mirrors provide the minimal MIX files needed for gameplay.
    // CNCNZ hosts the full disc ISOs with movies and music.
    // Source code released under GPL-3.0 in February 2025.
    // ══════════════════════════════════════════════════════════════════════
    DownloadPackage {
        id: DownloadId::TdBaseFiles,
        game: GameId::TiberianDawn,
        title: "Tiberian Dawn Base Game (OpenRA mirrors)",
        mirror_list_url: "https://www.openra.net/packages/cnc-mirrors.txt",
        // Direct fallback mirrors — only known-live URLs.
        // ppmsite.com and baxxster.no removed (dead as of 2026-03).
        // The mirror list at openra.net is the primary source.
        direct_urls: &[
            "https://cdn.mailaender.name/openra/cnc-packages.zip",
            "https://openra.0x47.net/cnc-packages.zip",
        ],
        sha1: "0000000000000000000000000000000000000000",
        info_hash: "",
        trackers: &[],
        provides: &[PackageId::TdBase],
        format: "zip",
        size_hint: 15_000_000,
    },
    // IC-hosted TD freeware content. Mirror lists from content-bootstrap repo.
    DownloadPackage {
        id: DownloadId::TdMusic,
        game: GameId::TiberianDawn,
        title: "Tiberian Dawn Music",
        mirror_list_url: "https://raw.githubusercontent.com/iron-curtain-engine/content-bootstrap/main/mirrors/td-music.txt",
        direct_urls: &[],
        sha1: "0000000000000000000000000000000000000000",
        info_hash: "",
        trackers: &[],
        provides: &[PackageId::TdMusic],
        format: "zip",
        size_hint: 40_000_000,
    },
    DownloadPackage {
        id: DownloadId::TdMoviesGdi,
        game: GameId::TiberianDawn,
        title: "GDI Campaign Movies",
        mirror_list_url: "https://raw.githubusercontent.com/iron-curtain-engine/content-bootstrap/main/mirrors/td-movies-gdi.txt",
        direct_urls: &[],
        sha1: "0000000000000000000000000000000000000000",
        info_hash: "",
        trackers: &[],
        provides: &[PackageId::TdMoviesGdi],
        format: "zip",
        size_hint: 250_000_000,
    },
    DownloadPackage {
        id: DownloadId::TdMoviesNod,
        game: GameId::TiberianDawn,
        title: "Nod Campaign Movies",
        mirror_list_url: "https://raw.githubusercontent.com/iron-curtain-engine/content-bootstrap/main/mirrors/td-movies-nod.txt",
        direct_urls: &[],
        sha1: "0000000000000000000000000000000000000000",
        info_hash: "",
        trackers: &[],
        provides: &[PackageId::TdMoviesNod],
        format: "zip",
        size_hint: 250_000_000,
    },
    DownloadPackage {
        id: DownloadId::TdCovertOps,
        game: GameId::TiberianDawn,
        title: "Covert Operations Expansion (freeware ISO)",
        mirror_list_url: "",
        direct_urls: &[
            "https://files.cncnz.com/cc1_tiberian_dawn/full_game/CovertOps_ISO.zip",
        ],
        sha1: "0000000000000000000000000000000000000000",
        info_hash: "",
        trackers: &[],
        provides: &[PackageId::TdCovertOps],
        format: "zip",
        size_hint: 309_000_000,
    },
    // Archive.org item `cnc-dos-eng-v-1.22` — validated by Nyerguds (C&C
    // community maintainer), checked for malware by validator@archive.org.
    // The btih covers the full item (both ISOs); BitTorrent clients can
    // selectively download individual files from multi-file torrents.
    // Trackers are Archive.org's closed trackers for their items.
    DownloadPackage {
        id: DownloadId::TdGdiIso,
        game: GameId::TiberianDawn,
        title: "Tiberian Dawn GDI Disc (freeware ISO)",
        mirror_list_url: "",
        // Multiple mirrors for resilience: CNCNZ (community archive) +
        // Archive.org (non-profit digital library, also acts as webseed).
        direct_urls: &[
            "https://files.cncnz.com/cc1_tiberian_dawn/full_game/GDI95.zip",
            "https://archive.org/download/cnc-dos-eng-v-1.22/C%26C%20DOS%20ENG%20v1.22%20Disk%201%20-%20GDI.iso",
        ],
        sha1: "0000000000000000000000000000000000000000",
        // Archive.org item btih — seeded via Archive.org webseed infra.
        info_hash: "8f430be74dee33f9d76f72b50bbf2a537c442794",
        trackers: &[
            "http://bt1.archive.org:6969/announce",
            "http://bt2.archive.org:6969/announce",
        ],
        provides: &[PackageId::TdBase, PackageId::TdMoviesGdi],
        format: "zip",
        size_hint: 608_000_000,
    },
    DownloadPackage {
        id: DownloadId::TdNodIso,
        game: GameId::TiberianDawn,
        title: "Tiberian Dawn Nod Disc (freeware ISO)",
        mirror_list_url: "",
        direct_urls: &[
            "https://files.cncnz.com/cc1_tiberian_dawn/full_game/NOD95.zip",
            "https://archive.org/download/cnc-dos-eng-v-1.22/C%26C%20DOS%20ENG%20v1.22%20Disk%202%20-%20Nod.iso",
        ],
        sha1: "0000000000000000000000000000000000000000",
        // Same Archive.org item as TdGdiIso — multi-file torrent.
        info_hash: "8f430be74dee33f9d76f72b50bbf2a537c442794",
        trackers: &[
            "http://bt1.archive.org:6969/announce",
            "http://bt2.archive.org:6969/announce",
        ],
        provides: &[PackageId::TdBase, PackageId::TdMoviesNod],
        format: "zip",
        size_hint: 608_000_000,
    },
    // NOTE: No Dune 2 or Dune 2000 downloads. These games are NOT freeware
    // and we do not take legal risks. Only EA-declared freeware (RA since 2008,
    // TD since 2007) may be downloaded. Dune 2 and Dune 2000 support local
    // source extraction only — users must provide their own copies.
];
