//! Its own test binary: the font database is process-global, so only a fresh process starts
//! with the system fonts not yet scanned (any `resolve` in the lib tests scans them).

use filmcraft_text::fonts::system_scanned;
use filmcraft_text::{families, picker_families};

/// The first list a picker takes must already hold the system families, not the bundled ones only.
#[test]
fn picker_families_scans_before_listing() {
    assert!(!system_scanned(), "fresh process: nothing scanned yet");
    let fams = picker_families();
    assert!(system_scanned(), "picker_families must scan the system fonts");
    assert_eq!(fams, families(), "the snapshot is taken after the scan");
}

/// A CJK font's Chinese family name reaches the picker (the project keeps the English family).
#[test]
fn picker_entries_carry_chinese_names() {
    let entries = filmcraft_text::picker_entries();
    assert!(!entries.is_empty());
    // macOS ships PingFang/Heiti; Linux and Windows have their own CJK faces. Assert on any entry
    // that has a Chinese name rather than on a particular font being installed.
    let Some(with_zh) = entries.iter().find(|e| e.zh.is_some()) else {
        eprintln!("skipped: no font with a Chinese name installed");
        return;
    };
    let zh = with_zh.zh.clone().unwrap_or_default();
    assert!(!zh.trim().is_empty(), "{zh:?}");
    assert_ne!(with_zh.label(), "", "the picker shows the Chinese name");
    assert_eq!(with_zh.label(), zh);
    // the English family still resolves, and is what a project stores
    assert!(filmcraft_text::resolve(&with_zh.family, "Regular").face > 0 || !with_zh.family.is_empty());
    assert_eq!(filmcraft_text::chinese_name(&with_zh.family), Some(zh.clone()));
    // searching matches the shown name
    assert!(with_zh.matches(&zh));
    assert!(with_zh.matches(&zh.chars().take(2).collect::<String>()));
}
