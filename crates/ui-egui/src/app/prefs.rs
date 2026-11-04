use super::{FilmcraftApp, ThemeKind, Tokens, dock, i18n, state, theme};

impl FilmcraftApp {
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
            self.workspace_restored = true;
            if let Some(w) = dock::find(&self.workspaces, &self.workspaces.current) {
                self.set_workspace(&w);
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
    pub(super) fn sync_timeline_view(&mut self) {
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
            self.timeline_view_of = Some(active);
            match stored.and_then(|s| s.checked()) {
                Some(s) => {
                    show(v, s);
                    self.timeline_view_last = Some(s);
                }
                None => {
                    let d = state::TimelineView::default();
                    (v.v_scroll, v.a_scroll, v.video_track_h, v.audio_track_h) = (0.0, 0.0, d.video_track_h, d.audio_track_h);
                    (v.fit_pending, v.fit_empty) = (true, None);
                    self.timeline_view_last = None;
                }
            }
        } else if v.fit_pending {
            // the panel has not fitted the sequence yet: nothing to keep
        } else if self.timeline_view_last != Some(shown) {
            self.session.state.timeline_views.insert(active, shown);
            self.timeline_view_last = Some(shown);
        } else if let Some(s) = stored.filter(|s| *s != shown).and_then(|s| s.checked()) {
            show(v, s);
            self.timeline_view_last = Some(s);
            self.session.state.timeline_views.insert(active, s);
        }
    }
}
