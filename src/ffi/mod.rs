// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! C-compatible FFI surface for consuming `cnc-content` from non-Rust languages.
//!
//! ## Purpose
//!
//! This module exposes the [`ContentSession`]
//! API via `extern "C"` functions so that C# (OpenRA), C++, Python, and any
//! other language with C FFI support can:
//!
//! - Download, verify, and manage C&C game content
//! - Participate in P2P seeding (growing the swarm for all users)
//! - Stream content files directly from the P2P network
//!
//! ## C# / OpenRA integration
//!
//! OpenRA (C#/.NET) consumes this via P/Invoke:
//!
//! ```csharp
//! [DllImport("cnc_content", CallingConvention = CallingConvention.Cdecl)]
//! static extern IntPtr cnc_session_open(int game_id, string content_root);
//!
//! [DllImport("cnc_content", CallingConvention = CallingConvention.Cdecl)]
//! static extern int cnc_session_ensure_required(
//!     IntPtr session,
//!     CncProgressCallback progress_callback   // null to disable
//! );
//!
//! [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
//! delegate void CncProgressCallback(ulong bytes_downloaded, ulong total_bytes);
//!
//! [DllImport("cnc_content", CallingConvention = CallingConvention.Cdecl)]
//! static extern void cnc_session_free(IntPtr session);
//! ```
//!
//! ## Safety contract
//!
//! All functions in this module are `unsafe extern "C"` — the caller is
//! responsible for:
//! - Passing valid, non-null pointers obtained from this API
//! - Not using a session pointer after calling `cnc_session_free`
//! - Freeing returned strings with `cnc_string_free`
//! - Single-threaded access to a session (or external synchronization)
//!
//! ## Build
//!
//! Enable the `ffi` feature and build as a C dynamic library:
//!
//! ```sh
//! cargo build --release --features ffi --lib
//! ```
//!
//! The `ffi` feature adds `cdylib` to the crate's library types, producing
//! `cnc_content.dll` (Windows), `libcnc_content.so` (Linux), or
//! `libcnc_content.dylib` (macOS).

use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::path::PathBuf;
use std::ptr;

use crate::session::ContentSession;
use crate::{GameId, SeedingPolicy};

// ── ABI integer constants ────────────────────────────────────────────
//
// All stable-ABI integers are gathered here so a maintainer can review the
// full numeric contract in one place.  Values are frozen on first release;
// new entries are always appended, never renumbered.

/// `GameId` wire values.  Part of the public ABI — must never change.
const GAME_RED_ALERT: i32 = 0;
const GAME_TIBERIAN_DAWN: i32 = 1;
const GAME_DUNE2: i32 = 2;
const GAME_DUNE2000: i32 = 3;
const GAME_TIBERIAN_SUN: i32 = 4;
const GAME_RED_ALERT2: i32 = 5;
const GAME_GENERALS: i32 = 6;

/// `SeedingPolicy` wire values.  Part of the public ABI — must never change.
const SEED_PAUSE_DURING_ONLINE: i32 = 0;
const SEED_ALWAYS: i32 = 1;
const SEED_KEEP_NO_SEED: i32 = 2;
const SEED_EXTRACT_AND_DELETE: i32 = 3;

/// FFI result codes.  Zero is success; all errors are negative.
///
/// Ranges are reserved to group related errors together:
/// - `-1` : argument/pointer errors
/// - `-2 … -3` : reserved (do not use)
/// - `-4` : policy / enum validation errors
/// - `-5 … -9` : reserved (do not use)
/// - `-10 … -19` : session / download errors
const CNC_OK: i32 = 0;
const CNC_ERR_NULL_POINTER: i32 = -1;
const CNC_ERR_INVALID_POLICY: i32 = -4;
const CNC_ERR_SESSION_OPEN: i32 = -10;
const CNC_ERR_DOWNLOAD: i32 = -11;
const CNC_ERR_NOT_FREEWARE: i32 = -12;
const CNC_ERR_PATH_TRAVERSAL: i32 = -13;

// ── ABI conversion functions ─────────────────────────────────────────
//
// Each pair converts between the Rust enum and its stable integer wire value.
// Using named constants (rather than inline integers) means the compiler
// catches any mismatch between the two directions of a round-trip.

/// Maps an FFI integer to a `GameId`.
///
/// Returns `None` for any value not in the ABI table so that callers
/// cannot accidentally open a session for the wrong game by passing
/// an out-of-range integer.
fn game_id_from_int(id: i32) -> Option<GameId> {
    match id {
        GAME_RED_ALERT => Some(GameId::RedAlert),
        GAME_TIBERIAN_DAWN => Some(GameId::TiberianDawn),
        GAME_DUNE2 => Some(GameId::Dune2),
        GAME_DUNE2000 => Some(GameId::Dune2000),
        GAME_TIBERIAN_SUN => Some(GameId::TiberianSun),
        GAME_RED_ALERT2 => Some(GameId::RedAlert2),
        GAME_GENERALS => Some(GameId::Generals),
        _ => None,
    }
}

/// Maps a `GameId` to its stable FFI integer.
///
/// Centralizing the mapping here ensures every caller (return values,
/// tests) uses the same ABI integers and cannot drift from
/// `game_id_from_int`.
fn game_id_to_int(id: GameId) -> i32 {
    match id {
        GameId::RedAlert => GAME_RED_ALERT,
        GameId::TiberianDawn => GAME_TIBERIAN_DAWN,
        GameId::Dune2 => GAME_DUNE2,
        GameId::Dune2000 => GAME_DUNE2000,
        GameId::TiberianSun => GAME_TIBERIAN_SUN,
        GameId::RedAlert2 => GAME_RED_ALERT2,
        GameId::Generals => GAME_GENERALS,
    }
}

/// Maps an FFI integer to a `SeedingPolicy`.
///
/// Returns `None` for unknown values so that a caller passing a bad
/// integer is rejected with an explicit error code rather than silently
/// applying a default policy.
fn seeding_policy_from_int(id: i32) -> Option<SeedingPolicy> {
    match id {
        SEED_PAUSE_DURING_ONLINE => Some(SeedingPolicy::PauseDuringOnlinePlay),
        SEED_ALWAYS => Some(SeedingPolicy::SeedAlways),
        SEED_KEEP_NO_SEED => Some(SeedingPolicy::KeepNoSeed),
        SEED_EXTRACT_AND_DELETE => Some(SeedingPolicy::ExtractAndDelete),
        _ => None,
    }
}

/// Maps a `SeedingPolicy` to its stable FFI integer.
///
/// Centralizing the mapping ensures callers reading the policy back out
/// always receive the same integer they originally passed in.
fn seeding_policy_to_int(policy: SeedingPolicy) -> i32 {
    match policy {
        SeedingPolicy::PauseDuringOnlinePlay => SEED_PAUSE_DURING_ONLINE,
        SeedingPolicy::SeedAlways => SEED_ALWAYS,
        SeedingPolicy::KeepNoSeed => SEED_KEEP_NO_SEED,
        SeedingPolicy::ExtractAndDelete => SEED_EXTRACT_AND_DELETE,
    }
}

// ── C string helpers ─────────────────────────────────────────────────

/// Converts a C string pointer to a Rust `&str`, returning `None` on null
/// or invalid UTF-8.
///
/// # Safety
///
/// The caller must ensure `ptr` points to a valid null-terminated C string
/// that remains valid for the duration of the call.
unsafe fn cstr_to_str<'input>(ptr: *const c_char) -> Option<&'input str> {
    if ptr.is_null() {
        return None;
    }
    // SAFETY: caller guarantees ptr is valid and null-terminated.
    unsafe { CStr::from_ptr(ptr) }.to_str().ok()
}

/// Converts a Rust string into a heap-allocated C string.
///
/// Ownership of the allocation is transferred to the caller via
/// `CString::into_raw`.  The caller must eventually pass the returned
/// pointer to `cnc_string_free`; any other deallocation is undefined
/// behavior.  Returns null if the string contains an interior NUL byte
/// or if allocation fails.
fn rust_str_to_cstring(s: &str) -> *mut c_char {
    CString::new(s).map_or(ptr::null_mut(), |c| c.into_raw())
}

// ── Session lifecycle ───────────────────────────────────────────────

/// Opens a content session for the given game.
///
/// - `game_id`: integer game identifier (see `CNC_GAME_*` constants)
/// - `content_root`: null-terminated UTF-8 path to the content directory,
///   or null to use the platform default
///
/// Returns an opaque session pointer on success, or null on error.
/// The caller must eventually call `cnc_session_free` to release it.
///
/// # Safety
///
/// `content_root` must be null or a valid null-terminated UTF-8 C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cnc_session_open(
    game_id: i32,
    content_root: *const c_char,
) -> *mut ContentSession {
    let game = match game_id_from_int(game_id) {
        Some(g) => g,
        None => return ptr::null_mut(),
    };

    let result = if content_root.is_null() {
        ContentSession::open(game)
    } else {
        // SAFETY: caller guarantees content_root is a valid C string.
        let path_str = match unsafe { cstr_to_str(content_root) } {
            Some(s) => s,
            None => return ptr::null_mut(),
        };
        ContentSession::open_with_root(game, PathBuf::from(path_str))
    };

    match result {
        Ok(session) => Box::into_raw(Box::new(session)),
        Err(_) => ptr::null_mut(),
    }
}

/// Gracefully shuts down and frees a content session.
///
/// After this call, the session pointer is invalid and must not be used.
/// Passing null is a safe no-op.
///
/// # Safety
///
/// `session` must be null or a valid pointer returned by `cnc_session_open`
/// that has not yet been freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cnc_session_free(session: *mut ContentSession) {
    if session.is_null() {
        return;
    }
    // SAFETY: caller guarantees this pointer is valid and owned.
    let session = unsafe { Box::from_raw(session) };
    session.shutdown();
}

// ── Content status ──────────────────────────────────────────────────

/// Returns the game ID for this session.
///
/// Returns a negative error code if `session` is null.
///
/// # Safety
///
/// `session` must be a valid pointer returned by `cnc_session_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cnc_session_game_id(session: *const ContentSession) -> i32 {
    if session.is_null() {
        return CNC_ERR_NULL_POINTER;
    }
    // SAFETY: caller guarantees session is valid.
    game_id_to_int(unsafe { &*session }.game())
}

/// Returns 1 if all required content is installed, 0 if not, negative on error.
///
/// # Safety
///
/// `session` must be a valid pointer returned by `cnc_session_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cnc_session_is_complete(session: *const ContentSession) -> i32 {
    if session.is_null() {
        return CNC_ERR_NULL_POINTER;
    }
    // SAFETY: caller guarantees session is valid.
    if unsafe { &*session }.is_content_complete() {
        1
    } else {
        0
    }
}

// ── Seeding policy ──────────────────────────────────────────────────

/// Returns the current seeding policy as an integer.
///
/// Returns a negative error code if `session` is null.
///
/// # Safety
///
/// `session` must be a valid pointer returned by `cnc_session_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cnc_session_seeding_policy(session: *const ContentSession) -> i32 {
    if session.is_null() {
        return CNC_ERR_NULL_POINTER;
    }
    // SAFETY: caller guarantees session is valid.
    seeding_policy_to_int(unsafe { &*session }.seeding_policy())
}

/// Sets the seeding policy.
///
/// Returns `CNC_OK` on success, negative error code on failure.
///
/// # Safety
///
/// `session` must be a valid pointer returned by `cnc_session_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cnc_session_set_seeding_policy(
    session: *mut ContentSession,
    policy: i32,
) -> i32 {
    if session.is_null() {
        return CNC_ERR_NULL_POINTER;
    }
    let policy = match seeding_policy_from_int(policy) {
        Some(p) => p,
        None => return CNC_ERR_INVALID_POLICY,
    };
    // SAFETY: caller guarantees session is valid and exclusively accessed.
    unsafe { &mut *session }.set_seeding_policy(policy);
    CNC_OK
}

/// Pauses seeding activity.
///
/// Call when the player starts an online game session. Safe to call even
/// when seeding is not active. No-op if session is null.
///
/// # Safety
///
/// `session` must be a valid pointer returned by `cnc_session_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cnc_session_pause_seeding(session: *const ContentSession) {
    if session.is_null() {
        return;
    }
    // SAFETY: caller guarantees session is valid.
    unsafe { &*session }.pause_seeding();
}

/// Resumes seeding activity.
///
/// Call when the player leaves online gameplay (back to menu, etc.).
/// No-op if session is null.
///
/// # Safety
///
/// `session` must be a valid pointer returned by `cnc_session_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cnc_session_resume_seeding(session: *const ContentSession) {
    if session.is_null() {
        return;
    }
    // SAFETY: caller guarantees session is valid.
    unsafe { &*session }.resume_seeding();
}

/// Returns 1 if seeding is currently paused, 0 if active, negative on error.
///
/// # Safety
///
/// `session` must be a valid pointer returned by `cnc_session_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cnc_session_is_seeding_paused(session: *const ContentSession) -> i32 {
    if session.is_null() {
        return CNC_ERR_NULL_POINTER;
    }
    // SAFETY: caller guarantees session is valid.
    if unsafe { &*session }.is_seeding_paused() {
        1
    } else {
        0
    }
}

// ── Download ────────────────────────────────────────────────────────

/// Downloads and installs all required content that is currently missing.
///
/// Blocks until complete. Returns `CNC_OK` on success, negative error code
/// on failure.
///
/// The `progress_callback` is called with `(bytes_downloaded, total_bytes)`
/// during the download. Pass null to disable progress reporting.
///
/// # Safety
///
/// `session` must be a valid pointer returned by `cnc_session_open`.
/// `progress_callback` must be null or a valid function pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cnc_session_ensure_required(
    session: *mut ContentSession,
    progress_callback: Option<unsafe extern "C" fn(bytes_downloaded: u64, total_bytes: u64)>,
) -> i32 {
    if session.is_null() {
        return CNC_ERR_NULL_POINTER;
    }
    // SAFETY: caller guarantees session is valid and exclusively accessed.
    let session = unsafe { &mut *session };
    let result = session.ensure_required(|progress| {
        if let Some(cb) = progress_callback {
            let (downloaded, total) = match &progress {
                crate::downloader::DownloadProgress::Downloading { bytes, total } => {
                    (*bytes, total.unwrap_or(0))
                }
                crate::downloader::DownloadProgress::Complete { .. } => (0, 0),
                _ => return, // Other variants don't carry byte progress.
            };
            // SAFETY: caller guarantees progress_callback is a valid function pointer.
            unsafe { cb(downloaded, total) };
        }
    });
    match result {
        Ok(()) => CNC_OK,
        Err(crate::session::SessionError::NotFreeware { .. }) => CNC_ERR_NOT_FREEWARE,
        Err(crate::session::SessionError::Download { .. }) => CNC_ERR_DOWNLOAD,
        Err(crate::session::SessionError::PathTraversal { .. }) => CNC_ERR_PATH_TRAVERSAL,
        Err(_) => CNC_ERR_SESSION_OPEN,
    }
}

// ── Content access ──────────────────────────────────────────────────

/// Returns the absolute path to a content file, or null if not found.
///
/// The returned string must be freed with `cnc_string_free`.
///
/// # Safety
///
/// - `session` must be a valid pointer returned by `cnc_session_open`.
/// - `relative_path` must be a valid null-terminated UTF-8 C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cnc_session_content_path(
    session: *const ContentSession,
    relative_path: *const c_char,
) -> *mut c_char {
    if session.is_null() || relative_path.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: caller guarantees both pointers are valid.
    let session = unsafe { &*session };
    let path_str = match unsafe { cstr_to_str(relative_path) } {
        Some(s) => s,
        None => return ptr::null_mut(),
    };
    match session.content_file_path(path_str) {
        Ok(Some(path)) => rust_str_to_cstring(&path.to_string_lossy()),
        _ => ptr::null_mut(),
    }
}

/// Returns the content root directory path as a C string.
///
/// The returned string must be freed with `cnc_string_free`.
///
/// # Safety
///
/// `session` must be a valid pointer returned by `cnc_session_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cnc_session_content_root(session: *const ContentSession) -> *mut c_char {
    if session.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: caller guarantees session is valid.
    let session = unsafe { &*session };
    rust_str_to_cstring(&session.content_root().to_string_lossy())
}

// ── Verification ────────────────────────────────────────────────────

/// Returns the number of files that are missing or corrupted.
///
/// Returns a negative error code if `session` is null.
///
/// # Safety
///
/// `session` must be a valid pointer returned by `cnc_session_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cnc_session_verify(session: *const ContentSession) -> i32 {
    if session.is_null() {
        return CNC_ERR_NULL_POINTER;
    }
    // SAFETY: caller guarantees session is valid.
    let issues = unsafe { &*session }.verify();
    // Saturate at i32::MAX to avoid overflow — more than 2 billion bad files
    // is not a realistic scenario.
    issues.len().min(i32::MAX as usize) as i32
}

// ── String cleanup ──────────────────────────────────────────────────

/// Frees a C string returned by any `cnc_*` function.
///
/// Passing null is a safe no-op.
///
/// # Safety
///
/// `s` must be null or a pointer returned by a `cnc_*` function.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cnc_string_free(s: *mut c_char) {
    if s.is_null() {
        return;
    }
    // SAFETY: caller guarantees this was allocated by CString::into_raw().
    drop(unsafe { CString::from_raw(s) });
}

// ── Version ─────────────────────────────────────────────────────────

/// Returns the crate version string (e.g. "0.1.0-alpha.0").
///
/// The returned string is statically allocated and must NOT be freed.
#[unsafe(no_mangle)]
pub extern "C" fn cnc_version() -> *const c_char {
    // Null-terminated at compile time. This is leaked intentionally —
    // it is a static and must not be freed by the caller.
    concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr().cast()
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests;
