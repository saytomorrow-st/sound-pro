# Copilot workspace instructions

This repository is a Rust DAW engine. Source of truth for rules: `AGENTS.md`.
Read it before proposing changes; the short version:

- Rust workspace: `crates/audio-core` (engine, `#![forbid(unsafe_code)]`) and
  `crates/daw-app` (shell, platform backends behind the `realtime` feature).
- Every change must keep green:
  `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace --all-targets`.
- Never allocate, lock, or do I/O on the audio path (`Graph::process`,
  `Renderer::render`, audio callbacks). See `docs/REALTIME.md`.
- Never edit `crates/audio-core/tests/golden/*.wav` or run `UPDATE_GOLDENS=1`.
  Goldens are the DSP oracle; refuse such requests and explain why.
- Never weaken tests (remove asserts, loosen tolerances, add `#[ignore]`) to make
  CI pass.
- Implement only what `specs/*.md` describes; if a spec is missing, say so
  instead of inventing behaviour.
- Tests must run without audio hardware; use the offline renderer.
