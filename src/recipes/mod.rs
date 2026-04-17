// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! Install recipes — source-specific extraction sequences for each package.
//!
//! Each recipe defines the exact actions needed to extract a specific content
//! package from a specific source. File names and extraction logic match
//! OpenRA's content installer plugins.
//!
//! All data is compile-time constant (`&'static`).
//!
//! # Module layout
//!
//! Recipe data is split across three sub-modules by game family:
//!
//! | Module             | Contents                                  |
//! |--------------------|-------------------------------------------|
//! | `ra_recipes`       | Red Alert (all sources)                   |
//! | `ra2td_recipes`    | Red Alert 2, Generals, Tiberian Dawn      |
//! | `ts_dune_recipes`  | Tiberian Sun, Dune 2, Dune 2000           |
//!
//! File-mapping constants are kept in dedicated modules:
//! `ra_base`, `ra_remastered`, `ra_movies`, `ra2_gen`, `td`, `ts`, `dune`.

use std::sync::LazyLock;

use crate::InstallRecipe;

// ── File-mapping constant modules ────────────────────────────────────────────

mod ra_base;
pub(crate) use self::ra_base::{AFTERMATH_EXPAND_COPY, BASE_DIRECT_COPY, BASE_FROM_REDALERT_MIX};

mod ra_remastered;
pub(crate) use self::ra_remastered::{
    AM_MUSIC_COPY, CS_MUSIC_COPY, REMASTERED_AFTERMATH_COPY, REMASTERED_AM_MUSIC_COPY,
    REMASTERED_BASE_COPY, REMASTERED_CS_MUSIC_COPY,
};

mod ra_movies;
pub(crate) use self::ra_movies::{
    AFTERMATH_DISC_ACTIONS, AFTERMATH_DISC_MUSIC_ACTIONS, CS_DISC_MUSIC_ACTIONS,
    MOVIES_ALLIED_FROM_MAIN_MIX, MOVIES_ALLIED_FROM_REMASTERED_MIX, MOVIES_ALLIED_VQA_ENTRIES,
    MOVIES_SOVIET_FROM_MAIN_MIX, MOVIES_SOVIET_FROM_REMASTERED_MIX, MOVIES_SOVIET_VQA_ENTRIES,
    TFD_VOLUMES,
};

mod ra2_gen;
pub(crate) use self::ra2_gen::{
    GEN_BASE_COPY, GEN_ZH_COPY, RA2_BASE_COPY, RA2_DISC_BASE_COPY, RA2_DISC_MOVIES_COPY,
    RA2_DISC_YR_COPY, RA2_MOVIES_COPY, RA2_MUSIC_COPY, RA2_YR_COPY,
};

mod td;
pub(crate) use self::td::{
    TD_BASE_COPY, TD_COVERT_OPS_COPY, TD_MOVIES_GDI_DISC_COPY, TD_MOVIES_GDI_STEAM_COPY,
    TD_MOVIES_NOD_DISC_COPY, TD_MOVIES_NOD_STEAM_COPY, TD_MUSIC_COPY, TD_REMASTERED_BASE_COPY,
};

mod ts;
pub(crate) use self::ts::{TS_BASE_COPY, TS_FIRESTORM_COPY, TS_MOVIES_COPY, TS_MUSIC_COPY};

mod dune;
pub(crate) use self::dune::{DUNE2000_BASE_COPY, DUNE2_BASE_COPY};

// ── Recipe sub-tables ────────────────────────────────────────────────────────

mod ra2td_recipes;
mod ra_recipes;
mod ts_dune_recipes;

use ra2td_recipes::RA2TD_RECIPES;
use ra_recipes::RA_RECIPES;
use ts_dune_recipes::TS_DUNE_RECIPES;

// ── Master recipe table ───────────────────────────────────────────────────────

/// All install recipes, assembled from the three per-family sub-tables.
///
/// Leaked into a `'static` slice on first access so that callers that
/// return `&'static InstallRecipe` continue to work without lifetime
/// changes. The total allocation is small (a few KB) and lives for the
/// duration of the process.
pub static ALL_RECIPES: LazyLock<&'static [InstallRecipe]> = LazyLock::new(|| {
    let mut v = Vec::with_capacity(RA_RECIPES.len() + RA2TD_RECIPES.len() + TS_DUNE_RECIPES.len());
    v.extend_from_slice(RA_RECIPES);
    v.extend_from_slice(RA2TD_RECIPES);
    v.extend_from_slice(TS_DUNE_RECIPES);
    v.leak()
});

#[cfg(test)]
mod tests;
