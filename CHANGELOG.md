# Changelog

## 2026-10-25 — Reliable codec oracle checks

- `crates/codecs/src/tests.rs`: explicitly encode the 16 kHz-source VoIP fixture at the 48 kHz Opus clock to avoid contradictory CodecDelay metadata from older external muxers. Use a fresh cache name and validate CodecDelay against OpusHead pre-skip before waveform comparisons. Keep the original 40 dB sequential and seek SNR assertions.
- `crates/vp9/tests/it/{common/mod.rs,conformance.rs}`: build the draft-mode fixture from key/inter pairs and explicitly clear inter-frame refresh masks, then decode the rewritten stream with the external oracle. Preserve the draft coverage and unflagged-frame exactness assertions independently of encoder temporal-layer heuristics.
- `docs/testing.md`: explain the fixture contracts and retained accuracy assertions.
