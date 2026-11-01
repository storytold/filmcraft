//! Auto-save file naming and rotation (Premiere behaviour).
//!
//! Auto-saves go to an `Auto-Save` folder next to the project, named
//! `<project name>-YYYY-MM-DD_HH-MM-SS-<unix seconds>.fcproj` (local wall-clock time, then the real
//! time that orders the files). If multiple saves have the same timestamp, `-collision-N` is appended.
//! After each write the oldest files of that project beyond the "Maximum Project Versions" limit are
//! deleted. Only files whose name is exactly `<name>-<timestamp>.fcproj`,
//! `<name>-<timestamp>-<unix seconds>.fcproj` or `<name>-<timestamp>-<unix seconds>-collision-N.fcproj`
//! are ever touched.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The folder name used next to a project.
pub const AUTO_SAVE_DIR: &str = "Auto-Save";

/// `Auto-Save` folder for a project file.
pub fn auto_save_dir(project_path: &Path) -> PathBuf {
    project_path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new(".")).join(AUTO_SAVE_DIR)
}

/// A file-name-safe version of a project name (path separators and control characters replaced).
pub fn file_stem(name: &str) -> String {
    let s: String = name.chars().map(|c| if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() { '_' } else { c }).collect();
    let s = s.trim().trim_matches('.').to_string();
    if s.is_empty() { "Untitled".into() } else { s }
}

/// Unix seconds → (proleptic Gregorian year, the same wall-clock instant moved into chrono's range
/// by whole 400-year cycles). The Gregorian calendar repeats every 400 years (146_097 days), so
/// month, day and time of day come from chrono and only the year is carried, which keeps every
/// `i64` exact instead of collapsing out-of-range years to the epoch.
fn utc_parts(secs: i64) -> (i64, chrono::NaiveDateTime) {
    use chrono::Datelike;
    let (days, sod) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (cycles, rest) = (days.div_euclid(146_097), days.rem_euclid(146_097));
    // `rest + 719_163` (1970-01-01 is CE day 719_163) is always a valid CE day, and `sod` < 86_400.
    let date = i32::try_from(rest + 719_163).ok().and_then(chrono::NaiveDate::from_num_days_from_ce_opt);
    let time = u32::try_from(sod).ok().and_then(|s| chrono::NaiveTime::from_num_seconds_from_midnight_opt(s, 0));
    match (date, time) {
        (Some(d), Some(t)) => {
            let y = i128::from(d.year()) + i128::from(cycles) * 400;
            (i64::try_from(y).unwrap_or(if y < 0 { i64::MIN } else { i64::MAX }), d.and_time(t))
        }
        _ => (1970, chrono::NaiveDateTime::default()), // unreachable
    }
}

/// Civil date-time (proleptic Gregorian) of a unix timestamp shifted by `offset_secs`.
/// Years far outside chrono's own range are preserved (the old hand-rolled conversion kept them);
/// only an overflowing `unix + offset_secs` shift (which the old code wrapped to garbage) reads as
/// the epoch.
pub fn civil(unix: i64, offset_secs: i32) -> (i64, u32, u32, u32, u32, u32) {
    use chrono::{Datelike, Timelike};
    let Some(s) = unix.checked_add(offset_secs as i64) else {
        return (1970, 1, 1, 0, 0, 0);
    };
    let (y, t) = utc_parts(s);
    (y, t.month(), t.day(), t.hour(), t.minute(), t.second())
}

/// `YYYY-MM-DD_HH-MM-SS` (the auto-save file-name stamp).
pub fn timestamp(unix: i64, offset_secs: i32) -> String {
    let (y, mo, d, h, mi, s) = civil(unix, offset_secs);
    format!("{y:04}-{mo:02}-{d:02}_{h:02}-{mi:02}-{s:02}")
}

/// `YYYY-MM-DD HH:MM:SS` for display.
pub fn display_time(unix: i64, offset_secs: i32) -> String {
    let (y, mo, d, h, mi, s) = civil(unix, offset_secs);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}")
}

/// `<stem>-YYYY-MM-DD_HH-MM-SS-<unix seconds>.fcproj`. The local stamp is for people; the unix time
/// orders the files (the local clock can go back, or repeat an hour, when daylight saving time ends).
pub fn auto_save_name(name: &str, unix: i64, offset_secs: i32) -> String {
    format!("{}-{}-{unix}.fcproj", file_stem(name), timestamp(unix, offset_secs))
}

fn is_stamp(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 19
        && b.iter().enumerate().all(|(i, c)| match i {
            4 | 7 | 13 | 16 => *c == b'-',
            10 => *c == b'_',
            _ => c.is_ascii_digit(),
        })
}

/// Parse an auto-save name into its wall-clock stamp, unix time and collision number.
fn parse_stamp(s: &str) -> Option<(&str, Option<i64>, Option<u64>)> {
    if is_stamp(s) {
        return Some((s, None, None));
    }
    let stamp = s.get(..19).filter(|st| is_stamp(st))?;
    let rest = s.get(19..)?.strip_prefix('-')?;
    let (unix_digits, collision) = match rest.split_once("-collision-") {
        Some((unix, collision)) => (unix, Some(collision)),
        None => (rest, None),
    };
    let unix: i64 = unix_digits.parse().ok()?;
    if unix.to_string() != unix_digits {
        return None;
    }
    let collision = match collision {
        Some(digits) => {
            let n: u64 = digits.parse().ok()?;
            if n.to_string() != digits {
                return None;
            }
            Some(n)
        }
        None => None,
    };
    Some((stamp, Some(unix), collision))
}

/// Auto-saves of project `name` in `dir`, oldest first (by real time, not by the local clock; files
/// without a unix time in their name are older than those with one).
pub fn list_auto_saves(dir: &Path, name: &str) -> Vec<PathBuf> {
    let prefix = format!("{}-", file_stem(name));
    let Ok(rd) = fs::read_dir(dir) else { return Vec::new() };
    let mut v: Vec<(Option<i64>, String, Option<u64>, PathBuf)> = rd
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let f = e.file_name().to_string_lossy().into_owned();
            let (stamp, unix, collision) = parse_stamp(f.strip_prefix(&prefix)?.strip_suffix(".fcproj")?)?;
            Some((unix, stamp.to_string(), collision, e.path()))
        })
        .collect();
    v.sort();
    v.into_iter().map(|(_, _, _, p)| p).collect()
}

/// Delete the oldest auto-saves of `name` so at most `max` remain. Returns the deleted paths.
pub fn rotate(dir: &Path, name: &str, max: usize) -> io::Result<Vec<PathBuf>> {
    let all = list_auto_saves(dir, name);
    let n = all.len().saturating_sub(max.max(1));
    let mut gone = Vec::new();
    for p in all.into_iter().take(n) {
        fs::remove_file(&p)?;
        gone.push(p);
    }
    Ok(gone)
}

/// Write one auto-save (atomically) into `dir` and rotate. Returns the new file.
pub fn write_auto_save(dir: &Path, name: &str, unix: i64, offset_secs: i32, data: &[u8], max: usize) -> io::Result<PathBuf> {
    fs::create_dir_all(dir)?;
    let base = auto_save_name(name, unix, offset_secs);
    let base_stem = base.trim_end_matches(".fcproj");
    let collision_prefix = format!("{base_stem}-collision-");
    let mut collision = 1_u64;
    let mut path = dir.join(&base);
    let has_collisions = fs::read_dir(dir)?.filter_map(|entry| entry.ok()).any(|entry| {
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else { return false };
        let Some(digits) = file_name.strip_prefix(&collision_prefix).and_then(|s| s.strip_suffix(".fcproj")) else {
            return false;
        };
        let Ok(n) = digits.parse::<u64>() else { return false };
        n.to_string() == digits
    });
    if !path.exists() && has_collisions {
        loop {
            let candidate = dir.join(format!("{base_stem}-collision-{collision}.fcproj"));
            if !candidate.exists() {
                path = candidate;
                break;
            }
            collision = collision.checked_add(1).ok_or_else(|| io::Error::other("auto-save name exhausted"))?;
        }
    }
    loop {
        match crate::atomic_write_new(&path, data) {
            Ok(()) => break,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                path = dir.join(format!("{base_stem}-collision-{collision}.fcproj"));
                collision = collision.checked_add(1).ok_or_else(|| io::Error::other("auto-save name exhausted"))?;
            }
            Err(e) => return Err(e),
        }
    }
    rotate(dir, name, max)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates() {
        assert_eq!(timestamp(0, 0), "1970-01-01_00-00-00");
        // 2026-09-30 13:45:07 UTC
        assert_eq!(timestamp(1_790_775_907, 0), "2026-09-30_13-45-07");
        assert_eq!(timestamp(1_790_775_907, 2 * 3600), "2026-09-30_15-45-07");
        assert_eq!(timestamp(1_790_775_907, -14 * 3600), "2026-09-29_23-45-07");
        assert_eq!(display_time(951_782_400, 0), "2000-02-29 00:00:00");
        assert_eq!(timestamp(-1, 0), "1969-12-31_23-59-59");
    }

    #[test]
    fn out_of_range_years_are_preserved_not_collapsed_to_the_epoch() {
        // The old hand-rolled conversion kept years far outside chrono's own range; the refactor
        // must too (reviewer FredApps). `10_000_000_000_000` s → 318857, not 1970.
        assert_eq!(timestamp(10_000_000_000_000, 0), "318857-05-20_17-46-40");
        assert_eq!(display_time(10_000_000_000_000, 0), "318857-05-20 17:46:40");
        // chrono's own upper edge (year 10000) and a negative value far before it.
        assert_eq!(timestamp(253_402_300_800, 0), "10000-01-01_00-00-00");
        assert_eq!(display_time(-10_000_000_000_000, 0), "-314918-08-13 06:13:20");
        // The input extremes the old code accepted (offset 0, so no shift overflow).
        assert_eq!(display_time(i64::MAX, 0), "292277026596-12-04 15:30:07");
        assert_eq!(display_time(i64::MIN, 0), "-292277022657-01-27 08:29:52");
        // Only an overflowing `unix + offset_secs` shift (old code wrapped it to garbage) reads as
        // the epoch; the separate overflow guard is kept.
        assert_eq!(civil(i64::MAX, 3600), (1970, 1, 1, 0, 0, 0));
        assert_eq!(civil(i64::MIN, -3600), (1970, 1, 1, 0, 0, 0));
    }

    #[test]
    fn names_and_stems() {
        assert_eq!(auto_save_name("My Film", 0, 0), "My Film-1970-01-01_00-00-00-0.fcproj");
        assert_eq!(file_stem("a/b:c"), "a_b_c");
        assert_eq!(file_stem("  "), "Untitled");
        assert_eq!(auto_save_dir(Path::new("/x/y/p.fcproj")), Path::new("/x/y/Auto-Save"));
    }

    #[test]
    fn rotation_survives_the_clock_going_back_at_the_end_of_dst() {
        let d = std::env::temp_dir().join(format!("filmcraft-rotate-dst-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        let t0 = 1_792_900_000;
        // 02:30 at +02:00, then 40 minutes later the clock reads 02:10 at +01:00.
        let old = write_auto_save(&d, "Cut", t0, 7200, b"old", 1).unwrap();
        let new = write_auto_save(&d, "Cut", t0 + 2400, 3600, b"new", 1).unwrap();
        assert!(new.exists(), "the newest auto-save was deleted by rotation");
        assert!(!old.exists(), "the oldest auto-save should be rotated out");
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn repeated_wall_clock_hour_does_not_overwrite_earlier_saves() {
        let d = std::env::temp_dir().join(format!("filmcraft-rotate-dst2-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        let t0 = 1_792_900_000;
        // The same wall-clock second, one hour apart (+02:00 then +01:00).
        let a = write_auto_save(&d, "Cut", t0, 7200, b"first", 5).unwrap();
        let b = write_auto_save(&d, "Cut", t0 + 3600, 3600, b"second", 5).unwrap();
        assert_ne!(a, b);
        assert_eq!(fs::read(&a).unwrap(), b"first");
        assert_eq!(fs::read(&b).unwrap(), b"second");
        assert_eq!(list_auto_saves(&d, "Cut"), vec![a, b]);
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn same_second_preserves_both_saves() {
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let d = std::env::temp_dir().join(format!("filmcraft-same-second-{}-{nonce}", std::process::id()));
        let timestamp = 1_790_000_000;
        let first = write_auto_save(&d, "Cut", timestamp, 0, b"first", 5).unwrap();
        let second = write_auto_save(&d, "Cut", timestamp, 0, b"second", 5).unwrap();
        assert_ne!(first, second);
        assert_eq!(fs::read(&first).unwrap(), b"first");
        assert_eq!(fs::read(&second).unwrap(), b"second");
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn rotation_uses_next_collision_after_base_is_removed() {
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let d = std::env::temp_dir().join(format!("filmcraft-rotate-collision-{}-{nonce}", std::process::id()));
        let unix = 1_790_000_000;
        let base = write_auto_save(&d, "Cut", unix, 0, b"base", 1).unwrap();
        let first_collision = write_auto_save(&d, "Cut", unix, 0, b"first collision", 1).unwrap();
        assert!(!base.exists());
        assert!(first_collision.exists());
        let second_collision = write_auto_save(&d, "Cut", unix, 0, b"second collision", 1).unwrap();
        assert!(second_collision.ends_with(format!("Cut-{}-{unix}-collision-2.fcproj", timestamp(unix, 0))));
        assert!(second_collision.exists());
        assert_eq!(fs::read(&second_collision).unwrap(), b"second collision");
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn noncanonical_collision_suffix_is_not_rotated() {
        let d = std::env::temp_dir().join(format!("filmcraft-rotate-invalid-collision-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        let invalid = d.join("Cut-2026-09-18_22-13-20-1790000000-collision-01.fcproj");
        fs::write(&invalid, b"keep").unwrap();
        assert!(list_auto_saves(&d, "Cut").is_empty());
        rotate(&d, "Cut", 1).unwrap();
        assert!(invalid.exists());
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn rotation_keeps_newest_and_ignores_other_files() {
        let d = std::env::temp_dir().join(format!("filmcraft-rotate-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        for i in 0..7 {
            write_auto_save(&d, "Cut", 1_790_000_000 + i * 60, 0, b"{}", 5).unwrap();
        }
        // Another project whose name starts with the same prefix, and an unrelated file.
        fs::write(d.join("Cut-Two-2026-01-01_00-00-00.fcproj"), b"{}").unwrap();
        fs::write(d.join("notes.txt"), b"keep").unwrap();
        let left = list_auto_saves(&d, "Cut");
        assert_eq!(left.len(), 5);
        assert!(left[0].to_string_lossy().ends_with(&format!("Cut-{}-1790000120.fcproj", timestamp(1_790_000_120, 0))));
        assert!(left[4].to_string_lossy().ends_with(&format!("Cut-{}-1790000360.fcproj", timestamp(1_790_000_360, 0))));
        assert_eq!(list_auto_saves(&d, "Cut-Two").len(), 1);
        assert!(d.join("notes.txt").exists());
        rotate(&d, "Cut", 2).unwrap();
        assert_eq!(list_auto_saves(&d, "Cut").len(), 2);
        assert_eq!(list_auto_saves(&d, "Cut-Two").len(), 1);
        fs::remove_dir_all(&d).unwrap();
    }
}
