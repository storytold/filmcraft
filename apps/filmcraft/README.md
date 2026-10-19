# FilmCraft desktop host

The desktop host connects the editor to native windows, audio devices, file dialogs and the
control server. See [contributing](../../docs/contributing.md) for build and run instructions.

## Razer 7.1 audio output

Settings ▸ Audio Hardware ▸ **7.1 surround compatibility (Windows)** controls a WASAPI
workaround that prefers supported signed 32-bit PCM over float output:

- **Auto** (default): match the case-insensitive names `7.1 Surround Sound` and
  `Razer 7.1 Surround Sound`, either alone or as a parenthesized suffix such as
  `Speakers (7.1 Surround Sound)`. Other names keep the existing float preference.
- **Off**: disable the workaround even when the name matches, restoring the previous format
  selection including its default-rate fallback.
- **On**: prefer supported PCM for the selected WASAPI output regardless of its name, useful
  for renamed endpoints. This is a global audio preference, so set it back to Auto or Off
  when switching to an output that does not need it.

The setting is persisted and available through `prefs.get` / `prefs.set` as
`audioHardware.surroundCompatibility` (`auto`, `on`, `off`), with automation id
`settings.audioHardware.surroundCompatibility`. Apply it with OK, then restart playback.
All three modes leave non-WASAPI hosts unchanged.

The Razer virtual endpoint commonly appears without the vendor name. This is a workaround for the float
distortion reported in [FilmCraft #2411](https://github.com/storytold/filmcraft/issues/2411)
and [CPAL #892](https://github.com/RustAudio/cpal/issues/892#issuecomment-2364194193).
The display-name match is a heuristic, **not a reliable vendor or hardware identifier**;
another driver's endpoint could have the same name. Such users can select Off. We do not yet
have hardware IDs or a uniquely identifying advertised-format signature from the affected
machine, and cannot claim that every matching driver has been validated. Renamed endpoints
can select On instead of relying on automatic detection.

Selection preserves the default channel count and requested sample rate when supported. If the
requested rate is unavailable, the workaround also applies at the default rate. If no matching
PCM configuration is advertised, selection falls back to the existing supported formats; no
unsupported format is forced. The mixer remains float internally and uses the existing PCM
conversion at the device boundary.

`cargo test -p filmcraft audio::tests` covers endpoint matching, overrides, format/rate/channel
selection, fallbacks and eight-channel PCM conversion without requiring a physical surround device.
The engine's `surround_compatibility_migrates_persists_and_validates` test covers persistence,
migration and validation; the UI's `surround_compatibility_dropdown_applies_and_cancels` test
exercises the setting through control-channel clicks.
These tests cannot establish whether the Razer driver distortion is gone. Hardware validation
still requires Windows with Razer 7.1: play the bundled demo through that endpoint, compare
with the ordinary output and with compatibility Off, and check stereo mapping and playback timing.
Also repeat with an unsupported requested rate to exercise the default-rate fallback.
