// SPDX-License-Identifier: MIT OR Apache-2.0

//! Package manifest — the bridge between index and transport layers.
//!
//! A `PackageManifest` is what the index stores for each published version.
//! It contains everything the transport layer needs to start a download
//! (info_hash, web_seeds) and everything the integrity layer needs to
//! verify the result (sha256). This type is the key integration point
//! between the three architectural layers:
//!
//! - **Index layer** produces `PackageManifest` entries (from git-index YAML)
//! - **Transport layer** consumes `info_hash` + `web_seeds` to start P2P download
//! - **Integrity layer** verifies the downloaded content against `sha256`
//!
//! Manifest immutability: once a `publisher/name@version` is published,
//! its manifest is fixed forever (D030). The SHA-256 hash and info_hash
//! form a content-addressed identity chain.
//!
//! # Platform independence
//!
//! No field in `PackageManifest` assumes a specific hosting platform.
//! The canonical identity is the `BlobId` (SHA-256 hash) — permanent and
//! universal. URLs (`web_seeds`, `download_urls`) are ephemeral delivery
//! hints that can point to any HTTP server. When a hosting platform
//! disappears, update the URLs in the index — the content identity (hash)
//! and P2P swarm remain unaffected.

use crate::blob::BlobId;
use crate::resource::{Channel, Dependency, ResourceCategory, ResourceId, ResourceVersion};

// ── Package manifest ─────────────────────────────────────────────────

/// A published package version entry as stored in the Workshop index.
///
/// This is the entry that lives in the Workshop index (git repo, HTTP API,
/// or any other backend). It maps a `publisher/name@version` identity to
/// everything needed to download, verify, and install the content.
///
/// # Content addressing chain
///
/// ```text
/// ResourceId + Version  →  PackageManifest  →  BlobId (SHA-256)
///                                            →  info_hash (SHA-1, for BT)
///                                            →  web_seeds (BEP 19 URLs)
/// ```
///
/// The `blob_id` is the canonical identity of the content. The `info_hash`
/// is derived from the torrent metadata over those same bytes. Together
/// they form a two-layer verification: SHA-256 for integrity, SHA-1 for
/// BitTorrent piece validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageManifest {
    /// The resource this manifest belongs to.
    id: ResourceId,
    /// The specific version.
    version: ResourceVersion,
    /// SHA-256 of the package file (canonical content identity).
    blob_id: BlobId,
    /// Total size of the package file in bytes.
    size: u64,
    /// BitTorrent info_hash (SHA-1 of the torrent's info dict).
    /// `None` if no torrent has been generated yet (early phase).
    info_hash: Option<[u8; 20]>,
    /// BEP 19 web seed URLs for HTTP-backed BitTorrent download.
    /// HTTP URLs from any hosting platform that serve the exact same
    /// bytes as the torrent content. Platform-agnostic by design.
    web_seeds: Vec<String>,
    /// Direct HTTP download URLs (fallback when P2P is unavailable).
    /// Multiple URLs across different hosting platforms provide
    /// resilience against any single platform becoming unavailable.
    /// These are delivery hints — the content's identity is `blob_id`.
    download_urls: Vec<String>,
    /// Dependencies on other Workshop resources.
    dependencies: Vec<Dependency>,
    /// Release channel (dev → beta → release progression).
    channel: Channel,
    /// Free-form category tags for discovery.
    categories: Vec<ResourceCategory>,
}

impl PackageManifest {
    /// Creates a new package manifest with hash-based identity only.
    ///
    /// URLs (web seeds, download mirrors) are added via builder methods.
    /// This separation emphasizes that identity is the hash, not a URL —
    /// the manifest works regardless of which platforms host the content.
    ///
    /// # Arguments
    ///
    /// * `id` — Resource identity (publisher/name)
    /// * `version` — Semantic version of this release
    /// * `blob_id` — SHA-256 content hash of the package file
    /// * `size` — Package file size in bytes
    pub fn new(id: ResourceId, version: ResourceVersion, blob_id: BlobId, size: u64) -> Self {
        Self {
            id,
            version,
            blob_id,
            size,
            info_hash: None,
            web_seeds: Vec::new(),
            download_urls: Vec::new(),
            dependencies: Vec::new(),
            channel: Channel::Release,
            categories: Vec::new(),
        }
    }

    /// Sets the BitTorrent info_hash for P2P distribution.
    pub fn with_info_hash(mut self, hash: [u8; 20]) -> Self {
        self.info_hash = Some(hash);
        self
    }

    /// Adds BEP 19 web seed URLs for HTTP-backed BitTorrent download.
    pub fn with_web_seeds(mut self, seeds: Vec<String>) -> Self {
        self.web_seeds = seeds;
        self
    }

    /// Adds direct download URLs for HTTP fallback (multi-platform).
    ///
    /// Multiple URLs across different hosting platforms provide resilience.
    /// URLs are delivery hints — the content's identity is its `blob_id`.
    pub fn with_download_urls(mut self, urls: Vec<String>) -> Self {
        self.download_urls = urls;
        self
    }

    /// Sets the release channel.
    pub fn with_channel(mut self, channel: Channel) -> Self {
        self.channel = channel;
        self
    }

    /// Adds dependency declarations.
    pub fn with_dependencies(mut self, deps: Vec<Dependency>) -> Self {
        self.dependencies = deps;
        self
    }

    /// Adds category tags.
    pub fn with_categories(mut self, cats: Vec<ResourceCategory>) -> Self {
        self.categories = cats;
        self
    }

    pub fn id(&self) -> &ResourceId {
        &self.id
    }

    pub fn version(&self) -> ResourceVersion {
        self.version
    }

    pub fn blob_id(&self) -> &BlobId {
        &self.blob_id
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn info_hash(&self) -> Option<&[u8; 20]> {
        self.info_hash.as_ref()
    }

    pub fn web_seeds(&self) -> &[String] {
        &self.web_seeds
    }

    pub fn download_urls(&self) -> &[String] {
        &self.download_urls
    }

    pub fn dependencies(&self) -> &[Dependency] {
        &self.dependencies
    }

    pub fn channel(&self) -> Channel {
        self.channel
    }

    pub fn categories(&self) -> &[ResourceCategory] {
        &self.categories
    }
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: creates a test manifest with minimal required fields.
    fn test_manifest() -> PackageManifest {
        let id = ResourceId::new("community", "hd-sprites").unwrap();
        let version = ResourceVersion::new(1, 0, 0);
        let blob_id = BlobId::new([0xAB; 32]);
        PackageManifest::new(id, version, blob_id, 1024 * 1024)
    }

    // ── Construction ─────────────────────────────────────────────────

    /// A freshly constructed manifest has sensible defaults.
    #[test]
    fn new_manifest_defaults() {
        let m = test_manifest();
        assert_eq!(m.id().to_string(), "community/hd-sprites");
        assert_eq!(m.version().to_string(), "1.0.0");
        assert_eq!(m.size(), 1024 * 1024);
        assert!(m.info_hash().is_none());
        assert!(m.web_seeds().is_empty());
        assert!(m.download_urls().is_empty());
        assert!(m.dependencies().is_empty());
        assert_eq!(m.channel(), Channel::Release);
        assert!(m.categories().is_empty());
    }

    // ── Builder methods ──────────────────────────────────────────────

    /// Builder methods chain to populate optional fields.
    #[test]
    fn builder_chaining() {
        let info = [0x42; 20];
        let m = test_manifest()
            .with_info_hash(info)
            .with_web_seeds(vec!["https://cdn.example.com/file.icpkg".to_string()])
            .with_channel(Channel::Beta)
            .with_categories(vec![ResourceCategory::new("sprites")]);

        assert_eq!(m.info_hash(), Some(&info));
        assert_eq!(m.web_seeds().len(), 1);
        assert_eq!(m.channel(), Channel::Beta);
        assert_eq!(m.categories().len(), 1);
    }

    // ── Content addressing ───────────────────────────────────────────

    /// The blob_id is the SHA-256 content identity — the core of CAS.
    #[test]
    fn blob_id_is_content_identity() {
        let m = test_manifest();
        assert_eq!(m.blob_id().as_bytes(), &[0xAB; 32]);
    }

    /// Two manifests with the same blob_id reference the same content.
    /// This is how cross-version deduplication works.
    #[test]
    fn shared_blob_id_means_same_content() {
        let m1 = test_manifest();

        // v2.0.0 of the same resource, same content (unchanged sprites)
        let m2 = PackageManifest::new(
            ResourceId::new("community", "hd-sprites").unwrap(),
            ResourceVersion::new(2, 0, 0),
            BlobId::new([0xAB; 32]),
            1024 * 1024,
        );

        // Different versions, same blob — CAS dedup in action
        assert_ne!(m1.version(), m2.version());
        assert_eq!(m1.blob_id(), m2.blob_id());
    }

    // ── P2P integration ──────────────────────────────────────────────

    /// Manifest provides the data needed to start a P2P download.
    #[test]
    fn p2p_download_metadata() {
        let info = [0x42; 20];
        let m = test_manifest()
            .with_info_hash(info)
            .with_web_seeds(vec!["https://mirror-a.example.com/file.icpkg".to_string()])
            .with_download_urls(vec![
                "https://mirror-a.example.com/file.icpkg".to_string(),
                "https://mirror-b.example.org/file.icpkg".to_string(),
            ]);

        // These are the fields p2p-distribute needs to start a download:
        // 1. info_hash — identifies the torrent
        assert!(m.info_hash().is_some());
        // 2. web_seeds — BEP 19 HTTP seeds for bootstrapping
        assert!(!m.web_seeds().is_empty());
        // 3. download_urls — multi-platform HTTP fallback
        assert!(m.download_urls().len() >= 2);
    }
}
