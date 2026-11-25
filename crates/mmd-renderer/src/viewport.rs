//! Offscreen viewport target.
//!
//! The scene clear and the editor grid are separate render passes. A model
//! pass, when one is recorded, loads both the color and the depth that the
//! grid wrote.

use wgpu::util::DeviceExt;

use crate::error::RendererError;
use crate::grid::{GRID_SHADER, GridVertex, editor_grid};

/// Color under the empty viewport before the editor grid is drawn.
pub const VIEWPORT_CLEAR_COLOR: wgpu::Color = wgpu::Color {
    r: 0.11,
    g: 0.12,
    b: 0.14,
    a: 1.0,
};

/// GPU resources for the color target displayed by egui.
pub struct ViewportRenderer {
    color: wgpu::Texture,
    view: wgpu::TextureView,
    scene_color: Option<wgpu::Texture>,
    scene_view: wgpu::TextureView,
    depth: wgpu::Texture,
    depth_view: wgpu::TextureView,
    sample_count: u32,
    width: u32,
    height: u32,
    pipeline: wgpu::RenderPipeline,
    vertex_buffer: wgpu::Buffer,
    floor_count: u32,
    axis_count: u32,
    uniform_buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

impl ViewportRenderer {
    /// Create a viewport target and the editor grid pipeline.
    pub fn new(
        device: &wgpu::Device,
        width: u32,
        height: u32,
        sample_count: u32,
    ) -> Result<Self, RendererError> {
        let targets = create_target(device, width, height, sample_count)?;
        let grid = editor_grid();
        let vertex_count =
            u32::try_from(grid.vertices.len()).map_err(|_| RendererError::GridVertexCount)?;
        if grid.floor_count + grid.axis_count != vertex_count {
            return Err(RendererError::GridVertexCount);
        }
        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("editor-grid-vertices"),
            contents: bytemuck::cast_slice(&grid.vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("editor-camera-uniform"),
            size: std::mem::size_of::<[[f32; 4]; 4]>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("editor-camera-layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("editor-camera-bind-group"),
            layout: &bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("editor-grid-shader"),
            source: wgpu::ShaderSource::Wgsl(GRID_SHADER.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("editor-grid-pipeline-layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("editor-grid-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[Some(vertex_layout())],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::LineList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::LessEqual),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState {
                count: sample_count,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        Ok(Self {
            color: targets.color,
            view: targets.color_view,
            scene_color: targets.scene_color,
            scene_view: targets.scene_view,
            depth: targets.depth,
            depth_view: targets.depth_view,
            sample_count,
            width,
            height,
            pipeline,
            vertex_buffer,
            floor_count: grid.floor_count,
            axis_count: grid.axis_count,
            uniform_buffer,
            bind_group,
        })
    }

    /// Current target size in physical pixels.
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Color target sampled by egui. The texture format is `Rgba8Unorm`.
    pub fn color_texture(&self) -> &wgpu::Texture {
        &self.color
    }

    /// Resolved color view sampled by egui.
    pub fn color_view(&self) -> &wgpu::TextureView {
        &self.view
    }

    /// Color view the scene passes render into. This is multisampled when the device allows it.
    pub fn scene_color_view(&self) -> &wgpu::TextureView {
        &self.scene_view
    }

    /// Depth target shared with the model passes.
    pub fn depth_view(&self) -> &wgpu::TextureView {
        &self.depth_view
    }

    /// Recreate the color target when the viewport widget changes size.
    pub fn resize(
        &mut self,
        device: &wgpu::Device,
        width: u32,
        height: u32,
    ) -> Result<(), RendererError> {
        if self.width == width && self.height == height {
            return Ok(());
        }
        let targets = create_target(device, width, height, self.sample_count)?;
        self.color = targets.color;
        self.view = targets.color_view;
        self.scene_color = targets.scene_color;
        self.scene_view = targets.scene_view;
        self.depth = targets.depth;
        self.depth_view = targets.depth_view;
        self.width = width;
        self.height = height;
        Ok(())
    }

    /// Record the empty scene clear and the editor grid pass.
    ///
    /// `clip_from_world` is the column-major matrix produced by `mmd-core`.
    pub fn encode(
        &self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        clip_from_world: &[[f32; 4]; 4],
        show_floor: bool,
        show_axes: bool,
    ) {
        queue.write_buffer(
            &self.uniform_buffer,
            0,
            bytemuck::cast_slice(std::slice::from_ref(clip_from_world)),
        );
        encode_scene_clear(encoder, &self.scene_view, &self.depth_view);
        if show_floor || show_axes {
            encode_editor_grid(self, encoder, show_floor, show_axes);
        }
    }

    /// Resolve the multisampled scene into the texture egui samples.
    pub fn resolve(&self, encoder: &mut wgpu::CommandEncoder) {
        if self.scene_color.is_none() {
            return;
        }
        let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("viewport-resolve"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.scene_view,
                resolve_target: Some(&self.view),
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Discard,
                },
                depth_slice: None,
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
    }
}

struct SceneTargets {
    color: wgpu::Texture,
    color_view: wgpu::TextureView,
    scene_color: Option<wgpu::Texture>,
    scene_view: wgpu::TextureView,
    depth: wgpu::Texture,
    depth_view: wgpu::TextureView,
}

fn create_target(
    device: &wgpu::Device,
    width: u32,
    height: u32,
    sample_count: u32,
) -> Result<SceneTargets, RendererError> {
    if width == 0 || height == 0 {
        return Err(RendererError::ZeroViewport);
    }
    let size = wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    };
    let color = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("viewport-color"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let color_view = color.create_view(&wgpu::TextureViewDescriptor::default());
    let (scene_color, scene_view) = if sample_count > 1 {
        let scene_color = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("viewport-color-msaa"),
            size,
            mip_level_count: 1,
            sample_count,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let scene_view = scene_color.create_view(&wgpu::TextureViewDescriptor::default());
        (Some(scene_color), scene_view)
    } else {
        (
            None,
            color.create_view(&wgpu::TextureViewDescriptor::default()),
        )
    };
    let depth = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("viewport-depth"),
        size,
        mip_level_count: 1,
        sample_count,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Depth32Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let depth_view = depth.create_view(&wgpu::TextureViewDescriptor::default());
    Ok(SceneTargets {
        color,
        color_view,
        scene_color,
        scene_view,
        depth,
        depth_view,
    })
}

fn vertex_layout() -> wgpu::VertexBufferLayout<'static> {
    const ATTRIBUTES: [wgpu::VertexAttribute; 2] = [
        wgpu::VertexAttribute {
            offset: 0,
            shader_location: 0,
            format: wgpu::VertexFormat::Float32x3,
        },
        wgpu::VertexAttribute {
            offset: 12,
            shader_location: 1,
            format: wgpu::VertexFormat::Float32x3,
        },
    ];
    wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<GridVertex>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &ATTRIBUTES,
    }
}

fn encode_scene_clear(
    encoder: &mut wgpu::CommandEncoder,
    view: &wgpu::TextureView,
    depth: &wgpu::TextureView,
) {
    let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("scene-clear"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(VIEWPORT_CLEAR_COLOR),
                store: wgpu::StoreOp::Store,
            },
            depth_slice: None,
        })],
        depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
            view: depth,
            depth_ops: Some(wgpu::Operations {
                load: wgpu::LoadOp::Clear(1.0),
                store: wgpu::StoreOp::Store,
            }),
            stencil_ops: None,
        }),
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
}

/// Draw the editor grid over the cleared scene color.
fn encode_editor_grid(
    renderer: &ViewportRenderer,
    encoder: &mut wgpu::CommandEncoder,
    show_floor: bool,
    show_axes: bool,
) {
    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("editor-grid"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: &renderer.scene_view,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Load,
                store: wgpu::StoreOp::Store,
            },
            depth_slice: None,
        })],
        depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
            view: &renderer.depth_view,
            depth_ops: Some(wgpu::Operations {
                load: wgpu::LoadOp::Load,
                store: wgpu::StoreOp::Store,
            }),
            stencil_ops: None,
        }),
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    pass.set_pipeline(&renderer.pipeline);
    pass.set_bind_group(0, &renderer.bind_group, &[]);
    pass.set_vertex_buffer(0, renderer.vertex_buffer.slice(..));
    if show_floor {
        pass.draw(0..renderer.floor_count, 0..1);
    }
    if show_axes {
        let start = renderer.floor_count;
        pass.draw(start..start + renderer.axis_count, 0..1);
    }
}
