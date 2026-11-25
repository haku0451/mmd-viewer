//! Rapier 0.36 world that reproduces the Bullet setup MikuMikuDance uses.
//!
//! `sim.rs` drives this world through a small, index-based interface. The
//! behaviour MikuMikuDance-compatible runtimes get from Bullet is written out
//! here explicitly, because Rapier's defaults differ:
//!
//! * Damping is Bullet's `v *= (1 - d)^dt` followed by Bullet's "additional
//!   damping", applied after every step. Rapier's own damping stays at zero.
//! * Inertia is Bullet's: `0.4 m r²` for spheres, the box formula for boxes,
//!   and the bounding box of the capsule for capsules.
//! * Friction and restitution of a contact are the product of both bodies'
//!   values, as in Bullet.
//! * Collision filtering is Bullet's two-way group and mask test.
//! * Joint springs are Hooke springs around the rest pose (Bullet's
//!   `btGeneric6DofSpringConstraint` applies `stiffness * delta` as force).
//!   They are not applied on a locked axis, where Bullet disables the motor.
//! * A locked rotation axis gives a few degrees under load, as it does in
//!   Bullet, where the solver does not fully converge. Rapier would hold it
//!   rigid, so the lock is a stiff spring fitted to Bullet's sag.
//! * Bodies never sleep and linear speed is not capped.

use glam::{Quat, Vec3};
use rapier3d::glamx;
use rapier3d::prelude::{
    CoefficientCombineRule, ColliderBuilder, GenericJointBuilder, Group, ImpulseJointHandle,
    InteractionGroups, InteractionTestMode, JointAxesMask, JointAxis, MassProperties, MotorModel,
    PhysicsWorld, Real, RigidBodyBuilder, RigidBodyHandle, SharedShape, SpringCoefficients,
};

const GRAVITY: f32 = -98.0;
const JOINT_FREQUENCY: Real = 60.0;
const JOINT_DAMPING_RATIO: Real = 1.0;
const LOCK_STIFFNESS: Real = 40_000.0;
const LOCK_DAMPING: Real = 2_000.0;
const SOLVER_SUBSTEPS: usize = 4;
const SOLVER_ITERATIONS: usize = 2;
const STABILIZATION_ITERATIONS: usize = 2;
const CONTACT_FREQUENCY: Real = 200.0;
const CONTACT_DAMPING_RATIO: Real = 21.0;
const ADDITIONAL_DAMPING_FACTOR: f32 = 0.005;
const ADDITIONAL_LINEAR_THRESHOLD_SQ: f32 = 0.01;
const ADDITIONAL_ANGULAR_THRESHOLD_SQ: f32 = 0.01;
const ADDITIONAL_DAMPING_STEP: f32 = 0.005;
const MIN_EXTENT: f32 = 0.01;

/// Collision shape of a file rigid body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Shape {
    Sphere,
    Box,
    Capsule,
}

impl Shape {
    pub(crate) fn from_file(value: u8) -> Self {
        match value {
            1 => Self::Box,
            2 => Self::Capsule,
            _ => Self::Sphere,
        }
    }
}

/// A rigid body to add, in world space at its rest pose.
pub(crate) struct BodyDesc {
    pub translation: Vec3,
    pub rotation: Quat,
    /// Sphere radius in x; box half extents; capsule radius in x, height in y.
    pub size: Vec3,
    pub shape: Shape,
    pub mass: f32,
    /// Bullet damping factors, already clamped to 0..1.
    pub linear_damping: f32,
    pub angular_damping: f32,
    pub friction: f32,
    pub restitution: f32,
    pub dynamic: bool,
    pub group: u32,
    pub mask: u32,
}

/// A 6DoF spring joint. Frames are in each body's local space.
pub(crate) struct JointDesc {
    pub body_a: u32,
    pub body_b: u32,
    pub frame_a: (Vec3, Quat),
    pub frame_b: (Vec3, Quat),
    /// A minimum above the maximum frees the axis; equal values lock it.
    pub linear_min: Vec3,
    pub linear_max: Vec3,
    pub angular_min: Vec3,
    pub angular_max: Vec3,
    /// Stiffness per axis; zero disables the spring.
    pub linear_spring: Vec3,
    pub angular_spring: Vec3,
}

struct Body {
    handle: RigidBodyHandle,
    dynamic: bool,
    linear_damping: f32,
    angular_damping: f32,
}

/// One Rapier world with the bodies of one model and a floor.
pub(crate) struct World {
    physics: PhysicsWorld,
    bodies: Vec<Body>,
    joints: Vec<ImpulseJointHandle>,
}

impl World {
    pub(crate) fn new() -> Self {
        let mut physics = PhysicsWorld::new();
        physics.gravity = rapier3d::prelude::Vector::new(0.0, GRAVITY, 0.0);
        let parameters = &mut physics.integration_parameters;
        // Bullet does not cap speed. A fast dance swings hair well past the cap.
        parameters.normalized_max_linear_velocity = Real::MAX;
        parameters.num_solver_iterations = SOLVER_SUBSTEPS;
        parameters.num_internal_pgs_iterations = SOLVER_ITERATIONS;
        parameters.num_internal_stabilization_iterations = STABILIZATION_ITERATIONS;
        let contacts = SpringCoefficients::new(CONTACT_FREQUENCY, CONTACT_DAMPING_RATIO);
        parameters.contact_softness = contacts;
        parameters.static_contact_softness = contacts;
        // Bullet does not cap how fast a penetration is pushed apart either.
        // Rapier's default of 3 units per second lets a squatting leg sink
        // into the skirt faster than the contact can push it out.
        parameters.normalized_max_corrective_velocity = Real::MAX;
        Self {
            physics,
            bodies: Vec::new(),
            joints: Vec::new(),
        }
    }

    /// Number of joints in the world.
    pub(crate) fn joint_count(&self) -> usize {
        self.joints.len()
    }

    pub(crate) fn add_body(&mut self, desc: &BodyDesc) -> u32 {
        let size = Vec3::new(
            extent(desc.size.x),
            extent(desc.size.y),
            extent(desc.size.z),
        );
        let (shape, inertia) = shape_and_unit_inertia(desc.shape, size, desc.size.y);
        let builder = if desc.dynamic {
            RigidBodyBuilder::dynamic()
        } else {
            RigidBodyBuilder::kinematic_position_based()
        };
        let mass = if desc.dynamic {
            finite_or(desc.mass, 1.0).max(0.001)
        } else {
            0.0
        };
        let mut builder = builder
            .pose(pose(desc.translation, desc.rotation))
            .can_sleep(false)
            .linear_damping(0.0)
            .angular_damping(0.0);
        if desc.dynamic {
            builder = builder.additional_mass_properties(MassProperties::new(
                rapier3d::prelude::Vector::ZERO,
                mass,
                to_rapier(inertia * mass),
            ));
        }
        let handle = self.physics.bodies.insert(builder);
        let collider = ColliderBuilder::new(shape)
            .density(0.0)
            .friction(finite_or(desc.friction, 0.5).max(0.0))
            .friction_combine_rule(CoefficientCombineRule::Multiply)
            .restitution(finite_or(desc.restitution, 0.0).clamp(0.0, 1.0))
            .restitution_combine_rule(CoefficientCombineRule::Multiply)
            .collision_groups(groups(desc.group, desc.mask));
        self.physics.insert_collider(collider, Some(handle));
        self.bodies.push(Body {
            handle,
            dynamic: desc.dynamic,
            linear_damping: desc.linear_damping,
            angular_damping: desc.angular_damping,
        });
        (self.bodies.len() - 1) as u32
    }

    /// The ground: everything below y = 0, in group bit 16.
    ///
    /// It is a half-space rather than a slab. A motion that kneels or sits can
    /// drive a long skirt's kinematic parents far enough down that the hem
    /// passes the bottom of a slab; the slab then pushes it further down, and
    /// the skirt stays folded under the floor after the dancer stands up. A
    /// half-space always pushes up.
    pub(crate) fn add_floor(&mut self, group: u32) {
        let handle = self.physics.bodies.insert(RigidBodyBuilder::fixed());
        let collider = ColliderBuilder::new(SharedShape::halfspace(rapier3d::prelude::Vector::Y))
            .friction(0.9)
            .friction_combine_rule(CoefficientCombineRule::Multiply)
            .restitution(0.0)
            .restitution_combine_rule(CoefficientCombineRule::Multiply)
            .collision_groups(groups(group, u32::MAX));
        self.physics.insert_collider(collider, Some(handle));
    }

    pub(crate) fn add_joint(&mut self, desc: &JointDesc) {
        let (Some(a), Some(b)) = (
            self.bodies.get(desc.body_a as usize),
            self.bodies.get(desc.body_b as usize),
        ) else {
            return;
        };
        if desc.body_a == desc.body_b {
            return;
        }
        let (handle_a, handle_b) = (a.handle, b.handle);
        let mut locked = JointAxesMask::empty();
        let mut builder = GenericJointBuilder::new(JointAxesMask::empty())
            .local_frame1(pose(desc.frame_a.0, desc.frame_a.1))
            .local_frame2(pose(desc.frame_b.0, desc.frame_b.1))
            // Linked bodies still collide when the file masks say so.
            .contacts_enabled(true)
            .softness(SpringCoefficients::new(
                JOINT_FREQUENCY,
                JOINT_DAMPING_RATIO,
            ));
        let all_axes = [
            (
                JointAxis::LinX,
                desc.linear_min.x,
                desc.linear_max.x,
                desc.linear_spring.x,
            ),
            (
                JointAxis::LinY,
                desc.linear_min.y,
                desc.linear_max.y,
                desc.linear_spring.y,
            ),
            (
                JointAxis::LinZ,
                desc.linear_min.z,
                desc.linear_max.z,
                desc.linear_spring.z,
            ),
            (
                JointAxis::AngX,
                desc.angular_min.x,
                desc.angular_max.x,
                desc.angular_spring.x,
            ),
            (
                JointAxis::AngY,
                desc.angular_min.y,
                desc.angular_max.y,
                desc.angular_spring.y,
            ),
            (
                JointAxis::AngZ,
                desc.angular_min.z,
                desc.angular_max.z,
                desc.angular_spring.z,
            ),
        ];
        for (axis, min, max, spring) in all_axes {
            if min == max {
                if is_angular(axis) {
                    // A stiff, mass-independent spring instead of a rigid lock:
                    // Bullet's locked rotation gives a few degrees under load.
                    builder = builder
                        .motor_model(axis, MotorModel::AccelerationBased)
                        .motor_position(axis, 0.0, LOCK_STIFFNESS, LOCK_DAMPING);
                } else {
                    locked |= axis.into();
                }
                // Bullet disables the motor, and so the file spring, on a locked axis.
                continue;
            }
            if min < max {
                builder = builder.limits(axis, [min, max]);
            }
            if spring.is_finite() && spring > 0.0 {
                builder = builder
                    .motor_model(axis, MotorModel::ForceBased)
                    .motor_position(axis, 0.0, spring, 0.0);
            }
        }
        let builder = builder.locked_axes(locked);
        let handle = self
            .physics
            .impulse_joints
            .insert(handle_a, handle_b, builder, true);
        self.joints.push(handle);
    }

    /// Advance by `dt` seconds, then damp dynamic bodies the way Bullet does.
    pub(crate) fn step(&mut self, dt: f32) {
        if dt.is_nan() || dt <= 0.0 {
            return;
        }
        self.physics.integration_parameters.dt = dt;
        self.physics.step();
        for body in &self.bodies {
            if !body.dynamic {
                continue;
            }
            let Some(rigid) = self.physics.bodies.get_mut(body.handle) else {
                continue;
            };
            let (linear, angular) = bullet_damping(
                from_rapier(rigid.linvel()),
                from_rapier(rigid.angvel()),
                body.linear_damping,
                body.angular_damping,
                dt,
            );
            rigid.set_linvel(to_rapier(linear), false);
            rigid.set_angvel(to_rapier(angular), false);
        }
    }

    /// Place a body. A kinematic body moved without a reset sweeps to the pose
    /// during the next step, so contacts and joints see its velocity.
    pub(crate) fn set_pose(
        &mut self,
        index: u32,
        translation: Vec3,
        rotation: Quat,
        reset_velocity: bool,
    ) {
        let Some(body) = self.bodies.get(index as usize) else {
            return;
        };
        let dynamic = body.dynamic;
        let Some(rigid) = self.physics.bodies.get_mut(body.handle) else {
            return;
        };
        // q and -q are the same rotation, but Rapier derives a kinematic
        // body's angular velocity from the quaternion difference. A target on
        // the other hemisphere from the current rotation reads as a full turn
        // within one step: the body spins at hundreds of radians per second
        // for that step and throws whatever touches it.
        let current = quat(rigid.position().rotation);
        let rotation = if current.dot(rotation) < 0.0 {
            -rotation
        } else {
            rotation
        };
        let target = pose(translation, rotation);
        if !dynamic && !reset_velocity {
            rigid.set_next_kinematic_position(target);
            return;
        }
        rigid.set_position(target, true);
        if !dynamic {
            rigid.set_next_kinematic_position(target);
        }
        if reset_velocity {
            rigid.set_linvel(rapier3d::prelude::Vector::ZERO, true);
            rigid.set_angvel(rapier3d::prelude::Vector::ZERO, true);
            rigid.reset_forces(true);
            rigid.reset_torques(true);
        }
    }

    pub(crate) fn pose(&self, index: u32) -> (Vec3, Quat) {
        let Some(rigid) = self
            .bodies
            .get(index as usize)
            .and_then(|body| self.physics.bodies.get(body.handle))
        else {
            return (Vec3::ZERO, Quat::IDENTITY);
        };
        let pose = rigid.position();
        let r = pose.rotation;
        (
            from_rapier(pose.translation),
            Quat::from_xyzw(r.x, r.y, r.z, r.w),
        )
    }

    pub(crate) fn linear_speed(&self, index: u32) -> f32 {
        self.rigid(index)
            .map(|rigid| from_rapier(rigid.linvel()).length())
            .unwrap_or(0.0)
    }

    pub(crate) fn angular_speed(&self, index: u32) -> f32 {
        self.rigid(index)
            .map(|rigid| from_rapier(rigid.angvel()).length())
            .unwrap_or(0.0)
    }

    pub(crate) fn zero_velocity(&mut self, index: u32) {
        let Some(body) = self.bodies.get(index as usize) else {
            return;
        };
        if let Some(rigid) = self.physics.bodies.get_mut(body.handle) {
            rigid.set_linvel(rapier3d::prelude::Vector::ZERO, false);
            rigid.set_angvel(rapier3d::prelude::Vector::ZERO, false);
            rigid.reset_forces(false);
            rigid.reset_torques(false);
        }
    }

    fn rigid(&self, index: u32) -> Option<&rapier3d::prelude::RigidBody> {
        let body = self.bodies.get(index as usize)?;
        self.physics.bodies.get(body.handle)
    }
}

fn quat(rotation: glamx::Quat) -> Quat {
    Quat::from_xyzw(rotation.x, rotation.y, rotation.z, rotation.w)
}

fn is_angular(axis: JointAxis) -> bool {
    matches!(axis, JointAxis::AngX | JointAxis::AngY | JointAxis::AngZ)
}

/// Bullet's `btRigidBody::applyDamping` with additional damping enabled.
fn bullet_damping(
    linear: Vec3,
    angular: Vec3,
    linear_damping: f32,
    angular_damping: f32,
    dt: f32,
) -> (Vec3, Vec3) {
    let mut linear = linear * (1.0 - linear_damping).powf(dt);
    let mut angular = angular * (1.0 - angular_damping).powf(dt);
    if angular.length_squared() < ADDITIONAL_ANGULAR_THRESHOLD_SQ
        && linear.length_squared() < ADDITIONAL_LINEAR_THRESHOLD_SQ
    {
        angular *= ADDITIONAL_DAMPING_FACTOR;
        linear *= ADDITIONAL_DAMPING_FACTOR;
    }
    linear = slow_down(linear, linear_damping);
    angular = slow_down(angular, angular_damping);
    (linear, angular)
}

/// Below the damping factor as a speed, Bullet takes 0.005 off each step.
fn slow_down(velocity: Vec3, damping: f32) -> Vec3 {
    let speed = velocity.length();
    if speed >= damping {
        velocity
    } else if speed > ADDITIONAL_DAMPING_STEP {
        velocity - velocity / speed * ADDITIONAL_DAMPING_STEP
    } else {
        Vec3::ZERO
    }
}

/// The collision shape and Bullet's inertia per unit mass.
fn shape_and_unit_inertia(shape: Shape, size: Vec3, file_height: f32) -> (SharedShape, Vec3) {
    match shape {
        Shape::Box => {
            let full = size * 2.0;
            let squared = full * full;
            (
                SharedShape::cuboid(size.x, size.y, size.z),
                Vec3::new(
                    squared.y + squared.z,
                    squared.x + squared.z,
                    squared.x + squared.y,
                ) / 12.0,
            )
        }
        // PMX height is the cylinder between the caps. A zero height is a sphere.
        Shape::Capsule if file_height.is_finite() && file_height.abs() > 1.0e-4 => {
            let radius = size.x;
            let half_height = file_height.abs() * 0.5;
            // Bullet approximates a capsule by the box that bounds it.
            let full = Vec3::new(radius, radius + half_height, radius) * 2.0;
            let squared = full * full;
            (
                SharedShape::capsule_y(half_height, radius),
                Vec3::new(
                    squared.y + squared.z,
                    squared.x + squared.z,
                    squared.x + squared.y,
                ) / 12.0,
            )
        }
        Shape::Sphere | Shape::Capsule => (
            SharedShape::ball(size.x),
            Vec3::splat(0.4 * size.x * size.x),
        ),
    }
}

fn groups(group: u32, mask: u32) -> InteractionGroups {
    InteractionGroups::new(
        Group::from_bits_retain(group),
        Group::from_bits_retain(mask),
        InteractionTestMode::And,
    )
}

fn extent(value: f32) -> f32 {
    finite_or(value, MIN_EXTENT).abs().max(MIN_EXTENT)
}

fn finite_or(value: f32, fallback: f32) -> f32 {
    if value.is_finite() { value } else { fallback }
}

fn to_rapier(value: Vec3) -> rapier3d::prelude::Vector {
    rapier3d::prelude::Vector::new(value.x, value.y, value.z)
}

fn from_rapier(value: rapier3d::prelude::Vector) -> Vec3 {
    Vec3::new(value.x, value.y, value.z)
}

fn pose(translation: Vec3, rotation: Quat) -> rapier3d::prelude::Pose {
    let rotation = if rotation.is_finite() && rotation.length_squared() > 1.0e-12 {
        rotation.normalize()
    } else {
        Quat::IDENTITY
    };
    rapier3d::prelude::Pose::from_parts(
        to_rapier(translation),
        glamx::Quat::from_xyzw(rotation.x, rotation.y, rotation.z, rotation.w),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn damping_follows_the_bullet_power_formula() {
        let (linear, _) = bullet_damping(Vec3::new(10.0, 0.0, 0.0), Vec3::ZERO, 0.9, 0.0, 1.0);
        assert!((linear.x - 1.0).abs() < 1.0e-4);
    }

    #[test]
    fn a_nearly_still_body_is_stopped_by_additional_damping() {
        let (linear, angular) = bullet_damping(
            Vec3::new(0.05, 0.0, 0.0),
            Vec3::new(0.0, 0.05, 0.0),
            0.5,
            0.5,
            1.0 / 120.0,
        );
        assert_eq!(linear, Vec3::ZERO);
        assert_eq!(angular, Vec3::ZERO);
    }

    #[test]
    fn capsule_inertia_is_the_bounding_box() {
        let (_, inertia) = shape_and_unit_inertia(Shape::Capsule, Vec3::new(1.0, 2.0, 1.0), 2.0);
        // Box 2 x 4 x 2.
        assert!((inertia.x - (16.0 + 4.0) / 12.0).abs() < 1.0e-5);
        assert!((inertia.y - (4.0 + 4.0) / 12.0).abs() < 1.0e-5);
    }
}
