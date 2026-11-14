//! Optional, independently loaded SVT-AV1 and Opus libraries. FFmpeg is not used here.
use std::path::PathBuf;

use filmcraft_export::Format;

mod opus;
mod svt;
mod svt_ffi;

pub(super) fn svt_available() -> bool {
    svt::available()
}

pub fn register() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        filmcraft_export::register_encoder(svt::factory);
        filmcraft_export::register_av1_probe(filmcraft_export::BitrateMode::Crf, svt::available);
        filmcraft_export::register_audio_encoder(opus::factory);
        filmcraft_export::register_format_probe(Format::Av1, svt::available);
        filmcraft_export::register_hdr_probe(Format::Av1, svt::available);
        filmcraft_export::register_format_probe(Format::Opus, opus::available);
    });
}

fn load(env: &str, name: &str) -> Result<libloading::Library, String> {
    let path = if let Some(path) = std::env::var_os(env) {
        PathBuf::from(path)
    } else {
        std::env::current_exe().map_err(|e| e.to_string())?.parent().ok_or("no executable directory")?.join("codecs").join(name)
    };
    // SAFETY: the caller opts into a native library at an explicit path or the app's codec directory.
    // The library is retained by the encoder for as long as its symbols and objects are used.
    unsafe { libloading::Library::new(&path) }.map_err(|e| format!("{}: {e}", path.display()))
}

fn video_error(e: impl std::fmt::Display) -> filmcraft_export::ExportError {
    filmcraft_export::ExportError::Encode(format!("SVT-AV1: {e}"))
}

fn audio_error(e: impl std::fmt::Display) -> filmcraft_export::ExportError {
    filmcraft_export::ExportError::Encode(format!("Opus: {e}"))
}
