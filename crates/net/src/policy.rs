//! The egress policy (Part 9 §153–§160).
//!
//! **Nothing in this module performs I/O.** That is the point of it: every rule
//! Part 9 states is decidable from a URL, a list of addresses and a count, so
//! every rule is unit-testable with no DNS and no network (NET-060). A policy
//! that can only be tested against the internet is a policy that gets tested
//! rarely and changed carelessly.
//!
//! Three shapes of answer, and the difference between them is load-bearing:
//!
//! - [`Decision::Allow`] — go.
//! - [`Decision::Confirm`] — a human must say yes, now, to this exact URL.
//! - [`Decision::Refuse`] — no, and there is no override. Refusals here map to
//!   `POL_DENIED`, which [`marrow_core::Code`] guarantees is never retryable.
//!   A denial a retry can defeat is not a policy.

use std::collections::BTreeSet;
use std::net::IpAddr;
use std::time::Duration;

use marrow_core::{Code, Error};
use serde::Serialize;

use crate::addr::{classify, AddressClass};
use crate::url::{Url, UrlError};

/// NET-032. Measured after decompression, because a cap on the compressed
/// stream is what a decompression bomb is for.
pub const MAX_BODY_BYTES: u64 = 2 * 1024 * 1024;

/// NET-034, for the whole fetch including every redirect and the body.
pub const MAX_ELAPSED: Duration = Duration::from_secs(10);

/// NET-015.
pub const MAX_REDIRECTS: u8 = 5;

/// NET-055. Per user turn, and the model asking again does not refill it.
pub const MAX_FETCHES_PER_TURN: u16 = 8;

/// NET-031. Honest, because spoofing a browser is a lie told to the operator
/// whose bandwidth is being spent.
pub const USER_AGENT: &str = "Marrow/0.1 (local knowledge runtime)";

/// NET-035. Nothing else is read; a binary is a download, not a fetch.
pub const ACCEPT: &str = "text/html, text/plain, application/xhtml+xml, application/json";

/// The only scheme (NET-001) and the only port (NET-002).
const SCHEME: &str = "https";
const PORT: u16 = 443;

/// Why a fetch did not happen.
///
/// Every variant carries the numbers that caused it, because §142.3's rule
/// applies here too: a refusal names the number. "Fetch failed" is a defect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Not a URL at all.
    Malformed(UrlError),
    /// NET-001.
    Scheme { scheme: String },
    /// NET-002.
    Port { port: u16 },
    /// NET-003.
    Credentials,
    /// NET-012. The name exists as far as the caller is concerned, but nothing
    /// answered for it.
    HostNotResolved { host: String },
    /// NET-008. The single most important refusal in the crate.
    Address {
        host: String,
        addr: IpAddr,
        class: AddressClass,
    },
    /// NET-010. Some addresses were public and some were not.
    MixedAddresses {
        host: String,
        addr: IpAddr,
        class: AddressClass,
        public: usize,
    },
    /// NET-015.
    TooManyRedirects { limit: u8 },
    /// NET-016.
    BadRedirect { status: u16, detail: String },
    /// NET-014. A redirect crossed to a host nobody confirmed.
    RedirectToUnconfirmedHost { from: String, to: String },
    /// NET-035.
    ContentType { got: String },
    /// NET-036.
    Status { status: u16 },
    /// NET-034.
    TimedOut { limit: Duration },
    /// NET-055.
    BudgetExhausted { used: u16, limit: u16 },
    /// NET-057.
    Repeat { url: String },
    /// The caller called `fetch` without having obtained consent first. Not a
    /// policy failure so much as a caller bug, but it must never be a silent
    /// upgrade to "allowed".
    NotConfirmed { url: String, why: ConfirmReason },
    /// The connection itself failed. The only variant here that is not a
    /// decision Marrow made.
    Unreachable { host: String, detail: String },
}

impl Refusal {
    /// The §108 code.
    ///
    /// Everything Marrow *decided* is `POL_DENIED` and therefore not retryable.
    /// Only the things the world decided — no answer, no route, no time — get a
    /// retryable code.
    pub fn code(&self) -> Code {
        match self {
            Refusal::NotConfirmed { .. } => Code::PolApprovalRequired,
            // The world decided these, not Marrow, so they are retryable and
            // they carry their own class: a host that will not answer and a
            // volume that will not mount are different problems, and a log
            // that conflates them makes "why did this not work" harder.
            Refusal::HostNotResolved { .. } | Refusal::Unreachable { .. } => Code::NetUnreachable,
            Refusal::TimedOut { .. } => Code::NetTimeout,
            // The server answered, and not with success. The same request gets
            // the same answer, so this one is not retryable.
            Refusal::Status { .. } => Code::NetBadStatus,
            _ => Code::PolDenied,
        }
    }

    /// Cause and action. These are read by a person deciding what to do next.
    pub fn message(&self) -> String {
        match self {
            Refusal::Malformed(e) => e.message().to_string(),
            Refusal::Scheme { scheme } => format!(
                "Marrow fetches https only, and that URL is {scheme}. Plain http would put the \
                 whole URL — including anything you asked about — on the wire in clear text, and \
                 would let anyone on the path choose what the model reads back. Supply an https \
                 URL."
            ),
            Refusal::Port { port } => format!(
                "Marrow fetches port 443 only, and that URL asks for port {port}. A request to any \
                 other port is a service probe rather than a web page."
            ),
            Refusal::Credentials => "That URL carries a username and password in its address. \
                 Marrow never sends credentials, so it will not fetch a URL that contains them."
                .to_string(),
            Refusal::HostNotResolved { host } => format!(
                "{host} did not resolve to any address. Check the spelling and the network."
            ),
            Refusal::Address { host, addr, class } => format!(
                "{host} resolves to {addr}, which is {}. Marrow refuses it: a fetch tool pointed \
                 inside your own network is a scanner running from inside it. This is not \
                 overridable.",
                class.why()
            ),
            Refusal::MixedAddresses {
                host,
                addr,
                class,
                public,
            } => format!(
                "{host} resolves to {public} public address(es) and also to {addr}, which is {}. \
                 A split answer like that is how DNS rebinding looks, so the whole fetch is \
                 refused rather than the public half being used.",
                class.why()
            ),
            Refusal::TooManyRedirects { limit } => format!(
                "That URL redirected more than {limit} times. Fetch the destination directly."
            ),
            Refusal::BadRedirect { status, detail } => format!(
                "The server answered {status} but its redirect could not be followed: {detail}. \
                 Marrow will not guess at a destination."
            ),
            Refusal::RedirectToUnconfirmedHost { from, to } => format!(
                "{from} redirected to {to}, which you have not confirmed. Confirming a host is \
                 not confirming wherever it decides to send the request. Fetch {to} directly if \
                 you want it."
            ),
            Refusal::ContentType { got } => format!(
                "That URL returned {got}, and Marrow's fetch tool reads text only. Nothing was \
                 downloaded."
            ),
            Refusal::Status { status } => format!(
                "The server answered {status}. There is no page there to read, and an error \
                 page's body is not evidence about anything."
            ),
            Refusal::TimedOut { limit } => format!(
                "The fetch did not finish within {} seconds and was stopped. Try again, or fetch \
                 a smaller page.",
                limit.as_secs()
            ),
            Refusal::BudgetExhausted { used, limit } => format!(
                "This turn has already used its {used} of {limit} fetches. Ask a new question to \
                 get a new budget — a ceiling that refills on request is not a ceiling."
            ),
            Refusal::Repeat { url } => format!(
                "{url} was already fetched during this turn. Use the result you already have \
                 rather than fetching it again."
            ),
            Refusal::NotConfirmed { url, why } => format!(
                "{url} has not been confirmed: {}. Ask, then fetch.",
                why.explain()
            ),
            Refusal::Unreachable { host, detail } => {
                format!("Could not reach {host}: {detail}. Check the network and try again.")
            }
        }
    }
}

impl From<Refusal> for Error {
    fn from(r: Refusal) -> Error {
        Error::new(r.code(), r.message()).with_context(format!("{r:?}"))
    }
}

impl From<UrlError> for Refusal {
    fn from(e: UrlError) -> Refusal {
        Refusal::Malformed(e)
    }
}

/// Why a human has to be asked.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "reason")]
pub enum ConfirmReason {
    /// NET-018. First fetch from this host this session.
    NewHost { host: String },
    /// NET-019. The query string is the payload, and a session-wide yes to a
    /// host was never a yes to arbitrary future payloads sent to it.
    CarriesAQuery { host: String, query: String },
}

impl ConfirmReason {
    pub fn explain(&self) -> String {
        match self {
            ConfirmReason::NewHost { host } => {
                format!("this session has not fetched from {host} before")
            }
            ConfirmReason::CarriesAQuery { host, query } => {
                format!("it sends `{query}` to {host}, and that text leaves this device")
            }
        }
    }
}

/// What the policy says about one URL.
#[derive(Clone, Debug, PartialEq)]
pub enum Decision {
    Allow,
    /// Ask, showing this URL and this reason. Answering yes means calling
    /// [`Consent::confirm_host`] or [`Consent::confirm_once`] and then fetching.
    Confirm {
        url: String,
        why: ConfirmReason,
    },
    /// No. Not overridable, not configurable, no `--force`.
    Refuse(Refusal),
}

impl Decision {
    pub fn allowed(&self) -> bool {
        matches!(self, Decision::Allow)
    }
}

/// Everything that has a number in it. Constructed by [`Policy::default`]; the
/// fields exist so tests can shrink a cap rather than generate 2 MB.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Policy {
    pub max_body_bytes: u64,
    pub max_elapsed: Duration,
    pub max_redirects: u8,
    pub max_fetches_per_turn: u16,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            max_body_bytes: MAX_BODY_BYTES,
            max_elapsed: MAX_ELAPSED,
            max_redirects: MAX_REDIRECTS,
            max_fetches_per_turn: MAX_FETCHES_PER_TURN,
        }
    }
}

impl Policy {
    /// §153.1 — everything decidable from the URL alone.
    ///
    /// Separate from [`Policy::decide`] because a redirect hop re-runs *this*
    /// without re-running the consent logic in a different order (NET-013).
    pub fn destination(&self, url: &Url) -> Result<(), Refusal> {
        if url.scheme() != SCHEME {
            return Err(Refusal::Scheme {
                scheme: url.scheme().to_string(),
            });
        }
        if url.userinfo().is_some() {
            return Err(Refusal::Credentials);
        }
        if url.port() != PORT {
            return Err(Refusal::Port { port: url.port() });
        }
        Ok(())
    }

    /// §153.2 — the rule that does the actual work.
    ///
    /// Takes every address the host resolved to, not one of them: a host that
    /// answers with a public address *and* `10.0.0.5` is refused outright
    /// (NET-010), because using the public half would be treating a rebinding
    /// attempt as a fallback.
    pub fn addresses(&self, host: &str, addrs: &[IpAddr]) -> Result<(), Refusal> {
        if addrs.is_empty() {
            return Err(Refusal::HostNotResolved {
                host: host.to_string(),
            });
        }
        let public = addrs.iter().filter(|a| classify(**a).is_public()).count();
        for addr in addrs {
            let class = classify(*addr);
            if class.is_public() {
                continue;
            }
            return Err(if public > 0 {
                Refusal::MixedAddresses {
                    host: host.to_string(),
                    addr: *addr,
                    class,
                    public,
                }
            } else {
                Refusal::Address {
                    host: host.to_string(),
                    addr: *addr,
                    class,
                }
            });
        }
        Ok(())
    }

    /// NET-035, checked before a byte of body is read.
    pub fn content_type(&self, header: Option<&str>) -> Result<(), Refusal> {
        // A missing content-type is treated as text: servers omit it for plain
        // files, and the byte cap plus the extractor already bound the damage.
        let Some(raw) = header else { return Ok(()) };
        let mime = raw
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        let ok = mime.starts_with("text/")
            || matches!(
                mime.as_str(),
                "application/xhtml+xml" | "application/json" | "application/xml"
            )
            || mime.ends_with("+json")
            || mime.ends_with("+xml");
        if ok {
            Ok(())
        } else {
            Err(Refusal::ContentType { got: mime })
        }
    }

    /// Everything, for the first hop of a fetch: destination, budget, consent.
    ///
    /// This is the **pre-flight** call: it is what a caller asks before
    /// prompting, against a turn that has not yet been charged for this
    /// attempt. [`Policy::decide_charged`] is the same question asked after
    /// [`Turn::spend`] has already run.
    ///
    /// Order is deliberate. The **refusals come first**, so a URL that will
    /// never be fetched does not produce a confirmation dialog — teaching
    /// someone to click through a prompt for a request that was going to be
    /// refused anyway is how prompts stop being read.
    pub fn decide(&self, url: &Url, consent: &Consent, turn: &Turn) -> Decision {
        if let Err(r) = self.destination(url) {
            return Decision::Refuse(r);
        }
        if turn.remaining(self) == 0 {
            return Decision::Refuse(Refusal::BudgetExhausted {
                used: turn.used(),
                limit: self.max_fetches_per_turn,
            });
        }
        self.decide_charged(url, consent, turn)
    }

    /// [`Policy::decide`] minus the budget branch.
    ///
    /// Exists because NET-056 charges the attempt *before* it is judged, so by
    /// the time the decision is made the budget has already been checked and
    /// spent. Re-checking it here would refuse the eighth of eight fetches —
    /// a ceiling of 8 that grants 7 is a bug, and it is the kind that only
    /// shows up at the boundary.
    pub fn decide_charged(&self, url: &Url, consent: &Consent, turn: &Turn) -> Decision {
        if let Err(r) = self.destination(url) {
            return Decision::Refuse(r);
        }
        if turn.already_fetched(&url.wire()) {
            return Decision::Refuse(Refusal::Repeat { url: url.wire() });
        }
        match self.consent(url, consent) {
            Some(why) => Decision::Confirm {
                url: url.to_string(),
                why,
            },
            None => Decision::Allow,
        }
    }

    /// §154. `None` means consent is already in hand.
    fn consent(&self, url: &Url, consent: &Consent) -> Option<ConfirmReason> {
        // A query string is content, so it is confirmed every single time and
        // a session-wide host confirmation does not cover it (NET-019).
        if let Some(q) = url.query() {
            if consent.has_once(&url.wire()) {
                return None;
            }
            return Some(ConfirmReason::CarriesAQuery {
                host: url.host().to_string(),
                query: decode_query(q),
            });
        }
        if consent.is_confirmed(url.host()) || consent.has_once(&url.wire()) {
            return None;
        }
        Some(ConfirmReason::NewHost {
            host: url.host().to_string(),
        })
    }
}

/// Percent-decode a query for display (NET-050).
///
/// Display only — the encoded form is what is sent. Reviewing `%20` is not
/// reviewing, and the user is being asked to approve the *meaning* of the text
/// that leaves, not its encoding.
pub fn decode_query(q: &str) -> String {
    let bytes = q.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(b) => {
                        out.push(b);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    // A decoded query with a control character in it would let the displayed
    // string lie about its own length. Replace rather than drop, so the user
    // can see that something was there.
    String::from_utf8_lossy(&out)
        .chars()
        .map(|c| if c.is_control() { '␀' } else { c })
        .collect()
}

/// What the user has said yes to, for this session and no longer (NET-021).
///
/// There is no persisted allow-list. A file of hosts approved months ago, still
/// approving them today, is exactly the artefact Part 9 exists to avoid.
#[derive(Clone, Debug, Default)]
pub struct Consent {
    /// Confirmed for the session. Exact hosts — no wildcards, no subdomains,
    /// no eTLD+1 (NET-018).
    hosts: BTreeSet<String>,
    /// Confirmed for exactly one fetch, and consumed by it (NET-020).
    once: BTreeSet<String>,
}

impl Consent {
    pub fn new() -> Self {
        Self::default()
    }

    /// NET-018. The host is lowercased; nothing else about it is interpreted.
    pub fn confirm_host(&mut self, host: &str) {
        self.hosts.insert(host.trim().to_ascii_lowercase());
    }

    /// NET-020. One fetch of exactly this URL. Consumed when it is used, so
    /// "every time" is a property of the type rather than a rule to remember.
    pub fn confirm_once(&mut self, url: &str) {
        self.once.insert(url.trim().to_string());
    }

    pub fn is_confirmed(&self, host: &str) -> bool {
        self.hosts.contains(&host.to_ascii_lowercase())
    }

    pub fn has_once(&self, url: &str) -> bool {
        self.once.contains(url)
    }

    /// Spend a one-shot confirmation. Returns whether there was one.
    pub fn take_once(&mut self, url: &str) -> bool {
        self.once.remove(url)
    }

    /// Everything the user has agreed to, for the disclosure surface.
    pub fn confirmed_hosts(&self) -> impl Iterator<Item = &str> {
        self.hosts.iter().map(String::as_str)
    }

    pub fn forget(&mut self) {
        self.hosts.clear();
        self.once.clear();
    }
}

/// One user turn's fetch budget (§160).
///
/// The budget belongs to the turn, not to the model, the session or the
/// process. A new question is a new `Turn`; a model that has spent its eight
/// does not get more by trying harder.
#[derive(Clone, Debug, Default)]
pub struct Turn {
    used: u16,
    fetched: BTreeSet<String>,
}

impl Turn {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn used(&self) -> u16 {
        self.used
    }

    pub fn remaining(&self, policy: &Policy) -> u16 {
        policy.max_fetches_per_turn.saturating_sub(self.used)
    }

    /// NET-058. What the UI shows before the budget runs out.
    pub fn budget_line(&self, policy: &Policy) -> String {
        format!(
            "{} of {} fetches used",
            self.used, policy.max_fetches_per_turn
        )
    }

    /// NET-056. Spend one attempt, whatever its outcome turns out to be.
    ///
    /// Called *before* the decision, so a refused fetch costs the same as a
    /// successful one. Otherwise refusals are free and a model probes until
    /// something works.
    pub fn spend(&mut self, policy: &Policy) -> Result<(), Refusal> {
        if self.used >= policy.max_fetches_per_turn {
            return Err(Refusal::BudgetExhausted {
                used: self.used,
                limit: policy.max_fetches_per_turn,
            });
        }
        self.used += 1;
        Ok(())
    }

    pub fn already_fetched(&self, url: &str) -> bool {
        self.fetched.contains(url)
    }

    /// NET-057.
    pub fn record(&mut self, url: &str) {
        self.fetched.insert(url.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s).expect("test URL must parse")
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().expect("test address must parse")
    }

    #[test]
    fn a_scheme_that_is_not_https_is_refused() {
        // NET-001. Plain http would put the URL — which may carry the user's
        // question — on the wire in clear text, and let anyone on the path
        // choose what the model reads back.
        let p = Policy::default();
        for s in [
            "http://example.com/",
            "ftp://example.com/",
            "gopher://example.com/",
            "ws://example.com/",
        ] {
            let e = p.destination(&url(s)).unwrap_err();
            assert!(matches!(e, Refusal::Scheme { .. }), "{s} -> {e:?}");
            assert_eq!(e.code(), Code::PolDenied);
            assert!(!e.code().retryable(), "a denial a retry defeats is not one");
        }
        assert!(p.destination(&url("https://example.com/")).is_ok());
    }

    #[test]
    fn a_port_that_is_not_443_is_refused() {
        // NET-002. A fetch tool reads web pages; any other port is a service
        // probe, and the model has no business choosing one.
        let p = Policy::default();
        for (s, port) in [
            ("https://example.com:8080/", 8080u16),
            ("https://example.com:22/", 22),
            ("https://example.com:9200/", 9200),
        ] {
            assert_eq!(p.destination(&url(s)).unwrap_err(), Refusal::Port { port });
        }
        assert!(p.destination(&url("https://example.com:443/")).is_ok());
    }

    #[test]
    fn a_url_carrying_credentials_is_refused() {
        // NET-003. The only two things that construct one are a mistake and an
        // attack.
        let p = Policy::default();
        assert_eq!(
            p.destination(&url("https://alice:secret@example.com/"))
                .unwrap_err(),
            Refusal::Credentials
        );
    }

    #[test]
    fn a_hostname_that_resolves_to_loopback_is_refused() {
        // The whole attack: `localtest.me` is a real, public, resolvable name
        // that answers 127.0.0.1. Nothing about the *name* is suspicious, so a
        // hostname block-list stops none of this — only the address does
        // (NET-007).
        let p = Policy::default();
        let e = p
            .addresses("innocent-looking.example", &[ip("127.0.0.1")])
            .unwrap_err();
        assert!(matches!(
            e,
            Refusal::Address {
                class: AddressClass::Loopback,
                ..
            }
        ));
        assert!(e.message().contains("this machine"), "{}", e.message());
        assert!(e.message().contains("not overridable"), "{}", e.message());
    }

    #[test]
    fn a_hostname_that_resolves_to_the_metadata_service_is_refused() {
        // 169.254.169.254 is the highest-value SSRF target that exists.
        let p = Policy::default();
        let e = p
            .addresses("meta.example", &[ip("169.254.169.254")])
            .unwrap_err();
        assert!(matches!(
            e,
            Refusal::Address {
                class: AddressClass::LinkLocal,
                ..
            }
        ));
    }

    #[test]
    fn a_host_that_resolves_to_both_public_and_private_addresses_is_refused_entirely() {
        // NET-010. Proceeding on the public subset would be treating a
        // rebinding attempt as a fallback.
        let p = Policy::default();
        let e = p
            .addresses("split.example", &[ip("93.184.215.14"), ip("10.0.0.5")])
            .unwrap_err();
        match e {
            Refusal::MixedAddresses { public, addr, .. } => {
                assert_eq!(public, 1);
                assert_eq!(addr, ip("10.0.0.5"));
            }
            other => panic!("expected a mixed-address refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_host_that_resolves_to_nothing_is_a_failure_not_an_empty_permitted_set() {
        // NET-012. An empty list satisfies "every address is public" for free,
        // which is the wrong answer arrived at correctly.
        let p = Policy::default();
        assert!(matches!(
            p.addresses("gone.example", &[]).unwrap_err(),
            Refusal::HostNotResolved { .. }
        ));
    }

    #[test]
    fn a_response_that_is_not_text_is_refused() {
        // NET-035. Marrow does not need a fetch tool to acquire a binary —
        // that is a download, with its own rules (§144).
        let p = Policy::default();
        for ct in [
            "image/png",
            "application/octet-stream",
            "application/pdf",
            "video/mp4",
        ] {
            assert!(p.content_type(Some(ct)).is_err(), "{ct} must be refused");
        }
        for ct in [
            "text/html; charset=utf-8",
            "TEXT/PLAIN",
            "application/json",
            "application/xhtml+xml",
            "application/ld+json",
        ] {
            assert!(p.content_type(Some(ct)).is_ok(), "{ct} must be accepted");
        }
        // A server that sends none is common for plain files, and the byte cap
        // plus the extractor already bound what that can cost.
        assert!(p.content_type(None).is_ok());
    }

    #[test]
    fn a_url_with_a_query_string_is_confirmed_every_time() {
        // NET-019/NET-020. The query is the payload. A session-wide yes to a
        // host was never a yes to arbitrary future payloads sent to it, and
        // the one-shot token is consumed so "every time" is structural.
        let p = Policy::default();
        let mut consent = Consent::new();
        consent.confirm_host("duckduckgo.com");
        let u = url("https://duckduckgo.com/?q=lease+renewal");

        match p.decide(&u, &consent, &Turn::new()) {
            Decision::Confirm {
                why: ConfirmReason::CarriesAQuery { query, .. },
                ..
            } => assert_eq!(query, "q=lease renewal", "the query is shown decoded"),
            other => panic!("a host confirmation must not cover a query: {other:?}"),
        }

        consent.confirm_once(&u.wire());
        assert!(p.decide(&u, &consent, &Turn::new()).allowed());
        consent.take_once(&u.wire());
        assert!(
            !p.decide(&u, &consent, &Turn::new()).allowed(),
            "the token must be spent, not merely checked"
        );
    }

    #[test]
    fn a_confirmed_host_is_not_confirmed_twice_in_one_session() {
        let p = Policy::default();
        let mut consent = Consent::new();
        let u = url("https://arxiv.org/abs/1234");
        assert!(matches!(
            p.decide(&u, &consent, &Turn::new()),
            Decision::Confirm { .. }
        ));
        consent.confirm_host("arxiv.org");
        assert!(p.decide(&u, &consent, &Turn::new()).allowed());
    }

    #[test]
    fn a_subdomain_of_a_confirmed_host_is_not_covered_by_it() {
        // NET-018. Wildcarding is how one confirmation comes to cover a host
        // the user never saw.
        let p = Policy::default();
        let mut consent = Consent::new();
        consent.confirm_host("example.com");
        assert!(!consent.is_confirmed("docs.example.com"));
        assert!(!consent.is_confirmed("example.com.evil.test"));
        assert!(matches!(
            p.decide(&url("https://docs.example.com/x"), &consent, &Turn::new()),
            Decision::Confirm { .. }
        ));
    }

    #[test]
    fn a_refusal_is_never_offered_as_a_confirmation() {
        // Prompting for a request that was going to be refused anyway is how
        // prompts stop being read.
        let p = Policy::default();
        let consent = Consent::new();
        match p.decide(&url("http://never.example/"), &consent, &Turn::new()) {
            Decision::Refuse(Refusal::Scheme { .. }) => {}
            other => panic!("expected a refusal ahead of any prompt, got {other:?}"),
        }
    }

    #[test]
    fn a_fetch_budget_that_is_exhausted_refuses_the_next_fetch() {
        // NET-055. The model asking again does not refill it.
        let p = Policy::default();
        let mut turn = Turn::new();
        for _ in 0..p.max_fetches_per_turn {
            turn.spend(&p).expect("within budget");
        }
        let e = turn.spend(&p).unwrap_err();
        assert!(matches!(e, Refusal::BudgetExhausted { limit: 8, .. }));
        assert!(e.message().contains("new question"), "{}", e.message());
        assert_eq!(turn.remaining(&p), 0);
    }

    #[test]
    fn a_refused_fetch_still_consumes_budget() {
        // NET-056. Otherwise refusals are free and the model probes until
        // something works — which is the runaway the budget exists to stop.
        let p = Policy::default();
        let mut turn = Turn::new();
        turn.spend(&p)
            .expect("the attempt is spent before it is judged");
        assert_eq!(turn.used(), 1);
        assert_eq!(turn.budget_line(&p), "1 of 8 fetches used");
    }

    #[test]
    fn the_same_url_is_not_fetched_twice_in_one_turn() {
        // NET-057. A re-fetch loop over one URL is the commonest shape of
        // runaway.
        let p = Policy::default();
        let mut consent = Consent::new();
        consent.confirm_host("example.com");
        let mut turn = Turn::new();
        let u = url("https://example.com/a");
        assert!(p.decide(&u, &consent, &turn).allowed());
        turn.record(&u.wire());
        assert!(matches!(
            p.decide(&u, &consent, &turn),
            Decision::Refuse(Refusal::Repeat { .. })
        ));
    }

    #[test]
    fn a_policy_refusal_is_never_retryable_and_a_transport_failure_is() {
        // The §108 rule: a denial a retry can defeat is not a policy. The
        // world being unavailable is a different kind of fact.
        for r in [
            Refusal::Credentials,
            Refusal::Port { port: 22 },
            Refusal::Repeat { url: "x".into() },
            Refusal::TooManyRedirects { limit: 5 },
        ] {
            assert_eq!(r.code(), Code::PolDenied);
            assert!(!r.code().retryable(), "{r:?}");
        }
        assert!(Refusal::Unreachable {
            host: "example.com".into(),
            detail: "connection reset".into()
        }
        .code()
        .retryable());
    }

    #[test]
    fn every_refusal_names_a_cause_and_an_action() {
        // SUP-001. Generic failure text is a defect, not a style issue.
        let all = [
            Refusal::Malformed(UrlError::NoScheme),
            Refusal::Scheme {
                scheme: "http".into(),
            },
            Refusal::Port { port: 8080 },
            Refusal::Credentials,
            Refusal::HostNotResolved {
                host: "a.example".into(),
            },
            Refusal::Address {
                host: "a.example".into(),
                addr: ip("127.0.0.1"),
                class: AddressClass::Loopback,
            },
            Refusal::MixedAddresses {
                host: "a.example".into(),
                addr: ip("10.0.0.1"),
                class: AddressClass::Private,
                public: 1,
            },
            Refusal::TooManyRedirects { limit: 5 },
            Refusal::BadRedirect {
                status: 302,
                detail: "no Location".into(),
            },
            Refusal::RedirectToUnconfirmedHost {
                from: "a.example".into(),
                to: "b.example".into(),
            },
            Refusal::ContentType {
                got: "image/png".into(),
            },
            Refusal::Status { status: 404 },
            Refusal::TimedOut { limit: MAX_ELAPSED },
            Refusal::BudgetExhausted { used: 8, limit: 8 },
            Refusal::Repeat {
                url: "https://a.example/".into(),
            },
            Refusal::NotConfirmed {
                url: "https://a.example/".into(),
                why: ConfirmReason::NewHost {
                    host: "a.example".into(),
                },
            },
            Refusal::Unreachable {
                host: "a.example".into(),
                detail: "reset".into(),
            },
        ];
        for r in all {
            let m = r.message();
            assert!(m.len() > 40, "{r:?} explains nothing: {m}");
            assert!(m.ends_with('.'), "{r:?} message is not a sentence: {m}");
            // And it converts to a core error without losing the code.
            let e: Error = r.clone().into();
            assert_eq!(e.code(), r.code());
        }
    }

    #[test]
    fn a_query_is_decoded_for_display_but_control_characters_are_not_let_through() {
        // NET-050: reviewing `%20` is not reviewing. But a decoded newline
        // would let the displayed string lie about its own length.
        assert_eq!(decode_query("q=who+owns+%2Fhome"), "q=who owns /home");
        assert_eq!(decode_query("q=a%0Ab"), "q=a␀b");
        assert_eq!(decode_query("q=100%25"), "q=100%");
        // A malformed escape is shown as written rather than dropped.
        assert_eq!(decode_query("q=%zz"), "q=%zz");
    }
}
