//! A fetch that is safe against SSRF (a user URL that points at our own network).
//!
//! - `http` and `https` only, ports 80 and 443 only.
//! - The host is resolved first. If ANY address is loopback, private, link local, CGNAT, multicast, unspecified or an
//!   IPv6 ULA, the fetch is refused. `169.254.169.254` (cloud metadata) is link local, so it is covered.
//! - The connection goes to the address that was checked (`resolve_to_addrs`), so there is no second DNS lookup. This stops DNS rebinding.
//! - Redirects are followed by hand (at most 3). Each target is checked the same way.
//! - Timeout 10 s. Body at most 2 MB. `text/html` and `text/plain` only. `fetch_image` takes PNG, JPEG, GIF and WebP, at most 5 MB.
use async_trait::async_trait;
use reqwest::Url;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

pub const MAX_REDIRECTS: usize = 3;
pub const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_IMAGE_BYTES: usize = 5 * 1024 * 1024;
pub const IMAGE_TYPES: [&str; 4] = ["image/png", "image/jpeg", "image/gif", "image/webp"];
pub const TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq)]
pub enum FetchError {
    /// The URL is not usable (scheme, port, host, syntax).
    BadUrl(String),
    /// The host points at an address we do not connect to.
    Blocked(String),
    TooManyRedirects,
    TooLarge,
    /// Not `text/html` or `text/plain`.
    NotText(String),
    /// Not a PNG, JPEG, GIF or WebP image.
    NotImage(String),
    Status(u16),
    Network(String),
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadUrl(m) => write!(f, "This address can not be used: {m}"),
            Self::Blocked(m) => write!(f, "This address is not allowed: {m}"),
            Self::TooManyRedirects => write!(f, "The page redirects too many times."),
            Self::TooLarge => write!(f, "The file is too large (2 MB for a page, 5 MB for an image)."),
            Self::NotText(t) => write!(f, "The page is not text or HTML ({t})."),
            Self::NotImage(t) => write!(f, "The file is not a PNG, JPEG, GIF or WebP image ({t})."),
            Self::Status(s) => write!(f, "The page answered with status {s}."),
            Self::Network(m) => write!(f, "The page could not be loaded: {m}"),
        }
    }
}

/// Is this address one we must never connect to?
pub fn ip_blocked(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4_blocked(v4),
        IpAddr::V6(v6) => v6_blocked(v6),
    }
}

fn v4_blocked(ip: &Ipv4Addr) -> bool {
    let o = ip.octets();
    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_multicast()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || o[0] == 0
        // CGNAT 100.64.0.0/10
        || (o[0] == 100 && (o[1] & 0xC0) == 64)
        // reserved 240.0.0.0/4
        || o[0] >= 240
        // IETF protocol assignments 192.0.0.0/24
        || (o[0] == 192 && o[1] == 0 && o[2] == 0)
}

fn v6_blocked(ip: &Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return v4_blocked(&v4);
    }
    let s = ip.segments();
    ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        // ULA fc00::/7
        || (s[0] & 0xFE00) == 0xFC00
        // link local fe80::/10
        || (s[0] & 0xFFC0) == 0xFE80
        // NAT64 64:ff9b::/96 carries an IPv4 address in the last 32 bits
        || (s[0] == 0x0064 && s[1] == 0xFF9B && s[2..6] == [0, 0, 0, 0] && v4_blocked(&Ipv4Addr::new((s[6] >> 8) as u8, s[6] as u8, (s[7] >> 8) as u8, s[7] as u8)))
}

#[async_trait]
pub trait Resolver: Send + Sync {
    async fn resolve(&self, host: &str, port: u16) -> std::io::Result<Vec<IpAddr>>;
}

#[derive(Default)]
pub struct SystemDns;

#[async_trait]
impl Resolver for SystemDns {
    async fn resolve(&self, host: &str, port: u16) -> std::io::Result<Vec<IpAddr>> {
        Ok(tokio::net::lookup_host((host, port)).await?.map(|a| a.ip()).collect())
    }
}

#[derive(Debug)]
pub struct Fetched {
    /// The URL after the redirects.
    pub url: Url,
    pub content_type: String,
    pub body: String,
}

#[derive(Debug)]
pub struct FetchedImage {
    /// The URL after the redirects.
    pub url: Url,
    pub content_type: String,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Text,
    Image,
}

pub struct SafeFetcher {
    resolver: Arc<dyn Resolver>,
    /// Decides which addresses are refused. Tests replace it to reach a local server.
    block: Arc<dyn Fn(&IpAddr) -> bool + Send + Sync>,
    ports: Vec<u16>,
}

impl Default for SafeFetcher {
    fn default() -> Self {
        Self { resolver: Arc::new(SystemDns), block: Arc::new(ip_blocked), ports: vec![80, 443] }
    }
}

impl SafeFetcher {
    pub fn with(resolver: Arc<dyn Resolver>, block: Arc<dyn Fn(&IpAddr) -> bool + Send + Sync>, ports: Vec<u16>) -> Self {
        Self { resolver, block, ports }
    }

    /// Check a URL and give the address to connect to.
    async fn vet(&self, url: &Url) -> Result<SocketAddr, FetchError> {
        if !matches!(url.scheme(), "http" | "https") {
            return Err(FetchError::BadUrl("only http and https are allowed".into()));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(FetchError::BadUrl("a user name in the address is not allowed".into()));
        }
        let host = url.host_str().ok_or_else(|| FetchError::BadUrl("no host".into()))?.to_string();
        let port = url.port_or_known_default().ok_or_else(|| FetchError::BadUrl("no port".into()))?;
        if !self.ports.contains(&port) {
            return Err(FetchError::BadUrl("only ports 80 and 443 are allowed".into()));
        }
        let host_clean = host.trim_start_matches('[').trim_end_matches(']');
        let ips: Vec<IpAddr> = match host_clean.parse::<IpAddr>() {
            Ok(ip) => vec![ip],
            Err(_) => self.resolver.resolve(host_clean, port).await.map_err(|e| FetchError::Network(format!("DNS lookup failed ({e})")))?,
        };
        if ips.is_empty() {
            return Err(FetchError::Network("the host has no address".into()));
        }
        // every address counts: one private address among public ones is enough to refuse
        if let Some(bad) = ips.iter().find(|ip| (self.block)(ip)) {
            return Err(FetchError::Blocked(format!("{host} points at a private address ({bad})")));
        }
        Ok(SocketAddr::new(ips[0], port))
    }

    pub async fn fetch(&self, start: &str) -> Result<Fetched, FetchError> {
        let (url, content_type, bytes) = self.fetch_raw(start, Kind::Text).await?;
        Ok(Fetched { url, content_type, body: String::from_utf8_lossy(&bytes).into_owned() })
    }

    /// An image (PNG, JPEG, GIF or WebP, at most 5 MB), with the same safety rules as `fetch`.
    pub async fn fetch_image(&self, start: &str) -> Result<FetchedImage, FetchError> {
        let (url, content_type, bytes) = self.fetch_raw(start, Kind::Image).await?;
        Ok(FetchedImage { url, content_type, bytes })
    }

    async fn fetch_raw(&self, start: &str, kind: Kind) -> Result<(Url, String, Vec<u8>), FetchError> {
        let mut url = Url::parse(start.trim()).map_err(|_| FetchError::BadUrl("not a valid address".into()))?;
        let max = if kind == Kind::Image { MAX_IMAGE_BYTES } else { MAX_BODY_BYTES };
        for hop in 0..=MAX_REDIRECTS {
            let addr = self.vet(&url).await?;
            let host = url.host_str().unwrap_or_default().trim_start_matches('[').trim_end_matches(']').to_string();
            let client = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(TIMEOUT)
                .resolve_to_addrs(&host, &[addr])
                .user_agent("Mozilla/5.0 (compatible; AikidoEmailBot/1.0)")
                .build()
                .map_err(|e| FetchError::Network(e.to_string()))?;
            let accept = if kind == Kind::Image { "image/png,image/jpeg,image/gif,image/webp" } else { "text/html,text/plain" };
            let mut resp = client.get(url.clone()).header("Accept", accept).send().await.map_err(|e| FetchError::Network(e.without_url().to_string()))?;
            let status = resp.status();
            if status.is_redirection() {
                let loc = resp.headers().get("location").and_then(|v| v.to_str().ok()).ok_or(FetchError::Status(status.as_u16()))?;
                if hop == MAX_REDIRECTS {
                    return Err(FetchError::TooManyRedirects);
                }
                url = url.join(loc).map_err(|_| FetchError::BadUrl("the redirect address is not valid".into()))?;
                continue;
            }
            if !status.is_success() {
                return Err(FetchError::Status(status.as_u16()));
            }
            let content_type = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_lowercase();
            let essence = content_type.split(';').next().unwrap_or("").trim().to_string();
            match kind {
                Kind::Text if essence != "text/html" && essence != "text/plain" => {
                    return Err(FetchError::NotText(if essence.is_empty() { "no content type".into() } else { essence }));
                }
                Kind::Image if !IMAGE_TYPES.contains(&essence.as_str()) => {
                    return Err(FetchError::NotImage(if essence.is_empty() { "no content type".into() } else { essence }));
                }
                _ => {}
            }
            if resp.content_length().is_some_and(|n| n as usize > max) {
                return Err(FetchError::TooLarge);
            }
            let mut bytes: Vec<u8> = Vec::new();
            while let Some(chunk) = resp.chunk().await.map_err(|e| FetchError::Network(e.without_url().to_string()))? {
                if bytes.len() + chunk.len() > max {
                    return Err(FetchError::TooLarge);
                }
                bytes.extend_from_slice(&chunk);
            }
            return Ok((url, essence, bytes));
        }
        Err(FetchError::TooManyRedirects)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn private_and_special_addresses_are_blocked() {
        for ip in [
            "127.0.0.1", "10.1.2.3", "172.16.0.1", "172.31.255.255", "192.168.1.1", "169.254.169.254", "169.254.1.1", "100.64.0.1", "100.127.255.255",
            "224.0.0.1", "0.0.0.0", "255.255.255.255", "240.0.0.1", "::1", "::", "fc00::1", "fd12:3456::1", "fe80::1", "ff02::1", "::ffff:10.0.0.1",
            "::ffff:169.254.169.254", "64:ff9b::a00:1",
        ] {
            assert!(ip_blocked(&ip.parse().unwrap()), "{ip} must be blocked");
        }
        for ip in ["8.8.8.8", "1.1.1.1", "93.184.216.34", "172.32.0.1", "100.128.0.1", "2606:4700:4700::1111"] {
            assert!(!ip_blocked(&ip.parse().unwrap()), "{ip} must be allowed");
        }
    }

    struct Fake(HashMap<&'static str, Vec<IpAddr>>);

    #[async_trait]
    impl Resolver for Fake {
        async fn resolve(&self, host: &str, _port: u16) -> std::io::Result<Vec<IpAddr>> {
            self.0.get(host).cloned().ok_or_else(|| std::io::Error::other("no such host"))
        }
    }

    fn fetcher(map: Vec<(&'static str, Vec<&str>)>) -> SafeFetcher {
        let m = map.into_iter().map(|(h, ips)| (h, ips.into_iter().map(|i| i.parse().unwrap()).collect())).collect();
        SafeFetcher::with(Arc::new(Fake(m)), Arc::new(ip_blocked), vec![80, 443])
    }

    #[tokio::test]
    async fn bad_schemes_ports_and_private_hosts_are_refused_before_any_connection() {
        let f = fetcher(vec![("evil.test", vec!["10.0.0.5"]), ("mixed.test", vec!["8.8.8.8", "192.168.0.9"]), ("ok.test", vec!["8.8.8.8"])]);
        assert!(matches!(f.fetch("ftp://ok.test/").await, Err(FetchError::BadUrl(_))));
        assert!(matches!(f.fetch("file:///etc/passwd").await, Err(FetchError::BadUrl(_))));
        assert!(matches!(f.fetch("http://ok.test:8080/").await, Err(FetchError::BadUrl(_))));
        assert!(matches!(f.fetch("http://user:pw@ok.test/").await, Err(FetchError::BadUrl(_))));
        assert!(matches!(f.fetch("http://evil.test/").await, Err(FetchError::Blocked(_))));
        assert!(matches!(f.fetch("http://127.0.0.1/").await, Err(FetchError::Blocked(_))));
        assert!(matches!(f.fetch("http://[::1]/").await, Err(FetchError::Blocked(_))));
        assert!(matches!(f.fetch("http://169.254.169.254/latest/meta-data/").await, Err(FetchError::Blocked(_))));
        // DNS rebinding: one public and one private answer. The private one counts.
        assert!(matches!(f.fetch("http://mixed.test/").await, Err(FetchError::Blocked(_))));
    }

    /// A local server for the redirect tests. The test fetcher lets loopback through, but only for the name `local.test`.
    async fn server(routes: Vec<(&'static str, axum::response::Response)>) -> u16 {
        use axum::routing::get;
        let mut app = axum::Router::new();
        for (path, resp) in routes {
            let body = std::sync::Arc::new(std::sync::Mutex::new(Some(resp)));
            app = app.route(path, get(move || {
                let b = body.clone();
                async move { b.lock().unwrap().take().unwrap_or_default() }
            }));
        }
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        port
    }

    fn local_fetcher(port: u16) -> SafeFetcher {
        let m: HashMap<&'static str, Vec<IpAddr>> = [("local.test", vec!["127.0.0.1".parse().unwrap()]), ("evil.test", vec!["10.0.0.5".parse().unwrap()])].into();
        // loopback is allowed here, everything else uses the real rule
        SafeFetcher::with(Arc::new(Fake(m)), Arc::new(|ip: &IpAddr| !ip.is_loopback() && ip_blocked(ip)), vec![port])
    }

    fn redirect(to: &str) -> axum::response::Response {
        axum::response::Response::builder().status(302).header("location", to).body(axum::body::Body::empty()).unwrap()
    }

    fn page(ct: &str, body: &str) -> axum::response::Response {
        axum::response::Response::builder().status(200).header("content-type", ct).body(axum::body::Body::from(body.to_string())).unwrap()
    }

    #[tokio::test]
    async fn a_page_is_fetched_and_a_redirect_is_followed() {
        let port = server(vec![("/a", redirect("/b")), ("/b", page("text/html; charset=utf-8", "<p>hello</p>"))]).await;
        let got = local_fetcher(port).fetch(&format!("http://local.test:{port}/a")).await.unwrap();
        assert_eq!(got.body, "<p>hello</p>");
        assert_eq!(got.content_type, "text/html");
        assert!(got.url.path() == "/b");
    }

    /// A server whose `/go` redirects to `target`. Returns its port.
    async fn redirector(target: impl Fn(u16) -> String + Send + Sync + 'static) -> u16 {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        let to = target(port);
        let app = axum::Router::new().route("/go", axum::routing::get(move || { let t = to.clone(); async move { redirect(&t) } }));
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        port
    }

    #[tokio::test]
    async fn a_redirect_to_a_private_address_is_blocked() {
        // to a name that resolves to a private address
        let port = redirector(|p| format!("http://evil.test:{p}/x")).await;
        let r = local_fetcher(port).fetch(&format!("http://local.test:{port}/go")).await;
        assert!(matches!(r, Err(FetchError::Blocked(_))), "{r:?}");
        // to the cloud metadata address, written as an IP
        let port = redirector(|p| format!("http://169.254.169.254:{p}/")).await;
        let r = local_fetcher(port).fetch(&format!("http://local.test:{port}/go")).await;
        assert!(matches!(r, Err(FetchError::Blocked(_))), "{r:?}");
    }

    #[tokio::test]
    async fn only_text_and_html_and_small_bodies_are_accepted() {
        let big = "x".repeat(MAX_BODY_BYTES + 10);
        let port = server(vec![("/img", page("image/png", "png")), ("/big", page("text/plain", &big)), ("/txt", page("text/plain", "plain text"))]).await;
        let f = local_fetcher(port);
        assert!(matches!(f.fetch(&format!("http://local.test:{port}/img")).await, Err(FetchError::NotText(_))));
        assert!(matches!(f.fetch(&format!("http://local.test:{port}/big")).await, Err(FetchError::TooLarge)));
        assert_eq!(f.fetch(&format!("http://local.test:{port}/txt")).await.unwrap().body, "plain text");
    }

    #[tokio::test]
    async fn too_many_redirects_stop() {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        let app = axum::Router::new().route("/loop", axum::routing::get(|| async { redirect("/loop") }));
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        let r = local_fetcher(port).fetch(&format!("http://local.test:{port}/loop")).await;
        assert_eq!(r.err(), Some(FetchError::TooManyRedirects));
    }
}
