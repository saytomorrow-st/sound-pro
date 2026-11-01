//! Sound Pro window (SPEC-015 to SPEC-021). All editing goes through
//! [`daw_gui::Editor`].

#![forbid(unsafe_code)]
// No console window behind the GUI in Windows release builds (SPEC-024).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio_editor;
mod shell;
#[cfg(target_os = "linux")]
mod x11_cursor;

use std::collections::HashMap;
use std::path::PathBuf;

use audio_core::{
    AutoPoint, Effect, EffectKind, EffectParam, Lane, MAX_BUSES, MAX_EFFECTS_PER_TRACK, Note,
    TRACK_COLORS, TrackFormat, TrackKind,
};
use daw_app::audio_settings::{AudioSettings, BUFFER_SIZES};
use daw_app::playback::PlaybackControl;
use daw_gui::layout::{Bottom, Layout, MeterState};
use daw_gui::timeline::{self, CycleHit};
use daw_gui::{
    Editor, EditorError, MAX_TEMPO, MIN_TEMPO, bars_in, clock, samples_to_x, sixteenth_samples,
    snap_to_beat, x_to_samples,
};
use eframe::egui::{
    self, Align2, Color32, CornerRadius, CursorIcon, FontId, Key, Pos2, Rect, Sense, Stroke,
    StrokeKind, Vec2, ViewportCommand,
};

const TRACK_HEIGHT: f32 = 72.0;
const RULER_HEIGHT: f32 = 24.0;
/// Distance in pixels from a clip boundary where the Fade tool makes a
/// crossfade instead of a fade (SPEC-043 §2).
const XFADE_GRAB: f32 = 6.0;
const MIN_ZOOM: f32 = 5.0;
const MAX_ZOOM: f32 = 2_000.0;
/// Width of the grab zone on each clip edge used for trimming.
const EDGE_PX: f32 = 6.0;
/// Piano roll: height of one key row and width of one 1/16 step.
const KEY_HEIGHT: f32 = 12.0;
const STEP_PX: f32 = 22.0;
const KEYS_PX: f32 = 44.0;
const PIANO_HEIGHT: f32 = 280.0;
/// Strength of notes drawn with the mouse.
const NEW_VELOCITY: u8 = 100;

/// A note being dragged in the piano roll.
#[derive(Clone, Copy)]
struct NoteDrag {
    index: usize,
    /// Pointer position minus note start, in samples, at the press.
    grab: i64,
}

const fn is_black_key(pitch: u8) -> bool {
    matches!(pitch % 12, 1 | 3 | 6 | 8 | 10)
}

/// What a drag on the ruler does (SPEC-040).
#[derive(Clone, Copy)]
enum RulerDrag {
    /// Upper strip: the playhead follows the pointer.
    Scrub,
    /// A new cycle from this anchor.
    New(u64),
    /// The whole cycle, as it was when the drag began, from this press.
    Move { start: u64, end: u64, from: u64 },
    /// One edge of the cycle.
    Resize(CycleHit),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DragMode {
    Move,
    TrimStart,
    TrimEnd,
    FadeIn,
    FadeOut,
}

/// Region edit tool (SPEC-038 §7).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tool {
    Pointer,
    Scissors,
    Glue,
    Fade,
}

impl Tool {
    pub(crate) const ALL: [Self; 4] = [Self::Pointer, Self::Scissors, Self::Glue, Self::Fade];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Pointer => "Стрелка",
            Self::Scissors => "Ножницы",
            Self::Glue => "Клей",
            Self::Fade => "Фейд",
        }
    }

    const fn next(self) -> Self {
        match self {
            Self::Pointer => Self::Scissors,
            Self::Scissors => Self::Glue,
            Self::Glue => Self::Fade,
            Self::Fade => Self::Pointer,
        }
    }
}

/// Size of the fade handles on a region's top corners, in pixels.
const FADE_HANDLE_PX: f32 = 14.0;

/// Drag mode for a press at `p` on a region drawn in `rect` with `tool`.
/// `None` means the tool does not drag.
/// `fades` are the fade-in and fade-out widths in pixels: a handle sits
/// where its ramp meets the top edge and is grabbed there (SPEC-040 §3).
/// `inside` tells whether the start and the end lie inside another clip:
/// such an edge is part of a crossfade and has no fade handle (SPEC-045 §5).
fn press_mode(
    tool: Tool,
    rect: Rect,
    p: Pos2,
    fades: (f32, f32),
    inside: (bool, bool),
) -> Option<DragMode> {
    let left_half = p.x < rect.center().x;
    match tool {
        Tool::Scissors | Tool::Glue => None,
        Tool::Fade => match (left_half, inside) {
            (true, (true, _)) | (false, (_, true)) => None,
            (true, _) => Some(DragMode::FadeIn),
            (false, _) => Some(DragMode::FadeOut),
        },
        Tool::Pointer => {
            let handle = FADE_HANDLE_PX.min(rect.width() / 3.0);
            let top = p.y <= rect.min.y + FADE_HANDLE_PX;
            let fade_in = rect.min.x + fades.0.min(rect.width());
            let fade_out = rect.max.x - fades.1.min(rect.width());
            if top && !inside.0 && p.x <= (rect.min.x + handle).max(fade_in + handle / 2.0) {
                Some(DragMode::FadeIn)
            } else if top && !inside.1 && p.x >= (rect.max.x - handle).min(fade_out - handle / 2.0)
            {
                Some(DragMode::FadeOut)
            } else {
                Some(edge_mode(rect, p.x))
            }
        }
    }
}

/// A clip being dragged with the mouse.
#[derive(Clone, Copy)]
struct Drag {
    track: usize,
    clip: usize,
    mode: DragMode,
    /// Pointer position minus clip start, in samples, at the press.
    grab: i64,
}

fn edge_mode(rect: Rect, x: f32) -> DragMode {
    let edge = EDGE_PX.min(rect.width() / 3.0);
    if x <= rect.min.x + edge {
        DragMode::TrimStart
    } else if x >= rect.max.x - edge {
        DragMode::TrimEnd
    } else {
        DragMode::Move
    }
}

/// Fade ramps on a region: shading over the faded part and the real gain
/// curve from the silent corner to full level (SPEC-045 §1); on the selected
/// or hovered region, handles on the top corners (SPEC-040 §3). An edge that
/// lies inside another clip belongs to a crossfade and gets no handle
/// (SPEC-045 §5). `(show, (start_inside, end_inside))`.
#[allow(clippy::cast_precision_loss)]
fn draw_fades(
    painter: &egui::Painter,
    rect: Rect,
    clip: &audio_core::session::Clip,
    px_per_sample: f32,
    (show, (start_inside, end_inside)): (bool, (bool, bool)),
) {
    let fade_in = (clip.fade_in_samples as f32 * px_per_sample).min(rect.width());
    let fade_out = (clip.fade_out_samples as f32 * px_per_sample).min(rect.width());
    if fade_in >= 1.0 {
        draw_fade_curve(painter, rect, rect.min.x, fade_in, clip.fade_in_curve);
    }
    if fade_out >= 1.0 {
        draw_fade_curve(painter, rect, rect.max.x, -fade_out, clip.fade_out_curve);
    }
    if show && rect.width() > FADE_HANDLE_PX * 2.0 {
        let handles = [
            (!start_inside, rect.min.x + fade_in),
            (!end_inside, rect.max.x - fade_out),
        ];
        for (_, x) in handles.into_iter().filter(|(on, _)| *on) {
            let x = x.clamp(rect.min.x + 4.0, rect.max.x - 4.0);
            let at = Pos2::new(x, rect.min.y + 4.0);
            painter.circle_filled(at, 4.0, Color32::WHITE);
            painter.circle_stroke(at, 4.0, Stroke::new(1.0_f32, Color32::from_gray(40)));
        }
    }
}

/// One fade ramp starting at the silent edge `x0` and spanning `width`
/// pixels (negative for a fade-out, which runs leftwards from the end).
/// The area above the curve is shaded in thin strips, so curved shapes
/// shade correctly where a convex polygon cannot.
#[allow(clippy::cast_precision_loss)]
fn draw_fade_curve(painter: &egui::Painter, rect: Rect, x0: f32, width: f32, curve: f32) {
    const STEPS: usize = 32;
    let shade = Color32::from_black_alpha(110);
    let line = Stroke::new(1.5_f32, Color32::from_gray(235));
    let y = |gain: f32| rect.max.y - gain * rect.height();
    let points: Vec<Pos2> = (0..=STEPS)
        .map(|i| {
            let f = i as f32 / STEPS as f32;
            Pos2::new(x0 + f * width, y(audio_core::shape_fade(f, curve)))
        })
        .collect();
    for pair in points.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let (lo, hi) = (a.x.min(b.x), a.x.max(b.x));
        let bottom = a.y.min(b.y);
        if bottom > rect.min.y {
            painter.rect_filled(
                Rect::from_min_max(Pos2::new(lo, rect.min.y), Pos2::new(hi, bottom)),
                0.0,
                shade,
            );
        }
    }
    painter.add(egui::Shape::line(points, line));
}

/// Width left for a track name once the header's right-hand buttons
/// (✕, FX, A and, when automation is open, the lane picker) are reserved,
/// so a long name is cut with "…" instead of running under them (SPEC-044 §4).
fn name_width(available: f32, lanes_open: bool) -> f32 {
    let reserved = if lanes_open { 210.0 } else { 90.0 };
    (available - reserved).max(24.0)
}

/// Paints an overlap of clips as one merged region with an X crossfade
/// (SPEC-044 §3): the clip borders disappear under its body, so neither
/// clip looks like it lies in front of the other.
fn draw_overlap_zone(
    painter: &egui::Painter,
    (start, end): (u64, u64),
    left: f32,
    top: f32,
    px_per_sample: f32,
    colors: &daw_gui::layout::RegionColors,
) -> Rect {
    let rgb = |c: [u8; 3]| Color32::from_rgb(c[0], c[1], c[2]);
    let x0 = left + start as f32 * px_per_sample;
    let x1 = (left + end as f32 * px_per_sample).max(x0 + 2.0);
    let rect = Rect::from_min_max(
        Pos2::new(x0, top + 3.0),
        Pos2::new(x1, top + TRACK_HEIGHT - 3.0),
    );
    let header_h = 14.0_f32.min(rect.height() * 0.4);
    let header = Rect::from_min_size(rect.min, Vec2::new(rect.width(), header_h));
    let body = Rect::from_min_max(Pos2::new(rect.min.x, header.max.y), rect.max);
    // The header keeps the top clip's name; only the body merges.
    painter.rect_filled(body, 0.0, rgb(colors.body).gamma_multiply(0.92));
    painter.rect_filled(body, 0.0, Color32::from_black_alpha(25));
    let edge = Stroke::new(1.0_f32, Color32::from_black_alpha(140));
    painter.hline(rect.x_range(), rect.min.y + 0.5, edge);
    painter.hline(rect.x_range(), rect.max.y - 0.5, edge);
    body
}

/// The thin X over a crossfade zone, drawn above the waveforms.
fn draw_crossfade_x(painter: &egui::Painter, body: Rect) {
    let line = Stroke::new(1.0_f32, Color32::from_white_alpha(170));
    painter.line_segment([body.left_bottom(), body.right_top()], line);
    painter.line_segment([body.left_top(), body.right_bottom()], line);
}

/// A bright crosshair plus a label at `p`, used by the drop debug overlay to
/// show where the OS cursor and egui's pointer each think the cursor is.
fn draw_cross(painter: &egui::Painter, p: egui::Pos2, color: Color32, label: &str) {
    let r = 12.0;
    let stroke = Stroke::new(2.0_f32, color);
    painter.line_segment([Pos2::new(p.x - r, p.y), Pos2::new(p.x + r, p.y)], stroke);
    painter.line_segment([Pos2::new(p.x, p.y - r), Pos2::new(p.x, p.y + r)], stroke);
    painter.circle_stroke(p, r, stroke);
    painter.text(
        Pos2::new(p.x + r + 3.0, p.y + r + 3.0),
        egui::Align2::LEFT_TOP,
        label,
        egui::FontId::proportional(14.0),
        color,
    );
}

fn rgb_detail(colors: &daw_gui::layout::RegionColors) -> Color32 {
    let [r, g, b] = colors.detail;
    Color32::from_rgb(r, g, b)
}

/// The waveform of `clip`: one wave for a mono source, left over right in two
/// halves of `body` for a stereo one (SPEC-050 §5).
fn draw_clip_wave(
    editor: &daw_gui::Editor,
    painter: &egui::Painter,
    rect: Rect,
    body: Rect,
    clip: &audio_core::session::Clip,
    detail: Color32,
) {
    if let Some((left, right)) = editor.stereo_peaks(clip) {
        let mid = body.center().y;
        let top = Rect::from_min_max(body.min, Pos2::new(body.max.x, mid));
        let bottom = Rect::from_min_max(Pos2::new(body.min.x, mid), body.max);
        draw_peaks(painter, rect, top, clip, left, detail);
        draw_peaks(painter, rect, bottom, clip, right, detail);
    } else if let Some(peaks) = editor.peaks(clip) {
        draw_peaks(painter, rect, body, clip, peaks, detail);
    }
}

/// The waveform of `clip` drawn into `body`; `rect` is the whole region.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "pixel columns and sample positions stay far below 2^24"
)]
fn draw_peaks(
    painter: &egui::Painter,
    rect: Rect,
    body: Rect,
    clip: &audio_core::session::Clip,
    peaks: &daw_gui::Peaks,
    detail: Color32,
) {
    let mid = body.center().y;
    let half = body.height() * 0.45;
    let visible = painter.clip_rect().intersect(rect);
    if visible.width() <= 0.0 {
        return;
    }
    let first = (visible.min.x - rect.min.x).floor().max(0.0) as usize;
    let last = (visible.max.x - rect.min.x).ceil().min(rect.width().ceil()) as usize;
    let per_px = clip.length_samples as f32 / rect.width();
    for px in first..last {
        let from = clip.offset_samples + (px as f32 * per_px) as u64;
        let to = clip.offset_samples + ((px + 1) as f32 * per_px).ceil() as u64;
        if let Some((lo, hi)) = peaks.range(from, to) {
            let x = rect.min.x + px as f32 + 0.5;
            painter.line_segment(
                [
                    Pos2::new(x, mid - hi.clamp(-1.0, 1.0) * half),
                    Pos2::new(x, mid - lo.clamp(-1.0, 1.0) * half + 1.0),
                ],
                Stroke::new(1.0_f32, detail),
            );
        }
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Sound Pro")
            .with_inner_size([1200.0, 700.0])
            .with_min_inner_size([640.0, 360.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    let start = std::env::args_os().nth(1).map(PathBuf::from);
    eframe::run_native(
        "Sound Pro",
        options,
        Box::new(move |_cc| Ok(Box::new(App::new(start)))),
    )
}

/// Whether a text field has the keyboard (SPEC-037). Any widget that was
/// clicked takes focus in egui, so plain focus would block every shortcut
/// after a click on a button or slider.
fn typing(ctx: &egui::Context) -> bool {
    ctx.memory(egui::Memory::focused)
        .is_some_and(|id| egui::text_edit::TextEditState::load(ctx, id).is_some())
}

/// A running playback: the control side and, with sound, the open stream.
struct Player {
    control: PlaybackControl,
    #[cfg(feature = "audio")]
    _stream: daw_app::realtime::RealtimeHandle,
    /// The project as it sounds now; an edit is swapped in while playing
    /// (SPEC-035).
    played: audio_core::session::Session,
    /// When the project last differed from `played`.
    changed: Option<std::time::Instant>,
}

/// Pause after the last edit before the new sound is built, so a drag does
/// not rebuild it every frame.
const REFRESH_DELAY: std::time::Duration = std::time::Duration::from_millis(120);

/// A file decoded on a background thread (SPEC-035): path, target track,
/// start, and the channels at the project rate (SPEC-050 §5).
type HoverBlocks = (PathBuf, usize, Vec<(f32, f32)>);

// X11 cursor position in physical pixels, read via XQueryPointer. A type alias
// keeps the non-Linux drop-debug fallbacks (which always produce `None`) from
// tripping clippy::type_complexity on the Windows/macOS cfg branch.
#[cfg(not(target_os = "linux"))]
type CursorWinPx = Option<(i16, i16)>;

type Decoded = (
    PathBuf,
    Option<usize>,
    Option<u64>,
    Result<daw_app::audio_file::Channels, EditorError>,
);

/// What a decoding thread sends: waveform blocks as they become known, then
/// the finished file.
enum ImportMsg {
    /// Peak blocks `first..` of the file at `path`.
    Blocks(PathBuf, usize, Vec<(f32, f32)>),
    Done(Decoded),
}

/// The result of a background mix export (SPEC-052): the destination path,
/// the sample rate it was written at, and either the render report or the
/// error that stopped it.
enum ExportMsg {
    Done(PathBuf, u32, Result<daw_app::SessionReport, EditorError>),
}

/// A region shown while its file still decodes (SPEC-050 §7): its length
/// comes from the header, its waveform fills in as the decoder advances.
struct Ghost {
    path: PathBuf,
    /// Row it is drawn on; equal to the track count for a new track.
    row: usize,
    start: u64,
    clip: audio_core::session::Clip,
    peaks: daw_gui::Peaks,
}

/// Files being decoded in the background, in the order they were asked for.
struct Imports {
    tx: std::sync::mpsc::Sender<ImportMsg>,
    rx: std::sync::mpsc::Receiver<ImportMsg>,
    ghosts: Vec<Ghost>,
    /// Files still decoding.
    pending: usize,
    /// Track the next file of the same drop goes to.
    chain: Option<usize>,
    added: usize,
    /// Files asked for since the queue was last empty (SPEC-050 §6).
    total: usize,
}

/// A microphone take in progress (SPEC-020).
#[cfg(feature = "audio")]
struct Recorder {
    control: daw_app::capture::CaptureControl,
    _stream: daw_app::realtime::RealtimeHandle,
    /// Timeline position where the take starts, in project samples.
    start: u64,
}

/// Region menu command chosen in this frame (SPEC-037).
#[derive(Clone, Copy)]
enum RegionAction {
    Piano,
    Copy,
    Paste,
    Duplicate,
    Split,
    Delete,
    FadeIn,
    FadeOut,
    NoFades,
    Join,
}

/// Track header command chosen in this frame (SPEC-029).
#[derive(Clone, Copy)]
enum TrackAction {
    Rename(usize),
    Color(usize, Option<u8>),
    Move(usize, usize),
    Duplicate(usize),
    New(TrackKind),
}

/// «New track» items of the track-header menus (SPEC-050 §4).
fn new_track_items(ui: &mut egui::Ui, action: &mut Option<TrackAction>) {
    if ui.button("Новая дорожка…").clicked() {
        *action = Some(TrackAction::New(TrackKind::Audio));
        ui.close();
    }
    if ui.button("Новая MIDI-дорожка").clicked() {
        *action = Some(TrackAction::New(TrackKind::Instrument));
        ui.close();
    }
}

/// Display colour of the track at `index`; tracks without their own colour
/// take one from the palette by position (SPEC-029, SPEC-036).
fn track_color(color: Option<u8>, index: usize) -> Color32 {
    const PALETTE: [Color32; TRACK_COLORS as usize] = [
        Color32::from_rgb(232, 82, 76),
        Color32::from_rgb(240, 146, 54),
        Color32::from_rgb(232, 200, 60),
        Color32::from_rgb(98, 196, 92),
        Color32::from_rgb(58, 196, 190),
        Color32::from_rgb(70, 146, 236),
        Color32::from_rgb(150, 110, 230),
        Color32::from_rgb(226, 96, 176),
    ];
    PALETTE[usize::from(daw_gui::layout::display_color(color, index)) % PALETTE.len()]
}

struct App {
    editor: Editor,
    /// Frames drawn so far; used by the `SOUND_PRO_SCREENSHOT` self-snapshot.
    frames: u32,
    #[cfg(feature = "audio")]
    recorder: Option<Recorder>,
    /// Playhead in samples while stopped; the player's position while playing.
    playhead: u64,
    player: Option<Player>,
    imports: Imports,
    /// Mix-export progress and options (SPEC-052).
    export_tx: std::sync::mpsc::Sender<ExportMsg>,
    export_rx: std::sync::mpsc::Receiver<ExportMsg>,
    export_open: bool,
    export_bits: audio_core::wav::BitDepth,
    export_rate: u32,
    export_normalize: bool,
    export_target_db: f32,
    export_pending: bool,
    /// File lengths read from headers while files hover (SPEC-050 §2).
    probes: HashMap<PathBuf, Result<u64, String>>,
    /// Waveforms of hovered files, filling in as they decode.
    hover_peaks: HashMap<PathBuf, daw_gui::Peaks>,
    hover_tx: std::sync::mpsc::Sender<HoverBlocks>,
    hover_rx: std::sync::mpsc::Receiver<HoverBlocks>,
    /// Last pointer position over the window; a drop no longer has one.
    drop_tracker: daw_gui::DropTracker,
    /// OS cursor query used at drop time (X11 only): winit drops
    /// `XdndPosition` during an external drag, so egui's pointer is stale.
    #[cfg(target_os = "linux")]
    x11_cursor: Option<x11_cursor::X11Cursor>,
    /// Draw the XQueryPointer / egui pointer debug crosses while a file is
    /// hovered (set `SOUND_PRO_DROP_DEBUG=1`). Read once at startup.
    drop_debug: bool,
    status: String,
    /// Timeline zoom in pixels per second.
    zoom: f32,
    /// Zoom the timeline was last drawn at; a difference means a zoom to
    /// apply around `zoom_anchor` (SPEC-037).
    drawn_zoom: f32,
    /// Point to keep still, in px from the left of the visible timeline.
    zoom_anchor: Option<f32>,
    /// Track to scroll into view on the next frame (SPEC-037).
    reveal_track: Option<usize>,
    /// Shown level of each track, by track index (SPEC-042).
    track_meters: Vec<MeterState>,
    /// Shown gain reduction, dB, of every insert of every track (SPEC-052).
    track_reduction: Vec<[f32; MAX_EFFECTS_PER_TRACK]>,
    /// Shown level of the master (SPEC-042).
    master_meter: MeterState,
    /// Horizontal scroll offset and width of the visible timeline.
    timeline_view: (f32, f32),
    title: String,
    confirm_quit: bool,
    /// Snap clip edits to beats (Shift turns it off while dragging).
    snap: bool,
    drag: Option<Drag>,
    /// Rubber band on empty timeline space: where the press began and
    /// whether it adds to the selection (SPEC-047 §2).
    band: Option<(Pos2, bool)>,
    /// Active region tool (SPEC-038).
    pub(crate) tool: Tool,
    /// MIDI clip open in the piano roll.
    piano: Option<(usize, usize)>,
    /// Audio clip editor (SPEC-039).
    audio_edit: audio_editor::AudioEdit,
    note_drag: Option<NoteDrag>,
    /// Scroll the piano roll to middle C on the next frame.
    piano_scroll: bool,
    /// Horizontal zoom of the piano roll and the offset to apply on the
    /// next frame so the time under the pointer stays put (SPEC-040 §5).
    piano_zoom: f32,
    piano_offset: Option<f32>,
    /// Track or bus whose effects window is open (SPEC-022, SPEC-028).
    fx: Option<FxTarget>,
    /// The buses window is open (SPEC-028).
    buses_open: bool,
    /// Bus whose strip the inspector shows on the right (SPEC-043 §5).
    selected_send: Option<usize>,
    /// Track whose name is being edited and the text typed so far (SPEC-029).
    rename: Option<(usize, String)>,
    /// Track whose automation lane is shown (SPEC-023).
    auto: Option<usize>,
    auto_lane: Lane,
    /// Index of the automation point being dragged.
    auto_drag: Option<usize>,
    /// What a drag on the ruler is doing (SPEC-025, SPEC-040).
    cycle_drag: Option<RulerDrag>,
    /// Marker being dragged and where it would land (SPEC-030).
    marker_drag: Option<(usize, u64)>,
    /// Marker whose name is being edited and the text typed so far (SPEC-030).
    marker_rename: Option<(usize, String)>,
    /// Sound devices and buffer size (SPEC-033).
    audio: AudioSettings,
    /// Where `audio` is saved; `None` when no config folder is known.
    audio_path: Option<PathBuf>,
    /// The audio settings window is open, with the device names last listed.
    audio_open: bool,
    about_open: bool,
    /// The «Новая дорожка» dialog, when open (SPEC-049).
    new_track: Option<shell::NewTrack>,
    outputs: Vec<String>,
    inputs: Vec<String>,
    /// Visible panels of the main window (SPEC-034).
    layout: Layout,
    themed: bool,
}

/// What the user asked to do with a marker flag (SPEC-030).
enum MarkerAction {
    Seek(u64),
    Move(usize, u64),
    Rename(usize),
    Delete(usize),
}

/// Device picker: "Системный" plus every listed name. A saved device that
/// is not plugged in stays visible, marked as missing.
fn device_combo(ui: &mut egui::Ui, id: &str, value: &mut Option<String>, names: &[String]) {
    let shown = match value.as_deref() {
        None => "Системный".to_owned(),
        Some(name) if names.iter().any(|n| n == name) => name.to_owned(),
        Some(name) => format!("{name} (не найдено)"),
    };
    egui::ComboBox::from_id_salt(id)
        .width(260.0)
        .selected_text(shown)
        .show_ui(ui, |ui| {
            ui.selectable_value(value, None, "Системный");
            for name in names {
                ui.selectable_value(value, Some(name.clone()), name);
            }
        });
}

fn buffer_label(frames: u32, rate: u32) -> String {
    format!(
        "{frames} ({:.1} мс)",
        daw_app::audio_settings::buffer_ms(frames, rate)
    )
}

impl App {
    fn new(start: Option<PathBuf>) -> Self {
        let (hover_tx, hover_rx) = std::sync::mpsc::channel();
        let (export_tx, export_rx) = std::sync::mpsc::channel();
        let mut app = Self {
            editor: Editor::new(),
            #[cfg(feature = "audio")]
            recorder: None,
            playhead: 0,
            imports: {
                let (tx, rx) = std::sync::mpsc::channel();
                Imports {
                    tx,
                    rx,
                    ghosts: Vec::new(),
                    pending: 0,
                    chain: None,
                    added: 0,
                    total: 0,
                }
            },
            export_tx,
            export_rx,
            export_open: false,
            export_bits: audio_core::wav::BitDepth::I16,
            export_rate: 48_000,
            export_normalize: false,
            export_target_db: -1.0,
            export_pending: false,
            probes: HashMap::new(),
            hover_peaks: HashMap::new(),
            hover_tx,
            hover_rx,
            drop_tracker: daw_gui::DropTracker::default(),
            #[cfg(target_os = "linux")]
            x11_cursor: None,
            drop_debug: std::env::var_os("SOUND_PRO_DROP_DEBUG").is_some(),
            player: None,
            status: "Готово. Файл → Импорт WAV…, чтобы добавить звук.".into(),
            zoom: 100.0,
            drawn_zoom: 100.0,
            zoom_anchor: None,
            reveal_track: None,
            track_meters: Vec::new(),
            track_reduction: Vec::new(),
            master_meter: MeterState::default(),
            timeline_view: (0.0, 0.0),
            title: String::new(),
            confirm_quit: false,
            snap: true,
            drag: None,
            band: None,
            tool: Tool::Pointer,
            piano: None,
            audio_edit: audio_editor::AudioEdit::default(),
            note_drag: None,
            piano_scroll: false,
            piano_zoom: 1.0,
            piano_offset: None,
            fx: None,
            buses_open: false,
            selected_send: None,
            rename: None,
            auto: None,
            auto_lane: Lane::Gain,
            auto_drag: None,
            cycle_drag: None,
            marker_drag: None,
            marker_rename: None,
            audio: AudioSettings::default(),
            audio_path: daw_app::audio_settings::default_path(),
            audio_open: false,
            about_open: false,
            new_track: None,
            outputs: Vec::new(),
            inputs: Vec::new(),
            layout: Layout::default(),
            themed: false,
            frames: 0,
        };
        if let Some(path) = &app.audio_path {
            app.audio = AudioSettings::load(path);
        }
        if let Some(path) = start {
            app.open(path);
        }
        app
    }

    fn report<T, E: std::fmt::Display>(&mut self, result: Result<T, E>, ok: &str) -> Option<T> {
        match result {
            Ok(value) => {
                self.status = ok.to_owned();
                Some(value)
            }
            Err(e) => {
                self.status = format!("Ошибка: {e}");
                None
            }
        }
    }

    fn is_playing(&self) -> bool {
        self.player.as_ref().is_some_and(|p| p.control.is_playing())
    }

    fn play(&mut self) {
        self.player = None;
        let started = self
            .editor
            .playback(self.playhead)
            .map(|(mut control, engine)| {
                control.play();
                (control, engine)
            });
        let Some((control, engine)) = self.report(started, "Играет") else {
            return;
        };
        self.playhead = control.position();
        #[cfg(feature = "audio")]
        {
            let rate = control.sample_rate();
            match daw_app::realtime::start_output_with(engine, rate, &self.audio) {
                Ok(stream) => {
                    self.player = Some(Player {
                        control,
                        _stream: stream,
                        played: self.editor.session().clone(),
                        changed: None,
                    });
                }
                Err(e) => self.status = format!("Ошибка звука: {e}"),
            }
        }
        #[cfg(not(feature = "audio"))]
        {
            drop((control, engine));
            self.status = "Звук выключен в этой сборке (соберите с --features audio)".into();
        }
    }

    fn pause(&mut self) {
        if let Some(player) = self.player.take() {
            self.playhead = player.control.position();
        }
        self.status = "Пауза".into();
    }

    #[cfg(feature = "audio")]
    fn is_recording(&self) -> bool {
        self.recorder.is_some()
    }

    #[cfg(not(feature = "audio"))]
    #[allow(clippy::unused_self)]
    const fn is_recording(&self) -> bool {
        false
    }

    fn toggle_record(&mut self) {
        if self.is_recording() {
            self.finish_recording();
        } else {
            self.start_recording();
        }
    }

    #[cfg(feature = "audio")]
    fn start_recording(&mut self) {
        // SPEC-045 §4: only an armed track records.
        if self.editor.armed_track().is_none() {
            self.status = "Включите R (готовность к записи) на дорожке".into();
            return;
        }
        let rate = self.editor.session().sample_rate;
        match daw_app::realtime::start_input_with(rate, 30, &self.audio) {
            Ok((control, stream)) => {
                let start = self.playhead;
                if !self.editor.session().tracks.is_empty() {
                    self.play();
                }
                self.recorder = Some(Recorder {
                    control,
                    _stream: stream,
                    start,
                });
                self.status = "Запись…".into();
            }
            Err(e) => self.status = format!("Ошибка микрофона: {e}"),
        }
    }

    #[cfg(not(feature = "audio"))]
    fn start_recording(&mut self) {
        self.status = "Звук выключен в этой сборке (соберите с --features audio)".into();
    }

    #[cfg(feature = "audio")]
    fn finish_recording(&mut self) {
        let Some(recorder) = self.recorder.take() else {
            return;
        };
        drop(recorder._stream);
        if let Some(player) = self.player.take() {
            self.playhead = player.control.position();
        }
        let take = recorder.control.finish();
        let dir = self.editor.recordings_dir();
        let added =
            self.editor
                .add_recording(&take.samples, take.sample_rate, recorder.start, &dir);
        #[allow(clippy::cast_precision_loss)]
        let seconds = take.samples.len() as f64 / f64::from(take.sample_rate.max(1));
        let mut ok = format!("Записано {seconds:.1} с");
        if take.dropped > 0 {
            ok.push_str(&format!(" (потеряно кадров: {})", take.dropped));
        }
        self.report(added, &ok);
    }

    #[cfg(not(feature = "audio"))]
    #[allow(clippy::unused_self)]
    const fn finish_recording(&mut self) {}

    /// Moves the take into the window's buffer and the playhead forward.
    #[cfg(feature = "audio")]
    fn poll_recorder(&mut self, ctx: &egui::Context) {
        let Some(recorder) = self.recorder.as_mut() else {
            return;
        };
        recorder.control.drain();
        if self.player.is_none() {
            let project = u64::from(self.editor.session().sample_rate);
            let rate = u64::from(recorder.control.sample_rate().max(1));
            self.playhead = recorder.start + recorder.control.frames() * project / rate;
        }
        ctx.request_repaint();
    }

    #[cfg(not(feature = "audio"))]
    #[allow(clippy::unused_self)]
    const fn poll_recorder(&mut self, _ctx: &egui::Context) {}

    fn toggle_play(&mut self) {
        if self.is_recording() {
            self.finish_recording();
            return;
        }
        if self.is_playing() {
            self.pause();
        } else {
            self.play();
        }
    }

    /// First press pauses; pressing again while stopped goes to the start.
    fn stop(&mut self) {
        if self.is_recording() {
            self.finish_recording();
        } else if self.is_playing() {
            self.pause();
        } else {
            self.player = None;
            self.playhead = 0;
            self.status = "Стоп".into();
        }
    }

    fn rewind(&mut self) {
        self.seek(0);
    }

    fn seek(&mut self, samples: u64) {
        let end = self.editor.session().length_samples();
        let samples = samples.min(end);
        match self.player.as_mut() {
            Some(player) if player.control.is_playing() => {
                player.control.seek(samples);
            }
            _ => {
                self.player = None;
                self.playhead = samples;
            }
        }
    }

    /// Follows the audio thread; drops a player that reached the end.
    fn poll_player(&mut self, ctx: &egui::Context) {
        let Some(player) = self.player.as_ref() else {
            return;
        };
        self.playhead = player.control.position();
        if player.control.is_playing() {
            ctx.request_repaint();
        } else {
            self.player = None;
            if !self.is_recording() {
                self.playhead = self.editor.active_cycle().map_or(0, |(from, _)| from);
                self.status = "Конец".into();
            }
        }
    }

    /// Reads the levels the audio thread measured since the last frame and
    /// lets the meters fall after a stop (SPEC-042).
    fn poll_meters(&mut self, ctx: &egui::Context) {
        let dt = ctx.input(|i| i.stable_dt).min(0.1);
        let count = self.editor.session().tracks.len();
        self.track_meters.resize(count, MeterState::default());
        let meters = self.editor.meters();
        for (index, meter) in self.track_meters.iter_mut().enumerate() {
            meter.update(meters.take_track(index), dt);
        }
        self.master_meter.update(meters.take_master(), dt);
        self.track_reduction
            .resize(count, [0.0; MAX_EFFECTS_PER_TRACK]);
        for (track, shown) in self.track_reduction.iter_mut().enumerate() {
            for (slot, value) in shown.iter_mut().enumerate() {
                *value = reduction_step(*value, meters.take_reduction(track, slot), dt);
            }
        }
        let live = !self.master_meter.is_idle()
            || self.track_meters.iter().any(|m| !m.is_idle())
            || self
                .track_reduction
                .iter()
                .any(|shown| shown.iter().any(|&value| value > 0.0));
        if live && self.player.is_none() {
            ctx.request_repaint();
        }
    }

    /// Swaps an edited project into the running player (SPEC-035): volume,
    /// pan, notes and clips change while it keeps playing. A new length
    /// restarts the player at the same spot.
    fn refresh_player(&mut self, ctx: &egui::Context) {
        let Some(player) = self.player.as_mut() else {
            return;
        };
        if !player.control.is_playing() || *self.editor.session() == player.played {
            player.changed = None;
            return;
        }
        let now = std::time::Instant::now();
        let since = *player.changed.get_or_insert(now);
        if now.duration_since(since) < REFRESH_DELAY {
            ctx.request_repaint_after(REFRESH_DELAY);
            return;
        }
        let session = self.editor.session().clone();
        if session.length_samples() != player.control.end()
            || session.sample_rate != player.played.sample_rate
        {
            self.playhead = player.control.position();
            self.play();
            return;
        }
        match self.editor.playback_graph() {
            Ok((graph, _)) => {
                if player.control.replace_graph(graph).is_ok() {
                    player.played = session;
                    player.changed = None;
                } else {
                    ctx.request_repaint_after(REFRESH_DELAY);
                }
            }
            Err(e) => {
                player.played = session;
                player.changed = None;
                self.status = format!("Ошибка: {e}");
            }
        }
    }

    /// Decodes audio files on a background thread so the window does not
    /// freeze on a long MP3 (SPEC-035). The first file goes to `track` at
    /// `start`; the others follow it on the same track.
    fn import_in_background(
        &mut self,
        paths: Vec<PathBuf>,
        track: Option<usize>,
        start: Option<u64>,
    ) {
        let paths: Vec<PathBuf> = paths
            .into_iter()
            .filter(|p| daw_app::audio_file::is_audio_file(p))
            .collect();
        if paths.is_empty() {
            self.status = "Можно добавлять только WAV, MP3, FLAC и OGG".into();
            return;
        }
        if self.imports.pending == 0 {
            self.imports.chain = track;
            self.imports.added = 0;
            self.imports.total = 0;
        }
        self.imports.pending += paths.len();
        self.imports.total += paths.len();
        self.status = format!(
            "Загрузка {}/{}…",
            self.imports.total - self.imports.pending,
            self.imports.total
        );
        let rate = self.editor.session().sample_rate;
        // Files a clip already uses are not decoded again (SPEC-050 §6).
        let cached: Vec<_> = paths.iter().map(|p| self.editor.cached_source(p)).collect();
        let cache = self.editor.pcm_cache();
        // Regions appear at once, sized from the headers.
        let mut lens = Vec::with_capacity(paths.len());
        let mut row = track.or(self.imports.chain);
        let mut at = start;
        for path in &paths {
            let len = daw_gui::probe_len(path, rate).ok();
            lens.push(len);
            if let Some(len) = len {
                let session = self.editor.session();
                let row_index = row.unwrap_or(session.tracks.len());
                let begin = at.unwrap_or_else(|| {
                    self.imports
                        .ghosts
                        .iter()
                        .rfind(|g| g.row == row_index)
                        .map(|g| g.start + g.clip.length_samples)
                        .or_else(|| session.tracks.get(row_index).map(|t| t.length_samples()))
                        .unwrap_or(0)
                });
                let mut clip = audio_core::session::Clip::full(begin, len);
                clip.name = path
                    .file_stem()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                self.imports.ghosts.push(Ghost {
                    path: path.clone(),
                    row: row_index,
                    start: begin,
                    clip,
                    peaks: daw_gui::Peaks::empty(len),
                });
                row = Some(row_index);
                at = Some(begin + len);
            }
        }
        let tx = self.imports.tx.clone();
        std::thread::spawn(move || {
            for (i, ((path, cached), len)) in paths.into_iter().zip(cached).zip(lens).enumerate() {
                let samples = match cached {
                    Some(c) => Ok(c),
                    None => {
                        let template = len.map(daw_gui::Peaks::empty);
                        let mut sent = 0;
                        let mut last = std::time::Instant::now();
                        let progress_tx = tx.clone();
                        let progress_path = path.clone();
                        daw_gui::read_source_cached(&path, rate, cache.as_ref(), &mut |from, ch| {
                            let (Some(template), Some(left)) = (&template, ch.first()) else {
                                return;
                            };
                            if last.elapsed() < std::time::Duration::from_millis(100) {
                                return;
                            }
                            last = std::time::Instant::now();
                            let blocks = template.blocks_from(left, from, rate, sent);
                            if blocks.is_empty() {
                                return;
                            }
                            let first = sent;
                            sent += blocks.len();
                            let _ = progress_tx.send(ImportMsg::Blocks(
                                progress_path.clone(),
                                first,
                                blocks,
                            ));
                        })
                    }
                };
                let at = if i == 0 { start } else { None };
                let first = i == 0;
                if tx
                    .send(ImportMsg::Done((
                        path,
                        first.then_some(track).flatten(),
                        at,
                        samples,
                    )))
                    .is_err()
                {
                    return;
                }
            }
        });
    }

    /// Regions of files still decoding, with the waveform decoded so far
    /// (SPEC-050 §7).
    #[allow(clippy::cast_precision_loss)]
    fn draw_ghosts(&self, painter: &egui::Painter, area: Rect) {
        let rate = self.editor.session().sample_rate;
        let px_per_sample = self.zoom / rate as f32;
        for g in &self.imports.ghosts {
            let top = area.min.y + RULER_HEIGHT + g.row as f32 * TRACK_HEIGHT;
            let left = area.min.x + g.start as f32 * px_per_sample;
            let w = (g.clip.length_samples as f32 * px_per_sample).max(2.0);
            let rect =
                Rect::from_min_size(Pos2::new(left, top + 3.0), Vec2::new(w, TRACK_HEIGHT - 6.0));
            let base = track_color(None, g.row);
            let colors = daw_gui::layout::region_colors([base.r(), base.g(), base.b()], false);
            let [r, gr, b] = colors.body;
            painter.rect_filled(rect, 4.0, Color32::from_rgb(r, gr, b).gamma_multiply(0.6));
            painter.rect_stroke(
                rect,
                4.0,
                Stroke::new(1.0_f32, Color32::from_gray(160)),
                StrokeKind::Inside,
            );
            draw_peaks(painter, rect, rect, &g.clip, &g.peaks, rgb_detail(&colors));
        }
    }

    /// Puts decoded files on the timeline as they arrive.
    fn poll_imports(&mut self, ctx: &egui::Context) {
        if self.imports.pending == 0 {
            return;
        }
        while let Ok(msg) = self.imports.rx.try_recv() {
            let (path, track, start, samples) = match msg {
                ImportMsg::Blocks(path, first, blocks) => {
                    if let Some(g) = self.imports.ghosts.iter_mut().find(|g| g.path == path) {
                        g.peaks.set_blocks(first, &blocks);
                    }
                    continue;
                }
                ImportMsg::Done(done) => done,
            };
            if let Some(i) = self.imports.ghosts.iter().position(|g| g.path == path) {
                self.imports.ghosts.remove(i);
            }
            self.imports.pending -= 1;
            let target = track.or(self.imports.chain);
            let result = samples.and_then(|s| self.editor.import_channels(&path, s, target, start));
            match result {
                Ok((t, _)) => {
                    self.imports.chain = Some(t);
                    self.imports.added += 1;
                    self.status = if self.imports.pending > 0 {
                        format!(
                            "Загрузка {}/{}…",
                            self.imports.total - self.imports.pending,
                            self.imports.total
                        )
                    } else {
                        format!("Добавлено файлов: {}", self.imports.added)
                    };
                }
                Err(e) => self.status = format!("Ошибка ({}): {e}", path.display()),
            }
        }
        if self.imports.pending > 0 {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
    }

    /// Passes the current cycle to a running player.
    fn sync_cycle(&mut self) {
        let cycle = self.editor.active_cycle();
        if let Some(player) = self.player.as_mut() {
            player.control.set_cycle(cycle);
        }
    }

    /// Passes the metronome switch, tempo and meter to a running player;
    /// cheap, so it runs every frame and also follows undo (SPEC-026).
    fn sync_metronome(&mut self) {
        let session = self.editor.session();
        let (tempo, beats) = (
            session.tempo_bpm,
            u32::from(session.time_signature.numerator),
        );
        let on = self.editor.metronome;
        if let Some(player) = self.player.as_mut() {
            player.control.set_metronome(on);
            player.control.set_meter(tempo, beats);
        }
    }

    fn toggle_metronome(&mut self) {
        self.editor.metronome = !self.editor.metronome;
        self.status = if self.editor.metronome {
            "Метроном включён".into()
        } else {
            "Метроном выключен".into()
        };
        self.sync_metronome();
    }

    fn toggle_cycle(&mut self) {
        if self.editor.cycle().is_none() {
            self.status = "Протяните мышью по линейке, чтобы задать цикл".into();
            return;
        }
        self.editor.cycle_enabled = !self.editor.cycle_enabled;
        self.status = if self.editor.cycle_enabled {
            "Цикл включён".into()
        } else {
            "Цикл выключен".into()
        };
        self.sync_cycle();
    }

    fn reset_playback(&mut self) {
        self.player = None;
        self.playhead = 0;
    }

    /// Tempo and time signature fields of the transport bar (SPEC-026).
    pub(crate) fn tempo_field(&mut self, ui: &mut egui::Ui) {
        let mut tempo = self.editor.session().tempo_bpm;
        let response = ui
            .add(
                egui::DragValue::new(&mut tempo)
                    .range(MIN_TEMPO..=MAX_TEMPO)
                    .speed(0.5)
                    .max_decimals(2),
            )
            .on_hover_text("Темп: тяните или дважды щёлкните для ввода. Правый клик — пресеты.");
        if response.drag_started() {
            self.editor.begin_group();
        }
        if response.changed() {
            self.editor.set_tempo(tempo);
        }
        if response.drag_stopped() {
            self.editor.end_group();
        }
        response.context_menu(|ui| {
            for preset in [60.0, 90.0, 100.0, 120.0, 140.0] {
                if ui.button(format!("{preset} BPM")).clicked() {
                    self.editor.set_tempo(preset);
                    ui.close();
                }
            }
            ui.separator();
            for stub in ["Удвоить темп", "Половина темпа", "Подстучать темп (Tap)"]
            {
                ui.add_enabled(false, egui::Button::new(stub))
                    .on_disabled_hover_text("Скоро");
            }
        });
        self.sync_metronome();
    }

    /// Time signature: numerator and denominator are both picked from a list.
    pub(crate) fn meter_field(&mut self, ui: &mut egui::Ui) {
        let signature = self.editor.session().time_signature;
        let mut numerator = signature.numerator;
        let mut denominator = signature.denominator;
        ui.spacing_mut().item_spacing.x = 2.0;
        ui.spacing_mut().combo_width = 34.0;
        egui::ComboBox::from_id_salt("numerator")
            .width(34.0)
            .selected_text(numerator.to_string())
            .show_ui(ui, |ui| {
                for n in 1u8..=16 {
                    ui.selectable_value(&mut numerator, n, n.to_string());
                }
            })
            .response
            .on_hover_text("Долей в такте");
        ui.label("/");
        let response = egui::ComboBox::from_id_salt("denominator")
            .width(34.0)
            .selected_text(denominator.to_string())
            .show_ui(ui, |ui| {
                for d in [2u8, 4, 8, 16] {
                    ui.selectable_value(&mut denominator, d, d.to_string());
                }
            })
            .response
            .on_hover_text("Длительность доли. Правый клик — частые размеры.");
        response.context_menu(|ui| {
            for (n, d) in [(2u8, 4u8), (3, 4), (4, 4), (5, 4), (6, 8), (7, 8), (12, 8)] {
                if ui.button(format!("{n}/{d}")).clicked() {
                    (numerator, denominator) = (n, d);
                    ui.close();
                }
            }
        });
        if (numerator, denominator) != (signature.numerator, signature.denominator) {
            let result = self.editor.set_time_signature(numerator, denominator);
            self.report(result, &format!("Размер {numerator}/{denominator}"));
        }
        self.sync_metronome();
    }

    fn open(&mut self, path: PathBuf) {
        self.reset_playback();
        let result = self.editor.open(&path);
        self.report(result, &format!("Открыт {}", path.display()));
    }

    fn open_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Проект Sound Pro", &["json"])
            .pick_file()
        {
            self.open(path);
        }
    }

    fn save(&mut self) {
        match self.editor.path().map(std::path::Path::to_path_buf) {
            Some(path) => self.save_to(path),
            None => self.save_as_dialog(),
        }
    }

    fn save_as_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Проект Sound Pro", &["json"])
            .set_file_name("song.json")
            .save_file()
        {
            self.save_to(path);
        }
    }

    fn save_to(&mut self, path: PathBuf) {
        let result = self.editor.save_as(&path);
        self.report(result, &format!("Сохранён {}", path.display()));
    }

    fn import_dialog(&mut self) {
        let Some(paths) = rfd::FileDialog::new()
            .add_filter(
                "WAV, MP3, FLAC, OGG",
                &[
                    "wav", "WAV", "mp3", "MP3", "flac", "FLAC", "ogg", "OGG", "oga", "OGA",
                ],
            )
            .pick_files()
        else {
            return;
        };
        let track = self.editor.selected_track;
        self.import_in_background(paths, track, None);
    }

    /// Opens the mix-export options window (SPEC-052). The project's sample
    /// rate is the initial target rate.
    fn export_dialog(&mut self) {
        self.export_rate = self.editor.session().sample_rate;
        self.export_open = true;
    }

    /// The mix-export window: bit depth, target sample rate, optional peak
    /// normalisation, and the button that starts the background render.
    fn export_window(&mut self, ctx: &egui::Context) {
        if !self.export_open {
            return;
        }
        let mut open = true;
        egui::Window::new("Экспорт сведения")
            .id(egui::Id::new("export"))
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .default_width(360.0)
            .show(ctx, |ui| self.export_ui(ui));
        if !open {
            self.export_open = false;
        }
    }

    fn export_ui(&mut self, ui: &mut egui::Ui) {
        egui::Grid::new("export-grid")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                ui.label("Разрядность");
                egui::ComboBox::from_id_salt("export-bits")
                    .selected_text(self.export_bits.bits().to_string())
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut self.export_bits,
                            audio_core::wav::BitDepth::I16,
                            "16 бит",
                        );
                        ui.selectable_value(
                            &mut self.export_bits,
                            audio_core::wav::BitDepth::I24,
                            "24 бит",
                        );
                    });
                ui.end_row();
                ui.label("Частота");
                egui::ComboBox::from_id_salt("export-rate")
                    .selected_text(format!("{} Гц", self.export_rate))
                    .show_ui(ui, |ui| {
                        for &rate in &[44_100, 48_000, 88_200, 96_000] {
                            ui.selectable_value(&mut self.export_rate, rate, format!("{rate} Гц"));
                        }
                    });
                ui.end_row();
                ui.label("Нормализация");
                ui.checkbox(&mut self.export_normalize, "пик до заданного dBFS");
                ui.end_row();
            });
        if self.export_normalize {
            ui.horizontal(|ui| {
                ui.label("Пик (dBFS)");
                ui.add(egui::DragValue::new(&mut self.export_target_db).range(-60.0..=0.0));
            });
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let enabled = !self.export_pending;
            if ui
                .add_enabled(enabled, egui::Button::new("Экспорт…"))
                .clicked()
            {
                self.start_export();
            }
            if self.export_pending {
                ui.spinner();
                ui.label("Экспорт…");
            }
        });
        ui.small("Экспорт идёт в фоне; можно продолжать работать.");
    }

    /// Asks for a destination and starts the export on a background thread so
    /// the UI stays responsive (SPEC-052).
    fn start_export(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("WAV", &["wav"])
            .set_file_name("mixdown.wav")
            .save_file()
        else {
            return;
        };
        let spec = daw_app::ExportSpec {
            bit_depth: self.export_bits,
            sample_rate: self.export_rate,
            normalize: self.export_normalize.then_some(self.export_target_db),
        };
        let tx = self.export_tx.clone();
        // Build the job here (it reads decoded files), then render off the
        // UI thread so the window stays responsive.
        let job = match self.editor.prepare_export(&path) {
            Ok(job) => job,
            Err(e) => {
                self.status = format!("Ошибка экспорта: {e}");
                return;
            }
        };
        self.export_pending = true;
        self.status = "Экспорт…".into();
        let rate = spec.sample_rate;
        std::thread::spawn(move || {
            let result =
                daw_app::render_export(job, spec).map_err(|e| EditorError::Render(e.to_string()));
            let _ = tx.send(ExportMsg::Done(path, rate, result));
        });
    }

    /// Collects a finished export and reports its result.
    fn poll_export(&mut self) {
        if !self.export_pending {
            return;
        }
        while let Ok(msg) = self.export_rx.try_recv() {
            let ExportMsg::Done(path, rate, result) = msg;
            self.export_pending = false;
            match result {
                Ok(report) => {
                    #[allow(clippy::cast_precision_loss)]
                    let seconds = report.frames as f64 / f64::from(rate);
                    self.status = format!("Сведение записано: {} ({seconds:.2} с)", path.display());
                }
                Err(e) => self.status = format!("Ошибка экспорта: {e}"),
            }
        }
    }

    fn menu(&mut self, ui: &mut egui::Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("Файл", |ui| {
                if ui.button("Новый").clicked() {
                    self.reset_playback();
                    self.editor = Editor::new();
                    self.status = "Новый проект".into();
                }
                if ui.button("Открыть…").clicked() {
                    self.open_dialog();
                }
                if ui.button("Сохранить").clicked() {
                    self.save();
                }
                if ui.button("Сохранить как…").clicked() {
                    self.save_as_dialog();
                }
                ui.separator();
                if ui.button("Импорт WAV…").clicked() {
                    self.import_dialog();
                }
                if ui.button("Экспорт сведения…").clicked() {
                    self.export_dialog();
                }
                ui.separator();
                if ui.button("Настройки звука…").clicked() {
                    self.open_audio_settings();
                }
                ui.separator();
                if ui.button("Выход").clicked() {
                    ui.ctx().send_viewport_cmd(ViewportCommand::Close);
                }
            });
            ui.menu_button("Дорожка", |ui| {
                let new = egui::Button::new("Новые дорожки…").shortcut_text("Ctrl+Alt+N");
                if ui.add(new).clicked() {
                    self.open_new_track(TrackKind::Audio);
                }
                if ui.button("Новая MIDI-дорожка").clicked() {
                    self.add_midi_track();
                }
            });
            ui.menu_button("Правка", |ui| {
                let undo = egui::Button::new("Отменить").shortcut_text("Ctrl+Z");
                if ui.add_enabled(self.editor.can_undo(), undo).clicked() {
                    self.undo();
                }
                let redo = egui::Button::new("Повторить").shortcut_text("Ctrl+Shift+Z");
                if ui.add_enabled(self.editor.can_redo(), redo).clicked() {
                    self.redo();
                }
                ui.separator();
                let has_clip = self.editor.selected_clip.is_some();
                let split = egui::Button::new("Разрезать по курсору").shortcut_text("S");
                if ui.add_enabled(has_clip, split).clicked() {
                    self.split_clip();
                }
                let copy = egui::Button::new("Копировать клип").shortcut_text("Ctrl+C");
                if ui.add_enabled(has_clip, copy).clicked() {
                    self.copy_clip(ui.ctx());
                }
                let paste = egui::Button::new("Вставить клип").shortcut_text("Ctrl+V");
                if ui.add_enabled(self.editor.has_clipboard(), paste).clicked() {
                    self.paste_clip();
                }
                let duplicate = egui::Button::new("Дублировать клип").shortcut_text("Ctrl+D");
                if ui.add_enabled(has_clip, duplicate).clicked() {
                    self.duplicate_clip();
                }
            });
            ui.menu_button("Справка", |ui| {
                if ui.button("О программе").clicked() {
                    self.about_open = true;
                    ui.close();
                }
            });
        });
    }

    #[allow(clippy::too_many_lines)]
    fn track_headers(&mut self, ui: &mut egui::Ui) {
        ui.add_space(RULER_HEIGHT);
        let count = self.editor.session().tracks.len();
        let mut delete = None;
        let mut action = None;
        for index in 0..count {
            let selected = self.editor.is_track_selected(index);
            let frame = egui::Frame::new()
                .fill(if selected {
                    Color32::from_rgb(48, 56, 72)
                } else {
                    Color32::from_gray(36)
                })
                .inner_margin(4.0);
            // SPEC-044 §1: the click-sensing scope is registered before
            // its children, so the M/S/R/I buttons inside get their clicks.
            let response = ui
                .scope_builder(egui::UiBuilder::new().sense(Sense::click()), |ui| {
                    frame.show(ui, |ui| {
                        ui.set_height(TRACK_HEIGHT - 8.0);
                        ui.set_width(ui.available_width());
                        let track = &self.editor.session().tracks[index];
                        let (mut mute, mut solo) = (track.mute, track.solo);
                        let track_monitor = track.input_monitor;
                        let track_stereo = track.format == TrackFormat::Stereo;
                        let (mut gain, mut pan) = (track.gain, track.pan);
                        let name = track.name.clone();
                        let audio = track.kind == TrackKind::Audio;
                        let color = track_color(track.color, index);
                        ui.horizontal(|ui| {
                            ui.add_space(6.0);
                            let (icon, _) =
                                ui.allocate_exact_size(Vec2::splat(22.0), Sense::hover());
                            ui.painter()
                                .rect_filled(icon, 4.0, color.gamma_multiply(0.35));
                            ui.painter().rect_stroke(
                                icon,
                                4.0,
                                egui::Stroke::new(1.0_f32, color),
                                egui::StrokeKind::Inside,
                            );
                            ui.painter().text(
                                icon.center(),
                                egui::Align2::CENTER_CENTER,
                                if audio { "〰" } else { "♪" },
                                egui::FontId::proportional(13.0),
                                color,
                            );
                            if let Some((_, text)) =
                                self.rename.as_mut().filter(|(track, _)| *track == index)
                            {
                                let edit =
                                    ui.add(egui::TextEdit::singleline(text).desired_width(110.0));
                                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                                    self.rename = None;
                                } else if edit.lost_focus() {
                                    action = Some(TrackAction::Rename(index));
                                } else {
                                    edit.request_focus();
                                }
                            } else if ui
                                .add_sized(
                                    Vec2::new(
                                        name_width(ui.available_width(), self.auto == Some(index)),
                                        ui.spacing().interact_size.y,
                                    ),
                                    egui::Label::new(egui::RichText::new(&name).strong())
                                        .truncate()
                                        .halign(egui::Align::LEFT)
                                        .sense(Sense::click()),
                                )
                                .on_hover_text(format!("{name}\nДвойной щелчок — переименовать"))
                                .double_clicked()
                            {
                                self.rename = Some((index, name.clone()));
                            }
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui
                                        .small_button("✕")
                                        .on_hover_text("Удалить дорожку")
                                        .clicked()
                                    {
                                        delete = Some(index);
                                    }
                                    let effects = self.editor.session().tracks[index].effects.len();
                                    let label = if effects == 0 {
                                        "FX".to_owned()
                                    } else {
                                        format!("FX {effects}")
                                    };
                                    if ui
                                        .selectable_label(
                                            self.fx == Some(FxTarget::Track(index)),
                                            label,
                                        )
                                        .on_hover_text("Эффекты дорожки")
                                        .clicked()
                                    {
                                        self.fx = if self.fx == Some(FxTarget::Track(index)) {
                                            None
                                        } else {
                                            Some(FxTarget::Track(index))
                                        };
                                    }
                                    if ui
                                        .selectable_label(self.auto == Some(index), "A")
                                        .on_hover_text("Автоматизация")
                                        .clicked()
                                    {
                                        self.auto = if self.auto == Some(index) {
                                            None
                                        } else {
                                            Some(index)
                                        };
                                        self.auto_drag = None;
                                    }
                                    if self.auto == Some(index) {
                                        self.lane_picker(ui, index);
                                    }
                                },
                            );
                        });
                        ui.horizontal(|ui| {
                            ui.add_space(34.0);
                            ui.spacing_mut().item_spacing.x = 3.0;
                            let m =
                                shell::pill(ui, &mut mute, "M", Color32::from_rgb(90, 170, 255))
                                    .on_hover_text("Без звука");
                            let s =
                                shell::pill(ui, &mut solo, "S", Color32::from_rgb(240, 200, 60))
                                    .on_hover_text("Соло");
                            let mut armed = self.editor.is_armed(index);
                            if shell::pill(ui, &mut armed, "R", Color32::from_rgb(230, 60, 60))
                                .on_hover_text("Запись на эту дорожку")
                                .changed()
                            {
                                self.editor.set_armed(index, armed);
                            }
                            let mut monitor = track_monitor;
                            let i =
                                shell::pill(ui, &mut monitor, "I", Color32::from_rgb(240, 160, 60))
                                    .on_hover_text("Мониторинг входа");
                            if s.changed() || m.changed() || i.changed() {
                                if let Some(t) = self.editor.track_mut(index) {
                                    t.mute = mute;
                                    t.solo = solo;
                                    t.input_monitor = monitor;
                                }
                            }
                            if audio {
                                let mut stereo = track_stereo;
                                if shell::pill(
                                    ui,
                                    &mut stereo,
                                    "◎",
                                    Color32::from_rgb(120, 200, 140),
                                )
                                .on_hover_text("Стерео / моно")
                                .changed()
                                {
                                    let format = if stereo {
                                        TrackFormat::Stereo
                                    } else {
                                        TrackFormat::Mono
                                    };
                                    let _ = self.editor.set_track_format(index, format);
                                }
                            }
                        });
                        let automation = &self.editor.session().tracks[index].automation;
                        let (gain_free, pan_free) =
                            (automation.gain.is_empty(), automation.pan.is_empty());
                        let (g, p) = ui
                            .horizontal(|ui| {
                                ui.add_space(30.0);
                                ui.spacing_mut().slider_width =
                                    ((ui.available_width() - 60.0) * 0.6).max(40.0);
                                let g = ui
                                    .add_enabled(
                                        gain_free,
                                        egui::Slider::new(&mut gain, 0.0..=1.0).show_value(false),
                                    )
                                    .on_hover_text(format!(
                                        "Громкость {}",
                                        daw_gui::layout::db_label(gain)
                                    ));
                                ui.spacing_mut().slider_width =
                                    (ui.available_width() - 8.0).max(30.0);
                                let p = ui
                                    .add_enabled(
                                        pan_free,
                                        egui::Slider::new(&mut pan, -1.0..=1.0).show_value(false),
                                    )
                                    .on_hover_text(format!(
                                        "Панорама {}",
                                        daw_gui::layout::pan_label(pan)
                                    ));
                                (g, p)
                            })
                            .inner;
                        if g.changed() || p.changed() {
                            if let Some(t) = self.editor.track_mut(index) {
                                t.gain = gain;
                                t.pan = pan;
                            }
                        }
                    });
                })
                .response;
            ui.painter().rect_filled(
                Rect::from_min_size(response.rect.min, Vec2::new(4.0, response.rect.height())),
                0.0,
                track_color(self.editor.session().tracks[index].color, index),
            );
            if let Some(meter) = self.track_meters.get(index) {
                let r = response.rect;
                let bar = Rect::from_min_max(
                    egui::pos2(r.left() + 10.0, r.bottom() - 5.0),
                    egui::pos2(r.right() - 8.0, r.bottom() - 1.0),
                );
                shell::paint_meter_bars(ui.painter(), bar, meter, false);
            }
            if response.clicked() {
                // SPEC-047 §5: Ctrl adds or removes a track, Shift a range.
                let (toggle, range) = ui.input(|i| (i.modifiers.command, i.modifiers.shift));
                self.editor.select_track(index, toggle, range);
            }
            response.context_menu(|ui| {
                if ui.button("Переименовать").clicked() {
                    self.rename = Some((index, self.editor.session().tracks[index].name.clone()));
                    ui.close();
                }
                ui.menu_button("Цвет", |ui| {
                    for color in 0..TRACK_COLORS {
                        let swatch = egui::RichText::new("■■■■").color(track_color(Some(color), 0));
                        if ui.button(swatch).clicked() {
                            action = Some(TrackAction::Color(index, Some(color)));
                            ui.close();
                        }
                    }
                    if ui.button("Без цвета").clicked() {
                        action = Some(TrackAction::Color(index, None));
                        ui.close();
                    }
                });
                if ui
                    .add_enabled(index > 0, egui::Button::new("Выше"))
                    .clicked()
                {
                    action = Some(TrackAction::Move(index, index - 1));
                    ui.close();
                }
                if ui
                    .add_enabled(index + 1 < count, egui::Button::new("Ниже"))
                    .clicked()
                {
                    action = Some(TrackAction::Move(index, index + 1));
                    ui.close();
                }
                if ui.button("Дублировать").clicked() {
                    action = Some(TrackAction::Duplicate(index));
                    ui.close();
                }
                if ui.button("Удалить").clicked() {
                    delete = Some(index);
                    ui.close();
                }
                ui.separator();
                new_track_items(ui, &mut action);
            });
        }
        if let Some(action) = action {
            self.track_action(action);
        }
        if let Some(index) = delete {
            self.rename = None;
            self.fx = None;
            self.auto = None;
            self.auto_drag = None;
            self.editor.delete_track(index);
            self.status = "Дорожка удалена".into();
        }
        if count == 0 {
            ui.label("Нет дорожек");
        }
        // SPEC-050 §4: tracks are created from the header panel, never from
        // empty timeline space.
        let add = ui.button("＋").on_hover_text("Новая дорожка (Ctrl+Alt+N)");
        if add.clicked() {
            action = Some(TrackAction::New(TrackKind::Audio));
        }
        let rest = ui.available_rect_before_wrap();
        let empty = ui.interact(rest, ui.id().with("headers_empty"), Sense::click());
        empty.context_menu(|ui| new_track_items(ui, &mut action));
        if let Some(action) = action {
            self.track_action(action);
        }
    }

    fn track_action(&mut self, action: TrackAction) {
        let result = match action {
            TrackAction::New(TrackKind::Instrument) => {
                self.add_midi_track();
                return;
            }
            TrackAction::New(kind) => {
                self.open_new_track(kind);
                return;
            }
            TrackAction::Rename(track) => {
                let Some((_, text)) = self.rename.take() else {
                    return;
                };
                self.editor
                    .rename_track(track, &text)
                    .map(|()| "Дорожка переименована")
            }
            TrackAction::Color(track, color) => self
                .editor
                .set_track_color(track, color)
                .map(|()| "Цвет изменён"),
            TrackAction::Move(from, to) => {
                self.fx = None;
                self.auto = None;
                self.auto_drag = None;
                self.editor
                    .move_track(from, to)
                    .map(|()| "Дорожка перемещена")
            }
            TrackAction::Duplicate(track) => {
                self.fx = None;
                self.auto = None;
                self.auto_drag = None;
                self.editor
                    .duplicate_track(track)
                    .map(|_| "Дорожка продублирована")
            }
        };
        self.status = match result {
            Ok(text) => text.into(),
            Err(EditorError::EmptyName) => "Имя не может быть пустым".into(),
            Err(err) => format!("Не получилось: {err}"),
        };
    }

    #[allow(clippy::cast_precision_loss, clippy::too_many_lines)]
    /// Drag and drop of WAV/MP3 files onto the timeline (SPEC-032).
    fn handle_drop(&mut self, ui: &egui::Ui, area: Rect, tracks: usize) {
        while let Ok((path, first, blocks)) = self.hover_rx.try_recv() {
            if let Some(p) = self.hover_peaks.get_mut(&path) {
                p.set_blocks(first, &blocks);
            }
        }
        let (hovered, dropped, pointer, free) = ui.ctx().input(|i| {
            (
                i.raw
                    .hovered_files
                    .iter()
                    .filter_map(|f| f.path.clone())
                    .collect::<Vec<_>>(),
                i.raw
                    .dropped_files
                    .iter()
                    .filter_map(|f| f.path.clone())
                    .collect::<Vec<_>>(),
                i.pointer.latest_pos(),
                i.modifiers.shift,
            )
        });
        // egui keeps `raw.hovered_files` until winit reports the drop or the
        // cancel (egui 0.33 clones it each frame, it is not drained).
        let hovering = !hovered.is_empty();
        let session = self.editor.session();
        let (rate, tempo) = (session.sample_rate, session.tempo_bpm);
        let audio_rows: Vec<bool> = session
            .tracks
            .iter()
            .map(|t| t.kind == audio_core::session::TrackKind::Audio)
            .collect();
        let snap = self.snap && !free;
        let zoom = self.zoom;
        let to_target = |local: (f32, f32)| {
            daw_gui::drop_target(
                Some(local),
                RULER_HEIGHT,
                TRACK_HEIGHT,
                &audio_rows,
                rate,
                zoom,
                tempo,
                snap,
            )
        };
        // `p` is window-local in egui points; `drop_target` wants the position
        // measured from the timeline's top-left corner.
        let target_at = |p: Pos2| to_target((p.x - area.min.x, p.y - area.min.y));
        // OS cursor from X11 `QueryPointer` (`win_x`/`win_y`, physical px ->
        // egui points). It is correct at drop time (owner-verified in PR #62),
        // but it can stay frozen while a Wayland client drives the drag.
        #[cfg(target_os = "linux")]
        let xquery = self
            .x11_cursor
            .as_ref()
            .and_then(x11_cursor::X11Cursor::position_debug);
        let ppp = ui.ctx().pixels_per_point();
        let ppp = if ppp.is_finite() && ppp > 0.0 {
            ppp
        } else {
            1.0
        };
        #[cfg(target_os = "linux")]
        let os_points: Option<(f32, f32)> = xquery
            .filter(|p| p.same_screen)
            .map(|p| (f32::from(p.win.0) / ppp, f32::from(p.win.1) / ppp));
        #[cfg(not(target_os = "linux"))]
        let os_points: Option<(f32, f32)> = None;
        let os_pointer = os_points.map(|(x, y)| Pos2::new(x, y));
        // One pointer source for both the ghost and the drop. The vendored
        // winit turns every `XdndPosition` into `CursorMoved`, so egui's
        // `latest_pos` stays live during an external drag on both X11 and
        // Wayland sources. XQueryPointer (`win_x`/`win_y`) can freeze while a
        // Wayland client (Nautilus) drives the drag, so it is only a fallback
        // when egui reports no pointer. Using one source guarantees the ghost
        // sits exactly where the region will land.
        let (effective_pointer, pointer_source) = match (pointer, os_pointer) {
            (Some(p), _) => (Some(p), "egui/xdnd"),
            (None, Some(p)) => (Some(p), "xquery"),
            (None, None) => (None, "none"),
        };
        if hovering {
            // The ghost is drawn on a foreground layer so it paints over the
            // clips/regions, which are drawn later on the timeline painter.
            let painter = ui
                .ctx()
                .layer_painter(egui::LayerId::new(
                    egui::Order::Foreground,
                    egui::Id::new("drop-ghost"),
                ))
                .with_clip_rect(area);
            let target = effective_pointer.map(target_at);
            self.drop_tracker.hover(target);
            let band = match target.and_then(|t| t.track) {
                Some(t) => {
                    let top = area.min.y + RULER_HEIGHT + t as f32 * TRACK_HEIGHT;
                    Rect::from_min_size(
                        Pos2::new(area.min.x, top),
                        Vec2::new(area.width(), TRACK_HEIGHT),
                    )
                }
                None => {
                    let top = area.min.y + RULER_HEIGHT + tracks as f32 * TRACK_HEIGHT;
                    Rect::from_min_max(Pos2::new(area.min.x, top), area.max)
                }
            };
            // The first audio file's length comes from its header, read once
            // per hover (SPEC-050 §2).
            let first = hovered
                .iter()
                .find(|p| daw_app::audio_file::is_audio_file(p))
                .cloned();
            let probe = first.as_ref().map(|path| {
                self.probes
                    .entry(path.clone())
                    .or_insert_with(|| daw_gui::probe_len(path, rate).map_err(|e| e.to_string()))
                    .clone()
            });
            if let (Some(path), Some(Ok(len))) = (&first, &probe) {
                self.start_hover_peaks(path, *len, rate);
            }
            match (probe, target) {
                (Some(Ok(len)), Some(t)) => {
                    let x0 = area.min.x + samples_to_x(t.start, rate, self.zoom);
                    let x1 = area.min.x + samples_to_x(t.start + len, rate, self.zoom);
                    let frame = Rect::from_min_max(
                        Pos2::new(x0, band.min.y + 2.0),
                        Pos2::new(x1.max(x0 + 2.0), band.min.y + TRACK_HEIGHT - 2.0),
                    );
                    painter.rect_filled(
                        frame,
                        4.0,
                        Color32::from_rgba_unmultiplied(80, 140, 255, 70),
                    );
                    if let Some(peaks) = first.as_ref().and_then(|p| self.hover_peaks.get(p)) {
                        let mut clip = audio_core::session::Clip::full(t.start, len);
                        clip.name = String::new();
                        draw_peaks(
                            &painter,
                            frame,
                            frame,
                            &clip,
                            peaks,
                            Color32::from_rgba_unmultiplied(200, 225, 255, 160),
                        );
                    }
                    painter.rect_stroke(
                        frame,
                        4.0,
                        Stroke::new(1.5_f32, Color32::from_rgb(120, 180, 255)),
                        StrokeKind::Inside,
                    );
                    self.status = "Отпустите файл: на дорожку или ниже — новая дорожка".into();
                    if self.drop_debug {
                        eprintln!(
                            "DROP-DEBUG hover ghost_rect=[({x0:.1},{:.1})..({x1:.1},{:.1})] start={} track={:?}",
                            band.min.y + 2.0,
                            band.min.y + TRACK_HEIGHT - 2.0,
                            t.start,
                            t.track,
                        );
                    }
                }
                (Some(Err(e)), _) => self.status = format!("Ошибка: {e}"),
                _ => {
                    painter.rect_filled(
                        band,
                        0.0,
                        Color32::from_rgba_unmultiplied(80, 140, 255, 40),
                    );
                    self.status = "Отпустите файл: на дорожку или ниже — новая дорожка".into();
                }
            }
            ui.ctx().request_repaint();
            if self.drop_debug {
                #[cfg(target_os = "linux")]
                let (xq_win, xq_root, xq_mask) = xquery.map_or((None, None, None), |q| {
                    (Some(q.win), Some(q.root), Some(q.mask))
                });
                #[cfg(not(target_os = "linux"))]
                let (xq_win, xq_root, xq_mask): (
                    CursorWinPx,
                    CursorWinPx,
                    Option<u16>,
                ) = (None, None, None);
                eprintln!(
                    "DROP-DEBUG hover source={pointer_source} egui={pointer:?} xquery_win={xq_win:?} \
                     xquery_root={xq_root:?} mask={xq_mask:?} ppp={ppp} effective={effective_pointer:?} \
                     area_min=({:.1},{:.1}) start={:?} track={:?}",
                    area.min.x,
                    area.min.y,
                    target.map(|t| t.start),
                    target.and_then(|t| t.track),
                );
                let painter = ui.ctx().layer_painter(egui::LayerId::new(
                    egui::Order::Foreground,
                    egui::Id::new("drop-debug-cross"),
                ));
                // Red: X11 QueryPointer; green: egui pointer (fed by XDND).
                if let Some(p) = os_pointer {
                    draw_cross(&painter, p, Color32::RED, "xq");
                }
                if let Some(p) = pointer {
                    draw_cross(&painter, p, Color32::GREEN, "eg");
                }
            }
        } else if dropped.is_empty() {
            self.probes.clear();
            self.drop_tracker.cancel();
        }
        if dropped.is_empty() {
            return;
        }
        self.probes.clear();
        // The ghost and the drop share one pointer source (`effective_pointer`)
        // and one conversion (`target_at`), so the region lands exactly where
        // the ghost was drawn. The last hover target wins over the drop-frame
        // position so a final-frame pointer glitch cannot move the drop.
        let target = match effective_pointer {
            Some(p) => {
                // Same live source the ghost used; drop right under it.
                self.drop_tracker.cancel();
                target_at(p)
            }
            _ => self.drop_tracker.release(pointer.map(target_at)),
        };
        if self.drop_debug {
            #[cfg(target_os = "linux")]
            let (xq_win, xq_root) = xquery.map_or((None, None), |q| (Some(q.win), Some(q.root)));
            #[cfg(not(target_os = "linux"))]
            let (xq_win, xq_root): (CursorWinPx, CursorWinPx) = (None, None);
            eprintln!(
                "DROP-DEBUG drop source={pointer_source} xquery_win={xq_win:?} xquery_root={xq_root:?} \
                 ppp={ppp} os_points={os_points:?} egui={pointer:?} effective={effective_pointer:?} \
                 area_min=({:.1},{:.1}) ruler_h={RULER_HEIGHT} row_h={TRACK_HEIGHT} tracks={tracks} \
                 target.track={:?} target.start={} ghost_at_egui={:?}",
                area.min.x,
                area.min.y,
                target.track,
                target.start,
                pointer.map(|p| {
                    let t = target_at(p);
                    (t.track, t.start)
                }),
            );
        }
        self.import_in_background(dropped, target.track, Some(target.start));
    }

    /// Decodes a hovered file in the background so its waveform can show in
    /// the preview frame; the decode also fills the PCM cache for the drop.
    fn start_hover_peaks(&mut self, path: &std::path::Path, len: u64, rate: u32) {
        if self.hover_peaks.contains_key(path) {
            return;
        }
        if self.hover_peaks.len() >= 8 {
            self.hover_peaks.clear();
        }
        let template = daw_gui::Peaks::empty(len);
        self.hover_peaks
            .insert(path.to_path_buf(), template.clone());
        let (tx, path, cache) = (
            self.hover_tx.clone(),
            path.to_path_buf(),
            self.editor.pcm_cache(),
        );
        std::thread::spawn(move || {
            let mut sent = 0;
            let mut last = std::time::Instant::now();
            let progress_path = path.clone();
            let result =
                daw_gui::read_source_cached(&path, rate, cache.as_ref(), &mut |from, ch| {
                    let Some(left) = ch.first() else { return };
                    if last.elapsed() < std::time::Duration::from_millis(100) {
                        return;
                    }
                    last = std::time::Instant::now();
                    let blocks = template.blocks_from(left, from, rate, sent);
                    if blocks.is_empty() {
                        return;
                    }
                    let first = sent;
                    sent += blocks.len();
                    let _ = tx.send((progress_path.clone(), first, blocks));
                });
            if let Ok(ch) = result {
                let all = template.blocks_from(&ch.left, rate, rate, sent);
                if !all.is_empty() {
                    let _ = tx.send((path, sent, all));
                }
            }
        });
    }

    /// Scroll offset that keeps the zoom anchor still (SPEC-037): the
    /// pointer for Ctrl+wheel, the playhead (or the middle) for the slider.
    #[allow(clippy::float_cmp)]
    fn zoom_scroll(&mut self) -> Option<f32> {
        let anchor = self.zoom_anchor.take();
        if self.zoom == self.drawn_zoom {
            return None;
        }
        let (offset, width) = self.timeline_view;
        let anchor = anchor.unwrap_or_else(|| {
            let rate = self.editor.session().sample_rate;
            let x = samples_to_x(self.playhead, rate, self.drawn_zoom);
            daw_gui::layout::slider_anchor(x, offset, width)
        });
        Some(daw_gui::layout::zoom_offset(
            self.drawn_zoom,
            self.zoom,
            offset,
            anchor,
        ))
    }

    fn timeline(&mut self, ui: &mut egui::Ui) {
        let rate = f64::from(self.editor.session().sample_rate);
        let session_seconds = self.editor.session().length_samples() as f64 / rate;
        let width = ((session_seconds as f32 + 10.0) * self.zoom).max(ui.available_width());
        let tracks = self.editor.session().tracks.len();
        let height = (RULER_HEIGHT + TRACK_HEIGHT * tracks as f32).max(ui.available_height() - 4.0);

        let (area, response) =
            ui.allocate_exact_size(Vec2::new(width, height), Sense::click_and_drag());
        self.drawn_zoom = self.zoom;
        if let Some(t) = self.reveal_track.take() {
            let top = area.min.y + RULER_HEIGHT + t as f32 * TRACK_HEIGHT;
            let left = area.min.x + samples_to_x(self.playhead, rate as u32, self.zoom);
            ui.scroll_to_rect(
                Rect::from_min_size(Pos2::new(left, top), Vec2::new(1.0, TRACK_HEIGHT)),
                None,
            );
        }
        let view = ui.clip_rect();
        self.timeline_view = ((view.min.x - area.min.x).max(0.0), view.width());
        let painter = ui.painter_at(area);
        painter.rect_filled(area, 0.0, Color32::from_gray(24));
        self.handle_drop(ui, area, tracks);

        // Ruler: one line per beat, labels on bar starts (SPEC-026).
        let beat_seconds = 60.0 / self.editor.session().tempo_bpm;
        let per_bar = usize::from(self.editor.session().time_signature.numerator.max(1));
        let beat_px = beat_seconds as f32 * self.zoom;
        let bar_px = beat_px * per_bar as f32;
        let bar_step = if bar_px < 40.0 {
            ((40.0 / bar_px).ceil() as usize).max(1)
        } else {
            1
        };
        let ruler = Rect::from_min_size(area.min, Vec2::new(width, RULER_HEIGHT));
        painter.rect_filled(ruler, 0.0, Color32::from_gray(40));
        let beats = (width / beat_px.max(0.01)).ceil() as usize;
        for beat in 0..=beats.min(100_000) {
            let x = area.min.x + beat as f32 * beat_px;
            let bar = beat / per_bar;
            if beat % per_bar == 0 && bar % bar_step == 0 {
                painter.line_segment(
                    [Pos2::new(x, area.min.y), Pos2::new(x, area.max.y)],
                    Stroke::new(1.0_f32, Color32::from_gray(60)),
                );
                painter.text(
                    Pos2::new(x + 3.0, area.min.y + 4.0),
                    Align2::LEFT_TOP,
                    (bar + 1).to_string(),
                    FontId::monospace(11.0),
                    Color32::from_gray(200),
                );
            } else if beat_px >= 8.0 && bar_step == 1 {
                painter.line_segment(
                    [Pos2::new(x, ruler.max.y), Pos2::new(x, area.max.y)],
                    Stroke::new(1.0_f32, Color32::from_gray(34)),
                );
            }
        }

        let rate_u32 = self.editor.session().sample_rate;
        if let Some((from, to)) = self.editor.cycle() {
            let x0 = area.min.x + samples_to_x(from, rate_u32, self.zoom);
            let x1 = area.min.x + samples_to_x(to, rate_u32, self.zoom);
            let (bar, under) = if self.editor.cycle_enabled {
                (
                    Color32::from_rgb(230, 190, 50),
                    Color32::from_rgba_unmultiplied(230, 190, 50, 24),
                )
            } else {
                (Color32::from_gray(110), Color32::TRANSPARENT)
            };
            painter.rect_filled(
                Rect::from_min_max(
                    Pos2::new(x0, ruler.min.y + RULER_HEIGHT * 0.55),
                    Pos2::new(x1.max(x0 + 2.0), ruler.max.y),
                ),
                2.0,
                bar,
            );
            painter.rect_filled(
                Rect::from_min_max(Pos2::new(x0, ruler.max.y), Pos2::new(x1, area.max.y)),
                0.0,
                under,
            );
        }

        let on_flag = self.markers(ui, area, ruler);

        let px_per_sample = self.zoom / rate as f32;
        let mut clicked = None;
        let mut open_piano = None;
        let mut rects = Vec::new();
        let pointer = response.interact_pointer_pos();
        for (t, track) in self.editor.session().tracks.iter().enumerate() {
            let top = area.min.y + RULER_HEIGHT + t as f32 * TRACK_HEIGHT;
            painter.line_segment(
                [
                    Pos2::new(area.min.x, top + TRACK_HEIGHT),
                    Pos2::new(area.max.x, top + TRACK_HEIGHT),
                ],
                Stroke::new(1.0_f32, Color32::from_gray(45)),
            );
            let mut selected_rects = Vec::new();
            for c in daw_gui::layout::draw_order(&track.clips) {
                let clip = &track.clips[c];
                let left = area.min.x + clip.start_samples as f32 * px_per_sample;
                let w = (clip.length_samples as f32 * px_per_sample).max(2.0);
                let rect = Rect::from_min_size(
                    Pos2::new(left, top + 3.0),
                    Vec2::new(w, TRACK_HEIGHT - 6.0),
                );
                let selected = self.editor.is_clip_selected((t, c));
                let base = track_color(track.color, t);
                let colors = daw_gui::layout::region_colors(
                    [base.r(), base.g(), base.b()],
                    !self.editor.session().is_audible(t),
                );
                let rgb = |c: [u8; 3]| Color32::from_rgb(c[0], c[1], c[2]);
                let header_h = 14.0_f32.min(rect.height() * 0.4);
                painter.rect_filled(rect, 4.0, rgb(colors.body).gamma_multiply(0.92));
                let header = Rect::from_min_size(rect.min, Vec2::new(rect.width(), header_h));
                painter.rect_filled(
                    header,
                    CornerRadius {
                        nw: 4,
                        ne: 4,
                        sw: 0,
                        se: 0,
                    },
                    rgb(colors.header),
                );
                let body = Rect::from_min_max(Pos2::new(rect.min.x, header.max.y), rect.max);
                let detail = rgb(colors.detail);
                if let Some(notes) = &clip.notes {
                    let end = clip.offset_samples + clip.length_samples;
                    let (lo, hi) = notes.iter().fold((127u8, 0u8), |(lo, hi), n| {
                        (lo.min(n.pitch), hi.max(n.pitch))
                    });
                    let span = f32::from(hi.saturating_sub(lo).max(12));
                    let mid = (f32::from(lo) + f32::from(hi)) / 2.0;
                    for n in notes {
                        if n.start + n.length <= clip.offset_samples || n.start >= end {
                            continue;
                        }
                        let from = n.start.max(clip.offset_samples) - clip.offset_samples;
                        let to = (n.start + n.length).min(end) - clip.offset_samples;
                        let rel = (f32::from(n.pitch) - mid) / span + 0.5;
                        let y = body.max.y - 5.0 - rel.clamp(0.0, 1.0) * (body.height() - 10.0);
                        let x0 = rect.min.x + from as f32 * px_per_sample;
                        let x1 = (rect.min.x + to as f32 * px_per_sample).max(x0 + 2.0);
                        painter.rect_filled(
                            Rect::from_min_max(Pos2::new(x0, y - 1.5), Pos2::new(x1, y + 1.5)),
                            1.0,
                            detail,
                        );
                    }
                }
                draw_clip_wave(&self.editor, &painter, rect, body, clip, detail);
                let hovered = pointer.is_some_and(|p| rect.contains(p));
                let handles = daw_gui::layout::edges_in_overlap(&track.clips, c);
                draw_fades(
                    &painter,
                    rect,
                    clip,
                    px_per_sample,
                    (selected || hovered, handles),
                );
                painter
                    .with_clip_rect(header.intersect(painter.clip_rect()))
                    .text(
                        rect.min + Vec2::new(5.0, header_h / 2.0),
                        Align2::LEFT_CENTER,
                        &clip.name,
                        FontId::proportional(11.0),
                        Color32::from_gray(245),
                    );
                let border = if selected {
                    Stroke::new(2.0_f32, Color32::from_rgb(255, 200, 60))
                } else {
                    Stroke::new(1.0_f32, Color32::from_black_alpha(140))
                };
                painter.rect_stroke(rect, 4.0, border, StrokeKind::Inside);
                if selected {
                    selected_rects.push((rect, border));
                }
                rects.push(((t, c), rect));
                if (response.clicked() || response.secondary_clicked())
                    && pointer.is_some_and(|p| rect.contains(p))
                {
                    clicked = Some((t, c));
                }
                if response.double_clicked() && pointer.is_some_and(|p| rect.contains(p)) {
                    open_piano = Some((t, c));
                }
            }
            let base = track_color(track.color, t);
            let colors = daw_gui::layout::region_colors(
                [base.r(), base.g(), base.b()],
                !self.editor.session().is_audible(t),
            );
            for zone in daw_gui::layout::overlap_zones(&track.clips) {
                let body =
                    draw_overlap_zone(&painter, zone, area.min.x, top, px_per_sample, &colors);
                // The merged body must not hide the sound (SPEC-045 §6): the
                // waveforms of every clip under the zone are drawn over it.
                let zone_painter = painter.with_clip_rect(body.intersect(painter.clip_rect()));
                for (c, clip) in track.clips.iter().enumerate() {
                    let Some(rect) = rects.iter().find(|(at, _)| *at == (t, c)).map(|(_, r)| *r)
                    else {
                        continue;
                    };
                    if !rect.intersects(body) {
                        continue;
                    }
                    let wave = Rect::from_min_max(Pos2::new(rect.min.x, body.min.y), rect.max);
                    draw_clip_wave(
                        &self.editor,
                        &zone_painter,
                        rect,
                        wave,
                        clip,
                        rgb_detail(&colors),
                    );
                }
                draw_crossfade_x(&painter, body);
            }
            for (rect, border) in selected_rects {
                painter.rect_stroke(rect, 4.0, border, StrokeKind::Inside);
            }
        }

        let auto_row = self.auto.filter(|&t| t < tracks).map(|t| {
            let top = area.min.y + RULER_HEIGHT + t as f32 * TRACK_HEIGHT;
            Rect::from_min_max(
                Pos2::new(area.min.x, top),
                Pos2::new(area.max.x, top + TRACK_HEIGHT),
            )
        });
        if let (Some(t), Some(row)) = (self.auto.filter(|&t| t < tracks), auto_row) {
            self.draw_lane(&painter, area, row, t);
        }
        // While a lane is shown, presses inside its row edit the lane, not clips.
        let pressed = response.is_pointer_button_down_on()
            || response.clicked()
            || response.secondary_clicked()
            || response.drag_stopped();
        let press_on_ruler = !on_flag && ui.input(press_pos).is_some_and(|p| ruler.contains(p));
        let ruler_owned = self.drag.is_none()
            && self.auto_drag.is_none()
            && (self.cycle_drag.is_some()
                || (press_on_ruler && (response.drag_started() || response.clicked())));
        let lane_owned = !ruler_owned
            && self.drag.is_none()
            && (self.auto_drag.is_some()
                || (pressed
                    && auto_row
                        .is_some_and(|row| ui.input(press_pos).is_some_and(|p| row.contains(p)))));

        self.draw_ghosts(&painter, area);
        self.draw_take(&painter, area);

        let cursor = area.min.x + samples_to_x(self.playhead, rate_u32, self.zoom);
        painter.line_segment(
            [Pos2::new(cursor, area.min.y), Pos2::new(cursor, area.max.y)],
            Stroke::new(1.5_f32, Color32::from_rgb(255, 90, 70)),
        );

        if ruler_owned {
            self.ruler_input(ui, &response, area);
        } else if lane_owned {
            if let (Some(t), Some(row)) = (self.auto, auto_row) {
                self.edit_lane(ui, &response, area, row, t);
            }
        } else if response.clicked() || response.secondary_clicked() {
            // SPEC-047 §1: Shift or Ctrl adds or removes a region; a right
            // click on a selected region keeps the group for its menu.
            let toggle = ui.input(|i| i.modifiers.shift || i.modifiers.command);
            match clicked {
                Some(at) if response.secondary_clicked() && self.editor.is_clip_selected(at) => {
                    self.editor.focus_clip(at);
                }
                Some(at) => self
                    .editor
                    .select_clip(Some(at), toggle && response.clicked()),
                None => {
                    if !toggle {
                        self.editor.select_clip(None, false);
                    }
                    if let Some(p) = pointer {
                        let row = ((p.y - area.min.y - RULER_HEIGHT) / TRACK_HEIGHT).floor();
                        if row >= 0.0 && (row as usize) < tracks {
                            self.editor.select_track(row as usize, false, false);
                        }
                    }
                }
            }
        }

        if let Some(at) = open_piano {
            self.open_clip_editor(at);
        } else if response.double_clicked() && !lane_owned && !ruler_owned {
            // SPEC-049 §6: a double click on an empty spot of an instrument
            // track makes a MIDI region there.
            if let Some(p) = pointer {
                let row = ((p.y - area.min.y - RULER_HEIGHT) / TRACK_HEIGHT).floor();
                if row >= 0.0 {
                    let x = x_to_samples((p.x - area.min.x).max(0.0), rate_u32, self.zoom);
                    self.new_region_on(row as usize, x);
                }
            }
        }
        if !lane_owned && !ruler_owned {
            self.region_menu(&response);
        }

        if !lane_owned && !ruler_owned {
            self.drag_clips(ui, &response, area, &rects);
        }

        if response.hovered() || ui.rect_contains_pointer(ui.clip_rect()) {
            // Pinch on a touchpad and Ctrl+wheel both arrive as one zoom
            // factor, anchored at the pointer (SPEC-040 §5).
            let factor = ui.input(egui::InputState::zoom_delta);
            if (factor - 1.0).abs() > f32::EPSILON {
                self.zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
                self.zoom_anchor = ui
                    .input(|i| i.pointer.hover_pos())
                    .map(|p| p.x - ui.clip_rect().min.x);
            }
        }
    }

    /// The take being recorded, growing as it records, with its waveform
    /// (SPEC-035). It lands on the armed track, else the selected one, else
    /// below the last one (SPEC-038).
    #[cfg(feature = "audio")]
    #[allow(clippy::cast_precision_loss)]
    fn draw_take(&self, painter: &egui::Painter, area: Rect) {
        let Some(recorder) = self.recorder.as_ref() else {
            return;
        };
        let samples = recorder.control.samples();
        let rate = recorder.control.sample_rate().max(1);
        let project = self.editor.session().sample_rate;
        let tracks = self.editor.session().tracks.len();
        let mut rows: Vec<usize> = self
            .editor
            .armed_tracks()
            .filter(|&t| self.editor.session().tracks[t].kind == TrackKind::Audio)
            .collect();
        if rows.is_empty() {
            rows.push(
                self.editor
                    .selected_track
                    .filter(|&t| t < tracks)
                    .unwrap_or(tracks),
            );
        }
        let left = area.min.x + samples_to_x(recorder.start, project, self.zoom);
        let seconds = samples.len() as f32 / rate as f32;
        for row in rows {
            let top = area.min.y + RULER_HEIGHT + row as f32 * TRACK_HEIGHT;
            let rect = Rect::from_min_size(
                Pos2::new(left, top + 3.0),
                Vec2::new((seconds * self.zoom).max(2.0), TRACK_HEIGHT - 6.0),
            );
            draw_take_rect(painter, rect, samples);
        }
    }

    #[cfg(not(feature = "audio"))]
    #[allow(clippy::unused_self)]
    const fn draw_take(&self, _painter: &egui::Painter, _area: Rect) {}

    /// Marker flags on the ruler and their clicks, drags and menus (SPEC-030).
    /// Returns whether the current press started on a flag.
    fn markers(&mut self, ui: &egui::Ui, area: Rect, ruler: Rect) -> bool {
        let session = self.editor.session();
        let (rate, tempo) = (session.sample_rate, session.tempo_bpm);
        let free = ui.input(|i| i.modifiers.shift) || !self.snap;
        let painter = ui.painter_at(area);
        let color = Color32::from_rgb(120, 200, 255);
        let mut action = None;
        let mut flags = Vec::new();
        for (index, marker) in session.markers.iter().enumerate() {
            let position = match self.marker_drag {
                Some((dragged, to)) if dragged == index => to,
                _ => marker.position,
            };
            let x = area.min.x + samples_to_x(position, rate, self.zoom);
            painter.line_segment(
                [Pos2::new(x, ruler.max.y), Pos2::new(x, area.max.y)],
                Stroke::new(1.0_f32, color.gamma_multiply(0.5)),
            );
            let galley = painter.layout_no_wrap(
                marker.name.clone(),
                FontId::proportional(11.0),
                Color32::BLACK,
            );
            let flag = Rect::from_min_size(
                Pos2::new(x, ruler.min.y + RULER_HEIGHT * 0.45),
                Vec2::new(galley.size().x + 8.0, RULER_HEIGHT * 0.55),
            );
            painter.rect_filled(flag, 2.0, color);
            painter.galley(flag.min + Vec2::new(4.0, 0.5), galley, Color32::BLACK);
            flags.push((index, marker.position, flag));
        }
        let origin = ui.input(press_pos);
        let on_flag = self.marker_drag.is_some()
            || origin.is_some_and(|p| flags.iter().any(|(_, _, flag)| flag.contains(p)));
        for (index, position, flag) in flags {
            let id = ui.id().with(("marker", index));
            let response = ui
                .interact(flag, id, Sense::click_and_drag())
                .on_hover_cursor(egui::CursorIcon::Grab);
            if response.clicked() {
                action = Some(MarkerAction::Seek(position));
            }
            if response.dragged()
                && let Some(p) = ui.input(|i| i.pointer.latest_pos())
            {
                let samples = x_to_samples((p.x - area.min.x).max(0.0), rate, self.zoom);
                let to = if free {
                    samples
                } else {
                    snap_to_beat(samples, rate, tempo)
                };
                self.marker_drag = Some((index, to));
            }
            if response.drag_stopped()
                && let Some((dragged, to)) = self.marker_drag.take()
            {
                action = Some(MarkerAction::Move(dragged, to));
            }
            response.context_menu(|ui| {
                if ui.button("Переименовать").clicked() {
                    action = Some(MarkerAction::Rename(index));
                    ui.close();
                }
                if ui.button("Удалить").clicked() {
                    action = Some(MarkerAction::Delete(index));
                    ui.close();
                }
            });
            if self
                .marker_rename
                .as_ref()
                .is_some_and(|(m, _)| *m == index)
            {
                self.marker_name_field(ui, flag);
            }
        }
        match action {
            Some(MarkerAction::Seek(position)) => self.seek(position),
            Some(MarkerAction::Move(index, to)) => {
                self.status = match self.editor.move_marker(index, to) {
                    Ok(_) => "Метка перенесена".into(),
                    Err(e) => format!("Не удалось перенести метку: {e}"),
                };
            }
            Some(MarkerAction::Rename(index)) => {
                let name = self.editor.session().markers[index].name.clone();
                self.marker_rename = Some((index, name));
            }
            Some(MarkerAction::Delete(index)) => {
                self.marker_rename = None;
                self.status = match self.editor.delete_marker(index) {
                    Ok(()) => "Метка удалена".into(),
                    Err(e) => format!("Не удалось удалить метку: {e}"),
                };
            }
            None => {}
        }
        on_flag
    }

    /// Text field over a marker flag while it is being renamed (SPEC-030).
    fn marker_name_field(&mut self, ui: &egui::Ui, flag: Rect) {
        let mut done = None;
        egui::Area::new(ui.id().with("marker_rename"))
            .fixed_pos(flag.min)
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                let Some((_, text)) = self.marker_rename.as_mut() else {
                    return;
                };
                let field = ui.add(egui::TextEdit::singleline(text).desired_width(140.0));
                if !field.has_focus() && !field.lost_focus() {
                    field.request_focus();
                }
                if ui.input(|i| i.key_pressed(Key::Escape)) {
                    done = Some(false);
                } else if field.lost_focus() {
                    done = Some(true);
                }
            });
        match done {
            Some(true) => {
                if let Some((index, text)) = self.marker_rename.take() {
                    self.status = match self.editor.rename_marker(index, &text) {
                        Ok(()) => "Метка переименована".into(),
                        Err(e) => format!("Не удалось переименовать: {e}"),
                    };
                }
            }
            Some(false) => self.marker_rename = None,
            None => {}
        }
    }

    /// Adds a marker at the play cursor (SPEC-030).
    fn add_marker(&mut self) {
        self.status = match self.editor.add_marker(self.playhead) {
            Ok(index) => format!("Добавлена {}", self.editor.session().markers[index].name),
            Err(e) => format!("Не удалось добавить метку: {e}"),
        };
    }

    /// Moves the cursor to the previous or next marker (SPEC-030).
    fn jump_marker(&mut self, forward: bool) {
        let target = if forward {
            self.editor.next_marker(self.playhead)
        } else {
            self.editor.previous_marker(self.playhead)
        };
        if let Some(position) = target {
            self.seek(position);
        }
    }

    /// Presses on the ruler (SPEC-025, SPEC-040). The lower strip holds the
    /// cycle: drag its body to move it, its edges to resize, empty space to
    /// draw a new one, click it to switch it on or off. The upper strip
    /// seeks on click and scrubs while dragged.
    #[allow(clippy::too_many_lines)]
    fn ruler_input(&mut self, ui: &egui::Ui, response: &egui::Response, area: Rect) {
        let session = self.editor.session();
        let (rate, tempo) = (session.sample_rate, session.tempo_bpm);
        let per_bar = u32::from(session.time_signature.numerator);
        let free = ui.input(|i| i.modifiers.shift) || !self.snap;
        let zoom = self.zoom;
        let raw = |x: f32| x_to_samples(x - area.min.x, rate, zoom);
        let at = |x: f32| {
            if free {
                raw(x)
            } else {
                snap_to_beat(raw(x), rate, tempo)
            }
        };
        let Some(origin) = ui.input(press_pos) else {
            self.cycle_drag = None;
            return;
        };
        let cycle_x = self.editor.cycle().map(|(a, b)| {
            (
                area.min.x + samples_to_x(a, rate, zoom),
                area.min.x + samples_to_x(b, rate, zoom),
            )
        });
        let in_cycle_strip = origin.y >= area.min.y + RULER_HEIGHT * 0.55;
        if response.clicked() && self.cycle_drag.is_none() {
            if in_cycle_strip && timeline::cycle_hit(origin.x, cycle_x) != CycleHit::Empty {
                self.toggle_cycle();
            } else {
                self.seek(raw(origin.x));
            }
            return;
        }
        if self.cycle_drag.is_none() {
            self.cycle_drag = Some(if !in_cycle_strip {
                RulerDrag::Scrub
            } else {
                match (timeline::cycle_hit(origin.x, cycle_x), self.editor.cycle()) {
                    (CycleHit::Body, Some((start, end))) => RulerDrag::Move {
                        start,
                        end,
                        from: raw(origin.x),
                    },
                    (hit @ (CycleHit::Start | CycleHit::End), Some(_)) => RulerDrag::Resize(hit),
                    _ => RulerDrag::New(at(origin.x)),
                }
            });
        }
        if let (Some(drag), Some(p)) = (self.cycle_drag, ui.input(|i| i.pointer.latest_pos())) {
            let changed = match drag {
                RulerDrag::Scrub => {
                    self.seek(raw(p.x));
                    false
                }
                RulerDrag::New(anchor) => self.editor.set_cycle(anchor, at(p.x)).is_ok(),
                RulerDrag::Move { start, end, from } => {
                    let delta = i64::try_from(raw(p.x)).unwrap_or(i64::MAX)
                        - i64::try_from(from).unwrap_or(i64::MAX);
                    let (mut a, _) = timeline::move_cycle(start, end, delta);
                    if !free {
                        a = snap_to_beat(a, rate, tempo);
                    }
                    let len = end - start;
                    self.editor.set_cycle(a, a + len).is_ok()
                }
                RulerDrag::Resize(hit) => {
                    let (start, end) = self.editor.cycle().unwrap_or((0, 0));
                    let min = timeline::bar_samples(rate, tempo, per_bar) / 16;
                    let (a, b) = timeline::resize_cycle(start, end, hit, at(p.x), min);
                    self.editor.set_cycle(a, b).is_ok()
                }
            };
            if changed {
                self.sync_cycle();
            }
        }
        if response.drag_stopped() || !ui.input(|i| i.pointer.primary_down()) {
            let was_cycle = !matches!(self.cycle_drag, Some(RulerDrag::Scrub));
            self.cycle_drag = None;
            if was_cycle && let Some((from, to)) = self.editor.cycle() {
                self.status = format!(
                    "Цикл: такт {} – {}",
                    bars_in(from, rate, tempo, per_bar),
                    bars_in(to, rate, tempo, per_bar)
                );
            }
        }
    }

    /// Moves the playhead one bar back or forward (⏪/⏩, SPEC-040).
    pub(crate) fn step_bar(&mut self, forward: bool) {
        let session = self.editor.session();
        let bar = timeline::bar_samples(
            session.sample_rate,
            session.tempo_bpm,
            u32::from(session.time_signature.numerator),
        );
        let at = self
            .player
            .as_ref()
            .map_or(self.playhead, |p| p.control.position());
        self.seek(timeline::step_bar(at, bar, forward));
    }

    /// Chooser of the shown automation lane: gain, pan or any parameter of
    /// the track's effects (SPEC-041).
    fn lane_picker(&mut self, ui: &mut egui::Ui, track: usize) {
        let kinds: Vec<EffectKind> = self.editor.session().tracks[track]
            .effects
            .iter()
            .map(|effect| effect.kind)
            .collect();
        if !lane_fits(self.auto_lane, &kinds) {
            self.auto_lane = Lane::Gain;
        }
        let mut lanes = vec![Lane::Gain, Lane::Pan];
        for (slot, kind) in kinds.iter().enumerate() {
            let Ok(slot) = u8::try_from(slot) else { break };
            lanes.extend(
                EffectParam::of(kind)
                    .iter()
                    .map(|&param| Lane::Effect { slot, param }),
            );
        }
        let mut chosen = self.auto_lane;
        egui::ComboBox::from_id_salt(("auto-lane", track))
            .selected_text(lane_label(chosen, &kinds))
            .width(70.0)
            .show_ui(ui, |ui| {
                for lane in lanes {
                    ui.selectable_value(&mut chosen, lane, lane_label(lane, &kinds));
                }
            })
            .response
            .on_hover_text("Какую линию показывать");
        if chosen != self.auto_lane {
            self.auto_lane = chosen;
            self.auto_drag = None;
        }
    }

    /// Draws one track's automation lane over its clips (SPEC-023).
    #[allow(clippy::cast_precision_loss)]
    fn draw_lane(&self, painter: &egui::Painter, area: Rect, row: Rect, track: usize) {
        let rate = self.editor.session().sample_rate;
        let t = &self.editor.session().tracks[track];
        let points = t.automation.lane(self.auto_lane);
        let color = Color32::from_rgb(250, 180, 60);
        painter.rect_filled(row, 0.0, Color32::from_black_alpha(90));
        if points.is_empty() {
            let fixed = match self.auto_lane {
                Lane::Gain => t.gain,
                Lane::Pan => t.pan,
                Lane::Effect { slot, param } => {
                    let Some(value) = t
                        .effects
                        .get(usize::from(slot))
                        .and_then(|effect| param.get(&effect.kind))
                    else {
                        return;
                    };
                    value
                }
            };
            let y = value_to_y(self.auto_lane, fixed, row);
            let mut x = row.min.x;
            while x < row.max.x {
                painter.line_segment(
                    [Pos2::new(x, y), Pos2::new((x + 6.0).min(row.max.x), y)],
                    Stroke::new(1.0_f32, color),
                );
                x += 12.0;
            }
            return;
        }
        let at = |p: &AutoPoint| {
            Pos2::new(
                area.min.x + samples_to_x(p.time, rate, self.zoom),
                value_to_y(self.auto_lane, p.value, row),
            )
        };
        let first = at(&points[0]);
        let last = at(&points[points.len() - 1]);
        let mut line = vec![Pos2::new(row.min.x, first.y)];
        line.extend(points.iter().map(at));
        line.push(Pos2::new(row.max.x, last.y));
        painter.add(egui::Shape::line(line, Stroke::new(2.0_f32, color)));
        for p in points {
            painter.circle_filled(at(p), POINT_RADIUS, color);
        }
    }

    /// Mouse editing of the shown automation lane: click adds a point, drag
    /// moves one, right click removes one (SPEC-023).
    fn edit_lane(
        &mut self,
        ui: &egui::Ui,
        response: &egui::Response,
        area: Rect,
        row: Rect,
        track: usize,
    ) {
        let rate = self.editor.session().sample_rate;
        let tempo = self.editor.session().tempo_bpm;
        let lane = self.auto_lane;
        let snap = self.snap && !ui.input(|i| i.modifiers.shift);
        let zoom = self.zoom;
        let to_point = |p: Pos2| {
            let time = x_to_samples((p.x - area.min.x).max(0.0), rate, zoom);
            AutoPoint {
                time: if snap {
                    snap_to_beat(time, rate, tempo)
                } else {
                    time
                },
                value: y_to_value(lane, p.y, row),
            }
        };
        let points = self.editor.session().tracks[track]
            .automation
            .lane(lane)
            .to_vec();
        let hit = |p: Pos2| -> Option<usize> {
            points.iter().position(|point| {
                let x = area.min.x + samples_to_x(point.time, rate, zoom);
                let y = value_to_y(lane, point.value, row);
                Pos2::new(x, y).distance(p) <= POINT_RADIUS + 3.0
            })
        };

        if response.secondary_clicked() {
            if let Some(index) = response.interact_pointer_pos().and_then(hit) {
                let result = self.editor.remove_point(track, lane, index);
                self.report(result, "Точка удалена");
            }
            return;
        }
        if response.clicked() {
            if let Some(p) = response.interact_pointer_pos() {
                if hit(p).is_none() {
                    let result = self.editor.add_point(track, lane, to_point(p));
                    self.report(result, "Точка добавлена");
                }
            }
            return;
        }
        if response.drag_started() {
            let origin = ui.input(|i| i.pointer.press_origin());
            self.auto_drag = origin.and_then(|p| {
                hit(p).or_else(|| self.editor.add_point(track, lane, to_point(p)).ok())
            });
        }
        let Some(index) = self.auto_drag else { return };
        if response.drag_stopped() {
            self.auto_drag = None;
            self.status = "Автоматизация изменена".into();
            return;
        }
        ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
        if let Some(p) = response.interact_pointer_pos() {
            let point = to_point(Pos2::new(p.x, p.y.clamp(row.min.y, row.max.y)));
            if let Err(e) = self.editor.move_point(track, lane, index, point) {
                self.status = format!("Ошибка: {e}");
                self.auto_drag = None;
            }
        }
    }

    /// Mouse editing of clips: drag the body to move, drag an edge to trim.
    fn drag_clips(
        &mut self,
        ui: &egui::Ui,
        response: &egui::Response,
        area: Rect,
        rects: &[((usize, usize), Rect)],
    ) {
        let hit = |p: Pos2| rects.iter().rev().find(|(_, r)| r.contains(p)).copied();
        let tool = self.tool;
        let px = self.zoom / self.editor.session().sample_rate.max(1) as f32;
        let session = self.editor.session();
        let fades = |(t, c): (usize, usize)| {
            session.tracks[t].clips.get(c).map_or((0.0, 0.0), |clip| {
                (
                    clip.fade_in_samples as f32 * px,
                    clip.fade_out_samples as f32 * px,
                )
            })
        };
        let inside =
            |(t, c): (usize, usize)| daw_gui::layout::edges_in_overlap(&session.tracks[t].clips, c);
        let mode_at =
            |p: Pos2| hit(p).and_then(|(at, r)| press_mode(tool, r, p, fades(at), inside(at)));
        if self.drag.is_none() {
            if let Some(p) = response.hover_pos() {
                let icon = match (tool, mode_at(p)) {
                    (Tool::Scissors, _) => Some(CursorIcon::Crosshair),
                    (Tool::Glue, _) => Some(CursorIcon::PointingHand),
                    (_, Some(DragMode::TrimStart | DragMode::TrimEnd)) => {
                        Some(CursorIcon::ResizeHorizontal)
                    }
                    (_, Some(DragMode::FadeIn)) => Some(CursorIcon::ResizeNorthWest),
                    (_, Some(DragMode::FadeOut)) => Some(CursorIcon::ResizeNorthEast),
                    _ => None,
                };
                if let Some(icon) = icon {
                    ui.ctx().set_cursor_icon(icon);
                }
            }
        }

        let rate = self.editor.session().sample_rate;
        let tempo = self.editor.session().tempo_bpm;
        let snap = self.snap && !ui.input(|i| i.modifiers.shift);
        let fit = |samples: u64| {
            if snap {
                snap_to_beat(samples, rate, tempo)
            } else {
                samples
            }
        };
        if response.clicked() && matches!(tool, Tool::Scissors | Tool::Glue) {
            if let Some((((t, c), _), p)) = response
                .interact_pointer_pos()
                .and_then(|p| Some((hit(p)?, p)))
            {
                self.drag = None;
                self.note_drag = None;
                if tool == Tool::Scissors {
                    let at = fit(x_to_samples(p.x - area.min.x, rate, self.zoom));
                    self.status = match self.editor.split_clip(t, c, at) {
                        Ok(_) => "Клип разрезан".into(),
                        Err(EditorError::SplitOutside) => "Точка на краю клипа".into(),
                        Err(e) => format!("Не удалось разрезать: {e}"),
                    };
                } else {
                    self.join_clip(t, c);
                }
            }
            return;
        }
        if response.drag_started() {
            let origin = ui.input(|i| i.pointer.press_origin());
            if tool == Tool::Fade {
                if let Some(pair) = origin.and_then(|p| self.boundary_clip(hit(p)?, p)) {
                    self.drag = None;
                    self.crossfade_clip(pair.0, pair.1);
                    return;
                }
            }
            self.drag = origin.and_then(|p| {
                let ((track, clip), _) = hit(p)?;
                let start = self.editor.session().tracks[track].clips[clip].start_samples;
                let at = x_to_samples(p.x - area.min.x, rate, self.zoom);
                Some(Drag {
                    track,
                    clip,
                    mode: mode_at(p)?,
                    grab: at as i64 - start as i64,
                })
            });
            if let Some(d) = self.drag {
                let at = (d.track, d.clip);
                if self.editor.is_clip_selected(at) {
                    self.editor.focus_clip(at);
                } else {
                    let toggle = ui.input(|i| i.modifiers.shift || i.modifiers.command);
                    self.editor.select_clip(Some(at), toggle);
                }
            } else if tool == Tool::Pointer {
                if let Some(p) = origin.filter(|&p| hit(p).is_none()) {
                    let additive = ui.input(|i| i.modifiers.shift || i.modifiers.command);
                    self.band = Some((p, additive));
                }
            }
        }
        if self.band.is_some() {
            self.rubber_band(ui, response, area);
            return;
        }

        let Some(mut drag) = self.drag else { return };
        if response.drag_stopped() {
            self.drag = None;
            self.status = "Клип изменён".into();
            return;
        }
        let Some(p) = response.interact_pointer_pos() else {
            return;
        };
        ui.ctx().set_cursor_icon(match drag.mode {
            DragMode::Move => CursorIcon::Grabbing,
            DragMode::FadeIn => CursorIcon::ResizeNorthWest,
            DragMode::FadeOut => CursorIcon::ResizeNorthEast,
            _ => CursorIcon::ResizeHorizontal,
        });
        let at = x_to_samples((p.x - area.min.x).max(0.0), rate, self.zoom);
        let result = match drag.mode {
            DragMode::Move => {
                let start = fit((at as i64 - drag.grab).max(0) as u64);
                let tracks = self.editor.session().tracks.len();
                let row = ((p.y - area.min.y - RULER_HEIGHT) / TRACK_HEIGHT).floor();
                let to = if row < 0.0 {
                    0
                } else {
                    (row as usize).min(tracks.saturating_sub(1))
                };
                if self.editor.selected_clips().len() > 1 {
                    // SPEC-047 §4: the grabbed clip leads the whole group.
                    let now =
                        self.editor.session().tracks[drag.track].clips[drag.clip].start_samples;
                    let delta = start as i64 - now as i64;
                    let rows = to as isize - drag.track as isize;
                    self.editor.move_selected_clips(delta, rows).map(|()| {
                        if let Some((t, c)) = self.editor.selected_clip {
                            drag.track = t;
                            drag.clip = c;
                        }
                    })
                } else {
                    self.editor
                        .move_clip(drag.track, drag.clip, start, to)
                        .map(|(t, c)| {
                            drag.track = t;
                            drag.clip = c;
                        })
                }
            }
            DragMode::TrimStart => self.editor.trim_start(drag.track, drag.clip, fit(at)),
            DragMode::TrimEnd => self.editor.trim_end(drag.track, drag.clip, fit(at)),
            DragMode::FadeIn | DragMode::FadeOut => {
                let clip = &self.editor.session().tracks[drag.track].clips[drag.clip];
                let (start, end) = (clip.start_samples, clip.start_samples + clip.length_samples);
                let (fade_in, fade_out) = (clip.fade_in_samples, clip.fade_out_samples);
                // Fades follow the pointer freely: the grid is too coarse.
                if drag.mode == DragMode::FadeIn {
                    let len = at.saturating_sub(start);
                    let len = len.min(clip.length_samples - fade_out);
                    self.editor.set_fades(drag.track, drag.clip, len, fade_out)
                } else {
                    let len = end.saturating_sub(at);
                    let len = len.min(clip.length_samples - fade_in);
                    self.editor.set_fades(drag.track, drag.clip, fade_in, len)
                }
            }
        };
        if let Err(e) = result {
            self.status = format!("Ошибка: {e}");
        }
        self.drag = Some(drag);
    }

    /// The rubber band: a frame dragged over empty space selects every
    /// region it touches (SPEC-047 §2).
    fn rubber_band(&mut self, ui: &egui::Ui, response: &egui::Response, area: Rect) {
        let Some((origin, additive)) = self.band else {
            return;
        };
        let Some(p) = ui.input(|i| i.pointer.interact_pos()) else {
            self.band = None;
            return;
        };
        let rate = self.editor.session().sample_rate;
        let tracks = self.editor.session().tracks.len();
        let row = |y: f32| {
            ((y - area.min.y - RULER_HEIGHT) / TRACK_HEIGHT)
                .floor()
                .max(0.0) as usize
        };
        let (top, bottom) = (row(origin.y.min(p.y)), row(origin.y.max(p.y)));
        let rows = if top < tracks {
            top..=bottom.min(tracks - 1)
        } else {
            // An empty range: the band is below the last track.
            #[allow(clippy::reversed_empty_ranges, reason = "selects nothing on purpose")]
            let none = 1..=0;
            none
        };
        let from = x_to_samples((origin.x - area.min.x).max(0.0), rate, self.zoom);
        let to = x_to_samples((p.x - area.min.x).max(0.0), rate, self.zoom);
        let done = response.drag_stopped() || !ui.input(|i| i.pointer.primary_down());
        if !additive || done {
            let count = self.editor.select_clips_in(rows, from, to, additive);
            if done {
                self.band = None;
                self.status = format!(
                    "Выбрано регионов: {}",
                    self.editor.selected_clips().len().max(count)
                );
                return;
            }
        }
        let frame = Rect::from_two_pos(origin, p).intersect(area);
        let painter = ui.painter_at(area);
        painter.rect_filled(
            frame,
            0.0,
            Color32::from_rgba_unmultiplied(255, 200, 60, 28),
        );
        painter.rect_stroke(
            frame,
            0.0,
            Stroke::new(1.0_f32, Color32::from_rgb(255, 200, 60)),
            StrokeKind::Inside,
        );
    }

    fn open_new_track(&mut self, kind: TrackKind) {
        self.new_track = Some(shell::NewTrack::new(kind));
    }

    /// Empty MIDI region at the bar under `at` on an instrument track.
    fn new_region_on(&mut self, track: usize, at: u64) {
        let is_instrument = self
            .editor
            .session()
            .tracks
            .get(track)
            .is_some_and(|t| t.kind == TrackKind::Instrument);
        if !is_instrument {
            return;
        }
        let (_, length) = self.editor.new_midi_region(at);
        let start = self.editor.new_clip_start(at);
        let result = self.editor.add_midi_region(track, start, length);
        if let Some(at) = self.report(result, "MIDI-регион создан") {
            self.open_piano(at);
        }
    }

    fn add_midi_track(&mut self) {
        let (start, length) = self.editor.new_midi_region(self.playhead);
        let result = self.editor.add_midi_track(start, length);
        if let Some(at) = self.report(result, "MIDI-дорожка добавлена") {
            self.open_piano(at);
            self.reveal_track = Some(at.0);
        }
    }

    /// Piano roll for a MIDI clip, the audio editor otherwise.
    fn open_clip_editor(&mut self, at: (usize, usize)) {
        let midi = self
            .editor
            .session()
            .tracks
            .get(at.0)
            .and_then(|track| track.clips.get(at.1))
            .is_some_and(audio_core::session::Clip::is_midi);
        if midi {
            self.open_piano(at);
        } else {
            self.open_audio_editor(at);
        }
    }

    fn close_piano(&mut self) {
        self.piano = None;
        self.note_drag = None;
    }

    fn open_piano(&mut self, at: (usize, usize)) {
        self.audio_edit = audio_editor::AudioEdit::default();
        self.piano = Some(at);
        self.note_drag = None;
        self.piano_scroll = true;
        self.editor.selected_clip = Some(at);
        self.editor.selected_track = Some(at.0);
        self.layout.show_editor();
    }

    /// The open piano roll clip, closed if it is gone or no longer MIDI.
    fn piano_clip(&mut self) -> Option<(usize, usize)> {
        let (t, c) = self.piano?;
        let ok = self
            .editor
            .session()
            .tracks
            .get(t)
            .and_then(|track| track.clips.get(c))
            .is_some_and(audio_core::session::Clip::is_midi);
        if !ok {
            self.piano = None;
            self.note_drag = None;
        }
        self.piano
    }

    fn piano_roll(&mut self, ui: &mut egui::Ui) {
        let Some((t, c)) = self.piano_clip() else {
            return;
        };
        ui.horizontal(|ui| {
            let name = &self.editor.session().tracks[t].name;
            ui.label(egui::RichText::new(format!("Piano roll — {name}")).strong());
            ui.label("клик — нота, правый клик — удалить, тяните — сдвиг");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("✕").on_hover_text("Закрыть").clicked() {
                    self.piano = None;
                    self.note_drag = None;
                }
            });
        });
        if self.piano.is_none() {
            return;
        }
        let mut scroll = egui::ScrollArea::both().id_salt("piano");
        if std::mem::take(&mut self.piano_scroll) {
            scroll = scroll.vertical_scroll_offset((127.0 - 72.0) * KEY_HEIGHT);
        }
        if let Some(offset) = self.piano_offset.take() {
            scroll = scroll.horizontal_scroll_offset(offset);
        }
        scroll.show(ui, |ui| self.piano_grid(ui, t, c));
    }

    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::too_many_lines
    )]
    fn piano_grid(&mut self, ui: &mut egui::Ui, t: usize, c: usize) {
        let session = self.editor.session();
        let step = sixteenth_samples(session.sample_rate, session.tempo_bpm);
        let clip = &session.tracks[t].clips[c];
        let content = clip.source_length;
        let notes = clip.notes.clone().unwrap_or_default();
        let steps = content.div_ceil(step);
        let step_px = STEP_PX * self.piano_zoom;
        let px_per_sample = step_px / step as f32;
        let width = KEYS_PX + content as f32 * px_per_sample + 2.0;
        let (area, response) = ui.allocate_exact_size(
            Vec2::new(width.max(ui.available_width()), KEY_HEIGHT * 128.0),
            Sense::click_and_drag(),
        );
        if response.hovered() {
            let factor = ui.input(egui::InputState::zoom_delta);
            if (factor - 1.0).abs() > f32::EPSILON {
                let old = self.piano_zoom;
                self.piano_zoom = daw_gui::timeline::piano_zoom(old, factor);
                let offset = (ui.clip_rect().min.x - area.min.x).max(0.0);
                let anchor = ui
                    .input(|i| i.pointer.hover_pos())
                    .map_or(0.0, |p| p.x - ui.clip_rect().min.x);
                self.piano_offset = Some(daw_gui::timeline::keyed_zoom_offset(
                    old,
                    self.piano_zoom,
                    offset,
                    anchor,
                    KEYS_PX,
                ));
            }
        }
        let painter = ui.painter_at(area);
        let grid_x = area.min.x + KEYS_PX;
        let row_y = |pitch: u8| area.min.y + f32::from(127 - pitch) * KEY_HEIGHT;
        painter.rect_filled(area, 0.0, Color32::from_gray(26));
        for pitch in 0..=127_u8 {
            if is_black_key(pitch) {
                let y = row_y(pitch);
                painter.rect_filled(
                    Rect::from_min_size(
                        Pos2::new(grid_x, y),
                        Vec2::new(width - KEYS_PX, KEY_HEIGHT),
                    ),
                    0.0,
                    Color32::from_gray(21),
                );
            }
        }
        for s in 0..=steps.min(100_000) {
            let x = grid_x + s as f32 * step_px;
            let gray = if s % 16 == 0 {
                80
            } else if s % 4 == 0 {
                52
            } else {
                36
            };
            painter.line_segment(
                [Pos2::new(x, area.min.y), Pos2::new(x, area.max.y)],
                Stroke::new(1.0_f32, Color32::from_gray(gray)),
            );
        }
        let mut note_rects = Vec::with_capacity(notes.len());
        for (i, n) in notes.iter().enumerate() {
            let rect = Rect::from_min_size(
                Pos2::new(
                    grid_x + n.start as f32 * px_per_sample,
                    row_y(n.pitch) + 1.0,
                ),
                Vec2::new((n.length as f32 * px_per_sample).max(3.0), KEY_HEIGHT - 2.0),
            );
            let dragged = self.note_drag.is_some_and(|d| d.index == i);
            let fill = if dragged {
                Color32::from_rgb(255, 200, 60)
            } else {
                Color32::from_rgb(110, 200, 120)
            };
            painter.rect_filled(rect, 2.0, fill);
            painter.rect_stroke(
                rect,
                2.0,
                Stroke::new(1.0_f32, Color32::from_gray(10)),
                StrokeKind::Inside,
            );
            note_rects.push(rect);
        }
        // Keys stay visible while the grid scrolls sideways.
        let keys_x = area.min.x.max(ui.clip_rect().min.x);
        let keys = ui.painter();
        for pitch in 0..=127_u8 {
            let rect = Rect::from_min_size(
                Pos2::new(keys_x, row_y(pitch)),
                Vec2::new(KEYS_PX, KEY_HEIGHT),
            );
            let fill = if is_black_key(pitch) {
                Color32::from_gray(30)
            } else {
                Color32::from_gray(210)
            };
            keys.rect_filled(rect, 0.0, fill);
            keys.rect_stroke(
                rect,
                0.0,
                Stroke::new(0.5_f32, Color32::from_gray(90)),
                StrokeKind::Inside,
            );
            if pitch % 12 == 0 {
                keys.text(
                    rect.right_center() - Vec2::new(3.0, 0.0),
                    Align2::RIGHT_CENTER,
                    format!("C{}", i32::from(pitch / 12) - 1),
                    FontId::monospace(9.0),
                    Color32::BLACK,
                );
            }
        }

        let hit = |p: Pos2| note_rects.iter().rposition(|r| r.contains(p));
        let at_pos = |p: Pos2| {
            let samples = ((p.x - grid_x).max(0.0) / px_per_sample) as u64;
            let row = ((p.y - area.min.y) / KEY_HEIGHT).floor().clamp(0.0, 127.0) as u8;
            (samples, 127 - row)
        };
        let pointer = response.interact_pointer_pos();

        if response.secondary_clicked() {
            if let Some(index) = pointer.and_then(hit) {
                let result = self.editor.delete_note(t, c, index);
                self.report(result, "Нота удалена");
            }
            return;
        }
        if response.clicked() {
            if let Some(p) = pointer.filter(|p| p.x >= grid_x && hit(*p).is_none()) {
                let (samples, pitch) = at_pos(p);
                let start = samples / step * step;
                let length = step.min(content.saturating_sub(start));
                if length > 0 {
                    let note = Note {
                        pitch,
                        start,
                        length,
                        velocity: NEW_VELOCITY,
                    };
                    let result = self.editor.add_note(t, c, note);
                    self.report(result, "Нота добавлена");
                }
            }
            return;
        }
        if response.drag_started() {
            let origin = ui.input(|i| i.pointer.press_origin());
            self.note_drag = origin.and_then(|p| {
                let index = hit(p)?;
                let (samples, _) = at_pos(p);
                Some(NoteDrag {
                    index,
                    grab: samples as i64 - notes[index].start as i64,
                })
            });
        }
        let Some(mut drag) = self.note_drag else {
            return;
        };
        if response.drag_stopped() {
            self.note_drag = None;
            self.status = "Нота сдвинута".into();
            return;
        }
        let (Some(p), Some(note)) = (pointer, notes.get(drag.index)) else {
            return;
        };
        ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
        let (samples, pitch) = at_pos(p);
        let raw = (samples as i64 - drag.grab).max(0) as u64;
        let latest = content.saturating_sub(note.length);
        let start = ((raw + step / 2) / step * step).min(latest);
        if start != note.start || pitch != note.pitch {
            match self.editor.move_note(t, c, drag.index, start, pitch) {
                Ok(index) => drag.index = index,
                Err(e) => self.status = format!("Ошибка: {e}"),
            }
        }
        self.note_drag = Some(drag);
    }

    fn undo(&mut self) {
        self.drag = None;
        self.note_drag = None;
        self.status = if self.editor.undo() {
            "Отменено".into()
        } else {
            "Нечего отменять".into()
        };
    }

    fn redo(&mut self) {
        self.drag = None;
        self.note_drag = None;
        self.status = if self.editor.redo() {
            "Повторено".into()
        } else {
            "Нечего повторять".into()
        };
    }

    /// Right-click menu of the tracks area (SPEC-037): region commands for
    /// the region under the pointer, paste and new track on empty space.
    fn region_menu(&mut self, response: &egui::Response) {
        let mut action = None;
        let selected = self.editor.selected_clip;
        let midi = selected.is_some_and(|(t, c)| {
            self.editor
                .session()
                .tracks
                .get(t)
                .and_then(|track| track.clips.get(c))
                .is_some_and(audio_core::session::Clip::is_midi)
        });
        response.context_menu(|ui| {
            ui.set_min_width(190.0);
            let mut item = |ui: &mut egui::Ui, label: &str, hint: &str, a: RegionAction| {
                if ui
                    .add(egui::Button::new(label).shortcut_text(hint))
                    .clicked()
                {
                    action = Some(a);
                    ui.close();
                }
            };
            if selected.is_some() {
                if midi {
                    item(ui, "Открыть в пиано-ролле", "", RegionAction::Piano);
                } else {
                    item(ui, "Открыть в редакторе", "", RegionAction::Piano);
                }
                ui.separator();
                item(ui, "Копировать", "Ctrl+C", RegionAction::Copy);
                item(ui, "Вставить", "Ctrl+V", RegionAction::Paste);
                item(ui, "Дублировать", "Ctrl+D", RegionAction::Duplicate);
                item(ui, "Разрезать по курсору", "S", RegionAction::Split);
                item(ui, "Склеить со следующим", "J", RegionAction::Join);
                ui.separator();
                item(ui, "Фейд-ин 10 мс", "", RegionAction::FadeIn);
                item(ui, "Фейд-аут 10 мс", "", RegionAction::FadeOut);
                item(ui, "Убрать фейды", "", RegionAction::NoFades);
                ui.separator();
                ui.separator();
                for stub in ["Переименовать", "Цвет", "Квантизация", "Зациклить"]
                {
                    ui.add_enabled(false, egui::Button::new(stub));
                }
                ui.separator();
                item(ui, "Удалить", "Del", RegionAction::Delete);
            } else {
                item(ui, "Вставить", "Ctrl+V", RegionAction::Paste);
                ui.add_enabled(false, egui::Button::new("Выделить всё"));
            }
        });
        match action {
            Some(RegionAction::Piano) => {
                if let Some(at) = selected {
                    self.open_clip_editor(at);
                }
            }
            Some(RegionAction::Copy) => self.copy_clip(&response.ctx),
            Some(RegionAction::Paste) => self.paste_clip(),
            Some(RegionAction::Duplicate) => self.duplicate_clip(),
            Some(RegionAction::Split) => self.split_clip(),
            Some(RegionAction::Delete) => self.delete_selected_clip(),
            Some(RegionAction::Join) => {
                if let Some((t, c)) = selected {
                    self.join_clip(t, c);
                }
            }
            Some(a @ (RegionAction::FadeIn | RegionAction::FadeOut | RegionAction::NoFades)) => {
                if let Some((t, c)) = selected {
                    self.quick_fade(t, c, a);
                }
            }
            None => {}
        }
    }

    /// Joins a clip with the next one on its track (SPEC-038 §5).
    /// Clip to crossfade when the Fade tool presses within
    /// [`XFADE_GRAB`] pixels of a boundary with a touching neighbour
    /// (SPEC-043 §2): the left clip of the pair.
    fn boundary_clip(
        &self,
        ((t, c), r): ((usize, usize), Rect),
        p: Pos2,
    ) -> Option<(usize, usize)> {
        let clips = &self.editor.session().tracks.get(t)?.clips;
        let clip = clips.get(c)?;
        if r.max.x - p.x <= XFADE_GRAB {
            let end = clip.end_samples();
            clips
                .iter()
                .enumerate()
                .any(|(i, o)| {
                    i != c && o.start_samples > clip.start_samples && o.start_samples <= end
                })
                .then_some((t, c))
        } else if p.x - r.min.x <= XFADE_GRAB {
            clips
                .iter()
                .enumerate()
                .filter(|&(i, o)| {
                    i != c
                        && o.start_samples < clip.start_samples
                        && o.end_samples() >= clip.start_samples
                })
                .max_by_key(|(_, o)| o.start_samples)
                .map(|(i, _)| (t, i))
        } else {
            None
        }
    }

    /// Crossfade of a clip with its neighbour, with a status message.
    fn crossfade_clip(&mut self, track: usize, clip: usize) {
        self.drag = None;
        self.note_drag = None;
        self.status = match self.editor.crossfade(track, clip) {
            Ok(()) => "Кроссфейд создан".into(),
            Err(EditorError::CannotCrossfade) => "Нет соседнего клипа для кроссфейда".into(),
            Err(e) => format!("Не удалось: {e}"),
        };
    }

    fn join_clip(&mut self, track: usize, clip: usize) {
        self.drag = None;
        self.note_drag = None;
        self.status = match self.editor.join_clips(track, clip) {
            Ok(_) => "Клипы склеены".into(),
            Err(EditorError::CannotJoin) => "Эти клипы нельзя склеить".into(),
            Err(e) => format!("Не удалось склеить: {e}"),
        };
    }

    /// Region menu fades: 10 ms in or out, or none (SPEC-038 §8).
    fn quick_fade(&mut self, track: usize, clip: usize, action: RegionAction) {
        let ten_ms = u64::from(self.editor.session().sample_rate / 100);
        let Some(c) = self
            .editor
            .session()
            .tracks
            .get(track)
            .and_then(|t| t.clips.get(clip))
        else {
            return;
        };
        let (fade_in, fade_out) = match action {
            RegionAction::FadeIn => (ten_ms, c.fade_out_samples),
            RegionAction::FadeOut => (c.fade_in_samples, ten_ms),
            _ => (0, 0),
        };
        if let Err(e) = self.editor.set_fades(track, clip, fade_in, fade_out) {
            self.status = format!("Ошибка: {e}");
        }
    }

    fn delete_selected_clip(&mut self) {
        self.drag = None;
        match self.editor.delete_selected_clips() {
            0 => {}
            1 => self.status = "Клип удалён".into(),
            n => self.status = format!("Удалено регионов: {n}"),
        }
    }

    fn split_clip(&mut self) {
        let Some((t, c)) = self.editor.selected_clip else {
            self.status = "Выберите клип".into();
            return;
        };
        self.drag = None;
        self.note_drag = None;
        self.status = match self.editor.split_clip(t, c, self.playhead) {
            Ok(_) => "Клип разрезан".into(),
            Err(EditorError::SplitOutside) => "Курсор вне клипа".into(),
            Err(e) => format!("Не удалось разрезать: {e}"),
        };
    }

    fn copy_clip(&mut self, ctx: &egui::Context) {
        let Some((t, c)) = self.editor.selected_clip else {
            self.status = "Выберите клип".into();
            return;
        };
        self.status = match self.editor.copy_clip(t, c) {
            Ok(()) => {
                // egui only reports Ctrl+V when the system clipboard holds text.
                ctx.copy_text("Sound Pro: клип".into());
                "Скопировано".into()
            }
            Err(e) => format!("Не удалось скопировать: {e}"),
        };
    }

    fn paste_clip(&mut self) {
        let Some(track) = self.editor.selected_track else {
            self.status = "Выберите дорожку".into();
            return;
        };
        self.drag = None;
        self.note_drag = None;
        self.status = match self.editor.paste_clip(track, self.playhead) {
            Ok(_) => "Вставлено".into(),
            Err(EditorError::EmptyClipboard) => "Буфер пуст".into(),
            Err(e) => format!("Не удалось вставить: {e}"),
        };
    }

    fn duplicate_clip(&mut self) {
        let Some((t, c)) = self.editor.selected_clip else {
            self.status = "Выберите клип".into();
            return;
        };
        self.drag = None;
        self.note_drag = None;
        self.status = match self.editor.duplicate_clip(t, c) {
            Ok(_) => "Продублировано".into(),
            Err(e) => format!("Не удалось дублировать: {e}"),
        };
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::F11)) {
            let full = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!full));
        }
        if !typing(ctx) {
            let (space, home, record, cycle, click) = ctx.input_mut(|i| {
                (
                    i.consume_key(egui::Modifiers::NONE, Key::Space),
                    i.consume_key(egui::Modifiers::NONE, Key::Home),
                    i.consume_key(egui::Modifiers::NONE, Key::R),
                    i.consume_key(egui::Modifiers::NONE, Key::C),
                    i.consume_key(egui::Modifiers::NONE, Key::K),
                )
            });
            let (marker, back, forward, bar_back, bar_forward, enter) = ctx.input_mut(|i| {
                (
                    i.consume_key(egui::Modifiers::NONE, Key::M),
                    i.consume_key(egui::Modifiers::ALT, Key::Comma),
                    i.consume_key(egui::Modifiers::ALT, Key::Period),
                    i.consume_key(egui::Modifiers::NONE, Key::Comma),
                    i.consume_key(egui::Modifiers::NONE, Key::Period),
                    i.consume_key(egui::Modifiers::NONE, Key::Enter),
                )
            });
            let xfade = ctx.input_mut(|i| {
                i.consume_key(egui::Modifiers::COMMAND | egui::Modifiers::ALT, Key::X)
            });
            if ctx.input_mut(|i| {
                i.consume_key(egui::Modifiers::COMMAND | egui::Modifiers::ALT, Key::N)
            }) {
                self.open_new_track(TrackKind::Audio);
            }
            if xfade {
                if let Some((t, c)) = self.editor.selected_clip {
                    self.crossfade_clip(t, c);
                }
            }
            if bar_back || bar_forward {
                self.step_bar(bar_forward);
            }
            if enter {
                self.rewind();
            }
            let (mixer, editors, inspector, library, smart, browser) = ctx.input_mut(|i| {
                (
                    i.consume_key(egui::Modifiers::NONE, Key::X),
                    i.consume_key(egui::Modifiers::NONE, Key::E),
                    i.consume_key(egui::Modifiers::NONE, Key::I),
                    i.consume_key(egui::Modifiers::NONE, Key::Y),
                    i.consume_key(egui::Modifiers::NONE, Key::B),
                    i.consume_key(egui::Modifiers::NONE, Key::F),
                )
            });
            if mixer {
                self.layout.toggle_bottom(Bottom::Mixer);
            }
            if editors {
                self.layout.toggle_bottom(Bottom::Editor);
            }
            if smart {
                self.layout.toggle_bottom(Bottom::SmartControls);
            }
            self.layout.inspector ^= inspector;
            self.layout.library ^= library;
            self.layout.browser ^= browser;
            if marker {
                self.add_marker();
            }
            if back || forward {
                self.jump_marker(forward);
            }
            let (split, duplicate, copy, paste) = ctx.input_mut(|i| {
                let copy = i.events.iter().any(|e| matches!(e, egui::Event::Copy));
                let paste = i.events.iter().any(|e| matches!(e, egui::Event::Paste(_)));
                (
                    i.consume_key(egui::Modifiers::NONE, Key::S),
                    i.consume_key(egui::Modifiers::COMMAND, Key::D),
                    copy || i.consume_key(egui::Modifiers::COMMAND, Key::C),
                    paste || i.consume_key(egui::Modifiers::COMMAND, Key::V),
                )
            });
            let (tool, pointer, join) = ctx.input_mut(|i| {
                (
                    i.consume_key(egui::Modifiers::NONE, Key::T),
                    i.key_pressed(Key::Escape),
                    i.consume_key(egui::Modifiers::NONE, Key::J),
                )
            });
            if tool {
                self.tool = self.tool.next();
                self.drag = None;
            }
            if pointer {
                self.tool = Tool::Pointer;
            }
            if join {
                if let Some((t, c)) = self.editor.selected_clip {
                    self.join_clip(t, c);
                }
            }
            if split {
                self.split_clip();
            }
            if duplicate {
                self.duplicate_clip();
            }
            if copy {
                self.copy_clip(ctx);
            }
            if paste {
                self.paste_clip();
            }
            if click {
                self.toggle_metronome();
            }
            if cycle {
                self.toggle_cycle();
            }
            if record {
                self.toggle_record();
            }
            if space {
                self.toggle_play();
            }
            if home {
                self.rewind();
            }
        }
        let in_text = typing(ctx);
        let (delete, save, open, import) = ctx.input_mut(|i| {
            (
                // SPEC-037: Delete and Backspace both remove the region,
                // unless a text field is taking the keys.
                !in_text
                    && (i.consume_key(egui::Modifiers::NONE, Key::Delete)
                        || i.consume_key(egui::Modifiers::NONE, Key::Backspace)),
                i.consume_key(egui::Modifiers::COMMAND, Key::S),
                i.consume_key(egui::Modifiers::COMMAND, Key::O),
                i.consume_key(egui::Modifiers::COMMAND, Key::I),
            )
        });
        // SPEC-047 §6: Ctrl+A selects every region.
        if !in_text && ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, Key::A)) {
            self.editor.select_all_clips();
            self.status = format!("Выбрано регионов: {}", self.editor.selected_clips().len());
        }
        if delete {
            self.delete_selected_clip();
        }
        if save {
            self.save();
        }
        if open {
            self.open_dialog();
        }
        if import {
            self.import_dialog();
        }
        if !typing(ctx) {
            let shift = egui::Modifiers::COMMAND | egui::Modifiers::SHIFT;
            let (redo, undo) = ctx.input_mut(|i| {
                (
                    i.consume_key(shift, Key::Z) || i.consume_key(egui::Modifiers::COMMAND, Key::Y),
                    i.consume_key(egui::Modifiers::COMMAND, Key::Z),
                )
            });
            if redo {
                self.redo();
            } else if undo {
                self.undo();
            }
        }
    }
}

/// Fall speed of the shown gain reduction, dB per second (SPEC-052).
const REDUCTION_FALL_DB_PER_S: f32 = 30.0;

/// The next shown gain reduction: jumps up to a larger `fresh` value,
/// otherwise falls towards it at [`REDUCTION_FALL_DB_PER_S`].
fn reduction_step(shown: f32, fresh: f32, dt: f32) -> f32 {
    if fresh >= shown {
        fresh
    } else {
        (shown - REDUCTION_FALL_DB_PER_S * dt).max(fresh)
    }
}

/// Text of the gain-reduction readout: «—» when nothing is reduced.
fn reduction_text(db: f32) -> String {
    if db < 0.05 {
        "—".into()
    } else {
        format!("{db:.1} дБ")
    }
}

/// Russian label of an effect type for the effects window.
const fn effect_title(kind: &EffectKind) -> &'static str {
    match kind {
        EffectKind::Eq { .. } => "Эквалайзер",
        EffectKind::Compressor { .. } => "Компрессор",
        EffectKind::Reverb { .. } => "Реверб",
        EffectKind::ChannelEq { .. } => "Канальный эквалайзер",
        EffectKind::Gate { .. } => "Гейт",
        EffectKind::Limiter { .. } => "Лимитер",
    }
}

/// Band names of the channel EQ in index order (SPEC-051).
const CHANNEL_EQ_BANDS: [&str; 8] = [
    "ФВЧ",
    "Низ. полка",
    "Пик 1",
    "Пик 2",
    "Пик 3",
    "Пик 4",
    "Верх. полка",
    "ФНЧ",
];

/// Sliders for every parameter of `kind`; `true` if one moved.
fn effect_sliders(ui: &mut egui::Ui, kind: &mut EffectKind) -> bool {
    let mut changed = false;
    let mut toggled = false;
    let mut slider = |ui: &mut egui::Ui,
                      value: &mut f32,
                      range: std::ops::RangeInclusive<f32>,
                      text: &str,
                      log: bool| {
        changed |= ui
            .add(egui::Slider::new(value, range).text(text).logarithmic(log))
            .changed();
    };
    match kind {
        EffectKind::Eq {
            low_db,
            mid_db,
            mid_hz,
            high_db,
        } => {
            slider(ui, low_db, -24.0..=24.0, "низ, дБ", false);
            slider(ui, mid_db, -24.0..=24.0, "середина, дБ", false);
            slider(ui, mid_hz, 100.0..=10_000.0, "середина, Гц", true);
            slider(ui, high_db, -24.0..=24.0, "верх, дБ", false);
        }
        EffectKind::Compressor {
            threshold_db,
            ratio,
            attack_ms,
            release_ms,
            makeup_db,
            knee_db,
        } => {
            slider(ui, threshold_db, -60.0..=0.0, "порог, дБ", false);
            slider(ui, ratio, 1.0..=20.0, "степень", true);
            slider(ui, attack_ms, 0.1..=100.0, "атака, мс", true);
            slider(ui, release_ms, 10.0..=1000.0, "спад, мс", true);
            slider(ui, knee_db, 0.0..=24.0, "колено, дБ", false);
            slider(ui, makeup_db, 0.0..=24.0, "усиление, дБ", false);
        }
        EffectKind::Gate {
            threshold_db,
            range_db,
            attack_ms,
            hold_ms,
            release_ms,
        } => {
            slider(ui, threshold_db, -80.0..=0.0, "порог, дБ", false);
            slider(ui, range_db, 0.0..=80.0, "глубина, дБ", false);
            slider(ui, attack_ms, 0.1..=100.0, "атака, мс", true);
            slider(ui, hold_ms, 0.0..=500.0, "удержание, мс", false);
            slider(ui, release_ms, 10.0..=1000.0, "спад, мс", true);
        }
        EffectKind::Limiter {
            ceiling_db,
            release_ms,
        } => {
            slider(ui, ceiling_db, -24.0..=0.0, "потолок, дБ", false);
            slider(ui, release_ms, 10.0..=1000.0, "спад, мс", true);
        }
        EffectKind::Reverb { room, damp, mix } => {
            slider(ui, room, 0.0..=1.0, "комната", false);
            slider(ui, damp, 0.0..=1.0, "глушение", false);
            slider(ui, mix, 0.0..=1.0, "доля", false);
        }
        EffectKind::ChannelEq { bands } => {
            for (index, band) in bands.iter_mut().enumerate() {
                let name = CHANNEL_EQ_BANDS[index];
                ui.horizontal(|ui| {
                    toggled |= ui.checkbox(&mut band.enabled, name).changed();
                    slider(ui, &mut band.hz, 20.0..=20_000.0, "Гц", true);
                    if (1..=6).contains(&index) {
                        slider(ui, &mut band.db, -24.0..=24.0, "дБ", false);
                    }
                    if (2..=5).contains(&index) {
                        slider(ui, &mut band.q, 0.1..=18.0, "Q", true);
                    }
                });
            }
        }
    }
    changed || toggled
}

impl App {
    /// The effects window of the track or bus chosen with its «FX» button.
    fn effects_window(&mut self, ctx: &egui::Context) {
        let Some(target) = self.fx else {
            return;
        };
        let session = self.editor.session();
        let title = match target {
            FxTarget::Track(index) => session
                .tracks
                .get(index)
                .map(|t| format!("Эффекты — {}", t.name)),
            FxTarget::Bus(index) => session
                .buses
                .get(index)
                .map(|b| format!("Эффекты шины — {}", b.name)),
        };
        let Some(title) = title else {
            self.fx = None;
            return;
        };
        let mut open = true;
        egui::Window::new(title)
            .id(egui::Id::new("effects"))
            .open(&mut open)
            .resizable(true)
            .default_width(320.0)
            .show(ctx, |ui| self.effects_ui(ui, target));
        if !open {
            self.fx = None;
        }
    }

    fn effects_ui(&mut self, ui: &mut egui::Ui, target: FxTarget) {
        ui.horizontal(|ui| {
            let mut add = None;
            if ui.button("+ EQ").clicked() {
                add = Some(EffectKind::eq());
            }
            if ui.button("+ Компрессор").clicked() {
                add = Some(EffectKind::compressor());
            }
            if ui.button("+ Реверб").clicked() {
                add = Some(EffectKind::reverb());
            }
            if ui.button("+ Канальный эквалайзер").clicked() {
                add = Some(EffectKind::channel_eq());
            }
            if ui.button("+ Гейт").clicked() {
                add = Some(EffectKind::gate());
            }
            if ui.button("+ Лимитер").clicked() {
                add = Some(EffectKind::limiter());
            }
            if let Some(kind) = add {
                let added = match target {
                    FxTarget::Track(track) => self.editor.add_effect(track, kind),
                    FxTarget::Bus(bus) => self.editor.add_bus_effect(bus, kind),
                };
                self.status = match added {
                    Ok(_) => "Эффект добавлен".into(),
                    Err(_) => format!("Не больше {MAX_EFFECTS_PER_TRACK} эффектов"),
                };
            }
        });
        let session = self.editor.session();
        let (effects, movable) = match target {
            FxTarget::Track(track) => (session.tracks[track].effects.clone(), true),
            FxTarget::Bus(bus) => (session.buses[bus].effects.clone(), false),
        };
        if effects.is_empty() {
            ui.label("Эффектов нет. Звук идёт без обработки.");
        }
        let last = effects.len().saturating_sub(1);
        let reduction = match target {
            FxTarget::Track(track) => self.track_reduction.get(track).copied(),
            FxTarget::Bus(_) => None,
        };
        let mut action = None;
        egui::ScrollArea::vertical().show(ui, |ui| {
            for (index, effect) in effects.into_iter().enumerate() {
                ui.separator();
                let mut edited = effect;
                let mut changed = false;
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(effect_title(&effect.kind)).strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("✕").on_hover_text("Удалить").clicked() {
                            action = Some(FxAction::Remove(index));
                        }
                        if movable {
                            if ui
                                .add_enabled(index < last, egui::Button::new("▼").small())
                                .clicked()
                            {
                                action = Some(FxAction::Move(index, index + 1));
                            }
                            if ui
                                .add_enabled(index > 0, egui::Button::new("▲").small())
                                .clicked()
                            {
                                action = Some(FxAction::Move(index, index - 1));
                            }
                        }
                        changed |= ui.checkbox(&mut edited.bypass, "Обход").changed();
                    });
                });
                ui.add_enabled_ui(!edited.bypass, |ui| {
                    changed |= effect_sliders(ui, &mut edited.kind);
                });
                if matches!(effect.kind, EffectKind::Compressor { .. })
                    && let Some(shown) = reduction
                {
                    let db = shown.get(index).copied().unwrap_or(0.0);
                    ui.horizontal(|ui| {
                        ui.label("Снижение усиления:");
                        ui.add(
                            egui::ProgressBar::new((db / 24.0).clamp(0.0, 1.0))
                                .desired_width(120.0)
                                .text(reduction_text(db)),
                        );
                    });
                }
                if changed {
                    action = Some(FxAction::Set(index, edited));
                }
            }
        });
        let Some(action) = action else {
            return;
        };
        let result = match (target, action) {
            (FxTarget::Track(track), FxAction::Remove(index)) => {
                self.editor.remove_effect(track, index)
            }
            (FxTarget::Track(track), FxAction::Move(from, to)) => {
                self.editor.move_effect(track, from, to)
            }
            (FxTarget::Track(track), FxAction::Set(index, effect)) => {
                self.editor.set_effect(track, index, effect)
            }
            (FxTarget::Bus(bus), FxAction::Remove(index)) => {
                self.editor.remove_bus_effect(bus, index)
            }
            (FxTarget::Bus(_), FxAction::Move(..)) => Ok(()),
            (FxTarget::Bus(bus), FxAction::Set(index, effect)) => {
                self.editor.set_bus_effect(bus, index, effect)
            }
        };
        if let Err(error) = result {
            self.status = error.to_string();
        }
    }

    fn open_audio_settings(&mut self) {
        #[cfg(feature = "audio")]
        {
            self.outputs = daw_app::realtime::output_device_names();
            self.inputs = daw_app::realtime::input_device_names();
        }
        self.audio_open = true;
    }

    /// About window: version and third-party credits (SPEC-039).
    fn about_window(&mut self, ctx: &egui::Context) {
        egui::Window::new("О программе")
            .id(egui::Id::new("about"))
            .open(&mut self.about_open)
            .resizable(false)
            .collapsible(false)
            .default_width(420.0)
            .show(ctx, |ui| {
                ui.heading(format!("Sound Pro {}", env!("CARGO_PKG_VERSION")));
                ui.label("Открытая DAW для Linux и Windows.");
                ui.separator();
                ui.label(daw_app::piano::PIANO_CREDIT);
            });
    }

    /// The audio settings window (SPEC-033): output, input, buffer size.
    fn audio_window(&mut self, ctx: &egui::Context) {
        if !self.audio_open {
            return;
        }
        let mut open = true;
        let before = self.audio.clone();
        egui::Window::new("Настройки звука")
            .id(egui::Id::new("audio-settings"))
            .open(&mut open)
            .resizable(false)
            .default_width(380.0)
            .show(ctx, |ui| self.audio_ui(ui));
        if !open {
            self.audio_open = false;
        }
        if self.audio != before {
            self.apply_audio_settings();
        }
    }

    fn audio_ui(&mut self, ui: &mut egui::Ui) {
        if !cfg!(feature = "audio") {
            ui.label("Звук выключен в этой сборке (соберите с --features audio).");
            ui.add_space(6.0);
        }
        let rate = self.editor.session().sample_rate;
        egui::Grid::new("audio-grid")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                ui.label("Выход");
                device_combo(ui, "audio-out", &mut self.audio.output, &self.outputs);
                ui.end_row();
                ui.label("Вход");
                device_combo(ui, "audio-in", &mut self.audio.input, &self.inputs);
                ui.end_row();
                ui.label("Буфер");
                let shown = self
                    .audio
                    .buffer_frames
                    .map_or_else(|| "Системный".to_owned(), |n| buffer_label(n, rate));
                egui::ComboBox::from_id_salt("audio-buffer")
                    .selected_text(shown)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.audio.buffer_frames, None, "Системный");
                        for n in BUFFER_SIZES {
                            ui.selectable_value(
                                &mut self.audio.buffer_frames,
                                Some(n),
                                buffer_label(n, rate),
                            );
                        }
                    });
                ui.end_row();
                ui.label("Частота");
                ui.label(format!("{rate} Гц (частота проекта)"));
                ui.end_row();
            });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.button("Обновить список").clicked() {
                self.open_audio_settings();
            }
            if ui.button("По умолчанию").clicked() {
                self.audio = AudioSettings::default();
            }
        });
        ui.small("Меньший буфер — меньше задержка, но выше нагрузка.");
    }

    /// Saves the settings and restarts playback on the new device.
    fn apply_audio_settings(&mut self) {
        if let Some(path) = &self.audio_path
            && let Err(e) = self.audio.save(path)
        {
            self.status = format!("Не удалось сохранить настройки звука: {e}");
            return;
        }
        self.status = "Настройки звука сохранены".into();
        if self.player.is_some() {
            self.pause();
            self.play();
        }
    }

    /// The buses window (SPEC-028): bus list and the selected track's sends.
    fn buses_window(&mut self, ctx: &egui::Context) {
        if !self.buses_open {
            return;
        }
        let mut open = true;
        egui::Window::new("Шины")
            .id(egui::Id::new("buses"))
            .open(&mut open)
            .resizable(true)
            .default_width(320.0)
            .show(ctx, |ui| self.buses_ui(ui));
        if !open {
            self.buses_open = false;
        }
    }

    fn buses_ui(&mut self, ui: &mut egui::Ui) {
        let buses = self.editor.session().buses.clone();
        ui.horizontal(|ui| {
            if ui
                .add_enabled(buses.len() < MAX_BUSES, egui::Button::new("+ Шина"))
                .clicked()
            {
                let name = format!("Шина {}", buses.len() + 1);
                if let Ok(index) = self.editor.add_bus(&name) {
                    // A new bus usually exists to hold a shared reverb.
                    let _ = self.editor.add_bus_effect(index, EffectKind::reverb());
                    self.status = format!("{name} добавлена, на ней реверб");
                }
            }
            ui.label(format!("до {MAX_BUSES}"));
        });
        if buses.is_empty() {
            ui.label("Шин нет. Шина — общий эффект, например реверб, для нескольких дорожек.");
            return;
        }
        let mut remove = None;
        for (index, bus) in buses.iter().enumerate() {
            ui.separator();
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(&bus.name).strong());
                let mut gain = bus.gain;
                if ui
                    .add(egui::Slider::new(&mut gain, 0.0..=1.0).text("громкость"))
                    .changed()
                {
                    let _ = self.editor.set_bus_gain(index, gain);
                }
                let label = if bus.effects.is_empty() {
                    "FX".to_owned()
                } else {
                    format!("FX {}", bus.effects.len())
                };
                let target = Some(FxTarget::Bus(index));
                if ui
                    .selectable_label(self.fx == target, label)
                    .on_hover_text("Эффекты шины")
                    .clicked()
                {
                    self.fx = if self.fx == target { None } else { target };
                }
                if ui.small_button("✕").on_hover_text("Удалить шину").clicked() {
                    remove = Some(index);
                }
            });
        }
        if let Some(index) = remove {
            if matches!(self.fx, Some(FxTarget::Bus(_))) {
                self.fx = None;
            }
            if let Err(error) = self.editor.remove_bus(index) {
                self.status = error.to_string();
            }
            return;
        }
        ui.separator();
        let Some(track) = self
            .editor
            .selected_track
            .filter(|&t| t < self.editor.session().tracks.len())
        else {
            ui.label("Выберите дорожку слева, чтобы настроить её посылы.");
            return;
        };
        let name = self.editor.session().tracks[track].name.clone();
        ui.label(format!("Посылы дорожки «{name}»:"));
        for (index, bus) in buses.iter().enumerate() {
            let mut level = self.editor.session().tracks[track].send_level(index);
            if ui
                .add(egui::Slider::new(&mut level, 0.0..=1.0).text(&bus.name))
                .changed()
            {
                let _ = self.editor.set_send(track, index, level);
            }
        }
    }
}

/// What the effects window edits.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FxTarget {
    Track(usize),
    Bus(usize),
}

/// One change requested in the effects window, applied after drawing it.
enum FxAction {
    Remove(usize),
    Move(usize, usize),
    Set(usize, Effect),
}

impl App {
    /// With `SOUND_PRO_SCREENSHOT=path.ppm` set, saves the window as a PPM
    /// image a few frames after start and then closes it.
    fn screenshot(&mut self, ctx: &egui::Context) {
        let Some(path) = std::env::var_os("SOUND_PRO_SCREENSHOT") else {
            return;
        };
        self.frames = self.frames.saturating_add(1);
        ctx.request_repaint();
        if self.frames == 20 {
            ctx.send_viewport_cmd(ViewportCommand::Screenshot(egui::UserData::default()));
        }
        let image = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = image {
            let [w, h] = image.size;
            let mut out = format!("P6 {w} {h} 255\n").into_bytes();
            for p in &image.pixels {
                out.extend_from_slice(&[p.r(), p.g(), p.b()]);
            }
            if let Err(e) = std::fs::write(&path, out) {
                eprintln!("screenshot: {e}");
            }
            ctx.send_viewport_cmd(ViewportCommand::Close);
        }
    }
}

/// Where the current press began. egui clears `press_origin` on the release
/// frame, which is exactly when `clicked()` fires, so a click falls back to
/// the release position (a click barely moves, SPEC-043 §1).
fn press_pos(input: &egui::InputState) -> Option<Pos2> {
    let pointer = &input.pointer;
    pointer.press_origin().or_else(|| {
        pointer
            .any_released()
            .then(|| pointer.interact_pos())
            .flatten()
    })
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        #[cfg(target_os = "linux")]
        if self.x11_cursor.is_none() {
            self.x11_cursor = x11_cursor::X11Cursor::from_frame(_frame);
        }
        if !self.themed {
            shell::apply_theme(ctx);
            self.themed = true;
        }
        self.screenshot(ctx);
        self.poll_player(ctx);
        self.poll_meters(ctx);
        self.poll_recorder(ctx);
        self.poll_imports(ctx);
        self.poll_export();
        // Everything done during one mouse press is one undo step.
        if ctx.input(|i| i.pointer.any_down()) {
            self.editor.begin_group();
        } else {
            self.editor.end_group();
        }
        self.shortcuts(ctx);

        if ctx.input(|i| i.viewport().close_requested()) && self.editor.is_dirty() {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            self.confirm_quit = true;
        }

        egui::TopBottomPanel::top("menu").show(ctx, |ui| self.menu(ui));
        egui::TopBottomPanel::top("transport")
            .frame(egui::Frame::side_top_panel(&ctx.style()).inner_margin(6.0))
            .show(ctx, |ui| self.control_bar(ui));

        egui::TopBottomPanel::bottom("status")
            .exact_height(30.0)
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    let mut master = self.editor.session().master_gain;
                    if ui
                        .add(egui::Slider::new(&mut master, 0.0..=1.0).text("Мастер"))
                        .changed()
                    {
                        self.editor.set_master_gain(master);
                    }
                    ui.separator();
                    ui.label(&self.status);
                });
            });

        self.bottom_area(ctx);

        if self.layout.library {
            egui::SidePanel::left("library")
                .resizable(true)
                .default_width(180.0)
                .show(ctx, |ui| self.library(ui));
        }
        if self.layout.inspector {
            egui::SidePanel::left("inspector")
                .resizable(true)
                .default_width(shell::INSPECTOR_WIDTH)
                .min_width(shell::INSPECTOR_WIDTH)
                .show(ctx, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("inspector")
                        .show(ui, |ui| self.inspector(ui));
                });
        }
        if self.layout.browser {
            egui::SidePanel::right("browser")
                .resizable(true)
                .default_width(220.0)
                .show(ctx, |ui| self.browser(ui));
        }
        egui::TopBottomPanel::top("tracks_toolbar").show(ctx, |ui| self.tracks_toolbar(ui));

        egui::SidePanel::left("tracks")
            .resizable(true)
            .default_width(230.0)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("headers")
                    .show(ui, |ui| self.track_headers(ui));
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            let mut area = egui::ScrollArea::both().id_salt("timeline");
            if let Some(offset) = self.zoom_scroll() {
                area = area.horizontal_scroll_offset(offset);
            }
            area.show(ui, |ui| self.timeline(ui));
        });

        self.effects_window(ctx);
        self.buses_window(ctx);
        self.audio_window(ctx);
        self.about_window(ctx);
        self.new_track_window(ctx);
        self.export_window(ctx);

        if self.confirm_quit {
            egui::Window::new("Несохранённые изменения")
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.label("Выйти без сохранения?");
                    ui.horizontal(|ui| {
                        if ui.button("Сохранить").clicked() {
                            self.save();
                            self.confirm_quit = false;
                        }
                        if ui.button("Выйти").clicked() {
                            self.reset_playback();
                            self.editor = Editor::new();
                            ctx.send_viewport_cmd(ViewportCommand::Close);
                        }
                        if ui.button("Отмена").clicked() {
                            self.confirm_quit = false;
                        }
                    });
                });
        }

        self.refresh_player(ctx);
        if let Some(player) = self.player.as_mut() {
            player.control.collect();
        }

        let title = self.editor.title();
        if title != self.title {
            ctx.send_viewport_cmd(ViewportCommand::Title(title.clone()));
            self.title = title;
        }
    }
}

/// Radius of an automation point in pixels.
const POINT_RADIUS: f32 = 4.0;

/// `true` when `lane` can be automated on a track with these effects.
fn lane_fits(lane: Lane, kinds: &[EffectKind]) -> bool {
    match lane {
        Lane::Gain | Lane::Pan => true,
        Lane::Effect { slot, param } => kinds
            .get(usize::from(slot))
            .is_some_and(|kind| param.get(kind).is_some()),
    }
}

/// Menu text of an automation lane.
fn lane_label(lane: Lane, kinds: &[EffectKind]) -> String {
    match lane {
        Lane::Gain => "Громк.".to_owned(),
        Lane::Pan => "Пан.".to_owned(),
        Lane::Effect { slot, param } => {
            let title = kinds.get(usize::from(slot)).map_or("?", effect_title);
            format!("{} {title} · {}", slot + 1, param_label(param))
        }
    }
}

/// Russian name of an effect parameter.
const fn param_label(param: EffectParam) -> &'static str {
    match param {
        EffectParam::LowDb => "Низ, дБ",
        EffectParam::MidDb => "Середина, дБ",
        EffectParam::MidHz => "Середина, Гц",
        EffectParam::HighDb => "Верх, дБ",
        EffectParam::ThresholdDb => "Порог",
        EffectParam::Ratio => "Степень",
        EffectParam::AttackMs => "Атака",
        EffectParam::ReleaseMs => "Восстановление",
        EffectParam::MakeupDb => "Усиление",
        EffectParam::Room => "Комната",
        EffectParam::Damp => "Глушение",
        EffectParam::Mix => "Микс",
        EffectParam::HpHz => "ФВЧ, Гц",
        EffectParam::LowHz => "Низ. полка, Гц",
        EffectParam::LowShelfDb => "Низ. полка, дБ",
        EffectParam::P1Hz => "Пик 1, Гц",
        EffectParam::P1Db => "Пик 1, дБ",
        EffectParam::P1Q => "Пик 1, Q",
        EffectParam::P2Hz => "Пик 2, Гц",
        EffectParam::P2Db => "Пик 2, дБ",
        EffectParam::P2Q => "Пик 2, Q",
        EffectParam::P3Hz => "Пик 3, Гц",
        EffectParam::P3Db => "Пик 3, дБ",
        EffectParam::P3Q => "Пик 3, Q",
        EffectParam::P4Hz => "Пик 4, Гц",
        EffectParam::P4Db => "Пик 4, дБ",
        EffectParam::P4Q => "Пик 4, Q",
        EffectParam::HighHz => "Верх. полка, Гц",
        EffectParam::HighShelfDb => "Верх. полка, дБ",
        EffectParam::LpHz => "ФНЧ, Гц",
        EffectParam::KneeDb => "Колено",
        EffectParam::GateThresholdDb => "Порог гейта",
        EffectParam::GateRangeDb => "Глубина гейта",
        EffectParam::GateAttackMs => "Атака гейта",
        EffectParam::GateHoldMs => "Удержание гейта",
        EffectParam::GateReleaseMs => "Восстановление гейта",
        EffectParam::CeilingDb => "Потолок",
        EffectParam::LimiterReleaseMs => "Восстановление лимитера",
    }
}

/// Maps a lane value to a y inside the track row: top is the maximum.
fn value_to_y(lane: Lane, value: f32, row: Rect) -> f32 {
    let (lo, hi) = lane.range();
    let norm = ((value - lo) / (hi - lo)).clamp(0.0, 1.0);
    let (top, bottom) = (row.min.y + POINT_RADIUS, row.max.y - POINT_RADIUS);
    bottom - norm * (bottom - top)
}

fn y_to_value(lane: Lane, y: f32, row: Rect) -> f32 {
    let (lo, hi) = lane.range();
    let (top, bottom) = (row.min.y + POINT_RADIUS, row.max.y - POINT_RADIUS);
    let norm = ((bottom - y) / (bottom - top)).clamp(0.0, 1.0);
    lo + norm * (hi - lo)
}

/// One growing take box with its live waveform (SPEC-035).
#[cfg(feature = "audio")]
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "pixel columns"
)]
fn draw_take_rect(painter: &egui::Painter, rect: Rect, samples: &[f32]) {
    painter.rect_filled(rect, 4.0, Color32::from_rgb(170, 40, 40));
    let columns = rect.width().ceil() as usize;
    if columns == 0 || samples.is_empty() {
        return;
    }
    let per_px = samples.len() as f32 / rect.width();
    let (mid, half) = (rect.center().y, rect.height() * 0.4);
    for px in 0..columns.min(20_000) {
        let from = (px as f32 * per_px) as usize;
        let to = (((px + 1) as f32 * per_px).ceil() as usize).min(samples.len());
        let peak = samples
            .get(from..to.max(from + 1).min(samples.len()))
            .unwrap_or(&[])
            .iter()
            .fold(0.0f32, |m, s| m.max(s.abs()))
            .min(1.0);
        let x = rect.min.x + px as f32;
        painter.line_segment(
            [
                Pos2::new(x, mid - peak * half),
                Pos2::new(x, mid + peak * half),
            ],
            Stroke::new(1.0_f32, Color32::from_rgb(255, 200, 200)),
        );
    }
}
