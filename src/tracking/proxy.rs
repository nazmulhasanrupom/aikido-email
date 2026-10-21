//! Some opens are not a person looking at the email: Apple Mail Privacy Protection, the Google image proxy
//! and security scanners. We mark them, and count a lead that only has such opens as "maybe opened".
use std::net::IpAddr;

/// An open this many seconds after the send, or sooner, is a scanner: a person cannot read that fast.
pub const SCANNER_SECS: i64 = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProxyKind {
    Apple,
    Google,
    Scanner,
}

impl ProxyKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ProxyKind::Apple => "apple",
            ProxyKind::Google => "google",
            ProxyKind::Scanner => "scanner",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cidr {
    addr: IpAddr,
    bits: u8,
}

impl Cidr {
    /// `17.0.0.0/8` or a single address.
    pub fn parse(s: &str) -> Option<Cidr> {
        let s = s.trim();
        let (addr, bits) = match s.split_once('/') {
            Some((a, b)) => (a.parse::<IpAddr>().ok()?, b.parse::<u8>().ok()?),
            None => {
                let a = s.parse::<IpAddr>().ok()?;
                (a, if a.is_ipv4() { 32 } else { 128 })
            }
        };
        let max = if addr.is_ipv4() { 32 } else { 128 };
        (bits <= max).then_some(Cidr { addr, bits })
    }

    pub fn contains(&self, ip: &IpAddr) -> bool {
        fn prefix_match(a: &[u8], b: &[u8], bits: u8) -> bool {
            let full = (bits / 8) as usize;
            if a[..full] != b[..full] {
                return false;
            }
            let rest = bits % 8;
            rest == 0 || (a[full] ^ b[full]) >> (8 - rest) == 0
        }
        match (self.addr, ip) {
            (IpAddr::V4(n), IpAddr::V4(i)) => prefix_match(&n.octets(), &i.octets(), self.bits),
            (IpAddr::V6(n), IpAddr::V6(i)) => prefix_match(&n.octets(), &i.octets(), self.bits),
            // an IPv4 client seen through an IPv6 socket
            (IpAddr::V4(_), IpAddr::V6(i)) => i.to_ipv4_mapped().is_some_and(|v4| self.contains(&IpAddr::V4(v4))),
            _ => false,
        }
    }
}

/// Apple's own network. Apple Mail Privacy Protection loads images from here. Private Relay traffic that leaves
/// through Cloudflare, Akamai or Fastly is not in this list: add those ranges with `AIKIDO_TRACKING_PROXY_CIDRS`.
pub const APPLE_RANGES: [&str; 2] = ["17.0.0.0/8", "2620:149::/32"];

pub fn apple_ranges(extra: &[Cidr]) -> Vec<Cidr> {
    APPLE_RANGES.iter().filter_map(|c| Cidr::parse(c)).chain(extra.iter().copied()).collect()
}

/// Is this open a proxy or a scanner? `secs_since_send` is `None` when the send is not saved yet
/// (the image was asked for before the send finished: a scanner).
pub fn classify(ip: Option<&str>, user_agent: &str, secs_since_send: Option<i64>, apple: &[Cidr]) -> Option<ProxyKind> {
    let ua = user_agent.to_ascii_lowercase();
    if ua.contains("googleimageproxy") || ua.contains("ggpht.com") {
        return Some(ProxyKind::Google);
    }
    if let Some(ip) = ip.and_then(|i| i.trim().parse::<IpAddr>().ok()) {
        if apple.iter().any(|c| c.contains(&ip)) {
            return Some(ProxyKind::Apple);
        }
    }
    match secs_since_send {
        Some(s) if s > SCANNER_SECS => None,
        _ => Some(ProxyKind::Scanner),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apple() -> Vec<Cidr> {
        apple_ranges(&[])
    }

    #[test]
    fn the_google_image_proxy_is_marked() {
        let ua = "Mozilla/5.0 (Windows NT 5.1; rv:11.0) Gecko Firefox/11.0 (via ggpht.com GoogleImageProxy)";
        assert_eq!(classify(Some("66.249.84.1"), ua, Some(3600), &apple()), Some(ProxyKind::Google));
    }

    #[test]
    fn an_apple_address_is_marked() {
        assert_eq!(classify(Some("17.58.12.9"), "Mozilla/5.0", Some(3600), &apple()), Some(ProxyKind::Apple));
        assert_eq!(classify(Some("2620:149:a44::1"), "", Some(3600), &apple()), Some(ProxyKind::Apple));
        assert_eq!(classify(Some("18.58.12.9"), "Mozilla/5.0", Some(3600), &apple()), None);
        assert_eq!(classify(Some("::ffff:17.1.2.3"), "", Some(3600), &apple()), Some(ProxyKind::Apple));
    }

    #[test]
    fn extra_ranges_from_the_operator_work() {
        let extra = [Cidr::parse("172.224.0.0/12").unwrap()];
        assert_eq!(classify(Some("172.225.1.1"), "", Some(3600), &apple_ranges(&extra)), Some(ProxyKind::Apple));
        assert_eq!(classify(Some("172.240.1.1"), "", Some(3600), &apple_ranges(&extra)), None);
    }

    #[test]
    fn a_fast_open_is_a_scanner() {
        assert_eq!(classify(Some("8.8.8.8"), "Mozilla", Some(2), &apple()), Some(ProxyKind::Scanner));
        assert_eq!(classify(Some("8.8.8.8"), "Mozilla", Some(5), &apple()), Some(ProxyKind::Scanner));
        assert_eq!(classify(Some("8.8.8.8"), "Mozilla", Some(6), &apple()), None);
        // the send was not saved yet
        assert_eq!(classify(Some("8.8.8.8"), "Mozilla", None, &apple()), Some(ProxyKind::Scanner));
    }

    #[test]
    fn a_normal_open_is_not_marked() {
        assert_eq!(classify(Some("203.0.113.5"), "Mozilla/5.0 (Macintosh) AppleWebKit", Some(7200), &apple()), None);
        assert_eq!(classify(None, "", Some(7200), &apple()), None);
    }

    #[test]
    fn cidr_edges() {
        let c = Cidr::parse("10.0.0.0/9").unwrap();
        assert!(c.contains(&"10.127.255.255".parse().unwrap()));
        assert!(!c.contains(&"10.128.0.0".parse().unwrap()));
        assert!(Cidr::parse("10.0.0.1").unwrap().contains(&"10.0.0.1".parse().unwrap()));
        assert!(Cidr::parse("10.0.0.0/33").is_none());
        assert!(Cidr::parse("nope").is_none());
    }
}
