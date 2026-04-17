//! Error display and version rejection tests for the InstallShield CAB reader.

use super::*;

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
    let err = IscabError::FileNotFound {
        name: "missing.dat".to_string(),
    };
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
    let err = IscabError::MissingVolume { volume: 3 };
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
