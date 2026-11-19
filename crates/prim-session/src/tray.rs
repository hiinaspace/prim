//! Where the system tray will live (round T1). It runs inside the session process
//! because that is the process that outlives Godot on a PC: a tray icon with a menu is
//! another client of [`crate::Control`], like the control socket is.
//!
//! Nothing is built here yet. T1 adds a thread started from [`start`] that owns the
//! icon, reads [`crate::Control::status`] for the voice state and calls the same
//! methods the socket does (`set_mic`, `quit`, ...). Keep supervision out of it.

use crate::Control;

/// Called once by `prim session` after the control socket is up. No tray yet.
pub fn start(_control: Control) {}
