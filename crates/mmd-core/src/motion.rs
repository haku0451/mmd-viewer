//! Sampled motion data.
//!
//! Bone curves belong to the destination key. The evaluator reads the later
//! key's curve when it interpolates into that key.

use glam::{Quat, Vec3};

use crate::time::FrameTime;

/// One cubic Bézier control pair. Components are file units, 0 to 127.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bezier {
    pub x1: u8,
    pub y1: u8,
    pub x2: u8,
    pub y2: u8,
}

impl Bezier {
    /// The curve MMD writes for a new linear key: (20, 20)–(107, 107).
    pub const LINEAR: Self = Self {
        x1: 20,
        y1: 20,
        x2: 107,
        y2: 107,
    };
}

/// A VMD or equivalent motion.
#[derive(Clone, Debug)]
pub struct Motion {
    /// Model name stored in the file. It may be empty.
    pub model_name: String,
    /// Bone tracks. Keys are sorted by frame.
    pub bones: Vec<BoneTrack>,
    /// Morph tracks. Keys are sorted by frame.
    pub morphs: Vec<MorphTrack>,
    /// Camera keys, sorted by frame.
    pub cameras: Vec<CameraKeyframe>,
    /// Light keys, sorted by frame.
    pub lights: Vec<LightKeyframe>,
    /// Self-shadow keys. The distance is stored and is not used to size the shadow map.
    pub shadows: Vec<ShadowKeyframe>,
    /// Model visibility and per-IK enable keys, sorted by frame.
    pub ik: Vec<IkKeyframe>,
}

/// Keys for one bone name.
#[derive(Clone, Debug)]
pub struct BoneTrack {
    /// Bone name in Shift-JIS decoded form.
    pub name: String,
    /// Keys sorted by frame. A later duplicate frame replaces the earlier one.
    pub keys: Vec<BoneKeyframe>,
}

#[derive(Clone, Debug)]
pub struct BoneKeyframe {
    pub frame: FrameTime,
    /// Translation offset from the rest pose.
    pub translation: Vec3,
    /// Rotation offset from the rest pose.
    pub rotation: Quat,
    /// Curves for X, Y, Z, and rotation. They describe arrival at this key.
    pub curves: [Bezier; 4],
}

/// Keys for one morph name.
#[derive(Clone, Debug)]
pub struct MorphTrack {
    pub name: String,
    /// Keys sorted by frame.
    pub keys: Vec<MorphKeyframe>,
}

/// One morph key. Morphs have no Bézier curve.
#[derive(Clone, Debug)]
pub struct MorphKeyframe {
    pub frame: FrameTime,
    pub weight: f32,
}

#[derive(Clone, Debug)]
pub struct CameraKeyframe {
    pub frame: FrameTime,
    /// Look-at point in model space.
    pub interest: Vec3,
    /// Euler angles in radians, applied in XYZ order.
    pub rotation: Vec3,
    /// Distance used by `interest + R * (0, 0, -distance)`.
    pub distance: f32,
    /// Vertical field of view in degrees.
    pub fov_degrees: f32,
    /// `false` when the file asks for an orthographic camera.
    pub perspective: bool,
    /// Curves for interest X/Y/Z, rotation, distance, and field of view.
    pub curves: [Bezier; 6],
}

#[derive(Clone, Debug)]
pub struct LightKeyframe {
    pub frame: FrameTime,
    pub color: Vec3,
    /// Direction the light travels. The file does not store a position.
    pub direction: Vec3,
}

/// One self-shadow key from a VMD file.
#[derive(Clone, Debug)]
pub struct ShadowKeyframe {
    pub frame: FrameTime,
    pub mode: u8,
    /// Distance byte field from the file.
    pub distance: f32,
}

/// Visibility of the model and of named IK bones.
#[derive(Clone, Debug)]
pub struct IkKeyframe {
    pub frame: FrameTime,
    pub visible: bool,
    /// IK bone name and whether that IK chain runs.
    pub chains: Vec<(String, bool)>,
}

impl Motion {
    /// Largest frame number in the motion, or zero when there are no keys.
    pub fn last_frame(&self) -> i32 {
        let mut last = 0;
        for track in &self.bones {
            if let Some(key) = track.keys.last() {
                last = last.max(key.frame.frame());
            }
        }
        for track in &self.morphs {
            if let Some(key) = track.keys.last() {
                last = last.max(key.frame.frame());
            }
        }
        for key in &self.cameras {
            last = last.max(key.frame.frame());
        }
        last
    }
}
