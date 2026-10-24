struct Params {
    progress: f32,
    kernel: u32,
    direction: u32,
    pad: u32,
    blur: f32,
    amount: f32,
    dx: f32,
    dy: f32,
    color: vec4<f32>,
    extra0: vec4<f32>,
    extra1: vec4<f32>,
    extra2: vec4<f32>,
    extra3: vec4<f32>,
    extra4: vec4<f32>,
}
@group(0) @binding(0) var outgoing: texture_2d<f32>;
@group(0) @binding(1) var incoming: texture_2d<f32>;
@group(0) @binding(2) var<uniform> params: Params;

fn pixel(side: u32, at: vec2<u32>) -> vec4<f32> {
    if side == 0u { return textureLoad(outgoing, vec2<i32>(at), 0); }
    return textureLoad(incoming, vec2<i32>(at), 0);
}
fn sample_clamped(side: u32, at: vec2<f32>, extent: vec2<u32>) -> vec4<f32> {
    let f = clamp(at - vec2(0.5), vec2(0.0), vec2<f32>(extent) - vec2(1.0));
    let lo = vec2<u32>(floor(f));
    let hi = min(lo + vec2(1u), extent - vec2(1u));
    let k = f - vec2<f32>(lo);
    let a = pixel(side, lo);
    let b = pixel(side, vec2(hi.x, lo.y));
    let c = pixel(side, vec2(lo.x, hi.y));
    let d = pixel(side, hi);
    return mix(mix(a, b, k.x), mix(c, d, k.x), k.y);
}
fn push_card(side: u32, offset: vec2<f32>, xy: vec2<f32>, extent: vec2<f32>) -> vec4<f32> {
    let eye = vec3(extent / 2.0, -max(extent.x, extent.y) * 6.0);
    let ex = vec3(1.0, 0.0, 0.0);
    let ey = vec3(0.0, 1.0, 0.0);
    let d = vec3(xy, 0.0) - eye;
    let nd = -d;
    let rhs = eye - vec3(offset, 0.0);
    let det = dot(ex, cross(ey, nd));
    let local = vec2(dot(rhs, cross(ey, nd)), dot(ex, cross(rhs, nd))) / det;
    let edge = min(min(local.x, extent.x - local.x), min(local.y, extent.y - local.y));
    let coverage = clamp(edge + 0.5, 0.0, 1.0);
    if coverage <= 0.0 { return vec4(0.0); }
    return sample_clamped(side, local, vec2<u32>(extent)) * coverage;
}
fn eased(q: f32) -> f32 {
    if q < 0.5 { return 4.0 * q * q * q; }
    let v = -2.0 * q + 2.0;
    return 1.0 - v * v * v / 2.0;
}
fn push(at: vec4<f32>) -> vec4<f32> {
    let extent = vec2<f32>(textureDimensions(outgoing));
    let p = params.progress;
    if p <= 0.0 { return pixel(0u, vec2<u32>(at.xy)); }
    if p >= 1.0 { return pixel(1u, vec2<u32>(at.xy)); }
    var direction = vec2(0.0, 1.0);
    switch params.direction {
        case 1u: { direction = vec2(-1.0, 0.0); }
        case 2u: { direction = vec2(0.0, -1.0); }
        case 3u: { direction = vec2(1.0, 0.0); }
        default: {}
    }
    let travel = max(dot(abs(direction), extent), 1.0);
    var count = 1u;
    if params.blur > 0.0 { count = clamp(u32(ceil(params.blur * 12.0)), 2u, 12u); }
    var sum = vec4(0.0);
    for (var s = 0u; s < count; s++) {
        var q = p;
        if count > 1u { q = clamp(p + (f32(s) / f32(count - 1u) - 0.5) * (0.12 * params.blur), 0.0, 1.0); }
        let e = eased(q);
        let a = push_card(0u, direction * travel * e, at.xy, extent);
        let b = push_card(1u, direction * travel * (e - 1.0), at.xy, extent);
        sum += b + a * (1.0 - b.a);
    }
    return sum / f32(count);
}

fn legacy_center_split(xy: vec2<f32>, extent: vec2<f32>) -> vec4<f32> {
    let ei = vec2<i32>(extent);
    let p = params.progress;
    var out = textureLoad(incoming, vec2<i32>(xy), 0);
    for (var q = 0u; q < 4u; q++) {
        let side = select(-1.0, 1.0, (q & 1u) != 0u);
        let vert = select(-1.0, 1.0, (q & 2u) != 0u);
        let shift = vec2(side * extent.x * 0.5 * p, vert * extent.y * 0.5 * p);
        let src = xy - shift;
        let inside = select((src.x < (extent.x * 0.5)), (src.x >= (extent.x * 0.5)), (q & 1u) != 0u)
            && select((src.y < (extent.y * 0.5)), (src.y >= (extent.y * 0.5)), (q & 2u) != 0u);
        if inside {
            out = motion_over(motion_sample_raw(0u, src, ei), out);
        }
    }
    return out;
}

fn legacy_band_slide(xy: vec2<f32>, extent: vec2<f32>) -> vec4<f32> {
    let ei = vec2<i32>(extent);
    let shift = extent.x * (1.0 - params.progress);
    var out = textureLoad(outgoing, vec2<i32>(xy), 0);
    for (var band = 0; band < 6; band++) {
        let sign = select(-1.0, 1.0, (band & 1) == 0);
        let src = xy - vec2(sign * shift, 0.0);
        if src.x >= 0.0 && src.x < extent.x && src.y >= 0.0 && src.y < extent.y
            && i32(floor(src.y / extent.y * 6.0)) == band {
            out = motion_over(motion_sample_raw(1u, src, ei), out);
        }
    }
    return out;
}

fn legacy_text_cells(xy: vec2<f32>, extent: vec2<f32>) -> vec4<f32> {
    let cols = params.blur;
    let rows = params.amount;
    let cell = extent / vec2(cols, rows);
    let ci = min(floor(xy.x / cell.x), cols - 1.0);
    let ri = min(floor(xy.y / cell.y), rows - 1.0);
    let index = ri * cols + ci;
    if params.kernel == 103u {
        let current = floor(params.progress * cols * rows);
        if index < current { return textureLoad(incoming, vec2<i32>(xy), 0); }
        if index == current && params.direction != 0u {
            let local_y = (xy.y - ri * cell.y) / cell.y;
            if local_y > 0.15 {
                let bg = textureLoad(outgoing, vec2<i32>(xy), 0);
                return vec4(0.75, 0.75, 0.75, 0.75) + bg * 0.25;
            }
        }
        return textureLoad(outgoing, vec2<i32>(xy), 0);
    }
    let delay = 0.6 * (0.7 * ci / cols + 0.3 * ri / rows);
    let q = clamp((params.progress - delay) / 0.4, 0.0, 1.0);
    let ease_in = q * q * q;
    let inv_q = 1.0 - q;
    let ease_out = 1.0 - inv_q * inv_q * inv_q;
    let top = ri * cell.y;
    let bottom = (ri + 1.0) * cell.y;
    let ya = xy.y + cell.y * ease_in;
    let yb = xy.y - cell.y * (1.0 - ease_out);
    var a = vec4(0.0);
    var b = vec4(0.0);
    if ya < bottom { a = sample_clamped(0u, vec2(xy.x, ya), vec2<u32>(extent)); }
    if yb >= top { b = sample_clamped(1u, vec2(xy.x, yb), vec2<u32>(extent)); }
    return mix(a, b, smoothstep(0.0, 1.0, q));
}

fn vr_random_blocks(position: vec4<f32>) -> vec4<f32> {
    let extent = vec2<f32>(textureDimensions(outgoing));
    let block = max(params.blur, 2.0);
    let bx = floor(position.x / block);
    let by = floor(position.y / block);
    let columns = max(ceil(extent.x / block), 1.0);
    let seed = params.direction;
    let softness = clamp(params.amount, 0.01, 0.45);
    let threshold = softness + (1.0 - 2.0 * softness) * dissolve_hash(i32(bx) % i32(columns), i32(by), seed);
    let at = vec2<i32>(position.xy);
    return mix(textureLoad(outgoing, at, 0), textureLoad(incoming, at, 0), smoothstep(threshold - softness, threshold + softness, params.progress));
}

fn vr_sample_wrap(side: u32, at: vec2<f32>, extent: vec2<u32>) -> vec4<f32> {
    let width = i32(extent.x);
    let height = i32(extent.y);
    let xw = rem_euclid(at.x - 0.5, f32(width));
    let x0 = i32(floor(xw));
    let x1 = (x0 + 1) % width;
    let tx = xw - f32(x0);
    let fy = clamp(at.y - 0.5, 0.0, f32(height - 1));
    let y0 = i32(floor(fy));
    let y1 = min(y0 + 1, height - 1);
    let ty = fy - f32(y0);
    let a = pixel(side, vec2<u32>(u32(x0), u32(y0)));
    let b = pixel(side, vec2<u32>(u32(x1), u32(y0)));
    let c = pixel(side, vec2<u32>(u32(x0), u32(y1)));
    let d = pixel(side, vec2<u32>(u32(x1), u32(y1)));
    return mix(mix(a, b, tx), mix(c, d, tx), ty);
}

fn vr_leaks(position: vec4<f32>) -> vec4<f32> {
    let extent = vec2<f32>(textureDimensions(outgoing));
    let p = params.progress;
    let env = 4.0 * p * (1.0 - p);
    let bell = env * env * (3.0 - 2.0 * env);
    let k = smoothstep(0.3, 0.7, p);
    var c = mix(textureLoad(outgoing, vec2<i32>(position.xy), 0), textureLoad(incoming, vec2<i32>(position.xy), 0), k);
    for (var i = 0u; i < 4u; i++) {
        let ii = i32(i);
        let cx = extent.x * (dissolve_hash(ii, 1, params.direction) + (p - 0.5) * (0.5 + dissolve_hash(ii, 2, params.direction)));
        let cy = extent.y * (0.2 + 0.6 * dissolve_hash(ii, 3, params.direction));
        let radius = extent.y * (0.3 + 0.3 * dissolve_hash(ii, 4, params.direction));
        let dx = rem_euclid(position.x - cx + extent.x * 0.5, extent.x) - extent.x * 0.5;
        let d2 = (dx * dx + (position.y - cy) * (position.y - cy)) / (radius * radius);
        var color = vec3(1.0, 0.6, 0.25);
        if params.pad == 1u {
            switch i {
                case 0u: { color = vec3(1.0, 0.1, 0.55); }
                case 1u: { color = vec3(0.1, 0.75, 1.0); }
                case 2u: { color = vec3(1.0, 0.85, 0.1); }
                default: { color = vec3(0.5, 0.2, 1.0); }
            }
        } else {
            switch i {
                case 0u: { color = vec3(1.0, 0.6, 0.25); }
                case 1u: { color = vec3(1.0, 0.45, 0.15); }
                case 2u: { color = vec3(1.0, 0.85, 0.6); }
                default: { color = vec3(1.0, 0.7, 0.4); }
            }
        }
        let light = exp(-d2) * params.extra0.x * bell * 0.7;
        c = vec4(c.rgb + color * light, min(c.a + max(light, 0.0), 1.0));
    }
    return c;
}

fn vr_light_rays(position: vec4<f32>) -> vec4<f32> {
    let extent = vec2<f32>(textureDimensions(outgoing));
    let p = params.progress;
    let env0 = 4.0 * p * (1.0 - p);
    let env = env0 * env0 * (3.0 - 2.0 * env0);
    let k = smoothstep(0.3, 0.7, p);
    let xy = position.xy;
    let flags = params.direction;
    let cx = select(extent.x * 0.5, params.extra0.x, (flags & 1u) != 0u);
    let cy = select(extent.y * 0.5, params.extra0.y, (flags & 2u) != 0u);
    let rx = rem_euclid(xy.x - cx + extent.x * 0.5, extent.x) - extent.x * 0.5;
    let ry = xy.y - cy;
    let a = atan2(ry, rx);
    let r = length(vec2(rx, ry)) / extent.y;
    let rays = pow(0.5 + 0.5 * sin(a * 23.0 + p * 3.0) * cos(a * 7.0 - p * 2.0), 6.0);
    let fall = exp(-r * 1.5);
    let light = (rays * fall + exp(-r * 6.0) * 0.8) * params.extra0.z * env * 2.0;
    let c = mix(textureLoad(outgoing, vec2<i32>(xy), 0), textureLoad(incoming, vec2<i32>(xy), 0), k);
    return vec4(c.rgb + vec3(1.0, 0.97, 0.9) * light, min(c.a + max(light, 0.0), 1.0));
}

fn vr_mobius_sample(side: u32, xy: vec2<f32>, zoom: f32, twist: f32, extent: vec2<f32>, rmax: f32) -> vec4<f32> {
    let d = xy - extent * 0.5;
    let r = length(d);
    let a = atan2(d.y, d.x) + twist * max(1.0 - r / rmax, 0.0);
    let r2 = r / zoom;
    return vr_sample_wrap(side, extent * 0.5 + r2 * vec2(cos(a), sin(a)), vec2<u32>(extent));
}

fn vr_mobius_zoom(position: vec4<f32>) -> vec4<f32> {
    let extent_u = textureDimensions(outgoing);
    let extent = vec2<f32>(extent_u);
    let p = params.progress;
    let k = smoothstep(0.3, 0.7, p);
    let zoom = params.extra0.x;
    let twist = params.extra0.y;
    let za = 1.0 + zoom * 3.0 * pow(min(p / 0.5, 1.0), 3.0);
    let q = clamp((p - 0.5) / 0.5, 0.0, 1.0);
    let zb = 1.0 + zoom * 3.0 * (1.0 - (1.0 - pow(1.0 - q, 3.0)));
    let rmax = length(extent) * 0.5;
    var a = vec4(0.0);
    var b = vec4(0.0);
    if k < 1.0 { a = vr_mobius_sample(0u, position.xy, za, twist * (4.0 * p * (1.0 - p)) * (4.0 * p * (1.0 - p)) * (3.0 - 2.0 * (4.0 * p * (1.0 - p))), extent, rmax); }
    if k > 0.0 { b = vr_mobius_sample(1u, position.xy, zb, -twist * (4.0 * p * (1.0 - p)) * (4.0 * p * (1.0 - p)) * (3.0 - 2.0 * (4.0 * p * (1.0 - p))), extent, rmax); }
    return mix(a, b, k);
}

fn transition_frame(position: vec4<f32>, extent: vec2<f32>) -> vec4<f32> {
    var dx = 0.0;
    var dy = 1.0;
    switch params.direction {
        case 1u: { dx = -1.0; dy = 0.0; }
        case 2u: { dx = 0.0; dy = -1.0; }
        case 3u: { dx = 1.0; dy = 0.0; }
        default: {}
    }
    let travel = max(abs(dx) * extent.x + abs(dy) * extent.y, 1.0);
    let p = params.progress;
    let shrink = smoothstep(0.0, 0.3, p) * (1.0 - smoothstep(0.7, 1.0, p));
    let s = 1.0 - (1.0 - params.amount) * shrink;
    let q = clamp((p - 0.25) / 0.5, 0.0, 1.0);
    var move_t = 4.0 * q * q * q;
    if q >= 0.5 { move_t = 1.0 - pow(-2.0 * q + 2.0, 3.0) * 0.5; }
    let border = params.blur * shrink;
    let step = travel * 0.5 * (1.0 + s) + border * 2.0 + 2.0;
    var out = vec4(0.0);
    for (var side = 0u; side < 2u; side++) {
        var off = move_t * step;
        if side == 1u { off = (move_t - 1.0) * step; }
        let ux = position.x - dx * off;
        let uy = position.y - dy * off;
        let lx = (ux - extent.x * 0.5) / s + extent.x * 0.5;
        let ly = (uy - extent.y * 0.5) / s + extent.y * 0.5;
        let bpx = border / s;
        let inside = lx >= 0.0 && lx < extent.x && ly >= 0.0 && ly < extent.y;
        let in_frame = lx >= -bpx && lx < extent.x + bpx && ly >= -bpx && ly < extent.y + bpx;
        var c = vec4(0.0);
        if inside { c = sample_clamped(side, vec2(lx, ly), vec2<u32>(extent)); }
        else if in_frame && bpx > 0.0 { c = params.color; }
        else { continue; }
        out = c + out * (1.0 - c.a);
    }
    return out;
}
fn smoothstep_cpu(e0: f32, e1: f32, x: f32) -> f32 {
    let t = clamp((x - e0) / (e1 - e0), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}
fn blob(x: f32, y: f32, cx: f32, cy: f32, radius: f32) -> f32 {
    let dx = x - cx;
    let dy = y - cy;
    return exp(-(dx * dx + dy * dy) / max(radius * radius, 1e-6));
}
fn legacy_step(edge: f32, x: f32) -> f32 {
    return clamp((x - edge) / 0.004 + 0.5, 0.0, 1.0);
}
fn wipe_pixel(a: vec4<f32>, b: vec4<f32>, d: f32, feather: f32, border: f32, aa: f32) -> vec4<f32> {
    let s = (feather + aa) * 0.5;
    let kb = 1.0 - smoothstep(-s, s, d);
    var ko = kb;
    if border > 0.0 { ko = max(1.0 - smoothstep(border - s, border + s, d), kb); }
    return a * (1.0 - ko) + params.color * (ko - kb) + b * kb;
}
fn linear_wipe(at: vec4<f32>, use_border: bool) -> vec4<f32> {
    let size = vec2<f32>(textureDimensions(outgoing));
    let corners = array<vec2<f32>, 4>(vec2(0.0), vec2(size.x, 0.0), vec2(0.0, size.y), size);
    var lo = dot(corners[0], vec2(params.dx, params.dy));
    var hi = lo;
    for (var i = 1u; i < 4u; i++) {
        let v = dot(corners[i], vec2(params.dx, params.dy));
        lo = min(lo, v);
        hi = max(hi, v);
    }
    let aa = bitcast<f32>(params.direction);
    let feather = max(params.blur, 0.0);
    var border = 0.0;
    if use_border { border = max(params.amount, 0.0); }
    let margin = border + feather + 2.0 * aa + 1.0 + (hi - lo) * 0.01;
    let threshold = lo - margin + params.progress * (hi - lo + 2.0 * margin);
    let d = dot(at.xy, vec2(params.dx, params.dy)) - threshold;
    return wipe_pixel(pixel(0u, vec2<u32>(at.xy)), pixel(1u, vec2<u32>(at.xy)), d, feather, border, aa);
}
fn legacy_wipe(at: vec4<f32>) -> vec4<f32> {
    let size = vec2<f32>(textureDimensions(outgoing));
    let uv = at.xy / size;
    let p = params.progress;
    var mix_b = 1.0 - legacy_step(p, uv.x);
    switch params.direction {
        case 0u: { mix_b = 1.0 - legacy_step(p, uv.y); }
        case 1u: { mix_b = legacy_step(1.0 - p, uv.x); }
        case 2u: { mix_b = legacy_step(1.0 - p, uv.y); }
        default: {}
    }
    return mix(pixel(0u, vec2<u32>(at.xy)), pixel(1u, vec2<u32>(at.xy)), mix_b);
}
fn rem_euclid(x: f32, n: f32) -> f32 {
    return x - floor(x / n) * n;
}
fn star_distance(x: f32, y: f32, points: u32, inner: f32) -> f32 {
    let r = sqrt(x * x + y * y);
    if r < 1e-6 { return 0.0; }
    let theta = atan2(-y, x) - 1.5707963267948966;
    let seg = 6.283185307179586 / f32(max(points, 2u));
    let a = abs(rem_euclid(theta, seg) - seg / 2.0);
    let edge_angle = seg / 2.0 - a;
    let ri = clamp(inner, 0.05, 1.0);
    let p1x = ri * cos(seg / 2.0);
    let p1y = ri * sin(seg / 2.0);
    let ex = p1x - 1.0;
    let ey = p1y;
    let dx = cos(edge_angle);
    let dy = sin(edge_angle);
    let denom = dx * ey - dy * ex;
    var tr = 1.0;
    if abs(denom) >= 1e-9 { tr = ey / denom; }
    return r / max(tr, 0.05);
}
fn field_wipe(at: vec4<f32>) -> vec4<f32> {
    let size = vec2<f32>(textureDimensions(outgoing));
    let xy = vec2<u32>(at.xy);
    let lo = params.extra0.x;
    let hi = params.extra0.y;
    let cx = params.extra0.z;
    let cy = params.extra0.w;
    var field = 0.0;
    if params.kernel == 20u {
        let angle = atan2(-(at.y - cy), at.x - cx) - params.extra1.x * 0.017453292519943295;
        field = rem_euclid(angle, 6.283185307179586) / 6.283185307179586 * ((size.x + size.y) * 0.5);
    } else if params.kernel == 21u {
        var ox = 0.0;
        var oy = 0.0;
        var a0 = 0.0;
        switch params.direction {
            case 1u: { ox = size.x; a0 = 1.5707963267948966; }
            case 2u: { ox = size.x; oy = size.y; a0 = 3.141592653589793; }
            case 3u: { oy = size.y; a0 = -1.5707963267948966; }
            default: {}
        }
        var angle = rem_euclid(atan2(at.y - oy, at.x - ox) - a0, 6.283185307179586);
        if angle > 4.71238898038469 { angle = 0.0; } else { angle = min(angle, 1.5707963267948966); }
        field = angle / 1.5707963267948966 * ((size.x + size.y) * 0.5);
    } else if params.kernel == 22u {
        let theta = params.extra2.x * 0.017453292519943295;
        let s = sin(theta);
        let c = cos(theta);
        let x = at.x - cx;
        let y = at.y - cy;
        field = star_distance(x * c + y * s, -x * s + y * c, u32(params.extra2.y), params.extra1.w);
    } else if params.kernel == 23u {
        var dx = 1.0;
        var dy = 0.0;
        switch params.direction {
            case 0u: { dx = 0.0; dy = 1.0; }
            case 1u: { dx = -1.0; dy = 0.0; }
            case 2u: { dx = 0.0; dy = -1.0; }
            default: {}
        }
        let panels = params.extra2.y;
        let travel = max(abs(dx) * size.x + abs(dy) * size.y, 1.0);
        var across_len = size.x;
        var across = at.x;
        if dx != 0.0 { across_len = size.y; across = at.y; }
        let along = at.x * dx + at.y * dy;
        let idx = clamp(floor(across / across_len * panels), 0.0, panels - 1.0);
        field = along + idx * travel * 0.6 / panels;
    } else if params.kernel == 24u {
        let dx = params.dx;
        let dy = params.dy;
        let span = max(params.extra1.z - params.extra1.y, 1.0);
        let step = max(size.x, size.y) * 0.35 / max(params.extra2.y, 1.0);
        let along = at.x * dx + at.y * dy;
        let across = ((-at.x * dy + at.y * dx) - params.extra1.y) / span;
        let idx = clamp(floor(across * params.extra2.y), 0.0, params.extra2.y - 1.0);
        field = along + abs(idx - (params.extra2.y - 1.0) / 2.0) * step;
    } else if params.kernel == 25u {
        let asp = size.x / max(size.y, 1.0);
        if params.direction == 1u { field = abs(at.x - cx); } else { field = abs(at.y - cy) * asp; }
    } else if params.kernel == 26u {
        var ox = 0.0;
        var oy = 0.0;
        switch params.direction {
            case 1u: { ox = size.x; }
            case 2u: { ox = size.x; oy = size.y; }
            case 3u: { oy = size.y; }
            default: {}
        }
        field = max(abs(at.x - ox), abs(at.y - oy) * size.x / max(size.y, 1.0));
    } else if params.kernel == 98u {
        let lon = (at.x / size.x) * 6.283185307179586 - 3.141592653589793;
        let lat = 1.5707963267948966 - (at.y / size.y) * 3.141592653589793;
        let lon0 = (cx / size.x) * 6.283185307179586 - 3.141592653589793;
        let lat0 = 1.5707963267948966 - (cy / size.y) * 3.141592653589793;
        let c = sin(lat0) * sin(lat) + cos(lat0) * cos(lat) * cos(lon - lon0);
        field = acos(clamp(c, -1.0, 1.0)) * (size.x / 6.283185307179586);
    } else {
        let asp = size.x / max(size.y, 1.0);
        let x = abs(at.x - cx);
        let y = abs(at.y - cy) * asp;
        if params.kernel == 27u { field = length(vec2(at.x - cx, at.y - cy)); }
        else if params.kernel == 28u { field = max(x, y); }
        else if params.kernel == 29u { field = x + y; }
        else {
            let k = 0.3;
            field = max(x / k, y);
            field = min(field, max(x, y / k));
        }
    }
    let margin = params.amount + params.blur + 2.0 * bitcast<f32>(params.pad) + 1.0 + (hi - lo) * 0.01;
    let threshold = lo - margin + params.progress * (hi - lo + 2.0 * margin);
    let d = field - threshold;
    return wipe_pixel(pixel(0u, xy), pixel(1u, xy), d, params.blur, params.amount, bitcast<f32>(params.pad));
}
fn stretch_wipe(at: vec4<f32>) -> vec4<f32> {
    let size = vec2<f32>(textureDimensions(outgoing));
    let xy = vec2<u32>(at.xy);
    let direction = vec2(params.dx, params.dy);
    let stretch = params.blur;
    let field = dot(at.xy, direction);
    let threshold = params.extra0.x - stretch + params.progress * (params.extra0.y - params.extra0.x + 2.0 * stretch);
    if field < threshold { return pixel(1u, xy); }
    if field < threshold + stretch {
        let back = field - threshold;
        let smear = sample_clamped(1u, at.xy - direction * back, vec2<u32>(size));
        return mix(smear, pixel(0u, xy), smoothstep_cpu(0.0, 1.0, back / stretch));
    }
    return pixel(0u, xy);
}
fn neon_wipe(at: vec4<f32>) -> vec4<f32> {
    let xy = vec2<u32>(at.xy);
    let p = params.progress;
    let glow = params.blur;
    let d = dot(at.xy, vec2(params.dx, params.dy)) -
        (params.extra0.x - (glow * 0.5 + 2.0) + p * (params.extra0.y - params.extra0.x + 2.0 * (glow * 0.5 + 2.0)));
    let base = wipe_pixel(pixel(0u, xy), pixel(1u, xy), d, 0.0, 0.0, bitcast<f32>(params.pad));
    let fade = min(min(p * 10.0, (1.0 - p) * 10.0), 1.0);
    let core = exp(-pow(d / (glow * 0.08), 2.0));
    let halo = exp(-abs(d) / (glow * 0.5));
    let k = (core * 1.5 + halo * 0.8) * params.amount * fade;
    return vec4(base.rgb + params.color.rgb * k, min(base.a + max(k, 0.0), 1.0));
}
fn vr_gradient_wipe(at: vec4<f32>) -> vec4<f32> {
    let size = vec2<f32>(textureDimensions(outgoing));
    let lon = (at.x / size.x) * 6.283185307179586;
    let g0 = clamp(at.y / size.y + 0.04 * sin(lon * 2.0), 0.0, 1.0);
    let g = select(g0, 1.0 - g0, params.direction != 0u);
    let softness = params.blur;
    let threshold = -softness + params.progress * (1.0 + 2.0 * softness);
    let mix_b = smoothstep_cpu(-softness, softness, threshold - g);
    return mix(pixel(0u, vec2<u32>(at.xy)), pixel(1u, vec2<u32>(at.xy)), mix_b);
}
fn dissolve_hash(x: i32, y: i32, seed: u32) -> f32 {
    var h = bitcast<u32>(x) * 0x8DA6B343u ^ bitcast<u32>(y) * 0xD8163841u ^ seed * 0xCB1AB31Fu ^ 0x9E3779B9u;
    h ^= h >> 15u;
    h *= 0x2C1B3C6Du;
    h ^= h >> 12u;
    h *= 0x297A2D39u;
    h ^= h >> 15u;
    return f32(h >> 8u) / 16777216.0;
}
fn dissolve_vnoise(x: f32, y: f32, seed: u32) -> f32 {
    let xi = floor(x);
    let yi = floor(y);
    let fx = x - xi;
    let fy = y - yi;
    let ux = fx * fx * (3.0 - 2.0 * fx);
    let uy = fy * fy * (3.0 - 2.0 * fy);
    let i = i32(xi);
    let j = i32(yi);
    let a = dissolve_hash(i, j, seed);
    let b = dissolve_hash(i + 1, j, seed);
    let c = dissolve_hash(i, j + 1, seed);
    let d = dissolve_hash(i + 1, j + 1, seed);
    let top = a + (b - a) * ux;
    let bot = c + (d - c) * ux;
    return top + (bot - top) * uy;
}
fn dissolve_fbm(x: f32, y: f32, seed: u32) -> f32 {
    var value = 0.0;
    var amplitude = 0.5;
    var frequency = 1.0;
    var norm = 0.0;
    for (var octave = 0u; octave < 4u; octave++) {
        value += dissolve_vnoise(x * frequency, y * frequency, seed + octave * 101u) * amplitude;
        norm += amplitude;
        amplitude *= 0.5;
        frequency *= 2.03;
    }
    return value / norm;
}
fn dissolve_burn(position: vec4<f32>) -> vec4<f32> {
    let size = vec2<f32>(textureDimensions(outgoing));
    let xy = vec2<i32>(position.xy);
    let a = textureLoad(outgoing, xy, 0);
    let b = textureLoad(incoming, xy, 0);
    let softness = params.blur;
    let noise_scale = max(220.0 * size.y / 1080.0, 1.0);
    let lum = pow(clamp(luma(a), 0.0, 1.0), 1.0 / 2.2);
    let noise = 0.55 * dissolve_fbm(position.x / noise_scale, position.y / noise_scale, params.direction);
    let g = noise + 0.45 * (1.0 - lum);
    let d = g - (-softness + params.progress * (1.0 + 2.0 * softness));
    let k = 1.0 - smoothstep_cpu(-softness * 0.3, softness * 0.3, d);
    let base = mix(a, b, k);
    let fade = min(min(params.progress * 10.0, (1.0 - params.progress) * 10.0), 1.0);
    var charred = 1.0;
    if d > 0.0 { charred = 1.0 - 0.6 * exp(-pow(d / softness, 2.0)) * fade; }
    let edge = exp(-pow(d / (softness * 0.6), 2.0)) * fade;
    let light = edge * 2.0;
    return vec4(base.rgb * charred + params.color.rgb * light, min(base.a + max(light, 0.0), 1.0));
}
fn dissolve_mosaic(position: vec4<f32>) -> vec4<f32> {
    let size = vec2<f32>(textureDimensions(outgoing));
    let p = params.progress;
    let bell_t = 4.0 * p * (1.0 - p);
    let bell = bell_t * bell_t * (3.0 - 2.0 * bell_t);
    let block = 1.0 + (params.blur - 1.0) * bell;
    let k = smoothstep_cpu(0.3, 0.7, p);
    if block < 1.5 {
        return mix(pixel(0u, vec2<u32>(position.xy)), pixel(1u, vec2<u32>(position.xy)), k);
    }
    let bx = floor(position.x / block);
    let by = floor(position.y / block);
    let center = (vec2(bx, by) + vec2(0.5)) * block;
    var mix_b = k;
    if params.direction != 0u {
        let random = dissolve_hash(i32(bx), i32(by), 7u);
        mix_b = smoothstep_cpu(random - 0.15, random + 0.15, k * 1.3 - 0.15);
    }
    return mix(sample_clamped(0u, center, vec2<u32>(size)), sample_clamped(1u, center, vec2<u32>(size)), mix_b);
}
fn transition_shape_distance(shape: u32, x: f32, y: f32) -> f32 {
    let r = sqrt(x * x + y * y);
    let theta = atan2(-y, x);
    if shape == 1u { return max(abs(x), abs(y)); }
    if shape == 2u { return (abs(x) + abs(y)) * 0.7071067811865476; }
    if shape == 3u { return star_distance(x, y, 5u, 0.45); }
    if shape == 4u { return r / (0.5 * (1.0 - sin(theta)) + 0.08) * 0.5; }
    if shape == 5u || shape == 6u {
        var n = 6.0;
        var rotation = 0.0;
        if shape == 5u { n = 3.0; rotation = 1.5707963267948966 - 3.141592653589793 / 3.0; }
        let segment = 6.283185307179586 / n;
        let angle = rem_euclid(theta - rotation, segment) - segment / 2.0;
        return r * cos(angle) / cos(3.141592653589793 / n);
    }
    return r;
}
fn shape_dissolve_field(position: vec4<f32>) -> f32 {
    let size = params.extra0.z;
    let x = position.x;
    let y = position.y;
    let cx = floor(x / size);
    let cy = floor(y / size);
    let lx = x - (cx + 0.5) * size;
    let ly = y - (cy + 0.5) * size;
    let rotation = params.dx * 0.017453292519943295;
    let s = sin(rotation);
    let c = cos(rotation);
    let u0 = lx / (size * 0.5);
    let v0 = ly / (size * 0.5);
    let u = u0 * c + v0 * s;
    let v = -u0 * s + v0 * c;
    let order = bitcast<u32>(params.amount);
    var delay = dissolve_hash(i32(cx), i32(cy), bitcast<u32>(params.dy));
    if order == 1u { delay = cx / params.extra0.w; }
    if order == 2u {
        let dx = (cx + 0.5) - params.extra1.x * 0.5;
        let dy = (cy + 0.5) - params.extra1.y * 0.5;
        delay = sqrt(dx * dx + dy * dy) / max(params.extra1.y, 1.0);
    }
    return (transition_shape_distance(params.direction, u, v) + delay * 1.5) * size * 0.5;
}
fn shape_dissolve(position: vec4<f32>) -> vec4<f32> {
    let d = shape_dissolve_field(position) -
        (params.extra0.x - (params.blur + 3.0) + params.progress * (params.extra0.y - params.extra0.x + 2.0 * (params.blur + 3.0)));
    return wipe_pixel(pixel(0u, vec2<u32>(position.xy)), pixel(1u, vec2<u32>(position.xy)), d, params.blur, 0.0, 1.0);
}
fn shape_flow(position: vec4<f32>) -> vec4<f32> {
    let size = vec2<f32>(textureDimensions(outgoing));
    let shape = params.direction;
    let direction = bitcast<u32>(params.amount);
    var dx = 1.0;
    var dy = 0.0;
    switch direction {
        case 0u: { dx = 0.0; dy = 1.0; }
        case 1u: { dx = -1.0; dy = 0.0; }
        case 2u: { dx = 0.0; dy = -1.0; }
        default: {}
    }
    let c1 = size * 0.5;
    let c0 = c1 - vec2(dx * size.x * 0.75, dy * size.y * 0.75);
    let cover = max(max(transition_shape_distance(shape, -c1.x, -c1.y), transition_shape_distance(shape, size.x - c1.x, -c1.y)),
        max(transition_shape_distance(shape, -c1.x, size.y - c1.y), transition_shape_distance(shape, size.x - c1.x, size.y - c1.y)));
    let softness = params.blur;
    let max_radius = cover * 1.05 + softness + 4.0;
    let p = params.progress;
    let ease_out = 1.0 - pow(1.0 - p, 3.0);
    var ease = 1.0 - pow(-2.0 * p + 2.0, 3.0) / 2.0;
    if p < 0.5 { ease = 4.0 * p * p * p; }
    let center = c0 + (c1 - c0) * ease_out;
    let radius = max_radius * ease;
    let d = transition_shape_distance(shape, position.x - center.x, position.y - center.y) - radius;
    return wipe_pixel(pixel(0u, vec2<u32>(position.xy)), pixel(1u, vec2<u32>(position.xy)), d, softness, 0.0, 1.0);
}
fn luma(c: vec4<f32>) -> f32 {
    return dot(c.rgb, vec3(0.2126, 0.7152, 0.0722));
}
fn dissolve_extra(position: vec4<f32>) -> vec4<f32> {
    let p = params.progress;
    let xy = vec2<i32>(position.xy);
    let a = textureLoad(outgoing, xy, 0);
    let b = textureLoad(incoming, xy, 0);
    if params.kernel == 10u {
        let half = (1.0 - params.blur) * 0.5;
        if p < half { return mix(a, params.color, p / half); }
        if p > 1.0 - half { return mix(params.color, b, (p - (1.0 - half)) / half); }
        return params.color;
    }
    if params.kernel == 11u {
        let softness = clamp(params.blur, 0.005, 1.0) * 0.5;
        let source = select(b, a, params.direction == 0u);
        let lum = pow(clamp(luma(source), 0.0, 1.0), 1.0 / 2.2);
        let g = select(1.0 - lum, lum, params.pad != 0u);
        let threshold = -softness + p * (1.0 + 2.0 * softness);
        return mix(a, b, smoothstep_cpu(-softness, softness, threshold - g));
    }
    if params.kernel == 12u {
        let ka = min(2.0 * (1.0 - p), 1.0);
        let kb = min(2.0 * p, 1.0);
        let aa = a * ka;
        let bb = b * kb;
        return select(bb, aa, luma(aa) >= luma(bb));
    }
    let size = vec2<f32>(textureDimensions(outgoing));
    let uv = position.xy / size;
    if params.kernel == 13u {
        let aspect = size.x / max(size.y, 1.0);
        let angle = atan2(-(uv.y - 0.5), (uv.x - 0.5) * aspect) / 6.283185307179586;
        let mix_b = 1.0 - legacy_step(p, fract(angle));
        return mix(a, b, mix_b);
    }
    if params.kernel == 14u {
        let mix_b = 1.0 - legacy_step(p, fract(uv.y * 10.0));
        return mix(a, b, mix_b);
    }
    if params.kernel == 15u {
        let cell = i32(floor(uv.x * 8.0)) + i32(floor(uv.y * 8.0));
        var offset = 0.0;
        if cell % 2 != 0 { offset = 0.5; }
        let threshold = clamp(p * 2.0 - offset, 0.0, 1.0);
        let mix_b = 1.0 - legacy_step(threshold, fract(uv.x * 8.0));
        return mix(a, b, mix_b);
    }
    let softness = params.blur;
    let g = clamp(uv.x * 0.7 + uv.y * 0.3, 0.0, 1.0);
    let mix_b = clamp((p * (1.0 + softness) - g) / softness, 0.0, 1.0);
    return mix(a, b, mix_b);
}
fn flare(at: vec4<f32>) -> vec4<f32> {
    let p = params.progress;
    let k = smoothstep_cpu(0.3, 0.7, p);
    let env_t = 4.0 * p * (1.0 - p);
    let env = env_t * env_t * (3.0 - 2.0 * env_t);
    let xy = vec2<i32>(at.xy);
    let ca = textureLoad(outgoing, xy, 0);
    let cb = textureLoad(incoming, xy, 0);
    var c = ca;
    if k >= 1.0 { c = cb; } else if k > 0.0 { c = ca + (cb - ca) * k; }
    let size = vec2<f32>(textureDimensions(outgoing));
    let diag = length(size);
    let fx = size.x / 2.0 + params.dx * diag * (p - 0.5) * 0.9;
    let fy = size.y / 2.0 + params.dy * diag * (p - 0.5) * 0.9;
    let unit = max(size.y, 1.0);
    let rx = at.x - fx;
    let ry = at.y - fy;
    let along = rx * params.dx + ry * params.dy;
    let across = -rx * params.dy + ry * params.dx;
    let core = blob(at.x, at.y, fx, fy, unit * 0.06) * 3.0 + blob(at.x, at.y, fx, fy, unit * 0.35) * 0.6;
    let streak = exp(-(abs(across) / (unit * 0.01))) * exp(-(abs(along) / (diag * 0.35))) * 1.5;
    let gx = size.x - fx;
    let gy = size.y - fy;
    let ghost = blob(at.x, at.y, gx, gy, unit * 0.08) * 0.4 + blob(at.x, at.y, (gx + fx) / 2.0, (gy + fy) / 2.0, unit * 0.05) * 0.3;
    let light = (core + streak + ghost) * params.amount * env;
    return vec4(c.rgb + params.color.rgb * light, min(c.a + max(light, 0.0), 1.0));
}

@vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let positions = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    return vec4(positions[i], 0.0, 1.0);
}

fn transition_pixel_color(position: vec4<f32>) -> vec4<f32> {
    let xy = vec2<i32>(position.xy);
    let a = textureLoad(outgoing, xy, 0);
    let b = textureLoad(incoming, xy, 0);
    let p = params.progress;
    // CPU apply_scaled returns exact input frames at both endpoints.
    if p <= 0.0 { return a; }
    if p >= 1.0 { return b; }
    if params.kernel == 5u { return push(position); }
    if params.kernel == 100u { return legacy_center_split(position.xy, vec2<f32>(textureDimensions(outgoing))); }
    if params.kernel == 101u { return legacy_band_slide(position.xy, vec2<f32>(textureDimensions(outgoing))); }
    if params.kernel == 102u || params.kernel == 103u { return legacy_text_cells(position.xy, vec2<f32>(textureDimensions(outgoing))); }
    if params.kernel == 112u { return vr_random_blocks(position); }
    if params.kernel == 113u || params.kernel == 114u { return vr_leaks(position); }
    if params.kernel == 115u { return vr_light_rays(position); }
    if params.kernel == 116u { return vr_mobius_zoom(position); }
    if params.kernel == 94u { return cards_apply(position); }
    if params.kernel == 95u { return cards_apply(position); }
    if params.kernel == 87u { return flip_motion_apply(position); }
    if params.kernel == 92u { return spin_3d_apply(position); }
    if params.kernel == 91u { return slice_apply(position); }
    if params.kernel == 93u { return spinback_3d_apply(position); }
    if params.kernel == 63u { return transition_frame(position, vec2<f32>(textureDimensions(outgoing))); }
    if params.kernel == 6u { return flare(position); }
    if params.kernel == 7u { return linear_wipe(position, true); }
    if params.kernel == 8u { return linear_wipe(position, false); }
    if params.kernel == 9u { return legacy_wipe(position); }
    if params.kernel >= 10u && params.kernel <= 16u { return dissolve_extra(position); }
    if (params.kernel >= 20u && params.kernel <= 30u) || params.kernel == 98u { return field_wipe(position); }
    if params.kernel == 31u { return stretch_wipe(position); }
    if params.kernel == 96u { return neon_wipe(position); }
    if params.kernel == 99u { return vr_gradient_wipe(position); }
    if params.kernel >= 64u && params.kernel <= 79u { return lights_apply(position); }
    if (params.kernel >= 32u && params.kernel <= 62u) { return motion_apply(position); }
    if (params.kernel >= 80u && params.kernel <= 86u) || (params.kernel >= 104u && params.kernel <= 106u) { return grunge_apply(position); }
    if params.kernel == 17u { return dissolve_burn(position); }
    if params.kernel == 18u { return dissolve_mosaic(position); }
    if params.kernel == 19u { return shape_dissolve(position); }
    if params.kernel == 97u { return shape_flow(position); }
    switch params.kernel {
        case 0u: { return a * (1.0 - p) + b * p; }
        case 1u: { return min(a * min(2.0 * (1.0 - p), 1.0) + b * min(2.0 * p, 1.0), vec4(1.0)); }
        case 2u: {
            let gamma = vec4(1.0 / 2.2);
            return pow(pow(max(a, vec4(0.0)), gamma) * (1.0 - p) + pow(max(b, vec4(0.0)), gamma) * p, vec4(2.2));
        }
        default: {
            let background = vec4(vec3(select(0.0, 1.0, params.kernel == 4u)), 1.0);
            var source = b * ((p - 0.5) * 2.0);
            if p < 0.5 { source = a * (1.0 - p * 2.0); }
            if source.a <= 0.0 { return background; }
            return source + background * (1.0 - source.a);
        }
    }
}

// Round before attachment conversion; truncating HDR values on some render targets loses a full half-float ULP.
fn transition_half_rne(v: f32) -> f32 {
    let magnitude = abs(v);
    if magnitude < 0.00006103515625 {
        return sign(v) * round(magnitude * 16777216.0) / 16777216.0;
    }
    if magnitude > 65504.0 { return v; }
    let bits = bitcast<u32>(v);
    return bitcast<f32>((bits + 4095u + ((bits >> 13u) & 1u)) & 0xffffe000u);
}

@fragment fn fs(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    return transition_pixel_color(position);
}

@fragment fn fs_half(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let c = transition_pixel_color(position);
    return vec4(transition_half_rne(c.r), transition_half_rne(c.g), transition_half_rne(c.b), transition_half_rne(c.a));
}
