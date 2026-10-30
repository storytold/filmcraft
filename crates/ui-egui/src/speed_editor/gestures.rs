//! Secondary legends are software gestures: the HID device only reports key edges.
//! A deferred first action prevents a double tap from also inserting or splitting clips.

use std::time::Duration;

use egui::Context;
use filmcraft_project::{ClipId, ItemId, TrackKind, TransitionId};
use filmcraft_time::Tick;
use serde_json::json;

use super::Button;
use crate::{FilmcraftApp, dock::PanelKind};

pub(super) const DOUBLE_TAP_SECONDS: f64 = 0.35;

#[derive(Default)]
pub(super) struct Taps {
    previous: Option<(Button, f64, bool)>,
}

impl Taps {
    pub fn press(&mut self, button: Button, now: f64) -> bool {
        let double = now.is_finite()
            && self.previous.is_some_and(|(previous, time, released)| previous == button && released && now >= time && now - time <= DOUBLE_TAP_SECONDS);
        self.previous = if double || !now.is_finite() || button.secondary_label().is_none() { None } else { Some((button, now, false)) };
        double
    }

    pub fn release(&mut self, button: Button) {
        if let Some((previous, _, released)) = &mut self.previous
            && *previous == button
        {
            *released = true;
        }
    }

    pub fn clear(&mut self) {
        self.previous = None;
    }
}

pub(super) struct Pending {
    pub button: Button,
    until: f64,
    revision: u64,
    sequence: Option<ItemId>,
    source_item: Option<ItemId>,
    playhead: Tick,
}

#[derive(Default)]
pub(super) struct Gestures {
    pub taps: Taps,
    pub pending: Option<Pending>,
}

impl Button {
    pub(super) fn secondary_label(self) -> Option<&'static str> {
        use Button::*;
        Some(match self {
            Escape => "UNDO",
            Split => "MOVE",
            AudioLevel => "MARK",
            TransitionDuration => "SET DEFAULT DURATION",
            Roll => "SLIDE",
            In => "CLEAR IN",
            Out => "CLEAR OUT",
            SmartInsert => "INSERT WHOLE CLIP",
            Append => "APPEND WHOLE CLIP",
            Transition => "TITLE",
            FullView => "REVIEW",
            CloseUp => "Y POSITION",
            Snap => "VIEWER ZOOM",
            LiveOverwrite => "RANDOM",
            _ => return None,
        })
    }

    fn deferred(self) -> bool {
        matches!(self, Self::Split | Self::SmartInsert | Self::Append | Self::Transition | Self::FullView | Self::CloseUp | Self::Snap | Self::LiveOverwrite)
    }
}

impl FilmcraftApp {
    pub(super) fn cancel_speed_editor_gestures(&mut self) {
        self.speed_editor.gestures = Gestures::default();
        self.speed_editor.secondary_held = None;
    }

    pub(super) fn flush_speed_editor_press(&mut self, ctx: &Context, force: bool) -> Result<(), String> {
        let Some(pending) = &self.speed_editor.gestures.pending else { return Ok(()) };
        let now = self.now();
        if !now.is_finite() {
            self.cancel_speed_editor_gestures();
            return Err("Controller clock is unavailable; tap the key again".into());
        }
        if force || (now >= pending.until && !self.speed_editor.held.contains(&pending.button)) {
            if let Some(pending) = self.speed_editor.gestures.pending.take() {
                if pending.revision != self.session.revision
                    || pending.sequence != self.session.state.active_sequence
                    || pending.source_item != self.session.state.source_item
                {
                    self.speed_editor.gestures.taps.clear();
                    return Err("Pending key cancelled because the editing context changed".into());
                }
                if pending.button == Button::Split {
                    self.speed_editor_command(ctx, "sequence.addEdit", json!({"time":pending.playhead.0}))?;
                } else {
                    self.speed_editor_press(ctx, pending.button)?;
                }
            }
        } else if now < pending.until {
            ctx.request_repaint_after(Duration::from_secs_f64((pending.until - now).clamp(0.001, DOUBLE_TAP_SECONDS)));
        }
        Ok(())
    }

    pub(super) fn speed_editor_key_down(&mut self, ctx: &Context, button: Button, now: f64) -> Result<(), String> {
        if !now.is_finite() {
            self.cancel_speed_editor_gestures();
            return Err("Controller clock is unavailable; tap the key again".into());
        }
        if self.speed_editor.gestures.pending.as_ref().is_some_and(|pending| {
            pending.revision != self.session.revision
                || pending.sequence != self.session.state.active_sequence
                || pending.source_item != self.session.state.source_item
        }) {
            self.cancel_speed_editor_gestures();
            return Err("Pending key cancelled because the editing context changed".into());
        }
        let double = self.speed_editor.held.len() == 1 && self.speed_editor.gestures.taps.press(button, now);
        if double {
            self.speed_editor.gestures.pending = None;
            self.reset_speed_editor(ctx);
            self.speed_editor.secondary_held = Some(button);
            self.speed_editor_secondary(ctx, button)
        } else {
            if button == Button::Escape {
                self.speed_editor.gestures.pending = None;
            } else {
                self.flush_speed_editor_press(ctx, true)?;
            }
            if button.deferred() {
                self.speed_editor.gestures.pending = Some(Pending {
                    button,
                    until: now + DOUBLE_TAP_SECONDS,
                    revision: self.session.revision,
                    sequence: self.session.state.active_sequence,
                    source_item: self.session.state.source_item,
                    playhead: self.session.playhead(),
                });
                ctx.request_repaint_after(Duration::from_secs_f64(DOUBLE_TAP_SECONDS));
                Ok(())
            } else if button == Button::Roll {
                // Selecting a roll clears the clip selection needed by double-tap Slide.
                // Arm it only once the first held press actually turns the dial.
                Ok(())
            } else {
                self.speed_editor_press(ctx, button)
            }
        }
    }

    fn speed_editor_secondary(&mut self, ctx: &Context, button: Button) -> Result<(), String> {
        use Button::*;
        let id = match button {
            Escape => "edit.undo",
            AudioLevel => "markers.add",
            In => "markers.clearIn",
            Out => "markers.clearOut",
            SmartInsert | Append => return self.speed_editor_place(ctx, button, true),
            Split | Roll | CloseUp => {
                self.stop();
                self.stop_source();
                if !self.session.state.edit_points.is_empty() {
                    self.speed_editor_command(ctx, "trim.clear", json!({}))?;
                }
                self.status(format!("Speed Editor: {} · hold and turn the dial", button.secondary_label().unwrap_or("Secondary action")));
                return Ok(());
            }
            TransitionDuration => {
                let (_, kind, duration) = self.speed_editor_transition()?;
                let frames = self.session.sequence_rate().frame_at(duration);
                if !(1..=10_000).contains(&frames) {
                    return Err("Default transition duration must be between 1 and 10,000 frames".into());
                }
                let prefix = if kind == TrackKind::Video { "video" } else { "audio" };
                // Write both values atomically so a stored duration in seconds becomes frames.
                self.speed_editor_command(
                    ctx,
                    "prefs.set",
                    json!({"values":{
                        format!("timeline.{prefix}TransitionUnit"):"frames",
                        format!("timeline.{prefix}TransitionDuration"):frames
                    }}),
                )?;
                self.status(format!("Speed Editor: default {prefix} transition duration set to {frames} frames"));
                return Ok(());
            }
            Transition => {
                self.speed_editor_command(ctx, "graphics.newText", json!({"text":"Title", "newClip":true}))?;
                return Ok(());
            }
            FullView => {
                self.speed_editor_command(ctx, "trim.selectNearest", json!({"kind":"roll"}))?;
                if self.ui.keys.maximized.is_some_and(|panel| panel != PanelKind::Program) {
                    self.speed_editor_command(ctx, "window.maximizeFrame", json!({}))?;
                }
                self.speed_editor_command(ctx, "window.panel.program", json!({}))?;
                if self.ui.keys.maximized != Some(PanelKind::Program) {
                    self.speed_editor_command(ctx, "window.maximizeFrame", json!({}))?;
                }
                "playback.playAround"
            }
            Snap => {
                let source = self.speed_editor.source;
                let zoom = if source { self.ui.source.zoom } else { self.ui.program.zoom };
                match (source, zoom.is_some()) {
                    (true, true) => "view.sourceZoomFit",
                    (true, false) => "view.sourceZoom100",
                    (false, true) => "view.programZoomFit",
                    (false, false) => "view.programZoom100",
                }
            }
            LiveOverwrite => return Err("RANDOM multicamera insertion needs a Sync Bin, which FilmCraft does not support yet".into()),
            _ => return Err("This key has no secondary action".into()),
        };
        self.speed_editor_command(ctx, id, json!({})).map(|_| ())
    }

    fn speed_editor_clip(&self) -> Result<ClipId, String> {
        self.session.state.selection.first().copied().ok_or_else(|| "Select a timeline clip first".into())
    }

    fn speed_editor_transition(&self) -> Result<(TransitionId, TrackKind, Tick), String> {
        let seq = self.session.active_sequence().ok_or("Open a sequence first")?;
        let selected = self.session.state.transition_selection.first();
        seq.all_tracks()
            .filter(|track| !track.locked)
            .flat_map(|track| track.transitions.iter().map(move |transition| (track, transition)))
            .filter(|(_, transition)| selected.is_none_or(|id| *id == transition.id))
            .min_by_key(|(_, transition)| transition.start.0.saturating_add(transition.duration.0 / 2).abs_diff(self.session.playhead().0))
            .map(|(track, transition)| (transition.id, track.kind, transition.duration))
            .ok_or_else(|| "Select an unlocked transition or add a transition near the playhead first".into())
    }

    pub(super) fn speed_editor_secondary_wheel(&mut self, ctx: &Context, button: Button, frames: i64) -> Result<bool, String> {
        use Button::*;
        match button {
            Roll if self.speed_editor.secondary_held != Some(Roll) => {
                if self.session.state.edit_points.is_empty() {
                    let remainder = self.speed_editor.remainder;
                    self.speed_editor_press(ctx, Roll)?;
                    self.speed_editor.remainder = remainder;
                }
                return Ok(false);
            }
            Split => {
                let sequence = self.session.active_sequence().ok_or("Open a sequence first")?;
                let delta = sequence.settings.frame_rate.sane().tick_of(frames).0;
                let mut seen = Vec::new();
                let mut moves = Vec::new();
                for clip in &self.session.state.selection {
                    if seen.contains(clip) {
                        continue;
                    }
                    seen.push(*clip);
                    let (track, item) = sequence.find_item(*clip).ok_or("Selected clip is unavailable")?;
                    moves.push(json!({"clip":clip.0,"track":track.0,"time":item.start.0.saturating_add(delta).min(Tick::MAX.0)}));
                }
                if moves.is_empty() {
                    return Err("Select clips to move".into());
                }
                self.speed_editor_command(ctx, "timeline.move", json!({"moves":moves,"insert":false}))?;
            }
            Roll if self.speed_editor.secondary_held == Some(Roll) => {
                let clip = self.speed_editor_clip()?;
                self.speed_editor_command(ctx, "timeline.slide", json!({"clip":clip.0,"deltaFrames":frames}))?;
            }
            AudioLevel if self.speed_editor.secondary_held == Some(AudioLevel) => return Ok(true),
            AudioLevel => {
                self.speed_editor_command(ctx, "clip.audioGain", json!({"mode":"adjust","db":frames as f64 * 0.1}))?;
            }
            TransitionDuration => {
                let (transition, _, duration) = self.speed_editor_transition()?;
                let length = self.session.sequence_rate().frame_at(duration).saturating_add(frames).clamp(1, 10_000_000);
                self.speed_editor_command(ctx, "sequence.setTransition", json!({"transition":transition.0,"frames":length}))?;
            }
            CloseUp if self.speed_editor.secondary_held == Some(CloseUp) => {
                let clip = self.speed_editor_clip()?;
                let sequence = self.session.active_sequence().ok_or("Open a sequence first")?;
                let (_, item) = sequence.find_item(clip).ok_or("Selected clip is unavailable")?;
                let motion = item.effect("motion").ok_or("Selected clip has no Motion effect")?;
                let at = Tick(self.session.playhead().0.saturating_sub(item.start.0));
                let position = motion.vec2_at("position", at);
                if !position.x.is_finite() || !position.y.is_finite() {
                    return Err("Selected clip has an invalid position".into());
                }
                let y = (position.y + frames as f64).clamp(-100_000.0, 100_000.0);
                self.speed_editor_command(ctx, "effects.setParam", json!({"clip":clip.0,"effect":"motion","param":"position","value":[position.x,y]}))?;
            }
            CloseUp | Snap => return Ok(true),
            _ => return Ok(false),
        }
        Ok(true)
    }
}

#[cfg(test)]
#[path = "gestures_tests.rs"]
mod tests;
