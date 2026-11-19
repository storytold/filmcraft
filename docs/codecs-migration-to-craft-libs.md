# FilmCraft Media Codec Migration to craft-libs

> **Status:** Completed for core media codecs (H.264, ProRes, DNx, AAC) · **Last updated:** 2026-11-19  
> **Tracking Issues:** [storytold/filmcraft#3940](https://github.com/storytold/filmcraft/issues/3940), [storytold/craft-libs#778](https://github.com/storytold/craft-libs/issues/778)  
> **Canonical Upstream:** [storytold/craft-libs](https://github.com/storytold/craft-libs) (`crates/video`, `crates/audio`)  

## Overview

Pure-Rust media codecs originally developed across FilmCraft and sibling applications have been centralized into `craft-libs` as shared libraries across the ArtCraft ecosystem.

FilmCraft's local codec implementations (`filmcraft-h264enc`, `filmcraft-prores`, `filmcraft-aac`) have been fully decommissioned. Codec decoding and encoding are now consumed directly from the centralized `craft-video` and `craft-audio` packages.

## Canonical Locations and Workspace Pins

### 1. Video Codecs: `craft-video` (`crates/video`)
- **Package:** `craft-video = "0.1.0"` in [craft-libs](https://github.com/storytold/craft-libs) (`crates/video`)
- **Features Consumed in FilmCraft:**
  - `h264-decode` and `h264-encode`: H.264 / AVC video decoding and encoding
  - `prores`: Apple ProRes 422 and 4444 decoding and encoding
  - `dnx`: Avid DNxHD / DNxHR decoding and encoding
  - `hevc-decode`, `vp9-decode`, `av1-decode`, `vp8-decode`, `mpeg2`, `apv`, `threads`
- **Consumer Manifests:**
  - [`Cargo.toml`](../Cargo.toml): Workspace dependency definition pinned to `craft-libs`
  - [`crates/codecs/Cargo.toml`](../crates/codecs/Cargo.toml): Decoding and sequence playback pipeline
  - [`crates/export/Cargo.toml`](../crates/export/Cargo.toml): Sequence render, encoding, and muxing pipeline

### 2. Audio Codecs: `craft-audio` (`crates/audio`)
- **Package:** `craft-audio = "0.1.0"` in [craft-libs](https://github.com/storytold/craft-libs) (`crates/audio`)
- **Features Consumed in FilmCraft:**
  - `aac-decode` and `aac-encode`: Advanced Audio Coding (AAC-LC) decoding and encoding
  - `ac3-decode`, `mpa-decode`, `mp3-decode`, `opus-decode`, `flac-encode`, `mp3-encode`
- **Consumer Manifests:**
  - [`Cargo.toml`](../Cargo.toml): Workspace dependency definition
  - [`crates/codecs/Cargo.toml`](../crates/codecs/Cargo.toml): Audio demuxing and decoding pipeline
  - [`crates/export/Cargo.toml`](../crates/export/Cargo.toml): Audio track mixdown and encoding pipeline

## Cutover Status

- [x] **H.264 Video Encode & Decode:** Local `filmcraft-h264enc` removed; consumed via `craft-video` with `h264-encode` / `h264-decode`.
- [x] **ProRes Video Encode & Decode:** Local `filmcraft-prores` removed; consumed via `craft-video` with `prores`.
- [x] **DNxHD / DNxHR:** Consumed via `craft-video` with `dnx`.
- [x] **AAC Audio Encode & Decode:** Local `filmcraft-aac` removed; consumed via `craft-audio` with `aac-encode` / `aac-decode`.
- [x] **Local Crates Cleaned Up:** FilmCraft no longer maintains duplicate local codec crates in `crates/`.
