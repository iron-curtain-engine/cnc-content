//! Size limit and zlib safety tests for the InstallShield CAB reader.

use super::*;

// ── Security: entry size limits ─────────────────────────────────

/// The ISCAB entry size limit is reasonable for C&C game content.
///
/// The largest individual files in C&C game installers are ~200 MB.
/// The limit must be generous enough for any real content while
/// preventing OOM from crafted headers declaring multi-GB entries.
#[test]
fn iscab_entry_size_limit_is_sane() {
    const {
        assert!(
            MAX_ISCAB_ENTRY_SIZE >= 200 * 1024 * 1024,
            "limit must handle large C&C game files (~200 MB)"
        );
        assert!(
            MAX_ISCAB_ENTRY_SIZE <= 1024 * 1024 * 1024,
            "limit should prevent OOM from crafted headers"
        );
    }
}

/// Zlib decompression rejects output that exceeds the declared expanded size.
///
/// A crafted zlib stream could decompress to vastly more data than the
/// header's `expanded_size` declares (CWE-409 decompression bomb). The
/// `decompress_zlib` function must cap output at the declared size and
/// return `IscabError::Corrupt` if the stream produces excess data.
#[test]
fn decompress_zlib_rejects_oversized_output() {
    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    use std::io::Write;

    // Create a zlib-compressed payload of 1000 zero bytes.
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::best());
    encoder.write_all(&[0u8; 1000]).unwrap();
    let compressed = encoder.finish().unwrap();

    // Declare expanded_size as 500 — less than actual decompressed size.
    // The function should reject because the stream produces more data
    // than declared.
    let result = decompress_zlib(&compressed, 500);
    assert!(
        result.is_err(),
        "should reject oversized decompression output"
    );
    let err = result.unwrap_err();
    assert!(
        matches!(err, IscabError::Corrupt { .. }),
        "expected Corrupt error, got: {err}"
    );
}

/// Zlib decompression accepts output that matches the declared size.
///
/// Normal case: compressed data decompresses to exactly the declared
/// expanded_size. This must succeed without error.
#[test]
fn decompress_zlib_accepts_matching_output() {
    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    use std::io::Write;

    let original = b"hello world from ISCAB test data";
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(original).unwrap();
    let compressed = encoder.finish().unwrap();

    let result = decompress_zlib(&compressed, original.len());
    assert!(result.is_ok(), "matching size should succeed");
    assert_eq!(result.unwrap(), original);
}

// ── Security: integer overflow in header parsing ─────────────────

/// Cab descriptor offset near usize::MAX must not wrap around the bounds check.
///
/// A crafted header with `cab_desc_offset` close to `usize::MAX` could cause
/// `cab_desc_offset + 0x24` to overflow and wrap to a small value, bypassing
/// the bounds check. The parser must use `checked_add` to detect this and
/// return `Corrupt` instead.
#[test]
fn open_rejects_overflowing_cab_desc_offset() {
    let tmp = std::env::temp_dir().join("cnc-iscab-overflow-cab");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();

    let hdr_path = tmp.join("data1.hdr");
    let mut data = vec![0u8; 64];
    data[0..4].copy_from_slice(&SIGNATURE.to_le_bytes());
    // Version 5
    data[4..8].copy_from_slice(&((5u32 << 12).to_le_bytes()));
    // Set cab_desc_offset at byte 0x0C to 0xFFFF_FF00 — near u32::MAX.
    // On 32-bit, adding 0x24 wraps around. On 64-bit, it's simply out of
    // bounds. Either way, the parser must reject it.
    write_u32(&mut data, 0x0C, 0xFFFF_FF00);
    std::fs::write(&hdr_path, &data).unwrap();

    let result = IscabArchive::open(&hdr_path);
    assert!(result.is_err(), "should reject overflowing cab_desc_offset");

    let _ = std::fs::remove_dir_all(&tmp);
}

/// The MAX_CSTRING_LEN constant is reasonable.
///
/// InstallShield filenames are short paths. The constant must be between
/// a minimum reasonable value and a maximum that prevents excessive
/// allocation from corrupted headers.
#[test]
fn cstring_length_limit_is_sane() {
    const {
        assert!(
            MAX_CSTRING_LEN >= 260,
            "limit must handle MAX_PATH-length filenames"
        );
        assert!(
            MAX_CSTRING_LEN <= 64 * 1024,
            "limit should not allow excessive string allocation"
        );
    }
}

/// `read_cstring` truncates at MAX_CSTRING_LEN when no NUL terminator is found.
///
/// A corrupted header offset pointing into non-NUL binary data should NOT
/// cause the parser to read megabytes of header data into a single String.
/// The scan must stop at MAX_CSTRING_LEN bytes.
#[test]
fn read_cstring_truncates_at_max_length() {
    // Create a buffer larger than MAX_CSTRING_LEN with no NUL bytes.
    let data = vec![b'A'; MAX_CSTRING_LEN + 1000];
    let result = read_cstring(&data, 0);
    assert_eq!(
        result.len(),
        MAX_CSTRING_LEN,
        "should truncate at MAX_CSTRING_LEN, got {} bytes",
        result.len()
    );
}

/// `read_cstring` returns the correct string when a NUL terminator is found
/// within the scan limit.
///
/// Normal case: the NUL terminator is well within MAX_CSTRING_LEN.
#[test]
fn read_cstring_finds_nul_within_limit() {
    let mut data = vec![0u8; 100];
    data[0] = b'h';
    data[1] = b'i';
    data[2] = 0; // NUL terminator
    let result = read_cstring(&data, 0);
    assert_eq!(result, "hi");
}
