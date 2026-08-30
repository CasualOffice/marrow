//! Which IP addresses may be connected to (Part 9 §153.2).
//!
//! The whole SSRF defence is one sentence: **the resolved address decides,
//! never the hostname.** A name is an indirection somebody else controls —
//! `localtest.me` resolves to `127.0.0.1` and always has — so a block-list of
//! hostnames is decoration.
//!
//! Two properties make this list safe to rely on:
//!
//! 1. **Default deny.** [`classify`] returns a class, and only `Public` is
//!    permitted. A range nobody thought about is not public by accident; it is
//!    refused because it never matched a rule that said otherwise.
//! 2. **One notation.** `127.0.0.1`, `::ffff:127.0.0.1` and `64:ff9b::7f00:1`
//!    are the same machine written three ways, and all three are unwrapped to
//!    the IPv4 address before anything is decided (NET-009).
//!
//! `std` has `is_global()` for exactly this, and it is still unstable. This is
//! the stable-Rust version of it, biased towards refusing.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// What kind of address this is. Only [`AddressClass::Public`] may be fetched.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AddressClass {
    /// Routable on the public internet, as far as we can tell.
    Public,
    /// 127.0.0.0/8, ::1. Marrow's own machine.
    Loopback,
    /// 169.254.0.0/16, fe80::/10 — where every cloud metadata service lives.
    LinkLocal,
    /// RFC 1918. The user's own network, reachable only from inside it.
    Private,
    /// 100.64.0.0/10. CGNAT, and where Tailscale puts a user's other machines.
    SharedAddressSpace,
    /// fc00::/7. IPv6's RFC 1918.
    UniqueLocal,
    Multicast,
    /// 0.0.0.0/8, ::. "This host", which many stacks route to loopback.
    Unspecified,
    /// 255.255.255.255.
    Broadcast,
    /// Documentation, benchmarking, future use, and anything else that is not
    /// a destination.
    Reserved,
}

impl AddressClass {
    pub fn is_public(self) -> bool {
        matches!(self, AddressClass::Public)
    }

    /// Why this class is refused, in words that name the risk rather than the
    /// range. "Not permitted" teaches nothing; "that is this machine" does.
    pub fn why(self) -> &'static str {
        match self {
            AddressClass::Public => "routable on the public internet",
            AddressClass::Loopback => "this machine itself, not anywhere on the internet",
            AddressClass::LinkLocal => {
                "a link-local address, which is where cloud metadata services live"
            }
            AddressClass::Private => "a private address on this network",
            AddressClass::SharedAddressSpace => {
                "a shared-address-space address, which is where carrier NAT and Tailscale sit"
            }
            AddressClass::UniqueLocal => "a unique-local address on this network",
            AddressClass::Multicast => "a multicast address, which is not a destination",
            AddressClass::Unspecified => {
                "the unspecified address, which most stacks route to this machine"
            }
            AddressClass::Broadcast => "the broadcast address, which is not a destination",
            AddressClass::Reserved => "a reserved address that is not routable on the internet",
        }
    }
}

/// Classify an address, unwrapping every IPv6 notation for an IPv4 one first.
pub fn classify(ip: IpAddr) -> AddressClass {
    match ip {
        IpAddr::V4(v4) => classify_v4(v4),
        IpAddr::V6(v6) => match unwrap_v4(v6) {
            Some(v4) => classify_v4(v4),
            None => classify_v6(v6),
        },
    }
}

/// The three ways an IPv4 address hides inside an IPv6 one.
///
/// NET-009. Missing any of them turns the entire list below into a suggestion.
fn unwrap_v4(v6: Ipv6Addr) -> Option<Ipv4Addr> {
    let s = v6.segments();
    // ::ffff:a.b.c.d — IPv4-mapped, the form every dual-stack socket produces.
    if s[0..5] == [0, 0, 0, 0, 0] && s[5] == 0xffff {
        return Some(embedded(s[6], s[7]));
    }
    // ::a.b.c.d — IPv4-compatible. Deprecated, still parsed by most stacks.
    // `::` and `::1` are excluded: they are their own thing, handled in v6.
    if s[0..6] == [0, 0, 0, 0, 0, 0] && !(s[6] == 0 && s[7] <= 1) {
        return Some(embedded(s[6], s[7]));
    }
    // 64:ff9b::a.b.c.d — the well-known NAT64 prefix. A NAT64 gateway will
    // happily forward this to 169.254.169.254 on the v4 side.
    if s[0] == 0x0064 && s[1] == 0xff9b && s[2..6] == [0, 0, 0, 0] {
        return Some(embedded(s[6], s[7]));
    }
    None
}

fn embedded(hi: u16, lo: u16) -> Ipv4Addr {
    Ipv4Addr::new(
        (hi >> 8) as u8,
        (hi & 0xff) as u8,
        (lo >> 8) as u8,
        (lo & 0xff) as u8,
    )
}

fn classify_v4(ip: Ipv4Addr) -> AddressClass {
    let [a, b, c, _] = ip.octets();
    if ip.is_unspecified() || a == 0 {
        return AddressClass::Unspecified;
    }
    if ip.is_loopback() {
        return AddressClass::Loopback;
    }
    if ip.is_link_local() {
        return AddressClass::LinkLocal;
    }
    if ip.is_private() {
        return AddressClass::Private;
    }
    // 100.64.0.0/10 — RFC 6598. `is_private()` does not cover it and it is
    // where a user's other machines actually are.
    if a == 100 && (64..=127).contains(&b) {
        return AddressClass::SharedAddressSpace;
    }
    if ip.is_multicast() {
        return AddressClass::Multicast;
    }
    if ip.is_broadcast() {
        return AddressClass::Broadcast;
    }
    if ip.is_documentation()
        // 192.0.0.0/24 IETF protocol assignments, 198.18.0.0/15 benchmarking,
        // 240.0.0.0/4 reserved for future use.
        || (a == 192 && b == 0 && c == 0)
        || (a == 198 && (b == 18 || b == 19))
        || a >= 240
    {
        return AddressClass::Reserved;
    }
    AddressClass::Public
}

fn classify_v6(ip: Ipv6Addr) -> AddressClass {
    if ip.is_unspecified() {
        return AddressClass::Unspecified;
    }
    if ip.is_loopback() {
        return AddressClass::Loopback;
    }
    if ip.is_multicast() {
        return AddressClass::Multicast;
    }
    let s = ip.segments();
    // fe80::/10
    if s[0] & 0xffc0 == 0xfe80 {
        return AddressClass::LinkLocal;
    }
    // fc00::/7
    if s[0] & 0xfe00 == 0xfc00 {
        return AddressClass::UniqueLocal;
    }
    // 2001:db8::/32 documentation, 100::/64 discard-only, 2001:2::/48
    // benchmarking, 3fff::/20 documentation.
    if (s[0] == 0x2001 && s[1] == 0x0db8)
        || (s[0] == 0x0100 && s[1..4] == [0, 0, 0])
        || (s[0] == 0x2001 && s[1] == 0x0002 && s[2] == 0)
        || (s[0] & 0xfff0 == 0x3ff0)
    {
        return AddressClass::Reserved;
    }
    AddressClass::Public
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(s: &str) -> AddressClass {
        classify(s.parse().expect("test address must parse"))
    }

    #[test]
    fn loopback_is_refused_in_every_notation_it_can_be_written_in() {
        // NET-009. `127.0.0.1` written three ways is one machine, and each
        // notation that is missed is a complete bypass of the whole list.
        assert_eq!(c("127.0.0.1"), AddressClass::Loopback);
        assert_eq!(c("127.99.1.2"), AddressClass::Loopback);
        assert_eq!(c("::1"), AddressClass::Loopback);
        assert_eq!(c("::ffff:127.0.0.1"), AddressClass::Loopback);
        assert_eq!(c("::7f00:1"), AddressClass::Loopback);
        assert_eq!(c("64:ff9b::7f00:1"), AddressClass::Loopback);
    }

    #[test]
    fn the_cloud_metadata_address_is_refused_including_through_nat64() {
        // 169.254.169.254 is the single most valuable SSRF target there is,
        // and a NAT64 gateway will forward the v6 spelling to it.
        assert_eq!(c("169.254.169.254"), AddressClass::LinkLocal);
        assert_eq!(c("::ffff:169.254.169.254"), AddressClass::LinkLocal);
        assert_eq!(c("64:ff9b::a9fe:a9fe"), AddressClass::LinkLocal);
        assert_eq!(c("fe80::1"), AddressClass::LinkLocal);
    }

    #[test]
    fn the_users_own_network_is_refused() {
        // The LAN is reachable only from inside it, which is exactly where
        // Marrow is running. That is what makes this a scanner otherwise.
        for a in ["10.0.0.5", "172.16.0.1", "172.31.255.254", "192.168.1.1"] {
            assert_eq!(c(a), AddressClass::Private, "{a}");
        }
        assert_eq!(c("fd00::1"), AddressClass::UniqueLocal);
        assert_eq!(c("fc00::1"), AddressClass::UniqueLocal);
    }

    #[test]
    fn the_shared_address_space_is_refused_because_tailscale_lives_there() {
        // `Ipv4Addr::is_private` does not cover 100.64.0.0/10, and a user's
        // other machines are commonly on it.
        assert_eq!(c("100.64.0.1"), AddressClass::SharedAddressSpace);
        assert_eq!(c("100.127.255.255"), AddressClass::SharedAddressSpace);
        // The edges of the /10 are public and must stay public.
        assert_eq!(c("100.63.255.255"), AddressClass::Public);
        assert_eq!(c("100.128.0.1"), AddressClass::Public);
    }

    #[test]
    fn nothing_outside_a_recognised_public_range_is_public_by_default() {
        // Default deny: the classes below are all refused because they never
        // matched a rule that said "public", not because someone listed them.
        for a in [
            "0.0.0.0",
            "0.1.2.3",
            "255.255.255.255",
            "224.0.0.1",
            "240.0.0.1",
            "192.0.2.1",
            "198.18.0.1",
            "::",
            "ff02::1",
            "2001:db8::1",
        ] {
            assert!(!c(a).is_public(), "{a} must not be public, got {:?}", c(a));
        }
    }

    #[test]
    fn a_real_public_address_is_still_public() {
        // A defence that refuses everything is not a defence, it is an outage.
        for a in ["93.184.215.14", "1.1.1.1", "8.8.8.8", "2606:4700::1111"] {
            assert_eq!(c(a), AddressClass::Public, "{a}");
        }
    }

    #[test]
    fn every_refusal_explains_the_risk_rather_than_naming_the_range() {
        // SUP-001: a cause-and-action message. "Not permitted" teaches nothing.
        assert!(AddressClass::Loopback.why().contains("this machine"));
        assert!(AddressClass::Private.why().contains("network"));
        assert!(AddressClass::LinkLocal.why().contains("metadata"));
        for class in [
            AddressClass::Loopback,
            AddressClass::LinkLocal,
            AddressClass::Private,
            AddressClass::SharedAddressSpace,
            AddressClass::UniqueLocal,
            AddressClass::Multicast,
            AddressClass::Unspecified,
            AddressClass::Broadcast,
            AddressClass::Reserved,
        ] {
            assert!(class.why().len() > 20, "{class:?} explains nothing");
        }
    }
}
