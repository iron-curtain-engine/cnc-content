// SPDX-License-Identifier: MIT OR Apache-2.0

//! Bounded upload slot management — XDCC-style queuing.
//!
//! ## What
//!
//! Manages a fixed number of concurrent upload slots with a FIFO waiting
//! queue. When all slots are occupied, new requests are queued rather than
//! rejected outright — the requester is told their queue position and
//! estimated wait time.
//!
//! ## Why — XDCC bot slot queuing lesson
//!
//! IRC XDCC bots serve files to exactly N users simultaneously (e.g.
//! "Slots: 3/3, Queue: 7/20"). This prevents a popular file from
//! overwhelming the bot's upload bandwidth. Queued users see their
//! position and can decide to wait or leave.
//!
//! Applied to P2P: a seeding peer with limited upload bandwidth should
//! not accept unbounded concurrent piece requests. Slot management
//! ensures fair service: each active download gets a guaranteed
//! bandwidth share, and queued peers get predictable wait times rather
//! than degraded speed for everyone.
//!
//! ## How
//!
//! - [`UploadQueue`]: Manages `max_slots` active slots and a bounded
//!   FIFO queue. When a slot frees up, the next queued peer is promoted.
//! - [`SlotResult`]: Returned when a peer requests a slot — either
//!   granted immediately or queued with position info.

use std::collections::VecDeque;
use std::time::Instant;

use crate::peer_id::PeerId;

// ── Constants ───────────────────────────────────────────────────────

/// Default maximum concurrent upload slots (XDCC convention: 3–5 slots).
pub const DEFAULT_MAX_SLOTS: usize = 4;

/// Default maximum queue depth — prevents unbounded memory from peers
/// that queue and never collect. XDCC bots typically cap at 20–50.
pub const DEFAULT_MAX_QUEUE: usize = 20;

// ── SlotResult ──────────────────────────────────────────────────────

/// Result of requesting an upload slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotResult {
    /// Slot granted immediately — the peer can start downloading.
    Granted,
    /// All slots occupied — peer is queued at the given position.
    /// Position 1 = next in line.
    Queued {
        /// 1-based position in the queue.
        position: usize,
        /// Total peers waiting ahead (same as `position - 1`).
        peers_ahead: usize,
    },
    /// Queue is full — peer should try again later or find another source.
    QueueFull {
        /// Maximum queue depth.
        max_queue: usize,
    },
}

// ── QueueEntry ──────────────────────────────────────────────────────

/// A peer waiting in the upload queue.
#[derive(Debug, Clone, PartialEq, Eq)]
struct QueueEntry {
    /// Peer identity.
    peer_id: PeerId,
    /// When the peer joined the queue (for wait-time reporting).
    queued_at: Instant,
}

// ── UploadQueue ─────────────────────────────────────────────────────

/// Bounded upload slot manager with FIFO queue.
///
/// ```
/// use p2p_distribute::upload_queue::{UploadQueue, SlotResult};
/// use p2p_distribute::PeerId;
/// use std::time::Instant;
///
/// let mut queue = UploadQueue::new(2, 5); // 2 slots, queue depth 5
/// let now = Instant::now();
///
/// let peer_a = PeerId::from_key_material(b"a");
/// let peer_b = PeerId::from_key_material(b"b");
/// let peer_c = PeerId::from_key_material(b"c");
///
/// assert_eq!(queue.request_slot(peer_a, now), SlotResult::Granted);
/// assert_eq!(queue.request_slot(peer_b, now), SlotResult::Granted);
/// assert_eq!(queue.request_slot(peer_c, now), SlotResult::Queued {
///     position: 1,
///     peers_ahead: 0,
/// });
/// ```
#[derive(Debug)]
pub struct UploadQueue {
    /// Maximum concurrent upload slots.
    max_slots: usize,
    /// Maximum queue depth.
    max_queue: usize,
    /// Currently active peers (occupying slots).
    active: Vec<PeerId>,
    /// FIFO queue of peers waiting for a slot.
    waiting: VecDeque<QueueEntry>,
}

impl UploadQueue {
    /// Creates a new upload queue with the given limits.
    pub fn new(max_slots: usize, max_queue: usize) -> Self {
        Self {
            max_slots,
            max_queue,
            active: Vec::with_capacity(max_slots),
            waiting: VecDeque::with_capacity(max_queue),
        }
    }

    /// Requests an upload slot for a peer.
    ///
    /// - If a slot is available, the peer is granted immediately.
    /// - If no slot is available but the queue has room, the peer is queued.
    /// - If both are full, returns `QueueFull`.
    ///
    /// If the peer is already active or already queued, the existing
    /// state is returned without duplicate insertion.
    pub fn request_slot(&mut self, peer_id: PeerId, now: Instant) -> SlotResult {
        // Already active — treat as re-request.
        if self.active.contains(&peer_id) {
            return SlotResult::Granted;
        }

        // Already queued — return current position.
        if let Some(pos) = self.waiting.iter().position(|e| e.peer_id == peer_id) {
            return SlotResult::Queued {
                position: pos.saturating_add(1),
                peers_ahead: pos,
            };
        }

        // Try to grant a slot.
        if self.active.len() < self.max_slots {
            self.active.push(peer_id);
            return SlotResult::Granted;
        }

        // Try to queue.
        if self.waiting.len() < self.max_queue {
            self.waiting.push_back(QueueEntry {
                peer_id,
                queued_at: now,
            });
            let position = self.waiting.len();
            return SlotResult::Queued {
                position,
                peers_ahead: position.saturating_sub(1),
            };
        }

        SlotResult::QueueFull {
            max_queue: self.max_queue,
        }
    }

    /// Releases a peer's upload slot and promotes the next queued peer.
    ///
    /// Returns the promoted peer's ID if one was waiting, or `None` if
    /// the queue was empty.
    ///
    /// Call this when a peer's download completes or the peer disconnects.
    pub fn release_slot(&mut self, peer_id: &PeerId) -> Option<PeerId> {
        // Remove from active.
        if let Some(pos) = self.active.iter().position(|id| id == peer_id) {
            self.active.remove(pos);
        }

        // Promote next queued peer if there's now a free slot.
        if self.active.len() < self.max_slots {
            if let Some(entry) = self.waiting.pop_front() {
                let promoted = entry.peer_id;
                self.active.push(promoted);
                return Some(promoted);
            }
        }

        None
    }

    /// Removes a peer from the queue (they gave up waiting).
    ///
    /// Returns `true` if the peer was found and removed.
    pub fn cancel_queue(&mut self, peer_id: &PeerId) -> bool {
        if let Some(pos) = self.waiting.iter().position(|e| e.peer_id == *peer_id) {
            self.waiting.remove(pos);
            return true;
        }
        false
    }

    /// Returns the number of active slots in use.
    pub fn active_count(&self) -> usize {
        self.active.len()
    }

    /// Returns the number of peers waiting in the queue.
    pub fn queue_length(&self) -> usize {
        self.waiting.len()
    }

    /// Returns the maximum slot count.
    pub fn max_slots(&self) -> usize {
        self.max_slots
    }

    /// Returns the maximum queue depth.
    pub fn max_queue(&self) -> usize {
        self.max_queue
    }

    /// Returns the queue position (1-based) for a peer, if queued.
    pub fn queue_position(&self, peer_id: &PeerId) -> Option<usize> {
        self.waiting
            .iter()
            .position(|e| e.peer_id == *peer_id)
            .map(|p| p.saturating_add(1))
    }

    /// Returns `true` if the peer currently holds an active slot.
    pub fn is_active(&self, peer_id: &PeerId) -> bool {
        self.active.iter().any(|id| id == peer_id)
    }

    /// Evicts queued entries older than `max_wait` from the queue.
    ///
    /// Prevents ghost entries from peers that queued and disconnected
    /// without cancelling.
    pub fn evict_stale(&mut self, now: Instant, max_wait: std::time::Duration) {
        self.waiting
            .retain(|entry| now.duration_since(entry.queued_at) < max_wait);
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn make_peer(name: &[u8]) -> PeerId {
        PeerId::from_key_material(name)
    }

    // ── Slot granting ───────────────────────────────────────────────

    /// Slots are granted immediately when available.
    ///
    /// With 3 max slots and 0 active, the first 3 peers should all
    /// receive Granted.
    #[test]
    fn slots_granted_when_available() {
        let mut q = UploadQueue::new(3, 10);
        let now = Instant::now();

        assert_eq!(q.request_slot(make_peer(b"a"), now), SlotResult::Granted);
        assert_eq!(q.request_slot(make_peer(b"b"), now), SlotResult::Granted);
        assert_eq!(q.request_slot(make_peer(b"c"), now), SlotResult::Granted);
        assert_eq!(q.active_count(), 3);
    }

    /// Excess peers are queued in FIFO order.
    ///
    /// When all slots are full, subsequent requests go to the queue with
    /// increasing position numbers.
    #[test]
    fn excess_peers_queued_fifo() {
        let mut q = UploadQueue::new(1, 5);
        let now = Instant::now();

        assert_eq!(q.request_slot(make_peer(b"a"), now), SlotResult::Granted);

        let r1 = q.request_slot(make_peer(b"b"), now);
        assert_eq!(
            r1,
            SlotResult::Queued {
                position: 1,
                peers_ahead: 0,
            }
        );

        let r2 = q.request_slot(make_peer(b"c"), now);
        assert_eq!(
            r2,
            SlotResult::Queued {
                position: 2,
                peers_ahead: 1,
            }
        );
    }

    /// Queue full rejection when both slots and queue are exhausted.
    ///
    /// The peer receives QueueFull with the max_queue value,
    /// enabling it to display a meaningful message.
    #[test]
    fn queue_full_rejection() {
        let mut q = UploadQueue::new(1, 1);
        let now = Instant::now();

        q.request_slot(make_peer(b"a"), now);
        q.request_slot(make_peer(b"b"), now); // queued
        let r = q.request_slot(make_peer(b"c"), now);
        assert_eq!(r, SlotResult::QueueFull { max_queue: 1 });
    }

    // ── Slot release and promotion ──────────────────────────────────

    /// Releasing a slot promotes the next queued peer.
    ///
    /// FIFO discipline: the first peer to queue is the first to be
    /// promoted when a slot opens.
    #[test]
    fn release_promotes_next_queued() {
        let mut q = UploadQueue::new(1, 5);
        let now = Instant::now();
        let a = make_peer(b"a");
        let b = make_peer(b"b");

        q.request_slot(a, now);
        q.request_slot(b, now); // queued

        let promoted = q.release_slot(&a);
        assert_eq!(promoted, Some(b));
        assert_eq!(q.active_count(), 1);
        assert_eq!(q.queue_length(), 0);
    }

    /// Releasing when queue is empty returns None.
    #[test]
    fn release_with_empty_queue() {
        let mut q = UploadQueue::new(2, 5);
        let now = Instant::now();
        let a = make_peer(b"a");

        q.request_slot(a, now);
        let promoted = q.release_slot(&a);
        assert_eq!(promoted, None);
        assert_eq!(q.active_count(), 0);
    }

    // ── Duplicate handling ──────────────────────────────────────────

    /// Re-requesting while active returns Granted without duplicating.
    ///
    /// A peer that already has a slot should not consume a second slot.
    #[test]
    fn duplicate_active_request() {
        let mut q = UploadQueue::new(2, 5);
        let now = Instant::now();
        let a = make_peer(b"a");

        q.request_slot(a, now);
        assert_eq!(q.request_slot(a, now), SlotResult::Granted);
        assert_eq!(q.active_count(), 1);
    }

    /// Re-requesting while queued returns current position.
    ///
    /// A peer already in the queue should not create a duplicate entry.
    #[test]
    fn duplicate_queued_request() {
        let mut q = UploadQueue::new(1, 5);
        let now = Instant::now();

        q.request_slot(make_peer(b"a"), now);
        q.request_slot(make_peer(b"b"), now); // queued at 1

        let r = q.request_slot(make_peer(b"b"), now);
        assert_eq!(
            r,
            SlotResult::Queued {
                position: 1,
                peers_ahead: 0,
            }
        );
        assert_eq!(q.queue_length(), 1); // no duplicate
    }

    // ── Queue operations ────────────────────────────────────────────

    /// cancel_queue removes a queued peer.
    ///
    /// A peer that gives up waiting should be cleanly removed.
    #[test]
    fn cancel_queue_removes_peer() {
        let mut q = UploadQueue::new(1, 5);
        let now = Instant::now();
        let b = make_peer(b"b");

        q.request_slot(make_peer(b"a"), now);
        q.request_slot(b, now);
        assert_eq!(q.queue_length(), 1);

        assert!(q.cancel_queue(&b));
        assert_eq!(q.queue_length(), 0);
    }

    /// cancel_queue returns false for unknown peers.
    #[test]
    fn cancel_queue_unknown_peer() {
        let q = &mut UploadQueue::new(2, 5);
        assert!(!q.cancel_queue(&make_peer(b"ghost")));
    }

    /// queue_position returns the correct 1-based position.
    #[test]
    fn queue_position_lookup() {
        let mut q = UploadQueue::new(1, 5);
        let now = Instant::now();
        let b = make_peer(b"b");
        let c = make_peer(b"c");

        q.request_slot(make_peer(b"a"), now);
        q.request_slot(b, now);
        q.request_slot(c, now);

        assert_eq!(q.queue_position(&b), Some(1));
        assert_eq!(q.queue_position(&c), Some(2));
        assert_eq!(q.queue_position(&make_peer(b"unknown")), None);
    }

    /// is_active returns true only for peers occupying slots.
    #[test]
    fn is_active_check() {
        let mut q = UploadQueue::new(1, 5);
        let now = Instant::now();
        let a = make_peer(b"a");
        let b = make_peer(b"b");

        q.request_slot(a, now);
        q.request_slot(b, now);

        assert!(q.is_active(&a));
        assert!(!q.is_active(&b));
    }

    /// Stale queue entry eviction removes old entries.
    ///
    /// Prevents ghost entries from peers that disconnected without
    /// cancelling their queue position.
    #[test]
    fn evict_stale_entries() {
        let now = Instant::now();
        let mut q = UploadQueue::new(1, 5);
        let a = make_peer(b"a");
        let b = make_peer(b"b");
        let c = make_peer(b"c");

        q.request_slot(a, now);
        q.request_slot(b, now); // queued at t=0
        let later = now + Duration::from_secs(60);
        q.request_slot(c, later); // queued at t=60

        // Evict entries older than 30 seconds.
        q.evict_stale(later, Duration::from_secs(30));

        // Peer b (queued at t=0) should be evicted. Peer c (queued at t=60) stays.
        assert_eq!(q.queue_length(), 1);
        assert_eq!(q.queue_position(&c), Some(1));
        assert_eq!(q.queue_position(&b), None);
    }

    // ── Boundary cases ──────────────────────────────────────────────

    /// Zero max_slots means everything goes to queue.
    ///
    /// Edge case: a seeder that temporarily disables uploads.
    #[test]
    fn zero_max_slots() {
        let mut q = UploadQueue::new(0, 3);
        let now = Instant::now();

        let r = q.request_slot(make_peer(b"a"), now);
        assert_eq!(
            r,
            SlotResult::Queued {
                position: 1,
                peers_ahead: 0,
            }
        );
    }

    /// Zero max_queue with available slots still grants.
    #[test]
    fn zero_max_queue_grants_if_slot_available() {
        let mut q = UploadQueue::new(2, 0);
        let now = Instant::now();

        assert_eq!(q.request_slot(make_peer(b"a"), now), SlotResult::Granted);
        assert_eq!(q.request_slot(make_peer(b"b"), now), SlotResult::Granted);
        let r = q.request_slot(make_peer(b"c"), now);
        assert_eq!(r, SlotResult::QueueFull { max_queue: 0 });
    }

    /// Accessors return expected values.
    #[test]
    fn accessor_values() {
        let q = UploadQueue::new(4, 20);
        assert_eq!(q.max_slots(), 4);
        assert_eq!(q.max_queue(), 20);
        assert_eq!(q.active_count(), 0);
        assert_eq!(q.queue_length(), 0);
    }
}
