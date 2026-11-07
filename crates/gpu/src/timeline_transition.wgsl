// Full-screen Flare transition. Images are linear-light premultiplied RGBA.
struct U {
    image: vec4<f32>,     // width, height, progress, amount fraction
    direction: vec4<f32>, // angle_dir(angle + 90 degrees), x/y
    color: vec4<f32>,     // opaque linear RGB
};

@group(0) @binding(0) var a: texture_2d<f32>;
@group(0) @binding(1) var b: texture_2d<f32>;
@group(0) @binding(2) var<uniform> u: U;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let p = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    return vec4(p[i], 0.0, 1.0);
}

fn smoothstep_cpu(e0: f32, e1: f32, x: f32) -> f32 {
    let t = clamp((x - e0) / (e1 - e0), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

fn blob(x: f32, y: f32, cx: f32, cy: f32, radius: f32) -> f32 {
    let dx = x - cx;
    let dy = y - cy;
    let d2 = (dx * dx + dy * dy) / max(radius * radius, 1e-6);
    return exp(-d2);
}

@fragment
fn fs_flare(@builtin(position) at: vec4<f32>) -> @location(0) vec4<f32> {
    let p = u.image.z;
    let amount = u.image.w;
    let dx = u.direction.x;
    let dy = u.direction.y;
    let x = at.x;
    let y = at.y;
    let coord = vec2<i32>(at.xy);
    let k = smoothstep_cpu(0.3, 0.7, p);
    let env_t = 4.0 * p * (1.0 - p);
    let env = env_t * env_t * (3.0 - 2.0 * env_t);
    let ca = textureLoad(a, coord, 0);
    let cb = textureLoad(b, coord, 0);
    var c = ca;
    if k >= 1.0 {
        c = cb;
    } else if k > 0.0 {
        c = ca + (cb - ca) * k;
    }

    let w = u.image.x;
    let h = u.image.y;
    let diag = sqrt(w * w + h * h);
    let fx = w / 2.0 + dx * diag * (p - 0.5) * 0.9;
    let fy = h / 2.0 + dy * diag * (p - 0.5) * 0.9;
    let unit = max(h, 1.0);
    let rx = x - fx;
    let ry = y - fy;
    let along = rx * dx + ry * dy;
    let across = -rx * dy + ry * dx;
    let core = blob(x, y, fx, fy, unit * 0.06) * 3.0
        + blob(x, y, fx, fy, unit * 0.35) * 0.6;
    let streak = exp(-(abs(across) / (unit * 0.01)))
        * exp(-(abs(along) / (diag * 0.35))) * 1.5;
    let gx = w - fx;
    let gy = h - fy;
    let ghost = blob(x, y, gx, gy, unit * 0.08) * 0.4
        + blob(x, y, (gx + fx) / 2.0, (gy + fy) / 2.0, unit * 0.05) * 0.3;
    let light = (core + streak + ghost) * amount * env;
    return vec4(
        c.rgb + u.color.rgb * light,
        min(c.a + max(light, 0.0), 1.0),
    );
}
