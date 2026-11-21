//! #4891: on a narrow window the in-window menu bar ran underneath the header's right-hand
//! cluster (Discord pill, workspace name, quick actions) and both were painted over each
//! other. The menu bar must stop at the cluster, shedding the Discord label first and
//! putting the menus that no longer fit behind an overflow button.

use egui::Rect;
use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use serde_json::json;

fn harness(width: f32) -> Harness<'static, FilmcraftApp> {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let app = FilmcraftApp::new(s);
    let mut harness = Harness::builder().with_size(egui::vec2(width, 700.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
    for _ in 0..4 {
        harness.step();
    }
    harness
}

fn rects(h: &Harness<'static, FilmcraftApp>, prefix: &str) -> Vec<(String, Rect)> {
    h.state()
        .auto
        .query(prefix)
        .iter()
        .map(|e| (e.id.clone(), Rect::from_min_size(egui::pos2(e.rect[0], e.rect[1]), egui::vec2(e.rect[2], e.rect[3]))))
        .collect()
}

#[test]
fn the_menu_bar_never_runs_under_the_right_hand_cluster() {
    let h = harness(1100.0);
    let menus = rects(&h, "menu.");
    assert!(!menus.is_empty(), "the in-window menu bar is drawn");
    let discord = rects(&h, "header.discord");
    assert_eq!(discord.len(), 1, "the Discord button is drawn");
    let d = discord[0].1;
    for (id, r) in &menus {
        assert!(r.max.x <= d.min.x + 1.0, "{id} ends at {} but the Discord button starts at {}: they overlap", r.max.x, d.min.x);
    }
    // every menu is still reachable: shown as a title, or behind the overflow button
    let titles = menus.iter().filter(|(id, _)| id != "menu.overflow").count();
    let overflow = menus.iter().any(|(id, _)| id == "menu.overflow");
    assert!(titles == 9 || overflow, "only {titles} of 9 menus are shown and there is no overflow button");
    assert!(h.state().ui_error.is_none(), "{:?}", h.state().ui_error);
}

#[test]
fn a_wide_window_shows_every_menu_without_overflow() {
    let h = harness(1600.0);
    let menus = rects(&h, "menu.");
    let titles: Vec<&String> = menus.iter().map(|(id, _)| id).filter(|id| *id != "menu.overflow").collect();
    assert_eq!(titles.len(), 9, "all nine menus are titles at 1600 pt: {titles:?}");
    assert!(!menus.iter().any(|(id, _)| id == "menu.overflow"), "no overflow button at 1600 pt");
    let discord = rects(&h, "header.discord");
    let d = discord[0].1;
    for (id, r) in &menus {
        assert!(r.max.x <= d.min.x + 1.0, "{id} ends at {} but the Discord button starts at {}", r.max.x, d.min.x);
    }
    assert!(h.state().ui_error.is_none(), "{:?}", h.state().ui_error);
}
