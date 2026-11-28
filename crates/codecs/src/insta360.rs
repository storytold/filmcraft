//! Insta360 INSV sources: an MP4 (usually two fisheye video tracks + audio) with a
//! proprietary metadata trailer, and the LRV low-resolution proxies the camera writes
//! beside them.
//!
//! The MP4 part is opened by [`Mp4Source`]; [`filmcraft_insta360`] reads the trailer at
//! end-of-file, which carries the camera model and the factory lens calibration. A file
//! whose trailer parses and whose two video tracks are the same size plays as a single
//! dual-fisheye picture: the two lens circles side by side (the layout the calibration
//! and the Immersive Video effects expect), reported as the `Insta360 INSV` container.
//! Anything else (no trailer, one track, mismatched sizes) is left to the plain MP4
//! opener.
//!
//! An `.lrv` (`LRV_<date>_<time>_01_<seg>.lrv`, beside `VID_<date>_<time>_00_<seg>.insv`)
//! is the camera's low-resolution proxy: one H.264 track holding the same dual-fisheye
//! picture at a fraction of the size, with the same trailer (its calibration and its
//! first-part reference name the `VID` recording). The circles sit in the *reverse* of
//! the INSV's track order — left = rear, right = front, verified against both lenses'
//! decoded tracks on X4 and X4 Air footage — so [`LrvSource`] swaps them before the
//! stitch. The engine attaches LRVs as INSV items' proxies on import.

use std::sync::{Arc, Mutex};

use filmcraft_frame::{AudioBuffer, Chroma, PixelData, Region, VideoFrame};
use filmcraft_media::{FrameRequest, MediaError, MediaInfo, MediaSource, SharedSource};
use filmcraft_time::Tick;

use crate::gop::GopCache;
use crate::mp4::{Mp4Source, Mp4Video};
use crate::stitch::{self, Stitch};
use crate::{CodecError, Src};

/// How an INSV's two lens circles are presented.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InsvLayout {
    /// The two fisheye circles side by side (the raw dual-fisheye layout).
    SideBySide,
    /// Stitched to equirectangular with the factory calibration (the Reframe-plugin job).
    Equirect,
}

/// The two fisheye tracks of an INSV file, decoded and placed side by side or stitched.
pub struct InsvSource {
    mp4: Mp4Source,
    info: MediaInfo,
    /// The second video track (the right lens) and its own decode cache.
    second: usize,
    second_cache: GopCache,
    /// The stitch mode: equirect when the trailer carries a usable calibration.
    layout: InsvLayout,
    /// The calibration scaled to the decoded circle, for the stitch (lens 0 = left).
    lenses: Option<[filmcraft_insta360::Lens; 2]>,
    /// The square lens circle size, to scale the calibration and bake the maps.
    circle: u32,
    /// The baked remap, made on the first stitched frame (baking is ~100 ms).
    stitch: Mutex<Option<(BakeKey, Arc<Stitch>)>>,
    /// This source's seam history (the bake is shared, the history is not).
    seam: stitch::SeamState,
}

impl InsvSource {
    pub fn open(name: &str, reader: filmcraft_media::SharedReader) -> crate::Result<Self> {
        let mp4 = Mp4Source::open_reader(name, reader.clone())?;
        let meta = filmcraft_insta360::parse(&Src(reader.clone())).map_err(|e| CodecError::Container(e.to_string()))?;
        let vtracks = mp4.vtracks.clone();
        if vtracks.len() < 2 {
            return Err(CodecError::Unsupported("not a dual-fisheye INSV".into()));
        }
        let (Some(a), Some(b)) = (mp4.info().video.clone(), mp4.track_video_size(vtracks[1])) else {
            return Err(CodecError::Unsupported("dual-fisheye INSV without two video streams".into()));
        };
        if a.width != b.width || a.height != b.height || a.width == 0 || a.height == 0 {
            return Err(CodecError::Unsupported("dual-fisheye INSV tracks differ in size".into()));
        }
        let mut video = a.clone();
        let mut info = mp4.info().clone();
        // the trailer's calibration, scaled to the decoded circle: with it the source
        // stitches to equirectangular (the Reframe-plugin job); without, the two circles
        // play side by side.
        let lenses = meta
            .as_ref()
            .and_then(|m| m.lenses)
            .filter(|_| a.width == a.height)
            .and_then(|[l0, l1]| Some([l0.scale_to_circle(0, a.width)?, l1.scale_to_circle(1, a.width)?]));
        let layout = if lenses.is_some() { InsvLayout::Equirect } else { InsvLayout::SideBySide };
        let (vw, vh) = match &lenses {
            Some(_) => {
                let (w, h) = stitch::equirect_size(a.width);
                (w, h)
            }
            None => (a.width.saturating_add(b.width), a.height),
        };
        video.width = vw;
        video.height = vh;
        video.bitrate = match (a.bitrate, b.bitrate) {
            (Some(x), Some(y)) => x.checked_add(y),
            (x, y) => x.or(y),
        };
        info.video = Some(video);
        info.container = match meta.as_ref().and_then(|m| m.model.as_deref()) {
            Some(model) => format!("Insta360 INSV ({model})"),
            None => "Insta360 INSV".into(),
        };
        let second_cache = GopCache::new(mp4.explicit_color);
        Ok(Self { mp4, info, second: vtracks[1], second_cache, layout, lenses, circle: a.width, stitch: Mutex::new(None), seam: stitch::SeamState::new() })
    }

    fn second_frame(&self, t: Tick) -> crate::Result<Arc<VideoFrame>> {
        let ti = self.second;
        let ts = i64::from(self.mp4.file.tracks.get(ti).map(|x| x.timescale.max(1)).unwrap_or(1));
        let late = filmcraft_media::cancel::catch_up().map(|m| (t - m).to_rational_round(1, ts));
        self.second_cache.frame_late(&Mp4Video { src: &self.mp4, track: ti }, t.to_rational_round(1, ts), late)
    }

    /// Stitch two circles to equirect, baking the remap once (lazily, on the first frame).
    fn stitch_frames(&self, left: &VideoFrame, right: &VideoFrame) -> Result<Arc<VideoFrame>, MediaError> {
        let lenses = self.lenses.as_ref().ok_or_else(|| MediaError::Decode("equirect layout without calibration".into()))?;
        let (w, h) = self.info.video.as_ref().map(|v| (v.width, v.height)).unwrap_or((0, 0));
        stitch_pair(&self.stitch, &self.seam, lenses, self.circle, (w, h), left, right)
    }
}

/// Stitch two circles to equirect, baking the remap once (lazily, on the first
/// frame) through the shared [`BAKES`] cache into `slot`. `lenses` are the
/// calibration scaled to the `circle`-pixel circles, in the order [`Stitch::apply`]
/// wants them: `left` = lens 0 = front, `right` = lens 1 = rear. `out` is the
/// equirectangular size to bake for.
fn stitch_pair(
    slot: &Mutex<Option<(BakeKey, Arc<Stitch>)>>,
    seam: &stitch::SeamState,
    lenses: &[filmcraft_insta360::Lens; 2],
    circle: u32,
    out: (u32, u32),
    left: &VideoFrame,
    right: &VideoFrame,
) -> Result<Arc<VideoFrame>, MediaError> {
    let key = BakeKey { lenses: *lenses, circle, out, chroma: left.chroma() };
    let s = {
        let mut g = slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        match g.as_ref().filter(|(k, _)| *k == key).map(|(_, s)| s.clone()) {
            Some(s) => s,
            None => {
                let s = cached_bake(key.clone()).ok_or_else(|| MediaError::Decode("could not bake the Insta360 stitch".into()))?;
                *g = Some((key, s.clone()));
                s
            }
        }
    };
    s.apply(seam, left, right).ok_or_else(|| MediaError::Decode("Insta360 stitch failed".into()))
}

/// What a baked remap depends on: the calibration, the circle size, the equirect
/// output size and the chroma layout. Every part of one spanned recording bakes
/// identically from these.
#[derive(Clone, PartialEq)]
struct BakeKey {
    lenses: [filmcraft_insta360::Lens; 2],
    circle: u32,
    out: (u32, u32),
    chroma: Chroma,
}

impl BakeKey {
    fn bake(&self) -> Option<Arc<Stitch>> {
        let (w, h) = (usize::try_from(self.out.0).ok()?, usize::try_from(self.out.1).ok()?);
        Stitch::bake(&self.lenses, self.circle, w, h, self.chroma).map(Arc::new)
    }
}

/// Recent bakes, as long as something still uses them. Playing a multi-part
/// recording used to leave one ~240 MB remap set resident per part; every part
/// of one recording bakes identically, so this cache hands one bake to all of
/// them. Entries are `Weak`: a bake lives exactly as long as the sources
/// referencing it (each part used to own a duplicate; now they share one), and
/// once a recording is fully closed its entry dies here rather than pinning
/// hundreds of megabytes until a capacity limit evicts it. A revisit after
/// every holder is gone simply re-bakes (~1 s, amortised over the part it
/// serves).
static BAKES: Mutex<Vec<(BakeKey, std::sync::Weak<Stitch>)>> = Mutex::new(Vec::new());

fn cached_bake(key: BakeKey) -> Option<Arc<Stitch>> {
    let mut b = BAKES.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    b.retain(|(_, w)| w.strong_count() > 0);
    if let Some(i) = b.iter().position(|(k, _)| *k == key) {
        let s = b[i].1.upgrade()?;
        let e = b.remove(i);
        b.insert(0, e);
        return Some(s);
    }
    let s = key.bake()?;
    b.insert(0, (key, std::sync::Arc::downgrade(&s)));
    Some(s)
}

/// The low-resolution proxy the camera writes beside an INSV recording (`LRV_…_.lrv`):
/// one H.264 track holding the two lens circles side by side in the *reverse* of the
/// INSV's track order — left = rear, right = front, verified against both lenses'
/// decoded tracks on X4 and X4 Air footage — with the same trailer and calibration. It
/// plays as the stitched equirectangular picture so it can stand in for the
/// full-resolution clip as its proxy (the engine attaches it on import).
pub struct LrvSource {
    mp4: Mp4Source,
    info: MediaInfo,
    /// The calibration scaled to the decoded circle, in the trailer's sensor order
    /// (lens 0 = front = the frame's *right* half here, lens 1 = rear = the left).
    lenses: [filmcraft_insta360::Lens; 2],
    /// The square circle size within the frame (half its width).
    circle: u32,
    /// The baked remap, made on the first stitched frame (baking is ~100 ms).
    stitch: Mutex<Option<(BakeKey, Arc<Stitch>)>>,
    /// This source's seam history (the bake is shared, the history is not).
    seam: stitch::SeamState,
}

impl LrvSource {
    pub fn open(name: &str, reader: filmcraft_media::SharedReader) -> crate::Result<Self> {
        let mp4 = Mp4Source::open_reader(name, reader.clone())?;
        let meta = filmcraft_insta360::parse(&Src(reader.clone())).map_err(|e| CodecError::Container(e.to_string()))?;
        if mp4.vtracks.len() != 1 {
            return Err(CodecError::Unsupported("not a single-track LRV".into()));
        }
        let Some(v) = mp4.info().video.clone() else {
            return Err(CodecError::Unsupported("LRV without a video stream".into()));
        };
        // the two circles fill the frame side by side: square, half its width each
        let Some(circle) = v.width.checked_div(2) else { return Err(CodecError::Unsupported("LRV frame too small".into())) };
        if circle < 8 || v.width % 2 != 0 || circle != v.height {
            return Err(CodecError::Unsupported("LRV frame is not a side-by-side circle pair".into()));
        }
        // without the trailer's calibration there is no projection to stitch by, and the
        // file is just a plain MP4: leave it to the MP4 opener
        let Some(lenses) = meta.as_ref().and_then(|m| m.lenses).and_then(|[l0, l1]| Some([l0.scale_to_circle(0, circle)?, l1.scale_to_circle(1, circle)?]))
        else {
            return Err(CodecError::Unsupported("LRV without a usable calibration".into()));
        };
        let (w, h) = stitch::equirect_size(circle);
        let mut video = v.clone();
        let mut info = mp4.info().clone();
        video.width = w;
        video.height = h;
        info.video = Some(video);
        info.container = match meta.as_ref().and_then(|m| m.model.as_deref()) {
            Some(model) => format!("Insta360 LRV ({model})"),
            None => "Insta360 LRV".into(),
        };
        Ok(Self { mp4, info, lenses, circle, stitch: Mutex::new(None), seam: stitch::SeamState::new() })
    }
}

impl MediaSource for LrvSource {
    fn info(&self) -> &MediaInfo {
        &self.info
    }

    fn video_frame(&self, req: FrameRequest) -> Result<Arc<VideoFrame>, MediaError> {
        let t = req.time.max(Tick::ZERO);
        let f = self.mp4.video_at(t)?;
        // the frame's halves are reversed relative to the calibration's sensor order:
        // the left circle is the rear lens, the right one the front
        let (cw, ch) = (f.width as usize, f.height as usize);
        let Some(circle) = cw.checked_div(2) else { return Err(MediaError::Decode("LRV frame too small".into())) };
        if circle != ch {
            return Err(MediaError::Decode("LRV frame is not a circle pair".into()));
        }
        let rear = f.cropped(Region { x: 0, y: 0, w: circle, h: ch });
        let front = f.cropped(Region { x: circle, y: 0, w: circle, h: ch });
        let (ew, eh) = self.info.video.as_ref().map(|v| (v.width, v.height)).unwrap_or((0, 0));
        stitch_pair(&self.stitch, &self.seam, &self.lenses, self.circle, (ew, eh), &front, &rear)
    }

    fn audio(&self, start: i64, frames: usize, sample_rate: u32) -> Result<AudioBuffer, MediaError> {
        self.mp4.audio(start, frames, sample_rate)
    }

    fn audio_stream(&self, stream: usize, start: i64, frames: usize, sample_rate: u32) -> Result<AudioBuffer, MediaError> {
        self.mp4.audio_stream(stream, start, frames, sample_rate)
    }
}

impl MediaSource for InsvSource {
    fn info(&self) -> &MediaInfo {
        &self.info
    }

    fn video_frame(&self, req: FrameRequest) -> Result<Arc<VideoFrame>, MediaError> {
        let t = req.time.max(Tick::ZERO);
        let left = self.mp4.video_at(t)?;
        let right = self.second_frame(t)?;
        match self.layout {
            InsvLayout::Equirect => self.stitch_frames(&left, &right),
            InsvLayout::SideBySide => hstack(&left, &right).ok_or_else(|| MediaError::Decode("dual-fisheye frames do not stack".into())),
        }
    }

    fn audio(&self, start: i64, frames: usize, sample_rate: u32) -> Result<AudioBuffer, MediaError> {
        self.mp4.audio(start, frames, sample_rate)
    }

    fn audio_stream(&self, stream: usize, start: i64, frames: usize, sample_rate: u32) -> Result<AudioBuffer, MediaError> {
        self.mp4.audio_stream(stream, start, frames, sample_rate)
    }
}

/// Two same-height frames side by side: planar for planar (lossless) when the
/// join is chroma-aligned, RGBA otherwise.
fn hstack(a: &VideoFrame, b: &VideoFrame) -> Option<Arc<VideoFrame>> {
    if a.height != b.height || a.width == 0 || b.width == 0 {
        return None;
    }
    let (w, h) = (a.width.checked_add(b.width)?, a.height);
    let (wu, hu) = (usize::try_from(w).ok()?, usize::try_from(h).ok()?);
    let (au, bu) = (usize::try_from(a.width).ok()?, usize::try_from(b.width).ok()?);
    let out = match (&a.data, &b.data) {
        (PixelData::Yuv8 { planes: pa, chroma: ca, .. }, PixelData::Yuv8 { planes: pb, chroma: cb, .. }) if ca == cb && chroma_join_ok(wu, hu, au, *ca) => {
            let (sw, sh) = ca.shifts();
            let mut planes = Vec::new();
            for (i, (x, y)) in pa.iter().zip(pb.iter()).enumerate() {
                // luma (plane 0) is full size; chroma is subsampled by the format's shifts
                let (sw, sh) = if i == 0 { (0, 0) } else { (sw, sh) };
                let (iw, ih) = (wu >> sw, hu >> sh);
                let (aw, bw) = (au >> sw, bu >> sw);
                let mut o = vec![0u8; iw.checked_mul(ih)?];
                for r in 0..ih {
                    let (ra, rb) = (x.get(r * aw..r * aw + aw)?, y.get(r * bw..r * bw + bw)?);
                    let row = r.checked_mul(iw)?;
                    o[row..row + aw].copy_from_slice(ra);
                    o[row + aw..row + iw].copy_from_slice(rb);
                }
                planes.push(Arc::new(o));
            }
            let [y, u, v] = <[Arc<Vec<u8>>; 3]>::try_from(planes).ok()?;
            VideoFrame { width: w, height: h, data: PixelData::Yuv8 { planes: [y, u, v], chroma: *ca, alpha: None }, color: a.color, par: (1, 1), pts: a.pts }
        }
        _ => {
            let (ra, rb) = (a.to_rgba8(), b.to_rgba8());
            let (aw, bw) = (au * 4, bu * 4);
            let mut o = vec![0u8; wu.checked_mul(hu)?.checked_mul(4)?];
            for r in 0..hu {
                let (sa, sb) = (r * aw, r * bw);
                let row = r * wu * 4;
                let (da, db) = (row, row + aw);
                o[da..db].copy_from_slice(ra.get(sa..sa + aw)?);
                o[db..db + bw].copy_from_slice(rb.get(sb..sb + bw)?);
            }
            VideoFrame::rgba8(w, h, o)
        }
    };
    Some(Arc::new(out.with_pts(a.pts)))
}

/// Whether the planar `hstack` can copy chroma exactly: on every axis the
/// format subsamples, a half width must divide evenly into whole chroma columns,
/// so the halves' `width >> shift` equals each source's true (rounding-up) chroma
/// width and the two together fill the joined row, and the joined height must
/// divide evenly into whole chroma ROWS, or the `height >> shift` the copy spans
/// would leave the last two luma picture rows of the output without a chroma row
/// of their own. Odd sizes fall through to the RGBA path instead of panicking in
/// `copy_from_slice` (width) or emitting a short plane (height).
fn chroma_join_ok(wu: usize, hu: usize, au: usize, chroma: Chroma) -> bool {
    let (sx, sy) = chroma.shifts();
    (sx == 0 || (wu & 1 == 0 && au & 1 == 0)) && (sy == 0 || (wu & 1 == 0 && hu & 1 == 0))
}

/// INSV files are named by the camera (`VID_…_.insv`); the extension is the cheap gate
/// before reading the trailer.
fn is_insv_name(name: &str) -> bool {
    name.rsplit('.').next().is_some_and(|e| e.eq_ignore_ascii_case("insv"))
}

/// LRV proxy files are named by the camera beside their recording (`LRV_…_.lrv`).
fn is_lrv_name(name: &str) -> bool {
    name.rsplit('.').next().is_some_and(|e| e.eq_ignore_ascii_case("lrv"))
}

/// Whether the last bytes of a file carry the INSV trailer signature.
fn has_trailer(tail: &[u8]) -> bool {
    tail.len() >= 72 && tail.get(tail.len() - 32..) == Some(filmcraft_insta360::SIGNATURE_BYTES)
}

pub fn opener(name: &str, bytes: Arc<[u8]>) -> Option<Result<SharedSource, MediaError>> {
    if !is_insv_name(name) || bytes.len() < 72 {
        return None;
    }
    if !has_trailer(&bytes[bytes.len() - 72..]) {
        return None;
    }
    let r: filmcraft_media::SharedReader = Arc::new(filmcraft_media::reader::MemReader(bytes));
    match InsvSource::open(name, r) {
        Ok(s) => Some(Ok(Arc::new(s) as SharedSource)),
        // not the dual-fisheye layout after all: let the MP4 opener take it
        Err(CodecError::Unsupported(_)) => None,
        Err(e) => Some(Err(e.into())),
    }
}

pub fn reader_opener(name: &str, head: &[u8], reader: &filmcraft_media::SharedReader) -> Option<Result<SharedSource, MediaError>> {
    if !is_insv_name(name) || !crate::mp4::sniff(head) || reader.len() < 72 {
        return None;
    }
    let mut tail = [0u8; 72];
    if let Err(e) = reader.read_at(reader.len() - 72, &mut tail) {
        // the file is still being fetched: say so (the pending flag is set) so the
        // dispatcher shows a slate and retries, rather than passing the file to the
        // plain MP4 opener, whose Ok it would cache unstitched
        return filmcraft_media::pending::is_set().then_some(Err(MediaError::Io(e.to_string())));
    }
    if !has_trailer(&tail) {
        return None;
    }
    match InsvSource::open(name, reader.clone()) {
        Ok(s) => Some(Ok(Arc::new(s) as SharedSource)),
        Err(CodecError::Unsupported(_)) => None,
        Err(e) => Some(Err(e.into())),
    }
}

pub fn lrv_opener(name: &str, bytes: Arc<[u8]>) -> Option<Result<SharedSource, MediaError>> {
    if !is_lrv_name(name) || bytes.len() < 72 {
        return None;
    }
    if !has_trailer(&bytes[bytes.len() - 72..]) {
        return None;
    }
    let r: filmcraft_media::SharedReader = Arc::new(filmcraft_media::reader::MemReader(bytes));
    match LrvSource::open(name, r) {
        Ok(s) => Some(Ok(Arc::new(s) as SharedSource)),
        // not the circle-pair layout after all: let the MP4 opener take it
        Err(CodecError::Unsupported(_)) => None,
        Err(e) => Some(Err(e.into())),
    }
}

pub fn lrv_reader_opener(name: &str, head: &[u8], reader: &filmcraft_media::SharedReader) -> Option<Result<SharedSource, MediaError>> {
    if !is_lrv_name(name) || !crate::mp4::sniff(head) || reader.len() < 72 {
        return None;
    }
    let mut tail = [0u8; 72];
    if let Err(e) = reader.read_at(reader.len() - 72, &mut tail) {
        // as in `reader_opener`: a still-fetching file must come back, not be cached as one
        return filmcraft_media::pending::is_set().then_some(Err(MediaError::Io(e.to_string())));
    }
    if !has_trailer(&tail) {
        return None;
    }
    match LrvSource::open(name, reader.clone()) {
        Ok(s) => Some(Ok(Arc::new(s) as SharedSource)),
        Err(CodecError::Unsupported(_)) => None,
        Err(e) => Some(Err(e.into())),
    }
}

/// Split the name of a spanned-recording part into its recording prefix and
/// segment number, whatever the file's extension: `VID_20261010_205908_00_055.insv`
/// -> `("VID_20261010_205908_00_", 55)`, and equally `VID_20261010_205908_00_055.mp4`
/// for a recording the user renamed or re-wrapped: the camera's naming survives, and
/// name-level matching (siblings, proxies) should too. None for any other name.
/// [`spanned_part`] adds the camera's own extension rule on top.
pub fn recording_part(name: &str) -> Option<(String, u32)> {
    let (stem, _ext) = name.rsplit_once('.')?;
    let stem = stem.rsplit(['/', '\\']).next().unwrap_or(stem);
    // VID_/LRV_ + 8-digit date + _ + 6-digit time + _ + 2-digit lens + _ +
    // 3-digit segment
    if stem.len() < 26 || !(stem.get(..4).is_some_and(|s| s.eq_ignore_ascii_case("VID_")) || stem.get(..4).is_some_and(|s| s.eq_ignore_ascii_case("LRV_"))) {
        return None;
    }
    let (head, seg) = stem.rsplit_once('_')?;
    if seg.len() != 3 || !seg.as_bytes().iter().all(u8::is_ascii_digit) {
        return None;
    }
    let (head, lens) = head.rsplit_once('_')?;
    if lens.len() != 2 || !lens.as_bytes().iter().all(u8::is_ascii_digit) {
        return None;
    }
    let (date, time) = head.get(4..)?.split_once('_')?;
    if date.len() != 8 || time.len() != 6 || !date.as_bytes().iter().all(u8::is_ascii_digit) || !time.as_bytes().iter().all(u8::is_ascii_digit) {
        return None;
    }
    Some((format!("{head}_{lens}_"), seg.parse().ok()?))
}

/// Split the name of a spanned-recording part into its recording prefix and
/// segment number: `VID_20261010_205908_00_055.insv` -> `("VID_20261010_205908_00_", 55)`, or
/// `LRV_20261010_205908_01_055.lrv` -> `("LRV_20261010_205908_01_", 55)` for the camera's
/// low-resolution proxy chain. The camera names parts `VID_<date>_<time>_<lens>_<seg>.insv`
/// (and their `LRV_..._.lrv` twins) and plays them back to back; None for any other name
/// (single-file recordings use their own naming, and other extensions go through
/// [`recording_part`]).
pub fn spanned_part(name: &str) -> Option<(String, u32)> {
    let (_stem, ext) = name.rsplit_once('.')?;
    let family = match ext.to_ascii_lowercase().as_str() {
        "insv" => "VID_",
        "lrv" => "LRV_",
        _ => return None,
    };
    let (prefix, seg) = recording_part(name)?;
    (prefix.starts_with(family)).then_some((prefix, seg))
}

/// The recording a [`spanned_part`] prefix belongs to: `VID_<date>_<time>_00_` and
/// `LRV_<date>_<time>_01_` are the same recording (the camera writes its
/// low-resolution proxy beside the main file, with its own family and stream fields),
/// so the key keeps only the `<date>_<time>` part. None for any other shape.
pub fn recording_key(prefix: &str) -> Option<&str> {
    // <VID|LRV>_ + 8-digit date + _ + 6-digit time + _
    let rest = prefix.get(4..prefix.len())?;
    let (key, tail) = rest.split_once('_')?;
    // tail: 6-digit time + _ + 2-digit stream + _
    if key.len() != 8
        || !key.as_bytes().iter().all(u8::is_ascii_digit)
        || tail.len() != 10
        || !tail.as_bytes()[..6].iter().all(u8::is_ascii_digit)
        || tail.as_bytes()[6] != b'_'
        || !tail.as_bytes()[7..9].iter().all(u8::is_ascii_digit)
        || !tail.ends_with('_')
    {
        return None;
    }
    rest.get(..15)
}

/// The X4 factory calibration as a camera writes it (INFO field 54, `OffsetV3`): two
/// lenses, side-by-side circles on a 16000×6000 sensor.
#[doc(hidden)]
pub const X4_OFFSET: &str = "2_1.948170_4623.310_4622.280_4005.910_3011.090_0.360_0.315_90.224_0.000000_0.000000_0.000000_0.39570114_1.25954950_-3.94179916_-0.00181744_0.00048328_16000_6000_71_1.948170_4615.100_4615.490_12009.110_2982.290_-0.492_0.369_89.702_0.000327_-0.000021_-0.031486_0.38433224_1.30846024_-4.00595903_0.00127593_0.00029782_16000_6000_71_197632";

/// A small synthetic dual-lens INSV for tests (also used by the engine's): an MP4 with
/// two same-size 64×32 ProRes tracks — `frames` samples each at 25 fps, lens 0 bright on
/// its left, lens 1 mirrored (or both flipped with `flip`) — plus a trailer naming the
/// model and calibration. `first_part` adds INFO field 26, marking a spanned recording.
#[doc(hidden)]
pub fn synthetic_insv(first_part: Option<&str>, frames: u32, flip: bool) -> Result<Vec<u8>, String> {
    synthetic_insv_circles(first_part, frames, flip, 64, 32)
}

/// The same with `w`×`h` circles (square ones are what the equirect stitch needs):
/// lens 0 (the front, the left circle) bright on its left half, lens 1 (the rear) on its
/// right, or swapped with `flip`.
#[doc(hidden)]
pub fn synthetic_insv_circles(first_part: Option<&str>, frames: u32, flip: bool, w: u32, h: u32) -> Result<Vec<u8>, String> {
    use filmcraft_isobmff::{Brand, FourCc, Mp4Writer, SampleEntry, TrackConfig, WriteSample, WriterOptions};

    let (a, b) = (circle_frame(w, h, !flip)?, circle_frame(w, h, flip)?);
    let mut mux = Mp4Writer::new(std::io::Cursor::new(Vec::new()), WriterOptions::new(Brand::Mp4)).map_err(|e| e.to_string())?;
    let mut tracks = Vec::new();
    for _ in 0..2 {
        tracks.push(mux.add_track(TrackConfig::new(SampleEntry::prores(FourCc(*b"apch"), w as u16, h as u16), 25)).map_err(|e| e.to_string())?);
    }
    for (ti, d) in tracks.iter().zip([&a, &b]) {
        for _ in 0..frames.max(1) {
            mux.write_sample(*ti, WriteSample { data: d, duration: 1, composition_offset: 0, is_sync: true }).map_err(|e| e.to_string())?;
        }
    }
    let mut bytes = mux.finish().map_err(|e| e.to_string())?.into_inner();
    append_trailer(&mut bytes, first_part);
    Ok(bytes)
}

/// A small synthetic LRV proxy for tests: one `2c`×`c` ProRes track — two square circles
/// side by side in the camera's reversed order (left = rear, right = front), `frames`
/// samples at 25 fps, `flip` swapping the halves as [`synthetic_insv`] does — plus the
/// same trailer as the main files. `first_part` marks a spanned recording (the camera's
/// LRV trailers name the recording's first MAIN part).
///
/// The circles are one shade dimmer than [`synthetic_insv_circles`'] (a proxy is a
/// re-encode, never bit-identical), so a test can tell the two chains apart by level
/// while still checking they show the same layout.
#[doc(hidden)]
pub fn synthetic_lrv(first_part: Option<&str>, frames: u32, flip: bool) -> Result<Vec<u8>, String> {
    use filmcraft_isobmff::{Brand, FourCc, Mp4Writer, SampleEntry, TrackConfig, WriteSample, WriterOptions};

    let c = 64u32;
    let (rear, front) = (lrv_circle(c, flip)?, lrv_circle(c, !flip)?);
    let f = hstack_encoded(c, &rear, &front)?;
    let mut mux = Mp4Writer::new(std::io::Cursor::new(Vec::new()), WriterOptions::new(Brand::Mp4)).map_err(|e| e.to_string())?;
    let ti = mux.add_track(TrackConfig::new(SampleEntry::prores(FourCc(*b"apch"), (c * 2) as u16, c as u16), 25)).map_err(|e| e.to_string())?;
    for _ in 0..frames.max(1) {
        mux.write_sample(ti, WriteSample { data: &f, duration: 1, composition_offset: 0, is_sync: true }).map_err(|e| e.to_string())?;
    }
    let mut bytes = mux.finish().map_err(|e| e.to_string())?.into_inner();
    append_trailer(&mut bytes, first_part);
    Ok(bytes)
}

/// One synthetic lens circle: half its width bright, half dark (`bright_left` picks
/// which), encoded as ProRes.
fn circle_frame(w: u32, h: u32, bright_left: bool) -> Result<Vec<u8>, String> {
    circle_frame_at(w, h, bright_left, 800)
}

/// The same with an explicit bright level (the LRV's circles sit a shade lower).
fn circle_frame_at(w: u32, h: u32, bright_left: bool, bright: u16) -> Result<Vec<u8>, String> {
    let mut fr = craft_video::prores::Frame::new(w, h, craft_video::prores::ChromaFormat::Yuv422, 10, false).map_err(|e| e.to_string())?;
    for (i, y) in fr.y.iter_mut().enumerate() {
        *y = if ((i as u32 % w) < w / 2) == bright_left { bright } else { 100 };
    }
    let mut enc = craft_video::prores::Encoder::new(craft_video::prores::Profile::Hq, w, h);
    enc.encode(&fr).map_err(|e| e.to_string())
}

/// One LRV circle: the same split as [`circle_frame`] at the proxy's level.
fn lrv_circle(c: u32, bright_left: bool) -> Result<Vec<u8>, String> {
    circle_frame_at(c, c, bright_left, 700)
}

/// Two same-size decoded ProRes frames side by side, re-encoded (for the LRV's single
/// circle-pair track).
fn hstack_encoded(c: u32, left: &[u8], right: &[u8]) -> Result<Vec<u8>, String> {
    let (l, r) = (craft_video::prores::decode_frame(left).map_err(|e| e.to_string())?, craft_video::prores::decode_frame(right).map_err(|e| e.to_string())?);
    let mut fr = craft_video::prores::Frame::new(c * 2, c, craft_video::prores::ChromaFormat::Yuv422, 10, false).map_err(|e| e.to_string())?;
    for y in 0..c as usize {
        for x in 0..c as usize {
            fr.y[y * (c * 2) as usize + x] = l.y[y * c as usize + x];
            fr.y[y * (c * 2) as usize + x + c as usize] = r.y[y * c as usize + x];
        }
    }
    let mut enc = craft_video::prores::Encoder::new(craft_video::prores::Profile::Hq, c * 2, c);
    enc.encode(&fr).map_err(|e| e.to_string())
}

/// Append the metadata trailer (INFO frame: model + OffsetV3 calibration, optional
/// first-part path, then the 72-byte header) to an MP4's bytes.
fn append_trailer(bytes: &mut Vec<u8>, first_part: Option<&str>) {
    let mut info = pb_field(2, b"Insta360 X4");
    info.extend(pb_field(54, X4_OFFSET.as_bytes()));
    if let Some(fp) = first_part {
        info.extend(pb_field(26, &pb_field(3, fp.as_bytes())));
    }
    let meta_start = bytes.len() as u64;
    bytes.extend_from_slice(&info);
    bytes.extend_from_slice(&[1, 1]);
    bytes.extend_from_slice(&(info.len() as i32).to_le_bytes());
    let mut head = vec![0u8; 72];
    head[32..36].copy_from_slice(&((bytes.len() - meta_start as usize + 72) as i32).to_le_bytes());
    head[36..40].copy_from_slice(&3i32.to_le_bytes());
    head[40..72].copy_from_slice(filmcraft_insta360::SIGNATURE_BYTES);
    bytes.extend_from_slice(&head);
}

fn pb_field(field: u32, payload: &[u8]) -> Vec<u8> {
    let mut v = varint_bytes((u64::from(field) << 3) | 2);
    v.extend(varint_bytes(payload.len() as u64));
    v.extend_from_slice(payload);
    v
}

fn varint_bytes(mut v: u64) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return out;
        }
        out.push(b | 0x80);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_frame::Chroma;

    #[test]
    fn name_gate_is_case_insensitive() {
        assert!(is_insv_name("VID_20261010_205908_00_055.insv"));
        assert!(is_insv_name("CLIP.INSV"));
        assert!(!is_insv_name("clip.mp4"));
        assert!(!is_insv_name("noext"));
    }

    #[test]
    fn trailer_gate_checks_the_signature() {
        let mut tail = [0u8; 72];
        assert!(!has_trailer(&tail));
        tail[40..72].copy_from_slice(filmcraft_insta360::SIGNATURE_BYTES);
        assert!(has_trailer(&tail));
    }

    /// Two 4×2 YUV420 frames stacked: the right half of the result is the second frame.
    #[test]
    fn hstack_places_the_right_lens_on_the_right() {
        let mk = |v: u8| VideoFrame {
            width: 4,
            height: 2,
            data: PixelData::Yuv8 { planes: [Arc::new(vec![v; 8]), Arc::new(vec![128; 2]), Arc::new(vec![128; 2])], chroma: Chroma::C420, alpha: None },
            color: filmcraft_color::ColorInfo::REC709,
            par: (1, 1),
            pts: Tick::ZERO,
        };
        let out = hstack(&mk(10), &mk(200)).expect("stack");
        assert_eq!((out.width, out.height), (8, 2));
        let PixelData::Yuv8 { planes, chroma, .. } = &out.data else { panic!("yuv expected") };
        assert_eq!(*chroma, Chroma::C420);
        assert_eq!(&planes[0][..4], &[10, 10, 10, 10]);
        assert_eq!(&planes[0][4..8], &[200, 200, 200, 200]);
        assert_eq!(&planes[0][8..12], &[10, 10, 10, 10]);
        assert_eq!(&planes[0][12..16], &[200, 200, 200, 200]);
        assert_eq!(planes[1].len(), 4);
    }

    /// Two odd-width C420 frames: the planar join would copy 31 samples into a
    /// 32-sample row and panic, so the result comes through the RGBA path.
    #[test]
    fn hstack_of_odd_width_chroma_frames_joins_without_panicking() {
        let mk = |v: u8| VideoFrame {
            width: 63,
            height: 32,
            data: PixelData::Yuv8 {
                planes: [Arc::new(vec![v; 63 * 32]), Arc::new(vec![128; 32 * 16]), Arc::new(vec![128; 32 * 16])],
                chroma: Chroma::C420,
                alpha: None,
            },
            color: filmcraft_color::ColorInfo::REC709,
            par: (1, 1),
            pts: Tick::ZERO,
        };
        let out = hstack(&mk(10), &mk(200)).expect("stack even through RGBA");
        assert_eq!((out.width, out.height), (126, 32));
        let rgba = out.to_rgba8();
        let px = |x: usize, y: usize| rgba[(y * 126 + x) * 4];
        assert!(px(4, 4) + 100 < px(70, 4), "each half keeps its frame's luma: {} vs {}", px(4, 4), px(70, 4));
    }

    /// Two odd-height C420 frames: the planar copy would write 16 chroma rows
    /// into a 33-row picture that needs 17, leaving the last luma rows of the
    /// output without a chroma row of their own - so the join goes through RGBA.
    #[test]
    fn hstack_of_odd_height_chroma_frames_keeps_every_chroma_row() {
        let mk = |v: u8| VideoFrame {
            width: 64,
            height: 33,
            data: PixelData::Yuv8 {
                planes: [Arc::new(vec![v; 64 * 33]), Arc::new(vec![128; 32 * 17]), Arc::new(vec![128; 32 * 17])],
                chroma: Chroma::C420,
                alpha: None,
            },
            color: filmcraft_color::ColorInfo::REC709,
            par: (1, 1),
            pts: Tick::ZERO,
        };
        let out = hstack(&mk(10), &mk(200)).expect("stack through RGBA");
        assert_eq!((out.width, out.height), (128, 33));
        let rgba = out.to_rgba8();
        let px = |x: usize, y: usize| rgba[(y * 128 + x) * 4];
        assert!(px(4, 30) + 100 < px(70, 30), "the picture's last rows keep both halves' luma");
        assert!(px(4, 4) + 100 < px(70, 4));
    }

    /// A whole INSV built by the shared helper: it opens as one stacked dual-fisheye
    /// source and decodes, lens 0 bright-left and lens 1 mirrored.
    #[test]
    fn synthetic_insv_stacks_two_tracks() {
        let bytes = synthetic_insv(None, 1, false).expect("build");
        let src = opener("clip.insv", bytes.into()).expect("claimed").expect("open");
        let info = src.info();
        assert_eq!(info.container, "Insta360 INSV (Insta360 X4)");
        let v = info.video.as_ref().expect("video");
        assert_eq!((v.width, v.height), (128, 32), "two 64-wide circles side by side");
        let f = src.video_frame(FrameRequest::full(Tick::ZERO)).expect("frame");
        let l = f.luma8();
        let mean = |x0: usize, x1: usize| -> u32 {
            let mut s = 0u64;
            for r in 0..32 {
                for c in x0..x1 {
                    s += u64::from(l[r * 128 + c]);
                }
            }
            (s / (32 * (x1 - x0) as u64)) as u32
        };
        // lens 0 (left circle) is bright on its left; lens 1 (right) is dark on its left
        assert!(mean(0, 32) > mean(32, 64), "left circle bright-left");
        assert!(mean(64, 96) < mean(96, 128), "right circle dark-left");
    }

    #[test]
    fn spanned_part_names_split_at_the_segment() {
        assert_eq!(spanned_part("VID_20261010_205908_00_055.insv"), Some(("VID_20261010_205908_00_".into(), 55)));
        assert_eq!(spanned_part("/card/DCIM/Camera01/VID_20261010_205908_00_056.INSV"), Some(("VID_20261010_205908_00_".into(), 56)));
        assert_eq!(spanned_part("LRV_20261010_205908_01_055.lrv"), Some(("LRV_20261010_205908_01_".into(), 55)), "the proxy chain names the same way");
        assert_eq!(spanned_part("clip.insv"), None, "not camera naming");
        assert_eq!(spanned_part("VID_20261010_205908_00_5.insv"), None, "segment must be 3 digits");
        assert_eq!(spanned_part("VID_20261010_205908_00_055.mp4"), None, "not insv");
        assert_eq!(spanned_part("VID_short.insv"), None, "too short");
    }

    #[test]
    fn recording_key_matches_the_vid_and_lrv_twins() {
        assert_eq!(recording_key("VID_20261010_205908_00_"), Some("20261010_205908"));
        assert_eq!(recording_key("LRV_20261010_205908_01_"), Some("20261010_205908"), "the proxy chain is the same recording");
        assert_eq!(recording_key("VID_20261010_205909_00_"), Some("20261010_205909"), "a different time is a different recording");
        assert_eq!(recording_key("GS0071"), None, "not camera naming");
    }

    /// A whole LRV built by the shared helper: it opens as one stitched equirectangular
    /// source, and its picture matches the INSV of the same scene — the camera stores
    /// the LRV's circles in the reverse of the INSV's track order (left = rear), and
    /// the source must undo that.
    #[test]
    fn synthetic_lrv_stitches_like_the_insv_of_the_same_scene() {
        let insv = synthetic_insv_circles(None, 1, false, 64, 64).expect("build insv");
        let lrv = synthetic_lrv(None, 1, false).expect("build lrv");
        let a = opener("clip.insv", insv.into()).expect("claimed").expect("open");
        let b = lrv_opener("LRV_20261010_205908_01_054.lrv", lrv.into()).expect("claimed").expect("open");
        let (ia, ib) = (a.info(), b.info());
        assert_eq!(ib.container, "Insta360 LRV (Insta360 X4)");
        let (va, vb) = (ia.video.as_ref().expect("insv video"), ib.video.as_ref().expect("lrv video"));
        assert_eq!((va.width, va.height), (vb.width, vb.height), "the proxy reports the same equirect size");
        let (fa, fb) = (a.video_frame(FrameRequest::full(Tick::ZERO)).expect("insv frame"), b.video_frame(FrameRequest::full(Tick::ZERO)).expect("lrv frame"));
        let (la, lb) = (fa.luma8(), fb.luma8());
        assert_eq!(la.len(), lb.len());
        // the same scene: the layout agrees everywhere, and the levels sit within a
        // proxy re-encode's range (the helper's circles differ by one shade on purpose)
        let (ma, mb) = (la.iter().copied().max().unwrap_or(0), lb.iter().copied().max().unwrap_or(0));
        assert!(ma > 150 && mb > 150, "both chains show the bright circle");
        let diff = la.iter().zip(&lb).map(|(x, y)| (i32::from(*x) - i32::from(*y)).unsigned_abs()).max().unwrap_or(0);
        assert!(diff <= 40, "the LRV stitch matches the INSV stitch's layout: max luma diff {diff}");
    }

    /// The stitch maps the LRV's RIGHT circle (the front lens) to lon 0: a bright
    /// object only the front circle carries must land at the equirect centre, and one
    /// only the rear (left) circle carries must wrap to the frame edges. If the halves
    /// were not swapped, these two checks trade results.
    #[test]
    fn lrv_front_circle_is_the_right_half() {
        // front (right circle) bright, rear (left) plain
        let bytes = synthetic_lrv_front_bright(true).expect("build");
        let src = lrv_opener("LRV_20261010_205908_01_054.lrv", bytes.into()).expect("claimed").expect("open");
        let f = src.video_frame(FrameRequest::full(Tick::ZERO)).expect("frame");
        let l = f.luma8();
        let (w, h) = (f.width as usize, f.height as usize);
        let centre = usize::from(l[h / 2 * w + w / 2]);
        let edge = usize::from(l[h / 2 * w]);
        assert!(centre > edge + 40, "the right circle's object lands at lon 0: {centre} vs {edge}");
    }

    /// One LRV frame with the front (right) circle carrying a bright centre patch and
    /// the rear (left) circle plain, for the orientation test.
    fn synthetic_lrv_front_bright(_bright: bool) -> Result<Vec<u8>, String> {
        use filmcraft_isobmff::{Brand, FourCc, Mp4Writer, SampleEntry, TrackConfig, WriteSample, WriterOptions};

        let c = 64u32;
        let mut fr = craft_video::prores::Frame::new(c * 2, c, craft_video::prores::ChromaFormat::Yuv422, 10, false).map_err(|e| e.to_string())?;
        for (i, y) in fr.y.iter_mut().enumerate() {
            let (x, r) = ((i as u32 % (c * 2)), (i as u32 / (c * 2)));
            // a bright blob at the centre of the RIGHT (front) circle's own centre
            let in_blob = x >= c + c / 2 - 8 && x < c + c / 2 + 8 && r >= c / 2 - 8 && r < c / 2 + 8;
            *y = if in_blob { 800 } else { 64 };
        }
        let d = {
            let mut enc = craft_video::prores::Encoder::new(craft_video::prores::Profile::Hq, c * 2, c);
            enc.encode(&fr).map_err(|e| e.to_string())?
        };
        let mut mux = Mp4Writer::new(std::io::Cursor::new(Vec::new()), WriterOptions::new(Brand::Mp4)).map_err(|e| e.to_string())?;
        let ti = mux.add_track(TrackConfig::new(SampleEntry::prores(FourCc(*b"apch"), (c * 2) as u16, c as u16), 25)).map_err(|e| e.to_string())?;
        mux.write_sample(ti, WriteSample { data: &d, duration: 1, composition_offset: 0, is_sync: true }).map_err(|e| e.to_string())?;
        let mut bytes = mux.finish().map_err(|e| e.to_string())?.into_inner();
        append_trailer(&mut bytes, None);
        Ok(bytes)
    }

    /// The builder can mark a spanned recording (INFO field 26), and the parser reads it back.
    #[test]
    fn synthetic_insv_can_name_its_first_part() {
        struct S(Vec<u8>);
        impl filmcraft_insta360::ByteSource for S {
            fn len(&self) -> u64 {
                self.0.len() as u64
            }
            fn read_at(&self, off: u64, buf: &mut [u8]) -> std::io::Result<()> {
                let o = off as usize;
                buf.copy_from_slice(self.0.get(o..o + buf.len()).ok_or(std::io::ErrorKind::UnexpectedEof)?);
                Ok(())
            }
        }
        let bytes = synthetic_insv(Some("/DCIM/Camera01/VID_20261010_205908_00_054.insv"), 1, false).expect("build");
        let meta = filmcraft_insta360::parse(&S(bytes)).expect("parse").expect("trailer");
        assert_eq!(meta.first_part.as_deref(), Some("/DCIM/Camera01/VID_20261010_205908_00_054.insv"));
    }
}

/// Every part of one recording shares its bake, but shares nothing else: the
/// temporally-smoothed seam history must belong to one source, so playing two
/// sources alternately (two angles of one camera, a chain and its original)
/// cannot blend one source's flow history into the other's seam placement.
#[test]
fn shared_bakes_keep_seam_history_per_source() {
    let c = 256usize;
    let (w, h) = (336usize, 168usize);
    // Mid-grey circle with a bright vertical bar near the column that maps into
    // the lon +90 overlap band (the aligned_blend_does_not_ghost test's rig).
    let bar = |col: usize| -> VideoFrame {
        let mut y = vec![120u8; c * c];
        for r in c / 2 - 40..c / 2 + 40 {
            for x in col..col + 6 {
                y[r * c + x] = 240;
            }
        }
        VideoFrame {
            width: c as u32,
            height: c as u32,
            data: PixelData::Yuv8 { planes: [Arc::new(y), Arc::new(vec![128; c * c / 4]), Arc::new(vec![128; c * c / 4])], chroma: Chroma::C420, alpha: None },
            color: filmcraft_color::ColorInfo::REC709,
            par: (1, 1),
            pts: filmcraft_time::Tick::ZERO,
        }
    };
    let lens = |yaw: f64| filmcraft_insta360::Lens {
        xi: 1.94817,
        fx: c as f64 * 0.77,
        fy: c as f64 * 0.77,
        cx: c as f64 * 0.5,
        cy: c as f64 * 0.5,
        yaw,
        pitch: 0.0,
        roll: 0.0,
        k1: 0.0,
        k2: 0.0,
        k3: 0.0,
        p1: 0.0,
        p2: 0.0,
        tx: 0.0,
        ty: 0.0,
        tz: 0.0,
        sensor_w: 2 * c as u32,
        sensor_h: c as u32,
    };
    let lenses = [lens(0.36), lens(-0.49)];
    // Two sources over the same bake: A is nearly aligned frame to frame (the
    // rear bar drifts one pixel); B sees a wide-parallax object. One shared
    // history would carry B's shift into A's second frame's rear warp.
    let (slot_a, seam_a) = (Mutex::new(None), stitch::SeamState::new());
    let (slot_b, seam_b) = (Mutex::new(None), stitch::SeamState::new());
    let st = |slot: &Mutex<Option<(BakeKey, Arc<Stitch>)>>, seam: &stitch::SeamState, f: &VideoFrame, r: &VideoFrame| {
        stitch_pair(slot, seam, &lenses, c as u32, (w as u32, h as u32), f, r).expect("stitch")
    };
    let a_front = bar(205);
    let (a0, a1) = (bar(209), bar(210));
    let b_rear = bar(232);
    let _a0 = st(&slot_a, &seam_a, &a_front, &a0);
    let alone = st(&slot_a, &seam_a, &a_front, &a1);
    // same frames, but B's two frames run between A's
    let (slot_a2, seam_a2) = (Mutex::new(None), stitch::SeamState::new());
    let _ = st(&slot_a2, &seam_a2, &a_front, &a0);
    let _ = st(&slot_b, &seam_b, &a_front, &b_rear);
    let _ = st(&slot_b, &seam_b, &a_front, &b_rear);
    let mixed = st(&slot_a2, &seam_a2, &a_front, &a1);
    assert_eq!(mixed.to_rgba8(), alone.to_rgba8(), "A's seam must not remember B's frames");
}

/// A file whose trailer is still being fetched (a web import): the opener must
/// report it through the pending flag so the caller retries — not return None,
/// which would let the plain MP4 opener claim the (already-read) head and
/// cache the clip unstitched. Once the bytes are there, the retry stitches.

#[test]
fn reader_opener_reports_a_trailer_still_fetching() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let bytes: Arc<[u8]> = synthetic_insv(None, 2, false).expect("build").into();
    struct HeadOnly(Arc<[u8]>, AtomicBool);
    impl filmcraft_media::reader::ByteReader for HeadOnly {
        fn len(&self) -> u64 {
            self.0.len() as u64
        }
        fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
            if !self.1.load(Ordering::Relaxed) {
                self.1.store(true, Ordering::Relaxed);
                return Err(filmcraft_media::pending::would_block());
            }
            buf.copy_from_slice(&self.0[offset as usize..offset as usize + buf.len()]);
            Ok(())
        }
    }
    let reader: filmcraft_media::SharedReader = Arc::new(HeadOnly(bytes.clone(), AtomicBool::new(false)));
    filmcraft_media::pending::take();
    let r = reader_opener("VID_20261010_205908_00_054.insv", &bytes[..32], &reader);
    assert!(matches!(r, Some(Err(filmcraft_media::MediaError::Io(_)))), "still fetching: an error, not a pass");
    assert!(filmcraft_media::pending::take(), "the opener reports the deferred read");
    // the bytes arrived: the same reader now opens as stitched equirect
    let r2 = reader_opener("VID_20261010_205908_00_054.insv", &bytes[..32], &reader).expect("opens once fetched");
    assert!(r2.is_ok(), "{:?}", r2.err().map(|e| e.to_string()));
}

/// Every part of a spanned recording bakes the same remap, so opening a second
/// part shares the first one's bake (`Arc::ptr_eq`) instead of allocating a
/// second ~240 MB set of tables, whatever the cache capacity allows.
#[test]
fn spanned_parts_share_one_bake() {
    let mut lens = filmcraft_insta360::Lens {
        xi: 1.4,
        fx: 32.0,
        fy: 32.0,
        cx: 32.0,
        cy: 32.0,
        yaw: 180.0,
        pitch: 0.0,
        roll: 0.0,
        k1: 0.0,
        k2: 0.0,
        k3: 0.0,
        p1: 0.0,
        p2: 0.0,
        tx: -0.015,
        ty: 0.0,
        tz: 0.0,
        sensor_w: 128,
        sensor_h: 64,
    };
    let lenses = [lens, {
        lens.yaw = 0.0;
        lens
    }];
    let circle = 64usize;
    let frame = |bright: bool| -> VideoFrame {
        let mut y = vec![20u8; circle * circle];
        for (i, v) in y.iter_mut().enumerate() {
            *v = if bright { (i * 7) as u8 } else { 255 - i as u8 % 200 };
        }
        VideoFrame {
            width: circle as u32,
            height: circle as u32,
            data: PixelData::Yuv8 {
                planes: [Arc::new(y), Arc::new(vec![128; circle * circle / 4]), Arc::new(vec![128; circle * circle / 4])],
                chroma: Chroma::C420,
                alpha: None,
            },
            color: filmcraft_color::ColorInfo::REC709,
            par: (1, 1),
            pts: filmcraft_time::Tick::ZERO,
        }
    };
    let (l, r) = (frame(true), frame(false));
    let (slot_a, slot_b) = (Mutex::new(None), Mutex::new(None));
    let (seam_a, seam_b) = (stitch::SeamState::new(), stitch::SeamState::new());
    assert!(stitch_pair(&slot_a, &seam_a, &lenses, 64, (96, 48), &l, &r).is_ok());
    assert!(stitch_pair(&slot_b, &seam_b, &lenses, 64, (96, 48), &r, &l).is_ok());
    let sa = slot_a.lock().unwrap().as_ref().unwrap().1.clone();
    let sb = slot_b.lock().unwrap().as_ref().unwrap().1.clone();
    assert!(Arc::ptr_eq(&sa, &sb), "the second stitch re-baked the ~240 MB remap the first already had");
}
