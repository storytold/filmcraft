//! Inspect native project/preset import fidelity without decoding or modifying source media.
use filmcraft_interchange::{ImportOptions, premiere};
use serde_json::json;
use std::io::Read;

fn inspect(path: &str) -> Result<serde_json::Value, String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    file.take(premiere::MAX_DOCUMENT_BYTES as u64 + 1).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if std::path::Path::new(path).extension().is_some_and(|e| e.eq_ignore_ascii_case("prfpset")) {
        let (presets, report) = premiere::import_presets(&bytes).map_err(|e| e.to_string())?;
        Ok(
            json!({"kind":"presets","presetCount":presets.len(),"presets":presets.iter().map(|p|json!({"name":p.name,"effects":p.effects.iter().map(|e|&e.effect).collect::<Vec<_>>(),"keyframes":p.effects.iter().flat_map(|e|e.params.values()).map(|p|p.keyframes.len()).sum::<usize>()})).collect::<Vec<_>>(),"report":report.entries}),
        )
    } else {
        let (imported, report) = premiere::import_project(&bytes, &ImportOptions::default()).map_err(|e| e.to_string())?;
        let sequences: Vec<_> = imported.sequences.iter().filter_map(|id| imported.project.sequence(*id)).collect();
        Ok(
            json!({"kind":"project","sequenceCount":sequences.len(),"itemCount":imported.project.items.len(),"videoClips":sequences.iter().flat_map(|s|&s.video_tracks).map(|t|t.items.len()).sum::<usize>(),"audioClips":sequences.iter().flat_map(|s|&s.audio_tracks).map(|t|t.items.len()).sum::<usize>(),"report":report.entries}),
        )
    }
}

fn main() -> std::process::ExitCode {
    if std::env::args().len() < 2 {
        eprintln!("Usage: premiere_inspect FILE.prproj|FILE.prfpset ...");
        return std::process::ExitCode::FAILURE;
    }
    let mut failed = false;
    for path in std::env::args().skip(1) {
        match inspect(&path) {
            Ok(result) => println!("{result}"),
            Err(error) => {
                failed = true;
                eprintln!("{error}");
            }
        }
    }
    if failed { std::process::ExitCode::FAILURE } else { std::process::ExitCode::SUCCESS }
}
