//! Writes shell completions (bash, zsh, fish, elvish, PowerShell) and man pages (one per
//! subcommand) generated from the CLI's clap definition (RFC-0012):
//! `cargo run -p filmcraft-cli --example man-and-completions -- <out-dir>`.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use clap::CommandFactory;
use clap_complete::{Shell, generate_to};
use filmcraft_cli::cli::Cli;

#[derive(Debug)]
enum GenerateError {
    Usage,
    Io(std::io::Error),
}

impl std::fmt::Display for GenerateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GenerateError::Usage => write!(f, "usage: man-and-completions <out-dir>"),
            GenerateError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl From<std::io::Error> for GenerateError {
    fn from(e: std::io::Error) -> Self {
        GenerateError::Io(e)
    }
}

fn man_pages(cmd: &clap::Command, name: &str, dir: &Path) -> std::io::Result<()> {
    let mut page = Vec::new();
    clap_mangen::Man::new(cmd.clone()).render(&mut page)?;
    std::fs::write(dir.join(format!("{name}.1")), page)?;
    for sub in cmd.get_subcommands().filter(|s| !s.is_hide_set() && s.get_name() != "help") {
        man_pages(sub, &format!("{name}-{}", sub.get_name()), dir)?;
    }
    Ok(())
}

fn generate() -> Result<(), GenerateError> {
    let out = PathBuf::from(std::env::args_os().nth(1).ok_or(GenerateError::Usage)?);
    let (completions, man) = (out.join("completions"), out.join("man"));
    std::fs::create_dir_all(&completions)?;
    std::fs::create_dir_all(&man)?;
    let mut cmd = Cli::command();
    for shell in [Shell::Bash, Shell::Zsh, Shell::Fish, Shell::Elvish, Shell::PowerShell] {
        generate_to(shell, &mut cmd, "filmcraft-cli", &completions)?;
    }
    man_pages(&cmd, "filmcraft-cli", &man)?;
    Ok(())
}

fn main() -> std::process::ExitCode {
    match generate() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            // a closed stderr must not turn the error status into a panic
            let _ = writeln!(std::io::stderr(), "error: {e}");
            std::process::ExitCode::from(2)
        }
    }
}
