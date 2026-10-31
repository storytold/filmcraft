//! Whole-style underline must follow `baseline_shift` the same way character-style runs do.
use filmcraft_text::{ParagraphStyle, StyleRun, TextStyle, layout, layout_rich};

fn style(shift: f32) -> TextStyle {
    TextStyle { size: 100.0, underline: true, baseline_shift: shift, ..Default::default() }
}

fn underline_y(shift: f32) -> (f32, f32) {
    let para = ParagraphStyle::default();
    let s = style(shift);
    let plain = layout("Hi", &s, &para);
    let rich = layout_rich("Hi", &s, &[StyleRun { range: 0..2, style: s.clone() }], &para);
    (plain.underlines[0][1], rich.underlines[0][1])
}

#[test]
fn whole_style_underline_follows_baseline_shift() {
    let (p0, r0) = underline_y(0.0);
    for shift in [40.0, -40.0] {
        let (plain, rich) = underline_y(shift);
        assert!((plain - rich).abs() < 1e-3, "plain {plain} vs rich {rich} at shift {shift}");
        assert!((plain - (p0 - shift)).abs() < 1e-3, "plain underline y {plain} should move with shift {shift}");
        assert!((rich - (r0 - shift)).abs() < 1e-3, "rich underline y {rich} should move with shift {shift}");
    }
}
