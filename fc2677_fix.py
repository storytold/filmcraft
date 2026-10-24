import pathlib
F = pathlib.Path("crates/ui-egui/src/panels/timeline.rs")
s = F.read_text()

old1 = """                Some(Drag::Pan { last, start_scroll: app.ui.timeline.target_scroll, start_v: app.ui.timeline.v_scroll, audio: last.y >= layout.split_y })"""
assert s.count(old1) == 1
new1 = """                let audio = last.y >= layout.split_y;
                let start_v = if audio { app.ui.timeline.a_scroll } else { app.ui.timeline.v_scroll };
                Some(Drag::Pan { last, start_scroll: app.ui.timeline.target_scroll, start_v, audio })"""
s = s.replace(old1, new1)

old2 = """                Some(Drag::Pan { last: p, start_scroll: app.ui.timeline.target_scroll, start_v: app.ui.timeline.v_scroll, audio: p.y >= layout.split_y })"""
assert s.count(old2) == 1
new2 = """                let audio = p.y >= layout.split_y;
                let start_v = if audio { app.ui.timeline.a_scroll } else { app.ui.timeline.v_scroll };
                Some(Drag::Pan { last: p, start_scroll: app.ui.timeline.target_scroll, start_v, audio })"""
s = s.replace(old2, new2)
F.write_text(s)
print("both constructors capture the started half's scroll")
