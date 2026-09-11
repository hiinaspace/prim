mod network;
mod session;
use godot::prelude::*;
struct PrimExtension;
#[gdextension]
unsafe impl ExtensionLibrary for PrimExtension {}
