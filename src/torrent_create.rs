// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! BitTorrent `.torrent` file creation — generates torrent metadata from local
//! files so that content packages can be distributed via P2P.
//!
//! This module is always available (no feature gate) because it only depends on
//! `sha1` which is a base dependency. It produces:
//!
//! - A `.torrent` file (bencoded metadata with piece hashes)
//! - The `info_hash` (SHA-1 of the bencoded info dictionary)
//!
//! ## Piece hashing
//!
//! Files are split into fixed-size pieces (default 256 KiB). Each piece is
//! SHA-1 hashed. The concatenated hashes form the `pieces` field in the
//! torrent info dictionary. The `info_hash` is the SHA-1 of the bencoded
//! info dictionary — this is what identifies the torrent on the DHT and
//! tracker networks.

use std::io::{self, Read};
use std::path::Path;

use thiserror::Error;

/// Default piece length: 256 KiB. Standard for files under ~1 GiB.
pub const DEFAULT_PIECE_LENGTH: u64 = 256 * 1024;

/// Errors from torrent creation.
#[derive(Debug, Error)]
pub enum TorrentCreateError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("file is empty: {0}")]
    EmptyFile(String),
}

/// Result of creating a torrent: the bencoded `.torrent` file and its info hash.
#[derive(Debug, Clone)]
pub struct TorrentMetadata {
    /// The full bencoded `.torrent` file content — write this to a `.torrent` file.
    pub torrent_data: Vec<u8>,
    /// The info hash (SHA-1 of the bencoded info dict), lowercase hex.
    pub info_hash: String,
    /// Number of pieces the file was split into.
    pub piece_count: u64,
    /// File size in bytes.
    pub file_size: u64,
}

/// Creates torrent metadata for a single file.
///
/// The resulting `.torrent` includes the public trackers from
/// `data/trackers.txt` and the info hash needed for magnet URI construction.
pub fn create_torrent(
    file_path: &Path,
    piece_length: u64,
    trackers: &[&str],
) -> Result<TorrentMetadata, TorrentCreateError> {
    use sha1::{Digest, Sha1};

    let file_name = file_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "content.zip".to_string());

    let metadata = std::fs::metadata(file_path)?;
    let file_size = metadata.len();
    if file_size == 0 {
        return Err(TorrentCreateError::EmptyFile(
            file_path.display().to_string(),
        ));
    }

    // ── Hash pieces ─────────────────────────────────────────────────
    let mut file = std::fs::File::open(file_path)?;
    let mut pieces = Vec::with_capacity((file_size / piece_length + 1) as usize * 20);
    let mut buf = vec![0u8; piece_length as usize];
    let mut piece_count: u64 = 0;

    loop {
        let mut read_total = 0;
        // Read exactly piece_length bytes (or until EOF).
        while read_total < buf.len() {
            let n = file.read(buf.get_mut(read_total..).unwrap_or(&mut []))?;
            if n == 0 {
                break;
            }
            read_total += n;
        }
        if read_total == 0 {
            break;
        }

        let mut hasher = Sha1::new();
        hasher.update(buf.get(..read_total).unwrap_or(&[]));
        pieces.extend_from_slice(hasher.finalize().as_slice());
        piece_count += 1;
    }

    // ── Build info dictionary ───────────────────────────────────────
    // Bencode the info dict manually — it's simple enough that a library
    // dependency is not justified.
    let info_dict = bencode_info_dict(&file_name, file_size, piece_length, &pieces);

    // ── Compute info_hash ───────────────────────────────────────────
    let mut hasher = Sha1::new();
    hasher.update(&info_dict);
    let info_hash_bytes = hasher.finalize();
    let info_hash = crate::verify::hex_encode(info_hash_bytes.as_slice());

    // ── Build full .torrent file ────────────────────────────────────
    let torrent_data = bencode_torrent(&info_dict, trackers);

    Ok(TorrentMetadata {
        torrent_data,
        info_hash,
        piece_count,
        file_size,
    })
}

// ── Bencode helpers ─────────────────────────────────────────────────────

/// Bencodes the info dictionary for a single-file torrent.
fn bencode_info_dict(name: &str, length: u64, piece_length: u64, pieces: &[u8]) -> Vec<u8> {
    // Keys must be sorted lexicographically per the bencode spec.
    // Order: "length", "name", "piece length", "pieces"
    let mut d = Vec::with_capacity(pieces.len() + 256);
    d.push(b'd');

    // "length"
    bencode_string(&mut d, b"length");
    bencode_int(&mut d, length as i64);

    // "name"
    bencode_string(&mut d, b"name");
    bencode_string(&mut d, name.as_bytes());

    // "piece length"
    bencode_string(&mut d, b"piece length");
    bencode_int(&mut d, piece_length as i64);

    // "pieces"
    bencode_string(&mut d, b"pieces");
    bencode_string(&mut d, pieces);

    d.push(b'e');
    d
}

/// Bencodes the full `.torrent` file (metainfo dict).
fn bencode_torrent(info_dict: &[u8], trackers: &[&str]) -> Vec<u8> {
    let mut d = Vec::with_capacity(info_dict.len() + 1024);
    d.push(b'd');

    // "announce" — first tracker
    if let Some(&first) = trackers.first() {
        bencode_string(&mut d, b"announce");
        bencode_string(&mut d, first.as_bytes());
    }

    // "announce-list" — all trackers (BEP 12)
    if trackers.len() > 1 {
        bencode_string(&mut d, b"announce-list");
        d.push(b'l');
        for tracker in trackers {
            d.push(b'l');
            bencode_string(&mut d, tracker.as_bytes());
            d.push(b'e');
        }
        d.push(b'e');
    }

    // "info" — the pre-bencoded info dictionary
    bencode_string(&mut d, b"info");
    d.extend_from_slice(info_dict);

    d.push(b'e');
    d
}

fn bencode_string(out: &mut Vec<u8>, s: &[u8]) {
    out.extend_from_slice(s.len().to_string().as_bytes());
    out.push(b':');
    out.extend_from_slice(s);
}

fn bencode_int(out: &mut Vec<u8>, n: i64) {
    out.push(b'i');
    out.extend_from_slice(n.to_string().as_bytes());
    out.push(b'e');
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Bencode helpers ─────────────────────────────────────────────

    /// Bencode string encoding produces length-prefixed format.
    #[test]
    fn bencode_string_encoding() {
        let mut out = Vec::new();
        bencode_string(&mut out, b"hello");
        assert_eq!(out, b"5:hello");
    }

    /// Bencode integer encoding wraps in i...e.
    #[test]
    fn bencode_int_encoding() {
        let mut out = Vec::new();
        bencode_int(&mut out, 42);
        assert_eq!(out, b"i42e");
    }

    /// Bencode integer zero.
    #[test]
    fn bencode_int_zero() {
        let mut out = Vec::new();
        bencode_int(&mut out, 0);
        assert_eq!(out, b"i0e");
    }

    /// Info dict has sorted keys per bencode spec.
    #[test]
    fn info_dict_keys_are_sorted() {
        let pieces = [0u8; 20]; // one fake piece hash
        let dict = bencode_info_dict("test.zip", 1024, 256, &pieces);
        let s = String::from_utf8_lossy(&dict);
        // Keys must appear in order: length, name, piece length, pieces
        let len_pos = s.find("6:length").unwrap();
        let name_pos = s.find("4:name").unwrap();
        let pl_pos = s.find("12:piece length").unwrap();
        let pieces_pos = s.find("6:pieces").unwrap();
        assert!(len_pos < name_pos, "length before name");
        assert!(name_pos < pl_pos, "name before piece length");
        assert!(pl_pos < pieces_pos, "piece length before pieces");
    }

    // ── Torrent creation ────────────────────────────────────────────

    /// Creating a torrent from a real file produces a valid info hash.
    #[test]
    fn create_torrent_produces_valid_info_hash() {
        let tmp = std::env::temp_dir().join("cnc-torrent-create");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        // Write a test file — 512 KiB of repeating data (2 pieces at 256 KiB).
        let data = vec![0xABu8; 512 * 1024];
        let file_path = tmp.join("test-content.zip");
        std::fs::write(&file_path, &data).unwrap();

        let result = create_torrent(&file_path, DEFAULT_PIECE_LENGTH, &[]).unwrap();

        assert_eq!(result.file_size, 512 * 1024);
        assert_eq!(result.piece_count, 2);
        assert_eq!(result.info_hash.len(), 40);
        assert!(result
            .info_hash
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert!(!result.torrent_data.is_empty());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Torrent with trackers includes announce and announce-list.
    #[test]
    fn create_torrent_includes_trackers() {
        let tmp = std::env::temp_dir().join("cnc-torrent-trackers");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let file_path = tmp.join("data.bin");
        std::fs::write(&file_path, b"some content for torrent").unwrap();

        let trackers = &[
            "udp://tracker.opentrackr.org:1337/announce",
            "udp://open.stealth.si:80/announce",
        ];
        let result = create_torrent(&file_path, DEFAULT_PIECE_LENGTH, trackers).unwrap();

        let torrent_str = String::from_utf8_lossy(&result.torrent_data);
        assert!(
            torrent_str.contains("tracker.opentrackr.org"),
            "should contain first tracker"
        );
        assert!(
            torrent_str.contains("open.stealth.si"),
            "should contain second tracker"
        );
        assert!(
            torrent_str.contains("announce-list"),
            "should have announce-list for multiple trackers"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Empty file is rejected.
    #[test]
    fn create_torrent_rejects_empty_file() {
        let tmp = std::env::temp_dir().join("cnc-torrent-empty");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let file_path = tmp.join("empty.zip");
        std::fs::write(&file_path, b"").unwrap();

        let result = create_torrent(&file_path, DEFAULT_PIECE_LENGTH, &[]);
        assert!(matches!(result, Err(TorrentCreateError::EmptyFile(_))));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Info hash is deterministic — same file always produces same hash.
    #[test]
    fn create_torrent_deterministic() {
        let tmp = std::env::temp_dir().join("cnc-torrent-deterministic");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let file_path = tmp.join("stable.bin");
        std::fs::write(&file_path, b"deterministic content").unwrap();

        let r1 = create_torrent(&file_path, DEFAULT_PIECE_LENGTH, &[]).unwrap();
        let r2 = create_torrent(&file_path, DEFAULT_PIECE_LENGTH, &[]).unwrap();
        assert_eq!(r1.info_hash, r2.info_hash);
        assert_eq!(r1.torrent_data, r2.torrent_data);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Error Display for EmptyFile includes the path.
    #[test]
    fn torrent_create_error_display_empty_file() {
        let err = TorrentCreateError::EmptyFile("test.zip".into());
        let msg = err.to_string();
        assert!(msg.contains("test.zip"), "should contain path: {msg}");
    }
}
