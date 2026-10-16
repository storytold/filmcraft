//! Synthetic surround signals, with the ISO BMFF ChannelCount fixed to the
//! mandated value 2 (ETSI TS 102 366 Annex F). FFmpeg is only an external oracle.
mod common;
use common::*;

const SIGNAL: &str =
    "aevalsrc=exprs=0.25*sin(2*PI*300*t)|0.25*sin(2*PI*400*t)|0.25*sin(2*PI*500*t)|0.25*sin(2*PI*60*t)|0.25*sin(2*PI*700*t)|0.25*sin(2*PI*800*t):s=48000:c=5.1";

fn check(codec: &str) {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let name = format!("layout_51_{codec}.mp4");
    let file = fixture(&ff, &name, &["-f", "lavfi", "-i", SIGNAL, "-t", "1", "-c:a", codec, "-b:a", "384k"]).unwrap();
    let mut data = std::fs::read(&file).unwrap();
    let fourcc: &[u8] = if codec == "ac3" { b"ac-3" } else { b"ec-3" };
    let at = data.windows(4).position(|x| x == fourcc).unwrap();
    // AudioSampleEntry: fourcc then reserved/data-reference, version/revision/vendor,
    // followed by ChannelCount. Other fields and all compressed samples stay intact.
    data[at + 20..at + 22].copy_from_slice(&2u16.to_be_bytes());
    for absent in [false, true] {
        let mut fallback = data.clone();
        let descriptor: &[u8] = if codec == "ac3" { b"dac3" } else { b"dec3" };
        let at = fallback.windows(4).position(|x| x == descriptor).unwrap();
        if absent {
            fallback[at..at + 4].copy_from_slice(b"junk");
        } else {
            fallback[at + 4..at + 7].fill(0xff);
        }
        let source = filmcraft_codecs::open_bytes(&name, fallback.into()).unwrap();
        assert_eq!(source.info().audio().unwrap().channels, 6, "{codec} frame-header fallback");
        assert_eq!(source.audio(0, 4800, 48_000).unwrap().channels.len(), 6);
    }
    let source = filmcraft_codecs::open_bytes(&name, data.into()).unwrap();
    let info = source.info().audio().unwrap();
    assert_eq!((info.sample_rate, info.channels), (48_000, 6), "{codec}");
    let want = ffmpeg_audio_f32(&ff, &file, &[]);
    let ours = source.audio(0, want.len() / 6, 48_000).unwrap().channels;
    assert_eq!(ours.len(), 6);
    // Includes centre/dialogue and surrounds, which were silently discarded before.
    for (c, channel) in ours.iter().enumerate() {
        let (mut signal, mut error) = (0.0f64, 0.0f64);
        for i in 1000..channel.len() - 1000 {
            let reference = f64::from(want[i * 6 + c]);
            signal += reference * reference;
            error += (f64::from(channel[i]) - reference).powi(2);
        }
        let snr = 10.0 * (signal / error.max(1e-30)).log10();
        assert!(snr > 50.0, "{codec} channel {c}: {snr} dB");
    }
    let window = source.audio(12_000, 4800, 48_000).unwrap();
    assert_eq!(window.channels.len(), 6);
    for c in 0..6 {
        for (k, value) in window.channels[c].iter().enumerate() {
            assert!((value - ours[c][12_000 + k]).abs() <= 1e-6);
        }
    }
}

#[test]
fn ac3_mp4_preserves_all_six_channels() {
    check("ac3");
}
#[test]
fn eac3_mp4_preserves_all_six_channels() {
    check("eac3");
}

#[test]
fn ec3_with_ac3_primary_preserves_surround_with_or_without_descriptor() {
    use filmcraft_isobmff::{Brand, CodecConfig, FourCc, Mp4Writer, TrackConfig, WriteSample, WriterOptions};
    let ff = filmcraft_testkit::require_ffmpeg!();
    let file = fixture(&ff, "layout_51_ac3.mp4", &["-f", "lavfi", "-i", SIGNAL, "-t", "1", "-c:a", "ac3", "-b:a", "384k"]).unwrap();
    let bytes = std::fs::read(file).unwrap();
    let parsed = filmcraft_isobmff::open(bytes.as_slice()).unwrap();
    let track = &parsed.tracks[0];
    let mut w = filmcraft_bitstream::BitWriter::new();
    for (value, count) in [(384, 13), (0, 3), (0, 2), (8, 5), (0, 5), (7, 3), (1, 1), (0, 8)] {
        w.write_bits(value, count);
    }
    let descriptor = w.finish();
    for dec3 in [descriptor, Vec::new()] {
        let mut entry = track.entries[0].clone();
        entry.format = FourCc(*b"ec-3");
        entry.codec = CodecConfig::Eac3 { dec3 };
        entry.audio.as_mut().unwrap().channels = 2;
        let mut out = Mp4Writer::new(std::io::Cursor::new(Vec::new()), WriterOptions::new(Brand::Mp4)).unwrap();
        let ti = out.add_track(TrackConfig::new(entry, track.timescale)).unwrap();
        for sample in &track.samples {
            out.write_sample(
                ti,
                WriteSample {
                    data: &bytes[sample.offset as usize..sample.offset as usize + sample.size as usize],
                    duration: sample.duration,
                    composition_offset: 0,
                    is_sync: true,
                },
            )
            .unwrap();
        }
        let source = filmcraft_codecs::open_bytes("ac3-primary-ec3.mp4", out.finish().unwrap().into_inner().into()).unwrap();
        assert_eq!(source.info().audio().unwrap().channels, 6);
        let audio = source.audio(0, 4800, 48_000).unwrap();
        assert_eq!(audio.channels.len(), 6);
        assert!(audio.channels[2].iter().any(|x| x.abs() > 0.1));
    }
}
