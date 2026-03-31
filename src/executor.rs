// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! Install action executor — runs a recipe's action sequence against a source.
//!
//! The executor takes a source path and a content root, then processes each
//! [`InstallAction`] in order, reporting progress through a callback channel.
//!
//! Both source and content directories are enforced as strict path boundaries
//! via [`strict_path::PathBoundary`] internally. Every file read is constrained
//! to the source boundary and every write to the content boundary, preventing
//! path traversal even if recipe data or archive contents contain malicious paths.
//! This enforcement is invisible to callers — the public API accepts standard
//! `&Path` values.

use std::fs;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use strict_path::{PathBoundary, StrictPath};
use thiserror::Error;

use crate::actions::{FileMapping, InstallAction, RawExtractEntry};
use crate::InstallRecipe;

/// Progress report emitted by the executor for UI feedback.
#[derive(Debug, Clone)]
pub enum InstallProgress {
    /// Starting a new action (index, total actions, description).
    ActionStarted {
        index: usize,
        total: usize,
        description: String,
    },
    /// A file was successfully written.
    FileWritten { path: PathBuf, bytes: u64 },
    /// The entire recipe completed successfully.
    Completed {
        files_written: usize,
        total_bytes: u64,
    },
}

/// Errors that can occur during install action execution.
#[derive(Debug, Error)]
pub enum ExecutorError {
    #[error("I/O error during install: {0}")]
    Io(#[from] io::Error),

    #[error("MIX archive not found at source: {path}")]
    MixNotFound { path: PathBuf },

    #[error("MIX entry not found: {entry} in {archive}")]
    MixEntryNotFound { archive: String, entry: String },

    #[error("InstallShield CAB error: {0}")]
    Iscab(#[from] crate::iscab::IscabError),

    #[error("ZIP extraction error: {0}")]
    ZipError(String),

    #[error("source file not found: {0}")]
    SourceFileNotFound(PathBuf),

    #[error("path traversal blocked: \"{path}\" escapes boundary ({detail})")]
    PathTraversal { path: String, detail: String },
}

/// Executes an install recipe, extracting content from `source_root` into
/// `content_root`.
///
/// Calls `on_progress` for each meaningful step so UI can show a progress bar.
pub fn execute_recipe(
    recipe: &InstallRecipe,
    source_root: &Path,
    content_root: &Path,
    mut on_progress: impl FnMut(InstallProgress),
) -> Result<(), ExecutorError> {
    // Boundary enforcement: all source reads stay within source_root,
    // all content writes stay within content_root. These boundaries are
    // an internal security measure — callers pass plain &Path values.
    let source =
        PathBoundary::<()>::try_new(source_root).map_err(|e| ExecutorError::PathTraversal {
            path: source_root.display().to_string(),
            detail: format!("invalid source root: {e}"),
        })?;
    let content = PathBoundary::<()>::try_new_create(content_root).map_err(|e| {
        ExecutorError::PathTraversal {
            path: content_root.display().to_string(),
            detail: format!("invalid content root: {e}"),
        }
    })?;

    let total = recipe.actions.len();
    let mut files_written: usize = 0;
    let mut total_bytes: u64 = 0;

    for (i, action) in recipe.actions.iter().enumerate() {
        on_progress(InstallProgress::ActionStarted {
            index: i,
            total,
            description: describe_action(action),
        });

        match action {
            InstallAction::Copy { files } => {
                for mapping in *files {
                    let src = bounded_path(&source, mapping.from)?;
                    let dst = bounded_path(&content, mapping.to)?;
                    dst.create_parent_dir_all()?;
                    // StrictPath has no cross-boundary copy, so we use fs::copy
                    // with the validated paths.
                    let bytes = std::fs::copy(src.interop_path(), dst.interop_path())?;
                    on_progress(InstallProgress::FileWritten {
                        path: dst.unstrict(),
                        bytes,
                    });
                    files_written += 1;
                    total_bytes += bytes;
                }
            }

            InstallAction::ExtractMix {
                source_mix,
                entries,
            } => {
                let mix = bounded_path(&source, source_mix)?;
                let (written, bytes) =
                    extract_from_mix(&mix, &content, entries, source_mix, &mut on_progress)?;
                files_written += written;
                total_bytes += bytes;
            }

            InstallAction::ExtractMixFromContent {
                content_mix,
                entries,
            } => {
                // MIX file is in the content root, not the source root.
                let mix = bounded_path(&content, content_mix)?;
                let (written, bytes) =
                    extract_from_mix(&mix, &content, entries, content_mix, &mut on_progress)?;
                files_written += written;
                total_bytes += bytes;
            }

            InstallAction::ExtractIscab {
                header,
                volumes,
                entries,
            } => {
                let (written, bytes) = extract_from_iscab(
                    &source,
                    &content,
                    header,
                    volumes,
                    entries,
                    &mut on_progress,
                )?;
                files_written += written;
                total_bytes += bytes;
            }

            InstallAction::ExtractRaw { entries } => {
                for entry in *entries {
                    let bytes = extract_raw_entry(&source, &content, entry)?;
                    let dst = bounded_path(&content, entry.to)?;
                    on_progress(InstallProgress::FileWritten {
                        path: dst.unstrict(),
                        bytes,
                    });
                    files_written += 1;
                    total_bytes += bytes;
                }
            }

            InstallAction::ExtractZip { entries } => {
                let (written, bytes) =
                    extract_from_zip(&source, &content, entries, &mut on_progress)?;
                files_written += written;
                total_bytes += bytes;
            }

            InstallAction::Delete { path } => {
                let target = bounded_path(&content, path)?;
                if target.exists() {
                    // StrictPath has no remove method — use fs::remove_file
                    // with the validated path.
                    fs::remove_file(target.interop_path())?;
                }
            }
        }
    }

    on_progress(InstallProgress::Completed {
        files_written,
        total_bytes,
    });

    Ok(())
}

// ── Internal helpers ─────────────────────────────────────────────────

/// Validates a subpath stays within a boundary, producing a descriptive
/// error on traversal attempts.
fn bounded_path(boundary: &PathBoundary<()>, subpath: &str) -> Result<StrictPath, ExecutorError> {
    boundary
        .strict_join(subpath)
        .map_err(|e| ExecutorError::PathTraversal {
            path: subpath.to_string(),
            detail: e.to_string(),
        })
}

/// Extracts entries from a MIX archive into the content boundary.
fn extract_from_mix(
    mix_path: &StrictPath,
    content: &PathBoundary<()>,
    entries: &[FileMapping],
    archive_name: &str,
    on_progress: &mut impl FnMut(InstallProgress),
) -> Result<(usize, u64), ExecutorError> {
    if !mix_path.exists() {
        return Err(ExecutorError::MixNotFound {
            path: mix_path.clone().unstrict(),
        });
    }

    let file = mix_path.open_file()?;
    let reader = io::BufReader::new(file);
    let mut archive = cnc_formats::mix::MixArchiveReader::open(reader)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;

    let mut files_written = 0;
    let mut total_bytes: u64 = 0;

    for mapping in entries {
        let data = archive
            .read(mapping.from)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;

        let data = data.ok_or_else(|| ExecutorError::MixEntryNotFound {
            archive: archive_name.to_string(),
            entry: mapping.from.to_string(),
        })?;

        let dst = bounded_path(content, mapping.to)?;
        dst.create_parent_dir_all()?;
        dst.write(&data)?;

        let bytes = data.len() as u64;
        on_progress(InstallProgress::FileWritten {
            path: dst.unstrict(),
            bytes,
        });
        files_written += 1;
        total_bytes += bytes;
    }

    Ok((files_written, total_bytes))
}

/// Extracts a raw byte range from a source file into the content boundary.
fn extract_raw_entry(
    source: &PathBoundary<()>,
    content: &PathBoundary<()>,
    entry: &RawExtractEntry,
) -> Result<u64, ExecutorError> {
    let src = bounded_path(source, entry.source)?;
    if !src.exists() {
        return Err(ExecutorError::SourceFileNotFound(src.unstrict()));
    }

    let mut file = src.open_file()?;
    file.seek(SeekFrom::Start(entry.offset))?;

    let mut buf = vec![0u8; entry.length as usize];
    file.read_exact(&mut buf)?;

    let dst = bounded_path(content, entry.to)?;
    dst.create_parent_dir_all()?;
    dst.write(&buf)?;

    Ok(entry.length)
}

/// Extracts entries from an InstallShield CAB archive.
fn extract_from_iscab(
    source: &PathBoundary<()>,
    content: &PathBoundary<()>,
    header: &str,
    volumes: &[(u32, &str)],
    entries: &[FileMapping],
    on_progress: &mut impl FnMut(InstallProgress),
) -> Result<(usize, u64), ExecutorError> {
    let hdr = bounded_path(source, header)?;
    let archive = crate::iscab::IscabArchive::open(&hdr.clone().unstrict())?;

    // Validate all volume paths against source boundary before extraction.
    let vol_paths: Vec<(u32, PathBuf)> = volumes
        .iter()
        .map(|(idx, name)| bounded_path(source, name).map(|sp| (*idx, sp.unstrict())))
        .collect::<Result<_, _>>()?;
    let vol_refs: Vec<(u32, &Path)> = vol_paths.iter().map(|(i, p)| (*i, p.as_path())).collect();

    let mut files_written = 0;
    let mut total_bytes: u64 = 0;

    for mapping in entries {
        let data = archive.extract(mapping.from, &vol_refs)?;
        let dst = bounded_path(content, mapping.to)?;
        dst.create_parent_dir_all()?;
        let bytes = data.len() as u64;
        dst.write(&data)?;
        on_progress(InstallProgress::FileWritten {
            path: dst.unstrict(),
            bytes,
        });
        files_written += 1;
        total_bytes += bytes;
    }

    Ok((files_written, total_bytes))
}

/// Extracts entries from a ZIP archive in the source directory.
fn extract_from_zip(
    source: &PathBoundary<()>,
    content: &PathBoundary<()>,
    entries: &[FileMapping],
    on_progress: &mut impl FnMut(InstallProgress),
) -> Result<(usize, u64), ExecutorError> {
    // The `from` field in each mapping is "zip_path/entry_name" where
    // zip_path is the path to the ZIP within the source root, and
    // entry_name is the name inside the ZIP. We split on the first entry
    // that actually exists as a ZIP file.
    let mut files_written = 0;
    let mut total_bytes: u64 = 0;

    for mapping in entries {
        let src = bounded_path(source, mapping.from)?;
        if src.exists() {
            // Direct file copy (the "ZIP" is already extracted or is a raw file).
            let dst = bounded_path(content, mapping.to)?;
            dst.create_parent_dir_all()?;
            let data = src.read()?;
            dst.write(&data)?;
            let bytes = data.len() as u64;
            on_progress(InstallProgress::FileWritten {
                path: dst.unstrict(),
                bytes,
            });
            files_written += 1;
            total_bytes += bytes;
        } else {
            // Try to find the file inside a ZIP archive. Walk up the `from`
            // path looking for a `.zip` file, then treat the remainder as
            // the entry name inside the archive.
            let extracted = extract_zip_entry(source, mapping.from, mapping.to, content)?;
            if let Some((dst, bytes)) = extracted {
                on_progress(InstallProgress::FileWritten { path: dst, bytes });
                files_written += 1;
                total_bytes += bytes;
            } else {
                return Err(ExecutorError::SourceFileNotFound(src.unstrict()));
            }
        }
    }

    Ok((files_written, total_bytes))
}

/// Tries to find and extract an entry from a ZIP archive within the source
/// boundary.
///
/// Splits `from_path` at each `/` to find a component that ends with `.zip`
/// (or exists as a file). The remainder becomes the entry name inside the ZIP.
fn extract_zip_entry(
    source: &PathBoundary<()>,
    from_path: &str,
    to_path: &str,
    content: &PathBoundary<()>,
) -> Result<Option<(PathBuf, u64)>, ExecutorError> {
    let parts: Vec<&str> = from_path.split('/').collect();

    for split_at in 1..=parts.len() {
        let zip_rel: String = parts[..split_at].join("/");
        let zip_strict = bounded_path(source, &zip_rel)?;

        if zip_strict.is_file() && split_at < parts.len() {
            let entry_name: String = parts[split_at..].join("/");

            let file = zip_strict.open_file()?;
            let mut archive = zip::ZipArchive::new(io::BufReader::new(file))
                .map_err(|e| ExecutorError::ZipError(e.to_string()))?;

            // Try exact match first, then case-insensitive.
            let entry_index = archive
                .index_for_name(&entry_name)
                .or_else(|| {
                    let lower = entry_name.to_ascii_lowercase();
                    (0..archive.len()).find(|&i| {
                        archive
                            .name_for_index(i)
                            .is_some_and(|n| n.to_ascii_lowercase() == lower)
                    })
                })
                .ok_or_else(|| {
                    ExecutorError::ZipError(format!(
                        "entry '{entry_name}' not found in {}",
                        zip_rel
                    ))
                })?;

            let mut entry = archive
                .by_index(entry_index)
                .map_err(|e| ExecutorError::ZipError(e.to_string()))?;

            // Validate output path against the content boundary.
            let dst = bounded_path(content, to_path)?;
            dst.create_parent_dir_all()?;
            let mut out = dst.create_file()?;
            let bytes = io::copy(&mut entry, &mut out)?;

            return Ok(Some((dst.unstrict(), bytes)));
        }
    }

    Ok(None)
}

/// Returns a human-readable description of an install action.
fn describe_action(action: &InstallAction) -> String {
    match action {
        InstallAction::Copy { files } => {
            format!("Copying {} file(s)", files.len())
        }
        InstallAction::ExtractMix {
            source_mix,
            entries,
        } => {
            format!(
                "Extracting {} entry/entries from {}",
                entries.len(),
                source_mix
            )
        }
        InstallAction::ExtractMixFromContent {
            content_mix,
            entries,
        } => {
            format!(
                "Extracting {} entry/entries from {} (content)",
                entries.len(),
                content_mix
            )
        }
        InstallAction::ExtractIscab {
            header, entries, ..
        } => {
            format!(
                "Extracting {} entry/entries from InstallShield {}",
                entries.len(),
                header
            )
        }
        InstallAction::ExtractRaw { entries } => {
            format!("Extracting {} raw byte range(s)", entries.len())
        }
        InstallAction::ExtractZip { entries } => {
            format!("Extracting {} entry/entries from ZIP", entries.len())
        }
        InstallAction::Delete { path } => {
            format!("Deleting {path}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::{FileMapping, RawExtractEntry};

    fn noop_progress(_: InstallProgress) {}

    // ── Helper: build a minimal MIX archive from name/data pairs ─────

    fn build_mix(files: &[(&str, &[u8])]) -> Vec<u8> {
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

    static COPY_FILES: [FileMapping; 2] = [
        FileMapping {
            from: "allies.mix",
            to: "allies.mix",
        },
        FileMapping {
            from: "conquer.mix",
            to: "conquer.mix",
        },
    ];
    static COPY_ACTIONS: [InstallAction; 1] = [InstallAction::Copy { files: &COPY_FILES }];

    static MIX_ENTRIES: [FileMapping; 2] = [
        FileMapping {
            from: "allies.mix",
            to: "allies.mix",
        },
        FileMapping {
            from: "conquer.mix",
            to: "conquer.mix",
        },
    ];
    static MIX_ACTIONS: [InstallAction; 1] = [InstallAction::ExtractMix {
        source_mix: "main.mix",
        entries: &MIX_ENTRIES,
    }];

    static CONTENT_MIX_ENTRIES: [FileMapping; 1] = [FileMapping {
        from: "inner.dat",
        to: "extracted/inner.dat",
    }];
    static CONTENT_MIX_ACTIONS: [InstallAction; 1] = [InstallAction::ExtractMixFromContent {
        content_mix: "intermediate.mix",
        entries: &CONTENT_MIX_ENTRIES,
    }];

    static RAW_ENTRIES: [RawExtractEntry; 1] = [RawExtractEntry {
        source: "patch.rtp",
        offset: 100,
        length: 8,
        to: "expand/chunk.dat",
    }];
    static RAW_ACTIONS: [InstallAction; 1] = [InstallAction::ExtractRaw {
        entries: &RAW_ENTRIES,
    }];

    static DELETE_ACTIONS: [InstallAction; 1] = [InstallAction::Delete { path: "temp.mix" }];
    static DELETE_NOOP_ACTIONS: [InstallAction; 1] = [InstallAction::Delete {
        path: "nonexistent.mix",
    }];

    static MIX_MISSING_ENTRIES: [FileMapping; 1] = [FileMapping {
        from: "foo",
        to: "foo",
    }];
    static MIX_MISSING_ACTIONS: [InstallAction; 1] = [InstallAction::ExtractMix {
        source_mix: "nonexistent.mix",
        entries: &MIX_MISSING_ENTRIES,
    }];

    static PROGRESS_FILES: [FileMapping; 1] = [FileMapping {
        from: "a.mix",
        to: "a.mix",
    }];
    static PROGRESS_ACTIONS: [InstallAction; 1] = [InstallAction::Copy {
        files: &PROGRESS_FILES,
    }];

    fn make_recipe(actions: &'static [InstallAction]) -> InstallRecipe {
        InstallRecipe {
            source: crate::SourceId::SteamTuc,
            package: crate::PackageId::RaBase,
            actions,
        }
    }

    // ── Copy action ──────────────────────────────────────────────────

    /// Copies multiple files from the source root into the content root.
    ///
    /// After a successful `Copy` action every listed file must appear in the
    /// content directory with its original byte content preserved.
    #[test]
    fn execute_copy_action() {
        let tmp = std::env::temp_dir().join("cnc-exec-copy");
        let _ = fs::remove_dir_all(&tmp);
        let src = tmp.join("source");
        let dst = tmp.join("content");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dst).unwrap();

        fs::write(src.join("allies.mix"), b"allies-data").unwrap();
        fs::write(src.join("conquer.mix"), b"conquer-data").unwrap();

        execute_recipe(&make_recipe(&COPY_ACTIONS), &src, &dst, noop_progress).unwrap();
        assert_eq!(fs::read(dst.join("allies.mix")).unwrap(), b"allies-data");
        assert_eq!(fs::read(dst.join("conquer.mix")).unwrap(), b"conquer-data");

        let _ = fs::remove_dir_all(&tmp);
    }

    // ── ExtractMix action ────────────────────────────────────────────

    /// Extracts named entries from a MIX archive in the source root.
    ///
    /// The executor must locate the archive, look up each entry by name, and
    /// write the decompressed bytes to the correct destination path in the
    /// content root.
    #[test]
    fn execute_extract_mix_action() {
        let tmp = std::env::temp_dir().join("cnc-exec-mix");
        let _ = fs::remove_dir_all(&tmp);
        let src = tmp.join("source");
        let dst = tmp.join("content");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dst).unwrap();

        let mix_data = build_mix(&[
            ("allies.mix", b"allies-content"),
            ("conquer.mix", b"conquer-content"),
        ]);
        fs::write(src.join("main.mix"), &mix_data).unwrap();

        execute_recipe(&make_recipe(&MIX_ACTIONS), &src, &dst, noop_progress).unwrap();
        assert_eq!(fs::read(dst.join("allies.mix")).unwrap(), b"allies-content");
        assert_eq!(
            fs::read(dst.join("conquer.mix")).unwrap(),
            b"conquer-content"
        );

        let _ = fs::remove_dir_all(&tmp);
    }

    // ── ExtractMixFromContent action ─────────────────────────────────

    /// Extracts entries from a MIX archive that already lives in the content root.
    ///
    /// `ExtractMixFromContent` reads a MIX that was written by an earlier action
    /// rather than from the source media. The extracted file must land at its
    /// declared sub-path inside the content directory.
    #[test]
    fn execute_extract_mix_from_content_action() {
        let tmp = std::env::temp_dir().join("cnc-exec-mix-content");
        let _ = fs::remove_dir_all(&tmp);
        let src = tmp.join("source");
        let dst = tmp.join("content");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dst).unwrap();

        let mix_data = build_mix(&[("inner.dat", b"inner-file-data")]);
        fs::write(dst.join("intermediate.mix"), &mix_data).unwrap();

        execute_recipe(
            &make_recipe(&CONTENT_MIX_ACTIONS),
            &src,
            &dst,
            noop_progress,
        )
        .unwrap();
        assert_eq!(
            fs::read(dst.join("extracted/inner.dat")).unwrap(),
            b"inner-file-data"
        );

        let _ = fs::remove_dir_all(&tmp);
    }

    // ── ExtractRaw action ────────────────────────────────────────────

    /// Extracts an exact byte range from a source file into the content root.
    ///
    /// The executor must seek to the specified offset and read the declared
    /// number of bytes, writing only that slice to the destination path.
    ///
    /// The test embeds the expected bytes at offset 100 inside a 256-byte file
    /// and asserts that only those 8 bytes appear in the output.
    #[test]
    fn execute_extract_raw_action() {
        let tmp = std::env::temp_dir().join("cnc-exec-raw");
        let _ = fs::remove_dir_all(&tmp);
        let src = tmp.join("source");
        let dst = tmp.join("content");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dst).unwrap();

        let mut data = vec![0u8; 256];
        data[100..108].copy_from_slice(b"RAWCHUNK");
        fs::write(src.join("patch.rtp"), &data).unwrap();

        execute_recipe(&make_recipe(&RAW_ACTIONS), &src, &dst, noop_progress).unwrap();
        assert_eq!(fs::read(dst.join("expand/chunk.dat")).unwrap(), b"RAWCHUNK");

        let _ = fs::remove_dir_all(&tmp);
    }

    // ── Delete action ────────────────────────────────────────────────

    /// Deletes a file from the content root when it exists.
    ///
    /// A `Delete` action is used to remove interim files produced by earlier
    /// recipe steps. After the action completes the file must no longer be
    /// present on the filesystem.
    #[test]
    fn execute_delete_action() {
        let tmp = std::env::temp_dir().join("cnc-exec-delete");
        let _ = fs::remove_dir_all(&tmp);
        let src = tmp.join("source");
        let dst = tmp.join("content");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dst).unwrap();

        fs::write(dst.join("temp.mix"), b"temporary").unwrap();
        assert!(dst.join("temp.mix").exists());

        execute_recipe(&make_recipe(&DELETE_ACTIONS), &src, &dst, noop_progress).unwrap();
        assert!(!dst.join("temp.mix").exists());

        let _ = fs::remove_dir_all(&tmp);
    }

    /// Deleting a file that does not exist succeeds without error.
    ///
    /// A `Delete` action is idempotent — if the target is already absent the
    /// recipe must continue normally rather than propagating a not-found error.
    #[test]
    fn execute_delete_nonexistent_is_ok() {
        let tmp = std::env::temp_dir().join("cnc-exec-delete-noop");
        let _ = fs::remove_dir_all(&tmp);
        let src = tmp.join("source");
        let dst = tmp.join("content");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dst).unwrap();

        execute_recipe(
            &make_recipe(&DELETE_NOOP_ACTIONS),
            &src,
            &dst,
            noop_progress,
        )
        .unwrap();

        let _ = fs::remove_dir_all(&tmp);
    }

    // ── Missing source errors ────────────────────────────────────────

    /// Returns `MixNotFound` when the referenced MIX archive is absent.
    ///
    /// If the declared archive does not exist in the source root the executor
    /// must report a clear `MixNotFound` error rather than an opaque I/O
    /// failure, so callers can present a meaningful diagnostic.
    #[test]
    fn extract_mix_missing_archive_errors() {
        let tmp = std::env::temp_dir().join("cnc-exec-mix-missing");
        let _ = fs::remove_dir_all(&tmp);
        let src = tmp.join("source");
        let dst = tmp.join("content");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dst).unwrap();

        let result = execute_recipe(
            &make_recipe(&MIX_MISSING_ACTIONS),
            &src,
            &dst,
            noop_progress,
        );
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ExecutorError::MixNotFound { .. }
        ));

        let _ = fs::remove_dir_all(&tmp);
    }

    // ── Progress reporting ───────────────────────────────────────────

    /// The executor emits `ActionStarted`, `FileWritten`, and `Completed` events.
    ///
    /// Progress callbacks are the only feedback channel available to UI layers.
    /// The sequence must start with `ActionStarted`, include a `FileWritten`
    /// event for every file, and end with a `Completed` event that carries the
    /// correct file count.
    #[test]
    fn executor_reports_progress() {
        let tmp = std::env::temp_dir().join("cnc-exec-progress");
        let _ = fs::remove_dir_all(&tmp);
        let src = tmp.join("source");
        let dst = tmp.join("content");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dst).unwrap();

        fs::write(src.join("a.mix"), b"aaa").unwrap();

        let mut events = Vec::new();
        execute_recipe(&make_recipe(&PROGRESS_ACTIONS), &src, &dst, |p| {
            events.push(p)
        })
        .unwrap();

        assert!(events.len() >= 3);
        assert!(matches!(events[0], InstallProgress::ActionStarted { .. }));
        assert!(matches!(events[1], InstallProgress::FileWritten { .. }));
        assert!(matches!(
            events.last().unwrap(),
            InstallProgress::Completed {
                files_written: 1,
                ..
            }
        ));

        let _ = fs::remove_dir_all(&tmp);
    }

    // ── Path traversal security ─────────────────────────────────────

    static TRAVERSAL_CONTENT_FILES: [FileMapping; 1] = [FileMapping {
        from: "allies.mix",
        to: "../../escaped.txt",
    }];
    static TRAVERSAL_CONTENT_ACTIONS: [InstallAction; 1] = [InstallAction::Copy {
        files: &TRAVERSAL_CONTENT_FILES,
    }];

    static TRAVERSAL_SOURCE_FILES: [FileMapping; 1] = [FileMapping {
        from: "../../etc/passwd",
        to: "harmless.txt",
    }];
    static TRAVERSAL_SOURCE_ACTIONS: [InstallAction; 1] = [InstallAction::Copy {
        files: &TRAVERSAL_SOURCE_FILES,
    }];

    static TRAVERSAL_BACKSLASH_FILES: [FileMapping; 1] = [FileMapping {
        from: "allies.mix",
        to: "..\\..\\escaped.txt",
    }];
    static TRAVERSAL_BACKSLASH_ACTIONS: [InstallAction; 1] = [InstallAction::Copy {
        files: &TRAVERSAL_BACKSLASH_FILES,
    }];

    #[cfg(not(windows))]
    static TRAVERSAL_ABSOLUTE_FILES: [FileMapping; 1] = [FileMapping {
        from: "allies.mix",
        to: "/tmp/escaped.txt",
    }];
    #[cfg(windows)]
    static TRAVERSAL_ABSOLUTE_FILES: [FileMapping; 1] = [FileMapping {
        from: "allies.mix",
        to: "C:\\escaped.txt",
    }];
    static TRAVERSAL_ABSOLUTE_ACTIONS: [InstallAction; 1] = [InstallAction::Copy {
        files: &TRAVERSAL_ABSOLUTE_FILES,
    }];

    /// Rejects a `to` path that traverses above the content root.
    ///
    /// Path traversal in recipe destinations would allow writing files outside
    /// the managed content directory, breaking the sandbox boundary.
    #[test]
    fn executor_rejects_content_path_traversal() {
        let tmp = std::env::temp_dir().join("cnc-exec-traversal-content");
        let _ = fs::remove_dir_all(&tmp);
        let src = tmp.join("source");
        let dst = tmp.join("content");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dst).unwrap();

        fs::write(src.join("allies.mix"), b"allies-data").unwrap();

        let result = execute_recipe(
            &make_recipe(&TRAVERSAL_CONTENT_ACTIONS),
            &src,
            &dst,
            noop_progress,
        );
        assert!(matches!(result, Err(ExecutorError::PathTraversal { .. })));

        // The escaped file must not exist above the content root.
        assert!(!tmp.join("escaped.txt").exists());

        let _ = fs::remove_dir_all(&tmp);
    }

    /// Rejects a `from` path that traverses above the source root.
    ///
    /// Path traversal in recipe sources would allow reading arbitrary files
    /// from the host filesystem, breaking source-boundary containment.
    #[test]
    fn executor_rejects_source_path_traversal() {
        let tmp = std::env::temp_dir().join("cnc-exec-traversal-source");
        let _ = fs::remove_dir_all(&tmp);
        let src = tmp.join("source");
        let dst = tmp.join("content");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dst).unwrap();

        let result = execute_recipe(
            &make_recipe(&TRAVERSAL_SOURCE_ACTIONS),
            &src,
            &dst,
            noop_progress,
        );
        assert!(matches!(result, Err(ExecutorError::PathTraversal { .. })));

        let _ = fs::remove_dir_all(&tmp);
    }

    /// Rejects backslash-style path traversal in the `to` field.
    ///
    /// Windows-style backslash separators can bypass naive forward-slash-only
    /// traversal checks. The boundary must normalise both separator styles.
    #[test]
    fn executor_rejects_backslash_traversal() {
        let tmp = std::env::temp_dir().join("cnc-exec-traversal-backslash");
        let _ = fs::remove_dir_all(&tmp);
        let src = tmp.join("source");
        let dst = tmp.join("content");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dst).unwrap();

        fs::write(src.join("allies.mix"), b"allies-data").unwrap();

        let result = execute_recipe(
            &make_recipe(&TRAVERSAL_BACKSLASH_ACTIONS),
            &src,
            &dst,
            noop_progress,
        );
        assert!(matches!(result, Err(ExecutorError::PathTraversal { .. })));

        let _ = fs::remove_dir_all(&tmp);
    }

    /// Rejects absolute paths in the `to` field of a Copy action.
    ///
    /// An absolute destination path bypasses the content root entirely,
    /// allowing writes to arbitrary filesystem locations.
    #[test]
    fn executor_rejects_absolute_path_in_copy() {
        let tmp = std::env::temp_dir().join("cnc-exec-traversal-absolute");
        let _ = fs::remove_dir_all(&tmp);
        let src = tmp.join("source");
        let dst = tmp.join("content");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dst).unwrap();

        fs::write(src.join("allies.mix"), b"allies-data").unwrap();

        let result = execute_recipe(
            &make_recipe(&TRAVERSAL_ABSOLUTE_ACTIONS),
            &src,
            &dst,
            noop_progress,
        );
        assert!(matches!(result, Err(ExecutorError::PathTraversal { .. })));

        let _ = fs::remove_dir_all(&tmp);
    }

    // ── Error Display messages ──────────────────────────────────────

    /// Display impl for MixNotFound includes the archive path.
    ///
    /// User-facing error messages must identify which file was missing so
    /// the user can diagnose the problem without reading source code.
    #[test]
    fn executor_error_display_mix_not_found() {
        let err = ExecutorError::MixNotFound {
            path: PathBuf::from("source/main.mix"),
        };
        let msg = err.to_string();
        assert!(msg.contains("source/main.mix"), "message was: {msg}");
    }

    /// Display impl for MixEntryNotFound includes both archive and entry.
    ///
    /// When a specific entry is missing from a MIX archive the message must
    /// name both the archive and the entry for actionable diagnostics.
    #[test]
    fn executor_error_display_mix_entry_not_found() {
        let err = ExecutorError::MixEntryNotFound {
            archive: "main.mix".to_string(),
            entry: "conquer.mix".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("main.mix"), "message was: {msg}");
        assert!(msg.contains("conquer.mix"), "message was: {msg}");
    }

    /// Display impl for PathTraversal includes the offending path and detail.
    ///
    /// Security-relevant errors must expose enough context in the message for
    /// audit logging without requiring structured error inspection.
    #[test]
    fn executor_error_display_path_traversal() {
        let err = ExecutorError::PathTraversal {
            path: "../../etc/passwd".to_string(),
            detail: "escapes boundary".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("../../etc/passwd"), "message was: {msg}");
        assert!(msg.contains("escapes boundary"), "message was: {msg}");
    }

    /// Display impl for SourceFileNotFound includes the file path.
    ///
    /// Missing-file errors must identify the path so callers can distinguish
    /// which source file was absent in multi-action recipes.
    #[test]
    fn executor_error_display_source_file_not_found() {
        let err = ExecutorError::SourceFileNotFound(PathBuf::from("missing/file.dat"));
        let msg = err.to_string();
        assert!(msg.contains("missing/file.dat"), "message was: {msg}");
    }

    // ── ExtractZip error cases ──────────────────────────────────────

    static ZIP_MISSING_FILES: [FileMapping; 1] = [FileMapping {
        from: "nonexistent.zip/entry.dat",
        to: "out.dat",
    }];
    static ZIP_MISSING_ACTIONS: [InstallAction; 1] = [InstallAction::ExtractZip {
        entries: &ZIP_MISSING_FILES,
    }];

    /// ExtractZip fails with SourceFileNotFound when the ZIP does not exist.
    ///
    /// When neither a direct file nor a containing ZIP archive can be found
    /// in the source tree, the executor must report a clear not-found error
    /// rather than silently skipping the entry.
    #[test]
    fn executor_extract_zip_missing_source() {
        let tmp = std::env::temp_dir().join("cnc-exec-zip-missing");
        let _ = fs::remove_dir_all(&tmp);
        let src = tmp.join("source");
        let dst = tmp.join("content");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dst).unwrap();

        let result = execute_recipe(
            &make_recipe(&ZIP_MISSING_ACTIONS),
            &src,
            &dst,
            noop_progress,
        );
        assert!(matches!(result, Err(ExecutorError::SourceFileNotFound(..))));

        let _ = fs::remove_dir_all(&tmp);
    }

    // ── First error stops execution ─────────────────────────────────

    static STOP_COPY_FILES: [FileMapping; 1] = [FileMapping {
        from: "should_not_exist.mix",
        to: "should_not_exist.mix",
    }];
    static STOP_ACTIONS: [InstallAction; 2] = [
        // Action 0: ExtractMix from a nonexistent archive — will fail.
        InstallAction::ExtractMix {
            source_mix: "nonexistent.mix",
            entries: &MIX_MISSING_ENTRIES,
        },
        // Action 1: Copy — should never run.
        InstallAction::Copy {
            files: &STOP_COPY_FILES,
        },
    ];

    /// Execution halts on the first failing action without running later ones.
    ///
    /// Continuing past a failed action could leave content in an inconsistent
    /// state. The executor must short-circuit and return the first error,
    /// leaving subsequent actions unattempted.
    #[test]
    fn executor_stops_on_first_error() {
        let tmp = std::env::temp_dir().join("cnc-exec-stop-first");
        let _ = fs::remove_dir_all(&tmp);
        let src = tmp.join("source");
        let dst = tmp.join("content");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dst).unwrap();

        // Place the file that action 1 would copy, so we can verify it
        // was never copied (proving action 1 was not attempted).
        fs::write(src.join("should_not_exist.mix"), b"action2-data").unwrap();

        let result = execute_recipe(&make_recipe(&STOP_ACTIONS), &src, &dst, noop_progress);
        assert!(result.is_err());

        // Action 2's output must not exist — it was never attempted.
        assert!(!dst.join("should_not_exist.mix").exists());

        let _ = fs::remove_dir_all(&tmp);
    }
}
