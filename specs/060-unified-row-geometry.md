# SPEC-060: unified row geometry, track colour, folders (v0.1.43)

## Problem
SPEC-057/058/059 left the live app broken: compact headers lose the volume/pan
sliders, waveforms are shorter than their lane (and shorter again inside a
folder, shifting later rows), folders misbehave, and a track's colour does not
travel with it.

## Requirements
1. One row-geometry source in `track_rows.rs` (`visible_rows`, `row_y`,
   `row_heights`, `total_height`). Header column, lanes, clips, waveforms, grid,
   hit-testing, drop targets and drag preview derive from it. No per-widget
   vertical padding, no vertical folder indent, no item spacing between rows.
2. Clip/waveform rect = lane rect minus `CLIP_INSET`, identical in and out of
   folders. Folder children differ only by horizontal indent.
3. Compact header: one line (name, M/S/R, compact volume and pan) that fits the
   compact height exactly.
4. Colour: default (palette-by-index) colours are pinned onto the track/folder
   before any structural edit, so colour follows the track. Pinning is not a
   separate undo step.
5. Folder drop zone (user-directed): "драг-область папки должна быть более
   явной: драгнуть можно в любое место развернутой папки, в том числе и между
   дорожками которые уже в папке, а не только в заголовок элемента, так же и
   вытаскивание из папки должно срабатывать в любом месте кроме папки".
   - Expanded folder: whole block (header + children + gaps) is the drop zone
     and is highlighted on hover; dropping inside puts tracks into the folder
     at the insertion position.
   - Dropping outside the block removes a dragged child from the folder.
   - Collapsed folder: header-only target; no region drop onto hidden tracks.
6. Reorder/drop-into-folder pass a post-removal position to `move_tracks`.

## Acceptance
Headless tests in `main.rs::header_drag_tests`, `track_rows.rs`, and
`tests/track_color.rs`; invariant: header heights == lane heights per preset,
with and without folders.
