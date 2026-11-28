//! Developer-only project-folder conversion. See docs/project-files.md.
mod converter;

use converter::{pack, unpack};
use filmcraft_format::atomic_write_new;
use std::{io, path::Path};

fn run() -> io::Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if let [command, source, destination] = args.as_slice() {
        let (src, dest) = (Path::new(source), Path::new(destination));
        if command == "pack" {
            return pack(src, dest, atomic_write_new);
        }
        if command == "unpack" {
            return unpack(src, dest);
        }
    }
    Err(io::Error::new(io::ErrorKind::InvalidInput, "usage: project_folder pack SOURCE.fcproj NEW_FOLDER | unpack FOLDER NEW_FILE.fcproj"))
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("project_folder: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}
