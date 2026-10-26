struct BoxParams { size: vec2<u32>, radius: u32, axis: u32 }
struct MixParams { image: vec4<f32> }
@group(0) @binding(0) var<uniform> box: BoxParams;
@group(0) @binding(1) var input: texture_2d<f32>;
@group(0) @binding(2) var output: texture_storage_2d<rgba32float, write>;

@group(0) @binding(3) var source: texture_2d<f32>;
@group(0) @binding(4) var a: texture_2d<f32>;
@group(0) @binding(5) var b: texture_2d<f32>;
@group(0) @binding(6) var<uniform> mix_params: MixParams;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let p = array<vec2<f32>,3>(vec2(-1.0,-1.0),vec2(3.0,-1.0),vec2(-1.0,3.0));
    return vec4(p[i],0.0,1.0);
}

@fragment
fn fs_convert(@builtin(position) at: vec4<f32>) -> @location(0) vec4<f32> {
    return textureLoad(source,vec2<i32>(at.xy),0);
}

@compute @workgroup_size(64)
fn cs_box(@builtin(global_invocation_id) id: vec3<u32>) {
    let vertical=box.axis!=0u;
    let n=select(box.size.x,box.size.y,vertical);
    let lines=select(box.size.y,box.size.x,vertical);
    if id.x>=lines || n==0u { return; }
    let line=i32(id.x); let r=i32(box.radius);
    let inv=1.0/f32(2*r+1); var acc=vec4(0.0);
    for(var i=-r;i<=r;i++) {
        let j=clamp(i,0,i32(n)-1);
        let p=select(vec2<i32>(j,line),vec2<i32>(line,j),vertical);
        acc+=textureLoad(input,p,0);
    }
    for(var x=0;x<i32(n);x++) {
        let out_p=select(vec2<i32>(x,line),vec2<i32>(line,x),vertical);
        textureStore(output,out_p,acc*inv);
        let out_i=x-r; let in_i=x+r+1;
        let out_j=clamp(out_i,0,i32(n)-1); acc-=textureLoad(input,select(vec2<i32>(out_j,line),vec2<i32>(line,out_j),vertical),0);
        let in_j=clamp(in_i,0,i32(n)-1); acc+=textureLoad(input,select(vec2<i32>(in_j,line),vec2<i32>(line,in_j),vertical),0);
    }
}

@fragment
fn fs_mix(@builtin(position) at: vec4<f32>) -> @location(0) vec4<f32> {
    let p=mix_params.image.z;
    let k=clamp((p-0.25)/0.5,0.0,1.0); let eased=k*k*(3.0-2.0*k);
    let xy=vec2<i32>(at.xy);
    return textureLoad(a,xy,0)*(1.0-eased)+textureLoad(b,xy,0)*eased;
}
