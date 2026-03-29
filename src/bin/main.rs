// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! `cnc-content` CLI — download, verify, and manage RA1 game content.

use std::path::PathBuf;
use std::process;

use clap::{Parser, Subcommand};

use cnc_content::downloader::{download_missing, download_package, DownloadProgress};

#[derive(Parser)]
#[command(
    name = "cnc-content",
    about = "C&C content manager — download, verify, and manage RA1 game content",
    version
)]
struct Cli {
    /// Content directory override (default: portable, next to executable).
    #[arg(long, global = true, env = "IC_CONTENT_DIR")]
    content_dir: Option<PathBuf>,

    /// Use OpenRA's content directory instead of the portable default.
    ///
    /// Downloads into OpenRA's managed path so both engines share the
    /// same content files. Ignored if --content-dir is also set.
    #[arg(long, global = true)]
    openra: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Show installed/missing content packages.
    Status,
    /// Download content from mirrors.
    Download {
        /// Download a specific package instead of all required.
        #[arg(long)]
        package: Option<String>,
        /// Download ALL content including optional (music, movies).
        #[arg(long)]
        all: bool,
    },
    /// Verify installed content integrity (SHA-256).
    Verify,
    /// Identify a content source at a given path.
    Identify {
        /// Path to the source directory (disc mount, Steam library, etc.).
        path: PathBuf,
    },
    /// Auto-detect local game installs (Steam, Origin, OpenRA, disc).
    Detect,
    /// Install content from a detected local source.
    Install {
        /// Path to the source directory.
        path: PathBuf,
        /// Install only a specific package from the source.
        #[arg(long)]
        package: Option<String>,
    },
    /// Remove all downloaded content.
    Clean {
        /// Skip confirmation prompt.
        #[arg(long, short = 'y')]
        yes: bool,
    },
}

fn main() {
    let cli = Cli::parse();

    let content_root = if let Some(dir) = cli.content_dir {
        dir
    } else if cli.openra {
        cnc_content::openra_content_root().unwrap_or_else(|| {
            eprintln!("Cannot determine OpenRA content path on this platform.");
            process::exit(1);
        })
    } else {
        cnc_content::default_content_root()
    };

    match cli.command {
        Command::Status => cmd_status(&content_root),
        Command::Download { package, all } => cmd_download(&content_root, package.as_deref(), all),
        Command::Verify => cmd_verify(&content_root),
        Command::Identify { path } => cmd_identify(&path),
        Command::Detect => cmd_detect(),
        Command::Install { path, package } => cmd_install(&content_root, &path, package.as_deref()),
        Command::Clean { yes } => cmd_clean(&content_root, yes),
    }
}

fn cmd_status(content_root: &std::path::Path) {
    println!("Content directory: {}", content_root.display());
    println!();

    println!("  Required:");
    for pkg in cnc_content::packages::ALL_PACKAGES
        .iter()
        .filter(|p| p.required)
    {
        let installed = pkg.test_files.iter().all(|f| content_root.join(f).exists());
        let marker = if installed { "✓" } else { "✗" };
        println!("    {marker} {}", pkg.title);

        if !installed {
            let missing: Vec<_> = pkg
                .test_files
                .iter()
                .filter(|f| !content_root.join(f).exists())
                .collect();
            if missing.len() <= 3 {
                for f in &missing {
                    println!("        missing: {f}");
                }
            } else {
                println!("        missing: {} files", missing.len());
            }
        }
    }

    println!();
    println!("  Optional:");
    for pkg in cnc_content::packages::ALL_PACKAGES
        .iter()
        .filter(|p| !p.required)
    {
        let installed = pkg.test_files.iter().all(|f| content_root.join(f).exists());
        let marker = if installed { "✓" } else { "–" };
        let dl_hint = if !installed {
            match pkg.download {
                Some(_) => " (downloadable)",
                None => " (requires local source)",
            }
        } else {
            ""
        };
        println!("    {marker} {}{dl_hint}", pkg.title);
    }

    println!();
    if cnc_content::is_content_complete(content_root) {
        println!("All required content is installed.");
        let optional_missing = cnc_content::missing_packages(content_root)
            .iter()
            .filter(|p| !p.required)
            .count();
        if optional_missing > 0 {
            println!(
                "{optional_missing} optional package(s) available. Run `cnc-content download --all` to fetch them."
            );
        }
    } else {
        let missing = cnc_content::missing_required_packages(content_root);
        println!(
            "{} required package(s) missing. Run `cnc-content download` to fetch them.",
            missing.len()
        );
    }
}

fn cmd_download(content_root: &std::path::Path, package_name: Option<&str>, download_all: bool) {
    if let Some(name) = package_name {
        let dl_id = match name.to_lowercase().as_str() {
            "quickinstall" | "quick" | "all" => cnc_content::DownloadId::QuickInstall,
            "base" | "basefiles" => cnc_content::DownloadId::BaseFiles,
            "aftermath" => cnc_content::DownloadId::Aftermath,
            "desert" | "cncdesert" => cnc_content::DownloadId::CncDesert,
            "music" | "scores" => cnc_content::DownloadId::Music,
            "movies-allied" | "moviesallied" | "allied" => cnc_content::DownloadId::MoviesAllied,
            "movies-soviet" | "moviessoviet" | "soviet" => cnc_content::DownloadId::MoviesSoviet,
            "music-cs" | "counterstrike" => cnc_content::DownloadId::MusicCounterstrike,
            "music-am" | "aftermath-music" => cnc_content::DownloadId::MusicAftermath,
            _ => {
                eprintln!("Unknown download package: {name}");
                eprintln!("Available: quickinstall, base, aftermath, desert, music,");
                eprintln!("          movies-allied, movies-soviet, music-cs, music-am");
                process::exit(1);
            }
        };

        let pkg = cnc_content::download(dl_id);
        println!("Downloading: {}", pkg.title);
        if let Err(e) = download_package(pkg, content_root, &mut print_progress) {
            eprintln!("Download failed: {e}");
            process::exit(1);
        }
    } else if download_all {
        // Download everything: required + optional.
        println!("Downloading all content (required + optional)...");

        // Required first (QuickInstall covers all three).
        if !cnc_content::is_content_complete(content_root) {
            println!("\n── Required content ──");
            if let Err(e) = download_missing(content_root, &mut print_progress) {
                eprintln!("Download failed: {e}");
                process::exit(1);
            }
        } else {
            println!("Required content already installed.");
        }

        // Optional packages.
        let optional_downloads = [
            cnc_content::DownloadId::Music,
            cnc_content::DownloadId::MoviesAllied,
            cnc_content::DownloadId::MoviesSoviet,
            cnc_content::DownloadId::MusicCounterstrike,
            cnc_content::DownloadId::MusicAftermath,
        ];

        for dl_id in optional_downloads {
            let dl = cnc_content::download(dl_id);
            // Check if already installed.
            let already_installed = dl.provides.iter().all(|&pkg_id| {
                let pkg = cnc_content::package(pkg_id);
                pkg.test_files.iter().all(|f| content_root.join(f).exists())
            });

            if already_installed {
                println!("\n{}: already installed.", dl.title);
                continue;
            }

            println!("\n── {} ──", dl.title);
            match download_package(dl, content_root, &mut print_progress) {
                Ok(()) => {}
                Err(e) => {
                    // Non-fatal for optional content — report and continue.
                    eprintln!("  Warning: {e}");
                    eprintln!(
                        "  (Skipping — this package may require IC mirrors or a local source.)"
                    );
                }
            }
        }
    } else {
        // Default: download required only.
        if cnc_content::is_content_complete(content_root) {
            println!("All required content is already installed.");
            println!("Use `--all` to also download optional content (music, movies).");
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

    // Verify all installed packages (not just required).
    let installed_ids: Vec<cnc_content::PackageId> = cnc_content::packages::ALL_PACKAGES
        .iter()
        .filter(|p| p.test_files.iter().all(|f| content_root.join(f).exists()))
        .map(|p| p.id)
        .collect();

    if installed_ids.is_empty() {
        println!("No content packages are installed.");
        return;
    }

    println!("Generating content manifest...");
    match cnc_content::verify::generate_manifest(content_root, "ra", "v1", &installed_ids) {
        Ok(manifest) => {
            println!("Verifying {} files...", manifest.files.len());
            let failures = cnc_content::verify::verify_installed_content(content_root, &manifest);
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
            for pkg in &provides {
                let req = if pkg.required { " (required)" } else { "" };
                println!("  - {}{req}", pkg.title);
            }

            // Show available recipes.
            let recipes = cnc_content::recipes_for_source(source_id);
            if !recipes.is_empty() {
                println!("Install recipes: {} available", recipes.len());
                println!(
                    "  Run `cnc-content install {}` to install from this source.",
                    path.display()
                );
            }
        }
        None => {
            println!("No known RA1 content source identified at this path.");
            println!(
                "Checked against {} known sources.",
                cnc_content::sources::ALL_SOURCES.len()
            );
        }
    }
}

fn cmd_detect() {
    println!("Scanning for local C&C content sources...\n");

    let detected = cnc_content::source::detect_all();

    if detected.is_empty() {
        println!("No local content sources detected.");
        println!();
        println!("Checked:");
        println!("  - Steam libraries");
        println!("  - Origin / EA App installs");
        println!("  - OpenRA content directories");
        println!("  - Mounted disc volumes");
        println!();
        println!("Use `cnc-content download` to fetch content from mirrors instead.");
        return;
    }

    println!("Found {} source(s):\n", detected.len());
    for (i, src) in detected.iter().enumerate() {
        let source_def = cnc_content::source(src.source_id);
        println!(
            "  {}. {} ({:?})",
            i + 1,
            source_def.title,
            source_def.source_type
        );
        println!("     Path: {}", src.path.display());
        println!("     Provides {} package(s):", src.packages.len());
        for pkg_id in &src.packages {
            let pkg = cnc_content::package(*pkg_id);
            let req = if pkg.required { " (required)" } else { "" };
            println!("       - {}{req}", pkg.title);
        }
        println!();
    }

    println!("To install from a source:");
    println!("  cnc-content install <path>");
    println!("  cnc-content install <path> --package music");
}

fn cmd_install(
    content_root: &std::path::Path,
    source_path: &std::path::Path,
    package_filter: Option<&str>,
) {
    if !source_path.exists() {
        eprintln!("Source path does not exist: {}", source_path.display());
        process::exit(1);
    }

    // Identify the source.
    let source_id = match cnc_content::verify::identify_source(source_path) {
        Some(id) => id,
        None => {
            eprintln!(
                "Cannot identify a known C&C content source at: {}",
                source_path.display()
            );
            process::exit(1);
        }
    };

    let source_def = cnc_content::source(source_id);
    println!(
        "Source: {} ({:?})",
        source_def.title, source_def.source_type
    );
    println!("Path: {}", source_path.display());

    // Find applicable recipes.
    let mut recipes = cnc_content::recipes_for_source(source_id);

    // Filter by package if specified.
    if let Some(filter) = package_filter {
        let pkg_id = match filter.to_lowercase().as_str() {
            "base" => cnc_content::PackageId::Base,
            "aftermath" | "aftermathbase" => cnc_content::PackageId::AftermathBase,
            "desert" | "cncdesert" => cnc_content::PackageId::CncDesert,
            "music" | "scores" => cnc_content::PackageId::Music,
            "movies-allied" | "moviesallied" | "allied" => cnc_content::PackageId::MoviesAllied,
            "movies-soviet" | "moviessoviet" | "soviet" => cnc_content::PackageId::MoviesSoviet,
            "music-cs" | "counterstrike" => cnc_content::PackageId::MusicCounterstrike,
            "music-am" | "aftermath-music" => cnc_content::PackageId::MusicAftermath,
            _ => {
                eprintln!("Unknown package: {filter}");
                process::exit(1);
            }
        };
        recipes.retain(|r| r.package == pkg_id);

        if recipes.is_empty() {
            eprintln!("No install recipe for {pkg_id:?} from {source_id:?}");
            process::exit(1);
        }
    }

    if recipes.is_empty() {
        eprintln!("No install recipes available for {source_id:?}");
        process::exit(1);
    }

    println!("\nInstalling {} package(s)...\n", recipes.len());

    std::fs::create_dir_all(content_root).unwrap_or_else(|e| {
        eprintln!("Failed to create content directory: {e}");
        process::exit(1);
    });

    let mut total_files = 0;
    let mut total_errors = 0;

    for recipe in &recipes {
        let pkg = cnc_content::package(recipe.package);
        println!("── {} ──", pkg.title);

        match cnc_content::executor::execute_recipe(recipe, source_path, content_root, |progress| {
            match progress {
                cnc_content::executor::InstallProgress::ActionStarted {
                    index,
                    total,
                    description,
                } => {
                    println!("  [{}/{}] {description}", index + 1, total);
                }
                cnc_content::executor::InstallProgress::FileWritten { path, bytes } => {
                    let name = path.file_name().unwrap_or_default().to_string_lossy();
                    if bytes > 1_048_576 {
                        println!("    {name} ({:.1} MB)", bytes as f64 / 1_048_576.0);
                    }
                }
                cnc_content::executor::InstallProgress::Completed {
                    files_written,
                    total_bytes,
                } => {
                    println!(
                        "  Done: {} files ({:.1} MB)",
                        files_written,
                        total_bytes as f64 / 1_048_576.0
                    );
                    total_files += files_written;
                }
            }
        }) {
            Ok(()) => {}
            Err(e) => {
                eprintln!("  Error: {e}");
                total_errors += 1;
            }
        }
        println!();
    }

    println!("Install complete: {total_files} files written.");
    if total_errors > 0 {
        eprintln!("{total_errors} package(s) had errors.");
        process::exit(1);
    }

    println!();
    cmd_status(content_root);
}

fn cmd_clean(content_root: &std::path::Path, skip_confirm: bool) {
    if !content_root.exists() {
        println!(
            "Content directory does not exist: {}",
            content_root.display()
        );
        println!("Nothing to clean.");
        return;
    }

    // Count what's there.
    let mut file_count = 0u64;
    let mut total_size = 0u64;
    if let Ok(entries) = walkdir(content_root) {
        for (_, size) in &entries {
            file_count += 1;
            total_size += size;
        }
    }

    if file_count == 0 {
        println!("Content directory is empty: {}", content_root.display());
        return;
    }

    println!("Content directory: {}", content_root.display());
    println!(
        "  {} file(s), {:.1} MB",
        file_count,
        total_size as f64 / 1_048_576.0
    );

    if !skip_confirm {
        println!();
        println!("Remove all content? This cannot be undone.");
        print!("Type 'yes' to confirm: ");
        use std::io::Write;
        std::io::stdout().flush().ok();

        let mut input = String::new();
        if std::io::stdin().read_line(&mut input).is_err() {
            eprintln!("Failed to read input.");
            process::exit(1);
        }
        if input.trim() != "yes" {
            println!("Cancelled.");
            return;
        }
    }

    match std::fs::remove_dir_all(content_root) {
        Ok(()) => {
            println!(
                "Removed {} file(s) ({:.1} MB).",
                file_count,
                total_size as f64 / 1_048_576.0
            );
        }
        Err(e) => {
            eprintln!("Failed to remove content directory: {e}");
            process::exit(1);
        }
    }
}

/// Recursively lists all files under a directory with their sizes.
fn walkdir(dir: &std::path::Path) -> std::io::Result<Vec<(PathBuf, u64)>> {
    let mut files = Vec::new();
    walk_recursive(dir, &mut files)?;
    Ok(files)
}

fn walk_recursive(dir: &std::path::Path, out: &mut Vec<(PathBuf, u64)>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        if ft.is_dir() {
            walk_recursive(&entry.path(), out)?;
        } else if ft.is_file() {
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            out.push((entry.path(), size));
        }
    }
    Ok(())
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
            println!("  Downloaded: {:.1} MB", bytes as f64 / 1_048_576.0);
        }
        DownloadProgress::Verifying => {
            println!("Verifying SHA-1...");
        }
        DownloadProgress::Extracting {
            entry,
            index,
            total,
        } => {
            if index % 10 == 0 || index + 1 == total {
                println!("  Extracting: {entry} ({}/{})", index + 1, total);
            }
        }
        DownloadProgress::Complete { files } => {
            println!("Complete: {files} files extracted.");
        }
    }
}
