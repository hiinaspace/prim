mod media;
mod network;
mod pose;
mod prim_client;
mod session;
mod xr_monitor;
pub mod viseme;
use godot::prelude::*;
struct PrimExtension;
#[gdextension]
unsafe impl ExtensionLibrary for PrimExtension {}
