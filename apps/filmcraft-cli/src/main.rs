//! `filmcraft-cli`: the process wiring only; see the library for the commands.

use std::process::ExitCode;

fn main() -> ExitCode {
    // `std::env::args` panics on an argument that is not Unicode (a Linux file name in Latin-1)
    let code = filmcraft_cli::run(std::env::args_os(), &mut std::io::stdout(), &mut std::io::stderr());
    ExitCode::from(code)
}
