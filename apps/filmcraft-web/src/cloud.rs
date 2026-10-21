//! CollabCraft from the browser build.
//!
//! Same jobs and events as the desktop worker ([`filmcraft::cloud`]), but the
//! transport is the page's own `fetch` and the session rides the cookie the
//! CollabCraft service set at sign-in.  There is no device flow, no polling
//! thread, and no token file.
//!
//! A `<meta name="craft-cloud-url">` on the page overrides the origin when
//! FilmCraft is served from somewhere other than CollabCraft itself.

use craft_cloud_client::{Client, Error};
use wasm_bindgen::JsCast as _;
use wasm_bindgen_futures::spawn_local;

use filmcraft_ui_egui::cloud::{Account, CloudBackend, Event, Fault, Job, Post};

#[path = "cloud_paths.rs"]
mod cloud_paths;
use cloud_paths::LOGOUT_PATH;

/// Hands cloud jobs to async fetch tasks.  Cheap to clone because `Client`
/// carries only a `String` base URL and an optional token string.
#[derive(Clone)]
pub struct Service {
    client: Client,
    ctx: egui::Context,
}

impl Service {
    /// Build from whatever the page says the service is.
    ///
    /// If the page carries `<meta name="craft-cloud-url">`, that URL is used;
    /// otherwise requests go to the origin that served the page — the ordinary
    /// case when CollabCraft hosts the FilmCraft build itself.
    pub fn new(ctx: egui::Context) -> Service {
        let client = Client::from_page();
        log::info!("filmcraft-web: cloud storage through {}", client.base());
        Service { client, ctx }
    }
}

impl CloudBackend for Service {
    fn submit(&self, job: Job, post: Post) {
        let worker = self.clone();
        spawn_local(async move {
            worker.run(job, post).await;
        });
    }
}

impl Service {
    async fn run(self, job: Job, post: Post) {
        let emit = |event: Event| {
            post.send(event);
            self.ctx.request_repaint();
        };
        match job {
            Job::Account => {
                let answer = self
                    .client
                    .me()
                    .await
                    .map(|me| Account { display: me.display, capabilities: me.capabilities, provider_linked: me.provider_linked })
                    .map_err(fault);
                emit(Event::Account(answer));
            }
            Job::LinkStart => match self.client.link_start().await.map_err(fault) {
                Ok(start) => emit(Event::Browser { url: start.login_url, handle: Some(start.handle) }),
                Err(e) => emit(Event::Linked(Err(e))),
            },
            Job::LinkPoll(handle) => {
                let answer = self.client.link_poll(&handle).await.map_err(fault);
                emit(Event::Linked(answer));
            }
            Job::SignOut => {
                if post_logout().await.is_ok() {
                    if let Some(w) = web_sys::window() {
                        let _ = w.location().reload();
                    }
                }
            }
            Job::List(path) => {
                let entries = self.client.list(&path).await.map_err(fault);
                emit(Event::Listed { path, entries });
            }
            Job::SharedWithMe => {
                let answer = self.client.shared_with_me().await.map_err(fault);
                emit(Event::Shared(answer));
            }
            Job::Read { entry, into } => {
                let id = entry.id.clone();
                let bytes = self.client.read(&id).await.map_err(fault);
                emit(Event::Read { entry, into, bytes });
            }
            Job::Write { uid, entry, bytes } => {
                let id = entry.id.clone();
                let etag = entry.etag.clone();
                let answer = self.client.write(&id, &bytes, etag.as_deref()).await.map_err(fault);
                emit(Event::Saved { uid, entry: answer });
            }
            Job::Create { uid, folder, name, bytes } => {
                let answer = self.client.create(&folder, &name, &bytes).await.map_err(fault);
                emit(Event::Saved { uid, entry: answer });
            }
            Job::Conversation(id) => {
                let answer = self.client.conversation(&id).await.map_err(fault);
                emit(Event::Conversation(answer));
            }
        }
    }
}

fn fault(e: Error) -> Fault {
    match e {
        Error::Unauthenticated => Fault::Unauthenticated,
        Error::NotLinked => Fault::NotLinked,
        Error::Unreachable(detail) => Fault::Unreachable(detail),
        Error::Refused { code, .. } if matches!(code.as_str(), "conflict" | "409" | "412") => Fault::Conflict,
        Error::Refused { code, detail } => Fault::Refused(if detail.trim().is_empty() { code } else { detail }),
        Error::Malformed(detail) => Fault::Refused(format!("the cloud service gave an answer FilmCraft could not read: {detail}")),
        Error::Unsendable(detail) => {
            log::error!("cloud request could not be encoded: {detail}");
            Fault::Refused("FilmCraft could not build that request".to_owned())
        }
    }
}

/// POST to the service's logout endpoint and let the browser clear the cookie.
async fn post_logout() -> Result<(), String> {
    let global = js_sys::global();
    let fetch =
        js_sys::Reflect::get(&global, &wasm_bindgen::JsValue::from_str("fetch")).ok().and_then(|v| v.dyn_into::<js_sys::Function>().ok()).ok_or("no fetch")?;
    let init = web_sys::RequestInit::new();
    init.set_method("POST");
    init.set_credentials(web_sys::RequestCredentials::Include);
    let req = web_sys::Request::new_with_str_and_init(LOGOUT_PATH, &init).map_err(|e| format!("{e:?}"))?;
    let promised = fetch.call1(global.as_ref(), req.as_ref()).map_err(|e| format!("{e:?}"))?;
    let promised: js_sys::Promise = promised.dyn_into().map_err(|_| "not a promise".to_owned())?;
    let value = wasm_bindgen_futures::JsFuture::from(promised).await.map_err(|e| format!("{e:?}"))?;
    let response: web_sys::Response = value.dyn_into().map_err(|_| "logout response was not a Response".to_owned())?;
    if !response.ok() {
        return Err(format!("logout failed with HTTP {}", response.status()));
    }
    Ok(())
}
