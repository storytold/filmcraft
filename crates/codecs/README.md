# filmcraft-codecs

The container/codec integration hub and software/hardware decoder registry.

capabilities::software_decoder_capabilities returns implementation-coverage records
for the media V1 H.264, HEVC and AV1 subset. It reads the immutable built-in factories,
does not initialize decoders and does not inspect registered hardware.

Records are candidates and require stream/session validation. Unknown size/rate/HDR
constraints remain unknown. Profiles listed here do not replace bitstream validation
and are not an exhaustive description of every implemented AV1 format.

Update these records when changing a built-in factory or its supported configurations.
