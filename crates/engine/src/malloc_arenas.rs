//! Keep glibc's allocator from holding gigabytes of freed memory (Linux).
//!
//! glibc's `malloc` gives each thread that allocates its own arena, up to eight per core (96 on a
//! 12-thread machine). Rendering and decoding run on every core and free multi-megabyte buffers all
//! the time, and an arena keeps what was freed in it for the next allocation *on that arena*. So
//! every worker sits on its own share of freed buffers. Measured on a 12-thread, 16 GB Linux
//! machine (#1443), exporting 600 frames of a 4K HEVC clip to 1080x1920: 4260 MB peak with the
//! default arenas against 2595 MB with `MALLOC_ARENA_MAX=2` (Motion 50 %), 4725 against 3435 MB
//! (55 %). The `MALLOC_ARENA_MAX=2` workaround is what reporters on #2185 (playback after an
//! export) found too.
//!
//! glibc reads the limit once, when its allocator starts, before `main`. A running program can
//! set it only through `mallopt`, which is `unsafe` FFI the workspace does not allow here, so
//! [`limit_and_restart`] starts the program again in place (`exec`, same process id) with the
//! variable set. It does nothing when the variable or the `glibc.malloc.arena_max` tunable is
//! already set (setting either is how to opt out: `MALLOC_ARENA_MAX=64`), on other systems, and when
//! the program cannot be started again (it then carries on with the default arenas).
//!
//! `Command::exec` puts SIGPIPE back to its default action just before it calls `execvp`, and
//! leaves it there when the call fails. A program that then wrote to a closed pipe (`| head`) would
//! be killed instead of getting the `EPIPE` the CLI handles (`stdout_failed`). After a failed
//! restart [`restart`] installs a SIGPIPE handler that does nothing (`signal-hook`, safe code), so
//! a write to a closed pipe fails with `EPIPE` again, as Rust programs normally do.

/// Arenas FilmCraft runs with on glibc.
pub const ARENAS: &str = "2";

/// Start this program again with [`ARENAS`] malloc arenas unless the user already chose a limit.
/// Call it first thing in `main`. Returns when it did not restart.
pub fn limit_and_restart() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        use std::os::unix::process::CommandExt;
        if !needs_limit(std::env::var_os("MALLOC_ARENA_MAX").as_deref(), std::env::var_os("GLIBC_TUNABLES").as_deref()) {
            return;
        }
        let Ok(exe) = std::env::current_exe() else { return };
        let mut args = std::env::args_os();
        let mut cmd = std::process::Command::new(exe);
        if let Some(argv0) = args.next() {
            cmd.arg0(argv0);
        }
        // `exec` only returns on failure (a binary replaced while running, a sandbox without
        // `/proc`): carry on with the default arenas
        let err = restart(cmd.args(args).env("MALLOC_ARENA_MAX", ARENAS));
        log::debug!("could not restart with MALLOC_ARENA_MAX={ARENAS}: {err}");
    }
}

/// `exec` `cmd` in place of this program. Returns only when that fails, with SIGPIPE answered by a
/// handler that does nothing, so writes to a closed pipe return `EPIPE` (see the module docs).
#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn restart(cmd: &mut std::process::Command) -> std::io::Error {
    use std::os::unix::process::CommandExt;
    let err = cmd.exec();
    // SIGPIPE is not one of signal-hook's forbidden signals, so this does not panic
    if let Err(e) = signal_hook::flag::register(signal_hook::consts::SIGPIPE, std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false))) {
        log::debug!("could not handle SIGPIPE after the failed restart: {e}");
    }
    err
}

/// Whether to set the limit: neither `MALLOC_ARENA_MAX` nor an `arena_max` tunable is given.
pub fn needs_limit(arena_max: Option<&std::ffi::OsStr>, tunables: Option<&std::ffi::OsStr>) -> bool {
    arena_max.is_none() && !tunables.is_some_and(|t| t.to_string_lossy().contains("glibc.malloc.arena_max"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn a_limit_the_user_chose_is_kept() {
        assert!(needs_limit(None, None));
        assert!(needs_limit(None, Some(OsStr::new("glibc.malloc.tcache_count=0"))));
        assert!(!needs_limit(Some(OsStr::new("64")), None));
        // empty still means the user set it (glibc ignores it, and so do we)
        assert!(!needs_limit(Some(OsStr::new("")), None));
        assert!(!needs_limit(None, Some(OsStr::new("glibc.malloc.check=0:glibc.malloc.arena_max=8"))));
    }

    /// A failed restart used to leave SIGPIPE at its default action (`Command::exec` resets it
    /// before `execvp`), so a write to a closed pipe killed the program instead of failing.
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    #[test]
    fn a_failed_restart_still_gets_epipe_from_a_closed_pipe() {
        use std::io::Write;
        let err = restart(&mut std::process::Command::new("/nonexistent/filmcraft-restart-test"));
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
        let (reader, mut writer) = std::io::pipe().unwrap();
        drop(reader);
        assert_eq!(writer.write_all(b"after the failed restart").unwrap_err().kind(), std::io::ErrorKind::BrokenPipe);
    }
}
