//! Czech catalog checks. Shared, exact-key exceptions document brands, Czech loanwords and
//! semantically equivalent source labels; they must never silently accept new collisions.

use std::collections::{BTreeMap, BTreeSet};

use super::catalog;

#[derive(serde::Deserialize)]
struct SharedText {
    sources: BTreeSet<String>,
    reason: String,
}

#[derive(serde::Deserialize)]
struct Exceptions {
    identical: BTreeMap<String, String>,
    english: BTreeMap<String, BTreeMap<String, String>>,
    collisions: BTreeMap<String, SharedText>,
}

#[test]
fn czech_catalog_quality_gate() {
    let exceptions: Exceptions = serde_json::from_str(include_str!("cs-quality-exceptions.json")).unwrap();
    let (entries, errors) = catalog::parse_entries(include_str!("cs.tsv"));
    assert!(errors.is_empty(), "{errors:?}");
    let mut shared: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut identical_used = BTreeSet::new();
    let mut english_used = BTreeSet::new();
    let mut collisions_used = BTreeSet::new();
    let words = [
        "the", "a", "an", "of", "to", "in", "on", "and", "or", "with", "when", "is", "are", "has", "have", "always", "made", "attached", "will", "can", "not",
        "this", "that", "for", "from", "by", "at",
    ];
    for (context, source, translation) in entries {
        let source_key = format!("{context}\t{source}");
        let mut a = catalog::placeholders(&source);
        let mut b = catalog::placeholders(&translation);
        a.sort_unstable();
        b.sort_unstable();
        assert_eq!(a, b, "placeholders: {source:?}");
        assert!(!translation.contains('"'), "straight quotes: {source:?}");
        assert_eq!(source.matches('…').count(), translation.matches('…').count(), "ellipsis count: {source:?}");
        assert_eq!(source.ends_with('…'), translation.ends_with('…'), "trailing ellipsis: {source:?}");
        if source.trim_start().chars().next().is_some_and(char::is_uppercase) {
            assert!(
                translation.trim_start().chars().next().is_some_and(|c| !c.is_alphabetic() || c.is_uppercase()),
                "capitalization: {source:?} → {translation:?}"
            );
        }
        if source == translation {
            assert!(exceptions.identical.get(&source_key).is_some_and(|reason| !reason.is_empty()), "unjustified identical: {source:?}");
            identical_used.insert(source_key.clone());
        }
        // Placeholder names are identifiers supplied by the English call site, not prose.
        let mut prose = translation.clone();
        for placeholder in &b {
            prose = prose.replace(&format!("{{{placeholder}}}"), "");
        }
        for word in prose.split(|c: char| !c.is_alphanumeric() && c != '_').filter(|word| !word.is_empty()) {
            if !words.contains(&word.to_lowercase().as_str()) {
                continue;
            }
            // Exact Czech homographs: coordinating conjunction, demonstrative and conditional.
            if matches!(word, "a" | "to" | "by") {
                continue;
            }
            assert!(
                exceptions.english.get(&source_key).and_then(|row| row.get(word)).is_some_and(|reason| !reason.is_empty()),
                "English word {word:?}: {source:?} → {translation:?}"
            );
            english_used.insert(source_key.clone());
        }
        shared.entry(format!("{context}\t{translation}")).or_default().insert(source);
    }
    for (key, sources) in shared.iter().filter(|(_, sources)| sources.len() > 1) {
        let approved = exceptions.collisions.get(key).unwrap_or_else(|| panic!("unreviewed shared text {key:?}: {sources:?}"));
        assert!(!approved.reason.is_empty(), "missing rationale: {key:?}");
        assert_eq!(&approved.sources, sources, "shared text changed: {key:?}");
        collisions_used.insert(key.clone());
    }
    assert_eq!(identical_used, exceptions.identical.keys().cloned().collect(), "stale identical-text exception");
    assert_eq!(english_used, exceptions.english.keys().cloned().collect(), "stale English-word exception");
    assert_eq!(collisions_used, exceptions.collisions.keys().cloned().collect(), "stale shared-text exception");
}

#[test]
fn czech_editing_glossary_is_consistent() {
    let language = super::Language::Cs;
    for (source, czech) in [
        ("Timeline", "Časová osa"),
        ("Bin", "Přihrádka"),
        ("Trash", "Koš"),
        ("Mark In", "Označit vstupní bod"),
        ("Mark Out", "Označit výstupní bod"),
        ("Lift", "Zvednout"),
        ("Extract", "Vyjmout s posunem"),
        ("Cut", "Vyjmout"),
        ("Ripple Delete", "Odstranit s posunem"),
        ("Razor Tool", "Nástroj Žiletka"),
        ("Selection Tool", "Nástroj Výběr"),
        ("Ripple Edit Tool", "Nástroj Úprava s posunem"),
        ("Color Space", "Barevný prostor"),
        ("Audio Track Mixer", "Mixážní pult zvukových stop"),
    ] {
        assert_eq!(language.tr(source), czech, "{source}");
    }
}
