---
applyTo: "**/tests/**/*.rs,**/*_test.rs"
---

# Test rules

- Tests must pass on CI machines **without audio hardware**. Drive the engine
  through `OfflineRenderer` / `Renderer`, never through a real device.
- Every behavioural claim in a `specs/*.md` needs a test that fails if the
  claim is broken. Prefer small, named tests over one big one.
- Compare floats with an explicit tolerance (`1e-6` for engine math) or with
  `to_bits()` when bit-exactness is the contract (block-size independence).
- Golden fixtures: compare via the `compare_with_golden` helper in
  `crates/audio-core/tests/golden.rs`. **Never** add `UPDATE_GOLDENS` to code,
  CI, or task files; regeneration is a human-only action in a dedicated PR.
- It is forbidden to weaken a test to make it pass: no removing asserts,
  widening tolerances, or `#[ignore]` without a linked issue explaining why.
- Fixtures shared between integration tests live in `tests/common/mod.rs`.
- Property-style loops (e.g. quantum sizes 16/64/1024) are encouraged; keep them
  deterministic — no randomness, no wall-clock, no sleeps.
