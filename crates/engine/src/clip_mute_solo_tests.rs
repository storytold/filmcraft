#[cfg(test)]
mod clip_mute_solo_tests {
    use serde_json::json;

    use crate::Session;

    #[test]
    fn clip_mute_solo_commands_registered() {
        assert!(crate::commands::find("clip.mute").is_some(), "clip.mute must be registered");
        assert!(crate::commands::find("clip.solo").is_some(), "clip.solo must be registered");
        let s = Session::default();
        assert!(!s.is_enabled("clip.mute"), "clip.mute disabled without selection");
        assert!(!s.is_enabled("clip.solo"), "clip.solo disabled without selection");
    }

    #[test]
    fn clip_mute_toggles_clip_muted() {
        let mut s = Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        let clip = {
            let q = s.active_sequence().unwrap();
            let t = s.playhead();
            q.video_tracks[0].item_at(t).unwrap().id
        };
        s.state.selection = vec![clip];

        s.execute("clip.mute", json!({})).unwrap();
        assert!(s.active_sequence().unwrap().find_item(clip).unwrap().1.muted, "clip must be muted after clip.mute");

        s.execute("clip.mute", json!({})).unwrap();
        assert!(!s.active_sequence().unwrap().find_item(clip).unwrap().1.muted, "clip must be unmuted after second clip.mute");
    }

    #[test]
    fn clip_solo_toggles_clip_solo() {
        let mut s = Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        let clip = {
            let q = s.active_sequence().unwrap();
            let t = s.playhead();
            q.video_tracks[0].item_at(t).unwrap().id
        };
        s.state.selection = vec![clip];

        s.execute("clip.solo", json!({})).unwrap();
        assert!(s.active_sequence().unwrap().find_item(clip).unwrap().1.solo, "clip must be soloed after clip.solo");
    }
}
