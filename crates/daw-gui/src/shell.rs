//! Main window chrome in the style of a classic DAW (SPEC-034): control bar
//! with position display, library, inspector, browser and the bottom area
//! with mixer, editor and smart controls. Placeholders are marked as such.

use daw_gui::layout::{
    Bottom, FADER_FLOOR_DB, MeterState, MeterZone, db_label, fader_gain, fader_position,
    lcd_position, meter_zone, pan_angle, pan_label,
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

const LCD_CELL_HEIGHT: f32 = 38.0;

/// One display cell: a value row of fixed height and a small caption below,
/// so every cell of the LCD lines up regardless of the widget inside.
fn lcd_slot<R>(
    ui: &mut egui::Ui,
    width: f32,
    caption: &str,
    value: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    ui.allocate_ui_with_layout(
        Vec2::new(width, LCD_CELL_HEIGHT),
        egui::Layout::top_down(egui::Align::Center),
        |ui| {
            ui.set_min_size(Vec2::new(width, LCD_CELL_HEIGHT));
            ui.spacing_mut().item_spacing.y = 0.0;
            let inner = ui
                .allocate_ui_with_layout(
                    Vec2::new(width, 24.0),
                    egui::Layout::left_to_right(egui::Align::Center)
                        .with_main_align(egui::Align::Center),
                    |ui| {
                        ui.set_min_size(Vec2::new(width, 24.0));
                        value(ui)
                    },
                )
                .inner;
            ui.label(
                RichText::new(caption)
                    .size(9.0)
                    .color(Color32::from_gray(140)),
            );
            inner
        },
    )
    .inner
}

fn lcd_text(ui: &mut egui::Ui, value: &str) -> egui::Response {
    ui.add(
        egui::Label::new(RichText::new(value).monospace().size(18.0).color(LCD_TEXT))
            .sense(Sense::click()),
    )
}

/// Makes an editable LCD value (the tempo) look like the read-only ones next
/// to it: same font and height, no button frame, so the cells line up
/// (SPEC-045 §8).
fn lcd_style(ui: &mut egui::Ui) {
    let style = ui.style_mut();
    style.override_font_id = Some(egui::FontId::monospace(18.0));
    style.spacing.interact_size.y = 24.0;
    style.spacing.button_padding.y = 0.0;
    for w in [
        &mut style.visuals.widgets.inactive,
        &mut style.visuals.widgets.hovered,
        &mut style.visuals.widgets.active,
    ] {
        w.weak_bg_fill = Color32::TRANSPARENT;
        w.bg_fill = Color32::TRANSPARENT;
        w.bg_stroke = Stroke::NONE;
        w.fg_stroke.color = LCD_TEXT;
    }
}

/// Greyed menu entry for a feature that is not ready yet.
fn menu_stub(ui: &mut egui::Ui, text: &str) {
    ui.add_enabled(false, egui::Button::new(text))
        .on_disabled_hover_text("Скоро");
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

/// Meter colour for a level zone.
fn zone_color(zone: MeterZone) -> Color32 {
    match zone {
        MeterZone::Safe => Color32::from_rgb(80, 210, 90),
        MeterZone::Hot => Color32::from_rgb(235, 205, 60),
        MeterZone::Clip => Color32::from_rgb(235, 60, 50),
    }
}

/// Paints a stereo meter into `rect`: two bars, green/yellow/red by level,
/// with the held peak as a thin mark. Vertical bars fill upwards,
/// horizontal ones to the right (SPEC-042).
pub(crate) fn paint_meter_bars(
    painter: &egui::Painter,
    rect: egui::Rect,
    meter: &MeterState,
    vertical: bool,
) {
    painter.rect_filled(rect, CornerRadius::same(1), Color32::from_gray(20));
    let hot = fader_position(10f32.powf(-6.0 / 20.0));
    let gap = 1.0;
    for ch in 0..2 {
        // One bar: `along` is the 0..1 span along the level axis.
        let bar = |from: f32, to: f32| -> egui::Rect {
            if vertical {
                let w = (rect.width() - gap) / 2.0;
                let left = rect.left() + ch as f32 * (w + gap);
                egui::Rect::from_min_max(
                    egui::pos2(left, rect.bottom() - rect.height() * to),
                    egui::pos2(left + w, rect.bottom() - rect.height() * from),
                )
            } else {
                let h = (rect.height() - gap) / 2.0;
                let top = rect.top() + ch as f32 * (h + gap);
                egui::Rect::from_min_max(
                    egui::pos2(rect.left() + rect.width() * from, top),
                    egui::pos2(rect.left() + rect.width() * to, top + h),
                )
            }
        };
        let lit = fader_position(meter.level[ch]);
        if lit > 0.0 {
            let zones = [
                (0.0, hot, MeterZone::Safe),
                (hot, 0.999, MeterZone::Hot),
                (0.999, 1.0, MeterZone::Clip),
            ];
            for (from, to, zone) in zones {
                if lit > from {
                    painter.rect_filled(bar(from, lit.min(to)), 0.0, zone_color(zone));
                }
            }
            if meter.level[ch] >= 1.0 {
                painter.rect_filled(bar(0.97, 1.0), 0.0, zone_color(MeterZone::Clip));
            }
        }
        let hold = fader_position(meter.hold[ch]);
        if hold > 0.0 {
            let span = if vertical {
                1.5 / rect.height()
            } else {
                1.5 / rect.width()
            };
            let mark = bar((hold - span).max(0.0), hold);
            painter.rect_filled(mark, 0.0, zone_color(meter_zone(meter.hold[ch])));
        }
    }
}

/// Gain and peak readouts above a fader, side by side (SPEC-042).
fn level_labels(ui: &mut egui::Ui, gain: f32, meter: &mut MeterState) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        ui.add_space((ui.available_width() - 72.0).max(0.0) / 2.0);
        ui.add_sized(
            Vec2::new(35.0, 15.0),
            egui::Label::new(
                RichText::new(db_label(gain))
                    .monospace()
                    .size(10.0)
                    .color(Color32::from_gray(210)),
            ),
        );
        peak_label(ui, meter);
    });
}

/// Highest level since the last reset, in dB, above a fader; red after a
/// clip; a click resets it (SPEC-042).
fn peak_label(ui: &mut egui::Ui, meter: &mut MeterState) {
    let text = if meter.peak > 0.0 {
        let db = 20.0 * meter.peak.log10();
        if db < FADER_FLOOR_DB {
            "-∞".to_owned()
        } else {
            format!("{db:.1}")
        }
    } else {
        "-∞".to_owned()
    };
    let (fill, color) = if meter.clipped() {
        (Color32::from_rgb(190, 40, 35), Color32::WHITE)
    } else {
        (Color32::from_gray(26), Color32::from_gray(200))
    };
    let button = egui::Button::new(RichText::new(text).monospace().size(10.0).color(color))
        .fill(fill)
        .min_size(Vec2::new(35.0, 15.0));
    if ui
        .add(button)
        .on_hover_text("Пиковый уровень с последнего сброса (щелчок — сбросить)")
        .clicked()
    {
        meter.reset_peak();
    }
}

/// Drawn volume fader with a stereo level meter on the left (SPEC-042).
fn fader(
    ui: &mut egui::Ui,
    gain: &mut f32,
    enabled: bool,
    height: f32,
    meter: &MeterState,
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
    let meter_rect = egui::Rect::from_min_max(
        egui::pos2(rect.left() + 3.0, track.top()),
        egui::pos2(rect.left() + 11.0, track.bottom()),
    );
    paint_meter_bars(painter, meter_rect, meter, true);
    let meter = meter_rect;
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

/// Size of one button of the tool group.
const TOOL_SIZE: Vec2 = Vec2::new(30.0, 22.0);

/// Segmented group of the editing tools with drawn icons (SPEC-044 §5).
/// Returns the tool the user clicked.
fn tool_group(ui: &mut egui::Ui, current: super::Tool) -> Option<super::Tool> {
    let mut picked = None;
    egui::Frame::new()
        .fill(Color32::from_gray(30))
        .corner_radius(CornerRadius::same(5))
        .inner_margin(2.0)
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            for tool in super::Tool::ALL {
                let (rect, response) = ui.allocate_exact_size(TOOL_SIZE, Sense::click());
                let on = tool == current;
                let fill = if on {
                    Color32::from_rgb(70, 110, 170)
                } else if response.hovered() {
                    Color32::from_gray(60)
                } else {
                    Color32::TRANSPARENT
                };
                let ink = if on {
                    Color32::WHITE
                } else {
                    Color32::from_gray(200)
                };
                ui.painter().rect_filled(rect, CornerRadius::same(4), fill);
                paint_tool_icon(ui.painter(), rect.shrink2(Vec2::new(8.0, 5.0)), tool, ink);
                if response
                    .on_hover_text(format!(
                        "{}\nT — следующий инструмент, Esc — стрелка",
                        tool.label()
                    ))
                    .clicked()
                {
                    picked = Some(tool);
                }
            }
        });
    picked
}

/// Draws the icon of `tool` inside `r` with strokes only, so it does not
/// depend on the glyphs a font happens to have.
fn paint_tool_icon(painter: &egui::Painter, r: egui::Rect, tool: super::Tool, ink: Color32) {
    let line = Stroke::new(1.5_f32, ink);
    let at = |x: f32, y: f32| egui::pos2(r.left() + x * r.width(), r.top() + y * r.height());
    match tool {
        super::Tool::Pointer => {
            painter.add(egui::Shape::convex_polygon(
                vec![at(0.2, 0.0), at(0.2, 0.85), at(0.42, 0.65), at(0.75, 0.62)],
                ink,
                Stroke::NONE,
            ));
            painter.line_segment([at(0.42, 0.65), at(0.6, 1.0)], line);
        }
        super::Tool::Scissors => {
            painter.line_segment([at(0.15, 0.0), at(0.7, 0.7)], line);
            painter.line_segment([at(0.85, 0.0), at(0.3, 0.7)], line);
            painter.circle_stroke(at(0.22, 0.85), r.height() * 0.15, line);
            painter.circle_stroke(at(0.78, 0.85), r.height() * 0.15, line);
        }
        super::Tool::Glue => {
            painter.rect_stroke(
                egui::Rect::from_min_max(at(0.0, 0.2), at(0.45, 0.8)),
                CornerRadius::same(1),
                line,
                egui::StrokeKind::Inside,
            );
            painter.rect_stroke(
                egui::Rect::from_min_max(at(0.55, 0.2), at(1.0, 0.8)),
                CornerRadius::same(1),
                line,
                egui::StrokeKind::Inside,
            );
            painter.line_segment([at(0.3, 0.5), at(0.7, 0.5)], line);
        }
        super::Tool::Fade => {
            painter.line_segment([at(0.0, 1.0), at(0.5, 0.0)], line);
            painter.line_segment([at(0.5, 0.0), at(1.0, 1.0)], line);
            painter.line_segment([at(0.0, 1.0), at(1.0, 1.0)], line);
        }
    }
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

/// Lowest region gain the inspector shows; below it the clip is silent.
const MIN_REGION_DB: f32 = -60.0;

/// Note values offered by Quantize (SPEC-043 §3).
pub(crate) const QUANTIZE_DIVISIONS: [u32; 4] = [4, 8, 16, 32];

/// Greyed inspector row for a parameter that is not ready yet.
fn grey_row(ui: &mut egui::Ui, name: &str) {
    ui.add_enabled(false, egui::Label::new(name));
    ui.add_enabled(false, egui::Button::new("—").small())
        .on_disabled_hover_text("Скоро");
    ui.end_row();
}

/// Collapsible inspector block that always spans the full panel width.
fn inspector_block(
    ui: &mut egui::Ui,
    width: f32,
    id: &str,
    title: &str,
    body: impl FnOnce(&mut egui::Ui),
) {
    egui::Frame::new()
        .fill(Color32::from_gray(44))
        .corner_radius(CornerRadius::same(4))
        .inner_margin(egui::Margin::symmetric(6, 2))
        .show(ui, |ui| {
            ui.set_width(width - 12.0);
            egui::CollapsingHeader::new(RichText::new(title).strong())
                .id_salt(id)
                .default_open(false)
                .show(ui, body);
        });
    ui.add_space(2.0);
}

/// In full screen the system title bar is gone, so the window controls are
/// drawn in the control bar: minimize, leave full screen, close.
fn window_buttons(ui: &mut egui::Ui) {
    let ctx = ui.ctx().clone();
    if !ctx.input(|i| i.viewport().fullscreen.unwrap_or(false)) {
        return;
    }
    if ui.button("✕").on_hover_text("Закрыть").clicked() {
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
    if ui
        .button("🗗")
        .on_hover_text("Выйти из полноэкранного режима (F11)")
        .clicked()
    {
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
    }
    if ui.button("🗕").on_hover_text("Свернуть").clicked() {
        // Most window managers ignore a minimize request from a full-screen
        // window, so leave full screen first (SPEC-045 §7).
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
    }
    ui.separator();
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
            let center_width = 1000.0_f32.min(full.width());
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
                window_buttons(ui);
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
            .on_hover_text("В начало (Home / Enter)")
            .clicked()
        {
            self.rewind();
        }
        if ui
            .add(egui::Button::new(big("⏪")).min_size(Vec2::new(28.0, 24.0)))
            .on_hover_text("Назад на такт (,)")
            .clicked()
        {
            self.step_bar(false);
        }
        if ui
            .add(egui::Button::new(big("⏩")).min_size(Vec2::new(28.0, 24.0)))
            .on_hover_text("Вперёд на такт (.)")
            .clicked()
        {
            self.step_bar(true);
        }
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

    /// The position display: bar.beat.sixteenth.tick, time, tempo, meter, key.
    fn lcd(&mut self, ui: &mut egui::Ui) {
        egui::Frame::new()
            .fill(LCD_BG)
            .corner_radius(CornerRadius::same(8))
            .stroke(Stroke::new(1.0_f32, Color32::from_gray(60)))
            .inner_margin(egui::Margin::symmetric(10, 3))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let session = self.editor.session();
                    let rate = session.sample_rate;
                    let [bar, beat, div, tick] = lcd_position(
                        self.playhead,
                        rate,
                        session.tempo_bpm,
                        u32::from(session.time_signature.numerator),
                    );
                    let position = format!("{bar:>3} {beat} {div} {tick:>3}");
                    let clock = super::clock(self.playhead, rate);
                    let r = lcd_slot(ui, 150.0, "ТАКТ ДОЛЯ ДЕЛ ТИК", |ui| {
                        lcd_text(ui, &position)
                    });
                    self.position_menu(&r);
                    ui.separator();
                    let r = lcd_slot(ui, 120.0, "ВРЕМЯ", |ui| lcd_text(ui, &clock));
                    self.position_menu(&r);
                    ui.separator();
                    ui.visuals_mut().override_text_color = Some(LCD_TEXT);
                    lcd_slot(ui, 96.0, "ТЕМП", |ui| {
                        lcd_style(ui);
                        self.tempo_field(ui);
                    });
                    ui.separator();
                    lcd_slot(ui, 96.0, "РАЗМЕР", |ui| self.meter_field(ui));
                    ui.separator();
                    let r = lcd_slot(ui, 80.0, "ТОНАЛЬНОСТЬ", |ui| {
                        lcd_text(ui, "C maj")
                    })
                    .on_hover_text("Тональность (правый клик — выбор, скоро)");
                    r.context_menu(|ui| {
                        for key in ["C maj", "G maj", "D maj", "A min", "E min"] {
                            menu_stub(ui, key);
                        }
                    });
                });
            });
    }

    fn position_menu(&mut self, response: &egui::Response) {
        response.context_menu(|ui| {
            if ui.button("В начало проекта").clicked() {
                self.rewind();
                ui.close();
            }
            if let Some((start, _)) = self.editor.active_cycle() {
                if ui.button("В начало цикла").clicked() {
                    self.seek(start);
                    ui.close();
                }
            }
            ui.separator();
            menu_stub(ui, "Показывать такты и время");
            menu_stub(ui, "Показывать SMPTE");
            menu_stub(ui, "Перейти к позиции…");
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
                let midi = self.editor.selected_clip.filter(|&(t, c)| {
                    self.editor
                        .session()
                        .tracks
                        .get(t)
                        .and_then(|track| track.clips.get(c))
                        .is_some_and(audio_core::session::Clip::is_midi)
                });
                ui.add_enabled_ui(midi.is_some(), |ui| {
                    ui.menu_button("Квантизация", |ui| {
                        for division in QUANTIZE_DIVISIONS {
                            if ui.button(format!("1/{division}")).clicked() {
                                if let Some((t, c)) = midi {
                                    self.quantize(t, c, division);
                                }
                                ui.close();
                            }
                        }
                    });
                })
                .response
                .on_disabled_hover_text("Выберите MIDI-клип");
            });
            ui.menu_button("Вид", |ui| {
                ui.checkbox(&mut self.layout.library, "Библиотека");
                ui.checkbox(&mut self.layout.inspector, "Инспектор");
                ui.checkbox(&mut self.layout.browser, "Браузер");
            });
            ui.separator();
            if ui
                .add(egui::Button::new("＋ Дорожка").min_size(Vec2::new(0.0, TOOL_SIZE.y)))
                .on_hover_text("Новая MIDI-дорожка")
                .clicked()
            {
                self.add_midi_track();
            }
            ui.add_space(6.0);
            if let Some(tool) = tool_group(ui, self.tool) {
                self.tool = tool;
            }
            ui.add_space(6.0);
            let snap = egui::Button::new("▦ Сетка")
                .selected(self.snap)
                .min_size(Vec2::new(0.0, TOOL_SIZE.y));
            if ui
                .add(snap)
                .on_hover_text("Привязка клипов к долям. Shift при перетаскивании отключает.")
                .clicked()
            {
                self.snap = !self.snap;
            }
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

    /// Inspector of the selected track (SPEC-043): region, track, the
    /// track's strip with its output or selected send, notes.
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
        let width = ui.available_width();
        let clip = self
            .editor
            .selected_clip
            .filter(|&(t, c)| t == index && c < self.editor.session().tracks[t].clips.len());
        let title = match clip {
            Some((t, c)) if self.editor.session().tracks[t].clips[c].is_midi() => "Регион: MIDI",
            Some(_) => "Регион: аудио",
            None => "Регион",
        };
        inspector_block(ui, width, "inspector_region", title, |ui| {
            self.region_body(ui, clip);
        });
        let name = self.editor.session().tracks[index].name.clone();
        inspector_block(ui, width, "inspector_track", &name, |ui| {
            self.track_body(ui, index);
        });
        ui.add_space(4.0);
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            let height = self.channel_strip(ui, index, true);
            let bus = self
                .selected_send
                .filter(|&b| b < self.editor.session().buses.len());
            if let Some(bus) = bus {
                self.bus_strip(ui, bus);
            } else {
                self.selected_send = None;
                self.master_strip(ui, 150.0, Some(height));
            }
        });
        ui.add_space(4.0);
        self.notes_tabs(ui, index);
    }
    /// Region section: audio gain and fades or MIDI note functions; the
    /// rest is greyed (SPEC-043 §3).
    fn region_body(&mut self, ui: &mut egui::Ui, clip: Option<(usize, usize)>) {
        let Some((t, c)) = clip else {
            ui.label(RichText::new("Выберите клип на дорожке").weak());
            return;
        };
        let region = self.editor.session().tracks[t].clips[c].clone();
        egui::Grid::new("region_grid")
            .num_columns(2)
            .spacing([6.0, 3.0])
            .show(ui, |ui| {
                if region.is_midi() {
                    self.midi_region_rows(ui, t, c);
                    for name in [
                        "Mute",
                        "Loop",
                        "Q-Swing",
                        "Fine Tune",
                        "Dynamics",
                        "Clip Length",
                        "Score",
                    ] {
                        grey_row(ui, name);
                    }
                } else {
                    self.audio_region_rows(ui, t, c, &region);
                    for name in [
                        "Mute",
                        "Loop",
                        "Quantize (Flex)",
                        "Transpose",
                        "Fine Tune",
                        "Flex Time",
                        "Curve",
                        "Speed Fade",
                    ] {
                        grey_row(ui, name);
                    }
                }
            });
    }

    fn audio_region_rows(
        &mut self,
        ui: &mut egui::Ui,
        t: usize,
        c: usize,
        region: &audio_core::session::Clip,
    ) {
        let rate = f64::from(self.editor.session().sample_rate.max(1));
        ui.label("Gain");
        let mut db = if region.gain > 0.0 {
            (20.0 * region.gain.log10()).max(MIN_REGION_DB)
        } else {
            MIN_REGION_DB
        };
        let r = ui.add(
            egui::DragValue::new(&mut db)
                .range(MIN_REGION_DB..=0.0)
                .speed(0.2)
                .max_decimals(1)
                .suffix(" dB"),
        );
        self.group_drag(&r);
        if r.changed() {
            let gain = if db <= MIN_REGION_DB {
                0.0
            } else {
                10f32.powf(db / 20.0)
            };
            let _ = self.editor.set_clip_gain(t, c, gain);
        }
        ui.end_row();
        let ms = |samples: u64| samples as f64 * 1000.0 / rate;
        let to_samples = |ms: f64| (ms.max(0.0) * rate / 1000.0).round() as u64;
        let (mut fade_in, mut fade_out) = (ms(region.fade_in_samples), ms(region.fade_out_samples));
        let max = ms(region.length_samples);
        ui.label("Fade In");
        let a = ui.add(
            egui::DragValue::new(&mut fade_in)
                .range(0.0..=max)
                .speed(1.0)
                .max_decimals(0)
                .suffix(" мс"),
        );
        ui.end_row();
        ui.label("Fade Out");
        let b = ui.add(
            egui::DragValue::new(&mut fade_out)
                .range(0.0..=max)
                .speed(1.0)
                .max_decimals(0)
                .suffix(" мс"),
        );
        ui.end_row();
        self.group_drag(&a);
        self.group_drag(&b);
        if a.changed() || b.changed() {
            let (fi, fo) = (to_samples(fade_in), to_samples(fade_out));
            // The edited fade wins; the other one shrinks to fit.
            let (fi, fo) = if a.changed() {
                let fi = fi.min(region.length_samples);
                (fi, fo.min(region.length_samples - fi))
            } else {
                let fo = fo.min(region.length_samples);
                (fi.min(region.length_samples - fo), fo)
            };
            let _ = self.editor.set_fades(t, c, fi, fo);
        }
        self.fade_curve_rows(ui, t, c, (region.fade_in_curve, region.fade_out_curve));
        ui.label("Кроссфейд");
        if ui
            .button("Создать")
            .on_hover_text("С соседним клипом (Ctrl+Alt+X)")
            .clicked()
        {
            self.crossfade_clip(t, c);
        }
        ui.end_row();
    }

    /// Fade shapes (SPEC-045 §9): a curve slider per fade plus the
    /// Logic-like presets — linear, exponential (slow start) and logarithmic
    /// (fast start). Drawn into the region inspector grid.
    fn fade_curve_rows(&mut self, ui: &mut egui::Ui, t: usize, c: usize, curves: (f32, f32)) {
        let (mut fade_in, mut fade_out) = curves;
        let mut changed = false;
        for (label, curve) in [("Форма In", &mut fade_in), ("Форма Out", &mut fade_out)] {
            ui.label(label);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                let r = ui.add(
                    egui::DragValue::new(curve)
                        .range(-1.0..=1.0)
                        .speed(0.01)
                        .max_decimals(2),
                );
                self.group_drag(&r);
                changed |= r.changed();
                for (text, value, hint) in [
                    ("╱", 0.0, "Линейная"),
                    ("⌣", -0.5, "Экспоненциальная (медленное начало)"),
                    ("⌒", 0.5, "Логарифмическая (быстрое начало)"),
                ] {
                    #[allow(clippy::float_cmp, reason = "presets are exact constants")]
                    let on = *curve == value;
                    if ui
                        .add(egui::Button::new(text).selected(on).small())
                        .on_hover_text(hint)
                        .clicked()
                    {
                        *curve = value;
                        changed = true;
                    }
                }
            });
            ui.end_row();
        }
        if changed {
            let result = self.editor.set_fade_curves(t, c, fade_in, fade_out);
            if let Err(e) = result {
                self.status = format!("Ошибка: {e}");
            }
        }
    }

    fn midi_region_rows(&mut self, ui: &mut egui::Ui, t: usize, c: usize) {
        ui.label("Quantize");
        ui.horizontal(|ui| {
            for division in QUANTIZE_DIVISIONS {
                if ui.small_button(format!("1/{division}")).clicked() {
                    self.quantize(t, c, division);
                }
            }
        });
        ui.end_row();
        let id = ui.id().with(("region_amounts", t, c));
        let (mut transpose, mut velocity, mut gate) = ui
            .data(|d| d.get_temp::<(i32, i32, u32)>(id))
            .unwrap_or((0, 0, 100));
        ui.label("Transpose");
        ui.horizontal(|ui| {
            ui.add(
                egui::DragValue::new(&mut transpose)
                    .range(-96..=96)
                    .suffix(" пт"),
            );
            if ui.small_button("Применить").clicked() && transpose != 0 {
                let r = self.editor.transpose_notes(t, c, transpose);
                self.report(r, "Ноты транспонированы");
                transpose = 0;
            }
        });
        ui.end_row();
        ui.label("Velocity");
        ui.horizontal(|ui| {
            ui.add(egui::DragValue::new(&mut velocity).range(-99..=99));
            if ui.small_button("Применить").clicked() && velocity != 0 {
                let r = self.editor.offset_velocity(t, c, velocity);
                self.report(r, "Сила нажатия изменена");
                velocity = 0;
            }
        });
        ui.end_row();
        ui.label("Gate Time");
        ui.horizontal(|ui| {
            ui.add(egui::DragValue::new(&mut gate).range(25..=200).suffix(" %"));
            if ui.small_button("Применить").clicked() && gate != 100 {
                let r = self.editor.scale_gate(t, c, gate);
                self.report(r, "Длина нот изменена");
                gate = 100;
            }
        });
        ui.end_row();
        ui.data_mut(|d| d.insert_temp(id, (transpose, velocity, gate)));
    }

    /// Quantizes a MIDI clip to `1/division` notes (SPEC-043 §3).
    pub(crate) fn quantize(&mut self, t: usize, c: usize, division: u32) {
        let r = self.editor.quantize_notes(t, c, division);
        self.report(r, &format!("Квантизация 1/{division}"));
    }

    /// One undo step for a whole drag of a value field.
    fn group_drag(&mut self, response: &egui::Response) {
        if response.drag_started() || response.gained_focus() {
            self.editor.begin_group();
        }
        if response.drag_stopped() || response.lost_focus() {
            self.editor.end_group();
        }
    }

    /// Track section: name and channel number; the rest is greyed
    /// (SPEC-043 §4).
    fn track_body(&mut self, ui: &mut egui::Ui, index: usize) {
        let id = ui.id().with(("track_name", index));
        let current = self.editor.session().tracks[index].name.clone();
        let mut name = ui
            .data(|d| d.get_temp::<String>(id))
            .unwrap_or_else(|| current.clone());
        egui::Grid::new("track_grid")
            .num_columns(2)
            .spacing([6.0, 3.0])
            .show(ui, |ui| {
                ui.label("Имя");
                let r = ui.add(egui::TextEdit::singleline(&mut name).desired_width(110.0));
                if r.has_focus() {
                    ui.data_mut(|d| d.insert_temp(id, name.clone()));
                } else {
                    if r.lost_focus() && name.trim() != current {
                        let result = self.editor.rename_track(index, name.trim());
                        self.report(result, "Дорожка переименована");
                    }
                    ui.data_mut(|d| d.remove::<String>(id));
                }
                ui.end_row();
                ui.label("Канал");
                ui.label(format!("{}", index + 1));
                ui.end_row();
                for name in [
                    "Freeze",
                    "VCA",
                    "Automation Mode",
                    "Flex Mode",
                    "Key Limit",
                    "Velocity Limit",
                    "Delay",
                    "No Transpose",
                ] {
                    grey_row(ui, name);
                }
            });
    }

    /// Track and project notes tabs at the bottom (SPEC-043 §6).
    fn notes_tabs(&mut self, ui: &mut egui::Ui, index: usize) {
        let id = ui.id().with("notes_tab");
        let mut project = ui.data(|d| d.get_temp::<bool>(id)).unwrap_or(false);
        ui.horizontal(|ui| {
            ui.selectable_value(&mut project, false, "Заметки дорожки");
            ui.selectable_value(&mut project, true, "Заметки проекта");
        });
        ui.data_mut(|d| d.insert_temp(id, project));
        let mut text = if project {
            self.editor.session().notes.clone()
        } else {
            self.editor.session().tracks[index].notes.clone()
        };
        let r = ui.add(
            egui::TextEdit::multiline(&mut text)
                .desired_rows(5)
                .desired_width(f32::INFINITY)
                .hint_text("Текст, настройки, напоминания…"),
        );
        self.group_drag(&r);
        if r.changed() {
            let result = if project {
                self.editor.set_project_notes(&text)
            } else {
                self.editor.set_track_notes(index, &text)
            };
            if let Err(e) = result {
                self.status = format!("Ошибка: {e}");
            }
        }
    }

    /// One channel strip; `full` adds effect slots and sends.
    #[allow(clippy::too_many_lines)]
    fn channel_strip(&mut self, ui: &mut egui::Ui, index: usize, full: bool) -> f32 {
        let track = self.editor.session().tracks[index].clone();
        let selected = self.editor.selected_track == Some(index);
        let fill = if selected {
            Color32::from_gray(56)
        } else {
            Color32::from_gray(44)
        };
        // The strip senses clicks *behind* its contents, so the buttons on it
        // keep their own clicks (SPEC-044 §1).
        let response = ui
            .scope_builder(egui::UiBuilder::new().sense(Sense::click()), |ui| {
                self.channel_strip_body(ui, index, full, &track, fill);
            })
            .response;
        if response.clicked() {
            self.editor.selected_track = Some(index);
        }
        response.rect.height()
    }

    #[allow(clippy::too_many_lines)]
    fn channel_strip_body(
        &mut self,
        ui: &mut egui::Ui,
        index: usize,
        full: bool,
        track: &audio_core::SessionTrack,
        fill: Color32,
    ) {
        egui::Frame::new()
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
                            let chosen = self.selected_send == Some(bus);
                            if ui
                                .selectable_label(chosen, RichText::new(&b.name).size(9.0))
                                .on_hover_text("Показать шину справа")
                                .clicked()
                            {
                                self.selected_send = (!chosen).then_some(bus);
                            }
                            if ui
                                .add(egui::Slider::new(&mut level, 0.0..=1.0).show_value(false))
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
                    let mut meter = self.track_meters.get(index).copied().unwrap_or_default();
                    level_labels(ui, track.gain, &mut meter);
                    let mut gain = track.gain;
                    let height = if full { 150.0 } else { 130.0 };
                    let g = fader(
                        ui,
                        &mut gain,
                        track.automation.gain.is_empty(),
                        height,
                        &meter,
                    );
                    if let Some(slot) = self.track_meters.get_mut(index) {
                        slot.peak = meter.peak;
                    }
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
                    if full {
                        ui.horizontal(|ui| {
                            ui.add_space((ui.available_width() - 44.0).max(0.0) / 2.0);
                            ui.spacing_mut().item_spacing.x = 4.0;
                            let mut armed = self.editor.armed_track() == Some(index);
                            if pill(ui, &mut armed, "R", Color32::from_rgb(230, 60, 60))
                                .on_hover_text("Запись на эту дорожку")
                                .changed()
                            {
                                self.editor.arm_track(armed.then_some(index));
                            }
                            let mut monitor = track.input_monitor;
                            if pill(ui, &mut monitor, "I", Color32::from_rgb(240, 160, 60))
                                .on_hover_text("Мониторинг входа")
                                .changed()
                            {
                                if let Some(t) = self.editor.track_mut(index) {
                                    t.input_monitor = monitor;
                                }
                            }
                        });
                    }
                    name_plate(ui, &track.name, track_color(track.color, index));
                });
            });
    }

    /// Master channel strip (Stereo Out). With `match_height` the strip is
    /// stretched to that outer height and its fader is pinned to the bottom,
    /// so it lines up with the neighbouring channel strip.
    fn master_strip(&mut self, ui: &mut egui::Ui, height: f32, match_height: Option<f32>) {
        egui::Frame::new()
            .fill(Color32::from_gray(48))
            .stroke(Stroke::new(1.0_f32, Color32::from_gray(28)))
            .corner_radius(CornerRadius::same(4))
            .inner_margin(4.0)
            .show(ui, |ui| {
                ui.set_width(STRIP_WIDTH);
                ui.spacing_mut().item_spacing.y = 3.0;
                let top = ui.cursor().top();
                ui.vertical_centered(|ui| {
                    stub_slot(ui, "Settings");
                    eq_thumb(ui);
                    strip_caption(ui, "Audio FX");
                    stub_slot(ui, "");
                });
                // Outer height minus the frame's margins and stroke.
                let inner = match_height.map_or(0.0, |h| h - 10.0);
                let rest = (inner - (ui.cursor().top() - top)).max(height + 70.0);
                ui.allocate_ui_with_layout(
                    Vec2::new(STRIP_WIDTH, rest),
                    egui::Layout::bottom_up(egui::Align::Center),
                    |ui| {
                        ui.set_min_size(Vec2::new(STRIP_WIDTH, rest));
                        name_plate(ui, "Stereo Out", Color32::from_gray(170));
                        ui.horizontal(|ui| {
                            ui.add_space((ui.available_width() - 44.0).max(0.0) / 2.0);
                            ui.spacing_mut().item_spacing.x = 4.0;
                            let session = self.editor.session();
                            let (mut mute, mut dim) = (session.master_mute, session.master_dim);
                            if pill(ui, &mut mute, "M", Color32::from_rgb(90, 170, 255))
                                .on_hover_text("Выключить звук мастера")
                                .changed()
                            {
                                self.editor.set_master_mute(mute);
                            }
                            if pill(ui, &mut dim, "D", Color32::from_rgb(240, 160, 60))
                                .on_hover_text("Приглушить мастер на 20 дБ")
                                .changed()
                            {
                                self.editor.set_master_dim(dim);
                            }
                        });
                        let mut master = self.editor.session().master_gain;
                        let meter = self.master_meter;
                        if fader(ui, &mut master, true, height, &meter).changed() {
                            self.editor.set_master_gain(master);
                        }
                        level_labels(ui, master, &mut self.master_meter);
                    },
                );
            });
    }

    fn bus_strip(&mut self, ui: &mut egui::Ui, index: usize) {
        let bus = self.editor.session().buses[index].clone();
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
                    // Buses have no measurement of their own yet (SPEC-042).
                    let mut meter = MeterState::default();
                    level_labels(ui, gain, &mut meter);
                    if fader(ui, &mut gain, true, 130.0, &meter).changed() {
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
                    let mut tallest = None;
                    for index in 0..self.editor.session().tracks.len() {
                        let h = self.channel_strip(ui, index, false);
                        tallest = Some(tallest.map_or(h, |t: f32| t.max(h)));
                    }
                    if self.editor.session().tracks.is_empty() {
                        ui.label(RichText::new("Нет дорожек").weak());
                    }
                    ui.separator();
                    for index in 0..self.editor.session().buses.len() {
                        self.bus_strip(ui, index);
                    }
                    self.master_strip(ui, 130.0, tallest);
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

    /// Editor tabs: piano roll of the open MIDI clip or the audio editor of
    /// the open audio clip (SPEC-039); Score and Step Sequencer are stubs.
    fn editors(&mut self, ui: &mut egui::Ui) {
        let audio = self.audio_clip().is_some();
        ui.horizontal(|ui| {
            if ui.selectable_label(!audio, "Piano Roll").clicked() && audio {
                self.audio_edit = super::audio_editor::AudioEdit::default();
            }
            placeholder(ui, "Score");
            placeholder(ui, "Step Sequencer");
            let _ = ui.selectable_label(audio, "Audio Track");
        });
        ui.separator();
        if self.audio_clip().is_some() {
            self.audio_editor(ui);
        } else if self.piano_clip().is_some() {
            self.piano_roll(ui);
        } else {
            ui.label(
                RichText::new(
                    "Дважды щёлкните по MIDI-клипу, чтобы открыть ноты, или по аудиоклипу — чтобы открыть волну.",
                )
                .weak(),
            );
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
