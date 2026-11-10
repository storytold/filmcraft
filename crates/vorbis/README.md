# filmcraft-vorbis

A clean-room Vorbis I encoder, written from the [Vorbis I specification](https://xiph.org/vorbis/doc/Vorbis_I_spec.html)
(Xiph.Org, 2020-07-04). Layer L0: no dependencies, no `unsafe`, builds for
`wasm32-unknown-unknown`. No libvorbis, FFmpeg or other Vorbis implementation was read or ported;
ffmpeg is used only as an external test oracle. Decoding Vorbis in FilmCraft is still done by the
symphonia bootstrap decoder in `filmcraft-codecs`.

## API

```rust
use filmcraft_vorbis::{Encoder, EncoderConfig};

/// The identification and setup headers, and the audio packets with their granule positions.
fn encode(left: &[f32], right: &[f32]) -> filmcraft_vorbis::Result<(Vec<u8>, Vec<u8>, Vec<(Vec<u8>, i64)>)> {
    let mut enc = Encoder::new(EncoderConfig::new(48_000, 2, 6.0))?; // rate, channels (1–8), quality −1…10
    let (id, setup) = (enc.identification(), enc.setup());
    let mut packets = enc.encode(&[left, right])?;                    // (packet, granule position)
    packets.extend(enc.finish()?);                                    // last granule = input length
    Ok((id, setup, packets))
}
```

The comment header (the second of the three) is not this crate's: build it with
`filmcraft_ogg::vorbis_comment`, a separate crate, as `crates/export` does. This crate has no
dependencies (layer L0).

Channels are in Vorbis order (§4.3.9; 5.1 is L, C, R, Ls, Rs, LFE).

## What it writes

| Part | |
|---|---|
| Blocks | one mode: 2048-sample blocks (the header's 256-sample size is never used), the Vorbis window, a forward MDCT computed as a DCT-IV through a 512-point FFT |
| Alignment | the first block is centred on input sample 0, so the decoder's first output sample is the first input sample; granule positions count samples exactly and the last one trims the padding |
| Floor | floor 1, 34 points spaced in log frequency, multiplier 2; the targets follow the spectral envelope (power smoothed over a quarter of the frequency, at least 8 bins, each side); the encoder renders the curve with the decoder's integer arithmetic |
| Residue | type 2 over the interleaved channels, partitions of 32, eight classes: silent; sparse and dense ±1 (4-D lattices), ±2, ±4 and ±7 (2-D); cascades of ±7·15 + ±7 and ±7·225 + ±7·15 + ±7 for larger values |
| Codebooks | Huffman lengths from fixed models, codewords assigned the way the decoder assigns them (§3.2.1) |
| Mapping | one submap, no channel coupling |

Quality sets where the floor sits against the envelope, from each region's spectral flatness:
tonal regions 4 dB (−1) to 30.4 dB (10) below it, noise-like regions 14 dB (−1) to 4.1 dB (10)
above it, so noise is coded coarsely and tones finely; a dead zone (0.8 to 0.53 of a step) drops
isolated noise peaks. The bit rate follows the material: on music-like test signals about 45, 55,
105–130, 190–215 and 265–295 kbit/s for stereo at qualities −1, 0, 3, 6 and 10 (the export's size
estimate assumes 190 kbit/s at 6).

Not yet: short blocks (transients pre-echo), channel coupling, a psychoacoustic model beyond the
smoothed envelope, bit-rate targets.

## Tests

`cargo test -p filmcraft-vorbis`: the MDCT against the direct formula, the specification's
codeword example, every book a complete prefix code, float packing, LSB-first bit packing,
granule positions and end trimming, hostile input; and ffmpeg's two Vorbis decoders (its own and
libvorbis) reading every stream with `-xerror`, exactly as long as the input, against SNR floors
and bit-rate bounds per quality, plus mono at 44.1 kHz, stereo at 22.05 kHz and 5.1 with a silent
channel (skipped without ffmpeg). `crates/export` checks FilmCraft's own import of its exports.
