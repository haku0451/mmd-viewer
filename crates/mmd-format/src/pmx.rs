//! PMX 2.0 and 2.1 reader.
//!
//! Index widths come from the header. Vertex indices are unsigned. Bone,
//! material, morph, and rigid-body indices are signed, and -1 means "none".

use mmd_core::{
    Append, Bone, BoneTail, DisplayFrame, Ik, IkLink, Joint, LocalAxis, Material, MaterialOffset,
    ModelAsset, Morph, RigidBody, Skinning, SphereMode, ToonSource, UvChannel, Vertex,
};

use crate::error::FormatError;
use crate::reader::Reader;
use crate::text::read_pmx_text;

const MAX_VERTICES: u32 = 2_000_000;
const MAX_INDICES: u32 = 6_000_000;
const MAX_BONES: u32 = 16_384;
const MAX_MATERIALS: u32 = 65_536;
const MAX_MORPHS: u32 = 65_536;
const MAX_TEXTURES: u32 = 65_536;
const MAX_GENERIC: u32 = 65_536;

struct Header {
    utf8: bool,
    additional_uv: u8,
    vertex_size: u8,
    texture_size: u8,
    material_size: u8,
    bone_size: u8,
    morph_size: u8,
    rigid_size: u8,
}

/// Parse a PMX 2.0 or 2.1 file.
pub fn parse_pmx(bytes: &[u8]) -> Result<ModelAsset, FormatError> {
    let mut reader = Reader::new(bytes);
    let header = read_header(&mut reader)?;
    let name = read_pmx_text(&mut reader, header.utf8, "model name")?;
    let english_name = read_pmx_text(&mut reader, header.utf8, "english model name")?;
    let comment = read_pmx_text(&mut reader, header.utf8, "comment")?;
    let _english_comment = read_pmx_text(&mut reader, header.utf8, "english comment")?;

    let vertices = read_vertices(&mut reader, &header)?;
    let indices = read_indices(&mut reader, &header, vertices.len())?;
    let textures = read_textures(&mut reader, &header)?;
    let materials = read_materials(&mut reader, &header, textures.len(), indices.len())?;
    let bones = read_bones(&mut reader, &header)?;
    validate_vertices(&vertices, bones.len())?;
    validate_bone_links(&bones)?;
    let morphs = read_morphs(
        &mut reader,
        &header,
        vertices.len(),
        bones.len(),
        materials.len(),
    )?;
    validate_morphs(&morphs)?;
    let display_frames = read_display(&mut reader, &header, bones.len(), morphs.len())?;
    let rigid_bodies = read_rigid_bodies(&mut reader, &header, bones.len())?;
    let joints = read_joints(&mut reader, &header, rigid_bodies.len())?;
    Ok(ModelAsset {
        name,
        english_name,
        comment,
        vertices,
        indices,
        textures,
        materials,
        bones,
        morphs,
        display_frames,
        rigid_bodies,
        joints,
    })
}

fn read_header(reader: &mut Reader<'_>) -> Result<Header, FormatError> {
    let magic = reader.read_exact(4, "PMX magic")?;
    if magic != b"PMX " {
        return Err(FormatError::BadMagic);
    }
    let version = reader.f32("PMX version")?;
    if (version - 2.0).abs() > 0.001 && (version - 2.1).abs() > 0.001 {
        return Err(FormatError::UnsupportedVersion {
            kind: "PMX",
            version: format!("{version}"),
        });
    }
    let globals = reader.u8("PMX globals length")?;
    if globals != 8 {
        return Err(FormatError::Invalid {
            context: "PMX globals",
            detail: "the globals block must be 8 bytes",
        });
    }
    let header = Header {
        utf8: match reader.u8("text encoding")? {
            0 => false,
            1 => true,
            _ => {
                return Err(FormatError::Invalid {
                    context: "text encoding",
                    detail: "the encoding byte must be 0 or 1",
                });
            }
        },
        additional_uv: reader.u8("additional uv count")?,
        vertex_size: reader.u8("vertex index size")?,
        texture_size: reader.u8("texture index size")?,
        material_size: reader.u8("material index size")?,
        bone_size: reader.u8("bone index size")?,
        morph_size: reader.u8("morph index size")?,
        rigid_size: reader.u8("rigid index size")?,
    };
    if header.additional_uv > 4 {
        return Err(FormatError::Invalid {
            context: "additional uv count",
            detail: "PMX allows at most 4 additional UV channels",
        });
    }
    for size in [
        header.vertex_size,
        header.texture_size,
        header.material_size,
        header.bone_size,
        header.morph_size,
        header.rigid_size,
    ] {
        if !matches!(size, 1 | 2 | 4) {
            return Err(FormatError::BadIndexSize(size));
        }
    }
    Ok(header)
}

fn read_signed(
    reader: &mut Reader<'_>,
    size: u8,
    context: &'static str,
) -> Result<i32, FormatError> {
    match size {
        1 => Ok(reader.u8(context)? as i8 as i32),
        2 => Ok(reader.u16(context)? as i16 as i32),
        4 => reader.i32(context),
        _ => Err(FormatError::BadIndexSize(size)),
    }
}

fn read_unsigned(
    reader: &mut Reader<'_>,
    size: u8,
    context: &'static str,
) -> Result<u32, FormatError> {
    match size {
        1 => Ok(u32::from(reader.u8(context)?)),
        2 => Ok(u32::from(reader.u16(context)?)),
        4 => reader.u32(context),
        _ => Err(FormatError::BadIndexSize(size)),
    }
}

fn optional_index(index: i32) -> Option<u32> {
    if index < 0 { None } else { Some(index as u32) }
}

fn read_vertices(reader: &mut Reader<'_>, header: &Header) -> Result<Vec<Vertex>, FormatError> {
    let stride = 38 + header.additional_uv as usize * 16;
    let count = reader.count("vertices", MAX_VERTICES, stride)?;
    let mut vertices = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let position = reader.vec3("vertex position")?;
        let normal = reader.vec3("vertex normal")?;
        let uv = glam::Vec2::new(reader.f32("vertex uv")?, reader.f32("vertex uv")?);
        for _ in 0..header.additional_uv {
            let _ = reader.vec4("additional uv")?;
        }
        let skin = read_skin(reader, header.bone_size)?;
        let edge_scale = reader.f32("edge scale")?;
        vertices.push(Vertex {
            position,
            normal,
            uv,
            skin,
            edge_scale,
        });
    }
    Ok(vertices)
}

fn read_skin(reader: &mut Reader<'_>, bone_size: u8) -> Result<Skinning, FormatError> {
    let kind = reader.u8("skinning kind")?;
    match kind {
        0 => Ok(Skinning::Bdef1 {
            bone: read_bone_index(reader, bone_size, "bdef1 bone")?,
        }),
        1 => Ok(Skinning::Bdef2 {
            bone0: read_bone_index(reader, bone_size, "bdef2 bone")?,
            bone1: read_bone_index(reader, bone_size, "bdef2 bone")?,
            weight0: reader.f32("bdef2 weight")?,
        }),
        2 => Ok(Skinning::Bdef4 {
            bones: read_bone_array(reader, bone_size)?,
            weights: read_weights(reader)?,
        }),
        3 => Ok(Skinning::Sdef {
            bone0: read_bone_index(reader, bone_size, "sdef bone")?,
            bone1: read_bone_index(reader, bone_size, "sdef bone")?,
            weight0: reader.f32("sdef weight")?,
            c: reader.vec3("sdef c")?,
            r0: reader.vec3("sdef r0")?,
            r1: reader.vec3("sdef r1")?,
        }),
        4 => Ok(Skinning::Qdef {
            bones: read_bone_array(reader, bone_size)?,
            weights: read_weights(reader)?,
        }),
        _ => Err(FormatError::Invalid {
            context: "skinning kind",
            detail: "the skinning kind must be 0 through 4",
        }),
    }
}

fn read_bone_index(
    reader: &mut Reader<'_>,
    size: u8,
    context: &'static str,
) -> Result<u16, FormatError> {
    let index = read_signed(reader, size, context)?;
    if index < 0 {
        return Ok(0);
    }
    u16::try_from(index).map_err(|_| FormatError::BadIndex {
        context,
        index: i64::from(index),
    })
}

fn read_bone_array(reader: &mut Reader<'_>, size: u8) -> Result<[u16; 4], FormatError> {
    Ok([
        read_bone_index(reader, size, "bdef bone")?,
        read_bone_index(reader, size, "bdef bone")?,
        read_bone_index(reader, size, "bdef bone")?,
        read_bone_index(reader, size, "bdef bone")?,
    ])
}

fn read_weights(reader: &mut Reader<'_>) -> Result<[f32; 4], FormatError> {
    Ok([
        reader.f32("bone weight")?,
        reader.f32("bone weight")?,
        reader.f32("bone weight")?,
        reader.f32("bone weight")?,
    ])
}

fn read_indices(
    reader: &mut Reader<'_>,
    header: &Header,
    vertex_count: usize,
) -> Result<Vec<u32>, FormatError> {
    let count = reader.count("indices", MAX_INDICES, header.vertex_size as usize)?;
    let mut indices = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let index = read_unsigned(reader, header.vertex_size, "face index")?;
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

fn read_textures(reader: &mut Reader<'_>, header: &Header) -> Result<Vec<String>, FormatError> {
    let count = reader.count("textures", MAX_TEXTURES, 4)?;
    let mut textures = Vec::with_capacity(count as usize);
    for _ in 0..count {
        textures.push(read_pmx_text(reader, header.utf8, "texture name")?);
    }
    Ok(textures)
}

fn material_index_sum(materials: &[Material]) -> u64 {
    materials
        .iter()
        .map(|material| u64::from(material.index_count))
        .sum()
}

fn read_materials(
    reader: &mut Reader<'_>,
    header: &Header,
    texture_count: usize,
    index_count: usize,
) -> Result<Vec<Material>, FormatError> {
    let start = reader.position();
    let standard = read_material_records(reader, header, texture_count, index_count, false);
    if standard
        .as_ref()
        .is_ok_and(|materials| material_index_sum(materials) == index_count as u64)
    {
        return standard;
    }
    reader.set_position(start)?;
    // mmd_tools inserts a comment string before the index count. PMX 2.0 does not
    // define that field, so it is used only when it accounts for every index.
    if let Ok(materials) = read_material_records(reader, header, texture_count, index_count, true)
        && material_index_sum(&materials) == index_count as u64
    {
        return Ok(materials);
    }
    standard
}

fn read_material_records(
    reader: &mut Reader<'_>,
    header: &Header,
    texture_count: usize,
    index_count: usize,
    material_comment: bool,
) -> Result<Vec<Material>, FormatError> {
    let count = reader.count("materials", MAX_MATERIALS, 4)?;
    let mut materials = Vec::with_capacity(count as usize);
    let mut consumed = 0u64;
    for _ in 0..count {
        let name = read_pmx_text(reader, header.utf8, "material name")?;
        let _english = read_pmx_text(reader, header.utf8, "material english name")?;
        let diffuse = reader.vec4("diffuse")?;
        let specular = reader.vec3("specular")?;
        let specular_power = reader.f32("specular power")?;
        let ambient = reader.vec3("ambient")?;
        let flags = reader.u8("material flags")?;
        let edge_color = reader.vec4("edge color")?;
        let edge_size = reader.f32("edge size")?;
        let texture = check_texture(
            read_signed(reader, header.texture_size, "texture index")?,
            texture_count,
            "texture",
        )?;
        let sphere = check_texture(
            read_signed(reader, header.texture_size, "sphere index")?,
            texture_count,
            "sphere texture",
        )?;
        let sphere_mode = match reader.u8("sphere mode")? {
            0 => SphereMode::None,
            1 => SphereMode::Multiply,
            2 => SphereMode::Add,
            _ => {
                return Err(FormatError::Invalid {
                    context: "sphere mode",
                    detail: "the sphere mode must be 0, 1, or 2",
                });
            }
        };
        let toon_shared = reader.u8("toon flag")?;
        let toon = if toon_shared == 0 {
            ToonSource::Texture(check_texture(
                read_signed(reader, header.texture_size, "toon texture")?,
                texture_count,
                "toon texture",
            )?)
        } else {
            let slot = reader.u8("shared toon")?;
            if slot > 9 {
                return Err(FormatError::BadIndex {
                    context: "shared toon",
                    index: i64::from(slot),
                });
            }
            ToonSource::Shared(slot)
        };
        if material_comment {
            let _comment = read_pmx_text(reader, header.utf8, "material comment")?;
        }
        let faces = reader.u32("material index count")?;
        consumed += u64::from(faces);
        if consumed > index_count as u64 {
            return Err(FormatError::Invalid {
                context: "material index count",
                detail: "materials consume more indices than the index buffer holds",
            });
        }
        materials.push(Material {
            name,
            diffuse: diffuse.to_array(),
            specular: specular.to_array(),
            specular_power,
            ambient: ambient.to_array(),
            flags,
            edge_color: edge_color.to_array(),
            edge_size,
            texture,
            sphere,
            sphere_mode,
            toon,
            index_count: faces,
        });
    }
    Ok(materials)
}

fn check_texture(
    index: i32,
    count: usize,
    context: &'static str,
) -> Result<Option<u32>, FormatError> {
    match optional_index(index) {
        None => Ok(None),
        Some(index) if (index as usize) < count => Ok(Some(index)),
        Some(index) => Err(FormatError::BadIndex {
            context,
            index: i64::from(index),
        }),
    }
}

fn read_bones(reader: &mut Reader<'_>, header: &Header) -> Result<Vec<Bone>, FormatError> {
    let count = reader.count("bones", MAX_BONES, 4)?;
    let mut bones = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let name = read_pmx_text(reader, header.utf8, "bone name")?;
        let english_name = read_pmx_text(reader, header.utf8, "bone english name")?;
        let position = reader.vec3("bone position")?;
        let parent = optional_bone(read_signed(reader, header.bone_size, "parent bone")?)?;
        let layer = reader.i32("bone layer")?;
        let flags = reader.u16("bone flags")?;
        let tail = if flags & 0x0001 != 0 {
            BoneTail::Bone(read_bone_index(reader, header.bone_size, "tail bone")?)
        } else {
            BoneTail::Offset(reader.vec3("tail offset")?)
        };
        let append = if flags & 0x0300 != 0 {
            Some(Append {
                parent: read_bone_index(reader, header.bone_size, "append parent")?,
                rate: reader.f32("append rate")?,
                local: flags & 0x0080 != 0,
            })
        } else {
            None
        };
        let fixed_axis = if flags & 0x0400 != 0 {
            Some(reader.vec3("fixed axis")?)
        } else {
            None
        };
        let local_axis = if flags & 0x0800 != 0 {
            Some(LocalAxis {
                x: reader.vec3("local axis x")?,
                z: reader.vec3("local axis z")?,
            })
        } else {
            None
        };
        let external_parent = if flags & 0x2000 != 0 {
            Some(reader.i32("external parent")?)
        } else {
            None
        };
        let ik = if flags & 0x0020 != 0 {
            Some(read_ik(reader, header.bone_size)?)
        } else {
            None
        };
        bones.push(Bone {
            name,
            english_name,
            position,
            parent,
            layer,
            flags,
            tail,
            append,
            fixed_axis,
            local_axis,
            external_parent,
            ik,
        });
    }
    Ok(bones)
}

fn optional_bone(index: i32) -> Result<Option<u16>, FormatError> {
    if index < 0 {
        return Ok(None);
    }
    u16::try_from(index)
        .map(Some)
        .map_err(|_| FormatError::BadIndex {
            context: "bone",
            index: i64::from(index),
        })
}

fn read_ik(reader: &mut Reader<'_>, bone_size: u8) -> Result<Ik, FormatError> {
    let target = read_bone_index(reader, bone_size, "ik target")?;
    let iterations = reader.i32("ik iterations")?;
    if iterations < 0 {
        return Err(FormatError::Invalid {
            context: "ik iterations",
            detail: "the iteration count must be non-negative",
        });
    }
    let limit_angle = reader.f32("ik limit angle")?;
    let link_count = reader.i32("ik link count")?;
    if link_count < 0 || link_count > MAX_BONES as i32 {
        return Err(FormatError::AllocationLimit {
            context: "ik links",
            count: link_count.max(0) as u64,
            limit: u64::from(MAX_BONES),
        });
    }
    reader.ensure_count("ik links", link_count as u32, MAX_BONES, 1)?;
    let mut links = Vec::with_capacity(link_count as usize);
    for _ in 0..link_count {
        let bone = read_bone_index(reader, bone_size, "ik link")?;
        let has_limit = reader.u8("ik link limit")?;
        let limits = if has_limit != 0 {
            Some((reader.vec3("ik limit min")?, reader.vec3("ik limit max")?))
        } else {
            None
        };
        links.push(IkLink { bone, limits });
    }
    Ok(Ik {
        target,
        iterations: iterations as u32,
        limit_angle,
        links,
    })
}

fn validate_vertices(vertices: &[Vertex], bone_count: usize) -> Result<(), FormatError> {
    for vertex in vertices {
        for bone in skin_bones(&vertex.skin) {
            if bone as usize >= bone_count {
                return Err(FormatError::BadIndex {
                    context: "vertex bone",
                    index: i64::from(bone),
                });
            }
        }
    }
    Ok(())
}

fn skin_bones(skin: &Skinning) -> Vec<u16> {
    match *skin {
        Skinning::Bdef1 { bone } => vec![bone],
        Skinning::Bdef2 { bone0, bone1, .. } | Skinning::Sdef { bone0, bone1, .. } => {
            vec![bone0, bone1]
        }
        Skinning::Bdef4 { bones, .. } | Skinning::Qdef { bones, .. } => bones.to_vec(),
    }
}

fn validate_morphs(morphs: &[Morph]) -> Result<(), FormatError> {
    let count = morphs.len();
    for morph in morphs {
        if let Morph::Group { offsets, .. } = morph {
            for (index, _) in offsets {
                if *index as usize >= count {
                    return Err(FormatError::BadIndex {
                        context: "group morph",
                        index: i64::from(*index),
                    });
                }
            }
        }
    }
    Ok(())
}

fn validate_bone_links(bones: &[Bone]) -> Result<(), FormatError> {
    let count = bones.len();
    for bone in bones {
        if let Some(parent) = bone.parent
            && parent as usize >= count
        {
            return Err(FormatError::BadIndex {
                context: "parent bone",
                index: i64::from(parent),
            });
        }
        if let BoneTail::Bone(tail) = bone.tail
            && tail as usize >= count
        {
            return Err(FormatError::BadIndex {
                context: "tail bone",
                index: i64::from(tail),
            });
        }
        if let Some(append) = &bone.append
            && append.parent as usize >= count
        {
            return Err(FormatError::BadIndex {
                context: "append parent",
                index: i64::from(append.parent),
            });
        }
        if let Some(ik) = &bone.ik {
            if ik.target as usize >= count {
                return Err(FormatError::BadIndex {
                    context: "ik target",
                    index: i64::from(ik.target),
                });
            }
            for link in &ik.links {
                if link.bone as usize >= count {
                    return Err(FormatError::BadIndex {
                        context: "ik link",
                        index: i64::from(link.bone),
                    });
                }
            }
        }
    }
    Ok(())
}

fn read_morphs(
    reader: &mut Reader<'_>,
    header: &Header,
    vertex_count: usize,
    bone_count: usize,
    material_count: usize,
) -> Result<Vec<Morph>, FormatError> {
    let count = reader.count("morphs", MAX_MORPHS, 4)?;
    let mut morphs = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let name = read_pmx_text(reader, header.utf8, "morph name")?;
        let english_name = read_pmx_text(reader, header.utf8, "morph english name")?;
        let panel = reader.u8("morph panel")?;
        let kind = reader.u8("morph kind")?;
        let offsets = reader.i32("morph offset count")?;
        if offsets < 0 || offsets as u32 > MAX_VERTICES {
            return Err(FormatError::AllocationLimit {
                context: "morph offsets",
                count: offsets.max(0) as u64,
                limit: u64::from(MAX_VERTICES),
            });
        }
        reader.ensure_count("morph offsets", offsets as u32, MAX_VERTICES, 1)?;
        morphs.push(read_morph(
            reader,
            header,
            name,
            english_name,
            panel,
            kind,
            offsets as u32,
            vertex_count,
            bone_count,
            material_count,
            morphs.len(),
        )?);
    }
    Ok(morphs)
}

#[allow(clippy::too_many_arguments)] // Each argument is one PMX morph header field or a count used to validate offsets.
fn read_morph(
    reader: &mut Reader<'_>,
    header: &Header,
    name: String,
    english_name: String,
    panel: u8,
    kind: u8,
    count: u32,
    vertex_count: usize,
    bone_count: usize,
    material_count: usize,
    _morph_count: usize,
) -> Result<Morph, FormatError> {
    match kind {
        0 => {
            let mut offsets = Vec::with_capacity(count as usize);
            for _ in 0..count {
                let index = read_signed(reader, header.morph_size, "group morph")?;
                let weight = reader.f32("group morph weight")?;
                let index = u16::try_from(index).map_err(|_| FormatError::BadIndex {
                    context: "group morph",
                    index: i64::from(index),
                })?;
                offsets.push((index, weight));
            }
            Ok(Morph::Group {
                name,
                english_name,
                panel,
                offsets,
            })
        }
        1 => {
            let mut offsets = Vec::with_capacity(count as usize);
            for _ in 0..count {
                let index = read_unsigned(reader, header.vertex_size, "vertex morph")?;
                let offset = reader.vec3("vertex morph offset")?;
                if index as usize >= vertex_count {
                    return Err(FormatError::BadIndex {
                        context: "vertex morph",
                        index: i64::from(index),
                    });
                }
                offsets.push((index, offset));
            }
            Ok(Morph::Vertex {
                name,
                english_name,
                panel,
                offsets,
            })
        }
        2 => {
            let mut offsets = Vec::with_capacity(count as usize);
            for _ in 0..count {
                let index = read_bone_index(reader, header.bone_size, "bone morph")?;
                let translation = reader.vec3("bone morph translation")?;
                let rotation = glam::Quat::from_xyzw(
                    reader.f32("bone morph rotation")?,
                    reader.f32("bone morph rotation")?,
                    reader.f32("bone morph rotation")?,
                    reader.f32("bone morph rotation")?,
                );
                if index as usize >= bone_count || rotation.length_squared() < 1.0e-12 {
                    return Err(FormatError::BadIndex {
                        context: "bone morph",
                        index: i64::from(index),
                    });
                }
                offsets.push((index, translation, rotation.normalize()));
            }
            Ok(Morph::Bone {
                name,
                english_name,
                panel,
                offsets,
            })
        }
        3..=7 => {
            let channel = match kind {
                3 => UvChannel::Primary,
                4 => UvChannel::Extra1,
                5 => UvChannel::Extra2,
                6 => UvChannel::Extra3,
                _ => UvChannel::Extra4,
            };
            let mut offsets = Vec::with_capacity(count as usize);
            for _ in 0..count {
                let index = read_unsigned(reader, header.vertex_size, "uv morph")?;
                let offset = reader.vec4("uv morph offset")?;
                if index as usize >= vertex_count {
                    return Err(FormatError::BadIndex {
                        context: "uv morph",
                        index: i64::from(index),
                    });
                }
                offsets.push((index, offset));
            }
            Ok(Morph::Uv {
                name,
                english_name,
                panel,
                channel,
                offsets,
            })
        }
        8 => {
            let mut offsets = Vec::with_capacity(count as usize);
            for _ in 0..count {
                let index = read_signed(reader, header.material_size, "material morph")?;
                let add = match reader.u8("material morph op")? {
                    0 => false,
                    1 => true,
                    _ => {
                        return Err(FormatError::Invalid {
                            context: "material morph",
                            detail: "the operation must be 0 or 1",
                        });
                    }
                };
                let material = match optional_index(index) {
                    None => None,
                    Some(index) if (index as usize) < material_count => Some(index),
                    Some(index) => {
                        return Err(FormatError::BadIndex {
                            context: "material morph",
                            index: i64::from(index),
                        });
                    }
                };
                offsets.push(MaterialOffset {
                    material,
                    add,
                    diffuse: reader.vec4("material diffuse")?.to_array(),
                    specular: reader.vec3("material specular")?.to_array(),
                    specular_power: reader.f32("material specular power")?,
                    ambient: reader.vec3("material ambient")?.to_array(),
                    edge_color: reader.vec4("material edge")?.to_array(),
                    edge_size: reader.f32("material edge size")?,
                    texture: reader.vec4("material texture factor")?.to_array(),
                    sphere: reader.vec4("material sphere factor")?.to_array(),
                    toon: reader.vec4("material toon factor")?.to_array(),
                });
            }
            Ok(Morph::Material {
                name,
                english_name,
                panel,
                offsets,
            })
        }
        _ => Err(FormatError::Invalid {
            context: "morph kind",
            detail: "the morph kind must be 0 through 8",
        }),
    }
}

fn read_display(
    reader: &mut Reader<'_>,
    header: &Header,
    bone_count: usize,
    morph_count: usize,
) -> Result<Vec<DisplayFrame>, FormatError> {
    let count = reader.count("display frames", MAX_GENERIC, 4)?;
    let mut frames = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let name = read_pmx_text(reader, header.utf8, "display frame")?;
        let _english = read_pmx_text(reader, header.utf8, "display frame english")?;
        let _special = reader.u8("display special")?;
        let entries_count = reader.i32("display entries")?;
        if entries_count < 0 || entries_count as u32 > MAX_GENERIC {
            return Err(FormatError::AllocationLimit {
                context: "display entries",
                count: entries_count.max(0) as u64,
                limit: u64::from(MAX_GENERIC),
            });
        }
        let mut entries = Vec::with_capacity(entries_count as usize);
        for _ in 0..entries_count {
            let is_morph = reader.u8("display entry kind")? != 0;
            let index = if is_morph {
                read_signed(reader, header.morph_size, "display morph")?
            } else {
                read_signed(reader, header.bone_size, "display bone")?
            };
            let limit = if is_morph { morph_count } else { bone_count };
            if index < 0 || index as usize >= limit {
                return Err(FormatError::BadIndex {
                    context: "display entry",
                    index: i64::from(index),
                });
            }
            entries.push((is_morph, index as u32));
        }
        frames.push(DisplayFrame { name, entries });
    }
    Ok(frames)
}

fn read_rigid_bodies(
    reader: &mut Reader<'_>,
    header: &Header,
    bone_count: usize,
) -> Result<Vec<RigidBody>, FormatError> {
    let count = reader.count("rigid bodies", MAX_GENERIC, 4)?;
    let mut bodies = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let name = read_pmx_text(reader, header.utf8, "rigid body name")?;
        let _english = read_pmx_text(reader, header.utf8, "rigid body english name")?;
        let bone = optional_bone(read_signed(reader, header.bone_size, "rigid bone")?)?;
        if let Some(bone) = bone
            && bone as usize >= bone_count
        {
            return Err(FormatError::BadIndex {
                context: "rigid bone",
                index: i64::from(bone),
            });
        }
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

fn read_joints(
    reader: &mut Reader<'_>,
    header: &Header,
    rigid_count: usize,
) -> Result<Vec<Joint>, FormatError> {
    let count = reader.count("joints", MAX_GENERIC, 4)?;
    let mut joints = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let name = read_pmx_text(reader, header.utf8, "joint name")?;
        let _english = read_pmx_text(reader, header.utf8, "joint english name")?;
        let kind = reader.u8("joint kind")?;
        let rigid_a = check_rigid(
            read_signed(reader, header.rigid_size, "joint rigid")?,
            rigid_count,
        )?;
        let rigid_b = check_rigid(
            read_signed(reader, header.rigid_size, "joint rigid")?,
            rigid_count,
        )?;
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

fn check_rigid(index: i32, count: usize) -> Result<Option<u32>, FormatError> {
    match optional_index(index) {
        None => Ok(None),
        Some(index) if (index as usize) < count => Ok(Some(index)),
        Some(index) => Err(FormatError::BadIndex {
            context: "joint rigid",
            index: i64::from(index),
        }),
    }
}
