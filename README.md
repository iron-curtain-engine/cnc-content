# cnc-content

<p align="center">
  <a href="https://github.com/iron-curtain-engine/cnc-content/actions/workflows/ci.yml"><img src="https://github.com/iron-curtain-engine/cnc-content/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/iron-curtain-engine/cnc-content/actions/workflows/audit.yml"><img src="https://github.com/iron-curtain-engine/cnc-content/actions/workflows/audit.yml/badge.svg" alt="Security Audit"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-GPL--3.0--or--later-blue.svg" alt="License"></a>
</p>

<p align="center">
  <a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/rust-1.85%2B-orange.svg" alt="Rust"></a>
</p>

Download, verify, and manage Command & Conquer game content from any supported
source. Works as a standalone CLI tool or as a library for engine integration.

## What it does

- **Defines** what RA1 content the game needs (packages, sources, downloads)
- **Identifies** content sources on disk (discs, Steam, GOG, Origin installs)
- **Downloads** content from OpenRA mirrors (with SHA-1 verification)
- **Extracts** content from MIX archives, InstallShield CABs, ZIPs, raw offsets
- **Verifies** installed content integrity (SHA-256 manifests)

## Status

> **Alpha / pre-1.0** — core download and verification pipeline is functional.
> Source probes and P2P distribution are planned for future phases.

## Supported Content Sources

| Source | Type | Status |
|--------|------|--------|
| OpenRA HTTP mirrors | Download | Working |
| Allied Disc | Disc | Defined |
| Soviet Disc | Disc | Defined |
| Aftermath Disc | Disc | Defined |
| Counterstrike Disc | Disc | Defined |
| The First Decade | InstallShield | Defined |
| Steam TUC (AppId 2229840) | Steam | Defined |
| Steam C&C (AppId 2229830) | Steam | Defined |
| Steam Remastered (AppId 1213210) | Steam | Defined |
| Origin TUC / C&C / Remastered | Origin | Defined |
| C&C 1995 | Registry | Defined |

## Content Packages

| Package | Required | Description |
|---------|----------|-------------|
| Base | Yes | Core RA1 data (allies.mix, conquer.mix, etc.) |
| Aftermath Base | Yes | Expansion files (expand2.mix, loose AUDs) |
| C&C Desert | Yes | Desert tileset from C&C (cnc/desert.mix) |
| Music | No | Score music (scores.mix) |
| Movies Allied | No | Allied campaign FMV cutscenes |
| Movies Soviet | No | Soviet campaign FMV cutscenes |
| Music Counterstrike | No | Counterstrike expansion music |
| Music Aftermath | No | Aftermath expansion music |

## CLI

Build with the `cli` feature (default) for the `cnc-content` command:

```sh
cnc-content status              # show installed/missing packages
cnc-content download            # download all required content
cnc-content download --package aftermath   # download a specific package
cnc-content verify              # check installed content integrity (SHA-256)
cnc-content identify <path>     # identify a content source at a path
```

### Options

```
--content-dir <path>   Content directory override
                       (default: ~/.iron-curtain/content/ra/v1/)
                       (env: IC_CONTENT_DIR)
```

## Library Usage

```rust,no_run
use cnc_content::{packages, sources, downloads, verify};

// Check if content is complete
let root = std::path::Path::new("~/.iron-curtain/content/ra/v1");
if !cnc_content::is_content_complete(root) {
    let missing = cnc_content::missing_required_packages(root);
    for pkg in missing {
        eprintln!("missing: {}", pkg.title);
    }
}
```

## Design

This crate is part of the [Iron Curtain](https://github.com/iron-curtain-engine/iron-curtain)
engine ecosystem but works standalone without any game engine dependency.

It depends on [`cnc-formats`](https://github.com/iron-curtain-engine/cnc-formats)
(MIT/Apache-2.0) for binary format parsing (MIX archives). This crate itself
is GPL-3.0-or-later because it implements game-specific content management
logic that may reference EA-derived knowledge (file layouts, content
definitions from OpenRA's GPL-licensed data).

### Key properties

- **No Bevy dependency** — pure Rust library, usable by any project
- **Offline-first** — content is cached locally after first download
- **OpenRA-compatible** — uses the same mirror infrastructure and file layout
- **Feature-gated networking** — `download` feature pulls in `ureq` + `zip`;
  disable it for library-only use without HTTP dependencies

## Features

| Feature | Default | Description |
|---------|---------|-------------|
| `cli` | Yes | `cnc-content` binary (implies `download`) |
| `download` | Yes | HTTP download + ZIP extraction (`ureq`, `zip`) |

## Design Documents

Architecture and format specifications are maintained in the
[Iron Curtain Design Documentation](https://github.com/iron-curtain-engine/iron-curtain-design-docs).

Key references:
- [D076 — Standalone crate extraction](https://iron-curtain-engine.github.io/iron-curtain-design-docs/decisions/09a/D076-standalone-crates.html)

## License

Licensed under the GNU General Public License v3.0 or later — see [LICENSE](LICENSE).

## Contributing

Contributions require a Developer Certificate of Origin (DCO) — add `Signed-off-by`
to your commit messages (`git commit -s`).

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this crate by you shall be licensed under GPL-3.0-or-later,
without any additional terms or conditions.
