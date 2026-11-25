#include "mmd-physics/src/bullet.rs.h"

#include "btBulletDynamicsCommon.h"

#include <algorithm>
#include <cmath>
#include <memory>
#include <utility>
#include <vector>

namespace {

btTransform make_transform(float tx, float ty, float tz, float qx, float qy, float qz, float qw) {
    btTransform transform;
    transform.setIdentity();
    transform.setOrigin(btVector3(tx, ty, tz));
    btQuaternion rotation(qx, qy, qz, qw);
    if (rotation.length2() < 1.0e-12f) {
        rotation = btQuaternion::getIdentity();
    } else {
        rotation.normalize();
    }
    transform.setRotation(rotation);
    return transform;
}

btTransform make_transform(const PoseF& pose) {
    return make_transform(pose.x, pose.y, pose.z, pose.qx, pose.qy, pose.qz, pose.qw);
}

float finite_or(float value, float fallback) {
    return std::isfinite(value) ? value : fallback;
}

}

struct World::Impl {
    btDefaultCollisionConfiguration configuration;
    btCollisionDispatcher dispatcher;
    btDbvtBroadphase broadphase;
    btSequentialImpulseConstraintSolver solver;
    btDiscreteDynamicsWorld world;
    std::vector<std::unique_ptr<btCollisionShape>> shapes;
    std::vector<std::unique_ptr<btDefaultMotionState>> motions;
    std::vector<std::unique_ptr<btRigidBody>> bodies;
    std::vector<std::unique_ptr<btTypedConstraint>> constraints;

    Impl()
        : dispatcher(&configuration),
          world(&dispatcher, &broadphase, &solver, &configuration) {
        // MikuMikuDance scales 9.8 m/s^2 by 10 model units per meter.
        world.setGravity(btVector3(0.0f, -98.0f, 0.0f));
        // Steps are 1/120 s. With joint limit ERP 0.1 (set per joint) the
        // correction per second matches Bullet's default of 0.2 at 1/60 s,
        // which is the softness models are made for: hair whose joints lock
        // every axis only hangs because a locked limit gives. The smaller
        // step halves the resting tremor of cross-linked skirts and the depth
        // of contacts during a dance. 7 iterations per step is slightly
        // stiffer than the default 10 per 1/60 s, so a short locked chain
        // such as an ahoge keeps its curl (tuned on shipped models).
        world.getSolverInfo().m_numIterations = 7;
    }

    btRigidBody* body_at(std::uint32_t index) const {
        if (index >= bodies.size()) {
            return nullptr;
        }
        return bodies[index].get();
    }
};

World::World() : impl(new Impl) {}

World::~World() {
    for (const auto& constraint : impl->constraints) {
        impl->world.removeConstraint(constraint.get());
    }
    for (const auto& body : impl->bodies) {
        impl->world.removeRigidBody(body.get());
    }
    delete impl;
}

std::unique_ptr<World> create_world() { return std::make_unique<World>(); }

std::uint32_t World::add_body(const BodyDesc& desc) {
    const float sx = std::max(std::fabs(finite_or(desc.sx, 0.01f)), 0.01f);
    const float sy = std::max(std::fabs(finite_or(desc.sy, 0.01f)), 0.01f);
    const float sz = std::max(std::fabs(finite_or(desc.sz, 0.01f)), 0.01f);
    btCollisionShape* shape = nullptr;
    switch (desc.shape) {
        case 0:
            shape = new btSphereShape(sx);
            break;
        case 1:
            shape = new btBoxShape(btVector3(sx, sy, sz));
            break;
        case 2:
            // PMX height is the cylinder between the caps. A zero height is a sphere.
            if (std::fabs(desc.sy) <= 1.0e-4f) {
                shape = new btSphereShape(sx);
            } else {
                shape = new btCapsuleShape(sx, std::fabs(desc.sy));
            }
            break;
        default:
            shape = new btSphereShape(sx);
            break;
    }
    impl->shapes.emplace_back(shape);

    const bool kinematic = desc.kind != 1 && desc.kind != 2;
    const float mass = kinematic ? 0.0f : std::max(finite_or(desc.mass, 1.0f), 0.001f);
    btVector3 inertia(0.0f, 0.0f, 0.0f);
    if (mass > 0.0f) {
        shape->calculateLocalInertia(mass, inertia);
    }
    const btTransform start = make_transform(desc.tx, desc.ty, desc.tz, desc.qx, desc.qy, desc.qz, desc.qw);
    auto motion = std::make_unique<btDefaultMotionState>(start);
    btRigidBody::btRigidBodyConstructionInfo info(mass, motion.get(), shape, inertia);
    info.m_linearDamping = finite_or(desc.linear_damping, 0.0f);
    info.m_angularDamping = finite_or(desc.angular_damping, 0.0f);
    info.m_friction = std::max(finite_or(desc.friction, 0.5f), 0.0f);
    info.m_restitution = std::clamp(finite_or(desc.restitution, 0.0f), 0.0f, 1.0f);
    // MikuMikuDance-compatible runtimes (MMDAgent, saba) enable this. It
    // stops slow residual drift so a resting chain does not creep.
    info.m_additionalDamping = !kinematic;
    auto body = std::make_unique<btRigidBody>(info);
    body->setActivationState(DISABLE_DEACTIVATION);
    if (kinematic) {
        body->setCollisionFlags(body->getCollisionFlags() | btCollisionObject::CF_KINEMATIC_OBJECT);
    }
    impl->world.addRigidBody(body.get(), desc.group, desc.mask);
    impl->motions.push_back(std::move(motion));
    impl->bodies.push_back(std::move(body));
    return static_cast<std::uint32_t>(impl->bodies.size() - 1);
}

void World::add_floor() {
    auto shape = std::make_unique<btBoxShape>(btVector3(80.0f, 1.0f, 80.0f));
    const btTransform start = make_transform(0.0f, -1.0f, 0.0f, 0.0f, 0.0f, 0.0f, 1.0f);
    auto motion = std::make_unique<btDefaultMotionState>(start);
    btRigidBody::btRigidBodyConstructionInfo info(0.0f, motion.get(), shape.get(), btVector3(0.0f, 0.0f, 0.0f));
    info.m_friction = 0.9f;
    info.m_restitution = 0.0f;
    auto body = std::make_unique<btRigidBody>(info);
    body->setActivationState(DISABLE_DEACTIVATION);
    // Group bit 16 is the floor, outside the 16 PMX groups. Every dynamic mask includes it.
    impl->world.addRigidBody(body.get(), 1 << 16, -1);
    impl->shapes.push_back(std::move(shape));
    impl->motions.push_back(std::move(motion));
    impl->bodies.push_back(std::move(body));
}

void World::add_joint(const JointDesc& desc) {
    if (desc.body_a < 0 || desc.body_b < 0) {
        return;
    }
    btRigidBody* body_a = impl->body_at(static_cast<std::uint32_t>(desc.body_a));
    btRigidBody* body_b = impl->body_at(static_cast<std::uint32_t>(desc.body_b));
    if (body_a == nullptr || body_b == nullptr || body_a == body_b) {
        return;
    }
    const btTransform frame_a = make_transform(desc.a_tx, desc.a_ty, desc.a_tz, desc.a_qx, desc.a_qy, desc.a_qz, desc.a_qw);
    const btTransform frame_b = make_transform(desc.b_tx, desc.b_ty, desc.b_tz, desc.b_qx, desc.b_qy, desc.b_qz, desc.b_qw);
    auto* spring = new btGeneric6DofSpringConstraint(*body_a, *body_b, frame_a, frame_b, true);
    spring->setLinearLowerLimit(btVector3(desc.lin_min_x, desc.lin_min_y, desc.lin_min_z));
    spring->setLinearUpperLimit(btVector3(desc.lin_max_x, desc.lin_max_y, desc.lin_max_z));
    spring->setAngularLowerLimit(btVector3(desc.ang_min_x, desc.ang_min_y, desc.ang_min_z));
    spring->setAngularUpperLimit(btVector3(desc.ang_max_x, desc.ang_max_y, desc.ang_max_z));
    const float linear_spring[3] = {desc.spring_lin_x, desc.spring_lin_y, desc.spring_lin_z};
    const float angular_spring[3] = {desc.spring_ang_x, desc.spring_ang_y, desc.spring_ang_z};
    for (int axis = 0; axis < 3; ++axis) {
        if (std::isfinite(linear_spring[axis]) && linear_spring[axis] > 0.0f) {
            spring->enableSpring(axis, true);
            spring->setStiffness(axis, linear_spring[axis]);
        }
        if (std::isfinite(angular_spring[axis]) && angular_spring[axis] > 0.0f) {
            spring->enableSpring(axis + 3, true);
            spring->setStiffness(axis + 3, angular_spring[axis]);
        }
    }
    // Half of Bullet's 0.2, because a step is half as long. See the world setup.
    for (int axis = 0; axis < 6; ++axis) {
        spring->setParam(BT_CONSTRAINT_STOP_ERP, 0.1f, axis);
    }
    spring->setEquilibriumPoint();
    // Linked bodies still collide when the file masks say so.
    impl->world.addConstraint(spring, false);
    impl->constraints.emplace_back(spring);
}

void World::step(float dt) {
    if (!(dt > 0.0f)) {
        return;
    }
    impl->world.stepSimulation(dt, 0, dt);
}

void World::set_pose(std::uint32_t index, const PoseF& pose, bool reset_velocity) {
    btRigidBody* body = impl->body_at(index);
    if (body == nullptr) {
        return;
    }
    const btTransform transform = make_transform(pose);
    if (body->isKinematicObject() && !reset_velocity) {
        if (btMotionState* motion = body->getMotionState()) {
            motion->setWorldTransform(transform);
        }
    } else {
        body->setWorldTransform(transform);
        if (btMotionState* motion = body->getMotionState()) {
            motion->setWorldTransform(transform);
        }
    }
    if (reset_velocity) {
        body->setLinearVelocity(btVector3(0.0f, 0.0f, 0.0f));
        body->setAngularVelocity(btVector3(0.0f, 0.0f, 0.0f));
        body->clearForces();
    }
    body->activate(true);
}

PoseF World::pose(std::uint32_t index) const {
    PoseF pose{0.0f, 0.0f, 0.0f, 0.0f, 0.0f, 0.0f, 1.0f};
    btRigidBody* body = impl->body_at(index);
    if (body == nullptr) {
        return pose;
    }
    const btTransform& transform = body->getWorldTransform();
    const btVector3& origin = transform.getOrigin();
    const btQuaternion rotation = transform.getRotation();
    pose.x = origin.x();
    pose.y = origin.y();
    pose.z = origin.z();
    pose.qx = rotation.x();
    pose.qy = rotation.y();
    pose.qz = rotation.z();
    pose.qw = rotation.w();
    return pose;
}

float World::linear_speed(std::uint32_t index) const {
    btRigidBody* body = impl->body_at(index);
    if (body == nullptr) {
        return 0.0f;
    }
    return body->getLinearVelocity().length();
}

float World::angular_speed(std::uint32_t index) const {
    btRigidBody* body = impl->body_at(index);
    if (body == nullptr) {
        return 0.0f;
    }
    return body->getAngularVelocity().length();
}

void World::zero_velocity(std::uint32_t index) {
    btRigidBody* body = impl->body_at(index);
    if (body == nullptr) {
        return;
    }
    body->setLinearVelocity(btVector3(0.0f, 0.0f, 0.0f));
    body->setAngularVelocity(btVector3(0.0f, 0.0f, 0.0f));
    body->clearForces();
}
