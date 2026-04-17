//! ISO 9660 tests: ExtractIso, ExtractMixFromIso, and ISO error Display messages.

use super::*;

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
