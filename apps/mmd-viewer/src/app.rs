//! Native window, egui integration, and viewport presentation.

use std::sync::Arc;
use std::time::Instant;

use anyhow::Context;
use glam::{Mat4, Vec3};
use mmd_core::wgpu_clip_from_view;
use mmd_editor::EditorCamera;
use mmd_renderer::{DrawFrame, RenderContext, RendererError, ViewportRenderer};
use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    window::{Window, WindowId},
};

/// Run the native event loop.
///
/// `Ok(None)` is a normal shutdown. `Ok(Some(error))` is a startup or GPU
/// failure that should be shown to the user.
pub fn run() -> anyhow::Result<Option<anyhow::Error>> {
    let event_loop = EventLoop::new().context("failed to create the window event loop")?;
    let mut app = ViewerApp::new();
    event_loop
        .run_app(&mut app)
        .context("the window event loop failed")?;
    Ok(app.error.take())
}

struct ViewerApp {
    window: Option<Arc<Window>>,
    gpu: Option<RenderContext>,
    viewport: Option<ViewportRenderer>,
    ctx: egui::Context,
    egui_state: Option<egui_winit::State>,
    egui_renderer: Option<egui_wgpu::Renderer>,
    viewport_texture: Option<egui::TextureId>,
    camera: EditorCamera,
    session: crate::session::Session,
    last_tick: Instant,
    repaint_soon: bool,
    desired_viewport: (u32, u32),
    error: Option<anyhow::Error>,
}

impl ViewerApp {
    fn new() -> Self {
        let ctx = egui::Context::default();
        ctx.set_visuals(egui::Visuals::dark());
        install_japanese_font(&ctx);
        Self {
            window: None,
            gpu: None,
            viewport: None,
            ctx,
            egui_state: None,
            egui_renderer: None,
            viewport_texture: None,
            camera: EditorCamera::default(),
            session: crate::session::Session::new(),
            last_tick: Instant::now(),
            repaint_soon: false,
            desired_viewport: (1, 1),
            error: None,
        }
    }

    fn start(&mut self, event_loop: &ActiveEventLoop) -> anyhow::Result<()> {
        let attributes = Window::default_attributes()
            .with_title("MMD Viewer")
            .with_inner_size(winit::dpi::LogicalSize::new(1280.0, 800.0))
            .with_min_inner_size(winit::dpi::LogicalSize::new(800.0, 600.0));
        let window = Arc::new(
            event_loop
                .create_window(attributes)
                .context("failed to create the native window")?,
        );
        let mut size = window.inner_size();
        if size.width == 0 || size.height == 0 {
            size = PhysicalSize::new(1280, 800);
        }

        let gpu = pollster::block_on(RenderContext::initialize(Arc::clone(&window), size))
            .context("failed to initialize wgpu")?;
        tracing::info!(
            backend = %gpu.summary().backend,
            dx12 = gpu.summary().dx12,
            "gpu context is ready"
        );
        let viewport = ViewportRenderer::new(
            gpu.device(),
            size.width,
            size.height,
            gpu.color_sample_count(),
        )
        .context("failed to create the viewport target")?;
        let egui_state = egui_winit::State::new(
            self.ctx.clone(),
            egui::ViewportId::ROOT,
            window.as_ref(),
            Some(window.scale_factor() as f32),
            None,
            Some(gpu.max_texture_dimension() as usize),
        );
        let mut egui_renderer = egui_wgpu::Renderer::new(
            gpu.device(),
            gpu.surface_format(),
            egui_wgpu::RendererOptions::default(),
        );
        let viewport_texture = egui_renderer.register_native_texture(
            gpu.device(),
            viewport.color_view(),
            wgpu::FilterMode::Linear,
        );

        self.desired_viewport = (size.width, size.height);
        self.viewport_texture = Some(viewport_texture);
        self.egui_renderer = Some(egui_renderer);
        self.egui_state = Some(egui_state);
        self.viewport = Some(viewport);
        self.gpu = Some(gpu);
        self.window = Some(window);
        if let Some(window) = &self.window {
            window.request_redraw();
        }
        tracing::info!("native window is ready");
        Ok(())
    }

    fn resize(&mut self, size: PhysicalSize<u32>) -> anyhow::Result<()> {
        if size.width == 0 || size.height == 0 {
            tracing::debug!("skipping surface resize because the window has no drawable area");
            return Ok(());
        }
        if let Some(gpu) = &mut self.gpu {
            gpu.resize_surface(size)
                .context("failed to resize the window surface")?;
        }
        Ok(())
    }

    fn draw(&mut self) -> Result<(), DrawError> {
        let window = self
            .window
            .as_ref()
            .ok_or_else(|| DrawError::fatal(anyhow::anyhow!("redraw without a window")))?
            .clone();
        let raw_input = self
            .egui_state
            .as_mut()
            .ok_or_else(|| DrawError::fatal(anyhow::anyhow!("redraw without egui state")))?
            .take_egui_input(&window);
        let gpu_summary = self
            .gpu
            .as_ref()
            .ok_or_else(|| DrawError::fatal(anyhow::anyhow!("redraw without a GPU context")))?
            .summary()
            .clone();
        if let Some(gpu) = &self.gpu {
            let device = gpu.device().clone();
            let queue = gpu.queue().clone();
            let sample_count = gpu.color_sample_count();
            self.session.poll(&device, &queue, sample_count);
        }
        let now = Instant::now();
        let dt = now.duration_since(self.last_tick).as_secs_f32();
        self.last_tick = now;
        self.session.advance(dt);
        let texture_id = self.viewport_texture;
        let ctx = self.ctx.clone();
        let mut close_requested = false;
        let full_output = ctx.run_ui(raw_input, |ui| {
            let ui_output = crate::ui::show(
                ui,
                &mut self.camera,
                &gpu_summary,
                &mut self.session,
                texture_id,
            );
            close_requested = ui_output.close_requested;
            if let Some(size) = ui_output.viewport_pixels {
                self.desired_viewport = size;
            }
        });

        let pixels_per_point = full_output.pixels_per_point;
        let repaint = full_output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .is_some_and(|viewport| viewport.repaint_delay.is_zero());
        self.egui_state
            .as_mut()
            .ok_or_else(|| {
                DrawError::fatal(anyhow::anyhow!("egui state disappeared during the frame"))
            })?
            .handle_platform_output(&window, full_output.platform_output);
        let primitives = ctx.tessellate(full_output.shapes, pixels_per_point);
        let mut texture_updates = TextureUpdates::new(full_output.textures_delta);

        self.sync_viewport_target()?;
        let aspect = viewport_aspect(self.desired_viewport);
        let clip_from_world = self.clip_matrix(aspect)?.to_cols_array_2d();

        let window_size = window.inner_size();
        if window_size.width == 0 || window_size.height == 0 {
            tracing::debug!("skipping a frame because the window has no drawable area");
            return Ok(());
        }

        let frame = match self
            .gpu
            .as_ref()
            .ok_or_else(|| DrawError::fatal(anyhow::anyhow!("GPU context disappeared")))?
            .acquire()
        {
            Ok(frame) => frame,
            Err(RendererError::SurfaceOccluded) => {
                tracing::debug!("window surface is occluded");
                return Ok(());
            }
            Err(RendererError::SurfaceTimeout) => {
                tracing::warn!("timed out while acquiring the window surface");
                window.request_redraw();
                return Ok(());
            }
            Err(RendererError::SurfaceOutdated) => {
                let size = window.inner_size();
                self.resize(size)?;
                tracing::warn!("reconfigured an outdated window surface");
                window.request_redraw();
                return Ok(());
            }
            Err(error) => return Err(DrawError::Fatal(error.into())),
        };

        let (device, queue) = {
            let gpu = self
                .gpu
                .as_ref()
                .ok_or_else(|| DrawError::fatal(anyhow::anyhow!("GPU context disappeared")))?;
            (gpu.device().clone(), gpu.queue().clone())
        };
        let surface_view = frame.view();
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mmd-viewer-frame"),
        });
        let mut uploaded = false;
        if let Some(viewport) = &self.viewport {
            viewport.encode(
                &queue,
                &mut encoder,
                &clip_from_world,
                self.session.show_guides && !self.session.stage_open,
                self.session.show_guides && !self.session.stage_open,
            );
            let camera_position = self.camera_position();
            let view_from_world = self.view_matrix()?;
            let clip = Mat4::from_cols_array_2d(&clip_from_world);
            for actor in self.session.draws() {
                let materials = crate::session::draw_materials(actor.pose);
                let draw = DrawFrame {
                    positions: &actor.pose.positions,
                    uvs: &actor.pose.uvs,
                    skin: &actor.pose.skin,
                    rotations: &actor.pose.bone_rotation,
                    translations: &actor.pose.bone_translation,
                    materials: &materials,
                    light_direction: actor.pose.light_direction,
                    camera_position,
                    clip_from_world: clip,
                    view_from_world,
                    visible: actor.pose.visible,
                    self_shadow: self.session.self_shadow,
                    ground_shadow: self.session.ground_shadow,
                    upload_vertices: actor.upload_vertices,
                };
                actor.gpu.prepare(&queue, &draw);
                actor.gpu.encode(
                    &mut encoder,
                    viewport.scene_color_view(),
                    viewport.depth_view(),
                    &draw,
                );
                uploaded |= actor.upload_vertices;
            }
            viewport.resolve(&mut encoder);
        }
        if uploaded {
            self.session.clear_geometry_dirty();
        }

        let mut textures_delta = texture_updates.take();
        let user_buffers = {
            let egui_renderer = self
                .egui_renderer
                .as_mut()
                .ok_or_else(|| DrawError::fatal(anyhow::anyhow!("egui renderer disappeared")))?;
            for (id, images) in &textures_delta.set {
                for image in images {
                    egui_renderer.update_texture(&device, &queue, *id, image);
                }
            }
            let screen = egui_wgpu::ScreenDescriptor {
                size_in_pixels: [window_size.width, window_size.height],
                pixels_per_point,
            };
            let user_buffers =
                egui_renderer.update_buffers(&device, &queue, &mut encoder, &primitives, &screen);
            {
                let mut pass = encoder
                    .begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("egui"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &surface_view,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu::Color {
                                    r: 0.05,
                                    g: 0.05,
                                    b: 0.06,
                                    a: 1.0,
                                }),
                                store: wgpu::StoreOp::Store,
                            },
                            depth_slice: None,
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    })
                    .forget_lifetime();
                egui_renderer.render(&mut pass, &primitives, &screen);
            }
            for id in &textures_delta.free {
                egui_renderer.free_texture(id);
            }
            user_buffers
        };
        textures_delta.clear();

        let suboptimal = frame.suboptimal;
        queue.submit(
            user_buffers
                .into_iter()
                .chain(std::iter::once(encoder.finish())),
        );
        queue.present(frame.texture);
        if suboptimal {
            let size = window.inner_size();
            self.resize(size)?;
            tracing::warn!("reconfigured a suboptimal window surface");
            window.request_redraw();
        }

        if repaint {
            self.repaint_soon = true;
        }
        if close_requested {
            return Err(DrawError::Close);
        }
        Ok(())
    }

    fn sync_viewport_target(&mut self) -> Result<(), DrawError> {
        let max_dimension = self
            .gpu
            .as_ref()
            .ok_or_else(|| DrawError::fatal(anyhow::anyhow!("GPU context disappeared")))?
            .max_texture_dimension();
        let requested = self.desired_viewport;
        let width = requested.0.clamp(1, max_dimension.max(1));
        let height = requested.1.clamp(1, max_dimension.max(1));
        if (width, height) != requested {
            tracing::warn!(
                requested_width = requested.0,
                requested_height = requested.1,
                width,
                height,
                "clamped the viewport to the device texture limit"
            );
            self.desired_viewport = (width, height);
        }

        let device = self
            .gpu
            .as_ref()
            .ok_or_else(|| DrawError::fatal(anyhow::anyhow!("GPU context disappeared")))?
            .device()
            .clone();
        let resized = {
            let viewport = self
                .viewport
                .as_mut()
                .ok_or_else(|| DrawError::fatal(anyhow::anyhow!("viewport disappeared")))?;
            let needs_resize = viewport.size() != (width, height);
            if needs_resize {
                viewport.resize(&device, width, height)?;
            }
            needs_resize
        };
        if resized {
            let view = self
                .viewport
                .as_ref()
                .ok_or_else(|| DrawError::fatal(anyhow::anyhow!("viewport disappeared")))?
                .color_view()
                .clone();
            let texture_id = self.viewport_texture.ok_or_else(|| {
                DrawError::fatal(anyhow::anyhow!("viewport texture was not registered"))
            })?;
            self.egui_renderer
                .as_mut()
                .ok_or_else(|| DrawError::fatal(anyhow::anyhow!("egui renderer disappeared")))?
                .update_egui_texture_from_wgpu_texture(
                    &device,
                    &view,
                    wgpu::FilterMode::Linear,
                    texture_id,
                );
        }
        Ok(())
    }

    fn clip_matrix(&self, aspect: f32) -> Result<glam::Mat4, DrawError> {
        if self.session.follow_camera
            && let Some(camera) = self.session.camera_pose().and_then(|pose| pose.camera)
        {
            let view = Mat4::look_at_lh(camera.eye, camera.interest, Vec3::Y);
            let span = camera.eye.distance(camera.interest).max(1.0);
            return wgpu_clip_from_view(
                view,
                camera.fov_y_radians,
                aspect,
                (span * 0.02).clamp(0.05, 4.0),
                (span * 8.0).max(400.0),
            )
            .map_err(|error| DrawError::Fatal(error.into()));
        }
        self.camera
            .clip_from_world(aspect)
            .map_err(|error| DrawError::Fatal(error.into()))
    }

    /// View matrix of the camera that `clip_matrix` projects through.
    fn view_matrix(&self) -> Result<Mat4, DrawError> {
        if self.session.follow_camera
            && let Some(camera) = self.session.camera_pose().and_then(|pose| pose.camera)
        {
            return Ok(Mat4::look_at_lh(camera.eye, camera.interest, Vec3::Y));
        }
        self.camera
            .view_matrix()
            .map_err(|error| DrawError::Fatal(error.into()))
    }

    fn camera_position(&self) -> Vec3 {
        if self.session.follow_camera
            && let Some(camera) = self.session.camera_pose().and_then(|pose| pose.camera)
        {
            return camera.eye;
        }
        self.camera.eye()
    }
}

impl ApplicationHandler for ViewerApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() || self.error.is_some() {
            return;
        }
        if let Err(error) = self.start(event_loop) {
            tracing::error!(error = %error, "failed to start MMD Viewer");
            self.error = Some(error);
            event_loop.exit();
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        if let (Some(window), Some(state)) = (&self.window, &mut self.egui_state) {
            let response = state.on_window_event(window, &event);
            if response.repaint {
                self.repaint_soon = true;
            }
        }

        match event {
            WindowEvent::DroppedFile(path) => self.session.load_dropped(path),
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Err(error) = self.resize(size) {
                    tracing::error!(error = %error, "failed to resize the window");
                    self.error = Some(error);
                    event_loop.exit();
                }
            }
            WindowEvent::RedrawRequested => match self.draw() {
                Ok(()) => {}
                Err(DrawError::Close) => event_loop.exit(),
                Err(DrawError::Fatal(error)) => {
                    tracing::error!(error = %error, "rendering failed");
                    self.error = Some(error);
                    event_loop.exit();
                }
            },
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let want = self.repaint_soon || self.session.needs_redraw();
        if !want {
            event_loop.set_control_flow(ControlFlow::Wait);
            return;
        }
        let Some(period) = self.session.frame_rate.period() else {
            self.repaint_soon = false;
            if let Some(window) = &self.window {
                window.request_redraw();
            }
            event_loop.set_control_flow(ControlFlow::Poll);
            return;
        };
        let next = self.last_tick + period;
        if Instant::now() >= next {
            self.repaint_soon = false;
            if let Some(window) = &self.window {
                window.request_redraw();
            }
            event_loop.set_control_flow(ControlFlow::Poll);
        } else {
            event_loop.set_control_flow(ControlFlow::WaitUntil(next));
        }
    }
}

enum DrawError {
    Fatal(anyhow::Error),
    Close,
}

impl From<anyhow::Error> for DrawError {
    fn from(error: anyhow::Error) -> Self {
        Self::Fatal(error)
    }
}

impl From<RendererError> for DrawError {
    fn from(error: RendererError) -> Self {
        Self::Fatal(error.into())
    }
}

impl DrawError {
    fn fatal(error: anyhow::Error) -> Self {
        Self::Fatal(error)
    }
}

fn install_japanese_font(ctx: &egui::Context) {
    let candidates = [
        r"C:\Windows\Fonts\YuGothM.ttc",
        r"C:\Windows\Fonts\YuGothR.ttc",
        r"C:\Windows\Fonts\meiryo.ttc",
        r"C:\Windows\Fonts\msgothic.ttc",
    ];
    for path in candidates {
        match std::fs::read(path) {
            Ok(bytes) => {
                ctx.add_font(egui::FontInsert::new(
                    "japanese-ui",
                    egui::FontData::from_owned(bytes),
                    vec![
                        egui::InsertFontFamily {
                            family: egui::FontFamily::Proportional,
                            priority: egui::FontPriority::Highest,
                        },
                        egui::InsertFontFamily {
                            family: egui::FontFamily::Monospace,
                            priority: egui::FontPriority::Highest,
                        },
                    ],
                ));
                tracing::info!(path, "installed a Japanese UI font");
                return;
            }
            Err(error) => {
                tracing::debug!(path, error = %error, "Japanese font candidate was not readable");
            }
        }
    }
    tracing::error!("no Japanese UI font was found");
}

fn viewport_aspect(size: (u32, u32)) -> f32 {
    let (width, height) = size;
    if width == 0 || height == 0 {
        1.0
    } else {
        width as f32 / height as f32
    }
}

/// Clears egui texture deltas when a frame returns before they are uploaded.
struct TextureUpdates {
    delta: Option<egui::TexturesDelta>,
}

impl TextureUpdates {
    fn new(delta: egui::TexturesDelta) -> Self {
        Self { delta: Some(delta) }
    }

    fn take(&mut self) -> egui::TexturesDelta {
        self.delta.take().unwrap_or_else(|| egui::TexturesDelta {
            set: Default::default(),
            free: Default::default(),
        })
    }
}

impl Drop for TextureUpdates {
    fn drop(&mut self) {
        if let Some(delta) = &mut self.delta {
            delta.clear();
        }
    }
}
