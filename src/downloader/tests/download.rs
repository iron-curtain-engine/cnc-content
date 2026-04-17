//! Download function tests — NotFreeware, Display messages, download_package,
//! download_missing, select_strategy, and security constant checks.

use super::*;

// ── DownloadError::NotFreeware ───────────────────────────────────

/// Verifies that `download_missing` rejects non-freeware games immediately
/// (before any HTTP calls), returning `DownloadError::NotFreeware`.
#[test]
fn download_missing_rejects_non_freeware() {
    let tmp = std::env::temp_dir().join("cnc-dl-notfreeware");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();

    let result = download_missing(
        &tmp,
        GameId::Dune2,
        SeedingPolicy::ExtractAndDelete,
        &mut noop_progress,
    );
    assert!(result.is_err(), "Dune2 is not freeware, should be rejected");

    let err = result.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("not freeware"),
        "error should mention 'not freeware': {msg}"
    );
    assert!(
        msg.contains("Dune"),
        "error should mention game name containing 'Dune': {msg}"
    );

    let _ = fs::remove_dir_all(&tmp);
}

// ── DownloadError Display messages ───────────────────────────────

/// Verifies the Display impl for `DownloadError::NoUrls` includes
/// the expected human-readable message.
#[test]
fn download_error_display_no_urls() {
    let err = DownloadError::NoUrls;
    let msg = err.to_string();
    assert!(
        msg.contains("no download URLs"),
        "NoUrls display should contain 'no download URLs': {msg}"
    );
}

/// Verifies the Display impl for `DownloadError::Sha1Mismatch`
/// includes both the expected and actual hash values.
#[test]
fn download_error_display_sha1_mismatch() {
    let err = DownloadError::Sha1Mismatch {
        expected: "aaa".into(),
        actual: "bbb".into(),
    };
    let msg = err.to_string();
    assert!(msg.contains("aaa"), "should contain expected hash: {msg}");
    assert!(msg.contains("bbb"), "should contain actual hash: {msg}");
}

/// Verifies the Display impl for `DownloadError::AllMirrorsFailed`
/// includes the mirror count and last error message.
#[test]
fn download_error_display_all_mirrors_failed() {
    let err = DownloadError::AllMirrorsFailed {
        count: 3,
        last_error: "timeout".into(),
    };
    let msg = err.to_string();
    assert!(msg.contains("3"), "should contain mirror count: {msg}");
    assert!(msg.contains("timeout"), "should contain last error: {msg}");
}

/// Verifies the Display impl for `DownloadError::NotFreeware`
/// includes the game name and "not freeware" phrasing.
#[test]
fn download_error_display_not_freeware() {
    let err = DownloadError::NotFreeware {
        game: "Dune II".into(),
    };
    let msg = err.to_string();
    assert!(msg.contains("Dune II"), "should contain game name: {msg}");
    assert!(
        msg.contains("not freeware"),
        "should contain 'not freeware': {msg}"
    );
}

// ── download_package orchestration (no HTTP) ────────────────────

/// Verifies that `download_package` returns `NoUrls` when a package has no mirror list and no direct URLs.
///
/// A package with both `mirror_list_url` and `direct_urls` empty has no reachable
/// download source. `download_package` must detect this before making any network
/// calls and return `DownloadError::NoUrls` immediately.
///
/// The test constructs a minimal `DownloadPackage` with all URL fields empty so
/// no HTTP request is attempted, making this an offline, deterministic assertion.
/// Uses an IC-hosted download ID that has no compiled mirrors, ensuring the
/// compiled-mirror path does not inject URLs.
#[test]
fn download_package_no_urls_returns_error() {
    let pkg = DownloadPackage {
        id: DownloadId::RaMoviesAllied,
        game: GameId::RedAlert,
        title: "Test Package".to_string(),
        mirror_list_url: None,
        mirrors: vec![],
        direct_urls: vec![],
        sha1: None,
        info_hash: None,
        trackers: vec![],
        web_seeds: vec![],
        provides: vec![crate::PackageId::RaMoviesAllied],
        format: "zip".to_string(),
        size_hint: 0,
    };

    let tmp = std::env::temp_dir().join("cnc-dl-no-urls");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();

    let result = download_package(&pkg, &tmp, &mut noop_progress);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("no download URLs"), "error: {err}");

    let _ = fs::remove_dir_all(&tmp);
}

// ── download_missing ────────────────────────────────────────────

/// Verifies that `download_missing` is a no-op when all required content files already exist.
///
/// If `is_content_complete` returns true, `download_missing` must return `Ok(())`
/// without emitting any progress events and without making network calls. This
/// guarantees idempotency: re-running the tool on an already-installed game does nothing.
///
/// The test seeds the temp directory with every required test-file sentinel for
/// Red Alert so that `is_content_complete` returns true, then confirms the progress
/// callback is never invoked.
#[test]
fn download_missing_already_complete_is_noop() {
    let tmp = std::env::temp_dir().join("cnc-dl-complete");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();

    // Create all required RA test files so is_content_complete returns true.
    for pkg in crate::packages_for_game(GameId::RedAlert) {
        if !pkg.required {
            continue;
        }
        for &test_file in pkg.test_files {
            let path = tmp.join(test_file);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&path, b"test").unwrap();
        }
    }

    // Should return Ok(()) immediately without any HTTP calls.
    let mut progress_called = false;
    download_missing(
        &tmp,
        GameId::RedAlert,
        SeedingPolicy::ExtractAndDelete,
        &mut |_| {
            progress_called = true;
        },
    )
    .expect("download_missing should succeed when content is complete");
    assert!(
        !progress_called,
        "no progress events when content is already complete"
    );

    let _ = fs::remove_dir_all(&tmp);
}

// ── select_strategy ─────────────────────────────────────────────

/// Verifies that `select_strategy` returns `Http` when the `torrent` feature is not compiled in.
///
/// A package with a non-empty `info_hash` would normally qualify for BitTorrent
/// download, but when the `torrent` feature flag is absent the P2P code path is
/// gated out at compile time. `select_strategy` must fall back to `Http` in that
/// case so the download still proceeds via HTTP mirrors.
#[test]
#[cfg(not(feature = "torrent"))]
fn select_strategy_http_for_no_torrent_feature() {
    let pkg = DownloadPackage {
        id: DownloadId::RaQuickInstall,
        game: GameId::RedAlert,
        title: "Test".to_string(),
        mirror_list_url: None,
        mirrors: vec![],
        direct_urls: vec![],
        sha1: None,
        info_hash: Some("abcdef1234567890abcdef1234567890abcdef12".to_string()),
        trackers: vec![],
        web_seeds: vec![],
        provides: vec![],
        format: "zip".to_string(),
        size_hint: 0,
    };
    // Even with an info_hash, without the torrent feature it should be Http.
    assert_eq!(select_strategy(&pkg), DownloadStrategy::Http);
}

// ── Security: download pre-allocation cap (CWE-400 / CWE-409) ──

/// The MAX_PREALLOC_SIZE constant is reasonable for known C&C content.
///
/// The largest downloadable packages are ~700 MB disc ISOs. The pre-allocation
/// cap must be large enough for any real content while preventing a spoofed
/// Content-Length header from allocating unbounded disk space (sparse file DoS).
#[test]
fn max_prealloc_size_is_sane() {
    use super::mirror::MAX_PREALLOC_SIZE;
    const {
        // Must fit the largest C&C disc ISOs (~700 MB).
        assert!(
            MAX_PREALLOC_SIZE >= 700_000_000,
            "pre-alloc cap must fit disc ISOs"
        );
        // Must not allow unbounded allocation (cap at a few GB).
        assert!(
            MAX_PREALLOC_SIZE <= 4 * 1024 * 1024 * 1024,
            "pre-alloc cap should prevent excessive allocation"
        );
    }
}

// ── Security: redirect limit (SSRF mitigation) ─────────────────

/// The MAX_REDIRECTS constant is reasonable for CDN content delivery.
///
/// CDN redirects (e.g. GitHub Releases → objects.githubusercontent.com)
/// typically need only 1–2 hops. Limiting to 5 prevents redirect-chain
/// attacks while allowing legitimate CDN routing.
#[test]
fn max_redirects_is_sane() {
    use super::mirror::MAX_REDIRECTS;
    const {
        // Must allow CDN redirects (GitHub uses 1–2).
        assert!(
            MAX_REDIRECTS >= 2,
            "redirect limit must allow CDN redirects"
        );
        // Must prevent long redirect chains (default ureq is 10).
        assert!(
            MAX_REDIRECTS <= 8,
            "redirect limit should prevent chain attacks"
        );
    }
}
