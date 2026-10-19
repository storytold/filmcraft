//! Synthetic regressions for subclip dependency, routing and structural validation.
use filmcraft_interchange::{avid_catalog as cat, avid_sequence as seq};
use filmcraft_project::{AudioComponent, ItemKind, MediaRef, PictureComponent, Project};
use filmcraft_time::{FrameRate, Tick};
use serde_json::{Value, json};

fn rate() -> Value {
    json!({"numerator":24,"denominator":1})
}
fn id(n: u8) -> Value {
    json!({"raw":format!("{n:02x}").repeat(32),"byte_order":"big"})
}
fn leaf(source: u8, track: u32, start: i64, length: i64) -> Value {
    json!({"class":"SCLP","media_kind":2,"length":length,"rate":rate(),"source_id":id(source),"source_track":track,"start":start})
}
fn fixture() -> Value {
    let master = json!({"kind":"MasterMob","name":"Master","live":true,"user_placed":true,"id":id(1),"length":240,"rate":rate(),"user":{},"tracks":[]});
    let sub = json!({"kind":"CompositionMob","name":"Sub","live":true,"user_placed":true,"usage_code":2,"id":id(2),"length":48,"rate":rate(),"attributes":{"_START":24,"_END":72},"tracks":[{"index":1,"component":leaf(1,1,0,240)}]});
    let timeline = json!({"kind":"CompositionMob","name":"Timeline","live":true,"user_placed":true,"usage_code":0,"id":id(3),"length":12,"rate":rate(),"attributes":{"SEQUERNCE_FORMAT_STRING":"HD 1080p/24"},"tracks":[{"index":1,"component":{"class":"TCCP","media_kind":3,"length":12,"rate":rate(),"start":86400,"fps":24,"flags":0}},{"index":1,"component":{"class":"SEQU","media_kind":2,"length":12,"rate":rate(),"components":[leaf(2,1,6,12)]}}]});
    json!({"schema_version":2,"workflow":"avid_managed_mxf","bins":[{"path":"Audit.avb","status":"parsed","snapshot_stable":true,"diagnostics":[],"summary":{"bin_uid":"audit"},"catalog":{"summary":{"master_count":1},"clips":[master.clone()]},"mobs":[master,sub,timeline]}]})
}
fn imported(v: &Value) -> (Project, filmcraft_project::BinId, Vec<String>) {
    let bytes = serde_json::to_vec(v).unwrap();
    let plan = cat::plan_scoped(&bytes, cat::TransferMode::Sequences).unwrap();
    let warnings = plan.warnings.clone();
    let mut p = Project::default();
    let bins = cat::apply(&mut p, plan, None).unwrap();
    for item in p.items.values_mut() {
        if let Some(m) = item.as_media_mut() {
            m.media = MediaRef::Components {
                picture: None,
                channels: (1..=2)
                    .map(|track| AudioComponent {
                        track,
                        file_package: [track as u8; 32],
                        path: None,
                        stream: 0,
                        channel: 0,
                        source_start: Tick::ZERO,
                        length: m.info.duration,
                    })
                    .collect(),
            };
            m.offline = false;
        }
    }
    (p, bins[0].bin, warnings)
}
#[test]
fn simple_avid_flip_wrappers_map_to_native_video_effects() {
    for (avid, expected) in [("EFF2_BLEND_FLIP_VERT", vec!["vertical_flip"]), ("EFF2_BLEND_FLIP_BOTH", vec!["vertical_flip", "horizontal_flip"])] {
        let mut v = fixture();
        let sub = &mut v["bins"][0]["mobs"][1];
        sub["tracks"][0]["component"]["media_kind"] = json!(1);
        let timeline = &mut v["bins"][0]["mobs"][2];
        let leaf = json!({"class":"SCLP","media_kind":1,"length":12,"rate":rate(),"source_id":id(2),"source_track":1,"start":6});
        timeline["tracks"][1]["component"] = json!({"class":"TKFX","media_kind":1,"length":12,"rate":rate(),"unsupported":"component_semantics",
            "properties":{"effect_id":avid,"mc_mode":0},"input_tracks":[{"index":1,"component":leaf}]});
        let (mut p, bin, _) = imported(&v);
        for item in p.items.values_mut() {
            if let Some(m) = item.as_media_mut() {
                m.info.kind = filmcraft_media::MediaKind::Movie;
                m.info.video = Some(filmcraft_media::VideoStreamInfo {
                    width: 1920,
                    height: 1080,
                    frame_rate: FrameRate::FPS_24,
                    par: (1, 1),
                    codec: "test".into(),
                    pixel_format: "test".into(),
                    color: filmcraft_color::ColorInfo::REC709,
                    has_alpha: false,
                    bitrate: None,
                    hdr: None,
                });
                m.info.audio_streams.clear();
                m.media = MediaRef::Components {
                    channels: Vec::new(),
                    picture: Some(PictureComponent {
                        format_pending: false,
                        track: 1,
                        file_package: [1; 32],
                        path: None,
                        source_start: Tick::ZERO,
                        length: m.info.duration,
                    }),
                };
            }
        }
        let plan = seq::plan(&p, &serde_json::to_vec(&v).unwrap(), bin).unwrap();
        let ids = seq::apply(&mut p, plan).unwrap();
        let clip = &p.sequence(ids[0]).unwrap().video_tracks[0].items[0];
        assert_eq!(clip.effects.iter().skip(3).map(|e| e.effect.as_str()).collect::<Vec<_>>(), expected);
        assert_eq!(clip.source_in, FrameRate::FPS_24.tick_of(30));
    }
}
#[test]
fn a_static_avid_warp_maps_2d_controls_without_flattening_unknown_3d_controls() {
    let mut v = fixture();
    let timeline = &mut v["bins"][0]["mobs"][2];
    let guid = |key: u16, value: f64| {
        let mut id = Vec::from(key.to_le_bytes());
        id.extend([0x56, 0x8d, 0x7e, 0x84, 0xd5, 0x11, 0x93, 0x5a, 0x50, 0xf8, 0x57, 0xc1, 0, 0]);
        json!({"id":id,"value":value})
    };
    let params = vec![
        guid(0x81a1, 0.59),
        guid(0x81a9, 1.0),
        guid(0x81aa, 100.0),
        guid(0x81ab, -100.0),
        guid(0x81b2, 1.0),
        guid(0x81b3, 50.0),
        guid(0x81b4, 75.0),
        guid(0x81b7, 1.0),
        guid(0x81ba, 30.0),
        guid(0x81bc, 1.0),
        guid(0x81ed, 1.0),
        guid(0x81fc, 255.0),
    ];
    let leaf = json!({"class":"SCLP","media_kind":1,"length":12,"rate":rate(),"source_id":id(1),"source_track":1,"start":0});
    timeline["tracks"][1]["component"] = json!({"class":"TKFX","media_kind":1,"length":12,"rate":rate(),"unsupported":"component_semantics",
    "properties":{"effect_id":"EFF2_SBLEND","mc_mode":0,"numeric_parameters":params,"attributes":{"_EFFECT_PLUGIN_NAME":"3DWarp"}},
    "input_tracks":[
        {"index":1,"component":{"class":"SEQU","media_kind":1,"length":12,"rate":rate(),"components":[{"class":"FILL","media_kind":1,"length":12,"rate":rate()}]}},
        {"index":2,"component":{"class":"SEQU","media_kind":1,"length":12,"rate":rate(),"components":[{"class":"FILL","media_kind":1,"length":0,"rate":rate()},leaf,{"class":"FILL","media_kind":1,"length":0,"rate":rate()}]}}
    ]});
    let (mut p, bin, _) = imported(&v);
    for item in p.items.values_mut() {
        if let Some(m) = item.as_media_mut() {
            m.info.kind = filmcraft_media::MediaKind::Movie;
            m.info.video = Some(filmcraft_media::VideoStreamInfo {
                width: 1920,
                height: 1080,
                frame_rate: FrameRate::FPS_24,
                par: (1, 1),
                codec: "test".into(),
                pixel_format: "test".into(),
                color: filmcraft_color::ColorInfo::REC709,
                has_alpha: false,
                bitrate: None,
                hdr: None,
            });
            m.info.audio_streams.clear();
            m.media = MediaRef::Components {
                channels: Vec::new(),
                picture: Some(PictureComponent {
                    format_pending: false,
                    track: 1,
                    file_package: [1; 32],
                    path: None,
                    source_start: Tick::ZERO,
                    length: m.info.duration,
                }),
            };
        }
    }
    let bytes = serde_json::to_vec(&v).unwrap();
    let plan = seq::plan(&p, &bytes, bin).unwrap();
    let ids = seq::apply(&mut p, plan).unwrap();
    let clip = &p.sequence(ids[0]).unwrap().video_tracks[0].items[0];
    let transform = clip.effects.iter().find(|e| e.effect == "transform").unwrap();
    assert_eq!(transform.vec2_at("position", Tick::ZERO), filmcraft_geom::Vec2::new(1152.0, 648.0));
    assert_eq!(transform.f64_at("scale_width", Tick::ZERO), 50.0);
    assert_eq!(transform.f64_at("scale_height", Tick::ZERO), 75.0);
    assert_eq!(transform.f64_at("rotation", Tick::ZERO), 30.0);

    // The same wrapper with X rotation is no longer a 2D effect. It must stay explicit.
    v["bins"][0]["mobs"][2]["tracks"][1]["component"]["properties"]["numeric_parameters"].as_array_mut().unwrap().push(guid(0x81b8, 30.0));
    let rejected = seq::plan(&p, &serde_json::to_vec(&v).unwrap(), bin).unwrap();
    let error = seq::apply(&mut p, rejected).unwrap_err().to_string();
    assert!(error.contains("not translated"), "{error}");
}
#[test]
fn unplaced_subclip_still_resolves_for_timeline() {
    let mut v = fixture();
    v["bins"][0]["mobs"][1]["user_placed"] = json!(false);
    let (p, bin, _) = imported(&v);
    let plan = seq::plan_best_effort(&p, &serde_json::to_vec(&v).unwrap(), bin).unwrap();
    assert!(!plan.warnings.iter().any(|w| w.contains("source master is missing")), "{:?}", plan.warnings);
}
#[test]
fn subclip_audio_track_routes_through_parent_mapping() {
    let mut v = fixture();
    // Subclip A1 contains master A2, while the sequence cuts from subclip A1.
    v["bins"][0]["mobs"][1]["tracks"][0]["component"]["source_track"] = json!(2);
    let (mut p, bin, _) = imported(&v);
    let plan = seq::plan_best_effort(&p, &serde_json::to_vec(&v).unwrap(), bin).unwrap();
    let (ids, rejected) = seq::apply_supported(&mut p, plan).unwrap();
    assert!(rejected.is_empty());
    let q = p.sequence(ids[0]).unwrap();
    assert_eq!(q.audio_tracks[0].items[0].source_channels, vec![1], "subclip A1 resolves to master A2");
}
#[test]
fn unsupported_only_bins_report_omissions() {
    for usage in [3, 4] {
        let mut v = fixture();
        v["bins"][0]["mobs"].as_array_mut().unwrap().truncate(2);
        v["bins"][0]["mobs"][1]["usage_code"] = json!(usage);
        let plan = cat::plan_scoped(&serde_json::to_vec(&v).unwrap(), cat::TransferMode::Sequences).unwrap();
        let mut p = Project::default();
        let bins = cat::apply(&mut p, plan.clone(), None).unwrap();
        // Engine calls the sequence planner only if this count is positive.
        assert_eq!(bins[0].compositions, 0);
        let kind = if usage == 3 { "saved effect template" } else { "group clip" };
        assert!(plan.warnings.iter().any(|w| w.contains(kind) && w.contains("Sub was not imported")), "{kind}: {:?}", plan.warnings);
    }
}
#[test]
fn non_linear_subclip_is_not_silently_flattened() {
    let mut v = fixture();
    let sub = &mut v["bins"][0]["mobs"][1];
    sub["attributes"] = json!({});
    // Two repeats of the same 24 frames are not a contiguous 48-frame range.
    sub["tracks"][0]["component"] = json!({"class":"SEQU","media_kind":2,"length":48,"rate":rate(),"components":[leaf(1,1,0,24),leaf(1,1,0,24)]});
    let (p, _, warnings) = imported(&v);
    assert!(!p.items.values().any(|i| matches!(i.kind, ItemKind::Subclip { .. })), "nonlinear source silently imported as linear; warnings: {warnings:?}");
}
#[test]
fn plain_subclip_control() {
    let v = fixture();
    let (mut p, bin, _) = imported(&v);
    let plan = seq::plan_best_effort(&p, &serde_json::to_vec(&v).unwrap(), bin).unwrap();
    let (ids, rejected) = seq::apply_supported(&mut p, plan).unwrap();
    assert!(rejected.is_empty());
    let q = p.sequence(ids[0]).unwrap();
    assert_eq!(q.audio_tracks[0].items[0].source_in, FrameRate::FPS_24.tick_of(30));
}

#[test]
fn referenced_subclips_keep_membership_and_survive_project_roundtrip() {
    let mut v = fixture();
    v["bins"][0]["mobs"][1]["user_placed"] = json!(false);
    // An unrelated unplaced subclip must not be imported merely because it exists in the file.
    let mut unused = v["bins"][0]["mobs"][1].clone();
    unused["id"] = id(4);
    unused["name"] = json!("Unused");
    v["bins"][0]["mobs"].as_array_mut().unwrap().push(unused);
    let bytes = serde_json::to_vec(&v).unwrap();
    let plan = cat::plan_scoped(&bytes, cat::TransferMode::Sequences).unwrap();
    let preview = cat::preview(&Project::default(), &plan, None).unwrap();
    assert_eq!((preview[0].subclips, preview[0].referenced_subclips), (0, 1));
    let (p, bin, _) = imported(&v);
    let mut p: Project = serde_json::from_slice(&serde_json::to_vec(&p).unwrap()).unwrap();
    assert!(!p.items.values().any(|i| i.name == "Unused"));
    let sub = p.items.values().find(|i| i.name == "Sub").unwrap();
    let evidence: Value = serde_json::from_str(&sub.metadata[cat::SOURCE_KEY]).unwrap();
    assert_eq!(evidence["placed"], false);
    let b = p.root.find_bin(bin).unwrap();
    assert!(!b.children.iter().any(|e| matches!(e,filmcraft_project::BinEntry::Item(i) if *i == sub.id)));
    assert!(b.children.iter().any(|e| matches!(e,filmcraft_project::BinEntry::Bin(b) if b.name == cat::REFERENCED_BIN)));
    let plan = seq::plan_best_effort(&p, &bytes, bin).unwrap();
    assert!(!plan.warnings.iter().any(|w| w.contains("placeholder")), "{:?}", plan.warnings);
    let (ids, rejected) = seq::apply_supported(&mut p, plan).unwrap();
    assert!(rejected.is_empty());
    assert_eq!(p.sequence(ids[0]).unwrap().audio_tracks[0].items[0].source_in, FrameRate::FPS_24.tick_of(30));
}

#[test]
fn uncertain_subclip_structures_are_reported_without_a_partial_import() {
    let base = fixture();
    let original = base["bins"][0]["mobs"][1]["tracks"][0]["component"].clone();
    let wrapper = |c: Value| json!({"class":"SEQU","media_kind":2,"length":240,"rate":rate(),"components":[c]});
    let mut deep = original.clone();
    for _ in 0..17 {
        deep = wrapper(deep);
    }
    let mut truncated = original.clone();
    truncated["truncated"] = json!(true);
    let mut missing_start = original.clone();
    missing_start.as_object_mut().unwrap().remove("start");
    let mut mixed_rate = original.clone();
    mixed_rate["rate"] = json!({"numerator":48,"denominator":1});
    let mut wrong_length = wrapper(original.clone());
    wrong_length["length"] = json!(120);
    let unknown_effect = json!({"class":"TKFX","media_kind":2,"length":240,"rate":rate(),"input_tracks":[{"component":original}]});
    let mut too_deep = base.clone();
    too_deep["bins"][0]["mobs"][1]["tracks"][0]["component"] = deep;
    assert!(cat::plan_scoped(&serde_json::to_vec(&too_deep).unwrap(), cat::TransferMode::Sequences).is_err());
    for component in [truncated, missing_start, mixed_rate, wrong_length, unknown_effect] {
        let mut v = base.clone();
        v["bins"][0]["mobs"][1]["tracks"][0]["component"] = component;
        let (p, _, warnings) = imported(&v);
        assert!(!p.items.values().any(|i| matches!(i.kind, ItemKind::Subclip { .. })));
        assert!(warnings.iter().any(|w| w.contains("subclip Sub was not imported")), "{warnings:?}");
    }
    // A timing-preserving wrapper remains supported.
    let mut v = base.clone();
    v["bins"][0]["mobs"][1]["tracks"][0]["component"] = wrapper(base["bins"][0]["mobs"][1]["tracks"][0]["component"].clone());
    let (p, _, _) = imported(&v);
    assert!(p.items.values().any(|i| matches!(i.kind, ItemKind::Subclip { .. })));
}

#[test]
fn missing_and_ambiguous_subclip_tracks_never_select_a_parent_channel() {
    for duplicate in [false, true] {
        let mut v = fixture();
        if duplicate {
            let mut track = v["bins"][0]["mobs"][1]["tracks"][0].clone();
            track["component"]["source_track"] = json!(2);
            v["bins"][0]["mobs"][1]["tracks"].as_array_mut().unwrap().push(track);
        } else {
            // The timeline asks for A2, but the subclip only has A1. Parent A2 must not authorize it.
            v["bins"][0]["mobs"][2]["tracks"][1]["component"]["components"][0]["source_track"] = json!(2);
        }
        let (p, bin, _) = imported(&v);
        let plan = seq::plan_best_effort(&p, &serde_json::to_vec(&v).unwrap(), bin).unwrap();
        assert!(plan.warnings.iter().any(|w| w.contains("placeholder")), "{:?}", plan.warnings);
    }
}

#[test]
#[ignore = "set FILMCRAFT_AVID_SUBCLIP_FIXTURES to a private manifest of paths and expected subclip counts"]
fn native_subclip_fixture_counts_and_unchanged_inputs() {
    let manifest: Value = serde_json::from_slice(&std::fs::read(std::env::var("FILMCRAFT_AVID_SUBCLIP_FIXTURES").unwrap()).unwrap()).unwrap();
    for entry in manifest.as_array().unwrap() {
        let path = entry["path"].as_str().unwrap();
        let before = std::fs::read(path).unwrap();
        let document = cat::native_document(&before, path).unwrap();
        let plan = cat::plan_scoped(&serde_json::to_vec(&document).unwrap(), cat::TransferMode::Sequences).unwrap();
        let preview = cat::preview(&Project::default(), &plan, None).unwrap();
        assert_eq!(preview[0].subclips as u64, entry["subclips"].as_u64().unwrap(), "{path}: {:?}", plan.warnings);
        assert_eq!(before, std::fs::read(path).unwrap(), "source changed: {path}");
    }
}

#[test]
fn subclip_range_cannot_start_before_its_component() {
    let mut v = fixture();
    // Attributes select frames 24..72, but the component only exposes frames 48..240.
    let c = &mut v["bins"][0]["mobs"][1]["tracks"][0]["component"];
    c["start"] = json!(48);
    c["length"] = json!(192);
    let (p, _, warnings) = imported(&v);
    assert!(!p.items.values().any(|i| matches!(i.kind, ItemKind::Subclip { .. })), "{warnings:?}");
    assert!(warnings.iter().any(|w| w.contains("subclip Sub was not imported")), "{warnings:?}");
}

#[test]
fn shifted_subclip_tracks_are_not_treated_as_aligned() {
    for position in [json!(12), json!(-12), json!("invalid")] {
        let mut v = fixture();
        v["bins"][0]["mobs"][1]["tracks"][0]["properties"] = json!({"start_pos":position});
        let (p, _, warnings) = imported(&v);
        assert!(!p.items.values().any(|i| matches!(i.kind, ItemKind::Subclip { .. })), "{warnings:?}");
        assert!(warnings.iter().any(|w| w.contains("subclip Sub was not imported")), "{warnings:?}");
    }
}

#[test]
fn invalid_saved_subclip_ranges_become_reported_placeholders() {
    for (start, duration) in [(Tick(-1000), FrameRate::FPS_24.tick_of(48)), (Tick::ZERO, FrameRate::FPS_24.tick_of(300)), (Tick::MAX, Tick::MAX)] {
        let v = fixture();
        let (mut p, bin, _) = imported(&v);
        let sub = p.items.values_mut().find(|i| i.name == "Sub").unwrap();
        if let ItemKind::Subclip { range, .. } = &mut sub.kind {
            *range = filmcraft_time::TimeRange::new(start, duration);
        }
        let plan = seq::plan_best_effort(&p, &serde_json::to_vec(&v).unwrap(), bin).unwrap();
        assert!(plan.warnings.iter().any(|w| w.contains("placeholder")), "{start:?}: {:?}", plan.warnings);
    }
}
