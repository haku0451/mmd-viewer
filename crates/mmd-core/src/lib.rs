//! Internal data model for MMD Viewer.
//!
//! The crate owns model assets, motions, frame time, and the clip-space
//! conversion. It does not parse files, evaluate animation, or talk to the GPU.

mod coordinates;
mod model;
mod motion;
mod time;

pub use coordinates::{
    CoordinateError, ProjectionParams, WGPU_FROM_OPENGL_CLIP, wgpu_clip_from_view,
};
pub use model::{
    Append, Bone, BoneTail, DisplayFrame, Ik, IkLink, Joint, LocalAxis, Material, MaterialOffset,
    ModelAsset, Morph, RigidBody, Skinning, SphereMode, ToonSource, UvChannel, Vertex,
};
pub use motion::{
    Bezier, BoneKeyframe, BoneTrack, CameraKeyframe, IkKeyframe, LightKeyframe, MorphKeyframe,
    MorphTrack, Motion, ShadowKeyframe,
};
pub use time::{FRAMES_PER_SECOND, FrameTime};
