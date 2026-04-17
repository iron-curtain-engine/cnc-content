// SPDX-License-Identifier: MIT OR Apache-2.0

//! Torrent subcommands: create, info, pem-encode, pem-decode.
//!
//! These exercise the torrent creation pipeline, metadata parsing,
//! and PEM text encoding — the core data-plane features of the crate.

use std::fs;
use std::path::PathBuf;

use clap::Subcommand;

/// Torrent file operations.
#[derive(Subcommand)]
pub enum TorrentCommand {
    /// Display metadata from a .torrent file.
    Info {
        /// Path to the .torrent file.
        file: PathBuf,
    },

    /// Create a .torrent file from a local file.
    Create {
        /// Path to the file to create a torrent for.
        file: PathBuf,

        /// Output path for the .torrent file.
        /// Defaults to <file>.torrent alongside the input.
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Piece length in bytes (default: auto-sized based on file size).
        #[arg(long)]
        piece_length: Option<u64>,

        /// Tracker announce URL (repeatable).
        #[arg(short, long)]
        tracker: Vec<String>,

        /// Web seed URL (repeatable, BEP 19).
        #[arg(short, long)]
        web_seed: Vec<String>,
    },

    /// PEM-encode a .torrent file for text-safe sharing.
    PemEncode {
        /// Path to the .torrent file to encode.
        file: PathBuf,
    },

    /// Decode a PEM-encoded .torrent from a text file.
    PemDecode {
        /// Path to the PEM text file.
        file: PathBuf,

        /// Output path for the decoded .torrent file.
        #[arg(short, long)]
        output: PathBuf,
    },
}

/// Run the torrent subcommand.
pub fn run(cmd: TorrentCommand) -> Result<(), Box<dyn std::error::Error>> {
    match cmd {
        TorrentCommand::Info { file } => cmd_info(&file),
        TorrentCommand::Create {
            file,
            output,
            piece_length,
            tracker,
            web_seed,
        } => cmd_create(&file, output.as_deref(), piece_length, &tracker, &web_seed),
        TorrentCommand::PemEncode { file } => cmd_pem_encode(&file),
        TorrentCommand::PemDecode { file, output } => cmd_pem_decode(&file, &output),
    }
}

// ── info ────────────────────────────────────────────────────────────

/// Decode and display .torrent metadata.
fn cmd_info(path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    let data = fs::read(path)?;
    let value = p2p_distribute::decode(&data)?;

    // Extract the info dictionary for TorrentInfo.
    let info_dict = value
        .dict_get(b"info")
        .ok_or("missing 'info' dictionary in torrent file")?;

    // Re-encode the info dict to feed into TorrentInfo parsing.
    let info_bytes = p2p_distribute::encode(info_dict);
    let info_value = p2p_distribute::decode(&info_bytes)?;

    // Extract fields from info dictionary.
    let name = info_value
        .dict_get(b"name")
        .and_then(|v| v.as_bytes())
        .map(|b| String::from_utf8_lossy(b).into_owned())
        .unwrap_or_else(|| "<unknown>".to_string());

    let piece_length = info_value
        .dict_get(b"piece length")
        .and_then(|v| v.as_int())
        .unwrap_or(0);

    let pieces = info_value
        .dict_get(b"pieces")
        .and_then(|v| v.as_bytes())
        .unwrap_or(&[]);

    let length = info_value
        .dict_get(b"length")
        .and_then(|v| v.as_int())
        .unwrap_or(0);

    let piece_count = if piece_length > 0 {
        pieces.len() / 20
    } else {
        0
    };

    // Display announce URLs.
    let announce = value
        .dict_get(b"announce")
        .and_then(|v| v.as_bytes())
        .map(|b| String::from_utf8_lossy(b).into_owned());

    let announce_list = value.dict_get(b"announce-list").and_then(|v| v.as_list());

    // Display web seeds.
    let url_list = value.dict_get(b"url-list").and_then(|v| v.as_list());

    // Compute info hash (SHA-1 of the bencoded info dictionary).
    let info_hash = {
        use sha1::Digest;
        let mut hasher = sha1::Sha1::new();
        hasher.update(&info_bytes);
        let digest = hasher.finalize();
        p2p_distribute::hex_encode(&digest)
    };

    println!("Name:         {name}");
    println!("Info hash:    {info_hash}");
    println!(
        "File size:    {length} bytes ({:.2} MB)",
        length as f64 / 1_048_576.0
    );
    println!(
        "Piece length: {piece_length} bytes ({:.0} KB)",
        piece_length as f64 / 1024.0
    );
    println!("Pieces:       {piece_count}");

    if let Some(url) = announce {
        println!("Announce:     {url}");
    }

    if let Some(tiers) = announce_list {
        println!("Announce list:");
        for (i, tier) in tiers.iter().enumerate() {
            if let Some(urls) = tier.as_list() {
                for url in urls {
                    if let Some(u) = url.as_bytes() {
                        println!("  tier {i}: {}", String::from_utf8_lossy(u));
                    }
                }
            }
        }
    }

    if let Some(seeds) = url_list {
        println!("Web seeds:");
        for s in seeds {
            if let Some(u) = s.as_bytes() {
                println!("  {}", String::from_utf8_lossy(u));
            }
        }
    }

    Ok(())
}

// ── create ──────────────────────────────────────────────────────────

/// Create a .torrent file from a local file.
fn cmd_create(
    file: &std::path::Path,
    output: Option<&std::path::Path>,
    piece_length: Option<u64>,
    trackers: &[String],
    web_seeds: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    let pl = piece_length.unwrap_or_else(|| {
        // Auto-size based on file metadata.
        let size = fs::metadata(file).map(|m| m.len()).unwrap_or(0);
        p2p_distribute::recommended_piece_length(size)
    });

    let tracker_refs: Vec<&str> = trackers.iter().map(|s| s.as_str()).collect();
    let seed_refs: Vec<&str> = web_seeds.iter().map(|s| s.as_str()).collect();

    let meta = p2p_distribute::create_torrent(file, pl, &tracker_refs, &seed_refs)?;

    let out_path = output.map_or_else(
        || {
            let mut p = file.to_path_buf();
            let ext = p
                .extension()
                .map(|e| format!("{}.torrent", e.to_string_lossy()))
                .unwrap_or_else(|| "torrent".to_string());
            p.set_extension(ext);
            p
        },
        |p| p.to_path_buf(),
    );

    fs::write(&out_path, &meta.torrent_data)?;

    println!("Created:      {}", out_path.display());
    println!("Info hash:    {}", meta.info_hash);
    println!("File size:    {} bytes", meta.file_size);
    println!("Pieces:       {}", meta.piece_count);
    println!("Piece length: {pl} bytes");

    Ok(())
}

// ── pem-encode ──────────────────────────────────────────────────────

/// PEM-encode a .torrent file and print to stdout.
fn cmd_pem_encode(path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    let data = fs::read(path)?;
    let pem = p2p_distribute::torrent_pem::encode(&data);
    print!("{pem}");
    Ok(())
}

// ── pem-decode ──────────────────────────────────────────────────────

/// Decode a PEM-encoded .torrent file and write the binary output.
fn cmd_pem_decode(
    pem_path: &std::path::Path,
    output: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let text = fs::read_to_string(pem_path)?;
    let data = p2p_distribute::torrent_pem::decode(&text)?;
    fs::write(output, &data)?;
    println!("Decoded {} bytes → {}", data.len(), output.display());
    Ok(())
}
