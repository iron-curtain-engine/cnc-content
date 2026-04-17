// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! Seeding policy + download strategy tests.

use super::*;

// ── SeedingPolicy tests ────────────────────────────────────────────────

/// Verifies that the default `SeedingPolicy` is `PauseDuringOnlinePlay`.
///
/// The default is user-visible: it appears in generated config files and help text.
/// Pinning it prevents an accidental `#[derive(Default)]` change from silently
/// switching new installations to a more or less aggressive seeding behavior.
#[test]
fn seeding_policy_default_is_pause_during_online_play() {
    assert_eq!(
        SeedingPolicy::default(),
        SeedingPolicy::PauseDuringOnlinePlay
    );
}

/// Verifies that `allows_seeding` returns the correct value for every `SeedingPolicy` variant.
///
/// Only `PauseDuringOnlinePlay` and `SeedAlways` permit seeding; `KeepNoSeed` and
/// `ExtractAndDelete` must not, since the latter two either discard the archive or
/// explicitly opt out of acting as a torrent peer.
#[test]
fn seeding_policy_allows_seeding() {
    assert!(SeedingPolicy::PauseDuringOnlinePlay.allows_seeding());
    assert!(SeedingPolicy::SeedAlways.allows_seeding());
    assert!(!SeedingPolicy::KeepNoSeed.allows_seeding());
    assert!(!SeedingPolicy::ExtractAndDelete.allows_seeding());
}

/// Verifies that `retains_archives` returns the correct value for every `SeedingPolicy` variant.
///
/// `ExtractAndDelete` is the only policy that discards the downloaded archive after
/// extraction; the other three all keep it (either for seeding or for re-verification),
/// so `retains_archives` must be `false` only for `ExtractAndDelete`.
#[test]
fn seeding_policy_retains_archives() {
    assert!(SeedingPolicy::PauseDuringOnlinePlay.retains_archives());
    assert!(SeedingPolicy::SeedAlways.retains_archives());
    assert!(SeedingPolicy::KeepNoSeed.retains_archives());
    assert!(!SeedingPolicy::ExtractAndDelete.retains_archives());
}

/// Verifies that `from_str_loose` parses all canonical names, aliases, and normalizations.
///
/// The loose parser is used for CLI arguments and config file values; it must accept
/// canonical names, underscore aliases, case variants, and hyphen-to-underscore
/// normalization, and must return `None` for unknown or empty input rather than
/// panicking.
#[test]
fn seeding_policy_from_str_loose_all_variants() {
    // Canonical names.
    assert_eq!(
        SeedingPolicy::from_str_loose("pause"),
        Some(SeedingPolicy::PauseDuringOnlinePlay)
    );
    assert_eq!(
        SeedingPolicy::from_str_loose("always"),
        Some(SeedingPolicy::SeedAlways)
    );
    assert_eq!(
        SeedingPolicy::from_str_loose("keep"),
        Some(SeedingPolicy::KeepNoSeed)
    );
    assert_eq!(
        SeedingPolicy::from_str_loose("delete"),
        Some(SeedingPolicy::ExtractAndDelete)
    );
    // Aliases.
    assert_eq!(
        SeedingPolicy::from_str_loose("default"),
        Some(SeedingPolicy::PauseDuringOnlinePlay)
    );
    assert_eq!(
        SeedingPolicy::from_str_loose("seed_always"),
        Some(SeedingPolicy::SeedAlways)
    );
    assert_eq!(
        SeedingPolicy::from_str_loose("no_seed"),
        Some(SeedingPolicy::KeepNoSeed)
    );
    assert_eq!(
        SeedingPolicy::from_str_loose("extract_and_delete"),
        Some(SeedingPolicy::ExtractAndDelete)
    );
    // Case insensitive.
    assert_eq!(
        SeedingPolicy::from_str_loose("ALWAYS"),
        Some(SeedingPolicy::SeedAlways)
    );
    // Hyphen → underscore normalization.
    assert_eq!(
        SeedingPolicy::from_str_loose("extract-and-delete"),
        Some(SeedingPolicy::ExtractAndDelete)
    );
    // Invalid.
    assert_eq!(SeedingPolicy::from_str_loose("unknown"), None);
    assert_eq!(SeedingPolicy::from_str_loose(""), None);
}

/// Verifies that every `SeedingPolicy` variant produces a non-empty display label.
///
/// Labels appear in CLI output and config file comments; an empty label would produce
/// a confusing blank entry in user-facing text.
#[test]
fn seeding_policy_label_non_empty() {
    for policy in [
        SeedingPolicy::PauseDuringOnlinePlay,
        SeedingPolicy::SeedAlways,
        SeedingPolicy::KeepNoSeed,
        SeedingPolicy::ExtractAndDelete,
    ] {
        assert!(
            !policy.label().is_empty(),
            "{:?} should have a label",
            policy
        );
    }
}

// ── Download strategy tests ────────────────────────────────────────────

/// Verifies that downloads without a torrent info hash are assigned the HTTP download strategy.
///
/// When no `info_hash` is set the torrent path is unavailable; falling back to anything
/// other than HTTP would leave those downloads permanently stuck.
#[cfg(feature = "download")]
#[test]
fn select_strategy_http_for_no_info_hash() {
    use crate::downloader::{select_strategy, DownloadStrategy};
    // Packages without info_hash should use HTTP strategy.
    for dl in downloads::all_downloads() {
        if dl.info_hash.is_none() {
            assert_eq!(
                select_strategy(dl),
                DownloadStrategy::Http,
                "{:?} has no info_hash, should use HTTP",
                dl.id,
            );
        }
    }
}
