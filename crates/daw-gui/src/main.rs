//! Sound Pro window (SPEC-015 to SPEC-021). All editing goes through
//! [`daw_gui::Editor`].

#![forbid(unsafe_code)]

use std::path::PathBuf;

use audio_core::Note;
use daw_app::playback::PlaybackControl;
use daw_gui::{Editor, bars, clock, samples_to_x, sixteenth_samples, snap_to_beat, x_to_samples};
use eframe::egui::{
    self, Align2, Color32, CursorIcon, FontId, Key, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2,
    ViewportCommand,
};

const TRACK_HEIGHT: f32 = 72.0;
const RULER_HEIGHT: f32 = 24.0;
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum DragMode {
    Move,
    TrimStart,
    TrimEnd,
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

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Sound Pro")
            .with_inner_size([1200.0, 700.0])
            .with_min_inner_size([640.0, 360.0]),
        ..Default::default()
    };
    let start = std::env::args_os().nth(1).map(PathBuf::from);
    eframe::run_native(
        "Sound Pro",
        options,
        Box::new(move |_cc| Ok(Box::new(App::new(start)))),
    )
}

/// A running playback: the control side and, with sound, the open stream.
struct Player {
    control: PlaybackControl,
    #[cfg(feature = "audio")]
    _stream: daw_app::realtime::RealtimeHandle,
}

/// A microphone take in progress (SPEC-020).
#[cfg(feature = "audio")]
struct Recorder {
    control: daw_app::capture::CaptureControl,
    _stream: daw_app::realtime::RealtimeHandle,
    /// Timeline position where the take starts, in project samples.
    start: u64,
}

struct App {
    editor: Editor,
    #[cfg(feature = "audio")]
    recorder: Option<Recorder>,
    /// Playhead in samples while stopped; the player's position while playing.
    playhead: u64,
    player: Option<Player>,
    status: String,
    /// Timeline zoom in pixels per second.
    zoom: f32,
    title: String,
    confirm_quit: bool,
    /// Snap clip edits to beats (Shift turns it off while dragging).
    snap: bool,
    drag: Option<Drag>,
    /// MIDI clip open in the piano roll.
    piano: Option<(usize, usize)>,
    note_drag: Option<NoteDrag>,
    /// Scroll the piano roll to middle C on the next frame.
    piano_scroll: bool,
}

impl App {
    fn new(start: Option<PathBuf>) -> Self {
        let mut app = Self {
            editor: Editor::new(),
            #[cfg(feature = "audio")]
            recorder: None,
            playhead: 0,
            player: None,
            status: "Готово. Файл → Импорт WAV…, чтобы добавить звук.".into(),
            zoom: 100.0,
            title: String::new(),
            confirm_quit: false,
            snap: true,
            drag: None,
            piano: None,
            note_drag: None,
            piano_scroll: false,
        };
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
            match daw_app::realtime::start_output(engine, rate) {
                Ok(stream) => {
                    self.player = Some(Player {
                        control,
                        _stream: stream,
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
        let rate = self.editor.session().sample_rate;
        match daw_app::realtime::start_input(rate, 30) {
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
                self.status = "Конец".into();
            }
        }
    }

    fn reset_playback(&mut self) {
        self.player = None;
        self.playhead = 0;
    }

    fn transport(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("⏮").on_hover_text("В начало (Home)").clicked() {
                self.rewind();
            }
            let label = if self.is_playing() { "⏸" } else { "▶" };
            if ui
                .button(label)
                .on_hover_text("Играть / пауза (пробел)")
                .clicked()
            {
                self.toggle_play();
            }
            if ui.button("⏹").on_hover_text("Стоп").clicked() {
                self.stop();
            }
            let rec = egui::RichText::new("⏺").color(if self.is_recording() {
                Color32::RED
            } else {
                Color32::from_rgb(200, 80, 80)
            });
            if ui
                .add(egui::Button::new(rec).selected(self.is_recording()))
                .on_hover_text("Запись с микрофона (R)")
                .clicked()
            {
                self.toggle_record();
            }
            ui.separator();
            let session = self.editor.session();
            let text = format!(
                "{}   такт {}",
                clock(self.playhead, session.sample_rate),
                bars(self.playhead, session.sample_rate, session.tempo_bpm)
            );
            ui.label(egui::RichText::new(text).monospace().size(16.0));
            ui.separator();
            ui.checkbox(&mut self.snap, "Сетка")
                .on_hover_text("Привязка клипов к долям. Shift при перетаскивании отключает.");
        });
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
            .add_filter("WAV", &["wav", "WAV"])
            .pick_files()
        else {
            return;
        };
        for path in paths {
            let result = self.editor.import_wav(&path);
            if self
                .report(result, &format!("Импортирован {}", path.display()))
                .is_none()
            {
                break;
            }
        }
    }

    fn export_dialog(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("WAV", &["wav"])
            .set_file_name("mixdown.wav")
            .save_file()
        else {
            return;
        };
        let result = self.editor.export(&path);
        if let Some(report) = self.report(result, "") {
            #[allow(clippy::cast_precision_loss)]
            let seconds = report.frames as f64 / f64::from(self.editor.session().sample_rate);
            self.status = format!("Сведение записано: {} ({seconds:.2} с)", path.display());
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
                if ui.button("Выход").clicked() {
                    ui.ctx().send_viewport_cmd(ViewportCommand::Close);
                }
            });
            ui.menu_button("Дорожка", |ui| {
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
            });
        });
    }

    fn track_headers(&mut self, ui: &mut egui::Ui) {
        ui.add_space(RULER_HEIGHT);
        let count = self.editor.session().tracks.len();
        let mut delete = None;
        for index in 0..count {
            let selected = self.editor.selected_track == Some(index);
            let frame = egui::Frame::new()
                .fill(if selected {
                    Color32::from_rgb(48, 56, 72)
                } else {
                    Color32::from_gray(36)
                })
                .inner_margin(4.0);
            let response = frame
                .show(ui, |ui| {
                    ui.set_height(TRACK_HEIGHT - 8.0);
                    ui.set_width(ui.available_width());
                    let track = &self.editor.session().tracks[index];
                    let (mut mute, mut solo) = (track.mute, track.solo);
                    let (mut gain, mut pan) = (track.gain, track.pan);
                    let name = track.name.clone();
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(name).strong());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .small_button("✕")
                                .on_hover_text("Удалить дорожку")
                                .clicked()
                            {
                                delete = Some(index);
                            }
                            let s = ui.toggle_value(&mut solo, "S").on_hover_text("Соло");
                            let m = ui.toggle_value(&mut mute, "M").on_hover_text("Без звука");
                            if s.changed() || m.changed() {
                                if let Some(t) = self.editor.track_mut(index) {
                                    t.mute = mute;
                                    t.solo = solo;
                                }
                            }
                        });
                    });
                    let g = ui.add(egui::Slider::new(&mut gain, 0.0..=1.0).text("громк."));
                    let p = ui.add(egui::Slider::new(&mut pan, -1.0..=1.0).text("пан."));
                    if g.changed() || p.changed() {
                        if let Some(t) = self.editor.track_mut(index) {
                            t.gain = gain;
                            t.pan = pan;
                        }
                    }
                })
                .response;
            if response.interact(Sense::click()).clicked() {
                self.editor.selected_track = Some(index);
            }
        }
        if let Some(index) = delete {
            self.editor.delete_track(index);
            self.status = "Дорожка удалена".into();
        }
        if count == 0 {
            ui.label("Нет дорожек");
        }
    }

    #[allow(clippy::cast_precision_loss, clippy::too_many_lines)]
    fn timeline(&mut self, ui: &mut egui::Ui) {
        let rate = f64::from(self.editor.session().sample_rate);
        let session_seconds = self.editor.session().length_samples() as f64 / rate;
        let width = ((session_seconds as f32 + 10.0) * self.zoom).max(ui.available_width());
        let tracks = self.editor.session().tracks.len();
        let height = (RULER_HEIGHT + TRACK_HEIGHT * tracks as f32).max(ui.available_height() - 4.0);

        let (area, response) =
            ui.allocate_exact_size(Vec2::new(width, height), Sense::click_and_drag());
        let painter = ui.painter_at(area);
        painter.rect_filled(area, 0.0, Color32::from_gray(24));

        // Ruler: one line per beat, labels on bar starts (4/4).
        let beat_seconds = 60.0 / self.editor.session().tempo_bpm;
        let beat_px = beat_seconds as f32 * self.zoom;
        let bar_step = if beat_px * 4.0 < 40.0 {
            ((40.0 / (beat_px * 4.0)).ceil() as usize).max(1)
        } else {
            1
        };
        let ruler = Rect::from_min_size(area.min, Vec2::new(width, RULER_HEIGHT));
        painter.rect_filled(ruler, 0.0, Color32::from_gray(40));
        let beats = (width / beat_px.max(0.01)).ceil() as usize;
        for beat in 0..=beats.min(100_000) {
            let x = area.min.x + beat as f32 * beat_px;
            let bar = beat / 4;
            if beat % 4 == 0 && bar % bar_step == 0 {
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
            for (c, clip) in track.clips.iter().enumerate() {
                let left = area.min.x + clip.start_samples as f32 * px_per_sample;
                let w = (clip.length_samples as f32 * px_per_sample).max(2.0);
                let rect = Rect::from_min_size(
                    Pos2::new(left, top + 3.0),
                    Vec2::new(w, TRACK_HEIGHT - 6.0),
                );
                let selected = self.editor.selected_clip == Some((t, c));
                let fill = if track.mute {
                    Color32::from_rgb(70, 70, 70)
                } else if clip.is_midi() {
                    Color32::from_rgb(60, 120, 70)
                } else {
                    Color32::from_rgb(44, 96, 140)
                };
                painter.rect_filled(rect, 4.0, fill);
                if let Some(notes) = &clip.notes {
                    let end = clip.offset_samples + clip.length_samples;
                    for n in notes {
                        if n.start + n.length <= clip.offset_samples || n.start >= end {
                            continue;
                        }
                        let from = n.start.max(clip.offset_samples) - clip.offset_samples;
                        let to = (n.start + n.length).min(end) - clip.offset_samples;
                        let y =
                            rect.max.y - 4.0 - f32::from(n.pitch) / 127.0 * (rect.height() - 8.0);
                        painter.line_segment(
                            [
                                Pos2::new(rect.min.x + from as f32 * px_per_sample, y),
                                Pos2::new(
                                    (rect.min.x + to as f32 * px_per_sample)
                                        .max(rect.min.x + from as f32 * px_per_sample + 2.0),
                                    y,
                                ),
                            ],
                            Stroke::new(2.0_f32, Color32::from_rgb(200, 240, 200)),
                        );
                    }
                }
                if let Some(peaks) = self.editor.peaks(clip) {
                    let mid = rect.center().y;
                    let half = rect.height() * 0.4;
                    let columns = rect.width().ceil() as usize;
                    let per_px = clip.length_samples as f32 / rect.width();
                    for px in 0..columns {
                        let from = clip.offset_samples + (px as f32 * per_px) as u64;
                        let to = clip.offset_samples + ((px + 1) as f32 * per_px).ceil() as u64;
                        if let Some((lo, hi)) = peaks.range(from, to) {
                            let x = rect.min.x + px as f32 + 0.5;
                            painter.line_segment(
                                [
                                    Pos2::new(x, mid - hi.clamp(-1.0, 1.0) * half),
                                    Pos2::new(x, mid - lo.clamp(-1.0, 1.0) * half + 1.0),
                                ],
                                Stroke::new(1.0_f32, Color32::from_rgb(170, 210, 240)),
                            );
                        }
                    }
                }
                painter.text(
                    rect.min + Vec2::new(4.0, 2.0),
                    Align2::LEFT_TOP,
                    &clip.name,
                    FontId::proportional(12.0),
                    Color32::WHITE,
                );
                let border = if selected {
                    Stroke::new(2.0_f32, Color32::from_rgb(255, 200, 60))
                } else {
                    Stroke::new(1.0_f32, Color32::from_gray(10))
                };
                painter.rect_stroke(rect, 4.0, border, StrokeKind::Inside);
                rects.push(((t, c), rect));
                if response.clicked() && pointer.is_some_and(|p| rect.contains(p)) {
                    clicked = Some((t, c));
                }
                if response.double_clicked()
                    && clip.is_midi()
                    && pointer.is_some_and(|p| rect.contains(p))
                {
                    open_piano = Some((t, c));
                }
            }
        }

        let rate_u32 = self.editor.session().sample_rate;
        let cursor = area.min.x + samples_to_x(self.playhead, rate_u32, self.zoom);
        painter.line_segment(
            [Pos2::new(cursor, area.min.y), Pos2::new(cursor, area.max.y)],
            Stroke::new(1.5_f32, Color32::from_rgb(255, 90, 70)),
        );

        if response.clicked() && pointer.is_some_and(|p| ruler.contains(p)) {
            if let Some(p) = pointer {
                self.seek(x_to_samples(p.x - area.min.x, rate_u32, self.zoom));
            }
        } else if response.clicked() {
            self.editor.selected_clip = clicked;
            if let Some((t, _)) = clicked {
                self.editor.selected_track = Some(t);
            } else if let Some(p) = pointer {
                let row = ((p.y - area.min.y - RULER_HEIGHT) / TRACK_HEIGHT).floor();
                self.editor.selected_track =
                    (row >= 0.0 && (row as usize) < tracks).then_some(row as usize);
            }
        }

        if let Some(at) = open_piano {
            self.open_piano(at);
        }

        self.drag_clips(ui, &response, area, &rects);

        if response.hovered() || ui.rect_contains_pointer(ui.clip_rect()) {
            let (ctrl, scroll) = ui.input(|i| (i.modifiers.command, i.raw_scroll_delta.y));
            if ctrl && scroll != 0.0 {
                self.zoom = (self.zoom * (1.0 + scroll * 0.002)).clamp(MIN_ZOOM, MAX_ZOOM);
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
        if self.drag.is_none() {
            if let Some((_, rect)) = response.hover_pos().and_then(hit) {
                if let Some(p) = response.hover_pos() {
                    if edge_mode(rect, p.x) != DragMode::Move {
                        ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
                    }
                }
            }
        }

        let rate = self.editor.session().sample_rate;
        let tempo = self.editor.session().tempo_bpm;
        if response.drag_started() {
            let origin = ui.input(|i| i.pointer.press_origin());
            self.drag = origin.and_then(|p| {
                let ((track, clip), rect) = hit(p)?;
                let start = self.editor.session().tracks[track].clips[clip].start_samples;
                let at = x_to_samples(p.x - area.min.x, rate, self.zoom);
                Some(Drag {
                    track,
                    clip,
                    mode: edge_mode(rect, p.x),
                    grab: at as i64 - start as i64,
                })
            });
            if let Some(d) = self.drag {
                self.editor.selected_clip = Some((d.track, d.clip));
                self.editor.selected_track = Some(d.track);
            }
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
            _ => CursorIcon::ResizeHorizontal,
        });
        let snap = self.snap && !ui.input(|i| i.modifiers.shift);
        let fit = |samples: u64| {
            if snap {
                snap_to_beat(samples, rate, tempo)
            } else {
                samples
            }
        };
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
                self.editor
                    .move_clip(drag.track, drag.clip, start, to)
                    .map(|(t, c)| {
                        drag.track = t;
                        drag.clip = c;
                    })
            }
            DragMode::TrimStart => self.editor.trim_start(drag.track, drag.clip, fit(at)),
            DragMode::TrimEnd => self.editor.trim_end(drag.track, drag.clip, fit(at)),
        };
        if let Err(e) = result {
            self.status = format!("Ошибка: {e}");
        }
        self.drag = Some(drag);
    }

    fn add_midi_track(&mut self) {
        let session = self.editor.session();
        let length = sixteenth_samples(session.sample_rate, session.tempo_bpm) * 64;
        let result = self.editor.add_midi_track(self.playhead, length);
        if let Some(at) = self.report(result, "MIDI-дорожка добавлена") {
            self.open_piano(at);
        }
    }

    fn open_piano(&mut self, at: (usize, usize)) {
        self.piano = Some(at);
        self.note_drag = None;
        self.piano_scroll = true;
        self.editor.selected_clip = Some(at);
        self.editor.selected_track = Some(at.0);
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
        let px_per_sample = STEP_PX / step as f32;
        let width = KEYS_PX + content as f32 * px_per_sample + 2.0;
        let (area, response) = ui.allocate_exact_size(
            Vec2::new(width.max(ui.available_width()), KEY_HEIGHT * 128.0),
            Sense::click_and_drag(),
        );
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
            let x = grid_x + s as f32 * STEP_PX;
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

    fn shortcuts(&mut self, ctx: &egui::Context) {
        if !ctx.wants_keyboard_input() {
            let (space, home, record) = ctx.input_mut(|i| {
                (
                    i.consume_key(egui::Modifiers::NONE, Key::Space),
                    i.consume_key(egui::Modifiers::NONE, Key::Home),
                    i.consume_key(egui::Modifiers::NONE, Key::R),
                )
            });
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
        let (delete, save, open, import) = ctx.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::NONE, Key::Delete),
                i.consume_key(egui::Modifiers::COMMAND, Key::S),
                i.consume_key(egui::Modifiers::COMMAND, Key::O),
                i.consume_key(egui::Modifiers::COMMAND, Key::I),
            )
        });
        if delete && !ctx.wants_keyboard_input() {
            if let Some((t, c)) = self.editor.selected_clip {
                self.editor.delete_clip(t, c);
                self.status = "Клип удалён".into();
            }
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
        if !ctx.wants_keyboard_input() {
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

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_player(ctx);
        self.poll_recorder(ctx);
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
        egui::TopBottomPanel::top("transport").show(ctx, |ui| self.transport(ui));

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

        if self.piano_clip().is_some() {
            egui::TopBottomPanel::bottom("piano")
                .resizable(true)
                .default_height(PIANO_HEIGHT)
                .show(ctx, |ui| self.piano_roll(ui));
        }

        egui::SidePanel::left("tracks")
            .resizable(true)
            .default_width(230.0)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("headers")
                    .show(ui, |ui| self.track_headers(ui));
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::both()
                .id_salt("timeline")
                .show(ui, |ui| self.timeline(ui));
        });

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

        let title = self.editor.title();
        if title != self.title {
            ctx.send_viewport_cmd(ViewportCommand::Title(title.clone()));
            self.title = title;
        }
    }
}
