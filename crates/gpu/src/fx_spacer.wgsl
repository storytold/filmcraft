// Spacer kernel for the multi-pass effect runner (crates/gpu/src/fx_multipass.rs).
// Contract: bindings 0 and 1 are the plan inputs (unfilterable Rgba32Float, read with textureLoad;
// slot 0 is the source and binding 1 mirrors it). Binding 2 is the pass output (write-only
// Rgba32Float storage). Binding 3 is the uniform SpacerUniform, filled by
// `SpacerParams::uniform_words` in crates/render/src/gpufx_spacer.rs. Every number in it is
// already evaluated on the CPU; the kernel only does the inverse mapping, the corner mask and the fill.
//
// Restriction: sampling is bilinear with no mip chain. The CPU path (Image::transformed) pre-filters
// when the layer is minified by two or more, so the caller must only route here when the scale is
// above 1/2 (SpacerParams::gpu_route_ok).

struct SpacerUniform {
    // x0, y0, x1, y1: the fitted layer in destination pixels.
    rect: vec4<f32>,
    // x: 1 / scale (source pixels per destination pixel). y, z, w: unused.
    src: vec4<f32>,
    // x: corner radius in destination pixels (0 = square corners). y: fill flag (> 0.5 = fill). z, w: unused.
    shape: vec4<f32>,
    // rgb: fill colour, linear. a: unused.
    fill_color: vec4<f32>,
}

@group(0) @binding(0) var input0: texture_2d<f32>;
@group(0) @binding(1) var input1: texture_2d<f32>;
@group(0) @binding(2) var output: texture_storage_2d<rgba32float, write>;
@group(0) @binding(3) var<uniform> params: SpacerUniform;

// Texel fetch with transparent borders, as Image::get_or_clear does on the CPU.
fn texel_or_clear(src: texture_2d<f32>, x: i32, y: i32, size: vec2<i32>) -> vec4<f32> {
    if x < 0 || y < 0 || x >= size.x || y >= size.y {
        return vec4<f32>(0.0);
    }
    return textureLoad(src, vec2<i32>(x, y), 0);
}

// Bilinear sample at continuous source coordinates (pixel centres at +0.5), as Image::sample_bilinear.
fn sample_bilinear(src: texture_2d<f32>, u: f32, v: f32) -> vec4<f32> {
    let size = vec2<i32>(textureDimensions(src));
    let fx = u - 0.5;
    let fy = v - 0.5;
    let x0f = floor(fx);
    let y0f = floor(fy);
    let tx = fx - x0f;
    let ty = fy - y0f;
    // Clamp before converting so that a hostile coordinate cannot overflow the integer conversion.
    let x0 = i32(clamp(x0f, -2.0, f32(size.x) + 1.0));
    let y0 = i32(clamp(y0f, -2.0, f32(size.y) + 1.0));
    let a = texel_or_clear(src, x0, y0, size);
    let b = texel_or_clear(src, x0 + 1, y0, size);
    let c = texel_or_clear(src, x0, y0 + 1, size);
    let d = texel_or_clear(src, x0 + 1, y0 + 1, size);
    return mix(mix(a, b, tx), mix(c, d, tx), ty);
}

// Signed distance from p to a rounded rectangle spanning p0..p1 (negative inside).
fn round_rect_sdf(p: vec2<f32>, p0: vec2<f32>, p1: vec2<f32>, r: f32) -> f32 {
    let centre = (p0 + p1) * 0.5;
    let half_size = (p1 - p0) * 0.5 - vec2<f32>(r);
    let q = abs(p - centre) - half_size;
    let outside = length(max(q, vec2<f32>(0.0)));
    return outside + min(max(q.x, q.y), 0.0) - r;
}

@compute @workgroup_size(8, 8, 1)
fn cs_spacer(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = textureDimensions(output);
    if id.x >= size.x || id.y >= size.y {
        return;
    }
    let dst = vec2<f32>(f32(id.x) + 0.5, f32(id.y) + 0.5);
    let origin = params.rect.xy;
    let inv_scale = params.src.x;
    // Inverse of dst = origin + scale * src.
    let u = (dst.x - origin.x) * inv_scale;
    let v = (dst.y - origin.y) * inv_scale;
    let src_size = vec2<f32>(textureDimensions(input0));

    var color = vec4<f32>(0.0);
    if u >= -1.0 && v >= -1.0 && u <= src_size.x + 1.0 && v <= src_size.y + 1.0 {
        color = sample_bilinear(input0, u, v);
    }

    let radius = params.shape.x;
    if radius > 0.0 {
        let d = round_rect_sdf(dst, params.rect.xy, params.rect.zw, radius);
        color = color * clamp(0.5 - d, 0.0, 1.0);
    }

    if params.shape.y > 0.5 {
        // Over the fill colour: premultiplied colour plus the uncovered share of the background, opaque.
        color = vec4<f32>(color.rgb + params.fill_color.rgb * (1.0 - color.a), 1.0);
    }

    textureStore(output, vec2<i32>(id.xy), color);
}
