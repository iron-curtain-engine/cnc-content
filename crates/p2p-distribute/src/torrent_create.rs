// SPDX-License-Identifier: MIT OR Apache-2.0

//! BitTorrent `.torrent` file creation — generates torrent metadata from local
//! files or streaming byte sources so that content can be distributed via P2P.
//!
//! This module provides two APIs:
//!
//! - **[`create_torrent`]** — single-shot: reads a file from disk, hashes all
//!   pieces, and returns the `.torrent` metadata. Simple but requires the file
//!   to exist on disk first.
//! - **[`TorrentBuilder`]** — streaming/incremental: feed bytes as they arrive
//!   (e.g. from an HTTP response body) and finalize into `.torrent` metadata
//!   when done. Zero disk I/O for the file content — pieces are hashed on the
//!   fly as bytes stream through. This is the preferred API for the maintainer
//!   `torrent-create` command.
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

/// Selects a piece length based on file size.
///
/// - <5 MB: 64 KiB (small packages, HTTP-only threshold)
/// - 5–50 MB: 256 KiB (standard, P2P+HTTP concurrent)
/// - 50–500 MB: 1 MiB (large packs)
/// - >500 MB: 4 MiB (full disc ISOs)
///
/// Larger pieces reduce metadata overhead (fewer SHA-1 hashes in the .torrent)
/// at the cost of coarser granularity for piece selection and web seed Range
/// requests.
pub fn recommended_piece_length(file_size: u64) -> u64 {
    if file_size < 5_000_000 {
        64 * 1024
    } else if file_size < 50_000_000 {
        256 * 1024
    } else if file_size < 500_000_000 {
        1024 * 1024
    } else {
        4 * 1024 * 1024
    }
}

/// Errors from torrent creation.
#[derive(Debug, Error)]
pub enum TorrentCreateError {
    #[error("I/O error: {source}")]
    Io {
        #[from]
        source: io::Error,
    },
    #[error("file is empty: {path}")]
    EmptyFile { path: String },
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
/// The resulting `.torrent` includes the provided trackers, any BEP 19 web seed
/// URLs, and the info hash needed for magnet URI construction.
///
/// `web_seeds` are embedded as the `url-list` key (BEP 19). Any BEP 19-capable
/// client that loads this `.torrent` will treat these HTTP URLs as always-available
/// seeds, fetching individual pieces via HTTP Range requests.
pub fn create_torrent(
    file_path: &Path,
    piece_length: u64,
    trackers: &[&str],
    web_seeds: &[&str],
) -> Result<TorrentMetadata, TorrentCreateError> {
    use sha1::{Digest, Sha1};

    let file_name = file_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "content.zip".to_string());

    let metadata = std::fs::metadata(file_path)?;
    let file_size = metadata.len();

    if file_size == 0 {
        return Err(TorrentCreateError::EmptyFile {
            path: file_path.display().to_string(),
        });
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
    let info_dict = bencode_info_dict(&file_name, file_size, piece_length, &pieces);

    // ── Compute info_hash ───────────────────────────────────────────
    let mut hasher = Sha1::new();
    hasher.update(&info_dict);
    let info_hash_bytes = hasher.finalize();
    let info_hash = crate::hex_encode(info_hash_bytes.as_slice());

    // ── Build full .torrent file ────────────────────────────────────
    let torrent_data = bencode_torrent(&info_dict, trackers, web_seeds);

    Ok(TorrentMetadata {
        torrent_data,
        info_hash,
        piece_count,
        file_size,
    })
}

// ── Streaming torrent builder ───────────────────────────────────────────
//
// Hashes pieces incrementally as bytes stream in. No disk I/O for the
// file content — the caller (e.g. HTTP download) feeds raw bytes and
// the builder accumulates SHA-1 piece hashes on the fly. When all bytes
// have been fed, `finalize()` produces the same `TorrentMetadata` that
// `create_torrent()` would, but without ever needing the file on disk.

/// Incremental torrent metadata builder — hash pieces as bytes stream in.
///
/// # Example
///
/// ```
/// use p2p_distribute::torrent_create::TorrentBuilder;
///
/// let mut builder = TorrentBuilder::new("content.zip", 256 * 1024);
/// builder.write(b"first chunk of data");
/// builder.write(b"second chunk of data");
/// let metadata = builder.finalize(&[], &[]).unwrap();
/// assert!(!metadata.info_hash.is_empty());
/// ```
pub struct TorrentBuilder {
    /// File name for the torrent info dictionary `name` field.
    file_name: String,
    /// Piece length in bytes — must match what all clients use for this
    /// file size so that everyone computes the same `info_hash`.
    piece_length: u64,
    /// Accumulated SHA-1 piece hashes (20 bytes each).
    pieces: Vec<u8>,
    /// SHA-1 hasher for the current (in-progress) piece.
    current_hasher: sha1::Sha1,
    /// Bytes fed into the current piece so far (0..piece_length).
    current_piece_bytes: u64,
    /// Total bytes fed across all pieces.
    total_bytes: u64,
    /// Number of completed pieces.
    piece_count: u64,
}

impl TorrentBuilder {
    /// Creates a new builder for the given file name and piece length.
    ///
    /// The `file_name` becomes the `name` field inside the info dictionary.
    /// It must be identical across all clients for the same content so that
    /// everyone computes the same `info_hash`.
    ///
    /// Use [`recommended_piece_length`] with the expected file size to select
    /// the correct piece length.
    pub fn new(file_name: &str, piece_length: u64) -> Self {
        use sha1::Digest;
        Self {
            file_name: file_name.to_string(),
            piece_length,
            pieces: Vec::new(),
            current_hasher: sha1::Sha1::new(),
            current_piece_bytes: 0,
            total_bytes: 0,
            piece_count: 0,
        }
    }

    /// Feeds a chunk of bytes into the builder.
    ///
    /// Bytes are accumulated into the current piece. When a piece boundary
    /// is reached (every `piece_length` bytes), the piece is finalized and
    /// its SHA-1 hash is appended to the pieces list. Chunks may span
    /// multiple piece boundaries.
    ///
    /// Call this repeatedly as data arrives from the network. Order matters
    /// — bytes must be fed in file order.
    pub fn write(&mut self, data: &[u8]) {
        use sha1::Digest;

        let mut offset = 0;
        while offset < data.len() {
            // How many bytes remain until the current piece is full.
            let piece_remaining = self.piece_length.saturating_sub(self.current_piece_bytes);
            let chunk_remaining = data.len().saturating_sub(offset);
            let take = std::cmp::min(piece_remaining as usize, chunk_remaining);

            if let Some(slice) = data.get(offset..offset.saturating_add(take)) {
                self.current_hasher.update(slice);
                self.current_piece_bytes = self.current_piece_bytes.saturating_add(take as u64);
                self.total_bytes = self.total_bytes.saturating_add(take as u64);
                offset = offset.saturating_add(take);
            }

            // If we've filled a complete piece, finalize it.
            if self.current_piece_bytes >= self.piece_length {
                let hasher = std::mem::replace(&mut self.current_hasher, sha1::Sha1::new());
                self.pieces.extend_from_slice(hasher.finalize().as_slice());
                self.current_piece_bytes = 0;
                self.piece_count += 1;
            }
        }
    }

    /// Returns the total number of bytes fed so far.
    pub fn bytes_written(&self) -> u64 {
        self.total_bytes
    }

    /// Finalizes the builder and produces torrent metadata.
    ///
    /// If there are leftover bytes in the final partial piece, they are
    /// hashed and appended. Returns the same `TorrentMetadata` that
    /// [`create_torrent`] would produce for a file with identical content.
    ///
    /// Returns an error if zero bytes were written (empty content).
    pub fn finalize(
        mut self,
        trackers: &[&str],
        web_seeds: &[&str],
    ) -> Result<TorrentMetadata, TorrentCreateError> {
        use sha1::Digest;

        if self.total_bytes == 0 {
            return Err(TorrentCreateError::EmptyFile {
                path: self.file_name.clone(),
            });
        }

        // Finalize the last partial piece (if any bytes remain).
        if self.current_piece_bytes > 0 {
            let hasher = std::mem::replace(&mut self.current_hasher, sha1::Sha1::new());
            self.pieces.extend_from_slice(hasher.finalize().as_slice());
            self.piece_count += 1;
        }

        // Build the info dictionary and compute info_hash — identical to
        // the create_torrent() code path.
        let info_dict = bencode_info_dict(
            &self.file_name,
            self.total_bytes,
            self.piece_length,
            &self.pieces,
        );

        let mut info_hasher = sha1::Sha1::new();
        info_hasher.update(&info_dict);
        let info_hash = crate::hex_encode(info_hasher.finalize().as_slice());

        let torrent_data = bencode_torrent(&info_dict, trackers, web_seeds);

        Ok(TorrentMetadata {
            torrent_data,
            info_hash,
            piece_count: self.piece_count,
            file_size: self.total_bytes,
        })
    }
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
///
/// Keys are sorted lexicographically per the bencode spec:
/// "announce", "announce-list", "info", "url-list".
/// The `url-list` key implements BEP 19 (web seeding) — each URL points to a
/// complete copy of the file. BEP 19-capable clients use HTTP Range requests
/// to fetch individual pieces from these web seeds.
fn bencode_torrent(info_dict: &[u8], trackers: &[&str], web_seeds: &[&str]) -> Vec<u8> {
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

    // "url-list" — BEP 19 web seed URLs.
    if !web_seeds.is_empty() {
        bencode_string(&mut d, b"url-list");
        if web_seeds.len() == 1 {
            // Single web seed: encode as a plain string (common BEP 19 form).
            bencode_string(
                &mut d,
                web_seeds.first().map(|s| s.as_bytes()).unwrap_or(b""),
            );
        } else {
            // Multiple web seeds: encode as a list of strings.
            d.push(b'l');
            for url in web_seeds {
                bencode_string(&mut d, url.as_bytes());
            }
            d.push(b'e');
        }
    }

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
        let pieces = [0u8; 20];
        let dict = bencode_info_dict("test.zip", 1024, 256, &pieces);
        let s = String::from_utf8_lossy(&dict);
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
        let tmp = std::env::temp_dir().join("p2p-torrent-create");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let data = vec![0xABu8; 512 * 1024];
        let file_path = tmp.join("test-content.zip");
        std::fs::write(&file_path, &data).unwrap();

        let result = create_torrent(&file_path, DEFAULT_PIECE_LENGTH, &[], &[]).unwrap();

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
        let tmp = std::env::temp_dir().join("p2p-torrent-trackers");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let file_path = tmp.join("data.bin");
        std::fs::write(&file_path, b"some content for torrent").unwrap();

        let trackers = &[
            "udp://tracker.opentrackr.org:1337/announce",
            "udp://open.stealth.si:80/announce",
        ];
        let result = create_torrent(&file_path, DEFAULT_PIECE_LENGTH, trackers, &[]).unwrap();

        let torrent_str = String::from_utf8_lossy(&result.torrent_data);
        assert!(torrent_str.contains("tracker.opentrackr.org"));
        assert!(torrent_str.contains("open.stealth.si"));
        assert!(torrent_str.contains("announce-list"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Empty file is rejected.
    #[test]
    fn create_torrent_rejects_empty_file() {
        let tmp = std::env::temp_dir().join("p2p-torrent-empty");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let file_path = tmp.join("empty.zip");
        std::fs::write(&file_path, b"").unwrap();

        let result = create_torrent(&file_path, DEFAULT_PIECE_LENGTH, &[], &[]);
        assert!(matches!(result, Err(TorrentCreateError::EmptyFile { .. })));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Info hash is deterministic — same file always produces same hash.
    #[test]
    fn create_torrent_deterministic() {
        let tmp = std::env::temp_dir().join("p2p-torrent-deterministic");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let file_path = tmp.join("stable.bin");
        std::fs::write(&file_path, b"deterministic content").unwrap();

        let r1 = create_torrent(&file_path, DEFAULT_PIECE_LENGTH, &[], &[]).unwrap();
        let r2 = create_torrent(&file_path, DEFAULT_PIECE_LENGTH, &[], &[]).unwrap();
        assert_eq!(r1.info_hash, r2.info_hash);
        assert_eq!(r1.torrent_data, r2.torrent_data);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Error Display for EmptyFile includes the path.
    #[test]
    fn torrent_create_error_display_empty_file() {
        let err = TorrentCreateError::EmptyFile {
            path: "test.zip".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("test.zip"), "should contain path: {msg}");
    }

    // ── BEP 19 web seeds ────────────────────────────────────────────

    /// Single web seed is encoded as a string.
    #[test]
    fn create_torrent_single_web_seed() {
        let tmp = std::env::temp_dir().join("p2p-torrent-webseed-single");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let file_path = tmp.join("data.bin");
        std::fs::write(&file_path, b"web seed content test").unwrap();

        let web_seeds = &["https://archive.org/download/test/file.zip"];
        let result = create_torrent(&file_path, DEFAULT_PIECE_LENGTH, &[], web_seeds).unwrap();

        let torrent_str = String::from_utf8_lossy(&result.torrent_data);
        assert!(torrent_str.contains("url-list"));
        assert!(torrent_str.contains("archive.org"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Multiple web seeds are encoded as a list.
    #[test]
    fn create_torrent_multiple_web_seeds() {
        let tmp = std::env::temp_dir().join("p2p-torrent-webseed-multi");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let file_path = tmp.join("data.bin");
        std::fs::write(&file_path, b"multi web seed test").unwrap();

        let web_seeds = &[
            "https://mirror1.example.com/file.zip",
            "https://mirror2.example.com/file.zip",
        ];
        let result = create_torrent(&file_path, DEFAULT_PIECE_LENGTH, &[], web_seeds).unwrap();

        let torrent_str = String::from_utf8_lossy(&result.torrent_data);
        assert!(torrent_str.contains("url-list"));
        assert!(torrent_str.contains("mirror1.example.com"));
        assert!(torrent_str.contains("mirror2.example.com"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// No web seeds omits `url-list` entirely.
    #[test]
    fn create_torrent_no_web_seeds_omits_url_list() {
        let tmp = std::env::temp_dir().join("p2p-torrent-no-webseed");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let file_path = tmp.join("data.bin");
        std::fs::write(&file_path, b"no web seed test").unwrap();

        let result = create_torrent(&file_path, DEFAULT_PIECE_LENGTH, &[], &[]).unwrap();
        let torrent_str = String::from_utf8_lossy(&result.torrent_data);
        assert!(!torrent_str.contains("url-list"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // ── TorrentBuilder (streaming) ──────────────────────────────────

    /// TorrentBuilder produces the same info_hash as create_torrent for
    /// identical content — this is the core correctness invariant.
    ///
    /// Both APIs must produce bitwise-identical torrent metadata so that
    /// streaming and file-based creation are interchangeable.
    #[test]
    fn builder_matches_create_torrent() {
        let tmp = std::env::temp_dir().join("p2p-builder-match");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let data = vec![0xCDu8; 512 * 1024]; // 512 KiB
        let file_path = tmp.join("content.zip");
        std::fs::write(&file_path, &data).unwrap();

        let piece_length = DEFAULT_PIECE_LENGTH;
        let trackers = &["udp://tracker.opentrackr.org:1337/announce"];
        let web_seeds = &["https://mirror.example.com/content.zip"];

        // File-based
        let file_result = create_torrent(&file_path, piece_length, trackers, web_seeds).unwrap();

        // Streaming
        let mut builder = TorrentBuilder::new("content.zip", piece_length);
        builder.write(&data);
        let stream_result = builder.finalize(trackers, web_seeds).unwrap();

        assert_eq!(file_result.info_hash, stream_result.info_hash);
        assert_eq!(file_result.torrent_data, stream_result.torrent_data);
        assert_eq!(file_result.piece_count, stream_result.piece_count);
        assert_eq!(file_result.file_size, stream_result.file_size);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// TorrentBuilder handles small chunks that span piece boundaries.
    ///
    /// Real HTTP responses arrive in arbitrary-sized chunks (often 8 KiB
    /// or 16 KiB). The builder must correctly split chunks across piece
    /// boundaries and produce the same result regardless of chunk sizes.
    #[test]
    fn builder_small_chunks_match() {
        let tmp = std::env::temp_dir().join("p2p-builder-chunks");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let data = vec![0xABu8; 300_000]; // ~293 KiB, spans 2 pieces at 256 KiB
        let file_path = tmp.join("chunked.zip");
        std::fs::write(&file_path, &data).unwrap();

        let piece_length = DEFAULT_PIECE_LENGTH;
        let file_result = create_torrent(&file_path, piece_length, &[], &[]).unwrap();

        // Feed in 1 KiB chunks — simulates small HTTP read buffers.
        let mut builder = TorrentBuilder::new("chunked.zip", piece_length);
        for chunk in data.chunks(1024) {
            builder.write(chunk);
        }
        let stream_result = builder.finalize(&[], &[]).unwrap();

        assert_eq!(file_result.info_hash, stream_result.info_hash);
        assert_eq!(file_result.piece_count, stream_result.piece_count);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// TorrentBuilder rejects zero bytes (same as create_torrent rejects
    /// empty files).
    #[test]
    fn builder_rejects_empty() {
        let builder = TorrentBuilder::new("empty.zip", DEFAULT_PIECE_LENGTH);
        let result = builder.finalize(&[], &[]);
        assert!(matches!(result, Err(TorrentCreateError::EmptyFile { .. })));
    }

    /// TorrentBuilder tracks bytes_written correctly.
    #[test]
    fn builder_bytes_written() {
        let mut builder = TorrentBuilder::new("test.zip", DEFAULT_PIECE_LENGTH);
        assert_eq!(builder.bytes_written(), 0);

        builder.write(&[0u8; 1000]);
        assert_eq!(builder.bytes_written(), 1000);

        builder.write(&[0u8; 500]);
        assert_eq!(builder.bytes_written(), 1500);
    }

    /// TorrentBuilder is deterministic — same bytes produce same info_hash.
    #[test]
    fn builder_deterministic() {
        let data = b"deterministic builder test content bytes";

        let mut b1 = TorrentBuilder::new("det.zip", DEFAULT_PIECE_LENGTH);
        b1.write(data);
        let r1 = b1.finalize(&[], &[]).unwrap();

        let mut b2 = TorrentBuilder::new("det.zip", DEFAULT_PIECE_LENGTH);
        b2.write(data);
        let r2 = b2.finalize(&[], &[]).unwrap();

        assert_eq!(r1.info_hash, r2.info_hash);
        assert_eq!(r1.torrent_data, r2.torrent_data);
    }

    /// TorrentBuilder handles data that is exactly one piece long.
    #[test]
    fn builder_exact_piece_boundary() {
        let tmp = std::env::temp_dir().join("p2p-builder-exact");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let piece_length = 64 * 1024;
        let data = vec![0xFFu8; piece_length as usize]; // exactly 1 piece
        let file_path = tmp.join("exact.zip");
        std::fs::write(&file_path, &data).unwrap();

        let file_result = create_torrent(&file_path, piece_length, &[], &[]).unwrap();

        let mut builder = TorrentBuilder::new("exact.zip", piece_length);
        builder.write(&data);
        let stream_result = builder.finalize(&[], &[]).unwrap();

        assert_eq!(file_result.info_hash, stream_result.info_hash);
        assert_eq!(stream_result.piece_count, 1);

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
