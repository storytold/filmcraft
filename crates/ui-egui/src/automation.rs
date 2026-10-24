//! Automation registry: every interactive element registers a stable id and its screen rect each
//! frame, so agents can click/drag by id (`ui.click {id:"timeline.track.V1.lock"}`) and
//! `ui.inspect` can report the full on-screen widget tree.

use egui::Rect;
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub struct Element {
    pub id: String,
    pub label: String,
    pub rect: [f32; 4],
    /// Present only when this entry represents a real egui widget, rather than a click alias.
    #[serde(rename = "widgetId", skip_serializing_if = "Option::is_none")]
    pub widget_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focusable: Option<bool>,
    #[serde(skip)]
    pub(crate) response_id: Option<egui::Id>,
}

#[derive(Default)]
pub struct Registry {
    pub elements: Vec<Element>,
    /// Last frame's elements (for lookups from control handlers between frames).
    pub previous: Vec<Element>,
    /// Last frame's coverage of interactive widgets by elements (`ui.coverage`), from
    /// [`Registry::end_frame`].
    pub coverage: Coverage,
}

impl Registry {
    pub fn add(&mut self, id: &str, rect: Rect, label: &str) {
        self.elements.push(Element {
            id: id.to_string(),
            label: label.to_string(),
            rect: [rect.min.x, rect.min.y, rect.width(), rect.height()],
            widget_id: None,
            enabled: None,
            focusable: None,
            response_id: None,
        });
    }
    /// Observe the widget without changing its accessibility role or input behavior.
    pub fn add_response(&mut self, id: &str, response: &egui::Response, label: &str) {
        self.add(id, response.rect, label);
        if let Some(element) = self.elements.last_mut() {
            element.widget_id = Some(format!("{:016x}", response.id.value()));
            element.enabled = Some(response.enabled());
            element.focusable = Some(response.sense.is_focusable());
            element.response_id = Some(response.id);
        }
    }
    /// [`Registry::add_response`] for a widget in a scrolling list: only the part a pointer can
    /// reach is registered (a click goes there), and nothing when it is scrolled out of view.
    pub fn add_visible(&mut self, id: &str, response: &egui::Response, label: &str) {
        let reach = response.interact_rect;
        if reach.width() <= 0.0 || reach.height() <= 0.0 {
            return;
        }
        self.add_response(id, response, label);
        if let Some(element) = self.elements.last_mut() {
            element.rect = [reach.min.x, reach.min.y, reach.width(), reach.height()];
        }
    }
    pub fn begin_frame(&mut self) {
        self.previous = std::mem::take(&mut self.elements);
    }
    pub fn find(&self, id: &str) -> Option<&Element> {
        self.previous.iter().chain(self.elements.iter()).find(|e| e.id == id)
    }
    /// Elements whose id starts with a prefix.
    pub fn query(&self, prefix: &str) -> Vec<&Element> {
        self.previous.iter().filter(|e| e.id.starts_with(prefix)).collect()
    }
}

/// An interactive widget on screen that no element covers (`ui.coverage`): an agent can reach it
/// only by coordinates.
#[derive(Clone, Debug, Serialize)]
pub struct Uncovered {
    #[serde(rename = "widgetId")]
    pub widget_id: String,
    /// The accessibility role egui gave it (`Button`, `CheckBox`, `SpinButton`, `TextInput`…), or
    /// `Unknown` for a custom-drawn widget that gave none.
    pub role: String,
    /// Its text, or the text label beside it.
    pub label: String,
    pub rect: [f32; 4],
    /// What it responds to: `click`, `drag`, `click+drag` or `focus`.
    pub sense: String,
    pub enabled: bool,
}

/// What `ui.coverage` reports about the last frame's interactive widgets.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Coverage {
    /// Widgets on screen that sense a click, a drag or focus (selectable text is not counted).
    pub interactive: usize,
    /// Of those, the ones an element covers.
    pub covered: usize,
    /// Of the covered ones, those named from the accessibility tree (`auto.…` ids) rather than
    /// registered by their panel.
    pub auto: usize,
    /// Controls with no element: the gaps.
    pub uncovered: Vec<Uncovered>,
    /// Drag-only areas with no role: scroll areas and resize handles, which agents drive with
    /// `ui.scroll`, `ui.drag` or layout commands rather than by id.
    pub background: Vec<Uncovered>,
}

/// One widget laid out this frame and what the accessibility tree says about it.
struct Seen {
    id: egui::Id,
    /// Where it was laid out, and the part of that a pointer can reach (clipped to its panel and
    /// the screen; nothing when scrolled out of view).
    rect: Rect,
    reach: Rect,
    sense: egui::Sense,
    enabled: bool,
    popup: bool,
    role: Option<egui::accesskit::Role>,
    label: String,
    window: Option<String>,
}

/// `s` as an id segment: lower case letters and digits joined by `-`, at most 40 characters.
fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if out.chars().count() >= 40 {
            break;
        }
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

fn rect_of(e: &Element) -> Rect {
    Rect::from_min_size(egui::pos2(e.rect[0], e.rect[1]), egui::vec2(e.rect[2], e.rect[3]))
}

/// The text label left of `r` on the same row (Effect Controls' "Position" for its value fields).
fn label_beside(texts: &[(Rect, String)], r: Rect) -> Option<String> {
    texts
        .iter()
        .filter(|(t, _)| t.max.x <= r.min.x + 2.0 && r.min.x - t.max.x < 320.0 && (t.center().y - r.center().y).abs() < r.height().max(t.height()) * 0.6)
        .min_by(|a, b| (r.min.x - a.0.max.x).total_cmp(&(r.min.x - b.0.max.x)))
        .map(|(_, s)| s.clone())
}

impl Registry {
    /// At the end of a frame, give each control drawn this frame that registered no element of its
    /// own one named from the accessibility tree: `auto.<scope>.<role>[.<label>][.<widget>]`, where the
    /// scope is the dialog, the panel (`effectControls`) or `popup`, and the label is the
    /// control's text or the text beside it. Then record coverage for `ui.coverage`. Needs AccessKit
    /// (the control channel turns it on): without it nothing is named, and every control with no
    /// element of its own counts as uncovered.
    pub fn end_frame(&mut self, ctx: &egui::Context) {
        use egui::accesskit::Role;
        let screen = ctx.content_rect();
        let seen: Vec<Seen> = ctx.viewport(|v| {
            let ak = v.this_pass.accesskit_state.as_ref();
            let window_of = |id: egui::Id| -> Option<String> {
                let s = ak?;
                let mut cur = id;
                for _ in 0..64 {
                    let parent = *s.parent_map.get(&cur)?;
                    if let Some(n) = s.nodes.get(&parent)
                        && matches!(n.role(), Role::Window | Role::Dialog | Role::AlertDialog)
                        && let Some(title) = n.label()
                    {
                        return Some(title.to_string());
                    }
                    cur = parent;
                }
                None
            };
            v.this_pass
                .widgets
                .layers()
                .flat_map(|(layer, rects)| rects.iter().map(move |w| (*layer, *w)))
                .map(|(layer, w)| {
                    let node = ak.and_then(|s| s.nodes.get(&w.id));
                    Seen {
                        id: w.id,
                        rect: w.rect,
                        reach: w.interact_rect.intersect(screen),
                        sense: w.sense,
                        enabled: w.enabled,
                        popup: matches!(layer.order, egui::Order::Foreground | egui::Order::Tooltip),
                        role: node.map(|n| n.role()),
                        label: node.and_then(|n| n.label().or_else(|| n.value())).map(str::trim).unwrap_or_default().to_string(),
                        window: window_of(w.id),
                    }
                })
                .collect()
        });
        // text on screen, for controls with none of their own
        let texts: Vec<(Rect, String)> =
            seen.iter().filter(|w| w.role == Some(Role::Label) && !w.label.is_empty() && w.reach.width() > 0.0).map(|w| (w.reach, w.label.clone())).collect();
        let panels: Vec<(String, Rect)> = self
            .elements
            .iter()
            .filter_map(|e| {
                let id = e.id.strip_prefix("panel.").filter(|id| !id.contains('.'))?;
                let mut chars = id.chars();
                let first = chars.next()?.to_ascii_lowercase();
                Some((std::iter::once(first).chain(chars).collect(), rect_of(e)))
            })
            .collect();
        let mut taken: std::collections::HashSet<String> = self.elements.iter().map(|e| e.id.clone()).collect();
        let mut done = std::collections::HashSet::new();
        let mut added = Vec::new();
        let mut out = Coverage::default();
        for w in &seen {
            let (click, drag, focus) = (w.sense.senses_click(), w.sense.senses_drag(), w.sense.is_focusable());
            let selectable_text = w.role == Some(Role::Label) && drag;
            if !(click || drag || focus) || selectable_text || w.reach.width() < 2.0 || w.reach.height() < 2.0 || !done.insert(w.id) {
                continue;
            }
            out.interactive += 1;
            if self.elements.iter().any(|e| covers(e, w.id, w.rect) || covers(e, w.id, w.reach)) {
                out.covered += 1;
                continue;
            }
            let role = w.role.filter(|r| !matches!(r, Role::Unknown | Role::GenericContainer));
            // a field's own text is its value, which changes as it is edited: such a control is
            // named after the label beside it, and its label shows both
            let value_like = role.is_some_and(|r| {
                matches!(r, Role::SpinButton | Role::TextInput | Role::MultilineTextInput | Role::SearchInput | Role::ComboBox | Role::Slider)
            });
            let beside = label_beside(&texts, w.reach).unwrap_or_default();
            let name = if value_like || slug(&w.label).is_empty() { beside.clone() } else { w.label.clone() };
            let label = match (value_like, w.label.is_empty(), beside.is_empty()) {
                (_, true, _) => name.clone(),
                (true, false, false) => format!("{beside}: {}", w.label),
                _ => w.label.clone(),
            };
            let sense = match (click, drag) {
                (true, true) => "click+drag",
                (true, false) => "click",
                (false, true) => "drag",
                (false, false) => "focus",
            };
            // a custom-drawn control with no role is named only when it is clicked and has a label
            let unnamed_ok = click && !name.is_empty();
            let Some(role) = role.or(if unnamed_ok { Some(Role::Unknown) } else { None }) else {
                let u = Uncovered {
                    widget_id: format!("{:016x}", w.id.value()),
                    role: "Unknown".into(),
                    label,
                    rect: [w.reach.min.x, w.reach.min.y, w.reach.width(), w.reach.height()],
                    sense: sense.into(),
                    enabled: w.enabled,
                };
                // drag-only areas, and the title bars a dialog is moved by
                if (drag && !click) || (drag && w.window.is_some()) {
                    out.background.push(u);
                } else {
                    out.uncovered.push(u);
                }
                continue;
            };
            let scope = match &w.window {
                Some(title) => format!("dialog.{}", slug(title)),
                None if w.popup => "popup".to_string(),
                None => panels.iter().find(|(_, r)| r.contains(w.reach.center())).map_or_else(|| "app".to_string(), |(p, _)| p.clone()),
            };
            let role_slug = format!("{role:?}").to_ascii_lowercase();
            let base = match slug(&name) {
                l if l.is_empty() => format!("auto.{scope}.{role_slug}"),
                l => format!("auto.{scope}.{role_slug}.{l}"),
            };
            added.push(Element {
                id: base,
                label,
                rect: [w.reach.min.x, w.reach.min.y, w.reach.width(), w.reach.height()],
                widget_id: Some(format!("{:016x}", w.id.value())),
                enabled: Some(w.enabled),
                focusable: Some(focus),
                response_id: Some(w.id),
            });
            out.covered += 1;
            out.auto += 1;
        }
        // A name several controls share (or a panel already uses) gets each control's own widget
        // id as a suffix, not its position: an id never moves to another control when one before
        // it goes away.
        let mut uses: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for e in &added {
            *uses.entry(e.id.clone()).or_default() += 1;
        }
        for e in &mut added {
            if uses.get(&e.id).copied().unwrap_or(0) > 1 || taken.contains(&e.id) {
                let widget = e.response_id.map_or(0, |id| id.value() & 0xffff_ffff);
                e.id = format!("{}.{widget:08x}", e.id);
            }
            let base = e.id.clone();
            let mut n = 1;
            while !taken.insert(e.id.clone()) {
                n += 1;
                e.id = format!("{base}.{n}");
            }
        }
        self.elements.extend(added);
        self.coverage = out;
    }
}

/// Whether `e` stands for the widget with `id` at `rect`: registered for its response, or drawn over
/// it (its rect holds the widget's centre and is not much larger than it, so a whole-panel element
/// does not hide the controls inside it).
fn covers(e: &Element, id: egui::Id, rect: Rect) -> bool {
    if e.response_id == Some(id) {
        return true;
    }
    let r = Rect::from_min_size(egui::pos2(e.rect[0], e.rect[1]), egui::vec2(e.rect[2], e.rect[3]));
    r.expand(1.0).contains(rect.center()) && r.area() <= rect.area() * 4.0 + 1600.0
}
