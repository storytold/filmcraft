//! Built-in export presets. These are FilmCraft's own functional definitions (names, sizes and
//! bitrates chosen by us for common delivery targets); no preset files from any other product are
//! used. User presets live in the engine (`<data dir>/export-presets.json`).

use filmcraft_time::FrameRate;
use serde::{Deserialize, Serialize};

use crate::settings::{AudioCodec, AudioSettings, BitrateMode, MxfVideoCodec, Scaling};
use crate::{ExportSettings, Format};

/// A named set of export settings (`path` and `range` are left empty).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportPreset {
    pub name: String,
    /// Browser group: "Match Source", "Web & Social", "Broadcast", "Image Sequence", "Animated
    /// GIF", "Audio Only" (user presets default to "User").
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub description: String,
    pub settings: ExportSettings,
    /// Built in (not saved, cannot be deleted).
    #[serde(skip)]
    pub builtin: bool,
}

/// Name comparison key: case- and punctuation-insensitive (so `Match Source - Adaptive High
/// Bitrate` finds `Match Source – Adaptive High Bitrate`).
pub fn preset_key(name: &str) -> String {
    name.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// The default preset (Quick Export, a fresh Export mode).
pub const DEFAULT_PRESET: &str = "Match Source – Adaptive High Bitrate";

fn h264(size: Option<(u32, u32)>, kbps: u32, max_kbps: u32) -> ExportSettings {
    ExportSettings {
        format: Format::H264,
        frame_size: size,
        bitrate_kbps: kbps,
        max_bitrate_kbps: Some(max_kbps),
        bitrate_mode: BitrateMode::Vbr1Pass,
        audio: AudioSettings { codec: AudioCodec::Aac, sample_rate: Some(48_000), channels: 2, bitrate_kbps: 320, bits: 16 },
        ..Default::default()
    }
}

fn adaptive(bpp: f32) -> ExportSettings {
    ExportSettings { adaptive_bitrate: Some(bpp), max_bitrate_kbps: None, ..h264(None, 10_000, 0) }
}

fn mov(format: Format, profile: &str) -> ExportSettings {
    let mut s = ExportSettings {
        format,
        audio: AudioSettings { codec: AudioCodec::Pcm, sample_rate: Some(48_000), channels: 2, bitrate_kbps: 320, bits: 24 },
        render_at_max_depth: true,
        ..Default::default()
    };
    match format {
        Format::ProRes => s.prores_profile = profile.into(),
        Format::Apv => s.apv_profile = profile.into(),
        _ => s.dnx_profile = profile.into(),
    }
    s
}

fn mxf(format: Format, codec: MxfVideoCodec, profile: &str) -> ExportSettings {
    let mut s = ExportSettings { mxf_video_codec: codec, ..mov(format, profile) };
    match codec {
        MxfVideoCodec::ProRes => s.prores_profile = profile.into(),
        MxfVideoCodec::Dnxhr => s.dnx_profile = profile.into(),
        MxfVideoCodec::H264 => {
            s.bitrate_kbps = 50_000;
            s.adaptive_bitrate = Some(0.3);
            s.bitrate_mode = BitrateMode::Vbr1Pass;
        }
    }
    s
}

fn audio_only(format: Format, bits: u16) -> ExportSettings {
    ExportSettings {
        format,
        audio: AudioSettings { codec: AudioCodec::Pcm, sample_rate: Some(48_000), channels: 2, bitrate_kbps: 0, bits },
        ..Default::default()
    }
}

fn p(name: &str, category: &str, description: &str, settings: ExportSettings) -> ExportPreset {
    ExportPreset { name: name.into(), category: category.into(), description: description.into(), settings, builtin: true }
}

/// Every built-in preset, in browser order.
pub fn builtin_presets() -> Vec<ExportPreset> {
    let ms = "Match Source";
    let web = "Web & Social";
    let bc = "Broadcast";
    let vertical = ExportSettings { scaling: Scaling::ScaleToFill, ..h264(Some((1080, 1920)), 12_000, 16_000) };
    vec![
        p(DEFAULT_PRESET, ms, "H.264 at the sequence's frame size and rate; bitrate adapts to the frame size (0.2 bits per pixel)", adaptive(0.2)),
        p("Match Source – Adaptive Medium Bitrate", ms, "H.264 at the sequence's frame size and rate; 0.1 bits per pixel", adaptive(0.1)),
        p("Match Source – Adaptive Low Bitrate", ms, "H.264 at the sequence's frame size and rate; 0.05 bits per pixel", adaptive(0.05)),
        p("High Quality 1080p HD", web, "H.264 1920×1080, VBR 1 pass, 20 Mbps target, AAC 320 kbps", h264(Some((1920, 1080)), 20_000, 24_000)),
        p("High Quality 2160p 4K", web, "H.264 3840×2160, VBR 1 pass, 60 Mbps target, AAC 320 kbps", h264(Some((3840, 2160)), 60_000, 72_000)),
        p("YouTube 1080p Full HD", web, "H.264 1920×1080 for video sharing sites, VBR 1 pass, 16 Mbps", h264(Some((1920, 1080)), 16_000, 20_000)),
        p("YouTube 2160p 4K Ultra HD", web, "H.264 3840×2160 for video sharing sites, VBR 1 pass, 45 Mbps", h264(Some((3840, 2160)), 45_000, 54_000)),
        p(
            "Vimeo 1080p Full HD",
            web,
            "H.264 1920×1080, VBR 2 pass, 18 Mbps",
            ExportSettings { bitrate_mode: BitrateMode::Vbr2Pass, ..h264(Some((1920, 1080)), 18_000, 24_000) },
        ),
        p(
            "Vimeo 2160p 4K Ultra HD",
            web,
            "H.264 3840×2160, VBR 2 pass, 50 Mbps",
            ExportSettings { bitrate_mode: BitrateMode::Vbr2Pass, ..h264(Some((3840, 2160)), 50_000, 65_000) },
        ),
        p("Social Vertical 1080×1920", web, "H.264 9:16 portrait for short-form social video, scaled to fill, 12 Mbps", vertical),
        p("Apple ProRes 422 HQ", bc, "QuickTime, ProRes 422 HQ, 24-bit 48 kHz PCM", mov(Format::ProRes, "hq")),
        p("Apple ProRes 422", bc, "QuickTime, ProRes 422, 24-bit 48 kHz PCM", mov(Format::ProRes, "standard")),
        p("Apple ProRes 422 LT", bc, "QuickTime, ProRes 422 LT, 24-bit 48 kHz PCM", mov(Format::ProRes, "lt")),
        p("Apple ProRes 422 Proxy", bc, "QuickTime, ProRes 422 Proxy, 24-bit 48 kHz PCM", mov(Format::ProRes, "proxy")),
        p("Apple ProRes 4444", bc, "QuickTime, ProRes 4444 (4:4:4), 24-bit 48 kHz PCM", mov(Format::ProRes, "4444")),
        p(
            "Apple ProRes 4444 with Alpha",
            bc,
            "QuickTime, ProRes 4444 (4:4:4) with alpha channel, 24-bit 48 kHz PCM",
            ExportSettings { alpha: true, ..mov(Format::ProRes, "4444") },
        ),
        p("Apple ProRes 4444 XQ", bc, "QuickTime, ProRes 4444 XQ (4:4:4), 24-bit 48 kHz PCM", mov(Format::ProRes, "4444xq")),
        p("Avid DNxHR HQ", bc, "QuickTime, DNxHR HQ (8-bit 4:2:2), 24-bit 48 kHz PCM", mov(Format::DnxHr, "hq")),
        p("Avid DNxHR SQ", bc, "QuickTime, DNxHR SQ (8-bit 4:2:2), 24-bit 48 kHz PCM", mov(Format::DnxHr, "sq")),
        p("Avid DNxHR LB", bc, "QuickTime, DNxHR LB (8-bit 4:2:2), 24-bit 48 kHz PCM", mov(Format::DnxHr, "lb")),
        p("APV 422-10", bc, "QuickTime, APV 422-10 (10-bit 4:2:2), 24-bit 48 kHz PCM", mov(Format::Apv, "422-10")),
        p("APV 422-12", bc, "QuickTime, APV 422-12 (12-bit 4:2:2), 24-bit 48 kHz PCM", mov(Format::Apv, "422-12")),
        p("MXF OP1a DNxHR HQ", bc, "MXF OP1a, DNxHR HQ (8-bit 4:2:2), 24-bit 48 kHz PCM, start timecode", mxf(Format::MxfOp1a, MxfVideoCodec::Dnxhr, "hq")),
        p("MXF OP1a ProRes 422 HQ", bc, "MXF OP1a, ProRes 422 HQ, 24-bit 48 kHz PCM, start timecode", mxf(Format::MxfOp1a, MxfVideoCodec::ProRes, "hq")),
        p("MXF OP1a H.264", bc, "MXF OP1a, H.264 High long GOP (0.3 bits per pixel), 24-bit 48 kHz PCM", mxf(Format::MxfOp1a, MxfVideoCodec::H264, "")),
        p(
            "MXF OP-Atom DNxHR (Avid)",
            bc,
            "Avid-style OP-Atom: DNxHR HQ picture file plus one 24-bit 48 kHz PCM file per channel (_A1, _A2…)",
            mxf(Format::MxfOpAtom, MxfVideoCodec::Dnxhr, "hq"),
        ),
        p(
            "PNG Sequence",
            "Image Sequence",
            "Numbered PNG stills at the sequence's frame size",
            ExportSettings { format: Format::PngSequence, ..Default::default() },
        ),
        p("TIFF Sequence", "Image Sequence", "Numbered TIFF stills (RGB, 8-bit)", ExportSettings { format: Format::TiffSequence, ..Default::default() }),
        p("BMP Sequence", "Image Sequence", "Numbered BMP stills (24-bit)", ExportSettings { format: Format::BmpSequence, ..Default::default() }),
        p(
            "Animated GIF 640×360",
            "Animated GIF",
            "Looping GIF, 640×360 at 15 fps",
            ExportSettings { format: Format::Gif, frame_size: Some((640, 360)), frame_rate: Some(FrameRate::new(15, 1)), ..Default::default() },
        ),
        p("Waveform Audio 48 kHz 16-bit", "Audio Only", "WAV, stereo 48 kHz 16-bit PCM", audio_only(Format::Wav, 16)),
        p("Waveform Audio 48 kHz 24-bit", "Audio Only", "WAV, stereo 48 kHz 24-bit PCM", audio_only(Format::Wav, 24)),
        p("AIFF 48 kHz 16-bit", "Audio Only", "AIFF, stereo 48 kHz 16-bit PCM", audio_only(Format::Aiff, 16)),
    ]
}

/// A built-in preset by name ([`preset_key`] comparison).
pub fn find_builtin(name: &str) -> Option<ExportPreset> {
    let k = preset_key(name);
    builtin_presets().into_iter().find(|p| preset_key(&p.name) == k)
}
