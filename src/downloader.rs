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
    // Track which MB we last reported to avoid spamming the callback.
    let mut last_reported_mb: u64 = 0;

    loop {
        let n = body.read(&mut buf)?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        total += n as u64;
        let current_mb = total / (1024 * 1024);
        if current_mb > last_reported_mb {
            last_reported_mb = current_mb;
            on_bytes(total);
        }
    }

    // Final report so callers see the total.
    on_bytes(total);
    file.flush()?;
    Ok(total)
}

/// Extracts a ZIP archive into `content_root`, validating every entry name
/// against a [`strict_path::PathBoundary`] to prevent Zip Slip
/// (CVE-2018-1000178).
fn extract_zip(
    zip_path: &Path,
    content_root: &Path,
    on_progress: &mut dyn FnMut(DownloadProgress),
) -> Result<usize, DownloadError> {
    let file = fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(io::BufReader::new(file))
        .map_err(|e| DownloadError::Zip(e.to_string()))?;

    // PathBoundary ensures ZIP entry names (untrusted, from network) cannot
    // escape content_root via path traversal (Zip Slip — CVE-2018-1000178).
    let boundary = strict_path::PathBoundary::<()>::try_new_create(content_root)
        .map_err(|e| DownloadError::Zip(format!("failed to create content boundary: {e}")))?;

    let total = archive.len();
    let mut files = 0;

    for i in 0..total {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| DownloadError::Zip(e.to_string()))?;

        let archive_entry_name = entry.name().to_string();
        if archive_entry_name.ends_with('/') {
            continue;
        }

        on_progress(DownloadProgress::Extracting {
            entry: archive_entry_name.clone(),
            index: i,
            total,
        });

        // Validate the untrusted archive entry name against our boundary.
        let dest = boundary.strict_join(&archive_entry_name).map_err(|e| {
            DownloadError::Zip(format!(
                "blocked path traversal in ZIP entry \"{archive_entry_name}\": {e}"
            ))
        })?;

        dest.create_parent_dir_all()?;
        let mut out = dest.create_file()?;
        io::copy(&mut entry, &mut out)?;
        files += 1;
    }

    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Creates an in-memory ZIP archive and writes it to `dest`.
    /// `entries` is a list of `(name, content)` tuples where `name` may
    /// contain path traversal sequences for security testing.
    fn create_test_zip(dest: &Path, entries: &[(&str, &[u8])]) {
        let file = fs::File::create(dest).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for &(name, data) in entries {
            writer.start_file(name, options).unwrap();
            writer.write_all(data).unwrap();
        }
        writer.finish().unwrap();
    }

    fn noop_progress(_: DownloadProgress) {}

    // ── Security tests: Zip Slip (CVE-2018-1000178) ─────────────────

    #[test]
    fn extract_zip_rejects_dot_dot_slash_traversal() {
        let tmp = std::env::temp_dir().join("cnc-zip-slip-dotdot");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let zip_path = tmp.join("malicious.zip");
        create_test_zip(&zip_path, &[("../../etc/passwd", b"pwned")]);

        let content_root = tmp.join("content");
        fs::create_dir_all(&content_root).unwrap();

        let result = extract_zip(&zip_path, &content_root, &mut noop_progress);
        assert!(result.is_err(), "should reject ../.. traversal");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("blocked path traversal") || err.contains("Escapes"),
            "error should mention traversal: {err}"
        );

        // The escaped file must NOT exist.
        assert!(
            !tmp.join("etc/passwd").exists(),
            "traversal payload must not be written"
        );

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn extract_zip_rejects_absolute_path_entry() {
        let tmp = std::env::temp_dir().join("cnc-zip-slip-abs");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let zip_path = tmp.join("malicious.zip");
        // Absolute path — should be rejected.
        create_test_zip(&zip_path, &[("/etc/shadow", b"pwned")]);

        let content_root = tmp.join("content");
        fs::create_dir_all(&content_root).unwrap();

        let result = extract_zip(&zip_path, &content_root, &mut noop_progress);
        assert!(result.is_err(), "should reject absolute path entry");

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn extract_zip_rejects_backslash_traversal() {
        let tmp = std::env::temp_dir().join("cnc-zip-slip-backslash");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let zip_path = tmp.join("malicious.zip");
        // Mixed separators — a common evasion technique.
        create_test_zip(&zip_path, &[("..\\..\\etc\\passwd", b"pwned")]);

        let content_root = tmp.join("content");
        fs::create_dir_all(&content_root).unwrap();

        let result = extract_zip(&zip_path, &content_root, &mut noop_progress);
        assert!(result.is_err(), "should reject backslash traversal");

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn extract_zip_accepts_valid_entries() {
        let tmp = std::env::temp_dir().join("cnc-zip-valid");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let zip_path = tmp.join("valid.zip");
        create_test_zip(
            &zip_path,
            &[
                ("allies.mix", b"fake-mix-data"),
                ("expand/expand2.mix", b"fake-expand"),
                ("movies/ally1.vqa", b"fake-vqa"),
            ],
        );

        let content_root = tmp.join("content");
        fs::create_dir_all(&content_root).unwrap();

        let count = extract_zip(&zip_path, &content_root, &mut noop_progress)
            .expect("valid ZIP should extract successfully");
        assert_eq!(count, 3);
        assert!(content_root.join("allies.mix").exists());
        assert!(content_root.join("expand/expand2.mix").exists());
        assert!(content_root.join("movies/ally1.vqa").exists());

        // Verify content is correct.
        assert_eq!(
            fs::read(content_root.join("allies.mix")).unwrap(),
            b"fake-mix-data"
        );

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn extract_zip_rejects_mixed_valid_and_malicious() {
        let tmp = std::env::temp_dir().join("cnc-zip-slip-mixed");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let zip_path = tmp.join("mixed.zip");
        create_test_zip(
            &zip_path,
            &[("good.mix", b"safe"), ("../../evil.txt", b"pwned")],
        );

        let content_root = tmp.join("content");
        fs::create_dir_all(&content_root).unwrap();

        let result = extract_zip(&zip_path, &content_root, &mut noop_progress);
        // The malicious entry should cause failure.
        assert!(
            result.is_err(),
            "should fail on malicious entry in mixed ZIP"
        );

        // The escaped file must not exist.
        assert!(!tmp.join("evil.txt").exists());

        let _ = fs::remove_dir_all(&tmp);
    }
}
