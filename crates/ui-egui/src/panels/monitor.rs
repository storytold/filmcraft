//! Source and Program monitors.
//!
//! Frames come from the background [`FrameServer`](crate::frames::FrameServer) at the *smaller* of
//! the playback resolution and the on-screen resolution (so a small monitor never pays for 4K).
//! While playing we prefetch the next frames in priority order; while scrubbing the exact frame is
//! requested first and the nearest cached frame is shown until it arrives (no black flashes), or
//! failing that the frame asked for a refresh earlier, so the picture follows a scrub or a value
//! that is dragged instead of waiting for the mouse to rest.

use egui::{Align2, Color32, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use filmcraft_project::ItemKind;
use filmcraft_time::{Tick, TimeDisplay, format_time};
use serde_json::json;

use crate::FilmcraftApp;
use crate::frames::{FrameKey, Target};
use crate::icons::{self, Icon};
use crate::panels::monitor_view;
use crate::state::{DisplayMode, PlaybackRes};
use crate::theme::Tokens;

#[derive(Clone, Copy, PartialEq)]
pub enum Which {
    Source,
    Program,
}

pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect, which: Which) {
    let t = app.tokens;
    let ctx = ui.ctx().clone();
    let stacked_controls = rect.width() < 540.0;
    let separate_source_drag = which == Which::Source && stacked_controls && rect.width() < 310.0;
    let source_drag_h = if separate_source_drag { 26.0 } else { 0.0 };
    let controls_h = 28.0 + 24.0 + 15.0 + 36.0 + if stacked_controls { 26.0 } else { 0.0 } + source_drag_h;
    let video_area = Rect::from_min_max(rect.min + vec2(4.0, 4.0), pos2(rect.max.x - 4.0, rect.max.y - controls_h));
    // the Source monitor's time ruler starts at `origin` (a subclip restricted to its range)
    let mut origin = Tick::ZERO;
    let (target, frame_size, rate, time, duration, drop_frame, mark_in, mark_out, name) = match which {
        Which::Program => {
            let Some(seq_id) = app.session.state.active_sequence else {
                crate::dock::placeholder(ui, rect, &t, tl!("(no sequences)"));
                return;
            };
            // A damaged project can name an active sequence that no longer exists.
            let Some(q) = app.session.active_sequence() else {
                crate::dock::placeholder(ui, rect, &t, tl!("(no sequences)"));
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
                crate::dock::placeholder(ui, rect, &t, tl!("(no clips)"));
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
            let mut view = view;
            if let Some(range) = crate::panels::source_range::preview(app, &ctx) {
                view.mark_in = Some(range.start);
                view.mark_out = Some(Tick(range.end().0.saturating_sub(view.rate.frame_duration().0)).max(view.start));
            }
            origin = view.start;
            (Target::Item(item), size, view.rate, app.session.state.source_playhead, view.end, false, view.mark_in, view.mark_out, pi.name.clone())
        }
    };
    // Display implicit full-clip marks without writing them into the project on open.
    let draft = crate::panels::source_range::preview_for(app, &ctx, which);
    let (mark_in, mark_out) = if which == Which::Source {
        (
            Some(draft.map(|r| r.start).unwrap_or(mark_in.unwrap_or(origin))),
            Some(
                draft
                    .map(|r| Tick(r.end().0.saturating_sub(rate.frame_duration().0)))
                    .unwrap_or_else(|| mark_out.unwrap_or_else(|| (duration - rate.frame_duration()).max(origin))),
            ),
        )
    } else {
        (
            mark_in.map(|m| draft.map(|r| r.start).unwrap_or(m)),
            mark_out.map(|m| draft.map(|r| Tick(r.end().0.saturating_sub(rate.frame_duration().0))).unwrap_or(m)),
        )
    };
    let range_start = mark_in.unwrap_or(origin);
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
        let shown = ruler_shown(mv.time_zoom, origin, duration);
        monitor_view::waveform(app, ui, wa, item, time, shown, mark_in, mark_out);
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
    // ---- picture, at its display aspect (non-square pixels are stretched on screen)
    let ppp = ctx.pixels_per_point();
    let display_w = (frame_size.0 as f64 * monitor_view::pixel_aspect(app, which)) as f32;
    let pic = if compare {
        fit(video_area, display_w, frame_size.1 as f32)
    } else if show_picture {
        monitor_view::picture_rect(video_area, display_w, frame_size.1 as f32, &mv, ppp)
    } else {
        video_area
    };
    let saved_clip = ui.clip_rect();
    ui.set_clip_rect(saved_clip.intersect(if mv.zoom.is_some() && !compare { video_area } else { picture_area.expand(12.0) }));
    if show_picture {
        ui.painter().rect_filled(pic, 0.0, t.monitor_bg);
        let playing = if which == Which::Program { app.playback.playing } else { app.source_playback.clock.playing };
        let res = mv.effective_res(playing);
        // frame pixels per screen pixel along the axis that needs the most of them
        let screen_scale = (pic.width() * ppp / frame_size.0.max(1) as f32).max(pic.height() * ppp / frame_size.1.max(1) as f32).min(1.0);
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
            let preroll = which == Which::Program && app.playback.preroll.is_some();
            let speed = if which == Which::Program { app.playback.speed } else { app.source_playback.clock.speed };
            app.frames.schedule_playback(key, rate, scale, &project, speed, preroll);
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
            let clock = if which == Which::Program { &mut app.playback } else { &mut app.source_playback.clock };
            if std::mem::take(&mut clock.hidden) {
                clock.meter.resync(frame, exact);
            } else {
                clock.meter.refresh(frame, exact);
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
            let rpic = fit(ra, display_w, frame_size.1 as f32);
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
            let label = match display {
                DisplayMode::Composite => tl!("Composite"),
                DisplayMode::Alpha => tl!("Alpha"),
                DisplayMode::Red => tl!("Red"),
                DisplayMode::Green => tl!("Green"),
                DisplayMode::Blue => tl!("Blue"),
                DisplayMode::AudioWaveform => tl!("Audio Waveform"),
                DisplayMode::Comparison => tl!("Comparison"),
                DisplayMode::VideoAndWaveform => tl!("Video and Waveform"),
            };
            ui.painter().text(pos2(video_area.max.x - 8.0, video_area.min.y + 6.0), Align2::RIGHT_TOP, label, Tokens::ui(10.0), t.text_dim);
        }
    }
    // Dropped-frame indicator (Program only)
    if which == Which::Program && app.playback.playing {
        let c = if app.playback.meter.counts().1 > 0 { t.render_yellow } else { t.render_green };
        ui.painter().circle_filled(pos2(video_area.min.x + 10.0, video_area.min.y + 10.0), 4.0, c);
    }
    // A click on the Program monitor around the picture lets go of what is selected on it (#683;
    // in the picture the overlays do that). Under the picture and the overlays, so they come first.
    if which == Which::Program && show_picture && ui.interact(video_area, egui::Id::new((prefix, "background")), Sense::click()).clicked() {
        crate::panels::graphics::deselect_on_monitor(app, ui);
    }
    // Click/drag in the picture: the Hand tool pans a magnified picture, otherwise focus.
    let pic_resp = ui.interact(pic, egui::Id::new((prefix, "pic")), if show_picture { Sense::click_and_drag() } else { Sense::hover() });
    app.auto.add(&format!("{prefix}.picture"), pic.intersect(video_area), "picture");
    // (Comparison View always fits both frames)
    if show_picture && !compare {
        monitor_view::zoom_pan_input(app, ui, which, video_area, pic, display_w, ppp);
    }
    if which == Which::Program && show_picture {
        // an armed eyedropper owns clicks on the picture: the overlays would sit on top and take
        // them (the selected clip's transform handles cover it, #1023)
        if app.ui.eyedropper.is_some() {
            eyedropper(app, ui, &pic_resp, pic, target, rate, time);
        } else {
            crate::panels::graphics::monitor_overlay(app, ui, pic, frame_size);
            crate::panels::masks::monitor_overlay(app, ui, pic, frame_size);
            crate::panels::transform_handles::monitor_overlay(app, ui, pic, frame_size);
        }
    }
    if show_picture {
        monitor_view::guides(app, ui, which, video_area, pic, frame_size);
    }
    ui.set_clip_rect(saved_clip);
    if let Some((top, left)) = ruler_rects {
        monitor_view::rulers(app, ui, which, top, left, video_area, pic, frame_size);
    }
    if which == Which::Source && show_picture && pic_resp.drag_started() && app.ui.tool != crate::state::Tool::Hand {
        crate::panels::source_drag::begin(app, ui, true, true);
    }
    if pic_resp.double_clicked() && which == Which::Source {
        // (Premiere opens the clip's settings; we show info)
        app.ui.status = name.clone();
    }

    // ---- controls row: timecode | zoom | res | wrench | duration
    let row1 = Rect::from_min_size(pos2(rect.min.x + 14.0, rect.max.y - controls_h + 2.0), vec2(rect.width() - 28.0, 26.0));
    let tc = format_time(time, rate, drop_frame, TimeDisplay::Timecode, 48000);
    let tc_rect = Rect::from_min_size(row1.min, vec2(110.0, row1.height()));
    let min = if which == Which::Program { Tick::ZERO } else { origin };
    match crate::widgets::timecode_field(ui, egui::Id::new((prefix, "timecode")), tc_rect, &tc, time, min, rate, drop_frame, t.timecode) {
        Some(Ok(to)) => {
            let cmd = if which == Which::Program { "playhead.set" } else { "source.setPlayhead" };
            if let Err(e) = app.session.execute(cmd, json!({"time": to.0})) {
                app.ui.status = e.to_string();
            }
        }
        Some(Err(e)) => app.ui.status = e,
        None => {}
    }
    app.auto.add(&format!("{prefix}.timecode"), tc_rect, &tc);
    let dur_tc = format_time(mark_out.map(|o| o + rate.frame_duration()).unwrap_or(duration) - range_start, rate, drop_frame, TimeDisplay::Timecode, 48000);
    ui.painter().text(pos2(row1.max.x, row1.center().y), Align2::RIGHT_CENTER, &dur_tc, Tokens::timecode(), t.text_dim);
    // At compact widths, timecodes and view settings need separate rows.
    let settings_y = row1.min.y + if stacked_controls { 26.0 } else { 0.0 };
    let zoom_x = if stacked_controls && which == Which::Source && !separate_source_drag {
        row1.min.x
    } else if stacked_controls {
        row1.center().x - 83.0
    } else {
        row1.min.x + 116.0
    };
    let zr = Rect::from_min_size(pos2(zoom_x, settings_y), vec2(70.0, 24.0));
    let zoom_label = match mv.zoom {
        None => tl!("Fit").to_string(),
        Some(z) => format!("{}%", (z * 100.0).round() as i32),
    };
    let zresp = crate::widgets::dropdown_text(ui, zr, &zoom_label, &t, egui::Id::new((prefix, "zoom")));
    app.auto.add(&format!("{prefix}.zoom"), zr, "Select Zoom Level");
    monitor_view::zoom_menu(app, &zresp, which);
    let res = mv.res;
    let resolution_x = if stacked_controls && which == Which::Source && !separate_source_drag {
        row1.max.x - 90.0
    } else if stacked_controls {
        zr.max.x + 6.0
    } else {
        row1.max.x - 196.0
    };
    let rr = Rect::from_min_size(pos2(resolution_x, settings_y), vec2(62.0, 24.0));
    if which == Which::Source && rr.min.x > zr.max.x + 12.0 {
        crate::panels::source_drag::controls(app, ui, Rect::from_min_max(pos2(zr.max.x + 6.0, settings_y), pos2(rr.min.x - 6.0, settings_y + 24.0)));
    }
    if separate_source_drag {
        crate::panels::source_drag::controls(app, ui, Rect::from_min_size(pos2(row1.center().x - 52.0, settings_y + 26.0), vec2(104.0, 24.0)));
    }
    let rresp = crate::widgets::dropdown_text(ui, rr, crate::i18n::t(res.label()), &t, egui::Id::new((prefix, "res")));
    app.auto.add(&format!("{prefix}.resolution"), rr, "Select Playback Resolution");
    egui::Popup::menu(&rresp).show(|ui| {
        for r in PlaybackRes::ALL {
            if ui.selectable_label(r == res, crate::i18n::t(r.label())).clicked() {
                monitor_view::view_mut(app, which).res = r;
            }
        }
    });
    let wr = Rect::from_min_size(pos2(rr.max.x + 6.0, settings_y + 1.0), vec2(22.0, 22.0));
    let wresp = ui.interact(wr, egui::Id::new((prefix, "wrench")), Sense::click());
    icons::paint(ui.painter(), wr.shrink(4.0), Icon::Wrench, if wresp.hovered() { t.tab_text_active } else { t.icon });
    app.auto.add(&format!("{prefix}.settings"), wr, "Settings");
    egui::Popup::menu(&wresp).show(|ui| {
        ui.set_min_width(240.0);
        monitor_view::wrench_items(app, ui, which);
        let mv = monitor_view::view_mut(app, which);
        ui.checkbox(&mut mv.safe_margins, tl!("Safe Margins"));
        ui.checkbox(&mut mv.show_transport, tl!("Show Transport Controls"));
        if which == Which::Source {
            let mut looping = app.source_playback.clock.looping;
            let response = ui.checkbox(&mut looping, tl!("Loop marked range"));
            app.auto.add("source.settings.loop", response.rect, "Loop marked range");
            if response.changed()
                && let Err(e) = crate::menus::invoke(app, &ctx, "playback.loop", json!({"monitor":"source"}))
            {
                app.ui.status = e;
            }
        }
        if which == Which::Program {
            ui.separator();
            ui.checkbox(&mut app.ui.show_scopes, tl!("Lumetri Scopes"));
            ui.checkbox(&mut app.playback.looping, tl!("Loop"));
            ui.separator();
            let mut follows = app.session.state.multicam_audio_follows_video;
            if ui.checkbox(&mut follows, tl!("Multi-Camera Audio Follows Video")).changed() {
                let _ = app.session.execute("multicam.audioFollowsVideo", json!({"enabled": follows}));
            }
            ui.checkbox(&mut app.ui.multicam_record, tl!("Multi-Camera Record"));
            let v = app.session.state.multicam_view.clone();
            for (cmd, label, on) in [
                ("multicam.selectionTopDown", tl!("Multi-Camera Selection Top Down"), v.top_down),
                ("multicam.showPreviewMonitor", tl!("Show Multi-Camera Preview Monitor"), v.show_preview),
                ("multicam.autoAdjustQuality", tl!("Auto-Adjust Multi-Camera Playback Quality"), v.auto_quality),
                ("multicam.transmitView", tl!("Transmit Multi-Camera View"), v.transmit),
            ] {
                let mut b = on;
                if ui.checkbox(&mut b, label).changed() {
                    let _ = app.session.execute(cmd, json!({"enabled": b}));
                }
            }
            ui.menu_button(tl!("Multi-Camera Layout"), |ui| {
                for (k, label) in [("auto", tl!("Automatic")), ("2x2", "2 × 2"), ("3x3", "3 × 3"), ("4x4", "4 × 4")] {
                    if ui.selectable_label(v.layout_name() == k, label).clicked() {
                        let _ = app.session.execute("multicam.gridLayout", json!({"layout": k}));
                    }
                }
            });
            if ui.button(tl!("Edit Cameras…")).clicked() {
                let _ = crate::panels::multicam::route(app, "multicam.editCamerasDialog", &json!({}));
                ui.close();
            }
        }
    });
    // ---- mini timeline / scrub bar
    let bar = Rect::from_min_size(pos2(rect.min.x + 14.0, settings_y + 28.0 + source_drag_h), vec2(rect.width() - 28.0, 22.0));
    let zoom_bar = Rect::from_min_size(pos2(bar.min.x, bar.max.y + 2.0), vec2(bar.width(), 13.0));
    follow_playhead(app, which, origin, duration, time);
    time_zoom_bar(app, ui, zoom_bar, which, origin, duration, rate);
    mini_timeline(app, ui, bar, which, origin, time, duration, rate, mark_in, mark_out);

    // ---- transport buttons
    let row3 = Rect::from_min_size(pos2(rect.min.x, zoom_bar.max.y + 4.0), vec2(rect.width(), 32.0));
    transport(app, ui, row3, which);
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

/// A click of an armed eyedropper waiting for its frame to render.
#[derive(Clone)]
struct EyedropperPick {
    /// The clicked point in the picture, 0..1 across and down.
    uv: (f32, f32),
    key: FrameKey,
    time: Tick,
    /// The project with the armed effect switched off, so a colour that effect keys out can be picked.
    project: std::sync::Arc<filmcraft_project::Project>,
}

/// Output scale of the frame an eyedropper samples (half size is plenty and quick to render).
const EYEDROPPER_SCALE: f32 = 0.5;

/// Eyedropper: while a colour parameter's eyedropper is armed (Effect Controls), the Program picture
/// takes a crosshair and a click fills that parameter with the colour under the pointer. The colour is
/// read from a render of the sequence at the playhead with the armed effect switched off (an Ultra Key
/// has already removed the green it is asked to key); Esc disarms.
fn eyedropper(app: &mut FilmcraftApp, ui: &mut egui::Ui, resp: &egui::Response, pic: Rect, target: Target, rate: filmcraft_time::FrameRate, time: Tick) {
    let Some(armed) = app.ui.eyedropper.clone() else { return };
    let ctx = ui.ctx().clone();
    let pending_id = egui::Id::new("eyedropper-pending");
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        app.ui.eyedropper = None;
        ctx.data_mut(|d| d.remove::<EyedropperPick>(pending_id));
        return;
    }
    if resp.hovered() {
        ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
    }
    if resp.clicked()
        && let (Some(pos), Target::Sequence(seq)) = (resp.interact_pointer_pos(), target)
        && pic.width() > 0.0
        && pic.height() > 0.0
    {
        let uv = (((pos.x - pic.min.x) / pic.width()).clamp(0.0, 1.0), ((pos.y - pic.min.y) / pic.height()).clamp(0.0, 1.0));
        let mut project = (*app.session.project).clone();
        if let Some(e) =
            project.sequence_mut(seq).and_then(|q| q.find_item_mut(filmcraft_project::ClipId(armed.clip))).and_then(|(_, it)| it.effects.get_mut(armed.effect))
        {
            e.enabled = false;
        }
        let frame = rate.frame_at(time);
        // a revision of its own, so this render never replaces what the monitor caches
        let revision = app.session.revision ^ 0x5eed_d70b_0000_0000 ^ armed.clip.rotate_left(17) ^ armed.effect as u64;
        let key = FrameKey { target, frame, size: (EYEDROPPER_SCALE * 1000.0) as u32, revision, draft: false };
        ctx.data_mut(|d| d.insert_temp(pending_id, EyedropperPick { uv, key, time: rate.tick_of(frame), project: std::sync::Arc::new(project) }));
    }
    let Some(pick) = ctx.data(|d| d.get_temp::<EyedropperPick>(pending_id)) else { return };
    app.frames.request(pick.key, pick.time, EYEDROPPER_SCALE, &pick.project, 0);
    let Some(img) = app.frames.get(&pick.key) else {
        ctx.request_repaint();
        return;
    };
    ctx.data_mut(|d| d.remove::<EyedropperPick>(pending_id));
    app.ui.eyedropper = None;
    let Some(rgb) = sample_rgb(&img, pick.uv) else {
        app.ui.status = tl!("Eyedropper: no picture to sample").to_string();
        return;
    };
    // the parameter stores linear values, like the colour picker beside it
    let lin = egui::Rgba::from(Color32::from_rgb(rgb[0], rgb[1], rgb[2]));
    let mut params = json!({"clip": armed.clip, "effect": armed.effect, "param": armed.param, "value": [lin.r(), lin.g(), lin.b(), 1.0]});
    if let Some(k) = armed.mask {
        params["mask"] = json!(k);
    }
    if let Err(e) = app.session.execute("effects.setParam", params) {
        app.ui.status = e.to_string();
    }
}

/// The average colour of the 5×5 pixels around `uv` (0..1 across and down) of a frame, so a noisy
/// green screen gives its true green. `None` for an empty frame.
fn sample_rgb(img: &crate::frames::Rgba, uv: (f32, f32)) -> Option<[u8; 3]> {
    if img.w == 0 || img.h == 0 || img.px.len() < img.w.saturating_mul(img.h).saturating_mul(4) {
        return None;
    }
    let cx = ((uv.0 * img.w as f32) as usize).min(img.w - 1);
    let cy = ((uv.1 * img.h as f32) as usize).min(img.h - 1);
    let (mut sum, mut n) = ([0u32; 3], 0u32);
    for y in cy.saturating_sub(2)..=(cy + 2).min(img.h - 1) {
        for x in cx.saturating_sub(2)..=(cx + 2).min(img.w - 1) {
            let i = (y * img.w + x) * 4;
            for (c, s) in sum.iter_mut().enumerate() {
                *s += u32::from(*img.px.get(i + c)?);
            }
            n += 1;
        }
    }
    let avg = |c: usize| u8::try_from(sum[c].checked_div(n)?).ok();
    Some([avg(0)?, avg(1)?, avg(2)?])
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

/// The narrowest the time zoom bar's thumb gets, the same as on the Timeline's zoom scroll bar:
/// room for its two handles and a part between them to grab.
const MIN_ZOOM_THUMB_W: f32 = 34.0;

/// The fewest frames the time ruler can be zoomed in to.
const MIN_ZOOM_FRAMES: i64 = 5;

/// A monitor's time zoom range as (start, end) fractions of the clip or sequence, made usable (it
/// can come from `ui.set`): anything empty, reversed or not a number is the whole length.
fn time_zoom_range(zoom: Option<[f64; 2]>) -> (f64, f64) {
    let (s, e) = zoom.map(|[s, e]| (s.clamp(0.0, 1.0), e.clamp(0.0, 1.0))).unwrap_or((0.0, 1.0));
    if s < e { (s, e) } else { (0.0, 1.0) }
}

/// The times (start, end) of `origin..end` that the time ruler shows at the zoom range `zoom`.
fn ruler_shown(zoom: Option<[f64; 2]>, origin: Tick, end: Tick) -> (Tick, Tick) {
    let (s, e) = time_zoom_range(zoom);
    let len = end.0.saturating_sub(origin.0).max(0) as f64;
    let at = |f: f64| Tick(origin.0.saturating_add((f * len) as i64));
    (if s > 0.0 { at(s) } else { origin }, if e < 1.0 { at(e) } else { end })
}

/// While the monitor plays, a zoomed time ruler follows the playhead `time` the way the Timeline
/// does (Settings ▸ Timeline ▸ Timeline Playback Auto-Scrolling): with page scrolling, a playhead
/// running off the right edge or jumping before the left one brings it back near the left edge;
/// with smooth scrolling it stays in the middle once it gets there. The range keeps its width.
fn follow_playhead(app: &mut FilmcraftApp, which: Which, origin: Tick, end: Tick, time: Tick) {
    let playing = if which == Which::Program { app.playback.playing } else { app.source_playback.clock.playing };
    let mode = app.session.prefs.timeline.auto_scroll.as_str();
    let zoom = monitor_view::view(app, which).time_zoom;
    let len = end.0.saturating_sub(origin.0);
    if !playing || mode == "noScroll" || zoom.is_none() || len <= 0 {
        return;
    }
    let (s, e) = time_zoom_range(zoom);
    let span = e - s;
    let f = time.0.saturating_sub(origin.0) as f64 / len as f64;
    let across = (f - s) / span;
    let start = if mode == "smoothScroll" {
        (!(0.0..=0.5).contains(&across)).then_some(f - span * 0.5)
    } else {
        (!(0.0..=0.95).contains(&across)).then_some(f - span * 0.05)
    };
    if let Some(ns) = start.map(|ns| ns.clamp(0.0, 1.0 - span)) {
        monitor_view::view_mut(app, which).time_zoom = Some([ns, (ns + span).min(1.0)]);
    }
}

/// The zoom scroll bar under the time ruler. Its thumb is the part of the clip or sequence
/// (`origin..end`) the ruler shows: drag a handle at either end to zoom, the middle to scroll, or
/// press beside the thumb to bring it there. A thumb filling the bar shows all of it.
fn time_zoom_bar(app: &mut FilmcraftApp, ui: &mut egui::Ui, bar: Rect, which: Which, origin: Tick, end: Tick, rate: filmcraft_time::FrameRate) {
    let t = app.tokens;
    let prefix = if which == Which::Program { "program" } else { "source" };
    let len = end.0.saturating_sub(origin.0);
    let min_span = if len > 0 { (rate.frame_duration().0.saturating_mul(MIN_ZOOM_FRAMES) as f64 / len as f64).min(1.0) } else { 1.0 };
    let (s, e) = time_zoom_range(monitor_view::view(app, which).time_zoom);
    let span = e - s;
    let w = (span as f32 * bar.width()).clamp(MIN_ZOOM_THUMB_W.min(bar.width()), bar.width());
    let travel = (bar.width() - w).max(0.0);
    let range = (1.0 - span).max(0.0);
    let left = bar.min.x + if range > 0.0 { (s / range).clamp(0.0, 1.0) as f32 * travel } else { 0.0 };
    let per_point = if travel > 0.5 { range / travel as f64 } else { 0.0 };
    let thumb = Rect::from_min_max(pos2(left, bar.min.y), pos2(left + w, bar.max.y));
    let p = ui.painter();
    p.rect_filled(bar, bar.height() / 2.0, t.separator);
    let hover = ui.rect_contains_pointer(thumb);
    p.rect_filled(thumb, thumb.height() / 2.0, if hover { t.scroll_thumb_hover } else { t.scroll_thumb });
    for x in [thumb.min.x + thumb.height() / 2.0, thumb.max.x - thumb.height() / 2.0] {
        p.circle_filled(pos2(x, thumb.center().y), 4.5, t.panel_bg);
        p.circle_stroke(pos2(x, thumb.center().y), 4.5, Stroke::new(1.5, t.icon_active));
    }
    app.auto.add(&format!("{prefix}.zoomBar"), thumb, "zoom scroll bar");
    app.auto.add(&format!("{prefix}.zoomBar.track"), bar, "zoom scroll bar track");
    let resp = ui.interact(bar, egui::Id::new((prefix, "zoomBar")), Sense::click_and_drag());
    // the start that brings the thumb's middle under `x`
    let jump = |x: f32| ((x - bar.min.x - w / 2.0) as f64 * per_point).clamp(0.0, range);
    let mut set = None;
    if resp.clicked()
        && per_point > 0.0
        && let Some(ns) = resp.interact_pointer_pos().filter(|pos| !thumb.contains(*pos)).map(|pos| jump(pos.x))
    {
        set = Some((ns, ns + span));
    }
    // (a drag is known to be one only after the pointer has moved: what was grabbed is what was
    // under the pointer when the button went down)
    let drag_id = egui::Id::new((prefix, "zoomBar-drag"));
    if resp.drag_started()
        && let Some(pos) = ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos())
    {
        let mut start = s;
        let mode = if !thumb.expand2(vec2(0.0, 4.0)).contains(pos) {
            if per_point > 0.0 {
                start = jump(pos.x);
                set = Some((start, start + span));
            }
            0u8
        } else if (pos.x - thumb.min.x).abs() < 9.0 {
            1
        } else if (pos.x - thumb.max.x).abs() < 9.0 {
            2
        } else {
            0
        };
        ui.data_mut(|d| d.insert_temp(drag_id, (pos.x, start, mode)));
    }
    if resp.dragged()
        && let Some((grab, start, mode)) = ui.data(|d| d.get_temp::<(f32, f64, u8)>(drag_id))
    {
        match mode {
            // the thumb's middle: scroll, the thumb staying under the pointer
            0 => {
                if let Some(pos) = resp.interact_pointer_pos() {
                    let ns = (start + (pos.x - grab) as f64 * per_point).clamp(0.0, range);
                    set = Some((ns, ns + span));
                }
            }
            // a handle: zoom, the other end staying where it is
            _ => {
                let dx = resp.drag_delta().x as f64 / bar.width().max(1.0) as f64;
                set = Some(if mode == 1 { ((s + dx).clamp(0.0, (e - min_span).max(0.0)), e) } else { (s, (e + dx).clamp((s + min_span).min(1.0), 1.0)) });
            }
        }
    }
    if resp.drag_stopped() {
        ui.data_mut(|d| d.remove::<(f32, f64, u8)>(drag_id));
    }
    if let Some((s, e)) = set {
        monitor_view::view_mut(app, which).time_zoom = (s > 0.0 || e < 1.0).then_some([s, e.min(1.0)]);
    }
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
    let duration = end;
    // the part the zoom bar shows
    let shown = ruler_shown(monitor_view::view(app, which).time_zoom, origin, end);
    let dur = (shown.1 - shown.0).0.max(1) as f64;
    let xof = |tk: Tick| bar.min.x + (((tk - shown.0).0 as f64 / dur) as f32).clamp(0.0, 1.0) * bar.width();
    // a time off a zoomed ruler is not drawn (past either end of it all, it sits at that end)
    let shows = |tk: Tick| (tk >= shown.0 || shown.0 <= origin) && (tk <= shown.1 || shown.1 >= end);
    let resp = ui.interact(bar, egui::Id::new((which as u8, "scrub")), Sense::click_and_drag());
    app.auto.add(if which == Which::Program { "program.scrubBar" } else { "source.scrubBar" }, bar, "scrub bar");
    let interaction = crate::panels::source_range::interact(app, ui, bar, which, shown);
    let p = ui.painter();
    // ticks: minor 3.5 pt high, major 8 pt, on whole frames chosen as on the Timeline's ruler (minor
    // at least 12 pt apart, major at least 110 pt), so they spread out and fill in as the ruler zooms
    let frame_px = rate.frame_duration().0 as f64 / dur * f64::from(bar.width());
    let base = rate.timecode_base();
    let steps: Vec<i64> =
        [1, 2, 5, 10, base / 2].into_iter().chain([1, 2, 5, 10, 15, 30, 60, 120, 300, 600, 1800, 3600].map(|k| base.saturating_mul(k))).collect();
    let major = steps.iter().copied().find(|s| *s > 0 && *s as f64 * frame_px >= 110.0).unwrap_or(base.saturating_mul(3600)).max(1);
    // a step that divides the major one, so every major tick is on a minor one
    let minor = steps.iter().copied().find(|s| *s > 0 && major % s == 0 && *s as f64 * frame_px >= 12.0).unwrap_or(major);
    let mut f = rate.frame_at(shown.0) / minor * minor;
    // (bounded: a ruler too short for its ticks shows the first few hundred)
    for _ in 0..512 {
        let x = bar.min.x + ((rate.tick_of(f).0 as f64 - shown.0.0 as f64) / dur) as f32 * bar.width();
        if x > bar.max.x + 0.5 {
            break;
        }
        if x >= bar.min.x - 0.5 {
            let h = if f % major == 0 { 8.0 } else { 3.5 };
            p.line_segment([pos2(x, bar.max.y - h), pos2(x, bar.max.y)], Stroke::new(1.0, t.text_faint));
        }
        f = f.saturating_add(minor);
    }
    let a_t = interaction.range.map(|r| r.start).unwrap_or(mark_in.unwrap_or(origin));
    // Out includes its frame; both the shading and its bracket end at the following boundary.
    let b_t = interaction.range.map(|r| r.end()).unwrap_or_else(|| mark_out.map(|o| Tick(o.0.saturating_add(rate.frame_duration().0))).unwrap_or(duration));
    let a = xof(a_t);
    let b = xof(b_t).max(a);
    if mark_in.is_some() || mark_out.is_some() {
        p.rect_filled(Rect::from_min_max(pos2(a, bar.min.y + 8.0), pos2(b, bar.max.y)), 0.0, Color32::from_rgba_unmultiplied(0x5c, 0x5c, 0x5c, 128));
    }
    if mark_in.is_some() || mark_out.is_some() {
        // Curly braces sit inside the gray band, with their waist at its exact range edge.
        let top = bar.min.y + 8.0;
        let bottom = bar.max.y;
        let mid = (top + bottom) * 0.5;
        for (x, direction, hot, edge) in [(a, 1.0_f32, interaction.in_hot, a_t), (b, -1.0_f32, interaction.out_hot, b_t)] {
            if hot || !shows(edge) || (direction > 0.0 && mark_in.is_none()) || (direction < 0.0 && mark_out.is_none()) {
                continue;
            }
            let point = |offset: f32, y: f32| pos2(x + direction * offset, y);
            p.line(
                vec![
                    point(4.0, top),
                    point(2.8, top),
                    point(2.0, top + 1.0),
                    point(2.0, mid - 2.0),
                    point(1.4, mid - 1.0),
                    point(0.0, mid),
                    point(1.4, mid + 1.0),
                    point(2.0, mid + 2.0),
                    point(2.0, bottom - 1.0),
                    point(2.8, bottom),
                    point(4.0, bottom),
                ],
                Stroke::new(1.0, t.playhead),
            );
        }
    }
    // markers: the sequence's (Program), the clip's or the subclip's inherited ones (Source)
    let markers = match which {
        Which::Program => app.session.active_sequence().map(|q| q.markers.clone()),
        Which::Source => app.session.state.source_item.and_then(|i| filmcraft_engine::clip_ops::source_view(&app.session, i)).map(|v| v.markers),
    };
    if let Some(markers) = markers {
        for m in markers.iter().filter(|m| shows(m.start)) {
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
    if shows(time) {
        let x = xof(time);
        // playhead: blue triangle over a line
        let hy = bar.max.y - 11.0;
        p.add(egui::Shape::convex_polygon(
            vec![pos2(x - 5.5, hy), pos2(x + 5.5, hy), pos2(x + 5.5, hy + 5.0), pos2(x, hy + 9.0), pos2(x - 5.5, hy + 5.0)],
            t.playhead,
            Stroke::NONE,
        ));
        p.line_segment([pos2(x, hy + 8.0), pos2(x, bar.max.y)], Stroke::new(1.0, t.playhead));
    }
    if interaction.in_hot {
        crate::panels::source_range::trim_cue(p, a, bar, 4.0);
    }
    if interaction.out_hot {
        crate::panels::source_range::trim_cue(p, b, bar, -4.0);
    }
    let scrub = interaction.scrub.or_else(|| {
        if !interaction.handled && (resp.dragged() || resp.clicked()) {
            resp.interact_pointer_pos().map(|pos| {
                let f = ((pos.x - bar.min.x) / bar.width().max(1.0)).clamp(0.0, 1.0) as f64;
                rate.snap(shown.0 + Tick((f * dur) as i64))
            })
        } else {
            None
        }
    });
    if let Some(tk) = scrub {
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

// A hidden transport action is still available through its regular shortcut/menu.
fn transport_visible(hidden: &[String], command: &str) -> bool {
    !hidden.iter().any(|id| id == command)
}

fn set_transport_visible(hidden: &mut Vec<String>, command: &str, visible: bool) {
    if visible {
        hidden.retain(|id| id != command);
    } else if transport_visible(hidden, command) {
        hidden.push(command.to_owned());
    }
}

/// Width and height of a transport button.
const TRANSPORT_BUTTON: f32 = 28.0;

fn transport(app: &mut FilmcraftApp, ui: &mut egui::Ui, row: Rect, which: Which) {
    let t = app.tokens;
    let src = which == Which::Source;
    let buttons: Vec<(Icon, &str, &str)> = if src {
        vec![
            (Icon::Marker, "markers.add", tl!("Add Marker (M)")),
            (Icon::MarkIn, "src.markIn", tl!("Mark In (I)")),
            (Icon::MarkOut, "src.markOut", tl!("Mark Out (O)")),
            (Icon::GoToIn, "src.goIn", tl!("Go to In (Shift+I)")),
            (Icon::StepBack, "src.stepBack", tl!("Step Back 1 Frame (Left)")),
            (if app.source_playback.clock.playing { Icon::Pause } else { Icon::Play }, "source.playback.toggle", tl!("Play-Stop Toggle (Space)")),
            (Icon::StepFwd, "src.stepFwd", tl!("Step Forward 1 Frame (Right)")),
            (Icon::GoToOut, "src.goOut", tl!("Go to Out (Shift+O)")),
            (Icon::Insert, "source.insert", tl!("Insert (,)")),
            (Icon::Overwrite, "source.overwrite", tl!("Overwrite (.)")),
            (Icon::Camera, "exportFrame", tl!("Export Frame")),
            (Icon::Proxy, "media.toggleProxies", tl!("Toggle Proxies")),
        ]
    } else {
        vec![
            (Icon::Marker, "markers.add", tl!("Add Marker (M)")),
            (Icon::MarkIn, "markers.markIn", tl!("Mark In (I)")),
            (Icon::MarkOut, "markers.markOut", tl!("Mark Out (O)")),
            (Icon::GoToIn, "markers.goToIn", tl!("Go to In (Shift+I)")),
            (Icon::StepBack, "playhead.stepBack", tl!("Step Back 1 Frame (Left)")),
            (if app.playback.playing { Icon::Pause } else { Icon::Play }, "playback.toggle", tl!("Play-Stop Toggle (Space)")),
            (Icon::StepFwd, "playhead.stepForward", tl!("Step Forward 1 Frame (Right)")),
            (Icon::GoToOut, "markers.goToOut", tl!("Go to Out (Shift+O)")),
            (Icon::Lift, "sequence.lift", tl!("Lift (;)")),
            (Icon::Extract, "sequence.extract", tl!("Extract (')")),
            (Icon::Camera, "exportFrame", tl!("Export Frame")),
            (Icon::Proxy, "media.toggleProxies", tl!("Toggle Proxies")),
        ]
    };
    let mut buttons = buttons;
    if !src {
        buttons.push((Icon::Loop, "playback.loop", tl!("Loop")));
    }
    let hidden = monitor_view::view(app, which).transport_hidden.clone();
    let all_buttons = buttons.clone();
    buttons.retain(|(_, command, _)| transport_visible(&hidden, command));
    let prefix = if src { "source" } else { "program" };
    if row.width() >= 30.0 {
        // Keep the existing button-editor affordance only when it has its own space.
        // Customize this monitor's transport separately from the other monitor.
        let r = Rect::from_min_size(pos2(row.max.x - 30.0, row.min.y + 5.0), vec2(22.0, 22.0));
        let resp = ui.interact(r, egui::Id::new((prefix, "btn-editor")), Sense::click()).on_hover_text(tl!("Button Editor"));
        app.auto.add(&format!("{prefix}.transport.editor"), r, "Button Editor");
        icons::paint(ui.painter(), r.shrink(5.0), Icon::Plus, if resp.hovered() { t.tab_text_active } else { t.text_dim });
        let mut edited_hidden = hidden.clone();
        let editor_width = (ui.ctx().content_rect().width() - 10.0).clamp(1.0, 225.0);
        let editor_height = (ui.ctx().content_rect().height() - 16.0).max(1.0);
        egui::Popup::menu(&resp).width(editor_width).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
            ui.set_width(editor_width);
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
            let scroll = egui::ScrollArea::vertical().id_salt((prefix, "editor-scroll")).max_height(editor_height).show(ui, |ui| {
                for &(_, cmd, tip) in &all_buttons {
                    let mut visible = transport_visible(&edited_hidden, cmd);
                    let checkbox = ui.checkbox(&mut visible, tip);
                    app.auto.add(&format!("{prefix}.transport.editor.{cmd}"), checkbox.rect, tip);
                    if checkbox.changed() {
                        set_transport_visible(&mut edited_hidden, cmd, visible);
                    }
                }
                ui.separator();
                let reset = ui.button(tl!("Reset"));
                app.auto.add(&format!("{prefix}.transport.editor.reset"), reset.rect, "Reset");
                if reset.clicked() {
                    edited_hidden = crate::state::MonitorView::default().transport_hidden;
                }
            });
            app.auto.add(&format!("{prefix}.transport.editor.scroll"), scroll.inner_rect, "Button editor");
        });
        if edited_hidden != hidden {
            monitor_view::view_mut(app, which).transport_hidden = edited_hidden;
        }
    }
    let available = (row.width() - 46.0).max(0.0);
    let widths = vec![TRANSPORT_BUTTON; buttons.len()];
    // roomy spacing when every button fits with it, tight spacing before any goes to the overflow menu
    let count = buttons.len() as f32;
    let gap = if count * TRANSPORT_BUTTON + (count - 1.0).max(0.0) * 6.0 <= available { 6.0 } else { 2.0 };
    let Some(fit) = craft_ui::layout::fit_toolbar(&widths, available, gap, TRANSPORT_BUTTON) else {
        ui.scope_builder(egui::UiBuilder::new().max_rect(Rect::from_min_max(row.min, pos2((row.right() - 30.0).max(row.left()), row.bottom()))), |ui| {
            egui::ScrollArea::horizontal().id_salt((prefix, "transport-scroll")).show(ui, |ui| {
                ui.horizontal(|ui| {
                    for (icon, command, label) in &buttons {
                        let (rect, _) = ui.allocate_exact_size(vec2(TRANSPORT_BUTTON, TRANSPORT_BUTTON), Sense::hover());
                        if transport_button(app, ui, rect, prefix, *icon, command, label) {
                            invoke_transport(app, ui.ctx(), which, command);
                        }
                    }
                });
            });
        });
        return;
    };
    let pitch = TRANSPORT_BUTTON + gap;
    let total = fit.shown.len() as f32 * pitch - gap + if fit.overflow.is_empty() { 0.0 } else { pitch };
    let mut x = row.center().x - 15.0 - total.max(TRANSPORT_BUTTON) / 2.0;
    for (index, _) in &fit.shown {
        let Some((icon, command, label)) = buttons.get(*index) else { continue };
        let rect = Rect::from_min_size(pos2(x, row.min.y + 2.0), vec2(TRANSPORT_BUTTON, TRANSPORT_BUTTON));
        if transport_button(app, ui, rect, prefix, *icon, command, label) {
            invoke_transport(app, ui.ctx(), which, command);
        }
        x += pitch;
    }
    if !fit.overflow.is_empty() {
        let rect = Rect::from_min_size(pos2(x, row.min.y + 2.0), vec2(TRANSPORT_BUTTON, TRANSPORT_BUTTON));
        let response = ui.interact(rect, egui::Id::new((prefix, "transport-more")), Sense::click()).on_hover_text(tl!("More transport controls"));
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, tl!("More transport controls")));
        if response.hovered() {
            ui.painter().rect_filled(rect, 4.0, t.hover);
        }
        for offset in [-6.0, 0.0, 6.0] {
            ui.painter().circle_filled(rect.center() + vec2(offset, 0.0), 2.0, t.icon);
        }
        if response.has_focus() {
            ui.painter().rect_stroke(rect.shrink(2.0), 2.0, Stroke::new(1.0, t.focus), StrokeKind::Inside);
        }
        app.auto.add_response(&format!("{prefix}.transport.more"), &response, "More transport controls");
        egui::Popup::menu(&response).show(|ui| {
            for index in &fit.overflow {
                let Some((_, command, label)) = buttons.get(*index) else { continue };
                let selected = match *command {
                    "media.toggleProxies" => Some(app.session.media.use_proxies()),
                    "playback.loop" => Some(app.playback.looping),
                    "playback.toggle" => Some(app.playback.playing),
                    "source.playback.toggle" => Some(app.source_playback.clock.playing),
                    _ => None,
                };
                let response = if let Some(selected) = selected { ui.selectable_label(selected, *label) } else { ui.button(*label) };
                app.auto.add_response(&format!("{prefix}.transport.{command}"), &response, label);
                if response.clicked() {
                    invoke_transport(app, ui.ctx(), which, command);
                    ui.close();
                }
            }
        });
    }
}

fn transport_is_lit(command: &str, proxies_enabled: bool, loop_enabled: bool) -> bool {
    match command {
        "media.toggleProxies" => proxies_enabled,
        "playback.loop" => loop_enabled,
        _ => false,
    }
}

fn transport_button(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect, prefix: &str, icon: Icon, command: &str, label: &str) -> bool {
    let t = app.tokens;
    let response = ui.interact(rect, egui::Id::new((prefix, command)), Sense::click()).on_hover_text(label);
    app.auto.add_response(&format!("{prefix}.transport.{command}"), &response, label);
    if response.hovered() {
        ui.painter().rect_filled(rect, 4.0, t.hover);
    }
    let size = if matches!(command, "playback.toggle" | "source.playback.toggle") { 20.0 } else { 18.0 };
    let color = if transport_is_lit(command, app.session.media.use_proxies(), app.playback.looping) {
        t.accent
    } else if response.hovered() {
        t.tab_text_active
    } else {
        t.icon
    };
    icons::paint(ui.painter(), Rect::from_center_size(rect.center(), vec2(size, size)), icon, color);
    response.clicked()
}

fn invoke_transport(app: &mut FilmcraftApp, ctx: &egui::Context, which: Which, command: &str) {
    let source = which == Which::Source;
    let result = match command {
        "src.markIn" => app.session.execute("markers.markIn", json!({"target": "source"})).map_err(|error| error.to_string()),
        "src.markOut" => app.session.execute("markers.markOut", json!({"target": "source"})).map_err(|error| error.to_string()),
        "src.goIn" | "src.goOut" | "src.stepBack" | "src.stepFwd" => {
            source_nav(app, command);
            Ok(serde_json::Value::Null)
        }
        "exportFrame" => crate::menus::invoke(app, ctx, "file.exportFrame", json!({"monitor": if source { "source" } else { "program" }})),
        command => crate::menus::invoke(app, ctx, command, json!({"monitor": if source { "source" } else { "program" }})),
    };
    if let Err(error) = result {
        app.ui.status = error;
    }
}

/// The Source panel menu's clips (#313): the recently opened clips, the shown one ticked, to
/// switch between, then Close / Close All. Returns whether the menu is done.
pub fn source_panel_menu(app: &mut FilmcraftApp, ui: &mut egui::Ui) -> bool {
    let mut done = false;
    let current = app.session.state.source_item;
    let recent: Vec<(u64, String)> =
        app.session.state.source_history.iter().filter_map(|i| app.session.project.item(*i).map(|it| (i.0, it.name.clone()))).collect();
    for (id, name) in &recent {
        let shown = current.is_some_and(|c| c.0 == *id);
        let r = ui.button(if shown { format!("✓ {name}") } else { name.clone() });
        app.auto.add(&format!("panel.menu.Source.recent.{id}"), r.rect, name);
        if r.clicked() {
            if !shown && let Err(e) = app.session.execute("source.open", json!({"item": id})) {
                app.ui.status = e.to_string();
            }
            done = true;
        }
    }
    if !recent.is_empty() {
        ui.separator();
    }
    for (cmd, id, label) in [("source.close", "close", tl!("Close")), ("source.closeAll", "closeAll", tl!("Close All"))] {
        let r = ui.add_enabled(current.is_some(), egui::Button::new(label));
        app.auto.add(&format!("panel.menu.Source.{id}"), r.rect, label);
        if r.clicked() {
            if let Err(e) = app.session.execute(cmd, json!({})) {
                app.ui.status = e.to_string();
            }
            done = true;
        }
    }
    done
}

fn source_nav(app: &mut FilmcraftApp, cmd: &str) {
    let Some(item) = app.session.state.source_item else { return };
    let Some(v) = filmcraft_engine::clip_ops::source_view(&app.session, item) else { return };
    let rate = v.rate;
    let cur = app.session.state.source_playhead;
    let t = match cmd {
        "src.goIn" => v.mark_in.unwrap_or(v.start),
        "src.goOut" => v.mark_out.unwrap_or(v.end - rate.frame_duration()),
        _ if rate.frame_duration() == Tick::ZERO => cur,
        "src.stepBack" => rate.tick_of(rate.frame_at(cur).saturating_sub(1)),
        _ => rate.tick_of(rate.frame_at(cur).saturating_add(1)),
    };
    let _ = app.session.execute("source.setPlayhead", json!({"time": t.0}));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frames::Rgba;

    fn flat(w: usize, h: usize, rgba: [u8; 4]) -> Rgba {
        Rgba { w, h, px: rgba.iter().copied().cycle().take(w * h * 4).collect() }
    }

    #[test]
    fn button_editor_updates_visibility_without_affecting_other_monitor() {
        let mut program = crate::state::MonitorView::default().transport_hidden;
        let source = program.clone();
        assert!(!transport_visible(&program, "playback.loop"));
        set_transport_visible(&mut program, "playback.loop", true);
        set_transport_visible(&mut program, "playback.loop", true);
        assert!(transport_visible(&program, "playback.loop"));
        assert!(program.is_empty(), "enabling twice must not leave hidden entries");
        set_transport_visible(&mut program, "playhead.stepForward", false);
        set_transport_visible(&mut program, "playhead.stepForward", false);
        assert_eq!(program, vec!["playhead.stepForward"]);
        assert!(transport_visible(&source, "playhead.stepForward"));
    }

    #[test]
    fn loop_transport_is_lit_only_while_looping() {
        assert!(!transport_is_lit("playback.loop", false, false));
        assert!(transport_is_lit("playback.loop", false, true));

        assert!(!transport_is_lit("media.toggleProxies", false, true));
        assert!(transport_is_lit("media.toggleProxies", true, false));

        assert!(!transport_is_lit("playback.toggle", true, true));
    }

    #[test]
    fn eyedropper_averages_the_pixels_around_the_point() {
        let mut img = flat(8, 8, [0, 200, 0, 255]);
        // one noisy pixel next to the centre is averaged away, not picked
        img.px[(4 * 8 + 5) * 4 + 1] = 0;
        assert_eq!(sample_rgb(&img, (0.5, 0.5)), Some([0, 192, 0]));
    }

    #[test]
    fn eyedropper_clamps_to_the_picture_edges() {
        let img = flat(4, 3, [10, 20, 30, 255]);
        for uv in [(0.0, 0.0), (1.0, 1.0), (0.999, 0.0), (0.0, 0.999)] {
            assert_eq!(sample_rgb(&img, uv), Some([10, 20, 30]));
        }
    }

    #[test]
    fn eyedropper_rejects_empty_and_truncated_frames() {
        assert_eq!(sample_rgb(&Rgba { w: 0, h: 0, px: vec![] }, (0.5, 0.5)), None);
        assert_eq!(sample_rgb(&Rgba { w: 4, h: 4, px: vec![0; 10] }, (0.5, 0.5)), None);
    }
}
