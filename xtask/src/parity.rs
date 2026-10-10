//! `cargo xtask parity [--reference FILE] [--ours FILE] [--out FILE] [--min PERCENT]`: measured
//! menu parity (docs/gaps.md G7).
//!
//! Compares FilmCraft's shipped menu bar (`crates/ui-egui/examples/menu_tree.rs`, or a saved dump
//! of it given with `--ours`) with a reference list of menu paths, and reports presence and
//! placement separately: a reference item is *same place* when we have its label at the same menu
//! path, *elsewhere* when we have the label under another path, and *missing* otherwise. A label
//! match is presence only, not behaviour.
//!
//! The reference defaults to the maintainers' local menu snapshot `plan/premiere/menus.json`
//! (gitignored, never committed). Without one the tool prints our own menu inventory and measures
//! nothing. The reference may be any JSON of menu paths: an array of paths (`["File", "New",
//! "Sequence…"]`) or of `"File > New > Sequence…"` strings, a tree of nodes (`{"name": …,
//! "children" | "items" | "submenu": […]}`), or nested objects keyed by label. App, Apple and Help
//! menus, recent-file lists, account / cloud items and macOS system items are counted out of scope.
//!
//! The report goes to `target/parity/parity-checklist.md` (or `--out`); `--min` fails when presence
//! is below the given percentage.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;

/// Deepest menu nesting accepted from a reference file.
const MAX_DEPTH: usize = 16;
/// Most menu items accepted from a reference file.
const MAX_ITEMS: usize = 20_000;
/// Keys holding a node's label, and its children.
const LABEL_KEYS: [&str; 3] = ["name", "title", "label"];
const CHILD_KEYS: [&str; 6] = ["children", "items", "submenu", "menu", "menuItems", "menu_items"];
/// Keys of a wrapper object around the whole menu bar.
const WRAPPER_KEYS: [&str; 5] = ["menus", "menuBar", "menu_bar", "menubar", "items"];

struct Args {
    reference: Option<PathBuf>,
    ours: Option<PathBuf>,
    out: Option<PathBuf>,
    min: Option<f64>,
}

fn parse_args(args: &[&str]) -> Result<Args, String> {
    let mut a = Args { reference: None, ours: None, out: None, min: None };
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let mut value = || it.next().map(|v| v.to_string()).ok_or_else(|| format!("{flag} needs a value"));
        match *flag {
            "--reference" => a.reference = Some(value()?.into()),
            "--ours" => a.ours = Some(value()?.into()),
            "--out" => a.out = Some(value()?.into()),
            "--min" => {
                let v = value()?;
                let pct: f64 = v.parse().map_err(|_| format!("--min {v}: not a number"))?;
                if !(0.0..=100.0).contains(&pct) {
                    return Err(format!("--min {v}: must be 0–100"));
                }
                a.min = Some(pct);
            }
            other => {
                return Err(format!(
                    "parity: unknown argument {other} (usage: cargo xtask parity [--reference FILE] [--ours FILE] [--out FILE] [--min PERCENT])"
                ));
            }
        }
    }
    Ok(a)
}

pub fn run(root: &Path, args: &[&str]) -> Result<(), String> {
    let args = parse_args(args)?;
    let ours_json = match &args.ours {
        Some(p) => std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?,
        None => dump_ours()?,
    };
    let ours = our_items(&serde_json::from_str(&ours_json).map_err(|e| format!("our menu dump: {e}"))?)?;
    let default_ref = root.join("plan/premiere/menus.json");
    let reference = match &args.reference {
        Some(p) => Some(p.clone()),
        None => default_ref.exists().then_some(default_ref),
    };
    let Some(reference) = reference else {
        print!("{}", inventory(&ours));
        println!("\nno reference menu list (plan/premiere/menus.json, or --reference FILE): menu parity not measured");
        return match args.min {
            Some(_) => Err("--min needs a reference menu list".into()),
            None => Ok(()),
        };
    };
    let text = std::fs::read_to_string(&reference).map_err(|e| format!("{}: {e}", reference.display()))?;
    let value: Value = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", reference.display()))?;
    let paths = reference_paths(&value)?;
    let name = reference.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let report = compare(&paths, &ours);
    let out = args.out.unwrap_or_else(|| crate::target_dir().join("parity/parity-checklist.md"));
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(&out, report.markdown(&name, ours.len())).map_err(|e| format!("{}: {e}", out.display()))?;
    println!("{}", report.summary());
    println!("report: {}", out.display());
    match args.min {
        Some(min) if report.presence() < min => Err(format!("menu presence {:.1}% is below --min {min}%", report.presence())),
        _ => Ok(()),
    }
}

/// Our menu bar as JSON, from the `menu_tree` example.
fn dump_ours() -> Result<String, String> {
    let mut cmd = Command::new(env!("CARGO"));
    cmd.args(["run", "-q", "-p", "filmcraft-ui-egui", "--example", "menu_tree"]).stderr(Stdio::inherit());
    let out = cmd.output().map_err(|e| format!("menu_tree: {e}"))?;
    if !out.status.success() {
        return Err(format!("menu_tree failed: {}", out.status));
    }
    String::from_utf8(out.stdout).map_err(|e| format!("menu_tree: {e}"))
}

/// Full paths (menu path + label) of our menu items, from the `menu_tree` dump.
fn our_items(v: &Value) -> Result<Vec<Vec<String>>, String> {
    let items = v.as_array().ok_or("our menu dump is not an array")?;
    Ok(items
        .iter()
        .filter_map(|it| {
            let label = it.get("label")?.as_str()?;
            let mut path: Vec<String> = it.get("path")?.as_array()?.iter().filter_map(|p| p.as_str().map(str::to_string)).collect();
            path.push(label.to_string());
            (path.len() >= 2).then_some(path)
        })
        .collect())
}

/// Item counts per top-level menu, for runs without a reference.
fn inventory(ours: &[Vec<String>]) -> String {
    let mut per: BTreeMap<&str, usize> = BTreeMap::new();
    for p in ours {
        if let Some(top) = p.first() {
            *per.entry(top.as_str()).or_default() += 1;
        }
    }
    let mut s = format!("FilmCraft menu bar: {} items\n", ours.len());
    for (top, n) in per {
        s.push_str(&format!("  {top:<22} {n}\n"));
    }
    s
}

/// Leaf menu paths (top-level menu first, item label last) of a reference file.
fn reference_paths(v: &Value) -> Result<Vec<Vec<String>>, String> {
    let mut root = v;
    if let Value::Object(o) = v
        && label_of(o).is_none()
        && let Some(inner) = WRAPPER_KEYS.iter().find_map(|k| o.get(*k).filter(|c| c.is_array() || c.is_object()))
    {
        root = inner;
    }
    let mut out = Vec::new();
    walk(root, &[], 0, &mut out)?;
    Ok(out)
}

fn label_of(o: &serde_json::Map<String, Value>) -> Option<&str> {
    LABEL_KEYS.iter().find_map(|k| o.get(*k)?.as_str())
}

/// A separator or placeholder rather than a menu item.
fn is_separator(label: &str) -> bool {
    let l = label.trim();
    l.is_empty() || l.chars().all(|c| matches!(c, '-' | '—' | '–' | '_')) || l.eq_ignore_ascii_case("missing value") || l.eq_ignore_ascii_case("separator")
}

fn push_leaf(prefix: &[String], label: &str, out: &mut Vec<Vec<String>>) -> Result<(), String> {
    if is_separator(label) || prefix.iter().any(|p| is_separator(p)) {
        return Ok(());
    }
    if out.len() >= MAX_ITEMS {
        return Err(format!("reference has more than {MAX_ITEMS} menu items"));
    }
    let mut path = prefix.to_vec();
    path.push(label.trim().to_string());
    if path.len() >= 2 {
        out.push(path);
    }
    Ok(())
}

fn split_path(s: &str) -> Option<Vec<String>> {
    let parts: Vec<String> = s.replace('\u{a0}', " ").split(['>', '▸']).map(|p| p.trim().to_string()).collect();
    (parts.len() >= 2 && parts.iter().all(|p| !p.is_empty())).then_some(parts)
}

fn walk(v: &Value, prefix: &[String], depth: usize, out: &mut Vec<Vec<String>>) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Err(format!("reference menus are nested deeper than {MAX_DEPTH} levels"));
    }
    let with = |label: &str| {
        let mut p = prefix.to_vec();
        p.push(label.trim().to_string());
        p
    };
    match v {
        Value::String(s) => match split_path(s).filter(|_| prefix.is_empty()) {
            Some(parts) => match parts.split_last() {
                Some((label, menus)) => push_leaf(menus, label, out),
                None => Ok(()),
            },
            None => push_leaf(prefix, s, out),
        },
        Value::Array(a) => {
            for el in a {
                match el {
                    Value::Array(path) if !path.is_empty() && path.iter().all(Value::is_string) => {
                        let parts: Vec<String> = prefix.iter().cloned().chain(path.iter().filter_map(|p| p.as_str().map(str::to_string))).collect();
                        if let Some((label, menus)) = parts.split_last() {
                            push_leaf(menus, label, out)?;
                        }
                    }
                    _ => walk(el, prefix, depth + 1, out)?,
                }
            }
            Ok(())
        }
        Value::Object(o) => match label_of(o) {
            Some(label) => {
                let children = CHILD_KEYS.iter().find_map(|k| o.get(*k).filter(|c| has_entries(c)));
                match children {
                    Some(c) if !is_separator(label) => walk(c, &with(label), depth + 1, out),
                    Some(_) => Ok(()),
                    None => push_leaf(prefix, label, out),
                }
            }
            None => {
                for (k, val) in o {
                    if has_entries(val) {
                        walk(val, &with(k), depth + 1, out)?;
                    } else {
                        push_leaf(prefix, k, out)?;
                    }
                }
                Ok(())
            }
        },
        _ => Ok(()),
    }
}

fn has_entries(v: &Value) -> bool {
    match v {
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
        _ => false,
    }
}

/// A label compared case-insensitively, without ellipsis, with `&` as "and" and one space.
fn norm(label: &str) -> String {
    let l = label.to_lowercase().replace('…', "").replace("...", "").replace(['’', '‘'], "'").replace('&', " and ");
    l.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Why a reference item is not counted, if it isn't.
fn out_of_scope(path: &[String]) -> Option<&'static str> {
    let (top, label) = (norm(path.first()?), norm(path.last()?));
    if top == "apple" || top == "help" || top.contains("premiere") {
        return Some("app, Apple and Help menus");
    }
    let parents = path.get(..path.len().saturating_sub(1)).unwrap_or_default();
    if parents.iter().skip(1).any(|p| norm(p).starts_with("open recent") || norm(p).starts_with("recent")) {
        return Some("recent-file lists");
    }
    if ["adobe", "creative cloud", "sign in", "sign out", "log in", "log out"].iter().any(|w| label.contains(w)) {
        return Some("account and cloud items");
    }
    if ["autofill", "start dictation", "emoji and symbols", "writing tools", "services"].iter().any(|w| label.starts_with(w)) {
        return Some("macOS system items");
    }
    None
}

enum Placement {
    Same,
    Elsewhere(Vec<String>),
    Missing,
}

struct Report {
    /// (reference path, where it is in FilmCraft).
    items: Vec<(Vec<String>, Placement)>,
    out_of_scope: BTreeMap<&'static str, usize>,
    ours_only: Vec<Vec<String>>,
}

fn compare(reference: &[Vec<String>], ours: &[Vec<String>]) -> Report {
    let key = |p: &[String]| p.iter().map(|s| norm(s)).collect::<Vec<_>>();
    let our_paths: HashSet<Vec<String>> = ours.iter().map(|p| key(p)).collect();
    let mut by_label: HashMap<String, &Vec<String>> = HashMap::new();
    for p in ours {
        if let Some(l) = p.last() {
            by_label.entry(norm(l)).or_insert(p);
        }
    }
    let mut report = Report { items: Vec::new(), out_of_scope: BTreeMap::new(), ours_only: Vec::new() };
    let mut seen = HashSet::new();
    for p in reference {
        if !seen.insert(key(p)) {
            continue;
        }
        if let Some(why) = out_of_scope(p) {
            *report.out_of_scope.entry(why).or_default() += 1;
            continue;
        }
        let label = p.last().map(|l| norm(l)).unwrap_or_default();
        let placement = if our_paths.contains(&key(p)) {
            Placement::Same
        } else if let Some(at) = by_label.get(&label) {
            Placement::Elsewhere((*at).clone())
        } else {
            Placement::Missing
        };
        report.items.push((p.clone(), placement));
    }
    let ref_labels: HashSet<String> = reference.iter().filter_map(|p| p.last().map(|l| norm(l))).collect();
    report.ours_only = ours.iter().filter(|p| p.last().is_some_and(|l| !ref_labels.contains(&norm(l)))).cloned().collect();
    report
}

fn pct(n: usize, of: usize) -> f64 {
    if of == 0 { 0.0 } else { n as f64 * 100.0 / of as f64 }
}

fn show(p: &[String]) -> String {
    p.join(" ▸ ")
}

impl Report {
    fn counts(&self) -> (usize, usize, usize) {
        let same = self.items.iter().filter(|(_, pl)| matches!(pl, Placement::Same)).count();
        let elsewhere = self.items.iter().filter(|(_, pl)| matches!(pl, Placement::Elsewhere(_))).count();
        (same, elsewhere, self.items.len().saturating_sub(same + elsewhere))
    }

    /// Reference items whose label we have anywhere, in percent of the items in scope.
    fn presence(&self) -> f64 {
        let (same, elsewhere, _) = self.counts();
        pct(same + elsewhere, self.items.len())
    }

    fn summary(&self) -> String {
        let (same, elsewhere, missing) = self.counts();
        let n = self.items.len();
        format!(
            "menus: presence {}/{n} ({:.1}%), same place {same}/{n} ({:.1}%), elsewhere {elsewhere}, missing {missing}, out of scope {}",
            same + elsewhere,
            self.presence(),
            pct(same, n),
            self.out_of_scope.values().sum::<usize>()
        )
    }

    fn markdown(&self, reference: &str, ours: usize) -> String {
        let (same, elsewhere, missing) = self.counts();
        let n = self.items.len();
        let mut s = String::from("# Menu parity checklist\n\n");
        s.push_str(&format!(
            "Generated by `cargo xtask parity` from `{reference}` ({n} items in scope, {} out of scope) against FilmCraft's \
             shipped menu bar ({ours} items). *Same place*: we have the label at the same menu path; *elsewhere*: under \
             another path; *missing*: nowhere. A label match is presence only, not behaviour.\n\n",
            self.out_of_scope.values().sum::<usize>()
        ));
        s.push_str("| Menu | Items | Same place | Elsewhere | Missing | Presence |\n|---|---|---|---|---|---|\n");
        let mut per: Vec<(&str, [usize; 3])> = Vec::new();
        for (p, pl) in &self.items {
            let top = p.first().map(String::as_str).unwrap_or_default();
            let i = match per.iter().position(|(t, _)| *t == top) {
                Some(i) => i,
                None => {
                    per.push((top, [0; 3]));
                    per.len() - 1
                }
            };
            let col = match pl {
                Placement::Same => 0,
                Placement::Elsewhere(_) => 1,
                Placement::Missing => 2,
            };
            if let Some(c) = per.get_mut(i).and_then(|(_, c)| c.get_mut(col)) {
                *c += 1;
            }
        }
        for (top, [a, b, c]) in &per {
            let total = a + b + c;
            s.push_str(&format!("| {top} | {total} | {a} | {b} | {c} | {:.1}% |\n", pct(a + b, total)));
        }
        s.push_str(&format!("| **Total** | **{n}** | **{same}** | **{elsewhere}** | **{missing}** | **{:.1}%** |\n", self.presence()));
        s.push_str(&format!("\n## Missing ({missing})\n\n"));
        for (p, _) in self.items.iter().filter(|(_, pl)| matches!(pl, Placement::Missing)) {
            s.push_str(&format!("- [ ] {}\n", show(p)));
        }
        s.push_str(&format!("\n## Elsewhere in FilmCraft ({elsewhere})\n\n"));
        for (p, pl) in &self.items {
            if let Placement::Elsewhere(at) = pl {
                s.push_str(&format!("- {} → {}\n", show(p), show(at)));
            }
        }
        s.push_str("\n## Out of scope\n\n");
        for (why, k) in &self.out_of_scope {
            s.push_str(&format!("- {why}: {k}\n"));
        }
        s.push_str(&format!("\n## FilmCraft only ({})\n\n", self.ours_only.len()));
        for p in &self.ours_only {
            s.push_str(&format!("- {}\n", show(p)));
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn paths(v: &[&[&str]]) -> Vec<Vec<String>> {
        v.iter().map(|p| p.iter().map(|s| s.to_string()).collect()).collect()
    }

    #[test]
    fn reference_formats_give_the_same_paths() {
        let want = paths(&[&["File", "New", "Sequence…"], &["File", "Save"], &["Edit", "Undo"]]);
        let forms = [
            json!([["File", "New", "Sequence…"], ["File", "Save"], ["Edit", "Undo"]]),
            json!(["File > New > Sequence…", "File ▸ Save", "Edit > Undo"]),
            json!({"menus": [
                {"name": "File", "children": [
                    {"name": "New", "items": [{"name": "Sequence…", "shortcut": "Cmd+N"}]},
                    {"name": "missing value"},
                    {"title": "Save"}
                ]},
                {"name": "Edit", "submenu": [{"label": "Undo"}]}
            ]}),
            json!({"Edit": {"Undo": "Cmd+Z"}, "File": {"New": ["Sequence…"], "-": null, "Save": null}}),
        ];
        for f in forms {
            let mut got = reference_paths(&f).unwrap();
            got.sort();
            let mut w = want.clone();
            w.sort();
            assert_eq!(got, w, "{f}");
        }
    }

    #[test]
    fn hostile_reference_is_refused_not_overflowed() {
        let mut v = json!("leaf");
        for _ in 0..200 {
            v = json!({ "name": "m", "children": [v] });
        }
        assert!(reference_paths(&v).unwrap_err().contains("nested deeper"));
        let many: Vec<String> = (0..MAX_ITEMS + 1).map(|i| format!("File > Item {i}")).collect();
        assert!(reference_paths(&json!(many)).unwrap_err().contains("more than"));
        // odd values are skipped, not trusted
        assert_eq!(reference_paths(&json!([1, null, true, {"name": 5}, ["File"]])).unwrap(), Vec::<Vec<String>>::new());
    }

    #[test]
    fn presence_and_placement_are_reported_separately() {
        let reference = paths(&[
            &["File", "New", "Sequence..."],
            &["File", "Export Frame"],
            &["Edit", "Find & Replace"],
            &["Edit", "Spelling"],
            &["Help", "Premiere Pro Help…"],
            &["File", "Open Recent", "clip.prproj"],
            &["Edit", "AutoFill"],
            &["Edit", "Spelling"],
        ]);
        let ours = paths(&[&["File", "New", "Sequence…"], &["File", "Export", "Export Frame"], &["Edit", "Find and Replace…"], &["Clip", "Nest…"]]);
        let r = compare(&reference, &ours);
        assert_eq!(r.counts(), (2, 1, 1), "duplicates count once; out-of-scope items are not counted");
        assert!((r.presence() - 75.0).abs() < 1e-9);
        assert_eq!(r.out_of_scope.values().sum::<usize>(), 3);
        assert_eq!(r.ours_only, paths(&[&["Clip", "Nest…"]]));
        let md = r.markdown("menus.json", ours.len());
        assert!(md.contains("| **Total** | **4** | **2** | **1** | **1** | **75.0%** |"), "{md}");
        assert!(md.contains("- [ ] Edit ▸ Spelling"));
        assert!(md.contains("- File ▸ Export Frame → File ▸ Export ▸ Export Frame"));
        assert!(r.summary().contains("presence 3/4 (75.0%), same place 2/4 (50.0%)"));
    }

    #[test]
    fn empty_reference_measures_zero_without_dividing_by_zero() {
        let r = compare(&[], &paths(&[&["File", "Save"]]));
        assert_eq!(r.presence(), 0.0);
        assert!(r.summary().contains("presence 0/0 (0.0%)"));
    }

    #[test]
    fn our_dump_and_arguments_are_checked() {
        let ours = our_items(&json!([{"id": "file.save", "label": "Save", "path": ["File"]}, {"id": "x", "label": "Orphan", "path": []}, {"bad": 1}])).unwrap();
        assert_eq!(ours, paths(&[&["File", "Save"]]));
        assert!(our_items(&json!({})).is_err());
        assert!(parse_args(&["--min", "101"]).is_err());
        assert!(parse_args(&["--min", "x"]).is_err());
        assert!(parse_args(&["--out"]).is_err());
        assert!(parse_args(&["--bogus"]).is_err());
        assert!(parse_args(&["--min", "90", "--reference", "r.json"]).is_ok_and(|a| a.min == Some(90.0) && a.reference.is_some()));
    }
}
