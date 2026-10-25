//! The desktop app survives a lost or starved graphics device during UI paint (AGENTS.md §0;
//! docs/architecture.md § GPU loss). Before the fix, egui-wgpu's "Failed to create staging buffer
//! for index data" panic closed the app with the user's work. Now the host catches it around the
//! event loop, recreates the window with the same session (in software rendering where the
//! platform has a software adapter) and shows "Graphics device lost; rendering in software. Save
//! your work."; the panic is in the app log and the crash log, and the loss is recorded in
//! `graphics-device-lost.json` without changing the saved start mode.
//!
//! These tests run the real binary with a window, driven over the control channel. They are
//! skipped on Linux without a display and when `FILMCRAFT_SKIP_WINDOW_TESTS` is set.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// One window at a time: each test opens its own app (its own port and data folder).
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

const MESSAGE: &str = "Graphics device lost; rendering in software. Save your work.";

fn skip_reason() -> Option<String> {
    if std::env::var_os("FILMCRAFT_SKIP_WINDOW_TESTS").is_some() {
        return Some("FILMCRAFT_SKIP_WINDOW_TESTS is set".into());
    }
    if cfg!(target_os = "linux") && std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        return Some("no display".into());
    }
    None
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").and_then(|l| l.local_addr()).map(|a| a.port()).expect("a free port")
}

/// A fresh control token for one app (`FILMCRAFT_CONTROL_TOKEN`: 64 hex characters), from the
/// standard library's randomly seeded hasher.
fn control_token() -> String {
    use std::hash::{BuildHasher, Hasher};
    (0..4)
        .map(|i| {
            let mut h = std::collections::hash_map::RandomState::new().build_hasher();
            h.write_u64(i);
            format!("{:016x}", h.finish())
        })
        .collect()
}

struct App {
    child: Child,
    port: u16,
    token: String,
    data_dir: PathBuf,
}

impl App {
    fn start(tag: &str) -> App {
        let data_dir = std::env::temp_dir().join(format!("filmcraft-graphics-loss-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&data_dir);
        std::fs::create_dir_all(&data_dir).expect("data dir");
        let port = free_port();
        let token = control_token();
        let out = std::fs::File::create(data_dir.join("stdout.txt")).expect("stdout file");
        let err = std::fs::File::create(data_dir.join("stderr.txt")).expect("stderr file");
        let child = Command::new(env!("CARGO_BIN_EXE_filmcraft"))
            .args(["--control", &port.to_string(), "--empty", "--no-recover", "--data-dir"])
            .arg(&data_dir)
            .env_remove("WGPU_BACKEND")
            .env("FILMCRAFT_CONTROL_TOKEN", &token)
            .stdin(Stdio::null())
            .stdout(Stdio::from(out))
            .stderr(Stdio::from(err))
            .spawn()
            .expect("spawn filmcraft");
        let mut app = App { child, port, token, data_dir };
        // up to a minute for the window and the control server
        let t0 = Instant::now();
        loop {
            if let Some(v) = app.try_call("ui.inspect", json!({}), Duration::from_secs(5))
                && v["ok"] == json!(true)
            {
                break;
            }
            assert!(app.child.try_wait().expect("wait").is_none(), "filmcraft exited while starting: {}", app.logs());
            assert!(t0.elapsed() < Duration::from_secs(60), "filmcraft did not answer within a minute: {}", app.logs());
            std::thread::sleep(Duration::from_millis(250));
        }
        app
    }

    /// One request on its own connection (the server allows several), `None` when the app can't
    /// be reached or doesn't answer within `timeout`.
    fn try_call(&mut self, method: &str, params: Value, timeout: Duration) -> Option<Value> {
        let mut s = TcpStream::connect_timeout(&std::net::SocketAddr::from(([127, 0, 0, 1], self.port)), Duration::from_secs(2)).ok()?;
        s.set_read_timeout(Some(timeout)).ok()?;
        writeln!(s, "{}", json!({"id": 1, "token": self.token, "method": method, "params": params})).ok()?;
        let mut line = String::new();
        BufReader::new(s).read_line(&mut line).ok()?;
        serde_json::from_str(&line).ok()
    }

    fn call(&mut self, method: &str, params: Value) -> Value {
        let v = self.try_call(method, params.clone(), Duration::from_secs(70)).unwrap_or_else(|| panic!("no reply to {method} {params}: {}", self.logs()));
        assert_eq!(v["ok"], json!(true), "{method} {params}: {v}");
        v["result"].clone()
    }

    /// The `graphics.notice` label once the recreated window shows it.
    fn wait_for_notice(&mut self) -> String {
        let t0 = Instant::now();
        loop {
            if let Some(v) = self.try_call("ui.elements", json!({"prefix": "graphics.notice"}), Duration::from_secs(70))
                && let Some(els) = v["result"].as_array()
                && let Some(label) = els.iter().find(|e| e["id"] == "graphics.notice").and_then(|e| e["label"].as_str())
            {
                return label.to_string();
            }
            assert!(self.child.try_wait().expect("wait").is_none(), "filmcraft exited instead of recreating its window: {}", self.logs());
            assert!(t0.elapsed() < Duration::from_secs(90), "no graphics notice after the fault: {}", self.logs());
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    fn quit(mut self) -> (std::process::ExitStatus, PathBuf) {
        let _ = self.try_call("app.quit", json!({}), Duration::from_secs(10));
        let t0 = Instant::now();
        let status = loop {
            if let Some(s) = self.child.try_wait().expect("wait") {
                break s;
            }
            if t0.elapsed() > Duration::from_secs(30) {
                let _ = self.child.kill();
                panic!("filmcraft did not quit: {}", self.logs());
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        let dir = self.data_dir.clone();
        std::mem::forget(self);
        (status, dir)
    }

    fn logs(&self) -> String {
        let read = |name: &str| std::fs::read_to_string(self.data_dir.join(name)).unwrap_or_default();
        let app_log = std::fs::read_to_string(self.data_dir.join("Logs").join("filmcraft.log")).unwrap_or_default();
        format!("\n--- filmcraft.log ---\n{app_log}\n--- stderr ---\n{}\n--- stdout ---\n{}", read("stderr.txt"), read("stdout.txt"))
    }
}

impl Drop for App {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn app_log(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("Logs").join("filmcraft.log")).unwrap_or_default()
}

fn crash_logs(dir: &Path) -> String {
    let Ok(entries) = std::fs::read_dir(dir.join("Logs")) else { return String::new() };
    entries
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("crash-"))
        .map(|e| std::fs::read_to_string(e.path()).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The field failure: the paint path panics inside egui-wgpu. The process must survive it, open
/// a new window with the same session, say so, log it, and keep the start mode.
#[test]
fn a_paint_panic_recreates_the_window_and_the_app_keeps_answering() {
    if let Some(why) = skip_reason() {
        eprintln!("SKIPPED (a_paint_panic_recreates_the_window_and_the_app_keeps_answering): {why}");
        return;
    }
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut app = App::start("paint-panic");
    // something to lose: an edit in the session that must survive the window
    app.call("engine.execute", json!({"command": "file.newBin", "params": {"name": "Survivor"}}));
    let r = app.call("ui.injectGraphicsFault", json!({"kind": "paintPanic"}));
    assert_eq!(r["kind"], "paintPanic");
    let label = app.wait_for_notice();
    assert_eq!(label, if cfg!(target_os = "macos") { "Graphics device lost; the window was recreated. Save your work." } else { MESSAGE });
    // the same session: the bin is still there, commands still run
    let project = app.call("engine.execute", json!({"command": "project.inspect"}));
    assert!(project.to_string().contains("Survivor"), "the session carried over: {project}");
    let r = app.call("ui.menu.invoke", json!({"id": "help.graphicsRecovery"}));
    assert_eq!(r["report"]["recovered"], json!(true), "{r}");
    assert_eq!(r["report"]["panicked"], json!(true), "{r}");
    assert_eq!(r["report"]["software"], json!(!cfg!(target_os = "macos")), "{r}");
    let (status, dir) = app.quit();
    assert!(status.success(), "clean exit after the recovery: {status:?}");
    // the panic is in the app log and the crash log
    let log = app_log(&dir);
    assert!(log.contains("Failed to create staging buffer for index data"), "app log has the panic:\n{log}");
    assert!(log.contains("recreating the window"), "app log says what was done:\n{log}");
    let crash = crash_logs(&dir);
    assert!(crash.contains("Failed to create staging buffer for index data"), "crash log has the panic:\n{crash}");
    // the loss is recorded; the saved start mode is untouched
    let lost: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("graphics-device-lost.json")).expect("graphics-device-lost.json")).expect("json");
    assert_eq!(lost["recoveredIn"], json!(if cfg!(target_os = "macos") { "default" } else { "software" }), "{lost}");
    assert!(lost["reason"].as_str().is_some_and(|r| r.contains("staging buffer")), "{lost}");
    let start = std::fs::read_to_string(dir.join("graphics-start.json")).unwrap_or_default();
    assert!(!start.contains("software"), "the start mode is not changed by a device loss: {start}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A device that refuses every allocation (destroyed, as a starved one does): the device-lost
/// callback flags it, the app stops drawing and the host opens a new window.
#[test]
fn a_destroyed_device_recreates_the_window() {
    if let Some(why) = skip_reason() {
        eprintln!("SKIPPED (a_destroyed_device_recreates_the_window): {why}");
        return;
    }
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut app = App::start("destroy");
    let r = app.call("ui.injectGraphicsFault", json!({"kind": "destroyDevice"}));
    assert_eq!(r["kind"], "destroyDevice");
    let label = app.wait_for_notice();
    assert!(label.starts_with("Graphics device lost"), "{label}");
    assert_eq!(app.call("ui.inspect", json!({}))["playback"]["playing"], json!(false));
    let r = app.call("ui.menu.invoke", json!({"id": "help.graphicsRecovery"}));
    assert_eq!(r["report"]["recovered"], json!(true), "{r}");
    let (status, dir) = app.quit();
    assert!(status.success(), "{status:?}");
    let log = app_log(&dir);
    assert!(log.contains("graphics device lost"), "app log has the loss:\n{log}");
    assert!(dir.join("graphics-device-lost.json").exists());
    let _ = std::fs::remove_dir_all(&dir);
}
