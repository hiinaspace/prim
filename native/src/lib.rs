mod media;
mod network;
mod session;
mod xr_monitor;
pub mod viseme;
use godot::prelude::*;
struct PrimExtension;
#[gdextension]
unsafe impl ExtensionLibrary for PrimExtension {}
