// Spatial family fragment for inclusion in fx.wgsl. Opcodes 64..74 are reserved by the family.

fn spatial_round_rect_sdf(p: vec2<f32>, bounds: vec4<f32>, radius: f32) -> f32 {
    let center = (bounds.xy + bounds.zw) * 0.5;
    let half_size = (bounds.zw - bounds.xy) * 0.5 - vec2(radius);
    let q = abs(p - center) - half_size;
    return length(max(q, vec2(0.0))) + min(max(q.x, q.y), 0.0) - radius;
}

fn spatial_mosaic_texel(tile: vec2<i32>, bw: f32, bh: f32, grid: vec2<i32>) -> vec4<f32> {
    let q = clamp(tile, vec2(0), grid - vec2(1));
    let pixel = (vec2<f32>(q) + vec2(0.5)) * vec2(bw, bh);
    return sample_clamped(pixel.x, pixel.y);
}

fn spatial_mosaic_sample(tile_position: vec2<f32>, bw: f32, bh: f32, grid: vec2<i32>) -> vec4<f32> {
    let f = clamp(tile_position - vec2(0.5), vec2(0.0), vec2<f32>(grid - vec2(1)));
    let i = vec2<i32>(floor(f));
    let t = f - vec2<f32>(i);
    let a = spatial_mosaic_texel(i, bw, bh, grid);
    let b = spatial_mosaic_texel(i + vec2(1, 0), bw, bh, grid);
    let c = spatial_mosaic_texel(i + vec2(0, 1), bw, bh, grid);
    let d = spatial_mosaic_texel(i + vec2(1, 1), bw, bh, grid);
    return mix(mix(a, b, t.x), mix(c, d, t.x), t.y);
}

fn spatial_blend(mode: u32, base: vec3<f32>, sample: vec3<f32>) -> vec3<f32> {
    switch mode {
        case 1u: { return base + sample; }
        case 2u: { return vec3(1.0) - (vec3(1.0) - min(base, vec3(1.0))) * (vec3(1.0) - min(sample, vec3(1.0))); }
        case 3u: { return base * sample; }
        case 4u: {
            var out: vec3<f32>;
            for (var c=0u;c<3u;c=c+1u) {
                if base[c] < 0.5 { out[c] = 2.0 * base[c] * sample[c]; }
                else { out[c] = 1.0 - 2.0 * (1.0-min(base[c],1.0)) * (1.0-min(sample[c],1.0)); }
            }
            return out;
        }
        default: { return sample; }
    }
}

fn spatial_i64(x: i32) -> vec2<u32> {
    return vec2(bitcast<u32>(x), select(0u, 0xffffffffu, x < 0));
}

fn spatial_hash_u(x: i32, y: i32, z: i32, seed: vec2<u32>) -> vec2<u32> {
    var h = noise_mul64(spatial_i64(x), vec2(0x7f4a7c15u,0x9e3779b9u));
    h = h ^ noise_mul64(spatial_i64(y), vec2(0x27d4eb4fu,0xc2b2ae3du));
    h = h ^ noise_mul64(spatial_i64(z), vec2(0x9e3779f9u,0x165667b1u));
    h = h ^ noise_mul64(seed, vec2(0x165667c5u,0x27d4eb2fu));
    h = h ^ noise_shr64(h,31u);
    h = noise_mul64(h,vec2(0x1ce4e5b9u,0xbf58476du));
    h = h ^ noise_shr64(h,29u);
    h = noise_mul64(h,vec2(0x133111ebu,0x94d049bbu));
    h = h ^ vec2(h.y,0u);
    return h;
}

fn spatial_grad3(h: vec2<u32>, p: vec3<f32>) -> f32 {
    let r = ((h.x % 12u) + ((h.y % 12u) * 4u)) % 12u;
    switch r {
        case 0u:{return p.x+p.y;} case 1u:{return -p.x+p.y;}
        case 2u:{return p.x-p.y;} case 3u:{return -p.x-p.y;}
        case 4u:{return p.x+p.z;} case 5u:{return -p.x+p.z;}
        case 6u:{return p.x-p.z;} case 7u:{return -p.x-p.z;}
        case 8u:{return p.y+p.z;} case 9u:{return -p.y+p.z;}
        case 10u:{return p.y-p.z;} default:{return -p.y-p.z;}
    }
}

fn spatial_fade(t:f32)->f32 { return t*t*t*(t*(t*6.0-15.0)+10.0); }

fn spatial_noise3(x:f32,y:f32,z:f32,seed:vec2<u32>)->f32 {
    let floor_p=floor(vec3(x,y,z));
    let cell=vec3<i32>(floor_p);
    let f=vec3(x,y,z)-floor_p;
    let w=vec3(spatial_fade(f.x),spatial_fade(f.y),spatial_fade(f.z));
    let g000=spatial_grad3(spatial_hash_u(cell.x,cell.y,cell.z,seed),f);
    let g100=spatial_grad3(spatial_hash_u(cell.x+1,cell.y,cell.z,seed),f-vec3(1.0,0.0,0.0));
    let g010=spatial_grad3(spatial_hash_u(cell.x,cell.y+1,cell.z,seed),f-vec3(0.0,1.0,0.0));
    let g110=spatial_grad3(spatial_hash_u(cell.x+1,cell.y+1,cell.z,seed),f-vec3(1.0,1.0,0.0));
    let g001=spatial_grad3(spatial_hash_u(cell.x,cell.y,cell.z+1,seed),f-vec3(0.0,0.0,1.0));
    let g101=spatial_grad3(spatial_hash_u(cell.x+1,cell.y,cell.z+1,seed),f-vec3(1.0,0.0,1.0));
    let g011=spatial_grad3(spatial_hash_u(cell.x,cell.y+1,cell.z+1,seed),f-vec3(0.0,1.0,1.0));
    let g111=spatial_grad3(spatial_hash_u(cell.x+1,cell.y+1,cell.z+1,seed),f-vec3(1.0));
    let x00=g000+(g100-g000)*w.x;
    let x10=g010+(g110-g010)*w.x;
    let x01=g001+(g101-g001)*w.x;
    let x11=g011+(g111-g011)*w.x;
    let y0=x00+(x10-x00)*w.y;
    let y1=x01+(x11-x01)*w.y;
    return (y0+(y1-y0)*w.z)*0.9;
}

fn spatial_fbm(x:f32,y:f32,z:f32,octaves:f32,seed:vec2<u32>)->f32 {
    let n=clamp(octaves,1.0,10.0);
    var sum=0.0;var amp=1.0;var freq=1.0;var norm=0.0;var i=0u;
    loop {
        if f32(i)>=n { break; }
        let k=min(n-f32(i),1.0);
        let s=noise_add64(seed,vec2(i,0u));
        sum=sum+spatial_noise3(x*freq,y*freq,z*freq+f32(i)*17.13,s)*amp*k;
        norm=norm+amp*k;amp=amp*0.5;freq=freq*2.0;i=i+1u;
    }
    return sum/max(norm,1e-6);
}

fn spatial_smoothstep(e0:f32,e1:f32,x:f32)->f32 {
    let t=clamp((x-e0)/max(e1-e0,1e-6),0.0,1.0);
    return t*t*(3.0-2.0*t);
}

fn spatial_turbulent_n(x:f32,y:f32,ch:f32,ex:f32,ey:f32,ez:f32,complexity:f32,seed:vec2<u32>)->f32 {
    return spatial_fbm(x+ex+ch*31.7,y+ey,ez+ch*3.1,complexity,seed);
}

fn spatial_turbulent(p:vec2<i32>,q:vec2<f32>)->vec4<f32> {
    let kind=u.i1.x;
    let amount=u.p0.x;let noise_size=u.p0.y;let ox=u.p0.z;let oy=u.p0.w;
    if abs(amount)<1e-4 { return ld(p); }
    let complexity=u.p1.x;let ex=u.p1.y;let ey=u.p1.z;let ez=u.p1.w;
    let seed=vec2(u.i1.y,u.i1.z);
    let pin=u.i1.w;
    let uv=q/noise_size+vec2(ox,oy);
    var d:vec2<f32>;
    if kind==0u||kind==3u {
        d=vec2(spatial_turbulent_n(uv.x,uv.y,0.0,ex,ey,ez,complexity,seed),spatial_turbulent_n(uv.x,uv.y,1.0,ex,ey,ez,complexity,seed));
    } else if kind==1u||kind==4u||kind==2u||kind==5u {
        let eps=0.05;
        let gx=(spatial_turbulent_n(uv.x+eps,uv.y,0.0,ex,ey,ez,complexity,seed)-spatial_turbulent_n(uv.x-eps,uv.y,0.0,ex,ey,ez,complexity,seed))/(2.0*eps);
        let gy=(spatial_turbulent_n(uv.x,uv.y+eps,0.0,ex,ey,ez,complexity,seed)-spatial_turbulent_n(uv.x,uv.y-eps,0.0,ex,ey,ez,complexity,seed))/(2.0*eps);
        if kind==1u||kind==4u { d=vec2(gx*0.3,gy*0.3); } else { d=vec2(-gy*0.3,gx*0.3); }
    } else if kind==6u { d=vec2(0.0,spatial_turbulent_n(uv.x,uv.y,0.0,ex,ey,ez,complexity,seed)); }
    else if kind==7u { d=vec2(spatial_turbulent_n(uv.x,uv.y,0.0,ex,ey,ez,complexity,seed),0.0); }
    else { let v=spatial_turbulent_n(uv.x,uv.y,0.0,ex,ey,ez,complexity,seed);d=vec2(v,v); }
    let wh=vec2<f32>(size());
    var kx=1.0;var ky=1.0;
    if pin==1u {
        let k=min(spatial_smoothstep(1.0,max(wh.x*0.1,2.0),min(q.x,wh.x-q.x)),spatial_smoothstep(1.0,max(wh.y*0.1,2.0),min(q.y,wh.y-q.y)));
        kx=k;ky=k;
    } else if pin==2u {
        let k=spatial_smoothstep(1.0,max(wh.x*0.1,2.0),min(q.x,wh.x-q.x));kx=k;ky=k;
    } else if pin==3u {
        let k=spatial_smoothstep(1.0,max(wh.y*0.1,2.0),min(q.y,wh.y-q.y));kx=k;ky=k;
    }
    return sample_bilinear(q.x-d.x*amount*kx,q.y-d.y*amount*ky);
}

fn spatial_vr_dir(lon: f32, lat: f32) -> vec3<f32> {
    return vec3(cos(lat) * sin(lon), sin(lat), cos(lat) * cos(lon));
}

fn spatial_vr_sample_view(local_pixel: vec2<f32>, origin: vec2<i32>, view_size: vec2<i32>) -> vec4<f32> {
    let fx = local_pixel.x - 0.5;
    let fy = clamp(local_pixel.y - 0.5, 0.0, f32(view_size.y - 1));
    let x0 = i32(floor(fx));
    let y0 = i32(floor(fy));
    let tx = fx - f32(x0);
    let ty = fy - f32(y0);
    let wx0 = ((x0 % view_size.x) + view_size.x) % view_size.x;
    let wx1 = (((x0 + 1) % view_size.x) + view_size.x) % view_size.x;
    let y1 = min(y0 + 1, view_size.y - 1);
    let a = textureLoad(src, origin + vec2(wx0, y0), 0);
    let b = textureLoad(src, origin + vec2(wx1, y0), 0);
    let c = textureLoad(src, origin + vec2(wx0, y1), 0);
    let d = textureLoad(src, origin + vec2(wx1, y1), 0);
    return mix(mix(a, b, tx), mix(c, d, tx), ty);
}

fn spatial_vr_sample_clamped_view(local_pixel: vec2<f32>, origin: vec2<i32>, view_size: vec2<i32>) -> vec4<f32> {
    let f = clamp(local_pixel - vec2(0.5), vec2(0.0), vec2<f32>(view_size - vec2(1)));
    let p0 = vec2<i32>(floor(f));
    let p1 = min(p0 + vec2(1), view_size - vec2(1));
    let t = f - vec2<f32>(p0);
    let a = textureLoad(src, origin + p0, 0);
    let b = textureLoad(src, origin + vec2(p1.x, p0.y), 0);
    let c = textureLoad(src, origin + vec2(p0.x, p1.y), 0);
    let d = textureLoad(src, origin + p1, 0);
    return mix(mix(a, b, t.x), mix(c, d, t.x), t.y);
}

fn spatial_vr_chromatic(p: vec2<i32>, original: vec4<f32>) -> vec4<f32> {
    if abs(u.p0.x) < 1e-4 && abs(u.p0.y) < 1e-4 && abs(u.p0.z) < 1e-4 { return original; }
    let dims = size();
    var origin = vec2<i32>(0);
    var local = p;
    var view_size = dims;
    if u.i1.x == 1u && dims.y >= 2 {
        let split = dims.y / 2;
        if p.y >= split { origin.y = split; view_size.y = dims.y - split; local.y -= split; }
        else { view_size.y = split; }
    } else if u.i1.x == 2u && dims.x >= 2 {
        let split = dims.x / 2;
        if p.x >= split { origin.x = split; view_size.x = dims.x - split; local.x -= split; }
        else { view_size.x = split; }
    }
    let pi = 3.141592653589793;
    let lon = (f32(local.x) + 0.5) / f32(view_size.x) * 2.0 * pi - pi;
    let lat = (0.5 - (f32(local.y) + 0.5) / f32(view_size.y)) * pi;
    let direction = spatial_vr_dir(lon, lat);
    let center = spatial_vr_dir(u.p1.x, u.p1.y);
    let theta = acos(clamp(dot(direction, center), -1.0, 1.0));
    var falloff = pow(theta / pi, u.p1.z);
    if u.i1.y != 0u { falloff = 1.0 - falloff; }
    let axis = cross(center, direction);
    let axis_len = length(axis);
    if axis_len <= 1e-9 { return original; }
    let k = axis / axis_len;
    let kd = cross(k, direction);
    let kdot = dot(k, direction);
    var out = original;
    for (var channel = 0u; channel < 3u; channel += 1u) {
        let angle = u.p0[channel] / 100.0 * 0.15 * falloff;
        let s = sin(-angle);
        let c = cos(-angle);
        let rotated = direction * c + kd * s + k * kdot * (1.0 - c);
        let sample_lon = atan2(rotated.x, rotated.z);
        let sample_lat = atan2(rotated.y, length(vec2(rotated.x, rotated.z)));
        let sx = (sample_lon / (2.0 * pi) + 0.5) * f32(view_size.x);
        let sy = (0.5 - sample_lat / pi) * f32(view_size.y);
        out[channel] = spatial_vr_sample_view(vec2(sx, sy), origin, view_size)[channel];
    }
    return out;
}

fn spatial_vr_color_gradients(p: vec2<i32>, original: vec4<f32>) -> vec4<f32> {
    let dims = size();
    var local = p;
    var view_size = dims;
    if u.i1.x == 1u && dims.y >= 2 {
        let split = dims.y / 2;
        if p.y >= split { view_size.y = dims.y - split; local.y -= split; }
        else { view_size.y = split; }
    } else if u.i1.x == 2u && dims.x >= 2 {
        let split = dims.x / 2;
        if p.x >= split { view_size.x = dims.x - split; local.x -= split; }
        else { view_size.x = split; }
    }
    let pi = 3.141592653589793;
    let lon = (f32(local.x) + 0.5) / f32(view_size.x) * 2.0 * pi - pi;
    let lat = (0.5 - (f32(local.y) + 0.5) / f32(view_size.y)) * pi;
    let direction = spatial_vr_dir(lon, lat);
    let d0 = spatial_vr_dir(u.p3.x, u.p3.y);
    let d1 = spatial_vr_dir(u.p3.z, u.p3.w);
    let d2 = spatial_vr_dir(u.p4.x, u.p4.y);
    let a0 = max(acos(clamp(dot(direction, d0), -1.0, 1.0)), 1e-3);
    let a1 = max(acos(clamp(dot(direction, d1), -1.0, 1.0)), 1e-3);
    let a2 = max(acos(clamp(dot(direction, d2), -1.0, 1.0)), 1e-3);
    let w0 = 1.0 / pow(a0, u.p4.z);
    let w1 = 1.0 / pow(a1, u.p4.z);
    let w2 = 1.0 / pow(a2, u.p4.z);
    let gradient = (u.p0.xyz * w0 + u.p1.xyz * w1 + u.p2.xyz * w2) / (w0 + w1 + w2);
    var base = vec3(0.0);
    if original.a > 1e-6 { base = original.rgb / original.a; }
    let blended = spatial_blend(u.i1.y, base, gradient);
    let rgb = base + (blended - base) * u.p4.w;
    let alpha = original.a + (1.0 - original.a) * u.p4.w;
    return vec4(rgb * alpha, alpha);
}

fn spatial_vr_projection(p: vec2<i32>, original: vec4<f32>) -> vec4<f32> {
    if u.i1.y != 0u { return original; }
    let dims = size();
    var origin = vec2<i32>(0);
    var local = p;
    var view_size = dims;
    if u.i1.x == 1u && dims.y >= 2 {
        let split = dims.y / 2;
        if p.y >= split { origin.y = split; view_size.y = dims.y - split; local.y -= split; }
        else { view_size.y = split; }
    } else if u.i1.x == 2u && dims.x >= 2 {
        let split = dims.x / 2;
        if p.x >= split { origin.x = split; view_size.x = dims.x - split; local.x -= split; }
        else { view_size.x = split; }
    }
    let pi = 3.141592653589793;
    let lon = (f32(local.x) + 0.5) / f32(view_size.x) * 2.0 * pi - pi;
    let lat = (0.5 - (f32(local.y) + 0.5) / f32(view_size.y)) * pi;
    let direction = spatial_vr_dir(lon * u.p2.y, lat * u.p2.z);
    let rotated = vec3(
        dot(u.p0.xyz, direction),
        dot(vec3(u.p0.w, u.p1.x, u.p1.y), direction),
        dot(vec3(u.p1.z, u.p1.w, u.p2.x), direction),
    );
    let sample_lon = atan2(rotated.x, rotated.z);
    let sample_lat = atan2(rotated.y, length(vec2(rotated.x, rotated.z)));
    let sx = (sample_lon / (2.0 * pi) + 0.5) * f32(view_size.x);
    let sy = (0.5 - sample_lat / pi) * f32(view_size.y);
    return spatial_vr_sample_view(vec2(sx, sy), origin, view_size);
}

fn spatial_vr_plane_to_sphere(p: vec2<i32>) -> vec4<f32> {
    let dims = size();
    var origin = vec2<i32>(0);
    var local = p;
    var view_size = dims;
    if u.i1.x == 1u && dims.y >= 2 {
        let split = dims.y / 2;
        if p.y >= split { origin.y = split; view_size.y = dims.y - split; local.y -= split; }
        else { view_size.y = split; }
    } else if u.i1.x == 2u && dims.x >= 2 {
        let split = dims.x / 2;
        if p.x >= split { origin.x = split; view_size.x = dims.x - split; local.x -= split; }
        else { view_size.x = split; }
    }
    let pi = 3.141592653589793;
    let lon = (f32(local.x) + 0.5) / f32(view_size.x) * 2.0 * pi - pi;
    let lat = (0.5 - (f32(local.y) + 0.5) / f32(view_size.y)) * pi;
    let direction = spatial_vr_dir(lon, lat);
    let rotated = vec3(
        dot(u.p0.xyz, direction),
        dot(vec3(u.p0.w, u.p1.x, u.p1.y), direction),
        dot(vec3(u.p1.z, u.p1.w, u.p2.x), direction),
    );
    if rotated.z <= 1e-6 { return vec4(0.0); }
    let tx = u.p2.w;
    let ty = tx * f32(view_size.y) / f32(view_size.x);
    let uv = vec2(rotated.x / rotated.z / tx, -rotated.y / rotated.z / ty);
    if abs(uv.x) > 1.0 || abs(uv.y) > 1.0 { return vec4(0.0); }
    var out = spatial_vr_sample_clamped_view((uv + vec2(1.0)) * 0.5 * vec2<f32>(view_size), origin, view_size);
    if u.p3.x > 0.0 {
        let edge = 1.0 - max(abs(uv.x), abs(uv.y));
        out *= spatial_smoothstep(0.0, u.p3.x, edge);
    }
    return out;
}

fn spatial_apply(p: vec2<i32>) -> vec4<f32> {
    let original = ld(p);
    let q = vec2<f32>(p) + vec2(0.5);
    let wh = vec2<f32>(size());
    switch u.i0.x {
        case 71u: { return spatial_turbulent(p,q); }
        case 64u: {
            let bw = max(wh.x / u.p0.x, 1.0);
            let bh = max(wh.y / u.p0.y, 1.0);
            let grid = vec2<i32>(u.i1.yz);
            let tile = vec2<i32>(vec2<f32>(p) / vec2(bw, bh));
            let sharp = spatial_mosaic_texel(tile, bw, bh, grid);
            let smooth_sample = spatial_mosaic_sample(q / vec2(bw, bh), bw, bh, grid);
            return sharp + (smooth_sample - sharp) * u.p0.z;
        }
        case 65u: {
            let n = f32(u.i1.x);
            let uv = vec2<f32>((f32(p.x * i32(u.i1.x) % size().x) + 0.5) + (n - 1.0) * 0.5 / n,
                               f32(p.y * i32(u.i1.x) % size().y) + 0.5);
            return sample_clamped(uv.x, uv.y);
        }
        case 66u: {
            let d = q - u.p0.xy;
            let radius = u.p0.w;
            let dist = length(d);
            if dist >= radius || radius <= 0.0 { return original; }
            let a = u.p0.z * (1.0 - dist / radius) * (1.0 - dist / radius);
            let c = cos(a); let s = sin(a);
            return sample_bilinear(u.p0.x + d.x * c - d.y * s, u.p0.y + d.x * s + d.y * c);
        }
        case 67u: {
            let d = q - u.p0.xy;
            let dist = length(d);
            let radius = u.p0.z;
            if radius < 0.5 || dist >= radius || dist < 1e-9 { return original; }
            let n = dist / radius;
            let k = 1.0 - 0.5 * sqrt(1.0 - n * n);
            return sample_bilinear(u.p0.x + d.x * k, u.p0.y + d.y * k);
        }
        case 68u: {
            let d=q-u.p0.xy;
            let distance=select(length(d),max(abs(d.x),abs(d.y)),u.i1.x!=0u);
            let k=select(clamp((u.p0.w-distance)/u.p1.x,0.0,1.0),clamp(u.p0.w-distance+0.5,0.0,1.0),u.p1.x<=0.0)*u.p1.y;
            if k<=0.0{return original;}
            let s=sample_bilinear(u.p0.x+d.x/u.p0.z,u.p0.y+d.y/u.p0.z);
            let base=select(vec3(0.0),original.rgb/original.a,original.a>1e-6);
            let sample=select(vec3(0.0),s.rgb/s.a,s.a>1e-6);
            let mixed=spatial_blend(u.i1.y,base,sample);
            let target_color=vec4(mixed*s.a,s.a);
            return original+(target_color-original)*k;
        }
        case 69u: {
            let d = (q - u.p0.xy) / u.p0.w;
            let scale = 1.0 - u.p0.z * dot(d, d);
            return sample_bilinear(u.p0.x + d.x * scale * u.p0.w, u.p0.y + d.y * scale * u.p0.w);
        }
        case 70u: {
            let direction = vec2<f32>(sin(u.p0.z), -cos(u.p0.z));
            let off = u.p0.x * sin(dot(q, direction) / u.p0.y * 6.28318530718 - u.p0.w);
            return sample_bilinear(q.x - direction.y * off, q.y + direction.x * off);
        }
        case 72u: {
            let m0=u.p0;let m1=u.p1;let m2=u.p2;
            let den=m1.z*q.x+m1.w*q.y+m2.x;
            if abs(den)<1e-12{return vec4(0.0);}
            let uv=vec2((m0.x*q.x+m0.y*q.y+m0.z)/den,(m0.w*q.x+m1.x*q.y+m1.y)/den);
            if uv.x< -0.001||uv.y< -0.001||uv.x>1.001||uv.y>1.001{return vec4(0.0);}
            let pixel = uv * vec2(m2.yz);
            return sample_bilinear(pixel.x, pixel.y);
        }
        case 75u: {
            let m0=u.p0;let m1=u.p1;let m2=u.p2;
            let den=m1.z*q.x+m1.w*q.y+m2.x;
            if abs(den)<1e-12||m2.y/den<=0.0{return vec4(0.0);}
            return sample_bilinear((m0.x*q.x+m0.y*q.y+m0.z)/den,(m0.w*q.x+m1.x*q.y+m1.y)/den);
        }
        case 73u: {
            let bounds = vec4(u.p0.x, u.p0.y, u.p0.z, u.p0.w);
            let d = spatial_round_rect_sdf(q, bounds, u.p1.x);
            var o = original;
            let border = u.p1.z;
            if border > 0.0 {
                let k = clamp(0.5 - d, 0.0, 1.0) - clamp(0.5 - d - border, 0.0, 1.0);
                if k > 0.0 {
                    let over = vec4(vec3(u.p1.w, u.p2.x, u.p2.y) * k, k);
                    o = over + o * (1.0 - k);
                }
            }
            let a = select(clamp(0.5 - d, 0.0, 1.0), clamp(-d / u.p1.y, 0.0, 1.0), u.p1.y > 0.0);
            return o * a;
        }
        case 74u: {
            if u.i1.w == 4u { return spatial_vr_plane_to_sphere(p); }
            if u.i1.w == 3u { return spatial_vr_projection(p, original); }
            if u.i1.w == 2u { return spatial_vr_color_gradients(p, original); }
            if u.i1.w == 1u { return spatial_vr_chromatic(p, original); }
            if abs(u.p0.x) < 1e-3 { return original; }
            var offset: vec2<f32>;
            if u.i1.x != 0u {
                offset = (q - u.p0.zw) / u.p1.y * u.p0.x;
            } else {
                offset = vec2(cos(u.p0.y), sin(u.p0.y)) * u.p0.x;
            }
            let r = sample_bilinear(q.x - offset.x, q.y - offset.y);
            let b = sample_bilinear(q.x + offset.x, q.y + offset.y);
            let base = vec4(r.r, original.g, b.b, max(r.a, max(original.a, b.a)));
            return base + (original - base) * u.p1.x;
        }
        default: { return original; }
    }
}
