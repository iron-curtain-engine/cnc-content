# cnc-content

<p align="center">
  <img src="images/logo.png" alt="Iron Curtain logo" width="280">
</p>

<p align="center">
  <a href="https://github.com/iron-curtain-engine/cnc-content/actions/workflows/ci.yml"><img src="https://github.com/iron-curtain-engine/cnc-content/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/iron-curtain-engine/cnc-content/actions/workflows/audit.yml"><img src="https://github.com/iron-curtain-engine/cnc-content/actions/workflows/audit.yml/badge.svg" alt="Security Audit"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-GPL--3.0--or--later-blue.svg" alt="License"></a>
</p>

<p align="center">
  <a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/rust-1.89%2B-orange.svg" alt="Rust"></a>
  &nbsp;&nbsp;
  <img src="https://img.shields.io/badge/LM-ready-blueviolet.svg" alt="LM Ready"><br>
  <img src="images/rust_inside.png" alt="Rust-based project" width="74">
  &nbsp;
  <img src="images/lm_ready.png" alt="LM Ready" width="74">
</p>

Download, verify, and manage Command & Conquer game content from any supported
source. Works as a standalone CLI tool or as a library for engine integration.

## What it does

- **Defines** what each game needs (packages, sources, downloads)
- **Identifies** content sources on disk (discs, Steam, GOG, Origin installs)
- **Downloads** freeware content via HTTP mirrors (with SHA-1 verification)
- **Extracts** content from MIX archives, InstallShield CABs, ZIPs, raw offsets
- **Verifies** installed content integrity (SHA-256 manifests)

## Status

> **Alpha / pre-1.0** — core download, verification, and source detection pipeline
> is functional. P2P distribution is planned for future phases.

## Supported Games

| Game                               | Slug       | Status                             |
| ---------------------------------- | ---------- | ---------------------------------- |
| Command & Conquer: Red Alert       | `ra`       | Freeware (EA, 2008) — downloadable |
| Command & Conquer: Tiberian Dawn   | `td`       | Freeware (EA, 2007) — downloadable |
| Dune II: The Building of a Dynasty | `dune2`    | NOT freeware — local source only   |
| Dune 2000                          | `dune2000` | NOT freeware — local source only   |
| Command & Conquer: Tiberian Sun    | `ts`       | NOT freeware — local source only   |
| Command & Conquer: Red Alert 2     | `ra2`      | NOT freeware — local source only   |
| Command & Conquer: Generals        | `generals` | NOT freeware — local source only   |

## Content Sources

| Source                           | Type          | Games                   |
| -------------------------------- | ------------- | ----------------------- |
| OpenRA HTTP mirrors              | Download      | RA, TD                  |
| Archive.org / CNCNZ              | Download      | RA, TD                  |
| Allied / Soviet / CS / AM Discs  | Disc          | RA                      |
| GDI / Nod / Covert Ops Discs     | Disc          | TD                      |
| The First Decade DVD             | InstallShield | RA, RA2                 |
| Steam — The Ultimate Collection  | Steam         | RA, TD, TS*, RA2*, Gen* |
| Steam — C&C Remastered           | Steam         | RA, TD                  |
| Origin / EA App                  | Origin        | RA, TD, TS, RA2, Gen    |
| GOG.com                          | GOG           | Dune 2, Dune 2000       |
| C&C 1995 (registry)              | Registry      | RA                      |
| Dune 2 / Dune 2000 Discs         | Disc          | Dune 2, Dune 2000       |
| TS / RA2 / Generals Retail Discs | Disc          | TS, RA2, Gen            |

\* Steam app IDs for TS, RA2, and Generals TUC are pending confirmation.

## Installation

Install from source with Cargo:

```sh
cargo install --git https://github.com/iron-curtain-engine/cnc-content.git
```

Or build locally:

```sh
git clone https://github.com/iron-curtain-engine/cnc-content.git
cd cnc-content
cargo build --release
# Binary at target/release/cnc-content(.exe)
```

## CLI

Build with the `cli` feature (default) for the `cnc-content` command:

```sh
cnc-content status                    # show installed/missing packages
cnc-content download                  # download all required content
cnc-content download --all            # download required + optional (music, movies)
cnc-content download --package music  # download a specific package
cnc-content -g td download            # download Tiberian Dawn content
cnc-content verify                    # check installed content integrity
cnc-content detect                    # scan for local content sources
cnc-content install /path/to/source   # install from a local disc/Steam/GOG path
cnc-content identify <path>           # identify a content source at a path
cnc-content games                     # list supported games
cnc-content clean                     # remove all downloaded content
cnc-content seed-config               # show current P2P seeding policy
cnc-content seed-config always        # change seeding policy
cnc-content torrent-create            # generate .torrent files (maintainer tool)
```

### Global Options

```
-g, --game <SLUG>      Game to manage (ra, td, dune2, dune2000) [default: ra]
--content-dir <PATH>   Content directory override
                       (default: portable, next to executable)
                       (env: CNC_CONTENT_ROOT)
--openra               Use OpenRA's content directory (share files between engines)
```

### Download Options

```
--all                  Download optional content too (music, movies)
--package <NAME>       Download a specific package by name
--seed <POLICY>        Seeding policy (pause, always, keep, delete)
```

## Library Usage

```rust
use cnc_content::GameId;

// Check if Red Alert content is complete
let root = std::path::Path::new("/path/to/content/ra/v1");
if !cnc_content::is_content_complete(root, GameId::RedAlert) {
    let missing = cnc_content::missing_required_packages(root, GameId::RedAlert);
    for pkg in missing {
        eprintln!("missing: {}", pkg.title);
    }
}
```

## Features

| Feature       | Default | Description                                                  |
| ------------- | ------- | ------------------------------------------------------------ |
| `cli`         | Yes     | `cnc-content` binary with progress bars (implies `download`) |
| `download`    | Yes     | HTTP download + ZIP extraction (`ureq`, `zip`)               |
| `fast-verify` | Yes     | Parallel SHA-256 verification via `rayon` + SIMD bitfields   |
| `torrent`     | No      | BitTorrent P2P transport via `librqbit`                      |

## Environment Variables

| Variable               | Purpose                                        | Default                                   |
| ---------------------- | ---------------------------------------------- | ----------------------------------------- |
| `CNC_CONTENT_ROOT`     | Override the content directory                 | Portable path next to executable          |
| `CNC_MIRROR_LIST_URL`  | Override the mirror list URL for all downloads | Per-package URL from download definitions |
| `CNC_DOWNLOAD_TIMEOUT` | HTTP download timeout in seconds               | `300`                                     |
| `CNC_NO_P2P`           | Disable P2P transport (`1` = disabled)         | `0` (P2P enabled when compiled in)        |

CLI flags take precedence over environment variables.

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
- **Freeware-only downloads** — only EA-declared freeware (RA, TD) can be
  downloaded; all other games require user-owned copies

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
