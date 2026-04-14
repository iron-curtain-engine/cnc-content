// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! Content package definitions — what each game needs to run.
//!
//! Packages are grouped by game. Test file lists are immutable properties of
//! games released in the 1990s — they belong inline as Rust slices, not in
//! external files.

use crate::{ContentPackage, DownloadId, GameId, PackageId, SourceId};

/// All content packages across all supported games.
pub static ALL_PACKAGES: &[ContentPackage] = &[
    // ══════════════════════════════════════════════════════════════════════
    // Red Alert — freeware since 31 August 2008 (EA release)
    // ══════════════════════════════════════════════════════════════════════

    // ── Required packages ─────────────────────────────────────────────
    ContentPackage {
        id: PackageId::RaBase,
        game: GameId::RedAlert,
        title: "Base Game Content",
        required: true,
        test_files: &[
            "allies.mix",
            "conquer.mix",
            "interior.mix",
            "hires.mix",
            "lores.mix",
            "local.mix",
            "speech.mix",
            "russian.mix",
            "snow.mix",
            "sounds.mix",
            "temperat.mix",
        ],
        sources: &[
            SourceId::AlliedDisc,
            SourceId::SovietDisc,
            SourceId::TheFirstDecade,
            SourceId::SteamTuc,
            SourceId::OriginTuc,
            SourceId::SteamRemastered,
            SourceId::OriginRemastered,
        ],
        download: Some(DownloadId::RaBaseFiles),
    },
    ContentPackage {
        id: PackageId::RaAftermathBase,
        game: GameId::RedAlert,
        title: "Aftermath Expansion",
        required: true,
        test_files: &[
            "expand/expand2.mix",
            "expand/hires1.mix",
            "expand/lores1.mix",
            "expand/chrotnk1.aud",
            "expand/fixit1.aud",
            "expand/jburn1.aud",
            "expand/jchrge1.aud",
            "expand/jcrisp1.aud",
            "expand/jdance1.aud",
            "expand/jjuice1.aud",
            "expand/jjump1.aud",
            "expand/jlight1.aud",
            "expand/jpower1.aud",
            "expand/jshock1.aud",
            "expand/jyes1.aud",
            "expand/madchrg2.aud",
            "expand/madexplo.aud",
            "expand/mboss1.aud",
            "expand/mhear1.aud",
            "expand/mhotdig1.aud",
            "expand/mhowdy1.aud",
            "expand/mhuh1.aud",
            "expand/mlaff1.aud",
            "expand/mrise1.aud",
            "expand/mwrench1.aud",
            "expand/myeehaw1.aud",
            "expand/myes1.aud",
        ],
        sources: &[
            SourceId::AftermathDisc,
            SourceId::TheFirstDecade,
            SourceId::SteamTuc,
            SourceId::OriginTuc,
            SourceId::SteamRemastered,
            SourceId::OriginRemastered,
        ],
        download: Some(DownloadId::RaAftermath),
    },
    ContentPackage {
        id: PackageId::RaCncDesert,
        game: GameId::RedAlert,
        title: "C&C Desert Tileset",
        required: true,
        test_files: &["cnc/desert.mix"],
        sources: &[
            SourceId::TheFirstDecade,
            SourceId::Cnc95,
            SourceId::SteamCnc,
            SourceId::OriginCnc,
            SourceId::SteamRemastered,
            SourceId::OriginRemastered,
        ],
        download: Some(DownloadId::RaCncDesert),
    },
    // ── Optional packages ─────────────────────────────────────────────
    ContentPackage {
        id: PackageId::RaMusic,
        game: GameId::RedAlert,
        title: "Red Alert Music",
        required: false,
        test_files: &["scores.mix"],
        sources: &[
            SourceId::AlliedDisc,
            SourceId::SovietDisc,
            SourceId::TheFirstDecade,
            SourceId::SteamTuc,
            SourceId::OriginTuc,
            SourceId::SteamRemastered,
            SourceId::OriginRemastered,
        ],
        download: Some(DownloadId::RaMusic),
    },
    ContentPackage {
        id: PackageId::RaMoviesAllied,
        game: GameId::RedAlert,
        title: "Allied Campaign Movies",
        required: false,
        test_files: &[
            "movies/aagun.vqa",
            "movies/aftrmath.vqa",
            "movies/ally1.vqa",
            "movies/ally10.vqa",
            "movies/ally10b.vqa",
            "movies/ally11.vqa",
            "movies/ally12.vqa",
            "movies/ally14.vqa",
            "movies/ally2.vqa",
            "movies/ally4.vqa",
            "movies/ally5.vqa",
            "movies/ally6.vqa",
            "movies/ally8.vqa",
            "movies/ally9.vqa",
            "movies/allyend.vqa",
            "movies/allymorf.vqa",
            "movies/apcescpe.vqa",
            "movies/assess.vqa",
            "movies/battle.vqa",
            "movies/binoc.vqa",
            "movies/bmap.vqa",
            "movies/brdgtilt.vqa",
            "movies/crontest.vqa",
            "movies/cronfail.vqa",
            "movies/destroyr.vqa",
            "movies/dud.vqa",
            "movies/elevator.vqa",
            "movies/flare.vqa",
            "movies/frozen.vqa",
            "movies/grvestne.vqa",
            "movies/landing.vqa",
            "movies/masasslt.vqa",
            "movies/mcv.vqa",
            "movies/mcv_land.vqa",
            "movies/montpass.vqa",
            "movies/oildrum.vqa",
            "movies/overrun.vqa",
            "movies/prolog.vqa",
            "movies/redintro.vqa",
            "movies/shipsink.vqa",
            "movies/shorbom1.vqa",
            "movies/shorbom2.vqa",
            "movies/shorbomb.vqa",
            "movies/snowbomb.vqa",
            "movies/soviet1.vqa",
            "movies/sovtstar.vqa",
            "movies/spy.vqa",
            "movies/tanya1.vqa",
            "movies/tanya2.vqa",
            "movies/toofar.vqa",
            "movies/trinity.vqa",
        ],
        sources: &[
            SourceId::AlliedDisc,
            SourceId::TheFirstDecade,
            SourceId::SteamTuc,
            SourceId::OriginTuc,
            SourceId::SteamRemastered,
            SourceId::OriginRemastered,
        ],
        download: Some(DownloadId::RaMoviesAllied),
    },
    ContentPackage {
        id: PackageId::RaMoviesSoviet,
        game: GameId::RedAlert,
        title: "Soviet Campaign Movies",
        required: false,
        test_files: &[
            "movies/aagun.vqa",
            "movies/airfield.vqa",
            "movies/ally1.vqa",
            "movies/allymorf.vqa",
            "movies/averted.vqa",
            "movies/beachead.vqa",
            "movies/bmap.vqa",
            "movies/bombrun.vqa",
            "movies/countdwn.vqa",
            "movies/cronfail.vqa",
            "movies/double.vqa",
            "movies/dpthchrg.vqa",
            "movies/execute.vqa",
            "movies/flare.vqa",
            "movies/landing.vqa",
            "movies/mcvbrdge.vqa",
            "movies/mig.vqa",
            "movies/movingin.vqa",
            "movies/mtnkfact.vqa",
            "movies/nukestok.vqa",
            "movies/onthprwl.vqa",
            "movies/periscop.vqa",
            "movies/prolog.vqa",
            "movies/radrraid.vqa",
            "movies/redintro.vqa",
            "movies/search.vqa",
            "movies/sfrozen.vqa",
            "movies/sitduck.vqa",
            "movies/slntsrvc.vqa",
            "movies/snowbomb.vqa",
            "movies/snstrafe.vqa",
            "movies/sovbatl.vqa",
            "movies/sovcemet.vqa",
            "movies/sovfinal.vqa",
            "movies/soviet1.vqa",
            "movies/soviet2.vqa",
            "movies/soviet3.vqa",
            "movies/soviet4.vqa",
            "movies/soviet5.vqa",
            "movies/soviet6.vqa",
            "movies/soviet7.vqa",
            "movies/soviet8.vqa",
            "movies/soviet9.vqa",
            "movies/soviet10.vqa",
            "movies/soviet11.vqa",
            "movies/soviet12.vqa",
            "movies/soviet13.vqa",
            "movies/soviet14.vqa",
            "movies/sovmcv.vqa",
            "movies/sovtstar.vqa",
            "movies/spotter.vqa",
            "movies/strafe.vqa",
            "movies/take_off.vqa",
            "movies/tesla.vqa",
            "movies/v2rocket.vqa",
        ],
        sources: &[
            SourceId::SovietDisc,
            SourceId::TheFirstDecade,
            SourceId::SteamTuc,
            SourceId::OriginTuc,
            SourceId::SteamRemastered,
            SourceId::OriginRemastered,
        ],
        download: Some(DownloadId::RaMoviesSoviet),
    },
    ContentPackage {
        id: PackageId::RaMusicCounterstrike,
        game: GameId::RedAlert,
        title: "Counterstrike Music",
        required: false,
        test_files: &[
            "expand/2nd_hand.aud",
            "expand/araziod.aud",
            "expand/backstab.aud",
            "expand/chaos2.aud",
            "expand/shut_it.aud",
            "expand/twinmix1.aud",
            "expand/under3.aud",
            "expand/vr2.aud",
        ],
        sources: &[
            SourceId::CounterstrikeDisc,
            SourceId::SteamTuc,
            SourceId::OriginTuc,
            SourceId::SteamRemastered,
            SourceId::OriginRemastered,
        ],
        download: Some(DownloadId::RaMusicCounterstrike),
    },
    ContentPackage {
        id: PackageId::RaMusicAftermath,
        game: GameId::RedAlert,
        title: "Aftermath Music",
        required: false,
        test_files: &[
            "expand/await.aud",
            "expand/bog.aud",
            "expand/float_v2.aud",
            "expand/gloom.aud",
            "expand/grndwire.aud",
            "expand/rpt.aud",
            "expand/search.aud",
            "expand/traction.aud",
            "expand/wastelnd.aud",
        ],
        sources: &[
            SourceId::AftermathDisc,
            SourceId::SteamTuc,
            SourceId::OriginTuc,
            SourceId::SteamRemastered,
            SourceId::OriginRemastered,
        ],
        download: Some(DownloadId::RaMusicAftermath),
    },
    // ══════════════════════════════════════════════════════════════════════
    // Tiberian Dawn — freeware since 31 August 2007 (EA release)
    // ══════════════════════════════════════════════════════════════════════
    ContentPackage {
        id: PackageId::TdBase,
        game: GameId::TiberianDawn,
        title: "Tiberian Dawn Base Game",
        required: true,
        test_files: &[
            "conquer.mix",
            "desert.mix",
            "general.mix",
            "scores.mix",
            "sounds.mix",
            "speech.mix",
            "temperat.mix",
            "transit.mix",
            "winter.mix",
        ],
        sources: &[
            SourceId::TdGdiDisc,
            SourceId::TdNodDisc,
            SourceId::TdSteamCnc,
            SourceId::TdSteamRemastered,
            SourceId::TdOriginCnc,
        ],
        download: Some(DownloadId::TdBaseFiles),
    },
    ContentPackage {
        id: PackageId::TdCovertOps,
        game: GameId::TiberianDawn,
        title: "Covert Operations Expansion",
        required: false,
        test_files: &["sc-000.mix", "sc-001.mix"],
        sources: &[
            SourceId::TdCovertOpsDisc,
            SourceId::TdSteamCnc,
            SourceId::TdOriginCnc,
        ],
        download: Some(DownloadId::TdCovertOps),
    },
    ContentPackage {
        id: PackageId::TdMusic,
        game: GameId::TiberianDawn,
        title: "Tiberian Dawn Music",
        required: false,
        test_files: &["scores.mix"],
        sources: &[
            SourceId::TdGdiDisc,
            SourceId::TdNodDisc,
            SourceId::TdSteamCnc,
            SourceId::TdOriginCnc,
        ],
        download: Some(DownloadId::TdMusic),
    },
    ContentPackage {
        id: PackageId::TdMoviesGdi,
        game: GameId::TiberianDawn,
        title: "GDI Campaign Movies",
        required: false,
        test_files: &[
            "movies/gdi1.vqa",
            "movies/gdi2.vqa",
            "movies/gdi3.vqa",
            "movies/gdi15.vqa",
            "movies/logo.vqa",
        ],
        sources: &[
            SourceId::TdGdiDisc,
            SourceId::TdSteamCnc,
            SourceId::TdOriginCnc,
        ],
        download: Some(DownloadId::TdMoviesGdi),
    },
    ContentPackage {
        id: PackageId::TdMoviesNod,
        game: GameId::TiberianDawn,
        title: "Nod Campaign Movies",
        required: false,
        test_files: &[
            "movies/nod1.vqa",
            "movies/nod2.vqa",
            "movies/nod3.vqa",
            "movies/nod10.vqa",
            "movies/logo.vqa",
        ],
        sources: &[
            SourceId::TdNodDisc,
            SourceId::TdSteamCnc,
            SourceId::TdOriginCnc,
        ],
        download: Some(DownloadId::TdMoviesNod),
    },
    // ══════════════════════════════════════════════════════════════════════
    // Dune 2 — LOCAL SOURCE ONLY (NOT freeware, no downloads)
    //
    // Dune 2 was NEVER officially released as freeware by EA. The Dune IP
    // belongs to the Herbert estate. We support extraction from local copies
    // the user already owns, but we do NOT download this game.
    // ══════════════════════════════════════════════════════════════════════
    ContentPackage {
        id: PackageId::Dune2Base,
        game: GameId::Dune2,
        title: "Dune II Complete Game (local source only)",
        required: true,
        test_files: &[
            "DUNE2.EXE",
            "SCENARIO.PAK",
            "SOUND.PAK",
            "DUNE.PAK",
            "ATRE.PAK",
            "HARK.PAK",
            "ORDOS.PAK",
        ],
        sources: &[SourceId::Dune2Disc, SourceId::GogDune2],
        download: None, // NOT freeware — no download
    },
    // ══════════════════════════════════════════════════════════════════════
    // Dune 2000 — LOCAL SOURCE ONLY (NOT freeware, no downloads)
    //
    // Dune 2000 was never released as freeware. We support extraction from
    // local copies the user already owns (retail disc, GOG, etc.).
    // ══════════════════════════════════════════════════════════════════════
    ContentPackage {
        id: PackageId::Dune2000Base,
        game: GameId::Dune2000,
        title: "Dune 2000 Game (local source only)",
        required: true,
        test_files: &[
            "DUNE2000.EXE",
            "SETUP.Z",
            "LEPTON.FNT",
            "ICON.ICN",
            "MOUSE.SHP",
        ],
        sources: &[SourceId::Dune2000Disc, SourceId::GogDune2000],
        download: None, // NOT freeware — no download
    },
    // ══════════════════════════════════════════════════════════════════════
    // Tiberian Sun — freeware since 2010 (EA release promoting C&C4)
    //
    // EA released Tiberian Sun + Firestorm as freeware in 2010.
    // cnc-comm.com hosts disc ISOs as ZIP archives. IC content-bootstrap
    // will host extracted music and movies as separate downloads.
    // ══════════════════════════════════════════════════════════════════════

    // ── Required packages ─────────────────────────────────────────────
    ContentPackage {
        id: PackageId::TsBase,
        game: GameId::TiberianSun,
        title: "Tiberian Sun Base Game",
        required: true,
        test_files: &[
            "tibsun.mix",
            "cache.mix",
            "conquer.mix",
            "local.mix",
            "isosnow.mix",
            "isotemp.mix",
        ],
        sources: &[
            SourceId::TsDisc,
            SourceId::TsSteamTuc,
            SourceId::TsOriginTuc,
        ],
        download: Some(DownloadId::TsBaseFiles),
    },
    ContentPackage {
        id: PackageId::TsFirestorm,
        game: GameId::TiberianSun,
        title: "Firestorm Expansion",
        required: false,
        test_files: &["e01sc01.mix", "e01sc02.mix"],
        sources: &[
            SourceId::TsFirestormDisc,
            SourceId::TsSteamTuc,
            SourceId::TsOriginTuc,
        ],
        download: Some(DownloadId::TsExpand),
    },
    // ── Optional packages ─────────────────────────────────────────────
    ContentPackage {
        id: PackageId::TsMusic,
        game: GameId::TiberianSun,
        title: "Tiberian Sun Music",
        required: false,
        test_files: &["scores.mix"],
        sources: &[
            SourceId::TsDisc,
            SourceId::TsSteamTuc,
            SourceId::TsOriginTuc,
        ],
        download: Some(DownloadId::TsMusic),
    },
    ContentPackage {
        id: PackageId::TsMovies,
        game: GameId::TiberianSun,
        title: "Tiberian Sun Movies",
        required: false,
        test_files: &["movies01.mix"],
        sources: &[
            SourceId::TsDisc,
            SourceId::TsSteamTuc,
            SourceId::TsOriginTuc,
        ],
        download: Some(DownloadId::TsMovies),
    },
    // ══════════════════════════════════════════════════════════════════════
    // Red Alert 2 — LOCAL SOURCE ONLY (NOT freeware, no downloads)
    //
    // Red Alert 2 was never released as freeware. We support extraction from
    // local copies the user already owns (retail disc, Steam TUC, Origin, etc.).
    // ══════════════════════════════════════════════════════════════════════

    // ── Required packages ─────────────────────────────────────────────
    ContentPackage {
        id: PackageId::Ra2Base,
        game: GameId::RedAlert2,
        title: "Red Alert 2 Base Game",
        required: true,
        test_files: &["ra2.mix", "language.mix", "multi.mix"],
        sources: &[
            SourceId::Ra2Disc,
            SourceId::Ra2TheFirstDecade,
            SourceId::Ra2SteamTuc,
            SourceId::Ra2OriginTuc,
        ],
        download: None, // NOT freeware — no download
    },
    ContentPackage {
        id: PackageId::Ra2YurisRevenge,
        game: GameId::RedAlert2,
        title: "Yuri's Revenge Expansion",
        required: false,
        test_files: &["ra2md.mix", "langmd.mix"],
        sources: &[
            SourceId::Ra2YrDisc,
            SourceId::Ra2TheFirstDecade,
            SourceId::Ra2SteamTuc,
            SourceId::Ra2OriginTuc,
        ],
        download: None,
    },
    // ── Optional packages ─────────────────────────────────────────────
    ContentPackage {
        id: PackageId::Ra2Music,
        game: GameId::RedAlert2,
        title: "Red Alert 2 Music",
        required: false,
        test_files: &["theme.mix"],
        sources: &[
            SourceId::Ra2Disc,
            SourceId::Ra2TheFirstDecade,
            SourceId::Ra2SteamTuc,
            SourceId::Ra2OriginTuc,
        ],
        download: None,
    },
    ContentPackage {
        id: PackageId::Ra2Movies,
        game: GameId::RedAlert2,
        title: "Red Alert 2 Movies",
        required: false,
        test_files: &["ra2.vqa"],
        sources: &[
            SourceId::Ra2Disc,
            SourceId::Ra2TheFirstDecade,
            SourceId::Ra2SteamTuc,
            SourceId::Ra2OriginTuc,
        ],
        download: None,
    },
    // ══════════════════════════════════════════════════════════════════════
    // C&C Generals — LOCAL SOURCE ONLY (NOT freeware, no downloads)
    //
    // Generals uses BIG archives instead of MIX. We support extraction from
    // local copies the user already owns (retail disc, Steam TUC, Origin, etc.).
    // ══════════════════════════════════════════════════════════════════════

    // ── Required packages ─────────────────────────────────────────────
    ContentPackage {
        id: PackageId::GenBase,
        game: GameId::Generals,
        title: "C&C Generals Base Game",
        required: true,
        test_files: &["INI.big", "Terrain.big", "W3D.big"],
        sources: &[
            SourceId::GenDisc,
            SourceId::GenSteamTuc,
            SourceId::GenOriginTuc,
        ],
        download: None, // NOT freeware — no download
    },
    // The Steam and Origin TUC editions merge Zero Hour content into the
    // base Generals BIG archives — no separate INIZH.big / W3DZH.big exist.
    // Only the retail disc has standalone ZH archives.
    ContentPackage {
        id: PackageId::GenZeroHour,
        game: GameId::Generals,
        title: "Zero Hour Expansion",
        required: false,
        test_files: &["INIZH.big", "W3DZH.big"],
        sources: &[SourceId::GenZhDisc],
        download: None,
    },
];
