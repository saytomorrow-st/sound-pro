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
use x11rb::connection::Connection;
use x11rb::protocol::xproto::ConnectionExt;

/// Converts a root (screen) pointer position to a client-area position by
/// subtracting the client window's own origin in root coords. A WM frame or
/// title bar above the client window is part of `origin_root`, so it is
/// excluded — this is the correction for the constant title-bar offset that
/// previously pushed drops below the track the user aimed at.
#[must_use]
pub fn root_to_client(root: (i16, i16), origin_root: (i16, i16)) -> (f32, f32) {
    (
        f32::from(root.0) - f32::from(origin_root.0),
        f32::from(root.1) - f32::from(origin_root.1),
    )
}

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

/// The cursor position in two coordinate spaces, for the debug cross overlay.
#[derive(Debug, Clone, Copy)]
pub struct CursorPos {
    /// Window-relative physical pixels, measured from the client area's
    /// top-left (what egui's `latest_pos` uses).
    pub client: (f32, f32),
    /// Root-window (screen) physical pixels.
    pub root: (i16, i16),
    /// The client window's (0,0) in root pixels (its top-left on screen,
    /// below any title bar).
    pub origin_root: (i16, i16),
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

    /// The client-area cursor position (physical px) plus the button mask,
    /// or `None` if the query failed or the pointer is on another screen.
    ///
    /// This is computed from the *root* pointer position minus the client
    /// window's own origin, obtained with `translate_coordinates` so the X
    /// window tree (WM reparenting, title bars) is walked correctly. Relying
    /// on `QueryPointer`'s `win_x`/`win_y` alone was wrong: under a decorated
    /// frame those are measured from the frame window's origin, which includes
    /// the title bar, so they ran a constant title-bar height below egui's
    /// client-area pointer.
    #[must_use]
    pub fn position_debug(&self) -> Option<CursorPos> {
        self.query()
    }

    /// Query the pointer in root coords and the window origin, then combine
    /// them into a client-area-relative position. `same_screen == false` means
    /// the pointer is on another screen.
    fn query(&self) -> Option<CursorPos> {
        let (conn, screen_num) = x11rb::connect(None).ok()?;
        let root = conn.setup().roots.get(screen_num)?.root;
        // Pointer position on the screen. Querying the root window makes
        // `root_x`/`root_y` true screen coordinates, independent of which
        // window (frame or client) owns the pointer.
        let ptr = conn.query_pointer(root).ok()?.reply().ok()?;
        // The client window's (0,0) in root coords. translate_coordinates
        // walks the window tree, so a WM frame / title bar above the client
        // window is reflected in `dst_y`.
        let origin = conn
            .translate_coordinates(self.window, root, 0, 0)
            .ok()?
            .reply()
            .ok()?;
        let client = root_to_client((ptr.root_x, ptr.root_y), (origin.dst_x, origin.dst_y));
        Some(CursorPos {
            client,
            root: (ptr.root_x, ptr.root_y),
            origin_root: (origin.dst_x, origin.dst_y),
            same_screen: ptr.same_screen && origin.same_screen,
            mask: u16::from(ptr.mask),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{CursorPos, root_to_client};

    fn pos(mask: u16) -> CursorPos {
        CursorPos {
            client: (0.0, 0.0),
            root: (0, 0),
            origin_root: (0, 0),
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

    // The owner's live bug: XQueryPointer's window coords included the title
    // bar, so a cursor over track 1 (egui y=271) read 32pt lower (303) and
    // landed below the tracks. Converting from root coords minus the client
    // window's origin removes that title-bar offset.
    #[test]
    fn root_to_client_removes_the_title_bar_offset() {
        // Title bar is 64px at 2x scale = 32 points; the client window origin
        // sits 64px below the screen top.
        let origin_root = (100_i16, 64_i16);
        // Cursor at screen y=606px: 64px title bar + 542px into the client.
        let root = (1000_i16, 606_i16);
        let client = root_to_client(root, origin_root);
        assert_eq!(client, (900.0, 542.0));
    }

    #[test]
    fn root_to_client_is_identity_for_an_undecorated_window() {
        let client = root_to_client((300_i16, 200_i16), (0_i16, 0_i16));
        assert_eq!(client, (300.0, 200.0));
    }
}
