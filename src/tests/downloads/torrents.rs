// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! Embedded torrent tests.

use super::*;

// ── Embedded torrent tests ─────────────────────────────────────────────

/// Verifies that all 8 generated packages have embedded `.torrent` data.
///
/// These are the packages whose mirrors are currently live. If a new torrent is
/// generated but the `include_bytes!` entry is missing, this test catches it.
#[test]
fn embedded_torrent_present_for_generated_packages() {
    let ids_with_torrent = [
        DownloadId::RaQuickInstall,
        DownloadId::RaBaseFiles,
        DownloadId::RaAftermath,
        DownloadId::RaCncDesert,
        DownloadId::TdBaseFiles,
        DownloadId::TdCovertOps,
        DownloadId::TdGdiIso,
        DownloadId::TdNodIso,
    ];
    for id in ids_with_torrent {
        assert!(
            embedded_torrent(id).is_some(),
            "{id:?} should have an embedded .torrent file",
        );
    }
}

/// Verifies that packages without generated torrents return `None`.
///
/// Archive.org packages use Archive.org's own torrents, and packages without
/// live mirror infrastructure don't have torrents yet. These must return `None`
/// so callers don't attempt to use stale or non-existent torrent data.
#[test]
fn embedded_torrent_none_for_unavailable_packages() {
    let ids_without_torrent = [
        DownloadId::RaFullDiscs,
        DownloadId::RaFullSet,
        DownloadId::RaMusic,
        DownloadId::RaMoviesAllied,
        DownloadId::RaMoviesSoviet,
        DownloadId::RaMusicCounterstrike,
        DownloadId::RaMusicAftermath,
        DownloadId::TdMusic,
        DownloadId::TdMoviesGdi,
        DownloadId::TdMoviesNod,
        DownloadId::TsBaseFiles,
        DownloadId::TsQuickInstall,
        DownloadId::TsExpand,
        DownloadId::TsGdiIso,
        DownloadId::TsNodIso,
        DownloadId::TsFirestormIso,
        DownloadId::TsMusic,
        DownloadId::TsMovies,
    ];
    for id in ids_without_torrent {
        assert!(
            embedded_torrent(id).is_none(),
            "{id:?} should NOT have an embedded .torrent file",
        );
    }
}

/// Verifies that embedded `.torrent` files start with a valid bencoded dictionary.
///
/// All `.torrent` files are bencoded dictionaries that must start with `d` (the
/// bencode dictionary marker). A corrupted or truncated file would start with a
/// different byte, catching file-copy or include_bytes! path errors.
#[test]
fn embedded_torrent_is_valid_bencode() {
    let ids = [
        DownloadId::RaQuickInstall,
        DownloadId::RaBaseFiles,
        DownloadId::RaAftermath,
        DownloadId::RaCncDesert,
        DownloadId::TdBaseFiles,
        DownloadId::TdCovertOps,
        DownloadId::TdGdiIso,
        DownloadId::TdNodIso,
    ];
    for id in ids {
        let data = embedded_torrent(id).unwrap();
        // Bencoded dictionaries start with 'd' (0x64) and end with 'e' (0x65).
        assert!(
            data.first() == Some(&b'd'),
            "{id:?} torrent should start with bencoded dict marker 'd', got {:?}",
            data.first(),
        );
        assert!(
            data.last() == Some(&b'e'),
            "{id:?} torrent should end with bencoded end marker 'e', got {:?}",
            data.last(),
        );
        // All embedded torrents should be at least 100 bytes (metadata + piece hashes).
        assert!(
            data.len() >= 100,
            "{id:?} torrent is suspiciously small: {} bytes",
            data.len(),
        );
    }
}

/// Verifies that embedded torrents with info_hash match the download definition.
///
/// The info_hash stored in `downloads.toml` must correspond to the embedded
/// `.torrent` file. This test computes the info_hash from the torrent data
/// and compares it with the declared value, catching stale or mismatched files.
#[test]
fn embedded_torrent_info_hash_matches_download_definition() {
    use sha1::{Digest, Sha1};

    let ids = [
        DownloadId::RaQuickInstall,
        DownloadId::RaBaseFiles,
        DownloadId::RaAftermath,
        DownloadId::RaCncDesert,
        DownloadId::TdBaseFiles,
        DownloadId::TdCovertOps,
        DownloadId::TdGdiIso,
        DownloadId::TdNodIso,
    ];
    for id in ids {
        let dl = download(id).unwrap();
        let declared_hash = match &dl.info_hash {
            Some(hash) => hash,
            None => continue,
        };
        let torrent_data = embedded_torrent(id).unwrap();

        // Extract the info dictionary from the bencoded torrent.
        // The info dict is the value associated with the "4:info" key.
        let info_key = b"4:info";
        let info_start = torrent_data
            .windows(info_key.len())
            .position(|w| w == info_key)
            .expect("torrent must contain '4:info' key");
        let info_dict_start = info_start + info_key.len();

        // Parse the bencoded info dict to find its extent.
        let info_dict_end = find_bencode_end(torrent_data, info_dict_start)
            .expect("info dict must be valid bencode");
        let info_bytes = torrent_data
            .get(info_dict_start..info_dict_end)
            .expect("info dict range must be valid");

        // SHA-1 hash of the info dict is the info_hash.
        let mut hasher = Sha1::new();
        hasher.update(info_bytes);
        let hash_bytes = hasher.finalize();
        let computed_hash: String = hash_bytes.iter().map(|b| format!("{b:02x}")).collect();

        assert_eq!(
            computed_hash, *declared_hash,
            "{id:?} embedded torrent info_hash mismatch: computed={computed_hash}, declared={declared_hash}",
        );
    }
}

/// Finds the end index (exclusive) of a bencoded value starting at `pos`.
///
/// Supports enough bencode to parse the info dictionary extent:
/// integers (`i...e`), strings (`N:...`), lists (`l...e`), dicts (`d...e`).
fn find_bencode_end(data: &[u8], pos: usize) -> Option<usize> {
    let first = *data.get(pos)?;
    match first {
        // Dictionary or list: scan elements until 'e'.
        b'd' | b'l' => {
            let mut cursor = pos + 1;
            loop {
                if data.get(cursor) == Some(&b'e') {
                    return Some(cursor + 1);
                }
                if first == b'd' {
                    // Dict has key-value pairs; key is always a string.
                    cursor = find_bencode_end(data, cursor)?;
                }
                cursor = find_bencode_end(data, cursor)?;
            }
        }
        // Integer: i<digits>e
        b'i' => {
            let end = data.get(pos..)?.iter().position(|&b| b == b'e')?;
            Some(pos + end + 1)
        }
        // String: <length>:<bytes>
        b'0'..=b'9' => {
            let colon = data.get(pos..)?.iter().position(|&b| b == b':')?;
            let len_str = std::str::from_utf8(data.get(pos..pos + colon)?).ok()?;
            let len: usize = len_str.parse().ok()?;
            Some(pos + colon + 1 + len)
        }
        _ => None,
    }
}
