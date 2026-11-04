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
