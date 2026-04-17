//! Archive + metadata tests: ExtractBagIdx and describe_action output.

use super::*;

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
    static BIG_DESC_ENTRIES: [FileMapping; 2] = [
        FileMapping {
            from: "data\\config.ini",
            to: "config.ini",
        },
        FileMapping {
            from: "data\\terrain.tga",
            to: "terrain.tga",
        },
    ];
    let desc = describe_action(&InstallAction::ExtractBig {
        source_big: "INI.big",
        entries: &BIG_DESC_ENTRIES,
    });
    assert!(desc.contains("BIG"), "BIG type: {desc}");
    assert!(desc.contains("INI.big"), "BIG name: {desc}");

    // ExtractMeg
    static MEG_DESC_ENTRIES: [FileMapping; 2] = [
        FileMapping {
            from: "data/audio.aud",
            to: "audio.aud",
        },
        FileMapping {
            from: "data/video.bik",
            to: "video.bik",
        },
    ];
    let desc = describe_action(&InstallAction::ExtractMeg {
        source_meg: "data.meg",
        entries: &MEG_DESC_ENTRIES,
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
