// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! High-level content session for downstream crates and game engines.
//!
//! `ContentSession` is the primary API for applications that embed `cnc-content`
//! as a library. It manages the full lifecycle: configuration, download,
//! extraction, seeding, and streaming.
//!
//! ## Downstream usage
//!
//! ```rust
//! use cnc_content::session::ContentSession;
//! use cnc_content::{GameId, SeedingPolicy};
//!
//! let tmp = std::env::temp_dir().join("cnc-session-doctest");
//! let mut session = ContentSession::open_with_root(
//!     GameId::RedAlert, tmp.clone(),
//! ).unwrap();
//!
//! assert_eq!(session.game(), GameId::RedAlert);
//! session.set_seeding_policy(SeedingPolicy::SeedAlways);
//! session.shutdown();
//! let _ = std::fs::remove_dir_all(&tmp);
//! ```

use std::path::{Path, PathBuf};

use strict_path::PathBoundary;

use crate::config::Config;
use crate::{GameId, PackageId, SeedingPolicy};

/// Errors from session operations.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("download error: {source}")]
    Download {
        #[from]
        source: crate::downloader::DownloadError,
    },
    #[error("I/O error: {source}")]
    Io {
        #[from]
        source: std::io::Error,
    },
    #[error("{game} is not freeware — cannot download")]
    NotFreeware { game: String },
    #[error("torrent session error: {message}")]
    Torrent { message: String },
    #[error("path traversal rejected: {detail}")]
    PathTraversal { detail: String },
}

/// High-level content session — the main entry point for downstream crates.
///
/// Wraps configuration, content root management, and the optional P2P torrent
/// session into a single API. Applications create one `ContentSession` per game
/// and use it for the entire application lifetime.
pub struct ContentSession {
    game: GameId,
    content_root: PathBuf,
    config: Config,
    /// The P2P torrent downloader, created lazily on first torrent download.
    #[cfg(feature = "torrent")]
    torrent: Option<crate::torrent::TorrentDownloader>,
}

impl ContentSession {
    /// Opens a content session for the given game using the default content root
    /// and persisted configuration.
    pub fn open(game: GameId) -> Result<Self, SessionError> {
        let content_root = crate::default_content_root_for_game(game);
        let config = Config::load();
        std::fs::create_dir_all(&content_root)?;
        Ok(Self {
            game,
            content_root,
            config,
            #[cfg(feature = "torrent")]
            torrent: None,
        })
    }

    /// Opens a session with a custom content root directory.
    pub fn open_with_root(game: GameId, content_root: PathBuf) -> Result<Self, SessionError> {
        let config = Config::load();
        std::fs::create_dir_all(&content_root)?;
        Ok(Self {
            game,
            content_root,
            config,
            #[cfg(feature = "torrent")]
            torrent: None,
        })
    }

    /// Opens a session with a custom content root and configuration.
    pub fn open_with_config(
        game: GameId,
        content_root: PathBuf,
        config: Config,
    ) -> Result<Self, SessionError> {
        std::fs::create_dir_all(&content_root)?;
        Ok(Self {
            game,
            content_root,
            config,
            #[cfg(feature = "torrent")]
            torrent: None,
        })
    }

    /// Returns the game this session manages.
    pub fn game(&self) -> GameId {
        self.game
    }

    /// Returns the content root directory.
    pub fn content_root(&self) -> &Path {
        &self.content_root
    }

    /// Returns the current seeding policy.
    pub fn seeding_policy(&self) -> SeedingPolicy {
        self.config.seeding_policy
    }

    /// Updates the seeding policy and persists it to disk.
    pub fn set_seeding_policy(&mut self, policy: SeedingPolicy) {
        self.config.seeding_policy = policy;
        let _ = self.config.save();
    }

    /// Returns `true` if all required packages for this game are installed.
    pub fn is_content_complete(&self) -> bool {
        crate::is_content_complete(&self.content_root, self.game)
    }

    /// Returns the list of missing required packages.
    pub fn missing_required_packages(&self) -> Vec<&'static crate::ContentPackage> {
        crate::missing_required_packages(&self.content_root, self.game)
    }

    /// Returns all packages (required + optional) that are not installed.
    pub fn missing_packages(&self) -> Vec<&'static crate::ContentPackage> {
        crate::missing_packages(&self.content_root, self.game)
    }

    /// Downloads and installs specific packages.
    ///
    /// Only downloads packages that are not already installed. Packages are
    /// matched to their download definitions, and the best strategy (P2P or
    /// HTTP) is selected automatically.
    ///
    /// This is the primary method downstream crates should use — request
    /// exactly the packages you need and let the session handle the rest.
    pub fn ensure_packages(
        &mut self,
        packages: &[PackageId],
        mut on_progress: impl FnMut(crate::downloader::DownloadProgress),
    ) -> Result<(), SessionError> {
        if !self.game.is_freeware() {
            return Err(SessionError::NotFreeware {
                game: self.game.title().to_string(),
            });
        }

        for &pkg_id in packages {
            let pkg = crate::package(pkg_id).ok_or_else(|| SessionError::Io {
                source: std::io::Error::other(format!("no package definition for {pkg_id:?}")),
            })?;

            // Skip if already installed.
            if pkg
                .test_files
                .iter()
                .all(|f| self.content_root.join(f).exists())
            {
                continue;
            }

            // Find a download that provides this package.
            let download = match pkg.download {
                Some(dl_id) => crate::download(dl_id).ok_or_else(|| SessionError::Io {
                    source: std::io::Error::other(format!("no download definition for {dl_id:?}")),
                })?,
                None => continue, // no download available
            };

            crate::downloader::download_and_install(
                download,
                &self.content_root,
                self.config.seeding_policy,
                &mut on_progress,
            )?;
        }

        Ok(())
    }

    /// Downloads and installs all required content that is currently missing.
    pub fn ensure_required(
        &mut self,
        on_progress: impl FnMut(crate::downloader::DownloadProgress),
    ) -> Result<(), SessionError> {
        let missing: Vec<PackageId> = self
            .missing_required_packages()
            .iter()
            .map(|p| p.id)
            .collect();
        self.ensure_packages(&missing, on_progress)
    }

    /// Downloads and installs ALL content (required + optional) that is missing.
    pub fn ensure_all(
        &mut self,
        on_progress: impl FnMut(crate::downloader::DownloadProgress),
    ) -> Result<(), SessionError> {
        let missing: Vec<PackageId> = self.missing_packages().iter().map(|p| p.id).collect();
        self.ensure_packages(&missing, on_progress)
    }

    /// Returns the full path to a content file relative to the content root.
    ///
    /// Returns `Some(path)` if the file exists on disk. This is the entry point
    /// for content access — the game engine asks for a file by relative path
    /// and gets back an absolute path it can open.
    ///
    /// ## Future: P2P streaming
    ///
    /// When `p2p-distribute` ships with sequential piece priority, this method
    /// will also trigger on-demand downloading: if the file is part of an active
    /// torrent but not yet fully downloaded, it will prioritize the needed pieces
    /// and return the path once they're available. This enables streaming video
    /// playback (FMV cutscenes) directly from the P2P swarm without waiting for
    /// the full download to complete.
    pub fn content_file_path(&self, relative_path: &str) -> Result<Option<PathBuf>, SessionError> {
        let boundary = PathBoundary::<()>::try_new_create(&self.content_root).map_err(|e| {
            SessionError::PathTraversal {
                detail: e.to_string(),
            }
        })?;
        let strict =
            boundary
                .strict_join(relative_path)
                .map_err(|e| SessionError::PathTraversal {
                    detail: e.to_string(),
                })?;
        let full = strict.unstrict();
        if full.exists() {
            Ok(Some(full))
        } else {
            Ok(None)
        }
    }

    /// Opens a content file for sequential reading (streaming).
    ///
    /// Returns a [`ContentReader`] that implements `std::io::Read` + `Seek`.
    /// The game engine can use this to stream video (FMV cutscenes), audio,
    /// or any other content file without loading the entire file into memory.
    ///
    /// Currently reads from disk. When `p2p-distribute` ships, this will
    /// transparently stream from the P2P swarm: the reader blocks until the
    /// next piece arrives, and the torrent client prioritizes pieces
    /// sequentially from the read head position.
    ///
    /// ## Example
    ///
    /// ```rust
    /// use cnc_content::session::ContentSession;
    /// use cnc_content::GameId;
    /// use std::io::Read;
    ///
    /// let tmp = std::env::temp_dir().join("cnc-open-content-doctest");
    /// let _ = std::fs::remove_dir_all(&tmp);
    /// std::fs::create_dir_all(&tmp).unwrap();
    /// std::fs::write(tmp.join("test.txt"), b"hello").unwrap();
    ///
    /// let session = ContentSession::open_with_root(
    ///     GameId::RedAlert, tmp.clone(),
    /// ).unwrap();
    /// let mut reader = session.open_content("test.txt").unwrap();
    /// let mut buf = Vec::new();
    /// reader.read_to_end(&mut buf).unwrap();
    /// assert_eq!(buf, b"hello");
    /// let _ = std::fs::remove_dir_all(&tmp);
    /// ```
    pub fn open_content(&self, relative_path: &str) -> Result<ContentReader, SessionError> {
        let boundary = PathBoundary::<()>::try_new_create(&self.content_root).map_err(|e| {
            SessionError::PathTraversal {
                detail: e.to_string(),
            }
        })?;
        let strict =
            boundary
                .strict_join(relative_path)
                .map_err(|e| SessionError::PathTraversal {
                    detail: e.to_string(),
                })?;
        let full = strict.unstrict();
        let file = std::fs::File::open(&full)?;
        let size = file.metadata()?.len();
        Ok(ContentReader {
            inner: file,
            path: full,
            size,
        })
    }

    /// Opens a content file for streaming playback (VQA cutscenes, etc.).
    ///
    /// Returns a [`StreamingReader`](crate::streaming::StreamingReader) that
    /// implements `Read + Seek`. When the file is fully downloaded, reads go
    /// straight to disk with zero overhead. When the file is partially
    /// downloaded (P2P still in progress), the reader blocks until the
    /// requested bytes arrive — enabling playback to start before the full
    /// download completes.
    ///
    /// ## Example
    ///
    /// ```rust
    /// use cnc_content::session::ContentSession;
    /// use cnc_content::GameId;
    /// use std::io::Read;
    ///
    /// let tmp = std::env::temp_dir().join("cnc-open-stream-doctest");
    /// let _ = std::fs::remove_dir_all(&tmp);
    /// std::fs::create_dir_all(&tmp).unwrap();
    /// std::fs::write(tmp.join("intro.vqa"), b"VQA data").unwrap();
    ///
    /// let session = ContentSession::open_with_root(
    ///     GameId::RedAlert, tmp.clone(),
    /// ).unwrap();
    /// let mut reader = session.open_stream("intro.vqa").unwrap();
    /// let mut buf = Vec::new();
    /// reader.read_to_end(&mut buf).unwrap();
    /// assert_eq!(buf, b"VQA data");
    /// let _ = std::fs::remove_dir_all(&tmp);
    /// ```
    pub fn open_stream(
        &self,
        relative_path: &str,
    ) -> Result<crate::streaming::StreamingReader, SessionError> {
        let boundary = PathBoundary::<()>::try_new_create(&self.content_root).map_err(|e| {
            SessionError::PathTraversal {
                detail: e.to_string(),
            }
        })?;
        let strict =
            boundary
                .strict_join(relative_path)
                .map_err(|e| SessionError::PathTraversal {
                    detail: e.to_string(),
                })?;
        let full = strict.unstrict();
        Ok(crate::streaming::StreamingReader::from_complete_file(
            &full,
        )?)
    }

    /// Opens a content file for streaming with a custom buffer policy.
    ///
    /// Use this when you need different buffering thresholds — e.g. a
    /// high-bitrate upscaled cutscene needs more pre-buffer than a
    /// standard 320×200 VQA.
    pub fn open_stream_with_policy(
        &self,
        relative_path: &str,
        policy: crate::streaming::BufferPolicy,
    ) -> Result<crate::streaming::StreamingReader, SessionError> {
        let boundary = PathBoundary::<()>::try_new_create(&self.content_root).map_err(|e| {
            SessionError::PathTraversal {
                detail: e.to_string(),
            }
        })?;
        let strict =
            boundary
                .strict_join(relative_path)
                .map_err(|e| SessionError::PathTraversal {
                    detail: e.to_string(),
                })?;
        let full = strict.unstrict();
        let file_size = std::fs::metadata(&full)?.len();
        let range_map = crate::streaming::ByteRangeMap::fully_available(file_size);
        let (reader, _notifier) =
            crate::streaming::StreamingReader::new_streaming(&full, range_map, policy)?;
        Ok(reader)
    }

    /// Returns an iterator over all content files that exist on disk,
    /// yielding `(relative_path, absolute_path)` pairs.
    pub fn installed_files(&self) -> Vec<(String, PathBuf)> {
        let mut files = Vec::new();
        for pkg in crate::packages_for_game(self.game) {
            for &test_file in pkg.test_files {
                let full = self.content_root.join(test_file);
                if full.exists() {
                    files.push((test_file.to_string(), full));
                }
            }
        }
        files
    }

    /// Pauses all seeding activity.
    ///
    /// Call this when the user enters online multiplayer to free up bandwidth.
    /// Only has an effect when the seeding policy is `PauseDuringOnlinePlay`.
    ///
    /// This is safe to call even when no torrent session is active.
    pub fn pause_seeding(&self) {
        #[cfg(feature = "torrent")]
        if let Some(ref torrent) = self.torrent {
            if self.config.seeding_policy == SeedingPolicy::PauseDuringOnlinePlay {
                torrent.pause_seeding();
            }
        }
    }

    /// Resumes seeding activity.
    ///
    /// Call this when the user leaves online multiplayer (back to menu, etc.).
    pub fn resume_seeding(&self) {
        #[cfg(feature = "torrent")]
        if let Some(ref torrent) = self.torrent {
            torrent.resume_seeding();
        }
    }

    /// Returns `true` if seeding is currently paused.
    pub fn is_seeding_paused(&self) -> bool {
        #[cfg(feature = "torrent")]
        if let Some(ref torrent) = self.torrent {
            return torrent.is_seeding_paused();
        }
        false
    }

    /// Verifies installed content integrity against the manifest.
    ///
    /// Returns a list of files that are missing or corrupted.
    pub fn verify(&self) -> Vec<String> {
        let manifest_path = self.content_root.join("content-manifest.toml");
        let manifest_str = match std::fs::read_to_string(&manifest_path) {
            Ok(s) => s,
            Err(_) => return Vec::new(), // no manifest = nothing to verify
        };
        let manifest: crate::verify::InstalledContentManifest = match toml::from_str(&manifest_str)
        {
            Ok(m) => m,
            Err(_) => return Vec::new(),
        };
        crate::verify::verify_installed_content(&self.content_root, &manifest)
    }

    /// Gracefully shuts down the session, stopping all seeding and saving config.
    pub fn shutdown(self) {
        let _ = self.config.save();
        #[cfg(feature = "torrent")]
        if let Some(torrent) = self.torrent {
            torrent.shutdown();
        }
    }
}

/// A streaming reader for content files.
///
/// Implements `Read` and `Seek` so the game engine can stream video, audio,
/// or data files without loading them entirely into memory.
///
/// Currently backed by a plain file handle. When `p2p-distribute` ships,
/// the backing store becomes a P2P piece-aware reader that:
/// - Requests sequential pieces starting from the current read position
/// - Blocks `read()` until the next piece arrives
/// - Allows the game to start playback before the full file is downloaded
///
/// This transparent upgrade is the "stream videos directly from P2P" feature:
/// the engine code stays the same, only the backing reader changes.
pub struct ContentReader {
    inner: std::fs::File,
    path: PathBuf,
    size: u64,
}

impl ContentReader {
    /// Returns the full path to the backing file on disk.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the total file size in bytes.
    pub fn size(&self) -> u64 {
        self.size
    }
}

impl std::io::Read for ContentReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(buf)
    }
}

impl std::io::Seek for ContentReader {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}

#[cfg(test)]
mod tests;
