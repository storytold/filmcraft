use super::format::ExportSettings;
use super::{Arc, AtomicU64, ExportError, Mutex, Ordering, PoisonError, Result, Write, pcm};

/// Receives in-memory export output: `(path, bytes)` per finished file.
#[derive(Clone)]
pub struct OutputSink(pub Arc<dyn Fn(&str, Vec<u8>) -> std::io::Result<()> + Send + Sync>);

impl std::fmt::Debug for OutputSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OutputSink")
    }
}

/// How an audio-only export codes its samples.
pub(crate) enum AudioWriter {
    Pcm(pcm::PcmContainer),
    Flac(Box<filmcraft_flac::Encoder>),
}

/// An export's output file: on disk, or in memory for an [`OutputSink`].
pub(crate) enum Out {
    File { writer: std::io::BufWriter<std::fs::File>, temp: Option<TempOutput> },
    Mem(std::io::Cursor<Vec<u8>>),
}

pub(crate) struct TempOutput {
    path: std::path::PathBuf,
    destination: std::path::PathBuf,
}

impl Drop for TempOutput {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

static NEXT_OUTPUT_TEMP: AtomicU64 = AtomicU64::new(0);

impl Out {
    pub(crate) fn create(settings: &ExportSettings) -> Result<Out> {
        if let Some(file) = &settings.output_file {
            let file = file.try_clone().map_err(|e| ExportError::Io(e.to_string()))?;
            return Ok(Out::File { writer: std::io::BufWriter::new(file), temp: None });
        }
        Out::create_path(settings, &settings.path)
    }

    /// An output file at `path` (OP-Atom exports write several).
    pub(crate) fn create_path(settings: &ExportSettings, path: &str) -> Result<Out> {
        if settings.sink.is_some() {
            return Ok(Out::Mem(std::io::Cursor::new(Vec::new())));
        }
        if path == settings.path
            && let Some(file) = &settings.output_file
        {
            let file = file.try_clone().map_err(|e| ExportError::Io(e.to_string()))?;
            return Ok(Out::File { writer: std::io::BufWriter::new(file), temp: None });
        }
        let destination = std::path::PathBuf::from(path);
        let dir = destination.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(std::path::Path::new("."));
        for _ in 0..32 {
            let id = NEXT_OUTPUT_TEMP.fetch_add(1, Ordering::Relaxed);
            let temp = dir.join(format!(".filmcraft-export-{}-{id}.tmp", std::process::id()));
            match std::fs::OpenOptions::new().write(true).create_new(true).open(&temp) {
                Ok(file) => return Ok(Out::File { writer: std::io::BufWriter::new(file), temp: Some(TempOutput { path: temp, destination }) }),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(ExportError::Io(format!("{path}: {e}"))),
            }
        }
        Err(ExportError::Io(format!("{path}: could not allocate a unique temporary output")))
    }

    /// Flush (and hand in-memory output to the sink); returns the file size.
    pub(crate) fn finish(self, settings: &ExportSettings) -> Result<u64> {
        self.finish_path(settings, &settings.path)
    }

    pub(crate) fn finish_path(self, settings: &ExportSettings, path: &str) -> Result<u64> {
        match self {
            Out::File { mut writer, temp } => {
                let name = temp.as_ref().map_or_else(|| path.to_owned(), |t| t.destination.display().to_string());
                let io = |e| ExportError::Io(format!("{name}: {e}"));
                writer.flush().map_err(io)?;
                let Some(temp) = temp else {
                    // a handle the caller opened: it is theirs to sync and keep
                    return Ok(writer.get_ref().metadata().map_err(io)?.len());
                };
                writer.get_ref().sync_all().map_err(io)?;
                let size = writer.get_ref().metadata().map_err(io)?.len();
                drop(writer);
                std::fs::rename(&temp.path, &temp.destination).map_err(io)?;
                Ok(size)
            }
            Out::Mem(c) => write_output(settings, path, c.into_inner()),
        }
    }
}

/// Write one finished output file (to the sink when there is one); returns its size.
pub(crate) fn write_output(settings: &ExportSettings, path: &str, data: Vec<u8>) -> Result<u64> {
    let n = data.len() as u64;
    match &settings.sink {
        Some(sink) => (sink.0)(path, data),
        None => std::fs::write(path, &data),
    }
    .map_err(|e| ExportError::Io(e.to_string()))?;
    Ok(n)
}

impl Write for Out {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Out::File { writer, .. } => writer.write(buf),
            Out::Mem(w) => w.write(buf),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Out::File { writer, .. } => writer.flush(),
            Out::Mem(w) => w.flush(),
        }
    }
}

impl std::io::Seek for Out {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        match self {
            Out::File { writer, .. } => writer.seek(pos),
            Out::Mem(w) => w.seek(pos),
        }
    }
}

/// An in-memory writer that stays reachable after the encoder that owns a clone is dropped.
#[derive(Clone, Default)]
pub(crate) struct SharedBuf(pub(crate) Arc<Mutex<Vec<u8>>>);

impl Write for SharedBuf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
