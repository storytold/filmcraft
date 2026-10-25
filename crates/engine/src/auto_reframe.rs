//! Sequence ▸ Auto Reframe Sequence… (#1564): a copy of a sequence in another aspect ratio (9:16,
//! 1:1, 4:5 or 16:9) whose video clips follow their subject.
//!
//! The Auto Reframe effect works inside the clip's own picture, so it can pan a window across it
//! but cannot grow it: on its own it leaves a 16:9 clip in a 9:16 frame letterboxed. The command
//! therefore does what Premiere's does: every video clip is scaled to fill the new frame (Motion
//! Scale, keyframes included, by how much more of the frame the picture must cover there than in
//! the old one; Position offsets from the centre scale with it), and gets Auto Reframe set to the
//! target aspect, which then picks the window of the picture that shows. Graphics, adjustment
//! layers and audio stay as they are. One undo step; the new sequence opens.

use filmcraft_project::{EffectInstance, ItemKind, Label, Param, ParamValue, Project, Sequence, TrackItem, TrackKind, find_effect, par_ratio};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, bad, has_seq, item_p, str_p};
use crate::{EngineError, Result, Session};

const ID: &str = "sequence.autoReframe";

/// Motion Scale's upper bound (the parameter's range, in %).
const MAX_SCALE: f64 = 10_000.0;

/// A target aspect ratio: its width and height terms, its name in Auto Reframe's Target Aspect
/// list, and that list's index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Aspect {
    pub w: u32,
    pub h: u32,
    pub label: &'static str,
    index: u32,
}

/// The aspects Auto Reframe Sequence offers, as in Premiere.
pub const ASPECTS: [Aspect; 4] = [
    Aspect { w: 9, h: 16, label: "Vertical 9:16", index: 1 },
    Aspect { w: 1, h: 1, label: "Square 1:1", index: 2 },
    Aspect { w: 4, h: 5, label: "Vertical 4:5", index: 3 },
    Aspect { w: 16, h: 9, label: "Horizontal 16:9", index: 4 },
];

/// `9:16`, `9x16`, `vertical 9:16`, `square`, `1:1`… (any case).
pub fn aspect_named(s: &str) -> Option<Aspect> {
    let n = s.trim().to_ascii_lowercase().replace('x', ":");
    ASPECTS.iter().copied().find(|a| {
        let ratio = format!("{}:{}", a.w, a.h);
        n == ratio
            || n == a.label.to_ascii_lowercase()
            || n.split_whitespace().next() == a.label.split_whitespace().next().map(str::to_ascii_lowercase).as_deref() && a.w == a.h
    })
}

/// The frame of the new sequence: the old frame's short side stays, the other follows the aspect
/// (1920 × 1080 → 1080 × 1920 for 9:16, 1080 × 1350 for 4:5), even numbers for the encoders.
pub fn target_size(old: (u32, u32), a: Aspect) -> (u32, u32) {
    let short = f64::from(old.0.min(old.1).max(2));
    let even = |x: f64| ((x / 2.0).round() * 2.0).max(2.0) as u32;
    if a.w <= a.h { (even(short), even(short * f64::from(a.h) / f64::from(a.w))) } else { (even(short * f64::from(a.w) / f64::from(a.h)), even(short)) }
}

/// Motion Preset index of Auto Reframe for `slower` / `default` / `faster`.
fn motion_preset(s: Option<&str>) -> Option<u32> {
    match s.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        None | Some("default" | "") => Some(1),
        Some("slower" | "slower motion") => Some(0),
        Some("faster" | "faster motion") => Some(2),
        _ => None,
    }
}

/// Reframe one video clip of `seq` for the frame change `from` → `to`. Returns false for a clip
/// without a picture of its own to reframe (graphics, adjustment layers, offline sizes).
fn reframe_item(pr: &Project, it: &mut TrackItem, seq_par: f64, from: (u32, u32), to: (u32, u32), aspect: Aspect, preset: u32) -> bool {
    if it.graphic.is_some() {
        return false;
    }
    let (Some(src), Some(par)) = (pr.source_size(it.item), pr.source_par(it.item)) else { return false };
    if src.0 == 0 || src.1 == 0 {
        return false;
    }
    // the picture's size in sequence pixels at 100 % (Motion scales its display size)
    let (dw, dh) = (f64::from(src.0) * par_ratio(par) / seq_par, f64::from(src.1));
    let cover = |(w, h): (u32, u32)| (f64::from(w) / dw).max(f64::from(h) / dh);
    let fit = |(w, h): (u32, u32)| (f64::from(w) / dw).min(f64::from(h) / dh);
    let (c0, c1) = (cover(from), cover(to));
    if !(c0.is_finite() && c1.is_finite() && c0 > 0.0 && c1 > 0.0) {
        return false;
    }
    // how much bigger the picture gets; Scale to Frame Size's fit becomes part of the Scale
    let zoom = c1 / c0;
    let pre = if it.scale_to_frame { fit(from) } else { 1.0 };
    it.scale_to_frame = false;
    if it.effect("motion").is_none()
        && let Some(def) = find_effect("motion")
    {
        it.effects.insert(0, def.instance());
    }
    if let Some(motion) = it.effect_mut("motion") {
        let (o0, o1) = ((f64::from(from.0) / 2.0, f64::from(from.1) / 2.0), (f64::from(to.0) / 2.0, f64::from(to.1) / 2.0));
        if let Some(p) = motion.param_mut("position") {
            // an automatic (NaN) position stays automatic: it follows the frame centre
            p.map_values(|v| match v {
                ParamValue::Vec2(mut v) => {
                    v.x = o1.0 + zoom * (v.x - o0.0);
                    v.y = o1.1 + zoom * (v.y - o0.1);
                    ParamValue::Vec2(v)
                }
                other => other,
            });
        }
        // A picture that filled the old frame, with or without bars (Scale to Frame Size, another
        // aspect, a portrait clip), covers the new one: zoomed in beyond covering the old frame,
        // it stays that much bigger than covering the new one; letterboxed, it is raised to
        // cover rather than keeping its bars (#2453 review). A picture smaller than that (a
        // picture-in-picture) keeps its size relative to the fitted picture: it takes the same
        // share of the new frame as of the old one, whatever the source's aspect.
        let (f0, f1) = (fit(from), fit(to));
        let rescale = |s: f64| {
            let old = s / 100.0 * pre;
            let new = if old >= f0 * (1.0 - 1e-6) { c1 * (old / c0).max(1.0) } else { old * f1 / f0 };
            if new.is_finite() { (new * 100.0).clamp(0.0, MAX_SCALE) } else { 100.0 * c1 }
        };
        // Scale decides; Scale Width (non-uniform scaling) follows by the same factor, so a
        // stretch is kept
        let s0 = motion.f64_at("scale", filmcraft_time::Tick::ZERO);
        let s0 = if s0.is_finite() && s0 > 0.0 { s0 } else { 100.0 };
        let width_factor = rescale(s0) / s0;
        match motion.param_mut("scale") {
            Some(p) => p.map_values(|v| match v {
                ParamValue::Float(s) => ParamValue::Float(rescale(s)),
                other => other,
            }),
            None => {
                motion.params.insert("scale".into(), Param::new(ParamValue::Float(rescale(100.0))));
            }
        }
        match motion.param_mut("scale_width") {
            Some(p) => p.map_values(|v| match v {
                ParamValue::Float(s) => ParamValue::Float((s * width_factor).clamp(0.0, MAX_SCALE)),
                other => other,
            }),
            None => {
                motion.params.insert("scale_width".into(), Param::new(ParamValue::Float((100.0 * width_factor).clamp(0.0, MAX_SCALE))));
            }
        }
    }
    let set = |e: &mut EffectInstance| {
        e.params.insert("aspect".into(), Param::new(ParamValue::Choice(aspect.index)));
        e.params.insert("preset".into(), Param::new(ParamValue::Choice(preset)));
    };
    match it.effects.iter_mut().find(|e| e.effect == "auto_reframe") {
        Some(e) => set(e),
        None => {
            if let Some(def) = find_effect("auto_reframe") {
                let mut e = def.instance();
                set(&mut e);
                it.effects.push(e);
            }
        }
    }
    true
}

/// Reframe every video clip of `seq` (already resized to `to`) that shows a picture of its own.
/// Returns how many were reframed.
fn reframe_sequence(pr: &Project, seq: &mut Sequence, from: (u32, u32), aspect: Aspect, preset: u32) -> usize {
    let to = (seq.settings.width, seq.settings.height);
    let seq_par = par_ratio(seq.settings.par);
    let mut n = 0;
    for track in seq.all_tracks_mut().filter(|t| t.kind == TrackKind::Video) {
        for it in &mut track.items {
            n += usize::from(reframe_item(pr, it, seq_par, from, to, aspect, preset));
        }
    }
    n
}

/// `sequence.autoReframe {sequence?, aspect, motion?, name?}`.
fn run(s: &mut Session, p: &Value) -> Result<Value> {
    let source = item_p(p, "sequence").or(s.state.active_sequence).ok_or(EngineError::NoSequence)?;
    let aspect = aspect_named(str_p(p, "aspect").unwrap_or("9:16")).ok_or_else(|| bad(ID, "`aspect` must be 9:16, 1:1, 4:5 or 16:9"))?;
    let preset = motion_preset(str_p(p, "motion")).ok_or_else(|| bad(ID, "`motion` must be slower, default or faster"))?;
    let item = s.project.item(source).ok_or_else(|| bad(ID, "no such sequence"))?;
    let ItemKind::Sequence(seq) = &item.kind else { return Err(bad(ID, "not a sequence")) };
    let from = (seq.settings.width, seq.settings.height);
    let size = target_size(from, aspect);
    let name = match str_p(p, "name").map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) => n.to_string(),
        None => format!("{} - {}", item.name, aspect.label.replace(':', "x")),
    };
    let label: Label = item.label;
    let bin = s.project.root.parent_of(source).filter(|b| *b != s.project.root.id);
    let (id, clips) = s.edit("Auto Reframe Sequence", |pr, st| {
        let mut seq = pr.sequence(source).ok_or_else(|| bad(ID, "no such sequence"))?.clone();
        seq.settings.width = size.0;
        seq.settings.height = size.1;
        seq.settings.preset = format!("{}x{} {}", size.0, size.1, seq.settings.frame_rate.label());
        seq.settings.validate().map_err(|e| bad(ID, e))?;
        let clips = reframe_sequence(pr, &mut seq, from, aspect, preset);
        let id = pr.add_item(&name, label, ItemKind::Sequence(std::sync::Arc::new(seq)), bin);
        st.active_sequence = Some(id);
        if !st.open_sequences.contains(&id) {
            st.open_sequences.push(id);
        }
        st.selection.clear();
        Ok((id, clips))
    })?;
    // Shows the new sequence's Timeline, restoring the panel if it was closed.
    s.events.push(crate::Event::OpenSequence(id));
    Ok(json!({"sequence": id.0, "name": name, "width": size.0, "height": size.1, "clips": clips}))
}

pub fn commands() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: ID,
        label: "Auto Reframe Sequence…",
        menu: &["Sequence"],
        shortcut: None,
        params: r#"{"sequence":itemId? (default: the active one),"aspect":"9:16|1:1|4:5|16:9"="9:16","motion":"slower|default|faster"="default","name":str? (default: "<name> - Vertical 9x16")}"#,
        enabled: has_seq,
        run,
        journal: true,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_sizes_keep_the_short_side() {
        let a = |s: &str| aspect_named(s).unwrap();
        assert_eq!(target_size((1920, 1080), a("9:16")), (1080, 1920));
        assert_eq!(target_size((1920, 1080), a("1:1")), (1080, 1080));
        assert_eq!(target_size((1920, 1080), a("4:5")), (1080, 1350));
        assert_eq!(target_size((1080, 1920), a("16:9")), (1920, 1080));
        assert_eq!(target_size((3840, 2160), a("9x16")), (2160, 3840));
        assert_eq!(target_size((1, 1), a("4:5")).0 % 2, 0, "even, never zero");
        assert_eq!(a("Square 1:1"), a("square"));
        assert_eq!(a("vertical 4:5").label, "Vertical 4:5");
        assert!(aspect_named("2:1").is_none() && aspect_named("vertical").is_none());
        assert_eq!(motion_preset(Some("Faster")), Some(2));
        assert_eq!(motion_preset(Some("sideways")), None);
    }
}
