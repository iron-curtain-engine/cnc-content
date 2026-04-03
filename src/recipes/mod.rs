// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! Install recipes — source-specific extraction sequences for each package.
//!
//! Each recipe defines the exact actions needed to extract a specific content
//! package from a specific source. File names and extraction logic match
//! OpenRA's content installer plugins.
//!
//! All data is compile-time constant (`&'static`).

use crate::actions::{FileMapping, InstallAction};
use crate::{InstallRecipe, PackageId, SourceId};

/// All install recipes.
pub static ALL_RECIPES: &[InstallRecipe] = &[
    // ── Steam TUC (AppId 2229840) ─────────────────────────────────────
    InstallRecipe {
        source: SourceId::SteamTuc,
        package: PackageId::RaBase,
        actions: &[InstallAction::Copy {
            files: &BASE_DIRECT_COPY,
        }],
    },
    InstallRecipe {
        source: SourceId::SteamTuc,
        package: PackageId::RaAftermathBase,
        actions: &[InstallAction::Copy {
            files: &AFTERMATH_EXPAND_COPY,
        }],
    },
    InstallRecipe {
        source: SourceId::SteamTuc,
        package: PackageId::RaMusic,
        actions: &[InstallAction::ExtractMix {
            source_mix: "MAIN.MIX",
            entries: &[FileMapping {
                from: "scores.mix",
                to: "scores.mix",
            }],
        }],
    },
    InstallRecipe {
        source: SourceId::SteamTuc,
        package: PackageId::RaMoviesAllied,
        actions: &MOVIES_ALLIED_FROM_MAIN_MIX,
    },
    InstallRecipe {
        source: SourceId::SteamTuc,
        package: PackageId::RaMoviesSoviet,
        actions: &MOVIES_SOVIET_FROM_MAIN_MIX,
    },
    InstallRecipe {
        source: SourceId::SteamTuc,
        package: PackageId::RaMusicCounterstrike,
        actions: &[InstallAction::Copy {
            files: &CS_MUSIC_COPY,
        }],
    },
    InstallRecipe {
        source: SourceId::SteamTuc,
        package: PackageId::RaMusicAftermath,
        actions: &[InstallAction::Copy {
            files: &AM_MUSIC_COPY,
        }],
    },
    // ── Steam Remastered (AppId 1213210) ──────────────────────────────
    InstallRecipe {
        source: SourceId::SteamRemastered,
        package: PackageId::RaBase,
        actions: &[InstallAction::Copy {
            files: &REMASTERED_BASE_COPY,
        }],
    },
    InstallRecipe {
        source: SourceId::SteamRemastered,
        package: PackageId::RaAftermathBase,
        actions: &[InstallAction::Copy {
            files: &REMASTERED_AFTERMATH_COPY,
        }],
    },
    InstallRecipe {
        source: SourceId::SteamRemastered,
        package: PackageId::RaCncDesert,
        actions: &[InstallAction::Copy {
            files: &[FileMapping {
                from: "Data/CNCDATA/TIBERIAN_DAWN/CD1/DESERT.MIX",
                to: "cnc/desert.mix",
            }],
        }],
    },
    InstallRecipe {
        source: SourceId::SteamRemastered,
        package: PackageId::RaMusic,
        actions: &[InstallAction::ExtractMix {
            source_mix: "Data/CNCDATA/RED_ALERT/CD1/MAIN.MIX",
            entries: &[FileMapping {
                from: "scores.mix",
                to: "scores.mix",
            }],
        }],
    },
    InstallRecipe {
        source: SourceId::SteamRemastered,
        package: PackageId::RaMusicCounterstrike,
        actions: &[InstallAction::Copy {
            files: &REMASTERED_CS_MUSIC_COPY,
        }],
    },
    InstallRecipe {
        source: SourceId::SteamRemastered,
        package: PackageId::RaMusicAftermath,
        actions: &[InstallAction::Copy {
            files: &REMASTERED_AM_MUSIC_COPY,
        }],
    },
    // ── Steam C&C (AppId 2229830) — desert only ──────────────────────
    InstallRecipe {
        source: SourceId::SteamCnc,
        package: PackageId::RaCncDesert,
        actions: &[InstallAction::Copy {
            files: &[FileMapping {
                from: "DESERT.MIX",
                to: "cnc/desert.mix",
            }],
        }],
    },
    // ── Origin TUC (same layout as Steam TUC) ────────────────────────
    InstallRecipe {
        source: SourceId::OriginTuc,
        package: PackageId::RaBase,
        actions: &[InstallAction::Copy {
            files: &BASE_DIRECT_COPY,
        }],
    },
    InstallRecipe {
        source: SourceId::OriginTuc,
        package: PackageId::RaAftermathBase,
        actions: &[InstallAction::Copy {
            files: &AFTERMATH_EXPAND_COPY,
        }],
    },
    InstallRecipe {
        source: SourceId::OriginTuc,
        package: PackageId::RaMusic,
        actions: &[InstallAction::ExtractMix {
            source_mix: "MAIN.MIX",
            entries: &[FileMapping {
                from: "scores.mix",
                to: "scores.mix",
            }],
        }],
    },
    InstallRecipe {
        source: SourceId::OriginTuc,
        package: PackageId::RaMoviesAllied,
        actions: &MOVIES_ALLIED_FROM_MAIN_MIX,
    },
    InstallRecipe {
        source: SourceId::OriginTuc,
        package: PackageId::RaMoviesSoviet,
        actions: &MOVIES_SOVIET_FROM_MAIN_MIX,
    },
    InstallRecipe {
        source: SourceId::OriginTuc,
        package: PackageId::RaMusicCounterstrike,
        actions: &[InstallAction::Copy {
            files: &CS_MUSIC_COPY,
        }],
    },
    InstallRecipe {
        source: SourceId::OriginTuc,
        package: PackageId::RaMusicAftermath,
        actions: &[InstallAction::Copy {
            files: &AM_MUSIC_COPY,
        }],
    },
    // ── Origin C&C — desert only ─────────────────────────────────────
    InstallRecipe {
        source: SourceId::OriginCnc,
        package: PackageId::RaCncDesert,
        actions: &[InstallAction::Copy {
            files: &[FileMapping {
                from: "DESERT.MIX",
                to: "cnc/desert.mix",
            }],
        }],
    },
    // ── Origin Remastered (same layout as Steam Remastered) ──────────
    InstallRecipe {
        source: SourceId::OriginRemastered,
        package: PackageId::RaBase,
        actions: &[InstallAction::Copy {
            files: &REMASTERED_BASE_COPY,
        }],
    },
    InstallRecipe {
        source: SourceId::OriginRemastered,
        package: PackageId::RaAftermathBase,
        actions: &[InstallAction::Copy {
            files: &REMASTERED_AFTERMATH_COPY,
        }],
    },
    InstallRecipe {
        source: SourceId::OriginRemastered,
        package: PackageId::RaCncDesert,
        actions: &[InstallAction::Copy {
            files: &[FileMapping {
                from: "Data/CNCDATA/TIBERIAN_DAWN/CD1/DESERT.MIX",
                to: "cnc/desert.mix",
            }],
        }],
    },
    InstallRecipe {
        source: SourceId::OriginRemastered,
        package: PackageId::RaMusic,
        actions: &[InstallAction::ExtractMix {
            source_mix: "Data/CNCDATA/RED_ALERT/CD1/MAIN.MIX",
            entries: &[FileMapping {
                from: "scores.mix",
                to: "scores.mix",
            }],
        }],
    },
    InstallRecipe {
        source: SourceId::OriginRemastered,
        package: PackageId::RaMusicCounterstrike,
        actions: &[InstallAction::Copy {
            files: &REMASTERED_CS_MUSIC_COPY,
        }],
    },
    InstallRecipe {
        source: SourceId::OriginRemastered,
        package: PackageId::RaMusicAftermath,
        actions: &[InstallAction::Copy {
            files: &REMASTERED_AM_MUSIC_COPY,
        }],
    },
    // ── Allied Disc ───────────────────────────────────────────────────
    InstallRecipe {
        source: SourceId::AlliedDisc,
        package: PackageId::RaBase,
        actions: &[InstallAction::ExtractMix {
            source_mix: "INSTALL/REDALERT.MIX",
            entries: &BASE_FROM_REDALERT_MIX,
        }],
    },
    InstallRecipe {
        source: SourceId::AlliedDisc,
        package: PackageId::RaMusic,
        actions: &[InstallAction::ExtractMix {
            source_mix: "MAIN.MIX",
            entries: &[FileMapping {
                from: "scores.mix",
                to: "scores.mix",
            }],
        }],
    },
    InstallRecipe {
        source: SourceId::AlliedDisc,
        package: PackageId::RaMoviesAllied,
        actions: &MOVIES_ALLIED_FROM_MAIN_MIX,
    },
    // ── Soviet Disc ───────────────────────────────────────────────────
    InstallRecipe {
        source: SourceId::SovietDisc,
        package: PackageId::RaBase,
        actions: &[InstallAction::ExtractMix {
            source_mix: "INSTALL/REDALERT.MIX",
            entries: &BASE_FROM_REDALERT_MIX,
        }],
    },
    InstallRecipe {
        source: SourceId::SovietDisc,
        package: PackageId::RaMusic,
        actions: &[InstallAction::ExtractMix {
            source_mix: "MAIN.MIX",
            entries: &[FileMapping {
                from: "scores.mix",
                to: "scores.mix",
            }],
        }],
    },
    InstallRecipe {
        source: SourceId::SovietDisc,
        package: PackageId::RaMoviesSoviet,
        actions: &MOVIES_SOVIET_FROM_MAIN_MIX,
    },
    // ── Aftermath Disc ────────────────────────────────────────────────
    InstallRecipe {
        source: SourceId::AftermathDisc,
        package: PackageId::RaAftermathBase,
        actions: &AFTERMATH_DISC_ACTIONS,
    },
    InstallRecipe {
        source: SourceId::AftermathDisc,
        package: PackageId::RaMusicAftermath,
        actions: &AFTERMATH_DISC_MUSIC_ACTIONS,
    },
    // ── Counterstrike Disc ────────────────────────────────────────────
    InstallRecipe {
        source: SourceId::CounterstrikeDisc,
        package: PackageId::RaMusicCounterstrike,
        actions: &CS_DISC_MUSIC_ACTIONS,
    },
    // ── The First Decade ──────────────────────────────────────────────
    InstallRecipe {
        source: SourceId::TheFirstDecade,
        package: PackageId::RaBase,
        actions: &[InstallAction::ExtractIscab {
            header: "data1.hdr",
            volumes: &[
                (1, "data1.cab"),
                (2, "data2.cab"),
                (3, "data3.cab"),
                (4, "data4.cab"),
                (5, "data5.cab"),
            ],
            entries: &BASE_FROM_REDALERT_MIX,
        }],
    },
    InstallRecipe {
        source: SourceId::TheFirstDecade,
        package: PackageId::RaCncDesert,
        actions: &[InstallAction::ExtractIscab {
            header: "data1.hdr",
            volumes: &[
                (1, "data1.cab"),
                (2, "data2.cab"),
                (3, "data3.cab"),
                (4, "data4.cab"),
                (5, "data5.cab"),
            ],
            entries: &[FileMapping {
                from: "DESERT.MIX",
                to: "cnc/desert.mix",
            }],
        }],
    },
    // ── C&C 95 ────────────────────────────────────────────────────────
    InstallRecipe {
        source: SourceId::Cnc95,
        package: PackageId::RaCncDesert,
        actions: &[InstallAction::Copy {
            files: &[FileMapping {
                from: "DESERT.MIX",
                to: "cnc/desert.mix",
            }],
        }],
    },
    // ══════════════════════════════════════════════════════════════════════
    // Red Alert 2 recipes
    // ══════════════════════════════════════════════════════════════════════

    // ── Steam TUC (AppId 2229850) — RA2 + Yuri's Revenge ─────────────
    //
    // The Steam TUC install directory contains all MIX archives directly
    // in the root — no subdirectories. Both base RA2 and YR files coexist.
    InstallRecipe {
        source: SourceId::Ra2SteamTuc,
        package: PackageId::Ra2Base,
        actions: &[InstallAction::Copy {
            files: &RA2_BASE_COPY,
        }],
    },
    InstallRecipe {
        source: SourceId::Ra2SteamTuc,
        package: PackageId::Ra2YurisRevenge,
        actions: &[InstallAction::Copy {
            files: &RA2_YR_COPY,
        }],
    },
    InstallRecipe {
        source: SourceId::Ra2SteamTuc,
        package: PackageId::Ra2Music,
        actions: &[InstallAction::Copy {
            files: &RA2_MUSIC_COPY,
        }],
    },
    // ── Origin TUC (same layout as Steam TUC) ────────────────────────
    InstallRecipe {
        source: SourceId::Ra2OriginTuc,
        package: PackageId::Ra2Base,
        actions: &[InstallAction::Copy {
            files: &RA2_BASE_COPY,
        }],
    },
    InstallRecipe {
        source: SourceId::Ra2OriginTuc,
        package: PackageId::Ra2YurisRevenge,
        actions: &[InstallAction::Copy {
            files: &RA2_YR_COPY,
        }],
    },
    InstallRecipe {
        source: SourceId::Ra2OriginTuc,
        package: PackageId::Ra2Music,
        actions: &[InstallAction::Copy {
            files: &RA2_MUSIC_COPY,
        }],
    },
    // ══════════════════════════════════════════════════════════════════════
    // C&C Generals recipes
    // ══════════════════════════════════════════════════════════════════════

    // ── Steam TUC (AppId 2229870) — Generals + Zero Hour merged ──────
    //
    // The Steam TUC merges base Generals and Zero Hour into a single
    // install directory. All BIG archives sit in the root alongside loose
    // data under Data/. Zero Hour content is folded into the base archives
    // — there are no separate INIZH.big / W3DZH.big files.
    InstallRecipe {
        source: SourceId::GenSteamTuc,
        package: PackageId::GenBase,
        actions: &[InstallAction::Copy {
            files: &GEN_BASE_COPY,
        }],
    },
    // ── Origin TUC (same layout as Steam TUC) ────────────────────────
    InstallRecipe {
        source: SourceId::GenOriginTuc,
        package: PackageId::GenBase,
        actions: &[InstallAction::Copy {
            files: &GEN_BASE_COPY,
        }],
    },
];

mod ra_base;
use self::ra_base::{AFTERMATH_EXPAND_COPY, BASE_DIRECT_COPY, BASE_FROM_REDALERT_MIX};

mod ra_remastered;
use self::ra_remastered::{
    AM_MUSIC_COPY, CS_MUSIC_COPY, REMASTERED_AFTERMATH_COPY, REMASTERED_AM_MUSIC_COPY,
    REMASTERED_BASE_COPY, REMASTERED_CS_MUSIC_COPY,
};

mod ra_movies;
use self::ra_movies::{
    AFTERMATH_DISC_ACTIONS, AFTERMATH_DISC_MUSIC_ACTIONS, CS_DISC_MUSIC_ACTIONS,
    MOVIES_ALLIED_FROM_MAIN_MIX, MOVIES_SOVIET_FROM_MAIN_MIX,
};

mod ra2_gen;
use self::ra2_gen::{GEN_BASE_COPY, RA2_BASE_COPY, RA2_MUSIC_COPY, RA2_YR_COPY};
