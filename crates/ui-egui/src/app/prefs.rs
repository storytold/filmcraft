use super::{FilmcraftApp, ThemeKind, Tokens, dock, i18n, state, theme};

impl FilmcraftApp {
    /// Persist an explicit control-channel view request before acknowledging it, even before the
    /// sequence's automatic first fit has run.
    pub(crate) fn explicit_timeline_view_edit(&mut self, keep_pending_fit: bool) {
        let Some(active) = self.session.state.active_sequence else { return };
        if keep_pending_fit {
            self.tl.track_anim = None;
            self.session.state.timeline_pending_fits.insert(active);
            let shown = filmcraft_engine::project::SequenceView {
                pps: 40.0,
                scroll: 0.0,
                v_scroll: self.ui.timeline.v_scroll,
                a_scroll: self.ui.timeline.a_scroll,
                video_track_h: self.ui.timeline.video_track_h,
                audio_track_h: self.ui.timeline.audio_track_h,
            };
            let previous = self.session.state.timeline_views.insert(active, shown);
            self.timeline_view_of = Some(active);
            self.timeline_view_last = None;
            if previous != Some(shown) {
                self.session.view_changed();
                self.session.sync_persistence();
            }
            return;
        }
        self.session.state.timeline_pending_fits.remove(&active);
        if !keep_pending_fit {
            self.ui.timeline.fit_pending = false;
        }
        self.ui.timeline.fit_empty = None;
        let shown = filmcraft_engine::project::SequenceView {
            pps: self.ui.timeline.target_pps,
            scroll: self.ui.timeline.target_scroll,
            v_scroll: self.ui.timeline.v_scroll,
            a_scroll: self.ui.timeline.a_scroll,
            video_track_h: self.ui.timeline.video_track_h,
            audio_track_h: self.ui.timeline.audio_track_h,
        };
        let Some(shown) = shown.checked() else { return };
        let previous = self.session.state.timeline_views.insert(active, shown).and_then(|view| view.checked());
        self.timeline_view_of = Some(active);
        self.timeline_view_last = Some(shown);
        if previous != Some(shown) {
            self.session.view_changed();
            self.session.sync_persistence();
        }
    }

    /// Show theme `k` with the Settings ▸ Appearance highlight colour and contrast.
    pub fn set_theme(&mut self, ctx: &egui::Context, k: ThemeKind) {
        let a = &self.session.prefs.appearance;
        let highlight = filmcraft_engine::settings::parse_hex(&a.highlight_color);
        self.tokens = Tokens::for_kind(k).with_appearance(highlight, a.accessible_contrast);
        theme::apply_visuals(ctx, &self.tokens);
        self.apply_tooltips(ctx);
        self.ui.dark = k != ThemeKind::Light;
    }

    /// The system appearance: the host's reading, else what the windowing toolkit reports.
    pub fn system_theme(&self, ctx: &egui::Context) -> Option<egui::Theme> {
        self.hooks.system_theme.as_ref().and_then(|read| read(ctx)).or_else(|| ctx.system_theme())
    }

    /// The theme Settings ▸ Appearance selects: the light or dark theme of the Appearance Mode,
    /// Auto following the system (dark when it gives no answer).
    pub fn appearance_kind(&self, ctx: &egui::Context) -> ThemeKind {
        let light = self.system_theme(ctx).map(|t| t == egui::Theme::Light);
        ThemeKind::from_pref(self.session.prefs.appearance.shown_theme(light))
    }

    fn apply_tooltips(&self, ctx: &egui::Context) {
        // Settings ▸ General ▸ Show Tool Tips
        let delay = if self.session.prefs.general.show_tool_tips { 0.5 } else { 1.0e9 };
        ctx.global_style_mut(|s| s.interaction.tooltip_delay = delay);
    }

    /// Settings ▸ Appearance ▸ UI Scale: a fixed scale makes a point that many physical pixels,
    /// whatever scale the system reports. It is applied only when the setting or the system scale
    /// changes, so Ctrl+- / Ctrl++ still zoom the interface for the session; going back to Auto
    /// restores the system's scale.
    fn apply_ui_scale(&mut self, ctx: &egui::Context) {
        let want = self.session.prefs.appearance.ui_scale_factor();
        let native = ctx.native_pixels_per_point().filter(|v| v.is_finite() && *v > 0.0).unwrap_or(1.0);
        if self.applied_ui_scale == Some((want, native)) {
            return;
        }
        let prev = self.applied_ui_scale.replace((want, native));
        match want {
            Some(scale) => ctx.set_zoom_factor(scale / native),
            None if prev.is_some_and(|(p, _)| p.is_some()) => ctx.set_zoom_factor(1.0),
            None => {}
        }
    }

    /// View ▸ Reset Interface Zoom: the scale Settings ▸ Appearance ▸ UI Scale asks for (the
    /// system's with Auto), undoing any session zoom (#2253).
    /// Returns the zoom factor set (egui reports it from the next pass on).
    pub(crate) fn reset_ui_zoom(&mut self, ctx: &egui::Context) -> f32 {
        let native = ctx.native_pixels_per_point().filter(|v| v.is_finite() && *v > 0.0).unwrap_or(1.0);
        let zoom = self.session.prefs.appearance.ui_scale_factor().map_or(1.0, |scale| scale / native);
        ctx.set_zoom_factor(zoom);
        zoom
    }

    /// Point the scrub output at the sequence that is active now (#2633).
    /// [`FilmcraftApp::apply_prefs`] carries the sequence's sample rate to the output, but it
    /// returns early while the settings stay the same, and Settings ▸ Sequence, opening another
    /// project or switching sequences change what the next grain needs without changing them. A
    /// stale rate mixes the grain at the wrong speed; and a stream that is still open keeps the
    /// fill it was opened with, which mixes the sequence that was active then, so another sequence
    /// needs it opened again even at the same rate.
    fn sync_scrub_output(&mut self) {
        let rate = self.session.active_sequence().map(|q| q.settings.sample_rate);
        let sequence = self.session.state.active_sequence;
        if rate == self.scrub_rate && sequence == self.scrub_sequence {
            return;
        }
        let rate_changed = rate != self.scrub_rate;
        self.scrub_rate = rate;
        self.scrub_sequence = sequence;
        if rate_changed {
            let hw = self.session.prefs.audio_hardware.clone();
            if let Some(out) = self.scrub_out.as_mut() {
                out.stop();
                out.configure(&hw, rate);
            }
        }
        self.scrub.device_stopped();
    }

    /// Make the UI follow the settings after they change (theme, tooltips, frame cache budget,
    /// play after rendering, audio device).
    pub fn apply_prefs(&mut self, ctx: &egui::Context) {
        self.apply_ui_scale(ctx);
        // Appearance Mode ▸ Sync with system: follow the system while running. The host reports
        // changes as they happen (no polling) and wakes the UI only when the value changes.
        if self.applied_prefs.is_some() {
            let k = self.appearance_kind(ctx);
            if k != self.tokens.kind {
                self.set_theme(ctx, k);
            }
        }
        // checked every frame: a theme or font change installs the style again
        let animation = if self.session.prefs.appearance.animations { theme::ANIMATION_TIME } else { 0.0 };
        if ctx.global_style().animation_time != animation {
            ctx.global_style_mut(|s| s.animation_time = animation);
        }
        if self.applied_prefs.as_ref() == Some(&self.session.prefs) {
            // A sequence can ask the device for another sample rate without touching the
            // preferences: Settings ▸ Sequence, opening another project, switching sequences.
            self.sync_scrub_output();
            return;
        }
        let p = self.session.prefs.clone();
        let prev = self.applied_prefs.take();
        if prev.as_ref().is_none_or(|q| q.appearance != p.appearance || q.general.show_tool_tips != p.general.show_tool_tips) {
            let k = self.appearance_kind(ctx);
            self.set_theme(ctx, k);
        }
        if prev.as_ref().is_none_or(|q| q.general.interface_language != p.general.interface_language) {
            let language = match i18n::Language::parse(&p.general.interface_language) {
                Some(l) => l,
                None if p.general.interface_language == "system" => {
                    i18n::Language::from_locales(&self.hooks.system_languages.as_ref().map(|f| f()).unwrap_or_default())
                }
                None => i18n::Language::default(),
            };
            if language == i18n::Language::Ja && !i18n::install_japanese_font(ctx) {
                self.ui.language = i18n::Language::En;
                self.ui.status = tl!("no Japanese font is installed on this system; the interface stays in English").into();
            } else if language == i18n::Language::ZhCn && !i18n::chinese_font_available() {
                self.ui.language = i18n::Language::En;
                self.ui.status = tl!("no Chinese font is installed on this system; the interface stays in English").into();
            } else if language == i18n::Language::Ko && !i18n::korean_font_available() {
                self.ui.language = i18n::Language::En;
                self.ui.status = tl!("no Korean font is installed on this system; the interface stays in English").into();
            } else {
                self.ui.language = language;
            }
            i18n::set_current(self.ui.language);
            ctx.set_language(Some(self.ui.language.bcp47().to_owned()));
        }
        self.frames.set_cache_budget(p.memory.frame_cache_mb as usize * (1 << 20));
        self.ui.play_after_render = p.timeline.play_after_rendering;
        let hardware_changed = prev.as_ref().is_none_or(|q| q.audio_hardware != p.audio_hardware);
        let pitch_changed = prev.as_ref().is_some_and(|q| q.audio.maintain_pitch_shuttling != p.audio.maintain_pitch_shuttling);
        if hardware_changed {
            if let Some(input) = self.session.voiceover.input.as_mut() {
                input.configure_host(&p.audio_hardware.device_class);
            }
            let rate = self.session.active_sequence().map(|q| q.settings.sample_rate);
            if let Some(a) = self.audio.as_mut() {
                a.stop();
                a.configure(&p.audio_hardware, rate);
            }
            if let Some(out) = self.scrub_out.as_mut() {
                out.stop();
                out.configure(&p.audio_hardware, rate);
            }
            self.scrub.device_stopped();
            self.scrub_rate = rate;
            self.scrub_sequence = self.session.state.active_sequence;
        }
        if (hardware_changed || pitch_changed) && self.playback.playing && self.playback.preroll.is_none() {
            self.playback.anchor_tick = self.session.playhead();
            let now = self.now();
            self.playback.anchor_time = now;
            self.playback.audio_seen = (0, now);
            self.start_audio(now);
        }
        if let Some(note) = self.audio.as_ref().and_then(|a| a.note()) {
            self.ui.status = note;
        }
        if prev.is_none() && !self.workspace_restored {
            // reopen the workspace in use when the app last closed
            if let Some(w) = dock::find(&self.workspaces, &self.workspaces.current) {
                self.workspace_restored = true;
                self.set_workspace(&w);
                self.restore_remembered_layout(&w);
            }
        }
        self.applied_prefs = Some(p);
    }

    /// Every sequence has its own Timeline view (zoom, scroll, track heights), as Premiere's
    /// sequence tabs do. `ui.timeline` holds the active sequence's; the views of all sequences are
    /// in `session.state.timeline_views`, which is saved with the project. Each frame this
    /// - gives `ui.timeline` the view of a sequence that has just become active (a sequence shown
    ///   for the first time is fitted, with default track heights),
    /// - writes a change made in the panel to the session,
    /// - and takes over a view that was changed in the session (by a command or the control
    ///   channel).
    pub(crate) fn sync_timeline_view(&mut self) {
        let Some(active) = self.session.state.active_sequence else {
            self.timeline_view_of = None;
            return;
        };
        let stored = self.session.state.timeline_views.get(&active).copied();
        let v = &mut self.ui.timeline;
        let shown = filmcraft_engine::project::SequenceView {
            pps: v.target_pps,
            scroll: v.target_scroll,
            v_scroll: v.v_scroll,
            a_scroll: v.a_scroll,
            video_track_h: v.video_track_h,
            audio_track_h: v.audio_track_h,
        };
        let show = |v: &mut state::TimelineView, s: filmcraft_engine::project::SequenceView| {
            (v.pps, v.target_pps, v.scroll, v.target_scroll) = (s.pps, s.pps, s.scroll, s.scroll);
            (v.v_scroll, v.a_scroll, v.video_track_h, v.audio_track_h) = (s.v_scroll, s.a_scroll, s.video_track_h, s.audio_track_h);
            (v.fit_pending, v.fit_empty) = (false, None);
        };
        if self.timeline_view_of != Some(active) {
            self.tl.track_anim = None;
            self.timeline_view_of = Some(active);
            // the zoom ceiling is applied by animate_view with the measured content width --
            // clamping here (before the first layout) would use the placeholder width
            match stored.and_then(|s| s.checked()) {
                Some(s) => {
                    if self.session.state.timeline_pending_fits.contains(&active) {
                        (v.v_scroll, v.a_scroll, v.video_track_h, v.audio_track_h) = (s.v_scroll, s.a_scroll, s.video_track_h, s.audio_track_h);
                        (v.fit_pending, v.fit_empty) = (true, None);
                        self.timeline_view_last = None;
                    } else {
                        show(v, s);
                        self.timeline_view_last = Some(s);
                    }
                }
                None => {
                    let d = state::TimelineView::default();
                    (v.v_scroll, v.a_scroll, v.video_track_h, v.audio_track_h) = (0.0, 0.0, d.video_track_h, d.audio_track_h);
                    (v.fit_pending, v.fit_empty) = (true, None);
                    self.timeline_view_last = None;
                }
            }
        } else if !v.fit_pending && self.session.state.timeline_pending_fits.remove(&active) {
            self.session.state.timeline_views.insert(active, shown);
            self.timeline_view_last = Some(shown);
        } else if v.fit_pending {
            // the panel has not fitted the sequence yet: nothing to keep
        } else if self.timeline_view_last != Some(shown) {
            let previous = self.timeline_view_last;
            self.session.state.timeline_views.insert(active, shown);
            self.timeline_view_last = Some(shown);
            // the zoom and scroll are part of the saved session: carry the change into the
            // autosave journal via the view counter (never the revision -- these change on
            // every frame of a drag, and a clean project must stay clean)
            let playback_only_scroll = self.tl.playback_auto_scroll.take().is_some_and(|(sequence, scroll)| {
                sequence == active
                    && scroll == shown.scroll
                    && previous.is_some_and(|previous| {
                        previous.pps == shown.pps
                            && previous.scroll != shown.scroll
                            && previous.v_scroll == shown.v_scroll
                            && previous.a_scroll == shown.a_scroll
                            && previous.video_track_h == shown.video_track_h
                            && previous.audio_track_h == shown.audio_track_h
                    })
            });
            if !playback_only_scroll {
                self.session.view_changed();
            }
        } else if let Some(s) = stored.filter(|s| *s != shown).and_then(|s| s.checked()) {
            show(v, s);
            self.timeline_view_last = Some(s);
            self.session.state.timeline_views.insert(active, s);
        }
    }
}

#[cfg(test)]
mod playback_view_dirty_tests {
    use super::*;
    use filmcraft_engine::Session;

    #[test]
    fn startup_restores_the_remembered_layout_for_a_valid_workspace() {
        let mut app = FilmcraftApp::new(Session::default());
        let expected = dock::saved_workspace(&dock::WorkspacePrefs::default(), "Color").layout;
        app.workspaces.current = "Editing".into();
        app.workspaces.last = Some(dock::SavedWorkspace::new("Editing", &expected).unwrap());

        app.apply_prefs(&egui::Context::default());

        assert!(app.workspace_restored);
        assert_eq!(app.ui.dock, expected);
    }

    #[test]
    fn playback_follow_scroll_is_saved_without_marking_the_view_user_dirty() {
        let mut session = Session::default();
        session.execute("file.openDemoProject", serde_json::json!({})).unwrap();
        let active = session.state.active_sequence.unwrap();
        let mut app = FilmcraftApp::new(session);
        app.ui.timeline.fit_pending = false;
        let view = filmcraft_engine::project::SequenceView {
            pps: app.ui.timeline.target_pps,
            scroll: app.ui.timeline.target_scroll,
            v_scroll: app.ui.timeline.v_scroll,
            a_scroll: app.ui.timeline.a_scroll,
            video_track_h: app.ui.timeline.video_track_h,
            audio_track_h: app.ui.timeline.audio_track_h,
        };
        app.timeline_view_of = Some(active);
        app.timeline_view_last = Some(view);
        app.session.state.timeline_views.insert(active, view);
        app.session.saved_view_epoch = app.session.view_epoch;
        app.playback.playing = true;
        app.ui.timeline.target_scroll = view.scroll + 5.0;
        app.tl.playback_auto_scroll = Some((active, app.ui.timeline.target_scroll));

        app.sync_timeline_view();
        assert_eq!(app.session.view_epoch, app.session.saved_view_epoch, "automatic follow does not prompt to save");
        assert_eq!(app.session.state.timeline_views.get(&active).map(|view| view.scroll), Some(view.scroll + 5.0));

        app.ui.timeline.target_scroll += 1.0;
        app.tl.playback_auto_scroll = Some((active, app.ui.timeline.target_scroll - 1.0));
        app.sync_timeline_view();
        assert_ne!(app.session.view_epoch, app.session.saved_view_epoch, "a manual scroll still marks the view unsaved");
    }

    #[test]
    fn a_pending_first_fit_keeps_the_requested_track_height_without_inherited_zoom() {
        let mut session = Session::default();
        session.execute("file.openDemoProject", serde_json::json!({})).unwrap();
        let first = session.state.active_sequence.unwrap();
        let first_view =
            filmcraft_engine::project::SequenceView { pps: 91.0, scroll: 27.0, v_scroll: 0.0, a_scroll: 0.0, video_track_h: 60.0, audio_track_h: 60.0 };
        session.state.timeline_views.insert(first, first_view);
        let mut app = FilmcraftApp::new(session);
        app.sync_timeline_view();
        let second = app.session.execute("file.newSequence", serde_json::json!({"name": "Second"})).unwrap()["sequence"].as_u64().unwrap();
        app.sync_timeline_view();
        assert!(app.ui.timeline.fit_pending);
        app.ui.timeline.video_track_h = 80.0;
        app.explicit_timeline_view_edit(true);
        let second = filmcraft_engine::project::ItemId(second);
        let stored = app.session.state.timeline_views.get(&second).copied();
        assert_eq!(stored.map(|view| (view.pps, view.scroll, view.video_track_h)), Some((40.0, 0.0, 80.0)));
        let project = app.session.project.clone();
        let saved_view = app.session.project_view();

        let mut reopened = Session::default();
        reopened.project = project;
        reopened.restore_project_view(saved_view);
        let mut reopened_app = FilmcraftApp::new(reopened);
        reopened_app.sync_timeline_view();
        assert!(reopened_app.ui.timeline.fit_pending);
        assert_eq!(reopened_app.ui.timeline.video_track_h, 80.0);
        assert_eq!(reopened_app.ui.timeline.target_pps, 40.0);
        assert_eq!(reopened_app.ui.timeline.target_scroll, 0.0);
    }
}
