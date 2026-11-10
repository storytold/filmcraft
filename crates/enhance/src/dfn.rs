//! DeepFilterNet inference (feature `deepfilter`): libDF's streaming runtime on tract.
//!
//! libDF's model state is not `Send` and its code may panic on a graph it does not expect, so
//! every model lives on its own worker thread, which runs each call under `catch_unwind` and
//! answers over a channel: a failure becomes an error and the effect falls back to the dry signal.
//! Loading a model takes a few hundred milliseconds, and seeking builds a new effect chain each
//! time, so idle workers are kept in a small pool and reused (reset from a pristine copy).

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Mutex, OnceLock, PoisonError};

use df::tract::{DfParams, DfTract, RuntimeParams};
use ndarray::Array2;

use crate::stream::FrameModel;

/// Idle workers kept for reuse.
const POOL: usize = 4;
/// libDF's post-filter strength when the switch is on (its documented default).
const POST_FILTER_BETA: f32 = 0.02;

enum Msg {
    Configure(f32, bool),
    Frame(Vec<Vec<f32>>),
    Reset,
}

type Reply = Result<Vec<Vec<f32>>, String>;

struct Shape {
    rate: u32,
    hop: usize,
    delay: usize,
}

struct Worker {
    path: PathBuf,
    channels: usize,
    shape: Shape,
    tx: Sender<Msg>,
    rx: Receiver<Reply>,
}

fn pool() -> &'static Mutex<Vec<Worker>> {
    static P: OnceLock<Mutex<Vec<Worker>>> = OnceLock::new();
    P.get_or_init(Default::default)
}

fn panic_text(p: Box<dyn std::any::Any + Send>) -> String {
    let text = p.downcast_ref::<&str>().map(|s| (*s).to_string()).or_else(|| p.downcast_ref::<String>().cloned());
    format!("the model runtime failed: {}", text.unwrap_or_else(|| "unknown error".into()))
}

fn build(path: &Path, channels: usize) -> Result<DfTract, String> {
    let params = DfParams::new(path.to_path_buf()).map_err(|e| format!("{e:#}"))?;
    DfTract::new(params, &RuntimeParams::default_with_ch(channels)).map_err(|e| format!("{e:#}"))
}

fn run_frame(model: &mut DfTract, io: &mut (Array2<f32>, Array2<f32>), frame: &mut [Vec<f32>]) -> Result<(), String> {
    let (input, output) = io;
    for (mut row, ch) in input.rows_mut().into_iter().zip(frame.iter()) {
        for (d, s) in row.iter_mut().zip(ch.iter()) {
            *d = if s.is_finite() { s.clamp(-8.0, 8.0) } else { 0.0 };
        }
    }
    model.process(input.view(), output.view_mut()).map_err(|e| format!("{e:#}"))?;
    for (row, ch) in output.rows().into_iter().zip(frame.iter_mut()) {
        for (d, s) in ch.iter_mut().zip(row.iter()) {
            *d = if s.is_finite() { *s } else { 0.0 };
        }
    }
    Ok(())
}

fn serve(path: PathBuf, channels: usize, ready: Sender<Result<Shape, String>>, rx: Receiver<Msg>, tx: Sender<Reply>) {
    let pristine = match catch_unwind(|| build(&path, channels)) {
        Ok(Ok(m)) => m,
        Ok(Err(e)) => {
            let _ = ready.send(Err(e));
            return;
        }
        Err(p) => {
            let _ = ready.send(Err(panic_text(p)));
            return;
        }
    };
    let (Ok(rate), hop) = (u32::try_from(pristine.sr), pristine.hop_size) else {
        let _ = ready.send(Err("the model reports an invalid sample rate".into()));
        return;
    };
    let Some(delay) = pristine.fft_size.checked_sub(hop).and_then(|d| pristine.lookahead.checked_mul(hop).and_then(|l| d.checked_add(l))) else {
        let _ = ready.send(Err("the model reports an invalid frame size".into()));
        return;
    };
    if hop == 0 || hop > 1 << 16 || ready.send(Ok(Shape { rate, hop, delay })).is_err() {
        return;
    }
    let mut model = pristine.clone();
    let mut io = (Array2::<f32>::zeros((channels, hop)), Array2::<f32>::zeros((channels, hop)));
    let (mut atten, mut post) = (100.0f32, false);
    while let Ok(msg) = rx.recv() {
        match msg {
            Msg::Configure(a, p) => {
                (atten, post) = (a, p);
                model.set_atten_lim(a);
                model.set_pf_beta(if p { POST_FILTER_BETA } else { 0.0 });
            }
            Msg::Reset => {
                model = pristine.clone();
                model.set_atten_lim(atten);
                model.set_pf_beta(if post { POST_FILTER_BETA } else { 0.0 });
            }
            Msg::Frame(mut frame) => {
                let ok = frame.len() == channels && frame.iter().all(|c| c.len() == hop);
                let r = if ok { catch_unwind(AssertUnwindSafe(|| run_frame(&mut model, &mut io, &mut frame))) } else { Ok(Err("wrong frame shape".into())) };
                let (reply, dead) = match r {
                    Ok(Ok(())) => (Ok(frame), false),
                    Ok(Err(e)) => (Err(e), false),
                    // the model's state is unknown after a panic: this worker is done
                    Err(p) => (Err(panic_text(p)), true),
                };
                if tx.send(reply).is_err() || dead {
                    return;
                }
            }
        }
    }
}

/// One DeepFilterNet model on its worker thread.
pub(crate) struct Dfn {
    worker: Option<Worker>,
    healthy: bool,
}

impl Dfn {
    /// Load the model archive at `path` for `channels` channels (or reuse an idle worker).
    pub(crate) fn load(path: &Path, channels: usize) -> Result<Dfn, String> {
        Dfn::open(path, channels)
    }

    fn open(path: &Path, channels: usize) -> Result<Dfn, String> {
        let pooled = {
            let mut p = pool().lock().unwrap_or_else(PoisonError::into_inner);
            p.iter().position(|w| w.channels == channels && w.path == path).map(|i| p.swap_remove(i))
        };
        if let Some(worker) = pooled {
            return Ok(Dfn { worker: Some(worker), healthy: true });
        }
        let (ready_tx, ready_rx) = channel();
        let (msg_tx, msg_rx) = channel();
        let (reply_tx, reply_rx) = channel();
        let thread_path = path.to_path_buf();
        std::thread::Builder::new()
            .name("filmcraft-enhance".into())
            .spawn(move || serve(thread_path, channels, ready_tx, msg_rx, reply_tx))
            .map_err(|e| format!("could not start the model thread: {e}"))?;
        let shape = ready_rx.recv().map_err(|_| "the model thread stopped while loading".to_string())??;
        Ok(Dfn { worker: Some(Worker { path: path.to_path_buf(), channels, shape, tx: msg_tx, rx: reply_rx }), healthy: true })
    }

    fn shape(&self) -> &Shape {
        static NONE: Shape = Shape { rate: 0, hop: 0, delay: 0 };
        self.worker.as_ref().map_or(&NONE, |w| &w.shape)
    }

    fn send(&mut self, msg: Msg) {
        if let Some(w) = self.worker.as_ref()
            && w.tx.send(msg).is_err()
        {
            self.healthy = false;
        }
    }

    fn frame(&mut self, frame: &mut [Vec<f32>]) -> Result<(), String> {
        let Some(w) = self.worker.as_ref().filter(|_| self.healthy) else { return Err("the model thread is gone".into()) };
        let owned: Vec<Vec<f32>> = frame.iter_mut().map(std::mem::take).collect();
        let reply =
            w.tx.send(Msg::Frame(owned))
                .map_err(|_| "the model thread is gone".to_string())
                .and_then(|()| w.rx.recv().map_err(|_| "the model thread is gone".to_string()));
        match reply {
            Ok(Ok(done)) => {
                for (f, d) in frame.iter_mut().zip(done) {
                    *f = d;
                }
                Ok(())
            }
            Ok(Err(e)) | Err(e) => {
                self.healthy = false;
                Err(e)
            }
        }
    }
}

impl Drop for Dfn {
    fn drop(&mut self) {
        let Some(w) = self.worker.take().filter(|_| self.healthy) else { return };
        if w.tx.send(Msg::Reset).is_err() || w.tx.send(Msg::Configure(100.0, false)).is_err() {
            return;
        }
        let mut p = pool().lock().unwrap_or_else(PoisonError::into_inner);
        if p.len() < POOL {
            p.push(w);
        }
    }
}

impl FrameModel for Dfn {
    fn sample_rate(&self) -> u32 {
        self.shape().rate
    }

    fn hop(&self) -> usize {
        self.shape().hop
    }

    fn delay(&self) -> usize {
        self.shape().delay
    }

    fn configure(&mut self, atten_db: f32, post_filter: bool) {
        self.send(Msg::Configure(atten_db, post_filter));
    }

    fn process(&mut self, frame: &mut [Vec<f32>]) -> Result<(), String> {
        self.frame(frame)
    }

    fn reset(&mut self) {
        self.send(Msg::Reset);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_that_is_not_a_model_is_an_error_not_a_crash() {
        let dir = tempfile::tempdir().unwrap();
        for (name, bytes) in [("empty.tar.gz", &b""[..]), ("text.tar.gz", &b"not a model"[..]), ("zeros.tar.gz", &[0u8; 4096][..])] {
            let path = dir.path().join(name);
            std::fs::write(&path, bytes).unwrap();
            assert!(Dfn::load(&path, 2).is_err(), "{name}");
        }
        assert!(Dfn::load(&dir.path().join("missing.tar.gz"), 2).is_err());
    }
}
