// SPDX-License-Identifier: MIT OR Apache-2.0

//! Bencode subcommand: decode and pretty-print raw bencode data.
//!
//! Useful for inspecting .torrent files, tracker responses, or any
//! bencoded payload. Prints a human-readable tree representation.

use std::fs;
use std::path::PathBuf;

use clap::Args;

/// Decode and display bencode data from a file.
#[derive(Args)]
pub struct BencodeArgs {
    /// Path to the bencoded file.
    file: PathBuf,

    /// Maximum nesting depth to display (default: 16).
    #[arg(long, default_value = "16")]
    max_depth: usize,
}

/// Run the bencode decode command.
pub fn run(args: BencodeArgs) -> Result<(), Box<dyn std::error::Error>> {
    let data = fs::read(&args.file)?;
    let value = p2p_distribute::decode(&data)?;

    println!("Decoded {} bytes from {}", data.len(), args.file.display());
    println!();
    print_value(&value, 0, args.max_depth);

    Ok(())
}

// ── Pretty printer ──────────────────────────────────────────────────

/// Recursively print a BencodeValue as an indented tree.
fn print_value(value: &p2p_distribute::BencodeValue, depth: usize, max_depth: usize) {
    let indent = "  ".repeat(depth);

    if depth >= max_depth {
        println!("{indent}...(depth limit)");
        return;
    }

    match value {
        p2p_distribute::BencodeValue::Int(n) => {
            println!("{indent}int: {n}");
        }
        p2p_distribute::BencodeValue::Bytes(b) => {
            // Try to display as UTF-8 if it looks textual, otherwise hex-summarize.
            if let Ok(s) = std::str::from_utf8(b) {
                if s.chars().all(|c| !c.is_control() || c == '\n') && b.len() <= 200 {
                    println!("{indent}str: \"{s}\"");
                } else {
                    println!(
                        "{indent}str: \"{}...\" ({} bytes)",
                        &s.get(..60).unwrap_or(s),
                        b.len()
                    );
                }
            } else if b.len() <= 40 {
                println!(
                    "{indent}bytes: {} ({} bytes)",
                    p2p_distribute::hex_encode(b),
                    b.len()
                );
            } else {
                // Show first 20 bytes hex.
                let preview = p2p_distribute::hex_encode(b.get(..20).unwrap_or(b));
                println!("{indent}bytes: {preview}... ({} bytes)", b.len());
            }
        }
        p2p_distribute::BencodeValue::List(items) => {
            println!("{indent}list ({} items):", items.len());
            for (i, item) in items.iter().enumerate() {
                println!("{indent}  [{i}]:");
                print_value(item, depth + 2, max_depth);
            }
        }
        p2p_distribute::BencodeValue::Dict(entries) => {
            println!("{indent}dict ({} keys):", entries.len());
            for (key, val) in entries {
                let key_str = String::from_utf8_lossy(key);
                println!("{indent}  \"{key_str}\":");
                print_value(val, depth + 2, max_depth);
            }
        }
    }
}
