//! Chinese fallback fonts for document text in the interface, independent of UI language.
//! Optional craft-fonts are preferred; native builds without them reuse one installed face.

use std::sync::{Arc, OnceLock};

use egui::{FontData, FontDefinitions, FontFamily};

const SAMPLE: &str = "中文轨道配乐音效简体汉语繁體漢語车";
const PREFERRED: &[&str] = &[
    "Microsoft YaHei UI",
    "Microsoft YaHei",
    "PingFang SC",
    "Noto Sans CJK SC",
    "Noto Sans SC",
    "Source Han Sans SC",
    "Source Han Sans CN",
    "WenQuanYi Micro Hei",
    "Microsoft JhengHei UI",
    "Microsoft JhengHei",
];

pub(crate) fn craft_fonts() -> impl Iterator<Item = &'static filmcraft_text::fonts::CraftFont> {
    filmcraft_text::fonts::CRAFT_FONTS.iter().filter(|f| f.scripts.iter().any(|s| matches!(*s, "Hans" | "Hant")))
}

// Hangul: neither the UI fonts nor the Chinese/Japanese fallbacks have it, so typed, pasted and
// IME-composed Korean drew as missing-glyph boxes (artcraft#2093).
const KOREAN_SAMPLE: &str = "한국어입력테스트";
const KOREAN_PREFERRED: &[&str] = &["Malgun Gothic", "Apple SD Gothic Neo", "Noto Sans CJK KR", "Noto Sans KR", "Source Han Sans K", "NanumGothic"];

// Thai: neither the UI fonts nor the Chinese/Japanese fallbacks have it, so Thai file and clip names
// drew as boxes (#3374).
const THAI_SAMPLE: &str = "ภาพถ่ายภาษาไทย";
const THAI_PREFERRED: &[&str] = &["Noto Sans Thai", "Thonburi", "Leelawadee UI", "Leelawadee", "Tahoma", "Sarabun", "Noto Sans Thai UI"];

fn system_font() -> Option<Arc<FontData>> {
    static FONT: OnceLock<Option<Arc<FontData>>> = OnceLock::new();
    FONT.get_or_init(|| find_system_font(SAMPLE, PREFERRED)).clone()
}

/// The font-data name of the installed Hangul face [`install`] appends to every family.
pub(crate) const KOREAN_FACE: &str = "system-korean";

fn korean_font() -> Option<Arc<FontData>> {
    static FONT: OnceLock<Option<Arc<FontData>>> = OnceLock::new();
    FONT.get_or_init(|| find_system_font(KOREAN_SAMPLE, KOREAN_PREFERRED)).clone()
}

/// The font-data name of the installed Thai face [`install`] appends to every family when
/// craft-fonts has none.
pub(crate) const THAI_FACE: &str = "system-thai";

pub(crate) fn thai_craft_fonts() -> impl Iterator<Item = &'static filmcraft_text::fonts::CraftFont> {
    filmcraft_text::fonts::CRAFT_FONTS.iter().filter(|f| f.scripts.contains(&"Thai"))
}

fn thai_font() -> Option<Arc<FontData>> {
    static FONT: OnceLock<Option<Arc<FontData>>> = OnceLock::new();
    FONT.get_or_init(|| find_system_font(THAI_SAMPLE, THAI_PREFERRED)).clone()
}

/// Whether [`install`] has a Thai face to add: a craft-fonts one, or an installed system face.
#[cfg(test)]
pub(crate) fn thai_available() -> bool {
    thai_craft_fonts().next().is_some() || thai_font().is_some()
}

/// A font collection's face index matters: Windows CJK UI fonts commonly live in TTC files.
/// Keep the owned bytes behind an Arc so theme reinstalls do not reread or leak a font file.
fn find_system_font(sample: &str, preferred: &[&str]) -> Option<Arc<FontData>> {
    if cfg!(target_arch = "wasm32") {
        return None;
    }
    filmcraft_text::fonts::scan_system();
    let faces: Vec<_> = filmcraft_text::fonts::all_faces().into_iter().filter(|f| f.info.origin == "system" && !f.info.italic).collect();
    let covers = |f: &filmcraft_text::fonts::Face| f.covers_text(sample);
    let by_weight = |f: &&Arc<filmcraft_text::fonts::Face>| f.info.weight.abs_diff(400);
    let found = preferred.iter().find_map(|name| faces.iter().filter(|f| f.info.family.eq_ignore_ascii_case(name) && covers(f)).min_by_key(by_weight));
    let face = found.or_else(|| faces.iter().filter(|f| covers(f)).min_by_key(by_weight))?;
    Some(Arc::new(FontData::from_blob(Arc::new(face.data()?), face.info.index)))
}

/// Whether [`install`] has a Hangul face to add (an installed system face; none is bundled).
/// The Korean interface needs one.
pub(crate) fn korean_available() -> bool {
    korean_font().is_some()
}

/// Whether [`install`] has a Chinese face to add: craft-fonts Hans/Hant faces, or an installed
/// system face (scanned once per process). The Simplified Chinese interface needs one.
pub(crate) fn available() -> bool {
    craft_fonts().next().is_some() || system_font().is_some()
}

/// Append Chinese faces after the Latin UI fonts and the Japanese fallbacks. This keeps the
/// interface's Latin typography and Japanese glyph forms, and covers the simplified and
/// traditional hanzi in media and track names that Japanese fonts lack.
pub(crate) fn install(fonts: &mut FontDefinitions) {
    install_chinese(fonts);
    if let Some(font) = korean_font() {
        fonts.font_data.insert(KOREAN_FACE.into(), font);
        for stack in fonts.families.values_mut() {
            stack.push(KOREAN_FACE.into());
        }
    }
    install_thai(fonts);
}

/// Thai letters are in no other UI font, so the faces go last; craft-fonts first, else one
/// installed face.
fn install_thai(fonts: &mut FontDefinitions) {
    let mut names = Vec::new();
    for f in thai_craft_fonts() {
        let name = format!("craft:{} {}", f.family, f.style);
        fonts.font_data.insert(name.clone(), Arc::new(FontData::from_static(f.bytes)));
        names.push(name);
    }
    if names.is_empty()
        && let Some(font) = thai_font()
    {
        fonts.font_data.insert(THAI_FACE.into(), font);
        names.push(THAI_FACE.into());
    }
    for stack in fonts.families.values_mut() {
        for name in &names {
            if !stack.contains(name) {
                stack.push(name.clone());
            }
        }
    }
}

fn install_chinese(fonts: &mut FontDefinitions) {
    let chinese: Vec<_> = craft_fonts().collect();
    if chinese.is_empty() {
        if let Some(font) = system_font() {
            fonts.font_data.insert("system-chinese".into(), font);
            for stack in fonts.families.values_mut() {
                stack.push("system-chinese".into());
            }
        }
        return;
    }
    let name = |f: &filmcraft_text::fonts::CraftFont| format!("craft:{} {}", f.family, f.style);
    for f in &chinese {
        fonts.font_data.insert(name(f), Arc::new(FontData::from_static(f.bytes)));
    }
    for (family, stack) in &mut fonts.families {
        let heavy = matches!(family, FontFamily::Name(n) if matches!(n.as_ref(), "semibold" | "medium"));
        let mut order = chinese.clone();
        order.sort_by_key(|f| (f.family.contains("Serif") && !f.family.contains("Sans"), (f.style == "Bold") != heavy));
        stack.extend(order.iter().map(|f| name(f)));
    }
}

#[cfg(test)]
mod font_blob_tests {
    use super::*;

    /// `FontData::from_blob` retains the TTC face index when constructed from a shared blob.
    #[test]
    fn a_ttc_face_index_survives_font_data_construction() {
        let font = FontData::from_blob(Arc::new(vec![0u8; 4]), 7);
        assert_eq!(font.index, 7, "the face index must travel with the shared blob");
    }

    /// Thai file and clip names need a Thai face in every UI font family (#3374): craft-fonts' Noto
    /// Sans Thai when built with it, else an installed one. Skipped on a machine with neither.
    #[test]
    fn install_adds_a_thai_face_to_every_family() {
        if !thai_available() {
            eprintln!("skipped: no craft-fonts Thai face and no installed Thai font");
            return;
        }
        let mut fonts = FontDefinitions::default();
        install(&mut fonts);
        for (family, stack) in &fonts.families {
            let thai = stack.iter().filter(|n| n.starts_with("craft:Noto Sans Thai") || n.as_str() == THAI_FACE).count();
            assert_eq!(thai, 1, "{family:?}: {stack:?}");
        }
    }
}
