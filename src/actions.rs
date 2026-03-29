// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! Install actions that extract or copy files from a source into managed storage.
//!
//! Each action maps to one of the operations OpenRA's install system supports:
//! Copy, ExtractMix, ExtractIscab (InstallShield CAB), ExtractRaw, ExtractZip,
//! and Delete. All types use `&'static` references so recipe data can be
//! compile-time constants.

/// A single file mapping: source path → target path (relative to content root).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMapping {
    /// Path inside the source (relative to source root or archive).
    pub from: &'static str,
    /// Destination path relative to the managed content root.
    pub to: &'static str,
}

/// A raw byte-range extraction entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawExtractEntry {
    /// Source file path relative to the source root.
    pub source: &'static str,
    /// Byte offset to start reading from.
    pub offset: u64,
    /// Number of bytes to read.
    pub length: u64,
    /// Destination path relative to the managed content root.
    pub to: &'static str,
}

/// An install action that the executor runs against a source path.
///
/// Actions are executed in order; later actions may depend on files created
/// by earlier ones (e.g. ExtractMix from a file that Copy placed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallAction {
    /// Copy files from source to managed storage.
    Copy {
        /// File mappings (source-relative → content-relative).
        files: &'static [FileMapping],
    },

    /// Extract named entries from a MIX archive in the source directory.
    ExtractMix {
        /// Path to the MIX file, relative to the source root.
        source_mix: &'static str,
        /// Entries to extract (MIX entry name → content-relative path).
        entries: &'static [FileMapping],
    },

    /// Extract files from an InstallShield CAB archive (The First Decade).
    ExtractIscab {
        /// Path to the `.hdr` header file, relative to the source root.
        header: &'static str,
        /// Volume files: (volume index, path relative to source root).
        volumes: &'static [(u32, &'static str)],
        /// Entries to extract (CAB entry name → content-relative path).
        entries: &'static [FileMapping],
    },

    /// Extract raw byte ranges from a file (e.g. Aftermath PATCH.RTP).
    ExtractRaw {
        /// Individual byte-range extraction entries.
        entries: &'static [RawExtractEntry],
    },

    /// Extract entries from a ZIP archive (HTTP downloads).
    ExtractZip {
        /// Entries to extract (ZIP entry name → content-relative path).
        entries: &'static [FileMapping],
    },

    /// Extract entries from a MIX archive already in the content root.
    ///
    /// Used for nested extraction: first extract a MIX from a source MIX into
    /// the content root (via `ExtractMix`), then extract individual files from
    /// that intermediate MIX (via this action).
    ExtractMixFromContent {
        /// Path to the MIX file, relative to the content root.
        content_mix: &'static str,
        /// Entries to extract (MIX entry name → content-relative path).
        entries: &'static [FileMapping],
    },

    /// Delete a temporary file created by a previous action.
    Delete {
        /// Path relative to the managed content root.
        path: &'static str,
    },
}
