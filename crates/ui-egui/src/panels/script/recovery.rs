//! Recovery of offline or deleted Script sources uses the same reviewed, undoable rebind command.

use super::*;

fn source_missing(app: &FilmcraftApp, source: ItemId) -> bool {
    let Some(item) = app.session.project.item(source) else { return true };
    match &item.kind {
        ItemKind::Media(_) | ItemKind::Subclip { .. } => app
            .session
            .project
            .resolve_media(source)
            .and_then(|(root, _, _)| app.session.project.item(root))
            .is_none_or(|item| crate::panels::project_views::media_badge(app, item).offline),
        ItemKind::Sequence(sequence) => sequence.multicam.is_none(),
        _ => true,
    }
}

pub(super) fn needs_rebind(app: &FilmcraftApp, take: &ScriptTake) -> bool {
    take.source.is_none_or(|source| source_missing(app, source))
}

fn selected_source(app: &FilmcraftApp) -> Option<ItemId> {
    let [source] = app.session.state.project_selection.as_slice() else { return None };
    (!source_missing(app, *source)).then_some(*source)
}

pub(super) fn source_menu(app: &mut FilmcraftApp, ui: &mut egui::Ui, take: &ScriptTake, actions: &mut Vec<Action>) {
    if let Some(source) = take.source
        && ui.button(tl!("Find in Project")).clicked()
    {
        actions.push(Action::FindInProject(source));
        ui.close();
    }
    let source = selected_source(app);
    let response = ui
        .add_enabled(source.is_some(), egui::Button::new(tl!("Rebind to Selected Project Clip")))
        .on_disabled_hover_text(tl!("Select one available media clip, subclip, or multicamera source in Project first."))
        .on_hover_text(tl!("Reconnect every matching candle in all scripts. Matching footage keeps its marks; other replacements require review."));
    app.auto.add(&format!("script.take.{}.rebindSelected", take.id.0), response.rect, "Rebind to Selected Project Clip");
    if response.clicked()
        && let Some(source) = source
    {
        actions.push(Action::Rebind { take: take.id.0, source: source.0 });
        ui.close();
    }
}
