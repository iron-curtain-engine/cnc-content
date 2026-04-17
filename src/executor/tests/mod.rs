//! Unit tests for the install recipe executor.
//!
//! Covers MIX extraction, BIG extraction, MEG extraction, BAG/IDX extraction,
//! ISCAB extraction, ZIP extraction, raw-offset extraction, file copy, and
//! delete actions, plus path-traversal security tests verifying that
//! `strict-path` boundaries are enforced.

use super::*;
use crate::actions::{FileMapping, RawExtractEntry};

pub(super) fn noop_progress(_: InstallProgress) {}

// ── Helper: build a minimal MIX archive from name/data pairs ─────

pub(super) fn build_mix(files: &[(&str, &[u8])]) -> Vec<u8> {
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

pub(super) static COPY_FILES: [FileMapping; 2] = [
    FileMapping {
        from: "allies.mix",
        to: "allies.mix",
    },
    FileMapping {
        from: "conquer.mix",
        to: "conquer.mix",
    },
];
pub(super) static COPY_ACTIONS: [InstallAction; 1] = [InstallAction::Copy { files: &COPY_FILES }];

pub(super) static MIX_ENTRIES: [FileMapping; 2] = [
    FileMapping {
        from: "allies.mix",
        to: "allies.mix",
    },
    FileMapping {
        from: "conquer.mix",
        to: "conquer.mix",
    },
];
pub(super) static MIX_ACTIONS: [InstallAction; 1] = [InstallAction::ExtractMix {
    source_mix: "main.mix",
    entries: &MIX_ENTRIES,
}];

pub(super) static CONTENT_MIX_ENTRIES: [FileMapping; 1] = [FileMapping {
    from: "inner.dat",
    to: "extracted/inner.dat",
}];
pub(super) static CONTENT_MIX_ACTIONS: [InstallAction; 1] =
    [InstallAction::ExtractMixFromContent {
        content_mix: "intermediate.mix",
        entries: &CONTENT_MIX_ENTRIES,
    }];

pub(super) static RAW_ENTRIES: [RawExtractEntry; 1] = [RawExtractEntry {
    source: "patch.rtp",
    offset: 100,
    length: 8,
    to: "expand/chunk.dat",
}];
pub(super) static RAW_ACTIONS: [InstallAction; 1] = [InstallAction::ExtractRaw {
    entries: &RAW_ENTRIES,
}];

pub(super) static DELETE_ACTIONS: [InstallAction; 1] = [InstallAction::Delete { path: "temp.mix" }];
pub(super) static DELETE_NOOP_ACTIONS: [InstallAction; 1] = [InstallAction::Delete {
    path: "nonexistent.mix",
}];

pub(super) static MIX_MISSING_ENTRIES: [FileMapping; 1] = [FileMapping {
    from: "foo",
    to: "foo",
}];
pub(super) static MIX_MISSING_ACTIONS: [InstallAction; 1] = [InstallAction::ExtractMix {
    source_mix: "nonexistent.mix",
    entries: &MIX_MISSING_ENTRIES,
}];

pub(super) static PROGRESS_FILES: [FileMapping; 1] = [FileMapping {
    from: "a.mix",
    to: "a.mix",
}];
pub(super) static PROGRESS_ACTIONS: [InstallAction; 1] = [InstallAction::Copy {
    files: &PROGRESS_FILES,
}];

pub(super) fn make_recipe(actions: &'static [InstallAction]) -> InstallRecipe {
    InstallRecipe {
        source: crate::SourceId::SteamTuc,
        package: crate::PackageId::RaBase,
        actions,
    }
}

mod actions;
mod bag_describe;
mod big_meg;
mod iso;
