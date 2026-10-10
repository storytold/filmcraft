//! Effect Controls: the selected clip's effects (fixed Motion/Opacity/Time Remapping first, as in
//! Premiere, then standard effects), generated from parameter schemas, with stopwatches and a
//! keyframe lane on the right. Also hosts the Lumetri Color panel body (same editor, grouped).

use egui::{Align2, Color32, Pos2, Rect, Sense, Stroke, pos2, vec2};
use filmcraft_project::{ClipId, EffectInstance, ParamKind, ParamValue, TrackItem, TrackKind};
use filmcraft_time::Tick;
use serde_json::{Value, json};

use crate::FilmcraftApp;
use crate::icons::{self, Icon};
use crate::state::KeyframeRef;
use crate::theme::Tokens;

const ROW_H: f32 = 22.0;
/// The narrowest the effect list and the keyframe area can be dragged (#643): the list's rows are
/// laid out for at least 260 points (values from 150 points in, the reset button at the end).
const MIN_LIST_W: f32 = 260.0;
const MIN_LANE_W: f32 = 60.0;
/// Keyframe lane header: the time ruler (beside the Source / Sequence pills) and the clip's bar.
const RULER_H: f32 = 24.0;
const CLIP_BAR_H: f32 = 18.0;
/// Distance between the keyframe navigator's buttons (◀ ◆ ▶).
const NAV_STEP: f32 = 16.0;
/// The Y curve of a point parameter's value graph (X uses the accent colour).
const Y_CURVE: Color32 = Color32::from_rgb(0x60, 0xc0, 0x80);
/// Properties panel: the keyframe diamonds' column, from a row's right edge (the arrows sit one
/// [`NAV_STEP`] to either side of it, and a section's reset button above it).
const PROPS_NAV_X: f32 = 26.0;
/// Properties panel: where a row's value fields end, left of the navigator.
const PROPS_VALUE_R: f32 = PROPS_NAV_X + NAV_STEP + 12.0;

/// Timeline time at which the clip shows media time `m` (where a keyframe sits in the sequence).
fn timeline_time_of(it: &TrackItem, m: Tick) -> Tick {
    it.start + Tick(((m - it.source_in).0 as f64 / it.speed.abs().max(1e-6)) as i64)
}

// The press frame can include movement before the button went down. Start from the press
// origin so that only gesture motion enters the preview, including same-frame movement.
fn graph_drag_offset(ui: &egui::Ui, response: &egui::Response, previous: egui::Vec2) -> egui::Vec2 {
    if response.drag_started() {
        response
            .interact_pointer_pos()
            .zip(ui.input(|i| i.pointer.press_origin()))
            .map_or_else(|| response.drag_delta(), |(position, origin)| position - origin)
    } else {
        previous + response.drag_delta()
    }
}

/// Whether `k` names a keyframe of the clip `it` (its effect, parameter and media time).
fn keyframe_exists(it: &TrackItem, k: &KeyframeRef) -> bool {
    let Some(e) = it.effects.get(k.effect) else { return false };
    let param = match k.mask {
        Some(m) => e.masks.get(m).and_then(|m| m.param(&k.param)),
        None => e.params.get(&k.param),
    };
    param.is_some_and(|p| p.keyframes.iter().any(|kf| kf.time == k.time))
}

fn selected_clip(app: &FilmcraftApp) -> Option<(ClipId, TrackItem, TrackKind)> {
    selected_clips(app).into_iter().next()
}

/// The selected clip (video first) and, for a video clip, the selected audio linked to it.
fn selected_clips(app: &FilmcraftApp) -> Vec<(ClipId, TrackItem, TrackKind)> {
    let Some(seq) = app.session.active_sequence() else { return Vec::new() };
    let picked: Vec<(ClipId, &TrackItem, TrackKind)> = app
        .session
        .state
        .selection
        .iter()
        .filter_map(|c| {
            let (tid, it) = seq.find_item(*c)?;
            Some((*c, it, seq.track(tid)?.kind))
        })
        .collect();
    let Some(&(clip, it, kind)) = picked.iter().find(|p| p.2 == TrackKind::Video).or(picked.first()) else { return Vec::new() };
    let mut out = vec![(clip, it.clone(), kind)];
    if kind == TrackKind::Video
        && let Some(&(ac, ai, ak)) = picked.iter().find(|p| p.2 == TrackKind::Audio && p.1.link.is_some() && p.1.link == it.link)
    {
        out.push((ac, ai.clone(), ak));
    }
    out
}

pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let clips = selected_clips(app);
    // keep only selected keyframes that still exist on a clip shown here
    app.ui.keyframe_selection.retain(|k| clips.iter().any(|(c, it, _)| c.0 == k.clip && keyframe_exists(it, k)));
    let Some((clip, it, _)) = clips.first().cloned() else {
        // a transition clicked in the Timeline (#430)
        if let Some(id) = crate::panels::transition_controls::selected(app) {
            crate::panels::transition_controls::effect_controls(app, ui, rect, id);
            return;
        }
        crate::dock::placeholder(ui, rect, &t, tl!("(no clip selected)"));
        return;
    };
    let Some(seq) = app.session.active_sequence().cloned() else {
        crate::dock::placeholder(ui, rect, &t, tl!("(no sequences)"));
        return;
    };
    // the divider between the effect list and the keyframe area; dragging it sets the list's width
    // (#643), each side keeping a minimum width
    // (the keyframe area has 4 points of margin left of it and 6 right)
    let max_list = (rect.width() - MIN_LANE_W - 10.0).max(MIN_LIST_W);
    let list_w = if app.ui.effect_controls_split > 0.0 { app.ui.effect_controls_split } else { (rect.width() * 0.58).max(260.0) };
    let list_w = list_w.clamp(MIN_LIST_W, max_list);
    let split = rect.min.x + list_w;
    let head = Rect::from_min_size(rect.min + vec2(8.0, 4.0), vec2(split - rect.min.x - 12.0, 24.0));
    // Premiere: two pill tabs — "Source · clip" and "Sequence · clip" (active)
    let pill = |ui: &mut egui::Ui, r: Rect, text: &str, active: bool| {
        ui.painter().rect_filled(r, 4.0, if active { t.pill_active_bg } else { t.panel_bg });
        if !active {
            ui.painter().rect_stroke(r, 4.0, Stroke::new(1.0, t.separator), egui::StrokeKind::Inside);
        }
        let cp = ui.painter().with_clip_rect(r.shrink(2.0));
        cp.text(pos2(r.min.x + 8.0, r.center().y), Align2::LEFT_CENTER, text, Tokens::ui(11.5), if active { t.text } else { t.text_dim });
    };
    let half = (head.width() - 6.0) / 2.0;
    pill(ui, Rect::from_min_size(head.min, vec2(half, 24.0)), &tlf!("Source · {name}", name = it.name), false);
    pill(ui, Rect::from_min_size(head.min + vec2(half + 6.0, 0.0), vec2(half, 24.0)), &format!("{} · {}", seq_name(app), it.name), true);
    // keyframe lane header, as Premiere's: a time ruler over the clip's stretch of the sequence,
    // which the playhead's handle rides on, and the clip's bar under it
    let lane = Rect::from_min_max(pos2(split + 4.0, rect.min.y + 4.0), pos2(rect.max.x - 6.0, rect.max.y - 26.0));
    ui.painter().rect_filled(lane, 0.0, t.tl_bg);
    let ph = app.session.playhead();
    let dur = it.duration.0.max(1) as f64;
    let lx = |tk: Tick| -> f32 { lane.min.x + (((tk - it.start).0 as f64 / dur) as f32).clamp(0.0, 1.0) * lane.width() };
    let rate = seq.settings.frame_rate;
    let ruler = Rect::from_min_max(lane.min, pos2(lane.max.x, lane.min.y + RULER_H));
    let bar = Rect::from_min_max(pos2(lane.min.x, ruler.max.y + 4.0), pos2(lane.max.x, ruler.max.y + 4.0 + CLIP_BAR_H));
    let scrub = Rect::from_min_max(ruler.min, bar.max);
    app.auto.add("effectControls.lane", lane, "keyframe lane");
    app.auto.add("effectControls.ruler", scrub, "time ruler");
    let body = Rect::from_min_max(pos2(rect.min.x, head.max.y + 4.0), pos2(split, rect.max.y - 26.0));
    let mut actions: Vec<(String, Value)> = Vec::new();
    // the rows span the list and the keyframe area: the wheel scrolls anywhere over them and the
    // scrollbar sits at the panel's right edge (#643)
    let mut bui = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_max(body.min, pos2(lane.max.x, body.max.y))).id_salt("ec-body"));
    bui.set_clip_rect(Rect::from_min_max(body.min, pos2(rect.max.x, body.max.y)));
    let scroll_out = egui::ScrollArea::vertical().id_salt("ec-scroll").auto_shrink([false, false]).show(&mut bui, |bui| {
        for (clip, it, kind) in &clips {
            let clip = *clip;
            let (hr, _) = bui.allocate_exact_size(vec2(body.width(), 18.0), Sense::hover());
            let heading = if *kind == TrackKind::Video { tl!("Video") } else { tl!("Audio") };
            bui.painter().text(pos2(hr.min.x + 8.0, hr.center().y), Align2::LEFT_CENTER, heading, Tokens::semibold(11.5), t.text_dim);
            let mt_now = it.source_time_at(ph.clamp(it.start, (it.end() - Tick(1)).max(it.start)));
            // Premiere lists the fixed effects (Motion, Opacity, Time Remapping / Volume…) first.
            let mut order: Vec<usize> = (0..it.effects.len()).collect();
            order.sort_by_key(|i| !it.effects[*i].def().is_some_and(|d| d.intrinsic));
            for idx in order {
                let e = &it.effects[idx];
                let Some(def) = e.def() else { continue };
                let key = format!("{}:{}", clip.0, idx);
                let open = !app.ui.collapsed_fx.contains(&key);
                let selected = app.session.state.selected_effect.as_ref().is_some_and(|s| s.clip == clip && s.index(&it.effects) == Some(idx));
                let (r, resp) = bui.allocate_exact_size(vec2(body.width(), ROW_H), Sense::click());
                if selected {
                    bui.painter().rect_filled(r, 0.0, t.row_selected);
                } else if resp.hovered() {
                    bui.painter().rect_filled(r, 0.0, t.hover);
                }
                row_line(bui, r, &lane, &t);
                // only the arrow twirls the effect open or shut; the rest of the header selects it
                let tw = Rect::from_center_size(pos2(r.min.x + 10.0, r.center().y), vec2(10.0, 10.0));
                icons::paint(bui.painter(), tw, if open { Icon::ChevronDown } else { Icon::ChevronRight }, t.text_dim);
                let twresp = bui.interact(tw.expand(3.0), egui::Id::new(("fx-twirl", clip.0, idx)), Sense::click());
                app.auto.add(&format!("effectControls.effect.{}.twirl", e.effect), tw, "twirl");
                // fx enable toggle
                let fxr = Rect::from_center_size(pos2(r.min.x + 28.0, r.center().y), vec2(18.0, 14.0));
                let fxresp = bui.interact(fxr, egui::Id::new(("fxen", clip.0, idx)), Sense::click());
                bui.painter().text(fxr.center(), Align2::CENTER_CENTER, "fx", Tokens::semibold(10.5), if e.enabled { t.text } else { t.text_faint });
                if !e.enabled {
                    bui.painter().line_segment([fxr.left_bottom(), fxr.right_top()], Stroke::new(1.0, t.text_faint));
                }
                if fxresp.clicked() {
                    actions.push(("effects.toggleEnabled".into(), json!({"clip": clip.0, "index": idx})));
                }
                // (the reset button sits at the row's right end)
                row_label(bui, pos2(r.min.x + 42.0, r.center().y), crate::i18n::t(def.name), r.max.x - 26.0 - (r.min.x + 42.0), Tokens::ui(12.0), t.text);
                // reset button
                let rr = Rect::from_center_size(pos2(r.max.x - 14.0, r.center().y), vec2(16.0, 16.0));
                let rresp = bui.interact(rr, egui::Id::new(("fxreset", clip.0, idx)), Sense::click()).on_hover_text(tl!("Reset Effect"));
                icons::paint(bui.painter(), rr.shrink(2.0), Icon::Reset, if rresp.hovered() { t.text } else { t.text_dim });
                if rresp.clicked() {
                    actions.push(("effects.reset".into(), json!({"clip": clip.0, "index": idx})));
                }
                app.auto.add(&format!("effectControls.effect.{}", e.effect), r, def.name);
                if twresp.clicked() {
                    if open {
                        app.ui.collapsed_fx.push(key.clone());
                    } else {
                        app.ui.collapsed_fx.retain(|k| *k != key);
                    }
                } else if resp.clicked() && !fxresp.clicked() && !rresp.clicked() {
                    actions.push(("effects.select".into(), json!({"clip": clip.0, "effect": idx})));
                }
                let mut save_preset = false;
                let fx_id = e.effect.clone();
                resp.context_menu(|ui| {
                    let sp = ui.button(tl!("Save Preset…"));
                    app.auto.add(&format!("effectControls.effect.{fx_id}.savePreset"), sp.rect, "Save Preset…");
                    if sp.clicked() {
                        save_preset = true;
                        ui.close();
                    }
                    if !def.intrinsic && ui.button(tl!("Clear")).clicked() {
                        actions.push(("effects.remove".into(), json!({"clip": clip.0, "index": idx})));
                        ui.close();
                    }
                });
                if save_preset {
                    crate::panels::presets::open_save(app, clip.0, vec![idx], def.name);
                }
                if !open {
                    continue;
                }
                if crate::panels::audio_fx_editor::has_editor(&e.effect) {
                    custom_setup_row(app, bui, body, &lane, clip, idx, &e.effect);
                }
                for pd in &def.params {
                    param_row(app, bui, body, clip, idx, e, None, pd, mt_now, &mut actions, &lane, &lx, it);
                    if app.ui.expanded_fx.contains(&graph_key(clip, idx, pd.id))
                        && let Some(param) = e.params.get(pd.id)
                        && param.is_animated()
                        && graphable(&param.value)
                    {
                        graph_rows(app, bui, body, clip, idx, None, pd, param, &lane, it, &mut actions);
                    }
                }
                if crate::panels::masks::maskable(e) {
                    crate::panels::masks::effect_rows(app, bui, body, clip, idx, e, mt_now, &mut actions, &lane, &lx, it);
                }
            }
        }
    });
    let _ = scroll_out;
    // the ruler and the clip's bar, painted after the rows so rows scrolled up under them (their
    // lines and keyframes) stay hidden
    ui.painter().rect_filled(Rect::from_min_max(lane.min, pos2(lane.max.x, bar.max.y)), 0.0, t.tl_bg);
    paint_ruler(ui.painter(), ruler, &it, rate, seq.settings.drop_frame, &t);
    ui.painter().rect_filled(bar.shrink2(vec2(0.0, 2.0)), 2.0, t.clip_bar_bg);
    ui.painter().with_clip_rect(bar).text(pos2(bar.min.x + 4.0, bar.center().y), Align2::LEFT_CENTER, &it.name, Tokens::ui(10.0), t.text);
    // click or drag anywhere on the ruler (or the bar) to move the playhead (after the rows, which
    // reach under it, so it gets the pointer first)
    let sresp = ui.interact(scrub, egui::Id::new(("ec-scrub", clip.0)), Sense::click_and_drag());
    if (sresp.dragged() || sresp.clicked())
        && let Some(pos) = sresp.interact_pointer_pos()
    {
        let f = ((pos.x - lane.min.x) / lane.width()).clamp(0.0, 1.0) as f64;
        let tk = rate.snap_nearest(it.start + Tick((f * it.duration.0 as f64) as i64));
        app.stop();
        app.session.set_playhead(tk);
    }
    // the divider: a line the height of the panel, a resize cursor and a wider grab area
    let divider = Rect::from_min_max(pos2(split - 2.0, rect.min.y), pos2(split + 4.0, rect.max.y - 26.0));
    let dresp = ui.interact(divider, egui::Id::new("ec-divider"), Sense::drag()).on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
    if dresp.dragged() {
        app.ui.effect_controls_split = (list_w + dresp.drag_delta().x).clamp(MIN_LIST_W, max_list);
    }
    let dcol = if dresp.hovered() || dresp.dragged() { t.accent } else { t.separator };
    ui.painter().line_segment([pos2(split + 1.5, rect.min.y), pos2(split + 1.5, rect.max.y - 26.0)], Stroke::new(1.0, dcol));
    app.auto.add("effectControls.divider", divider, "divider");
    // playhead: a handle on the ruler and a line down the lane, while it is on the clip
    if ph >= it.start && ph <= it.end() {
        let px = lx(ph);
        let (top, tip) = (ruler.max.y - 13.0, ruler.max.y);
        let head = vec![pos2(px - 5.0, top), pos2(px + 5.0, top), pos2(px + 5.0, tip - 5.0), pos2(px, tip), pos2(px - 5.0, tip - 5.0)];
        ui.painter().add(egui::Shape::convex_polygon(head, t.playhead, Stroke::NONE));
        ui.painter().line_segment([pos2(px, tip), pos2(px, lane.max.y)], Stroke::new(1.0, t.playhead));
        app.auto.add("effectControls.playhead", Rect::from_min_max(pos2(px - 5.0, top), pos2(px + 5.0, tip)), "playhead");
    }
    // footer timecode
    let tc = filmcraft_time::format_time(ph, seq.settings.frame_rate, seq.settings.drop_frame, filmcraft_time::TimeDisplay::Timecode, 48000);
    ui.painter().text(pos2(rect.min.x + 10.0, rect.max.y - 13.0), Align2::LEFT_CENTER, tc, Tokens::mono(13.0), t.timecode);
    run(app, ui.ctx(), actions);
}

/// The thin line under a row, across the names, the values and the keyframe lane, so a row can be
/// followed across the panel, as in Premiere (#640). It runs through the middle of the spacing
/// between rows, so each row sits centred between its two lines.
pub(crate) fn row_line(ui: &egui::Ui, r: Rect, lane: &Rect, t: &Tokens) {
    let y = r.max.y + ui.spacing().item_spacing.y / 2.0;
    ui.painter().line_segment([pos2(r.min.x, y), pos2(lane.max.x, y)], Stroke::new(1.0, t.separator));
}

/// A row's label, cut off with "…" where it would run into what follows it, as Premiere does when
/// the effect list is narrow (#643).
pub(crate) fn row_label(ui: &egui::Ui, left_center: Pos2, text: &str, max_w: f32, font: egui::FontId, color: Color32) {
    let mut job = egui::text::LayoutJob::single_section(text.to_owned(), egui::TextFormat::simple(font, color));
    job.wrap.max_width = max_w.max(0.0);
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    job.wrap.overflow_character = Some('…');
    let g = ui.fonts_mut(|f| f.layout_job(job));
    ui.painter().galley(pos2(left_center.x, left_center.y - g.size().y / 2.0), g, color);
}

/// Make a dropdown or a checkbox in a row fit inside the row, centred between its row lines.
pub(crate) fn fit_to_row(ui: &mut egui::Ui) {
    ui.spacing_mut().interact_size.y = ROW_H - 4.0;
    ui.spacing_mut().button_padding.y = 1.0;
}

/// Premiere's "Custom Setup ▸ Edit…" row: opens the effect's Clip Fx Editor window.
fn custom_setup_row(app: &mut FilmcraftApp, ui: &mut egui::Ui, body: Rect, lane: &Rect, clip: ClipId, idx: usize, effect: &str) {
    let t = app.tokens;
    let (r, _) = ui.allocate_exact_size(vec2(body.width(), ROW_H), Sense::hover());
    row_line(ui, r, lane, &t);
    ui.painter().text(pos2(r.min.x + 42.0, r.center().y), Align2::LEFT_CENTER, tl!("Custom Setup"), Tokens::ui(12.0), t.text_dim);
    let br = Rect::from_min_size(pos2(r.min.x + 160.0, r.min.y + 2.0), vec2(60.0, ROW_H - 4.0));
    let resp = ui.interact(br, egui::Id::new(("fx-custom-setup", clip.0, idx)), Sense::click());
    ui.painter().rect_filled(br, 3.0, if resp.hovered() { t.hover } else { t.field_bg });
    ui.painter().rect_stroke(br, 3.0, Stroke::new(1.0, t.field_border), egui::StrokeKind::Inside);
    ui.painter().text(br.center(), Align2::CENTER_CENTER, tl!("Edit…"), Tokens::ui(11.5), t.text);
    app.auto.add(&format!("effectControls.effect.{effect}.edit"), br, "Edit…");
    if resp.clicked() {
        crate::panels::audio_fx_editor::open(app, crate::panels::audio_fx_editor::FxTarget::Clip { clip: clip.0, index: idx });
    }
}

fn seq_name(app: &FilmcraftApp) -> String {
    app.session.state.active_sequence.and_then(|s| app.session.project.item(s)).map(|i| i.name.clone()).unwrap_or_default()
}

/// The keyframe lane's time ruler: ticks and sequence timecode across the clip's stretch of the
/// timeline (the lane shows exactly the clip, so the ruler starts at the clip's start).
fn paint_ruler(p: &egui::Painter, ruler: Rect, it: &TrackItem, rate: filmcraft_time::FrameRate, drop_frame: bool, t: &Tokens) {
    let p = p.with_clip_rect(ruler);
    let dur = it.duration.0.max(1) as f64;
    let frame_px = ruler.width() as f64 * rate.frame_duration().0.max(1) as f64 / dur;
    let base = rate.timecode_base();
    let steps = [
        1,
        2,
        5,
        10,
        base / 2,
        base,
        base * 2,
        base * 5,
        base * 10,
        base * 15,
        base * 30,
        base * 60,
        base * 120,
        base * 300,
        base * 600,
        base * 1800,
        base * 3600,
    ];
    // labels far enough apart to read; the small ticks divide the labelled ones evenly
    let label_step = steps.iter().copied().find(|s| *s > 0 && *s as f64 * frame_px >= 80.0);
    let minor = steps.iter().copied().find(|s| *s > 0 && *s as f64 * frame_px >= 8.0 && label_step.is_none_or(|l| l % s == 0));
    let (f0, f1) = (rate.frame_at(it.start), rate.frame_at(it.end()));
    let base_y = ruler.max.y - 1.0;
    for (step, h, labelled) in [(minor, 3.0, false), (label_step, 7.0, true)] {
        let Some(step) = step else { continue };
        let mut f = f0.div_euclid(step).saturating_mul(step);
        // a step is at least 8 px wide, so this covers any ruler; the cap is for damaged numbers
        for _ in 0..4096 {
            if f > f1 {
                break;
            }
            let x = ruler.min.x + ((rate.tick_of(f) - it.start).0 as f64 / dur) as f32 * ruler.width();
            p.line_segment([pos2(x, base_y - h), pos2(x, base_y)], Stroke::new(1.0, t.tl_ruler_tick));
            if labelled {
                let label = filmcraft_time::format_time(rate.tick_of(f), rate, drop_frame, filmcraft_time::TimeDisplay::Timecode, 48000);
                p.text(pos2(x, ruler.min.y + 7.0), Align2::CENTER_CENTER, label, Tokens::ui(10.0), t.tl_ruler_text);
            }
            f = f.saturating_add(step);
        }
    }
}

/// Premiere's keyframe navigator, centred on `at`: a diamond that adds a keyframe at the playhead
/// or removes the one there (filled while the playhead is on a keyframe) and, once the parameter
/// is animated, arrows to the previous and the next keyframe. `param` is `None` while the effect
/// is not on the clip yet. Automation ids: `<auto>.addKeyframe`, `.prevKeyframe`, `.nextKeyframe`.
/// Returns whether the diamond was clicked; the arrows queue their own `playhead.set`.
#[allow(clippy::too_many_arguments)]
fn keyframe_nav(
    app: &mut FilmcraftApp,
    ui: &mut egui::Ui,
    at: Pos2,
    param: Option<&filmcraft_project::Param>,
    mt: Tick,
    it: &TrackItem,
    id: egui::Id,
    auto: &str,
    tip: &str,
    actions: &mut Vec<(String, Value)>,
) -> bool {
    let t = app.tokens;
    if let Some(param) = param.filter(|p| p.is_animated()) {
        for (d, target, name, tip) in
            [(-1.0, param.prev_keyframe(mt), "prevKeyframe", "Go to previous keyframe"), (1.0, param.next_keyframe(mt), "nextKeyframe", "Go to next keyframe")]
        {
            // the arrows almost the row's height to click, as Premiere's
            let r = Rect::from_center_size(at + vec2(d * NAV_STEP, 0.0), vec2(14.0, 20.0));
            let resp = ui.interact(r, id.with(name), Sense::click()).on_hover_text(crate::i18n::t(tip));
            let c = r.center();
            let pts = vec![c + vec2(-3.5 * d, -5.5), c + vec2(-3.5 * d, 5.5), c + vec2(3.5 * d, 0.0)];
            ui.painter().add(egui::Shape::convex_polygon(pts, if target.is_some() { t.text } else { t.text_faint }, Stroke::NONE));
            app.auto.add(&format!("{auto}.{name}"), r, tip);
            if resp.clicked()
                && let Some(k) = target
            {
                actions.push(("playhead.set".into(), json!({"time": timeline_time_of(it, k).0})));
            }
        }
    }
    let r = Rect::from_center_size(at, vec2(15.0, 15.0));
    let resp = ui.interact(r.expand(1.5), id.with("key"), Sense::click()).on_hover_text(crate::i18n::t(tip));
    if param.is_some_and(|p| p.keyframes.iter().any(|k| k.time == mt)) {
        icons::paint(ui.painter(), r, Icon::Keyframe, t.hot_text);
    } else {
        let s = 4.7;
        let outline = vec![at + vec2(0.0, -s), at + vec2(s, 0.0), at + vec2(0.0, s), at + vec2(-s, 0.0)];
        ui.painter().add(egui::Shape::closed_line(outline, Stroke::new(1.2, if resp.hovered() { t.text } else { t.text_dim })));
    }
    app.auto.add(&format!("{auto}.addKeyframe"), r, tip);
    resp.clicked()
}

/// A Properties row's keyframe navigator for `effect`.`param`, with Premiere's tooltips. The
/// diamond is `effects.addKeyframe`: it adds a keyframe at the playhead (turning animation on) or
/// removes the one there, so the value falls back to what the remaining keyframes give. An effect
/// the clip does not have yet (Crop) is applied first.
#[allow(clippy::too_many_arguments)]
fn properties_nav(
    app: &mut FilmcraftApp,
    ui: &mut egui::Ui,
    row: Rect,
    clip: ClipId,
    it: &TrackItem,
    mt: Tick,
    effect: &str,
    param: &str,
    actions: &mut Vec<(String, Value)>,
) {
    let prm = it.effect(effect).and_then(|e| e.param(param));
    let tip = match prm {
        Some(p) if p.keyframes.iter().any(|k| k.time == mt) => "Remove keyframe",
        Some(p) if p.is_animated() => "Add keyframe",
        _ => "Turn on animation and add keyframe",
    };
    let at = pos2(row.max.x - PROPS_NAV_X, row.center().y);
    let id = egui::Id::new(("props-nav", effect, param, clip.0));
    if keyframe_nav(app, ui, at, prm, mt, it, id, &format!("properties.{effect}.{param}"), tip, actions) {
        if it.effect(effect).is_none() {
            actions.push(("effects.apply".into(), json!({"clips": [clip.0], "effect": effect})));
        }
        actions.push(("effects.addKeyframe".into(), json!({"clip": clip.0, "effect": effect, "param": param})));
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn param_row(
    app: &mut FilmcraftApp,
    ui: &mut egui::Ui,
    body: Rect,
    clip: ClipId,
    idx: usize,
    e: &EffectInstance,
    mask: Option<usize>,
    pd: &filmcraft_project::ParamDef,
    mt: Tick,
    actions: &mut Vec<(String, Value)>,
    lane: &Rect,
    lx: &dyn Fn(Tick) -> f32,
    it: &TrackItem,
) {
    let _ = lx;
    let t = app.tokens;
    let param = match mask {
        Some(k) => e.masks.get(k).and_then(|m| m.param(pd.id)),
        None => e.params.get(pd.id),
    };
    let Some(param) = param else { return };
    // mask parameters: automation ids / widget ids get a `m<k>.` prefix, commands a `mask` field
    let pkey: String = match mask {
        Some(k) => format!("mask{k}.{}", pd.id),
        None => pd.id.to_string(),
    };
    let pkey = pkey.as_str();
    let with_mask = |mut v: Value| -> Value {
        if let Some(k) = mask {
            v["mask"] = json!(k);
        }
        v
    };
    let (r, row) = ui.allocate_exact_size(vec2(body.width(), ROW_H), if mask.is_none() { Sense::click() } else { Sense::hover() });
    row_line(ui, r, lane, &t);
    // clicking one of an effect's properties selects the effect, as clicking its header does
    if row.clicked() {
        actions.push(("effects.select".into(), json!({"clip": clip.0, "effect": idx})));
    }
    let mut x = r.min.x + 26.0;
    // twirl-down for the value/velocity graphs (animated scalar and point params)
    if param.is_animated() && graphable(&param.value) {
        let key = graph_key(clip, idx, pkey);
        let open = app.ui.expanded_fx.contains(&key);
        let tw = Rect::from_center_size(pos2(r.min.x + 12.0, r.center().y), vec2(12.0, 12.0));
        let tresp = ui.interact(tw.expand(2.0), egui::Id::new(("twirl", clip.0, idx, pkey)), Sense::click()).on_hover_text(tl!("Show graphs"));
        icons::paint(ui.painter(), tw, if open { Icon::ChevronDown } else { Icon::ChevronRight }, t.text_dim);
        app.auto.add(&format!("effectControls.{}.{}.graphs", e.effect, pkey), tw, "Show graphs");
        if tresp.clicked() {
            if open {
                app.ui.expanded_fx.retain(|k| *k != key);
            } else {
                app.ui.expanded_fx.push(key);
            }
        }
    }
    if pd.animatable {
        let sw = Rect::from_center_size(pos2(x, r.center().y), vec2(14.0, 14.0));
        let resp = ui.interact(sw, egui::Id::new(("sw", clip.0, idx, pkey)), Sense::click()).on_hover_text(tl!("Toggle animation"));
        icons::paint(ui.painter(), sw, Icon::Stopwatch, if param.is_animated() { t.accent } else { t.text_dim });
        if resp.clicked() {
            actions.push(("effects.toggleAnimation".into(), with_mask(json!({"clip": clip.0, "effect": idx, "param": pd.id}))));
        }
        app.auto.add(&format!("effectControls.{}.{}.stopwatch", e.effect, pkey), sw, "Toggle animation");
    }
    x += 14.0;
    let vx = r.min.x + (r.width() * 0.5).max(150.0);
    row_label(ui, pos2(x, r.center().y), crate::i18n::t(pd.label), vx - 6.0 - x, Tokens::ui(12.0), t.text);
    let value = param.value_at(mt);
    let mut vui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(Rect::from_min_max(pos2(vx, r.min.y + 1.0), pos2(r.max.x - 26.0, r.max.y - 1.0)))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    let id = egui::Id::new(("pv", clip.0, idx, pkey));
    let mut set: Option<Value> = None;
    match (&pd.kind, &value) {
        (ParamKind::Float { min, max, soft_min, soft_max, unit, decimals }, ParamValue::Float(v)) => {
            let speed = ((soft_max - soft_min) / 400.0).max(0.01);
            let (_, nv) = crate::widgets::hot_number(&mut vui, id, *v, speed, (*min, *max), *decimals as usize, unit, &t);
            if let Some(nv) = nv {
                set = Some(json!(nv));
            }
        }
        (ParamKind::Angle, ParamValue::Float(v)) => {
            let (_, nv) = crate::widgets::hot_number(&mut vui, id, *v, 0.5, (-36000.0, 36000.0), 1, "°", &t);
            if let Some(nv) = nv {
                set = Some(json!(nv));
            }
        }
        (ParamKind::Point, ParamValue::Vec2(p)) => {
            let (_, nx) = crate::widgets::hot_number(&mut vui, id.with("x"), p.x, 1.0, (-100_000.0, 100_000.0), 1, "", &t);
            let (_, ny) = crate::widgets::hot_number(&mut vui, id.with("y"), p.y, 1.0, (-100_000.0, 100_000.0), 1, "", &t);
            if nx.is_some() || ny.is_some() {
                set = Some(json!([nx.unwrap_or(p.x), ny.unwrap_or(p.y)]));
            }
        }
        (ParamKind::Bool, ParamValue::Bool(b)) => {
            let mut v = *b;
            fit_to_row(&mut vui);
            if vui.checkbox(&mut v, "").changed() {
                set = Some(json!(v));
            }
        }
        (ParamKind::Choice(opts), ParamValue::Choice(c)) => {
            let mut sel = *c as usize;
            fit_to_row(&mut vui);
            // (narrower when the effect list is narrow, so it stays left of the divider)
            let w = vui.available_width().min(130.0);
            egui::ComboBox::from_id_salt(id).selected_text(opts.get(sel).map_or("", |o| crate::i18n::t(o))).width(w).show_ui(&mut vui, |ui| {
                for (i, o) in opts.iter().enumerate() {
                    if ui.selectable_value(&mut sel, i, crate::i18n::t(o)).changed() {
                        set = Some(json!(i));
                    }
                }
            });
        }
        (ParamKind::Color, ParamValue::Color(c)) => {
            let mut rgba = egui::Rgba::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]);
            if egui::color_picker::color_edit_button_rgba(&mut vui, &mut rgba, egui::color_picker::Alpha::Opaque).changed() {
                set = Some(json!([rgba.r(), rgba.g(), rgba.b(), rgba.a()]));
            }
            // Eyedropper: arm it, then click a pixel in the Program monitor (Esc cancels)
            let target = crate::state::Eyedropper { clip: clip.0, effect: idx, param: pd.id.to_string(), mask };
            let armed = app.ui.eyedropper.as_ref() == Some(&target);
            let drop = icons::button(&mut vui, Icon::Eyedropper, 18.0, armed, &t, tl!("Eyedropper (pick a color from the Program monitor)"));
            app.auto.add(&format!("effectControls.{}.{}.eyedropper", e.effect, pkey), drop.rect, "Eyedropper");
            if drop.clicked() {
                app.ui.eyedropper = if armed { None } else { Some(target) };
            }
        }
        (ParamKind::Path, _) => {
            crate::panels::masks::path_value(app, &mut vui, clip, idx, mask, actions);
        }
        (ParamKind::Text, ParamValue::Text(s)) if !pd.id.ends_with("_lut") => {
            // edited in a buffer; committed (one undo step) when the field loses focus
            let mut buf = vui.data_mut(|d| d.get_temp::<String>(id)).unwrap_or_else(|| s.clone());
            let r = vui.add(egui::TextEdit::singleline(&mut buf).desired_width(160.0));
            app.auto.add(&format!("effectControls.text.{}.{}", idx, pd.id), r.rect, pd.label);
            if r.lost_focus() {
                if buf != *s {
                    set = Some(json!(buf));
                }
                vui.data_mut(|d| d.remove::<String>(id));
            } else if r.has_focus() {
                vui.data_mut(|d| d.insert_temp(id, buf));
            } else {
                vui.data_mut(|d| d.remove::<String>(id));
            }
        }
        _ => {
            vui.label(param_text(&app.session.project, pd.id, &value));
        }
    }
    if let Some(v) = set {
        actions.push(("effects.setParam".into(), with_mask(json!({"clip": clip.0, "effect": idx, "param": pd.id, "value": v}))));
    }
    // keyframe navigator ◀ ◆ ▶ (when animated)
    let eff_json = json!(idx);
    if pd.animatable && param.is_animated() {
        let at = pos2(r.max.x - 42.0, r.center().y);
        let auto = format!("effectControls.{}.{}", e.effect, pkey);
        if keyframe_nav(app, ui, at, Some(param), mt, it, egui::Id::new(("knav", clip.0, idx, pkey)), &auto, "Add/Remove Keyframe", actions) {
            actions.push(("effects.addKeyframe".into(), with_mask(json!({"clip": clip.0, "effect": eff_json, "param": pd.id}))));
        }
    }
    // reset to the default, under the effect's own reset button (an animated parameter keeps its
    // keyframes and gets one at the playhead)
    if mask.is_none() {
        let rr = Rect::from_center_size(pos2(r.max.x - 14.0, r.center().y), vec2(12.0, 12.0));
        let rresp = ui.interact(rr, egui::Id::new(("param-reset", clip.0, idx, pkey)), Sense::click()).on_hover_text(tl!("Reset Parameter"));
        icons::paint(ui.painter(), rr, Icon::Reset, if rresp.hovered() { t.text } else { t.text_dim });
        app.auto.add(&format!("effectControls.{}.{}.reset", e.effect, pkey), rr, "Reset Parameter");
        if rresp.clicked() {
            actions.push(("effects.resetParam".into(), json!({"clip": clip.0, "effect": eff_json, "param": pd.id})));
        }
    }
    // keyframes in the lane: draggable diamonds; right-click for interpolation
    if param.is_animated() {
        let y = r.center().y;
        let dur = it.duration.0.max(1) as f64;
        let rate = app.session.sequence_rate();
        for k in &param.keyframes {
            let tl = it.start + Tick(((k.time - it.source_in).0 as f64 / it.speed.abs().max(1e-6)) as i64);
            let f = ((tl - it.start).0 as f64 / dur) as f32;
            if !(-0.01..=1.01).contains(&f) {
                continue;
            }
            let id = egui::Id::new(("kf", clip.0, idx, pkey, k.time.0));
            let drag_off: Option<f32> = ui.data(|d| d.get_temp(id));
            let kx = lane.min.x + f * lane.width() + drag_off.unwrap_or(0.0);
            // about Premiere's size: a diamond ~9 points wide
            let kr = Rect::from_center_size(pos2(kx, y), vec2(15.0, 15.0));
            let resp = ui.interact(kr.expand(2.0), id, Sense::click_and_drag());
            app.auto.add(&format!("effectControls.{}.{}.keyframe.{}", e.effect, pkey, k.time.0), kr, "keyframe");
            // highlighted when selected (not merely under the playhead: that lit up every
            // parameter's keyframe at the same time, #412)
            let is_this = |s: &KeyframeRef| s.clip == clip.0 && s.effect == idx && s.mask == mask && s.param == pd.id && s.time == k.time;
            if resp.clicked() || resp.drag_started() {
                app.ui.keyframe_selection = vec![KeyframeRef { clip: clip.0, effect: idx, param: pd.id.to_string(), mask, time: k.time }];
            }
            let sel = resp.dragged() || app.ui.keyframe_selection.iter().any(is_this);
            let col = if sel { t.hot_text } else { Color32::from_rgb(0xb0, 0xb0, 0xb0) };
            match k.interp {
                filmcraft_project::Interpolation::Hold => {
                    ui.painter().rect_filled(Rect::from_center_size(kr.center(), vec2(9.0, 9.0)), 0.0, col);
                }
                filmcraft_project::Interpolation::Linear => icons::paint(ui.painter(), kr, Icon::Keyframe, col),
                _ => {
                    ui.painter().circle_filled(kr.center(), 5.5, col);
                }
            }
            if resp.dragged() {
                let off = drag_off.unwrap_or(0.0) + resp.drag_delta().x;
                ui.data_mut(|d| d.insert_temp(id, off));
            }
            if resp.drag_stopped() {
                let off = drag_off.unwrap_or(0.0);
                ui.data_mut(|d| d.remove::<f32>(id));
                let new_tl =
                    rate.snap_nearest(it.start + Tick(((f + off / lane.width()) as f64 * dur) as i64)).clamp(it.start, it.end() - rate.frame_duration());
                let new_media = it.source_in + Tick(((new_tl - it.start).0 as f64 * it.speed.abs()) as i64);
                if new_media != k.time {
                    // the moved keyframe stays selected
                    for s in app.ui.keyframe_selection.iter_mut().filter(|s| is_this(s)) {
                        s.time = new_media;
                    }
                    actions.push((
                        "effects.moveKeyframe".into(),
                        with_mask(json!({"clip": clip.0, "effect": eff_json, "param": pd.id, "mediaTime": k.time.0, "to": new_media.0})),
                    ));
                }
            }
            if resp.clicked() {
                actions.push(("playhead.set".into(), json!({"time": tl.0})));
            }
            resp.context_menu(|ui| {
                ui.label(egui::RichText::new(tl!("Temporal Interpolation")).color(t.text_dim));
                for (label, key) in [
                    (tl!("Linear"), "linear"),
                    (tl!("Bezier"), "bezier"),
                    (tl!("Auto Bezier"), "autoBezier"),
                    (tl!("Continuous Bezier"), "continuousBezier"),
                    (tl!("Hold"), "hold"),
                    (tl!("Ease In"), "easeIn"),
                    (tl!("Ease Out"), "easeOut"),
                ] {
                    if ui.button(label).clicked() {
                        actions.push((
                            "effects.setInterpolation".into(),
                            with_mask(json!({"clip": clip.0, "effect": eff_json, "param": pd.id, "mediaTime": k.time.0, "interpolation": key})),
                        ));
                        ui.close();
                    }
                }
                ui.separator();
                if ui.button(tl!("Clear")).clicked() {
                    actions
                        .push(("effects.deleteKeyframe".into(), with_mask(json!({"clip": clip.0, "effect": eff_json, "param": pd.id, "mediaTime": k.time.0}))));
                    ui.close();
                }
            });
        }
    }
}

/// Lumetri Color panel: edits (or adds) the Lumetri effect on the selected clip, grouped by section.
pub fn lumetri_panel(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let Some((clip, it, _)) = selected_clip(app) else {
        crate::dock::placeholder(ui, rect, &t, tl!("Select a clip to grade"));
        return;
    };
    let idx = it.effects.iter().position(|e| e.effect == "lumetri");
    let mut bui = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink(6.0)).id_salt("lumetri"));
    bui.label(egui::RichText::new(tlf!("Master · {name}", name = it.name)).color(t.text_dim));
    let Some(idx) = idx else {
        if bui.button(tl!("Add Lumetri Color to clip")).clicked() {
            let _ = app.session.execute("effects.apply", json!({"clips": [clip.0], "effect": "lumetri"}));
        }
        return;
    };
    let e = it.effects[idx].clone();
    let Some(def) = e.def() else { return };
    let ph = app.session.playhead();
    let mt = it.source_time_at(ph.clamp(it.start, it.end() - Tick(1)));
    let mut actions = Vec::new();
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(&mut bui, |ui| {
        let mut groups: Vec<&str> = Vec::new();
        for p in &def.params {
            if let Some(g) = p.group
                && !groups.contains(&g)
            {
                groups.push(g);
            }
        }
        for g in groups {
            let key = format!("lumetri:{g}");
            let open = !app.ui.collapsed_fx.contains(&key);
            let (resp, now_open) = crate::widgets::section_header(ui, egui::Id::new(&key), crate::i18n::t(g), open, &t, true);
            app.auto.add(&format!("lumetri.section.{g}"), resp.rect, g);
            if now_open != open {
                if now_open {
                    app.ui.collapsed_fx.retain(|k| *k != key);
                } else {
                    app.ui.collapsed_fx.push(key.clone());
                }
            }
            if !now_open {
                continue;
            }
            for pd in def.params.iter().filter(|p| p.group == Some(g)) {
                let v = e.params.get(pd.id).map(|p| p.value_at(mt)).unwrap_or(pd.default.clone());
                ui.horizontal(|ui| {
                    ui.add_space(18.0);
                    ui.add_sized(vec2(110.0, 18.0), egui::Label::new(egui::RichText::new(crate::i18n::t(pd.label)).size(12.0)));
                    if let (ParamKind::Float { min, max, soft_min, soft_max, .. }, ParamValue::Float(x)) = (&pd.kind, &v) {
                        let mut val = *x;
                        let s = ui.add(egui::Slider::new(&mut val, *soft_min..=*soft_max).show_value(false));
                        let (_, nv) =
                            crate::widgets::hot_number(ui, egui::Id::new(("lum", pd.id)), val, (soft_max - soft_min) / 300.0, (*min, *max), 1, "", &t);
                        if s.changed() {
                            actions.push(json!({"clip": clip.0, "effect": idx, "param": pd.id, "value": val}));
                        } else if let Some(nv) = nv {
                            actions.push(json!({"clip": clip.0, "effect": idx, "param": pd.id, "value": nv}));
                        }
                        if s.double_clicked() {
                            actions.push(json!({"clip": clip.0, "effect": idx, "param": pd.id, "value": pd.default.as_f64().unwrap_or(0.0)}));
                        }
                    } else if let ParamValue::Color(c) = v {
                        let mut rgba = egui::Rgba::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]);
                        if egui::color_picker::color_edit_button_rgba(ui, &mut rgba, egui::color_picker::Alpha::Opaque).changed() {
                            actions.push(json!({"clip": clip.0, "effect": idx, "param": pd.id, "value": [rgba.r(), rgba.g(), rgba.b(), 1.0]}));
                        }
                    }
                });
            }
        }
    });
    run(app, ui.ctx(), actions.into_iter().map(|a| ("effects.setParam".to_string(), a)).collect());
}

/// Properties panel (Premiere 26): a compact inspector for the selected clip.
pub fn properties_panel(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let clips = selected_clips(app);
    let Some((clip, it, kind)) = clips.first().cloned() else {
        crate::dock::placeholder(ui, rect, &t, tl!("Select a clip to see its properties"));
        return;
    };
    let ph = app.session.playhead();
    let mt = it.source_time_at(ph.clamp(it.start, it.end() - Tick(1)));
    let mut actions: Vec<(String, Value)> = Vec::new();
    let mut bui = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(14.0, 8.0))).id_salt("props"));
    // header: clip name + menu
    let (hr, _) = bui.allocate_exact_size(vec2(bui.available_width(), 30.0), Sense::hover());
    let sw = Rect::from_center_size(pos2(hr.min.x + 8.0, hr.center().y), vec2(12.0, 12.0));
    let lc = app.session.prefs.labels.rgb(it.label);
    bui.painter().rect_filled(sw, 2.0, Color32::from_rgb(lc[0], lc[1], lc[2]));
    bui.painter().text(pos2(hr.min.x + 22.0, hr.center().y), Align2::LEFT_CENTER, &it.name, Tokens::semibold(12.5), t.text);
    let row = |ui: &mut egui::Ui, label: &str| -> Rect {
        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::hover());
        ui.painter().text(pos2(r.min.x + 18.0, r.center().y), Align2::LEFT_CENTER, label, Tokens::ui(12.0), t.text_dim);
        r
    };
    let section = |ui: &mut egui::Ui, app: &mut FilmcraftApp, name: &str, reset: Option<(usize, u64)>, actions: &mut Vec<(String, Value)>| -> bool {
        ui.add_space(4.0);
        let key = format!("props:{name}");
        let open = !app.ui.collapsed_fx.contains(&key);
        let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click());
        ui.painter().line_segment([r.left_top(), r.right_top()], Stroke::new(1.0, t.separator));
        icons::paint(
            ui.painter(),
            Rect::from_center_size(pos2(r.min.x + 6.0, r.center().y), vec2(10.0, 10.0)),
            if open { Icon::ChevronDown } else { Icon::ChevronRight },
            t.text_dim,
        );
        ui.painter().text(pos2(r.min.x + 18.0, r.center().y), Align2::LEFT_CENTER, crate::i18n::t(name), Tokens::semibold(13.0), t.text);
        if let Some((idx, c)) = reset {
            let rr = Rect::from_center_size(pos2(r.max.x - PROPS_NAV_X, r.center().y), vec2(14.0, 14.0));
            icons::paint(ui.painter(), rr, Icon::Reset, t.text_dim);
            if ui.interact(rr, egui::Id::new(("props-reset", name, c)), Sense::click()).clicked() {
                actions.push(("effects.reset".into(), json!({"clip": c, "index": idx})));
            }
        }
        if resp.clicked() {
            if open {
                app.ui.collapsed_fx.push(key);
            } else {
                app.ui.collapsed_fx.retain(|k| *k != key);
            }
        }
        open
    };
    let eff_idx = |id: &str| it.effects.iter().position(|e| e.effect == id);
    let val = |eid: &str, p: &str| it.effect(eid).and_then(|e| e.param(p)).map(|p| p.value_at(mt));
    if kind == TrackKind::Video {
        if section(&mut bui, app, "Transform", eff_idx("motion").map(|i| (i, clip.0)), &mut actions) {
            for (label, p, unit, speed, range) in [
                (tl!("Position"), "position", "", 1.0, (-100_000.0, 100_000.0)),
                (tl!("Anchor point"), "anchor", "", 1.0, (-100_000.0, 100_000.0)),
                (tl!("Scale"), "scale", " %", 0.5, (0.0, 10000.0)),
                (tl!("Rotation"), "rotation", " °", 0.5, (-36000.0, 36000.0)),
            ] {
                let r = row(&mut bui, label);
                let mut vui = bui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(Rect::from_min_max(pos2(r.min.x + 150.0, r.min.y + 4.0), pos2(r.max.x - PROPS_VALUE_R, r.max.y - 4.0)))
                        .layout(egui::Layout::left_to_right(egui::Align::Center)),
                );
                match val("motion", p) {
                    Some(ParamValue::Vec2(v)) => {
                        let (_, nx) = crate::widgets::hot_number(&mut vui, egui::Id::new(("pp", p, "x", clip.0)), v.x, speed, range, 0, " X", &t);
                        vui.add_space(10.0);
                        let (_, ny) = crate::widgets::hot_number(&mut vui, egui::Id::new(("pp", p, "y", clip.0)), v.y, speed, range, 0, " Y", &t);
                        if nx.is_some() || ny.is_some() {
                            actions.push((
                                "effects.setParam".into(),
                                json!({"clip": clip.0, "effect": "motion", "param": p, "value": [nx.unwrap_or(v.x), ny.unwrap_or(v.y)]}),
                            ));
                        }
                    }
                    Some(ParamValue::Float(v)) => {
                        let (_, nv) =
                            crate::widgets::hot_number(&mut vui, egui::Id::new(("pp", p, clip.0)), v, speed, range, if p == "scale" { 0 } else { 1 }, unit, &t);
                        if let Some(nv) = nv {
                            actions.push(("effects.setParam".into(), json!({"clip": clip.0, "effect": "motion", "param": p, "value": nv})));
                        }
                    }
                    _ => {}
                }
                properties_nav(app, &mut bui, r, clip, &it, mt, "motion", p, &mut actions);
            }
            let r = row(&mut bui, tl!("Opacity"));
            let mut vui = bui.new_child(
                egui::UiBuilder::new()
                    .max_rect(Rect::from_min_max(pos2(r.min.x + 150.0, r.min.y + 4.0), pos2(r.max.x - PROPS_VALUE_R, r.max.y - 4.0)))
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
            );
            if let Some(ParamValue::Float(v)) = val("opacity", "opacity") {
                let (_, nv) = crate::widgets::hot_number(&mut vui, egui::Id::new(("pp-op", clip.0)), v, 0.5, (0.0, 100.0), 0, " %", &t);
                if let Some(nv) = nv {
                    actions.push(("effects.setParam".into(), json!({"clip": clip.0, "effect": "opacity", "param": "opacity", "value": nv})));
                }
            }
            properties_nav(app, &mut bui, r, clip, &it, mt, "opacity", "opacity", &mut actions);
        }
        let crop = eff_idx("crop");
        if section(&mut bui, app, "Crop", crop.map(|i| (i, clip.0)), &mut actions) {
            for (label, p) in [(tl!("Left"), "left"), (tl!("Top"), "top"), (tl!("Right"), "right"), (tl!("Bottom"), "bottom")] {
                let r = row(&mut bui, label);
                let mut vui = bui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(Rect::from_min_max(pos2(r.min.x + 150.0, r.min.y + 4.0), pos2(r.max.x - PROPS_VALUE_R, r.max.y - 4.0)))
                        .layout(egui::Layout::left_to_right(egui::Align::Center)),
                );
                let v = val("crop", p).and_then(|v| v.as_f64()).unwrap_or(0.0);
                let (_, nv) = crate::widgets::hot_number(&mut vui, egui::Id::new(("pp-crop", p, clip.0)), v, 0.2, (0.0, 100.0), 1, " %", &t);
                if let Some(nv) = nv {
                    if crop.is_none() {
                        actions.push(("effects.apply".into(), json!({"clips": [clip.0], "effect": "crop"})));
                    }
                    actions.push(("effects.setParam".into(), json!({"clip": clip.0, "effect": "crop", "param": p, "value": nv})));
                }
                properties_nav(app, &mut bui, r, clip, &it, mt, "crop", p, &mut actions);
            }
        }
    }
    if let Some((clip, it, _)) = clips.iter().find(|c| c.2 == TrackKind::Audio)
        && section(&mut bui, app, "Audio", it.effects.iter().position(|e| e.effect == "volume").map(|i| (i, clip.0)), &mut actions)
    {
        let clip = *clip;
        let mt = it.source_time_at(ph.clamp(it.start, (it.end() - Tick(1)).max(it.start)));
        let val = |eid: &str, p: &str| it.effect(eid).and_then(|e| e.param(p)).map(|p| p.value_at(mt));
        let r = row(&mut bui, tl!("Level"));
        let mut vui = bui.new_child(
            egui::UiBuilder::new()
                .max_rect(Rect::from_min_max(pos2(r.min.x + 150.0, r.min.y + 4.0), pos2(r.max.x - PROPS_VALUE_R, r.max.y - 4.0)))
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        let v = val("volume", "level").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let (_, nv) = crate::widgets::hot_number(&mut vui, egui::Id::new(("pp-vol", clip.0)), v, 0.1, (-96.0, 15.0), 1, " dB", &t);
        if let Some(nv) = nv {
            actions.push(("effects.setParam".into(), json!({"clip": clip.0, "effect": "volume", "param": "level", "value": nv})));
        }
        properties_nav(app, &mut bui, r, clip, it, mt, "volume", "level", &mut actions);
        let r = row(&mut bui, tl!("Pan"));
        let mut vui = bui.new_child(
            egui::UiBuilder::new()
                .max_rect(Rect::from_min_max(pos2(r.min.x + 150.0, r.min.y + 4.0), pos2(r.max.x - PROPS_VALUE_R, r.max.y - 4.0)))
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        let v = val("panner", "balance").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let (_, nv) = crate::widgets::hot_number(&mut vui, egui::Id::new(("pp-pan", clip.0)), v, 0.5, (-100.0, 100.0), 0, "", &t);
        if let Some(nv) = nv {
            actions.push(("effects.setParam".into(), json!({"clip": clip.0, "effect": "panner", "param": "balance", "value": nv})));
        }
    }
    // speed footer
    bui.add_space(12.0);
    let (br, bresp) = bui.allocate_exact_size(vec2(118.0, 26.0), Sense::click());
    bui.painter().rect_filled(br, 4.0, if bresp.hovered() { t.hover } else { t.panel_bg });
    bui.painter().rect_stroke(br, 4.0, Stroke::new(1.0, t.separator), egui::StrokeKind::Inside);
    bui.painter().text(br.center(), Align2::CENTER_CENTER, tlf!("Speed {n}%", n = format!("{:.0}", it.speed * 100.0)), Tokens::ui(12.0), t.text);
    run(app, ui.ctx(), actions);
}

/// Run the panel's actions. Parameter changes made while the mouse button is down (a drag) share
/// one undo step, which the first change of each press begins (#201); typed values and clicks stay
/// separate steps. A drag value only changes once the mouse moves, after the press frame, so the
/// press that already began a step is remembered by its start time. A change that brings its own
/// `merge` key (a Program monitor handle changing several parameters) keeps it.
pub(crate) fn run(app: &mut FilmcraftApp, ctx: &egui::Context, actions: Vec<(String, Value)>) {
    let (down, press) = ctx.input(|i| (i.pointer.any_down(), i.pointer.press_start_time()));
    let key = egui::Id::new("effect-controls-drag-step");
    for (cmd, mut p) in actions {
        if cmd == "effects.setParam" && down {
            let begun = ctx.data(|d| d.get_temp::<Option<f64>>(key)).flatten();
            if p.get("merge").is_none() {
                p["merge"] = json!(true);
            }
            p["begin"] = json!(begun != press);
            ctx.data_mut(|d| d.insert_temp(key, press));
        }
        if let Err(e) = app.session.execute(&cmd, p) {
            app.ui.status = e.to_string();
        }
    }
}

pub(crate) fn graph_key(clip: ClipId, idx: usize, pid: &str) -> String {
    format!("graph:{}:{}:{}", clip.0, idx, pid)
}

/// Parameters with value/velocity graphs: scalars, and points such as Position (#239).
pub(crate) fn graphable(v: &ParamValue) -> bool {
    matches!(v, ParamValue::Float(_) | ParamValue::Vec2(_))
}

/// The curves a graphed value draws: a scalar's one, or a point's X and Y.
fn graph_components(v: &ParamValue) -> Vec<f64> {
    match v {
        ParamValue::Float(x) => vec![*x],
        ParamValue::Vec2(p) => vec![p.x, p.y],
        _ => Vec::new(),
    }
}

/// Value and velocity graphs of an animated scalar or point parameter, drawn across the keyframe
/// lane. A point draws an X and a Y curve, and its velocity graph is the speed along its path.
/// Keyframes drag vertically (value); Bezier influence handles drag horizontally.
#[allow(clippy::too_many_arguments)]
pub(crate) fn graph_rows(
    app: &mut FilmcraftApp,
    ui: &mut egui::Ui,
    body: Rect,
    clip: ClipId,
    idx: usize,
    mask: Option<usize>,
    pd: &filmcraft_project::ParamDef,
    param: &filmcraft_project::Param,
    lane: &Rect,
    it: &TrackItem,
    actions: &mut Vec<(String, Value)>,
) {
    let with_mask = |mut v: Value| -> Value {
        if let Some(k) = mask {
            v["mask"] = json!(k);
        }
        v
    };
    let t = app.tokens;
    let (vr, _) = ui.allocate_exact_size(vec2(body.width(), 110.0), Sense::hover());
    let (velr, _) = ui.allocate_exact_size(vec2(body.width(), 64.0), Sense::hover());
    row_line(ui, velr, lane, &t);
    let speed = it.speed.abs().max(1e-6);
    let dur = it.duration.0.max(1) as f64;
    let to_media = |f: f64| it.source_in + Tick((f * dur * speed) as i64);
    let to_f = |m: Tick| ((m - it.source_in).0 as f64 / speed / dur) as f32;
    let x_of = |f: f32| lane.min.x + f * lane.width();
    // samples: one row of curve values per sample (a point's NaN "auto" coordinate is skipped)
    let n = (lane.width() / 2.0).max(8.0) as usize;
    let vals: Vec<Vec<f64>> = (0..=n).map(|i| graph_components(&param.value_at(to_media(i as f64 / n as f64)))).collect();
    let curves = graph_components(&param.value).len();
    let at_keys: Vec<Vec<f64>> = param.keyframes.iter().map(|k| graph_components(&k.value)).collect();
    let (mut lo, mut hi) = vals.iter().chain(&at_keys).flatten().filter(|v| v.is_finite()).fold((f64::MAX, f64::MIN), |(a, b), v| (a.min(*v), b.max(*v)));
    if lo > hi {
        (lo, hi) = (0.0, 0.0);
    }
    if hi - lo < 1e-6 {
        lo -= 1.0;
        hi += 1.0;
    }
    let pad = (hi - lo) * 0.12;
    let (mut lo, mut hi) = (lo - pad, hi + pad);
    if let ParamKind::Float { min, max, .. } = pd.kind {
        lo = lo.max(min);
        hi = hi.min(max);
    }
    let area = Rect::from_min_max(pos2(lane.min.x, vr.min.y + 4.0), pos2(lane.max.x, vr.max.y - 4.0));
    let y_of = |v: f64| area.max.y - ((v - lo) / (hi - lo)) as f32 * area.height();
    let v_of = |y: f32| lo + ((area.max.y - y) / area.height()) as f64 * (hi - lo);
    let p = ui.painter();
    p.rect_filled(area, 0.0, t.keyframe_plot_bg);
    for g in 1..4 {
        let y = area.min.y + area.height() * g as f32 / 4.0;
        p.line_segment([pos2(area.min.x, y), pos2(area.max.x, y)], Stroke::new(1.0, t.keyframe_plot_grid));
    }
    // range labels in the property column
    let dec = if let ParamKind::Float { decimals, .. } = pd.kind { decimals as usize } else { 1 };
    p.text(pos2(vr.max.x - 30.0, area.min.y + 6.0), Align2::RIGHT_CENTER, format!("{hi:.dec$}"), Tokens::ui(10.0), t.text_dim);
    p.text(pos2(vr.max.x - 30.0, area.max.y - 6.0), Align2::RIGHT_CENTER, format!("{lo:.dec$}"), Tokens::ui(10.0), t.text_dim);
    p.text(pos2(vr.min.x + 44.0, area.center().y), Align2::LEFT_CENTER, tl!("Value"), Tokens::ui(11.0), t.text_dim);
    let curve_color = |c: usize| if c == 0 { t.accent } else { Y_CURVE };
    if curves > 1 {
        p.text(pos2(vr.min.x + 44.0, area.center().y + 14.0), Align2::LEFT_CENTER, "X", Tokens::ui(10.5), curve_color(0));
        p.text(pos2(vr.min.x + 58.0, area.center().y + 14.0), Align2::LEFT_CENTER, "Y", Tokens::ui(10.5), curve_color(1));
    }
    for c in 0..curves {
        let line: Vec<Pos2> =
            vals.iter().enumerate().filter_map(|(i, v)| v.get(c).filter(|v| v.is_finite()).map(|v| pos2(x_of(i as f32 / n as f32), y_of(*v)))).collect();
        p.add(egui::Shape::line(line, Stroke::new(1.5, curve_color(c))));
    }
    // velocity (units per second, derivative of the sampled value; a point's speed along its path)
    let secs = (dur * speed / filmcraft_time::TICKS_PER_SECOND as f64 / n as f64).max(1e-9);
    let vel: Vec<f64> = vals
        .windows(2)
        .map(|w| {
            let [a, b] = w else { return 0.0 };
            let d: Vec<f64> = a.iter().zip(b).map(|(a, b)| (b - a) / secs).collect();
            match d.as_slice() {
                [v] => *v,
                d => d.iter().map(|v| v * v).sum::<f64>().sqrt(),
            }
        })
        .collect();
    let vmax = vel.iter().filter(|v| v.is_finite()).fold(1e-6f64, |a, v| a.max(v.abs())) * 1.15;
    let varea = Rect::from_min_max(pos2(lane.min.x, velr.min.y + 2.0), pos2(lane.max.x, velr.max.y - 4.0));
    p.rect_filled(varea, 0.0, t.keyframe_plot_bg);
    let vy = |v: f64| varea.center().y - (v / vmax) as f32 * varea.height() / 2.0;
    p.line_segment([pos2(varea.min.x, varea.center().y), pos2(varea.max.x, varea.center().y)], Stroke::new(1.0, t.keyframe_plot_axis));
    let vline: Vec<Pos2> = vel.iter().enumerate().filter(|(_, v)| v.is_finite()).map(|(i, v)| pos2(x_of((i as f32 + 0.5) / n as f32), vy(*v))).collect();
    p.add(egui::Shape::line(vline, Stroke::new(1.2, Color32::from_rgb(0xd0, 0xa0, 0x40))));
    p.text(pos2(velr.min.x + 44.0, varea.center().y), Align2::LEFT_CENTER, tl!("Velocity"), Tokens::ui(11.0), t.text_dim);
    p.text(pos2(velr.max.x - 30.0, varea.min.y + 6.0), Align2::RIGHT_CENTER, format!("{vmax:.1}/s"), Tokens::ui(10.0), t.text_dim);
    let p = p.clone();
    // keyframes + handles
    let ks = &param.keyframes;
    for (i, k) in ks.iter().enumerate() {
        let f = to_f(k.time);
        if !(-0.01..=1.01).contains(&f) {
            continue;
        }
        let eases_out =
            !matches!(k.interp, filmcraft_project::Interpolation::Linear | filmcraft_project::Interpolation::Hold | filmcraft_project::Interpolation::EaseIn);
        let eases_in =
            !matches!(k.interp, filmcraft_project::Interpolation::Linear | filmcraft_project::Interpolation::Hold | filmcraft_project::Interpolation::EaseOut);
        let comps = graph_components(&k.value);
        for (ci, &v) in comps.iter().enumerate() {
            if !v.is_finite() {
                continue;
            }
            let id = egui::Id::new(("kfg", clip.0, idx, mask, pd.id, k.time.0));
            let id = if ci == 0 { id } else { id.with(ci) };
            let dy: f32 = ui.data(|d| d.get_temp(id)).unwrap_or(0.0);
            let c = pos2(x_of(f), y_of(v) + dy);
            let r = Rect::from_center_size(c, vec2(10.0, 10.0));
            let resp = ui.interact(r.expand(2.0), id, Sense::drag());
            let auto_id = match (comps.len(), ci) {
                (1, _) => format!("effectControls.{}.graph.keyframe.{}", pd.id, k.time.0),
                (_, 0) => format!("effectControls.{}.graph.keyframe.{}.x", pd.id, k.time.0),
                _ => format!("effectControls.{}.graph.keyframe.{}.y", pd.id, k.time.0),
            };
            app.auto.add(&auto_id, r, "keyframe value");
            // influence handles: flat (ease) tangents with length ∝ influence × neighbouring segment;
            // a point's sit on its X curve only, as both coordinates share the influence
            let (eases_out, eases_in) = (ci == 0 && eases_out, ci == 0 && eases_in);
            for (side, on, nb) in [(1.0f32, eases_out, ks.get(i + 1)), (-1.0f32, eases_in && i > 0, if i > 0 { ks.get(i - 1) } else { None })] {
                let (true, Some(nb)) = (on, nb) else { continue };
                let seg = (x_of(to_f(nb.time)) - c.x).abs();
                let infl = if side > 0.0 { k.out_influence } else { k.in_influence } as f32;
                let hid = id.with(if side > 0.0 { "out" } else { "in" });
                let hdx: f32 = ui.data(|d| d.get_temp(hid)).unwrap_or(0.0);
                let hx = c.x + side * (infl * seg + hdx * side).clamp(seg * 0.01, seg);
                let hp = pos2(hx, c.y);
                p.line_segment([c, hp], Stroke::new(1.0, t.plot_handle_dim));
                p.circle_filled(hp, 3.5, t.keyframe_handle);
                let hr = ui.interact(Rect::from_center_size(hp, vec2(10.0, 10.0)), hid.with("h"), Sense::drag());
                if hr.dragged() {
                    let nx = graph_drag_offset(ui, &hr, vec2(hdx, 0.0)).x;
                    ui.data_mut(|d| d.insert_temp(hid, nx));
                }
                if hr.drag_stopped() {
                    ui.data_mut(|d| d.remove::<f32>(hid));
                    let ni = ((infl * seg + hdx * side) / seg.max(1.0)).clamp(0.01, 1.0);
                    let key = if side > 0.0 { "outInfluence" } else { "inInfluence" };
                    actions.push((
                        "effects.setKeyframe".into(),
                        with_mask(json!({"clip": clip.0, "effect": idx, "param": pd.id, "mediaTime": k.time.0, key: ni})),
                    ));
                }
            }
            p.circle_filled(c, 4.5, if resp.dragged() { t.hot_text } else { t.plot_handle });
            if resp.dragged() {
                let ny = graph_drag_offset(ui, &resp, vec2(0.0, dy)).y;
                ui.data_mut(|d| d.insert_temp(id, ny));
                p.text(c + vec2(8.0, -10.0), Align2::LEFT_BOTTOM, format!("{:.dec$}", v_of(c.y)), Tokens::ui(10.5), t.hot_text);
            }
            if resp.drag_stopped() {
                ui.data_mut(|d| d.remove::<f32>(id));
                let nv = v_of(c.y);
                let nv = if let ParamKind::Float { min, max, .. } = pd.kind { nv.clamp(min, max) } else { nv };
                let value = if comps.len() == 1 {
                    json!(nv)
                } else {
                    let mut moved = comps.clone();
                    if let Some(slot) = moved.get_mut(ci) {
                        *slot = nv;
                    }
                    json!(moved)
                };
                actions.push((
                    "effects.setKeyframe".into(),
                    with_mask(json!({"clip": clip.0, "effect": idx, "param": pd.id, "mediaTime": k.time.0, "value": value})),
                ));
            }
        }
    }
}

/// Read-only text for parameters without an inline editor (LUT references, free text, curves).
fn param_text(project: &filmcraft_project::Project, id: &str, value: &ParamValue) -> String {
    match value {
        ParamValue::Text(s) if id.ends_with("_lut") => filmcraft_render::luts::label(Some(project), s),
        ParamValue::Text(s) if s.is_empty() => tl!("None").into(),
        ParamValue::Text(s) => s.clone(),
        ParamValue::Curve(pts) if pts.is_empty() || *pts == [[0.0, 0.0], [1.0, 1.0]] => tl!("Default").into(),
        ParamValue::Curve(pts) => tlf!("Custom ({n} points)", n = pts.len()),
        other => format!("{other:?}"),
    }
}

#[cfg(test)]
mod param_text_tests {
    use super::*;

    #[test]
    fn readable_values_for_text_lut_and_curve_params() {
        let p = filmcraft_project::Project::default();
        assert_eq!(param_text(&p, "input_lut", &ParamValue::Text(String::new())), "None");
        assert_eq!(param_text(&p, "look_lut", &ParamValue::Text("builtin:look-teal-orange".into())), "Teal & Orange");
        assert_eq!(param_text(&p, "label", &ParamValue::Text(String::new())), "None");
        assert_eq!(param_text(&p, "label", &ParamValue::Text("Reel 3".into())), "Reel 3");
        assert_eq!(param_text(&p, "curve_luma", &ParamValue::Curve(vec![[0.0, 0.0], [1.0, 1.0]])), "Default");
        assert_eq!(param_text(&p, "hue_vs_sat", &ParamValue::Curve(vec![])), "Default");
        assert_eq!(param_text(&p, "curve_luma", &ParamValue::Curve(vec![[0.0, 0.0], [0.5, 0.6], [1.0, 1.0]])), "Custom (3 points)");
    }
}

#[cfg(test)]
mod drag_undo_tests {
    use serde_json::json;

    /// #201: a drag value changes only once the mouse moves, after the press frame; two drags of
    /// the same parameter are still two undo steps.
    #[test]
    fn each_press_begins_its_own_undo_step() {
        let mut s = filmcraft_engine::Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        let clip = s.active_sequence().unwrap().video_tracks[0].items[0].id.0;
        let mut app = crate::FilmcraftApp::new(s);
        let ctx = egui::Context::default();
        let opacity = |app: &crate::FilmcraftApp| {
            let q = app.session.active_sequence().unwrap();
            let it = q.find_item(filmcraft_project::ClipId(clip)).unwrap().1;
            it.effects.iter().find(|e| e.effect == "opacity").unwrap().params["opacity"].value.as_f64().unwrap()
        };
        let start = opacity(&app);
        let pos = egui::pos2(10.0, 10.0);
        let button = |pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
        let mut time = 0.0;
        let mut frame = |app: &mut crate::FilmcraftApp, events: Vec<egui::Event>, value: Option<f64>| {
            time += 0.1;
            let raw = egui::RawInput { time: Some(time), events, ..Default::default() };
            let mut out = ctx.run_ui(raw, |ui| {
                let actions = value.map(|v| ("effects.setParam".to_string(), json!({"clip": clip, "effect": "opacity", "param": "opacity", "value": v})));
                super::run(app, ui.ctx(), actions.into_iter().collect());
            });
            out.textures_delta.clear();
        };
        for values in [[90.0, 70.0], [50.0, 60.0]] {
            frame(&mut app, vec![egui::Event::PointerMoved(pos), button(true)], None); // the press: nothing changes yet
            for v in values {
                frame(&mut app, vec![egui::Event::PointerMoved(pos)], Some(v));
            }
            frame(&mut app, vec![button(false)], None);
        }
        assert_eq!(opacity(&app), 60.0);
        app.session.undo();
        assert_eq!(opacity(&app), 70.0, "undo takes back only the second drag");
        app.session.undo();
        assert_eq!(opacity(&app), start, "and then the first");
    }
}

#[cfg(test)]
mod lane_tests {
    use super::*;

    /// A context with the app's fonts (the panels use the semibold family).
    fn context() -> egui::Context {
        let ctx = egui::Context::default();
        crate::theme::install(&ctx, &Tokens::for_kind(Default::default()));
        ctx
    }

    /// A project file is only checked for overflow, so a damaged one can carry a clip with no
    /// (or a negative) duration. Selecting it must still draw both panels, ruler, playhead and
    /// keyframe navigators included.
    #[test]
    fn a_clip_without_a_duration_still_draws() {
        let mut s = filmcraft_engine::Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        let clip = s.active_sequence().unwrap().video_tracks[0].items[0].id;
        s.execute("timeline.select", json!({"clips": [clip.0]})).unwrap();
        s.execute("effects.toggleAnimation", json!({"clip": clip.0, "effect": "motion", "param": "scale"})).unwrap();
        let seq = s.state.active_sequence.unwrap();
        let mut app = crate::FilmcraftApp::new(s);
        let ctx = context();
        for duration in [0, -5 * filmcraft_time::TICKS_PER_SECOND] {
            let project = std::sync::Arc::make_mut(&mut app.session.project);
            project.sequence_mut(seq).unwrap().find_item_mut(clip).unwrap().1.duration = Tick(duration);
            let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(700.0, 500.0));
            let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
                show(&mut app, ui, rect);
                properties_panel(&mut app, ui, rect);
            });
            out.textures_delta.clear();
        }
    }

    /// The ruler takes its numbers from the project (clip times, the sequence's frame rate) and
    /// the panel's size. Anything a project that opens can carry (times within `Tick::MIN..=MAX`,
    /// a rate within `SequenceSettings::validate`, or an unset one) must not make it panic or
    /// tick forever.
    #[test]
    fn ruler_survives_damaged_numbers() {
        let mut s = filmcraft_engine::Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        let mut it = s.active_sequence().unwrap().video_tracks[0].items[0].clone();
        let ctx = context();
        let t = Tokens::for_kind(Default::default());
        let big = i64::from(u32::MAX);
        let rates = [(24000, 1001), (30, 1), (1000, 1), (1, 1), (1, big), (big, big), (0, 0), (-25, 1)];
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            for width in [300.0, 0.0, -40.0, 0.001, 1.0e9, f32::NAN, f32::INFINITY] {
                for duration in [1, 0, -7, filmcraft_time::TICKS_PER_SECOND, Tick::MAX.0 / 2] {
                    for start in [0, Tick::MIN.0, Tick::MAX.0 / 2] {
                        for (num, den) in rates {
                            it.start = Tick(start);
                            it.duration = Tick(duration);
                            let ruler = Rect::from_min_size(pos2(10.0, 10.0), vec2(width, RULER_H));
                            paint_ruler(ui.painter(), ruler, &it, filmcraft_time::FrameRate { num, den }, false, &t);
                        }
                    }
                }
            }
        });
        out.textures_delta.clear();
    }

    /// A five-second lane 300 px wide is labelled every two seconds, in sequence timecode.
    #[test]
    fn ruler_labels_sequence_timecode() {
        let mut s = filmcraft_engine::Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        let mut it = s.active_sequence().unwrap().video_tracks[0].items[0].clone();
        let rate = filmcraft_time::FrameRate { num: 24, den: 1 };
        it.start = rate.tick_of(24 * 60);
        it.duration = rate.tick_of(24 * 5);
        let ctx = context();
        let t = Tokens::for_kind(Default::default());
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            paint_ruler(ui.painter(), Rect::from_min_size(pos2(0.0, 0.0), vec2(300.0, RULER_H)), &it, rate, false, &t);
        });
        out.textures_delta.clear();
        fn texts(s: egui::Shape, out: &mut Vec<String>) {
            match s {
                egui::Shape::Vec(v) => v.into_iter().for_each(|s| texts(s, out)),
                egui::Shape::Text(t) => out.push(t.galley.text().to_string()),
                _ => {}
            }
        }
        let mut labels = Vec::new();
        out.shapes.into_iter().for_each(|c| texts(c.shape, &mut labels));
        assert_eq!(labels, ["00:01:00:00", "00:01:02:00", "00:01:04:00"]);
    }
}

#[cfg(test)]
mod graph_drag_tests {
    use super::*;
    use filmcraft_project::{Interpolation, Keyframe, Param};

    struct Driver {
        app: FilmcraftApp,
        ctx: egui::Context,
        clip: ClipId,
        time: f64,
        param: &'static str,
    }

    impl Driver {
        fn new() -> Self {
            Self::with("scale", ParamValue::Float(40.0), ParamValue::Float(80.0))
        }

        fn with(param: &'static str, from: ParamValue, to: ParamValue) -> Self {
            let mut session = filmcraft_engine::Session::default();
            session.execute("file.openDemoProject", json!({})).unwrap();
            let seq = session.state.active_sequence.unwrap();
            let clip = session.active_sequence().unwrap().video_tracks[0].items[0].id;
            let it = std::sync::Arc::make_mut(&mut session.project).sequence_mut(seq).unwrap().find_item_mut(clip).unwrap().1;
            let mut a = Keyframe::new(it.source_in + Tick(it.duration.0 / 4), from.clone());
            let mut b = Keyframe::new(it.source_in + Tick(it.duration.0 * 3 / 4), to);
            a.interp = Interpolation::Bezier;
            b.interp = Interpolation::Bezier;
            it.effects.iter_mut().find(|e| e.effect == "motion").unwrap().params.insert(param.into(), Param { value: from, keyframes: vec![a, b] });
            let app = FilmcraftApp::new(session);
            let ctx = egui::Context::default();
            crate::theme::install(&ctx, &app.tokens);
            Self { app, ctx, clip, time: 0.0, param }
        }

        fn param(&self) -> Param {
            self.app.session.active_sequence().unwrap().find_item(self.clip).unwrap().1.effect("motion").unwrap().params[self.param].clone()
        }

        fn undo_len(&mut self) -> usize {
            self.app.session.execute("history.list", json!({})).unwrap()["undo"].as_array().unwrap().len()
        }

        // Draw the actual scalar graph and execute its emitted command, as the panel does.
        // Current paint shapes give a real held-frame position rather than previous telemetry.
        fn frame(&mut self, events: Vec<egui::Event>) -> [Pos2; 2] {
            self.time += 0.1;
            self.app.auto.begin_frame();
            let mut out = self.ctx.run_ui(egui::RawInput { time: Some(self.time), events, ..Default::default() }, |ui| {
                let it = self.app.session.active_sequence().unwrap().find_item(self.clip).unwrap().1.clone();
                let index = it.effects.iter().position(|e| e.effect == "motion").unwrap();
                let effect = &it.effects[index];
                let pd = effect.def().unwrap().param(self.param).unwrap();
                let body = Rect::from_min_size(pos2(8.0, 8.0), vec2(320.0, 200.0));
                let lane = Rect::from_min_max(pos2(360.0, 8.0), pos2(760.0, 220.0));
                let mut actions = Vec::new();
                graph_rows(&mut self.app, ui, body, self.clip, index, None, pd, &effect.params[self.param], &lane, &it, &mut actions);
                run(&mut self.app, ui.ctx(), actions);
            });
            out.textures_delta.clear();
            fn first_circle(shape: &egui::Shape, radius: f32) -> Option<Pos2> {
                match shape {
                    egui::Shape::Circle(circle) if circle.radius == radius => Some(circle.center),
                    egui::Shape::Vec(shapes) => shapes.iter().find_map(|shape| first_circle(shape, radius)),
                    _ => None,
                }
            }
            [4.5, 3.5].map(|radius| {
                out.shapes.iter().find_map(|shape| first_circle(&shape.shape, radius)).expect("scalar keyframe and outgoing influence are painted")
            })
        }
    }

    fn button(pos: Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() }
    }

    #[test]
    fn scalar_graph_press_starts_at_the_pointer_origin_and_release_is_one_undo_step() {
        for influence in [false, true] {
            for batched in [true, false] {
                let mut d = Driver::new();
                let before = d.param();
                let undo = d.undo_len();
                let target = usize::from(influence);
                let start = d.frame(vec![])[target];
                // A different prior cursor makes the pre-press movement observable.
                d.frame(vec![egui::Event::PointerMoved(start - vec2(80.0, 40.0))]);
                if batched {
                    d.frame(vec![egui::Event::PointerMoved(start), button(start, true)]);
                } else {
                    d.frame(vec![egui::Event::PointerMoved(start)]);
                    d.frame(vec![button(start, true)]);
                }
                let pressed = d.frame(vec![])[target];
                assert!(pressed.distance(start) < 0.05, "press cannot jump: influence={influence}, batched={batched}, start={start:?}, pressed={pressed:?}");
                assert_eq!(d.param(), before);
                assert_eq!(d.undo_len(), undo);
                let offset = if influence { vec2(20.0, 0.0) } else { vec2(0.0, -20.0) };
                d.frame(vec![egui::Event::PointerMoved(start + offset)]);
                let preview = d.frame(vec![])[target];
                assert!(
                    preview.distance(start + offset) < 0.05,
                    "preview follows only gesture motion: influence={influence}, batched={batched}, start={start:?}, preview={preview:?}"
                );
                assert_eq!(d.param(), before, "held preview cannot commit");
                assert_eq!(d.undo_len(), undo);
                d.frame(vec![button(start + offset, false)]);
                d.frame(vec![]);
                let changed = d.param();
                assert_eq!(d.undo_len(), undo + 1);
                assert_eq!(changed.keyframes[0].time, before.keyframes[0].time);
                assert_eq!(changed.keyframes[1], before.keyframes[1]);
                if influence {
                    assert_eq!(changed.keyframes[0].value, before.keyframes[0].value);
                    assert!(changed.keyframes[0].out_influence > before.keyframes[0].out_influence);
                } else {
                    assert!(changed.keyframes[0].value.as_f64().unwrap() > before.keyframes[0].value.as_f64().unwrap());
                    assert_eq!(changed.keyframes[0].out_influence, before.keyframes[0].out_influence);
                }
                d.app.session.undo().expect("one graph gesture to undo");
                assert_eq!(d.param(), before);
                assert_eq!(d.undo_len(), undo);
                d.app.session.redo().expect("the graph gesture to redo");
                assert_eq!(d.param(), changed);
                assert_eq!(d.undo_len(), undo + 1);
            }
        }
    }

    /// #239: Position (a point) has graphs too; dragging its X keyframe moves only X, in one undo step.
    #[test]
    fn position_graph_drags_one_coordinate_of_a_point_keyframe() {
        use filmcraft_geom::Vec2;
        let mut d = Driver::with("position", ParamValue::Vec2(Vec2::new(400.0, 300.0)), ParamValue::Vec2(Vec2::new(800.0, 500.0)));
        assert!(graphable(&d.param().value));
        let before = d.param();
        let undo = d.undo_len();
        let start = d.frame(vec![])[0];
        assert!(d.app.auto.find(&format!("effectControls.position.graph.keyframe.{}.y", before.keyframes[0].time.0)).is_some());
        d.frame(vec![egui::Event::PointerMoved(start), button(start, true)]);
        d.frame(vec![egui::Event::PointerMoved(start - vec2(0.0, 20.0))]);
        d.frame(vec![button(start - vec2(0.0, 20.0), false)]);
        d.frame(vec![]);
        let changed = d.param();
        assert_eq!(d.undo_len(), undo + 1);
        let (old, new) = (before.keyframes[0].value.as_vec2().unwrap(), changed.keyframes[0].value.as_vec2().unwrap());
        assert!(new.x > old.x, "dragging the X keyframe up raises X: {old:?} -> {new:?}");
        assert_eq!(new.y, old.y);
        assert_eq!(changed.keyframes[1], before.keyframes[1]);
    }
}
