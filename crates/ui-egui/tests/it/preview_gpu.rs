//! Render previews composited on the GPU (Settings ▸ Playback ▸ Render previews on the GPU =
//! Auto) against the same previews rendered on the CPU, read back through the decoded ProRes
//! files, across a segment boundary. Skipped without a GPU adapter.

use std::sync::Arc;

use filmcraft_engine::Session;
use filmcraft_media::Generator;
use filmcraft_media::generators::GeneratorSource;
use filmcraft_project::{ItemId, Label, SequenceSettings, TrackKind, find_effect};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick, TimeRange};
use serde_json::json;

/// The GPU export frame renderer, as the app and CLI register it at startup.
fn gpu_renderer() -> Option<Box<dyn filmcraft_export::FrameRenderer>> {
    struct R(filmcraft_gpu::ExportRenderer);
    impl filmcraft_export::FrameRenderer for R {
        fn render(
            &mut self,
            project: &filmcraft_project::Project,
            seq: ItemId,
            t: Tick,
            opts: filmcraft_render::RenderOptions,
            sources: &dyn filmcraft_render::SourceProvider,
        ) -> Option<filmcraft_render::Image> {
            self.0.render(project, seq, t, opts, sources)
        }

        fn caps(&self) -> filmcraft_render::plan::DeviceCaps {
            self.0.caps()
        }
    }
    filmcraft_gpu::ExportRenderer::new().map(|r| Box::new(R(r)) as Box<dyn filmcraft_export::FrameRenderer>)
}

/// A 160×90 24 fps sequence: the counting leader with Tint for frames 0..24, then plain for
/// 24..48, so frames 20..28 cross the boundary of two preview segments.
fn session(dir: &std::path::Path) -> (Session, ItemId) {
    let mut s = Session::default();
    s.previews.set_dir(Some(dir.to_path_buf()));
    let r = FrameRate::FPS_24;
    let mut p = (*s.project).clone();
    let src = GeneratorSource::new(Generator::CountingLeader, 160, 90, r, Tick(10 * TICKS_PER_SECOND));
    let clip = filmcraft_engine::demo::add_generator(&mut p, &s.media, src, "Leader", Label::Iris, None);
    let seq = p.new_sequence("Seq", SequenceSettings { width: 160, height: 90, frame_rate: r, ..Default::default() }, 1, 0, None);
    let mut a = p.make_track_item(clip, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, r.tick_of(24)), r).unwrap();
    let mut b = p.make_track_item(clip, TrackKind::Video, r.tick_of(24), TimeRange::new(r.tick_of(24), r.tick_of(24)), r).unwrap();
    for e in a.effects.iter_mut().chain(b.effects.iter_mut()) {
        filmcraft_project::resolve_auto_points(e, (160, 90), (160, 90));
    }
    a.effects.push(find_effect("tint").unwrap().instance());
    p.sequence_mut(seq).unwrap().video_tracks[0].items = vec![a, b];
    s.project = Arc::new(p);
    s.state.active_sequence = Some(seq);
    s.state.open_sequences = vec![seq];
    (s, seq)
}

/// Render In to Out with the preview GPU setting `gpu` and read frames 20..28 from the previews.
fn preview_frames(s: &mut Session, seq: ItemId, gpu: &str) -> Vec<Vec<u8>> {
    let mut prefs = s.prefs.clone();
    prefs.playback.preview_gpu_rendering = gpu.into();
    s.set_prefs(prefs).unwrap();
    let v = s.execute("sequence.renderInToOut", json!({"wait": true})).unwrap();
    let id = v["job"].as_u64().expect("a render job");
    let j = s.jobs.iter().find(|j| j.id == id).unwrap().to_json();
    assert!(j["result"].get("error").is_none(), "{j}");
    (20..28).map(|f| s.previews.frame(&s.media, &s.project, seq, f, 1.0).expect("a preview frame").to_rgba8()).collect()
}

#[test]
fn gpu_rendered_previews_match_the_cpu_ones() {
    if gpu_renderer().is_none() {
        eprintln!("SKIPPED (gpu previews): no adapter");
        return;
    }
    filmcraft_export::register_frame_renderer(gpu_renderer);
    let dir = std::env::temp_dir().join(format!("filmcraft-preview-gpu-{}", std::process::id()));
    let (mut s, seq) = session(&dir);
    let cpu = preview_frames(&mut s, seq, "off");
    let before = filmcraft_export::gpu_render_stats().frames;
    let gpu = preview_frames(&mut s, seq, "auto");
    assert!(filmcraft_export::gpu_render_stats().frames >= before + 48, "Auto rendered the preview frames on the GPU");
    assert!(cpu.windows(2).any(|w| w[0] != w[1]), "the test needs frames that differ");
    for (k, (c, g)) in cpu.iter().zip(&gpu).enumerate() {
        let mut d: Vec<u8> = c.iter().zip(g).map(|(a, b)| a.abs_diff(*b)).collect();
        d.sort_unstable();
        let (p99, max) = (d[d.len() * 99 / 100], d[d.len() - 1]);
        // the GPU compositor's golden tolerance against the CPU reference (8-bit), after both went
        // through the same 10-bit ProRes encode and decode
        assert!(p99 <= 2 && max <= 8, "frame {}: GPU preview vs CPU preview p99 {p99}, max {max}", 20 + k);
    }
    let _ = std::fs::remove_dir_all(&dir);
}
