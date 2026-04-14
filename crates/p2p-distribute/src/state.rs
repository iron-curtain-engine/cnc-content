// SPDX-License-Identifier: MIT OR Apache-2.0

//! Formal download state machine — tracks each download's lifecycle phase.
//!
//! ## Design (informed by librqbit, libtorrent, aria2)
//!
//! Every production BT client represents a download as a state machine:
//! librqbit uses `ManagedTorrentState` (Initializing/Paused/Live/Error/None),
//! libtorrent has 3 queues (checking/downloading/seeding), and aria2 tracks
//! states for pause/resume/retry. Without a formal state type, the coordinator
//! relies on implicit state (boolean flags, error variants) which makes it
//! impossible for callers to query "what is this download doing right now?"
//!
//! ## How
//!
//! [`DownloadState`] is an enum encoding every valid lifecycle phase. State
//! transitions are enforced: certain transitions are valid, others are not.
//! The [`DownloadStateMachine`] wrapper tracks the current state and provides
//! methods that only succeed for valid transitions.

use std::fmt;

// ── DownloadState ───────────────────────────────────────────────────

/// Lifecycle phase of a single download.
///
/// Transitions:
/// ```text
/// Queued ──→ Checking ──→ Downloading ──→ Seeding ──→ Completed
///   │           │              │             │
///   └───────────┴──────────────┴─────────────┘──→ Paused
///                              │                      │
///                              └──────────────────────┘
///                              │
///                              └──→ Error
/// ```
///
/// `Queued → Checking → Downloading` is the happy path. `Paused` can be
/// entered from any active state and exited back to the previous state.
/// `Error` is entered on unrecoverable failure. `Seeding` is entered after
/// download completion when the peer wants to serve pieces to others.
/// `Completed` is the terminal state after seeding has stopped or was never
/// started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadState {
    /// Waiting in the download queue. Not yet started.
    Queued,
    /// Verifying existing data on disk (resume scenario). The `progress`
    /// field tracks how far verification has gotten (0.0–1.0).
    Checking { progress: u32 },
    /// Actively downloading pieces. This is the coordinator's main loop.
    Downloading,
    /// Download complete, now serving pieces to other peers.
    Seeding,
    /// Download is paused. The `previous` field records the state to
    /// return to when unpausing.
    Paused {
        /// Which state the download was in before pausing.
        previous: Box<DownloadState>,
    },
    /// An unrecoverable error occurred.
    Error {
        /// Human-readable error description.
        message: String,
    },
    /// Terminal state — download is done and no longer active.
    Completed,
}

impl fmt::Display for DownloadState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Queued => write!(f, "queued"),
            Self::Checking { progress } => write!(f, "checking ({progress}%)"),
            Self::Downloading => write!(f, "downloading"),
            Self::Seeding => write!(f, "seeding"),
            Self::Paused { previous } => write!(f, "paused (was: {previous})"),
            Self::Error { message } => write!(f, "error: {message}"),
            Self::Completed => write!(f, "completed"),
        }
    }
}

// ── DownloadStateMachine ────────────────────────────────────────────

/// State machine wrapper that enforces valid transitions.
///
/// Consumers query the current state and request transitions. Invalid
/// transitions return `Err` with a description of why the transition is
/// not allowed.
pub struct DownloadStateMachine {
    state: DownloadState,
}

/// Error returned when a state transition is invalid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransitionError {
    /// The state the download was in.
    pub from: String,
    /// The state the caller tried to transition to.
    pub to: String,
    /// Why the transition is not allowed.
    pub reason: String,
}

impl fmt::Display for TransitionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid transition from {} to {}: {}",
            self.from, self.to, self.reason
        )
    }
}

impl std::error::Error for TransitionError {}

impl DownloadStateMachine {
    /// Creates a new state machine in the `Queued` state.
    pub fn new() -> Self {
        Self {
            state: DownloadState::Queued,
        }
    }

    /// Returns the current state.
    pub fn state(&self) -> &DownloadState {
        &self.state
    }

    /// Transitions to `Checking` (start hash verification of existing data).
    pub fn start_checking(&mut self) -> Result<(), TransitionError> {
        match &self.state {
            DownloadState::Queued => {
                self.state = DownloadState::Checking { progress: 0 };
                Ok(())
            }
            other => Err(TransitionError {
                from: other.to_string(),
                to: "checking".into(),
                reason: "can only start checking from queued state".into(),
            }),
        }
    }

    /// Updates verification progress (0–100).
    pub fn update_checking_progress(&mut self, progress: u32) {
        if let DownloadState::Checking { progress: p } = &mut self.state {
            *p = progress.min(100);
        }
    }

    /// Transitions to `Downloading` (start fetching pieces).
    pub fn start_downloading(&mut self) -> Result<(), TransitionError> {
        match &self.state {
            DownloadState::Queued | DownloadState::Checking { .. } => {
                self.state = DownloadState::Downloading;
                Ok(())
            }
            DownloadState::Paused { previous }
                if matches!(**previous, DownloadState::Downloading) =>
            {
                self.state = DownloadState::Downloading;
                Ok(())
            }
            other => Err(TransitionError {
                from: other.to_string(),
                to: "downloading".into(),
                reason: "can only start downloading from queued, checking, or paused(downloading)"
                    .into(),
            }),
        }
    }

    /// Transitions to `Seeding` (download complete, serving to others).
    pub fn start_seeding(&mut self) -> Result<(), TransitionError> {
        match &self.state {
            DownloadState::Downloading => {
                self.state = DownloadState::Seeding;
                Ok(())
            }
            other => Err(TransitionError {
                from: other.to_string(),
                to: "seeding".into(),
                reason: "can only start seeding from downloading".into(),
            }),
        }
    }

    /// Transitions to `Paused`, remembering the current state.
    pub fn pause(&mut self) -> Result<(), TransitionError> {
        match &self.state {
            DownloadState::Queued
            | DownloadState::Checking { .. }
            | DownloadState::Downloading
            | DownloadState::Seeding => {
                let prev = std::mem::replace(&mut self.state, DownloadState::Queued);
                self.state = DownloadState::Paused {
                    previous: Box::new(prev),
                };
                Ok(())
            }
            DownloadState::Paused { .. } => Err(TransitionError {
                from: "paused".into(),
                to: "paused".into(),
                reason: "already paused".into(),
            }),
            other => Err(TransitionError {
                from: other.to_string(),
                to: "paused".into(),
                reason: "cannot pause from a terminal state".into(),
            }),
        }
    }

    /// Resumes from `Paused` back to the previous state.
    pub fn resume(&mut self) -> Result<(), TransitionError> {
        match &self.state {
            DownloadState::Paused { .. } => {
                // Extract the previous state from the Paused variant.
                if let DownloadState::Paused { previous } =
                    std::mem::replace(&mut self.state, DownloadState::Queued)
                {
                    self.state = *previous;
                }
                Ok(())
            }
            other => Err(TransitionError {
                from: other.to_string(),
                to: "resume".into(),
                reason: "can only resume from paused state".into(),
            }),
        }
    }

    /// Transitions to `Error`.
    pub fn fail(&mut self, message: String) {
        self.state = DownloadState::Error { message };
    }

    /// Transitions to `Completed` (terminal state).
    pub fn complete(&mut self) -> Result<(), TransitionError> {
        match &self.state {
            DownloadState::Downloading | DownloadState::Seeding => {
                self.state = DownloadState::Completed;
                Ok(())
            }
            other => Err(TransitionError {
                from: other.to_string(),
                to: "completed".into(),
                reason: "can only complete from downloading or seeding".into(),
            }),
        }
    }

    /// Returns `true` if the download is in a terminal state (Error or Completed).
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.state,
            DownloadState::Error { .. } | DownloadState::Completed
        )
    }

    /// Returns `true` if the download is actively transferring data.
    pub fn is_active(&self) -> bool {
        matches!(
            self.state,
            DownloadState::Downloading | DownloadState::Seeding
        )
    }
}

impl Default for DownloadStateMachine {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Happy path ──────────────────────────────────────────────────

    /// Full lifecycle: Queued → Checking → Downloading → Seeding → Completed.
    ///
    /// The standard download lifecycle must proceed through all phases without
    /// error when transitions are requested in order.
    #[test]
    fn happy_path_lifecycle() {
        let mut sm = DownloadStateMachine::new();
        assert_eq!(sm.state(), &DownloadState::Queued);

        sm.start_checking().unwrap();
        assert!(matches!(sm.state(), DownloadState::Checking { .. }));

        sm.update_checking_progress(50);
        assert!(matches!(
            sm.state(),
            DownloadState::Checking { progress: 50 }
        ));

        sm.start_downloading().unwrap();
        assert_eq!(sm.state(), &DownloadState::Downloading);

        sm.start_seeding().unwrap();
        assert_eq!(sm.state(), &DownloadState::Seeding);

        sm.complete().unwrap();
        assert_eq!(sm.state(), &DownloadState::Completed);
        assert!(sm.is_terminal());
    }

    /// Skip checking: Queued → Downloading directly.
    ///
    /// Fresh downloads with no resume data skip the checking phase.
    #[test]
    fn skip_checking_queued_to_downloading() {
        let mut sm = DownloadStateMachine::new();
        sm.start_downloading().unwrap();
        assert_eq!(sm.state(), &DownloadState::Downloading);
    }

    /// Complete directly from Downloading (without seeding).
    ///
    /// Downloads that don't participate in seeding go straight to completed.
    #[test]
    fn complete_from_downloading() {
        let mut sm = DownloadStateMachine::new();
        sm.start_downloading().unwrap();
        sm.complete().unwrap();
        assert_eq!(sm.state(), &DownloadState::Completed);
    }

    // ── Pause / Resume ──────────────────────────────────────────────

    /// Pause from Downloading and resume back.
    ///
    /// The state machine must remember the previous state and restore it
    /// on resume.
    #[test]
    fn pause_and_resume_downloading() {
        let mut sm = DownloadStateMachine::new();
        sm.start_downloading().unwrap();
        sm.pause().unwrap();
        assert!(matches!(sm.state(), DownloadState::Paused { .. }));
        assert!(!sm.is_active());

        sm.resume().unwrap();
        assert_eq!(sm.state(), &DownloadState::Downloading);
        assert!(sm.is_active());
    }

    /// Cannot pause when already paused.
    #[test]
    fn cannot_double_pause() {
        let mut sm = DownloadStateMachine::new();
        sm.start_downloading().unwrap();
        sm.pause().unwrap();
        assert!(sm.pause().is_err());
    }

    /// Cannot resume when not paused.
    #[test]
    fn cannot_resume_when_not_paused() {
        let mut sm = DownloadStateMachine::new();
        sm.start_downloading().unwrap();
        assert!(sm.resume().is_err());
    }

    // ── Error transitions ───────────────────────────────────────────

    /// `fail()` moves to Error from any state.
    #[test]
    fn fail_from_downloading() {
        let mut sm = DownloadStateMachine::new();
        sm.start_downloading().unwrap();
        sm.fail("disk full".into());
        assert!(matches!(sm.state(), DownloadState::Error { .. }));
        assert!(sm.is_terminal());
    }

    /// Cannot transition out of Error (except via new state machine).
    #[test]
    fn cannot_download_from_error() {
        let mut sm = DownloadStateMachine::new();
        sm.fail("test error".into());
        assert!(sm.start_downloading().is_err());
        assert!(sm.start_checking().is_err());
    }

    /// Cannot transition out of Completed.
    #[test]
    fn cannot_download_from_completed() {
        let mut sm = DownloadStateMachine::new();
        sm.start_downloading().unwrap();
        sm.complete().unwrap();
        assert!(sm.start_downloading().is_err());
        assert!(sm.pause().is_err());
    }

    // ── Invalid transitions ─────────────────────────────────────────

    /// Cannot seed from Queued.
    #[test]
    fn cannot_seed_from_queued() {
        let mut sm = DownloadStateMachine::new();
        assert!(sm.start_seeding().is_err());
    }

    /// Cannot complete from Queued.
    #[test]
    fn cannot_complete_from_queued() {
        let mut sm = DownloadStateMachine::new();
        assert!(sm.complete().is_err());
    }

    // ── Display ─────────────────────────────────────────────────────

    /// `DownloadState::Display` produces meaningful messages.
    #[test]
    fn state_display() {
        assert_eq!(DownloadState::Queued.to_string(), "queued");
        assert_eq!(DownloadState::Downloading.to_string(), "downloading");
        assert_eq!(DownloadState::Seeding.to_string(), "seeding");
        assert_eq!(DownloadState::Completed.to_string(), "completed");
        assert!(DownloadState::Checking { progress: 42 }
            .to_string()
            .contains("42"));
        assert!(DownloadState::Error {
            message: "disk full".into()
        }
        .to_string()
        .contains("disk full"));
    }

    /// `TransitionError::Display` includes from, to, and reason.
    #[test]
    fn transition_error_display() {
        let err = TransitionError {
            from: "queued".into(),
            to: "seeding".into(),
            reason: "need to download first".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("queued"), "{msg}");
        assert!(msg.contains("seeding"), "{msg}");
        assert!(msg.contains("need to download first"), "{msg}");
    }
}
