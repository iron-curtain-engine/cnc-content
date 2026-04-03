// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025-present Iron Curtain contributors

//! Static file-mapping arrays for Red Alert Counterstrike, Aftermath, and Remastered sources.
//!
//! Split from recipes/mod.rs — contains the music and base file mappings
//! for the Counterstrike and Aftermath expansions, and all mappings for the
//! C&C Remastered Collection editions of Red Alert.

use crate::actions::FileMapping;

pub(super) static CS_MUSIC_COPY: [FileMapping; 8] = [
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
pub(super) static AM_MUSIC_COPY: [FileMapping; 9] = [
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

pub(super) static REMASTERED_BASE_COPY: [FileMapping; 11] = [
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

pub(super) static REMASTERED_AFTERMATH_COPY: [FileMapping; 27] = [
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

pub(super) static REMASTERED_CS_MUSIC_COPY: [FileMapping; 8] = [
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

pub(super) static REMASTERED_AM_MUSIC_COPY: [FileMapping; 9] = [
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
