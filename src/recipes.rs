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
        package: PackageId::Base,
        actions: &[InstallAction::Copy {
            files: &BASE_DIRECT_COPY,
        }],
    },
    InstallRecipe {
        source: SourceId::SteamTuc,
        package: PackageId::AftermathBase,
        actions: &[InstallAction::Copy {
            files: &AFTERMATH_EXPAND_COPY,
        }],
    },
    InstallRecipe {
        source: SourceId::SteamTuc,
        package: PackageId::Music,
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
        package: PackageId::MoviesAllied,
        actions: &MOVIES_ALLIED_FROM_MAIN_MIX,
    },
    InstallRecipe {
        source: SourceId::SteamTuc,
        package: PackageId::MoviesSoviet,
        actions: &MOVIES_SOVIET_FROM_MAIN_MIX,
    },
    InstallRecipe {
        source: SourceId::SteamTuc,
        package: PackageId::MusicCounterstrike,
        actions: &[InstallAction::Copy {
            files: &CS_MUSIC_COPY,
        }],
    },
    InstallRecipe {
        source: SourceId::SteamTuc,
        package: PackageId::MusicAftermath,
        actions: &[InstallAction::Copy {
            files: &AM_MUSIC_COPY,
        }],
    },
    // ── Steam Remastered (AppId 1213210) ──────────────────────────────
    InstallRecipe {
        source: SourceId::SteamRemastered,
        package: PackageId::Base,
        actions: &[InstallAction::Copy {
            files: &REMASTERED_BASE_COPY,
        }],
    },
    InstallRecipe {
        source: SourceId::SteamRemastered,
        package: PackageId::AftermathBase,
        actions: &[InstallAction::Copy {
            files: &REMASTERED_AFTERMATH_COPY,
        }],
    },
    InstallRecipe {
        source: SourceId::SteamRemastered,
        package: PackageId::CncDesert,
        actions: &[InstallAction::Copy {
            files: &[FileMapping {
                from: "Data/CNCDATA/TIBERIAN_DAWN/CD1/DESERT.MIX",
                to: "cnc/desert.mix",
            }],
        }],
    },
    InstallRecipe {
        source: SourceId::SteamRemastered,
        package: PackageId::Music,
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
        package: PackageId::MusicCounterstrike,
        actions: &[InstallAction::Copy {
            files: &REMASTERED_CS_MUSIC_COPY,
        }],
    },
    InstallRecipe {
        source: SourceId::SteamRemastered,
        package: PackageId::MusicAftermath,
        actions: &[InstallAction::Copy {
            files: &REMASTERED_AM_MUSIC_COPY,
        }],
    },
    // ── Steam C&C (AppId 2229830) — desert only ──────────────────────
    InstallRecipe {
        source: SourceId::SteamCnc,
        package: PackageId::CncDesert,
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
        package: PackageId::Base,
        actions: &[InstallAction::Copy {
            files: &BASE_DIRECT_COPY,
        }],
    },
    InstallRecipe {
        source: SourceId::OriginTuc,
        package: PackageId::AftermathBase,
        actions: &[InstallAction::Copy {
            files: &AFTERMATH_EXPAND_COPY,
        }],
    },
    InstallRecipe {
        source: SourceId::OriginTuc,
        package: PackageId::Music,
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
        package: PackageId::MoviesAllied,
        actions: &MOVIES_ALLIED_FROM_MAIN_MIX,
    },
    InstallRecipe {
        source: SourceId::OriginTuc,
        package: PackageId::MoviesSoviet,
        actions: &MOVIES_SOVIET_FROM_MAIN_MIX,
    },
    InstallRecipe {
        source: SourceId::OriginTuc,
        package: PackageId::MusicCounterstrike,
        actions: &[InstallAction::Copy {
            files: &CS_MUSIC_COPY,
        }],
    },
    InstallRecipe {
        source: SourceId::OriginTuc,
        package: PackageId::MusicAftermath,
        actions: &[InstallAction::Copy {
            files: &AM_MUSIC_COPY,
        }],
    },
    // ── Origin C&C — desert only ─────────────────────────────────────
    InstallRecipe {
        source: SourceId::OriginCnc,
        package: PackageId::CncDesert,
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
        package: PackageId::Base,
        actions: &[InstallAction::Copy {
            files: &REMASTERED_BASE_COPY,
        }],
    },
    InstallRecipe {
        source: SourceId::OriginRemastered,
        package: PackageId::AftermathBase,
        actions: &[InstallAction::Copy {
            files: &REMASTERED_AFTERMATH_COPY,
        }],
    },
    InstallRecipe {
        source: SourceId::OriginRemastered,
        package: PackageId::CncDesert,
        actions: &[InstallAction::Copy {
            files: &[FileMapping {
                from: "Data/CNCDATA/TIBERIAN_DAWN/CD1/DESERT.MIX",
                to: "cnc/desert.mix",
            }],
        }],
    },
    InstallRecipe {
        source: SourceId::OriginRemastered,
        package: PackageId::Music,
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
        package: PackageId::MusicCounterstrike,
        actions: &[InstallAction::Copy {
            files: &REMASTERED_CS_MUSIC_COPY,
        }],
    },
    InstallRecipe {
        source: SourceId::OriginRemastered,
        package: PackageId::MusicAftermath,
        actions: &[InstallAction::Copy {
            files: &REMASTERED_AM_MUSIC_COPY,
        }],
    },
    // ── Allied Disc ───────────────────────────────────────────────────
    InstallRecipe {
        source: SourceId::AlliedDisc,
        package: PackageId::Base,
        actions: &[InstallAction::ExtractMix {
            source_mix: "INSTALL/REDALERT.MIX",
            entries: &BASE_FROM_REDALERT_MIX,
        }],
    },
    InstallRecipe {
        source: SourceId::AlliedDisc,
        package: PackageId::Music,
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
        package: PackageId::MoviesAllied,
        actions: &MOVIES_ALLIED_FROM_MAIN_MIX,
    },
    // ── Soviet Disc ───────────────────────────────────────────────────
    InstallRecipe {
        source: SourceId::SovietDisc,
        package: PackageId::Base,
        actions: &[InstallAction::ExtractMix {
            source_mix: "INSTALL/REDALERT.MIX",
            entries: &BASE_FROM_REDALERT_MIX,
        }],
    },
    InstallRecipe {
        source: SourceId::SovietDisc,
        package: PackageId::Music,
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
        package: PackageId::MoviesSoviet,
        actions: &MOVIES_SOVIET_FROM_MAIN_MIX,
    },
    // ── Aftermath Disc ────────────────────────────────────────────────
    InstallRecipe {
        source: SourceId::AftermathDisc,
        package: PackageId::AftermathBase,
        actions: &AFTERMATH_DISC_ACTIONS,
    },
    InstallRecipe {
        source: SourceId::AftermathDisc,
        package: PackageId::MusicAftermath,
        actions: &AFTERMATH_DISC_MUSIC_ACTIONS,
    },
    // ── Counterstrike Disc ────────────────────────────────────────────
    InstallRecipe {
        source: SourceId::CounterstrikeDisc,
        package: PackageId::MusicCounterstrike,
        actions: &CS_DISC_MUSIC_ACTIONS,
    },
    // ── The First Decade ──────────────────────────────────────────────
    InstallRecipe {
        source: SourceId::TheFirstDecade,
        package: PackageId::Base,
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
        package: PackageId::CncDesert,
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
        package: PackageId::CncDesert,
        actions: &[InstallAction::Copy {
            files: &[FileMapping {
                from: "DESERT.MIX",
                to: "cnc/desert.mix",
            }],
        }],
    },
];

// ══════════════════════════════════════════════════════════════════════
//  Static file mapping tables
// ══════════════════════════════════════════════════════════════════════

/// Base game files — direct copy (Steam TUC, Origin TUC layout).
static BASE_DIRECT_COPY: [FileMapping; 11] = [
    FileMapping {
        from: "allies.mix",
        to: "allies.mix",
    },
    FileMapping {
        from: "conquer.mix",
        to: "conquer.mix",
    },
    FileMapping {
        from: "interior.mix",
        to: "interior.mix",
    },
    FileMapping {
        from: "hires.mix",
        to: "hires.mix",
    },
    FileMapping {
        from: "lores.mix",
        to: "lores.mix",
    },
    FileMapping {
        from: "local.mix",
        to: "local.mix",
    },
    FileMapping {
        from: "speech.mix",
        to: "speech.mix",
    },
    FileMapping {
        from: "russian.mix",
        to: "russian.mix",
    },
    FileMapping {
        from: "snow.mix",
        to: "snow.mix",
    },
    FileMapping {
        from: "sounds.mix",
        to: "sounds.mix",
    },
    FileMapping {
        from: "temperat.mix",
        to: "temperat.mix",
    },
];

/// Base game files — extracted from INSTALL/REDALERT.MIX (disc layout).
static BASE_FROM_REDALERT_MIX: [FileMapping; 11] = [
    FileMapping {
        from: "allies.mix",
        to: "allies.mix",
    },
    FileMapping {
        from: "conquer.mix",
        to: "conquer.mix",
    },
    FileMapping {
        from: "interior.mix",
        to: "interior.mix",
    },
    FileMapping {
        from: "hires.mix",
        to: "hires.mix",
    },
    FileMapping {
        from: "lores.mix",
        to: "lores.mix",
    },
    FileMapping {
        from: "local.mix",
        to: "local.mix",
    },
    FileMapping {
        from: "speech.mix",
        to: "speech.mix",
    },
    FileMapping {
        from: "russian.mix",
        to: "russian.mix",
    },
    FileMapping {
        from: "snow.mix",
        to: "snow.mix",
    },
    FileMapping {
        from: "sounds.mix",
        to: "sounds.mix",
    },
    FileMapping {
        from: "temperat.mix",
        to: "temperat.mix",
    },
];

/// Aftermath expansion files — direct copy from expand/.
static AFTERMATH_EXPAND_COPY: [FileMapping; 27] = [
    FileMapping {
        from: "expand/expand2.mix",
        to: "expand/expand2.mix",
    },
    FileMapping {
        from: "expand/hires1.mix",
        to: "expand/hires1.mix",
    },
    FileMapping {
        from: "expand/lores1.mix",
        to: "expand/lores1.mix",
    },
    FileMapping {
        from: "expand/chrotnk1.aud",
        to: "expand/chrotnk1.aud",
    },
    FileMapping {
        from: "expand/fixit1.aud",
        to: "expand/fixit1.aud",
    },
    FileMapping {
        from: "expand/jburn1.aud",
        to: "expand/jburn1.aud",
    },
    FileMapping {
        from: "expand/jchrge1.aud",
        to: "expand/jchrge1.aud",
    },
    FileMapping {
        from: "expand/jcrisp1.aud",
        to: "expand/jcrisp1.aud",
    },
    FileMapping {
        from: "expand/jdance1.aud",
        to: "expand/jdance1.aud",
    },
    FileMapping {
        from: "expand/jjuice1.aud",
        to: "expand/jjuice1.aud",
    },
    FileMapping {
        from: "expand/jjump1.aud",
        to: "expand/jjump1.aud",
    },
    FileMapping {
        from: "expand/jlight1.aud",
        to: "expand/jlight1.aud",
    },
    FileMapping {
        from: "expand/jpower1.aud",
        to: "expand/jpower1.aud",
    },
    FileMapping {
        from: "expand/jshock1.aud",
        to: "expand/jshock1.aud",
    },
    FileMapping {
        from: "expand/jyes1.aud",
        to: "expand/jyes1.aud",
    },
    FileMapping {
        from: "expand/madchrg2.aud",
        to: "expand/madchrg2.aud",
    },
    FileMapping {
        from: "expand/madexplo.aud",
        to: "expand/madexplo.aud",
    },
    FileMapping {
        from: "expand/mboss1.aud",
        to: "expand/mboss1.aud",
    },
    FileMapping {
        from: "expand/mhear1.aud",
        to: "expand/mhear1.aud",
    },
    FileMapping {
        from: "expand/mhotdig1.aud",
        to: "expand/mhotdig1.aud",
    },
    FileMapping {
        from: "expand/mhowdy1.aud",
        to: "expand/mhowdy1.aud",
    },
    FileMapping {
        from: "expand/mhuh1.aud",
        to: "expand/mhuh1.aud",
    },
    FileMapping {
        from: "expand/mlaff1.aud",
        to: "expand/mlaff1.aud",
    },
    FileMapping {
        from: "expand/mrise1.aud",
        to: "expand/mrise1.aud",
    },
    FileMapping {
        from: "expand/mwrench1.aud",
        to: "expand/mwrench1.aud",
    },
    FileMapping {
        from: "expand/myeehaw1.aud",
        to: "expand/myeehaw1.aud",
    },
    FileMapping {
        from: "expand/myes1.aud",
        to: "expand/myes1.aud",
    },
];

/// Counterstrike music — direct copy.
static CS_MUSIC_COPY: [FileMapping; 8] = [
    FileMapping {
        from: "expand/2nd_hand.aud",
        to: "expand/2nd_hand.aud",
    },
    FileMapping {
        from: "expand/araziod.aud",
        to: "expand/araziod.aud",
    },
    FileMapping {
        from: "expand/backstab.aud",
        to: "expand/backstab.aud",
    },
    FileMapping {
        from: "expand/chaos2.aud",
        to: "expand/chaos2.aud",
    },
    FileMapping {
        from: "expand/shut_it.aud",
        to: "expand/shut_it.aud",
    },
    FileMapping {
        from: "expand/twinmix1.aud",
        to: "expand/twinmix1.aud",
    },
    FileMapping {
        from: "expand/under3.aud",
        to: "expand/under3.aud",
    },
    FileMapping {
        from: "expand/vr2.aud",
        to: "expand/vr2.aud",
    },
];

/// Aftermath music — direct copy.
static AM_MUSIC_COPY: [FileMapping; 9] = [
    FileMapping {
        from: "expand/await.aud",
        to: "expand/await.aud",
    },
    FileMapping {
        from: "expand/bog.aud",
        to: "expand/bog.aud",
    },
    FileMapping {
        from: "expand/float_v2.aud",
        to: "expand/float_v2.aud",
    },
    FileMapping {
        from: "expand/gloom.aud",
        to: "expand/gloom.aud",
    },
    FileMapping {
        from: "expand/grndwire.aud",
        to: "expand/grndwire.aud",
    },
    FileMapping {
        from: "expand/rpt.aud",
        to: "expand/rpt.aud",
    },
    FileMapping {
        from: "expand/search.aud",
        to: "expand/search.aud",
    },
    FileMapping {
        from: "expand/traction.aud",
        to: "expand/traction.aud",
    },
    FileMapping {
        from: "expand/wastelnd.aud",
        to: "expand/wastelnd.aud",
    },
];

// ── Remastered layout ─────────────────────────────────────────────────
// Remastered stores files under Data/CNCDATA/RED_ALERT/{CD1,CD2}/.

static REMASTERED_BASE_COPY: [FileMapping; 11] = [
    FileMapping {
        from: "Data/CNCDATA/RED_ALERT/CD1/allies.mix",
        to: "allies.mix",
    },
    FileMapping {
        from: "Data/CNCDATA/RED_ALERT/CD1/conquer.mix",
        to: "conquer.mix",
    },
    FileMapping {
        from: "Data/CNCDATA/RED_ALERT/CD1/interior.mix",
        to: "interior.mix",
    },
    FileMapping {
        from: "Data/CNCDATA/RED_ALERT/CD1/hires.mix",
        to: "hires.mix",
    },
    FileMapping {
        from: "Data/CNCDATA/RED_ALERT/CD1/lores.mix",
        to: "lores.mix",
    },
    FileMapping {
        from: "Data/CNCDATA/RED_ALERT/CD1/local.mix",
        to: "local.mix",
    },
    FileMapping {
        from: "Data/CNCDATA/RED_ALERT/CD1/speech.mix",
        to: "speech.mix",
    },
    FileMapping {
        from: "Data/CNCDATA/RED_ALERT/CD1/russian.mix",
        to: "russian.mix",
    },
    FileMapping {
        from: "Data/CNCDATA/RED_ALERT/CD1/snow.mix",
        to: "snow.mix",
    },
    FileMapping {
        from: "Data/CNCDATA/RED_ALERT/CD1/sounds.mix",
        to: "sounds.mix",
    },
    FileMapping {
        from: "Data/CNCDATA/RED_ALERT/CD1/temperat.mix",
        to: "temperat.mix",
    },
];

static REMASTERED_AFTERMATH_COPY: [FileMapping; 27] = [
    FileMapping {
        from: "Data/CNCDATA/RED_ALERT/AFTERMATH/expand2.mix",
        to: "expand/expand2.mix",
    },
    FileMapping {
        from: "Data/CNCDATA/RED_ALERT/AFTERMATH/hires1.mix",
        to: "expand/hires1.mix",
    },
    FileMapping {
        from: "Data/CNCDATA/RED_ALERT/AFTERMATH/lores1.mix",
        to: "expand/lores1.mix",
    },
    FileMapping {
        from: "Data/CNCDATA/RED_ALERT/AFTERMATH/chrotnk1.aud",
        to: "expand/chrotnk1.aud",
    },
    FileMapping {
        from: "Data/CNCDATA/RED_ALERT/AFTERMATH/fixit1.aud",
        to: "expand/fixit1.aud",
    },
    FileMapping {
        from: "Data/CNCDATA/RED_ALERT/AFTERMATH/jburn1.aud",
        to: "expand/jburn1.aud",
    },
    FileMapping {
        from: "Data/CNCDATA/RED_ALERT/AFTERMATH/jchrge1.aud",
        to: "expand/jchrge1.aud",
    },
    FileMapping {
        from: "Data/CNCDATA/RED_ALERT/AFTERMATH/jcrisp1.aud",
        to: "expand/jcrisp1.aud",
    },
    FileMapping {
        from: "Data/CNCDATA/RED_ALERT/AFTERMATH/jdance1.aud",
        to: "expand/jdance1.aud",
    },
    FileMapping {
        from: "Data/CNCDATA/RED_ALERT/AFTERMATH/jjuice1.aud",
        to: "expand/jjuice1.aud",
    },
    FileMapping {
        from: "Data/CNCDATA/RED_ALERT/AFTERMATH/jjump1.aud",
        to: "expand/jjump1.aud",
    },
    FileMapping {
        from: "Data/CNCDATA/RED_ALERT/AFTERMATH/jlight1.aud",
        to: "expand/jlight1.aud",
    },
    FileMapping {
        from: "Data/CNCDATA/RED_ALERT/AFTERMATH/jpower1.aud",
        to: "expand/jpower1.aud",
    },
    FileMapping {
        from: "Data/CNCDATA/RED_ALERT/AFTERMATH/jshock1.aud",
        to: "expand/jshock1.aud",
    },
    FileMapping {
        from: "Data/CNCDATA/RED_ALERT/AFTERMATH/jyes1.aud",
        to: "expand/jyes1.aud",
    },
    FileMapping {
        from: "Data/CNCDATA/RED_ALERT/AFTERMATH/madchrg2.aud",
        to: "expand/madchrg2.aud",
    },
    FileMapping {
        from: "Data/CNCDATA/RED_ALERT/AFTERMATH/madexplo.aud",
        to: "expand/madexplo.aud",
    },
    FileMapping {
        from: "Data/CNCDATA/RED_ALERT/AFTERMATH/mboss1.aud",
        to: "expand/mboss1.aud",
    },
    FileMapping {
        from: "Data/CNCDATA/RED_ALERT/AFTERMATH/mhear1.aud",
        to: "expand/mhear1.aud",
    },
    FileMapping {
        from: "Data/CNCDATA/RED_ALERT/AFTERMATH/mhotdig1.aud",
        to: "expand/mhotdig1.aud",
    },
    FileMapping {
        from: "Data/CNCDATA/RED_ALERT/AFTERMATH/mhowdy1.aud",
        to: "expand/mhowdy1.aud",
    },
    FileMapping {
        from: "Data/CNCDATA/RED_ALERT/AFTERMATH/mhuh1.aud",
        to: "expand/mhuh1.aud",
    },
    FileMapping {
        from: "Data/CNCDATA/RED_ALERT/AFTERMATH/mlaff1.aud",
        to: "expand/mlaff1.aud",
    },
    FileMapping {
        from: "Data/CNCDATA/RED_ALERT/AFTERMATH/mrise1.aud",
        to: "expand/mrise1.aud",
    },
    FileMapping {
        from: "Data/CNCDATA/RED_ALERT/AFTERMATH/mwrench1.aud",
        to: "expand/mwrench1.aud",
    },
    FileMapping {
        from: "Data/CNCDATA/RED_ALERT/AFTERMATH/myeehaw1.aud",
        to: "expand/myeehaw1.aud",
    },
    FileMapping {
        from: "Data/CNCDATA/RED_ALERT/AFTERMATH/myes1.aud",
        to: "expand/myes1.aud",
    },
];

static REMASTERED_CS_MUSIC_COPY: [FileMapping; 8] = [
    FileMapping {
        from: "Data/CNCDATA/RED_ALERT/COUNTERSTRIKE/2nd_hand.aud",
        to: "expand/2nd_hand.aud",
    },
    FileMapping {
        from: "Data/CNCDATA/RED_ALERT/COUNTERSTRIKE/araziod.aud",
        to: "expand/araziod.aud",
    },
    FileMapping {
        from: "Data/CNCDATA/RED_ALERT/COUNTERSTRIKE/backstab.aud",
        to: "expand/backstab.aud",
    },
    FileMapping {
        from: "Data/CNCDATA/RED_ALERT/COUNTERSTRIKE/chaos2.aud",
        to: "expand/chaos2.aud",
    },
    FileMapping {
        from: "Data/CNCDATA/RED_ALERT/COUNTERSTRIKE/shut_it.aud",
        to: "expand/shut_it.aud",
    },
    FileMapping {
        from: "Data/CNCDATA/RED_ALERT/COUNTERSTRIKE/twinmix1.aud",
        to: "expand/twinmix1.aud",
    },
    FileMapping {
        from: "Data/CNCDATA/RED_ALERT/COUNTERSTRIKE/under3.aud",
        to: "expand/under3.aud",
    },
    FileMapping {
        from: "Data/CNCDATA/RED_ALERT/COUNTERSTRIKE/vr2.aud",
        to: "expand/vr2.aud",
    },
];

static REMASTERED_AM_MUSIC_COPY: [FileMapping; 9] = [
    FileMapping {
        from: "Data/CNCDATA/RED_ALERT/AFTERMATH/await.aud",
        to: "expand/await.aud",
    },
    FileMapping {
        from: "Data/CNCDATA/RED_ALERT/AFTERMATH/bog.aud",
        to: "expand/bog.aud",
    },
    FileMapping {
        from: "Data/CNCDATA/RED_ALERT/AFTERMATH/float_v2.aud",
        to: "expand/float_v2.aud",
    },
    FileMapping {
        from: "Data/CNCDATA/RED_ALERT/AFTERMATH/gloom.aud",
        to: "expand/gloom.aud",
    },
    FileMapping {
        from: "Data/CNCDATA/RED_ALERT/AFTERMATH/grndwire.aud",
        to: "expand/grndwire.aud",
    },
    FileMapping {
        from: "Data/CNCDATA/RED_ALERT/AFTERMATH/rpt.aud",
        to: "expand/rpt.aud",
    },
    FileMapping {
        from: "Data/CNCDATA/RED_ALERT/AFTERMATH/search.aud",
        to: "expand/search.aud",
    },
    FileMapping {
        from: "Data/CNCDATA/RED_ALERT/AFTERMATH/traction.aud",
        to: "expand/traction.aud",
    },
    FileMapping {
        from: "Data/CNCDATA/RED_ALERT/AFTERMATH/wastelnd.aud",
        to: "expand/wastelnd.aud",
    },
];

// ── Movie extraction from MAIN.MIX (nested MIX) ──────────────────────

/// Allied movies: extract movies1.mix from MAIN.MIX → extract VQAs → cleanup.
static MOVIES_ALLIED_FROM_MAIN_MIX: [InstallAction; 3] = [
    InstallAction::ExtractMix {
        source_mix: "MAIN.MIX",
        entries: &[FileMapping {
            from: "movies1.mix",
            to: ".tmp/movies1.mix",
        }],
    },
    InstallAction::ExtractMixFromContent {
        content_mix: ".tmp/movies1.mix",
        entries: &MOVIES_ALLIED_VQA_ENTRIES,
    },
    InstallAction::Delete {
        path: ".tmp/movies1.mix",
    },
];

/// Soviet movies: extract movies2.mix from MAIN.MIX → extract VQAs → cleanup.
static MOVIES_SOVIET_FROM_MAIN_MIX: [InstallAction; 3] = [
    InstallAction::ExtractMix {
        source_mix: "MAIN.MIX",
        entries: &[FileMapping {
            from: "movies2.mix",
            to: ".tmp/movies2.mix",
        }],
    },
    InstallAction::ExtractMixFromContent {
        content_mix: ".tmp/movies2.mix",
        entries: &MOVIES_SOVIET_VQA_ENTRIES,
    },
    InstallAction::Delete {
        path: ".tmp/movies2.mix",
    },
];

static MOVIES_ALLIED_VQA_ENTRIES: [FileMapping; 51] = [
    FileMapping {
        from: "aagun.vqa",
        to: "movies/aagun.vqa",
    },
    FileMapping {
        from: "aftrmath.vqa",
        to: "movies/aftrmath.vqa",
    },
    FileMapping {
        from: "ally1.vqa",
        to: "movies/ally1.vqa",
    },
    FileMapping {
        from: "ally10.vqa",
        to: "movies/ally10.vqa",
    },
    FileMapping {
        from: "ally10b.vqa",
        to: "movies/ally10b.vqa",
    },
    FileMapping {
        from: "ally11.vqa",
        to: "movies/ally11.vqa",
    },
    FileMapping {
        from: "ally12.vqa",
        to: "movies/ally12.vqa",
    },
    FileMapping {
        from: "ally14.vqa",
        to: "movies/ally14.vqa",
    },
    FileMapping {
        from: "ally2.vqa",
        to: "movies/ally2.vqa",
    },
    FileMapping {
        from: "ally4.vqa",
        to: "movies/ally4.vqa",
    },
    FileMapping {
        from: "ally5.vqa",
        to: "movies/ally5.vqa",
    },
    FileMapping {
        from: "ally6.vqa",
        to: "movies/ally6.vqa",
    },
    FileMapping {
        from: "ally8.vqa",
        to: "movies/ally8.vqa",
    },
    FileMapping {
        from: "ally9.vqa",
        to: "movies/ally9.vqa",
    },
    FileMapping {
        from: "allyend.vqa",
        to: "movies/allyend.vqa",
    },
    FileMapping {
        from: "allymorf.vqa",
        to: "movies/allymorf.vqa",
    },
    FileMapping {
        from: "apcescpe.vqa",
        to: "movies/apcescpe.vqa",
    },
    FileMapping {
        from: "assess.vqa",
        to: "movies/assess.vqa",
    },
    FileMapping {
        from: "battle.vqa",
        to: "movies/battle.vqa",
    },
    FileMapping {
        from: "binoc.vqa",
        to: "movies/binoc.vqa",
    },
    FileMapping {
        from: "bmap.vqa",
        to: "movies/bmap.vqa",
    },
    FileMapping {
        from: "brdgtilt.vqa",
        to: "movies/brdgtilt.vqa",
    },
    FileMapping {
        from: "crontest.vqa",
        to: "movies/crontest.vqa",
    },
    FileMapping {
        from: "cronfail.vqa",
        to: "movies/cronfail.vqa",
    },
    FileMapping {
        from: "destroyr.vqa",
        to: "movies/destroyr.vqa",
    },
    FileMapping {
        from: "dud.vqa",
        to: "movies/dud.vqa",
    },
    FileMapping {
        from: "elevator.vqa",
        to: "movies/elevator.vqa",
    },
    FileMapping {
        from: "flare.vqa",
        to: "movies/flare.vqa",
    },
    FileMapping {
        from: "frozen.vqa",
        to: "movies/frozen.vqa",
    },
    FileMapping {
        from: "grvestne.vqa",
        to: "movies/grvestne.vqa",
    },
    FileMapping {
        from: "landing.vqa",
        to: "movies/landing.vqa",
    },
    FileMapping {
        from: "masasslt.vqa",
        to: "movies/masasslt.vqa",
    },
    FileMapping {
        from: "mcv.vqa",
        to: "movies/mcv.vqa",
    },
    FileMapping {
        from: "mcv_land.vqa",
        to: "movies/mcv_land.vqa",
    },
    FileMapping {
        from: "montpass.vqa",
        to: "movies/montpass.vqa",
    },
    FileMapping {
        from: "oildrum.vqa",
        to: "movies/oildrum.vqa",
    },
    FileMapping {
        from: "overrun.vqa",
        to: "movies/overrun.vqa",
    },
    FileMapping {
        from: "prolog.vqa",
        to: "movies/prolog.vqa",
    },
    FileMapping {
        from: "redintro.vqa",
        to: "movies/redintro.vqa",
    },
    FileMapping {
        from: "shipsink.vqa",
        to: "movies/shipsink.vqa",
    },
    FileMapping {
        from: "shorbom1.vqa",
        to: "movies/shorbom1.vqa",
    },
    FileMapping {
        from: "shorbom2.vqa",
        to: "movies/shorbom2.vqa",
    },
    FileMapping {
        from: "shorbomb.vqa",
        to: "movies/shorbomb.vqa",
    },
    FileMapping {
        from: "snowbomb.vqa",
        to: "movies/snowbomb.vqa",
    },
    FileMapping {
        from: "soviet1.vqa",
        to: "movies/soviet1.vqa",
    },
    FileMapping {
        from: "sovtstar.vqa",
        to: "movies/sovtstar.vqa",
    },
    FileMapping {
        from: "spy.vqa",
        to: "movies/spy.vqa",
    },
    FileMapping {
        from: "tanya1.vqa",
        to: "movies/tanya1.vqa",
    },
    FileMapping {
        from: "tanya2.vqa",
        to: "movies/tanya2.vqa",
    },
    FileMapping {
        from: "toofar.vqa",
        to: "movies/toofar.vqa",
    },
    FileMapping {
        from: "trinity.vqa",
        to: "movies/trinity.vqa",
    },
];

static MOVIES_SOVIET_VQA_ENTRIES: [FileMapping; 55] = [
    FileMapping {
        from: "aagun.vqa",
        to: "movies/aagun.vqa",
    },
    FileMapping {
        from: "airfield.vqa",
        to: "movies/airfield.vqa",
    },
    FileMapping {
        from: "ally1.vqa",
        to: "movies/ally1.vqa",
    },
    FileMapping {
        from: "allymorf.vqa",
        to: "movies/allymorf.vqa",
    },
    FileMapping {
        from: "averted.vqa",
        to: "movies/averted.vqa",
    },
    FileMapping {
        from: "beachead.vqa",
        to: "movies/beachead.vqa",
    },
    FileMapping {
        from: "bmap.vqa",
        to: "movies/bmap.vqa",
    },
    FileMapping {
        from: "bombrun.vqa",
        to: "movies/bombrun.vqa",
    },
    FileMapping {
        from: "countdwn.vqa",
        to: "movies/countdwn.vqa",
    },
    FileMapping {
        from: "cronfail.vqa",
        to: "movies/cronfail.vqa",
    },
    FileMapping {
        from: "double.vqa",
        to: "movies/double.vqa",
    },
    FileMapping {
        from: "dpthchrg.vqa",
        to: "movies/dpthchrg.vqa",
    },
    FileMapping {
        from: "execute.vqa",
        to: "movies/execute.vqa",
    },
    FileMapping {
        from: "flare.vqa",
        to: "movies/flare.vqa",
    },
    FileMapping {
        from: "landing.vqa",
        to: "movies/landing.vqa",
    },
    FileMapping {
        from: "mcvbrdge.vqa",
        to: "movies/mcvbrdge.vqa",
    },
    FileMapping {
        from: "mig.vqa",
        to: "movies/mig.vqa",
    },
    FileMapping {
        from: "movingin.vqa",
        to: "movies/movingin.vqa",
    },
    FileMapping {
        from: "mtnkfact.vqa",
        to: "movies/mtnkfact.vqa",
    },
    FileMapping {
        from: "nukestok.vqa",
        to: "movies/nukestok.vqa",
    },
    FileMapping {
        from: "onthprwl.vqa",
        to: "movies/onthprwl.vqa",
    },
    FileMapping {
        from: "periscop.vqa",
        to: "movies/periscop.vqa",
    },
    FileMapping {
        from: "prolog.vqa",
        to: "movies/prolog.vqa",
    },
    FileMapping {
        from: "radrraid.vqa",
        to: "movies/radrraid.vqa",
    },
    FileMapping {
        from: "redintro.vqa",
        to: "movies/redintro.vqa",
    },
    FileMapping {
        from: "search.vqa",
        to: "movies/search.vqa",
    },
    FileMapping {
        from: "sfrozen.vqa",
        to: "movies/sfrozen.vqa",
    },
    FileMapping {
        from: "sitduck.vqa",
        to: "movies/sitduck.vqa",
    },
    FileMapping {
        from: "slntsrvc.vqa",
        to: "movies/slntsrvc.vqa",
    },
    FileMapping {
        from: "snowbomb.vqa",
        to: "movies/snowbomb.vqa",
    },
    FileMapping {
        from: "snstrafe.vqa",
        to: "movies/snstrafe.vqa",
    },
    FileMapping {
        from: "sovbatl.vqa",
        to: "movies/sovbatl.vqa",
    },
    FileMapping {
        from: "sovcemet.vqa",
        to: "movies/sovcemet.vqa",
    },
    FileMapping {
        from: "sovfinal.vqa",
        to: "movies/sovfinal.vqa",
    },
    FileMapping {
        from: "soviet1.vqa",
        to: "movies/soviet1.vqa",
    },
    FileMapping {
        from: "soviet2.vqa",
        to: "movies/soviet2.vqa",
    },
    FileMapping {
        from: "soviet3.vqa",
        to: "movies/soviet3.vqa",
    },
    FileMapping {
        from: "soviet4.vqa",
        to: "movies/soviet4.vqa",
    },
    FileMapping {
        from: "soviet5.vqa",
        to: "movies/soviet5.vqa",
    },
    FileMapping {
        from: "soviet6.vqa",
        to: "movies/soviet6.vqa",
    },
    FileMapping {
        from: "soviet7.vqa",
        to: "movies/soviet7.vqa",
    },
    FileMapping {
        from: "soviet8.vqa",
        to: "movies/soviet8.vqa",
    },
    FileMapping {
        from: "soviet9.vqa",
        to: "movies/soviet9.vqa",
    },
    FileMapping {
        from: "soviet10.vqa",
        to: "movies/soviet10.vqa",
    },
    FileMapping {
        from: "soviet11.vqa",
        to: "movies/soviet11.vqa",
    },
    FileMapping {
        from: "soviet12.vqa",
        to: "movies/soviet12.vqa",
    },
    FileMapping {
        from: "soviet13.vqa",
        to: "movies/soviet13.vqa",
    },
    FileMapping {
        from: "soviet14.vqa",
        to: "movies/soviet14.vqa",
    },
    FileMapping {
        from: "sovmcv.vqa",
        to: "movies/sovmcv.vqa",
    },
    FileMapping {
        from: "sovtstar.vqa",
        to: "movies/sovtstar.vqa",
    },
    FileMapping {
        from: "spotter.vqa",
        to: "movies/spotter.vqa",
    },
    FileMapping {
        from: "strafe.vqa",
        to: "movies/strafe.vqa",
    },
    FileMapping {
        from: "take_off.vqa",
        to: "movies/take_off.vqa",
    },
    FileMapping {
        from: "tesla.vqa",
        to: "movies/tesla.vqa",
    },
    FileMapping {
        from: "v2rocket.vqa",
        to: "movies/v2rocket.vqa",
    },
];

// ── Aftermath disc extraction ─────────────────────────────────────────
// The Aftermath disc has a PATCH.RTP that contains expansion files at
// raw byte offsets, plus loose files.

static AFTERMATH_DISC_ACTIONS: [InstallAction; 1] = [InstallAction::Copy {
    files: &AFTERMATH_EXPAND_COPY,
}];

static AFTERMATH_DISC_MUSIC_ACTIONS: [InstallAction; 1] = [InstallAction::Copy {
    files: &AM_MUSIC_COPY,
}];

static CS_DISC_MUSIC_ACTIONS: [InstallAction; 1] = [InstallAction::Copy {
    files: &CS_MUSIC_COPY,
}];
