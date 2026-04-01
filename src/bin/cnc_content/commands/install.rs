//! Content acquisition commands: download, install, clean, torrent-create.

use std::path::PathBuf;
use std::process;

use cnc_content::downloader::download_missing;
use cnc_content::GameId;

use super::status::{cmd_status, format_size, walkdir};
use crate::progress;

pub fn cmd_download(
    content_root: &std::path::Path,
    game: GameId,
    package_name: Option<&str>,
    download_all: bool,
    seeding_policy: cnc_content::SeedingPolicy,
) {
    use console::style;

    eprintln!(
        "{} {}",
        style("Seeding policy:").dim(),
        seeding_policy.label()
    );

    // Block downloads for non-freeware games.
    if !game.is_freeware() {
        eprintln!(
            "{} {} is NOT freeware — downloading is not supported.",
            style("✗").red().bold(),
            game.title()
        );
        eprintln!("Only EA-declared freeware games (Red Alert, Tiberian Dawn) can be downloaded.");
        eprintln!(
            "If you have a local copy, use: cnc-content --game {} install <path>",
            game.slug()
        );
        process::exit(1);
    }

    if let Some(name) = package_name {
        // ── Single named package download ────────────────────────────
        let dl_id = resolve_download_name(name, game);
        let pkg = cnc_content::download(dl_id);
        let strategy = cnc_content::downloader::select_strategy(pkg);
        progress::print_section_header(pkg.title, &format!("{strategy:?}"));
        let mut display = progress::ProgressDisplay::new(pkg.title);
        if let Err(e) = cnc_content::downloader::download_and_install(
            pkg,
            content_root,
            seeding_policy,
            &mut |evt| display.update(evt),
        ) {
            eprintln!("\n  {} Download failed: {e}", style("✗").red().bold());
            process::exit(1);
        }
    } else if download_all {
        // ── Download everything: required + optional ─────────────────
        eprintln!(
            "\n{} {} content (required + optional)",
            style("Downloading all").bold(),
            game.title()
        );

        // Required first.
        if !cnc_content::is_content_complete(content_root, game) {
            progress::print_section_header("Required content", "HTTP");
            let title = format!("{} required", game.title());
            let mut display = progress::ProgressDisplay::new(&title);
            if let Err(e) = download_missing(content_root, game, &mut |evt| display.update(evt)) {
                eprintln!("\n  {} Download failed: {e}", style("✗").red().bold());
                process::exit(1);
            }
        } else {
            eprintln!("  {} Required content already installed.", style("·").dim());
        }

        // Optional packages.
        let optional_downloads: Vec<_> = cnc_content::downloads_for_game(game)
            .into_iter()
            .filter(|dl| {
                // Skip downloads that only provide required packages
                // (already handled above).
                dl.provides.iter().any(|&pkg_id| {
                    let pkg = cnc_content::package(pkg_id);
                    !pkg.required
                })
            })
            .collect();

        for dl in optional_downloads {
            let already_installed = dl.provides.iter().all(|&pkg_id| {
                let pkg = cnc_content::package(pkg_id);
                pkg.test_files.iter().all(|f| content_root.join(f).exists())
            });

            if already_installed {
                progress::print_already_installed(dl.title);
                continue;
            }

            let strategy = cnc_content::downloader::select_strategy(dl);
            progress::print_section_header(dl.title, &format!("{strategy:?}"));
            let mut display = progress::ProgressDisplay::new(dl.title);
            match cnc_content::downloader::download_and_install(
                dl,
                content_root,
                seeding_policy,
                &mut |evt| display.update(evt),
            ) {
                Ok(()) => {}
                Err(e) => {
                    progress::print_download_warning(dl.title, &e.to_string());
                }
            }
        }
    } else {
        // ── Default: download required only ──────────────────────────
        if cnc_content::is_content_complete(content_root, game) {
            eprintln!(
                "  {} All required content is already installed.",
                style("✓").green()
            );
            eprintln!(
                "  {} Use {} to also download optional content (music, movies).",
                style("·").dim(),
                style("--all").bold()
            );
            return;
        }

        progress::print_section_header(&format!("{} — required content", game.title()), "HTTP");
        let title = format!("{} required", game.title());
        let mut display = progress::ProgressDisplay::new(&title);
        if let Err(e) = download_missing(content_root, game, &mut |evt| display.update(evt)) {
            eprintln!("\n  {} Download failed: {e}", style("✗").red().bold());
            process::exit(1);
        }
    }

    eprintln!();
    cmd_status(content_root, game);
}

pub fn cmd_install(
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
        let pkg_id = resolve_package_name(filter);
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

    // Determine game from the first recipe's package.
    if let Some(first) = recipes.first() {
        let game = cnc_content::package(first.package).game;
        println!();
        cmd_status(content_root, game);
    }
}

pub fn cmd_clean(content_root: &std::path::Path, skip_confirm: bool) {
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

pub fn cmd_torrent_create(output_dir: Option<&std::path::Path>, game_filter: Option<&str>) {
    use cnc_content::torrent_create::{create_torrent, DEFAULT_PIECE_LENGTH};

    let filter_game = game_filter.and_then(GameId::from_slug);
    if let Some(filter_str) = game_filter {
        if filter_game.is_none() {
            eprintln!("Unknown game filter: '{filter_str}'. Use: ra, td, dune2, dune2000");
            process::exit(1);
        }
    }

    let output = output_dir
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    std::fs::create_dir_all(&output).ok();

    let trackers: Vec<&str> = cnc_content::public_trackers().collect();
    let mut generated = 0u32;
    let mut skipped = 0u32;

    println!("Generating .torrent files...\n");

    for dl in cnc_content::downloads::ALL_DOWNLOADS {
        if let Some(game) = filter_game {
            if dl.game != game {
                continue;
            }
        }

        // We need a local copy of the package to hash pieces. Look for the
        // downloaded ZIP in the output directory (named by download ID slug).
        let file_name = format!("{:?}.zip", dl.id).to_lowercase().replace("::", "-");
        let zip_path = output.join(&file_name);

        if !zip_path.exists() {
            // Try to download the package.
            if !dl.is_available() {
                println!("  SKIP {:?} — no download source available", dl.id);
                skipped += 1;
                continue;
            }

            println!(
                "  Downloading {:?} ({})...",
                dl.id,
                format_size(dl.size_hint)
            );
            match download_to_file(dl, &zip_path) {
                Ok(()) => println!("    Downloaded to {}", zip_path.display()),
                Err(e) => {
                    eprintln!("    FAILED: {e}");
                    skipped += 1;
                    continue;
                }
            }
        } else {
            println!("  Using cached {:?} at {}", dl.id, zip_path.display());
        }

        match create_torrent(&zip_path, DEFAULT_PIECE_LENGTH, &trackers) {
            Ok(meta) => {
                // Write .torrent file.
                let torrent_name = file_name.replace(".zip", ".torrent");
                let torrent_path = output.join(&torrent_name);
                if let Err(e) = std::fs::write(&torrent_path, &meta.torrent_data) {
                    eprintln!("    Failed to write {}: {e}", torrent_path.display());
                    continue;
                }

                println!("    info_hash: {}", meta.info_hash);
                println!(
                    "    pieces: {}, file size: {}",
                    meta.piece_count,
                    format_size(meta.file_size)
                );
                println!("    .torrent: {}", torrent_path.display());
                println!();
                generated += 1;
            }
            Err(e) => {
                eprintln!("    Torrent creation failed: {e}");
                skipped += 1;
            }
        }
    }

    println!("\nDone: {generated} torrent(s) generated, {skipped} skipped.");
    if generated > 0 {
        println!("\nPaste these info_hash values into src/downloads.rs:");
        println!("Then seed the .torrent files to activate P2P distribution.");
    }
}

/// Resolves a CLI package name to a DownloadId for the given game.
fn resolve_download_name(name: &str, game: GameId) -> cnc_content::DownloadId {
    use cnc_content::DownloadId;
    match game {
        GameId::RedAlert => match name.to_lowercase().as_str() {
            "quickinstall" | "quick" | "all" => DownloadId::RaQuickInstall,
            "base" | "basefiles" => DownloadId::RaBaseFiles,
            "aftermath" => DownloadId::RaAftermath,
            "desert" | "cncdesert" => DownloadId::RaCncDesert,
            "music" | "scores" => DownloadId::RaMusic,
            "movies-allied" | "moviesallied" | "allied" => DownloadId::RaMoviesAllied,
            "movies-soviet" | "moviessoviet" | "soviet" => DownloadId::RaMoviesSoviet,
            "music-cs" | "counterstrike" => DownloadId::RaMusicCounterstrike,
            "music-am" | "aftermath-music" => DownloadId::RaMusicAftermath,
            "full-discs" | "fulldiscs" | "discs" | "isos" => DownloadId::RaFullDiscs,
            "full-set" | "fullset" | "4cd" | "complete" => DownloadId::RaFullSet,
            _ => {
                eprintln!("Unknown RA download package: {name}");
                eprintln!("Available: quickinstall, base, aftermath, desert, music,");
                eprintln!("          movies-allied, movies-soviet, music-cs, music-am,");
                eprintln!("          full-discs, full-set");
                process::exit(1);
            }
        },
        GameId::TiberianDawn => {
            match name.to_lowercase().as_str() {
                "base" | "basefiles" => DownloadId::TdBaseFiles,
                "music" | "scores" => DownloadId::TdMusic,
                "movies-gdi" | "moviesgdi" | "gdi" => DownloadId::TdMoviesGdi,
                "movies-nod" | "moviesnod" | "nod" => DownloadId::TdMoviesNod,
                "covertops" | "covert-ops" | "expansion" => DownloadId::TdCovertOps,
                "gdi-iso" | "gdiiso" => DownloadId::TdGdiIso,
                "nod-iso" | "nodiso" => DownloadId::TdNodIso,
                _ => {
                    eprintln!("Unknown TD download package: {name}");
                    eprintln!("Available: base, music, movies-gdi, movies-nod, covertops, gdi-iso, nod-iso");
                    process::exit(1);
                }
            }
        }
        // Non-freeware games are blocked in cmd_download before reaching here.
        _ => {
            eprintln!(
                "{} is not freeware — downloading is not available.",
                game.title()
            );
            process::exit(1);
        }
    }
}

/// Resolves a CLI package name to a PackageId (game-agnostic).
///
/// Tries all games' package names. Used by `install --package` where the
/// source (not the `--game` flag) determines which game we're installing.
fn resolve_package_name(name: &str) -> cnc_content::PackageId {
    use cnc_content::PackageId;
    match name.to_lowercase().as_str() {
        // ── Red Alert ────────────────────────────────────────────
        "ra-base" => PackageId::RaBase,
        "aftermath" | "aftermathbase" | "ra-aftermath" => PackageId::RaAftermathBase,
        "desert" | "cncdesert" | "ra-desert" => PackageId::RaCncDesert,
        "ra-music" => PackageId::RaMusic,
        "movies-allied" | "moviesallied" | "allied" => PackageId::RaMoviesAllied,
        "movies-soviet" | "moviessoviet" | "soviet" => PackageId::RaMoviesSoviet,
        "music-cs" | "counterstrike" => PackageId::RaMusicCounterstrike,
        "music-am" | "aftermath-music" => PackageId::RaMusicAftermath,

        // ── Tiberian Dawn ────────────────────────────────────────
        "td-base" => PackageId::TdBase,
        "covertops" | "covert-ops" | "td-covertops" => PackageId::TdCovertOps,
        "td-music" => PackageId::TdMusic,
        "movies-gdi" | "moviesgdi" | "gdi" => PackageId::TdMoviesGdi,
        "movies-nod" | "moviesnod" | "nod" => PackageId::TdMoviesNod,

        // ── Ambiguous names — resolve by looking at common usage ─
        "base" => PackageId::RaBase, // most common; use ra-base/td-base to disambiguate
        "music" | "scores" => PackageId::RaMusic, // use ra-music/td-music to disambiguate

        // ── Dune 2 ──────────────────────────────────────────────
        "dune2-base" | "dune2" => PackageId::Dune2Base,

        // ── Dune 2000 ───────────────────────────────────────────
        "dune2000-base" | "dune2000" => PackageId::Dune2000Base,

        _ => {
            eprintln!("Unknown package: '{name}'");
            eprintln!();
            eprintln!("Red Alert:       base, aftermath, desert, music, movies-allied,");
            eprintln!("                 movies-soviet, music-cs, music-am");
            eprintln!("Tiberian Dawn:   td-base, covertops, td-music, movies-gdi, movies-nod");
            eprintln!("Dune 2:          dune2-base");
            eprintln!("Dune 2000:       dune2000-base");
            process::exit(1);
        }
    }
}

/// Downloads a package to a local file using the HTTP downloader.
fn download_to_file(
    dl: &cnc_content::DownloadPackage,
    dest: &std::path::Path,
) -> Result<(), String> {
    // Resolve URLs — try mirror list first, then direct URLs.
    let urls = if !dl.mirror_list_url.is_empty() {
        // Fetch mirror list.
        match ureq::get(dl.mirror_list_url).call() {
            Ok(response) => {
                let body = response
                    .into_body()
                    .read_to_string()
                    .map_err(|e| e.to_string())?;
                body.lines()
                    .map(|l| l.trim().to_string())
                    .filter(|l| !l.is_empty() && !l.starts_with('#'))
                    .collect::<Vec<_>>()
            }
            Err(e) => {
                eprintln!("    Mirror list fetch failed: {e}");
                dl.direct_urls.iter().map(|u| u.to_string()).collect()
            }
        }
    } else {
        dl.direct_urls.iter().map(|u| u.to_string()).collect()
    };

    if urls.is_empty() {
        return Err("no download URLs available".into());
    }

    // Try each URL until one succeeds.
    let mut last_err = String::new();
    for url in &urls {
        match download_url(url, dest) {
            Ok(()) => return Ok(()),
            Err(e) => {
                last_err = e;
                continue;
            }
        }
    }
    Err(format!("all mirrors failed, last error: {last_err}"))
}

/// Downloads a single URL to a file.
fn download_url(url: &str, dest: &std::path::Path) -> Result<(), String> {
    let response = ureq::get(url).call().map_err(|e| e.to_string())?;
    let mut body = response.into_body().into_reader();
    let mut file = std::fs::File::create(dest).map_err(|e| e.to_string())?;
    std::io::copy(&mut body, &mut file).map_err(|e| e.to_string())?;
    Ok(())
}
