// Original FilmCraft code, MIT OR Apache-2.0. SDR linear premultiplied accumulator →
// quantized sRGB → limited-range BT.709 NV12 (same order as the portable encoder).
#include <metal_stdlib>
using namespace metal;
float code(float value) {
    if (isnan(value)) return 0.0f;
    float v = clamp(value, 0.0f, 1.0f);
    float e = v <= 0.0031308f ? v * 12.92f : 1.055f * pow(v, 1.0f / 2.4f) - 0.055f;
    return round(e * 255.0f) / 255.0f;
}
kernel void nv12(texture2d<float, access::read> src [[texture(0)]],
                 texture2d<float, access::write> yplane [[texture(1)]],
                 texture2d<float, access::write> uvplane [[texture(2)]],
                 uint2 p [[thread_position_in_grid]]) {
    if (p.x >= uvplane.get_width() || p.y >= uvplane.get_height()) return;
    float cb = 0.0f, cr = 0.0f;
    for (uint dy = 0; dy < 2; dy++) {
        for (uint dx = 0; dx < 2; dx++) {
            uint2 pos = p * 2 + uint2(dx, dy);
            // Premultiplied RGB already represents compositing over black.
            float3 rgb = src.read(pos).rgb;
            rgb = float3(code(rgb.r), code(rgb.g), code(rgb.b));
            float l = 0.2126f * rgb.r + 0.7152f * rgb.g + 0.0722f * rgb.b;
            float y = clamp(round(16.0f + 219.0f * l), 1.0f, 254.0f);
            yplane.write(float4(y / 255.0f, 0, 0, 1), pos);
            cb += (rgb.b - l) / 1.8556f;
            cr += (rgb.r - l) / 1.5748f;
        }
    }
    float2 uv = clamp(round(128.0f + 224.0f * float2(cb, cr) / 4.0f), float2(1.0f), float2(254.0f));
    uvplane.write(float4(uv / 255.0f, 0, 1), p);
}
