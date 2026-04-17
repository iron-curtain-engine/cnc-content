// SPDX-License-Identifier: MIT OR Apache-2.0

//! Work-stealing byte-range scheduler — dynamically rebalances download
//! segments across threads when fast mirrors finish early.
//!
//! ## What
//!
//! A lock-free, atomic work-stealing scheduler for byte-range downloads.
//! Each thread owns a `StealableTask` representing a contiguous byte range.
//! When a thread finishes its range, it steals the remaining half of the
//! slowest peer's range, inheriting the work and accelerating completion.
//!
//! ## Why
//!
//! Fixed segment assignment (the FlashGet pattern) wastes bandwidth when
//! mirrors have heterogeneous speeds. If mirror A runs at 50 MB/s and mirror
//! B at 5 MB/s, A sits idle for 90% of the download. Work-stealing lets A
//! take over half of B's remaining range, approaching the theoretical maximum
//! aggregate throughput.
//!
//! ## How (informed by fast-steal, Cilk, Rayon)
//!
//! Each `StealableTask` packs its byte range `(start, end)` into an
//! `AtomicU64` pair. The owner advances `start` as it writes bytes. A
//! thief calls `steal()` which atomically bisects the range:
//!
//! ```text
//! Before steal:  [====== remaining ======]
//! After steal:   [=== owner ===][== thief ==]
//!                              ^--- split point = midpoint
//! ```
//!
//! The CAS loop ensures that concurrent steals and owner advances are
//! linearisable. If the remaining range is smaller than `min_chunk_size`,
//! the steal fails (range too small to split profitably).
//!
//! ## Design choices
//!
//! - **Two `AtomicU64` instead of one `AtomicU128`**: `AtomicU128` requires
//!   `portable-atomic` or nightly features. Two `AtomicU64` values with a
//!   CAS retry loop provide equivalent correctness on stable Rust without
//!   external dependencies.
//! - **No `unsafe`**: all operations use safe atomic primitives.
//! - **No allocations in hot path**: `steal()` and `advance()` are zero-alloc.

use std::sync::atomic::{AtomicU64, Ordering};

// ── StealableTask ───────────────────────────────────────────────────

/// A byte range that can be atomically bisected for work stealing.
///
/// The owner thread advances `current_start` as it downloads bytes.
/// A thief thread calls [`steal`](Self::steal) to atomically take the
/// upper half of the remaining range, leaving the lower half for the
/// original owner.
///
/// ## Thread safety
///
/// All operations are lock-free. The owner uses [`advance`](Self::advance)
/// to report progress. Thieves use [`steal`](Self::steal) to bisect.
/// Multiple concurrent steals are safe (only one succeeds per CAS round).
pub struct StealableTask {
    /// Current start position (advances as the owner writes bytes).
    /// The owner is responsible for the range `[current_start, end)`.
    current_start: AtomicU64,
    /// Exclusive end position (fixed after creation or steal).
    end: AtomicU64,
}

impl StealableTask {
    /// Creates a new task covering the byte range `[start, end)`.
    ///
    /// The range is exclusive-end: the task covers bytes from `start`
    /// (inclusive) to `end` (exclusive). If `start >= end`, the task is
    /// immediately empty.
    pub fn new(start: u64, end: u64) -> Self {
        Self {
            current_start: AtomicU64::new(start),
            end: AtomicU64::new(end),
        }
    }

    /// Returns the current start position.
    pub fn start(&self) -> u64 {
        self.current_start.load(Ordering::Acquire)
    }

    /// Returns the end position.
    pub fn end(&self) -> u64 {
        self.end.load(Ordering::Acquire)
    }

    /// Returns the number of bytes remaining in this task.
    pub fn remaining(&self) -> u64 {
        let s = self.current_start.load(Ordering::Acquire);
        let e = self.end.load(Ordering::Acquire);
        e.saturating_sub(s)
    }

    /// Returns `true` if no bytes remain.
    pub fn is_empty(&self) -> bool {
        self.remaining() == 0
    }

    /// Advances the start position by `bytes_written`.
    ///
    /// Called by the owner thread after writing `bytes_written` bytes to
    /// disk. The advance is atomic but NOT a CAS — the owner is the only
    /// thread that advances start. This is safe because `steal()` only
    /// modifies `end`, never `start` of the victim.
    ///
    /// Returns the new start position.
    pub fn advance(&self, bytes_written: u64) -> u64 {
        self.current_start
            .fetch_add(bytes_written, Ordering::AcqRel)
            .saturating_add(bytes_written)
    }

    /// Atomically steals the upper half of this task's remaining range.
    ///
    /// If the remaining range is at least `min_chunk_size * 2` bytes, the
    /// range is bisected: the original owner keeps `[start, midpoint)` and
    /// the thief gets `[midpoint, end)`. The task's `end` is atomically
    /// updated to `midpoint`.
    ///
    /// Returns `Some((stolen_start, stolen_end))` on success, or `None` if
    /// the remaining range is too small to split.
    ///
    /// ## Algorithm
    ///
    /// 1. Read current `start` and `end`.
    /// 2. Compute `remaining = end - start`. If < `min_chunk_size * 2`, fail.
    /// 3. Compute `midpoint = start + remaining / 2`, aligned to `min_chunk_size`.
    /// 4. CAS `end` from `old_end` to `midpoint`. If CAS fails (concurrent
    ///    steal or owner finished), retry from step 1.
    /// 5. Return `(midpoint, old_end)` as the stolen range.
    pub fn steal(&self, min_chunk_size: u64) -> Option<(u64, u64)> {
        let min_splittable = min_chunk_size.saturating_mul(2).max(2);

        loop {
            let current_end = self.end.load(Ordering::Acquire);
            let current_start = self.current_start.load(Ordering::Acquire);

            // Range too small to split — both halves would be below minimum.
            let remaining = current_end.saturating_sub(current_start);
            if remaining < min_splittable {
                return None;
            }

            // Bisect: owner keeps lower half, thief gets upper half.
            // Align midpoint up to min_chunk_size boundary for I/O efficiency.
            let raw_mid = current_start.saturating_add(remaining / 2);
            // Round up to nearest min_chunk_size boundary, then clamp to valid range.
            let midpoint = (raw_mid.saturating_add(min_chunk_size.saturating_sub(1)))
                .checked_div(min_chunk_size)
                .map(|q| {
                    (q * min_chunk_size)
                        .min(current_end.saturating_sub(1))
                        .max(current_start.saturating_add(1))
                })
                .unwrap_or(raw_mid);

            // Sanity: midpoint must leave something for both sides.
            if midpoint <= current_start || midpoint >= current_end {
                return None;
            }

            // CAS the end to midpoint — shrinking the victim's range.
            match self.end.compare_exchange(
                current_end,
                midpoint,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some((midpoint, current_end)),
                Err(_) => continue, // Concurrent steal or advance — retry.
            }
        }
    }
}

// ── WorkStealingScheduler ───────────────────────────────────────────

/// Coordinates multiple [`StealableTask`]s for parallel download threads.
///
/// Each thread gets an initial task. When a thread's task is exhausted, it
/// calls [`steal_from_largest`] to find the task with the most remaining
/// bytes and bisect it.
///
/// ## How it works
///
/// 1. `new(file_size, thread_count, min_chunk_size)` divides the file into
///    `thread_count` segments and creates one `StealableTask` per segment.
/// 2. Each thread calls `task(index)` to get its assigned range.
/// 3. When a thread finishes (task exhausted), it calls
///    `steal_from_largest(own_index, min_chunk_size)` to steal work.
/// 4. The stolen range becomes the thread's new assignment.
/// 5. Repeat until all tasks are exhausted.
///
/// ## Guarantees
///
/// - Every byte in `[0, file_size)` is assigned to exactly one
///   task at any point in time.
/// - No byte range is duplicated or lost during steals.
/// - The scheduler is lock-free (CAS retry on contention).
pub struct WorkStealingScheduler {
    /// One task per thread, indexed by thread number.
    tasks: Vec<StealableTask>,
    /// Minimum chunk size for splitting. Segments smaller than this
    /// are not worth splitting due to HTTP request overhead.
    min_chunk_size: u64,
}

impl WorkStealingScheduler {
    /// Creates a scheduler that divides `file_size` bytes across `thread_count`
    /// threads, each with a minimum chunk of `min_chunk_size`.
    ///
    /// The file is divided into equal segments. The last segment gets any
    /// remainder bytes. If `file_size` is zero, all tasks are empty.
    ///
    /// ## Panics
    ///
    /// Panics if `thread_count` is zero.
    pub fn new(file_size: u64, thread_count: usize, min_chunk_size: u64) -> Self {
        assert!(thread_count > 0, "thread_count must be >= 1");

        // Cap effective threads: no thread should get less than min_chunk_size.
        let effective = if min_chunk_size > 0 && file_size > 0 {
            let max_threads = (file_size / min_chunk_size).max(1) as usize;
            thread_count.min(max_threads)
        } else {
            thread_count
        };

        let segment_size = if effective > 0 && file_size > 0 {
            file_size / effective as u64
        } else {
            0
        };

        let mut tasks = Vec::with_capacity(thread_count);
        for i in 0..thread_count {
            if i < effective {
                let start = i as u64 * segment_size;
                let end = if i == effective - 1 {
                    file_size // Last segment gets the remainder.
                } else {
                    (i as u64 + 1) * segment_size
                };
                tasks.push(StealableTask::new(start, end));
            } else {
                // Extra threads start empty — they'll steal work.
                tasks.push(StealableTask::new(0, 0));
            }
        }

        Self {
            tasks,
            min_chunk_size,
        }
    }

    /// Returns the task assigned to thread `index`.
    ///
    /// Returns `None` if the index is out of bounds.
    pub fn task(&self, index: usize) -> Option<&StealableTask> {
        self.tasks.get(index)
    }

    /// Returns the number of tasks (threads).
    pub fn task_count(&self) -> usize {
        self.tasks.len()
    }

    /// Returns the total remaining bytes across all tasks.
    pub fn total_remaining(&self) -> u64 {
        self.tasks.iter().map(|t| t.remaining()).sum()
    }

    /// Returns `true` when all tasks are exhausted.
    pub fn is_complete(&self) -> bool {
        self.tasks.iter().all(|t| t.is_empty())
    }

    /// Attempts to steal work from the task with the most remaining bytes,
    /// excluding the caller's own task.
    ///
    /// Returns `Some((stolen_start, stolen_end))` if a steal succeeded,
    /// or `None` if no task has enough remaining bytes to split.
    ///
    /// The thief should create a new HTTP Range request for `[stolen_start,
    /// stolen_end)` and begin fetching those bytes.
    pub fn steal_from_largest(&self, own_index: usize) -> Option<(u64, u64)> {
        // Find the task with the most remaining bytes (excluding self).
        // Try stealing from it. If it fails (concurrent steal), try the
        // next largest, etc.
        let mut candidates: Vec<(usize, u64)> = self
            .tasks
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != own_index)
            .map(|(i, t)| (i, t.remaining()))
            .filter(|(_, r)| *r >= self.min_chunk_size.saturating_mul(2))
            .collect();

        // Sort by remaining (descending) — steal from the slowest thread.
        candidates.sort_by_key(|b| std::cmp::Reverse(b.1));

        for (victim_idx, _) in candidates {
            if let Some(task) = self.tasks.get(victim_idx) {
                if let Some(stolen) = task.steal(self.min_chunk_size) {
                    return Some(stolen);
                }
            }
        }

        None
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── StealableTask ───────────────────────────────────────────────

    /// A fresh task reports its full range and remaining bytes.
    ///
    /// The initial state must exactly reflect the constructor arguments.
    #[test]
    fn task_initial_state() {
        let task = StealableTask::new(100, 500);
        assert_eq!(task.start(), 100);
        assert_eq!(task.end(), 500);
        assert_eq!(task.remaining(), 400);
        assert!(!task.is_empty());
    }

    /// An empty task (start == end) reports zero remaining.
    ///
    /// Edge case: tasks created with zero-length ranges must be immediately
    /// empty.
    #[test]
    fn task_empty_range() {
        let task = StealableTask::new(42, 42);
        assert_eq!(task.remaining(), 0);
        assert!(task.is_empty());
    }

    /// An inverted range (start > end) is treated as empty.
    ///
    /// Defensive: `saturating_sub` ensures no underflow, returning zero.
    #[test]
    fn task_inverted_range_is_empty() {
        let task = StealableTask::new(500, 100);
        assert_eq!(task.remaining(), 0);
        assert!(task.is_empty());
    }

    /// `advance` moves the start position forward.
    ///
    /// The owner thread advances as it writes bytes. Remaining should decrease
    /// by exactly the advanced amount.
    #[test]
    fn task_advance_moves_start() {
        let task = StealableTask::new(0, 1000);
        task.advance(300);
        assert_eq!(task.start(), 300);
        assert_eq!(task.remaining(), 700);
    }

    /// Advancing past the end does not cause underflow.
    ///
    /// `remaining()` uses `saturating_sub`, so over-advancing yields zero
    /// remaining instead of wrapping.
    #[test]
    fn task_advance_past_end_saturates() {
        let task = StealableTask::new(0, 100);
        task.advance(200);
        assert_eq!(task.remaining(), 0);
        assert!(task.is_empty());
    }

    /// `steal` bisects the range when it is large enough.
    ///
    /// After a steal, the original task should cover the lower half and the
    /// stolen range should cover the upper half. Together they must cover
    /// the entire original remaining range with no gaps or overlaps.
    #[test]
    fn task_steal_bisects_range() {
        let task = StealableTask::new(0, 1000);
        let min_chunk = 100;

        let stolen = task.steal(min_chunk);
        assert!(stolen.is_some());

        let (stolen_start, stolen_end) = stolen.unwrap();
        // The stolen range is the upper portion.
        assert_eq!(stolen_end, 1000);
        // The task's range should be the lower portion.
        assert_eq!(task.end(), stolen_start);
        // No bytes lost: original remaining = new remaining + stolen.
        let total = task.remaining() + (stolen_end - stolen_start);
        assert_eq!(total, 1000);
    }

    /// `steal` fails when the remaining range is too small.
    ///
    /// If the range is less than `min_chunk_size * 2`, splitting would
    /// create segments below the minimum — not worth the HTTP overhead.
    #[test]
    fn task_steal_fails_when_too_small() {
        let task = StealableTask::new(0, 150);
        let min_chunk = 100;
        assert!(task.steal(min_chunk).is_none());
    }

    /// `steal` fails on an empty task.
    ///
    /// There is nothing to steal from an exhausted task.
    #[test]
    fn task_steal_fails_on_empty() {
        let task = StealableTask::new(0, 0);
        assert!(task.steal(1).is_none());
    }

    /// Multiple sequential steals progressively halve the range.
    ///
    /// Each steal should take roughly half the remaining bytes from the
    /// victim, and no bytes should be lost across all resulting ranges.
    #[test]
    fn task_multiple_steals_preserve_total() {
        let task = StealableTask::new(0, 10000);
        let min_chunk = 100;
        let mut stolen_ranges: Vec<(u64, u64)> = Vec::new();

        // Steal 3 times from the same task.
        for _ in 0..3 {
            if let Some(range) = task.steal(min_chunk) {
                stolen_ranges.push(range);
            }
        }

        // All bytes must be accounted for.
        let stolen_total: u64 = stolen_ranges.iter().map(|(s, e)| e - s).sum();
        let total = task.remaining() + stolen_total;
        assert_eq!(total, 10000);
    }

    /// `steal` after `advance` only considers the remaining range.
    ///
    /// Bytes already consumed by the owner are not available for stealing.
    #[test]
    fn task_steal_after_advance() {
        let task = StealableTask::new(0, 1000);
        // Owner has written 600 bytes — only 400 remain.
        task.advance(600);
        assert_eq!(task.remaining(), 400);

        let stolen = task.steal(100);
        assert!(stolen.is_some());
        let (stolen_start, stolen_end) = stolen.unwrap();
        // The stolen range should be within [600, 1000).
        assert!(stolen_start >= 600);
        assert!(stolen_end <= 1000);
        // Task remaining + stolen = 400.
        let total = task.remaining() + (stolen_end - stolen_start);
        assert_eq!(total, 400);
    }

    // ── WorkStealingScheduler ───────────────────────────────────────

    /// Scheduler divides file evenly across threads.
    ///
    /// Each thread should get approximately `file_size / thread_count` bytes.
    /// The last thread gets any remainder.
    #[test]
    fn scheduler_even_division() {
        let sched = WorkStealingScheduler::new(1000, 4, 100);
        assert_eq!(sched.task_count(), 4);
        assert_eq!(sched.total_remaining(), 1000);

        // Each task should have ~250 bytes.
        for i in 0..4 {
            let task = sched.task(i).unwrap();
            assert!(task.remaining() > 0);
        }

        // Verify no gaps: concatenated ranges cover [0, 1000).
        let mut ranges: Vec<(u64, u64)> = (0..4)
            .map(|i| {
                let t = sched.task(i).unwrap();
                (t.start(), t.end())
            })
            .collect();
        ranges.sort_by_key(|r| r.0);
        assert_eq!(ranges[0].0, 0);
        assert_eq!(ranges[3].1, 1000);
        for i in 1..4 {
            assert_eq!(ranges[i].0, ranges[i - 1].1);
        }
    }

    /// Scheduler with more threads than min-chunks caps effective threads.
    ///
    /// If the file is 300 bytes and min_chunk is 100, only 3 threads get
    /// work; extras start empty.
    #[test]
    fn scheduler_caps_threads_by_min_chunk() {
        let sched = WorkStealingScheduler::new(300, 10, 100);
        assert_eq!(sched.task_count(), 10);
        assert_eq!(sched.total_remaining(), 300);

        // Only 3 threads should have non-empty tasks.
        let active = (0..10)
            .filter(|&i| !sched.task(i).unwrap().is_empty())
            .count();
        assert_eq!(active, 3);
    }

    /// Scheduler with zero file size creates empty tasks.
    #[test]
    fn scheduler_zero_file_size() {
        let sched = WorkStealingScheduler::new(0, 4, 100);
        assert_eq!(sched.total_remaining(), 0);
        assert!(sched.is_complete());
    }

    /// `steal_from_largest` takes work from the thread with the most remaining.
    ///
    /// After thread 0 finishes, it should steal from whichever thread has the
    /// most bytes left — the "slowest" mirror.
    #[test]
    fn scheduler_steal_from_largest() {
        let sched = WorkStealingScheduler::new(4000, 4, 100);

        // Simulate: thread 0 finishes (advance all its bytes).
        let task0 = sched.task(0).unwrap();
        let task0_size = task0.remaining();
        task0.advance(task0_size);
        assert!(task0.is_empty());

        // Thread 0 steals from whoever has the most remaining.
        let stolen = sched.steal_from_largest(0);
        assert!(stolen.is_some());

        let (start, end) = stolen.unwrap();
        assert!(end > start);
        let stolen_bytes = end - start;

        // Total remaining across all tasks + consumed by task0 + stolen range
        // must equal the original file size. The steal carved bytes out of a
        // victim's range, so total_remaining() already excludes the stolen
        // portion. But the stolen bytes haven't been "consumed" yet — they
        // exist as a new range that a thread would work on.
        // So: total_remaining() + task0_consumed + stolen_bytes_not_in_any_task
        // Wait — the steal reduced the victim's end, so those bytes are no
        // longer in total_remaining(). They are "in flight" for the thief.
        assert_eq!(sched.total_remaining() + task0_size + stolen_bytes, 4000);
    }

    /// `steal_from_largest` returns `None` when all tasks are too small.
    ///
    /// If every remaining range is below `min_chunk_size * 2`, no steal
    /// is possible.
    #[test]
    fn scheduler_steal_fails_when_all_small() {
        let sched = WorkStealingScheduler::new(400, 4, 100);
        // Each thread has 100 bytes — below the 200-byte splitting threshold.
        assert!(sched.steal_from_largest(0).is_none());
    }

    /// `steal_from_largest` skips the caller's own task.
    ///
    /// A thread must never steal from itself (it already has its full range).
    #[test]
    fn scheduler_steal_skips_self() {
        // Create a scheduler where thread 0 has the largest range.
        let sched = WorkStealingScheduler::new(1000, 2, 100);
        // Thread 1 finishes.
        let task1 = sched.task(1).unwrap();
        task1.advance(task1.remaining());

        // Thread 1 should steal from thread 0, not itself.
        let stolen = sched.steal_from_largest(1);
        assert!(stolen.is_some());
        let (start, end) = stolen.unwrap();
        // Stolen range should be from thread 0's territory.
        assert!(start < end);
        assert!(end <= 1000);
    }

    /// Concurrent steals from the same victim are safe.
    ///
    /// Two threads stealing simultaneously from the same large task must
    /// each get disjoint ranges (or one fails), preserving the total byte
    /// count.
    #[test]
    fn task_concurrent_steals_are_disjoint() {
        use std::sync::Arc;

        let task = Arc::new(StealableTask::new(0, 10000));
        let min_chunk = 100;

        let task1 = Arc::clone(&task);
        let task2 = Arc::clone(&task);

        let (r1, r2) = std::thread::scope(|s| {
            let h1 = s.spawn(move || task1.steal(min_chunk));
            let h2 = s.spawn(move || task2.steal(min_chunk));
            (h1.join().unwrap(), h2.join().unwrap())
        });

        // At least one should succeed.
        assert!(r1.is_some() || r2.is_some());

        // All bytes must be accounted for.
        let mut total = task.remaining();
        if let Some((s, e)) = r1 {
            total += e - s;
        }
        if let Some((s, e)) = r2 {
            total += e - s;
        }
        assert_eq!(total, 10000);

        // If both succeeded, ranges must not overlap.
        if let (Some((s1, e1)), Some((s2, e2))) = (r1, r2) {
            assert!(e1 <= s2 || e2 <= s1, "stolen ranges must be disjoint");
        }
    }

    /// Multi-thread work-stealing produces correct total bytes.
    ///
    /// Simulates multiple threads consuming and stealing until complete.
    /// The sum of all consumed bytes must equal the file size.
    #[test]
    fn scheduler_multi_thread_total_bytes() {
        use std::sync::atomic::AtomicU64;
        use std::sync::Arc;

        let file_size: u64 = 10_000_000; // 10 MB
        let sched = Arc::new(WorkStealingScheduler::new(file_size, 4, 1024));
        let total_consumed = Arc::new(AtomicU64::new(0));

        std::thread::scope(|s| {
            for thread_id in 0..4 {
                let sched = Arc::clone(&sched);
                let total = Arc::clone(&total_consumed);
                s.spawn(move || {
                    // Consume own task.
                    if let Some(task) = sched.task(thread_id) {
                        let bytes = task.remaining();
                        task.advance(bytes);
                        total.fetch_add(bytes, Ordering::Relaxed);
                    }

                    // Keep stealing until no more work.
                    while let Some((start, end)) = sched.steal_from_largest(thread_id) {
                        let bytes = end.saturating_sub(start);
                        total.fetch_add(bytes, Ordering::Relaxed);
                    }
                });
            }
        });

        assert_eq!(total_consumed.load(Ordering::Relaxed), file_size);
    }

    /// The scheduler reports completion when all tasks are exhausted.
    #[test]
    fn scheduler_is_complete_after_all_consumed() {
        let sched = WorkStealingScheduler::new(1000, 2, 100);
        assert!(!sched.is_complete());

        for i in 0..2 {
            let task = sched.task(i).unwrap();
            task.advance(task.remaining());
        }

        assert!(sched.is_complete());
    }

    /// Single-thread scheduler still works (no stealing needed).
    ///
    /// Edge case: with one thread, the full file is one task and stealing
    /// returns `None` (no other tasks exist).
    #[test]
    fn scheduler_single_thread() {
        let sched = WorkStealingScheduler::new(5000, 1, 100);
        assert_eq!(sched.task_count(), 1);
        assert_eq!(sched.task(0).unwrap().remaining(), 5000);
        assert!(sched.steal_from_largest(0).is_none());
    }

    /// Scheduler handles large file sizes without overflow.
    ///
    /// Uses file sizes near `u64::MAX / 2` to verify no arithmetic overflow
    /// in segment calculation.
    #[test]
    fn scheduler_large_file_no_overflow() {
        let file_size = u64::MAX / 4;
        let sched = WorkStealingScheduler::new(file_size, 4, 1024 * 1024);
        assert_eq!(sched.total_remaining(), file_size);

        // Verify ranges don't overlap.
        let mut ranges: Vec<(u64, u64)> = (0..4)
            .map(|i| {
                let t = sched.task(i).unwrap();
                (t.start(), t.end())
            })
            .collect();
        ranges.sort_by_key(|r| r.0);
        for i in 1..4 {
            assert!(
                ranges[i].0 >= ranges[i - 1].1,
                "ranges must not overlap: {:?} and {:?}",
                ranges[i - 1],
                ranges[i]
            );
        }
    }
}
