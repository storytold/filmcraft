//! Source and Program monitors.
//!
//! Frames come from the background [`FrameServer`](crate::frames::FrameServer) at the *smaller* of
//! the playback resolution and the on-screen resolution (so a small monitor never pays for 4K).
//! While playing we prefetch the next frames in priority order; while scrubbing the exact frame is
//! requested first and the nearest cached frame is shown until it arrives (no black flashes), or
//! failing that the frame asked for a refresh earlier, so the picture follows a scrub or a value
//! that is dragged instead of waiting for the mouse to rest.
//!
//! The Source monitor plays on its own clock (its Play button, [`toggle_source`]); playing one
//! monitor stops the other. Automation ids: `<monitor>.timecode`, `.zoom`, `.resolution` (+
//! `.resolution.<full|half|quarter|eighth|sixteenth>` while open), `.settings` (the wrench; its
//! entries `.settings.<…>`, e.g. `.settings.safeMargins`, `program.settings.playback.loop`,
//! `program.settings.multicam.gridLayout.<auto|2x2|3x3|4x4>`), `.scrubBar`, `.picture`,
//! `.transport.<command>` (the transport bar) and `.transport.buttonEditor` (the `+`), whose popup
//! has `<monitor>.buttonEditor.<command>` (show / hide that button) and `.buttonEditor.reset`.

use egui::{Align2, Color32, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use filmcraft_project::ItemKind;
use filmcraft_time::{Tick, TimeDisplay, format_time};
use serde_json::json;

use crate::FilmcraftApp;
use crate::frames::{FrameKey, Target};
use crate::icons::{self, Icon};
use crate::panels::monitor_view;
use crate::state::DisplayMode;
use crate::theme::Tokens;

#[derive(Clone, Copy, PartialEq)]
pub enum Which {
    Source,
    Program,
}

/// Source monitor playback (its Play button): the clip it plays and the clock's anchor. Kept in
/// the egui context while playing; the Program monitor's playback lives in `FilmcraftApp::playback`.
#[derive(Clone, Copy, Debug)]
struct SourcePlay {
    item: u64,
    /// egui time (s) of the first frame played; negative until that frame.
    anchor_time: f64,
    anchor: Tick,
}

fn source_play_id() -> egui::Id {
    egui::Id::new("source-monitor-playback")
}

/// Whether the Source monitor is playing.
pub fn source_playing(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<SourcePlay>(source_play_id())).is_some()
}

/// Stop Source monitor playback (no-op when it is not playing).
pub fn stop_source(ctx: &egui::Context) {
    ctx.data_mut(|d| d.remove::<SourcePlay>(source_play_id()));
}

/// Play / stop the Source monitor, as its Play button does. Playing it stops the Program monitor
/// (one monitor plays at a time, as in Premiere); from the last frame it restarts at the In point
/// (or the clip's start).
pub fn toggle_source(app: &mut FilmcraftApp, ctx: &egui::Context) {
    if source_playing(ctx) {
        stop_source(ctx);
        return;
    }
    let Some(item) = app.session.state.source_item else {
        app.ui.status = "Open a clip in the Source monitor to play it".into();
        return;
    };
    let Some(view) = filmcraft_engine::clip_ops::source_view(&app.session, item) else { return };
    if app.playback.playing {
        app.stop();
    }
    let last = view.end - view.rate.frame_duration();
    let mut at = app.session.state.source_playhead;
    if at >= last {
        at = view.mark_in.unwrap_or(view.start);
        if let Err(e) = app.session.execute("source.setPlayhead", json!({"time": at.0})) {
            app.ui.status = e.to_string();
        }
    }
    ctx.data_mut(|d| d.insert_temp(source_play_id(), SourcePlay { item: item.0, anchor_time: -1.0, anchor: at }));
    ctx.request_repaint();
}

/// Move the Source monitor's playhead along while it plays (once per frame, before drawing it):
/// at real-time speed from where Play was pressed, stopping on the clip's last frame, when the
/// Program monitor starts playing, or when another clip is loaded.
fn advance_source(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(mut sp) = ctx.data(|d| d.get_temp::<SourcePlay>(source_play_id())) else { return };
    let view = app.session.state.source_item.filter(|i| i.0 == sp.item).and_then(|i| filmcraft_engine::clip_ops::source_view(&app.session, i));
    let Some(view) = view.filter(|_| !app.playback.playing) else {
        stop_source(ctx);
        return;
    };
    let now = ctx.input(|i| i.time);
    if sp.anchor_time < 0.0 {
        sp.anchor_time = now;
        ctx.data_mut(|d| d.insert_temp(source_play_id(), sp));
    }
    let last = view.end - view.rate.frame_duration();
    let t = sp.anchor + Tick::from_seconds_f64((now - sp.anchor_time).max(0.0));
    let at = if t >= last {
        stop_source(ctx);
        last
    } else {
        t
    };
    if let Err(e) = app.session.execute("source.setPlayhead", json!({"time": at.0})) {
        app.ui.status = e.to_string();
        stop_source(ctx);
    }
    ctx.request_repaint();
}

pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect, which: Which) {
    let t = app.tokens;
    let ctx = ui.ctx().clone();
    if which == Which::Source {
        advance_source(app, &ctx);
    }
    let controls_h = 28.0 + 24.0 + 36.0;
    let video_area = Rect::from_min_max(rect.min + vec2(4.0, 4.0), pos2(rect.max.x - 4.0, rect.max.y - controls_h));
    // the Source monitor's time ruler starts at `origin` (a subclip restricted to its range)
    let mut origin = Tick::ZERO;
    let (target, frame_size, rate, time, duration, drop_frame, mark_in, mark_out, name) = match which {
        Which::Program => {
            let Some(seq_id) = app.session.state.active_sequence else {
                crate::dock::placeholder(ui, rect, &t, "(no sequences)");
                return;
            };
            // A damaged project can name an active sequence that no longer exists.
            let Some(q) = app.session.active_sequence() else {
                crate::dock::placeholder(ui, rect, &t, "(no sequences)");
                return;
            };
            (
                Target::Sequence(seq_id),
                (q.settings.width, q.settings.height),
                q.settings.frame_rate,
                app.session.playhead(),
                q.duration(),
                q.settings.drop_frame,
                q.mark_in,
                q.mark_out,
                app.session.project.item(seq_id).map(|i| i.name.clone()).unwrap_or_default(),
            )
        }
        Which::Source => {
            let Some(item) = app.session.state.source_item else {
                crate::dock::placeholder(ui, rect, &t, "(no clips)");
                return;
            };
            let Some(pi) = app.session.project.item(item) else { return };
            let Some(view) = filmcraft_engine::clip_ops::source_view(&app.session, item) else { return };
            // a subclip shows its parent media's frames
            let size = match app.session.project.item(view.media).map(|i| &i.kind) {
                Some(ItemKind::Media(m)) => m.info.video.as_ref().map(|v| (v.width, v.height)).unwrap_or((0, 0)),
                Some(ItemKind::Sequence(s)) => (s.settings.width, s.settings.height),
                _ => (1920, 1080),
            };
            origin = view.start;
            (Target::Item(item), size, view.rate, app.session.state.source_playhead, view.end, false, view.mark_in, view.mark_out, pi.name.clone())
        }
    };
    let prefix = if which == Which::Program { "program" } else { "source" };
    let mv = monitor_view::view(app, which).clone();
    let display = mv.display_mode().unwrap_or(DisplayMode::Composite);
    // Multi-Camera view: the angle grid on the left, the program on the right
    let multicam = which == Which::Program && mv.multicam;
    // without the preview monitor the grid takes the whole picture area
    let grid_only = multicam && !app.session.state.multicam_view.show_preview;
    let (grid_area, video_area) = if grid_only {
        (Some(video_area), Rect::from_min_size(video_area.max, vec2(0.0, 0.0)))
    } else if multicam {
        let mid = video_area.center().x;
        (Some(Rect::from_min_max(video_area.min, pos2(mid - 2.0, video_area.max.y))), Rect::from_min_max(pos2(mid + 2.0, video_area.min.y), video_area.max))
    } else {
        (None, video_area)
    };
    if let Some(g) = grid_area {
        crate::panels::multicam::grid(app, ui, g);
    }
    ui.painter().rect_filled(video_area, 0.0, t.panel_bg);
    let has_video = frame_size.0 > 0;
    // Source waveform modes: Audio Waveform (or an audio-only clip) and the video/waveform split.
    let source_item = if which == Which::Source { app.session.state.source_item } else { None };
    let wave_only = source_item.is_some() && (!has_video || display == DisplayMode::AudioWaveform);
    let (video_area, wave_area) = if wave_only {
        (video_area, Some(video_area))
    } else if source_item.is_some() && display == DisplayMode::VideoAndWaveform {
        let split = video_area.min.y + video_area.height() * 0.62;
        (Rect::from_min_max(video_area.min, pos2(video_area.max.x, split - 2.0)), Some(Rect::from_min_max(pos2(video_area.min.x, split + 2.0), video_area.max)))
    } else {
        (video_area, None)
    };
    if let (Some(wa), Some(item)) = (wave_area, source_item) {
        monitor_view::waveform(app, ui, wa, item, time, duration, mark_in, mark_out);
    }
    let show_picture = has_video && !wave_only && !grid_only;
    // Rulers along the top and left of the picture area.
    let rulers = show_picture && mv.show_rulers;
    let (ruler_rects, video_area) = if rulers {
        let r = monitor_view::RULER;
        (
            Some((
                Rect::from_min_max(pos2(video_area.min.x + r, video_area.min.y), pos2(video_area.max.x, video_area.min.y + r)),
                Rect::from_min_max(pos2(video_area.min.x, video_area.min.y + r), pos2(video_area.min.x + r, video_area.max.y)),
            )),
            Rect::from_min_max(video_area.min + vec2(r, r), video_area.max),
        )
    } else {
        (None, video_area)
    };
    let picture_area = video_area;
    // Comparison View: the reference frame on the left, the current frame on the right.
    let compare = which == Which::Program && show_picture && display == DisplayMode::Comparison;
    let (ref_area, video_area) = if compare {
        let mid = video_area.center().x;
        let bar_h = 22.0;
        (
            Some(Rect::from_min_max(video_area.min, pos2(mid - 2.0, video_area.max.y - bar_h))),
            Rect::from_min_max(pos2(mid + 2.0, video_area.min.y), pos2(video_area.max.x, video_area.max.y - bar_h)),
        )
    } else {
        (None, video_area)
    };
    // ---- picture
    let ppp = ctx.pixels_per_point();
    let pic = if compare {
        fit(video_area, frame_size.0 as f32, frame_size.1 as f32)
    } else if show_picture {
        monitor_view::picture_rect(video_area, frame_size.0 as f32, frame_size.1 as f32, &mv, ppp)
    } else {
        video_area
    };
    let saved_clip = ui.clip_rect();
    ui.set_clip_rect(saved_clip.intersect(if mv.zoom.is_some() && !compare { video_area } else { picture_area.expand(12.0) }));
    if show_picture {
        ui.painter().rect_filled(pic, 0.0, t.monitor_bg);
        let playing = which == Which::Program && app.playback.playing;
        let res = mv.effective_res(playing);
        let screen_scale = (pic.width() * ppp / frame_size.0 as f32).min(1.0);
        let scale = quantize_scale(res.scale().min(screen_scale.max(1.0 / 32.0)));
        let frame = rate.frame_at(time);
        let rev = match target {
            Target::Item(i) => app.item_revision(i),
            _ => app.session.revision,
        };
        let size_key = (scale * 1000.0) as u32;
        let channel = display.is_channel();
        let use_gpu = which == Which::Program && app.gpu.is_some() && !channel && !crate::panels::menu_dialogs::software_renderer(app);
        let cpu_target = target;
        let target = match (use_gpu, target) {
            (true, Target::Sequence(s)) => Target::SequencePlan(s),
            (_, t) => t,
        };
        // Draft decoding (opt-in): only while playing at 1/2 resolution or lower.
        let draft = crate::frames::draft_playback(playing, scale, app.session.prefs.playback.draft_decode);
        let key = FrameKey { target, frame, size: size_key, revision: rev, draft };
        let project = app.session.project.clone();
        if playing {
            let preroll = app.playback.preroll.is_some();
            app.frames.schedule_playback(key, rate, scale, &project, app.playback.speed, preroll);
            if preroll {
                app.playback.preroll_ready = app.frames.preroll_ready(key, app.playback.speed, rate.frame_at(duration) - 1);
            }
        } else {
            app.frames.request(key, rate.tick_of(frame), scale, &project, 0);
        }
        let asked = asked_before(app, prefix, key, playing);
        let tex_name = if channel { format!("monitor-{prefix}-{display:?}") } else { format!("monitor-{prefix}") };
        let (shown, on_screen) = if use_gpu {
            let found = app
                .frames
                .get_plan(&key)
                .map(|p| (key, p))
                .or_else(|| app.frames.nearest_plan(key, 6))
                .or_else(|| asked.iter().find_map(|k| app.frames.get_plan(k).map(|p| (*k, p))));
            match found {
                Some((k, plan)) => (app.gpu_present(k, &plan).map(|(id, _)| id), Some(k)),
                None => (app.gpu.as_ref().and_then(|g| g.texture), app.gpu.as_ref().and_then(|g| g.last_key)),
            }
        } else {
            cpu_texture(app, &ctx, &tex_name, key, &asked, display)
        };
        let exact = on_screen == Some(key);
        if which == Which::Program {
            app.program_shown = on_screen;
        }
        if !playing && !exact {
            app.monitor_inexact = true;
        }
        if playing {
            if std::mem::take(&mut app.playback.hidden) {
                app.playback.meter.resync(frame, exact);
            } else {
                app.playback.meter.refresh(frame, exact);
            }
        }
        if let Some(tex) = shown {
            ui.painter().image(tex, pic, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        }
        if let Some(ra) = ref_area {
            // the reference frame (CPU path)
            let rf = rate.frame_at(Tick(mv.compare_ref.unwrap_or(time.0)));
            let rkey = FrameKey { target: cpu_target, frame: rf, size: size_key, revision: rev, draft: false };
            app.frames.request(rkey, rate.tick_of(rf), scale, &project, 1);
            let rpic = fit(ra, frame_size.0 as f32, frame_size.1 as f32);
            ui.painter().rect_filled(rpic, 0.0, t.monitor_bg);
            if let (Some(tex), _) = cpu_texture(app, &ctx, &format!("monitor-{prefix}-ref"), rkey, &[], DisplayMode::Composite) {
                ui.painter().image(tex, rpic, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            }
            app.auto.add("program.compare.reference", rpic, "reference frame");
            let bar = Rect::from_min_max(pos2(ra.min.x, ra.max.y + 2.0), pos2(video_area.max.x, ra.max.y + 22.0));
            monitor_view::compare_bar(app, ui, bar, rate, drop_frame);
        }
        if mv.safe_margins {
            for (f, c) in [(0.9, Color32::from_white_alpha(120)), (0.8, Color32::from_white_alpha(90))] {
                let r = Rect::from_center_size(pic.center(), pic.size() * f);
                ui.painter().rect_stroke(r, 0.0, Stroke::new(1.0, c), StrokeKind::Middle);
            }
            let c = pic.center();
            ui.painter().line_segment([c - vec2(8.0, 0.0), c + vec2(8.0, 0.0)], Stroke::new(1.0, Color32::from_white_alpha(120)));
            ui.painter().line_segment([c - vec2(0.0, 8.0), c + vec2(0.0, 8.0)], Stroke::new(1.0, Color32::from_white_alpha(120)));
        }
        if display != DisplayMode::Composite && !compare {
            let label = format!("{display:?}");
            ui.painter().text(pos2(video_area.max.x - 8.0, video_area.min.y + 6.0), Align2::RIGHT_TOP, label, Tokens::ui(10.0), t.text_dim);
        }
    }
    // Dropped-frame indicator (Program only)
    if which == Which::Program && app.playback.playing {
        let c = if app.playback.meter.counts().1 > 0 { t.render_yellow } else { t.render_green };
        ui.painter().circle_filled(pos2(video_area.min.x + 10.0, video_area.min.y + 10.0), 4.0, c);
    }
    // Click/drag in the picture: the Hand tool pans a magnified picture, otherwise focus.
    let pic_resp = ui.interact(pic, egui::Id::new((prefix, "pic")), Sense::click());
    app.auto.add(&format!("{prefix}.picture"), pic.intersect(video_area), "picture");
    if show_picture {
        monitor_view::pan_input(app, ui, which, video_area, pic);
    }
    if which == Which::Program && show_picture {
        crate::panels::graphics::monitor_overlay(app, ui, pic, frame_size);
        crate::panels::masks::monitor_overlay(app, ui, pic, frame_size);
    }
    if show_picture {
        monitor_view::guides(app, ui, which, video_area, pic, frame_size);
    }
    ui.set_clip_rect(saved_clip);
    if let Some((top, left)) = ruler_rects {
        monitor_view::rulers(app, ui, which, top, left, video_area, pic, frame_size);
    }
    if pic_resp.double_clicked() && which == Which::Source {
        // (Premiere opens the clip's settings; we show info)
        app.ui.status = name.clone();
    }

    // ---- controls row: timecode | zoom | res | wrench | duration
    let row1 = Rect::from_min_size(pos2(rect.min.x + 14.0, rect.max.y - controls_h + 2.0), vec2(rect.width() - 28.0, 26.0));
    let tc = format_time(time, rate, drop_frame, TimeDisplay::Timecode, 48000);
    ui.painter().text(pos2(row1.min.x, row1.center().y), Align2::LEFT_CENTER, &tc, Tokens::timecode(), t.timecode);
    app.auto.add(&format!("{prefix}.timecode"), Rect::from_min_size(row1.min, vec2(110.0, row1.height())), &tc);
    let dur_tc = format_time(
        mark_out.map(|o| o + rate.frame_duration()).unwrap_or(duration) - mark_in.unwrap_or(Tick::ZERO),
        rate,
        drop_frame,
        TimeDisplay::Timecode,
        48000,
    );
    ui.painter().text(pos2(row1.max.x, row1.center().y), Align2::RIGHT_CENTER, &dur_tc, Tokens::timecode(), t.text_dim);
    // zoom + resolution dropdowns centred-ish
    let zr = Rect::from_min_size(pos2(row1.min.x + 116.0, row1.min.y), vec2(70.0, 24.0));
    let zoom_label = match mv.zoom {
        None => "Fit".to_string(),
        Some(z) => format!("{}%", (z * 100.0).round() as i32),
    };
    let zresp = crate::widgets::dropdown_text(ui, zr, &zoom_label, &t, egui::Id::new((prefix, "zoom")));
    app.auto.add(&format!("{prefix}.zoom"), zr, "Select Zoom Level");
    monitor_view::zoom_menu(app, &zresp, which);
    let res = mv.res;
    let rr = Rect::from_min_size(pos2(row1.max.x - 196.0, row1.min.y), vec2(62.0, 24.0));
    let rresp = crate::widgets::dropdown_text(ui, rr, res.label(), &t, egui::Id::new((prefix, "res")));
    app.auto.add(&format!("{prefix}.resolution"), rr, "Select Playback Resolution");
    egui::Popup::menu(&rresp).show(|ui| {
        for (k, r) in monitor_view::RES_NAMES {
            let o = ui.selectable_label(r == res, r.label());
            app.auto.add(&format!("{prefix}.resolution.{k}"), o.rect, r.label());
            if o.clicked() {
                monitor_view::view_mut(app, which).res = r;
            }
        }
    });
    let wr = Rect::from_min_size(pos2(rr.max.x + 6.0, row1.min.y + 1.0), vec2(22.0, 22.0));
    let wresp = ui.interact(wr, egui::Id::new((prefix, "wrench")), Sense::click());
    icons::paint(ui.painter(), wr.shrink(4.0), Icon::Wrench, if wresp.hovered() { t.tab_text_active } else { t.icon });
    app.auto.add(&format!("{prefix}.settings"), wr, "Settings");
    egui::Popup::menu(&wresp).show(|ui| {
        ui.set_min_width(240.0);
        monitor_view::wrench_items(app, ui, which);
        let mv = monitor_view::view_mut(app, which);
        let r = ui.checkbox(&mut mv.safe_margins, "Safe Margins");
        app.auto.add(&format!("{prefix}.settings.safeMargins"), r.rect, "Safe Margins");
        let mv = monitor_view::view_mut(app, which);
        let r = ui.checkbox(&mut mv.show_transport, "Show Transport Controls");
        app.auto.add(&format!("{prefix}.settings.showTransport"), r.rect, "Show Transport Controls");
        if which == Which::Program {
            ui.separator();
            // shows / closes the Lumetri Scopes panel
            let mut scopes = app.ui.dock.contains(crate::dock::PanelKind::LumetriScopes);
            let r = ui.checkbox(&mut scopes, "Lumetri Scopes");
            app.auto.add("program.settings.lumetriScopes", r.rect, "Lumetri Scopes");
            if r.changed() {
                app.ui.show_scopes = scopes;
                if scopes {
                    app.show_panel(crate::dock::PanelKind::LumetriScopes);
                } else {
                    app.ui.dock.close(crate::dock::PanelKind::LumetriScopes);
                }
            }
            let r = ui.checkbox(&mut app.playback.looping, "Loop");
            app.auto.add("program.settings.playback.loop", r.rect, "Loop");
            ui.separator();
            let mut errors: Vec<String> = Vec::new();
            let mut follows = app.session.state.multicam_audio_follows_video;
            let r = ui.checkbox(&mut follows, "Multi-Camera Audio Follows Video");
            app.auto.add("program.settings.multicam.audioFollowsVideo", r.rect, "Multi-Camera Audio Follows Video");
            if r.changed()
                && let Err(e) = app.session.execute("multicam.audioFollowsVideo", json!({"enabled": follows}))
            {
                errors.push(e.to_string());
            }
            let r = ui.checkbox(&mut app.ui.multicam_record, "Multi-Camera Record");
            app.auto.add("program.settings.multicam.recordToggle", r.rect, "Multi-Camera Record");
            let v = app.session.state.multicam_view.clone();
            for (cmd, label, on) in [
                ("multicam.selectionTopDown", "Multi-Camera Selection Top Down", v.top_down),
                ("multicam.showPreviewMonitor", "Show Multi-Camera Preview Monitor", v.show_preview),
                ("multicam.autoAdjustQuality", "Auto-Adjust Multi-Camera Playback Quality", v.auto_quality),
                ("multicam.transmitView", "Transmit Multi-Camera View", v.transmit),
            ] {
                let mut b = on;
                let r = ui.checkbox(&mut b, label);
                app.auto.add(&format!("program.settings.{cmd}"), r.rect, label);
                if r.changed()
                    && let Err(e) = app.session.execute(cmd, json!({"enabled": b}))
                {
                    errors.push(e.to_string());
                }
            }
            let sub = ui.menu_button("Multi-Camera Layout", |ui| {
                for (k, label) in [("auto", "Automatic"), ("2x2", "2 × 2"), ("3x3", "3 × 3"), ("4x4", "4 × 4")] {
                    let o = ui.selectable_label(v.layout_name() == k, label);
                    app.auto.add(&format!("program.settings.multicam.gridLayout.{k}"), o.rect, label);
                    if o.clicked()
                        && let Err(e) = app.session.execute("multicam.gridLayout", json!({"layout": k}))
                    {
                        errors.push(e.to_string());
                    }
                }
            });
            app.auto.add("program.settings.multicam.gridLayout", sub.response.rect, "Multi-Camera Layout");
            let b = ui.button("Edit Cameras…");
            app.auto.add("program.settings.multicam.editCameras", b.rect, "Edit Cameras…");
            if b.clicked() {
                if let Some(Err(e)) = crate::panels::multicam::route(app, "multicam.editCamerasDialog", &json!({})) {
                    errors.push(e);
                }
                ui.close();
            }
            if let Some(e) = errors.pop() {
                app.ui.status = e;
            }
        }
    });
    // ---- mini timeline / scrub bar
    let bar = Rect::from_min_size(pos2(rect.min.x + 14.0, row1.max.y + 2.0), vec2(rect.width() - 28.0, 22.0));
    mini_timeline(app, ui, bar, which, origin, time, duration, rate, mark_in, mark_out);

    // ---- transport buttons (wrench ▸ Show Transport Controls)
    let row3 = Rect::from_min_size(pos2(rect.min.x, bar.max.y + 4.0), vec2(rect.width(), 32.0));
    if monitor_view::view(app, which).show_transport {
        transport(app, ui, row3, which);
    }
}

/// How many of a paused monitor's latest requests are remembered ([`asked_before`]): about a
/// second of refreshes, so a frame that took several refreshes to render is still known when it
/// is ready.
const ASKED_KEYS: usize = 64;

/// Note that the monitor `prefix` asks for `key`, and return the frames it asked for on the passes
/// before, newest first: the stand-ins for `key` while it renders.
///
/// A value that is dragged asks for a new revision on every pass, and a scrub for a new frame, so
/// the frame asked for is never the one that is ready; the one asked for a pass earlier usually
/// is, and showing it keeps the picture one pass behind the mouse instead of frozen until the
/// mouse rests. Playback has its own schedule and remembers nothing (what was on screen before
/// Play is no stand-in for where it stopped).
fn asked_before(app: &mut FilmcraftApp, prefix: &str, key: FrameKey, playing: bool) -> Vec<FrameKey> {
    let asked = app.monitor_asked.entry(prefix.to_string()).or_default();
    if playing {
        asked.clear();
        return Vec::new();
    }
    // another sequence or clip: its pictures are not this one's
    asked.retain(|k| k.target == key.target && *k != key);
    let before: Vec<FrameKey> = asked.iter().rev().copied().collect();
    asked.push_back(key);
    while asked.len() > ASKED_KEYS {
        asked.pop_front();
    }
    before
}

/// A monitor texture from the CPU frame path: the exact frame, else the nearest cached one before
/// it, else the newest ready one of `asked` (see [`asked_before`]), with the display mode's
/// channel mapping. Returns the texture and the frame it shows.
fn cpu_texture(
    app: &mut FilmcraftApp,
    ctx: &egui::Context,
    name: &str,
    key: FrameKey,
    asked: &[FrameKey],
    mode: DisplayMode,
) -> (Option<egui::TextureId>, Option<FrameKey>) {
    let found = app
        .frames
        .get(&key)
        .map(|img| (key, img))
        .or_else(|| (1..=6).map(|d| FrameKey { frame: key.frame.saturating_sub(d), ..key }).find_map(|k| app.frames.get(&k).map(|img| (k, img))))
        .or_else(|| asked.iter().find_map(|k| app.frames.get(k).map(|img| (*k, img))));
    match found {
        Some((k, img)) => {
            let tex = if mode.is_channel() {
                app.texture_for_mapped(ctx, name, k, &img, |i| monitor_view::channel_view(i, mode))
            } else {
                app.texture_for(ctx, name, k, &img)
            };
            (Some(tex), Some(k))
        }
        None => (app.texture_existing(name).map(|(id, _)| id), app.texture_key(name)),
    }
}

pub fn quantize_scale(s: f32) -> f32 {
    // Buckets keep the cache effective while resizing the panel.
    let buckets = [1.0 / 32.0, 1.0 / 16.0, 1.0 / 8.0, 0.1875, 0.25, 0.375, 0.5, 0.75, 1.0];
    *buckets.iter().find(|b| **b >= s - 1e-4).unwrap_or(&1.0)
}

pub fn fit(area: Rect, w: f32, h: f32) -> Rect {
    let s = (area.width() / w).min(area.height() / h);
    Rect::from_center_size(area.center(), vec2(w * s, h * s))
}

#[allow(clippy::too_many_arguments)]
fn mini_timeline(
    app: &mut FilmcraftApp,
    ui: &mut egui::Ui,
    bar: Rect,
    which: Which,
    origin: Tick,
    time: Tick,
    end: Tick,
    rate: filmcraft_time::FrameRate,
    mark_in: Option<Tick>,
    mark_out: Option<Tick>,
) {
    let t = app.tokens;
    let p = ui.painter();
    let duration = end;
    let dur = (end - origin).0.max(1) as f64;
    let xof = |tk: Tick| bar.min.x + (((tk - origin).0 as f64 / dur) as f32).clamp(0.0, 1.0) * bar.width();
    // ticks: minor 4 pt, major 10 pt, ~20 pt spacing
    let n = ((bar.width() / 20.0) as i32).max(2);
    for i in 0..=n {
        let x = bar.min.x + bar.width() * i as f32 / n as f32;
        let h = if i % 5 == 0 { 8.0 } else { 3.5 };
        p.line_segment([pos2(x, bar.max.y - h), pos2(x, bar.max.y)], Stroke::new(1.0, t.text_faint));
    }
    if mark_in.is_some() || mark_out.is_some() {
        let a = xof(mark_in.unwrap_or(origin));
        let b = xof(mark_out.map(|o| o + rate.frame_duration()).unwrap_or(duration));
        p.rect_filled(Rect::from_min_max(pos2(a, bar.min.y + 8.0), pos2(b, bar.max.y)), 0.0, Color32::from_rgb(0x5c, 0x5c, 0x5c));
    }
    // markers: the sequence's (Program), the clip's or the subclip's inherited ones (Source)
    let markers = match which {
        Which::Program => app.session.active_sequence().map(|q| q.markers.clone()),
        Which::Source => app.session.state.source_item.and_then(|i| filmcraft_engine::clip_ops::source_view(&app.session, i)).map(|v| v.markers),
    };
    if let Some(markers) = markers {
        for m in &markers {
            let x = xof(m.start);
            let c = m.color.marker_rgb();
            let c = Color32::from_rgb(c[0], c[1], c[2]);
            let y = bar.min.y;
            p.add(egui::Shape::convex_polygon(
                vec![pos2(x - 3.5, y), pos2(x + 3.5, y), pos2(x + 3.5, y + 7.0), pos2(x, y + 10.0), pos2(x - 3.5, y + 7.0)],
                c,
                Stroke::NONE,
            ));
        }
    }
    let x = xof(time);
    // playhead: blue triangle over a line
    let hy = bar.max.y - 11.0;
    p.add(egui::Shape::convex_polygon(
        vec![pos2(x - 5.5, hy), pos2(x + 5.5, hy), pos2(x + 5.5, hy + 5.0), pos2(x, hy + 9.0), pos2(x - 5.5, hy + 5.0)],
        t.playhead,
        Stroke::NONE,
    ));
    p.line_segment([pos2(x, hy + 8.0), pos2(x, bar.max.y)], Stroke::new(1.0, t.playhead));
    let resp = ui.interact(bar, egui::Id::new((which as u8, "scrub")), Sense::click_and_drag());
    app.auto.add(if which == Which::Program { "program.scrubBar" } else { "source.scrubBar" }, bar, "scrub bar");
    if (resp.dragged() || resp.clicked())
        && let Some(pos) = resp.interact_pointer_pos()
    {
        let f = ((pos.x - bar.min.x) / bar.width()).clamp(0.0, 1.0) as f64;
        let tk = rate.snap(origin + Tick((f * dur) as i64));
        match which {
            Which::Program => {
                app.stop();
                app.session.set_playhead(tk);
            }
            Which::Source => {
                let _ = app.session.execute("source.setPlayhead", json!({"time": tk.0}));
            }
        }
    }
}

/// A transport button: icon, command, tooltip.
type Button = (Icon, &'static str, &'static str);

/// A monitor's default transport buttons.
fn default_buttons(app: &FilmcraftApp, ctx: &egui::Context, src: bool) -> Vec<Button> {
    if src {
        vec![
            (Icon::Marker, "markers.add", "Add Marker (M)"),
            (Icon::MarkIn, "src.markIn", "Mark In (I)"),
            (Icon::MarkOut, "src.markOut", "Mark Out (O)"),
            (Icon::GoToIn, "src.goIn", "Go to In (Shift+I)"),
            (Icon::StepBack, "src.stepBack", "Step Back 1 Frame (Left)"),
            (if source_playing(ctx) { Icon::Pause } else { Icon::Play }, "src.play", "Play-Stop Toggle (Space)"),
            (Icon::StepFwd, "src.stepFwd", "Step Forward 1 Frame (Right)"),
            (Icon::GoToOut, "src.goOut", "Go to Out (Shift+O)"),
            (Icon::Insert, "source.insert", "Insert (,)"),
            (Icon::Overwrite, "source.overwrite", "Overwrite (.)"),
            (Icon::Camera, "exportFrame", "Export Frame (Shift+E)"),
            (Icon::Proxy, "media.toggleProxies", "Toggle Proxies"),
        ]
    } else {
        vec![
            (Icon::Marker, "markers.add", "Add Marker (M)"),
            (Icon::MarkIn, "markers.markIn", "Mark In (I)"),
            (Icon::MarkOut, "markers.markOut", "Mark Out (O)"),
            (Icon::GoToIn, "markers.goToIn", "Go to In (Shift+I)"),
            (Icon::StepBack, "playhead.stepBack", "Step Back 1 Frame (Left)"),
            (if app.playback.playing { Icon::Pause } else { Icon::Play }, "playback.toggle", "Play-Stop Toggle (Space)"),
            (Icon::StepFwd, "playhead.stepForward", "Step Forward 1 Frame (Right)"),
            (Icon::GoToOut, "markers.goToOut", "Go to Out (Shift+O)"),
            (Icon::Lift, "sequence.lift", "Lift (;)"),
            (Icon::Extract, "sequence.extract", "Extract (')"),
            (Icon::Camera, "exportFrame", "Export Frame (Shift+E)"),
            (Icon::Proxy, "media.toggleProxies", "Toggle Proxies"),
        ]
    }
}

/// The buttons the Button Editor can add to a monitor's transport bar.
fn extra_buttons(src: bool) -> Vec<Button> {
    if src {
        vec![(Icon::Grid, "view.safeMargins", "Safe Margins")]
    } else {
        vec![
            (Icon::ChevronLeft, "markers.goPrev", "Go to Previous Marker (Cmd+Shift+M)"),
            (Icon::ChevronRight, "markers.goNext", "Go to Next Marker (Shift+M)"),
            (Icon::GoToIn, "playback.inToOut", "Play In to Out (Alt+K)"),
            (Icon::Loop, "playback.loop", "Loop"),
            (Icon::Grid, "view.safeMargins", "Safe Margins"),
            (Icon::Close, "markers.clearInOut", "Clear In and Out"),
        ]
    }
}

fn transport(app: &mut FilmcraftApp, ui: &mut egui::Ui, row: Rect, which: Which) {
    let t = app.tokens;
    let src = which == Which::Source;
    let defaults = default_buttons(app, ui.ctx(), src);
    let extras = extra_buttons(src);
    let custom = if src { app.ui.panels.transport.source.clone() } else { app.ui.panels.transport.program.clone() };
    let mut buttons: Vec<Button> = defaults.iter().copied().filter(|b| !custom.hidden.iter().any(|h| h == b.1)).collect();
    buttons.extend(custom.added.iter().filter_map(|a| extras.iter().find(|b| b.1 == a.as_str()).copied()));
    let bw = 30.0;
    let total = buttons.len() as f32 * bw;
    let mut x = row.center().x - total / 2.0;
    let ctx = ui.ctx().clone();
    let prefix = if src { "source" } else { "program" };
    for (icon, cmd, tip) in buttons {
        let r = Rect::from_min_size(pos2(x, row.min.y + 2.0), vec2(bw - 2.0, 26.0));
        let resp = ui.interact(r, egui::Id::new((prefix, cmd)), Sense::click()).on_hover_text(tip);
        app.auto.add(&format!("{prefix}.transport.{cmd}"), r, tip);
        let is_play = cmd.ends_with("play") || cmd == "playback.toggle";
        if resp.hovered() {
            ui.painter().rect_filled(r, 4.0, t.hover);
        }
        let sz = if is_play { 16.0 } else { 14.0 };
        let on = match cmd {
            "media.toggleProxies" => app.session.media.use_proxies(),
            "playback.loop" => app.playback.looping,
            "view.safeMargins" => monitor_view::view(app, which).safe_margins,
            _ => false,
        };
        let col = if on {
            t.accent
        } else if resp.hovered() {
            t.tab_text_active
        } else {
            t.icon
        };
        icons::paint(ui.painter(), Rect::from_center_size(r.center(), vec2(sz, sz)), icon, col);
        if resp.clicked() {
            let r = match cmd {
                "src.markIn" => app.session.execute("markers.markIn", json!({"target": "source"})).map_err(|e| e.to_string()),
                "src.markOut" => app.session.execute("markers.markOut", json!({"target": "source"})).map_err(|e| e.to_string()),
                "src.goIn" | "src.goOut" | "src.stepBack" | "src.stepFwd" => {
                    source_nav(app, cmd);
                    Ok(serde_json::Value::Null)
                }
                "src.play" => {
                    toggle_source(app, &ctx);
                    Ok(serde_json::Value::Null)
                }
                "exportFrame" => {
                    app.ui.status = "Export Frame: use Export mode (M6)".into();
                    Ok(serde_json::Value::Null)
                }
                "view.safeMargins" => crate::menus::invoke(app, &ctx, cmd, json!({"monitor": prefix})),
                c => crate::menus::invoke(app, &ctx, c, json!({})),
            };
            if let Err(e) = r {
                app.ui.status = e;
            }
        }
        x += bw;
    }
    // Button Editor "+": shows or hides each default button and adds extra ones
    let r = Rect::from_min_size(pos2(row.max.x - 30.0, row.min.y + 4.0), vec2(22.0, 22.0));
    let resp = ui.interact(r, egui::Id::new((prefix, "btn-editor")), Sense::click()).on_hover_text("Button Editor");
    icons::paint(ui.painter(), r.shrink(5.0), Icon::Plus, if resp.hovered() { t.tab_text_active } else { t.text_dim });
    app.auto.add(&format!("{prefix}.transport.buttonEditor"), r, "Button Editor");
    button_editor(app, &resp, which, &defaults, &extras);
}

/// The Button Editor popup of a monitor's `+`: a checkbox per button (the bar's defaults, then the
/// extra ones it can take) and Reset Layout. Automation ids `<monitor>.buttonEditor.<command>`
/// and `<monitor>.buttonEditor.reset`.
fn button_editor(app: &mut FilmcraftApp, resp: &egui::Response, which: Which, defaults: &[Button], extras: &[Button]) {
    let prefix = if which == Which::Source { "source" } else { "program" };
    let mut custom = if which == Which::Source { app.ui.panels.transport.source.clone() } else { app.ui.panels.transport.program.clone() };
    let before = custom.clone();
    let mut elems: Vec<(String, Rect, String)> = Vec::new();
    egui::Popup::menu(resp).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
        ui.set_min_width(260.0);
        ui.label(egui::RichText::new("Buttons on the transport bar").weak());
        for (i, (_, cmd, tip)) in defaults.iter().chain(extras.iter()).enumerate() {
            let label = tip.split(" (").next().unwrap_or(tip);
            let default = i < defaults.len();
            let mut shown = if default { !custom.hidden.iter().any(|h| h == cmd) } else { custom.added.iter().any(|a| a == cmd) };
            let r = ui.checkbox(&mut shown, label);
            elems.push((format!("{prefix}.buttonEditor.{cmd}"), r.rect, label.to_string()));
            if r.changed() {
                let list = if default { &mut custom.hidden } else { &mut custom.added };
                list.retain(|x| x != cmd);
                // a default button shows unless hidden; an extra one only once added
                if shown != default {
                    list.push(cmd.to_string());
                }
            }
        }
        ui.separator();
        let b = ui.button("Reset Layout");
        elems.push((format!("{prefix}.buttonEditor.reset"), b.rect, "Reset Layout".into()));
        if b.clicked() {
            custom = Default::default();
        }
    });
    for (id, r, l) in elems {
        app.auto.add(&id, r, &l);
    }
    if custom != before {
        if which == Which::Source {
            app.ui.panels.transport.source = custom;
        } else {
            app.ui.panels.transport.program = custom;
        }
    }
}

fn source_nav(app: &mut FilmcraftApp, cmd: &str) {
    let Some(item) = app.session.state.source_item else { return };
    let Some(v) = filmcraft_engine::clip_ops::source_view(&app.session, item) else { return };
    let rate = v.rate;
    let cur = app.session.state.source_playhead;
    let t = match cmd {
        "src.goIn" => v.mark_in.unwrap_or(v.start),
        "src.goOut" => v.mark_out.unwrap_or(v.end - rate.frame_duration()),
        "src.stepBack" => cur - rate.frame_duration(),
        _ => cur + rate.frame_duration(),
    };
    let _ = app.session.execute("source.setPlayhead", json!({"time": t.0}));
}
