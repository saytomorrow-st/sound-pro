//! Main window chrome in the style of a classic DAW (SPEC-034): control bar
//! with position display, library, inspector, browser and the bottom area
//! with mixer, editor and smart controls. Placeholders are marked as such.

use daw_gui::layout::{Bottom, db_label, lcd_position, pan_label};
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
        .on_disabled_hover_text("Заглушка: появится позже");
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
            ui.separator();
            self.transport_buttons(ui);
            ui.separator();
            self.lcd(ui);
            ui.separator();
            self.mode_buttons(ui);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.toggle_value(&mut self.layout.browser, "🗁")
                    .on_hover_text("Браузер файлов (F)");
                placeholder(ui, "📝");
                placeholder(ui, "♫ Loops");
            });
        });
    }

    fn transport_buttons(&mut self, ui: &mut egui::Ui) {
        if ui.button("⏮").on_hover_text("В начало (Home)").clicked() {
            self.rewind();
        }
        let label = if self.is_playing() { "⏸" } else { "▶" };
        if ui
            .add(egui::Button::new(RichText::new(label).size(16.0)).selected(self.is_playing()))
            .on_hover_text("Играть / пауза (пробел)")
            .clicked()
        {
            self.toggle_play();
        }
        if ui.button("⏹").on_hover_text("Стоп").clicked() {
            self.stop();
        }
        let rec = RichText::new("⏺").color(if self.is_recording() {
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
    }

    /// The position display: bar.beat.sixteenth.tick, time, tempo, meter.
    fn lcd(&mut self, ui: &mut egui::Ui) {
        egui::Frame::new()
            .fill(LCD_BG)
            .corner_radius(CornerRadius::same(4))
            .inner_margin(egui::Margin::symmetric(10, 2))
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
        placeholder(ui, "Tuner");
        placeholder(ui, "Count-in");
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
                ui.label("Без петли, транспонирование 0");
                ui.label(RichText::new("(заглушка)").weak());
            });
        ui.label(
            RichText::new(format!(
                "Дорожка: {}",
                self.editor.session().tracks[index].name
            ))
            .strong(),
        );
        ui.add_space(4.0);
        self.channel_strip(ui, index, true);
    }

    /// One channel strip; `full` adds effect slots and sends.
    #[allow(clippy::too_many_lines)]
    fn channel_strip(&mut self, ui: &mut egui::Ui, index: usize, full: bool) {
        let track = self.editor.session().tracks[index].clone();
        let selected = self.editor.selected_track == Some(index);
        let fill = if selected {
            Color32::from_gray(58)
        } else {
            Color32::from_gray(46)
        };
        let response = egui::Frame::new()
            .fill(fill)
            .corner_radius(CornerRadius::same(3))
            .inner_margin(4.0)
            .show(ui, |ui| {
                ui.set_width(if full {
                    ui.available_width()
                } else {
                    STRIP_WIDTH
                });
                ui.vertical_centered(|ui| {
                    ui.add_enabled(false, egui::Button::new("Settings").small())
                        .on_disabled_hover_text("Заглушка");
                    ui.add_enabled(false, egui::Button::new("EQ").small())
                        .on_disabled_hover_text("Заглушка");
                    if full {
                        ui.label(RichText::new("Audio FX").size(10.0).weak());
                        for effect in &track.effects {
                            let text = if effect.bypass {
                                format!("({})", effect_title(&effect.kind))
                            } else {
                                effect_title(&effect.kind).to_owned()
                            };
                            if ui.button(text).clicked() {
                                self.fx = Some(FxTarget::Track(index));
                            }
                        }
                        if ui
                            .small_button("+")
                            .on_hover_text("Добавить эффект")
                            .clicked()
                        {
                            self.fx = Some(FxTarget::Track(index));
                        }
                        ui.label(RichText::new("Sends").size(10.0).weak());
                        let buses = self.editor.session().buses.clone();
                        for (bus, b) in buses.iter().enumerate() {
                            let mut level = track.send_level(bus);
                            if ui
                                .add(
                                    egui::Slider::new(&mut level, 0.0..=1.0)
                                        .text(&b.name)
                                        .show_value(false),
                                )
                                .changed()
                            {
                                let _ = self.editor.set_send(index, bus, level);
                            }
                        }
                        if buses.is_empty() && ui.small_button("+ шина").clicked() {
                            self.buses_open = true;
                        }
                        ui.label(RichText::new("Stereo Out").size(10.0).weak());
                    } else {
                        let count = track.effects.len();
                        let label = if count == 0 {
                            "FX".to_owned()
                        } else {
                            format!("FX {count}")
                        };
                        if ui.small_button(label).clicked() {
                            self.fx = Some(FxTarget::Track(index));
                        }
                    }
                    let mut pan = track.pan;
                    let p = ui.add_enabled(
                        track.automation.pan.is_empty(),
                        egui::DragValue::new(&mut pan)
                            .range(-1.0..=1.0)
                            .speed(0.01)
                            .custom_formatter(|v, _| {
                                #[allow(clippy::cast_possible_truncation)]
                                pan_label(v as f32)
                            }),
                    );
                    ui.label(RichText::new(db_label(track.gain)).monospace().size(11.0));
                    let mut gain = track.gain;
                    let g = ui.add_enabled(
                        track.automation.gain.is_empty(),
                        egui::Slider::new(&mut gain, 0.0..=1.0)
                            .vertical()
                            .show_value(false),
                    );
                    if g.changed() || p.changed() {
                        if let Some(t) = self.editor.track_mut(index) {
                            t.gain = gain;
                            t.pan = pan;
                        }
                    }
                    let (mut mute, mut solo) = (track.mute, track.solo);
                    ui.horizontal(|ui| {
                        let m = ui.toggle_value(&mut mute, "M").on_hover_text("Без звука");
                        let s = ui.toggle_value(&mut solo, "S").on_hover_text("Соло");
                        if m.changed() || s.changed() {
                            if let Some(t) = self.editor.track_mut(index) {
                                t.mute = mute;
                                t.solo = solo;
                            }
                        }
                    });
                    let name = RichText::new(&track.name).size(11.0).color(Color32::BLACK);
                    egui::Frame::new()
                        .fill(track_color(track.color).gamma_multiply(1.4))
                        .corner_radius(CornerRadius::same(2))
                        .inner_margin(2.0)
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.add(egui::Label::new(name).truncate());
                        });
                });
            })
            .response;
        if response.interact(Sense::click()).clicked() {
            self.editor.selected_track = Some(index);
        }
    }

    /// Master channel strip at the right of the mixer.
    fn master_strip(&mut self, ui: &mut egui::Ui) {
        egui::Frame::new()
            .fill(Color32::from_gray(50))
            .corner_radius(CornerRadius::same(3))
            .inner_margin(4.0)
            .show(ui, |ui| {
                ui.set_width(STRIP_WIDTH);
                ui.vertical_centered(|ui| {
                    let mut master = self.editor.session().master_gain;
                    ui.label(RichText::new(db_label(master)).monospace().size(11.0));
                    if ui
                        .add(
                            egui::Slider::new(&mut master, 0.0..=1.0)
                                .vertical()
                                .show_value(false),
                        )
                        .changed()
                    {
                        self.editor.set_master_gain(master);
                    }
                    ui.label(RichText::new("Stereo Out").strong().size(11.0));
                });
            });
    }

    fn bus_strip(&mut self, ui: &mut egui::Ui, index: usize) {
        let bus = self.editor.session().buses[index].clone();
        egui::Frame::new()
            .fill(Color32::from_gray(44))
            .corner_radius(CornerRadius::same(3))
            .inner_margin(4.0)
            .show(ui, |ui| {
                ui.set_width(STRIP_WIDTH);
                ui.vertical_centered(|ui| {
                    let count = bus.effects.len();
                    let label = if count == 0 {
                        "FX".to_owned()
                    } else {
                        format!("FX {count}")
                    };
                    if ui.small_button(label).clicked() {
                        self.fx = Some(FxTarget::Bus(index));
                    }
                    let mut gain = bus.gain;
                    ui.label(RichText::new(db_label(gain)).monospace().size(11.0));
                    if ui
                        .add(
                            egui::Slider::new(&mut gain, 0.0..=1.0)
                                .vertical()
                                .show_value(false),
                        )
                        .changed()
                    {
                        let _ = self.editor.set_bus_gain(index, gain);
                    }
                    ui.add(egui::Label::new(RichText::new(&bus.name).size(11.0)).truncate());
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
                    self.master_strip(ui);
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
