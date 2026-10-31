//! On-disk cache of decoded files at the project rate, keyed by path,
//! modification time and size. Importing the same MP3 again then reads raw
//! samples instead of decoding and resampling it. Never called on the audio
//! path.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::audio_file::Channels;

const MAGIC: &[u8; 8] = b"SPPCM001";
/// Entries kept; the least recently written ones are removed first.
const MAX_ENTRIES: usize = 16;

/// A file's identity: where it is and what it looked like when read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stamp {
    path: PathBuf,
    modified: Duration,
    size: u64,
}

impl Stamp {
    /// The stamp of `path` now, or `None` when it cannot be read.
    #[must_use]
    pub fn of(path: &Path) -> Option<Self> {
        let path = std::path::absolute(path).ok()?;
        let meta = std::fs::metadata(&path).ok()?;
        let modified = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
        Some(Self {
            path,
            modified,
            size: meta.len(),
        })
    }
}

/// A directory of decoded files.
#[derive(Debug, Clone)]
pub struct PcmCache {
    dir: PathBuf,
}

impl PcmCache {
    /// A cache in `dir`, created on the first store.
    #[must_use]
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// `$XDG_CACHE_HOME/sound-pro/pcm`, else `~/.cache/sound-pro/pcm`.
    #[must_use]
    pub fn default_location() -> Option<Self> {
        let base = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
        Some(Self::new(base.join("sound-pro").join("pcm")))
    }

    fn entry(&self, stamp: &Stamp, rate: u32) -> PathBuf {
        let mut hasher = DefaultHasher::new();
        stamp.path.hash(&mut hasher);
        self.dir
            .join(format!("{:016x}-{rate}.pcm", hasher.finish()))
    }

    /// The cached content of `path` at `rate`, if the file is unchanged
    /// since it was stored.
    #[must_use]
    pub fn load(&self, path: &Path, rate: u32) -> Option<Channels> {
        let stamp = Stamp::of(path)?;
        let bytes = std::fs::read(self.entry(&stamp, rate)).ok()?;
        decode(&bytes, &stamp, rate)
    }

    /// Stores `channels`, read from the file as it looked at `stamp`.
    /// Failures are ignored: the cache only saves time.
    pub fn store(&self, stamp: &Stamp, rate: u32, channels: &Channels) {
        let _ = self.try_store(stamp, rate, channels);
    }

    fn try_store(&self, stamp: &Stamp, rate: u32, channels: &Channels) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let target = self.entry(stamp, rate);
        let temp = target.with_extension("tmp");
        let mut out = std::io::BufWriter::new(std::fs::File::create(&temp)?);
        out.write_all(&encode_header(stamp, rate, channels))?;
        for data in std::iter::once(&channels.left).chain(channels.right.as_ref()) {
            for chunk in data.chunks(1 << 14) {
                let bytes: Vec<u8> = chunk.iter().flat_map(|v| v.to_le_bytes()).collect();
                out.write_all(&bytes)?;
            }
        }
        out.flush()?;
        drop(out);
        std::fs::rename(&temp, &target)?;
        self.evict();
        Ok(())
    }

    fn evict(&self) {
        let Ok(dir) = std::fs::read_dir(&self.dir) else {
            return;
        };
        let mut entries: Vec<(SystemTime, PathBuf)> = dir
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().is_some_and(|x| x == "pcm"))
            .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
            .collect();
        if entries.len() <= MAX_ENTRIES {
            return;
        }
        entries.sort();
        let surplus = entries.len() - MAX_ENTRIES;
        for (_, path) in entries.into_iter().take(surplus) {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn encode_header(stamp: &Stamp, rate: u32, channels: &Channels) -> Vec<u8> {
    let path = stamp.path.to_string_lossy();
    let mut head = Vec::new();
    head.extend_from_slice(MAGIC);
    head.extend_from_slice(&(path.len() as u32).to_le_bytes());
    head.extend_from_slice(path.as_bytes());
    head.extend_from_slice(&stamp.modified.as_secs().to_le_bytes());
    head.extend_from_slice(&stamp.modified.subsec_nanos().to_le_bytes());
    head.extend_from_slice(&stamp.size.to_le_bytes());
    head.extend_from_slice(&rate.to_le_bytes());
    head.push(if channels.right.is_some() { 2 } else { 1 });
    head.extend_from_slice(&(channels.left.len() as u64).to_le_bytes());
    head
}

fn decode(bytes: &[u8], stamp: &Stamp, rate: u32) -> Option<Channels> {
    let mut at = 0usize;
    let mut take = |n: usize| -> Option<&[u8]> {
        let part = bytes.get(at..at.checked_add(n)?)?;
        at += n;
        Some(part)
    };
    if take(8)? != MAGIC {
        return None;
    }
    let path_len = u32::from_le_bytes(take(4)?.try_into().ok()?) as usize;
    if take(path_len)? != stamp.path.to_string_lossy().as_bytes() {
        return None;
    }
    let secs = u64::from_le_bytes(take(8)?.try_into().ok()?);
    let nanos = u32::from_le_bytes(take(4)?.try_into().ok()?);
    let size = u64::from_le_bytes(take(8)?.try_into().ok()?);
    let stored_rate = u32::from_le_bytes(take(4)?.try_into().ok()?);
    if Duration::new(secs, nanos) != stamp.modified || size != stamp.size || stored_rate != rate {
        return None;
    }
    let count = take(1)?[0];
    let frames = usize::try_from(u64::from_le_bytes(take(8)?.try_into().ok()?)).ok()?;
    if !(1..=2).contains(&count) || frames == 0 {
        return None;
    }
    // The size is checked before anything is allocated.
    let body_len = frames.checked_mul(4)?.checked_mul(usize::from(count))?;
    let body = take(body_len)?;
    if at != bytes.len() {
        return None;
    }
    let channel = |part: &[u8]| -> Vec<f32> {
        part.chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect()
    };
    let (left, right) = body.split_at(frames * 4);
    Some(Channels {
        left: channel(left),
        right: (count == 2).then(|| channel(right)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dir(PathBuf);

    impl Dir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("pcm-cache-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn stereo() -> Channels {
        Channels {
            left: vec![0.5, -0.25, 1.0],
            right: Some(vec![0.0, 0.125, -1.0]),
        }
    }

    fn source(dir: &Dir, bytes: &[u8]) -> PathBuf {
        let path = dir.0.join("song.mp3");
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn a_stored_file_comes_back_unchanged() {
        let dir = Dir::new("hit");
        let cache = PcmCache::new(dir.0.join("cache"));
        let path = source(&dir, b"abc");
        assert!(cache.load(&path, 48_000).is_none());
        cache.store(&Stamp::of(&path).unwrap(), 48_000, &stereo());
        assert_eq!(cache.load(&path, 48_000), Some(stereo()));
        let mono = Channels {
            left: vec![0.1, 0.2],
            right: None,
        };
        cache.store(&Stamp::of(&path).unwrap(), 44_100, &mono);
        assert_eq!(cache.load(&path, 44_100), Some(mono));
        assert_eq!(cache.load(&path, 48_000), Some(stereo()));
    }

    #[test]
    fn a_changed_file_misses() {
        let dir = Dir::new("stale");
        let cache = PcmCache::new(dir.0.join("cache"));
        let path = source(&dir, b"abc");
        cache.store(&Stamp::of(&path).unwrap(), 48_000, &stereo());
        // Same size, later modification time.
        let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        let later = SystemTime::now() + Duration::from_secs(60);
        file.set_modified(later).unwrap();
        assert!(cache.load(&path, 48_000).is_none());
        // Same time, different size.
        let stamp = Stamp::of(&path).unwrap();
        cache.store(&stamp, 48_000, &stereo());
        assert!(cache.load(&path, 48_000).is_some());
        std::fs::write(&path, b"abcdef").unwrap();
        file.set_modified(later).unwrap();
        assert!(cache.load(&path, 48_000).is_none());
    }

    #[test]
    fn damaged_entries_are_ignored() {
        let dir = Dir::new("damaged");
        let cache = PcmCache::new(dir.0.join("cache"));
        let path = source(&dir, b"abc");
        let stamp = Stamp::of(&path).unwrap();
        cache.store(&stamp, 48_000, &stereo());
        let entry = cache.entry(&stamp, 48_000);
        let good = std::fs::read(&entry).unwrap();
        for cut in [0, 5, good.len() - 1] {
            std::fs::write(&entry, &good[..cut]).unwrap();
            assert!(cache.load(&path, 48_000).is_none(), "cut at {cut}");
        }
        let mut long = good.clone();
        long.push(0);
        std::fs::write(&entry, long).unwrap();
        assert!(cache.load(&path, 48_000).is_none());
        std::fs::write(&entry, good).unwrap();
        assert!(cache.load(&path, 48_000).is_some());
    }

    #[test]
    fn old_entries_are_evicted() {
        let dir = Dir::new("evict");
        let cache = PcmCache::new(dir.0.join("cache"));
        let path = source(&dir, b"abc");
        let stamp = Stamp::of(&path).unwrap();
        for rate in 1..=(MAX_ENTRIES as u32 + 4) {
            cache.store(&stamp, rate, &stereo());
        }
        let count = std::fs::read_dir(dir.0.join("cache")).unwrap().count();
        assert_eq!(count, MAX_ENTRIES);
    }
}
