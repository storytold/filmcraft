//! LUT resolution for Lumetri (Input LUT, Creative Look) and the built-in LUTs.
//!
//! Lumetri stores a LUT reference as text:
//! * `""` — none;
//! * `lib:<id>` — a LUT in the project's LUT library (`Project::luts`, imported `.cube`/`.3dl`);
//! * `builtin:<id>` — a LUT FilmCraft generates from code: camera log → Rec. 709 conversions
//!   (from the published curves in `filmcraft-color`; input is video-range normalised, as camera
//!   LUTs expect) and the procedural Creative looks.
//!
//! Parsed LUTs are cached by content, so a library LUT is parsed once however many clips use it.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, OnceLock};

use filmcraft_color::{ColorSpace, Lut, Lut3d, LutFormat, Range};
use filmcraft_project::{MAX_LUT_TEXT_BYTES, Project};

/// A built-in LUT: id, label, and how it is made.
pub struct Builtin {
    pub id: String,
    pub label: String,
    kind: BuiltinKind,
}

#[derive(Clone, Copy)]
enum BuiltinKind {
    LogTo709(ColorSpace),
    Look(u32),
}

/// Built-in LUTs offered in the Input LUT menu (camera conversions) and the Look menu.
pub fn builtins() -> &'static [Builtin] {
    static B: OnceLock<Vec<Builtin>> = OnceLock::new();
    B.get_or_init(|| {
        let mut v: Vec<Builtin> = ColorSpace::ALL
            .into_iter()
            .filter(|c| c.is_log())
            .map(|c| Builtin { id: format!("{}-to-rec709", c.id()), label: format!("{} to Rec. 709", c.label()), kind: BuiltinKind::LogTo709(c) })
            .collect();
        let looks = filmcraft_project::find_effect("lumetri")
            .and_then(|d| d.param("look"))
            .and_then(|p| if let filmcraft_project::ParamKind::Choice(o) = p.kind { Some(o) } else { None })
            .unwrap_or(&[]);
        for (i, name) in looks.iter().enumerate().skip(1) {
            v.push(Builtin { id: format!("look-{}", slug(name)), label: (*name).to_string(), kind: BuiltinKind::Look(i as u32) });
        }
        v
    })
}

/// The camera conversions only (Input LUT menu).
pub fn input_builtins() -> impl Iterator<Item = &'static Builtin> {
    builtins().iter().filter(|b| matches!(b.kind, BuiltinKind::LogTo709(_)))
}

fn slug(s: &str) -> String {
    let mut o = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            o.push(c.to_ascii_lowercase());
        } else if !o.ends_with('-') {
            o.push('-');
        }
    }
    o.trim_matches('-').to_string()
}

impl Builtin {
    /// Generate the LUT (33³; our own procedural work).
    pub fn generate(&self) -> Lut {
        let mut cube = match self.kind {
            BuiltinKind::LogTo709(cs) => {
                let t = filmcraft_color::InputTransform::new(cs, Range::Limited, &filmcraft_color::ColorPipeline::REC709, None);
                Lut3d::from_fn(33, |c| t.convert(c).map(|v| filmcraft_color::linear_to_srgb(v.clamp(0.0, 1.0))))
            }
            BuiltinKind::Look(n) => Lut3d::from_fn(33, |c| crate::effects::apply_look(n, c).map(|v| v.clamp(0.0, 1.0))),
        };
        cube.title = format!("FilmCraft {}", self.label);
        Lut::from_cube(cube)
    }
}

/// Identity of immutable project LUT text. Pointer equality is intentional: an `Arc<str>` changes
/// identity whenever a project edit replaces its text, while clones of an unchanged project share
/// it. Keeping the Arc in the cache also prevents allocator-address reuse from aliasing old LUTs.
#[derive(Clone, Debug)]
struct LutText(Arc<str>);

impl PartialEq for LutText {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for LutText {}

impl Hash for LutText {
    fn hash<H: Hasher>(&self, state: &mut H) {
        (Arc::as_ptr(&self.0) as *const ()).hash(state);
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum CacheKey {
    Builtin(String),
    Library { text: LutText, format: String },
}

fn cache() -> &'static Mutex<HashMap<CacheKey, Arc<Lut>>> {
    static C: OnceLock<Mutex<HashMap<CacheKey, Arc<Lut>>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

fn cached(key: CacheKey, make: impl FnOnce() -> Option<Lut>) -> Option<Arc<Lut>> {
    if let Some(l) = cache().lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        return Some(l.clone());
    }
    let l = Arc::new(make()?);
    let mut c = cache().lock().unwrap_or_else(|e| e.into_inner());
    if c.len() > 64 {
        c.clear();
    }
    c.insert(key, l.clone());
    Some(l)
}

/// Resolve a Lumetri LUT reference (see module docs). `None` for "" or unknown references.
pub fn resolve(project: Option<&Project>, spec: &str) -> Option<Arc<Lut>> {
    let spec = spec.trim();
    if let Some(id) = spec.strip_prefix("builtin:") {
        let b = builtins().iter().find(|b| b.id == id)?;
        return cached(CacheKey::Builtin(id.to_string()), || Some(b.generate()));
    }
    let id = spec.strip_prefix("lib:")?;
    let entry = project?.luts.iter().find(|l| l.id == id)?;
    if entry.text.len() > MAX_LUT_TEXT_BYTES {
        return None;
    }
    let key = CacheKey::Library { text: LutText(entry.text.clone()), format: entry.format.clone() };
    let fmt = LutFormat::from_path(&format!("x.{}", entry.format));
    cached(key, || Lut::parse(&entry.text, fmt).ok())
}

/// Human-readable name of a LUT reference (for panels).
pub fn label(project: Option<&Project>, spec: &str) -> String {
    if spec.is_empty() {
        return "None".into();
    }
    if let Some(id) = spec.strip_prefix("builtin:") {
        return builtins().iter().find(|b| b.id == id).map(|b| b.label.clone()).unwrap_or_else(|| id.to_string());
    }
    if let Some(id) = spec.strip_prefix("lib:") {
        return project.and_then(|p| p.luts.iter().find(|l| l.id == id)).map(|l| l.name.clone()).unwrap_or_else(|| format!("{id} (missing)"));
    }
    spec.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_generate_and_resolve() {
        assert!(input_builtins().count() >= 9);
        let b = builtins().iter().find(|b| b.id == "slog3-sgamut3cine-to-rec709").unwrap();
        let l = b.generate();
        // S-Log3 mid grey (code 420; the LUT sees video-range normalised values like the rest of
        // Lumetri) → ≈ 18 % linear on the sRGB curve
        let g = l.apply([(420.0 - 64.0) / 876.0; 3]);
        assert!((g[0] - filmcraft_color::linear_to_srgb(0.18)).abs() < 0.03, "{g:?}");
        assert!(resolve(None, "builtin:slog3-sgamut3cine-to-rec709").is_some());
        assert!(resolve(None, "builtin:look-teal-orange").is_some());
        assert!(resolve(None, "").is_none() && resolve(None, "lib:nope").is_none());
        // a look LUT reproduces the procedural look
        let lk = resolve(None, "builtin:look-teal-orange").unwrap();
        let c = [0.3, 0.5, 0.7];
        let (a, want) = (lk.apply(c), crate::effects::apply_look(1, c));
        assert!((0..3).all(|k| (a[k] - want[k]).abs() < 0.01), "{a:?} vs {want:?}");
    }

    #[test]
    fn library_luts_resolve_by_content() {
        let mut p = Project::new("t");
        let text = Lut::from_cube(Lut3d::from_fn(5, |c| [c[2], c[1], c[0]])).to_cube();
        p.luts.push(filmcraft_project::ProjectLut { id: "lut1".into(), name: "Swap".into(), source_path: None, format: "cube".into(), text: text.into() });
        let l = resolve(Some(&p), "lib:lut1").unwrap();
        assert_eq!(l.apply([1.0, 0.0, 0.0]), [0.0, 0.0, 1.0]);
        assert_eq!(label(Some(&p), "lib:lut1"), "Swap");
        assert_eq!(label(Some(&p), "lib:x"), "x (missing)");
    }

    #[test]
    fn library_cache_uses_retained_text_identity_without_rehashing_source() {
        let text: Arc<str> = Lut::from_cube(Lut3d::from_fn(3, |c| c)).to_cube().into();
        let a = CacheKey::Library { text: LutText(text.clone()), format: "cube".into() };
        let b = CacheKey::Library { text: LutText(text), format: "cube".into() };
        let c = CacheKey::Library { text: LutText(Arc::<str>::from(a_match_source_but_not_identity())), format: "cube".into() };
        assert_eq!(a, b, "unchanged project snapshots share their LUT identity");
        assert_ne!(a, c, "replacement text gets a new cache identity even when bytes match");
    }

    fn a_match_source_but_not_identity() -> String {
        Lut::from_cube(Lut3d::from_fn(3, |c| c)).to_cube()
    }

    #[test]
    fn oversized_embedded_lut_is_not_parsed() {
        let mut p = Project::new("t");
        p.luts.push(filmcraft_project::ProjectLut {
            id: "too-big".into(),
            name: "too-big".into(),
            source_path: None,
            format: "cube".into(),
            text: "#".repeat(MAX_LUT_TEXT_BYTES + 1).into(),
        });
        assert!(resolve(Some(&p), "lib:too-big").is_none());
    }
}
