//! Just enough URL to make a policy decision (Part 9 §153.1).
//!
//! Hand-written rather than pulled from a crate, for one reason: the policy in
//! [`crate::policy`] has to be able to say *why* a URL is refused, and the
//! refusals here — credentials in the authority, a non-ASCII host, a port that
//! is not 443 — are policy facts, not parse failures. A parser that normalises
//! those away silently is the wrong shape for this job.
//!
//! What it does **not** do is significant and deliberate:
//!
//! - **No IDN, no punycode.** Non-ASCII is refused (NET-004). A parser that
//!   guessed at a homograph host would be worse than one that refuses.
//! - **No percent-decoding of the path.** What was written is what is sent, so
//!   the disclosure (NET-023) shows the bytes that actually left.
//! - **No relative-URL exotica.** Only the four forms a `Location` header
//!   really uses (§153.3).

use std::fmt;

/// A URL that parsed. Whether it is *allowed* is [`crate::Policy`]'s question,
/// not this type's — the fields that decide it are all public for that reason.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Url {
    scheme: String,
    /// Lowercased, trailing root dot removed. IPv6 literals without brackets.
    host: String,
    /// Present only when the URL wrote one, so "no port" and ":443" stay
    /// distinguishable in the disclosure.
    port: Option<u16>,
    /// Always begins with `/`.
    path: String,
    query: Option<String>,
    fragment: Option<String>,
    /// The `user:pass@` that was present, if any. Kept rather than dropped so
    /// NET-003 can refuse it instead of silently succeeding without it.
    userinfo: Option<String>,
    /// True when the host is a bare IP literal rather than a name.
    literal: bool,
}

/// Why a string is not a URL at all.
///
/// Distinct from a policy refusal: these are strings nothing could make sense
/// of, not addresses we decline to visit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UrlError {
    Empty,
    /// No `scheme://`. A bare `example.com/x` is ambiguous, and guessing
    /// `https` for it would mean the confirmation dialog showed the user a URL
    /// they did not write.
    NoScheme,
    NoHost,
    /// A port that is not a number, or does not fit in 16 bits.
    BadPort,
    /// Control characters, spaces or anything above ASCII. See NET-004.
    NotAscii,
    /// NET-005.
    TooLong,
    /// An IPv6 literal with no closing bracket.
    Unbalanced,
}

impl UrlError {
    /// Cause and action, per the §108 rule. These reach the user.
    pub fn message(&self) -> &'static str {
        match self {
            UrlError::Empty => "An empty string is not a URL. Supply a full https:// address.",
            UrlError::NoScheme => {
                "That address has no scheme. Write the full URL including https://, so that what \
                 is confirmed is what is sent."
            }
            UrlError::NoHost => "That URL has no host. Supply a full https:// address.",
            UrlError::BadPort => {
                "That URL's port is not a number. Only https on port 443 is fetched."
            }
            UrlError::NotAscii => {
                "That URL contains characters outside ASCII. Marrow has no punycode conversion, \
                 and without one a look-alike host cannot be told from the real one. Supply the \
                 ASCII form of the host."
            }
            UrlError::TooLong => {
                "That URL is longer than 2048 bytes, which makes it a payload rather than an \
                 address. Shorten it or fetch the page it belongs to."
            }
            UrlError::Unbalanced => "That URL's IPv6 literal is missing its closing bracket.",
        }
    }
}

/// NET-005.
pub const MAX_URL_BYTES: usize = 2048;

impl Url {
    pub fn parse(s: &str) -> Result<Url, UrlError> {
        let s = s.trim();
        if s.is_empty() {
            return Err(UrlError::Empty);
        }
        if s.len() > MAX_URL_BYTES {
            return Err(UrlError::TooLong);
        }
        // Everything outside printable ASCII is refused up front: control
        // characters would let a URL smuggle a header break past a naive
        // client, and non-ASCII is NET-004.
        if s.bytes().any(|b| !(0x21..=0x7e).contains(&b)) {
            return Err(UrlError::NotAscii);
        }

        let (scheme, rest) = s.split_once("://").ok_or(UrlError::NoScheme)?;
        if scheme.is_empty()
            || !scheme
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'-' || b == b'.')
        {
            return Err(UrlError::NoScheme);
        }

        // Authority runs to the first of / ? # — in that order of appearance,
        // not that order of precedence.
        let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let (authority, tail) = rest.split_at(end);

        // Last `@`, not the first: `a@b@host` has `a@b` as userinfo.
        let (userinfo, hostport) = match authority.rfind('@') {
            Some(i) => (Some(authority[..i].to_string()), &authority[i + 1..]),
            None => (None, authority),
        };

        let (host, port, literal) = split_host_port(hostport)?;
        if host.is_empty() {
            return Err(UrlError::NoHost);
        }

        let (before_frag, fragment) = match tail.split_once('#') {
            Some((a, b)) => (a, Some(b.to_string())),
            None => (tail, None),
        };
        let (path, query) = match before_frag.split_once('?') {
            Some((a, b)) => (a, Some(b.to_string())),
            None => (before_frag, None),
        };
        let path = if path.is_empty() {
            "/".to_string()
        } else {
            normalize_path(path)
        };

        Ok(Url {
            scheme: scheme.to_ascii_lowercase(),
            host,
            port,
            path,
            query,
            fragment,
            userinfo,
            literal,
        })
    }

    /// Resolve a `Location` against this URL (§153.3).
    ///
    /// Four forms, which is all a redirect really uses: absolute,
    /// scheme-relative (`//host/x`), root-relative (`/x`) and relative (`x`).
    /// Anything else is a parse error rather than a guess — a redirect Marrow
    /// cannot resolve confidently is one it must not follow.
    pub fn join(&self, location: &str) -> Result<Url, UrlError> {
        let location = location.trim();
        if location.is_empty() {
            return Err(UrlError::Empty);
        }
        if location.contains("://") {
            return Url::parse(location);
        }
        if let Some(rest) = location.strip_prefix("//") {
            return Url::parse(&format!("{}://{}", self.scheme, rest));
        }
        let base = format!("{}://{}", self.scheme, self.authority());
        if location.starts_with('/') {
            return Url::parse(&format!("{base}{location}"));
        }
        if location.starts_with('?') || location.starts_with('#') {
            return Url::parse(&format!("{base}{}{location}", self.path));
        }
        // Relative to the current directory, which is the path up to and
        // including its last `/`.
        let dir = match self.path.rfind('/') {
            Some(i) => &self.path[..=i],
            None => "/",
        };
        Url::parse(&format!("{base}{dir}{location}"))
    }

    pub fn scheme(&self) -> &str {
        &self.scheme
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    /// The port that will actually be connected to.
    pub fn port(&self) -> u16 {
        self.port.unwrap_or(match self.scheme.as_str() {
            "https" => 443,
            "http" => 80,
            // Not a scheme we will ever connect on; NET-001 refuses it before
            // this value is used for anything.
            _ => 0,
        })
    }

    /// The port as written, or `None` when it was implied. The disclosure shows
    /// what the model wrote, not what it meant.
    pub fn explicit_port(&self) -> Option<u16> {
        self.port
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn query(&self) -> Option<&str> {
        self.query.as_deref()
    }

    pub fn fragment(&self) -> Option<&str> {
        self.fragment.as_deref()
    }

    pub fn userinfo(&self) -> Option<&str> {
        self.userinfo.as_deref()
    }

    /// Whether the host was written as a bare IP address.
    pub fn is_ip_literal(&self) -> bool {
        self.literal
    }

    fn authority(&self) -> String {
        let host = if self.literal && self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        match self.port {
            Some(p) => format!("{host}:{p}"),
            None => host,
        }
    }

    /// Exactly what goes on the wire.
    ///
    /// The fragment is **not** here: it is never transmitted (NET-006). It is
    /// still reachable through [`Url::fragment`] so the disclosure can show
    /// that the model wrote one.
    pub fn wire(&self) -> String {
        let mut s = format!("{}://{}{}", self.scheme, self.authority(), self.path);
        if let Some(q) = &self.query {
            s.push('?');
            s.push_str(q);
        }
        s
    }
}

impl fmt::Display for Url {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.wire())?;
        if let Some(fr) = &self.fragment {
            write!(f, "#{fr}")?;
        }
        Ok(())
    }
}

fn split_host_port(s: &str) -> Result<(String, Option<u16>, bool), UrlError> {
    if let Some(rest) = s.strip_prefix('[') {
        let close = rest.find(']').ok_or(UrlError::Unbalanced)?;
        let host = rest[..close].to_ascii_lowercase();
        let after = &rest[close + 1..];
        let port = match after.strip_prefix(':') {
            Some(p) => Some(p.parse::<u16>().map_err(|_| UrlError::BadPort)?),
            None if after.is_empty() => None,
            None => return Err(UrlError::BadPort),
        };
        return Ok((host, port, true));
    }
    let (host, port) = match s.rsplit_once(':') {
        Some((h, p)) => (h, Some(p.parse::<u16>().map_err(|_| UrlError::BadPort)?)),
        None => (s, None),
    };
    // A trailing root dot is a legitimate FQDN form and a fine way to slip past
    // an exact host comparison. Normalise it off.
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    let literal = host.parse::<std::net::IpAddr>().is_ok();
    Ok((host, port, literal))
}

/// Remove `.` and `..` segments, so a relative redirect cannot climb out of the
/// path it was resolved against and land somewhere the confirmation did not
/// describe.
fn normalize_path(path: &str) -> String {
    let trailing = path.ends_with('/');
    let mut out: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            s => out.push(s),
        }
    }
    let mut s = String::with_capacity(path.len());
    for seg in &out {
        s.push('/');
        s.push_str(seg);
    }
    if s.is_empty() {
        return "/".to_string();
    }
    if trailing {
        s.push('/');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_url_without_a_scheme_is_not_guessed_at() {
        // Guessing https would mean the confirmation dialog showed the user a
        // URL they did not write (NET-023).
        assert_eq!(Url::parse("example.com/x"), Err(UrlError::NoScheme));
    }

    #[test]
    fn credentials_survive_parsing_so_the_policy_can_refuse_them() {
        // A parser that dropped `user:pass@` would turn NET-003 into a silent
        // success.
        let u = Url::parse("https://alice:hunter2@example.com/x").unwrap();
        assert_eq!(u.userinfo(), Some("alice:hunter2"));
        assert_eq!(u.host(), "example.com");
    }

    #[test]
    fn the_last_at_sign_ends_the_userinfo_not_the_first() {
        // `https://real.example@evil.example/` points at evil.example. Reading
        // it the other way is a classic phishing parse.
        let u = Url::parse("https://real.example@evil.example/x").unwrap();
        assert_eq!(u.host(), "evil.example");
    }

    #[test]
    fn a_trailing_root_dot_is_normalised_away() {
        // `example.com.` and `example.com` are the same host, and an exact
        // consent comparison (NET-018) must not see two.
        assert_eq!(
            Url::parse("https://Example.COM./x").unwrap().host(),
            "example.com"
        );
    }

    #[test]
    fn a_url_with_non_ascii_is_refused_rather_than_normalised() {
        // NET-004: without punycode a homograph host is indistinguishable from
        // the real one.
        let e = Url::parse("https://exämple.com/").unwrap_err();
        assert_eq!(e, UrlError::NotAscii);
        assert!(e.message().contains("look-alike"), "{}", e.message());
    }

    #[test]
    fn a_url_containing_a_control_character_is_refused() {
        // A newline in a URL is a header-injection attempt wearing an address.
        assert_eq!(
            Url::parse("https://example.com/a\r\nX: y"),
            Err(UrlError::NotAscii)
        );
    }

    #[test]
    fn a_url_longer_than_the_cap_is_refused() {
        // NET-005: at that length it is a payload, not an address.
        let long = format!("https://example.com/{}", "a".repeat(MAX_URL_BYTES));
        assert_eq!(Url::parse(&long), Err(UrlError::TooLong));
    }

    #[test]
    fn an_ipv6_literal_keeps_its_brackets_only_on_the_wire() {
        let u = Url::parse("https://[::ffff:127.0.0.1]:443/x").unwrap();
        assert_eq!(u.host(), "::ffff:127.0.0.1");
        assert!(u.is_ip_literal());
        assert_eq!(u.wire(), "https://[::ffff:127.0.0.1]:443/x");
    }

    #[test]
    fn the_fragment_is_never_part_of_what_goes_on_the_wire() {
        // NET-006. It is still visible for the disclosure.
        let u = Url::parse("https://example.com/a?b=c#secret").unwrap();
        assert_eq!(u.wire(), "https://example.com/a?b=c");
        assert_eq!(u.fragment(), Some("secret"));
        assert_eq!(u.to_string(), "https://example.com/a?b=c#secret");
    }

    #[test]
    fn a_relative_redirect_cannot_climb_out_of_its_own_path() {
        // `../..` walking above the root would resolve to a URL the
        // confirmation never described.
        let base = Url::parse("https://example.com/a/b/c").unwrap();
        assert_eq!(base.join("../../../etc").unwrap().path(), "/etc");
        assert_eq!(base.join("d").unwrap().wire(), "https://example.com/a/b/d");
        assert_eq!(base.join("/d").unwrap().wire(), "https://example.com/d");
    }

    #[test]
    fn a_scheme_relative_redirect_keeps_the_scheme_and_changes_the_host() {
        let base = Url::parse("https://example.com/a").unwrap();
        let next = base.join("//other.example/b").unwrap();
        assert_eq!(next.scheme(), "https");
        assert_eq!(next.host(), "other.example");
    }

    #[test]
    fn an_absolute_redirect_replaces_everything_including_the_scheme() {
        // Which is exactly why the policy re-runs on every hop (NET-013): a
        // redirect can change the scheme to one that is refused, and a hop that
        // inherited the base's scheme would hide that.
        let base = Url::parse("https://example.com/a").unwrap();
        let next = base.join("http://downgraded.example/a").unwrap();
        assert_eq!(next.scheme(), "http");
        assert_eq!(next.host(), "downgraded.example");
        // And a scheme with no authority at all does not resolve into one.
        assert_eq!(base.join("file:///etc/passwd"), Err(UrlError::NoHost));
    }
}
