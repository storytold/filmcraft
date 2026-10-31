//! Installed faces the interface falls back to: Hangul in every build, and Chinese, Japanese,
//! Arabic and Hebrew when the craft-fonts don't cover them (see [`crate::cjk`], [`crate::rtl`]
//! and [`crate::i18n`]).
//!
//! **Read when text needs them.** These files are large (Apple SD Gothic Neo is 53 MB) and most
//! sessions never draw a character from them, so the faces in [`FALLBACKS`] are not read at
//! start-up. [`Watch`], an egui plugin, looks at the text each pass painted, in each font family.
//! When a character is in the script of a face that hasn't been read, and no face ahead of the
//! fallbacks draws it, the face is read, the fonts are installed again and egui redoes the pass
//! before anything is shown: the first frame that shows the text already has its glyphs.
//!
//! A character in no fallback's script that nothing can draw (a symbol only an installed face
//! has) is looked up in the faces not read yet, whose character maps are read once without
//! keeping the files, and the face that has it is read: so nothing that drew before draws as a
//! box now, and a character no face has (a newer emoji) reads nothing. The interface language
//! needs no special case: its text is painted like any other.
//!
//! egui itself asks [`Provider`], an egui [`egui::FontProvider`], for a font while it lays out a
//! character no installed font has, before it would draw a box: the same decision ([`plan`]) reads
//! the face there, so even the pass that first lays the text out draws it (and a test harness that
//! treats a missing glyph as a bug sees none). egui appends such a face to the family; [`Watch`]
//! then installs the fonts again, every face read in its place in the fallback order.
//!
//! **Read once, never copied.** A face's file is read once, into the font database's face, which
//! keeps it for the whole process, and egui is given those same bytes ([`Face::shared_bytes`]) as
//! one more reference to them. egui 0.37's `FontData` holds its file as a shared blob, so egui no
//! longer copies it, but handing it a copy of the file ([`Face::data`]) meant two copies of every
//! face: the database's and ours.

use std::collections::{HashMap, HashSet};
use std::ops::RangeInclusive;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use egui::epaint::{ClippedShape, Shape};
use egui::{FontData, FontDefinitions, FontFamily, FontId};
use filmcraft_text::fonts::Face;

/// An installed face, found by characters it must cover.
pub(crate) struct SystemFace {
    /// Characters the face must cover.
    sample: &'static str,
    /// Installed families to prefer, best first.
    preferred: &'static [&'static str],
    found: OnceLock<Option<Arc<Face>>>,
}

impl SystemFace {
    pub(crate) const fn new(sample: &'static str, preferred: &'static [&'static str]) -> Self {
        Self { sample, preferred, found: OnceLock::new() }
    }

    /// The installed face (see [`find`]), looked up once per process. `None` on the web and on
    /// systems without one.
    pub(crate) fn face(&self) -> Option<&Arc<Face>> {
        self.found.get_or_init(|| find(self.sample, self.preferred)).as_ref()
    }

    /// Whether the face has been looked for and there is none.
    fn missing(&self) -> bool {
        matches!(self.found.get(), Some(None))
    }

    /// The face for egui, sharing the font database's bytes (`None` without a face, or when its
    /// file can't be read).
    pub(crate) fn font_data(&self) -> Option<Arc<FontData>> {
        shared(self.face()?)
    }
}

/// `face` for egui, sharing the bytes the font database keeps for it: no copy, however often the
/// fonts are installed. `None` when the face's file can't be read.
pub(crate) fn shared(face: &Face) -> Option<Arc<FontData>> {
    Some(Arc::new(FontData::from_blob(face.shared_bytes()?, face.info.index)))
}

/// The installed face that covers `sample`: the first family of `preferred` that has one (its
/// style closest to regular), else any installed face that does. A font collection's face index
/// matters: Windows CJK UI fonts commonly live in TTC files. Coverage is probed without keeping
/// the files ([`Face::covers_text`]), and within a family the face closest to regular is probed
/// first, so a collection is read once rather than once per face in it. Scans the system font
/// folders on first use; `None` on the web.
pub(crate) fn find(sample: &str, preferred: &[&str]) -> Option<Arc<Face>> {
    if cfg!(target_arch = "wasm32") {
        return None;
    }
    filmcraft_text::fonts::scan_system();
    let mut faces: Vec<_> = filmcraft_text::fonts::all_faces().into_iter().filter(|f| f.info.origin == "system" && !f.info.italic).collect();
    faces.sort_by_key(|f| f.info.weight.abs_diff(400));
    let covers = |f: &&Arc<Face>| f.covers_text(sample);
    let found = preferred.iter().find_map(|name| faces.iter().filter(|f| f.info.family.eq_ignore_ascii_case(name)).find(covers));
    found.or_else(|| faces.iter().find(covers)).cloned()
}

/// An installed face the interface falls back to for a script, read when text needs it.
pub(crate) struct Fallback {
    /// The egui font name.
    pub(crate) name: &'static str,
    pub(crate) face: SystemFace,
    /// The script's code points.
    script: &'static [RangeInclusive<char>],
    /// Whether this build falls back to an installed face for the script at all (false when the
    /// craft-fonts cover it).
    wanted: fn() -> bool,
    read: AtomicBool,
    /// The code points the face maps, while it isn't read (see [`Candidate::might_cover`]).
    coverage: OnceLock<Vec<(u32, u32)>>,
}

impl Fallback {
    pub(crate) const fn new(
        name: &'static str,
        sample: &'static str,
        preferred: &'static [&'static str],
        script: &'static [RangeInclusive<char>],
        wanted: fn() -> bool,
    ) -> Self {
        Self { name, face: SystemFace::new(sample, preferred), script, wanted, read: AtomicBool::new(false), coverage: OnceLock::new() }
    }

    /// The face for egui once [`Watch`] (or [`Fallback::read`]) has read it, else `None`.
    pub(crate) fn font_data(&self) -> Option<Arc<FontData>> {
        if !self.is_read() {
            return None;
        }
        self.face.font_data()
    }

    pub(crate) fn is_read(&self) -> bool {
        self.read.load(Ordering::Acquire)
    }

    /// Read the face's file (once), so that installing the fonts includes it. False when this
    /// build doesn't fall back for the script, or nothing installed covers it.
    pub(crate) fn read(&self) -> bool {
        if self.is_read() {
            return true;
        }
        if !(self.wanted)() || self.face.face().and_then(|f| f.shared_bytes()).is_none() {
            return false;
        }
        self.read.store(true, Ordering::Release);
        true
    }

    /// Nothing left to do for this fallback: read, not wanted, or not installed.
    fn settled(&self) -> bool {
        self.is_read() || !(self.wanted)() || self.face.missing()
    }

    /// Use `face` as the installed face (a bundled font standing in for one).
    #[cfg(test)]
    fn use_face(&self, face: Arc<Face>) {
        let _ = self.face.found.set(Some(face));
    }
}

/// What [`plan`] needs to know of a fallback.
trait Candidate {
    /// Whether `c` is in the script the face is the fallback for.
    fn in_script(&self, c: char) -> bool;
    fn is_read(&self) -> bool;
    /// Whether the face, once read, draws `c`.
    fn covers(&self, c: char) -> bool;
    /// Read the face; false when there is nothing to read.
    fn read(&self) -> bool;
    /// Whether the face (read or not) draws `c`. A face not read yet is looked up in its
    /// character map, read once without keeping the file.
    fn might_cover(&self, c: char) -> bool;
}

impl Candidate for Fallback {
    fn in_script(&self, c: char) -> bool {
        self.script.iter().any(|r| r.contains(&c))
    }
    fn is_read(&self) -> bool {
        Fallback::is_read(self)
    }
    fn covers(&self, c: char) -> bool {
        self.is_read() && self.face.face().is_some_and(|f| f.has_char(c))
    }
    fn read(&self) -> bool {
        Fallback::read(self)
    }
    fn might_cover(&self, c: char) -> bool {
        if self.is_read() {
            return self.covers(c);
        }
        let Some(face) = self.face.face().filter(|_| (self.wanted)()) else { return false };
        let cp = u32::from(c);
        let within = |&(a, b): &(u32, u32)| {
            if b < cp {
                std::cmp::Ordering::Less
            } else if a > cp {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        };
        self.coverage.get_or_init(|| face.coverage()).binary_search_by(within).is_ok()
    }
}

/// How the fonts a pass was laid out with draw a character.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Drawn {
    /// A face ahead of the fallbacks draws it, or egui draws nothing for it.
    early: bool,
    /// Some face draws it.
    any: bool,
}

/// Read the fallbacks (in fallback order) the painted character `c` needs, given how the fonts
/// the pass was laid out with draw it, so that the same face draws `c` as when every fallback was
/// read at start-up. Returns whether a face was read.
///
/// - Nothing is read when a face ahead of the fallbacks draws `c`.
/// - Otherwise the first fallback that has `c` draws it: one already read, or one whose script has
///   `c`, which is read (in order, until one has `c`).
/// - A character no face draws reads the first fallback not read yet whose character map has it
///   (a symbol only an installed face has), and nothing when none has (a newer emoji). One that a
///   face after the fallbacks draws (the system Japanese face) is left to it.
fn plan(c: char, drawn: Drawn, fallbacks: &[&dyn Candidate]) -> bool {
    if drawn.early {
        return false;
    }
    let mut read_any = false;
    for f in fallbacks {
        if f.is_read() {
            if f.covers(c) {
                return read_any;
            }
        } else if f.in_script(c) && f.read() {
            read_any = true;
            if f.covers(c) {
                return true;
            }
        }
    }
    if !drawn.any
        && let Some(f) = fallbacks.iter().find(|f| !f.is_read() && !f.in_script(c) && f.might_cover(c))
    {
        read_any |= f.read();
    }
    read_any
}

/// How `fonts` draws `c` in `family`, `has_glyph` being egui's answer. `late` names the faces that
/// come after the fallbacks' place in a family (the fallbacks themselves, and faces added after
/// them). egui's `has_glyph` answers no for the characters of the face that supplies its
/// replacement glyph (the ◻ of Noto Emoji, or the `?` of the first face in a family without one),
/// so the faces' character maps are asked too.
fn classify(fonts: &FontDefinitions, family: &FontFamily, c: char, has_glyph: bool, late: &[&str]) -> Drawn {
    let Some(stack) = fonts.families.get(family) else { return Drawn { early: true, any: true } };
    let maps = |late_faces: bool| {
        stack
            .iter()
            .filter(|name| late.contains(&name.as_str()) == late_faces)
            .filter_map(|name| fonts.font_data.get(name))
            .any(|data| filmcraft_text::fonts::maps_char(data.bytes(), data.index, c))
    };
    let (early, later) = (maps(false), maps(true));
    Drawn { early: early || (has_glyph && !later), any: has_glyph || early || later }
}

/// Characters that never need a fallback: ASCII, Latin-1 and Latin Extended-A (which every
/// interface font has), spacing and controls (egui draws nothing for them).
fn plain(c: char) -> bool {
    u32::from(c) < 0x180 || c.is_whitespace() || c.is_control()
}

/// The characters looked at so far, per font family (families differ in their faces): nothing more
/// to read for them. A bit per character of the Basic Multilingual Plane, a set for the others.
#[derive(Default)]
struct Settled(HashMap<FontFamily, (Vec<u64>, HashSet<char>)>);

impl Settled {
    fn insert(&mut self, family: FontFamily, c: char) {
        let (bits, rest) = self.0.entry(family).or_insert_with(|| (vec![0; 1024], HashSet::new()));
        let cp = u32::from(c);
        match bits.get_mut((cp / 64) as usize) {
            Some(word) => *word |= 1 << (cp % 64),
            None => {
                rest.insert(c);
            }
        }
    }

    #[cfg(test)]
    fn contains(&self, family: &FontFamily, c: char) -> bool {
        self.0.get(family).is_some_and(|entry| settled_in(entry, c))
    }
}

fn settled_in((bits, rest): &(Vec<u64>, HashSet<char>), c: char) -> bool {
    let cp = u32::from(c);
    match bits.get((cp / 64) as usize) {
        Some(word) => word & (1 << (cp % 64)) != 0,
        None => rest.contains(&c),
    }
}

/// The characters (with the font family drawing them) in `shapes` that are not `settled`, each
/// once. Bounded: a pass with more new characters leaves the rest to the next pass.
fn new_characters(shapes: &[ClippedShape], settled: &Settled) -> Vec<(char, FontFamily)> {
    const MAX: usize = 4096;
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let mut stack: Vec<&Shape> = shapes.iter().map(|s| &s.shape).collect();
    while let Some(shape) = stack.pop() {
        match shape {
            Shape::Vec(v) => stack.extend(v.iter()),
            Shape::Text(t) => {
                let job = &t.galley.job;
                if job.text.is_ascii() {
                    continue;
                }
                for section in &job.sections {
                    let Some(text) = job.text.get(section.byte_range.start.0..section.byte_range.end.0) else { continue };
                    let family = &section.format.font_id.family;
                    let done = settled.0.get(family);
                    for c in text.chars().filter(|c| !plain(*c) && !done.is_some_and(|d| settled_in(d, *c))) {
                        if out.len() >= MAX {
                            return out;
                        }
                        if seen.insert((c, family.clone())) {
                            out.push((c, family.clone()));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// The interface's fallbacks for scripts its own fonts lack, in fallback order: the order they
/// come in each font family (`theme::font_definitions`).
pub(crate) static FALLBACKS: [&Fallback; 4] = [&crate::cjk::CHINESE, &crate::cjk::KOREAN, &crate::rtl::ARABIC, &crate::rtl::HEBREW];

/// The egui font provider that reads, during layout, the installed fallback a character no
/// installed font has needs (see the module docs). [`Watch`] adds it.
struct Provider {
    fallbacks: &'static [&'static Fallback],
}

impl egui::FontProvider for Provider {
    /// egui asks only when no installed font has the character, so it is a character nothing
    /// draws: read what [`plan`] reads for it, and hand egui the first face read that has it.
    fn font_for(&self, request: &egui::FallbackRequest<'_>) -> Option<egui::FontInsert> {
        let c = request.cluster.chars().next()?;
        let candidates: Vec<&dyn Candidate> = self.fallbacks.iter().map(|f| *f as &dyn Candidate).collect();
        plan(c, Drawn { early: false, any: false }, &candidates);
        let fallback = self.fallbacks.iter().find(|f| f.covers(c))?;
        Some(egui::FontInsert { name: fallback.name.to_owned(), data: Arc::unwrap_or_clone(fallback.font_data()?), families: Vec::new() })
    }
}

/// The egui plugin that reads [`FALLBACKS`] when painted text needs them (see the module docs).
/// `theme::install` adds it.
pub(crate) struct Watch {
    fallbacks: &'static [&'static Fallback],
    /// Installs the fonts again, with the fallbacks read so far.
    install: fn(&egui::Context),
    /// The faces that come after the fallbacks' place in a family (see [`classify`]).
    late: Vec<&'static str>,
    settled: Settled,
    /// How many faces [`Provider`] had handed egui when the fonts were last installed again for
    /// them (egui forgets them when the fonts change; if they didn't, nothing more to do).
    discovered: usize,
    /// When the last read started, to log how long the redone pass took.
    #[cfg(not(target_arch = "wasm32"))]
    started: Option<std::time::Instant>,
}

impl Watch {
    pub(crate) fn new() -> Self {
        let mut watch = Self::with(&FALLBACKS, reinstall);
        // the system Japanese face a Japanese interface adds after the fallbacks
        watch.late.push(crate::i18n::JAPANESE_FONT);
        watch
    }

    fn with(fallbacks: &'static [&'static Fallback], install: fn(&egui::Context)) -> Self {
        Self {
            fallbacks,
            install,
            late: fallbacks.iter().map(|f| f.name).collect(),
            settled: Settled::default(),
            discovered: 0,
            #[cfg(not(target_arch = "wasm32"))]
            started: None,
        }
    }
}

/// Redo the pass the fonts were just installed again for before anything is shown. egui allows a
/// limited number of passes per frame: when this was the last, the next frame shows the result.
fn redo_pass(ctx: &egui::Context, output: &mut egui::FullOutput, reason: &'static str) {
    if output.platform_output.num_completed_passes < ctx.options(|o| o.max_passes.get()) {
        output.platform_output.request_discard_reasons.push(egui::RepaintCause::new_reason(reason));
    } else {
        ctx.request_repaint();
    }
}

impl egui::Plugin for Watch {
    fn debug_name(&self) -> &'static str {
        "filmcraft fallback fonts"
    }

    /// A pass a `Grid` or an area's sizing already had redone can still be redone once more for a
    /// face read in it, so its text doesn't show boxes for a frame.
    fn setup(&mut self, ctx: &egui::Context) {
        if let Some(three) = std::num::NonZeroUsize::new(3) {
            ctx.options_mut(|o| o.max_passes = o.max_passes.max(three));
        }
        ctx.add_font_provider(Arc::new(Provider { fallbacks: self.fallbacks }));
    }

    fn output_hook(&mut self, ctx: &egui::Context, output: &mut egui::FullOutput) {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(started) = self.started.take() {
            log::info!("fonts: the pass redone with the new fallback face took {:.1} ms in all", started.elapsed().as_secs_f64() * 1e3);
        }
        let discovered = ctx.fonts_mut(|f| f.discovered_fonts().len());
        if discovered != self.discovered {
            self.discovered = discovered;
            if discovered > 0 {
                // [`Provider`] read a face during layout and egui appended it to the family: install
                // the fonts again, the face in its place, and look at this text with them
                (self.install)(ctx);
                redo_pass(ctx, output, "put an installed fallback face in its place");
                return;
            }
        }
        if self.fallbacks.iter().all(|f| f.settled()) {
            return;
        }
        let chars = new_characters(&output.shapes, &self.settled);
        if chars.is_empty() {
            return;
        }
        #[cfg(not(target_arch = "wasm32"))]
        let started = std::time::Instant::now();
        let candidates: Vec<&dyn Candidate> = self.fallbacks.iter().map(|f| *f as &dyn Candidate).collect();
        let mut read = false;
        for (c, family) in chars {
            // the fonts this pass was laid out with (new ones apply from the next pass)
            let late = &self.late;
            let drawn = ctx.fonts_mut(|f| {
                if !f.definitions().families.contains_key(&family) {
                    return Drawn { early: true, any: true }; // (painted text always has a family)
                }
                let has_glyph = f.has_glyph(&FontId::new(12.0, family.clone()), c);
                classify(f.definitions(), &family, c, has_glyph, late)
            });
            read |= plan(c, drawn, &candidates);
            self.settled.insert(family, c);
        }
        if !read {
            return;
        }
        (self.install)(ctx);
        #[cfg(not(target_arch = "wasm32"))]
        {
            let names: Vec<&str> = self.fallbacks.iter().filter(|f| f.is_read()).map(|f| f.name).collect();
            log::info!("fonts: read installed fallback faces {names:?} in {:.1} ms", started.elapsed().as_secs_f64() * 1e3);
            self.started = Some(started);
        }
        redo_pass(ctx, output, "read an installed fallback face");
    }
}

/// Install the theme's fonts again with the fallbacks read so far, keeping faces added on top of
/// them (the system Japanese face, `i18n::install_japanese_font`) where they were: at the end of
/// their families.
fn reinstall(ctx: &egui::Context) {
    let mut fonts = crate::theme::font_definitions();
    let fallbacks: Vec<&str> = FALLBACKS.iter().map(|f| f.name).collect();
    keep_added(&mut fonts, &ctx.fonts_mut(|f| f.definitions().clone()), &fallbacks);
    ctx.set_fonts(fonts);
}

/// Add to `fonts` the faces `current` has that it lacks, at the end of the families they are in,
/// except `fallbacks`: where they go, and whether (a face both right-to-left fallbacks found is
/// installed once), is `fonts`' to say.
fn keep_added(fonts: &mut FontDefinitions, current: &FontDefinitions, fallbacks: &[&str]) {
    for (name, data) in &current.font_data {
        if fonts.font_data.contains_key(name) || fallbacks.contains(&name.as_str()) {
            continue;
        }
        fonts.font_data.insert(name.clone(), data.clone());
        for (family, stack) in &current.families {
            if stack.contains(name) {
                fonts.families.entry(family.clone()).or_default().push(name.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// The installed fallbacks reach egui without a copy: every install hands egui the bytes the
    /// font database keeps, at the same address.
    #[test]
    fn installed_faces_are_shared_not_copied() {
        static INSTALLED: SystemFace = SystemFace::new("A", &["Arial", "Helvetica", "DejaVu Sans", "Segoe UI"]);
        let Some(face) = INSTALLED.face() else {
            eprintln!("SKIPPED: no installed Latin font");
            return;
        };
        let (Some(a), Some(b)) = (INSTALLED.font_data(), INSTALLED.font_data()) else {
            panic!("the installed face's file could not be read");
        };
        let Some(kept) = face.shared_bytes() else { panic!("the database keeps no bytes for the face") };
        for data in [&a, &b] {
            assert_eq!(data.bytes().as_ptr(), (*kept).as_ref().as_ptr(), "egui must see the database's own bytes, not a copy");
            assert_eq!(data.index, face.info.index);
        }
    }

    /// A fallback for [`plan`]: its script, what it covers, whether it is installed, and how often
    /// it was read.
    struct Fake {
        script: RangeInclusive<char>,
        covers: &'static str,
        installed: bool,
        read: Cell<bool>,
        reads: Cell<u32>,
    }

    impl Fake {
        fn new(script: RangeInclusive<char>, covers: &'static str) -> Self {
            Self { script, covers, installed: true, read: Cell::new(false), reads: Cell::new(0) }
        }
        fn read_already(self) -> Self {
            self.read.set(true);
            self
        }
    }

    impl Candidate for Fake {
        fn in_script(&self, c: char) -> bool {
            self.script.contains(&c)
        }
        fn is_read(&self) -> bool {
            self.read.get()
        }
        fn covers(&self, c: char) -> bool {
            self.read.get() && self.covers.contains(c)
        }
        fn read(&self) -> bool {
            if !self.installed {
                return false;
            }
            if !self.read.get() {
                self.reads.set(self.reads.get() + 1);
            }
            self.read.set(true);
            true
        }
        fn might_cover(&self, c: char) -> bool {
            self.installed && self.covers.contains(c)
        }
    }

    const HANGUL: RangeInclusive<char> = '\u{ac00}'..='\u{d7a3}';
    const HAN: RangeInclusive<char> = '\u{4e00}'..='\u{9fff}';

    /// Drawn by a face ahead of the fallbacks; by a face after them only; by none.
    const EARLY: Drawn = Drawn { early: true, any: true };
    const LATE: Drawn = Drawn { early: false, any: true };
    const NONE: Drawn = Drawn { early: false, any: false };

    #[test]
    fn text_the_fonts_draw_reads_nothing() {
        let korean = Fake::new(HANGUL, "한국");
        assert!(!plan('Ж', EARLY, &[&korean]));
        // Hangul the craft-fonts' Chinese face already draws, ahead of the Korean fallback
        assert!(!plan('한', EARLY, &[&korean]));
        assert_eq!(korean.reads.get(), 0);
    }

    #[test]
    fn text_nothing_draws_reads_its_scripts_face() {
        let chinese = Fake::new(HAN, "中文");
        let korean = Fake::new(HANGUL, "한국");
        assert!(plan('한', NONE, &[&chinese, &korean]));
        assert_eq!((chinese.reads.get(), korean.reads.get()), (0, 1), "only the Korean face");
        // once read, the face draws the rest of the script: nothing more to read
        assert!(!plan('국', NONE, &[&chinese, &korean]));
        assert_eq!((chinese.reads.get(), korean.reads.get()), (0, 1));
    }

    #[test]
    fn faces_for_a_script_are_read_in_order_until_one_covers_the_character() {
        let sparse = Fake::new(HAN, "中");
        let full = Fake::new(HAN, "中文");
        let after = Fake::new(HAN, "中文");
        assert!(plan('文', NONE, &[&sparse, &full, &after]));
        assert_eq!((sparse.reads.get(), full.reads.get(), after.reads.get()), (1, 1, 0));
    }

    /// The fallback order decides which face draws a character: when only a face that comes later
    /// draws it (a fallback read for its own script, or the system Japanese face added after the
    /// fallbacks), the earlier one is read so it draws it, as it did when every face was read at
    /// start-up.
    #[test]
    fn an_earlier_face_is_read_when_a_later_one_draws_its_script() {
        let chinese = Fake::new(HAN, "中文");
        let korean = Fake::new(HANGUL, "한국中").read_already();
        assert!(plan('中', LATE, &[&chinese, &korean]));
        assert_eq!(chinese.reads.get(), 1);
        let chinese = Fake::new(HAN, "中文");
        assert!(plan('中', LATE, &[&chinese]), "drawn by a face added after the fallbacks");
        // the other way round, nothing is read: the earlier face already wins
        let chinese = Fake::new(HAN, "中文").read_already();
        let korean = Fake::new(HANGUL, "한국中");
        assert!(!plan('中', LATE, &[&chinese, &korean]));
        assert_eq!(korean.reads.get(), 0);
    }

    /// A character in no fallback's script that nothing draws reads the face that has it (a symbol
    /// only an installed face has), and nothing when no face has it (a newer emoji, Ethiopic) or
    /// when a face after the fallbacks draws it.
    #[test]
    fn a_character_in_no_script_reads_only_the_face_that_has_it() {
        let chinese = Fake::new(HAN, "中");
        let korean = Fake::new(HANGUL, "한⇢↲");
        let hebrew = Fake::new('\u{5d0}'..='\u{5ea}', "א");
        assert!(!plan('↲', LATE, &[&chinese, &korean, &hebrew]));
        assert!(plan('⇢', NONE, &[&chinese, &korean, &hebrew]));
        assert_eq!((chinese.reads.get(), korean.reads.get(), hebrew.reads.get()), (0, 1, 0));
        assert!(!plan('\u{1fae0}', NONE, &[&chinese, &korean, &hebrew]));
        assert!(!plan('\u{1200}', NONE, &[&chinese, &korean, &hebrew]));
        assert_eq!((chinese.reads.get(), korean.reads.get(), hebrew.reads.get()), (0, 1, 0), "nothing has them: nothing read");
    }

    #[test]
    fn a_face_that_is_not_installed_is_skipped() {
        let mut missing = Fake::new(HANGUL, "한");
        missing.installed = false;
        let other = Fake::new(HAN, "中");
        assert!(!plan('한', NONE, &[&missing]));
        assert!(plan('中', NONE, &[&missing, &other]));
        assert_eq!(other.reads.get(), 1);
    }

    /// Characters are settled per font family: one drawn in a family whose faces have it may still
    /// need a fallback in another (a ★ Noto Emoji draws in the proportional family, but not in
    /// the semibold one, which has no Noto Emoji).
    #[test]
    fn characters_are_settled_per_family() {
        let semibold = FontFamily::Name("semibold".into());
        let mut settled = Settled::default();
        settled.insert(FontFamily::Proportional, '★');
        settled.insert(FontFamily::Proportional, '\u{1fae0}');
        assert!(settled.contains(&FontFamily::Proportional, '★'));
        assert!(settled.contains(&FontFamily::Proportional, '\u{1fae0}'));
        assert!(!settled.contains(&semibold, '★'));
        assert!(!settled.contains(&FontFamily::Proportional, '☆'));
    }

    #[test]
    fn only_new_characters_are_collected() {
        let ctx = egui::Context::default();
        ctx.run_ui(egui::RawInput::default(), |_| {}).textures_delta.clear();
        let font = FontId::proportional(12.0);
        let galley = ctx.fonts_mut(|f| f.layout_no_wrap("Hello Ǆ ɕ 한국 한 ñ".into(), font.clone(), egui::Color32::WHITE));
        let text = Shape::galley(egui::Pos2::ZERO, galley, egui::Color32::WHITE);
        let shapes = vec![ClippedShape::new(egui::Rect::EVERYTHING, Shape::Vec(vec![text]))];
        let chars: Vec<char> = new_characters(&shapes, &Settled::default()).into_iter().map(|(c, _)| c).collect();
        assert_eq!(chars, ['Ǆ', 'ɕ', '한', '국'], "each new character once; ASCII, Latin-1 and spaces skipped");
        let mut settled = Settled::default();
        settled.insert(FontFamily::Proportional, '한');
        let chars: Vec<char> = new_characters(&shapes, &settled).into_iter().map(|(c, _)| c).collect();
        assert_eq!(chars, ['Ǆ', 'ɕ', '국']);
    }

    /// egui's `has_glyph` answers no for what Noto Emoji (the face of its replacement glyph)
    /// draws: an emoji counts as drawn anyway, by a face ahead of the fallbacks unless it is a
    /// fallback that draws it.
    #[test]
    fn classify_finds_the_face_that_draws_a_character() {
        let fonts = FontDefinitions::default();
        let p = FontFamily::Proportional;
        assert_eq!(classify(&fonts, &p, '\u{1f3ac}', false, &[]), Drawn { early: true, any: true }, "🎬 is in Noto Emoji");
        let all_late = ["Ubuntu-Light", "NotoEmoji-Regular", "emoji-icon-font"];
        assert_eq!(classify(&fonts, &p, '\u{1f3ac}', false, &all_late), Drawn { early: false, any: true }, "drawn, but only by late faces");
        assert_eq!(classify(&fonts, &p, '한', false, &[]), Drawn { early: false, any: false });
        assert_eq!(classify(&fonts, &p, '\u{200d}', true, &[]), Drawn { early: true, any: true }, "egui draws nothing for a joiner");
        assert_eq!(classify(&fonts, &FontFamily::Name("none".into()), 'A', false, &[]), Drawn { early: true, any: true });
    }

    /// Cyrillic Supplement: the interface fonts lack it and the bundled Noto Serif has it, so Noto
    /// Serif stands in for an installed face.
    const CYRILLIC_SUPPLEMENT: RangeInclusive<char> = '\u{500}'..='\u{52f}';
    const SUPPLEMENT: &[RangeInclusive<char>] = &[CYRILLIC_SUPPLEMENT];

    fn noto_serif() -> Arc<Face> {
        let ids = filmcraft_text::fonts::add_font_data(filmcraft_text::fonts::NOTO_SERIF_REGULAR.to_vec());
        filmcraft_text::fonts::face(ids[0])
    }

    fn yes() -> bool {
        true
    }

    /// Inter and egui's own fonts, then `fallbacks` once read: the theme's fonts without the
    /// installed faces other tests may have read.
    fn install_with(ctx: &egui::Context, fallbacks: &[&'static Fallback]) {
        let mut fonts = FontDefinitions::default();
        fonts.font_data.insert("inter".into(), Arc::new(FontData::from_static(filmcraft_text::fonts::INTER_REGULAR)));
        fonts.families.entry(FontFamily::Proportional).or_default().insert(0, "inter".into());
        for f in fallbacks {
            if let Some(data) = f.font_data() {
                fonts.font_data.insert(f.name.into(), data);
                for stack in fonts.families.values_mut() {
                    stack.push(f.name.into());
                }
            }
        }
        ctx.set_fonts(fonts);
    }

    /// One frame showing `text` in a label: how many passes it took, and whether every glyph of
    /// the label it shows is real (not the missing-glyph box).
    fn frame(ctx: &egui::Context, text: &str) -> (usize, bool) {
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            ui.label(text);
        });
        out.textures_delta.clear();
        let galley = out.shapes.iter().find_map(|s| match &s.shape {
            Shape::Text(t) if t.galley.job.text == text => Some(t.galley.clone()),
            _ => None,
        });
        let Some(galley) = galley else { panic!("the label is painted") };
        let font = galley.job.sections[0].format.font_id.clone();
        let missing = ctx.fonts_mut(|f| f.layout_no_wrap("\u{10ffff}".into(), font, egui::Color32::WHITE).rows[0].glyphs[0].uv_rect);
        let real = galley.rows.iter().flat_map(|r| &r.glyphs).filter(|g| g.chr != ' ').all(|g| g.uv_rect != missing && !g.uv_rect.is_nothing());
        (out.platform_output.num_completed_passes, real)
    }

    /// The first frame that shows text in a fallback's script already draws it: the face is read
    /// during that frame and egui redoes the pass with it before anything is shown.
    #[test]
    fn the_first_frame_draws_text_whose_face_was_not_read() {
        static FACE: Fallback = Fallback::new("test-supplement", "Ԁ", &[], SUPPLEMENT, yes);
        static ALL: [&Fallback; 1] = [&FACE];
        fn install(ctx: &egui::Context) {
            install_with(ctx, &ALL);
        }
        FACE.use_face(noto_serif());
        let ctx = egui::Context::default();
        install(&ctx);
        ctx.add_plugin(Watch::with(&ALL, install));
        assert!(!FACE.is_read(), "nothing read before text needs it");
        let (passes, real) = frame(&ctx, "Ԁԁ Ԃԃ");
        assert!(FACE.is_read());
        assert_eq!(passes, 2, "the pass is redone with the face");
        assert!(real, "the first frame shows real glyphs");
        assert_eq!(frame(&ctx, "Ԁԁ Ԃԃ"), (1, true), "nothing more to read");
    }

    #[test]
    fn text_the_interface_fonts_draw_reads_no_face() {
        static FACE: Fallback = Fallback::new("test-supplement", "Ԁ", &[], SUPPLEMENT, yes);
        static ALL: [&Fallback; 1] = [&FACE];
        fn install(ctx: &egui::Context) {
            install_with(ctx, &ALL);
        }
        FACE.use_face(noto_serif());
        let ctx = egui::Context::default();
        install(&ctx);
        ctx.add_plugin(Watch::with(&ALL, install));
        assert_eq!(frame(&ctx, "Timeline Привет Ελληνικά 🎬"), (1, true));
        assert!(!FACE.is_read());
    }

    /// A symbol in no fallback's script that only a face not read yet has (Noto Serif's archaic
    /// Greek here; Apple SD Gothic Neo's arrows in the app) still draws on its first frame.
    #[test]
    fn a_character_only_an_unread_face_has_reads_it() {
        static FACE: Fallback = Fallback::new("test-supplement", "Ԁ", &[], SUPPLEMENT, yes);
        static ALL: [&Fallback; 1] = [&FACE];
        fn install(ctx: &egui::Context) {
            install_with(ctx, &ALL);
        }
        FACE.use_face(noto_serif());
        let ctx = egui::Context::default();
        install(&ctx);
        ctx.add_plugin(Watch::with(&ALL, install));
        assert_eq!(frame(&ctx, "Ϙϙ"), (2, true));
        assert!(FACE.is_read());
    }

    /// egui asks [`Provider`] for a character no installed font has while it lays the text out: the
    /// face is read there, so even the first pass draws it (a missing-glyph policy that panics, as
    /// the UI test harness's does, sees no box), and the watch then installs the fonts again with
    /// the face in its place in the fallback order, not where egui appended it.
    #[test]
    fn a_face_read_during_layout_draws_the_first_pass_and_takes_its_place() {
        static FACE: Fallback = Fallback::new("test-supplement", "Ԁ", &[], SUPPLEMENT, yes);
        static ALL: [&Fallback; 1] = [&FACE];
        fn install(ctx: &egui::Context) {
            install_with(ctx, &ALL);
        }
        FACE.use_face(noto_serif());
        let ctx = egui::Context::default();
        install(&ctx);
        ctx.add_plugin(Watch::with(&ALL, install));
        ctx.set_missing_glyph_policy(egui::MissingGlyphPolicy::Panic);
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            ui.label("Ԁԁ Ԃԃ");
        });
        out.textures_delta.clear();
        assert!(FACE.is_read());
        assert_eq!(out.platform_output.num_completed_passes, 2, "the pass is redone with the face in its place");
        let placed = ctx.fonts_mut(|f| (f.discovered_fonts().len(), f.definitions().families[&FontFamily::Proportional].last().cloned()));
        assert_eq!(placed, (0, Some(FACE.name.to_owned())), "installed by the theme, not appended by egui");
        ctx.set_missing_glyph_policy(egui::MissingGlyphPolicy::Tofu);
        assert_eq!(frame(&ctx, "Ԁԁ Ԃԃ"), (1, true), "nothing more to read or move");
    }

    /// A character no face has (a newer emoji than Noto Emoji's, Ethiopic here) reads nothing: it
    /// draws as a box as it always did, and the faces stay unread.
    #[test]
    fn a_character_no_face_has_reads_nothing() {
        static FACE: Fallback = Fallback::new("test-supplement", "Ԁ", &[], SUPPLEMENT, yes);
        static ALL: [&Fallback; 1] = [&FACE];
        fn install(ctx: &egui::Context) {
            install_with(ctx, &ALL);
        }
        FACE.use_face(noto_serif());
        let ctx = egui::Context::default();
        install(&ctx);
        ctx.add_plugin(Watch::with(&ALL, install));
        assert_eq!(frame(&ctx, "ሀሁ").0, 1);
        assert!(!FACE.is_read());
    }

    #[test]
    fn faces_added_on_top_of_the_theme_stay() {
        let mut current = FontDefinitions::default();
        current.font_data.insert("system-japanese".into(), Arc::new(FontData::from_static(filmcraft_text::fonts::INTER_REGULAR)));
        current.families.entry(FontFamily::Proportional).or_default().push("system-japanese".into());
        let mut fonts = FontDefinitions::default();
        current.font_data.insert("system-hebr".into(), Arc::new(FontData::from_static(filmcraft_text::fonts::INTER_REGULAR)));
        keep_added(&mut fonts, &current, &["system-hebr"]);
        assert!(!fonts.font_data.contains_key("system-hebr"), "a fallback's place is the theme's to say");
        assert!(fonts.font_data.contains_key("system-japanese"));
        assert_eq!(fonts.families[&FontFamily::Proportional].last().map(String::as_str), Some("system-japanese"));
        assert!(!fonts.families[&FontFamily::Monospace].iter().any(|n| n == "system-japanese"));
    }
}
