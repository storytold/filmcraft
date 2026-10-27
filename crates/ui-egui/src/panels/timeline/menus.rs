use super::*;

/// The clip context menu: groups (separated by rules) of (label, command id). Entries marked `…`
/// open their dialog through `menus::invoke`, like the same item in the Clip menu.
/// The menu of a right-clicked Timeline marker (automation ids `timeline.markerMenu.<command>`);
/// the marker is under the playhead, where Clear Selected Marker looks for it (#695).
pub(super) fn marker_menu(app: &mut FilmcraftApp, ctx: &egui::Context, ui: &mut egui::Ui, marker: MarkerId) {
    let Some(marker) = app.session.active_sequence().and_then(|q| q.markers.iter().find(|m| m.id == marker)).cloned() else {
        ui.close();
        return;
    };
    crate::panels::misc::marker_label_menu(app, ui, &marker, "timeline.markerMenu");
    ui.separator();
    for (label, cmd) in [("Clear Selected Marker", "markers.clearCurrent"), ("Clear Markers", "markers.clearAll")] {
        let label = crate::i18n::t(label);
        let r = ui.button(label);
        app.auto.add(&format!("timeline.markerMenu.{cmd}"), r.rect, label);
        if r.clicked() {
            if let Err(e) = crate::menus::invoke(app, ctx, cmd, json!({"target":"program"})) {
                app.ui.status = e;
            }
            ui.close();
        }
    }
}

pub(crate) const CLIP_MENU: &[&[(&str, &str)]] = &[
    &[
        ("Cut", "edit.cut"),
        ("Copy", "edit.copy"),
        ("Paste Attributes…", "edit.pasteAttributes"),
        ("Remove Attributes…", "edit.removeAttributes"),
        ("Clear", "edit.clear"),
        ("Ripple Delete", "edit.rippleDelete"),
    ],
    &[("Edit Original", "edit.editOriginal"), ("Replace With Clip From Source Monitor", "clip.replaceFromSource")],
    &[
        ("Enable", "clip.enable"),
        ("Link", "clip.link"),
        ("Group", "clip.group"),
        ("Ungroup", "clip.ungroup"),
        ("Synchronize…", "clip.synchronize"),
        ("Merge Clips…", "clip.mergeClips"),
        ("Nest…", "clip.nest"),
        ("Make Subsequence", "sequence.makeSubsequence"),
        ("Reveal Nested Sequence", "sequence.revealNested"),
        ("Multi-Camera", "clip.multicam"),
    ],
    &[("Label", "edit.label")],
    &[("Add Marker", "markers.add")],
    &[("Speed/Duration…", "clip.speedDuration"), ("Scene Edit Detection…", "clip.sceneEditDetection")],
    &[
        ("Frame Hold Options…", "clip.frameHoldOptions"),
        ("Add Frame Hold", "clip.frameHold"),
        ("Insert Frame Hold Segment", "clip.insertFrameHoldSegment"),
        ("Field Options…", "clip.fieldOptions"),
        ("Scale to Frame Size", "clip.scaleToFrameSize"),
        ("Fit to frame", "clip.fitToFrame"),
        ("Fill frame", "clip.fillFrame"),
    ],
    &[("Reveal in Project", "clip.revealInProject"), ("Join Through Edits", "sequence.joinThroughEdits")],
];

/// The cut right-clicked on a track (#219): the clip ending there and the clip starting there,
/// either missing at the ends of a gap. Its menu offers the edit point's trim types, Apply Default
/// Transitions and Join Through Edits, like Premiere's edit point menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct EditPointTarget {
    pub(super) left: Option<ClipId>,
    pub(super) right: Option<ClipId>,
}

impl EditPointTarget {
    pub(super) fn at(seq: &Sequence, track: TrackId, clip: ClipId, edge: filmcraft_edit::Edge) -> Option<Self> {
        let tr = seq.track(track)?;
        let it = tr.item(clip)?;
        Some(match edge {
            filmcraft_edit::Edge::Out => Self { left: Some(clip), right: tr.items.iter().find(|x| x.start == it.end()).map(|x| x.id) },
            filmcraft_edit::Edge::In => Self { left: tr.items.iter().find(|x| x.end() == it.start).map(|x| x.id), right: Some(clip) },
        })
    }
}

/// The edit point menu's trim types: label, id (`timeline.editPointMenu.<id>`), trim kind, and
/// which side of the cut it selects (the incoming clip's In or the outgoing clip's Out).
pub(crate) const EDIT_POINT_TYPES: &[(&str, &str, &str, filmcraft_edit::Edge)] = &[
    ("Ripple Trim In", "rippleIn", "ripple", filmcraft_edit::Edge::In),
    ("Ripple Trim Out", "rippleOut", "ripple", filmcraft_edit::Edge::Out),
    ("Roll Edit", "roll", "roll", filmcraft_edit::Edge::Out),
    ("Trim In", "trimIn", "trim", filmcraft_edit::Edge::In),
    ("Trim Out", "trimOut", "trim", filmcraft_edit::Edge::Out),
];

/// The edit point context menu (#219).
pub(super) fn edit_point_menu(app: &mut FilmcraftApp, ctx: &egui::Context, ui: &mut egui::Ui, seq: &Sequence, target: EditPointTarget) {
    let selected = app.session.state.edit_points.first().copied();
    for &(label, id, kind, edge) in EDIT_POINT_TYPES {
        let clip = match edge {
            filmcraft_edit::Edge::In => target.right,
            filmcraft_edit::Edge::Out => target.left,
        };
        // a roll needs a clip on both sides of the cut
        let clip = clip.filter(|_| kind != "roll" || (target.left.is_some() && target.right.is_some()));
        let out = edge == filmcraft_edit::Edge::Out;
        let current = selected.is_some_and(|ep| Some(ep.clip) == clip && ep.out == out && serde_json::to_value(ep.kind).ok() == Some(json!(kind)));
        let label = crate::i18n::t(label);
        let text = if current { format!("✓ {label}") } else { label.to_string() };
        let r = ui.add_enabled(clip.is_some(), egui::Button::new(text));
        app.auto.add(&format!("timeline.editPointMenu.{id}"), r.rect, label);
        if r.clicked()
            && let Some(c) = clip
        {
            let e = if out { "out" } else { "in" };
            if let Err(e) = app.session.execute("trim.selectEditPoint", json!({"clip": c.0, "edge": e, "kind": kind})) {
                app.ui.status = e.to_string();
            }
            ui.close();
        }
    }
    ui.separator();
    let label = tl!("Apply Default Transitions");
    let r = ui.add_enabled(app.session.is_enabled("trim.applyDefaultTransition"), egui::Button::new(label));
    app.auto.add("timeline.editPointMenu.applyDefaultTransitions", r.rect, label);
    if r.clicked() {
        if let Err(e) = crate::menus::invoke(app, ctx, "trim.applyDefaultTransition", json!({})) {
            app.ui.status = e;
        }
        ui.close();
    }
    // only this cut (and its linked partners), and only when it is a through edit
    let through = filmcraft_edit::through::through_edits(seq).iter().any(|e| Some(e.left) == target.left && Some(e.right) == target.right);
    let label = tl!("Join Through Edits");
    let r = ui.add_enabled(through, egui::Button::new(label));
    app.auto.add("timeline.editPointMenu.joinThroughEdits", r.rect, label);
    if r.clicked()
        && let (Some(left), Some(right)) = (target.left, target.right)
    {
        if let Err(e) = app.session.execute("sequence.joinThroughEdits", json!({"cut": [left.0, right.0]})) {
            app.ui.status = e.to_string();
        }
        ui.close();
    }
}

/// The menu of a right-clicked gap: Ripple Delete closes it.
pub(super) fn gap_menu(app: &mut FilmcraftApp, ctx: &egui::Context, ui: &mut egui::Ui) {
    let label = tl!("Ripple Delete");
    let r = ui.add_enabled(app.session.state.gap_selection.is_some(), egui::Button::new(label));
    app.auto.add("timeline.gapMenu.edit.rippleDelete", r.rect, label);
    if r.clicked() {
        if let Err(e) = crate::menus::invoke(app, ctx, "edit.rippleDelete", json!({})) {
            app.ui.status = e;
        }
        ui.close();
    }
}

/// The clip menu's Multi-Camera submenu: Enable, Flatten, and the cameras of the selected nested
/// sequence clips (greyed out when none of the selected clips is a nested sequence).
pub(super) fn multicam_menu(app: &mut FilmcraftApp, ui: &mut egui::Ui, picked: &[&TrackItem]) {
    let nests: Vec<&TrackItem> = picked.iter().copied().filter(|it| app.session.project.sequence(it.item).is_some()).collect();
    let enabled = !nests.is_empty() && nests.iter().all(|it| it.multicam.is_some_and(|m| m.enabled));
    // camera names of the first selected nest, and the angle it shows
    let cameras: Vec<(usize, String)> = nests
        .first()
        .and_then(|it| app.session.project.sequence(it.item))
        .map(|q| q.cameras().video_angles().map(|(i, c)| (i, c.name.clone())).collect())
        .unwrap_or_default();
    let shown = nests.first().and_then(|it| it.multicam).filter(|m| m.enabled).map(|m| m.angle as usize);
    let mut run: Option<(&str, Value)> = None;
    ui.add_enabled_ui(!nests.is_empty(), |ui| {
        let r = ui.menu_button(tl!("Multi-Camera"), |ui| {
            for (label, cmd) in [(if enabled { tl!("✓ Enable") } else { tl!("Enable") }, "clip.multicamEnable"), (tl!("Flatten"), "clip.multicamFlatten")] {
                let r = ui.add_enabled(app.session.is_enabled(cmd), egui::Button::new(label));
                app.auto.add(&format!("timeline.clipMenu.{cmd}"), r.rect, label);
                if r.clicked() {
                    run = Some((cmd, json!({})));
                }
            }
            if enabled && !cameras.is_empty() {
                ui.separator();
                for (angle, name) in &cameras {
                    let label = if shown == Some(*angle) { format!("✓ {name}") } else { name.clone() };
                    let r = ui.button(&label);
                    app.auto.add(&format!("timeline.clipMenu.multicam.camera.{angle}"), r.rect, &label);
                    if r.clicked() {
                        run = Some(("multicam.switchAngle", json!({"angle": angle, "clips": nests.iter().map(|it| it.id.0).collect::<Vec<_>>()})));
                    }
                }
            }
        });
        app.auto.add("timeline.clipMenu.clip.multicam", r.response.rect, "Multi-Camera");
    });
    if let Some((cmd, params)) = run {
        if let Err(e) = app.session.execute(cmd, params) {
            app.ui.status = e.to_string();
        }
        ui.close();
    }
}
