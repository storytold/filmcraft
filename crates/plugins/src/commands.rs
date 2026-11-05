//! The host's own commands. Owner `app.filmcraft`, ids `plugin.app.filmcraft.<verb>`.
//!
//! They make the plug-in manager drivable from the control channel, the command palette and the
//! MCP server, like every FilmCraft command. A plug-in cannot list or run them
//! ([`crate::services::HOST_PREFIX`]).
//!
//! | Id suffix | Params | Result |
//! |---|---|---|
//! | `manage` | none | opens the manager window; menu Plugins |
//! | `list` | none | `{"plugins": [...]}` |
//! | `install` | `{"path"}` | `{"outcome": ...}`; a plug-in that asks for permissions waits for `grant` |
//! | `grant` | `{"id", "accept"?, "optional"?}` | `{"outcome": ...}` |
//! | `enable`, `disable`, `remove` | `{"id"}` | `{"ok": true}` |
//! | `revoke` | `{"id", "permission"}` | `{"ok": true}` |

use std::path::Path;
use std::sync::{Arc, Weak};

use craft_command::{DynCommand, Enablement, Owner, Visibility};
use craft_plugin_abi::Permission;
use craft_plugin_host::lifecycle::{InstallOutcome, PluginState};
use filmcraft_engine::{EngineError, Session};
use serde_json::{Value, json};

use craft_plugin_ui::manager::state_text;

use crate::host::PluginHost;
use crate::services::HOST_PREFIX;

/// The owner of the host commands.
pub const OWNER: &str = "app.filmcraft";

fn other(msg: impl std::fmt::Display) -> EngineError {
    EngineError::Other(msg.to_string())
}

fn text<'a>(params: &'a Value, name: &str) -> Result<&'a str, EngineError> {
    params.get(name).and_then(Value::as_str).ok_or_else(|| EngineError::BadParams { cmd: "plugin".into(), msg: format!("`{name}` must be a string") })
}

/// JSON of an install outcome.
pub fn outcome_json(outcome: &InstallOutcome) -> Value {
    match outcome {
        InstallOutcome::Loaded { generation, replaced } => json!({ "outcome": "loaded", "generation": generation, "replaced": replaced }),
        InstallOutcome::NeedsGrant(r) => json!({
            "outcome": "needs-grant",
            "id": r.plugin_id,
            "version": r.version,
            "publisher": r.publisher,
            "required": r.required.iter().map(ToString::to_string).collect::<Vec<_>>(),
            "new_required": r.new_required.iter().map(ToString::to_string).collect::<Vec<_>>(),
            "optional": r.optional.iter().map(ToString::to_string).collect::<Vec<_>>(),
        }),
        InstallOutcome::StoredDisabled => json!({ "outcome": "stored-disabled" }),
        InstallOutcome::UpdateDeclined => json!({ "outcome": "update-declined" }),
    }
}

fn list_json(host: &PluginHost) -> Value {
    let mut rows: Vec<Value> = host
        .manager
        .list()
        .iter()
        .filter_map(|id| host.manager.info(id))
        .map(|i| {
            json!({
                "id": i.id,
                "version": i.version,
                "state": state_text(&i.state),
                "enabled": i.enabled,
                "commands": i.commands,
                "hooks": { "registered": i.hooks.registered, "skipped": i.hooks.skipped.iter().map(|s| json!({"contribution": s.contribution, "reason": s.reason})).collect::<Vec<_>>() },
                "surfaces": { "registered": i.surfaces.registered, "skipped": i.surfaces.skipped.iter().map(|s| json!({"contribution": s.contribution, "reason": s.reason})).collect::<Vec<_>>() },
                "unsupported_kinds": i.unsupported_kinds,
                "grants": i.grants,
                "update_pending": i.update_pending,
                "legacy": i.legacy.as_ref().map(|l| json!({"abi": format!("{:?}", l.abi), "limits": l.limits})),
            })
        })
        .collect();
    // Packages `restore` could not load have no manifest to list. They are still installed.
    for (id, state) in &host.restored {
        if matches!(state, PluginState::Faulted { .. }) && !rows.iter().any(|r| r["id"] == json!(id)) {
            rows.push(json!({ "id": id, "state": state_text(state), "enabled": false }));
        }
    }
    json!({ "plugins": rows })
}

type Run = Arc<dyn Fn(&mut Session, &Value) -> Result<Value, EngineError> + Send + Sync>;

fn row(id: &str, label: &str, params: &str, menu: &[&str], run: Run) -> DynCommand<Session, EngineError> {
    DynCommand {
        id: Arc::from(format!("{HOST_PREFIX}{id}")),
        label: Arc::from(label),
        params: Arc::from(params),
        shortcut: None,
        menu: menu.iter().map(|part| Arc::from(*part)).collect(),
        owner: Owner { plugin_id: Arc::from(OWNER), generation: 1 },
        enablement: Enablement::Always,
        visibility: Visibility { menu: !menu.is_empty(), palette: true, shortcut: false },
        journal: false,
        run,
    }
}

/// Run `f` with the host if it is still alive.
fn with_host(weak: &Weak<PluginHost>, f: impl Fn(&Arc<PluginHost>, &Value) -> Result<Value, EngineError> + Send + Sync + 'static) -> Run {
    let weak = Weak::clone(weak);
    Arc::new(move |_session: &mut Session, params: &Value| {
        let host = weak.upgrade().ok_or_else(|| other("the plug-in host has shut down"))?;
        f(&host, params)
    })
}

fn id_arg(params: &Value) -> Result<String, EngineError> {
    text(params, "id").map(str::to_owned)
}

/// Register every host command.
pub fn register(host: &Arc<PluginHost>) -> Result<(), String> {
    let weak = Arc::downgrade(host);
    let rows = vec![
        row(
            "manage",
            "Manage Plug-ins...",
            "{}",
            &["Plugins"],
            with_host(&weak, |h, _| {
                h.ui_state().manager_open = true;
                Ok(json!({ "dialog": "plugins" }))
            }),
        ),
        row("list", "List Plug-ins", "{}", &[], with_host(&weak, |h, _| Ok(list_json(h)))),
        row(
            "install",
            "Install Plug-in",
            r#"{"path":str}"#,
            &[],
            with_host(&weak, |h, p| {
                let path = text(p, "path")?;
                h.install_path(Path::new(path)).map(|o| outcome_json(&o)).map_err(other)
            }),
        ),
        row(
            "grant",
            "Answer Plug-in Permission Prompt",
            r#"{"id":str,"accept":bool?,"optional":[str]?}"#,
            &[],
            with_host(&weak, |h, p| {
                let id = id_arg(p)?;
                let accept = p.get("accept").and_then(Value::as_bool).unwrap_or(true);
                let mut optional = Vec::new();
                for name in p.get("optional").and_then(Value::as_array).into_iter().flatten() {
                    let name = name.as_str().ok_or_else(|| other("`optional` must list strings"))?;
                    optional.push(name.parse::<Permission>().map_err(other)?);
                }
                h.answer(&id, accept, optional).map(|o| outcome_json(&o)).map_err(other)
            }),
        ),
        row("enable", "Enable Plug-in", r#"{"id":str}"#, &[], with_host(&weak, |h, p| h.manager.enable(&id_arg(p)?).map(|o| outcome_json(&o)).map_err(other))),
        row(
            "disable",
            "Disable Plug-in",
            r#"{"id":str}"#,
            &[],
            with_host(&weak, |h, p| h.manager.disable(&id_arg(p)?).map(|()| json!({"ok": true})).map_err(other)),
        ),
        row(
            "remove",
            "Remove Plug-in",
            r#"{"id":str}"#,
            &[],
            with_host(&weak, |h, p| h.manager.remove(&id_arg(p)?).map(|()| json!({"ok": true})).map_err(other)),
        ),
        row(
            "revoke",
            "Revoke Plug-in Permission",
            r#"{"id":str,"permission":str}"#,
            &[],
            with_host(&weak, |h, p| {
                let permission = text(p, "permission")?.parse::<Permission>().map_err(other)?;
                h.manager.revoke(&id_arg(p)?, &permission).map(|()| json!({"ok": true})).map_err(other)
            }),
        ),
    ];
    for cmd in rows {
        let id = Arc::clone(&cmd.id);
        host.registry.register(cmd).map_err(|e| format!("{id}: {e}"))?;
    }
    Ok(())
}
