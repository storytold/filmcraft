# filmcraft-android

FilmCraft on Android: a small `apps/` crate that hosts the existing egui UI in an `android-activity`
(GameActivity) window, plus the platform seams the desktop shell normally supplies — and the
MediaCodec hardware decoder behind them.

The engine, the UI and the theme are the desktop ones; `apps/filmcraft` is not compiled, so its
`rfd`, `cpal`, `muda` and macOS/Windows/Linux platform code never enter the graph.

## What the shell does

- **Entry point** — builds as `libmain.so`, loaded with `System.loadLibrary`; `android_main` is the
  usual `android-activity` entry.
- **File pickers** — FilmCraft's `HostHooks` (`pick_files`, `pick_open_file`, `pick_open_project`)
  start the Storage Access Framework instead of `rfd`. The hook cannot block the frame, so it answers
  "nothing picked" and a worker starts `ACTION_OPEN_DOCUMENT`; the Activity copies the chosen
  documents into `<filesDir>/import/` and the paths come back over the app's own control channel as
  the *desktop* `file.import` command.
- **Saves and exports** — the engine writes plain paths, so a save answers with a scratch path in
  `<filesDir>/export/`; a worker shows `ACTION_CREATE_DOCUMENT` and copies the finished file out once
  its size has been stable for 1.5 s. Cancelling loses nothing.
- **Storage** — `AndroidApp::internal_data_path()` is the data directory (autosave, crash log,
  preferences, imports and exports all live under it).
- **Hardware decoding** — `filmcraft_platform::register()` now has an Android arm: a MediaCodec
  (`AMediaCodec`) decoder for H.264/HEVC, wrapped in the same `HybridDecoder` the macOS backend uses
  so a mid-stream failure falls back to the software decoder. Encoders, VP9/AV1 and >10-bit streams
  are declined rather than forced through.
- **Self-test** — a background thread decodes three embedded 320×240 clips (H.264, HEVC, HEVC Main
  10) through the engine's own `make_video_decoder()` path; the 10-bit clip is also decoded with the
  software decoder and compared picture by picture, which checks the P010 output conversion.
- **Fonts / touch** — the `/system/fonts` CJK face is appended to every family after the theme is in
  place; hit targets go to 40 pt. The layout is not zoomed (411 pt wide phone vs a desktop layout).

## Known gaps

- Audio output is not wired (`cpal` has an Android backend, but it needs permissions and routing).
- The system clipboard is not wired (`arboard` has no Android backend).
- Printing has no backend; the menu entry is unreachable rather than broken.
- Documents open from the app's own import directory: "save back to the original" is not wired.

## Build

```bash
cargo ndk -t arm64-v8a -o apps/filmcraft-android/jniLibs build --release -p filmcraft-android
cd apps/filmcraft-android/android && ./gradlew assembleRelease
```

`jniLibs/` and the Gradle output are gitignored; the APK is not part of this change.
