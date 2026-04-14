// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025-present Iron Curtain contributors

//! Static file-mapping arrays for Red Alert 2 and C&C Generals content.

use crate::actions::FileMapping;

/// RA2 base game files — direct copy from the TUC install root.
///
/// The TUC places all MIX archives flat in the install directory. File
/// names on disk use mixed case but are accessed case-insensitively
/// on Windows; we use the on-disk case for `from` and the expected
/// content-root case for `to`.
pub(super) static RA2_BASE_COPY: [FileMapping; 3] = [
    FileMapping {
        from: "ra2.mix",
        to: "ra2.mix",
    },
    FileMapping {
        from: "language.mix",
        to: "language.mix",
    },
    FileMapping {
        from: "MULTI.MIX",
        to: "multi.mix",
    },
];

/// Yuri's Revenge expansion files — direct copy.
pub(super) static RA2_YR_COPY: [FileMapping; 2] = [
    FileMapping {
        from: "ra2md.mix",
        to: "ra2md.mix",
    },
    FileMapping {
        from: "langmd.mix",
        to: "langmd.mix",
    },
];

/// RA2 music — direct copy.
///
/// The Steam TUC stores this as `THEME.MIX` (uppercase); the content root
/// expects lowercase `theme.mix`.
pub(super) static RA2_MUSIC_COPY: [FileMapping; 1] = [FileMapping {
    from: "THEME.MIX",
    to: "theme.mix",
}];

/// RA2 movies — direct copy (Steam/Origin TUC, lowercase filenames).
pub(super) static RA2_MOVIES_COPY: [FileMapping; 1] = [FileMapping {
    from: "ra2.vqa",
    to: "ra2.vqa",
}];

/// RA2 base game files — disc and TFD layout (uppercase filenames).
///
/// The retail disc and The First Decade DVD store MIX archives with
/// uppercase DOS names, unlike the lowercase Steam/Origin TUC builds.
pub(super) static RA2_DISC_BASE_COPY: [FileMapping; 3] = [
    FileMapping {
        from: "RA2.MIX",
        to: "ra2.mix",
    },
    FileMapping {
        from: "LANGUAGE.MIX",
        to: "language.mix",
    },
    FileMapping {
        from: "MULTI.MIX",
        to: "multi.mix",
    },
];

/// Yuri's Revenge — disc / TFD layout (uppercase filenames).
pub(super) static RA2_DISC_YR_COPY: [FileMapping; 2] = [
    FileMapping {
        from: "RA2MD.MIX",
        to: "ra2md.mix",
    },
    FileMapping {
        from: "LANGMD.MIX",
        to: "langmd.mix",
    },
];

/// RA2 movies — disc / TFD layout (uppercase filenames).
pub(super) static RA2_DISC_MOVIES_COPY: [FileMapping; 1] = [FileMapping {
    from: "RA2.VQA",
    to: "ra2.vqa",
}];

// ══════════════════════════════════════════════════════════════════════
//  C&C Generals file mappings — Steam / Origin TUC layout
// ══════════════════════════════════════════════════════════════════════

/// Generals base game BIG archives — direct copy from the TUC install root.
///
/// The Steam TUC merges base Generals and Zero Hour into a single install
/// directory. All BIG archives are in the root alongside loose files in
/// Data/. We copy only the three archives listed as test_files for GenBase.
pub(super) static GEN_BASE_COPY: [FileMapping; 3] = [
    FileMapping {
        from: "INI.big",
        to: "INI.big",
    },
    FileMapping {
        from: "Terrain.big",
        to: "Terrain.big",
    },
    FileMapping {
        from: "W3D.big",
        to: "W3D.big",
    },
];

/// Zero Hour BIG archives — disc only.
///
/// The retail Zero Hour disc has standalone expansion archives. The
/// Steam/Origin TUC merges these into the base archives, so this
/// mapping is only used for the `GenZhDisc` source.
pub(super) static GEN_ZH_COPY: [FileMapping; 2] = [
    FileMapping {
        from: "INIZH.big",
        to: "INIZH.big",
    },
    FileMapping {
        from: "W3DZH.big",
        to: "W3DZH.big",
    },
];
