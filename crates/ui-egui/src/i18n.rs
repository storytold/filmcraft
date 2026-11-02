//! Interface translations. Strings in code stay English and are the lookup keys; one catalog per
//! language (`i18n/<code>.tsv`, format in the header of `es.tsv`) maps them to display text at draw
//! time. Untranslated strings are shown in English, so coverage can grow incrementally. Command ids,
//! automation ids, preference values, the control channel, the CLI and MCP stay English (an
//! automation element's label is the text shown, so it follows the interface language).
//!
//! # Looking strings up
//! - `tl!` with a string literal: that UI string in the language the UI is drawn in (see
//!   [`set_current`]). Use it for every label, button, heading, tooltip and hint; `cargo xtask
//!   i18n-report` lists the `tl!` literals each catalog lacks.
//! - [`t`]: the same for a string that is not a literal (a name from a registry or a list).
//! - [`tr_ctx`]: when one English word needs different translations (the Edit menu, the Edit button).
//! - [`fmt`]: fill `{name}` placeholders after translating a template; translators may reorder them.
//!
//! Japanese text uses the Japanese craft-fonts when FilmCraft was built with them (`CRAFT_FONTS_DIR`;
//! `theme::install` already puts them in every font family, see [`craft_japanese_font`]), otherwise
//! a font already installed on the system ([`system_japanese_font`]); with neither, switching to
//! Japanese is refused with a message.
//!
//! Simplified Chinese uses the Chinese fallback `theme::install` always adds (craft-fonts Hans/Hant
//! faces, otherwise one installed system face, see `cjk.rs`); without one, switching to Chinese is
//! refused the same way ([`chinese_font_available`]).

mod catalog;

use std::cell::Cell;
use std::sync::{Arc, OnceLock};

use catalog::Catalog;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    #[default]
    En,
    Ja,
    Es,
    /// Persisted as `pt-br` (the blanket `rename_all` would produce `ptbr`).
    #[serde(rename = "pt-br")]
    PtBr,
    Uk,
    /// Simplified Chinese, persisted as `zh-cn`.
    #[serde(rename = "zh-cn")]
    ZhCn,
    De,
    Ru,
    Fr,
    Id,
    It,
    /// Bosnian, Latin script (Ijekavian); `bs-Cyrl` locales use it too.
    Bs,
    /// Korean; Hangul needs an installed system font ([`korean_font_available`]).
    Ko,
    /// Czech; Latin with háčky and čárky, covered by the bundled fonts.
    Cs,
    /// Polish.
    Pl,
    /// Swedish.
    Sv,
    Tr,
}

static JAPANESE: OnceLock<Catalog> = OnceLock::new();
static SPANISH: OnceLock<Catalog> = OnceLock::new();
static PORTUGUESE: OnceLock<Catalog> = OnceLock::new();
static UKRAINIAN: OnceLock<Catalog> = OnceLock::new();
static CHINESE: OnceLock<Catalog> = OnceLock::new();
static GERMAN: OnceLock<Catalog> = OnceLock::new();
static RUSSIAN: OnceLock<Catalog> = OnceLock::new();
static FRENCH: OnceLock<Catalog> = OnceLock::new();
static KOREAN: OnceLock<Catalog> = OnceLock::new();
static POLISH: OnceLock<Catalog> = OnceLock::new();
static INDONESIAN: OnceLock<Catalog> = OnceLock::new();
static ITALIAN: OnceLock<Catalog> = OnceLock::new();
static BOSNIAN: OnceLock<Catalog> = OnceLock::new();
static CZECH: OnceLock<Catalog> = OnceLock::new();
static SWEDISH: OnceLock<Catalog> = OnceLock::new();
static TURKISH: OnceLock<Catalog> = OnceLock::new();

impl Language {
    pub const ALL: [Self; 17] = [
        Self::En,
        Self::Ja,
        Self::Es,
        Self::PtBr,
        Self::Uk,
        Self::ZhCn,
        Self::De,
        Self::Ru,
        Self::Fr,
        Self::Id,
        Self::Ko,
        Self::It,
        Self::Cs,
        Self::Pl,
        Self::Sv,
        Self::Tr,
        Self::Bs,
    ];

    /// BCP 47 tag for AccessKit and the web IME's hidden input. Keep the persisted spelling
    /// (`pt-br`, `zh-cn`) out of the protocol-facing value.
    pub fn bcp47(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::Ja => "ja",
            Self::Es => "es",
            Self::PtBr => "pt-BR",
            Self::Uk => "uk",
            Self::ZhCn => "zh-CN",
            Self::De => "de",
            Self::Ru => "ru",
            Self::Fr => "fr",
            Self::Id => "id",
            Self::It => "it",
            Self::Bs => "bs",
            Self::Ko => "ko",
            Self::Cs => "cs",
            Self::Pl => "pl",
            Self::Sv => "sv",
            Self::Tr => "tr",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::En => "English",
            Self::Ja => "日本語",
            Self::Es => "Español",
            Self::PtBr => "Português (Brasil)",
            Self::Uk => "Українська",
            Self::ZhCn => "简体中文",
            Self::De => "Deutsch",
            Self::Ru => "Русский",
            Self::Fr => "Français",
            Self::Id => "Bahasa Indonesia",
            Self::It => "Italiano",
            Self::Bs => "Bosanski",
            Self::Ko => "한국어",
            Self::Cs => "Čeština",
            Self::Pl => "Polski",
            Self::Sv => "Svenska",
            Self::Tr => "Türkçe",
        }
    }

    pub fn parse(code: &str) -> Option<Self> {
        match code {
            "en" => Some(Self::En),
            "ja" => Some(Self::Ja),
            "es" => Some(Self::Es),
            "pt-br" => Some(Self::PtBr),
            "uk" => Some(Self::Uk),
            "zh-cn" => Some(Self::ZhCn),
            "de" => Some(Self::De),
            "ru" => Some(Self::Ru),
            "fr" => Some(Self::Fr),
            "id" => Some(Self::Id),
            "it" => Some(Self::It),
            "bs" => Some(Self::Bs),
            "ko" => Some(Self::Ko),
            "cs" => Some(Self::Cs),
            "pl" => Some(Self::Pl),
            "sv" => Some(Self::Sv),
            "tr" => Some(Self::Tr),
            _ => None,
        }
    }

    /// Interface Language ▸ System Language (#218): the first of the user's preferred languages
    /// (BCP 47 or POSIX locale tags such as `es-419`, `pt_BR.UTF-8`, most preferred first) that the
    /// interface has, else English. Any Portuguese gets the Brazilian catalog, the only one there is;
    /// any German (`de-AT`, `de_CH.UTF-8`…) gets the German one and any French (`fr-CA`, `fr_BE.UTF-8`…)
    /// the French one; any Indonesian (`id-ID`, `id_ID.UTF-8`, and the legacy `in-ID` that older Java
    /// and Android runtimes report) the Indonesian one; any Italian (`it-IT`, `it-CH`, `it_IT.UTF-8`…)
    /// the Italian one; any Bosnian (`bs-BA`, `bs_BA.UTF-8`, `bs-Latn-BA`…) the Bosnian one; any Swedish (`sv-SE`, `sv-FI`, `sv_SE.UTF-8`…) the Swedish one; any Turkish (`tr`, `tr-TR`, `tr_TR.UTF-8`…) the Turkish one. Chinese gets the Simplified catalog
    /// only for Simplified locales (`zh`, `zh-CN`, `zh-SG`, `zh-Hans…`); Traditional ones (`zh-TW`,
    /// `zh-HK`, `zh-MO`, `zh-Hant…`) are skipped.
    pub fn from_locales(tags: &[String]) -> Self {
        const PRIMARY: [(&str, Language); 17] = [
            ("en", Language::En),
            ("ja", Language::Ja),
            ("es", Language::Es),
            ("pt", Language::PtBr),
            ("uk", Language::Uk),
            ("de", Language::De),
            ("ru", Language::Ru),
            ("fr", Language::Fr),
            ("id", Language::Id),
            ("in", Language::Id),
            ("it", Language::It),
            ("bs", Language::Bs),
            ("ko", Language::Ko),
            ("cs", Language::Cs),
            ("pl", Language::Pl),
            ("sv", Language::Sv),
            ("tr", Language::Tr),
        ];
        tags.iter()
            .find_map(|tag| {
                let mut parts = tag.split(['-', '_', '.', '@']);
                let primary = parts.next().unwrap_or_default();
                if primary.eq_ignore_ascii_case("zh") {
                    // the subtag after `zh` (a script or a region), if any; `.UTF-8` / `@x` end it
                    let rest = tag.get(primary.len()..).unwrap_or_default();
                    let sub = if rest.starts_with(['-', '_']) { parts.next().unwrap_or_default() } else { "" };
                    let simplified = sub.is_empty() || ["cn", "sg", "hans"].iter().any(|s| sub.eq_ignore_ascii_case(s));
                    return simplified.then_some(Language::ZhCn);
                }
                PRIMARY.iter().find(|(code, _)| primary.eq_ignore_ascii_case(code)).map(|(_, l)| *l)
            })
            .unwrap_or_default()
    }

    /// Stable code: the `general.interfaceLanguage` preference value (`Language::parse` reads it back).
    pub fn code(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::Ja => "ja",
            Self::Es => "es",
            Self::PtBr => "pt-br",
            Self::Uk => "uk",
            Self::ZhCn => "zh-cn",
            Self::De => "de",
            Self::Ru => "ru",
            Self::Fr => "fr",
            Self::Id => "id",
            Self::It => "it",
            Self::Bs => "bs",
            Self::Ko => "ko",
            Self::Cs => "cs",
            Self::Pl => "pl",
            Self::Sv => "sv",
            Self::Tr => "tr",
        }
    }

    /// The language's catalog (none for English, the source language).
    fn catalog(self) -> Option<&'static Catalog> {
        match self {
            Self::En => None,
            Self::Ja => Some(JAPANESE.get_or_init(|| Catalog::parse(include_str!("i18n/ja.tsv")))),
            Self::Es => Some(SPANISH.get_or_init(|| Catalog::parse(include_str!("i18n/es.tsv")))),
            Self::PtBr => Some(PORTUGUESE.get_or_init(|| Catalog::parse(include_str!("i18n/pt-br.tsv")))),
            Self::Uk => Some(UKRAINIAN.get_or_init(|| Catalog::parse(include_str!("i18n/uk.tsv")))),
            Self::ZhCn => Some(CHINESE.get_or_init(|| Catalog::parse(include_str!("i18n/zh-cn.tsv")))),
            Self::De => Some(GERMAN.get_or_init(|| Catalog::parse(include_str!("i18n/de.tsv")))),
            Self::Ru => Some(RUSSIAN.get_or_init(|| Catalog::parse(include_str!("i18n/ru.tsv")))),
            Self::Fr => Some(FRENCH.get_or_init(|| Catalog::parse(include_str!("i18n/fr.tsv")))),
            Self::Id => Some(INDONESIAN.get_or_init(|| Catalog::parse(include_str!("i18n/id.tsv")))),
            Self::It => Some(ITALIAN.get_or_init(|| Catalog::parse(include_str!("i18n/it.tsv")))),
            Self::Bs => Some(BOSNIAN.get_or_init(|| Catalog::parse(include_str!("i18n/bs.tsv")))),
            Self::Ko => Some(KOREAN.get_or_init(|| Catalog::parse(include_str!("i18n/ko.tsv")))),
            Self::Cs => Some(CZECH.get_or_init(|| Catalog::parse(include_str!("i18n/cs.tsv")))),
            Self::Pl => Some(POLISH.get_or_init(|| Catalog::parse(include_str!("i18n/pl.tsv")))),
            Self::Sv => Some(SWEDISH.get_or_init(|| Catalog::parse(include_str!("i18n/sv.tsv")))),
            Self::Tr => Some(TURKISH.get_or_init(|| Catalog::parse(include_str!("i18n/tr.tsv")))),
        }
    }

    /// Translate an English UI string; unknown strings come back unchanged.
    pub fn tr(self, text: &str) -> &str {
        self.catalog().and_then(|c| c.plain(text)).unwrap_or(text)
    }

    /// Does this language's catalog translate `text`? (English never does: it is the source.)
    pub fn has(self, text: &str) -> bool {
        self.catalog().and_then(|c| c.plain(text)).is_some()
    }

    /// Uppercase `text`, using Turkish-aware rules (`i` → `İ`, `ı` → `I`) when the language is
    /// Turkish.
    pub fn to_uppercase(self, text: &str) -> String {
        if self == Self::Tr {
            let mapped: String = text
                .chars()
                .map(|c| match c {
                    'i' => 'İ',
                    'ı' => 'I',
                    other => other,
                })
                .collect();
            mapped.to_uppercase()
        } else {
            text.to_uppercase()
        }
    }

    /// Lowercase `text`, using Turkish-aware rules (`İ` → `i`, `I` → `ı`) when the language is
    /// Turkish. Decomposed `I` followed by `\u{0307}` is normalized to `i`.
    pub fn to_lowercase(self, text: &str) -> String {
        if self == Self::Tr {
            let normalized = text.replace("I\u{0307}", "i");
            let mapped: String = normalized
                .chars()
                .map(|c| match c {
                    'İ' => 'i',
                    'I' => 'ı',
                    other => other,
                })
                .collect();
            mapped.to_lowercase()
        } else {
            text.to_lowercase()
        }
    }
}

thread_local! {
    /// The language the UI is drawn in. Per thread: the UI is drawn on one thread, and tests that
    /// switch language don't leak it into tests running on other threads.
    static CURRENT: Cell<Language> = const { Cell::new(Language::En) };
}

/// Set the language for drawing. `FilmcraftApp::frame` calls this at the start of every frame from
/// the UI state, so widgets translate without every call site carrying the language around.
pub fn set_current(lang: Language) {
    CURRENT.with(|c| c.set(lang));
}

/// The language the UI is drawn in.
pub fn current() -> Language {
    CURRENT.with(Cell::get)
}

/// Translate an English UI string into the current language (the `tl!` macro, for literals).
pub fn t(text: &str) -> &str {
    current().tr(text)
}

/// Uppercase `text` in the current interface language (with Turkish-aware casing when the
/// language is Turkish).
pub fn to_uppercase(text: &str) -> String {
    current().to_uppercase(text)
}

/// Lowercase `text` in the current interface language (with Turkish-aware casing when the
/// language is Turkish).
pub fn to_lowercase(text: &str) -> String {
    current().to_lowercase(text)
}

fn fold_query(s: &str) -> String {
    s.replace('İ', "i").replace('\u{0307}', "").to_lowercase()
}

fn fold_turkish(s: &str) -> String {
    let normalized = s.replace("I\u{0307}", "i");
    let mapped: String = normalized
        .chars()
        .map(|c| match c {
            'İ' => 'i',
            'I' => 'ı',
            other => other,
        })
        .collect();
    mapped.replace('\u{0307}', "").to_lowercase()
}

/// Match a search query against both the source and the displayed label.
pub fn matches_query(text: &str, query: &str) -> bool {
    let q = fold_query(query);
    if fold_query(text).contains(&q) || fold_query(t(text)).contains(&q) {
        return true;
    }
    let q_tr = fold_turkish(query);
    fold_turkish(t(text)).contains(&q_tr) || fold_turkish(text).contains(&q_tr)
}

/// Like [`t`], for an English string that needs a disambiguating `context` (a catalog row whose
/// first column is that context); falls back to the context-free translation.
pub fn tr_ctx<'a>(context: &str, text: &'a str) -> &'a str {
    let lang = current();
    lang.catalog().and_then(|c| c.contextual(context, text)).unwrap_or_else(|| lang.tr(text))
}

/// Fill `{name}` placeholders. Unknown placeholders are left as written.
pub fn fmt(template: &str, args: &[(&str, &str)]) -> String {
    let mut out = String::new();
    let mut rest = template;
    while let Some((prefix, after)) = rest.split_once('{') {
        out.push_str(prefix);
        let Some((name, tail)) = after.split_once('}') else {
            out.push('{');
            out.push_str(after);
            return out;
        };
        if let Some((_, value)) = args.iter().find(|(key, _)| *key == name) {
            out.push_str(value);
        } else {
            out.push('{');
            out.push_str(name);
            out.push('}');
        }
        rest = tail;
    }
    out.push_str(rest);
    out
}

/// Text every Japanese interface font must cover (menus use kanji, hiragana and katakana).
const JAPANESE_SAMPLE: &str = "日本語ファイル編集あア";

/// Installed families preferred for Japanese interface text, best first (Gothic / sans-serif faces
/// read best at menu sizes). Any other installed face that covers `JAPANESE_SAMPLE` is used if
/// none of these is present.
const PREFERRED_JAPANESE: &[&str] = &[
    "Hiragino Sans",
    "Hiragino Kaku Gothic ProN",
    "Hiragino Kaku Gothic Pro",
    "Yu Gothic UI",
    "Yu Gothic",
    "Meiryo UI",
    "Meiryo",
    "Noto Sans CJK JP",
    "Noto Sans JP",
    "Source Han Sans JP",
    "Source Han Sans",
    "IPAexGothic",
    "IPAGothic",
    "TakaoGothic",
    "VL Gothic",
];

const JAPANESE_FONT: &str = "system-japanese";

/// A Japanese font already installed on this system, for the interface (none is bundled). Looked up
/// once per process: the system font folders are scanned on first use (name tables only), then the
/// chosen face's file is read. `None` on the web and on systems without a Japanese font.
pub fn system_japanese_font() -> Option<Arc<egui::FontData>> {
    static FONT: OnceLock<Option<Arc<egui::FontData>>> = OnceLock::new();
    FONT.get_or_init(|| {
        filmcraft_text::fonts::scan_system();
        let faces: Vec<_> = filmcraft_text::fonts::all_faces().into_iter().filter(|f| f.info.origin == "system" && !f.info.italic).collect();
        let covers = |f: &filmcraft_text::fonts::Face| f.covers_text(JAPANESE_SAMPLE);
        // within a family, the face closest to regular weight
        let by_weight = |f: &&Arc<filmcraft_text::fonts::Face>| f.info.weight.abs_diff(400);
        let preferred =
            PREFERRED_JAPANESE.iter().find_map(|name| faces.iter().filter(|f| f.info.family.eq_ignore_ascii_case(name) && covers(f)).min_by_key(by_weight));
        let face = preferred.or_else(|| faces.iter().filter(|f| covers(f)).min_by_key(by_weight))?;
        // kept for the life of the process (one font, read once) so installing it again after a
        // theme change shares the bytes instead of copying the whole file
        let bytes: &'static [u8] = Box::leak(face.data()?.into_boxed_slice());
        Some(Arc::new(egui::FontData::from_blob(Arc::new(bytes), face.info.index)))
    })
    .clone()
}

/// Whether the craft-fonts build input (empty unless built with `CRAFT_FONTS_DIR`) supplies a
/// Japanese interface font: some craft-fonts face covers `JAPANESE_SAMPLE`. `theme::install` adds
/// these faces to every font family, so nothing else needs installing (and no system scan runs).
pub fn craft_japanese_font() -> bool {
    filmcraft_text::fonts::craft_japanese().next().is_some()
        && filmcraft_text::fonts::all_faces()
            .iter()
            .any(|f| f.info.origin == filmcraft_text::fonts::CRAFT_ORIGIN && JAPANESE_SAMPLE.chars().all(|c| f.has_char(c)))
}

/// Japanese for the interface: true when built with the craft-fonts (already installed by
/// `theme::install`). Otherwise add the system's Japanese font as the last fallback of every theme font family, from the next
/// pass on. Returns false (and changes nothing) when no Japanese font is installed. Call it again
/// after `theme::install`, which replaces the font definitions.
pub fn install_japanese_font(ctx: &egui::Context) -> bool {
    if craft_japanese_font() {
        return true;
    }
    let Some(font) = system_japanese_font() else { return false };
    let families = crate::theme::font_families()
        .into_iter()
        .map(|family| egui::epaint::text::InsertFontFamily { family, priority: egui::epaint::text::FontPriority::Lowest })
        .collect();
    // queued for the next pass (works before the first frame); a no-op when already installed
    ctx.add_font(egui::epaint::text::FontInsert { name: JAPANESE_FONT.into(), data: (*font).clone(), families });
    true
}

/// Whether the interface can show Simplified Chinese: `theme::install` always adds a Chinese
/// fallback face when there is one (craft-fonts Hans/Hant faces, otherwise an installed system
/// face, see `cjk.rs`), so nothing needs installing here. False on systems without either.
pub fn chinese_font_available() -> bool {
    crate::cjk::available()
}

/// Whether the interface can show Korean: `theme::install` adds an installed system Hangul face
/// (Malgun Gothic, Apple SD Gothic Neo, Noto Sans CJK KR…, see `cjk.rs`) when there is one; none
/// is bundled. False on systems (and the web) without one.
pub fn korean_font_available() -> bool {
    crate::cjk::korean_available()
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod bcp47_tests {
    use super::Language;

    /// Protocol-facing BCP 47 tags use `pt-BR`/`zh-CN`; every tag is non-empty ASCII.
    #[test]
    fn accesskit_tags_use_protocol_spelling() {
        assert_eq!(Language::En.bcp47(), "en");
        assert_eq!(Language::PtBr.bcp47(), "pt-BR");
        assert_eq!(Language::ZhCn.bcp47(), "zh-CN");
        for language in Language::ALL {
            let tag = language.bcp47();
            assert!(!tag.is_empty() && tag.is_ascii(), "{tag:?}");
        }
    }
}
