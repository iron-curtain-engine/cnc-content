// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025-present Iron Curtain contributors

//! Static file-mapping arrays for Red Alert movie (VQA cutscene) extraction.
//!
//! Split from recipes/mod.rs — contains the large Allied and Soviet VQA
//! file lists (51 and 55 entries respectively), the movie extraction action
//! sequences, and the disc-based expansion actions.
//!
//! These arrays are large because RA ships dozens of cutscene files split
//! across Allied and Soviet disc editions.

use super::ra_base::AFTERMATH_EXPAND_COPY;
use super::ra_remastered::{AM_MUSIC_COPY, CS_MUSIC_COPY};
use crate::actions::{FileMapping, InstallAction};

/// Allied movies: extract movies1.mix from MAIN.MIX → extract VQAs → cleanup.
pub(super) static MOVIES_ALLIED_FROM_MAIN_MIX: [InstallAction; 3] = [
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
pub(super) static MOVIES_SOVIET_FROM_MAIN_MIX: [InstallAction; 3] = [
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

pub(super) static MOVIES_ALLIED_VQA_ENTRIES: [FileMapping; 51] = [
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

pub(super) static MOVIES_SOVIET_VQA_ENTRIES: [FileMapping; 55] = [
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

pub(super) static AFTERMATH_DISC_ACTIONS: [InstallAction; 1] = [InstallAction::Copy {
    files: &AFTERMATH_EXPAND_COPY,
}];

pub(super) static AFTERMATH_DISC_MUSIC_ACTIONS: [InstallAction; 1] = [InstallAction::Copy {
    files: &AM_MUSIC_COPY,
}];

pub(super) static CS_DISC_MUSIC_ACTIONS: [InstallAction; 1] = [InstallAction::Copy {
    files: &CS_MUSIC_COPY,
}];
