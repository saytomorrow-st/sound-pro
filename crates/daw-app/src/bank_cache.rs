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

/// The name of the cache folder inside a bank directory.
pub const CACHE_DIR_NAME: &str = "cache";

/// The manifest file name inside [`CACHE_DIR_NAME`].
pub const MANIFEST_FILE_NAME: &str = "manifest.json";

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
    /// xxh3-64 of the first/middle/last 64 KiB blocks of the `.pcm` payload,
    /// lowercase hex, used by the L1 sampled integrity check. Optional so
    /// manifests written before this field existed still parse (L1 then only
    /// checks the source `mtime`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sampled_xxh3: Option<String>,
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

/// The block size used by the L1 sampled integrity check.
pub const SAMPLE_BLOCK_LEN: usize = 64 * 1024;

/// xxh3-64 of three representative 64 KiB blocks of a `.pcm` payload — the
/// first, the middle and the last — as lowercase hex. This is the reference
/// value stored in [`ManifestEntry::sampled_xxh3`] and recomputed by the L1
/// check. Short payloads fall back to hashing the whole buffer once.
#[must_use]
pub fn sample_blocks_xxh3_64(bytes: &[u8]) -> String {
    use xxhash_rust::xxh3::Xxh3;
    if bytes.len() <= SAMPLE_BLOCK_LEN {
        return xxh3_64_hex(bytes);
    }
    let mut h = Xxh3::new();
    h.update(&bytes[..SAMPLE_BLOCK_LEN]);
    let mid = bytes.len() / 2;
    let mid_start = mid
        .saturating_sub(SAMPLE_BLOCK_LEN / 2)
        .min(bytes.len() - SAMPLE_BLOCK_LEN);
    h.update(&bytes[mid_start..mid_start + SAMPLE_BLOCK_LEN]);
    h.update(&bytes[bytes.len() - SAMPLE_BLOCK_LEN..]);
    format!("{:016x}", h.digest())
}

/// Recomputes the sampled-block xxh3-64 of a `.pcm` file on disk, streaming
/// only the three 64 KiB blocks (never the whole file), for the L1 check.
/// Returns the hash and the file's total length.
///
/// # Errors
///
/// A human-readable reason if the file cannot be opened or read.
pub fn sample_blocks_xxh3_64_file(path: &Path) -> Result<(String, u64), String> {
    use std::io::{Read, Seek, SeekFrom};
    use xxhash_rust::xxh3::Xxh3;

    let mut f = std::fs::File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let len = f
        .metadata()
        .map_err(|e| format!("stat {}: {e}", path.display()))?
        .len();
    let read_block = |f: &mut std::fs::File, offset: u64| -> Result<Vec<u8>, String> {
        f.seek(SeekFrom::Start(offset))
            .map_err(|e| format!("seek {}: {e}", path.display()))?;
        let mut buf = vec![0u8; SAMPLE_BLOCK_LEN];
        let mut filled = 0usize;
        while filled < buf.len() {
            let n = f
                .read(&mut buf[filled..])
                .map_err(|e| format!("read {}: {e}", path.display()))?;
            if n == 0 {
                break;
            }
            filled += n;
        }
        buf.truncate(filled);
        Ok(buf)
    };

    if len <= SAMPLE_BLOCK_LEN as u64 {
        let whole = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
        return Ok((xxh3_64_hex(&whole), len));
    }

    let mut h = Xxh3::new();
    h.update(&read_block(&mut f, 0)?);
    let mid_start = (len / 2)
        .saturating_sub(SAMPLE_BLOCK_LEN as u64 / 2)
        .min(len - SAMPLE_BLOCK_LEN as u64);
    h.update(&read_block(&mut f, mid_start)?);
    h.update(&read_block(&mut f, len - SAMPLE_BLOCK_LEN as u64)?);
    Ok((format!("{:016x}", h.digest()), len))
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

/// The cache folder for a bank: `<bank_dir>/cache`.
#[must_use]
pub fn cache_dir(bank_dir: &Path) -> PathBuf {
    bank_dir.join(CACHE_DIR_NAME)
}

/// The manifest path for a bank: `<bank_dir>/cache/manifest.json`.
#[must_use]
pub fn manifest_path(bank_dir: &Path) -> PathBuf {
    cache_dir(bank_dir).join(MANIFEST_FILE_NAME)
}

/// What [`write_pcm_file`] computed about the written sample.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrittenPcm {
    /// Full `.pcm` file size in bytes (`64 + frames × channels × 2`).
    pub size: u64,
    /// xxh3-128 of the `.pcm` payload (header + data), lowercase hex.
    pub xxh3_128: String,
    /// blake3 of the `.pcm` payload (header + data), lowercase hex, when
    /// requested.
    pub blake3: Option<String>,
    /// xxh3-64 of the first/middle/last 64 KiB blocks of the payload, lowercase
    /// hex, stored as [`ManifestEntry::sampled_xxh3`] for the L1 check.
    pub sampled_xxh3: String,
}

/// Serializes one decoded sample to `<cache_dir>/<name>` atomically (`.tmp`
/// then rename), computing its size and hashes over the exact bytes written.
///
/// `header` and `data` (interleaved `i16`, `frames × channels` samples) are
/// the only inputs; `src_size`/`src_xxh3`/`mtime_ns` are filled by the caller
/// from the source file. Hashing and file I/O run on the control/loader
/// threads, never the audio path.
///
/// # Errors
///
/// A human-readable reason on I/O failure.
pub fn write_pcm_file(
    cache_dir: &Path,
    name: &str,
    header: &PcmHeader,
    data: &[i16],
    compute_blake3: bool,
) -> Result<WrittenPcm, String> {
    std::fs::create_dir_all(cache_dir)
        .map_err(|e| format!("mkdir {}: {e}", cache_dir.display()))?;

    // Header + raw interleaved i16 payload, hashed as a single byte stream so
    // the manifest hash matches the file on disk bit-for-bit.
    let header_bytes = header.encode();
    let mut payload = Vec::with_capacity(PCM_HEADER_LEN + data.len() * 2);
    payload.extend_from_slice(&header_bytes);
    for s in data {
        payload.extend_from_slice(&s.to_le_bytes());
    }

    let xxh3_128 = xxh3_128_hex(&payload);
    let blake3 = compute_blake3.then(|| blake3_hex(&payload));
    let sampled_xxh3 = sample_blocks_xxh3_64(&payload);

    let path = cache_dir.join(name);
    let tmp = temp_path(&path);
    std::fs::write(&tmp, &payload).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path)
        .map_err(|e| format!("rename {} -> {}: {e}", tmp.display(), path.display()))?;

    Ok(WrittenPcm {
        size: payload.len() as u64,
        xxh3_128,
        blake3,
        sampled_xxh3,
    })
}

/// Reads a `.pcm` file back into its header and interleaved `i16` payload.
///
/// The header is validated (magic / version / CRC) and the payload is read as
/// `frames × channels` little-endian `i16` values. This runs on the
/// control/loader threads only, never the audio path.
///
/// # Errors
///
/// A human-readable reason when the file is missing, too short, or its header
/// is corrupt.
pub fn read_pcm_file(path: &Path) -> Result<(PcmHeader, Vec<i16>), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let header = PcmHeader::decode(&bytes)
        .ok_or_else(|| format!("{}: corrupt or truncated SPCM header", path.display()))?;
    let data_len = header
        .frames
        .saturating_mul(u64::from(header.channels))
        .saturating_mul(2) as usize;
    let payload = bytes
        .get(PCM_HEADER_LEN..PCM_HEADER_LEN + data_len)
        .ok_or_else(|| format!("{}: payload too short", path.display()))?;
    let mut out = Vec::with_capacity(payload.len() / 2);
    for chunk in payload.chunks_exact(2) {
        out.push(i16::from_le_bytes([chunk[0], chunk[1]]));
    }
    Ok((header, out))
}

/// The source-file metadata recorded in a manifest entry, read with a single
/// `stat` for the mtime and size (the xxh3-64 source hash is computed by the
/// caller, which already streams the file).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceMeta {
    /// Source file size in bytes.
    pub size: u64,
    /// Source file modification time in nanoseconds since the Unix epoch.
    pub mtime_ns: u64,
}

/// Reads `path`'s size and modification time for the manifest. Returns `None`
/// if the file cannot be stat-ed (the caller then records the sample as
/// missing).
#[must_use]
pub fn source_meta(path: &Path) -> Option<SourceMeta> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?;
    let mtime_ns = mtime
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    Some(SourceMeta {
        size: meta.len(),
        mtime_ns,
    })
}

/// xxh3-64 of a source file, read in full (used when building the cache).
///
/// # Errors
///
/// A human-readable reason on read failure.
pub fn xxh3_64_file(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    Ok(xxh3_64_hex(&bytes))
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
                sampled_xxh3: None,
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

    #[test]
    fn write_pcm_file_round_trips_header_and_data() {
        let dir = std::env::temp_dir().join(format!("bank-cache-pcm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let header = PcmHeader {
            channels: 2,
            sample_rate: 48_000,
            frames: 3,
            src_flac_size: 10,
            src_flac_xxh3: 0x1234_5678,
        };
        let data: [i16; 6] = [0, 1, -2, 3, 32767, -32768];
        let written =
            write_pcm_file(&dir, "000001.pcm", &header, &data, true).expect("write succeeds");
        assert_eq!(written.size, 64 + 6 * 2);
        assert_eq!(written.xxh3_128.len(), 32);
        assert_eq!(written.blake3.as_deref().map(str::len), Some(64));
        assert_eq!(written.sampled_xxh3.len(), 16);

        // The file on disk parses back to the same header and payload.
        let on_disk = std::fs::read(dir.join("000001.pcm")).expect("file exists");
        assert_eq!(on_disk.len(), written.size as usize);
        assert_eq!(PcmHeader::decode(&on_disk), Some(header));
        for (i, s) in data.iter().enumerate() {
            let lo = on_disk[64 + i * 2];
            let hi = on_disk[64 + i * 2 + 1];
            assert_eq!(i16::from_le_bytes([lo, hi]), *s);
        }
        // No `.tmp` leftover.
        assert!(!temp_path(&dir.join("000001.pcm")).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_pcm_file_round_trips() {
        let dir = std::env::temp_dir().join(format!("bank-cache-read-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let header = PcmHeader {
            channels: 2,
            sample_rate: 48_000,
            frames: 3,
            src_flac_size: 10,
            src_flac_xxh3: 0x1234_5678,
        };
        let data: [i16; 6] = [0, 1, -2, 3, 32767, -32768];
        write_pcm_file(&dir, "000001.pcm", &header, &data, false).unwrap();
        let (got_header, got_data) = read_pcm_file(&dir.join("000001.pcm")).unwrap();
        assert_eq!(got_header, header);
        assert_eq!(got_data, data);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_pcm_file_rejects_corrupt_header() {
        let dir = std::env::temp_dir().join(format!("bank-cache-readbad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let header = PcmHeader {
            channels: 1,
            sample_rate: 48_000,
            frames: 1,
            src_flac_size: 0,
            src_flac_xxh3: 0,
        };
        write_pcm_file(&dir, "000000.pcm", &header, &[0], false).unwrap();
        let path = dir.join("000000.pcm");
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[0] = b'X';
        std::fs::write(&path, &bytes).unwrap();
        assert!(read_pcm_file(&path).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_pcm_file_blake3_is_optional() {
        let dir = std::env::temp_dir().join(format!("bank-cache-noblake-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let header = PcmHeader {
            channels: 1,
            sample_rate: 44_100,
            frames: 1,
            src_flac_size: 0,
            src_flac_xxh3: 0,
        };
        let written = write_pcm_file(&dir, "000000.pcm", &header, &[0], false).unwrap();
        assert_eq!(written.blake3, None);
        assert_eq!(written.xxh3_128.len(), 32);
        assert_eq!(written.sampled_xxh3.len(), 16);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
