//! Unit and security tests for the HTTP downloader.
//!
//! Covers happy-path downloads, parallel mirror racing, SHA-1 verification,
//! and adversarial inputs (path traversal, mismatched hashes).

use super::*;
use std::io::Write;

mod download;
mod urls;
mod zip_tests;

/// Creates an in-memory ZIP archive and writes it to `dest`.
/// `entries` is a list of `(name, content)` tuples where `name` may
/// contain path traversal sequences for security testing.
pub(super) fn create_test_zip(dest: &Path, entries: &[(&str, &[u8])]) {
    let file = fs::File::create(dest).unwrap();
    let mut writer = zip::ZipWriter::new(file);
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for &(name, data) in entries {
        writer.start_file(name, options).unwrap();
        writer.write_all(data).unwrap();
    }
    writer.finish().unwrap();
}

pub(super) fn noop_progress(_: DownloadProgress) {}
