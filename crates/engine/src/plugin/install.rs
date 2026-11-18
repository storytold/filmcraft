//! Installing plug-ins: from a file, base64 data, a GitHub repository's release or a catalog's
//! package, always reviewed before anything is written. The network parts take a [`Fetch`] and no
//! session, so the UI can run them on a worker thread.

use std::sync::Arc;

use craft_plugin::fetch::{self, Fetch};
use craft_plugin::{Manifest, Source, bundle, catalog, github};
use serde_json::{Value, json};

use super::APP;
use super::runtime::InstalledPlugin;
use crate::commands::bad;
use crate::{Result, Session};

/// A bundle obtained for installing: its bytes, where it came from and, for a repository, whether
/// a catalog in use lists it.
#[derive(Clone, Debug)]
pub struct Candidate {
    pub bytes: Vec<u8>,
    pub source: Source,
    /// `Some(true)` from a catalog; for a repository, whether a catalog in use lists it.
    pub in_catalog: Option<bool>,
}

/// A bundle checked and shown for review (`plugin.install {review: true}`), kept so the install
/// that follows doesn't download it again.
pub(crate) struct Pending {
    pub sha256: String,
    /// The `plugin.install` parameters that named it.
    pub request: Value,
    pub candidate: Candidate,
}

/// A non-empty text parameter.
fn text<'a>(p: &'a Value, k: &str) -> Option<&'a str> {
    p.get(k).and_then(Value::as_str).map(str::trim).filter(|t| !t.is_empty())
}

/// Download the bundle `request` names from the network: `repo` (`owner/repo[@tag]`) or
/// `catalog` with `id` (and optionally `version`). `catalogs` are the ones in use, to say whether
/// a repository is listed in one. Blocks until the downloads finish: the UI calls it on a worker
/// thread.
pub fn download(f: &mut dyn Fetch, catalogs: &[String], request: &Value) -> std::result::Result<Candidate, String> {
    if let Some(spec) = text(request, "repo") {
        let repo = github::parse_repo_spec(spec)?;
        let plan = fetch::resolve_repo(f, &repo)?;
        let bytes = fetch::download_repo(f, &plan)?;
        let source = Source::Github { repo: plan.repo.slug(), tag: plan.repo.tag.clone().unwrap_or_default(), asset: plan.asset.clone() };
        let in_catalog = catalogs.iter().any(|url| {
            fetch::fetch_catalog(f, url)
                .is_ok_and(|(cat, _)| cat.packages.iter().any(|pkg| pkg.repo_ref().is_some_and(|r| r.same_repo(&repo.owner, &repo.name))))
        });
        return Ok(Candidate { bytes, source, in_catalog: Some(in_catalog) });
    }
    if let Some(url) = text(request, "catalog") {
        let id = text(request, "id").ok_or("`id` required with `catalog` (a package of the catalog)")?;
        let (cat, _) = fetch::fetch_catalog(f, url)?;
        let package = cat.package(id).ok_or_else(|| format!("{url} has no package `{id}`"))?;
        let release = match text(request, "version") {
            Some(v) => package.release(v).ok_or_else(|| format!("{id} has no version {v} in {url}"))?,
            None => package.latest_for(&APP).ok_or_else(|| format!("{id} has no release for FilmCraft {}", APP.version))?,
        };
        release.target(&package.kind, &APP)?;
        let bytes = fetch::download_release(f, url, package, release)?;
        let (_, manifest) = bundle::open(&bytes)?;
        package.check_manifest(release, &manifest)?;
        return Ok(Candidate { bytes, source: Source::Catalog { catalog: url.into(), package: id.into() }, in_catalog: Some(true) });
    }
    Err("pass `path`, `data` (base64), `repo` (owner/repo or owner/repo@tag) or `catalog` with `id`".into())
}

/// Read the bundle `p` names: `path`, `data` (base64), or what [`download`] takes.
pub(crate) fn obtain(s: &Session, cmd: &str, p: &Value) -> Result<Candidate> {
    if let Some(path) = text(p, "path") {
        let bytes = s.services.read_file_bounded(path, bundle::MAX_BUNDLE_BYTES as u64).map_err(|e| bad(cmd, format!("{path}: {e}")))?;
        return Ok(Candidate { bytes, source: Source::File { path: path.into() }, in_catalog: None });
    }
    if let Some(data) = text(p, "data") {
        if data.len() > bundle::MAX_BUNDLE_BYTES / 3 * 4 + 4 {
            return Err(bad(cmd, format!("the bundle is larger than {} MB", bundle::MAX_BUNDLE_BYTES >> 20)));
        }
        let bytes = filmcraft_project::gtemplate::base64_decode(data).ok_or_else(|| bad(cmd, "`data` isn't base64"))?;
        let name = text(p, "name").unwrap_or("data");
        return Ok(Candidate { bytes, source: Source::File { path: name.into() }, in_catalog: None });
    }
    let catalogs = s.prefs.plugin_data.catalogs.clone();
    s.plugins.fetcher().with(|f| download(f, &catalogs, p)).map_err(|e| bad(cmd, e))
}

/// What an install confirmation shows about `c`: the manifest's permissions, kind and commands,
/// the source, the SHA-256, whether a catalog lists it and what it replaces. An error when the
/// bundle isn't a plug-in FilmCraft can run.
pub(crate) fn review(s: &Session, cmd: &str, c: &Candidate) -> Result<(Manifest, Value)> {
    let (_, manifest) = bundle::open(&c.bytes).map_err(|e| bad(cmd, e))?;
    manifest.target(&APP).map_err(|e| bad(cmd, format!("{} can't be installed: {e}", manifest.name)))?;
    c.source.check().map_err(|e| bad(cmd, e))?;
    let old = s.plugins.get(&manifest.id);
    let mut v = serde_json::to_value(&manifest).unwrap_or_else(|_| json!({"id": manifest.id}));
    v["kind"] = json!(manifest.kind);
    v["sha256"] = json!(bundle::sha256_hex(&c.bytes));
    v["source"] = serde_json::to_value(&c.source).unwrap_or(Value::Null);
    v["inCatalog"] = json!(c.in_catalog);
    v["replaces"] = json!(old.map(|o| &o.manifest.version));
    v["permissionsChanged"] = json!(old.is_some_and(|o| craft_plugin::permissions::changed(&o.manifest, &manifest)));
    v["stored"] = json!(s.plugins.has_store());
    Ok((manifest, v))
}

/// Install `c` (already reviewed): into the user's folder when the session has one, else for the
/// session. Replaces an installed plug-in with the same id; its settings stay.
pub(crate) fn install(s: &mut Session, cmd: &str, c: Candidate) -> Result<InstalledPlugin> {
    let (manifest, _) = review(s, cmd, &c)?;
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(store) = &s.plugins.store {
        let (i, _) = store.install_for(&APP, &c.bytes, c.source).map_err(|e| bad(cmd, e))?;
        let plugin = InstalledPlugin { manifest: i.manifest, sha256: i.record.sha256, source: i.record.source, stored: true, wasm: Arc::new(i.bundle.wasm) };
        return Ok(s.plugins.keep(plugin));
    }
    let sha256 = bundle::sha256_hex(&c.bytes);
    let (b, _) = bundle::open(&c.bytes).map_err(|e| bad(cmd, e))?;
    Ok(s.plugins.keep(InstalledPlugin { manifest, sha256, source: c.source, stored: false, wasm: Arc::new(b.wasm) }))
}

/// Keep `c`, named by the install parameters `request`, for the install that follows its review
/// (one at a time).
pub(crate) fn hold(s: &mut Session, sha256: &str, request: Value, c: Candidate) {
    s.plugins.pending = Some(Pending { sha256: sha256.into(), request, candidate: c });
}

/// The bundle reviewed last, when it is `sha256` and `request` named it.
pub(crate) fn take_held(s: &mut Session, sha256: &str, request: &Value) -> Option<Candidate> {
    let held = s.plugins.pending.take_if(|h| h.sha256.eq_ignore_ascii_case(sha256) && h.request == *request)?;
    Some(held.candidate)
}

/// Review a bundle a worker downloaded ([`download`]) for `request` and keep it for the install
/// that follows (`plugin.install` with `request` and the `sha256` this returns): the confirmation's
/// content, as `plugin.install {review: true}` gives it.
pub fn offer(s: &mut Session, request: &Value, c: Candidate) -> Result<Value> {
    let (_, mut info) = review(s, "plugin.install", &c)?;
    let sha256 = info["sha256"].as_str().unwrap_or_default().to_string();
    hold(s, &sha256, request_key(request), c);
    info["installed"] = json!(false);
    Ok(info)
}

/// `plugin.install` parameters without the review flags, which name the bundle the same way.
pub(crate) fn request_key(p: &Value) -> Value {
    let mut request = p.as_object().cloned().unwrap_or_default();
    request.remove("review");
    request.remove("sha256");
    Value::Object(request)
}

/// Remove plug-in `id` and its settings; whether it was installed.
pub(crate) fn remove(s: &mut Session, cmd: &str, id: &str) -> Result<bool> {
    let Some(p) = s.plugins.get(id).cloned() else { return Ok(false) };
    #[cfg(not(target_arch = "wasm32"))]
    if p.stored
        && let Some(store) = &s.plugins.store
    {
        store.remove(&p.manifest.id).map_err(|e| bad(cmd, e))?;
    }
    #[cfg(target_arch = "wasm32")]
    let _ = cmd;
    s.plugins.forget(&p.manifest.id);
    let before = s.prefs.plugin_data.settings.len();
    s.prefs.plugin_data.settings.retain(|k, _| !k.eq_ignore_ascii_case(&p.manifest.id));
    if s.prefs.plugin_data.settings.len() != before {
        super::save_prefs(s);
    }
    Ok(true)
}

/// The catalog `url`'s packages FilmCraft can run, with their newest release for it and the
/// version of each that is `installed`.
pub fn catalog_packages_with(f: &mut dyn Fetch, url: &str, installed: &[InstalledPlugin]) -> std::result::Result<(Vec<Value>, Vec<String>), String> {
    let (cat, warnings) = fetch::fetch_catalog(f, url)?;
    let packages = cat
        .packages
        .iter()
        .filter_map(|pkg| {
            let release = pkg.latest_for(&APP)?;
            let installed = installed.iter().find(|p| p.manifest.id.eq_ignore_ascii_case(&pkg.id)).map(|p| p.manifest.version.clone());
            Some(json!({
                "catalog": url, "id": pkg.id, "name": pkg.name, "version": release.version, "author": pkg.author,
                "description": pkg.description, "license": pkg.license, "repo": pkg.repo, "kind": pkg.kind,
                "sha256": release.sha256, "installed": installed,
            }))
        })
        .collect();
    Ok((packages, warnings))
}

/// Whether `url` is a catalog address FilmCraft reads (`https://` on GitHub's hosts, or
/// `file://`), or why not.
pub(crate) fn check_catalog_url(url: &str) -> std::result::Result<(), String> {
    catalog::check_url(url, None).map(|_| ())
}

/// Where plug-in `p` has a newer release: the install parameters that get it and its version (a
/// tag, for a repository). `None` when it is up to date or came from a file.
pub fn update_for_with(f: &mut dyn Fetch, p: &InstalledPlugin) -> std::result::Result<Option<(Value, String)>, String> {
    match &p.source {
        Source::Catalog { catalog: url, package: id } => {
            let (cat, _) = fetch::fetch_catalog(f, url)?;
            let release = cat.package(id).and_then(|pkg| pkg.latest_for(&APP));
            Ok(release
                .filter(|r| bundle::newer(&r.version, &p.manifest.version))
                .map(|r| (json!({"catalog": url, "id": id, "version": r.version}), r.version.clone())))
        }
        Source::Github { repo, tag: installed, .. } => {
            let mut repo = github::parse_repo_spec(repo)?;
            repo.tag = None;
            let plan = fetch::resolve_repo(f, &repo)?;
            let tag = plan.repo.tag.clone().unwrap_or_default();
            Ok((tag != *installed).then(|| (json!({"repo": format!("{}@{tag}", plan.repo.slug())}), tag)))
        }
        Source::File { .. } | Source::Other { .. } => Ok(None),
    }
}
