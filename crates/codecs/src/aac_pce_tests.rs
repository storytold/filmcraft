//! AAC whose channels are given by a program config element (#4258).
//!
//! FFmpeg writes `5.1(side)` AAC as channel configuration 0 with a PCE, and older versions leave 2
//! in the MP4 sample entry. The probe took that 2, so only two of the six decoded channels reached
//! the mix. The stream here is our encoder's 5.1 with its AudioSpecificConfig rewritten to a PCE
//! naming the same elements, muxed with a sample entry of 2 channels.

use std::io::Cursor;
use std::sync::Arc;

use filmcraft_aac::{AudioSpecificConfig, ProgramConfig};
use filmcraft_isobmff::{Brand, Mp4Writer, SampleEntry, TrackConfig, WriteSample, WriterOptions};
use filmcraft_media::MediaSource;

use crate::Mp4Source;

const RATE: u32 = 48_000;
/// One tone per channel, in the encoder's element order: C, L R, Ls Rs, LFE.
const TONES: [f32; 6] = [500.0, 300.0, 400.0, 700.0, 800.0, 60.0];

/// A second of the six tones as AAC 5.1 (channel configuration 6).
fn aac_51() -> (Vec<u8>, Vec<Vec<u8>>) {
    let chans: Vec<Vec<f32>> =
        TONES.iter().map(|&hz| (0..RATE as usize).map(|i| 0.25 * (std::f32::consts::TAU * hz * i as f32 / RATE as f32).sin()).collect()).collect();
    let refs: Vec<&[f32]> = chans.iter().map(Vec::as_slice).collect();
    let mut enc = filmcraft_aac::Encoder::new(filmcraft_aac::EncoderConfig::cbr(RATE, 6, 384_000)).unwrap();
    let mut units = enc.encode(&refs);
    units.extend(enc.flush());
    (enc.audio_specific_config(), units)
}

/// `asc` (configuration 6) as channel configuration 0 with a program config element naming the
/// same elements and tags, so the access units decode unchanged.
fn as_pce(asc: &[u8]) -> Vec<u8> {
    let mut c = AudioSpecificConfig::parse(asc).unwrap();
    assert_eq!(c.channel_config, 6);
    c.pce = Some(ProgramConfig {
        object_type: 1,
        sf_index: c.sf_index,
        front: vec![(false, 0), (true, 0)],
        side: vec![(true, 1)],
        lfe: vec![0],
        ..Default::default()
    });
    c.channel_config = 0;
    c.to_bytes()
}

/// An MP4 holding `units`, its sample entry claiming `entry_channels`.
fn mp4(asc: &[u8], entry_channels: u32, units: &[Vec<u8>]) -> Arc<[u8]> {
    let mut w = Mp4Writer::new(Cursor::new(Vec::new()), WriterOptions::new(Brand::Mp4)).unwrap();
    let t = w.add_track(TrackConfig::new(SampleEntry::aac(asc.to_vec(), entry_channels, RATE), RATE)).unwrap();
    for u in units {
        w.write_sample(t, WriteSample { data: u, duration: 1024, composition_offset: 0, is_sync: true }).unwrap();
    }
    w.finish().unwrap().into_inner().into()
}

/// The level of `hz` in `x` (Goertzel), relative to a full tone of the test's amplitude.
fn level(x: &[f32], hz: f32) -> f32 {
    let w = std::f32::consts::TAU * hz / RATE as f32;
    let (mut s1, mut s2) = (0.0f32, 0.0f32);
    for &v in x {
        let s0 = v + 2.0 * w.cos() * s1 - s2;
        (s2, s1) = (s1, s0);
    }
    (s1 * s1 + s2 * s2 - 2.0 * w.cos() * s1 * s2).sqrt() / (0.125 * x.len() as f32)
}

#[test]
fn a_pce_names_six_channels_whatever_the_sample_entry_says() {
    let (asc, units) = aac_51();
    let pce = as_pce(&asc);
    for entry in [2, 6] {
        let src = Mp4Source::open("pce.mp4", mp4(&pce, entry, &units)).unwrap();
        assert_eq!(src.info().audio_streams[0].channels, 6, "the PCE's six channels, not the sample entry's {entry}");
        // Every channel is read, each holding its own tone, in the order the PCE names them.
        let b = src.audio(4096, 16_384, RATE).unwrap();
        assert_eq!(b.channels.len(), 6);
        for (c, &hz) in b.channels.iter().zip(&TONES) {
            assert!(level(c, hz) > 0.5, "the {hz} Hz channel is read (sample entry {entry})");
        }
    }
}

/// A configuration-0 AudioSpecificConfig that the PCE can't be read from leaves the sample
/// entry's count, as before.
#[test]
fn an_unreadable_pce_falls_back_to_the_sample_entry() {
    let (asc, units) = aac_51();
    let mut pce = as_pce(&asc);
    pce.truncate(2);
    let src = Mp4Source::open("cut.mp4", mp4(&pce, 2, &units)).unwrap();
    assert_eq!(src.info().audio_streams[0].channels, 2);
}
