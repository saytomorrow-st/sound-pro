//! Sound Pro window (SPEC-015 to SPEC-021). All editing goes through
//! [`daw_gui::Editor`].

#![forbid(unsafe_code)]
// No console window behind the GUI in Windows release builds (SPEC-024).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;

use audio_core::{
    AutoPoint, Effect, EffectKind, Lane, MAX_BUSES, MAX_EFFECTS_PER_TRACK, Note, TRACK_COLORS,
};
use daw_app::playback::PlaybackControl;
use daw_gui::{
    Editor, EditorError, MAX_TEMPO, MIN_TEMPO, bars_in, clock, samples_to_x, sixteenth_samples,
    snap_to_beat, x_to_samples,
};
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

/// Track header command chosen in this frame (SPEC-029).
#[derive(Clone, Copy)]
enum TrackAction {
    Rename(usize),
    Color(usize, Option<u8>),
    Move(usize, usize),
    Duplicate(usize),
}

/// Display colour of a track; `None` is the neutral default (SPEC-029).
fn track_color(color: Option<u8>) -> Color32 {
    const PALETTE: [Color32; TRACK_COLORS as usize] = [
        Color32::from_rgb(200, 70, 70),
        Color32::from_rgb(220, 130, 50),
        Color32::from_rgb(200, 180, 50),
        Color32::from_rgb(80, 160, 80),
        Color32::from_rgb(50, 160, 160),
        Color32::from_rgb(60, 110, 200),
        Color32::from_rgb(140, 90, 200),
        Color32::from_rgb(200, 90, 160),
    ];
    color
        .and_then(|c| PALETTE.get(usize::from(c)).copied())
        .unwrap_or(Color32::from_gray(90))
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
    /// Track or bus whose effects window is open (SPEC-022, SPEC-028).
    fx: Option<FxTarget>,
    /// The buses window is open (SPEC-028).
    buses_open: bool,
    /// Track whose name is being edited and the text typed so far (SPEC-029).
    rename: Option<(usize, String)>,
    /// Track whose automation lane is shown (SPEC-023).
    auto: Option<usize>,
    auto_lane: Lane,
    /// Index of the automation point being dragged.
    auto_drag: Option<usize>,
    /// Anchor of the cycle region being drawn on the ruler (SPEC-025).
    cycle_drag: Option<u64>,
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
            fx: None,
            buses_open: false,
            rename: None,
            auto: None,
            auto_lane: Lane::Gain,
            auto_drag: None,
            cycle_drag: None,
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
            let cycle = egui::Button::new("🔁").selected(self.editor.active_cycle().is_some());
            if ui
                .add(cycle)
                .on_hover_text("Цикл (C). Протяните по линейке, чтобы задать участок.")
                .clicked()
            {
                self.toggle_cycle();
            }
            let click = egui::Button::new("🎵").selected(self.editor.metronome);
            if ui.add(click).on_hover_text("Метроном (K)").clicked() {
                self.toggle_metronome();
            }
            let buses = egui::Button::new("Шины").selected(self.buses_open);
            if ui
                .add(buses)
                .on_hover_text("Общие эффекты для нескольких дорожек")
                .clicked()
            {
                self.buses_open = !self.buses_open;
            }
            ui.separator();
            let session = self.editor.session();
            let text = format!(
                "{}   такт {}",
                clock(self.playhead, session.sample_rate),
                bars_in(
                    self.playhead,
                    session.sample_rate,
                    session.tempo_bpm,
                    u32::from(session.time_signature.numerator),
                )
            );
            ui.label(egui::RichText::new(text).monospace().size(16.0));
            ui.separator();
            self.meter_fields(ui);
            ui.separator();
            ui.checkbox(&mut self.snap, "Сетка")
                .on_hover_text("Привязка клипов к долям. Shift при перетаскивании отключает.");
        });
    }

    /// Tempo and time signature fields of the transport bar (SPEC-026).
    fn meter_fields(&mut self, ui: &mut egui::Ui) {
        let session = self.editor.session();
        let mut tempo = session.tempo_bpm;
        let signature = session.time_signature;
        let response = ui
            .add(
                egui::DragValue::new(&mut tempo)
                    .range(MIN_TEMPO..=MAX_TEMPO)
                    .speed(0.5)
                    .max_decimals(2)
                    .suffix(" BPM"),
            )
            .on_hover_text("Темп");
        if response.drag_started() {
            self.editor.begin_group();
        }
        if response.changed() {
            self.editor.set_tempo(tempo);
        }
        if response.drag_stopped() {
            self.editor.end_group();
        }
        let mut numerator = signature.numerator;
        let mut denominator = signature.denominator;
        ui.add(
            egui::DragValue::new(&mut numerator)
                .range(1..=16)
                .speed(0.1),
        )
        .on_hover_text("Долей в такте");
        ui.label("/");
        egui::ComboBox::from_id_salt("denominator")
            .width(40.0)
            .selected_text(denominator.to_string())
            .show_ui(ui, |ui| {
                for d in [2u8, 4, 8, 16] {
                    ui.selectable_value(&mut denominator, d, d.to_string());
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
        });
    }

    #[allow(clippy::too_many_lines)]
    fn track_headers(&mut self, ui: &mut egui::Ui) {
        ui.add_space(RULER_HEIGHT);
        let count = self.editor.session().tracks.len();
        let mut delete = None;
        let mut action = None;
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
                        ui.add_space(6.0);
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
                            .add(
                                egui::Label::new(egui::RichText::new(&name).strong())
                                    .sense(Sense::click()),
                            )
                            .on_hover_text("Двойной щелчок — переименовать")
                            .double_clicked()
                        {
                            self.rename = Some((index, name.clone()));
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
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
                                .selectable_label(self.fx == Some(FxTarget::Track(index)), label)
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
                                let other = match self.auto_lane {
                                    Lane::Gain => Lane::Pan,
                                    Lane::Pan => Lane::Gain,
                                };
                                if ui
                                    .button(lane_label(self.auto_lane))
                                    .on_hover_text("Какую линию показывать")
                                    .clicked()
                                {
                                    self.auto_lane = other;
                                    self.auto_drag = None;
                                }
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
                    let automation = &self.editor.session().tracks[index].automation;
                    let (gain_free, pan_free) =
                        (automation.gain.is_empty(), automation.pan.is_empty());
                    let g = ui.add_enabled(
                        gain_free,
                        egui::Slider::new(&mut gain, 0.0..=1.0).text("громк."),
                    );
                    let p = ui.add_enabled(
                        pan_free,
                        egui::Slider::new(&mut pan, -1.0..=1.0).text("пан."),
                    );
                    if g.changed() || p.changed() {
                        if let Some(t) = self.editor.track_mut(index) {
                            t.gain = gain;
                            t.pan = pan;
                        }
                    }
                })
                .response;
            ui.painter().rect_filled(
                Rect::from_min_size(response.rect.min, Vec2::new(4.0, response.rect.height())),
                0.0,
                track_color(self.editor.session().tracks[index].color),
            );
            let click = response.interact(Sense::click());
            if click.clicked() {
                self.editor.selected_track = Some(index);
            }
            click.context_menu(|ui| {
                if ui.button("Переименовать").clicked() {
                    self.rename = Some((index, self.editor.session().tracks[index].name.clone()));
                    ui.close();
                }
                ui.menu_button("Цвет", |ui| {
                    for color in 0..TRACK_COLORS {
                        let swatch = egui::RichText::new("■■■■").color(track_color(Some(color)));
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
    }

    fn track_action(&mut self, action: TrackAction) {
        let result = match action {
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
                } else if track.color.is_some() {
                    track_color(track.color)
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
        let press_on_ruler = ui
            .input(|i| i.pointer.press_origin())
            .is_some_and(|p| ruler.contains(p));
        let ruler_owned = self.drag.is_none()
            && self.auto_drag.is_none()
            && (self.cycle_drag.is_some() || (response.drag_started() && press_on_ruler));
        let lane_owned = !ruler_owned
            && self.drag.is_none()
            && (self.auto_drag.is_some()
                || (pressed
                    && auto_row.is_some_and(|row| {
                        ui.input(|i| i.pointer.press_origin())
                            .is_some_and(|p| row.contains(p))
                    })));

        let cursor = area.min.x + samples_to_x(self.playhead, rate_u32, self.zoom);
        painter.line_segment(
            [Pos2::new(cursor, area.min.y), Pos2::new(cursor, area.max.y)],
            Stroke::new(1.5_f32, Color32::from_rgb(255, 90, 70)),
        );

        if ruler_owned {
            self.draw_cycle(ui, &response, area);
        } else if response.clicked() && pointer.is_some_and(|p| ruler.contains(p)) {
            if let Some(p) = pointer {
                self.seek(x_to_samples(p.x - area.min.x, rate_u32, self.zoom));
            }
        } else if lane_owned {
            if let (Some(t), Some(row)) = (self.auto, auto_row) {
                self.edit_lane(ui, &response, area, row, t);
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

        if !lane_owned && !ruler_owned {
            self.drag_clips(ui, &response, area, &rects);
        }

        if response.hovered() || ui.rect_contains_pointer(ui.clip_rect()) {
            let (ctrl, scroll) = ui.input(|i| (i.modifiers.command, i.raw_scroll_delta.y));
            if ctrl && scroll != 0.0 {
                self.zoom = (self.zoom * (1.0 + scroll * 0.002)).clamp(MIN_ZOOM, MAX_ZOOM);
            }
        }
    }

    /// Drawing a cycle region by dragging on the ruler (SPEC-025).
    fn draw_cycle(&mut self, ui: &egui::Ui, response: &egui::Response, area: Rect) {
        let session = self.editor.session();
        let (rate, tempo) = (session.sample_rate, session.tempo_bpm);
        let per_bar = u32::from(session.time_signature.numerator);
        let free = ui.input(|i| i.modifiers.shift) || !self.snap;
        let at = |x: f32| {
            let samples = x_to_samples(x - area.min.x, rate, self.zoom);
            if free {
                samples
            } else {
                snap_to_beat(samples, rate, tempo)
            }
        };
        if self.cycle_drag.is_none() {
            let Some(origin) = ui.input(|i| i.pointer.press_origin()) else {
                return;
            };
            self.cycle_drag = Some(at(origin.x));
        }
        if let (Some(anchor), Some(p)) = (self.cycle_drag, ui.input(|i| i.pointer.latest_pos()))
            && self.editor.set_cycle(anchor, at(p.x)).is_ok()
        {
            self.sync_cycle();
        }
        if response.drag_stopped() || !ui.input(|i| i.pointer.primary_down()) {
            self.cycle_drag = None;
            if let Some((from, to)) = self.editor.cycle() {
                self.status = format!(
                    "Цикл: такт {} – {}",
                    bars_in(from, rate, tempo, per_bar),
                    bars_in(to, rate, tempo, per_bar)
                );
            }
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
        if !ctx.wants_keyboard_input() {
            let (space, home, record, cycle, click) = ctx.input_mut(|i| {
                (
                    i.consume_key(egui::Modifiers::NONE, Key::Space),
                    i.consume_key(egui::Modifiers::NONE, Key::Home),
                    i.consume_key(egui::Modifiers::NONE, Key::R),
                    i.consume_key(egui::Modifiers::NONE, Key::C),
                    i.consume_key(egui::Modifiers::NONE, Key::K),
                )
            });
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

/// Russian label of an effect type for the effects window.
const fn effect_title(kind: &EffectKind) -> &'static str {
    match kind {
        EffectKind::Eq { .. } => "Эквалайзер",
        EffectKind::Compressor { .. } => "Компрессор",
        EffectKind::Reverb { .. } => "Реверб",
    }
}

/// Sliders for every parameter of `kind`; `true` if one moved.
fn effect_sliders(ui: &mut egui::Ui, kind: &mut EffectKind) -> bool {
    let mut changed = false;
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
        } => {
            slider(ui, threshold_db, -60.0..=0.0, "порог, дБ", false);
            slider(ui, ratio, 1.0..=20.0, "степень", true);
            slider(ui, attack_ms, 0.1..=100.0, "атака, мс", true);
            slider(ui, release_ms, 10.0..=1000.0, "спад, мс", true);
            slider(ui, makeup_db, 0.0..=24.0, "усиление, дБ", false);
        }
        EffectKind::Reverb { room, damp, mix } => {
            slider(ui, room, 0.0..=1.0, "комната", false);
            slider(ui, damp, 0.0..=1.0, "глушение", false);
            slider(ui, mix, 0.0..=1.0, "доля", false);
        }
    }
    changed
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

        self.effects_window(ctx);
        self.buses_window(ctx);

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

/// Radius of an automation point in pixels.
const POINT_RADIUS: f32 = 4.0;

fn lane_label(lane: Lane) -> &'static str {
    match lane {
        Lane::Gain => "Громк.",
        Lane::Pan => "Пан.",
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
