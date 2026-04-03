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
use std::io;
use std::path::{Path, PathBuf};

use strict_path::PathBoundary;
use thiserror::Error;

use crate::actions::InstallAction;
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

            InstallAction::ExtractBig {
                source_big,
                entries,
            } => {
                let big = bounded_path(&source, source_big)?;
                let (written, bytes) =
                    extract_from_big(&big, &content, entries, source_big, &mut on_progress)?;
                files_written += written;
                total_bytes += bytes;
            }

            InstallAction::ExtractMeg {
                source_meg,
                entries,
            } => {
                let meg = bounded_path(&source, source_meg)?;
                let (written, bytes) =
                    extract_from_meg(&meg, &content, entries, source_meg, &mut on_progress)?;
                files_written += written;
                total_bytes += bytes;
            }

            InstallAction::ExtractBagIdx {
                source_idx,
                source_bag,
                entries,
            } => {
                let (written, bytes) = extract_from_bag_idx(
                    &source,
                    &content,
                    source_idx,
                    source_bag,
                    entries,
                    &mut on_progress,
                )?;
                files_written += written;
                total_bytes += bytes;
            }
        }
    }

    on_progress(InstallProgress::Completed {
        files_written,
        total_bytes,
    });

    Ok(())
}

// ── Internal extraction helpers ───────────────────────────────────────

mod extract;
use self::extract::{
    bounded_path, describe_action, extract_from_bag_idx, extract_from_big, extract_from_iscab,
    extract_from_meg, extract_from_mix, extract_from_zip, extract_raw_entry,
};

#[cfg(test)]
mod tests;
