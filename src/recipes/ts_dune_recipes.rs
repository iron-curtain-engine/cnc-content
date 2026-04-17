// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! Tiberian Sun and Dune install recipes.

use crate::actions::InstallAction;
use crate::{InstallRecipe, PackageId, SourceId};

use super::{
    DUNE2000_BASE_COPY, DUNE2_BASE_COPY, TS_BASE_COPY, TS_FIRESTORM_COPY, TS_MOVIES_COPY,
    TS_MUSIC_COPY,
};

pub(super) static TS_DUNE_RECIPES: &[InstallRecipe] = &[
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
