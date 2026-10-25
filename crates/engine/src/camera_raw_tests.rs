//! Tests of the Camera Raw panel (`clip.sourceSettings`) and the project's Camera RAW settings
//! (`file.projectSettings.cameraRaw`), on hand-made R3D NE items: the commands work on the
//! project, so the RED SDK is optional; a stand-in decoder plays the RMD sidecar.

use std::sync::{Arc, Mutex};

use filmcraft_frame::{AudioBuffer, VideoFrame};
use filmcraft_media::{
    DecodeUsing, FrameRequest, Hundredths, MediaError, MediaInfo, MediaSource, RAW_ISO_VALUES, RawAsShot, RawClipInfo, RawColorSpace, RawDevelop, RawGamma,
    RawQuality, RawRollOff, RawToneMap,
};
use filmcraft_project::{ItemId, ItemKind, MediaClip};
use serde_json::{Value, json};

use crate::Session;
use crate::media_test_util::tmp_dir;

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

fn item_named(s: &Session, name: &str) -> ItemId {
    s.project.items.values().find(|i| i.name == name).map(|i| i.id).unwrap_or_else(|| panic!("no item {name}"))
}

/// A copy of the demo's MP4 item as an R3D NE clip at `path` (no decoder: the file does not exist).
fn add_r3d(s: &mut Session, name: &str, path: &str) -> ItemId {
    let ocean = item_named(s, "Ocean_Sunset.mp4");
    let mut p = (*s.project).clone();
    let mut clip = p.item(ocean).unwrap().as_media().unwrap().clone();
    clip.media = filmcraft_project::MediaRef::File { path: path.into() };
    clip.info.video.as_mut().unwrap().codec = "R3D NE".into();
    clip.info.video.as_mut().unwrap().color.transfer = filmcraft_color::Transfer::Linear;
    let id = p.add_item(name, filmcraft_project::Label::Iris, ItemKind::Media(clip), None);
    s.project = Arc::new(p);
    id
}

fn media(s: &Session, id: ItemId) -> MediaClip {
    s.project.item(id).unwrap().as_media().unwrap().clone()
}

fn setting(r: &Value, id: &str) -> Value {
    r["settings"].as_array().unwrap().iter().find(|x| x["id"] == id).cloned().unwrap_or_else(|| panic!("no setting {id} in {r}"))
}

/// The panel's controls in their three columns, as a RED Camera Raw panel lays them out; a new
/// project's clip decodes with the project's settings (IPP2, Rec. 709, BT.1886), whose
/// development controls it shows disabled until Decode Using is Clip; Blend Type and Bias are
/// disabled with a reason.
#[test]
fn the_panel_reports_three_columns_and_a_new_projects_standard() {
    let mut s = demo();
    let id = add_r3d(&mut s, "DSC_0001.R3D", "/footage/DSC_0001.R3D");
    s.execute("project.select", json!({"items": [id.0]})).unwrap();
    let r = s.execute("clip.sourceSettings", json!({})).unwrap();
    let column =
        |c: u64| r["settings"].as_array().unwrap().iter().filter(|x| x["column"] == c).map(|x| x["id"].as_str().unwrap().to_string()).collect::<Vec<_>>();
    assert_eq!(column(1), ["quality", "decodeUsing", "colorScience", "colorSpace", "gamma", "blendType", "blendBias", "metadataCurves", "creativeLut", "cdl"]);
    assert_eq!(column(2), ["iso", "toneMap", "rolloff", "hdrPeakNits", "chromaNoiseReduction", "flashingPixel"]);
    assert_eq!(column(3), ["kelvin", "tint", "exposure"]);
    assert_eq!(r["develop"]["decodeUsing"], "project", "{r}");
    assert_eq!((r["develop"]["colorSpace"].as_str(), r["develop"]["gamma"].as_str()), (Some("rec709"), Some("bt1886")));
    assert_eq!((r["develop"]["toneMap"].as_str(), r["develop"]["rolloff"].as_str()), (Some("medium"), Some("soft")));
    assert_eq!(setting(&r, "quality")["value"], "project");
    assert_eq!(setting(&r, "colorScience")["options"][0]["label"], "IPP2");
    for id in ["colorSpace", "gamma", "iso", "kelvin", "toneMap", "cdl"] {
        assert_eq!(setting(&r, id)["enabled"], false, "{id} follows the project");
    }
    for id in ["quality", "decodeUsing"] {
        assert_eq!(setting(&r, id)["enabled"], true, "{id}");
    }
    let blend = setting(&r, "blendType");
    assert_eq!((blend["enabled"].as_bool(), blend["kind"].as_str()), (Some(false), Some("fixed")));
    assert!(blend["note"].as_str().unwrap().contains("HDRx"), "{blend}");
    assert_eq!(media(&s, id).interpret.develop, None, "reading changes nothing");
    // Decode Using ▸ Clip: the clip starts from the project's settings, now editable
    let r = s.execute("clip.sourceSettings", json!({"decodeUsing": "clip"})).unwrap();
    assert_eq!(media(&s, id).interpret.develop, Some(RawDevelop { decode_using: DecodeUsing::Clip, ..RawDevelop::STANDARD }));
    assert_eq!((setting(&r, "gamma")["enabled"].as_bool(), setting(&r, "toneMap")["enabled"].as_bool()), (Some(true), Some(true)));
    assert_eq!(setting(&r, "hdrPeakNits")["enabled"], false, "HDR peak is for ST 2084 only");
    // only the combinations the compositor interprets fit
    let gamma = setting(&r, "gamma");
    let fits: Vec<&str> = gamma["options"].as_array().unwrap().iter().filter(|o| o["fits"] == true).map(|o| o["id"].as_str().unwrap()).collect();
    assert_eq!(fits, ["linear", "bt1886", "srgb"]);
}

/// Colour space and gamma are separate lists; one given alone brings a fitting other along, both
/// given must fit. The compositor's interpretation follows (nothing is stored for it), HDR
/// peaks reach the tone mapping, and hostile values change nothing.
#[test]
fn colour_space_and_gamma_develop_and_the_interpretation_follows() {
    let mut s = demo();
    let id = add_r3d(&mut s, "DSC_0001.R3D", "/footage/DSC_0001.R3D");
    s.execute("project.select", json!({"items": [id.0]})).unwrap();
    let effective = |s: &mut Session| s.execute("media.colorInfo", json!({"item": id.0})).unwrap()["effective"].as_str().map(str::to_string);
    let d = |s: &Session| media(s, id).interpret.develop.unwrap();
    s.execute("clip.sourceSettings", json!({"colorSpace": "redWideGamutRgb"})).unwrap();
    assert_eq!((d(&s).color_space, d(&s).gamma, d(&s).decode_using), (RawColorSpace::RedWideGamutRgb, RawGamma::Log3G10, DecodeUsing::Clip));
    assert_eq!(effective(&mut s).as_deref(), Some("log3g10-rwg"));
    assert_eq!(media(&s, id).interpret.color_space, None, "derived, not stored");
    s.execute("clip.sourceSettings", json!({"gamma": "linear"})).unwrap();
    assert_eq!((d(&s).color_space, d(&s).gamma), (RawColorSpace::RedWideGamutRgb, RawGamma::Linear), "linear goes with any space");
    s.execute("clip.sourceSettings", json!({"gamma": "st2084", "hdrPeakNits": 1000})).unwrap();
    assert_eq!((d(&s).color_space, d(&s).hdr_peak_nits), (RawColorSpace::Rec2020, Some(1000)));
    assert_eq!(effective(&mut s).as_deref(), Some("rec2100-pq"));
    let peak = media(&s, id).info.video.unwrap().hdr.and_then(|h| h.mastering_max_nits);
    assert_eq!(peak, Some(1000.0), "the tone mapping knows the peak");
    s.execute("clip.sourceSettings", json!({"colorSpace": "p3d65"})).unwrap();
    assert_eq!((d(&s).gamma, effective(&mut s).as_deref()), (RawGamma::Bt1886, Some("p3d65")));
    assert_eq!(media(&s, id).info.video.unwrap().hdr, None);
    s.execute("clip.sourceSettings", json!({"colorSpace": "rec709", "gamma": "srgb"})).unwrap();
    // Rec. 709 frames say what they are: no interpretation (the fast path)
    let info = s.execute("media.colorInfo", json!({"item": id.0})).unwrap();
    assert_eq!((info["override"].clone(), info["effective"].as_str()), (Value::Null, Some("linear709")), "{info}");
    // the first version's `output` still works
    s.execute("clip.sourceSettings", json!({"output": "log3g10"})).unwrap();
    assert_eq!((d(&s).color_space, d(&s).gamma), (RawColorSpace::RedWideGamutRgb, RawGamma::Log3G10));
    // a person's own Interpret Footage choice wins until the output changes
    s.execute("clip.interpretFootage", json!({"items": [id.0], "colorSpace": "rec2020"})).unwrap();
    assert_eq!(effective(&mut s).as_deref(), Some("rec2020"));
    s.execute("clip.sourceSettings", json!({"toneMap": "high"})).unwrap();
    assert_eq!(effective(&mut s).as_deref(), Some("rec2020"));
    s.execute("clip.sourceSettings", json!({"gamma": "bt1886", "colorSpace": "rec709"})).unwrap();
    assert_eq!(effective(&mut s).as_deref(), Some("linear709"), "a new output clears an interpretation that conflicts with it");
    // processing options
    s.execute(
        "clip.sourceSettings",
        json!({"chromaNoiseReduction": false, "flashingPixel": "strong", "metadataCurves": false, "cdl": true, "quality": "halfGood"}),
    )
    .unwrap();
    let got = d(&s);
    assert_eq!(
        (got.chroma_noise_reduction, got.flashing_pixel.map(|f| f.id()), got.metadata_curves, got.cdl, got.quality),
        (Some(false), Some("strong"), false, true, Some(RawQuality::HalfGood))
    );
    s.execute("clip.sourceSettings", json!({"chromaNoiseReduction": null, "flashingPixel": null, "quality": "project"})).unwrap();
    let got = d(&s);
    assert_eq!((got.chroma_noise_reduction, got.flashing_pixel, got.quality), (None, None, None), "back to as recorded");
    // hostile values are refused and change nothing
    let before = media(&s, id);
    let rev = s.revision;
    for bad in [
        json!({"colorSpace": "redWideGamutRgb", "gamma": "bt1886"}),
        json!({"colorSpace": "rec709", "gamma": "hlg"}),
        json!({"gamma": "rec2020"}),
        json!({"colorSpace": 1}),
        json!({"output": "rec2020"}),
        json!({"toneMap": "ultra"}),
        json!({"rolloff": {}}),
        json!({"hdrPeakNits": 99}),
        json!({"hdrPeakNits": 10_001}),
        json!({"hdrPeakNits": -5}),
        json!({"hdrPeakNits": "1000"}),
        json!({"chromaNoiseReduction": "yes"}),
        json!({"flashingPixel": "blinding"}),
        json!({"metadataCurves": 1}),
        json!({"quality": "eighth"}),
        json!({"quality": 2}),
        json!({"decodeUsing": "camera"}),
        json!({"useChanges": "yes"}),
        json!({"updateRmd": 1}),
    ] {
        assert!(s.execute("clip.sourceSettings", bad.clone()).is_err(), "{bad}");
    }
    assert_eq!((s.revision, media(&s, id)), (rev, before));
    // the project round-trips with the settings
    let text = serde_json::to_string(&*s.project).unwrap();
    let back: filmcraft_project::Project = serde_json::from_str(&text).unwrap();
    assert_eq!(back.item(id).unwrap().as_media().unwrap().interpret.develop, media(&s, id).interpret.develop);
    // an MP4 has no source settings
    let ocean = item_named(&s, "Ocean_Sunset.mp4");
    s.execute("project.select", json!({"items": [ocean.0]})).unwrap();
    for p in [json!({"gamma": "bt1886"}), json!({"iso": 800}), json!({"useSettings": true}), json!({"updateRmd": true})] {
        assert!(s.execute("clip.sourceSettings", p.clone()).is_err(), "{p}");
    }
    assert!(s.execute("clip.sourceSettings", json!({})).unwrap()["settings"].as_array().unwrap().is_empty());
}

/// ISO, colour temperature, tint and exposure: as shot until set, set in one undo step, put back
/// one at a time (`null`) or together (`asShot`), refused outside the decoder's ranges.
#[test]
fn raw_adjustments_are_set_reset_and_range_checked() {
    let mut s = demo();
    let id = add_r3d(&mut s, "DSC_0002.R3D", "/footage/DSC_0002.R3D");
    s.execute("project.select", json!({"items": [id.0]})).unwrap();
    let r = s.execute("clip.sourceSettings", json!({})).unwrap();
    let iso = setting(&r, "iso");
    assert_eq!((iso["kind"].as_str(), iso["value"].clone(), iso["asShot"].clone()), (Some("iso"), Value::Null, Value::Null), "{iso}");
    assert_eq!(iso["options"], json!(RAW_ISO_VALUES));
    let kelvin = setting(&r, "kelvin");
    assert_eq!((kelvin["label"].as_str(), kelvin["min"].as_u64(), kelvin["max"].as_u64()), (Some("Color Temp"), Some(1700), Some(100_000)));
    assert_eq!(setting(&r, "exposure")["label"], "Exposure Adjust");
    let rev = s.revision;
    let r = s.execute("clip.sourceSettings", json!({"iso": 3200, "kelvin": 4300, "tint": -12.5, "exposure": 0.333})).unwrap();
    assert_eq!((r["develop"]["exposure"].clone(), r["develop"]["tint"].clone()), (json!(0.33), json!(-12.5)), "{r}");
    let d = media(&s, id).interpret.develop.unwrap();
    assert_eq!(
        (d.iso, d.kelvin, d.tint, d.exposure, d.decode_using),
        (Some(3200), Some(4300), Some(Hundredths(-1250)), Some(Hundredths(33)), DecodeUsing::Clip)
    );
    assert!(s.revision > rev);
    assert_eq!(
        crate::media_pool::media_key(&media(&s, id), &s.project.settings.camera_raw),
        "file:/footage/DSC_0002.R3D|develop:rec709/bt1886/medium/soft/iso3200/k4300/t-1250/e33",
        "the pool key carries them"
    );
    s.execute("clip.sourceSettings", json!({"tint": null})).unwrap();
    assert_eq!(media(&s, id).interpret.develop.unwrap().tint, None);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(media(&s, id).interpret.develop.unwrap().tint, Some(Hundredths(-1250)), "one undo step per change");
    s.execute("edit.redo", json!({})).unwrap();
    s.execute("clip.sourceSettings", json!({"asShot": true})).unwrap();
    assert!(!media(&s, id).interpret.develop.unwrap().adjusts_raw());
    let before = media(&s, id);
    let rev = s.revision;
    for bad in [
        json!({"iso": 1250}),
        json!({"iso": 0}),
        json!({"iso": "800"}),
        json!({"iso": 800.5}),
        json!({"kelvin": 1699}),
        json!({"kelvin": 100_001}),
        json!({"kelvin": 5600.5}),
        json!({"tint": 100.01}),
        json!({"tint": "warm"}),
        json!({"exposure": 8.01}),
        json!({"exposure": 1e300}),
        json!({"asShot": "yes"}),
        json!({"iso": 3200, "kelvin": 5}),
    ] {
        assert!(s.execute("clip.sourceSettings", bad.clone()).is_err(), "{bad}");
    }
    assert_eq!((s.revision, media(&s, id)), (rev, before), "a refused request changes nothing, not even the valid part");
    s.execute("clip.sourceSettings", json!({"iso": 25600, "kelvin": 100_000, "tint": 100, "exposure": -8})).unwrap();
    let d = media(&s, id).interpret.develop.unwrap();
    assert_eq!((d.iso, d.kelvin, d.tint, d.exposure), (Some(25600), Some(100_000), Some(Hundredths(10_000)), Some(Hundredths(-800))));
}

/// The project's Camera RAW settings: a new project's standard, an old project's scene-linear
/// Rec. 709 (its clips keep their picture), every clip under Decode Using ▸ Project following a
/// change (one undo step) while clips with settings of their own keep them.
#[test]
fn project_settings_are_the_default_of_clips_that_decode_using_project() {
    let mut s = demo();
    assert_eq!(s.project.settings.camera_raw, RawDevelop::STANDARD, "a new project starts from the standard");
    let a = add_r3d(&mut s, "A.R3D", "/footage/A.R3D");
    let b = add_r3d(&mut s, "B.R3D", "/footage/B.R3D");
    s.execute("clip.sourceSettings", json!({"items": [b.0], "gamma": "srgb"})).unwrap();
    let key = |s: &Session, id: ItemId| crate::media_pool::media_key(&media(s, id), &s.project.settings.camera_raw);
    assert_eq!(key(&s, a), "file:/footage/A.R3D|develop:rec709/bt1886/medium/soft");
    let r = s.execute("file.projectSettings.cameraRaw", json!({})).unwrap();
    assert_eq!((r["standard"].as_bool(), r["clips"].as_u64()), (Some(true), Some(1)), "{r}");
    assert!(r["settings"].as_array().unwrap().iter().all(|c| c["id"] != "decodeUsing"), "Decode Using is a clip setting");
    let steps = s.history.undo.len();
    s.execute("file.projectSettings.cameraRaw", json!({"colorSpace": "redWideGamutRgb", "quality": "quarterGood", "iso": 1600})).unwrap();
    assert_eq!(s.history.undo.len(), steps + 1);
    let cr = s.project.settings.camera_raw;
    assert_eq!((cr.color_space, cr.gamma, cr.quality, cr.iso), (RawColorSpace::RedWideGamutRgb, RawGamma::Log3G10, Some(RawQuality::QuarterGood), Some(1600)));
    assert_eq!(key(&s, a), "file:/footage/A.R3D|develop:redWideGamutRgb/log3g10/medium/soft/q:quarterGood/iso1600", "A follows the project");
    assert_eq!(s.execute("media.colorInfo", json!({"item": a.0})).unwrap()["effective"], "log3g10-rwg");
    assert_eq!(media(&s, b).interpret.develop.unwrap().gamma, RawGamma::Srgb, "B keeps its own");
    assert!(key(&s, b).contains("rec709/srgb"), "{}", key(&s, b));
    // the clip's own decode quality wins over the project's, Decode Using aside
    s.execute("clip.sourceSettings", json!({"items": [a.0], "quality": "fullPremium"})).unwrap();
    assert_eq!(media(&s, a).interpret.develop.map(|d| (d.decode_using, d.quality)), Some((DecodeUsing::Project, Some(RawQuality::FullPremium))));
    assert!(key(&s, a).contains("q:fullPremium"), "{}", key(&s, a));
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project.settings.camera_raw, RawDevelop::STANDARD, "one undo step");
    // a development setting on a clip that follows the project gives it settings of its own
    s.execute("clip.sourceSettings", json!({"items": [a.0], "kelvin": 3000})).unwrap();
    let own = media(&s, a).interpret.develop.unwrap();
    assert_eq!((own.decode_using, own.gamma, own.kelvin), (DecodeUsing::Clip, RawGamma::Bt1886, Some(3000)));
    // back to the project's drops them
    s.execute("clip.sourceSettings", json!({"items": [a.0], "decodeUsing": "project", "kelvin": 3200})).unwrap();
    assert_eq!(media(&s, a).interpret.develop, None, "Decode Using ▸ Project: nothing of its own");
    // the project command refuses clip settings and hostile values, changing nothing
    let rev = s.revision;
    for bad in [
        json!({"decodeUsing": "clip"}),
        json!({"quality": "project"}),
        json!({"gamma": "log3g10", "colorSpace": "rec709"}),
        json!({"iso": 1250}),
        json!({"standard": 1}),
    ] {
        assert!(s.execute("file.projectSettings.cameraRaw", bad.clone()).is_err(), "{bad}");
    }
    assert_eq!(s.revision, rev);
    s.execute("file.projectSettings.cameraRaw", json!({"gamma": "linear"})).unwrap();
    s.execute("file.projectSettings.cameraRaw", json!({"standard": true})).unwrap();
    assert_eq!(s.project.settings.camera_raw, RawDevelop::STANDARD);
}

/// A project change of output reaches the clips that follow it as it does a clip's own: ST 2084
/// gives them HDR metadata with the project's peak (what tone mapping reads), and leaving it
/// takes the metadata away. A clip with settings of its own keeps what it had.
#[test]
fn project_settings_refresh_the_hdr_metadata_of_clips_that_follow_them() {
    let mut s = demo();
    let a = add_r3d(&mut s, "A.R3D", "/footage/A.R3D");
    let b = add_r3d(&mut s, "B.R3D", "/footage/B.R3D");
    s.execute("clip.sourceSettings", json!({"items": [b.0], "gamma": "srgb"})).unwrap();
    let peak = |s: &Session, id: ItemId| media(s, id).info.video.unwrap().hdr.and_then(|h| h.mastering_max_nits);
    s.execute("file.projectSettings.cameraRaw", json!({"gamma": "st2084", "hdrPeakNits": 1000})).unwrap();
    assert_eq!((peak(&s, a), peak(&s, b)), (Some(1000.0), None));
    assert_eq!(s.execute("media.colorInfo", json!({"item": a.0})).unwrap()["effective"], "rec2100-pq");
    s.execute("file.projectSettings.cameraRaw", json!({"hdrPeakNits": 4000})).unwrap();
    assert_eq!(peak(&s, a), Some(4000.0));
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(peak(&s, a), Some(1000.0), "undo puts the metadata back with the settings");
    s.execute("file.projectSettings.cameraRaw", json!({"standard": true})).unwrap();
    assert_eq!(peak(&s, a), None);
}

/// A project saved before Camera RAW settings existed opens with scene-linear Rec. 709 as its
/// project settings (what its clips developed with), and a clip's old `output` still reads.
#[test]
fn an_old_project_keeps_what_it_had() {
    let mut s = demo();
    let a = add_r3d(&mut s, "A.R3D", "/footage/A.R3D");
    let b = add_r3d(&mut s, "B.R3D", "/footage/B.R3D");
    let mut doc = serde_json::to_value(&*s.project).unwrap();
    doc["settings"].as_object_mut().unwrap().remove("camera_raw");
    let items = doc["items"].as_object_mut().unwrap();
    for (k, v) in items.iter_mut() {
        if *k == b.0.to_string() {
            v["kind"]["Media"]["interpret"]["develop"] = json!({"output": "rec709", "toneMap": "high", "rolloff": "soft", "iso": 800});
        }
    }
    let old: filmcraft_project::Project = serde_json::from_value(doc).unwrap();
    assert_eq!(old.settings.camera_raw, RawDevelop::default());
    let da = old.develop_of(old.item(a).unwrap().as_media().unwrap()).unwrap();
    assert_eq!((da.color_space, da.gamma, da.decode_using), (RawColorSpace::Rec709, RawGamma::Linear, DecodeUsing::Project), "as before: scene-linear");
    let db = old.develop_of(old.item(b).unwrap().as_media().unwrap()).unwrap();
    assert_eq!(
        (db.decode_using, db.gamma, db.tone_map, db.rolloff, db.iso),
        (DecodeUsing::Clip, RawGamma::Bt1886, RawToneMap::High, RawRollOff::Soft, Some(800)),
        "the old output and settings"
    );
    assert_eq!(crate::media_pool::media_key(old.item(a).unwrap().as_media().unwrap(), &old.settings.camera_raw), "file:/footage/A.R3D");
}

/// Use Changes copies the shown clip's settings to every selected clip, and a live preview of it
/// stays one undo step with the dialog's other changes.
#[test]
fn use_changes_applies_one_clips_settings_to_every_selected_clip_in_one_step() {
    let mut s = demo();
    let ids: Vec<ItemId> = (0..3).map(|i| add_r3d(&mut s, &format!("C{i}.R3D"), &format!("/footage/C{i}.R3D"))).collect();
    let ocean = item_named(&s, "Ocean_Sunset.mp4");
    s.execute("clip.sourceSettings", json!({"items": [ids[2].0], "iso": 400, "gamma": "srgb"})).unwrap();
    let items: Vec<u64> = std::iter::once(ids[0].0).chain(ids.iter().skip(1).map(|i| i.0)).chain([ocean.0]).collect();
    let steps = s.history.undo.len();
    s.execute("clip.sourceSettings", json!({"items": [ids[0].0], "kelvin": 3000, "exposure": 0.5, "preview": true})).unwrap();
    let r = s.execute("clip.sourceSettings", json!({"items": items, "useChanges": true, "preview": true})).unwrap();
    assert_eq!(r["item"], ids[0].0, "the first RAW clip speaks for the selection");
    let first = media(&s, ids[0]).interpret.develop.unwrap();
    assert_eq!((first.kelvin, first.exposure), (Some(3000), Some(Hundredths(50))));
    for id in &ids {
        assert_eq!(media(&s, *id).interpret.develop, Some(first), "{id:?}");
    }
    assert_eq!(media(&s, ocean).interpret.develop, None, "the MP4 is skipped");
    s.execute("clip.sourceSettings", json!({"items": items, "commit": true})).unwrap();
    // the dialog's previews were on the first clip's merge key, Use Changes on the selection's:
    // two steps at most, and undoing them restores every clip
    assert!(s.history.undo.len() <= steps + 2);
    while s.history.undo.len() > steps {
        s.execute("edit.undo", json!({})).unwrap();
    }
    assert_eq!(media(&s, ids[1]).interpret.develop, None);
    assert_eq!(media(&s, ids[2]).interpret.develop.unwrap().iso, Some(400));
    // without a preview: one ordinary undo step for every clip
    s.execute("clip.sourceSettings", json!({"items": [ids[0].0], "tint": 5})).unwrap();
    let steps = s.history.undo.len();
    s.execute("clip.sourceSettings", json!({"items": items, "useChanges": true})).unwrap();
    assert_eq!(s.history.undo.len(), steps + 1);
    assert!(ids.iter().all(|i| media(&s, *i).interpret.develop.unwrap().tint == Some(Hundredths(500))));
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(media(&s, ids[2]).interpret.develop.unwrap().iso, Some(400), "one undo restores them all");
    assert_eq!(media(&s, ids[1]).interpret.develop, None);
}

/// A stand-in RAW decoder: reports recorded settings and an RMD path in a temp folder, and writes
/// the settings it is given there as JSON (the RED SDK writes XML; the real-clip test on the
/// helper checks that).
struct Sidecar {
    info: MediaInfo,
    dir: std::path::PathBuf,
    written: Mutex<Option<RawDevelop>>,
}
impl MediaSource for Sidecar {
    fn info(&self) -> &MediaInfo {
        &self.info
    }
    fn video_frame(&self, _: FrameRequest) -> filmcraft_media::Result<Arc<VideoFrame>> {
        Err(MediaError::NoStream("video"))
    }
    fn audio(&self, _: i64, _: usize, _: u32) -> filmcraft_media::Result<AudioBuffer> {
        Err(MediaError::NoStream("audio"))
    }
    fn set_develop(&self, _: &RawDevelop) -> filmcraft_media::Result<()> {
        Ok(())
    }
    fn raw_as_shot(&self) -> Option<RawAsShot> {
        let w = *self.written.lock().unwrap();
        Some(RawAsShot { iso: w.and_then(|d| d.iso).unwrap_or(800), kelvin: 5600.0, tint: 0.0, exposure: 0.0 })
    }
    fn raw_clip_info(&self) -> Option<RawClipInfo> {
        let path = self.dir.join("DSC_0009.RMD").to_string_lossy().into_owned();
        let present = self.written.lock().unwrap().is_some();
        Some(RawClipInfo {
            color_space: Some(RawColorSpace::Rec709),
            gamma: Some(RawGamma::Bt1886),
            lut: true,
            lut_enabled: true,
            cdl_enabled: false,
            rmd: present.then(|| path.clone()),
            rmd_path: path,
            ..Default::default()
        })
    }
    fn write_raw_sidecar(&self, develop: &RawDevelop) -> filmcraft_media::Result<String> {
        let path = self.dir.join("DSC_0009.RMD");
        std::fs::write(&path, serde_json::to_vec(develop).unwrap()).map_err(|e| MediaError::Io(e.to_string()))?;
        *self.written.lock().unwrap() = Some(*develop);
        Ok(path.to_string_lossy().into_owned())
    }
}

/// Update RMD writes the clip's settings beside it (only when asked; never an undo step) and the
/// panel then says the RMD is in effect; Use Settings puts the clip back to what it records.
#[test]
fn update_rmd_writes_the_sidecar_and_use_settings_reverts_to_the_recorded() {
    let dir = tmp_dir("camera-raw-rmd");
    let mut s = demo();
    let path = dir.join("DSC_0009.R3D").to_string_lossy().into_owned();
    let id = add_r3d(&mut s, "DSC_0009.R3D", &path);
    let src = Arc::new(Sidecar { info: media(&s, id).info, dir: dir.clone(), written: Mutex::default() });
    s.media.insert_keyed(id, String::new(), src.clone());
    s.execute("project.select", json!({"items": [id.0]})).unwrap();
    let r = s.execute("clip.sourceSettings", json!({})).unwrap();
    assert_eq!(r["rmd"], Value::Null);
    assert!(r["rmdNote"].as_str().unwrap().starts_with("No RMD"), "{r}");
    assert_eq!(setting(&r, "creativeLut")["enabled"], false, "follows the project");
    assert!(!dir.join("DSC_0009.RMD").exists(), "nothing is written without the action");
    s.execute("clip.sourceSettings", json!({"iso": 1600, "gamma": "srgb", "creativeLut": false})).unwrap();
    let steps = s.history.undo.len();
    let r = s.execute("clip.sourceSettings", json!({"updateRmd": true})).unwrap();
    assert_eq!(s.history.undo.len(), steps, "writing a file is not an undo step");
    assert_eq!(r["rmdWritten"][0]["path"].as_str(), Some(dir.join("DSC_0009.RMD").to_string_lossy().as_ref()), "{r}");
    let written: RawDevelop = serde_json::from_slice(&std::fs::read(dir.join("DSC_0009.RMD")).unwrap()).unwrap();
    assert_eq!((written.iso, written.gamma), (Some(1600), RawGamma::Srgb), "the settings in effect");
    assert!(r["rmd"].is_string() && r["rmdNote"].as_str().unwrap().contains("RMD sidecar is in effect"), "{r}");
    assert_eq!(setting(&r, "iso")["asShot"], json!(1600.0), "the decoder now records the RMD's ISO");
    // Use Settings: back to the clip's recorded settings, the LUT on as the clip asks
    s.execute("clip.sourceSettings", json!({"exposure": 1.0, "toneMap": "low"})).unwrap();
    let steps = s.history.undo.len();
    s.execute("clip.sourceSettings", json!({"useSettings": true})).unwrap();
    assert_eq!(s.history.undo.len(), steps + 1);
    let d = media(&s, id).interpret.develop.unwrap();
    assert_eq!(
        (d.decode_using, d.iso, d.exposure, d.tone_map, d.creative_lut, d.gamma),
        (DecodeUsing::Clip, None, None, RawToneMap::Clip, true, RawGamma::Bt1886),
        "as recorded"
    );
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(media(&s, id).interpret.develop.unwrap().exposure, Some(Hundredths(100)));
    // a clip whose decoder cannot write is an error naming it, and changes nothing
    let other = add_r3d(&mut s, "Gone.R3D", "/nowhere/Gone.R3D");
    let e = s.execute("clip.sourceSettings", json!({"items": [other.0], "updateRmd": true})).unwrap_err().to_string();
    assert!(e.contains("RMD not written") && e.contains("Gone.R3D"), "{e}");
    let _ = std::fs::remove_dir_all(dir);
}

/// The live preview flow is the dialog's: previews merge into one undo step, a commit ends it, a
/// revert takes it back, hostile previews change nothing.
#[test]
fn previews_coalesce_into_one_undo_step_and_revert() {
    let mut s = demo();
    let id = add_r3d(&mut s, "DSC_0004.R3D", "/footage/DSC_0004.R3D");
    s.execute("project.select", json!({"items": [id.0]})).unwrap();
    let develop = |s: &Session| media(s, id).interpret.develop;
    let steps = |s: &Session| s.history.undo.len();
    s.execute("clip.sourceSettings", json!({"iso": 800})).unwrap();
    let opened = (develop(&s), steps(&s));
    for iso in [1600, 3200, 6400] {
        s.execute("clip.sourceSettings", json!({"items": [id.0], "iso": iso, "preview": true})).unwrap();
        assert_eq!((develop(&s).unwrap().iso, steps(&s)), (Some(iso), opened.1 + 1), "previews share one undo step");
    }
    s.execute("clip.sourceSettings", json!({"items": [id.0], "gamma": "log3g10", "decodeUsing": "clip", "preview": true})).unwrap();
    for bad in [json!({"iso": 1250, "preview": true}), json!({"gamma": "hlg", "colorSpace": "p3d65", "preview": true}), json!({"iso": 800, "preview": "yes"})] {
        assert!(s.execute("clip.sourceSettings", bad.clone()).is_err(), "{bad}");
    }
    assert_eq!((develop(&s).unwrap().gamma, steps(&s)), (RawGamma::Log3G10, opened.1 + 1));
    let r = s.execute("clip.sourceSettings", json!({"items": [id.0], "kelvin": 4300, "commit": true})).unwrap();
    assert_eq!((r["preview"].clone(), develop(&s).unwrap().kelvin, steps(&s)), (json!(false), Some(4300), opened.1 + 1), "{r}");
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!((develop(&s), steps(&s)), opened, "one Undo takes the whole dialog back");
    let undone = s.history.redo.len();
    let before = (develop(&s), steps(&s));
    s.execute("clip.sourceSettings", json!({"items": [id.0], "decodeUsing": "project", "preview": true})).unwrap();
    assert_eq!(develop(&s), None);
    let r = s.execute("clip.sourceSettings", json!({"items": [id.0], "revert": true})).unwrap();
    assert_eq!(r["reverted"], true);
    assert_eq!((develop(&s), steps(&s)), before, "Cancel leaves no trace");
    // nor takes away the Redo of what was undone before the preview
    assert_eq!(s.history.redo.len(), undone, "the redo steps are back");
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(develop(&s).unwrap().kelvin, Some(4300), "Redo brings the committed dialog back");
    s.execute("edit.undo", json!({})).unwrap();
    for bad in [json!({"revert": true, "iso": 800}), json!({"revert": true, "updateRmd": true}), json!({"commit": 1})] {
        assert!(s.execute("clip.sourceSettings", bad.clone()).is_err(), "{bad}");
    }
}
