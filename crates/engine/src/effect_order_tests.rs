use super::*;
use filmcraft_project::{ClipId, EffectInstance};
use serde_json::json;

/// A 2-second colour matte in a new sequence with `effects` applied after its fixed effects.
fn setup(effects: &[&str]) -> (Session, ClipId) {
    let mut s = Session::default();
    s.execute("file.newSequence", json!({"width":64,"height":64,"fps":30,"video":1,"audio":1})).unwrap();
    let item = s.execute("file.newColorMatte", json!({"color":"#8090a0","width":64,"height":64,"seconds":2})).unwrap()["item"].as_u64().unwrap();
    s.execute("source.open", json!({"item":item})).unwrap();
    let c = ClipId(s.execute("source.overwrite", json!({})).unwrap()["clips"][0].as_u64().unwrap());
    for effect in effects {
        s.execute("effects.apply", json!({"clips":[c.0],"effect":effect})).unwrap();
    }
    (s, c)
}

fn stack(s: &Session, c: ClipId) -> Vec<EffectInstance> {
    s.active_sequence().unwrap().find_item(c).unwrap().1.effects.clone()
}

fn index_of(s: &Session, c: ClipId, effect: &str) -> usize {
    stack(s, c).iter().position(|e| e.effect == effect).unwrap()
}

fn reorders(s: &mut Session) -> Vec<(ClipId, Vec<usize>)> {
    s.drain_events()
        .into_iter()
        .filter_map(|e| match e {
            Event::EffectsReordered { clip, order } => Some((clip, order)),
            _ => None,
        })
        .collect()
}

#[test]
fn a_move_carries_the_whole_instance_and_is_one_undo_step() {
    let (mut s, c) = setup(&["gaussian_blur", "rounded_crop", "drop_shadow"]);
    let (blur, crop, shadow) = (index_of(&s, c, "gaussian_blur"), index_of(&s, c, "rounded_crop"), index_of(&s, c, "drop_shadow"));
    s.execute("effects.setParam", json!({"clip":c.0,"effect":shadow,"param":"distance","value":12,"keyframe":true,"time":12345})).unwrap();
    s.execute("masks.add", json!({"clip":c.0,"effect":shadow,"shape":"ellipse","center":[32,32],"size":[20,20]})).unwrap();
    let before = stack(&s, c);
    let mark = s.history.undo.len();
    let answer = s.execute("effects.reorder", json!({"clip":c.0,"from":shadow,"to":blur})).unwrap();
    let after = stack(&s, c);
    assert_eq!(after[blur], before[shadow], "the moved effect keeps its parameters, keyframes and masks");
    assert_eq!((&after[crop], &after[shadow]), (&before[blur], &before[crop]), "the effects it passed shift down one place");
    assert_eq!(after[..blur], before[..blur], "the fixed effects stay where they were");
    assert_eq!(answer["index"], json!(blur));
    assert_eq!(s.history.undo.len(), mark + 1);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(stack(&s, c), before);
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(stack(&s, c), after);
    let mark = s.history.undo.len();
    s.execute("effects.reorder", json!({"clip":c.0,"from":blur,"to":blur})).unwrap();
    assert_eq!(s.history.undo.len(), mark, "a move onto itself adds no undo step");
}

#[test]
fn reordered_crop_and_shadow_change_the_picture_and_undo_restores_it() {
    let (mut s, c) = setup(&["rounded_crop", "drop_shadow"]);
    let (crop, shadow) = (index_of(&s, c, "rounded_crop"), index_of(&s, c, "drop_shadow"));
    for (effect, param, value) in [
        (crop, "left", 20),
        (crop, "right", 20),
        (crop, "top", 20),
        (crop, "bottom", 20),
        (crop, "radius", 8),
        (shadow, "distance", 10),
        (shadow, "opacity", 100),
    ] {
        s.execute("effects.setParam", json!({"clip":c.0,"effect":effect,"param":param,"value":value})).unwrap();
    }
    s.execute("effects.setParam", json!({"clip":c.0,"effect":shadow,"param":"color","value":"#ffffff"})).unwrap();
    s.execute("playhead.set", json!({"seconds":0.5})).unwrap();
    let before = s.render_program(1.0).unwrap();
    assert!(before.px.as_chunks::<4>().0.iter().any(|p| p[0] > 0.1), "fixture must render inside the clip");
    s.execute("effects.reorder", json!({"clip":c.0,"from":shadow,"to":crop})).unwrap();
    let after = s.render_program(1.0).unwrap();
    assert!(before.px.iter().zip(&after.px).any(|(a, b)| (a - b).abs() > 0.01), "the shadow now falls before the crop");
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.render_program(1.0).unwrap().px, before.px);
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(s.render_program(1.0).unwrap().px, after.px);
}

#[test]
fn fixed_effects_and_graphic_layers_keep_their_slots_while_effects_move_past_them() {
    let (mut s, c) = setup(&["gaussian_blur", "rounded_crop", "drop_shadow"]);
    // As an older file can have it: Time Remapping and a graphic layer between the standard effects.
    s.edit_sequence("fixture", |q, _, _| {
        let (_, it) = q.find_item_mut(c).unwrap();
        let remap = it.effects.iter().position(|e| e.effect == "time_remap").unwrap();
        let remap = it.effects.remove(remap);
        let shadow = it.effects.iter().position(|e| e.effect == "drop_shadow").unwrap();
        it.effects.insert(shadow, remap);
        it.effects.insert(shadow + 1, filmcraft_project::graphic::new_text_layer("A", Default::default(), 20.0));
        Ok(())
    })
    .unwrap();
    let before = stack(&s, c);
    let fixed: Vec<usize> = (0..before.len()).filter(|i| !effect_order::movable(&before[*i])).collect();
    let (blur, shadow) = (index_of(&s, c, "gaussian_blur"), index_of(&s, c, "drop_shadow"));
    assert!(fixed.iter().any(|i| *i > blur && *i < shadow), "the fixture puts fixed slots between the effects that move");
    s.execute("effects.reorder", json!({"clip":c.0,"from":shadow,"to":blur})).unwrap();
    let after = stack(&s, c);
    for i in &fixed {
        assert_eq!(after[*i], before[*i], "slot {i} keeps its fixed effect or layer");
    }
    let movable = |v: &[EffectInstance]| v.iter().filter(|e| effect_order::movable(e)).map(|e| e.effect.clone()).collect::<Vec<_>>();
    assert_eq!(movable(&after), ["drop_shadow", "gaussian_blur", "rounded_crop"]);
    for i in &fixed {
        assert!(s.execute("effects.reorder", json!({"clip":c.0,"from":i,"to":blur})).is_err(), "slot {i} cannot be moved");
        assert!(s.execute("effects.reorder", json!({"clip":c.0,"from":blur,"to":i})).is_err(), "slot {i} cannot be taken");
    }
    assert_eq!(stack(&s, c), after);
}

#[test]
fn the_selected_effect_or_mask_follows_the_moved_effect_through_undo_and_redo() {
    for mask in [false, true] {
        // two instances of one effect, so the selected effect (an id and an instance number) changes
        let (mut s, c) = setup(&["gaussian_blur", "rounded_crop", "gaussian_blur"]);
        let first = index_of(&s, c, "gaussian_blur");
        let second = first + 2;
        s.execute("effects.setParam", json!({"clip":c.0,"effect":second,"param":"blurriness","value":41})).unwrap();
        s.execute("masks.add", json!({"clip":c.0,"effect":second,"shape":"ellipse","center":[32,32],"size":[20,20]})).unwrap();
        if mask {
            s.execute("masks.select", json!({"clip":c.0,"effect":second,"mask":0})).unwrap();
        } else {
            s.execute("effects.select", json!({"clip":c.0,"effect":second})).unwrap();
        }
        let selected = |s: &Session| {
            if mask { s.state.selected_mask.map(|m| m.effect) } else { s.state.selected_effect.as_ref().and_then(|e| e.index(&stack(s, c))) }
        };
        assert_eq!(selected(&s), Some(second));
        s.drain_events();
        s.execute("effects.reorder", json!({"clip":c.0,"from":second,"to":first})).unwrap();
        assert_eq!(selected(&s), Some(first), "mask selected: {mask}");
        let moved = reorders(&mut s);
        assert_eq!(moved.len(), 1);
        assert_eq!(moved[0].1[second], first, "the frontends hear where each effect went");
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(selected(&s), Some(second), "mask selected: {mask}");
        assert_eq!(reorders(&mut s).first().map(|r| r.1[first]), Some(second), "Undo reports the reverse move");
        s.execute("edit.redo", json!({})).unwrap();
        assert_eq!(selected(&s), Some(first), "mask selected: {mask}");
    }
}

#[test]
fn undo_and_redo_put_identical_copies_back_where_they_were() {
    // identical blurs apart, and next to each other (where the stacks before and after the move
    // are equal, so only the recorded move can say which copy went where)
    for effects in [["gaussian_blur", "rounded_crop", "gaussian_blur"], ["rounded_crop", "gaussian_blur", "gaussian_blur"]] {
        let (mut s, c) = setup(&effects);
        let first = index_of(&s, c, "gaussian_blur");
        let second = stack(&s, c).iter().rposition(|e| e.effect == "gaussian_blur").unwrap();
        let selected = |s: &Session| s.state.selected_effect.as_ref().and_then(|e| e.index(&stack(s, c)));
        s.execute("effects.select", json!({"clip":c.0,"effect":second})).unwrap();
        s.execute("effects.reorder", json!({"clip":c.0,"from":second,"to":first})).unwrap();
        assert_eq!(selected(&s), Some(first), "{effects:?}");
        s.drain_events();
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(selected(&s), Some(second), "{effects:?}: Undo takes the selection back to the copy that moved");
        assert_eq!(reorders(&mut s).first().map(|r| r.1[first]), Some(second), "{effects:?}");
        s.execute("edit.redo", json!({})).unwrap();
        assert_eq!(selected(&s), Some(first), "{effects:?}");
    }
}

#[test]
fn undo_reports_a_move_on_a_clip_that_is_no_longer_selected() {
    let (mut s, c) = setup(&["rounded_crop", "drop_shadow"]);
    let (crop, shadow) = (index_of(&s, c, "rounded_crop"), index_of(&s, c, "drop_shadow"));
    s.execute("effects.reorder", json!({"clip":c.0,"from":shadow,"to":crop})).unwrap();
    s.execute("timeline.select", json!({"clips":[]})).unwrap();
    s.execute("effects.select", json!({"none":true})).unwrap();
    assert!(s.state.selection.is_empty());
    s.drain_events();
    s.execute("edit.undo", json!({})).unwrap();
    let moved = reorders(&mut s);
    assert_eq!(moved.len(), 1, "the frontends hear about the clip although nothing on it is selected");
    assert_eq!((moved[0].0, moved[0].1[crop], moved[0].1[shadow]), (c, shadow, crop), "Undo swaps them back");
}

#[test]
fn undoing_an_edit_that_is_not_a_move_reports_no_reorder() {
    let (mut s, c) = setup(&["gaussian_blur", "rounded_crop"]);
    let blur = index_of(&s, c, "gaussian_blur");
    s.execute("effects.select", json!({"clip":c.0,"effect":blur})).unwrap();
    s.execute("effects.setParam", json!({"clip":c.0,"effect":blur,"param":"blurriness","value":30})).unwrap();
    s.drain_events();
    s.execute("edit.undo", json!({})).unwrap();
    assert!(reorders(&mut s).is_empty());
    assert_eq!(s.state.selected_effect.as_ref().unwrap().index(&stack(&s, c)), Some(blur));
}

#[test]
fn move_up_and_down_step_past_fixed_slots_and_stop_at_the_ends() {
    let (mut s, c) = setup(&["gaussian_blur", "rounded_crop"]);
    s.edit_sequence("fixture", |q, _, _| {
        let (_, it) = q.find_item_mut(c).unwrap();
        let crop = it.effects.iter().position(|e| e.effect == "rounded_crop").unwrap();
        it.effects.insert(crop, filmcraft_project::graphic::new_text_layer("A", Default::default(), 20.0));
        Ok(())
    })
    .unwrap();
    let names = |s: &Session| stack(s, c).iter().filter(|e| effect_order::movable(e)).map(|e| e.effect.clone()).collect::<Vec<_>>();
    assert!(s.execute("effects.moveUp", json!({})).is_err(), "nothing selected");
    s.execute("effects.select", json!({"clip":c.0,"effect":"rounded_crop"})).unwrap();
    s.execute("effects.moveUp", json!({})).unwrap();
    assert_eq!(names(&s), ["rounded_crop", "gaussian_blur"], "Move Up passes the graphic layer to the previous effect");
    assert_eq!(s.state.selected_effect.as_ref().unwrap().effect, "rounded_crop");
    let mark = s.history.undo.len();
    assert!(s.execute("effects.moveUp", json!({})).is_err(), "the first effect can't move up");
    assert_eq!(s.history.undo.len(), mark);
    s.execute("effects.moveDown", json!({})).unwrap();
    assert_eq!(names(&s), ["gaussian_blur", "rounded_crop"]);
    assert!(s.execute("effects.moveDown", json!({})).is_err(), "the last effect can't move down");
    let blur = index_of(&s, c, "gaussian_blur");
    s.execute("effects.moveDown", json!({"clip":c.0,"index":blur})).unwrap();
    assert_eq!(names(&s), ["rounded_crop", "gaussian_blur"], "an explicit clip and index need no selection");
}

#[test]
fn hostile_parameters_are_errors_that_change_nothing() {
    let empty = Session::default();
    assert!(!empty.is_enabled("effects.reorder"));
    let (mut s, c) = setup(&["rounded_crop", "drop_shadow"]);
    let crop = index_of(&s, c, "rounded_crop");
    let len = stack(&s, c).len();
    // with an effect selected that could move up, a bad `clip` must not fall back to it
    s.execute("effects.select", json!({"clip":c.0,"effect":"drop_shadow"})).unwrap();
    let before = stack(&s, c);
    let mark = s.history.undo.len();
    for (cmd, p) in [
        ("effects.reorder", json!({})),
        ("effects.reorder", json!(null)),
        ("effects.reorder", json!({"clip":c.0,"from":-1,"to":crop})),
        ("effects.reorder", json!({"clip":c.0,"from":crop,"to":1.5})),
        ("effects.reorder", json!({"clip":c.0,"from":u64::MAX,"to":crop})),
        ("effects.reorder", json!({"clip":c.0,"from":crop,"to":len})),
        ("effects.reorder", json!({"clip":c.0,"from":"rounded_crop","to":crop})),
        ("effects.reorder", json!({"clip":u64::MAX,"from":crop,"to":crop + 1})),
        ("effects.reorder", json!({"clip":c.0,"from":0,"to":crop})),
        ("effects.moveUp", json!({"clip":c.0})),
        ("effects.moveUp", json!({"index":crop})),
        ("effects.moveDown", json!({"clip":c.0,"index":-3})),
        ("effects.moveDown", json!({"clip":c.0,"index":u64::MAX})),
        ("effects.moveDown", json!({"clip":u64::MAX,"index":crop})),
        ("effects.moveUp", json!({"clip":c.0,"index":0})),
        ("effects.moveUp", json!({"clip":"bogus"})),
        ("effects.moveUp", json!({"clip":-1})),
        ("effects.moveUp", json!({"clip":1.5})),
    ] {
        assert!(s.execute(cmd, p.clone()).is_err(), "{cmd} {p}");
        assert_eq!(stack(&s, c), before, "{cmd} {p}");
        assert_eq!(s.history.undo.len(), mark, "{cmd} {p}");
    }
}
