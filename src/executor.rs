// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! Install action executor — runs a recipe's action sequence against a source.
//!
//! The executor takes a source path and a content root, then processes each
//! [`InstallAction`] in order, reporting progress through a callback channel.

use std::fs;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::actions::{InstallAction, RawExtractEntry};
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
    FileWritten {
        path: PathBuf,
        bytes: u64,
    },
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

    #[error("InstallShield CAB support not yet implemented")]
    IscabNotImplemented,

    #[error("ZIP extraction error: {0}")]
    ZipError(String),

    #[error("source file not found: {0}")]
    SourceFileNotFound(PathBuf),
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
                for mapping in files {
                    let src = source_root.join(mapping.from);
                    let dst = content_root.join(mapping.to);
                    ensure_parent(&dst)?;
                    let bytes = fs::copy(&src, &dst)?;
                    on_progress(InstallProgress::FileWritten {
                        path: dst,
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
                let mix_path = source_root.join(source_mix);
                if !mix_path.exists() {
                    return Err(ExecutorError::MixNotFound { path: mix_path });
                }

                let file = fs::File::open(&mix_path)?;
                let reader = io::BufReader::new(file);
                let mut archive =
                    cnc_formats::mix::MixArchiveReader::open(reader).map_err(|e| {
                        io::Error::new(io::ErrorKind::InvalidData, e.to_string())
                    })?;

                for mapping in entries {
                    let entry_name = mapping.from;

                    let data = archive.read(entry_name).map_err(|e| {
                        io::Error::new(io::ErrorKind::InvalidData, e.to_string())
                    })?;

                    let data = data.ok_or_else(|| ExecutorError::MixEntryNotFound {
                        archive: source_mix.to_string(),
                        entry: entry_name.to_string(),
                    })?;

                    let dst = content_root.join(mapping.to);
                    ensure_parent(&dst)?;
                    fs::write(&dst, &data)?;

                    let bytes = data.len() as u64;
                    on_progress(InstallProgress::FileWritten { path: dst, bytes });
                    files_written += 1;
                    total_bytes += bytes;
                }
            }

            InstallAction::ExtractIscab { .. } => {
                return Err(ExecutorError::IscabNotImplemented);
            }

            InstallAction::ExtractRaw { entries } => {
                for entry in entries {
                    let bytes = extract_raw_entry(source_root, content_root, entry)?;
                    on_progress(InstallProgress::FileWritten {
                        path: content_root.join(entry.to),
                        bytes,
                    });
                    files_written += 1;
                    total_bytes += bytes;
                }
            }

            InstallAction::ExtractZip { entries } => {
                // ZIP extraction requires the `zip` crate — stubbed for now.
                // The downloaded ZIP path is expected in the content_root temp area.
                let _ = entries;
                return Err(ExecutorError::ZipError(
                    "ZIP extraction not yet implemented".into(),
                ));
            }

            InstallAction::Delete { path } => {
                let target = content_root.join(path);
                if target.exists() {
                    fs::remove_file(&target)?;
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

/// Extracts a raw byte range from a source file.
fn extract_raw_entry(
    source_root: &Path,
    content_root: &Path,
    entry: &RawExtractEntry,
) -> Result<u64, ExecutorError> {
    let src_path = source_root.join(entry.source);
    if !src_path.exists() {
        return Err(ExecutorError::SourceFileNotFound(src_path));
    }

    let mut file = fs::File::open(&src_path)?;
    file.seek(SeekFrom::Start(entry.offset))?;

    let mut buf = vec![0u8; entry.length as usize];
    file.read_exact(&mut buf)?;

    let dst = content_root.join(entry.to);
    ensure_parent(&dst)?;
    fs::write(&dst, &buf)?;

    Ok(entry.length)
}

/// Ensures the parent directory of a path exists.
fn ensure_parent(path: &Path) -> Result<(), io::Error> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(())
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
        InstallAction::ExtractIscab { header, entries, .. } => {
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
