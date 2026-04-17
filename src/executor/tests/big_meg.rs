//! Archive action tests: ExtractBig and ExtractMeg.

use super::*;

// ── Helper: build a minimal BIGF archive from name/data pairs ────

/// Builds a synthetic BIG archive (BIGF variant) containing the given files.
///
/// Format: 4-byte magic "BIGF", u32 LE archive_size, u32 BE entry_count,
/// u32 BE first_data_offset, then per-entry u32 BE offset + u32 BE size +
/// NUL-terminated name, followed by raw file data.
pub(super) fn build_big(files: &[(&str, &[u8])]) -> Vec<u8> {
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
pub(super) fn build_meg(files: &[(&str, &[u8])]) -> Vec<u8> {
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
