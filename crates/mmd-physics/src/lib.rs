//! Bullet 3.25 runtime for PMX and PMD rigid bodies.
//!
//! The solver is the Bullet discrete world, linked through CXX. It is set up
//! the way MikuMikuDance-compatible runtimes set up Bullet: file collision
//! groups and masks, file damping with Bullet's default formula, and
//! `btGeneric6DofSpringConstraint` joints. The solved bodies are written back
//! to the bones without further correction.
//!
//! Animation stays in `mmd-animation`. This crate reads the posed bones, steps
//! the world in fixed 1/120 s steps, and writes dynamic bones back. The caller
//! drives it from the motion clock, so physics stays in step with the body.

mod bullet;
mod correct;
mod sim;

pub use sim::{PhysicsWorld, STEP_SECONDS, SimulationFlags};
