use super::*;

/// GameId round-trips through integer conversion.
///
/// Every variant must survive a to_int → from_int cycle without loss.
/// This guards against ABI drift if new variants are added.
#[test]
fn game_id_round_trip() {
    let games = [
        GameId::RedAlert,
        GameId::TiberianDawn,
        GameId::Dune2,
        GameId::Dune2000,
        GameId::TiberianSun,
        GameId::RedAlert2,
        GameId::Generals,
    ];
    for game in games {
        let id = game_id_to_int(game);
        assert_eq!(
            game_id_from_int(id),
            Some(game),
            "round-trip failed for {game:?}"
        );
    }
}

/// SeedingPolicy round-trips through integer conversion.
///
/// Every variant must survive a `to_int` → `from_int` cycle without loss.
/// This guards against ABI drift when new variants are added.
#[test]
fn seeding_policy_round_trip() {
    let policies = [
        SeedingPolicy::PauseDuringOnlinePlay,
        SeedingPolicy::SeedAlways,
        SeedingPolicy::KeepNoSeed,
        SeedingPolicy::ExtractAndDelete,
    ];
    for policy in policies {
        let id = seeding_policy_to_int(policy);
        assert_eq!(
            seeding_policy_from_int(id),
            Some(policy),
            "round-trip failed for {policy:?}"
        );
    }
}

/// Invalid game ID returns None.
///
/// Out-of-range integers from FFI callers must be rejected cleanly rather
/// than mapping to an arbitrary `GameId` variant — silent truncation would
/// open a session for the wrong game.
#[test]
fn invalid_game_id_returns_none() {
    assert_eq!(game_id_from_int(-1), None);
    assert_eq!(game_id_from_int(99), None);
}

/// Invalid seeding policy returns None.
///
/// Out-of-range integers from FFI callers must be rejected with an error
/// code rather than mapping silently to a default policy — the caller's
/// intent must be validated, not guessed.
#[test]
fn invalid_seeding_policy_returns_none() {
    assert_eq!(seeding_policy_from_int(-1), None);
    assert_eq!(seeding_policy_from_int(99), None);
}

/// `cnc_version` returns a non-null, valid UTF-8 string.
///
/// The version pointer is statically allocated and must not be freed by the
/// caller.  This test verifies it is non-null and well-formed so that
/// callers can safely display it without crashing.
#[test]
fn version_is_valid() {
    let ptr = cnc_version();
    assert!(!ptr.is_null());
    // SAFETY: cnc_version returns a statically allocated null-terminated string.
    let version = unsafe { CStr::from_ptr(ptr) }.to_str().unwrap();
    // Verify it parses as MAJOR.MINOR.PATCH[-pre] rather than hard-coding
    // the major version, so the assertion survives a v1.0 release.
    let first_char = version.chars().next().unwrap_or('\0');
    assert!(
        first_char.is_ascii_digit(),
        "version must start with a digit, got: {version}"
    );
    assert!(
        version.contains('.'),
        "version must contain '.', got: {version}"
    );
}

/// Every exported `cnc_*` function handles null pointer arguments without crashing.
///
/// FFI callers receive null when a prior call fails (e.g. `cnc_session_open`
/// returns null on error).  Every function must degrade gracefully — return
/// an error code, return null, or be a silent no-op — rather than
/// dereferencing a null pointer.  This is the exhaustive null-safety
/// regression test required by the FFI Safety Contract.
#[test]
fn null_pointer_arguments_are_safe() {
    // SAFETY: explicitly testing null pointer handling for every exported function.
    unsafe {
        // Session read functions return CNC_ERR_NULL_POINTER.
        assert_eq!(cnc_session_game_id(ptr::null()), CNC_ERR_NULL_POINTER);
        assert_eq!(cnc_session_is_complete(ptr::null()), CNC_ERR_NULL_POINTER);
        assert_eq!(
            cnc_session_seeding_policy(ptr::null()),
            CNC_ERR_NULL_POINTER
        );
        assert_eq!(
            cnc_session_is_seeding_paused(ptr::null()),
            CNC_ERR_NULL_POINTER
        );
        assert_eq!(cnc_session_verify(ptr::null()), CNC_ERR_NULL_POINTER);

        // Session write functions return CNC_ERR_NULL_POINTER.
        assert_eq!(
            cnc_session_set_seeding_policy(ptr::null_mut(), 0),
            CNC_ERR_NULL_POINTER
        );
        assert_eq!(
            cnc_session_ensure_required(ptr::null_mut(), None),
            CNC_ERR_NULL_POINTER
        );

        // Path functions return null on null session or null relative_path.
        assert!(cnc_session_content_root(ptr::null()).is_null());
        assert!(cnc_session_content_path(ptr::null(), ptr::null()).is_null());

        // Void functions are silent no-ops on null — no crash.
        cnc_session_free(ptr::null_mut());
        cnc_session_pause_seeding(ptr::null());
        cnc_session_resume_seeding(ptr::null());

        // String free is a no-op on null — no crash.
        cnc_string_free(ptr::null_mut());
    }
}

/// `cnc_session_open` with an invalid game ID returns null.
///
/// A null return signals the failure to the caller before any session
/// state is allocated — no cleanup is required on the caller's side.
#[test]
fn open_invalid_game_returns_null() {
    // SAFETY: testing invalid game ID.
    let session = unsafe { cnc_session_open(99, ptr::null()) };
    assert!(session.is_null());
}

/// Full session lifecycle: open → query → close.
///
/// Verifies that a session can be opened with a temporary content root,
/// queried for game ID and status, and freed without leaking.
#[test]
fn session_lifecycle() {
    let tmp = std::env::temp_dir().join("cnc-ffi-lifecycle-test");
    let _ = std::fs::remove_dir_all(&tmp);

    let root = CString::new(tmp.to_string_lossy().as_ref()).unwrap();
    // SAFETY: root is a valid CString, game ID 0 = RedAlert.
    let session = unsafe { cnc_session_open(GAME_RED_ALERT, root.as_ptr()) };
    assert!(!session.is_null(), "session open should succeed");

    // SAFETY: session is valid.
    unsafe {
        assert_eq!(cnc_session_game_id(session), GAME_RED_ALERT);
        // Fresh session — content is not complete.
        assert_eq!(cnc_session_is_complete(session), 0);
        // Seeding policy is readable and not an error code.
        let policy = cnc_session_seeding_policy(session);
        assert!(policy >= 0, "seeding policy should not be an error code");
        // Content root should be non-null.
        let root_ptr = cnc_session_content_root(session);
        assert!(!root_ptr.is_null());
        cnc_string_free(root_ptr);
        // Clean up.
        cnc_session_free(session);
    }

    let _ = std::fs::remove_dir_all(&tmp);
}
