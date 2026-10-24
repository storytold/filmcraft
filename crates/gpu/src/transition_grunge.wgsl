fn grunge_hash(x: i32, y: i32, seed: u32) -> f32 {
    var h = bitcast<u32>(x) * 0x8DA6B343u ^ bitcast<u32>(y) * 0xD8163841u ^ seed * 0xCB1AB31Fu ^ 0x9E3779B9u;
    h ^= h >> 15u; h *= 0x2C1B3C6Du; h ^= h >> 12u; h *= 0x297A2D39u; h ^= h >> 15u;
    return f32(h >> 8u) / 16777216.0;
}
fn grunge_smooth(a: f32, b: f32, x: f32) -> f32 {
    if b <= a { return select(1.0, 0.0, x < a); }
    let t = clamp((x - a) / (b - a), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}
fn grunge_bell(p: f32) -> f32 {
    let t = 4.0 * p * (1.0 - p);
    return t * t * (3.0 - 2.0 * t);
}
fn grunge_vnoise(x: f32, y: f32, seed: u32) -> f32 {
    let cell = floor(vec2(x, y));
    let f = fract(vec2(x, y));
    let u = f * f * (vec2(3.0) - 2.0 * f);
    let i = vec2<i32>(cell);
    let a = grunge_hash(i.x, i.y, seed);
    let b = grunge_hash(i.x + 1, i.y, seed);
    let c = grunge_hash(i.x, i.y + 1, seed);
    let d = grunge_hash(i.x + 1, i.y + 1, seed);
    return mix(mix(a, b, u.x), mix(c, d, u.x), u.y);
}
fn grunge_fbm(x: f32, y: f32, seed: u32) -> f32 {
    var value = 0.0; var amp = 0.5; var freq = 1.0; var norm = 0.0;
    for (var octave = 0u; octave < 4u; octave++) {
        value += grunge_vnoise(x * freq, y * freq, seed + octave * 101u) * amp;
        norm += amp; amp *= 0.5; freq *= 2.03;
    }
    return value / norm;
}
fn grunge_ab(xy: vec2<f32>, k: f32) -> vec4<f32> {
    return mix(sample_clamped(0u, xy, textureDimensions(outgoing)), sample_clamped(1u, xy, textureDimensions(incoming)), k);
}
fn grunge_split(xy: vec2<f32>, d: f32, k: f32) -> vec4<f32> {
    let center = grunge_ab(xy, k);
    if abs(d) < 0.25 { return center; }
    let red = grunge_ab(xy + vec2(d, 0.0), k);
    let blue = grunge_ab(xy - vec2(d, 0.0), k);
    return vec4(red.r, center.g, blue.b, max(center.a, max(red.a, blue.a)));
}
fn grunge_apply(position: vec4<f32>) -> vec4<f32> {
    let p = params.progress;
    let xy = position.xy;
    let extent = vec2<f32>(textureDimensions(outgoing));
    let env = grunge_bell(p);
    let k = grunge_smooth(0.3, 0.7, p);
    let seed = params.direction;
    let f0 = params.extra0.x;
    let f1 = params.extra0.y;
    let f2 = params.extra0.z;
    let f3 = params.extra0.w;
    let color0 = params.extra1;
    let color1 = params.extra2;
    switch params.kernel {
        case 80u: {
            let amount = f0;
            let frame = i32(floor(p * 30.0));
            let gx = floor(xy.x / (extent.x / 8.0)); let gy = floor(xy.y / (extent.y / 6.0));
            let fx = floor(xy.x / (extent.x / 23.0)); let fy = floor(xy.y / (extent.y / 13.0));
            let r = grunge_hash(i32(gx), i32(gy), seed + bitcast<u32>(frame));
            let fine = grunge_hash(i32(fx), i32(fy), seed + bitcast<u32>(frame * 7 + 1));
            var ox = 0.0; var oy = 0.0;
            if r > 0.55 { ox = (grunge_hash(i32(gx), i32(gy), seed + 99u) - 0.5) * amount * env * extent.x * 0.15; oy = (grunge_hash(i32(gx), i32(gy), seed + 7u) - 0.5) * amount * env * extent.x * 0.045; }
            var kk = select(0.0, 1.0, fine < k);
            kk = mix(kk, k, 1.0 - env);
            return grunge_split(xy - vec2(ox, oy), (fine - 0.5) * amount * env * 30.0 * max(extent.y / 1080.0, 0.01), kk);
        }
        case 81u: {
            let shake = f0;
            let n = params.extra0.z;
            let shutter = f1;
            var sum = vec4(0.0);
            for (var i = 0u; i < u32(n); i++) {
                var q = p;
                if n > 1.0 { q = clamp(p + (f32(i) / (n - 1.0) - 0.5) * shutter, 0.0, 1.0); }
                let e = grunge_bell(q);
                let ox = (grunge_vnoise(q * 45.0, 0.5, seed) - 0.5) * 2.0 * shake * e;
                let oy = (grunge_vnoise(q * 45.0, 7.5, seed + 1u) - 0.5) * 2.0 * shake * e;
                let rot = (grunge_vnoise(q * 30.0, 3.5, seed + 2u) - 0.5) * 0.04 * shake / max(extent.x, 1.0) * 10.0 * e;
                let zoom = 1.0 + 0.08 * e;
                let d = (xy - extent * 0.5) / zoom;
                let cs = cos(rot); let sn = sin(rot);
                let src = vec2(d.x * cs - d.y * sn, d.x * sn + d.y * cs) + extent * 0.5 - vec2(ox, oy);
                sum += grunge_ab(src, grunge_smooth(0.3, 0.7, q));
            }
            return sum / n;
        }
        case 82u: {
            let cell = max(f0, 4.0); let refr = f1; let unit = max(extent.y / 1080.0, 0.01);
            let grid = vec2<i32>(floor(xy / cell));
            var best = 3.402823e+38; var second = 3.402823e+38; var id = vec2<i32>(0);
            for (var j = -1; j <= 1; j++) { for (var i = -1; i <= 1; i++) {
                let c = grid + vec2(i, j);
                let point = (vec2<f32>(c) + vec2(grunge_hash(c.x, c.y, seed), grunge_hash(c.x, c.y, seed + 1u))) * cell;
                let dist = length(xy - point);
                if dist < best { second = best; best = dist; id = c; } else if dist < second { second = dist; }
            } }
            let tau = 0.3 + 0.4 * grunge_hash(id.x, id.y, seed + 2u);
            let kk = grunge_smooth(tau - 0.1, tau + 0.1, p);
            let angle = grunge_hash(id.x, id.y, seed + 3u) * 6.28318530718;
            let off = refr * env * grunge_hash(id.x, id.y, seed + 4u);
            let c = grunge_ab(xy + vec2(cos(angle), sin(angle)) * off, kk);
            let edge = exp(-(second - best) / (1.5 * max(unit, 0.5))) * env;
            let facet = 1.0 + (grunge_hash(id.x, id.y, seed + 5u) - 0.5) * 0.4 * env;
            let lit = c.rgb * facet + vec3(edge * 0.8);
            return vec4(lit, min(c.a + max(edge * 0.8, 0.0), 1.0));
        }
        case 83u: {
            let amount = f0; let frame = u32(floor(p * 24.0));
            let band_h = extent.y / (6.0 + 18.0 * grunge_hash(i32(frame), 0, seed));
            let band = i32(floor(xy.y / max(band_h, 1.0)));
            let r = grunge_hash(band, i32(frame), seed);
            var ox = 0.0;
            if r > 0.6 { ox = (grunge_hash(band, i32(frame), seed + 1u) - 0.5) * amount * env * extent.x * 0.25; }
            let tau = 0.4 + 0.2 * grunge_hash(band, 9, seed);
            var kk = select(0.0, 1.0, p > tau); kk = kk * env + k * (1.0 - env);
            let c = grunge_split(xy - vec2(ox, 0.0), amount * env * 18.0 * max(r - 0.3, 0.0) * 2.0 * max(extent.y / 1080.0, 0.01), kk);
            let bx = i32(floor(xy.x / (24.0 * max(extent.y / 1080.0, 0.2)))); let by = i32(floor(xy.y / (12.0 * max(extent.y / 1080.0, 0.2))));
            if grunge_hash(bx, by, seed + frame * 3u) > 1.0 - 0.04 * amount * env { let v = grunge_hash(bx, by, seed + 77u); return mix(c, vec4(v, 1.0 - v, v * 0.5, 1.0), 0.8); }
            return c;
        }
        case 84u: {
            let scale = max(f0, 4.0); let feather = f1; let border = f2; let edge_color = color0;
            var lo = 3.402823e+38; var hi = -3.402823e+38;
            for (var j = 0u; j <= 48u; j++) { for (var i = 0u; i <= 48u; i++) {
                let x = extent.x * f32(i) / 48.0; let y = extent.y * f32(j) / 48.0;
                let field = grunge_fbm(x / scale, y / scale, seed) * 0.8 + grunge_vnoise(x / (scale * 0.15), y / (scale * 0.15), seed + 9u) * 0.2;
                lo = min(lo, field); hi = max(hi, field);
            } }
            lo *= scale * 3.0; hi *= scale * 3.0;
            let margin = border + feather + 3.0 + (hi - lo) * 0.01;
            let threshold = lo - margin + p * (hi - lo + 2.0 * margin);
            let field = (grunge_fbm(xy.x / scale, xy.y / scale, seed) * 0.8 + grunge_vnoise(xy.x / (scale * 0.15), xy.y / (scale * 0.15), seed + 9u) * 0.2) * scale * 3.0;
            let d = field - threshold; let s = (feather + 1.0) * 0.5;
            let kb = 1.0 - grunge_smooth(-s, s, d);
            let ko = max(1.0 - grunge_smooth(border - s, border + s, d), kb);
            return pixel(0u, vec2<u32>(xy)) * (1.0 - ko) + edge_color * (ko - kb) + pixel(1u, vec2<u32>(xy)) * kb;
        }
        case 85u: {
            let line = max(f0, 1.0); let glow = color0; var q = 0.0; var side = 0u;
            if p < 0.5 { q = p * 2.0; } else { q = (1.0 - p) * 2.0; side = 1u; }
            let sy = 1.0 - grunge_smooth(0.0, 0.65, q); let sx = 1.0 - grunge_smooth(0.6, 1.0, q);
            let hy = max(extent.y * 0.5 * sy, line * 0.5 * sx); let hx = extent.x * 0.5 * sx;
            let white = grunge_smooth(0.2, 0.7, q); let r = xy - extent * 0.5;
            var c = vec4(0.0, 0.0, 0.0, 1.0);
            if abs(r.x) <= hx && abs(r.y) <= hy && hx > 0.0 {
                let src = extent * 0.5 + vec2(r.x / max(sx, 1e-3), clamp(r.y / max(sy, 1e-3), 0.0, extent.y));
                c = grunge_ab(src, 0.0);
                let from_side = sample_clamped(side, src, textureDimensions(outgoing));
                c = vec4(mix(from_side.rgb, glow.rgb, white), 1.0);
            }
            let dy = max(abs(r.y) - hy, 0.0); let dx = max(abs(r.x) - hx, 0.0);
            let halo = exp(-length(vec2(dx, dy)) / (line * 3.0)) * white * 0.8;
            return vec4(c.rgb + glow.rgb * halo, min(c.a + halo, 1.0));
        }
        case 86u: {
            let amount = f0; let frame = u32(floor(p * 30.0)); let band_y = extent.y * (1.2 - 1.4 * p);
            let unit = max(extent.y / 1080.0, 0.01); let row = i32(floor(xy.y / (2.0 * max(unit, 0.5))));
            let jitter = (grunge_hash(row, i32(frame), seed) - 0.5) * amount * env * extent.x * 0.02;
            let in_band = exp(-pow((xy.y - band_y) / (extent.y * 0.06), 2.0));
            let ox = jitter + in_band * amount * env * extent.x * 0.08 * (grunge_vnoise(xy.y * 0.05, p * 20.0, seed) - 0.5) * 2.0;
            let c = grunge_split(xy - vec2(ox, 0.0), amount * env * 10.0 * unit, k);
            let grain = (grunge_hash(i32(xy.x), i32(xy.y), seed + frame) - 0.5) * 0.25 * amount * env;
            let band_noise = in_band * env * amount * 0.5 * grunge_hash(i32(xy.x) / 3, row, frame);
            let l = dot(c.rgb, vec3(0.2126, 0.7152, 0.0722)); let desat = mix(c.rgb, vec3(l), 0.4 * amount * env);
            return vec4(desat + vec3(grain + band_noise), c.a);
        }
        case 104u: {
            let count = params.extra0.x;
            let step = i32(floor(p * count * 2.0));
            let show_b = grunge_hash(step, 3, seed) < grunge_smooth(0.1, 0.9, p);
            let gain = 1.0 + (grunge_hash(step, 5, seed) - 0.5) * 1.2 * env;
            var c = textureLoad(outgoing, vec2<i32>(xy), 0);
            if show_b { c = textureLoad(incoming, vec2<i32>(xy), 0); }
            return vec4(c.rgb * gain, c.a);
        }
        case 105u: {
            let segments = params.extra0.x;
            let zoom = 1.0 + params.extra0.y * env;
            let angle = 6.28318530718 / segments;
            let rxy = xy - extent * 0.5;
            let radius = length(rxy) / zoom;
            let theta = atan2(rxy.y, rxy.x) + p * 1.57079632679;
            let a = abs((theta - floor(theta / angle) * angle) - angle * 0.5);
            let kaleido = grunge_ab(extent * 0.5 + radius * vec2(cos(a), sin(a)), k);
            return mix(grunge_ab(xy, k), kaleido, grunge_smooth(0.0, 0.6, env));
        }
        case 106u: {
            let amount = params.extra0.x;
            let scale = max(params.extra0.y, 4.0);
            let ox = (grunge_fbm(xy.x / scale + p * 2.0, xy.y / scale, seed) - 0.5) * 2.0 * amount * env;
            let oy = (grunge_fbm(xy.x / scale, xy.y / scale - p * 2.0, seed + 5u) - 0.5) * 2.0 * amount * env;
            return grunge_ab(xy + vec2(ox, oy), k);
        }
        default: { return vec4(0.0); }
    }
}
