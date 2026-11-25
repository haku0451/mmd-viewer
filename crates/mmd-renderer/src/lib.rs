//! GPU rendering for MMD Viewer.
//!
//! The viewport clears color and depth, draws the editor grid, and can then
//! draw a skinned model. Model materials, the edge shell, the shadow map, and
//! the ground shadow are separate passes.

mod context;
mod error;
mod grid;
mod model_draw;
mod viewport;

pub use context::{AcquiredFrame, AdapterSummary, RenderContext};
pub use error::RendererError;
pub use model_draw::{DrawFrame, DrawMaterial, ImageUpload, SkinnedModel};
pub use viewport::ViewportRenderer;
