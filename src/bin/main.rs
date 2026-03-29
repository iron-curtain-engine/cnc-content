// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! `cnc-content` CLI — download, verify, and manage RA1 game content.

use std::path::PathBuf;
use std::process;

use clap::{Parser, Subcommand};

use cnc_content::downloader::{DownloadProgress, download_missing, download_package};

#[derive(Parser)]
#[command(
    name = "cnc-content",
    about = "C&C content manager — download, verify, and manage RA1 game content",
    version
)]
struct Cli {
    /// Content directory override (default: ~/.iron-curtain/content/ra/v1/)
    #[arg(long, global = true, env = "IC_CONTENT_DIR")]
    content_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Show installed/missing content packages.
    Status,
    /// Download all required content from OpenRA mirrors.
    Download {
        /// Download a specific package instead of all required.
        #[arg(long)]
        package: Option<String>,
    },
    /// Verify installed content integrity (SHA-256).
    Verify,
    /// Identify a content source at a given path.
    Identify {
        /// Path to the source directory (disc mount, Steam library, etc.).
        path: PathBuf,
    },
}

fn main() {
    let cli = Cli::parse();
    let content_root = cli
        .content_dir
        .unwrap_or_else(cnc_content::default_content_root);

    match cli.command {
        Command::Status => cmd_status(&content_root),
        Command::Download { package } => cmd_download(&content_root, package.as_deref()),
        Command::Verify => cmd_verify(&content_root),
        Command::Identify { path } => cmd_identify(&path),
    }
}

fn cmd_status(content_root: &std::path::Path) {
    println!("Content directory: {}", content_root.display());
    println!();

    for pkg in cnc_content::packages::ALL_PACKAGES {
        let installed = pkg
            .test_files
            .iter()
            .all(|f| content_root.join(f).exists());
        let marker = if installed { "✓" } else { "✗" };
        let req = if pkg.required { " (required)" } else { "" };
        println!("  {marker} {}{req}", pkg.title);

        if !installed {
            let missing: Vec<_> = pkg
                .test_files
                .iter()
                .filter(|f| !content_root.join(f).exists())
                .collect();
            if missing.len() <= 3 {
                for f in &missing {
                    println!("      missing: {f}");
                }
            } else {
                println!("      missing: {} files", missing.len());
            }
        }
    }

    println!();
    if cnc_content::is_content_complete(content_root) {
        println!("All required content is installed.");
    } else {
        let missing = cnc_content::missing_required_packages(content_root);
        println!(
            "{} required package(s) missing. Run `cnc-content download` to fetch them.",
            missing.len()
        );
    }
}

fn cmd_download(content_root: &std::path::Path, package_name: Option<&str>) {
    if let Some(name) = package_name {
        let dl_id = match name.to_lowercase().as_str() {
            "quickinstall" | "quick" | "all" => cnc_content::DownloadId::QuickInstall,
            "base" | "basefiles" => cnc_content::DownloadId::BaseFiles,
            "aftermath" => cnc_content::DownloadId::Aftermath,
            "desert" | "cncdesert" => cnc_content::DownloadId::CncDesert,
            _ => {
                eprintln!("Unknown download package: {name}");
                eprintln!("Available: quickinstall, base, aftermath, desert");
                process::exit(1);
            }
        };

        let pkg = cnc_content::download(dl_id);
        println!("Downloading: {}", pkg.title);
        if let Err(e) = download_package(pkg, content_root, &mut print_progress) {
            eprintln!("Download failed: {e}");
            process::exit(1);
        }
    } else {
        if cnc_content::is_content_complete(content_root) {
            println!("All required content is already installed.");
            return;
        }

        println!("Downloading required content...");
        if let Err(e) = download_missing(content_root, &mut print_progress) {
            eprintln!("Download failed: {e}");
            process::exit(1);
        }
    }

    println!();
    cmd_status(content_root);
}

fn cmd_verify(content_root: &std::path::Path) {
    if !content_root.exists() {
        eprintln!(
            "Content directory does not exist: {}",
            content_root.display()
        );
        process::exit(1);
    }

    let required_ids = [
        cnc_content::PackageId::Base,
        cnc_content::PackageId::AftermathBase,
        cnc_content::PackageId::CncDesert,
    ];

    println!("Generating content manifest...");
    match cnc_content::verify::generate_manifest(content_root, "ra", "v1", &required_ids) {
        Ok(manifest) => {
            println!("Verifying {} files...", manifest.files.len());
            let failures =
                cnc_content::verify::verify_installed_content(content_root, &manifest);
            if failures.is_empty() {
                println!("All {} files verified successfully.", manifest.files.len());
            } else {
                eprintln!("{} file(s) failed verification:", failures.len());
                for f in &failures {
                    eprintln!("  ✗ {f}");
                }
                process::exit(1);
            }
        }
        Err(e) => {
            eprintln!("Failed to generate manifest: {e}");
            process::exit(1);
        }
    }
}

fn cmd_identify(path: &std::path::Path) {
    if !path.exists() {
        eprintln!("Path does not exist: {}", path.display());
        process::exit(1);
    }

    println!("Scanning: {}", path.display());

    match cnc_content::verify::identify_source(path) {
        Some(source_id) => {
            let src = cnc_content::source(source_id);
            println!("Identified: {} ({:?})", src.title, src.source_type);

            // Show which packages this source provides.
            let provides: Vec<_> = cnc_content::packages::ALL_PACKAGES
                .iter()
                .filter(|p| p.sources.contains(&source_id))
                .collect();
            println!("Provides {} package(s):", provides.len());
            for pkg in provides {
                let req = if pkg.required { " (required)" } else { "" };
                println!("  - {}{req}", pkg.title);
            }
        }
        None => {
            println!("No known RA1 content source identified at this path.");
            println!("Checked against {} known sources.", cnc_content::sources::ALL_SOURCES.len());
        }
    }
}

fn print_progress(progress: DownloadProgress) {
    match progress {
        DownloadProgress::FetchingMirrors { url } => {
            println!("Fetching mirror list: {url}");
        }
        DownloadProgress::TryingMirror { index, total, url } => {
            println!("  Mirror {}/{total}: {url}", index + 1);
        }
        DownloadProgress::Downloading { bytes } => {
            if bytes % (1024 * 1024) < 65536 {
                println!("  Downloaded: {:.1} MB", bytes as f64 / 1_048_576.0);
            }
        }
        DownloadProgress::Verifying => {
            println!("Verifying SHA-1...");
        }
        DownloadProgress::Extracting { entry, index, total } => {
            if index % 10 == 0 || index + 1 == total {
                println!("  Extracting: {entry} ({}/{})", index + 1, total);
            }
        }
        DownloadProgress::Complete { files } => {
            println!("Complete: {files} files extracted.");
        }
    }
}
