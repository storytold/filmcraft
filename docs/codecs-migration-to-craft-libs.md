# FilmCraft Codecs Migration to craft-libs

> **Tracking Issue:** [storytold/filmcraft#3940](https://github.com/storytold/filmcraft/issues/3940)  
> **Upstream PRs:** [storytold/craft-libs#504](https://github.com/storytold/craft-libs/pull/504) (Video Codecs), [storytold/craft-libs#443-#444](https://github.com/storytold/craft-libs/pull/443) (AAC Audio)  
> **Author:** `el-j`  

## Overview
Pure-Rust media codecs originally hosted in FilmCraft (`filmcraft-h264enc`, `filmcraft-prores`, `filmcraft-aac`) are undergoing centralization inside `craft-libs` as the canonical One True Source for all ArtCraft applications.

### Canonical Locations in `craft-libs`
- `crates/h264enc` (`craft-h264enc`): H.264 High/Main/Baseline video encoder ([#504](https://github.com/storytold/craft-libs/pull/504))
- `crates/prores` (`craft-prores`): Apple ProRes 422/4444 video encoder & decoder ([#504](https://github.com/storytold/craft-libs/pull/504))
- `crates/audio` (`craft-audio`): AAC-LC CBR/VBR audio encoder & decoder ([#443](https://github.com/storytold/craft-libs/pull/443), [#444](https://github.com/storytold/craft-libs/pull/444))
- `crates/codecs` (`craft-codecs`): Umbrella crate re-exporting video codecs behind features ([#504](https://github.com/storytold/craft-libs/pull/504))

Once FilmCraft adopts these upstream `craft-libs` crates, `filmcraft`, `effectcraft`, and `encodercraft` will share identical, SIMD-accelerated, zero-panic codec implementations without cross-application dependencies. Local crates within FilmCraft will be deprecated or re-export the `craft-*` implementations.
