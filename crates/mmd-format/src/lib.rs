//! PMX, PMD, and VMD parsers.
//!
//! The parsers reject truncated files, indices that do not exist, and
//! non-finite floats. Rigid bodies and joints are stored and are not simulated.

mod error;
mod pmd;
mod pmx;
mod reader;
mod text;
mod vmd;

pub use error::FormatError;
pub use vmd::{bone_curves, camera_curves};

use std::path::Path;

use mmd_core::{ModelAsset, Motion};

/// Read a `.pmx` or `.pmd` file.
pub fn load_model(path: &Path) -> Result<ModelAsset, FormatError> {
    let bytes = std::fs::read(path)?;
    parse_model(&bytes)
}

/// Parse model bytes. The kind is taken from the magic bytes.
pub fn parse_model(bytes: &[u8]) -> Result<ModelAsset, FormatError> {
    if bytes.starts_with(b"PMX ") {
        pmx::parse_pmx(bytes)
    } else if bytes.starts_with(b"Pmd") {
        pmd::parse_pmd(bytes)
    } else {
        Err(FormatError::BadMagic)
    }
}

/// Read a `.vmd` file.
pub fn load_motion(path: &Path) -> Result<Motion, FormatError> {
    let bytes = std::fs::read(path)?;
    parse_motion(&bytes)
}

/// Parse VMD bytes.
pub fn parse_motion(bytes: &[u8]) -> Result<Motion, FormatError> {
    vmd::parse_vmd(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pmx_text(value: &str) -> Vec<u8> {
        let mut bytes = (value.len() as u32).to_le_bytes().to_vec();
        bytes.extend(value.as_bytes());
        bytes
    }

    fn sample_pmx() -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend(b"PMX ");
        bytes.extend(2.0f32.to_le_bytes());
        bytes.push(8);
        bytes.extend([1, 0, 1, 1, 1, 1, 1, 1]);
        for text in ["Miku", "", "", ""] {
            bytes.extend(pmx_text(text));
        }
        bytes.extend(1u32.to_le_bytes());
        bytes.extend([0.0f32, 1.0, 0.0].into_iter().flat_map(f32::to_le_bytes));
        bytes.extend([0.0f32, 1.0, 0.0].into_iter().flat_map(f32::to_le_bytes));
        bytes.extend([0.0f32, 0.0].into_iter().flat_map(f32::to_le_bytes));
        bytes.push(0);
        bytes.push(0);
        bytes.extend(1.0f32.to_le_bytes());
        bytes.extend(3u32.to_le_bytes());
        bytes.extend([0u8, 0, 0]);
        bytes.extend(0u32.to_le_bytes());
        bytes.extend(1u32.to_le_bytes());
        bytes.extend(pmx_text("skin"));
        bytes.extend(pmx_text(""));
        bytes.extend(
            [1.0f32, 1.0, 1.0, 1.0]
                .into_iter()
                .flat_map(f32::to_le_bytes),
        );
        bytes.extend([1.0f32, 1.0, 1.0].into_iter().flat_map(f32::to_le_bytes));
        bytes.extend(10.0f32.to_le_bytes());
        bytes.extend([0.5f32, 0.5, 0.5].into_iter().flat_map(f32::to_le_bytes));
        bytes.push(0x1f);
        bytes.extend(
            [0.0f32, 0.0, 0.0, 1.0]
                .into_iter()
                .flat_map(f32::to_le_bytes),
        );
        bytes.extend(1.0f32.to_le_bytes());
        bytes.push(0xff);
        bytes.push(0xff);
        bytes.push(0);
        bytes.push(1);
        bytes.push(0);
        bytes.extend(3u32.to_le_bytes());
        bytes.extend(1u32.to_le_bytes());
        bytes.extend(pmx_text("center"));
        bytes.extend(pmx_text(""));
        bytes.extend([0.0f32, 0.0, 0.0].into_iter().flat_map(f32::to_le_bytes));
        bytes.push(0xff);
        bytes.extend(0i32.to_le_bytes());
        bytes.extend(0x0002u16.to_le_bytes());
        bytes.extend([0.0f32, 1.0, 0.0].into_iter().flat_map(f32::to_le_bytes));
        bytes.extend(0u32.to_le_bytes());
        bytes.extend(0u32.to_le_bytes());
        bytes.extend(0u32.to_le_bytes());
        bytes.extend(0u32.to_le_bytes());
        bytes
    }

    #[test]
    fn pmx_reads_a_one_bone_model() {
        let model = parse_model(&sample_pmx()).expect("pmx");
        assert_eq!(model.name, "Miku");
        assert_eq!(model.vertices.len(), 1);
        assert_eq!(model.indices, vec![0, 0, 0]);
        assert_eq!(model.materials.len(), 1);
        assert!(model.materials[0].ground_shadow());
        assert_eq!(model.bones[0].name, "center");
        assert!(model.bones[0].parent.is_none());
    }

    #[test]
    fn pmx_reads_a_material_comment_written_before_the_index_count() {
        let mut bytes = sample_pmx();
        let faces = 3u32.to_le_bytes();
        let slot = bytes
            .windows(4)
            .rposition(|window| window == faces)
            .expect("index count");
        bytes.splice(slot..slot, 0u32.to_le_bytes());
        let model = parse_model(&bytes).expect("pmx with material comment");
        assert_eq!(model.materials[0].name, "skin");
        assert_eq!(model.materials[0].index_count, 3);
        assert_eq!(model.bones[0].name, "center");
    }

    #[test]
    fn pmx_rejects_a_truncated_file_and_a_huge_count() {
        assert!(matches!(
            parse_model(b"PMX "),
            Err(FormatError::Truncated { .. })
        ));
        let mut bytes = sample_pmx();
        let count_at = bytes
            .windows(4)
            .position(|window| window == 1u32.to_le_bytes())
            .expect("vertex count");
        bytes[count_at..count_at + 4].copy_from_slice(&3_000_000u32.to_le_bytes());
        assert!(matches!(
            parse_model(&bytes),
            Err(FormatError::AllocationLimit { .. })
        ));
    }

    #[test]
    fn pmx_rejects_a_non_finite_position() {
        let mut bytes = sample_pmx();
        let marker: Vec<u8> = [0.0f32, 1.0, 0.0]
            .into_iter()
            .flat_map(f32::to_le_bytes)
            .collect();
        let position = bytes
            .windows(12)
            .position(|window| window == marker.as_slice())
            .expect("position");
        bytes[position..position + 4].copy_from_slice(&f32::NAN.to_le_bytes());
        assert!(matches!(
            parse_model(&bytes),
            Err(FormatError::NonFinite(_))
        ));
    }

    fn sample_pmd() -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend(b"Pmd");
        bytes.extend(1.0f32.to_le_bytes());
        bytes.extend(std::iter::repeat_n(0u8, 20 + 256));
        bytes.extend(0u32.to_le_bytes());
        bytes.extend(0u32.to_le_bytes());
        bytes.extend(0u32.to_le_bytes());
        bytes.extend(1u16.to_le_bytes());
        bytes.extend(b"center");
        bytes.extend(std::iter::repeat_n(0u8, 14));
        bytes.extend(u16::MAX.to_le_bytes());
        bytes.extend(u16::MAX.to_le_bytes());
        bytes.push(0);
        bytes.extend(0u16.to_le_bytes());
        bytes.extend([0.0f32, 0.0, 0.0].into_iter().flat_map(f32::to_le_bytes));
        bytes.extend(0u16.to_le_bytes());
        bytes.extend(0u16.to_le_bytes());
        bytes.push(0);
        bytes.push(0);
        bytes.extend(0u32.to_le_bytes());
        bytes.push(0);
        bytes.extend(std::iter::repeat_n(0u8, 1000));
        bytes.extend(0u32.to_le_bytes());
        bytes.extend(0u32.to_le_bytes());
        bytes
    }

    #[test]
    fn pmd_reads_a_single_bone() {
        let model = parse_model(&sample_pmd()).expect("pmd");
        assert_eq!(model.bones.len(), 1);
        assert_eq!(model.bones[0].name, "center");
        assert!(model.rigid_bodies.is_empty());
    }

    fn sample_vmd() -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend(b"Vocaloid Motion Data 0002");
        bytes.extend(std::iter::repeat_n(0u8, 5));
        bytes.extend(std::iter::repeat_n(0u8, 20));
        bytes.extend(1u32.to_le_bytes());
        bytes.extend(b"center");
        bytes.extend(std::iter::repeat_n(0u8, 9));
        bytes.extend(0u32.to_le_bytes());
        bytes.extend([0.0f32, 1.0, 0.0].into_iter().flat_map(f32::to_le_bytes));
        bytes.extend(
            [0.0f32, 0.0, 0.0, 1.0]
                .into_iter()
                .flat_map(f32::to_le_bytes),
        );
        let mut curves = [0u8; 64];
        curves[..16].copy_from_slice(&[
            20, 20, 0, 0, 20, 20, 20, 20, 107, 107, 107, 107, 107, 107, 107, 107,
        ]);
        curves[16..32].copy_from_slice(&[
            20, 20, 20, 20, 20, 20, 20, 107, 107, 107, 107, 107, 107, 107, 107, 0,
        ]);
        bytes.extend(curves);
        bytes.extend(0u32.to_le_bytes());
        bytes.extend(1u32.to_le_bytes());
        bytes.extend(10u32.to_le_bytes());
        bytes.extend((-12.0f32).to_le_bytes());
        bytes.extend([0.0f32, 1.0, 0.0].into_iter().flat_map(f32::to_le_bytes));
        bytes.extend([0.0f32, 0.0, 0.0].into_iter().flat_map(f32::to_le_bytes));
        bytes.extend(std::iter::repeat_n(0u8, 24));
        bytes.extend(30u32.to_le_bytes());
        bytes.push(0);
        bytes
    }

    #[test]
    fn vmd_reads_z_and_rotation_x1_from_the_shifted_row() {
        let curves = bone_curves(&[
            20, 20, 0, 0, 20, 20, 20, 20, 107, 107, 107, 107, 107, 107, 107, 107, 20, 20, 20, 20,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ]);
        assert_eq!(curves[2].x1, 20);
        assert_eq!(curves[3].x1, 20);
        assert_eq!(curves[0].x1, 20);
        let motion = parse_motion(&sample_vmd()).expect("vmd");
        assert_eq!(motion.bones[0].keys[0].curves[2].x1, 20);
        assert_eq!(motion.cameras.len(), 1);
        assert_eq!(motion.cameras[0].distance, -12.0);
        assert!(motion.cameras[0].perspective);
    }

    #[test]
    fn unknown_magic_is_rejected() {
        assert!(matches!(parse_model(b"XXXX"), Err(FormatError::BadMagic)));
        assert!(matches!(
            parse_motion(b"not a vmd"),
            Err(FormatError::BadMagic)
        ));
    }
}
