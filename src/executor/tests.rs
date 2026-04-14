//! Unit tests for the install recipe executor.
//!
//! Covers MIX extraction, BIG extraction, MEG extraction, BAG/IDX extraction,
//! ISCAB extraction, ZIP extraction, raw-offset extraction, file copy, and
//! delete actions, plus path-traversal security tests verifying that
//! `strict-path` boundaries are enforced.

use super::*;
use crate::actions::{FileMapping, RawExtractEntry};

fn noop_progress(_: InstallProgress) {}

// ── Helper: build a minimal MIX archive from name/data pairs ─────

fn build_mix(files: &[(&str, &[u8])]) -> Vec<u8> {
    use cnc_formats::mix::crc;
    let mut entries: Vec<(cnc_formats::mix::MixCrc, &[u8])> = files
        .iter()
        .map(|(name, data)| (crc(name), *data))
        .collect();
    entries.sort_by_key(|(c, _)| c.to_raw() as i32);

    let count = entries.len() as u16;
    let mut offsets = Vec::with_capacity(entries.len());
    let mut cur = 0u32;
    for (_, data) in &entries {
        offsets.push(cur);
        cur += data.len() as u32;
    }
    let data_size = cur;

    let mut out = Vec::new();
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&data_size.to_le_bytes());
    for (i, (c, data)) in entries.iter().enumerate() {
        out.extend_from_slice(&c.to_raw().to_le_bytes());
        out.extend_from_slice(&offsets[i].to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    }
    for (_, data) in &entries {
        out.extend_from_slice(data);
    }
    out
}

// ── Static recipe data for tests ───────────────────────────────

static COPY_FILES: [FileMapping; 2] = [
    FileMapping {
        from: "allies.mix",
        to: "allies.mix",
    },
    FileMapping {
        from: "conquer.mix",
        to: "conquer.mix",
    },
];
static COPY_ACTIONS: [InstallAction; 1] = [InstallAction::Copy { files: &COPY_FILES }];

static MIX_ENTRIES: [FileMapping; 2] = [
    FileMapping {
        from: "allies.mix",
        to: "allies.mix",
    },
    FileMapping {
        from: "conquer.mix",
        to: "conquer.mix",
    },
];
static MIX_ACTIONS: [InstallAction; 1] = [InstallAction::ExtractMix {
    source_mix: "main.mix",
    entries: &MIX_ENTRIES,
}];

static CONTENT_MIX_ENTRIES: [FileMapping; 1] = [FileMapping {
    from: "inner.dat",
    to: "extracted/inner.dat",
}];
static CONTENT_MIX_ACTIONS: [InstallAction; 1] = [InstallAction::ExtractMixFromContent {
    content_mix: "intermediate.mix",
    entries: &CONTENT_MIX_ENTRIES,
}];

static RAW_ENTRIES: [RawExtractEntry; 1] = [RawExtractEntry {
    source: "patch.rtp",
    offset: 100,
    length: 8,
    to: "expand/chunk.dat",
}];
static RAW_ACTIONS: [InstallAction; 1] = [InstallAction::ExtractRaw {
    entries: &RAW_ENTRIES,
}];

static DELETE_ACTIONS: [InstallAction; 1] = [InstallAction::Delete { path: "temp.mix" }];
static DELETE_NOOP_ACTIONS: [InstallAction; 1] = [InstallAction::Delete {
    path: "nonexistent.mix",
}];

static MIX_MISSING_ENTRIES: [FileMapping; 1] = [FileMapping {
    from: "foo",
    to: "foo",
}];
static MIX_MISSING_ACTIONS: [InstallAction; 1] = [InstallAction::ExtractMix {
    source_mix: "nonexistent.mix",
    entries: &MIX_MISSING_ENTRIES,
}];

static PROGRESS_FILES: [FileMapping; 1] = [FileMapping {
    from: "a.mix",
    to: "a.mix",
}];
static PROGRESS_ACTIONS: [InstallAction; 1] = [InstallAction::Copy {
    files: &PROGRESS_FILES,
}];

fn make_recipe(actions: &'static [InstallAction]) -> InstallRecipe {
    InstallRecipe {
        source: crate::SourceId::SteamTuc,
        package: crate::PackageId::RaBase,
        actions,
    }
}

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

// ── Helper: build a minimal BIGF archive from name/data pairs ────

/// Builds a synthetic BIG archive (BIGF variant) containing the given files.
///
/// Format: 4-byte magic "BIGF", u32 LE archive_size, u32 BE entry_count,
/// u32 BE first_data_offset, then per-entry u32 BE offset + u32 BE size +
/// NUL-terminated name, followed by raw file data.
fn build_big(files: &[(&str, &[u8])]) -> Vec<u8> {
    // Pre-calculate index size to know first_data_offset.
    let mut index_size: usize = 0;
    for (name, _) in files {
        // 8 (offset + size) + name bytes + NUL terminator
        index_size += 8 + name.len() + 1;
    }
    let first_data_offset = 16 + index_size;

    // Pre-calculate data offsets.
    let mut offsets = Vec::with_capacity(files.len());
    let mut cur_data = first_data_offset;
    for (_, data) in files {
        offsets.push(cur_data as u32);
        cur_data += data.len();
    }
    let archive_size = cur_data;

    let mut out = Vec::with_capacity(archive_size);
    // Header
    out.extend_from_slice(b"BIGF");
    out.extend_from_slice(&(archive_size as u32).to_le_bytes());
    out.extend_from_slice(&(files.len() as u32).to_be_bytes());
    out.extend_from_slice(&(first_data_offset as u32).to_be_bytes());

    // Index entries
    for (i, (name, data)) in files.iter().enumerate() {
        out.extend_from_slice(&offsets[i].to_be_bytes());
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(name.as_bytes());
        out.push(0); // NUL terminator
    }

    // File data
    for (_, data) in files {
        out.extend_from_slice(data);
    }

    out
}

// ── Helper: build a minimal legacy MEG archive from name/data pairs ──

/// Builds a synthetic legacy MEG archive (format 1) containing the given files.
///
/// Legacy format: u32 LE num_filenames, u32 LE num_files, then filename table
/// (u16 LE length + name bytes per entry), then file records (20 bytes each:
/// crc u32 + index u32 + size u32 + start u32 + name_index u32), then data.
fn build_meg(files: &[(&str, &[u8])]) -> Vec<u8> {
    let num = files.len();

    // Build filename table bytes.
    let mut name_table = Vec::new();
    for (name, _) in files {
        name_table.extend_from_slice(&(name.len() as u16).to_le_bytes());
        name_table.extend_from_slice(name.as_bytes());
    }

    // file records start after header (8 bytes) + name table
    let records_start = 8 + name_table.len();
    let data_start = records_start + num * 20;

    // Build file records.
    let mut records = Vec::new();
    let mut data_offset = data_start;
    for (i, (_, data)) in files.iter().enumerate() {
        records.extend_from_slice(&0u32.to_le_bytes()); // CRC placeholder
        records.extend_from_slice(&(i as u32).to_le_bytes()); // index
        records.extend_from_slice(&(data.len() as u32).to_le_bytes()); // size
        records.extend_from_slice(&(data_offset as u32).to_le_bytes()); // start (absolute)
        records.extend_from_slice(&(i as u32).to_le_bytes()); // name_index
        data_offset += data.len();
    }

    let mut out = Vec::new();
    // Header
    out.extend_from_slice(&(num as u32).to_le_bytes()); // num_filenames
    out.extend_from_slice(&(num as u32).to_le_bytes()); // num_files
                                                        // Filename table
    out.extend_from_slice(&name_table);
    // File records
    out.extend_from_slice(&records);
    // File data
    for (_, data) in files {
        out.extend_from_slice(data);
    }

    out
}

// ── Helper: build a minimal IDX index file ──────────────────────

/// Builds a synthetic IDX index file with 36-byte entries.
///
/// Format per entry: 16-byte NUL-padded name + u32 LE offset + u32 LE size
/// + u32 LE sample_rate + u32 LE flags + u32 LE chunk_size.
fn build_idx(entries: &[(&str, u32, u32)]) -> Vec<u8> {
    let mut out = Vec::with_capacity(entries.len() * 36);
    for (name, offset, size) in entries {
        let mut name_buf = [0u8; 16];
        let copy_len = name.len().min(15);
        name_buf[..copy_len].copy_from_slice(name.as_bytes().get(..copy_len).unwrap_or(b""));
        out.extend_from_slice(&name_buf);
        out.extend_from_slice(&offset.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&22050u32.to_le_bytes()); // sample_rate
        out.extend_from_slice(&0u32.to_le_bytes()); // flags
        out.extend_from_slice(&0u32.to_le_bytes()); // chunk_size
    }
    out
}

// ── ExtractBig action ────────────────────────────────────────────

static BIG_ENTRIES: [FileMapping; 2] = [
    FileMapping {
        from: "data\\config.ini",
        to: "config.ini",
    },
    FileMapping {
        from: "data\\terrain.tga",
        to: "terrain.tga",
    },
];
static BIG_ACTIONS: [InstallAction; 1] = [InstallAction::ExtractBig {
    source_big: "test.big",
    entries: &BIG_ENTRIES,
}];

static BIG_MISSING_ENTRIES: [FileMapping; 1] = [FileMapping {
    from: "missing_entry",
    to: "out.dat",
}];
static BIG_MISSING_ENTRY_ACTIONS: [InstallAction; 1] = [InstallAction::ExtractBig {
    source_big: "test.big",
    entries: &BIG_MISSING_ENTRIES,
}];

static BIG_MISSING_ARCHIVE_ENTRIES: [FileMapping; 1] = [FileMapping {
    from: "entry",
    to: "out.dat",
}];
static BIG_MISSING_ARCHIVE_ACTIONS: [InstallAction; 1] = [InstallAction::ExtractBig {
    source_big: "nonexistent.big",
    entries: &BIG_MISSING_ARCHIVE_ENTRIES,
}];

static BIG_TRAVERSAL_ENTRIES: [FileMapping; 1] = [FileMapping {
    from: "data\\config.ini",
    to: "../../escaped.txt",
}];
static BIG_TRAVERSAL_ACTIONS: [InstallAction; 1] = [InstallAction::ExtractBig {
    source_big: "test.big",
    entries: &BIG_TRAVERSAL_ENTRIES,
}];

/// Extracts named entries from a synthetic BIG archive.
///
/// BIG archives store full file paths with Windows-style backslash separators.
/// The executor must locate entries by case-insensitive name and write them
/// to the content root at the declared destination paths.
#[test]
fn execute_extract_big_action() {
    let tmp = std::env::temp_dir().join("cnc-exec-big");
    let _ = fs::remove_dir_all(&tmp);
    let src = tmp.join("source");
    let dst = tmp.join("content");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(&dst).unwrap();

    let big_data = build_big(&[
        ("data\\config.ini", b"[General]\nSpeed=5"),
        ("data\\terrain.tga", b"TGA-MOCK-DATA"),
    ]);
    fs::write(src.join("test.big"), &big_data).unwrap();

    execute_recipe(&make_recipe(&BIG_ACTIONS), &src, &dst, noop_progress).unwrap();

    assert_eq!(
        fs::read(dst.join("config.ini")).unwrap(),
        b"[General]\nSpeed=5"
    );
    assert_eq!(fs::read(dst.join("terrain.tga")).unwrap(), b"TGA-MOCK-DATA");

    let _ = fs::remove_dir_all(&tmp);
}

/// Returns an error when a referenced entry is not found in the BIG archive.
///
/// Missing entries must produce a clear diagnostic rather than silently
/// skipping, so the user knows which file was absent in the archive.
#[test]
fn extract_big_missing_entry_errors() {
    let tmp = std::env::temp_dir().join("cnc-exec-big-entry-missing");
    let _ = fs::remove_dir_all(&tmp);
    let src = tmp.join("source");
    let dst = tmp.join("content");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(&dst).unwrap();

    let big_data = build_big(&[("other.dat", b"data")]);
    fs::write(src.join("test.big"), &big_data).unwrap();

    let result = execute_recipe(
        &make_recipe(&BIG_MISSING_ENTRY_ACTIONS),
        &src,
        &dst,
        noop_progress,
    );
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("missing_entry"), "message was: {msg}");

    let _ = fs::remove_dir_all(&tmp);
}

/// Returns `SourceFileNotFound` when the BIG archive itself is absent.
///
/// A missing archive file must be reported before attempting any entry
/// extraction to avoid confusing I/O errors.
#[test]
fn extract_big_missing_archive_errors() {
    let tmp = std::env::temp_dir().join("cnc-exec-big-archive-missing");
    let _ = fs::remove_dir_all(&tmp);
    let src = tmp.join("source");
    let dst = tmp.join("content");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(&dst).unwrap();

    let result = execute_recipe(
        &make_recipe(&BIG_MISSING_ARCHIVE_ACTIONS),
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

/// Rejects path traversal in BIG extraction destinations.
///
/// Archive entry destinations are untrusted input. A `to` path containing
/// `../` must be blocked by the strict-path boundary to prevent writes
/// outside the content root.
#[test]
fn extract_big_rejects_traversal() {
    let tmp = std::env::temp_dir().join("cnc-exec-big-traversal");
    let _ = fs::remove_dir_all(&tmp);
    let src = tmp.join("source");
    let dst = tmp.join("content");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(&dst).unwrap();

    let big_data = build_big(&[("data\\config.ini", b"data")]);
    fs::write(src.join("test.big"), &big_data).unwrap();

    let result = execute_recipe(
        &make_recipe(&BIG_TRAVERSAL_ACTIONS),
        &src,
        &dst,
        noop_progress,
    );
    assert!(matches!(result, Err(ExecutorError::PathTraversal { .. })));
    assert!(!tmp.join("escaped.txt").exists());

    let _ = fs::remove_dir_all(&tmp);
}

// ── ExtractMeg action ────────────────────────────────────────────

static MEG_ENTRIES: [FileMapping; 2] = [
    FileMapping {
        from: "data/audio.aud",
        to: "audio.aud",
    },
    FileMapping {
        from: "data/video.bik",
        to: "video.bik",
    },
];
static MEG_ACTIONS: [InstallAction; 1] = [InstallAction::ExtractMeg {
    source_meg: "test.meg",
    entries: &MEG_ENTRIES,
}];

static MEG_MISSING_ENTRIES: [FileMapping; 1] = [FileMapping {
    from: "missing_entry",
    to: "out.dat",
}];
static MEG_MISSING_ENTRY_ACTIONS: [InstallAction; 1] = [InstallAction::ExtractMeg {
    source_meg: "test.meg",
    entries: &MEG_MISSING_ENTRIES,
}];

static MEG_MISSING_ARCHIVE_ENTRIES: [FileMapping; 1] = [FileMapping {
    from: "entry",
    to: "out.dat",
}];
static MEG_MISSING_ARCHIVE_ACTIONS: [InstallAction; 1] = [InstallAction::ExtractMeg {
    source_meg: "nonexistent.meg",
    entries: &MEG_MISSING_ARCHIVE_ENTRIES,
}];

static MEG_TRAVERSAL_ENTRIES: [FileMapping; 1] = [FileMapping {
    from: "data/audio.aud",
    to: "../../escaped.txt",
}];
static MEG_TRAVERSAL_ACTIONS: [InstallAction; 1] = [InstallAction::ExtractMeg {
    source_meg: "test.meg",
    entries: &MEG_TRAVERSAL_ENTRIES,
}];

/// Extracts named entries from a synthetic MEG archive.
///
/// MEG archives use a legacy format with length-prefixed filenames and
/// 20-byte file records. The executor must locate entries case-insensitively
/// and write them to the content root.
#[test]
fn execute_extract_meg_action() {
    let tmp = std::env::temp_dir().join("cnc-exec-meg");
    let _ = fs::remove_dir_all(&tmp);
    let src = tmp.join("source");
    let dst = tmp.join("content");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(&dst).unwrap();

    let meg_data = build_meg(&[
        ("data/audio.aud", b"AUD-CONTENT-123"),
        ("data/video.bik", b"BIK-MOCK"),
    ]);
    fs::write(src.join("test.meg"), &meg_data).unwrap();

    execute_recipe(&make_recipe(&MEG_ACTIONS), &src, &dst, noop_progress).unwrap();

    assert_eq!(fs::read(dst.join("audio.aud")).unwrap(), b"AUD-CONTENT-123");
    assert_eq!(fs::read(dst.join("video.bik")).unwrap(), b"BIK-MOCK");

    let _ = fs::remove_dir_all(&tmp);
}

/// Returns an error when a referenced entry is not found in the MEG archive.
///
/// Missing entries must produce a clear diagnostic identifying the absent
/// entry and the archive path, matching the pattern used for BIG and MIX.
#[test]
fn extract_meg_missing_entry_errors() {
    let tmp = std::env::temp_dir().join("cnc-exec-meg-entry-missing");
    let _ = fs::remove_dir_all(&tmp);
    let src = tmp.join("source");
    let dst = tmp.join("content");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(&dst).unwrap();

    let meg_data = build_meg(&[("other.dat", b"data")]);
    fs::write(src.join("test.meg"), &meg_data).unwrap();

    let result = execute_recipe(
        &make_recipe(&MEG_MISSING_ENTRY_ACTIONS),
        &src,
        &dst,
        noop_progress,
    );
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("missing_entry"), "message was: {msg}");

    let _ = fs::remove_dir_all(&tmp);
}

/// Returns `SourceFileNotFound` when the MEG archive itself is absent.
///
/// Analogous to the BIG and MIX missing-archive tests: the executor must
/// detect the missing file before any entry lookup.
#[test]
fn extract_meg_missing_archive_errors() {
    let tmp = std::env::temp_dir().join("cnc-exec-meg-archive-missing");
    let _ = fs::remove_dir_all(&tmp);
    let src = tmp.join("source");
    let dst = tmp.join("content");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(&dst).unwrap();

    let result = execute_recipe(
        &make_recipe(&MEG_MISSING_ARCHIVE_ACTIONS),
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

/// Rejects path traversal in MEG extraction destinations.
///
/// Even though MEG filenames are typically well-formed, the `to` path is
/// untrusted recipe data. Traversal attempts must be blocked by the
/// strict-path boundary.
#[test]
fn extract_meg_rejects_traversal() {
    let tmp = std::env::temp_dir().join("cnc-exec-meg-traversal");
    let _ = fs::remove_dir_all(&tmp);
    let src = tmp.join("source");
    let dst = tmp.join("content");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(&dst).unwrap();

    let meg_data = build_meg(&[("data/audio.aud", b"data")]);
    fs::write(src.join("test.meg"), &meg_data).unwrap();

    let result = execute_recipe(
        &make_recipe(&MEG_TRAVERSAL_ACTIONS),
        &src,
        &dst,
        noop_progress,
    );
    assert!(matches!(result, Err(ExecutorError::PathTraversal { .. })));
    assert!(!tmp.join("escaped.txt").exists());

    let _ = fs::remove_dir_all(&tmp);
}

// ── ExtractBagIdx action ─────────────────────────────────────────

static BAGIDX_ENTRIES: [FileMapping; 2] = [
    FileMapping {
        from: "alert.wav",
        to: "audio/alert.wav",
    },
    FileMapping {
        from: "bomb.wav",
        to: "audio/bomb.wav",
    },
];
static BAGIDX_ACTIONS: [InstallAction; 1] = [InstallAction::ExtractBagIdx {
    source_idx: "audio.idx",
    source_bag: "audio.bag",
    entries: &BAGIDX_ENTRIES,
}];

static BAGIDX_MISSING_ENTRIES: [FileMapping; 1] = [FileMapping {
    from: "missing.wav",
    to: "out.wav",
}];
static BAGIDX_MISSING_ENTRY_ACTIONS: [InstallAction; 1] = [InstallAction::ExtractBagIdx {
    source_idx: "audio.idx",
    source_bag: "audio.bag",
    entries: &BAGIDX_MISSING_ENTRIES,
}];

static BAGIDX_TRAVERSAL_ENTRIES: [FileMapping; 1] = [FileMapping {
    from: "alert.wav",
    to: "../../escaped.wav",
}];
static BAGIDX_TRAVERSAL_ACTIONS: [InstallAction; 1] = [InstallAction::ExtractBagIdx {
    source_idx: "audio.idx",
    source_bag: "audio.bag",
    entries: &BAGIDX_TRAVERSAL_ENTRIES,
}];

/// Extracts audio entries from a synthetic BAG/IDX pair.
///
/// The IDX file provides a flat array of 36-byte entries mapping names to
/// offsets within the BAG file. The executor must parse the IDX, seek to
/// the correct BAG offset, and write the data to the content root.
#[test]
fn execute_extract_bag_idx_action() {
    let tmp = std::env::temp_dir().join("cnc-exec-bagidx");
    let _ = fs::remove_dir_all(&tmp);
    let src = tmp.join("source");
    let dst = tmp.join("content");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(&dst).unwrap();

    let alert_data = b"RIFF-ALERT-WAV";
    let bomb_data = b"RIFF-BOMB-WAV-DATA";

    // IDX entries: (name, offset, size)
    let idx = build_idx(&[
        ("alert.wav", 0, alert_data.len() as u32),
        ("bomb.wav", alert_data.len() as u32, bomb_data.len() as u32),
    ]);

    // BAG: concatenated audio data at the declared offsets.
    let mut bag = Vec::new();
    bag.extend_from_slice(alert_data);
    bag.extend_from_slice(bomb_data);

    fs::write(src.join("audio.idx"), &idx).unwrap();
    fs::write(src.join("audio.bag"), &bag).unwrap();

    execute_recipe(&make_recipe(&BAGIDX_ACTIONS), &src, &dst, noop_progress).unwrap();

    assert_eq!(
        fs::read(dst.join("audio/alert.wav")).unwrap(),
        alert_data.as_slice()
    );
    assert_eq!(
        fs::read(dst.join("audio/bomb.wav")).unwrap(),
        bomb_data.as_slice()
    );

    let _ = fs::remove_dir_all(&tmp);
}

/// Returns an error when a referenced entry is not found in the IDX index.
///
/// BAG/IDX entry lookup is by name. If the requested name is absent, the
/// executor must report which entry was missing and in which index file.
#[test]
fn extract_bag_idx_missing_entry_errors() {
    let tmp = std::env::temp_dir().join("cnc-exec-bagidx-entry-missing");
    let _ = fs::remove_dir_all(&tmp);
    let src = tmp.join("source");
    let dst = tmp.join("content");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(&dst).unwrap();

    let idx = build_idx(&[("other.wav", 0, 4)]);
    let bag = vec![0u8; 4];
    fs::write(src.join("audio.idx"), &idx).unwrap();
    fs::write(src.join("audio.bag"), &bag).unwrap();

    let result = execute_recipe(
        &make_recipe(&BAGIDX_MISSING_ENTRY_ACTIONS),
        &src,
        &dst,
        noop_progress,
    );
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("missing.wav"), "message was: {msg}");

    let _ = fs::remove_dir_all(&tmp);
}

/// Rejects path traversal in BAG/IDX extraction destinations.
///
/// The `to` path in the entry mapping is untrusted. Traversal attempts
/// must hit the strict-path boundary before any data is written.
#[test]
fn extract_bag_idx_rejects_traversal() {
    let tmp = std::env::temp_dir().join("cnc-exec-bagidx-traversal");
    let _ = fs::remove_dir_all(&tmp);
    let src = tmp.join("source");
    let dst = tmp.join("content");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(&dst).unwrap();

    let idx = build_idx(&[("alert.wav", 0, 4)]);
    let bag = vec![0u8; 4];
    fs::write(src.join("audio.idx"), &idx).unwrap();
    fs::write(src.join("audio.bag"), &bag).unwrap();

    let result = execute_recipe(
        &make_recipe(&BAGIDX_TRAVERSAL_ACTIONS),
        &src,
        &dst,
        noop_progress,
    );
    assert!(matches!(result, Err(ExecutorError::PathTraversal { .. })));
    assert!(!tmp.join("escaped.wav").exists());

    let _ = fs::remove_dir_all(&tmp);
}

// ── describe_action output ───────────────────────────────────────

/// `describe_action` returns distinct, informative strings for all 10 action types.
///
/// These strings are displayed in progress bars and logs. Each description
/// must identify the action type and include the archive name or file count
/// so the user can follow installation progress.
#[test]
fn describe_action_covers_all_variants() {
    // Copy
    let desc = describe_action(&InstallAction::Copy { files: &COPY_FILES });
    assert!(desc.contains("Copying"), "Copy: {desc}");
    assert!(desc.contains("2"), "Copy count: {desc}");

    // ExtractMix
    let desc = describe_action(&InstallAction::ExtractMix {
        source_mix: "main.mix",
        entries: &MIX_ENTRIES,
    });
    assert!(desc.contains("main.mix"), "MIX name: {desc}");
    assert!(desc.contains("2"), "MIX count: {desc}");

    // ExtractMixFromContent
    let desc = describe_action(&InstallAction::ExtractMixFromContent {
        content_mix: "intermediate.mix",
        entries: &CONTENT_MIX_ENTRIES,
    });
    assert!(desc.contains("intermediate.mix"), "ContentMIX: {desc}");
    assert!(desc.contains("content"), "ContentMIX context: {desc}");

    // ExtractIscab
    static ISCAB_MAP: [FileMapping; 1] = [FileMapping { from: "f", to: "f" }];
    let desc = describe_action(&InstallAction::ExtractIscab {
        header: "data1.hdr",
        volumes: &[],
        entries: &ISCAB_MAP,
    });
    assert!(desc.contains("data1.hdr"), "ISCAB header: {desc}");
    assert!(desc.contains("InstallShield"), "ISCAB type: {desc}");

    // ExtractRaw
    let desc = describe_action(&InstallAction::ExtractRaw {
        entries: &RAW_ENTRIES,
    });
    assert!(desc.contains("raw"), "Raw: {desc}");
    assert!(desc.contains("1"), "Raw count: {desc}");

    // ExtractZip
    static ZIP_MAP: [FileMapping; 2] = [
        FileMapping { from: "a", to: "a" },
        FileMapping { from: "b", to: "b" },
    ];
    let desc = describe_action(&InstallAction::ExtractZip { entries: &ZIP_MAP });
    assert!(desc.contains("ZIP"), "ZIP: {desc}");
    assert!(desc.contains("2"), "ZIP count: {desc}");

    // Delete
    let desc = describe_action(&InstallAction::Delete { path: "temp.mix" });
    assert!(desc.contains("temp.mix"), "Delete path: {desc}");

    // ExtractBig
    let desc = describe_action(&InstallAction::ExtractBig {
        source_big: "INI.big",
        entries: &BIG_ENTRIES,
    });
    assert!(desc.contains("BIG"), "BIG type: {desc}");
    assert!(desc.contains("INI.big"), "BIG name: {desc}");

    // ExtractMeg
    let desc = describe_action(&InstallAction::ExtractMeg {
        source_meg: "data.meg",
        entries: &MEG_ENTRIES,
    });
    assert!(desc.contains("MEG"), "MEG type: {desc}");
    assert!(desc.contains("data.meg"), "MEG name: {desc}");

    // ExtractBagIdx
    let desc = describe_action(&InstallAction::ExtractBagIdx {
        source_idx: "audio.idx",
        source_bag: "audio.bag",
        entries: &BAGIDX_ENTRIES,
    });
    assert!(desc.contains("BAG/IDX"), "BAG/IDX type: {desc}");
    assert!(desc.contains("audio.idx"), "BAG/IDX name: {desc}");

    // ExtractIso
    static ISO_MAP: [FileMapping; 1] = [FileMapping {
        from: "README.TXT",
        to: "readme.txt",
    }];
    let desc = describe_action(&InstallAction::ExtractIso {
        source_iso: "game.iso",
        entries: &ISO_MAP,
    });
    assert!(desc.contains("ISO"), "ISO type: {desc}");
    assert!(desc.contains("game.iso"), "ISO name: {desc}");

    // ExtractMixFromIso
    let desc = describe_action(&InstallAction::ExtractMixFromIso {
        source_iso: "disc.iso",
        iso_mix_path: "INSTALL/MAIN.MIX",
        entries: &ISO_MAP,
    });
    assert!(
        desc.contains("INSTALL/MAIN.MIX"),
        "MixFromIso MIX path: {desc}"
    );
    assert!(desc.contains("disc.iso"), "MixFromIso ISO name: {desc}");
}

// ── Helper: build a minimal ISO 9660 image from name/data pairs ──

/// ISO 9660 sector size used by the synthetic builder.
const ISO_SECTOR_SIZE: usize = 2048;

/// ISO 9660 directory flag bit.
const ISO_FLAG_DIRECTORY: u8 = 0x02;

/// Builds a minimal valid ISO 9660 image with the given files.
///
/// Each file is specified as `(path, data)` where `path` uses forward
/// slashes. Single-level paths go into the root directory; paths with
/// slashes create the necessary subdirectories. All filenames are stored
/// as uppercase ASCII with ";1" version suffixes, matching real ISO 9660
/// Level 1 images.
fn build_iso(files: &[(&str, &[u8])]) -> Vec<u8> {
    // ── Plan layout ─────────────────────────────────────────────────────

    let mut dirs: Vec<String> = vec![String::new()]; // root = ""
    for (path, _) in files {
        let parts: Vec<&str> = path.split('/').collect();
        for i in 1..parts.len() {
            let dir = parts[..i].join("/");
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
    }

    struct FileEntry<'a> {
        name: &'a str,
        parent: String,
        data: &'a [u8],
        sector: u32,
    }
    let mut file_entries: Vec<FileEntry> = Vec::new();
    for (path, data) in files {
        let (parent, name) = match path.rfind('/') {
            Some(pos) => (path[..pos].to_string(), &path[pos + 1..]),
            None => (String::new(), *path),
        };
        file_entries.push(FileEntry {
            name,
            parent,
            data,
            sector: 0,
        });
    }

    // ── Assign sectors ──────────────────────────────────────────────────

    let mut next_sector: u32 = 18;

    struct DirLayout {
        path: String,
        sector: u32,
        extent_size: u32,
    }
    let mut dir_layouts: Vec<DirLayout> = Vec::new();
    for dir in &dirs {
        dir_layouts.push(DirLayout {
            path: dir.clone(),
            sector: next_sector,
            extent_size: 0,
        });
        next_sector += 1;
    }

    for entry in &mut file_entries {
        entry.sector = next_sector;
        let sectors_needed = entry.data.len().div_ceil(ISO_SECTOR_SIZE).max(1) as u32;
        next_sector += sectors_needed;
    }

    let total_sectors = next_sector;
    let image_size = total_sectors as usize * ISO_SECTOR_SIZE;
    let mut image = vec![0u8; image_size];

    // ── Build directory records ──────────────────────────────────────────

    for dir_idx in 0..dir_layouts.len() {
        let dir_path = dir_layouts[dir_idx].path.clone();
        let dir_sector = dir_layouts[dir_idx].sector;
        let mut records = Vec::new();

        // "." record
        records.extend_from_slice(&build_iso_dir_record(
            dir_sector,
            ISO_SECTOR_SIZE as u32,
            ISO_FLAG_DIRECTORY,
            &[0x00],
        ));

        // ".." record
        let parent_sector = if dir_path.is_empty() {
            dir_sector
        } else {
            let parent_path = match dir_path.rfind('/') {
                Some(pos) => &dir_path[..pos],
                None => "",
            };
            dir_layouts
                .iter()
                .find(|d| d.path == parent_path)
                .map_or(dir_sector, |d| d.sector)
        };
        records.extend_from_slice(&build_iso_dir_record(
            parent_sector,
            ISO_SECTOR_SIZE as u32,
            ISO_FLAG_DIRECTORY,
            &[0x01],
        ));

        // Subdirectory entries
        for sub_dir in &dir_layouts {
            if sub_dir.path.is_empty() {
                continue;
            }
            let sub_parent = match sub_dir.path.rfind('/') {
                Some(pos) => &sub_dir.path[..pos],
                None => "",
            };
            if sub_parent == dir_path {
                let leaf = match sub_dir.path.rfind('/') {
                    Some(pos) => &sub_dir.path[pos + 1..],
                    None => &sub_dir.path,
                };
                let name_upper = leaf.to_ascii_uppercase();
                records.extend_from_slice(&build_iso_dir_record(
                    sub_dir.sector,
                    ISO_SECTOR_SIZE as u32,
                    ISO_FLAG_DIRECTORY,
                    name_upper.as_bytes(),
                ));
            }
        }

        // File entries
        for entry in &file_entries {
            if entry.parent == dir_path {
                let name_with_version = format!("{};1", entry.name.to_ascii_uppercase());
                records.extend_from_slice(&build_iso_dir_record(
                    entry.sector,
                    entry.data.len() as u32,
                    0,
                    name_with_version.as_bytes(),
                ));
            }
        }

        let extent_size = records.len();
        let dest_offset = dir_sector as usize * ISO_SECTOR_SIZE;
        image[dest_offset..dest_offset + extent_size].copy_from_slice(&records);

        // Patch "." self-reference data length.
        let len_bytes = (extent_size as u32).to_le_bytes();
        image[dest_offset + 10..dest_offset + 14].copy_from_slice(&len_bytes);

        dir_layouts[dir_idx].extent_size = extent_size as u32;
    }

    // ── Write file data ─────────────────────────────────────────────────

    for entry in &file_entries {
        let offset = entry.sector as usize * ISO_SECTOR_SIZE;
        image[offset..offset + entry.data.len()].copy_from_slice(entry.data);
    }

    // ── Write PVD (sector 16) ───────────────────────────────────────────

    let pvd_offset = 16 * ISO_SECTOR_SIZE;
    image[pvd_offset] = 1;
    image[pvd_offset + 1..pvd_offset + 6].copy_from_slice(b"CD001");
    image[pvd_offset + 6] = 1;

    image[pvd_offset + 80..pvd_offset + 84].copy_from_slice(&total_sectors.to_le_bytes());
    image[pvd_offset + 84..pvd_offset + 88].copy_from_slice(&total_sectors.to_be_bytes());

    image[pvd_offset + 128..pvd_offset + 130]
        .copy_from_slice(&(ISO_SECTOR_SIZE as u16).to_le_bytes());
    image[pvd_offset + 130..pvd_offset + 132]
        .copy_from_slice(&(ISO_SECTOR_SIZE as u16).to_be_bytes());

    let root_sector = dir_layouts[0].sector;
    let root_extent_size = dir_layouts[0].extent_size;
    let root_record =
        build_iso_dir_record(root_sector, root_extent_size, ISO_FLAG_DIRECTORY, &[0x00]);
    image[pvd_offset + 156..pvd_offset + 156 + root_record.len()].copy_from_slice(&root_record);

    // ── Write VD Set Terminator (sector 17) ─────────────────────────────

    let term_offset = 17 * ISO_SECTOR_SIZE;
    image[term_offset] = 255;
    image[term_offset + 1..term_offset + 6].copy_from_slice(b"CD001");
    image[term_offset + 6] = 1;

    image
}

/// Builds a single ISO 9660 directory record.
fn build_iso_dir_record(
    extent_lba: u32,
    data_length: u32,
    flags: u8,
    identifier: &[u8],
) -> Vec<u8> {
    let id_len = identifier.len();
    let base_len = 33 + id_len;
    let record_len = if base_len.is_multiple_of(2) {
        base_len
    } else {
        base_len + 1
    };

    let mut rec = vec![0u8; record_len];
    rec[0] = record_len as u8;
    rec[2..6].copy_from_slice(&extent_lba.to_le_bytes());
    rec[6..10].copy_from_slice(&extent_lba.to_be_bytes());
    rec[10..14].copy_from_slice(&data_length.to_le_bytes());
    rec[14..18].copy_from_slice(&data_length.to_be_bytes());
    rec[25] = flags;
    rec[32] = id_len as u8;
    rec[33..33 + id_len].copy_from_slice(identifier);
    rec
}

// ── ExtractIso action ────────────────────────────────────────────

static ISO_ENTRIES: [FileMapping; 2] = [
    FileMapping {
        from: "README.TXT",
        to: "readme.txt",
    },
    FileMapping {
        from: "DATA.BIN",
        to: "subdir/data.bin",
    },
];
static ISO_ACTIONS: [InstallAction; 1] = [InstallAction::ExtractIso {
    source_iso: "game.iso",
    entries: &ISO_ENTRIES,
}];

static ISO_MISSING_ENTRIES: [FileMapping; 1] = [FileMapping {
    from: "NONEXISTENT.TXT",
    to: "out.txt",
}];
static ISO_MISSING_ENTRY_ACTIONS: [InstallAction; 1] = [InstallAction::ExtractIso {
    source_iso: "game.iso",
    entries: &ISO_MISSING_ENTRIES,
}];

static ISO_MISSING_ARCHIVE_ENTRIES: [FileMapping; 1] = [FileMapping {
    from: "README.TXT",
    to: "out.txt",
}];
static ISO_MISSING_ARCHIVE_ACTIONS: [InstallAction; 1] = [InstallAction::ExtractIso {
    source_iso: "nonexistent.iso",
    entries: &ISO_MISSING_ARCHIVE_ENTRIES,
}];

static ISO_TRAVERSAL_ENTRIES: [FileMapping; 1] = [FileMapping {
    from: "README.TXT",
    to: "../../escaped.txt",
}];
static ISO_TRAVERSAL_ACTIONS: [InstallAction; 1] = [InstallAction::ExtractIso {
    source_iso: "game.iso",
    entries: &ISO_TRAVERSAL_ENTRIES,
}];

/// Extracts named files from a synthetic ISO 9660 disc image.
///
/// The executor must open the ISO, locate each file by name within the
/// ISO's directory structure, and write the data to the content root at
/// the declared destination paths.
#[test]
fn execute_extract_iso_action() {
    let tmp = std::env::temp_dir().join("cnc-exec-iso");
    let _ = fs::remove_dir_all(&tmp);
    let src = tmp.join("source");
    let dst = tmp.join("content");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(&dst).unwrap();

    let iso_data = build_iso(&[
        ("README.TXT", b"Hello from ISO"),
        ("DATA.BIN", b"\x00\x01\x02\x03"),
    ]);
    fs::write(src.join("game.iso"), &iso_data).unwrap();

    execute_recipe(&make_recipe(&ISO_ACTIONS), &src, &dst, noop_progress).unwrap();

    assert_eq!(fs::read(dst.join("readme.txt")).unwrap(), b"Hello from ISO");
    assert_eq!(
        fs::read(dst.join("subdir/data.bin")).unwrap(),
        b"\x00\x01\x02\x03"
    );

    let _ = fs::remove_dir_all(&tmp);
}

/// Returns `IsoEntryNotFound` when a referenced file is absent in the ISO.
///
/// Missing entries must produce a clear diagnostic identifying the absent
/// entry and the ISO filename so users can troubleshoot.
#[test]
fn extract_iso_missing_entry_errors() {
    let tmp = std::env::temp_dir().join("cnc-exec-iso-entry-missing");
    let _ = fs::remove_dir_all(&tmp);
    let src = tmp.join("source");
    let dst = tmp.join("content");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(&dst).unwrap();

    let iso_data = build_iso(&[("OTHER.TXT", b"other")]);
    fs::write(src.join("game.iso"), &iso_data).unwrap();

    let result = execute_recipe(
        &make_recipe(&ISO_MISSING_ENTRY_ACTIONS),
        &src,
        &dst,
        noop_progress,
    );
    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        ExecutorError::IsoEntryNotFound { .. }
    ));

    let _ = fs::remove_dir_all(&tmp);
}

/// Returns `IsoNotFound` when the ISO file itself is absent.
///
/// A missing ISO file must be reported as `IsoNotFound` (not a generic
/// I/O error) so callers can present actionable diagnostics.
#[test]
fn extract_iso_missing_archive_errors() {
    let tmp = std::env::temp_dir().join("cnc-exec-iso-archive-missing");
    let _ = fs::remove_dir_all(&tmp);
    let src = tmp.join("source");
    let dst = tmp.join("content");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(&dst).unwrap();

    let result = execute_recipe(
        &make_recipe(&ISO_MISSING_ARCHIVE_ACTIONS),
        &src,
        &dst,
        noop_progress,
    );
    assert!(matches!(result, Err(ExecutorError::IsoNotFound { .. })));

    let _ = fs::remove_dir_all(&tmp);
}

/// Rejects path traversal in ISO extraction destinations.
///
/// Archive entry destinations are untrusted input. A `to` path containing
/// `../` must be blocked by the strict-path boundary to prevent writes
/// outside the content root.
#[test]
fn extract_iso_rejects_traversal() {
    let tmp = std::env::temp_dir().join("cnc-exec-iso-traversal");
    let _ = fs::remove_dir_all(&tmp);
    let src = tmp.join("source");
    let dst = tmp.join("content");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(&dst).unwrap();

    let iso_data = build_iso(&[("README.TXT", b"data")]);
    fs::write(src.join("game.iso"), &iso_data).unwrap();

    let result = execute_recipe(
        &make_recipe(&ISO_TRAVERSAL_ACTIONS),
        &src,
        &dst,
        noop_progress,
    );
    assert!(matches!(result, Err(ExecutorError::PathTraversal { .. })));
    assert!(!tmp.join("escaped.txt").exists());

    let _ = fs::remove_dir_all(&tmp);
}

// ── ExtractMixFromIso action ─────────────────────────────────────

static MIX_FROM_ISO_ENTRIES: [FileMapping; 2] = [
    FileMapping {
        from: "allies.mix",
        to: "allies.mix",
    },
    FileMapping {
        from: "conquer.mix",
        to: "conquer.mix",
    },
];
static MIX_FROM_ISO_ACTIONS: [InstallAction; 1] = [InstallAction::ExtractMixFromIso {
    source_iso: "disc.iso",
    iso_mix_path: "INSTALL/MAIN.MIX",
    entries: &MIX_FROM_ISO_ENTRIES,
}];

static MIX_FROM_ISO_MISSING_MIX_ENTRIES: [FileMapping; 1] = [FileMapping {
    from: "allies.mix",
    to: "allies.mix",
}];
static MIX_FROM_ISO_MISSING_MIX_ACTIONS: [InstallAction; 1] = [InstallAction::ExtractMixFromIso {
    source_iso: "disc.iso",
    iso_mix_path: "INSTALL/NONEXISTENT.MIX",
    entries: &MIX_FROM_ISO_MISSING_MIX_ENTRIES,
}];

static MIX_FROM_ISO_MISSING_ENTRY_ENTRIES: [FileMapping; 1] = [FileMapping {
    from: "nonexistent.mix",
    to: "out.mix",
}];
static MIX_FROM_ISO_MISSING_ENTRY_ACTIONS: [InstallAction; 1] =
    [InstallAction::ExtractMixFromIso {
        source_iso: "disc.iso",
        iso_mix_path: "INSTALL/MAIN.MIX",
        entries: &MIX_FROM_ISO_MISSING_ENTRY_ENTRIES,
    }];

static MIX_FROM_ISO_MISSING_ISO_ENTRIES: [FileMapping; 1] = [FileMapping {
    from: "allies.mix",
    to: "allies.mix",
}];
static MIX_FROM_ISO_MISSING_ISO_ACTIONS: [InstallAction; 1] = [InstallAction::ExtractMixFromIso {
    source_iso: "nonexistent.iso",
    iso_mix_path: "INSTALL/MAIN.MIX",
    entries: &MIX_FROM_ISO_MISSING_ISO_ENTRIES,
}];

static MIX_FROM_ISO_TRAVERSAL_ENTRIES: [FileMapping; 1] = [FileMapping {
    from: "allies.mix",
    to: "../../escaped.mix",
}];
static MIX_FROM_ISO_TRAVERSAL_ACTIONS: [InstallAction; 1] = [InstallAction::ExtractMixFromIso {
    source_iso: "disc.iso",
    iso_mix_path: "INSTALL/MAIN.MIX",
    entries: &MIX_FROM_ISO_TRAVERSAL_ENTRIES,
}];

/// Extracts MIX entries from a MIX archive nested inside a synthetic ISO.
///
/// This tests the two-level extraction chain: ISO disc image → MIX archive
/// inside the ISO → individual entries from the MIX. The MIX data is read
/// directly from the ISO via a bounded entry reader — no intermediate file
/// is extracted to disk.
#[test]
fn execute_extract_mix_from_iso_action() {
    let tmp = std::env::temp_dir().join("cnc-exec-mix-from-iso");
    let _ = fs::remove_dir_all(&tmp);
    let src = tmp.join("source");
    let dst = tmp.join("content");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(&dst).unwrap();

    // Build a MIX archive containing two entries.
    let mix_data = build_mix(&[
        ("allies.mix", b"allies-from-iso"),
        ("conquer.mix", b"conquer-from-iso"),
    ]);

    // Embed the MIX archive inside a synthetic ISO at INSTALL/MAIN.MIX.
    let iso_data = build_iso(&[("INSTALL/MAIN.MIX", &mix_data)]);
    fs::write(src.join("disc.iso"), &iso_data).unwrap();

    execute_recipe(
        &make_recipe(&MIX_FROM_ISO_ACTIONS),
        &src,
        &dst,
        noop_progress,
    )
    .unwrap();

    assert_eq!(
        fs::read(dst.join("allies.mix")).unwrap(),
        b"allies-from-iso"
    );
    assert_eq!(
        fs::read(dst.join("conquer.mix")).unwrap(),
        b"conquer-from-iso"
    );

    let _ = fs::remove_dir_all(&tmp);
}

/// Returns `IsoEntryNotFound` when the MIX path inside the ISO is absent.
///
/// If the MIX archive referenced by `iso_mix_path` does not exist in the
/// ISO's filesystem, the executor must report it as an ISO entry lookup
/// failure.
#[test]
fn extract_mix_from_iso_missing_mix_errors() {
    let tmp = std::env::temp_dir().join("cnc-exec-mix-from-iso-missing-mix");
    let _ = fs::remove_dir_all(&tmp);
    let src = tmp.join("source");
    let dst = tmp.join("content");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(&dst).unwrap();

    // ISO that contains a file but not the expected MIX path.
    let iso_data = build_iso(&[("OTHER.TXT", b"other")]);
    fs::write(src.join("disc.iso"), &iso_data).unwrap();

    let result = execute_recipe(
        &make_recipe(&MIX_FROM_ISO_MISSING_MIX_ACTIONS),
        &src,
        &dst,
        noop_progress,
    );
    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        ExecutorError::IsoEntryNotFound { .. }
    ));

    let _ = fs::remove_dir_all(&tmp);
}

/// Returns `MixEntryNotFound` when a MIX entry is absent inside the ISO's MIX.
///
/// When the MIX archive is found inside the ISO but the requested entry
/// does not exist within the MIX, the error must identify both the archive
/// chain and the missing entry name.
#[test]
fn extract_mix_from_iso_missing_entry_errors() {
    let tmp = std::env::temp_dir().join("cnc-exec-mix-from-iso-missing-entry");
    let _ = fs::remove_dir_all(&tmp);
    let src = tmp.join("source");
    let dst = tmp.join("content");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(&dst).unwrap();

    let mix_data = build_mix(&[("other.dat", b"data")]);
    let iso_data = build_iso(&[("INSTALL/MAIN.MIX", &mix_data)]);
    fs::write(src.join("disc.iso"), &iso_data).unwrap();

    let result = execute_recipe(
        &make_recipe(&MIX_FROM_ISO_MISSING_ENTRY_ACTIONS),
        &src,
        &dst,
        noop_progress,
    );
    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        ExecutorError::MixEntryNotFound { .. }
    ));

    let _ = fs::remove_dir_all(&tmp);
}

/// Returns `IsoNotFound` when the ISO file itself is absent.
///
/// Analogous to `MixNotFound` — the missing-file condition must be caught
/// before any ISO parsing is attempted.
#[test]
fn extract_mix_from_iso_missing_iso_errors() {
    let tmp = std::env::temp_dir().join("cnc-exec-mix-from-iso-missing-iso");
    let _ = fs::remove_dir_all(&tmp);
    let src = tmp.join("source");
    let dst = tmp.join("content");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(&dst).unwrap();

    let result = execute_recipe(
        &make_recipe(&MIX_FROM_ISO_MISSING_ISO_ACTIONS),
        &src,
        &dst,
        noop_progress,
    );
    assert!(matches!(result, Err(ExecutorError::IsoNotFound { .. })));

    let _ = fs::remove_dir_all(&tmp);
}

/// Rejects path traversal in MIX-from-ISO extraction destinations.
///
/// The `to` paths in MIX entries nested inside an ISO are untrusted recipe
/// data. Traversal attempts must be blocked by the content-root boundary.
#[test]
fn extract_mix_from_iso_rejects_traversal() {
    let tmp = std::env::temp_dir().join("cnc-exec-mix-from-iso-traversal");
    let _ = fs::remove_dir_all(&tmp);
    let src = tmp.join("source");
    let dst = tmp.join("content");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(&dst).unwrap();

    let mix_data = build_mix(&[("allies.mix", b"data")]);
    let iso_data = build_iso(&[("INSTALL/MAIN.MIX", &mix_data)]);
    fs::write(src.join("disc.iso"), &iso_data).unwrap();

    let result = execute_recipe(
        &make_recipe(&MIX_FROM_ISO_TRAVERSAL_ACTIONS),
        &src,
        &dst,
        noop_progress,
    );
    assert!(matches!(result, Err(ExecutorError::PathTraversal { .. })));
    assert!(!tmp.join("escaped.mix").exists());

    let _ = fs::remove_dir_all(&tmp);
}

// ── ISO error Display messages ──────────────────────────────────

/// Display impl for IsoNotFound includes the archive path.
///
/// Missing-ISO errors must identify which file was absent for diagnostic
/// purposes, matching the pattern of MixNotFound.
#[test]
fn executor_error_display_iso_not_found() {
    let err = ExecutorError::IsoNotFound {
        path: PathBuf::from("source/game.iso"),
    };
    let msg = err.to_string();
    assert!(msg.contains("source/game.iso"), "message was: {msg}");
}

/// Display impl for IsoEntryNotFound includes both archive and entry names.
///
/// When a specific file is missing from an ISO the message must name both
/// the ISO and the entry for actionable diagnostics.
#[test]
fn executor_error_display_iso_entry_not_found() {
    let err = ExecutorError::IsoEntryNotFound {
        archive: "disc.iso".to_string(),
        entry: "INSTALL/MAIN.MIX".to_string(),
    };
    let msg = err.to_string();
    assert!(msg.contains("disc.iso"), "message was: {msg}");
    assert!(msg.contains("INSTALL/MAIN.MIX"), "message was: {msg}");
}
