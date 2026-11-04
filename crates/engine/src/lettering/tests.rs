use super::*;

fn ws(s: &str) -> Vec<Word> {
    words_from_text(s)
}

/// Fake metrics: 55 px per character at size 100; hero flags and binding as the rules say.
fn metrics(words: &[Word], hero: &[usize]) -> Vec<WordMetric> {
    words
        .iter()
        .enumerate()
        .map(|(i, w)| WordMetric {
            width100: w.text.chars().count() as f32 * 55.0,
            hero: hero.contains(&i),
            binds_next: binds_forward(&normalize(&w.text)) || (has_digit(&w.text) && words.get(i + 1).is_some_and(|x| x.text.chars().count() <= 4)),
            clause_end: w.text.ends_with([',', '.', '?', '!']),
        })
        .collect()
}

fn params(box_w: f32) -> Params {
    Params { box_w, box_h: 1600.0, size: 60.0, hero_scale: 1.6, space100: 28.0, ..Default::default() }
}

fn line_texts(w: &[Word], c: &Composed) -> Vec<String> {
    c.lines.iter().map(|l| l.iter().map(|&i| w[i].text.as_str()).collect::<Vec<_>>().join(" ")).collect()
}

#[test]
fn never_more_than_three_lines_and_no_lone_last_word() {
    for text in [
        "Você precisa de um sistema de edição mais rápido",
        "A marca é o que as pessoas dizem de você quando você sai da sala",
        "Isso muda tudo",
        "Nunca mais perca um corte por causa de uma pausa longa demais no meio da fala",
        "Marca forte vende mais caro todos os dias",
    ] {
        let w = ws(text);
        let h = heroes(&w);
        let c = compose(&metrics(&w, &h), &params(900.0));
        if c.overflow {
            assert!(c.split_at.is_some(), "{text}");
            continue;
        }
        assert!(c.lines.len() <= MAX_LINES, "{text}: {:?}", line_texts(&w, &c));
        let last = c.lines.last().unwrap();
        if c.lines.len() > 1 && last.len() == 1 {
            let m = metrics(&w, &h);
            assert!(m[last[0]].hero || c.words.iter().find(|p| p.index == last[0]).unwrap().width >= 0.45 * c.width, "widow in {text}: {:?}", line_texts(&w, &c));
        }
        // every word is placed once, in order
        let idx: Vec<usize> = c.words.iter().map(|p| p.index).collect();
        assert_eq!(idx, (0..w.len()).collect::<Vec<_>>());
    }
}

#[test]
fn greedy_widow_is_avoided() {
    // five equal words, a box that holds four: greedy would leave the fifth alone.
    let w = ws("aaaa bbbb cccc dddd eeee");
    let c = compose(&metrics(&w, &[]), &params(4.0 * 4.0 * 55.0 * 0.6 + 3.0 * 28.0 * 0.6 + 1.0));
    assert!(!c.overflow);
    let t = line_texts(&w, &c);
    assert!(t.last().unwrap().split(' ').count() >= 2, "{t:?}");
}

#[test]
fn a_line_does_not_end_on_an_article_or_preposition() {
    let w = ws("Isso é o fim de tudo que construímos juntos");
    let c = compose(&metrics(&w, &[]), &params(700.0));
    for l in &c.lines[..c.lines.len() - 1] {
        let last = &w[*l.last().unwrap()].text;
        assert!(!binds_forward(&normalize(last)), "line ends on `{last}`: {:?}", line_texts(&w, &c));
    }
}

#[test]
fn a_number_stays_with_its_unit() {
    let w = ws("Cobrei 10 mil reais por um logo");
    let c = compose(&metrics(&w, &[1]), &params(520.0));
    for l in &c.lines[..c.lines.len() - 1] {
        assert_ne!(w[*l.last().unwrap()].text, "10", "{:?}", line_texts(&w, &c));
    }
}

#[test]
fn too_long_asks_to_split_at_a_clause() {
    let w = ws("Quando a gente começou ninguém acreditava que daria certo, mas a gente insistiu em cada detalhe de cada entrega durante muitos meses seguidos");
    let c = compose(&metrics(&w, &[]), &params(500.0));
    assert!(c.overflow);
    let at = c.split_at.unwrap();
    assert!(at > 3 && at < w.len() - 3, "{at}");
    assert!(!binds_forward(&normalize(&w[at - 1].text)));
}

#[test]
fn heroes_are_content_words_and_few() {
    let w = ws("Nunca pague 10 mil por um logo sem estratégia");
    let h = heroes(&w);
    assert!(!h.is_empty() && h.len() <= 3, "{h:?}");
    for i in &h {
        assert!(!STOP.contains(&normalize(&w[*i].text).as_str()) || NEGATION.contains(&normalize(&w[*i].text).as_str()), "{}", w[*i].text);
    }
    assert!(h.contains(&2) || h.contains(&0) || h.contains(&8), "{h:?}");
    // an article is never a hero, even alone
    assert!(heroes(&ws("o a de um")).is_empty());
}

#[test]
fn speech_stress_lifts_a_word() {
    let mut w = ws("isso muda tudo");
    w[1].start = 0.0;
    w[1].end = 0.9;
    w[2].start = 1.5;
    w[2].end = 1.7;
    assert!(score(&w, 1) > score(&ws("isso muda tudo"), 1));
}

#[test]
fn block_flush_left_and_stair_steps_forward() {
    let w = ws("Você precisa de um sistema de edição mais rápido");
    let h = heroes(&w);
    let blk = compose(&metrics(&w, &h), &params(900.0));
    let first_x: Vec<f32> = (0..blk.lines.len()).map(|k| blk.words.iter().find(|p| p.line == k).unwrap().x).collect();
    assert!(first_x.iter().all(|&x| x == 0.0), "{first_x:?}");
    let st = compose(&metrics(&w, &h), &Params { composition: Composition::Stair, ..params(900.0) });
    if st.lines.len() > 1 {
        let xs: Vec<f32> = (0..st.lines.len()).map(|k| st.words.iter().find(|p| p.line == k).unwrap().x).collect();
        assert!(xs.windows(2).all(|p| p[1] > p[0]), "{xs:?}");
    }
    // baselines grow, heroes are bigger than support
    let b: Vec<f32> = (0..blk.lines.len()).map(|k| blk.words.iter().find(|p| p.line == k).unwrap().baseline).collect();
    assert!(b.windows(2).all(|p| p[1] > p[0]));
    let hero_size = blk.words.iter().find(|p| p.hero).map(|p| p.size);
    let sup_size = blk.words.iter().find(|p| !p.hero).map(|p| p.size).unwrap();
    if let Some(hs) = hero_size {
        assert!(hs > sup_size);
    }
}

#[test]
fn words_fit_inside_the_box() {
    let w = ws("Marca forte vende mais caro todos os dias");
    let h = heroes(&w);
    let c = compose(&metrics(&w, &h), &params(640.0));
    assert!(!c.overflow);
    assert!(c.width <= 640.0 + 0.5, "{}", c.width);
}

#[test]
fn plan_with_the_real_shaper() {
    let r = plan(&json!({"text": "Você precisa de um sistema de edição mais rápido", "family": "Inter", "supportStyle": "Regular", "heroStyle": "Bold", "box": [880, 1200], "size": 64})).unwrap();
    let lines = r["lines"].as_array().unwrap();
    assert!(!lines.is_empty() && lines.len() <= 3, "{r}");
    assert_eq!(r["layout"]["overflow"], false);
    assert!(plan(&json!({})).is_err());
}

fn seq() -> Session {
    let mut s = Session::default();
    s.execute("file.newSequence", json!({"width": 1080, "height": 1920, "fps": 30})).unwrap();
    s
}

#[test]
fn apply_makes_one_clip_with_a_layer_per_word_and_no_overshoot() {
    let mut s = seq();
    let r = s.execute("lettering.apply", json!({"text": "Nunca pague 10 mil por um logo sem estratégia", "family": "Inter", "supportStyle": "Regular", "heroStyle": "Bold"})).unwrap();
    assert_eq!(r["applied"], true, "{r}");
    let clip = filmcraft_project::ClipId(r["clip"].as_u64().unwrap());
    let q = s.active_sequence().unwrap();
    let (_, it) = q.find_item(clip).unwrap();
    let layers: Vec<_> = it.effects.iter().filter(|e| e.params.contains_key("text")).collect();
    assert_eq!(layers.len(), 9);
    // heroes carry the accent colour, the rest the off-white
    let fill = |e: &filmcraft_project::EffectInstance| e.params["fill_color"].value.clone();
    let hero_fill = fill(layers.iter().find(|e| e.params["text"].value == ParamValue::Text("Nunca".into())).unwrap());
    let plain_fill = fill(layers.iter().find(|e| e.params["text"].value == ParamValue::Text("logo".into())).unwrap());
    assert_ne!(hero_fill, plain_fill);
    // opacity stays within 0..=100 at every frame (no overshoot) and every word ends visible
    let end = Tick::from_seconds_f64(r["duration"].as_f64().unwrap());
    for e in &layers {
        let op = &e.params["opacity"];
        let mut t = Tick(0);
        while t < end {
            let v = op.value_at(t).as_f64().unwrap();
            assert!((-1e-6..=100.0 + 1e-6).contains(&v), "opacity {v} at {t:?}");
            t = Tick(t.0 + filmcraft_time::TICKS_PER_SECOND / 30);
        }
        assert_eq!(op.value_at(Tick::from_seconds_f64(2.0)).as_f64().unwrap().round(), 100.0);
    }
    // words arrive one after another
    let first_visible: Vec<f64> = layers
        .iter()
        .map(|e| {
            let op = &e.params["opacity"];
            (0..600).map(|f| f as f64 / 100.0).find(|t| op.value_at(Tick::from_seconds_f64(*t)).as_f64().unwrap() > 1.0).unwrap_or(f64::MAX)
        })
        .collect();
    assert!(first_visible.windows(2).all(|w| w[1] >= w[0]), "{first_visible:?}");
}

#[test]
fn apply_reports_a_phrase_that_cannot_fit_instead_of_squeezing_it() {
    let mut s = seq();
    let long = "Quando a gente começou ninguém acreditava que daria certo, mas a gente insistiu em cada detalhe de cada entrega durante muitos meses seguidos e nunca desistiu de nada";
    let r = s.execute("lettering.apply", json!({"text": long, "size": 120, "boxHeight": 300})).unwrap();
    assert_eq!(r["applied"], false);
    assert!(r["splitAt"].as_u64().is_some());
    assert!(s.active_sequence().unwrap().video_tracks.iter().all(|t| t.items.is_empty()));
}

#[test]
fn from_selection_letters_the_selected_caption_where_it_sits() {
    let mut s = seq();
    let cap = s.execute("captions.add", json!({"track": "C1", "text": "Nunca pague 10 mil por um logo sem estratégia", "seconds": 5.0, "durationSeconds": 3.0})).unwrap()["caption"].as_u64().unwrap();
    s.execute("captions.select", json!({"captions": [cap]})).unwrap();
    let r = s.execute("lettering.fromSelection", json!({"family": "Inter", "supportStyle": "Regular", "heroStyle": "Bold"})).unwrap();
    assert_eq!(r["applied"], true, "{r}");
    let clip = filmcraft_project::ClipId(r["clip"].as_u64().unwrap());
    let q = s.active_sequence().unwrap();
    let (_, it) = q.find_item(clip).unwrap();
    // it starts a little before the caption, so the first word is on screen as it is said
    assert!((it.start.seconds() - 4.9).abs() < 0.05, "{}", it.start.seconds());
    assert_eq!(it.effects.iter().filter(|e| e.params.contains_key("text")).count(), 9);
}

#[test]
fn from_selection_takes_the_text_of_a_selected_graphic() {
    let mut s = seq();
    let t = s.execute("graphics.newText", json!({"text": "Marca forte vende mais caro", "newClip": true})).unwrap();
    s.state.selection = vec![filmcraft_project::ClipId(t["clip"].as_u64().unwrap())];
    s.state.caption_selection.clear();
    let r = s.execute("lettering.fromSelection", json!({"family": "Inter", "supportStyle": "Regular", "heroStyle": "Bold"})).unwrap();
    assert_eq!(r["applied"], true, "{r}");
}

#[test]
fn from_selection_without_anything_selected_is_an_error() {
    let mut s = seq();
    assert!(s.execute("lettering.fromSelection", json!({})).is_err());
}
