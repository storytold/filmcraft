//! Native sequence transfer from a standalone-reader catalog, into an already imported bin.
use filmcraft_interchange::avid_sequence::{self as adapter, Plan};
use filmcraft_project::BinId;
use serde_json::{Value, json};

use crate::{EngineError, Result, Session};
fn failure(e: impl std::fmt::Display) -> EngineError {
    EngineError::Other(e.to_string())
}
fn prepare(s: &Session, p: &Value) -> Result<Plan> {
    let bin = BinId(p.get("bin").and_then(Value::as_u64).ok_or_else(|| failure("Avid sequences require an imported `bin` ID"))?);
    let bytes = crate::avid_catalog::read_report(s, p)?;
    adapter::plan(&s.project, &bytes, bin).map_err(failure)
}
pub fn preview(s: &Session, p: &Value) -> Result<Value> {
    let plan = prepare(s, p)?;
    serde_json::to_value(&plan).map_err(failure)
}
pub fn import(s: &mut Session, p: &Value) -> Result<Value> {
    let plan = prepare(s, p)?;
    let warnings = plan.warnings.clone();
    let ids = s.edit("Import Avid Sequences", move |project, state| {
        let ids = adapter::apply(project, plan).map_err(failure)?;
        state.active_sequence = ids.first().copied();
        if let Some(id) = state.active_sequence {
            state.open_sequences.push(id);
        }
        Ok(ids)
    })?;
    Ok(json!({"sequences":ids,"warnings":warnings}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Services;
    use filmcraft_interchange::avid_catalog as adapter_catalog;
    use filmcraft_project::{AudioChannels, AudioComponent, MediaRef};
    use filmcraft_time::{FrameRate, Tick};
    use std::sync::Arc;

    struct Report(Vec<u8>);
    impl Services for Report {
        fn read_file(&self, _: &str) -> std::io::Result<Vec<u8>> {
            Ok(self.0.clone())
        }
        fn write_file(&self, _: &str, _: &[u8]) -> std::io::Result<()> {
            Err(std::io::Error::other("unexpected write"))
        }
    }
    fn fixture() -> Value {
        let mut v = crate::avid_catalog_tests::fixture();
        let rate = json!({"numerator":24,"denominator":1});
        let leaf = |track| json!({"class":"SCLP","length":48,"rate":rate,"media_kind":2,"source_id":v["bins"][0]["catalog"]["clips"][0]["id"],"source_track":track,"start":24});
        let fill = |n| json!({"class":"FILL","length":n,"rate":rate,"media_kind":2});
        let combiner = json!({"class":"TKFX","length":48,"rate":rate,"media_kind":2,"unsupported":"component_semantics","properties":{"effect_id":"EFF2_AUDIO_CHANNEL_COMBINER"},"input_tracks":[{"component":leaf(2)},{"component":leaf(1)}]});
        let seq = json!({"kind":"CompositionMob","name":"Cuts 🎵","live":true,"user_placed":true,"id":{"raw":"02".repeat(32),"byte_order":"little"},"length":96,"rate":rate,
            "attributes":{"SEQUERNCE_FORMAT_STRING":"HD 1080p/24","_COLOR_R":25856,"_COLOR_G":50432,"_COLOR_B":21248},
            "tracks":[{"index":1,"component":{"class":"TCCP","media_kind":3,"length":96,"rate":rate,"start":86400,"fps":24,"flags":0}},
            {"index":1,"component":{"class":"SEQU","media_kind":2,"length":96,"rate":rate,"components":[fill(24),leaf(1),fill(24)]}},
            {"index":3,"properties":{"attributes":{"_TRACK_FORMAT":2}},"component":{"class":"SEQU","media_kind":2,"length":96,"rate":rate,"components":[fill(48),combiner]}}]});
        v["bins"][0]["mobs"].as_array_mut().unwrap().push(seq);
        v
    }
    fn session(v: &Value) -> (Session, BinId) {
        let mut s = Session::new(Arc::new(Report(serde_json::to_vec(v).unwrap())));
        let r = s.execute("file.importAvidCatalog", json!({"path":"test"})).unwrap();
        let bin = BinId(r["bins"][0]["bin"].as_u64().unwrap());
        let id = *s.project.items.keys().next().unwrap();
        let m = Arc::make_mut(&mut s.project).item_mut(id).unwrap().as_media_mut().unwrap();
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
        (s, bin)
    }
    #[test]
    fn sequence_transfer_keeps_offsets_gaps_channels_color_and_undo() {
        let (mut s, bin) = session(&fixture());
        let before = s.project.clone();
        let n = s.history.undo.len();
        let preview = s.execute("file.previewAvidSequences", json!({"path":"test","bin":bin})).unwrap();
        assert_eq!(preview["rejected"], json!([]));
        assert_eq!(s.project, before);
        assert_eq!(s.history.undo.len(), n);
        let out = s.execute("file.importAvidSequences", json!({"path":"test","bin":bin})).unwrap();
        let id = filmcraft_project::ItemId(out["sequences"][0].as_u64().unwrap());
        let q = s.project.sequence(id).unwrap();
        assert_eq!(q.start_timecode, 86400);
        assert_eq!(q.duration(), Tick::from_units(96, 24));
        assert_eq!(q.audio_tracks.len(), 3);
        assert!(q.audio_tracks[1].items.is_empty());
        let a = &q.audio_tracks[0].items[0];
        assert_eq!((a.start, a.source_in, a.duration), (Tick::from_units(24, 24), Tick::from_units(24, 24), Tick::from_units(48, 24)));
        assert_eq!(a.source_channels, vec![0]);
        assert_eq!(q.audio_tracks[2].channels, AudioChannels::Stereo);
        assert_eq!(q.audio_tracks[2].items[0].source_channels, vec![1, 0], "combiner input order is routing, not sorted track numbers");
        let raw: Value = serde_json::from_str(&s.project.item(id).unwrap().metadata[adapter::SEQUENCE_KEY]).unwrap();
        assert_eq!(raw["composition"]["attributes"]["_COLOR_G"], 50432);
        let after = s.project.clone();
        let decoded = filmcraft_format::decode(&filmcraft_format::encode(&s.project, false)).unwrap();
        assert_eq!(decoded.project, *after);
        assert_eq!(s.history.undo.len(), n + 1);
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(s.project, before);
        s.execute("edit.redo", json!({})).unwrap();
        assert_eq!(s.project, after);
    }
    fn set_sequence_clock(v: &mut Value, rate: &Value) {
        match v {
            Value::Object(map) => {
                if map.contains_key("rate") {
                    map.insert("rate".into(), rate.clone());
                }
                for (key, value) in map {
                    if key != "rate" {
                        set_sequence_clock(value, rate);
                    }
                }
            }
            Value::Array(values) => {
                for value in values {
                    set_sequence_clock(value, rate);
                }
            }
            _ => {}
        }
    }
    #[test]
    fn sequence_formats_preserve_raster_rational_clock_cuts_and_timecode() {
        for (label, n, d, exact, size) in [
            ("HD 1080p/23.976", 23976, 1000, FrameRate::FPS_23_976, (1920, 1080)),
            ("HD 720p/25", 25, 1, FrameRate::new(25, 1), (1280, 720)),
            ("UHD/29.97", 2997, 100, FrameRate::FPS_29_97, (3840, 2160)),
            ("4K 4096x2160p/24", 24, 1, FrameRate::FPS_24, (4096, 2160)),
            ("UHD/50", 50, 1, FrameRate::new(50, 1), (3840, 2160)),
            ("UHD/59.94", 5994, 100, FrameRate::FPS_59_94, (3840, 2160)),
        ] {
            let mut v = fixture();
            let seq = &mut v["bins"][0]["mobs"][1];
            seq["attributes"]["SEQUERNCE_FORMAT_STRING"] = json!(label);
            set_sequence_clock(seq, &json!({"numerator":n,"denominator":d}));
            seq["tracks"][0]["component"]["fps"] = json!(exact.timecode_base());
            let raw = seq.clone();
            let (mut s, bin) = session(&v);
            let out = s.execute("file.importAvidSequences", json!({"path":"test","bin":bin})).unwrap();
            let id = filmcraft_project::ItemId(out["sequences"][0].as_u64().unwrap());
            let q = s.project.sequence(id).unwrap();
            assert_eq!((q.settings.width, q.settings.height), size);
            assert_eq!(q.settings.frame_rate, exact);
            assert_eq!(q.start_timecode, 86400);
            assert_eq!(q.duration(), exact.tick_of(96));
            let c = &q.audio_tracks[0].items[0];
            assert_eq!((c.start, c.source_in, c.duration), (exact.tick_of(24), exact.tick_of(24), exact.tick_of(48)));
            assert_eq!(q.audio_tracks[2].items[0].source_channels, vec![1, 0]);
            let evidence: Value = serde_json::from_str(&s.project.item(id).unwrap().metadata[adapter::SEQUENCE_KEY]).unwrap();
            assert_eq!(evidence["composition"], raw);
            assert_eq!(filmcraft_format::decode(&filmcraft_format::encode(&s.project, false)).unwrap().project, *s.project);
        }
    }
    #[test]
    fn sequence_drop_frame_preserves_frame_count_and_saved_display_mode() {
        for (label, n, d, exact, start) in [("HD 1080p/29.97", 2997, 100, FrameRate::FPS_29_97, 107892), ("UHD/59.94", 5994, 100, FrameRate::FPS_59_94, 215784)]
        {
            for flags in [0, 1, 2, 3, u32::MAX] {
                let mut v = fixture();
                let seq = &mut v["bins"][0]["mobs"][1];
                seq["attributes"]["SEQUERNCE_FORMAT_STRING"] = json!(label);
                set_sequence_clock(seq, &json!({"numerator":n,"denominator":d}));
                let tc = &mut seq["tracks"][0]["component"];
                tc["fps"] = json!(exact.timecode_base());
                tc["flags"] = json!(flags);
                tc["start"] = json!(start);
                let (mut s, bin) = session(&v);
                let before = s.project.clone();
                let result = s.execute("file.importAvidSequences", json!({"path":"test","bin":bin}));
                if flags > 1 {
                    assert!(result.is_err());
                    assert_eq!(s.project, before);
                    continue;
                }
                let id = filmcraft_project::ItemId(result.unwrap()["sequences"][0].as_u64().unwrap());
                let q = s.project.sequence(id).unwrap();
                assert_eq!(q.settings.drop_frame, flags == 1);
                assert_eq!(q.start_timecode, start);
                assert_eq!(q.duration(), exact.tick_of(96));
                if flags == 1 {
                    assert_eq!(filmcraft_time::format_timecode_frames(q.start_timecode, exact, true), "01;00;00;00");
                }
                assert_eq!(filmcraft_format::decode(&filmcraft_format::encode(&s.project, false)).unwrap().project, *s.project);
                let after = s.project.clone();
                s.execute("edit.undo", json!({})).unwrap();
                assert_eq!(s.project, before);
                s.execute("edit.redo", json!({})).unwrap();
                assert_eq!(s.project, after);
            }
        }
        let mut v = fixture();
        v["bins"][0]["mobs"][1]["tracks"][0]["component"]["flags"] = json!(1);
        let (mut s, bin) = session(&v);
        assert!(s.execute("file.importAvidSequences", json!({"path":"test","bin":bin})).is_err(), "24 fps cannot use drop-frame");
    }
    #[test]
    fn fractional_sequence_keeps_long_gap_without_drift() {
        let mut v = fixture();
        let seq = &mut v["bins"][0]["mobs"][1];
        seq["attributes"]["SEQUERNCE_FORMAT_STRING"] = json!("HD 1080p/29.97");
        set_sequence_clock(seq, &json!({"numerator":2997,"denominator":100}));
        let lead = 108000i64;
        let leaf = seq["tracks"][1]["component"]["components"][1].clone();
        let r = json!({"numerator":2997,"denominator":100});
        seq["length"] = json!(lead + 48);
        seq["tracks"] = json!([
            {"index":1,"component":{"class":"TCCP","length":lead+48,"rate":r,"media_kind":3,"fps":30,"start":107892,"flags":0}},
            {"index":1,"component":{"class":"SEQU","length":lead+48,"rate":r,"media_kind":2,"components":[
                {"class":"FILL","length":lead,"rate":r,"media_kind":2},leaf]}}
        ]);
        let (mut s, bin) = session(&v);
        let out = s.execute("file.importAvidSequences", json!({"path":"test","bin":bin})).unwrap();
        let q = s.project.sequence(filmcraft_project::ItemId(out["sequences"][0].as_u64().unwrap())).unwrap();
        assert_eq!(q.audio_tracks[0].items[0].start, FrameRate::FPS_29_97.tick_of(lead));
        assert_eq!(q.duration(), FrameRate::FPS_29_97.tick_of(lead + 48));
        assert_eq!(q.start_timecode, 107892);
    }
    #[test]
    fn a_sequence_resolves_sources_its_bin_references_without_placing() {
        // Avid keeps a cut bin's masters in the file without placing them in the bin, so a
        // sequence-only bin must still produce items: media binds to an item, never to a
        // timeline clip, and an unbound clip could never be relinked.
        let mut v = fixture();
        for pointer in ["/bins/0/catalog/clips/0/user_placed", "/bins/0/mobs/0/user_placed"] {
            *v.pointer_mut(pointer).unwrap() = json!(false);
        }
        let bytes = serde_json::to_vec(&v).unwrap();
        let mut s = Session::new(Arc::new(Report(bytes.clone())));
        let plan = adapter_catalog::plan_scoped(&bytes, adapter_catalog::TransferMode::Sequences).unwrap();
        assert!(plan.warnings.iter().any(|w| w.contains("referenced by this bin's sequences")));
        let bins = adapter_catalog::apply(Arc::make_mut(&mut s.project), plan, None).unwrap();
        assert_eq!((bins[0].clips, bins[0].referenced, bins[0].compositions), (0, 1, 1));

        // The bin still lists what Avid showed; the source lives in the referenced sub-bin.
        let bin = s.project.root.find_bin(bins[0].bin).unwrap();
        assert_eq!(bin.children.iter().filter(|c| matches!(c, filmcraft_project::BinEntry::Item(_))).count(), 0);
        let filmcraft_project::BinEntry::Bin(sub) = &bin.children[0] else { panic!("referenced sub-bin missing") };
        assert_eq!(sub.name, adapter_catalog::REFERENCED_BIN);
        assert_eq!(sub.children.len(), 1);
        let filmcraft_project::BinEntry::Item(source) = sub.children[0] else { panic!("referenced source missing") };
        let provenance: Value = serde_json::from_str(&s.project.item(source).unwrap().metadata[adapter_catalog::SOURCE_KEY]).unwrap();
        assert_eq!(provenance["placed"], json!(false), "a referenced source is never reported as bin contents");

        // Bound like any imported master, it carries a real edit instead of a placeholder.
        let m = Arc::make_mut(&mut s.project).item_mut(source).unwrap().as_media_mut().unwrap();
        let duration = m.info.duration;
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
                    length: duration,
                })
                .collect(),
        };
        m.offline = false;
        let plan = adapter::plan_best_effort(&s.project, &bytes, bins[0].bin).unwrap();
        assert!(!plan.warnings.iter().any(|w| w.contains("source master is missing from selected bin")));
        let (ids, rejected) = adapter::apply_supported(Arc::make_mut(&mut s.project), plan).unwrap();
        assert!(rejected.is_empty());
        let q = s.project.sequence(ids[0]).unwrap();
        let clip = &q.audio_tracks[0].items[0];
        assert_eq!(clip.item, source);
        assert_eq!(clip.start, FrameRate::FPS_24.tick_of(24));
        assert_eq!(clip.duration, FrameRate::FPS_24.tick_of(48));
        assert!(!s.project.item(clip.item).unwrap().as_media().unwrap().offline);
    }
    #[test]
    fn placed_compositions_that_are_not_timelines_are_named_rather_than_converted() {
        // A bin holds subclips, saved effect templates, group clips and motion effects alongside
        // its sequences. Each is a composition mob, and converting one yields a sequence item
        // that is not a sequence.
        let mut v = fixture();
        let rate = json!({"numerator":24,"denominator":1});
        for (usage, name) in [(3, "Resize"), (4, "Angles (2cam)"), (6, "Fast (48.00 FPS)")] {
            let mob = json!({"kind":"CompositionMob","name":name,"live":true,"user_placed":true,
                "id":{"raw":format!("{usage:02}").repeat(32),"byte_order":"little"},"length":48,"rate":rate,"usage_code":usage,
                "tracks":[{"index":1,"component":{"class":"SEQU","media_kind":1,"length":48,"rate":rate,"components":[]}}]});
            v["bins"][0]["mobs"].as_array_mut().unwrap().push(mob);
        }
        let (mut s, bin) = session(&v);
        let plan = adapter::plan_best_effort(&s.project, &serde_json::to_vec(&v).unwrap(), bin).unwrap();
        for kind in ["saved effect templates", "group clips", "motion effect clips"] {
            assert!(plan.warnings.iter().any(|w| w.contains(kind)), "{kind} unreported in {:?}", plan.warnings);
        }
        let (ids, rejected) = adapter::apply_supported(Arc::make_mut(&mut s.project), plan).unwrap();
        assert!(rejected.is_empty(), "none of them is rejected either: {rejected:?}");
        // Only the fixture's own sequence becomes a sequence.
        for id in &ids {
            let name = s.project.item(*id).map(|i| i.name.clone()).unwrap_or_default();
            assert!(!["Resize", "Angles (2cam)", "Fast (48.00 FPS)"].contains(&name.as_str()), "{name} was converted into a sequence");
        }
        assert_eq!(ids.len(), 1);
    }
    #[test]
    fn a_sequence_cutting_from_a_subclip_resolves_it_in_the_masters_media_time() {
        // Cut bins reference subclips, not only masters. A clip of a subclip is expressed in the
        // parent's media time, so the subclip's own offset has to be added to the component's.
        let mut v = fixture();
        let master = v["bins"][0]["catalog"]["clips"][0].clone();
        let rate = json!({"numerator":24,"denominator":1});
        // A subclip over frames 24..72 of its master, and a sequence cutting 12 frames from 6
        // frames into that subclip: 30 frames into the master.
        let sub = json!({"kind":"CompositionMob","name":"Trim.sub.01","live":true,"user_placed":true,
            "id":{"raw":"06".repeat(32),"byte_order":"little"},"length":96,"rate":master["rate"],"usage_code":2,
            "attributes":{"_START":24,"_END":72},
            "tracks":[{"index":1,"component":{"class":"SCLP","media_kind":2,"length":96,"rate":master["rate"],
                "source_id":master["id"],"source_track":1,"start":0}}]});
        v["bins"][0]["mobs"].as_array_mut().unwrap().push(sub.clone());
        let cut = json!({"class":"SCLP","length":12,"rate":rate,"media_kind":2,"source_id":sub["id"],"source_track":1,"start":6});
        let seq = json!({"kind":"CompositionMob","name":"Cut from a subclip","live":true,"user_placed":true,
            "id":{"raw":"07".repeat(32),"byte_order":"little"},"length":12,"rate":rate,
            "attributes":{"SEQUERNCE_FORMAT_STRING":"HD 1080p/24"},
            "tracks":[{"index":1,"component":{"class":"TCCP","media_kind":3,"length":12,"rate":rate,"start":86400,"fps":24,"flags":0}},
                {"index":1,"component":{"class":"SEQU","media_kind":2,"length":12,"rate":rate,"components":[cut]}}]});
        v["bins"][0]["mobs"].as_array_mut().unwrap().push(seq);
        let (mut s, bin) = session(&v);
        let plan = adapter::plan_best_effort(&s.project, &serde_json::to_vec(&v).unwrap(), bin).unwrap();
        assert!(!plan.warnings.iter().any(|w| w.contains("source master is missing")), "{:?}", plan.warnings);
        let (ids, rejected) = adapter::apply_supported(Arc::make_mut(&mut s.project), plan).unwrap();
        assert!(rejected.is_empty(), "{rejected:?}");
        let id = *ids.iter().find(|id| s.project.item(**id).is_some_and(|i| i.name == "Cut from a subclip")).expect("the sequence that cuts from the subclip");
        let q = s.project.sequence(id).expect("a sequence");
        let clip = &q.audio_tracks[0].items[0];
        // The clip references the subclip itself, and reads 24 + 6 = 30 frames into the master.
        assert!(matches!(s.project.item(clip.item).map(|i| &i.kind), Some(filmcraft_project::ItemKind::Subclip { .. })));
        assert_eq!(clip.source_in, FrameRate::FPS_24.tick_of(30), "the subclip offset is added to the cut");
        assert_eq!(clip.duration, FrameRate::FPS_24.tick_of(12));
        assert!(!s.project.item(clip.item).and_then(|i| i.as_media()).is_some_and(|m| m.offline));
    }
    #[test]
    fn a_multi_component_auxiliary_track_warns_without_placeholdering_the_sequence() {
        // Feature-film bins carry kind-7 auxiliary tracks holding more than the single filler
        // component this reader accepts. That decision belongs to the track, so the real edits
        // and timing survive instead of collapsing into one whole-sequence placeholder.
        let mut v = fixture();
        let r = json!({"numerator":24,"denominator":1});
        let filler = |n| json!({"class":"FILL","length":n,"rate":r,"media_kind":7});
        v["bins"][0]["mobs"][1]["tracks"]
            .as_array_mut()
            .unwrap()
            .push(json!({"index":4,"component":{"class":"SEQU","media_kind":7,"length":96,"rate":r,"components":[filler(48),filler(48)]}}));
        let (mut s, bin) = session(&v);
        let plan = adapter::plan_best_effort(&s.project, &serde_json::to_vec(&v).unwrap(), bin).unwrap();
        assert!(plan.warnings.iter().any(|w| w.contains("Auxiliary track 4 was not converted")));
        assert!(!plan.warnings.iter().any(|w| w.contains("Entire sequence represented by an offline placeholder")));
        let (ids, rejected) = adapter::apply_supported(Arc::make_mut(&mut s.project), plan).unwrap();
        assert!(rejected.is_empty());
        let q = s.project.sequence(ids[0]).unwrap();
        assert_eq!(q.duration(), FrameRate::FPS_24.tick_of(96));
        assert_eq!(q.start_timecode, 86400);
        let clip = &q.audio_tracks[0].items[0];
        assert_eq!(clip.start, FrameRate::FPS_24.tick_of(24));
        assert_eq!(clip.duration, FrameRate::FPS_24.tick_of(48));
        // Strict conversion still refuses the unrecognized auxiliary shape, naming the track
        // rather than the component list it was read from.
        let strict = serde_json::to_value(adapter::plan(&s.project, &serde_json::to_vec(&v).unwrap(), bin).unwrap()).unwrap();
        assert!(strict["sequences"].as_array().unwrap().is_empty());
        assert!(strict["rejected"][0].as_str().unwrap().contains("auxiliary kind-7 track"), "{strict:?}");
    }
    #[test]
    fn best_effort_placeholders_keep_fractional_clock_and_known_raster() {
        let mut v = fixture();
        let seq = &mut v["bins"][0]["mobs"][1];
        seq["attributes"]["SEQUERNCE_FORMAT_STRING"] = json!("UHD/23.976");
        set_sequence_clock(seq, &json!({"numerator":23976,"denominator":1000}));
        seq["tracks"][1]["component"]["components"][1]["class"] = json!("UNKNOWN");
        let (mut s, bin) = session(&v);
        let plan = adapter::plan_best_effort(&s.project, &serde_json::to_vec(&v).unwrap(), bin).unwrap();
        let (ids, rejected) = adapter::apply_supported(Arc::make_mut(&mut s.project), plan).unwrap();
        assert!(rejected.is_empty());
        let q = s.project.sequence(ids[0]).unwrap();
        assert_eq!((q.settings.width, q.settings.height), (3840, 2160));
        assert_eq!(q.settings.frame_rate, FrameRate::FPS_23_976);
        assert_eq!(q.duration(), FrameRate::FPS_23_976.tick_of(96));
        let clip = &q.audio_tracks[0].items[0];
        assert_eq!(clip.start, FrameRate::FPS_23_976.tick_of(24));
        assert_eq!(clip.duration, FrameRate::FPS_23_976.tick_of(48));
        assert!(s.project.item(clip.item).unwrap().as_media().unwrap().offline);
    }
    #[test]
    fn sequence_same_rate_picture_preserves_trim_and_empty_lead() {
        use filmcraft_media::MediaSource;
        let mut v = fixture();
        let mut picture = v["bins"][0]["mobs"][1]["tracks"][1].clone();
        picture["component"]["media_kind"] = json!(1);
        for c in picture["component"]["components"].as_array_mut().unwrap() {
            c["media_kind"] = json!(1);
        }
        v["bins"][0]["mobs"][1]["tracks"].as_array_mut().unwrap().push(picture);
        let (mut s, bin) = session(&v);
        let m = Arc::make_mut(&mut s.project).items.values_mut().next().unwrap().as_media_mut().unwrap();
        let generator =
            filmcraft_media::generators::GeneratorSource::new(filmcraft_media::Generator::BlackVideo, 1920, 1080, FrameRate::FPS_24, m.info.duration);
        m.info.video = generator.info().video.clone();
        m.info.kind = filmcraft_media::MediaKind::Movie;
        let MediaRef::Components { picture, .. } = &mut m.media else { panic!("expected components") };
        *picture = Some(filmcraft_project::PictureComponent {
            format_pending: false,
            track: 1,
            file_package: [3; 32],
            path: None,
            source_start: Tick::ZERO,
            length: m.info.duration,
        });
        let out = s.execute("file.importAvidSequences", json!({"path":"test","bin":bin})).unwrap();
        let q = s.project.sequence(filmcraft_project::ItemId(out["sequences"][0].as_u64().unwrap())).unwrap();
        let clip = &q.video_tracks[0].items[0];
        assert_eq!((clip.start, clip.source_in, clip.duration), (Tick::from_units(24, 24), Tick::from_units(24, 24), Tick::from_units(48, 24)));
        assert!(clip.source_channels.is_empty());
    }

    fn rate_fixture() -> Value {
        let mut v = fixture();
        let output_rate = json!({"numerator":24,"denominator":1});
        let source_rate = json!({"numerator":2997,"denominator":100});
        let leaf = json!({"class":"SCLP","media_kind":1,"length":60,"start":30,"rate":source_rate,"source_track":1,"source_id":v["bins"][0]["catalog"]["clips"][0]["id"]});
        let motion = json!({"class":"SPED","media_kind":1,"length":48,"rate":output_rate,"unsupported":"component_semantics",
            "properties":{"effect_id":"EFF_TIMEWARP.MOTION_CTL","attributes":{"_MIXMATCH_MOTIONADAPTER":2},"phase_offset":0,"offset_adjust":0.0,"new_source_calculation":false,"mc_mode":0,"speed_ratio":[1001,1250]},
            "input_tracks":[{"index":1,"component":leaf}]});
        let spatial = json!({"class":"TKFX","media_kind":1,"length":48,"rate":output_rate,"unsupported":"component_semantics",
            "properties":{"effect_id":"INT_EFF_SPATIAL_ADAPTER"},"input_tracks":[{"index":1,"component":motion}]});
        let fill = json!({"class":"FILL","length":24,"media_kind":1,"rate":output_rate});
        v["bins"][0]["mobs"][1]["tracks"]
            .as_array_mut()
            .unwrap()
            .push(json!({"index":1,"component":{"class":"SEQU","length":96,"media_kind":1,"rate":output_rate,"components":[fill,spatial,fill]}}));
        v
    }
    fn rate_session(v: &Value) -> (Session, BinId) {
        use filmcraft_media::MediaSource;
        let (mut s, bin) = session(v);
        let m = Arc::make_mut(&mut s.project).items.values_mut().next().unwrap().as_media_mut().unwrap();
        let r = FrameRate::new(30000, 1001);
        let generator = filmcraft_media::generators::GeneratorSource::new(filmcraft_media::Generator::BlackVideo, 1920, 1080, r, m.info.duration);
        m.info.video = generator.info().video.clone();
        m.info.kind = filmcraft_media::MediaKind::Movie;
        let MediaRef::Components { picture, .. } = &mut m.media else { panic!("expected components") };
        *picture = Some(filmcraft_project::PictureComponent {
            format_pending: false,
            track: 1,
            file_package: [3; 32],
            path: None,
            source_start: Tick::ZERO,
            length: m.info.duration,
        });
        (s, bin)
    }
    #[test]
    fn sequence_rate_adapter_preserves_real_time_and_nonzero_source_in() {
        let (mut s, bin) = rate_session(&rate_fixture());
        let out = s.execute("file.importAvidSequences", json!({"path":"test","bin":bin})).unwrap();
        assert!(out["warnings"].as_array().unwrap().iter().any(|v| v.as_str().unwrap().contains("Frame Sampling")));
        let id = filmcraft_project::ItemId(out["sequences"][0].as_u64().unwrap());
        let q = s.project.sequence(id).unwrap();
        let c = &q.video_tracks[0].items[0];
        assert_eq!(c.start, Tick::from_units(24, 24));
        assert_eq!(c.duration, Tick::from_units(48, 24));
        assert_eq!(c.source_in, FrameRate::new(30000, 1001).tick_of(30));
        assert_eq!(c.speed, 1.0);
        assert!(c.scale_to_frame);
        assert_eq!(c.time_interpolation, filmcraft_project::TimeInterpolation::FrameSampling);
        let selected: Vec<_> = (0..48).map(|f| FrameRate::new(30000, 1001).frame_at(c.source_time_at(c.start + Tick::from_units(f, 24)))).collect();
        let expected: Vec<_> = (0..48).map(|f| 30 + f * 1250 / 1001).collect();
        assert_eq!(selected, expected, "output/source ratio must not become playback speed");
        let saved = filmcraft_format::decode(&filmcraft_format::encode(&s.project, false)).unwrap();
        assert_eq!(saved.project, *s.project);
        assert!(s.project.item(id).unwrap().metadata[adapter::SEQUENCE_KEY].contains("Frame Sampling"));
    }
    #[test]
    fn sequence_automatic_audio_adapter_keeps_sample_clock_and_channel() {
        for (label, n, d, exact) in [
            ("HD 1080p/23.976", 23976, 1000, FrameRate::FPS_23_976),
            ("HD 1080p/29.97", 2997, 100, FrameRate::FPS_29_97),
            ("2160p/25", 25, 1, FrameRate::new(25, 1)),
        ] {
            let mut v = fixture();
            let seq = &mut v["bins"][0]["mobs"][1];
            let mut leaf = seq["tracks"][1]["component"]["components"][1].clone();
            leaf["source_track"] = json!(2);
            leaf["length"] = json!((48 * 24 * exact.den + exact.num - 1) / exact.num);
            set_sequence_clock(seq, &json!({"numerator":n,"denominator":d}));
            seq["attributes"]["SEQUERNCE_FORMAT_STRING"] = json!(label);
            seq["tracks"][0]["component"]["fps"] = json!(exact.timecode_base());
            seq["tracks"].as_array_mut().unwrap().truncate(2);
            seq["length"] = json!(72);
            seq["tracks"][0]["component"]["length"] = json!(72);
            seq["tracks"][1]["component"]["length"] = json!(72);
            seq["tracks"][1]["component"]["components"].as_array_mut().unwrap().truncate(2);
            let ratio = FrameRate::new(exact.num, exact.den * 24);
            seq["tracks"][1]["component"]["components"][1] = json!({
                "class":"SPED","media_kind":2,"length":48,"rate":{"numerator":n,"denominator":d},"unsupported":"component_semantics",
                "properties":{"effect_id":"EFF_TIMEWARP.AUDIO_TIME_WARP","attributes":{"_MIXMATCH_MOTIONADAPTER":2},"speed_ratio":[ratio.num,ratio.den],"phase_offset":0,"offset_adjust":0.0,"new_source_calculation":false,"mc_mode":0},
                "input_tracks":[{"index":1,"component":leaf}]});
            let (mut s, bin) = session(&v);
            let out = s.execute("file.importAvidSequences", json!({"path":"test","bin":bin})).unwrap();
            let q = s.project.sequence(filmcraft_project::ItemId(out["sequences"][0].as_u64().unwrap())).unwrap();
            let clip = &q.audio_tracks[0].items[0];
            assert_eq!((clip.start, clip.source_in, clip.duration), (exact.tick_of(24), Tick::from_units(1, 1), exact.tick_of(48)));
            assert_eq!(clip.source_channels, vec![1]);
            assert_eq!(clip.speed, 1.0);
            // Video edit units must not resample or retime the PCM clock.
            assert_eq!(clip.source_time_at(clip.start + Tick::from_units(48001, 48000)), Tick::from_units(1, 1) + Tick::from_units(48001, 48000));
            assert_eq!(filmcraft_format::decode(&filmcraft_format::encode(&s.project, false)).unwrap().project, *s.project);
            let pointer = "/bins/0/mobs/1/tracks/1/component/components/1";
            let mut phased = v.clone();
            *phased.pointer_mut(&format!("{pointer}/properties/phase_offset")).unwrap() = json!(109);
            let raw = phased["bins"][0]["mobs"][1].clone();
            let (mut approximate, bin) = session(&phased);
            let plan = adapter::plan_best_effort(&approximate.project, &serde_json::to_vec(&phased).unwrap(), bin).unwrap();
            let (ids, rejected) = adapter::apply_supported(Arc::make_mut(&mut approximate.project), plan).unwrap();
            assert!(rejected.is_empty());
            let q = approximate.project.sequence(ids[0]).unwrap();
            assert_eq!(q.audio_tracks[0].items[0], *clip);
            let evidence: Value = serde_json::from_str(&approximate.project.item(ids[0]).unwrap().metadata[adapter::SEQUENCE_KEY]).unwrap();
            assert_eq!(evidence["composition"], raw);
            assert!(evidence.to_string().contains("phase offset 109 is not translated"));
            for (field, bad_value) in [
                ("/properties/phase_offset", json!(1)),
                ("/properties/speed_ratio", json!([2, 1])),
                ("/input_tracks/0/component/source_track", json!(3)),
                ("/input_tracks/0/component/start", json!(1_000_000)),
                ("/input_tracks/0/component/length", json!(1)),
                ("/input_tracks/0/component/media_kind", json!(1)),
            ] {
                let mut invalid = v.clone();
                *invalid.pointer_mut(&format!("{pointer}{field}")).unwrap() = bad_value;
                let (mut s, bin) = session(&invalid);
                let before = s.project.clone();
                assert!(s.execute("file.importAvidSequences", json!({"path":"test","bin":bin})).is_err(), "{field}");
                assert_eq!(s.project, before);
            }
        }
    }
    #[test]
    fn mixed_rate_picture_keeps_source_clock_on_fractional_and_integer_timelines() {
        for (label, n, d, out_rate) in [
            ("HD 1080p/23.976", 23976, 1000, FrameRate::FPS_23_976),
            ("HD 1080p/25", 25, 1, FrameRate::new(25, 1)),
            ("UHD/50", 50, 1, FrameRate::new(50, 1)),
            ("UHD/59.94", 5994, 100, FrameRate::FPS_59_94),
        ] {
            let mut v = rate_fixture();
            let seq = &mut v["bins"][0]["mobs"][1];
            let path = "/tracks/3/component/components/1/input_tracks/0/component";
            let leaf = seq.pointer(&format!("{path}/input_tracks/0/component")).unwrap().clone();
            set_sequence_clock(seq, &json!({"numerator":n,"denominator":d}));
            seq["attributes"]["SEQUERNCE_FORMAT_STRING"] = json!(label);
            seq["tracks"][0]["component"]["fps"] = json!(out_rate.timecode_base());
            let motion = seq.pointer_mut(path).unwrap();
            motion["input_tracks"][0]["component"] = leaf;
            let sr = FrameRate::FPS_29_97;
            let ratio = FrameRate::new(out_rate.num * sr.den, out_rate.den * sr.num);
            motion["properties"]["speed_ratio"] = json!([ratio.num, ratio.den]);
            motion["input_tracks"][0]["component"]["length"] = json!((48 * ratio.den + ratio.num - 1) / ratio.num);
            let (mut s, bin) = rate_session(&v);
            let out = s.execute("file.importAvidSequences", json!({"path":"test","bin":bin})).unwrap();
            let id = filmcraft_project::ItemId(out["sequences"][0].as_u64().unwrap());
            let q = s.project.sequence(id).unwrap();
            assert_eq!(q.settings.frame_rate, out_rate);
            let c = &q.video_tracks[0].items[0];
            assert_eq!((c.start, c.source_in, c.duration), (out_rate.tick_of(24), sr.tick_of(30), out_rate.tick_of(48)));
            assert_eq!(c.speed, 1.0);
            for frame in 0..48 {
                assert_eq!(sr.frame_at(c.source_time_at(c.start + out_rate.tick_of(frame))), 30 + frame * ratio.den / ratio.num);
            }
        }
    }
    #[test]
    fn sequences_in_one_bin_keep_independent_settings() {
        let mut v = fixture();
        let mut other = v["bins"][0]["mobs"][1].clone();
        other["id"]["raw"] = json!("03".repeat(32));
        other["name"] = json!("UHD 25 cut");
        other["attributes"]["SEQUERNCE_FORMAT_STRING"] = json!("UHD/25");
        set_sequence_clock(&mut other, &json!({"numerator":25,"denominator":1}));
        other["tracks"][0]["component"]["fps"] = json!(25);
        v["bins"][0]["mobs"].as_array_mut().unwrap().push(other);
        let (mut s, bin) = session(&v);
        let before = s.project.clone();
        let out = s.execute("file.importAvidSequences", json!({"path":"test","bin":bin})).unwrap();
        let ids = out["sequences"].as_array().unwrap();
        assert_eq!(ids.len(), 2);
        for (i, (size, rate)) in [((1920, 1080), FrameRate::FPS_24), ((3840, 2160), FrameRate::new(25, 1))].iter().enumerate() {
            let q = s.project.sequence(filmcraft_project::ItemId(ids[i].as_u64().unwrap())).unwrap();
            assert_eq!((q.settings.width, q.settings.height), *size);
            assert_eq!(q.settings.frame_rate, *rate);
        }
        let after = s.project.clone();
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(s.project, before);
        s.execute("edit.redo", json!({})).unwrap();
        assert_eq!(s.project, after);
    }
    #[test]
    fn repeated_mixed_rate_cuts_preserve_every_boundary_and_source_frame() {
        let mut v = rate_fixture();
        let template = v["bins"][0]["mobs"][1]["tracks"][3]["component"]["components"][1].clone();
        let output_rate = json!({"numerator":24,"denominator":1});
        let mut components = Vec::new();
        let mut expected = Vec::new();
        let mut frame = 0i64;
        // Vary both gaps and cut lengths so rounding each source duration would accumulate drift.
        for cut in 0..4096i64 {
            let gap = cut % 4;
            components.push(json!({"class":"FILL","media_kind":1,"length":gap,"rate":output_rate}));
            frame += gap;
            let duration = 1 + cut * 17 % 72;
            let source_in = cut * 7 % 40;
            let mut clip = template.clone();
            clip["length"] = json!(duration);
            let motion = &mut clip["input_tracks"][0]["component"];
            motion["length"] = json!(duration);
            let leaf = &mut motion["input_tracks"][0]["component"];
            leaf["length"] = json!((duration * 1250 + 1000) / 1001);
            leaf["start"] = json!(source_in);
            components.push(clip);
            expected.push((frame, duration, source_in));
            frame += duration;
        }
        let seq = &mut v["bins"][0]["mobs"][1];
        seq["length"] = json!(frame);
        seq["tracks"] = json!([
            {"index":1,"component":{"class":"TCCP","media_kind":3,"length":frame,"rate":output_rate,"start":86400,"fps":24,"flags":0}},
            {"index":1,"component":{"class":"SEQU","media_kind":1,"length":frame,"rate":output_rate,"components":components}}
        ]);
        let (mut s, bin) = rate_session(&v);
        let before = s.project.clone();
        let out = s.execute("file.importAvidSequences", json!({"path":"test","bin":bin})).unwrap();
        let id = filmcraft_project::ItemId(out["sequences"][0].as_u64().unwrap());
        let q = s.project.sequence(id).unwrap();
        assert_eq!(q.duration(), FrameRate::FPS_24.tick_of(frame));
        assert_eq!(q.video_tracks[0].items.len(), expected.len());
        let source_rate = FrameRate::new(30000, 1001);
        for (clip, &(at, duration, source_in)) in q.video_tracks[0].items.iter().zip(&expected) {
            assert_eq!((clip.start, clip.duration), (FrameRate::FPS_24.tick_of(at), FrameRate::FPS_24.tick_of(duration)));
            assert_eq!(clip.source_in, source_rate.tick_of(source_in));
            assert_eq!(clip.speed, 1.0);
            for f in [0, duration / 2, duration - 1] {
                let sampled = source_rate.frame_at(clip.source_time_at(FrameRate::FPS_24.tick_of(at + f)));
                assert_eq!(sampled, source_in + f * 1250 / 1001);
            }
        }
        let after = s.project.clone();
        assert_eq!(filmcraft_format::decode(&filmcraft_format::encode(&after, false)).unwrap().project, *after);
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(s.project, before);
        s.execute("edit.redo", json!({})).unwrap();
        assert_eq!(s.project, after);
        eprintln!("4096 mixed-rate cuts: {frame} timeline frames, exact boundaries and source frame selections, undo/redo/roundtrip verified");
    }

    #[test]
    fn sequence_rate_adapter_rejects_inconsistent_or_unsupported_timing() {
        let base = "/tracks/3/component/components/1/input_tracks/0/component";
        for (suffix, value) in [
            ("/properties/speed_ratio/0", json!(0)),
            ("/properties/speed_ratio/1", json!(0)),
            ("/properties/speed_ratio/0", json!(-1001)),
            ("/properties/speed_ratio/0", json!(i64::MAX)),
            ("/properties/speed_ratio", json!([1, 1])),
            ("/properties/phase_offset", json!(1)),
            ("/properties/offset_adjust", json!(1.0)),
            ("/properties/new_source_calculation", json!(true)),
            ("/properties/attributes/_MIXMATCH_MOTIONADAPTER", json!(0)),
            ("/input_tracks/0/component/length", json!(59)),
            ("/input_tracks/0/component/length", json!(61)),
            ("/input_tracks/0/component/start", json!(i64::MAX)),
            ("/input_tracks/0/component/rate/numerator", json!(2998)),
            ("/input_tracks/0/component/rate/denominator", json!(0)),
            ("/input_tracks/0/component/source_track", json!(2)),
        ] {
            let mut v = rate_fixture();
            *v["bins"][0]["mobs"][1].pointer_mut(&format!("{base}{suffix}")).unwrap() = value;
            let (mut s, bin) = rate_session(&v);
            let before = s.project.clone();
            assert!(s.execute("file.importAvidSequences", json!({"path":"test","bin":bin})).is_err(), "{suffix}");
            assert_eq!(s.project, before);
        }
        let mut v = rate_fixture();
        v["bins"][0]["mobs"][1].pointer_mut(base).unwrap()["truncated"] = json!(true);
        let (mut s, bin) = rate_session(&v);
        assert!(s.execute("file.importAvidSequences", json!({"path":"test","bin":bin})).is_err());
    }

    #[test]
    fn sequence_rejects_hostile_parameters_and_structures_atomically() {
        let good = fixture();
        let (mut s, bin) = session(&good);
        let before = s.project.clone();
        for p in
            [json!(null), json!({}), json!({"path":"test"}), json!({"path":"test","bin":-1}), json!({"path":3,"bin":bin}), json!({"path":"test","bin":999})]
        {
            assert!(s.execute("file.importAvidSequences", p).is_err());
            assert_eq!(s.project, before);
        }
        for (path, value) in [
            ("/length", json!(i64::MAX)),
            ("/rate/denominator", json!(0)),
            ("/tracks/1/component/truncated", json!(true)),
            ("/tracks/1/component/components/1/source_track", json!(3)),
            ("/tracks/1/component/components/1/start", json!(i64::MAX)),
            ("/tracks/1/component/components/1/rate/numerator", json!(25)),
            ("/tracks/1/component/components/1/length", json!(49)),
            ("/tracks/1/component/components/1/class", json!("SPED")),
            ("/tracks/2/component/components/1/input_tracks/1/component/start", json!(25)),
            ("/tracks/2/component/components/1/input_tracks/1/component/source_track", json!(2)),
        ] {
            let mut v = good.clone();
            let comp = &mut v["bins"][0]["mobs"][1];
            if path.ends_with("truncated") {
                comp["tracks"][1]["component"]["truncated"] = value;
            } else {
                *comp.pointer_mut(path).unwrap() = value;
            }
            let (mut s, bin) = session(&v);
            let before = s.project.clone();
            let n = s.history.undo.len();
            let preview = s.execute("file.previewAvidSequences", json!({"path":"test","bin":bin})).unwrap();
            assert!(!preview["rejected"].as_array().unwrap().is_empty(), "{path}");
            assert!(s.execute("file.importAvidSequences", json!({"path":"test","bin":bin})).is_err(), "{path}");
            assert_eq!(s.project, before);
            assert_eq!(s.history.undo.len(), n);
        }
        // Unbound masters and duplicate identities must never select an arbitrary source.
        let (mut s, bin) = session(&good);
        let p = Arc::make_mut(&mut s.project);
        let item = p.items.values().next().unwrap().clone();
        let alias = p.add_item("alias", item.label, item.kind.clone(), Some(bin));
        p.item_mut(alias).unwrap().metadata = item.metadata;
        assert!(s.execute("file.previewAvidSequences", json!({"path":"test","bin":bin})).is_err());
        let (mut s, bin) = session(&good);
        Arc::make_mut(&mut s.project).items.values_mut().next().unwrap().as_media_mut().unwrap().media = MediaRef::File { path: String::new() };
        assert!(s.execute("file.importAvidSequences", json!({"path":"test","bin":bin})).is_err());
    }

    #[test]
    #[ignore = "set FILMCRAFT_AVID_REPORTS; uses existing private reports and managed MXFs"]
    fn sequence_local_corpus_and_audible_playback() {
        let root = std::path::PathBuf::from(std::env::var("FILMCRAFT_AVID_REPORTS").unwrap());
        for (file, count, frames) in [("phase9-transport.json", 3, 240), ("phase10-transport.json", 2, 240), ("phase11-playback.json", 2, 531)] {
            let mut s = Session::default();
            let path = root.join(file);
            let r = s.execute("file.importAvidCatalog", json!({"path":path})).unwrap();
            let bin = r["bins"][0]["bin"].clone();
            let ids: Vec<_> = s.project.items.keys().copied().collect();
            s.execute("media.bindAvidAudio", json!({"items":ids})).unwrap();
            let preview = s.execute("file.previewAvidSequences", json!({"path":path,"bin":bin})).unwrap();
            assert_eq!(preview["rejected"], json!([]), "{file}: {preview}");
            let r = s.execute("file.importAvidSequences", json!({"path":path,"bin":bin})).unwrap();
            let id = filmcraft_project::ItemId(r["sequences"][0].as_u64().unwrap());
            let q = s.project.sequence(id).unwrap();
            assert_eq!(q.settings.frame_rate, FrameRate::FPS_24);
            assert_eq!(q.start_timecode, 86400);
            assert_eq!(q.duration(), Tick::from_units(frames, 24));
            assert_eq!(q.all_tracks().map(|t| t.items.len()).sum::<usize>(), count);
            assert!(q.video_tracks[0].items.is_empty());
            if file.starts_with("phase10") {
                assert_eq!(q.audio_tracks[2].items[0].source_channels, vec![0, 1]);
                assert_eq!(q.audio_tracks[0].items[0].start, Tick::from_units(120, 24));
            }
            if file.starts_with("phase11") {
                let source_id = q.audio_tracks[0].items[0].item;
                let source = s.media.full_res_source(&s.project, source_id, &*s.services).unwrap();
                let provider = s.media.provider(s.project.clone(), s.services.clone());
                let count = 1_062_000usize;
                let full = source.audio(0, count, 48000).unwrap();
                let (peak_at, peak) = full.channels[0].iter().enumerate().max_by(|a, b| a.1.abs().total_cmp(&b.1.abs())).unwrap();
                assert!(peak.abs() > 0.01, "fixture must contain real sound");
                for start in [0, count / 2, peak_at.saturating_sub(512).min(count - 1024), count - 1024] {
                    let start = start as i64;
                    let pcm = source.audio(start, 1024, 48000).unwrap();
                    let mixed = filmcraft_render::mixer::mix_graph(&s.project, q, start, 1024, &provider, None);
                    // Two mono tracks use native center pan, each at -3 dB into both outputs.
                    for ch in &mixed.channels {
                        for (i, x) in ch.iter().enumerate() {
                            let expected = (pcm.channels[0][i] + pcm.channels[1][i]) * std::f32::consts::FRAC_1_SQRT_2;
                            assert!((x - expected).abs() < 1e-5, "mixed {x}, expected {expected}");
                        }
                    }
                }
                if let Ok(path) = std::env::var("FILMCRAFT_AVID_SEQUENCE_OUTPUT") {
                    Arc::make_mut(&mut s.project).name = "Avid Phase 11 Converted Sequence".into();
                    s.execute("file.saveAs", json!({"path":path})).unwrap();
                }
            }
            let saved = filmcraft_format::encode(&s.project, false);
            let reopened = filmcraft_format::decode(&saved).unwrap();
            assert_eq!(*s.project, reopened.project);
            eprintln!("{file}: imported {count} clips, {frames} frames, correct routing and timecode");
        }
        let path = root.join("phase12-transport.json");
        let mut s = Session::default();
        let r = s.execute("file.importAvidCatalog", json!({"path":path})).unwrap();
        let bin = r["bins"][0]["bin"].clone();
        let ids: Vec<_> = s.project.items.values().map(|i| i.id).collect();
        s.execute("media.bindAvidMedia", json!({"items":ids})).unwrap();
        let preview = s.execute("file.previewAvidSequences", json!({"path":path,"bin":bin})).unwrap();
        assert_eq!(preview["rejected"], json!([]), "{preview}");
        assert!(preview["warnings"].as_array().unwrap().iter().any(|v| v.as_str().unwrap().contains("Frame Sampling")));
        let result = s.execute("file.importAvidSequences", json!({"path":path,"bin":bin})).unwrap();
        let q = s.project.sequence(filmcraft_project::ItemId(result["sequences"][0].as_u64().unwrap())).unwrap();
        assert_eq!(q.duration(), Tick::from_units(152, 24));
        assert_eq!(q.all_tracks().map(|t| t.items.len()).sum::<usize>(), 5);
        let last = &q.video_tracks[0].items[1];
        assert_eq!((last.start, last.duration, last.speed), (Tick::from_units(106, 24), Tick::from_units(46, 24), 1.0));
        assert!(last.scale_to_frame);
        eprintln!("Phase 12: 152 frames, five clips; native frame sampling at frame 106 with 100% speed");
    }
    #[test]
    #[ignore = "set FILMCRAFT_AVID_REPORTS; Phase 12 MXFs and ffmpeg oracle; optional FILMCRAFT_AVID_PHASE12_SEQUENCE_OUTPUT"]
    fn sequence_local_phase12_frame_sampling_and_playback() {
        use filmcraft_media::FrameRequest;
        let path = std::path::PathBuf::from(std::env::var("FILMCRAFT_AVID_REPORTS").unwrap()).join("phase12-transport.json");
        let mut s = Session::default();
        let result = s.execute("file.importAvidCatalog", json!({"path":path})).unwrap();
        let bin = result["bins"][0]["bin"].clone();
        let ids: Vec<_> = s.project.items.keys().copied().collect();
        let bound = s.execute("media.bindAvidMedia", json!({"items":ids})).unwrap();
        assert!(
            bound["bound"].as_array().unwrap().iter().any(|r| r["warnings"].as_array().unwrap().iter().any(|v| v.as_str().unwrap().contains("exact rate")))
        );
        let imported = s.execute("file.importAvidSequences", json!({"path":path,"bin":bin})).unwrap();
        let id = filmcraft_project::ItemId(imported["sequences"][0].as_u64().unwrap());
        let saved = filmcraft_format::decode(&filmcraft_format::encode(&s.project, false)).unwrap();
        s.project = Arc::new(saved.project);
        let q = s.project.sequence(id).unwrap();
        assert_eq!(q.duration(), Tick::from_units(152, 24));
        assert_eq!(q.video_tracks[0].items.len(), 2);
        assert_eq!(q.video_tracks[0].items[0].source_in, Tick::from_units(21, 24));
        for track in &q.audio_tracks[..2] {
            assert_eq!(track.items[0].start, Tick::from_units(81, 24));
            assert_eq!(track.items[0].source_in, Tick::from_units(87, 24));
            assert_eq!(track.items[0].duration, Tick::from_units(38, 24));
        }
        assert_eq!(q.audio_tracks[2].items[0].source_channels, vec![0, 1]);
        let clip = q.video_tracks[0].items[1].clone();
        let m = s.project.item(clip.item).unwrap().as_media().unwrap();
        let rate = FrameRate::new(30000, 1001);
        assert_eq!(m.info.video.as_ref().unwrap().frame_rate, rate);
        assert_eq!(m.info.duration, rate.tick_of(334));
        let raw: Value = serde_json::from_str(&s.project.item(clip.item).unwrap().metadata[filmcraft_interchange::avid_catalog::SOURCE_KEY]).unwrap();
        assert_eq!(raw["clip"]["rate"]["numerator"], 2997);
        assert_eq!(raw["picture_rate_reconciliation"]["verified_mxf"]["numerator"], 30000);
        let MediaRef::Components { picture: Some(picture), .. } = &m.media else { panic!("picture binding missing") };
        let picture = picture.clone();
        let source = s.media.full_res_source(&s.project, clip.item, &*s.services).unwrap();
        for (frame, expected) in [(106, 0), (107, 1), (118, 14), (151, 56)] {
            let time = Tick::from_units(frame, 24);
            let source_time = clip.source_time_at(time);
            assert_eq!(rate.frame_at(source_time), expected);
            let actual = source.video_frame(FrameRequest::full(source_time)).unwrap();
            let filmcraft_frame::PixelData::Yuv8 { planes, .. } = &actual.data else { panic!("expected YUV8") };
            let oracle = std::process::Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-i",
                    picture.path.as_deref().unwrap(),
                    "-vf",
                    &format!("select=eq(n\\,{expected})"),
                    "-frames:v",
                    "1",
                    "-pix_fmt",
                    "yuv422p",
                    "-f",
                    "rawvideo",
                    "-",
                ])
                .output()
                .unwrap();
            assert!(oracle.status.success(), "{}", String::from_utf8_lossy(&oracle.stderr));
            assert_eq!(oracle.stdout.len(), 1920 * 1080 * 2);
            let max = planes.iter().flat_map(|p| p.iter()).zip(&oracle.stdout).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
            assert!(max <= 2, "frame {expected}: max YUV difference {max}");
            s.execute("playhead.set", json!({"time":time.0})).unwrap();
            let rendered = s.render_program(0.125).unwrap();
            assert!(rendered.px.chunks_exact(4).any(|p| p[0] > 0.05), "mixed-rate shot must render real picture");
            eprintln!("timeline frame {frame} -> MXF frame {expected}, max YUV difference {max}");
        }
        for frame in [88, 100, 105] {
            s.execute("playhead.set", json!({"time":Tick::from_units(frame,24).0})).unwrap();
            let gap = s.render_program(0.125).unwrap();
            assert!(gap.px.chunks_exact(4).all(|p| p[0].abs() < 1e-6 && p[1].abs() < 1e-6 && p[2].abs() < 1e-6));
        }
        let provider = s.media.provider(s.project.clone(), s.services.clone());
        let q = s.project.sequence(id).unwrap();
        let sound = q.audio_tracks[0].items[0].item;
        let sound = s.media.full_res_source(&s.project, sound, &*s.services).unwrap();
        // At frame 90 picture is in the gap and only the trimmed dual-mono SFX is audible.
        let direct = sound.audio((87 + 90 - 81) * 2000, 1024, 48000).unwrap();
        let mixed = filmcraft_render::mixer::mix_graph(&s.project, q, 90 * 2000, 1024, &provider, None);
        for ch in &mixed.channels {
            for (i, v) in ch.iter().enumerate() {
                let expected = (direct.channels[0][i] + direct.channels[1][i]) * std::f32::consts::FRAC_1_SQRT_2;
                assert!((v - expected).abs() < 1e-5);
            }
        }
        if let Ok(path) = std::env::var("FILMCRAFT_AVID_PHASE12_SEQUENCE_OUTPUT") {
            Arc::make_mut(&mut s.project).name = "Avid Phase 12 Converted Sequence".into();
            s.execute("playhead.set", json!({"time":0})).unwrap();
            s.execute("file.saveAs", json!({"path":path})).unwrap();
        }
    }
}
