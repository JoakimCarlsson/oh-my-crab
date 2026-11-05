//! Which addresses a URL read may connect to.
//!
//! The user approves a URL by what it says. A host written as an address, or as
//! `localhost`, says where it points, so it is reached as written. Any other
//! name could hide a loopback, private or link-local address behind a public
//! looking name, either from the start or by changing its DNS answer after the
//! user approved it, so such names are only allowed to resolve to public
//! addresses. The check runs as the connection is made, for every hop.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use reqwest::dns::{Addrs, Name, Resolve, Resolving};

/// A resolver that drops every non-public address a name resolves to, unless
/// the name is `localhost` or below it, and fails when nothing is left.
pub(super) struct PublicOnly;

impl Resolve for PublicOnly {
    /// Resolves `name` with the system resolver and keeps the addresses it may
    /// reach.
    ///
    /// # Errors
    ///
    /// The future fails when the lookup fails, or when the name only resolves
    /// to addresses that are not public.
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_owned();
        Box::pin(async move {
            let found: Vec<SocketAddr> =
                tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
            let kept = reachable(&host, found);
            if kept.is_empty() {
                return Err(format!(
                    "{host} resolves only to local or private addresses, which a host name \
                     may not reach; do not retry with the address unless the user asked for it"
                )
                .into());
            }
            Ok(Box::new(kept.into_iter()) as Addrs)
        })
    }
}

/// The addresses in `found` that the name `host` may connect to: all of them
/// for `localhost` and names below it, and only the public ones otherwise.
fn reachable(host: &str, found: Vec<SocketAddr>) -> Vec<SocketAddr> {
    if names_local(host) {
        return found;
    }
    found
        .into_iter()
        .filter(|address| is_public(address.ip()))
        .collect()
}

/// Whether `host` is `localhost` or a name below it, which says plainly that
/// it points at this machine.
fn names_local(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    host == "localhost" || host.ends_with(".localhost")
}

/// Whether `ip` is an address on the public internet: not loopback, private,
/// link-local, unspecified, shared, multicast, broadcast or reserved for
/// documentation and benchmarks, and for IPv6 also not one of the ranges that
/// carry an IPv4 address unless that address is public itself.
pub(super) fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => is_public_v6(v6),
    }
}

/// Whether the IPv4 address `ip` is public, as [`is_public`] means it.
fn is_public_v4(ip: Ipv4Addr) -> bool {
    let [first, second, ..] = ip.octets();
    let shared = first == 100 && (64..128).contains(&second);
    let benchmarking = first == 198 && (18..20).contains(&second);
    let reserved = first >= 240;
    !(ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_documentation()
        || first == 0
        || shared
        || benchmarking
        || reserved)
}

/// Whether the IPv6 address `ip` is public, as [`is_public`] means it.
///
/// Only global unicast, `2000::/3`, can be public. Addresses that carry an
/// IPv4 address, mapped (`::ffff:0:0/96`), well-known NAT64 (`64:ff9b::/96`)
/// and 6to4 (`2002::/16`), are judged by that IPv4 address. Within global
/// unicast, the protocol assignments in `2001::/23` (Teredo, benchmarking and
/// others) and both documentation ranges are not public.
fn is_public_v6(ip: Ipv6Addr) -> bool {
    if let Some(v4) = embedded_v4(ip) {
        return is_public_v4(v4);
    }
    let [first, second, ..] = ip.segments();
    let global_unicast = first & 0xe000 == 0x2000;
    let protocol_assignments = first == 0x2001 && second < 0x0200;
    let documentation = (first == 0x2001 && second == 0x0db8) || first & 0xfff0 == 0x3ff0;
    global_unicast && !protocol_assignments && !documentation
}

/// The IPv4 address `ip` carries, when it is IPv4-mapped, well-known-prefix
/// NAT64 or 6to4.
fn embedded_v4(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    let segments = ip.segments();
    let low = |high: u16, low: u16| Ipv4Addr::from((u32::from(high) << 16) | u32::from(low));
    if let Some(v4) = ip.to_ipv4_mapped() {
        return Some(v4);
    }
    if segments[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
        return Some(low(segments[6], segments[7]));
    }
    if segments[0] == 0x2002 {
        return Some(low(segments[1], segments[2]));
    }
    None
}

#[cfg(test)]
/// Tests for the address policy.
mod tests {
    use super::*;

    /// Parses `text` as an address.
    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    /// Public addresses pass and local, private and reserved ones do not.
    #[test]
    fn tells_public_addresses() {
        for public in [
            "93.184.216.34",
            "1.1.1.1",
            "2606:4700::1111",
            "2a00:1450:4001::200e",
            "64:ff9b::101:101",
            "2002:5db8:d822::1",
            "::ffff:1.1.1.1",
        ] {
            assert!(is_public(ip(public)), "{public}");
        }
        for local in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "0.0.0.0",
            "100.64.0.1",
            "198.18.0.1",
            "224.0.0.1",
            "255.255.255.255",
            "::1",
            "::",
            "fd00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "::127.0.0.1",
            "64:ff9b::7f00:1",
            "64:ff9b::a9fe:a9fe",
            "64:ff9b:1::a00:1",
            "2002:7f00:1::",
            "2002:c0a8:101::1",
            "2001:0:4136:e378::1",
            "2001:db8::1",
            "3fff::1",
            "fec0::1",
            "100::1",
        ] {
            assert!(!is_public(ip(local)), "{local}");
        }
    }

    /// A name keeps only its public addresses, except `localhost`, which keeps all.
    #[test]
    fn keeps_local_addresses_only_for_localhost() {
        let found = vec![
            SocketAddr::new(ip("127.0.0.1"), 0),
            SocketAddr::new(ip("93.184.216.34"), 0),
        ];
        assert_eq!(reachable("example.com", found.clone()), vec![found[1]]);
        assert_eq!(reachable("localhost", found.clone()), found);
        assert_eq!(reachable("app.localhost.", found.clone()), found);
        assert!(reachable("evil.example", vec![found[0]]).is_empty());
    }

    /// `localhost` still resolves through the guard, to its loopback addresses.
    #[tokio::test]
    async fn resolves_localhost() {
        let name: Name = "localhost".parse().unwrap();
        let addresses: Vec<SocketAddr> = PublicOnly.resolve(name).await.unwrap().collect();
        assert!(addresses.iter().all(|address| address.ip().is_loopback()));
        assert!(!addresses.is_empty());
    }
}
