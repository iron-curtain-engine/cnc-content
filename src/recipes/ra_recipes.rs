// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! Red Alert install recipes — all source-package combinations for RA.

use crate::actions::{FileMapping, InstallAction};
use crate::{InstallRecipe, PackageId, SourceId};

use super::{
    AFTERMATH_DISC_ACTIONS, AFTERMATH_DISC_MUSIC_ACTIONS, AFTERMATH_EXPAND_COPY, AM_MUSIC_COPY,
    BASE_DIRECT_COPY, BASE_FROM_REDALERT_MIX, CS_DISC_MUSIC_ACTIONS, CS_MUSIC_COPY,
    MOVIES_ALLIED_FROM_MAIN_MIX, MOVIES_ALLIED_FROM_REMASTERED_MIX, MOVIES_ALLIED_VQA_ENTRIES,
    MOVIES_SOVIET_FROM_MAIN_MIX, MOVIES_SOVIET_FROM_REMASTERED_MIX, MOVIES_SOVIET_VQA_ENTRIES,
    REMASTERED_AFTERMATH_COPY, REMASTERED_AM_MUSIC_COPY, REMASTERED_BASE_COPY,
    REMASTERED_CS_MUSIC_COPY, TFD_VOLUMES,
};

pub(super) static RA_RECIPES: &[InstallRecipe] = &[
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
];
