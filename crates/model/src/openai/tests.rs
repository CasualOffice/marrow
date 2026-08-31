//! Everything here runs with no network and no key.
//!
//! One test starts a TCP listener on `127.0.0.1` and serves a canned HTTP
//! response, which exercises the **real** `ureq` transport, the real pinned
//! resolver and the real SSE reader. It is not `#[ignore]`d because it touches
//! nothing outside this machine — but it is the only one that opens a socket,
//! and every rule below it is proved against a fake.

use std::collections::HashMap;
use std::io::{Cursor, Read};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use marrow_core::{Origin, ProvenanceClass, SourceSpan};

use super::*;
use crate::envelope::{Builder, Evidence, RandomNonce};
use crate::provider::Finish;
use crate::queue::Cancel;
use crate::request::Reasoning;
use crate::secrets::MemorySecrets;

const KEY: &str = "sk-test-never-print-me";

// ── fakes ──────────────────────────────────────────────────────────────────

/// Says what a name resolves to, which is the whole boundary question.
#[derive(Debug, Default)]
struct Dns(HashMap<String, Vec<IpAddr>>);

impl Dns {
    fn with(pairs: &[(&str, &str)]) -> Self {
        let mut m: HashMap<String, Vec<IpAddr>> = HashMap::new();
        for (host, ip) in pairs {
            m.entry((*host).to_string())
                .or_default()
                .push(ip.parse().expect("test address must parse"));
        }
        Dns(m)
    }
}

impl Resolve for Dns {
    fn resolve(&self, host: &str, _port: u16) -> Result<Vec<IpAddr>> {
        match self.0.get(host) {
            Some(v) => Ok(v.clone()),
            None => Err(unreachable_host(host, "no such host in the test resolver")),
        }
    }
}

/// A reader that counts what was actually consumed, so "the stream was
/// abandoned rather than drained" is an assertion rather than a hope.
#[derive(Debug)]
struct Counted {
    inner: Cursor<Vec<u8>>,
    read: Arc<AtomicUsize>,
}

impl Read for Counted {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.read.fetch_add(n, Ordering::SeqCst);
        Ok(n)
    }
}

#[derive(Debug, Default)]
struct Recorded {
    url: String,
    body: String,
    authorization: Option<String>,
}

/// A transport that returns a canned response and remembers what it was asked
/// to send.
#[derive(Debug)]
struct Stub {
    status: u16,
    payload: Vec<u8>,
    seen: Mutex<Vec<Recorded>>,
    consumed: Arc<AtomicUsize>,
}

impl Stub {
    fn new(status: u16, payload: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            payload: payload.into(),
            seen: Mutex::new(Vec::new()),
            consumed: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn ok(payload: &str) -> Self {
        Self::new(200, payload.as_bytes().to_vec())
    }

    fn last(&self) -> Recorded {
        self.seen
            .lock()
            .expect("lock")
            .last()
            .cloned()
            .unwrap_or_default()
    }
}

impl Clone for Recorded {
    fn clone(&self) -> Self {
        Self {
            url: self.url.clone(),
            body: self.body.clone(),
            authorization: self.authorization.clone(),
        }
    }
}

impl Chat for Stub {
    fn post(&self, request: ChatRequest<'_>) -> Result<ChatResponse> {
        self.seen.lock().expect("lock").push(Recorded {
            url: request.url.to_string(),
            body: request.body.to_string(),
            authorization: request.api_key.map(|k| format!("Bearer {}", k.expose())),
        });
        Ok(ChatResponse {
            status: self.status,
            body: Box::new(Counted {
                inner: Cursor::new(self.payload.clone()),
                read: Arc::clone(&self.consumed),
            }),
        })
    }
}

// ── helpers ────────────────────────────────────────────────────────────────

fn envelope() -> crate::envelope::Envelope {
    Builder::new("You are Marrow.", "when does the lease renew?")
        .evidence(Evidence {
            id: "E1".into(),
            text: "the agreement renews on 31 December 2026".into(),
            source: "leases/office.pdf".into(),
            span: SourceSpan::Whole,
            provenance: ProvenanceClass::Exact,
            external: false,
            origin: Origin::User,
        })
        .finish(&mut RandomNonce)
}

fn provider(endpoint: Endpoint, dns: &Dns, http: Arc<dyn Chat>) -> OpenAiProvider {
    OpenAiProvider::connect(
        endpoint,
        "Test provider",
        Arc::new(MemorySecrets::with("cloud-provider", KEY)),
        http,
        dns,
    )
    .expect("the endpoint must be usable")
}

fn cloud_endpoint() -> Endpoint {
    Endpoint::new("https://api.example.com/v1", "gpt-test")
}

fn cloud_dns() -> Dns {
    Dns::with(&[("api.example.com", "93.184.216.34")])
}

fn run(
    p: &OpenAiProvider,
    env: &crate::envelope::Envelope,
    cancel: &Cancel,
    on_event: &mut dyn FnMut(StreamEvent),
) -> Result<Completion> {
    p.generate(
        GenerateRequest {
            model_id: "gpt-test",
            envelope: env,
            reasoning: Reasoning::Off,
            max_output_tokens: 256,
            cancel,
        },
        on_event,
    )
}

/// Two content deltas, a finish reason and the usage totals — the shape every
/// compatible server produces.
const STREAM: &str = concat!(
    ": keep-alive\n",
    "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\n",
    "\n",
    "data: {\"choices\":[{\"delta\":{\"content\":\"It renews \"}}]}\n",
    "data: {\"choices\":[{\"delta\":{\"content\":\"on 31 December 2026 [E1].\"}}]}\n",
    "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n",
    "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":812,\"completion_tokens\":11,\
       \"prompt_tokens_details\":{\"cached_tokens\":768}}}\n",
    "data: [DONE]\n",
);

// ── the wire format ────────────────────────────────────────────────────────

#[test]
fn the_prompt_is_the_envelope_and_nothing_else() {
    // Retrieved content never grants authority. The convenient shape for this API is a system message of
    // instructions plus the evidence as context, and that is precisely the
    // shape that grants retrieved content authority. The bytes sent must be
    // the bytes the local worker gets.
    let http = Arc::new(Stub::ok(STREAM));
    let p = provider(cloud_endpoint(), &cloud_dns(), http.clone());
    let env = envelope();
    run(&p, &env, &Cancel::new(), &mut |_| {}).expect("generate");

    let sent: serde_json::Value =
        serde_json::from_str(&http.last().body).expect("the body must be JSON");
    let messages = sent["messages"].as_array().expect("messages");
    assert_eq!(
        messages.len(),
        1,
        "one message, not a role-per-block prompt"
    );
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(
        messages[0]["content"].as_str().expect("content"),
        env.text,
        "the envelope goes on the wire byte for byte"
    );
    assert!(
        sent.get("system").is_none(),
        "there is no separate system field for evidence to be promoted into"
    );
    assert_eq!(sent["model"], "gpt-test");
    assert_eq!(sent["stream"], true);
    assert_eq!(
        http.last().url,
        "https://api.example.com/v1/chat/completions"
    );
}

#[test]
fn the_evidence_never_becomes_a_system_message() {
    // Stated separately from the shape above because this is the property, and
    // the shape is only how it is currently achieved: whatever the message
    // list becomes, no excerpt may appear in a `system` role.
    let http = Arc::new(Stub::ok(STREAM));
    let p = provider(cloud_endpoint(), &cloud_dns(), http.clone());
    run(&p, &envelope(), &Cancel::new(), &mut |_| {}).expect("generate");
    let sent: serde_json::Value = serde_json::from_str(&http.last().body).expect("json");
    for m in sent["messages"].as_array().expect("messages") {
        if m["role"] == "system" {
            assert!(
                !m["content"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("31 December 2026"),
                "an excerpt reached the one role every model is trained to obey"
            );
        }
    }
}

#[test]
fn the_key_is_sent_as_a_bearer_header_and_nowhere_else() {
    let http = Arc::new(Stub::ok(STREAM));
    let p = provider(cloud_endpoint(), &cloud_dns(), http.clone());
    run(&p, &envelope(), &Cancel::new(), &mut |_| {}).expect("generate");
    let seen = http.last();
    assert_eq!(
        seen.authorization.as_deref(),
        Some("Bearer sk-test-never-print-me")
    );
    assert!(
        !seen.body.contains(KEY),
        "the key must not be duplicated into the request body"
    );
    assert!(!seen.url.contains(KEY), "nor into the URL");
}

#[test]
fn a_provider_with_no_key_configured_is_refused_before_the_request() {
    // Better than a 401 the user has to interpret: the missing thing is named
    // and so is where it goes.
    let p = OpenAiProvider::connect(
        cloud_endpoint(),
        "Test provider",
        Arc::new(MemorySecrets::new()),
        Arc::new(Stub::ok(STREAM)),
        &cloud_dns(),
    )
    .expect("connect");
    let e = run(&p, &envelope(), &Cancel::new(), &mut |_| {}).expect_err("must refuse");
    assert_eq!(e.code(), marrow_core::Code::CfgInvalid);
    assert!(e.message().contains("keychain"), "{}", e.message());
}

#[test]
fn a_local_endpoint_needs_no_key_at_all() {
    // LM Studio and llama.cpp want none, and demanding one would make the
    // commonest private endpoint the hardest to configure.
    let p = OpenAiProvider::connect(
        Endpoint::new("http://localhost:1234/v1", "qwen"),
        "LM Studio",
        Arc::new(MemorySecrets::new()),
        Arc::new(Stub::ok(STREAM)),
        &Dns::with(&[("localhost", "127.0.0.1")]),
    )
    .expect("connect");
    assert_eq!(p.boundary(), Boundary::Private);
    let c = run(&p, &envelope(), &Cancel::new(), &mut |_| {}).expect("generate");
    assert_eq!(c.boundary, Boundary::Private);
}

#[test]
fn thorough_is_refused_rather_than_answered_as_fast() {
    // GEN-013. There is no portable field for "think first", so an endpoint
    // that has not been told how must say so.
    let p = provider(cloud_endpoint(), &cloud_dns(), Arc::new(Stub::ok(STREAM)));
    let env = envelope();
    let e = p
        .generate(
            GenerateRequest {
                model_id: "gpt-test",
                envelope: &env,
                reasoning: Reasoning::THOROUGH,
                max_output_tokens: 256,
                cancel: &Cancel::new(),
            },
            &mut |_| {},
        )
        .expect_err("must refuse");
    assert_eq!(e.code(), marrow_core::Code::ModUnsupportedCapability);

    // And when it has been told how, the field goes on the wire.
    let mut endpoint = cloud_endpoint();
    endpoint.reasoning_effort = Some("high".into());
    let http = Arc::new(Stub::ok(STREAM));
    let p = provider(endpoint, &cloud_dns(), http.clone());
    p.generate(
        GenerateRequest {
            model_id: "gpt-test",
            envelope: &env,
            reasoning: Reasoning::THOROUGH,
            max_output_tokens: 256,
            cancel: &Cancel::new(),
        },
        &mut |_| {},
    )
    .expect("generate");
    let sent: serde_json::Value = serde_json::from_str(&http.last().body).expect("json");
    assert_eq!(sent["reasoning_effort"], "high");
}

// ── streaming ──────────────────────────────────────────────────────────────

#[test]
fn tokens_arrive_as_they_stream_rather_than_at_the_end() {
    let p = provider(cloud_endpoint(), &cloud_dns(), Arc::new(Stub::ok(STREAM)));
    let mut seen = Vec::new();
    let c = run(&p, &envelope(), &Cancel::new(), &mut |t| seen.push(t)).expect("generate");
    assert_eq!(
        seen,
        vec![
            StreamEvent::text("It renews "),
            StreamEvent::text("on 31 December 2026 [E1]."),
            // The cost and the stop reason arrive *on the stream*, last. They
            // used to exist only as this function's return value, so a window
            // rendering tokens could not put a number in the footer until the
            // whole answer was over.
            StreamEvent::Finish(Finish::new(c.usage, StopReason::Stop)),
        ]
    );
    assert_eq!(c.text, "It renews on 31 December 2026 [E1].");
    assert_eq!(c.stop_reason, StopReason::Stop);
    assert_eq!(c.usage.prompt_tokens, 812);
    assert_eq!(c.usage.output_tokens, 11);
    assert_eq!(
        c.usage.cached_prefix_tokens, 768,
        "their prompt cache answers 'why was the second question faster'"
    );
}

#[test]
fn reasoning_deltas_are_kept_apart_from_the_answer() {
    // GEN-014/GEN-015: kept, shown collapsed, never citable. Two spellings,
    // because the compatible servers do not agree on one.
    let stream = concat!(
        "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"the lease says…\"}}]}\n",
        "data: {\"choices\":[{\"delta\":{\"reasoning\":\" and so\"}}]}\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"31 December 2026.\"}}]}\n",
        "data: [DONE]\n",
    );
    let p = provider(cloud_endpoint(), &cloud_dns(), Arc::new(Stub::ok(stream)));
    let mut seen = Vec::new();
    let c = run(&p, &envelope(), &Cancel::new(), &mut |t| seen.push(t)).expect("generate");
    assert_eq!(c.text, "31 December 2026.");
    assert_eq!(c.thinking.as_deref(), Some("the lease says… and so"));
    assert!(matches!(seen[0], StreamEvent::Thinking { .. }));
    assert!(matches!(seen[2], StreamEvent::Text { .. }));
}

#[test]
fn an_answer_cut_off_at_the_limit_is_labelled_rather_than_presented_as_complete() {
    let stream = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"It renews on\"}}]}\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"length\"}]}\n",
        "data: [DONE]\n",
    );
    let p = provider(cloud_endpoint(), &cloud_dns(), Arc::new(Stub::ok(stream)));
    let c = run(&p, &envelope(), &Cancel::new(), &mut |_| {}).expect("generate");
    assert_eq!(c.stop_reason, StopReason::Length);
}

#[test]
fn a_chunk_that_does_not_parse_does_not_throw_away_the_answer() {
    // A keep-alive comment, a blank line, and one malformed frame. Losing the
    // whole generation to any of them would be a worse failure than the frame.
    let stream = concat!(
        ": ping\n",
        "\n",
        "data: {not json at all}\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"still here\"}}]}\n",
        "data: [DONE]\n",
    );
    let p = provider(cloud_endpoint(), &cloud_dns(), Arc::new(Stub::ok(stream)));
    let c = run(&p, &envelope(), &Cancel::new(), &mut |_| {}).expect("generate");
    assert_eq!(c.text, "still here");
}

#[test]
fn a_warning_mid_stream_does_not_end_the_answer() {
    // The case the event exists for. Something worth telling the user happens
    // half way through — here a frame that could not be read, so a few words
    // of their answer are missing — and the answer keeps arriving afterwards.
    // Before there was a `Notice`, this was a `warn!` in a log the user will
    // never open, or a hard error that threw away a good answer.
    let stream = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"It renews \"}}]}\n",
        "data: {not json at all}\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"on 31 December 2026 [E1].\"}}]}\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n",
        "data: [DONE]\n",
    );
    let p = provider(cloud_endpoint(), &cloud_dns(), Arc::new(Stub::ok(stream)));
    let mut seen = Vec::new();
    let c = run(&p, &envelope(), &Cancel::new(), &mut |e| seen.push(e)).expect("generate");

    let at = seen
        .iter()
        .position(|e| matches!(e, StreamEvent::Notice(_)))
        .expect("the unreadable frame must be said out loud, not only logged");
    assert!(
        seen[at + 1..]
            .iter()
            .any(|e| matches!(e, StreamEvent::Text { .. })),
        "text must keep arriving after the warning: {seen:?}"
    );
    assert!(
        matches!(seen.last(), Some(StreamEvent::Finish(_))),
        "and the stream must still finish: {seen:?}"
    );
    assert_eq!(c.text, "It renews on 31 December 2026 [E1].");
    assert_eq!(c.stop_reason, StopReason::Stop);

    let StreamEvent::Notice(n) = &seen[at] else {
        unreachable!()
    };
    assert!(n.message.contains("api.example.com"), "{}", n.message);
    assert!(
        n.message.contains("Ask again"),
        "cause and action, not just cause: {}",
        n.message
    );
}

#[test]
fn only_one_warning_however_many_frames_are_unreadable() {
    // A server producing garbage produces a lot of it. The same sentence four
    // hundred times is not more informative than the same sentence once, and
    // it would bury the answer it is about.
    let stream = "data: {not json}\n".repeat(20) + "data: [DONE]\n";
    let p = provider(cloud_endpoint(), &cloud_dns(), Arc::new(Stub::ok(&stream)));
    let mut notices = 0;
    run(&p, &envelope(), &Cancel::new(), &mut |e| {
        if matches!(e, StreamEvent::Notice(_)) {
            notices += 1;
        }
    })
    .expect("generate");
    assert_eq!(notices, 1);
}

#[test]
fn an_answer_the_provider_stopped_filtering_is_not_reported_as_a_clean_finish() {
    // `content_filter` used to fall into the `_ =>` arm and arrive as
    // `StopReason::Stop` — an answer that was cut short presented as one the
    // model chose to end. There is no third `StopReason` to add without
    // changing what every consumer renders, so the truth goes on the stream.
    let stream = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"The clause says\"}}]}\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"content_filter\"}]}\n",
        "data: [DONE]\n",
    );
    let p = provider(cloud_endpoint(), &cloud_dns(), Arc::new(Stub::ok(stream)));
    let mut seen = Vec::new();
    let c = run(&p, &envelope(), &Cancel::new(), &mut |e| seen.push(e)).expect("generate");

    let notice = seen
        .iter()
        .find_map(|e| match e {
            StreamEvent::Notice(n) => Some(n),
            _ => None,
        })
        .expect("a filtered answer must say so");
    assert!(notice.message.contains("content_filter"), "{notice:?}");
    assert!(notice.message.contains("incomplete"), "{notice:?}");
    assert_eq!(c.text, "The clause says");
}

#[test]
fn a_failure_after_the_text_has_streamed_keeps_the_text_and_announces_no_finish() {
    // The third thing the old shape could not express. The provider fails
    // after emitting real words: those words already reached the consumer
    // through the stream and they stand, but nothing announces this as a
    // finished generation, because it was not one. The error is still the
    // `Result`, so no caller can forget to look at it.
    let stream = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"It renews on 31 Dec\"}}]}\n",
        "data: {\"error\":{\"message\":\"upstream capacity\",\"type\":\"server_error\"}}\n",
    );
    let p = provider(cloud_endpoint(), &cloud_dns(), Arc::new(Stub::ok(stream)));
    let mut seen = Vec::new();
    let e = run(&p, &envelope(), &Cancel::new(), &mut |ev| seen.push(ev))
        .expect_err("the provider failed");

    assert!(
        seen.iter().any(|ev| matches!(ev, StreamEvent::Text { .. })),
        "what streamed before the failure is not taken back"
    );
    assert!(
        !seen.iter().any(|ev| matches!(ev, StreamEvent::Finish(_))),
        "a failed generation must not announce a finish: {seen:?}"
    );
    assert!(!e.message().is_empty());
}

#[test]
fn a_stream_that_ends_without_done_is_not_a_failure() {
    let stream = "data: {\"choices\":[{\"delta\":{\"content\":\"abrupt\"}}]}\n";
    let p = provider(cloud_endpoint(), &cloud_dns(), Arc::new(Stub::ok(stream)));
    let c = run(&p, &envelope(), &Cancel::new(), &mut |_| {}).expect("generate");
    assert_eq!(c.text, "abrupt");
    assert_eq!(
        c.usage.output_tokens, 1,
        "no usage frame arrived, so the count is the one we made"
    );
}

// ── cancellation ───────────────────────────────────────────────────────────

#[test]
fn a_cancelled_generation_returns_what_streamed_and_stops_reading() {
    // Two halves, and the second is the one that matters for a paid provider:
    // draining the rest of the stream would keep it generating for an answer
    // nobody is waiting for.
    let long: String = (0..200)
        .map(|i| format!("data: {{\"choices\":[{{\"delta\":{{\"content\":\"tok{i} \"}}}}]}}\n"))
        .collect::<String>()
        + "data: [DONE]\n";
    let http = Arc::new(Stub::ok(&long));
    let consumed = Arc::clone(&http.consumed);
    let p = provider(cloud_endpoint(), &cloud_dns(), http.clone());

    let cancel = Cancel::new();
    let mut seen = 0;
    let c = run(&p, &envelope(), &cancel, &mut |_| {
        seen += 1;
        if seen == 3 {
            cancel.cancel();
        }
    })
    .expect("a cancel is not an error");

    assert_eq!(c.stop_reason, StopReason::Cancelled);
    assert!(c.text.starts_with("tok0 tok1 tok2"), "{}", c.text);
    assert_eq!(
        c.usage.output_tokens, 3,
        "what streamed is counted; zero under visible words is a wrong number"
    );
    assert!(
        consumed.load(Ordering::SeqCst) < long.len(),
        "the rest of the stream was drained instead of abandoned"
    );
}

#[test]
fn a_generation_cancelled_before_it_starts_never_reaches_the_wire() {
    let http = Arc::new(Stub::ok(STREAM));
    let p = provider(cloud_endpoint(), &cloud_dns(), http.clone());
    let cancel = Cancel::new();
    cancel.cancel();
    let c = run(&p, &envelope(), &cancel, &mut |_| {}).expect("not an error");
    assert_eq!(c.stop_reason, StopReason::Cancelled);
    assert!(
        http.seen.lock().expect("lock").is_empty(),
        "a cancelled request must not be sent at all"
    );
}

// ── failure, named ─────────────────────────────────────────────────────────

fn failure(status: u16, body: &str) -> marrow_core::Error {
    let p = provider(
        cloud_endpoint(),
        &cloud_dns(),
        Arc::new(Stub::new(status, body.as_bytes().to_vec())),
    );
    run(&p, &envelope(), &Cancel::new(), &mut |_| {}).expect_err("must fail")
}

#[test]
fn a_wrong_key_says_it_is_the_key() {
    let e = failure(
        401,
        r#"{"error":{"message":"Incorrect API key provided","code":"invalid_api_key"}}"#,
    );
    assert_eq!(e.code(), marrow_core::Code::NetBadStatus);
    assert!(e.message().contains("key"), "{}", e.message());
    assert!(
        !e.code().retryable(),
        "asking again with the same key is a loop"
    );
    assert!(
        !e.message().contains(KEY),
        "the message must not quote the key back"
    );
}

#[test]
fn a_rate_limit_is_retryable_and_a_wrong_key_is_not() {
    // The distinction is the whole reason NET_RATE_LIMITED exists: one of
    // these succeeds if you wait, and the other never does.
    let e = failure(429, r#"{"error":{"message":"Rate limit reached"}}"#);
    assert_eq!(e.code(), marrow_core::Code::NetRateLimited);
    assert!(e.code().retryable());
    assert!(e.message().contains("Wait"), "{}", e.message());
}

#[test]
fn a_model_the_endpoint_does_not_have_names_the_model_and_not_the_network() {
    let e = failure(
        404,
        r#"{"error":{"message":"The model `gpt-test` does not exist","code":"model_not_found"}}"#,
    );
    assert_eq!(e.code(), marrow_core::Code::ModNotInstalled);
    assert!(e.message().contains("gpt-test"), "{}", e.message());
}

#[test]
fn a_wrong_base_url_says_so_rather_than_blaming_the_model() {
    // The commonest configuration mistake by a distance: the address without
    // its `/v1`.
    let e = failure(404, "<html><body>Not Found</body></html>");
    assert_eq!(e.code(), marrow_core::Code::NetBadStatus);
    assert!(e.message().contains("/v1"), "{}", e.message());
}

#[test]
fn a_failure_delivered_inside_the_stream_is_the_same_failure() {
    // Several gateways answer 200 and then put the error in the first frame.
    let stream =
        "data: {\"error\":{\"message\":\"Rate limit exceeded\",\"code\":\"rate_limit_exceeded\"}}\n";
    let p = provider(cloud_endpoint(), &cloud_dns(), Arc::new(Stub::ok(stream)));
    let e = run(&p, &envelope(), &Cancel::new(), &mut |_| {}).expect_err("must fail");
    assert_eq!(e.code(), marrow_core::Code::NetRateLimited);
}

#[test]
fn a_host_that_does_not_resolve_is_named_before_anything_is_sent() {
    let e = OpenAiProvider::connect(
        cloud_endpoint(),
        "Test",
        Arc::new(MemorySecrets::new()),
        Arc::new(Stub::ok(STREAM)),
        &Dns::default(),
    )
    .expect_err("must fail");
    assert_eq!(e.code(), marrow_core::Code::NetUnreachable);
    assert!(e.message().contains("api.example.com"), "{}", e.message());
}

#[test]
fn the_provider_error_is_quoted_because_it_is_the_specific_half() {
    let e = failure(400, r#"{"error":{"message":"context length exceeded"}}"#);
    assert!(
        e.message().contains("context length exceeded"),
        "{}",
        e.message()
    );
}

// ── the boundary ───────────────────────────────────────────────────────────

#[test]
fn the_resolved_address_decides_the_boundary_and_not_the_hostname() {
    // The attack Part 9 §153.2 is written about, pointed the other way: here a
    // name that *looks* local would earn the words "on your own server" for a
    // request that leaves the building.
    let local = provider(
        Endpoint::new("http://localhost:1234/v1", "m"),
        &Dns::with(&[("localhost", "127.0.0.1")]),
        Arc::new(Stub::ok(STREAM)),
    );
    assert_eq!(local.boundary(), Boundary::Private);

    let lying = OpenAiProvider::connect(
        Endpoint::new("https://localhost.example.com/v1", "m"),
        "Test",
        Arc::new(MemorySecrets::with("cloud-provider", KEY)),
        Arc::new(Stub::ok(STREAM)),
        &Dns::with(&[("localhost.example.com", "93.184.216.34")]),
    )
    .expect("connect");
    assert_eq!(
        lying.boundary(),
        Boundary::Cloud,
        "a hostname that reads as local must not earn a local label"
    );
}

#[test]
fn every_notation_of_this_machine_is_the_same_machine() {
    // NET-009's rule, reused: `::ffff:127.0.0.1` and NAT64 are 127.0.0.1
    // written differently, and three notations must not be three chances to
    // get the label wrong.
    for ip in ["127.0.0.1", "::1", "::ffff:127.0.0.1", "64:ff9b::7f00:1"] {
        assert_eq!(
            boundary_of(&[ip.parse().expect("addr")]),
            Boundary::Private,
            "{ip}"
        );
    }
    for ip in [
        "10.0.0.5",
        "192.168.1.10",
        "100.64.0.1",
        "fd00::1",
        "fe80::1",
    ] {
        assert_eq!(
            boundary_of(&[ip.parse().expect("addr")]),
            Boundary::Private,
            "{ip} is a network the user is on"
        );
    }
}

#[test]
fn anything_not_certainly_the_users_own_is_cloud() {
    // Default deny, applied to the *claim* rather than to the connection:
    // over-warning costs a word on screen and under-warning costs the user's
    // documents.
    assert_eq!(
        boundary_of(&["93.184.216.34".parse().unwrap()]),
        Boundary::Cloud
    );
    assert_eq!(boundary_of(&[]), Boundary::Cloud);
    assert_eq!(
        boundary_of(&[
            "127.0.0.1".parse().unwrap(),
            "93.184.216.34".parse().unwrap()
        ]),
        Boundary::Cloud,
        "a mixture cannot be described in one word, so it gets the strict one"
    );
    assert_eq!(
        boundary_of(&["224.0.0.1".parse().unwrap()]),
        Boundary::Cloud,
        "a class nobody thought about is not private by accident"
    );
}

#[test]
fn plain_http_to_somewhere_that_is_not_yours_is_refused() {
    // The question and every excerpt would cross the wire in clear text, and
    // whoever is on the path would choose what comes back.
    let e = OpenAiProvider::connect(
        Endpoint::new("http://api.example.com/v1", "m"),
        "Test",
        Arc::new(MemorySecrets::new()),
        Arc::new(Stub::ok(STREAM)),
        &cloud_dns(),
    )
    .expect_err("must refuse");
    assert_eq!(e.code(), marrow_core::Code::CfgInvalid);
    assert!(e.message().contains("clear text"), "{}", e.message());
}

#[test]
fn an_address_that_cannot_be_used_is_refused_with_the_reason() {
    for (url, needle) in [
        ("ftp://example.com/v1", "not a scheme"),
        ("example.com/v1", "no scheme"),
        ("https://user:pass@example.com/v1", "credentials"),
        ("https://exámple.com/v1", "non-ASCII"),
    ] {
        let e = Target::parse(url).expect_err(url);
        assert_eq!(e.code(), marrow_core::Code::CfgInvalid, "{url}");
        assert!(e.message().contains(needle), "{url}: {}", e.message());
    }
}

#[test]
fn a_port_is_taken_from_the_address_and_defaulted_from_the_scheme() {
    assert_eq!(
        Target::parse("https://api.example.com/v1").expect("parse"),
        Target {
            tls: true,
            host: "api.example.com".into(),
            port: 443
        }
    );
    assert_eq!(
        Target::parse("http://localhost:1234/v1")
            .expect("parse")
            .port,
        1234
    );
    assert_eq!(
        Target::parse("http://[::1]:11434/v1").expect("parse"),
        Target {
            tls: false,
            host: "::1".into(),
            port: 11434
        }
    );
}

#[test]
fn the_completions_path_is_appended_once_however_the_base_was_typed() {
    for base in ["https://a.example/v1", "https://a.example/v1/"] {
        assert_eq!(
            Endpoint::new(base, "m").completions_url(),
            "https://a.example/v1/chat/completions"
        );
    }
}

// ── the key, and where it must never appear ────────────────────────────────

#[test]
fn the_configuration_that_is_written_to_disk_has_no_room_for_a_key() {
    // LLM-030 as a property of the type: `Endpoint` is what
    // `preferences.json` holds, and it names the keyring account rather than
    // the secret. There is no field to put a key in by accident.
    let json = serde_json::to_string(&cloud_endpoint()).expect("serialise");
    assert!(!json.contains(KEY));
    assert!(json.contains("keyAccount"));
    for forbidden in ["\"apiKey\"", "\"key\"", "\"secret\"", "\"token\""] {
        assert!(!json.contains(forbidden), "{json}");
    }
}

#[test]
fn nothing_a_provider_prints_contains_the_key() {
    let p = provider(cloud_endpoint(), &cloud_dns(), Arc::new(Stub::ok(STREAM)));
    assert!(!format!("{p:?}").contains(KEY), "Debug leaked the key");
    assert!(!p.describe().contains(KEY));
    let req = ChatRequest {
        url: "https://api.example.com/v1/chat/completions",
        addrs: &[],
        api_key: Some(&Secret::new(KEY)),
        body: "the user's documents",
    };
    let printed = format!("{req:?}");
    assert!(!printed.contains(KEY), "{printed}");
    assert!(
        !printed.contains("the user's documents"),
        "the body is the user's files and must not be printable either: {printed}"
    );
}

#[test]
fn nothing_the_provider_logs_contains_the_key_or_the_prompt() {
    // The rule that actually gets broken is not `Debug` — it is a `tracing`
    // line added later that seemed harmless. Logs get pasted into bug reports
    // (NET-051), so this captures real subscriber output for a whole
    // generation, success and failure, and asserts on it.
    //
    // The subscriber is installed **globally** rather than for this thread.
    // A thread-local one passed on its own and failed in a full run:
    // `tracing` caches per callsite whether anything is listening, another
    // test reaches this module's `debug!` first with no subscriber at all,
    // and the callsite is cached as "never" for the rest of the process.
    // `set_global_default` rebuilds that cache, which is the only version of
    // this that is not order-dependent. It means other tests' output lands in
    // the sink too — which makes the assertion wider rather than weaker.
    use std::sync::{Arc as StdArc, Mutex as StdMutex};
    use tracing_subscriber::fmt::MakeWriter;

    #[derive(Clone, Default)]
    struct Sink(StdArc<StdMutex<Vec<u8>>>);
    impl std::io::Write for Sink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("lock").extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl<'a> MakeWriter<'a> for Sink {
        type Writer = Sink;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    let sink = Sink::default();
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_writer(sink.clone())
            .finish(),
    )
    .expect("this is the only test in the crate that installs one");

    let env = envelope();
    let p = provider(cloud_endpoint(), &cloud_dns(), Arc::new(Stub::ok(STREAM)));
    run(&p, &env, &Cancel::new(), &mut |_| {}).expect("generate");
    // And the failure path, which is where a "helpful" dump of the request
    // would be added.
    let bad = provider(
        cloud_endpoint(),
        &cloud_dns(),
        Arc::new(Stub::new(401, b"nope".to_vec())),
    );
    let _ = run(&bad, &env, &Cancel::new(), &mut |_| {});

    let logged = String::from_utf8_lossy(&sink.0.lock().expect("lock")).into_owned();
    assert!(
        logged.contains("sending a chat completion"),
        "the provider's own log line was not captured, so this would pass \
         vacuously: {logged}"
    );
    assert!(!logged.contains(KEY), "the key reached a log: {logged}");
    assert!(
        !logged.contains("31 December 2026"),
        "an excerpt from the user's files reached a log: {logged}"
    );
}

// ── the real transport, against a local socket ─────────────────────────────

#[test]
fn the_real_client_speaks_to_a_real_server_and_streams_what_it_says() {
    // Everything above this line is proved against a fake, and a fake contains
    // what its author thought of (Part 9 §161.2). This one uses the real
    // `ureq` agent, the real pinned resolver and the real SSE reader against a
    // socket on this machine. It spends nothing and needs no network.
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let server = std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().expect("accept");
        // Read the request head so the client is not writing into a closed
        // socket, and hand back what it sent for the assertions below.
        let mut head = String::new();
        let mut reader = BufReader::new(sock.try_clone().expect("clone"));
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).expect("read") == 0 || line == "\r\n" {
                break;
            }
            head.push_str(&line);
        }
        let len: usize = head
            .lines()
            .find_map(|l| {
                l.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(|v| v.trim().parse().expect("len"))
            })
            .expect("a content-length");
        let mut body = vec![0u8; len];
        std::io::Read::read_exact(&mut reader, &mut body).expect("body");

        let payload = STREAM.as_bytes();
        sock.write_all(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                payload.len()
            )
            .as_bytes(),
        )
        .expect("head");
        sock.write_all(payload).expect("body");
        sock.flush().expect("flush");
        (head, String::from_utf8_lossy(&body).into_owned())
    });

    let p = OpenAiProvider::connect(
        Endpoint::new(format!("http://127.0.0.1:{port}/v1"), "local-model"),
        "A server on this machine",
        Arc::new(MemorySecrets::with("cloud-provider", KEY)),
        Arc::new(Https),
        &SystemDns,
    )
    .expect("connect");
    assert_eq!(p.boundary(), Boundary::Private);
    assert_eq!(p.addresses(), vec!["127.0.0.1".to_string()]);

    let env = envelope();
    let mut streamed = String::new();
    let c = p
        .generate(
            GenerateRequest {
                model_id: "local-model",
                envelope: &env,
                reasoning: Reasoning::Off,
                max_output_tokens: 64,
                cancel: &Cancel::new(),
            },
            &mut |t| {
                if let StreamEvent::Text { text: s } = t {
                    streamed.push_str(&s);
                }
            },
        )
        .expect("generate");

    let (head, body) = server.join().expect("server thread");
    assert!(head.starts_with("POST /v1/chat/completions "), "{head}");
    assert!(
        head.to_ascii_lowercase().contains("authorization: bearer "),
        "{head}"
    );
    let sent: serde_json::Value = serde_json::from_str(&body).expect("json");
    assert_eq!(sent["messages"][0]["content"], env.text);
    assert_eq!(streamed, "It renews on 31 December 2026 [E1].");
    assert_eq!(c.stop_reason, StopReason::Stop);
    assert_eq!(c.model_id, "local-model");
}
