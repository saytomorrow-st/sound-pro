//! Sampler: SFZ instrument support (RFC-001).
//!
//! This module hosts the sampler described in `docs/rfc/001-sampler-sfz.md`.
//! PR 1 delivered the pure text parser in [`sfz`]; PR 2 adds the voice engine:
//! [`region`] (resolved regions and key lookup), [`voice`] (playhead, envelope,
//! interpolation) and [`engine`] (voice pool, stealing, groups, rendering).
//! [`source`] abstracts resident vs. streamed sample data and [`stream`]
//! implements the audio-thread side of disk streaming (RFC-002 stage 5).

pub mod engine;
pub mod region;
pub mod sfz;
pub mod source;
pub mod stream;
pub mod voice;
