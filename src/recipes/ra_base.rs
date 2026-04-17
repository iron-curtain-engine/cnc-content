// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025-present Iron Curtain contributors

//! Static file-mapping arrays for Red Alert base and Aftermath content.
//!
//! Split from recipes/mod.rs — these arrays define the individual file
//! mappings used in the ALL_RECIPES installation sequence for the base RA
//! game files, the Aftermath expansion, and the Counterstrike expansion music.

use crate::actions::FileMapping;

pub(crate) static BASE_DIRECT_COPY: [FileMapping; 11] = [
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
pub(crate) static BASE_FROM_REDALERT_MIX: [FileMapping; 11] = [
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
pub(crate) static AFTERMATH_EXPAND_COPY: [FileMapping; 27] = [
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
