//! Trusted reverse proxies: decides which client IP a request counts as when it arrives through one.
use std::net::IpAddr;

/// IPs and CIDRs whose `X-Forwarded-For` we believe. Empty (the default) trusts nobody.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrustedProxies(Vec<(IpAddr, u8)>);

impl TrustedProxies {
    /// Comma list of `ip` or `ip/prefix`; blank entries are skipped.
    pub fn parse(s: &str) -> Result<Self, String> {
        let mut v = Vec::new();
        for e in s.split(',').map(str::trim).filter(|e| !e.is_empty()) {
            let (ip, bits) = e.split_once('/').map_or((e, None), |(a, b)| (a, Some(b)));
            let ip: IpAddr = ip.parse().map_err(|_| format!("bad proxy address {e:?}"))?;
            let ip = ip.to_canonical();
            let max = if ip.is_ipv4() { 32 } else { 128 };
            let bits = match bits {
                None => max,
                Some(b) => b
                    .parse::<u8>()
                    .ok()
                    .filter(|b| *b <= max)
                    .ok_or_else(|| format!("bad prefix length in {e:?}"))?,
            };
            v.push((ip, bits));
        }
        Ok(Self(v))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        let ip = ip.to_canonical();
        self.0.iter().any(|(net, bits)| match (net, ip) {
            (IpAddr::V4(n), IpAddr::V4(a)) => {
                let m = u32::MAX.checked_shl(32 - u32::from(*bits)).unwrap_or(0);
                u32::from(*n) & m == u32::from(a) & m
            }
            (IpAddr::V6(n), IpAddr::V6(a)) => {
                let m = u128::MAX.checked_shl(128 - u32::from(*bits)).unwrap_or(0);
                u128::from(*n) & m == u128::from(a) & m
            }
            _ => false,
        })
    }

    /// The client address: the socket `peer` unless it is a trusted proxy; then the first untrusted
    /// entry walking `X-Forwarded-For` from the right (the leftmost entry is client-controlled).
    /// A malformed entry, or a chain made only of proxies, falls back to `peer`.
    pub fn client_ip<'a>(&self, peer: IpAddr, xff: impl IntoIterator<Item = &'a str>) -> IpAddr {
        if !self.contains(peer) {
            return peer;
        }
        let hops: Vec<&str> = xff.into_iter().flat_map(|v| v.split(',')).collect();
        for h in hops.iter().rev() {
            let Ok(ip) = h.trim().parse::<IpAddr>() else {
                return peer;
            };
            if !self.contains(ip) {
                return ip.to_canonical();
            }
        }
        peer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }
    fn tp(s: &str) -> TrustedProxies {
        TrustedProxies::parse(s).unwrap()
    }

    #[test]
    fn parses_ips_and_cidrs_and_rejects_junk() {
        assert!(tp("").is_empty());
        assert!(tp(" , ").is_empty());
        assert!(!tp("10.0.0.1, fd00::/8").is_empty());
        for bad in ["nope", "10.0.0.1/33", "::1/129", "10.0.0.1/x"] {
            assert!(TrustedProxies::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn cidr_matching() {
        let t = tp("10.0.0.0/8,192.168.1.5,fd00::/8,0.0.0.0/0");
        assert!(t.contains(ip("10.255.0.1")));
        assert!(t.contains(ip("192.168.1.5")));
        assert!(t.contains(ip("fd12::1")));
        assert!(t.contains(ip("::ffff:10.1.1.1")));
        let t = tp("10.0.0.0/8,192.168.1.5,fd00::/8");
        assert!(!t.contains(ip("11.0.0.1")));
        assert!(!t.contains(ip("192.168.1.6")));
        assert!(!t.contains(ip("fe80::1")));
        assert!(tp("0.0.0.0/0").contains(ip("8.8.8.8")));
        assert!(!tp("0.0.0.0/0").contains(ip("::1")));
    }

    #[test]
    fn untrusted_peer_xff_is_ignored() {
        let t = tp("10.0.0.1");
        assert_eq!(t.client_ip(ip("1.2.3.4"), ["9.9.9.9"]), ip("1.2.3.4"));
        assert_eq!(
            TrustedProxies::default().client_ip(ip("10.0.0.1"), ["9.9.9.9"]),
            ip("10.0.0.1")
        );
    }

    #[test]
    fn trusted_peer_uses_rightmost_untrusted_not_the_spoofable_leftmost() {
        let t = tp("10.0.0.0/8");
        // client sent a forged leftmost entry; the proxy appended the real one.
        assert_eq!(
            t.client_ip(ip("10.0.0.1"), ["6.6.6.6, 203.0.113.7"]),
            ip("203.0.113.7")
        );
        // multiple hops and multiple header lines
        assert_eq!(
            t.client_ip(
                ip("10.0.0.1"),
                ["6.6.6.6", "203.0.113.7, 10.0.0.2, 10.0.0.3"]
            ),
            ip("203.0.113.7")
        );
    }

    #[test]
    fn ipv6_clients() {
        let t = tp("::1,10.0.0.1");
        assert_eq!(t.client_ip(ip("::1"), ["2001:db8::5"]), ip("2001:db8::5"));
        assert_eq!(
            t.client_ip(ip("::ffff:10.0.0.1"), ["1.1.1.1, ::ffff:10.0.0.1"]),
            ip("1.1.1.1")
        );
    }

    #[test]
    fn malformed_or_all_trusted_falls_back_to_peer() {
        let t = tp("10.0.0.0/8");
        let p = ip("10.0.0.1");
        assert_eq!(t.client_ip(p, ["garbage"]), p);
        assert_eq!(t.client_ip(p, ["1.1.1.1, garbage"]), p);
        assert_eq!(t.client_ip(p, ["10.0.0.5, 10.0.0.6"]), p);
        assert_eq!(t.client_ip(p, []), p);
    }
}
