struct PassParams {
    size_radius_axis: vec4<u32>, // width, height, box radius, axis
    op: vec4<u32>,              // kernel, source side, unused, unused
    f0: vec4<f32>,              // progress, z/length, amount, intensity
    f1: vec4<f32>,              // center x/y, width, height
    f2: vec4<f32>,              // unused
}
@group(0) @binding(0) var<uniform> params: PassParams;
@group(0) @binding(1) var input_a: texture_2d<f32>;
@group(0) @binding(2) var input_b: texture_2d<f32>;
@group(0) @binding(3) var output_image: texture_storage_2d<rgba32float, write>;
@group(0) @binding(4) var auxiliary: texture_2d<f32>;

fn light_smooth(e0: f32, e1: f32, x: f32) -> f32 {
    let t = clamp((x - e0) / (e1 - e0), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}
fn midmix(p: f32) -> f32 { return light_smooth(0.3, 0.7, p); }
fn bell(p: f32) -> f32 {
    let t = 4.0 * p * (1.0 - p);
    return t * t * (3.0 - 2.0 * t);
}
fn load_zero(src: texture_2d<f32>, p: vec2<i32>) -> vec4<f32> {
    let size = vec2<i32>(textureDimensions(src));
    if any(p < vec2(0)) || any(p >= size) { return vec4(0.0); }
    return textureLoad(src, p, 0);
}
fn sample_linear(src: texture_2d<f32>, xy: vec2<f32>) -> vec4<f32> {
    let f = xy - vec2(0.5);
    let lo = vec2<i32>(floor(f));
    let t = f - vec2<f32>(lo);
    let a = load_zero(src, lo);
    let b = load_zero(src, lo + vec2(1, 0));
    let c = load_zero(src, lo + vec2(0, 1));
    let d = load_zero(src, lo + vec2(1, 1));
    return mix(mix(a, b, t.x), mix(c, d, t.x), t.y);
}
fn sample_clamped(src: texture_2d<f32>, xy: vec2<f32>) -> vec4<f32> {
    let size = vec2<i32>(textureDimensions(src));
    let f = clamp(xy - vec2(0.5), vec2(0.0), vec2<f32>(size - vec2(1)));
    let lo = vec2<i32>(floor(f));
    let hi = min(lo + vec2(1), size - vec2(1));
    let t = f - vec2<f32>(lo);
    return mix(mix(textureLoad(src, lo, 0), textureLoad(src, vec2(hi.x, lo.y), 0), t.x), mix(textureLoad(src, vec2(lo.x, hi.y), 0), textureLoad(src, hi, 0), t.x), t.y);
}
fn down_pixel(src: texture_2d<f32>, xy: vec2<i32>) -> vec4<f32> {
    let size = vec2<i32>(textureDimensions(src));
    let base = xy * 2;
    let a = textureLoad(src, clamp(base, vec2(0), size - vec2(1)), 0);
    let b = textureLoad(src, clamp(base + vec2(1, 0), vec2(0), size - vec2(1)), 0);
    let c = textureLoad(src, clamp(base + vec2(0, 1), vec2(0), size - vec2(1)), 0);
    let d = textureLoad(src, clamp(base + vec2(1, 1), vec2(0), size - vec2(1)), 0);
    return (a + b + c + d) * 0.25;
}
fn sample_down(src: texture_2d<f32>, xy: vec2<f32>) -> vec4<f32> {
    let f = xy - vec2(0.5);
    let lo = vec2<i32>(floor(f));
    let t = f - vec2<f32>(lo);
    let half_size = max(vec2<i32>(textureDimensions(src)) / 2, vec2(1));
    let at = array<vec2<i32>, 4>(lo, lo + vec2(1, 0), lo + vec2(0, 1), lo + vec2(1, 1));
    var values: array<vec4<f32>, 4>;
    for (var i = 0u; i < 4u; i++) {
        if any(at[i] < vec2(0)) || any(at[i] >= half_size) { values[i] = vec4(0.0); }
        else { values[i] = down_pixel(src, at[i]); }
    }
    return mix(mix(values[0], values[1], t.x), mix(values[2], values[3], t.x), t.y);
}

@compute @workgroup_size(8, 8)
fn cs_mix(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = params.size_radius_axis.xy;
    if any(id.xy >= size) { return; }
    let p = params.f0.x;
    let k = midmix(p);
    textureStore(output_image, vec2<i32>(id.xy), mix(textureLoad(input_a, vec2<i32>(id.xy), 0), textureLoad(input_b, vec2<i32>(id.xy), 0), k));
}

@compute @workgroup_size(8, 8)
fn cs_ray_bright(@builtin(global_invocation_id) id: vec3<u32>) {
    if any(id.xy >= params.size_radius_axis.xy) { return; }
    let c = mix(textureLoad(input_a, vec2<i32>(id.xy), 0), textureLoad(input_b, vec2<i32>(id.xy), 0), midmix(params.f0.x));
    let luma = clamp(dot(c.rgb, vec3(0.2126, 0.7152, 0.0722)), 0.0, 1.0);
    let light = max(pow(luma, 1.0 / 2.2) - 0.55, 0.0) * 2.2;
    textureStore(output_image, vec2<i32>(id.xy), c * light);
}

// One invocation processes one complete row or column. Initial radius cost is bounded by
// gaussian_boxes' image-relative radius cap; sliding updates are O(1) per pixel.
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
        let xy = select(vec2<i32>(j, line), vec2<i32>(line, j), vertical);
        acc += textureLoad(input_a, xy, 0);
    }
    for (var x = 0; x < i32(n); x++) {
        let out_xy = select(vec2<i32>(x, line), vec2<i32>(line, x), vertical);
        textureStore(output_image, out_xy, acc * inv);
        let out_i = x - r;
        let in_i = x + r + 1;
        let out_j = clamp(out_i, 0, i32(n) - 1);
        let in_j = clamp(in_i, 0, i32(n) - 1);
        acc -= textureLoad(input_a, select(vec2<i32>(out_j, line), vec2<i32>(line, out_j), vertical), 0);
        acc += textureLoad(input_a, select(vec2<i32>(in_j, line), vec2<i32>(line, in_j), vertical), 0);
    }
}

@compute @workgroup_size(8, 8)
fn cs_ray_zoom(@builtin(global_invocation_id) id: vec3<u32>) {
    if any(id.xy >= params.size_radius_axis.xy) { return; }
    let xy = vec2<f32>(id.xy) + vec2(0.5);
    let center = params.f1.xy;
    let amount = params.f0.y;
    let dx = (xy.x - center.x) * amount * 0.5;
    let dy = (xy.y - center.y) * amount * 0.5;
    if abs(dx) + abs(dy) < 0.5 {
        textureStore(output_image, vec2<i32>(id.xy), sample_clamped(input_a, xy - vec2(dx, dy)));
        return;
    }
    var acc = vec4(0.0);
    for (var i = 0u; i < 24u; i++) {
        let t = f32(i) / 23.0 * 2.0 - 1.0;
        acc += sample_clamped(input_a, vec2(xy.x - dx + dx * t, xy.y - dy + dy * t));
    }
    textureStore(output_image, vec2<i32>(id.xy), acc / 24.0);
}

@compute @workgroup_size(8, 8)
fn cs_cross_zoom_legacy(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = params.size_radius_axis.xy;
    if any(id.xy >= size) { return; }
    let xy = vec2<f32>(id.xy) + vec2(0.5);
    let p = params.f0.x;
    let z = 1.0 + 3.0 * select((1.0 - p) * 2.0, p * 2.0, p < 0.5);
    let center = vec2<f32>(size) * 0.5;
    var sample_xy = center + (xy - center) / z;
    var color: vec4<f32>;
    if z >= 2.0 && all(size >= vec2(4u)) {
        sample_xy = (xy - center) / (2.0 * z) + center * 0.5;
        color = sample_down(input_a, sample_xy);
    } else {
        color = sample_linear(input_a, sample_xy);
    }
    textureStore(output_image, vec2<i32>(id.xy), color);
}

@vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let positions = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    return vec4(positions[i], 0.0, 1.0);
}
@fragment fn fs_light(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let xy = vec2<i32>(position.xy);
    let p = params.f0.x;
    let kind = params.op.x;
    if p <= 0.0 { return textureLoad(input_a, xy, 0); }
    if p >= 1.0 { return textureLoad(input_b, xy, 0); }
    if kind == 77u {
        let sharp = mix(textureLoad(input_a, xy, 0), textureLoad(input_b, xy, 0), midmix(p));
        let blurred = textureLoad(auxiliary, xy, 0);
        let gain = params.f0.w * bell(p) * 2.0;
        return vec4(sharp.rgb + blurred.rgb * gain, max(sharp.a, blurred.a * min(gain, 1.0)));
    }
    if kind == 78u {
        let sharp = mix(textureLoad(input_a, xy, 0), textureLoad(input_b, xy, 0), midmix(p));
        let rays = textureLoad(auxiliary, xy, 0);
        let gain = params.f0.z * bell(p) * 3.0;
        return vec4(sharp.rgb + rays.rgb * gain, max(sharp.a, rays.a * min(gain, 1.0)));
    }
    return textureLoad(auxiliary, xy, 0);
}
