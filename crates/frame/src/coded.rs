//! Pictures still in their compressed form: the GPU compositor decodes them itself (intra-only
//! codecs whose slices decode independently, `filmcraft-gpu`'s compute shaders), so the frame
//! reaches the GPU as a few megabytes of bitstream instead of tens of megabytes of samples.
//!
//! Only frame jobs that composite on the GPU get these (decoders hand them out under the
//! `filmcraft_media::cancel::with_gpu_frames` hint). Every CPU consumer reads the pixels through
//! [`VideoFrame::cpu_frame`](crate::VideoFrame::cpu_frame), which decodes the picture once on the
//! CPU through the fallback the decoder supplies, so nothing can see a missing or wrong picture —
//! only a slower one.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use crate::Chroma;

/// Largest picture a failed decode is replaced by a black one of its size (the largest texture a
/// GPU compositor takes); a larger one gets empty planes, which every consumer shows transparent.
/// The size comes from the bitstream: a hostile one must not make us allocate gigabytes.
const MAX_PLACEHOLDER_SAMPLES: usize = 8192 * 8192;

/// The codec a [`CodedImage`] is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CodedFormat {
    /// One Apple ProRes frame (SMPTE RDD 36), as stored in a QuickTime / MXF sample.
    ProRes,
}

/// Decodes a coded picture on the CPU (planar Y'CbCr, as the codec's CPU decoder returns it).
pub type CodedFallback = Box<dyn Fn(&[u8]) -> Option<crate::PixelData> + Send + Sync>;

/// One compressed picture and how to decode it on the CPU.
pub struct CodedImage {
    format: CodedFormat,
    data: Arc<[u8]>,
    chroma: Chroma,
    bits: u32,
    alpha: bool,
    fallback: CodedFallback,
    cpu: OnceLock<crate::PixelData>,
    /// The GPU found the bitstream corrupt: show the CPU decode (and its error) from now on.
    gpu_failed: AtomicBool,
}

impl std::fmt::Debug for CodedImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodedImage")
            .field("format", &self.format)
            .field("bytes", &self.data.len())
            .field("chroma", &self.chroma)
            .field("bits", &self.bits)
            .finish()
    }
}

impl CodedImage {
    /// `chroma`, `bits` and `alpha` describe the planes the picture decodes to (what `fallback`
    /// returns as [`PixelData::Yuv16`](crate::PixelData::Yuv16)).
    pub fn new(format: CodedFormat, data: Arc<[u8]>, chroma: Chroma, bits: u32, alpha: bool, fallback: CodedFallback) -> Self {
        CodedImage { format, data, chroma, bits, alpha, fallback, cpu: OnceLock::new(), gpu_failed: AtomicBool::new(false) }
    }
    pub fn format(&self) -> CodedFormat {
        self.format
    }
    /// The coded bytes.
    pub fn data(&self) -> &[u8] {
        &self.data
    }
    pub fn chroma(&self) -> Chroma {
        self.chroma
    }
    /// Significant bits of the decoded samples (alpha included).
    pub fn bits(&self) -> u32 {
        self.bits
    }
    pub fn has_alpha(&self) -> bool {
        self.alpha
    }

    /// Whether the GPU decoder rejected the bitstream (see [`CodedImage::set_gpu_failed`]).
    pub fn gpu_failed(&self) -> bool {
        self.gpu_failed.load(Ordering::Relaxed)
    }
    /// The GPU decoder found corrupt entropy-coded data: compositors take the CPU decode from now
    /// on (which reports the error the way a CPU-decoded frame does).
    pub fn set_gpu_failed(&self) {
        self.gpu_failed.store(true, Ordering::Relaxed);
    }

    /// Memory the picture holds: its bitstream, plus its CPU decode once made.
    pub fn byte_size(&self) -> usize {
        let cpu = match self.cpu.get() {
            Some(crate::PixelData::Yuv16 { planes, alpha, .. }) => (planes.iter().map(|p| p.len()).sum::<usize>() + alpha.as_ref().map_or(0, |a| a.len())) * 2,
            Some(_) | None => 0,
        };
        self.data.len() + cpu
    }

    /// The decoded planes in host memory, decoded once through the fallback; a black picture of
    /// the right geometry when it fails (a corrupt frame) or delivers anything else (empty planes
    /// above [`MAX_PLACEHOLDER_SAMPLES`]).
    pub fn cpu(&self, width: u32, height: u32) -> crate::PixelData {
        self.cpu
            .get_or_init(|| {
                let (sx, sy) = self.chroma.shifts();
                let n = (width as usize).saturating_mul(height as usize);
                let cn = (width as usize).div_ceil(1 << sx).saturating_mul((height as usize).div_ceil(1 << sy));
                match (self.fallback)(&self.data) {
                    Some(crate::PixelData::Yuv16 { planes, chroma, bits, alpha })
                        if chroma == self.chroma
                            && bits == self.bits
                            && planes[0].len() == n
                            && planes[1].len() == cn
                            && planes[2].len() == cn
                            && alpha.as_ref().is_none_or(|a| a.len() == n) =>
                    {
                        crate::PixelData::Yuv16 { planes, chroma, bits, alpha }
                    }
                    other => {
                        log::warn!(
                            "{:?} frame {width}×{height}: the CPU decode {} — showing it black",
                            self.format,
                            if other.is_some() { "delivered the wrong geometry" } else { "failed" }
                        );
                        let s = self.bits.saturating_sub(8).min(8);
                        let (n, cn) = if n <= MAX_PLACEHOLDER_SAMPLES { (n, cn) } else { (0, 0) };
                        crate::PixelData::Yuv16 {
                            planes: [Arc::new(vec![16u16 << s; n]), Arc::new(vec![128u16 << s; cn]), Arc::new(vec![128u16 << s; cn])],
                            chroma: self.chroma,
                            bits: self.bits,
                            alpha: None,
                        }
                    }
                }
            })
            .clone()
    }

    /// Whether the CPU decode has been made already.
    pub fn has_cpu_pixels(&self) -> bool {
        self.cpu.get().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_decode_of_a_huge_claimed_picture_allocates_nothing() {
        // a header can claim 65535×65535 (8.6 GB of planes): the placeholder must not follow it
        let img = CodedImage::new(CodedFormat::ProRes, Arc::from(&[0u8; 16][..]), Chroma::C422, 10, false, Box::new(|_| None));
        let crate::PixelData::Yuv16 { planes, .. } = img.cpu(65535, 65535) else { panic!("planes") };
        assert!(planes.iter().all(|p| p.is_empty()));
        // a normal-sized one is black
        let img = CodedImage::new(CodedFormat::ProRes, Arc::from(&[0u8; 16][..]), Chroma::C422, 10, false, Box::new(|_| None));
        let crate::PixelData::Yuv16 { planes, .. } = img.cpu(64, 32) else { panic!("planes") };
        assert_eq!((planes[0].len(), planes[1].len()), (64 * 32, 32 * 32));
        assert!(planes[0].iter().all(|&v| v == 64));
    }
}
