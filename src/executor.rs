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
                for mapping in *files {
                    let src = source_root.join(mapping.from);
                    let dst = content_root.join(mapping.to);
                    ensure_parent(&dst)?;
                    let bytes = fs::copy(&src, &dst)?;
                    on_progress(InstallProgress::FileWritten { path: dst, bytes });
                    files_written += 1;
                    total_bytes += bytes;
                }
            }

            InstallAction::ExtractMix {
                source_mix,
                entries,
            } => {
                let mix_path = source_root.join(source_mix);
                let (written, bytes) = extract_from_mix(
                    &mix_path,
                    content_root,
                    entries,
                    source_mix,
                    &mut on_progress,
                )?;
                files_written += written;
                total_bytes += bytes;
            }

            InstallAction::ExtractMixFromContent {
                content_mix,
                entries,
            } => {
                // MIX file is in the content root, not the source root.
                let mix_path = content_root.join(content_mix);
                let (written, bytes) = extract_from_mix(
                    &mix_path,
                    content_root,
                    entries,
                    content_mix,
                    &mut on_progress,
                )?;
                files_written += written;
                total_bytes += bytes;
            }

            InstallAction::ExtractIscab { .. } => {
                return Err(ExecutorError::IscabNotImplemented);
            }

            InstallAction::ExtractRaw { entries } => {
                for entry in *entries {
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
                #[cfg(feature = "download")]
                {
                    let _ = entries;
                    // ZIP extraction for install actions is handled by the
                    // downloader pipeline — it extracts the full ZIP and the
                    // actions here are informational only.
                    return Err(ExecutorError::ZipError(
                        "ZIP install actions should be handled by the download pipeline".into(),
                    ));
                }
                #[cfg(not(feature = "download"))]
                {
                    let _ = entries;
                    return Err(ExecutorError::ZipError(
                        "ZIP extraction requires the `download` feature".into(),
                    ));
                }
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

/// Extracts entries from a MIX archive at `mix_path` into `content_root`.
fn extract_from_mix(
    mix_path: &Path,
    content_root: &Path,
    entries: &[crate::actions::FileMapping],
    archive_name: &str,
    on_progress: &mut impl FnMut(InstallProgress),
) -> Result<(usize, u64), ExecutorError> {
    if !mix_path.exists() {
        return Err(ExecutorError::MixNotFound {
            path: mix_path.to_path_buf(),
        });
    }

    let file = fs::File::open(mix_path)?;
    let reader = io::BufReader::new(file);
    let mut archive = cnc_formats::mix::MixArchiveReader::open(reader)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;

    let mut files_written = 0;
    let mut total_bytes: u64 = 0;

    for mapping in entries {
        let entry_name = mapping.from;

        let data = archive
            .read(entry_name)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;

        let data = data.ok_or_else(|| ExecutorError::MixEntryNotFound {
            archive: archive_name.to_string(),
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

    Ok((files_written, total_bytes))
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
            package: crate::PackageId::Base,
            actions,
        }
    }

    // ── Copy action ──────────────────────────────────────────────────

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
}
