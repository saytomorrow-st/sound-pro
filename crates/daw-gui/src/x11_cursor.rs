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

/// Everything [`X11Cursor::query`] learns in one shot, for the drop logic and
/// the debug overlay.
#[derive(Debug, Clone, Copy)]
pub struct CursorPos {
    /// Window-relative physical pixels, measured from the client window's
    /// top-left. This is the value that (divided by `pixels_per_point`) maps
    /// to egui points and is what the drop target wants.
    pub win: (i16, i16),
    /// Root-window (screen) physical pixels.
    pub root: (i16, i16),
    /// Whether the pointer and the window are on the same screen.
    pub same_screen: bool,
    /// The current button/modifier mask from `QueryPointer`. A file drag holds
    /// a mouse button, so a non-zero button mask means the drag is still in
    /// flight (used to tell a drag-in-progress from a cancelled drag, since
    /// egui only reports the hovered files once at drag enter).
    pub mask: u16,
}

impl CursorPos {
    /// Whether any mouse button (1-5) is held, i.e. a file drag is still
    /// active. Used to detect when an XDND drag was cancelled without a drop.
    #[must_use]
    pub fn any_button_down(&self) -> bool {
        const BUTTONS: u16 = 0x1f00;
        self.mask & BUTTONS != 0
    }
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

    /// Full query result (window/root coords, same-screen, button mask) for
    /// cancel detection and the `SOUND_PRO_DROP_DEBUG` overlay.
    #[must_use]
    pub fn position_debug(&self) -> Option<CursorPos> {
        self.query()
    }

    /// Open a fresh connection and query the pointer.
    fn query(&self) -> Option<CursorPos> {
        let (conn, _screen) = x11rb::connect(None).ok()?;
        let reply = conn.query_pointer(self.window).ok()?.reply().ok()?;
        Some(CursorPos {
            win: (reply.win_x, reply.win_y),
            root: (reply.root_x, reply.root_y),
            same_screen: reply.same_screen,
            mask: u16::from(reply.mask),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::CursorPos;

    fn pos(mask: u16) -> CursorPos {
        CursorPos {
            win: (0, 0),
            root: (0, 0),
            same_screen: true,
            mask,
        }
    }

    // A file drag is carried by a held mouse button. The button mask from
    // QueryPointer is the only reliable signal that a drag is still in flight,
    // because egui only reports `hovered_files` once at drag enter.
    #[test]
    fn any_button_down_detects_a_drag_in_flight() {
        assert!(pos(0x0100).any_button_down()); // Button1
        assert!(pos(0x0400).any_button_down()); // Button3
        assert!(pos(0x1000).any_button_down()); // Button5
        assert!(pos(0x1f00).any_button_down()); // all buttons
        assert!(!pos(0x0000).any_button_down()); // no button: drag cancelled
        // Modifier keys alone (Shift=0x0001, etc.) must not count as a drag.
        assert!(!pos(0x0001).any_button_down());
        assert!(!pos(0x00ff).any_button_down());
    }
}
