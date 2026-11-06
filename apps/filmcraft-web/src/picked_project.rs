//! Where a project picked with File › Open or dropped on the page goes in the virtual file table.
//! Pure, so it is tested natively in `tests/it/picked_project.rs`.

/// The path a picked or dropped project named `name` is put at before it opens. `save_target` is
/// the file the Save Project prompt's **Save** writes before the picked project opens: the open
/// project's file, or for a project never saved `/projects/<project name>.fcproj`, the name the web
/// build saves it under. It is `None` when nothing is unsaved and no prompt is shown, and the path
/// is then `/projects/<name>`, replacing a file picked before under that name. With a prompt, the
/// picked file may still be turned down with **Cancel**, so it goes to the first of
/// `/projects/<name>`, `/projects/opened/<name>`, `/projects/opened/2/<name>`… that is not the save
/// target and that `taken` does not report as already in the file table: neither the save nor the
/// picked file replaces another file.
pub fn picked_project_path(name: &str, save_target: Option<&str>, taken: impl Fn(&str) -> bool) -> String {
    let path = format!("/projects/{name}");
    let Some(save_target) = save_target else { return path };
    if save_target != path && !taken(&path) {
        return path;
    }
    let mut n = 1u64;
    loop {
        let other = if n == 1 { format!("/projects/opened/{name}") } else { format!("/projects/opened/{n}/{name}") };
        if save_target != other && !taken(&other) {
            return other;
        }
        n += 1;
    }
}

/// The file the Save Project prompt's **Save** writes: the open project's `path`, or for a project
/// never saved, the `/projects/<name>.fcproj` the web build's save hook answers.
pub fn save_target(path: Option<&str>, project_name: &str) -> String {
    path.map_or_else(|| format!("/projects/{project_name}.fcproj"), str::to_string)
}
