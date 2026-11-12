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
        let err = cmd.args(args).env("MALLOC_ARENA_MAX", ARENAS).exec();
        log::debug!("could not restart with MALLOC_ARENA_MAX={ARENAS}: {err}");
    }
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
}
