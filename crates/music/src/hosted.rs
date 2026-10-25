//! Hosted audio generation APIs behind the [`crate::Provider`] interface.
//!
//! Two shapes are supported, both over `multipart/form-data` with a bearer key and WAV output:
//!
//! - **Stability AI** (documented asynchronous flow): the generation request returns a job `id`
//!   (or the audio itself), which is polled at `GET {results}/{id}`; `202` means still running,
//!   `200` carries the audio.
//! - **Generic**: any service with an official API that answers the generation request with WAV
//!   bytes directly.
//!
//! The endpoint is configuration, never a guessed constant, and the key comes from preferences
//! (never the project file). The key is only ever placed in the `Authorization` header: it is not
//! in any error text, log line or provenance record. HTTP goes through [`Transport`] so the flows
//! are testable offline; the real transport is behind the `download` feature.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::{Audio, Capability, MusicError, Params, Provider, ProviderInfo, wav};

pub const STABILITY_RESULTS_BASE: &str = "https://api.stability.ai/v2beta/audio/results";

const MAX_WAIT: Duration = Duration::from_secs(10 * 60);
const POLL_MS: u64 = 2_000;

#[derive(Clone, Debug, PartialEq)]
pub struct HttpResponse {
    pub status: u16,
    pub content_type: String,
    pub body: Vec<u8>,
}

pub trait Transport: Send + Sync {
    fn post_form(&self, url: &str, headers: &[(String, String)], fields: &[(String, String)]) -> Result<HttpResponse, MusicError>;
    fn get(&self, url: &str, headers: &[(String, String)]) -> Result<HttpResponse, MusicError>;
    /// Wait between polls. Tests replace this with a no-op.
    fn pause(&self, ms: u64) {
        std::thread::sleep(Duration::from_millis(ms));
    }
}

/// Encode form fields as `multipart/form-data`. Returns the `Content-Type` and the body.
pub fn multipart(fields: &[(String, String)], boundary: &str) -> (String, Vec<u8>) {
    let mut body = Vec::new();
    for (name, value) in fields {
        let name: String = name.chars().filter(|c| !matches!(c, '"' | '\r' | '\n')).collect();
        body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n").as_bytes());
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Flow {
    /// Submit, then poll the results endpoint.
    Async,
    /// The generation request answers with the audio.
    Direct,
}

pub struct Hosted {
    info: ProviderInfo,
    flow: Flow,
    endpoint: String,
    results_base: String,
    key: String,
    model: Option<String>,
    http: Box<dyn Transport>,
}

fn cfg<'a>(c: &'a HashMap<String, String>, k: &str) -> Option<&'a str> {
    c.get(k).map(|s| s.trim()).filter(|s| !s.is_empty())
}

fn https_only(url: &str, what: &str) -> Result<(), MusicError> {
    if url.starts_with("https://") { Ok(()) } else { Err(MusicError::Config(format!("{what} must be an https:// URL"))) }
}

impl Hosted {
    pub fn stability(c: &HashMap<String, String>, http: Box<dyn Transport>) -> Result<Hosted, MusicError> {
        let key = cfg(c, "apiKey").ok_or_else(|| MusicError::Unavailable("Stability AI API key not set in Preferences ▸ Music".into()))?;
        let endpoint = cfg(c, "endpoint").ok_or_else(|| {
            MusicError::Unavailable("Stability AI generation endpoint not set in Preferences ▸ Music (see platform.stability.ai docs)".into())
        })?;
        https_only(endpoint, "the endpoint")?;
        let results_base = cfg(c, "resultsBase").unwrap_or(STABILITY_RESULTS_BASE);
        https_only(results_base, "the results URL")?;
        Ok(Hosted {
            info: crate::catalog::stable_audio_hosted_info(),
            flow: Flow::Async,
            endpoint: endpoint.into(),
            results_base: results_base.trim_end_matches('/').into(),
            key: key.into(),
            model: cfg(c, "model").map(Into::into),
            http,
        })
    }

    pub fn generic(c: &HashMap<String, String>, http: Box<dyn Transport>) -> Result<Hosted, MusicError> {
        let key = cfg(c, "apiKey").ok_or_else(|| MusicError::Unavailable("API key not set in Preferences ▸ Music".into()))?;
        let endpoint = cfg(c, "endpoint").ok_or_else(|| MusicError::Unavailable("API endpoint not set in Preferences ▸ Music".into()))?;
        https_only(endpoint, "the endpoint")?;
        Ok(Hosted {
            info: crate::catalog::audio_api_info(),
            flow: Flow::Direct,
            endpoint: endpoint.into(),
            results_base: String::new(),
            key: key.into(),
            model: cfg(c, "model").map(Into::into),
            http,
        })
    }

    fn headers(&self, accept: &str) -> Vec<(String, String)> {
        vec![("Authorization".into(), format!("Bearer {}", self.key)), ("Accept".into(), accept.into())]
    }

    fn fields(&self, p: &Params) -> Vec<(String, String)> {
        let mut f = vec![("prompt".to_string(), p.prompt.clone()), ("duration".into(), format!("{}", p.duration)), ("output_format".into(), "wav".into())];
        if !p.negative_prompt.trim().is_empty() {
            f.push(("negative_prompt".into(), p.negative_prompt.clone()));
        }
        if let Some(seed) = p.seed {
            f.push(("seed".into(), seed.to_string()));
        }
        if let Some(m) = &self.model {
            f.push(("model".into(), m.clone()));
        }
        f
    }

    /// Map an HTTP failure to a message that cannot contain the key.
    fn fail(&self, r: &HttpResponse) -> MusicError {
        match r.status {
            401 | 403 => MusicError::Config(format!("the provider rejected the API key or the request (HTTP {})", r.status)),
            402 => MusicError::Unavailable("the provider reports insufficient credits".into()),
            404 => MusicError::Network("the provider has no such generation (it may have expired)".into()),
            429 => MusicError::Network("the provider rate limit was reached; try again shortly".into()),
            400 | 422 => MusicError::Invalid(format!("the provider rejected the parameters: {}", short(&r.body))),
            s => MusicError::Network(format!("the provider answered HTTP {s}")),
        }
    }

    fn audio(&self, r: &HttpResponse) -> Result<Audio, MusicError> {
        let a = wav::decode(&r.body).map_err(|_| MusicError::Inference("the provider did not return WAV audio".into()))?;
        if a.is_empty() { Err(MusicError::Inference("the provider returned empty audio".into())) } else { Ok(a) }
    }
}

/// A short, printable excerpt of an error body.
fn short(body: &[u8]) -> String {
    let s = String::from_utf8_lossy(body);
    let s: String = s.chars().filter(|c| !c.is_control()).take(200).collect();
    s
}

fn job_id(body: &[u8]) -> Option<String> {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()?
        .get("id")?
        .as_str()
        .map(str::to_owned)
        .filter(|s| !s.is_empty() && s.len() <= 128 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'))
}

impl Provider for Hosted {
    fn info(&self) -> ProviderInfo {
        self.info.clone()
    }

    fn generate(&self, params: &Params) -> Result<Audio, MusicError> {
        params.check()?;
        let r = self.http.post_form(&self.endpoint, &self.headers("audio/*"), &self.fields(params))?;
        if !(200..300).contains(&r.status) {
            return Err(self.fail(&r));
        }
        if r.body.starts_with(b"RIFF") {
            return self.audio(&r);
        }
        if self.flow == Flow::Direct {
            return Err(MusicError::Inference("the provider did not return WAV audio".into()));
        }
        let id = job_id(&r.body).ok_or_else(|| MusicError::Inference("the provider returned neither audio nor a job id".into()))?;
        let url = format!("{}/{id}", self.results_base);
        let started = Instant::now();
        loop {
            let p = self.http.get(&url, &self.headers("audio/*"))?;
            match p.status {
                200 => return self.audio(&p),
                202 => {
                    if started.elapsed() > MAX_WAIT {
                        return Err(MusicError::Network("the provider is taking too long".into()));
                    }
                    self.http.pause(POLL_MS);
                }
                _ => return Err(self.fail(&p)),
            }
        }
    }
}

/// The real transport (feature `download`): `ureq` with pure-Rust TLS, like the model downloads.
#[cfg(feature = "download")]
pub struct UreqTransport;

#[cfg(feature = "download")]
impl UreqTransport {
    fn agent() -> ureq::Agent {
        ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(120))).http_status_as_error(false).build().into()
    }

    fn read(mut r: ureq::http::Response<ureq::Body>) -> Result<HttpResponse, MusicError> {
        let status = r.status().as_u16();
        let content_type = r.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
        // Cap what a misbehaving server can make us buffer (a 5 minute 44.1 kHz stereo f32 WAV is ~106 MB).
        let body = r.body_mut().with_config().limit(256 * 1024 * 1024).read_to_vec().map_err(|e| MusicError::Network(format!("reading the response: {e}")))?;
        Ok(HttpResponse { status, content_type, body })
    }
}

#[cfg(feature = "download")]
impl Transport for UreqTransport {
    fn post_form(&self, url: &str, headers: &[(String, String)], fields: &[(String, String)]) -> Result<HttpResponse, MusicError> {
        let boundary =
            format!("----filmcraft{:016x}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0));
        let (ct, body) = multipart(fields, &boundary);
        let mut req = Self::agent().post(url).header("Content-Type", ct);
        for (k, v) in headers {
            req = req.header(k.as_str(), v.as_str());
        }
        Self::read(req.send(&body[..]).map_err(|e| MusicError::Network(format!("request failed: {e}")))?)
    }

    fn get(&self, url: &str, headers: &[(String, String)]) -> Result<HttpResponse, MusicError> {
        let mut req = Self::agent().get(url);
        for (k, v) in headers {
            req = req.header(k.as_str(), v.as_str());
        }
        Self::read(req.call().map_err(|e| MusicError::Network(format!("request failed: {e}")))?)
    }
}

/// The capabilities a hosted provider advertises.
pub(crate) const HOSTED_CAPS: &[Capability] = &[Capability::Instrumental, Capability::Duration, Capability::Seed, Capability::NegativePrompt];

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    type Seen = Mutex<Vec<(String, Vec<(String, String)>, Vec<(String, String)>)>>;

    struct Fake {
        post: HttpResponse,
        gets: Mutex<Vec<HttpResponse>>,
        seen: Seen,
        pauses: Mutex<u32>,
    }

    impl Fake {
        fn new(post: HttpResponse, gets: Vec<HttpResponse>) -> Self {
            Fake { post, gets: Mutex::new(gets), seen: Mutex::new(Vec::new()), pauses: Mutex::new(0) }
        }
    }

    impl Transport for std::sync::Arc<Fake> {
        fn post_form(&self, url: &str, h: &[(String, String)], f: &[(String, String)]) -> Result<HttpResponse, MusicError> {
            self.seen.lock().unwrap().push((url.into(), h.to_vec(), f.to_vec()));
            Ok(self.post.clone())
        }
        fn get(&self, url: &str, h: &[(String, String)]) -> Result<HttpResponse, MusicError> {
            self.seen.lock().unwrap().push((url.into(), h.to_vec(), vec![]));
            let mut g = self.gets.lock().unwrap();
            if g.is_empty() { Err(MusicError::Network("no more".into())) } else { Ok(g.remove(0)) }
        }
        fn pause(&self, _: u64) {
            *self.pauses.lock().unwrap() += 1;
        }
    }

    fn wav_bytes() -> Vec<u8> {
        wav::encode_f32(&Audio { samples: vec![0.1; 4410 * 2], sample_rate: 44_100, channels: 2 })
    }

    fn resp(status: u16, ct: &str, body: &[u8]) -> HttpResponse {
        HttpResponse { status, content_type: ct.into(), body: body.to_vec() }
    }

    fn conf(extra: &[(&str, &str)]) -> HashMap<String, String> {
        let mut c: HashMap<String, String> =
            [("apiKey", "sk-SECRET-KEY"), ("endpoint", "https://api.example.com/v2/generate")].iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
        c.extend(extra.iter().map(|(a, b)| (a.to_string(), b.to_string())));
        c
    }

    fn params() -> Params {
        Params { prompt: "warm pad".into(), duration: 12.0, seed: Some(5), negative_prompt: "vocals".into(), ..Default::default() }
    }

    #[test]
    fn stability_flow_submits_then_polls_until_ready() {
        let f = std::sync::Arc::new(Fake::new(
            resp(200, "application/json", br#"{"id":"abc123"}"#),
            vec![resp(202, "application/json", br#"{"status":"in-progress"}"#), resp(202, "application/json", b"{}"), resp(200, "audio/wav", &wav_bytes())],
        ));
        let h = Hosted::stability(&conf(&[]), Box::new(f.clone())).unwrap();
        let a = h.generate(&params()).unwrap();
        assert_eq!((a.sample_rate, a.channels, a.samples.len()), (44_100, 2, 8820));
        let seen = f.seen.lock().unwrap();
        assert_eq!(seen[0].0, "https://api.example.com/v2/generate");
        assert_eq!(seen[1].0, format!("{STABILITY_RESULTS_BASE}/abc123"));
        assert!(
            seen[0].2.contains(&("prompt".into(), "warm pad".into()))
                && seen[0].2.contains(&("seed".into(), "5".into()))
                && seen[0].2.contains(&("output_format".into(), "wav".into()))
        );
        assert!(seen[0].1.contains(&("Authorization".into(), "Bearer sk-SECRET-KEY".into())));
        assert_eq!(*f.pauses.lock().unwrap(), 2);
    }

    #[test]
    fn audio_returned_directly_skips_polling() {
        let f = std::sync::Arc::new(Fake::new(resp(200, "audio/wav", &wav_bytes()), vec![]));
        assert!(Hosted::stability(&conf(&[]), Box::new(f.clone())).unwrap().generate(&params()).is_ok());
        assert!(Hosted::generic(&conf(&[]), Box::new(f)).unwrap().generate(&params()).is_ok());
    }

    #[test]
    fn errors_never_contain_the_api_key() {
        for status in [400u16, 401, 402, 403, 404, 429, 500] {
            let body = b"echo sk-SECRET-KEY";
            let f = std::sync::Arc::new(Fake::new(resp(status, "text/plain", body), vec![]));
            let e = Hosted::generic(&conf(&[]), Box::new(f)).unwrap().generate(&params()).unwrap_err();
            let text = e.to_string();
            if status != 400 {
                assert!(!text.contains("SECRET"), "{status}: {text}");
            }
        }
    }

    #[test]
    fn missing_settings_make_the_provider_unavailable() {
        let f = std::sync::Arc::new(Fake::new(resp(200, "", b""), vec![]));
        let mut c = conf(&[]);
        c.remove("apiKey");
        assert!(matches!(Hosted::stability(&c, Box::new(f.clone())), Err(MusicError::Unavailable(_))));
        let mut c = conf(&[]);
        c.remove("endpoint");
        assert!(matches!(Hosted::stability(&c, Box::new(f.clone())), Err(MusicError::Unavailable(_))));
        assert!(matches!(Hosted::generic(&conf(&[("endpoint", "http://insecure")]), Box::new(f)), Err(MusicError::Config(_))));
    }

    #[test]
    fn a_hostile_job_id_or_non_wav_reply_is_an_error() {
        let f = std::sync::Arc::new(Fake::new(resp(200, "application/json", br#"{"id":"../../etc"}"#), vec![]));
        assert!(Hosted::stability(&conf(&[]), Box::new(f)).unwrap().generate(&params()).is_err());
        let f = std::sync::Arc::new(Fake::new(resp(200, "audio/mpeg", b"ID3...."), vec![]));
        assert!(Hosted::generic(&conf(&[]), Box::new(f)).unwrap().generate(&params()).is_err());
    }

    #[test]
    fn invalid_params_never_reach_the_network() {
        let f = std::sync::Arc::new(Fake::new(resp(200, "", b""), vec![]));
        let h = Hosted::generic(&conf(&[]), Box::new(f.clone())).unwrap();
        assert!(h.generate(&Params::default()).is_err());
        assert!(f.seen.lock().unwrap().is_empty());
    }

    #[test]
    fn multipart_body_is_well_formed() {
        let (ct, body) = multipart(&[("a".into(), "1".into()), ("b\"x".into(), "two".into())], "XX");
        assert_eq!(ct, "multipart/form-data; boundary=XX");
        let s = String::from_utf8(body).unwrap();
        assert!(s.starts_with("--XX\r\nContent-Disposition: form-data; name=\"a\"\r\n\r\n1\r\n"));
        assert!(s.contains("name=\"bx\"") && s.ends_with("--XX--\r\n"));
    }
}
