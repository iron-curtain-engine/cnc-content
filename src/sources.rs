// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! RA1 content source definitions — where content can be obtained.
//!
//! Each source carries SHA-1 ID file checks that identify the specific edition
//! at a filesystem path. These hashes match OpenRA's published verification
//! data exactly so source detection is compatible across both projects.

use crate::{ContentSource, IdFileCheck, PlatformHint, SourceId, SourceType};

/// All known RA1 content sources.
///
/// Sources are ordered by detection priority: disc editions first (for users
/// with physical media), then digital storefronts, then standalone downloads.
pub static ALL_SOURCES: &[ContentSource] = &[
    // ── Disc sources ───────────────────────────────────────────────────
    ContentSource {
        id: SourceId::AlliedDisc,
        title: "Red Alert Allied Disc",
        source_type: SourceType::Disc,
        id_files: &[
            IdFileCheck {
                path: "MAIN.MIX",
                sha1: "20ebe16f91ff79be2d672f1db5bae9048ff9357c",
                prefix_length: Some(4096),
            },
            IdFileCheck {
                path: "INSTALL/REDALERT.MIX",
                sha1: "0e58f4b54f44f6cd29fecf8cf379d33cf2d4caef",
                prefix_length: None,
            },
        ],
        platform_hint: None,
    },
    ContentSource {
        id: SourceId::SovietDisc,
        title: "Red Alert Soviet Disc",
        source_type: SourceType::Disc,
        id_files: &[
            IdFileCheck {
                path: "MAIN.MIX",
                sha1: "9d108f18560716b684ab8b1da42cc7f3d1b52519",
                prefix_length: Some(4096),
            },
            IdFileCheck {
                path: "INSTALL/REDALERT.MIX",
                sha1: "0e58f4b54f44f6cd29fecf8cf379d33cf2d4caef",
                prefix_length: None,
            },
        ],
        platform_hint: None,
    },
    ContentSource {
        id: SourceId::CounterstrikeDisc,
        title: "Counterstrike Expansion Disc",
        source_type: SourceType::Disc,
        id_files: &[
            IdFileCheck {
                path: "README.TXT",
                sha1: "0efe8087383f0b159a9633f891fb5f53c6097cd4",
                prefix_length: None,
            },
            IdFileCheck {
                path: "SETUP/INSTALL/CSTRIKE.RTP",
                sha1: "fae8ba82db71574f6ecd8fb4ff4026fcb65d2adc",
                prefix_length: None,
            },
        ],
        platform_hint: None,
    },
    ContentSource {
        id: SourceId::AftermathDisc,
        title: "Aftermath Expansion Disc",
        source_type: SourceType::Disc,
        id_files: &[
            IdFileCheck {
                path: "README.TXT",
                sha1: "9902fb74c019df1b76ff5634e68f0371d790b5e0",
                prefix_length: None,
            },
            IdFileCheck {
                path: "SETUP/INSTALL/PATCH.RTP",
                sha1: "5bce93f834f9322ddaa7233242e5b6c7fea0bf17",
                prefix_length: None,
            },
        ],
        platform_hint: None,
    },
    // ── The First Decade (InstallShield CAB) ───────────────────────────
    ContentSource {
        id: SourceId::TheFirstDecade,
        title: "C&C: The First Decade",
        source_type: SourceType::Disc,
        id_files: &[
            IdFileCheck {
                path: "data1.hdr",
                sha1: "bef3a08c3fc1b1caf28ca0dbb97c1f900005930e",
                prefix_length: None,
            },
            IdFileCheck {
                path: "data1.cab",
                sha1: "12ad6113a6890a1b4d5651a75378c963eaf513b9",
                prefix_length: None,
            },
        ],
        platform_hint: None,
    },
    // ── Standalone / registry sources ──────────────────────────────────
    ContentSource {
        id: SourceId::Cnc95,
        title: "C&C95 Disc",
        source_type: SourceType::Disc,
        id_files: &[IdFileCheck {
            path: "CONQUER.MIX",
            sha1: "833e02a09aae694659eb312d3838367f681d1b30",
            prefix_length: None,
        }],
        platform_hint: None,
    },
    // ── Steam sources ──────────────────────────────────────────────────
    ContentSource {
        id: SourceId::SteamTuc,
        title: "Steam — The Ultimate Collection (RA)",
        source_type: SourceType::Steam,
        id_files: &[IdFileCheck {
            path: "REDALERT.MIX",
            sha1: "0e58f4b54f44f6cd29fecf8cf379d33cf2d4caef",
            prefix_length: None,
        }],
        platform_hint: Some(PlatformHint::SteamAppId(2229840)),
    },
    ContentSource {
        id: SourceId::SteamCnc,
        title: "Steam — C&C (for desert.mix)",
        source_type: SourceType::Steam,
        id_files: &[IdFileCheck {
            path: "CONQUER.MIX",
            sha1: "713b53fa4c188ca9619c6bbeadbfc86513704266",
            prefix_length: None,
        }],
        platform_hint: Some(PlatformHint::SteamAppId(2229830)),
    },
    ContentSource {
        id: SourceId::SteamRemastered,
        title: "Steam — C&C Remastered Collection",
        source_type: SourceType::Steam,
        id_files: &[IdFileCheck {
            path: "Data/CNCDATA/RED_ALERT/CD1/REDALERT.MIX",
            sha1: "0e58f4b54f44f6cd29fecf8cf379d33cf2d4caef",
            prefix_length: None,
        }],
        platform_hint: Some(PlatformHint::SteamAppId(1213210)),
    },
    // ── Origin / EA App sources ────────────────────────────────────────
    ContentSource {
        id: SourceId::OriginTuc,
        title: "Origin — The Ultimate Collection (RA)",
        source_type: SourceType::Origin,
        id_files: &[IdFileCheck {
            path: "REDALERT.MIX",
            sha1: "0e58f4b54f44f6cd29fecf8cf379d33cf2d4caef",
            prefix_length: None,
        }],
        platform_hint: Some(PlatformHint::RegistryKey {
            key: r"SOFTWARE\EA Games\Command and Conquer Red Alert",
            value: "Install Dir",
        }),
    },
    ContentSource {
        id: SourceId::OriginCnc,
        title: "Origin — C&C (for desert.mix)",
        source_type: SourceType::Origin,
        id_files: &[IdFileCheck {
            path: "CONQUER.MIX",
            sha1: "833e02a09aae694659eb312d3838367f681d1b30",
            prefix_length: None,
        }],
        platform_hint: Some(PlatformHint::RegistryKey {
            key: r"SOFTWARE\EA Games\CNC and The Covert Operations",
            value: "Install Dir",
        }),
    },
    ContentSource {
        id: SourceId::OriginRemastered,
        title: "Origin — C&C Remastered Collection",
        source_type: SourceType::Origin,
        id_files: &[IdFileCheck {
            path: "Data/CNCDATA/RED_ALERT/CD1/REDALERT.MIX",
            sha1: "0e58f4b54f44f6cd29fecf8cf379d33cf2d4caef",
            prefix_length: None,
        }],
        platform_hint: Some(PlatformHint::RegistryKey {
            key: r"SOFTWARE\Petroglyph\CnCRemastered",
            value: "Install Dir",
        }),
    },
];
