# AMD Linux video backend

## Decision

VA-API H.264/HEVC is the only production AMD video-encode route. Vulkan Video is diagnostic only and must not be selected as an export encoder. The VA-API context and Vulkan device must refer to the same DRM node.

Keep decoded/composed video pixels in GPU memory. GPU-to-GPU copies are allowed when external-memory layouts or ownership transitions require them; do not read video pixels back to CPU memory. Copy the encoded bitstream to the CPU only when the muxer needs it.

Bound in-flight transfer frames to at most three owned slots, with backpressure under the 90% host-RAM runtime ceiling. This transfer limit does not reduce the codec's required DPB. Reuse a slot only after the exact GPU consumer submission, VA-API completion, and bitstream extraction have completed. If the route or synchronization is unavailable, fail closed; do not fall back to software encoding, CPU pixel processing, or FFmpeg.

Integrated Radeon APUs are supported: `DEVICE_LOCAL` describes device access, not dedicated VRAM. Keep driver power and thermal controls enabled; 100% GPU busy is not a fixed contract.

## Current status

The 0.10.0 export integration is incomplete. `AmdRequired` returns an explicit unsupported error until the resident VA-API/Vulkan route reaches the encoder. This decision records the target contract; it does not claim end-to-end GPU-resident export or hardware acceptance.
