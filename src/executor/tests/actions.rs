//! Basic action tests: Copy, ExtractMix, ExtractMixFromContent, ExtractRaw,
//! Delete, MissingSource, Progress, path-traversal security, error Display,
//! ExtractZip error cases, and FirstError stop behaviour.

use super::*;

// ── Copy action ──────────────────────────────────────────────────

/// Copies multiple files from the source root into the content root.
///
/// After a successful `Copy` action every listed file must appear in the
/// content directory with its original byte content preserved.
#[test]
fn execute_copy_action() {
    let tmp = std::env::temp_dir().join("cnc-exec-copy");
    let _ = fs::remove_dir_all(&tmp);
    let src = tmp.join("source");
    let dst = tmp.join("content");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(&dst).unwrap();

    fs::write(src.join("allies.mix"), b"allies-data").unwrap();
    fs::write(src.join("conquer.mix"), b"conquer-data").unwrap();

    execute_recipe(&make_recipe(&COPY_ACTIONS), &src, &dst, noop_progress).unwrap();
    assert_eq!(fs::read(dst.join("allies.mix")).unwrap(), b"allies-data");
    assert_eq!(fs::read(dst.join("conquer.mix")).unwrap(), b"conquer-data");

    let _ = fs::remove_dir_all(&tmp);
}

// ── ExtractMix action ────────────────────────────────────────────

/// Extracts named entries from a MIX archive in the source root.
///
/// The executor must locate the archive, look up each entry by name, and
/// write the decompressed bytes to the correct destination path in the
/// content root.
#[test]
fn execute_extract_mix_action() {
    let tmp = std::env::temp_dir().join("cnc-exec-mix");
    let _ = fs::remove_dir_all(&tmp);
    let src = tmp.join("source");
    let dst = tmp.join("content");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(&dst).unwrap();

    let mix_data = build_mix(&[
        ("allies.mix", b"allies-content"),
        ("conquer.mix", b"conquer-content"),
    ]);
    fs::write(src.join("main.mix"), &mix_data).unwrap();

    execute_recipe(&make_recipe(&MIX_ACTIONS), &src, &dst, noop_progress).unwrap();
    assert_eq!(fs::read(dst.join("allies.mix")).unwrap(), b"allies-content");
    assert_eq!(
        fs::read(dst.join("conquer.mix")).unwrap(),
        b"conquer-content"
    );

    let _ = fs::remove_dir_all(&tmp);
}

// ── ExtractMixFromContent action ─────────────────────────────────

/// Extracts entries from a MIX archive that already lives in the content root.
///
/// `ExtractMixFromContent` reads a MIX that was written by an earlier action
/// rather than from the source media. The extracted file must land at its
/// declared sub-path inside the content directory.
#[test]
fn execute_extract_mix_from_content_action() {
    let tmp = std::env::temp_dir().join("cnc-exec-mix-content");
    let _ = fs::remove_dir_all(&tmp);
    let src = tmp.join("source");
    let dst = tmp.join("content");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(&dst).unwrap();

    let mix_data = build_mix(&[("inner.dat", b"inner-file-data")]);
    fs::write(dst.join("intermediate.mix"), &mix_data).unwrap();

    execute_recipe(
        &make_recipe(&CONTENT_MIX_ACTIONS),
        &src,
        &dst,
        noop_progress,
    )
    .unwrap();
    assert_eq!(
        fs::read(dst.join("extracted/inner.dat")).unwrap(),
        b"inner-file-data"
    );

    let _ = fs::remove_dir_all(&tmp);
}

// ── ExtractRaw action ────────────────────────────────────────────

/// Extracts an exact byte range from a source file into the content root.
///
/// The executor must seek to the specified offset and read the declared
/// number of bytes, writing only that slice to the destination path.
///
/// The test embeds the expected bytes at offset 100 inside a 256-byte file
/// and asserts that only those 8 bytes appear in the output.
#[test]
fn execute_extract_raw_action() {
    let tmp = std::env::temp_dir().join("cnc-exec-raw");
    let _ = fs::remove_dir_all(&tmp);
    let src = tmp.join("source");
    let dst = tmp.join("content");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(&dst).unwrap();

    let mut data = vec![0u8; 256];
    data[100..108].copy_from_slice(b"RAWCHUNK");
    fs::write(src.join("patch.rtp"), &data).unwrap();

    execute_recipe(&make_recipe(&RAW_ACTIONS), &src, &dst, noop_progress).unwrap();
    assert_eq!(fs::read(dst.join("expand/chunk.dat")).unwrap(), b"RAWCHUNK");

    let _ = fs::remove_dir_all(&tmp);
}

// ── Delete action ────────────────────────────────────────────────

/// Deletes a file from the content root when it exists.
///
/// A `Delete` action is used to remove interim files produced by earlier
/// recipe steps. After the action completes the file must no longer be
/// present on the filesystem.
#[test]
fn execute_delete_action() {
    let tmp = std::env::temp_dir().join("cnc-exec-delete");
    let _ = fs::remove_dir_all(&tmp);
    let src = tmp.join("source");
    let dst = tmp.join("content");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(&dst).unwrap();

    fs::write(dst.join("temp.mix"), b"temporary").unwrap();
    assert!(dst.join("temp.mix").exists());

    execute_recipe(&make_recipe(&DELETE_ACTIONS), &src, &dst, noop_progress).unwrap();
    assert!(!dst.join("temp.mix").exists());

    let _ = fs::remove_dir_all(&tmp);
}

/// Deleting a file that does not exist succeeds without error.
///
/// A `Delete` action is idempotent — if the target is already absent the
/// recipe must continue normally rather than propagating a not-found error.
#[test]
fn execute_delete_nonexistent_is_ok() {
    let tmp = std::env::temp_dir().join("cnc-exec-delete-noop");
    let _ = fs::remove_dir_all(&tmp);
    let src = tmp.join("source");
    let dst = tmp.join("content");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(&dst).unwrap();

    execute_recipe(
        &make_recipe(&DELETE_NOOP_ACTIONS),
        &src,
        &dst,
        noop_progress,
    )
    .unwrap();

    let _ = fs::remove_dir_all(&tmp);
}

// ── Missing source errors ────────────────────────────────────────

/// Returns `MixNotFound` when the referenced MIX archive is absent.
///
/// If the declared archive does not exist in the source root the executor
/// must report a clear `MixNotFound` error rather than an opaque I/O
/// failure, so callers can present a meaningful diagnostic.
#[test]
fn extract_mix_missing_archive_errors() {
    let tmp = std::env::temp_dir().join("cnc-exec-mix-missing");
    let _ = fs::remove_dir_all(&tmp);
    let src = tmp.join("source");
    let dst = tmp.join("content");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(&dst).unwrap();

    let result = execute_recipe(
        &make_recipe(&MIX_MISSING_ACTIONS),
        &src,
        &dst,
        noop_progress,
    );
    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        ExecutorError::MixNotFound { .. }
    ));

    let _ = fs::remove_dir_all(&tmp);
}

// ── Progress reporting ───────────────────────────────────────────

/// The executor emits `ActionStarted`, `FileWritten`, and `Completed` events.
///
/// Progress callbacks are the only feedback channel available to UI layers.
/// The sequence must start with `ActionStarted`, include a `FileWritten`
/// event for every file, and end with a `Completed` event that carries the
/// correct file count.
#[test]
fn executor_reports_progress() {
    let tmp = std::env::temp_dir().join("cnc-exec-progress");
    let _ = fs::remove_dir_all(&tmp);
    let src = tmp.join("source");
    let dst = tmp.join("content");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(&dst).unwrap();

    fs::write(src.join("a.mix"), b"aaa").unwrap();

    let mut events = Vec::new();
    execute_recipe(&make_recipe(&PROGRESS_ACTIONS), &src, &dst, |p| {
        events.push(p)
    })
    .unwrap();

    assert!(events.len() >= 3);
    assert!(matches!(events[0], InstallProgress::ActionStarted { .. }));
    assert!(matches!(events[1], InstallProgress::FileWritten { .. }));
    assert!(matches!(
        events.last().unwrap(),
        InstallProgress::Completed {
            files_written: 1,
            ..
        }
    ));

    let _ = fs::remove_dir_all(&tmp);
}

// ── Path traversal security ─────────────────────────────────────

static TRAVERSAL_CONTENT_FILES: [FileMapping; 1] = [FileMapping {
    from: "allies.mix",
    to: "../../escaped.txt",
}];
static TRAVERSAL_CONTENT_ACTIONS: [InstallAction; 1] = [InstallAction::Copy {
    files: &TRAVERSAL_CONTENT_FILES,
}];

static TRAVERSAL_SOURCE_FILES: [FileMapping; 1] = [FileMapping {
    from: "../../etc/passwd",
    to: "harmless.txt",
}];
static TRAVERSAL_SOURCE_ACTIONS: [InstallAction; 1] = [InstallAction::Copy {
    files: &TRAVERSAL_SOURCE_FILES,
}];

#[cfg(target_os = "windows")]
static TRAVERSAL_BACKSLASH_FILES: [FileMapping; 1] = [FileMapping {
    from: "allies.mix",
    to: "..\\..\\escaped.txt",
}];
#[cfg(target_os = "windows")]
static TRAVERSAL_BACKSLASH_ACTIONS: [InstallAction; 1] = [InstallAction::Copy {
    files: &TRAVERSAL_BACKSLASH_FILES,
}];

#[cfg(not(windows))]
static TRAVERSAL_ABSOLUTE_FILES: [FileMapping; 1] = [FileMapping {
    from: "allies.mix",
    to: "/tmp/escaped.txt",
}];
#[cfg(windows)]
static TRAVERSAL_ABSOLUTE_FILES: [FileMapping; 1] = [FileMapping {
    from: "allies.mix",
    to: "C:\\escaped.txt",
}];
static TRAVERSAL_ABSOLUTE_ACTIONS: [InstallAction; 1] = [InstallAction::Copy {
    files: &TRAVERSAL_ABSOLUTE_FILES,
}];

/// Rejects a `to` path that traverses above the content root.
///
/// Path traversal in recipe destinations would allow writing files outside
/// the managed content directory, breaking the sandbox boundary.
#[test]
fn executor_rejects_content_path_traversal() {
    let tmp = std::env::temp_dir().join("cnc-exec-traversal-content");
    let _ = fs::remove_dir_all(&tmp);
    let src = tmp.join("source");
    let dst = tmp.join("content");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(&dst).unwrap();

    fs::write(src.join("allies.mix"), b"allies-data").unwrap();

    let result = execute_recipe(
        &make_recipe(&TRAVERSAL_CONTENT_ACTIONS),
        &src,
        &dst,
        noop_progress,
    );
    assert!(matches!(result, Err(ExecutorError::PathTraversal { .. })));

    // The escaped file must not exist above the content root.
    assert!(!tmp.join("escaped.txt").exists());

    let _ = fs::remove_dir_all(&tmp);
}

/// Rejects a `from` path that traverses above the source root.
///
/// Path traversal in recipe sources would allow reading arbitrary files
/// from the host filesystem, breaking source-boundary containment.
#[test]
fn executor_rejects_source_path_traversal() {
    let tmp = std::env::temp_dir().join("cnc-exec-traversal-source");
    let _ = fs::remove_dir_all(&tmp);
    let src = tmp.join("source");
    let dst = tmp.join("content");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(&dst).unwrap();

    let result = execute_recipe(
        &make_recipe(&TRAVERSAL_SOURCE_ACTIONS),
        &src,
        &dst,
        noop_progress,
    );
    assert!(matches!(result, Err(ExecutorError::PathTraversal { .. })));

    let _ = fs::remove_dir_all(&tmp);
}

/// Rejects backslash-style path traversal in the `to` field.
///
/// On Windows, backslashes are path separators, so `..\\..\\escaped.txt`
/// is a genuine traversal attack that `strict-path` must reject.
/// On Unix, backslashes are literal filename characters — `..\\..\\`
/// is a single valid filename component, not traversal. This test
/// only applies to Windows.
#[cfg(target_os = "windows")]
#[test]
fn executor_rejects_backslash_traversal() {
    let tmp = std::env::temp_dir().join("cnc-exec-traversal-backslash");
    let _ = fs::remove_dir_all(&tmp);
    let src = tmp.join("source");
    let dst = tmp.join("content");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(&dst).unwrap();

    fs::write(src.join("allies.mix"), b"allies-data").unwrap();

    let result = execute_recipe(
        &make_recipe(&TRAVERSAL_BACKSLASH_ACTIONS),
        &src,
        &dst,
        noop_progress,
    );
    assert!(matches!(result, Err(ExecutorError::PathTraversal { .. })));

    let _ = fs::remove_dir_all(&tmp);
}

/// Rejects absolute paths in the `to` field of a Copy action.
///
/// An absolute destination path bypasses the content root entirely,
/// allowing writes to arbitrary filesystem locations.
#[test]
fn executor_rejects_absolute_path_in_copy() {
    let tmp = std::env::temp_dir().join("cnc-exec-traversal-absolute");
    let _ = fs::remove_dir_all(&tmp);
    let src = tmp.join("source");
    let dst = tmp.join("content");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(&dst).unwrap();

    fs::write(src.join("allies.mix"), b"allies-data").unwrap();

    let result = execute_recipe(
        &make_recipe(&TRAVERSAL_ABSOLUTE_ACTIONS),
        &src,
        &dst,
        noop_progress,
    );
    assert!(matches!(result, Err(ExecutorError::PathTraversal { .. })));

    let _ = fs::remove_dir_all(&tmp);
}

// ── Error Display messages ──────────────────────────────────────

/// Display impl for MixNotFound includes the archive path.
///
/// User-facing error messages must identify which file was missing so
/// the user can diagnose the problem without reading source code.
#[test]
fn executor_error_display_mix_not_found() {
    let err = ExecutorError::MixNotFound {
        path: PathBuf::from("source/main.mix"),
    };
    let msg = err.to_string();
    assert!(msg.contains("source/main.mix"), "message was: {msg}");
}

/// Display impl for MixEntryNotFound includes both archive and entry.
///
/// When a specific entry is missing from a MIX archive the message must
/// name both the archive and the entry for actionable diagnostics.
#[test]
fn executor_error_display_mix_entry_not_found() {
    let err = ExecutorError::MixEntryNotFound {
        archive: "main.mix".to_string(),
        entry: "conquer.mix".to_string(),
    };
    let msg = err.to_string();
    assert!(msg.contains("main.mix"), "message was: {msg}");
    assert!(msg.contains("conquer.mix"), "message was: {msg}");
}

/// Display impl for PathTraversal includes the offending path and detail.
///
/// Security-relevant errors must expose enough context in the message for
/// audit logging without requiring structured error inspection.
#[test]
fn executor_error_display_path_traversal() {
    let err = ExecutorError::PathTraversal {
        path: "../../etc/passwd".to_string(),
        detail: "escapes boundary".to_string(),
    };
    let msg = err.to_string();
    assert!(msg.contains("../../etc/passwd"), "message was: {msg}");
    assert!(msg.contains("escapes boundary"), "message was: {msg}");
}

/// Display impl for SourceFileNotFound includes the file path.
///
/// Missing-file errors must identify the path so callers can distinguish
/// which source file was absent in multi-action recipes.
#[test]
fn executor_error_display_source_file_not_found() {
    let err = ExecutorError::SourceFileNotFound {
        path: PathBuf::from("missing/file.dat"),
    };
    let msg = err.to_string();
    assert!(msg.contains("missing/file.dat"), "message was: {msg}");
}

// ── ExtractZip error cases ──────────────────────────────────────

static ZIP_MISSING_FILES: [FileMapping; 1] = [FileMapping {
    from: "nonexistent.zip/entry.dat",
    to: "out.dat",
}];
static ZIP_MISSING_ACTIONS: [InstallAction; 1] = [InstallAction::ExtractZip {
    entries: &ZIP_MISSING_FILES,
}];

/// ExtractZip fails with SourceFileNotFound when the ZIP does not exist.
///
/// When neither a direct file nor a containing ZIP archive can be found
/// in the source tree, the executor must report a clear not-found error
/// rather than silently skipping the entry.
#[test]
fn executor_extract_zip_missing_source() {
    let tmp = std::env::temp_dir().join("cnc-exec-zip-missing");
    let _ = fs::remove_dir_all(&tmp);
    let src = tmp.join("source");
    let dst = tmp.join("content");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(&dst).unwrap();

    let result = execute_recipe(
        &make_recipe(&ZIP_MISSING_ACTIONS),
        &src,
        &dst,
        noop_progress,
    );
    assert!(matches!(
        result,
        Err(ExecutorError::SourceFileNotFound { .. })
    ));

    let _ = fs::remove_dir_all(&tmp);
}

// ── First error stops execution ─────────────────────────────────

static STOP_COPY_FILES: [FileMapping; 1] = [FileMapping {
    from: "should_not_exist.mix",
    to: "should_not_exist.mix",
}];
static STOP_ACTIONS: [InstallAction; 2] = [
    // Action 0: ExtractMix from a nonexistent archive — will fail.
    InstallAction::ExtractMix {
        source_mix: "nonexistent.mix",
        entries: &MIX_MISSING_ENTRIES,
    },
    // Action 1: Copy — should never run.
    InstallAction::Copy {
        files: &STOP_COPY_FILES,
    },
];

/// Execution halts on the first failing action without running later ones.
///
/// Continuing past a failed action could leave content in an inconsistent
/// state. The executor must short-circuit and return the first error,
/// leaving subsequent actions unattempted.
#[test]
fn executor_stops_on_first_error() {
    let tmp = std::env::temp_dir().join("cnc-exec-stop-first");
    let _ = fs::remove_dir_all(&tmp);
    let src = tmp.join("source");
    let dst = tmp.join("content");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(&dst).unwrap();

    // Place the file that action 1 would copy, so we can verify it
    // was never copied (proving action 1 was not attempted).
    fs::write(src.join("should_not_exist.mix"), b"action2-data").unwrap();

    let result = execute_recipe(&make_recipe(&STOP_ACTIONS), &src, &dst, noop_progress);
    assert!(result.is_err());

    // Action 2's output must not exist — it was never attempted.
    assert!(!dst.join("should_not_exist.mix").exists());

    let _ = fs::remove_dir_all(&tmp);
}
