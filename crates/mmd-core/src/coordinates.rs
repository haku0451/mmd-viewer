//! Model space and the single clip-space conversion.
//!
//! Model space matches the PMX/PMD convention used by the parsers: left-handed
//! and Y-up. A model faces -Z, and +X is the model's left. Bone quaternions
//! and positions stay in that space. The editor camera uses `look_at_lh` and
//! sits on -Z so the default view is the face.
//!
//! [`wgpu_clip_from_view`] is the only projection. It uses glam's
//! `perspective_lh`, whose depth range is already wgpu's 0 to 1. From an eye
//! on -Z that matrix puts world +X on the right of the screen, which is the
//! model's left.
//!
//! This has not been compared numerically with the original MikuMikuDance
//! application. The handedness follows the published PMX description and is
//! covered by the projection tests in this module.

use glam::{Mat4, Vec4};

/// Maps an OpenGL clip matrix into wgpu's 0-to-1 depth range.
///
/// Retained for callers that still hold an OpenGL-style projection. The MMD
/// viewport path does not use it.
pub const WGPU_FROM_OPENGL_CLIP: Mat4 = Mat4::from_cols(
    Vec4::new(1.0, 0.0, 0.0, 0.0),
    Vec4::new(0.0, 1.0, 0.0, 0.0),
    Vec4::new(0.0, 0.0, 0.5, 0.0),
    Vec4::new(0.0, 0.0, 0.5, 1.0),
);

/// Finite perspective settings accepted by [`wgpu_clip_from_view`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProjectionParams {
    /// Vertical field of view in radians.
    pub fov_y_radians: f32,
    /// Width divided by height. Must be positive.
    pub aspect: f32,
    /// Positive distance to the near plane.
    pub z_near: f32,
    /// Positive distance to the far plane, greater than `z_near`.
    pub z_far: f32,
}

/// Rejected camera or projection values.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum CoordinateError {
    #[error("a coordinate or projection value was not finite")]
    NonFinite,
    #[error("vertical field of view must be between 0 and PI radians")]
    InvalidFieldOfView,
    #[error("viewport aspect ratio must be positive")]
    InvalidAspect,
    #[error("depth range must satisfy 0 < near < far")]
    InvalidDepthRange,
}

/// Validate a perspective projection before building a matrix.
pub fn validate_projection(
    fov_y_radians: f32,
    aspect: f32,
    z_near: f32,
    z_far: f32,
) -> Result<ProjectionParams, CoordinateError> {
    if !fov_y_radians.is_finite()
        || !aspect.is_finite()
        || !z_near.is_finite()
        || !z_far.is_finite()
    {
        return Err(CoordinateError::NonFinite);
    }
    if fov_y_radians <= 0.0 || fov_y_radians >= std::f32::consts::PI {
        return Err(CoordinateError::InvalidFieldOfView);
    }
    if aspect <= 0.0 {
        return Err(CoordinateError::InvalidAspect);
    }
    if z_near <= 0.0 || z_far <= z_near {
        return Err(CoordinateError::InvalidDepthRange);
    }
    Ok(ProjectionParams {
        fov_y_radians,
        aspect,
        z_near,
        z_far,
    })
}

/// Build a wgpu clip matrix from a left-handed view.
///
/// `view` transforms model space into the camera's left-handed view space.
pub fn wgpu_clip_from_view(
    view: Mat4,
    fov_y_radians: f32,
    aspect: f32,
    z_near: f32,
    z_far: f32,
) -> Result<Mat4, CoordinateError> {
    let params = validate_projection(fov_y_radians, aspect, z_near, z_far)?;
    if !view.is_finite() {
        return Err(CoordinateError::NonFinite);
    }
    let projection = Mat4::perspective_lh(
        params.fov_y_radians,
        params.aspect,
        params.z_near,
        params.z_far,
    );
    let clip_from_world = projection * view;
    if !clip_from_world.is_finite() {
        return Err(CoordinateError::NonFinite);
    }
    Ok(clip_from_world)
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;

    fn front_view() -> Mat4 {
        Mat4::look_at_lh(Vec3::new(0.0, 0.0, -5.0), Vec3::ZERO, Vec3::Y)
    }

    #[test]
    fn origin_in_front_of_camera_lands_inside_wgpu_depth() {
        let clip = wgpu_clip_from_view(front_view(), 60_f32.to_radians(), 1.0, 0.1, 100.0)
            .expect("valid projection");
        let point = clip * Vec4::new(0.0, 0.0, 0.0, 1.0);
        assert!(point.w > 0.0);
        let ndc_z = point.z / point.w;
        assert!((0.0..=1.0).contains(&ndc_z), "ndc z {ndc_z}");
    }

    #[test]
    fn positive_world_axes_keep_their_screen_directions() {
        let clip = wgpu_clip_from_view(front_view(), 60_f32.to_radians(), 1.0, 0.1, 100.0)
            .expect("valid projection");
        let right = clip * Vec4::new(1.0, 0.0, 0.0, 1.0);
        let up = clip * Vec4::new(0.0, 1.0, 0.0, 1.0);
        assert!(right.x / right.w > 0.0);
        assert!(up.y / up.w > 0.0);
    }

    #[test]
    fn projection_rejects_non_finite_and_invalid_ranges() {
        assert_eq!(
            validate_projection(f32::NAN, 1.0, 0.1, 10.0),
            Err(CoordinateError::NonFinite)
        );
        assert_eq!(
            validate_projection(0.0, 1.0, 0.1, 10.0),
            Err(CoordinateError::InvalidFieldOfView)
        );
        assert_eq!(
            validate_projection(1.0, 0.0, 0.1, 10.0),
            Err(CoordinateError::InvalidAspect)
        );
        assert_eq!(
            validate_projection(1.0, 1.0, 10.0, 0.1),
            Err(CoordinateError::InvalidDepthRange)
        );
    }
}
