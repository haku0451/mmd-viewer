//! Pose evaluation.
//!
//! The stages are: sample keys, apply morphs, grant append transforms in
//! deform order, constrain fixed axes, solve IK, then build skin matrices.
//! Bones flagged as post-physics are evaluated in that same pass. The viewer
//! runs Bullet afterwards and overwrites physics bones. External parents are
//! ignored, so those bones keep the transform computed from their own keys.
//!
//! IK links with limits are hinges on local X, using the X components of the
//! stored minimum and maximum. That is the classic MMD link constraint.

use std::collections::HashMap;

use glam::{EulerRot, Mat4, Quat, Vec2, Vec3};
use mmd_core::{
    Bone, CameraKeyframe, FrameTime, IkKeyframe, Material, ModelAsset, Morph, Motion, Skinning,
    UvChannel,
};

use crate::bezier;
use crate::skin;

/// Direction used when a motion has no light key.
///
/// The vector points the way the light travels. MMD models face -Z, so the
/// light travels slightly toward +Z and reaches the face from the front.
pub fn default_light_direction() -> Vec3 {
    Vec3::new(0.2, -1.0, 0.4).normalize()
}

/// One evaluated frame.
#[derive(Clone, Debug)]
pub struct EvaluatedFrame {
    /// Morphed positions in model space.
    pub positions: Vec<Vec3>,
    /// Morphed primary UVs.
    pub uvs: Vec<Vec2>,
    /// Skin matrices, `world * inverse(rest world)`.
    pub skin: Vec<Mat4>,
    /// World rotation of each bone.
    pub bone_rotation: Vec<Quat>,
    /// World translation of each bone.
    pub bone_translation: Vec<Vec3>,
    /// Materials after material morphs.
    pub materials: Vec<MaterialFrame>,
    /// Morph weights after group morphs.
    pub morph_weights: Vec<f32>,
    /// Model visibility from the latest IK/visibility key.
    pub visible: bool,
    /// Camera sample, when the motion has a camera key.
    pub camera: Option<CameraSample>,
    /// `true` when the current camera key asks for an orthographic camera.
    pub orthographic_camera: bool,
    /// Light travel direction.
    pub light_direction: Vec3,
}

/// Material values after morphs.
#[derive(Clone, Debug)]
pub struct MaterialFrame {
    /// Diffuse color and alpha.
    pub diffuse: [f32; 4],
    pub specular: [f32; 3],
    pub specular_power: f32,
    pub ambient: [f32; 3],
    pub edge_color: [f32; 4],
    /// Edge width passed to the shell. The renderer scales it.
    pub edge_size: f32,
    pub texture: [f32; 4],
    pub sphere: [f32; 4],
    pub double_sided: bool,
    pub ground_shadow: bool,
    pub cast_shadow: bool,
    pub receive_shadow: bool,
    pub draw_edge: bool,
}

/// A camera placed in model space.
#[derive(Clone, Copy, Debug)]
pub struct CameraSample {
    pub eye: Vec3,
    /// Look-at point.
    pub interest: Vec3,
    /// Vertical field of view in radians.
    pub fov_y_radians: f32,
}

/// Evaluate `model` at `frame`.
///
/// `motion` may be absent. The result is then the rest pose with no morphs.
pub fn evaluate(model: &ModelAsset, motion: Option<&Motion>, frame: FrameTime) -> EvaluatedFrame {
    let morph_weights = morph_weights(model, motion, frame);
    let positions = morphed_positions(model, &morph_weights);
    let uvs = morphed_uvs(model, &morph_weights);
    let materials = morphed_materials(model, &morph_weights);
    let (local_t, local_r) = local_pose(model, motion, frame, &morph_weights);
    let (world, skin) = skin_from_local(model, &local_t, &local_r);
    let mut bone_rotation = Vec::with_capacity(world.len());
    let mut bone_translation = Vec::with_capacity(world.len());
    for matrix in &world {
        bone_rotation.push(Quat::from_mat4(matrix));
        bone_translation.push(matrix.transform_point3(Vec3::ZERO));
    }
    let (visible, orthographic_camera, camera, light_direction) = motion_extras(motion, frame);
    EvaluatedFrame {
        positions,
        uvs,
        skin,
        bone_rotation,
        bone_translation,
        materials,
        morph_weights,
        visible,
        camera,
        orthographic_camera,
        light_direction,
    }
}

/// Names of bones that declare an external parent.
pub fn external_parent_names(model: &ModelAsset) -> Vec<&str> {
    model
        .bones
        .iter()
        .filter(|bone| bone.external_parent.is_some())
        .map(|bone| bone.name.as_str())
        .collect()
}

fn motion_extras(
    motion: Option<&Motion>,
    frame: FrameTime,
) -> (bool, bool, Option<CameraSample>, Vec3) {
    let Some(motion) = motion else {
        return (true, false, None, default_light_direction());
    };
    let mut visible = true;
    for key in &motion.ik {
        if key.frame.frame() <= frame.frame() {
            visible = key.visible;
        }
    }
    let camera_key = active_camera(&motion.cameras, frame);
    let orthographic = camera_key.is_some_and(|key| !key.perspective);
    let camera = camera_key
        .filter(|key| key.perspective)
        .map(|key| camera_sample(key, &motion.cameras, frame));
    let mut light = default_light_direction();
    for key in &motion.lights {
        if key.frame.frame() <= frame.frame() && key.direction.length_squared() > 1.0e-8 {
            light = key.direction.normalize();
        }
    }
    (visible, orthographic, camera, light)
}

fn active_camera(keys: &[CameraKeyframe], frame: FrameTime) -> Option<&CameraKeyframe> {
    if keys.is_empty() {
        return None;
    }
    let frame = frame.frame();
    if frame <= keys[0].frame.frame() {
        return Some(&keys[0]);
    }
    if frame >= keys[keys.len() - 1].frame.frame() {
        return Some(&keys[keys.len() - 1]);
    }
    let right = keys.partition_point(|key| key.frame.frame() <= frame);
    Some(&keys[right - 1])
}

fn camera_sample(
    current: &CameraKeyframe,
    keys: &[CameraKeyframe],
    frame: FrameTime,
) -> CameraSample {
    let pose = interpolated_camera(keys, frame).unwrap_or((
        current.interest,
        current.rotation,
        current.distance,
        current.fov_degrees,
    ));
    let (interest, rotation, distance, fov) = pose;
    let orient = Mat4::from_euler(EulerRot::XYZ, rotation.x, rotation.y, rotation.z);
    let eye = interest + orient.transform_vector3(Vec3::new(0.0, 0.0, -distance));
    CameraSample {
        eye,
        interest,
        fov_y_radians: fov.to_radians().clamp(0.01, std::f32::consts::PI - 0.01),
    }
}

fn interpolated_camera(
    keys: &[CameraKeyframe],
    frame: FrameTime,
) -> Option<(Vec3, Vec3, f32, f32)> {
    if keys.is_empty() {
        return None;
    }
    let frame = frame.frame();
    if frame <= keys[0].frame.frame() {
        let key = &keys[0];
        return Some((key.interest, key.rotation, key.distance, key.fov_degrees));
    }
    if frame >= keys[keys.len() - 1].frame.frame() {
        let key = &keys[keys.len() - 1];
        return Some((key.interest, key.rotation, key.distance, key.fov_degrees));
    }
    let right = keys.partition_point(|key| key.frame.frame() <= frame);
    let left = &keys[right - 1];
    let right = &keys[right];
    let span = (right.frame.frame() - left.frame.frame()).max(1) as f32;
    let x = (frame - left.frame.frame()) as f32 / span;
    let interest = Vec3::new(
        lerp(
            left.interest.x,
            right.interest.x,
            bezier::sample(right.curves[0], x),
        ),
        lerp(
            left.interest.y,
            right.interest.y,
            bezier::sample(right.curves[1], x),
        ),
        lerp(
            left.interest.z,
            right.interest.z,
            bezier::sample(right.curves[2], x),
        ),
    );
    let rotation_weight = bezier::sample(right.curves[3], x);
    let rotation = left.rotation.lerp(right.rotation, rotation_weight);
    let distance = lerp(
        left.distance,
        right.distance,
        bezier::sample(right.curves[4], x),
    );
    let fov = lerp(
        left.fov_degrees,
        right.fov_degrees,
        bezier::sample(right.curves[5], x),
    );
    Some((interest, rotation, distance, fov))
}

fn morph_weights(model: &ModelAsset, motion: Option<&Motion>, frame: FrameTime) -> Vec<f32> {
    let mut weights = vec![0.0; model.morphs.len()];
    if let Some(motion) = motion {
        for (index, morph) in model.morphs.iter().enumerate() {
            if let Some(track) = motion
                .morphs
                .iter()
                .find(|track| track.name == morph.name())
            {
                weights[index] = sample_morph(track, frame);
            }
        }
    }
    for index in 0..model.morphs.len() {
        let Morph::Group { offsets, .. } = &model.morphs[index] else {
            continue;
        };
        let weight = weights[index];
        for (target, amount) in offsets {
            let target = *target as usize;
            if target < weights.len() {
                weights[target] += weight * *amount;
            }
        }
    }
    weights
}

fn sample_morph(track: &mmd_core::MorphTrack, frame: FrameTime) -> f32 {
    let keys = &track.keys;
    if keys.is_empty() {
        return 0.0;
    }
    let frame = frame.frame();
    if frame <= keys[0].frame.frame() {
        return keys[0].weight;
    }
    if frame >= keys[keys.len() - 1].frame.frame() {
        return keys[keys.len() - 1].weight;
    }
    let right = keys.partition_point(|key| key.frame.frame() <= frame);
    let left = &keys[right - 1];
    let right = &keys[right];
    let span = (right.frame.frame() - left.frame.frame()).max(1) as f32;
    let t = (frame - left.frame.frame()) as f32 / span;
    lerp(left.weight, right.weight, t)
}

fn morphed_positions(model: &ModelAsset, weights: &[f32]) -> Vec<Vec3> {
    let mut positions: Vec<Vec3> = model
        .vertices
        .iter()
        .map(|vertex| vertex.position)
        .collect();
    for (morph, weight) in model.morphs.iter().zip(weights) {
        let Morph::Vertex { offsets, .. } = morph else {
            continue;
        };
        for (index, offset) in offsets {
            if let Some(position) = positions.get_mut(*index as usize) {
                *position += *offset * *weight;
            }
        }
    }
    positions
}

fn morphed_uvs(model: &ModelAsset, weights: &[f32]) -> Vec<Vec2> {
    let mut uvs: Vec<Vec2> = model.vertices.iter().map(|vertex| vertex.uv).collect();
    for (morph, weight) in model.morphs.iter().zip(weights) {
        let Morph::Uv {
            channel: UvChannel::Primary,
            offsets,
            ..
        } = morph
        else {
            continue;
        };
        for (index, offset) in offsets {
            if let Some(uv) = uvs.get_mut(*index as usize) {
                uv.x += offset.x * *weight;
                uv.y += offset.y * *weight;
            }
        }
    }
    uvs
}

fn morphed_materials(model: &ModelAsset, weights: &[f32]) -> Vec<MaterialFrame> {
    let mut materials: Vec<MaterialFrame> =
        model.materials.iter().map(MaterialFrame::from).collect();
    for (morph, weight) in model.morphs.iter().zip(weights) {
        let Morph::Material { offsets, .. } = morph else {
            continue;
        };
        for offset in offsets {
            let range: Vec<usize> = match offset.material {
                Some(index) => vec![index as usize],
                None => (0..materials.len()).collect(),
            };
            for index in range {
                let Some(material) = materials.get_mut(index) else {
                    continue;
                };
                apply_material(material, offset, *weight);
            }
        }
    }
    materials
}

fn apply_material(material: &mut MaterialFrame, offset: &mmd_core::MaterialOffset, weight: f32) {
    if offset.add {
        material.diffuse = add4(material.diffuse, offset.diffuse, weight);
        material.specular = add3(material.specular, offset.specular, weight);
        material.specular_power += offset.specular_power * weight;
        material.ambient = add3(material.ambient, offset.ambient, weight);
        material.edge_color = add4(material.edge_color, offset.edge_color, weight);
        material.edge_size += offset.edge_size * weight;
        material.texture = add4(material.texture, offset.texture, weight);
        material.sphere = add4(material.sphere, offset.sphere, weight);
    } else {
        material.diffuse = mul4(material.diffuse, offset.diffuse, weight);
        material.specular = mul3(material.specular, offset.specular, weight);
        material.specular_power = lerp(
            material.specular_power,
            material.specular_power * offset.specular_power,
            weight,
        );
        material.ambient = mul3(material.ambient, offset.ambient, weight);
        material.edge_color = mul4(material.edge_color, offset.edge_color, weight);
        material.edge_size = lerp(
            material.edge_size,
            material.edge_size * offset.edge_size,
            weight,
        );
        material.texture = mul4(material.texture, offset.texture, weight);
        material.sphere = mul4(material.sphere, offset.sphere, weight);
    }
}

fn local_pose(
    model: &ModelAsset,
    motion: Option<&Motion>,
    frame: FrameTime,
    morph_weights: &[f32],
) -> (Vec<Vec3>, Vec<Quat>) {
    let count = model.bones.len();
    let mut translation = vec![Vec3::ZERO; count];
    let mut rotation = vec![Quat::IDENTITY; count];
    for (index, bone) in model.bones.iter().enumerate() {
        translation[index] = rest_translation(&model.bones, index);
        if let Some(motion) = motion
            && let Some(track) = motion.bones.iter().find(|track| track.name == bone.name)
        {
            let (offset, orient) = sample_bone(track, frame);
            translation[index] += offset;
            rotation[index] = orient;
        }
    }
    for (morph, weight) in model.morphs.iter().zip(morph_weights) {
        let Morph::Bone { offsets, .. } = morph else {
            continue;
        };
        for (index, offset, orient) in offsets {
            let index = *index as usize;
            if index >= count {
                continue;
            }
            translation[index] += *offset * *weight;
            rotation[index] =
                Quat::IDENTITY.slerp(*orient, (*weight).clamp(0.0, 1.0)) * rotation[index];
        }
    }
    let disabled = disabled_ik(model, motion, frame);
    for index in deform_order(&model.bones) {
        apply_append(&model.bones, index, &mut translation, &mut rotation);
        if let Some(axis) = model.bones[index].fixed_axis {
            rotation[index] = twist_around(rotation[index], axis);
        }
        if model.bones[index].ik.is_some() && !disabled.contains(&index) {
            solve_ik(model, index, &mut translation, &mut rotation);
        }
    }
    (translation, rotation)
}

fn disabled_ik(model: &ModelAsset, motion: Option<&Motion>, frame: FrameTime) -> Vec<usize> {
    let Some(motion) = motion else {
        return Vec::new();
    };
    let mut enabled: HashMap<&str, bool> = HashMap::new();
    for key in &motion.ik {
        if key.frame.frame() > frame.frame() {
            break;
        }
        apply_ik_key(model, key, &mut enabled);
    }
    model
        .bones
        .iter()
        .enumerate()
        .filter_map(|(index, bone)| {
            if bone.ik.is_some() && enabled.get(bone.name.as_str()) == Some(&false) {
                Some(index)
            } else {
                None
            }
        })
        .collect()
}

fn apply_ik_key<'a>(
    _model: &'a ModelAsset,
    key: &'a IkKeyframe,
    enabled: &mut HashMap<&'a str, bool>,
) {
    for (name, chain_enabled) in &key.chains {
        enabled.insert(name.as_str(), *chain_enabled);
    }
}

fn sample_bone(track: &mmd_core::BoneTrack, frame: FrameTime) -> (Vec3, Quat) {
    let keys = &track.keys;
    if keys.is_empty() {
        return (Vec3::ZERO, Quat::IDENTITY);
    }
    let frame = frame.frame();
    if frame <= keys[0].frame.frame() {
        return (keys[0].translation, keys[0].rotation);
    }
    if frame >= keys[keys.len() - 1].frame.frame() {
        let last = &keys[keys.len() - 1];
        return (last.translation, last.rotation);
    }
    let right_index = keys.partition_point(|key| key.frame.frame() <= frame);
    let left = &keys[right_index - 1];
    let right = &keys[right_index];
    let span = (right.frame.frame() - left.frame.frame()).max(1) as f32;
    let x = (frame - left.frame.frame()) as f32 / span;
    let translation = Vec3::new(
        lerp(
            left.translation.x,
            right.translation.x,
            bezier::sample(right.curves[0], x),
        ),
        lerp(
            left.translation.y,
            right.translation.y,
            bezier::sample(right.curves[1], x),
        ),
        lerp(
            left.translation.z,
            right.translation.z,
            bezier::sample(right.curves[2], x),
        ),
    );
    let rotation = left
        .rotation
        .slerp(right.rotation, bezier::sample(right.curves[3], x));
    (translation, rotation)
}

fn rest_translation(bones: &[Bone], index: usize) -> Vec3 {
    match bones[index]
        .parent
        .and_then(|parent| bones.get(parent as usize))
    {
        Some(parent) => bones[index].position - parent.position,
        None => bones[index].position,
    }
}

fn deform_order(bones: &[Bone]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..bones.len()).collect();
    order.sort_by(|&left, &right| {
        bones[left]
            .layer
            .cmp(&bones[right].layer)
            .then(left.cmp(&right))
    });
    order
}

fn apply_append(bones: &[Bone], index: usize, translation: &mut [Vec3], rotation: &mut [Quat]) {
    let Some(append) = &bones[index].append else {
        return;
    };
    let parent = append.parent as usize;
    if parent >= bones.len() {
        return;
    }
    let parent_rotation = quat_pow(rotation[parent], append.rate);
    if append.local {
        rotation[index] *= parent_rotation;
    } else {
        rotation[index] = parent_rotation * rotation[index];
    }
    let parent_offset = translation[parent] - rest_translation(bones, parent);
    translation[index] += parent_offset * append.rate;
}

fn quat_pow(quat: Quat, rate: f32) -> Quat {
    let quat = quat.normalize();
    let (axis, angle) = quat.to_axis_angle();
    if !axis.is_finite() || axis.length_squared() < 1.0e-8 {
        return Quat::IDENTITY;
    }
    Quat::from_axis_angle(axis, angle * rate)
}

fn twist_around(quat: Quat, axis: Vec3) -> Quat {
    let axis = axis.normalize_or_zero();
    if axis.length_squared() < 1.0e-8 {
        return quat;
    }
    let vector = Vec3::new(quat.x, quat.y, quat.z);
    let projected = axis * vector.dot(axis);
    Quat::from_xyzw(projected.x, projected.y, projected.z, quat.w).normalize()
}

fn solve_ik(model: &ModelAsset, ik_index: usize, translation: &mut [Vec3], rotation: &mut [Quat]) {
    let Some(ik) = &model.bones[ik_index].ik else {
        return;
    };
    let effector = ik.target as usize;
    if effector >= model.bones.len() {
        return;
    }
    let limit = ik.limit_angle.max(0.0);
    let children = child_lists(&model.bones);
    let mut world = world_matrices(&model.bones, translation, rotation);
    let mut seen = vec![false; model.bones.len()];
    for _ in 0..ik.iterations {
        for link in &ik.links {
            let link_index = link.bone as usize;
            if link_index >= model.bones.len() {
                continue;
            }
            let link_pos = world[link_index].transform_point3(Vec3::ZERO);
            let effector_pos = world[effector].transform_point3(Vec3::ZERO);
            let goal_pos = world[ik_index].transform_point3(Vec3::ZERO);
            let to_effector = effector_pos - link_pos;
            let to_goal = goal_pos - link_pos;
            if to_effector.length_squared() < 1.0e-8 || to_goal.length_squared() < 1.0e-8 {
                continue;
            }
            let delta = Quat::from_rotation_arc(to_effector.normalize(), to_goal.normalize());
            let (axis, angle) = delta.to_axis_angle();
            let angle = angle.min(limit);
            if angle < 1.0e-5 || !axis.is_finite() {
                continue;
            }
            let delta = Quat::from_axis_angle(axis, angle);
            let parent_rotation = parent_world_rotation(&model.bones, &world, link_index);
            let world_rotation = Quat::from_mat4(&world[link_index]);
            let mut local = parent_rotation.inverse() * delta * world_rotation;
            if let Some((min_limit, max_limit)) = link.limits {
                let (x, _, _) = local.to_euler(EulerRot::XYZ);
                local =
                    Quat::from_euler(EulerRot::XYZ, x.clamp(min_limit.x, max_limit.x), 0.0, 0.0);
            }
            rotation[link_index] = local.normalize();
            refresh_world(
                &model.bones,
                &children,
                translation,
                rotation,
                &mut world,
                &mut seen,
                link_index,
            );
        }
    }
}

fn child_lists(bones: &[Bone]) -> Vec<Vec<usize>> {
    let mut children = vec![Vec::new(); bones.len()];
    for (index, bone) in bones.iter().enumerate() {
        let Some(parent) = bone.parent else {
            continue;
        };
        let parent = parent as usize;
        if parent < bones.len() && parent != index {
            children[parent].push(index);
        }
    }
    children
}

fn refresh_world(
    bones: &[Bone],
    children: &[Vec<usize>],
    translation: &[Vec3],
    rotation: &[Quat],
    world: &mut [Mat4],
    seen: &mut [bool],
    root: usize,
) {
    seen.fill(false);
    let mut stack = vec![root];
    let mut guard = 0usize;
    while let Some(index) = stack.pop() {
        if index >= bones.len() || seen[index] {
            continue;
        }
        seen[index] = true;
        guard += 1;
        if guard > bones.len() {
            break;
        }
        let local = Mat4::from_rotation_translation(rotation[index], translation[index]);
        world[index] = match bones[index]
            .parent
            .and_then(|parent| world.get(parent as usize).copied())
        {
            Some(parent) => parent * local,
            None => local,
        };
        stack.extend_from_slice(&children[index]);
    }
}

fn parent_world_rotation(bones: &[Bone], world: &[Mat4], index: usize) -> Quat {
    match bones[index]
        .parent
        .and_then(|parent| world.get(parent as usize))
    {
        Some(parent) => Quat::from_mat4(parent),
        None => Quat::IDENTITY,
    }
}

fn skin_from_local(
    model: &ModelAsset,
    translation: &[Vec3],
    rotation: &[Quat],
) -> (Vec<Mat4>, Vec<Mat4>) {
    let rest_t: Vec<Vec3> = (0..model.bones.len())
        .map(|index| rest_translation(&model.bones, index))
        .collect();
    let rest_r = vec![Quat::IDENTITY; model.bones.len()];
    let rest = world_matrices(&model.bones, &rest_t, &rest_r);
    let world = world_matrices(model.bones.as_slice(), translation, rotation);
    let skin = rest
        .iter()
        .zip(&world)
        .map(|(rest, world)| *world * rest.inverse())
        .collect();
    (world, skin)
}

fn world_matrices(bones: &[Bone], translation: &[Vec3], rotation: &[Quat]) -> Vec<Mat4> {
    let mut world = vec![Mat4::IDENTITY; bones.len()];
    for index in parent_order(bones) {
        let local = Mat4::from_rotation_translation(rotation[index], translation[index]);
        world[index] = match bones[index]
            .parent
            .and_then(|parent| world.get(parent as usize).copied())
        {
            Some(parent) => parent * local,
            None => local,
        };
    }
    world
}

fn parent_order(bones: &[Bone]) -> Vec<usize> {
    let mut depth = vec![0u32; bones.len()];
    for index in 0..bones.len() {
        let mut cursor = bones[index].parent;
        let mut seen = vec![false; bones.len()];
        seen[index] = true;
        let mut steps = 0u32;
        while let Some(parent) = cursor {
            if parent as usize >= bones.len() || seen[parent as usize] {
                break;
            }
            seen[parent as usize] = true;
            steps += 1;
            cursor = bones[parent as usize].parent;
            if steps as usize > bones.len() {
                break;
            }
        }
        depth[index] = steps;
    }
    let mut order: Vec<usize> = (0..bones.len()).collect();
    order.sort_by_key(|&index| (depth[index], index));
    order
}

fn lerp(left: f32, right: f32, weight: f32) -> f32 {
    left + (right - left) * weight
}

fn add4(value: [f32; 4], offset: [f32; 4], weight: f32) -> [f32; 4] {
    [
        value[0] + offset[0] * weight,
        value[1] + offset[1] * weight,
        value[2] + offset[2] * weight,
        value[3] + offset[3] * weight,
    ]
}

fn add3(value: [f32; 3], offset: [f32; 3], weight: f32) -> [f32; 3] {
    [
        value[0] + offset[0] * weight,
        value[1] + offset[1] * weight,
        value[2] + offset[2] * weight,
    ]
}

fn mul4(value: [f32; 4], factor: [f32; 4], weight: f32) -> [f32; 4] {
    [
        lerp(value[0], value[0] * factor[0], weight),
        lerp(value[1], value[1] * factor[1], weight),
        lerp(value[2], value[2] * factor[2], weight),
        lerp(value[3], value[3] * factor[3], weight),
    ]
}

fn mul3(value: [f32; 3], factor: [f32; 3], weight: f32) -> [f32; 3] {
    [
        lerp(value[0], value[0] * factor[0], weight),
        lerp(value[1], value[1] * factor[1], weight),
        lerp(value[2], value[2] * factor[2], weight),
    ]
}

impl From<&Material> for MaterialFrame {
    fn from(material: &Material) -> Self {
        Self {
            diffuse: material.diffuse,
            specular: material.specular,
            specular_power: material.specular_power,
            ambient: material.ambient,
            edge_color: material.edge_color,
            edge_size: material.edge_size,
            texture: [1.0, 1.0, 1.0, 1.0],
            sphere: [1.0, 1.0, 1.0, 1.0],
            double_sided: material.double_sided(),
            ground_shadow: material.ground_shadow(),
            cast_shadow: material.cast_shadow(),
            receive_shadow: material.receive_shadow(),
            draw_edge: material.draw_edge(),
        }
    }
}

/// Skin one vertex with the evaluated bone matrices.
pub fn skin_vertex(position: Vec3, skinning: &Skinning, skin: &[Mat4]) -> Vec3 {
    match *skinning {
        Skinning::Bdef1 { bone } => bdef_one(position, bone, skin),
        Skinning::Bdef2 {
            bone0,
            bone1,
            weight0,
        } => skin::bdef(position, &[bone0, bone1], &[weight0, 1.0 - weight0], skin),
        Skinning::Bdef4 { bones, weights } => skin::bdef(position, &bones, &weights, skin),
        Skinning::Sdef {
            bone0,
            bone1,
            weight0,
            c,
            r0,
            r1,
        } => skin::sdef(
            position,
            weight0,
            bone0 as usize,
            bone1 as usize,
            c,
            r0,
            r1,
            skin,
        ),
        Skinning::Qdef { bones, weights } => skin::qdef(position, &bones, &weights, skin),
    }
}

fn bdef_one(position: Vec3, bone: u16, skin: &[Mat4]) -> Vec3 {
    skin[usize::from(bone)].transform_point3(position)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mmd_core::{Append, Bezier, BoneKeyframe, BoneTrack, MorphTrack, Skinning, Vertex};

    fn bone(name: &str, position: Vec3, parent: Option<u16>) -> Bone {
        Bone {
            name: name.to_string(),
            english_name: String::new(),
            position,
            parent,
            layer: 0,
            flags: 0,
            tail: mmd_core::BoneTail::Offset(Vec3::Y),
            append: None,
            fixed_axis: None,
            local_axis: None,
            external_parent: None,
            ik: None,
        }
    }

    fn empty_model(bones: Vec<Bone>, vertices: Vec<Vertex>) -> ModelAsset {
        ModelAsset {
            name: String::new(),
            english_name: String::new(),
            comment: String::new(),
            vertices,
            indices: Vec::new(),
            textures: Vec::new(),
            materials: Vec::new(),
            bones,
            morphs: Vec::new(),
            display_frames: Vec::new(),
            rigid_bodies: Vec::new(),
            joints: Vec::new(),
        }
    }

    #[test]
    fn child_translation_is_relative_to_its_parent() {
        let model = empty_model(
            vec![
                bone("root", Vec3::ZERO, None),
                bone("child", Vec3::Y, Some(0)),
            ],
            vec![Vertex {
                position: Vec3::Y,
                normal: Vec3::Y,
                uv: Vec2::ZERO,
                skin: Skinning::Bdef1 { bone: 1 },
                edge_scale: 1.0,
            }],
        );
        let motion = Motion {
            model_name: String::new(),
            bones: vec![BoneTrack {
                name: "child".to_string(),
                keys: vec![BoneKeyframe {
                    frame: FrameTime::new(0),
                    translation: Vec3::X,
                    rotation: Quat::IDENTITY,
                    curves: [Bezier::LINEAR; 4],
                }],
            }],
            morphs: Vec::new(),
            cameras: Vec::new(),
            lights: Vec::new(),
            shadows: Vec::new(),
            ik: Vec::new(),
        };
        let frame = evaluate(&model, Some(&motion), FrameTime::ZERO);
        let skinned = skin_vertex(Vec3::Y, &model.vertices[0].skin, &frame.skin);
        assert!(
            skinned.abs_diff_eq(Vec3::new(1.0, 1.0, 0.0), 1.0e-4),
            "{skinned}"
        );
    }

    #[test]
    fn append_grants_the_parent_rotation() {
        let mut child = bone("child", Vec3::Y, Some(0));
        child.append = Some(Append {
            parent: 0,
            rate: 1.0,
            local: false,
        });
        let model = empty_model(vec![bone("root", Vec3::ZERO, None), child], Vec::new());
        let motion = Motion {
            model_name: String::new(),
            bones: vec![BoneTrack {
                name: "root".to_string(),
                keys: vec![BoneKeyframe {
                    frame: FrameTime::ZERO,
                    translation: Vec3::ZERO,
                    rotation: Quat::from_rotation_y(std::f32::consts::FRAC_PI_2),
                    curves: [Bezier::LINEAR; 4],
                }],
            }],
            morphs: Vec::new(),
            cameras: Vec::new(),
            lights: Vec::new(),
            shadows: Vec::new(),
            ik: Vec::new(),
        };
        let frame = evaluate(&model, Some(&motion), FrameTime::ZERO);
        let child_rot = frame.bone_rotation[1];
        let expected = Quat::from_rotation_y(std::f32::consts::PI);
        assert!(child_rot.dot(expected).abs() > 0.99, "{child_rot}");
    }

    #[test]
    fn fixed_axis_drops_rotation_off_the_axis() {
        let mut axis_bone = bone("axis", Vec3::ZERO, None);
        axis_bone.fixed_axis = Some(Vec3::Y);
        let model = empty_model(vec![axis_bone], Vec::new());
        let motion = Motion {
            model_name: String::new(),
            bones: vec![BoneTrack {
                name: "axis".to_string(),
                keys: vec![BoneKeyframe {
                    frame: FrameTime::ZERO,
                    translation: Vec3::ZERO,
                    rotation: Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
                    curves: [Bezier::LINEAR; 4],
                }],
            }],
            morphs: Vec::new(),
            cameras: Vec::new(),
            lights: Vec::new(),
            shadows: Vec::new(),
            ik: Vec::new(),
        };
        let frame = evaluate(&model, Some(&motion), FrameTime::ZERO);
        assert!(frame.bone_rotation[0].dot(Quat::IDENTITY).abs() > 0.99);
    }

    #[test]
    fn vertex_morph_moves_the_named_vertex() {
        let mut model = empty_model(
            vec![bone("root", Vec3::ZERO, None)],
            vec![Vertex {
                position: Vec3::ZERO,
                normal: Vec3::Y,
                uv: Vec2::ZERO,
                skin: Skinning::Bdef1 { bone: 0 },
                edge_scale: 1.0,
            }],
        );
        model.morphs.push(Morph::Vertex {
            name: "smile".to_string(),
            english_name: String::new(),
            panel: 1,
            offsets: vec![(0, Vec3::X)],
        });
        let motion = Motion {
            model_name: String::new(),
            bones: Vec::new(),
            morphs: vec![MorphTrack {
                name: "smile".to_string(),
                keys: vec![mmd_core::MorphKeyframe {
                    frame: FrameTime::ZERO,
                    weight: 0.5,
                }],
            }],
            cameras: Vec::new(),
            lights: Vec::new(),
            shadows: Vec::new(),
            ik: Vec::new(),
        };
        let frame = evaluate(&model, Some(&motion), FrameTime::ZERO);
        assert!(frame.positions[0].abs_diff_eq(Vec3::new(0.5, 0.0, 0.0), 1.0e-5));
    }

    #[test]
    fn ik_moves_the_effector_toward_the_goal() {
        let mut ik_bone = bone("ik", Vec3::new(1.0, 2.0, 0.0), None);
        ik_bone.ik = Some(mmd_core::Ik {
            target: 2,
            iterations: 12,
            limit_angle: 1.0,
            links: vec![
                mmd_core::IkLink {
                    bone: 1,
                    limits: None,
                },
                mmd_core::IkLink {
                    bone: 0,
                    limits: None,
                },
            ],
        });
        let model = empty_model(
            vec![
                bone("root", Vec3::ZERO, None),
                bone("mid", Vec3::Y, Some(0)),
                bone("end", Vec3::new(0.0, 2.0, 0.0), Some(1)),
                ik_bone,
            ],
            Vec::new(),
        );
        let frame = evaluate(&model, None, FrameTime::ZERO);
        assert!(
            frame.bone_translation[2].x > 0.2,
            "effector {:?}",
            frame.bone_translation[2]
        );
    }

    #[test]
    fn identity_camera_with_negative_distance_sits_on_positive_z() {
        let model = empty_model(Vec::new(), Vec::new());
        let motion = Motion {
            model_name: String::new(),
            bones: Vec::new(),
            morphs: Vec::new(),
            cameras: vec![CameraKeyframe {
                frame: FrameTime::ZERO,
                interest: Vec3::ZERO,
                rotation: Vec3::ZERO,
                distance: -10.0,
                fov_degrees: 30.0,
                perspective: true,
                curves: [Bezier::LINEAR; 6],
            }],
            lights: Vec::new(),
            shadows: Vec::new(),
            ik: Vec::new(),
        };
        let frame = evaluate(&model, Some(&motion), FrameTime::ZERO);
        let camera = frame.camera.expect("camera");
        assert!(camera.eye.abs_diff_eq(Vec3::new(0.0, 0.0, 10.0), 1.0e-4));
        assert!((camera.fov_y_radians - 30.0_f32.to_radians()).abs() < 1.0e-4);
    }
}
