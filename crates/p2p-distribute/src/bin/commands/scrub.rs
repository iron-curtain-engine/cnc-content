// SPDX-License-Identifier: MIT OR Apache-2.0

//! Scrub subcommand: verify storage integrity against piece hashes.
//!
//! Reads piece data from a flat file, compares SHA-1 hashes against
//! the torrent's piece hashes, and reports corrupt or unreadable pieces.
//! This exercises the `scrub` and `storage` modules end-to-end.

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use clap::Args;

use p2p_distribute::scrub::{PieceHealth, ScrubConfig, ScrubProgress, StorageScrubber};
use p2p_distribute::storage::{MemoryStorage, PieceStorage};
use p2p_distribute::torrent_info::TorrentInfo;

/// Verify storage integrity against a torrent's piece hashes.
#[derive(Args)]
pub struct ScrubArgs {
    /// Path to the .torrent file containing piece hashes.
    #[arg(short, long)]
    torrent: PathBuf,

    /// Path to the data file to verify.
    data: PathBuf,

    /// Stop on first error instead of checking all pieces.
    #[arg(long)]
    stop_on_error: bool,

    /// Timeout in seconds (0 = no timeout).
    #[arg(long, default_value = "0")]
    timeout: u64,
}

/// Run the scrub command.
pub fn run(args: ScrubArgs) -> Result<(), Box<dyn std::error::Error>> {
    // Parse the torrent to get piece hashes and piece length.
    let torrent_data = fs::read(&args.torrent)?;
    let torrent_value = p2p_distribute::decode(&torrent_data)?;

    let info_dict = torrent_value
        .dict_get(b"info")
        .ok_or("missing 'info' dictionary in torrent file")?;

    let piece_length = info_dict
        .dict_get(b"piece length")
        .and_then(|v| v.as_int())
        .ok_or("missing 'piece length' in info dict")? as u64;

    let pieces_raw = info_dict
        .dict_get(b"pieces")
        .and_then(|v| v.as_bytes())
        .ok_or("missing 'pieces' in info dict")?;

    let file_name = info_dict
        .dict_get(b"name")
        .and_then(|v| v.as_bytes())
        .map(|b| String::from_utf8_lossy(b).into_owned())
        .unwrap_or_else(|| "<unknown>".to_string());

    // Read the data file.
    let file_data = fs::read(&args.data)?;
    let file_size = file_data.len() as u64;

    // Build TorrentInfo.
    let info = TorrentInfo {
        piece_length,
        piece_hashes: pieces_raw.to_vec(),
        file_size,
        file_name: file_name.clone(),
    };

    // Load data into memory storage (piece by piece).
    let storage = MemoryStorage::new(file_size);
    let piece_count = info.piece_count();
    for idx in 0..piece_count {
        let offset = info.piece_offset(idx);
        let size = info.piece_size(idx) as usize;
        let byte_offset = offset as usize;
        if let Some(chunk) = file_data.get(byte_offset..byte_offset.saturating_add(size)) {
            let _ = storage.write_piece(idx, offset, chunk);
        }
    }

    // Configure scrub.
    let config = ScrubConfig {
        continue_on_error: !args.stop_on_error,
        deadline: if args.timeout > 0 {
            Some(Duration::from_secs(args.timeout))
        } else {
            None
        },
    };

    let scrubber = StorageScrubber::new(&info, config);

    println!(
        "Scrubbing {} ({} pieces, {} bytes)...",
        file_name, piece_count, file_size
    );
    println!();

    // Run scrub with progress reporting.
    let report = scrubber.scrub(&storage, &mut |progress| match progress {
        ScrubProgress::PieceChecked {
            piece_index,
            health,
            checked,
            total,
        } => match health {
            PieceHealth::Corrupt { .. } => {
                eprintln!("  CORRUPT piece {piece_index} ({checked}/{total})");
            }
            PieceHealth::Unreadable { ref detail } => {
                eprintln!("  UNREADABLE piece {piece_index}: {detail} ({checked}/{total})");
            }
            _ => {}
        },
        ScrubProgress::Stopped {
            checked,
            total,
            reason,
        } => {
            eprintln!("  Stopped after {checked}/{total} pieces: {reason:?}");
        }
    })?;

    // Display summary.
    println!("Scrub complete in {:.1}s", report.elapsed().as_secs_f64());
    println!("  Total:      {}", report.total_pieces());
    println!("  Healthy:    {}", report.healthy_count());
    println!("  Corrupt:    {}", report.corrupt_count());
    println!("  Unreadable: {}", report.unreadable_count());
    println!("  Skipped:    {}", report.skipped_count());

    if report.is_clean() {
        println!();
        println!("All pieces verified OK.");
    } else {
        println!();
        if report.corrupt_count() > 0 {
            println!("Corrupt pieces: {:?}", report.corrupt_piece_indices());
        }
        if report.unreadable_count() > 0 {
            println!("Unreadable pieces: {:?}", report.unreadable_piece_indices());
        }
        return Err("scrub found integrity errors".into());
    }

    Ok(())
}
