//! Opt-in corpus comparison against per-project truth JSON exported through Premiere's scripting
//! API. Paths and results are never compiled into the binary. MIT OR Apache-2.0.
use super::*;
use serde_json::Value;

/// Premiere reports a timecode-based in point for stills; FilmCraft starts stills at zero.
const STILLS: [&str; 7] = [".bmp", ".png", ".jpg", ".jpeg", ".tif", ".tiff", ".psd"];

#[derive(Debug, PartialEq)]
struct Shape {
    video: Vec<Vec<(i64, i64)>>,
    audio: Vec<Vec<(i64, i64)>>,
    in_points: Vec<Vec<Option<i64>>>,
    markers: Vec<i64>,
}

fn ticks(v: &Value, key: &str) -> i64 {
    let t = &v[key]["ticks"];
    t.as_i64().or_else(|| t.as_str().and_then(|s| s.parse().ok())).unwrap_or(i64::MIN)
}

fn truth_tracks(seq: &Value, key: &str, in_points: &mut Vec<Vec<Option<i64>>>) -> Vec<Vec<(i64, i64)>> {
    let mut out = Vec::new();
    for track in seq[key].as_array().into_iter().flatten() {
        let mut clips: Vec<_> = track["clips"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|c| {
                let still = STILLS.iter().any(|ext| c["name"].as_str().unwrap_or("").to_ascii_lowercase().ends_with(ext));
                (ticks(c, "start"), ticks(c, "end"), (!still).then(|| ticks(c, "inPoint")))
            })
            .collect();
        clips.sort();
        if !clips.is_empty() {
            in_points.push(clips.iter().map(|c| c.2).collect());
            out.push(clips.iter().map(|c| (c.0, c.1)).collect());
        }
    }
    out
}

fn truth(json: &Value) -> Vec<(String, Shape)> {
    let mut out = Vec::new();
    for seq in json["sequences"].as_array().into_iter().flatten() {
        let mut in_points = Vec::new();
        let video = truth_tracks(seq, "videoTracks", &mut in_points);
        let audio = truth_tracks(seq, "audioTracks", &mut in_points);
        let mut markers: Vec<_> = seq["markers"].as_array().into_iter().flatten().map(|m| ticks(m, "start")).collect();
        markers.sort();
        out.push((seq["name"].as_str().unwrap_or("").to_string(), Shape { video, audio, in_points, markers }));
    }
    out
}

fn imported_tracks(tracks: &[filmcraft_project::Track], in_points: &mut Vec<Vec<Option<i64>>>) -> Vec<Vec<(i64, i64)>> {
    let mut out = Vec::new();
    for track in tracks {
        let mut clips: Vec<_> = track.items.iter().map(|c| (c.start.0, c.end().0, c.source_in.0)).collect();
        clips.sort();
        if !clips.is_empty() {
            in_points.push(clips.iter().map(|c| Some(c.2)).collect());
            out.push(clips.iter().map(|c| (c.0, c.1)).collect());
        }
    }
    out
}

fn imported(project: &filmcraft_project::Project) -> Vec<(String, Shape)> {
    let mut out = Vec::new();
    for item in project.items.values() {
        let Some(seq) = item.as_sequence() else { continue };
        let mut in_points = Vec::new();
        let video = imported_tracks(&seq.video_tracks, &mut in_points);
        let audio = imported_tracks(&seq.audio_tracks, &mut in_points);
        let mut markers: Vec<_> = seq.markers.iter().map(|m| m.start.0).collect();
        markers.sort();
        out.push((item.name.clone(), Shape { video, audio, in_points, markers }));
    }
    out
}

/// Every `<stem>.prproj` with a sibling `<stem>.json` (UTF-8, BOM allowed) is imported and each
/// truth sequence is matched by name, in order. Prints counts and mismatches; asserts only that
/// the corpus was read, so it measures fidelity rather than gating on it.
#[test]
#[ignore = "set FILMCRAFT_PREMIERE_TRUTH to a folder of .prproj files with Premiere scripting-API truth .json"]
fn premiere_corpus_matches_scripting_api_truth() {
    let dir = std::path::PathBuf::from(std::env::var_os("FILMCRAFT_PREMIERE_TRUTH").expect("truth corpus folder required"));
    let mut entries: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).collect();
    entries.sort();
    let (mut total, mut ranges, mut ins, mut markers, mut failed, mut projects) = (0, 0, 0, 0, 0, 0);
    let mut problems = Vec::new();
    for path in entries.iter().filter(|p| p.extension().is_some_and(|e| e == "prproj")) {
        let stem = path.file_stem().unwrap().to_string_lossy().to_string();
        let Ok(raw) = std::fs::read(path.with_extension("json")) else { continue };
        let raw = raw.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&raw);
        let expected = truth(&serde_json::from_slice(raw).unwrap());
        projects += 1;
        let options = ImportOptions { base_dir: Some(dir.to_string_lossy().to_string()), ..Default::default() };
        let bytes = std::fs::read(path).unwrap();
        let actual = match std::panic::catch_unwind(|| import(&bytes, &options, None)) {
            Ok(Ok((fragment, _, _))) => imported(&fragment.project),
            // A project without sequences is expected to fail with `Error::Empty`, and only that.
            Ok(Err(crate::Error::Empty)) if expected.is_empty() => continue,
            Ok(Err(error)) => {
                failed += 1;
                total += expected.len();
                problems.push(format!("{stem}: import failed ({error}), {} sequences lost", expected.len()));
                continue;
            }
            Err(_) => panic!("{stem}: import panicked"),
        };
        let mut seen = BTreeMap::<&str, usize>::new();
        for (name, want) in &expected {
            total += 1;
            let nth = seen.entry(name).or_default();
            let got = actual.iter().filter(|(n, _)| n == name).nth(*nth).map(|(_, s)| s);
            *nth += 1;
            let Some(got) = got else {
                problems.push(format!("{stem}/{name}: sequence missing"));
                continue;
            };
            if got.video == want.video && got.audio == want.audio {
                ranges += 1;
                let same = want.in_points.len() == got.in_points.len()
                    && want.in_points.iter().zip(&got.in_points).all(|(w, g)| w.len() == g.len() && w.iter().zip(g).all(|(w, g)| w.is_none() || w == g));
                if same {
                    ins += 1;
                } else {
                    problems.push(format!("{stem}/{name}: in points truth {:?} imported {:?}", want.in_points, got.in_points));
                }
            } else {
                problems.push(format!(
                    "{stem}/{name}: ranges differ\n   truth video {:?} audio {:?}\n   imported video {:?} audio {:?}",
                    want.video, want.audio, got.video, got.audio
                ));
            }
            if got.markers == want.markers {
                markers += 1;
            } else {
                problems.push(format!("{stem}/{name}: markers truth {:?} imported {:?}", want.markers, got.markers));
            }
        }
    }
    println!("projects {projects}: ranges {ranges}/{total} in {ins}/{total} markers {markers}/{total} failed {failed}");
    for problem in &problems {
        println!(" - {problem}");
    }
    assert!(projects > 0, "no .prproj with a truth .json in {}", dir.display());
}
