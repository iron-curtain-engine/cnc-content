// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! Static file-mapping arrays for Tiberian Sun and Firestorm content.
//!
//! Tiberian Sun uses a flat directory layout — all MIX archives sit in
//! the install root for both disc and digital editions. The Steam/Origin
//! TUC and retail disc share the same flat structure.

use crate::actions::FileMapping;

// ── Base game files ─────────────────────────────────────────────────

/// TS base game MIX files — direct copy from a flat source directory.
///
/// All six required MIX archives are uppercase on source (DOS convention)
/// and lowercase in the managed content root. Used for disc, Steam TUC,
/// and Origin TUC editions.
pub(super) static TS_BASE_COPY: [FileMapping; 6] = [
    FileMapping {
        from: "TIBSUN.MIX",
        to: "tibsun.mix",
    },
    FileMapping {
        from: "CACHE.MIX",
        to: "cache.mix",
    },
    FileMapping {
        from: "CONQUER.MIX",
        to: "conquer.mix",
    },
    FileMapping {
        from: "LOCAL.MIX",
        to: "local.mix",
    },
    FileMapping {
        from: "ISOSNOW.MIX",
        to: "isosnow.mix",
    },
    FileMapping {
        from: "ISOTEMP.MIX",
        to: "isotemp.mix",
    },
];

// ── Expansion content ───────────────────────────────────────────────

/// Firestorm expansion MIX files — direct copy.
///
/// The Firestorm disc and Steam/Origin TUC editions both have these
/// expansion archives in the install root.
pub(super) static TS_FIRESTORM_COPY: [FileMapping; 2] = [
    FileMapping {
        from: "E01SC01.MIX",
        to: "e01sc01.mix",
    },
    FileMapping {
        from: "E01SC02.MIX",
        to: "e01sc02.mix",
    },
];

// ── Music ───────────────────────────────────────────────────────────

/// TS music — direct copy of scores.mix.
pub(super) static TS_MUSIC_COPY: [FileMapping; 1] = [FileMapping {
    from: "SCORES.MIX",
    to: "scores.mix",
}];

// ── Movies ──────────────────────────────────────────────────────────

/// TS movies — direct copy of the consolidated movies archive.
///
/// Tiberian Sun stores all FMV cutscenes in a single MIX archive
/// rather than as loose VQA files like the earlier games.
pub(super) static TS_MOVIES_COPY: [FileMapping; 1] = [FileMapping {
    from: "MOVIES01.MIX",
    to: "movies01.mix",
}];
