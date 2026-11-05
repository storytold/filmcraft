//! The socket side of `link.editIn`: find or start EffectCraft, say hello, send `link.open`.

use craft_ipc::activate::Activated;
use craft_ipc::client::{Client, HelloClient};
use craft_ipc::wire::{PROTOCOL_MAX, PROTOCOL_MIN};
use craft_ipc::{Discovery, InstanceRecord, LinkError};
use serde_json::{Value, json};

use crate::edit_in::{Attached, Closed, EditInError, LinkPeer};
use crate::host::{Host, Launched};
use crate::prompt::{EFFECTCRAFT_APP, InstallAction, InstanceChoice};

/// Which EffectCraft to talk to.
#[derive(Debug, Clone, Default)]
pub struct PeerChoice<'a> {
    /// The instance the user picked (uuid).
    pub instance: Option<&'a str>,
    /// Prefer the instance that has this EffectCraft project open (the link's sidecar project).
    pub holds_project: Option<&'a str>,
}

/// What [`select_instance`] decided.
#[derive(Debug, Clone, PartialEq)]
pub enum Selection {
    /// Talk to this one.
    One(Box<InstanceRecord>),
    /// Nothing runs. Start EffectCraft.
    Start,
    /// Several run and none is the obvious one. Ask.
    Choose(Vec<InstanceChoice>),
}

/// Pick among the running EffectCraft instances. Pure: no I/O.
///
/// 1. A named instance wins. If it is gone the answer is an error, not another instance.
/// 2. One instance runs: use it.
/// 3. Several run: the one that holds the link's project, if exactly one does.
/// 4. Otherwise ask.
pub fn select_instance(records: &[InstanceRecord], choice: &PeerChoice<'_>) -> Result<Selection, EditInError> {
    let running: Vec<&InstanceRecord> = records.iter().filter(|record| record.app == EFFECTCRAFT_APP).collect();
    if let Some(wanted) = choice.instance {
        return match running.iter().find(|record| record.instance == wanted) {
            Some(record) => Ok(Selection::One(Box::new((*record).clone()))),
            None => Err(EditInError::Failed("the EffectCraft window you chose has closed".to_string())),
        };
    }
    match running.as_slice() {
        [] => Ok(Selection::Start),
        [only] => Ok(Selection::One(Box::new((*only).clone()))),
        many => {
            let holders: Vec<&&InstanceRecord> = many.iter().filter(|record| holds(record, choice.holds_project)).collect();
            if let [holder] = holders.as_slice() {
                return Ok(Selection::One(Box::new((**holder).clone())));
            }
            Ok(Selection::Choose(many.iter().map(|record| choice_for(record, choice.holds_project)).collect()))
        }
    }
}

fn holds(record: &InstanceRecord, project: Option<&str>) -> bool {
    project.is_some_and(|wanted| record.docs.iter().any(|doc| doc.path.as_deref() == Some(wanted)))
}

fn choice_for(record: &InstanceRecord, project: Option<&str>) -> InstanceChoice {
    InstanceChoice {
        instance: record.instance.clone(),
        pid: record.pid,
        app_version: record.app_version.clone(),
        docs: record.docs.iter().map(|doc| doc.title.clone()).collect(),
        holds_link: holds(record, project),
    }
}

/// Which app a `protocol_unsupported` answer tells the user to update.
///
/// `ours` is this build's `(min, max)`. The peer answered with its own range.
pub fn app_to_update(ours: (u32, u32), server_min: u32, server_max: u32) -> &'static str {
    if ours.0 > server_max {
        "EffectCraft"
    } else if ours.1 < server_min {
        "FilmCraft"
    } else {
        "FilmCraft or EffectCraft"
    }
}

fn update_error(server_min: u32, server_max: u32) -> EditInError {
    let app = app_to_update((PROTOCOL_MIN, PROTOCOL_MAX), server_min, server_max);
    EditInError::UpdateApp {
        app,
        message: format!("Update {app}: FilmCraft speaks link protocol {PROTOCOL_MIN} to {PROTOCOL_MAX}, EffectCraft speaks {server_min} to {server_max}."),
    }
}

/// Map a link failure to the user-facing error. `NotInstalled` carries the install action.
fn map_link_error(err: LinkError) -> EditInError {
    match err {
        LinkError::NotInstalled { .. } => EditInError::NotInstalled(InstallAction::resolve()),
        LinkError::Protocol { server_min, server_max } => update_error(server_min, server_max),
        other => EditInError::Failed(other.to_string()),
    }
}

/// Map a failure of the start step. `NotInstalled` keeps its install action. Anything else means
/// FilmCraft started EffectCraft and it did not come up, which the user must see as a dialog.
fn map_launch_error(err: LinkError) -> EditInError {
    match err {
        LinkError::NotInstalled { .. } | LinkError::Protocol { .. } => map_link_error(err),
        other => EditInError::LaunchFailed(launch_failure_text(other)),
    }
}

/// The sentence the launch-failed dialog shows.
pub(crate) fn launch_failure_text(err: LinkError) -> String {
    match err {
        LinkError::NotReady(text) | LinkError::Io(text) => text,
        LinkError::Timeout => "EffectCraft did not become ready in time.".to_string(),
        other => other.to_string(),
    }
}

/// A connected, greeted EffectCraft instance.
pub struct SocketPeer {
    client: Client,
    record: InstanceRecord,
}

impl SocketPeer {
    /// Find a running EffectCraft or start one, then connect and say hello.
    ///
    /// Nothing in the project is touched, so every error here leaves it as it was.
    ///
    /// When EffectCraft must be started and `wait` is false, a window session gets
    /// [`EditInError::Starting`] at once and the start goes on in the background. `wait` blocks
    /// until EffectCraft is ready or has failed.
    pub fn connect(host: &Host, choice: &PeerChoice<'_>, wait: bool) -> Result<Self, EditInError> {
        let dir = host.runtime_dir().map_err(map_link_error)?;
        let records = Discovery::scan(&dir).map_err(map_link_error)?;
        let record = match select_instance(&records, choice)? {
            Selection::One(record) => *record,
            Selection::Choose(choices) => return Err(EditInError::ChooseInstance(choices)),
            Selection::Start => match host.launcher.launch(&dir, wait).map_err(map_launch_error)? {
                Launched::Ready(record) => *record,
                Launched::Starting => return Err(EditInError::Starting(Value::Null)),
            },
        };
        Self::greet(record)
    }

    fn greet(record: InstanceRecord) -> Result<Self, EditInError> {
        let mut client = Client::connect(&record).map_err(map_link_error)?;
        let identity = HelloClient {
            app: "filmcraft".to_string(),
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            pid: std::process::id(),
            instance: craft_ipc::random_uuid().map_err(map_link_error)?,
            protocol_min: PROTOCOL_MIN,
            protocol_max: PROTOCOL_MAX,
        };
        let hello = client.hello(&identity).map_err(map_link_error)?;
        let can_receive = hello.get("caps").and_then(Value::as_array).is_some_and(|caps| caps.iter().any(|cap| cap.as_str() == Some("link.receive")));
        if !can_receive {
            return Err(EditInError::UpdateApp {
                app: "EffectCraft",
                message: "Update EffectCraft: this version cannot receive a hand-off from FilmCraft (no link.receive).".to_string(),
            });
        }
        Ok(Self { client, record })
    }
}

impl LinkPeer for SocketPeer {
    fn instance(&self) -> &str {
        &self.record.instance
    }

    fn open(&mut self, params: Value) -> Result<Value, EditInError> {
        self.client.call("link.open", params).map_err(map_link_error)
    }

    fn close(&mut self, link_id: &str) -> Result<Closed, EditInError> {
        let reply = self.client.call("link.close", json!({ "link_id": link_id })).map_err(map_link_error)?;
        Ok(Closed {
            edited: reply.get("edited").and_then(Value::as_bool).unwrap_or(false),
            comp: reply.get("comp_name").and_then(Value::as_str).unwrap_or_default().to_string(),
        })
    }

    fn activate(&mut self, token: Option<&str>, link_id: &str) -> Result<Activated, EditInError> {
        let params = json!({ "token": token, "from_pid": std::process::id(), "open": { "link_id": link_id } });
        let result = self.client.call("app.activate", params).map_err(map_link_error)?;
        match result.get("activated") {
            Some(Value::Bool(true)) => Ok(Activated::Yes),
            Some(Value::Bool(false)) => Ok(Activated::No),
            Some(Value::String(text)) if text == "requested" => Ok(Activated::Requested),
            other => Err(EditInError::Failed(format!("app.activate returned an unknown `activated`: {other:?}"))),
        }
    }

    fn attach(&mut self, link_id: &str, project: &str, comp: &str) -> Result<Attached, EditInError> {
        match self.client.call("link.attach", json!({ "link_id": link_id, "project": project, "comp": comp })) {
            Ok(_) => Ok(Attached::Reopened),
            // `craft_ipc` has no variant for `comp_missing` (no link bound, no sidecar, no such
            // comp) or for `io_error` (a sidecar that does not open). Both arrive as `Invalid`.
            // Opening the link again from the nest answers either one.
            Err(LinkError::Invalid(why)) => Ok(Attached::Missing(why)),
            Err(other) => Err(map_link_error(other)),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use craft_ipc::{DocInfo, Endpoint};

    use super::*;

    fn record(app: &str, pid: u32, project: Option<&str>) -> InstanceRecord {
        let endpoint = Endpoint::Unix { path: format!("/tmp/{app}-{pid}.sock") };
        let mut record = InstanceRecord::new(app, "0.8.3", pid, endpoint).expect("record");
        if let Some(path) = project {
            record.docs.push(DocInfo { doc_id: format!("doc-{pid}"), title: format!("title {pid}"), path: Some(path.to_string()), extra: Default::default() });
        }
        record
    }

    #[test]
    fn no_instance_means_start() {
        assert_eq!(select_instance(&[], &PeerChoice::default()).unwrap(), Selection::Start);
        // Other apps do not count.
        let other = [record("filmcraft", 1, None)];
        assert_eq!(select_instance(&other, &PeerChoice::default()).unwrap(), Selection::Start);
    }

    #[test]
    fn one_instance_is_used() {
        let records = [record("effectcraft", 7, None), record("filmcraft", 8, None)];
        assert_eq!(select_instance(&records, &PeerChoice::default()).unwrap(), Selection::One(Box::new(records[0].clone())));
    }

    #[test]
    fn several_instances_ask_and_mark_the_holder() {
        let records = [record("effectcraft", 1, Some("/a.ecproj")), record("effectcraft", 2, Some("/b.ecproj"))];
        let Selection::Choose(choices) = select_instance(&records, &PeerChoice::default()).unwrap() else { panic!("expected a choice") };
        assert_eq!(choices.len(), 2);
        assert!(choices.iter().all(|c| !c.holds_link));
        let Selection::Choose(choices) = select_instance(&records, &PeerChoice { instance: None, holds_project: Some("/zzz.ecproj") }).unwrap() else {
            panic!("expected a choice")
        };
        assert!(choices.iter().all(|c| !c.holds_link), "no instance has that project");
    }

    #[test]
    fn several_instances_prefer_the_one_holding_the_link() {
        let records = [record("effectcraft", 1, Some("/a.ecproj")), record("effectcraft", 2, Some("/b.ecproj"))];
        let picked = select_instance(&records, &PeerChoice { instance: None, holds_project: Some("/b.ecproj") }).unwrap();
        assert_eq!(picked, Selection::One(Box::new(records[1].clone())));
    }

    #[test]
    fn two_holders_still_ask() {
        let records = [record("effectcraft", 1, Some("/b.ecproj")), record("effectcraft", 2, Some("/b.ecproj"))];
        let Selection::Choose(choices) = select_instance(&records, &PeerChoice { instance: None, holds_project: Some("/b.ecproj") }).unwrap() else {
            panic!("expected a choice")
        };
        assert!(choices.iter().all(|c| c.holds_link));
    }

    #[test]
    fn a_named_instance_wins_and_a_gone_one_is_an_error() {
        let records = [record("effectcraft", 1, None), record("effectcraft", 2, None)];
        let named = select_instance(&records, &PeerChoice { instance: Some(&records[1].instance), holds_project: None }).unwrap();
        assert_eq!(named, Selection::One(Box::new(records[1].clone())));
        let gone = select_instance(&records, &PeerChoice { instance: Some("no-such-instance"), holds_project: None });
        assert!(matches!(gone, Err(EditInError::Failed(_))), "{gone:?}");
    }

    #[test]
    fn the_older_app_is_named_for_update() {
        // We speak 1..=1. The server only speaks 2..=3: we are older.
        assert_eq!(app_to_update((1, 1), 2, 3), "FilmCraft");
        // We speak 4..=5. The server only speaks 1..=3: the server is older.
        assert_eq!(app_to_update((4, 5), 1, 3), "EffectCraft");
        assert_eq!(app_to_update((1, 3), 2, 4), "FilmCraft or EffectCraft");
    }

    #[test]
    fn protocol_refusal_names_the_app() {
        let err = map_link_error(LinkError::Protocol { server_min: PROTOCOL_MAX + 1, server_max: PROTOCOL_MAX + 2 });
        let EditInError::UpdateApp { app, message } = err else { panic!("expected UpdateApp") };
        assert_eq!(app, "FilmCraft");
        assert!(message.contains("Update FilmCraft"), "{message}");
    }

    #[test]
    fn a_failed_start_is_a_launch_failure_with_the_reason() {
        let err = map_launch_error(LinkError::NotReady("effectcraft stopped at start-up".to_string()));
        assert_eq!(err, EditInError::LaunchFailed("effectcraft stopped at start-up".to_string()));
        assert_eq!(err.prompt(), Some(crate::prompt::Prompt::LaunchFailed { message: "effectcraft stopped at start-up".to_string() }));
        assert!(matches!(map_launch_error(LinkError::Timeout), EditInError::LaunchFailed(_)));
        assert!(matches!(map_launch_error(LinkError::NotInstalled { app: "effectcraft".to_string() }), EditInError::NotInstalled(_)));
    }

    #[test]
    fn not_installed_carries_an_install_action() {
        let err = map_link_error(LinkError::NotInstalled { app: "effectcraft".to_string() });
        assert!(matches!(err, EditInError::NotInstalled(_)));
        assert_eq!(err.to_string(), "EffectCraft is not installed");
    }
}
