//! The dialog for a waiting [`Prompt`](crate::prompt::Prompt).
//!
//! The app calls [`show`] once per frame. It also looks for result files of linked items. Every
//! choice is a labelled button or a text row, so nothing depends on colour.

use std::time::Duration;

use filmcraft_engine::Session;
use filmcraft_engine::panels::Level;

use crate::host::{Host, Launched, host};
use crate::notice;
use crate::prompt::{self, Prompt};

/// What the user pressed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    Dismiss,
    Install,
    Choose(String),
    /// Stop the start of EffectCraft that is running, and drop the question.
    CancelStart,
    /// Stop the render of the sources that have no file, and drop the question.
    CancelPrepare,
}

/// How often the progress dialog looks at the starting app.
const START_LOOK: Duration = Duration::from_millis(100);

/// Draw the waiting prompt and the hand-off notice, if any, and carry out the answers.
pub fn show(ctx: &egui::Context, session: &mut Session) {
    show_with(ctx, session, &host());
}

/// [`show`] with the UI translation. `translate` maps an English string to the language in use.
pub fn show_translated(ctx: &egui::Context, session: &mut Session, translate: &dyn Fn(&str) -> String) {
    let host = host();
    show_notice(ctx, session, &host, translate);
    show_prompts(ctx, session, &host);
}

/// [`show`] for a given host.
pub fn show_with(ctx: &egui::Context, session: &mut Session, host: &Host) {
    show_notice(ctx, session, host, &|text| text.to_string());
    show_prompts(ctx, session, host);
}

/// The result watch and the waiting question. The notice is not drawn here: each entry point draws
/// it once with its own translation.
fn show_prompts(ctx: &egui::Context, session: &mut Session, host: &Host) {
    watch_results(ctx, session);
    let Some(waiting) = prompt::peek() else { return };
    if matches!(waiting, Prompt::Starting { .. }) && !advance_start(ctx, session, host, &waiting) {
        return;
    }
    if matches!(waiting, Prompt::Preparing { .. }) && !advance_prepare(ctx, session, &waiting) {
        return;
    }
    // The start may have ended above and left another question.
    let Some(waiting) = prompt::peek() else { return };
    if let Some(answer) = draw(ctx, host, &waiting) {
        answer_with(session, host, &waiting, answer);
    }
}

/// A result file that appears brings its item online. While one is awaited, look again soon.
fn watch_results(ctx: &egui::Context, session: &mut Session) {
    crate::manager::pump(session);
    if crate::link_ops::poll_results(session) {
        ctx.request_repaint_after(crate::link_ops::RESULT_POLL);
    }
}

/// The user's answer to `waiting`: drop the question, then carry the answer out. A cancel also stops
/// the app that is starting.
pub fn answer_with(session: &mut Session, host: &Host, waiting: &Prompt, answer: Answer) {
    prompt::take();
    match answer {
        Answer::CancelStart => host.launcher.cancel(),
        Answer::CancelPrepare => crate::prepare::cancel(),
        _ => {}
    }
    apply(session, waiting, answer);
}

/// Look at the app that is starting. Returns true when the dialog still has something to draw.
///
/// - Ready: the question goes and the call that was waiting runs again, now with an instance to talk to.
/// - Failed: the question becomes the launch-failed dialog with the reason.
/// - Still starting: look again soon.
fn advance_start(ctx: &egui::Context, session: &mut Session, host: &Host, waiting: &Prompt) -> bool {
    let polled = host.runtime_dir().and_then(|runtime| host.launcher.poll(&runtime));
    match polled {
        Ok(Launched::Starting) => {
            ctx.request_repaint_after(START_LOOK);
            true
        }
        Ok(Launched::Ready(_)) => {
            prompt::take();
            retry(session, waiting, None);
            // The re-run may have left a question of its own.
            prompt::peek().is_some()
        }
        Err(err) => {
            prompt::set(Prompt::LaunchFailed { message: crate::peer::launch_failure_text(err) });
            true
        }
    }
}

/// Look at the renders. Returns true when the dialog still has something to draw.
///
/// - Still rendering: look again soon.
/// - Ended: the question goes and the call that was waiting runs again. It finds the movies in the
///   cache, or the reason a render failed, and shows that.
fn advance_prepare(ctx: &egui::Context, session: &mut Session, waiting: &Prompt) -> bool {
    match crate::prepare::state() {
        crate::prepare::State::Running { .. } => {
            ctx.request_repaint_after(START_LOOK);
            true
        }
        crate::prepare::State::Idle => {
            prompt::take();
            retry(session, waiting, None);
            prompt::peek().is_some()
        }
    }
}

fn draw(ctx: &egui::Context, host: &Host, waiting: &Prompt) -> Option<Answer> {
    let mut answer = None;
    egui::Window::new("Edit in EffectCraft").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).show(
        ctx,
        |ui| match waiting {
            Prompt::NotInstalled { action } => {
                ui.label("EffectCraft is not installed.");
                ui.label("Your project was not changed.");
                ui.horizontal(|ui| {
                    if ui.button(action.label()).clicked() {
                        answer = Some(Answer::Install);
                    }
                    if ui.button("Close").clicked() {
                        answer = Some(Answer::Dismiss);
                    }
                });
            }
            Prompt::Starting { .. } => {
                ui.label("Starting EffectCraft");
                match host.launcher.progress() {
                    Some(progress) => {
                        let fraction = progress.elapsed.as_secs_f32() / progress.ceiling.as_secs_f32().max(f32::EPSILON);
                        ui.add(egui::ProgressBar::new(fraction.clamp(0.0, 1.0)).text(format!("{} s", progress.elapsed.as_secs())));
                    }
                    None => {
                        ui.spinner();
                    }
                }
                if ui.button("Cancel").clicked() {
                    answer = Some(Answer::CancelStart);
                }
            }
            Prompt::Preparing { .. } => {
                ui.label("Preparing clip for EffectCraft");
                match crate::prepare::state() {
                    crate::prepare::State::Running { done, total } if total > 0 => {
                        ui.add(egui::ProgressBar::new((done as f32 / total as f32).clamp(0.0, 1.0)).text(format!("{done} / {total} frames")));
                    }
                    _ => {
                        ui.spinner();
                    }
                }
                if ui.button("Cancel").clicked() {
                    answer = Some(Answer::CancelPrepare);
                }
            }
            Prompt::PrepareFailed { message } => {
                ui.label("The clip could not be prepared for EffectCraft.");
                ui.label(message);
                ui.label("Your project was not changed.");
                if ui.button("Close").clicked() {
                    answer = Some(Answer::Dismiss);
                }
            }
            Prompt::LaunchFailed { message } => {
                ui.label("EffectCraft did not start.");
                ui.label(message);
                ui.label("Your project was not changed.");
                if ui.button("Close").clicked() {
                    answer = Some(Answer::Dismiss);
                }
            }
            Prompt::UpdateApp { message, .. } => {
                ui.label(message);
                ui.label("Your project was not changed.");
                if ui.button("Close").clicked() {
                    answer = Some(Answer::Dismiss);
                }
            }
            Prompt::ChooseInstance { choices } => {
                ui.label("More than one EffectCraft window is open. Which one should get this?");
                for choice in choices {
                    let docs = if choice.docs.is_empty() { "no document".to_string() } else { choice.docs.join(", ") };
                    let mark = if choice.holds_link { "  (has this link)" } else { "" };
                    if ui.button(format!("EffectCraft {} (pid {}): {docs}{mark}", choice.app_version, choice.pid)).clicked() {
                        answer = Some(Answer::Choose(choice.instance.clone()));
                    }
                }
                if ui.button("Cancel").clicked() {
                    answer = Some(Answer::Dismiss);
                }
            }
        },
    );
    answer
}

/// Carry out `answer` to `waiting`. A re-run goes through `Session::execute`, so a new prompt or
/// an error reaches the user the same way the first attempt did.
pub fn apply(session: &mut Session, waiting: &Prompt, answer: Answer) {
    match answer {
        Answer::Dismiss | Answer::CancelStart | Answer::CancelPrepare => {}
        Answer::Install => {
            if let Prompt::NotInstalled { action } = waiting
                && let Err(err) = action.run()
            {
                session.log.push(Level::Warning, "link.editIn", err);
            }
        }
        Answer::Choose(instance) => retry(session, waiting, Some(&instance)),
    }
}

fn retry(session: &mut Session, waiting: &Prompt, choice: Option<&str>) {
    let Some(params) = waiting.retry_params(choice) else {
        session.log.push(Level::Warning, "link.editIn", "that answer does not apply to this question");
        return;
    };
    // The result and any error are logged by `execute`. A prompt it leaves is drawn next frame.
    let _ = session.execute("link.editIn", params);
}

/// An app name in the banner text and the colour family it is drawn in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Brand {
    FilmCraft,
    EffectCraft,
}

/// Split `text` at the app names. `Some` marks a name.
pub fn brand_segments(text: &str) -> Vec<(&str, Option<Brand>)> {
    let mut out = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let hit = [("FilmCraft", Brand::FilmCraft), ("EffectCraft", Brand::EffectCraft)]
            .into_iter()
            .filter_map(|(name, brand)| rest.find(name).map(|at| (at, name, brand)))
            .min_by_key(|(at, _, _)| *at);
        match hit {
            Some((at, name, brand)) => {
                if at > 0 {
                    out.push((&rest[..at], None));
                }
                out.push((&rest[at..at + name.len()], Some(brand)));
                rest = &rest[at + name.len()..];
            }
            None => {
                out.push((rest, None));
                break;
            }
        }
    }
    out
}

/// Blue for FilmCraft and orange for EffectCraft. They differ on the blue and yellow axis and in
/// brightness, so red-green colour blindness does not merge them. The name is also bold text.
pub fn brand_color(brand: Brand, dark: bool) -> egui::Color32 {
    match (brand, dark) {
        (Brand::FilmCraft, true) => egui::Color32::from_rgb(0x5a, 0x9b, 0xff),
        (Brand::FilmCraft, false) => egui::Color32::from_rgb(0x0a, 0x3f, 0xa8),
        (Brand::EffectCraft, true) => egui::Color32::from_rgb(0xff, 0xc2, 0x66),
        (Brand::EffectCraft, false) => egui::Color32::from_rgb(0xb3, 0x59, 0x00),
    }
}

/// One line of text with the app names bold and coloured.
fn brand_line(ui: &mut egui::Ui, text: &str) {
    let dark = ui.visuals().dark_mode;
    let bold = egui::FontFamily::Name("semibold".into());
    let has_bold = ui.ctx().fonts(|fonts| fonts.definitions().families.contains_key(&bold));
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for (part, brand) in brand_segments(text) {
            let mut rich = egui::RichText::new(part);
            if let Some(brand) = brand {
                rich = rich.color(brand_color(brand, dark)).strong();
                if has_bold {
                    rich = rich.family(bold.clone());
                }
            }
            ui.label(rich);
        }
    });
}

/// Name `ui.inspect` reports in `dialog` is for blocking questions only. The notice is a banner and
/// reports through [`notice::peek`].
fn show_notice(ctx: &egui::Context, session: &mut Session, host: &Host, translate: &dyn Fn(&str) -> String) {
    let Some(pending) = notice::peek() else { return };
    // Any later edit makes the nest part of the user's history. The banner is answered by moving on.
    if !notice::undoable(session) {
        notice::take();
        return;
    }
    let mut answer = None;
    egui::Area::new(egui::Id::unique("link-notice")).order(egui::Order::Foreground).anchor(egui::Align2::CENTER_BOTTOM, [0.0, -56.0]).show(ctx, |ui| {
        egui::Frame::popup(ui.style()).show(ui, |ui| {
            ui.set_max_width(560.0);
            brand_line(ui, &translate(notice::CONTINUE_TEXT));
            let effects = pending.effects();
            if !effects.is_empty() {
                brand_line(ui, &format!("{} {}", translate(notice::EFFECTS_LABEL), effects.join(", ")));
            }
            for line in pending.details().into_iter().skip(usize::from(!effects.is_empty())) {
                brand_line(ui, &line);
            }
            ui.horizontal(|ui| {
                if ui.button(translate(notice::CONTINUE_LABEL)).clicked() {
                    answer = Some(true);
                }
                if ui.button(translate(notice::UNDO_LABEL)).clicked() {
                    answer = Some(false);
                }
            });
        });
    });
    match answer {
        Some(true) => {
            notice::take();
        }
        Some(false) => {
            notice::take();
            let message = undo_notice(session, host, &pending);
            session.toast(message);
        }
        None => {}
    }
}

/// Undo the hand-off of `pending`: find the EffectCraft that holds it and let [`notice::undo`] run.
fn undo_notice(session: &mut Session, host: &Host, pending: &notice::LinkNotice) -> String {
    let mut peer = crate::peer::SocketPeer::connect(
        host,
        &crate::peer::PeerChoice { instance: (!pending.instance.is_empty()).then_some(pending.instance.as_str()), holds_project: None },
        false,
    )
    .ok();
    let peer = peer.as_mut().map(|peer| peer as &mut dyn crate::edit_in::LinkPeer);
    notice::undo(session, peer, pending).unwrap_or_else(|why| why)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::fixtures::{linked_offline, write_movie};

    fn collect_text(shape: &egui::Shape, out: &mut Vec<String>) {
        match shape {
            egui::Shape::Text(text) => out.push(text.galley.text().to_string()),
            egui::Shape::Vec(shapes) => shapes.iter().for_each(|inner| collect_text(inner, out)),
            _ => {}
        }
    }

    /// The text the last of a few frames of `draw` paints. An `Area` is hidden on its first frame,
    /// while it measures itself. Both banners use the same `Area` id, so counting layers cannot tell
    /// one banner from two. Counting painted text can.
    fn painted_text(mut draw: impl FnMut(&egui::Context)) -> Vec<String> {
        let ctx = egui::Context::default();
        let mut texts = Vec::new();
        for _ in 0..4 {
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| draw(&ui.ctx().clone()));
            output.textures_delta.clear();
            texts.clear();
            output.shapes.iter().for_each(|clipped| collect_text(&clipped.shape, &mut texts));
        }
        texts
    }

    /// The hand-off banner is drawn once per frame. Drawn twice, the English banner would sit on
    /// top of the translated one.
    #[test]
    fn each_entry_point_draws_the_banner_once() {
        let _exclusive = crate::manager::exclusive();
        let mut session = Session::default();
        session.history.undo.push((notice::STEP.to_string(), std::sync::Arc::clone(&session.project), None));
        notice::set(notice::LinkNotice { link_id: "l".into(), instance: String::new(), lifted: vec!["Blur".into()], dropped: Vec::new() });
        let sentence = |texts: &[String]| texts.iter().filter(|text| text.contains("Click Continue")).cloned().collect::<Vec<_>>();

        let translated = painted_text(|ctx| show_translated(ctx, &mut session, &|text| format!("translated {text}")));
        let drawn = sentence(&translated);
        assert_eq!(drawn.len(), 1, "show_translated painted the sentence {} times: {drawn:?}", drawn.len());
        assert!(drawn[0].starts_with("translated "), "the sentence on screen is the translated one: {drawn:?}");

        let plain = painted_text(|ctx| show_with(ctx, &mut session, &crate::host::host()));
        let drawn = sentence(&plain);
        assert_eq!(drawn.len(), 1, "show_with painted the sentence {} times: {drawn:?}", drawn.len());
        assert!(!drawn[0].starts_with("translated "));
        notice::take();
    }

    /// The per-frame hook is what brings a result online when no `link.refresh` arrives.
    #[test]
    fn a_frame_brings_a_new_result_online() {
        let _exclusive = crate::manager::exclusive();
        let dir = std::env::temp_dir().join(format!("fc-link-frame-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("result.mov").to_string_lossy().into_owned();
        let mut session = Session::default();
        let item = linked_offline(&mut session, "b0516273-4e8f-4091-a2a3-5e6f708192a3", &path);
        write_movie(&path, 160, 90, 24);
        // The prompt slot is shared by every test in the process, so only the result watch runs here.
        // The process poller looks once a second and other tests share it, so frames run until it looks.
        let until = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while session.project.item(item).unwrap().as_media().unwrap().offline && std::time::Instant::now() < until {
            watch_results(&egui::Context::default(), &mut session);
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let media = session.project.item(item).unwrap().as_media().unwrap();
        assert!(!media.offline, "the frame hook left the finished result offline");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
