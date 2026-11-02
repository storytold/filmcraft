# FilmCraft Codecs Migration to craft-libs

> **Tracking Issue:** [storytold/filmcraft#3940](https://github.com/storytold/filmcraft/issues/3940)  
> **Upstream PR:** [storytold/craft-libs#504](https://github.com/storytold/craft-libs/pull/504)  
> **Author:** `el-j`  

## Overview
Pure-Rust media codecs originally hosted in FilmCraft (`filmcraft-h264enc`, `filmcraft-prores`, `filmcraft-aac`) are undergoing centralization inside `craft-libs` ([storytold/craft-libs#504](https://github.com/storytold/craft-libs/pull/504)) as the canonical One True Source for all ArtCraft applications.

### Canonical Locations in `craft-libs`
- `crates/h264enc` (`craft-h264enc`): H.264 High/Main/Baseline video encoder
- `crates/prores` (`craft-prores`): Apple ProRes 422/4444 video encoder & decoder
- `crates/aac` (`craft-aac`): AAC-LC CBR/VBR audio encoder & decoder
- `crates/codecs` (`craft-codecs`): Umbrella crate re-exporting all video and audio encoders

Once FilmCraft adopts these upstream `craft-libs` crates, `filmcraft`, `effectcraft`, and `encodercraft` will share identical, SIMD-accelerated, zero-panic codec implementations without cross-application dependencies. Local crates within FilmCraft will be deprecated or re-export the `craft-*` implementations.
