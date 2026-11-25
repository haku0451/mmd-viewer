//! MMD animation evaluation.
//!
//! Frame time is an integer. Sampling, morphs, append, fixed axes, IK, and
//! skin matrices are applied in that order. Physics is not part of this crate.

mod bezier;
mod evaluate;
mod skin;

pub use evaluate::{
    CameraSample, EvaluatedFrame, MaterialFrame, default_light_direction, evaluate,
    external_parent_names, skin_vertex,
};
pub use skin::{bdef, project_onto_ground, qdef, sdef};
