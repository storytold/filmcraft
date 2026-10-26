//! Headless UI tests of the Premiere Pro-style Track Header Context Menu:
//! menu structure, Rename Track, Add Track, Add Audio Submix Track, Delete Track (with confirmation),
//! Add Tracks..., Delete Tracks..., Track Output Channel Assignments, Voice-Over Record Settings,
//! and Customize Track Header Controls.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
}

impl Driver {
    fn new() -> Self {
        let mut s = Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(s).with_control(rx);
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_step_dt(1.0 / 60.0).with_max_steps(10_000).build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx };
        d.frames(4);
        d
    }

    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            let ctx = self.harness.ctx.clone();
            let mut raw = std::mem::take(self.harness.input_mut());
            eframe::App::raw_input_hook(self.harness.state_mut(), &ctx, &mut raw);
            *self.harness.input_mut() = raw;
            self.harness.step();
        }
    }

    fn call(&mut self, method: &str, params: Value) -> Value {
        let (req, reply) = ControlRequest::new(method, params.clone());
        self.tx.send(req).unwrap();
        for _ in 0..600 {
            self.frames(1);
            if let Ok(v) = reply.try_recv() {
                return v;
            }
        }
        panic!("no reply to {method} {params}");
    }

    fn ok(&mut self, method: &str, params: Value) -> Value {
        let v = self.call(method, params.clone());
        assert_eq!(v["ok"], json!(true), "{method} {params} failed: {v}");
        v["result"].clone()
    }

    fn exec(&mut self, command: &str, params: Value) -> Value {
        self.ok("engine.execute", json!({"command": command, "params": params}))
    }

    fn click(&mut self, id: &str) {
        self.ok("ui.click", json!({"id": id}));
        self.frames(3);
    }

    fn right_click(&mut self, id: &str) {
        self.ok("ui.click", json!({"id": id, "button": "right"}));
        self.frames(3);
    }

    fn has(&mut self, id: &str) -> bool {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        v.as_array().unwrap().iter().any(|e| e["id"] == json!(id))
    }

    fn app(&mut self) -> &mut FilmcraftApp {
        self.harness.state_mut()
    }

    fn v1(&mut self, i: usize) -> filmcraft_engine::project::TrackItem {
        self.app().session.active_sequence().unwrap().video_tracks[0].items[i].clone()
    }
}

#[test]
fn video_track_header_context_menu_structure() {
    let mut d = Driver::new();
    d.right_click("timeline.track.V1.target");
    d.frames(3);

    // Verify all menu items from Premiere reference structure exist
    assert!(d.has("timeline.track.V1.menu.rename"), "Rename");
    assert!(d.has("timeline.track.V1.menu.addTrack"), "Add Track");
    assert!(d.has("timeline.track.V1.duplicate"), "Duplicate Track");
    assert!(d.has("timeline.track.V1.menu.addSubmix"), "Add Audio Submix Track");
    assert!(d.has("timeline.track.V1.menu.deleteTrack"), "Delete Track");
    assert!(d.has("timeline.track.V1.menu.addTracks"), "Add Tracks…");
    assert!(d.has("timeline.track.V1.menu.deleteTracks"), "Delete Tracks…");
    assert!(d.has("timeline.track.V1.menu.outputChannels"), "Track Output Channel Assignments…");
    assert!(d.has("timeline.track.V1.menu.voiceoverSettings"), "Voice-Over Record Settings…");
    assert!(d.has("timeline.track.V1.menu.customize"), "Customize…");
}

#[test]
fn audio_track_header_context_menu_structure() {
    let mut d = Driver::new();
    d.right_click("timeline.track.A1.target");
    d.frames(3);

    assert!(d.has("timeline.track.A1.menu.rename"), "Rename");
    assert!(d.has("timeline.track.A1.menu.addTrack"), "Add Track");
    assert!(d.has("timeline.track.A1.duplicate"), "Duplicate Track");
    assert!(d.has("timeline.track.A1.menu.addSubmix"), "Add Audio Submix Track");
    assert!(d.has("timeline.track.A1.menu.deleteTrack"), "Delete Track");
    assert!(d.has("timeline.track.A1.menu.addTracks"), "Add Tracks…");
    assert!(d.has("timeline.track.A1.menu.deleteTracks"), "Delete Tracks…");
    assert!(d.has("timeline.track.A1.menu.outputChannels"), "Track Output Channel Assignments…");
    assert!(d.has("timeline.track.A1.menu.voiceoverSettings"), "Voice-Over Record Settings…");
    assert!(d.has("timeline.track.A1.menu.customize"), "Customize…");
}

#[test]
fn rename_track_action_and_persistence() {
    let mut d = Driver::new();
    d.right_click("timeline.track.V1.target");
    d.frames(3);
    assert!(d.has("timeline.track.V1.menu.rename"));
    d.click("timeline.track.V1.menu.rename");
    d.frames(3);

    // Rename dialog opens
    assert!(d.has("renameTrack.input"));
    assert!(d.has("renameTrack.ok"));

    // Modify name in draft
    if let Some(draft) = &mut d.app().ui.timeline.rename_track {
        draft.name = "Dialogue Main".into();
    }
    d.click("renameTrack.ok");
    d.frames(3);

    // Track name updated in sequence
    let q = d.app().session.active_sequence().unwrap();
    assert_eq!(q.video_tracks[0].name, "Dialogue Main");

    // Undo reverts the rename
    d.exec("edit.undo", json!({}));
    let q = d.app().session.active_sequence().unwrap();
    assert_ne!(q.video_tracks[0].name, "Dialogue Main");
}

#[test]
fn add_track_video_and_audio() {
    let mut d = Driver::new();
    let initial_v = d.app().session.active_sequence().unwrap().video_tracks.len();
    let initial_a = d.app().session.active_sequence().unwrap().audio_tracks.len();

    // Add Video Track
    d.right_click("timeline.track.V1.target");
    d.click("timeline.track.V1.menu.addTrack");
    d.frames(3);
    let q = d.app().session.active_sequence().unwrap();
    assert_eq!(q.video_tracks.len(), initial_v + 1);

    // Add Audio Track
    d.right_click("timeline.track.A1.target");
    d.click("timeline.track.A1.menu.addTrack");
    d.frames(3);
    let q = d.app().session.active_sequence().unwrap();
    assert_eq!(q.audio_tracks.len(), initial_a + 1);

    // Undo restores original track counts
    d.exec("edit.undo", json!({}));
    d.exec("edit.undo", json!({}));
    let q = d.app().session.active_sequence().unwrap();
    assert_eq!(q.video_tracks.len(), initial_v);
    assert_eq!(q.audio_tracks.len(), initial_a);
}

#[test]
fn add_audio_submix_track_and_routing() {
    let mut d = Driver::new();
    assert_eq!(d.app().session.active_sequence().unwrap().submix_tracks.len(), 0);

    d.right_click("timeline.track.A1.target");
    d.click("timeline.track.A1.menu.addSubmix");
    d.frames(3);

    let q = d.app().session.active_sequence().unwrap();
    assert_eq!(q.submix_tracks.len(), 1);
    assert_eq!(q.submix_tracks[0].name, "Submix 1");

    // Undo removes the submix track
    d.exec("edit.undo", json!({}));
    let q = d.app().session.active_sequence().unwrap();
    assert_eq!(q.submix_tracks.len(), 0);
}

#[test]
fn delete_track_with_confirmation_for_clips() {
    let mut d = Driver::new();
    let initial_v = d.app().session.active_sequence().unwrap().video_tracks.len();
    assert_eq!(initial_v, 3);
    assert_eq!(d.app().session.active_sequence().unwrap().video_tracks[2].items.len(), 0);

    // Add an empty video track after V1 (becomes V2, which has 0 clips)
    d.right_click("timeline.track.V1.target");
    d.click("timeline.track.V1.menu.addTrack");
    d.frames(3);
    assert_eq!(d.app().session.active_sequence().unwrap().video_tracks.len(), initial_v + 1);
    assert_eq!(d.app().session.active_sequence().unwrap().video_tracks[1].items.len(), 0);

    // Delete empty track V2 (no confirmation needed)
    d.right_click("timeline.track.V2.target");
    d.click("timeline.track.V2.menu.deleteTrack");
    d.frames(3);
    assert_eq!(d.app().session.active_sequence().unwrap().video_tracks.len(), initial_v);

    // V1 contains clips -> Delete Track shows confirmation modal
    d.right_click("timeline.track.V1.target");
    d.click("timeline.track.V1.menu.deleteTrack");
    d.frames(3);

    assert!(d.has("deleteTrack.confirm"));
    assert!(d.has("deleteTrack.cancel"));

    // Cancel preserves the track
    d.click("deleteTrack.cancel");
    d.frames(3);
    assert_eq!(d.app().session.active_sequence().unwrap().video_tracks.len(), initial_v);

    // Confirm deletes the track and removes clips safely
    d.right_click("timeline.track.V1.target");
    d.click("timeline.track.V1.menu.deleteTrack");
    d.frames(3);
    d.click("deleteTrack.confirm");
    d.frames(3);
    assert_eq!(d.app().session.active_sequence().unwrap().video_tracks.len(), initial_v - 1);

    // Undo restores the deleted track and its clips
    d.exec("edit.undo", json!({}));
    d.frames(3);
    assert_eq!(d.app().session.active_sequence().unwrap().video_tracks.len(), initial_v);
}

#[test]
fn add_and_delete_tracks_dialogs() {
    let mut d = Driver::new();

    // Add Tracks... opens Add Tracks dialog
    d.right_click("timeline.track.V1.target");
    d.click("timeline.track.V1.menu.addTracks");
    d.frames(3);
    assert!(d.has("addTracks.video.amount"));
    assert!(d.has("addTracks.ok"));
    d.click("addTracks.cancel");
    d.frames(3);

    // Delete Tracks... opens Delete Tracks dialog
    d.right_click("timeline.track.V1.target");
    d.click("timeline.track.V1.menu.deleteTracks");
    d.frames(3);
    assert!(d.has("deleteTracks.video"));
    assert!(d.has("deleteTracks.ok"));
    d.click("deleteTracks.cancel");
    d.frames(3);
}

#[test]
fn track_output_channel_assignments_dialog() {
    let mut d = Driver::new();

    d.right_click("timeline.track.A1.target");
    d.click("timeline.track.A1.menu.outputChannels");
    d.frames(3);

    assert!(d.has("outputChannels.format"));
    assert!(d.has("outputChannels.destination"));
    assert!(d.has("outputChannels.mapping"));
    assert!(d.has("outputChannels.ok"));

    // Change channels to 5.1 surround in draft
    if let Some(draft) = &mut d.app().ui.timeline.output_channels_dialog {
        draft.channels = filmcraft_project::AudioChannels::Surround51;
    }
    d.click("outputChannels.ok");
    d.frames(3);

    let q = d.app().session.active_sequence().unwrap();
    assert_eq!(q.audio_tracks[0].channels, filmcraft_project::AudioChannels::Surround51);

    // Undo reverts changes
    d.exec("edit.undo", json!({}));
    let q = d.app().session.active_sequence().unwrap();
    assert_eq!(q.audio_tracks[0].channels, filmcraft_project::AudioChannels::Stereo);
}

#[test]
fn voiceover_record_settings_dialog() {
    let mut d = Driver::new();

    d.right_click("timeline.track.A1.target");
    d.click("timeline.track.A1.menu.voiceoverSettings");
    d.frames(3);

    assert!(d.has("voiceover.name"));
    assert!(d.has("voiceover.countdown"));
    assert!(d.has("voiceover.preroll"));
    assert!(d.has("voiceover.cancel"));

    d.click("voiceover.cancel");
    d.frames(3);
}

#[test]
fn customize_track_header_controls() {
    let mut d = Driver::new();

    d.right_click("timeline.track.V1.target");
    d.click("timeline.track.V1.menu.customize");
    d.frames(3);

    assert!(d.has("customizeTrackHeader.lock"));
    assert!(d.has("customizeTrackHeader.target"));
    assert!(d.has("customizeTrackHeader.visibility"));
    assert!(d.has("customizeTrackHeader.mute"));
    assert!(d.has("customizeTrackHeader.solo"));
    assert!(d.has("customizeTrackHeader.voiceover"));
    assert!(d.has("customizeTrackHeader.reset"));
    assert!(d.has("customizeTrackHeader.done"));

    // Lock button starts visible
    assert!(d.has("timeline.track.V1.locked"));

    // Turn off lock visibility
    d.app().ui.timeline.header_customization.show_lock = false;
    d.frames(3);
    assert!(!d.has("timeline.track.V1.locked"), "lock control must now be hidden");

    // Reset restores all controls
    d.click("customizeTrackHeader.reset");
    d.frames(3);
    assert!(d.has("timeline.track.V1.locked"), "lock control restored");

    d.click("customizeTrackHeader.done");
    d.frames(3);
}

#[test]
fn clip_context_menu_remains_separate_from_track_header_menu() {
    let mut d = Driver::new();
    let clip = d.v1(0);

    // Right-clicking a clip opens the clip context menu, NOT track header menu
    d.right_click(&format!("timeline.clip.{}", clip.id.0));
    d.frames(3);

    assert!(d.has("timeline.clipMenu.timeline.duplicate"));
    assert!(d.has("timeline.clipMenu.edit.copy"));
    assert!(!d.has("timeline.track.V1.menu.addTrack"));
    assert!(!d.has("timeline.track.V1.menu.customize"));
}
