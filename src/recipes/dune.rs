// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! Static file-mapping arrays for Dune 2 and Dune 2000 content.
//!
//! Both games are NOT freeware — extraction from user-owned local sources
//! only, no downloads. Dune 2 uses PAK archives; Dune 2000 uses a mix of
//! executables and data files. All source files use uppercase DOS-era
//! filenames. Target filenames preserve the original case since downstream
//! consumers (DOSBox, OpenRA Dune mod) expect uppercase.

use crate::actions::FileMapping;

// ── Dune 2 ──────────────────────────────────────────────────────────

/// Dune II game files — direct copy from disc or GOG install.
///
/// The complete set of files needed to run Dune II. Includes the main
/// executable, scenario data, sound, and house-specific archives.
/// File names are preserved in uppercase to match DOSBox expectations.
pub(super) static DUNE2_BASE_COPY: [FileMapping; 7] = [
    FileMapping {
        from: "DUNE2.EXE",
        to: "DUNE2.EXE",
    },
    FileMapping {
        from: "SCENARIO.PAK",
        to: "SCENARIO.PAK",
    },
    FileMapping {
        from: "SOUND.PAK",
        to: "SOUND.PAK",
    },
    FileMapping {
        from: "DUNE.PAK",
        to: "DUNE.PAK",
    },
    FileMapping {
        from: "ATRE.PAK",
        to: "ATRE.PAK",
    },
    FileMapping {
        from: "HARK.PAK",
        to: "HARK.PAK",
    },
    FileMapping {
        from: "ORDOS.PAK",
        to: "ORDOS.PAK",
    },
];

// ── Dune 2000 ───────────────────────────────────────────────────────

/// Dune 2000 game files — direct copy from disc or GOG install.
///
/// Includes the main executable, setup data, font, icon resources,
/// and mouse cursor sprites. File names preserved in uppercase.
pub(super) static DUNE2000_BASE_COPY: [FileMapping; 5] = [
    FileMapping {
        from: "DUNE2000.EXE",
        to: "DUNE2000.EXE",
    },
    FileMapping {
        from: "SETUP.Z",
        to: "SETUP.Z",
    },
    FileMapping {
        from: "LEPTON.FNT",
        to: "LEPTON.FNT",
    },
    FileMapping {
        from: "ICON.ICN",
        to: "ICON.ICN",
    },
    FileMapping {
        from: "MOUSE.SHP",
        to: "MOUSE.SHP",
    },
];
