// Kernel contract for the multi-pass effect runner (crates/gpu/src/fx_multipass.rs).
// Bindings 0 and 1 are the plan inputs: unfilterable Rgba32Float, read with textureLoad.
// Binding 2 is the pass output: write-only Rgba32Float storage.

@group(0) @binding(0) var input0: texture_2d<f32>;
@group(0) @binding(1) var input1: texture_2d<f32>;
@group(0) @binding(2) var output: texture_storage_2d<rgba32float, write>;
// Binding 3: the pass parameter block, zero-padded to 16 bytes, at most 128 bytes (MAX_PARAM_BYTES).
@group(0) @binding(3) var<uniform> params: array<vec4<u32>, 8>;

// Identity pass: copies input0 to the output. Used to prove the chain, the pool and the bindings.
@compute @workgroup_size(8, 8, 1)
fn cs_copy(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = textureDimensions(output);
    if id.x >= size.x || id.y >= size.y {
        return;
    }
    let texel = vec2<i32>(id.xy);
    textureStore(output, texel, textureLoad(input0, texel, 0));
}

// Mixes input0 and input1 by 50 percent. Used to prove that a pass can read two slots.
@compute @workgroup_size(8, 8, 1)
fn cs_mix(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = textureDimensions(output);
    if id.x >= size.x || id.y >= size.y {
        return;
    }
    let texel = vec2<i32>(id.xy);
    let a = textureLoad(input0, texel, 0);
    let b = textureLoad(input1, texel, 0);
    textureStore(output, texel, mix(a, b, 0.5));
}
