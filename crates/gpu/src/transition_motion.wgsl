// Exact image-space legacy affine transitions. Shared catalog supplies textures and params.
fn motion_bell(p: f32) -> f32 {
    let t = 4.0 * p * (1.0 - p);
    return t * t * (3.0 - 2.0 * t);
}
fn motion_hash(x: i32, y: i32, seed: u32) -> f32 {
    var h = bitcast<u32>(x) * 0x8DA6B343u ^ bitcast<u32>(y) * 0xD8163841u ^ seed * 0xCB1AB31Fu ^ 0x9E3779B9u;
    h = h ^ (h >> 15u);
    h = h * 0x2C1B3C6Du;
    h = h ^ (h >> 12u);
    h = h * 0x297A2D39u;
    h = h ^ (h >> 15u);
    return f32(h >> 8u) / 16777216.0;
}
fn motion_ease_in_out(p: f32) -> f32 {
    if p < 0.5 { return 4.0 * p * p * p; }
    return 1.0 - pow(-2.0 * p + 2.0, 3.0) * 0.5;
}
fn motion_over(fg: vec4<f32>, bg: vec4<f32>) -> vec4<f32> {
    return fg + bg * (1.0 - fg.a);
}
fn motion_raw_pixel(side: u32, at: vec2<i32>, extent: vec2<i32>) -> vec4<f32> {
    if any(at < vec2<i32>(0)) || any(at >= extent) { return vec4(0.0); }
    if side == 0u { return textureLoad(outgoing, at, 0); }
    return textureLoad(incoming, at, 0);
}
fn motion_sample_raw(side: u32, at: vec2<f32>, extent: vec2<i32>) -> vec4<f32> {
    let f = at - vec2(0.5);
    let lo = vec2<i32>(floor(f));
    let k = f - vec2<f32>(lo);
    let a = motion_raw_pixel(side, lo, extent);
    let b = motion_raw_pixel(side, lo + vec2(1, 0), extent);
    let c = motion_raw_pixel(side, lo + vec2(0, 1), extent);
    let d = motion_raw_pixel(side, lo + vec2(1, 1), extent);
    return mix(mix(a, b, k.x), mix(c, d, k.x), k.y);
}
fn motion_card_local(xy: vec2<f32>, offset: vec2<f32>, z: f32, extent: vec2<f32>) -> vec2<f32> {
    let focal = max(extent.x, extent.y) * 6.0;
    let center = extent * 0.5;
    let t = (focal + z) / focal;
    return center + (xy - center) * t - offset;
}
fn motion_coverage(local: vec2<f32>, rect: vec4<f32>) -> f32 {
    let edge = min(min(local.x - rect.x, rect.z - local.x), min(local.y - rect.y, rect.w - local.y));
    return clamp(edge + 0.5, 0.0, 1.0);
}
fn motion_affine_sample(side: u32, xy: vec2<f32>, extent: vec2<f32>, offset: vec2<f32>, scale: f32, angle: f32) -> vec4<f32> {
    let center = extent * 0.5;
    let d = (xy - center - offset) / max(scale, 1e-3);
    let sn = sin(angle);
    let cs = cos(angle);
    let local = center + vec2(d.x * cs + d.y * sn, -d.x * sn + d.y * cs);
    return sample_clamped(side, local, vec2<u32>(extent)) * motion_coverage(local, vec4(0.0, 0.0, extent.x, extent.y));
}
fn motion_roll3d_hit(origin: vec3<f32>, ex: vec3<f32>, ey: vec3<f32>, eye: vec3<f32>, xy: vec2<f32>) -> vec4<f32> {
    let d = vec3(xy, 0.0) - eye;
    let nd = -d;
    let rhs = eye - origin;
    let det = dot(ex, cross(ey, nd));
    if abs(det) < 1e-9 { return vec4(0.0); }
    let lx = dot(rhs, cross(ey, nd)) / det;
    let ly = dot(ex, cross(rhs, nd)) / det;
    let t = dot(ex, cross(ey, rhs)) / det;
    if t <= 1e-4 { return vec4(0.0); }
    let front = select(0.0, 1.0, dot(cross(ex, ey), d) > 0.0);
    return vec4(lx, ly, t, front);
}
fn motion_roll3d_card(origin: vec3<f32>, ex: vec3<f32>, ey: vec3<f32>, eye: vec3<f32>, xy: vec2<f32>, extent: vec2<f32>, side: u32) -> vec4<f32> {
    let hit = motion_roll3d_hit(origin, ex, ey, eye, xy);
    if hit.z <= 0.0 || hit.w <= 0.0 { return vec4(0.0); }
    let cov = motion_coverage(hit.xy, vec4(0.0, 0.0, extent.x, extent.y));
    if cov <= 0.0 { return vec4(0.0); }
    let n = cross(ex, ey);
    let light = 0.45 + 0.55 * abs(n.z) / max(length(n), 1e-9);
    let color = sample_clamped(side, hit.xy, vec2<u32>(extent));
    return vec4(color.rgb * light, color.a) * cov;
}
fn motion_roll3d_apply(xy: vec2<f32>, extent: vec2<f32>, p: f32, direction: u32, perspective: f32) -> vec4<f32> {
    let horizontal = direction == 1u || direction == 3u;
    let side = select(extent.y, extent.x, horizontal);
    let sign = select(select(1.0, -1.0, direction == 2u), select(-1.0, 1.0, direction == 3u), horizontal);
    let focal = max(max(extent.x, extent.y) * (6.0 - 5.0 * clamp(perspective * 0.01, 0.0, 1.0)), max(extent.x, extent.y) * 0.6);
    let center = vec3(extent * 0.5, side * 0.5);
    let eye = vec3(extent * 0.5, -max(focal, 1.0));
    let angle_a = motion_ease_in_out(p) * 1.5707963267948966 * sign;
    let angle_b = angle_a - 1.5707963267948966 * sign;
    let back = side * 0.45 * motion_bell(p);
    var origin_a = vec3(0.0);
    var origin_b = vec3(0.0);
    var ex_a = vec3(1.0, 0.0, 0.0);
    var ex_b = vec3(1.0, 0.0, 0.0);
    var ey_a = vec3(0.0, 1.0, 0.0);
    var ey_b = vec3(0.0, 1.0, 0.0);
    if horizontal {
        let ca = cos(angle_a); let sa = sin(angle_a);
        let cb = cos(angle_b); let sb = sin(angle_b);
        ex_a = vec3(ca, 0.0, sa); ex_b = vec3(cb, 0.0, sb);
        origin_a = vec3(center.x * (1.0 - ca) + center.z * sa, 0.0, center.z * (1.0 - ca) - center.x * sa + back);
        origin_b = vec3(center.x * (1.0 - cb) + center.z * sb, 0.0, center.z * (1.0 - cb) - center.x * sb + back);
    } else {
        let ca = cos(angle_a); let sa = sin(angle_a);
        let cb = cos(angle_b); let sb = sin(angle_b);
        ey_a = vec3(0.0, ca, sa); ey_b = vec3(0.0, cb, sb);
        origin_a = vec3(0.0, center.y * (1.0 - ca) + center.z * sa, center.z * (1.0 - ca) - center.y * sa + back);
        origin_b = vec3(0.0, center.y * (1.0 - cb) + center.z * sb, center.z * (1.0 - cb) - center.y * sb + back);
    }
    let ha = motion_roll3d_hit(origin_a, ex_a, ey_a, eye, xy);
    let hb = motion_roll3d_hit(origin_b, ex_b, ey_b, eye, xy);
    let a = motion_roll3d_card(origin_a, ex_a, ey_a, eye, xy, extent, 0u);
    let b = motion_roll3d_card(origin_b, ex_b, ey_b, eye, xy, extent, 1u);
    if ha.z <= 0.0 { return b; }
    if hb.z <= 0.0 { return a; }
    if ha.z > hb.z { return motion_over(b, a); }
    return motion_over(a, b);
}
fn motion_camera_sample(side: u32, xy: vec2<f32>, zoom: f32, rotation: f32, blur: f32, extent: vec2<u32>) -> vec4<f32> {
    let center = vec2<f32>(extent) * 0.5;
    let uv = (xy - center) / zoom;
    let sn = sin(rotation);
    let cs = cos(rotation);
    let at = vec2<f32>(uv.x * cs + uv.y * sn, -uv.x * sn + uv.y * cs) + center;
    let count = clamp(u32(ceil(blur * f32(extent.x) * 0.15)), 1u, 40u);
    let delta = (at - center) * blur * 0.5;
    let start = at - delta;
    if count == 1u || abs(delta.x) + abs(delta.y) < 0.5 { return sample_clamped(side, start, extent); }
    var sum = vec4(0.0);
    for (var i = 0u; i < count; i++) {
        let t = f32(i) / f32(count - 1u) * 2.0 - 1.0;
        sum += sample_clamped(side, start + delta * t, extent);
    }
    return sum / f32(count);
}
fn motion_apply(position: vec4<f32>) -> vec4<f32> {
    let extent_u = textureDimensions(outgoing);
    let extent_i = vec2<i32>(extent_u);
    let extent = vec2<f32>(extent_u);
    let xy = position.xy;
    let p = params.progress;
    if p <= 0.0 { return textureLoad(outgoing, vec2<i32>(xy), 0); }
    if p >= 1.0 { return textureLoad(incoming, vec2<i32>(xy), 0); }
    let op = params.kernel;
    if op == 60u {
        var d = vec2(0.0, 1.0);
        switch params.direction {
            case 1u: { d = vec2(-1.0, 0.0); }
            case 2u: { d = vec2(0.0, -1.0); }
            case 3u: { d = vec2(1.0, 0.0); }
            default: {}
        }
        let travel = dot(abs(d), extent);
        let a = motion_sample_raw(0u, xy - d * travel * p, extent_i);
        let b = motion_sample_raw(1u, xy - d * travel * (p - 1.0), extent_i);
        return motion_over(b, a);
    }
    if op == 61u {
        var offset = vec2(0.0, -extent.y);
        switch params.direction {
            case 1u: { offset = vec2(extent.x, 0.0); }
            case 2u: { offset = vec2(0.0, extent.y); }
            case 3u: { offset = vec2(-extent.x, 0.0); }
            default: {}
        }
        let b = motion_sample_raw(1u, xy - offset * (1.0 - p), extent_i);
        return motion_over(b, textureLoad(outgoing, vec2<i32>(xy), 0));
    }
    if op == 62u {
        let shift = extent.x * 0.5 * p;
        let left = select(vec4(0.0), motion_sample_raw(0u, xy + vec2(shift, 0.0), extent_i), xy.x < extent.x * 0.5 - shift);
        let right = select(vec4(0.0), motion_sample_raw(0u, xy - vec2(shift, 0.0), extent_i), xy.x >= extent.x * 0.5 + shift);
        return motion_over(right, motion_over(left, textureLoad(incoming, vec2<i32>(xy), 0)));
    }
    if op == 59u {
        let pixel = vec2<f32>(vec2<i32>(xy));
        let uv_mask = (pixel + 0.5) / extent;
        let mask_d = (uv_mask.x + uv_mask.y) * 0.5 - (1.0 - p);
        let k = select(0.0, 1.0, mask_d > 0.0);
        var out = mix(textureLoad(outgoing, vec2<i32>(xy), 0), textureLoad(incoming, vec2<i32>(xy), 0), k);
        let d = (pixel.x / extent.x + pixel.y / extent.y) * 0.5 - (1.0 - p);
        if d < 0.0 && d > -0.06 {
            let shade = 0.75 + 0.25 * (-d / 0.06);
            out = vec4(out.rgb * shade, out.a);
        }
        return out;
    }
    if op == 52u {
        let horizontal = params.direction == 1u || params.direction == 3u;
        let negative = params.direction == 1u || params.direction == 2u;
        let axis_len = select(extent.y, extent.x, horizontal);
        let refl = axis_len * 0.3;
        let edge = -refl + motion_ease_in_out(p) * (axis_len + 2.0 * refl);
        let s = select(xy.y, xy.x, horizontal);
        let from_entry = select(s, axis_len - s, negative);
        if from_entry < edge { return sample_clamped(1u, xy, extent_u); }
        let a = sample_clamped(0u, xy, extent_u);
        let d = from_entry - edge;
        if d < refl {
            let mirrored = edge - d;
            var at = xy;
            if horizontal { at.x = select(mirrored, axis_len - mirrored, negative); }
            else { at.y = select(mirrored, axis_len - mirrored, negative); }
            return mix(a, sample_clamped(1u, at, extent_u), 0.75 * (1.0 - d / refl));
        }
        return a;
    }
    if op == 53u || op == 54u {
        let blur = clamp(select(params.extra0.x, params.extra0.y, op == 54u) * 0.01, 0.0, 1.0);
        let count = select(1u, clamp(u32(ceil(blur * 12.0)), 2u, 12u), blur > 0.0);
        var d = vec2(0.0, 1.0);
        switch params.direction {
            case 1u: { d = vec2(-1.0, 0.0); }
            case 2u: { d = vec2(0.0, -1.0); }
            case 3u: { d = vec2(1.0, 0.0); }
            default: {}
        }
        let travel = dot(abs(d), extent);
        let zoom = select(0.0, params.extra0.x * 0.01, op == 54u);
        var total = vec4(0.0);
        for (var i = 0u; i < count; i++) {
            var q = p;
            if count > 1u { q = clamp(p + (f32(i) / f32(count - 1u) - 0.5) * (select(0.1, 0.08, op == 54u) * blur), 0.0, 1.0); }
            let e = motion_ease_in_out(q);
            var sa = 1.0;
            var sb = 1.0;
            var oa = d * travel * e;
            var ob = d * travel * (e - 1.0);
            if op == 53u {
                sa = 1.0 - 0.25 * motion_bell(q);
                sb = sa;
            } else {
                sa = 1.0 - zoom * motion_bell(q);
                sb = sa;
                let step = travel * 1.08;
                oa = d * step * e;
                ob = d * step * (e - 1.0);
            }
            let a = motion_affine_sample(0u, xy, extent, oa, sa, 0.0);
            let b = motion_affine_sample(1u, xy, extent, ob, sb, 0.0);
            total += motion_over(b, a);
        }
        return total / f32(count);
    }
    if op == 55u {
        let blur = clamp(params.extra0.y * 0.01, 0.0, 1.0);
        let count = select(1u, clamp(u32(ceil(blur * 12.0)), 2u, 12u), blur > 0.0);
        let turn = params.extra0.x * 0.017453292519943295 * select(1.0, -1.0, params.direction == 1u);
        var total = vec4(0.0);
        for (var i = 0u; i < count; i++) {
            var q = p;
            if count > 1u { q = clamp(p + (f32(i) / f32(count - 1u) - 0.5) * (0.06 * blur), 0.0, 1.0); }
            var c = vec4(0.0);
            if q < 0.5 {
                let k = q * 2.0;
                let eased = k * k * k;
                let scale = max(1.0 - eased, 1e-3);
                c = motion_affine_sample(0u, xy, extent, vec2(0.0), scale, turn * 0.5 * eased);
            } else {
                let k = (q - 0.5) * 2.0;
                let eased = 1.0 - pow(1.0 - k, 3.0);
                let scale = max(eased, 1e-3);
                c = motion_affine_sample(1u, xy, extent, vec2(0.0), scale, -turn * 0.5 * (1.0 - eased));
            }
            total += c;
        }
        return total / f32(count);
    }
    if op == 56u {
        return motion_roll3d_apply(xy, extent, p, params.direction, params.extra0.x);
    }
    if op == 57u {
        let horizontal = params.direction == 1u || params.direction == 3u;
        let cols = clamp(round(params.extra0.x), 2.0, 32.0);
        let rows = clamp(round(cols * extent.y / extent.x), 1.0, 32.0);
        let lanes = select(cols, rows, horizontal);
        let cells = select(rows, cols, horizontal);
        let len_al = select(extent.y, extent.x, horizontal);
        let len_ac = select(extent.x, extent.y, horizontal);
        let along = select(xy.y, xy.x, horizontal);
        let across = select(xy.x, xy.y, horizontal);
        let sgn = select(select(1.0, -1.0, params.direction == 2u), select(-1.0, 1.0, params.direction == 3u), horizontal);
        let lane = i32(clamp(floor(across / len_ac * lanes), 0.0, lanes - 1.0));
        let cell_len = len_al / cells;
        let blur = clamp(params.extra0.y * 0.01, 0.0, 1.0);
        let count = select(1u, min(clamp(u32(ceil(blur * 12.0)), 2u, 12u), 4u), blur > 0.0);
        var sum = vec4(0.0);
        for (var sample_i = 0u; sample_i < count; sample_i++) {
            var q = p;
            if count > 1u { q = clamp(p + (f32(sample_i) / f32(count - 1u) - 0.5) * (0.08 * blur), 0.0, 1.0); }
            var color_a = vec4(0.0);
            var color_b = vec4(0.0);
            var got_a = false;
            var got_b = false;
            for (var ci = 0; ci < i32(cells); ci++) {
                let fi = f32(ci);
                let order = select(fi, cells - 1.0 - fi, sgn > 0.0) / cells;
                let delay = 0.6 * (0.6 * order + 0.4 * motion_hash(ci, lane, 17u));
                let qi = clamp((q - delay) / 0.4, 0.0, 1.0);
                let e = motion_ease_in_out(qi);
                let off_a = sgn * len_al * e;
                let off_b = sgn * len_al * (e - 1.0);
                let lo = fi * cell_len;
                let hi = (fi + 1.0) * cell_len;
                let pa = along - off_a;
                if !got_a && pa >= lo && pa < hi {
                    var at_a = xy;
                    if horizontal { at_a.x = pa; } else { at_a.y = pa; }
                    color_a = sample_clamped(0u, at_a, extent_u);
                    got_a = true;
                }
                let pb = along - off_b;
                if !got_b && pb >= lo && pb < hi {
                    var at_b = xy;
                    if horizontal { at_b.x = pb; } else { at_b.y = pb; }
                    color_b = sample_clamped(1u, at_b, extent_u);
                    got_b = true;
                }
            }
            if got_a { sum += motion_over(color_b, color_a); }
            else { sum += color_b; }
        }
        return sum / f32(count);
    }
    if op == 47u {
        let zoom = params.extra0.x * 0.01;
        let rotation = params.extra0.y * 0.017453292519943295;
        let blur = params.extra0.z * 0.01 * motion_bell(p) * 0.35;
        let ease_in = p * p * p;
        let camera_a = min(p / 0.7, 1.0);
        let zoom_a = 1.0 + zoom * camera_a * camera_a * camera_a;
        let camera_b = clamp((p - 0.3) / 0.7, 0.0, 1.0);
        let zoom_b = 1.0 + zoom * pow(1.0 - camera_b, 3.0);
        let rot_a = rotation * ease_in;
        let rot_b = -rotation * pow(1.0 - p, 3.0);
        let ca = motion_camera_sample(0u, xy, zoom_a, rot_a, blur, extent_u);
        let cb = motion_camera_sample(1u, xy, zoom_b, rot_b, blur, extent_u);
        return mix(ca, cb, smoothstep(0.3, 0.7, p));
    }
    if op == 42u {
        let blur = clamp(params.extra0.x * 0.01, 0.0, 1.0);
        let count = select(1u, clamp(u32(ceil(blur * 12.0)), 2u, 12u), blur > 0.0);
        let horizontal = params.direction == 1u || params.direction == 3u;
        let radius = select(extent.x, extent.y, horizontal) * 2.0;
        var hub = vec2(extent.x * 0.5 - radius, extent.y * 0.5);
        if horizontal { hub = vec2(extent.x * 0.5, extent.y * 0.5 + radius); }
        let axis_length = select(extent.y, extent.x, horizontal);
        let gap = axis_length * 1.15 / radius;
        let sign = select(select(1.0, -1.0, params.direction == 2u), select(-1.0, 1.0, params.direction == 3u), horizontal);
        var accum = vec4(0.0);
        for (var i = 0u; i < count; i++) {
            var q = p;
            if count > 1u { q = clamp(p + (f32(i) / f32(count - 1u) - 0.5) * (0.1 * blur), 0.0, 1.0); }
            let angle_a = motion_ease_in_out(q) * gap * sign;
            let angle_b = angle_a - gap * sign;
            let rel = xy - hub;
            let local_a = hub + vec2(cos(angle_a) * rel.x + sin(angle_a) * rel.y, -sin(angle_a) * rel.x + cos(angle_a) * rel.y);
            let local_b = hub + vec2(cos(angle_b) * rel.x + sin(angle_b) * rel.y, -sin(angle_b) * rel.x + cos(angle_b) * rel.y);
            let edge_a = min(min(local_a.x, extent.x - local_a.x), min(local_a.y, extent.y - local_a.y));
            let edge_b = min(min(local_b.x, extent.x - local_b.x), min(local_b.y, extent.y - local_b.y));
            let cov_a = clamp(edge_a + 0.5, 0.0, 1.0);
            let cov_b = clamp(edge_b + 0.5, 0.0, 1.0);
            let ca = sample_clamped(0u, local_a, extent_u) * cov_a;
            let cb = sample_clamped(1u, local_b, extent_u) * cov_b;
            accum += motion_over(cb, ca);
        }
        return accum / f32(count);
    }
    if op == 41u {
        let gap = max(params.extra0.x, 0.0);
        let blur = clamp(params.extra0.y * 0.01, 0.0, 1.0);
        let gap_color = params.extra1.xyz;
        let count = select(1u, clamp(u32(ceil(blur * 12.0)), 2u, 12u), blur > 0.0);
        let horizontal = params.direction == 1u || params.direction == 3u;
        var d = vec2(0.0, 1.0);
        if params.direction == 1u { d = vec2(-1.0, 0.0); }
        if params.direction == 2u { d = vec2(0.0, -1.0); }
        if params.direction == 3u { d = vec2(1.0, 0.0); }
        let travel = dot(abs(d), extent);
        let across = select(xy.x, xy.y, horizontal);
        let across_len = select(extent.x, extent.y, horizontal);
        let pitch = across_len / 14.0;
        var total = vec4(0.0);
        for (var i = 0u; i < count; i++) {
            var q = p;
            if count > 1u { q = clamp(p + (f32(i) / f32(count - 1u) - 0.5) * (0.1 * blur), 0.0, 1.0); }
            let off = (travel + gap) * motion_ease_in_out(q);
            let ax = xy - d * off;
            let bx = ax + d * (travel + gap);
            if all(ax >= vec2(0.0)) && all(ax < extent) {
                total += sample_clamped(0u, ax, extent_u);
            } else if all(bx >= vec2(0.0)) && all(bx < extent) {
                total += sample_clamped(1u, bx, extent_u);
            } else {
                let along = select(ax.x, ax.y, horizontal);
                let edge_band = across / across_len < 0.12 || across / across_len > 0.88;
                let f = fract(across / pitch);
                let remainder = along - floor(along / (travel + gap)) * (travel + gap);
                let g = clamp((remainder - travel) / max(gap, 1.0), 0.0, 1.0);
                let hole = !edge_band && f >= 0.25 && f < 0.75 && g >= 0.3 && g < 0.7;
                if hole { total += vec4(0.5, 0.5, 0.5, 1.0); }
                else { total += vec4(gap_color, 1.0); }
            }
        }
        return total / f32(count);
    }
    if op == 45u {
        let horizontal = params.direction == 1u || params.direction == 3u;
        let axis = select(xy.y, xy.x, horizontal);
        let length = select(extent.y, extent.x, horizontal);
        let forward = select(params.direction == 0u, params.direction == 3u, horizontal);
        let split_at = select(length * (1.0 - motion_ease_in_out(p)), length * motion_ease_in_out(p), forward);
        var local = axis;
        var from_b = false;
        if forward {
            if axis < split_at { from_b = true; local = axis / max(split_at, 1e-3) * length; }
            else { local = (axis - split_at) / max(length - split_at, 1e-3) * length; }
        } else if axis >= split_at {
            from_b = true; local = (axis - split_at) / max(length - split_at, 1e-3) * length;
        } else { local = axis / max(split_at, 1e-3) * length; }
        var sample_at = xy;
        if horizontal { sample_at.x = local; } else { sample_at.y = local; }
        if from_b { return sample_clamped(1u, sample_at, extent_u); }
        return sample_clamped(0u, sample_at, extent_u);
    }
    if op == 48u {
        let e = motion_ease_in_out(p);
        let s = params.extra0.x * 0.01;
        let za = 1.0 + s * e;
        let zb = max(1.0 + s * (e - 1.0), 1e-6);
        let center = extent * 0.5;
        let ca = sample_clamped(0u, (xy - center) / za + center, extent_u);
        let cb = sample_clamped(1u, (xy - center) / zb + center, extent_u);
        return mix(ca, cb, e);
    }
    if op == 50u {
        let direction = params.direction;
        let horizontal = direction == 1u || direction == 3u;
        let negative = direction == 1u || direction == 2u;
        let axis = select(xy.y, xy.x, horizontal);
        let across = select(xy.x, xy.y, horizontal);
        let axis_len = select(extent.y, extent.x, horizontal);
        let travel = axis_len;
        let band = travel * 0.25;
        let edge = -band + p * (travel + 2.0 * band);
        let s = select(axis, axis_len - axis, negative);
        let near = exp(-pow((s - edge) / band, 2.0));
        let disp = params.extra0.x * motion_bell(p) * near * sin(6.283185307179586 * across / max(params.extra0.y, 4.0) - p * 6.0);
        var sample_at = xy;
        if horizontal { sample_at.x += disp; sample_at.y += disp * 0.3; }
        else { sample_at.x += disp * 0.3; sample_at.y += disp; }
        let k = 1.0 - smoothstep(-band * 0.3, band * 0.3, s - edge);
        return mix(sample_clamped(0u, sample_at, extent_u), sample_clamped(1u, sample_at, extent_u), k);
    }
    if op == 51u {
        let horizontal_slats = params.direction == 0u;
        let pos = select(xy.x, xy.y, horizontal_slats);
        let len = select(extent.x, extent.y, horizontal_slats);
        let n = clamp(round(params.extra0.x), 1.0, 64.0);
        let sl = len / n;
        let i = clamp(floor(pos / sl), 0.0, n - 1.0);
        let mid = (i + 0.5) * sl;
        let q = clamp((p - 0.4 * i / n) / 0.6, 0.0, 1.0);
        let co = cos(motion_ease_in_out(q) * 3.141592653589793);
        if abs(co) < 1e-3 { return vec4(0.0, 0.0, 0.0, 1.0); }
        let local = (pos - mid) / co;
        if abs(local) > sl * 0.5 { return vec4(0.0, 0.0, 0.0, 1.0); }
        let src = mid + select(-local, local, co > 0.0);
        var at = xy;
        if horizontal_slats { at.y = src; } else { at.x = src; }
        let color = sample_clamped(select(1u, 0u, co > 0.0), at, extent_u);
        let light = 0.5 + 0.5 * abs(co);
        return vec4(color.rgb * light, color.a);
    }
    if op == 38u {
        let bounce = clamp(params.extra0.x * 0.01, 0.0, 1.0);
        let blur = clamp(params.extra0.y * 0.01, 0.0, 1.0);
        let count = select(1u, clamp(u32(ceil(blur * 12.0)), 2u, 12u), blur > 0.0);
        let omega = (2.0 + 4.0 * bounce) * 3.141592653589793;
        let decay = 4.0 + 6.0 * (1.0 - bounce);
        let f1 = 1.0 - exp(-decay) * cos(omega);
        var d = vec2(0.0, 1.0);
        if params.direction == 1u { d = vec2(-1.0, 0.0); }
        if params.direction == 2u { d = vec2(0.0, -1.0); }
        if params.direction == 3u { d = vec2(1.0, 0.0); }
        let travel = dot(abs(d), extent);
        var total = vec4(0.0);
        for (var i = 0u; i < count; i++) {
            var q = p;
            if count > 1u { q = clamp(p + (f32(i) / f32(count - 1u) - 0.5) * (0.08 * blur), 0.0, 1.0); }
            let x = pow(q, 1.6);
            let fx = 1.0 - exp(-decay * x) * cos(omega * x);
            let e = fx + x * (1.0 - f1);
            let local_a = xy - d * travel * e;
            let local_b = xy - d * travel * (e - 1.0);
            let rect = vec4(0.0, 0.0, extent.x, extent.y);
            let ca = sample_clamped(0u, local_a, extent_u) * motion_coverage(local_a, rect);
            let cb = sample_clamped(1u, local_b, extent_u) * motion_coverage(local_b, rect);
            total += motion_over(cb, ca);
        }
        return total / f32(count);
    }
    if op == 43u {
        let blur = clamp(params.extra0.x * 0.01, 0.0, 1.0);
        let count = select(1u, clamp(u32(ceil(blur * 12.0)), 2u, 12u), blur > 0.0);
        var d = vec2(0.0, 1.0);
        if params.direction == 1u { d = vec2(-1.0, 0.0); }
        if params.direction == 2u { d = vec2(0.0, -1.0); }
        if params.direction == 3u { d = vec2(1.0, 0.0); }
        let travel = max(dot(abs(d), extent), 1.0);
        var accum = vec4(0.0);
        for (var i = 0u; i < count; i++) {
            var q = p;
            if count > 1u { q = clamp(p + (f32(i) / f32(count - 1u) - 0.5) * (0.12 * blur), 0.0, 1.0); }
            let eased = motion_ease_in_out(q);
            let offset = d * travel * (eased - 1.0);
            let local = motion_card_local(xy, offset, 0.0, extent);
            let cb = sample_clamped(1u, local, extent_u) * motion_coverage(local, vec4(0.0, 0.0, extent.x, extent.y));
            let ca = textureLoad(outgoing, vec2<i32>(xy), 0);
            accum += motion_over(cb, ca);
        }
        return accum / f32(count);
    }
    if op == 44u {
        let blur = clamp(params.extra0.x * 0.01, 0.0, 1.0);
        let count = select(1u, clamp(u32(ceil(blur * 12.0)), 2u, 12u), blur > 0.0);
        let vertical_cut = params.direction == 0u;
        let shift = select(vec2(0.0, extent.y * 0.5), vec2(extent.x * 0.5, 0.0), vertical_cut);
        var accum = vec4(0.0);
        for (var i = 0u; i < count; i++) {
            var q = p;
            if count > 1u { q = clamp(p + (f32(i) / f32(count - 1u) - 0.5) * (0.12 * blur), 0.0, 1.0); }
            let eased = motion_ease_in_out(q);
            let offset = shift * eased;
            let b = textureLoad(incoming, vec2<i32>(xy), 0);
            let left_offset = -offset + vec2(0.0, 0.0);
            let right_offset = offset + vec2(0.0, 0.0);
            let left_local = motion_card_local(xy, left_offset, -0.01, extent);
            let right_local = motion_card_local(xy, right_offset, -0.01, extent);
            let left_rect = select(vec4(0.0, 0.0, extent.x, extent.y * 0.5), vec4(0.0, 0.0, extent.x * 0.5, extent.y), vertical_cut);
            let right_rect = select(vec4(0.0, extent.y * 0.5, extent.x, extent.y), vec4(extent.x * 0.5, 0.0, extent.x, extent.y), vertical_cut);
            let left = sample_clamped(0u, left_local, extent_u) * motion_coverage(left_local, left_rect);
            let right = sample_clamped(0u, right_local, extent_u) * motion_coverage(right_local, right_rect);
            accum += motion_over(right, motion_over(left, b));
        }
        return accum / f32(count);
    }
    // Coordinator must reject every unimplemented motion opcode before shader dispatch.
    return vec4(0.0);
}
