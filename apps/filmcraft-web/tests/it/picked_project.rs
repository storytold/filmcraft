//! Where a picked or dropped project goes in the virtual file table. It is pure, so it runs natively
//! even though the rest of the crate is wasm32-only.

#[path = "../../src/picked_project.rs"]
mod rules;

use rules::{picked_project_path, save_target};

fn nothing_taken(_: &str) -> bool {
    false
}

#[test]
fn a_picked_project_goes_under_projects_by_its_name() {
    assert_eq!(picked_project_path("Trailer.fcproj", None, nothing_taken), "/projects/Trailer.fcproj", "nothing unsaved");
    assert_eq!(picked_project_path("Trailer.fcproj", Some("/projects/Cut.fcproj"), nothing_taken), "/projects/Trailer.fcproj");
    assert_eq!(picked_project_path("Trailer.fcproj", Some("/opfs/recovery/snapshot.fcproj"), nothing_taken), "/projects/Trailer.fcproj");
    // picked again: the same file's copy is replaced, as before
    assert_eq!(picked_project_path("Trailer.fcproj", None, |_| true), "/projects/Trailer.fcproj");
}

/// Save on the Save Project prompt writes the open project before the picked one opens: a picked
/// file with the same name goes elsewhere, so the save does not replace it.
#[test]
fn a_picked_project_named_like_the_file_save_writes_goes_elsewhere() {
    assert_eq!(picked_project_path("Cut.fcproj", Some("/projects/Cut.fcproj"), nothing_taken), "/projects/opened/Cut.fcproj");
    // a project never saved, saved under its name
    let target = save_target(None, "Untitled");
    assert_eq!(target, "/projects/Untitled.fcproj");
    assert_eq!(picked_project_path("Untitled.fcproj", Some(&target), nothing_taken), "/projects/opened/Untitled.fcproj");
}

/// With a prompt, the picked file never goes where a file is already, nor where the save writes:
/// Cancel leaves every file as it was.
#[test]
fn a_picked_project_does_not_replace_another_file() {
    let taken = |p: &str| matches!(p, "/projects/opened/Cut.fcproj" | "/projects/opened/2/Cut.fcproj");
    assert_eq!(picked_project_path("Cut.fcproj", Some("/projects/Cut.fcproj"), taken), "/projects/opened/3/Cut.fcproj");
    // a project opened from /projects/opened/ before, with unsaved changes
    assert_eq!(picked_project_path("Cut.fcproj", Some("/projects/opened/Cut.fcproj"), nothing_taken), "/projects/Cut.fcproj");
    // ... while /projects/Cut.fcproj holds another project
    let taken = |p: &str| matches!(p, "/projects/opened/Cut.fcproj" | "/projects/Cut.fcproj");
    assert_eq!(picked_project_path("Cut.fcproj", Some("/projects/opened/Cut.fcproj"), taken), "/projects/opened/2/Cut.fcproj");
    // a file picked before under the same name, while another project has unsaved changes
    let taken = |p: &str| p == "/projects/Trailer.fcproj";
    assert_eq!(picked_project_path("Trailer.fcproj", Some("/projects/Cut.fcproj"), taken), "/projects/opened/Trailer.fcproj");
}

#[test]
fn save_writes_the_open_projects_file_or_a_new_one_named_after_the_project() {
    assert_eq!(save_target(Some("/projects/Cut.fcproj"), "Trailer"), "/projects/Cut.fcproj");
    assert_eq!(save_target(Some("/projects/opened/Cut.fcproj"), "Cut"), "/projects/opened/Cut.fcproj");
    assert_eq!(save_target(None, "Trailer v2"), "/projects/Trailer v2.fcproj");
}
