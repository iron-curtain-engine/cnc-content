// SPDX-License-Identifier: MIT OR Apache-2.0

//! Error types for workshop-core operations.
//!
//! Every error carries structured context (what failed, where, why) so
//! callers can produce actionable diagnostics without inspecting opaque
//! error chains.

use std::path::PathBuf;

use thiserror::Error;

/// Errors that can occur in workshop-core operations.
#[derive(Debug, Error)]
pub enum WorkshopError {
    /// A resource identifier component (publisher or name) is invalid.
    #[error("invalid {field}: \"{value}\" — {reason}")]
    InvalidIdentifier {
        field: String,
        value: String,
        reason: String,
    },

    /// A content hash did not match the expected value.
    #[error("integrity check failed for {path}: expected {expected}, got {actual}")]
    IntegrityMismatch {
        path: String,
        expected: String,
        actual: String,
    },

    /// A resource was not found in the index.
    #[error("resource not found: {id}")]
    NotFound { id: String },

    /// A version was not found for a known resource.
    #[error("version {version} not found for {id}")]
    VersionNotFound { id: String, version: String },

    /// An index backend operation failed.
    #[error("index error: {detail}")]
    Index { detail: String },

    /// A blob store operation failed.
    #[error("blob store error: {detail}")]
    BlobStore { detail: String },

    /// An I/O error occurred.
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },

    /// A manifest parsing error occurred.
    #[error("manifest parse error: {detail}")]
    ManifestParse { detail: String },

    /// All index sources failed during a failover query.
    ///
    /// This means every configured backend was unreachable or returned
    /// an error. Check network connectivity and backend health.
    #[error("all {count} index sources failed; last error: {last_error}")]
    AllSourcesFailed { count: usize, last_error: String },

    // ── Announcement security errors ─────────────────────────────────
    /// An announcement's Ed25519 signature failed verification.
    ///
    /// This means the announcement was either tampered with in transit,
    /// or forged by someone who doesn't hold the publisher's private key.
    /// **Drop the announcement and ban the sending peer.**
    #[error("invalid signature on announcement from {publisher} for {resource}")]
    InvalidSignature { publisher: String, resource: String },

    /// An announcement has a sequence number ≤ the highest already seen
    /// for this (publisher, resource) pair.
    ///
    /// This is either a replay attack (adversary re-broadcasting old
    /// announcements) or benign out-of-order delivery. Either way,
    /// the announcement is stale and must be discarded.
    #[error("stale announcement for {resource}: sequence {received} ≤ known {known}")]
    StaleAnnouncement {
        resource: String,
        received: u64,
        known: u64,
    },

    /// An announcement's timestamp is too far in the future.
    ///
    /// Peers reject announcements with timestamps more than a tolerance
    /// window ahead of local time. This prevents attackers from
    /// setting far-future timestamps to make their announcements
    /// appear "newest" indefinitely.
    #[error("announcement timestamp {timestamp} is {drift_secs}s ahead of local time (max {max_drift_secs}s)")]
    FutureTimestamp {
        timestamp: u64,
        drift_secs: u64,
        max_drift_secs: u64,
    },

    /// An announcement's sequence number is suspiciously high.
    ///
    /// An attacker might set sequence to `u64::MAX` to prevent the real
    /// publisher from ever publishing a higher sequence. Peers reject
    /// sequence jumps larger than a configurable threshold.
    #[error(
        "suspicious sequence jump for {resource}: {old_seq} → {new_seq} (max jump {max_jump})"
    )]
    SequenceJumpTooLarge {
        resource: String,
        old_seq: u64,
        new_seq: u64,
        max_jump: u64,
    },

    /// The peer has exceeded the announcement rate limit.
    ///
    /// A malicious peer flooding announcements is a denial-of-service
    /// vector. Peers enforce a per-publisher rate limit to bound memory
    /// and CPU usage from processing announcements.
    #[error("rate limit exceeded for {publisher}: {count} announcements in {window_secs}s (max {max_count})")]
    RateLimitExceeded {
        publisher: String,
        count: u64,
        window_secs: u64,
        max_count: u64,
    },

    /// A publisher's key is on the revocation list.
    ///
    /// The publisher's signing key has been explicitly revoked (key
    /// compromise, DMCA takedown, malware). All announcements from
    /// this key are rejected regardless of signature validity.
    #[error("publisher {publisher} is revoked: {reason}")]
    PublisherRevoked { publisher: String, reason: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Display messages carry context ───────────────────────────────

    /// InvalidIdentifier display includes field, value, and reason.
    #[test]
    fn display_invalid_identifier() {
        let err = WorkshopError::InvalidIdentifier {
            field: "publisher".to_string(),
            value: "Bad_Name".to_string(),
            reason: "must be lowercase".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("publisher"), "{msg}");
        assert!(msg.contains("Bad_Name"), "{msg}");
        assert!(msg.contains("must be lowercase"), "{msg}");
    }

    /// IntegrityMismatch display includes path and both hashes.
    #[test]
    fn display_integrity_mismatch() {
        let err = WorkshopError::IntegrityMismatch {
            path: "community/sprites@1.0.0".to_string(),
            expected: "abc123".to_string(),
            actual: "def456".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("abc123"), "{msg}");
        assert!(msg.contains("def456"), "{msg}");
    }

    /// NotFound display includes the resource identifier.
    #[test]
    fn display_not_found() {
        let err = WorkshopError::NotFound {
            id: "community/hd-sprites".to_string(),
        };
        assert!(err.to_string().contains("community/hd-sprites"));
    }

    /// AllSourcesFailed display includes count and last error.
    #[test]
    fn display_all_sources_failed() {
        let err = WorkshopError::AllSourcesFailed {
            count: 3,
            last_error: "connection refused".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("3"), "{msg}");
        assert!(msg.contains("connection refused"), "{msg}");
    }

    // ── Announcement security error display ──────────────────────────

    /// InvalidSignature display includes publisher and resource.
    #[test]
    fn display_invalid_signature() {
        let err = WorkshopError::InvalidSignature {
            publisher: "pub:aabbccdd".to_string(),
            resource: "alice/sprites".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("pub:aabbccdd"), "{msg}");
        assert!(msg.contains("alice/sprites"), "{msg}");
    }

    /// StaleAnnouncement display includes sequence numbers.
    #[test]
    fn display_stale_announcement() {
        let err = WorkshopError::StaleAnnouncement {
            resource: "alice/sprites".to_string(),
            received: 3,
            known: 5,
        };
        let msg = err.to_string();
        assert!(msg.contains("3"), "{msg}");
        assert!(msg.contains("5"), "{msg}");
    }

    /// FutureTimestamp display includes drift details.
    #[test]
    fn display_future_timestamp() {
        let err = WorkshopError::FutureTimestamp {
            timestamp: 9_999_999_999,
            drift_secs: 3600,
            max_drift_secs: 300,
        };
        let msg = err.to_string();
        assert!(msg.contains("3600"), "{msg}");
        assert!(msg.contains("300"), "{msg}");
    }

    /// SequenceJumpTooLarge display includes old, new, and max.
    #[test]
    fn display_sequence_jump() {
        let err = WorkshopError::SequenceJumpTooLarge {
            resource: "alice/sprites".to_string(),
            old_seq: 5,
            new_seq: 1_000_000,
            max_jump: 1000,
        };
        let msg = err.to_string();
        assert!(msg.contains("1000000"), "{msg}");
        assert!(msg.contains("1000"), "{msg}");
    }

    /// RateLimitExceeded display includes publisher and limits.
    #[test]
    fn display_rate_limit() {
        let err = WorkshopError::RateLimitExceeded {
            publisher: "pub:aabbccdd".to_string(),
            count: 100,
            window_secs: 60,
            max_count: 10,
        };
        let msg = err.to_string();
        assert!(msg.contains("100"), "{msg}");
        assert!(msg.contains("10"), "{msg}");
    }

    /// PublisherRevoked display includes reason.
    #[test]
    fn display_publisher_revoked() {
        let err = WorkshopError::PublisherRevoked {
            publisher: "pub:aabbccdd".to_string(),
            reason: "key compromised".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("revoked"), "{msg}");
        assert!(msg.contains("key compromised"), "{msg}");
    }
}
