//! The provider core (L1): HTTP request messages in; responses, request records
//! and rule decisions out.
//!
//! This is jig's business logic with the I/O taken out. It is a pure
//! [`Core`] step function: no sockets, clock, locks or closures. It owns the
//! script's [`Plan`]; when the plan is [`Plan::External`] it asks the embedder
//! with [`HostReq::Decide`] and renders the [`Comp::Decision`] that comes back,
//! because the rule closure lives with the embedder, not here. See
//! `docs/explanation/sans-io-shell.md` §4.10 and §5.6.
//!
//! Every response is byte-identical to what the async server at `ca1edfd`
//! wrote: the planner writes the status line and headers verbatim, so the
//! header lists below are exactly that server's lines, in its order.
//!
//! [`serve_request`] drives one request through the core in process, for
//! callers that want jig's answers without any I/O (temper's model-level tests,
//! §8).

use std::collections::BTreeMap;
use std::time::Duration;

use jig_core::render::frames_to_body;
use jig_core::request::{parse_anthropic, parse_codex, parse_openai};
use jig_core::{
    Dialect, HttpError, Next, Plan, RecordedRequest, Reply, RequestView, Rule, ScriptAction,
    render_anthropic, render_codex, render_openai,
};
use steploop::run::Core;
use steploop::time::Time;

pub use steploop::http1::{Body, ReqId, Request, Response};

/// Inputs to the provider core.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Comp {
    /// A complete request from the server planner.
    Request { req: ReqId, request: Request },
    /// The embedder's answer to [`HostReq::Decide`] for `req`.
    Decision { req: ReqId, action: ScriptAction },
    /// The embedder wants the server to stop.
    Stop,
}

/// Requests to the I/O step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IoReq {
    /// Write `response` for `req`, then close its connection.
    Respond { req: ReqId, response: Response },
    /// Stop accepting, and give responses in flight `grace` to finish.
    Shutdown { grace: Duration },
}

/// Requests to the embedder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostReq {
    /// Append to the request log. Emitted before the response is requested, so
    /// the log reflects what the client sent however the response goes.
    Record(RecordedRequest),
    /// Decide the action for `req` with the rule closure, and answer with
    /// [`Comp::Decision`].
    Decide { req: ReqId, view: RequestView },
}

/// Provider settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderConfig {
    /// How long responses being written may take to finish after `Stop`.
    pub grace: Duration,
}

impl Default for ProviderConfig {
    fn default() -> Self {
        ProviderConfig {
            grace: Duration::from_secs(1),
        }
    }
}

/// The pure provider core. See the module docs.
#[derive(Debug, Clone)]
pub struct Provider {
    plan: Plan,
    config: ProviderConfig,
    /// Requests waiting for a [`Comp::Decision`], with the dialect of the
    /// route they arrived on (it picks the renderer). A `BTreeMap` so any
    /// iteration is deterministic (house rule 10).
    undecided: BTreeMap<ReqId, Dialect>,
    /// `Shutdown` has been issued.
    stopping: bool,
}

impl Provider {
    /// A provider serving `plan` with the default configuration.
    pub fn new(plan: Plan) -> Self {
        Provider::with_config(plan, ProviderConfig::default())
    }

    /// A provider serving `plan` with `config`.
    pub fn with_config(plan: Plan, config: ProviderConfig) -> Self {
        Provider {
            plan,
            config,
            undecided: BTreeMap::new(),
            stopping: false,
        }
    }
}

impl Core for Provider {
    type Comp = Comp;
    type IoReq = IoReq;
    type HostReq = HostReq;

    fn step(
        &mut self,
        _now: Time,
        comps: &mut Vec<Comp>,
        io: &mut Vec<IoReq>,
        host: &mut Vec<HostReq>,
    ) {
        for comp in comps.drain(..) {
            match comp {
                Comp::Request { req, request } => {
                    on_request(&mut self.plan, &mut self.undecided, req, request, io, host);
                }
                // An unknown or repeated decision misses the lookup and is
                // ignored (house rule 6).
                Comp::Decision { req, action } => {
                    if let Some(dialect) = self.undecided.remove(&req) {
                        io.push(IoReq::Respond {
                            req,
                            response: render_action(dialect, action),
                        });
                    }
                }
                // Requests that still arrive are served; which of them get
                // written is the I/O step's call under the grace period.
                Comp::Stop => {
                    if !self.stopping {
                        self.stopping = true;
                        io.push(IoReq::Shutdown {
                            grace: self.config.grace,
                        });
                    }
                }
            }
        }
    }

    /// The core keeps no timers: the grace period is the I/O step's.
    fn deadline(&self) -> Option<Time> {
        None
    }

    fn done(&self) -> bool {
        self.stopping && self.undecided.is_empty()
    }
}

/// Record the request, then answer it from the plan or ask the host.
fn on_request(
    plan: &mut Plan,
    undecided: &mut BTreeMap<ReqId, Dialect>,
    req: ReqId,
    request: Request,
    io: &mut Vec<IoReq>,
    host: &mut Vec<HostReq>,
) {
    let path = request.path().to_string();
    // Pairing the dialect with its view means a dialect route always has a
    // view: the projections never fail, they return a sparse view instead.
    let routed = Dialect::for_path(&path).map(|dialect| (dialect, project(dialect, &request.body)));

    host.push(HostReq::Record(RecordedRequest {
        path,
        method: request.method,
        body: request.body,
        view: routed.as_ref().map(|(_, view)| view.clone()),
    }));

    let Some((dialect, view)) = routed else {
        io.push(IoReq::Respond {
            req,
            response: not_found(),
        });
        return;
    };
    match plan.next(&view) {
        Next::Action(action) => io.push(IoReq::Respond {
            req,
            response: render_action(dialect, action),
        }),
        Next::External => {
            undecided.insert(req, dialect);
            host.push(HostReq::Decide { req, view });
        }
    }
}

/// Serve one request in process, answering any decision with `rule`.
///
/// Returns the response and the request's record. Nothing touches a socket, so
/// this is how a test drives jig at the HTTP-message level (design §8). The
/// request id is chosen so it cannot collide with a decision `provider` is
/// still waiting for.
///
/// # Panics
///
/// If the provider's plan is [`Plan::External`] and `rule` is `None`: nothing
/// can decide the action.
pub fn serve_request(
    provider: &mut Provider,
    mut rule: Option<&mut Rule>,
    request: Request,
) -> (Response, RecordedRequest) {
    let req = provider
        .undecided
        .keys()
        .next_back()
        .map_or(ReqId(0), |last| ReqId(last.0 + 1));
    let mut comps = vec![Comp::Request { req, request }];
    let (mut io, mut host) = (Vec::new(), Vec::new());
    let (mut response, mut record) = (None, None);

    // At most two rounds: the request, then the rule's decision.
    while !comps.is_empty() {
        provider.step(Time::ZERO, &mut comps, &mut io, &mut host);
        for request in host.drain(..) {
            match request {
                HostReq::Record(recorded) => record = Some(recorded),
                HostReq::Decide { req, view } => {
                    let rule = rule
                        .as_deref_mut()
                        .expect("serve_request: the plan is External but no rule was given");
                    comps.push(Comp::Decision {
                        req,
                        action: rule.decide(&view),
                    });
                }
            }
        }
        for request in io.drain(..) {
            if let IoReq::Respond { response: r, .. } = request {
                response = Some(r);
            }
        }
    }

    (
        response.expect("the provider answers every request it decides"),
        record.expect("the provider records every request"),
    )
}

fn project(dialect: Dialect, body: &[u8]) -> RequestView {
    match dialect {
        Dialect::OpenAi => parse_openai(body),
        Dialect::Anthropic => parse_anthropic(body),
        Dialect::Codex => parse_codex(body),
    }
}

fn render_action(dialect: Dialect, action: ScriptAction) -> Response {
    match action {
        ScriptAction::Reply(reply) => sse_response(render_reply(dialect, &reply)),
        ScriptAction::HttpError(error) => http_error_response(dialect, error),
        // Extension points without stream renderers yet: fail loudly as a
        // provider-shaped error rather than pretend the model completed.
        ScriptAction::StreamError(_) | ScriptAction::AbortStream(_) => http_error_response(
            dialect,
            HttpError::provider(
                501,
                "unsupported_script_action",
                "script action is not implemented by jig-server yet",
            ),
        ),
    }
}

fn render_reply(dialect: Dialect, reply: &Reply) -> String {
    let frames = match dialect {
        Dialect::OpenAi => render_openai(reply),
        Dialect::Anthropic => render_anthropic(reply),
        Dialect::Codex => render_codex(reply),
    };
    frames_to_body(&frames)
}

/// A `200` SSE response with the whole body as one chunk. `Connection: close`
/// lets the client treat EOF as the end of the stream.
fn sse_response(body: String) -> Response {
    Response {
        status: 200,
        reason: "OK".to_string(),
        headers: header_list(&[
            ("Content-Type", "text/event-stream"),
            ("Cache-Control", "no-cache"),
            ("Transfer-Encoding", "chunked"),
            ("Connection", "close"),
        ]),
        body: Body::Chunked(vec![body.into_bytes()]),
    }
}

/// A non-2xx response with a `Content-Length` body. The error's own
/// `Content-Type` (first match, any case) overrides the rendered one; its
/// framing headers are dropped because the core owns framing; every other
/// header follows in the error's order.
fn http_error_response(route_dialect: Dialect, error: HttpError) -> Response {
    let rendered = error.render_body(route_dialect);
    let content_type = error
        .headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
        .map_or(rendered.content_type, |(_, value)| value.clone());
    let body = rendered.body.into_bytes();

    let mut headers = vec![
        ("Content-Type".to_string(), content_type),
        ("Content-Length".to_string(), body.len().to_string()),
        ("Connection".to_string(), "close".to_string()),
    ];
    headers.extend(error.headers.into_iter().filter(|(name, _)| {
        !(name.eq_ignore_ascii_case("content-type")
            || name.eq_ignore_ascii_case("content-length")
            || name.eq_ignore_ascii_case("connection"))
    }));

    Response {
        status: error.status,
        reason: reason_phrase(error.status).to_string(),
        headers,
        body: Body::Full(body),
    }
}

/// A bare `404` for unknown paths.
fn not_found() -> Response {
    Response {
        status: 404,
        reason: "Not Found".to_string(),
        headers: header_list(&[("Content-Length", "0"), ("Connection", "close")]),
        body: Body::Empty,
    }
}

fn header_list(headers: &[(&str, &str)]) -> Vec<(String, String)> {
    headers
        .iter()
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect()
}

fn reason_phrase(status: u16) -> &'static str {
    match status {
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        408 => "Request Timeout",
        409 => "Conflict",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "Error",
    }
}

#[cfg(test)]
mod tests;
