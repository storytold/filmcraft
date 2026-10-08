//! Linux VA-API H.264 decoding.
//!
//! VA-API decoders are confined to a worker thread because the underlying decoder contains
//! thread-affine state. The adapter presents FilmCraft's synchronous, `Send` decoder interface and
//! downloads decoded NV12 surfaces into the planar YUV frames used by the CPU compositor.

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

use filmcraft_codecs::hw::NalStreamInfo;
use filmcraft_codecs::{CodecError, DecodedFrame, Result, VideoDecoder};
use filmcraft_frame::{Chroma, PixelData, VideoFrame};
use filmcraft_time::Tick;

use crate::annexb::to_annex_b;

enum Request {
    Decode(Vec<u8>, u64, Sender<std::result::Result<Vec<moq_vaapi::decode::Frame>, String>>),
    Flush(Sender<std::result::Result<Vec<moq_vaapi::decode::Frame>, String>>),
    Reset(Sender<()>),
    Stop,
}

enum WorkerInit {
    Ready,
    Failed(String),
}

/// A VA-API H.264 decoder adapted to FilmCraft's decoder contract.
pub struct VaapiDecoder {
    tx: Sender<Request>,
    worker: Option<JoinHandle<()>>,
    info: NalStreamInfo,
    headers: bool,
    draft: bool,
}

pub fn supported(info: &NalStreamInfo) -> bool {
    matches!(info.codec, filmcraft_codecs::hw::NalCodec::H264)
        && info.bit_depth_luma == 8
        && info.bit_depth_chroma == 8
        && info.chroma_format_idc == 1
        && !info.interlaced
        && info.crop.0 == 0
        && info.crop.1 == 0
}

pub fn hardware_decoder_for(entry: &filmcraft_isobmff::SampleEntry) -> bool {
    let Some(info) = filmcraft_codecs::hw::NalStreamInfo::from_entry(entry).and_then(Result::ok) else { return false };
    supported(&info) && VaapiDecoder::new(info).is_ok()
}

impl VaapiDecoder {
    pub fn new(info: NalStreamInfo) -> std::result::Result<Self, String> {
        let (request_tx, request_rx) = mpsc::channel();
        let (init_tx, init_rx) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("filmcraft-vaapi".into())
            .spawn(move || worker_loop(request_rx, init_tx))
            .map_err(|e| format!("start VA-API worker: {e}"))?;
        match init_rx.recv().map_err(|_| "VA-API worker exited during startup".to_string())? {
            WorkerInit::Ready => Ok(Self { tx: request_tx, worker: Some(worker), info, headers: true, draft: false }),
            WorkerInit::Failed(e) => {
                let _ = worker.join();
                Err(e)
            }
        }
    }

    fn request(&self, req: Request, rx: Receiver<std::result::Result<Vec<moq_vaapi::decode::Frame>, String>>) -> Result<Vec<moq_vaapi::decode::Frame>> {
        self.tx.send(req).map_err(|_| CodecError::Decode("VA-API worker exited".into()))?;
        rx.recv().map_err(|_| CodecError::Decode("VA-API worker exited".into()))?.map_err(CodecError::Decode)
    }

    fn convert(&self, frame: moq_vaapi::decode::Frame, draft: bool) -> Result<DecodedFrame> {
        let width = usize::try_from(frame.width).map_err(|_| CodecError::Decode("VA-API frame width overflows".into()))?;
        let height = usize::try_from(frame.height).map_err(|_| CodecError::Decode("VA-API frame height overflows".into()))?;
        let y_len = width.checked_mul(height).ok_or_else(|| CodecError::Decode("VA-API frame size overflows".into()))?;
        let chroma_width = width.div_ceil(2);
        let chroma_height = height.div_ceil(2);
        let chroma_len = chroma_width.checked_mul(chroma_height).ok_or_else(|| CodecError::Decode("VA-API chroma size overflows".into()))?;
        let uv_len = chroma_len.checked_mul(2).ok_or_else(|| CodecError::Decode("VA-API chroma size overflows".into()))?;
        let Some((y, uv)) = frame.data.split_at_checked(y_len) else { return Err(CodecError::Decode("VA-API returned a truncated frame".into())) };
        if uv.len() != uv_len {
            return Err(CodecError::Decode("VA-API returned an invalid NV12 frame size".into()));
        }
        let mut u = Vec::with_capacity(chroma_len);
        let mut v = Vec::with_capacity(chroma_len);
        for pair in uv.as_chunks::<2>().0 {
            u.push(pair[0]);
            v.push(pair[1]);
        }
        // VA-API treats the timestamp as opaque. Preserve FilmCraft's signed timeline exactly,
        // including negative PTS values from edit lists, without narrowing or offsetting them.
        let pts = i64::from_ne_bytes(frame.timestamp.to_ne_bytes());
        Ok(DecodedFrame {
            pts,
            frame: VideoFrame {
                width: frame.width,
                height: frame.height,
                data: PixelData::Yuv8 { planes: [y.to_vec().into(), u.into(), v.into()], chroma: Chroma::C420, alpha: None },
                color: self.info.color,
                par: self.info.par,
                pts: Tick(pts),
            },
            draft,
        })
    }
}

impl VideoDecoder for VaapiDecoder {
    fn decode(&mut self, sample: &[u8], pts: i64) -> Result<Vec<DecodedFrame>> {
        let annex_b = to_annex_b(&self.info, sample, self.headers).map_err(CodecError::Decode)?;
        self.headers = false;
        let (tx, rx) = mpsc::channel();
        let timestamp = u64::from_ne_bytes(pts.to_ne_bytes());
        let frames = self.request(Request::Decode(annex_b, timestamp, tx), rx)?;
        frames.into_iter().map(|f| self.convert(f, self.draft)).collect()
    }

    fn flush(&mut self) -> Vec<DecodedFrame> {
        let (tx, rx) = mpsc::channel();
        let Ok(frames) = self.request(Request::Flush(tx), rx) else { return Vec::new() };
        frames.into_iter().filter_map(|f| self.convert(f, self.draft).ok()).collect()
    }

    fn reset(&mut self) {
        let (tx, rx) = mpsc::channel();
        let _ = self.tx.send(Request::Reset(tx));
        let _ = rx.recv();
        self.headers = true;
    }

    fn name(&self) -> &str {
        "VA-API H.264"
    }

    fn is_random_access(&self, sample: &[u8]) -> Option<bool> {
        self.info.is_random_access(sample)
    }

    fn is_disposable(&self, sample: &[u8]) -> bool {
        self.info.is_disposable(sample)
    }

    fn set_draft(&mut self, on: bool) {
        self.draft = on;
    }
}

impl Drop for VaapiDecoder {
    fn drop(&mut self) {
        let _ = self.tx.send(Request::Stop);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn worker_loop(rx: Receiver<Request>, init: mpsc::SyncSender<WorkerInit>) {
    let mut decoder = match moq_vaapi::decode::Decoder::new(moq_vaapi::decode::Config::new()) {
        Ok(decoder) => decoder,
        Err(e) => {
            let _ = init.send(WorkerInit::Failed(e.to_string()));
            return;
        }
    };
    if init.send(WorkerInit::Ready).is_err() {
        return;
    }
    while let Ok(request) = rx.recv() {
        match request {
            Request::Decode(bytes, timestamp, reply) => {
                let _ = reply.send(decoder.decode(&bytes, timestamp).map_err(|e| e.to_string()));
            }
            Request::Flush(reply) => {
                let _ = reply.send(decoder.flush().map_err(|e| e.to_string()));
            }
            Request::Reset(reply) => {
                let _ = decoder.flush();
                let _ = reply.send(());
            }
            Request::Stop => break,
        }
    }
}
