//! Shared application-dialog chrome and actions. Native file pickers remain host-owned.
use crate::theme::Tokens;
use egui::{Color32, CornerRadius, Frame, Id, InnerResponse, Margin, Rect, RichText, Stroke, Ui, vec2};
use std::sync::Arc;

pub const RADIUS: u8 = 6;
pub const HEADER_BG: Color32 = Color32::WHITE;
pub const HEADER_TEXT: Color32 = Color32::from_gray(90);
pub const PRIMARY_BG: Color32 = Color32::from_rgb(0x2f, 0x6b, 0xdf);

pub fn frame(t: &Tokens) -> Frame {
    Frame::new().fill(t.panel_bg).stroke(Stroke::new(1.0, t.separator)).corner_radius(RADIUS).inner_margin(0)
}

/// Header for blocking Modal containers and custom draggable dialogs such as Export Frame.
pub fn header(ui: &mut Ui, title: &str, width: f32) -> Rect {
    let (bar, _) = ui.allocate_exact_size(vec2(width, 30.0), egui::Sense::hover());
    ui.painter().rect_filled(bar, CornerRadius { nw: RADIUS, ne: RADIUS, sw: 0, se: 0 }, HEADER_BG);
    ui.painter().text(bar.left_center() + vec2(10.0, 0.0), egui::Align2::LEFT_CENTER, title, Tokens::ui(13.0), HEADER_TEXT);
    bar
}

pub fn primary(label: impl Into<String>) -> egui::Button<'static> {
    egui::Button::new(RichText::new(label.into()).color(Color32::WHITE)).fill(PRIMARY_BG).min_size(vec2(88.0, 28.0)).corner_radius(14)
}

pub fn secondary(label: impl Into<String>) -> egui::Button<'static> {
    egui::Button::new(label.into()).min_size(vec2(88.0, 28.0)).corner_radius(14)
}

/// Right-aligned actions; callers list Cancel first, then the primary action.
pub fn actions<R>(ui: &mut Ui, contents: impl FnOnce(&mut Ui) -> R) -> InnerResponse<R> {
    ui.allocate_ui_with_layout(vec2(ui.available_width(), 30.0), egui::Layout::right_to_left(egui::Align::Center), contents)
}

/// Restore context style even if a dialog body unwinds into the app's crash guard.
struct StyleScope {
    ctx: egui::Context,
    original: Arc<egui::Style>,
    applied: Arc<egui::Style>,
}
impl Drop for StyleScope {
    fn drop(&mut self) {
        if Arc::ptr_eq(&self.ctx.global_style(), &self.applied) {
            self.ctx.set_global_style(self.original.clone());
        }
    }
}

/// egui Window builder with the shared dialog header. Movement, sizing and close behavior stay native.
pub struct Window<'a> {
    inner: egui::Window<'a>,
    frame: Option<Frame>,
}
impl<'a> Window<'a> {
    pub fn new(title: impl Into<String>) -> Self {
        Self { inner: egui::Window::new(RichText::new(title.into()).color(HEADER_TEXT).size(13.0)), frame: None }
    }
    pub fn id(mut self, id: Id) -> Self {
        self.inner = self.inner.id(id);
        self
    }
    pub fn open(mut self, open: &'a mut bool) -> Self {
        self.inner = self.inner.open(open);
        self
    }
    pub fn collapsible(mut self, value: bool) -> Self {
        self.inner = self.inner.collapsible(value);
        self
    }
    pub fn resizable(mut self, value: impl Into<egui::Vec2b>) -> Self {
        self.inner = self.inner.resizable(value);
        self
    }
    pub fn anchor(mut self, align: egui::Align2, offset: impl Into<egui::Vec2>) -> Self {
        self.inner = self.inner.anchor(align, offset);
        self
    }
    pub fn pivot(mut self, value: egui::Align2) -> Self {
        self.inner = self.inner.pivot(value);
        self
    }
    pub fn default_pos(mut self, value: impl Into<egui::Pos2>) -> Self {
        self.inner = self.inner.default_pos(value);
        self
    }
    pub fn default_width(mut self, value: f32) -> Self {
        self.inner = self.inner.default_width(value);
        self
    }
    pub fn default_height(mut self, value: f32) -> Self {
        self.inner = self.inner.default_height(value);
        self
    }
    pub fn default_size(mut self, value: impl Into<egui::Vec2>) -> Self {
        self.inner = self.inner.default_size(value);
        self
    }
    pub fn fixed_size(mut self, value: impl Into<egui::Vec2>) -> Self {
        self.inner = self.inner.fixed_size(value);
        self
    }
    pub fn min_width(mut self, value: f32) -> Self {
        self.inner = self.inner.min_width(value);
        self
    }
    pub fn min_height(mut self, value: f32) -> Self {
        self.inner = self.inner.min_height(value);
        self
    }
    pub fn max_width(mut self, value: f32) -> Self {
        self.inner = self.inner.max_width(value);
        self
    }
    pub fn max_height(mut self, value: f32) -> Self {
        self.inner = self.inner.max_height(value);
        self
    }
    pub fn vscroll(mut self, value: bool) -> Self {
        self.inner = self.inner.vscroll(value);
        self
    }
    pub fn hscroll(mut self, value: bool) -> Self {
        self.inner = self.inner.hscroll(value);
        self
    }
    pub fn frame(mut self, value: Frame) -> Self {
        self.frame = Some(value);
        self
    }
    pub fn show<R>(self, ctx: &egui::Context, contents: impl FnOnce(&mut Ui) -> R) -> Option<InnerResponse<Option<R>>> {
        let original = ctx.global_style();
        let mut title_style = (*original).clone();
        title_style.visuals.widgets.open.weak_bg_fill = HEADER_BG;
        title_style.visuals.override_text_color = Some(HEADER_TEXT);
        for widget in [&mut title_style.visuals.widgets.inactive, &mut title_style.visuals.widgets.hovered, &mut title_style.visuals.widgets.active] {
            widget.fg_stroke.color = HEADER_TEXT;
        }
        ctx.set_global_style(title_style);
        let _scope = StyleScope { ctx: ctx.clone(), original: original.clone(), applied: ctx.global_style() };
        let body = self.frame.unwrap_or_else(|| Frame::window(&original)).corner_radius(RADIUS);
        let title = Frame::new().fill(HEADER_BG).corner_radius(RADIUS).inner_margin(Margin { left: 10, right: 10, top: 6, bottom: 6 });
        self.inner.frame(body).title_frame(title).show(ctx, |ui| {
            ctx.set_global_style(original.clone());
            ui.set_style(original);
            contents(ui)
        })
    }
}
