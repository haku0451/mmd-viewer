//! CXX bridge to the Bullet 3.25 world in `bullet_world.cpp`.

#[cxx::bridge]
pub mod ffi {
    struct PoseF {
        x: f32,
        y: f32,
        z: f32,
        qx: f32,
        qy: f32,
        qz: f32,
        qw: f32,
    }

    struct BodyDesc {
        tx: f32,
        ty: f32,
        tz: f32,
        qx: f32,
        qy: f32,
        qz: f32,
        qw: f32,
        sx: f32,
        sy: f32,
        sz: f32,
        mass: f32,
        linear_damping: f32,
        angular_damping: f32,
        friction: f32,
        restitution: f32,
        shape: i32,
        kind: i32,
        group: i32,
        mask: i32,
    }

    struct JointDesc {
        body_a: i32,
        body_b: i32,
        a_tx: f32,
        a_ty: f32,
        a_tz: f32,
        a_qx: f32,
        a_qy: f32,
        a_qz: f32,
        a_qw: f32,
        b_tx: f32,
        b_ty: f32,
        b_tz: f32,
        b_qx: f32,
        b_qy: f32,
        b_qz: f32,
        b_qw: f32,
        lin_min_x: f32,
        lin_min_y: f32,
        lin_min_z: f32,
        lin_max_x: f32,
        lin_max_y: f32,
        lin_max_z: f32,
        ang_min_x: f32,
        ang_min_y: f32,
        ang_min_z: f32,
        ang_max_x: f32,
        ang_max_y: f32,
        ang_max_z: f32,
        spring_lin_x: f32,
        spring_lin_y: f32,
        spring_lin_z: f32,
        spring_ang_x: f32,
        spring_ang_y: f32,
        spring_ang_z: f32,
    }

    unsafe extern "C++" {
        include!("mmd-physics/src/bullet_world.h");

        type World;

        fn create_world() -> UniquePtr<World>;
        fn add_body(self: Pin<&mut World>, desc: &BodyDesc) -> u32;
        fn add_floor(self: Pin<&mut World>);
        fn add_joint(self: Pin<&mut World>, desc: &JointDesc);
        fn step(self: Pin<&mut World>, dt: f32);
        fn set_pose(self: Pin<&mut World>, index: u32, pose: &PoseF, reset_velocity: bool);
        fn pose(self: &World, index: u32) -> PoseF;
        fn linear_speed(self: &World, index: u32) -> f32;
        fn angular_speed(self: &World, index: u32) -> f32;
        fn zero_velocity(self: Pin<&mut World>, index: u32);
    }
}
