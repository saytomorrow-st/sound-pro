//! Audio clip editor in the bottom area (SPEC-039 §9–§14).

use eframe::egui::{self, Color32, Pos2, Rect, RichText, Sense, Stroke, Vec2};

use super::{App, Tool};

/// Pointer distance, in points, that grabs a clip edge.
const EDGE_GRAB: f32 = 6.0;

/// Which edge of the playing part is being dragged.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Edge {
    Start,
    End,
}

/// State of the audio editor panel.
#[derive(Default)]
pub(crate) struct AudioEdit {
    /// Audio clip open in the editor.
    pub(crate) clip: Option<(usize, usize)>,
    /// Edge drag in progress and the source frame it is at.
    trim: Option<(Edge, u64)>,
    /// Gain slider value (dB) while it is being dragged.
    gain_db: Option<f32>,
    /// Fade fields (ms) while they are being dragged.
    fades_ms: Option<(f64, f64)>,
}

/// Lowest value of the gain slider; below it the clip is silent.
const MIN_DB: f32 = -60.0;

fn gain_to_db(gain: f32) -> f32 {
    if gain <= 0.0 {
        MIN_DB
    } else {
        (20.0 * gain.log10()).max(MIN_DB)
    }
}

fn db_to_gain(db: f32) -> f32 {
    if db <= MIN_DB {
        0.0
    } else {
        10.0_f32.powf(db / 20.0)
    }
}

impl App {
    pub(crate) fn open_audio_editor(&mut self, at: (usize, usize)) {
        self.audio_edit = AudioEdit {
            clip: Some(at),
            ..AudioEdit::default()
        };
        self.close_piano();
        self.editor.selected_clip = Some(at);
        self.editor.selected_track = Some(at.0);
        self.layout.show_editor();
    }

    /// The open audio clip, closed if it is gone or is MIDI.
    pub(crate) fn audio_clip(&mut self) -> Option<(usize, usize)> {
        let (t, c) = self.audio_edit.clip?;
        let ok = self
            .editor
            .session()
            .tracks
            .get(t)
            .and_then(|track| track.clips.get(c))
            .is_some_and(|clip| !clip.is_midi());
        if !ok {
            self.audio_edit = AudioEdit::default();
        }
        self.audio_edit.clip
    }

    pub(crate) fn audio_editor(&mut self, ui: &mut egui::Ui) {
        let Some((t, c)) = self.audio_clip() else {
            return;
        };
        ui.horizontal(|ui| {
            let name = &self.editor.session().tracks[t].clips[c].name;
            ui.label(RichText::new(format!("Аудио — {name}")).strong());
            ui.label(
                RichText::new("тяните края — обрезка, клик — курсор, ножницы — разрез").weak(),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("✕").on_hover_text("Закрыть").clicked() {
                    self.audio_edit = AudioEdit::default();
                }
            });
        });
        if self.audio_edit.clip.is_none() {
            return;
        }
        self.audio_controls(ui, t, c);
        ui.add_space(4.0);
        self.audio_wave(ui, t, c);
    }

    fn audio_controls(&mut self, ui: &mut egui::Ui, t: usize, c: usize) {
        let clip = self.editor.session().tracks[t].clips[c].clone();
        let rate = f64::from(self.editor.session().sample_rate.max(1));
        ui.horizontal(|ui| {
            ui.label("Громкость");
            let mut db = self
                .audio_edit
                .gain_db
                .unwrap_or_else(|| gain_to_db(clip.gain));
            let slider = egui::Slider::new(&mut db, MIN_DB..=0.0)
                .suffix(" дБ")
                .custom_formatter(|v, _| {
                    if v <= f64::from(MIN_DB) {
                        "−∞".into()
                    } else {
                        format!("{v:.1}")
                    }
                });
            let response = ui.add(slider);
            if response.dragged() {
                self.audio_edit.gain_db = Some(db);
            } else if response.changed() || response.drag_stopped() {
                self.audio_edit.gain_db = None;
                let result = self.editor.set_clip_gain(t, c, db_to_gain(db));
                self.report(result, "Громкость клипа изменена");
            }
            ui.separator();
            #[allow(clippy::cast_precision_loss)]
            let current = (
                clip.fade_in_samples as f64 * 1000.0 / rate,
                clip.fade_out_samples as f64 * 1000.0 / rate,
            );
            let (mut fade_in, mut fade_out) = self.audio_edit.fades_ms.unwrap_or(current);
            ui.label("Фейд-ин, мс");
            let a = ui.add(
                egui::DragValue::new(&mut fade_in)
                    .range(0.0..=60_000.0)
                    .speed(1.0),
            );
            ui.label("Фейд-аут, мс");
            let b = ui.add(
                egui::DragValue::new(&mut fade_out)
                    .range(0.0..=60_000.0)
                    .speed(1.0),
            );
            if a.dragged() || b.dragged() {
                self.audio_edit.fades_ms = Some((fade_in, fade_out));
            } else if a.changed() || b.changed() || a.drag_stopped() || b.drag_stopped() {
                self.audio_edit.fades_ms = None;
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let to_samples = |ms: f64| (ms * rate / 1000.0).round().max(0.0) as u64;
                let result = self
                    .editor
                    .set_fades(t, c, to_samples(fade_in), to_samples(fade_out));
                self.report(result, "Фейды изменены");
            }
            ui.separator();
            if ui.button("✂ Разрезать по курсору").clicked() {
                self.split_clip();
            }
        });
    }

    /// Source frame under `x` in `rect`, which shows the whole file.
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    fn frame_at(rect: Rect, frames: u64, x: f32) -> u64 {
        let k = ((x - rect.min.x) / rect.width().max(1.0)).clamp(0.0, 1.0);
        (f64::from(k) * frames as f64).round() as u64
    }

    #[allow(clippy::cast_precision_loss)]
    fn frame_x(rect: Rect, frames: u64, frame: u64) -> f32 {
        rect.min.x + rect.width() * (frame as f64 / frames.max(1) as f64) as f32
    }

    #[allow(clippy::too_many_lines, clippy::cast_precision_loss)]
    fn audio_wave(&mut self, ui: &mut egui::Ui, t: usize, c: usize) {
        let clip = self.editor.session().tracks[t].clips[c].clone();
        let frames = clip.source_length.max(1);
        let size = Vec2::new(ui.available_width(), ui.available_height().max(80.0));
        let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 4.0, Color32::from_gray(24));

        let (mut from, mut to) = (
            clip.offset_samples,
            clip.offset_samples + clip.length_samples,
        );
        if let Some((edge, at)) = self.audio_edit.trim {
            match edge {
                Edge::Start => from = at.min(to.saturating_sub(1)),
                Edge::End => to = at.max(from + 1).min(frames),
            }
        }
        let x0 = Self::frame_x(rect, frames, from);
        let x1 = Self::frame_x(rect, frames, to);
        let active = Rect::from_x_y_ranges(x0..=x1, rect.y_range());
        painter.rect_filled(active, 0.0, Color32::from_rgb(40, 52, 70));

        let mid = rect.center().y;
        let half = rect.height() * 0.45;
        let gain = clip.gain;
        if let Some(peaks) = self.editor.peaks(&clip) {
            let width = rect.width().max(1.0) as usize;
            for px in 0..width {
                let x = rect.min.x + px as f32;
                let a = Self::frame_at(rect, frames, x);
                let b = Self::frame_at(rect, frames, x + 1.0).max(a + 1);
                let Some((lo, hi)) = peaks.range(a, b) else {
                    continue;
                };
                let color = if a >= from && a < to {
                    Color32::from_rgb(120, 200, 255)
                } else {
                    Color32::from_gray(80)
                };
                painter.line_segment(
                    [
                        Pos2::new(x, mid - hi * gain * half),
                        Pos2::new(x, mid - lo * gain * half),
                    ],
                    Stroke::new(1.0_f32, color),
                );
            }
        } else {
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "Волна не загружена (файл не найден)",
                egui::FontId::proportional(14.0),
                Color32::GRAY,
            );
        }

        let fade = Stroke::new(1.5_f32, Color32::from_rgb(255, 200, 60));
        if clip.fade_in_samples > 0 {
            let x = Self::frame_x(rect, frames, from + clip.fade_in_samples);
            painter.line_segment([Pos2::new(x0, rect.max.y), Pos2::new(x, rect.min.y)], fade);
        }
        if clip.fade_out_samples > 0 {
            let x = Self::frame_x(rect, frames, to.saturating_sub(clip.fade_out_samples));
            painter.line_segment([Pos2::new(x, rect.min.y), Pos2::new(x1, rect.max.y)], fade);
        }
        let edge = Stroke::new(2.0_f32, Color32::from_rgb(255, 255, 255));
        painter.line_segment([Pos2::new(x0, rect.min.y), Pos2::new(x0, rect.max.y)], edge);
        painter.line_segment([Pos2::new(x1, rect.min.y), Pos2::new(x1, rect.max.y)], edge);

        // Playhead, in source frames.
        let project_start = clip.start_samples;
        if let Some(f) = (self.playhead + clip.offset_samples).checked_sub(project_start) {
            if f <= frames {
                let x = Self::frame_x(rect, frames, f);
                painter.line_segment(
                    [Pos2::new(x, rect.min.y), Pos2::new(x, rect.max.y)],
                    Stroke::new(1.0_f32, Color32::from_rgb(255, 80, 80)),
                );
            }
        }

        self.audio_wave_input(&response, rect, t, c, (x0, x1));
    }

    fn audio_wave_input(
        &mut self,
        response: &egui::Response,
        rect: Rect,
        t: usize,
        c: usize,
        (x0, x1): (f32, f32),
    ) {
        let clip = self.editor.session().tracks[t].clips[c].clone();
        let frames = clip.source_length.max(1);
        let pointer = response.interact_pointer_pos().or(response.hover_pos());
        let near = |x: f32| pointer.is_some_and(|p| (p.x - x).abs() <= EDGE_GRAB);
        if near(x0) || near(x1) || self.audio_edit.trim.is_some() {
            response
                .ctx
                .set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
        if response.drag_started() {
            if near(x0) {
                self.audio_edit.trim = Some((Edge::Start, clip.offset_samples));
            } else if near(x1) {
                self.audio_edit.trim = Some((Edge::End, clip.offset_samples + clip.length_samples));
            }
        }
        if let (Some((edge, _)), Some(p)) = (self.audio_edit.trim, pointer) {
            self.audio_edit.trim = Some((edge, Self::frame_at(rect, frames, p.x)));
        }
        if response.drag_stopped() {
            if let Some((edge, at)) = self.audio_edit.trim.take() {
                // Source frame → timeline position of the clip's content.
                let position = if at >= clip.offset_samples {
                    clip.start_samples + (at - clip.offset_samples)
                } else {
                    clip.start_samples.saturating_sub(clip.offset_samples - at)
                };
                let result = match edge {
                    Edge::Start => self.editor.trim_start(t, c, position),
                    Edge::End => self.editor.trim_end(t, c, position),
                };
                self.report(result, "Клип обрезан");
            }
            return;
        }
        if response.clicked() {
            let Some(p) = pointer else { return };
            if p.x < x0 || p.x > x1 {
                return;
            }
            let frame = Self::frame_at(rect, frames, p.x);
            let at = (clip.start_samples + frame).saturating_sub(clip.offset_samples);
            if self.tool == Tool::Scissors {
                self.editor.selected_clip = Some((t, c));
                self.playhead_split(t, c, at);
            } else {
                self.seek(at);
            }
        }
    }

    fn playhead_split(&mut self, t: usize, c: usize, at: u64) {
        self.status = match self.editor.split_clip(t, c, at) {
            Ok(_) => "Клип разрезан".into(),
            Err(daw_gui::EditorError::SplitOutside) => "Точка вне клипа".into(),
            Err(e) => format!("Не удалось разрезать: {e}"),
        };
        self.editor.selected_clip = Some((t, c));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decibels_round_trip_and_floor_at_silence() {
        assert!((gain_to_db(1.0)).abs() < 1e-6);
        assert!((db_to_gain(-6.0) - 0.501).abs() < 1e-3);
        assert!((db_to_gain(gain_to_db(0.25)) - 0.25).abs() < 1e-6);
        assert!((gain_to_db(0.0) - MIN_DB).abs() < f32::EPSILON);
        assert!(db_to_gain(MIN_DB).abs() < f32::EPSILON);
    }
}
