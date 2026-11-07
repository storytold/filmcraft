// Simple affine and alpha bevel effects; uses the shared fx uniform and image samplers.
fn stylize_apply(p: vec2<i32>) -> vec4<f32> {
    let original = ld(p);
    let q = vec2<f32>(p) + vec2<f32>(0.5);
    if u.i0.x == 128u {
        let uv = vec2<f32>(u.p0.x*q.x + u.p0.z*q.y + u.p1.x,
                           u.p0.y*q.x + u.p0.w*q.y + u.p1.y);
        return sample_bilinear(uv.x, uv.y);
    }
    if u.i0.x == 129u {
        let t = u.p0.x;
        let lx = u.p0.y;
        let ly = u.p0.z;
        let intensity = u.p0.w;
        let gx = sample_clamped(q.x+t,q.y).a - sample_clamped(q.x-t,q.y).a;
        let gy = sample_clamped(q.x,q.y+t).a - sample_clamped(q.x,q.y-t).a;
        let shade = -(gx*lx + gy*ly)*intensity;
        return vec4<f32>(clamp(original.rgb + vec3<f32>(shade*original.a), vec3<f32>(0.0), vec3<f32>(original.a)), original.a);
    }
    return original;
}
