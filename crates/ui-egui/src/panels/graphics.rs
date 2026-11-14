//! Graphics UI (M10.2): the Program-monitor overlay for graphic clips and the graphic sections of
//! the Properties / Essential Graphics panels.
//!
//! Monitor overlay:
//! - **Type tool (T):** click on empty picture → a new point-text layer (in the selected graphic
//!   clip under the playhead, else a new graphic clip) with a caret; drag on empty picture → a new
//!   paragraph-text layer whose box is the dragged rectangle; click on a text layer → caret at
//!   the click. Typing edits the text (one undo step per typing session); arrows, Home/End,
//!   Shift-selection, ⌘A, ⌘C/⌘X/⌘V, Return (new line) and Esc (stop editing) work as in a text
//!   field. Double-clicking a text layer with the Selection tool also edits it.
//! - **Selection tool:** click selects a layer (bounding box with handles and anchor point); drag
//!   moves it; drag the anchor point to move that alone. What a handle does depends on the layer
//!   (see `HandleDrag`): point text scales about its anchor point, a paragraph-text box is
//!   resized and its text re-wraps, a shape stretches away from its opposite side. A rectangle
//!   also has four small circles just inside its corners: dragging one rounds all four corners.
//! - **Rectangle / Ellipse tools:** drag out a shape. **Pen tool:** click points; click the first
//!   point again (or press Return) to close the path; Esc cancels.
//!
//! All edits go through `graphics.*` commands. Automation ids: `program.layer.<clip>.<layer>`,
//! `program.layer.<clip>.<layer>.handle.<n>`, `program.layer.<clip>.<layer>.anchor`,
//! `program.layer.<clip>.<layer>.corner.<n>`,
//! `program.textEdit`, `graphics.*` in the panels.

use crate::widgets::CenteredDialog as _;
use egui::ime_context::ImeTarget;
use egui::{Align2, Color32, Pos2, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use filmcraft_engine::Session;
use filmcraft_geom::{Affine, Vec2};
use filmcraft_project::graphic::{self, LayerContent, LayerSpec, eval_layer, layer_display_name, layer_indices};
use filmcraft_project::{ClipId, EffectInstance, ItemKind, ParamValue, TrackItem};
use filmcraft_render::graphic_clip::{layer_local_bounds, layer_matrix, text_layout};
use filmcraft_time::Tick;
use serde_json::{Value, json};

use crate::FilmcraftApp;
use crate::icons::{self, Icon};
use crate::state::{GfxEdit, TextPropsDialog, Tool};
use crate::theme::Tokens;
use crate::widgets::HotNumberOptions;

mod canvas;
mod handles;
mod ime;
mod layer_identity;
mod overlay;
mod properties;
mod text_edit;

pub use canvas::visible_layers;
pub(crate) use canvas::{frame_to_screen, unscale};
use canvas::{screen_scale, sp};
pub(crate) use handles::handle_points;
#[cfg(test)]
use ime::graphics_ime_target;
pub(crate) use ime::{DrawnImeEditor, current_reset_destination, finish_graphics_ime};
pub(crate) use layer_identity::current_layer_identity;
pub use overlay::monitor_overlay;
pub(crate) use overlay::{TEXT_EDIT_ID, deselect_on_monitor, select_all_edited_text};
pub use properties::{dialogs, properties};
#[cfg(test)]
use text_edit::{
    apply_surrounding_deletion, char_boundary_ceil, char_boundary_floor, clamp_edit_offsets, next_n_chars, prev_n_chars, surrounding_byte_deletion_ranges,
    text_edit,
};

/// A graphic layer visible at the playhead, with its layer→screen transform.
pub struct LayerView {
    pub clip: ClipId,
    pub layer: usize,
    pub spec: LayerSpec,
    pub to_screen: Affine,
    /// Graphic canvas → screen (the clip's Motion and the monitor fit).
    pub canvas_to_screen: Affine,
    pub local: [f32; 4],
}

impl LayerView {
    pub fn quad(&self) -> [Pos2; 4] {
        let b = self.local;
        [(b[0], b[1]), (b[2], b[1]), (b[2], b[3]), (b[0], b[3])].map(|(x, y)| sp(&self.to_screen, x, y))
    }
    fn to_local(&self, p: Pos2) -> Option<(f32, f32)> {
        let v = self.to_screen.inverse()?.apply(Vec2::new(p.x as f64, p.y as f64));
        Some((v.x as f32, v.y as f32))
    }
    pub(crate) fn hit(&self, p: Pos2) -> bool {
        let pad = 4.0 / screen_scale(&self.to_screen).max(1e-6);
        self.to_local(p).is_some_and(|(x, y)| x >= self.local[0] - pad && x <= self.local[2] + pad && y >= self.local[1] - pad && y <= self.local[3] + pad)
    }
    pub(crate) fn is_text(&self) -> bool {
        matches!(self.spec.content, LayerContent::Text(_))
    }
}

/// Whether a clip id is a graphic clip in the active sequence.
pub fn is_graphic_clip(app: &FilmcraftApp, c: ClipId) -> bool {
    app.session
        .active_sequence()
        .and_then(|q| q.find_item(c))
        .and_then(|(_, it)| app.session.project.item(it.item))
        .is_some_and(|p| matches!(p.kind, ItemKind::Graphic { .. }))
}

/// The selected graphic clip, if any.
pub fn selected_graphic(app: &FilmcraftApp) -> Option<(ClipId, TrackItem)> {
    let q = app.session.active_sequence()?;
    app.session.state.selection.iter().copied().find(|c| is_graphic_clip(app, *c)).and_then(|c| q.find_item(c).map(|(_, it)| (c, it.clone())))
}

/// Whether the Properties panel should show the graphic editor.
pub fn graphic_selected(app: &FilmcraftApp) -> bool {
    selected_graphic(app).is_some()
}

#[cfg(test)]
mod ime_byte_delete_tests {
    use super::{
        GfxEdit, apply_surrounding_deletion, char_boundary_ceil, char_boundary_floor, clamp_edit_offsets, graphics_ime_target, next_n_chars, prev_n_chars,
        surrounding_byte_deletion_ranges,
    };

    /// Floor/ceil byte offsets clamp to UTF-8 character boundaries, including offsets past the
    /// end and offsets inside a multi-byte character.
    #[test]
    fn byte_boundaries_never_split_a_code_point() {
        let s = "aé😀z"; // 1 + 2 + 4 + 1 bytes
        assert_eq!(char_boundary_floor(s, 2), 1);
        assert_eq!(char_boundary_ceil(s, 2), 3);
        assert_eq!(char_boundary_floor(s, 4), 3);
        assert_eq!(char_boundary_ceil(s, 4), 7);
        assert_eq!(char_boundary_floor(s, 999), s.len());
    }

    /// Byte-count surrounding deletion removes only whole characters: a partial é or emoji byte
    /// count deletes nothing, and the selection itself is always excluded.
    #[test]
    fn deleting_surrounding_bytes_leaves_partial_characters() {
        let s = "aé😀z"; // a=1, é=1..3, 😀=3..7, z=7..8
        // A cursor after "aé" (byte 3): one byte of "é" is not a whole character, so nothing is
        // deleted; two bytes are exactly "é".
        assert_eq!(surrounding_byte_deletion_ranges(s, 3, 3, 1, 0), (3..3, 3..3));
        assert_eq!(surrounding_byte_deletion_ranges(s, 3, 3, 2, 0), (1..3, 3..3));
        assert_eq!(&s[1..3], "é");
        // After the cursor: the 4-byte emoji is only deleted when all four bytes are reported.
        for partial in 1..4 {
            assert_eq!(surrounding_byte_deletion_ranges(s, 3, 3, 0, partial), (3..3, 3..3), "after_bytes={partial}");
        }
        assert_eq!(surrounding_byte_deletion_ranges(s, 3, 3, 0, 4), (3..3, 3..7));
        // The selection itself is excluded; only its surroundings are deleted.
        assert_eq!(surrounding_byte_deletion_ranges(s, 1, 3, 0, 0), (1..1, 3..3));
        assert_eq!(surrounding_byte_deletion_ranges(s, 1, 3, 1, 4), (0..1, 3..7));
    }

    /// Applying before/after surrounding deletions preserves a reversed selection's direction
    /// while shifting both endpoints by the preceding deletion.
    #[test]
    fn surrounding_deletion_keeps_reversed_selection_direction() {
        let mut text = "aé😀z".to_string();
        let mut ed = GfxEdit { clip: 1, layer: 0, caret: 3, anchor: 1, ..Default::default() };
        let (before, after) = surrounding_byte_deletion_ranges(&text, 1, 3, 1, 4);
        apply_surrounding_deletion(&mut text, &mut ed, before, after);
        assert_eq!(text, "éz");
        assert_eq!((ed.caret, ed.anchor), (2, 0), "primary caret stays after the anchor");
    }

    /// Hostile `gfxEdit` caret/anchor offsets inside multi-byte characters clamp to boundaries
    /// without panicking.
    #[test]
    fn hostile_edit_offsets_are_clamped_to_char_boundaries() {
        let s = "é😀";
        let mut ed = GfxEdit { clip: 1, layer: 0, caret: 1, anchor: 3, ..Default::default() };
        clamp_edit_offsets(s, &mut ed);
        assert_eq!((ed.caret, ed.anchor), (0, 2), "offsets inside the two characters move to boundaries");
        assert_eq!(s.get(ed.caret..ed.anchor), Some("é"));
    }

    /// Character-count deletion stops at both text ends and never splits a character, even for
    /// counts larger than the text.
    #[test]
    fn character_deletion_is_bounded() {
        let s = "aé😀z".to_string();
        assert_eq!(prev_n_chars(&s, s.len(), 99), 0);
        assert_eq!(next_n_chars(&s, 0, 2), "aé".len());
        assert_eq!(prev_n_chars(&s, 5, 1), 1);
        assert_eq!(next_n_chars(&s, 1, 1), 3);
    }

    /// The graphics IME owner prefers stable uid identity, falls back to the layer index only
    /// without a uid, and separates sequence and replacement epochs.
    #[test]
    fn ime_owner_uses_stable_component_identity_and_explicit_replacement() {
        let a = graphics_ime_target(7, 11, 3, 42, 5);
        let b = graphics_ime_target(7, 11, 9, 42, 5);
        assert_eq!(a, b, "the stable layer uid, not the mutable layer index, is the component");
        assert_ne!(a, graphics_ime_target(7, 11, 3, 42, 6), "replacement epoch is explicit");
        assert_ne!(a, graphics_ime_target(7, 11, 3, 43, 5), "a different component is a different owner");
        assert_ne!(a, graphics_ime_target(8, 11, 3, 42, 5), "sequence identity is part of the owner");
        let no_uid = graphics_ime_target(7, 11, 3, 0, 5);
        assert_ne!(no_uid, graphics_ime_target(7, 11, 4, 0, 5), "without a layer uid, the index is the identity");
    }

    /// A pointer-down starts a new target, so later Text, Paste and IME events in the same raw
    /// batch must not be applied to the layer that was being edited before the click. The old loop
    /// cleared `custom_mutations` here; the extracted helper once fell through to `Applied` and
    /// typed every later event into the old layer.
    #[test]
    fn pointer_press_rejects_text_paste_and_ime_later_in_the_same_raw_batch() {
        use filmcraft_geom::{Affine, Vec2};
        use filmcraft_project::ClipId;
        use filmcraft_project::graphic::{Appearance, LayerContent, LayerSpec, LayerTransform, TextProps};

        let mut app = crate::FilmcraftApp::new(filmcraft_engine::Session::default());
        let ed = GfxEdit { clip: 1, layer: 0, caret: 3, anchor: 3, ..Default::default() };
        app.ui.gfx_edit = Some(ed.clone());
        let props = TextProps {
            text: "old".into(),
            font: String::new(),
            style: String::new(),
            size: 72.0,
            align: 0,
            tracking: 0.0,
            kerning: true,
            ligatures: true,
            leading: 0.0,
            baseline_shift: 0.0,
            faux_bold: false,
            faux_italic: false,
            caps: 0,
            underline: false,
            strikethrough: false,
            box_width: 0.0,
            box_height: 0.0,
            vertical: false,
            runs: Vec::new(),
        };
        let view = super::LayerView {
            clip: ClipId(1),
            layer: 0,
            spec: LayerSpec {
                name: "Text".into(),
                enabled: true,
                uid: 0,
                pin: None,
                content: LayerContent::Text(props),
                appearance: Appearance { fill: None, gradient: None, strokes: Vec::new(), background: None, shadow: None },
                transform: LayerTransform { position: Vec2::ZERO, anchor: Vec2::ZERO, scale: Vec2::new(1.0, 1.0), rotation: 0.0, opacity: 1.0 },
            },
            to_screen: Affine::IDENTITY,
            canvas_to_screen: Affine::IDENTITY,
            local: [-50.0, -25.0, 50.0, 25.0],
        };

        let ctx = egui::Context::default();
        let raw = egui::RawInput {
            focused: true,
            events: vec![
                egui::Event::PointerButton {
                    pos: egui::pos2(20.0, 20.0),
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
                egui::Event::Text("typed".into()),
                egui::Event::Paste("pasted".into()),
                egui::Event::Ime(egui::ImeEvent::Commit("committed".into())),
            ],
            ..Default::default()
        };
        let mut focused = false;
        let mut out = ctx.run_ui(raw, |ui| {
            let response = ui.allocate_response(egui::vec2(100.0, 100.0), egui::Sense::click());
            super::text_edit(&mut app, ui, &view, ed.clone(), &response);
            focused = ui.memory(|m| m.has_focus(egui::Id::unique(super::TEXT_EDIT_ID)));
        });
        out.textures_delta.clear();

        assert!(focused, "the text editor must own input for this batch");
        // the session has no such clip: a `graphics.setText` would have failed into the status
        assert!(app.ui.status.is_empty(), "the pointer press must stop Text, Paste and IME editing the old layer: {}", app.ui.status);
    }
}
