//! Sound Pro window (SPEC-015, SPEC-016). All editing goes through
//! [`daw_gui::Editor`].

#![forbid(unsafe_code)]

use std::path::PathBuf;

use daw_app::playback::PlaybackControl;
use daw_gui::{Editor, bars, clock, samples_to_x, x_to_samples};
use eframe::egui::{
    self, Align2, Color32, FontId, Key, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2,
    ViewportCommand,
};

const TRACK_HEIGHT: f32 = 72.0;
const RULER_HEIGHT: f32 = 24.0;
const MIN_ZOOM: f32 = 5.0;
const MAX_ZOOM: f32 = 2_000.0;

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

struct App {
    editor: Editor,
    /// Playhead in samples while stopped; the player's position while playing.
    playhead: u64,
    player: Option<Player>,
    status: String,
    /// Timeline zoom in pixels per second.
    zoom: f32,
    title: String,
    confirm_quit: bool,
}

impl App {
    fn new(start: Option<PathBuf>) -> Self {
        let mut app = Self {
            editor: Editor::new(),
            playhead: 0,
            player: None,
            status: "Готово. Файл → Импорт WAV…, чтобы добавить звук.".into(),
            zoom: 100.0,
            title: String::new(),
            confirm_quit: false,
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

    fn toggle_play(&mut self) {
        if self.is_playing() {
            self.pause();
        } else {
            self.play();
        }
    }

    /// First press pauses; pressing again while stopped goes to the start.
    fn stop(&mut self) {
        if self.is_playing() {
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
            self.status = "Конец".into();
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
            ui.separator();
            let session = self.editor.session();
            let text = format!(
                "{}   такт {}",
                clock(self.playhead, session.sample_rate),
                bars(self.playhead, session.sample_rate, session.tempo_bpm)
            );
            ui.label(egui::RichText::new(text).monospace().size(16.0));
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

        let (area, response) = ui.allocate_exact_size(Vec2::new(width, height), Sense::click());
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
                } else {
                    Color32::from_rgb(44, 96, 140)
                };
                painter.rect_filled(rect, 4.0, fill);
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
                if response.clicked() && pointer.is_some_and(|p| rect.contains(p)) {
                    clicked = Some((t, c));
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

        if response.hovered() || ui.rect_contains_pointer(ui.clip_rect()) {
            let (ctrl, scroll) = ui.input(|i| (i.modifiers.command, i.raw_scroll_delta.y));
            if ctrl && scroll != 0.0 {
                self.zoom = (self.zoom * (1.0 + scroll * 0.002)).clamp(MIN_ZOOM, MAX_ZOOM);
            }
        }
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        if !ctx.wants_keyboard_input() {
            let (space, home) = ctx.input_mut(|i| {
                (
                    i.consume_key(egui::Modifiers::NONE, Key::Space),
                    i.consume_key(egui::Modifiers::NONE, Key::Home),
                )
            });
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
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_player(ctx);
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
