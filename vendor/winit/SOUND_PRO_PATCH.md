# Vendored winit 0.30.13 (sound-pro patch)

Source: crates.io `winit` 0.30.13, copied verbatim (license: Apache-2.0, see
`LICENSE`). Wired in via `[patch.crates-io]` in the workspace `Cargo.toml`.

## Patch

Only `src/platform_impl/linux/x11/event_processor.rs` is changed:

- In the `XdndPosition` branch, after the drag is accepted, unpack the root
  coordinates from `data.l[2]` (`(x << 16) | y`), subtract the window origin
  from `TranslateCoordinates(window, root)` and emit
  `WindowEvent::CursorMoved`. Upstream ignores these coordinates, so egui's
  pointer froze during external file drags (ghost preview did not move).
- `unpack_xdnd_position` helper + unit test `sound_pro_xdnd_tests`.

Check: `diff -r ~/.cargo/registry/src/*/winit-0.30.13 vendor/winit`.

## Updating

When eframe moves to a newer winit: copy the new release from the cargo
registry, re-apply the hunk above (or drop the vendor if upstream emits
`CursorMoved` for XDND), update the `[patch.crates-io]` entry, run
`cargo update -p winit` and the full CI cycle.
