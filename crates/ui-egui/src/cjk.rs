//! CJK fallback fonts for document text and script-appropriate interface text.
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

/// Append Chinese faces after the Latin UI fonts and the Japanese fallbacks. This keeps the
/// interface's Latin typography and Japanese glyph forms, and covers the simplified and
/// traditional hanzi in media and track names that Japanese fonts lack.
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

/// Representative UI glyphs, including characters that distinguish the two Chinese scripts.
pub(crate) fn interface_sample(language: crate::i18n::Language) -> Option<&'static str> {
    use crate::i18n::Language;
    match language {
        Language::ZhTw => Some("繁體中文臺灣檔案編輯設定軌道匯入匯出"),
        _ => None,
    }
}

fn preferred_interface_families(language: crate::i18n::Language) -> &'static [&'static str] {
    use crate::i18n::Language;
    match language {
        Language::ZhTw => &["Noto Sans CJK TC", "Microsoft JhengHei UI", "Microsoft JhengHei", "PingFang TC", "Noto Sans TC"],
        _ => &[],
    }
}

/// Cache one owned font per script; collection face indices are retained on Windows.
fn interface_font(language: crate::i18n::Language, bold: bool) -> Option<Arc<FontData>> {
    use crate::i18n::Language;
    static FONTS: [OnceLock<Option<Arc<FontData>>>; 2] = [const { OnceLock::new() }; 2];
    let script = match language {
        Language::ZhTw => 0,
        _ => return None,
    };
    let cache = FONTS.get(script * 2 + usize::from(bold))?;
    cache
        .get_or_init(|| {
            let sample = interface_sample(language)?;
            let choose = |origin: &str| {
                let faces: Vec<_> = filmcraft_text::fonts::all_faces()
                    .into_iter()
                    .filter(|face| face.info.origin == origin && !face.info.italic && face.covers_text(sample))
                    .collect();
                let families = preferred_interface_families(language);
                faces.into_iter().min_by_key(|face| {
                    (
                        families.iter().position(|family| face.info.family.eq_ignore_ascii_case(family)).unwrap_or(families.len()),
                        face.info.weight.abs_diff(if bold { 700 } else { 400 }),
                    )
                })
            };
            let craft = choose(filmcraft_text::fonts::CRAFT_ORIGIN);
            let preferred = |face: &Arc<filmcraft_text::fonts::Face>| {
                preferred_interface_families(language).iter().any(|family| face.info.family.eq_ignore_ascii_case(family))
            };
            let face = craft.as_ref().filter(|face| preferred(face)).cloned().or_else(|| {
                if cfg!(target_arch = "wasm32") {
                    return craft;
                }
                filmcraft_text::fonts::scan_system();
                let system = choose("system");
                system.as_ref().filter(|face| preferred(face)).cloned().or(craft).or(system)
            })?;
            Some(Arc::new(FontData { font: std::borrow::Cow::Owned(face.data()?), index: face.info.index, tweak: Default::default() }))
        })
        .clone()
}

/// Keep the Latin faces first, then this language's CJK face ahead of other script fallbacks.
/// Traditional Chinese must not rely on a Japanese-only font.
/// Other interface languages keep the existing Chinese fallback for the language menu label;
/// resolve and read a Taiwan face only when Traditional Chinese is selected.
pub(crate) fn install_interface(fonts: &mut FontDefinitions, language: crate::i18n::Language) -> bool {
    if interface_sample(language).is_none() {
        return true;
    }
    let Some(font) = interface_font(language, false) else {
        return false;
    };
    let name = format!("interface-{}", language.code());
    fonts.font_data.insert(name.clone(), font);
    for (family, stack) in &mut fonts.families {
        let selected = if matches!(family, FontFamily::Name(n) if matches!(n.as_ref(), "semibold" | "medium")) {
            if let Some(font) = interface_font(language, true) {
                let bold_name = format!("{name}-bold");
                fonts.font_data.insert(bold_name.clone(), font);
                bold_name
            } else {
                name.clone()
            }
        } else {
            name.clone()
        };
        stack.retain(|entry| entry != &selected);
        let at = stack
            .iter()
            .position(|entry| entry.starts_with("craft:") || entry.starts_with("system-") || entry.starts_with("interface-"))
            .unwrap_or(stack.len());
        stack.insert(at, selected);
    }
    true
}
