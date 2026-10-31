use super::settings::{check, combo, row};
use super::{FileDialog, FilmcraftApp, Rect, Reg, Value, json, vec2};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------------------------
// the YouTube destination (storytold/filmcraft#2626): sign-in and the video's details in the
// middle column; Export uploads the file once it is written (`publish.youtube.upload` with
// `afterJob`)
// ---------------------------------------------------------------------------------------------

/// The YouTube destination's settings (`ui.set {"export": {"youtube": {…}}}`).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct YoutubeUi {
    /// Upload after the export.
    pub enabled: bool,
    /// Empty: the file name.
    pub title: String,
    pub description: String,
    /// Comma-separated.
    pub tags: String,
    /// `private` | `unlisted` | `public`.
    pub privacy: String,
    pub made_for_kids: bool,
    /// The last upload's address, or why it failed.
    pub last_upload: String,
    /// `publish.youtube.status`, asked again after every account change.
    #[serde(skip)]
    pub status: Option<Value>,
    /// The sign-in and upload jobs started here, followed until they finish.
    #[serde(skip)]
    pub sign_in_job: Option<u64>,
    #[serde(skip)]
    pub upload_job: Option<u64>,
}

impl Default for YoutubeUi {
    fn default() -> Self {
        YoutubeUi {
            enabled: false,
            title: String::new(),
            description: String::new(),
            tags: String::new(),
            privacy: "private".into(),
            made_for_kids: false,
            last_upload: String::new(),
            status: None,
            sign_in_job: None,
            upload_job: None,
        }
    }
}

const PRIVACY: [(&str, &str); 3] = [("private", "Private"), ("unlisted", "Unlisted"), ("public", "Public")];

/// `publish.youtube.status`, asked once and again after each change.
fn status(app: &mut FilmcraftApp) -> Value {
    if let Some(s) = &app.ui.export.youtube.status {
        return s.clone();
    }
    let s = app.session.execute("publish.youtube.status", json!({})).unwrap_or_default();
    app.ui.export.youtube.status = Some(s.clone());
    s
}

/// Whether this build can publish (feature `publish`); without it there is no YouTube destination.
pub(super) fn available(app: &mut FilmcraftApp) -> bool {
    status(app)["available"] == true
}

fn job_json(app: &FilmcraftApp, id: u64) -> Option<Value> {
    app.session.jobs.iter().find(|j| j.id == id).map(|j| j.to_json())
}

fn running(app: &FilmcraftApp, id: Option<u64>) -> bool {
    id.and_then(|id| job_json(app, id)).is_some_and(|j| j["finished"] != true)
}

/// Follow the sign-in and upload jobs: when one finishes, ask for the account again and say how
/// it went in the status bar.
pub(super) fn poll(app: &mut FilmcraftApp) {
    if let Some(j) = app.ui.export.youtube.sign_in_job.and_then(|id| job_json(app, id)).filter(|j| j["finished"] == true) {
        app.ui.export.youtube.sign_in_job = None;
        app.ui.export.youtube.status = None;
        app.ui.status = j["status"].as_str().unwrap_or_default().to_string();
    }
    if let Some(j) = app.ui.export.youtube.upload_job.and_then(|id| job_json(app, id)).filter(|j| j["finished"] == true) {
        let yt = &mut app.ui.export.youtube;
        yt.upload_job = None;
        // a refused account is forgotten by the job
        yt.status = None;
        yt.last_upload = match j["result"]["path"].as_str() {
            Some(url) => url.to_string(),
            None => j["status"].as_str().unwrap_or_default().to_string(),
        };
        app.ui.status = j["status"].as_str().unwrap_or_default().to_string();
    }
}

/// The video's title: the one typed, else the file name.
fn title(app: &FilmcraftApp) -> String {
    let yt = &app.ui.export.youtube;
    let typed = yt.title.trim();
    if typed.is_empty() { app.ui.export.file_name.trim().to_string() } else { typed.to_string() }
}

/// Why Export can't upload to YouTube now (it would export the file and then fail), if it can't.
pub(super) fn not_ready(app: &mut FilmcraftApp) -> Option<String> {
    if !app.ui.export.youtube.enabled || !available(app) {
        return None;
    }
    if status(app)["signedIn"] != true {
        return Some(tl!("Sign in to YouTube first (Destinations ▸ YouTube)").to_string());
    }
    if title(app).is_empty() {
        return Some(tl!("Give the YouTube video a title").to_string());
    }
    None
}

/// After `file.exportMedia` started job `export` writing `path`: upload the file once it is written.
pub(super) fn upload_after(app: &mut FilmcraftApp, export: u64, path: &str) {
    if !app.ui.export.youtube.enabled || !available(app) {
        return;
    }
    let yt = &app.ui.export.youtube;
    let tags: Vec<&str> = yt.tags.split(',').map(str::trim).filter(|t| !t.is_empty()).collect();
    let p = json!({
        "path": path,
        "title": title(app),
        "description": yt.description,
        "tags": tags,
        "privacy": yt.privacy,
        "madeForKids": yt.made_for_kids,
        "afterJob": export,
    });
    match app.session.execute("publish.youtube.upload", p) {
        Ok(v) => app.ui.export.youtube.upload_job = v["job"].as_u64(),
        Err(e) => app.ui.status = e.to_string(),
    }
}

/// A one-line field named `name` for screen readers, showing `hint` while empty.
fn field(ui: &mut egui::Ui, reg: &mut Reg, id: &str, v: &mut String, name: &str, hint: &str) {
    let r = crate::widgets::accessible_name(ui.add(egui::TextEdit::singleline(v).hint_text(hint).desired_width(f32::INFINITY)), name);
    reg.add(id, r.rect, name);
}

enum Action {
    ChooseClient,
    SignIn,
    CancelSignIn,
    SignOut,
    Open(String),
}

pub(super) fn youtube_column(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let mut reg = Reg::default();
    let st = status(app);
    let signing_in = running(app, app.ui.export.youtube.sign_in_job);
    let uploading = running(app, app.ui.export.youtube.upload_job);
    let file_name = app.ui.export.file_name.clone();
    let mut action = None;
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(14.0, 10.0))).id_salt("export-youtube"));
    let yt = &mut app.ui.export.youtube;
    egui::ScrollArea::vertical().id_salt("export-youtube-scroll").auto_shrink([false, false]).show(&mut child, |ui| {
        ui.label(egui::RichText::new(tl!("YouTube")).strong().size(14.0));
        ui.add_space(6.0);
        check(ui, &mut reg, "export.youtube.enabled", &mut yt.enabled, tl!("Upload to YouTube after the export"));
        ui.add_space(8.0);

        // the account
        ui.label(egui::RichText::new(tl!("Account")).color(t.text_dim).size(12.0));
        let mut button = |ui: &mut egui::Ui, id: &str, label: &str, then: Action| {
            let r = ui.button(label);
            reg.add(id, r.rect, label);
            if r.clicked() {
                action = Some(then);
            }
        };
        if st["client"].is_null() {
            ui.label(
                egui::RichText::new(tl!("Choose the OAuth client file (a Desktop app client) you downloaded from Google Cloud.")).color(t.text_dim).size(11.5),
            );
            button(ui, "export.youtube.client", tl!("Choose Client File…"), Action::ChooseClient);
        } else if signing_in {
            ui.label(egui::RichText::new(tl!("Finish signing in in your browser…")).size(12.0));
            button(ui, "export.youtube.cancelSignIn", tl!("Cancel"), Action::CancelSignIn);
        } else if st["signedIn"] == true {
            ui.label(egui::RichText::new(tl!("Signed in")).color(t.render_green).size(12.0));
            ui.add_enabled_ui(!uploading, |ui| button(ui, "export.youtube.signOut", tl!("Sign Out"), Action::SignOut));
        } else {
            ui.label(egui::RichText::new(tl!("Not signed in")).color(t.text_dim).size(12.0));
            ui.horizontal(|ui| {
                button(ui, "export.youtube.signIn", tl!("Sign In…"), Action::SignIn);
                button(ui, "export.youtube.client", tl!("Change Client File…"), Action::ChooseClient);
            });
        }
        ui.add_space(10.0);

        // the video
        ui.label(egui::RichText::new(tl!("Video")).color(t.text_dim).size(12.0));
        row(ui, &t, tl!("Title"), |ui| field(ui, &mut reg, "export.youtube.title", &mut yt.title, tl!("Title"), &file_name));
        row(ui, &t, tl!("Description"), |ui| {
            let r = crate::widgets::accessible_name(
                ui.add(egui::TextEdit::multiline(&mut yt.description).desired_rows(4).desired_width(f32::INFINITY)),
                tl!("Description"),
            );
            reg.add("export.youtube.description", r.rect, "Description");
        });
        row(ui, &t, tl!("Tags"), |ui| field(ui, &mut reg, "export.youtube.tags", &mut yt.tags, tl!("Tags"), tl!("Comma-separated")));
        row(ui, &t, tl!("Visibility"), |ui| {
            let selected = PRIVACY.iter().find(|(k, _)| *k == yt.privacy).map_or("Private", |(_, l)| *l);
            let options: Vec<(String, bool)> = PRIVACY.iter().map(|(_, l)| (l.to_string(), true)).collect();
            if let Some(i) = combo(ui, &mut reg, "export.youtube.privacy", selected, &options, 160.0)
                && let Some((k, _)) = PRIVACY.get(i)
            {
                yt.privacy = (*k).into();
            }
        });
        row(ui, &t, tl!("Audience"), |ui| check(ui, &mut reg, "export.youtube.madeForKids", &mut yt.made_for_kids, tl!("Made for kids")));
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new(tl!("Export writes the file, then uploads it. A private video is visible only to you until you change it in YouTube Studio."))
                .color(t.text_dim)
                .size(11.5),
        );
        if !yt.last_upload.is_empty() {
            ui.add_space(8.0);
            ui.label(egui::RichText::new(tl!("Last upload")).color(t.text_dim).size(12.0));
            if yt.last_upload.starts_with("https://") {
                let r = ui.link(&yt.last_upload);
                reg.add("export.youtube.lastUpload", r.rect, &yt.last_upload);
                if r.clicked() {
                    action = Some(Action::Open(yt.last_upload.clone()));
                }
            } else {
                let r = ui.label(egui::RichText::new(&yt.last_upload).color(t.danger).size(11.5));
                reg.add("export.youtube.lastUpload", r.rect, &yt.last_upload);
            }
        }
    });
    reg.flush(app);
    let result = match action {
        None => return,
        Some(Action::ChooseClient) => {
            app.pick_ui(FileDialog::open_file(tl!("OAuth client file"), &["json"]), |app, paths| {
                if let Some(path) = paths.into_iter().next() {
                    if let Err(e) = app.session.execute("publish.youtube.setClient", json!({"path": path})) {
                        app.ui.status = e.to_string();
                    }
                    app.ui.export.youtube.status = None;
                }
            });
            return;
        }
        Some(Action::SignIn) => app.session.execute("publish.youtube.signIn", json!({})).map(|v| app.ui.export.youtube.sign_in_job = v["job"].as_u64()),
        Some(Action::CancelSignIn) => match app.ui.export.youtube.sign_in_job {
            Some(id) => app.session.execute("jobs.cancel", json!({"job": id})).map(drop),
            None => Ok(()),
        },
        Some(Action::SignOut) => app.session.execute("publish.youtube.signOut", json!({})).map(drop),
        Some(Action::Open(url)) => {
            crate::links::open(app, ui.ctx(), &url);
            return;
        }
    };
    app.ui.export.youtube.status = None;
    if let Err(e) = result {
        app.ui.status = e.to_string();
    }
}
