# FilmCraft Android

Android mobile runner for FilmCraft using `eframe` and `android-activity`.

## Prerequisites

1. **Android NDK** (version r25 or newer) and Android SDK.
2. Rust target for your test device / emulator architecture:
   ```bash
   rustup target add aarch64-linux-android
   # or x86_64 for emulators:
   rustup target add x86_64-linux-android
   ```
3. `cargo-apk`:
   ```bash
   cargo install cargo-apk
   ```

## Building & Running

Run directly on a connected device or emulator with:

```bash
cargo apk run -p filmcraft-android --target aarch64-linux-android
```
