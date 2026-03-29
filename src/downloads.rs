// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! HTTP download definitions for RA1 content packages.
//!
//! OpenRA downloads (QuickInstall, BaseFiles, Aftermath, CncDesert) use
//! community mirrors at `openra.net`.
//!
//! IC-hosted downloads (Music, Movies, expansion music) serve freeware content
//! released by EA in 2008. These use Iron Curtain's own mirror infrastructure.
//! Until IC mirrors are deployed, these downloads will fail gracefully at the
//! mirror-list fetch step.

use crate::{DownloadId, DownloadPackage, PackageId};

/// All HTTP download packages.
///
/// The first four use OpenRA's mirror infrastructure. The remaining five use
/// IC-hosted mirrors for content OpenRA doesn't distribute (music, movies,
/// expansion music) — all freeware since EA's 2008 release.
pub static ALL_DOWNLOADS: &[DownloadPackage] = &[
    // ── OpenRA mirrors ────────────────────────────────────────────────
    DownloadPackage {
        id: DownloadId::QuickInstall,
        title: "Quick Install (Base + Aftermath + Desert)",
        mirror_list_url: "https://www.openra.net/packages/ra-quickinstall-mirrors.txt",
        sha1: "44241f68e69db9511db82cf83c174737ccda300b",
        provides: &[
            PackageId::Base,
            PackageId::AftermathBase,
            PackageId::CncDesert,
        ],
    },
    DownloadPackage {
        id: DownloadId::BaseFiles,
        title: "Base Game Files",
        mirror_list_url: "https://www.openra.net/packages/ra-base-mirrors.txt",
        sha1: "aa022b208a3b45b4a45c00fdae22ccf3c6de3e5c",
        provides: &[PackageId::Base],
    },
    DownloadPackage {
        id: DownloadId::Aftermath,
        title: "Aftermath Expansion Files",
        mirror_list_url: "https://www.openra.net/packages/ra-aftermath-mirrors.txt",
        sha1: "d511d4363b485e11c63eecf96d4365d42ec4ef5e",
        provides: &[PackageId::AftermathBase],
    },
    DownloadPackage {
        id: DownloadId::CncDesert,
        title: "C&C Desert Tileset",
        mirror_list_url: "https://www.openra.net/packages/ra-cncdesert-mirrors.txt",
        sha1: "039849f16e39e4722e8c838a393c8a0d6529fd59",
        provides: &[PackageId::CncDesert],
    },
    // ── IC-hosted freeware mirrors ────────────────────────────────────
    //
    // EA released Red Alert as freeware in 2008. These packages contain
    // content that OpenRA doesn't distribute but which is legally
    // redistributable. SHA-1 hashes will be populated when IC content
    // packages are built and hosted.
    DownloadPackage {
        id: DownloadId::Music,
        title: "Red Alert Music (scores.mix)",
        mirror_list_url: "https://content.iron-curtain.net/packages/ra-music-mirrors.txt",
        sha1: "0000000000000000000000000000000000000000",
        provides: &[PackageId::Music],
    },
    DownloadPackage {
        id: DownloadId::MoviesAllied,
        title: "Allied Campaign Movies",
        mirror_list_url: "https://content.iron-curtain.net/packages/ra-movies-allied-mirrors.txt",
        sha1: "0000000000000000000000000000000000000000",
        provides: &[PackageId::MoviesAllied],
    },
    DownloadPackage {
        id: DownloadId::MoviesSoviet,
        title: "Soviet Campaign Movies",
        mirror_list_url: "https://content.iron-curtain.net/packages/ra-movies-soviet-mirrors.txt",
        sha1: "0000000000000000000000000000000000000000",
        provides: &[PackageId::MoviesSoviet],
    },
    DownloadPackage {
        id: DownloadId::MusicCounterstrike,
        title: "Counterstrike Expansion Music",
        mirror_list_url:
            "https://content.iron-curtain.net/packages/ra-music-counterstrike-mirrors.txt",
        sha1: "0000000000000000000000000000000000000000",
        provides: &[PackageId::MusicCounterstrike],
    },
    DownloadPackage {
        id: DownloadId::MusicAftermath,
        title: "Aftermath Expansion Music",
        mirror_list_url: "https://content.iron-curtain.net/packages/ra-music-aftermath-mirrors.txt",
        sha1: "0000000000000000000000000000000000000000",
        provides: &[PackageId::MusicAftermath],
    },
];
