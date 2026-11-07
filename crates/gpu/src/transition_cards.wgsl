// Page-peel pointwise kernel. Uses catalog bindings and clamped bilinear sampler.
fn cards_page_inside(xy: vec2<f32>, extent: vec2<f32>) -> bool {
    return all(xy >= vec2(0.0)) && all(xy < extent);
}

fn cards_page_back(c: vec4<f32>, light: f32) -> vec4<f32> {
    return vec4(mix(c.rgb, vec3(c.a), 0.6) * light, c.a);
}

fn cards_page_peel(xy: vec2<f32>, p: f32, corner: u32, radius: f32, shadow: bool, extent: vec2<f32>) -> vec4<f32> {
    var origin = extent;
    switch corner {
        case 0u: { origin = vec2(0.0, 0.0); }
        case 1u: { origin = vec2(extent.x, 0.0); }
        case 3u: { origin = vec2(0.0, extent.y); }
        default: {}
    }
    var normal = extent * 0.5 - origin;
    normal /= max(length(normal), 1e-3);
    let smax = abs(extent.x - 2.0 * origin.x) * abs(normal.x) + abs(extent.y - 2.0 * origin.y) * abs(normal.y);
    let f = -1.0 + p * (smax + radius * 1.2 + 2.0);
    let s = dot(xy - origin, normal);

    if s >= f {
        let so = 2.0 * f - s - 3.141592653589793 * radius;
        let src = xy - normal * (s - so);
        if cards_page_inside(src, extent) {
            return cards_page_back(sample_clamped(0u, src, vec2<u32>(extent)), 0.95);
        }
    }

    let d = f - s;
    if d >= 0.0 && d < radius {
        let phi_back = 3.141592653589793 - asin(d / radius);
        let so_back = f - radius * phi_back;
        let src_back = xy - normal * (s - so_back);
        if cards_page_inside(src_back, extent) {
            return cards_page_back(sample_clamped(0u, src_back, vec2<u32>(extent)), 0.75 + 0.25 * sin(phi_back));
        }
        let phi_front = asin(d / radius);
        let so_front = f - radius * phi_front;
        let src_front = xy - normal * (s - so_front);
        if cards_page_inside(src_front, extent) {
            let light = 1.0 - 0.45 * (phi_front / 1.5707963267948966);
            let c = sample_clamped(0u, src_front, vec2<u32>(extent));
            return vec4(c.rgb * light, c.a);
        }
    }

    if s >= f { return pixel(0u, vec2<u32>(xy)); }
    let b = pixel(1u, vec2<u32>(xy));
    if !shadow { return b; }
    let t_in = clamp(p / 0.05, 0.0, 1.0);
    let smooth_in = t_in * t_in * (3.0 - 2.0 * t_in);
    let t_out = clamp((p - 0.9) / (1.0 - 0.9), 0.0, 1.0);
    let smooth_out = t_out * t_out * (3.0 - 2.0 * t_out);
    let distance = max(d - radius, 0.0);
    let attenuation = exp(-distance / (radius * 1.5));
    let k = 1.0 - 0.45 * attenuation * smooth_in * (1.0 - smooth_out);
    return vec4(b.rgb * k, b.a);
}

fn cards_whip_scene(xy: vec2<f32>, p: f32, direction: u32, extent: vec2<f32>) -> vec4<f32> {
    let s0 = 1.0 / (1.0 + exp(7.0));
    let s1 = 1.0 / (1.0 + exp(-7.0));
    let sp = 1.0 / (1.0 + exp(-(p - 0.5) * 14.0));
    let e = (sp - s0) / (s1 - s0);
    var delta = vec2(1.0, 0.0);
    switch direction {
        case 0u: { delta = vec2(0.0, 1.0); }
        case 1u: { delta = vec2(-1.0, 0.0); }
        case 2u: { delta = vec2(0.0, -1.0); }
        default: {}
    }
    let travel = max(dot(abs(delta), extent), 1.0);
    let local_a = xy - delta * travel * e;
    let local_b = xy - delta * travel * (e - 1.0);
    var a = vec4(0.0);
    var b = vec4(0.0);
    if all(local_a >= vec2(-0.5)) && all(local_a <= extent + vec2(0.5)) {
        let edge = min(min(local_a.x, extent.x - local_a.x), min(local_a.y, extent.y - local_a.y));
        a = sample_clamped(0u, local_a, vec2<u32>(extent)) * clamp(edge + 0.5, 0.0, 1.0);
    }
    if all(local_b >= vec2(-0.5)) && all(local_b <= extent + vec2(0.5)) {
        let edge = min(min(local_b.x, extent.x - local_b.x), min(local_b.y, extent.y - local_b.y));
        b = sample_clamped(1u, local_b, vec2<u32>(extent)) * clamp(edge + 0.5, 0.0, 1.0);
    }
    return b + a * (1.0 - b.a);
}

fn cards_whip(xy: vec2<f32>, p: f32, direction: u32, blur_percent: f32, extent: vec2<f32>) -> vec4<f32> {
    var delta = vec2(1.0, 0.0);
    switch direction {
        case 0u: { delta = vec2(0.0, 1.0); }
        case 1u: { delta = vec2(-1.0, 0.0); }
        case 2u: { delta = vec2(0.0, -1.0); }
        default: {}
    }
    let travel = max(dot(abs(delta), extent), 1.0);
    let bell_t = 4.0 * p * (1.0 - p);
    let bell = bell_t * bell_t * (3.0 - 2.0 * bell_t);
    let len = clamp(blur_percent, 0.0, 100.0) * 0.01 * bell * travel * 0.18;
    if len < 0.75 { return cards_whip_scene(xy, p, direction, extent); }
    let count = clamp(u32(ceil(len)), 1u, 40u);
    var sum = vec4(0.0);
    for (var i = 0u; i < count; i++) {
        let t = f32(i) / f32(max(count - 1u, 1u)) * 2.0 - 1.0;
        let at = clamp(xy + delta * len * t, vec2(0.5), extent - vec2(0.5));
        sum += cards_whip_scene(at, p, direction, extent);
    }
    return sum / f32(count);
}

fn cards_apply(position: vec4<f32>) -> vec4<f32> {
    let extent = vec2<f32>(textureDimensions(outgoing));
    if params.kernel == 95u {
        return cards_whip(position.xy, params.progress, params.direction, params.extra0.x, extent);
    }
    if params.kernel != 94u { return vec4(0.0); }
    return cards_page_peel(position.xy, params.progress, params.direction, params.extra0.x, params.extra0.y > 0.5, extent);
}
