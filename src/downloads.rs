// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! HTTP download definitions for RA1 content packages.
//!
//! These mirror OpenRA's download sources — ZIP archives served from
//! community mirrors. Each entry carries a mirror-list URL (returns
//! newline-separated direct links) and a SHA-1 of the expected archive.
//!
//! IC will prefer its own P2P network when available, falling back to
//! these HTTP mirrors.

use crate::{DownloadId, DownloadPackage, PackageId};

/// All HTTP download packages.
///
/// Mirror list URLs point to OpenRA's mirror infrastructure. Each URL
/// returns a plain-text list of direct download links, one per line.
/// SHA-1 hashes match OpenRA's published values exactly.
pub static ALL_DOWNLOADS: &[DownloadPackage] = &[
    DownloadPackage {
        id: DownloadId::QuickInstall,
        title: "Quick Install (Base + Aftermath + Desert)",
        mirror_list_url: "https://www.openra.net/packages/ra-quickinstall-mirrors.txt",
        sha1: "44241f68e69db9511db82cf83c174737ccda300b",
        provides: &[PackageId::Base, PackageId::AftermathBase, PackageId::CncDesert],
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
];
