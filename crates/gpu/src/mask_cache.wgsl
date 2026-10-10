// Original FilmCraft mask coverage math; cached as full-precision R32Float.
struct Params {width:u32,height:u32,count:u32,pad:u32};
@group(0) @binding(0) var<uniform> params:Params;
@group(0) @binding(3) var result:texture_storage_2d<r32float,write>;
struct MaskInfo {
    start: u32,
    len: u32,
    mode: u32,
    inverted: u32,
    feather: f32,
    expansion: f32,
    opacity: f32,
    _pad: f32,
};


@group(0) @binding(1) var<storage, read> masks: array<MaskInfo>;
@group(0) @binding(2) var<storage, read> pts: array<vec2<f32>>;
fn falloff(s: f32, feather: f32) -> f32 {
    let w = max(feather, 1.0);
    let u = clamp(s / w + 0.5, 0.0, 1.0);
    let smooth_u = u * u * (3.0 - 2.0 * u);
    return u + (smooth_u - u) * min(feather, 1.0);
}

// MaskMode index: None 0, Add 1, Subtract 2, Intersect 3, Lighten 4, Darken 5, Difference 6.
fn mode_start(mode: u32) -> f32 {
    if (mode == 2u || mode == 3u || mode == 5u) {
        return 1.0;
    }
    return 0.0;
}

fn combine(mode: u32, a: f32, m: f32) -> f32 {
    switch mode {
        case 1u: { return a + m - a * m; }
        case 2u: { return a * (1.0 - m); }
        case 3u: { return a * m; }
        case 4u: { return max(a, m); }
        case 5u: { return min(a, m); }
        case 6u: { return a + m - 2.0 * a * m; }
        default: { return a; }
    }
}

fn seg_dist2(p: vec2<f32>, a: vec2<f32>, b: vec2<f32>) -> f32 {
    let d = b - a;
    let q = p - a;
    let l2 = dot(d, d);
    var t = 0.0;
    if (l2 > 0.0) {
        t = clamp(dot(q, d) / l2, 0.0, 1.0);
    }
    let e = q - d * t;
    return dot(e, e);
}

fn mask_value(m: MaskInfo, p: vec2<f32>) -> f32 {
    let band = abs(m.expansion) + max(m.feather, 1.0) * 0.5 + 1.0;
    var sd = -band;
    if (m.len >= 3u) {
        var wind = 0;
        var d2 = band * band;
        for (var i = 0u; i < m.len; i = i + 1u) {
            let a = pts[m.start + i];
            let c = pts[m.start + (i + 1u) % m.len];
            if ((a.y <= p.y) != (c.y <= p.y)) {
                let t = (p.y - a.y) / (c.y - a.y);
                if (a.x + (c.x - a.x) * t < p.x) {
                    if (c.y > a.y) { wind = wind + 1; } else { wind = wind - 1; }
                }
            }
            d2 = min(d2, seg_dist2(p, a, c));
        }
        let d = sqrt(d2);
        if (wind != 0) { sd = d; } else { sd = -d; }
    }
    let c = falloff(sd + m.expansion, m.feather);
    if (m.inverted != 0u) {
        return m.opacity * (1.0 - c);
    }
    return m.opacity * c;
}


fn coverage(p: vec2<f32>) -> f32 {
    var acc = mode_start(masks[0].mode);
    for (var i = 0u; i < params.count; i++) {
        let m = masks[i];
        acc = clamp(combine(m.mode, acc, mask_value(m, p)), 0.0, 1.0);
    }
    return acc;
}


@compute @workgroup_size(16,16)
fn main(@builtin(global_invocation_id) id:vec3<u32>) {
    if id.x>=params.width || id.y>=params.height {return;}
    textureStore(result,vec2<i32>(id.xy),vec4(coverage(vec2<f32>(id.xy)+0.5),0.0,0.0,0.0));
}
