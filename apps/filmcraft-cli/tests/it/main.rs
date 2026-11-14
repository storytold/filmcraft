mod cli;
mod workflows;

fn filmcraft_command() -> std::process::Command {
    let executable = std::env::var_os("NEXTEST_BIN_EXE_filmcraft_cli").unwrap_or_else(|| env!("CARGO_BIN_EXE_filmcraft-cli").into());
    std::process::Command::new(executable)
}

#[test]
fn cli_executable_is_resolved() {
    assert!(std::path::Path::new(filmcraft_command().get_program()).is_file());
}
