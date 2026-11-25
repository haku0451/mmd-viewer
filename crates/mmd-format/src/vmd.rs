//! VMD reader.
//!
//! Bone curves use the later key. x1 of the Z and rotation channels is read
//! from bytes 17 and 18. Bytes 2 and 3 of the first row are zero in files
//! written by MikuMikuDance, so reading them makes those channels nonlinear.
//!
//! A camera key places the eye at `interest + R * (0, 0, -distance)` with `R`
//! built from XYZ Euler angles. Keys one frame apart are cuts: integer frame
//! evaluation already lands on a key and does not blend across that gap.
//!
//! Light, self-shadow, and IK blocks are optional. Reading stops when fewer
//! than four bytes remain.

use mmd_core::{
    Bezier, BoneKeyframe, BoneTrack, CameraKeyframe, FrameTime, IkKeyframe, LightKeyframe,
    MorphKeyframe, MorphTrack, Motion, ShadowKeyframe,
};

use crate::error::FormatError;
use crate::reader::Reader;
use crate::text::decode_shift_jis;

const MAX_KEYS: u32 = 5_000_000;

/// Parse a VMD motion.
pub fn parse_vmd(bytes: &[u8]) -> Result<Motion, FormatError> {
    let mut reader = Reader::new(bytes);
    if !bytes.starts_with(b"Vocaloid Motion Data") {
        return Err(FormatError::BadMagic);
    }
    let _magic = reader.read_exact(30, "VMD header")?;
    let model_name = decode_shift_jis(reader.read_exact(20, "model name")?);
    let mut bones = read_bone_tracks(&mut reader)?;
    let mut morphs = read_morph_tracks(&mut reader)?;
    let mut cameras = read_optional_cameras(&mut reader)?;
    let lights = read_optional_lights(&mut reader)?;
    let shadows = read_optional_shadows(&mut reader)?;
    let ik = read_optional_ik(&mut reader)?;
    sort_tracks(&mut bones, &mut morphs, &mut cameras);
    Ok(Motion {
        model_name,
        bones,
        morphs,
        cameras,
        lights,
        shadows,
        ik,
    })
}

pub fn bone_curves(bytes: &[u8; 64]) -> [Bezier; 4] {
    let mut curves = [Bezier::LINEAR; 4];
    for channel in 0..4 {
        let x1 = match channel {
            2 => bytes[17],
            3 => bytes[18],
            index => bytes[index],
        };
        curves[channel] = Bezier {
            x1,
            y1: bytes[channel + 4],
            x2: bytes[channel + 8],
            y2: bytes[channel + 12],
        };
    }
    curves
}

pub fn camera_curves(bytes: &[u8; 24]) -> [Bezier; 6] {
    let mut curves = [Bezier::LINEAR; 6];
    for (channel, curve) in curves.iter_mut().enumerate() {
        let offset = channel * 4;
        *curve = Bezier {
            x1: bytes[offset],
            x2: bytes[offset + 1],
            y1: bytes[offset + 2],
            y2: bytes[offset + 3],
        };
    }
    curves
}

fn read_bone_tracks(reader: &mut Reader<'_>) -> Result<Vec<BoneTrack>, FormatError> {
    let count = reader.count("bone keys", MAX_KEYS, 111)?;
    let mut grouped: Vec<BoneTrack> = Vec::new();
    for _ in 0..count {
        let name = decode_shift_jis(reader.read_exact(15, "bone name")?);
        let frame = FrameTime::new(reader.u32("bone frame")? as i32);
        let translation = reader.vec3("bone translation")?;
        let rotation = read_quat(reader)?;
        let mut raw = [0u8; 64];
        raw.copy_from_slice(reader.read_exact(64, "bone interpolation")?);
        let key = BoneKeyframe {
            frame,
            translation,
            rotation,
            curves: bone_curves(&raw),
        };
        push_bone(&mut grouped, name, key);
    }
    Ok(grouped)
}

fn push_bone(tracks: &mut Vec<BoneTrack>, name: String, key: BoneKeyframe) {
    if let Some(track) = tracks.iter_mut().find(|track| track.name == name) {
        track.keys.push(key);
    } else {
        tracks.push(BoneTrack {
            name,
            keys: vec![key],
        });
    }
}

fn read_quat(reader: &mut Reader<'_>) -> Result<glam::Quat, FormatError> {
    let quat = glam::Quat::from_xyzw(
        reader.f32("rotation")?,
        reader.f32("rotation")?,
        reader.f32("rotation")?,
        reader.f32("rotation")?,
    );
    if quat.length_squared() < 1.0e-12 {
        return Err(FormatError::NonFinite("rotation"));
    }
    Ok(quat.normalize())
}

fn read_morph_tracks(reader: &mut Reader<'_>) -> Result<Vec<MorphTrack>, FormatError> {
    let count = reader.count("morph keys", MAX_KEYS, 23)?;
    let mut grouped: Vec<MorphTrack> = Vec::new();
    for _ in 0..count {
        let name = decode_shift_jis(reader.read_exact(15, "morph name")?);
        let frame = FrameTime::new(reader.u32("morph frame")? as i32);
        let weight = reader.f32("morph weight")?;
        let key = MorphKeyframe { frame, weight };
        if let Some(track) = grouped.iter_mut().find(|track| track.name == name) {
            track.keys.push(key);
        } else {
            grouped.push(MorphTrack {
                name,
                keys: vec![key],
            });
        }
    }
    Ok(grouped)
}

fn read_optional_cameras(reader: &mut Reader<'_>) -> Result<Vec<CameraKeyframe>, FormatError> {
    if reader.remaining() < 4 {
        return Ok(Vec::new());
    }
    let count = reader.count("camera keys", MAX_KEYS, 61)?;
    let mut keys = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let frame = FrameTime::new(reader.u32("camera frame")? as i32);
        let distance = reader.f32("camera distance")?;
        let interest = reader.vec3("camera interest")?;
        let rotation = reader.vec3("camera rotation")?;
        let mut raw = [0u8; 24];
        raw.copy_from_slice(reader.read_exact(24, "camera interpolation")?);
        let fov = reader.u32("camera fov")?;
        let perspective = reader.u8("camera perspective")? == 0;
        keys.push(CameraKeyframe {
            frame,
            interest,
            rotation,
            distance,
            fov_degrees: fov as f32,
            perspective,
            curves: camera_curves(&raw),
        });
    }
    Ok(keys)
}

fn read_optional_lights(reader: &mut Reader<'_>) -> Result<Vec<LightKeyframe>, FormatError> {
    if reader.remaining() < 4 {
        return Ok(Vec::new());
    }
    let count = reader.count("light keys", MAX_KEYS, 28)?;
    let mut keys = Vec::with_capacity(count as usize);
    for _ in 0..count {
        keys.push(LightKeyframe {
            frame: FrameTime::new(reader.u32("light frame")? as i32),
            color: reader.vec3("light color")?,
            direction: reader.vec3("light direction")?,
        });
    }
    Ok(keys)
}

fn read_optional_shadows(reader: &mut Reader<'_>) -> Result<Vec<ShadowKeyframe>, FormatError> {
    if reader.remaining() < 4 {
        return Ok(Vec::new());
    }
    let count = reader.count("shadow keys", MAX_KEYS, 9)?;
    let mut keys = Vec::with_capacity(count as usize);
    for _ in 0..count {
        keys.push(ShadowKeyframe {
            frame: FrameTime::new(reader.u32("shadow frame")? as i32),
            mode: reader.u8("shadow mode")?,
            distance: reader.f32("shadow distance")?,
        });
    }
    Ok(keys)
}

fn read_optional_ik(reader: &mut Reader<'_>) -> Result<Vec<IkKeyframe>, FormatError> {
    if reader.remaining() < 4 {
        return Ok(Vec::new());
    }
    let count = reader.u32("ik keys")?;
    if count > MAX_KEYS {
        return Err(FormatError::AllocationLimit {
            context: "ik keys",
            count: u64::from(count),
            limit: u64::from(MAX_KEYS),
        });
    }
    let mut keys = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let frame = FrameTime::new(reader.u32("ik frame")? as i32);
        let visible = reader.u8("model visibility")? != 0;
        let chains_count = reader.u32("ik chain count")?;
        if chains_count > 16_384 {
            return Err(FormatError::AllocationLimit {
                context: "ik chains",
                count: u64::from(chains_count),
                limit: 16_384,
            });
        }
        reader.ensure_count("ik chains", chains_count, 16_384, 21)?;
        let mut chains = Vec::with_capacity(chains_count as usize);
        for _ in 0..chains_count {
            let name = decode_shift_jis(reader.read_exact(20, "ik name")?);
            let enabled = reader.u8("ik enabled")? != 0;
            chains.push((name, enabled));
        }
        keys.push(IkKeyframe {
            frame,
            visible,
            chains,
        });
    }
    keys.sort_by_key(|key| key.frame);
    Ok(keys)
}

fn sort_tracks(
    bones: &mut [BoneTrack],
    morphs: &mut [MorphTrack],
    cameras: &mut Vec<CameraKeyframe>,
) {
    for track in bones {
        track.keys.sort_by_key(|key| key.frame);
        track.keys.dedup_by(|later, earlier| {
            if later.frame == earlier.frame {
                *earlier = later.clone();
                true
            } else {
                false
            }
        });
    }
    for track in morphs {
        track.keys.sort_by_key(|key| key.frame);
        track.keys.dedup_by(|later, earlier| {
            if later.frame == earlier.frame {
                *earlier = later.clone();
                true
            } else {
                false
            }
        });
    }
    cameras.sort_by_key(|key| key.frame);
    cameras.dedup_by(|later, earlier| {
        if later.frame == earlier.frame {
            *earlier = later.clone();
            true
        } else {
            false
        }
    });
}
