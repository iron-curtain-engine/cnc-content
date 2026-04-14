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

/// Prints the result of a successful torrent creation.
fn print_torrent_result(
    meta: &cnc_content::torrent_create::TorrentMetadata,
    torrent_path: &std::path::Path,
) {
    println!("    info_hash: {}", meta.info_hash);
    println!(
        "    pieces: {}, file size: {}",
        meta.piece_count,
        format_size(meta.file_size)
    );
    println!("    .torrent: {}", torrent_path.display());
    println!();
}

/// Resolves download URLs for a package (mirror list + direct URLs).
fn resolve_download_urls(dl: &cnc_content::DownloadPackage) -> Vec<String> {
    // Fetch mirror list if available, fall back to direct URLs.
    if let Some(url) = &dl.mirror_list_url {
        match ureq::get(url).call() {
            Ok(response) => {
                let body = response.into_body().read_to_string().unwrap_or_default();
                let mirrors: Vec<String> = body
                    .lines()
                    .map(|l| l.trim().to_string())
                    .filter(|l| !l.is_empty() && !l.starts_with('#'))
                    .collect();
                if !mirrors.is_empty() {
                    return mirrors;
                }
            }
            Err(e) => {
                eprintln!("    Mirror list fetch failed: {e}");
            }
        }
    }
    dl.direct_urls.iter().map(|u| u.to_string()).collect()
}

/// Streams a file from HTTP mirrors through a TorrentBuilder, hashing
/// pieces on the fly without writing the file to disk.
///
/// Tries each URL in order. On the first successful HTTP response, reads
/// the body in 64 KiB chunks and feeds each chunk to the builder. Returns
/// the finalized TorrentMetadata.
fn stream_and_hash(
    urls: &[String],
    file_name: &str,
    piece_length: u64,
    trackers: &[&str],
    web_seeds: &[&str],
) -> Result<cnc_content::torrent_create::TorrentMetadata, String> {
    use std::io::Read;

    use cnc_content::torrent_create::TorrentBuilder;

    let mut last_err = String::new();
    for url in urls {
        let response = match ureq::get(url).call() {
            Ok(r) => r,
            Err(e) => {
                last_err = format!("{url}: {e}");
                continue;
            }
        };

        let mut reader = response.into_body().into_reader();
        let mut builder = TorrentBuilder::new(file_name, piece_length);
        let mut buf = [0u8; 64 * 1024]; // 64 KiB read buffer

        loop {
            match reader.read(&mut buf) {
                Ok(0) => break, // EOF
                Ok(n) => {
                    if let Some(chunk) = buf.get(..n) {
                        builder.write(chunk);
                    }
                }
                Err(e) => {
                    last_err = format!("{url}: read error: {e}");
                    break;
                }
            }
        }

        // Only finalize if we read at least some bytes.
        if builder.bytes_written() > 0 {
            return builder
                .finalize(trackers, web_seeds)
                .map_err(|e| e.to_string());
        }
    }

    Err(format!("all mirrors failed, last error: {last_err}"))
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
