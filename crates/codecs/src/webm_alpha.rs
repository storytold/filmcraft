//! WebM alpha (#402): a VP9 track with `AlphaMode` 1 carries, in each block's `BlockAdditional`
//! with `BlockAddID` 1, a second VP9 stream coded independently of the colour one. Its luma plane,
//! full range, is the frame's alpha (the WebM convention, as libvpx-based decoders read it).
//!
//! [`AlphaDecoder`] wraps the colour decoder: every sample it decodes, it also decodes that
//! sample's alpha frame with a second (software) VP9 decoder and attaches the luma plane to the
//! colour picture with the same timestamp, holding a colour picture back until its alpha is out. The alpha stream has key frames of its own, which need
//! not coincide with the colour ones: after a seek (or any other break in the order samples are
//! fed), the alpha decoder restarts from the nearest alpha key frame at or before the sample.
//!
//! Alpha is best effort: a missing, damaged or undecodable alpha frame leaves that picture opaque,
//! never fails the colour decode.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use filmcraft_frame::{PixelData, VideoFrame};

use crate::video::{DecodedFrame, VideoDecoder};

/// How far back the alpha decoder looks for a key frame when it resynchronises.
const MAX_RESYNC: usize = 600;
/// Decoded alpha pictures waiting for their colour picture, and colour pictures waiting for their
/// alpha (VP9 pictures come out in order, so a few are plenty; past that a picture goes out opaque
/// rather than stall).
const MAX_PENDING: usize = 8;

/// Where each sample's alpha frame is stored.
pub(crate) struct AlphaLayer {
    src: crate::Src,
    /// Per sample, in file (decode) order.
    additions: Vec<Option<(u64, u32)>>,
    pts: Vec<i64>,
    /// First sample with each pts.
    by_pts: HashMap<i64, usize>,
}

impl AlphaLayer {
    /// `samples`: (pts, addition) of every sample of the track, in file order.
    pub(crate) fn new(src: crate::Src, samples: impl Iterator<Item = (i64, Option<(u64, u32)>)>) -> Self {
        let (mut additions, mut all_pts) = (Vec::new(), Vec::new());
        let mut by_pts = HashMap::new();
        for (i, (pts, add)) in samples.enumerate() {
            additions.push(add);
            all_pts.push(pts);
            by_pts.entry(pts).or_insert(i);
        }
        Self { src, additions, pts: all_pts, by_pts }
    }

    fn read(&self, i: usize) -> Option<Vec<u8>> {
        let (offset, size) = (*self.additions.get(i)?)?;
        let end = offset.checked_add(size as u64)?;
        if end > self.src.0.len() {
            return None;
        }
        let mut data = vec![0; size as usize];
        self.src.0.read_at(offset, &mut data).ok()?;
        Some(data)
    }
}

/// A colour decoder plus the decoder of its alpha layer.
///
/// Both decoders may hand pictures back later than the sample that made them (frame threading),
/// and not necessarily after the same call. A colour picture is therefore held until its alpha
/// picture has come out, or until it is certain that it won't: the alpha decoder has moved past
/// it (pictures come out in order), it failed, or the sample has no alpha frame.
pub(crate) struct AlphaDecoder {
    color: Box<dyn VideoDecoder>,
    alpha: Box<dyn VideoDecoder>,
    layer: Arc<AlphaLayer>,
    /// Sample the alpha decoder expects next; `None` after a reset or a failure.
    next: Option<usize>,
    /// Decoded alpha pictures not yet paired, by pts.
    pending: VecDeque<(i64, VideoFrame)>,
    /// Latest pts the alpha decoder has produced since the last reset.
    alpha_max: Option<i64>,
    /// Colour pictures waiting for their alpha, in output order.
    held: VecDeque<DecodedFrame>,
}

impl AlphaDecoder {
    pub(crate) fn new(color: Box<dyn VideoDecoder>, alpha: Box<dyn VideoDecoder>, layer: Arc<AlphaLayer>) -> Self {
        Self { color, alpha, layer, next: None, pending: VecDeque::new(), alpha_max: None, held: VecDeque::new() }
    }

    fn keep(&mut self, frames: Vec<DecodedFrame>) {
        for f in frames {
            self.alpha_max = Some(self.alpha_max.map_or(f.pts, |m| m.max(f.pts)));
            self.pending.push_back((f.pts, f.frame));
        }
        while self.pending.len() > MAX_PENDING {
            self.pending.pop_front();
        }
    }

    /// Decode the alpha frame of the sample with `pts`, first catching up from an alpha key frame
    /// when the sample doesn't follow the previous one.
    fn feed_alpha(&mut self, pts: i64) {
        let Some(&i) = self.layer.by_pts.get(&pts) else {
            self.next = None;
            return;
        };
        if self.next != Some(i) {
            self.alpha.reset();
            self.pending.clear();
            self.alpha_max = None;
            let is_key = |k: usize| self.layer.read(k).is_some_and(|d| filmcraft_vp9::is_keyframe(&d));
            let start = (i.saturating_sub(MAX_RESYNC)..=i).rev().find(|&k| is_key(k));
            if let Some(start) = start {
                for k in start..i {
                    let (Some(d), Some(&p)) = (self.layer.read(k), self.layer.pts.get(k)) else { continue };
                    match self.alpha.decode(&d, p) {
                        Ok(frames) => self.keep(frames),
                        Err(_) => break,
                    }
                }
            }
        }
        self.next = Some(i + 1);
        let Some(d) = self.layer.read(i) else { return };
        match self.alpha.decode(&d, pts) {
            Ok(frames) => self.keep(frames),
            Err(e) => {
                log::debug!("WebM alpha frame at pts {pts}: {e}");
                self.next = None;
            }
        }
    }

    /// Whether the alpha picture for `pts` can still come out of the alpha decoder.
    fn alpha_may_come(&self, pts: i64) -> bool {
        let has_frame = self.layer.by_pts.get(&pts).is_some_and(|&i| self.layer.additions.get(i).is_some_and(|a| a.is_some()));
        has_frame && self.next.is_some() && self.alpha_max.is_none_or(|m| m < pts)
    }

    /// Release held colour pictures, in order, as far as their alpha is settled. `all`: release
    /// every one (flush), with or without alpha.
    fn release(&mut self, all: bool) -> Vec<DecodedFrame> {
        let mut out = Vec::new();
        while let Some(f) = self.held.front() {
            let pts = f.pts;
            let found = self.pending.iter().position(|(p, _)| *p == pts);
            if found.is_none() && !all && self.held.len() <= MAX_PENDING && self.alpha_may_come(pts) {
                break;
            }
            let Some(mut f) = self.held.pop_front() else { break };
            if let Some(k) = found
                && let Some((_, a)) = self.pending.remove(k)
            {
                set_alpha(&mut f.frame, &a);
            }
            out.push(f);
        }
        out
    }
}

impl VideoDecoder for AlphaDecoder {
    fn decode(&mut self, sample: &[u8], pts: i64) -> crate::Result<Vec<DecodedFrame>> {
        let out = self.color.decode(sample, pts)?;
        self.held.extend(out);
        self.feed_alpha(pts);
        Ok(self.release(false))
    }
    fn flush(&mut self) -> Vec<DecodedFrame> {
        let out = self.color.flush();
        self.held.extend(out);
        let rest = self.alpha.flush();
        self.keep(rest);
        self.release(true)
    }
    fn reset(&mut self) {
        self.color.reset();
        self.alpha.reset();
        self.next = None;
        self.pending.clear();
        self.alpha_max = None;
        self.held.clear();
    }
    fn name(&self) -> &str {
        self.color.name()
    }
    fn intra_only(&self) -> bool {
        self.color.intra_only()
    }
    fn is_random_access(&self, sample: &[u8]) -> Option<bool> {
        self.color.is_random_access(sample)
    }
    fn is_disposable(&self, sample: &[u8]) -> bool {
        self.color.is_disposable(sample)
    }
    fn set_draft(&mut self, on: bool) {
        self.color.set_draft(on);
        self.alpha.set_draft(on);
    }
    fn thread_limited(&self) -> bool {
        self.color.thread_limited()
    }
}

/// The luma plane of `a` (an alpha picture) as `frame`'s alpha, converted to `frame`'s bit depth.
/// Nothing happens when the sizes differ or either picture has no usable planes.
pub(crate) fn set_alpha(frame: &mut VideoFrame, a: &VideoFrame) {
    if (frame.width, frame.height) != (a.width, a.height) {
        return;
    }
    let n = frame.width as usize * frame.height as usize;
    // alpha codes as 16-bit with their depth, so every pairing converts through one path
    let (codes, abits): (Vec<u16>, u32) = match &a.data {
        PixelData::Yuv8 { planes, .. } => (planes[0].iter().map(|&v| v as u16).collect(), 8),
        PixelData::Yuv16 { planes, bits, .. } => (planes[0].to_vec(), (*bits).clamp(1, 16)),
        _ => return,
    };
    if codes.len() != n {
        return;
    }
    let rescale = |v: u16, to: u32| -> u32 {
        let (from_max, to_max) = ((1u32 << abits) - 1, (1u32 << to.clamp(1, 16)) - 1);
        (v as u32 * to_max + from_max / 2) / from_max
    };
    match &mut frame.data {
        PixelData::Yuv8 { alpha, .. } => *alpha = Some(Arc::new(codes.iter().map(|&v| rescale(v, 8) as u8).collect())),
        PixelData::Yuv16 { alpha, bits, .. } => {
            let to = *bits;
            *alpha = Some(Arc::new(codes.iter().map(|&v| rescale(v, to) as u16).collect()));
        }
        PixelData::Rgba8(rgba) => {
            let px = Arc::make_mut(rgba);
            for (p, &v) in px.as_chunks_mut::<4>().0.iter_mut().zip(&codes) {
                p[3] = rescale(v, 8) as u8;
            }
        }
        PixelData::RgbaF32(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_frame::Chroma;

    fn yuv8(w: u32, h: u32, luma: Vec<u8>) -> VideoFrame {
        let n = (w * h) as usize;
        let c = n / 4;
        let data = PixelData::Yuv8 { planes: [Arc::new(luma), Arc::new(vec![128; c]), Arc::new(vec![128; c])], chroma: Chroma::C420, alpha: None };
        VideoFrame { width: w, height: h, data, color: Default::default(), par: (1, 1), pts: filmcraft_time::Tick::ZERO }
    }

    fn yuv16(w: u32, h: u32, bits: u32) -> VideoFrame {
        let n = (w * h) as usize;
        let data =
            PixelData::Yuv16 { planes: [Arc::new(vec![0; n]), Arc::new(vec![0; n / 4]), Arc::new(vec![0; n / 4])], chroma: Chroma::C420, bits, alpha: None };
        VideoFrame { width: w, height: h, data, color: Default::default(), par: (1, 1), pts: filmcraft_time::Tick::ZERO }
    }

    #[test]
    fn alpha_luma_becomes_the_alpha_plane_at_the_frame_depth() {
        let a = yuv8(2, 2, vec![0, 128, 255, 64]);
        // 8-bit picture: the luma as is
        let mut f = yuv8(2, 2, vec![9; 4]);
        set_alpha(&mut f, &a);
        let PixelData::Yuv8 { alpha, .. } = &f.data else { panic!() };
        assert_eq!(alpha.as_deref(), Some(&vec![0, 128, 255, 64]));
        // 10-bit picture: full range kept (255 → 1023)
        let mut f = yuv16(2, 2, 10);
        set_alpha(&mut f, &a);
        let PixelData::Yuv16 { alpha, .. } = &f.data else { panic!() };
        assert_eq!(alpha.as_deref(), Some(&vec![0, 514, 1023, 257]));
        // RGBA picture (VP9 RGB profiles): the A channel
        let mut f = VideoFrame::rgba8(2, 2, vec![7; 16]);
        set_alpha(&mut f, &a);
        let PixelData::Rgba8(px) = &f.data else { panic!() };
        assert_eq!(px.chunks(4).map(|p| p[3]).collect::<Vec<_>>(), [0, 128, 255, 64]);
    }

    #[test]
    fn an_alpha_picture_of_another_size_is_ignored() {
        let mut f = yuv8(4, 2, vec![9; 8]);
        set_alpha(&mut f, &yuv8(2, 2, vec![0; 4]));
        let PixelData::Yuv8 { alpha, .. } = &f.data else { panic!() };
        assert!(alpha.is_none());
        // a luma plane shorter than the picture (damaged) is ignored too
        let mut f = yuv8(2, 2, vec![9; 4]);
        set_alpha(&mut f, &yuv8(2, 2, vec![0; 3]));
        let PixelData::Yuv8 { alpha, .. } = &f.data else { panic!() };
        assert!(alpha.is_none());
    }
}
