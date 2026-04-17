// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! Red Alert 2, Generals, and Tiberian Dawn install recipes.

use crate::actions::InstallAction;
use crate::{InstallRecipe, PackageId, SourceId};

use super::{
    GEN_BASE_COPY, GEN_ZH_COPY, RA2_BASE_COPY, RA2_DISC_BASE_COPY, RA2_DISC_MOVIES_COPY,
    RA2_DISC_YR_COPY, RA2_MOVIES_COPY, RA2_MUSIC_COPY, RA2_YR_COPY, TD_BASE_COPY,
    TD_COVERT_OPS_COPY, TD_MOVIES_GDI_DISC_COPY, TD_MOVIES_GDI_STEAM_COPY, TD_MOVIES_NOD_DISC_COPY,
    TD_MOVIES_NOD_STEAM_COPY, TD_MUSIC_COPY, TD_REMASTERED_BASE_COPY,
};

pub(super) static RA2TD_RECIPES: &[InstallRecipe] = &[
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
];
