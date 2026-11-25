//! Vertex skinning formulas shared by the tests and the renderer.
//!
//! The SDEF result is the blended transformed center, plus the blended
//! rotation of `position - c`, plus the difference between the blended
//! transformed R point and the same point predicted by that rotation.
//! When both bone matrices are the same rigid transform, including identity,
//! the extra term is zero and the result is that transform applied to the
//! position. This has not been compared numerically with the original
//! application.

use glam::{Mat4, Quat, Vec3};

/// Blend up to four skin matrices.
pub fn bdef(position: Vec3, bones: &[u16], weights: &[f32], skin: &[Mat4]) -> Vec3 {
    let mut result = Vec3::ZERO;
    for (bone, weight) in bones.iter().zip(weights) {
        if *weight == 0.0 {
            continue;
        }
        result += skin[usize::from(*bone)].transform_point3(position) * *weight;
    }
    result
}

/// SDEF between two rigid bone matrices.
#[allow(clippy::too_many_arguments)] // The SDEF points and the two bones are independent inputs.
pub fn sdef(
    position: Vec3,
    weight0: f32,
    bone0: usize,
    bone1: usize,
    c: Vec3,
    r0: Vec3,
    r1: Vec3,
    skin: &[Mat4],
) -> Vec3 {
    let weight1 = 1.0 - weight0;
    let m0 = skin[bone0];
    let m1 = skin[bone1];
    let rot0 = Quat::from_mat4(&m0);
    let rot1 = Quat::from_mat4(&m1);
    let rotation = rot0.slerp(rot1, weight1);
    let center = m0.transform_point3(c) * weight0 + m1.transform_point3(c) * weight1;
    let blended_r = m0.transform_point3(r0) * weight0 + m1.transform_point3(r1) * weight1;
    let rest_r = r0 * weight0 + r1 * weight1;
    let predicted = center + rotation * (rest_r - c);
    center + rotation * (position - c) + (blended_r - predicted)
}

/// Dual-quaternion blend. A weight of 0 or 1 matches that bone's rigid transform.
pub fn qdef(position: Vec3, bones: &[u16], weights: &[f32], skin: &[Mat4]) -> Vec3 {
    let mut real = glam::Vec4::ZERO;
    let mut dual = glam::Vec4::ZERO;
    let mut first: Option<glam::Vec4> = None;
    for (bone, weight) in bones.iter().zip(weights) {
        if *weight == 0.0 {
            continue;
        }
        let matrix = skin[usize::from(*bone)];
        let mut rotation = Quat::from_mat4(&matrix);
        let translation = matrix.transform_point3(Vec3::ZERO);
        if let Some(anchor) = first
            && rotation.dot(Quat::from_vec4(anchor)) < 0.0
        {
            rotation = -rotation;
        }
        let rotation_v = rotation.to_array();
        let rotation_q =
            glam::Vec4::from_array([rotation_v[0], rotation_v[1], rotation_v[2], rotation_v[3]]);
        if first.is_none() {
            first = Some(rotation_q);
        }
        let translation_q = glam::Vec4::new(translation.x, translation.y, translation.z, 0.0);
        let dual_q = quat_mul(translation_q, rotation_q) * 0.5;
        real += rotation_q * *weight;
        dual += dual_q * *weight;
    }
    let length = real.length();
    if length < 1.0e-8 {
        return position;
    }
    real /= length;
    dual /= length;
    let translation_q = quat_mul(dual, quat_conj(real)) * 2.0;
    let rotation = Quat::from_vec4(real);
    rotation * position + Vec3::new(translation_q.x, translation_q.y, translation_q.z)
}

fn quat_mul(a: glam::Vec4, b: glam::Vec4) -> glam::Vec4 {
    glam::Vec4::new(
        a.w * b.x + a.x * b.w + a.y * b.z - a.z * b.y,
        a.w * b.y - a.x * b.z + a.y * b.w + a.z * b.x,
        a.w * b.z + a.x * b.y - a.y * b.x + a.z * b.w,
        a.w * b.w - a.x * b.x - a.y * b.y - a.z * b.z,
    )
}

fn quat_conj(q: glam::Vec4) -> glam::Vec4 {
    glam::Vec4::new(-q.x, -q.y, -q.z, q.w)
}

/// Project `position` onto Y = 0 along `light_direction`.
///
/// `light_direction` is the direction the light travels. A downward light has
/// a negative Y component.
pub fn project_onto_ground(position: Vec3, light_direction: Vec3) -> Vec3 {
    if light_direction.y.abs() < 1.0e-6 {
        return position;
    }
    let t = -position.y / light_direction.y;
    position + light_direction * t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bdef_weight_one_uses_the_first_bone() {
        let moved = Mat4::from_translation(Vec3::new(2.0, 0.0, 0.0));
        let point = bdef(
            Vec3::new(1.0, 0.0, 0.0),
            &[0, 1],
            &[1.0, 0.0],
            &[moved, Mat4::IDENTITY],
        );
        assert!(point.abs_diff_eq(Vec3::new(3.0, 0.0, 0.0), 1.0e-4));
    }

    #[test]
    fn sdef_matches_a_single_rigid_bone_and_identity() {
        let identity = [Mat4::IDENTITY, Mat4::IDENTITY];
        let point = Vec3::new(0.2, 1.0, -0.4);
        let c = Vec3::new(0.0, 1.0, 0.0);
        let same = sdef(point, 0.3, 0, 1, c, Vec3::Y, Vec3::X, &identity);
        assert!(same.abs_diff_eq(point, 1.0e-4));

        let moved =
            Mat4::from_rotation_translation(Quat::from_rotation_y(0.4), Vec3::new(1.0, 2.0, 3.0));
        let both = [moved, moved];
        let skinned = sdef(point, 0.25, 0, 1, c, c, c, &both);
        assert!(skinned.abs_diff_eq(moved.transform_point3(point), 1.0e-4));

        let only_first = sdef(point, 1.0, 0, 1, c, c, Vec3::ZERO, &[moved, Mat4::IDENTITY]);
        assert!(only_first.abs_diff_eq(moved.transform_point3(point), 1.0e-4));
        let only_second = sdef(point, 0.0, 0, 1, c, Vec3::ZERO, c, &[Mat4::IDENTITY, moved]);
        assert!(only_second.abs_diff_eq(moved.transform_point3(point), 1.0e-4));
    }

    #[test]
    fn qdef_endpoints_match_the_rigid_bones() {
        let moved =
            Mat4::from_rotation_translation(Quat::from_rotation_x(0.5), Vec3::new(0.0, 1.0, 2.0));
        let point = Vec3::new(1.0, 0.0, 0.0);
        let first = qdef(point, &[0, 1], &[1.0, 0.0], &[moved, Mat4::IDENTITY]);
        let second = qdef(point, &[0, 1], &[0.0, 1.0], &[Mat4::IDENTITY, moved]);
        assert!(first.abs_diff_eq(moved.transform_point3(point), 1.0e-4));
        assert!(second.abs_diff_eq(moved.transform_point3(point), 1.0e-4));
    }

    #[test]
    fn ground_projection_follows_the_light() {
        let projected = project_onto_ground(Vec3::new(1.0, 2.0, 3.0), Vec3::new(0.0, -1.0, 0.0));
        assert!(projected.abs_diff_eq(Vec3::new(1.0, 0.0, 3.0), 1.0e-5));
    }
}
