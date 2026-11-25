//! Bullet 3.25 world built from the model's rigid bodies and joints.
//!
//! The world is set up the way MikuMikuDance-compatible runtimes (MMDAgent,
//! saba) set up Bullet: gravity -98, `btGeneric6DofSpringConstraint` with the
//! file limits and stiffness, the file's collision group and mask, Bullet's
//! default solver and damping formula, and no extra contact filtering.
//!
//! Each call advances the world by the elapsed time in fixed 1/120 s steps:
//!
//! 1. Kinematic bodies (type 0) are moved toward their animated bones. Across
//!    the steps of one call the target moves from the pose of the previous
//!    call to this one, so one 30 fps motion frame becomes four even steps
//!    instead of a jump and a pause.
//! 2. The world steps.
//! 3. Bones are walked parent first. A type 1 bone takes the body's position
//!    and rotation. A type 2 bone (ボーン位置合わせ) takes the body's rotation
//!    and keeps its animated offset from its parent. Any other bone below a
//!    solved bone keeps its animated offset from that parent.
//!
//! A type 1 bone whose body hangs from the parent bone's body by a joint with
//! every linear axis locked also keeps its offset from the parent and takes
//! only the body's rotation. The file says that link cannot change length;
//! the soft solver lets it stretch, and the skinned texture would stretch
//! with it. The drape comes from the rotations, so it is kept.

use glam::{EulerRot, Mat4, Quat, Vec3};
use mmd_core::{Joint, ModelAsset, RigidBody};

use crate::bullet::ffi::{self, BodyDesc, JointDesc, PoseF, World};
use crate::correct;

/// Length of one fixed physics step in seconds.
///
/// One 30 fps motion frame is four steps.
pub const STEP_SECONDS: f32 = 1.0 / 120.0;

/// Whether this call should reset, step, and write the pose.
#[derive(Clone, Copy, Debug)]
pub struct SimulationFlags {
    /// Run the step while physics is turned on.
    pub enabled: bool,
    /// The timeline is advancing. The step still runs when this is false, so a
    /// stopped pose can settle.
    pub playing: bool,
    /// Snap bodies back to the animated pose before the step.
    pub reset: bool,
}

/// Steps one call may take. A longer stall drops the rest of the time.
const MAX_STEPS_PER_CALL: u32 = 12;
const SETTLE_SPEED: f32 = 0.04;
const SETTLE_SPIN: f32 = 0.05;
const SETTLE_STEPS: u32 = 90;
/// Membership bit of the floor, above the 16 PMX groups.
const FLOOR_GROUP: u32 = 1 << 16;

/// One simulated model.
pub struct PhysicsWorld {
    world: cxx::UniquePtr<World>,
    driven: Vec<Driven>,
    previous_translation: Vec<Vec3>,
    previous_rotation: Vec<Quat>,
    /// Elapsed time not yet covered by a whole step.
    pending: f32,
    /// Bones whose link to the parent body has every linear axis locked.
    held_length: Vec<bool>,
    quiet_frames: u32,
    settled: bool,
}

struct Driven {
    index: u32,
    bone: Option<usize>,
    /// 0 kinematic, 1 dynamic, 2 dynamic with the bone's position kept.
    kind: u8,
    offset_translation: Vec3,
    offset_rotation: Quat,
}

impl Driven {
    fn dynamic(&self) -> bool {
        self.kind != 0
    }
}

impl PhysicsWorld {
    /// Build a Bullet world from the stored rigid bodies and joints.
    pub fn new(model: &ModelAsset) -> Self {
        let mut world = ffi::create_world();
        let mut driven = Vec::new();
        let mut file_slots = Vec::new();
        for rigid in &model.rigid_bodies {
            if let Some(slot) = add_file_body(model, rigid, world.pin_mut()) {
                file_slots.push(Some(driven.len()));
                driven.push(slot);
            } else {
                file_slots.push(None);
            }
        }
        world.pin_mut().add_floor();
        let joints = add_joints(model, &file_slots, &driven, world.pin_mut());
        tracing::info!(bodies = driven.len(), joints, "created a Bullet world");
        Self {
            world,
            driven,
            previous_translation: Vec::new(),
            previous_rotation: Vec::new(),
            pending: 0.0,
            held_length: held_length_bones(model),
            quiet_frames: 0,
            settled: false,
        }
    }

    /// The pose has stopped moving while playback is paused.
    pub fn is_settled(&self) -> bool {
        self.settled
    }

    /// Advance the world by `elapsed` seconds toward the animated bones, then
    /// write the solved pose into them.
    ///
    /// `translation`, `rotation`, and `skin` hold the animated pose on entry
    /// and the solved pose on return. `flags.reset` snaps every body to the
    /// animated pose and clears velocity first. Time shorter than one step is
    /// carried to the next call.
    pub fn simulate(
        &mut self,
        model: &ModelAsset,
        translation: &mut [Vec3],
        rotation: &mut [Quat],
        skin: &mut [Mat4],
        flags: SimulationFlags,
        elapsed: f32,
    ) {
        if translation.len() != model.bones.len() || rotation.len() != model.bones.len() {
            return;
        }
        if flags.reset || !flags.enabled {
            self.snap(translation, rotation);
            self.remember_pose(translation, rotation);
            self.pending = 0.0;
            self.quiet_frames = 0;
            self.settled = false;
        }
        if !flags.enabled {
            return;
        }
        let same = self.same_animated(translation, rotation);
        if flags.playing || flags.reset || !same {
            self.quiet_frames = 0;
            self.settled = false;
        }
        if self.settled {
            self.write_dynamic(model, translation, rotation, skin);
            return;
        }
        if self.previous_translation.len() != translation.len() {
            self.remember_pose(translation, rotation);
        }
        let steps = self.take_steps(elapsed);
        for step in 1..=steps {
            let amount = step as f32 / steps as f32;
            self.sync_kinematic(translation, rotation, amount);
            self.world.pin_mut().step(STEP_SECONDS);
        }
        if steps > 0 {
            self.remember_pose(translation, rotation);
        }
        self.write_dynamic(model, translation, rotation, skin);
        if steps > 0 {
            self.note_motion(flags.playing, steps);
        }
    }

    /// Whole steps covered by the time carried over plus `elapsed`.
    fn take_steps(&mut self, elapsed: f32) -> u32 {
        if elapsed.is_finite() && elapsed > 0.0 {
            self.pending += elapsed;
        }
        let whole = (self.pending / STEP_SECONDS + 1.0e-3).floor();
        if whole < 1.0 {
            return 0;
        }
        self.pending = (self.pending - whole * STEP_SECONDS).max(0.0);
        if whole > MAX_STEPS_PER_CALL as f32 {
            self.pending = 0.0;
            return MAX_STEPS_PER_CALL;
        }
        whole as u32
    }

    fn same_animated(&self, translation: &[Vec3], rotation: &[Quat]) -> bool {
        if self.previous_translation.len() != translation.len()
            || self.previous_rotation.len() != rotation.len()
        {
            return false;
        }
        self.driven.iter().all(|driven| {
            if driven.dynamic() {
                return true;
            }
            let Some(bone) = driven.bone else {
                return true;
            };
            bone < translation.len()
                && translation[bone].distance_squared(self.previous_translation[bone]) < 1.0e-8
                && rotation[bone].dot(self.previous_rotation[bone]).abs() > 0.99999
        })
    }

    fn note_motion(&mut self, playing: bool, steps: u32) {
        if playing {
            self.quiet_frames = 0;
            self.settled = false;
            return;
        }
        let (linear, angular) = self.max_speed();
        if linear < SETTLE_SPEED && angular < SETTLE_SPIN {
            self.quiet_frames = self.quiet_frames.saturating_add(steps);
        } else {
            self.quiet_frames = 0;
        }
        // Keep stepping while anything is still moving, so a rest pose can drape.
        self.settled = self.quiet_frames >= SETTLE_STEPS;
        if self.settled {
            for driven in &self.driven {
                if driven.dynamic() {
                    self.world.pin_mut().zero_velocity(driven.index);
                }
            }
        }
    }

    fn max_speed(&self) -> (f32, f32) {
        let mut linear: f32 = 0.0;
        let mut angular: f32 = 0.0;
        for driven in &self.driven {
            if !driven.dynamic() {
                continue;
            }
            linear = linear.max(self.world.linear_speed(driven.index));
            angular = angular.max(self.world.angular_speed(driven.index));
        }
        (linear, angular)
    }

    fn snap(&mut self, translation: &[Vec3], rotation: &[Quat]) {
        for driven in &self.driven {
            let (position, orientation) = target_pose(driven, translation, rotation);
            self.world
                .pin_mut()
                .set_pose(driven.index, &pose_f(position, orientation), true);
        }
    }

    fn remember_pose(&mut self, translation: &[Vec3], rotation: &[Quat]) {
        self.previous_translation.clear();
        self.previous_translation.extend_from_slice(translation);
        self.previous_rotation.clear();
        self.previous_rotation.extend_from_slice(rotation);
    }

    /// Move kinematic bodies `amount` of the way from the last tick's bones to these.
    fn sync_kinematic(&mut self, translation: &[Vec3], rotation: &[Quat], amount: f32) {
        for driven in &self.driven {
            if driven.dynamic() {
                continue;
            }
            let (position, orientation) = match driven.bone {
                Some(bone)
                    if bone < translation.len()
                        && bone < self.previous_translation.len()
                        && bone < self.previous_rotation.len() =>
                {
                    compose(
                        self.previous_translation[bone].lerp(translation[bone], amount),
                        self.previous_rotation[bone].slerp(rotation[bone], amount),
                        driven.offset_translation,
                        driven.offset_rotation,
                    )
                }
                _ => target_pose(driven, translation, rotation),
            };
            self.world
                .pin_mut()
                .set_pose(driven.index, &pose_f(position, orientation), false);
        }
    }

    /// Write solved bones parent first, carrying unsolved children along.
    fn write_dynamic(
        &mut self,
        model: &ModelAsset,
        translation: &mut [Vec3],
        rotation: &mut [Quat],
        skin: &mut [Mat4],
    ) {
        let count = translation.len();
        let mut solved_by: Vec<Option<usize>> = vec![None; count];
        for (slot, driven) in self.driven.iter().enumerate() {
            if driven.dynamic()
                && let Some(bone) = driven.bone
                && bone < count
            {
                solved_by[bone] = Some(slot);
            }
        }
        let animated_translation = translation.to_vec();
        let animated_rotation = rotation.to_vec();
        let mut changed = vec![false; count];
        for index in correct::bone_order(model) {
            if index >= count {
                continue;
            }
            let parent = model.bones[index]
                .parent
                .map(usize::from)
                .filter(|parent| *parent < count && changed[*parent]);
            let (mut bone_translation, mut bone_rotation) = match parent {
                Some(parent) => rehang(
                    animated_translation[parent],
                    animated_rotation[parent],
                    translation[parent],
                    rotation[parent],
                    animated_translation[index],
                    animated_rotation[index],
                ),
                None => (animated_translation[index], animated_rotation[index]),
            };
            if let Some(slot) = solved_by[index] {
                let driven = &self.driven[slot];
                let (body_translation, body_rotation) =
                    bone_from_body(driven, self.world.pose(driven.index));
                bone_rotation = body_rotation;
                let held = self.held_length.get(index).copied().unwrap_or(false);
                if driven.kind == 1 && !held {
                    bone_translation = body_translation;
                }
            } else if parent.is_none() {
                continue;
            }
            translation[index] = bone_translation;
            rotation[index] = bone_rotation.normalize();
            changed[index] = true;
        }
        correct::write_skin(model, translation, rotation, skin, &changed);
    }
}

/// Bones whose rigid body hangs from the parent bone's body by a joint with
/// every linear axis locked.
fn held_length_bones(model: &ModelAsset) -> Vec<bool> {
    let mut held = vec![false; model.bones.len()];
    let body_bone = |index: Option<u32>| -> Option<(usize, u8)> {
        let rigid = model.rigid_bodies.get(index? as usize)?;
        let bone = rigid.bone? as usize;
        (bone < model.bones.len()).then_some((bone, rigid.body_type))
    };
    for joint in &model.joints {
        let locked = axis_locked(joint.position_min.x, joint.position_max.x)
            && axis_locked(joint.position_min.y, joint.position_max.y)
            && axis_locked(joint.position_min.z, joint.position_max.z);
        if !locked {
            continue;
        }
        let (Some(a), Some(b)) = (body_bone(joint.rigid_a), body_bone(joint.rigid_b)) else {
            continue;
        };
        for ((parent, _), (child, kind)) in [(a, b), (b, a)] {
            if kind == 1 && model.bones[child].parent.map(usize::from) == Some(parent) {
                held[child] = true;
            }
        }
    }
    held
}

fn axis_locked(min: f32, max: f32) -> bool {
    min.is_finite() && max.is_finite() && min <= max && max - min <= 1.0e-4
}

/// Keep a child's animated offset from its parent after the parent moved.
fn rehang(
    parent_before_translation: Vec3,
    parent_before_rotation: Quat,
    parent_translation: Vec3,
    parent_rotation: Quat,
    child_translation: Vec3,
    child_rotation: Quat,
) -> (Vec3, Quat) {
    let inverse = parent_before_rotation.inverse();
    let local_translation = inverse * (child_translation - parent_before_translation);
    let local_rotation = inverse * child_rotation;
    compose(
        parent_translation,
        parent_rotation,
        local_translation,
        local_rotation,
    )
}

fn add_file_body(
    model: &ModelAsset,
    rigid: &RigidBody,
    world: std::pin::Pin<&mut World>,
) -> Option<Driven> {
    if !rigid.position.is_finite() || !rigid.rotation.is_finite() || !rigid.size.is_finite() {
        return None;
    }
    let bone = rigid
        .bone
        .map(|index| index as usize)
        .filter(|index| *index < model.bones.len());
    let orientation = euler(rigid.rotation);
    let (offset_translation, offset_rotation) = match bone {
        Some(index) => (rigid.position - model.bones[index].position, orientation),
        None => (rigid.position, orientation),
    };
    let kind = match rigid.body_type {
        1 => 1,
        2 => 2,
        _ => 0,
    };
    let mut mask = u32::from(rigid.mask);
    if kind != 0 {
        mask |= FLOOR_GROUP;
    }
    let index = world.add_body(&BodyDesc {
        tx: rigid.position.x,
        ty: rigid.position.y,
        tz: rigid.position.z,
        qx: orientation.x,
        qy: orientation.y,
        qz: orientation.z,
        qw: orientation.w,
        sx: rigid.size.x,
        sy: rigid.size.y,
        sz: rigid.size.z,
        mass: rigid.mass,
        linear_damping: file_damping(rigid.linear_damping),
        angular_damping: file_damping(rigid.angular_damping),
        friction: rigid.friction,
        restitution: rigid.restitution,
        shape: i32::from(rigid.shape),
        kind: i32::from(kind),
        group: (1u32 << (rigid.group & 15)) as i32,
        mask: mask as i32,
    });
    Some(Driven {
        index,
        bone,
        kind,
        offset_translation,
        offset_rotation,
    })
}

fn add_joints(
    model: &ModelAsset,
    file_slots: &[Option<usize>],
    driven: &[Driven],
    mut world: std::pin::Pin<&mut World>,
) -> usize {
    let mut count = 0;
    for joint in &model.joints {
        let (Some(index_a), Some(index_b)) = (joint.rigid_a, joint.rigid_b) else {
            continue;
        };
        let (Some(slot_a), Some(slot_b)) = (
            file_slots.get(index_a as usize).and_then(|slot| *slot),
            file_slots.get(index_b as usize).and_then(|slot| *slot),
        ) else {
            continue;
        };
        let (Some(body_a), Some(body_b)) = (driven.get(slot_a), driven.get(slot_b)) else {
            continue;
        };
        if !joint.position.is_finite() || !joint.rotation.is_finite() {
            continue;
        }
        let Some(rigid_a) = model.rigid_bodies.get(index_a as usize) else {
            continue;
        };
        let Some(rigid_b) = model.rigid_bodies.get(index_b as usize) else {
            continue;
        };
        let anchor_rotation = euler(joint.rotation);
        let frame_a = local_frame(
            rigid_a.position,
            euler(rigid_a.rotation),
            joint.position,
            anchor_rotation,
        );
        let frame_b = local_frame(
            rigid_b.position,
            euler(rigid_b.rotation),
            joint.position,
            anchor_rotation,
        );
        let (lin_min, lin_max) = joint_limits(joint, true);
        let (ang_min, ang_max) = joint_limits(joint, false);
        world.as_mut().add_joint(&JointDesc {
            body_a: body_a.index as i32,
            body_b: body_b.index as i32,
            a_tx: frame_a.0.x,
            a_ty: frame_a.0.y,
            a_tz: frame_a.0.z,
            a_qx: frame_a.1.x,
            a_qy: frame_a.1.y,
            a_qz: frame_a.1.z,
            a_qw: frame_a.1.w,
            b_tx: frame_b.0.x,
            b_ty: frame_b.0.y,
            b_tz: frame_b.0.z,
            b_qx: frame_b.1.x,
            b_qy: frame_b.1.y,
            b_qz: frame_b.1.z,
            b_qw: frame_b.1.w,
            lin_min_x: lin_min.x,
            lin_min_y: lin_min.y,
            lin_min_z: lin_min.z,
            lin_max_x: lin_max.x,
            lin_max_y: lin_max.y,
            lin_max_z: lin_max.z,
            ang_min_x: ang_min.x,
            ang_min_y: ang_min.y,
            ang_min_z: ang_min.z,
            ang_max_x: ang_max.x,
            ang_max_y: ang_max.y,
            ang_max_z: ang_max.z,
            spring_lin_x: spring_on(joint.spring_position.x),
            spring_lin_y: spring_on(joint.spring_position.y),
            spring_lin_z: spring_on(joint.spring_position.z),
            spring_ang_x: spring_on(joint.spring_rotation.x),
            spring_ang_y: spring_on(joint.spring_rotation.y),
            spring_ang_z: spring_on(joint.spring_rotation.z),
        });
        count += 1;
    }
    count
}

/// Point-to-point and cone-twist lock translation. A slider locks rotation.
/// A hinge keeps angular X. Spring 6DoF and plain 6DoF use the file limits.
fn joint_limits(joint: &Joint, linear: bool) -> (Vec3, Vec3) {
    let (min, max) = if linear {
        (joint.position_min, joint.position_max)
    } else {
        (joint.rotation_min, joint.rotation_max)
    };
    let mins = [min.x, min.y, min.z];
    let maxs = [max.x, max.y, max.z];
    let mut low = [0.0; 3];
    let mut high = [0.0; 3];
    for axis in 0..3 {
        let locked = match joint.kind {
            2 | 3 => linear,
            4 => !linear,
            5 => linear || axis != 0,
            _ => false,
        };
        if locked {
            low[axis] = 0.0;
            high[axis] = 0.0;
        } else if !mins[axis].is_finite() || !maxs[axis].is_finite() {
            low[axis] = 1.0;
            high[axis] = -1.0;
        } else {
            low[axis] = mins[axis];
            high[axis] = maxs[axis];
        }
    }
    (Vec3::from_array(low), Vec3::from_array(high))
}

/// The file stiffness, enabled when it is a positive number.
fn spring_on(spring: f32) -> f32 {
    if spring.is_finite() && spring > 0.0 {
        spring
    } else {
        0.0
    }
}

/// PMX damping is Bullet's damping factor. Bullet clamps it to 0..1 and damps
/// with `velocity *= (1 - damping)^dt`.
fn file_damping(file_value: f32) -> f32 {
    if file_value.is_finite() {
        file_value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn target_pose(driven: &Driven, translation: &[Vec3], rotation: &[Quat]) -> (Vec3, Quat) {
    match driven.bone {
        Some(index) if index < translation.len() && index < rotation.len() => compose(
            translation[index],
            rotation[index],
            driven.offset_translation,
            driven.offset_rotation,
        ),
        _ => (driven.offset_translation, driven.offset_rotation),
    }
}

fn bone_from_body(driven: &Driven, body: PoseF) -> (Vec3, Quat) {
    let (inverse_translation, inverse_rotation) =
        inverse_pose(driven.offset_translation, driven.offset_rotation);
    compose(
        Vec3::new(body.x, body.y, body.z),
        Quat::from_xyzw(body.qx, body.qy, body.qz, body.qw),
        inverse_translation,
        inverse_rotation,
    )
}

fn local_frame(
    body_translation: Vec3,
    body_rotation: Quat,
    anchor_translation: Vec3,
    anchor_rotation: Quat,
) -> (Vec3, Quat) {
    let (inverse_translation, inverse_rotation) = inverse_pose(body_translation, body_rotation);
    compose(
        inverse_translation,
        inverse_rotation,
        anchor_translation,
        anchor_rotation,
    )
}

fn compose(
    origin_translation: Vec3,
    origin_rotation: Quat,
    local_translation: Vec3,
    local_rotation: Quat,
) -> (Vec3, Quat) {
    (
        origin_translation + origin_rotation * local_translation,
        origin_rotation * local_rotation,
    )
}

fn inverse_pose(translation: Vec3, rotation: Quat) -> (Vec3, Quat) {
    let rotation = rotation.inverse();
    (-(rotation * translation), rotation)
}

fn pose_f(translation: Vec3, rotation: Quat) -> PoseF {
    PoseF {
        x: translation.x,
        y: translation.y,
        z: translation.z,
        qx: rotation.x,
        qy: rotation.y,
        qz: rotation.z,
        qw: rotation.w,
    }
}

/// Rigid-body and joint angles from the file, composed as `Ry * Rx * Rz`.
///
/// This is Direct3D's yaw-pitch-roll order. On three shipped models it lines
/// capsule axes and the twist axis of Y-locked joints up with their bones far
/// better than Bullet's `setEulerZYX` order (joint twist axis: 0.89 against
/// 0.80 on one model, 0.995 against 0.97 on another). A wrongly turned joint
/// frame locks a swing axis, and the hair cannot fall toward the back.
fn euler(radians: Vec3) -> Quat {
    Quat::from_euler(EulerRot::YXZ, radians.y, radians.x, radians.z).normalize()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One display tick at 60 Hz: two steps.
    const TICK: f32 = 1.0 / 60.0;
    use mmd_core::{Bone, BoneTail, Joint, ModelAsset};

    fn bone(name: &str, position: Vec3) -> Bone {
        Bone {
            name: name.to_owned(),
            english_name: String::new(),
            position,
            parent: None,
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
    fn file_angles_compose_yaw_pitch_roll() {
        let angles = Vec3::new(0.4, -0.7, 1.1);
        let expected = Quat::from_rotation_y(angles.y)
            * Quat::from_rotation_x(angles.x)
            * Quat::from_rotation_z(angles.z);
        assert!(euler(angles).dot(expected).abs() > 0.99999);
        let yaw = std::f32::consts::FRAC_PI_2;
        let turned = euler(Vec3::new(0.0, yaw, 0.0)) * Vec3::X;
        assert!(turned.abs_diff_eq(Vec3::new(0.0, 0.0, -1.0), 1.0e-4));
    }

    fn body(bone: u16, shape: u8, size: Vec3, position: Vec3, kind: u8) -> RigidBody {
        RigidBody {
            name: String::new(),
            bone: Some(bone),
            group: 0,
            mask: 0xffff,
            shape,
            size,
            position,
            rotation: Vec3::ZERO,
            mass: 1.0,
            linear_damping: 0.0,
            angular_damping: 0.0,
            restitution: 0.0,
            friction: 0.5,
            body_type: kind,
        }
    }

    #[test]
    fn a_dynamic_sphere_lands_on_the_kinematic_box() {
        let model = ModelAsset {
            name: String::new(),
            english_name: String::new(),
            comment: String::new(),
            vertices: Vec::new(),
            indices: Vec::new(),
            textures: Vec::new(),
            materials: Vec::new(),
            bones: vec![bone("床", Vec3::ZERO), bone("玉", Vec3::new(0.0, 8.0, 0.0))],
            morphs: Vec::new(),
            display_frames: Vec::new(),
            rigid_bodies: vec![
                body(0, 1, Vec3::new(2.0, 0.4, 2.0), Vec3::ZERO, 0),
                body(1, 0, Vec3::new(0.5, 0.5, 0.5), Vec3::new(0.0, 8.0, 0.0), 1),
            ],
            joints: Vec::<Joint>::new(),
        };
        let mut world = PhysicsWorld::new(&model);
        let mut translation = vec![Vec3::ZERO, Vec3::new(0.0, 8.0, 0.0)];
        let mut rotation = vec![Quat::IDENTITY, Quat::IDENTITY];
        let mut skin = vec![Mat4::IDENTITY; 2];
        for _ in 0..90 {
            world.simulate(
                &model,
                &mut translation,
                &mut rotation,
                &mut skin,
                SimulationFlags {
                    enabled: true,
                    playing: true,
                    reset: false,
                },
                TICK,
            );
        }
        assert!(
            translation[1].y > 0.7,
            "sphere fell through the box to {}",
            translation[1].y
        );
        assert!(
            translation[1].y < 4.0,
            "sphere did not fall, height {}",
            translation[1].y
        );
    }

    #[test]
    fn a_skirt_panel_is_pushed_off_a_leg() {
        let mut skirt = body(1, 0, Vec3::splat(0.3), Vec3::new(0.0, 2.0, 0.0), 1);
        skirt.group = 7;
        let model = ModelAsset {
            name: String::new(),
            english_name: String::new(),
            comment: String::new(),
            vertices: Vec::new(),
            indices: Vec::new(),
            textures: Vec::new(),
            materials: Vec::new(),
            bones: vec![
                bone("足", Vec3::new(0.0, 2.0, 0.0)),
                bone("裙", Vec3::new(0.0, 2.0, 0.0)),
            ],
            morphs: Vec::new(),
            display_frames: Vec::new(),
            rigid_bodies: vec![
                body(
                    0,
                    1,
                    Vec3::new(0.15, 4.0, 0.15),
                    Vec3::new(0.0, 2.0, 0.0),
                    0,
                ),
                skirt,
            ],
            joints: Vec::new(),
        };
        let mut world = PhysicsWorld::new(&model);
        let mut translation = vec![Vec3::new(0.0, 2.0, 0.0); 2];
        let mut rotation = vec![Quat::IDENTITY; 2];
        let mut skin = vec![Mat4::IDENTITY; 2];
        for _ in 0..40 {
            world.simulate(
                &model,
                &mut translation,
                &mut rotation,
                &mut skin,
                SimulationFlags {
                    enabled: true,
                    playing: false,
                    reset: false,
                },
                TICK,
            );
        }
        assert!(
            translation[1].x.abs() > 0.2 || translation[1].z.abs() > 0.2,
            "skirt stayed inside the leg at {}",
            translation[1]
        );
    }

    #[test]
    fn a_dropped_sphere_lands_on_the_floor() {
        let model = ModelAsset {
            name: String::new(),
            english_name: String::new(),
            comment: String::new(),
            vertices: Vec::new(),
            indices: Vec::new(),
            textures: Vec::new(),
            materials: Vec::new(),
            bones: vec![bone("玉", Vec3::new(0.0, 6.0, 0.0))],
            morphs: Vec::new(),
            display_frames: Vec::new(),
            rigid_bodies: vec![body(
                0,
                0,
                Vec3::new(0.5, 0.5, 0.5),
                Vec3::new(0.0, 6.0, 0.0),
                1,
            )],
            joints: Vec::<Joint>::new(),
        };
        let mut world = PhysicsWorld::new(&model);
        let mut translation = vec![Vec3::new(0.0, 6.0, 0.0)];
        let mut rotation = vec![Quat::IDENTITY];
        let mut skin = vec![Mat4::IDENTITY];
        for _ in 0..120 {
            world.simulate(
                &model,
                &mut translation,
                &mut rotation,
                &mut skin,
                SimulationFlags {
                    enabled: true,
                    playing: true,
                    reset: false,
                },
                TICK,
            );
        }
        assert!(
            translation[0].y > 0.3 && translation[0].y < 1.2,
            "sphere did not settle on the floor, height {}",
            translation[0].y
        );
    }

    #[test]
    fn equal_joint_limits_hold_a_dynamic_body_up() {
        let model = ModelAsset {
            name: String::new(),
            english_name: String::new(),
            comment: String::new(),
            vertices: Vec::new(),
            indices: Vec::new(),
            textures: Vec::new(),
            materials: Vec::new(),
            bones: vec![bone("親", Vec3::ZERO), bone("髪", Vec3::new(0.0, 5.0, 0.0))],
            morphs: Vec::new(),
            display_frames: Vec::new(),
            rigid_bodies: vec![
                body(0, 1, Vec3::splat(0.2), Vec3::ZERO, 0),
                body(1, 0, Vec3::splat(0.2), Vec3::new(0.0, 5.0, 0.0), 1),
            ],
            joints: vec![Joint {
                name: String::new(),
                kind: 0,
                rigid_a: Some(0),
                rigid_b: Some(1),
                position: Vec3::new(0.0, 5.0, 0.0),
                rotation: Vec3::ZERO,
                position_min: Vec3::ZERO,
                position_max: Vec3::ZERO,
                rotation_min: Vec3::ZERO,
                rotation_max: Vec3::ZERO,
                spring_position: Vec3::ZERO,
                spring_rotation: Vec3::ZERO,
            }],
        };
        let mut world = PhysicsWorld::new(&model);
        let mut translation = vec![Vec3::ZERO, Vec3::new(0.0, 5.0, 0.0)];
        let mut rotation = vec![Quat::IDENTITY; 2];
        let mut skin = vec![Mat4::IDENTITY; 2];
        for _ in 0..30 {
            world.simulate(
                &model,
                &mut translation,
                &mut rotation,
                &mut skin,
                SimulationFlags {
                    enabled: true,
                    playing: true,
                    reset: false,
                },
                TICK,
            );
        }
        assert!(
            translation[1].y > 4.5,
            "locked joint let the body fall to {}",
            translation[1].y
        );
    }

    #[test]
    fn a_locked_joint_gives_like_mikumikudance_but_stays_attached() {
        let model = ModelAsset {
            name: String::new(),
            english_name: String::new(),
            comment: String::new(),
            vertices: Vec::new(),
            indices: Vec::new(),
            textures: Vec::new(),
            materials: Vec::new(),
            bones: vec![
                bone("親", Vec3::new(0.0, 5.0, 0.0)),
                Bone {
                    parent: Some(0),
                    ..bone("房", Vec3::new(1.0, 5.0, 0.0))
                },
            ],
            morphs: Vec::new(),
            display_frames: Vec::new(),
            rigid_bodies: vec![
                body(0, 0, Vec3::splat(0.1), Vec3::new(0.0, 5.0, 0.0), 0),
                RigidBody {
                    // Hair bodies in shipped models are this thick and damp at about 1.
                    linear_damping: 0.999,
                    angular_damping: 0.999,
                    ..body(1, 0, Vec3::splat(0.5), Vec3::new(1.0, 5.0, 0.0), 1)
                },
            ],
            joints: vec![Joint {
                name: String::new(),
                kind: 0,
                rigid_a: Some(0),
                rigid_b: Some(1),
                position: Vec3::new(0.0, 5.0, 0.0),
                rotation: Vec3::ZERO,
                position_min: Vec3::ZERO,
                position_max: Vec3::ZERO,
                rotation_min: Vec3::ZERO,
                rotation_max: Vec3::ZERO,
                spring_position: Vec3::ZERO,
                spring_rotation: Vec3::ZERO,
            }],
        };
        let mut world = PhysicsWorld::new(&model);
        let mut translation = vec![Vec3::new(0.0, 5.0, 0.0), Vec3::new(1.0, 5.0, 0.0)];
        let mut rotation = vec![Quat::IDENTITY; 2];
        let mut skin = vec![Mat4::IDENTITY; 2];
        for _ in 0..300 {
            world.simulate(
                &model,
                &mut translation,
                &mut rotation,
                &mut skin,
                SimulationFlags {
                    enabled: true,
                    playing: true,
                    reset: false,
                },
                TICK,
            );
        }
        // Hair in shipped models locks every joint axis and still hangs in
        // MikuMikuDance, so the limit softness must be kept: the lever sags.
        // The locked linear axes keep the bone at its length from the parent,
        // even where the soft solver lets the bodies drift apart.
        let angle = rotation[1].w.abs().clamp(0.0, 1.0).acos().to_degrees() * 2.0;
        let reach = translation[1].distance(Vec3::new(0.0, 5.0, 0.0));
        assert!(
            (reach - 1.0).abs() < 1.0e-3,
            "a locked link changed length, reach {reach}"
        );
        // One link gives a few degrees; a 20-link chain adds that up into a drape.
        assert!(angle > 2.0, "a locked joint held rigid at {angle} deg");
        assert!(angle < 90.0, "a locked joint fell through to {angle} deg");
        let before = translation[1];
        for _ in 0..10 {
            translation[0] = Vec3::new(0.0, 5.0, 0.0);
            rotation[0] = Quat::IDENTITY;
            world.simulate(
                &model,
                &mut translation,
                &mut rotation,
                &mut skin,
                SimulationFlags {
                    enabled: true,
                    playing: true,
                    reset: false,
                },
                TICK,
            );
        }
        assert!(
            translation[1].distance(before) < 0.02,
            "the sagged body kept moving from {before} to {}",
            translation[1]
        );
    }

    #[test]
    fn a_type2_bone_keeps_its_animated_position() {
        let model = ModelAsset {
            name: String::new(),
            english_name: String::new(),
            comment: String::new(),
            vertices: Vec::new(),
            indices: Vec::new(),
            textures: Vec::new(),
            materials: Vec::new(),
            bones: vec![
                bone("親", Vec3::new(0.0, 5.0, 0.0)),
                bone("裙", Vec3::new(1.0, 5.0, 0.0)),
            ],
            morphs: Vec::new(),
            display_frames: Vec::new(),
            rigid_bodies: vec![
                body(0, 0, Vec3::splat(0.1), Vec3::new(0.0, 5.0, 0.0), 0),
                body(1, 0, Vec3::splat(0.1), Vec3::new(1.0, 5.0, 0.0), 2),
            ],
            joints: vec![Joint {
                name: String::new(),
                kind: 0,
                rigid_a: Some(0),
                rigid_b: Some(1),
                position: Vec3::new(0.0, 5.0, 0.0),
                rotation: Vec3::ZERO,
                position_min: Vec3::ZERO,
                position_max: Vec3::ZERO,
                rotation_min: Vec3::ZERO,
                rotation_max: Vec3::ZERO,
                spring_position: Vec3::ZERO,
                spring_rotation: Vec3::ZERO,
            }],
        };
        let mut world = PhysicsWorld::new(&model);
        let mut translation = vec![Vec3::new(0.0, 5.0, 0.0), Vec3::new(1.0, 5.0, 0.0)];
        let mut rotation = vec![Quat::IDENTITY; 2];
        let mut skin = vec![Mat4::IDENTITY; 2];
        for _ in 0..120 {
            world.simulate(
                &model,
                &mut translation,
                &mut rotation,
                &mut skin,
                SimulationFlags {
                    enabled: true,
                    playing: true,
                    reset: false,
                },
                TICK,
            );
        }
        let angle = rotation[1].w.abs().clamp(0.0, 1.0).acos().to_degrees() * 2.0;
        assert!(
            translation[1].abs_diff_eq(Vec3::new(1.0, 5.0, 0.0), 1.0e-4),
            "a type 2 bone left its animated position for {}",
            translation[1]
        );
        assert!(
            angle > 5.0,
            "a type 2 bone did not take the body's rotation"
        );
    }

    #[test]
    fn a_file_spring_holds_a_ranged_axis() {
        // Bullet drives a spring as a motor, and a motor stops acting at a
        // limit, so the range is wide enough that the lever never reaches it.
        let loose = ranged_drop(0.0);
        let held = ranged_drop(10_000.0);
        assert!(
            held > loose + 0.15,
            "file spring {held} did not hold above the unsprung drop {loose}"
        );
    }

    fn ranged_drop(spring: f32) -> f32 {
        let model = ModelAsset {
            name: String::new(),
            english_name: String::new(),
            comment: String::new(),
            vertices: Vec::new(),
            indices: Vec::new(),
            textures: Vec::new(),
            materials: Vec::new(),
            bones: vec![
                bone("親", Vec3::new(0.0, 5.0, 0.0)),
                bone("房", Vec3::new(1.0, 5.0, 0.0)),
            ],
            morphs: Vec::new(),
            display_frames: Vec::new(),
            rigid_bodies: vec![
                body(0, 0, Vec3::splat(0.1), Vec3::new(0.0, 5.0, 0.0), 0),
                body(1, 0, Vec3::splat(0.1), Vec3::new(1.0, 5.0, 0.0), 1),
            ],
            joints: vec![Joint {
                name: String::new(),
                kind: 0,
                rigid_a: Some(0),
                rigid_b: Some(1),
                position: Vec3::new(0.0, 5.0, 0.0),
                rotation: Vec3::ZERO,
                position_min: Vec3::ZERO,
                position_max: Vec3::ZERO,
                rotation_min: Vec3::new(0.0, 0.0, -3.0),
                rotation_max: Vec3::new(0.0, 0.0, 3.0),
                spring_position: Vec3::ZERO,
                spring_rotation: Vec3::new(0.0, 0.0, spring),
            }],
        };
        let mut world = PhysicsWorld::new(&model);
        let mut translation = vec![Vec3::new(0.0, 5.0, 0.0), Vec3::new(1.0, 5.0, 0.0)];
        let mut rotation = vec![Quat::IDENTITY; 2];
        let mut skin = vec![Mat4::IDENTITY; 2];
        for _ in 0..90 {
            world.simulate(
                &model,
                &mut translation,
                &mut rotation,
                &mut skin,
                SimulationFlags {
                    enabled: true,
                    playing: true,
                    reset: false,
                },
                TICK,
            );
        }
        translation[1].y
    }

    #[test]
    fn a_type2_child_follows_the_parent_chain() {
        let mut child = bone("子", Vec3::new(1.0, 5.0, 0.0));
        child.parent = Some(0);
        let mut tip = bone("先", Vec3::new(2.0, 5.0, 0.0));
        tip.parent = Some(1);
        let model = ModelAsset {
            name: String::new(),
            english_name: String::new(),
            comment: String::new(),
            vertices: Vec::new(),
            indices: Vec::new(),
            textures: Vec::new(),
            materials: Vec::new(),
            bones: vec![bone("親", Vec3::new(0.0, 5.0, 0.0)), child, tip],
            morphs: Vec::new(),
            display_frames: Vec::new(),
            rigid_bodies: vec![
                body(0, 0, Vec3::splat(0.1), Vec3::new(0.0, 5.0, 0.0), 0),
                body(1, 0, Vec3::splat(0.1), Vec3::new(1.0, 5.0, 0.0), 2),
                body(2, 0, Vec3::splat(0.1), Vec3::new(2.0, 5.0, 0.0), 2),
            ],
            joints: vec![
                Joint {
                    name: String::new(),
                    kind: 0,
                    rigid_a: Some(0),
                    rigid_b: Some(1),
                    position: Vec3::new(0.0, 5.0, 0.0),
                    rotation: Vec3::ZERO,
                    position_min: Vec3::ZERO,
                    position_max: Vec3::ZERO,
                    rotation_min: Vec3::new(0.0, 0.0, -1.2),
                    rotation_max: Vec3::new(0.0, 0.0, 1.2),
                    spring_position: Vec3::ZERO,
                    spring_rotation: Vec3::ZERO,
                },
                Joint {
                    name: String::new(),
                    kind: 0,
                    rigid_a: Some(1),
                    rigid_b: Some(2),
                    position: Vec3::new(1.0, 5.0, 0.0),
                    rotation: Vec3::ZERO,
                    position_min: Vec3::ZERO,
                    position_max: Vec3::ZERO,
                    rotation_min: Vec3::ZERO,
                    rotation_max: Vec3::ZERO,
                    spring_position: Vec3::ZERO,
                    spring_rotation: Vec3::splat(1000.0),
                },
            ],
        };
        let mut world = PhysicsWorld::new(&model);
        let mut translation = vec![
            Vec3::new(0.0, 5.0, 0.0),
            Vec3::new(1.0, 5.0, 0.0),
            Vec3::new(2.0, 5.0, 0.0),
        ];
        let mut rotation = vec![Quat::IDENTITY; 3];
        let mut skin = vec![Mat4::IDENTITY; 3];
        for _ in 0..90 {
            world.simulate(
                &model,
                &mut translation,
                &mut rotation,
                &mut skin,
                SimulationFlags {
                    enabled: true,
                    playing: true,
                    reset: false,
                },
                TICK,
            );
        }
        assert!(
            (translation[1] - Vec3::new(1.0, 5.0, 0.0)).length() < 0.05,
            "aligned root left its parent at {}",
            translation[1]
        );
        assert!(
            translation[2].y < 4.7,
            "aligned tip did not hang from the rotated parent at {}",
            translation[2]
        );
    }

    fn fall_after(damping: f32) -> f32 {
        let mut sphere = body(0, 0, Vec3::splat(0.5), Vec3::new(0.0, 60.0, 0.0), 1);
        sphere.linear_damping = damping;
        let model = ModelAsset {
            name: String::new(),
            english_name: String::new(),
            comment: String::new(),
            vertices: Vec::new(),
            indices: Vec::new(),
            textures: Vec::new(),
            materials: Vec::new(),
            bones: vec![bone("玉", Vec3::new(0.0, 60.0, 0.0))],
            morphs: Vec::new(),
            display_frames: Vec::new(),
            rigid_bodies: vec![sphere],
            joints: Vec::<Joint>::new(),
        };
        let mut world = PhysicsWorld::new(&model);
        let mut translation = vec![Vec3::new(0.0, 60.0, 0.0)];
        let mut rotation = vec![Quat::IDENTITY];
        let mut skin = vec![Mat4::IDENTITY];
        for _ in 0..30 {
            world.simulate(
                &model,
                &mut translation,
                &mut rotation,
                &mut skin,
                SimulationFlags {
                    enabled: true,
                    playing: true,
                    reset: false,
                },
                TICK,
            );
        }
        60.0 - translation[0].y
    }

    #[test]
    fn file_damping_uses_the_power_formula() {
        // (1 - 0.9)^t keeps 10% of the velocity per second. The old linear
        // formula kept about 99% per step and barely slowed anything.
        let free = fall_after(0.0);
        let damped = fall_after(0.9);
        assert!(free > 10.0, "free fall only dropped {free}");
        // Analytically about 8.7 for damping 0.9 against 12.25 undamped.
        assert!(
            damped < free * 0.8,
            "damping 0.9 dropped {damped}, free fall {free}"
        );
        assert!(damped > 0.5, "damping 0.9 froze the body at {damped}");
    }

    #[test]
    fn file_damping_is_clamped_like_bullet() {
        assert_eq!(file_damping(2.0), 1.0);
        assert_eq!(file_damping(-1.0), 0.0);
        assert_eq!(file_damping(f32::NAN), 0.0);
        assert_eq!(file_damping(0.5), 0.5);
    }

    #[test]
    fn an_unsolved_child_keeps_its_offset_from_a_solved_parent() {
        let mut child = bone("先", Vec3::new(0.0, 4.0, 0.0));
        child.parent = Some(0);
        let model = ModelAsset {
            name: String::new(),
            english_name: String::new(),
            comment: String::new(),
            vertices: Vec::new(),
            indices: Vec::new(),
            textures: Vec::new(),
            materials: Vec::new(),
            bones: vec![bone("玉", Vec3::new(0.0, 6.0, 0.0)), child],
            morphs: Vec::new(),
            display_frames: Vec::new(),
            rigid_bodies: vec![body(0, 0, Vec3::splat(0.5), Vec3::new(0.0, 6.0, 0.0), 1)],
            joints: Vec::<Joint>::new(),
        };
        let mut world = PhysicsWorld::new(&model);
        let mut translation = vec![Vec3::new(0.0, 6.0, 0.0), Vec3::new(0.0, 4.0, 0.0)];
        let mut rotation = vec![Quat::IDENTITY; 2];
        let mut skin = vec![Mat4::IDENTITY; 2];
        world.simulate(
            &model,
            &mut translation,
            &mut rotation,
            &mut skin,
            SimulationFlags {
                enabled: true,
                playing: true,
                reset: false,
            },
            STEP_SECONDS,
        );
        for _ in 0..20 {
            translation[1] = Vec3::new(0.0, 4.0, 0.0);
            translation[0] = Vec3::new(0.0, 6.0, 0.0);
            rotation = vec![Quat::IDENTITY; 2];
            world.simulate(
                &model,
                &mut translation,
                &mut rotation,
                &mut skin,
                SimulationFlags {
                    enabled: true,
                    playing: true,
                    reset: false,
                },
                TICK,
            );
        }
        assert!(
            translation[0].y < 5.5,
            "sphere did not fall: {}",
            translation[0]
        );
        let offset = rotation[0].inverse() * (translation[1] - translation[0]);
        assert!(
            offset.abs_diff_eq(Vec3::new(0.0, -2.0, 0.0), 1.0e-3),
            "child left its parent: offset {offset}"
        );
        let expected = Mat4::from_rotation_translation(rotation[1], translation[1])
            * Mat4::from_translation(Vec3::new(0.0, -4.0, 0.0));
        assert!(skin[1].abs_diff_eq(expected, 1.0e-4));
    }
}
