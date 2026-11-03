//! Native file/folder dialog scheduling, kept off the UI thread.
//!
//! `rfd::FileDialog` is synchronous: calling it on the egui/winit event-loop
//! thread blocks the whole window until the native dialog is dismissed, and on
//! some Linux setups (GTK3 backend, or the XDG portal timing out) the blocking
//! call can wedge the event loop and crash the process. To avoid that we never
//! call `rfd` on the UI thread. Instead a worker thread shows the dialog and
//! sends the result back over an `mpsc` channel, which the app polls in
//! `update()` and repaints when a result arrives.
//!
//! This module holds only the *pure* scheduling state (the one-dialog guard and
//! the cancel-as-no-op rule) so it can be unit-tested without a display or any
//! native dialog. The actual `rfd` call and thread live in `main.rs`.

/// Whether a native dialog is currently open.
///
/// Exactly one native dialog may be open at a time. A second request while one
/// is in flight is ignored rather than stacking a second blocking dialog on top
/// of the first, which is itself a known crash trigger on Linux.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DialogGuard {
    open: bool,
}

impl DialogGuard {
    /// Returns `true` and records the dialog as open if none was open, or
    /// `false` (leaving the state unchanged) if a dialog is already in flight.
    pub fn begin(&mut self) -> bool {
        if self.open {
            return false;
        }
        self.open = true;
        true
    }

    /// Marks the dialog as closed. Called on every outcome — a picked path *and*
    /// a cancel — so a cancel cannot leave the guard stuck open.
    pub fn end(&mut self) {
        self.open = false;
    }

    /// Whether a dialog is currently open.
    pub const fn is_open(&self) -> bool {
        self.open
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn begin_opens_and_blocks_a_second_dialog() {
        let mut guard = DialogGuard::default();
        assert!(!guard.is_open());
        assert!(guard.begin());
        assert!(guard.is_open());
        // A second dialog while one is open is refused.
        assert!(!guard.begin());
        assert!(guard.is_open());
    }

    #[test]
    fn end_resets_on_cancel() {
        let mut guard = DialogGuard::default();
        assert!(guard.begin());
        // Cancel (no path) must still close the guard.
        guard.end();
        assert!(!guard.is_open());
        // A later dialog can open again.
        assert!(guard.begin());
    }

    #[test]
    fn end_when_idle_is_a_no_op() {
        let mut guard = DialogGuard::default();
        guard.end();
        assert!(!guard.is_open());
    }
}
