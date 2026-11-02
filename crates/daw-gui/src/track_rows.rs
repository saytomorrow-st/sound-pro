//! Track-row layout for track folders (SPEC-055).
//!
//! The timeline and the track-headers panel agree on a single ordered list of
//! visible rows. Each row is either a folder header or a track; a collapsed
//! folder contributes only its header row and hides its children. This is
//! pure state (no drawing), so the ordering, collapsing and indentation are
//! tested without a display.

use audio_core::session::Session;

/// Default height of a track row, in pixels (SPEC-056 §1). This is also the
/// [`Standard`] height preset (SPEC-058 §3).
pub const DEFAULT_TRACK_HEIGHT: f32 = 72.0;
/// Minimum height of a track row, in pixels (SPEC-056 §1).
pub const MIN_TRACK_HEIGHT: f32 = 32.0;
/// Maximum height of a track row, in pixels (SPEC-056 §1).
pub const MAX_TRACK_HEIGHT: f32 = 400.0;
/// Fixed height of a folder header row, in pixels (SPEC-056 §2).
pub const FOLDER_ROW_HEIGHT: f32 = 28.0;
/// Compact track-height preset, in pixels (SPEC-058 §3).
pub const COMPACT_TRACK_HEIGHT: f32 = 40.0;
/// Standard track-height preset, in pixels (SPEC-058 §3). Identical to
/// [`DEFAULT_TRACK_HEIGHT`].
pub const STANDARD_TRACK_HEIGHT: f32 = 72.0;
/// Expanded track-height preset, in pixels (SPEC-058 §3). Stereo waveforms
/// show both channels only at or above this height (SPEC-058 §4).
pub const EXPANDED_TRACK_HEIGHT: f32 = 144.0;

/// The three fixed track-height presets, smallest first (SPEC-058 §3).
#[must_use]
pub const fn height_presets() -> [f32; 3] {
    [
        COMPACT_TRACK_HEIGHT,
        STANDARD_TRACK_HEIGHT,
        EXPANDED_TRACK_HEIGHT,
    ]
}

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

/// The height of the drag ghost for a header drag (SPEC-058 §1): the sum of
/// the dragged track rows, or a folder header plus its visible children. It is
/// independent of the window size, so it never spans the whole timeline.
#[must_use]
pub fn drag_ghost_height(
    heights: &[f32],
    session: &Session,
    tracks: &[usize],
    folder: Option<usize>,
) -> f32 {
    if let Some(folder) = folder {
        let mut h = FOLDER_ROW_HEIGHT;
        if !session.folders[folder].collapsed {
            for &child in &session.folder_children[folder] {
                h += track_height(heights, child);
            }
        }
        return h;
    }
    tracks
        .iter()
        .fold(0.0, |acc, &t| acc + track_height(heights, t))
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

/// The target track for a clip move at vertical offset `y`, clamped to a
/// valid track (SPEC-058 §2). A folder header row yields `fallback` (the clip
/// stays on its current track); `y` above the first row resolves to track
/// `0`, and `y` below the last row resolves to the last track — it never
/// wraps back to the top.
#[must_use]
pub fn clip_drop_track(heights: &[f32], session: &Session, y: f32, fallback: usize) -> usize {
    let last = session.tracks.len().saturating_sub(1);
    let Some(row) = row_at_y(heights, session, y) else {
        return if y < 0.0 { 0 } else { last };
    };
    track_at_row(session, row).unwrap_or(fallback).min(last)
}

/// Snaps a resized track height to the nearest fixed preset while it is at or
/// below [`EXPANDED_TRACK_HEIGHT`]; above that the height is free (custom)
/// up to [`MAX_TRACK_HEIGHT`] (SPEC-058 §3).
///
/// The result is always clamped to `[MIN_TRACK_HEIGHT, MAX_TRACK_HEIGHT]`.
/// A value below or equal to the midpoint between two presets snaps to the
/// nearer one; above [`EXPANDED_TRACK_HEIGHT`] the raw (clamped) value is
/// kept, so the drag switches from snapping to continuous once the row grows
/// past the largest preset.
#[must_use]
pub fn snap_track_height(height: f32) -> f32 {
    if !height.is_finite() {
        return STANDARD_TRACK_HEIGHT;
    }
    let height = height.clamp(MIN_TRACK_HEIGHT, MAX_TRACK_HEIGHT);
    if height >= EXPANDED_TRACK_HEIGHT {
        return height;
    }
    // Snap to the nearest preset; an exact midpoint rounds up to the larger
    // preset (a growing drag crosses into the next preset at its midpoint).
    let mut best = STANDARD_TRACK_HEIGHT;
    let mut best_d = f32::INFINITY;
    for preset in height_presets() {
        let d = (preset - height).abs();
        if d < best_d || (d == best_d && preset > best) {
            best = preset;
            best_d = d;
        }
    }
    best
}

/// The insertion row index (in `0..=rows.len()`) for a drop at vertical
/// offset `y`, for a drag-reorder indicator (SPEC-057 §1). A pointer in the
/// top half of a row inserts before that row; in the bottom half, after it.
/// Above the first row returns `0`, below the last returns `rows.len()`.
#[must_use]
pub fn drop_insertion_row(row_heights: &[f32], y: f32) -> usize {
    if !y.is_finite() || y < 0.0 {
        return 0;
    }
    let Some(row) = row_at_y_heights(row_heights, y) else {
        // Below the last row (or no rows): insert at the end.
        return row_heights.len();
    };
    let mut acc = 0.0;
    for (i, h) in row_heights.iter().enumerate() {
        if i == row {
            let mid = acc + h / 2.0;
            return if y < mid { row } else { row + 1 };
        }
        acc += h;
    }
    row_heights.len()
}

/// The track index where a drop at `insertion_row` lands: the number of track
/// rows shown strictly above that row (SPEC-057 §1). A folder header row
/// contributes no track, so dropping just above a folder's first child lands
/// at the top-level position right before that child.
#[must_use]
pub fn track_index_at_insertion(session: &Session, insertion_row: usize) -> usize {
    visible_rows(session)
        .iter()
        .take(insertion_row)
        .filter(|r| matches!(r, Row::Track(_)))
        .count()
}

/// The result of resolving a completed header drag at a vertical offset
/// (SPEC-057, SPEC-058 §0). Pure state: both fields are rebuilt from the
/// inputs, so the UI and the tests share one source of truth.
#[derive(Clone, Debug, PartialEq)]
pub struct DropOutcome {
    /// New order of tracks: `track_order[i]` is the original track index now
    /// at position `i`.
    pub track_order: Vec<usize>,
    /// New folder membership: `folder_children[f]` lists the original track
    /// indices now inside folder `f`, sorted ascending.
    pub folder_children: Vec<Vec<usize>>,
}

/// Where a header drop lands: either a reorder into a block at a track
/// position, or an addition to a folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DropTarget {
    /// Insert the dragged tracks as a contiguous block before track position
    /// `to` (in the post-removal track list).
    Reorder(usize),
    /// Add the dragged tracks to `folder` (their order is unchanged).
    Folder(usize),
}

/// Resolves a completed header drag at vertical offset `y` into the resulting
/// track order and folder membership, without touching any editor state
/// (SPEC-058 §0). Mirrors the Editor's `move_tracks` / `move_tracks_to_folder`
/// / `move_folder` math so the drop computation is testable end to end.
///
/// `dragged_tracks` are the (sorted, deduplicated) track indices being moved;
/// `dragged_folder` is `Some(folder)` when dragging a folder header (then
/// `dragged_tracks` is ignored). Returns `None` when `dragged_folder` is out
/// of range or the session is empty.
#[must_use]
pub fn apply_header_drop(
    session: &Session,
    row_heights: &[f32],
    y: f32,
    dragged_tracks: &[usize],
    dragged_folder: Option<usize>,
) -> Option<DropOutcome> {
    let count = session.tracks.len();
    let rows = visible_rows(session);
    let ins = drop_insertion_row(row_heights, y);
    let to_full = track_index_at_insertion(session, ins);

    // Determine the folder under the pointer (a folder header, or a track
    // that belongs to a folder).
    let pointer_row = row_at_y_heights(row_heights, y).and_then(|r| rows.get(r).copied());
    let target_folder = match pointer_row {
        Some(Row::Folder(f)) => Some(f),
        Some(Row::Track(t)) => folder_of_track(session, t),
        None => None,
    };

    // A folder drag reorders the folder's children as one block; it never
    // nests (single-level folders, SPEC-055 §7).
    let target = match dragged_folder {
        Some(folder) => {
            if folder >= session.folders.len() {
                return None;
            }
            DropTarget::Reorder(folder_drop_position(session, folder, to_full))
        }
        None => match target_folder {
            Some(folder) => DropTarget::Folder(folder),
            None => DropTarget::Reorder(to_full),
        },
    };

    // A moving set (tracks) vs. a folder's children, both as sorted indices.
    let moved: Vec<usize> = match dragged_folder {
        Some(folder) => {
            let mut c = session.folder_children.get(folder)?.clone();
            c.sort_unstable();
            c
        }
        None => {
            let mut m = dragged_tracks.to_vec();
            m.sort_unstable();
            m.dedup();
            m
        }
    };
    if moved.is_empty() {
        // Nothing to move: the order and folders are unchanged.
        return Some(DropOutcome {
            track_order: (0..count).collect(),
            folder_children: session.folder_children.clone(),
        });
    }

    match target {
        DropTarget::Folder(folder) => {
            // move_tracks_to_folder: keep order, re-parent the tracks.
            let mut folder_children = session.folder_children.clone();
            for list in &mut folder_children {
                list.retain(|c| !moved.contains(c));
            }
            if let Some(list) = folder_children.get_mut(folder) {
                list.extend_from_slice(&moved);
                list.sort_unstable();
            }
            Some(DropOutcome {
                track_order: (0..count).collect(),
                folder_children,
            })
        }
        DropTarget::Reorder(to) => {
            // move_tracks (preceded by remove_tracks_from_folder when any
            // moved track was inside a folder): a top-level reorder leaves the
            // dragged tracks un-parented, then re-indexes the rest.
            let insert_at = to.min(count.saturating_sub(moved.len()));
            let mut old_to_new = vec![0usize; count];
            let mut non_moved = 0usize;
            for (t, slot) in old_to_new.iter_mut().enumerate() {
                if moved.binary_search(&t).is_ok() {
                    continue;
                }
                *slot = non_moved;
                non_moved += 1;
            }
            for (order, &t) in moved.iter().enumerate() {
                old_to_new[t] = insert_at + order;
            }
            for (t, slot) in old_to_new.iter_mut().enumerate() {
                if moved.binary_search(&t).is_err() && *slot >= insert_at {
                    *slot += moved.len();
                }
            }
            // Build the new order as the inverse permutation.
            let mut track_order = vec![0usize; count];
            for (old, &new) in old_to_new.iter().enumerate() {
                track_order[new] = old;
            }
            let mut folder_children = session.folder_children.clone();
            for list in &mut folder_children {
                list.retain(|c| !moved.contains(c));
            }
            for list in &mut folder_children {
                for c in list.iter_mut() {
                    *c = old_to_new[*c];
                }
                list.sort_unstable();
            }
            Some(DropOutcome {
                track_order,
                folder_children,
            })
        }
    }
}

/// Maps a full-track-list insertion position `to_full` into the index space
/// `move_folder` expects: the number of non-child tracks strictly before the
/// dragged folder's children in the post-removal list. The dragged children
/// themselves are excluded so the block lands at the correct place relative
/// to the tracks that stay put.
#[must_use]
pub fn folder_drop_position(session: &Session, folder: usize, to_full: usize) -> usize {
    let Some(children) = session.folder_children.get(folder) else {
        return to_full;
    };
    let mut sorted = children.clone();
    sorted.sort_unstable();
    // A track's original index is also its full-list position, so the count of
    // non-children with index < `to_full` is exactly the insertion index among
    // the tracks that stay put.
    (0..session.tracks.len())
        .filter(|t| sorted.binary_search(t).is_err() && *t < to_full)
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session_with_folders() -> Session {
        let mut session = Session::new("s", 48_000, 120.0);
        for n in 0..4 {
            session
                .tracks
                .push(audio_core::SessionTrack::new(&format!("t{n}")));
        }
        session.folders.push(audio_core::TrackFolder::new("F"));
        session.folder_children.push(vec![0, 2]);
        session
    }

    #[test]
    fn drop_insertion_row_splits_a_row_into_before_and_after() {
        // Three rows of 10px each.
        let heights = vec![10.0, 10.0, 10.0];
        assert_eq!(drop_insertion_row(&heights, -1.0), 0);
        assert_eq!(drop_insertion_row(&heights, 0.0), 0);
        assert_eq!(drop_insertion_row(&heights, 4.9), 0);
        assert_eq!(drop_insertion_row(&heights, 5.0), 1);
        assert_eq!(drop_insertion_row(&heights, 14.9), 1);
        assert_eq!(drop_insertion_row(&heights, 15.0), 2);
        assert_eq!(drop_insertion_row(&heights, 24.9), 2);
        assert_eq!(drop_insertion_row(&heights, 25.0), 3);
        assert_eq!(drop_insertion_row(&heights, 30.0), 3);
        assert_eq!(drop_insertion_row(&heights, 99.0), 3);
    }

    #[test]
    fn drop_insertion_row_handles_empty_and_non_finite() {
        assert_eq!(drop_insertion_row(&[], 0.0), 0);
        assert_eq!(drop_insertion_row(&[10.0], f32::NAN), 0);
    }

    #[test]
    fn track_index_at_insertion_counts_track_rows_above() {
        let session = session_with_folders();
        // visible_rows: Folder(0), Track(0), Track(1), Track(2), Track(3).
        let rows = visible_rows(&session);
        assert_eq!(rows.len(), 5);
        // Insertion before the folder header -> no tracks above.
        assert_eq!(track_index_at_insertion(&session, 0), 0);
        // Between folder header and track 0 -> one track (0) above.
        assert_eq!(track_index_at_insertion(&session, 1), 0);
        // After track 1 -> tracks 0 and 1 above.
        assert_eq!(track_index_at_insertion(&session, 3), 2);
        // At the very end -> all four tracks above.
        assert_eq!(track_index_at_insertion(&session, 5), 4);
    }

    #[test]
    fn snap_track_height_snaps_below_expanded_and_frees_above() {
        assert_eq!(snap_track_height(0.0), COMPACT_TRACK_HEIGHT);
        assert_eq!(snap_track_height(30.0), COMPACT_TRACK_HEIGHT);
        assert_eq!(snap_track_height(39.9), COMPACT_TRACK_HEIGHT);
        // Midpoint between Compact (40) and Standard (72) is 56.
        assert_eq!(snap_track_height(56.0), STANDARD_TRACK_HEIGHT);
        assert_eq!(snap_track_height(72.0), STANDARD_TRACK_HEIGHT);
        // Midpoint between Standard (72) and Expanded (144) is 108.
        assert_eq!(snap_track_height(108.0), EXPANDED_TRACK_HEIGHT);
        assert_eq!(snap_track_height(144.0), EXPANDED_TRACK_HEIGHT);
        // Above Expanded: free (custom) height.
        assert_eq!(snap_track_height(150.0), 150.0);
        assert_eq!(snap_track_height(200.0), 200.0);
        // Clamping to the allowed range.
        assert_eq!(snap_track_height(-5.0), COMPACT_TRACK_HEIGHT);
        assert_eq!(snap_track_height(10_000.0), MAX_TRACK_HEIGHT);
        assert_eq!(snap_track_height(f32::NAN), STANDARD_TRACK_HEIGHT);
    }

    fn session_with_two_folders() -> Session {
        let mut session = Session::new("s", 48_000, 120.0);
        for n in 0..6 {
            session
                .tracks
                .push(audio_core::SessionTrack::new(&format!("t{n}")));
        }
        session.folders.push(audio_core::TrackFolder::new("A"));
        session.folders.push(audio_core::TrackFolder::new("B"));
        session.folder_children.push(vec![1, 3]); // folder A
        session.folder_children.push(vec![5]); // folder B
        session
    }

    /// The vertical offset inside visual `row` at fraction `frac` of its
    /// height (`frac < 0.5` inserts before the row, `>= 0.5` after it).
    fn y_at(session: &Session, heights: &[f32], row: usize, frac: f32) -> f32 {
        row_y(heights, session, row) + row_height(heights, session, row) * frac
    }

    #[test]
    fn drop_moves_track_to_top() {
        let session = session_with_two_folders();
        // rows: Track0, FolderA, Track1, Track2, Track3, Track4, FolderB, Track5
        let heights = vec![DEFAULT_TRACK_HEIGHT; 6];
        let rh = row_heights(&heights, &session);
        let out =
            apply_header_drop(&session, &rh, y_at(&session, &heights, 0, 0.0), &[4], None).unwrap();
        assert_eq!(out.track_order, vec![4, 0, 1, 2, 3, 5]);
    }

    #[test]
    fn drop_moves_track_to_bottom() {
        let session = session_with_two_folders();
        let heights = vec![DEFAULT_TRACK_HEIGHT; 6];
        let rh = row_heights(&heights, &session);
        // Last visual row is Track5; drop after it (frac > 0.5).
        let last = visible_rows(&session).len() - 1;
        let out = apply_header_drop(
            &session,
            &rh,
            y_at(&session, &heights, last, 1.0),
            &[0],
            None,
        )
        .unwrap();
        assert_eq!(out.track_order, vec![1, 2, 3, 4, 5, 0]);
    }

    #[test]
    fn drop_moves_track_between_folder_rows() {
        let session = session_with_two_folders();
        let heights = vec![DEFAULT_TRACK_HEIGHT; 6];
        let rh = row_heights(&heights, &session);
        // Move track 0 to just after folder A's children (after track 3).
        // Visual rows: Track0(0) FolderA(1) Track1(2) Track2(3) Track3(4) Track4(5) FolderB(6) Track5(7)
        // Drop in the bottom half of Track3 (row 4) -> insert before track 4.
        let out =
            apply_header_drop(&session, &rh, y_at(&session, &heights, 4, 1.0), &[0], None).unwrap();
        // Remove 0, insert before track 4 -> [1,2,3,4,0,5].
        assert_eq!(out.track_order, vec![1, 2, 3, 4, 0, 5]);
    }

    #[test]
    fn drop_onto_folder_header_moves_track_into_folder() {
        let session = session_with_two_folders();
        let heights = vec![DEFAULT_TRACK_HEIGHT; 6];
        let rh = row_heights(&heights, &session);
        // Track 2 is a free track; drop it onto folder A's header (row 1).
        let out =
            apply_header_drop(&session, &rh, y_at(&session, &heights, 1, 0.5), &[2], None).unwrap();
        // Order unchanged; folder A now holds {1,2,3}.
        assert_eq!(out.track_order, vec![0, 1, 2, 3, 4, 5]);
        assert_eq!(out.folder_children[0], vec![1, 2, 3]);
        assert_eq!(out.folder_children[1], vec![5]);
    }

    #[test]
    fn drop_onto_free_track_moves_track_out_of_folder() {
        let session = session_with_two_folders();
        let heights = vec![DEFAULT_TRACK_HEIGHT; 6];
        let rh = row_heights(&heights, &session);
        // Track 1 is inside folder A; drop it onto free track 4's row (row 5,
        // bottom half). This is a plain reorder: track 1 leaves folder A and
        // lands after track 4.
        let out = apply_header_drop(&session, &rh, y_at(&session, &heights, 5, 0.75), &[1], None)
            .unwrap();
        assert_eq!(out.track_order, vec![0, 2, 3, 4, 5, 1]);
        // Track 1 left folder A; track 3 (original index 3) is now at index 2.
        assert_eq!(out.folder_children[0], vec![2]);
        // Track 5 (original index 5) is now at index 4.
        assert_eq!(out.folder_children[1], vec![4]);
    }

    #[test]
    fn drop_moves_folder_above_another_folder() {
        let session = session_with_two_folders();
        let heights = vec![DEFAULT_TRACK_HEIGHT; 6];
        let rh = row_heights(&heights, &session);
        // Drag folder B (children {5}) and drop before folder A (row 1, frac 0).
        // Folder A children {1,3}; folder B children {5}.
        let out = apply_header_drop(
            &session,
            &rh,
            y_at(&session, &heights, 1, 0.0),
            &[],
            Some(1),
        )
        .unwrap();
        // B's child 5 moves to the top of the non-child ordering before A's children.
        assert_eq!(out.track_order, vec![0, 5, 1, 2, 3, 4]);
    }

    #[test]
    fn drop_moves_folder_below_a_track() {
        let session = session_with_two_folders();
        let heights = vec![DEFAULT_TRACK_HEIGHT; 6];
        let rh = row_heights(&heights, &session);
        // Drag folder A (children {1,3}) and drop after track 2 (row 3, frac 1.0).
        // Non-child tracks in order: 0, 2, 4, 5. Dropping after track 2 -> to = 2.
        let out = apply_header_drop(
            &session,
            &rh,
            y_at(&session, &heights, 3, 1.0),
            &[],
            Some(0),
        )
        .unwrap();
        // Children {1,3} as a block move after track 2: [0,2,1,3,4,5].
        assert_eq!(out.track_order, vec![0, 2, 1, 3, 4, 5]);
    }

    #[test]
    fn drop_below_last_row_clamps_to_last_track() {
        let session = session_with_two_folders();
        let heights = vec![DEFAULT_TRACK_HEIGHT; 6];
        let rh = row_heights(&heights, &session);
        // A y far below the last row must insert at the end, never wrap to 0.
        let out = apply_header_drop(&session, &rh, 10_000.0, &[0], None).unwrap();
        assert_eq!(out.track_order, vec![1, 2, 3, 4, 5, 0]);
    }

    #[test]
    fn clip_drop_track_clamps_above_and_below() {
        let session = session_with_two_folders();
        let heights = vec![DEFAULT_TRACK_HEIGHT; 6];
        // 6 tracks; folder A holds {1,3}, folder B holds {5}.
        // Above the first row -> track 0.
        assert_eq!(clip_drop_track(&heights, &session, -10.0, 4), 0);
        // Below the last row -> last track (5), never wraps to 0.
        assert_eq!(clip_drop_track(&heights, &session, 10_000.0, 4), 5);
        // On a folder header row -> fallback (keep the clip in place).
        let folder_y = row_y(&heights, &session, 1) + FOLDER_ROW_HEIGHT / 2.0;
        assert_eq!(clip_drop_track(&heights, &session, folder_y, 4), 4);
        // On a track row -> that track.
        let track2_y = row_y(&heights, &session, 3) + DEFAULT_TRACK_HEIGHT / 2.0;
        assert_eq!(clip_drop_track(&heights, &session, track2_y, 4), 2);
    }

    #[test]
    fn drag_ghost_height_matches_dragged_rows_and_folder() {
        let mut session = session_with_folders();
        // Folder "F" holds tracks {0, 2}; not collapsed.
        let heights = vec![40.0, 72.0, 144.0, 72.0];
        // A single track: its own height.
        assert_eq!(drag_ghost_height(&heights, &session, &[1], None), 72.0);
        // A group of tracks: the sum of their heights, window-independent.
        assert_eq!(
            drag_ghost_height(&heights, &session, &[0, 2], None),
            40.0 + 144.0
        );
        // A folder: header plus its visible children, not the whole timeline.
        assert_eq!(
            drag_ghost_height(&heights, &session, &[], Some(0)),
            FOLDER_ROW_HEIGHT + 40.0 + 144.0
        );
        // Collapsed folder: only the header row.
        session.folders[0].collapsed = true;
        assert_eq!(
            drag_ghost_height(&heights, &session, &[], Some(0)),
            FOLDER_ROW_HEIGHT
        );
    }
}
