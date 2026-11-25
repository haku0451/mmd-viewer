//! Skinned model passes.
//!
//! Draw order is the shadow map, then materials in file order, then the edge
//! shell, then the ground shadow. The edge shell is an inverted hull and is
//! not MikuMikuDance's edge algorithm. The self shadow is one directional
//! depth map. The ground shadow is a projection onto Y = 0 along the light.
//! Neither shadow matches the original application exactly.
//!
//! A missing toon file is already replaced by the caller. The shader samples
//! whatever texture was uploaded.

use std::cell::RefCell;

use glam::{Mat4, Quat, Vec2, Vec3};
use mmd_core::{ModelAsset, Skinning, SphereMode, ToonSource};
use wgpu::util::DeviceExt;

use crate::error::RendererError;

const MODEL_SHADER: &str = include_str!("../../../assets/shaders/model.wgsl");
const SHADOW_SIZE: u32 = 2048;
const EDGE_SCALE: f32 = 0.02;

/// RGBA8 image supplied by the asset loader.
pub struct ImageUpload<'a> {
    pub width: u32,
    pub height: u32,
    pub rgba: &'a [u8],
    /// Mip levels after level 0. Empty means the upload builds them.
    pub extra_mips: &'a [Vec<u8>],
}

/// Per-frame values the evaluator computed.
pub struct DrawFrame<'a> {
    /// Morphed positions.
    pub positions: &'a [Vec3],
    /// Morphed UVs.
    pub uvs: &'a [Vec2],
    pub skin: &'a [Mat4],
    /// World bone rotations.
    pub rotations: &'a [Quat],
    /// World bone translations.
    pub translations: &'a [Vec3],
    /// Material colors after morphs.
    pub materials: &'a [DrawMaterial],
    /// Direction the light travels.
    pub light_direction: Vec3,
    /// Camera eye, used by the specular term.
    pub camera_position: Vec3,
    /// Viewport clip matrix from `mmd-core`.
    pub clip_from_world: Mat4,
    /// Camera view matrix. Sphere maps are looked up with the view-space
    /// normal, as in MikuMikuDance, so a reflection turns with the camera.
    pub view_from_world: Mat4,
    /// When false, no model pass is recorded.
    pub visible: bool,
    /// Self shadow: the shadow map darkens faces toward the toon's bottom
    /// color. MikuMikuDance leaves it off by default; toon shading stays
    /// either way.
    pub self_shadow: bool,
    /// The flattened shadow on the ground plane.
    pub ground_shadow: bool,
    /// Positions or UVs changed. Bone and camera data are written every frame.
    pub upload_vertices: bool,
}

/// Material parameters that can change with a morph.
#[derive(Clone, Copy, Debug)]
pub struct DrawMaterial {
    /// Diffuse color and alpha.
    pub diffuse: [f32; 4],
    pub specular: [f32; 3],
    pub specular_power: f32,
    pub ambient: [f32; 3],
    pub edge_color: [f32; 4],
    pub edge_size: f32,
    pub texture_factor: [f32; 4],
    pub sphere_factor: [f32; 4],
    pub double_sided: bool,
    pub ground_shadow: bool,
    pub cast_shadow: bool,
    pub receive_shadow: bool,
    pub draw_edge: bool,
}

/// GPU resources for one model.
pub struct SkinnedModel {
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    bone_buffer: wgpu::Buffer,
    camera_buffer: wgpu::Buffer,
    vertices: Vec<GpuVertex>,
    materials: Vec<GpuDraw>,
    frame_bind: wgpu::BindGroup,
    shadow_view: wgpu::TextureView,
    color_pipeline: [wgpu::RenderPipeline; 2],
    edge_pipeline: wgpu::RenderPipeline,
    edge_crease_pipeline: wgpu::RenderPipeline,
    ground_pipeline: wgpu::RenderPipeline,
    shadow_pipeline: wgpu::RenderPipeline,
    index_count: u32,
    bone_count: u32,
    cache: RefCell<PrepareCache>,
}

struct PrepareCache {
    bones: Vec<GpuBone>,
    light: Mat4,
    ready: bool,
}

struct GpuDraw {
    uniform: wgpu::Buffer,
    frame_group: wgpu::BindGroup,
    texture_group: wgpu::BindGroup,
    index_start: u32,
    index_count: u32,
    sphere_mode: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuVertex {
    position: [f32; 3],
    normal: [f32; 3],
    uv: [f32; 2],
    bones: [u32; 4],
    weights: [f32; 4],
    skin_kind: u32,
    sdef_c: [f32; 4],
    sdef_r0: [f32; 3],
    sdef_r1: [f32; 3],
    edge_scale: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuBone {
    skin: [[f32; 4]; 4],
    rotation: [f32; 4],
    translation: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CameraUniform {
    clip_from_world: [[f32; 4]; 4],
    light_clip: [[f32; 4]; 4],
    view_from_world: [[f32; 4]; 4],
    light_dir: [f32; 4],
    camera_pos: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MaterialUniform {
    diffuse: [f32; 4],
    specular: [f32; 4],
    ambient: [f32; 4],
    edge_color: [f32; 4],
    texture_factor: [f32; 4],
    sphere_factor: [f32; 4],
    params: [f32; 4],
}

impl SkinnedModel {
    /// Upload a model and its textures.
    pub fn upload(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        model: &ModelAsset,
        textures: &[ImageUpload<'_>],
        toons: &[ImageUpload<'_>],
        sample_count: u32,
    ) -> Result<Self, RendererError> {
        if model.vertices.is_empty() {
            return Err(RendererError::InconsistentMesh);
        }
        let vertices = pack_vertices(model)?;
        let index_count =
            u32::try_from(model.indices.len()).map_err(|_| RendererError::InconsistentMesh)?;
        let bone_count =
            u32::try_from(model.bones.len().max(1)).map_err(|_| RendererError::TooManyBones)?;
        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("model-vertices"),
            contents: bytemuck::cast_slice(&vertices),
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        });
        let indices = if model.indices.is_empty() {
            vec![0u32]
        } else {
            model.indices.clone()
        };
        let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("model-indices"),
            contents: bytemuck::cast_slice(&indices),
            usage: wgpu::BufferUsages::INDEX,
        });
        let bone_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("model-bones"),
            size: u64::from(bone_count) * std::mem::size_of::<GpuBone>() as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let camera_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("model-camera"),
            size: std::mem::size_of::<CameraUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("model-shader"),
            source: wgpu::ShaderSource::Wgsl(MODEL_SHADER.into()),
        });
        let frame_layout = bind_layout(device, "model-frame-layout", false, false);
        let material_layout = bind_layout(device, "model-material-layout", true, false);
        let texture_layout = bind_layout(device, "model-texture-layout", false, true);
        let frame_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("model-frame-bind"),
            layout: &frame_layout,
            entries: &[
                buffer_entry(0, &camera_buffer),
                buffer_entry(1, &bone_buffer),
            ],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("model-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            anisotropy_clamp: 8,
            lod_max_clamp: 0.0,
            ..Default::default()
        });
        // MMD repeats the color texture. Clamping stretches the border texel
        // across a skirt UV that sits just outside 0..1.
        let wrap_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("model-wrap-sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            address_mode_w: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            anisotropy_clamp: 8,
            lod_max_clamp: 0.0,
            ..Default::default()
        });
        let shadow_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("shadow-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            compare: Some(wgpu::CompareFunction::LessEqual),
            ..Default::default()
        });
        let white = solid_texture(device, queue, [255, 255, 255, 255]);
        let black = solid_texture(device, queue, [0, 0, 0, 255]);
        let ramp = ramp_texture(device, queue);
        let gpu_textures = upload_list(device, queue, textures);
        let gpu_toons = upload_list(device, queue, toons);
        let shadow = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("shadow-map"),
            size: wgpu::Extent3d {
                width: SHADOW_SIZE,
                height: SHADOW_SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let shadow_view = shadow.create_view(&wgpu::TextureViewDescriptor::default());
        let mut index_start = 0u32;
        let mut materials = Vec::with_capacity(model.materials.len());
        for source in &model.materials {
            let uniform = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("material-uniform"),
                size: std::mem::size_of::<MaterialUniform>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let frame_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("material-frame"),
                layout: &material_layout,
                entries: &[
                    buffer_entry(0, &camera_buffer),
                    buffer_entry(1, &bone_buffer),
                    buffer_entry(2, &uniform),
                ],
            });
            let color = texture_view(source.texture, &gpu_textures, &white);
            // A missing sphere added as white washes the whole material out.
            let sphere = texture_view(source.sphere, &gpu_textures, &black);
            let toon = match source.toon {
                ToonSource::Shared(slot) => gpu_toons
                    .get(slot as usize)
                    .map(|texture| texture.create_view(&wgpu::TextureViewDescriptor::default()))
                    .unwrap_or_else(|| ramp.create_view(&wgpu::TextureViewDescriptor::default())),
                // PMX toon index -1 means no toon: MikuMikuDance leaves the
                // material unshaded. The dark ramp turned the eyes gray.
                ToonSource::Texture(index) => texture_view(index, &gpu_textures, &white),
            };
            let texture_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("material-textures"),
                layout: &texture_layout,
                entries: &[
                    view_entry(0, &color),
                    sampler_entry(1, &wrap_sampler),
                    view_entry(2, &toon),
                    sampler_entry(3, &sampler),
                    view_entry(4, &sphere),
                    sampler_entry(5, &sampler),
                    depth_entry(6, &shadow_view),
                    sampler_entry(7, &shadow_sampler),
                ],
            });
            let count = source
                .index_count
                .min(index_count.saturating_sub(index_start));
            materials.push(GpuDraw {
                uniform,
                frame_group,
                texture_group,
                index_start,
                index_count: count,
                sphere_mode: match (source.sphere_mode, source.sphere) {
                    (SphereMode::Multiply, Some(_)) => 1.0,
                    (SphereMode::Add, Some(_)) => 2.0,
                    _ => 0.0,
                },
            });
            index_start = index_start.saturating_add(count);
        }
        let color_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("model-color-layout"),
            bind_group_layouts: &[Some(&material_layout), Some(&texture_layout)],
            immediate_size: 0,
        });
        let material_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("model-edge-layout"),
                bind_group_layouts: &[Some(&material_layout)],
                immediate_size: 0,
            });
        let frame_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("model-shadow-layout"),
                bind_group_layouts: &[Some(&frame_layout)],
                immediate_size: 0,
            });
        // MikuMikuDance draws every material in file order with alpha
        // blending and depth writes, so a material's fade blends over what
        // was drawn before it and hides what is drawn after it.
        let color_pipeline = [true, false].map(|cull| {
            model_pipeline(
                device,
                &shader,
                &color_layout,
                "vs_model",
                Some("fs_model"),
                cull,
                true,
                true,
                false,
                false,
                false,
                sample_count,
            )
        });
        let edge_pipeline = model_pipeline(
            device,
            &shader,
            &material_pipeline_layout,
            "vs_edge",
            Some("fs_edge"),
            false,
            true,
            true,
            true,
            false,
            false,
            sample_count,
        );
        let edge_crease_pipeline = model_pipeline(
            device,
            &shader,
            &material_pipeline_layout,
            "vs_edge",
            Some("fs_edge_crease"),
            false,
            true,
            true,
            true,
            false,
            true,
            sample_count,
        );
        let ground_pipeline = model_pipeline(
            device,
            &shader,
            &frame_pipeline_layout,
            "vs_ground",
            Some("fs_ground"),
            false,
            true,
            false,
            false,
            false,
            false,
            sample_count,
        );
        let shadow_pipeline = model_pipeline(
            device,
            &shader,
            &frame_pipeline_layout,
            "vs_shadow",
            None,
            true,
            false,
            true,
            false,
            true,
            false,
            1,
        );
        Ok(Self {
            vertex_buffer,
            index_buffer,
            bone_buffer,
            camera_buffer,
            vertices,
            materials,
            frame_bind,
            shadow_view,
            color_pipeline,
            edge_pipeline,
            edge_crease_pipeline,
            ground_pipeline,
            shadow_pipeline,
            index_count,
            bone_count,
            cache: RefCell::new(PrepareCache {
                bones: Vec::new(),
                light: Mat4::IDENTITY,
                ready: false,
            }),
        })
    }

    /// Write this frame's vertices, bones, and material colors.
    pub fn prepare(&self, queue: &wgpu::Queue, frame: &DrawFrame<'_>) {
        if frame.positions.len() != self.vertices.len() || frame.uvs.len() != self.vertices.len() {
            tracing::warn!("skipping a model update because the vertex count changed");
            return;
        }
        let mut cache = self.cache.borrow_mut();
        if frame.upload_vertices || !cache.ready {
            let mut vertices = self.vertices.clone();
            for (vertex, (position, uv)) in vertices
                .iter_mut()
                .zip(frame.positions.iter().zip(frame.uvs))
            {
                vertex.position = position.to_array();
                vertex.uv = uv.to_array();
            }
            queue.write_buffer(&self.vertex_buffer, 0, bytemuck::cast_slice(&vertices));
            cache.light = light_matrix(frame.positions, light_direction(frame));
            cache.ready = true;
            for (gpu, material) in self.materials.iter().zip(frame.materials) {
                let uniform = material_uniform(gpu, material);
                queue.write_buffer(&gpu.uniform, 0, bytemuck::bytes_of(&uniform));
            }
        }
        let light_clip = cache.light;
        if cache.bones.len() != self.bone_count as usize {
            cache
                .bones
                .resize(self.bone_count as usize, identity_bone());
        }
        for bone in &mut cache.bones {
            *bone = identity_bone();
        }
        for (index, bone) in cache.bones.iter_mut().enumerate().take(frame.skin.len()) {
            let rotation = frame
                .rotations
                .get(index)
                .copied()
                .unwrap_or(Quat::IDENTITY);
            let translation = frame.translations.get(index).copied().unwrap_or(Vec3::ZERO);
            bone.skin = frame.skin[index].to_cols_array_2d();
            bone.rotation = rotation.to_array();
            bone.translation = [translation.x, translation.y, translation.z, 0.0];
        }
        queue.write_buffer(&self.bone_buffer, 0, bytemuck::cast_slice(&cache.bones));
        let light = light_direction(frame);
        let camera = CameraUniform {
            clip_from_world: frame.clip_from_world.to_cols_array_2d(),
            light_clip: light_clip.to_cols_array_2d(),
            view_from_world: frame.view_from_world.to_cols_array_2d(),
            light_dir: [
                light.x,
                light.y,
                light.z,
                if frame.self_shadow { 1.0 } else { 0.0 },
            ],
            camera_pos: [
                frame.camera_position.x,
                frame.camera_position.y,
                frame.camera_position.z,
                1.0,
            ],
        };
        queue.write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(&camera));
    }

    /// Record the shadow map, materials, edge shell, and ground shadow.
    pub fn encode(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        color: &wgpu::TextureView,
        depth: &wgpu::TextureView,
        frame: &DrawFrame<'_>,
    ) {
        if !frame.visible || self.index_count == 0 {
            return;
        }
        if frame.self_shadow {
            self.encode_shadow(encoder, frame);
        }
        self.encode_materials(encoder, color, depth, frame);
        self.encode_edges(encoder, color, depth, frame);
        if frame.ground_shadow {
            self.encode_ground(encoder, color, depth, frame);
        }
    }

    fn encode_shadow(&self, encoder: &mut wgpu::CommandEncoder, frame: &DrawFrame<'_>) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("shadow-map"),
            color_attachments: &[],
            depth_stencil_attachment: Some(depth_attachment(&self.shadow_view, true)),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.shadow_pipeline);
        pass.set_bind_group(0, &self.frame_bind, &[]);
        pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
        pass.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
        for (gpu, material) in self.materials.iter().zip(frame.materials) {
            if material.cast_shadow && gpu.index_count > 0 {
                pass.draw_indexed(gpu.index_start..gpu.index_start + gpu.index_count, 0, 0..1);
            }
        }
    }

    fn encode_materials(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        color: &wgpu::TextureView,
        depth: &wgpu::TextureView,
        frame: &DrawFrame<'_>,
    ) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("model-materials"),
            color_attachments: &[Some(color_attachment(color, false))],
            depth_stencil_attachment: Some(depth_attachment(depth, false)),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
        pass.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
        for (gpu, material) in self.materials.iter().zip(frame.materials) {
            if gpu.index_count == 0 {
                continue;
            }
            pass.set_pipeline(&self.color_pipeline[usize::from(material.double_sided)]);
            pass.set_bind_group(0, &gpu.frame_group, &[]);
            pass.set_bind_group(1, &gpu.texture_group, &[]);
            pass.draw_indexed(gpu.index_start..gpu.index_start + gpu.index_count, 0, 0..1);
        }
    }

    fn encode_edges(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        color: &wgpu::TextureView,
        depth: &wgpu::TextureView,
        frame: &DrawFrame<'_>,
    ) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("model-edge"),
            color_attachments: &[Some(color_attachment(color, false))],
            depth_stencil_attachment: Some(depth_attachment(depth, false)),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
        pass.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
        for pipeline in [&self.edge_pipeline, &self.edge_crease_pipeline] {
            pass.set_pipeline(pipeline);
            for (gpu, material) in self.materials.iter().zip(frame.materials) {
                if material.draw_edge && gpu.index_count > 0 {
                    pass.set_bind_group(0, &gpu.frame_group, &[]);
                    pass.draw_indexed(gpu.index_start..gpu.index_start + gpu.index_count, 0, 0..1);
                }
            }
        }
    }

    fn encode_ground(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        color: &wgpu::TextureView,
        depth: &wgpu::TextureView,
        frame: &DrawFrame<'_>,
    ) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("ground-shadow"),
            color_attachments: &[Some(color_attachment(color, false))],
            depth_stencil_attachment: Some(depth_attachment(depth, false)),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.ground_pipeline);
        pass.set_bind_group(0, &self.frame_bind, &[]);
        pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
        pass.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
        for (gpu, material) in self.materials.iter().zip(frame.materials) {
            if material.ground_shadow && gpu.index_count > 0 {
                pass.draw_indexed(gpu.index_start..gpu.index_start + gpu.index_count, 0, 0..1);
            }
        }
    }
}

fn pack_vertices(model: &ModelAsset) -> Result<Vec<GpuVertex>, RendererError> {
    let mut vertices = Vec::with_capacity(model.vertices.len());
    for vertex in &model.vertices {
        let (bones, weights, kind, c, r0, r1) = match vertex.skin {
            Skinning::Bdef1 { bone } => (
                [u32::from(bone), 0, 0, 0],
                [1.0, 0.0, 0.0, 0.0],
                0,
                [0.0; 4],
                [0.0; 3],
                [0.0; 3],
            ),
            Skinning::Bdef2 {
                bone0,
                bone1,
                weight0,
            } => (
                [u32::from(bone0), u32::from(bone1), 0, 0],
                [weight0, 1.0 - weight0, 0.0, 0.0],
                0,
                [0.0; 4],
                [0.0; 3],
                [0.0; 3],
            ),
            Skinning::Bdef4 { bones, weights } => (
                bones.map(u32::from),
                weights,
                0,
                [0.0; 4],
                [0.0; 3],
                [0.0; 3],
            ),
            Skinning::Sdef {
                bone0,
                bone1,
                weight0,
                c,
                r0,
                r1,
            } => (
                [u32::from(bone0), u32::from(bone1), 0, 0],
                [weight0, 1.0 - weight0, 0.0, 0.0],
                1,
                [c.x, c.y, c.z, weight0],
                r0.to_array(),
                r1.to_array(),
            ),
            Skinning::Qdef { bones, weights } => (
                bones.map(u32::from),
                weights,
                2,
                [0.0; 4],
                [0.0; 3],
                [0.0; 3],
            ),
        };
        if bones.iter().any(|bone| *bone as usize >= model.bones.len()) {
            return Err(RendererError::InconsistentMesh);
        }
        vertices.push(GpuVertex {
            position: vertex.position.to_array(),
            normal: vertex.normal.to_array(),
            uv: vertex.uv.to_array(),
            bones,
            weights,
            skin_kind: kind,
            sdef_c: c,
            sdef_r0: r0,
            sdef_r1: r1,
            edge_scale: vertex.edge_scale,
        });
    }
    smooth_shared_normals(&mut vertices, &sphere_vertices(model));
    Ok(vertices)
}

/// Vertices whose material samples a sphere map. Their normals are the UV of
/// that map, so averaging them slides the eye highlight off the iris.
fn sphere_vertices(model: &ModelAsset) -> Vec<bool> {
    let mut marked = vec![false; model.vertices.len()];
    let mut start = 0usize;
    for material in &model.materials {
        let end = (start + material.index_count as usize).min(model.indices.len());
        if material.sphere_mode != SphereMode::None {
            for index in &model.indices[start..end] {
                let index = *index as usize;
                if index < marked.len() {
                    marked[index] = true;
                }
            }
        }
        start = end;
    }
    marked
}

/// Average normals that share a position and face the same way.
///
/// Opposite normals, such as the two sides of a hair card, stay separate.
/// `keep` vertices are left unchanged.
fn smooth_shared_normals(vertices: &mut [GpuVertex], keep: &[bool]) {
    use std::collections::HashMap;
    let mut groups: HashMap<(i32, i32, i32), Vec<usize>> = HashMap::new();
    for (index, vertex) in vertices.iter().enumerate() {
        let key = (
            (vertex.position[0] * 1000.0).round() as i32,
            (vertex.position[1] * 1000.0).round() as i32,
            (vertex.position[2] * 1000.0).round() as i32,
        );
        groups.entry(key).or_default().push(index);
    }
    for indices in groups.values() {
        if indices.len() < 2 {
            continue;
        }
        let mut used = vec![false; indices.len()];
        for start in 0..indices.len() {
            if used[start] || keep.get(indices[start]).copied().unwrap_or(false) {
                used[start] = true;
                continue;
            }
            let base = Vec3::from_array(vertices[indices[start]].normal);
            if base.length_squared() < 1.0e-8 {
                used[start] = true;
                continue;
            }
            let base = base.normalize();
            let mut sum = Vec3::ZERO;
            let mut members = Vec::new();
            for (offset, index) in indices.iter().enumerate() {
                if used[offset] {
                    continue;
                }
                if keep.get(*index).copied().unwrap_or(false) {
                    continue;
                }
                let normal = Vec3::from_array(vertices[*index].normal);
                if normal.length_squared() < 1.0e-8 || normal.normalize().dot(base) < 0.15 {
                    continue;
                }
                used[offset] = true;
                sum += normal.normalize();
                members.push(*index);
            }
            if members.len() < 2 || sum.length_squared() < 1.0e-8 {
                continue;
            }
            let averaged = sum.normalize().to_array();
            for index in members {
                vertices[index].normal = averaged;
            }
        }
    }
}

fn light_direction(frame: &DrawFrame<'_>) -> Vec3 {
    if frame.light_direction.length_squared() > 1.0e-8 {
        frame.light_direction.normalize()
    } else {
        Vec3::new(0.0, -1.0, 0.0)
    }
}

fn material_uniform(gpu: &GpuDraw, material: &DrawMaterial) -> MaterialUniform {
    MaterialUniform {
        diffuse: material.diffuse,
        specular: [
            material.specular[0],
            material.specular[1],
            material.specular[2],
            material.specular_power,
        ],
        ambient: [
            material.ambient[0],
            material.ambient[1],
            material.ambient[2],
            0.0,
        ],
        edge_color: material.edge_color,
        texture_factor: material.texture_factor,
        sphere_factor: material.sphere_factor,
        params: [
            gpu.sphere_mode,
            material.edge_size * EDGE_SCALE,
            if material.receive_shadow { 1.0 } else { 0.0 },
            0.0,
        ],
    }
}

fn light_matrix(positions: &[Vec3], direction: Vec3) -> Mat4 {
    let mut min = Vec3::splat(f32::MAX);
    let mut max = Vec3::splat(f32::MIN);
    if positions.is_empty() {
        min = Vec3::new(-1.0, 0.0, -1.0);
        max = Vec3::new(1.0, 2.0, 1.0);
    } else {
        for position in positions {
            min = min.min(*position);
            max = max.max(*position);
        }
    }
    let center = (min + max) * 0.5;
    let radius = (max - min).max(Vec3::splat(1.0)).length().max(1.0) * 1.25;
    let up = if direction.dot(Vec3::Y).abs() > 0.9 {
        Vec3::Z
    } else {
        Vec3::Y
    };
    let eye = center - direction * radius * 2.0;
    let view = Mat4::look_at_lh(eye, center, up);
    let projection = Mat4::orthographic_lh(-radius, radius, -radius, radius, 0.05, radius * 5.0);
    projection * view
}

fn identity_bone() -> GpuBone {
    GpuBone {
        skin: Mat4::IDENTITY.to_cols_array_2d(),
        rotation: [0.0, 0.0, 0.0, 1.0],
        translation: [0.0, 0.0, 0.0, 0.0],
    }
}

fn bind_layout(
    device: &wgpu::Device,
    label: &str,
    material: bool,
    textures: bool,
) -> wgpu::BindGroupLayout {
    let mut entries = Vec::new();
    if !textures {
        entries.push(uniform_binding(
            0,
            wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
        ));
        entries.push(storage_binding(1, wgpu::ShaderStages::VERTEX));
        if material {
            entries.push(uniform_binding(
                2,
                wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
            ));
        }
    } else {
        for binding in [0, 2, 4] {
            entries.push(wgpu::BindGroupLayoutEntry {
                binding,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            });
        }
        for binding in [1, 3, 5] {
            entries.push(sampler_binding(binding, false));
        }
        entries.push(wgpu::BindGroupLayoutEntry {
            binding: 6,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Depth,
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        });
        entries.push(sampler_binding(7, true));
    }
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some(label),
        entries: &entries,
    })
}

fn uniform_binding(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn storage_binding(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: true },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn sampler_binding(binding: u32, comparison: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(if comparison {
            wgpu::SamplerBindingType::Comparison
        } else {
            wgpu::SamplerBindingType::Filtering
        }),
        count: None,
    }
}

fn buffer_entry(binding: u32, buffer: &wgpu::Buffer) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry {
        binding,
        resource: buffer.as_entire_binding(),
    }
}

fn view_entry(binding: u32, view: &wgpu::TextureView) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry {
        binding,
        resource: wgpu::BindingResource::TextureView(view),
    }
}

fn depth_entry(binding: u32, view: &wgpu::TextureView) -> wgpu::BindGroupEntry<'_> {
    view_entry(binding, view)
}

fn sampler_entry(binding: u32, sampler: &wgpu::Sampler) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry {
        binding,
        resource: wgpu::BindingResource::Sampler(sampler),
    }
}

/// Color blends over what is drawn; alpha accumulates toward 1.
///
/// The viewport texture is composited by egui with its alpha, so a
/// translucent fade must not leave a hole that shows the UI behind it.
const MODEL_BLEND: wgpu::BlendState = wgpu::BlendState {
    color: wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::SrcAlpha,
        dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
        operation: wgpu::BlendOperation::Add,
    },
    alpha: wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::One,
        dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
        operation: wgpu::BlendOperation::Add,
    },
};

#[allow(clippy::too_many_arguments)] // Pipeline variants differ by entry point, culling, blend, and depth.
fn model_pipeline(
    device: &wgpu::Device,
    shader: &wgpu::ShaderModule,
    layout: &wgpu::PipelineLayout,
    vertex_entry: &str,
    fragment_entry: Option<&str>,
    cull: bool,
    blend: bool,
    depth_write: bool,
    edge: bool,
    shadow_pass: bool,
    cover_surface: bool,
    sample_count: u32,
) -> wgpu::RenderPipeline {
    let targets = if fragment_entry.is_some() {
        vec![Some(wgpu::ColorTargetState {
            format: wgpu::TextureFormat::Rgba8Unorm,
            blend: if blend {
                Some(MODEL_BLEND)
            } else {
                Some(wgpu::BlendState::REPLACE)
            },
            write_mask: wgpu::ColorWrites::ALL,
        })]
    } else {
        Vec::new()
    };
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(vertex_entry),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some(vertex_entry),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[Some(vertex_layout())],
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Cw,
            cull_mode: if edge {
                Some(wgpu::Face::Front)
            } else if cull {
                Some(wgpu::Face::Back)
            } else {
                None
            },
            unclipped_depth: false,
            polygon_mode: wgpu::PolygonMode::Fill,
            conservative: false,
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth32Float,
            depth_write_enabled: Some(depth_write),
            depth_compare: Some(wgpu::CompareFunction::LessEqual),
            stencil: wgpu::StencilState::default(),
            bias: if shadow_pass {
                wgpu::DepthBiasState {
                    constant: 4,
                    slope_scale: 2.0,
                    clamp: 0.0,
                }
            } else if edge && cover_surface {
                wgpu::DepthBiasState {
                    constant: -2,
                    slope_scale: -1.0,
                    clamp: 0.0,
                }
            } else if edge {
                wgpu::DepthBiasState {
                    constant: 2,
                    slope_scale: 1.0,
                    clamp: 0.0,
                }
            } else {
                wgpu::DepthBiasState::default()
            },
        }),
        multisample: wgpu::MultisampleState {
            count: sample_count,
            mask: !0,
            alpha_to_coverage_enabled: false,
        },
        fragment: fragment_entry.map(|entry| wgpu::FragmentState {
            module: shader,
            entry_point: Some(entry),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &targets,
        }),
        multiview_mask: None,
        cache: None,
    })
}

fn vertex_layout() -> wgpu::VertexBufferLayout<'static> {
    const ATTRIBUTES: [wgpu::VertexAttribute; 10] = [
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
        wgpu::VertexAttribute {
            offset: 24,
            shader_location: 2,
            format: wgpu::VertexFormat::Float32x2,
        },
        wgpu::VertexAttribute {
            offset: 32,
            shader_location: 3,
            format: wgpu::VertexFormat::Uint32x4,
        },
        wgpu::VertexAttribute {
            offset: 48,
            shader_location: 4,
            format: wgpu::VertexFormat::Float32x4,
        },
        wgpu::VertexAttribute {
            offset: 64,
            shader_location: 5,
            format: wgpu::VertexFormat::Uint32,
        },
        wgpu::VertexAttribute {
            offset: 68,
            shader_location: 6,
            format: wgpu::VertexFormat::Float32x4,
        },
        wgpu::VertexAttribute {
            offset: 84,
            shader_location: 7,
            format: wgpu::VertexFormat::Float32x3,
        },
        wgpu::VertexAttribute {
            offset: 96,
            shader_location: 8,
            format: wgpu::VertexFormat::Float32x3,
        },
        wgpu::VertexAttribute {
            offset: 108,
            shader_location: 9,
            format: wgpu::VertexFormat::Float32,
        },
    ];
    wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<GpuVertex>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &ATTRIBUTES,
    }
}

fn color_attachment(view: &wgpu::TextureView, clear: bool) -> wgpu::RenderPassColorAttachment<'_> {
    wgpu::RenderPassColorAttachment {
        view,
        resolve_target: None,
        ops: wgpu::Operations {
            load: if clear {
                wgpu::LoadOp::Clear(crate::viewport::VIEWPORT_CLEAR_COLOR)
            } else {
                wgpu::LoadOp::Load
            },
            store: wgpu::StoreOp::Store,
        },
        depth_slice: None,
    }
}

fn depth_attachment(
    view: &wgpu::TextureView,
    clear: bool,
) -> wgpu::RenderPassDepthStencilAttachment<'_> {
    wgpu::RenderPassDepthStencilAttachment {
        view,
        depth_ops: Some(wgpu::Operations {
            load: if clear {
                wgpu::LoadOp::Clear(1.0)
            } else {
                wgpu::LoadOp::Load
            },
            store: wgpu::StoreOp::Store,
        }),
        stencil_ops: None,
    }
}

fn texture_view(
    index: Option<u32>,
    textures: &[wgpu::Texture],
    fallback: &wgpu::Texture,
) -> wgpu::TextureView {
    index
        .and_then(|index| textures.get(index as usize))
        .unwrap_or(fallback)
        .create_view(&wgpu::TextureViewDescriptor::default())
}

fn upload_list(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    images: &[ImageUpload<'_>],
) -> Vec<wgpu::Texture> {
    images
        .iter()
        .map(|image| upload_rgba(device, queue, image, "model-texture"))
        .collect()
}

fn solid_texture(device: &wgpu::Device, queue: &wgpu::Queue, rgba: [u8; 4]) -> wgpu::Texture {
    let image = ImageUpload {
        width: 1,
        height: 1,
        rgba: &rgba,
        extra_mips: &[],
    };
    upload_rgba(device, queue, &image, "white-texture")
}

fn ramp_texture(device: &wgpu::Device, queue: &wgpu::Queue) -> wgpu::Texture {
    let mut rgba = Vec::with_capacity(256 * 4);
    for y in 0..=255u8 {
        let value = 255 - y;
        rgba.extend([value, value, value, 255]);
    }
    let image = ImageUpload {
        width: 1,
        height: 256,
        rgba: &rgba,
        extra_mips: &[],
    };
    upload_rgba(device, queue, &image, "toon-ramp")
}

fn mip_level_count(width: u32, height: u32) -> u32 {
    let largest = width.max(height).max(1);
    u32::BITS - largest.leading_zeros()
}

fn downsample_box(src: &[u8], width: u32, height: u32) -> Vec<u8> {
    let dst_width = (width / 2).max(1);
    let dst_height = (height / 2).max(1);
    let mut dst = vec![0u8; (dst_width * dst_height * 4) as usize];
    for y in 0..dst_height {
        for x in 0..dst_width {
            let mut sum = [0u32; 4];
            let mut count = 0u32;
            for offset_y in 0..2 {
                for offset_x in 0..2 {
                    let sample_x = (x * 2 + offset_x).min(width - 1);
                    let sample_y = (y * 2 + offset_y).min(height - 1);
                    let index = ((sample_y * width + sample_x) * 4) as usize;
                    if index + 3 < src.len() {
                        for channel in 0..4 {
                            sum[channel] += u32::from(src[index + channel]);
                        }
                        count += 1;
                    }
                }
            }
            if count == 0 {
                continue;
            }
            let index = ((y * dst_width + x) * 4) as usize;
            for channel in 0..4 {
                dst[index + channel] = (sum[channel] / count) as u8;
            }
        }
    }
    dst
}

fn upload_rgba(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    image: &ImageUpload<'_>,
    label: &str,
) -> wgpu::Texture {
    let width = image.width.max(1);
    let height = image.height.max(1);
    let levels = mip_level_count(width, height);
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: levels,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let prepared = prepared_mips(image.extra_mips, width, height, levels);
    if let Some(mips) = prepared {
        let mut level_width = width;
        let mut level_height = height;
        write_mip(queue, &texture, 0, level_width, level_height, image.rgba);
        for (mip, pixels) in mips.iter().enumerate() {
            level_width = (level_width / 2).max(1);
            level_height = (level_height / 2).max(1);
            write_mip(
                queue,
                &texture,
                (mip + 1) as u32,
                level_width,
                level_height,
                pixels,
            );
        }
        return texture;
    }
    let mut level = tight_rgba(image.rgba, width, height).into_owned();
    let mut level_width = width;
    let mut level_height = height;
    for mip in 0..levels {
        write_mip(queue, &texture, mip, level_width, level_height, &level);
        if mip + 1 == levels {
            break;
        }
        level = downsample_box(&level, level_width, level_height);
        level_width = (level_width / 2).max(1);
        level_height = (level_height / 2).max(1);
    }
    texture
}

fn prepared_mips(mips: &[Vec<u8>], width: u32, height: u32, levels: u32) -> Option<&[Vec<u8>]> {
    if mips.len() + 1 != levels as usize {
        return None;
    }
    let mut level_width = width;
    let mut level_height = height;
    for pixels in mips {
        level_width = (level_width / 2).max(1);
        level_height = (level_height / 2).max(1);
        let expected = (level_width as usize)
            .saturating_mul(level_height as usize)
            .saturating_mul(4);
        if pixels.len() != expected {
            return None;
        }
    }
    Some(mips)
}

/// The pixels as exactly `width * height` texels, borrowed when there are enough.
fn tight_rgba(rgba: &[u8], width: u32, height: u32) -> std::borrow::Cow<'_, [u8]> {
    let needed = (width as usize)
        .saturating_mul(height as usize)
        .saturating_mul(4);
    if rgba.len() >= needed {
        std::borrow::Cow::Borrowed(&rgba[..needed])
    } else {
        let mut pixels = vec![0u8; needed];
        pixels[..rgba.len()].copy_from_slice(rgba);
        std::borrow::Cow::Owned(pixels)
    }
}

/// Write one mip level. `Queue::write_texture` takes tightly packed rows;
/// only buffer-to-texture copies need 256-byte row alignment.
fn write_mip(
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    mip: u32,
    width: u32,
    height: u32,
    rgba: &[u8],
) {
    let tight = tight_rgba(rgba, width, height);
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: mip,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &tight,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(width.saturating_mul(4)),
            rows_per_image: Some(height),
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vertex_stride_matches_the_attribute_layout() {
        assert_eq!(std::mem::size_of::<GpuVertex>(), 112);
    }

    #[test]
    fn box_downsample_averages_a_two_by_two_block() {
        let src = [0, 0, 0, 255, 100, 0, 0, 255, 0, 40, 0, 255, 0, 0, 80, 255];
        let dst = downsample_box(&src, 2, 2);
        assert_eq!(dst, vec![25, 10, 20, 255]);
        assert_eq!(mip_level_count(1, 1), 1);
        assert_eq!(mip_level_count(2048, 1024), 12);
    }

    #[test]
    fn model_shader_is_valid_wgsl() {
        let module = naga::front::wgsl::parse_str(MODEL_SHADER).expect("parse model shader");
        let mut validator = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        );
        validator.validate(&module).expect("validate model shader");
    }
}
