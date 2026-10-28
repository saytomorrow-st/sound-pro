---
applyTo: "crates/audio-core/**/*.rs"
---

# audio-core rules

Code in this crate runs inside the audio callback. Apply these rules to every
line reachable from `Graph::process`, `Renderer::render`, `NodeProcessor::process`,
and `OfflineRenderer`'s inner loop:

1. **No allocation**: no `Vec::push`/`with_capacity`, `Box::new`, `String`,
   `format!`, `to_string`, `collect`, `clone` of heap data. Reserve in
   constructors; grow via `try_reserve` returning `GraphError`.
2. **No blocking**: no `Mutex`, `RwLock`, `mpsc`, `.await`, `thread::sleep`.
3. **No I/O / no panics on input**: no `println!`, `dbg!`, `std::fs`, `env`;
   validate inputs and return `Result` instead of `assert!`/`unwrap`.
4. **Errors over panics** on hot paths; `assert!` is acceptable only for
   setup-time constructor arguments. Document it under `# Panics`.
5. **Full docs**: every public item has a doc comment; `#![warn(missing_docs)]`
   and pedantic clippy are enabled. Avoid `#[allow(...)]`; if unavoidable,
   scope it to the smallest item and add a reason.
6. **Buffers are planar** (`AudioBuffer` is channel-major). Read/write only the
   first `ctx.frames` samples of each channel.
7. **Transport gating lives in the graph**, not in nodes: generators should not
   check `is_playing` themselves.
8. **No platform APIs** (`cpal`, WASAPI, ALSA, threads) — those belong to
   `daw-app`.
9. **No `unsafe`** (`#![forbid(unsafe_code)]`).

When adding DSP: write the formula in a doc comment with a source
(e.g. book/paper), add a unit test for the formula and, if it changes audio
output, a golden test. Never update existing goldens.
