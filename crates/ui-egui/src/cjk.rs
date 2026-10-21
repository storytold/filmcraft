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

// Hangul. The Japanese craft faces do not cover it. Noto Sans CJK (the Hans/Hant face) does, and
// egui draws a code point from the first face that has it, so that pan-CJK Hangul was what the
// Korean UI used. Dedicated Korean faces come first instead: Malgun Gothic on Windows, Apple SD
// Gothic Neo on macOS, then the shared OFL faces (Noto Sans KR, Nanum Gothic). The pan-CJK faces
// stay last, for a machine with nothing else (artcraft#2093).
const KOREAN_SAMPLE: &str = "한국어입력테스트";
const KOREAN_PREFERRED: &[&str] =
    &["Malgun Gothic", "Apple SD Gothic Neo", "Noto Sans KR", "NanumGothic", "Nanum Gothic", "Noto Sans CJK KR", "Source Han Sans K"];

fn system_font() -> Option<Arc<FontData>> {
    static FONT: OnceLock<Option<Arc<FontData>>> = OnceLock::new();
    FONT.get_or_init(|| find_system_font(SAMPLE, PREFERRED)).clone()
}

fn korean_font() -> Option<Arc<FontData>> {
    static FONT: OnceLock<Option<Arc<FontData>>> = OnceLock::new();
    FONT.get_or_init(|| find_system_font(KOREAN_SAMPLE, KOREAN_PREFERRED)).clone()
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
    Some(Arc::new(FontData { font: std::borrow::Cow::Owned(face.data()?), index: face.info.index, tweak: Default::default() }))
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

/// Korean and Chinese faces. Japanese craft faces are already in the stack (and do not cover
/// Hangul), so kana and kanji keep Japanese forms.
///
/// `korean_first` is the Korean interface: there Hangul must come from the Korean face because the
/// pan-CJK Chinese face also covers it. In every other interface language Chinese stays ahead so
/// shared Hanzi in labels and media names keep their Chinese glyph forms; the Korean face is last
/// and only supplies Hangul.
pub(crate) fn install(fonts: &mut FontDefinitions, korean_first: bool) {
    if korean_first {
        install_korean(fonts);
        install_chinese(fonts);
    } else {
        install_chinese(fonts);
        install_korean(fonts);
    }
}

fn install_korean(fonts: &mut FontDefinitions) {
    let Some(font) = korean_font() else { return };
    fonts.font_data.insert("system-korean".into(), font);
    for stack in fonts.families.values_mut() {
        stack.push("system-korean".into());
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
