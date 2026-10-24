// Payload in extra0..extra4: angle, amount, strength, intensity, width, radius,
// stripes, length, decay, center.xy, linear color.rgb, seed (u32), remaining zero.
fn lights_f(i: u32) -> f32 {
    switch i {
        case 0u: { return params.extra0.x; } case 1u: { return params.extra0.y; }
        case 2u: { return params.extra0.z; } case 3u: { return params.extra0.w; }
        case 4u: { return params.extra1.x; } case 5u: { return params.extra1.y; }
        case 6u: { return params.extra1.z; } case 7u: { return params.extra1.w; }
        case 8u: { return params.extra2.x; } case 9u: { return params.extra2.y; }
        case 10u: { return params.extra2.z; } case 11u: { return params.extra2.w; }
        case 12u: { return params.extra3.x; } case 13u: { return params.extra3.y; }
        case 14u: { return params.extra3.z; } default: { return params.extra3.w; }
    }
}
fn lights_smooth(a: f32, b: f32, x: f32) -> f32 {
    if b <= a { return select(1.0, 0.0, x < a); }
    let t = clamp((x - a) / (b - a), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}
fn lights_bell(p: f32) -> f32 {
    let t = 4.0 * p * (1.0 - p);
    return t * t * (3.0 - 2.0 * t);
}
fn lights_base(xy: vec2<f32>, k: f32) -> vec4<f32> {
    let at = vec2<u32>(xy);
    if k <= 0.0 { return pixel(0u, at); }
    if k >= 1.0 { return pixel(1u, at); }
    return mix(pixel(0u, at), pixel(1u, at), k);
}
fn lights_mix_clamped(side: u32, xy: vec2<f32>) -> vec4<f32> {
    return sample_clamped(side, xy, textureDimensions(outgoing));
}
fn lights_line(side: u32, x: f32, y: f32, dx: f32, dy: f32, n: u32) -> vec4<f32> {
    if n <= 1u || abs(dx) + abs(dy) < 0.5 { return lights_mix_clamped(side, vec2(x, y)); }
    var sum = vec4(0.0);
    for (var i = 0u; i < n; i++) {
        let q = f32(i) / f32(n - 1u) * 2.0 - 1.0;
        sum += lights_mix_clamped(side, vec2(x + dx * q, y + dy * q));
    }
    return sum / f32(n);
}
fn lights_zoom(side: u32, x: f32, y: f32, cx: f32, cy: f32, amount: f32, n: u32) -> vec4<f32> {
    let dx = (x - cx) * amount * 0.5;
    let dy = (y - cy) * amount * 0.5;
    return lights_line(side, x - dx, y - dy, dx, dy, n);
}
fn lights_disc(side: u32, x: f32, y: f32, radius: f32, n: u32) -> vec4<f32> {
    if radius < 0.5 || n <= 1u { return lights_mix_clamped(side, vec2(x, y)); }
    var sum = vec4(0.0);
    for (var i = 0u; i < n; i++) {
        let r = radius * sqrt((f32(i) + 0.5) / f32(n));
        let a = f32(i) * 2.399963;
        sum += lights_mix_clamped(side, vec2(x + r * cos(a), y + r * sin(a)));
    }
    return sum / f32(n);
}
fn lights_spin(side: u32, x: f32, y: f32, cx: f32, cy: f32, angle: f32, n: u32) -> vec4<f32> {
    if n <= 1u || abs(angle) < 1e-4 { return lights_mix_clamped(side, vec2(x, y)); }
    let rx = x - cx;
    let ry = y - cy;
    var sum = vec4(0.0);
    for (var i = 0u; i < n; i++) {
        let a = (f32(i) / f32(n - 1u) - 0.5) * angle;
        sum += lights_mix_clamped(side, vec2(cx + rx * cos(a) - ry * sin(a), cy + rx * sin(a) + ry * cos(a)));
    }
    return sum / f32(n);
}
fn lights_taps(len: f32) -> u32 { return u32(clamp(ceil(abs(len) * 0.5), 1.0, 40.0)); }
fn lights_hash(x: u32, y: u32) -> f32 {
    let seed = bitcast<u32>(params.extra3.z);
    var h = x * 0x8DA6B343u ^ y * 0xD8163841u ^ seed * 0xCB1AB31Fu ^ 0x9E3779B9u;
    h ^= h >> 15u; h *= 0x2C1B3C6Du; h ^= h >> 12u; h *= 0x297A2D39u; h ^= h >> 15u;
    return f32(h >> 8u) / 16777216.0;
}
fn lights_blob(x: f32, y: f32, cx: f32, cy: f32, r: f32) -> f32 {
    let d = vec2(x - cx, y - cy);
    return exp(-dot(d, d) / max(r * r, 1e-6));
}
fn lights_add(c: vec4<f32>, rgb: vec3<f32>, k: f32) -> vec4<f32> {
    return vec4(c.rgb + rgb * k, min(c.a + max(k, 0.0), 1.0));
}
fn lights_luma(c: vec4<f32>) -> f32 { return 0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b; }
fn lights_perceptual(c: vec4<f32>) -> f32 { return pow(clamp(lights_luma(c), 0.0, 1.0), 1.0 / 2.2); }

fn lights_apply(position: vec4<f32>) -> vec4<f32> {
    let p = params.progress;
    let xy = position.xy;
    let sz = vec2<f32>(textureDimensions(outgoing));
    let k = lights_smooth(0.3, 0.7, p);
    let env = lights_bell(p);
    let angle = lights_f(0u);
    let amount = lights_f(1u);
    let strength = lights_f(2u);
    let intensity = lights_f(3u);
    let width = lights_f(4u);
    let radius = lights_f(5u);
    let nstripes = lights_f(6u);
    let len = lights_f(7u);
    let decay = lights_f(8u);
    let center_flags = bitcast<u32>(params.extra3.w);
    let center = vec2(select(sz.x * 0.5, lights_f(9u), (center_flags & 1u) != 0u), select(sz.y * 0.5, lights_f(10u), (center_flags & 2u) != 0u));
    let color = vec3(lights_f(11u), lights_f(12u), lights_f(13u));
    let dx = sin(angle); let dy = -cos(angle);
    switch params.kernel {
        case 64u: {
            let hue = p * 6.28318530718;
            let tint = vec3(1.0 + 0.5 * cos(hue), 1.0 + 0.5 * cos(hue + 2.1), 1.0 + 0.5 * cos(hue + 4.2));
            let c = lights_base(xy, k); let g = amount * env * 1.8; let l = lights_luma(c);
            return vec4(c.rgb * (vec3(1.0) + g * tint) + vec3(l * g * 0.3, l * g * 0.2, l * g * 0.1), c.a);
        }
        case 65u, 70u: {
            var c = lights_base(xy, k);
            for (var i = 0u; i < 3u; i++) {
                let r0 = lights_hash(i, 1u); let r1 = lights_hash(i, 2u); let r2 = lights_hash(i, 3u); let r3 = lights_hash(i, 4u);
                let leak = params.kernel == 70u;
                let side = select(r0, 0.0, leak);
                let cx = sz.x * (side + (r1 - 0.5) * 0.4 + (p - 0.5) * (0.8 + r0));
                let cy = sz.y * (0.2 + 0.6 * r2 + (p - 0.5) * (r1 - 0.5));
                var col = vec3(1.0, 0.15, 0.6);
                if i == 1u { col = vec3(0.1, 0.8, 1.0); } else if i == 2u { col = vec3(1.0, 0.85, 0.1); }
                if leak { col = color; if i == 1u { col = color * vec3(1.0, 0.8, 0.5); } else if i == 2u { col = vec3(1.0, 0.9, 0.7); } }
                c = lights_add(c, col, lights_blob(xy.x, xy.y, cx, cy, max(sz.x, sz.y) * (0.25 + 0.25 * r3)) * amount * env * 0.7);
            }
            return c;
        }
        case 66u, 76u: {
            let zooms = params.kernel == 66u;
            var za = 1.0 + 0.1 * strength * pow(p, 3.0);
            var zb = 1.0 + 0.1 * strength * (1.0 - pow(1.0 - p, 3.0));
            if zooms { za = 1.0 + 3.0 * strength * pow(min(p * 2.0, 1.0), 3.0); zb = 1.0 + 3.0 * strength * (1.0 - pow(1.0 - clamp((p - 0.5) / 0.5, 0.0, 1.0), 3.0)); }
            let blur = strength * env * 0.6; let taps = lights_taps(blur * max(sz.x, sz.y) * 0.5);
            var a = vec4(0.0); var b = vec4(0.0);
            if k < 1.0 { a = lights_zoom(0u, (xy.x - center.x) / za + center.x, (xy.y - center.y) / za + center.y, center.x, center.y, blur, taps); }
            if k > 0.0 { b = lights_zoom(1u, (xy.x - center.x) / zb + center.x, (xy.y - center.y) / zb + center.y, center.x, center.y, blur, taps); }
            return mix(a, b, k);
        }
        case 67u: {
            let extent = width * env; let taps = lights_taps(extent * 2.0); let shift = extent * 0.5;
            var a = vec4(0.0); var b = vec4(0.0);
            if k < 1.0 { a = lights_line(0u, xy.x - dx * shift * p, xy.y - dy * shift * p, dx * extent, dy * extent, taps); }
            if k > 0.0 { b = lights_line(1u, xy.x + dx * shift * (1.0 - p), xy.y + dy * shift * (1.0 - p), dx * extent, dy * extent, taps); }
            return mix(a, b, k);
        }
        case 68u: {
            let cut = lights_smooth(0.45, 0.55, p);
            let f = clamp(intensity, 0.0, 1.0) * exp(-pow((p - 0.5) / 0.12, 2.0)) * lights_smooth(0.0, 0.2, env);
            return mix(lights_base(xy, cut), vec4(color, 1.0), f);
        }
        case 69u: {
            let ra = radius * lights_smooth(0.0, 0.55, p); let rb = radius * (1.0 - lights_smooth(0.45, 1.0, p));
            let count = u32(clamp(radius * 0.8, 8.0, 48.0));
            var a = vec4(0.0); var b = vec4(0.0);
            if k < 1.0 { a = lights_disc(0u, xy.x, xy.y, ra, count); }
            if k > 0.0 { b = lights_disc(1u, xy.x, xy.y, rb, count); }
            return mix(a, b, k);
        }
        case 71u: {
            let corners = array<vec2<f32>, 4>(vec2(0.0), vec2(sz.x, 0.0), vec2(0.0, sz.y), sz);
            var lo = dot(corners[0], vec2(dx, dy)); var hi = lo;
            for (var j = 1u; j < 4u; j++) { let v = dot(corners[j], vec2(dx, dy)); lo = min(lo, v); hi = max(hi, v); }
            let band = max(width, 2.0); let edge = lo - band + p * (hi - lo + 2.0 * band); let d = dot(xy, vec2(dx, dy)) - edge;
            let c = mix(pixel(0u, vec2<u32>(xy)), pixel(1u, vec2<u32>(xy)), 1.0 - lights_smooth(-band * 0.3, band * 0.3, d));
            return lights_add(c, color, exp(-pow(d / (band * 0.35), 2.0)) * amount * 2.0);
        }
        case 72u: {
            let kk = lights_smooth(0.15, 0.85, p); let persist = pow(1.0 - p, 0.5 + 3.0 * clamp(decay, 0.0, 1.0)) * lights_smooth(0.0, 0.15, p);
            let a = pixel(0u, vec2<u32>(xy)); let c = mix(a, pixel(1u, vec2<u32>(xy)), kk);
            return lights_add(c, color, pow(lights_perceptual(a), 2.0) * persist * 1.5);
        }
        case 73u: {
            let max_angle = angle; let blur = max_angle * env; let count = lights_taps(blur * max(sz.x, sz.y) * 0.5);
            let ra = max_angle * 0.25 * pow(p, 3.0); let rb = -max_angle * 0.25 * (1.0 - (1.0 - pow(1.0 - p, 3.0)));
            let ux = xy.x - center.x; let uy = xy.y - center.y;
            var a = vec4(0.0); var b = vec4(0.0);
            if k < 1.0 { a = lights_spin(0u, center.x + ux * cos(ra) + uy * sin(ra), center.y - ux * sin(ra) + uy * cos(ra), center.x, center.y, blur, count); }
            if k > 0.0 { b = lights_spin(1u, center.x + ux * cos(rb) + uy * sin(rb), center.y - ux * sin(rb) + uy * cos(rb), center.x, center.y, blur, count); }
            return mix(a, b, k);
        }
        case 74u: {
            let c = lights_base(xy, k); let a = c.a;
            let g = min(pow(max(c.rgb, vec3(0.0)), vec3(1.0 / 2.2)), vec3(1.0));
            let s = min(select(g, vec3(1.0) - g, g > vec3(0.5)) * 2.0, vec3(1.0));
            return mix(c, vec4(pow(s, vec3(2.2)) * a, a), clamp(amount, 0.0, 1.0) * env);
        }
        case 75u: {
            let sdx = cos(angle); let sdy = sin(angle); let perp = vec2(-sdy, sdx); let sdir = vec2(sdx, sdy);
            let corners = array<vec2<f32>, 4>(vec2(0.0), vec2(sz.x, 0.0), vec2(0.0, sz.y), sz);
            var alo = dot(corners[0], sdir); var ahi = alo; var clo = dot(corners[0], perp); var chi = clo;
            for (var j = 1u; j < 4u; j++) { let aa = dot(corners[j], sdir); let cc = dot(corners[j], perp); alo = min(alo, aa); ahi = max(ahi, aa); clo = min(clo, cc); chi = max(chi, cc); }
            let span = max(ahi - alo, 1.0); let bar = span * 0.08;
            let i = clamp(floor((dot(xy, perp) - clo) / max(chi - clo, 1.0) * nstripes), 0.0, nstripes - 1.0);
            let qi = clamp((p - 0.4 * i / nstripes) / 0.6, 0.0, 1.0); let edge = alo - bar + qi * (span + 2.0 * bar); let d = dot(xy, sdir) - edge;
            let c = mix(pixel(0u, vec2<u32>(xy)), pixel(1u, vec2<u32>(xy)), 1.0 - lights_smooth(-1.0, 1.0, d));
            var light = 0.0; if d >= 0.0 && d < bar { light = pow(1.0 - d / bar, 2.0) * lights_smooth(0.0, 0.1, qi) * (1.0 - lights_smooth(0.9, 1.0, qi)); }
            return lights_add(c, color, light * 1.5);
        }
        default: { return lights_base(xy, k); }
    }
}
