//! `enhance.*` commands and the `neural_enhance` effect in a session.

use serde_json::json;

use crate::Session;

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

#[test]
fn the_catalogue_is_listed_with_what_a_download_dialog_shows() {
    let mut s = demo();
    let v = s.execute("enhance.models", json!({})).unwrap();
    assert_eq!(v["available"], cfg!(feature = "neural-enhance"));
    assert_eq!(v["canDownload"], cfg!(feature = "speech-download"));
    assert_eq!(v["default"], "deepfilternet3");
    assert_eq!(v["effect"], "neural_enhance");
    let models = v["models"].as_array().unwrap();
    assert!(models.len() >= 2);
    for (i, m) in models.iter().enumerate() {
        assert_eq!(m["index"], i);
        assert!(m["size"].as_u64().unwrap() > 1_000_000, "{m}");
        assert!(m["license"].as_str().unwrap().contains("MIT"), "{m}");
        assert!(m["source"].as_str().unwrap().starts_with("https://"), "{m}");
        assert!(m["missingBytes"].as_u64().unwrap() <= m["size"].as_u64().unwrap());
        assert_eq!(m["installed"].as_bool().unwrap(), m["missingBytes"] == 0);
    }
    // the effect's Model choice lists the same models in the same order
    let def = filmcraft_project::effect_defs().iter().find(|d| d.id == "neural_enhance").unwrap();
    let filmcraft_project::ParamKind::Choice(names) = def.params.iter().find(|p| p.id == "model").unwrap().kind else { panic!("model is not a choice") };
    assert_eq!(names, models.iter().map(|m| m["name"].as_str().unwrap()).collect::<Vec<_>>());
    assert_eq!(names, filmcraft_enhance::MODEL_NAMES);
}

#[test]
fn hostile_model_parameters_are_errors() {
    let mut s = demo();
    for p in [json!({"model": "../../etc/passwd"}), json!({"model": ""}), json!({"model": "whisper-base"}), json!({"model": 7})] {
        if p["model"].is_string() {
            let e = s.execute("enhance.downloadModel", p.clone()).unwrap_err().to_string();
            assert!(e.contains("unknown speech-enhancement model"), "{e}");
        }
        assert!(s.execute("enhance.removeModel", p).is_err());
    }
    assert!(s.execute("enhance.removeModel", json!({})).unwrap_err().to_string().contains("model"));
}

#[test]
fn a_build_without_downloads_says_so() {
    let mut s = demo();
    let v = s.execute("enhance.models", json!({})).unwrap();
    let installed = v["models"][0]["installed"].as_bool().unwrap();
    if !cfg!(feature = "speech-download") && !installed {
        let e = s.execute("enhance.downloadModel", json!({})).unwrap_err().to_string();
        assert!(e.contains("not available in this build"), "{e}");
    }
    if installed {
        assert_eq!(s.execute("enhance.downloadModel", json!({})).unwrap()["installed"], true);
    }
}
