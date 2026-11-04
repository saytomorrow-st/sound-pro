//! The source of a decoded `i16` sample (RFC-002 §3.1, stage 0).
//!
//! A [`SampleSource`] abstracts where a sample's `i16` PCM lives so the engine
//! can later support disk streaming without another rewrite. On this stage it
//! has exactly one variant, [`SampleSource::Resident`], which holds the whole
//! sample in memory and is bit-for-bit the same as the previous
//! `Vec<Arc<[i16]>>` representation.

use std::sync::Arc;

/// The source of a decoded `i16` sample, indexed by a region's `sample_index`.
#[derive(Debug, Clone)]
pub enum SampleSource {
    /// The whole sample is resident in memory.
    Resident(Arc<[i16]>),
}

impl SampleSource {
    /// Wraps already-decoded `i16` PCM in a resident source.
    #[must_use]
    pub fn resident(data: Arc<[i16]>) -> Self {
        Self::Resident(data)
    }

    /// The underlying `i16` PCM.
    ///
    /// Realtime-safe: no allocation, locking, or I/O.
    #[must_use]
    pub fn as_i16(&self) -> &[i16] {
        match self {
            Self::Resident(data) => data,
        }
    }

    /// Number of `i16` values in the sample (`data.len()`).
    ///
    /// Realtime-safe.
    #[must_use]
    pub fn len(&self) -> usize {
        self.as_i16().len()
    }

    /// Whether the sample holds no data.
    ///
    /// Realtime-safe.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.as_i16().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resident_exposes_data() {
        let data: Arc<[i16]> = Arc::from([1_i16, 2, 3].as_slice());
        let src = SampleSource::resident(Arc::clone(&data));
        assert_eq!(src.as_i16(), &[1, 2, 3]);
        assert_eq!(src.len(), 3);
        assert!(!src.is_empty());
    }

    #[test]
    fn resident_empty_is_empty() {
        let src = SampleSource::resident(Arc::from(Vec::<i16>::new()));
        assert!(src.is_empty());
        assert_eq!(src.len(), 0);
        assert_eq!(src.as_i16(), []);
    }
}
