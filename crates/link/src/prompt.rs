//! Questions `link.editIn` puts to the user, and the one-slot mailbox the UI reads them from.
//!
//! A command cannot open a dialog: it runs inside `Session::execute`, and egui draws dialogs
//! between frames. So a command that needs an answer fails, rolls the project back, and leaves a
//! [`Prompt`] in the slot. [`crate::ui::show`] draws it. The answer re-runs the command with
//! parameters that settle the question ([`Prompt::retry_params`]).

use std::path::PathBuf;
use std::process::Command;
use std::sync::{Mutex, PoisonError};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Catalog id of EffectCraft in discovery records.
pub const EFFECTCRAFT_APP: &str = "effectcraft";
/// macOS bundle id of EffectCraft, for `yield_to`.
pub const EFFECTCRAFT_BUNDLE_ID: &str = "ai.storyteller.effectcraft";
/// Name the launcher binary is searched under.
pub const LAUNCHER_APP: &str = "artcraft-launcher";
/// Where a person gets EffectCraft when no launcher is installed.
pub const DOWNLOAD_URL: &str = "https://getartcraft.com/apps/effectcraft";

/// What the install button does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum InstallAction {
    /// Start the ArtCraft launcher found on this machine.
    OpenLauncher(PathBuf),
    /// Open the download page in the browser.
    OpenUrl(String),
}

impl InstallAction {
    /// The launcher when this machine has one, else the download page.
    pub fn resolve() -> Self {
        match craft_ipc::launch::resolve(LAUNCHER_APP) {
            Ok(path) => Self::OpenLauncher(path),
            Err(_) => Self::OpenUrl(DOWNLOAD_URL.to_string()),
        }
    }

    /// Text for the button.
    pub fn label(&self) -> &'static str {
        match self {
            Self::OpenLauncher(_) => "Open the ArtCraft launcher",
            Self::OpenUrl(_) => "Open the download page",
        }
    }

    /// Start the launcher or the browser. Does not wait for it.
    pub fn run(&self) -> Result<(), String> {
        let mut command = match self {
            Self::OpenLauncher(path) => Command::new(path),
            Self::OpenUrl(url) => open_url_command(url)?,
        };
        let mut child = command.spawn().map_err(|err| format!("could not start {}: {err}", self.label()))?;
        // Reap the child so it does not stay a zombie. The thread ends when the child exits.
        std::thread::Builder::new()
            .name("craft-ipc-install".into())
            .spawn(move || {
                let _ = child.wait();
            })
            .map_err(|err| err.to_string())?;
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn open_url_command(url: &str) -> Result<Command, String> {
    let mut command = Command::new("xdg-open");
    command.arg(url);
    Ok(command)
}

#[cfg(target_os = "macos")]
fn open_url_command(url: &str) -> Result<Command, String> {
    let mut command = Command::new("open");
    command.arg(url);
    Ok(command)
}

#[cfg(windows)]
fn open_url_command(url: &str) -> Result<Command, String> {
    let mut command = Command::new("cmd");
    command.args(["/C", "start", "", url]);
    Ok(command)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn open_url_command(_url: &str) -> Result<Command, String> {
    Err("this platform has no URL opener".to_string())
}

/// One running EffectCraft instance the user can pick.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InstanceChoice {
    pub instance: String,
    pub pid: u32,
    pub app_version: String,
    /// Titles of the open documents.
    pub docs: Vec<String>,
    /// True when this instance has the link's project open.
    pub holds_link: bool,
}

/// A part of the selection EffectCraft could not keep (`link.open` reply `unsupported`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unsupported {
    pub what: String,
    pub name: String,
    #[serde(default)]
    pub kept_as: String,
}

impl Unsupported {
    /// One line for the dialog.
    pub fn line(&self) -> String {
        if self.kept_as.is_empty() || self.kept_as == "none" {
            format!("{} \"{}\" is dropped", self.what, self.name)
        } else {
            format!("{} \"{}\" is kept as {}", self.what, self.name, self.kept_as)
        }
    }
}

/// A question for the user. Each variant says what the answer changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prompt {
    /// EffectCraft is missing. The answer is to run the install action, or dismiss.
    NotInstalled { action: InstallAction },
    /// The two apps share no protocol version. `app` is the one to update. Dismiss only.
    UpdateApp { app: String, message: String },
    /// EffectCraft was started and did not become ready. Dismiss only.
    LaunchFailed { message: String },
    /// EffectCraft was started and is not ready yet. The dialog shows progress and a Cancel button.
    /// `retry` holds the parameters of the `link.editIn` call. The dialog runs it again when
    /// EffectCraft is ready.
    Starting { retry: Value },
    /// FilmCraft renders a source that has no file, for EffectCraft. The dialog shows progress and a
    /// Cancel button, and runs `retry` (the parameters of the `link.editIn` call) when the render ends.
    Preparing { retry: Value },
    /// The render of a source failed. Dismiss only.
    PrepareFailed { message: String },
    /// Several instances run. The answer picks one.
    ChooseInstance { choices: Vec<InstanceChoice> },
}

impl Prompt {
    /// Parameters for the re-run of `link.editIn` after `choice` (an instance uuid) or after
    /// a plain re-run (`None`). `None` for a prompt that has no re-run.
    pub fn retry_params(&self, choice: Option<&str>) -> Option<Value> {
        match self {
            Self::ChooseInstance { choices } => {
                let picked = choice.filter(|id| choices.iter().any(|c| c.instance == *id))?;
                Some(json!({ "instance": picked }))
            }
            Self::Starting { retry } | Self::Preparing { retry } => Some(retry.clone()),
            Self::NotInstalled { .. } | Self::UpdateApp { .. } | Self::LaunchFailed { .. } | Self::PrepareFailed { .. } => None,
        }
    }
}

impl Prompt {
    /// The name `ui.inspect` reports in `dialog` while this prompt shows.
    pub fn dialog_name(&self) -> &'static str {
        match self {
            Self::NotInstalled { .. } => "LinkNotInstalled",
            Self::UpdateApp { .. } => "LinkUpdateApp",
            Self::LaunchFailed { .. } => "LinkLaunchFailed",
            Self::Starting { .. } | Self::Preparing { .. } => "LinkStarting",
            Self::PrepareFailed { .. } => "LinkPrepareFailed",
            Self::ChooseInstance { .. } => "LinkChooseInstance",
        }
    }
}

static SLOT: Mutex<Option<Prompt>> = Mutex::new(None);

/// Leave `prompt` for the UI. A later prompt replaces an earlier one.
pub fn set(prompt: Prompt) {
    *SLOT.lock().unwrap_or_else(PoisonError::into_inner) = Some(prompt);
}

/// The waiting prompt, without removing it. The UI draws it every frame until answered.
pub fn peek() -> Option<Prompt> {
    SLOT.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

/// Name of the waiting prompt's dialog, for `ui.inspect`. `None` when no prompt waits.
pub fn dialog_name() -> Option<String> {
    peek().map(|prompt| prompt.dialog_name().to_string())
}

/// Remove and return the waiting prompt.
pub fn take() -> Option<Prompt> {
    SLOT.lock().unwrap_or_else(PoisonError::into_inner).take()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choice(id: &str) -> InstanceChoice {
        InstanceChoice { instance: id.to_string(), pid: 1, app_version: "1".to_string(), docs: Vec::new(), holds_link: false }
    }

    #[test]
    fn choose_instance_retry_names_only_a_listed_instance() {
        let prompt = Prompt::ChooseInstance { choices: vec![choice("a"), choice("b")] };
        assert_eq!(prompt.retry_params(Some("b")), Some(json!({"instance": "b"})));
        assert_eq!(prompt.retry_params(Some("zzz")), None, "an instance that was not offered");
        assert_eq!(prompt.retry_params(None), None, "no choice made");
    }

    #[test]
    fn every_prompt_has_its_own_dialog_name() {
        let names = [
            Prompt::NotInstalled { action: InstallAction::OpenUrl(DOWNLOAD_URL.into()) }.dialog_name(),
            Prompt::UpdateApp { app: String::new(), message: String::new() }.dialog_name(),
            Prompt::LaunchFailed { message: String::new() }.dialog_name(),
            Prompt::Starting { retry: json!({}) }.dialog_name(),
            Prompt::ChooseInstance { choices: Vec::new() }.dialog_name(),
        ];
        let distinct: std::collections::BTreeSet<_> = names.iter().collect();
        assert_eq!(distinct.len(), names.len(), "{names:?}");
    }

    #[test]
    fn a_starting_prompt_retries_the_call_that_began_it() {
        let params = json!({"instance": "i1"});
        assert_eq!(Prompt::Starting { retry: params.clone() }.retry_params(None), Some(params));
    }

    #[test]
    fn dead_end_prompts_have_no_retry() {
        assert_eq!(Prompt::UpdateApp { app: "EffectCraft".into(), message: String::new() }.retry_params(None), None);
        assert_eq!(Prompt::NotInstalled { action: InstallAction::OpenUrl(DOWNLOAD_URL.into()) }.retry_params(None), None);
        assert_eq!(Prompt::LaunchFailed { message: String::new() }.retry_params(None), None);
    }

    #[test]
    fn unsupported_line_says_dropped_or_kept() {
        let dropped = Unsupported { what: "effect".into(), name: "Lumetri Color".into(), kept_as: "none".into() };
        assert_eq!(dropped.line(), "effect \"Lumetri Color\" is dropped");
        let kept = Unsupported { what: "effect".into(), name: "Blur".into(), kept_as: "rendered picture".into() };
        assert_eq!(kept.line(), "effect \"Blur\" is kept as rendered picture");
    }
}
