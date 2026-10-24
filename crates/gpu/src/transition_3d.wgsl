fn flip_motion_bell(p: f32) -> f32 {
    let t = 4.0 * p * (1.0 - p);
    return t * t * (3.0 - 2.0 * t);
}

fn flip_motion_back_out(x: f32) -> f32 {
    let u = clamp(x, 0.0, 1.0) - 1.0;
    return 1.0 + 2.2 * u * u * u + 1.2 * u * u;
}

fn flip_motion_card_sample(xy: vec2<f32>, extent: vec2<f32>, q: f32, flip: bool) -> vec4<f32> {
    let horizontal = params.direction == 1u || params.direction == 3u;
    let sign = select(select(-1.0, 1.0, params.direction == 2u), select(-1.0, 1.0, params.direction == 3u), horizontal);
    let flip_e = select(0.5 + flip_motion_back_out(q * 2.0 - 1.0) * 0.5, pow(q * 2.0, 3.0) * 0.5, q < 0.5);
    let spin_e = select(1.0 - pow(2.0 - 2.0 * q, 3.0) * 0.5, 4.0 * q * q * q, q < 0.5);
    let e = select(spin_e, flip_e, flip);
    let angle = e * 3.141592653589793 * sign;
    let z = select(0.8, 0.25, flip) * flip_motion_bell(q) * max(extent.x, extent.y);
    let c = cos(angle);
    let s = sin(angle);
    let center = extent * 0.5;
    var origin: vec3<f32>;
    var ex: vec3<f32>;
    var ey: vec3<f32>;
    if horizontal {
        origin = vec3(center.x * (1.0 - c), 0.0, -center.x * s + z);
        ex = vec3(c, 0.0, s);
        ey = vec3(0.0, 1.0, 0.0);
    } else {
        origin = vec3(0.0, center.y * (1.0 - c), -center.y * s + z);
        ex = vec3(1.0, 0.0, 0.0);
        ey = vec3(0.0, c, s);
    }
    let focal = max(max(extent.x, extent.y) * (6.0 - 5.0 * clamp(params.extra0.x * 0.01, 0.0, 1.0)), max(extent.x, extent.y) * 0.6);
    let eye = vec3(center, -max(focal, 1.0));
    let d = vec3(xy, 0.0) - eye;
    let nd = -d;
    let rhs = eye - origin;
    let det = dot(ex, cross(ey, nd));
    if abs(det) < 1e-9 { return vec4(0.0); }
    let lx = dot(rhs, cross(ey, nd)) / det;
    let ly = dot(ex, cross(rhs, nd)) / det;
    let t = dot(ex, cross(ey, rhs)) / det;
    if t <= 1e-4 { return vec4(0.0); }
    let normal = cross(ex, ey);
    let front = dot(normal, d) > 0.0;
    let local = vec2(lx, ly);
    let edge = min(min(lx, extent.x - lx), min(ly, extent.y - ly));
    let coverage = clamp(edge + 0.5, 0.0, 1.0);
    if coverage <= 0.0 { return vec4(0.0); }
    let back_local = select(vec2(extent.x - lx, ly), vec2(lx, extent.y - ly), !horizontal);
    let texel = select(back_local, local, front);
    let light = 0.45 + 0.55 * abs(normal.z) / max(length(normal), 1e-9);
    let side = select(1u, 0u, front);
    var color = sample_clamped(side, texel, vec2<u32>(extent));
    color = vec4(color.rgb * light, color.a);
    return color * coverage;
}

fn flip_motion_apply(position: vec4<f32>) -> vec4<f32> {
    let extent_u = textureDimensions(outgoing);
    let extent = vec2<f32>(extent_u);
    let p = params.progress;
    if p <= 0.0 { return textureLoad(outgoing, vec2<i32>(position.xy), 0); }
    if p >= 1.0 { return textureLoad(incoming, vec2<i32>(position.xy), 0); }
    let blur = clamp(params.extra0.y * 0.01, 0.0, 1.0);
    var count = 1u;
    if blur > 0.0 { count = clamp(u32(ceil(blur * 12.0)), 2u, 12u); }
    var sum = vec4(0.0);
    for (var i = 0u; i < count; i++) {
        var q = p;
        if count > 1u { q = clamp(p + (f32(i) / f32(count - 1u) - 0.5) * (0.06 * blur), 0.0, 1.0); }
        sum += flip_motion_card_sample(position.xy, extent, q, true);
    }
    return sum / f32(count);
}

fn spin_3d_apply(position: vec4<f32>) -> vec4<f32> {
    let p = params.progress;
    if p <= 0.0 { return textureLoad(outgoing, vec2<i32>(position.xy), 0); }
    if p >= 1.0 { return textureLoad(incoming, vec2<i32>(position.xy), 0); }
    return flip_motion_card_sample(position.xy, vec2<f32>(textureDimensions(outgoing)), p, false);
}

fn slice_apply(position: vec4<f32>) -> vec4<f32> {
    let extent_u = textureDimensions(outgoing);
    let extent = vec2<f32>(extent_u);
    let p = params.progress;
    let slices = params.extra0.x;
    let angle = params.extra0.y * 0.017453292519943295;
    let motion_blur = clamp(params.extra0.z * 0.01, 0.0, 1.0);
    let along_axis = vec2(cos(angle), sin(angle));
    let across_axis = vec2(-along_axis.y, along_axis.x);
    let lo = min(0.0, extent.x * across_axis.x) + min(0.0, extent.y * across_axis.y);
    let hi = max(0.0, extent.x * across_axis.x) + max(0.0, extent.y * across_axis.y);
    let span = max(hi - lo, 1.0);
    let distance = extent.x * abs(along_axis.x) + extent.y * abs(along_axis.y) + span * 0.2;
    var count = 1u;
    if motion_blur > 0.0 { count = clamp(u32(ceil(motion_blur * 12.0)), 2u, 12u); }
    var sum = vec4(0.0);
    for (var sample_i = 0u; sample_i < count; sample_i++) {
        var q = p;
        if count > 1u { q = clamp(p + (f32(sample_i) / f32(count - 1u) - 0.5) * (0.08 * motion_blur), 0.0, 1.0); }
        let xy = position.xy;
        let i = clamp(floor((dot(xy, across_axis) - lo) / span * slices), 0.0, slices - 1.0);
        let sign = select(-1.0, 1.0, i32(i) % 2 == 0);
        let qi = clamp((q - 0.3 * i / max(slices, 1.0)) / 0.7, 0.0, 1.0);
        let off = sign * distance * qi * qi * qi;
        let source_a = xy - along_axis * off;
        let zoom = 1.0 + 0.05 * (1.0 - q);
        let b = sample_clamped(1u, (xy - extent * 0.5) / zoom + extent * 0.5, extent_u);
        var color = b;
        if all(source_a >= vec2(0.0)) && all(source_a < extent) {
            let j = clamp(floor((dot(source_a, across_axis) - lo) / span * slices), 0.0, slices - 1.0);
            if j == i {
                color = motion_over(sample_clamped(0u, source_a, extent_u), b);
            }
        }
        sum += color;
    }
    return sum / f32(count);
}

fn spinback_3d_card_sample(xy: vec2<f32>, extent: vec2<f32>, q: f32) -> vec4<f32> {
    let direction = params.direction;
    let horizontal = direction == 1u || direction == 3u;
    let sign = select(select(1.0, -1.0, direction == 0u), -1.0, direction == 1u);
    let local = select(q * 2.0, (q - 0.5) * 2.0, q >= 0.5);
    let amount = select(local * local * local, (1.0 - local) * (1.0 - local) * (1.0 - local), q >= 0.5);
    let angle = select(amount, -amount, q >= 0.5) * 1.5707963267948966 * sign;
    let depth = clamp(params.extra0.x * 0.01, 0.0, 1.0);
    let z = depth * max(extent.x, extent.y) * 1.5 * flip_motion_bell(q);
    let c = cos(angle);
    let s = sin(angle);
    let center = extent * 0.5;
    var origin: vec3<f32>;
    var ex: vec3<f32>;
    var ey: vec3<f32>;
    if horizontal {
        origin = vec3(0.0, center.y * (1.0 - c), -center.y * s + z);
        ex = vec3(1.0, 0.0, 0.0);
        ey = vec3(0.0, c, s);
    } else {
        origin = vec3(center.x * (1.0 - c), 0.0, -center.x * s + z);
        ex = vec3(c, 0.0, s);
        ey = vec3(0.0, 1.0, 0.0);
    }
    let focal = max(extent.x, extent.y) * 3.0;
    let eye = vec3(center, -max(focal, 1.0));
    let d = vec3(xy, 0.0) - eye;
    let nd = -d;
    let rhs = eye - origin;
    let det = dot(ex, cross(ey, nd));
    if abs(det) < 1e-9 { return vec4(0.0); }
    let lx = dot(rhs, cross(ey, nd)) / det;
    let ly = dot(ex, cross(rhs, nd)) / det;
    let t = dot(ex, cross(ey, rhs)) / det;
    if t <= 1e-4 { return vec4(0.0); }
    let normal = cross(ex, ey);
    if dot(normal, d) <= 0.0 { return vec4(0.0); }
    let edge = min(min(lx, extent.x - lx), min(ly, extent.y - ly));
    let coverage = clamp(edge + 0.5, 0.0, 1.0);
    if coverage <= 0.0 { return vec4(0.0); }
    let side = select(0u, 1u, q >= 0.5);
    var color = sample_clamped(side, vec2(lx, ly), vec2<u32>(extent));
    let light = 0.45 + 0.55 * abs(normal.z) / max(length(normal), 1e-9);
    color = vec4(color.rgb * light, color.a);
    return color * coverage;
}

fn spinback_3d_apply(position: vec4<f32>) -> vec4<f32> {
    let p = params.progress;
    if p <= 0.0 { return textureLoad(outgoing, vec2<i32>(position.xy), 0); }
    if p >= 1.0 { return textureLoad(incoming, vec2<i32>(position.xy), 0); }
    return spinback_3d_card_sample(position.xy, vec2<f32>(textureDimensions(outgoing)), p);
}

// Other family routes remain unsupported pending source-equivalent implementations.
