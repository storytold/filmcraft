//! MP4 AC-3 / E-AC-3 channel layout against ffmpeg (#1701). ISO-BMFF mandates a stereo channel
//! count in the sample entry for both codecs; the real layout is in the `dac3` / `dec3` descriptor.
//! Each channel carries a distinct tone, so a dropped centre/surround sample fails the oracle.

use crate::common;

use std::fs;
use std::io::Cursor;

use common::*;
use filmcraft_isobmff::{Brand, CodecConfig, Mp4Writer, TrackConfig, WriteSample, WriterOptions};

const RATE: u32 = 48_000;

fn test_file(ff: &std::path::Path, codec: &str) -> std::path::PathBuf {
    fixture(
        ff,
        &format!("layout_51_{codec}.mp4"),
        &[
            "-f",
            "lavfi",
            "-i",
            "aevalsrc=0.25*sin(2*PI*300*t)|0.25*sin(2*PI*400*t)|0.25*sin(2*PI*500*t)|0.25*sin(2*PI*60*t)|0.25*sin(2*PI*700*t)|0.25*sin(2*PI*800*t):s=48000:d=0.5:c=5.1",
            "-c:a",
            codec,
            "-b:a",
            "384k",
        ],
    )
    .unwrap_or_else(|| panic!("ffmpeg can't encode {codec}"))
}

fn rewrap(path: &std::path::Path, codec: &str) -> Vec<u8> {
    let data = fs::read(path).unwrap();
    let file = filmcraft_isobmff::open(data.as_slice()).unwrap();
    let track = file.tracks.iter().find(|t| t.kind == filmcraft_isobmff::TrackKind::Audio).unwrap();
    let mut entry = track.entries[0].clone();
    entry.audio.as_mut().unwrap().channels = 2;
    if codec == "fallback"
        && let CodecConfig::Ac3 { dac3 } = &mut entry.codec
    {
        dac3.clear();
    }
    let mut mux = Mp4Writer::new(Cursor::new(Vec::new()), WriterOptions::new(Brand::Mp4)).unwrap();
    let ti = mux.add_track(TrackConfig::new(entry, track.timescale)).unwrap();
    for s in &track.samples {
        let au = data[s.offset as usize..s.offset as usize + s.size as usize].to_vec();
        mux.write_sample(ti, WriteSample { data: &au, duration: s.duration, composition_offset: 0, is_sync: true }).unwrap();
    }
    mux.finish().unwrap().into_inner()
}

#[test]
fn ac3_and_eac3_mp4_report_and_decode_all_six_channels() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    for codec in ["ac3", "eac3"] {
        let path = test_file(&ff, codec);
        let data = bytes(&path);
        let source = filmcraft_codecs::open_bytes(&format!("{codec}.mp4"), data.clone()).unwrap();
        let info = source.info().audio().unwrap();
        assert_eq!(info.sample_rate, RATE, "{codec} sample rate");
        assert_eq!(info.channels, 6, "{codec} channels");
        let want = ffmpeg_audio_f32(&ff, &path, &[]);
        let ours = source.audio(0, want.len() / 6, RATE).unwrap().channels;
        assert_eq!(ours.len(), 6, "{codec} decoded channels");
        // Skip decoder warm-up, then require a strong oracle match in every channel (including
        // centre/dialogue, LFE and both surrounds).
        for (c, channel) in ours.iter().enumerate() {
            let (mut signal, mut error) = (0.0_f64, 0.0_f64);
            for i in 1024..channel.len().saturating_sub(1024) {
                let reference = f64::from(want[i * 6 + c]);
                signal += reference * reference;
                error += (f64::from(channel[i]) - reference).powi(2);
            }
            let snr = 10.0 * (signal / error.max(1e-30)).log10();
            assert!(snr > 20.0, "{codec} channel {c}: {snr:.1} dB");
        }
    }
}

#[test]
fn descriptor_layout_survives_the_mandated_stereo_entry() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let out = rewrap(&test_file(&ff, "ac3"), "ac3");
    let src = filmcraft_codecs::open_bytes("rewrapped-ac3.mp4", out.into()).unwrap();
    assert_eq!(src.info().audio().unwrap().channels, 6);
}

#[test]
fn missing_descriptor_falls_back_to_a_primary_frame() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let out = rewrap(&test_file(&ff, "ac3"), "fallback");
    let src = filmcraft_codecs::open_bytes("fallback-ac3.mp4", out.into()).unwrap();
    assert_eq!(src.info().audio().unwrap().channels, 6);
}
