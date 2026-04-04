// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! HTTP/torrent download definitions for all supported games.
//!
//! Download metadata is maintained in `data/downloads.toml` — a plain-text
//! TOML file embedded at compile time via `include_str!`. This design serves
//! two purposes:
//!
//! 1. **Easy maintenance** — info hashes, mirror URLs, web seeds, and SHA-1
//!    values change over time (new mirrors, torrent creation, hash updates).
//!    Editing a TOML file is simpler than modifying Rust struct literals.
//!
//! 2. **Closed content set** — the P2P engine is hardcoded to distribute
//!    *only* the packages listed in this file. It cannot be repurposed as a
//!    general-purpose BitTorrent client. Only EA-declared freeware (Red Alert
//!    since 2008, Tiberian Dawn since 2007) has download entries.
//!
//! ## Legal basis
//!
//! - **Red Alert**: EA-declared freeware (2008-08-31)
//! - **Tiberian Dawn**: EA-declared freeware (2007-08-31), GPL-3.0 source (2025-02)
//! - **No other games** have download packages — Dune 2, Dune 2000, TS, RA2,
//!   and Generals are local-source-only.

use std::sync::LazyLock;

use serde::Deserialize;

use crate::DownloadPackage;

// ── Embedded download manifest ────────────────────────────────────────

/// Raw TOML content from `data/downloads.toml`, embedded at compile time.
/// This is the single source of truth for all downloadable content.
const DOWNLOADS_TOML: &str = include_str!("../data/downloads.toml");

/// Wrapper struct for TOML deserialization of the `[[download]]` array.
#[derive(Deserialize)]
struct DownloadManifest {
    download: Vec<DownloadPackage>,
}

/// Parsed download packages from `data/downloads.toml`.
///
/// Parsed once on first access. The TOML is embedded at compile time so
/// any syntax error is caught on the first test run, not at deployment.
static DOWNLOADS_PARSED: LazyLock<Vec<DownloadPackage>> = LazyLock::new(|| {
    // The TOML is compile-time embedded data — a parse failure here means
    // the data file has a syntax or schema error that must be fixed before
    // shipping. Panicking is correct: this is a build-time data integrity
    // check, not a runtime error path.
    let manifest: DownloadManifest = toml::from_str(DOWNLOADS_TOML)
        .expect("data/downloads.toml is embedded at compile time and must be valid TOML");
    manifest.download
});

/// Returns all HTTP/torrent download packages across all games.
///
/// The returned slice lives for `'static` because it is backed by a
/// `LazyLock` in a `static` variable.
pub fn all_downloads() -> &'static [DownloadPackage] {
    &DOWNLOADS_PARSED
}
