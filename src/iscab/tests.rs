use super::*;

// ── Helper: build a minimal synthetic v5 InstallShield archive ───

/// Builds a synthetic data1.hdr + data1.cab pair for testing.
/// Contains a single uncompressed file and a single zlib-compressed file.
fn build_test_archive(dir: &Path) -> (std::path::PathBuf, Vec<(u32, std::path::PathBuf)>) {
    let hdr_path = dir.join("data1.hdr");
    let cab_path = dir.join("data1.cab");

    // File data for the cab volume.
    let file1_data = b"HELLO WORLD";
    let file2_raw = b"COMPRESSED DATA HERE";
    let file2_compressed = zlib_compress(file2_raw);

    // Build cab volume: file1 at offset 0, file2 at offset file1_data.len().
    let mut cab = Vec::new();
    cab.extend_from_slice(file1_data);
    cab.extend_from_slice(&file2_compressed);
    std::fs::write(&cab_path, &cab).unwrap();

    // Build header.
    let mut hdr = Vec::new();

    // ── Main header (20 bytes) ───────────────────────────────────
    hdr.extend_from_slice(&SIGNATURE.to_le_bytes()); // 0x00: signature
    hdr.extend_from_slice(&0x0000_5000u32.to_le_bytes()); // 0x04: version (major=5)
    hdr.extend_from_slice(&0u32.to_le_bytes()); // 0x08: volume_info
    let cab_desc_offset: u32 = 20; // immediately after header
    hdr.extend_from_slice(&cab_desc_offset.to_le_bytes()); // 0x0C
    hdr.extend_from_slice(&0u32.to_le_bytes()); // 0x10: cab_desc_size (unused)

    // ── Cab descriptor (at offset 20) ────────────────────────────
    // We'll place the file table right after the cab descriptor.
    // Cab descriptor is 0x24 bytes (9 u32 fields).
    let file_table_rel: u32 = 0x24; // relative to cab_desc_offset
    let directory_count: u32 = 1;
    let file_count: u32 = 2;

    // Directory table: 1 directory entry (u32 offset to name string).
    // File descriptors start after directory table.
    // Layout of file table area:
    //   [dir_ptr_0: u32] [fd0: 0x33 bytes] [fd1: 0x33 bytes] [strings...]

    let dir_ptrs_size = (directory_count as usize) * 4;
    let fds_size = (file_count as usize) * FD_SIZE_V5;
    let strings_start = dir_ptrs_size + fds_size;

    // String table:
    let dir_name = b"INSTALL\0";
    let file1_name = b"TEST.MIX\0";
    let file2_name = b"DATA.PAK\0";

    let dir_name_offset = strings_start;
    let file1_name_offset = dir_name_offset + dir_name.len();
    let file2_name_offset = file1_name_offset + file1_name.len();

    // File table offset2 = relative offset to file descriptors from cab_desc.
    let file_table_offset2_rel = file_table_rel + dir_ptrs_size as u32;

    // Cab descriptor fields.
    hdr.extend_from_slice(&file_table_rel.to_le_bytes()); // 0x00: file_table_offset
    hdr.extend_from_slice(&0u32.to_le_bytes()); // 0x04: unknown
    hdr.extend_from_slice(&0u32.to_le_bytes()); // 0x08: file_table_size
    hdr.extend_from_slice(&0u32.to_le_bytes()); // 0x0C: file_table_size2
    hdr.extend_from_slice(&directory_count.to_le_bytes()); // 0x10: directory_count
    hdr.extend_from_slice(&0u32.to_le_bytes()); // 0x14: reserved
    hdr.extend_from_slice(&0u32.to_le_bytes()); // 0x18: reserved
    hdr.extend_from_slice(&file_count.to_le_bytes()); // 0x1C: file_count
    hdr.extend_from_slice(&file_table_offset2_rel.to_le_bytes()); // 0x20: file_table_offset2

    // ── File table area ──────────────────────────────────────────
    // Directory pointer: offset to dir_name relative to file_table start.
    hdr.extend_from_slice(&(dir_name_offset as u32).to_le_bytes());

    // File descriptor 0: TEST.MIX (uncompressed).
    let mut fd0 = vec![0u8; FD_SIZE_V5];
    write_u32(&mut fd0, 0x00, file1_name_offset as u32); // name_offset
    write_u32(&mut fd0, 0x04, 0); // directory_index
    write_u16(&mut fd0, 0x08, 0); // flags (uncompressed)
    write_u32(&mut fd0, 0x0A, file1_data.len() as u32); // expanded_size
    write_u32(&mut fd0, 0x0E, file1_data.len() as u32); // compressed_size
    write_u32(&mut fd0, 0x26, 0); // data_offset in cab
    write_u16(&mut fd0, 0x2E, 0); // volume (0 = first)
    hdr.extend_from_slice(&fd0);

    // File descriptor 1: DATA.PAK (compressed).
    let mut fd1 = vec![0u8; FD_SIZE_V5];
    write_u32(&mut fd1, 0x00, file2_name_offset as u32);
    write_u32(&mut fd1, 0x04, 0);
    write_u16(&mut fd1, 0x08, FLAG_COMPRESSED);
    write_u32(&mut fd1, 0x0A, file2_raw.len() as u32);
    write_u32(&mut fd1, 0x0E, file2_compressed.len() as u32);
    write_u32(&mut fd1, 0x26, file1_data.len() as u32); // after file1 in cab
    write_u16(&mut fd1, 0x2E, 0);
    hdr.extend_from_slice(&fd1);

    // String data.
    hdr.extend_from_slice(dir_name);
    hdr.extend_from_slice(file1_name);
    hdr.extend_from_slice(file2_name);

    std::fs::write(&hdr_path, &hdr).unwrap();

    (hdr_path, vec![(1, cab_path)])
}

fn zlib_compress(data: &[u8]) -> Vec<u8> {
    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    use std::io::Write;

    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(data).unwrap();
    encoder.finish().unwrap()
}

fn write_u32(buf: &mut [u8], offset: usize, val: u32) {
    buf[offset..offset + 4].copy_from_slice(&val.to_le_bytes());
}

fn write_u16(buf: &mut [u8], offset: usize, val: u16) {
    buf[offset..offset + 2].copy_from_slice(&val.to_le_bytes());
}

// ── Signature validation ─────────────────────────────────────────

/// Opening a header whose first four bytes are not the IS magic fails with `BadSignature`.
///
/// The signature check is the first line of defence against accidentally
/// passing the wrong file — any non-IS binary must be rejected before
/// further parsing is attempted.
#[test]
fn open_rejects_bad_signature() {
    let tmp = std::env::temp_dir().join("cnc-iscab-bad-sig");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();

    let hdr_path = tmp.join("data1.hdr");
    std::fs::write(&hdr_path, [0u8; 20]).unwrap();

    let result = IscabArchive::open(&hdr_path);
    assert!(matches!(result, Err(IscabError::BadSignature { .. })));

    let _ = std::fs::remove_dir_all(&tmp);
}

/// Opening a header whose version major field is outside {5, 6} fails with `UnsupportedVersion`.
///
/// Only InstallShield versions 5 and 6 are supported. Version 9 is used
/// here as a representative unsupported value to confirm the error variant
/// and the extracted major number.
#[test]
fn open_rejects_unsupported_version() {
    let tmp = std::env::temp_dir().join("cnc-iscab-bad-ver");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();

    let hdr_path = tmp.join("data1.hdr");
    let mut data = vec![0u8; 64];
    data[0..4].copy_from_slice(&SIGNATURE.to_le_bytes());
    // Version with major = 9 (unsupported).
    data[4..8].copy_from_slice(&0x0000_9000u32.to_le_bytes());
    std::fs::write(&hdr_path, &data).unwrap();

    let result = IscabArchive::open(&hdr_path);
    assert!(matches!(
        result,
        Err(IscabError::UnsupportedVersion { major: 9 })
    ));

    let _ = std::fs::remove_dir_all(&tmp);
}

// ── Extraction ───────────────────────────────────────────────────

/// Extracts an uncompressed file from a synthetic v5 archive verbatim.
///
/// Uncompressed entries must be returned byte-for-byte as stored in the
/// cabinet volume with no decompression or transformation applied.
#[test]
fn extract_uncompressed_file() {
    let tmp = std::env::temp_dir().join("cnc-iscab-extract-raw");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();

    let (hdr_path, vol_paths) = build_test_archive(&tmp);
    let archive = IscabArchive::open(&hdr_path).unwrap();

    let vol_refs: Vec<(u32, &Path)> = vol_paths.iter().map(|(i, p)| (*i, p.as_path())).collect();
    let data = archive.extract("TEST.MIX", &vol_refs).unwrap();
    assert_eq!(data, b"HELLO WORLD");

    let _ = std::fs::remove_dir_all(&tmp);
}

/// Extracts a zlib-compressed file and returns the original uncompressed bytes.
///
/// When the `FLAG_COMPRESSED` bit is set in the file descriptor the reader
/// must decompress the cabinet data before returning it, restoring the
/// original content exactly.
#[test]
fn extract_compressed_file() {
    let tmp = std::env::temp_dir().join("cnc-iscab-extract-zlib");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();

    let (hdr_path, vol_paths) = build_test_archive(&tmp);
    let archive = IscabArchive::open(&hdr_path).unwrap();

    let vol_refs: Vec<(u32, &Path)> = vol_paths.iter().map(|(i, p)| (*i, p.as_path())).collect();
    let data = archive.extract("DATA.PAK", &vol_refs).unwrap();
    assert_eq!(data, b"COMPRESSED DATA HERE");

    let _ = std::fs::remove_dir_all(&tmp);
}

/// File lookup is case-insensitive so `test.mix` matches the `TEST.MIX` entry.
///
/// InstallShield archives use uppercase names on Windows-era installers, but
/// callers on case-sensitive systems may supply lowercase names. The extractor
/// must normalise both sides to lowercase before comparing.
#[test]
fn extract_case_insensitive() {
    let tmp = std::env::temp_dir().join("cnc-iscab-case");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();

    let (hdr_path, vol_paths) = build_test_archive(&tmp);
    let archive = IscabArchive::open(&hdr_path).unwrap();

    let vol_refs: Vec<(u32, &Path)> = vol_paths.iter().map(|(i, p)| (*i, p.as_path())).collect();
    let data = archive.extract("test.mix", &vol_refs).unwrap();
    assert_eq!(data, b"HELLO WORLD");

    let _ = std::fs::remove_dir_all(&tmp);
}

/// Requesting a file that is not in the archive returns `FileNotFound`.
///
/// The error must be returned rather than panicking or producing empty
/// data, so callers can distinguish a missing entry from other failures.
#[test]
fn extract_nonexistent_file_errors() {
    let tmp = std::env::temp_dir().join("cnc-iscab-notfound");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();

    let (hdr_path, vol_paths) = build_test_archive(&tmp);
    let archive = IscabArchive::open(&hdr_path).unwrap();

    let vol_refs: Vec<(u32, &Path)> = vol_paths.iter().map(|(i, p)| (*i, p.as_path())).collect();
    let result = archive.extract("NONEXISTENT.MIX", &vol_refs);
    assert!(matches!(result, Err(IscabError::FileNotFound(_))));

    let _ = std::fs::remove_dir_all(&tmp);
}

/// `file_names` iterates over every entry in the archive without omissions.
///
/// The iterator must yield exactly as many entries as the header declares
/// and each name must be reachable via a subsequent `extract` call.
#[test]
fn file_names_lists_all_entries() {
    let tmp = std::env::temp_dir().join("cnc-iscab-list");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();

    let (hdr_path, _) = build_test_archive(&tmp);
    let archive = IscabArchive::open(&hdr_path).unwrap();

    let names: Vec<&str> = archive.file_names().collect();
    assert_eq!(names.len(), 2);
    assert!(names.iter().any(|n| n.contains("TEST.MIX")));
    assert!(names.iter().any(|n| n.contains("DATA.PAK")));

    let _ = std::fs::remove_dir_all(&tmp);
}

/// Opening a header shorter than 20 bytes returns an error without panicking.
///
/// The minimum parseable header is 20 bytes. Truncated files must be
/// rejected cleanly so a partial download or corrupt file does not cause
/// an out-of-bounds read or panic inside the parser.
#[test]
fn open_rejects_truncated_header() {
    let tmp = std::env::temp_dir().join("cnc-iscab-truncated");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();

    let hdr_path = tmp.join("data1.hdr");
    std::fs::write(&hdr_path, [0x49, 0x53, 0x63, 0x28, 0, 0]).unwrap();

    let result = IscabArchive::open(&hdr_path);
    assert!(result.is_err());

    let _ = std::fs::remove_dir_all(&tmp);
}

// ── MissingVolume error ─────────────────────────────────────────

/// Extracting a file whose volume is not in the provided volumes slice
/// returns `MissingVolume`.
///
/// Guards against silent data loss when a multi-volume archive is only
/// partially available — the caller must supply every referenced cabinet.
#[test]
fn extract_missing_volume_errors() {
    let tmp = std::env::temp_dir().join("cnc-iscab-missing-vol");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();

    // Build a custom v5 header with one file on volume 2.
    let hdr_path = tmp.join("data1.hdr");
    let cab_path = tmp.join("data1.cab");

    let file_data = b"VOLUME2 DATA";
    std::fs::write(&cab_path, file_data).unwrap();

    let mut hdr = Vec::new();

    // Main header (20 bytes).
    hdr.extend_from_slice(&SIGNATURE.to_le_bytes());
    hdr.extend_from_slice(&0x0000_5000u32.to_le_bytes()); // major=5
    hdr.extend_from_slice(&0u32.to_le_bytes());
    let cab_desc_offset: u32 = 20;
    hdr.extend_from_slice(&cab_desc_offset.to_le_bytes());
    hdr.extend_from_slice(&0u32.to_le_bytes());

    // Cab descriptor (0x24 bytes).
    let file_table_rel: u32 = 0x24;
    let directory_count: u32 = 1;
    let file_count: u32 = 1;

    let dir_ptrs_size = (directory_count as usize) * 4;
    let fds_size = (file_count as usize) * FD_SIZE_V5;
    let strings_start = dir_ptrs_size + fds_size;

    let dir_name = b"\0"; // empty directory
    let file_name = b"test_file\0";

    let dir_name_offset = strings_start;
    let file_name_offset = dir_name_offset + dir_name.len();

    let file_table_offset2_rel = file_table_rel + dir_ptrs_size as u32;

    hdr.extend_from_slice(&file_table_rel.to_le_bytes());
    hdr.extend_from_slice(&0u32.to_le_bytes());
    hdr.extend_from_slice(&0u32.to_le_bytes());
    hdr.extend_from_slice(&0u32.to_le_bytes());
    hdr.extend_from_slice(&directory_count.to_le_bytes());
    hdr.extend_from_slice(&0u32.to_le_bytes());
    hdr.extend_from_slice(&0u32.to_le_bytes());
    hdr.extend_from_slice(&file_count.to_le_bytes());
    hdr.extend_from_slice(&file_table_offset2_rel.to_le_bytes());

    // Directory pointer.
    hdr.extend_from_slice(&(dir_name_offset as u32).to_le_bytes());

    // File descriptor: volume = 2.
    let mut fd = vec![0u8; FD_SIZE_V5];
    write_u32(&mut fd, 0x00, file_name_offset as u32);
    write_u32(&mut fd, 0x04, 0);
    write_u16(&mut fd, 0x08, 0); // uncompressed
    write_u32(&mut fd, 0x0A, file_data.len() as u32);
    write_u32(&mut fd, 0x0E, file_data.len() as u32);
    write_u32(&mut fd, 0x26, 0);
    write_u16(&mut fd, 0x2E, 2); // volume 2
    hdr.extend_from_slice(&fd);

    // Strings.
    hdr.extend_from_slice(dir_name);
    hdr.extend_from_slice(file_name);

    std::fs::write(&hdr_path, &hdr).unwrap();

    let archive = IscabArchive::open(&hdr_path).unwrap();

    // Provide only volume 1.
    let volumes: Vec<(u32, &Path)> = vec![(1, cab_path.as_path())];
    let result = archive.extract("test_file", &volumes);
    assert!(
        matches!(result, Err(IscabError::MissingVolume(2))),
        "expected MissingVolume(2), got {result:?}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

// ── Error Display messages ──────────────────────────────────────

/// `BadSignature` display includes both the actual and expected signature
/// in hex.
///
/// Ensures the error message is actionable: a user or developer can see
/// the expected magic bytes alongside what was found.
#[test]
fn iscab_error_display_bad_signature() {
    let err = IscabError::BadSignature {
        actual: 0xDEAD_BEEF,
    };
    let msg = err.to_string();
    assert!(
        msg.contains("0xdeadbeef"),
        "expected actual signature in message: {msg}"
    );
    assert!(
        msg.contains("0x28635349"),
        "expected expected signature in message: {msg}"
    );
}

/// `FileNotFound` display includes the requested filename.
///
/// Ensures the user can identify which file lookup failed without
/// inspecting the error variant programmatically.
#[test]
fn iscab_error_display_file_not_found() {
    let err = IscabError::FileNotFound("missing.dat".to_string());
    let msg = err.to_string();
    assert!(
        msg.contains("missing.dat"),
        "expected filename in message: {msg}"
    );
}

/// `UnsupportedVersion` display includes the rejected major version
/// number.
///
/// Helps diagnose which archive format was encountered when parsing
/// fails.
#[test]
fn iscab_error_display_unsupported_version() {
    let err = IscabError::UnsupportedVersion { major: 99 };
    let msg = err.to_string();
    assert!(msg.contains("99"), "expected version in message: {msg}");
}

/// `MissingVolume` display includes the volume number that was not
/// provided.
///
/// Lets the caller know exactly which cabinet file needs to be
/// supplied.
#[test]
fn iscab_error_display_missing_volume() {
    let err = IscabError::MissingVolume(3);
    let msg = err.to_string();
    assert!(
        msg.contains("3"),
        "expected volume number in message: {msg}"
    );
}

// ── Version boundary tests ──────────────────────────────────────

/// Major version 4 (one below the minimum supported) is rejected.
///
/// Validates the lower boundary of the version check — only versions 5
/// and 6 are accepted, so 4 must fail.
#[test]
fn open_rejects_version_4() {
    let tmp = std::env::temp_dir().join("cnc-iscab-ver4");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();

    let hdr_path = tmp.join("data1.hdr");
    let mut data = vec![0u8; 64];
    data[0..4].copy_from_slice(&SIGNATURE.to_le_bytes());
    // major = (version >> 12) & 0xF = 4
    data[4..8].copy_from_slice(&((4u32 << 12).to_le_bytes()));
    std::fs::write(&hdr_path, &data).unwrap();

    let result = IscabArchive::open(&hdr_path);
    assert!(
        matches!(result, Err(IscabError::UnsupportedVersion { major: 4 })),
        "expected UnsupportedVersion with major 4"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

/// Major version 7 (one above the maximum supported) is rejected.
///
/// Validates the upper boundary of the version check — only versions 5
/// and 6 are accepted, so 7 must fail.
#[test]
fn open_rejects_version_7() {
    let tmp = std::env::temp_dir().join("cnc-iscab-ver7");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();

    let hdr_path = tmp.join("data1.hdr");
    let mut data = vec![0u8; 64];
    data[0..4].copy_from_slice(&SIGNATURE.to_le_bytes());
    // major = (version >> 12) & 0xF = 7
    data[4..8].copy_from_slice(&((7u32 << 12).to_le_bytes()));
    std::fs::write(&hdr_path, &data).unwrap();

    let result = IscabArchive::open(&hdr_path);
    assert!(
        matches!(result, Err(IscabError::UnsupportedVersion { major: 7 })),
        "expected UnsupportedVersion with major 7"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}
