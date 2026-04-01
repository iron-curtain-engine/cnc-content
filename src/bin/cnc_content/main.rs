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

mod commands;
mod progress;

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
        Command::Status => commands::status::cmd_status(&content_root, game),
        Command::Download { package, all, seed } => {
            let seeding_policy = match seed {
                Some(ref s) => cnc_content::SeedingPolicy::from_str_loose(s).unwrap_or_else(|| {
                    eprintln!("Unknown seeding policy: '{s}'. Use: pause, always, keep, or delete");
                    process::exit(1);
                }),
                None => cnc_content::config::Config::load().seeding_policy,
            };
            commands::install::cmd_download(
                &content_root,
                game,
                package.as_deref(),
                all,
                seeding_policy,
            )
        }
        Command::Verify => commands::status::cmd_verify(&content_root, game),
        Command::Identify { path } => commands::status::cmd_identify(&path),
        Command::Detect => commands::status::cmd_detect(),
        Command::Install { path, package } => {
            commands::install::cmd_install(&content_root, &path, package.as_deref())
        }
        Command::Clean { yes } => commands::install::cmd_clean(&content_root, yes),
        Command::Games => commands::status::cmd_games(),
        Command::SeedConfig { policy } => commands::status::cmd_seed_config(policy.as_deref()),
        Command::TorrentCreate {
            output,
            game_filter,
        } => commands::install::cmd_torrent_create(output.as_deref(), game_filter.as_deref()),
    }
}
