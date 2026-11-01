//! Where commands go: an in-process session, or the desktop app's control channel.

use std::path::Path;

use filmcraft_automation::BridgeClient;
use filmcraft_engine::Session;
use serde_json::{Value, json};

use crate::cli::Global;
use crate::error::{CliError, ExecError, Failure, UsageError};

pub enum Backend {
    Local(Box<Session>),
    Bridge(BridgeClient),
}

/// A path as a command parameter.
pub fn path_param(p: &Path) -> Value {
    json!(p.to_string_lossy())
}

impl Backend {
    pub fn open(g: &Global) -> Result<Self, CliError> {
        if let Some(addr) = &g.bridge {
            return BridgeClient::new(addr.0.as_str()).map(Backend::Bridge).map_err(|e| UsageError::Bridge(e).into());
        }
        let mut s = Session::default();
        // user export presets (and other per-user libraries) from the data directory
        if let Some(dir) = g.data_dir.clone().or_else(filmcraft_engine::autosave::default_data_dir) {
            s.export_presets.set_dir(&dir);
        }
        if let Some(p) = &g.project {
            s.execute("file.open", json!({"path": path_param(p)})).map_err(|e| Failure::Backend(e.into()))?;
        } else if g.demo {
            let _ = s.execute("file.openDemoProject", json!({}));
        }
        Ok(Backend::Local(Box::new(s)))
    }

    pub async fn exec(&mut self, id: &str, mut params: Value) -> Result<Value, ExecError> {
        match self {
            // this process exits after the command, so a background export would never be written:
            // long commands wait unless the caller says otherwise (#2342)
            Backend::Local(s) => {
                if filmcraft_automation::long_job::LONG_COMMANDS.contains(&id)
                    && let Some(p) = params.as_object_mut()
                {
                    // anything but an explicit `false` (missing, null, a typo'd value) waits
                    if p.get("wait").and_then(Value::as_bool) != Some(false) {
                        p.insert("wait".into(), json!(true));
                    }
                }
                Ok(s.execute(id, params)?)
            }
            // a blocking export runs as an app job polled by the client (#91, #92)
            Backend::Bridge(b) => Ok(b.execute(id, params).await?),
        }
    }

    /// Save to the --project path (`--save`) or to `--save-as`. Bridge mode saves in the app.
    pub async fn finish(&mut self, g: &Global) -> Result<(), Failure> {
        let r = if let Some(p) = &g.save_as {
            self.exec("file.saveAs", json!({"path": path_param(p)})).await
        } else if g.save {
            match &g.project {
                Some(p) => self.exec("file.save", json!({"path": path_param(p)})).await,
                None => self.exec("file.save", json!({})).await,
            }
        } else {
            return Ok(());
        };
        r.map(drop).map_err(Failure::Save)
    }
}
