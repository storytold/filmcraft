//! Registers the Craft Link commands with a session.
//!
//! | Command | Kind | Why |
//! |---|---|---|
//! | `link.editIn` | static row in `filmcraft-engine`, run through [`set_link_edit_in`](filmcraft_engine::commands::set_link_edit_in) | `Registry::register` reserves every id that does not start with `plugin.<owner>.`, so a dynamic command cannot be called `link.editIn` |
//! | `plugin.craft.link.focus` | dynamic, owner `craft.link` | the wire method `link.focus` maps to it in `control.rs` |
//! | `plugin.craft.link.unlink` | dynamic, owner `craft.link`, Clip menu | |
//! | `plugin.craft.link.refresh` | dynamic, owner `craft.link` | the wire method `link.refresh` maps to it |

use std::sync::Arc;

use craft_command::{DynCommand, Enablement, Owner, Visibility};
use filmcraft_engine::panels::Level;
use filmcraft_engine::{EngineError, Session};
use serde_json::Value;

use crate::edit_in::{EditInError, EditInOptions, edit_in, edit_in_enabled, require_saved};
use crate::host::{Host, host};
use crate::link_ops;
use crate::notice;
use crate::peer::{PeerChoice, SocketPeer};
use crate::prompt;

/// Owner id of the dynamic commands.
pub const PLUGIN_ID: &str = "craft.link";
/// Dynamic command ids. The wire methods are these without `plugin.craft.`.
pub const FOCUS_ID: &str = "plugin.craft.link.focus";
pub const UNLINK_ID: &str = "plugin.craft.link.unlink";
pub const REFRESH_ID: &str = "plugin.craft.link.refresh";

/// Point `link.editIn` at [`run_edit_in`] and register the dynamic commands on `session`.
///
/// Call once per process before the first menu is built, after the session exists. A failure is
/// written to the session log: the app still runs, the commands that failed are missing.
pub fn install(session: &mut Session) {
    // The first call wins. A second install in one process keeps the first hook, which is the same function.
    let _ = filmcraft_engine::commands::set_link_edit_in(run_installed);
    if let Err(err) = register(session) {
        session.log.push(Level::Error, "link", format!("Craft Link commands are not available: {err}"));
    }
}

/// Register the dynamic commands. An error names the command that was refused.
pub fn register(session: &Session) -> Result<(), String> {
    let owner = Owner { plugin_id: Arc::from(PLUGIN_ID), generation: 1 };
    let hidden = Visibility { menu: false, palette: true, shortcut: false };
    let rows = [
        row(
            &owner,
            FOCUS_ID,
            "Focus Link",
            r#"{"link_id":str,"place":object?,"activation":object?}"#,
            &[],
            Enablement::HasDocument,
            hidden,
            false,
            Arc::new(|session: &mut Session, params: &Value| link_ops::focus(session, params)),
        ),
        row(
            &owner,
            UNLINK_ID,
            "Unlink from EffectCraft",
            r#"{"link_id":str?}"#,
            &["Clip"],
            Enablement::HasSelection,
            Visibility { menu: true, palette: true, shortcut: false },
            true,
            Arc::new(|session: &mut Session, params: &Value| link_ops::unlink(session, params)),
        ),
        row(
            &owner,
            REFRESH_ID,
            "Refresh Link",
            r#"{"link_id":str,"epoch":int?}"#,
            &[],
            Enablement::HasDocument,
            hidden,
            false,
            Arc::new(|session: &mut Session, params: &Value| link_ops::refresh(session, params)),
        ),
    ];
    for cmd in rows {
        let id = Arc::clone(&cmd.id);
        session.registry.register(cmd).map_err(|err| format!("{id}: {err}"))?;
    }
    Ok(())
}

type Run = Arc<dyn Fn(&mut Session, &Value) -> Result<Value, EngineError> + Send + Sync>;

#[allow(clippy::too_many_arguments)]
fn row(
    owner: &Owner,
    id: &str,
    label: &str,
    params: &str,
    menu: &[&str],
    enablement: Enablement,
    visibility: Visibility,
    journal: bool,
    run: Run,
) -> DynCommand<Session, EngineError> {
    DynCommand {
        id: Arc::from(id),
        label: Arc::from(label),
        params: Arc::from(params),
        shortcut: None,
        menu: menu.iter().map(|part| Arc::from(*part)).collect(),
        owner: owner.clone(),
        enablement,
        visibility,
        journal,
        run,
    }
}

fn run_installed(session: &mut Session, params: &Value) -> filmcraft_engine::Result<Value> {
    // A new attempt answers the old question, whatever it was.
    prompt::take();
    // A new hand-off keeps the old one: the old banner is answered by moving on.
    notice::take();
    let reply = run_edit_in(session, params, &host()).map_err(|err| {
        if let Some(question) = err.prompt() {
            prompt::set(question);
        }
        EngineError::Other(err.to_string())
    })?;
    if let Some(pending) = notice::LinkNotice::from_reply(&reply) {
        notice::set(pending);
    }
    Ok(reply)
}

/// `link.editIn` on `host`: check, find or start EffectCraft, then [`edit_in`].
///
/// EffectCraft is reached before the project is touched, so a missing install, an unsupported
/// protocol, or an unanswered choice leaves the project exactly as it was.
pub fn run_edit_in(session: &mut Session, params: &Value, host: &Host) -> Result<Value, EditInError> {
    let opts = EditInOptions::from_params(params)?;
    require_saved(session).map_err(EditInError::Refused)?;
    edit_in_enabled(session).map_err(EditInError::Refused)?;
    // A clip that plays a linked result reopens that link, so prefer the window that holds it.
    let linked = crate::reopen::linked_selection(session).map_err(EditInError::Refused)?;
    let holds_project = linked.as_ref().map(|linked| linked.record.target_project.as_str());
    let mut peer = SocketPeer::connect(host, &PeerChoice { instance: opts.instance.as_deref(), holds_project }, opts.wait).map_err(|err| match err {
        // The dialog re-runs the command with the parameters it was given.
        EditInError::Starting(_) => EditInError::Starting(params.clone()),
        other => other,
    })?;
    edit_in(session, &mut peer, &*host.backend, &opts).map_err(|err| match err {
        // Same: the dialog runs the call again when the render of the missing sources ends.
        EditInError::Preparing(_) => EditInError::Preparing(params.clone()),
        other => other,
    })
}
