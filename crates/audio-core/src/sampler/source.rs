//! The source of a decoded `i16` sample (RFC-002 §3.1, stages 0 and 5).
//!
//! A [`SampleSource`] abstracts where a sample's `i16` PCM lives:
//!
//! - [`SampleSource::Resident`] holds the whole sample in memory and is
//!   bit-for-bit the same as the previous `Vec<Arc<[i16]>>` representation.
//! - [`SampleSource::Streaming`] keeps only the resident head
//!   ([`SampleHead`]); the tail is delivered by a
//!   [`VoiceStream`](super::stream::VoiceStream). Wiring streamed sources into
//!   the engine is RFC-002 stage 6; until then [`SampleSource::as_i16`] exposes
//!   just the head.

use std::sync::Arc;

use super::stream::SampleHead;

/// The source of a decoded `i16` sample, indexed by a region's `sample_index`.
#[derive(Debug, Clone)]
pub enum SampleSource {
    /// The whole sample is resident in memory.
    Resident(Arc<[i16]>),
    /// Only the head is resident; the tail is streamed from disk.
    Streaming(SampleHead),
}

impl SampleSource {
    /// Wraps already-decoded `i16` PCM in a resident source.
    #[must_use]
    pub fn resident(data: Arc<[i16]>) -> Self {
        Self::Resident(data)
    }

    /// Wraps a streamed sample's resident head.
    #[must_use]
    pub fn streaming(head: SampleHead) -> Self {
        Self::Streaming(head)
    }

    /// Whether the tail of this sample is streamed.
    #[must_use]
    pub fn is_streaming(&self) -> bool {
        matches!(self, Self::Streaming(_))
    }

    /// The streamed head, if this is a [`SampleSource::Streaming`] source.
    #[must_use]
    pub fn head(&self) -> Option<&SampleHead> {
        match self {
            Self::Resident(_) => None,
            Self::Streaming(head) => Some(head),
        }
    }

    /// The resident `i16` PCM: the whole sample for
    /// [`SampleSource::Resident`], only the head for
    /// [`SampleSource::Streaming`].
    ///
    /// Realtime-safe: no allocation, locking, or I/O.
    #[must_use]
    pub fn as_i16(&self) -> &[i16] {
        match self {
            Self::Resident(data) => data,
            Self::Streaming(head) => head.data(),
        }
    }

    /// Number of resident `i16` values (`as_i16().len()`).
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

    #[test]
    fn streaming_exposes_head_only() {
        let pcm: Vec<i16> = (0..10).collect();
        let head = SampleHead::from_pcm(&pcm, 1, 7, 4);
        let src = SampleSource::streaming(head);
        assert!(src.is_streaming());
        assert_eq!(src.as_i16(), &[0, 1, 2, 3]);
        assert_eq!(src.len(), 4);
        let head = src.head().expect("streaming head");
        assert_eq!(head.total_frames(), 10);
        assert_eq!(head.file_id(), 7);
        assert!(head.has_tail());
    }

    #[test]
    fn resident_has_no_head() {
        let src = SampleSource::resident(Arc::from([1_i16].as_slice()));
        assert!(!src.is_streaming());
        assert!(src.head().is_none());
    }
}
