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
    InstallRecipe {
        source: SourceId::SteamRemastered,
        package: PackageId::RaMoviesAllied,
        actions: &MOVIES_ALLIED_FROM_REMASTERED_MIX,
    },
    InstallRecipe {
        source: SourceId::SteamRemastered,
        package: PackageId::RaMoviesSoviet,
        actions: &MOVIES_SOVIET_FROM_REMASTERED_MIX,
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
    InstallRecipe {
        source: SourceId::OriginRemastered,
        package: PackageId::RaMoviesAllied,
        actions: &MOVIES_ALLIED_FROM_REMASTERED_MIX,
    },
    InstallRecipe {
        source: SourceId::OriginRemastered,
        package: PackageId::RaMoviesSoviet,
        actions: &MOVIES_SOVIET_FROM_REMASTERED_MIX,
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
    //
    // TFD uses InstallShield CAB archives. All RA content (base, aftermath,
    // music, movies) is stored across data1–data5.cab volumes.
    InstallRecipe {
        source: SourceId::TheFirstDecade,
        package: PackageId::RaBase,
        actions: &[InstallAction::ExtractIscab {
            header: "data1.hdr",
            volumes: &TFD_VOLUMES,
            entries: &BASE_FROM_REDALERT_MIX,
        }],
    },
    InstallRecipe {
        source: SourceId::TheFirstDecade,
        package: PackageId::RaCncDesert,
        actions: &[InstallAction::ExtractIscab {
            header: "data1.hdr",
            volumes: &TFD_VOLUMES,
            entries: &[FileMapping {
                from: "DESERT.MIX",
                to: "cnc/desert.mix",
            }],
        }],
    },
    InstallRecipe {
        source: SourceId::TheFirstDecade,
        package: PackageId::RaAftermathBase,
        actions: &[InstallAction::ExtractIscab {
            header: "data1.hdr",
            volumes: &TFD_VOLUMES,
            entries: &AFTERMATH_EXPAND_COPY,
        }],
    },
    InstallRecipe {
        source: SourceId::TheFirstDecade,
        package: PackageId::RaMusic,
        actions: &[InstallAction::ExtractIscab {
            header: "data1.hdr",
            volumes: &TFD_VOLUMES,
            entries: &[FileMapping {
                from: "scores.mix",
                to: "scores.mix",
            }],
        }],
    },
    InstallRecipe {
        source: SourceId::TheFirstDecade,
        package: PackageId::RaMoviesAllied,
        actions: &[InstallAction::ExtractIscab {
            header: "data1.hdr",
            volumes: &TFD_VOLUMES,
            entries: &MOVIES_ALLIED_VQA_ENTRIES,
        }],
    },
    InstallRecipe {
        source: SourceId::TheFirstDecade,
        package: PackageId::RaMoviesSoviet,
        actions: &[InstallAction::ExtractIscab {
            header: "data1.hdr",
            volumes: &TFD_VOLUMES,
            entries: &MOVIES_SOVIET_VQA_ENTRIES,
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
    // ── Retail disc ──────────────────────────────────────────────────
    InstallRecipe {
        source: SourceId::GenDisc,
        package: PackageId::GenBase,
        actions: &[InstallAction::Copy {
            files: &GEN_BASE_COPY,
        }],
    },
    // ── Zero Hour disc ───────────────────────────────────────────────
    InstallRecipe {
        source: SourceId::GenZhDisc,
        package: PackageId::GenZeroHour,
        actions: &[InstallAction::Copy {
            files: &GEN_ZH_COPY,
        }],
    },
    // ── RA2 disc sources ─────────────────────────────────────────────
    //
    // The retail RA2 disc and Yuri's Revenge disc use uppercase filenames.
    InstallRecipe {
        source: SourceId::Ra2Disc,
        package: PackageId::Ra2Base,
        actions: &[InstallAction::Copy {
            files: &RA2_DISC_BASE_COPY,
        }],
    },
    InstallRecipe {
        source: SourceId::Ra2Disc,
        package: PackageId::Ra2Music,
        actions: &[InstallAction::Copy {
            files: &RA2_MUSIC_COPY,
        }],
    },
    InstallRecipe {
        source: SourceId::Ra2Disc,
        package: PackageId::Ra2Movies,
        actions: &[InstallAction::Copy {
            files: &RA2_DISC_MOVIES_COPY,
        }],
    },
    InstallRecipe {
        source: SourceId::Ra2YrDisc,
        package: PackageId::Ra2YurisRevenge,
        actions: &[InstallAction::Copy {
            files: &RA2_DISC_YR_COPY,
        }],
    },
    // ── The First Decade (RA2 + YR) ──────────────────────────────────
    //
    // TFD installs use the same uppercase naming as the retail disc.
    InstallRecipe {
        source: SourceId::Ra2TheFirstDecade,
        package: PackageId::Ra2Base,
        actions: &[InstallAction::Copy {
            files: &RA2_DISC_BASE_COPY,
        }],
    },
    InstallRecipe {
        source: SourceId::Ra2TheFirstDecade,
        package: PackageId::Ra2YurisRevenge,
        actions: &[InstallAction::Copy {
            files: &RA2_DISC_YR_COPY,
        }],
    },
    InstallRecipe {
        source: SourceId::Ra2TheFirstDecade,
        package: PackageId::Ra2Music,
        actions: &[InstallAction::Copy {
            files: &RA2_MUSIC_COPY,
        }],
    },
    InstallRecipe {
        source: SourceId::Ra2TheFirstDecade,
        package: PackageId::Ra2Movies,
        actions: &[InstallAction::Copy {
            files: &RA2_DISC_MOVIES_COPY,
        }],
    },
    // ── RA2 Steam/Origin TUC — movies ────────────────────────────────
    InstallRecipe {
        source: SourceId::Ra2SteamTuc,
        package: PackageId::Ra2Movies,
        actions: &[InstallAction::Copy {
            files: &RA2_MOVIES_COPY,
        }],
    },
    InstallRecipe {
        source: SourceId::Ra2OriginTuc,
        package: PackageId::Ra2Movies,
        actions: &[InstallAction::Copy {
            files: &RA2_MOVIES_COPY,
        }],
    },
    // ══════════════════════════════════════════════════════════════════════
    // Tiberian Dawn recipes
    // ══════════════════════════════════════════════════════════════════════

    // ── GDI Disc ─────────────────────────────────────────────────────
    //
    // The GDI disc has base MIX files and GDI campaign movies at the root.
    InstallRecipe {
        source: SourceId::TdGdiDisc,
        package: PackageId::TdBase,
        actions: &[InstallAction::Copy {
            files: &TD_BASE_COPY,
        }],
    },
    InstallRecipe {
        source: SourceId::TdGdiDisc,
        package: PackageId::TdMusic,
        actions: &[InstallAction::Copy {
            files: &TD_MUSIC_COPY,
        }],
    },
    InstallRecipe {
        source: SourceId::TdGdiDisc,
        package: PackageId::TdMoviesGdi,
        actions: &[InstallAction::Copy {
            files: &TD_MOVIES_GDI_DISC_COPY,
        }],
    },
    // ── Nod Disc ─────────────────────────────────────────────────────
    //
    // The Nod disc has the same base MIX files but Nod campaign movies.
    InstallRecipe {
        source: SourceId::TdNodDisc,
        package: PackageId::TdBase,
        actions: &[InstallAction::Copy {
            files: &TD_BASE_COPY,
        }],
    },
    InstallRecipe {
        source: SourceId::TdNodDisc,
        package: PackageId::TdMusic,
        actions: &[InstallAction::Copy {
            files: &TD_MUSIC_COPY,
        }],
    },
    InstallRecipe {
        source: SourceId::TdNodDisc,
        package: PackageId::TdMoviesNod,
        actions: &[InstallAction::Copy {
            files: &TD_MOVIES_NOD_DISC_COPY,
        }],
    },
    // ── Covert Operations Disc ───────────────────────────────────────
    InstallRecipe {
        source: SourceId::TdCovertOpsDisc,
        package: PackageId::TdCovertOps,
        actions: &[InstallAction::Copy {
            files: &TD_COVERT_OPS_COPY,
        }],
    },
    // ── Steam C&C (AppId 2229830) ────────────────────────────────────
    //
    // Contains TD base game, Covert Ops, and all movies in a flat
    // directory with a MOVIES/ subdirectory for VQA files.
    InstallRecipe {
        source: SourceId::TdSteamCnc,
        package: PackageId::TdBase,
        actions: &[InstallAction::Copy {
            files: &TD_BASE_COPY,
        }],
    },
    InstallRecipe {
        source: SourceId::TdSteamCnc,
        package: PackageId::TdCovertOps,
        actions: &[InstallAction::Copy {
            files: &TD_COVERT_OPS_COPY,
        }],
    },
    InstallRecipe {
        source: SourceId::TdSteamCnc,
        package: PackageId::TdMusic,
        actions: &[InstallAction::Copy {
            files: &TD_MUSIC_COPY,
        }],
    },
    InstallRecipe {
        source: SourceId::TdSteamCnc,
        package: PackageId::TdMoviesGdi,
        actions: &[InstallAction::Copy {
            files: &TD_MOVIES_GDI_STEAM_COPY,
        }],
    },
    InstallRecipe {
        source: SourceId::TdSteamCnc,
        package: PackageId::TdMoviesNod,
        actions: &[InstallAction::Copy {
            files: &TD_MOVIES_NOD_STEAM_COPY,
        }],
    },
    // ── Steam Remastered (AppId 1213210) — TD data ───────────────────
    //
    // The Remastered Collection nests TD files under a deep path.
    // Only base game files are accessible — the remastered edition
    // stores movies and music in its own proprietary format.
    InstallRecipe {
        source: SourceId::TdSteamRemastered,
        package: PackageId::TdBase,
        actions: &[InstallAction::Copy {
            files: &TD_REMASTERED_BASE_COPY,
        }],
    },
    // ── Origin C&C (same flat layout as Steam C&C) ───────────────────
    InstallRecipe {
        source: SourceId::TdOriginCnc,
        package: PackageId::TdBase,
        actions: &[InstallAction::Copy {
            files: &TD_BASE_COPY,
        }],
    },
    InstallRecipe {
        source: SourceId::TdOriginCnc,
        package: PackageId::TdCovertOps,
        actions: &[InstallAction::Copy {
            files: &TD_COVERT_OPS_COPY,
        }],
    },
    InstallRecipe {
        source: SourceId::TdOriginCnc,
        package: PackageId::TdMusic,
        actions: &[InstallAction::Copy {
            files: &TD_MUSIC_COPY,
        }],
    },
    InstallRecipe {
        source: SourceId::TdOriginCnc,
        package: PackageId::TdMoviesGdi,
        actions: &[InstallAction::Copy {
            files: &TD_MOVIES_GDI_STEAM_COPY,
        }],
    },
    InstallRecipe {
        source: SourceId::TdOriginCnc,
        package: PackageId::TdMoviesNod,
        actions: &[InstallAction::Copy {
            files: &TD_MOVIES_NOD_STEAM_COPY,
        }],
    },
    // ══════════════════════════════════════════════════════════════════════
    // Tiberian Sun recipes
    // ══════════════════════════════════════════════════════════════════════

    // ── Retail disc ──────────────────────────────────────────────────
    InstallRecipe {
        source: SourceId::TsDisc,
        package: PackageId::TsBase,
        actions: &[InstallAction::Copy {
            files: &TS_BASE_COPY,
        }],
    },
    InstallRecipe {
        source: SourceId::TsDisc,
        package: PackageId::TsMusic,
        actions: &[InstallAction::Copy {
            files: &TS_MUSIC_COPY,
        }],
    },
    InstallRecipe {
        source: SourceId::TsDisc,
        package: PackageId::TsMovies,
        actions: &[InstallAction::Copy {
            files: &TS_MOVIES_COPY,
        }],
    },
    // ── Firestorm disc ───────────────────────────────────────────────
    InstallRecipe {
        source: SourceId::TsFirestormDisc,
        package: PackageId::TsFirestorm,
        actions: &[InstallAction::Copy {
            files: &TS_FIRESTORM_COPY,
        }],
    },
    // ── Steam TUC ────────────────────────────────────────────────────
    //
    // The Steam TUC contains both base TS and Firestorm content in
    // a single flat directory.
    InstallRecipe {
        source: SourceId::TsSteamTuc,
        package: PackageId::TsBase,
        actions: &[InstallAction::Copy {
            files: &TS_BASE_COPY,
        }],
    },
    InstallRecipe {
        source: SourceId::TsSteamTuc,
        package: PackageId::TsFirestorm,
        actions: &[InstallAction::Copy {
            files: &TS_FIRESTORM_COPY,
        }],
    },
    InstallRecipe {
        source: SourceId::TsSteamTuc,
        package: PackageId::TsMusic,
        actions: &[InstallAction::Copy {
            files: &TS_MUSIC_COPY,
        }],
    },
    InstallRecipe {
        source: SourceId::TsSteamTuc,
        package: PackageId::TsMovies,
        actions: &[InstallAction::Copy {
            files: &TS_MOVIES_COPY,
        }],
    },
    // ── Origin TUC (same layout as Steam TUC) ────────────────────────
    InstallRecipe {
        source: SourceId::TsOriginTuc,
        package: PackageId::TsBase,
        actions: &[InstallAction::Copy {
            files: &TS_BASE_COPY,
        }],
    },
    InstallRecipe {
        source: SourceId::TsOriginTuc,
        package: PackageId::TsFirestorm,
        actions: &[InstallAction::Copy {
            files: &TS_FIRESTORM_COPY,
        }],
    },
    InstallRecipe {
        source: SourceId::TsOriginTuc,
        package: PackageId::TsMusic,
        actions: &[InstallAction::Copy {
            files: &TS_MUSIC_COPY,
        }],
    },
    InstallRecipe {
        source: SourceId::TsOriginTuc,
        package: PackageId::TsMovies,
        actions: &[InstallAction::Copy {
            files: &TS_MOVIES_COPY,
        }],
    },
    // ══════════════════════════════════════════════════════════════════════
    // Dune 2 recipes — local source only (NOT freeware)
    // ══════════════════════════════════════════════════════════════════════
    InstallRecipe {
        source: SourceId::Dune2Disc,
        package: PackageId::Dune2Base,
        actions: &[InstallAction::Copy {
            files: &DUNE2_BASE_COPY,
        }],
    },
    InstallRecipe {
        source: SourceId::GogDune2,
        package: PackageId::Dune2Base,
        actions: &[InstallAction::Copy {
            files: &DUNE2_BASE_COPY,
        }],
    },
    // ══════════════════════════════════════════════════════════════════════
    // Dune 2000 recipes — local source only (NOT freeware)
    // ══════════════════════════════════════════════════════════════════════
    InstallRecipe {
        source: SourceId::Dune2000Disc,
        package: PackageId::Dune2000Base,
        actions: &[InstallAction::Copy {
            files: &DUNE2000_BASE_COPY,
        }],
    },
    InstallRecipe {
        source: SourceId::GogDune2000,
        package: PackageId::Dune2000Base,
        actions: &[InstallAction::Copy {
            files: &DUNE2000_BASE_COPY,
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
    MOVIES_ALLIED_FROM_MAIN_MIX, MOVIES_ALLIED_FROM_REMASTERED_MIX, MOVIES_ALLIED_VQA_ENTRIES,
    MOVIES_SOVIET_FROM_MAIN_MIX, MOVIES_SOVIET_FROM_REMASTERED_MIX, MOVIES_SOVIET_VQA_ENTRIES,
    TFD_VOLUMES,
};

mod ra2_gen;
use self::ra2_gen::{
    GEN_BASE_COPY, GEN_ZH_COPY, RA2_BASE_COPY, RA2_DISC_BASE_COPY, RA2_DISC_MOVIES_COPY,
    RA2_DISC_YR_COPY, RA2_MOVIES_COPY, RA2_MUSIC_COPY, RA2_YR_COPY,
};

mod td;
use self::td::{
    TD_BASE_COPY, TD_COVERT_OPS_COPY, TD_MOVIES_GDI_DISC_COPY, TD_MOVIES_GDI_STEAM_COPY,
    TD_MOVIES_NOD_DISC_COPY, TD_MOVIES_NOD_STEAM_COPY, TD_MUSIC_COPY, TD_REMASTERED_BASE_COPY,
};

mod ts;
use self::ts::{TS_BASE_COPY, TS_FIRESTORM_COPY, TS_MOVIES_COPY, TS_MUSIC_COPY};

mod dune;
use self::dune::{DUNE2000_BASE_COPY, DUNE2_BASE_COPY};

#[cfg(test)]
mod tests;
