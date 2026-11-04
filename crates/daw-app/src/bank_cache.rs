//! The sample-bank PCM cache format (RFC-002 §5).
//!
//! A bank's decoded samples are stored as raw interleaved `i16` PCM with a
//! 64-byte little-endian header (`SPCM` magic), one file per unique sample,
//! under `<bank>/cache/`. A `manifest.json` alongside them records each file's
//! name, size, parameters and hashes so the loader can verify integrity before
//! trusting the cache (RFC-002 §6).
//!
//! Everything here runs on the control or loader threads — never on the audio
//! path. `audio-core` has no knowledge of this format; hashing and file I/O
//! live only in `daw-app`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The `.pcm` header size in bytes; sample data is 64-byte aligned right
/// after it.
pub const PCM_HEADER_LEN: usize = 64;

/// The `.pcm` magic, first four bytes of every cache file.
const PCM_MAGIC: &[u8; 4] = b"SPCM";

/// The current on-disk cache format version. A mismatch forces a rebuild.
pub const PCM_FORMAT_VERSION: u16 = 1;

/// The manifest format version (`manifest.json` `format_version`).
pub const MANIFEST_FORMAT_VERSION: u32 = 1;

/// The `.pcm` header, little-endian, 64 bytes (RFC-002 §5.1).
///
/// | offset | field |
/// |---|---|
/// | 0 | `SPCM` magic |
/// | 4 | `format_version` u16 |
/// | 6 | `channels` u16 |
/// | 8 | `sample_rate` u32 |
/// | 12 | `reserved` u32 |
/// | 16 | `frames` u64 |
/// | 24 | `src_flac_size` u64 |
/// | 32 | `src_flac_xxh3` u64 |
/// | 40 | `header_crc` u32 (CRC-32 of bytes 0..40) |
/// | 44 | padding to 64 |
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PcmHeader {
    /// Channel count (`1` or `2`).
    pub channels: u16,
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Frames per channel.
    pub frames: u64,
    /// Size of the source FLAC in bytes (0 if the source is not FLAC).
    pub src_flac_size: u64,
    /// xxh3-64 of the source file, for cross-checking against the manifest.
    pub src_flac_xxh3: u64,
}

impl PcmHeader {
    /// Serializes to the canonical 64-byte little-endian header.
    #[must_use]
    pub fn encode(&self) -> [u8; PCM_HEADER_LEN] {
        let mut out = [0u8; PCM_HEADER_LEN];
        out[0..4].copy_from_slice(PCM_MAGIC);
        out[4..6].copy_from_slice(&PCM_FORMAT_VERSION.to_le_bytes());
        out[6..8].copy_from_slice(&self.channels.to_le_bytes());
        out[8..12].copy_from_slice(&self.sample_rate.to_le_bytes());
        // reserved (12..16) stays zero.
        out[16..24].copy_from_slice(&self.frames.to_le_bytes());
        out[24..32].copy_from_slice(&self.src_flac_size.to_le_bytes());
        out[32..40].copy_from_slice(&self.src_flac_xxh3.to_le_bytes());
        let crc = crc32(&out[0..40]);
        out[40..44].copy_from_slice(&crc.to_le_bytes());
        out
    }

    /// Parses `bytes` and validates the magic, format version and header CRC.
    /// Returns `None` on any mismatch or a too-short buffer.
    #[must_use]
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let bytes = bytes.get(..PCM_HEADER_LEN)?;
        if &bytes[0..4] != PCM_MAGIC {
            return None;
        }
        if u16::from_le_bytes([bytes[4], bytes[5]]) != PCM_FORMAT_VERSION {
            return None;
        }
        let stored_crc = u32::from_le_bytes([bytes[40], bytes[41], bytes[42], bytes[43]]);
        if crc32(&bytes[0..40]) != stored_crc {
            return None;
        }
        Some(Self {
            channels: u16::from_le_bytes([bytes[6], bytes[7]]),
            sample_rate: u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
            frames: u64::from_le_bytes([
                bytes[16], bytes[17], bytes[18], bytes[19], bytes[20], bytes[21], bytes[22],
                bytes[23],
            ]),
            src_flac_size: u64::from_le_bytes([
                bytes[24], bytes[25], bytes[26], bytes[27], bytes[28], bytes[29], bytes[30],
                bytes[31],
            ]),
            src_flac_xxh3: u64::from_le_bytes([
                bytes[32], bytes[33], bytes[34], bytes[35], bytes[36], bytes[37], bytes[38],
                bytes[39],
            ]),
        })
    }

    /// The full on-disk size of a `.pcm` file for this header: header plus
    /// `frames × channels × 2` bytes of interleaved `i16`.
    #[must_use]
    pub fn file_size(&self) -> u64 {
        PCM_HEADER_LEN as u64
            + self
                .frames
                .saturating_mul(u64::from(self.channels))
                .saturating_mul(2)
    }
}

/// A single cache entry in [`Manifest`] (RFC-002 §5.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestEntry {
    /// Order in `unique` (as in `load_bank_with_hint_impl`); stable key is `src`.
    pub sample_id: u32,
    /// Source path relative to the bank root.
    pub src: String,
    /// Cache file name relative to `cache/`.
    pub cache: String,
    /// Size of the `.pcm` file in bytes (`64 + frames × channels × 2`).
    pub size: u64,
    /// xxh3-128 of the `.pcm` payload (header + data), as lowercase hex.
    pub xxh3_128: String,
    /// Optional blake3 of the `.pcm` payload, lowercase hex.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blake3: Option<String>,
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Channel count (`1` or `2`).
    pub channels: u16,
    /// Frames per channel.
    pub frames: u64,
    /// Size of the source file in bytes.
    pub src_size: u64,
    /// xxh3-64 of the source file, lowercase hex.
    pub src_xxh3: String,
    /// Source file modification time in nanoseconds since the Unix epoch.
    pub mtime_ns: u64,
}

/// The `cache/manifest.json` contents (RFC-002 §5.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// Must equal [`MANIFEST_FORMAT_VERSION`].
    pub format_version: u32,
    /// The bank id (e.g. `"osiris"`).
    pub bank_id: String,
    /// ISO-8601 creation timestamp.
    pub created: String,
    /// Sum of `entries[].size`.
    pub total_bytes: u64,
    /// One entry per cached sample, in `sample_id` order.
    pub entries: Vec<ManifestEntry>,
}

/// Reads and parses `cache/manifest.json`.
///
/// # Errors
///
/// A human-readable reason on I/O or parse failure.
pub fn read_manifest(path: &Path) -> Result<Manifest, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("parse {}: {e}", path.display()))
}

/// Writes `manifest` to `path` atomically: writes `path.tmp` then renames it
/// over `path`, so a reader never sees a half-written manifest.
///
/// # Errors
///
/// A human-readable reason on serialization or I/O failure.
pub fn write_manifest(path: &Path, manifest: &Manifest) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(manifest).map_err(|e| e.to_string())?;
    let dir = path
        .parent()
        .ok_or_else(|| format!("{} has no parent", path.display()))?;
    std::fs::create_dir_all(dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    let tmp = temp_path(path);
    std::fs::write(&tmp, bytes).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .map_err(|e| format!("rename {} -> {}: {e}", tmp.display(), path.display()))?;
    Ok(())
}

/// The temporary path used by [`write_manifest`] before the final rename.
#[must_use]
pub fn temp_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".tmp");
    path.with_file_name(name)
}

/// xxh3-128 of `bytes`, as lowercase hex (the routine cache hash).
#[must_use]
pub fn xxh3_128_hex(bytes: &[u8]) -> String {
    let hash = xxhash_rust::xxh3::xxh3_128(bytes);
    format!("{hash:032x}")
}

/// xxh3-64 of `bytes`, as lowercase hex (the source-file cross-check hash).
#[must_use]
pub fn xxh3_64_hex(bytes: &[u8]) -> String {
    let hash = xxhash_rust::xxh3::xxh3_64(bytes);
    format!("{hash:016x}")
}

/// blake3 of `bytes`, as lowercase hex (the optional strong hash).
#[must_use]
pub fn blake3_hex(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(bytes);
    hasher.finalize()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_round_trips() {
        let header = PcmHeader {
            channels: 2,
            sample_rate: 48_000,
            frames: 308_000,
            src_flac_size: 412_345,
            src_flac_xxh3: 0xdead_beef_cafe_f00d,
        };
        let encoded = header.encode();
        assert_eq!(encoded.len(), PCM_HEADER_LEN);
        assert_eq!(&encoded[0..4], b"SPCM");
        assert_eq!(PcmHeader::decode(&encoded), Some(header));
    }

    #[test]
    fn header_size_is_header_plus_data() {
        let header = PcmHeader {
            channels: 2,
            sample_rate: 44_100,
            frames: 100,
            src_flac_size: 0,
            src_flac_xxh3: 0,
        };
        assert_eq!(header.file_size(), 64 + 100 * 2 * 2);
    }

    #[test]
    fn header_rejects_bad_magic() {
        let mut encoded = PcmHeader {
            channels: 1,
            sample_rate: 48_000,
            frames: 1,
            src_flac_size: 0,
            src_flac_xxh3: 0,
        }
        .encode();
        encoded[0] = b'X';
        assert!(PcmHeader::decode(&encoded).is_none());
    }

    #[test]
    fn header_rejects_corrupt_crc() {
        let mut encoded = PcmHeader {
            channels: 1,
            sample_rate: 48_000,
            frames: 1,
            src_flac_size: 0,
            src_flac_xxh3: 0,
        }
        .encode();
        encoded[30] ^= 0xff; // flip a byte inside the CRC'd region
        assert!(PcmHeader::decode(&encoded).is_none());
    }

    #[test]
    fn header_rejects_wrong_version() {
        let mut encoded = PcmHeader {
            channels: 1,
            sample_rate: 48_000,
            frames: 1,
            src_flac_size: 0,
            src_flac_xxh3: 0,
        }
        .encode();
        encoded[4] = 2; // format_version != 1
        assert!(PcmHeader::decode(&encoded).is_none());
    }

    #[test]
    fn header_rejects_truncated() {
        let encoded = PcmHeader {
            channels: 1,
            sample_rate: 48_000,
            frames: 1,
            src_flac_size: 0,
            src_flac_xxh3: 0,
        }
        .encode();
        assert!(PcmHeader::decode(&encoded[..63]).is_none());
    }

    fn sample_manifest() -> Manifest {
        Manifest {
            format_version: MANIFEST_FORMAT_VERSION,
            bank_id: "osiris".to_owned(),
            created: "2026-01-01T00:00:00Z".to_owned(),
            total_bytes: 64 + 4,
            entries: vec![ManifestEntry {
                sample_id: 17,
                src: "Samples/C4_v3.flac".to_owned(),
                cache: "000017.pcm".to_owned(),
                size: 64 + 4,
                xxh3_128: "0".repeat(32),
                blake3: None,
                sample_rate: 48_000,
                channels: 2,
                frames: 1,
                src_size: 412_345,
                src_xxh3: "0".repeat(16),
                mtime_ns: 0,
            }],
        }
    }

    #[test]
    fn manifest_round_trips_through_json() {
        let manifest = sample_manifest();
        let json = serde_json::to_vec(&manifest).unwrap();
        let back: Manifest = serde_json::from_slice(&json).unwrap();
        assert_eq!(back, manifest);
    }

    #[test]
    fn manifest_writes_atomically() {
        let dir = std::env::temp_dir().join(format!("bank-cache-manifest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("cache").join("manifest.json");
        write_manifest(&path, &sample_manifest()).unwrap();
        assert_eq!(read_manifest(&path).unwrap(), sample_manifest());
        // No `.tmp` leftover after a successful write.
        assert!(!temp_path(&path).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn manifest_read_errors_on_garbage() {
        let dir = std::env::temp_dir().join(format!("bank-cache-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("manifest.json");
        std::fs::write(&path, b"not json").unwrap();
        assert!(read_manifest(&path).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn xxh3_and_blake3_hex_are_stable_and_distinct() {
        let a = xxh3_128_hex(b"hello");
        let b = xxh3_128_hex(b"hello");
        assert_eq!(a, b);
        assert_eq!(a.len(), 32);
        assert_ne!(a, xxh3_128_hex(b"hellp"));
        let bl = blake3_hex(b"hello");
        assert_eq!(bl.len(), 64);
        assert_eq!(xxh3_64_hex(b"hello").len(), 16);
    }
}
