//! Scheduling priority of background work (ADR 0002): the threads of an export, Render Previews or
//! proxy job ([`filmcraft_export::on_own_threads`]) run at the default quality-of-service class
//! with the lowest relative priority, below the monitors' frame workers (the default class at 0)
//! and the user-interactive main thread, so editing stays responsive while an export runs. Not the
//! utility class: macOS runs utility threads on the efficiency cores first, and an export of a
//! sequence with effects took 15 % longer on an idle M4 Max. A thread these threads create starts
//! at the default class at 0 again (macOS does not pass the class on), so decoders an export opens
//! keep their priority.

/// The relative priority of background job threads within the default class: the lowest
/// (`QOS_MIN_RELATIVE_PRIORITY`).
const RELATIVE_PRIORITY: i32 = -15;

/// Lower the calling thread below the default class's other threads. A thread the system refuses
/// stays as it was, and the error says why.
pub fn lower_current_thread() -> std::io::Result<()> {
    // SAFETY: `pthread_set_qos_class_self_np` changes only the calling thread's own scheduling
    // class; it takes two plain integers and has no memory-safety preconditions.
    let r = unsafe { libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_DEFAULT, RELATIVE_PRIORITY) };
    if r == 0 { Ok(()) } else { Err(std::io::Error::from_raw_os_error(r)) }
}

#[cfg(test)]
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
