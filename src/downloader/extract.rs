// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025-present Iron Curtain contributors

//! Post-download extraction helpers — ZIP, torrent archive output, ISO disc image processing.
//!
//! Split from downloader/mod.rs to keep HTTP download logic separate from
//! extraction and recipe execution. All functions in this module operate on
//! files already on disk (after the download step completes).
//!
//! ## Security
//!
//! extract_zip enforces strict path boundaries via strict_path::PathBoundary
//! to prevent Zip Slip (CVE-2018-1000178) and archive bomb attacks.

use std::fs;
use std::io;
use std::path::Path;

use super::{DownloadError, DownloadProgress};
#[cfg(feature = "torrent")]
use crate::DownloadPackage;

#[cfg(feature = "torrent")]
pub(super) fn extract_torrent_output(
    archive_dir: &Path,
    content_root: &Path,
    package: &DownloadPackage,
    on_progress: &mut (dyn FnMut(DownloadProgress) + Send),
) -> Result<usize, DownloadError> {
    let mut total_files = 0;

    // PathBoundary ensures raw file copies cannot escape content_root.
    // ZIP and ISO paths are validated by their own extraction code;
    // this boundary covers loose files (raw .mix, .aud, etc.).
    let boundary = strict_path::PathBoundary::<()>::try_new_create(content_root).map_err(|e| {
        DownloadError::Zip {
            detail: format!("failed to create content boundary: {e}"),
        }
    })?;

    // Collect downloadable files from the archive directory.
    let entries: Vec<_> = fs::read_dir(archive_dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .collect();

    for entry in &entries {
        let path = entry.path();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        match ext.as_str() {
            "zip" => {
                total_files += extract_zip(&path, content_root, on_progress)?;
            }
            "iso" => {
                // ISO disc images are processed through the recipe system.
                // Identify which source this ISO represents, then run its recipes.
                total_files += extract_iso_via_recipes(&path, content_root, package, on_progress)?;
            }
            _ => {
                // Raw files (e.g. loose .mix, .aud): copy into content_root.
                // Path::file_name() strips directory components; strict-path
                // additionally guards against ADS, 8.3 names, and other
                // platform-specific edge cases (defense-in-depth).
                let name = path.file_name().and_then(|n| n.to_str()).ok_or_else(|| {
                    DownloadError::Zip {
                        detail: format!("non-UTF-8 filename in torrent output: {}", path.display()),
                    }
                })?;
                let dest = boundary.strict_join(name).map_err(|e| DownloadError::Zip {
                    detail: format!("blocked path in torrent output \"{name}\": {e}"),
                })?;
                let mut src_file = fs::File::open(&path)?;
                let mut dst_file = dest.create_file()?;
                io::copy(&mut src_file, &mut dst_file)?;
                total_files += 1;
            }
        }
    }

    // Also recurse one level into subdirectories — Archive.org torrents
    // sometimes nest files in a subdirectory named after the item.
    let subdirs: Vec<_> = fs::read_dir(archive_dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .collect();

    for subdir in &subdirs {
        total_files += extract_torrent_output(&subdir.path(), content_root, package, on_progress)?;
    }

    Ok(total_files)
}

#[cfg(feature = "torrent")]
/// Extracts game content from an ISO disc image using the recipe system.
///
/// Identifies which source the ISO corresponds to, then runs the matching
/// install recipes to extract the correct files into `content_root`.
fn extract_iso_via_recipes(
    iso_path: &Path,
    content_root: &Path,
    package: &DownloadPackage,
    on_progress: &mut (dyn FnMut(DownloadProgress) + Send),
) -> Result<usize, DownloadError> {
    // Try to identify this ISO as a known source.
    let source_id = crate::verify::identify_source(iso_path);

    let source_id = match source_id {
        Some(id) => id,
        None => {
            // Can't identify — try all sources that provide the packages
            // this download covers. Use the first one that has recipes.
            let mut found = None;
            for &pkg_id in &package.provides {
                let pkg = crate::package(pkg_id).ok_or_else(|| DownloadError::Io {
                    source: io::Error::other(format!("no package definition for {pkg_id:?}")),
                })?;
                for &src_id in pkg.sources {
                    if crate::recipe(src_id, pkg_id).is_some() {
                        found = Some(src_id);
                        break;
                    }
                }
                if found.is_some() {
                    break;
                }
            }
            match found {
                Some(id) => id,
                None => return Ok(0), // no recipes available
            }
        }
    };

    let mut files = 0;
    for &pkg_id in &package.provides {
        if let Some(recipe) = crate::recipe(source_id, pkg_id) {
            on_progress(DownloadProgress::Extracting {
                entry: format!(
                    "recipe: {} from {}",
                    pkg_id_label(pkg_id),
                    source_label(source_id)
                ),
                index: files,
                total: package.provides.len(),
            });
            crate::executor::execute_recipe(recipe, iso_path, content_root, |_| {}).map_err(
                |e| DownloadError::Zip {
                    detail: format!("recipe execution failed for {:?}: {e}", pkg_id),
                },
            )?;
            files += recipe.actions.len();
        }
    }

    Ok(files)
}

#[cfg(feature = "torrent")]
fn pkg_id_label(id: crate::PackageId) -> &'static str {
    crate::package(id).map(|p| p.title).unwrap_or("(unknown)")
}

#[cfg(feature = "torrent")]
fn source_label(id: crate::SourceId) -> &'static str {
    crate::source(id).map(|s| s.title).unwrap_or("(unknown)")
}

/// Maximum uncompressed size per ZIP entry (1 GB).
///
/// Prevents archive bombs (zip bombs) where a small compressed file expands
/// to fill all available disk. C&C game content files are at most ~700 MB
/// (full disc ISOs), so 1 GB per file is generous.
pub(super) const MAX_ENTRY_UNCOMPRESSED: u64 = 1_073_741_824;

/// Maximum total uncompressed size across all ZIP entries (5 GB).
///
/// An entire game's content (base + expansion + music + movies) is under 2 GB.
/// 5 GB provides headroom for future content without enabling abuse.
pub(super) const MAX_TOTAL_UNCOMPRESSED: u64 = 5_368_709_120;

/// Maximum number of entries allowed in a ZIP archive (100,000).
///
/// C&C content packages contain at most ~200 files. 100K is generous enough
/// to handle any legitimate archive while preventing entry-count bombs that
/// exhaust memory building the central directory.
pub(super) const MAX_ZIP_ENTRIES: usize = 100_000;

/// Extracts a ZIP archive into `content_root` with path traversal protection
/// and archive bomb mitigation.
///
/// Returns the number of files extracted. Directory entries are skipped.
///
/// ## Security
///
/// - **Zip Slip**: [`strict_path::PathBoundary`] prevents entry names from
///   escaping `content_root` via `../` traversal (CVE-2018-1000178).
/// - **Archive bombs**: Per-entry and total uncompressed size limits prevent
///   a small ZIP from expanding to fill disk. Entry count is also limited.
pub fn extract_zip(
    zip_path: &Path,
    content_root: &Path,
    on_progress: &mut (dyn FnMut(DownloadProgress) + Send),
) -> Result<usize, DownloadError> {
    let file = fs::File::open(zip_path)?;
    let mut archive =
        zip::ZipArchive::new(io::BufReader::new(file)).map_err(|e| DownloadError::Zip {
            detail: e.to_string(),
        })?;

    // Reject archives with too many entries (memory exhaustion via central directory).
    if archive.len() > MAX_ZIP_ENTRIES {
        return Err(DownloadError::Zip {
            detail: format!(
                "archive has {} entries (max {})",
                archive.len(),
                MAX_ZIP_ENTRIES
            ),
        });
    }

    // PathBoundary ensures ZIP entry names (untrusted, from network) cannot
    // escape content_root via path traversal (Zip Slip — CVE-2018-1000178).
    let boundary = strict_path::PathBoundary::<()>::try_new_create(content_root).map_err(|e| {
        DownloadError::Zip {
            detail: format!("failed to create content boundary: {e}"),
        }
    })?;

    let total = archive.len();
    let mut files = 0;
    let mut total_uncompressed: u64 = 0;

    for i in 0..total {
        let mut entry = archive.by_index(i).map_err(|e| DownloadError::Zip {
            detail: e.to_string(),
        })?;

        let archive_entry_name = entry.name().to_string();
        if archive_entry_name.ends_with('/') {
            continue;
        }

        // Archive bomb check: per-entry size limit.
        let declared_size = entry.size();
        if declared_size > MAX_ENTRY_UNCOMPRESSED {
            return Err(DownloadError::Zip {
                detail: format!(
                    "entry \"{archive_entry_name}\" declares {declared_size} bytes uncompressed \
                     (max {MAX_ENTRY_UNCOMPRESSED})"
                ),
            });
        }

        // Archive bomb check: total uncompressed size limit.
        total_uncompressed = total_uncompressed.saturating_add(declared_size);
        if total_uncompressed > MAX_TOTAL_UNCOMPRESSED {
            return Err(DownloadError::Zip {
                detail: format!(
                    "total uncompressed size exceeds {MAX_TOTAL_UNCOMPRESSED} bytes — \
                     possible archive bomb"
                ),
            });
        }

        on_progress(DownloadProgress::Extracting {
            entry: archive_entry_name.clone(),
            index: i,
            total,
        });

        // Validate the untrusted archive entry name against our boundary.
        let dest = boundary
            .strict_join(&archive_entry_name)
            .map_err(|e| DownloadError::Zip {
                detail: format!(
                    "blocked path traversal in ZIP entry \"{archive_entry_name}\": {e}"
                ),
            })?;

        dest.create_parent_dir_all()?;
        let mut out = dest.create_file()?;
        io::copy(&mut entry, &mut out)?;
        files += 1;
    }

    Ok(files)
}
