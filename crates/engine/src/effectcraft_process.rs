//! Bounded, cancellable execution of the explicitly authorized EffectCraft CLI.
use std::io::Read;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::time::Duration;

use filmcraft_export::Progress;

pub(super) type Chunk = Result<(bool, Vec<u8>), String>;
pub(super) const MAX_OUTPUT: usize = 16 * 1024 * 1024;
pub(super) const MAX_LINE: usize = 64 * 1024;

pub(super) fn reader<R: Read + Send + 'static>(pipe: Option<R>, stdout: bool, sender: SyncSender<Chunk>) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new().name("effectcraft-output".into()).spawn(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if let Some(mut pipe) = pipe {
                let mut bytes = [0; 8192];
                loop {
                    let n = pipe.read(&mut bytes).map_err(|e| e.to_string())?;
                    if n == 0 {
                        break;
                    }
                    let bytes = bytes.get(..n).ok_or("invalid subprocess read length")?.to_vec();
                    if sender.send(Ok((stdout, bytes))).is_err() {
                        break;
                    }
                }
            }
            Ok::<_, String>(())
        }))
        .unwrap_or_else(|_| Err("EffectCraft output reader failed".into()));
        if let Err(error) = result {
            let _ = sender.send(Err(error));
        }
    })
}

fn progress_lines(pending: &mut Vec<u8>, progress: Option<&Progress>) -> Result<(), String> {
    while let Some(end) = pending.iter().position(|b| *b == b'\n') {
        if end > MAX_LINE {
            return Err("EffectCraft output line exceeds 64 KiB".into());
        }
        let bytes: Vec<_> = pending.drain(..=end).collect();
        let line = String::from_utf8_lossy(&bytes);
        if let Some(p) = progress
            && let Some((done, total, left)) = crate::effectcraft_link::parse_progress(line.trim())
        {
            crate::effectcraft_link::report_progress(p, done, total, left);
        }
    }
    if pending.len() > MAX_LINE {
        return Err("EffectCraft output line exceeds 64 KiB".into());
    }
    Ok(())
}

pub(crate) fn output(cli: &str, args: &[String], progress: Option<&Progress>, max_bytes: u64) -> Result<String, String> {
    static SLOT: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let started = web_time::Instant::now();
    let _slot = loop {
        match SLOT.try_lock() {
            Ok(slot) => break slot,
            Err(std::sync::TryLockError::Poisoned(error)) => break error.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => {
                if filmcraft_media::cancel::cancelled() || progress.is_some_and(|p| p.cancel.load(Ordering::Relaxed)) {
                    return Err("EffectCraft render cancelled".into());
                }
                if started.elapsed() > Duration::from_secs(600) {
                    return Err("EffectCraft renderer queue timed out".into());
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        }
    };
    if filmcraft_media::cancel::cancelled() || progress.is_some_and(|p| p.cancel.load(Ordering::Relaxed)) {
        return Err("EffectCraft render cancelled".into());
    }
    let mut command = crate::effectcraft_link::cli_command(cli, args);
    command.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    let mut child = command.spawn().map_err(|e| format!("can't run {cli}: {e} (set EFFECTCRAFT_CLI)"))?;
    let (sender, receiver): (SyncSender<Chunk>, Receiver<Chunk>) = mpsc::sync_channel(16);
    let mut readers = Vec::new();
    let spawned = (|| {
        readers.push(reader(child.stdout.take(), true, sender.clone()).map_err(|e| e.to_string())?);
        readers.push(reader(child.stderr.take(), false, sender.clone()).map_err(|e| e.to_string())?);
        Ok::<_, String>(())
    })();
    drop(sender);
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut pending = Vec::new();
    let outcome = spawned.and_then(|()| {
        let mut status = None;
        let mut pipes_done = false;
        loop {
            if filmcraft_media::cancel::cancelled() || progress.is_some_and(|p| p.cancel.load(Ordering::Relaxed)) {
                return Err("EffectCraft render cancelled".into());
            }
            if started.elapsed() > Duration::from_secs(600) {
                return Err("EffectCraft process exceeded ten minutes".into());
            }
            if let Some(path) = args.windows(2).find(|pair| pair.first().is_some_and(|arg| arg == "--out")).and_then(|pair| pair.get(1))
                && std::fs::metadata(path).is_ok_and(|m| m.len() > max_bytes)
            {
                return Err("EffectCraft output exceeds the available cache quota or per-render limit".into());
            }
            match receiver.recv_timeout(Duration::from_millis(25)) {
                Ok(chunk) => {
                    let (is_stdout, bytes) = chunk?;
                    if stdout.len().saturating_add(stderr.len()).saturating_add(bytes.len()) > MAX_OUTPUT {
                        return Err("EffectCraft diagnostic output exceeds 16 MiB".into());
                    }
                    if is_stdout {
                        stdout.extend(bytes)
                    } else {
                        pending.extend_from_slice(&bytes);
                        progress_lines(&mut pending, progress)?;
                        stderr.extend(bytes);
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => pipes_done = true,
            }
            if status.is_none() {
                status = child.try_wait().map_err(|e| e.to_string())?;
            }
            if pipes_done {
                if let Some(status) = status {
                    if status.success() {
                        return Ok(String::from_utf8_lossy(&stdout).into_owned());
                    }
                    let error = String::from_utf8_lossy(if stderr.is_empty() { &stdout } else { &stderr });
                    let tail: String = error.chars().rev().take(600).collect::<Vec<_>>().into_iter().rev().collect();
                    return Err(format!("effectcraft-cli failed ({status}): {}", tail.trim()));
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        }
    });
    if outcome.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    drop(receiver);
    for reader in readers {
        if reader.join().is_err() {
            return Err("EffectCraft output reader failed".into());
        }
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancelled_process_is_not_started() {
        let progress = Progress::default();
        progress.cancel.store(true, Ordering::Relaxed);
        assert!(output("does-not-exist", &[], Some(&progress), crate::linked_cache::MAX_RENDER_BYTES).unwrap_err().contains("cancelled"));
    }

    #[test]
    fn unterminated_diagnostic_lines_are_bounded() {
        assert!(progress_lines(&mut vec![b'x'; MAX_LINE + 1], None).is_err());
    }

    #[test]
    fn active_process_is_killed_when_cancelled() {
        let executable = std::env::current_exe().unwrap().to_string_lossy().into_owned();
        let progress = std::sync::Arc::new(Progress::default());
        let cancel = progress.clone();
        let thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            cancel.cancel.store(true, Ordering::Relaxed);
        });
        let started = web_time::Instant::now();
        let args = ["--exact", "effectcraft_process::tests::waiting_child_fixture", "--ignored", "--nocapture"].map(String::from);
        assert!(output(&executable, &args, Some(&progress), crate::linked_cache::MAX_RENDER_BYTES).unwrap_err().contains("cancelled"));
        assert!(started.elapsed() < Duration::from_secs(5));
        thread.join().unwrap();
    }

    #[test]
    #[ignore = "subprocess fixture; invoked only by active_process_is_killed_when_cancelled"]
    fn waiting_child_fixture() {
        std::thread::sleep(Duration::from_secs(10));
    }
}
