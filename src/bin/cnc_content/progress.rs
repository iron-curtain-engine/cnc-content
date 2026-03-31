// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! Download progress display for the CLI.
//!
//! Renders `DownloadProgress` events as animated terminal progress bars using
//! `indicatif`. The display transitions through phases:
//!
//! 1. **Fetching mirrors** — spinner while the mirror list resolves
//! 2. **Downloading** — progress bar with speed, ETA, and bytes transferred
//! 3. **Verifying** — spinner during SHA-1 hash check
//! 4. **Extracting** — progress bar counting files
//! 5. **Complete** — green checkmark summary
//!
//! All output is written to stderr so stdout remains clean for piping.

use std::time::Instant;

use console::style;
use indicatif::{ProgressBar, ProgressStyle};

use cnc_content::downloader::DownloadProgress;

// ── Style templates ──────────────────────────────────────────────────
//
// indicatif templates use {placeholders} that are filled at render time.
// `wide_bar` expands to fill the terminal width. `binary_bytes` and
// `binary_bytes_per_sec` format as KiB/MiB/GiB automatically.

/// Download progress bar: `  Downloading  ━━━━━━━━━━━━━━  45.2 MiB / 120.3 MiB  2.1 MiB/s  eta 36s`
const DOWNLOAD_STYLE: &str =
    "  {msg}  {wide_bar:.cyan/dim}  {bytes}/{total_bytes}  {binary_bytes_per_sec}  eta {eta}";

/// Download bar when total size is unknown: `  Downloading  ━━━╸  45.2 MiB  2.1 MiB/s`
const DOWNLOAD_UNKNOWN_STYLE: &str =
    "  {msg}  {wide_bar:.cyan/dim}  {bytes}  {binary_bytes_per_sec}";

/// Extraction progress: `  Extracting  ━━━━━━━━━━  23/45 files`
const EXTRACT_STYLE: &str = "  {msg}  {wide_bar:.green/dim}  {pos}/{len} files";

/// Spinner for indeterminate phases (mirror fetch, verification).
const SPINNER_STYLE: &str = "  {spinner:.cyan} {msg}";

// ── Progress bar characters ──────────────────────────────────────────
//
// Using Unicode box-drawing characters for a clean, modern look.
// The three characters are: filled, in-progress tip, empty.

const BAR_CHARS: &str = "━╸─";

// ── Spinner frames ───────────────────────────────────────────────────
//
// A compact set of braille-dot frames that cycle smoothly. Each frame
// is one character wide so the spinner doesn't shift the message text.

const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

// ── ProgressDisplay ──────────────────────────────────────────────────

/// Tracks the active progress bar state across `DownloadProgress` events.
///
/// Create one `ProgressDisplay` per download operation (or per package in a
/// multi-package flow). Call `update()` for each `DownloadProgress` event.
/// The display handles phase transitions automatically — it finishes the
/// previous bar and creates a new one when the phase changes.
pub struct ProgressDisplay {
    /// Currently active progress bar (download, extract, or spinner).
    bar: Option<ProgressBar>,
    /// Which phase we're currently in, to detect transitions.
    phase: Phase,
    /// When this download started, for elapsed time on completion.
    started: Instant,
    /// Title of the package being downloaded, shown in completion message.
    package_title: String,
}

/// Internal phase tracking — used to detect when we need to swap bars.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Initial state, no bar created yet.
    Idle,
    /// Spinner: fetching mirror list.
    FetchingMirrors,
    /// Spinner or status: trying a mirror URL.
    TryingMirror,
    /// Progress bar: downloading bytes.
    Downloading,
    /// Spinner: SHA-1 verification.
    Verifying,
    /// Progress bar: extracting files.
    Extracting,
    /// Terminal: all done.
    Complete,
}

impl ProgressDisplay {
    /// Creates a new progress display for a single package download.
    ///
    /// `package_title` is shown in the completion message (e.g. "RA Base Files").
    pub fn new(package_title: &str) -> Self {
        Self {
            bar: None,
            phase: Phase::Idle,
            started: Instant::now(),
            package_title: package_title.to_string(),
        }
    }

    /// Handles a `DownloadProgress` event, updating the terminal display.
    ///
    /// This is the callback you pass to `download_package()` /
    /// `download_and_install()` / `download_missing()`.
    pub fn update(&mut self, event: DownloadProgress) {
        match event {
            DownloadProgress::FetchingMirrors { url } => {
                self.transition_to(Phase::FetchingMirrors);
                let bar = self.ensure_spinner();
                // Show just the domain from the URL for a cleaner display.
                let domain = extract_domain(&url);
                bar.set_message(format!("Fetching mirrors from {domain}..."));
            }

            DownloadProgress::TryingMirror { index, total, url } => {
                self.transition_to(Phase::TryingMirror);
                let bar = self.ensure_spinner();
                if total == 1 {
                    let domain = extract_domain(&url);
                    bar.set_message(format!("Connecting to {domain}..."));
                } else {
                    bar.set_message(format!("Racing {total} mirrors..."));
                    // Only show this on the first call to avoid flicker.
                    if index == 0 {
                        bar.set_message(format!("Racing {total} mirrors..."));
                    }
                }
            }

            DownloadProgress::Downloading { bytes, total } => {
                if self.phase != Phase::Downloading {
                    self.transition_to(Phase::Downloading);
                    let bar = self.ensure_download_bar(total);
                    bar.set_message("Downloading");
                    bar.set_position(bytes);
                } else if let Some(bar) = &self.bar {
                    bar.set_position(bytes);
                }
            }

            DownloadProgress::Verifying => {
                self.transition_to(Phase::Verifying);
                let bar = self.ensure_spinner();
                bar.set_message("Verifying SHA-1...");
            }

            DownloadProgress::Extracting {
                entry: _,
                index,
                total,
            } => {
                if self.phase != Phase::Extracting {
                    self.transition_to(Phase::Extracting);
                    let bar = self.ensure_extract_bar(total);
                    bar.set_message("Extracting");
                    bar.set_position((index + 1) as u64);
                } else if let Some(bar) = &self.bar {
                    bar.set_position((index + 1) as u64);
                }
            }

            DownloadProgress::Complete { files } => {
                self.transition_to(Phase::Complete);
                let elapsed = self.started.elapsed();
                let secs = elapsed.as_secs();
                let time_str = if secs >= 60 {
                    format!("{}m {:02}s", secs / 60, secs % 60)
                } else {
                    format!("{secs}s")
                };
                eprintln!(
                    "  {} {} — {files} files installed in {time_str}",
                    style("✓").green().bold(),
                    style(&self.package_title).bold(),
                );
            }
        }
    }

    // ── Internal helpers ─────────────────────────────────────────────

    /// Finishes the current bar (if any) when transitioning phases.
    fn transition_to(&mut self, new_phase: Phase) {
        if self.phase == new_phase {
            return;
        }
        // Finish the old bar cleanly.
        if let Some(bar) = self.bar.take() {
            match self.phase {
                Phase::Verifying => {
                    // Show a green checkmark for completed verification.
                    bar.finish_with_message(format!("Verifying SHA-1... {}", style("✓").green()));
                }
                Phase::FetchingMirrors | Phase::TryingMirror => {
                    bar.finish_and_clear();
                }
                _ => {
                    bar.finish_and_clear();
                }
            }
        }
        self.phase = new_phase;
    }

    /// Returns (or creates) a spinner bar for indeterminate phases.
    fn ensure_spinner(&mut self) -> &ProgressBar {
        if self.bar.is_none() {
            let bar = ProgressBar::new_spinner();
            let tick_strings: Vec<&str> = SPINNER_FRAMES.to_vec();
            bar.set_style(
                ProgressStyle::with_template(SPINNER_STYLE)
                    .expect("valid spinner template")
                    .tick_strings(&tick_strings),
            );
            bar.enable_steady_tick(std::time::Duration::from_millis(80));
            self.bar = Some(bar);
        }
        self.bar.as_ref().expect("just created")
    }

    /// Creates a download progress bar with byte tracking.
    fn ensure_download_bar(&mut self, total: Option<u64>) -> &ProgressBar {
        let bar = if let Some(total) = total {
            let pb = ProgressBar::new(total);
            pb.set_style(
                ProgressStyle::with_template(DOWNLOAD_STYLE)
                    .expect("valid download template")
                    .progress_chars(BAR_CHARS),
            );
            pb
        } else {
            let pb = ProgressBar::new_spinner();
            pb.set_style(
                ProgressStyle::with_template(DOWNLOAD_UNKNOWN_STYLE)
                    .expect("valid unknown-size template")
                    .progress_chars(BAR_CHARS),
            );
            pb.enable_steady_tick(std::time::Duration::from_millis(100));
            pb
        };
        self.bar = Some(bar);
        self.bar.as_ref().expect("just created")
    }

    /// Creates an extraction progress bar with file count tracking.
    fn ensure_extract_bar(&mut self, total: usize) -> &ProgressBar {
        let bar = ProgressBar::new(total as u64);
        bar.set_style(
            ProgressStyle::with_template(EXTRACT_STYLE)
                .expect("valid extract template")
                .progress_chars(BAR_CHARS),
        );
        self.bar = Some(bar);
        self.bar.as_ref().expect("just created")
    }
}

impl Drop for ProgressDisplay {
    fn drop(&mut self) {
        // Ensure any active bar is finished on drop so the terminal is clean.
        if let Some(bar) = self.bar.take() {
            bar.finish_and_clear();
        }
    }
}

// ── Section header ───────────────────────────────────────────────────

/// Prints a styled section header for a download group.
///
/// Example output: `── RA Base Files (HTTP) ──`
pub fn print_section_header(title: &str, strategy: &str) {
    eprintln!(
        "\n{} {} {} {}",
        style("──").dim(),
        style(title).bold(),
        style(format!("({strategy})")).dim(),
        style("──").dim(),
    );
}

/// Prints a skip message for already-installed packages.
pub fn print_already_installed(title: &str) {
    eprintln!("  {} {title} — already installed", style("·").dim(),);
}

/// Prints a warning for a non-fatal download failure.
pub fn print_download_warning(title: &str, error: &str) {
    eprintln!(
        "  {} {} — {error}",
        style("⚠").yellow(),
        style(title).yellow(),
    );
    eprintln!(
        "    {}",
        style("(may require IC mirrors or a local source)").dim(),
    );
}

// ── Helpers ──────────────────────────────────────────────────────────

/// Extracts the domain portion from a URL for compact display.
///
/// Returns the input unchanged if parsing fails — this is best-effort
/// display formatting, never a security boundary.
fn extract_domain(url: &str) -> &str {
    // Strip scheme.
    let after_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    // Take up to the first `/` or end of string.
    match after_scheme.find('/') {
        Some(pos) => after_scheme.get(..pos).unwrap_or(after_scheme),
        None => after_scheme,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── extract_domain ───────────────────────────────────────────────

    /// Verifies domain extraction from HTTPS URLs.
    ///
    /// The domain should be the hostname without scheme or path, used
    /// for compact progress display.
    #[test]
    fn extract_domain_https() {
        assert_eq!(
            extract_domain("https://www.openra.net/packages/ra-mirrors.txt"),
            "www.openra.net"
        );
    }

    /// Verifies domain extraction from HTTP URLs.
    #[test]
    fn extract_domain_http() {
        assert_eq!(
            extract_domain("http://files.cncnz.com/path/file.zip"),
            "files.cncnz.com"
        );
    }

    /// Verifies that a bare hostname with no path is returned as-is.
    #[test]
    fn extract_domain_no_path() {
        assert_eq!(extract_domain("https://example.com"), "example.com");
    }

    /// Verifies that garbage input passes through unchanged.
    ///
    /// extract_domain is display formatting, not a security function,
    /// so it must never panic on bad input.
    #[test]
    fn extract_domain_garbage_input() {
        assert_eq!(extract_domain("not-a-url"), "not-a-url");
    }

    /// Verifies domain extraction from URLs with port numbers.
    #[test]
    fn extract_domain_with_port() {
        assert_eq!(
            extract_domain("https://localhost:8080/path"),
            "localhost:8080"
        );
    }
}
