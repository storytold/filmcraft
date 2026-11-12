# filmcraft-opusenc

A clean-room CELT-only Opus encoder (RFC 6716, updated by RFC 8251) and the `OpusHead` / `dOps`
it writes (RFC 7845 §5.1). Layer L0: no dependencies, no `unsafe`, builds for
`wasm32-unknown-unknown`. No libopus code was read or ported; libopus and ffmpeg are used only as
external test oracles.

Decoding Opus is `craft_audio::opus` ([craft-libs](https://github.com/storytold/craft-libs)). This
crate stays in FilmCraft until craft-audio has an Opus encoder; then export switches to it and this
crate is removed.

## Use

```rust
use filmcraft_opusenc::{Encoder, EncoderConfig};

fn encode(left: &[f32], right: &[f32]) -> filmcraft_opusenc::Result<Vec<Vec<u8>>> {
    let mut enc = Encoder::new(EncoderConfig::new(2, 160_000))?; // stereo, 160 kbit/s
    let _head = enc.head();                                      // OpusHead: pre-skip PRE_SKIP, 48 kHz
    let mut packets = enc.encode(&[left, right])?;               // planar 48 kHz f32, any chunk size
    packets.extend(enc.finish()?);                               // padded so the last sample decodes
    // the stream is enc.samples_in() samples long after the pre-skip
    Ok(packets)
}
```

| | |
|---|---|
| Packets | TOC configuration 31 (CELT, fullband, 20 ms), one frame (code 0); every packet the same size, so the bit rate is exact: `MIN_BITRATE` to `MAX_BITRATE` (1.2–510.4 kbit/s, 3- to 1276-byte packets), rounded to whole bytes per packet. FilmCraft's export offers 6–510 kbit/s |
| Delay | 120 samples (the CELT window overlap), signalled as the pre-skip |
| Analysis | pre-emphasis, a forward MDCT built as the exact inverse of the decoder's (window fold + the scaled transpose of its FFT core), band energies |
| Energy | coarse energy with the Laplace model (intra on the first frame, then inter prediction), fine energy, the final bits on leftover budget |
| Shapes | greedy PVQ search after the spreading rotation; band splitting, mid/side and intensity stereo through the decoder's own code |
| Decisions | no post-filter, long blocks only, no TF changes, aggressive spreading below 32 kbit/s per channel, default trim, no dynamic allocation boosts, intensity stereo from the bit rate |

The allocation and band-shape code (`celt/rate.rs`, `celt/bands.rs`) serves both directions
through the `Coder` trait (`range.rs`) and come from the decoder that is now craft-audio's: the
decoder reads each decision, the encoder makes it and runs the same reconstruction, so its folding source is bit-identical to the decoder's and the two
cannot drift apart. The range encoder counts bits exactly as the decoder does (`tell`,
`tell_frac`, the final range), which keeps every budget test on both sides in step.

Not yet: SILK and hybrid speech modes, short blocks (transients), other frame sizes, variable bit
rate, the pitch pre-filter, multistream surround.

## Tests

`cargo test -p filmcraft-opusenc`:
- the range coder round-trips every symbol kind with the decoder's bit counts after each (the
  range decoder, MDCT inverse and Laplace decoder are kept, test-only, as oracles);
- every PVQ codeword of small sizes encodes to the index the decoder walks back to;
- the forward MDCT is undone by the decoder's overlap-add at every frame size;
- `tests/it/encoder.rs` decodes with `craft_audio::opus` (a dev-dependency) and round-trips tones,
  chords, music-like signals, noise and silence, mono and stereo, 1.2–510 kbit/s, against SNR
  floors; it also checks the length after the pre-skip, the last input sample surviving the
  padding, determinism, and that NaN, infinite, ragged or mismatched input is an error, never a
  panic.

Export (`crates/export`) checks that libopus and ffmpeg's own decoder read every `.opus`, MP4 and
MOV it writes without a complaint. `.opus` files decode to the exact length (the last granule
position trims the padding). In MP4 / MOV the edit list spans exactly the input, which FilmCraft's
own import honours; ffmpeg skips the pre-skip but plays the last packet in full, up to 20 ms more.
