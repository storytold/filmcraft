//! Robustness: mutated, truncated and spliced interchange files (EDL, FCP7 XML, FCPXML, OTIO, AAF,
//! OMF, ALE) must never panic; errors and partial imports are fine.
//!
//! `FILMCRAFT_FUZZ_ITERS=<n>` runs `n` mutations per seed instead of the default, for a deeper
//! local run. Run it in a debug build too: only there does integer overflow panic.
//!
//! One known panic is left out by default: a `file://` URL whose byte 9 after the scheme falls
//! inside a multi-byte character (`file:///Users/Jöran/…`) panics in `file_url_to_path`
//! (`src/common.rs`), which an open pull request fixes separately (storytold/filmcraft#1609).
//! `FILMCRAFT_FUZZ_URL_PANICS=1` counts it too; once that fix is in, drop [`known_url_panic`].

mod support;

use std::cell::RefCell;
use std::panic::{AssertUnwindSafe, PanicHookInfo, catch_unwind};
use std::sync::Once;

use filmcraft_interchange::{ExportOptions, Format, detect, export, import};
use filmcraft_project::{ItemId, Project, TrackKind, TransitionAlign};
use filmcraft_time::FrameRate;
use support::*;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// Numbers that break careless arithmetic, written over a number in a text format.
const HOSTILE_NUMBERS: &[&str] =
    &["0", "-1", "-5", "1.5", "4294967296", "99999999999999999999", "-9223372036854775808", "NaN", "inf", "1e308", "0/0", "1/0", "99:99:99:99", "00:00:00;00"];

fn mutate(seed: &[u8], rng: &mut Rng) -> Vec<u8> {
    let mut d = seed.to_vec();
    for _ in 0..1 + rng.below(4) {
        match rng.below(7) {
            0 => {
                // truncation
                let n = rng.below(d.len() + 1);
                d.truncate(n);
            }
            1 if !d.is_empty() => {
                // bit flip
                let p = rng.below(d.len());
                d[p] ^= 1 << rng.below(8);
            }
            2 if !d.is_empty() => {
                // a byte that ends strings, starts markup or begins a multi-byte character
                let p = rng.below(d.len());
                d[p] = [0, 0xff, 0x7f, 0x80, 0xc3, 0xe6, b'9', b'-', b'<', b'"', b'\n'][rng.below(11)];
            }
            3 => {
                // a run of digits replaced by a hostile number
                let starts: Vec<usize> = (0..d.len()).filter(|&i| d[i].is_ascii_digit() && (i == 0 || !d[i - 1].is_ascii_digit())).collect();
                if !starts.is_empty() {
                    let s = starts[rng.below(starts.len())];
                    let mut e = s;
                    while e < d.len() && (d[e].is_ascii_digit() || d[e] == b'.') {
                        e += 1;
                    }
                    let n = HOSTILE_NUMBERS[rng.below(HOSTILE_NUMBERS.len())].as_bytes();
                    d.splice(s..e, n.iter().copied());
                }
            }
            4 if d.len() > 4 => {
                // a span removed
                let s = rng.below(d.len());
                let e = (s + 1 + rng.below(64)).min(d.len());
                d.drain(s..e);
            }
            5 if d.len() > 4 => {
                // a span duplicated elsewhere
                let s = rng.below(d.len());
                let e = (s + 1 + rng.below(256)).min(d.len());
                let chunk = d[s..e].to_vec();
                let at = rng.below(d.len());
                d.splice(at..at, chunk);
            }
            6 if d.len() >= 4 => {
                // a hostile 32-bit value (sizes, counts and offsets in AAF / OMF)
                let p = rng.below(d.len() - 3);
                let v = [0u32, u32::MAX, 0x7fff_ffff, 0x8000_0000, 0x1000_0000][rng.below(5)];
                d[p..p + 4].copy_from_slice(&v.to_le_bytes());
            }
            _ => {}
        }
    }
    d
}

/// Linked audio and video, two video tracks, a transition, and a media path outside ASCII.
fn project() -> (Project, ItemId) {
    let r = FrameRate::FPS_29_97;
    let mut p = Project::new("fuzz");
    let a = media(&mut p, "/m/a.mov", true, true, r);
    let b = media(&mut p, "/m/Jöran 日本/b.mov", true, true, r);
    let s = sequence(&mut p, "Seq", r, true);
    let v1 = clip(&mut p, s, TrackKind::Video, 0, a, 0, 50, 10);
    let a1 = clip(&mut p, s, TrackKind::Audio, 0, a, 0, 50, 10);
    link(&mut p, s, &[v1, a1]);
    let v2 = clip(&mut p, s, TrackKind::Video, 0, b, 50, 60, 100);
    clip(&mut p, s, TrackKind::Video, 1, b, 20, 30, 0);
    clip(&mut p, s, TrackKind::Audio, 1, b, 30, 70, 5);
    transition(&mut p, s, TrackKind::Video, 0, "cross_dissolve", Some(v1), Some(v2), 45, 10, TransitionAlign::CenterAtCut);
    (p, s)
}

fn iterations(default: usize) -> usize {
    std::env::var("FILMCRAFT_FUZZ_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

thread_local! {
    /// While a fuzz input runs on this thread: where its panic happened (`file:line`).
    static PANIC_AT: RefCell<Option<Option<String>>> = const { RefCell::new(None) };
}

/// Record the location of panics inside [`never_panics`] (quietly: there can be many); every
/// other panic, such as a failing assertion, goes to the default hook as usual.
fn install_hook() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let default = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info: &PanicHookInfo| {
            let fuzzing = PANIC_AT.with(|p| {
                let mut p = p.borrow_mut();
                p.as_mut().map(|at| *at = info.location().map(|l| format!("{}:{}", l.file(), l.line()))).is_some()
            });
            if !fuzzing {
                default(info);
            }
        }));
    });
}

/// The `file_url_to_path` char-boundary panic (storytold/filmcraft#1609), left out by default.
fn known_url_panic(at: &str, msg: &str) -> bool {
    std::env::var_os("FILMCRAFT_FUZZ_URL_PANICS").is_none()
        && at.replace('\\', "/").ends_with("interchange/src/common.rs:68")
        && msg.contains("is not a char boundary")
}

/// Run `f` on `iters` mutations of each seed under `catch_unwind`; fail with every distinct panic.
fn never_panics(seeds: &[(String, Vec<u8>)], iters: usize, f: &dyn Fn(&str, &[u8])) {
    install_hook();
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut panics: Vec<(String, String)> = Vec::new();
    let mut known = 0usize;
    for (name, seed) in seeds {
        for _ in 0..iters {
            let d = mutate(seed, &mut rng);
            PANIC_AT.with(|p| *p.borrow_mut() = Some(None));
            let r = catch_unwind(AssertUnwindSafe(|| f(name, &d)));
            let at = PANIC_AT.with(|p| p.borrow_mut().take()).flatten().unwrap_or_default();
            if let Err(e) = r {
                let msg = e.downcast_ref::<String>().cloned().or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default();
                if known_url_panic(&at, &msg) {
                    known += 1;
                    continue;
                }
                let key = format!("{name}: {at}: {msg}");
                if !panics.iter().any(|(k, _)| *k == key) {
                    panics.push((key, format!("{:?}", String::from_utf8_lossy(&d[..d.len().min(200)]))));
                }
            }
        }
    }
    if known > 0 {
        eprintln!("left out {known} known file-URL panics (storytold/filmcraft#1609)");
    }
    let report: Vec<String> = panics.iter().map(|(k, input)| format!("{k}\n    input: {input}")).collect();
    assert!(panics.is_empty(), "{} distinct panics:\n{}", panics.len(), report.join("\n"));
}

#[test]
fn mutated_documents_never_panic() {
    let (p, s) = project();
    let mut seeds = Vec::new();
    for f in Format::ALL {
        let (bytes, _) = export(&p, s, f, &ExportOptions::default()).expect("seed export");
        import(&bytes, f, None).expect("seed imports");
        seeds.push((format!("{f:?}"), bytes));
    }
    never_panics(&seeds, iterations(2000), &|name, d| {
        let Some(f) = Format::ALL.into_iter().find(|f| format!("{f:?}") == name) else { return };
        let _ = import(d, f, None);
        let _ = import(d, f, Some("/base dir/ü"));
        let _ = detect(d, None);
        let _ = detect(d, Some(f.extension()));
    });
}

#[test]
fn mutated_ale_never_panics() {
    let ale = "Heading\nFIELD_DELIM\tTABS\nVIDEO_FORMAT\t1080\nAUDIO_FORMAT\t48khz\nFPS\t29.97\n\nColumn\nName\tTracks\tStart\tEnd\tTape\tSource File\n\nData\n\
               clip1\tVA1A2\t01:00:00;00\t01:00:10;00\tA001\t/m/a.mov\nclip2\tV\t01:00:10;00\t01:00:12;15\tA002\t/m/Jöran/b.mov\n";
    filmcraft_interchange::ale::parse(ale).expect("seed parses");
    never_panics(&[("ALE".into(), ale.as_bytes().to_vec())], iterations(4000), &|_, d| {
        let _ = filmcraft_interchange::ale::parse(&String::from_utf8_lossy(d));
    });
}
