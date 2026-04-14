// SPDX-License-Identifier: MIT OR Apache-2.0

//! Content source discovery — runtime discovery of content mirrors,
//! peers, and bootstrap nodes.
//!
//! ## What
//!
//! Provides a pluggable registry of content source discovery methods,
//! combining static configuration (embedded mirror lists) with runtime
//! discovery (HTTP mirror list fetch, DHT announces, PEX gossip).
//!
//! ## Why — lessons from C&C community content infrastructure
//!
//! C&C community content distribution uses multiple discovery layers:
//!
//! - **OpenRA mirror lists**: The OpenRA project hosts master mirror
//!   lists (e.g. `www.openra.net/packages/...`) that redirect to
//!   community-operated mirrors. This indirection allows mirror
//!   rotation without client updates — mirrors that die are silently
//!   removed from the list, new mirrors are added.
//! - **CnCNet tunnel discovery**: CnCNet clients discover relay
//!   tunnels at startup by querying a central list that returns
//!   available tunnel servers with their region and capacity.
//! - **Iron Curtain content-bootstrap**: The IC engine hosts mirror
//!   lists in a GitHub repo, served via `raw.githubusercontent.com`.
//!   These act as the bootstrap layer — the first place a client
//!   checks for mirror URLs.
//!
//! Key lessons for P2P content distribution:
//!
//! - **Static + dynamic layering**: Embed a handful of well-known
//!   bootstrap URLs at compile time, but fetch the real mirror list
//!   at runtime. This survives both stale binaries (bootstrap URLs
//!   point to the list, not the content) and list server outages
//!   (static fallbacks still work).
//! - **Discovery must be additive**: Each discovery method adds to
//!   the pool; none replaces the pool. A DHT announce coexists with
//!   HTTP mirrors and PEX. This maximises availability.
//! - **Freshness metadata**: Each discovered source records when it
//!   was discovered and when it was last verified. Stale sources are
//!   deprioritised but not immediately removed.
//!
//! ## How
//!
//! - [`DiscoverySource`]: A discovered content source with metadata
//!   (URL, discovery method, freshness, health).
//! - [`DiscoveryMethod`]: How the source was found (Static, MirrorList,
//!   Dht, Pex, Bootstrap).
//! - [`SourceRegistry`]: Accumulates discovered sources, deduplicates,
//!   ranks by health and freshness.

use std::time::{Duration, Instant};

// ── Constants ───────────────────────────────────────────────────────

/// Maximum number of sources tracked per content item.
const DEFAULT_MAX_SOURCES: usize = 128;

/// Duration after which a source is considered stale if not re-verified.
const DEFAULT_STALE_THRESHOLD: Duration = Duration::from_secs(3600); // 1 hour.

/// Maximum number of failures before a source is considered dead.
const FAILURE_THRESHOLD: u32 = 5;

// ── Discovery method ────────────────────────────────────────────────

/// How a content source was discovered.
///
/// The method carries provenance information used for logging and
/// prioritisation. Static and Bootstrap sources are trusted more
/// than Dht/Pex sources initially.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveryMethod {
    /// Compiled into the binary (e.g. known mirror URLs).
    Static,
    /// Fetched at runtime from a mirror list URL.
    MirrorList,
    /// Announced via DHT (Kademlia or similar).
    Dht,
    /// Received via peer exchange (PEX).
    Pex,
    /// From a bootstrap node or well-known endpoint.
    Bootstrap,
}

impl DiscoveryMethod {
    /// Base trust score for this discovery method.
    ///
    /// Static and Bootstrap sources are more trusted because they are
    /// curated. DHT and PEX sources could be malicious peers.
    pub fn trust_base(self) -> f64 {
        match self {
            Self::Static => 1.0,
            Self::Bootstrap => 0.9,
            Self::MirrorList => 0.8,
            Self::Pex => 0.5,
            Self::Dht => 0.4,
        }
    }
}

// ── Discovery source ────────────────────────────────────────────────

/// A discovered content source with provenance and health metadata.
///
/// Each source represents a single endpoint that can serve content
/// (an HTTP mirror URL, a peer address, a BitTorrent web seed, etc.).
#[derive(Debug, Clone)]
pub struct DiscoverySource {
    /// Unique identifier for this source (typically URL or peer address).
    url: String,
    /// How this source was discovered.
    method: DiscoveryMethod,
    /// When the source was first discovered.
    discovered_at: Instant,
    /// When the source was last successfully contacted.
    last_verified: Option<Instant>,
    /// Number of consecutive failures.
    failures: u32,
    /// Number of successful contacts.
    successes: u32,
    /// Optional region hint (e.g. "eu", "us-east").
    region: Option<String>,
}

impl DiscoverySource {
    /// Creates a new discovery source.
    pub fn new(url: String, method: DiscoveryMethod, now: Instant) -> Self {
        Self {
            url,
            method,
            discovered_at: now,
            last_verified: None,
            failures: 0,
            successes: 0,
            region: None,
        }
    }

    /// Creates a discovery source with a region hint.
    pub fn with_region(mut self, region: String) -> Self {
        self.region = Some(region);
        self
    }

    /// Records a successful contact.
    pub fn record_success(&mut self, now: Instant) {
        self.successes = self.successes.saturating_add(1);
        self.failures = 0; // Reset consecutive failures.
        self.last_verified = Some(now);
    }

    /// Records a failed contact.
    pub fn record_failure(&mut self) {
        self.failures = self.failures.saturating_add(1);
    }

    /// Returns whether this source is considered dead.
    pub fn is_dead(&self) -> bool {
        self.failures >= FAILURE_THRESHOLD
    }

    /// Returns whether this source is stale (not verified recently).
    pub fn is_stale(&self, now: Instant, threshold: Duration) -> bool {
        match self.last_verified {
            Some(verified) => now.duration_since(verified) > threshold,
            None => now.duration_since(self.discovered_at) > threshold,
        }
    }

    /// Computes a composite score for ranking.
    ///
    /// Combines trust (from discovery method), success rate, and
    /// freshness into a single score in `[0.0, 1.0]`.
    pub fn score(&self, now: Instant) -> f64 {
        let trust = self.method.trust_base();

        // Success rate component (0.0–1.0).
        let total = self.successes.saturating_add(self.failures);
        let success_rate = if total == 0 {
            0.5 // No data → neutral.
        } else {
            self.successes as f64 / total as f64
        };

        // Freshness component: 1.0 if just verified, decays toward 0.
        let freshness = match self.last_verified {
            Some(verified) => {
                let age_secs = now.duration_since(verified).as_secs() as f64;
                1.0 - (age_secs / DEFAULT_STALE_THRESHOLD.as_secs() as f64).min(1.0)
            }
            None => 0.3, // Never verified → low but not zero.
        };

        // Penalty for consecutive failures.
        let failure_penalty = if self.failures > 0 {
            1.0 - (self.failures as f64 / FAILURE_THRESHOLD as f64).min(1.0)
        } else {
            1.0
        };

        // Composite: trust (30%) + success_rate (30%) + freshness (20%) +
        // failure_penalty (20%).
        let raw = trust * 0.3 + success_rate * 0.3 + freshness * 0.2 + failure_penalty * 0.2;
        raw.clamp(0.0, 1.0)
    }

    /// Returns the source URL.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Returns how this source was discovered.
    pub fn method(&self) -> DiscoveryMethod {
        self.method
    }

    /// Returns the region hint.
    pub fn region(&self) -> Option<&str> {
        self.region.as_deref()
    }

    /// Returns the number of successful contacts.
    pub fn successes(&self) -> u32 {
        self.successes
    }

    /// Returns the number of consecutive failures.
    pub fn failures(&self) -> u32 {
        self.failures
    }
}

// ── Source registry ─────────────────────────────────────────────────

/// Error conditions for the source registry.
#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    /// Registry is full.
    #[error("source registry full (max {max} sources)")]
    Full {
        /// Maximum number of sources allowed.
        max: usize,
    },
}

/// Accumulates discovered content sources from multiple discovery
/// methods, deduplicates by URL, and provides ranked access.
///
/// ```
/// use p2p_distribute::content_discovery::{SourceRegistry, DiscoverySource, DiscoveryMethod};
/// use std::time::Instant;
///
/// let mut registry = SourceRegistry::new(64);
/// let now = Instant::now();
///
/// let source = DiscoverySource::new(
///     "https://mirror.example.com/content.zip".into(),
///     DiscoveryMethod::MirrorList,
///     now,
/// );
/// registry.add(source).unwrap();
///
/// assert_eq!(registry.len(), 1);
/// let ranked = registry.ranked(now);
/// assert_eq!(ranked.len(), 1);
/// ```
pub struct SourceRegistry {
    /// All tracked sources.
    sources: Vec<DiscoverySource>,
    /// Maximum capacity.
    max_sources: usize,
}

impl SourceRegistry {
    /// Creates a new source registry with the given capacity.
    pub fn new(max_sources: usize) -> Self {
        Self {
            sources: Vec::with_capacity(max_sources.min(DEFAULT_MAX_SOURCES)),
            max_sources,
        }
    }

    /// Adds a source to the registry.
    ///
    /// If the source URL already exists, the entry is updated with
    /// the new discovery method if it has higher trust. Returns an
    /// error if the registry is full and no duplicates exist.
    pub fn add(&mut self, source: DiscoverySource) -> Result<(), RegistryError> {
        // Check for existing source with same URL.
        if let Some(existing) = self.sources.iter_mut().find(|s| s.url == source.url) {
            // Keep the higher-trust discovery method.
            if source.method.trust_base() > existing.method.trust_base() {
                existing.method = source.method;
            }
            // Refresh discovery timestamp.
            existing.discovered_at = source.discovered_at;
            return Ok(());
        }

        if self.sources.len() >= self.max_sources {
            return Err(RegistryError::Full {
                max: self.max_sources,
            });
        }

        self.sources.push(source);
        Ok(())
    }

    /// Records a successful contact for a source URL.
    pub fn record_success(&mut self, url: &str, now: Instant) {
        if let Some(source) = self.sources.iter_mut().find(|s| s.url == url) {
            source.record_success(now);
        }
    }

    /// Records a failed contact for a source URL.
    pub fn record_failure(&mut self, url: &str) {
        if let Some(source) = self.sources.iter_mut().find(|s| s.url == url) {
            source.record_failure();
        }
    }

    /// Returns all sources ranked by score (best first).
    pub fn ranked(&self, now: Instant) -> Vec<&DiscoverySource> {
        let mut ranked: Vec<&DiscoverySource> = self.sources.iter().collect();
        ranked.sort_by(|a, b| {
            b.score(now)
                .partial_cmp(&a.score(now))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        ranked
    }

    /// Returns only live (non-dead) sources ranked by score.
    pub fn live_ranked(&self, now: Instant) -> Vec<&DiscoverySource> {
        let mut ranked: Vec<&DiscoverySource> =
            self.sources.iter().filter(|s| !s.is_dead()).collect();
        ranked.sort_by(|a, b| {
            b.score(now)
                .partial_cmp(&a.score(now))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        ranked
    }

    /// Prunes dead sources from the registry.
    pub fn prune_dead(&mut self) -> usize {
        let before = self.sources.len();
        self.sources.retain(|s| !s.is_dead());
        before.saturating_sub(self.sources.len())
    }

    /// Prunes sources stale beyond the given threshold.
    pub fn prune_stale(&mut self, now: Instant, threshold: Duration) -> usize {
        let before = self.sources.len();
        self.sources.retain(|s| !s.is_stale(now, threshold));
        before.saturating_sub(self.sources.len())
    }

    /// Returns the number of tracked sources.
    pub fn len(&self) -> usize {
        self.sources.len()
    }

    /// Returns whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }

    /// Returns the number of live (non-dead) sources.
    pub fn live_count(&self) -> usize {
        self.sources.iter().filter(|s| !s.is_dead()).count()
    }

    /// Returns a source by URL.
    pub fn get(&self, url: &str) -> Option<&DiscoverySource> {
        self.sources.iter().find(|s| s.url == url)
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_source(url: &str, method: DiscoveryMethod, now: Instant) -> DiscoverySource {
        DiscoverySource::new(url.into(), method, now)
    }

    // ── DiscoveryMethod trust ordering ──────────────────────────────

    /// Static sources have the highest trust.
    ///
    /// Compiled-in mirrors are curated by the developer and verified
    /// pre-release.
    #[test]
    fn trust_ordering() {
        assert!(DiscoveryMethod::Static.trust_base() > DiscoveryMethod::Bootstrap.trust_base());
        assert!(DiscoveryMethod::Bootstrap.trust_base() > DiscoveryMethod::MirrorList.trust_base());
        assert!(DiscoveryMethod::MirrorList.trust_base() > DiscoveryMethod::Pex.trust_base());
        assert!(DiscoveryMethod::Pex.trust_base() > DiscoveryMethod::Dht.trust_base());
    }

    // ── DiscoverySource scoring ─────────────────────────────────────

    /// New source gets a moderate score.
    ///
    /// No successes or failures yet → score reflects trust base and
    /// neutral success rate.
    #[test]
    fn new_source_moderate_score() {
        let now = Instant::now();
        let source = make_source(
            "http://mirror.example.com/a",
            DiscoveryMethod::MirrorList,
            now,
        );

        let score = source.score(now);
        assert!(score > 0.3 && score < 0.8, "score = {score}");
    }

    /// Successes improve score.
    ///
    /// Sources that respond reliably should be ranked higher.
    #[test]
    fn successes_improve_score() {
        let now = Instant::now();
        let mut source = make_source(
            "http://mirror.example.com/a",
            DiscoveryMethod::MirrorList,
            now,
        );

        let before = source.score(now);
        source.record_success(now);
        source.record_success(now);
        let after = source.score(now);

        assert!(
            after > before,
            "after {after} should exceed before {before}"
        );
    }

    /// Failures reduce score and eventually mark dead.
    ///
    /// Consecutive failures degrade scoring and eventually trigger
    /// dead classification.
    #[test]
    fn failures_degrade_and_kill() {
        let now = Instant::now();
        let mut source = make_source(
            "http://mirror.example.com/a",
            DiscoveryMethod::MirrorList,
            now,
        );

        for _ in 0..FAILURE_THRESHOLD {
            source.record_failure();
        }

        assert!(source.is_dead());
        assert!(
            source.score(now) < 0.4,
            "dead source score = {}",
            source.score(now)
        );
    }

    /// Success resets consecutive failure counter.
    ///
    /// A source that recovers should not carry old failures forever.
    #[test]
    fn success_resets_failures() {
        let now = Instant::now();
        let mut source = make_source(
            "http://mirror.example.com/a",
            DiscoveryMethod::MirrorList,
            now,
        );

        source.record_failure();
        source.record_failure();
        assert_eq!(source.failures(), 2);

        source.record_success(now);
        assert_eq!(source.failures(), 0);
    }

    /// Stale check based on verification time.
    ///
    /// Sources not re-verified within the threshold should be marked
    /// stale.
    #[test]
    fn stale_after_threshold() {
        let now = Instant::now();
        let source = make_source(
            "http://mirror.example.com/a",
            DiscoveryMethod::MirrorList,
            now,
        );

        // Just created → not stale.
        assert!(!source.is_stale(now, DEFAULT_STALE_THRESHOLD));

        // After threshold.
        let later = now + DEFAULT_STALE_THRESHOLD + Duration::from_secs(1);
        assert!(source.is_stale(later, DEFAULT_STALE_THRESHOLD));
    }

    // ── SourceRegistry ──────────────────────────────────────────────

    /// Duplicate URL updates existing source.
    ///
    /// Re-discovering the same URL from a higher-trust method should
    /// upgrade the entry's trust level.
    #[test]
    fn duplicate_url_updates_trust() {
        let now = Instant::now();
        let mut registry = SourceRegistry::new(10);

        let dht = make_source("http://mirror.example.com/a", DiscoveryMethod::Dht, now);
        registry.add(dht).unwrap();

        let static_src = make_source("http://mirror.example.com/a", DiscoveryMethod::Static, now);
        registry.add(static_src).unwrap();

        assert_eq!(registry.len(), 1, "should not duplicate");
        let source = registry.get("http://mirror.example.com/a").unwrap();
        assert_eq!(source.method(), DiscoveryMethod::Static);
    }

    /// Registry respects capacity limits.
    ///
    /// Prevents unbounded memory growth from DHT/PEX flood.
    #[test]
    fn registry_full() {
        let now = Instant::now();
        let mut registry = SourceRegistry::new(2);

        registry
            .add(make_source("http://a.com/1", DiscoveryMethod::Static, now))
            .unwrap();
        registry
            .add(make_source("http://b.com/2", DiscoveryMethod::Static, now))
            .unwrap();

        let result = registry.add(make_source("http://c.com/3", DiscoveryMethod::Static, now));
        assert!(result.is_err());
    }

    /// Ranked returns sources in score-descending order.
    ///
    /// The coordinator picks sources from the top to maximise
    /// reliability.
    #[test]
    fn ranked_order() {
        let now = Instant::now();
        let mut registry = SourceRegistry::new(10);

        // Static source should rank higher than DHT.
        registry
            .add(make_source(
                "http://dht.example.com/a",
                DiscoveryMethod::Dht,
                now,
            ))
            .unwrap();
        registry
            .add(make_source(
                "http://static.example.com/a",
                DiscoveryMethod::Static,
                now,
            ))
            .unwrap();

        let ranked = registry.ranked(now);
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].url(), "http://static.example.com/a");
    }

    /// Live ranked excludes dead sources.
    ///
    /// Dead sources should not be offered to the download
    /// coordinator.
    #[test]
    fn live_ranked_excludes_dead() {
        let now = Instant::now();
        let mut registry = SourceRegistry::new(10);

        registry
            .add(make_source(
                "http://live.example.com/a",
                DiscoveryMethod::Static,
                now,
            ))
            .unwrap();
        registry
            .add(make_source(
                "http://dead.example.com/a",
                DiscoveryMethod::Static,
                now,
            ))
            .unwrap();

        // Kill the second source.
        for _ in 0..FAILURE_THRESHOLD {
            registry.record_failure("http://dead.example.com/a");
        }

        let live = registry.live_ranked(now);
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].url(), "http://live.example.com/a");
    }

    /// Prune dead removes dead sources.
    ///
    /// Periodic cleanup reclaims capacity.
    #[test]
    fn prune_dead_sources() {
        let now = Instant::now();
        let mut registry = SourceRegistry::new(10);

        registry
            .add(make_source("http://a.com/1", DiscoveryMethod::Static, now))
            .unwrap();
        registry
            .add(make_source("http://b.com/2", DiscoveryMethod::Static, now))
            .unwrap();

        for _ in 0..FAILURE_THRESHOLD {
            registry.record_failure("http://b.com/2");
        }

        let pruned = registry.prune_dead();
        assert_eq!(pruned, 1);
        assert_eq!(registry.len(), 1);
    }

    /// Empty registry returns empty rankings.
    ///
    /// Edge case: no sources discovered yet.
    #[test]
    fn empty_registry() {
        let now = Instant::now();
        let registry = SourceRegistry::new(10);

        assert!(registry.is_empty());
        assert_eq!(registry.live_count(), 0);
        assert!(registry.ranked(now).is_empty());
    }

    /// Region hint is preserved through add.
    ///
    /// Sources with region hints should retain them for geographic
    /// selection.
    #[test]
    fn region_hint_preserved() {
        let now = Instant::now();
        let mut registry = SourceRegistry::new(10);

        let source = DiscoverySource::new(
            "http://eu.mirror.example.com/a".into(),
            DiscoveryMethod::MirrorList,
            now,
        )
        .with_region("eu-west".into());

        registry.add(source).unwrap();
        let found = registry.get("http://eu.mirror.example.com/a").unwrap();
        assert_eq!(found.region(), Some("eu-west"));
    }
}
