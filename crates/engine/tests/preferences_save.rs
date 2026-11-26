//! Kept in its own test binary because the decoder preference changes process-wide state.

use filmcraft_engine::Session;
use filmcraft_engine::autosave::Preferences;

#[test]
fn failed_preference_saves_preserve_runtime_state_and_allow_identical_retries() {
    for failure in ["lock", "destination"] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("preferences.json");
        let original = Preferences::default();
        original.save(&path).unwrap();
        let mut session = Session::default();
        session.load_preferences(&path);
        let revision = session.revision;
        let previews = session.previews.temp_root();

        let mut requested = original.clone();
        requested.playback.hardware_decoding = "off".into();
        requested.media.enable_proxies = !original.media.enable_proxies;
        requested.timeline.selection_follows_playhead = !original.timeline.selection_follows_playhead;
        requested.media_cache.location = dir.path().join("new-cache").to_string_lossy().into_owned();
        requested.media_cache.management = "olderThan".into();
        requested.media_cache.older_than_days = 1;
        let cache = filmcraft_engine::settings::media_cache_dir(&requested.media_cache, Some(dir.path())).unwrap();
        std::fs::create_dir_all(&cache).unwrap();
        let old_cache_file = cache.join("old-preview");
        let file = std::fs::File::create(&old_cache_file).unwrap();
        file.set_times(std::fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH)).unwrap();
        drop(file);

        let backup = dir.path().join("saved-preferences.json");
        let lock = if failure == "lock" {
            let file = std::fs::File::options().read(true).write(true).open(dir.path().join(".preferences.json.lock")).unwrap();
            file.lock().unwrap();
            Some(file)
        } else {
            // Fail after acquiring the lock, during the atomic replacement, on every platform.
            std::fs::rename(&path, &backup).unwrap();
            std::fs::create_dir(&path).unwrap();
            None
        };

        assert!(session.set_prefs(requested.clone()).is_err(), "{failure}");
        assert_eq!(session.prefs, original, "failed {failure} save must not look applied");
        assert!(filmcraft_codecs::hw::hardware_decoding(), "decoder state changed on {failure} failure");
        assert_eq!(session.media.use_proxies(), original.media.enable_proxies);
        assert_eq!(session.state.selection_follows_playhead, original.timeline.selection_follows_playhead);
        assert_eq!(session.previews.temp_root(), previews);
        assert_eq!(session.revision, revision);
        assert!(old_cache_file.exists(), "failed save must not run the new cache cleanup policy");

        drop(lock);
        if failure == "destination" {
            std::fs::remove_dir(&path).unwrap();
            std::fs::rename(&backup, &path).unwrap();
        }
        assert_eq!(Preferences::load(&path), original);

        // Frontends may avoid writes when their requested values already match the session.
        let retry_needed = session.prefs != requested;
        assert!(retry_needed, "an identical retry must still be offered");
        if retry_needed {
            session.set_prefs(requested.clone()).unwrap();
        }
        assert_eq!(Preferences::load(&path), requested);
        assert_eq!(session.prefs, requested);
        assert!(!filmcraft_codecs::hw::hardware_decoding());
        assert_eq!(session.media.use_proxies(), requested.media.enable_proxies);
        assert_eq!(session.state.selection_follows_playhead, requested.timeline.selection_follows_playhead);
        assert_eq!(session.previews.temp_root(), Some(cache.join("Previews")));
        assert!(session.revision > revision);
        assert!(!old_cache_file.exists(), "cache policy applies after a successful save");
    }
}
