// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! Static file-mapping arrays for Tiberian Dawn content.
//!
//! Tiberian Dawn uses a flat directory layout for base game MIX files.
//! The Steam/Origin C&C editions and original disc releases all place
//! files directly in the root directory with uppercase DOS-era filenames.
//! The Remastered Collection nests them under
//! `Data/CNCDATA/TIBERIAN_DAWN/CD1/`.

use crate::actions::FileMapping;

// ── Base game files ─────────────────────────────────────────────────

/// TD base game MIX files — direct copy from a flat source directory.
///
/// Used for disc, Steam C&C (AppId 2229830), and Origin C&C editions.
/// All files are uppercase on disk (DOS convention preserved by digital
/// storefronts). Target names are lowercase for the managed content root.
pub(super) static TD_BASE_COPY: [FileMapping; 9] = [
    FileMapping {
        from: "CONQUER.MIX",
        to: "conquer.mix",
    },
    FileMapping {
        from: "DESERT.MIX",
        to: "desert.mix",
    },
    FileMapping {
        from: "GENERAL.MIX",
        to: "general.mix",
    },
    FileMapping {
        from: "SCORES.MIX",
        to: "scores.mix",
    },
    FileMapping {
        from: "SOUNDS.MIX",
        to: "sounds.mix",
    },
    FileMapping {
        from: "SPEECH.MIX",
        to: "speech.mix",
    },
    FileMapping {
        from: "TEMPERAT.MIX",
        to: "temperat.mix",
    },
    FileMapping {
        from: "TRANSIT.MIX",
        to: "transit.mix",
    },
    FileMapping {
        from: "WINTER.MIX",
        to: "winter.mix",
    },
];

/// TD base from Remastered — same nine MIX files but nested under the
/// Remastered Collection's deep directory structure.
pub(super) static TD_REMASTERED_BASE_COPY: [FileMapping; 9] = [
    FileMapping {
        from: "Data/CNCDATA/TIBERIAN_DAWN/CD1/CONQUER.MIX",
        to: "conquer.mix",
    },
    FileMapping {
        from: "Data/CNCDATA/TIBERIAN_DAWN/CD1/DESERT.MIX",
        to: "desert.mix",
    },
    FileMapping {
        from: "Data/CNCDATA/TIBERIAN_DAWN/CD1/GENERAL.MIX",
        to: "general.mix",
    },
    FileMapping {
        from: "Data/CNCDATA/TIBERIAN_DAWN/CD1/SCORES.MIX",
        to: "scores.mix",
    },
    FileMapping {
        from: "Data/CNCDATA/TIBERIAN_DAWN/CD1/SOUNDS.MIX",
        to: "sounds.mix",
    },
    FileMapping {
        from: "Data/CNCDATA/TIBERIAN_DAWN/CD1/SPEECH.MIX",
        to: "speech.mix",
    },
    FileMapping {
        from: "Data/CNCDATA/TIBERIAN_DAWN/CD1/TEMPERAT.MIX",
        to: "temperat.mix",
    },
    FileMapping {
        from: "Data/CNCDATA/TIBERIAN_DAWN/CD1/TRANSIT.MIX",
        to: "transit.mix",
    },
    FileMapping {
        from: "Data/CNCDATA/TIBERIAN_DAWN/CD1/WINTER.MIX",
        to: "winter.mix",
    },
];

// ── Expansion content ───────────────────────────────────────────────

/// Covert Operations expansion MIX files — direct copy.
///
/// Present on the Covert Ops disc and in Steam/Origin C&C installs.
pub(super) static TD_COVERT_OPS_COPY: [FileMapping; 2] = [
    FileMapping {
        from: "SC-000.MIX",
        to: "sc-000.mix",
    },
    FileMapping {
        from: "SC-001.MIX",
        to: "sc-001.mix",
    },
];

// ── Music ───────────────────────────────────────────────────────────

/// TD music — direct copy of scores.mix.
///
/// The music archive is also part of TdBase, but this mapping allows
/// extracting music independently from a source that only provides it.
pub(super) static TD_MUSIC_COPY: [FileMapping; 1] = [FileMapping {
    from: "SCORES.MIX",
    to: "scores.mix",
}];

// ── Movies ──────────────────────────────────────────────────────────
//
// On the original C&C95 discs, VQA movies are loose files at the disc
// root with uppercase DOS names. On Steam/Origin C&C editions, they
// are in a MOVIES/ subdirectory. Both map to movies/ in the content root.

/// GDI campaign movies — from disc root (uppercase, no subdirectory).
pub(super) static TD_MOVIES_GDI_DISC_COPY: [FileMapping; 5] = [
    FileMapping {
        from: "GDI1.VQA",
        to: "movies/gdi1.vqa",
    },
    FileMapping {
        from: "GDI2.VQA",
        to: "movies/gdi2.vqa",
    },
    FileMapping {
        from: "GDI3.VQA",
        to: "movies/gdi3.vqa",
    },
    FileMapping {
        from: "GDI15.VQA",
        to: "movies/gdi15.vqa",
    },
    FileMapping {
        from: "LOGO.VQA",
        to: "movies/logo.vqa",
    },
];

/// Nod campaign movies — from disc root (uppercase, no subdirectory).
pub(super) static TD_MOVIES_NOD_DISC_COPY: [FileMapping; 5] = [
    FileMapping {
        from: "NOD1.VQA",
        to: "movies/nod1.vqa",
    },
    FileMapping {
        from: "NOD2.VQA",
        to: "movies/nod2.vqa",
    },
    FileMapping {
        from: "NOD3.VQA",
        to: "movies/nod3.vqa",
    },
    FileMapping {
        from: "NOD10.VQA",
        to: "movies/nod10.vqa",
    },
    FileMapping {
        from: "LOGO.VQA",
        to: "movies/logo.vqa",
    },
];

/// GDI campaign movies — from Steam/Origin MOVIES/ subdirectory.
pub(super) static TD_MOVIES_GDI_STEAM_COPY: [FileMapping; 5] = [
    FileMapping {
        from: "MOVIES/GDI1.VQA",
        to: "movies/gdi1.vqa",
    },
    FileMapping {
        from: "MOVIES/GDI2.VQA",
        to: "movies/gdi2.vqa",
    },
    FileMapping {
        from: "MOVIES/GDI3.VQA",
        to: "movies/gdi3.vqa",
    },
    FileMapping {
        from: "MOVIES/GDI15.VQA",
        to: "movies/gdi15.vqa",
    },
    FileMapping {
        from: "MOVIES/LOGO.VQA",
        to: "movies/logo.vqa",
    },
];

/// Nod campaign movies — from Steam/Origin MOVIES/ subdirectory.
pub(super) static TD_MOVIES_NOD_STEAM_COPY: [FileMapping; 5] = [
    FileMapping {
        from: "MOVIES/NOD1.VQA",
        to: "movies/nod1.vqa",
    },
    FileMapping {
        from: "MOVIES/NOD2.VQA",
        to: "movies/nod2.vqa",
    },
    FileMapping {
        from: "MOVIES/NOD3.VQA",
        to: "movies/nod3.vqa",
    },
    FileMapping {
        from: "MOVIES/NOD10.VQA",
        to: "movies/nod10.vqa",
    },
    FileMapping {
        from: "MOVIES/LOGO.VQA",
        to: "movies/logo.vqa",
    },
];
