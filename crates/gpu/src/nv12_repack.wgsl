struct Params {
    width: u32,
    height: u32,
    y_offset: u32,
    y_pitch: u32,
    uv_offset: u32,
    uv_pitch: u32,
    y_stride: u32,
    uv_stride: u32,
    output_uv_offset: u32,
    output_size: u32,
    origin_x: u32,
    origin_y: u32,
    _pad0: u32,
    _pad1: u32,
};

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> source_words: array<u32>;
@group(0) @binding(2) var<storage, read_write> output_words: array<u32>;

fn source_byte(offset: u32) -> u32 {
    let word = source_words[offset >> 2u];
    let shift = (offset & 3u) * 8u;
    return (word >> shift) & 255u;
}

fn output_byte(offset: u32) -> u32 {
    if offset < params.output_uv_offset {
        let row = offset / params.y_stride;
        let column = offset % params.y_stride;
        if row < params.height && column < params.width {
            return source_byte(params.y_offset + (row + params.origin_y) * params.y_pitch + column + params.origin_x);
        }
        return 0u;
    }
    if offset < params.output_size {
        let uv_relative = offset - params.output_uv_offset;
        let row = uv_relative / params.uv_stride;
        let column = uv_relative % params.uv_stride;
        if row < params.height / 2u && column < params.width {
            return source_byte(params.uv_offset + (row + params.origin_y / 2u) * params.uv_pitch + column + params.origin_x);
        }
    }
    return 0u;
}

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let word_index = id.x;
    let first_byte = word_index * 4u;
    if first_byte >= params.output_size {
        return;
    }
    var packed = 0u;
    for (var lane = 0u; lane < 4u; lane += 1u) {
        packed |= output_byte(first_byte + lane) << (lane * 8u);
    }
    output_words[word_index] = packed;
}
