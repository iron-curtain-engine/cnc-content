// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! Content downloader — fetches RA1 content from OpenRA mirrors.
//!
//! Supports the `download` feature (requires `ureq` + `zip` crates).

use std::fs;
use std::io::{self, Read, Write};
use std::path::Path;

use thiserror::Error;

use crate::{DownloadId, DownloadPackage};

/// Errors from download operations.
#[derive(Debug, Error)]
pub enum DownloadError {
    #[error("failed to fetch mirror list from {url}: {source}")]
    MirrorListFetch {
        url: String,
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("mirror list is empty")]
    EmptyMirrorList,
    #[error("all {count} mirrors failed; last error: {last_error}")]
    AllMirrorsFailed { count: usize, last_error: String },
    #[error("SHA-1 mismatch: expected {expected}, got {actual}")]
    Sha1Mismatch { expected: String, actual: String },
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("ZIP extraction error: {0}")]
    Zip(String),
}

/// Progress callback events.
#[derive(Debug, Clone)]
pub enum DownloadProgress {
    /// Fetching the mirror list.
    FetchingMirrors { url: String },
    /// Trying a mirror.
    TryingMirror {
        index: usize,
        total: usize,
        url: String,
    },
    /// Download progress (bytes so far).
    Downloading { bytes: u64 },
    /// Download complete, verifying.
    Verifying,
    /// Extracting ZIP.
    Extracting {
        entry: String,
        index: usize,
        total: usize,
    },
    /// All done.
    Complete { files: usize },
}

/// Downloads and extracts a content package from OpenRA mirrors.
///
/// Fetches the mirror list, tries each mirror in order, verifies SHA-1,
/// then extracts the ZIP into `content_root`.
pub fn download_package(
    package: &DownloadPackage,
    content_root: &Path,
    on_progress: &mut dyn FnMut(DownloadProgress),
) -> Result<(), DownloadError> {
    fs::create_dir_all(content_root)?;
    let zip_path = content_root.join(".download.zip.tmp");

    // 1. Fetch mirror list.
    on_progress(DownloadProgress::FetchingMirrors {
        url: package.mirror_list_url.to_string(),
    });
    let mirrors = fetch_mirror_list(package.mirror_list_url)?;

    // 2. Download from first reachable mirror.
    let mut last_error = String::new();
    let mut downloaded = false;
    for (i, mirror) in mirrors.iter().enumerate() {
        on_progress(DownloadProgress::TryingMirror {
            index: i,
            total: mirrors.len(),
            url: mirror.clone(),
        });

        match try_download(mirror, &zip_path, &mut |bytes| {
            on_progress(DownloadProgress::Downloading { bytes });
        }) {
            Ok(_) => {
                downloaded = true;
                break;
            }
            Err(e) => {
                last_error = e.to_string();
            }
        }
    }

    if !downloaded {
        let _ = fs::remove_file(&zip_path);
        return Err(DownloadError::AllMirrorsFailed {
            count: mirrors.len(),
            last_error,
        });
    }

    // 3. Verify SHA-1.
    on_progress(DownloadProgress::Verifying);
    let actual_sha1 = crate::verify::sha1_file(&zip_path, None)?;
    if actual_sha1 != package.sha1 {
        let _ = fs::remove_file(&zip_path);
        return Err(DownloadError::Sha1Mismatch {
            expected: package.sha1.to_string(),
            actual: actual_sha1,
        });
    }

    // 4. Extract ZIP.
    let files = extract_zip(&zip_path, content_root, on_progress)?;
    let _ = fs::remove_file(&zip_path);

    on_progress(DownloadProgress::Complete { files });
    Ok(())
}

/// Downloads all required content that is currently missing.
pub fn download_missing(
    content_root: &Path,
    on_progress: &mut dyn FnMut(DownloadProgress),
) -> Result<(), DownloadError> {
    // QuickInstall covers all three required packages in one download.
    if !crate::is_content_complete(content_root) {
        let pkg = crate::download(DownloadId::QuickInstall);
        download_package(pkg, content_root, on_progress)?;
    }
    Ok(())
}

fn fetch_mirror_list(url: &str) -> Result<Vec<String>, DownloadError> {
    let body = ureq::get(url)
        .call()
        .map_err(|e| DownloadError::MirrorListFetch {
            url: url.to_string(),
            source: Box::new(e),
        })?
        .into_body()
        .read_to_string()
        .map_err(|e| DownloadError::MirrorListFetch {
            url: url.to_string(),
            source: Box::new(e),
        })?;

    let mirrors: Vec<String> = body
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();

    if mirrors.is_empty() {
        return Err(DownloadError::EmptyMirrorList);
    }

    Ok(mirrors)
}

fn try_download(
    url: &str,
    dest: &Path,
    on_bytes: &mut dyn FnMut(u64),
) -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
    let resp = ureq::get(url).call()?;
    let mut body = resp.into_body().into_reader();
    let mut file = fs::File::create(dest)?;
    let mut buf = [0u8; 65536];
    let mut total: u64 = 0;

    loop {
        let n = body.read(&mut buf)?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        total += n as u64;
        on_bytes(total);
    }

    file.flush()?;
    Ok(total)
}

fn extract_zip(
    zip_path: &Path,
    content_root: &Path,
    on_progress: &mut dyn FnMut(DownloadProgress),
) -> Result<usize, DownloadError> {
    let file = fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(io::BufReader::new(file))
        .map_err(|e| DownloadError::Zip(e.to_string()))?;

    let total = archive.len();
    let mut files = 0;

    for i in 0..total {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| DownloadError::Zip(e.to_string()))?;

        let name = entry.name().to_string();
        if name.ends_with('/') {
            continue;
        }

        on_progress(DownloadProgress::Extracting {
            entry: name.clone(),
            index: i,
            total,
        });

        let dest = content_root.join(&name);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }

        let mut out = fs::File::create(&dest)?;
        io::copy(&mut entry, &mut out)?;
        files += 1;
    }

    Ok(files)
}
