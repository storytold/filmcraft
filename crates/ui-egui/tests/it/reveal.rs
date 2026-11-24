//! Shared test scrolling helpers. Keep each driver's input pacing intact.
use serde_json::{Value, json};

pub(super) fn panel_containing<D>(
    driver: &mut D,
    id: &str,
    rect: impl Fn(&mut D, &str) -> [f64; 4],
    ok: impl Fn(&mut D, &str, Value) -> Value,
    frames: impl Fn(&mut D, usize),
) {
    let mut last = None;
    for _ in 0..30 {
        let e = rect(driver, id);
        let cx = e[0] + e[2] / 2.0;
        let panels = ok(driver, "ui.elements", json!({"prefix": "panel."}));
        let panel = panels
            .as_array()
            .into_iter()
            .flatten()
            .filter(|p| p["id"].as_str().is_some_and(|pid| pid.matches('.').count() == 1))
            .filter_map(|p| {
                let r: Vec<f64> = p["rect"].as_array()?.iter().filter_map(Value::as_f64).collect();
                (r.len() == 4).then(|| [r[0], r[1], r[2], r[3]])
            })
            .find(|r| cx >= r[0] && cx <= r[0] + r[2] && e[1] >= r[1] && e[1] <= r[1] + r[3] + 40.0);
        let Some(p) = panel else { return };
        if e[1] + e[3] <= p[1] + p[3] * 0.6 || last == Some(e[1]) {
            return;
        }
        last = Some(e[1]);
        ok(driver, "ui.scroll", json!({"x": cx, "y": p[1] + p[3] / 2.0, "dy": -40.0}));
        frames(driver, 3);
    }
}

pub(super) fn effect_controls<D>(
    driver: &mut D,
    id: &str,
    rect: impl Fn(&mut D, &str) -> [f64; 4],
    ok: impl Fn(&mut D, &str, Value) -> Value,
    frames: impl Fn(&mut D, usize),
    scroll_up: bool,
) {
    let p = rect(driver, "panel.EffectControls");
    let mut last = None;
    for _ in 0..30 {
        let e = rect(driver, id);
        let dy = if scroll_up && e[1] < p[1] + 40.0 {
            40.0
        } else if e[1] + e[3] > p[1] + p[3] * 0.6 {
            -40.0
        } else {
            return;
        };
        if last == Some(e[1]) {
            return;
        }
        last = Some(e[1]);
        ok(driver, "ui.scroll", json!({"x": p[0] + 40.0, "y": p[1] + p[3] / 2.0, "dy": dy}));
        frames(driver, 3);
    }
}
