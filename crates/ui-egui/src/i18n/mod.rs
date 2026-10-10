//! Interface translations. Strings in code stay English and are the lookup keys; one catalog per
//! language (`i18n/<code>.tsv`, format in the header of `es.tsv`) maps them to display text at draw
//! time. Untranslated strings are shown in English, so coverage can grow incrementally. Command ids,
//! automation ids, preference values, the control channel, the CLI and MCP stay English (an
//! automation element's label is the text shown, so it follows the interface language).
//!
//! # Looking strings up
//! - `tl!` with a string literal: that UI string in the language the UI is drawn in (see
//!   [`set_current`]). Use it for every label, button, heading, tooltip and hint; the tests check
//!   that each `tl!` literal has a Spanish translation.
//! - [`t`]: the same for a string that is not a literal (a name from a registry or a list).
//! - [`tr_ctx`]: when one English word needs different translations (the Edit menu, the Edit button).
//! - [`fmt`]: fill `{name}` placeholders after translating a template; translators may reorder them.
//!
//! Japanese text uses the Japanese craft-fonts when FilmCraft was built with them (`CRAFT_FONTS_DIR`;
//! `theme::install` already puts them in every font family, see [`craft_japanese_font`]), otherwise
//! a font already installed on the system ([`system_japanese_font`]); with neither, switching to
//! Japanese is refused with a message.

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
}

static JAPANESE: OnceLock<Catalog> = OnceLock::new();
static SPANISH: OnceLock<Catalog> = OnceLock::new();
static PORTUGUESE: OnceLock<Catalog> = OnceLock::new();

impl Language {
    pub const ALL: [Self; 4] = [Self::En, Self::Ja, Self::Es, Self::PtBr];

    pub fn name(self) -> &'static str {
        match self {
            Self::En => "English",
            Self::Ja => "日本語",
            Self::Es => "Español",
            Self::PtBr => "Português (Brasil)",
        }
    }

    pub fn parse(code: &str) -> Option<Self> {
        match code {
            "en" => Some(Self::En),
            "ja" => Some(Self::Ja),
            "es" => Some(Self::Es),
            "pt-br" => Some(Self::PtBr),
            _ => None,
        }
    }

    /// Interface Language ▸ System Language (#218): the first of the user's preferred languages
    /// (BCP 47 or POSIX locale tags such as `es-419`, `pt_BR.UTF-8`, most preferred first) that the
    /// interface has, else English. Any Portuguese gets the Brazilian catalog, the only one there is.
    pub fn from_locales(tags: &[String]) -> Self {
        const PRIMARY: [(&str, Language); 4] = [("en", Language::En), ("ja", Language::Ja), ("es", Language::Es), ("pt", Language::PtBr)];
        tags.iter()
            .find_map(|tag| {
                let primary = tag.split(['-', '_', '.', '@']).next().unwrap_or_default();
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
        }
    }

    /// The language's catalog (none for English, the source language).
    fn catalog(self) -> Option<&'static Catalog> {
        match self {
            Self::En => None,
            Self::Ja => Some(JAPANESE.get_or_init(|| Catalog::parse(include_str!("ja.tsv")))),
            Self::Es => Some(SPANISH.get_or_init(|| Catalog::parse(include_str!("es.tsv")))),
            Self::PtBr => Some(PORTUGUESE.get_or_init(|| Catalog::parse(include_str!("pt-br.tsv")))),
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

/// Match a lowercased search query against both the source and the displayed label.
pub fn matches_query(text: &str, query: &str) -> bool {
    text.to_lowercase().contains(query) || t(text).to_lowercase().contains(query)
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
/// read best at menu sizes). Any other installed face that covers [`JAPANESE_SAMPLE`] is used if
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
        let covers = |f: &filmcraft_text::fonts::Face| JAPANESE_SAMPLE.chars().all(|c| f.has_char(c));
        // within a family, the face closest to regular weight
        let by_weight = |f: &&Arc<filmcraft_text::fonts::Face>| f.info.weight.abs_diff(400);
        let preferred =
            PREFERRED_JAPANESE.iter().find_map(|name| faces.iter().filter(|f| f.info.family.eq_ignore_ascii_case(name) && covers(f)).min_by_key(by_weight));
        let face = preferred.or_else(|| faces.iter().filter(|f| covers(f)).min_by_key(by_weight))?;
        // kept for the life of the process (one font, read once) so installing it again after a
        // theme change shares the bytes instead of copying the whole file
        let bytes: &'static [u8] = Box::leak(face.data()?.into_boxed_slice());
        Some(Arc::new(egui::FontData { font: std::borrow::Cow::Borrowed(bytes), index: face.info.index, tweak: Default::default() }))
    })
    .clone()
}

/// Whether the craft-fonts build input (empty unless built with `CRAFT_FONTS_DIR`) supplies a
/// Japanese interface font: some craft-fonts face covers [`JAPANESE_SAMPLE`]. `theme::install` adds
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogs_are_well_formed() {
        for (code, text) in [("es", include_str!("es.tsv")), ("ja", include_str!("ja.tsv")), ("pt-br", include_str!("pt-br.tsv"))] {
            let (entries, errors) = catalog::parse_entries(text);
            assert!(errors.is_empty(), "{code}: {errors:?}");
            for (i, (ctx, en, tr)) in entries.iter().enumerate() {
                assert!(entries.iter().take(i).all(|(c, e, _)| (c, e) != (ctx, en)), "{code}: duplicate entry {ctx:?} {en:?}");
                let (mut a, mut b) = (catalog::placeholders(en), catalog::placeholders(tr));
                a.sort_unstable();
                b.sort_unstable();
                assert_eq!(a, b, "{code}: placeholders differ in {en:?} → {tr:?}");
                assert_eq!(en.ends_with('…'), tr.ends_with('…'), "{code}: ellipsis differs in {en:?} → {tr:?}");
            }
        }
        assert_eq!(Language::Es.tr("File"), "Archivo");
        assert_eq!(Language::Ja.tr("File"), "ファイル");
        assert_eq!(Language::En.tr("File"), "File");
        assert_eq!(Language::Es.tr("mi video.mp4"), "mi video.mp4");
        assert_eq!(Language::Ja.tr("日本語の文書.pdf"), "日本語の文書.pdf");
        assert_eq!(Language::parse("es"), Some(Language::Es));
        assert_eq!(Language::parse("xx"), None);
        assert_eq!(Language::PtBr.tr("File"), "Arquivo");
        assert_eq!(Language::PtBr.tr("meu video.mp4"), "meu video.mp4");
        assert_eq!(Language::PtBr.name(), "Português (Brasil)");
        for l in Language::ALL {
            assert_eq!(Language::parse(l.code()), Some(l));
            let json = serde_json::to_string(&l).unwrap();
            assert_eq!(json, format!("\"{}\"", l.code()));
            assert_eq!(serde_json::from_str::<Language>(&json).unwrap(), l);
        }
        assert_eq!(fmt("{b} y {a}", &[("a", "1"), ("b", "2")]), "2 y 1");
        assert_eq!(fmt("{a} {zz}", &[("a", "1")]), "1 {zz}");
    }

    #[test]
    fn the_current_language_is_per_thread() {
        set_current(Language::Es);
        assert_eq!((current(), t("File")), (Language::Es, "Archivo"));
        // another thread (another test) still draws in English
        assert_eq!(std::thread::spawn(|| t("File").to_string()).join().ok().as_deref(), Some("File"));
        set_current(Language::En);
        assert_eq!(t("File"), "File");
    }

    #[test]
    fn template_values_are_not_interpreted_as_placeholders() {
        assert_eq!(fmt("{name}: {n}", &[("name", "Vídeo {n}.mp4"), ("n", "3")]), "Vídeo {n}.mp4: 3");
        assert_eq!(fmt("{n} {name} {n}", &[("name", "{n}"), ("n", "日本語")]), "日本語 {n} 日本語");
        assert_eq!(fmt("{missing} y {unfinished", &[]), "{missing} y {unfinished");
    }

    #[test]
    fn searches_match_spanish_labels_and_english_names() {
        set_current(Language::Es);
        assert!(matches_query("Gaussian Blur", "desenfoque"));
        assert!(matches_query("Gaussian Blur", "gaussian"));
        assert!(matches_query("Selection Tool", &"SELECCIÓN".to_lowercase()));
        assert!(!matches_query("Gaussian Blur", "selección"));
        set_current(Language::En);
    }

    #[test]
    fn spanish_translates_every_menu_label() {
        let app = crate::FilmcraftApp::new(filmcraft_engine::Session::default());
        let mut missing: Vec<String> = Vec::new();
        let mut need = |text: &str| {
            if translatable(text) && !Language::Es.has(text) && !missing.iter().any(|m| m == text) {
                missing.push(text.to_string());
            }
        };
        for top in crate::menus::MENUS {
            need(top);
        }
        for it in crate::menus::menu_items(&app) {
            need(&it.label);
            for p in &it.path {
                need(p);
            }
        }
        missing.sort();
        assert!(missing.is_empty(), "untranslated menu labels: {missing:#?}");
    }

    /// The Brazilian Portuguese catalog covers the menus; every entry must still be a menu label
    /// (a renamed command would otherwise leave a dead entry and an untranslated menu item).
    #[test]
    fn portuguese_entries_are_menu_labels() {
        let app = crate::FilmcraftApp::new(filmcraft_engine::Session::default());
        let items = crate::menus::menu_items(&app);
        let known = |text: &str| crate::menus::MENUS.contains(&text) || items.iter().any(|it| it.label == text || it.path.iter().any(|p| p == text));
        let (entries, _) = catalog::parse_entries(include_str!("pt-br.tsv"));
        assert!(!entries.is_empty());
        for (_, en, _) in entries {
            assert!(known(&en), "not a menu label: {en}");
        }
        assert_eq!(Language::PtBr.tr("File"), "Arquivo");
    }

    /// Every `tl!`/`tlf!` literal outside test modules has a Spanish translation.
    #[test]
    fn spanish_translates_every_tl_literal() {
        let mut missing = Vec::new();
        for (path, text) in sources() {
            for lit in tl_literals(&text) {
                if !Language::Es.has(&lit) && !missing.contains(&lit) {
                    missing.push(lit);
                }
            }
            assert!(!text.contains("tl!(r") && !text.contains("tlf!(r"), "{path}: use a plain string literal in tl! and tlf!");
        }
        missing.sort();
        assert!(missing.is_empty(), "untranslated tl! strings: {missing:#?}");
    }

    /// The crate's sources (`src/**/*.rs`), each cut at its test module.
    fn sources() -> Vec<(String, String)> {
        let mut out = Vec::new();
        let mut dirs = vec![std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/src"))];
        while let Some(d) = dirs.pop() {
            for entry in std::fs::read_dir(&d).into_iter().flatten().flatten() {
                let p = entry.path();
                if p.is_dir() {
                    dirs.push(p);
                } else if p.extension().is_some_and(|e| e == "rs") {
                    // a Windows checkout with core.autocrlf has CRLF, which the `\n` below would not match
                    let text = std::fs::read_to_string(&p).unwrap_or_default().replace("\r\n", "\n");
                    let cut = text.find("#[cfg(test)]\nmod tests").unwrap_or(text.len());
                    out.push((p.display().to_string(), text[..cut].to_string()));
                }
            }
        }
        out
    }

    /// The (unescaped) string literals passed to `tl!` and `tlf!` in a source file.
    fn tl_literals(text: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut rest = text;
        loop {
            let next = ["tl!(\"", "tlf!(\""].into_iter().filter_map(|p| Some((rest.find(p)?, p.len()))).min();
            let Some((at, len)) = next else { break };
            rest = &rest[at + len..];
            let mut lit = String::new();
            let mut chars = rest.char_indices();
            let mut end = rest.len();
            while let Some((i, c)) = chars.next() {
                match c {
                    '"' => {
                        end = i + 1;
                        break;
                    }
                    '\\' => match chars.next().map(|(_, e)| e) {
                        Some('n') => lit.push('\n'),
                        Some('t') => lit.push('\t'),
                        Some('u') => {
                            // \u{201c}
                            let hex: String = chars.by_ref().map(|(_, h)| h).take_while(|h| *h != '}').filter(|h| *h != '{').collect();
                            lit.extend(u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32));
                        }
                        Some(e) => lit.push(e),
                        None => {}
                    },
                    c => lit.push(c),
                }
            }
            rest = &rest[end..];
            out.push(lit);
        }
        out
    }

    /// Does `s` hold words to translate? Numbers with units ("1.25 kHz", "24 fps", "8 Bit", "100 IRE")
    /// and the language names (always shown in their own language) don't.
    fn translatable(s: &str) -> bool {
        const UNITS: [&str; 14] = ["Hz", "kHz", "dB", "oct", "fps", "IRE", "mm", "px", "ms", "s", "x", "Bit", "HLG", "PQ"];
        !Language::ALL.iter().any(|l| l.name() == s) && s.split(|c: char| !c.is_alphabetic()).any(|w| !w.is_empty() && !UNITS.contains(&w))
    }

    fn push(out: &mut Vec<String>, s: &str) {
        if translatable(s) && !out.iter().any(|o| o == s) {
            out.push(s.to_string());
        }
    }

    fn settings_rows(list: &[filmcraft_engine::settings::Row], out: &mut Vec<String>) {
        use filmcraft_engine::settings::{Kind, Row};
        for r in list {
            match r {
                Row::Field(f) => {
                    push(out, f.label);
                    match f.kind {
                        Kind::Int { unit, .. } | Kind::Float { unit, .. } => push(out, unit),
                        Kind::Choice(opts) => opts.iter().for_each(|(_, l)| push(out, l)),
                        _ => {}
                    }
                }
                Row::Group(title, inner) => {
                    push(out, title);
                    settings_rows(inner, out);
                }
                Row::Note(text) => push(out, text),
                Row::Button { label, .. } => push(out, label),
                Row::Custom(_) => {}
            }
        }
    }

    /// Names the interface shows through `i18n::t` rather than as `tl!` literals: registries and
    /// schemas from the engine and the other crates, and the UI's own enums.
    fn dynamic_names() -> Vec<String> {
        let mut out = Vec::new();
        for c in filmcraft_engine::settings::categories() {
            push(&mut out, c.title);
            settings_rows(c.rows, &mut out);
        }
        filmcraft_engine::settings::DURATION_UNITS.iter().for_each(|(_, l)| push(&mut out, l));
        for top in filmcraft_project::vtransition::EFFECT_TOP_FOLDERS {
            push(&mut out, top);
        }
        for d in filmcraft_project::effect_defs() {
            push(&mut out, d.name);
            d.category.iter().for_each(|c| push(&mut out, c));
            for p in &d.params {
                push(&mut out, p.label);
                p.group.iter().for_each(|g| push(&mut out, g));
                if let filmcraft_project::ParamKind::Choice(opts) = p.kind {
                    opts.iter().for_each(|o| push(&mut out, o));
                }
            }
        }
        crate::panels::timeline::CLIP_MENU.iter().flat_map(|g| g.iter()).for_each(|(l, _)| push(&mut out, l));
        crate::panels::timeline::EDIT_POINT_TYPES.iter().for_each(|(l, ..)| push(&mut out, l));
        crate::panels::project::NEW_ITEMS.iter().for_each(|(l, _)| push(&mut out, l));
        // section headers keyed by their English name (collapsed state), translated when drawn
        let sections = [
            // Lumetri Color
            "Basic Correction",
            "Creative",
            "Curves",
            "Color Wheels & Match",
            "HSL Secondary",
            "Vignette",
            // Properties
            "Transform",
            "Crop",
            // Essential Graphics
            "Layers",
            "Template Properties",
            "Responsive Design - Time",
            "Responsive Design - Position",
            "Align and Transform",
            "Text",
            "Shape",
            "Appearance",
        ];
        sections.iter().for_each(|s| push(&mut out, s));
        for p in filmcraft_render::lumetri_presets::presets() {
            push(&mut out, p.name);
            push(&mut out, p.folder);
            push(&mut out, p.description);
        }
        crate::dock::PanelKind::ALL.iter().for_each(|p| push(&mut out, p.title()));
        crate::dock::WORKSPACES.iter().for_each(|w| push(&mut out, w));
        filmcraft_project::Label::ALL.iter().for_each(|l| push(&mut out, l.name()));
        crate::panels::panel_state::TcMode::ALL.iter().for_each(|m| push(&mut out, m.label()));
        crate::panels::panel_state::TcSource::ALL.iter().for_each(|s| push(&mut out, s.label()));
        crate::panels::panel_state::SCOPE_PRESETS.iter().for_each(|p| push(&mut out, p.0));
        filmcraft_time::TimeDisplay::ALL.iter().for_each(|d| push(&mut out, d.label()));
        use filmcraft_engine::panels::Level;
        [Level::Info, Level::Warning, Level::Error].iter().for_each(|l| push(&mut out, l.label()));
        use filmcraft_scopes as sc;
        sc::ScopeKind::ALL.iter().for_each(|k| push(&mut out, k.label()));
        sc::WaveformType::ALL.iter().for_each(|k| push(&mut out, k.label()));
        sc::ParadeType::ALL.iter().for_each(|k| push(&mut out, k.label()));
        sc::ColorSpace::ALL.iter().for_each(|k| push(&mut out, k.label()));
        sc::Scale::ALL.iter().for_each(|k| push(&mut out, k.label()));
        sc::Brightness::ALL.iter().for_each(|k| push(&mut out, k.label()));
        [sc::Targets::Percent75, sc::Targets::Percent100].iter().for_each(|k| push(&mut out, k.label()));
        crate::state::PlaybackRes::ALL.iter().for_each(|r| push(&mut out, r.label()));
        crate::state::Tool::ALL.iter().for_each(|t| push(&mut out, t.label()));
        crate::credits::NameMode::ALL.iter().for_each(|m| push(&mut out, m.label()));
        crate::credits::SortKey::ALL.iter().for_each(|k| push(&mut out, k.label().0));
        filmcraft_project::TimeInterpolation::ALL.iter().for_each(|m| push(&mut out, m.label()));
        filmcraft_project::CaptionFormat::ALL.iter().for_each(|f| push(&mut out, f.label()));
        filmcraft_project::MaskMode::ALL.iter().for_each(|m| push(&mut out, m.label()));
        use filmcraft_project::essential as es;
        for kind in es::AudioType::ALL {
            push(&mut out, kind.label());
            kind.sections().iter().for_each(|s| push(&mut out, s.label()));
        }
        es::EQ_PRESETS.iter().for_each(|p| push(&mut out, p.name));
        es::REVERB_PRESETS.iter().for_each(|p| push(&mut out, p.name));
        filmcraft_engine::essential_sound::presets(&filmcraft_engine::Session::default()).iter().for_each(|p| push(&mut out, &p.name));
        filmcraft_project::TrackMethod::ALL.iter().for_each(|m| push(&mut out, m.label()));
        filmcraft_project::AutomationMode::ALL.iter().for_each(|m| push(&mut out, m.label()));
        filmcraft_project::graphic_design::RollMode::ALL.iter().for_each(|m| push(&mut out, m.label()));
        filmcraft_project::InputMap::ALL.iter().for_each(|m| push(&mut out, m.label()));
        filmcraft_engine::panels::LOG_FIELDS.iter().for_each(|f| push(&mut out, f));
        filmcraft_engine::project_panel::BUILTIN_COLUMNS.iter().for_each(|c| push(&mut out, c));
        filmcraft_project::find::COLUMNS.iter().for_each(|c| push(&mut out, c));
        filmcraft_project::FindOp::ALL.iter().for_each(|o| push(&mut out, o.label()));
        filmcraft_engine::media_browser::ALL_COLUMNS.iter().for_each(|c| push(&mut out, c));
        // export: presets, settings choices, ranges and pixel aspects
        use filmcraft_engine::export as ex;
        for p in ex::builtin_presets() {
            push(&mut out, &p.name);
            push(&mut out, &p.category);
            push(&mut out, &p.description);
        }
        ex::Format::ALL.iter().for_each(|f| push(&mut out, f.label()));
        ex::MxfVideoCodec::ALL.iter().for_each(|c| push(&mut out, c.label()));
        ex::Placement::ALL.iter().for_each(|p| push(&mut out, p.label()));
        [ex::FieldOrder::Progressive, ex::FieldOrder::UpperFirst, ex::FieldOrder::LowerFirst].iter().for_each(|f| push(&mut out, f.label()));
        [ex::BitrateMode::Cbr, ex::BitrateMode::Vbr1Pass, ex::BitrateMode::Vbr2Pass].iter().for_each(|m| push(&mut out, m.label()));
        [ex::H264Profile::Baseline, ex::H264Profile::Main, ex::H264Profile::High].iter().for_each(|p| push(&mut out, p.label()));
        crate::panels::export_mode::RANGES.iter().for_each(|(_, l)| push(&mut out, l));
        crate::panels::export_mode::PARS.iter().for_each(|(l, _)| push(&mut out, l));
        for template in filmcraft_project::gtemplate::builtin_templates() {
            push(&mut out, &template.name);
            push(&mut out, &template.category);
            push(&mut out, &template.description);
            template.controls.iter().for_each(|c| push(&mut out, &c.name));
        }
        // effect presets (Effects panel ▸ Presets)
        for p in filmcraft_engine::presets::builtin_presets() {
            push(&mut out, &p.name);
            push(&mut out, &p.description);
        }
        // every command (Keyboard Shortcuts dialog, History panel) and the shortcut contexts
        let app = crate::FilmcraftApp::new(filmcraft_engine::Session::default());
        for c in app.session.shortcuts.commands() {
            push(&mut out, &c.label);
            push(&mut out, &c.category);
        }
        push(&mut out, filmcraft_engine::shortcuts::APPLICATION);
        filmcraft_engine::shortcuts::PANELS.iter().for_each(|p| push(&mut out, p));
        // metadata rows, item types and history entries of the demo project
        let mut s = filmcraft_engine::Session::default();
        let _ = s.execute("file.openDemoProject", serde_json::json!({}));
        for it in s.project.items.values() {
            push(&mut out, it.type_label());
            for f in filmcraft_engine::panels::metadata_fields(&s.project, it.id) {
                push(&mut out, f.section);
                push(&mut out, &f.name);
            }
        }
        out
    }

    #[test]
    fn spanish_translates_dynamic_names() {
        let mut missing: Vec<String> = dynamic_names().into_iter().filter(|n| !Language::Es.has(n)).collect();
        missing.sort();
        assert!(missing.is_empty(), "untranslated names: {missing:#?}");
    }

    #[test]
    fn tl_literals_are_found_and_unescaped() {
        let src = "a(tl!(\"Save\")); b(tl!(\"Say \\\"hi\\\"\\n\")); tl!(x); tlf!(\"{n} file(s)\", n);";
        assert_eq!(tl_literals(src), ["Save", "Say \"hi\"\n", "{n} file(s)"]);
    }

    #[test]
    fn language_commands_switch_and_persist_without_a_document() {
        let mut app = crate::FilmcraftApp::new(filmcraft_engine::Session::default());
        let ctx = egui::Context::default();
        crate::menus::invoke(&mut app, &ctx, "app.language.japanese", serde_json::json!({})).unwrap();
        assert_eq!(app.ui.language, Language::Ja);
        let saved = serde_json::to_string(&app.ui).unwrap();
        let restored: crate::state::UiState = serde_json::from_str(&saved).unwrap();
        assert_eq!(restored.language, Language::Ja);
        crate::menus::invoke(&mut app, &ctx, "app.language.spanish", serde_json::json!({})).unwrap();
        assert_eq!(app.ui.language, Language::Es);
        let prefs = serde_json::to_string(&app.session.prefs).unwrap();
        let mut restarted = filmcraft_engine::Session::default();
        restarted.prefs = serde_json::from_str(&prefs).unwrap();
        assert_eq!(crate::FilmcraftApp::new(restarted).ui.language, Language::Es);
        assert!(crate::menus::menu_items(&app).iter().any(|it| it.id == "app.language.spanish" && it.checked == Some(true)));
        crate::menus::invoke(&mut app, &ctx, "app.language.portuguese", serde_json::json!({})).unwrap();
        assert_eq!(app.ui.language, Language::PtBr);
        assert_eq!(app.session.prefs.general.interface_language, "pt-br");
        assert!(crate::menus::menu_items(&app).iter().any(|it| it.id == "app.language.portuguese" && it.checked == Some(true)));
        crate::menus::invoke(&mut app, &ctx, "app.language.english", serde_json::json!({})).unwrap();
        assert_eq!(app.ui.language, Language::En);
        set_current(Language::En);
    }

    /// #218: System Language picks the first of the user's preferred languages the interface has.
    #[test]
    fn system_language_follows_the_preferred_locales() {
        let l = |tags: &[&str]| Language::from_locales(&tags.iter().map(|t| t.to_string()).collect::<Vec<_>>());
        assert_eq!(l(&["es_ES.UTF-8"]), Language::Es);
        assert_eq!(l(&["es-419"]), Language::Es);
        assert_eq!(l(&["ES"]), Language::Es);
        assert_eq!(l(&["ja-JP"]), Language::Ja);
        assert_eq!(l(&["pt-BR"]), Language::PtBr);
        assert_eq!(l(&["pt_PT.UTF-8@euro"]), Language::PtBr);
        assert_eq!(l(&["fr-FR", "de", "es-MX", "ja"]), Language::Es, "the first one the interface has");
        assert_eq!(l(&["en-GB", "es"]), Language::En);
        for none in [&[][..], &["fr"], &["C"], &["POSIX"], &[""], &["e"], &["esp"], &["-es"]] {
            assert_eq!(l(none), Language::En, "{none:?}");
        }
        assert_eq!(l(&[&"x".repeat(1 << 20), "es"]), Language::Es);
    }

    /// #218: the `system` preference (the default) asks the host for the system's languages; an
    /// explicit choice does not, and without a host hook System Language is English.
    #[test]
    fn system_language_preference_uses_the_host_languages() {
        let ctx = egui::Context::default();
        let session = filmcraft_engine::Session::default();
        assert_eq!(session.prefs.general.interface_language, "system");
        let mut app = crate::FilmcraftApp::new(session);
        app.apply_prefs(&ctx);
        assert_eq!(app.ui.language, Language::En, "no host hook");

        let mut app = crate::FilmcraftApp::new(filmcraft_engine::Session::default());
        let asked = std::rc::Rc::new(std::cell::Cell::new(0));
        let count = asked.clone();
        app.hooks.system_languages = Some(Box::new(move || {
            count.set(count.get() + 1);
            vec!["fr-FR".into(), "es-ES".into()]
        }));
        app.apply_prefs(&ctx);
        assert_eq!(app.ui.language, Language::Es);
        assert_eq!(current(), Language::Es);
        crate::menus::invoke(&mut app, &ctx, "app.language.english", serde_json::json!({})).unwrap();
        app.apply_prefs(&ctx);
        assert_eq!((app.ui.language, app.session.prefs.general.interface_language.as_str()), (Language::En, "en"));
        let asked_before = asked.get();
        app.session.execute("prefs.set", serde_json::json!({"key": "general.interfaceLanguage", "value": "ja"})).unwrap();
        app.apply_prefs(&ctx);
        assert_eq!(asked.get(), asked_before, "an explicit language does not ask the system");
        app.session.execute("prefs.set", serde_json::json!({"key": "general.interfaceLanguage", "value": "system"})).unwrap();
        app.apply_prefs(&ctx);
        assert_eq!(app.ui.language, Language::Es);
        assert_eq!(asked.get(), asked_before + 1);
        set_current(Language::En);
    }

    #[test]
    fn japanese_needs_an_installed_font() {
        let mut app = crate::FilmcraftApp::new(filmcraft_engine::Session::default());
        let ctx = egui::Context::default();
        crate::theme::install(&ctx, &crate::theme::Tokens::for_kind(crate::theme::ThemeKind::default()));
        let r = crate::menus::invoke(&mut app, &ctx, "app.language.japanese", serde_json::json!({}));
        let craft = craft_japanese_font();
        if !craft && system_japanese_font().is_none() {
            // no craft-fonts and no system font: refused, and the interface stays English
            assert!(r.is_err(), "{r:?}");
            assert_eq!(app.ui.language, Language::En);
            return;
        }
        assert!(r.is_ok(), "{r:?}");
        let mut output = ctx.run_ui(egui::RawInput::default(), |_| {});
        output.textures_delta.clear();
        ctx.fonts_mut(|fonts| {
            // every theme family falls back to the craft-fonts (when built with them; no system
            // font is added then) or to the system Japanese font
            for family in crate::theme::font_families() {
                let stack = fonts.definitions().families.get(&family).cloned().unwrap_or_default();
                if craft {
                    assert!(stack.last().is_some_and(|n| n.starts_with("craft:")), "{family:?}: {stack:?}");
                    assert!(!stack.iter().any(|n| n == JAPANESE_FONT), "{family:?}: {stack:?}");
                } else {
                    assert_eq!(stack.last().map(String::as_str), Some(JAPANESE_FONT), "{family:?}: {stack:?}");
                }
            }
            // and the glyphs resolve. (Only families whose replacement-box face is another font:
            // egui's `has_glyph` reports false for any character served by the face it also uses
            // for the replacement box, which in the Inter-only "medium"/"semibold" stacks is the
            // Japanese font itself.)
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                let font = egui::FontId::new(13.0, family);
                for ch in JAPANESE_SAMPLE.chars() {
                    assert!(fonts.has_glyph(&font, ch), "missing {ch} in {font:?}");
                }
            }
        });
    }
}
