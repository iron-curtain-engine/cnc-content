// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! InstallShield CAB archive reader — extracts files from `.hdr` + `.cab` pairs.
//!
//! Supports InstallShield versions 5 and 6 (used by C&C: The First Decade and
//! similar era installers). Compressed entries use zlib/deflate.
//!
//! ## Format overview
//!
//! An InstallShield CAB archive consists of:
//! - A **header file** (`.hdr`) containing the file catalog, directory table,
//!   and metadata offsets.
//! - One or more **cabinet volumes** (`.cab`) containing the actual file data,
//!   optionally zlib-compressed.
//!
//! The header starts with signature `ISc(` (0x28635349). A cab descriptor at a
//! known offset within the header points to the file and directory tables.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use thiserror::Error;

/// Magic signature bytes: "ISc(" in little-endian.
const SIGNATURE: u32 = 0x2863_5349;

/// Flag bit indicating a file entry is zlib-compressed.
const FLAG_COMPRESSED: u16 = 0x04;

/// File descriptor size for version 5 archives.
const FD_SIZE_V5: usize = 0x33;

/// File descriptor size for version 6+ archives.
const FD_SIZE_V6: usize = 0x39;

/// Errors from InstallShield CAB operations.
#[derive(Debug, Error)]
pub enum IscabError {
    #[error("I/O error reading InstallShield archive: {source}")]
    Io {
        #[from]
        source: std::io::Error,
    },

    #[error(
        "invalid InstallShield signature: expected 0x{:08x}, got 0x{actual:08x}",
        SIGNATURE
    )]
    BadSignature { actual: u32 },

    #[error("unsupported InstallShield major version {major} (supported: 5, 6)")]
    UnsupportedVersion { major: u32 },

    #[error("file not found in InstallShield archive: {name}")]
    FileNotFound { name: String },

    #[error("zlib decompression failed: {detail}")]
    Decompress { detail: String },

    #[error("cabinet volume {volume} not provided")]
    MissingVolume { volume: u32 },

    #[error("corrupt archive: {detail}")]
    Corrupt { detail: String },
}

/// A parsed file entry from the InstallShield header.
#[derive(Debug, Clone)]
struct FileEntry {
    /// Full path: directory + "/" + filename (lowercase for matching).
    full_path: String,
    /// Original filename (as stored in the archive).
    name: String,
    /// Flags (bit 2 = compressed).
    flags: u16,
    /// Uncompressed file size.
    expanded_size: u64,
    /// Compressed size in the cabinet volume (equals expanded_size if uncompressed).
    compressed_size: u64,
    /// Byte offset into the cabinet volume where this file's data starts.
    data_offset: u64,
    /// 1-based cabinet volume index (1 = data1.cab, 2 = data2.cab, etc.).
    volume: u32,
}

/// An opened InstallShield CAB archive, ready for file extraction.
pub struct IscabArchive {
    entries: Vec<FileEntry>,
}

impl IscabArchive {
    /// Opens an InstallShield archive by parsing its header file.
    ///
    /// The header file (e.g. `data1.hdr`) contains the complete file catalog.
    /// Cabinet volume files are only needed when extracting specific entries.
    pub fn open(header_path: &Path) -> Result<Self, IscabError> {
        let data = std::fs::read(header_path)?;
        if data.len() < 20 {
            return Err(IscabError::Corrupt {
                detail: "header file too small (< 20 bytes)".into(),
            });
        }

        // ── Parse main header ────────────────────────────────────────
        let signature = read_u32(&data, 0)?;
        if signature != SIGNATURE {
            return Err(IscabError::BadSignature { actual: signature });
        }

        let version = read_u32(&data, 4)?;
        let major = (version >> 12) & 0xF;
        if major != 5 && major != 6 {
            return Err(IscabError::UnsupportedVersion { major });
        }

        let cab_desc_offset = read_u32(&data, 0x0C)? as usize;
        let fd_size = if major < 6 { FD_SIZE_V5 } else { FD_SIZE_V6 };

        // ── Parse cab descriptor ─────────────────────────────────────
        if cab_desc_offset + 0x24 > data.len() {
            return Err(IscabError::Corrupt {
                detail: "cab descriptor offset out of bounds".into(),
            });
        }

        let file_table_offset =
            cab_desc_offset.saturating_add(read_u32(&data, cab_desc_offset)? as usize);
        let directory_count = read_u32(&data, cab_desc_offset + 0x0C)? as usize;
        let file_count = read_u32(&data, cab_desc_offset + 0x1C)? as usize;
        let file_table_offset2 =
            cab_desc_offset.saturating_add(read_u32(&data, cab_desc_offset + 0x20)? as usize);

        // ── Parse directory names ────────────────────────────────────
        // Directory entries are u32 offsets (relative to file_table_offset)
        // pointing to NUL-terminated strings.
        let mut directories = Vec::with_capacity(directory_count);
        for i in 0..directory_count {
            // Overflow-safe: i and 4 are both controlled, but cab-supplied
            // directory_count could be large, so saturate instead of wrapping.
            let ptr_offset = file_table_offset.saturating_add(i.saturating_mul(4));
            if ptr_offset + 4 > data.len() {
                break;
            }
            let name_offset = file_table_offset + read_u32(&data, ptr_offset)? as usize;
            let name = read_cstring(&data, name_offset);
            directories.push(name);
        }

        // ── Parse file descriptors ───────────────────────────────────
        let mut entries = Vec::with_capacity(file_count);
        for i in 0..file_count {
            // Overflow-safe: both i and fd_size come from untrusted data.
            let base = file_table_offset2.saturating_add(i.saturating_mul(fd_size));
            if base + fd_size > data.len() {
                break;
            }

            let name_offset = file_table_offset + read_u32(&data, base)? as usize;
            let dir_index = read_u32(&data, base + 0x04)? as usize;
            let flags = read_u16(&data, base + 0x08)?;

            let (expanded_size, compressed_size, data_offset, volume_offset) = if major < 6 {
                (
                    read_u32(&data, base + 0x0A)? as u64,
                    read_u32(&data, base + 0x0E)? as u64,
                    read_u32(&data, base + 0x26)? as u64,
                    base + 0x2E,
                )
            } else {
                (
                    read_u64(&data, base + 0x0A)?,
                    read_u64(&data, base + 0x12)?,
                    read_u32(&data, base + 0x2E)? as u64,
                    base + 0x36,
                )
            };

            let volume = if volume_offset + 2 <= data.len() {
                read_u16(&data, volume_offset)? as u32
            } else {
                1
            };
            // Volume is often 0-based in the descriptor; normalize to 1-based.
            // A value of 0 means "first volume".
            let volume = if volume == 0 { 1 } else { volume };

            let name = read_cstring(&data, name_offset);
            let dir = directories.get(dir_index).map(|s| s.as_str()).unwrap_or("");

            let full_path = if dir.is_empty() {
                name.clone()
            } else {
                format!("{dir}/{name}")
            };

            entries.push(FileEntry {
                full_path,
                name,
                flags,
                expanded_size,
                compressed_size,
                data_offset,
                volume,
            });
        }

        Ok(Self { entries })
    }

    /// Lists all file entries in the archive.
    pub fn file_names(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|e| e.full_path.as_str())
    }

    /// Extracts a single file from the archive by name.
    ///
    /// `name` is matched case-insensitively against both the full path
    /// (directory/filename) and the bare filename. `volumes` maps 1-based
    /// volume indices to filesystem paths (e.g. `[(1, "data1.cab"), ...]`).
    pub fn extract(&self, name: &str, volumes: &[(u32, &Path)]) -> Result<Vec<u8>, IscabError> {
        let name_lower = name.to_ascii_lowercase();

        let entry = self
            .entries
            .iter()
            .find(|e| {
                e.full_path.to_ascii_lowercase() == name_lower
                    || e.name.to_ascii_lowercase() == name_lower
            })
            .ok_or_else(|| IscabError::FileNotFound {
                name: name.to_string(),
            })?;

        let vol_path = volumes
            .iter()
            .find(|(idx, _)| *idx == entry.volume)
            .map(|(_, p)| *p)
            .ok_or(IscabError::MissingVolume {
                volume: entry.volume,
            })?;

        let mut file = std::fs::File::open(vol_path)?;
        file.seek(SeekFrom::Start(entry.data_offset))?;

        let read_size = if entry.flags & FLAG_COMPRESSED != 0 {
            entry.compressed_size
        } else {
            entry.expanded_size
        };

        let mut raw = vec![0u8; read_size as usize];
        file.read_exact(&mut raw)?;

        if entry.flags & FLAG_COMPRESSED != 0 {
            decompress_zlib(&raw, entry.expanded_size as usize)
        } else {
            Ok(raw)
        }
    }
}

/// Decompresses zlib-compressed data to the expected uncompressed size.
fn decompress_zlib(data: &[u8], expected_size: usize) -> Result<Vec<u8>, IscabError> {
    use flate2::read::ZlibDecoder;

    let mut decoder = ZlibDecoder::new(data);
    let mut out = Vec::with_capacity(expected_size);
    decoder
        .read_to_end(&mut out)
        .map_err(|e| IscabError::Decompress {
            detail: e.to_string(),
        })?;
    Ok(out)
}

/// Reads a little-endian u32 at the given byte offset.
fn read_u32(data: &[u8], offset: usize) -> Result<u32, IscabError> {
    let bytes: [u8; 4] = data
        .get(offset..offset.saturating_add(4))
        .and_then(|s| s.try_into().ok())
        .ok_or_else(|| IscabError::Corrupt {
            detail: format!("u32 read at offset {offset} out of bounds"),
        })?;
    Ok(u32::from_le_bytes(bytes))
}

/// Reads a little-endian u16 at the given byte offset.
fn read_u16(data: &[u8], offset: usize) -> Result<u16, IscabError> {
    let bytes: [u8; 2] = data
        .get(offset..offset.saturating_add(2))
        .and_then(|s| s.try_into().ok())
        .ok_or_else(|| IscabError::Corrupt {
            detail: format!("u16 read at offset {offset} out of bounds"),
        })?;
    Ok(u16::from_le_bytes(bytes))
}

/// Reads a little-endian u64 at the given byte offset.
fn read_u64(data: &[u8], offset: usize) -> Result<u64, IscabError> {
    let bytes: [u8; 8] = data
        .get(offset..offset.saturating_add(8))
        .and_then(|s| s.try_into().ok())
        .ok_or_else(|| IscabError::Corrupt {
            detail: format!("u64 read at offset {offset} out of bounds"),
        })?;
    Ok(u64::from_le_bytes(bytes))
}

/// Reads a NUL-terminated C string starting at the given offset.
fn read_cstring(data: &[u8], offset: usize) -> String {
    let tail = data.get(offset..).unwrap_or(&[]);
    let len = tail.iter().position(|&b| b == 0).unwrap_or(tail.len());
    String::from_utf8_lossy(tail.get(..len).unwrap_or(&[])).into_owned()
}

#[cfg(test)]
mod tests;
