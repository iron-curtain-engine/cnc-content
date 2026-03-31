// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! `cnc-content` CLI — download, verify, and manage C&C game content.

// Use mimalloc as the global allocator on native targets for reduced
// fragmentation and improved throughput during download/extraction workloads.
#[cfg(not(target_arch = "wasm32"))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use std::path::PathBuf;
use std::process;

use clap::{Parser, Subcommand};

mod progress;

use cnc_content::downloader::download_missing;
use cnc_content::GameId;

// ── Help text constants ───────────────────────────────────────────────
//
// Extracted as constants so they're easy to find and update. The
// `after_long_help` sections are structured for LLM consumption: an LLM
// reading `cnc-content --help` should be able to generate correct
// commands without running the tool or reading source code.

const ABOUT: &str = "\
C&C content manager — download, verify, and manage Command & Conquer game content";

const AFTER_HELP: &str = "\
SUPPORTED GAMES:
  ra         Command & Conquer: Red Alert       Freeware (EA, 2008) — downloadable
  td         Command & Conquer: Tiberian Dawn   Freeware (EA, 2007) — downloadable
  dune2      Dune II: Building of a Dynasty     NOT freeware — local source only
  dune2000   Dune 2000                          NOT freeware — local source only

COMMON WORKFLOWS:
  Download Red Alert (default game):
    cnc-content status
    cnc-content download
    cnc-content download --all

  Download Tiberian Dawn:
    cnc-content -g td download

  Install from a local disc/Steam/GOG source:
    cnc-content detect
    cnc-content install /path/to/source

  Verify installed content:
    cnc-content verify

  Share downloaded content via P2P:
    cnc-content seed-config always

ENVIRONMENT:
  CNC_CONTENT_ROOT  Override the content directory (same as --content-dir)

NOTE ON FREEWARE:
  Only Red Alert and Tiberian Dawn are EA-declared freeware and can be
  downloaded. Dune 2 and Dune 2000 require a local copy (disc, GOG, etc.)
  and can only be installed via the 'install' command.";

const DOWNLOAD_AFTER_HELP: &str = "\
PACKAGE NAMES (--package <NAME>):
  Red Alert (ra):
    base, aftermath, desert, music, movies-allied, movies-soviet,
    music-cs, music-am, full-discs, full-set

  Tiberian Dawn (td):
    base, music, movies-gdi, movies-nod, covertops, gdi-iso, nod-iso

SEEDING POLICIES (--seed <POLICY>):
  pause    Seed content, pause during online play (default)
  always   Seed continuously, even during online play
  keep     Keep archives but never upload to peers
  delete   Extract content, then delete archives

EXAMPLES:
  cnc-content download                          # required RA content
  cnc-content download --all                    # required + optional
  cnc-content download --package music          # just the music
  cnc-content -g td download                    # Tiberian Dawn content
  cnc-content download --seed always            # seed continuously";

const INSTALL_AFTER_HELP: &str = "\
PACKAGE NAMES (--package <NAME>):
  Red Alert:       base, aftermath, desert, music, movies-allied,
                   movies-soviet, music-cs, music-am
  Tiberian Dawn:   base, covertops, music, movies-gdi, movies-nod
  Dune 2:          base
  Dune 2000:       base

EXAMPLES:
  cnc-content detect                            # find local sources first
  cnc-content install D:\\                       # install from disc
  cnc-content install /path/to/steam/ra         # install from Steam
  cnc-content install D:\\ --package music       # install just music";

const SEED_CONFIG_AFTER_HELP: &str = "\
POLICIES:
  pause    Seed content, pause during online play (default)
  always   Seed continuously, even during online play
  keep     Keep archives but never upload to peers
  delete   Extract content, then delete archives

EXAMPLES:
  cnc-content seed-config                       # show current policy
  cnc-content seed-config always                # seed continuously
  cnc-content seed-config delete                # minimize disk usage";

// ── CLI struct ────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "cnc-content",
    about = ABOUT,
    after_long_help = AFTER_HELP,
    version
)]
struct Cli {
    /// Content directory override (default: portable, next to executable).
    #[arg(long, global = true, env = "CNC_CONTENT_ROOT")]
    content_dir: Option<PathBuf>,

    /// Use OpenRA's content directory so both engines share the same files.
    #[arg(long, global = true)]
    openra: bool,

    /// Game to manage [possible values: ra, td, dune2, dune2000].
    #[arg(long, short, global = true, default_value = "ra")]
    game: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Show installed/missing content packages for the selected game.
    Status,

    /// Download freeware content from HTTP mirrors or P2P.
    ///
    /// Downloads required packages by default. Use --all for optional
    /// content (music, FMV cutscenes). Only works for freeware games
    /// (ra, td); non-freeware games require the 'install' command.
    #[command(after_long_help = DOWNLOAD_AFTER_HELP)]
    Download {
        /// Download a specific package by name (see PACKAGE NAMES below).
        #[arg(long, value_name = "NAME")]
        package: Option<String>,

        /// Download ALL content including optional (music, FMV cutscenes).
        #[arg(long)]
        all: bool,

        /// Seeding policy override [possible values: pause, always, keep, delete].
        ///
        /// If omitted, uses the persisted policy from `seed-config`.
        #[arg(long, value_name = "POLICY")]
        seed: Option<String>,
    },

    /// Verify installed content integrity using SHA-256 checksums.
    ///
    /// Hashes every installed file and compares against the expected
    /// digest. Reports corrupted or missing files. Uses parallel hashing
    /// on multi-core systems for faster verification.
    Verify,

    /// Identify a content source at a given path (disc, Steam, GOG, etc.).
    ///
    /// Checks the directory against all known source fingerprints and
    /// reports which packages can be installed from it.
    Identify {
        /// Path to the source directory (disc mount, Steam library, etc.).
        path: PathBuf,
    },

    /// Auto-detect all local game installs (Steam, Origin, GOG, OpenRA, disc).
    ///
    /// Scans platform-specific paths for known C&C installations and
    /// reports which content packages each source can provide.
    Detect,

    /// Install content from a local source (disc, Steam, GOG, Origin).
    ///
    /// Identifies the source at <PATH>, then extracts content using the
    /// matching install recipe. Works for all games including non-freeware
    /// titles (Dune 2, Dune 2000).
    #[command(after_long_help = INSTALL_AFTER_HELP)]
    Install {
        /// Path to the source directory.
        path: PathBuf,

        /// Install only a specific package by name (see PACKAGE NAMES below).
        #[arg(long, value_name = "NAME")]
        package: Option<String>,
    },

    /// Remove all downloaded content from the content directory.
    Clean {
        /// Skip the confirmation prompt.
        #[arg(long, short = 'y')]
        yes: bool,
    },

    /// List all supported games with freeware/download status.
    Games,

    /// Show or change the P2P seeding policy.
    ///
    /// Controls how downloaded content is shared with other BitTorrent
    /// peers. The policy persists across runs in the config file.
    #[command(after_long_help = SEED_CONFIG_AFTER_HELP)]
    SeedConfig {
        /// New policy to set [possible values: pause, always, keep, delete].
        /// If omitted, shows the current policy.
        #[arg(value_name = "POLICY")]
        policy: Option<String>,
    },

    /// Generate .torrent files and print info hashes for seeding.
    ///
    /// Downloads each available package (if not already cached), creates
    /// a .torrent file, and prints the info_hash for use in the download
    /// registry. This is a developer/maintainer command.
    TorrentCreate {
        /// Output directory for .torrent files (default: current directory).
        #[arg(long, short)]
        output: Option<PathBuf>,

        /// Only generate for packages matching this game [possible values: ra, td].
        #[arg(long, value_name = "GAME")]
        game_filter: Option<String>,
    },
}

fn main() {
    let cli = Cli::parse();

    let game = GameId::from_slug(&cli.game).unwrap_or_else(|| {
        eprintln!("Unknown game: '{}'. Use: ra, td, or dune2", cli.game);
        process::exit(1);
    });

    let content_root = if let Some(dir) = cli.content_dir {
        dir
    } else if cli.openra {
        cnc_content::openra_content_root().unwrap_or_else(|| {
            eprintln!("Cannot determine OpenRA content path on this platform.");
            process::exit(1);
        })
    } else {
        cnc_content::default_content_root_for_game(game)
    };

    match cli.command {
        Command::Status => cmd_status(&content_root, game),
        Command::Download { package, all, seed } => {
            let seeding_policy = match seed {
                Some(ref s) => cnc_content::SeedingPolicy::from_str_loose(s).unwrap_or_else(|| {
                    eprintln!("Unknown seeding policy: '{s}'. Use: pause, always, keep, or delete");
                    process::exit(1);
                }),
                None => cnc_content::config::Config::load().seeding_policy,
            };
            cmd_download(&content_root, game, package.as_deref(), all, seeding_policy)
        }
        Command::Verify => cmd_verify(&content_root, game),
        Command::Identify { path } => cmd_identify(&path),
        Command::Detect => cmd_detect(),
        Command::Install { path, package } => cmd_install(&content_root, &path, package.as_deref()),
        Command::Clean { yes } => cmd_clean(&content_root, yes),
        Command::Games => cmd_games(),
        Command::SeedConfig { policy } => cmd_seed_config(policy.as_deref()),
        Command::TorrentCreate {
            output,
            game_filter,
        } => cmd_torrent_create(output.as_deref(), game_filter.as_deref()),
    }
}

fn cmd_games() {
    println!("Supported games:\n");
    for &game in GameId::ALL {
        let slug = game.slug();
        let title = game.title();
        let legal = match game {
            GameId::RedAlert => "Freeware (EA, 2008) — downloadable",
            GameId::TiberianDawn => "Freeware (EA, 2007) — downloadable",
            GameId::Dune2 => "NOT freeware — local source only",
            GameId::Dune2000 => "NOT freeware — local source only",
        };
        println!("  {slug:<10} {title}");
        println!("             Status: {legal}");
        println!();
    }
    println!("Freeware games:     cnc-content --game <slug> download");
    println!("Non-freeware games: cnc-content --game <slug> install <path>");
}

fn cmd_seed_config(policy: Option<&str>) {
    use cnc_content::SeedingPolicy;

    if let Some(name) = policy {
        let policy = SeedingPolicy::from_str_loose(name).unwrap_or_else(|| {
            eprintln!("Unknown seeding policy: '{name}'");
            eprintln!("Available: pause, always, keep, delete");
            process::exit(1);
        });

        let mut config = cnc_content::config::Config::load();
        config.seeding_policy = policy;
        if let Err(e) = config.save() {
            eprintln!("Warning: could not save config: {e}");
        }

        println!("Seeding policy set to: {}", policy.label());
        println!();
        println!(
            "  Allows seeding:    {}",
            if policy.allows_seeding() { "yes" } else { "no" }
        );
        println!(
            "  Retains archives:  {}",
            if policy.retains_archives() {
                "yes"
            } else {
                "no"
            }
        );
        println!();
        match policy {
            SeedingPolicy::PauseDuringOnlinePlay => {
                println!("Downloads will be shared with other players when idle.");
                println!("Seeding automatically pauses during online gameplay.");
            }
            SeedingPolicy::SeedAlways => {
                println!("Downloads will be shared continuously, even during online play.");
                println!("Recommended for users with high bandwidth.");
            }
            SeedingPolicy::KeepNoSeed => {
                println!("Downloaded archives are kept for fast re-extraction.");
                println!("No data is uploaded to other players.");
            }
            SeedingPolicy::ExtractAndDelete => {
                println!("Downloaded archives are deleted after extraction.");
                println!("Minimizes disk usage. Re-download required for repairs.");
            }
        }
    } else {
        let config = cnc_content::config::Config::load();
        println!("Current seeding policy: {}", config.seeding_policy.label());
        println!();
        println!("Available policies:");
        for (slug, policy) in [
            ("pause", SeedingPolicy::PauseDuringOnlinePlay),
            ("always", SeedingPolicy::SeedAlways),
            ("keep", SeedingPolicy::KeepNoSeed),
            ("delete", SeedingPolicy::ExtractAndDelete),
        ] {
            let marker = if policy == config.seeding_policy {
                " (current)"
            } else {
                ""
            };
            println!("  {slug:<10} {}{}", policy.label(), marker);
        }
        println!();
        println!("Set with: cnc-content seed-config <policy>");
        println!("Or per-download: cnc-content download --seed <policy>");
    }
}

fn cmd_status(content_root: &std::path::Path, game: GameId) {
    println!("{}", game.title());
    println!("Content directory: {}", content_root.display());
    println!();

    let game_packages = cnc_content::packages_for_game(game);
    let required: Vec<_> = game_packages.iter().filter(|p| p.required).collect();
    let optional: Vec<_> = game_packages.iter().filter(|p| !p.required).collect();

    if !required.is_empty() {
        println!("  Required:");
        for pkg in &required {
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
    }

    if !optional.is_empty() {
        println!();
        println!("  Optional:");
        for pkg in &optional {
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
    }

    println!();
    if cnc_content::is_content_complete(content_root, game) {
        println!("All required content is installed.");
        let optional_missing = cnc_content::missing_packages(content_root, game)
            .iter()
            .filter(|p| !p.required)
            .count();
        if optional_missing > 0 {
            println!(
                "{optional_missing} optional package(s) available. Run `cnc-content --game {} download --all` to fetch them.",
                game.slug()
            );
        }
    } else {
        let missing = cnc_content::missing_required_packages(content_root, game);
        println!(
            "{} required package(s) missing. Run `cnc-content --game {} download` to fetch them.",
            missing.len(),
            game.slug()
        );
    }
}

fn cmd_download(
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

fn cmd_verify(content_root: &std::path::Path, game: GameId) {
    if !content_root.exists() {
        eprintln!(
            "Content directory does not exist: {}",
            content_root.display()
        );
        process::exit(1);
    }

    // Verify all installed packages for this game.
    let installed_ids: Vec<cnc_content::PackageId> = cnc_content::packages_for_game(game)
        .iter()
        .filter(|p| p.test_files.iter().all(|f| content_root.join(f).exists()))
        .map(|p| p.id)
        .collect();

    if installed_ids.is_empty() {
        println!("No {} content packages are installed.", game.title());
        return;
    }

    println!("Generating content manifest...");
    match cnc_content::verify::generate_manifest(content_root, game.slug(), "v1", &installed_ids) {
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
            println!("No known C&C content source identified at this path.");
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
        println!("  - GOG.com / GOG Galaxy installs");
        println!("  - Windows registry (legacy Westwood/EA)");
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

fn cmd_torrent_create(output_dir: Option<&std::path::Path>, game_filter: Option<&str>) {
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

fn format_size(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.1} GB", bytes as f64 / 1_073_741_824.0)
    } else if bytes >= 1_000_000 {
        format!("{:.1} MB", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1000 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}
