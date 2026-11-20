use std::borrow::Cow;
use std::path::{Path, PathBuf};

use egui::{ColorImage, TextureHandle, TextureOptions};
use filmcraft_engine::commands::preview_transition;
use filmcraft_engine::graphic_templates::thumbnail_cached;
use filmcraft_engine::render::{Image, caption_overlays, transitions};
use filmcraft_project::{Caption, CaptionStyle, CaptionTrack, ClipId, EffectKind, Project, SequenceSettings, TrackId, TrackKind, gtemplate, vtransition};
use filmcraft_time::Tick;
use filmcraft_ui_egui::FilmcraftApp;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{read_preferences, run, save_preferences, selected_video_clip};

#[derive(Clone, Deserialize, Serialize)]
struct StylePack {
    name: Cow<'static, str>,
    template: Cow<'static, str>,
    font: Cow<'static, str>,
    font_style: Cow<'static, str>,
    color: [u8; 4],
    background: [u8; 4],
    size: f32,
    transition: Cow<'static, str>,
}

const PACKS: &[StylePack] = &[
    StylePack {
        name: Cow::Borrowed("Social clips"),
        template: Cow::Borrowed("builtin:portrait-title-card"),
        font: Cow::Borrowed("Barlow Condensed"),
        font_style: Cow::Borrowed("Bold"),
        color: [255, 255, 255, 255],
        background: [16, 28, 48, 221],
        size: 54.0,
        transition: Cow::Borrowed("cross_dissolve"),
    },
    StylePack {
        name: Cow::Borrowed("Interviews"),
        template: Cow::Borrowed("builtin:lower-third-rule"),
        font: Cow::Borrowed("Atkinson Hyperlegible"),
        font_style: Cow::Borrowed("Regular"),
        color: [255, 255, 255, 255],
        background: [17, 17, 17, 221],
        size: 54.0,
        transition: Cow::Borrowed("cross_dissolve"),
    },
    StylePack {
        name: Cow::Borrowed("Product demos"),
        template: Cow::Borrowed("builtin:title-boxed"),
        font: Cow::Borrowed("Inter"),
        font_style: Cow::Borrowed("SemiBold"),
        color: [250, 244, 229, 255],
        background: [44, 33, 56, 221],
        size: 54.0,
        transition: Cow::Borrowed("dip_to_black"),
    },
];

pub(super) struct StylePacks {
    open: bool,
    pack: usize,
    caption_track: Option<TrackId>,
    custom: Vec<StylePack>,
    path: Option<PathBuf>,
    draft: StylePack,
    name: String,
    fonts: Vec<(String, Vec<String>)>,
    message: String,
}

#[derive(Clone)]
struct Preview {
    title: Option<TextureHandle>,
    caption: TextureHandle,
    transition: Vec<TextureHandle>,
}

impl StylePacks {
    pub(super) fn new(private: Option<&Path>) -> Self {
        let path = private.map(|dir| dir.join("creator-styles.json"));
        Self {
            open: false,
            pack: 0,
            caption_track: None,
            custom: path.as_deref().and_then(read_preferences).unwrap_or_default(),
            path,
            draft: PACKS[0].clone(),
            name: String::new(),
            fonts: filmcraft_text::fonts::picker_families(),
            message: String::new(),
        }
    }

    pub(super) fn open(&mut self, app: &FilmcraftApp) {
        self.open = true;
        self.caption_track =
            app.session.state.caption_selection.first().and_then(|&id| app.session.active_sequence()?.find_caption(id).map(|(track, _)| track));
    }

    pub(super) fn show(&mut self, ctx: &egui::Context, app: &mut FilmcraftApp) {
        if !self.open {
            return;
        }
        let mut open = true;
        egui::Window::new("Style packs").open(&mut open).default_width(390.0).vscroll(true).show(ctx, |ui| {
            let selected = self.pack;
            let packs: Vec<_> = PACKS.iter().chain(&self.custom).cloned().collect();
            egui::ComboBox::from_id_salt("creator-style-pack")
                .selected_text(packs.get(selected).map(|pack| pack.name.as_ref()).unwrap_or("Style pack"))
                .show_ui(ui, |ui| {
                    for (index, pack) in packs.iter().enumerate() {
                        let response = ui.selectable_value(&mut self.pack, index, pack.name.as_ref());
                        app.auto.add_response(&format!("creator.stylePacks.pack.{index}"), &response, pack.name.as_ref());
                    }
                });
            if selected != self.pack
                && let Some(pack) = packs.get(self.pack)
            {
                self.draft = pack.clone();
                self.name.clear();
            }
            ui.collapsing("Customize caption style", |ui| {
                egui::ComboBox::from_id_salt("creator-style-font").selected_text(self.draft.font.as_ref()).show_ui(ui, |ui| {
                    for (family, styles) in &self.fonts {
                        if ui.selectable_label(self.draft.font == family.as_str(), family).clicked() {
                            self.draft.font = Cow::Owned(family.clone());
                            self.draft.font_style = Cow::Owned(styles.first().cloned().unwrap_or_else(|| "Regular".into()));
                        }
                    }
                });
                if let Some((_, styles)) = self.fonts.iter().find(|(family, _)| self.draft.font == family.as_str()) {
                    egui::ComboBox::from_id_salt("creator-style-font-face").selected_text(self.draft.font_style.as_ref()).show_ui(ui, |ui| {
                        for style in styles {
                            if ui.selectable_label(self.draft.font_style == style.as_str(), style).clicked() {
                                self.draft.font_style = Cow::Owned(style.clone());
                            }
                        }
                    });
                }
                ui.horizontal_wrapped(|ui| {
                    ui.label("Text");
                    ui.color_edit_button_srgba_unmultiplied(&mut self.draft.color);
                    ui.label("Background");
                    ui.color_edit_button_srgba_unmultiplied(&mut self.draft.background);
                });
                ui.add(egui::Slider::new(&mut self.draft.size, 24.0..=104.0).text("Caption size"));
                egui::ComboBox::from_id_salt("creator-style-transition")
                    .selected_text(
                        vtransition::find_transition(self.draft.transition.as_ref(), EffectKind::VideoTransition)
                            .map(|definition| definition.name)
                            .unwrap_or("Transition"),
                    )
                    .show_ui(ui, |ui| {
                        for id in ["cross_dissolve", "dip_to_black", "linear_wipe", "push", "slide"] {
                            if let Some(definition) = vtransition::find_transition(id, EffectKind::VideoTransition) {
                                ui.selectable_value(&mut self.draft.transition, Cow::Borrowed(id), definition.name);
                            }
                        }
                    });
                ui.add(egui::TextEdit::singleline(&mut self.name).hint_text("Name your style pack").char_limit(64));
                let name = self.name.trim();
                let unique = !packs.iter().any(|pack| pack.name.eq_ignore_ascii_case(name));
                if ui.add_enabled(!name.is_empty() && unique && self.custom.len() < 48, egui::Button::new("Save as style pack")).clicked() {
                    let mut next = self.custom.clone();
                    next.push(StylePack { name: Cow::Owned(name.into()), ..self.draft.clone() });
                    match save_preferences(self.path.as_deref(), &next) {
                        Ok(()) => {
                            self.pack = PACKS.len() + self.custom.len();
                            if let Some(pack) = next.last() {
                                self.draft = pack.clone();
                            }
                            self.custom = next;
                            self.name.clear();
                            self.message = "Style pack saved.".into();
                        }
                        Err(error) => self.message = error,
                    }
                }
                if !self.message.is_empty() {
                    ui.label(&self.message);
                }
            });
            self.show_pack(ui, app, &self.draft.clone());
        });
        self.open = open;
    }

    fn show_pack(&mut self, ui: &mut egui::Ui, app: &mut FilmcraftApp, pack: &StylePack) {
        let preview = preview(ui.ctx(), pack);
        ui.separator();
        ui.horizontal_wrapped(|ui| {
            if let Some(title) = &preview.title {
                let size = title.size_vec2();
                let scale = (170.0 / size.x).min(118.0 / size.y);
                ui.image((title.id(), size * scale));
            }
            let button = ui.add_enabled(app.session.active_sequence().is_some(), egui::Button::new("Insert editable title"));
            app.auto.add_response("creator.stylePacks.insertTitle", &button, "Insert editable title");
            if button.clicked() {
                run(app, "graphics.template.apply", json!({"template": pack.template}));
            }
        });
        ui.separator();
        ui.label(format!("Captions · {} {}", pack.font, pack.font_style));
        ui.image((preview.caption.id(), preview.caption.size_vec2()));
        self.show_caption_controls(ui, app, pack);
        ui.separator();
        transition_controls(ui, app, pack, &preview.transition);
    }

    fn show_caption_controls(&mut self, ui: &mut egui::Ui, app: &mut FilmcraftApp, pack: &StylePack) {
        let tracks = app.session.active_sequence().map(|sequence| sequence.caption_tracks.as_slice()).unwrap_or_default();
        let Some((selected, selected_name)) =
            self.caption_track.and_then(|id| tracks.iter().find(|track| track.id == id)).or_else(|| tracks.first()).map(|track| (track.id, track.name.clone()))
        else {
            ui.label("Add a caption track to apply this style.");
            return;
        };
        let mut target = selected;
        let apply = ui
            .horizontal_wrapped(|ui| {
                ui.label("Caption track");
                let picker = egui::ComboBox::from_id_salt("creator-caption-track").selected_text(selected_name).show_ui(ui, |ui| {
                    for (index, track) in tracks.iter().enumerate() {
                        let name = format!("C{} · {}", index + 1, track.name);
                        let response = ui.selectable_value(&mut target, track.id, &name);
                        app.auto.add_response(&format!("creator.stylePacks.captionTrack.{}", track.id.0), &response, &name);
                    }
                });
                app.auto.add_response("creator.stylePacks.captionTrack", &picker.response, "Caption track");
                let button = ui.button("Apply caption style");
                app.auto.add_response("creator.stylePacks.applyCaptions", &button, "Apply caption style");
                button.clicked()
            })
            .inner;
        if apply {
            run(
                app,
                "captions.setStyle",
                json!({"track": target.0, "font": pack.font, "fontStyle": pack.font_style,
                    "color": pack.color, "background": true, "backgroundColor": pack.background, "anchor": "bottom", "size":pack.size}),
            );
        }
        self.caption_track = Some(target);
    }
}

fn transition_controls(ui: &mut egui::Ui, app: &mut FilmcraftApp, pack: &StylePack, preview: &[TextureHandle]) {
    if let Some(definition) = vtransition::find_transition(pack.transition.as_ref(), EffectKind::VideoTransition) {
        ui.label(definition.name);
    }
    ui.horizontal_wrapped(|ui| {
        for (label, image) in ["Before", "Midpoint", "After"].into_iter().zip(preview) {
            ui.vertical(|ui| {
                ui.image((image.id(), image.size_vec2()));
                ui.small(label);
            });
        }
    });

    let target = selected_video_clip(&app.session).and_then(|clip| {
        let sequence = app.session.active_sequence()?;
        let (track, item) = sequence.find_item(clip)?;
        let track = sequence.track(track)?;
        Some((clip, format!("{} · {}", item.name, track.name)))
    });
    let Some((clip, name)) = target else {
        ui.label("Select a video clip to apply this transition.");
        return;
    };
    ui.label(name);
    ui.horizontal_wrapped(|ui| {
        for (label, edge) in [("Apply at start", "in"), ("Apply at end", "out")] {
            let args = json!({"clip": clip.0, "edge": edge, "effect": pack.transition});
            let available = preview_transition(&app.session, &args, TrackKind::Video);
            let mut button = ui.add_enabled(available.is_ok(), egui::Button::new(label));
            if let Err(error) = available {
                button = button.on_disabled_hover_text(error.to_string());
            }
            app.auto.add_response(&format!("creator.stylePacks.transition.{edge}"), &button, label);
            if button.clicked() {
                run(app, "sequence.applyVideoTransition", args);
            }
        }
    });
}

fn preview(ctx: &egui::Context, pack: &StylePack) -> Preview {
    let slot = egui::Id::unique("creator-style-preview");
    let key = egui::Id::unique((&pack.template, &pack.font, &pack.font_style, pack.size.to_bits(), pack.color, pack.background, &pack.transition));
    if let Some((_, preview)) = ctx.data(|cache| cache.get_temp::<(egui::Id, Preview)>(slot)).filter(|(cached, _)| *cached == key) {
        return preview;
    }
    let title = gtemplate::builtin_templates().into_iter().find(|template| template.id == pack.template.as_ref()).map(|template| {
        let px = thumbnail_cached(&template, 192);
        texture(ctx, "style-title-preview", px.0 as usize, px.1 as usize, &px.2)
    });
    let caption = caption_preview(ctx, pack);
    let transition = vtransition::find_transition(pack.transition.as_ref(), EffectKind::VideoTransition).map_or_else(Vec::new, |effect| {
        let from = Image::filled(80, 42, [0.025, 0.29, 0.24, 1.0]);
        let to = Image::filled(80, 42, [0.48, 0.12, 0.10, 1.0]);
        let effect = effect.instance();
        [0.0, 0.5, 1.0]
            .into_iter()
            .map(|progress| {
                let image = transitions::apply(&effect, &from, &to, progress);
                texture(ctx, "style-transition-preview", image.w, image.h, &image.to_rgba8())
            })
            .collect()
    });
    let preview = Preview { title, caption, transition };
    ctx.data_mut(|cache| cache.insert_temp(slot, (key, preview.clone())));
    preview
}

fn caption_preview(ctx: &egui::Context, pack: &StylePack) -> TextureHandle {
    let (width, height) = (272, 80);
    let mut image = Image::filled(width, height, [0.045, 0.056, 0.078, 1.0]);
    let mut project = Project::new("Caption preview");
    let id = project.new_sequence("Caption preview", SequenceSettings::default(), 0, 0, None);
    let track_id = TrackId(project.alloc_id());
    let caption_id = ClipId(project.alloc_id());
    if let Some(sequence) = project.sequence_mut(id).filter(|_| image.w == width) {
        let mut track = CaptionTrack::new(track_id, "Preview".into(), Default::default());
        track.style = CaptionStyle {
            font: pack.font.to_string(),
            font_style: pack.font_style.to_string(),
            color: pack.color,
            background_color: pack.background,
            // Make the real typeface readable in the small sample.
            size: (pack.size * 4.0).clamp(110.0, 340.0),
            margin: 0.28,
            ..Default::default()
        };
        track.captions.push(Caption {
            id: caption_id,
            start: Tick::ZERO,
            duration: Tick::from_seconds_f64(1.0),
            text: "Sample caption".into(),
            speaker: None,
            cue_id: None,
            settings: String::new(),
        });
        sequence.caption_tracks.push(track);
        for overlay in caption_overlays(sequence, Tick::ZERO, width, height) {
            overlay.composite_onto(&mut image.px, width, height);
        }
    }
    texture(ctx, "style-caption-preview", image.w, image.h, &image.to_rgba8())
}

fn texture(ctx: &egui::Context, id: &str, w: usize, h: usize, rgba: &[u8]) -> TextureHandle {
    let color = if w > 0 && h > 0 { ColorImage::from_rgba_unmultiplied([w, h], rgba) } else { ColorImage::filled([1, 1], egui::Color32::TRANSPARENT) };
    ctx.load_texture(id, color, TextureOptions::LINEAR)
}
