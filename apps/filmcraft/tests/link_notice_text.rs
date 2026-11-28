//! The hand-off banner text exists in English and in every shipped catalog, and it names both apps.

use filmcraft_link::notice::{CONTINUE_LABEL, CONTINUE_TEXT, EFFECTS_LABEL, UNDO_LABEL};
use filmcraft_link::ui::{Brand, brand_color, brand_segments};
use filmcraft_ui_egui::i18n::{Language, set_current, t};

#[test]
fn the_banner_says_what_the_user_asked_for() {
    assert_eq!(CONTINUE_TEXT, "Click Continue to nest your clip. This keeps the effect in FilmCraft, but you will not see it applied in EffectCraft.");
}

#[test]
fn the_effects_label_is_exactly_what_the_user_asked_for() {
    assert_eq!(EFFECTS_LABEL, "Unimportable FilmCraft effects:");
}

#[test]
fn the_banner_strings_have_es_zh_ja_fr_entries() {
    for lang in [Language::Es, Language::ZhCn, Language::Ja, Language::Fr] {
        set_current(lang);
        for key in [CONTINUE_TEXT, CONTINUE_LABEL, UNDO_LABEL, EFFECTS_LABEL] {
            assert_ne!(t(key), key, "{lang:?} has no translation of {key:?}");
        }
        for brand in ["FilmCraft", "EffectCraft"] {
            assert!(t(CONTINUE_TEXT).contains(brand), "{lang:?} lost the app name {brand}");
        }
    }
    set_current(Language::En);
}

#[test]
fn app_names_are_split_out_exactly_as_written() {
    let names: Vec<_> = brand_segments(CONTINUE_TEXT).into_iter().filter_map(|(text, brand)| brand.map(|b| (text, b))).collect();
    assert_eq!(names, [("FilmCraft", Brand::FilmCraft), ("EffectCraft", Brand::EffectCraft)]);
}

/// Relative luminance (sRGB), for the colour-blind check.
fn luminance(c: egui::Color32) -> f32 {
    let lin = |v: u8| {
        let v = f32::from(v) / 255.0;
        if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b())
}

#[test]
fn the_two_brand_colours_differ_on_blue_and_in_brightness() {
    for dark in [true, false] {
        let (film, effect) = (brand_color(Brand::FilmCraft, dark), brand_color(Brand::EffectCraft, dark));
        assert!(film.b() > film.r() && effect.r() > effect.b(), "FilmCraft must lean blue and EffectCraft orange (dark: {dark})");
        let (a, b) = (luminance(film), luminance(effect));
        assert!((a.max(b) + 0.05) / (a.min(b) + 0.05) > 1.2, "the two names need a brightness gap, dark: {dark}");
    }
}
