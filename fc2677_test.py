import pathlib
F = pathlib.Path("crates/ui-egui/tests/hand_drag_half_2536.rs")
s = F.read_text()

anchor = """/// The Hand tool picks the half the same way."""
assert s.count(anchor) == 1

test = '''/// Escape mid-drag puts the started half's scroll back — not the other half's value (#2536
/// review): the cancel must restore what the dragged half had, even when the halves differ.
#[test]
fn cancelling_an_audio_drag_restores_the_audio_scroll() {
    let mut d = Driver::demo();
    let split = d.split_y();
    // The two halves start at clearly different scrolls; the bug set a_scroll to v_scroll.
    d.app().ui.timeline.v_scroll = 30.0;
    d.app().ui.timeline.a_scroll = 70.0;
    d.frames(2);
    let from = egui::pos2(900.0, split + 60.0);
    let to = from - egui::vec2(0.0, 40.0);
    d.send(egui::Event::PointerMoved(from));
    d.middle(from, true);
    for k in 1..=8 {
        d.send(egui::Event::PointerMoved(from.lerp(to, k as f32 / 8.0)));
    }
    // Escape while the button is still down cancels the drag (#580).
    d.send(egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() });
    d.middle(to, false);
    d.frames(2);
    let tl = &d.app().ui.timeline;
    assert!((tl.a_scroll - 70.0).abs() < 1e-6, "the audio scroll returns to its own start: a_scroll={}", tl.a_scroll);
    assert!((tl.v_scroll - 30.0).abs() < 1e-6, "the video scroll never moved: v_scroll={}", tl.v_scroll);
}

'''
s = s.replace(anchor, test + anchor)
F.write_text(s)
print("cancel test added")
