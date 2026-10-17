struct Params {
    image: vec4<f32>,
    style: vec4<f32>,
    color: vec4<f32>,
    sweep: vec4<f32>,
    shape: vec4<f32>,
}

@group(0) @binding(0) var a: texture_2d<f32>;
@group(0) @binding(1) var b: texture_2d<f32>;
@group(0) @binding(2) var<uniform> params: Params;

const PI: f32 = 3.141592653589793;
const TAU: f32 = 6.283185307179586;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let p = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    return vec4(p[i], 0.0, 1.0);
}

fn pixel(side: u32, at: vec2<i32>) -> vec4<f32> {
    if side == 0u { return textureLoad(a, at, 0); }
    return textureLoad(b, at, 0);
}

fn euclidean_mod(x: f32, n: f32) -> f32 {
    let r = x - floor(x / n) * n;
    return select(r, 0.0, r >= n);
}

fn field(at: vec2<f32>) -> f32 {
    let w = params.image.x;
    let h = params.image.y;
    let kind = u32(params.image.w);
    let cx = params.shape.y;
    let cy = params.shape.z;
    let aspect = params.shape.w;
    if kind == 0u || kind == 9u {
        return at.x * params.sweep.w + at.y * params.shape.x;
    }
    if kind == 1u {
        let angle = atan2(at.x - cx, -(at.y - cy)) - params.sweep.w;
        return euclidean_mod(angle, TAU) / TAU * (w + h) * 0.5;
    }
    if kind == 2u {
        let corner = u32(params.style.w);
        var origin = vec2(0.0, 0.0);
        var start = 0.0;
        if corner == 1u { origin = vec2(w, 0.0); start = PI * 0.5; }
        if corner == 2u { origin = vec2(w, h); start = PI; }
        if corner == 3u { origin = vec2(0.0, h); start = -PI * 0.5; }
        var angle = euclidean_mod(atan2(at.y - origin.y, at.x - origin.x) - start, TAU);
        if angle > PI * 1.5 { angle = 0.0; } else { angle = min(angle, PI * 0.5); }
        return angle / (PI * 0.5) * (w + h) * 0.5;
    }
    if kind == 3u {
        let p = at - vec2(cx, cy);
        return length(p);
    }
    if kind == 4u {
        let p = abs(at - vec2(cx, cy));
        return max(p.x, p.y * aspect);
    }
    if kind == 5u {
        let p = abs(at - vec2(cx, cy));
        return p.x + p.y * aspect;
    }
    if kind == 6u {
        let p = abs(at - vec2(cx, cy));
        let u = p.x;
        let v = p.y * aspect;
        return min(max(u / 0.3, v), max(u, v / 0.3));
    }
    if kind == 7u {
        let p = abs(at - vec2(cx, cy));
        return select(p.y * aspect, p.x, u32(params.style.w) == 1u);
    }
    if kind == 8u {
        let corner = u32(params.style.w);
        var origin = vec2(0.0, 0.0);
        if corner == 1u { origin = vec2(w, 0.0); }
        if corner == 2u { origin = vec2(w, h); }
        if corner == 3u { origin = vec2(0.0, h); }
        let p = abs(at - origin);
        return max(p.x, p.y * aspect);
    }
    return at.x;
}

fn smooth(edge0: f32, edge1: f32, x: f32) -> f32 {
    if edge1 <= edge0 { return select(1.0, 0.0, x < edge0); }
    let t = clamp((x - edge0) / (edge1 - edge0), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

fn legacy_step(edge: f32, x: f32) -> f32 {
    return clamp((x - edge) / 0.004 + 0.5, 0.0, 1.0);
}

@fragment
fn fs(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let progress = params.image.z;
    let at = vec2<f32>(position.xy);
    if progress <= 0.0 { return pixel(0u, vec2<i32>(at)); }
    if progress >= 1.0 { return pixel(1u, vec2<i32>(at)); }

    if u32(params.image.w) == 9u {
        let uv = at / params.image.xy;
        let direction = u32(params.style.w);
        var k = 1.0 - legacy_step(progress, uv.x);
        if direction == 0u { k = 1.0 - legacy_step(progress, uv.y); }
        if direction == 1u { k = legacy_step(1.0 - progress, uv.x); }
        if direction == 2u { k = legacy_step(1.0 - progress, uv.y); }
        let va = pixel(0u, vec2<i32>(at));
        let vb = pixel(1u, vec2<i32>(at));
        return va * (1.0 - k) + vb * k;
    }

    let lo = params.sweep.x;
    let hi = params.sweep.y;
    let margin = params.sweep.z + (hi - lo) * 0.01;
    let threshold = lo - margin + progress * (hi - lo + 2.0 * margin);
    let distance = field(at) - threshold;
    let feather = params.style.y;
    let aa = params.style.x;
    let border = params.style.z;
    let softness = (feather + aa) * 0.5;
    let kb = 1.0 - smooth(-softness, softness, distance);
    var ko = kb;
    if border > 0.0 { ko = 1.0 - smooth(border - softness, border + softness, distance); }
    ko = max(ko, kb);
    let va = pixel(0u, vec2<i32>(at));
    let vb = pixel(1u, vec2<i32>(at));
    return va * (1.0 - ko) + params.color * (ko - kb) + vb * kb;
}
