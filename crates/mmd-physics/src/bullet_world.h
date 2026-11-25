#pragma once

#include <cstdint>
#include <memory>

// Defined by the CXX bridge. The generated header completes these types.
struct PoseF;
struct BodyDesc;
struct JointDesc;

// One Bullet discrete world. The cpp file owns the Bullet objects.
class World {
public:
    World();
    ~World();

    World(const World&) = delete;
    World& operator=(const World&) = delete;

    std::uint32_t add_body(const BodyDesc& desc);
    void add_floor();
    void add_joint(const JointDesc& desc);
    void step(float dt);
    void set_pose(std::uint32_t index, const PoseF& pose, bool reset_velocity);
    PoseF pose(std::uint32_t index) const;
    float linear_speed(std::uint32_t index) const;
    float angular_speed(std::uint32_t index) const;
    void zero_velocity(std::uint32_t index);

private:
    struct Impl;
    Impl* impl;
};

std::unique_ptr<World> create_world();
