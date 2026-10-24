struct Params {
    size_radius_axis: vec4<u32>,
    op: vec4<u32>,
    f0: vec4<f32>,
    f1: vec4<f32>,
}
@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var input_a: texture_2d<f32>;
@group(0) @binding(2) var input_b: texture_2d<f32>;
@group(0) @binding(3) var output_image: texture_storage_2d<rgba32float, write>;

fn blur_smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let t = clamp((x - a) / (b - a), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

fn whip_load(src: texture_2d<f32>, xy: vec2<i32>) -> vec4<f32> {
    let size = vec2<i32>(textureDimensions(src));
    if any(xy < vec2(0)) || any(xy >= size) { return vec4(0.0); }
    return textureLoad(src, xy, 0);
}
fn whip_sample(src: texture_2d<f32>, xy: vec2<f32>) -> vec4<f32> {
    let f = xy - vec2(0.5);
    let lo = vec2<i32>(floor(f));
    let t = f - vec2<f32>(lo);
    let a = whip_load(src, lo);
    let b = whip_load(src, lo + vec2(1, 0));
    let c = whip_load(src, lo + vec2(0, 1));
    let d = whip_load(src, lo + vec2(1, 1));
    return mix(mix(a, b, t.x), mix(c, d, t.x), t.y);
}

// Opcode 111: legacy cubic-eased full-frame push, normal-over incoming side.
// Dispatch before cs_box; stage then applies horizontal Gaussian boxes only.
@compute @workgroup_size(8, 8)
fn cs_whip_prepare(@builtin(global_invocation_id) id: vec3<u32>) {
    let size_u = params.size_radius_axis.xy;
    if any(id.xy >= size_u) { return; }
    let p = params.f0.x;
    let ease = select(1.0 - pow(2.0 - 2.0 * p, 3.0) * 0.5, 4.0 * p * p * p, p < 0.5);
    let direction = params.op.y;
    var delta = vec2<f32>(f32(size_u.x), 0.0);
    if direction == 0u { delta = vec2(0.0, f32(size_u.y)); }
    if direction == 1u { delta = vec2(-f32(size_u.x), 0.0); }
    if direction == 2u { delta = vec2(0.0, -f32(size_u.y)); }
    let xy = vec2<f32>(id.xy) + vec2(0.5);
    let a = whip_sample(input_a, xy - delta * ease);
    let b = whip_sample(input_b, xy - delta * (ease - 1.0));
    textureStore(output_image, vec2<i32>(id.xy), b + a * (1.0 - b.a));
}

// Repeat-edge sampling matches transitions::blurred -> effects::gaussian(..., true).
@compute @workgroup_size(64)
fn cs_box(@builtin(global_invocation_id) id: vec3<u32>) {
    let vertical = params.size_radius_axis.w != 0u;
    let size = params.size_radius_axis.xy;
    let n = select(size.x, size.y, vertical);
    let lines = select(size.y, size.x, vertical);
    if id.x >= lines || n == 0u { return; }
    let r = i32(params.size_radius_axis.z);
    let line = i32(id.x);
    let inv = 1.0 / f32(2 * r + 1);
    var acc = vec4(0.0);
    for (var i = -r; i <= r; i++) {
        let j = clamp(i, 0, i32(n) - 1);
        acc += textureLoad(input_a, select(vec2<i32>(j, line), vec2<i32>(line, j), vertical), 0);
    }
    for (var x = 0; x < i32(n); x++) {
        let pos = select(vec2<i32>(x, line), vec2<i32>(line, x), vertical);
        textureStore(output_image, pos, acc * inv);
        let out_j = clamp(x - r, 0, i32(n) - 1);
        let in_j = clamp(x + r + 1, 0, i32(n) - 1);
        acc -= textureLoad(input_a, select(vec2<i32>(out_j, line), vec2<i32>(line, out_j), vertical), 0);
        acc += textureLoad(input_a, select(vec2<i32>(in_j, line), vec2<i32>(line, in_j), vertical), 0);
    }
}

// 108 uses smoothstep(.25,.75,p); 109 uses mid_mix(.3,.7,p).
@compute @workgroup_size(8, 8)
fn cs_mix(@builtin(global_invocation_id) id: vec3<u32>) {
    if any(id.xy >= params.size_radius_axis.xy) { return; }
    let p = params.f0.x;
    let edges = select(vec2<f32>(0.25, 0.75), vec2<f32>(0.3, 0.7), params.op.x == 109u);
    let k = blur_smoothstep(edges.x, edges.y, p);
    textureStore(output_image, vec2<i32>(id.xy), mix(textureLoad(input_a, vec2<i32>(id.xy), 0), textureLoad(input_b, vec2<i32>(id.xy), 0), k));
}

@vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let positions = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    return vec4(positions[i], 0.0, 1.0);
}
@fragment fn fs_dissolve(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let xy = vec2<i32>(position.xy);
    let k = blur_smoothstep(0.25, 0.75, params.f0.x);
    return mix(textureLoad(input_a, xy, 0), textureLoad(input_b, xy, 0), k);
}
@fragment fn fs_spherical(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    return textureLoad(input_a, vec2<i32>(position.xy), 0);
}
