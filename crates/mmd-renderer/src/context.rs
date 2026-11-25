//! wgpu device and window-surface ownership.
//!
//! Direct3D 12 is requested first. If that attempt fails, initialization is
//! retried with wgpu's primary backends and the failure is logged. The
//! selected backend is part of [`AdapterSummary`] so the UI can show it.

use std::sync::Arc;

use winit::{dpi::PhysicalSize, window::Window};

use crate::error::RendererError;

/// GPU identity shown in the editor and written to the log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdapterSummary {
    /// Adapter name reported by wgpu.
    pub name: String,
    /// Backend name, such as `Dx12`.
    pub backend: String,
    /// Adapter class, such as `DiscreteGpu` or `IntegratedGpu`.
    pub device_type: String,
    pub dx12: bool,
    /// `true` when the Direct3D 12 attempt failed before this adapter was selected.
    pub used_fallback_backends: bool,
}

/// Device, queue, and swap chain used by the viewer window.
pub struct RenderContext {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    summary: AdapterSummary,
    max_texture_dimension: u32,
    color_sample_count: u32,
}

impl RenderContext {
    /// Create a device compatible with `window`.
    ///
    /// This future is intended to be polled once during application startup.
    pub async fn initialize(
        window: Arc<Window>,
        size: PhysicalSize<u32>,
    ) -> Result<Self, RendererError> {
        if size.width == 0 || size.height == 0 {
            return Err(RendererError::ZeroSurface);
        }
        match try_initialize(window.clone(), size, wgpu::Backends::DX12, false).await {
            Ok(context) => Ok(context),
            Err(dx12_error) => {
                tracing::warn!(
                    error = %dx12_error,
                    "Direct3D 12 initialization failed; retrying with primary backends"
                );
                try_initialize(window, size, wgpu::Backends::PRIMARY, true)
                    .await
                    .map_err(|fallback| RendererError::DeviceInit {
                        dx12: dx12_error.to_string(),
                        fallback: fallback.to_string(),
                    })
            }
        }
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    /// Swap-chain format. egui must render to this format.
    pub fn surface_format(&self) -> wgpu::TextureFormat {
        self.config.format
    }

    /// Largest 2D texture dimension supported by the selected device limits.
    pub fn max_texture_dimension(&self) -> u32 {
        self.max_texture_dimension
    }

    /// Sample count used by the viewport color and depth targets.
    pub fn color_sample_count(&self) -> u32 {
        self.color_sample_count
    }

    pub fn summary(&self) -> &AdapterSummary {
        &self.summary
    }

    pub fn resize_surface(&mut self, size: PhysicalSize<u32>) -> Result<(), RendererError> {
        if size.width == 0 || size.height == 0 {
            return Err(RendererError::ZeroSurface);
        }
        self.config.width = size.width;
        self.config.height = size.height;
        self.surface.configure(&self.device, &self.config);
        Ok(())
    }

    pub fn acquire(&self) -> Result<AcquiredFrame, RendererError> {
        match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(texture) => Ok(AcquiredFrame {
                texture,
                suboptimal: false,
            }),
            wgpu::CurrentSurfaceTexture::Suboptimal(texture) => Ok(AcquiredFrame {
                texture,
                suboptimal: true,
            }),
            wgpu::CurrentSurfaceTexture::Timeout => Err(RendererError::SurfaceTimeout),
            wgpu::CurrentSurfaceTexture::Occluded => Err(RendererError::SurfaceOccluded),
            wgpu::CurrentSurfaceTexture::Outdated => Err(RendererError::SurfaceOutdated),
            wgpu::CurrentSurfaceTexture::Lost => Err(RendererError::SurfaceLost(
                "the swap chain was lost and needs to be recreated".to_owned(),
            )),
            wgpu::CurrentSurfaceTexture::Validation => Err(RendererError::SurfaceLost(
                "acquiring the swap chain image raised a validation error".to_owned(),
            )),
        }
    }
}

/// A swap-chain image that must be presented or explicitly dropped.
pub struct AcquiredFrame {
    pub texture: wgpu::SurfaceTexture,
    /// The swap chain should be configured again after this image is presented.
    pub suboptimal: bool,
}

impl AcquiredFrame {
    pub fn view(&self) -> wgpu::TextureView {
        self.texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default())
    }
}

async fn try_initialize(
    window: Arc<Window>,
    size: PhysicalSize<u32>,
    backends: wgpu::Backends,
    used_fallback_backends: bool,
) -> Result<RenderContext, RendererError> {
    let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
    descriptor.backends = backends;
    let instance = wgpu::Instance::new(descriptor);
    tracing::info!(?backends, "creating wgpu instance");

    let surface = instance
        .create_surface(window)
        .map_err(|error| RendererError::Surface(error.to_string()))?;
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
            // This process is trusted desktop code, so adapter limits stay unbucketed.
            apply_limit_buckets: false,
        })
        .await
        .map_err(|error| RendererError::Adapter(error.to_string()))?;

    let color_sample_count = choose_color_sample_count(&adapter);
    let info = adapter.get_info();
    tracing::info!(
        name = %info.name,
        backend = ?info.backend,
        device_type = ?info.device_type,
        driver = %info.driver,
        "selected GPU adapter"
    );
    if info.device_type == wgpu::DeviceType::Cpu {
        tracing::warn!("selected adapter is a CPU rasterizer");
    }

    let (device, queue) = request_device(&adapter).await?;

    let capabilities = surface.get_capabilities(&adapter);
    let format = capabilities
        .formats
        .iter()
        .copied()
        .find(|format| {
            matches!(
                format,
                wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Rgba8Unorm
            )
        })
        .or_else(|| capabilities.formats.first().copied())
        .ok_or_else(|| RendererError::Surface("surface exposes no texture formats".to_owned()))?;
    let present_mode = capabilities
        .present_modes
        .iter()
        .copied()
        .find(|mode| *mode == wgpu::PresentMode::AutoVsync)
        .unwrap_or(wgpu::PresentMode::Fifo);
    let alpha_mode = capabilities
        .alpha_modes
        .first()
        .copied()
        .ok_or_else(|| RendererError::Surface("surface exposes no alpha modes".to_owned()))?;

    let config = wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format,
        color_space: wgpu::SurfaceColorSpace::Auto,
        width: size.width,
        height: size.height,
        present_mode,
        desired_maximum_frame_latency: 2,
        alpha_mode,
        view_formats: vec![],
    };
    surface.configure(&device, &config);
    tracing::info!(
        ?format,
        ?present_mode,
        color_sample_count,
        "configured window surface"
    );

    let dx12 = info.backend == wgpu::Backend::Dx12;
    if !dx12 {
        tracing::warn!(backend = ?info.backend, "Direct3D 12 was not selected");
    }

    Ok(RenderContext {
        surface,
        device,
        queue,
        config,
        summary: AdapterSummary {
            name: info.name,
            backend: format!("{:?}", info.backend),
            device_type: format!("{:?}", info.device_type),
            dx12,
            used_fallback_backends,
        },
        max_texture_dimension: adapter.limits().max_texture_dimension_2d,
        color_sample_count,
    })
}

async fn request_device(
    adapter: &wgpu::Adapter,
) -> Result<(wgpu::Device, wgpu::Queue), RendererError> {
    adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("mmd-viewer-device"),
            required_features: wgpu::Features::empty(),
            required_limits: adapter.limits(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            memory_hints: wgpu::MemoryHints::MemoryUsage,
            trace: wgpu::Trace::Off,
        })
        .await
        .map_err(|error| RendererError::Device(error.to_string()))
}

fn choose_color_sample_count(adapter: &wgpu::Adapter) -> u32 {
    let color = adapter
        .get_texture_format_features(wgpu::TextureFormat::Rgba8Unorm)
        .flags;
    let depth = adapter
        .get_texture_format_features(wgpu::TextureFormat::Depth32Float)
        .flags;
    for (count, flag) in [
        (4, wgpu::TextureFormatFeatureFlags::MULTISAMPLE_X4),
        (2, wgpu::TextureFormatFeatureFlags::MULTISAMPLE_X2),
    ] {
        if color.contains(flag) && depth.contains(flag) {
            return count;
        }
    }
    1
}
