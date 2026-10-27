use super::*;

// ---------------------------------------------------------------------------------------------
// middle: settings
// ---------------------------------------------------------------------------------------------

const LABEL_W: f32 = 132.0;

/// A labelled settings row.
fn row<R>(ui: &mut egui::Ui, t: &Tokens, label: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.horizontal(|ui| {
        ui.add_sized(vec2(LABEL_W, 20.0), egui::Label::new(egui::RichText::new(label).color(t.text_dim).size(12.0)).truncate());
        add(ui)
    })
    .inner
}

/// A combo box over `options` (label, enabled); returns the chosen index.
pub(super) fn combo(ui: &mut egui::Ui, reg: &mut Reg, id: &str, selected: &str, options: &[(String, bool)], width: f32) -> Option<usize> {
    let why = tl!("Not supported by FilmCraft's encoders yet");
    let options: Vec<(String, Option<&str>)> = options.iter().map(|(l, on)| (l.clone(), (!on).then_some(why))).collect();
    combo_why(ui, reg, id, selected, &options, width)
}

/// [`combo`] whose options carry why they are greyed out (label, `Some(reason)` when disabled).
fn combo_why(ui: &mut egui::Ui, reg: &mut Reg, id: &str, selected: &str, options: &[(String, Option<&str>)], width: f32) -> Option<usize> {
    let mut chosen = None;
    let r = craft_ui::ComboBox::from_id_salt(id).selected_text(crate::i18n::t(selected)).width(width).show_ui(ui, |ui| {
        for (i, (label, why)) in options.iter().enumerate() {
            let enabled = why.is_none();
            let resp = ui.add_enabled(enabled, egui::Button::selectable(label == selected, crate::i18n::t(label)));
            let resp = match why {
                Some(why) => resp.on_disabled_hover_text(*why),
                None => resp,
            };
            reg.add(format!("{id}.option.{i}"), resp.rect, label.clone());
            if resp.clicked() {
                chosen = Some(i);
            }
        }
    });
    reg.add(id, r.response.rect, selected);
    chosen
}

/// Why a format cannot be exported on this machine, or None when it can. H.265 and AV1 have no
/// software encoder: without a hardware one (NVENC on NVIDIA, VideoToolbox on macOS) they are
/// greyed out with the reason, instead of failing at export time.
fn unavailable_why(format: Format) -> Option<&'static str> {
    if filmcraft_engine::export::available(format) {
        return None;
    }
    Some(match format {
        Format::Hevc => tl!(
            "H.265 (HEVC) needs a hardware encoder (NVENC on NVIDIA GPUs, VideoToolbox on macOS); none was found on this machine, and FilmCraft has no software H.265 encoder yet. Choose H.264 or ProRes instead."
        ),
        Format::Av1 => tl!("AV1 needs a hardware encoder; none was found on this machine, and FilmCraft has no software AV1 encoder yet."),
        _ => tl!("Not supported by FilmCraft's encoders yet"),
    })
}

fn opts(labels: &[&str]) -> Vec<(String, bool)> {
    labels.iter().map(|l| (l.to_string(), true)).collect()
}

fn section(ui: &mut egui::Ui, reg: &mut Reg, open: &mut Vec<String>, key: &str, title: &str, t: &Tokens) -> bool {
    let is_open = open.iter().any(|k| k == key);
    let (r, now) = crate::widgets::section_header(ui, egui::Id::unique(("export-sec", key)), title, is_open, t, true);
    reg.add(format!("export.section.{key}"), r.rect, title);
    if now != is_open {
        if now {
            open.push(key.into());
        } else {
            open.retain(|k| k != key);
        }
    }
    now
}

pub(super) fn drag(
    ui: &mut egui::Ui,
    reg: &mut Reg,
    id: &str,
    v: &mut f64,
    range: std::ops::RangeInclusive<f64>,
    speed: f64,
    suffix: &str,
    decimals: usize,
) -> bool {
    let before = *v;
    let r = ui.add(crate::widgets::number_drag(v).range(range).speed(speed).suffix(suffix).max_decimals(decimals));
    reg.add(id, r.rect, format!("{v}"));
    // a drag ended by Escape goes back to the value it started from (egui ends the drag, #580)
    let home = r.id.with("before-drag");
    if r.drag_started() {
        ui.data_mut(|d| d.insert_temp(home, before));
    }
    if r.drag_stopped()
        && let Some(b) = ui.data_mut(|d| d.remove_temp::<f64>(home))
        && ui.input(|i| i.key_pressed(egui::Key::Escape))
    {
        *v = b;
        return true;
    }
    r.changed()
}

pub(super) fn check(ui: &mut egui::Ui, reg: &mut Reg, id: &str, v: &mut bool, label: &str) -> bool {
    let r = ui.checkbox(v, label);
    reg.add(id, r.rect, label);
    r.changed()
}

fn text(ui: &mut egui::Ui, reg: &mut Reg, id: &str, v: &mut String, hint: &str) {
    let r = crate::widgets::accessible_name(ui.add(egui::TextEdit::singleline(v).hint_text(hint).desired_width(f32::INFINITY)), hint);
    reg.add(id, r.rect, hint);
}

/// A text field over a value the caller fills in again every frame (a file name taken from a path,
/// a folder that falls back to the default one). While it has focus it shows what is typed, even
/// when that is not usable yet (emptied to retype, a half-typed path); each non-empty value is
/// returned as it comes. Leaving shows the stored value again.
pub(super) fn draft_text(ui: &mut egui::Ui, reg: &mut Reg, id: &str, stored: &str, hint: &str) -> Option<String> {
    let draft_id = egui::Id::unique(("export-draft", id));
    let mut v = ui.data(|m| m.get_temp::<String>(draft_id)).unwrap_or_else(|| stored.to_string());
    let r = crate::widgets::accessible_name(ui.add(egui::TextEdit::singleline(&mut v).hint_text(hint).desired_width(f32::INFINITY)), hint);
    reg.add(id, r.rect, hint);
    if r.has_focus() {
        ui.data_mut(|m| m.insert_temp(draft_id, v.clone()));
    } else {
        ui.data_mut(|m| m.remove::<String>(draft_id));
    }
    (r.changed() && !v.trim().is_empty()).then_some(v)
}

/// Longest folder tail the link lays out or shows on hover.
pub(super) const MAX_LOCATION_DISPLAY_BYTES: usize = 4096;

/// Keep the tail of a UTF-8 folder name without copying or splitting a codepoint. Project item
/// names can enter the generated export path, so UI display work must not scale with an arbitrary
/// imported name.
pub(super) fn location_tail(s: &str) -> (&str, bool) {
    if s.len() <= MAX_LOCATION_DISPLAY_BYTES {
        return (s, false);
    }
    let mut start = s.len() - MAX_LOCATION_DISPLAY_BYTES;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    (&s[start..], true)
}

/// The output folder as a link (folder icon, accent colour, underlined); the caller opens the save
/// dialog on a click. A path too long for the row loses its start, so the folder's name stays.
pub(super) fn location_link(ui: &mut egui::Ui, t: &Tokens, folder: &str) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::click());
    let col = if resp.hovered() { t.accent_hover } else { t.accent };
    crate::icons::paint(ui.painter(), Rect::from_center_size(pos2(r.min.x + 8.0, r.center().y), vec2(14.0, 14.0)), crate::icons::Icon::Folder, col);
    let room = (r.width() - 22.0).max(0.0);
    let (folder, truncated) = location_tail(folder);
    let mut galley = ui.painter().layout_no_wrap(folder.to_string(), Tokens::ui(12.5), col);
    if galley.size().x > room {
        let starts: Vec<usize> = folder.char_indices().map(|(i, _)| i).collect();
        let suffix = if starts.len() > 1 {
            let mut lo = 1usize;
            let mut hi = starts.len() - 1;
            while lo < hi {
                let mid = (lo + hi) / 2;
                let candidate = format!("…{}", &folder[starts[mid]..]);
                if ui.painter().layout_no_wrap(candidate, Tokens::ui(12.5), col).size().x <= room {
                    hi = mid;
                } else {
                    lo = mid + 1;
                }
            }
            format!("…{}", &folder[starts[lo]..])
        } else {
            "…".into()
        };
        galley = ui.painter().layout_no_wrap(suffix, Tokens::ui(12.5), col);
    }
    let at = pos2(r.min.x + 20.0, r.center().y - galley.size().y / 2.0);
    let underline = at.y + galley.size().y;
    ui.painter().hline(at.x..=at.x + galley.size().x, underline, egui::Stroke::new(1.0, col));
    ui.painter().galley(at, galley, col);
    let hover = if truncated { format!("…{folder}") } else { folder.to_string() };
    resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(hover)
}

const RATES: [(&str, FrameRate); 11] = [
    ("10", FrameRate { num: 10, den: 1 }),
    ("12", FrameRate { num: 12, den: 1 }),
    ("15", FrameRate { num: 15, den: 1 }),
    ("23.976", FrameRate::FPS_23_976),
    ("24", FrameRate::FPS_24),
    ("25", FrameRate::FPS_25),
    ("29.97", FrameRate::FPS_29_97),
    ("30", FrameRate::FPS_30),
    ("50", FrameRate::FPS_50),
    ("59.94", FrameRate::FPS_59_94),
    ("60", FrameRate::FPS_60),
];

pub(crate) const PARS: [(&str, Option<(u32, u32)>); 6] = [
    ("Square Pixels (1.0)", None),
    ("D1/DV NTSC (0.9091)", Some((10, 11))),
    ("D1/DV PAL (1.0940)", Some((59, 54))),
    ("HD Anamorphic 1080 (1.333)", Some((4, 3))),
    ("DVCPRO HD (1.5)", Some((3, 2))),
    ("Anamorphic 2:1 (2.0)", Some((2, 1))),
];

pub(super) fn settings_column(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let mut reg = Reg::default();
    let mut open_manager = false;
    let mut chosen_preset: Option<String> = None;
    let (seq_w, seq_h, seq_rate, seq_sr) = app
        .session
        .active_sequence()
        .map(|q| (q.settings.width, q.settings.height, q.settings.frame_rate, q.settings.sample_rate))
        .unwrap_or((1920, 1080, FrameRate::FPS_24, 48_000));
    let has_captions = app.session.active_sequence().is_some_and(|q| !q.caption_tracks.is_empty());
    // an HDR sequence exports HDR (H.265 Main 10 where the hardware encoder has it) unless SDR is asked for
    let seq_hdr = app.session.active_sequence().is_some_and(|q| q.settings.color.working.is_hdr());
    let all_presets = app.session.export_presets.all();
    let favs: Vec<String> = all_presets.iter().filter(|p| app.session.export_presets.is_favorite(&p.name)).map(|p| p.name.clone()).collect();
    let mut pick_folder = false;
    let mut pick_overlay = false;
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(14.0, 10.0))).id_salt("export-settings"));
    let ex = &mut app.ui.export;
    egui::ScrollArea::vertical().id_salt("export-settings-scroll").auto_shrink([false, false]).show(&mut child, |ui| {
        ui.label(egui::RichText::new(tl!("Media File")).strong().size(14.0));
        ui.add_space(6.0);
        row(ui, &t, tl!("File Name"), |ui| {
            if let Some(v) = draft_text(ui, &mut reg, "export.fileName", &ex.file_name, tl!("Sequence name")) {
                ex.file_name = v;
            }
        });
        row(ui, &t, tl!("Location"), |ui| {
            let r = ui.small_button("…");
            reg.add("export.location.browse", r.rect, "Choose folder");
            pick_folder = r.clicked();
            if let Some(v) = draft_text(ui, &mut reg, "export.location", &ex.location, tl!("Folder")) {
                ex.location = v;
            }
        });
        // Preset: favourites, then every preset by category, then More presets…
        row(ui, &t, tl!("Preset"), |ui| {
            let r = craft_ui::ComboBox::from_id_salt("export.preset").selected_text(crate::i18n::t(&ex.preset)).width(ui.available_width() - 4.0).show_ui(
                ui,
                |ui| {
                    if !favs.is_empty() {
                        ui.label(egui::RichText::new(tl!("Favorites")).color(t.text_dim).size(11.0));
                        for f in &favs {
                            if ui.selectable_label(*f == ex.preset, crate::i18n::t(f)).clicked() {
                                chosen_preset = Some(f.clone());
                            }
                        }
                        ui.separator();
                    }
                    let mut cat = String::new();
                    for p in &all_presets {
                        if p.category != cat {
                            cat = p.category.clone();
                            ui.label(egui::RichText::new(crate::i18n::t(&cat)).color(t.text_dim).size(11.0));
                        }
                        if ui.selectable_label(p.name == ex.preset, crate::i18n::t(&p.name)).clicked() {
                            chosen_preset = Some(p.name.clone());
                        }
                    }
                    ui.separator();
                    if ui.button(tl!("More presets…")).clicked() {
                        open_manager = true;
                    }
                },
            );
            reg.add("export.preset", r.response.rect, ex.preset.clone());
        });
        ui.horizontal(|ui| {
            ui.add_space(LABEL_W + 8.0);
            let r = ui.link(tl!("More presets…"));
            reg.add("export.preset.more", r.rect, "More presets…");
            if r.clicked() {
                open_manager = true;
            }
        });
        row(ui, &t, tl!("Format"), |ui| {
            // a format without an encoder here says why: H.265 and AV1 have hardware encoders only
            let labels: Vec<(String, Option<&str>)> = Format::ALL.iter().map(|f| (f.label().to_string(), unavailable_why(*f))).collect();
            if let Some(i) = combo_why(ui, &mut reg, "export.format", ex.settings.format.label(), &labels, ui.available_width() - 4.0) {
                let f = Format::ALL[i];
                if f != ex.settings.format {
                    // a format switch starts from that format's defaults (keeping size / rate / range)
                    let keep = ex.settings.clone();
                    ex.settings = ExportSettings {
                        format: f,
                        frame_size: keep.frame_size,
                        frame_rate: keep.frame_rate,
                        effects: keep.effects,
                        metadata: keep.metadata,
                        ..Default::default()
                    };
                }
            }
        });
        // a preset can name a format this machine cannot encode (an H.265 preset without a hardware
        // encoder): say so where the format is chosen, before the export fails
        if let Some(why) = unavailable_why(ex.settings.format) {
            let r = ui.label(egui::RichText::new(why).color(t.danger).small());
            reg.add("export.format.unavailable", r.rect, why);
        }
        ui.add_space(8.0);
        let s = &mut ex.settings;
        if s.has_video() && section(ui, &mut reg, &mut ex.open_sections, "video", tl!("Video"), &t) {
            video_section(ui, &mut reg, s, &t, seq_w, seq_h, seq_hdr);
        }
        if (s.has_audio()
            || !s.has_video()
            || s.format.is_mp4_video()
            || matches!(s.format, Format::ProRes | Format::DnxHr | Format::Apv | Format::Mjpeg)
            || s.format.is_mxf())
            && !s.is_image_sequence()
            && s.format != Format::Gif
            && section(ui, &mut reg, &mut ex.open_sections, "audio", tl!("Audio"), &t)
        {
            audio_section(ui, &mut reg, s, &t, seq_sr);
        }
        if s.format.is_h26x() && section(ui, &mut reg, &mut ex.open_sections, "multiplexer", tl!("Multiplexer"), &t) {
            row(ui, &t, tl!("Multiplexer"), |ui| {
                let cur = if s.multiplexer == Multiplexer::Mp4 { "MP4" } else { "QuickTime" };
                if let Some(i) = combo(ui, &mut reg, "export.multiplexer", cur, &opts(&["MP4", "QuickTime"]), 160.0) {
                    s.multiplexer = if i == 0 { Multiplexer::Mp4 } else { Multiplexer::Mov };
                }
            });
        }
        if s.has_video() && section(ui, &mut reg, &mut ex.open_sections, "captions", tl!("Captions"), &t) {
            if !has_captions {
                ui.label(egui::RichText::new(tl!("The sequence has no caption track.")).color(t.text_dim).size(11.5));
            }
            row(ui, &t, tl!("Export Options"), |ui| {
                let modes = [tl!("None"), tl!("Burn Captions Into Video"), tl!("Create Sidecar File")];
                let cur = if s.burn_captions {
                    modes[1]
                } else if s.caption_sidecar.is_some() {
                    modes[2]
                } else {
                    modes[0]
                };
                if let Some(i) = combo(ui, &mut reg, "export.captions.mode", cur, &opts(&modes), 220.0) {
                    s.burn_captions = i == 1;
                    s.caption_sidecar = (i == 2).then(|| s.caption_sidecar.clone().unwrap_or_else(|| "srt".into()));
                }
            });
            if let Some(kind) = s.caption_sidecar.clone() {
                row(ui, &t, tl!("File Format"), |ui| {
                    let formats = [tl!("SubRip Subtitle (.srt)"), tl!("WebVTT (.vtt)")];
                    let cur = if kind == "vtt" { formats[1] } else { formats[0] };
                    if let Some(i) = combo(ui, &mut reg, "export.captions.format", cur, &opts(&formats), 220.0) {
                        s.caption_sidecar = Some(if i == 0 { "srt" } else { "vtt" }.into());
                    }
                });
            }
        }
        if section(ui, &mut reg, &mut ex.open_sections, "effects", tl!("Effects"), &t) {
            pick_overlay = effects_section(ui, &mut reg, s, &t);
        }
        if matches!(s.format, Format::H264 | Format::Hevc | Format::Av1 | Format::ProRes | Format::DnxHr | Format::Apv | Format::Mjpeg)
            && section(ui, &mut reg, &mut ex.open_sections, "metadata", tl!("Metadata"), &t)
        {
            let m = &mut s.metadata;
            for (key, label, v) in [
                ("title", tl!("Title"), &mut m.title),
                ("creator", tl!("Creator"), &mut m.creator),
                ("copyright", tl!("Copyright"), &mut m.copyright),
                ("description", tl!("Description"), &mut m.description),
                ("comment", tl!("Comment"), &mut m.comment),
            ] {
                row(ui, &t, label, |ui| text(ui, &mut reg, &format!("export.metadata.{key}"), v, ""));
            }
        }
        let _ = seq_rate;
    });
    if let Some(name) = chosen_preset {
        apply_preset(app, &name);
    }
    if open_manager {
        app.ui.export.manager = Some(PresetManager { selected: app.ui.export.preset.clone(), ..Default::default() });
    }
    if pick_folder {
        app.pick_ui(FileDialog::Folder { at: None }, |app, paths| {
            if let Some(d) = paths.into_iter().next() {
                app.ui.export.location = d;
            }
        });
    }
    if pick_overlay {
        app.pick_ui(FileDialog::open_file(tl!("Image"), &["png", "jpg", "jpeg"]), |app, paths| {
            if let Some(f) = paths.into_iter().next() {
                app.ui.export.settings.effects.image_overlay.path = f;
                app.ui.export.settings.effects.image_overlay.enabled = true;
            }
        });
    }
    reg.flush(app);
}

fn video_section(ui: &mut egui::Ui, reg: &mut Reg, s: &mut ExportSettings, t: &Tokens, seq_w: u32, seq_h: u32, seq_hdr: bool) {
    // frame size
    let mut match_size = s.frame_size.is_none();
    row(ui, t, tl!("Frame Size"), |ui| {
        if check(ui, reg, "export.video.matchSize", &mut match_size, tl!("Match Source")) {
            s.frame_size = if match_size { None } else { Some((seq_w, seq_h)) };
        }
    });
    if let Some((w, h)) = s.frame_size.as_mut() {
        row(ui, t, "", |ui| {
            let (mut fw, mut fh) = (*w as f64, *h as f64);
            drag(ui, reg, "export.video.width", &mut fw, 16.0..=8192.0, 1.0, "", 0);
            ui.label("×");
            drag(ui, reg, "export.video.height", &mut fh, 16.0..=8192.0, 1.0, "", 0);
            (*w, *h) = (fw as u32, fh as u32);
        });
    }
    // frame rate
    let mut match_rate = s.frame_rate.is_none();
    row(ui, t, tl!("Frame Rate"), |ui| {
        if check(ui, reg, "export.video.matchRate", &mut match_rate, tl!("Match Source")) {
            s.frame_rate = if match_rate { None } else { Some(FrameRate::FPS_29_97) };
        }
        if let Some(r) = s.frame_rate {
            let cur = RATES.iter().find(|(_, x)| *x == r).map(|(l, _)| l.to_string()).unwrap_or_else(|| r.label());
            let labels: Vec<(String, bool)> = RATES.iter().map(|(l, _)| (l.to_string(), true)).collect();
            if let Some(i) = combo(ui, reg, "export.video.fps", &cur, &labels, 90.0) {
                s.frame_rate = Some(RATES[i].1);
            }
        }
    });
    row(ui, t, tl!("Field Order"), |ui| {
        let o = [FieldOrder::Progressive, FieldOrder::UpperFirst, FieldOrder::LowerFirst];
        let labels: Vec<(String, bool)> = o.iter().map(|f| (f.label().to_string(), *f == FieldOrder::Progressive)).collect();
        if let Some(i) = combo(ui, reg, "export.video.fieldOrder", s.field_order.label(), &labels, 160.0) {
            s.field_order = o[i];
        }
    });
    row(ui, t, tl!("Aspect"), |ui| {
        let cur = PARS.iter().find(|(_, p)| *p == s.pixel_aspect).map(|(l, _)| l.to_string()).unwrap_or_else(|| tl!("Custom").into());
        let labels: Vec<(String, bool)> = PARS.iter().map(|(l, _)| (l.to_string(), true)).collect();
        if let Some(i) = combo(ui, reg, "export.video.aspect", &cur, &labels, 220.0) {
            s.pixel_aspect = PARS[i].1;
        }
    });
    if s.format.is_mxf() {
        row(ui, t, tl!("Video Codec"), |ui| {
            let labels: Vec<(String, bool)> = MxfVideoCodec::ALL.iter().map(|c| (c.label().to_string(), true)).collect();
            if let Some(i) = combo(ui, reg, "export.video.mxfCodec", s.mxf_video_codec.label(), &labels, 180.0) {
                s.mxf_video_codec = MxfVideoCodec::ALL[i];
            }
        });
    }
    if s.supports_alpha() {
        row(ui, t, tl!("Alpha"), |ui| {
            check(ui, reg, "export.video.alpha", &mut s.alpha, tl!("Include Alpha Channel"));
        });
    }
    match s.video_format() {
        f @ (Format::H264 | Format::Hevc | Format::Av1) => {
            // H.265 and AV1: hardware encoders only (one profile, level chosen by the encoder)
            let hevc = f != Format::H264;
            if hevc {
                if f == Format::Av1 {
                    row(ui, t, tl!("Profile"), |ui| ui.label(tl!("Main (8-bit)")));
                    row(ui, t, tl!("Encoder"), |ui| ui.label(tl!("Hardware (AV1 has no software encoder)")));
                } else {
                    // the hardware encoder writes Main (8-bit 4:2:0), or Main 10 for HDR sequences where it can; level chosen by the encoder
                    let label = match (filmcraft_engine::export::hdr_available(Format::Hevc), seq_hdr && !s.sdr) {
                        (true, true) => tl!("Main 10 (10-bit, HDR)"),
                        (true, false) => tl!("Main (8-bit; Main 10 for HDR sequences)"),
                        (false, _) => tl!("Main (8-bit)"),
                    };
                    row(ui, t, tl!("Profile"), |ui| ui.label(label));
                    row(ui, t, tl!("Encoder"), |ui| ui.label(tl!("Hardware (H.265 has no software encoder)")));
                }
            } else {
                row(ui, t, tl!("Profile"), |ui| {
                    let o = [H264Profile::Baseline, H264Profile::Main, H264Profile::High];
                    let labels: Vec<(String, bool)> = o.iter().map(|p| (p.label().to_string(), true)).collect();
                    if let Some(i) = combo(ui, reg, "export.video.profile", s.h264_profile.label(), &labels, 120.0) {
                        s.h264_profile = o[i];
                    }
                });
                row(ui, t, tl!("Level"), |ui| {
                    let levels: [Option<u8>; 13] =
                        [None, Some(30), Some(31), Some(32), Some(40), Some(41), Some(42), Some(50), Some(51), Some(52), Some(60), Some(61), Some(62)];
                    let lab = |l: &Option<u8>| l.map(|l| format!("{}.{}", l / 10, l % 10)).unwrap_or_else(|| tl!("Auto").into());
                    let labels: Vec<(String, bool)> = levels.iter().map(|l| (lab(l), true)).collect();
                    if let Some(i) = combo(ui, reg, "export.video.level", &lab(&s.h264_level), &labels, 120.0) {
                        s.h264_level = levels[i];
                    }
                });
            }
            row(ui, t, tl!("Bitrate Encoding"), |ui| {
                // a hardware encoder has one pass and no constant-quality mode: H.265 offers neither
                let o = [BitrateMode::Cbr, BitrateMode::Vbr1Pass, BitrateMode::Vbr2Pass, BitrateMode::Crf];
                let labels: Vec<(String, bool)> =
                    o.iter().map(|m| (m.label().to_string(), !(hevc && matches!(m, BitrateMode::Vbr2Pass | BitrateMode::Crf)))).collect();
                if let Some(i) = combo(ui, reg, "export.video.bitrateMode", s.bitrate_mode.label(), &labels, 140.0) {
                    s.bitrate_mode = o[i];
                }
            });
            if s.bitrate_mode == BitrateMode::Crf {
                row(ui, t, tl!("Quality (CRF)"), |ui| {
                    let mut crf = if s.crf.is_finite() { f64::from(s.crf) } else { f64::from(filmcraft_engine::export::DEFAULT_CRF) };
                    if drag(ui, reg, "export.video.crf", &mut crf, 0.0..=51.0, 0.1, "", 0) {
                        s.crf = crf.round() as f32;
                    }
                });
            } else if let Some(bpp) = s.adaptive_bitrate {
                row(ui, t, tl!("Target Bitrate"), |ui| {
                    ui.label(egui::RichText::new(tlf!("Adaptive ({bpp} bits per pixel)", bpp)).size(12.0));
                    let r = ui.small_button(tl!("Set"));
                    reg.add("export.video.fixedBitrate", r.rect, "Set a fixed bitrate");
                    if r.clicked() {
                        s.adaptive_bitrate = None;
                    }
                });
            } else {
                row(ui, t, tl!("Target Bitrate"), |ui| {
                    let mut mbps = s.bitrate_kbps as f64 / 1000.0;
                    if drag(ui, reg, "export.video.target", &mut mbps, 0.1..=800.0, 0.1, " Mbps", 1) {
                        s.bitrate_kbps = (mbps * 1000.0).round() as u32;
                    }
                });
                if s.bitrate_mode != BitrateMode::Cbr {
                    row(ui, t, tl!("Maximum Bitrate"), |ui| {
                        let mut mbps = s.max_bitrate_kbps.unwrap_or(s.bitrate_kbps * 3 / 2) as f64 / 1000.0;
                        if drag(ui, reg, "export.video.max", &mut mbps, 0.1..=1000.0, 0.1, " Mbps", 1) {
                            s.max_bitrate_kbps = Some((mbps * 1000.0).round() as u32);
                        }
                    });
                }
            }
            row(ui, t, tl!("Key Frame Distance"), |ui| {
                let mut on = s.keyframe_distance.is_some();
                if check(ui, reg, "export.video.keyframeOn", &mut on, "") {
                    s.keyframe_distance = on.then_some(48);
                }
                if let Some(k) = s.keyframe_distance.as_mut() {
                    let mut v = *k as f64;
                    drag(ui, reg, "export.video.keyframe", &mut v, 1.0..=600.0, 1.0, " frames", 0);
                    *k = v as u32;
                }
            });
            // The system's hardware encoder where there is one (VideoToolbox on macOS, NVENC on NVIDIA
            // GPUs on Windows, VA-API on Linux); everything it does not take (two-pass, HDR, MXF) and
            // every machine without one keeps the built-in encoder.
            // H.265 and AV1 have only hardware encoders: choosing the format is the opt-in.
            if !hevc {
                row(ui, t, "Hardware Encoding", |ui| {
                    let mut on = s.hardware_encoding == HardwareEncoding::Auto;
                    if check(ui, reg, "export.video.hardwareEncoding", &mut on, "Use the hardware encoder when available") {
                        s.hardware_encoding = if on { HardwareEncoding::Auto } else { HardwareEncoding::Off };
                    }
                });
            }
        }
        Format::ProRes => {
            row(ui, t, tl!("Profile"), |ui| {
                let keys = ["proxy", "lt", "standard", "hq", "4444", "4444xq"];
                let labels =
                    ["Apple ProRes 422 Proxy", "Apple ProRes 422 LT", "Apple ProRes 422", "Apple ProRes 422 HQ", "Apple ProRes 4444", "Apple ProRes 4444 XQ"];
                let cur = keys.iter().position(|k| *k == s.prores_profile).unwrap_or(3);
                if let Some(i) = combo(ui, reg, "export.video.proresProfile", labels[cur], &opts(&labels), 220.0) {
                    s.prores_profile = keys[i].into();
                }
            });
        }
        Format::DnxHr => {
            row(ui, t, tl!("Profile"), |ui| {
                let keys = ["lb", "sq", "hq", "hqx"];
                let labels = ["DNxHR LB", "DNxHR SQ", "DNxHR HQ", "DNxHR HQX (10-bit)"];
                let cur = keys.iter().position(|k| *k == s.dnx_profile).unwrap_or(2);
                if let Some(i) = combo(ui, reg, "export.video.dnxProfile", labels[cur], &opts(&labels), 220.0) {
                    s.dnx_profile = keys[i].into();
                }
            });
        }
        Format::Apv => {
            row(ui, t, tl!("Profile"), |ui| {
                let keys = ["422-10", "422-12", "444-10", "444-12"];
                let labels = ["APV 422-10", "APV 422-12", "APV 444-10", "APV 444-12"];
                let cur = keys.iter().position(|k| *k == s.apv_profile).unwrap_or(0);
                if let Some(i) = combo(ui, reg, "export.video.apvProfile", labels[cur], &opts(&labels), 220.0) {
                    s.apv_profile = keys[i].into();
                }
            });
        }
        Format::ExrSequence => {
            row(ui, t, tl!("Compression"), |ui| {
                // every mode is lossless; RLE and the ZIP variants shrink the files
                let label = |c: ExrCompression| match c {
                    ExrCompression::None => tl!("None"),
                    ExrCompression::Rle => "RLE",
                    ExrCompression::Zips => tl!("ZIP (1 scan line)"),
                    ExrCompression::Zip => tl!("ZIP (16 scan lines)"),
                };
                let labels: Vec<&str> = ExrCompression::ALL.iter().map(|&c| label(c)).collect();
                if let Some(i) = combo(ui, reg, "export.video.exrCompression", label(s.exr_compression), &opts(&labels), 220.0) {
                    s.exr_compression = ExrCompression::ALL[i];
                }
            });
        }
        Format::Mjpeg | Format::JpegSequence => {
            row(ui, t, tl!("Quality"), |ui| {
                let mut q = s.quality as f64;
                drag(ui, reg, "export.video.quality", &mut q, 1.0..=100.0, 1.0, "", 0);
                s.quality = q as u8;
            });
        }
        _ => {}
    }
    // Composite the picture on the GPU (filmcraft-gpu) instead of the CPU reference renderer;
    // Auto falls back to the CPU wherever the GPU cannot render the frame.
    row(ui, t, tl!("GPU Rendering"), |ui| {
        let mut on = s.gpu_rendering == GpuRendering::Auto;
        if check(ui, reg, "export.gpuRendering", &mut on, tl!("Composite on the GPU when available")) {
            s.gpu_rendering = if on { GpuRendering::Auto } else { GpuRendering::Off };
        }
    });
    row(ui, t, "", |ui| check(ui, reg, "export.video.maxDepth", &mut s.render_at_max_depth, tl!("Render at Maximum Depth")));
    row(ui, t, "", |ui| check(ui, reg, "export.video.maxQuality", &mut s.max_render_quality, tl!("Use Maximum Render Quality")));
}

fn audio_section(ui: &mut egui::Ui, reg: &mut Reg, s: &mut ExportSettings, t: &Tokens, seq_sr: u32) {
    let audio_only = !s.has_video();
    if !audio_only {
        row(ui, t, "", |ui| check(ui, reg, "export.audio.include", &mut s.include_audio, tl!("Export Audio")));
        if !s.include_audio {
            return;
        }
    }
    row(ui, t, tl!("Audio Format"), |ui| {
        let fixed = audio_only || s.format.is_mxf();
        let cur = match s.audio_codec() {
            AudioCodec::Aac => "AAC",
            AudioCodec::Flac => tl!("FLAC (lossless)"),
            _ => tl!("Uncompressed (PCM)"),
        };
        // MP4 takes AAC or FLAC; QuickTime any of the three
        let labels = vec![
            ("AAC".to_string(), !audio_only),
            (tl!("Uncompressed (PCM)").to_string(), !s.format.is_mp4_with(s.multiplexer)),
            (tl!("FLAC (lossless)").to_string(), true),
        ];
        if fixed {
            ui.label(cur);
        } else if let Some(i) = combo(ui, reg, "export.audio.codec", cur, &labels, 180.0) {
            s.audio.codec = [AudioCodec::Aac, AudioCodec::Pcm, AudioCodec::Flac][i.min(2)];
        }
    });
    row(ui, t, tl!("Sample Rate"), |ui| {
        let rates: [Option<u32>; 5] = [None, Some(32_000), Some(44_100), Some(48_000), Some(96_000)];
        let lab = |r: &Option<u32>| r.map(|r| format!("{r} Hz")).unwrap_or_else(|| tlf!("Match Source ({seq_sr} Hz)", seq_sr));
        let labels: Vec<(String, bool)> = rates.iter().map(|r| (lab(r), true)).collect();
        if let Some(i) = combo(ui, reg, "export.audio.sampleRate", &lab(&s.audio.sample_rate), &labels, 180.0) {
            s.audio.sample_rate = rates[i];
        }
    });
    row(ui, t, tl!("Channels"), |ui| {
        let names = [tl!("Mono"), tl!("Stereo"), "5.1"];
        let cur = match s.audio.channels {
            1 => names[0],
            6 => names[2],
            _ => names[1],
        };
        if let Some(i) = combo(ui, reg, "export.audio.channels", cur, &opts(&names), 120.0) {
            s.audio.channels = [1, 2, 6][i.min(2)];
        }
    });
    if s.audio_codec() == AudioCodec::Aac {
        row(ui, t, tl!("Audio Bitrate"), |ui| {
            let rates = [128u32, 160, 192, 256, 320];
            let labels: Vec<(String, bool)> = rates.iter().map(|r| (format!("{r} kbps"), true)).collect();
            if let Some(i) = combo(ui, reg, "export.audio.bitrate", &format!("{} kbps", s.audio.bitrate_kbps), &labels, 120.0) {
                s.audio.bitrate_kbps = rates[i];
            }
        });
    } else {
        row(ui, t, tl!("Sample Size"), |ui| {
            // every size, the ones this codec and container cannot store greyed out; a size the
            // audio format no longer takes (float after switching to FLAC) becomes 32-bit integer
            if !s.supports_sample_format(s.audio.sample_format()) {
                s.audio.set_sample_format(SampleFormat::Int32);
            }
            let sizes = [tl!("16 bit"), tl!("24 bit"), tl!("32 bit"), tl!("32 bit float")];
            let why = |f: SampleFormat| match (s.supports_sample_format(f), s.audio_codec()) {
                (true, _) => None,
                (false, AudioCodec::Flac) => Some(tl!("FLAC stores whole-number samples only")),
                (false, _) if s.format.is_mxf() => Some(tl!("MXF export writes integer PCM only")),
                (false, _) => Some(tl!("Not supported by FilmCraft's encoders yet")),
            };
            let labels: Vec<(String, Option<&str>)> = SampleFormat::ALL.iter().zip(sizes).map(|(&f, l)| (l.to_string(), why(f))).collect();
            let cur = SampleFormat::ALL.iter().position(|&f| f == s.audio.sample_format()).and_then(|i| sizes.get(i).copied()).unwrap_or(sizes[0]);
            if let Some(&f) = combo_why(ui, reg, "export.audio.bits", cur, &labels, 120.0).and_then(|i| SampleFormat::ALL.get(i)) {
                s.audio.set_sample_format(f);
            }
        });
    }
    if s.audio_codec() == AudioCodec::Flac {
        // every level is lossless: higher levels take longer for smaller files
        row(ui, t, tl!("Compression Level"), |ui| {
            let mut level = f64::from(s.audio.flac_level.min(8));
            if drag(ui, reg, "export.audio.flacLevel", &mut level, 0.0..=8.0, 0.05, "", 0) {
                s.audio.flac_level = level.round() as u8;
            }
            ui.label(egui::RichText::new(tl!("0 fastest, 8 smallest")).size(12.0));
        });
    }
}

fn placement_combo(ui: &mut egui::Ui, reg: &mut Reg, id: &str, p: &mut Placement) {
    let labels: Vec<(String, bool)> = Placement::ALL.iter().map(|x| (x.label().to_string(), true)).collect();
    if let Some(i) = combo(ui, reg, id, p.label(), &labels, 150.0) {
        *p = Placement::ALL[i];
    }
}

fn text_overlay(ui: &mut egui::Ui, reg: &mut Reg, key: &str, o: &mut TextOverlay, t: &Tokens, text_label: &str) {
    row(ui, t, text_label, |ui| text(ui, reg, &format!("export.effects.{key}.text"), &mut o.text, ""));
    row(ui, t, tl!("Position"), |ui| placement_combo(ui, reg, &format!("export.effects.{key}.position"), &mut o.placement));
    row(ui, t, tl!("Size / Opacity"), |ui| {
        let (mut sz, mut op) = (o.size_percent as f64, o.opacity as f64);
        drag(ui, reg, &format!("export.effects.{key}.size"), &mut sz, 1.0..=50.0, 0.2, " %", 1);
        drag(ui, reg, &format!("export.effects.{key}.opacity"), &mut op, 0.0..=100.0, 1.0, " %", 0);
        (o.size_percent, o.opacity) = (sz as f32, op as f32);
    });
}

/// Returns whether the image overlay's Browse… was clicked.
fn effects_section(ui: &mut egui::Ui, reg: &mut Reg, s: &mut ExportSettings, t: &Tokens) -> bool {
    let mut browse = false;
    let audio = s.has_audio() || !s.has_video();
    let video = s.has_video();
    let fx = &mut s.effects;
    if video {
        check(ui, reg, "export.effects.imageOverlay", &mut fx.image_overlay.enabled, tl!("Image Overlay"));
        if fx.image_overlay.enabled {
            let o = &mut fx.image_overlay;
            row(ui, t, tl!("Image"), |ui| {
                let r = ui.small_button(tl!("Browse…"));
                reg.add("export.effects.imageOverlay.browse", r.rect, "Browse…");
                browse = r.clicked();
                text(ui, reg, "export.effects.imageOverlay.path", &mut o.path, tl!("PNG or JPEG file"));
            });
            row(ui, t, tl!("Position"), |ui| placement_combo(ui, reg, "export.effects.imageOverlay.position", &mut o.placement));
            row(ui, t, tl!("Size / Opacity"), |ui| {
                let (mut sz, mut op) = (o.size_percent as f64, o.opacity as f64);
                drag(ui, reg, "export.effects.imageOverlay.size", &mut sz, 1.0..=100.0, 0.5, " %", 1);
                drag(ui, reg, "export.effects.imageOverlay.opacity", &mut op, 0.0..=100.0, 1.0, " %", 0);
                (o.size_percent, o.opacity) = (sz as f32, op as f32);
            });
        }
        check(ui, reg, "export.effects.nameOverlay", &mut fx.name_overlay.enabled, tl!("Name Overlay"));
        if fx.name_overlay.enabled {
            text_overlay(ui, reg, "nameOverlay", &mut fx.name_overlay, t, tl!("Name"));
        }
        check(ui, reg, "export.effects.timecodeOverlay", &mut fx.timecode_overlay.enabled, tl!("Timecode Overlay"));
        if fx.timecode_overlay.enabled {
            text_overlay(ui, reg, "timecodeOverlay", &mut fx.timecode_overlay, t, tl!("Prefix"));
        }
        check(ui, reg, "export.effects.videoLimiter", &mut fx.video_limiter.enabled, tl!("Video Limiter"));
        if fx.video_limiter.enabled {
            row(ui, t, tl!("Luma Min / Max"), |ui| {
                let (mut lo, mut hi) = (fx.video_limiter.min_percent as f64, fx.video_limiter.max_percent as f64);
                drag(ui, reg, "export.effects.videoLimiter.min", &mut lo, 0.0..=50.0, 0.5, " %", 1);
                drag(ui, reg, "export.effects.videoLimiter.max", &mut hi, 50.0..=100.0, 0.5, " %", 1);
                (fx.video_limiter.min_percent, fx.video_limiter.max_percent) = (lo as f32, hi as f32);
            });
        }
    }
    if audio {
        check(ui, reg, "export.effects.loudness", &mut fx.loudness.enabled, tl!("Loudness Normalization"));
        if fx.loudness.enabled {
            row(ui, t, tl!("Target Loudness"), |ui| {
                drag(ui, reg, "export.effects.loudness.target", &mut fx.loudness.target_lufs, -40.0..=-5.0, 0.1, " LUFS", 1);
            });
            row(ui, t, tl!("True Peak Limit"), |ui| {
                drag(ui, reg, "export.effects.loudness.truePeak", &mut fx.loudness.true_peak_dbtp, -9.0..=0.0, 0.1, " dBTP", 1);
            });
        }
    }
    browse
}
