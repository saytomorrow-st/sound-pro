//! Main window chrome in the style of a classic DAW (SPEC-034): control bar
//! with position display, library, inspector, browser and the bottom area
//! with mixer, editor and smart controls. Placeholders are marked as such.

use daw_gui::layout::{
    Bottom, db_label, fader_gain, fader_position, lcd_position, pan_angle, pan_label,
};
use eframe::egui::{self, Color32, CornerRadius, RichText, Sense, Stroke, Vec2};

use super::{App, FxTarget, PIANO_HEIGHT, effect_title, track_color};

/// Dark grey theme close to a studio app.
pub fn apply_theme(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = Color32::from_gray(38);
    visuals.window_fill = Color32::from_gray(42);
    visuals.extreme_bg_color = Color32::from_gray(24);
    visuals.faint_bg_color = Color32::from_gray(44);
    visuals.widgets.noninteractive.bg_fill = Color32::from_gray(46);
    visuals.widgets.inactive.bg_fill = Color32::from_gray(58);
    visuals.widgets.inactive.weak_bg_fill = Color32::from_gray(58);
    visuals.widgets.hovered.weak_bg_fill = Color32::from_gray(72);
    visuals.selection.bg_fill = Color32::from_rgb(50, 110, 200);
    visuals.selection.stroke = Stroke::new(1.0_f32, Color32::WHITE);
    ctx.set_visuals(visuals);
}

const LCD_BG: Color32 = Color32::from_rgb(22, 26, 30);
const LCD_TEXT: Color32 = Color32::from_rgb(170, 220, 255);
const STRIP_WIDTH: f32 = 74.0;

fn lcd_cell(ui: &mut egui::Ui, value: &str, caption: &str) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        ui.label(RichText::new(value).monospace().size(18.0).color(LCD_TEXT));
        ui.label(
            RichText::new(caption)
                .size(9.0)
                .color(Color32::from_gray(140)),
        );
    });
}

fn placeholder(ui: &mut egui::Ui, text: &str) {
    ui.add_enabled(false, egui::Button::new(text))
        .on_disabled_hover_text("Скоро");
}

/// Greyed slot of a channel strip for a feature that is not ready yet.
fn stub_slot(ui: &mut egui::Ui, text: &str) {
    ui.add_enabled(
        false,
        egui::Button::new(RichText::new(text).size(10.0))
            .min_size(Vec2::new(ui.available_width(), 16.0))
            .corner_radius(CornerRadius::same(3)),
    )
    .on_disabled_hover_text("Скоро");
}

/// Small caption over a group of strip slots.
fn strip_caption(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).size(9.0).color(Color32::from_gray(130)));
}

/// Rounded toggle like the M/S/R/I buttons of the reference.
pub(crate) fn pill(ui: &mut egui::Ui, on: &mut bool, text: &str, color: Color32) -> egui::Response {
    let size = Vec2::new(20.0, 15.0);
    let (rect, mut response) = ui.allocate_exact_size(size, Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    let hovered = response.hovered();
    let (fill, ink) = if *on {
        (color, Color32::BLACK)
    } else if hovered {
        (Color32::from_gray(80), Color32::from_gray(220))
    } else {
        (Color32::from_gray(62), Color32::from_gray(190))
    };
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(4), fill);
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        egui::FontId::proportional(10.0),
        ink,
    );
    response
}

/// Disabled rounded button with the same look as [`pill`].
pub(crate) fn pill_stub(ui: &mut egui::Ui, text: &str) {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(20.0, 15.0), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(4), Color32::from_gray(48));
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        egui::FontId::proportional(10.0),
        Color32::from_gray(100),
    );
    response.on_hover_text("Скоро");
}

/// Drawn pan knob; dragging up/right turns it clockwise.
fn pan_knob(ui: &mut egui::Ui, pan: &mut f32, enabled: bool) -> egui::Response {
    let size = Vec2::splat(30.0);
    let sense = if enabled {
        Sense::click_and_drag()
    } else {
        Sense::hover()
    };
    let (rect, mut response) = ui.allocate_exact_size(size, sense);
    if response.dragged() {
        let d = response.drag_delta();
        *pan = (*pan + (d.x - d.y) * 0.01).clamp(-1.0, 1.0);
        response.mark_changed();
    }
    if response.double_clicked() {
        *pan = 0.0;
        response.mark_changed();
    }
    let painter = ui.painter();
    let c = rect.center();
    let r = 12.0;
    painter.circle_filled(c, r, Color32::from_gray(30));
    painter.circle_stroke(c, r, Stroke::new(1.0_f32, Color32::from_gray(90)));
    let angle = pan_angle(*pan);
    let start = 0.0_f32;
    let arc = |a: f32| c + Vec2::new(a.sin(), -a.cos()) * (r - 2.0);
    let steps = 12;
    #[allow(clippy::cast_precision_loss)]
    let points: Vec<egui::Pos2> = (0..=steps)
        .map(|i| arc(start + (angle - start) * i as f32 / steps as f32))
        .collect();
    let accent = if enabled {
        Color32::from_rgb(90, 170, 255)
    } else {
        Color32::from_gray(110)
    };
    painter.add(egui::Shape::line(points, Stroke::new(2.5_f32, accent)));
    painter.line_segment([c, arc(angle)], Stroke::new(2.0_f32, Color32::WHITE));
    response.on_hover_text(format!(
        "Панорама {}  (двойной щелчок — центр)",
        pan_label(*pan)
    ))
}

/// Drawn volume fader with a decorative level meter on the left.
fn fader(
    ui: &mut egui::Ui,
    gain: &mut f32,
    enabled: bool,
    height: f32,
    level: f32,
) -> egui::Response {
    let size = Vec2::new(40.0, height);
    let sense = if enabled {
        Sense::click_and_drag()
    } else {
        Sense::hover()
    };
    let (rect, mut response) = ui.allocate_exact_size(size, sense);
    let track = egui::Rect::from_min_max(
        egui::pos2(rect.center().x + 3.0, rect.top() + 6.0),
        egui::pos2(rect.center().x + 7.0, rect.bottom() - 6.0),
    );
    if response.dragged() {
        if let Some(pos) = response.interact_pointer_pos() {
            let p = 1.0 - (pos.y - track.top()) / track.height();
            *gain = fader_gain(p.clamp(0.0, 1.0));
            response.mark_changed();
        }
    }
    if response.double_clicked() {
        *gain = 1.0;
        response.mark_changed();
    }
    let painter = ui.painter();
    let meter = egui::Rect::from_min_max(
        egui::pos2(rect.left() + 4.0, track.top()),
        egui::pos2(rect.left() + 10.0, track.bottom()),
    );
    painter.rect_filled(meter, CornerRadius::same(1), Color32::from_gray(20));
    let lit = meter.height() * level.clamp(0.0, 1.0);
    if lit > 0.0 {
        let fill =
            egui::Rect::from_min_max(egui::pos2(meter.left(), meter.bottom() - lit), meter.max);
        painter.rect_filled(fill, CornerRadius::same(1), Color32::from_rgb(80, 210, 90));
    }
    for db in [0.0_f32, -6.0, -12.0, -24.0, -40.0] {
        let y = track.bottom() - track.height() * fader_position(10f32.powf(db / 20.0));
        painter.line_segment(
            [
                egui::pos2(meter.right() + 2.0, y),
                egui::pos2(track.left() - 2.0, y),
            ],
            Stroke::new(1.0_f32, Color32::from_gray(80)),
        );
    }
    painter.rect_filled(track, CornerRadius::same(2), Color32::from_gray(18));
    let y = track.bottom() - track.height() * fader_position(*gain);
    let cap = egui::Rect::from_center_size(egui::pos2(track.center().x, y), Vec2::new(22.0, 12.0));
    let cap_fill = if enabled {
        Color32::from_gray(200)
    } else {
        Color32::from_gray(110)
    };
    painter.rect_filled(cap, CornerRadius::same(2), cap_fill);
    painter.line_segment(
        [
            egui::pos2(cap.left() + 2.0, y),
            egui::pos2(cap.right() - 2.0, y),
        ],
        Stroke::new(1.0_f32, Color32::from_gray(40)),
    );
    response.on_hover_text(format!(
        "Громкость {}  (двойной щелчок — 0 dB)",
        db_label(*gain)
    ))
}

/// Greyed EQ thumbnail (the channel EQ is not ready yet).
fn eq_thumb(ui: &mut egui::Ui) {
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 24.0), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(3), Color32::from_gray(26));
    let mid = rect.center().y;
    painter.line_segment(
        [
            egui::pos2(rect.left() + 3.0, mid),
            egui::pos2(rect.right() - 3.0, mid),
        ],
        Stroke::new(1.0_f32, Color32::from_gray(80)),
    );
    painter.text(
        rect.right_top() + Vec2::new(-3.0, 2.0),
        egui::Align2::RIGHT_TOP,
        "EQ",
        egui::FontId::proportional(8.0),
        Color32::from_gray(90),
    );
    response.on_hover_text("Эквалайзер: скоро");
}

/// Coloured name plate at the bottom of a strip.
fn name_plate(ui: &mut egui::Ui, name: &str, color: Color32) {
    egui::Frame::new()
        .fill(color)
        .corner_radius(CornerRadius::same(3))
        .inner_margin(2.0)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.vertical_centered(|ui| {
                ui.add(
                    egui::Label::new(
                        RichText::new(name)
                            .size(11.0)
                            .strong()
                            .color(Color32::BLACK),
                    )
                    .truncate(),
                );
            });
        });
}

fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(4.0);
    ui.label(RichText::new(title).strong().color(Color32::from_gray(200)));
    ui.separator();
}

impl App {
    /// Top control bar: view buttons, transport, position display, modes.
    pub(crate) fn control_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let l = &mut self.layout;
            ui.toggle_value(&mut l.library, "📚")
                .on_hover_text("Библиотека (Y)");
            ui.toggle_value(&mut l.inspector, "ⓘ")
                .on_hover_text("Инспектор (I)");
            let mut bottom_button = |ui: &mut egui::Ui, panel, text: &str, hint: &str| {
                if ui
                    .selectable_label(self.layout.bottom == Some(panel), text)
                    .on_hover_text(hint)
                    .clicked()
                {
                    self.layout.toggle_bottom(panel);
                }
            };
            bottom_button(ui, Bottom::SmartControls, "🎛", "Быстрые регуляторы (B)");
            bottom_button(ui, Bottom::Mixer, "🎚", "Микшер (X)");
            bottom_button(ui, Bottom::Editor, "✏", "Редактор нот (E)");
            let full = ui.max_rect();
            let center_width = 640.0_f32.min(full.width());
            let center = egui::Rect::from_center_size(
                egui::pos2(full.center().x, full.center().y),
                Vec2::new(center_width, full.height()),
            );
            ui.scope_builder(
                egui::UiBuilder::new()
                    .max_rect(center)
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
                |ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    self.transport_buttons(ui);
                    ui.add_space(6.0);
                    self.lcd(ui);
                    ui.add_space(6.0);
                    self.mode_buttons(ui);
                },
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.toggle_value(&mut self.layout.browser, "🗁")
                    .on_hover_text("Браузер файлов (F)");
                placeholder(ui, "📝");
                placeholder(ui, "♫");
                placeholder(ui, "☰");
            });
        });
    }

    fn transport_buttons(&mut self, ui: &mut egui::Ui) {
        let big = |t: &str| RichText::new(t).size(15.0);
        if ui
            .add(egui::Button::new(big("⏮")).min_size(Vec2::new(28.0, 24.0)))
            .on_hover_text("В начало (Home)")
            .clicked()
        {
            self.rewind();
        }
        ui.add_enabled(
            false,
            egui::Button::new(big("⏪")).min_size(Vec2::new(28.0, 24.0)),
        )
        .on_disabled_hover_text("Скоро");
        ui.add_enabled(
            false,
            egui::Button::new(big("⏩")).min_size(Vec2::new(28.0, 24.0)),
        )
        .on_disabled_hover_text("Скоро");
        if ui
            .add(egui::Button::new(big("⏹")).min_size(Vec2::new(28.0, 24.0)))
            .on_hover_text("Стоп")
            .clicked()
        {
            self.stop();
        }
        let play = RichText::new("▶").size(15.0).color(if self.is_playing() {
            Color32::from_rgb(90, 220, 110)
        } else {
            Color32::from_gray(220)
        });
        if ui
            .add(
                egui::Button::new(play)
                    .selected(self.is_playing())
                    .min_size(Vec2::new(28.0, 24.0)),
            )
            .on_hover_text("Играть / пауза (пробел)")
            .clicked()
        {
            self.toggle_play();
        }
        let rec = RichText::new("⏺").size(15.0).color(if self.is_recording() {
            Color32::RED
        } else {
            Color32::from_rgb(220, 70, 70)
        });
        if ui
            .add(
                egui::Button::new(rec)
                    .selected(self.is_recording())
                    .min_size(Vec2::new(28.0, 24.0)),
            )
            .on_hover_text("Запись с микрофона (R)")
            .clicked()
        {
            self.toggle_record();
        }
    }

    /// The position display: bar.beat.sixteenth.tick, time, tempo, meter.
    fn lcd(&mut self, ui: &mut egui::Ui) {
        egui::Frame::new()
            .fill(LCD_BG)
            .corner_radius(CornerRadius::same(8))
            .stroke(Stroke::new(1.0_f32, Color32::from_gray(60)))
            .inner_margin(egui::Margin::symmetric(12, 2))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let session = self.editor.session();
                    let [bar, beat, div, tick] = lcd_position(
                        self.playhead,
                        session.sample_rate,
                        session.tempo_bpm,
                        u32::from(session.time_signature.numerator),
                    );
                    lcd_cell(
                        ui,
                        &format!("{bar:>3} {beat} {div} {tick:>3}"),
                        "ТАКТ ДОЛЯ ДЕЛ ТИК",
                    );
                    ui.separator();
                    lcd_cell(
                        ui,
                        &super::clock(self.playhead, session.sample_rate),
                        "ВРЕМЯ",
                    );
                    ui.separator();
                    ui.visuals_mut().override_text_color = Some(LCD_TEXT);
                    ui.vertical(|ui| {
                        ui.horizontal(|ui| self.meter_fields(ui));
                        ui.label(
                            RichText::new("ТЕМП / РАЗМЕР")
                                .size(9.0)
                                .color(Color32::from_gray(140)),
                        );
                    });
                    ui.separator();
                    lcd_cell(ui, "C maj", "ТОНАЛЬНОСТЬ");
                });
            });
    }

    fn mode_buttons(&mut self, ui: &mut egui::Ui) {
        let cycle = egui::Button::new("🔁").selected(self.editor.active_cycle().is_some());
        if ui
            .add(cycle)
            .on_hover_text("Цикл (C). Протяните по линейке, чтобы задать участок.")
            .clicked()
        {
            self.toggle_cycle();
        }
        placeholder(ui, "🎤");
        placeholder(ui, "1234");
        let click = egui::Button::new("🎵").selected(self.editor.metronome);
        if ui.add(click).on_hover_text("Метроном (K)").clicked() {
            self.toggle_metronome();
        }
    }

    /// Menus of the tracks area: Edit, Functions, View, snap and zoom.
    pub(crate) fn tracks_toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.menu_button("Правка", |ui| {
                if ui.button("Отменить (Ctrl+Z)").clicked() {
                    self.undo();
                    ui.close();
                }
                if ui.button("Повторить (Ctrl+Y)").clicked() {
                    self.redo();
                    ui.close();
                }
                if ui.button("Разрезать клип (S)").clicked() {
                    self.split_clip();
                    ui.close();
                }
                if ui.button("Дублировать клип (Ctrl+D)").clicked() {
                    self.duplicate_clip();
                    ui.close();
                }
            });
            ui.menu_button("Функции", |ui| {
                if ui.button("+ Маркер (M)").clicked() {
                    self.add_marker();
                    ui.close();
                }
                if ui.button("Шины…").clicked() {
                    self.buses_open = true;
                    ui.close();
                }
                ui.add_enabled(false, egui::Button::new("Квантизация (скоро)"));
            });
            ui.menu_button("Вид", |ui| {
                ui.checkbox(&mut self.layout.library, "Библиотека");
                ui.checkbox(&mut self.layout.inspector, "Инспектор");
                ui.checkbox(&mut self.layout.browser, "Браузер");
            });
            ui.separator();
            if ui
                .button("＋")
                .on_hover_text("Новая MIDI-дорожка")
                .clicked()
            {
                self.add_midi_track();
            }
            ui.separator();
            ui.checkbox(&mut self.snap, "Сетка: доля")
                .on_hover_text("Привязка клипов к долям. Shift при перетаскивании отключает.");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add(
                    egui::Slider::new(&mut self.zoom, super::MIN_ZOOM..=super::MAX_ZOOM)
                        .logarithmic(true)
                        .show_value(false),
                )
                .on_hover_text("Масштаб");
                ui.label("🔍");
            });
        });
    }

    /// Sound library: patch categories (placeholder).
    pub(crate) fn library(&mut self, ui: &mut egui::Ui) {
        section(ui, "Библиотека");
        ui.label(RichText::new("Звуки появятся позже").weak());
        for name in [
            "Электронные барабаны",
            "Акустические барабаны",
            "Бас",
            "Клавишные",
            "Синтезаторы",
            "Струнные",
            "Гитара",
            "Вокал",
        ] {
            ui.add_enabled(false, egui::Button::selectable(false, format!("▸ {name}")));
        }
    }

    /// Inspector of the selected track: region, channel strip, sends.
    pub(crate) fn inspector(&mut self, ui: &mut egui::Ui) {
        section(ui, "Инспектор");
        let Some(index) = self
            .editor
            .selected_track
            .filter(|&t| t < self.editor.session().tracks.len())
        else {
            ui.label(RichText::new("Щёлкните по дорожке").weak());
            return;
        };
        egui::CollapsingHeader::new("Регион")
            .default_open(false)
            .show(ui, |ui| {
                ui.label(RichText::new("Петля, транспонирование: скоро").weak());
            });
        egui::CollapsingHeader::new(
            RichText::new(&self.editor.session().tracks[index].name).strong(),
        )
        .id_salt("inspector_track")
        .default_open(false)
        .show(ui, |ui| {
            ui.label(RichText::new("Параметры дорожки: скоро").weak());
        });
        ui.add_space(4.0);
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            self.channel_strip(ui, index, true);
            self.master_strip(ui, 150.0);
        });
    }

    /// One channel strip; `full` adds effect slots and sends.
    #[allow(clippy::too_many_lines)]
    fn channel_strip(&mut self, ui: &mut egui::Ui, index: usize, full: bool) {
        let track = self.editor.session().tracks[index].clone();
        let selected = self.editor.selected_track == Some(index);
        let fill = if selected {
            Color32::from_gray(56)
        } else {
            Color32::from_gray(44)
        };
        let playing = self.is_playing();
        let response = egui::Frame::new()
            .fill(fill)
            .stroke(Stroke::new(1.0_f32, Color32::from_gray(28)))
            .corner_radius(CornerRadius::same(4))
            .inner_margin(4.0)
            .show(ui, |ui| {
                ui.set_width(STRIP_WIDTH);
                ui.spacing_mut().item_spacing.y = 3.0;
                ui.vertical_centered(|ui| {
                    stub_slot(ui, "Settings");
                    eq_thumb(ui);
                    if full {
                        strip_caption(ui, "MIDI FX");
                        stub_slot(ui, "");
                        strip_caption(ui, "Audio FX");
                        for effect in &track.effects {
                            let title = effect_title(&effect.kind);
                            let color = if effect.bypass {
                                Color32::from_gray(120)
                            } else {
                                Color32::from_rgb(150, 200, 255)
                            };
                            let button =
                                egui::Button::new(RichText::new(title).size(10.0).color(color))
                                    .min_size(Vec2::new(ui.available_width(), 16.0))
                                    .fill(Color32::from_gray(30));
                            if ui.add(button).clicked() {
                                self.fx = Some(FxTarget::Track(index));
                            }
                        }
                        let add = egui::Button::new(RichText::new("+").size(10.0))
                            .min_size(Vec2::new(ui.available_width(), 16.0));
                        if ui.add(add).on_hover_text("Добавить эффект").clicked() {
                            self.fx = Some(FxTarget::Track(index));
                        }
                        strip_caption(ui, "Sends");
                        let buses = self.editor.session().buses.clone();
                        for (bus, b) in buses.iter().enumerate() {
                            let mut level = track.send_level(bus);
                            if ui
                                .add(
                                    egui::Slider::new(&mut level, 0.0..=1.0)
                                        .text(RichText::new(&b.name).size(9.0))
                                        .show_value(false),
                                )
                                .changed()
                            {
                                let _ = self.editor.set_send(index, bus, level);
                            }
                        }
                        let add = egui::Button::new(RichText::new("+ шина").size(10.0))
                            .min_size(Vec2::new(ui.available_width(), 16.0));
                        if ui.add(add).clicked() {
                            self.buses_open = true;
                        }
                        strip_caption(ui, "Output");
                        stub_slot(ui, "Stereo Out");
                    } else {
                        let count = track.effects.len();
                        let label = if count == 0 {
                            "Audio FX".to_owned()
                        } else {
                            format!("FX ×{count}")
                        };
                        let button = egui::Button::new(RichText::new(label).size(10.0))
                            .min_size(Vec2::new(ui.available_width(), 16.0));
                        if ui.add(button).clicked() {
                            self.fx = Some(FxTarget::Track(index));
                        }
                    }
                    ui.add_space(2.0);
                    let mut pan = track.pan;
                    let p = pan_knob(ui, &mut pan, track.automation.pan.is_empty());
                    ui.label(
                        RichText::new(db_label(track.gain))
                            .monospace()
                            .size(10.0)
                            .color(Color32::from_gray(210)),
                    );
                    let mut gain = track.gain;
                    let level = if playing && !track.mute {
                        fader_position(gain) * 0.85
                    } else {
                        0.0
                    };
                    let height = if full { 150.0 } else { 130.0 };
                    let g = fader(
                        ui,
                        &mut gain,
                        track.automation.gain.is_empty(),
                        height,
                        level,
                    );
                    if g.changed() || p.changed() {
                        if let Some(t) = self.editor.track_mut(index) {
                            t.gain = gain;
                            t.pan = pan;
                        }
                    }
                    let (mut mute, mut solo) = (track.mute, track.solo);
                    ui.horizontal(|ui| {
                        ui.add_space((ui.available_width() - 44.0).max(0.0) / 2.0);
                        ui.spacing_mut().item_spacing.x = 4.0;
                        let m = pill(ui, &mut mute, "M", Color32::from_rgb(90, 170, 255))
                            .on_hover_text("Без звука");
                        let s = pill(ui, &mut solo, "S", Color32::from_rgb(240, 200, 60))
                            .on_hover_text("Соло");
                        if m.changed() || s.changed() {
                            if let Some(t) = self.editor.track_mut(index) {
                                t.mute = mute;
                                t.solo = solo;
                            }
                        }
                    });
                    name_plate(ui, &track.name, track_color(track.color, index));
                });
            })
            .response;
        if response.interact(Sense::click()).clicked() {
            self.editor.selected_track = Some(index);
        }
    }

    /// Master channel strip (Stereo Out).
    fn master_strip(&mut self, ui: &mut egui::Ui, height: f32) {
        let playing = self.is_playing();
        egui::Frame::new()
            .fill(Color32::from_gray(48))
            .stroke(Stroke::new(1.0_f32, Color32::from_gray(28)))
            .corner_radius(CornerRadius::same(4))
            .inner_margin(4.0)
            .show(ui, |ui| {
                ui.set_width(STRIP_WIDTH);
                ui.spacing_mut().item_spacing.y = 3.0;
                ui.vertical_centered(|ui| {
                    stub_slot(ui, "Settings");
                    eq_thumb(ui);
                    strip_caption(ui, "Audio FX");
                    stub_slot(ui, "");
                    ui.add_space(34.0);
                    let mut master = self.editor.session().master_gain;
                    ui.label(
                        RichText::new(db_label(master))
                            .monospace()
                            .size(10.0)
                            .color(Color32::from_gray(210)),
                    );
                    let level = if playing {
                        fader_position(master) * 0.85
                    } else {
                        0.0
                    };
                    if fader(ui, &mut master, true, height, level).changed() {
                        self.editor.set_master_gain(master);
                    }
                    ui.horizontal(|ui| {
                        ui.add_space((ui.available_width() - 44.0).max(0.0) / 2.0);
                        ui.spacing_mut().item_spacing.x = 4.0;
                        pill_stub(ui, "M");
                        pill_stub(ui, "D");
                    });
                    name_plate(ui, "Stereo Out", Color32::from_gray(170));
                });
            });
    }

    fn bus_strip(&mut self, ui: &mut egui::Ui, index: usize) {
        let bus = self.editor.session().buses[index].clone();
        let playing = self.is_playing();
        egui::Frame::new()
            .fill(Color32::from_gray(42))
            .stroke(Stroke::new(1.0_f32, Color32::from_gray(28)))
            .corner_radius(CornerRadius::same(4))
            .inner_margin(4.0)
            .show(ui, |ui| {
                ui.set_width(STRIP_WIDTH);
                ui.spacing_mut().item_spacing.y = 3.0;
                ui.vertical_centered(|ui| {
                    stub_slot(ui, "Settings");
                    eq_thumb(ui);
                    let count = bus.effects.len();
                    let label = if count == 0 {
                        "Audio FX".to_owned()
                    } else {
                        format!("FX ×{count}")
                    };
                    let button = egui::Button::new(RichText::new(label).size(10.0))
                        .min_size(Vec2::new(ui.available_width(), 16.0));
                    if ui.add(button).clicked() {
                        self.fx = Some(FxTarget::Bus(index));
                    }
                    ui.add_space(34.0);
                    let mut gain = bus.gain;
                    ui.label(
                        RichText::new(db_label(gain))
                            .monospace()
                            .size(10.0)
                            .color(Color32::from_gray(210)),
                    );
                    let level = if playing {
                        fader_position(gain) * 0.6
                    } else {
                        0.0
                    };
                    if fader(ui, &mut gain, true, 130.0, level).changed() {
                        let _ = self.editor.set_bus_gain(index, gain);
                    }
                    ui.add_space(18.0);
                    name_plate(ui, &bus.name, Color32::from_rgb(140, 150, 170));
                });
            });
    }

    /// Mixer: a strip per track, then buses and the master.
    fn mixer(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Микшер").strong());
            placeholder(ui, "Single");
            placeholder(ui, "Tracks");
            placeholder(ui, "All");
        });
        egui::ScrollArea::horizontal()
            .id_salt("mixer")
            .show(ui, |ui| {
                ui.horizontal_top(|ui| {
                    for index in 0..self.editor.session().tracks.len() {
                        self.channel_strip(ui, index, false);
                    }
                    if self.editor.session().tracks.is_empty() {
                        ui.label(RichText::new("Нет дорожек").weak());
                    }
                    ui.separator();
                    for index in 0..self.editor.session().buses.len() {
                        self.bus_strip(ui, index);
                    }
                    self.master_strip(ui, 130.0);
                });
            });
    }

    /// Smart controls: big knobs of the selected track (mostly placeholders).
    fn smart_controls(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Быстрые регуляторы").strong());
        let Some(index) = self
            .editor
            .selected_track
            .filter(|&t| t < self.editor.session().tracks.len())
        else {
            ui.label(RichText::new("Выберите дорожку").weak());
            return;
        };
        ui.horizontal_wrapped(|ui| {
            let track = &self.editor.session().tracks[index];
            let (mut gain, mut pan) = (track.gain, track.pan);
            let g = ui.add(egui::Slider::new(&mut gain, 0.0..=1.0).text("Громкость"));
            let p = ui.add(egui::Slider::new(&mut pan, -1.0..=1.0).text("Панорама"));
            if g.changed() || p.changed() {
                if let Some(t) = self.editor.track_mut(index) {
                    t.gain = gain;
                    t.pan = pan;
                }
            }
            for name in [
                "Яркость",
                "Атака",
                "Хвост",
                "Пространство",
                "Драйв",
                "Ширина",
            ] {
                ui.add_enabled(
                    false,
                    egui::Button::new(format!("◉ {name}")).min_size(Vec2::new(90.0, 40.0)),
                )
                .on_disabled_hover_text("Заглушка");
            }
        });
    }

    /// Editor tabs: piano roll of the open MIDI clip, the rest are stubs.
    fn editors(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let _ = ui.selectable_label(true, "Piano Roll");
            placeholder(ui, "Score");
            placeholder(ui, "Step Sequencer");
            placeholder(ui, "Audio Track");
        });
        ui.separator();
        if self.piano_clip().is_some() {
            self.piano_roll(ui);
        } else {
            ui.label(RichText::new("Дважды щёлкните по MIDI-клипу, чтобы открыть ноты.").weak());
        }
    }

    /// Bottom area chosen in the control bar.
    pub(crate) fn bottom_area(&mut self, ctx: &egui::Context) {
        let Some(panel) = self.layout.bottom else {
            return;
        };
        let height = match panel {
            Bottom::Editor => PIANO_HEIGHT,
            Bottom::Mixer => 300.0,
            Bottom::SmartControls => 140.0,
        };
        egui::TopBottomPanel::bottom("bottom_area")
            .resizable(true)
            .default_height(height)
            .show(ctx, |ui| match panel {
                Bottom::Mixer => self.mixer(ui),
                Bottom::Editor => self.editors(ui),
                Bottom::SmartControls => self.smart_controls(ui),
            });
    }

    /// File browser on the right (tabs are placeholders except Project).
    pub(crate) fn browser(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let _ = ui.selectable_label(true, "Проект");
            placeholder(ui, "Медиа");
            placeholder(ui, "Все файлы");
        });
        ui.separator();
        ui.label(RichText::new("Файлы проекта:").weak());
        let mut seen = std::collections::BTreeSet::new();
        for track in &self.editor.session().tracks {
            for clip in &track.clips {
                if let Some(path) = clip.source.as_ref() {
                    if let Some(name) = std::path::Path::new(path).file_name() {
                        seen.insert(name.to_string_lossy().into_owned());
                    }
                }
            }
        }
        if seen.is_empty() {
            ui.label("Пусто. Перетащите WAV или MP3 на дорожки.");
        }
        for name in seen {
            ui.label(format!("🔊 {name}"));
        }
    }
}
