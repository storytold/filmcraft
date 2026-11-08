# AC-3 and E-AC-3 audio export

FilmCraft exports AC-3 and E-AC-3 using the shared Rust encoder in craft-audio.
The implementation follows ETSI TS 102 366 V1.4.1 (2017-09), including Annex E
for Enhanced AC-3 and Annex F for ISO base media file storage. This is a baseline
encoder, not a claim of Dolby certification or support for every optional tool
in the specification.

## Delivery settings

Raw `.ac3` and `.eac3` files contain audio only. MP4 and MOV can carry either
codec alongside video. Selecting either codec preserves that choice instead of
silently replacing it with AAC. Existing export defaults are unchanged.

The supported sample rates are 32, 44.1 and 48 kHz. FilmCraft exposes mono,
stereo and 5.1 for both codecs, plus discrete rear-and-side 7.1 for E-AC-3.
Unsupported rates, layouts and bitrate combinations produce an error before
opening the output. Disabling audio leaves video-only export independent of
these audio settings. Finite timeline values beyond full scale are clipped at
the encoder boundary; non-finite samples are errors. Loudness normalization and
limiting remain the existing optional export controls.

## Speaker assignments

| Layout | Planar channel order |
| --- | --- |
| Mono | Centre |
| Stereo | Left, Right |
| 5.1 | Left, Right, Centre, LFE, Left Surround, Right Surround |
| 7.1 | Left, Right, Centre, LFE, Left Rear, Right Rear, Left Side, Right Side |

The 7.1 order is WAVE mask `0x63f`, not the distinct 7.1-wide arrangement.
A verified media layout or an explicit **Modify Audio Channels → 7.1**
interpretation establishes these assignments. Eight planes alone do not prove
speaker positions: 7.1 export refuses unknown eight-channel sources until their
layout is assigned. A selected mono or stereo pair remains a deliberate pair.
The check follows the export range, graph solo/mute rules and audible pre-fader
sends. Ordinary nested clips map the selected source range; nested remix, pitch
preservation and audio effects conservatively check their full source sequence
because their history or non-linear reads can exceed the visible interval.
Count-only eight-channel AAF/OMF tracks remain adaptive, not inferred 7.1.

A 7.1 track's balance affects the left/right front, rear and side pairs together;
Centre and LFE stay unchanged. Converting 5.1 to 7.1 places its surrounds in the
side pair and leaves the rear pair silent. Converting 7.1 to 5.1 combines each
rear/side pair at -3 dB each; stereo downmix excludes LFE. The existing 5.1
panner handles lower-width sources routed into a 7.1 bus. These controls do not
constitute a new object-based or immersive panner.

Clip-level Auto-Match and ducking analysis use the same verified or manually
assigned 7.1 speaker layout through the media provider, including rear/side
content. Media-pool, monitor and proxy wrappers preserve the selected audio
source's speaker metadata.

## Timing and container metadata

Each encoded access unit represents 1,536 samples. A 7.1 E-AC-3 access unit holds
its independent and dependent syncframes together. The sample-entry channel
count is the Annex F value of two; `dac3`/`dec3` describes the actual speaker
layout. The E-AC-3 data rate covers the complete programme, including its
dependent substream.

The encoder has 256 samples of priming and pads the final access unit. MP4/MOV
uses an edit list to present the requested duration and skip priming once;
FilmCraft reimport respects both boundaries, including after resampling. Raw
streams lack exact-duration metadata, so priming and final padding remain in
raw playback. Some external decoding tools output trailing padded PCM even
when the container reports the correct presentation duration; raw PCM consumers
must honor that duration themselves.

## Validation scope

Tests exercise settings, discrete channel routing, finite overload handling,
raw export/reimport, MP4/MOV metadata, priming and presentation-end trimming.
Independent black-box decoding checks interoperability without linking or
bundling an external codec. Certification, exhaustive decoder compatibility,
and subjective listening across a broad production corpus are separate from
these functional checks.
