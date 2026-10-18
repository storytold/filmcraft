# FilmCraft desktop host

The desktop host connects the editor to native windows, audio devices, file dialogs and the
control server. See [contributing](../../docs/contributing.md) for build and run instructions.

## Razer 7.1 audio output

On WASAPI, endpoints whose names contain `7.1 Surround Sound` prefer supported signed 32-bit
PCM over float output. The Razer virtual endpoint commonly appears as
`Speakers (7.1 Surround Sound)`, without the vendor name. This is a workaround for the float
distortion reported in [FilmCraft #2411](https://github.com/storytold/filmcraft/issues/2411)
and [CPAL #892](https://github.com/RustAudio/cpal/issues/892#issuecomment-2364194193).
Other endpoints retain the existing float preference. Renamed endpoints that no longer contain
that name are not detected.

Selection preserves the default channel count and requested sample rate when supported. If the
requested rate is unavailable, the workaround also applies at the default rate. If no matching
PCM configuration is advertised, selection falls back to the existing supported formats; no
unsupported format is forced. The mixer remains float internally and uses the existing PCM
conversion at the device boundary.

`cargo test -p filmcraft audio::tests` covers endpoint matching, format/rate/channel selection,
fallbacks and eight-channel PCM conversion without requiring a physical surround device.
These tests cannot establish whether the Razer driver distortion is gone. Hardware validation
still requires Windows with Razer 7.1: play the bundled demo through that endpoint, compare
with the ordinary output, and check stereo mapping and playback timing. Also repeat with an
unsupported requested rate to exercise the default-rate fallback.
