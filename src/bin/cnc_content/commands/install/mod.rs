//! Content acquisition commands: download, install, clean, torrent-create.

use std::path::PathBuf;
use std::process;

use cnc_content::downloader::download_missing;
use cnc_content::GameId;

use super::status::{cmd_status, format_size, walkdir};
use crate::progress;

mod helpers;
use helpers::{
    print_torrent_result, resolve_download_name, resolve_download_urls, resolve_package_name,
    stream_and_hash,
};

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
        eprintln!("Only EA-declared freeware games (Red Alert, Tiberian Dawn, Tiberian Sun) can be downloaded.");
        eprintln!(
            "If you have a local copy, use: cnc-content --game {} install <path>",
            game.slug()
        );
        process::exit(1);
    }

    if let Some(name) = package_name {
        // ── Single named package download ────────────────────────────
        let dl_id = resolve_download_name(name, game);
        let pkg = cnc_content::download(dl_id).unwrap_or_else(|| {
            eprintln!("Internal error: no download definition for {dl_id:?}");
            std::process::exit(1);
        });
        let strategy = cnc_content::downloader::select_strategy(pkg);
        progress::print_section_header(&pkg.title, &format!("{strategy:?}"));
        let mut display = progress::ProgressDisplay::new(&pkg.title);
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
            if let Err(e) = download_missing(content_root, game, seeding_policy, &mut |evt| {
                display.update(evt)
            }) {
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
                    cnc_content::package(pkg_id)
                        .map(|p| !p.required)
                        .unwrap_or(false)
                })
            })
            .collect();

        for dl in optional_downloads {
            let already_installed = dl.provides.iter().all(|&pkg_id| {
                cnc_content::package(pkg_id)
                    .map(|p| p.test_files.iter().all(|f| content_root.join(f).exists()))
                    .unwrap_or(false)
            });

            if already_installed {
                progress::print_already_installed(&dl.title);
                continue;
            }

            let strategy = cnc_content::downloader::select_strategy(dl);
            progress::print_section_header(&dl.title, &format!("{strategy:?}"));
            let mut display = progress::ProgressDisplay::new(&dl.title);
            match cnc_content::downloader::download_and_install(
                dl,
                content_root,
                seeding_policy,
                &mut |evt| display.update(evt),
            ) {
                Ok(()) => {}
                Err(e) => {
                    progress::print_download_warning(&dl.title, &e.to_string());
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
        if let Err(e) = download_missing(content_root, game, seeding_policy, &mut |evt| {
            display.update(evt)
        }) {
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

    let source_def = cnc_content::source(source_id).unwrap_or_else(|| {
        eprintln!("Internal error: no source definition for {source_id:?}");
        std::process::exit(1);
    });
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
        let pkg = cnc_content::package(recipe.package).unwrap_or_else(|| {
            eprintln!(
                "Internal error: no package definition for {:?}",
                recipe.package
            );
            std::process::exit(1);
        });
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
        let game = cnc_content::package(first.package)
            .unwrap_or_else(|| {
                eprintln!(
                    "Internal error: no package definition for {:?}",
                    first.package
                );
                std::process::exit(1);
            })
            .game;
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
    use cnc_content::torrent_create::{create_torrent, recommended_piece_length};

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

    for dl in cnc_content::downloads::all_downloads() {
        if let Some(game) = filter_game {
            if dl.game != game {
                continue;
            }
        }

        let file_name = format!("{:?}.zip", dl.id).to_lowercase().replace("::", "-");
        let zip_path = output.join(&file_name);
        let web_seed_strs: Vec<&str> = dl.web_seeds.iter().map(String::as_str).collect();
        let torrent_name = file_name.replace(".zip", ".torrent");
        let torrent_path = output.join(&torrent_name);

        // ── Cached file on disk: use file-based create_torrent ──────
        if zip_path.exists() {
            println!("  Using cached {:?} at {}", dl.id, zip_path.display());

            let file_size = std::fs::metadata(&zip_path).map(|m| m.len()).unwrap_or(0);
            let piece_length = recommended_piece_length(file_size);

            match create_torrent(&zip_path, piece_length, &trackers, &web_seed_strs) {
                Ok(meta) => {
                    if let Err(e) = std::fs::write(&torrent_path, &meta.torrent_data) {
                        eprintln!("    Failed to write {}: {e}", torrent_path.display());
                        continue;
                    }
                    print_torrent_result(&meta, &torrent_path);
                    generated += 1;
                }
                Err(e) => {
                    eprintln!("    Torrent creation failed: {e}");
                    skipped += 1;
                }
            }
            continue;
        }

        // ── No cached file: stream from HTTP, hash on the fly ───────
        //
        // Resolves mirror URLs, streams the HTTP response body through a
        // TorrentBuilder that hashes pieces as bytes arrive. The file
        // never touches disk — the .torrent is produced purely from the
        // network stream. This avoids downloading hundreds of megabytes
        // to disk just to re-read them for piece hashing.
        if !dl.is_available() {
            println!("  SKIP {:?} — no download source available", dl.id);
            skipped += 1;
            continue;
        }

        let urls = resolve_download_urls(dl);
        if urls.is_empty() {
            println!("  SKIP {:?} — no download URLs resolved", dl.id);
            skipped += 1;
            continue;
        }

        println!("  Streaming {:?} ({})...", dl.id, format_size(dl.size_hint));

        // Determine piece length from the expected file size (size_hint).
        // If size_hint is 0, use DEFAULT_PIECE_LENGTH — the actual file
        // size is unknown until the download completes.
        let piece_length = if dl.size_hint > 0 {
            recommended_piece_length(dl.size_hint)
        } else {
            cnc_content::torrent_create::DEFAULT_PIECE_LENGTH
        };

        // Derive the torrent file name from the first URL's path component.
        // This must match what all clients would use as the filename so that
        // the info_hash is identical.
        let torrent_file_name = urls
            .first()
            .and_then(|u| u.rsplit('/').next())
            .unwrap_or(&file_name);

        match stream_and_hash(
            &urls,
            torrent_file_name,
            piece_length,
            &trackers,
            &web_seed_strs,
        ) {
            Ok(meta) => {
                if let Err(e) = std::fs::write(&torrent_path, &meta.torrent_data) {
                    eprintln!("    Failed to write {}: {e}", torrent_path.display());
                    continue;
                }
                print_torrent_result(&meta, &torrent_path);
                generated += 1;
            }
            Err(e) => {
                eprintln!("    FAILED: {e}");
                skipped += 1;
            }
        }
    }

    println!("\nDone: {generated} torrent(s) generated, {skipped} skipped.");
    if generated > 0 {
        println!("\nPaste these info_hash values into data/downloads.toml:");
        println!("Then embed the .torrent files in data/torrents/ to activate P2P.");
    }
}
