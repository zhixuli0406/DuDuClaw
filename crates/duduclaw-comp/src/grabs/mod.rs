// Adapted from smithay's `smallvil` example (`smallvil/src/grabs/mod.rs`),
// MIT License. See `main.rs` for the full attribution note.

pub mod move_grab;
pub use move_grab::{MoveClamp, MoveSurfaceGrab};

pub mod resize_grab;
pub use resize_grab::{ResizeClamp, ResizeSurfaceGrab};
