//! Test helpers: a transport that answers from a script and records what it was sent.

use crate::http::{Method, Request, Response, Transport, TransportError};
use std::cell::RefCell;
use std::collections::VecDeque;

/// One request as the server saw it.
#[derive(Debug, Clone)]
pub(crate) struct Seen {
    pub method: Method,
    pub url: String,
    pub content_type: Option<String>,
    pub content_range: Option<String>,
    pub authorization: Option<String>,
    pub body: Vec<u8>,
}

/// Replies in order; every request is recorded.
#[derive(Default)]
pub(crate) struct Script {
    replies: RefCell<VecDeque<Result<Response, TransportError>>>,
    seen: RefCell<Vec<Seen>>,
    /// Watched requests report half their body as sent before the reply.
    halfway: bool,
}

impl Script {
    pub(crate) fn new(replies: Vec<Result<Response, TransportError>>) -> Self {
        Self { replies: RefCell::new(replies.into()), seen: RefCell::default(), halfway: false }
    }

    pub(crate) fn reporting_halfway(mut self) -> Self {
        self.halfway = true;
        self
    }

    pub(crate) fn seen(&self) -> Vec<Seen> {
        self.seen.borrow().clone()
    }

    pub(crate) fn ranges(&self) -> Vec<String> {
        self.seen().into_iter().map(|s| s.content_range.unwrap_or_default()).collect()
    }
}

impl Transport for Script {
    fn send(&self, request: &Request<'_>) -> Result<Response, TransportError> {
        self.record(request);
        self.replies.borrow_mut().pop_front().unwrap_or_else(|| panic!("no reply scripted for request {}", self.seen.borrow().len()))
    }

    fn send_watched(&self, request: &Request<'_>, watch: &mut dyn FnMut(u64) -> bool) -> Result<Response, TransportError> {
        let half = request.body.len() as u64 / 2;
        if self.halfway && half > 0 && !watch(half) {
            self.record(request);
            return Err(TransportError("stopped".into()));
        }
        self.send(request)
    }
}

impl Script {
    fn record(&self, request: &Request<'_>) {
        self.seen.borrow_mut().push(Seen {
            method: request.method,
            url: request.url.into(),
            content_type: request.header("content-type").map(str::to_string),
            content_range: request.header("content-range").map(str::to_string),
            authorization: request.header("authorization").map(str::to_string),
            body: request.body.to_vec(),
        });
    }
}

pub(crate) fn reply(status: u16, headers: &[(&str, &str)], body: &str) -> Result<Response, TransportError> {
    Ok(Response { status, headers: headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(), body: body.as_bytes().to_vec() })
}
