// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! Compiled mirror cache + mirror reachability tests.

use super::*;

// ── Compiled mirror cache tests ────────────────────────────────────────

/// Verifies that every package with a live mirror_list_url has cached mirror data.
///
/// Packages whose `mirror_list_url` points to a live upstream source should
/// have a `compiled_mirrors()` entry. Currently only OpenRA-hosted packages
/// are live; IC-hosted packages will be added as infrastructure comes online.
#[test]
fn compiled_mirrors_present_for_live_mirror_list_packages() {
    let expected = [
        DownloadId::RaQuickInstall,
        DownloadId::RaBaseFiles,
        DownloadId::RaAftermath,
        DownloadId::RaCncDesert,
        DownloadId::TdBaseFiles,
        DownloadId::TsBaseFiles,
        DownloadId::TsQuickInstall,
        DownloadId::TsExpand,
    ];
    for id in expected {
        assert!(
            downloads::compiled_mirrors(id).is_some(),
            "{id:?} should have compiled mirrors",
        );
    }
}

/// Verifies that packages without cached mirror lists return `None`.
///
/// IC-hosted packages (not yet live), Archive.org, and CNCNZ-only
/// packages have no cached mirror lists yet. Returning `Some` would
/// inject incorrect URLs.
#[test]
fn compiled_mirrors_none_for_uncached_packages() {
    let expected_none = [
        DownloadId::RaFullDiscs,
        DownloadId::RaFullSet,
        DownloadId::RaMoviesAllied,
        DownloadId::RaMoviesSoviet,
        DownloadId::RaMusicCounterstrike,
        DownloadId::RaMusicAftermath,
        DownloadId::TdMoviesGdi,
        DownloadId::TdMoviesNod,
        DownloadId::TdCovertOps,
        DownloadId::TdGdiIso,
        DownloadId::TdNodIso,
        DownloadId::TsGdiIso,
        DownloadId::TsNodIso,
        DownloadId::TsFirestormIso,
        DownloadId::TsMovies,
    ];
    for id in expected_none {
        assert!(
            downloads::compiled_mirrors(id).is_none(),
            "{id:?} should NOT have compiled mirrors",
        );
    }
}

/// Verifies that cached mirrors are valid HTTPS URLs.
///
/// Every mirror URL in the `mirrors` array must be HTTPS. This catches
/// accidental HTTP URLs or malformed entries that would bypass TLS.
#[test]
fn compiled_mirrors_are_valid_https_urls() {
    let ids = [
        DownloadId::RaQuickInstall,
        DownloadId::RaBaseFiles,
        DownloadId::RaAftermath,
        DownloadId::RaCncDesert,
        DownloadId::RaMusic,
        DownloadId::TdBaseFiles,
        DownloadId::TdMusic,
        DownloadId::TsBaseFiles,
        DownloadId::TsQuickInstall,
        DownloadId::TsExpand,
        DownloadId::TsMusic,
    ];
    for id in ids {
        let urls = downloads::compiled_mirrors(id).unwrap();

        assert!(
            !urls.is_empty(),
            "{id:?} compiled mirrors should contain at least one URL",
        );

        for url in urls {
            assert!(
                url.starts_with("https://"),
                "{id:?} compiled mirror URL should be HTTPS: {url}",
            );
        }
    }
}

/// Verifies that mirror_list_url fields point to the expected upstream repos.
///
/// OpenRA packages must target the OpenRA WebsiteV3 GitHub repo.
/// Packages without an upstream mirror list have `mirror_list_url = None` and
/// their mirrors are populated directly in the `mirrors` array.
/// This ensures the GH Action and runtime fetch both use the correct source.
#[test]
fn mirror_list_urls_point_to_expected_repos() {
    let openra_ids = [
        DownloadId::RaQuickInstall,
        DownloadId::RaBaseFiles,
        DownloadId::RaAftermath,
        DownloadId::RaCncDesert,
        DownloadId::TdBaseFiles,
        DownloadId::TsBaseFiles,
        DownloadId::TsQuickInstall,
        DownloadId::TsExpand,
    ];
    for id in openra_ids {
        let dl = download(id).unwrap();
        let url = dl.mirror_list_url.as_deref().unwrap_or("");
        assert!(
            url.starts_with(
                "https://raw.githubusercontent.com/OpenRA/OpenRAWebsiteV3/master/packages/"
            ),
            "{id:?} mirror_list_url should point to OpenRA GitHub repo, got: {url}",
        );
    }

    // Packages without upstream mirror lists have mirror_list_url = None.
    // This includes community-mirrored music packages AND packages whose
    // mirrors are populated directly in the mirrors array.
    let no_mirror_list_ids = [
        DownloadId::RaMusic,
        DownloadId::TdMusic,
        DownloadId::TsMusic,
        // Archive.org and single-mirror packages
        DownloadId::RaFullDiscs,
        DownloadId::RaFullSet,
        DownloadId::TdCovertOps,
        DownloadId::TdGdiIso,
        DownloadId::TdNodIso,
        DownloadId::TsGdiIso,
        DownloadId::TsNodIso,
        DownloadId::TsFirestormIso,
    ];
    for id in no_mirror_list_ids {
        let dl = download(id).unwrap();
        assert!(
            dl.mirror_list_url.is_none(),
            "{id:?} mirror_list_url should be None (mirrors in array, not upstream list), got: {:?}",
            dl.mirror_list_url,
        );
    }
}

// ── Mirror reachability (CI-only, requires network) ────────────────

/// Verifies that every package with compiled mirrors has at least one reachable mirror.
///
/// Mirror URLs are compiled into the binary and used at runtime to download
/// game content. Community mirrors are inherently unreliable — individual
/// mirrors may go down temporarily — so this test checks that **at least one**
/// mirror per package responds successfully. A package with zero reachable
/// mirrors is completely broken for users.
///
/// Sends a lightweight HEAD request (no body transfer) to each URL. Individual
/// mirror failures are printed as warnings; the test only fails when a package
/// has no working mirrors at all.
///
/// Gated behind the `CNC_TEST_MIRRORS=1` environment variable because it
/// requires network access and depends on external server availability.
/// CI sets this variable; local `cargo test` skips it by default.
#[cfg(feature = "download")]
#[test]
fn compiled_mirrors_are_reachable() {
    if std::env::var("CNC_TEST_MIRRORS").as_deref() != Ok("1") {
        return;
    }

    let agent = ureq::config::Config::builder()
        .timeout_global(Some(std::time::Duration::from_secs(30)))
        .build()
        .new_agent();

    let mut dead_packages: Vec<String> = Vec::new();

    for dl in downloads::all_downloads() {
        if dl.mirrors.is_empty() {
            continue;
        }

        let mut any_ok = false;
        for url in &dl.mirrors {
            // HEAD request — verifies reachability without downloading the file.
            match agent.head(url).call() {
                Ok(resp) => {
                    let status = resp.status().as_u16();
                    if (200..=399).contains(&status) {
                        any_ok = true;
                    } else {
                        eprintln!("  WARN: {:?} mirror returned HTTP {status}: {url}", dl.id,);
                    }
                }
                Err(err) => {
                    eprintln!("  WARN: {:?} mirror unreachable: {url} ({err})", dl.id);
                }
            }
        }

        if !any_ok {
            dead_packages.push(format!(
                "{:?}: all {} mirrors unreachable",
                dl.id,
                dl.mirrors.len(),
            ));
        }
    }

    assert!(
        dead_packages.is_empty(),
        "Packages with zero reachable mirrors:\n{}",
        dead_packages.join("\n"),
    );
}
