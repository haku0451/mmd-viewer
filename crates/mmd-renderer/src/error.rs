//! Renderer failures that the application can report.

/// Errors produced while creating or using the viewport GPU resources.
#[derive(Debug, thiserror::Error)]
pub enum RendererError {
    #[error("failed to create a GPU surface: {0}")]
    Surface(String),
    #[error("failed to select a GPU adapter: {0}")]
    Adapter(String),
    #[error("failed to create a GPU device: {0}")]
    Device(String),
    #[error(
        "Direct3D 12 initialization failed ({dx12}); fallback backends also failed ({fallback})"
    )]
    DeviceInit { dx12: String, fallback: String },
    #[error("window surface size must be non-zero")]
    ZeroSurface,
    #[error("viewport size must be non-zero")]
    ZeroViewport,
    #[error("editor grid vertex count does not fit in a 32-bit draw range")]
    GridVertexCount,
    #[error("the model mesh is empty or its indices do not match the vertices")]
    InconsistentMesh,
    #[error("the model has more bones than the renderer can store")]
    TooManyBones,
    /// The swap chain is temporarily unavailable because another window covers it.
    #[error("window surface is occluded")]
    SurfaceOccluded,
    #[error("window surface is outdated")]
    SurfaceOutdated,
    #[error("timed out while acquiring the window surface")]
    SurfaceTimeout,
    /// A swap-chain error that does not have a more specific recovery path.
    #[error("window surface error: {0}")]
    SurfaceLost(String),
}
