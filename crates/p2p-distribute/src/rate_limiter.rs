// SPDX-License-Identifier: MIT OR Apache-2.0

//! Token bucket rate limiter — per-peer request throttling.
//!
//! ## What
//!
//! Provides a token bucket rate limiter that controls how frequently a
//! peer (or any entity) can perform a particular action. Each action
//! consumes one token; tokens regenerate at a configurable rate up to a
//! burst capacity. When the bucket is empty, further requests are denied
//! until tokens regenerate.
//!
//! ## Why — IRC flood control lesson (RFC 1459 §8.10)
//!
//! IRC servers throttle clients with a penalty-based token bucket: each
//! command consumes a token, tokens regenerate at a fixed rate (e.g. 1
//! per 2 seconds), and when the bucket empties the server queues or
//! drops messages. This prevents flood attacks without hard-disconnecting
//! well-behaved clients that occasionally burst.
//!
//! Applied to P2P: a single peer could flood the coordinator with piece
//! requests, PEX messages, or DHT queries. A per-peer token bucket lets
//! bursty-but-honest peers through while capping sustained abuse.
//!
//! ## How
//!
//! - [`TokenBucket`]: Single-entity rate limiter with configurable
//!   capacity and refill rate.
//! - [`RateLimiterMap`]: Keyed collection of token buckets (one per
//!   peer), with automatic creation on first access.
//!
//! The refill is computed lazily on each `try_consume()` call — no
//! background timer needed. Elapsed time since the last refill is
//! multiplied by the tokens-per-second rate and added to the current
//! token count (capped at burst capacity).

use std::collections::HashMap;
use std::hash::Hash;
use std::time::Instant;

// ── Constants ───────────────────────────────────────────────────────

/// Default burst capacity — matches IRC's typical 5-message burst
/// allowance before throttling kicks in.
pub const DEFAULT_BURST: u32 = 10;

/// Default refill rate in tokens per second. One token per second is
/// conservative; callers should tune based on expected message rate.
pub const DEFAULT_REFILL_RATE: f64 = 2.0;

// ── TokenBucket ─────────────────────────────────────────────────────

/// A token bucket rate limiter for a single entity.
///
/// Tokens regenerate continuously at `refill_rate` tokens per second,
/// up to a maximum of `burst` tokens. Each action consumes one token.
/// When tokens are exhausted, actions are denied until tokens regenerate.
///
/// ## Design
///
/// The bucket uses lazy refill: tokens are computed on demand based on
/// elapsed time, not via a background timer. This is efficient for
/// systems with many buckets (one per peer) where most are idle.
///
/// ```
/// use p2p_distribute::rate_limiter::TokenBucket;
/// use std::time::Instant;
///
/// let mut bucket = TokenBucket::new(5, 1.0, Instant::now());
///
/// // Burst of 5 requests succeeds.
/// let now = Instant::now();
/// for _ in 0..5 {
///     assert!(bucket.try_consume(now));
/// }
///
/// // 6th request fails — bucket exhausted.
/// assert!(!bucket.try_consume(now));
/// ```
#[derive(Debug, Clone)]
pub struct TokenBucket {
    /// Maximum tokens the bucket can hold (burst capacity).
    burst: u32,
    /// Tokens added per second during refill.
    refill_rate: f64,
    /// Current token count (fractional tokens tracked internally).
    tokens: f64,
    /// Timestamp of the last refill computation.
    last_refill: Instant,
}

impl TokenBucket {
    /// Creates a new token bucket, initially full.
    ///
    /// - `burst`: maximum tokens (and initial count).
    /// - `refill_rate`: tokens regenerated per second.
    /// - `now`: current timestamp for refill tracking.
    pub fn new(burst: u32, refill_rate: f64, now: Instant) -> Self {
        Self {
            burst,
            refill_rate,
            tokens: burst as f64,
            last_refill: now,
        }
    }

    /// Attempts to consume one token. Returns `true` if allowed,
    /// `false` if the bucket is empty (rate limit exceeded).
    ///
    /// Automatically refills tokens based on elapsed time since the
    /// last call.
    pub fn try_consume(&mut self, now: Instant) -> bool {
        self.refill(now);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// Attempts to consume `n` tokens at once. Returns `true` if all
    /// tokens were available, `false` if insufficient (no partial
    /// consumption).
    pub fn try_consume_n(&mut self, n: u32, now: Instant) -> bool {
        self.refill(now);
        let needed = n as f64;
        if self.tokens >= needed {
            self.tokens -= needed;
            true
        } else {
            false
        }
    }

    /// Returns the current token count (truncated to integer).
    pub fn available_tokens(&self) -> u32 {
        self.tokens as u32
    }

    /// Returns the burst capacity.
    pub fn burst(&self) -> u32 {
        self.burst
    }

    /// Returns the refill rate in tokens per second.
    pub fn refill_rate(&self) -> f64 {
        self.refill_rate
    }

    /// Computes elapsed time since last refill and adds tokens.
    fn refill(&mut self, now: Instant) {
        let elapsed = now.duration_since(self.last_refill);
        let new_tokens = elapsed.as_secs_f64() * self.refill_rate;
        if new_tokens > 0.0 {
            self.tokens = (self.tokens + new_tokens).min(self.burst as f64);
            self.last_refill = now;
        }
    }
}

// ── RateLimiterMap ──────────────────────────────────────────────────

/// A keyed collection of token buckets — one per peer (or per entity).
///
/// Buckets are created lazily on first access with the configured
/// defaults. This avoids pre-allocating buckets for peers that never
/// send requests.
///
/// ```
/// use p2p_distribute::rate_limiter::RateLimiterMap;
/// use std::time::Instant;
///
/// let mut limiters: RateLimiterMap<u32> = RateLimiterMap::new(5, 1.0);
/// let now = Instant::now();
///
/// // Peer 42 gets a fresh bucket on first access.
/// assert!(limiters.try_consume(&42, now));
/// ```
#[derive(Debug)]
pub struct RateLimiterMap<K: Eq + Hash> {
    /// Per-key token buckets.
    buckets: HashMap<K, TokenBucket>,
    /// Default burst capacity for new buckets.
    default_burst: u32,
    /// Default refill rate for new buckets.
    default_refill_rate: f64,
}

impl<K: Eq + Hash> RateLimiterMap<K> {
    /// Creates a new rate limiter map with the given defaults.
    pub fn new(default_burst: u32, default_refill_rate: f64) -> Self {
        Self {
            buckets: HashMap::new(),
            default_burst,
            default_refill_rate,
        }
    }

    /// Attempts to consume one token for `key`. Creates a new bucket
    /// if this is the first access for the key.
    pub fn try_consume(&mut self, key: &K, now: Instant) -> bool
    where
        K: Clone,
    {
        let bucket = self
            .buckets
            .entry(key.clone())
            .or_insert_with(|| TokenBucket::new(self.default_burst, self.default_refill_rate, now));
        bucket.try_consume(now)
    }

    /// Returns the bucket for a key, if it exists.
    pub fn get(&self, key: &K) -> Option<&TokenBucket> {
        self.buckets.get(key)
    }

    /// Removes the bucket for a key (e.g. when a peer disconnects).
    pub fn remove(&mut self, key: &K) {
        self.buckets.remove(key);
    }

    /// Returns the number of tracked entities.
    pub fn len(&self) -> usize {
        self.buckets.len()
    }

    /// Returns `true` if no entities are tracked.
    pub fn is_empty(&self) -> bool {
        self.buckets.is_empty()
    }

    /// Removes all entries whose last refill is older than `stale_before`.
    ///
    /// Prevents unbounded memory growth from peers that connected once
    /// and never returned.
    pub fn evict_stale(&mut self, stale_before: Instant) {
        self.buckets
            .retain(|_, bucket| bucket.last_refill >= stale_before);
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    // ── TokenBucket ─────────────────────────────────────────────────

    /// Full burst is available immediately.
    ///
    /// A new bucket starts full, allowing the configured burst of
    /// requests before any regeneration.
    #[test]
    fn full_burst_available() {
        let now = Instant::now();
        let mut bucket = TokenBucket::new(5, 1.0, now);
        for _ in 0..5 {
            assert!(bucket.try_consume(now));
        }
        assert!(!bucket.try_consume(now));
    }

    /// Tokens regenerate over time at the configured rate.
    ///
    /// After consuming all tokens, waiting for 1/rate seconds should
    /// regenerate at least one token.
    #[test]
    fn tokens_regenerate() {
        let now = Instant::now();
        let mut bucket = TokenBucket::new(2, 1.0, now);

        // Drain the bucket.
        assert!(bucket.try_consume(now));
        assert!(bucket.try_consume(now));
        assert!(!bucket.try_consume(now));

        // After 1 second at rate=1.0, one token should be available.
        let later = now + Duration::from_secs(1);
        assert!(bucket.try_consume(later));
        assert!(!bucket.try_consume(later));
    }

    /// Tokens do not exceed burst capacity even after long idle periods.
    ///
    /// The bucket caps at `burst` — excess regeneration is discarded.
    #[test]
    fn tokens_capped_at_burst() {
        let now = Instant::now();
        let mut bucket = TokenBucket::new(3, 10.0, now);

        // Wait a long time — should still cap at 3.
        let later = now + Duration::from_secs(100);
        assert_eq!(bucket.available_tokens(), 3);
        bucket.refill(later);
        assert_eq!(bucket.available_tokens(), 3);
    }

    /// try_consume_n consumes exactly N tokens atomically.
    ///
    /// If N tokens are available, all are consumed. If fewer than N
    /// are available, no tokens are consumed (all-or-nothing).
    #[test]
    fn consume_n_atomic() {
        let now = Instant::now();
        let mut bucket = TokenBucket::new(5, 1.0, now);

        assert!(bucket.try_consume_n(3, now));
        assert_eq!(bucket.available_tokens(), 2);

        // Can't consume 3 more — only 2 remain.
        assert!(!bucket.try_consume_n(3, now));
        // No tokens consumed on failure.
        assert_eq!(bucket.available_tokens(), 2);
    }

    /// Zero burst means all requests are denied.
    ///
    /// Edge case: a bucket with zero capacity never allows anything.
    #[test]
    fn zero_burst_denies_all() {
        let now = Instant::now();
        let mut bucket = TokenBucket::new(0, 1.0, now);
        assert!(!bucket.try_consume(now));
        let later = now + Duration::from_secs(10);
        assert!(!bucket.try_consume(later));
    }

    /// Fractional token regeneration accumulates correctly.
    ///
    /// A rate of 0.5 tokens/sec means a token every 2 seconds. After
    /// 1 second, the 0.5 fractional tokens are not enough but after
    /// 2 seconds they accumulate to 1.0.
    #[test]
    fn fractional_regeneration() {
        let now = Instant::now();
        let mut bucket = TokenBucket::new(5, 0.5, now);

        // Drain all tokens.
        for _ in 0..5 {
            assert!(bucket.try_consume(now));
        }

        // After 1 second at 0.5/sec, only 0.5 tokens — not enough.
        let later_1 = now + Duration::from_secs(1);
        assert!(!bucket.try_consume(later_1));

        // After 2 more seconds (3 total), 1.5 tokens accumulated → 1 available.
        let later_3 = now + Duration::from_secs(3);
        assert!(bucket.try_consume(later_3));
    }

    // ── RateLimiterMap ──────────────────────────────────────────────

    /// Lazy bucket creation for new peers.
    ///
    /// Buckets are created with full burst on first access — no
    /// pre-allocation required.
    #[test]
    fn lazy_bucket_creation() {
        let mut map: RateLimiterMap<u32> = RateLimiterMap::new(3, 1.0);
        let now = Instant::now();

        assert!(map.is_empty());
        assert!(map.try_consume(&1, now));
        assert_eq!(map.len(), 1);
    }

    /// Per-key isolation — one peer's exhaustion doesn't affect another.
    ///
    /// Each peer gets its own independent bucket, so a flood from peer A
    /// doesn't throttle peer B.
    #[test]
    fn per_key_isolation() {
        let mut map: RateLimiterMap<u32> = RateLimiterMap::new(2, 0.0);
        let now = Instant::now();

        // Drain peer 1's bucket.
        assert!(map.try_consume(&1, now));
        assert!(map.try_consume(&1, now));
        assert!(!map.try_consume(&1, now));

        // Peer 2's bucket is still full.
        assert!(map.try_consume(&2, now));
    }

    /// Stale entry eviction removes idle peers.
    ///
    /// Prevents unbounded memory growth from peers that connected
    /// once and never returned.
    #[test]
    fn stale_eviction() {
        let now = Instant::now();
        let mut map: RateLimiterMap<u32> = RateLimiterMap::new(5, 1.0);

        map.try_consume(&1, now);
        map.try_consume(&2, now);
        assert_eq!(map.len(), 2);

        // Peer 3 seen later.
        let later = now + Duration::from_secs(60);
        map.try_consume(&3, later);

        // Evict entries not seen since `later` — peers 1 and 2 are stale.
        map.evict_stale(later);
        assert_eq!(map.len(), 1);
        assert!(map.get(&3).is_some());
        assert!(map.get(&1).is_none());
    }

    /// Remove cleans up a specific peer's bucket.
    ///
    /// Used when a peer disconnects and its rate limit state is no
    /// longer needed.
    #[test]
    fn remove_peer() {
        let mut map: RateLimiterMap<u32> = RateLimiterMap::new(5, 1.0);
        let now = Instant::now();

        map.try_consume(&42, now);
        assert_eq!(map.len(), 1);
        map.remove(&42);
        assert!(map.is_empty());
    }

    /// Accessors return expected values.
    ///
    /// Verifies that burst() and refill_rate() accessors report the
    /// values passed during construction.
    #[test]
    fn bucket_accessors() {
        let bucket = TokenBucket::new(7, 3.5, Instant::now());
        assert_eq!(bucket.burst(), 7);
        assert!((bucket.refill_rate() - 3.5).abs() < f64::EPSILON);
    }
}
