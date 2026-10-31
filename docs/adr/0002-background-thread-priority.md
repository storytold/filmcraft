# ADR 0002: lower the scheduling priority of background job threads in `crates/platform`

- **Status:** accepted (2026-10-30, approved by the project owner; extends the scope of
  [ADR 0001](0001-platform-ffi.md), whose containment rules apply unchanged)

## Context

An export renders on every core. Exports, the export queue, Render Previews and proxies ran at the
same priority as everything else, so when the machine was busy the operating system could not know
that the export can wait and the monitors cannot.

Measured on an M4 Max (2026-10-30, release builds, a one-minute 4K60 H.264 export of a phone
footage sequence with effects): while it ran, 12 s of Program-monitor playback elsewhere in the
sequence showed 4 and 17 frames (the rest dropped) with the job's threads at the default priority,
and 713–715 of 716 with them lowered. Lowering them to the utility class cost the export 15 % on
the idle machine (373 → 429 s): macOS runs utility threads on the efficiency cores first. The
default class with the lowest relative priority gives playback the same frames and costs the
export about 1 % (377, 380 s).

Two things the monitors shared with background jobs are fixed without any `unsafe`: the rendering
threads (background jobs run on a pool of their own, `filmcraft_export::on_own_threads`) and the
decoders (a job reads its own copy of each file, `MediaPool::own_source`; that was why playing a
clip while it exported dropped 93 % of the frames). Telling the system which threads can wait is
an OS call (on macOS `pthread_set_qos_class_self_np`), which needs `unsafe` Rust. ADR 0001 allows
`unsafe` only for OS media FFI and says anything else needs a new decision: this is that decision.

## Decision

`crates/platform` may also call the OS to lower the scheduling priority of the calling thread,
for the threads of background jobs, in one FFI module (`thread_priority`), under ADR 0001's
containment rules (`#[allow(unsafe_code)]` on that module only, a `// SAFETY:` comment on every
block, a safe public API, nothing that can unwind across the boundary).

- **How it is wired:** the export crate runs a hook on each thread of a background job's pool as
  it starts (`filmcraft_export::register_background_thread_start`); `filmcraft_platform::register`
  installs `thread_priority::lower_current_thread` there. Lower layers do not depend on the
  platform crate, and without it (tests, the browser) nothing changes.
- **macOS:** the default quality-of-service class at the lowest relative priority (−15). The
  monitors' frame workers stay at the default class at 0 and the main thread at user-interactive.
  Not the utility class, which macOS runs on the efficiency cores first. A thread these threads
  create starts at the default class at 0 again (macOS does not pass the class on; tested), so
  decoders an export opens keep their priority.
- **Windows:** below-normal thread priority (`SetThreadPriority`). A thread these threads create
  starts at normal priority again (tested).
- **Linux:** a `nice` value 10 higher than before (`setpriority` on the thread's id; Linux keeps
  the value per thread). A thread these threads create takes the lowered value (tested). The
  decoders an export makes for its own copy of a file serve only that export, so their threads
  running lowered is right. The shared decode pool decodes for the monitors too:
  `filmcraft_platform::register` makes it at startup (`filmcraft_codecs::gop::start_decode_pool`),
  so a lowered thread decoding before anything else has cannot be the one that makes it (tested).

## Consequences

- When the machine is busy, the monitors and the UI get the cores first and the export takes
  longer; when it is idle, the export still uses every core.
- The only new `unsafe` is a call that changes the calling thread's own scheduling attributes;
  it reads and writes no memory of ours.
- Anything else that wants `unsafe` still needs a new decision.
