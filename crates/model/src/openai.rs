//! An OpenAI-compatible chat-completions provider (Part 8 §140).
//!
//! One implementation, many endpoints. The chat-completions shape is the wire
//! format everything speaks — OpenAI, OpenRouter, Together, Groq, vLLM, LM
//! Studio, llama.cpp's server, Ollama's OpenAI shim — so this is the first
//! remote provider not because OpenAI matters most but because it is the
//! lingua franca. Anthropic's Messages API is a different shape and would be a
//! second implementation behind the same trait, not a branch inside this one.
//!
//! ```text
//!   Envelope (§114)            ── unchanged, byte for byte ──▶  one user message
//!        │                                                            │
//!        │  disclosure: excerpts, files, bytes                        ▼
//!        ▼                                              POST /chat/completions
//!   what left the device (LLM-033)                       stream=true, SSE back
//! ```
//!
//! Three properties are load-bearing and each has a test:
//!
//! 1. **The prompt is the envelope and nothing else.** Retrieved content never
//!    grants authority, so it does not become a `system`
//!    message because an API makes that convenient. The bytes sent here are
//!    the same bytes the local worker gets.
//! 2. **The key is read at request time and never printed.** See
//!    [`crate::secrets`].
//! 3. **The boundary is decided by the resolved address**, not by the
//!    hostname, and the connection is pinned to the addresses that were
//!    classified — the same rule as Part 9's NET-007/NET-011, for a different
//!    reason: here it is the *label* that would otherwise lie.

use std::fmt;
use std::io::{BufRead, BufReader, Read};
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

use marrow_core::{Code, Error, Result};
use marrow_net::addr::{classify, AddressClass};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::provider::{
    Boundary, Completion, GenerateRequest, GenerationProvider, Notice, StopReason, StreamEvent,
    Usage,
};
use crate::secrets::{Secret, SecretStore};

/// How long to wait for the connection, and then for the first byte.
///
/// The body has no cap of its own: an answer legitimately takes minutes, and a
/// deadline on the whole stream would cut off the long answers a user asked a
/// large model for. Cancellation is what stops a running generation.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const FIRST_BYTE_TIMEOUT: Duration = Duration::from_secs(120);

/// The most of an error body that is read back to build a message.
///
/// A provider that answers a failure with a megabyte of HTML gets a sentence
/// of it quoted, not a megabyte held in memory.
const MAX_ERROR_BODY: u64 = 4096;

// ── configuration ──────────────────────────────────────────────────────────

/// Where to send a chat completion, and under what name.
///
/// **There is no key field, and there never can be one.** This struct is
/// serialised into `preferences.json`; the key is in the OS keyring under
/// `key_account` (LLM-030). A struct that could hold both would eventually
/// hold both.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Endpoint {
    /// The base, up to but not including `/chat/completions` — usually ending
    /// in `/v1`. Stored as the user typed it so the disclosure can show them
    /// their own words.
    pub base_url: String,
    /// The model name the endpoint knows. Sent verbatim: Marrow has no
    /// catalogue of somebody else's model names and inventing one would mean
    /// mapping a name the user can read to one they cannot.
    pub model: String,
    /// Which keyring account holds the key.
    pub key_account: String,
    pub max_output_tokens: u32,
    /// `low` · `medium` · `high`, sent as `reasoning_effort` when the user
    /// asks for Thorough. `None` means this endpoint has no portable way to be
    /// asked to reason, and Thorough is **refused** rather than silently
    /// answered as Fast (GEN-013).
    pub reasoning_effort: Option<String>,
}

impl Endpoint {
    /// A sensible starting point for a hand-typed endpoint.
    pub fn new(base_url: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            model: model.into(),
            key_account: "cloud-provider".into(),
            max_output_tokens: 2048,
            reasoning_effort: None,
        }
    }

    /// The URL a request actually goes to.
    pub fn completions_url(&self) -> String {
        let base = self.base_url.trim_end_matches('/');
        format!("{base}/chat/completions")
    }
}

/// The pieces of a URL this provider needs, checked once.
///
/// Not [`marrow_net::Url`]: that type enforces NET-001 and NET-002 — https
/// only, port 443 only — which are exactly right for a URL a *model* chose and
/// exactly wrong for an endpoint the *user* configured, where
/// `http://localhost:1234` is the commonest correct answer in the world.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub tls: bool,
    pub host: String,
    pub port: u16,
}

impl Target {
    pub fn parse(url: &str) -> Result<Self> {
        let refuse = |why: &str| {
            Err(Error::new(
                Code::CfgInvalid,
                format!("That endpoint address cannot be used: {why}"),
            ))
        };
        if url.len() > 2048 {
            return refuse(
                "it is longer than 2048 characters, which is a payload rather than an address.",
            );
        }
        if !url.is_ascii() {
            // NET-004's reasoning, and it applies here too: with no punycode
            // normalisation a homograph host is indistinguishable from the
            // real one, and guessing is worse than refusing.
            return refuse(
                "it contains non-ASCII characters. Marrow does not normalise \
                 international domain names, and a look-alike host would be \
                 indistinguishable from the real one.",
            );
        }
        let (tls, rest) = match url.split_once("://") {
            Some(("https", rest)) => (true, rest),
            Some(("http", rest)) => (false, rest),
            Some((other, _)) => {
                return refuse(&format!(
                    "`{other}` is not a scheme this can use. Use https, or http for a server on this machine."
                ))
            }
            None => return refuse("it has no scheme. Start it with https:// or http://."),
        };
        let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
        if authority.is_empty() {
            return refuse("it names no host.");
        }
        if authority.contains('@') {
            // NET-003. Nothing legitimate builds one of these.
            return refuse(
                "it carries credentials in the address. Save the key in Settings instead — \
                 it goes to the system keychain, not into a URL.",
            );
        }
        let (host, port) = if let Some(rest) = authority.strip_prefix('[') {
            // An IPv6 literal: `[::1]:1234`.
            let (h, tail) = rest
                .split_once(']')
                .ok_or_else(|| Error::new(Code::CfgInvalid, "That endpoint address cannot be used: the IPv6 address is missing its closing bracket."))?;
            (h.to_string(), tail.strip_prefix(':').map(str::to_string))
        } else {
            match authority.split_once(':') {
                Some((h, p)) => (h.to_string(), Some(p.to_string())),
                None => (authority.to_string(), None),
            }
        };
        if host.is_empty() {
            return refuse("it names no host.");
        }
        let port = match port {
            Some(p) => p.parse::<u16>().map_err(|_| {
                Error::new(
                    Code::CfgInvalid,
                    format!("That endpoint address cannot be used: `{p}` is not a port number."),
                )
            })?,
            None if tls => 443,
            None => 80,
        };
        Ok(Self { tls, host, port })
    }
}

// ── the seams ──────────────────────────────────────────────────────────────

/// What a name resolves to.
///
/// A seam for the same reason `net::fetch::Resolve` is one: the interesting
/// cases are "this ordinary-looking hostname answers `127.0.0.1`" and "this
/// one answers two addresses of different kinds", and neither can be produced
/// by asking the real DNS during a test.
pub trait Resolve: fmt::Debug + Send + Sync {
    /// **Every** address, not the first: a host that answers with a mixture is
    /// a host whose boundary cannot be stated in one word.
    fn resolve(&self, host: &str, port: u16) -> Result<Vec<IpAddr>>;
}

/// The real one.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemDns;

impl Resolve for SystemDns {
    fn resolve(&self, host: &str, port: u16) -> Result<Vec<IpAddr>> {
        let authority = if host.contains(':') {
            format!("[{host}]:{port}")
        } else {
            format!("{host}:{port}")
        };
        match authority.to_socket_addrs() {
            Ok(iter) => {
                let addrs: Vec<IpAddr> = iter.map(|s| s.ip()).collect();
                if addrs.is_empty() {
                    // NET-012's rule: nothing is a failure, not an empty
                    // permitted set.
                    return Err(unreachable_host(host, "it resolved to no addresses"));
                }
                Ok(addrs)
            }
            Err(e) => Err(unreachable_host(host, &e.to_string())),
        }
    }
}

/// One chat-completions request.
///
/// Separated from the provider so the wire format, the streaming, the
/// cancellation and the error mapping are all testable with no network at all
/// — following `net::fetch::Http`, and for the same reason: a transport that
/// can only be exercised against a live server is exercised rarely.
pub trait Chat: fmt::Debug + Send + Sync {
    fn post(&self, request: ChatRequest<'_>) -> Result<ChatResponse>;
}

/// What goes on the wire. The key is borrowed rather than owned so it lives
/// exactly as long as the call.
pub struct ChatRequest<'a> {
    pub url: &'a str,
    /// The addresses that were classified. The connection is pinned to these.
    pub addrs: &'a [SocketAddr],
    pub api_key: Option<&'a Secret>,
    pub body: &'a str,
}

/// Deliberately hand-written: the derived one would print `body`, which is the
/// user's documents, and `api_key`, which is the key.
impl fmt::Debug for ChatRequest<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChatRequest")
            .field("url", &self.url)
            .field("addrs", &self.addrs)
            .field("api_key", &self.api_key.map(|_| "<redacted>"))
            .field("body_bytes", &self.body.len())
            .finish()
    }
}

/// What came back. The body is a stream, because the whole point is not to
/// wait for the end of it.
pub struct ChatResponse {
    pub status: u16,
    pub body: Box<dyn Read + Send>,
}

impl fmt::Debug for ChatResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChatResponse")
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

/// The real transport: `ureq` over rustls, pinned to the checked addresses.
#[derive(Debug, Default, Clone, Copy)]
pub struct Https;

impl Chat for Https {
    fn post(&self, request: ChatRequest<'_>) -> Result<ChatResponse> {
        let config = ureq::config::Config::builder()
            // A 4xx has to come back as a 4xx: the body carries the provider's
            // own explanation, and that is most of what makes the error
            // specific rather than "the request failed".
            .http_status_as_error(false)
            // A redirect from a chat endpoint is not something to follow
            // silently to an address nobody classified.
            .max_redirects(0)
            .max_redirects_will_error(false)
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .timeout_recv_response(Some(FIRST_BYTE_TIMEOUT))
            .user_agent("Marrow/0.1 (local knowledge runtime)")
            .build();
        let agent = ureq::Agent::with_parts(
            config,
            ureq::unversioned::transport::DefaultConnector::new(),
            Pinned(request.addrs.to_vec()),
        );

        let mut call = agent
            .post(request.url)
            .header("content-type", "application/json")
            .header("accept", "text/event-stream");
        if let Some(key) = request.api_key {
            call = call.header("authorization", &format!("Bearer {}", key.expose()));
        }
        let response = call.send(request.body).map_err(|e| match e {
            ureq::Error::Timeout(_) => Error::new(
                Code::NetTimeout,
                "The provider did not answer within the time limit. Check the \
                 endpoint address, then try again.",
            ),
            other => {
                // `other.to_string()` names the host and the OS error. It
                // never contains a header, so it never contains the key.
                unreachable_host(host_of(request.url).as_str(), &other.to_string())
            }
        })?;
        Ok(ChatResponse {
            status: response.status().as_u16(),
            body: Box::new(response.into_body().into_reader()),
        })
    }
}

/// A resolver that has already been told the answer (NET-011's mechanism).
#[derive(Debug)]
struct Pinned(Vec<SocketAddr>);

impl ureq::unversioned::resolver::Resolver for Pinned {
    fn resolve(
        &self,
        _uri: &ureq::http::Uri,
        _config: &ureq::config::Config,
        _timeout: ureq::unversioned::transport::NextTimeout,
    ) -> std::result::Result<ureq::unversioned::resolver::ResolvedSocketAddrs, ureq::Error> {
        let mut out = self.empty();
        for addr in self.0.iter().take(16) {
            out.push(*addr);
        }
        if out.is_empty() {
            return Err(ureq::Error::HostNotFound);
        }
        Ok(out)
    }
}

// ── the boundary ───────────────────────────────────────────────────────────

/// Which side of the boundary an endpoint is on, decided by the addresses it
/// resolves to.
///
/// **The user is not asked, and the hostname is not consulted.** Both of the
/// obvious alternatives are worse:
///
/// - *Ask them.* "Is this your own server?" is a question about network
///   topology answered by someone who is configuring a text field. A wrong
///   answer produces a UI that says "on your own server" while the documents
///   go to a company — the exact failure UX-012 exists to prevent.
/// - *Match the hostname.* `localhost` is a name, and a name is an
///   indirection somebody else controls. Part 9 §153.2 is a page and a half
///   about why, and the lesson transfers unchanged.
///
/// So the resolved address decides, every notation is unwrapped first
/// ([`marrow_net::addr::classify`], NET-009), and **anything not certainly the
/// user's own is `Cloud`** — a mixture, a reserved range, an address nobody
/// classified. The failure direction matters: over-warning costs a word on
/// screen, under-warning costs the user's documents.
pub fn boundary_of(addrs: &[IpAddr]) -> Boundary {
    if addrs.is_empty() {
        return Boundary::Cloud;
    }
    let all = |f: fn(AddressClass) -> bool| addrs.iter().copied().map(classify).all(f);
    if all(|c| {
        matches!(
            c,
            AddressClass::Loopback
                | AddressClass::Unspecified
                | AddressClass::Private
                | AddressClass::LinkLocal
                | AddressClass::UniqueLocal
                | AddressClass::SharedAddressSpace
        )
    }) {
        // Loopback is this machine; the rest are networks the user is on —
        // including 100.64/10, which is where Tailscale puts their other
        // machines. `Private` is the enum's word for "a server the user runs",
        // and all of these are that.
        Boundary::Private
    } else {
        Boundary::Cloud
    }
}

// ── the provider ───────────────────────────────────────────────────────────

/// A [`GenerationProvider`] that speaks chat-completions.
///
/// Constructed per request rather than held open, because it costs a DNS
/// lookup and no memory — unlike the local worker, which holds several
/// gigabytes and is cached for exactly that reason. Constructing it per
/// request is also what makes [`GenerationProvider::boundary`] honest: it
/// describes the addresses *this* request will be pinned to, not the ones some
/// earlier request saw.
#[derive(Debug)]
pub struct OpenAiProvider {
    endpoint: Endpoint,
    url: String,
    target: Target,
    addrs: Vec<SocketAddr>,
    boundary: Boundary,
    label: String,
    secrets: Arc<dyn SecretStore>,
    http: Arc<dyn Chat>,
}

impl OpenAiProvider {
    /// Resolve, classify, and refuse the configurations that cannot be made
    /// safe. Does not contact the endpoint.
    pub fn connect(
        endpoint: Endpoint,
        label: impl Into<String>,
        secrets: Arc<dyn SecretStore>,
        http: Arc<dyn Chat>,
        resolver: &dyn Resolve,
    ) -> Result<Self> {
        if endpoint.model.trim().is_empty() {
            return Err(Error::new(
                Code::CfgInvalid,
                "No model name is set for that endpoint. Enter the name the \
                 endpoint uses — Marrow sends it verbatim.",
            ));
        }
        let target = Target::parse(&endpoint.base_url)?;
        let addrs = resolver.resolve(&target.host, target.port)?;
        let boundary = boundary_of(&addrs);
        if !target.tls && boundary != Boundary::Private {
            return Err(Error::new(
                Code::CfgInvalid,
                format!(
                    "{} is not on this machine or on your own network, so plain http \
                     cannot be used for it — the question and every excerpt would cross \
                     the wire in clear text. Use https://.",
                    target.host
                ),
            ));
        }
        Ok(Self {
            url: endpoint.completions_url(),
            addrs: addrs
                .iter()
                .map(|a| SocketAddr::new(*a, target.port))
                .collect(),
            boundary,
            label: label.into(),
            target,
            endpoint,
            secrets,
            http,
        })
    }

    /// The real thing: system DNS, `ureq` over rustls, the OS keyring.
    pub fn live(endpoint: Endpoint, label: impl Into<String>) -> Result<Self> {
        Self::connect(
            endpoint,
            label,
            Arc::new(crate::secrets::Keyring),
            Arc::new(Https),
            &SystemDns,
        )
    }

    /// The host the request goes to, for the disclosure.
    pub fn host(&self) -> &str {
        &self.target.host
    }

    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// The addresses the connection is pinned to, for the disclosure. A
    /// boundary the user cannot check is a claim rather than a fact.
    pub fn addresses(&self) -> Vec<String> {
        self.addrs.iter().map(|a| a.ip().to_string()).collect()
    }

    fn body(&self, request: &GenerateRequest<'_>) -> Result<String> {
        let mut root = serde_json::Map::new();
        root.insert("model".into(), self.endpoint.model.clone().into());
        root.insert("stream".into(), true.into());
        // `max_tokens` rather than `max_completion_tokens`: the older name is
        // what every compatible server accepts, and this provider exists to
        // reach all of them rather than the newest one.
        root.insert(
            "max_tokens".into(),
            request
                .max_output_tokens
                .min(self.endpoint.max_output_tokens)
                .into(),
        );
        // Asks for the usage totals on the final chunk. A server that does not
        // know the field ignores it, and the fallback is counted tokens.
        root.insert(
            "stream_options".into(),
            serde_json::json!({ "include_usage": true }),
        );
        if request.reasoning.is_on() {
            match &self.endpoint.reasoning_effort {
                Some(effort) => {
                    root.insert("reasoning_effort".into(), effort.clone().into());
                }
                None => {
                    // GEN-013: refused with the reason, never silently
                    // downgraded to a Fast answer wearing a Thorough label.
                    return Err(Error::new(
                        Code::ModUnsupportedCapability,
                        format!(
                            "{} is not configured to reason before answering. Set a \
                             reasoning effort for it in Settings, or ask in Fast mode.",
                            self.label
                        ),
                    ));
                }
            }
        }
        // **One user message, the envelope verbatim.**
        //
        // Not a system message and a set of assistant messages: retrieved file
        // content never grants authority, and the §114 envelope
        // is the mechanism that says so — its own SYS block is first, its
        // untrusted evidence is labelled, and a runtime instruction is last.
        // Splitting it across chat roles would put the user's documents into
        // the one role every model is trained to obey, and would do it because
        // an API made it convenient.
        root.insert(
            "messages".into(),
            serde_json::json!([{ "role": "user", "content": request.envelope.text }]),
        );
        serde_json::to_string(&serde_json::Value::Object(root))
            .map_err(|e| Error::invariant("the request body could not be encoded").with_source(e))
    }
}

impl GenerationProvider for OpenAiProvider {
    fn boundary(&self) -> Boundary {
        self.boundary
    }

    fn describe(&self) -> String {
        // LLM-039: specific enough to debug. "cloud" is not a name.
        format!("{} via {}", self.endpoint.model, self.target.host)
    }

    fn generate(
        &self,
        request: GenerateRequest<'_>,
        on_event: &mut dyn FnMut(StreamEvent),
    ) -> Result<Completion> {
        let body = self.body(&request)?;
        // Read here rather than held on the struct: a key rotated in the
        // keychain takes effect on the next question, and the value exists for
        // the length of one call.
        let key = match self.endpoint.key_account.trim() {
            "" => None,
            account => self.secrets.get(account)?.filter(|s| !s.is_empty()),
        };
        if key.is_none() && self.boundary == Boundary::Cloud {
            return Err(Error::new(
                Code::CfgInvalid,
                format!(
                    "No key is saved for {}. Add it in Settings — it goes to the \
                     system keychain, and Marrow never sends it anywhere but {}.",
                    self.label, self.target.host
                ),
            ));
        }

        // Never the body and never the key: the body is the user's documents
        // and this is the same rule as NET-051.
        debug!(
            host = %self.target.host,
            model = %self.endpoint.model,
            boundary = ?self.boundary,
            prompt_bytes = request.envelope.text.len(),
            excerpts = request.envelope.disclosure.evidence_blocks,
            files = request.envelope.disclosure.distinct_sources,
            "sending a chat completion"
        );

        if request.cancel.is_cancelled() {
            return Ok(cancelled(self, String::new(), String::new(), 0, 0).announced(on_event));
        }

        let response = self.http.post(ChatRequest {
            url: &self.url,
            addrs: &self.addrs,
            api_key: key.as_ref(),
            body: &body,
        })?;
        if response.status != 200 {
            let status = response.status;
            let detail = read_capped(response.body, MAX_ERROR_BODY);
            return Err(self.map_status(status, &detail));
        }

        self.stream(response.body, &request, on_event)
    }
}

impl OpenAiProvider {
    /// Read the event stream, emitting tokens as they arrive.
    fn stream(
        &self,
        body: Box<dyn Read + Send>,
        request: &GenerateRequest<'_>,
        on_event: &mut dyn FnMut(StreamEvent),
    ) -> Result<Completion> {
        let mut reader = BufReader::new(body);
        let mut line = String::new();
        let mut text = String::new();
        let mut thinking = String::new();
        let mut streamed = 0u32;
        let mut streamed_thinking = 0u32;
        let mut usage: Option<Usage> = None;
        let mut stop = StopReason::Stop;
        let mut warned_unreadable = false;

        loop {
            if request.cancel.is_cancelled() {
                // The reader is **dropped**, not drained. Draining would keep
                // the provider generating — and billing — for an answer nobody
                // is waiting for, on a connection nobody is watching.
                drop(reader);
                return Ok(cancelled(self, text, thinking, streamed, streamed_thinking)
                    .announced(on_event));
            }
            line.clear();
            // Cancellation is checked between events. A provider that has
            // stopped sending holds this read until the transport's own
            // timeout; in exchange, a token arriving is felt immediately and
            // there is no second thread to leave draining.
            let read = reader.read_line(&mut line).map_err(|e| {
                Error::new(
                    Code::NetTimeout,
                    format!(
                        "The connection to {} ended while the answer was still arriving. \
                         What is above is what had been written.",
                        self.target.host
                    ),
                )
                .with_source(e)
            })?;
            if read == 0 {
                // End of stream with no `[DONE]`. Not an error: several
                // servers simply close.
                break;
            }
            let Some(data) = line.trim_end().strip_prefix("data:") else {
                // Comments (`: keep-alive`), blank separators and `event:`
                // lines are all legitimately ignorable.
                continue;
            };
            let data = data.trim();
            if data == "[DONE]" {
                break;
            }
            if data.is_empty() {
                continue;
            }
            let chunk: Chunk = match serde_json::from_str(data) {
                Ok(c) => c,
                Err(e) => {
                    // One unreadable chunk is not a reason to throw away an
                    // answer that is arriving. It is also not something to keep
                    // to the log: a few words may be missing from the middle of
                    // the text the user is reading, and only they can judge
                    // whether that matters. Said once — a server producing
                    // garbage produces a lot of it, and one sentence repeated
                    // four hundred times is not more informative than one.
                    warn!(error = %e, "a chunk of the event stream did not parse");
                    if !warned_unreadable {
                        warned_unreadable = true;
                        on_event(StreamEvent::notice(format!(
                            "Part of the response from {} could not be read, so a \
                             few words may be missing from this answer. Ask again \
                             if it reads wrongly.",
                            self.target.host
                        )));
                    }
                    continue;
                }
            };
            if let Some(err) = chunk.error {
                return Err(self.map_status(err.status_hint(), &err.message()));
            }
            if let Some(u) = chunk.usage {
                usage = Some(u.into());
            }
            for choice in chunk.choices {
                if let Some(delta) = choice.delta {
                    // `reasoning_content` is what the OpenAI-compatible
                    // reasoning servers emit. Kept apart from the answer so
                    // the UI never has to guess which half it is rendering,
                    // and never citable (GEN-015).
                    if let Some(t) = delta.reasoning_content.or(delta.reasoning) {
                        if !t.is_empty() {
                            thinking.push_str(&t);
                            streamed_thinking += 1;
                            on_event(StreamEvent::thinking(t));
                        }
                    }
                    if let Some(t) = delta.content {
                        if !t.is_empty() {
                            text.push_str(&t);
                            streamed += 1;
                            on_event(StreamEvent::text(t));
                        }
                    }
                }
                if let Some(reason) = choice.finish_reason {
                    stop = match reason.as_str() {
                        "length" | "max_tokens" => StopReason::Length,
                        // Everything else is *presented* as a clean stop,
                        // because there is no third [`StopReason`] and
                        // inventing one is a change to what every consumer
                        // renders. But `content_filter` is not a clean stop and
                        // neither is a reason this build has never heard of,
                        // and until there was somewhere to say so they were
                        // silently identical to "the model finished". That is
                        // the shape of failure this crate exists to refuse:
                        // an answer that looks complete and is not.
                        other => {
                            if !matches!(other, "stop" | "eos" | "end_turn") {
                                on_event(StreamEvent::Notice(Notice::new(format!(
                                    "{} stopped this answer early and gave the reason \
                                     \"{other}\". What is above may be incomplete.",
                                    self.target.host
                                ))));
                            }
                            StopReason::Stop
                        }
                    };
                }
            }
        }

        // The counted fallback, for a server that does not send usage. One
        // delta is not one token, so these are approximate — but they are the
        // approximation the local worker already makes, and a zero under
        // several hundred visible words is a wrong number rather than a
        // missing one.
        let usage = usage.unwrap_or(Usage {
            prompt_tokens: 0,
            output_tokens: streamed,
            thinking_tokens: streamed_thinking,
            cached_prefix_tokens: 0,
        });
        Ok(Completion {
            text,
            thinking: (!thinking.is_empty()).then_some(thinking),
            usage,
            stop_reason: stop,
            boundary: self.boundary,
            model_id: self.endpoint.model.clone(),
        }
        .announced(on_event))
    }

    /// Four different problems, four different actions (SUP-001).
    ///
    /// A wrong key, a rate limit, a model the endpoint does not have and a
    /// server that is not there are not one failure called "the request
    /// failed", and the difference is what the user does next.
    fn map_status(&self, status: u16, detail: &str) -> Error {
        let said = provider_said(detail);
        let host = &self.target.host;
        match status {
            401 | 403 => Error::new(
                Code::NetBadStatus,
                format!(
                    "{host} rejected the key (HTTP {status}). Check the key saved in \
                     Settings, and that it is a key for this endpoint.{said}"
                ),
            ),
            429 => Error::new(
                Code::NetRateLimited,
                format!(
                    "{host} is rate-limiting this key (HTTP 429). Wait and ask again, \
                     or use a local model in the meantime.{said}"
                ),
            ),
            // A model name the endpoint does not have. `model_not_found` is
            // what OpenAI says; the others put the word in the message.
            400 | 404 if mentions_model(detail) => Error::new(
                Code::ModNotInstalled,
                format!(
                    "{host} has no model called `{}`. Check the name in Settings against \
                     the models this endpoint offers — Marrow sends it verbatim.{said}",
                    self.endpoint.model
                ),
            ),
            404 => Error::new(
                Code::NetBadStatus,
                format!(
                    "There is no chat-completions endpoint at {}. Check the address in \
                     Settings — it usually ends in `/v1`.{said}",
                    self.url
                ),
            ),
            500..=599 => Error::new(
                Code::NetBadStatus,
                format!("{host} reported a server error (HTTP {status}). Try again shortly.{said}"),
            ),
            _ => Error::new(
                Code::NetBadStatus,
                format!("{host} refused the request (HTTP {status}).{said}"),
            ),
        }
    }
}

/// A cancelled generation returns what streamed, exactly as the local worker
/// does — the two must not disagree about what "stopped" looks like.
fn cancelled(
    provider: &OpenAiProvider,
    text: String,
    thinking: String,
    streamed: u32,
    streamed_thinking: u32,
) -> Completion {
    Completion {
        text,
        thinking: (!thinking.is_empty()).then_some(thinking),
        usage: Usage {
            prompt_tokens: 0,
            output_tokens: streamed,
            thinking_tokens: streamed_thinking,
            cached_prefix_tokens: 0,
        },
        stop_reason: StopReason::Cancelled,
        boundary: provider.boundary,
        model_id: provider.endpoint.model.clone(),
    }
}

fn unreachable_host(host: &str, detail: &str) -> Error {
    Error::new(
        Code::NetUnreachable,
        format!(
            "{host} could not be reached. Check the endpoint address in Settings, \
             and that the server is running."
        ),
    )
    .with_context(detail.to_string())
}

fn host_of(url: &str) -> String {
    Target::parse(url)
        .map(|t| t.host)
        .unwrap_or_else(|_| "the endpoint".to_string())
}

/// Quote the provider's own explanation, trimmed to one readable sentence.
fn provider_said(detail: &str) -> String {
    let message = serde_json::from_str::<ErrorEnvelope>(detail)
        .ok()
        .map(|e| e.error.message())
        .unwrap_or_else(|| detail.trim().to_string());
    let flat = message.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.is_empty() {
        return String::new();
    }
    let cut: String = flat.chars().take(200).collect();
    format!(" It said: {cut}")
}

fn mentions_model(detail: &str) -> bool {
    detail.to_ascii_lowercase().contains("model")
}

fn read_capped(mut body: Box<dyn Read + Send>, cap: u64) -> String {
    let mut out = Vec::new();
    let _ = std::io::copy(&mut body.as_mut().take(cap), &mut out);
    String::from_utf8_lossy(&out).into_owned()
}

// ── the wire ───────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct Chunk {
    #[serde(default)]
    choices: Vec<Choice>,
    #[serde(default)]
    usage: Option<WireUsage>,
    /// Some servers deliver a failure inside the stream rather than as a
    /// status. It is the same failure and gets the same message.
    #[serde(default)]
    error: Option<WireError>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    #[serde(default)]
    delta: Option<Delta>,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Delta {
    #[serde(default)]
    content: Option<String>,
    /// DeepSeek and the vLLM reasoning parsers.
    #[serde(default)]
    reasoning_content: Option<String>,
    /// OpenRouter's spelling of the same thing.
    #[serde(default)]
    reasoning: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WireUsage {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
    #[serde(default)]
    completion_tokens_details: Option<CompletionDetails>,
    #[serde(default)]
    prompt_tokens_details: Option<PromptDetails>,
}

#[derive(Debug, Default, Deserialize)]
struct CompletionDetails {
    #[serde(default)]
    reasoning_tokens: u32,
}

#[derive(Debug, Default, Deserialize)]
struct PromptDetails {
    #[serde(default)]
    cached_tokens: u32,
}

impl From<WireUsage> for Usage {
    fn from(w: WireUsage) -> Self {
        let thinking = w
            .completion_tokens_details
            .unwrap_or_default()
            .reasoning_tokens;
        Usage {
            prompt_tokens: w.prompt_tokens,
            // The provider's `completion_tokens` includes reasoning; Marrow
            // counts the two apart (GEN-016), so the answer's own total is
            // what is left after the thinking.
            output_tokens: w.completion_tokens.saturating_sub(thinking),
            thinking_tokens: thinking,
            // Their prompt cache, reported in the field Marrow already has for
            // "why was the second question faster" (LLM-045).
            cached_prefix_tokens: w.prompt_tokens_details.unwrap_or_default().cached_tokens,
        }
    }
}

#[derive(Debug, Deserialize)]
struct ErrorEnvelope {
    error: WireError,
}

#[derive(Debug, Default, Deserialize)]
struct WireError {
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    r#type: Option<String>,
}

impl WireError {
    fn message(&self) -> String {
        self.message
            .clone()
            .or_else(|| self.code.clone())
            .or_else(|| self.r#type.clone())
            .unwrap_or_default()
    }

    /// An error delivered inside a 200 stream has no status of its own. The
    /// code is what says which failure it is.
    fn status_hint(&self) -> u16 {
        let code = format!(
            "{} {}",
            self.code.clone().unwrap_or_default(),
            self.r#type.clone().unwrap_or_default()
        )
        .to_ascii_lowercase();
        if code.contains("rate_limit") {
            429
        } else if code.contains("authentication") || code.contains("invalid_api_key") {
            401
        } else if code.contains("model") {
            404
        } else {
            500
        }
    }
}

#[cfg(test)]
mod tests;
