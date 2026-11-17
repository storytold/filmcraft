//! French pronunciation: spoken French words → Kokoro phoneme symbols (IPA).
//!
//! Kokoro's French voice (`ff_siwis`) was trained on IPA labels, so this module writes
//! standard French IPA: oral vowels `a e ɛ ə i o ɔ u y ø œ`, nasal vowels as a vowel plus
//! U+0303 (`ɑ̃ ɛ̃ ɔ̃ œ̃`), glides `j w ɥ`, `ʁ`, `ɲ`, `ʃ ʒ`, and `ˈ` before the last vowel of each
//! stressed word (French stresses the final syllable; function words take none).
//!
//! Lookup order per word: user overrides, a small table of irregular words, then
//! letter-to-sound rules: silent endings (`-e`, plural `-s`, verb `-ent`, the final
//! consonants other than c, r, f, l), nasal vowels, `e` (é / è / mute e by syllable shape),
//! `o` (open or closed), `eu` / `œu`, `ill`, soft `c` / `g`, `gn`, `qu`, `tion`, intervocalic
//! `s`, `x`. Elisions (`l'`, `qu'`) and common liaisons (`les amis`, `un ami`, `c'est un`)
//! are applied across words.
//!
//! Rules and the word table were written from scratch from French spelling conventions and
//! public IPA charts (clean-room: no G2P or TTS source was read).

/// Every character a French phoneme string may contain.
pub const SYMBOLS: &str = "ˈˌ aɑeɛəiouyøœɔ\u{303}jwɥbdfɡklmnɲŋpʁsʃtvzʒ,.!?;:—…()\"";

/// Phonemes and stress of one word.
#[derive(Clone, Debug, Default, PartialEq)]
struct Pron {
    phones: Vec<&'static str>,
    /// No stress mark (articles, pronouns, prepositions...).
    unstressed: bool,
}

/// Irregular words: spelling → phonemes (without stress), as a sorted table.
const WORDS: &[(&str, &str)] = &[
    ("abbaye", "abei"),
    ("ail", "aj"),
    ("album", "albɔm"),
    ("aliment", "alimɑ̃"),
    ("amer", "amɛʁ"),
    ("ans", "ɑ̃"),
    ("août", "ut"),
    ("après", "apʁɛ"),
    ("aspect", "aspɛ"),
    ("atlas", "atlas"),
    ("aujourd'hui", "oʒuʁdɥi"),
    ("aussi", "osi"),
    ("autobus", "otɔbys"),
    ("avec", "avɛk"),
    ("bœuf", "bœf"),
    ("bonus", "bɔnys"),
    ("bus", "bys"),
    ("but", "byt"),
    ("cactus", "kaktys"),
    ("campus", "kɑ̃pys"),
    ("cent", "sɑ̃"),
    ("cents", "sɑ̃"),
    ("chaos", "kao"),
    ("cher", "ʃɛʁ"),
    ("chœur", "kœʁ"),
    ("cinq", "sɛ̃k"),
    ("clef", "kle"),
    ("club", "klœb"),
    ("corps", "kɔʁ"),
    ("correct", "kɔʁɛkt"),
    ("dès", "dɛ"),
    ("deuxième", "døzjɛm"),
    ("direct", "diʁɛkt"),
    ("dix", "dis"),
    ("dixième", "dizjɛm"),
    ("donc", "dɔ̃k"),
    ("doigt", "dwa"),
    ("écho", "eko"),
    ("email", "imɛl"),
    ("emmener", "ɑ̃mne"),
    ("ennemi", "ɛnmi"),
    ("enfer", "ɑ̃fɛʁ"),
    ("es", "ɛ"),
    ("est", "ɛ"),
    ("estomac", "ɛstɔma"),
    ("et", "e"),
    ("euro", "øʁo"),
    ("europe", "øʁɔp"),
    ("euros", "øʁo"),
    ("eu", "y"),
    ("eue", "y"),
    ("eus", "y"),
    ("eut", "y"),
    ("exact", "ɛɡzakt"),
    ("examen", "ɛɡzamɛ̃"),
    ("faisaient", "fəzɛ"),
    ("faisais", "fəzɛ"),
    ("faisait", "fəzɛ"),
    ("faisons", "fəzɔ̃"),
    ("fait", "fɛ"),
    ("femme", "fam"),
    ("femmes", "fam"),
    ("fer", "fɛʁ"),
    ("fier", "fjɛʁ"),
    ("fils", "fis"),
    ("football", "futbɔl"),
    ("fusil", "fyzi"),
    ("gentil", "ʒɑ̃ti"),
    ("hélas", "elas"),
    ("hier", "jɛʁ"),
    ("hiver", "ivɛʁ"),
    ("huit", "ɥit"),
    ("instinct", "ɛ̃stɛ̃"),
    ("internet", "ɛ̃tɛʁnɛt"),
    ("jadis", "ʒadis"),
    ("madame", "madam"),
    ("mars", "maʁs"),
    ("maximum", "maksimɔm"),
    ("mer", "mɛʁ"),
    ("messieurs", "mesjø"),
    ("milliard", "miljaʁ"),
    ("milliards", "miljaʁ"),
    ("mille", "mil"),
    ("million", "miljɔ̃"),
    ("millions", "miljɔ̃"),
    ("minimum", "minimɔm"),
    ("moins", "mwɛ̃"),
    ("monsieur", "məsjø"),
    ("net", "nɛt"),
    ("neuf", "nœf"),
    ("œil", "œj"),
    ("œuf", "œf"),
    ("œufs", "ø"),
    ("oignon", "ɔɲɔ̃"),
    ("ok", "oke"),
    ("orchestre", "ɔʁkɛstʁ"),
    ("os", "ɔs"),
    ("ouest", "wɛst"),
    ("oui", "wi"),
    ("ours", "uʁs"),
    ("parfum", "paʁfœ̃"),
    ("parking", "paʁkiŋ"),
    ("patience", "pasjɑ̃s"),
    ("patient", "pasjɑ̃"),
    ("pays", "pei"),
    ("paysage", "peizaʒ"),
    ("plus", "ply"),
    ("poids", "pwa"),
    ("porc", "pɔʁ"),
    ("près", "pʁɛ"),
    ("psychologie", "psikɔlɔʒi"),
    ("respect", "ʁɛspɛ"),
    ("second", "səɡɔ̃"),
    ("seconde", "səɡɔ̃d"),
    ("sens", "sɑ̃s"),
    ("sept", "sɛt"),
    ("six", "sis"),
    ("sixième", "sizjɛm"),
    ("siwis", "siwis"),
    ("soixante", "swasɑ̃t"),
    ("solennel", "sɔlanɛl"),
    ("sud", "syd"),
    ("super", "sypɛʁ"),
    ("tabac", "taba"),
    ("technique", "tɛknik"),
    ("temps", "tɑ̃"),
    ("tennis", "tenis"),
    ("tous", "tu"),
    ("tranquille", "tʁɑ̃kil"),
    ("très", "tʁɛ"),
    ("ver", "vɛʁ"),
    ("village", "vilaʒ"),
    ("ville", "vil"),
    ("vingt", "vɛ̃"),
    ("virus", "viʁys"),
    ("vis", "vis"),
    ("week", "wik"),
    ("yeux", "jø"),
    ("zéro", "zeʁo"),
];

/// Words that take no stress mark.
const FUNCTION_WORDS: &[&str] = &[
    "à", "au", "aux", "ce", "ces", "cet", "cette", "dans", "de", "des", "du", "elle", "elles", "en", "et", "il", "ils", "je", "l", "la", "le", "les", "leur",
    "leurs", "lui", "ma", "me", "mes", "mon", "ne", "nos", "notre", "nous", "on", "ou", "par", "pour", "qu", "que", "qui", "sa", "se", "ses", "son", "sur",
    "ta", "te", "tes", "ton", "tu", "un", "une", "vos", "votre", "vous", "y",
];

/// Nouns and adjectives ending in `-ent` that are said (the other `-ent` words are verbs
/// in the third person plural, whose `-ent` is silent).
const ENT_SAID: &[&str] = &[
    "absent",
    "accent",
    "accident",
    "adolescent",
    "agent",
    "argent",
    "ardent",
    "client",
    "comment",
    "content",
    "continent",
    "décent",
    "dent",
    "différent",
    "évident",
    "équivalent",
    "excellent",
    "fragment",
    "fréquent",
    "gent",
    "incident",
    "indulgent",
    "innocent",
    "intelligent",
    "lent",
    "orient",
    "parent",
    "patient",
    "permanent",
    "présent",
    "président",
    "prudent",
    "quotient",
    "récent",
    "segment",
    "sergent",
    "serpent",
    "souvent",
    "talent",
    "torrent",
    "urgent",
    "vent",
    "violent",
];

/// Verbs in `-ment` (third person plural) after a vowel, where `-ent` is silent.
const MENT_VERBS: &[&str] =
    &["aiment", "animent", "estiment", "expriment", "fument", "impriment", "nomment", "priment", "sèment", "suppriment", "consument", "riment"];

/// Words ending in `-er` that keep the r (and an open è).
const ER_SAID: &[&str] = &["amer", "cancer", "cher", "enfer", "fer", "fier", "hamster", "hier", "hiver", "laser", "mer", "poster", "revolver", "super", "ver"];

/// Words ending in `s` (after the plural is ruled out) that say it.
const S_SAID: &[&str] =
    &["atlas", "bus", "cactus", "campus", "fils", "hélas", "iris", "jadis", "lys", "maïs", "mars", "oasis", "os", "ours", "sens", "tennis", "virus", "vis"];

/// `ill` words that say `il`, not `ij`.
const ILL_SAID: &[&str] = &["mille", "million", "milliard", "tranquille", "ville", "village", "villa", "distiller", "osciller"];

/// Aspirated h: no elision and no liaison before these.
const H_ASPIRE: &[&str] = &[
    "haut",
    "haute",
    "hauts",
    "hasard",
    "héros",
    "honte",
    "haricot",
    "hache",
    "haine",
    "hall",
    "hamac",
    "hameau",
    "hanche",
    "handicap",
    "hangar",
    "harpe",
    "hausse",
    "hibou",
    "hockey",
    "homard",
    "hongrois",
    "hors",
    "hurler",
    "huit",
    "huitième",
];

/// Liaison: word → consonant said before a vowel.
const LIAISON: &[(&str, &str)] = &[
    ("aux", "z"),
    ("c'est", "t"),
    ("cent", "t"),
    ("n'est", "t"),
    ("vingt", "t"),
    ("ces", "z"),
    ("chez", "z"),
    ("dans", "z"),
    ("des", "z"),
    ("deux", "z"),
    ("dix", "z"),
    ("elles", "z"),
    ("en", "n"),
    ("est", "t"),
    ("grand", "t"),
    ("ils", "z"),
    ("les", "z"),
    ("leurs", "z"),
    ("mes", "z"),
    ("mon", "n"),
    ("nos", "z"),
    ("nous", "z"),
    ("on", "n"),
    ("ont", "t"),
    ("petit", "t"),
    ("plus", "z"),
    ("quand", "t"),
    ("sans", "z"),
    ("ses", "z"),
    ("six", "z"),
    ("son", "n"),
    ("sont", "t"),
    ("tes", "z"),
    ("ton", "n"),
    ("tout", "t"),
    ("très", "z"),
    ("trois", "z"),
    ("un", "n"),
    ("vos", "z"),
    ("vous", "z"),
];

/// Elided prefixes and what they say.
const ELISIONS: &[(&str, &str)] = &[
    ("jusqu", "ʒysk"),
    ("lorsqu", "lɔʁsk"),
    ("puisqu", "pɥisk"),
    ("presqu", "pʁɛsk"),
    ("quoiqu", "kwak"),
    ("qu", "k"),
    ("c", "s"),
    ("d", "d"),
    ("j", "ʒ"),
    ("l", "l"),
    ("m", "m"),
    ("n", "n"),
    ("s", "s"),
    ("t", "t"),
];

fn table(word: &str) -> Option<&'static str> {
    WORDS.iter().chain(SHORT_WORDS).find(|(w, _)| *w == word).map(|(_, p)| *p)
}

/// Short grammatical words the rules would get wrong (mute e, closed é).
const SHORT_WORDS: &[(&str, &str)] = &[
    ("ce", "sə"),
    ("ces", "se"),
    ("chez", "ʃe"),
    ("de", "də"),
    ("des", "de"),
    ("en", "ɑ̃"),
    ("je", "ʒə"),
    ("le", "lə"),
    ("les", "le"),
    ("me", "mə"),
    ("mes", "me"),
    ("ne", "nə"),
    ("nez", "ne"),
    ("non", "nɔ̃"),
    ("où", "u"),
    ("ou", "u"),
    ("que", "kə"),
    ("se", "sə"),
    ("ses", "se"),
    ("te", "tə"),
    ("tes", "te"),
    ("un", "œ̃"),
    ("une", "yn"),
];

/// Letter name of a single letter (spelled acronyms: SNCF).
fn letter_name(c: char) -> &'static str {
    match c.to_lowercase().next().unwrap_or(c) {
        'a' => "a",
        'b' => "be",
        'c' => "se",
        'd' => "de",
        'e' => "ə",
        'f' => "ɛf",
        'g' => "ʒe",
        'h' => "aʃ",
        'i' => "i",
        'j' => "ʒi",
        'k' => "ka",
        'l' => "ɛl",
        'm' => "ɛm",
        'n' => "ɛn",
        'o' => "o",
        'p' => "pe",
        'q' => "ky",
        'r' => "ɛʁ",
        's' => "ɛs",
        't' => "te",
        'u' => "y",
        'v' => "ve",
        'w' => "dubləve",
        'x' => "iks",
        'y' => "iɡʁɛk",
        'z' => "zɛd",
        _ => "",
    }
}

fn is_vowel_letter(c: char) -> bool {
    matches!(c, 'a' | 'à' | 'â' | 'ä' | 'e' | 'é' | 'è' | 'ê' | 'ë' | 'i' | 'î' | 'ï' | 'o' | 'ô' | 'ö' | 'u' | 'ù' | 'û' | 'ü' | 'y' | 'œ' | 'æ')
}

fn is_front_vowel(c: char) -> bool {
    matches!(c, 'e' | 'é' | 'è' | 'ê' | 'ë' | 'i' | 'î' | 'ï' | 'y')
}

/// A vowel phoneme (the first char of a phone).
fn is_vowel_phone(p: &str) -> bool {
    p.chars().next().is_some_and(|c| "aɑeɛəiouyøœɔ".contains(c))
}

/// Split a phoneme string into phones (a nasal vowel keeps its tilde).
fn phones_of(s: &'static str) -> Vec<&'static str> {
    let mut out = Vec::new();
    let mut start = 0usize;
    for (i, c) in s.char_indices() {
        if i > start && c != '\u{303}' {
            if let Some(p) = s.get(start..i) {
                out.push(p);
            }
            start = i;
        }
    }
    if let Some(p) = s.get(start..).filter(|p| !p.is_empty()) {
        out.push(p);
    }
    out
}

/// Interned phones for the rules (each a `&'static str`).
fn ph(s: &str) -> &'static str {
    const ALL: &[&str] = &[
        "a", "ɑ", "e", "ɛ", "ə", "i", "o", "ɔ", "u", "y", "ø", "œ", "ɑ̃", "ɛ̃", "ɔ̃", "œ̃", "j", "w", "ɥ", "b", "d", "f", "ɡ", "k", "l", "m", "n", "ɲ", "ŋ", "p",
        "ʁ", "s", "ʃ", "t", "v", "z", "ʒ",
    ];
    ALL.iter().copied().find(|p| *p == s).unwrap_or("")
}

/// Pronounce one word (letters, maybe with an elision apostrophe).
fn word_pron(word: &str) -> Pron {
    word_pron_within(word, 0)
}

/// Elisions a word can chain (`jusqu'à l'…` is two); past this, apostrophes are dropped, so a
/// hostile `a'a'a'…` word neither recurses deeply nor re-reads its tail once per apostrophe.
const MAX_ELISIONS: usize = 4;

fn word_pron_within(word: &str, depth: usize) -> Pron {
    let mut lower = word.to_lowercase().replace("oeu", "œu").replace("oeil", "œil");
    if lower.is_empty() {
        return Pron::default();
    }
    if depth >= MAX_ELISIONS {
        lower.retain(|c| c != '\'');
    }
    // spelled letters (acronyms come from the normalizer one letter at a time); an accented
    // capital (À demain, État) is a word, not a letter name
    let mut chars = word.chars();
    if let (Some(c), None) = (chars.next(), chars.next())
        && c.is_ascii_uppercase()
    {
        return Pron { phones: phones_of(letter_name(c)), unstressed: false };
    }
    if let Some(p) = table(&lower) {
        return Pron { phones: phones_of(p), unstressed: FUNCTION_WORDS.contains(&lower.as_str()) };
    }
    // elision: l'homme, qu'il, jusqu'à
    if let Some((head, tail)) = lower.split_once('\'') {
        if let Some((_, said)) = ELISIONS.iter().find(|(h, _)| *h == head) {
            let mut p = word_pron_within(tail, depth + 1);
            let mut phones = phones_of(said);
            phones.append(&mut p.phones);
            return Pron { phones, unstressed: p.unstressed };
        }
        let mut p = word_pron_within(head, depth + 1);
        p.phones.append(&mut word_pron_within(tail, depth + 1).phones);
        return p;
    }
    let letters: Vec<char> = lower.chars().filter(|c| c.is_alphabetic()).collect();
    if letters.is_empty() {
        return Pron::default();
    }
    Pron { phones: rules(&letters, &lower), unstressed: FUNCTION_WORDS.contains(&lower.as_str()) }
}

/// The pronounced part of a word: how many letters are said, and how the end reads.
struct Core {
    /// Letters said (a prefix of the word).
    end: usize,
    /// The last said letter is a consonant that is pronounced (a mute `e` followed it, or it
    /// is one of c r f l q k b).
    final_consonant_said: bool,
    /// The word ends in `-er` / `-ez` / `-ed`: the letters before them, then a closed é.
    closed_e_suffix: bool,
}

fn core(w: &[char], word: &str) -> Core {
    let n = w.len();
    let ends = |s: &str| word.ends_with(s);
    let mut end = n;
    let mut e_dropped = false;
    // verb -ent (3rd plural) and its look-alikes
    if n > 3 && ends("ent") {
        let said_ent = ENT_SAID.contains(&word)
            || (ends("ment") && n > 5 && w.get(n - 5).is_some_and(|&c| is_vowel_letter(c)) && !MENT_VERBS.contains(&word))
            || ends("vient")
            || ends("tient");
        if !said_ent {
            end = n - 3;
            e_dropped = true;
            // "-ient" verbs: étudient → i
        }
    }
    // plural / 2nd person s, then the mute e
    let s_said = S_SAID.contains(&word);
    if !e_dropped && end > 2 && w.get(end - 1) == Some(&'s') && !s_said && w.get(end - 2) != Some(&'s') {
        // es: plural of a word in mute e
        if w.get(end - 2) == Some(&'e') && !matches!(word, "les" | "des" | "mes" | "tes" | "ses" | "ces") {
            end -= 2;
            e_dropped = true;
        } else {
            end -= 1;
        }
    }
    if !e_dropped && end > 2 && w.get(end - 1) == Some(&'e') {
        end -= 1;
        e_dropped = true;
    }
    if e_dropped {
        // a vowel before the dropped e stays (vie, joue); a consonant is said (porte)
        let last = w.get(end.saturating_sub(1)).copied().unwrap_or('a');
        return Core { end, final_consonant_said: !is_vowel_letter(last), closed_e_suffix: false };
    }
    // -er / -ez / -ed / -ers: closed é, consonant silent
    let tail: String = w.get(end.saturating_sub(2)..end).unwrap_or(&[]).iter().collect();
    if end > 2 && matches!(tail.as_str(), "er" | "ez" | "ed") && !(tail == "er" && ER_SAID.contains(&word)) {
        return Core { end: end - 2, final_consonant_said: false, closed_e_suffix: true };
    }
    // final consonants: c r f l q k b are said (CaReFuL), the rest fall silent, several in a row too
    let mut said = false;
    if s_said {
        said = true;
    } else {
        while end > 1 {
            let Some(&c) = w.get(end - 1) else { break };
            match c {
                'd' | 'g' | 'p' | 's' | 't' | 'x' | 'z' => end -= 1,
                'c' if matches!(w.get(end - 2), Some('n')) => end -= 1,
                'l' if end >= 3 && w.get(end - 2) == Some(&'i') && !w.get(end - 3).is_some_and(|&v| is_vowel_letter(v)) && n > 3 => {
                    // gentil, outil: covered by the table; other -il words say l (avril, fil)
                    said = true;
                    break;
                }
                c if !is_vowel_letter(c) && c != 'h' && c != 'n' && c != 'm' => {
                    said = true;
                    break;
                }
                _ => break,
            }
        }
    }
    Core { end, final_consonant_said: said, closed_e_suffix: false }
}

/// The letters of one word being read, with its pronounced core.
struct Scan<'a> {
    w: &'a [char],
    word: &'a str,
    c: Core,
    /// `ill` is said `il` (ville, mille).
    ill_said: bool,
}

/// One rule's result: the phones it says and the letters it reads.
type Step = (&'static str, usize);

impl Scan<'_> {
    /// Letter `i` of the said core.
    fn at(&self, i: usize) -> Option<char> {
        if i < self.c.end { self.w.get(i).copied() } else { None }
    }
    /// Letter `i` of the whole word (also past the core: France, rose).
    fn raw(&self, i: usize) -> Option<char> {
        self.w.get(i).copied()
    }
    fn vowel_at(&self, i: usize) -> bool {
        self.at(i).is_some_and(is_vowel_letter)
    }
    fn starts(&self, i: usize, s: &str) -> bool {
        s.chars().enumerate().all(|(k, ch)| self.at(i + k) == Some(ch))
    }
    /// An n / m at `j` that closes a nasal vowel: not followed by a vowel or another n / m.
    fn nasal_after(&self, j: usize) -> bool {
        let end = self.c.end;
        matches!(self.at(j), Some('n' | 'm'))
            && !self.vowel_at(j + 1)
            && !matches!(self.at(j + 1), Some('n' | 'm'))
            && !(j + 1 >= end && self.raw(j + 1).is_some_and(is_vowel_letter) && self.c.final_consonant_said)
    }
    /// Letters read by an `ill` / final `il` group starting at `i` (after the vowel).
    fn ill_len(&self, i: usize) -> usize {
        if self.at(i + 1) == Some('l') && self.at(i + 2) == Some('l') { 3 } else { 2 }
    }

    /// Vowel digraphs and trigraphs: eau, au, ai, ei, oi, ou, eu, the ill groups.
    fn vowel_group(&self, i: usize) -> Option<Step> {
        let (w, end) = (self.w, self.c.end);
        let l = self.at(i)?;
        let next = self.at(i + 1);
        let i_next = matches!(next, Some('i' | 'î'));
        Some(match l {
            'e' if self.starts(i, "eau") => ("o", 3),
            'a' if next == Some('u') => ("o", 2),
            'a' | 'e' if next == Some('i') && self.nasal_after(i + 2) => ("ɛ̃", 3),
            'a' | 'e' if i_next && starts_ill(w, i + 1) && !self.ill_said => (if l == 'a' { "aj" } else { "ɛj" }, 1 + self.ill_len(i + 1)),
            'a' | 'e' if i_next => (if i + 2 == w.len() && l == 'a' { "e" } else { "ɛ" }, 2),
            'a' if next == Some('y') && self.vowel_at(i + 2) => ("ɛj", 2),
            'o' if i_next && self.nasal_after(i + 2) => ("wɛ̃", 3),
            'o' if i_next => ("wa", 2),
            'o' if next == Some('y') && self.vowel_at(i + 2) => ("waj", 2),
            'o' if matches!(next, Some('u' | 'ù' | 'û')) => {
                if starts_ill(w, i + 2) && !self.ill_said {
                    ("uj", 2 + self.ill_len(i + 2))
                } else if self.vowel_at(i + 2) && i + 2 < end {
                    ("w", 2)
                } else {
                    ("u", 2)
                }
            }
            'e' | 'œ' if next == Some('u') => {
                if starts_ill(w, i + 2) {
                    ("œj", 2 + self.ill_len(i + 2))
                } else {
                    let closed = (i + 2 >= end && !self.c.final_consonant_said) || (matches!(self.at(i + 2), Some('s' | 'z')) && self.vowel_at(i + 3));
                    (if closed { "ø" } else { "œ" }, 2)
                }
            }
            'u' if next == Some('e') && starts_ill(w, i + 2) => ("œj", 2 + self.ill_len(i + 2)),
            _ => return None,
        })
    }

    /// Nasal vowels and single vowel letters.
    fn vowel(&self, i: usize) -> Option<Step> {
        let w = self.w;
        let l = self.at(i)?;
        let next = self.at(i + 1);
        Some(match l {
            'a' | 'e' if self.nasal_after(i + 1) => {
                // bien, moyen, européen (and vient, tient): ɛ̃ after i / y / é at the end
                let word = self.word;
                let after_i = l == 'e'
                    && i > 0
                    && matches!(self.raw(i - 1), Some('i' | 'y' | 'é'))
                    && i + 2 >= self.c.end
                    && (!word.ends_with("ent") || word.ends_with("vient") || word.ends_with("tient"));
                (if after_i { "ɛ̃" } else { "ɑ̃" }, 2)
            }
            'i' | 'y' if self.nasal_after(i + 1) => ("ɛ̃", 2),
            'o' if self.nasal_after(i + 1) => ("ɔ̃", 2),
            // un, brun (Latin -um words are in the table)
            'u' if self.nasal_after(i + 1) => ("œ̃", 2),
            // fille, famille
            'i' if starts_from(w, i, "ill") && i > 0 && !self.ill_said => ("ij", 3),
            'i' | 'ï' | 'y' if i > 0 && (self.vowel_at(i + 1) || (i + 1 == self.c.end && self.c.closed_e_suffix)) => {
                // a glide before a vowel (piano, ciel); after a consonant + r / l: ij (crier)
                let cluster = i >= 2 && matches!(self.raw(i - 1), Some('r' | 'l')) && self.raw(i - 2).is_some_and(|p| !is_vowel_letter(p));
                (if cluster { "ij" } else { "j" }, 1)
            }
            'i' | 'î' | 'ï' | 'y' => ("i", 1),
            'u' | 'û' | 'ü' if self.vowel_at(i + 1) && i > 0 && !matches!(self.raw(i - 1), Some('q' | 'g')) => ("ɥ", 1),
            'u' | 'û' | 'ü' => ("y", 1),
            'ù' => ("u", 1),
            'a' | 'à' | 'â' | 'ä' => ("a", 1),
            'é' | 'œ' => ("e", 1),
            'è' | 'ê' | 'ë' => ("ɛ", 1),
            'ô' => ("o", 1),
            'o' | 'ö' => {
                // closed o at the end of the said word, before a z sound (rose) or -tion; open otherwise
                let final_open = i + 1 >= self.c.end && !self.c.final_consonant_said;
                let before_z = (next == Some('s') && self.raw(i + 2).is_some_and(is_vowel_letter)) || next == Some('z');
                (if final_open || before_z || starts_from(w, i + 1, "tion") { "o" } else { "ɔ" }, 1)
            }
            'e' => (e_sound(w, i, self.c.end, &self.c), 1),
            _ => return None,
        })
    }

    /// c, g, s, t, x: the letters whose sound depends on what follows.
    fn soft_consonant(&self, i: usize) -> Option<Step> {
        let w = self.w;
        let l = self.at(i)?;
        let next = self.at(i + 1);
        let next2 = self.at(i + 2);
        let front = |k: usize| self.raw(k).is_some_and(is_front_vowel);
        Some(match l {
            'c' if next == Some('h') => {
                let hard = matches!(next2, Some('r' | 'l')) || starts_from(w, i, "chor") || starts_from(w, i, "chao");
                (if hard { "k" } else { "ʃ" }, 2)
            }
            'c' if next == Some('c') && front(i + 2) => ("ks", 2),
            'c' if front(i + 1) => ("s", 1),
            'c' => ("k", if matches!(next, Some('k' | 'c' | 'q')) { 2 } else { 1 }),
            'q' => ("k", if next == Some('u') { 2 } else { 1 }),
            'g' if next == Some('n') => ("ɲ", 2),
            'g' if next == Some('u') && front(i + 2) => ("ɡ", 2),
            'g' if self.raw(i + 1) == Some('e') && matches!(self.raw(i + 2), Some('a' | 'o' | 'u' | 'â' | 'ô')) => ("ʒ", 2),
            'g' if front(i + 1) => ("ʒ", 1),
            'g' => ("ɡ", if next == Some('g') && !next2.is_some_and(is_front_vowel) { 2 } else { 1 }),
            's' if next == Some('h') => ("ʃ", 2),
            's' if next == Some('c') && next2 == Some('h') => ("ʃ", 3),
            's' if next == Some('c') && next2.is_some_and(is_front_vowel) => ("s", 2),
            's' if next == Some('s') => ("s", 2),
            's' if i > 0
                && self.raw(i - 1).is_some_and(is_vowel_letter)
                && (self.vowel_at(i + 1) || (i + 1 >= self.c.end && self.c.final_consonant_said && i + 1 < w.len())) =>
            {
                ("z", 1)
            }
            't' if next == Some('h') => ("t", 2),
            't' if ["tion", "tiel", "tial", "tience"].iter().any(|s| starts_from(w, i, s)) && !(i > 0 && matches!(self.raw(i - 1), Some('s' | 'x'))) => {
                ("s", 1)
            }
            'x' if i == 1 && self.raw(0) == Some('e') && (self.vowel_at(2) || self.raw(2) == Some('h')) => ("ɡz", 1),
            'x' if starts_from(w, i + 1, "ième") => ("z", 1),
            'x' => (if next == Some('c') && next2.is_some_and(is_front_vowel) { "k" } else { "ks" }, 1),
            _ => return None,
        })
    }

    /// The other consonants; doubled ones are said once.
    fn consonant(&self, i: usize) -> Step {
        let Some(l) = self.at(i) else { return ("", 1) };
        let next = self.at(i + 1);
        let twice = if next == Some(l) { 2 } else { 1 };
        match l {
            'ç' => ("s", 1),
            'k' => ("k", 1),
            'p' if next == Some('h') => ("f", 2),
            'h' => ("", 1),
            'j' => ("ʒ", 1),
            'r' => ("ʁ", if matches!(next, Some('r' | 'h')) { 2 } else { 1 }),
            'w' => (if i == 0 { "w" } else { "v" }, 1),
            'b' => ("b", twice),
            'd' => ("d", twice),
            'f' => ("f", twice),
            'l' => ("l", twice),
            'm' => ("m", twice),
            'n' => ("n", twice),
            'p' => ("p", twice),
            's' => ("s", twice),
            't' => ("t", twice),
            'v' => ("v", twice),
            'z' => ("z", twice),
            _ => ("", 1),
        }
    }
}

/// Letter-to-sound rules on `w` (the word's letters).
fn rules(w: &[char], word: &str) -> Vec<&'static str> {
    let scan = Scan { w, word, c: core(w, word), ill_said: ILL_SAID.iter().any(|s| word.starts_with(s)) };
    let mut out: Vec<&'static str> = Vec::new();
    let mut i = 0usize;
    while i < scan.c.end {
        let (said, used) = scan.vowel_group(i).or_else(|| scan.vowel(i)).or_else(|| scan.soft_consonant(i)).unwrap_or_else(|| scan.consonant(i));
        out.extend(phones_of_dyn(said));
        i += used.max(1);
    }
    if scan.c.closed_e_suffix {
        out.push("e");
    }
    out
}

/// `ill`, or a word-final `il` (travail, soleil, fauteuil), at `i`.
fn starts_ill(w: &[char], i: usize) -> bool {
    starts_from(w, i, "ill") || (starts_from(w, i, "il") && i + 2 == w.len())
}

fn starts_from(w: &[char], i: usize, s: &str) -> bool {
    s.chars().enumerate().all(|(k, ch)| w.get(i + k) == Some(&ch))
}

/// The sound of a plain `e` at `i`: é before the closed endings (handled by the core), è in a
/// closed syllable, mute ə in an open one.
fn e_sound(w: &[char], i: usize, end: usize, c: &Core) -> &'static str {
    let at = |k: usize| if k < end { w.get(k).copied() } else { None };
    let consonant = |k: usize| at(k).is_some_and(|ch| !is_vowel_letter(ch) && ch != 'h');
    // the last said letter: monosyllables (le, de, que) keep ə; elsewhere the core dropped it
    if i + 1 >= end {
        return if c.final_consonant_said || i + 1 == w.len() { "ə" } else { "ɛ" };
    }
    // before a said final consonant: sel, mer, avec, chef
    if i + 2 == end && consonant(i + 1) && c.final_consonant_said && !w.get(end).is_some_and(|&ch| ch == 'e') {
        return "ɛ";
    }
    // e + x: ɛ (exemple, texte)
    if at(i + 1) == Some('x') {
        return "ɛ";
    }
    // two consonants (or a doubled one) after e: ɛ (belle, merci, reste), except a consonant + r / l
    if consonant(i + 1) && consonant(i + 2) {
        let (a, b) = (at(i + 1), at(i + 2));
        let muta_liquida = matches!(b, Some('r' | 'l')) && matches!(a, Some('b' | 'c' | 'd' | 'f' | 'g' | 'p' | 't' | 'v'))
            || (a == Some('c') && b == Some('h'))
            || (a == Some('g') && b == Some('n'))
            || (a == Some('p') && b == Some('h'))
            || (a == Some('t') && b == Some('h'));
        if !muta_liquida {
            return "ɛ";
        }
    }
    // e + consonant ending a said word whose final e was dropped: ə only in the first
    // syllable of short words (petit, demain); otherwise ɛ before a final said consonant
    if consonant(i + 1) && i + 2 >= end {
        return if c.final_consonant_said { "ɛ" } else { "ə" };
    }
    "ə"
}

/// Phones of a rule's output (several phones in one literal).
fn phones_of_dyn(s: &str) -> Vec<&'static str> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in s.chars() {
        if ch == '\u{303}' {
            cur.push(ch);
            continue;
        }
        if !cur.is_empty() {
            out.push(ph(&cur));
        }
        cur = ch.to_string();
    }
    if !cur.is_empty() {
        out.push(ph(&cur));
    }
    out.retain(|p| !p.is_empty());
    out
}

/// Render phones with the stress mark before the last vowel.
fn render(p: &Pron) -> String {
    let last_vowel = if p.unstressed {
        None
    } else {
        p.phones.iter().rposition(|x| is_vowel_phone(x) && *x != "ə").or_else(|| p.phones.iter().rposition(|x| is_vowel_phone(x)))
    };
    let mut s = String::new();
    for (k, x) in p.phones.iter().enumerate() {
        if Some(k) == last_vowel && p.phones.iter().filter(|x| is_vowel_phone(x)).count() > 0 {
            s.push('ˈ');
        }
        s.push_str(x);
    }
    s
}

/// The phonemes of one French word, stressed (no liaison).
pub fn word(word: &str) -> String {
    render(&word_pron(word))
}

/// A word starts with a vowel sound for liaison and elision (not an aspirated h).
fn vowel_initial(word: &str, pron: &Pron) -> bool {
    let lower = word.to_lowercase();
    if H_ASPIRE.iter().any(|h| lower == *h) {
        return false;
    }
    pron.phones.first().is_some_and(|p| is_vowel_phone(p) || (*p == "j" && lower.starts_with('y')))
}

/// Phonemes of a sequence of words with liaisons: one string per word, in order.
pub fn words(ws: &[&str]) -> Vec<String> {
    let prons: Vec<Pron> = ws.iter().map(|w| word_pron(w)).collect();
    let mut out = Vec::with_capacity(ws.len());
    for (k, (w, p)) in ws.iter().zip(&prons).enumerate() {
        let mut s = render(p);
        let lower = w.to_lowercase();
        if let (Some(next_w), Some(next_p)) = (ws.get(k + 1), prons.get(k + 1))
            && vowel_initial(next_w, next_p)
            && let Some((_, cons)) = LIAISON.iter().find(|(l, _)| *l == lower.as_str())
        {
            // six / dix end in s already: the liaison voices it
            if s.ends_with('s') && *cons == "z" {
                s.pop();
            }
            s.push_str(cons);
        }
        out.push(s);
    }
    out
}

#[cfg(test)]
mod tests;
