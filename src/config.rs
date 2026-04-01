// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! Persistent configuration — seeding policy and runtime settings.
//!
//! Stored as TOML at the platform-appropriate config directory:
//! - Windows: `%APPDATA%/cnc-content/config.toml`
//! - Linux:   `~/.config/cnc-content/config.toml`
//! - macOS:   `~/Library/Application Support/cnc-content/config.toml`
//!
//! Falls back to `CNC_CONTENT_ROOT/config.toml` if set, or `./config.toml`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::SeedingPolicy;

/// Persistent user configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// How to handle seeding after download.
    #[serde(default)]
    pub seeding_policy: SeedingPolicy,
    /// Maximum upload speed in bytes/sec (0 = unlimited).
    #[serde(default = "default_max_upload")]
    pub max_upload_speed: u64,
    /// Maximum download speed in bytes/sec (0 = unlimited).
    #[serde(default)]
    pub max_download_speed: u64,
}

fn default_max_upload() -> u64 {
    1_048_576 // 1 MB/s
}

impl Default for Config {
    fn default() -> Self {
        Self {
            seeding_policy: SeedingPolicy::default(),
            max_upload_speed: default_max_upload(),
            max_download_speed: 0,
        }
    }
}

impl Config {
    /// Loads config from the default path, returning `Config::default()` if
    /// the file doesn't exist or can't be parsed.
    pub fn load() -> Self {
        let path = config_path();
        Self::load_from(&path).unwrap_or_default()
    }

    /// Loads config from a specific path.
    pub fn load_from(path: &Path) -> Option<Self> {
        let contents = std::fs::read_to_string(path).ok()?;
        toml::from_str(&contents).ok()
    }

    /// Saves config to the default path.
    pub fn save(&self) -> Result<(), std::io::Error> {
        let path = config_path();
        self.save_to(&path)
    }

    /// Saves config to a specific path, creating parent directories as needed.
    pub fn save_to(&self, path: &Path) -> Result<(), std::io::Error> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let toml_str = toml::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
        std::fs::write(path, toml_str)
    }
}

/// Returns the default config file path.
pub fn config_path() -> PathBuf {
    if let Ok(dir) = std::env::var("CNC_CONTENT_ROOT") {
        return PathBuf::from(dir).join("config.toml");
    }

    app_path::try_app_path!("config.toml")
        .map(|p| p.into_path_buf())
        .unwrap_or_else(|_| PathBuf::from("config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Round-trip serialisation ────────────────────────────────────

    /// Serialising a default `Config` to TOML and deserialising it back preserves all fields.
    ///
    /// A config that has never been customised must survive a save/load cycle
    /// unchanged — this guards against missing `#[serde(default)]` annotations
    /// that would silently drop unset fields.
    #[test]
    fn config_default_roundtrip() {
        let config = Config::default();
        let toml_str = toml::to_string_pretty(&config).unwrap();
        let loaded: Config = toml::from_str(&toml_str).unwrap();
        assert_eq!(loaded.seeding_policy, config.seeding_policy);
        assert_eq!(loaded.max_upload_speed, config.max_upload_speed);
        assert_eq!(loaded.max_download_speed, config.max_download_speed);
    }

    /// Saving a customised config to a file and loading it back restores the same values.
    ///
    /// `save_to` / `load_from` are the primary persistence API and must produce
    /// a consistent round-trip for non-default field values such as a custom
    /// seeding policy and upload speed.
    #[test]
    fn config_save_and_load() {
        let tmp = std::env::temp_dir().join("cnc-config-test");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let path = tmp.join("config.toml");
        let config = Config {
            seeding_policy: SeedingPolicy::SeedAlways,
            max_upload_speed: 512_000,
            ..Config::default()
        };

        config.save_to(&path).unwrap();
        let loaded = Config::load_from(&path).unwrap();
        assert_eq!(loaded.seeding_policy, SeedingPolicy::SeedAlways);
        assert_eq!(loaded.max_upload_speed, 512_000);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // ── Load error paths ───────────────────────────────────────

    /// Loading from a path that does not exist returns `None`.
    ///
    /// Callers distinguish between "no file yet" (use defaults) and a parse
    /// error via the `Option` return type, so a missing file must not propagate
    /// as an error.
    #[test]
    fn config_load_missing_file_returns_none() {
        assert!(Config::load_from(Path::new("/nonexistent/config.toml")).is_none());
    }

    /// A TOML file that omits fields fills them in from `#[serde(default)]` values.
    ///
    /// Users may hand-edit their config and omit optional fields. Serde's
    /// default attribute must supply the canonical defaults so the loaded struct
    /// is fully initialised.
    #[test]
    fn config_load_partial_toml_uses_defaults() {
        let tmp = std::env::temp_dir().join("cnc-config-partial");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let path = tmp.join("config.toml");
        std::fs::write(&path, "seeding_policy = \"SeedAlways\"\n").unwrap();

        let loaded = Config::load_from(&path).unwrap();
        assert_eq!(loaded.seeding_policy, SeedingPolicy::SeedAlways);
        assert_eq!(loaded.max_upload_speed, default_max_upload());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A file containing syntactically invalid TOML returns `None`.
    ///
    /// Parse failures must be silently swallowed by `load_from` and converted
    /// to `None` so callers can fall back to defaults without an unwrap site.
    #[test]
    fn config_load_invalid_toml_returns_none() {
        let tmp = std::env::temp_dir().join("cnc-config-invalid");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let path = tmp.join("config.toml");
        std::fs::write(&path, "{{{{ not valid toml !@#$").unwrap();

        assert!(Config::load_from(&path).is_none());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A TOML file where a field has the wrong type (string instead of u64) returns `None`.
    ///
    /// Type mismatches in the config file must not panic or silently coerce —
    /// they should be treated the same as a corrupt file and cause the whole
    /// load to fail gracefully.
    #[test]
    fn config_load_wrong_type_returns_none() {
        let tmp = std::env::temp_dir().join("cnc-config-wrong-type");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        // max_upload_speed is u64, but we give it a string.
        let path = tmp.join("config.toml");
        std::fs::write(&path, "max_upload_speed = \"not a number\"\n").unwrap();

        assert!(Config::load_from(&path).is_none());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// An unrecognised `seeding_policy` variant name causes `load_from` to return `None`.
    ///
    /// The `SeedingPolicy` enum is closed — unknown variant strings must not
    /// deserialise to an arbitrary default and must instead signal a failure so
    /// the user knows their config contains an invalid value.
    #[test]
    fn config_load_unknown_seeding_policy_returns_none() {
        let tmp = std::env::temp_dir().join("cnc-config-unknown-policy");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let path = tmp.join("config.toml");
        std::fs::write(&path, "seeding_policy = \"NonexistentPolicy\"\n").unwrap();

        assert!(Config::load_from(&path).is_none());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// An empty TOML file deserialises to a fully-defaulted `Config`.
    ///
    /// An empty file is valid TOML (no keys, no syntax error). All `#[serde(default)]`
    /// annotations must fire and produce the same struct as `Config::default()`.
    #[test]
    fn config_empty_file_uses_defaults() {
        let tmp = std::env::temp_dir().join("cnc-config-empty");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let path = tmp.join("config.toml");
        std::fs::write(&path, "").unwrap();

        let loaded = Config::load_from(&path).unwrap();
        assert_eq!(loaded.seeding_policy, SeedingPolicy::default());
        assert_eq!(loaded.max_upload_speed, default_max_upload());
        assert_eq!(loaded.max_download_speed, 0);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // ── Boundary values ─────────────────────────────────────────────────────

    /// Zero values for both speed limits round-trip through TOML correctly.
    ///
    /// Zero is the sentinel for "unlimited" and must not be treated as absent
    /// or replaced by the default value of 1 MB/s during serialisation.
    #[test]
    fn config_zero_speeds_are_valid() {
        let config = Config {
            max_upload_speed: 0,
            max_download_speed: 0,
            ..Config::default()
        };
        let toml_str = toml::to_string_pretty(&config).unwrap();
        let loaded: Config = toml::from_str(&toml_str).unwrap();
        assert_eq!(loaded.max_upload_speed, 0);
        assert_eq!(loaded.max_download_speed, 0);
    }

    /// Very large speed values up to `i64::MAX` round-trip through TOML without loss.
    ///
    /// TOML integers are signed 64-bit, so `u64` values above `i64::MAX` would
    /// overflow. The test uses `i64::MAX` as the boundary to confirm that the
    /// maximum safe value survives serialisation and deserialisation intact.
    #[test]
    fn config_large_speeds_roundtrip() {
        // TOML integers are i64, so we test the max safe value.
        let max_safe = i64::MAX as u64;
        let config = Config {
            max_upload_speed: max_safe,
            max_download_speed: max_safe,
            ..Config::default()
        };
        let toml_str = toml::to_string_pretty(&config).unwrap();
        let loaded: Config = toml::from_str(&toml_str).unwrap();
        assert_eq!(loaded.max_upload_speed, max_safe);
        assert_eq!(loaded.max_download_speed, max_safe);
    }
}
