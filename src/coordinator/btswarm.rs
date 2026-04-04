// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! BitTorrent swarm peer — wraps librqbit's entire BT swarm as one logical peer
//! in the coordinator's unified piece picker.
//!
//! ## How it works
//!
//! librqbit runs its own download loop internally, managing DHT, tracker
//! announces, peer connections, and piece requests. The `BtSwarmPeer` wraps
//! this entire swarm as a single "mega-peer":
//!
//! 1. When created, a magnet URI is added to the librqbit session. librqbit
//!    starts downloading the file to a temporary directory in the background.
//! 2. `has_piece(i)` — checks whether librqbit has written and verified
//!    piece `i` by reading from librqbit's output file and SHA-1 hashing it.
//!    Verified pieces are cached to avoid redundant hashing.
//! 3. `fetch_piece(i)` — reads piece data from librqbit's output file.
//!    The coordinator will SHA-1 verify it again (defense in depth).
//! 4. Speed estimate — derived from observed piece download rates.
//!
//! The coordinator and librqbit write to DIFFERENT files. The coordinator
//! writes verified pieces to the final output. librqbit writes to its own
//! temp file. After the coordinator completes, librqbit's temp file is
//! discarded. This eliminates all write conflicts.
//!
//! ## Why not inject HTTP peers into librqbit?
//!
//! librqbit's `PeerConnectionHandler` is hardwired to the BT wire protocol
//! (TCP SocketAddr, BT handshake, BT messages). Injecting HTTP as a peer
//! type would require forking librqbit. Instead, the coordinator treats
//! librqbit's entire swarm as one peer and HTTP mirrors as other peers —
//! both are equal in the piece picker.

use std::io::{Read, Seek};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use super::{Peer, PeerError, PeerKind, TorrentInfo};

/// A BitTorrent swarm peer backed by a running librqbit session.
///
/// Monitors librqbit's output file for completed pieces and serves them
/// to the coordinator on demand. librqbit runs autonomously in the
/// background — this peer simply observes what it has produced.
pub struct BtSwarmPeer {
    /// Path to the file that librqbit is downloading to.
    librqbit_output: PathBuf,
    /// Torrent metadata — piece length, piece hashes, file size.
    info: Arc<TorrentInfo>,
    /// Cache of which pieces have been verified from librqbit's output.
    /// Once a piece is verified, we don't re-hash it on subsequent calls.
    verified: Vec<AtomicBool>,
    /// Rolling download speed estimate in bytes/sec.
    speed_bytes_per_sec: AtomicU64,
    /// librqbit tokio runtime — kept alive to ensure the torrent session
    /// continues running in the background.
    _runtime: Arc<tokio::runtime::Runtime>,
    /// librqbit session — kept alive so the torrent keeps downloading.
    _session: Arc<librqbit::Session>,
}

impl BtSwarmPeer {
    /// Creates a new BT swarm peer.
    ///
    /// The caller must ensure:
    /// - `librqbit_output` is the file path where librqbit will write the downloaded file
    /// - The librqbit session has already been started and the torrent added
    /// - `info` matches the torrent being downloaded (piece hashes, piece length)
    pub fn new(
        librqbit_output: PathBuf,
        info: Arc<TorrentInfo>,
        runtime: Arc<tokio::runtime::Runtime>,
        session: Arc<librqbit::Session>,
    ) -> Self {
        let piece_count = info.piece_count() as usize;
        let mut verified = Vec::with_capacity(piece_count);
        for _ in 0..piece_count {
            verified.push(AtomicBool::new(false));
        }
        Self {
            librqbit_output,
            info,
            verified,
            speed_bytes_per_sec: AtomicU64::new(0),
            _runtime: runtime,
            _session: session,
        }
    }

    /// Checks whether a piece has been fully written by librqbit by reading
    /// it from disk and SHA-1 verifying against the expected hash.
    ///
    /// Caches the result: once verified, subsequent calls return `true`
    /// without re-hashing.
    fn check_piece_available(&self, piece_index: u32) -> bool {
        // Check cache first.
        if let Some(v) = self.verified.get(piece_index as usize) {
            if v.load(Ordering::Acquire) {
                return true;
            }
        }

        // Try to read and verify the piece from librqbit's output file.
        let offset = self.info.piece_offset(piece_index);
        let length = self.info.piece_size(piece_index) as u64;

        let data = match self.read_piece_from_file(offset, length) {
            Ok(d) => d,
            Err(_) => return false,
        };

        // Verify SHA-1 against expected hash.
        let expected = match self.info.piece_hash(piece_index) {
            Some(h) => h,
            None => return false,
        };

        use sha1::{Digest, Sha1};
        let mut hasher = Sha1::new();
        hasher.update(&data);
        let actual = hasher.finalize();

        if actual.as_slice() == expected {
            // Cache the verification result.
            if let Some(v) = self.verified.get(piece_index as usize) {
                v.store(true, Ordering::Release);
            }
            true
        } else {
            false
        }
    }

    /// Reads `length` bytes from `offset` in librqbit's output file.
    fn read_piece_from_file(&self, offset: u64, length: u64) -> Result<Vec<u8>, PeerError> {
        let mut file =
            std::fs::File::open(&self.librqbit_output).map_err(|e| PeerError::Io { source: e })?;

        file.seek(std::io::SeekFrom::Start(offset))
            .map_err(|e| PeerError::Io { source: e })?;

        let mut buf = Vec::with_capacity(length as usize);
        file.take(length)
            .read_to_end(&mut buf)
            .map_err(|e| PeerError::Io { source: e })?;

        Ok(buf)
    }
}

impl Peer for BtSwarmPeer {
    fn kind(&self) -> PeerKind {
        PeerKind::BtSwarm
    }

    /// Checks whether librqbit has completed this piece by reading from its
    /// output file and SHA-1 verifying. Results are cached.
    fn has_piece(&self, piece_index: u32) -> bool {
        self.check_piece_available(piece_index)
    }

    /// BT swarm choke state is reflected by piece availability — if a piece
    /// isn't available yet (librqbit hasn't downloaded it), `has_piece`
    /// returns false. We never report as choked to avoid the coordinator
    /// skipping us entirely.
    fn is_choked(&self) -> bool {
        false
    }

    /// Reads piece data from librqbit's output file.
    ///
    /// Should only be called after `has_piece()` returns true, but the
    /// coordinator will SHA-1 verify regardless.
    fn fetch_piece(
        &self,
        _piece_index: u32,
        offset: u64,
        length: u32,
    ) -> Result<Vec<u8>, PeerError> {
        self.read_piece_from_file(offset, length as u64)
    }

    fn speed_estimate(&self) -> u64 {
        self.speed_bytes_per_sec.load(Ordering::Relaxed)
    }
}
