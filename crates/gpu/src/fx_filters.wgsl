// Median's fixed 3×3 channel-wise selection. Radius > 1 is rejected by FilterOp::gpu_ok.
fn median_total_key(v: f32) -> u32 {
    let bits = bitcast<u32>(v);
    return select(bits ^ 0x80000000u, ~bits, (bits & 0x80000000u) != 0u);
}

fn median_apply(p: vec2<i32>) -> vec4<f32> {
    var values: array<vec4<f32>, 9>;
    var n = 0u;
    for (var y = -1; y <= 1; y += 1) {
        for (var x = -1; x <= 1; x += 1) {
            values[n] = sample_clamped(f32(p.x + x) + 0.5, f32(p.y + y) + 0.5);
            n += 1u;
        }
    }
    // Sort each component independently, matching Image::median.
    var out = vec4<f32>(0.0);
    for (var c = 0u; c < 4u; c += 1u) {
        var channel: array<f32, 9>;
        for (var i = 0u; i < 9u; i += 1u) { channel[i] = values[i][c]; }
        for (var i = 1u; i < 9u; i += 1u) {
            let key = channel[i];
            var j = i;
            loop {
                if j == 0u || median_total_key(channel[j - 1u]) <= median_total_key(key) { break; }
                channel[j] = channel[j - 1u];
                j -= 1u;
            }
            channel[j] = key;
        }
        out[c] = channel[4];
    }
    return out;
}
