//! Safe X11 cursor-position query for the drop-under-cursor fix.
//!
//! While a file is dragged from a file manager (XDND), winit's X11 backend
//! receives `XdndPosition` events but discards them, so it emits no
//! `CursorMoved` and egui's pointer position is frozen or missing. At drop
//! time we therefore read the OS cursor directly through X11's
//! `QueryPointer`, which reports the real window-relative position in
//! physical pixels. This module wraps that query behind a safe interface
//! using [`x11rb`] (no `unsafe`), so it can live inside
//! `#![forbid(unsafe_code)]` code.
//!
//! A `Window` id is server-global, so a fresh connection opened here can
//! query the same window eframe owns.

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use x11rb::protocol::xproto::ConnectionExt;

/// A lazily-opened X11 connection used to read the cursor at drop time.
///
/// Only the window id is kept; a fresh connection is opened per query and
/// dropped immediately, so the connection's event buffer never grows over a
/// long session. On non-X11 platforms (or when the raw handle is not Xlib)
/// this is a no-op so the drop falls back to egui's pointer.
#[derive(Debug)]
pub struct X11Cursor {
    window: x11rb::protocol::xproto::Window,
}

impl X11Cursor {
    /// Captures the window id from the eframe frame. Returns `None` when not
    /// running under X11 (Xlib). No connection is opened yet.
    pub fn from_frame(frame: &eframe::Frame) -> Option<Self> {
        let RawWindowHandle::Xlib(handle) = frame.window_handle().ok()?.as_raw() else {
            return None;
        };
        Some(Self {
            window: handle.window as x11rb::protocol::xproto::Window,
        })
    }

    /// The window-relative cursor position in physical pixels, or `None` if
    /// the query failed or the pointer is on another screen. `win_x`/`win_y`
    /// track the cursor even during an XDND drag, unlike egui's pointer.
    #[must_use]
    pub fn position(&self) -> Option<(f32, f32)> {
        self.query()
            .map(|r| (f32::from(r.win_x), f32::from(r.win_y)))
    }

    /// Debugging helper: window-relative and root-relative physical coords,
    /// plus `same_screen`, to disambiguate coordinate spaces in the logs.
    #[must_use]
    pub fn position_debug(&self) -> Option<(i16, i16, i16, i16, bool)> {
        self.query()
            .map(|r| (r.win_x, r.win_y, r.root_x, r.root_y, r.same_screen))
    }

    /// Open a fresh connection and query the pointer; returns the raw reply.
    /// `same_screen == false` means the pointer is on another screen.
    fn query(&self) -> Option<x11rb::protocol::xproto::QueryPointerReply> {
        let (conn, _screen) = x11rb::connect(None).ok()?;
        let reply = conn.query_pointer(self.window).ok()?.reply().ok()?;
        reply.same_screen.then_some(reply)
    }
}
