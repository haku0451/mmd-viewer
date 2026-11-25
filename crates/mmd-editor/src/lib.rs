//! Editor operations that do not own GPU resources.
//!
//! The orbit camera frames the viewport. Scene editing, selection, gizmos,
//! and undo are later editor work and are not represented here.

mod camera;

pub use camera::{CameraDrag, CameraInput, CameraPointerButton, EditorCamera, EditorError};
