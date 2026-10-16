//! Explicit opt-in real-model retrieval check. Fixture manifest is local, never bundled.
#![cfg(feature = "clip")]
#[test]
#[ignore = "requires FILMCRAFT_CLIP_DIR and FILMCRAFT_VISUAL_FIXTURES"]
fn real_model_ranks_inspected_images() {
    let root = std::env::var("FILMCRAFT_CLIP_DIR").expect("model directory");
    let manifest = std::env::var("FILMCRAFT_VISUAL_FIXTURES").expect("fixture manifest");
    let items: serde_json::Value = serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
    let items = items.as_array().unwrap();
    let load = std::time::Instant::now();
    let model = filmcraft_vision::load(std::path::Path::new(&root)).unwrap();
    eprintln!("model_load_seconds={:.3}", load.elapsed().as_secs_f64());
    let mut vectors = Vec::new();
    for item in items {
        let image = image::open(item["path"].as_str().unwrap()).unwrap().to_rgba8();
        let start = std::time::Instant::now();
        let v = model.image(image.width(), image.height(), image.as_raw()).unwrap();
        eprintln!("image={} inference_seconds={:.3}", item["query"], start.elapsed().as_secs_f64());
        vectors.push(v);
    }
    let mut correct = 0;
    for (expected, item) in items.iter().enumerate() {
        let start = std::time::Instant::now();
        let query = model.text(item["query"].as_str().unwrap()).unwrap();
        let mut scores: Vec<_> = vectors.iter().enumerate().map(|(i, v)| (i, filmcraft_vision::similarity(&query, v).unwrap())).collect();
        scores.sort_by(|a, b| b.1.total_cmp(&a.1));
        eprintln!("query={} expected={} ranked={:?} seconds={:.3}", item["query"], expected, scores, start.elapsed().as_secs_f64());
        if scores[0].0 == expected {
            correct += 1;
        }
    }
    eprintln!("top1={correct}/{}", items.len());
    assert!(items.len() >= 4, "use at least four inspected images");
    assert!(correct * 4 >= items.len() * 3, "at least 75% top-1 on this bounded fixture");
    assert!(model.text("").is_err());
    assert!(model.image(0, 0, &[]).is_err());
}
