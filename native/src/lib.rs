mod network;
mod session;
pub mod viseme;
use godot::prelude::*;
struct PrimExtension;
#[gdextension]
unsafe impl ExtensionLibrary for PrimExtension {}
