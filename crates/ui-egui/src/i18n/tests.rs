use super::*;

/// The catalog file of a language (none for English, the source language).
fn catalog_text(l: Language) -> Option<&'static str> {
    match l {
        Language::En => None,
        Language::Ja => Some(include_str!("ja.tsv")),
        Language::Es => Some(include_str!("es.tsv")),
        Language::PtBr => Some(include_str!("pt-br.tsv")),
        Language::Uk => Some(include_str!("uk.tsv")),
        Language::ZhCn => Some(include_str!("zh-cn.tsv")),
        Language::De => Some(include_str!("de.tsv")),
        Language::Ru => Some(include_str!("ru.tsv")),
        Language::Fr => Some(include_str!("fr.tsv")),
        Language::Id => Some(include_str!("id.tsv")),
        Language::It => Some(include_str!("it.tsv")),
        Language::Bs => Some(include_str!("bs.tsv")),
        Language::Ko => Some(include_str!("ko.tsv")),
        Language::Cs => Some(include_str!("cs.tsv")),
        Language::Pl => Some(include_str!("pl.tsv")),
        Language::Sv => Some(include_str!("sv.tsv")),
        Language::Tr => Some(include_str!("tr.tsv")),
    }
}

#[test]
fn catalogs_are_well_formed() {
    for l in Language::ALL {
        let Some(text) = catalog_text(l) else { continue };
        let code = l.code();
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
}

#[test]
fn unknown_strings_pass_through_and_languages_round_trip() {
    for l in Language::ALL {
        for text in ["An untranslated label", "mi video.mp4", "日本語の文書.pdf", "Größe ändern.mp4", "Montage d'été.mp4"] {
            assert_eq!(l.tr(text), text, "{}", l.code());
        }
        assert_eq!(Language::parse(l.code()), Some(l));
        let json = serde_json::to_string(&l).unwrap();
        assert_eq!(json, format!("\"{}\"", l.code()));
        assert_eq!(serde_json::from_str::<Language>(&json).unwrap(), l);
    }
    assert_eq!(Language::En.tr("File"), "File");
    assert_eq!(Language::parse("xx"), None);
    for code in ["de-DE", "fr-FR", "id-ID", "it-IT", "cs-CZ", "sv-SE"] {
        assert_eq!(Language::parse(code), None, "the preference stores the bare code");
    }
    // language names are always shown in their own language
    for (l, name) in [
        (Language::PtBr, "Português (Brasil)"),
        (Language::Uk, "Українська"),
        (Language::ZhCn, "简体中文"),
        (Language::De, "Deutsch"),
        (Language::Ru, "Русский"),
        (Language::Fr, "Français"),
        (Language::Id, "Bahasa Indonesia"),
        (Language::It, "Italiano"),
        (Language::Bs, "Bosanski"),
        (Language::Ko, "한국어"),
        (Language::Cs, "Čeština"),
        (Language::Pl, "Polski"),
        (Language::Sv, "Svenska"),
        (Language::Tr, "Türkçe"),
    ] {
        assert_eq!(l.name(), name);
    }
    assert_eq!(fmt("{b} y {a}", &[("a", "1"), ("b", "2")]), "2 y 1");
    assert_eq!(fmt("{a} {zz}", &[("a", "1")]), "1 {zz}");
}

#[test]
fn the_current_language_is_per_thread() {
    // any entry the Spanish catalog has
    let (entries, _) = catalog::parse_entries(include_str!("es.tsv"));
    let Some((_, en, es)) = entries.into_iter().find(|(ctx, en, es)| ctx.is_empty() && en != es) else { return };
    set_current(Language::Es);
    assert_eq!((current(), t(&en)), (Language::Es, es.as_str()));
    // another thread (another test) still draws in English
    let source = en.clone();
    assert_eq!(std::thread::spawn(move || t(&source).to_string()).join().ok(), Some(en.clone()));
    set_current(Language::En);
    assert_eq!(t(&en), en);
}

#[test]
fn template_values_are_not_interpreted_as_placeholders() {
    assert_eq!(fmt("{name}: {n}", &[("name", "Vídeo {n}.mp4"), ("n", "3")]), "Vídeo {n}.mp4: 3");
    assert_eq!(fmt("{n} {name} {n}", &[("name", "{n}"), ("n", "日本語")]), "日本語 {n} 日本語");
    assert_eq!(fmt("{missing} y {unfinished", &[]), "{missing} y {unfinished");
}

/// A search for a language's label finds the item, and so does its English name.
#[test]
fn searches_match_translated_labels_and_english_names() {
    for l in Language::ALL {
        set_current(l);
        let blur = l.tr("Gaussian Blur").to_lowercase();
        let selection = l.tr("Selection Tool").to_lowercase();
        assert!(matches_query("Gaussian Blur", &blur), "{}: {blur}", l.code());
        assert!(matches_query("Gaussian Blur", "gaussian"), "{}", l.code());
        assert!(matches_query("Selection Tool", &selection), "{}: {selection}", l.code());
        assert!(!matches_query("Gaussian Blur", &selection), "{}: {selection}", l.code());
    }
    set_current(Language::En);
}

/// `tl!` and `tlf!` take plain string literals: the catalog scan (`tl_literals`) reads no other kind.
#[test]
fn tl_literals_are_plain_strings() {
    for (path, text) in sources() {
        assert!(!text.contains("tl!(r") && !text.contains("tlf!(r"), "{path}: use a plain string literal in tl! and tlf!");
    }
}

/// Every string the interface translates: menu labels and paths (the native macOS app menu
/// included), `tl!`/`tlf!` literals, and names translated through `i18n::t`.
fn interface_strings() -> Vec<String> {
    let app = crate::FilmcraftApp::new(filmcraft_engine::Session::default());
    let mut out = Vec::new();
    for top in crate::menus::MENUS.into_iter().chain(["Audio"]).chain(crate::menus::NATIVE_APP_MENU) {
        push(&mut out, top);
    }
    for it in crate::menus::menu_items(&app) {
        push(&mut out, &it.label);
        it.path.iter().for_each(|p| push(&mut out, p));
    }
    for (_, text) in sources() {
        tl_literals(&text).iter().for_each(|lit| push(&mut out, lit));
    }
    dynamic_names().iter().for_each(|n| push(&mut out, n));
    out
}

/// The missing-translations report. It always passes: a string a catalog lacks shows in
/// English. `cargo xtask ci` runs it with `--nocapture` so the list shows in the gate log
/// (`cargo xtask i18n-report` runs it alone). Per language it lists the interface strings the
/// catalog lacks, then the catalog entries whose source the interface no longer has (renamed
/// or removed strings, or names this scan doesn't enumerate).
#[test]
fn missing_translations_report() {
    let strings = interface_strings();
    println!("Missing translations ({} interface strings; each shows in English):", strings.len());
    for l in Language::ALL {
        let Some(text) = catalog_text(l) else { continue };
        let code = l.code();
        let mut missing: Vec<&str> = strings.iter().map(String::as_str).filter(|s| !l.has(s)).collect();
        missing.sort_unstable();
        println!("{code}: {} missing", missing.len());
        for s in missing {
            println!("  {code}: {s:?}");
        }
        let (entries, _) = catalog::parse_entries(text);
        let unused: Vec<_> = entries.iter().filter(|(_, en, _)| translatable(en) && !strings.contains(en)).collect();
        if !unused.is_empty() {
            println!("{code}: {} entries without a source", unused.len());
            for (ctx, en, _) in unused {
                println!("  {code}: {ctx:?} {en:?}");
            }
        }
    }
}

/// Chinese switches and persists like the other languages when a Chinese face is available,
/// and is refused (the interface stays as it was) without one.
#[test]
fn chinese_switches_and_persists_or_is_refused_without_a_font() {
    let mut app = crate::FilmcraftApp::new(filmcraft_engine::Session::default());
    let ctx = egui::Context::default();
    let r = crate::menus::invoke(&mut app, &ctx, "app.language.chinese", serde_json::json!({}));
    if !chinese_font_available() {
        assert!(r.is_err(), "{r:?}");
        assert_eq!(app.ui.language, Language::En);
        assert_eq!(app.session.prefs.general.interface_language, "system");
        // a saved Chinese preference falls back to English at startup
        app.session.execute("prefs.set", serde_json::json!({"key": "general.interfaceLanguage", "value": "zh-cn"})).unwrap();
        app.apply_prefs(&ctx);
        assert_eq!(app.ui.language, Language::En);
        set_current(Language::En);
        return;
    }
    assert_eq!(r.unwrap(), serde_json::json!("zh-cn"));
    assert_eq!(app.ui.language, Language::ZhCn);
    assert_eq!(app.session.prefs.general.interface_language, "zh-cn");
    assert!(crate::menus::menu_items(&app).iter().any(|it| it.id == "app.language.chinese" && it.checked == Some(true)));
    let prefs = serde_json::to_string(&app.session.prefs).unwrap();
    let mut restarted = filmcraft_engine::Session::default();
    restarted.prefs = serde_json::from_str(&prefs).unwrap();
    assert_eq!(crate::FilmcraftApp::new(restarted).ui.language, Language::ZhCn);
    crate::menus::invoke(&mut app, &ctx, "app.language.english", serde_json::json!({})).unwrap();
    set_current(Language::En);
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
            } else if p.extension().is_some_and(|e| e == "rs") && p.file_name().is_some_and(|n| n != "tests.rs") {
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
    [ex::BitrateMode::Cbr, ex::BitrateMode::Vbr1Pass, ex::BitrateMode::Vbr2Pass, ex::BitrateMode::Crf].iter().for_each(|m| push(&mut out, m.label()));
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
fn tl_literals_are_found_and_unescaped() {
    let src = "a(tl!(\"Save\")); b(tl!(\"Say \\\"hi\\\"\\n\")); tl!(x); tlf!(\"{n} file(s)\", n);";
    assert_eq!(tl_literals(src), ["Save", "Say \"hi\"\n", "{n} file(s)"]);
}

type App = crate::FilmcraftApp;

fn switch(app: &mut App, ctx: &egui::Context, cmd: &str) -> serde_json::Value {
    crate::menus::invoke(app, ctx, cmd, serde_json::json!({})).unwrap()
}

/// The saved UI state round-trips the language (`code` is its serialized tag).
fn assert_ui_persists(app: &App, lang: Language, code: &str) {
    let saved = serde_json::to_string(&app.ui).unwrap();
    assert!(saved.contains(&format!("\"language\":\"{code}\"")), "{saved}");
    let restored: crate::state::UiState = serde_json::from_str(&saved).unwrap();
    assert_eq!(restored.language, lang);
}

/// A restart with the saved prefs comes up in `lang`.
fn assert_restart_keeps(app: &App, lang: Language) {
    let prefs = serde_json::to_string(&app.session.prefs).unwrap();
    let mut restarted = filmcraft_engine::Session::default();
    restarted.prefs = serde_json::from_str(&prefs).unwrap();
    assert_eq!(App::new(restarted).ui.language, lang);
}

/// Only `checked_id` is checked among the language menu items.
fn assert_only_checked(app: &App, checked_id: &str) {
    for it in crate::menus::menu_items(app).iter().filter(|it| it.id.starts_with("app.language.")) {
        assert_eq!(it.checked, Some(it.id == checked_id), "{}", it.id);
    }
}

fn assert_menu_checked(app: &App, id: &str) {
    assert!(crate::menus::menu_items(app).iter().any(|it| it.id == id && it.checked == Some(true)));
}

fn assert_labels(app: &App, labels: &[(&str, &str)]) {
    let items = crate::menus::menu_items(app);
    for (id, want) in labels {
        assert_eq!(items.iter().find(|it| it.id == *id).map(|it| it.label.as_str()), Some(*want), "{id}");
    }
}

/// Switch through the menu command and check everything a language change touches.
/// `earlier` is a language name from an earlier step: it must still show in its own language.
fn switch_and_check(app: &mut App, ctx: &egui::Context, name: &str, lang: Language, code: &str, own_name: &str, earlier: Option<(&str, &str)>) {
    let result = switch(app, ctx, &format!("app.language.{name}"));
    assert_eq!(result, serde_json::json!(code));
    assert_eq!((app.ui.language, current()), (lang, lang));
    assert_eq!(app.session.prefs.general.interface_language, code);
    assert_only_checked(app, &format!("app.language.{name}"));
    assert_labels(app, &[("file.saveAs", lang.tr("Save As…")), (&format!("app.language.{name}"), own_name)]);
    if let Some((id, label)) = earlier {
        assert_labels(app, &[(id, label)]);
    }
    assert_ui_persists(app, lang, code);
    assert_restart_keeps(app, lang);
}

#[test]
fn language_commands_switch_and_persist_without_a_document() {
    let mut app = App::new(filmcraft_engine::Session::default());
    let ctx = egui::Context::default();
    // Japanese needs an installed font (see `japanese_needs_an_installed_font`); skip that step without one
    if craft_japanese_font() || system_japanese_font().is_some() {
        switch(&mut app, &ctx, "app.language.japanese");
        assert_eq!(app.ui.language, Language::Ja);
        assert_ui_persists(&app, Language::Ja, "ja");
    }
    switch(&mut app, &ctx, "app.language.spanish");
    assert_eq!(app.ui.language, Language::Es);
    assert_restart_keeps(&app, Language::Es);
    assert_menu_checked(&app, "app.language.spanish");
    switch(&mut app, &ctx, "app.language.portuguese");
    assert_eq!(app.ui.language, Language::PtBr);
    assert_eq!(app.session.prefs.general.interface_language, "pt-br");
    assert_menu_checked(&app, "app.language.portuguese");
    for (name, lang, code, own, earlier) in [
        ("ukrainian", Language::Uk, "uk", "Українська", None),
        ("german", Language::De, "de", "Deutsch", Some(("app.language.spanish", "Español"))),
        ("french", Language::Fr, "fr", "Français", Some(("app.language.german", "Deutsch"))),
        ("indonesian", Language::Id, "id", "Bahasa Indonesia", Some(("app.language.french", "Français"))),
        ("italian", Language::It, "it", "Italiano", Some(("app.language.indonesian", "Bahasa Indonesia"))),
        ("czech", Language::Cs, "cs", "Čeština", Some(("app.language.italian", "Italiano"))),
        ("polish", Language::Pl, "pl", "Polski", Some(("app.language.italian", "Italiano"))),
        ("swedish", Language::Sv, "sv", "Svenska", Some(("app.language.italian", "Italiano"))),
        ("turkish", Language::Tr, "tr", "Türkçe", Some(("app.language.italian", "Italiano"))),
        ("bosnian", Language::Bs, "bs", "Bosanski", Some(("app.language.italian", "Italiano"))),
    ] {
        switch_and_check(&mut app, &ctx, name, lang, code, own, earlier);
    }
    switch(&mut app, &ctx, "app.language.english");
    assert_eq!(app.ui.language, Language::En);
    set_current(Language::En);
}

fn locales(tags: &[&str]) -> Language {
    Language::from_locales(&tags.iter().map(|t| t.to_string()).collect::<Vec<_>>())
}

/// #218: System Language picks the first of the user's preferred languages the interface has.
#[test]
fn system_language_follows_the_preferred_locales() {
    let l = locales;
    assert_eq!(l(&["es_ES.UTF-8"]), Language::Es);
    assert_eq!(l(&["es-419"]), Language::Es);
    assert_eq!(l(&["ES"]), Language::Es);
    assert_eq!(l(&["ja-JP"]), Language::Ja);
    assert_eq!(l(&["pt-BR"]), Language::PtBr);
    for tag in ["uk", "uk-UA", "uk_UA.UTF-8", "UK-ua"] {
        assert_eq!(l(&[tag]), Language::Uk);
    }
    assert_eq!(l(&["pt_PT.UTF-8@euro"]), Language::PtBr);
    for tag in ["zh", "zh-CN", "zh_CN.UTF-8", "zh-Hans", "zh-Hans-CN", "zh-SG", "ZH-cn", "zh.UTF-8"] {
        assert_eq!(l(&[tag]), Language::ZhCn, "{tag}");
    }
    for tag in ["zh-TW", "zh_TW.UTF-8", "zh-HK", "zh-MO", "zh-Hant", "zh-Hant-TW"] {
        assert_eq!(l(&[tag]), Language::En, "{tag}: Traditional Chinese has no catalog");
    }
    assert_eq!(l(&["zh-TW", "ja-JP"]), Language::Ja);
}

#[test]
fn system_language_picks_german_french_indonesian() {
    let l = locales;
    for tag in ["de", "DE", "de-DE", "de-AT", "de-CH", "de-LI", "de-LU", "de_DE.UTF-8", "de_AT.UTF-8@euro", "de_CH"] {
        assert_eq!(l(&[tag]), Language::De, "{tag}");
    }
    for tag in ["fr", "FR", "fr-FR", "fr-CA", "fr-BE", "fr-CH", "fr_FR.UTF-8", "fr_BE.UTF-8@euro", "fr_CA"] {
        assert_eq!(l(&[tag]), Language::Fr, "{tag}");
    }
    for tag in ["id", "ID", "id-ID", "id_ID.UTF-8", "id_ID", "in", "in-ID", "in_ID.UTF-8"] {
        assert_eq!(l(&[tag]), Language::Id, "{tag}");
    }
    assert_eq!(l(&["jv-ID", "ms-MY", "id-ID", "en"]), Language::Id, "Javanese and Malay have no catalog");
    for none in [&["ind"][..], &["i"], &["-id"], &["ms"], &["ms-ID"], &["jv"]] {
        assert_eq!(l(none), Language::En, "{none:?}");
    }
}

#[test]
fn system_language_picks_italian_korean_and_the_first_known() {
    let l = locales;
    for tag in ["it", "IT", "it-IT", "it-CH", "it-SM", "it_IT.UTF-8", "it_IT.UTF-8@euro", "it_CH"] {
        assert_eq!(l(&[tag]), Language::It, "{tag}");
    }
    assert_eq!(l(&["it-CH", "de-CH", "fr-CH", "en"]), Language::It);
    for tag in ["ko", "ko-KR", "ko_KR.UTF-8"] {
        assert_eq!(l(&[tag]), Language::Ko, "{tag}");
    }
    for tag in ["cs", "CS", "cs-CZ", "cs_CZ.UTF-8"] {
        assert_eq!(l(&[tag]), Language::Cs, "{tag}");
    }
    for tag in ["sv", "SV", "sv-SE", "sv-FI", "sv-AX", "sv_SE.UTF-8", "sv_FI.UTF-8@euro", "sv_SE"] {
        assert_eq!(l(&[tag]), Language::Sv, "{tag}");
    }
    assert_eq!(l(&["fi-FI", "sv-FI", "en"]), Language::Sv, "Finnish has no catalog");
    for none in [&["swe"][..], &["s"], &["-sv"], &["da"], &["nb-NO"], &["nn"], &["no"]] {
        assert_eq!(l(none), Language::En, "{none:?}");
    }
    for tag in ["tr", "TR", "tr-TR", "tr_TR.UTF-8", "tr_TR"] {
        assert_eq!(l(&[tag]), Language::Tr, "{tag}");
    }
    for none in [&["ita"][..], &["i"], &["-it"]] {
        assert_eq!(l(none), Language::En, "{none:?}");
    }
    assert_eq!(l(&["da-DK", "nl", "es-MX", "ja"]), Language::Es, "the first one the interface has");
    assert_eq!(l(&["rm-CH", "de-CH", "fr-CH", "en"]), Language::De);
    assert_eq!(l(&["rm-CH", "fr-CH", "de-CH", "en"]), Language::Fr);
    assert_eq!(l(&["en-GB", "es"]), Language::En);
    assert_eq!(l(&["en-US", "de"]), Language::En);
    for none in [&[][..], &["nl"], &["C"], &["POSIX"], &[""], &["e"], &["esp"], &["-es"], &["deu"], &["d"], &["-de"], &["fra"], &["f"], &["-fr"]] {
        assert_eq!(l(none), Language::En, "{none:?}");
    }
    assert_eq!(l(&[&"x".repeat(1 << 20), "es"]), Language::Es);
    assert_eq!(l(&[&"de".repeat(1 << 19), "de-DE"]), Language::De);
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
        vec!["nl-NL".into(), "es-ES".into()]
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

/// Every non-ASCII character of `text` renders with the bundled interface fonts, in every family.
fn assert_bundled_fonts_cover(text: impl Iterator<Item = char>) {
    let ctx = egui::Context::default();
    crate::theme::install(&ctx, &crate::theme::Tokens::for_kind(crate::theme::ThemeKind::default()));
    let mut output = ctx.run_ui(egui::RawInput::default(), |_| {});
    output.textures_delta.clear();
    let chars: std::collections::BTreeSet<_> = text.filter(|ch| !ch.is_ascii()).collect();
    ctx.fonts_mut(|fonts| {
        for family in crate::theme::font_families() {
            let font = egui::FontId::new(13.0, family);
            // has_glyph compares font faces, giving false negatives when Inter also supplies
            // the replacement character (the named medium/semibold stacks). Compare the
            // actual glyph texture regions instead; the noncharacter U+10FFFF is missing.
            let text: String = chars.iter().copied().chain(['\u{10ffff}']).collect();
            let galley = fonts.layout_no_wrap(text, font.clone(), egui::Color32::WHITE);
            let glyphs = &galley.rows[0].glyphs;
            assert_eq!(glyphs.len(), chars.len() + 1);
            let replacement = glyphs.last().unwrap().uv_rect;
            for glyph in glyphs.iter().take(chars.len()) {
                assert_ne!(glyph.uv_rect, replacement, "missing {} in {font:?}", glyph.chr);
            }
        }
    });
}

#[test]
fn ukrainian_renders_with_bundled_fonts() {
    let (entries, _) = catalog::parse_entries(include_str!("uk.tsv"));
    // Include the entire alphabet, even letters not yet used by a translated menu label.
    let alphabet = "АБВГҐДЕЄЖЗИІЇЙКЛМНОПРСТУФХЦЧШЩЬЮЯабвгґдеєжзиіїйклмнопрстуфхцчшщьюя";
    assert_bundled_fonts_cover(alphabet.chars().chain(Language::Uk.name().chars()).chain(entries.iter().flat_map(|(_, _, uk)| uk.chars())));
}

#[test]
fn portuguese_renders_with_bundled_fonts() {
    let (entries, _) = catalog::parse_entries(include_str!("pt-br.tsv"));
    // every Portuguese letter, even ones no entry uses yet, and every letter the entries use
    let alphabet = "ÁÂÃÀÇÉÊÍÓÔÕÚÜáâãàçéêíóôõúü";
    let letters = entries.iter().flat_map(|(_, _, pt)| pt.chars()).filter(|ch| ch.is_alphabetic());
    assert_bundled_fonts_cover(alphabet.chars().chain(Language::PtBr.name().chars()).chain(letters));
}

#[test]
fn french_renders_with_bundled_fonts() {
    let (entries, _) = catalog::parse_entries(include_str!("fr.tsv"));
    // every French letter and quotation mark, even ones no entry uses yet, and every letter the
    // entries use (symbols such as ✔ come from the English sources)
    let alphabet = "ÀÂÆÇÉÈÊËÎÏÔŒÙÛÜŸàâæçéèêëîïôœùûüÿ«»’";
    let letters = entries.iter().flat_map(|(_, _, fr)| fr.chars()).filter(|ch| ch.is_alphabetic());
    assert_bundled_fonts_cover(alphabet.chars().chain(Language::Fr.name().chars()).chain(letters));
}

#[test]
fn german_renders_with_bundled_fonts() {
    let (entries, _) = catalog::parse_entries(include_str!("de.tsv"));
    // every German letter and quotation mark, even ones no entry uses yet, and every letter,
    // dash and ellipsis the entries use (symbols such as ✔ come from the English sources)
    let alphabet = "ÄÖÜäöüß„“‚‘";
    let letters = entries.iter().flat_map(|(_, _, de)| de.chars()).filter(|ch| ch.is_alphabetic() || "„“‚‘–—…".contains(*ch));
    assert_bundled_fonts_cover(alphabet.chars().chain(Language::De.name().chars()).chain(letters));
}

#[test]
fn indonesian_renders_with_bundled_fonts() {
    let (entries, _) = catalog::parse_entries(include_str!("id.tsv"));
    // Indonesian is written in basic Latin letters; any other letter an entry uses, and its
    // curly quotes, dashes and ellipses, must still render (symbols such as ✔ come from the
    // English sources)
    let letters = entries.iter().flat_map(|(_, _, id)| id.chars()).filter(|ch| ch.is_alphabetic() || "“”‘’–—…".contains(*ch));
    assert_bundled_fonts_cover(Language::Id.name().chars().chain(letters));
}

#[test]
fn italian_renders_with_bundled_fonts() {
    let (entries, _) = catalog::parse_entries(include_str!("it.tsv"));
    // every Italian accented letter, even ones no entry uses yet, and every letter, curly quote,
    // dash and ellipsis the entries use (symbols such as ✔ come from the English sources)
    let alphabet = "ÀÈÉÌÍÎÒÓÙÚàèéìíîòóùú";
    let letters = entries.iter().flat_map(|(_, _, it)| it.chars()).filter(|ch| ch.is_alphabetic() || "“”‘’–—…".contains(*ch));
    assert_bundled_fonts_cover(alphabet.chars().chain(Language::It.name().chars()).chain(letters));
}

#[test]
fn czech_renders_with_bundled_fonts() {
    let (entries, _) = catalog::parse_entries(include_str!("cs.tsv"));
    let alphabet = "ÁČĎÉĚÍŇÓŘŠŤÚŮÝŽáčďéěíňóřšťúůýž„“‚‘’";
    let letters = entries.iter().flat_map(|(_, _, cs)| cs.chars()).filter(|ch| ch.is_alphabetic());
    assert_bundled_fonts_cover(alphabet.chars().chain(Language::Cs.name().chars()).chain(letters));
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
        // font is added then) or to the system Japanese font. Installed right-to-left faces
        // (`rtl::install`) come after every other fallback by design, so they are left out.
        for family in crate::theme::font_families() {
            let mut stack = fonts.definitions().families.get(&family).cloned().unwrap_or_default();
            stack.retain(|n| !crate::rtl::is_system_face(n));
            stack.retain(|n| n != crate::cjk::KOREAN_FACE && n != "system-chinese");
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

/// Edit ▸ Language and Settings ▸ Interface Language list the same languages in the same order.
#[test]
fn language_menu_and_settings_choices_are_in_the_same_order() {
    use filmcraft_engine::settings::{Kind, Row};
    fn choices(rows: &[Row], out: &mut Vec<&'static str>) {
        for r in rows {
            match r {
                Row::Field(f) if f.key == "general.interfaceLanguage" => {
                    if let Kind::Choice(opts) = f.kind {
                        out.extend(opts.iter().filter(|(code, _)| *code != "system").map(|(_, label)| *label));
                    }
                }
                Row::Group(_, inner) => choices(inner, out),
                _ => {}
            }
        }
    }
    let mut settings = Vec::new();
    for c in filmcraft_engine::settings::categories() {
        choices(c.rows, &mut settings);
    }
    let app = crate::FilmcraftApp::new(filmcraft_engine::Session::default());
    let menu: Vec<String> = crate::menus::menu_items(&app).into_iter().filter(|it| it.id.starts_with("app.language.")).map(|it| it.label).collect();
    assert!(!settings.is_empty());
    assert_eq!(menu, settings);
}

#[test]
fn polish_renders_with_bundled_fonts() {
    let (entries, errors) = catalog::parse_entries(include_str!("pl.tsv"));
    assert!(errors.is_empty(), "{errors:?}");
    // every Polish letter, even ones no entry uses yet, and every letter, curly quote, dash and
    // ellipsis the entries use
    let alphabet = "ĄĆĘŁŃÓŚŹŻąćęłńóśźż„”";
    let letters = entries.iter().flat_map(|(_, _, pl)| pl.chars()).filter(|ch| ch.is_alphabetic() || "„”“‘’–—…".contains(*ch));
    assert_bundled_fonts_cover(alphabet.chars().chain(Language::Pl.name().chars()).chain(letters));
}

#[test]
fn swedish_renders_with_bundled_fonts() {
    let (entries, _) = catalog::parse_entries(include_str!("sv.tsv"));
    // every Swedish letter (é as in "idé", ü in loanwords), even ones no entry uses yet, and every
    // letter, curly quote, dash and ellipsis the entries use (symbols such as ✔ come from the
    // English sources)
    let alphabet = "ÅÄÖÉÜåäöéü";
    let letters = entries.iter().flat_map(|(_, _, sv)| sv.chars()).filter(|ch| ch.is_alphabetic() || "“”‘’–—…".contains(*ch));
    assert_bundled_fonts_cover(alphabet.chars().chain(Language::Sv.name().chars()).chain(letters));
}

#[test]
fn russian_renders_with_bundled_fonts() {
    let (entries, _) = catalog::parse_entries(include_str!("ru.tsv"));
    // the whole alphabet including «ё», even letters no entry uses yet, and every letter, quote,
    // dash and ellipsis the entries use
    let alphabet = "АБВГДЕЁЖЗИЙКЛМНОПРСТУФХЦЧШЩЪЫЬЭЮЯабвгдеёжзийклмнопрстуфхцчшщъыьэюя«»";
    let letters = entries.iter().flat_map(|(_, _, ru)| ru.chars()).filter(|ch| ch.is_alphabetic() || "«»“”‘’–—…".contains(*ch));
    assert_bundled_fonts_cover(alphabet.chars().chain(Language::Ru.name().chars()).chain(letters));
}

#[test]
fn turkish_casing_and_search_folding() {
    assert_eq!(Language::Tr.to_uppercase("i"), "İ");
    assert_eq!(Language::Tr.to_uppercase("ı"), "I");
    assert_eq!(Language::Tr.to_lowercase("İ"), "i");
    assert_eq!(Language::Tr.to_lowercase("I"), "ı");
    assert_eq!(Language::Tr.to_lowercase("I\u{0307}"), "i");
    assert_eq!(to_uppercase("file"), "FILE");
    set_current(Language::Tr);
    assert_eq!(to_uppercase("i"), "İ");
    assert_eq!(to_lowercase("I"), "ı");
    assert_eq!(to_lowercase("I\u{0307}"), "i");
    assert!(matches_query("Gaussian Blur", "gaussian"));
    assert!(matches_query("Gaussian Blur", "GAUSSIAN"));
    assert!(matches_query("Inceleme", "inceleme"));
    assert!(matches_query("İnceleme", "inceleme"));
    assert!(matches_query("inceleme", "İnceleme"));
    assert!(matches_query("Işık", "ışık"));
    assert!(matches_query("ışık", "IŞIK"));
    set_current(Language::En);
}

#[test]
fn turkish_renders_with_bundled_fonts() {
    let (entries, _) = catalog::parse_entries(include_str!("tr.tsv"));
    let alphabet = "ABCÇDEFGĞHIİJKLMNOÖPRSŞTUÜVYZabcçdefgğhıijklmnoöprsştuüvyz";
    let letters = entries.iter().flat_map(|(_, _, tr)| tr.chars()).filter(|ch| ch.is_alphabetic() || "“”‘’–—…".contains(*ch));
    assert_bundled_fonts_cover(alphabet.chars().chain(Language::Tr.name().chars()).chain(letters));
}

/// Every Bosnian locale, with region, script or encoding, picks the Bosnian catalog.
#[test]
fn system_language_picks_bosnian() {
    let l = locales;
    for tag in ["bs", "BS", "bs-BA", "bs_BA.UTF-8", "bs-Latn-BA", "bs-Cyrl-BA", "BS-ba"] {
        assert_eq!(l(&[tag]), Language::Bs, "{tag}");
    }
    assert_eq!(l(&["hr-HR", "bs-BA"]), Language::Bs, "Croatian has no catalog here, so the next preference wins");
}
