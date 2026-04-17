//! Unit tests for the InstallShield CAB v5/v6 archive reader.
//!
//! Builds synthetic `.hdr`/`.cab` pairs in memory and verifies that
//! `parse_header` and `extract_file` handle valid archives, corrupt
//! inputs, and edge cases (empty files, zlib-compressed data) correctly.

use super::*;

mod errors;
mod extract;
mod limits;

// ── Helper: build a minimal synthetic v5 InstallShield archive ───

/// Builds a synthetic data1.hdr + data1.cab pair for testing.
/// Contains a single uncompressed file and a single zlib-compressed file.
pub(super) fn build_test_archive(
    dir: &Path,
) -> (std::path::PathBuf, Vec<(u32, std::path::PathBuf)>) {
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

pub(super) fn zlib_compress(data: &[u8]) -> Vec<u8> {
    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    use std::io::Write;

    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(data).unwrap();
    encoder.finish().unwrap()
}

pub(super) fn write_u32(buf: &mut [u8], offset: usize, val: u32) {
    buf[offset..offset + 4].copy_from_slice(&val.to_le_bytes());
}

pub(super) fn write_u16(buf: &mut [u8], offset: usize, val: u16) {
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
