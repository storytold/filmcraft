//! Search templates, generators, fonts, effects and transitions from the Creator toolbar.
use std::collections::BTreeSet;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use egui::{ColorImage, TextureHandle, TextureOptions};
use filmcraft_engine::graphic_templates::thumbnail_cached;
use filmcraft_engine::render::{Image, effects, transitions};
use filmcraft_media::generators::{Generator, GeneratorSource};
use filmcraft_media::{FrameRequest, MediaSource};
use filmcraft_project::gtemplate::{self, GraphicsTemplate};
use filmcraft_project::keyframe::{Param, ParamValue};
use filmcraft_project::{EffectDef, EffectKind};
use filmcraft_time::Tick;
use filmcraft_ui_egui::FilmcraftApp;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::creator::selected_video_clip;
#[path = "creator/style_packs.rs"]
mod style_packs;
use style_packs::StylePacks;

const GROUPS: [&str; 7] = ["All", "Favorites", "Titles", "Fonts", "Generators", "Effects", "Transitions"];

pub(super) struct CreateMenu {
    open: bool,
    search: String,
    group: &'static str,
    selected: Option<usize>,
    assets: Vec<Asset>,
    favorites: BTreeSet<String>,
    favorite_path: Option<PathBuf>,
    preview: Option<(PreviewKey, Option<TextureHandle>)>,
    style_packs: StylePacks,
    gradient_colors: [[u8; 3]; 2],
    gradient_horizontal: bool,
}

struct Asset {
    label: String,
    group: &'static str,
    search: String,
    key: String,
    action: Action,
}

enum Action {
    Template(Box<GraphicsTemplate>),
    Font { family: String, style: String },
    Generator(&'static str),
    Effect(&'static EffectDef),
    Transition(&'static EffectDef),
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct PreviewKey {
    index: usize,
    colors: [[u8; 3]; 2],
    horizontal: bool,
}

impl Asset {
    fn new(label: &str, group: &'static str, key: &str, tags: &str, action: Action) -> Self {
        Self { label: label.into(), group, key: format!("{group}/{key}"), search: format!("{label} {group} {tags}").to_lowercase(), action }
    }
}

impl CreateMenu {
    pub fn new(private: Option<&Path>) -> Self {
        let favorite_path = private.map(|dir| dir.join("creator-favorites.json"));
        Self {
            open: false,
            search: String::new(),
            group: "All",
            selected: None,
            assets: catalog(),
            favorites: favorite_path.as_deref().and_then(read_preferences).unwrap_or_default(),
            favorite_path,
            preview: None,
            style_packs: StylePacks::new(private),
            gradient_colors: [[16, 28, 48], [43, 118, 107]],
            gradient_horizontal: false,
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui, app: &mut FilmcraftApp) {
        if ui.button("Create").clicked() {
            self.open = true;
        }
        if self.open {
            let mut open = self.open;
            let ctx = ui.ctx().clone();
            egui::Window::new("Create").open(&mut open).default_width(470.0).vscroll(true).show(&ctx, |ui| {
                if ui.button("Style packs").clicked() {
                    self.style_packs.open(app);
                }
                ui.add(egui::TextEdit::singleline(&mut self.search).hint_text("Search fonts, titles, generators, effects…"));
                ui.horizontal_wrapped(|ui| {
                    for group in GROUPS {
                        ui.selectable_value(&mut self.group, group, group);
                    }
                });
                let query = self.search.to_lowercase();
                let visible: Vec<_> = self
                    .assets
                    .iter()
                    .enumerate()
                    .filter(|(_, asset)| {
                        let in_group = match self.group {
                            "All" => true,
                            "Favorites" => self.favorites.contains(&asset.key),
                            group => asset.group == group,
                        };
                        in_group && query.split_whitespace().all(|word| asset.search.contains(word))
                    })
                    .collect();
                if visible.is_empty() {
                    ui.label("No matches");
                    self.selected = None;
                } else {
                    if !visible.iter().any(|(index, _)| Some(*index) == self.selected) {
                        self.selected = visible.first().map(|(index, _)| *index);
                    }
                    egui::ScrollArea::vertical().id_salt("create-library").max_height(218.0).show_rows(ui, 24.0, visible.len(), |ui, rows| {
                        for &(index, asset) in &visible[rows] {
                            ui.selectable_value(&mut self.selected, Some(index), &asset.label);
                        }
                    });
                }
                self.show_asset(ui, app);
            });
            self.open = open;
            if !open {
                self.preview = None;
            }
        }
        self.style_packs.show(ui.ctx(), app);
    }

    fn show_asset(&mut self, ui: &mut egui::Ui, app: &mut FilmcraftApp) {
        let Some(index) = self.selected else { return };
        let Some(asset) = self.assets.get(index) else { return };
        ui.separator();
        ui.horizontal_wrapped(|ui| {
            ui.strong(&asset.label);
            let favored = self.favorites.contains(&asset.key);
            if ui.button(if favored { "Remove from favorites" } else { "Add to favorites" }).clicked() {
                let mut next = self.favorites.clone();
                if favored {
                    next.remove(&asset.key);
                } else {
                    next.insert(asset.key.clone());
                }
                match save_preferences(self.favorite_path.as_deref(), &next) {
                    Ok(()) => self.favorites = next,
                    Err(error) => app.status(error),
                }
            }
        });
        let key = PreviewKey { index, colors: self.gradient_colors, horizontal: self.gradient_horizontal };
        if self.preview.as_ref().is_none_or(|(saved, _)| *saved != key) {
            self.preview = Some((key, preview(ui.ctx(), &asset.action, key)));
        }
        if let Some((_, Some(texture))) = &self.preview {
            ui.image((texture.id(), texture.size_vec2()));
        }
        match &asset.action {
            Action::Generator("file.newColorGradient") => {
                ui.horizontal(|ui| {
                    for color in &mut self.gradient_colors {
                        ui.color_edit_button_srgb(color);
                    }
                });
                ui.checkbox(&mut self.gradient_horizontal, "Left to right");
                if ui.button("Add gradient to project").clicked() {
                    let [start, end] = self.gradient_colors.map(|[r, g, b]| format!("#{r:02x}{g:02x}{b:02x}"));
                    run(app, "file.newColorGradient", json!({"start":start,"end":end,"horizontal":self.gradient_horizontal}));
                }
            }
            Action::Generator(command) => {
                if ui.add_enabled(app.session.is_enabled(command), egui::Button::new("Add to project")).clicked() {
                    run(app, command, json!({}));
                }
            }
            Action::Template(template) => {
                if ui.add_enabled(app.session.is_enabled("graphics.template.apply"), egui::Button::new("Insert editable graphic")).clicked() {
                    run(app, "graphics.template.apply", json!({"template":template.id}));
                }
            }
            Action::Font { family, style } => {
                if ui.add_enabled(app.session.active_sequence().is_some(), egui::Button::new("Create title with font")).clicked() {
                    run(app, "graphics.newText", json!({"text":"New title","font":family,"fontStyle":style,"newClip":true}));
                }
            }
            Action::Effect(effect) => {
                let enabled = selected_video_clip(&app.session).is_some() && app.session.is_enabled("effects.apply");
                if ui.add_enabled(enabled, egui::Button::new("Apply to selected clip")).clicked() {
                    run(app, "effects.apply", json!({"effect":effect.id, "clip":selected_video_clip(&app.session).map(|id| id.0)}));
                }
            }
            Action::Transition(effect) => {
                if let Some(clip) = selected_video_clip(&app.session) {
                    for (edge, label) in [("in", "Apply at clip start"), ("out", "Apply at clip end")] {
                        let params = json!({"effect":effect.id, "clip":clip.0, "edge":edge});
                        let enabled = filmcraft_engine::commands::preview_transition(&app.session, &params, filmcraft_project::TrackKind::Video).is_ok();
                        if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
                            run(app, "sequence.applyVideoTransition", params);
                        }
                    }
                } else {
                    ui.label("Select a video clip to apply this transition.");
                }
            }
        }
    }
}

fn catalog() -> Vec<Asset> {
    let mut assets = Vec::new();
    for template in gtemplate::builtin_templates() {
        let group = if template.category == "Generators" { "Generators" } else { "Titles" };
        let (label, id, description) = (template.name.clone(), template.id.clone(), template.description.clone());
        assets.push(Asset::new(&label, group, &id, &description, Action::Template(Box::new(template))));
    }
    for (family, styles) in filmcraft_text::fonts::picker_families() {
        for style in styles {
            let label = format!("{family} {style}");
            assets.push(Asset::new(&label, "Fonts", &label, "type typography", Action::Font { family: family.clone(), style }));
        }
    }
    for (label, tags, command) in [
        ("Gradient", "two-color background", "file.newColorGradient"),
        ("Color matte", "solid background", "file.newColorMatte"),
        ("Adjustment layer", "effect", "file.newAdjustmentLayer"),
        ("Bars and tone", "test audio", "file.newBarsAndTone"),
        ("Counting leader", "countdown", "file.newCountingLeader"),
        ("Transparent video", "empty", "file.newTransparentVideo"),
    ] {
        assets.push(Asset::new(label, "Generators", command, tags, Action::Generator(command)));
    }
    for effect in filmcraft_project::effect::effect_defs().iter().filter(|effect| !effect.intrinsic) {
        match effect.kind {
            EffectKind::Video if !effect.id.starts_with("graphic_") => {
                assets.push(Asset::new(effect.name, "Effects", effect.id, &effect.category.join(" "), Action::Effect(effect)))
            }
            EffectKind::VideoTransition => {
                let tags = if effect.id == "linear_wipe" { "wipe comparison before after".into() } else { effect.category.join(" ") };
                assets.push(Asset::new(effect.name, "Transitions", effect.id, &tags, Action::Transition(effect)));
            }
            _ => {}
        }
    }
    assets
}

fn preview(ctx: &egui::Context, action: &Action, key: PreviewKey) -> Option<TextureHandle> {
    let (width, height, pixels) = match action {
        Action::Template(template) => {
            let preview = thumbnail_cached(template, 250);
            (preview.0 as usize, preview.1 as usize, preview.2.clone())
        }
        Action::Font { family, style } => {
            let mut sample = gtemplate::builtin_templates().into_iter().find(|template| template.id == "builtin:title-centered")?;
            sample.layers.truncate(1);
            let layer = sample.layers.first_mut()?;
            for (key, value) in [("font", family.as_str()), ("font_style", style.as_str()), ("text", "Aa Bb 123")] {
                layer.params.insert(key.into(), Param::new(ParamValue::Text(value.into())));
            }
            let preview = thumbnail_cached(&sample, 250);
            (preview.0 as usize, preview.1 as usize, preview.2.clone())
        }
        Action::Effect(definition) => {
            let mut image = swatch(false);
            let cx = effects::FxCtx {
                t: Tick::ZERO,
                px_scale: 1.0,
                mask_scale: 1.0,
                seconds: 0.0,
                timecode: "00:00:00:00",
                clip_name: "Sample",
                project: None,
                env: None,
                frame_rate: 30.0,
                working: Default::default(),
            };
            effects::apply(&mut image, &definition.instance(), &cx);
            (image.w, image.h, image.to_rgba8())
        }
        Action::Transition(definition) => {
            let image = transitions::apply(&definition.instance(), &swatch(false), &swatch(true), 0.5);
            (image.w, image.h, image.to_rgba8())
        }
        Action::Generator(command) => {
            let generator = match *command {
                "file.newColorGradient" => {
                    let [start, end] = key.colors.map(|rgb| rgb.map(|channel| f32::from(channel) / 255.0));
                    Generator::ColorGradient { start, end, horizontal: key.horizontal }
                }
                "file.newColorMatte" => Generator::ColorMatte { color: [0.1, 0.1, 0.1, 1.0] },
                "file.newBarsAndTone" => Generator::BarsAndTone,
                "file.newCountingLeader" => Generator::CountingLeader,
                "file.newTransparentVideo" => Generator::TransparentVideo,
                _ => return None,
            };
            let source = GeneratorSource::new(generator, 200, 112, Default::default(), Tick::from_seconds_f64(5.0));
            let frame = source.video_frame(FrameRequest::full(Tick::from_seconds_f64(1.0))).ok()?;
            (frame.width as usize, frame.height as usize, frame.to_rgba8())
        }
    };
    Some(ctx.load_texture("create-preview", ColorImage::from_rgba_unmultiplied([width, height], &pixels), TextureOptions::LINEAR))
}

fn swatch(reversed: bool) -> Image {
    let mut image = Image::filled(200, 108, [0.08, 0.14, 0.2, 1.0]);
    let colors = [[0.04, 0.34, 0.31, 1.0], [0.57, 0.14, 0.08, 1.0], [0.42, 0.33, 0.17, 1.0]];
    for (index, pixel) in image.px.chunks_exact_mut(4).enumerate() {
        let (x, y) = (index % image.w, index / image.w);
        if (15..185).contains(&x) && (14..94).contains(&y) {
            let column = (x - 15) / 57;
            pixel.copy_from_slice(&colors[if reversed { 2 - column } else { column }]);
        }
    }
    image
}

fn run(app: &mut FilmcraftApp, command: &str, params: Value) {
    if let Err(error) = app.session.execute(command, params) {
        app.status(error.to_string());
    }
}

fn read_preferences<T: DeserializeOwned>(path: &Path) -> Option<T> {
    let file = File::open(path).ok()?;
    serde_json::from_reader(file.take(256 * 1024)).ok()
}

fn save_preferences<T: serde::Serialize>(path: Option<&Path>, value: &T) -> Result<(), String> {
    let Some(path) = path else { return Ok(()) };
    let bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    filmcraft_format::atomic_write(path, &bytes).map_err(|error| format!("Could not save Creator preferences. {error}"))
}
