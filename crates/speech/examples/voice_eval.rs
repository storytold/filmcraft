//! Measure [`filmcraft_speech::voice`] on a real recording against reference word timings.
//!
//! ```sh
//! cargo run --release -p filmcraft-speech --example voice_eval -- clip.wav reference.json words.json [--verbose]
//! ```
//!
//! - `clip.wav`: mono (or multi-channel, downmixed) 16 kHz PCM16 or float32 WAV.
//! - `reference.json`: the most precise word timings available (`{"words":[{"text","start","end"}]}`,
//!   seconds), used as ground truth.
//! - `words.json`: the recogniser's words, same format: passed to `voice_map` and snapped.
//!
//! Metrics: speech kept (middle 60 % of every reference word inside a span, no word losing its
//! first/last 20 ms), pauses found (reference gaps of 120 ms+ and 80–120 ms at least 70 % outside
//! the spans), edge accuracy at 120 ms+ pauses, word snapping before/after, and stray clicks.
//! The reference itself is not perfect: misses list the audio level in the region so a reference
//! gap with loud voice in it (or a reference word over silence) can be told from a real miss, and
//! an "edge sanity" line checks reference and span edges against the audio alone.
//!
//! Options: `--verbose` (every miss, edge and word), `--at=a,b` (levels, spans and words from `a`
//! to `b` seconds, 5 ms steps), `--quiet-inside` (60 ms+ stretches 30 dB under speech kept inside
//! spans).

use filmcraft_project::Word;
use filmcraft_speech::voice::{VoiceMap, snap_words, voice_map};
use filmcraft_speech::{SAMPLE_RATE, seconds_tick};
use filmcraft_time::Tick;

type Res<T> = Result<T, String>;

fn main() {
    if let Err(e) = run() {
        eprintln!("voice_eval: {e}");
        std::process::exit(1);
    }
}

fn run() -> Res<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let verbose = args.iter().any(|a| a == "--verbose" || a == "-v");
    let pos: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();
    let [wav, reference, hyp] = pos.as_slice() else {
        return Err("usage: voice_eval <clip.wav> <reference.json> <words.json> [--verbose] [--at=a,b] [--quiet-inside]".into());
    };
    let audio = read_wav(wav)?;
    let refw = read_words(reference)?;
    let hyp = read_words(hyp)?;
    let t0 = std::time::Instant::now();
    let vm = voice_map(&audio, &hyp);
    let t_map = t0.elapsed().as_secs_f64();
    let mut snapped = hyp.clone();
    let t1 = std::time::Instant::now();
    snap_words(&mut snapped, &vm);
    let t_snap = t1.elapsed().as_secs_f64();
    let lv = FrameLevels::new(&audio);
    let spans: Vec<(f64, f64)> = vm.spans.iter().map(|r| (sec(r.start), sec(r.start + r.duration))).collect();
    let dur = audio.len() as f64 / f64::from(SAMPLE_RATE);
    println!("== {wav}");
    println!(
        "audio {dur:.1}s | floor {:.1} dBFS, speech {:.1} dBFS, gate {:.1} dBFS | {} spans | voice_map {:.0} ms, snap_words {:.1} ms",
        vm.floor_db,
        vm.speech_db,
        vm.gate_db,
        spans.len(),
        t_map * 1e3,
        t_snap * 1e3
    );
    let mut hist = [0usize; 6];
    for w in spans.windows(2) {
        let [(_, a), (b, _)] = w else { continue };
        let g = b - a;
        let k = [0.060, 0.080, 0.120, 0.220, 0.500].iter().filter(|t| g >= **t).count();
        if let Some(h) = hist.get_mut(k) {
            *h += 1;
        }
    }
    println!("gaps between spans: <60 ms {} | 60-80 {} | 80-120 {} | 120-220 {} | 220-500 {} | >=500 {}", hist[0], hist[1], hist[2], hist[3], hist[4], hist[5]);
    if args.iter().any(|a| a == "--quiet-inside") {
        // stretches of 60 ms+ at least 30 dB below the speech level that the spans keep
        for &(a, b) in &spans {
            let mut t = a;
            let mut run: Option<f64> = None;
            while t < b {
                let q = lv.max_db(t, t + 0.005) < vm.speech_db - 30.0;
                match (q, run) {
                    (true, None) => run = Some(t),
                    (false, Some(r0)) => {
                        if t - r0 >= 0.060 {
                            println!("   quiet inside span {r0:8.3}-{t:8.3} ({:3.0} ms)", (t - r0) * 1e3);
                        }
                        run = None;
                    }
                    _ => {}
                }
                t += 0.005;
            }
        }
    }
    let r: Vec<(f64, f64, &str)> = refw.iter().map(|w| (sec(w.start), sec(w.end), w.text.as_str())).collect();
    speech_kept(&r, &spans, &lv, vm.speech_db, verbose);
    pauses(&r, &spans, &lv, verbose);
    edges(&r, &spans, &lv, vm.speech_db, verbose);
    if let Some(at) = args.iter().find_map(|a| a.strip_prefix("--at=")) {
        dump(at, &spans, &refw, &hyp, &snapped, &lv);
    }
    snapping(&refw, &hyp, &snapped, &vm, verbose);
    clicks(&r, &spans, &lv);
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// metrics

fn speech_kept(r: &[(f64, f64, &str)], spans: &[(f64, f64)], lv: &FrameLevels, speech_db: f32, verbose: bool) {
    let (mut total, mut kept, mut total_v, mut kept_v) = (0.0, 0.0, 0.0, 0.0);
    let mut short_mid = Vec::new();
    let mut lost_edges = Vec::new();
    // what was lost counts as audible voice when it is within 30 dB of the speech level
    let audible = speech_db - 30.0;
    for &(s, e, text) in r {
        let d = e - s;
        if d <= 0.0 {
            continue;
        }
        let (a, b) = (s + 0.2 * d, e - 0.2 * d);
        let c = covered(spans, a, b);
        total += b - a;
        kept += c;
        // a reference word whose middle has no voice at all (40 dB under speech) is misplaced
        if lv.max_db(a, b) >= speech_db - 40.0 {
            total_v += b - a;
            kept_v += c;
        }
        if (b - a) - c > 0.0005 {
            short_mid.push((s, e, text, (b - a) - c, lost_level(spans, lv, a, b)));
        }
        let ed = 0.02f64.min(d);
        for (tag, x, y) in [("first", s, s + ed), ("last", e - ed, e)] {
            if covered(spans, x, y) < 0.5 * (y - x) {
                lost_edges.push((s, e, text, tag, lost_level(spans, lv, x, y)));
            }
        }
    }
    println!(
        "1. speech kept: {:.2} % of the middle 60 % of {} reference words inside spans (target >= 99.5 %); {:.2} % without reference words that have no voice under them",
        100.0 * kept / total.max(1e-9),
        r.len(),
        100.0 * kept_v / total_v.max(1e-9),
    );
    println!(
        "   {} words with a gap in the middle ({} losing a frame within 30 dB of speech); {} words lose their first/last 20 ms (< 50 % covered; {} of them audible voice)",
        short_mid.len(),
        short_mid.iter().filter(|m| m.4 >= audible).count(),
        lost_edges.len(),
        lost_edges.iter().filter(|m| m.4 >= audible).count()
    );
    let lim = if verbose { usize::MAX } else { 12 };
    for (s, e, t, miss, lvl) in short_mid.iter().filter(|m| verbose || m.4 >= audible).take(lim) {
        println!("     mid gap   {s:8.2}-{e:8.2} {t:<14} {:4.0} ms missing, loudest missing frame {lvl:6.1} dBFS", miss * 1e3);
    }
    for (s, e, t, tag, lvl) in lost_edges.iter().filter(|m| verbose || m.4 >= audible).take(lim) {
        println!("     lost {tag:<5} {s:8.2}-{e:8.2} {t:<14} loudest lost frame {lvl:6.1} dBFS");
    }
}

fn pauses(r: &[(f64, f64, &str)], spans: &[(f64, f64)], lv: &FrameLevels, verbose: bool) {
    for (lo, hi, label) in [(0.120, f64::MAX, ">= 120 ms"), (0.080, 0.120, "80-120 ms")] {
        let (mut n, mut found, mut n_quiet, mut found_quiet) = (0, 0, 0, 0);
        let mut misses = Vec::new();
        for w in r.windows(2) {
            let [(_, pe, pt), (ns, _, nt)] = w else { continue };
            let g = ns - pe;
            if g < lo || g >= hi {
                continue;
            }
            n += 1;
            let free = 1.0 - covered(spans, *pe, *ns) / g;
            // a reference gap with 30 ms+ of loud sound is not a pause in the audio
            let loud_ms = lv.ms_above(*pe, *ns, -35.0);
            let quiet = loud_ms < 30.0;
            if quiet {
                n_quiet += 1;
            }
            if free >= 0.7 {
                found += 1;
                if quiet {
                    found_quiet += 1;
                }
            } else {
                misses.push((*pe, *ns, *pt, *nt, free, loud_ms, lv.max_db(*pe, *ns)));
            }
        }
        println!(
            "2. pauses {label}: {found}/{n} = {:.1} % found (>= 70 % outside spans); excluding {} reference gaps with 30 ms+ above -35 dBFS: {found_quiet}/{n_quiet} = {:.1} %",
            100.0 * found as f64 / f64::from(n.max(1)),
            n - n_quiet,
            100.0 * found_quiet as f64 / f64::from(n_quiet.max(1)),
        );
        let lim = if verbose { usize::MAX } else { 10 };
        for (a, b, pt, nt, free, loud, mx) in misses.iter().take(lim) {
            println!(
                "     missed {a:8.2}-{b:8.2} ({:3.0} ms) {pt}|{nt}: {:3.0} % outside spans; {loud:3.0} ms above -35 dBFS, max {mx:6.1}",
                (b - a) * 1e3,
                free * 100.0
            );
        }
    }
}

fn edges(r: &[(f64, f64, &str)], spans: &[(f64, f64)], lv: &FrameLevels, speech_db: f32, verbose: bool) {
    // gaps between spans (with the open ends)
    let mut gaps = Vec::with_capacity(spans.len() + 1);
    let mut prev = f64::MIN;
    for &(a, b) in spans {
        gaps.push((prev, a));
        prev = b;
    }
    gaps.push((prev, f64::MAX));
    let mut rows = Vec::new();
    let mut levels: Vec<(f32, f32)> = Vec::new();
    let mut plaus: Vec<([bool; 3], [bool; 3])> = Vec::new();
    for w in r.windows(2) {
        let [(_, pe, _), (ns, _, _)] = w else { continue };
        if ns - pe < 0.120 {
            continue;
        }
        let best = gaps.iter().map(|&(a, b)| (overlap(a, b, *pe, *ns), a, b)).max_by(|x, y| x.0.total_cmp(&y.0));
        let Some((ov, a, b)) = best else { continue };
        if ov <= 0.0 {
            continue;
        }
        let quiet = lv.ms_above(*pe, *ns, -35.0) < 30.0;
        if quiet && a > f64::MIN {
            levels.push((lv.max_db(*pe - 0.0025, *pe + 0.0025), lv.max_db(a - 0.0075, a - 0.0025)));
        }
        if quiet && a > f64::MIN && b < f64::MAX {
            // an end that still has loud voice 10-30 ms after it, a start that has loud voice
            // 10-30 ms before it, or a start with 25 ms of deep silence after it
            let (loud, deep) = (speech_db - 25.0, speech_db - 45.0);
            let check = |end: f64, start: f64| {
                [lv.max_db(end + 0.010, end + 0.030) >= loud, lv.max_db(start - 0.030, start - 0.010) >= loud, lv.max_db(start + 0.005, start + 0.030) < deep]
            };
            let c = check(a, b);
            if verbose && c.iter().any(|x| *x) {
                println!("     span edge suspicious near pause {pe:8.3}-{ns:8.3}: span gap {a:8.3}-{b:8.3} {c:?}");
            }
            plaus.push((check(*pe, *ns), c));
        }
        rows.push((*pe, *ns, (a > f64::MIN).then_some(a - pe), (b < f64::MAX).then_some(b - ns), quiet));
    }
    for (label, only_quiet) in [("all", false), ("quiet reference gaps only", true)] {
        let sel = || rows.iter().filter(|r| !only_quiet || r.4);
        let ends: Vec<f64> = sel().filter_map(|r| r.2).collect();
        let starts: Vec<f64> = sel().filter_map(|r| r.3).collect();
        let all: Vec<f64> = ends.iter().chain(&starts).map(|v| v.abs()).collect();
        println!(
            "3. edges at pauses >= 120 ms ({label}, {} edges): |span edge - reference| median {:.1} ms, p90 {:.1} ms (target median <= 25); span end - word end median {:+.1} ms; span start - word start median {:+.1} ms",
            all.len(),
            median(&all) * 1e3,
            pct(&all, 0.9) * 1e3,
            median(&ends) * 1e3,
            median(&starts) * 1e3
        );
    }
    let count = |f: &dyn Fn(&([bool; 3], [bool; 3])) -> bool| plaus.iter().filter(|p| f(p)).count();
    println!(
        "   edge sanity at {} quiet pauses (audio, no reference needed): ends with loud voice 10-30 ms later: reference {} / spans {}; starts with loud voice 10-30 ms earlier: reference {} / spans {}; starts followed by 25 ms of deep silence: reference {} / spans {}",
        plaus.len(),
        count(&|p| p.0[0]),
        count(&|p| p.1[0]),
        count(&|p| p.0[1]),
        count(&|p| p.1[1]),
        count(&|p| p.0[2]),
        count(&|p| p.1[2])
    );
    let at_ref: Vec<f64> = levels.iter().map(|l| f64::from(l.0)).collect();
    let at_span: Vec<f64> = levels.iter().map(|l| f64::from(l.1)).collect();
    println!(
        "   level where the voice ends at quiet pauses: at the reference word end median {:.1} dBFS, in the last 5 ms of the span median {:.1} dBFS",
        median(&at_ref),
        median(&at_span)
    );
    if verbose {
        for (pe, ns, de, ds, quiet) in rows {
            println!(
                "     pause {pe:8.2}-{ns:8.2}{}: end {:+5.0} ms, start {:+5.0} ms",
                if quiet { "" } else { " (loud)" },
                de.unwrap_or(f64::NAN) * 1e3,
                ds.unwrap_or(f64::NAN) * 1e3
            );
        }
    }
}

fn snapping(refw: &[Word], hyp: &[Word], snapped: &[Word], vm: &VoiceMap, verbose: bool) {
    let pairs = align(refw, hyp);
    let stats = |ws: &[Word]| {
        let mut s = Vec::new();
        let mut e = Vec::new();
        for &(ri, hi) in &pairs {
            let (Some(rw), Some(hw)) = (refw.get(ri), ws.get(hi)) else { continue };
            s.push((sec(hw.start) - sec(rw.start)).abs());
            e.push((sec(hw.end) - sec(rw.end)).abs());
        }
        let long = ws.iter().filter(|w| sec(w.end) - sec(w.start) > 1.5).count();
        (s, e, long)
    };
    let (bs, be, bl) = stats(hyp);
    let (as_, ae, al) = stats(snapped);
    // only the bounds next to a reference pause of 120 ms+ (where the audio has something to say)
    let at_pause = |ws: &[Word]| {
        let mut v = Vec::new();
        for &(ri, hi) in &pairs {
            let (Some(rw), Some(hw)) = (refw.get(ri), ws.get(hi)) else { continue };
            let before = ri.checked_sub(1).and_then(|p| refw.get(p)).is_none_or(|p| sec(rw.start) - sec(p.end) >= 0.120);
            let after = refw.get(ri + 1).is_none_or(|n| sec(n.start) - sec(rw.end) >= 0.120);
            if before {
                v.push((sec(hw.start) - sec(rw.start)).abs());
            }
            if after {
                v.push((sec(hw.end) - sec(rw.end)).abs());
            }
        }
        v
    };
    let (bp, ap) = (at_pause(hyp), at_pause(snapped));
    if verbose {
        for &(ri, hi) in &pairs {
            let (Some(rw), Some(hw), Some(sw)) = (refw.get(ri), hyp.get(hi), snapped.get(hi)) else { continue };
            let before = ri.checked_sub(1).and_then(|p| refw.get(p)).is_none_or(|p| sec(rw.start) - sec(p.end) >= 0.120);
            let after = refw.get(ri + 1).is_none_or(|n| sec(n.start) - sec(rw.end) >= 0.120);
            let ds = (sec(sw.start) - sec(rw.start)) * 1e3;
            let de = (sec(sw.end) - sec(rw.end)) * 1e3;
            if (before && ds.abs() > 80.0) || (after && de.abs() > 80.0) {
                println!(
                    "     far at pause: {:<12} ref {:8.3}-{:8.3} hyp {:8.3}-{:8.3} snapped {:8.3}-{:8.3} ({}{})",
                    rw.text,
                    sec(rw.start),
                    sec(rw.end),
                    sec(hw.start),
                    sec(hw.end),
                    sec(sw.start),
                    sec(sw.end),
                    if before { format!("start {ds:+.0} ") } else { String::new() },
                    if after { format!("end {de:+.0}") } else { String::new() }
                );
            }
        }
    }
    println!("4. snapping ({} of {} words matched to the reference by text):", pairs.len(), hyp.len());
    println!("              start |err| median / p90     end |err| median / p90     words > 1.5 s");
    println!(
        "     before   {:7.1} / {:7.1} ms         {:7.1} / {:7.1} ms         {bl}",
        median(&bs) * 1e3,
        pct(&bs, 0.9) * 1e3,
        median(&be) * 1e3,
        pct(&be, 0.9) * 1e3
    );
    println!(
        "     after    {:7.1} / {:7.1} ms         {:7.1} / {:7.1} ms         {al}",
        median(&as_) * 1e3,
        pct(&as_, 0.9) * 1e3,
        median(&ae) * 1e3,
        pct(&ae, 0.9) * 1e3
    );
    println!(
        "     bounds next to a reference pause >= 120 ms ({}): |err| median / p90 before {:.1} / {:.1} ms, after {:.1} / {:.1} ms",
        bp.len(),
        median(&bp) * 1e3,
        pct(&bp, 0.9) * 1e3,
        median(&ap) * 1e3,
        pct(&ap, 0.9) * 1e3
    );
    // what the caller sees: a gap between spans is a pause unless one word covers it
    let spans: Vec<(f64, f64)> = vm.spans.iter().map(|r| (sec(r.start), sec(r.start + r.duration))).collect();
    let r: Vec<(f64, f64)> = refw.iter().map(|w| (sec(w.start), sec(w.end))).collect();
    for (label, ws) in [("before", hyp), ("after", snapped)] {
        let wr: Vec<(f64, f64)> = ws.iter().map(|w| (sec(w.start), sec(w.end))).collect();
        let (mut n, mut ok) = (0, 0);
        for w in r.windows(2) {
            let [(_, pe), (ns, _)] = w else { continue };
            if ns - pe < 0.120 {
                continue;
            }
            n += 1;
            // span gaps overlapping this reference gap that no single word swallows
            let mut free = 0.0;
            let mut prev = 0.0f64;
            for &(a, b) in spans.iter().chain(std::iter::once(&(f64::MAX, f64::MAX))) {
                let (ga, gb) = (prev, a);
                prev = b;
                let ov = overlap(ga, gb, *pe, *ns);
                if ov <= 0.0 {
                    continue;
                }
                let inside = wr.iter().any(|&(ws, we)| ws <= ga.max(*pe) && we >= gb.min(*ns));
                if !inside {
                    free += ov;
                }
            }
            if free >= 0.7 * (ns - pe) {
                ok += 1;
            }
        }
        println!("     pauses >= 120 ms not swallowed by a word ({label} snapping): {ok}/{n} = {:.1} %", 100.0 * ok as f64 / f64::from(n.max(1)));
    }
    let silent = |ws: &[Word]| ws.iter().filter(|w| covered(&spans, sec(w.start), sec(w.end)) <= 0.0).count();
    let partly = |ws: &[Word]| ws.iter().filter(|w| covered(&spans, sec(w.start), sec(w.end)) < 0.5 * (sec(w.end) - sec(w.start))).count();
    println!(
        "     words with no voice under them: before {}, after {}; words less than half voiced: before {}, after {}",
        silent(hyp),
        silent(snapped),
        partly(hyp),
        partly(snapped)
    );
    if verbose {
        for (i, (h, s)) in hyp.iter().zip(snapped).enumerate() {
            let m = pairs.iter().find(|p| p.1 == i).and_then(|p| refw.get(p.0));
            println!(
                "     {:<14} {:8.3}-{:8.3} -> {:8.3}-{:8.3}   ref {}",
                h.text,
                sec(h.start),
                sec(h.end),
                sec(s.start),
                sec(s.end),
                m.map(|w| format!("{:8.3}-{:8.3}", sec(w.start), sec(w.end))).unwrap_or_default()
            );
        }
    }
}

fn clicks(r: &[(f64, f64, &str)], spans: &[(f64, f64)], lv: &FrameLevels) {
    let stray: Vec<&(f64, f64)> = spans.iter().filter(|(a, b)| b - a < 0.040 && !r.iter().any(|(s, e, _)| s < b && a < e)).collect();
    println!("5. clicks: {} spans < 40 ms overlapping no reference word (target ~0)", stray.len());
    for (i, (a, b)) in spans.iter().enumerate() {
        if b - a >= 0.060 {
            continue;
        }
        let prev = i.checked_sub(1).and_then(|p| spans.get(p)).map_or(f64::INFINITY, |p| a - p.1);
        let next = spans.get(i + 1).map_or(f64::INFINITY, |n| n.0 - b);
        let words: Vec<&str> = r.iter().filter(|(s, e, _)| s < b && a < e).map(|w| w.2).collect();
        println!(
            "     short span {a:8.3}-{b:8.3} ({:2.0} ms), {:4.0} ms after the previous, {:4.0} ms before the next; reference words {words:?}",
            (b - a) * 1e3,
            prev * 1e3,
            next * 1e3
        );
    }
    for (a, b) in stray.iter().take(20) {
        println!("     {a:8.3}-{b:8.3} max {:6.1} dBFS", lv.max_db(*a, *b));
    }
}

/// `--at=a,b`: spans, words and levels between `a` and `b` seconds.
fn dump(at: &str, spans: &[(f64, f64)], refw: &[Word], hyp: &[Word], snapped: &[Word], lv: &FrameLevels) {
    let mut it = at.split(',').filter_map(|v| v.parse::<f64>().ok());
    let (Some(a), Some(b)) = (it.next(), it.next()) else { return };
    let at_t = |ws: &[Word], t: f64| ws.iter().filter(|w| sec(w.start) <= t && t < sec(w.end)).map(|w| w.text.clone()).collect::<Vec<_>>().join(",");
    let mut t = a;
    while t < b {
        let db = lv.max_db(t, t + 0.005);
        let inside = covered(spans, t, t + 0.005) > 0.0025;
        let bar = "#".repeat(((db + 100.0) / 2.0).max(0.0) as usize);
        println!(
            "{t:8.3} {db:6.1} {} {bar:<45} ref {:<12} hyp {:<12} snapped {}",
            if inside { "V" } else { "." },
            at_t(refw, t),
            at_t(hyp, t),
            at_t(snapped, t)
        );
        t += 0.005;
    }
}

// ---------------------------------------------------------------------------------------------
// helpers

fn sec(t: Tick) -> f64 {
    t.seconds()
}

fn overlap(a: f64, b: f64, c: f64, d: f64) -> f64 {
    (b.min(d) - a.max(c)).max(0.0)
}

/// Seconds of `a..b` inside `spans` (sorted, non-overlapping).
fn covered(spans: &[(f64, f64)], a: f64, b: f64) -> f64 {
    let i = spans.partition_point(|s| s.1 <= a);
    spans.get(i..).unwrap_or(&[]).iter().take_while(|s| s.0 < b).map(|s| overlap(s.0, s.1, a, b)).sum()
}

/// Loudest frame of `a..b` outside the spans.
fn lost_level(spans: &[(f64, f64)], lv: &FrameLevels, a: f64, b: f64) -> f32 {
    let mut m = f32::MIN;
    let mut t = a;
    while t < b {
        if covered(spans, t, t + 0.001) < 0.0005 {
            m = m.max(lv.max_db(t, t + 0.001));
        }
        t += 0.005;
    }
    m
}

fn median(v: &[f64]) -> f64 {
    pct(v, 0.5)
}

fn pct(v: &[f64], p: f64) -> f64 {
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    let i = ((s.len().saturating_sub(1)) as f64 * p).round() as usize;
    s.get(i).copied().unwrap_or(f64::NAN)
}

/// 10 ms RMS levels every 5 ms, for reporting.
struct FrameLevels {
    db: Vec<f32>,
}

impl FrameLevels {
    fn new(audio: &[f32]) -> Self {
        let db = (0..audio.len().div_ceil(80))
            .map(|i| {
                let w = audio.get(i * 80..(i * 80 + 160).min(audio.len())).unwrap_or(&[]);
                let e = w.iter().map(|x| f64::from(*x) * f64::from(*x)).sum::<f64>() / w.len().max(1) as f64;
                (10.0 * (e + 1e-20).log10()) as f32
            })
            .collect();
        Self { db }
    }
    /// Frames (10 ms windows every 5 ms) centred in `a..b`, or the one centred nearest to `a`.
    fn frames(&self, a: f64, b: f64) -> &[f32] {
        let n = self.db.len();
        let fa = ((a * 200.0 - 1.0).ceil().max(0.0) as usize).min(n);
        let fb = ((b * 200.0 - 1.0).ceil().max(0.0) as usize).clamp(fa, n);
        if fb > fa {
            return self.db.get(fa..fb).unwrap_or(&[]);
        }
        let i = ((a * 200.0 - 1.0).round().max(0.0) as usize).min(n.saturating_sub(1));
        self.db.get(i..i + 1).unwrap_or(&[])
    }
    fn max_db(&self, a: f64, b: f64) -> f32 {
        self.frames(a, b).iter().copied().fold(-200.0, f32::max)
    }
    fn ms_above(&self, a: f64, b: f64, th: f32) -> f64 {
        self.frames(a, b).iter().filter(|v| **v > th).count() as f64 * 5.0
    }
}

/// Align `hyp` to `refw` by normalised text (Levenshtein); returns matching index pairs.
fn align(refw: &[Word], hyp: &[Word]) -> Vec<(usize, usize)> {
    let a: Vec<String> = refw.iter().map(Word::normalized).collect();
    let b: Vec<String> = hyp.iter().map(Word::normalized).collect();
    let (n, m) = (a.len(), b.len());
    let mut d = vec![vec![0u32; m + 1]; n + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i as u32;
    }
    for j in 0..=m {
        d[0][j] = j as u32;
    }
    for i in 1..=n {
        for j in 1..=m {
            let sub = d[i - 1][j - 1] + u32::from(a[i - 1] != b[j - 1]);
            d[i][j] = sub.min(d[i - 1][j] + 1).min(d[i][j - 1] + 1);
        }
    }
    let (mut i, mut j) = (n, m);
    let mut out = Vec::new();
    while i > 0 && j > 0 {
        if a[i - 1] == b[j - 1] && d[i][j] == d[i - 1][j - 1] {
            out.push((i - 1, j - 1));
            i -= 1;
            j -= 1;
        } else if d[i][j] == d[i - 1][j - 1] + 1 {
            i -= 1;
            j -= 1;
        } else if d[i][j] == d[i - 1][j] + 1 {
            i -= 1;
        } else {
            j -= 1;
        }
    }
    out.reverse();
    out
}

fn read_words(path: &str) -> Res<Vec<Word>> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("{path}: {e}"))?;
    let words = v.get("words").and_then(|w| w.as_array()).ok_or_else(|| format!("{path}: no `words` array"))?;
    Ok(words
        .iter()
        .filter_map(|w| {
            let t = w.get("text")?.as_str()?.trim();
            let s = w.get("start")?.as_f64()?;
            let e = w.get("end")?.as_f64()?;
            Some(Word::new(t, seconds_tick(s), seconds_tick(e)))
        })
        .collect())
}

/// Mono samples of a 16 kHz PCM16 / float32 WAV.
fn read_wav(path: &str) -> Res<Vec<f32>> {
    let b = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    if b.get(0..4) != Some(b"RIFF") || b.get(8..12) != Some(b"WAVE") {
        return Err(format!("{path}: not a RIFF/WAVE file"));
    }
    let u16_at = |o: usize| b.get(o..o + 2).map(|s| u16::from_le_bytes([s[0], s[1]]));
    let u32_at = |o: usize| b.get(o..o + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]));
    let mut fmt = None;
    let mut data = None;
    let mut o = 12usize;
    while let (Some(id), Some(size)) = (b.get(o..o + 4), u32_at(o + 4)) {
        let body = o + 8;
        let size = size as usize;
        match id {
            b"fmt " => fmt = Some((u16_at(body), u16_at(body + 2), u32_at(body + 4), u16_at(body + 14))),
            b"data" => data = Some(b.get(body..(body + size).min(b.len())).unwrap_or(&[])),
            _ => {}
        }
        o = body.saturating_add(size).saturating_add(size & 1);
    }
    let (Some((Some(tag), Some(ch), Some(rate), Some(bits))), Some(data)) = (fmt, data) else {
        return Err(format!("{path}: missing fmt or data chunk"));
    };
    if rate != SAMPLE_RATE {
        return Err(format!("{path}: {rate} Hz; resample to 16 kHz first"));
    }
    let ch = usize::from(ch.max(1));
    let samples: Vec<f32> = match (tag, bits) {
        (1 | 0xFFFE, 16) => data.chunks_exact(2).map(|s| f32::from(i16::from_le_bytes([s[0], s[1]])) / 32768.0).collect(),
        (3 | 0xFFFE, 32) => data.chunks_exact(4).map(|s| f32::from_le_bytes([s[0], s[1], s[2], s[3]])).collect(),
        _ => return Err(format!("{path}: unsupported WAV format {tag} / {bits} bits")),
    };
    Ok(samples.chunks_exact(ch).map(|f| f.iter().sum::<f32>() / ch as f32).collect())
}
