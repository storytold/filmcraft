//! Chinese fallback fonts for document text in the interface, independent of UI language.
//! Optional craft-fonts are preferred by default. Native builds without them reuse an installed face.
//! Windows Simplified Chinese UI overrides the CJK order through [`prefer_system`].

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

/// Whether [`install`] has a Chinese face to add: craft-fonts Hans/Hant faces, or an installed
/// system face (scanned once per process). The Simplified Chinese interface needs one.
pub(crate) fn available() -> bool {
    craft_fonts().next().is_some() || system_font().is_some()
}

/// Windows Simplified Chinese UI: prefer the installed Chinese face (YaHei UI first) after Latin faces,
/// ahead of embedded Japanese and Chinese faces, including Hans/Hant faces when supplied.
/// Called before RTL font installation, so the `craft:` entries searched here are CJK faces.
/// Existing CJK faces remain available for glyphs missing from the system face.
pub(crate) fn prefer_system(fonts: &mut FontDefinitions) {
    let Some(font) = system_font() else { return };
    fonts.font_data.insert("system-chinese".into(), font);
    for stack in fonts.families.values_mut() {
        stack.retain(|name| name != "system-chinese");
        let at =
            stack.iter().position(|name| name.starts_with("craft:") || matches!(name.as_str(), "system-japanese" | "system-korean")).unwrap_or(stack.len());
        stack.insert(at, "system-chinese".into());
    }
}

/// Append Chinese faces after Latin UI fonts and embedded Japanese faces, covering hanzi the
/// Japanese faces lack. [`prefer_system`] later overrides this order for Windows Simplified Chinese UI.
/// Without craft-fonts, the system Japanese face is installed separately by i18n.
pub(crate) fn install(fonts: &mut FontDefinitions) {
    install_chinese(fonts);
    if let Some(font) = korean_font() {
        fonts.font_data.insert("system-korean".into(), font);
        for stack in fonts.families.values_mut() {
            stack.push("system-korean".into());
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
