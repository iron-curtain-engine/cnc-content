// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! Archive extraction helpers for the install action executor.
//!
//! All functions operate on pre-validated `PathBoundary` / `StrictPath` values
//! produced by `execute_recipe` — path security is enforced at the entry point,
//! not repeated per helper. Each helper is responsible only for reading the
//! correct archive format and writing to the provided content boundary.
//!
//! Split from `executor/mod.rs` to keep the public API (`execute_recipe`,
//! `InstallProgress`, `ExecutorError`) separate from the format-specific
//! extraction implementations.

use std::io::{self, Read, Seek, SeekFrom};
use std::path::PathBuf;

use strict_path::{PathBoundary, StrictPath};

use super::{ExecutorError, InstallProgress};
use crate::actions::{FileMapping, InstallAction, RawExtractEntry};

/// Validates a subpath stays within a boundary, producing a descriptive
/// error on traversal attempts.
pub(super) fn bounded_path(
    boundary: &PathBoundary<()>,
    subpath: &str,
) -> Result<StrictPath, ExecutorError> {
    boundary
        .strict_join(subpath)
        .map_err(|e| ExecutorError::PathTraversal {
            path: subpath.to_string(),
            detail: e.to_string(),
        })
}

/// Extracts entries from a MIX archive into the content boundary.
pub(super) fn extract_from_mix(
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
pub(super) fn extract_raw_entry(
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
pub(super) fn extract_from_iscab(
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
    let vol_refs: Vec<(u32, &std::path::Path)> =
        vol_paths.iter().map(|(i, p)| (*i, p.as_path())).collect();

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
pub(super) fn extract_from_zip(
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

/// Extracts entries from a BIG archive (C&C Generals / Zero Hour).
///
/// Uses `cnc_formats::big::BigArchiveReader` for streaming reads. Entry
/// lookups are case-insensitive to handle Windows-style path conventions
/// in BIG archives.
pub(super) fn extract_from_big(
    big_path: &StrictPath,
    content: &PathBoundary<()>,
    entries: &[FileMapping],
    archive_name: &str,
    on_progress: &mut impl FnMut(InstallProgress),
) -> Result<(usize, u64), ExecutorError> {
    if !big_path.exists() {
        return Err(ExecutorError::SourceFileNotFound {
            path: big_path.clone().unstrict(),
        });
    }

    let file = big_path.open_file()?;
    let reader = io::BufReader::new(file);
    let mut archive = cnc_formats::big::BigArchiveReader::open(reader)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;

    let mut files_written = 0;
    let mut total_bytes: u64 = 0;

    for mapping in entries {
        let data = archive
            .read(mapping.from)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;

        let data = data.ok_or_else(|| ExecutorError::ZipError {
            detail: format!("BIG entry '{}' not found in {}", mapping.from, archive_name),
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

/// Extracts entries from a MEG archive (C&C Remastered / Petroglyph).
///
/// Uses `cnc_formats::meg::MegArchiveReader` for streaming reads. Entry
/// lookups are case-insensitive.
pub(super) fn extract_from_meg(
    meg_path: &StrictPath,
    content: &PathBoundary<()>,
    entries: &[FileMapping],
    archive_name: &str,
    on_progress: &mut impl FnMut(InstallProgress),
) -> Result<(usize, u64), ExecutorError> {
    if !meg_path.exists() {
        return Err(ExecutorError::SourceFileNotFound {
            path: meg_path.clone().unstrict(),
        });
    }

    let file = meg_path.open_file()?;
    let reader = io::BufReader::new(file);
    let mut archive = cnc_formats::meg::MegArchiveReader::open(reader)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;

    let mut files_written = 0;
    let mut total_bytes: u64 = 0;

    for mapping in entries {
        let data = archive
            .read(mapping.from)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;

        let data = data.ok_or_else(|| ExecutorError::ZipError {
            detail: format!("MEG entry '{}' not found in {}", mapping.from, archive_name),
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

/// Extracts audio entries from a BAG/IDX pair (Red Alert 2 / Yuri's Revenge).
///
/// Parses the small .idx index file to locate entries, then reads data from
/// the .bag file at the specified offsets. This avoids loading the entire
/// .bag file (which can be hundreds of MB) into memory.
pub(super) fn extract_from_bag_idx(
    source: &PathBoundary<()>,
    content: &PathBoundary<()>,
    idx_path: &str,
    bag_path: &str,
    entries: &[FileMapping],
    on_progress: &mut impl FnMut(InstallProgress),
) -> Result<(usize, u64), ExecutorError> {
    // Read and parse the index file (small — typically a few KB).
    let idx_strict = bounded_path(source, idx_path)?;
    let idx_data = idx_strict.read()?;
    let index = cnc_formats::bag_idx::IdxFile::parse(&idx_data)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;

    // Open the bag file for seeking — not loaded into memory.
    let bag_strict = bounded_path(source, bag_path)?;
    let mut bag_file = bag_strict.open_file()?;

    let mut files_written = 0;
    let mut total_bytes: u64 = 0;

    for mapping in entries {
        let entry = index
            .get(mapping.from)
            .ok_or_else(|| ExecutorError::ZipError {
                detail: format!("BAG/IDX entry '{}' not found in {}", mapping.from, idx_path),
            })?;

        // Seek to the entry's offset in the .bag file and read its data.
        bag_file.seek(SeekFrom::Start(entry.offset as u64))?;
        let mut buf = vec![0u8; entry.size as usize];
        bag_file.read_exact(&mut buf)?;

        let dst = bounded_path(content, mapping.to)?;
        dst.create_parent_dir_all()?;
        dst.write(&buf)?;

        let bytes = buf.len() as u64;
        on_progress(InstallProgress::FileWritten {
            path: dst.unstrict(),
            bytes,
        });
        files_written += 1;
        total_bytes += bytes;
    }

    Ok((files_written, total_bytes))
}

/// Extracts files directly from an ISO 9660 disc image.
///
/// Opens the ISO, locates each named file within its filesystem using
/// case-insensitive lookup, and writes the data to the content root.
/// Used for extracting loose files (e.g. VQA movies, standalone MIX
/// archives) from disc images without mounting.
pub(super) fn extract_from_iso(
    iso_path: &StrictPath,
    content: &PathBoundary<()>,
    entries: &[FileMapping],
    archive_name: &str,
    on_progress: &mut impl FnMut(InstallProgress),
) -> Result<(usize, u64), ExecutorError> {
    if !iso_path.exists() {
        return Err(ExecutorError::IsoNotFound {
            path: iso_path.clone().unstrict(),
        });
    }

    let file = iso_path.open_file()?;
    let reader = io::BufReader::new(file);
    let mut archive = cnc_formats::iso9660::Iso9660ArchiveReader::open(reader)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;

    let mut files_written = 0;
    let mut total_bytes: u64 = 0;

    for mapping in entries {
        let data = archive
            .read(mapping.from)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;

        let data = data.ok_or_else(|| ExecutorError::IsoEntryNotFound {
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

/// Extracts entries from a MIX archive nested inside an ISO 9660 disc image.
///
/// Opens the ISO, locates the MIX file inside it as a bounded entry reader
/// (zero extraction to disk), then opens the MIX through the chained reader
/// and extracts the requested entries. This enables two-level extraction
/// from disc images — e.g. Red Alert's `INSTALL/REDALERT.MIX` on the
/// Allied/Soviet disc ISOs.
pub(super) fn extract_mix_from_iso(
    iso_path: &StrictPath,
    content: &PathBoundary<()>,
    iso_mix_path: &str,
    entries: &[FileMapping],
    archive_name: &str,
    on_progress: &mut impl FnMut(InstallProgress),
) -> Result<(usize, u64), ExecutorError> {
    if !iso_path.exists() {
        return Err(ExecutorError::IsoNotFound {
            path: iso_path.clone().unstrict(),
        });
    }

    let file = iso_path.open_file()?;
    let reader = io::BufReader::new(file);
    let mut iso = cnc_formats::iso9660::Iso9660ArchiveReader::open(reader)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;

    // Open the MIX file entry within the ISO as a bounded reader.
    // This avoids extracting the MIX to disk — the MIX reader operates
    // directly on the ISO's byte range for that entry.
    let entry_reader = iso
        .open_entry(iso_mix_path)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?
        .ok_or_else(|| ExecutorError::IsoEntryNotFound {
            archive: archive_name.to_string(),
            entry: iso_mix_path.to_string(),
        })?;

    // Chain: ISO entry reader → MIX archive reader.
    // The entry reader already sits on a buffered ISO reader, so no
    // additional BufReader layer is needed.
    let mut mix = cnc_formats::mix::MixArchiveReader::open(entry_reader)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;

    let mix_label = format!("{archive_name}:{iso_mix_path}");
    let mut files_written = 0;
    let mut total_bytes: u64 = 0;

    for mapping in entries {
        let data = mix
            .read(mapping.from)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;

        let data = data.ok_or_else(|| ExecutorError::MixEntryNotFound {
            archive: mix_label.clone(),
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

/// Returns a human-readable description of an install action.
pub(super) fn describe_action(action: &InstallAction) -> String {
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
        InstallAction::ExtractBig {
            source_big,
            entries,
        } => {
            format!(
                "Extracting {} entry/entries from BIG {}",
                entries.len(),
                source_big
            )
        }
        InstallAction::ExtractMeg {
            source_meg,
            entries,
        } => {
            format!(
                "Extracting {} entry/entries from MEG {}",
                entries.len(),
                source_meg
            )
        }
        InstallAction::ExtractBagIdx {
            source_idx,
            entries,
            ..
        } => {
            format!(
                "Extracting {} entry/entries from BAG/IDX {}",
                entries.len(),
                source_idx
            )
        }
        InstallAction::ExtractIso {
            source_iso,
            entries,
        } => {
            format!(
                "Extracting {} entry/entries from ISO {}",
                entries.len(),
                source_iso
            )
        }
        InstallAction::ExtractMixFromIso {
            source_iso,
            iso_mix_path,
            entries,
        } => {
            format!(
                "Extracting {} entry/entries from MIX {} in ISO {}",
                entries.len(),
                iso_mix_path,
                source_iso
            )
        }
    }
}
