//! Step-wise export of the encoded-video formats (H.264 MP4/MOV, ProRes / DNxHR / Motion-JPEG MOV).
//!
//! [`Exporter::step`] renders, encodes and muxes one batch of frames (plus the audio up to its
//! end) per call, so a host without threads (the web app) can run an export a slice at a time
//! between UI frames; [`crate::export`] simply loops it on a worker thread.
//!
//! Before the first batch, loudness normalization (when enabled) measures the export range. A
//! two-pass VBR H.264 export renders and analyses every frame once (first pass, nothing written),
//! then encodes for real with the first pass's statistics.
//!
//! When a source's bytes are not available yet (asynchronous web reads mark
//! [`filmcraft_media::pending`]), the batch is rendered again on the next call: nothing is encoded
//! from frames or audio with missing media.
//!
//! The file does not depend on how many frames a step renders (one per core natively, one on the
//! web): packets go to the muxer in fixed groups of [`INTERLEAVE`] output frames, each followed by
//! the audio up to its end, so the same project and settings give the same bytes on any machine.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use filmcraft_isobmff::{Brand, Mp4Writer, PcmConfig, SampleEntry, TrackConfig, WriteSample, WriterOptions};
use filmcraft_project::{ItemId, Project};
use filmcraft_render::SourceProvider;
use filmcraft_time::FrameRate;
use rayon::prelude::*;

use crate::audio_out::AudioOut;
use crate::mxf_out::{MxfMux, MxfSetup};
use crate::pipeline::Pipeline;
use crate::settings::{AudioCodec, BitrateMode};
use crate::{
    AudioEncoder, ColorSignal, EncodedPacket, EncoderFrame, ExportError, ExportSettings, Format, H264Pass, Out, Progress, Report, Result, VideoEncoder,
    audio_factories, export_range, frame_span, video_factories,
};

/// Output frames per interleaved group: the video packets of these frames, then the audio up to
/// their end, form one chunk of each track. Fixed (not the batch size, which follows the core
/// count) so the file layout is the same on every machine.
pub const INTERLEAVE: i64 = 16;

/// What one [`Exporter::step`] did.
#[derive(Debug)]
pub enum Step {
    /// A batch was encoded; call again.
    Progress,
    /// Media bytes are still loading: nothing was encoded, call again later.
    Pending,
    /// The file is complete.
    Done(Report),
}

/// An export of an encoded-video format, advanced one batch at a time.
pub struct Exporter {
    pipe: Pipeline,
    settings: ExportSettings,
    f0: i64,
    f1: i64,
    /// Next frame to render.
    next: i64,
    batch: i64,
    brand: Brand,
    venc: Box<dyn VideoEncoder>,
    aenc: Option<Box<dyn AudioEncoder>>,
    audio: Option<AudioOut>,
    /// The loudness pass ran (or is not needed).
    measured: bool,
    /// The first pass of a two-pass encode is running.
    first_pass: bool,
    /// Encoded video packets not written yet (the current [`INTERLEAVE`] group).
    queued: Vec<EncodedPacket>,
    /// Created after the first group was encoded (encoders may finalise their codec config then).
    mux: Option<Mp4Writer<Out>>,
    /// MXF exports write through this instead of `mux`.
    mxf: Option<MxfMux>,
    seq: ItemId,
    project: Arc<Project>,
    vt: usize,
    at: Option<usize>,
    t0: web_time::Instant,
}

/// Whether [`Exporter`] handles a format.
pub fn stepped(format: Format) -> bool {
    matches!(
        format,
        Format::H264 | Format::Hevc | Format::Av1 | Format::ProRes | Format::DnxHr | Format::Apv | Format::Mjpeg | Format::MxfOp1a | Format::MxfOpAtom
    )
}

fn make_venc(settings: &ExportSettings, w: u32, h: u32, rate: FrameRate) -> Result<Box<dyn VideoEncoder>> {
    video_factories()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find_map(|fac| fac(settings.video_format(), w, h, rate, settings))
        .ok_or_else(|| ExportError::Unsupported(format!("{} encoder not available yet", settings.video_format().label())))?
}

impl Exporter {
    /// Set up an export (encoder, output size, colour signalling); sets `progress.total`.
    pub fn new(project: Arc<Project>, seq: ItemId, settings: &ExportSettings, progress: &Progress) -> Result<Self> {
        if !stepped(settings.format) {
            return Err(ExportError::Unsupported(format!("{} is not a stepped export", settings.format.label())));
        }
        settings.validate()?;
        let q = project.sequence(seq).ok_or(ExportError::NoSequence)?;
        // HDR sequences export HDR (H.264 / ProRes / DNxHR / APV) unless SDR is asked for (H.265 export is 8-bit SDR)
        let pipe = q.settings.color;
        let hdr_out = pipe.working.is_hdr() && !settings.sdr && matches!(settings.video_format(), Format::H264 | Format::ProRes | Format::DnxHr | Format::Apv);
        let mut settings = settings.clone();
        settings.signal = match (hdr_out, pipe.working) {
            (true, filmcraft_color::WorkingSpace::Rec2100Pq) => ColorSignal::PQ,
            (true, _) => ColorSignal::HLG,
            _ => ColorSignal::default(),
        };
        // bake the derived bitrate / keyframe values in for the encoder factories
        let r = settings.resolve(q.settings.width, q.settings.height, q.settings.frame_rate, q.settings.sample_rate);
        settings.bitrate_kbps = r.target_kbps;
        settings.max_bitrate_kbps = Some(r.max_kbps);
        settings.keyframe_distance = Some(r.keyint);
        settings.adaptive_bitrate = None;
        let two_pass = settings.video_format() == Format::H264 && settings.bitrate_mode == BitrateMode::Vbr2Pass;
        settings.h264_pass = if two_pass { H264Pass::First } else { H264Pass::Single };
        let range = export_range(&project, seq, &settings)?;
        let pipe = Pipeline::new(project.clone(), seq, &settings, hdr_out)?;
        let (f0, f1) = frame_span(pipe.rate, range);
        let nframes = (f1 - f0) as u64;
        if !settings.part_of_batch {
            progress.total.store(nframes * if two_pass { 2 } else { 1 }, Ordering::Relaxed);
            progress.set_status(format!("Exporting {} frames ({})", nframes, settings.format.label()));
        }
        let venc = make_venc(&settings, pipe.w, pipe.h, pipe.rate)?;
        let brand = if settings.format.is_mp4_with(settings.multiplexer) { Brand::Mp4 } else { Brand::Mov };
        let audio = if settings.has_audio() { Some(AudioOut::new(project.clone(), seq, &settings, range)?) } else { None };
        let aenc: Option<Box<dyn AudioEncoder>> = match (&audio, settings.audio_codec()) {
            (Some(a), AudioCodec::Aac) => {
                let found =
                    audio_factories().read().unwrap_or_else(|e| e.into_inner()).iter().find_map(|f| f(settings.format, a.sr, a.channels as u32, &settings));
                Some(found.ok_or_else(|| ExportError::Unsupported("AAC encoder not available".into()))??)
            }
            _ => None,
        };
        let measured = !settings.effects.loudness.enabled || audio.is_none();
        Ok(Self {
            pipe,
            f0,
            f1,
            next: f0,
            batch: rayon::current_num_threads().clamp(2, 16) as i64,
            brand,
            venc,
            aenc,
            audio,
            measured,
            first_pass: two_pass,
            queued: Vec::new(),
            mux: None,
            mxf: None,
            seq,
            project: project.clone(),
            vt: 0,
            at: None,
            t0: web_time::Instant::now(),
            settings,
        })
    }

    /// Frames in the export.
    pub fn frames(&self) -> u64 {
        (self.f1 - self.f0).max(0) as u64
    }

    /// Frames per [`Self::step`] (default: one per core); 1 keeps web UI frames short.
    pub fn set_batch(&mut self, n: i64) {
        self.batch = n.max(1);
    }

    fn sample_at_frame(&self, f: i64, sr: u32) -> i64 {
        self.pipe.rate.tick_of(f).to_units_floor(sr as i64)
    }

    fn write_audio(&mut self, planar: Option<Vec<Vec<f32>>>) -> Result<()> {
        if let Some(m) = self.mxf.as_mut() {
            return match planar {
                Some(buf) => m.write_audio(&buf),
                None => Ok(()),
            };
        }
        let (Some(at), Some(buf)) = (self.at, planar) else { return Ok(()) };
        let n = buf.first().map_or(0, Vec::len);
        if n == 0 {
            return Ok(());
        }
        let mux = self.mux.as_mut().ok_or_else(|| ExportError::Encode("internal: the container writer was not created".into()))?;
        match self.aenc.as_mut() {
            Some(a) => {
                for au in a.encode(&buf)? {
                    mux.write_sample(at, WriteSample { data: &au, duration: a.frame_size(), composition_offset: 0, is_sync: true })
                        .map_err(|e| ExportError::Io(e.to_string()))?;
                }
            }
            None => {
                let bps = if self.settings.audio.bits >= 24 { 3 } else { 2 };
                let mut pcm = Vec::with_capacity(n * buf.len() * bps);
                for i in 0..n {
                    for c in &buf {
                        let s = c[i].clamp(-1.0, 1.0);
                        if bps == 3 {
                            pcm.extend_from_slice(&((s * 8_388_607.0).round() as i32).to_le_bytes()[..3]);
                        } else {
                            pcm.extend_from_slice(&((s * 32767.0).round() as i16).to_le_bytes());
                        }
                    }
                }
                mux.write_sample(at, WriteSample { data: &pcm, duration: n as u32, composition_offset: 0, is_sync: true })
                    .map_err(|e| ExportError::Io(e.to_string()))?;
            }
        }
        Ok(())
    }

    fn write_video(&mut self, packets: Vec<EncodedPacket>) -> Result<()> {
        if let Some(m) = self.mxf.as_mut() {
            return m.write_video(packets);
        }
        let mux = self.mux.as_mut().ok_or_else(|| ExportError::Encode("internal: the container writer was not created".into()))?;
        for p in packets {
            mux.write_sample(self.vt, WriteSample { data: &p.data, duration: p.duration, composition_offset: p.composition_offset, is_sync: p.key })
                .map_err(|e| ExportError::Io(e.to_string()))?;
        }
        Ok(())
    }

    /// Create the muxer and its tracks (after the first batch was encoded).
    fn open_mux(&mut self) -> Result<()> {
        if self.settings.format.is_mxf() {
            let q = self.project.sequence(self.seq).ok_or(ExportError::NoSequence)?;
            // start timecode of the first exported frame, at the output rate
            let seq_rate = q.settings.frame_rate;
            let start = self.pipe.rate.frame_at(seq_rate.tick_of(q.start_timecode)) + self.f0;
            let name = self.project.item(self.seq).map(|i| i.name.clone()).unwrap_or_default();
            self.mxf = Some(MxfMux::new(MxfSetup {
                settings: &self.settings,
                name,
                width: self.pipe.w,
                height: self.pipe.h,
                rate: self.pipe.rate,
                audio: self.audio.as_ref().map(|a| (a.sr, a.channels)),
                timecode: (start, q.settings.drop_frame),
            })?);
            return Ok(());
        }
        let file = Out::create(&self.settings)?;
        let mut opts = WriterOptions::new(self.brand);
        opts.metadata = self.settings.metadata.udta();
        let mut mux = Mp4Writer::new(file, opts).map_err(|e| ExportError::Io(e.to_string()))?;
        let mut vcfg = TrackConfig::new(self.venc.sample_entry(), self.venc.timescale());
        vcfg.media_start = self.venc.media_start();
        self.vt = mux.add_track(vcfg).map_err(|e| ExportError::Io(e.to_string()))?;
        self.at = match (&self.audio, &self.aenc) {
            (None, _) => None,
            (Some(a), Some(enc)) => {
                let mut c = TrackConfig::new(enc.sample_entry(), a.sr);
                c.media_start = Some(enc.priming() as i64);
                Some(mux.add_track(c).map_err(|e| ExportError::Io(e.to_string()))?)
            }
            (Some(a), None) => {
                let bits = if self.settings.audio.bits >= 24 { 24 } else { 16 };
                let pcm = PcmConfig { bits, float: false, big_endian: false, signed: true, channels: a.channels as _, sample_rate: a.sr as f64 };
                Some(mux.add_track(TrackConfig::new(SampleEntry::pcm(pcm), a.sr)).map_err(|e| ExportError::Io(e.to_string()))?)
            }
        };
        self.mux = Some(mux);
        Ok(())
    }

    /// Render, encode and mux the next batch (or finish the file).
    pub fn step(&mut self, sources: &dyn SourceProvider, progress: &Progress) -> Result<Step> {
        if progress.cancel.load(Ordering::Relaxed) {
            return Err(ExportError::Cancelled);
        }
        let _ = filmcraft_media::pending::take();
        if !self.measured {
            if !self.settings.part_of_batch {
                progress.set_status("Measuring loudness");
            }
            let a = self.audio.as_mut().ok_or_else(|| ExportError::Encode("internal: the audio pipeline was not created".into()))?;
            a.measure(&self.settings, sources, &|| progress.cancel.load(Ordering::Relaxed))?;
            if filmcraft_media::pending::take() {
                return Ok(Step::Pending);
            }
            *progress.loudness.lock().unwrap_or_else(|e| e.into_inner()) = a.loudness;
            self.measured = true;
            if !self.settings.part_of_batch {
                progress.set_status(format!("Exporting {} frames ({})", self.frames(), self.settings.format.label()));
            }
            return Ok(Step::Progress);
        }
        if self.next < self.f1 {
            let (f, end) = (self.next, (self.next + self.batch).min(self.f1));
            let pipe = &self.pipe;
            let frames: Vec<(Vec<u8>, Vec<f32>)> = (f..end).into_par_iter().map(|fi| pipe.frame(fi, sources)).collect();
            if self.first_pass {
                if filmcraft_media::pending::take() {
                    return Ok(Step::Pending);
                }
                self.encode(&frames, f)?;
                progress.done.fetch_add((end - f) as u64, Ordering::Relaxed);
                self.next = end;
                return Ok(Step::Progress);
            }
            // the audio of every interleave group that ends in this batch (before encoding, so a
            // pending source can still make the batch run again)
            let cuts: Vec<i64> = (f + 1..=end).filter(|&g| (g - self.f0) % INTERLEAVE == 0).collect();
            let mut mixed = Vec::with_capacity(cuts.len());
            if let Some(sr) = self.audio.as_ref().map(|a| a.sr) {
                for &g in &cuts {
                    let until = self.sample_at_frame(g, sr);
                    mixed.push(self.audio.as_mut().and_then(|a| a.pull(until, sources)));
                }
            }
            if filmcraft_media::pending::take() {
                return Ok(Step::Pending);
            }
            let mut mixed = mixed.into_iter();
            for (k, frame) in frames.iter().enumerate() {
                let fi = f + k as i64;
                let packets = self.encode(std::slice::from_ref(frame), fi)?;
                self.queued.extend(packets);
                if cuts.contains(&(fi + 1)) {
                    if self.mux.is_none() && self.mxf.is_none() {
                        self.open_mux()?;
                    }
                    let packets = std::mem::take(&mut self.queued);
                    self.write_video(packets)?;
                    self.write_audio(mixed.next().flatten())?;
                }
            }
            progress.done.fetch_add((end - f) as u64, Ordering::Relaxed);
            self.next = end;
            return Ok(Step::Progress);
        }
        if self.first_pass {
            // end of the analysis pass: encode for real with its statistics
            let _ = self.venc.flush()?;
            let stats = self.venc.pass_stats().ok_or_else(|| ExportError::Encode("two-pass statistics missing".into()))?;
            self.settings.h264_pass = H264Pass::Second(stats);
            self.venc = make_venc(&self.settings, self.pipe.w, self.pipe.h, self.pipe.rate)?;
            self.first_pass = false;
            self.next = self.f0;
            return Ok(Step::Progress);
        }
        let mixed = self.audio.as_mut().and_then(|a| a.rest(sources));
        if filmcraft_media::pending::take() {
            return Ok(Step::Pending);
        }
        if self.mux.is_none() && self.mxf.is_none() {
            self.open_mux()?;
        }
        let mut tail = std::mem::take(&mut self.queued);
        tail.extend(self.venc.flush()?);
        self.write_video(tail)?;
        self.write_audio(mixed)?;
        if let (Some(at), Some(a)) = (self.at, self.aenc.as_mut()) {
            let fs = a.frame_size();
            let mux = self.mux.as_mut().ok_or_else(|| ExportError::Encode("internal: the container writer was not created".into()))?;
            for au in a.flush()? {
                mux.write_sample(at, WriteSample { data: &au, duration: fs, composition_offset: 0, is_sync: true })
                    .map_err(|e| ExportError::Io(e.to_string()))?;
            }
        }
        let (bytes, extra_files) = match self.mxf.take() {
            Some(m) => m.finish(&self.settings)?,
            None => {
                let w = self
                    .mux
                    .take()
                    .ok_or_else(|| ExportError::Encode("internal: the container writer was not created".into()))?
                    .finish()
                    .map_err(|e| ExportError::Io(e.to_string()))?;
                (w.finish(&self.settings)?, Vec::new())
            }
        };
        let secs = self.t0.elapsed().as_secs_f64();
        let nframes = self.frames();
        if !self.settings.part_of_batch {
            progress.finished.store(true, Ordering::Relaxed);
            progress.set_status(format!("Done in {secs:.1}s"));
        }
        Ok(Step::Done(Report {
            path: self.settings.path.clone(),
            frames: nframes,
            seconds: secs,
            bytes,
            render_fps: nframes as f64 / secs.max(1e-6),
            extra_files,
        }))
    }

    fn encode(&mut self, frames: &[(Vec<u8>, Vec<f32>)], f: i64) -> Result<Vec<EncodedPacket>> {
        let mut packets = Vec::new();
        for (k, (rgba, hdr)) in frames.iter().enumerate() {
            let fr = EncoderFrame {
                width: self.pipe.w,
                height: self.pipe.h,
                rgba,
                hdr: self.pipe.hdr_out.then_some(hdr.as_slice()),
                index: (f - self.f0) as u64 + k as u64,
            };
            packets.extend(self.venc.encode(&fr)?);
        }
        Ok(packets)
    }
}
