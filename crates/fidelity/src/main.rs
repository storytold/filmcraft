//! `fidelity [--out DIR] [--reference DIR] [--case ID]… [--seconds N] [--size WxH] [--min PERCENT]
//! [--list]`: run the fidelity harness (see the crate docs; `cargo xtask fidelity` calls this).
//!
//! Exit status: 0 when the run completed (and the pass rate met `--min`, if given); 1 on an error
//! or a pass rate below `--min`; 2 on a usage error.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

use std::path::PathBuf;
use std::process::ExitCode;

use filmcraft_fidelity::{CASES, Compare, CorpusSettings, Options};

const USAGE: &str = "usage: fidelity [--out DIR] [--reference DIR] [--case ID]... [--seconds N] [--size WxH] [--min PERCENT] [--list]";

struct Args {
    options: Options,
    min: Option<f64>,
    list: bool,
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut a = Args {
        options: Options { out_dir: PathBuf::from("target/fidelity"), reference_dir: None, only: Vec::new(), settings: CorpusSettings::default() },
        min: None,
        list: false,
    };
    let mut reference_given = false;
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let mut value = || it.next().cloned().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--out" => a.options.out_dir = PathBuf::from(value()?),
            "--reference" => {
                a.options.reference_dir = Some(PathBuf::from(value()?));
                reference_given = true;
            }
            "--case" => a.options.only.push(value()?),
            "--seconds" => {
                let v = value()?;
                let n: u32 = v.parse().map_err(|_| format!("--seconds {v}: not a number"))?;
                if !(1..=60).contains(&n) {
                    return Err(format!("--seconds {v}: must be 1–60"));
                }
                a.options.settings.seconds = n;
            }
            "--size" => {
                let v = value()?;
                let (w, h) = v.split_once(['x', '×']).ok_or_else(|| format!("--size {v}: expected WxH"))?;
                let w: u32 = w.parse().map_err(|_| format!("--size {v}: bad width"))?;
                let h: u32 = h.parse().map_err(|_| format!("--size {v}: bad height"))?;
                if !(16..=8192).contains(&w) || !(16..=8192).contains(&h) || !w.is_multiple_of(2) || !h.is_multiple_of(2) {
                    return Err(format!("--size {v}: even sizes between 16 and 8192"));
                }
                a.options.settings.width = w;
                a.options.settings.height = h;
            }
            "--min" => {
                let v = value()?;
                let pct: f64 = v.parse().map_err(|_| format!("--min {v}: not a number"))?;
                if !(0.0..=100.0).contains(&pct) {
                    return Err(format!("--min {v}: must be 0–100"));
                }
                a.min = Some(pct);
            }
            "--list" => a.list = true,
            other => return Err(format!("unknown argument {other} ({USAGE})")),
        }
    }
    if !reference_given {
        // the maintainers' local renders (absolute, so PREMIERE.md names the directory in full)
        let cwd = std::env::current_dir().map_err(|e| format!("current directory: {e}"))?;
        a.options.reference_dir = Some(cwd.join("plan/premiere/fidelity"));
    }
    Ok(a)
}

fn list() -> String {
    let mut s = format!("{} cases\n", CASES.len());
    let width = CASES.iter().map(|c| c.id.len()).max().unwrap_or(0);
    for c in CASES {
        let side = match c.compare {
            Compare::Video => "picture",
            Compare::Audio => "sound  ",
        };
        let via = if c.xml_complete { "XML   " } else { "manual" };
        s.push_str(&format!("  {:<width$}  {side}  {via}  {:<12} {}\n", c.id, c.group, c.title));
    }
    s
}

fn main() -> ExitCode {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let args = match parse_args(&raw) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("fidelity: {e}");
            return ExitCode::from(2);
        }
    };
    if args.list {
        print!("{}", list());
        return ExitCode::SUCCESS;
    }
    let outcome = match filmcraft_fidelity::run(&args.options) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("fidelity: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("{}", outcome.summary());
    println!("report:   {}", outcome.report.display());
    println!("manifest: {}", outcome.manifest.display());
    println!("premiere: {}", outcome.premiere_md.display());
    if outcome.compared() == 0 {
        let where_ = match &args.options.reference_dir {
            Some(d) => format!("no Premiere renders under {}", d.display()),
            None => "no reference directory".to_string(),
        };
        println!("{where_}: fidelity not measured; follow {} to render the corpus in Premiere", outcome.premiere_md.display());
        return match args.min {
            Some(_) => {
                eprintln!("fidelity: --min needs reference renders");
                ExitCode::FAILURE
            }
            None => ExitCode::SUCCESS,
        };
    }
    match args.min {
        Some(min) if outcome.pass_rate() < min => {
            eprintln!("fidelity: pass rate {:.1}% is below --min {min}%", outcome.pass_rate());
            ExitCode::FAILURE
        }
        _ => ExitCode::SUCCESS,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_are_checked() {
        let a =
            parse_args(&["--out".into(), "/tmp/x".into(), "--case".into(), "cut".into(), "--case".into(), "crop".into(), "--min".into(), "80".into()]).unwrap();
        assert_eq!(a.options.out_dir, PathBuf::from("/tmp/x"));
        assert_eq!(a.options.only, ["cut", "crop"]);
        assert_eq!(a.min, Some(80.0));
        let a = parse_args(&["--size".into(), "320x180".into(), "--seconds".into(), "1".into(), "--list".into()]).unwrap();
        assert_eq!((a.options.settings.width, a.options.settings.height, a.options.settings.seconds), (320, 180, 1));
        assert!(a.list);
        for bad in
            [vec!["--min", "101"], vec!["--min", "x"], vec!["--out"], vec!["--size", "7x5"], vec!["--size", "640"], vec!["--seconds", "0"], vec!["--bogus"]]
        {
            let v: Vec<String> = bad.iter().map(|s| s.to_string()).collect();
            assert!(parse_args(&v).is_err(), "{bad:?}");
        }
        assert!(list().contains("gaussian_blur") && list().contains("manual"));
    }
}
