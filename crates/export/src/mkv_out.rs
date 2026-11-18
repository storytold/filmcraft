//! AV1 Matroska output (RFC 9559 and the Matroska codec mappings).
//! One block per cluster keeps timestamps exact without a reorder buffer. Cues and a patched
//! SeekHead provide seeking; Opus CodecDelay and DiscardPadding preserve the mixed sample count.
use std::collections::VecDeque;
use std::io::{Seek, SeekFrom, Write};

use craft_media::matroska::{
    ebml::{write_id, write_size},
    element_ids as id,
};
use filmcraft_isobmff::{CodecConfig, SampleEntry};

use crate::{AudioEncoder, ColorSignal, EncodedPacket, ExportError, ExportSettings, Out, Result};

const SCALE: i64 = 1000; // microsecond timestamps; DiscardPadding itself remains in nanoseconds
const SEEK_SPACE: usize = 128;

fn el(out: &mut Vec<u8>, tag: u32, data: &[u8]) {
    write_id(out, tag);
    write_size(out, data.len() as u64, 1);
    out.extend_from_slice(data);
}
fn uint(out: &mut Vec<u8>, tag: u32, value: u64) {
    let bytes = value.to_be_bytes();
    el(out, tag, &bytes[(value.leading_zeros() as usize / 8).min(7)..]);
}
fn float(out: &mut Vec<u8>, tag: u32, value: f64) {
    el(out, tag, &value.to_be_bytes());
}
fn io(e: impl std::fmt::Display) -> ExportError {
    ExportError::Io(format!("Matroska: {e}"))
}
fn ns(ticks: i128, scale: u32) -> Result<i64> {
    let n = ticks.saturating_mul(1_000_000_000) / i128::from(scale.max(1));
    i64::try_from(n).map_err(io)
}
fn void(out: &mut Vec<u8>, size: usize) {
    out.push(0xec);
    out.push(0x80 | (size - 2) as u8);
    out.resize(out.len() + size - 2, 0);
}

struct Audio {
    rate: u32,
    priming: u32,
    total: u64,
    written: u64,
}

pub(crate) struct MkvMux {
    out: Out,
    segment: u64,
    seek: u64,
    timescale: u32,
    video_start: i64,
    video_ticks: i64,
    cues: Vec<(u64, u64)>,
    audio: Option<Audio>,
    pending_audio: VecDeque<(i64, Vec<u8>, i64)>,
    pending_bytes: usize,
    last_video: Option<i64>,
}

pub(crate) struct Setup<'a> {
    pub settings: &'a ExportSettings,
    pub video: SampleEntry,
    pub timescale: u32,
    pub video_start: i64,
    pub frame_duration: u32,
    pub duration_ns: i64,
    pub audio: Option<(&'a dyn AudioEncoder, u32, u64)>,
}

fn colour(signal: ColorSignal, bits: u64) -> Vec<u8> {
    let mut c = Vec::new();
    for (tag, value) in [
        (id::MATRIX_COEFFICIENTS, u64::from(signal.matrix)),
        (id::BITS_PER_CHANNEL, bits),
        (id::RANGE, 1),
        (id::PRIMARIES, u64::from(signal.primaries)),
        (id::TRANSFER_CHARACTERISTICS, u64::from(signal.transfer)),
    ] {
        uint(&mut c, tag, value);
    }
    if let Some((md, (cll, fall))) = signal.static_metadata() {
        uint(&mut c, id::MAX_CLL, u64::from(cll));
        uint(&mut c, id::MAX_FALL, u64::from(fall));
        let mut m = Vec::new();
        // ISOBMFF orders its primaries G,B,R; Matroska names R,G,B explicitly.
        for (tag, value) in [
            (id::PRIMARY_R_X, md.primaries[2].0),
            (id::PRIMARY_R_Y, md.primaries[2].1),
            (id::PRIMARY_G_X, md.primaries[0].0),
            (id::PRIMARY_G_Y, md.primaries[0].1),
            (id::PRIMARY_B_X, md.primaries[1].0),
            (id::PRIMARY_B_Y, md.primaries[1].1),
            (id::WHITE_POINT_X, md.white_point.0),
            (id::WHITE_POINT_Y, md.white_point.1),
        ] {
            float(&mut m, tag, f64::from(value) / 50000.0);
        }
        float(&mut m, id::LUMINANCE_MAX, md.max_nits());
        float(&mut m, id::LUMINANCE_MIN, f64::from(md.min_luminance) / 10000.0);
        el(&mut c, id::MASTERING_METADATA, &m);
    }
    c
}

impl MkvMux {
    pub fn new(s: Setup<'_>) -> Result<Self> {
        let CodecConfig::Av1(cfg) = &s.video.codec else {
            return Err(io("Matroska export needs AV1"));
        };
        let video = s.video.video.as_ref().ok_or_else(|| io("missing AV1 dimensions"))?;
        let mut header = Vec::new();
        for (tag, value) in [
            (id::EBML_VERSION, 1),
            (id::EBML_READ_VERSION, 1),
            (id::EBML_MAX_ID_LENGTH, 4),
            (id::EBML_MAX_SIZE_LENGTH, 8),
            (id::DOC_TYPE_VERSION, 4),
            (id::DOC_TYPE_READ_VERSION, 2),
        ] {
            uint(&mut header, tag, value);
        }
        el(&mut header, id::DOC_TYPE, b"matroska");
        let mut bytes = Vec::new();
        el(&mut bytes, id::EBML, &header);
        write_id(&mut bytes, id::SEGMENT);
        bytes.extend_from_slice(&[0x01, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);
        let segment = bytes.len() as u64;
        let seek = segment;
        void(&mut bytes, SEEK_SPACE);
        let mut info = Vec::new();
        uint(&mut info, id::TIMESTAMP_SCALE, SCALE as u64);
        float(&mut info, id::DURATION, s.duration_ns as f64 / SCALE as f64);
        el(&mut info, id::MUXING_APP, b"FilmCraft");
        el(&mut info, id::WRITING_APP, b"FilmCraft");
        el(&mut bytes, id::INFO, &info);
        let mut tracks = Vec::new();
        let mut v = Vec::new();
        uint(&mut v, id::TRACK_NUMBER, 1);
        uint(&mut v, id::TRACK_UID, 1);
        uint(&mut v, id::TRACK_TYPE, 1);
        el(&mut v, id::CODEC_ID, b"V_AV1");
        el(&mut v, id::CODEC_PRIVATE, &cfg.to_bytes());
        uint(&mut v, id::DEFAULT_DURATION, ns(i128::from(s.frame_duration), s.timescale)? as u64);
        let mut visual = Vec::new();
        uint(&mut visual, id::PIXEL_WIDTH, u64::from(video.width));
        uint(&mut visual, id::PIXEL_HEIGHT, u64::from(video.height));
        el(&mut visual, id::COLOUR, &colour(s.settings.signal, if cfg.high_bitdepth { 10 } else { 8 }));
        el(&mut v, id::VIDEO, &visual);
        el(&mut tracks, id::TRACK_ENTRY, &v);
        let mut audio = None;
        if let Some((encoder, rate, total)) = s.audio {
            let entry = encoder.sample_entry();
            let (codec, private, opus) = match &entry.codec {
                CodecConfig::Opus(c) => ("A_OPUS", c.opus_head(), true),
                CodecConfig::Aac(c) => ("A_AAC", c.asc.clone(), false),
                CodecConfig::Flac(c) => {
                    let mut b = b"fLaC".to_vec();
                    b.extend_from_slice(&c.metadata_blocks);
                    ("A_FLAC", b, false)
                }
                _ => return Err(io("Matroska AV1 supports AAC, Opus or FLAC audio")),
            };
            let mut a = Vec::new();
            uint(&mut a, id::TRACK_NUMBER, 2);
            uint(&mut a, id::TRACK_UID, 2);
            uint(&mut a, id::TRACK_TYPE, 2);
            el(&mut a, id::CODEC_ID, codec.as_bytes());
            el(&mut a, id::CODEC_PRIVATE, &private);
            uint(&mut a, id::CODEC_DELAY, ns(i128::from(encoder.priming()), rate)? as u64);
            if opus {
                uint(&mut a, id::SEEK_PRE_ROLL, 80_000_000);
            }
            let mut sound = Vec::new();
            float(&mut sound, id::SAMPLING_FREQUENCY, f64::from(rate));
            uint(&mut sound, id::CHANNELS, u64::from(entry.audio.as_ref().ok_or_else(|| io("missing audio parameters"))?.channels));
            el(&mut a, id::AUDIO, &sound);
            el(&mut tracks, id::TRACK_ENTRY, &a);
            audio = Some(Audio { rate, priming: encoder.priming(), total, written: 0 });
        }
        el(&mut bytes, id::TRACKS, &tracks);
        let mut out = Out::create(s.settings)?;
        out.write_all(&bytes).map_err(io)?;
        Ok(Self {
            out,
            segment,
            seek,
            timescale: s.timescale,
            video_start: s.video_start,
            video_ticks: 0,
            cues: Vec::new(),
            audio,
            pending_audio: VecDeque::new(),
            pending_bytes: 0,
            last_video: None,
        })
    }

    fn block(&mut self, track: u8, pts: i64, key: bool, data: &[u8], discard: i64) -> Result<()> {
        let ticks = u64::try_from(pts.max(0) / SCALE).map_err(io)?;
        let position = self.out.stream_position().map_err(io)?.saturating_sub(self.segment);
        let mut cluster = Vec::new();
        uint(&mut cluster, id::TIMESTAMP, ticks);
        let mut block = vec![0x80 | track, 0, 0, if track == 1 && key { 0x80 } else { 0 }];
        block.extend_from_slice(data);
        if track == 1 {
            el(&mut cluster, id::SIMPLE_BLOCK, &block);
        } else {
            let mut group = Vec::new();
            el(&mut group, id::BLOCK, &block);
            if discard > 0 {
                el(&mut group, id::DISCARD_PADDING, &discard.to_be_bytes());
            }
            el(&mut cluster, id::BLOCK_GROUP, &group);
        }
        let mut bytes = Vec::new();
        el(&mut bytes, id::CLUSTER, &cluster);
        self.out.write_all(&bytes).map_err(io)?;
        if track == 1 && key {
            // Bound the index allocation even for extremely long all-intra timelines.
            if self.cues.len() >= 1_000_000 {
                return Err(io("too many Matroska cue entries"));
            }
            self.cues.push((ticks, position));
        }
        Ok(())
    }

    pub fn write_video(&mut self, packets: Vec<EncodedPacket>) -> Result<()> {
        for p in packets {
            let pts = ns(i128::from(self.video_ticks) + i128::from(p.composition_offset) - i128::from(self.video_start), self.timescale)?;
            self.drain_audio(pts.saturating_sub(1))?;
            self.block(1, pts, p.key, &p.data, 0)?;
            self.last_video = Some(self.last_video.unwrap_or(pts).max(pts));
            self.drain_audio(pts)?;
            self.video_ticks = self.video_ticks.saturating_add(i64::from(p.duration));
        }
        Ok(())
    }
    pub fn write_audio(&mut self, packets: Vec<(Vec<u8>, u32)>) -> Result<()> {
        for (bytes, samples) in packets {
            let Some(a) = self.audio.as_mut() else {
                return Err(io("missing Matroska audio track"));
            };
            let end = a.total.saturating_add(u64::from(a.priming));
            if a.written >= end {
                break;
            }
            let pts = ns(i128::from(a.written), a.rate)?;
            let discard = ns(i128::from(a.written.saturating_add(u64::from(samples)).saturating_sub(end)), a.rate)?;
            a.written = a.written.saturating_add(u64::from(samples));
            // Software AV1 may retain seconds of video. Keep its accompanying audio near the
            // eventual video clusters so seeking to a video Cue also finds the matching audio.
            self.pending_bytes = self.pending_bytes.saturating_add(bytes.len());
            if self.pending_bytes > 64 * 1024 * 1024 {
                return Err(io("Matroska audio interleave buffer exceeds 64 MiB"));
            }
            self.pending_audio.push_back((pts, bytes, discard));
        }
        if let Some(pts) = self.last_video {
            self.drain_audio(pts)?;
        }
        Ok(())
    }
    fn drain_audio(&mut self, until: i64) -> Result<()> {
        while self.pending_audio.front().is_some_and(|p| p.0 <= until) {
            if let Some((pts, bytes, discard)) = self.pending_audio.pop_front() {
                self.pending_bytes = self.pending_bytes.saturating_sub(bytes.len());
                self.block(2, pts, true, &bytes, discard)?;
            }
        }
        Ok(())
    }
    pub fn finish(mut self, settings: &ExportSettings) -> Result<(u64, Vec<String>)> {
        self.drain_audio(i64::MAX)?;
        let cue_position = self.out.stream_position().map_err(io)?.saturating_sub(self.segment);
        self.cues.sort_unstable();
        let mut cues = Vec::new();
        for (time, position) in self.cues {
            let mut p = Vec::new();
            uint(&mut p, id::CUE_TIME, time);
            let mut pos = Vec::new();
            uint(&mut pos, id::CUE_TRACK, 1);
            uint(&mut pos, id::CUE_CLUSTER_POSITION, position);
            el(&mut p, id::CUE_TRACK_POSITIONS, &pos);
            el(&mut cues, id::CUE_POINT, &p);
        }
        let mut bytes = Vec::new();
        el(&mut bytes, id::CUES, &cues);
        self.out.write_all(&bytes).map_err(io)?;
        let mut entry = Vec::new();
        let mut cue_id = Vec::new();
        write_id(&mut cue_id, id::CUES);
        el(&mut entry, id::SEEK_ID, &cue_id);
        uint(&mut entry, id::SEEK_POSITION, cue_position);
        let mut seek = Vec::new();
        el(&mut seek, id::SEEK, &entry);
        let mut bytes = Vec::new();
        el(&mut bytes, id::SEEK_HEAD, &seek);
        let remaining = SEEK_SPACE.checked_sub(bytes.len()).filter(|n| *n >= 2).ok_or_else(|| io("Matroska SeekHead reservation is too small"))?;
        void(&mut bytes, remaining);
        self.out.seek(SeekFrom::Start(self.seek)).map_err(io)?;
        self.out.write_all(&bytes).map_err(io)?;
        Ok((self.out.finish(settings)?, Vec::new()))
    }
}
