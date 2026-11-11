//! The engine id list and the menu tables are one list. This fails when they drift.

#[test]
fn ui_command_ids_match_the_menu_tables() {
    let mut ids: Vec<&str> = filmcraft_ui_egui::menus::UI_COMMANDS.iter().map(|command| command.id).collect();
    ids.extend(filmcraft_ui_egui::panels::keyboard::COMMANDS.iter().map(|command| command.id));
    assert_eq!(ids, filmcraft_engine::ui_ids::ui_command_ids());
}
