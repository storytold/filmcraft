//! MXF output of the stepped exporter: OP1a (one file, frame-wrapped picture + one PCM track) or
//! OP-Atom (Avid style: the picture in `path`, one mono PCM file per audio channel next to it,
//! `<stem>_A1.mxf`, `<stem>_A2.mxf`, …), written with [`filmcraft_mxf::MxfWriter`].

use filmcraft_mxf::{
    CodedKind, ColorSpace, FrameInfo, MxfWriter, PackageIds, Pattern, PictureCoding, PictureDesc, Rational, SoundDesc, StartTimecode, Timestamp, WriterConfig,
};
use filmcraft_time::FrameRate;

use crate::{ColorSignal, EncodedPacket, ExportError, ExportSettings, Format, H264Profile, Out, Result};

/// Paths of the OP-Atom audio files for an export to `path` with `channels` channels.
pub fn opatom_audio_paths(path: &str, channels: usize) -> Vec<String> {
    let p = std::path::Path::new(path);
    let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "export".into());
    (1..=channels)
        .map(|k| {
            let name = format!("{stem}_A{k}.mxf");
            match p.parent().filter(|d| !d.as_os_str().is_empty()) {
                Some(d) => d.join(name).to_string_lossy().to_string(),
                None => name,
            }
        })
        .collect()
}

fn io(e: std::io::Error) -> ExportError {
    ExportError::Io(e.to_string())
}

/// The picture essence description for an export (codec from the MXF video codec setting).
fn picture(settings: &ExportSettings, w: u32, h: u32) -> PictureDesc {
    let coding = match settings.video_format() {
        Format::ProRes => {
            use filmcraft_prores::Profile;
            PictureCoding::ProRes {
                profile: match crate::prores_profile(&settings.prores_profile) {
                    Profile::Proxy => 1,
                    Profile::Lt => 2,
                    Profile::Standard => 3,
                    Profile::Hq => 4,
                    Profile::P4444 => 5,
                    Profile::P4444Xq => 6,
                },
            }
        }
        Format::H264 => PictureCoding::Avc {
            profile_idc: match settings.h264_profile {
                H264Profile::Baseline => 66,
                H264Profile::Main => 77,
                H264Profile::High => 100,
            },
            intra: false,
        },
        _ => PictureCoding::Vc3 { cid: crate::dnx_profile(&settings.dnx_profile).cid() },
    };
    let mut d = PictureDesc::new(coding, w, h);
    d.color = match settings.signal {
        s if s == ColorSignal::PQ => ColorSpace::Rec2020Pq,
        s if s == ColorSignal::HLG => ColorSpace::Rec2020Hlg,
        _ => ColorSpace::Rec709,
    };
    if let Some((n, dn)) = settings.pixel_aspect.filter(|(n, d)| *n > 0 && *d > 0 && n != d) {
        let (a, b) = (w as u64 * n as u64, h as u64 * dn as u64);
        let g = gcd(a, b);
        d.aspect = Rational::new((a / g) as i32, (b / g) as i32);
    }
    d
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a.max(1) } else { gcd(b, a % b) }
}

/// Everything the MXF muxer needs from the export.
pub(crate) struct MxfSetup<'a> {
    pub settings: &'a ExportSettings,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub rate: FrameRate,
    /// (sample rate, channels) of the audio, if any.
    pub audio: Option<(u32, usize)>,
    /// Start timecode in frames of `rate`, and drop-frame.
    pub timecode: (i64, bool),
}

/// An MXF export in progress.
pub(crate) struct MxfMux {
    video: MxfWriter<Out>,
    /// OP-Atom: one writer per audio channel.
    audio: Vec<(String, MxfWriter<Out>)>,
    atom: bool,
    bits: u32,
    channels: usize,
    /// Stored pictures so far, and the composition offset of the first (H.264 reorder delay).
    stored: i64,
    first_offset: Option<i64>,
    max_display: i64,
    frame_den: i64,
}

impl MxfMux {
    pub fn new(s: MxfSetup) -> Result<Self> {
        let settings = s.settings;
        let atom = settings.format == Format::MxfOpAtom;
        let rate = Rational::new(s.rate.num as i32, s.rate.den as i32);
        let now = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let seed = format!("{}|{now}", settings.path);
        let ids = PackageIds::from_seed(&seed, &s.name);
        let bits = if settings.audio.bits >= 24 { 24 } else { 16 };
        let mut cfg = WriterConfig::new(if atom { Pattern::OpAtom } else { Pattern::Op1a }, rate, ids.clone());
        cfg.picture = Some(picture(settings, s.width, s.height));
        cfg.modified = Timestamp::from_unix((now / 1_000_000_000) as i64);
        cfg.timecode = Some(StartTimecode { frames: s.timecode.0, rate, drop_frame: s.timecode.1 && s.rate.supports_drop_frame() });
        if !atom && let Some((sr, ch)) = s.audio {
            cfg.sound = vec![SoundDesc { sample_rate: sr, channels: ch as u32, bits }];
        }
        let mut audio = Vec::new();
        if atom && let Some((sr, ch)) = s.audio {
            for (k, path) in opatom_audio_paths(&settings.path, ch).into_iter().enumerate() {
                // Avid style: the files share the material package; each has its own file package.
                let mut a = WriterConfig::new(
                    Pattern::OpAtom,
                    rate,
                    PackageIds {
                        material: ids.material,
                        file: filmcraft_mxf::Umid::from_seed(format!("file:{seed}:A{}", k + 1).as_bytes()),
                        material_name: ids.material_name.clone(),
                        file_name: format!("{} A{}", s.name, k + 1),
                    },
                );
                a.first_track_id = 3 + k as u32;
                a.sound = vec![SoundDesc { sample_rate: sr, channels: 1, bits }];
                a.timecode = cfg.timecode;
                a.modified = cfg.modified;
                let out = Out::create_path(settings, &path)?;
                audio.push((path, MxfWriter::new(out, a).map_err(io)?));
            }
        }
        let video = MxfWriter::new(Out::create_path(settings, &settings.path)?, cfg).map_err(io)?;
        Ok(MxfMux {
            video,
            audio,
            atom,
            bits,
            channels: s.audio.map_or(0, |a| a.1),
            stored: 0,
            first_offset: None,
            max_display: -1,
            frame_den: s.rate.den.max(1),
        })
    }

    pub fn write_video(&mut self, packets: Vec<EncodedPacket>) -> Result<()> {
        for p in packets {
            let intra = !matches!(self.video.config().picture.as_ref().map(|p| p.coding), Some(PictureCoding::Avc { .. }));
            let info = if intra {
                FrameInfo::intra()
            } else {
                // display position from the composition offset (pts − dts) relative to the first
                let off = p.composition_offset as i64;
                let first = *self.first_offset.get_or_insert(off);
                let display = self.stored + (off - first) / self.frame_den;
                let kind = if p.key {
                    CodedKind::Intra
                } else if display < self.max_display {
                    CodedKind::Bidirectional
                } else {
                    CodedKind::Predicted
                };
                self.max_display = self.max_display.max(display);
                FrameInfo { key: p.key, kind, display: Some(display) }
            };
            self.video.push_picture(&p.data, info).map_err(io)?;
            self.stored += 1;
        }
        Ok(())
    }

    pub fn write_audio(&mut self, planar: &[Vec<f32>]) -> Result<()> {
        if self.channels == 0 || planar.first().is_none_or(Vec::is_empty) {
            return Ok(());
        }
        if self.atom {
            for (k, (_, w)) in self.audio.iter_mut().enumerate() {
                if let Some(c) = planar.get(k) {
                    w.push_sound(0, &filmcraft_mxf::encode_pcm(std::slice::from_ref(c), self.bits)).map_err(io)?;
                }
            }
            Ok(())
        } else {
            self.video.push_sound(0, &filmcraft_mxf::encode_pcm(planar, self.bits)).map_err(io)
        }
    }

    /// Finish every file; returns (total bytes, extra files written besides `settings.path`).
    pub fn finish(self, settings: &ExportSettings) -> Result<(u64, Vec<String>)> {
        let mut total = self.video.finish().map_err(io)?.finish_path(settings, &settings.path)?;
        let mut extra = Vec::new();
        for (path, w) in self.audio {
            total += w.finish().map_err(io)?.finish_path(settings, &path)?;
            extra.push(path);
        }
        Ok((total, extra))
    }
}
