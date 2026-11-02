//! Track-row layout for track folders (SPEC-055).
//!
//! The timeline and the track-headers panel agree on a single ordered list of
//! visible rows. Each row is either a folder header or a track; a collapsed
//! folder contributes only its header row and hides its children. This is
//! pure state (no drawing), so the ordering, collapsing and indentation are
//! tested without a display.

use audio_core::session::Session;

/// Default height of a track row, in pixels (SPEC-056 §1).
pub const DEFAULT_TRACK_HEIGHT: f32 = 72.0;
/// Minimum height of a track row, in pixels (SPEC-056 §1).
pub const MIN_TRACK_HEIGHT: f32 = 32.0;
/// Maximum height of a track row, in pixels (SPEC-056 §1).
pub const MAX_TRACK_HEIGHT: f32 = 400.0;
/// Fixed height of a folder header row, in pixels (SPEC-056 §2).
pub const FOLDER_ROW_HEIGHT: f32 = 28.0;

/// One visible row in the track area: a folder header or a track.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    /// A folder header row, addressed by folder index.
    Folder(usize),
    /// A track row, addressed by track index.
    Track(usize),
}

/// Maps each track index to its folder (if any), in one pass.
fn folder_of(session: &Session) -> Vec<Option<usize>> {
    let mut map = vec![None; session.tracks.len()];
    for (folder, children) in session.folder_children.iter().enumerate() {
        for &child in children {
            if child < map.len() {
                map[child] = Some(folder);
            }
        }
    }
    map
}

/// The ordered list of visible rows for `session` (SPEC-055 §4).
///
/// A folder header is emitted immediately before the first (lowest-index) of
/// its children; a collapsed folder hides its children. Tracks not in any
/// folder appear at the top level in track order. Empty folders are emitted
/// after every track, in folder order.
#[must_use]
pub fn visible_rows(session: &Session) -> Vec<Row> {
    let map = folder_of(session);
    // First child (lowest track index) of each folder, if any.
    let mut first_child = vec![usize::MAX; session.folders.len()];
    for (folder, children) in session.folder_children.iter().enumerate() {
        if let Some(&first) = children.iter().min() {
            first_child[folder] = first;
        }
    }
    let mut rows = Vec::new();
    let mut emitted = vec![false; session.folders.len()];
    for (track, folder) in map.iter().enumerate() {
        // Emit any folder header whose first child is this track.
        for (f, &first) in first_child.iter().enumerate() {
            if first == track && !emitted[f] {
                rows.push(Row::Folder(f));
                emitted[f] = true;
            }
        }
        match folder {
            Some(f) if session.folders[*f].collapsed => {}
            _ => rows.push(Row::Track(track)),
        }
    }
    // Empty folders (no children) at the bottom, in folder order.
    for (folder, &first) in first_child.iter().enumerate() {
        if first == usize::MAX && !emitted[folder] {
            rows.push(Row::Folder(folder));
            emitted[folder] = true;
        }
    }
    rows
}

/// The visual row index of a track, or `None` when the track is hidden
/// (inside a collapsed folder).
#[must_use]
pub fn row_of_track(session: &Session, track: usize) -> Option<usize> {
    visible_rows(session)
        .iter()
        .position(|row| *row == Row::Track(track))
}

/// The visual row index of a folder header, or `None` when it is not shown.
#[must_use]
pub fn row_of_folder(session: &Session, folder: usize) -> Option<usize> {
    visible_rows(session)
        .iter()
        .position(|row| *row == Row::Folder(folder))
}

/// Whether `track` is the child of a collapsed folder, and therefore hidden.
#[must_use]
pub fn is_hidden(session: &Session, track: usize) -> bool {
    let map = folder_of(session);
    map[track].is_some_and(|f| session.folders[f].collapsed)
}

/// The folder that holds `track`, if any (SPEC-055).
#[must_use]
pub fn folder_of_track(session: &Session, track: usize) -> Option<usize> {
    folder_of(session).get(track).copied().flatten()
}

/// The track shown on visual row `row`, or `None` when `row` is a folder
/// header or out of range. The inverse of [`row_of_track`].
#[must_use]
pub fn track_at_row(session: &Session, row: usize) -> Option<usize> {
    visible_rows(session).get(row).and_then(|r| match *r {
        Row::Track(t) => Some(t),
        Row::Folder(_) => None,
    })
}

/// The number of visible rows (folder headers plus unfolded tracks).
#[must_use]
pub fn row_count(session: &Session) -> usize {
    visible_rows(session).len()
}

/// Height of track `track`, or [`DEFAULT_TRACK_HEIGHT`] when the per-track
/// height is unknown (e.g. a freshly added track) (SPEC-056 §1).
#[must_use]
pub fn track_height(heights: &[f32], track: usize) -> f32 {
    heights
        .get(track)
        .copied()
        .unwrap_or(DEFAULT_TRACK_HEIGHT)
        .clamp(MIN_TRACK_HEIGHT, MAX_TRACK_HEIGHT)
}

/// Height of one visual row: a folder is [`FOLDER_ROW_HEIGHT`], a track its
/// per-track height (SPEC-056 §1–2).
fn row_height_of(heights: &[f32], row: Row) -> f32 {
    match row {
        Row::Folder(_) => FOLDER_ROW_HEIGHT,
        Row::Track(t) => track_height(heights, t),
    }
}

/// Height of the visual row `row` (folder fixed, track per-track).
#[must_use]
pub fn row_height(heights: &[f32], session: &Session, row: usize) -> f32 {
    visible_rows(session)
        .get(row)
        .copied()
        .map_or(0.0, |r| row_height_of(heights, r))
}

/// The vertical offset (in pixels from the top of the track area) of visual
/// row `row`: the sum of the heights of every row above it (SPEC-056 §1).
#[must_use]
pub fn row_y(heights: &[f32], session: &Session, row: usize) -> f32 {
    let rows = visible_rows(session);
    rows.iter()
        .take(row)
        .fold(0.0, |acc, r| acc + row_height_of(heights, *r))
}

/// The total height of every visible row (SPEC-056 §1).
#[must_use]
pub fn total_height(heights: &[f32], session: &Session) -> f32 {
    visible_rows(session)
        .iter()
        .fold(0.0, |acc, r| acc + row_height_of(heights, *r))
}

/// Heights of every visible row, in visual order (folders fixed, tracks
/// per-track) (SPEC-056 §1–2).
#[must_use]
pub fn row_heights(heights: &[f32], session: &Session) -> Vec<f32> {
    visible_rows(session)
        .iter()
        .map(|r| row_height_of(heights, *r))
        .collect()
}

/// Hit-test a vertical offset `y` against precomputed per-row heights (see
/// [`row_heights`]), returning the visual row under it, or `None` above the
/// first row or below the last (SPEC-056 §1).
#[must_use]
pub fn row_at_y_heights(row_heights: &[f32], y: f32) -> Option<usize> {
    if !y.is_finite() || y < 0.0 {
        return None;
    }
    let mut acc = 0.0;
    for (i, h) in row_heights.iter().enumerate() {
        if y < acc + h {
            return Some(i);
        }
        acc += h;
    }
    None
}

/// Hit-test a vertical offset `y` (in pixels from the top of the track area,
/// i.e. below the ruler/tempo header) to the visual row under it (SPEC-056
/// §1). Returns `None` above the first row or below the last.
#[must_use]
pub fn row_at_y(heights: &[f32], session: &Session, y: f32) -> Option<usize> {
    row_at_y_heights(&row_heights(heights, session), y)
}
