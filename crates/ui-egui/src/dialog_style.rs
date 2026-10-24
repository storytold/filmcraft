//! Shared application-dialog chrome. All colours come from the active theme.
use crate::theme::Tokens;
use egui::{CornerRadius, Frame, Id, InnerResponse, Margin, Rect, RichText, Stroke, Ui, vec2};

/// Corner radius shared by application dialog frames.
pub const RADIUS: u8 = 6;

/// Dialog body using the active panel background and separator roles.
pub fn frame(t: &Tokens) -> Frame {
    Frame::new().fill(t.panel_bg).stroke(Stroke::new(1.0, t.separator)).corner_radius(RADIUS).inner_margin(0)
}

/// Header for blocking Modal containers and custom draggable dialogs such as Export Frame.
pub fn header(ui: &mut Ui, t: &Tokens, title: &str, width: f32) -> Rect {
    let (bar, _) = ui.allocate_exact_size(vec2(width, 30.0), egui::Sense::hover());
    ui.painter().rect_filled(bar, CornerRadius { nw: RADIUS, ne: RADIUS, sw: 0, se: 0 }, t.header_bg);
    ui.painter().text(bar.left_center() + vec2(10.0, 0.0), egui::Align2::LEFT_CENTER, title, Tokens::ui(13.0), t.text_dim);
    bar
}

/// Primary action using the chosen accent and a contrasting foreground.
pub fn primary(t: &Tokens, label: impl Into<String>) -> egui::Button<'static> {
    egui::Button::new(RichText::new(label.into()).color(t.accent_text())).fill(t.accent).min_size(vec2(88.0, 28.0)).corner_radius(14)
}

/// Secondary action using the active field and text roles.
pub fn secondary(t: &Tokens, label: impl Into<String>) -> egui::Button<'static> {
    egui::Button::new(RichText::new(label.into()).color(t.text))
        .fill(t.field_bg)
        .stroke(Stroke::new(1.0, t.field_border))
        .min_size(vec2(88.0, 28.0))
        .corner_radius(14)
}

/// Right-aligned actions in an explicit, consistent desktop order: the dismissive action is
/// rightmost (OK / Cancel, or Save / Don't Save / Cancel visually from left to right).
/// Callers submit Cancel first because the layout proceeds right to left. This also preserves
/// FilmCraft's established Export Frame order and the Windows Save Project convention.
pub fn actions<R>(ui: &mut Ui, contents: impl FnOnce(&mut Ui) -> R) -> InnerResponse<R> {
    ui.allocate_ui_with_layout(vec2(ui.available_width(), 30.0), egui::Layout::right_to_left(egui::Align::Center), contents)
}

/// egui Window builder using native title-frame painting and a dialog-local body frame.
/// It never changes Context styles; other windows, popups and panic recovery keep their styles.
pub struct Window<'a> {
    inner: egui::Window<'a>,
    frame: Option<Frame>,
    tokens: Tokens,
}
impl<'a> Window<'a> {
    pub fn new(t: &Tokens, title: impl Into<String>) -> Self {
        Self { inner: egui::Window::new(RichText::new(title.into()).color(t.text_dim).size(13.0)), frame: None, tokens: *t }
    }
    pub fn centered(mut self, ctx: &egui::Context) -> Self {
        use crate::widgets::CenteredDialog;
        self.inner = self.inner.centered(ctx);
        self
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
    /// Dialog body using the active panel background and separator roles.
    pub fn frame(mut self, value: Frame) -> Self {
        self.frame = Some(value);
        self
    }
    pub fn show<R>(self, ctx: &egui::Context, contents: impl FnOnce(&mut Ui) -> R) -> Option<InnerResponse<Option<R>>> {
        let t = self.tokens;
        let body =
            self.frame.unwrap_or_else(|| Frame::window(&ctx.global_style()).fill(t.panel_bg).stroke(Stroke::new(1.0, t.separator))).corner_radius(RADIUS);
        let title = Frame::new().fill(t.header_bg).corner_radius(RADIUS).inner_margin(Margin { left: 10, right: 10, top: 6, bottom: 6 });
        self.inner.frame(body).title_frame(title).show(ctx, contents)
    }
}
