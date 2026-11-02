// FilmCraft GPU immersive-video (VR) kernels: the equirectangular blur, copy, sharpen and denoise
// of `filmcraft_render::vfx::immersive`, run as passes of `filmcraft_render::gpufx_multipass`.
// The math mirrors `row_radius`, `blur_row_wrap`, `box_rows` (repeat edge) and the `vr_sharpen` /
// `vr_denoise` branches of `apply`; the CPU code is the spec. Kernel contract: see fx_multipass.wgsl
// (bindings 0 and 1 are the inputs, 2 the output, 3 the parameter block of the pass).
//
// Parameter block, u32 words (floats as bit patterns):
//   row blur:      [view_x, view_y, view_w, view_h, sigma]
//   column box:    [view_x, view_y, view_w, view_h, radius]
//   copy:          [view_x, view_y, view_w, view_h]
//   sharpen:       [view_x, view_y, view_w, view_h, amount, 0, 0]
//   denoise:       [view_x, view_y, view_w, view_h, 0, show, threshold]
// A view is addressed inside the full-size slots: (view_x, view_y) is its top-left texel.

@group(0) @binding(0) var input0: texture_2d<f32>;
@group(0) @binding(1) var input1: texture_2d<f32>;
@group(0) @binding(2) var output: texture_storage_2d<rgba32float, write>;
@group(0) @binding(3) var<uniform> params: array<vec4<u32>, 8>;

fn pu(i: u32) -> u32 {
    return params[i >> 2u][i & 3u];
}

fn pf(i: u32) -> f32 {
    return bitcast<f32>(pu(i));
}

// No PI constant in WGSL; this is pi rounded to f32.
const PI: f32 = 3.1415927;

// `row_radius`: the box radius of row y of a view h rows high and w wide. Rounding is floor(x + 0.5)
// as in Rust; `min(max(f, 1), cap)` keeps the Rust order (`clamp` would be wrong when cap < 1).
fn row_radius(sigma: f32, w: u32, h: u32, y: u32) -> u32 {
    let lat = (0.5 - (f32(y) + 0.5) / f32(h)) * PI;
    let s = min(sigma / max(cos(lat), 1e-3), f32(w) / 4.0);
    if s <= 0.3 {
        return 0u;
    }
    let f = floor((sqrt(12.0 * s * s / 3.0 + 1.0) - 1.0) / 2.0 + 0.5);
    let cap = f32((w - 1u) / 2u);
    return u32(min(max(f, 1.0), cap));
}

fn wrap_x(a: i32, n: i32) -> i32 {
    return ((a % n) + n) % n;
}

// One row per invocation (x = 0 only): a row blur pass of `blur_row_wrap`. The copy when the
// radius is 0 keeps the ping-pong chain exact.
@compute @workgroup_size(8, 8, 1)
fn vr_blur_row(@builtin(global_invocation_id) id: vec3<u32>) {
    let vw = pu(2u);
    let vh = pu(3u);
    let y = id.y;
    if id.x != 0u || y >= vh || vw == 0u {
        return;
    }
    let x0 = i32(pu(0u));
    let row_y = i32(pu(1u)) + i32(y);
    let ww = i32(vw);
    var r = 0u;
    let sigma = pf(4u);
    if sigma > 0.3 {
        r = row_radius(sigma, vw, vh, y);
    }
    if r == 0u {
        for (var x = 0; x < ww; x = x + 1) {
            let c = vec2<i32>(x0 + x, row_y);
            textureStore(output, c, textureLoad(input0, c, 0));
        }
        return;
    }
    let ri = i32(r);
    let inv = 1.0 / f32(2 * ri + 1);
    // one box pass with wrap-around; the plan runs three of them, each reading the last one's output
    var acc = vec4<f32>(0.0);
    for (var j = -ri; j <= ri; j = j + 1) {
        acc = acc + textureLoad(input0, vec2<i32>(x0 + wrap_x(j, ww), row_y), 0);
    }
    for (var x = 0; x < ww; x = x + 1) {
        textureStore(output, vec2<i32>(x0 + x, row_y), acc * inv);
        let v_in = textureLoad(input0, vec2<i32>(x0 + wrap_x(x + ri + 1, ww), row_y), 0);
        let v_out = textureLoad(input0, vec2<i32>(x0 + wrap_x(x - ri, ww), row_y), 0);
        acc = acc + (v_in - v_out);
    }
}

// One column per invocation: a vertical box pass with repeat edge (`box_rows`, repeat = true).
@compute @workgroup_size(8, 8, 1)
fn vr_box_col(@builtin(global_invocation_id) id: vec3<u32>) {
    let vw = pu(2u);
    let vh = pu(3u);
    let x = id.x;
    if x >= vw || id.y != 0u || vh == 0u {
        return;
    }
    let r = i32(pu(4u));
    let inv = 1.0 / f32(2 * r + 1);
    let col_x = i32(pu(0u)) + i32(x);
    let y0 = i32(pu(1u));
    let hh = i32(vh);
    for (var y = 0; y < hh; y = y + 1) {
        var acc = vec4<f32>(0.0);
        for (var j = -r; j <= r; j = j + 1) {
            let yy = clamp(y + j, 0, hh - 1);
            acc = acc + textureLoad(input0, vec2<i32>(col_x, y0 + yy), 0);
        }
        textureStore(output, vec2<i32>(col_x, y0 + y), acc * inv);
    }
}

// Copies the view (the result of a blur-only route).
@compute @workgroup_size(8, 8, 1)
fn vr_copy(@builtin(global_invocation_id) id: vec3<u32>) {
    let vw = pu(2u);
    let vh = pu(3u);
    if id.x >= vw || id.y >= vh {
        return;
    }
    let c = vec2<i32>(i32(pu(0u) + id.x), i32(pu(1u) + id.y));
    textureStore(output, c, textureLoad(input0, c, 0));
}

// Sharpen: input0 is the original, input1 the blurred view. Alpha is left as it is.
@compute @workgroup_size(8, 8, 1)
fn vr_sharpen_combine(@builtin(global_invocation_id) id: vec3<u32>) {
    let vw = pu(2u);
    let vh = pu(3u);
    if id.x >= vw || id.y >= vh {
        return;
    }
    let c = vec2<i32>(i32(pu(0u) + id.x), i32(pu(1u) + id.y));
    let p = textureLoad(input0, c, 0);
    let q = textureLoad(input1, c, 0);
    let amount = pf(4u);
    let rgb = max(p.rgb + (p.rgb - q.rgb) * amount, vec3<f32>(0.0));
    textureStore(output, c, vec4<f32>(rgb, p.a));
}

// Denoise: the blend weight falls off with the largest RGB difference to the blurred view.
@compute @workgroup_size(8, 8, 1)
fn vr_denoise_combine(@builtin(global_invocation_id) id: vec3<u32>) {
    let vw = pu(2u);
    let vh = pu(3u);
    if id.x >= vw || id.y >= vh {
        return;
    }
    let c = vec2<i32>(i32(pu(0u) + id.x), i32(pu(1u) + id.y));
    let p = textureLoad(input0, c, 0);
    let q = textureLoad(input1, c, 0);
    let d = max(max(abs(p.r - q.r), abs(p.g - q.g)), abs(p.b - q.b));
    // the CPU keeps threshold >= 0.02; the floor only stops a zero threshold from giving 0/0
    let thr = max(pf(6u), 1e-6);
    let k = exp(-(d * d) / (thr * thr));
    let o = p + (q - p) * k;
    if pu(5u) == 1u {
        let rgb = max((p.rgb - o.rgb) * 4.0 + vec3<f32>(0.18), vec3<f32>(0.0)) * p.a;
        textureStore(output, c, vec4<f32>(rgb, p.a));
    } else {
        textureStore(output, c, o);
    }
}
