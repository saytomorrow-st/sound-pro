//! Sampler: SFZ instrument support (RFC-001).
//!
//! This module is the future home of the sampler described in
//! `docs/rfc/001-sampler-sfz.md`. PR 1 of that RFC delivers only the pure text
//! parser in [`sfz`]; voice, region selection, engine and bank modules follow
//! in later PRs.

pub mod sfz;
