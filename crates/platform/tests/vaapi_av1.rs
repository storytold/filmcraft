//! VA-API AV1 decoding against the software decoder (Linux): the same pictures in the same order,
//! bit for bit, over whole streams and after seeks. Skips without ffmpeg (fixture generator), an
//! AV1 encoder or a driver that decodes AV1.
#![cfg(target_os = "linux")]

#[path = "it/common/mod.rs"]
mod common;

use std::path::{Path, PathBuf};

use common::*;
use filmcraft_codecs::VideoDecoder;
use filmcraft_codecs::hw::FrameStreamInfo;
use filmcraft_platform::vaapi::VaAv1Decoder;

/// An SVT-AV1 stream with hierarchical B pyramids (hidden frames shown later) and the given options.
fn av1_fixture(ff: &Path, name: &str, pix_fmt: &str, params: &str) -> Option<PathBuf> {
    fixture(
        ff,
        name,
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=640x360:r=24:d=4,noise=alls=8:allf=t",
            "-c:v",
            "libsvtav1",
            "-preset",
            "10",
            "-crf",
            "35",
            "-g",
            "24",
            "-pix_fmt",
            pix_fmt,
            "-svtav1-params",
            params,
        ],
    )
}

fn hardware(s: &Stream) -> Option<VaAv1Decoder> {
    let info = FrameStreamInfo::from_entry(&s.entry)?.ok()?;
    match VaAv1Decoder::new(info) {
        Ok(d) => Some(d),
        Err(why) => {
            eprintln!("SKIPPED: no VA-API AV1 decoder here: {why}");
            None
        }
    }
}

fn software(s: &Stream) -> Box<dyn VideoDecoder> {
    filmcraft_codecs::software_video_decoder(&s.entry).unwrap()
}

fn parity(name: &str, pix_fmt: &str, params: &str) {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let Some(path) = av1_fixture(&ff, name, pix_fmt, params) else { return };
    let s = read_stream(&path);
    let Some(mut hw) = hardware(&s) else { return };
    let reference = decode_all(software(&s).as_mut(), &s.samples);
    assert!(reference.len() > 90, "{name}: {} pictures", reference.len());
    assert_same(&format!("{name}: whole stream"), &decode_all(&mut hw, &s.samples), &reference);

    // a seek: restart at the second key frame after a reset
    let key = s.sync.iter().enumerate().filter(|&(_, &k)| k).map(|(i, _)| i).nth(1).unwrap();
    hw.reset();
    let mut sw = software(&s);
    sw.reset();
    assert_same(&format!("{name}: after a seek"), &decode_all(&mut hw, &s.samples[key..]), &decode_all(sw.as_mut(), &s.samples[key..]));
}

#[test]
fn eight_bit_matches_the_software_decoder() {
    parity("av1_8.mp4", "yuv420p", "tune=0");
}

#[test]
fn ten_bit_matches_the_software_decoder() {
    parity("av1_10.mp4", "yuv420p10le", "tune=0");
}

#[test]
fn several_tiles_match_the_software_decoder() {
    parity("av1_tiles.mp4", "yuv420p", "tile-columns=1:tile-rows=1");
}

/// A driver error or a damaged sample ends in an error, never a crash; the hybrid then falls back.
#[test]
fn damaged_samples_are_errors_not_crashes() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let Some(path) = av1_fixture(&ff, "av1_8.mp4", "yuv420p", "tune=0") else { return };
    let s = read_stream(&path);
    let Some(mut hw) = hardware(&s) else { return };
    for (i, (sample, pts)) in s.samples.iter().take(12).enumerate() {
        for cut in [0, 1, 2, 5, sample.len() / 2] {
            let _ = hw.decode(&sample[..cut.min(sample.len())], *pts);
        }
        let mut flipped = sample.clone();
        if let Some(b) = flipped.get_mut(i + 1) {
            *b ^= 0x55;
        }
        let _ = hw.decode(&flipped, *pts);
        hw.reset();
    }
}

/// `HybridDecoder` takes over from a decoder that fails mid-stream, without losing a picture.
#[test]
fn a_failure_mid_stream_continues_in_software() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let Some(path) = av1_fixture(&ff, "av1_8.mp4", "yuv420p", "tune=0") else { return };
    let s = read_stream(&path);
    let Some(mut hw) = hardware(&s) else { return };
    hw.fail_after(30);
    let info = FrameStreamInfo::from_entry(&s.entry).unwrap().unwrap();
    let mut hybrid = filmcraft_platform::HybridDecoder::new(Box::new(hw), s.entry.clone(), info);
    let reference = decode_all(software(&s).as_mut(), &s.samples);
    assert_same("failure at sample 30", &decode_all(&mut hybrid, &s.samples), &reference);
}

/// Film grain comes out as the software decoder's, or the stream goes on in software: either way
/// the pictures are the same.
#[test]
fn film_grain_matches_the_software_decoder() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let Some(path) = av1_fixture(&ff, "av1_grain.mp4", "yuv420p", "film-grain=10") else { return };
    let s = read_stream(&path);
    let Some(hw) = hardware(&s) else { return };
    let info = FrameStreamInfo::from_entry(&s.entry).unwrap().unwrap();
    let mut hybrid = filmcraft_platform::HybridDecoder::new(Box::new(hw), s.entry.clone(), info);
    let reference = decode_all(software(&s).as_mut(), &s.samples);
    assert_same("film grain", &decode_all(&mut hybrid, &s.samples), &reference);
}
