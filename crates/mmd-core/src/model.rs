//! Runtime model asset.
//!
//! This is the in-memory model after a PMX or PMD file has been parsed. It is
//! not the file layout. Positions and bone quaternions stay in left-handed,
//! Y-up model space.

use glam::{Quat, Vec2, Vec3, Vec4};

/// A parsed model that can be evaluated and drawn.
#[derive(Clone, Debug)]
pub struct ModelAsset {
    /// Name from the file. It may be empty.
    pub name: String,
    /// English name from the file. It may be empty.
    pub english_name: String,
    pub comment: String,
    /// Vertices in file order.
    pub vertices: Vec<Vertex>,
    /// Triangle indices. Three entries form one face.
    pub indices: Vec<u32>,
    /// Texture paths as stored, including a `*` sphere suffix when present.
    pub textures: Vec<String>,
    /// Materials in draw order.
    pub materials: Vec<Material>,
    /// Bones. A parent index refers to an earlier or later bone in this list.
    pub bones: Vec<Bone>,
    /// Morphs in file order. Group morphs are applied in this order.
    pub morphs: Vec<Morph>,
    /// Display frames from the file. The viewer does not edit them.
    pub display_frames: Vec<DisplayFrame>,
    /// Rigid bodies. The physics crate simulates them.
    pub rigid_bodies: Vec<RigidBody>,
    pub joints: Vec<Joint>,
}

/// One skinned vertex.
#[derive(Clone, Debug)]
pub struct Vertex {
    /// Position in model space.
    pub position: Vec3,
    /// Normal in model space.
    pub normal: Vec3,
    /// Primary texture coordinate.
    pub uv: Vec2,
    pub skin: Skinning,
    pub edge_scale: f32,
}

/// How a vertex is attached to bones.
#[derive(Clone, Debug)]
pub enum Skinning {
    /// One bone.
    Bdef1 { bone: u16 },
    /// Two bones and the weight of the first.
    Bdef2 {
        bone0: u16,
        bone1: u16,
        /// Weight of `bone0`. The second weight is `1 - weight0`.
        weight0: f32,
    },
    /// Up to four bones.
    Bdef4 {
        bones: [u16; 4],
        /// Weights in the same order as `bones`.
        weights: [f32; 4],
    },
    /// Spherical deform between two bones.
    Sdef {
        bone0: u16,
        bone1: u16,
        weight0: f32,
        /// SDEF center in model space.
        c: Vec3,
        /// SDEF R0 in model space.
        r0: Vec3,
        /// SDEF R1 in model space.
        r1: Vec3,
    },
    /// Dual-quaternion blend of up to four bones.
    Qdef {
        bones: [u16; 4],
        /// Weights in the same order as `bones`.
        weights: [f32; 4],
    },
}

/// Draw-state for one material.
#[derive(Clone, Debug)]
pub struct Material {
    pub name: String,
    /// Diffuse color and alpha.
    pub diffuse: [f32; 4],
    pub specular: [f32; 3],
    pub specular_power: f32,
    pub ambient: [f32; 3],
    /// PMX draw flags. See [`Material::double_sided`] and the related methods.
    pub flags: u8,
    /// Edge color and alpha.
    pub edge_color: [f32; 4],
    pub edge_size: f32,
    /// Color texture, or `None` when the file index is -1.
    pub texture: Option<u32>,
    /// Sphere texture, or `None` when the file index is -1.
    pub sphere: Option<u32>,
    pub sphere_mode: SphereMode,
    pub toon: ToonSource,
    /// Number of indices consumed from the model index buffer.
    pub index_count: u32,
}

impl Material {
    pub fn double_sided(&self) -> bool {
        self.flags & 0x01 != 0
    }

    pub fn ground_shadow(&self) -> bool {
        self.flags & 0x02 != 0
    }

    /// The material is drawn into the self-shadow map.
    pub fn cast_shadow(&self) -> bool {
        self.flags & 0x04 != 0
    }

    /// The material is darkened by the self-shadow map.
    pub fn receive_shadow(&self) -> bool {
        self.flags & 0x08 != 0
    }

    pub fn draw_edge(&self) -> bool {
        self.flags & 0x10 != 0
    }
}

/// Sphere-map combine mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SphereMode {
    None,
    Multiply,
    Add,
}

/// Where the toon ramp comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToonSource {
    /// A texture index in [`ModelAsset::textures`], or none.
    Texture(Option<u32>),
    /// A shared toon slot, 0 through 9.
    Shared(u8),
}

/// One bone in model space.
#[derive(Clone, Debug)]
pub struct Bone {
    /// Bone name used by VMD keys.
    pub name: String,
    pub english_name: String,
    /// Rest position in model space. This is absolute, not a parent offset.
    pub position: Vec3,
    /// Parent bone, or `None` for a root.
    pub parent: Option<u16>,
    /// Deform layer. Lower layers are evaluated first.
    pub layer: i32,
    /// Raw PMX flag word. PMD import fills the bits it can represent.
    pub flags: u16,
    /// Tail used by display and by PMD fixed-axis import.
    pub tail: BoneTail,
    /// Append parent, when the bone has a grant transform.
    pub append: Option<Append>,
    /// Fixed axis in model space.
    pub fixed_axis: Option<Vec3>,
    /// Local axes stored for a later gizmo. They do not change skinning.
    pub local_axis: Option<LocalAxis>,
    /// External-parent key. The evaluator does not apply an external parent.
    pub external_parent: Option<i32>,
    pub ik: Option<Ik>,
}

/// Bone tail stored by PMX.
#[derive(Clone, Debug)]
pub enum BoneTail {
    /// Offset from the bone origin.
    Offset(Vec3),
    Bone(u16),
}

/// Append (付与) parent.
#[derive(Clone, Debug)]
pub struct Append {
    /// Bone whose motion is granted.
    pub parent: u16,
    /// Grant ratio. The file allows values outside 0 to 1.
    pub rate: f32,
    /// When set, the grant rotation is applied in the bone's local frame.
    pub local: bool,
}

/// Local X and Z axes from the file.
#[derive(Clone, Debug)]
pub struct LocalAxis {
    pub x: Vec3,
    pub z: Vec3,
}

/// IK chain owned by the IK bone.
#[derive(Clone, Debug)]
pub struct Ik {
    /// Effector bone.
    pub target: u16,
    /// CCD iteration count.
    pub iterations: u32,
    /// Maximum rotation applied to one link in one iteration, in radians.
    pub limit_angle: f32,
    /// Links from the child toward the root.
    pub links: Vec<IkLink>,
}

#[derive(Clone, Debug)]
pub struct IkLink {
    pub bone: u16,
    /// Hinge limits in radians. The X component is the one the solver uses.
    pub limits: Option<(Vec3, Vec3)>,
}

/// A morph target.
#[derive(Clone, Debug)]
pub enum Morph {
    /// Adds weight to other morphs.
    Group {
        name: String,
        english_name: String,
        /// Panel category from the file.
        panel: u8,
        /// Target morph and the weight added at full strength.
        offsets: Vec<(u16, f32)>,
    },
    /// Moves vertices.
    Vertex {
        name: String,
        english_name: String,
        /// Panel category from the file.
        panel: u8,
        /// Vertex index and position offset.
        offsets: Vec<(u32, Vec3)>,
    },
    /// Adds a local translation and rotation to bones.
    Bone {
        name: String,
        english_name: String,
        /// Panel category from the file.
        panel: u8,
        /// Bone index, translation, and rotation.
        offsets: Vec<(u16, Vec3, Quat)>,
    },
    /// Moves the primary UV or an extra UV channel.
    Uv {
        name: String,
        english_name: String,
        /// Panel category from the file.
        panel: u8,
        channel: UvChannel,
        /// Vertex index and UV offset.
        offsets: Vec<(u32, Vec4)>,
    },
    /// Multiplies or adds material colors.
    Material {
        name: String,
        english_name: String,
        /// Panel category from the file.
        panel: u8,
        /// Per-material offsets. A material index of `None` means every material.
        offsets: Vec<MaterialOffset>,
    },
}

impl Morph {
    /// Name used by VMD morph keys.
    pub fn name(&self) -> &str {
        match self {
            Self::Group { name, .. }
            | Self::Vertex { name, .. }
            | Self::Bone { name, .. }
            | Self::Uv { name, .. }
            | Self::Material { name, .. } => name,
        }
    }
}

/// UV channel targeted by a UV morph.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UvChannel {
    /// The texture coordinate that the renderer samples.
    Primary,
    /// Extra UV 1. It is parsed and is not sampled.
    Extra1,
    /// Extra UV 2. It is parsed and is not sampled.
    Extra2,
    /// Extra UV 3. It is parsed and is not sampled.
    Extra3,
    /// Extra UV 4. It is parsed and is not sampled.
    Extra4,
}

/// One material-morph record.
#[derive(Clone, Debug)]
pub struct MaterialOffset {
    /// Target material, or every material when `None`.
    pub material: Option<u32>,
    /// Multiply when false, add when true.
    pub add: bool,
    pub diffuse: [f32; 4],
    pub specular: [f32; 3],
    pub specular_power: f32,
    pub ambient: [f32; 3],
    pub edge_color: [f32; 4],
    pub edge_size: f32,
    pub texture: [f32; 4],
    pub sphere: [f32; 4],
    pub toon: [f32; 4],
}

/// A display frame entry.
#[derive(Clone, Debug)]
pub struct DisplayFrame {
    pub name: String,
    /// Bone or morph indices. `true` means the index is a morph.
    pub entries: Vec<(bool, u32)>,
}

/// A rigid body stored from the file.
///
/// `mmd-physics` steps these values. This crate only stores them.
#[derive(Clone, Debug)]
pub struct RigidBody {
    pub name: String,
    pub bone: Option<u16>,
    pub group: u8,
    pub mask: u16,
    /// Shape kind from the file.
    pub shape: u8,
    pub size: Vec3,
    /// Position in model space.
    pub position: Vec3,
    /// Rotation in radians.
    pub rotation: Vec3,
    pub mass: f32,
    pub linear_damping: f32,
    pub angular_damping: f32,
    pub restitution: f32,
    pub friction: f32,
    /// Body type from the file.
    pub body_type: u8,
}

/// A physics joint stored from the file.
///
/// `mmd-physics` builds a Bullet 6DoF spring joint from these values.
#[derive(Clone, Debug)]
pub struct Joint {
    pub name: String,
    /// Joint type from the file.
    pub kind: u8,
    pub rigid_a: Option<u32>,
    pub rigid_b: Option<u32>,
    /// Position in model space.
    pub position: Vec3,
    /// Rotation in radians.
    pub rotation: Vec3,
    pub position_min: Vec3,
    pub position_max: Vec3,
    pub rotation_min: Vec3,
    pub rotation_max: Vec3,
    pub spring_position: Vec3,
    pub spring_rotation: Vec3,
}

impl ModelAsset {
    pub fn bone_index(&self, name: &str) -> Option<usize> {
        self.bones.iter().position(|bone| bone.name == name)
    }

    pub fn morph_index(&self, name: &str) -> Option<usize> {
        self.morphs.iter().position(|morph| morph.name() == name)
    }
}
