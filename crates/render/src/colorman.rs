//! Colour management in the renderer: source colour space resolution (Interpret Footage override
//! or stream metadata), source → working conversion of decoded frames, and the working → monitor
//! transform. The maths lives in `filmcraft_color::transform`.
//!
//! A sequence whose pipeline is "plain" (Rec. 709 working space without wide gamut) and media
//! whose metadata says Rec. 709 / sRGB / linear take the original fast path unchanged. On such a
//! sequence, display-referred HDR and wide-gamut Y'CbCr (PQ, HLG, BT.2020, P3) stays on the GPU
//! path as well, with its input transform run in the compositor's shader ([`gpu_input`]); log
//! media and the other pipelines are converted here, on the CPU.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use filmcraft_color::{ColorPipeline, ColorSpace, InputTransform, OutputTransform, Range};
use filmcraft_frame::VideoFrame;
use filmcraft_project::{ItemId, Project};

use crate::Image;

/// The Interpret Footage colour-space override of an item (following subclips to their media), or
/// for camera RAW the interpretation its development needs ([`Project::color_space_override`]).
pub fn override_of(project: &Project, item: ItemId) -> Option<ColorSpace> {
    project.resolve_media(item).and_then(|(_, media, _)| project.color_space_override(media))
}

/// The colour space a frame of `item` is interpreted in.
pub fn source_space(project: &Project, item: ItemId, frame: &VideoFrame) -> ColorSpace {
    override_of(project, item).unwrap_or_else(|| ColorSpace::from_info(&frame.color))
}

/// Whether the frame needs more than the default (transfer-only) decode.
pub fn needs_management(pipe: &ColorPipeline, cs: ColorSpace, frame: &VideoFrame) -> bool {
    let default_decode = cs == ColorSpace::from_info(&frame.color) && matches!(cs, ColorSpace::Rec709 | ColorSpace::Srgb | ColorSpace::Linear709);
    !(pipe.is_plain() && default_decode)
}

type Key = (ColorSpace, Range, ColorPipeline, Option<u32>);

/// The content peak (cd/m²) of an item's HDR metadata (MaxCLL, else the mastering display peak),
/// following subclips to their media. Used as the tone-mapping source peak of PQ media.
pub fn source_peak_nits(project: &Project, item: ItemId) -> Option<f32> {
    project.resolve_media(item).and_then(|(_, media, _)| media.info.video.as_ref()?.hdr.as_ref()?.peak_nits())
}

/// A cached input transform (`peak_nits`: the PQ content peak from the file's HDR metadata).
pub fn input_transform(cs: ColorSpace, range: Range, pipe: &ColorPipeline, peak_nits: Option<f32>) -> Arc<InputTransform> {
    static C: OnceLock<Mutex<HashMap<Key, Arc<InputTransform>>>> = OnceLock::new();
    let c = C.get_or_init(Default::default);
    let key = (cs, range, *pipe, peak_nits.map(|p| p.round() as u32));
    if let Some(t) = c.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        return t.clone();
    }
    let t = Arc::new(InputTransform::new(cs, range, pipe, key.3.map(f64::from)));
    c.lock().unwrap_or_else(|e| e.into_inner()).insert(key, t.clone());
    t
}

/// The input transform of a frame the GPU compositor can colour manage itself (#965): a Y'CbCr
/// frame of display-referred HDR or wide-gamut media (PQ, HLG, BT.2020, P3) on a plain pipeline,
/// whose working → monitor transform is the identity. None for log media (camera-gamut
/// compression), RGB frames and other pipelines, which [`decode`] converts on the CPU.
pub fn gpu_input(project: &Project, item: ItemId, pipe: &ColorPipeline, cs: ColorSpace, frame: &VideoFrame) -> Option<Arc<InputTransform>> {
    if !pipe.is_plain() || !matches!(frame.data, filmcraft_frame::PixelData::Yuv8 { .. } | filmcraft_frame::PixelData::Yuv16 { .. }) {
        return None;
    }
    let t = input_transform(cs, frame.color.range, pipe, source_peak_nits(project, item));
    t.gpu_params().map(|_| t)
}

/// Decode a frame of `item` (box-decimated by `n`) into the working space of `pipe`.
pub fn decode(project: &Project, item: ItemId, frame: &VideoFrame, n: usize, pipe: &ColorPipeline) -> Image {
    let cs = source_space(project, item, frame);
    if !needs_management(pipe, cs, frame) {
        let (w, h, px) = frame.to_linear_f32_decimated(n);
        return Image { w, h, px };
    }
    // RGB stills/generators carry full-range code values; YUV carries its own range flag
    let range = match frame.data {
        filmcraft_frame::PixelData::Rgba8(_) | filmcraft_frame::PixelData::RgbaF32(_) | filmcraft_frame::PixelData::RgbaF16(_) => Range::Full,
        _ => frame.color.range,
    };
    let t = input_transform(cs, range, pipe, source_peak_nits(project, item));
    let float_src = matches!(frame.data, filmcraft_frame::PixelData::RgbaF32(_) | filmcraft_frame::PixelData::RgbaF16(_));
    // float frames are linear already: only the gamut/tone stages apply to them
    let (w, h, px) = frame.to_linear_f32_decimated_with(n, if float_src { None } else { Some(&t.table) });
    let mut img = Image { w, h, px };
    if t.has_pixel_stage() {
        img.map_rgb(|c, _, _| t.apply(c));
    }
    img
}

/// Working space → monitor (SDR BT.709) for a non-plain pipeline; no-op otherwise.
pub fn to_display(img: &mut Image, pipe: &ColorPipeline) {
    if pipe.is_plain() {
        return;
    }
    let o = display_transform(pipe);
    if o.is_identity() {
        return;
    }
    img.map_rgb(|c, _, _| o.apply(c));
}

pub fn display_transform(pipe: &ColorPipeline) -> Arc<OutputTransform> {
    static C: OnceLock<Mutex<HashMap<ColorPipeline, Arc<OutputTransform>>>> = OnceLock::new();
    let c = C.get_or_init(Default::default);
    c.lock().unwrap_or_else(|e| e.into_inner()).entry(*pipe).or_insert_with(|| Arc::new(OutputTransform::display(pipe))).clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_color::{ColorInfo, Primaries, Transfer, WorkingSpace};

    fn frame(rgb: [u8; 3], color: ColorInfo) -> VideoFrame {
        let mut f = VideoFrame::rgba8(2, 2, [rgb[0], rgb[1], rgb[2], 255].repeat(4));
        f.color = color;
        f
    }

    #[test]
    fn mastering_metadata_sets_the_tone_mapping_peak() {
        use filmcraft_color::HdrMetadata;
        use filmcraft_media::MediaSource;
        use filmcraft_project::{ItemKind, Label, MediaClip, MediaRef};
        let pq = ColorInfo { transfer: Transfer::Pq, primaries: Primaries::Bt2020, ..ColorInfo::SRGB_FULL };
        // a PQ grey of ≈ 2000 cd/m² (code 0.83) and one of ≈ 4000 cd/m² (code 0.90)
        let px = |nits: f32| (filmcraft_color::pq_inverse_eotf(nits / 10_000.0) * 255.0).round() as u8;
        let item_with = |hdr: Option<HdrMetadata>| {
            let mut p = Project::new("t");
            let g = filmcraft_media::Generator::BlackVideo;
            let mut info = filmcraft_media::generators::GeneratorSource::new(g.clone(), 2, 2, Default::default(), filmcraft_time::Tick(1)).info().clone();
            if let Some(v) = info.video.as_mut() {
                v.color = pq;
                v.hdr = hdr;
            }
            let clip = MediaClip {
                media: MediaRef::Generator(g),
                info,
                interpret: Default::default(),
                mark_in: None,
                mark_out: None,
                markers: vec![],
                offline: false,
                proxy: None,
                identity: None,
            };
            let id = p.add_item("hdr", Label::Iris, ItemKind::Media(clip), None);
            (p, id)
        };
        let out = |hdr: Option<HdrMetadata>, nits: f32| {
            let (p, id) = item_with(hdr);
            decode(&p, id, &frame([px(nits); 3], pq), 1, &ColorPipeline::REC709).px[0]
        };
        // no metadata: a 1000 cd/m² master is assumed, so 2000 and 4000 both clip to SDR white
        assert!((out(None, 2000.0) - 1.0).abs() < 0.01 && (out(None, 4000.0) - 1.0).abs() < 0.01);
        // MaxCLL 4000: the knee spreads up to 4000, so 1000 and 2000 stay below white and 4000 reaches it
        let m4000 = HdrMetadata { max_cll: Some(4000.0), mastering_max_nits: Some(4000.0), ..Default::default() };
        let (a, a2, b) = (out(Some(m4000), 1000.0), out(Some(m4000), 2000.0), out(Some(m4000), 4000.0));
        assert!(a < out(None, 1000.0) - 0.01 && a > 0.8 && a < a2 && a2 < b && (b - 1.0).abs() < 0.02, "{a} {a2} {b}");
        // the mastering peak alone works too; MaxCLL wins when it is lower (the real content peak)
        let md = HdrMetadata { mastering_max_nits: Some(4000.0), ..Default::default() };
        assert_eq!(md.peak_nits(), Some(4000.0));
        let both = HdrMetadata { mastering_max_nits: Some(4000.0), max_cll: Some(1500.0), ..Default::default() };
        assert_eq!(both.peak_nits(), Some(1500.0));
        assert_eq!(HdrMetadata { max_cll: Some(0.0), ..Default::default() }.peak_nits(), None, "0 = unknown");
        // and reference white is no lower peak than 203
        assert_eq!(HdrMetadata { max_cll: Some(80.0), ..Default::default() }.peak_nits(), Some(203.0));
    }

    /// Camera RAW: the decoder hands over linear light in the developed primaries (the 16-bit code
    /// values of a display or log gamma linearised with the compositor's own curve), and the
    /// clip's development decides how the compositor interprets it. For every colour space and
    /// gamma Source Settings offers, that gives exactly what the compositor makes of the code
    /// values themselves; mid grey comes back as mid grey, and a BT.1886 or sRGB grey reaches the
    /// monitor as the code value the decoder wrote.
    #[test]
    fn camera_raw_outputs_are_interpreted_as_their_code_values_and_mid_grey_round_trips() {
        use filmcraft_color::transform::{decode_channel, encode_channel};
        use filmcraft_media::{MediaSource, RawColorSpace, RawDevelop, RawGamma};
        use filmcraft_project::{ItemKind, Label, MediaClip, MediaRef};
        let clip_with = |d: RawDevelop| {
            let mut p = Project::new("raw");
            let g = filmcraft_media::Generator::BlackVideo;
            let mut info = filmcraft_media::generators::GeneratorSource::new(g, 2, 1, Default::default(), filmcraft_time::Tick(1)).info().clone();
            if let Some(v) = info.video.as_mut() {
                v.codec = "R3D NE".into();
                v.color = ColorInfo { transfer: Transfer::Linear, ..ColorInfo::SRGB_FULL };
            }
            let interpret = filmcraft_project::Interpretation { develop: Some(d), ..Default::default() };
            let clip = MediaClip {
                media: MediaRef::File { path: "/footage/DSC_0001.R3D".into() },
                info,
                interpret,
                mark_in: None,
                mark_out: None,
                markers: vec![],
                offline: false,
                proxy: None,
                identity: None,
            };
            let id = p.add_item("DSC_0001.R3D", Label::Iris, ItemKind::Media(clip), None);
            (p, id)
        };
        let half = |v: f64| filmcraft_frame::f32_to_half(v as f32).to_le_bytes();
        for pipe in [ColorPipeline::REC709, ColorPipeline { working: WorkingSpace::Rec2100Pq, ..ColorPipeline::REC709 }] {
            for space in RawColorSpace::ALL {
                for gamma in RawGamma::ALL.into_iter().filter(|g| RawDevelop::supports(space, *g)) {
                    let d = RawDevelop { color_space: space, gamma, ..Default::default() };
                    let (p, id) = clip_with(d);
                    let curve = gamma.curve();
                    // a neutral mid grey and a colour, as the decoder's 16-bit code values
                    let grey = if gamma == RawGamma::Linear { 0.18 } else { (encode_channel(0.18, curve, Range::Full) * 65535.0).round() / 65535.0 };
                    let colour = [0.55, 0.40, 0.25].map(|v| if gamma == RawGamma::Linear { v * 0.4 } else { v });
                    // what the source delivers: linear light as halves, tagged linear in its primaries
                    let lin = |v: f64| if gamma == RawGamma::Linear { v } else { decode_channel(v, curve, Range::Full) };
                    let mut bytes = Vec::new();
                    for px in [[grey; 3], colour] {
                        for v in px {
                            bytes.extend(half(lin(v)));
                        }
                        bytes.extend(half(1.0));
                    }
                    let primaries = match space {
                        RawColorSpace::Rec2020 => Primaries::Bt2020,
                        RawColorSpace::P3D65 => Primaries::P3D65,
                        _ => Primaries::Bt709,
                    };
                    let mut f = VideoFrame::rgba_f16(2, 1, bytes);
                    f.color = ColorInfo { transfer: Transfer::Linear, primaries, ..ColorInfo::SRGB_FULL };
                    let got = decode(&p, id, &f, 1, &pipe);
                    // the compositor's own reading of those code values in the interpreted space
                    let cs = d.interpretation().unwrap_or(match gamma {
                        RawGamma::Linear => ColorSpace::Linear709,
                        RawGamma::Srgb => ColorSpace::Srgb,
                        _ => ColorSpace::Rec709,
                    });
                    // Rec. 709 frames need no interpretation: they say they are linear Rec. 709 (the fast path)
                    assert_eq!(source_space(&p, id, &f), d.interpretation().unwrap_or(ColorSpace::Linear709), "{space:?} {gamma:?}");
                    let t = InputTransform::new(cs, Range::Full, &pipe, None);
                    for (i, px) in [[grey; 3], colour].into_iter().enumerate() {
                        let code = px.map(|v| v as f32);
                        let want = if gamma == RawGamma::Linear { t.apply(code) } else { t.convert(code) };
                        for c in 0..3 {
                            let g = got.px[i * 4 + c];
                            assert!(
                                (g - want[c]).abs() <= 2e-3 * want[c].abs().max(0.05),
                                "{pipe:?} {space:?} {gamma:?} pixel {i} channel {c}: {g} vs {}",
                                want[c]
                            );
                        }
                    }
                    // mid grey stays mid grey (and neutral) wherever nothing tone maps it
                    let tone_mapped = cs.is_hdr() && !pipe.working.is_hdr();
                    let g = &got.px[..3];
                    assert!((g[0] - g[1]).abs() < 1e-3 && (g[1] - g[2]).abs() < 1e-3, "{space:?} {gamma:?}: grey is not neutral {g:?}");
                    if !tone_mapped && gamma != RawGamma::Hlg {
                        assert!((g[1] - 0.18).abs() < 0.18 * 3e-3, "{pipe:?} {space:?} {gamma:?}: mid grey {}", g[1]);
                    }
                    if pipe.is_plain() && matches!(gamma, RawGamma::Bt1886 | RawGamma::Srgb) && space == RawColorSpace::Rec709 {
                        let shown = filmcraft_color::linear_to_srgb(g[1]) as f64;
                        assert!((shown - grey).abs() < 1.0 / 1023.0, "{gamma:?}: the monitor shows {shown}, the decoder wrote {grey}");
                    }
                }
            }
        }
    }

    #[test]
    fn plain_media_keeps_the_fast_path() {
        let f = frame([200, 100, 50], ColorInfo::SRGB_FULL);
        assert!(!needs_management(&ColorPipeline::REC709, ColorSpace::Srgb, &f));
        let hdr = ColorPipeline { working: WorkingSpace::Rec2100Pq, ..ColorPipeline::REC709 };
        assert!(needs_management(&hdr, ColorSpace::Srgb, &f));
        let pq = ColorInfo { transfer: Transfer::Pq, primaries: Primaries::Bt2020, ..ColorInfo::SRGB_FULL };
        assert!(needs_management(&ColorPipeline::REC709, ColorSpace::from_info(&pq), &frame([1, 2, 3], pq)));
    }

    #[test]
    fn pq_frame_is_tone_mapped_into_rec709() {
        let p = Project::new("t");
        let pq = ColorInfo { transfer: Transfer::Pq, primaries: Primaries::Bt2020, ..ColorInfo::SRGB_FULL };
        // ~1000 cd/m² white (PQ 0.75) → SDR peak after BT.2390; 203 cd/m² (PQ 0.58) → ≈ 0.8
        let hi = decode(&p, ItemId(99), &frame([192, 192, 192], pq), 1, &ColorPipeline::REC709);
        assert!((hi.px[0] - 1.0).abs() < 0.03, "{:?}", &hi.px[..4]);
        let rw = decode(&p, ItemId(99), &frame([148, 148, 148], pq), 1, &ColorPipeline::REC709);
        assert!((0.7..0.9).contains(&rw.px[0]), "{:?}", &rw.px[..4]);
        // the same frame in a PQ sequence keeps its HDR values (working units, 1.0 = 203 cd/m²)
        let hdr = ColorPipeline { working: WorkingSpace::Rec2100Pq, ..ColorPipeline::REC709 };
        let w = decode(&p, ItemId(99), &frame([192, 192, 192], pq), 1, &hdr);
        assert!(w.px[0] > 4.0, "{:?}", &w.px[..4]);
        let mut d = w.clone();
        to_display(&mut d, &hdr);
        assert!((d.px[0] - 1.0).abs() < 0.03);
    }
}
