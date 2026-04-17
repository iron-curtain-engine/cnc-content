// SPDX-License-Identifier: MIT OR Apache-2.0

//! `p2p-distribute` CLI — inspect, create, and test BitTorrent features.
//!
//! This binary exercises every layer of the p2p-distribute library:
//! torrent file creation and inspection, PEM encoding, bencode
//! decoding, credential issuance and verification, and storage
//! integrity scrubbing.
//!
//! Feature-gated behind `cli` so library consumers don't pull in
//! `clap` or `mimalloc`.

// Use mimalloc for reduced fragmentation during download/hashing workloads.
#[cfg(not(target_arch = "wasm32"))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use std::process;

use clap::{Parser, Subcommand};

mod commands;

// ── CLI structure ───────────────────────────────────────────────────

const ABOUT: &str = "\
p2p-distribute — BitTorrent toolkit and P2P distribution utility";

const AFTER_HELP: &str = "\
SUBCOMMAND GROUPS:
  torrent    Create, inspect, and encode .torrent files
  credential Issue and verify peer credentials for CDN groups
  bencode    Decode and inspect raw bencode data
  scrub      Verify storage integrity against piece hashes

EXAMPLES:
  Inspect a .torrent file:
    p2p-distribute torrent info my-file.torrent

  Create a .torrent from a local file:
    p2p-distribute torrent create my-archive.zip

  PEM-encode a .torrent for text sharing:
    p2p-distribute torrent pem-encode my-file.torrent

  Issue a CDN mirror credential:
    p2p-distribute credential issue --network my-cdn --role mirror --key secret.key

  Decode raw bencode from stdin:
    p2p-distribute bencode decode data.bin";

#[derive(Parser)]
#[command(
    name = "p2p-distribute",
    about = ABOUT,
    after_long_help = AFTER_HELP,
    version,
    propagate_version = true
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Torrent file operations: create, inspect, PEM encode/decode.
    #[command(subcommand)]
    Torrent(commands::torrent::TorrentCommand),

    /// Peer credential operations: issue, verify, inspect.
    #[command(subcommand)]
    Credential(commands::credential::CredentialCommand),

    /// Decode and display bencode data.
    Bencode(commands::bencode::BencodeArgs),

    /// Verify storage integrity against torrent piece hashes.
    Scrub(commands::scrub::ScrubArgs),
}

// ── Main ────────────────────────────────────────────────────────────

fn main() {
    let cli = Cli::parse();

    let result = match cli.command {
        Command::Torrent(cmd) => commands::torrent::run(cmd),
        Command::Credential(cmd) => commands::credential::run(cmd),
        Command::Bencode(args) => commands::bencode::run(args),
        Command::Scrub(args) => commands::scrub::run(args),
    };

    if let Err(e) = result {
        eprintln!("error: {e}");
        process::exit(1);
    }
}
