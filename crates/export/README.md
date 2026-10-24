# filmcraft-export

Sequence rendering, encoding, muxing, presets, progress and cancellation.

capabilities::software_encoder_capabilities reports CPU implementation coverage for
media V1: H.264 exists, while HEVC and AV1 software encoding are not implemented.
Hardware format availability is a separate question.

H.264 records describe the existing export path: 8-bit 4:2:0, progressive, constrained
Baseline/Main/High, even dimensions through 16384, and the export validator's 1000 fps
ceiling. These are coarse candidate constraints, not a guarantee of memory, level,
bitrate or arbitrary-stream support. Existing validators/session creation still apply.

Existing 8-bit H.264 can carry HDR color signaling. Complete HDR guarantees remain
unknown in these records; do not infer 10-bit output or label it SDR-only.
Update the records and tests when adding software HEVC/AV1 or changing export support.
