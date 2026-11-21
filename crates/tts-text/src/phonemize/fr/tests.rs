use super::*;
use crate::{Chunk, Lang, Lexicon, Overrides, normalize, phonemize};

fn said(script: &str) -> String {
    phonemize(script, Lang::Fr, &Lexicon::default(), &Overrides::default())
        .unwrap()
        .into_iter()
        .map(|c| match c {
            Chunk::Phonemes(p) => p,
            Chunk::Pause { millis } => format!("[{millis}]"),
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

fn words_of(script: &str) -> String {
    normalize(script, Lang::Fr).unwrap().into_iter().filter_map(|t| if let crate::Token::Word(w) = t { Some(w) } else { None }).collect::<Vec<_>>().join(" ")
}

/// Common words, checked against their standard French pronunciation.
#[test]
fn common_words() {
    for (w, want) in [
        ("bonjour", "bɔ̃ʒˈuʁ"),
        ("France", "fʁˈɑ̃s"),
        ("rose", "ʁˈoz"),
        ("langue", "lˈɑ̃ɡ"),
        ("premier", "pʁəmjˈe"),
        ("parler", "paʁlˈe"),
        ("billet", "bijˈɛ"),
        ("enfant", "ɑ̃fˈɑ̃"),
        ("bien", "bjˈɛ̃"),
        ("vient", "vjˈɛ̃"),
        ("parlent", "pˈaʁl"),
        ("étudient", "etydˈi"),
        ("comment", "kɔmˈɑ̃"),
        ("rapidement", "ʁapidəmˈɑ̃"),
        ("question", "kɛstjˈɔ̃"),
        ("nation", "nasjˈɔ̃"),
        ("soleil", "sɔlˈɛj"),
        ("travail", "tʁavˈaj"),
        ("famille", "famˈij"),
        ("avril", "avʁˈil"),
        ("heure", "ˈœʁ"),
        ("deux", "dˈø"),
        ("bonne", "bˈɔn"),
        ("maison", "mɛzˈɔ̃"),
        ("petit", "pətˈi"),
        ("merci", "mɛʁsˈi"),
        ("avoir", "avwˈaʁ"),
        ("être", "ˈɛtʁ"),
        ("j'ai", "ʒˈe"),
        ("l'homme", "lˈɔm"),
        ("nuit", "nɥˈi"),
        ("guerre", "ɡˈɛʁ"),
        ("école", "ekˈɔl"),
        ("quinze", "kˈɛ̃z"),
        ("monde", "mˈɔ̃d"),
        ("science", "sjˈɑ̃s"),
        ("temps", "tˈɑ̃"),
        ("cœur", "kˈœʁ"),
        ("fauteuil", "fotˈœj"),
        ("travailler", "tʁavajˈe"),
        ("exemple", "ɛɡzˈɑ̃pl"),
        ("texte", "tˈɛkst"),
        ("femme", "fˈam"),
        ("histoire", "istwˈaʁ"),
        ("aujourd'hui", "oʒuʁdɥˈi"),
        ("français", "fʁɑ̃sˈɛ"),
        ("journée", "ʒuʁnˈe"),
        ("beaucoup", "bokˈu"),
        ("toujours", "tuʒˈuʁ"),
        ("deuxième", "døzjˈɛm"),
        ("euros", "øʁˈo"),
        ("Siwis", "siwˈis"),
    ] {
        assert_eq!(word(w), want, "{w}");
    }
}

/// Function words take no stress; spelled letters use the French letter names.
#[test]
fn function_words_and_letters() {
    assert_eq!(word("les"), "le");
    assert_eq!(word("de"), "də");
    assert_eq!(word("vous"), "vu");
    assert_eq!(said("La SNCF."), "la ˈɛs ˈɛn sˈe ˈɛf.");
}

/// Liaison before a vowel, none before a consonant or an aspirated h.
#[test]
fn liaisons() {
    assert_eq!(said("les amis"), "lez amˈi");
    assert_eq!(said("les chats"), "le ʃˈa");
    assert_eq!(said("les héros"), "le eʁˈo");
    assert_eq!(said("un ami"), "œ̃n amˈi");
    assert_eq!(said("C'est un grand enfant."), "sˈɛt œ̃ ɡʁˈɑ̃t ɑ̃fˈɑ̃.");
    assert_eq!(said("six enfants"), "sˈiz ɑ̃fˈɑ̃");
}

/// No liaison across kept punctuation; an accented capital is read as a word, not a letter
/// name; a hostile word full of apostrophes is read without deep recursion.
#[test]
fn review_cases() {
    assert_eq!(said("Chez nous, on mange."), "ʃˈe nu, ɔ̃ mˈɑ̃ʒ.");
    assert_eq!(said("les amis"), "lez amˈi");
    assert_eq!(said("À demain."), "a dəmˈɛ̃.");
    assert_eq!(said("État"), "etˈa");
    let hostile = "a'".repeat(200_000) + "a";
    let r = std::thread::Builder::new().stack_size(256 * 1024).spawn(move || said(&hostile).len()).unwrap().join();
    assert!(r.is_ok_and(|n| n > 0), "a word of 200 000 apostrophes is read on a small stack");
    assert_eq!(word("jusqu'à"), "ʒyska");
    assert_eq!(word("aujourd'hui"), "oʒuʁdɥˈi");
}

/// French counting and the other number forms.
#[test]
fn numbers() {
    for (script, want) in [
        ("0", "zéro"),
        ("17", "dix sept"),
        ("21", "vingt et un"),
        ("71", "soixante et onze"),
        ("75", "soixante quinze"),
        ("80", "quatre vingts"),
        ("81", "quatre vingt un"),
        ("99", "quatre vingt dix neuf"),
        ("200", "deux cents"),
        ("280", "deux cent quatre vingts"),
        ("1000", "mille"),
        ("1 000", "mille"),
        ("2026", "deux mille vingt six"),
        ("1 234 567", "un million deux cent trente quatre mille cinq cent soixante sept"),
        ("3 000 000 000", "trois milliards"),
        ("21,5", "vingt et un virgule cinq"),
        ("3,05", "trois virgule zéro cinq"),
        ("-4", "moins quatre"),
        ("15 %", "quinze pour cent"),
        ("3,50 €", "trois euros cinquante"),
        ("1 €", "un euro"),
        ("14h30", "quatorze heures trente"),
        ("9 h", "neuf heures"),
        ("1er", "premier"),
        ("1re", "première"),
        ("2e", "deuxième"),
        ("21e", "vingt et unième"),
        ("06 12", "zéro six douze"),
        ("123456789012345", "un deux trois quatre cinq six sept huit neuf zéro un deux trois quatre cinq"),
    ] {
        assert_eq!(words_of(script), want, "{script}");
    }
}

/// Abbreviations, symbols, pauses and sentence ends.
#[test]
fn abbreviations_and_markers() {
    assert_eq!(words_of("M. Dupont et Mme Martin, etc."), "monsieur Dupont et madame Martin et cetera");
    assert_eq!(words_of("n° 5 & co"), "numéro cinq et co");
    assert_eq!(said("Oui. [pause 1s] Non ?"), "wˈi. | [1000] | nˈɔ̃?");
    assert_eq!(said("Vraiment !"), "vʁɛmˈɑ̃!");
}

/// Overrides: raw French phonemes, or a French respelling.
#[test]
fn overrides() {
    let mut o = Overrides::default();
    o.0.insert("Dupont".into(), "/dypɔ̃/".into());
    o.0.insert("Lyon".into(), "lion".into());
    o.0.insert("faux".into(), "/θ/".into());
    let p = phonemize("Dupont à Lyon, faux", Lang::Fr, &Lexicon::default(), &o).unwrap();
    assert_eq!(p, vec![Chunk::Phonemes("dypɔ̃ a ljˈɔ̃, fˈo".into())]);
}

/// Every output character is in [`SYMBOLS`] (Kokoro's vocabulary) and hostile scripts
/// never panic.
#[test]
fn output_stays_in_the_symbol_set_on_hostile_input() {
    let mut rng = 0x5eed_1234_9876_abcdu64;
    let mut next = move || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };
    let alphabet: Vec<char> = "aàâbcçdeéèêëfghiîïjklmnoôœpqrstuùûüvwxyzAÉÇ0123456789 '’-.,;:!?%€$&@#/()[]«»\u{a0}\u{202f}hpause".chars().collect();
    for _ in 0..3000 {
        let n = (next() % 60) as usize;
        let s: String = (0..n).map(|_| alphabet[(next() % alphabet.len() as u64) as usize]).collect();
        let r = std::panic::catch_unwind(|| phonemize(&s, Lang::Fr, &Lexicon::default(), &Overrides::default()));
        let Ok(Ok(chunks)) = r else { panic!("{s:?} failed: {r:?}") };
        for c in chunks {
            if let Chunk::Phonemes(p) = c {
                assert!(p.chars().all(|ch| SYMBOLS.contains(ch)), "{s:?} → {p:?}");
            }
        }
    }
    let long = "9".repeat(5000) + &" quatre-vingt".repeat(2000);
    assert!(phonemize(&long, Lang::Fr, &Lexicon::default(), &Overrides::default()).is_ok());
}

/// Print words for a manual look (`FR_PROBE="..." cargo test -p filmcraft-tts-text probe -- --ignored --nocapture`).
#[test]
#[ignore]
fn probe() {
    let list = std::env::var("FR_PROBE").unwrap_or_default();
    for w in list.split_whitespace() {
        println!("{w:>16}  {}", word(w));
    }
    let script = std::env::var("FR_SCRIPT").unwrap_or_default();
    println!("{}", said(&script));
}
