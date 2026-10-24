//! Opt-in private corpus acceptance; input paths/results are never compiled into the binary.
//! MIT OR Apache-2.0.
use crate::{Session, premiere::hash_file};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;

fn sequence_audio_and_links(project: &filmcraft_project::Project) -> Vec<Value> {
    project
        .items
        .values()
        .filter_map(|item| item.as_sequence())
        .map(|seq| {
            let mut links = BTreeMap::<u64, (usize, usize)>::new();
            for clip in seq.video_tracks.iter().flat_map(|track| &track.items) {
                if let Some(link) = clip.link {
                    let group = links.entry(link).or_default();
                    group.0 = group.0.saturating_add(1);
                }
            }
            for clip in seq.audio_tracks.iter().flat_map(|track| &track.items) {
                if let Some(link) = clip.link {
                    let group = links.entry(link).or_default();
                    group.1 = group.1.saturating_add(1);
                }
            }
            let av: Vec<_> = links.values().filter(|(video, audio)| *video > 0 && *audio > 0).collect();
            json!({"sampleRate":seq.settings.sample_rate,"avLinkGroups":av.len(),
            "avPairs":av.iter().fold(0usize, |sum, (video, audio)| sum.saturating_add(video.saturating_mul(*audio))),
            "linkedVideoClips":av.iter().map(|(video, _)| *video).sum::<usize>(),
            "linkedAudioClips":av.iter().map(|(_, audio)| *audio).sum::<usize>()})
        })
        .collect()
}

/// Manifest: [{"source":"...","maxOffline":N,"frameRate":{"num":25,"den":1}}].
/// frameRate is an optional expectation, never an import override. Linked copies stay under target/.
#[test]
#[ignore = "set FILMCRAFT_PREMIERE_CORPUS to a local JSON manifest of user-owned projects"]
fn premiere_private_corpus_preserves_sources_and_offline_counts() {
    let manifest = std::env::var_os("FILMCRAFT_PREMIERE_CORPUS").expect("explicit corpus manifest required");
    let cases: Vec<Value> = serde_json::from_slice(&std::fs::read(manifest).unwrap()).unwrap();
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let root = filmcraft_testkit::workspace_root().join("target/premiere-corpus").join(format!("{}-{stamp}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let mut rows = Vec::new();
    for (index, case) in cases.iter().enumerate() {
        let original = PathBuf::from(case["source"].as_str().unwrap());
        let folder = root.join(index.to_string());
        std::fs::create_dir(&folder).unwrap();
        let before = hash_file(&original).unwrap();
        let input = folder.join("source.prproj");
        std::fs::copy(&original, &input).unwrap();
        assert_eq!(hash_file(&input).unwrap(), before);
        let mut session = Session::default();
        session.execute("prefs.set", json!({"key":"playback.hardwareDecoding","value":"off"})).unwrap();
        let destination = folder.join("import");
        let result = session.execute("premiere.import", json!({"path":input,"destination":destination,"mediaMode":"link"})).unwrap();
        let offline = result["offlineMedia"].as_array().unwrap().len();
        assert!(offline as u64 <= case["maxOffline"].as_u64().unwrap(), "case {index}: {result}");
        let workflow = session.project.workflow.clone().unwrap();
        assert!(workflow.imports.iter().flat_map(|i| &i.media).all(|m| m.sha256.is_none()), "linked import must defer full media hashes");
        let observations = session.execute("premiere.hashMedia", json!({"import":0})).unwrap();
        let media_count = observations["media"].as_array().unwrap().iter().filter(|m| m["sha256"].is_string()).count();
        assert_eq!(result["sourceSha256"], before.0);
        assert_eq!(hash_file(&destination.join("source.prproj")).unwrap(), before);
        let audio_and_links = sequence_audio_and_links(&session.project);
        let project = folder.join("project.fcproj");
        session.execute("file.saveAs", json!({"path":project})).unwrap();
        // Compare the record as represented by JSON: its decimal parser can normalize an f64
        // by one ULP. Exact original bytes are independently verified against source.prproj.
        let expected: filmcraft_project::workflow::Workflow = serde_json::from_slice(&serde_json::to_vec(workflow.as_ref()).unwrap()).unwrap();
        session.execute("file.open", json!({"path":project})).unwrap();
        assert_eq!(session.project.workflow.as_deref(), Some(&expected));
        assert_eq!(sequence_audio_and_links(&session.project), audio_and_links);
        assert_eq!(session.execute("premiere.hashMedia", json!({"import":0})).unwrap(), observations);
        let rates: Vec<_> = session.project.items.values().filter_map(|i| i.as_sequence().map(|s| s.frame_rate())).collect();
        if let Some(expected) = case.get("frameRate") {
            let expected: filmcraft_time::FrameRate = serde_json::from_value(expected.clone()).expect("manifest frameRate must be a valid rational rate");
            assert!(rates.iter().all(|rate| *rate == expected), "case {index}: expected {expected:?}, imported {rates:?}");
        }
        let perf = session.execute("perf.stats", json!({})).unwrap();
        assert_eq!(perf["decode"]["hardware"]["backend"], Value::Null);
        assert_eq!(perf["decode"]["hardware"]["sessions"], 0);
        assert_eq!(perf["export"]["hardware"]["sessions"], 0);
        let after = hash_file(&original).unwrap();
        assert_eq!(after, before);
        assert_eq!(hash_file(&input).unwrap(), before);
        rows.push(json!({"case":index,"sourceBefore":before.0,"sourceAfter":after.0,"offline":offline,
            "warnings":result["report"].as_array().unwrap().len(),"rates":rates,"observedMediaUnchanged":media_count,"hardwareSessions":0,
            "sequenceAudioAndLinks":audio_and_links}));
        std::fs::write(root.join("results.json"), serde_json::to_vec_pretty(&rows).unwrap()).unwrap();
    }
    println!("Corpus results: {}", root.join("results.json").display());
}
