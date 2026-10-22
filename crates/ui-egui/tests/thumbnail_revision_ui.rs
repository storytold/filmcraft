//! Thumbnail identities must follow the picture's dependencies, not unrelated outer edits.
use filmcraft_engine::Session;
use filmcraft_media::generators::GeneratorSource;
use filmcraft_media::{Generator, MediaSource};
use filmcraft_project::{ItemId, ItemKind, Label, MediaClip, MediaRef, ParamValue, Project, SequenceSettings, TrackKind};
use filmcraft_time::{FrameRate, Tick, TimeRange};
use filmcraft_ui_egui::FilmcraftApp;
use std::sync::Arc;

fn fixture() -> (FilmcraftApp, ItemId, ItemId, ItemId) {
    let mut p = Project::new("nested thumbnail");
    let rate = FrameRate::FPS_24;
    let source = GeneratorSource::new(Generator::ColorMatte { color: [1.0, 0.0, 0.0, 1.0] }, 32, 18, rate, rate.tick_of(24));
    let media = p.add_item(
        "red",
        Label::Iris,
        ItemKind::Media(MediaClip {
            media: MediaRef::Generator(source.generator.clone()),
            info: source.info().clone(),
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
    let settings = SequenceSettings { width: 32, height: 18, frame_rate: rate, ..Default::default() };
    let inner = p.new_sequence("inner", settings.clone(), 1, 0, None);
    let clip = p.make_track_item(media, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, rate.tick_of(24)), rate).unwrap();
    p.sequence_mut(inner).unwrap().video_tracks[0].items.push(clip);
    let outer = p.new_sequence("outer", settings, 1, 0, None);
    let nest = p.make_track_item(inner, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, rate.tick_of(24)), rate).unwrap();
    p.sequence_mut(outer).unwrap().video_tracks[0].items.push(nest);
    let mut s = Session::default();
    s.project = Arc::new(p);
    s.state.active_sequence = Some(outer);
    (FilmcraftApp::new(s), inner, outer, media)
}

#[test]
fn outer_motion_reuses_the_unchanged_inner_thumbnail() {
    let (mut app, inner, outer, _) = fixture();
    let before = app.item_revision(inner);
    let outer_before = app.item_revision(outer);
    app.session
        .edit("outer motion", |p, _| {
            p.sequence_mut(outer).unwrap().video_tracks[0].items[0].effect_mut("motion").unwrap().param_mut("position").unwrap().value =
                ParamValue::Vec2(filmcraft_geom::Vec2::new(18.0, 9.0));
            Ok(())
        })
        .unwrap();
    assert_eq!(app.item_revision(inner), before, "outer-only motion must not discard the inner picture");
    assert_ne!(app.item_revision(outer), outer_before, "the outer picture did change");
}

fn picture(app: &FilmcraftApp, item: ItemId) -> Arc<filmcraft_ui_egui::frames::Rgba> {
    use filmcraft_ui_egui::frames::{FrameKey, Target};
    let key = FrameKey { target: Target::Item(item), frame: 0, size: 32, revision: app.item_revision(item), draft: false };
    app.frames.request(key, Tick::ZERO, 1.0, &app.session.project, 50);
    for _ in 0..2000 {
        if let Some(image) = app.frames.get(&key) {
            return image;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("thumbnail never completed");
}

#[test]
fn cached_pixels_survive_outer_edits_but_follow_child_edits_and_undo() {
    let (mut app, inner, outer, media) = fixture();
    let generation = app.session.media.content_generation();
    let red = picture(&app, inner);
    assert!(red.px.as_chunks::<4>().0.iter().any(|p| p[0] > 200 && p[1] < 20), "fixture must be visibly red");
    assert_eq!(app.session.media.content_generation(), generation, "lazy source opens do not invalidate completed pictures");
    app.session
        .edit("outer", |p, _| {
            p.sequence_mut(outer).unwrap().video_tracks[0].items[0].enabled = false;
            Ok(())
        })
        .unwrap();
    assert!(Arc::ptr_eq(&red, &picture(&app, inner)), "reuse the actual cached inner frame");
    let before = app.item_revision(inner);
    app.session
        .edit("green", |p, _| {
            p.item_mut(media).unwrap().as_media_mut().unwrap().media = MediaRef::Generator(Generator::ColorMatte { color: [0.0, 1.0, 0.0, 1.0] });
            Ok(())
        })
        .unwrap();
    assert_ne!(app.item_revision(inner), before);
    let green = picture(&app, inner);
    assert!(green.px.as_chunks::<4>().0.iter().any(|p| p[1] > 200 && p[0] < 20), "new pixels must be green");
    app.session.undo().unwrap();
    assert_eq!(picture(&app, inner).px, red.px, "undo must restore the red picture");
    app.session.redo().unwrap();
    assert_eq!(picture(&app, inner).px, green.px);
}

#[test]
fn pool_refresh_and_proxy_policy_invalidate_without_a_project_edit() {
    let (app, inner, _, media) = fixture();
    let red = picture(&app, inner);
    let before = app.item_revision(inner);
    let rate = FrameRate::FPS_24;
    let green = Arc::new(GeneratorSource::new(Generator::ColorMatte { color: [0.0, 1.0, 0.0, 1.0] }, 32, 18, rate, rate.tick_of(24)));
    app.session.media.insert(media, green);
    assert_ne!(app.item_revision(inner), before);
    assert_ne!(picture(&app, inner).px, red.px, "explicit source replacement must not leave stale pixels");
    app.session.media.clear();
    assert_eq!(picture(&app, inner).px, red.px, "after clear, the project's actual red source is reopened");
    let before = app.item_revision(inner);
    app.session.media.set_use_proxies(true);
    let proxy = app.item_revision(inner);
    assert_ne!(proxy, before);
    app.session.media.set_use_proxies(true);
    assert_eq!(app.item_revision(inner), proxy, "same policy is not a change");
    app.session.media.set_use_proxies(false);
    assert_ne!(app.item_revision(inner), proxy);
}

#[test]
fn picture_dependencies_invalidate_even_when_the_sequence_arc_does_not_change() {
    use filmcraft_project::{Caption, CaptionFormat, CaptionTrack, ClipId, TrackId};
    let (mut app, inner, _, media) = fixture();
    let mut prior = app.item_revision(inner);
    app.session
        .edit("interpret", |p, _| {
            p.item_mut(media).unwrap().as_media_mut().unwrap().interpret.ignore_alpha = true;
            Ok(())
        })
        .unwrap();
    assert_ne!(app.item_revision(inner), prior);
    prior = app.item_revision(inner);
    app.session
        .edit("offline", |p, _| {
            p.item_mut(media).unwrap().as_media_mut().unwrap().offline = true;
            Ok(())
        })
        .unwrap();
    assert_ne!(app.item_revision(inner), prior);
    prior = app.item_revision(inner);
    app.session
        .edit("proxy", |p, _| {
            p.item_mut(media).unwrap().as_media_mut().unwrap().proxy = Some(MediaRef::File { path: "new-proxy.mov".into() });
            Ok(())
        })
        .unwrap();
    assert_ne!(app.item_revision(inner), prior);
    prior = app.item_revision(inner);
    app.session
        .edit("caption", |p, _| {
            let mut track = CaptionTrack::new(TrackId(9000), "text".into(), CaptionFormat::Subtitle);
            track.captions.push(Caption {
                id: ClipId(9001),
                start: Tick::ZERO,
                duration: FrameRate::FPS_24.tick_of(24),
                text: "visible".into(),
                speaker: None,
                cue_id: None,
                settings: String::new(),
            });
            p.sequence_mut(inner).unwrap().caption_tracks.push(track);
            Ok(())
        })
        .unwrap();
    assert_ne!(app.item_revision(inner), prior);
    prior = app.item_revision(inner);
    app.session
        .edit("canvas", |p, _| {
            p.sequence_mut(inner).unwrap().settings.width = 48;
            Ok(())
        })
        .unwrap();
    assert_ne!(app.item_revision(inner), prior);
    prior = app.item_revision(inner);
    app.session
        .edit("source graphic", |p, _| {
            p.source_graphics.insert(media, Default::default());
            Ok(())
        })
        .unwrap();
    assert_ne!(app.item_revision(inner), prior);
    prior = app.item_revision(inner);
    app.session
        .edit("lut", |p, _| {
            p.luts.push(filmcraft_project::ProjectLut {
                id: "look".into(),
                name: "look".into(),
                source_path: None,
                format: "cube".into(),
                text: Arc::from("TITLE test"),
            });
            Ok(())
        })
        .unwrap();
    assert_ne!(app.item_revision(inner), prior);
}

#[test]
fn transitive_subclip_missing_item_and_recreation_refresh() {
    let (mut app, inner, _, media) = fixture();
    app.session
        .edit("subclip", |p, _| {
            let sub = p.add_item(
                "sub",
                Label::Iris,
                ItemKind::Subclip { parent: media, range: TimeRange::new(Tick::ZERO, FrameRate::FPS_24.tick_of(12)), restrict_trims: true },
                None,
            );
            p.sequence_mut(inner).unwrap().video_tracks[0].items[0].item = sub;
            Ok(())
        })
        .unwrap();
    let prior = app.item_revision(inner);
    app.session
        .edit("parent metadata", |p, _| {
            p.item_mut(media).unwrap().as_media_mut().unwrap().interpret.par = Some((2, 1));
            Ok(())
        })
        .unwrap();
    assert_ne!(app.item_revision(inner), prior);
    let prior = app.item_revision(inner);
    let saved = app.session.project.item(media).unwrap().clone();
    app.session
        .edit("remove", |p, _| {
            p.items.remove(&media);
            Ok(())
        })
        .unwrap();
    assert_ne!(app.item_revision(inner), prior);
    let prior = app.item_revision(inner);
    app.session
        .edit("restore id", |p, _| {
            p.items.insert(media, saved);
            Ok(())
        })
        .unwrap();
    assert_ne!(app.item_revision(inner), prior);
}

#[test]
fn unsupported_dependency_shapes_invalidate_conservatively() {
    for shape in ["cycle", "depth", "width", "nodes"] {
        let (mut app, inner, outer, media) = fixture();
        let p = Arc::make_mut(&mut app.session.project);
        let template = p.sequence(inner).unwrap().video_tracks[0].items[0].clone();
        match shape {
            "cycle" => p.sequence_mut(inner).unwrap().video_tracks[0].items[0].item = inner,
            "depth" => {
                let mut parent = inner;
                for _ in 0..40 {
                    let id = p.new_sequence("deep", SequenceSettings { width: 32, height: 18, ..Default::default() }, 1, 0, None);
                    let mut clip = template.clone();
                    clip.item = id;
                    p.sequence_mut(parent).unwrap().video_tracks[0].items = vec![clip];
                    parent = id;
                }
            }
            "width" => p.sequence_mut(inner).unwrap().video_tracks[0].items = vec![template.clone(); 1100],
            "nodes" => {
                let mut children = vec![];
                for _ in 0..300 {
                    let id = p.new_sequence("child", SequenceSettings { width: 32, height: 18, ..Default::default() }, 1, 0, None);
                    let mut leaf = template.clone();
                    leaf.item = media;
                    p.sequence_mut(id).unwrap().video_tracks[0].items.push(leaf);
                    let mut clip = template.clone();
                    clip.item = id;
                    children.push(clip);
                }
                p.sequence_mut(inner).unwrap().video_tracks[0].items = children;
            }
            _ => unreachable!(),
        }
        let prior = app.item_revision(inner);
        // Malformed loaded graphs are allowed here; bypass the command validator deliberately.
        Arc::make_mut(&mut app.session.project).sequence_mut(outer).unwrap().settings.width += 1;
        app.session.revision += 1;
        assert_ne!(app.item_revision(inner), prior, "{shape}: failure to establish equality must invalidate");
    }
}

#[test]
fn changing_a_grandchild_sequence_invalidates_its_ancestors() {
    let (mut app, inner, outer, _) = fixture();
    let grandchild = app
        .session
        .edit("deeper nest", |p, _| {
            let leaf = p.new_sequence("leaf", p.sequence(inner).unwrap().settings.clone(), 1, 0, None);
            let clip = p.sequence_mut(inner).unwrap().video_tracks[0].items.remove(0);
            p.sequence_mut(leaf).unwrap().video_tracks[0].items.push(clip);
            let nest =
                p.make_track_item(leaf, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, FrameRate::FPS_24.tick_of(24)), FrameRate::FPS_24).unwrap();
            p.sequence_mut(inner).unwrap().video_tracks[0].items.push(nest);
            Ok(leaf)
        })
        .unwrap();
    let a = app.item_revision(inner);
    let b = app.item_revision(outer);
    let red = picture(&app, inner);
    app.session
        .edit("hide grandchild", |p, _| {
            p.sequence_mut(grandchild).unwrap().video_tracks[0].enabled = false;
            Ok(())
        })
        .unwrap();
    assert_ne!(app.item_revision(inner), a);
    assert_ne!(app.item_revision(outer), b);
    assert_ne!(picture(&app, inner).px, red.px, "transitive change must alter real cached pixels");
}

#[test]
fn old_project_snapshots_are_not_kept_without_a_bound() {
    let (mut app, _, _, _) = fixture();
    let mut weak = vec![];
    for i in 0..40 {
        let p = Arc::make_mut(&mut app.session.project);
        let id = p.new_sequence(&format!("sequence{i}"), Default::default(), 1, 0, None);
        app.session.revision += 1;
        app.item_revision(id);
        weak.push(Arc::downgrade(&app.session.project));
    }
    assert!(weak[..weak.len() - 1].iter().filter(|w| w.upgrade().is_some()).count() <= 8, "at most eight snapshot references belong to the thumbnail memo");
}

#[test]
fn captions_update_visible_thumbnail_pixels() {
    use filmcraft_project::{Caption, CaptionFormat, CaptionTrack, ClipId, TrackId};
    let (mut app, inner, outer, _) = fixture();
    let before = picture(&app, outer);
    app.session
        .edit("caption", |p, _| {
            let mut track = CaptionTrack::new(TrackId(9100), "caption".into(), CaptionFormat::Subtitle);
            // Caption sizes are referenced to a 1080-line frame; use eight pixels at 18 lines.
            track.style.size = 480.0;
            track.captions.push(Caption {
                id: ClipId(9101),
                start: Tick::ZERO,
                duration: FrameRate::FPS_24.tick_of(24),
                text: "A".into(),
                speaker: None,
                cue_id: None,
                settings: String::new(),
            });
            p.sequence_mut(inner).unwrap().caption_tracks.push(track);
            Ok(())
        })
        .unwrap();
    assert_ne!(picture(&app, outer).px, before.px, "a visible caption must replace the cached plain frame");
}

#[test]
fn path_policy_and_pool_replacement_invalidate_existing_keys() {
    let (mut app, inner, _, _) = fixture();
    let a = app.item_revision(inner);
    app.session.media.add_stored_paths(["relative.mov".into()], Some("/external/project.fcproj"));
    let b = app.item_revision(inner);
    assert_ne!(b, a);
    app.session.media.add_stored_paths(["relative.mov".into()], Some("/other/project.fcproj"));
    assert_eq!(app.item_revision(inner), b, "existing stored paths keep their first document");
    app.session.media.allow("relative.mov");
    let c = app.item_revision(inner);
    assert_ne!(c, b);
    app.session.media.add_stored_paths(["another.mov".into()], None);
    let d = app.item_revision(inner);
    app.session.media.clear_stored();
    assert_ne!(app.item_revision(inner), d);
    let before = app.item_revision(inner);
    app.session.media = Arc::new(Default::default());
    assert_ne!(app.item_revision(inner), before, "a new pool must not reuse the previous pool's key");
}

#[test]
fn more_sequence_cards_than_memo_slots_do_not_regenerate_while_idle() {
    let (mut app, _, _, _) = fixture();
    let ids: Vec<_> = (0..24).map(|i| Arc::make_mut(&mut app.session.project).new_sequence(&format!("card{i}"), Default::default(), 1, 0, None)).collect();
    let before: Vec<_> = ids.iter().map(|id| app.item_revision(*id)).collect();
    for _ in 0..3 {
        let after: Vec<_> = ids.iter().map(|id| app.item_revision(*id)).collect();
        assert_eq!(after, before, "visible sequence cards must keep their frame keys while idle, even when the memo is full");
    }
    Arc::make_mut(&mut app.session.project).notes = "an edit".into();
    app.session.revision += 1;
    let changed: Vec<_> = ids.iter().map(|id| app.item_revision(*id)).collect();
    assert_ne!(changed.last(), before.last(), "uncached overflow still invalidates on a new project snapshot");
}

#[test]
fn oversized_name_and_effect_metadata_use_conservative_keys() {
    for large_name in [true, false] {
        let (mut app, inner, outer, _) = fixture();
        app.session
            .edit("large metadata", |p, _| {
                if large_name {
                    p.item_mut(inner).unwrap().name = "n".repeat(4097);
                } else {
                    p.sequence_mut(inner).unwrap().video_tracks[0].items[0].effects =
                        vec![filmcraft_project::effect::find_effect("motion").unwrap().instance(); 4097];
                }
                Ok(())
            })
            .unwrap();
        let before = app.item_revision(inner);
        app.session
            .edit("outer edit", |p, _| {
                p.sequence_mut(outer).unwrap().video_tracks[0].items[0].enabled = false;
                Ok(())
            })
            .unwrap();
        assert_ne!(app.item_revision(inner), before, "oversized comparison metadata must not gain optimistic reuse");
    }
}
