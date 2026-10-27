use super::output::OutputSink;
use super::run::validate_range;
use super::*;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Format {
    /// MPEG-4 (or QuickTime, see [`Multiplexer`]), H.264 video + AAC audio.
    #[default]
    #[serde(rename = "h264", alias = "H264")]
    H264,
    /// MPEG-4 (or QuickTime, see [`Multiplexer`]), H.265 / HEVC Main (8-bit) video + AAC audio. There is no
    /// built-in encoder: a platform hardware encoder registers one ([`register_encoder`]) and says so
    /// with [`register_format_probe`]; [`available`] is false without it. HDR sequences export as HDR
    /// (Main 10, PQ / HLG) only where an encoder also says so with [`register_hdr_probe`]
    /// ([`hdr_available`]); elsewhere they are tone-mapped to SDR.
    #[serde(rename = "hevc", alias = "Hevc")]
    Hevc,
    /// MPEG-4, AV1 Main (8-bit) video + AAC audio. Like H.265 it has no built-in encoder: offered
    /// where a hardware encoder registers one.
    #[serde(rename = "av1", alias = "Av1")]
    Av1,
    /// QuickTime, Apple ProRes 422 (HQ unless the settings pick another flavour) + PCM.
    #[serde(rename = "prores", alias = "ProRes")]
    ProRes,
    /// QuickTime, Avid DNxHR (HQ unless the settings pick another profile) + PCM.
    #[serde(rename = "dnxhr", alias = "DnxHr")]
    DnxHr,
    /// QuickTime, APV (Advanced Professional Video, RFC 9924; 422-10 unless the settings pick
    /// another profile) + PCM.
    #[serde(rename = "apv", alias = "Apv")]
    Apv,
    /// QuickTime, Motion-JPEG + PCM.
    #[serde(rename = "mjpeg", alias = "Mjpeg")]
    Mjpeg,
    /// Numbered PNG stills ([`image_sequence_path`]).
    #[serde(rename = "png", alias = "PngSequence")]
    PngSequence,
    /// Numbered TIFF stills.
    #[serde(rename = "tiff", alias = "TiffSequence")]
    TiffSequence,
    /// Numbered BMP stills.
    #[serde(rename = "bmp", alias = "BmpSequence")]
    BmpSequence,
    /// Numbered JPEG stills (baseline, [`ExportSettings::quality`]).
    #[serde(rename = "jpg", alias = "JpegSequence")]
    JpegSequence,
    /// Numbered Targa stills (uncompressed, 24-bit, or 32-bit with [`ExportSettings::alpha`]).
    #[serde(rename = "tga", alias = "TgaSequence")]
    TgaSequence,
    /// Numbered DPX stills (SMPTE ST 268, 10-bit RGB, BT.709).
    #[serde(rename = "dpx", alias = "DpxSequence")]
    DpxSequence,
    /// Numbered OpenEXR stills (half-float scene-linear RGB, RGBA with [`ExportSettings::alpha`]).
    #[serde(rename = "exr", alias = "ExrSequence")]
    ExrSequence,
    #[serde(rename = "gif", alias = "Gif")]
    Gif,
    #[serde(rename = "wav", alias = "Wav")]
    Wav,
    #[serde(rename = "aiff", alias = "Aiff")]
    Aiff,
    /// AAC Audio: the mix alone, AAC-LC in an MPEG-4 audio file (`.m4a`), or a raw ADTS stream when
    /// the path ends in `.aac`.
    #[serde(rename = "aac", alias = "Aac")]
    Aac,
    /// FLAC (RFC 9639): lossless compressed audio.
    #[serde(rename = "flac", alias = "Flac")]
    Flac,
    /// MXF OP1a (SMPTE ST 378): frame-wrapped DNxHR, ProRes or H.264 ([`MxfVideoCodec`]) + PCM.
    #[serde(rename = "mxf-op1a", alias = "MxfOp1a")]
    MxfOp1a,
    /// MXF OP-Atom (SMPTE ST 390, Avid style): the picture in one file, one mono PCM file per
    /// audio channel ([`opatom_audio_paths`]).
    #[serde(rename = "mxf-opatom", alias = "MxfOpAtom")]
    MxfOpAtom,
}

impl Format {
    pub fn from_name(s: &str) -> Option<Format> {
        Some(match s.to_ascii_lowercase().replace([' ', '-', '_', '.'], "").as_str() {
            "h264" | "mp4" | "avc" | "m4v" => Format::H264,
            "hevc" | "h265" | "hvc1" | "hev1" | "x265" => Format::Hevc,
            "av1" | "av01" => Format::Av1,
            "prores" | "mov" | "appleprores" => Format::ProRes,
            "dnxhr" | "dnxhd" | "dnx" | "avid" | "aviddnxhr" | "aviddnxhd" | "vc3" => Format::DnxHr,
            "apv" | "apv1" => Format::Apv,
            "mxf" | "mxfop1a" | "op1a" => Format::MxfOp1a,
            "mxfopatom" | "opatom" | "mxfatom" | "avidmxf" => Format::MxfOpAtom,
            "mjpeg" | "motionjpeg" | "jpeg" => Format::Mjpeg,
            "png" | "pngsequence" => Format::PngSequence,
            "tif" | "tiff" | "tiffsequence" => Format::TiffSequence,
            "bmp" | "bmpsequence" => Format::BmpSequence,
            // "jpeg" alone stays Motion-JPEG (QuickTime), as it always was
            "jpg" | "jpgsequence" | "jpegsequence" => Format::JpegSequence,
            "tga" | "targa" | "tgasequence" | "targasequence" => Format::TgaSequence,
            "dpx" | "dpxsequence" => Format::DpxSequence,
            "exr" | "openexr" | "exrsequence" | "openexrsequence" => Format::ExrSequence,
            "gif" | "animatedgif" => Format::Gif,
            "wav" | "waveform" | "waveformaudio" => Format::Wav,
            "aif" | "aiff" | "aifc" => Format::Aiff,
            "aac" | "aacaudio" | "m4a" | "adts" => Format::Aac,
            "flac" | "fla" => Format::Flac,
            _ => return None,
        })
    }
    /// Stable id, as accepted by [`Format::from_name`] and used in serialized settings.
    pub fn id(self) -> &'static str {
        match self {
            Format::H264 => "h264",
            Format::Hevc => "hevc",
            Format::Av1 => "av1",
            Format::ProRes => "prores",
            Format::DnxHr => "dnxhr",
            Format::Apv => "apv",
            Format::Mjpeg => "mjpeg",
            Format::PngSequence => "png",
            Format::TiffSequence => "tiff",
            Format::BmpSequence => "bmp",
            Format::JpegSequence => "jpg",
            Format::TgaSequence => "tga",
            Format::DpxSequence => "dpx",
            Format::ExrSequence => "exr",
            Format::Gif => "gif",
            Format::Wav => "wav",
            Format::Aiff => "aiff",
            Format::Aac => "aac",
            Format::Flac => "flac",
            Format::MxfOp1a => "mxf-op1a",
            Format::MxfOpAtom => "mxf-opatom",
        }
    }
    pub fn extension(self) -> &'static str {
        match self {
            Format::H264 | Format::Hevc | Format::Av1 => "mp4",
            Format::ProRes | Format::DnxHr | Format::Apv | Format::Mjpeg => "mov",
            Format::PngSequence => "png",
            Format::TiffSequence => "tif",
            Format::BmpSequence => "bmp",
            Format::JpegSequence => "jpg",
            Format::TgaSequence => "tga",
            Format::DpxSequence => "dpx",
            Format::ExrSequence => "exr",
            Format::Gif => "gif",
            Format::Wav => "wav",
            Format::Aiff => "aif",
            Format::Aac => "m4a",
            Format::Flac => "flac",
            Format::MxfOp1a | Format::MxfOpAtom => "mxf",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Format::H264 => "H.264",
            Format::Hevc => "H.265 (HEVC)",
            Format::Av1 => "AV1",
            Format::ProRes => "Apple ProRes",
            Format::DnxHr => "Avid DNxHR",
            Format::Apv => "APV",
            Format::Mjpeg => "QuickTime (Motion JPEG)",
            Format::PngSequence => "PNG",
            Format::TiffSequence => "TIFF",
            Format::BmpSequence => "BMP",
            Format::JpegSequence => "JPEG",
            Format::TgaSequence => "Targa",
            Format::DpxSequence => "DPX",
            Format::ExrSequence => "OpenEXR",
            Format::Gif => "Animated GIF",
            Format::Wav => "Waveform Audio",
            Format::Aiff => "AIFF",
            Format::Aac => "AAC Audio",
            Format::Flac => "FLAC",
            Format::MxfOp1a => "MXF OP1a",
            Format::MxfOpAtom => "MXF OP-Atom",
        }
    }
    /// Numbered stills, one per frame ([`image_sequence_path`]).
    pub fn is_image_sequence(self) -> bool {
        matches!(
            self,
            Format::PngSequence
                | Format::TiffSequence
                | Format::BmpSequence
                | Format::JpegSequence
                | Format::TgaSequence
                | Format::DpxSequence
                | Format::ExrSequence
        )
    }
    /// An MXF container format.
    pub fn is_mxf(self) -> bool {
        matches!(self, Format::MxfOp1a | Format::MxfOpAtom)
    }
    /// Whether the crate carries an encoder for the format; the others need one registered at runtime
    /// ([`register_format_probe`]).
    pub fn has_builtin_encoder(self) -> bool {
        !matches!(self, Format::Hevc | Format::Av1)
    }
    /// Sound without picture: Waveform Audio, AIFF, AAC Audio and FLAC.
    pub fn is_audio_only(self) -> bool {
        matches!(self, Format::Wav | Format::Aiff | Format::Aac | Format::Flac)
    }
    /// H.264 or H.265: MPEG-4 (or QuickTime) with AAC audio, set up with the same bitrate controls.
    pub fn is_h26x(self) -> bool {
        matches!(self, Format::H264 | Format::Hevc)
    }
    /// A bitrate-controlled MPEG-4 video format with AAC audio: H.264, H.265 or AV1 (MPEG-4 only).
    pub fn is_mp4_video(self) -> bool {
        matches!(self, Format::H264 | Format::Hevc | Format::Av1)
    }
    /// Whether the file is MPEG-4 rather than QuickTime for `mux`.
    pub fn is_mp4_with(self, mux: Multiplexer) -> bool {
        self == Format::Av1 || (self.is_h26x() && mux == Multiplexer::Mp4)
    }
    pub const ALL: [Format; 21] = [
        Format::H264,
        Format::Hevc,
        Format::Av1,
        Format::ProRes,
        Format::DnxHr,
        Format::Apv,
        Format::Mjpeg,
        Format::PngSequence,
        Format::TiffSequence,
        Format::BmpSequence,
        Format::JpegSequence,
        Format::TgaSequence,
        Format::DpxSequence,
        Format::ExrSequence,
        Format::Gif,
        Format::Wav,
        Format::Aiff,
        Format::Aac,
        Format::Flac,
        Format::MxfOp1a,
        Format::MxfOpAtom,
    ];
}

/// Two-pass state of an H.264 export (set by the exporter, not by callers).
#[derive(Clone, Debug, Default)]
pub enum H264Pass {
    #[default]
    Single,
    First,
    Second(filmcraft_h264enc::PassStats),
}

/// Every Export-mode setting. Serialized in camelCase; every field is optional on input.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ExportSettings {
    pub format: Format,
    pub path: String,
    /// Timeline range (default: In/Out if set, else the whole sequence).
    pub range: Option<TimeRange>,
    /// Output scale (1.0 = sequence frame size).
    pub scale: f32,
    pub include_audio: bool,
    /// Quality 0–100 for lossy codecs (Motion-JPEG, JPEG sequences).
    pub quality: u8,
    /// Target video bitrate (kbps) for bitrate-driven encoders.
    pub bitrate_kbps: u32,
    /// Burn the visible caption tracks into the picture (Export ▸ Captions ▸ Burn Captions Into
    /// Video).
    pub burn_captions: bool,
    /// This export is one part of a larger job (render previews): the caller owns
    /// `progress.total`, `finished` and the final status; this call only adds to `done`.
    #[serde(default)]
    pub part_of_batch: bool,
    /// Most CPU threads the export may use (0 = all of them, as before). Rendering, the software
    /// encoders and the rest of the export's parallel work run on a pool of this size; a number
    /// above what the machine has (or above the pool the export already runs in) changes nothing.
    /// The output does not depend on it. Hardware encoders and the decoders' own threads are not
    /// counted.
    #[serde(default)]
    pub max_threads: u32,
    /// ProRes flavour: `proxy`, `lt`, `standard`, `hq`, `4444` or `4444xq` (empty = HQ).
    #[serde(default)]
    pub prores_profile: String,
    /// DNxHR profile: `lb`, `sq`, `hq` or `hqx` (empty = HQ).
    #[serde(default)]
    pub dnx_profile: String,
    /// APV profile: `422-10`, `422-12`, `444-10` or `444-12` (empty = `422-10`).
    #[serde(default)]
    pub apv_profile: String,
    /// Video codec of the MXF formats (DNxHR unless set; the ProRes / DNxHR profile fields apply).
    #[serde(default)]
    pub mxf_video_codec: MxfVideoCodec,
    /// Encode display-referred SDR (Rec. 709, tone mapped) even when the sequence works in
    /// Rec. 2100 PQ/HLG. Otherwise H.264 and ProRes exports of an HDR sequence are encoded in the
    /// sequence's HDR space and signal it (VUI / `colr` / `mdcv` / `clli` / SEI).
    #[serde(default)]
    pub sdr: bool,
    /// Keep the alpha channel in PNG, TIFF, Targa and OpenEXR sequences and ProRes 4444 / 4444 XQ
    /// QuickTime movies (straight alpha, Premiere's "Include Alpha Channel"; OpenEXR stores it
    /// premultiplied, as the format defines). Off, every frame is flattened over black (#160).
    /// Other formats ignore it.
    #[serde(default)]
    pub alpha: bool,
    /// Compression of OpenEXR sequences (uncompressed unless set); other formats ignore it.
    #[serde(default)]
    pub exr_compression: ExrCompression,
    /// Output frame size (None = Match Source: the sequence size times `scale`).
    pub frame_size: Option<(u32, u32)>,
    /// Output frame rate (None = Match Source).
    pub frame_rate: Option<FrameRate>,
    /// How the picture fills a frame of another aspect ratio.
    pub scaling: Scaling,
    /// Pixel aspect ratio (None = square pixels). Signalled in the H.264 VUI.
    pub pixel_aspect: Option<(u32, u32)>,
    pub field_order: FieldOrder,
    pub h264_profile: H264Profile,
    /// H.264 level × 10 (41 = 4.1); None = the lowest level that fits. A level below what the
    /// stream needs is raised.
    pub h264_level: Option<u8>,
    pub bitrate_mode: BitrateMode,
    /// Constant rate factor of [`BitrateMode::Crf`], 0 (best) to 51 (smallest); 23 by default,
    /// 18 is visually near-lossless.
    #[serde(default = "default_crf")]
    pub crf: f32,
    /// May H.264 be encoded by the system's hardware encoder (VideoToolbox on macOS, NVENC on
    /// Windows)? Off unless asked for: hardware output depends on the machine, so it is not
    /// byte-reproducible like the built-in encoder's (`determinism_tests`).
    #[serde(default)]
    pub hardware_encoding: HardwareEncoding,
    /// Composite the exported frames on the GPU instead of the CPU reference renderer (Export ▸
    /// GPU rendering: Auto / Off, Off by default). Off = the CPU reference renderer exactly.
    #[serde(default)]
    pub gpu_rendering: GpuRendering,
    /// VBR maximum bitrate (None = 1.5 × target).
    pub max_bitrate_kbps: Option<u32>,
    /// Adaptive bitrate (the Match Source presets): bits per pixel per frame; replaces
    /// `bitrate_kbps` with `width × height × fps × bpp / 1000`.
    pub adaptive_bitrate: Option<f32>,
    /// Frames between keyframes (None = 2 seconds).
    pub keyframe_distance: Option<u32>,
    /// Render at Maximum Depth. FilmCraft always composites in 32-bit float, so this changes
    /// nothing; it is kept so presets round-trip.
    pub render_at_max_depth: bool,
    /// Use Maximum Render Quality: render at full size and scale with an area filter.
    pub max_render_quality: bool,
    pub audio: AudioSettings,
    pub multiplexer: Multiplexer,
    /// Captions ▸ Create Sidecar File: `srt` or `vtt` next to the output (written by the engine).
    pub caption_sidecar: Option<String>,
    pub effects: ExportEffects,
    pub metadata: ExportMetadata,
    /// Two-pass state (set by the exporter).
    #[serde(skip)]
    pub h264_pass: H264Pass,
    /// Colour signalling chosen by [`export`] for the encoders (not set by callers).
    #[serde(skip)]
    pub signal: ColorSignal,
    /// Encode into memory and hand each finished file (path, bytes) to this sink instead of
    /// writing `path` (hosts without a filesystem: the web app offers the file as a download).
    #[serde(skip)]
    pub sink: Option<OutputSink>,
    /// An already-created primary output file. Internal callers use this to retain an exclusive
    /// reservation made while choosing an output name, rather than reopening that path later.
    #[serde(skip)]
    pub output_file: Option<Arc<std::fs::File>>,
}

/// Colour description of the encoded stream (ITU-T H.273 code points).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ColorSignal {
    pub primaries: u8,
    pub transfer: u8,
    pub matrix: u8,
}

impl Default for ColorSignal {
    fn default() -> Self {
        ColorSignal { primaries: 1, transfer: 1, matrix: 1 }
    }
}

impl ColorSignal {
    pub const PQ: ColorSignal = ColorSignal { primaries: 9, transfer: 16, matrix: 9 };
    pub const HLG: ColorSignal = ColorSignal { primaries: 9, transfer: 18, matrix: 9 };
    pub fn is_hdr(&self) -> bool {
        matches!(self.transfer, 16 | 18)
    }
    /// (Kr, Kb) of the matrix.
    pub fn kr_kb(&self) -> (f32, f32) {
        if self.matrix == 9 { (0.2627, 0.0593) } else { (0.2126, 0.0722) }
    }
    /// Sample-entry boxes: `colr` (nclx for MP4, nclc for MOV) plus `mdcv`/`clli` for PQ
    /// (mastering display BT.2020/D65, 1000/0.0001 cd/m²; MaxCLL/MaxFALL 0 = unknown). SDR gets
    /// its `colr` too: Apple's media stack (QuickTime Player, Final Cut Pro) reads the colour of
    /// ProRes, DNxHR and APV only from this box, not from their frame headers (#2142).
    pub fn apply_to(&self, e: &mut SampleEntry, mov: bool) {
        if let Some(v) = e.video.as_mut() {
            let (p, t, m) = (self.primaries as u16, self.transfer as u16, self.matrix as u16);
            v.color = Some(if mov {
                filmcraft_isobmff::ColorInfo::Nclc { primaries: p, transfer: t, matrix: m }
            } else {
                filmcraft_isobmff::ColorInfo::Nclx { primaries: p, transfer: t, matrix: m, full_range: false }
            });
            if let Some((md, cll)) = self.static_metadata() {
                v.mastering_display = Some(md);
                v.content_light = Some(cll);
            }
        }
    }
    /// HDR10 static metadata of a PQ signal: the mastering display (BT.2020/D65, 1000 / 0.0001 cd/m²)
    /// and (MaxCLL, MaxFALL) = (0, 0), unknown. `None` for HLG and SDR. The one definition of these
    /// values: the `mdcv` / `clli` boxes and the SEI messages of every encoder come from here.
    pub fn static_metadata(&self) -> Option<(filmcraft_isobmff::MasteringDisplay, (u16, u16))> {
        (self.transfer == 16).then(|| (filmcraft_isobmff::MasteringDisplay::bt2020(1000.0, 0.0001), (0, 0)))
    }
}

impl Default for ExportSettings {
    fn default() -> Self {
        Self {
            format: Format::H264,
            path: String::new(),
            range: None,
            scale: 1.0,
            include_audio: true,
            quality: 90,
            bitrate_kbps: 20_000,
            burn_captions: false,
            part_of_batch: false,
            max_threads: 0,
            prores_profile: String::new(),
            dnx_profile: String::new(),
            apv_profile: String::new(),
            mxf_video_codec: MxfVideoCodec::default(),
            sdr: false,
            alpha: false,
            exr_compression: ExrCompression::None,
            frame_size: None,
            frame_rate: None,
            scaling: Scaling::default(),
            pixel_aspect: None,
            field_order: FieldOrder::Progressive,
            h264_profile: H264Profile::High,
            h264_level: None,
            bitrate_mode: BitrateMode::default(),
            crf: DEFAULT_CRF,
            hardware_encoding: HardwareEncoding::default(),
            gpu_rendering: GpuRendering::Off,
            max_bitrate_kbps: None,
            adaptive_bitrate: None,
            keyframe_distance: None,
            render_at_max_depth: false,
            max_render_quality: false,
            audio: AudioSettings::default(),
            multiplexer: Multiplexer::Mp4,
            caption_sidecar: None,
            effects: ExportEffects::default(),
            metadata: ExportMetadata::default(),
            h264_pass: H264Pass::Single,
            signal: ColorSignal::default(),
            sink: None,
            output_file: None,
        }
    }
}

/// [`ExportSettings::crf`] of new settings and of settings saved before it existed.
pub const DEFAULT_CRF: f32 = 23.0;

fn default_crf() -> f32 {
    DEFAULT_CRF
}

impl ExportSettings {
    fn validate_audio_settings(&self) -> Result<()> {
        if self.audio.sample_rate.is_some_and(|rate| rate == 0 || rate > 384_000) {
            return Err(ExportError::Unsupported("output sample rate must be between 1 and 384000 Hz".into()));
        }
        if self.audio.float && self.audio.bits != 32 {
            return Err(ExportError::Unsupported("floating-point audio is 32-bit: set the sample size to 32".into()));
        }
        let sample_format = self.audio.sample_format();
        if (self.has_audio() || self.format.is_audio_only()) && self.audio_codec() != AudioCodec::Aac && !self.supports_sample_format(sample_format) {
            let codec = if self.audio_codec() == AudioCodec::Flac { "FLAC" } else { self.format.label() };
            return Err(ExportError::Unsupported(format!("{codec} audio has no {} samples: choose 16, 24 or 32 bit", sample_format.label())));
        }
        Ok(())
    }

    fn validate_bitrate_mode(&self) -> Result<()> {
        if self.format == Format::Hevc && self.bitrate_mode == BitrateMode::Vbr2Pass {
            return Err(ExportError::Unsupported("H.265 export has no two-pass mode: choose CBR or VBR, 1 pass".into()));
        }
        if self.format == Format::Av1 && self.bitrate_mode == BitrateMode::Vbr2Pass {
            return Err(ExportError::Unsupported("AV1 export has no two-pass mode: choose CBR or VBR, 1 pass".into()));
        }
        if self.bitrate_mode == BitrateMode::Crf {
            if self.format == Format::Hevc {
                return Err(ExportError::Unsupported("H.265 export has no CRF mode: choose CBR or VBR, 1 pass".into()));
            }
            if self.format == Format::Av1 {
                return Err(ExportError::Unsupported("AV1 export has no CRF mode: choose CBR or VBR, 1 pass".into()));
            }
            if !self.crf.is_finite() || !(0.0..=51.0).contains(&self.crf) {
                return Err(ExportError::Unsupported("CRF must be between 0 and 51".into()));
            }
        }
        Ok(())
    }

    /// Reject settings the encoders cannot honour.
    pub fn validate(&self) -> Result<()> {
        if let Some(range) = self.range {
            validate_range(range)?;
        }
        if !self.scale.is_finite() || self.scale <= 0.0 {
            return Err(ExportError::Unsupported("output scale must be finite and positive".into()));
        }
        if let Some((width, height)) = self.frame_size {
            filmcraft_project::validate_frame_size(width, height).map_err(ExportError::Unsupported)?;
        }
        if let Some(rate) = self.frame_rate
            && (rate.num <= 0
                || rate.den <= 0
                || rate.as_f64() > 1000.0
                || rate.num > i64::from(u32::MAX)
                || rate.den > i64::from(u32::MAX)
                || (i128::from(filmcraft_time::TICKS_PER_SECOND) * i128::from(rate.den) / i128::from(rate.num.max(1))) > i128::from(Tick::MAX.0))
        {
            return Err(ExportError::Unsupported("output frame rate must be positive and at most 1000 fps".into()));
        }
        self.validate_audio_settings()?;
        if self.field_order != FieldOrder::Progressive && self.has_video() {
            return Err(ExportError::Unsupported(format!("{} field order: FilmCraft's encoders write progressive frames", self.field_order.label())));
        }
        if self.effects.image_overlay.enabled && self.effects.image_overlay.path.trim().is_empty() {
            return Err(ExportError::Unsupported("image overlay: no image file chosen".into()));
        }
        self.validate_bitrate_mode()?;
        Ok(())
    }
}
