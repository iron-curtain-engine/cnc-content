// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025-present Iron Curtain contributors

//! FlashGet-style segmented parallel HTTP download.
//!
//! Splits a file into byte-range segments and assigns each segment to a
//! different mirror, aggregating bandwidth from all mirrors simultaneously.

use std::fs;
use std::io::{Read, Write};
use std::path::Path;

use super::mirror::{make_agent, parallel_download};

/// Minimum segment size (1 MB). Segments smaller than this don't benefit
/// from parallelism — HTTP request overhead dominates.
const MIN_SEGMENT_SIZE: u64 = 1024 * 1024;

/// Downloads a file by splitting it into segments and assigning each segment
/// to a different mirror via HTTP Range requests. Returns the SHA-1 hex
/// digest of the assembled file, computed inline during segment assembly.
///
/// ## How it works (FlashGet / Internet Download Manager strategy)
///
/// 1. The file is divided into N segments (one per mirror, minimum 1 MB each).
/// 2. Each thread fetches its assigned byte range via `Range: bytes=start-end`.
/// 3. Segments are written to individual temp files, then assembled in order
///    while a SHA-1 hasher consumes every byte (fused assembly + verification).
/// 4. If a mirror fails mid-segment, another mirror takes over the remaining
///    bytes of that segment.
///
/// This aggregates bandwidth from all mirrors simultaneously — a file served
/// by 5 mirrors at 10 MB/s each downloads at ~50 MB/s total.
///
/// ## Fallback
///
/// If the first mirror returns HTTP 200 (no Range support) instead of 206,
/// we fall back to `parallel_download` (mirror racing) since segmented
/// download is impossible without Range support.
pub(super) fn segmented_download(
    urls: &[String],
    dest: &Path,
    file_size: u64,
    on_bytes: &mut dyn FnMut(u64, Option<u64>),
) -> Result<String, String> {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::Arc;

    let mirror_count = urls.len();
    if mirror_count == 0 {
        return Err("no mirrors available".to_string());
    }

    // Probe first mirror for Range support before committing to segmented.
    if !probe_range_support(urls.first().map(|s| s.as_str()).unwrap_or("")) {
        // Server doesn't support Range requests — fall back to mirror racing.
        return parallel_download(urls, dest, file_size, on_bytes);
    }

    // Split into segments: one per mirror, minimum MIN_SEGMENT_SIZE each.
    let effective_mirrors = std::cmp::min(
        mirror_count,
        std::cmp::max(1, (file_size / MIN_SEGMENT_SIZE) as usize),
    );
    let segment_size = file_size / effective_mirrors as u64;
    let segments: Vec<(u64, u64, usize)> = (0..effective_mirrors)
        .map(|i| {
            let start = i as u64 * segment_size;
            let end = if i == effective_mirrors - 1 {
                file_size.saturating_sub(1) // Last segment gets the remainder.
            } else {
                ((i as u64 + 1) * segment_size).saturating_sub(1)
            };
            (start, end, i)
        })
        .collect();

    // Shared progress tracking across all threads.
    let total_downloaded = Arc::new(AtomicU64::new(0));
    let failed = Arc::new(AtomicBool::new(false));

    // Segment temp files: dest.seg.0, dest.seg.1, etc.
    let seg_paths: Vec<std::path::PathBuf> = (0..effective_mirrors)
        .map(|i| dest.with_extension(format!("seg.{i}")))
        .collect();

    let result: Result<(), String> = std::thread::scope(|scope| {
        let handles: Vec<_> = segments
            .iter()
            .map(|&(start, end, seg_idx)| {
                let url = urls
                    .get(seg_idx % mirror_count)
                    .cloned()
                    .unwrap_or_default();
                let seg_path = seg_paths
                    .get(seg_idx)
                    .cloned()
                    .unwrap_or_else(|| dest.with_extension(format!("seg.{seg_idx}")));
                let total_downloaded = Arc::clone(&total_downloaded);
                let failed = Arc::clone(&failed);

                // Collect fallback mirror URLs for retry (excluding primary).
                let fallback_urls: Vec<String> = urls
                    .iter()
                    .enumerate()
                    .filter(|&(i, _)| i != seg_idx % mirror_count)
                    .map(|(_, u)| u.clone())
                    .collect();

                scope.spawn(move || {
                    // Try primary mirror first, then fall back to others.
                    let result = download_segment(&url, &seg_path, start, end, &total_downloaded);

                    match result {
                        Ok(()) => Ok(()),
                        Err(primary_err) => {
                            // Primary mirror failed — try fallbacks.
                            for fallback_url in &fallback_urls {
                                if failed.load(Ordering::Relaxed) {
                                    break;
                                }
                                if download_segment(
                                    fallback_url,
                                    &seg_path,
                                    start,
                                    end,
                                    &total_downloaded,
                                )
                                .is_ok()
                                {
                                    return Ok(());
                                }
                            }
                            failed.store(true, Ordering::Relaxed);
                            Err(primary_err)
                        }
                    }
                })
            })
            .collect();

        // Wait for all threads and report progress periodically.
        let mut last_reported: u64 = 0;
        let mut all_ok = true;
        let mut last_error = String::new();

        for handle in handles {
            match handle.join() {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    all_ok = false;
                    last_error = e;
                }
                Err(_) => {
                    all_ok = false;
                    last_error = "segment download thread panicked".to_string();
                }
            }
        }

        // Final progress report.
        let final_bytes = total_downloaded.load(Ordering::Relaxed);
        if final_bytes > last_reported {
            last_reported = final_bytes;
            on_bytes(last_reported, Some(file_size));
        }
        let _ = last_reported; // suppress unused after final update

        if !all_ok {
            return Err(last_error);
        }
        Ok(())
    });

    // Report final progress before assembly.
    let total = total_downloaded.load(Ordering::Relaxed);
    on_bytes(total, Some(file_size));

    if let Err(e) = result {
        // Clean up segment files on failure.
        for p in &seg_paths {
            let _ = fs::remove_file(p);
        }
        return Err(e);
    }

    // Assemble segments into the final file, computing SHA-1 during the copy.
    let sha1_hex =
        assemble_segments(&seg_paths, dest).map_err(|e| format!("segment assembly failed: {e}"))?;

    // Clean up segment temp files.
    for p in &seg_paths {
        let _ = fs::remove_file(p);
    }

    Ok(sha1_hex)
}

/// Probes whether a server supports HTTP Range requests by sending a
/// small Range request and checking for 206 Partial Content.
fn probe_range_support(url: &str) -> bool {
    let result = make_agent().get(url).header("Range", "bytes=0-0").call();

    match result {
        Ok(resp) => resp.status().as_u16() == 206,
        Err(_) => false,
    }
}

/// Downloads a single byte-range segment to a file.
///
/// Uses HTTP Range header to request exactly `start..=end` bytes from
/// the mirror. Tracks cumulative progress via the shared atomic counter.
fn download_segment(
    url: &str,
    dest: &Path,
    start: u64,
    end: u64,
    total_downloaded: &std::sync::atomic::AtomicU64,
) -> Result<(), String> {
    use std::sync::atomic::Ordering;

    let range_header = format!("bytes={start}-{end}");
    let expected_len = end.saturating_sub(start).saturating_add(1);

    let resp = make_agent()
        .get(url)
        .header("Range", &range_header)
        .call()
        .map_err(|e| format!("segment {start}-{end} from {url}: {e}"))?;

    let status = resp.status().as_u16();
    if status != 206 {
        return Err(format!(
            "segment {start}-{end}: expected HTTP 206, got {status}"
        ));
    }

    let mut body = resp.into_body().into_reader();
    let mut file = fs::File::create(dest)
        .map_err(|e| format!("create segment file {}: {e}", dest.display()))?;
    let mut buf = [0u8; 65536];
    let mut written: u64 = 0;

    loop {
        let remaining = expected_len.saturating_sub(written);
        if remaining == 0 {
            break;
        }
        let to_read = std::cmp::min(remaining as usize, buf.len());
        let n = body
            .read(buf.get_mut(..to_read).unwrap_or(&mut []))
            .map_err(|e| format!("segment {start}-{end} read: {e}"))?;
        if n == 0 {
            break;
        }
        file.write_all(buf.get(..n).unwrap_or(&[]))
            .map_err(|e| format!("segment {start}-{end} write: {e}"))?;
        written = written.saturating_add(n as u64);
        total_downloaded.fetch_add(n as u64, Ordering::Relaxed);
    }

    file.flush()
        .map_err(|e| format!("segment {start}-{end} flush: {e}"))?;

    if written != expected_len {
        return Err(format!(
            "segment {start}-{end}: expected {expected_len} bytes, got {written}"
        ));
    }

    Ok(())
}

/// Assembles ordered segment files into the final output file, computing
/// SHA-1 inline during the copy.
///
/// Reads each segment sequentially and appends to the destination while
/// feeding every byte to a SHA-1 hasher. This fuses what were previously
/// two separate passes (assembly + verification) into one. Returns the
/// SHA-1 hex digest of the assembled file.
fn assemble_segments(
    seg_paths: &[std::path::PathBuf],
    dest: &Path,
) -> Result<String, std::io::Error> {
    use sha1::Digest;

    let mut output = fs::File::create(dest)?;
    let mut buf = [0u8; 65536];
    let mut hasher = sha1::Sha1::new();

    for seg_path in seg_paths {
        let mut seg_file = fs::File::open(seg_path)?;
        loop {
            let n = seg_file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            let slice = buf.get(..n).unwrap_or(&[]);
            hasher.update(slice);
            output.write_all(slice)?;
        }
    }

    output.flush()?;
    Ok(crate::verify::hex_encode(hasher.finalize().as_slice()))
}
