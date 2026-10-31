//! Scheduling priority of background work (ADR 0002): the threads of an export, Render Previews or
//! proxy job ([`filmcraft_export::on_own_threads`]) run below the monitors' frame workers and the
//! main thread, so editing stays responsive while an export runs. When the machine is idle they
//! still get every core.
//!
//! - **macOS:** the default quality-of-service class with the lowest relative priority. Not the
//!   utility class: macOS runs utility threads on the efficiency cores first, and an export of a
//!   sequence with effects took 15 % longer on an idle M4 Max. A thread these threads create starts
//!   at the default class at 0 again (macOS does not pass the class on).
//! - **Windows:** below-normal thread priority. A new thread starts at normal priority again.
//! - **Linux:** a `nice` value 10 higher. A new thread takes its creator's value, so the decoders an
//!   export makes for its own copy of a file run lowered with it, and the shared decode pool, which
//!   decodes for the monitors too, is made at startup ([`register`]) rather than by whichever
//!   thread decodes first.

#[cfg(target_os = "macos")]
/// The relative priority of background job threads within the default class: the lowest
/// (`QOS_MIN_RELATIVE_PRIORITY`).
const RELATIVE_PRIORITY: i32 = -15;

#[cfg(target_os = "linux")]
/// How much nicer than the thread that made it a background job thread is: a nice-0 thread gets
/// about nine times its CPU time when both want a core.
const NICER_BY: i32 = 10;

/// Lower the priority of each background job thread as it starts.
pub(crate) fn register() {
    filmcraft_export::register_background_thread_start(|| {
        // a thread the system refuses to lower runs at the default priority, as before ADR 0002
        if let Err(e) = lower_current_thread() {
            log::debug!("could not lower a background thread's priority: {e}");
        }
    });
    #[cfg(target_os = "linux")]
    filmcraft_codecs::gop::start_decode_pool();
}

/// Lower the calling thread below the threads at the default priority. A thread the system refuses
/// stays as it was, and the error says why.
#[cfg(target_os = "macos")]
pub fn lower_current_thread() -> std::io::Result<()> {
    // SAFETY: `pthread_set_qos_class_self_np` changes only the calling thread's own scheduling
    // class; it takes two plain integers and has no memory-safety preconditions.
    let r = unsafe { libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_DEFAULT, RELATIVE_PRIORITY) };
    if r == 0 { Ok(()) } else { Err(std::io::Error::from_raw_os_error(r)) }
}

/// Lower the calling thread below the threads at the default priority. A thread the system refuses
/// stays as it was, and the error says why.
#[cfg(target_os = "windows")]
pub fn lower_current_thread() -> std::io::Result<()> {
    use windows::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL};
    // SAFETY: `GetCurrentThread` returns the calling thread's pseudo-handle, which needs no closing
    // and is valid for the call; `SetThreadPriority` changes only that thread's priority.
    unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL) }.map_err(std::io::Error::other)
}

/// Lower the calling thread below the threads at the default priority. A thread the system refuses
/// stays as it was, and the error says why.
#[cfg(target_os = "linux")]
pub fn lower_current_thread() -> std::io::Result<()> {
    // SAFETY: `gettid` takes nothing and returns the calling thread's id.
    let tid = unsafe { libc::gettid() };
    let tid = libc::id_t::try_from(tid).map_err(std::io::Error::other)?;
    // `getpriority` can return -1 as a nice value: errno, cleared first, tells it from an error
    // SAFETY: `__errno_location` points to the calling thread's errno, valid while it runs.
    unsafe { *libc::__errno_location() = 0 };
    // SAFETY: `getpriority` reads the nice value of the thread `tid` (this one); no pointers.
    let now = unsafe { libc::getpriority(libc::PRIO_PROCESS, tid) };
    if now == -1 {
        let e = std::io::Error::last_os_error();
        if e.raw_os_error().is_some_and(|c| c != 0) {
            return Err(e);
        }
    }
    // SAFETY: `setpriority` sets the nice value of the thread `tid` (this one); on Linux it is a
    // per-thread value. No pointers.
    let r = unsafe { libc::setpriority(libc::PRIO_PROCESS, tid, now.saturating_add(NICER_BY).min(19)) };
    if r == 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    /// The calling thread's QoS class (as its raw value) and relative priority.
    fn class() -> (u32, i32) {
        let mut class = libc::qos_class_t::QOS_CLASS_UNSPECIFIED;
        let mut relative = 0;
        // SAFETY: `pthread_self` is the calling thread, alive for the call; both out-pointers are
        // valid, writable locals.
        let r = unsafe { libc::pthread_get_qos_class_np(libc::pthread_self(), &mut class, &mut relative) };
        assert_eq!(r, 0);
        (class as u32, relative)
    }

    #[test]
    fn lowers_only_the_calling_thread() {
        let default = libc::qos_class_t::QOS_CLASS_DEFAULT as u32;
        std::thread::spawn(move || {
            assert_eq!(class(), (default, 0));
            lower_current_thread().unwrap();
            assert_eq!(class(), (default, RELATIVE_PRIORITY));
            // a thread it creates is not lowered with it
            let child = std::thread::spawn(class).join().unwrap();
            assert_eq!(child, (default, 0));
        })
        .join()
        .unwrap();
    }
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use windows::Win32::System::Threading::{GetCurrentThread, GetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL, THREAD_PRIORITY_NORMAL};

    use super::*;

    fn priority() -> i32 {
        // SAFETY: the calling thread's pseudo-handle, valid for the call.
        unsafe { GetThreadPriority(GetCurrentThread()) }
    }

    #[test]
    fn lowers_only_the_calling_thread() {
        std::thread::spawn(|| {
            assert_eq!(priority(), THREAD_PRIORITY_NORMAL.0);
            lower_current_thread().unwrap();
            assert_eq!(priority(), THREAD_PRIORITY_BELOW_NORMAL.0);
            // a thread it creates is not lowered with it
            assert_eq!(std::thread::spawn(priority).join().unwrap(), THREAD_PRIORITY_NORMAL.0);
        })
        .join()
        .unwrap();
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    /// The nice value of thread `tid` of this process, from `/proc` (field 19 of its `stat`, the
    /// 17th after the name in parentheses).
    fn nice_of(tid: &str) -> i32 {
        let stat = std::fs::read_to_string(format!("/proc/self/task/{tid}/stat")).unwrap();
        let after_name = &stat[stat.rfind(')').unwrap() + 1..];
        after_name.split_whitespace().nth(16).unwrap().parse().unwrap()
    }

    fn own_nice() -> i32 {
        // SAFETY: `gettid` takes nothing and returns the calling thread's id.
        nice_of(&unsafe { libc::gettid() }.to_string())
    }

    #[test]
    fn lowers_the_calling_thread_and_the_threads_it_makes() {
        std::thread::spawn(|| {
            let before = own_nice();
            lower_current_thread().unwrap();
            assert_eq!(own_nice(), (before + NICER_BY).min(19));
            // Linux passes the value on: why the shared decode pool is made at startup
            assert_eq!(std::thread::spawn(own_nice).join().unwrap(), (before + NICER_BY).min(19));
        })
        .join()
        .unwrap();
    }

    /// An export that decodes before anything else has must not make the shared decode pool, which
    /// the monitors decode on too, with its own lowered priority.
    #[test]
    fn the_shared_decode_pool_is_not_made_lowered() {
        let before = own_nice();
        register();
        std::thread::spawn(|| {
            lower_current_thread().unwrap();
            filmcraft_codecs::gop::start_decode_pool();
        })
        .join()
        .unwrap();
        let decode: Vec<i32> = std::fs::read_dir("/proc/self/task")
            .unwrap()
            .filter_map(|t| t.ok())
            .map(|t| t.file_name().to_string_lossy().into_owned())
            .filter(|tid| std::fs::read_to_string(format!("/proc/self/task/{tid}/comm")).is_ok_and(|n| n.starts_with("decode-")))
            .map(|tid| nice_of(&tid))
            .collect();
        assert!(!decode.is_empty(), "no decode pool threads");
        assert!(decode.iter().all(|&n| n == before), "decode pool threads lowered: {decode:?}");
    }
}
