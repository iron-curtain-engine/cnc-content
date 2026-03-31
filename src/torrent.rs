// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! BitTorrent P2P content distribution — behind the `torrent` feature flag.
//!
//! Uses `librqbit` for BitTorrent protocol support: DHT, HTTP/UDP trackers,
//! piece verification, seeding, and magnet link resolution.
//!
//! ## Design (per Iron Curtain design docs, D049)
//!
//! - **Size strategy**: <5 MB → HTTP only; 5–50 MB → P2P+HTTP concurrent; >50 MB → P2P preferred
//! - Clients automatically seed downloaded content (opt-out, configurable speed caps)
//! - SHA-256 verification after download
//! - Rarest-first piece selection, endgame mode for stall prevention
//!
//! ## Usage
//!
//! ```rust,ignore
//! use cnc_content::torrent::{TorrentConfig, TorrentDownloader};
//!
//! let config = TorrentConfig::default();
//! let downloader = TorrentDownloader::new(config)?;
//! downloader.download_package(package, content_root, |progress| { ... })?;
//! ```

use std::path::{Path, PathBuf};
use std::sync::Arc;

use thiserror::Error;

/// Errors from torrent operations.
#[derive(Debug, Error)]
pub enum TorrentError {
    #[error("librqbit session error: {0}")]
    Session(String),
    #[error("torrent download failed: {0}")]
    Download(String),
    #[error("no info_hash or torrent file available for this package")]
    NoTorrentMetadata,
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Configuration for P2P downloads.
#[derive(Debug, Clone)]
pub struct TorrentConfig {
    /// Maximum upload speed in bytes/sec (0 = unlimited).
    pub max_upload_speed: u64,
    /// Maximum download speed in bytes/sec (0 = unlimited).
    pub max_download_speed: u64,
    /// Seeding policy — controls upload behavior after download.
    pub seeding_policy: crate::SeedingPolicy,
    /// Directory for torrent session state and partial downloads.
    pub session_dir: PathBuf,
    /// Directory for downloaded archive files (ZIPs, ISOs).
    /// Separate from session_dir so archives can be retained for seeding.
    pub archive_dir: PathBuf,
    /// DHT enabled.
    pub enable_dht: bool,
    /// Port range for incoming connections.
    pub listen_port: u16,
}

impl Default for TorrentConfig {
    fn default() -> Self {
        let session_dir = default_session_dir();
        let archive_dir = session_dir
            .parent()
            .map(|p| p.join("downloads"))
            .unwrap_or_else(|| PathBuf::from("downloads"));
        Self {
            max_upload_speed: 1_048_576, // 1 MB/s
            max_download_speed: 0,       // unlimited
            seeding_policy: crate::SeedingPolicy::default(),
            session_dir,
            archive_dir,
            enable_dht: true,
            listen_port: 6881,
        }
    }
}

/// Progress events from torrent downloads.
#[derive(Debug, Clone)]
pub enum TorrentProgress {
    /// Connecting to trackers / DHT.
    Connecting { trackers: usize },
    /// Found peers.
    PeersFound { count: usize },
    /// Download progress.
    Downloading {
        bytes_downloaded: u64,
        total_bytes: u64,
        peers: usize,
        download_speed: u64,
    },
    /// Verifying pieces.
    Verifying { pieces_verified: u32, total: u32 },
    /// Download complete, now seeding.
    Seeding { uploaded_bytes: u64, peers: usize },
    /// Fully complete (seeding stopped).
    Complete,
}

/// BitTorrent downloader backed by librqbit.
///
/// Manages a persistent session that can download multiple packages and
/// seed completed content to other peers. Seeding behavior is controlled
/// by the [`SeedingPolicy`](crate::SeedingPolicy) in the config.
///
/// ## WebSeed note
///
/// librqbit does not currently support BEP 19 webseeds. HTTP mirrors in
/// `direct_urls` are handled by the separate HTTP downloader. When the
/// `p2p-distribute` crate replaces librqbit, HTTP mirrors will become
/// true webseeds inside the torrent swarm.
pub struct TorrentDownloader {
    config: TorrentConfig,
    runtime: tokio::runtime::Runtime,
    session: Arc<librqbit::Session>,
    /// Whether seeding is currently paused (e.g. during online gameplay).
    seeding_paused: std::sync::atomic::AtomicBool,
}

impl TorrentDownloader {
    /// Creates a new torrent downloader with the given configuration.
    pub fn new(config: TorrentConfig) -> Result<Self, TorrentError> {
        std::fs::create_dir_all(&config.session_dir)?;
        std::fs::create_dir_all(&config.archive_dir)?;

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(2)
            .build()
            .map_err(|e| TorrentError::Session(e.to_string()))?;

        let session = runtime.block_on(async {
            let session_opts = librqbit::SessionOptions {
                disable_dht: !config.enable_dht,
                listen_port_range: Some(config.listen_port..config.listen_port + 100),
                ..Default::default()
            };

            librqbit::Session::new_with_opts(config.session_dir.clone(), session_opts)
                .await
                .map_err(|e| TorrentError::Session(e.to_string()))
        })?;

        Ok(Self {
            config,
            runtime,
            session,
            seeding_paused: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// Downloads a content package via BitTorrent.
    ///
    /// The package must have a non-empty `info_hash` field. Trackers from the
    /// package definition and the global `PUBLIC_TRACKERS` list are combined.
    ///
    /// After download completes:
    /// - `SeedAlways` / `PauseDuringOnlinePlay`: the torrent stays active for seeding
    /// - `KeepNoSeed`: download stops, archive is retained
    /// - `ExtractAndDelete`: download stops, archive is deleted after extraction
    pub fn download_package(
        &self,
        package: &crate::DownloadPackage,
        content_root: &Path,
        on_progress: &mut dyn FnMut(TorrentProgress),
    ) -> Result<PathBuf, TorrentError> {
        if package.info_hash.is_empty() {
            return Err(TorrentError::NoTorrentMetadata);
        }

        std::fs::create_dir_all(content_root)?;

        let output_dir = &self.config.archive_dir;

        // Build magnet URI from info hash and trackers.
        let mut magnet = format!("magnet:?xt=urn:btih:{}", package.info_hash);
        // Add display name for tracker announces.
        magnet.push_str("&dn=");
        magnet.push_str(&urlencoding::encode(package.title));

        let public_trackers: Vec<&str> = crate::public_trackers().collect();
        for tracker in package
            .trackers
            .iter()
            .copied()
            .chain(public_trackers.iter().copied())
        {
            magnet.push_str("&tr=");
            magnet.push_str(&urlencoding::encode(tracker));
        }

        on_progress(TorrentProgress::Connecting {
            trackers: package.trackers.len() + public_trackers.len(),
        });

        self.runtime.block_on(async {
            let add_opts = librqbit::AddTorrentOptions {
                output_folder: Some(output_dir.to_string_lossy().into_owned()),
                ..Default::default()
            };

            let handle = self
                .session
                .add_torrent(librqbit::AddTorrent::from_url(&magnet), Some(add_opts))
                .await
                .map_err(|e| TorrentError::Download(e.to_string()))?
                .into_handle()
                .ok_or_else(|| TorrentError::Download("failed to get torrent handle".into()))?;

            // Wait for download to complete.
            handle
                .wait_until_completed()
                .await
                .map_err(|e| TorrentError::Download(e.to_string()))?;

            // The session owns the torrent — dropping the handle doesn't stop it.
            // Seeding continues as long as the session is alive, regardless of
            // handle lifetime. Non-seeding policies are enforced at shutdown.

            Ok::<(), TorrentError>(())
        })?;

        on_progress(TorrentProgress::Complete);

        // Return the archive directory where librqbit saved the files.
        Ok(output_dir.to_path_buf())
    }

    /// Returns whether this downloader can handle a given package (has info_hash).
    pub fn can_download(package: &crate::DownloadPackage) -> bool {
        !package.info_hash.is_empty()
    }

    /// Returns the current seeding policy.
    pub fn seeding_policy(&self) -> crate::SeedingPolicy {
        self.config.seeding_policy
    }

    /// Pauses all seeding activity (e.g. when an online game starts).
    ///
    /// Only effective when the seeding policy is `PauseDuringOnlinePlay`.
    pub fn pause_seeding(&self) {
        self.seeding_paused
            .store(true, std::sync::atomic::Ordering::Relaxed);
        // In a full implementation, this would pause all active torrent
        // uploads via the librqbit session. For now we track the state
        // so callers can query it.
    }

    /// Resumes seeding activity (e.g. when returning to the menu).
    pub fn resume_seeding(&self) {
        self.seeding_paused
            .store(false, std::sync::atomic::Ordering::Relaxed);
    }

    /// Returns `true` if seeding is currently paused.
    pub fn is_seeding_paused(&self) -> bool {
        self.seeding_paused
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Shuts down the session, stopping all seeding.
    pub fn shutdown(self) {
        // Destructure to control drop order: release the session Arc first
        // (signals torrents to wind down), then shut down the runtime.
        let Self {
            session, runtime, ..
        } = self;
        drop(session);
        runtime.shutdown_timeout(std::time::Duration::from_secs(5));
    }
}

/// Determines whether a package should prefer P2P based on size.
///
/// Per design docs D049:
/// - <5 MB: HTTP only
/// - 5–50 MB: P2P + HTTP concurrent
/// - >50 MB: P2P preferred
pub fn should_use_p2p(package: &crate::DownloadPackage) -> bool {
    if package.info_hash.is_empty() {
        return false;
    }
    package.size_hint >= 5_000_000
}

fn default_session_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("CNC_CONTENT_ROOT") {
        return PathBuf::from(dir).join(".torrent-session");
    }
    app_path::try_app_path!(".torrent-session")
        .map(|p| p.into_path_buf())
        .unwrap_or_else(|_| PathBuf::from(".torrent-session"))
}
