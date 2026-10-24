# filmcraft-vp8

Clean-room, pure-Rust (no `unsafe`) VP8 decoder, implemented from RFC 6386, *VP8 Data Format and
Decoding Guide*, sections 1-19. It is an L0 crate: it depends only on `thiserror`, and builds for
`wasm32-unknown-unknown`.

The constant tables (coefficient and mode probabilities and their update probabilities, the
subblock mode contexts, quantizer lookups, MV probabilities, interpolation filters...) were
extracted from the text of the RFC by [`tools/extract_tables.py`](tools/extract_tables.py) into
`src/spec_tables.rs`; the script cuts the text at section 20, so nothing is read from the
reference decoder attached to the RFC. No code was taken or ported from libvpx, FFmpeg or any
other decoder; ffmpeg (with libvpx) and vpxenc are used only as external fixture generators and
test oracles.

## Status

| Area | Status |
|---|---|
| Frame tag, key frame header, frame header (segmentation, loop filter type / level / sharpness, deltas, partitions, quantizer indices, reference updates and sign bias, probability updates) | done |
| Boolean decoder, tree decoding, probability persistence across frames and `refresh_entropy_probs` | done |
| Intra prediction: 16x16 and chroma modes, the ten 4x4 subblock modes, key frame subblock mode contexts, edge rules | done |
| Inter prediction: last / golden / altref with sign bias, NEAREST / NEAR / ZERO / NEW / SPLIT (all four partitionings), MV decoding and clamping, six-tap and bilinear filters, full-pixel chroma (bitstream versions 0-3) | done |
| Coefficient tokens (1-8 partitions), dequantization (frame and segment quantizers), inverse WHT and DCT | done |
| Normal and simple loop filters, per-macroblock levels from segments and reference / mode deltas | done |
| Golden / altref copies, hidden (altref) frames, frame size changes at key frames | done |
| Damaged input: errors (never panics), decoding resumes at the next key frame | done |
| Upscaling hints of the key frame header (9.1) | not applied (reported in `KeyframeInfo`) |

Bit-exact with libvpx on the 62 libvpx VP8 test vectors checked (`vp80-00-comprehensive-001` to
`018` and the `vp80-01` to `vp80-06` intra, inter, segmentation, partition, sharpness and small
size vectors; `tests/it/vectors.rs` checks them when they are present) and on the generated fixtures
of `tests/it/conformance.rs`.

## API

```rust
use filmcraft_vp8::Decoder;

let mut dec = Decoder::new();
for (pts, frame) in frames {      // one IVF frame / WebM block / MP4 sample each
    let skippable = dec.is_disposable(frame); // could this frame be left out (catching up)?
    if let Some(pic) = dec.decode(frame, pts)? {
        // pic.width / pic.height, pic.chroma_width / chroma_height,
        // pic.y / pic.u / pic.v: tight 8-bit 4:2:0 planes, pic.pts, pic.key,
        // pic.color_space (0: BT.601 YUV)
    }
    // None: a hidden (altref) frame
}
dec.reset(); // forget the reference frames (seek): decoding resumes at the next key frame
let stats = dec.stats(); // frames, key frames, hidden frames

// Random access points
let key: bool = filmcraft_vp8::is_keyframe(frame);
let info: Option<filmcraft_vp8::KeyframeInfo> = filmcraft_vp8::keyframe_info(frame);
```

`filmcraft-codecs` wires the decoder into Matroska / WebM (`V_VP8`, including the alpha layer of
transparent WebM in `BlockAdditional`) and MP4 (`vp08`). VP8 has no colour signalling: pictures are
BT.601, studio swing (RFC 6386 9.2), unless the container's `Colour` says otherwise. VP8 has no
superframes either: a hidden altref frame is a block of its own, and the WebM reader takes a
request for its time as a request for the shown frame that follows it.

## Tests and tools

- `cargo test -p filmcraft-vp8`: unit tests, bit-exactness against libvpx on generated fixtures
  (`target/fixtures/vp8-tests`, skipped without ffmpeg / libvpx), robustness (corrupted,
  truncated, reordered and random input), and the libvpx test vectors when present in
  `target/fixtures/vp8/conformance` (or `$VP8_TEST_VECTORS`).
- `cargo run --release -p filmcraft-vp8 --example vp8dec in.ivf out.yuv`: decode to raw I420.
- `cargo run -p filmcraft-vp8 --example fuzz -- <iterations> <seed> files.ivf...`: decode
  randomly damaged variants of IVF files (in a debug build, so overflow panics too) and report
  any panic.
