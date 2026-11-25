//! Bone ordering and skin matrices for bones the solver moved.

use glam::{Mat4, Quat, Vec3};
use mmd_core::ModelAsset;

pub(crate) fn write_skin(
    model: &ModelAsset,
    translation: &[Vec3],
    rotation: &[Quat],
    skin: &mut [Mat4],
    changed: &[bool],
) {
    for index in 0..model.bones.len() {
        if index >= skin.len() || !changed[index] {
            continue;
        }
        let world = Mat4::from_rotation_translation(rotation[index], translation[index]);
        let rest = Mat4::from_translation(model.bones[index].position);
        skin[index] = world * rest.inverse();
    }
}

pub(crate) fn bone_order(model: &ModelAsset) -> Vec<usize> {
    let count = model.bones.len();
    let mut pending: Vec<usize> = (0..count).collect();
    let mut order = Vec::with_capacity(count);
    let mut done = vec![false; count];
    while !pending.is_empty() {
        let mut later = Vec::new();
        let before = order.len();
        for index in pending {
            let ready = model.bones[index]
                .parent
                .map(|parent| (parent as usize) < count && done[parent as usize])
                .unwrap_or(true);
            if ready {
                done[index] = true;
                order.push(index);
            } else {
                later.push(index);
            }
        }
        if order.len() == before {
            order.extend(later);
            break;
        }
        pending = later;
    }
    order
}

#[cfg(test)]
mod tests {
    use super::*;
    use mmd_core::{Bone, BoneTail};

    fn bone(name: &str, parent: Option<u16>, position: Vec3) -> Bone {
        Bone {
            name: name.to_owned(),
            english_name: String::new(),
            position,
            parent,
            layer: 0,
            flags: 0,
            tail: BoneTail::Offset(Vec3::Y),
            append: None,
            fixed_axis: None,
            local_axis: None,
            external_parent: None,
            ik: None,
        }
    }

    #[test]
    fn a_parent_comes_before_its_child() {
        let model = ModelAsset {
            name: String::new(),
            english_name: String::new(),
            comment: String::new(),
            vertices: Vec::new(),
            indices: Vec::new(),
            textures: Vec::new(),
            materials: Vec::new(),
            bones: vec![bone("子", Some(1), Vec3::Y), bone("親", None, Vec3::ZERO)],
            morphs: Vec::new(),
            display_frames: Vec::new(),
            rigid_bodies: Vec::new(),
            joints: Vec::new(),
        };
        assert_eq!(bone_order(&model), vec![1, 0]);
    }
}
