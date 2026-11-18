//! The `plugin.*` commands: list, install (a file, base64 data, a GitHub repository or a
//! catalog's package), update, remove and run plug-ins, and the catalogs they come from.

use serde_json::{Value, json};

use super::install::{self, request_key};
use super::{InstalledPlugin, MAX_CATALOGS, save_prefs};
use crate::commands::{CommandSpec, NamedKeys, always, bad, bool_p, str_p, u64_p};
use crate::{Result, Session};

fn list(s: &mut Session, _p: &Value) -> Result<Value> {
    let plugins: Vec<Value> = s.plugins.list().iter().map(InstalledPlugin::describe).collect();
    let last = s.plugins.last.as_ref().map(|(id, command, _)| json!({"id": id, "command": command}));
    Ok(json!({
        "plugins": plugins,
        "problems": s.plugins.problems(),
        "catalogs": s.prefs.plugin_data.catalogs,
        "stored": s.plugins.has_store(),
        "network": s.plugins.fetcher().has_network(),
        "last": last,
    }))
}

fn install_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "plugin.install";
    let request = request_key(p);
    let wanted = str_p(p, "sha256").map(str::to_string);
    let c = match wanted.as_deref().and_then(|h| install::take_held(s, h, &request)) {
        Some(c) => c,
        None => install::obtain(s, cmd, p)?,
    };
    let (_, mut info) = install::review(s, cmd, &c)?;
    let sha256 = info["sha256"].as_str().unwrap_or_default().to_string();
    if let Some(w) = &wanted
        && !w.eq_ignore_ascii_case(&sha256)
    {
        return Err(bad(cmd, format!("the bundle's SHA-256 is {sha256}, not the {w} that was reviewed: review it again")));
    }
    if bool_p(p, "review").unwrap_or(false) {
        install::hold(s, &sha256, request, c);
        info["installed"] = json!(false);
        return Ok(info);
    }
    install::install(s, cmd, c)?;
    info["installed"] = json!(true);
    Ok(info)
}

fn update(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "plugin.update";
    let only = str_p(p, "id");
    if let Some(id) = only
        && s.plugins.get(id).is_none()
    {
        return Err(bad(cmd, format!("no plug-in `{id}` is installed")));
    }
    let apply = bool_p(p, "apply").unwrap_or(false);
    let targets: Vec<InstalledPlugin> = s.plugins.list().iter().filter(|x| only.is_none_or(|id| x.manifest.id.eq_ignore_ascii_case(id))).cloned().collect();
    let (mut updates, mut updated, mut errors) = (vec![], vec![], vec![]);
    let fetcher = s.plugins.fetcher();
    for old in targets {
        let id = old.manifest.id.clone();
        let (params, available) = match fetcher.with(|f| super::update_for_with(f, &old)) {
            Ok(Some(found)) => found,
            Ok(None) => continue,
            Err(e) => {
                errors.push(json!({"id": id, "error": e}));
                continue;
            }
        };
        let mut entry = json!({"id": id, "installed": old.manifest.version, "available": available, "install": params});
        if apply {
            match apply_update(s, cmd, &params) {
                Ok(true) => updated.push(json!(id)),
                Ok(false) => entry["permissionsChanged"] = json!(true),
                Err(e) => errors.push(json!({"id": id, "error": e.to_string()})),
            }
        }
        updates.push(entry);
    }
    Ok(json!({"updates": updates, "updated": updated, "errors": errors}))
}

/// Install an update unless it asks for other permissions (that one waits for its confirmation,
/// `plugin.install`): whether it was installed.
fn apply_update(s: &mut Session, cmd: &str, params: &Value) -> Result<bool> {
    let c = install::obtain(s, cmd, params)?;
    let (_, info) = install::review(s, cmd, &c)?;
    if info["permissionsChanged"].as_bool().unwrap_or(true) {
        return Ok(false);
    }
    install::install(s, cmd, c).map(|_| true)
}

fn remove(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "plugin.remove";
    let id = str_p(p, "id").ok_or_else(|| bad(cmd, "`id` required (see plugin.list)"))?.to_string();
    if !install::remove(s, cmd, &id)? {
        return Err(bad(cmd, format!("no plug-in `{id}` is installed")));
    }
    Ok(json!({"removed": id}))
}

fn run(s: &mut Session, p: &Value) -> Result<Value> {
    let id = str_p(p, "id").ok_or_else(|| bad("plugin.run", "`id` required (see plugin.list)"))?.to_string();
    let command = str_p(p, "command").map(str::to_string);
    let fuel = u64_p(p, "fuel");
    super::run(s, &id, command.as_deref(), p.get("params").unwrap_or(&Value::Null), fuel)
}

fn catalog_list(s: &mut Session, p: &Value) -> Result<Value> {
    let urls = match str_p(p, "url") {
        Some(url) => vec![url.to_string()],
        None => s.prefs.plugin_data.catalogs.clone(),
    };
    let (mut packages, mut warnings) = (vec![], vec![]);
    let installed = s.plugins.list().to_vec();
    let fetcher = s.plugins.fetcher();
    for url in &urls {
        match fetcher.with(|f| super::catalog_packages_with(f, url, &installed)) {
            Ok((found, w)) => {
                packages.extend(found);
                warnings.extend(w.into_iter().map(|w| format!("{url}: {w}")));
            }
            Err(e) => warnings.push(format!("{url}: {e}")),
        }
    }
    Ok(json!({"catalogs": urls, "packages": packages, "warnings": warnings}))
}

fn catalog_add(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "plugin.catalog.add";
    let url = str_p(p, "url").map(str::trim).filter(|u| !u.is_empty()).ok_or_else(|| bad(cmd, "`url` required"))?.to_string();
    let catalogs = &mut s.prefs.plugin_data.catalogs;
    if bool_p(p, "remove").unwrap_or(false) {
        catalogs.retain(|u| *u != url);
    } else if !catalogs.contains(&url) {
        install::check_catalog_url(&url).map_err(|e| bad(cmd, e))?;
        if catalogs.len() >= MAX_CATALOGS {
            return Err(bad(cmd, format!("at most {MAX_CATALOGS} catalogs")));
        }
        catalogs.push(url);
    }
    let out = json!({"catalogs": s.prefs.plugin_data.catalogs});
    save_prefs(s);
    Ok(out)
}

fn spec(id: &'static str, label: &'static str, params: &'static str, run: fn(&mut Session, &Value) -> Result<Value>, journal: bool) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled: always, run, journal, named: NamedKeys::NONE }
}

/// None of these is an undo step by itself: `plugin.run` records one for the commands it runs.
pub fn specs() -> Vec<CommandSpec> {
    vec![
        spec(
            "plugin.list",
            "Plug-ins",
            "{} → {plugins: [{id, name, version, kind, permissions, commands, source, sha256, stored}], problems, catalogs, stored (kept in the Plug-ins folder; else this session only), network, last}",
            list,
            false,
        ),
        spec(
            "plugin.install",
            "Install Plug-in",
            r#"{"path":str,"review":bool?,"sha256":str?} | {"data":base64 .craftplugin,"name":str?,"review":bool?,"sha256":str?} | {"repo":"owner/repo"|"owner/repo@tag" (its release's .craftplugin, checked against SHA256SUMS),"review":bool?,"sha256":str?} | {"catalog":index.json URL,"id":str,"version":str? (default: the newest for FilmCraft),"review":bool?,"sha256":str?} — review: check and describe it without installing (the confirmation); sha256: install only that bundle → {id, name, version, kind, permissions, commands, source, sha256, inCatalog, replaces, permissionsChanged, stored, installed}"#,
            install_cmd,
            true,
        ),
        spec(
            "plugin.update",
            "Update Plug-ins",
            r#"{"id":str?,"apply":bool? (install the updates that ask for no other permissions)} — checks each plug-in's catalog or repository → {updates: [{id, installed, available, install (plugin.install params), permissionsChanged?}], updated, errors}"#,
            update,
            true,
        ),
        spec("plugin.remove", "Remove Plug-in", r#"{"id":str} — removes it and its settings → {removed}"#, remove, true),
        spec(
            "plugin.run",
            "Run Plug-in",
            r#"{"id":str,"command":str? (default: the first),"params":{}? (the command's parameters),"fuel":u64? (a lower instruction budget)} — runs the commands it calls in their JSON form, as one undo step named after the plug-in, rolled back when it fails → {plugin, command, output, notes, logs, calls, fuel, panel}"#,
            run,
            true,
        ),
        spec(
            "plugin.catalog.list",
            "Plug-in Catalogs",
            r#"{"url":str?} — the packages FilmCraft can run in that catalog, or in every catalog added → {catalogs, packages: [{catalog, id, name, version, author, description, license, repo, kind, sha256, installed}], warnings}"#,
            catalog_list,
            false,
        ),
        spec("plugin.catalog.add", "Add Plug-in Catalog", r#"{"url":https:// on GitHub or file:// index.json,"remove":bool?} → {catalogs}"#, catalog_add, true),
    ]
}
