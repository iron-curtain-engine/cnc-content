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
    #[error("I/O error during install: {source}")]
    Io {
        #[from]
        source: io::Error,
    },

    #[error("MIX archive not found at source: {path}")]
    MixNotFound { path: PathBuf },

    #[error("MIX entry not found: {entry} in {archive}")]
    MixEntryNotFound { archive: String, entry: String },

    #[error("InstallShield CAB error: {source}")]
    Iscab {
        #[from]
        source: crate::iscab::IscabError,
    },

    #[error("ZIP extraction error: {detail}")]
    ZipError { detail: String },

    #[error("source file not found: {path}")]
    SourceFileNotFound { path: PathBuf },

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
        return Err(ExecutorError::SourceFileNotFound {
            path: src.unstrict(),
        });
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
                return Err(ExecutorError::SourceFileNotFound {
                    path: src.unstrict(),
                });
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
        let zip_rel: String = parts.get(..split_at).unwrap_or(&parts).join("/");
        let zip_strict = bounded_path(source, &zip_rel)?;

        if zip_strict.is_file() && split_at < parts.len() {
            let entry_name: String = parts.get(split_at..).unwrap_or(&[]).join("/");

            let file = zip_strict.open_file()?;
            let mut archive = zip::ZipArchive::new(io::BufReader::new(file)).map_err(|e| {
                ExecutorError::ZipError {
                    detail: e.to_string(),
                }
            })?;

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
                .ok_or_else(|| ExecutorError::ZipError {
                    detail: format!("entry '{entry_name}' not found in {}", zip_rel),
                })?;

            let mut entry = archive
                .by_index(entry_index)
                .map_err(|e| ExecutorError::ZipError {
                    detail: e.to_string(),
                })?;

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
mod tests;
