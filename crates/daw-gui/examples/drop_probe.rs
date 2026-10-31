//! Diagnostic: compare candidate cursor-position sources during an external
//! (XDND) drag-and-drop, to determine which one reports the *real* cursor.
//!
//! The bug: on X11/XWayland, winit 0.30.13 discards `XdndPosition` and emits no
//! `CursorMoved` during a drag, so egui's `latest_pos` freezes. This tool shows
//! four labelled panels, one per candidate source, and draws a marker where each
//! source *thinks* the cursor is. Drop a file onto the window and see which
//! marker actually lines up with the cursor.
//!
//! Run: `cargo run -p daw-gui --bin drop-probe`
//!
//! Methods:
//!   A  latest_pos()     — egui's reported pointer (what the app uses today).
//!   B  interact_pos()   — egui's interact pointer.
//!   C  last PointerMoved event — last position in the raw event stream.
//!   D  XQueryPointer()  — raw X11 OS query (the ground truth).
//!
//! This is a throwaway diagnostic, not part of the product.

// Diagnostic only: XQueryPointer goes through `x11-dl`'s raw function pointer,
// which is inherently `unsafe`. This example is not part of the product.
#![allow(unsafe_code)]

use std::ptr::NonNull;

use eframe::Frame;
use eframe::egui::{self, Align2, Color32, FontId, Pos2, Stroke};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use x11_dl::xlib::Display;

/// A labelled drop zone whose marker is computed from one position source.
struct Panel {
    title: &'static str,
    color: Color32,
    /// Last computed cursor position in window-local points (for the marker).
    pos: Option<Pos2>,
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([900.0, 620.0])
            .with_title("drop-probe — XDND cursor-position diagnosis"),
        ..Default::default()
    };
    eframe::run_native(
        "drop-probe",
        options,
        Box::new(|cc| Ok(Box::new(DropProbe::new(cc)))),
    )
}

struct DropProbe {
    xlib: Option<Xlib>,
    /// Cached (display, window) extracted from the raw window handle.
    x11_target: Option<(NonNull<Display>, u64)>,
    panels: Vec<Panel>,
    last_event_pos: Option<Pos2>,
    /// OS native pixels-per-point (DPI scale), refreshed each frame.
    native_ppp: f32,
}

struct Xlib {
    lib: x11_dl::xlib::Xlib,
}

impl Xlib {
    fn load() -> Option<Self> {
        x11_dl::xlib::Xlib::open().ok().map(|lib| Self { lib })
    }
}

impl DropProbe {
    fn new(_cc: &eframe::CreationContext) -> Self {
        Self {
            xlib: Xlib::load(),
            x11_target: None,
            panels: vec![
                Panel {
                    title: "A  latest_pos()",
                    color: Color32::from_rgb(255, 120, 120),
                    pos: None,
                },
                Panel {
                    title: "B  interact_pos()",
                    color: Color32::from_rgb(120, 255, 120),
                    pos: None,
                },
                Panel {
                    title: "C  last PointerMoved event",
                    color: Color32::from_rgb(120, 160, 255),
                    pos: None,
                },
                Panel {
                    title: "D  XQueryPointer()",
                    color: Color32::from_rgb(255, 200, 80),
                    pos: None,
                },
            ],
            last_event_pos: None,
            native_ppp: 1.0,
        }
    }

    /// Query the OS cursor via X11 and return it in window-local points.
    fn x11_cursor_points(&self, display: NonNull<Display>, window: u64) -> Option<Pos2> {
        let lib = &self.xlib.as_ref()?.lib;
        let mut root = 0u64;
        let mut child = 0u64;
        let mut root_x = 0i32;
        let mut root_y = 0i32;
        let mut win_x = 0i32;
        let mut win_y = 0i32;
        let mut mask = 0u32;
        let ok = unsafe {
            (lib.XQueryPointer)(
                display.as_ptr().cast::<Display>(),
                window,
                &mut root,
                &mut child,
                &mut root_x,
                &mut root_y,
                &mut win_x,
                &mut win_y,
                &mut mask,
            )
        };
        if ok == 0 {
            return None;
        }
        // win_x/win_y are window-relative in physical pixels.
        let scale = if self.native_ppp.is_finite() && self.native_ppp > 0.0 {
            self.native_ppp
        } else {
            1.0
        };
        Some(Pos2::new(win_x as f32 / scale, win_y as f32 / scale))
    }

    fn draw_panel(&self, ui: &mut egui::Ui, idx: usize, panel: &Panel) {
        let (rect, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 96.0), egui::Sense::hover());
        let painter = ui.painter();
        painter.rect_filled(rect, 4.0, Color32::from_gray(30));
        painter.rect_stroke(
            rect,
            4.0,
            Stroke::new(1.0_f32, panel.color),
            egui::StrokeKind::Inside,
        );
        painter.text(
            rect.min + egui::vec2(12.0, 10.0),
            Align2::LEFT_TOP,
            panel.title,
            FontId::proportional(18.0),
            Color32::WHITE,
        );
        let label = match panel.pos {
            Some(p) => format!("({:.1}, {:.1}) points", p.x, p.y),
            None => "no position".to_owned(),
        };
        painter.text(
            rect.min + egui::vec2(12.0, 34.0),
            Align2::LEFT_TOP,
            label,
            FontId::proportional(14.0),
            Color32::from_gray(200),
        );
        if let Some(p) = panel.pos {
            let x = rect.min.x + p.x;
            if x >= rect.min.x && x <= rect.max.x {
                painter.line_segment(
                    [Pos2::new(x, rect.min.y), Pos2::new(x, rect.max.y)],
                    Stroke::new(3.0_f32, panel.color),
                );
            }
        }
        let _ = idx;
    }
}

impl eframe::App for DropProbe {
    fn update(&mut self, ctx: &egui::Context, frame: &mut Frame) {
        if self.x11_target.is_none() {
            self.x11_target = x11_target(frame);
        }

        // Refresh the OS DPI scale each frame; egui's ViewportInfo carries it.
        self.native_ppp = ctx
            .input(|i| i.viewport().native_pixels_per_point)
            .filter(|s| s.is_finite() && *s > 0.0)
            .unwrap_or(1.0);

        let (latest, interact) = ctx.input(|i| (i.pointer.latest_pos(), i.pointer.interact_pos()));
        ctx.input(|i| {
            for e in &i.raw.events {
                if let egui::Event::PointerMoved(p) = e {
                    self.last_event_pos = Some(*p);
                }
            }
        });
        let xquery = self
            .x11_target
            .and_then(|(d, w)| self.x11_cursor_points(d, w));

        self.panels[0].pos = latest;
        self.panels[1].pos = interact;
        self.panels[2].pos = self.last_event_pos;
        self.panels[3].pos = xquery;

        let dropped = ctx.input(|i| !i.raw.dropped_files.is_empty());
        if dropped {
            let fmt = |p: Option<Pos2>| match p {
                Some(p) => format!("({:.1}, {:.1})", p.x, p.y),
                None => "  none  ".to_owned(),
            };
            println!(
                "DROP  latest={}  interact={}  event={}  xquery={}  (window-local points, native_ppp={:.2})",
                fmt(latest),
                fmt(interact),
                fmt(self.last_event_pos),
                fmt(xquery),
                self.native_ppp,
            );
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("drop-probe — drop a file and watch which marker follows the cursor");
            ui.label(format!(
                "native pixels-per-point ≈ {:.2}. Each panel's marker shows where its source thinks the cursor is.",
                self.native_ppp
            ));
            ui.label("A/B/C freeze on X11 during an XDND drag; D should track the real cursor.");
            ui.separator();

            for (idx, panel) in self.panels.iter().enumerate() {
                self.draw_panel(ui, idx, panel);
                ui.add_space(4.0);
            }
        });

        ctx.request_repaint_after(std::time::Duration::from_millis(50));
    }
}

/// Extract the X11 (Display, Window) from the raw window handle, if on X11.
fn x11_target(frame: &Frame) -> Option<(NonNull<Display>, u64)> {
    let display = match frame.display_handle().ok()?.as_raw() {
        RawDisplayHandle::Xlib(h) => h.display?,
        RawDisplayHandle::Xcb(_) | RawDisplayHandle::Wayland(_) | _ => return None,
    };
    let window = match frame.window_handle().ok()?.as_raw() {
        RawWindowHandle::Xlib(h) => h.window,
        RawWindowHandle::Xcb(_) => return None,
        _ => return None,
    };
    Some((display.cast::<Display>(), window))
}
