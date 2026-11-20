# FilmCraft for GoogleBook

This Android build runs the FilmCraft editor and export engine in a native app. It uses the same
project format and includes FilmCraft's effects, transitions, audio processing, fonts, and templates.
The Creator bar includes vertical, wide, and square sequences, a searchable **Create** library for
fonts, titles, generators, effects, and transitions, a caption workspace, proxy controls, and adaptive
playback quality. **Create** has previews and a Favorites filter. Open **Style packs** in the library
to customize caption colors, font, size, and the transition, and save your own reusable pack.

## Build

Install Android SDK Platform 36, Build Tools 36, NDK r28 or later, a JDK, Python 3, and the
Rust Android targets. Configure signing with a local keystore. Keep the same key for future updates.

```sh
rustup target add aarch64-linux-android x86_64-linux-android
export ANDROID_SDK_ROOT=/path/to/android-sdk
export ANDROID_NDK_HOME=/path/to/android-ndk
export FILMCRAFT_ANDROID_KEYSTORE=/path/to/release.p12
export FILMCRAFT_ANDROID_KEY_ALIAS=release
read -rs FILMCRAFT_ANDROID_KEYSTORE_PASSWORD
export FILMCRAFT_ANDROID_KEYSTORE_PASSWORD
python3 packaging/android/prepare-fonts.py target/android-creator-fonts
export CRAFT_FONTS_DIR="$PWD/target/android-creator-fonts"
export FILMCRAFT_ANDROID_VERSION_CODE=20008
bash packaging/android/build.sh
```

The update uses Android version code `20008`. Use that code when reproducing this build and
increase it for later updates. The default build contains ARM64 and x86-64. Set `FILMCRAFT_ANDROID_ABIS=arm64-v8a` or
`FILMCRAFT_ANDROID_ABIS=x86_64` to build for just one processor. Set `FILMCRAFT_ANDROID_OUT`
to override the output directory, which defaults to `target/android-apk`.

The optional creator font pack bundles Barlow Condensed Bold and Atkinson Hyperlegible Regular
with their SIL Open Font Licenses. The packaging script can also take an existing
`CRAFT_FONTS_DIR` checkout or build with FilmCraft's stock fonts.

The launcher, audio adapter, and in-app file browser are adapted from
[BartuAbiHD's Android port](https://github.com/BartuAbiHD/filmcraft/tree/aa820b0f914288a1d96262f14c1d9b303378ae7e/apps/filmcraft-android).

## Install and edit

Use GoogleBook's supported
[ADB installation flow](https://developer.android.com/develop/adaptive-apps/guides/googlebook/adb-debugging).

```sh
adb shell getprop ro.product.cpu.abilist
adb install -r FilmCraft-GoogleBook.apk
```

**Import** and **Link Media → Locate** use Android's file picker. FilmCraft copies selected files to
its Media folder so projects can reopen them after picker access expires. The progress indicator shows
how much FilmCraft has copied. When the provider reports file sizes, FilmCraft checks available storage
before copying. Use **Cancel import** to discard the current transfer.
Project open and save still use the in-app browser.

**Tools → Media Storage** reports the size of completed picker copies and unfinished transfers. Use **Clear
unfinished transfers** to reclaim incomplete imports. Completed copies stay available to Undo and
to projects saved elsewhere.

**Tools → Project backup** collects the current project and its original media into a new folder under
**Exports/Project backups**. Enable **Include proxies** to include existing proxies. To move the backup,
choose **Save ZIP to Downloads**, extract the ZIP together with its media, and open the `.fcproj` inside it.
On Android, use **Tools → Restore backup → Choose backup ZIP**. Review the storage requirement,
select **Restore project**, and open the restored project. FilmCraft keeps the original backup and
creates a new folder under **Projects**. Leave space for the ZIP copy and the extracted files.

**Export** offers Match Source, 1080p, 4K, and vertical presets. It displays the estimated size and
can use the entire sequence or the marked In/Out range. Choose burned-in captions or a separate
SRT or VTT from the first caption track. The vertical preset scales to fill the 9:16 frame.
FilmCraft publishes the files to
**Downloads/FilmCraft** and offers **Share** for the MP4. Use **Cancel export** during rendering.
If publishing fails, **Retry saving to Downloads** uses the rendered file in the Exports folder.

## Measure performance on Googlebook

On Android 10 or later, FilmCraft can try hardware MediaCodec playback for eligible progressive
8-bit 4:2:0 H.264 and HEVC media. Unsupported streams and decoder failures fall back to software.
For eligible SDR H.264 exports, select **Export → Try hardware encoding**. FilmCraft uses
software if MediaCodec declines the configuration or fails its startup test. After an encoder failure,
**Retry in software** uses the same export settings and frame range while the edit is still current.

For a repeatable comparison, use the same 10 seconds of a 1080p sequence and then a 4K sequence.
Set **Preview quality → Full**, turn proxies off, position the playhead at the same In point before
each run, and choose **Tools → Performance → Sample playback**. The panel shows displayed and dropped
frames and reports hardware-decoded frames. Compare with hardware decoding set to Off in Playback
settings. Export the same marked range once with **Try hardware encoding** off and once with it on.
**Performance** shows which encoder ran, the render time, and the export rate relative to real time.
Run the same sessions with H.264 and HEVC inputs. Use **Preview quality → Create lightweight proxies
for 4K clips** to make quarter-size H.264 copies of the active sequence's 4K sources. Toggle **Use
proxies** and repeat the playback sample at the same starting frame. Scrub back and forth across a cut
in each mode and record how quickly the preview settles.

## Googlebook release check

1. Install the signed ARM64 APK over the previous build and open a project with video and audio.
2. Start a saved MCP pairing, paste the Antigravity config, and check the three MCP status lines.
3. Ask Antigravity to inspect the timeline and propose two edits, such as a trim and an audio gain
   adjustment. Compare both versions in **Review AI edit**, apply the proposal, and undo it.
4. Export an In/Out range with a separate subtitle file. Open both files from **Downloads/FilmCraft**.
5. Back up the project, restore its ZIP, and play the restored timeline.
6. Run the playback and export comparisons above and record the selected codec, preview quality,
   proxy setting, dropped frames, and export time.

## Pair an MCP client

Open **MCP Config** in the toolbar and start the embedded MCP server. Choose Antigravity, Codex, or Claude Code
and copy its config directly from FilmCraft. **Create saved local pairing** keeps the same config across
server restarts, with approval required after a project switch. Every proposal is reviewed in the app
and applies as one undoable edit. See the [MCP setup guide](../../packaging/android/MCP.md).
