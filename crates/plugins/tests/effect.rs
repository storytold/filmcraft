//! A plug-in `effect` runs in FilmCraft's render path. The plug-in is `tests/fixtures/pixels.wat`:
//! `copy` (identity), `x4` (every f32 sample times 4) and `err` (a plug-in error).
//!
//! What runs: the catalog and Effects panel list, `effects::apply` on real pixels, the whole
//! `render_program` path on the demo project, and the plan: a plug-in effect is not a GPU effect,
//! so the plan draws the layer on the CPU and hands the GPU a canvas.
//! What does not run: the wgpu composite of that canvas, and the egui panel drawing the row.

mod common;

use filmcraft_engine::project::{EffectInstance, find_effect};
use filmcraft_engine::render::effects::{FxCtx, apply};
use filmcraft_engine::render::plan::{FramePlan, plan_frame};
use filmcraft_engine::render::{Image, RenderOptions};
use filmcraft_engine::time::Tick;
use serde_json::json;

const ID: &str = "org.example.fx";
const COPY: &str = "org.example.fx.copy";
const X4: &str = "org.example.fx.x4";
const ERR: &str = "org.example.fx.err";
/// The fixture allocates from a 1 MB heap, so each band of a layer stays far below that.
const BAND: u64 = 65_536;

fn package() -> Vec<u8> {
    let manifest = common::manifest(
        ID,
        &[],
        &[],
        json!([
            { "kind": "effect", "id": "copy", "entry": 0, "label": "Copy", "category": "Test", "process": { "sample": ["f32"], "band_bytes": BAND } },
            { "kind": "effect", "id": "x4", "entry": 3, "label": "Times four", "category": "Test", "process": { "sample": ["f32"], "alpha": "straight", "band_bytes": BAND } },
            { "kind": "effect", "id": "err", "entry": 5, "label": "Fails", "category": "Test", "process": { "sample": ["f32"], "band_bytes": BAND } }
        ]),
    );
    common::package(&common::fixture_text("pixels"), &manifest)
}

fn ctx<'a>() -> FxCtx<'a> {
    FxCtx { t: Tick::ZERO, px_scale: 1.0, seconds: 0.0, timecode: "", clip_name: "", project: None, env: None, frame_rate: 24.0, working: Default::default() }
}

fn instance(id: &str) -> EffectInstance {
    find_effect(id).expect("the plug-in effect is in the catalog").instance()
}

/// Left half opaque gray 0.1, right half opaque black.
fn edge(w: usize, h: usize) -> Image {
    let mut img = Image::new(w, h);
    for (i, px) in img.px.chunks_exact_mut(4).enumerate() {
        let v = if i % w < w / 2 { 0.1 } else { 0.0 };
        px.copy_from_slice(&[v, v, v, 1.0]);
    }
    img
}

#[test]
fn a_plugin_effect_changes_real_pixels_through_the_render_path() {
    let (mut session, host) = common::rig();
    assert!(find_effect(X4).is_none(), "nothing is in the catalog before the install");
    common::install(&host, &package());
    common::assert_active(&host, ID);

    // ---- the catalog: the Effects panel reads `plugin_effect_defs`, `effects.list` and `find_effect`
    let def = find_effect(X4).expect("the plug-in effect is in the catalog");
    assert_eq!((def.name, def.category), ("Times four", ["Video Effects", "Test"].as_slice()));
    assert!(filmcraft_engine::project::plugin_effect_defs().iter().any(|d| d.id == X4));
    let listed = session.execute("effects.list", json!({"folder": "Video Effects/Test"})).expect("effects.list");
    assert!(listed.as_array().expect("list").iter().any(|d| d["id"] == X4), "{listed}");

    // ---- real pixels: `effects::apply`
    let src = edge(16, 8);
    let mut same = src.clone();
    apply(&mut same, &instance(COPY), &ctx());
    assert_eq!(same.px, src.px, "copy hands the layer back unchanged");
    let mut times_four = src.clone();
    apply(&mut times_four, &instance(X4), &ctx());
    for (i, (got, was)) in times_four.px.chunks_exact(4).zip(src.px.chunks_exact(4)).enumerate() {
        assert!((got[0] - was[0] * 4.0).abs() < 0.01, "pixel {i}: red {} should be four times {}", got[0], was[0]);
        assert_eq!(got[3], 1.0, "pixel {i}: alpha stays opaque");
    }
    let mut off_instance = instance(X4);
    off_instance.enabled = false;
    let mut skipped = src.clone();
    apply(&mut skipped, &off_instance, &ctx());
    assert_eq!(skipped.px, src.px, "a disabled effect does nothing");

    // ---- a failing plug-in leaves the image alone
    let mut failed = src.clone();
    apply(&mut failed, &instance(ERR), &ctx());
    assert_eq!(failed.px, src.px, "an error from the plug-in leaves the image as it was");

    // ---- the whole path: demo project, effect on every video clip at the playhead
    let clips: Vec<_> = session
        .active_sequence()
        .expect("sequence")
        .all_tracks()
        .filter(|t| t.kind == filmcraft_engine::project::TrackKind::Video)
        .flat_map(|t| t.items.iter().filter(|i| i.start == Tick::ZERO).map(|i| i.id.0))
        .collect();
    assert!(!clips.is_empty(), "the demo project has a video clip at 0");
    let before = session.render_program(0.5).expect("render");
    session.execute("effects.apply", json!({"effect": COPY, "clips": clips})).expect("apply copy");
    let identity = session.render_program(0.5).expect("render");
    let moved = |a: &Image, b: &Image| a.px.iter().zip(&b.px).filter(|(x, y)| (**x - **y).abs() > 0.004).count();
    assert_eq!(moved(&before, &identity), 0, "copy is an identity through the pipeline");
    session.execute("effects.apply", json!({"effect": X4, "clips": clips})).expect("apply times four");
    let brighter = session.render_program(0.5).expect("render");
    let changed = moved(&before, &brighter);
    assert!(changed > 200, "the plug-in changes the picture ({changed} samples of {})", before.px.len());

    // ---- the plan: not a GPU effect, so the layer is a CPU canvas and the GPU gets no effect stage
    let seq = session.state.active_sequence.expect("sequence");
    let provider = session.media.provider(session.project.clone(), session.services.clone());
    let plan = plan_frame(
        &session.project,
        seq,
        session.playhead(),
        RenderOptions { scale: 0.5, ..RenderOptions::default() },
        filmcraft_engine::render::plan::DeviceCaps::ALL,
        &provider,
    );
    let layers = match &plan {
        FramePlan::Layers { layers, .. } => layers.as_slice(),
        FramePlan::Composite { steps, .. } => match steps.first() {
            Some(filmcraft_engine::render::plan::PlanStep::Layers(layers)) => layers.as_slice(),
            _ => &[],
        },
        FramePlan::Image(_) => &[],
    };
    assert!(!layers.is_empty() && layers.iter().all(|l| l.fx.is_none()), "a plug-in effect never reaches the GPU effect stage");
    let cpu = filmcraft_engine::render::plan::execute_cpu(&plan);
    let off = cpu.px.iter().zip(&brighter.px).filter(|(a, b)| (**a - **b).abs() > 0.02).count();
    assert!(off < cpu.px.len() / 200, "the plan renders the picture render_program shows ({off} samples differ)");

    // ---- removing the plug-in takes the effect out of the catalog, and the clip draws as before
    host.manager.remove(ID).expect("remove");
    assert!(find_effect(X4).is_none(), "the effect leaves the catalog");
    let gone = session.render_program(0.5).expect("render");
    assert_eq!(moved(&before, &gone), 0, "an effect with no plug-in is a no-op, as an unknown id always was");
}
