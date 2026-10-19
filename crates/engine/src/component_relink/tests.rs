use super::*;
use filmcraft_media::{AudioStreamInfo, MediaInfo, MediaKind};
use filmcraft_project::{ItemKind, Label};
use filmcraft_time::Tick;

fn fixture(name: &str) -> (Session, ItemId, Vec<filmcraft_project::AudioComponent>, std::path::PathBuf) {
    let (mut s, channels, dir) = crate::component_audio_tests::fixtures(name);
    let mut missing = channels.clone();
    for c in &mut missing {
        c.path = Some(format!("/old-drive/{}.mxf", c.track));
    }
    let item = Arc::make_mut(&mut s.project).add_item(
        "Stereo",
        Label::Blue,
        ItemKind::Media(MediaClip {
            media: MediaRef::Components { channels: missing, picture: None },
            info: MediaInfo {
                name: "Stereo".into(),
                kind: MediaKind::AudioOnly,
                duration: Tick::from_units(100, 48000),
                video: None,
                audio_streams: vec![AudioStreamInfo { sample_rate: 48000, channels: 2, codec: "PCM".into(), bits_per_sample: Some(16) }],
                container: "MXF components".into(),
                start_timecode: None,
                file_size: None,
            },
            interpret: Default::default(),
            mark_in: None,
            mark_out: None,
            markers: vec![],
            offline: false,
            proxy: None,
            identity: None,
        }),
        None,
    );
    (s, item, channels, dir)
}
fn search(s: &mut Session, dir: &Path) -> (u64, Value) {
    let j = s.execute("media.searchComponents", json!({"folder":dir,"wait":true})).unwrap()["job"].as_u64().unwrap();
    let result = s.execute("media.componentSearchResult", json!({"job":j})).unwrap();
    (j, result)
}
fn binding(item: ItemId, channel: usize, path: &str) -> Value {
    json!({"item":item,"component":{"kind":"audio","channel":channel},"path":path})
}
#[test]
fn renamed_identity_matches_batch_undo_and_save_preserve_channel_slots() {
    let (mut s, id, c, dir) = fixture("relink-moved");
    let moved = dir.join("音声-renamed.MXF");
    std::fs::rename(c[0].path.as_ref().unwrap(), &moved).unwrap();
    let before = s.project.clone();
    let originals = [std::fs::read(&moved).unwrap(), std::fs::read(c[1].path.as_ref().unwrap()).unwrap()];
    let missing = crate::relink::refresh(&mut s);
    assert_eq!(s.offline.missing, vec![id]);
    assert_eq!(missing.len(), 2);
    assert_ne!(missing[0].component, missing[1].component);
    let (job, result) = search(&mut s, &dir);
    assert_eq!(result["report"]["complete"], true, "{result}");
    assert!(result["report"]["results"].as_array().unwrap().iter().all(|r| r["status"] == "unique"));
    s.execute("media.applyComponentSearch", json!({"job":job})).unwrap();
    assert!(s.offline.rows.is_empty());
    let source = s.media.full_res_source(&s.project, id, &*s.services).unwrap();
    let samples = source.audio(0, 8, 48000).unwrap();
    assert_eq!(samples.channels[0], vec![4000.0 / 32768.0; 8]);
    assert_eq!(samples.channels[1], vec![-8000.0 / 32768.0; 8]);
    let saved = filmcraft_format::encode(&s.project, false);
    assert_eq!(filmcraft_format::decode(&saved).unwrap().project, *s.project);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project, before);
    assert_eq!(std::fs::read(&moved).unwrap(), originals[0]);
    assert_eq!(std::fs::read(c[1].path.as_ref().unwrap()).unwrap(), originals[1]);
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn duplicates_require_choice_and_wrong_identity_never_substitutes_missing_a1() {
    let (mut s, id, c, dir) = fixture("relink-duplicate");
    std::fs::copy(c[1].path.as_ref().unwrap(), dir.join("duplicate.mxf")).unwrap();
    // A2 under A1's filename must not match A1.
    std::fs::copy(c[1].path.as_ref().unwrap(), c[0].path.as_ref().unwrap()).unwrap();
    let before = s.project.clone();
    let (job, result) = search(&mut s, &dir);
    assert_eq!(result["report"]["results"][0]["status"], "absent");
    assert_eq!(result["report"]["results"][1]["status"], "ambiguous");
    assert!(s.execute("media.applyComponentSearch", json!({"job":job})).is_err());
    assert_eq!(s.project, before);
    s.execute("media.applyComponentSearch", json!({"job":job,"choice":binding(id,1,c[1].path.as_ref().unwrap())})).unwrap();
    let source = s.media.full_res_source(&s.project, id, &*s.services).unwrap();
    let samples = source.audio(0, 8, 48000).unwrap();
    assert_eq!(samples.channels[0], vec![0.0; 8]);
    assert_eq!(samples.channels[1], vec![-8000.0 / 32768.0; 8]);
    assert_eq!(s.offline.rows.len(), 1);
    assert_eq!(s.offline.rows[0].component, Some(ComponentKey::Audio { channel: 0 }));
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn atomic_apply_rechecks_files_and_rejects_stale_results() {
    let (mut s, id, c, dir) = fixture("relink-recheck");
    let before = s.project.clone();
    let (job, _) = search(&mut s, &dir);
    let original = std::fs::read(c[1].path.as_ref().unwrap()).unwrap();
    std::fs::copy(c[0].path.as_ref().unwrap(), c[1].path.as_ref().unwrap()).unwrap();
    assert!(s.execute("media.applyComponentSearch", json!({"job":job})).is_err());
    assert_eq!(s.project, before);
    std::fs::write(c[1].path.as_ref().unwrap(), original).unwrap();
    s.execute("media.relinkComponents", json!({"bindings":[binding(id,0,c[0].path.as_ref().unwrap())]})).unwrap();
    let after = s.project.clone();
    assert!(s.execute("media.applyComponentSearch", json!({"job":job})).unwrap_err().to_string().contains("changed"));
    assert_eq!(s.project, after);
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn incomplete_and_cancelled_search_cannot_auto_apply_and_hostile_params_do_not_edit() {
    let (mut s, id, c, dir) = fixture("relink-bounds");
    let before = s.project.clone();
    std::fs::write(dir.join("truncated.mxf"), b"bad header").unwrap();
    let (job, result) = search(&mut s, &dir);
    assert_eq!(result["report"]["complete"], false);
    assert!(!result["report"]["diagnostics"].as_array().unwrap().is_empty());
    assert!(s.execute("media.applyComponentSearch", json!({"job":job})).is_err());
    assert_eq!(s.project, before);
    let prog = Arc::new(filmcraft_export::Progress::default());
    prog.cancel.store(true, Ordering::Relaxed);
    let found =
        scan(&s.project, vec![Target { item: id, component: ComponentKey::Audio { channel: 0 } }], vec![dir.to_str().unwrap().into()], &*s.services, prog);
    assert!(!found.complete);
    assert_eq!(found.entries, 0);
    for p in [
        Value::Null,
        json!({"bindings":[]}),
        json!({"bindings":[binding(id,usize::MAX,c[0].path.as_ref().unwrap())]}),
        json!({"bindings":[binding(id,0,"relative.mxf")]}),
        json!({"bindings":[binding(id,0,c[0].path.as_ref().unwrap()),binding(id,0,c[0].path.as_ref().unwrap())]}),
    ] {
        assert!(s.execute("media.relinkComponents", p).is_err());
        assert_eq!(s.project, before);
    }
    for p in [
        json!({}),
        json!({"folder":dir,"targets":[{"item":id,"component":{"kind":"audio","channel":-1}}]}),
        json!({"folder":dir,"targets":[]}),
        json!({"folder":dir,"targets":[{"item":id,"component":{"kind":"picture"}}]}),
    ] {
        assert!(s.execute("media.searchComponents", p).is_err());
        assert_eq!(s.project, before);
    }
    // Explicitly choosing a verified file is still possible after an incomplete search.
    s.execute("media.applyComponentSearch", json!({"job":job,"choice":binding(id,1,c[1].path.as_ref().unwrap())})).unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
#[ignore = "local Phase 12 project and media; creates only disposable target output"]
fn phase12_picture_and_audio_relink_preserve_native_sequence() {
    let path = std::env::var("FILMCRAFT_AVID_RELINK_PROJECT").expect("set Phase 12 project path");
    let original_bytes = std::fs::read(&path).unwrap();
    let original = filmcraft_format::decode(&original_bytes).unwrap().project;
    let mut offline = original.clone();
    let mut paths = Vec::new();
    for item in offline.items.values_mut() {
        if let Some(m) = item.as_media_mut()
            && let MediaRef::Components { channels, picture } = &mut m.media
        {
            for path in channels.iter_mut().map(|c| &mut c.path).chain(picture.iter_mut().map(|c| &mut c.path)) {
                if let Some(old) = path.take() {
                    paths.push(old.clone());
                    *path = Some(format!("/unmounted{old}"));
                }
            }
        }
    }
    assert!(!paths.is_empty());
    let mut root = Path::new(&paths[0]).parent().unwrap();
    while !paths.iter().all(|p| Path::new(p).starts_with(root)) {
        root = root.parent().unwrap();
    }
    eprintln!("Phase 12 search root: {}", root.display());
    let mut s = Session { project: Arc::new(offline), ..Default::default() };
    let before: Vec<_> = paths.iter().map(|p| crate::relink::identity_of(&*s.services, p).unwrap()).collect();
    let output = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/Avid Component Relink Test.fcproj");
    std::fs::write(&output, filmcraft_format::encode(&s.project, false)).unwrap();
    let (job, result) = search(&mut s, root);
    eprintln!("{result}");
    assert_eq!(result["report"]["complete"], true);
    assert!(result["report"]["results"].as_array().unwrap().iter().all(|r| r["status"] == "unique"));
    s.execute("media.applyComponentSearch", json!({"job":job})).unwrap();
    assert_eq!(*s.project, original);
    assert_eq!(std::fs::read(&path).unwrap(), original_bytes);
    for (p, expected) in paths.iter().zip(before) {
        assert_eq!(crate::relink::identity_of(&*s.services, p).unwrap(), expected);
    }
    // Search outcome is independent of the picture and audio appearing in the same logical clip.
    assert!(crate::relink::scan(&s.project, &s.media, &*s.services).is_empty());
}

#[test]
fn search_reader_enforces_budget_and_checks_cancellation_between_reads() {
    use filmcraft_mxf::ByteSource;
    let progress = Arc::new(filmcraft_export::Progress::default());
    let source = Source {
        reader: Arc::new(filmcraft_media::reader::MemReader(Arc::from([7u8; 8]))),
        progress: progress.clone(),
        bytes: AtomicU64::new(64 * 1024 * 1024),
        calls: AtomicU64::new(0),
    };
    assert!(source.read_at(0, &mut [0u8; 1]).unwrap_err().to_string().contains("budget"));
    source.bytes.store(0, Ordering::Relaxed);
    source.calls.store(100000, Ordering::Relaxed);
    assert!(source.read_at(0, &mut [0u8; 1]).unwrap_err().to_string().contains("budget"));
    source.calls.store(0, Ordering::Relaxed);
    progress.cancel.store(true, Ordering::Relaxed);
    assert!(source.read_at(0, &mut [0u8; 1]).unwrap_err().to_string().contains("cancelled"));
}

#[cfg(unix)]
#[test]
fn symbolic_link_cycles_are_not_followed_or_treated_as_complete_evidence() {
    let (mut s, _, _, dir) = fixture("relink-symlink");
    std::os::unix::fs::symlink(&dir, dir.join("cycle")).unwrap();
    let (_, result) = search(&mut s, &dir);
    assert_eq!(result["report"]["complete"], false);
    assert_eq!(result["report"]["entries"], 3);
    assert!(result["report"]["diagnostics"][0].as_str().unwrap().contains("symbolic link"));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn only_known_opatom_notices_are_nonblocking() {
    assert!(known_atom_notice("material track 2 refers to a package not in this file"));
    assert!(known_atom_notice("skipped opaque non-SMPTE header packet at byte 5888 (96 bytes)"));
    for warning in [
        "lost KLV sync at byte 5000",
        "file truncated: KLV at byte 100 needs 1000 bytes, 100 left",
        "no essence in this file for track 2 (number 0100)",
        "material track unknown refers to a package not in this file",
        "skipped opaque non-SMPTE header packet at byte bad (96 bytes)",
    ] {
        assert!(!known_atom_notice(warning), "{warning}");
    }
}

#[test]
fn import_search_preserves_ambiguity_partial_slots_and_cancellation() {
    let (s, id, c, dir) = fixture("native-import-search");
    let original = s.project.clone();
    std::fs::copy(c[0].path.as_ref().unwrap(), dir.join("copy.mxf")).unwrap();
    let mut prepared = (*original).clone();
    let report = link_prepared(&mut prepared, &[id], vec![dir.to_str().unwrap().into()], &*s.services, Arc::default()).unwrap();
    assert_eq!(report["linkedComponents"], 1);
    assert_eq!(report["offlineComponents"], 1);
    let MediaRef::Components { channels, .. } = &prepared.item(id).unwrap().as_media().unwrap().media else { panic!() };
    assert_eq!(channels[0].path.as_deref(), Some("/old-drive/1.mxf"));
    assert_eq!(channels[1].path, c[1].path);
    assert_eq!(s.project, original);
    std::fs::write(dir.join("bad.mxf"), b"truncated").unwrap();
    let mut prepared = (*original).clone();
    let report = link_prepared(&mut prepared, &[id], vec![dir.to_str().unwrap().into()], &*s.services, Arc::default()).unwrap();
    assert_eq!(report["linkedComponents"], 0);
    assert_eq!(report["search"]["complete"], false);
    assert_eq!(prepared, *original);
    let progress = Arc::new(filmcraft_export::Progress::default());
    progress.cancel.store(true, Ordering::Relaxed);
    assert!(link_prepared(&mut prepared, &[id], vec![dir.to_str().unwrap().into()], &*s.services, progress).is_err());
    assert_eq!(prepared, *original);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn import_search_combines_roots_deduplicates_overlap_and_preserves_real_copies() {
    let (s, id, components, dir) = fixture("native-multiple-roots");
    let a = dir.join("drive-a");
    let nested = a.join("media/1");
    let b = dir.join("drive-b");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let destinations = [nested.join("renamed-a1.mxf"), b.join("renamed-a2.mxf")];
    for (component, path) in components.iter().zip(&destinations) {
        std::fs::rename(component.path.as_ref().unwrap(), path).unwrap();
    }
    let originals: Vec<_> = destinations.iter().map(|p| std::fs::read(p).unwrap()).collect();
    let roots = |paths: &[&Path]| paths.iter().map(|p| p.to_string_lossy().into_owned()).collect::<Vec<_>>();
    for folders in [roots(&[&a, &b]), roots(&[&nested, &a, &b, &a]), roots(&[&a, &nested, &b])] {
        let mut project = (*s.project).clone();
        let report = link_prepared(&mut project, &[id], folders, &*s.services, Arc::default()).unwrap();
        assert_eq!(report["search"]["complete"], true, "{report}");
        assert_eq!(report["linkedComponents"], 2);
        assert_eq!(report["offlineComponents"], 0);
        assert!(report["search"]["results"].as_array().unwrap().iter().all(|r| r["candidates"].as_array().unwrap().len() == 1));
        let media = project.item(id).unwrap().as_media().unwrap();
        let MediaRef::Components { channels, .. } = &media.media else { panic!() };
        let (source, _) = crate::component_audio::open(media.info.clone(), channels, &*s.services).unwrap();
        let audio = source.audio(0, 8, 48000).unwrap();
        assert_eq!(audio.channels[0], vec![4000.0 / 32768.0; 8]);
        assert_eq!(audio.channels[1], vec![-8000.0 / 32768.0; 8]);
    }
    std::fs::copy(&destinations[0], b.join("separate-copy.mxf")).unwrap();
    let mut project = (*s.project).clone();
    let report = link_prepared(&mut project, &[id], roots(&[&a, &b]), &*s.services, Arc::default()).unwrap();
    assert_eq!(report["search"]["results"][0]["status"], "ambiguous");
    assert_eq!(report["linkedComponents"], 1);
    assert_eq!(report["offlineComponents"], 1);
    let mut project = (*s.project).clone();
    let report = link_prepared(&mut project, &[id], roots(&[&a, &dir.join("disconnected-drive")]), &*s.services, Arc::default()).unwrap();
    assert_eq!(report["search"]["complete"], false);
    assert_eq!(report["linkedComponents"], 0);
    assert_eq!(project, *s.project);
    for (path, expected) in destinations.iter().zip(originals) {
        assert_eq!(std::fs::read(path).unwrap(), expected);
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn blocking_search_problems_remain_visible_after_notice_limit() {
    let mut report = SearchResult { complete: true, ..Default::default() };
    for n in 0..150 {
        report.notice(format!("ordinary notice {n}"));
    }
    report.problem("unreadable candidate".into());
    assert!(!report.complete);
    assert_eq!(report.diagnostics.len(), 128);
    assert_eq!(report.diagnostics[0], "unreadable candidate");
    assert_eq!(report.omitted_diagnostics, 23);
}

#[test]
fn early_binding_paths_are_not_published_when_final_validation_fails() {
    let (s, id, channels, dir) = fixture("early-binding-stale-path");
    let identities = channels.iter().map(|c| c.file_package).collect();
    let index = index_identities(identities, vec![dir.to_str().unwrap().into()], &*s.services, Arc::default()).unwrap();
    let mut prepared = (*s.project).clone();
    let media = prepared.item_mut(id).unwrap().as_media_mut().unwrap();
    media.media = MediaRef::Components { channels: channels.clone(), picture: None };
    std::fs::write(channels[0].path.as_ref().unwrap(), b"truncated").unwrap();
    let report = link_prepared_index(&mut prepared, &[id], &index, &*s.services, Arc::default()).unwrap();
    assert_eq!(report["linkedComponents"], 1);
    assert_eq!(report["offlineComponents"], 1);
    let MediaRef::Components { channels: after, .. } = &prepared.item(id).unwrap().as_media().unwrap().media else { panic!() };
    assert!(after[0].path.is_none());
    assert_eq!(after[1].path, channels[1].path);
    std::fs::remove_dir_all(dir).unwrap();
}
