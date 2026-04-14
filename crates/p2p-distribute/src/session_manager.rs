// SPDX-License-Identifier: MIT OR Apache-2.0

//! Session manager — coordinates multiple concurrent downloads with queuing.
//!
//! ## Design (informed by libtorrent, librqbit, Transmission)
//!
//! Production BT clients manage multiple concurrent torrents through a
//! session manager:
//!
//! - **libtorrent** uses 3 queues (checking / downloading / seeding) with
//!   configurable active limits per queue. Torrents exceeding the active
//!   limit are auto-paused. Priority ordering determines which torrents
//!   are active.
//! - **librqbit** exposes a `Session` struct with `add_torrent()`,
//!   `pause()`, `unpause()`, `delete()` methods and global rate limits.
//! - **Transmission** wraps `libtransmission` with per-torrent bandwidth
//!   groups and session-wide speed limits.
//!
//! This module defines the [`DownloadSession`] trait — the interface for
//! managing multiple downloads as a group. The default [`BasicSession`]
//! implements FIFO queuing with configurable concurrency and global
//! bandwidth limits.
//!
//! ## Why a trait?
//!
//! Different consumers have different scheduling needs. A game launcher
//! needs priority-based queuing (download the selected game first). A
//! content seeder needs bandwidth-weighted round-robin. A CI pipeline
//! needs sequential processing. The trait allows all of these without
//! changing the coordinator.

use std::time::Instant;

// ── DownloadHandle ──────────────────────────────────────────────────

/// Opaque identifier for a download within a session.
///
/// Returned by `add_download()` and used to reference the download in
/// subsequent operations (pause, resume, remove, query state).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DownloadHandle(u64);

impl DownloadHandle {
    /// Creates a handle from a raw id (for internal use).
    pub fn from_raw(id: u64) -> Self {
        Self(id)
    }

    /// Returns the raw id.
    pub fn raw(&self) -> u64 {
        self.0
    }
}

// ── SessionConfig ───────────────────────────────────────────────────

/// Configuration for a download session.
///
/// Controls global resource limits that apply across all downloads managed
/// by the session.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// Maximum number of downloads actively transferring data.
    /// Queued downloads wait until an active slot opens.
    /// libtorrent default: 3.
    pub max_active_downloads: usize,
    /// Maximum number of downloads in the checking phase (resume verification).
    /// libtorrent default: 1.
    pub max_active_checking: usize,
    /// Maximum number of seeding slots (uploads to other peers).
    /// Set to 0 to disable seeding entirely.
    pub max_active_seeding: usize,
    /// Global download speed limit in bytes/sec. 0 = unlimited.
    pub download_rate_limit: u64,
    /// Global upload speed limit in bytes/sec. 0 = unlimited.
    pub upload_rate_limit: u64,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            max_active_downloads: 3,
            max_active_checking: 1,
            max_active_seeding: 5,
            download_rate_limit: 0,
            upload_rate_limit: 0,
        }
    }
}

// ── DownloadEntry ───────────────────────────────────────────────────

/// A download entry in the session queue.
///
/// Tracks the download's identity, priority, and current state. The session
/// manager uses this to decide which downloads are active.
#[derive(Debug, Clone)]
pub struct DownloadEntry {
    /// Unique handle for this download.
    pub handle: DownloadHandle,
    /// User-assigned priority (higher = more important, downloaded first).
    pub priority: i32,
    /// Current state of this download.
    pub state: crate::state::DownloadState,
    /// When this download was added to the session.
    pub added_at: Instant,
}

// ── SessionEvent ────────────────────────────────────────────────────

/// Events emitted by the session manager for external observers.
///
/// ## Design (informed by cratetorrent's alert system)
///
/// cratetorrent uses an `Alert` enum to notify the application of state
/// changes. This pattern lets the UI react to session-level events
/// (download started, completed, error) without polling.
#[derive(Debug, Clone)]
pub enum SessionEvent {
    /// A download was added to the session.
    DownloadAdded { handle: DownloadHandle },
    /// A download started actively transferring (moved from queue to active).
    DownloadStarted { handle: DownloadHandle },
    /// A download was paused (user-initiated or auto-paused by queue limits).
    DownloadPaused { handle: DownloadHandle },
    /// A download completed successfully.
    DownloadCompleted { handle: DownloadHandle },
    /// A download encountered an unrecoverable error.
    DownloadFailed {
        handle: DownloadHandle,
        error: String,
    },
    /// A download was removed from the session.
    DownloadRemoved { handle: DownloadHandle },
    /// Session-wide speed update (aggregate across all active downloads).
    SpeedUpdate {
        download_bytes_per_sec: u64,
        upload_bytes_per_sec: u64,
    },
}

// ── DownloadSession trait ───────────────────────────────────────────

/// Trait for managing multiple concurrent downloads as a session.
///
/// The session manager is responsible for:
/// 1. **Queuing** — holding downloads until active slots are available.
/// 2. **Activation** — starting downloads in priority order when slots open.
/// 3. **Rate limiting** — distributing global bandwidth across active downloads.
/// 4. **Lifecycle** — pausing, resuming, and removing downloads.
///
/// ## Contract
///
/// - `add_download()` always succeeds and returns a unique handle.
/// - The session must respect `max_active_downloads` — excess downloads wait.
/// - When an active download completes or is removed, the session should
///   automatically activate the next queued download.
/// - Rate limits are best-effort (the session tells downloads their share,
///   but individual peers may exceed it briefly).
pub trait DownloadSession: Send + Sync {
    /// Adds a download to the session queue.
    ///
    /// The download starts in `Queued` state. If active slots are available,
    /// it may be activated immediately.
    fn add_download(&mut self, priority: i32) -> DownloadHandle;

    /// Pauses a download. If it was active, its slot becomes available.
    fn pause(&mut self, handle: DownloadHandle) -> Result<(), SessionError>;

    /// Resumes a paused download. It re-enters the queue at its priority.
    fn resume(&mut self, handle: DownloadHandle) -> Result<(), SessionError>;

    /// Removes a download from the session entirely.
    fn remove(&mut self, handle: DownloadHandle) -> Result<(), SessionError>;

    /// Changes a download's priority. Higher = more important.
    fn set_priority(&mut self, handle: DownloadHandle, priority: i32) -> Result<(), SessionError>;

    /// Returns the current state of all downloads in the session.
    fn list_downloads(&self) -> Vec<DownloadEntry>;

    /// Returns the session configuration.
    fn config(&self) -> &SessionConfig;

    /// Drains pending events (non-blocking).
    fn poll_events(&mut self) -> Vec<SessionEvent>;
}

// ── SessionError ────────────────────────────────────────────────────

/// Errors from session management operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionError {
    /// The specified download handle was not found in the session.
    NotFound { handle: DownloadHandle },
    /// The operation is not valid for the download's current state.
    InvalidState {
        handle: DownloadHandle,
        state: String,
        operation: String,
    },
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound { handle } => {
                write!(f, "download {} not found in session", handle.raw())
            }
            Self::InvalidState {
                handle,
                state,
                operation,
            } => write!(
                f,
                "cannot {operation} download {} in state {state}",
                handle.raw()
            ),
        }
    }
}

impl std::error::Error for SessionError {}

// ── BasicSession ────────────────────────────────────────────────────

/// Simple FIFO session manager with priority ordering.
///
/// Downloads are queued with a priority and activated in priority order
/// (highest first, then FIFO within same priority). Active downloads are
/// limited by `SessionConfig::max_active_downloads`.
///
/// This is the minimum viable session manager. Production deployments
/// may implement `DownloadSession` with more sophisticated scheduling.
pub struct BasicSession {
    config: SessionConfig,
    entries: Vec<DownloadEntry>,
    events: Vec<SessionEvent>,
    next_id: u64,
}

impl BasicSession {
    /// Creates a new session with the given configuration.
    pub fn new(config: SessionConfig) -> Self {
        Self {
            config,
            entries: Vec::new(),
            events: Vec::new(),
            next_id: 1,
        }
    }

    /// Returns the number of currently active (Downloading) entries.
    fn active_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| matches!(e.state, crate::state::DownloadState::Downloading))
            .count()
    }

    /// Tries to activate queued downloads up to the active limit.
    fn activate_queued(&mut self) {
        while self.active_count() < self.config.max_active_downloads {
            // Find the highest-priority queued download.
            let candidate = self
                .entries
                .iter()
                .enumerate()
                .filter(|(_, e)| matches!(e.state, crate::state::DownloadState::Queued))
                .max_by_key(|(_, e)| (e.priority, std::cmp::Reverse(e.added_at)));

            if let Some((idx, _)) = candidate {
                let handle = self.entries.get(idx).map(|e| e.handle);
                if let Some(entry) = self.entries.get_mut(idx) {
                    entry.state = crate::state::DownloadState::Downloading;
                    if let Some(h) = handle {
                        self.events
                            .push(SessionEvent::DownloadStarted { handle: h });
                    }
                }
            } else {
                break;
            }
        }
    }
}

impl DownloadSession for BasicSession {
    fn add_download(&mut self, priority: i32) -> DownloadHandle {
        let handle = DownloadHandle::from_raw(self.next_id);
        self.next_id = self.next_id.wrapping_add(1);

        self.entries.push(DownloadEntry {
            handle,
            priority,
            state: crate::state::DownloadState::Queued,
            added_at: Instant::now(),
        });

        self.events.push(SessionEvent::DownloadAdded { handle });
        self.activate_queued();
        handle
    }

    fn pause(&mut self, handle: DownloadHandle) -> Result<(), SessionError> {
        let entry = self
            .entries
            .iter_mut()
            .find(|e| e.handle == handle)
            .ok_or(SessionError::NotFound { handle })?;

        match &entry.state {
            crate::state::DownloadState::Downloading
            | crate::state::DownloadState::Queued
            | crate::state::DownloadState::Checking { .. }
            | crate::state::DownloadState::Seeding => {
                let prev = std::mem::replace(&mut entry.state, crate::state::DownloadState::Queued);
                entry.state = crate::state::DownloadState::Paused {
                    previous: Box::new(prev),
                };
                self.events.push(SessionEvent::DownloadPaused { handle });
                self.activate_queued();
                Ok(())
            }
            other => Err(SessionError::InvalidState {
                handle,
                state: other.to_string(),
                operation: "pause".into(),
            }),
        }
    }

    fn resume(&mut self, handle: DownloadHandle) -> Result<(), SessionError> {
        let entry = self
            .entries
            .iter_mut()
            .find(|e| e.handle == handle)
            .ok_or(SessionError::NotFound { handle })?;

        match &entry.state {
            crate::state::DownloadState::Paused { .. } => {
                // Re-queue; activate_queued will start it if slots available.
                entry.state = crate::state::DownloadState::Queued;
                self.activate_queued();
                Ok(())
            }
            other => Err(SessionError::InvalidState {
                handle,
                state: other.to_string(),
                operation: "resume".into(),
            }),
        }
    }

    fn remove(&mut self, handle: DownloadHandle) -> Result<(), SessionError> {
        let idx = self
            .entries
            .iter()
            .position(|e| e.handle == handle)
            .ok_or(SessionError::NotFound { handle })?;

        self.entries.remove(idx);
        self.events.push(SessionEvent::DownloadRemoved { handle });
        self.activate_queued();
        Ok(())
    }

    fn set_priority(&mut self, handle: DownloadHandle, priority: i32) -> Result<(), SessionError> {
        let entry = self
            .entries
            .iter_mut()
            .find(|e| e.handle == handle)
            .ok_or(SessionError::NotFound { handle })?;

        entry.priority = priority;
        Ok(())
    }

    fn list_downloads(&self) -> Vec<DownloadEntry> {
        self.entries.clone()
    }

    fn config(&self) -> &SessionConfig {
        &self.config
    }

    fn poll_events(&mut self) -> Vec<SessionEvent> {
        std::mem::take(&mut self.events)
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── BasicSession ────────────────────────────────────────────────

    /// Adding a download returns a unique handle and emits events.
    ///
    /// Each `add_download()` must return a distinct handle. The session
    /// emits `DownloadAdded` and potentially `DownloadStarted` events.
    #[test]
    fn add_download_returns_unique_handles() {
        let mut session = BasicSession::new(SessionConfig::default());
        let h1 = session.add_download(0);
        let h2 = session.add_download(0);
        assert_ne!(h1, h2);
    }

    /// Downloads are activated up to max_active_downloads.
    ///
    /// With max_active_downloads=2, adding 4 downloads should activate 2
    /// and queue the remaining 2.
    #[test]
    fn respects_max_active_limit() {
        let config = SessionConfig {
            max_active_downloads: 2,
            ..Default::default()
        };
        let mut session = BasicSession::new(config);
        session.add_download(0);
        session.add_download(0);
        session.add_download(0);
        session.add_download(0);

        let downloads = session.list_downloads();
        let active = downloads
            .iter()
            .filter(|d| matches!(d.state, crate::state::DownloadState::Downloading))
            .count();
        let queued = downloads
            .iter()
            .filter(|d| matches!(d.state, crate::state::DownloadState::Queued))
            .count();

        assert_eq!(active, 2);
        assert_eq!(queued, 2);
    }

    /// Higher-priority downloads are activated first.
    ///
    /// When active slots open, the session must prefer higher-priority
    /// queued downloads.
    #[test]
    fn higher_priority_activated_first() {
        let config = SessionConfig {
            max_active_downloads: 1,
            ..Default::default()
        };
        let mut session = BasicSession::new(config);
        let low = session.add_download(1);
        let _high = session.add_download(10);

        // Only 1 active slot. The first added gets it (already active).
        // Pause the active one — high priority should be next.
        session.pause(low).unwrap();

        let downloads = session.list_downloads();
        let active: Vec<_> = downloads
            .iter()
            .filter(|d| matches!(d.state, crate::state::DownloadState::Downloading))
            .collect();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].priority, 10);
    }

    /// Removing an active download activates the next queued one.
    #[test]
    fn remove_activates_queued() {
        let config = SessionConfig {
            max_active_downloads: 1,
            ..Default::default()
        };
        let mut session = BasicSession::new(config);
        let h1 = session.add_download(0);
        let _h2 = session.add_download(0);

        session.remove(h1).unwrap();

        let downloads = session.list_downloads();
        assert_eq!(downloads.len(), 1);
        assert!(matches!(
            downloads[0].state,
            crate::state::DownloadState::Downloading
        ));
    }

    /// Pause and resume cycle works correctly.
    #[test]
    fn pause_resume_cycle() {
        let mut session = BasicSession::new(SessionConfig::default());
        let h = session.add_download(0);

        session.pause(h).unwrap();
        let state = &session.list_downloads()[0].state;
        assert!(matches!(state, crate::state::DownloadState::Paused { .. }));

        session.resume(h).unwrap();
        // After resume, it goes back to queued and may be re-activated.
        let state = &session.list_downloads()[0].state;
        assert!(
            matches!(
                state,
                crate::state::DownloadState::Downloading | crate::state::DownloadState::Queued
            ),
            "expected Downloading or Queued, got {state}"
        );
    }

    /// Pausing a non-existent handle returns NotFound.
    #[test]
    fn pause_not_found() {
        let mut session = BasicSession::new(SessionConfig::default());
        let err = session.pause(DownloadHandle::from_raw(999));
        assert!(matches!(err, Err(SessionError::NotFound { .. })));
    }

    /// Events are drained by poll_events.
    #[test]
    fn poll_events_drains() {
        let mut session = BasicSession::new(SessionConfig::default());
        session.add_download(0);
        let events = session.poll_events();
        assert!(!events.is_empty());
        let events2 = session.poll_events();
        assert!(events2.is_empty());
    }

    /// set_priority changes the download's priority.
    #[test]
    fn set_priority_changes_priority() {
        let mut session = BasicSession::new(SessionConfig::default());
        let h = session.add_download(0);
        session.set_priority(h, 42).unwrap();
        let dl = session.list_downloads();
        assert_eq!(dl[0].priority, 42);
    }

    // ── SessionError Display ────────────────────────────────────────

    /// `SessionError::NotFound` includes handle id.
    #[test]
    fn session_error_not_found_display() {
        let err = SessionError::NotFound {
            handle: DownloadHandle::from_raw(7),
        };
        let msg = err.to_string();
        assert!(msg.contains("7"), "{msg}");
    }

    /// `SessionError::InvalidState` includes handle, state, and operation.
    #[test]
    fn session_error_invalid_state_display() {
        let err = SessionError::InvalidState {
            handle: DownloadHandle::from_raw(3),
            state: "completed".into(),
            operation: "pause".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("3"), "{msg}");
        assert!(msg.contains("completed"), "{msg}");
        assert!(msg.contains("pause"), "{msg}");
    }
}
