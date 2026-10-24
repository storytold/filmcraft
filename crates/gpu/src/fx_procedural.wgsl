// Typed procedural effect payload. Included by fx.wgsl after its shared helpers.
fn procedural_smooth(a: f32, b: f32, x: f32) -> f32 {
    let t = clamp((x-a)/max(b-a, 1e-6), 0.0, 1.0);
    return t*t*(3.0-2.0*t);
}
fn procedural_hash_mul_hi(a:u32,b:u32)->u32 {
    let a0=a&0xffffu; let a1=a>>16u; let b0=b&0xffffu; let b1=b>>16u;
    let p0=a0*b0; let p1=a0*b1; let p2=a1*b0;
    let mid=(p0>>16u)+(p1&0xffffu)+(p2&0xffffu);
    return a1*b1+(p1>>16u)+(p2>>16u)+(mid>>16u);
}
fn procedural_mul64(a:vec2<u32>,b:vec2<u32>)->vec2<u32>{return vec2(a.x*b.x,procedural_hash_mul_hi(a.x,b.x)+a.x*b.y+a.y*b.x);}
fn procedural_hash(x:i32,y:i32,z:i32,seed:u32)->f32 {
    var h=procedural_mul64(vec2(bitcast<u32>(x),select(0xffffffffu,0u,x>=0)),vec2(0x7f4a7c15u,0x9e3779b9u));
    h=h^procedural_mul64(vec2(bitcast<u32>(y),select(0xffffffffu,0u,y>=0)),vec2(0x27d4eb4fu,0xc2b2ae3du));
    h=h^procedural_mul64(vec2(bitcast<u32>(z),select(0xffffffffu,0u,z>=0)),vec2(0x9e3779f9u,0x165667b1u));
    h=h^procedural_mul64(vec2(seed,0u),vec2(0x165667c5u,0x27d4eb2fu));
    h=h^vec2((h.x>>31u)|(h.y<<1u),h.y>>31u);
    h=procedural_mul64(h,vec2(0x1ce4e5b9u,0xbf58476du));
    h=h^vec2((h.x>>29u)|(h.y<<3u),h.y>>29u);
    h=procedural_mul64(h,vec2(0x133111ebu,0x94d049bbu));
    h=h^vec2(h.y,0u);
    return f32(h.y>>8u)/16777216.0;
}
fn procedural_over(d:vec4<f32>,s:vec4<f32>)->vec4<f32>{return s+d*(1.0-s.a);}
fn procedural_lens_pick(i:u32,a:vec4<f32>,b:vec4<f32>)->f32 { let v=select(a,b,i>=4u); return v[i&3u]; }
fn procedural_lens_hue(i:u32)->vec3<f32> {
    let k=i&3u;
    if k==0u {return vec3(0.4,0.6,1.0);} if k==1u {return vec3(1.0,0.5,0.3);} if k==2u {return vec3(0.4,1.0,0.5);}
    return vec3(0.9,0.4,1.0);
}
fn procedural_apply(p: vec2<i32>) -> vec4<f32> {
    let o=ld(p); let x=f32(p.x)+0.5; let y=f32(p.y)+0.5;
    let op=u.i0.x;
    if op==109u {
        let c=u.p0.xy; let scale=u.p0.z; let k=u.p0.w; let tint=vec3(u.p1.x,u.p1.y,u.p1.z);
        let lens=u.i1.x; let ng=u.i1.y; let mid=vec2<f32>(size())*0.5; let q=vec2(x,y);
        var core_k=18.0; if lens==1u {core_k=26.0;} else if lens==2u {core_k=14.0;} else if lens>=3u {core_k=22.0;}
        let d=length(q-c)/scale;
        var add=vec3(0.35*exp(-d*core_k)+0.08*exp(-d*3.0));
        if lens==1u||lens==2u { add+=max(1.0-abs((d-0.12)/0.012),0.0)*0.08*vec3(0.6,0.8,1.0); }
        if lens==3u {
            let s=exp(-(abs(q.y-c.y)/scale)*160.0)*exp(-(abs(q.x-c.x)/scale)*2.0)*0.5;
            add+=vec3(s*0.5,s*0.7,s);
        }
        for (var i=0u;i<ng;i=i+1u) {
            let t=procedural_lens_pick(i,u.p2,u.p3); let rad=procedural_lens_pick(i,u.p4,u.p5);
            let g=c+(mid-c)*2.0*t; let dd=length(q-g)/scale;
            let gl=max(1.0-abs((dd-rad)/0.01),0.0)*0.12+select(0.0,0.05,dd<rad);
            add+=procedural_lens_hue(i)*gl;
        }
        let a=add*tint*k; let m=clamp(max(a.x,max(a.y,a.z)),0.0,1.0);
        return vec4(o.rgb+a,o.a+(1.0-o.a)*m);
    }
    if op==96u {
        let a=u.p0.xy; let wh=max(u.p0.zw,vec2(1.0)); let uv=(vec2(x,y)-a)/wh;
        let parity=f32((i32(floor(uv.x))+i32(floor(uv.y)))&1);
        let d=min(abs(uv.x-round(uv.x))*wh.x,abs(uv.y-round(uv.y))*wh.y);
        let edge=select(1.0,procedural_smooth(0.0,u.p1.x,d),u.p1.x>0.0); let k=parity*edge*u.p2.x;
        let s=vec4(u.p1.yzw*k,k);
        if u.i1.x==0u{return s;} if u.i1.x==1u{return procedural_over(o,s);}
        if o.a<=1e-6{return o;} let b=o.rgb/o.a; var c=b;
        if u.i1.x==2u{c=b+u.p1.yzw;} else if u.i1.x==3u{c=b*u.p1.yzw;} else if u.i1.x==4u{c=b+u.p1.yzw-b*u.p1.yzw;} else{c=2.0*b*u.p1.yzw;}
        return vec4(mix(b,c,k)*o.a,o.a);
    }
    if op==97u {
        let s=max(u.p0.x,1.0); let q=vec2(f32(p.x),f32(p.y))-vec2(f32(u.i0.y),f32(u.i0.z))*0.5;
        let rx=q.x-floor(q.x/s)*s; let ry=q.y-floor(q.y/s)*s; let d=min(min(rx,s-rx),min(ry,s-ry));
        let a=clamp(u.p0.y*0.5-d+0.5,0.0,1.0)*u.p1.y;
        return procedural_over(o,vec4(vec3(u.p0.z,u.p0.w,u.p1.x)*a,a));
    }
    if op==98u {
        let d=distance(vec2(x,y),u.p0.xy); let a=clamp(u.p0.z-d+0.5,0.0,1.0)*u.p1.z;
        return procedural_over(o,vec4(vec3(u.p0.w,u.p1.xy)*a,a));
    }
    if op==99u {
        let q=(vec2(x,y)-u.p0.xy)/max(u.p0.zw,vec2(0.5)); let d=abs(length(q)-1.0)*min(u.p0.z,u.p0.w); let half=u.p1.x*0.5;
        let a=1.0-procedural_smooth(half*(1.0-u.p1.y)-0.5,half+0.5,d); let t=1.0-procedural_smooth(0.0,half,d);
        let col=mix(u.p2.yzw,vec3(u.p1.z,u.p1.w,u.p2.x),t); let s=vec4(col*a,a); if u.i1.x!=0u{return procedural_over(o,s);} return s;
    }
    if op==100u || op==101u {
        let v=u.p0.xy; let d=u.p0.zw-v; let l2=max(dot(d,d),1e-6); let q=vec2(x,y)-v;
        var t=dot(q,d)/l2;
        if op==100u {if u.i1.x!=0u{t=length(q)/sqrt(l2);} t=clamp(t,0.0,1.0);let c=dec(mix(u.p1.xyz,u.p2.xyz,t));let k=u.p3.x;return vec4(c*(1.0-k)+o.rgb*k,1.0-k+o.a*k);}
        let across=(-q.x*d.y+q.y*d.x)/l2;
        if u.i1.x==1u{t=length(q)/sqrt(l2);}else if u.i1.x==2u{t=abs(t);}else if u.i1.x==3u{t=abs(t)+abs(across);}
        if u.p3.x>0.0{t+=(procedural_hash(p.x,p.y,0,13u)-0.5)*u.p3.x*0.1;}
        t=pow(clamp(t,0.0,1.0),u.p3.y); let c=dec(mix(u.p1.xyz,u.p2.xyz,t)); let a=mix(u.p1.w,u.p2.w,t);let k=u.p3.z;let g=vec4(c*a,a);return mix(g,o,k);
    }
    if op==102u {
        let pts=array<vec2<f32>,4>(vec2(0.25*u.p3.z,0.25*u.p3.w),vec2(0.75*u.p3.z,0.25*u.p3.w),vec2(0.25*u.p3.z,0.75*u.p3.w),vec2(0.75*u.p3.z,0.75*u.p3.w));
        let cols=array<vec3<f32>,4>(u.p0.xyz,vec3(u.p0.w,u.p1.xy),vec3(u.p1.z,u.p1.w,u.p2.x),u.p2.yzw);
        var acc=vec3(0.0);var sum=0.0;for(var i=0u;i<4u;i++){let w=1.0/pow(max(distance(vec2(x,y),pts[i]),1.0),u.p3.x);acc+=cols[i]*w;sum+=w;}
        let c=dec(acc/sum);let a=u.p3.y;return procedural_over(o,vec4(c*a,a));
    }
    if op==103u {
        let size=max(u.p0.z,1.0);let uv=(vec2(x,y)+vec2(u.p0.w,u.p1.x))/size;let cell=vec2<i32>(floor(uv));var f1=1e20;var f2=1e20;var ident=0.0;
        for(var dy=-1;dy<=1;dy++){for(var dx=-1;dx<=1;dx++){let g=cell+vec2(dx,dy);let ph=procedural_hash(g.x,g.y,9,u.i1.w);let jx=0.5+(procedural_hash(g.x,g.y,0,u.i1.w)-0.5)*u.p0.y+cos(u.p1.y+ph*6.2831853)*0.15*u.p0.y;let jy=0.5+(procedural_hash(g.x,g.y,1,u.i1.w)-0.5)*u.p0.y+sin(u.p1.y+ph*6.2831853)*0.15*u.p0.y;let d=distance(vec2<f32>(g)+vec2(jx,jy),uv);if d<f1{f2=f1;f1=d;ident=procedural_hash(g.x,g.y,2,u.i1.w);}else if d<f2{f2=d;}}}
        var v=1.0-smoothstep(0.0,0.12,abs(f1-0.35));if u.i1.x==0u{v=max(1.0-f1*1.4,0.0);}else if u.i1.x==1u{v=(f2-f1)*2.0;}else if u.i1.x==2u||u.i1.x==3u{v=ident;}else if u.i1.x==4u{v=ident*procedural_smooth(0.0,0.12,f2-f1);}else if u.i1.x==5u{v=(f2-f1)/max(f2+f1,1e-4)*2.0;}else if u.i1.x==6u{v=((f2-f1)*2.0+ident)*0.5;}
        v=(v-0.5)*u.p0.x+0.5;if u.i1.z==1u{v=1.0/(1.0+exp(-(v-0.5)*4.0));}else if u.i1.z==2u{let m=v-2.0*floor(v/2.0);v=select(m,2.0-m,m>1.0);}else{v=clamp(v,0.0,1.0);}if u.i1.y!=0u{v=1.0-v;}let c=dec(vec3(v));return vec4(c,1.0);
    }
    if op==104u || op==105u {
        let blend=select(u.p0.x,u.p1.x,op==104u);var v=0.0;
        if op==104u{let d=u.p0.xy*u.p0.z;let a=sample_clamped(f32(p.x)+0.5+d.x,f32(p.y)+0.5+d.y);let b=sample_clamped(f32(p.x)+0.5-d.x,f32(p.y)+0.5-d.y);var la=luma709(a.rgb);var lb=luma709(b.rgb);if u.i1.x!=0u{la=luma709(a.rgb/max(a.a,1e-6));lb=luma709(b.rgb/max(b.a,1e-6));}let delta=(la-lb)*u.p0.w*2.0;if u.i1.x==0u{v=dec(vec3(clamp(0.5+delta,0.0,1.0))).x;return vec4(vec3(v*o.a)*(1.0-u.p1.x)+o.rgb*u.p1.x,o.a);}let c=clamp(enc(o.rgb/max(o.a,1e-6))+vec3(delta),vec3(0.0),vec3(1.0));return vec4(dec(mix(c,enc(o.rgb/max(o.a,1e-6)),u.p1.x))*o.a,o.a);}
        let l0=luma709(ld(clamp(p+vec2(-1,-1),vec2(0),vec2(i32(u.i0.y)-1,i32(u.i0.z)-1))).rgb);let l1=luma709(ld(clamp(p+vec2(0,-1),vec2(0),vec2(i32(u.i0.y)-1,i32(u.i0.z)-1))).rgb);let l2=luma709(ld(clamp(p+vec2(1,-1),vec2(0),vec2(i32(u.i0.y)-1,i32(u.i0.z)-1))).rgb);let l3=luma709(ld(clamp(p+vec2(-1,0),vec2(0),vec2(i32(u.i0.y)-1,i32(u.i0.z)-1))).rgb);let l5=luma709(ld(clamp(p+vec2(1,0),vec2(0),vec2(i32(u.i0.y)-1,i32(u.i0.z)-1))).rgb);let l6=luma709(ld(clamp(p+vec2(-1,1),vec2(0),vec2(i32(u.i0.y)-1,i32(u.i0.z)-1))).rgb);let l7=luma709(ld(clamp(p+vec2(0,1),vec2(0),vec2(i32(u.i0.y)-1,i32(u.i0.z)-1))).rgb);let l8=luma709(ld(clamp(p+vec2(1,1),vec2(0),vec2(i32(u.i0.y)-1,i32(u.i0.z)-1))).rgb);let gx=l2+2.0*l5+l8-l0-2.0*l3-l6;let gy=l6+2.0*l7+l8-l0-2.0*l1-l2;v=min(length(vec2(gx,gy)),1.0);if u.i1.x==0u{v=1.0-v;}let q=v*o.a*(1.0-blend);return vec4(vec3(q)+o.rgb*blend,o.a);
    }
    if op==106u {if u.i1.x==0u{return o;}let c=mix(u.p0.xyz,o.rgb/max(o.a,1e-6),u.p0.w);return vec4(c*o.a,o.a);}
    return o;
}
