//! Private group-clip evidence checks. No fixture discovery or source writes.
use filmcraft_interchange::avid_catalog;
use serde_json::{Value, json};

#[test]
fn sequence_recovery_never_bypasses_a_selector_as_an_effect() {
    use filmcraft_interchange::avid_sequence;
    use filmcraft_project::Project;
    let rate = json!({"numerator":24,"denominator":1});
    let selector = json!({"class":"SLCT","media_kind":1,"length":48,"rate":rate,
        "input_tracks":[{"component":{"class":"FILL","media_kind":1,"length":48,"rate":rate}}]});
    let composition = json!({"kind":"CompositionMob","name":"Selector edit","live":true,
        "user_placed":true,"usage_code":0,"id":{"raw":"01".repeat(32),"byte_order":"big"},
        "length":48,"rate":rate,"attributes":{"SEQUERNCE_FORMAT_STRING":"HD 1080p/24"},
        "tracks":[{"index":1,"component":selector},
        {"index":1,"component":{"class":"TCCP","media_kind":3,"length":48,"rate":rate,"start":0,"fps":24,"flags":0}}]});
    let bytes = serde_json::to_vec(&json!({"schema_version":2,"workflow":"avid_managed_mxf","bins":[{
        "path":"Selector.avb","status":"parsed","snapshot_stable":true,"diagnostics":[],"summary":{"bin_uid":"selector"},
        "catalog":{"summary":{"master_count":0},"clips":[]},"mobs":[composition]}]}))
    .unwrap();
    let catalog = avid_catalog::plan_scoped(&bytes, avid_catalog::TransferMode::Sequences).unwrap();
    let mut p = Project::default();
    let bins = avid_catalog::apply(&mut p, catalog, None).unwrap();
    let plan = avid_sequence::plan_best_effort(&p, &bytes, bins[0].bin).unwrap();
    assert!(plan.warnings.iter().any(|w| w.contains("placeholder for SLCT")), "{:?}", plan.warnings);
    assert!(!plan.warnings.iter().any(|w| w.contains("effect bypassed")));
    let (ids, rejected) = avid_sequence::apply_supported(&mut p, plan).unwrap();
    assert!(rejected.is_empty());
    let timeline = p.sequence(ids[0]).unwrap();
    assert_eq!(timeline.duration(), filmcraft_time::FrameRate::FPS_24.tick_of(48));
    let evidence: Value = serde_json::from_str(&p.item(ids[0]).unwrap().metadata[avid_sequence::SEQUENCE_KEY]).unwrap();
    assert_eq!(evidence["composition"]["tracks"][0]["component"]["class"], "SLCT");
}

fn components<'a>(c: &'a Value, out: &mut Vec<&'a Value>, depth: usize) {
    assert!(depth <= 20);
    out.push(c);
    if let Some(children) = c["components"].as_array() {
        for child in children {
            components(child, out, depth + 1);
        }
    }
    for key in ["input_tracks", "alternatives"] {
        if let Some(tracks) = c[key].as_array() {
            for track in tracks {
                components(&track["component"], out, depth + 1);
            }
        }
    }
}

#[test]
#[ignore = "set FILMCRAFT_AVID_GROUP_BIN to the private Phase 15 or 16 group fixture"]
fn native_group_fixture_preserves_selector_evidence() {
    let path = std::env::var("FILMCRAFT_AVID_GROUP_BIN").unwrap();
    let original = std::fs::read(&path).unwrap();
    let doc = avid_catalog::read_native(&original, &path).unwrap();
    let mobs = doc.bins()[0]["mobs"].as_array().unwrap();
    let groups: Vec<_> = mobs.iter().filter(|m| m["live"] == true && m["user_placed"] == true && m["usage_code"] == 4).collect();
    assert_eq!(groups.len(), 1);
    let group = groups[0];
    assert_eq!(group["name"], "A + B + C sources");
    assert!(group["native_decode_errors"].as_array().unwrap().is_empty());
    let mut nodes = Vec::new();
    for track in group["tracks"].as_array().unwrap() {
        components(&track["component"], &mut nodes, 0);
    }
    let selectors: Vec<_> = nodes.into_iter().filter(|c| c["class"] == "SLCT").collect();
    assert_eq!(selectors.len(), 3, "picture and two mono audio selectors");
    let mut evidence = Vec::new();
    for selector in selectors {
        assert_eq!(selector["properties"]["selection_raw"], 1);
        assert!(selector.get("input_tracks").is_none());
        let alternatives = selector["alternatives"].as_array().unwrap();
        assert_eq!(alternatives.len(), 2);
        let mut choices = Vec::new();
        for alt in alternatives {
            let mut children = Vec::new();
            components(&alt["component"], &mut children, 0);
            let refs: Vec<_> = children.into_iter().filter(|c| c["class"] == "SCLP").collect();
            assert_eq!(refs.len(), 1);
            let source = refs[0];
            let matching: Vec<_> = mobs.iter().filter(|m| m["live"] == true && m["id"] == source["source_id"]).collect();
            assert_eq!(matching.len(), 1);
            let internal = matching[0];
            assert_eq!(internal["usage_code"], 5);
            let tracks: Vec<_> = internal["tracks"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|t| t["index"] == source["source_track"] && t["component"]["media_kind"] == source["media_kind"])
                .collect();
            assert_eq!(tracks.len(), 1);
            let mut leaves = Vec::new();
            components(&tracks[0]["component"], &mut leaves, 0);
            let mut masters = Vec::new();
            for leaf in leaves.into_iter().filter(|c| c["class"] == "SCLP") {
                for master in mobs.iter().filter(|m| m["live"] == true && m["id"] == leaf["source_id"] && m["kind"] == "MasterMob") {
                    masters.push(master["name"].clone());
                }
            }
            assert!(!masters.is_empty());
            choices.push(json!({"internal_track":source["source_track"],"masters":masters}));
        }
        evidence.push(json!({"media_kind":selector["media_kind"],"choices":choices}));
    }
    let a = "A - audio (dual mono) + video - master clip";
    let b = "B - video - master clip";
    let c = "C - audio (stereo pair) - master clip";
    assert_eq!(
        json!(evidence),
        json!([
            {"media_kind":1,"choices":[{"internal_track":1,"masters":[a]},{"internal_track":2,"masters":[b]}]},
            {"media_kind":2,"choices":[{"internal_track":1,"masters":[a]},{"internal_track":3,"masters":[c]}]},
            {"media_kind":2,"choices":[{"internal_track":2,"masters":[a]},{"internal_track":4,"masters":[c]}]}
        ]),
        "the second audio alternative belongs to C, not video camera B"
    );
    eprintln!("group selector evidence: {}", serde_json::to_string(&evidence).unwrap());
    assert_eq!(std::fs::read(path).unwrap(), original);
}
