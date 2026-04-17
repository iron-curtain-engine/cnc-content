use super::*;

// ── VerifyBitfield ──────────────────────────────────────────────

/// The verify bit-field must correctly set and retrieve individual bit positions.
///
/// The bit-field is the core data structure for tracking which files have passed
/// verification; incorrect `set`/`get` round-trips would silently mark failed
/// files as passing, undermining the entire integrity check.
#[cfg(feature = "fast-verify")]
#[test]
fn bitfield_set_and_get() {
    let mut bf = VerifyBitfield::new(100);
    assert!(!bf.get(0));
    assert!(!bf.get(99));

    bf.set(0);
    bf.set(42);
    bf.set(99);

    assert!(bf.get(0));
    assert!(bf.get(42));
    assert!(bf.get(99));
    assert!(!bf.get(1));
    assert!(!bf.get(98));
}

/// `count_ones` and `count_failures` on the verify bit-field must reflect the exact number of set bits.
///
/// Progress reporting and repair decisions depend on these counts being accurate;
/// an off-by-one would either hide failures or trigger unnecessary re-downloads.
#[cfg(feature = "fast-verify")]
#[test]
fn bitfield_count_ones() {
    let mut bf = VerifyBitfield::new(256);
    assert_eq!(bf.count_ones(), 0);

    bf.set(0);
    bf.set(63);
    bf.set(64);
    bf.set(127);
    bf.set(255);
    assert_eq!(bf.count_ones(), 5);
    assert_eq!(bf.count_failures(), 251);
}

/// SIMD `and` and `or` operations on the verify bit-field must compute correct set intersection and union.
///
/// These operations answer "which files are both installed and verified" (AND) and
/// "which files have been touched at all" (OR); wrong SIMD lane indexing would
/// corrupt the bit positions and produce incorrect answers for all subsequent queries.
#[cfg(feature = "fast-verify")]
#[test]
fn bitfield_and_or_operations() {
    let mut a = VerifyBitfield::new(128);
    let mut b = VerifyBitfield::new(128);

    a.set(0);
    a.set(1);
    a.set(2);

    b.set(1);
    b.set(2);
    b.set(3);

    let intersection = a.and(&b);
    assert!(!intersection.get(0));
    assert!(intersection.get(1));
    assert!(intersection.get(2));
    assert!(!intersection.get(3));
    assert_eq!(intersection.count_ones(), 2);

    let union = a.or(&b);
    assert!(union.get(0));
    assert!(union.get(1));
    assert!(union.get(2));
    assert!(union.get(3));
    assert_eq!(union.count_ones(), 4);
}

/// The `and_not` operation must return bits set in `self` but not in `other`.
///
/// This computes the "remaining work" set: starting from all files, subtracting
/// already-checked files gives exactly the files that still need verification.
/// An incorrect implementation would either re-check completed files or skip
/// files that still need checking.
#[cfg(feature = "fast-verify")]
#[test]
fn bitfield_and_not_for_remaining_work() {
    let mut all = VerifyBitfield::new(64);
    for i in 0..64 {
        all.set(i);
    }
    let mut checked = VerifyBitfield::new(64);
    checked.set(0);
    checked.set(10);
    checked.set(63);

    let remaining = all.and_not(&checked);
    assert_eq!(remaining.count_ones(), 61);
    assert!(!remaining.get(0));
    assert!(!remaining.get(10));
    assert!(!remaining.get(63));
    assert!(remaining.get(1));
}

/// `set_indices` must return exactly the indices of all set bits in ascending order.
///
/// Callers use this to translate the compact bit representation back into file
/// indices; a missing or duplicated index would cause a file to be skipped or
/// repaired twice.
#[cfg(feature = "fast-verify")]
#[test]
fn bitfield_set_indices() {
    let mut bf = VerifyBitfield::new(300);
    bf.set(0);
    bf.set(100);
    bf.set(200);
    bf.set(299);

    let indices = bf.set_indices();
    assert_eq!(indices, vec![0, 100, 200, 299]);
}

/// Bit positions that straddle a 256-bit SIMD lane boundary must be handled correctly.
///
/// Each `u64x4` lane holds 256 bits; bit 255 is the last bit of lane 0 and bit 256
/// is the first bit of lane 1. An off-by-one in the lane or word index calculation
/// would silently corrupt either of these positions while all other bits appear correct.
#[cfg(feature = "fast-verify")]
#[test]
fn bitfield_cross_lane_boundary() {
    // Test bits that cross the 256-bit lane boundary.
    let mut bf = VerifyBitfield::new(512);
    bf.set(255); // last bit of lane 0
    bf.set(256); // first bit of lane 1
    assert!(bf.get(255));
    assert!(bf.get(256));
    assert!(!bf.get(254));
    assert!(!bf.get(257));
    assert_eq!(bf.count_ones(), 2);
}

// ── Incremental verification ────────────────────────────────────

/// `verify_incremental` must distribute all files across slots with no gaps or overlaps.
///
/// The staggered verification scheme is only correct if every file appears in exactly
/// one slot across a full cycle; a file that falls into no slot would never be verified,
/// allowing silent corruption to go undetected indefinitely.
///
/// The test creates 10 files, runs all 5 slots, and asserts that the total checked
/// count equals 10 and every slot reports no failures.
#[test]
fn incremental_verify_distributes_files() {
    let tmp = std::env::temp_dir().join("cnc-verify-incremental");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();

    // Create 10 files with known content.
    let mut manifest_files = BTreeMap::new();
    for i in 0..10 {
        let name = format!("file{i}.bin");
        let data = format!("content for file {i}");
        let path = tmp.join(&name);
        fs::write(&path, data.as_bytes()).unwrap();
        let blake3 = blake3_file(&path).unwrap();
        let size = data.len() as u64;
        manifest_files.insert(name, FileDigest { blake3, size });
    }

    let manifest = InstalledContentManifest {
        version: CONTENT_MANIFEST_VERSION,
        game: "test".to_string(),
        content_version: "v1".to_string(),
        files: manifest_files,
    };

    // With 5 slots, each slot should check ~2 files.
    let mut total_checked = 0;
    for slot in 0..5 {
        let result = verify_incremental(&tmp, &manifest, slot, 5);
        assert!(result.failures.is_empty());
        assert_eq!(result.total_files, 10);
        assert_eq!(result.num_slots, 5);
        total_checked += result.checked.len();
    }
    // All 10 files should be covered across 5 slots.
    assert_eq!(total_checked, 10);

    let _ = fs::remove_dir_all(&tmp);
}

/// `verify_incremental` must report a failure when a file has been tampered with since manifest generation.
///
/// The incremental path must exercise the same hash comparison logic as the full
/// verification path; if it silently skipped the comparison, corruption introduced
/// between verification cycles would never be detected.
#[test]
fn incremental_verify_detects_corruption() {
    let tmp = std::env::temp_dir().join("cnc-verify-incr-corrupt");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();

    let path = tmp.join("data.bin");
    fs::write(&path, b"original").unwrap();
    let blake3 = blake3_file(&path).unwrap();

    let mut files = BTreeMap::new();
    files.insert("data.bin".to_string(), FileDigest { blake3, size: 8 });

    let manifest = InstalledContentManifest {
        version: CONTENT_MANIFEST_VERSION,
        game: "test".to_string(),
        content_version: "v1".to_string(),
        files,
    };

    // Corrupt the file.
    fs::write(&path, b"tampered").unwrap();

    let result = verify_incremental(&tmp, &manifest, 0, 1);
    assert_eq!(result.failures.len(), 1);
    assert_eq!(result.failures[0], "data.bin");

    let _ = fs::remove_dir_all(&tmp);
}
