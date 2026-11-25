//! PMD reader.
//!
//! Strings are fixed-width Shift-JIS. A knee bone whose name contains `ひざ`
//! receives an X-axis hinge of -π to 0. That constraint is the conventional
//! PMD import rule used here.
//!
//! PMD twist and axis-restricted bone kinds do not store an axis vector. Those
//! bones stay ordinary rotating bones so a guessed axis does not change the pose.

use mmd_core::{
    Bone, BoneTail, Ik, IkLink, Joint, Material, ModelAsset, Morph, RigidBody, Skinning,
    SphereMode, ToonSource, Vertex,
};

use crate::error::FormatError;
use crate::reader::Reader;
use crate::text::decode_shift_jis;

const MAX_VERTICES: u32 = 2_000_000;
const MAX_INDICES: u32 = 6_000_000;
const MAX_BONES: u32 = 16_384;
const MAX_MATERIALS: u32 = 65_536;

/// Parse a PMD file.
pub fn parse_pmd(bytes: &[u8]) -> Result<ModelAsset, FormatError> {
    let mut reader = Reader::new(bytes);
    let magic = reader.read_exact(3, "PMD magic")?;
    if magic != b"Pmd" {
        return Err(FormatError::BadMagic);
    }
    let version = reader.f32("PMD version")?;
    if (version - 1.0).abs() > 0.001 {
        return Err(FormatError::UnsupportedVersion {
            kind: "PMD",
            version: format!("{version}"),
        });
    }
    let name = decode_shift_jis(reader.read_exact(20, "model name")?);
    let comment = decode_shift_jis(reader.read_exact(256, "comment")?);
    let vertices = read_vertices(&mut reader)?;
    let indices = read_indices(&mut reader, vertices.len())?;
    let (materials, textures) = read_materials(&mut reader, indices.len())?;
    let mut bones = read_bones(&mut reader)?;
    validate_vertex_bones(&vertices, bones.len())?;
    read_ik(&mut reader, &mut bones)?;
    let morphs = read_morphs(&mut reader, vertices.len())?;
    skip_display(&mut reader, bones.len(), morphs.len())?;
    let toon_names = read_toon_names(&mut reader)?;
    let rigid_bodies = read_rigid_bodies(&mut reader, bones.len())?;
    let joints = read_joints(&mut reader, rigid_bodies.len())?;
    let _ = toon_names;

    Ok(ModelAsset {
        name,
        english_name: String::new(),
        comment,
        vertices,
        indices,
        textures,
        materials,
        bones,
        morphs,
        display_frames: Vec::new(),
        rigid_bodies,
        joints,
    })
}

fn read_vertices(reader: &mut Reader<'_>) -> Result<Vec<Vertex>, FormatError> {
    let count = reader.count("vertices", MAX_VERTICES, 38)?;
    let mut vertices = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let position = reader.vec3("vertex position")?;
        let normal = reader.vec3("vertex normal")?;
        let uv = glam::Vec2::new(reader.f32("vertex uv")?, reader.f32("vertex uv")?);
        let bone0 = reader.u16("vertex bone")?;
        let bone1 = reader.u16("vertex bone")?;
        let weight = f32::from(reader.u8("vertex weight")?) / 100.0;
        let edge = reader.u8("vertex edge")?;
        let skin = if (weight - 1.0).abs() < 0.0001 || bone0 == bone1 {
            Skinning::Bdef1 { bone: bone0 }
        } else {
            Skinning::Bdef2 {
                bone0,
                bone1,
                weight0: weight,
            }
        };
        vertices.push(Vertex {
            position,
            normal,
            uv,
            skin,
            edge_scale: if edge == 0 { 0.0 } else { 1.0 },
        });
    }
    Ok(vertices)
}

fn validate_vertex_bones(vertices: &[Vertex], bone_count: usize) -> Result<(), FormatError> {
    for vertex in vertices {
        let bones = match vertex.skin {
            Skinning::Bdef1 { bone } => [bone, bone],
            Skinning::Bdef2 { bone0, bone1, .. } => [bone0, bone1],
            _ => [0, 0],
        };
        if bone_count == 0 || bones[0] as usize >= bone_count || bones[1] as usize >= bone_count {
            return Err(FormatError::BadIndex {
                context: "vertex bone",
                index: i64::from(bones[0]),
            });
        }
    }
    Ok(())
}

fn read_indices(reader: &mut Reader<'_>, vertex_count: usize) -> Result<Vec<u32>, FormatError> {
    let count = reader.count("indices", MAX_INDICES, 2)?;
    let mut indices = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let index = u32::from(reader.u16("face index")?);
        if index as usize >= vertex_count {
            return Err(FormatError::BadIndex {
                context: "face",
                index: i64::from(index),
            });
        }
        indices.push(index);
    }
    Ok(indices)
}

fn read_materials(
    reader: &mut Reader<'_>,
    index_count: usize,
) -> Result<(Vec<Material>, Vec<String>), FormatError> {
    let count = reader.count("materials", MAX_MATERIALS, 70)?;
    let mut materials = Vec::with_capacity(count as usize);
    let mut textures = Vec::new();
    let mut consumed = 0u64;
    for _ in 0..count {
        let diffuse = reader.vec4("diffuse")?;
        let power = reader.f32("specular power")?;
        let specular = reader.vec3("specular")?;
        let ambient = reader.vec3("ambient")?;
        let toon = reader.u8("toon index")?;
        let edge = reader.u8("edge flag")?;
        let faces = reader.u32("material index count")?;
        let texture_name = decode_shift_jis(reader.read_exact(20, "texture name")?);
        consumed += u64::from(faces);
        if consumed > index_count as u64 {
            return Err(FormatError::Invalid {
                context: "material index count",
                detail: "materials consume more indices than the index buffer holds",
            });
        }
        let (texture, sphere) = split_texture(&texture_name, &mut textures);
        let toon = if toon == 0xFF {
            ToonSource::Texture(None)
        } else if toon <= 9 {
            ToonSource::Shared(toon)
        } else {
            return Err(FormatError::BadIndex {
                context: "toon",
                index: i64::from(toon),
            });
        };
        let mut flags = 0x04 | 0x08;
        if edge != 0 {
            flags |= 0x10 | 0x02;
        }
        materials.push(Material {
            name: String::new(),
            diffuse: diffuse.to_array(),
            specular: specular.to_array(),
            specular_power: power,
            ambient: ambient.to_array(),
            flags,
            edge_color: [0.0, 0.0, 0.0, 1.0],
            edge_size: if edge == 0 { 0.0 } else { 1.0 },
            texture,
            sphere,
            sphere_mode: if sphere.is_some() {
                SphereMode::Multiply
            } else {
                SphereMode::None
            },
            toon,
            index_count: faces,
        });
    }
    Ok((materials, textures))
}

fn split_texture(name: &str, textures: &mut Vec<String>) -> (Option<u32>, Option<u32>) {
    if name.is_empty() {
        return (None, None);
    }
    let mut parts = name.splitn(2, '*');
    let color = parts.next().unwrap_or("");
    let sphere = parts.next().unwrap_or("");
    let color_index = if color.is_empty() {
        None
    } else {
        let index = textures.len() as u32;
        textures.push(color.to_string());
        Some(index)
    };
    let sphere_index = if sphere.is_empty() {
        None
    } else {
        let index = textures.len() as u32;
        textures.push(sphere.to_string());
        Some(index)
    };
    (color_index, sphere_index)
}

fn read_bones(reader: &mut Reader<'_>) -> Result<Vec<Bone>, FormatError> {
    let count = u32::from(reader.u16("bone count")?);
    reader.ensure_count("bones", count, MAX_BONES, 39)?;
    let mut bones = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let name = decode_shift_jis(reader.read_exact(20, "bone name")?);
        let parent_raw = reader.u16("parent bone")?;
        let tail_raw = reader.u16("tail bone")?;
        let kind = reader.u8("bone kind")?;
        let _ik = reader.u16("bone ik")?;
        let position = reader.vec3("bone position")?;
        let parent = if parent_raw == u16::MAX {
            None
        } else {
            Some(parent_raw)
        };
        let tail = if tail_raw == u16::MAX {
            BoneTail::Offset(glam::Vec3::ZERO)
        } else {
            BoneTail::Bone(tail_raw)
        };
        let fixed_axis = None;
        bones.push(Bone {
            name,
            english_name: String::new(),
            position,
            parent,
            layer: 0,
            flags: bone_flags(kind),
            tail,
            append: None,
            fixed_axis,
            local_axis: None,
            external_parent: None,
            ik: None,
        });
    }
    for bone in &bones {
        if let Some(parent) = bone.parent
            && parent as usize >= bones.len()
        {
            return Err(FormatError::BadIndex {
                context: "parent bone",
                index: i64::from(parent),
            });
        }
    }
    Ok(bones)
}

fn bone_flags(kind: u8) -> u16 {
    match kind {
        1 => 0x0002 | 0x0004 | 0x0008 | 0x0010,
        2 | 4 | 5 | 8 | 9 => 0x0002 | 0x0008,
        _ => 0x0002 | 0x0008 | 0x0010,
    }
}

fn read_ik(reader: &mut Reader<'_>, bones: &mut [Bone]) -> Result<(), FormatError> {
    let count = u32::from(reader.u16("ik count")?);
    reader.ensure_count("ik", count, MAX_BONES, 11)?;
    for _ in 0..count {
        let bone_index = reader.u16("ik bone")?;
        let target = reader.u16("ik target")?;
        let chain = reader.u8("ik chain")?;
        let iterations = reader.u16("ik iterations")?;
        let limit_angle = reader.f32("ik limit")?;
        if bone_index as usize >= bones.len() || target as usize >= bones.len() {
            return Err(FormatError::BadIndex {
                context: "ik bone",
                index: i64::from(bone_index),
            });
        }
        let mut links = Vec::with_capacity(chain as usize);
        for _ in 0..chain {
            let link = reader.u16("ik link")?;
            if link as usize >= bones.len() {
                return Err(FormatError::BadIndex {
                    context: "ik link",
                    index: i64::from(link),
                });
            }
            let limits = if bones[link as usize].name.contains("ひざ") {
                Some((
                    glam::Vec3::new(-std::f32::consts::PI, 0.0, 0.0),
                    glam::Vec3::ZERO,
                ))
            } else {
                None
            };
            links.push(IkLink { bone: link, limits });
        }
        bones[bone_index as usize].flags |= 0x0020;
        bones[bone_index as usize].ik = Some(Ik {
            target,
            iterations: u32::from(iterations),
            limit_angle,
            links,
        });
    }
    Ok(())
}

fn read_morphs(reader: &mut Reader<'_>, vertex_count: usize) -> Result<Vec<Morph>, FormatError> {
    let count = u32::from(reader.u16("morph count")?);
    reader.ensure_count("morphs", count, 65_536, 25)?;
    let mut morphs = Vec::with_capacity(count as usize);
    for index in 0..count {
        let name = decode_shift_jis(reader.read_exact(20, "morph name")?);
        let offset_count = reader.u32("morph vertices")?;
        let category = reader.u8("morph category")?;
        reader.ensure_count("morph vertices", offset_count, MAX_VERTICES, 16)?;
        let mut offsets = Vec::with_capacity(offset_count as usize);
        for _ in 0..offset_count {
            let vertex = reader.u32("morph vertex")?;
            let offset = reader.vec3("morph offset")?;
            if vertex as usize >= vertex_count {
                return Err(FormatError::BadIndex {
                    context: "morph vertex",
                    index: i64::from(vertex),
                });
            }
            if index != 0 {
                offsets.push((vertex, offset));
            }
        }
        if index == 0 {
            continue;
        }
        morphs.push(Morph::Vertex {
            name,
            english_name: String::new(),
            panel: category,
            offsets,
        });
    }
    Ok(morphs)
}

fn skip_display(
    reader: &mut Reader<'_>,
    bone_count: usize,
    morph_count: usize,
) -> Result<(), FormatError> {
    let morph_display = u32::from(reader.u8("morph display count")?);
    reader.ensure_count("morph display", morph_display, 65_536, 2)?;
    for _ in 0..morph_display {
        let index = reader.u16("morph display")?;
        if index as usize >= morph_count.saturating_add(1) {
            return Err(FormatError::BadIndex {
                context: "morph display",
                index: i64::from(index),
            });
        }
    }
    let frames = u32::from(reader.u8("bone frame count")?);
    reader.ensure_count("bone frames", frames, 256, 50)?;
    let _ = reader.read_exact(frames as usize * 50, "bone frame names")?;
    let displayed = reader.u32("bone display count")?;
    reader.ensure_count("bone display", displayed, MAX_BONES, 3)?;
    for _ in 0..displayed {
        let bone = reader.u16("displayed bone")?;
        let _frame = reader.u8("display frame")?;
        if bone as usize >= bone_count {
            return Err(FormatError::BadIndex {
                context: "displayed bone",
                index: i64::from(bone),
            });
        }
    }
    let english = reader.u8("english flag")?;
    if english > 1 {
        return Err(FormatError::Invalid {
            context: "english flag",
            detail: "the english block flag must be 0 or 1",
        });
    }
    if english == 1 {
        let _ = reader.read_exact(20 + 256, "english header")?;
        reader.ensure_count("english bones", bone_count as u32, MAX_BONES, 20)?;
        let _ = reader.read_exact(bone_count * 20, "english bone names")?;
        let morph_names = morph_count.saturating_sub(0) as u32;
        reader.ensure_count("english morphs", morph_names, 65_536, 20)?;
        let _ = reader.read_exact(morph_count * 20, "english morph names")?;
        reader.ensure_count("english frames", frames, 256, 50)?;
        let _ = reader.read_exact(frames as usize * 50, "english frame names")?;
    }
    Ok(())
}

fn read_toon_names(reader: &mut Reader<'_>) -> Result<Vec<String>, FormatError> {
    let mut names = Vec::with_capacity(10);
    for _ in 0..10 {
        names.push(decode_shift_jis(reader.read_exact(100, "toon name")?));
    }
    Ok(names)
}

fn read_rigid_bodies(
    reader: &mut Reader<'_>,
    bone_count: usize,
) -> Result<Vec<RigidBody>, FormatError> {
    let count = reader.count("rigid bodies", 65_536, 83)?;
    let mut bodies = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let name = decode_shift_jis(reader.read_exact(20, "rigid name")?);
        let bone_raw = reader.u16("rigid bone")?;
        let bone = if bone_raw == u16::MAX || bone_raw as usize >= bone_count {
            if bone_raw != u16::MAX && bone_raw as usize >= bone_count {
                return Err(FormatError::BadIndex {
                    context: "rigid bone",
                    index: i64::from(bone_raw),
                });
            }
            None
        } else {
            Some(bone_raw)
        };
        bodies.push(RigidBody {
            name,
            bone,
            group: reader.u8("rigid group")?,
            mask: reader.u16("rigid mask")?,
            shape: reader.u8("rigid shape")?,
            size: reader.vec3("rigid size")?,
            position: reader.vec3("rigid position")?,
            rotation: reader.vec3("rigid rotation")?,
            mass: reader.f32("rigid mass")?,
            linear_damping: reader.f32("rigid linear damping")?,
            angular_damping: reader.f32("rigid angular damping")?,
            restitution: reader.f32("rigid restitution")?,
            friction: reader.f32("rigid friction")?,
            body_type: reader.u8("rigid type")?,
        });
    }
    Ok(bodies)
}

fn read_joints(reader: &mut Reader<'_>, rigid_count: usize) -> Result<Vec<Joint>, FormatError> {
    let count = reader.count("joints", 65_536, 125)?;
    let mut joints = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let name = decode_shift_jis(reader.read_exact(20, "joint name")?);
        let kind = reader.u8("joint kind")?;
        let rigid_a = optional_rigid(reader.u32("joint rigid")?, rigid_count)?;
        let rigid_b = optional_rigid(reader.u32("joint rigid")?, rigid_count)?;
        joints.push(Joint {
            name,
            kind,
            rigid_a,
            rigid_b,
            position: reader.vec3("joint position")?,
            rotation: reader.vec3("joint rotation")?,
            position_min: reader.vec3("joint position min")?,
            position_max: reader.vec3("joint position max")?,
            rotation_min: reader.vec3("joint rotation min")?,
            rotation_max: reader.vec3("joint rotation max")?,
            spring_position: reader.vec3("joint spring position")?,
            spring_rotation: reader.vec3("joint spring rotation")?,
        });
    }
    Ok(joints)
}

fn optional_rigid(index: u32, count: usize) -> Result<Option<u32>, FormatError> {
    if index == u32::MAX {
        return Ok(None);
    }
    if index as usize >= count {
        return Err(FormatError::BadIndex {
            context: "joint rigid",
            index: i64::from(index),
        });
    }
    Ok(Some(index))
}
