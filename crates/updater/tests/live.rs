//! Talks to the real github.com. Not run by `cargo test`; run it by hand with
//! `cargo test -p filmcraft-updater --test live -- --ignored --nocapture`.

use filmcraft_updater::{Availability, Staged, Updater};
use semver::Version;

#[test]
#[ignore = "talks to github.com"]
fn newest_release_downloads_verifies_extracts_and_runs() {
    let dir = tempfile::tempdir().unwrap();
    // Pretend to be an old self-managed install in a writable directory.
    let u = Updater { current: Version::new(0, 0, 1), exe: dir.path().join("filmcraft"), cache_dir: dir.path().join("cache") };
    let release = match u.check(None).unwrap() {
        Availability::Available(r) => r,
        other => panic!("expected an update from 0.0.1, got {other:?}"),
    };
    println!("latest: {} ({}) sha256 {}", release.version, release.archive_name, release.sha256);
    let mut last = 0u64;
    let work = u
        .stage(&release, &mut |done, total| {
            if done / (5 << 20) != last / (5 << 20) {
                println!("  downloaded {done} of {total:?}");
            }
            last = done;
            true
        })
        .unwrap();
    let staged = Staged::load(&work).unwrap();
    println!("staged {} file(s) for {}:", staged.files.len(), staged.version);
    for f in &staged.files {
        println!("  {} -> {}", f.src.display(), f.dst.display());
    }
    assert_eq!(staged.version, release.version.to_string());
    assert!(staged.files.iter().all(|f| f.src.is_file()));
}
