// Model, shadow, edge, and ground-shadow passes.
//
// Skinning follows the CPU formulas in mmd-animation: BDEF blends transformed
// points, SDEF adds the R-point correction, and QDEF blends dual quaternions.
//
// Material shading follows MikuMikuDance's default shader (full.fx):
//   base     = saturate(diffuse * light + ambient) * texture, then the sphere
//              map (view-space normal) is added or multiplied in;
//   specular = pow(max(0, N·H), power) * specular * light;
//   no self shadow:  base * toon(0.5, 0.5 - N·L * 0.5) + specular;
//   self shadow:     mix(base * toon(bottom), base + specular,
//                        min(saturate(N·L * 3), shadow map)).
// MME's AMBIENT semantic returns the PMX diffuse and EMISSIVE the PMX
// ambient, which is why diffuse is scaled by the light and ambient is not.
// The light is MikuMikuDance's default (154, 154, 154). Toon bitmaps are
// light at the top and dark at the bottom; the shaded side takes the warm
// bottom row of a skin toon.

struct CameraUniform {
    clip_from_world: mat4x4<f32>,
    light_clip: mat4x4<f32>,
    view_from_world: mat4x4<f32>,
    light_dir: vec4<f32>,
    camera_pos: vec4<f32>,
}

// MikuMikuDance's default light color, (154, 154, 154) of 255.
const LIGHT_COLOR: vec3<f32> = vec3<f32>(0.6039216, 0.6039216, 0.6039216);
// full.fx multiplies N·L by this before it limits the self-shadow term.
const TOON_SHADOW_SCALE: f32 = 3.0;

struct Bone {
    skin: mat4x4<f32>,
    rotation: vec4<f32>,
    translation: vec4<f32>,
}

struct MaterialUniform {
    diffuse: vec4<f32>,
    specular: vec4<f32>,
    ambient: vec4<f32>,
    edge_color: vec4<f32>,
    texture_factor: vec4<f32>,
    sphere_factor: vec4<f32>,
    params: vec4<f32>,
}

struct VertexIn {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) bones: vec4<u32>,
    @location(4) weights: vec4<f32>,
    @location(5) skin_kind: u32,
    @location(6) sdef_c: vec4<f32>,
    @location(7) sdef_r0: vec3<f32>,
    @location(8) sdef_r1: vec3<f32>,
    @location(9) edge_scale: f32,
}

struct VertexOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world_pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) shadow_clip: vec4<f32>,
}

@group(0) @binding(0) var<uniform> camera: CameraUniform;
@group(0) @binding(1) var<storage, read> bones: array<Bone>;
@group(0) @binding(2) var<uniform> material: MaterialUniform;

@group(1) @binding(0) var color_tex: texture_2d<f32>;
@group(1) @binding(1) var color_samp: sampler;
@group(1) @binding(2) var toon_tex: texture_2d<f32>;
@group(1) @binding(3) var toon_samp: sampler;
@group(1) @binding(4) var sphere_tex: texture_2d<f32>;
@group(1) @binding(5) var sphere_samp: sampler;
@group(1) @binding(6) var shadow_tex: texture_depth_2d;
@group(1) @binding(7) var shadow_samp: sampler_comparison;

fn quat_mul(a: vec4<f32>, b: vec4<f32>) -> vec4<f32> {
    return vec4<f32>(
        a.w * b.xyz + b.w * a.xyz + cross(a.xyz, b.xyz),
        a.w * b.w - dot(a.xyz, b.xyz),
    );
}

fn quat_slerp(a_in: vec4<f32>, b_in: vec4<f32>, t: f32) -> vec4<f32> {
    var b = b_in;
    var dot_ab = dot(a_in, b);
    if (dot_ab < 0.0) {
        b = -b;
        dot_ab = -dot_ab;
    }
    if (dot_ab > 0.9995) {
        return normalize(mix(a_in, b, t));
    }
    let theta = acos(clamp(dot_ab, -1.0, 1.0));
    let s = sin(theta);
    return sin((1.0 - t) * theta) / s * a_in + sin(t * theta) / s * b;
}

fn rotate_vector(q: vec4<f32>, v: vec3<f32>) -> vec3<f32> {
    return quat_mul(quat_mul(q, vec4<f32>(v, 0.0)), vec4<f32>(-q.xyz, q.w)).xyz;
}

fn bdef_matrix(ids: vec4<u32>, weights: vec4<f32>) -> mat4x4<f32> {
    return bones[ids.x].skin * weights.x
        + bones[ids.y].skin * weights.y
        + bones[ids.z].skin * weights.z
        + bones[ids.w].skin * weights.w;
}

fn sdef_position(input: VertexIn) -> vec3<f32> {
    let w0 = input.sdef_c.w;
    let w1 = 1.0 - w0;
    let m0 = bones[input.bones.x].skin;
    let m1 = bones[input.bones.y].skin;
    let r0 = bones[input.bones.x].rotation;
    let r1 = bones[input.bones.y].rotation;
    let rotation = quat_slerp(r0, r1, w1);
    let c = input.sdef_c.xyz;
    let center = (m0 * vec4<f32>(c, 1.0)).xyz * w0 + (m1 * vec4<f32>(c, 1.0)).xyz * w1;
    let blended_r = (m0 * vec4<f32>(input.sdef_r0, 1.0)).xyz * w0
        + (m1 * vec4<f32>(input.sdef_r1, 1.0)).xyz * w1;
    let rest_r = input.sdef_r0 * w0 + input.sdef_r1 * w1;
    let predicted = center + rotate_vector(rotation, rest_r - c);
    return center + rotate_vector(rotation, input.position - c) + (blended_r - predicted);
}

fn qdef_accumulate(real: ptr<function, vec4<f32>>, dual: ptr<function, vec4<f32>>, anchor: ptr<function, vec4<f32>>, have_anchor: ptr<function, bool>, bone: u32, weight: f32) {
    if (weight == 0.0) {
        return;
    }
    var rotation = bones[bone].rotation;
    let translation = bones[bone].translation.xyz;
    if (*have_anchor && dot(rotation, *anchor) < 0.0) {
        rotation = -rotation;
    }
    if (!*have_anchor) {
        *anchor = rotation;
        *have_anchor = true;
    }
    let tq = vec4<f32>(translation, 0.0);
    *real = *real + rotation * weight;
    *dual = *dual + quat_mul(tq, rotation) * 0.5 * weight;
}

fn qdef_position(input: VertexIn) -> vec3<f32> {
    var real = vec4<f32>(0.0);
    var dual = vec4<f32>(0.0);
    var anchor = vec4<f32>(0.0);
    var have_anchor = false;
    qdef_accumulate(&real, &dual, &anchor, &have_anchor, input.bones.x, input.weights.x);
    qdef_accumulate(&real, &dual, &anchor, &have_anchor, input.bones.y, input.weights.y);
    qdef_accumulate(&real, &dual, &anchor, &have_anchor, input.bones.z, input.weights.z);
    qdef_accumulate(&real, &dual, &anchor, &have_anchor, input.bones.w, input.weights.w);
    let scale = length(real);
    if (scale < 1e-6) {
        return input.position;
    }
    real = real / scale;
    dual = dual / scale;
    let translation_q = quat_mul(dual, vec4<f32>(-real.xyz, real.w)) * 2.0;
    return rotate_vector(real, input.position) + translation_q.xyz;
}

fn world_position(input: VertexIn) -> vec3<f32> {
    if (input.skin_kind == 1u) {
        return sdef_position(input);
    }
    if (input.skin_kind == 2u) {
        return qdef_position(input);
    }
    return (bdef_matrix(input.bones, input.weights) * vec4<f32>(input.position, 1.0)).xyz;
}

fn world_normal(input: VertexIn) -> vec3<f32> {
    if (input.skin_kind == 1u) {
        let rotation = quat_slerp(
            bones[input.bones.x].rotation,
            bones[input.bones.y].rotation,
            1.0 - input.sdef_c.w,
        );
        let n = rotate_vector(rotation, input.normal);
        let length_n = length(n);
        if (length_n < 1e-6) {
            return vec3<f32>(0.0, 1.0, 0.0);
        }
        return n / length_n;
    }
    let m = bdef_matrix(input.bones, input.weights);
    let n = (m * vec4<f32>(input.normal, 0.0)).xyz;
    let length_n = length(n);
    if (length_n < 1e-6) {
        return vec3<f32>(0.0, 1.0, 0.0);
    }
    return n / length_n;
}

fn project_ground(world: vec3<f32>) -> vec3<f32> {
    let dir = camera.light_dir.xyz;
    if (abs(dir.y) < 1e-5) {
        return world;
    }
    return world + dir * (-world.y / dir.y);
}

fn write_common(input: VertexIn, world: vec3<f32>) -> VertexOut {
    var output: VertexOut;
    output.clip = camera.clip_from_world * vec4<f32>(world, 1.0);
    output.world_pos = world;
    output.normal = world_normal(input);
    output.uv = input.uv;
    output.shadow_clip = camera.light_clip * vec4<f32>(world, 1.0);
    return output;
}

@vertex
fn vs_model(input: VertexIn) -> VertexOut {
    return write_common(input, world_position(input));
}

struct ShadowOut {
    @builtin(position) clip: vec4<f32>,
}

@vertex
fn vs_shadow(input: VertexIn) -> ShadowOut {
    var output: ShadowOut;
    output.clip = camera.light_clip * vec4<f32>(world_position(input), 1.0);
    return output;
}

@vertex
fn vs_ground(input: VertexIn) -> VertexOut {
    return write_common(input, project_ground(world_position(input)));
}

@vertex
fn vs_edge(input: VertexIn) -> VertexOut {
    let world = world_position(input);
    var output = write_common(input, world);
    let clip_n = camera.clip_from_world * vec4<f32>(output.normal, 0.0);
    let len = length(clip_n.xy);
    let view_dir = normalize(camera.camera_pos.xyz - world);
    let facing = abs(dot(output.normal, view_dir));
    if (len > 1e-5) {
        let width = material.params.y
            * input.edge_scale
            * 0.12
            * max(saturate(1.0 - facing), 0.28);
        output.clip = output.clip + vec4<f32>((clip_n.xy / len) * width * output.clip.w, 0.0, 0.0);
    }
    return output;
}

struct FragmentIn {
    @builtin(front_facing) front: bool,
    @location(0) world_pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) shadow_clip: vec4<f32>,
}

fn self_shadow(shadow_clip: vec4<f32>) -> f32 {
    let ndc = shadow_clip.xyz / max(shadow_clip.w, 1e-5);
    let shadow_uv = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
    let inside = shadow_uv.x > 0.0
        && shadow_uv.y > 0.0
        && shadow_uv.x < 1.0
        && shadow_uv.y < 1.0
        && ndc.z >= 0.0
        && ndc.z <= 1.0;
    if (!inside) {
        return 1.0;
    }
    let texel = 1.0 / 2048.0;
    var lit = 0.0;
    for (var y = -1; y <= 1; y++) {
        for (var x = -1; x <= 1; x++) {
            let uv = shadow_uv + vec2<f32>(f32(x), f32(y)) * texel;
            lit += textureSampleCompareLevel(shadow_tex, shadow_samp, uv, ndc.z - 0.001);
        }
    }
    return lit / 9.0;
}

@fragment
fn fs_model(input: FragmentIn) -> @location(0) vec4<f32> {
    var n = normalize(input.normal);
    if (!input.front) {
        n = -n;
    }
    let light = normalize(-camera.light_dir.xyz);
    let ndotl = dot(n, light);
    let view_dir = normalize(camera.camera_pos.xyz - input.world_pos);
    let view_n = normalize((camera.view_from_world * vec4<f32>(n, 0.0)).xyz);
    let sphere_uv = vec2<f32>(view_n.x * 0.5 + 0.5, 0.5 - view_n.y * 0.5);

    let tex = textureSample(color_tex, color_samp, input.uv) * material.texture_factor;
    let sphere = textureSample(sphere_tex, sphere_samp, sphere_uv).rgb * material.sphere_factor.rgb;
    let toon = textureSample(toon_tex, toon_samp, vec2<f32>(0.5, 0.5 - ndotl * 0.5)).rgb;
    let toon_shadow = textureSample(toon_tex, toon_samp, vec2<f32>(0.5, 1.0)).rgb;

    let alpha = material.diffuse.a * tex.a;
    // A clear texel must not write depth and hide what is drawn after it.
    if (alpha < 1.0 / 255.0) {
        discard;
    }

    var color = saturate(material.diffuse.rgb * LIGHT_COLOR + material.ambient.rgb) * tex.rgb;
    if (material.params.x > 1.5) {
        color = color + sphere;
    } else if (material.params.x > 0.5) {
        color = color * sphere;
    }
    var specular = vec3<f32>(0.0);
    let power = material.specular.w;
    if (power > 0.0) {
        let half_dir = normalize(view_dir + light);
        specular = pow(max(dot(n, half_dir), 0.0), power) * material.specular.rgb * LIGHT_COLOR;
    }

    if (material.params.z > 0.5 && camera.light_dir.w > 0.5) {
        let comp = min(saturate(ndotl * TOON_SHADOW_SCALE), self_shadow(input.shadow_clip));
        color = mix(color * toon_shadow, color + specular, comp);
    } else {
        color = color * toon + specular;
    }
    return vec4<f32>(saturate(color), alpha);
}

@fragment
fn fs_edge(input: VertexOut) -> @location(0) vec4<f32> {
    return material.edge_color;
}

@fragment
fn fs_edge_crease(input: VertexOut) -> @location(0) vec4<f32> {
    let n = normalize(input.normal);
    let view_dir = normalize(camera.camera_pos.xyz - input.world_pos);
    if (dot(n, view_dir) < 0.55) {
        discard;
    }
    return material.edge_color;
}

@fragment
fn fs_ground(input: VertexOut) -> @location(0) vec4<f32> {
    return vec4<f32>(0.05, 0.05, 0.08, 0.45);
}
