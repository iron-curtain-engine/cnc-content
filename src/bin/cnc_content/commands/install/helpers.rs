//! Private helper functions for install command handlers.

use std::process;

use cnc_content::GameId;

use crate::commands::status::format_size;

/// Prints the result of a successful torrent creation.
pub(super) fn print_torrent_result(
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
pub(super) fn resolve_download_urls(dl: &cnc_content::DownloadPackage) -> Vec<String> {
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
pub(super) fn stream_and_hash(
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
pub(super) fn resolve_download_name(name: &str, game: GameId) -> cnc_content::DownloadId {
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
pub(super) fn resolve_package_name(name: &str) -> cnc_content::PackageId {
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
