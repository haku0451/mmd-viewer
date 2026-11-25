//! Orbit camera for the editor viewport.
//!
//! The camera lives in left-handed model space. Yaw 0 places the eye on -Z,
//! looking at the face of an MMD model. It is not MikuMikuDance camera animation.

use glam::{Mat4, Vec3};
use mmd_core::{CoordinateError, wgpu_clip_from_view};

const MIN_DISTANCE: f32 = 0.25;
const MAX_DISTANCE: f32 = 400.0;
const MIN_PITCH: f32 = -89.0_f32.to_radians();
const MAX_PITCH: f32 = 89.0_f32.to_radians();
const DEFAULT_DISTANCE: f32 = 12.0;
const DEFAULT_PITCH: f32 = 20.0_f32.to_radians();
const DEFAULT_FOV_Y: f32 = 45.0_f32.to_radians();
const DEFAULT_Z_NEAR: f32 = 0.05;
const DEFAULT_Z_FAR: f32 = 200.0;
const ORBIT_RADIANS_PER_PIXEL: f32 = 0.005;
const ZOOM_PER_PIXEL: f32 = 0.0015;

/// Pointer button that can move the editor camera.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CameraPointerButton {
    /// Right button. Orbits around the target.
    Secondary,
    /// Middle button. Pans the target in the view plane.
    Middle,
}

/// A pointer drag expressed in logical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CameraDrag {
    pub button: CameraPointerButton,
    /// Horizontal drag. Positive is to the right.
    pub delta_x_px: f32,
    /// Vertical drag. Positive is downward, matching egui.
    pub delta_y_px: f32,
}

/// Camera manipulation produced by the viewport widget.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CameraInput {
    Drag(CameraDrag),
    /// Mouse-wheel movement. Positive zooms toward the target.
    Scroll {
        /// Wheel delta in logical pixels.
        delta_y_px: f32,
    },
    /// Restore the default framing.
    Reset,
}

/// Failure while updating or projecting the editor camera.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum EditorError {
    /// Projection settings were rejected by `mmd-core`.
    #[error(transparent)]
    Coordinates(#[from] CoordinateError),
    #[error("camera input contained a non-finite value")]
    NonFiniteInput,
    #[error("camera view basis is degenerate")]
    DegenerateBasis,
    #[error("viewport height must be positive to pan")]
    InvalidViewport,
}

/// Orbit camera stored in left-handed model space.
#[derive(Clone, Debug)]
pub struct EditorCamera {
    target: Vec3,
    yaw: f32,
    pitch: f32,
    distance: f32,
    fov_y_radians: f32,
    z_near: f32,
    z_far: f32,
}

impl Default for EditorCamera {
    fn default() -> Self {
        Self {
            target: Vec3::ZERO,
            yaw: 0.0,
            pitch: DEFAULT_PITCH,
            distance: DEFAULT_DISTANCE,
            fov_y_radians: DEFAULT_FOV_Y,
            z_near: DEFAULT_Z_NEAR,
            z_far: DEFAULT_Z_FAR,
        }
    }
}

impl EditorCamera {
    pub fn target(&self) -> Vec3 {
        self.target
    }

    /// Yaw in radians. Zero places the eye on the world's -Z side.
    pub fn yaw(&self) -> f32 {
        self.yaw
    }

    /// Pitch in radians, clamped away from the world up axis.
    pub fn pitch(&self) -> f32 {
        self.pitch
    }

    pub fn distance(&self) -> f32 {
        self.distance
    }

    /// Vertical field of view in radians.
    pub fn fov_y_radians(&self) -> f32 {
        self.fov_y_radians
    }

    /// Apply one viewport input.
    ///
    /// `viewport_height_px` is the logical height of the viewport widget and is
    /// used to keep pan speed stable as the window changes size.
    pub fn apply_input(
        &mut self,
        input: CameraInput,
        viewport_height_px: f32,
    ) -> Result<(), EditorError> {
        match input {
            CameraInput::Reset => {
                *self = Self::default();
                Ok(())
            }
            CameraInput::Scroll { delta_y_px } => self.zoom(delta_y_px),
            CameraInput::Drag(drag) => match drag.button {
                CameraPointerButton::Secondary => {
                    self.orbit_pixels(drag.delta_x_px, drag.delta_y_px)
                }
                CameraPointerButton::Middle => {
                    self.pan_pixels(drag.delta_x_px, drag.delta_y_px, viewport_height_px)
                }
            },
        }
    }

    /// Eye position in internal world space.
    pub fn eye(&self) -> Vec3 {
        let horizontal = self.pitch.cos();
        let offset = Vec3::new(
            self.yaw.sin() * horizontal,
            self.pitch.sin(),
            -self.yaw.cos() * horizontal,
        ) * self.distance;
        self.target + offset
    }

    /// Left-handed view matrix for the current eye and target.
    pub fn view_matrix(&self) -> Result<Mat4, EditorError> {
        let eye = self.eye();
        let view = Mat4::look_at_lh(eye, self.target, Vec3::Y);
        if !view.is_finite() {
            return Err(EditorError::DegenerateBasis);
        }
        Ok(view)
    }

    /// Clip matrix for a viewport with the given width/height aspect.
    pub fn clip_from_world(&self, aspect: f32) -> Result<Mat4, EditorError> {
        let view = self.view_matrix()?;
        // A fixed far plane clips the far side of a model once the camera
        // pulls back, and those missing fragments read as black patches.
        let near = (self.distance * 0.02).clamp(self.z_near, 4.0);
        let far = (self.distance * 8.0).max(self.z_far).max(400.0);
        wgpu_clip_from_view(view, self.fov_y_radians, aspect, near, far).map_err(EditorError::from)
    }

    fn orbit_pixels(&mut self, delta_x_px: f32, delta_y_px: f32) -> Result<(), EditorError> {
        if !delta_x_px.is_finite() || !delta_y_px.is_finite() {
            return Err(EditorError::NonFiniteInput);
        }
        self.yaw -= delta_x_px * ORBIT_RADIANS_PER_PIXEL;
        self.pitch =
            (self.pitch - delta_y_px * ORBIT_RADIANS_PER_PIXEL).clamp(MIN_PITCH, MAX_PITCH);
        self.normalize_yaw();
        Ok(())
    }

    fn pan_pixels(
        &mut self,
        delta_x_px: f32,
        delta_y_px: f32,
        viewport_height_px: f32,
    ) -> Result<(), EditorError> {
        if !delta_x_px.is_finite() || !delta_y_px.is_finite() || !viewport_height_px.is_finite() {
            return Err(EditorError::NonFiniteInput);
        }
        if viewport_height_px <= 0.0 {
            return Err(EditorError::InvalidViewport);
        }
        let eye = self.eye();
        let forward = (self.target - eye).normalize();
        let Some(right) = forward.cross(Vec3::Y).try_normalize() else {
            return Err(EditorError::DegenerateBasis);
        };
        let up = right.cross(forward).normalize();
        if !forward.is_finite() || !up.is_finite() {
            return Err(EditorError::DegenerateBasis);
        }
        let half_height = self.distance * (self.fov_y_radians * 0.5).tan();
        let pixels_per_world = viewport_height_px / (2.0 * half_height);
        if !pixels_per_world.is_finite() || pixels_per_world <= 0.0 {
            return Err(EditorError::DegenerateBasis);
        }
        // Dragging the scene to the right moves the target toward screen-right.
        let world_x = -delta_x_px / pixels_per_world;
        let world_y = delta_y_px / pixels_per_world;
        let target = self.target + right * world_x + up * world_y;
        if !target.is_finite() {
            return Err(EditorError::DegenerateBasis);
        }
        self.target = target;
        Ok(())
    }

    fn zoom(&mut self, delta_y_px: f32) -> Result<(), EditorError> {
        if !delta_y_px.is_finite() {
            return Err(EditorError::NonFiniteInput);
        }
        let factor = (-delta_y_px * ZOOM_PER_PIXEL).exp();
        if !factor.is_finite() {
            return Err(EditorError::NonFiniteInput);
        }
        self.distance = (self.distance * factor).clamp(MIN_DISTANCE, MAX_DISTANCE);
        Ok(())
    }

    fn normalize_yaw(&mut self) {
        let tau = std::f32::consts::TAU;
        self.yaw = self.yaw.rem_euclid(tau);
        if self.yaw > std::f32::consts::PI {
            self.yaw -= tau;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(left: f32, right: f32) -> bool {
        (left - right).abs() < 1.0e-4
    }

    #[test]
    fn default_camera_sits_on_negative_z_and_looks_toward_positive_z() {
        let camera = EditorCamera::default();
        let eye = camera.eye();
        assert!(close(eye.x, 0.0));
        assert!(eye.y > 0.0);
        assert!(eye.z < camera.target().z);
        let forward = (camera.target() - eye).normalize();
        assert!(forward.z > 0.0);
        assert!(close(eye.distance(camera.target()), camera.distance()));
    }

    #[test]
    fn right_drag_decreases_yaw_and_extreme_pitch_is_clamped() {
        let mut camera = EditorCamera::default();
        let yaw_before = camera.yaw();
        camera
            .apply_input(
                CameraInput::Drag(CameraDrag {
                    button: CameraPointerButton::Secondary,
                    delta_x_px: 20.0,
                    delta_y_px: 0.0,
                }),
                400.0,
            )
            .expect("orbit");
        assert!(camera.yaw() < yaw_before);

        camera
            .apply_input(
                CameraInput::Drag(CameraDrag {
                    button: CameraPointerButton::Secondary,
                    delta_x_px: 0.0,
                    delta_y_px: -100_000.0,
                }),
                400.0,
            )
            .expect("pitch clamp");
        assert!(close(camera.pitch(), MAX_PITCH));
    }

    #[test]
    fn positive_scroll_zooms_in_and_distance_stays_inside_limits() {
        let mut camera = EditorCamera::default();
        let before = camera.distance();
        camera
            .apply_input(CameraInput::Scroll { delta_y_px: 80.0 }, 400.0)
            .expect("zoom");
        assert!(camera.distance() < before);

        camera
            .apply_input(CameraInput::Scroll { delta_y_px: 1.0e8 }, 400.0)
            .expect("zoom clamp");
        assert!(close(camera.distance(), MIN_DISTANCE));
    }

    #[test]
    fn pan_moves_target_without_changing_distance() {
        let mut camera = EditorCamera::default();
        let distance = camera.distance();
        camera
            .apply_input(
                CameraInput::Drag(CameraDrag {
                    button: CameraPointerButton::Middle,
                    delta_x_px: 40.0,
                    delta_y_px: 0.0,
                }),
                400.0,
            )
            .expect("pan");
        assert!(camera.target().x > 0.0);
        assert!(close(camera.distance(), distance));
        assert!(close(camera.eye().distance(camera.target()), distance));
    }

    #[test]
    fn reset_restores_the_default_camera() {
        let mut camera = EditorCamera::default();
        camera
            .apply_input(CameraInput::Scroll { delta_y_px: 100.0 }, 400.0)
            .expect("zoom");
        camera
            .apply_input(CameraInput::Reset, 400.0)
            .expect("reset");
        let default = EditorCamera::default();
        assert!(close(camera.distance(), default.distance()));
        assert!(close(camera.yaw(), default.yaw()));
        assert!(close(camera.pitch(), default.pitch()));
        assert_eq!(camera.target(), default.target());
    }

    #[test]
    fn invalid_input_does_not_change_the_camera() {
        let mut camera = EditorCamera::default();
        let before = camera.distance();
        let error = camera
            .apply_input(
                CameraInput::Scroll {
                    delta_y_px: f32::NAN,
                },
                400.0,
            )
            .expect_err("nan scroll");
        assert_eq!(error, EditorError::NonFiniteInput);
        assert!(close(camera.distance(), before));

        let error = camera
            .apply_input(
                CameraInput::Drag(CameraDrag {
                    button: CameraPointerButton::Middle,
                    delta_x_px: 1.0,
                    delta_y_px: 0.0,
                }),
                0.0,
            )
            .expect_err("zero viewport");
        assert_eq!(error, EditorError::InvalidViewport);
    }

    #[test]
    fn default_projection_is_finite() {
        let camera = EditorCamera::default();
        let clip = camera.clip_from_world(16.0 / 9.0).expect("projection");
        assert!(clip.is_finite());
    }
}
