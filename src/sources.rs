// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! Content source definitions — where content can be obtained.
//!
//! Each source carries SHA-1 ID file checks that identify the specific edition
//! at a filesystem path. Red Alert hashes match OpenRA's published verification
//! data exactly so source detection is compatible across both projects.

use crate::{ContentSource, IdFileCheck, PlatformHint, SourceId, SourceType};

/// All known content sources across all games.
///
/// Sources are ordered by detection priority: disc editions first (for users
/// with physical media), then digital storefronts, then standalone downloads.
pub static ALL_SOURCES: &[ContentSource] = &[
    // ══════════════════════════════════════════════════════════════════════
    // Red Alert sources
    // ══════════════════════════════════════════════════════════════════════

    // ── Disc sources ──────────────────────────────────────────────────
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
    // ── The First Decade (InstallShield CAB) ──────────────────────────
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
    // ── Standalone / registry sources ─────────────────────────────────
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
    // ── Steam sources ─────────────────────────────────────────────────
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
    // ── Origin / EA App sources ───────────────────────────────────────
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
    // ══════════════════════════════════════════════════════════════════════
    // Tiberian Dawn sources — freeware since 2007
    // ══════════════════════════════════════════════════════════════════════
    ContentSource {
        id: SourceId::TdGdiDisc,
        title: "Tiberian Dawn GDI Disc",
        source_type: SourceType::Disc,
        id_files: &[IdFileCheck {
            path: "CONQUER.MIX",
            sha1: "713b53fa4c188ca9619c6bbeadbfc86513704266",
            prefix_length: None,
        }],
        platform_hint: None,
    },
    ContentSource {
        id: SourceId::TdNodDisc,
        title: "Tiberian Dawn Nod Disc",
        source_type: SourceType::Disc,
        id_files: &[IdFileCheck {
            path: "CONQUER.MIX",
            sha1: "713b53fa4c188ca9619c6bbeadbfc86513704266",
            prefix_length: None,
        }],
        platform_hint: None,
    },
    ContentSource {
        id: SourceId::TdCovertOpsDisc,
        title: "Covert Operations Disc",
        source_type: SourceType::Disc,
        id_files: &[IdFileCheck {
            path: "SC-000.MIX",
            sha1: "0000000000000000000000000000000000000000",
            prefix_length: None,
        }],
        platform_hint: None,
    },
    ContentSource {
        id: SourceId::TdSteamCnc,
        title: "Steam — C&C (Tiberian Dawn)",
        source_type: SourceType::Steam,
        id_files: &[IdFileCheck {
            path: "CONQUER.MIX",
            sha1: "713b53fa4c188ca9619c6bbeadbfc86513704266",
            prefix_length: None,
        }],
        platform_hint: Some(PlatformHint::SteamAppId(2229830)),
    },
    ContentSource {
        id: SourceId::TdSteamRemastered,
        title: "Steam — C&C Remastered (TD data)",
        source_type: SourceType::Steam,
        id_files: &[IdFileCheck {
            path: "Data/CNCDATA/TIBERIAN_DAWN/CD1/CONQUER.MIX",
            sha1: "713b53fa4c188ca9619c6bbeadbfc86513704266",
            prefix_length: None,
        }],
        platform_hint: Some(PlatformHint::SteamAppId(1213210)),
    },
    ContentSource {
        id: SourceId::TdOriginCnc,
        title: "Origin — C&C (Tiberian Dawn)",
        source_type: SourceType::Origin,
        id_files: &[IdFileCheck {
            path: "CONQUER.MIX",
            sha1: "713b53fa4c188ca9619c6bbeadbfc86513704266",
            prefix_length: None,
        }],
        platform_hint: Some(PlatformHint::RegistryKey {
            key: r"SOFTWARE\EA Games\CNC and The Covert Operations",
            value: "Install Dir",
        }),
    },
    // ══════════════════════════════════════════════════════════════════════
    // Dune 2 sources — local only (NOT freeware, no downloads)
    // ══════════════════════════════════════════════════════════════════════
    ContentSource {
        id: SourceId::Dune2Disc,
        title: "Dune II Game Files",
        source_type: SourceType::Disc,
        id_files: &[IdFileCheck {
            path: "DUNE2.EXE",
            sha1: "0000000000000000000000000000000000000000",
            prefix_length: None,
        }],
        platform_hint: None,
    },
    ContentSource {
        id: SourceId::GogDune2,
        title: "Dune II (GOG.com)",
        source_type: SourceType::Gog,
        id_files: &[IdFileCheck {
            path: "DUNE2.EXE",
            sha1: "0000000000000000000000000000000000000000",
            prefix_length: None,
        }],
        platform_hint: Some(PlatformHint::GogGameId(1207658856)),
    },
    // ══════════════════════════════════════════════════════════════════════
    // Dune 2000 sources — local only (NOT freeware, no downloads)
    // ══════════════════════════════════════════════════════════════════════
    ContentSource {
        id: SourceId::Dune2000Disc,
        title: "Dune 2000 Game Files",
        source_type: SourceType::Disc,
        id_files: &[IdFileCheck {
            path: "DUNE2000.EXE",
            sha1: "0000000000000000000000000000000000000000",
            prefix_length: None,
        }],
        platform_hint: None,
    },
    ContentSource {
        id: SourceId::GogDune2000,
        title: "Dune 2000 (GOG.com)",
        source_type: SourceType::Gog,
        id_files: &[IdFileCheck {
            path: "DUNE2000.EXE",
            sha1: "0000000000000000000000000000000000000000",
            prefix_length: None,
        }],
        platform_hint: Some(PlatformHint::GogGameId(1207659107)),
    },
    // ══════════════════════════════════════════════════════════════════════
    // Tiberian Sun sources — local only
    //
    // TS was briefly free-to-download from EA but later removed.
    // Placeholder SHA-1 hashes are used until verified from real media.
    // ══════════════════════════════════════════════════════════════════════

    // ── Disc sources ──────────────────────────────────────────────────
    ContentSource {
        id: SourceId::TsDisc,
        title: "Tiberian Sun Retail CD",
        source_type: SourceType::Disc,
        id_files: &[IdFileCheck {
            path: "TIBSUN.MIX",
            sha1: "0000000000000000000000000000000000000000",
            prefix_length: None,
        }],
        platform_hint: None,
    },
    ContentSource {
        id: SourceId::TsFirestormDisc,
        title: "Firestorm Expansion CD",
        source_type: SourceType::Disc,
        id_files: &[IdFileCheck {
            path: "E01SC01.MIX",
            sha1: "0000000000000000000000000000000000000000",
            prefix_length: None,
        }],
        platform_hint: None,
    },
    // ── Steam / Origin sources ────────────────────────────────────────
    ContentSource {
        id: SourceId::TsSteamTuc,
        title: "Steam — The Ultimate Collection (TS)",
        // Manual until Steam app ID is confirmed; then upgrade to Steam.
        source_type: SourceType::Manual,
        id_files: &[IdFileCheck {
            path: "TIBSUN.MIX",
            sha1: "0000000000000000000000000000000000000000",
            prefix_length: None,
        }],
        platform_hint: None,
    },
    ContentSource {
        id: SourceId::TsOriginTuc,
        title: "Origin — The Ultimate Collection (TS)",
        source_type: SourceType::Origin,
        id_files: &[IdFileCheck {
            path: "TIBSUN.MIX",
            sha1: "0000000000000000000000000000000000000000",
            prefix_length: None,
        }],
        platform_hint: Some(PlatformHint::RegistryKey {
            key: r"SOFTWARE\EA Games\Tiberian Sun",
            value: "Install Dir",
        }),
    },
    // ══════════════════════════════════════════════════════════════════════
    // Red Alert 2 sources — local only (NOT freeware, no downloads)
    //
    // Placeholder SHA-1 hashes are used until verified from real media.
    // ══════════════════════════════════════════════════════════════════════

    // ── Disc sources ──────────────────────────────────────────────────
    ContentSource {
        id: SourceId::Ra2Disc,
        title: "Red Alert 2 Retail CD",
        source_type: SourceType::Disc,
        id_files: &[IdFileCheck {
            path: "RA2.MIX",
            sha1: "0000000000000000000000000000000000000000",
            prefix_length: None,
        }],
        platform_hint: None,
    },
    ContentSource {
        id: SourceId::Ra2YrDisc,
        title: "Yuri's Revenge Expansion CD",
        source_type: SourceType::Disc,
        id_files: &[IdFileCheck {
            path: "RA2MD.MIX",
            sha1: "0000000000000000000000000000000000000000",
            prefix_length: None,
        }],
        platform_hint: None,
    },
    ContentSource {
        id: SourceId::Ra2TheFirstDecade,
        title: "C&C: The First Decade (RA2 + YR)",
        source_type: SourceType::Disc,
        id_files: &[IdFileCheck {
            path: "RA2.MIX",
            sha1: "0000000000000000000000000000000000000000",
            prefix_length: None,
        }],
        platform_hint: None,
    },
    // ── Steam / Origin sources ────────────────────────────────────────
    ContentSource {
        id: SourceId::Ra2SteamTuc,
        title: "Steam — The Ultimate Collection (RA2)",
        source_type: SourceType::Steam,
        id_files: &[IdFileCheck {
            // ra2.mix is the primary game archive — present in all RA2 editions.
            path: "ra2.mix",
            sha1: "3bd92246320f4bf1ff1ed76207ee793c33ff6a05",
            prefix_length: None,
        }],
        platform_hint: Some(PlatformHint::SteamAppId(2229850)),
    },
    ContentSource {
        id: SourceId::Ra2OriginTuc,
        title: "Origin — The Ultimate Collection (RA2)",
        source_type: SourceType::Origin,
        id_files: &[IdFileCheck {
            // Same TUC build as Steam — identical ra2.mix hash.
            path: "ra2.mix",
            sha1: "3bd92246320f4bf1ff1ed76207ee793c33ff6a05",
            prefix_length: None,
        }],
        platform_hint: Some(PlatformHint::RegistryKey {
            key: r"SOFTWARE\EA Games\Red Alert 2",
            value: "Install Dir",
        }),
    },
    // ══════════════════════════════════════════════════════════════════════
    // C&C Generals sources — local only (NOT freeware, no downloads)
    //
    // Generals uses BIG archives (BIGF format) instead of MIX.
    // Placeholder SHA-1 hashes are used until verified from real media.
    // ══════════════════════════════════════════════════════════════════════

    // ── Disc sources ──────────────────────────────────────────────────
    ContentSource {
        id: SourceId::GenDisc,
        title: "C&C Generals Retail Disc",
        source_type: SourceType::Disc,
        id_files: &[IdFileCheck {
            path: "INI.big",
            sha1: "0000000000000000000000000000000000000000",
            prefix_length: None,
        }],
        platform_hint: None,
    },
    ContentSource {
        id: SourceId::GenZhDisc,
        title: "Zero Hour Expansion Disc",
        source_type: SourceType::Disc,
        id_files: &[IdFileCheck {
            path: "INIZH.big",
            sha1: "0000000000000000000000000000000000000000",
            prefix_length: None,
        }],
        platform_hint: None,
    },
    // ── Steam / Origin sources ────────────────────────────────────────
    ContentSource {
        id: SourceId::GenSteamTuc,
        title: "Steam — The Ultimate Collection (Generals)",
        // Steam TUC merges base Generals + Zero Hour into a single install
        // directory — there is no separate INIZH.big. Both GenBase and
        // GenZeroHour packages list this source.
        source_type: SourceType::Steam,
        id_files: &[IdFileCheck {
            // INI.big is the primary configuration archive for Generals.
            path: "INI.big",
            sha1: "3f1957f73bff6aebef8220ed5392a22b274420d6",
            prefix_length: None,
        }],
        platform_hint: Some(PlatformHint::SteamAppId(2229870)),
    },
    ContentSource {
        id: SourceId::GenOriginTuc,
        title: "Origin — The Ultimate Collection (Generals)",
        source_type: SourceType::Origin,
        id_files: &[IdFileCheck {
            // Same TUC build as Steam — identical INI.big hash.
            path: "INI.big",
            sha1: "3f1957f73bff6aebef8220ed5392a22b274420d6",
            prefix_length: None,
        }],
        platform_hint: Some(PlatformHint::RegistryKey {
            key: r"SOFTWARE\EA Games\Command and Conquer Generals Zero Hour",
            value: "InstallPath",
        }),
    },
];
