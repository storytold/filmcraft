struct Params { image: vec4<f32>, direction: vec4<f32> }
@group(0) @binding(0) var a: texture_2d<f32>;
@group(0) @binding(1) var b: texture_2d<f32>;
@group(0) @binding(2) var<uniform> params: Params;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let p = array<vec2<f32>, 3>(vec2(-1.0,-1.0), vec2(3.0,-1.0), vec2(-1.0,3.0));
    return vec4(p[i],0.0,1.0);
}
fn pixel(side: u32, at: vec2<u32>) -> vec4<f32> {
    if side == 0u { return textureLoad(a,vec2<i32>(at),0); }
    return textureLoad(b,vec2<i32>(at),0);
}
fn sample_clamped(side: u32, at: vec2<f32>) -> vec4<f32> {
    let f = clamp(at-vec2(0.5), vec2(0.0), params.image.xy-vec2(1.0));
    let lo = vec2<u32>(floor(f));
    let hi = min(lo+vec2(1u), vec2<u32>(params.image.xy)-vec2(1u));
    let k = f-vec2<f32>(lo);
    let va = pixel(side,lo); let vb = pixel(side,vec2(hi.x,lo.y));
    let vc = pixel(side,vec2(lo.x,hi.y)); let vd = pixel(side,hi);
    let top = va+(vb-va)*k.x; let bot = vc+(vd-vc)*k.x;
    return top+(bot-top)*k.y;
}
fn card(side: u32, offset: vec2<f32>, xy: vec2<f32>) -> vec4<f32> {
    // CPU Card::hit order preserves rounding at moving edges.
    let eye = vec3(params.image.xy/2.0, -max(params.image.x,params.image.y)*6.0);
    let ex = vec3(1.0,0.0,0.0); let ey = vec3(0.0,1.0,0.0);
    let d = vec3(xy,0.0)-eye; let rhs = eye-vec3(offset,0.0); let nd = -d;
    let det = dot(ex,cross(ey,nd));
    let local = vec2(dot(rhs,cross(ey,nd)),dot(ex,cross(rhs,nd)))/det;
    let edge = min(min(local.x,params.image.x-local.x),min(local.y,params.image.y-local.y));
    let coverage = clamp(edge+0.5,0.0,1.0);
    if coverage <= 0.0 { return vec4(0.0); }
    return sample_clamped(side,local)*coverage;
}
fn eased(q: f32) -> f32 {
    if q < 0.5 { return 4.0*q*q*q; }
    let v = -2.0*q+2.0;
    return 1.0-v*v*v/2.0;
}
@fragment
fn fs(@builtin(position) at: vec4<f32>) -> @location(0) vec4<f32> {
    let p = params.image.z;
    if p <= 0.0 { return pixel(0u,vec2<u32>(at.xy)); }
    if p >= 1.0 { return pixel(1u,vec2<u32>(at.xy)); }
    var direction = vec2(0.0,1.0);
    switch u32(params.direction.x) { case 1u: { direction=vec2(-1.0,0.0); } case 2u: { direction=vec2(0.0,-1.0); } case 3u: { direction=vec2(1.0,0.0); } default: {} }
    let travel = max(dot(abs(direction),params.image.xy),1.0);
    // CPU shutter: 12 taps at full blur, 0.12 timeline fraction exposure.
    let blur = params.image.w; var count=1u;
    if blur > 0.0 { count=clamp(u32(ceil(blur*12.0)),2u,12u); }
    var sum=vec4(0.0);
    for(var s=0u;s<count;s++) {
        var q=p;
        if count>1u { q=clamp(p+(f32(s)/f32(count-1u)-0.5)*(0.12*blur),0.0,1.0); }
        let e=eased(q);
        let va=card(0u,direction*travel*e,at.xy);
        let vb=card(1u,direction*travel*(e-1.0),at.xy);
        sum+=vb+va*(1.0-vb.a);
    }
    return sum*(1.0/f32(count));
}
