//! Extraction tests for the InstallShield CAB reader.

use super::*;

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
    assert!(matches!(result, Err(IscabError::FileNotFound { .. })));

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
        matches!(result, Err(IscabError::MissingVolume { volume: 2 })),
        "expected MissingVolume {{ volume: 2 }}, got {result:?}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}
