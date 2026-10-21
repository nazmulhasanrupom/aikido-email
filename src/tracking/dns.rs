//! The CNAME check of a tracking domain. Tests replace the resolver with `FakeCname` (`AppState::cname`).
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone, Debug, PartialEq)]
pub enum CnameAnswer {
    /// The CNAME targets of the name (lower case, no dot at the end).
    Targets(Vec<String>),
    /// The name has no CNAME (or does not exist).
    None,
    /// Timeout or DNS error: we do not know.
    Unknown,
}

#[async_trait]
pub trait CnameResolver: Send + Sync {
    async fn cname(&self, name: &str) -> CnameAnswer;
}

const TIMEOUT: Duration = Duration::from_secs(5);

/// The real resolver: the system DNS settings. Made on first use.
#[derive(Default)]
pub struct SystemCname {
    inner: tokio::sync::OnceCell<Option<hickory_resolver::TokioResolver>>,
}

#[async_trait]
impl CnameResolver for SystemCname {
    async fn cname(&self, name: &str) -> CnameAnswer {
        let resolver = self
            .inner
            .get_or_init(|| async { hickory_resolver::Resolver::builder_tokio().and_then(|b| b.build()).ok() })
            .await;
        let Some(resolver) = resolver else { return CnameAnswer::Unknown };
        let fqdn = format!("{name}.");
        match tokio::time::timeout(TIMEOUT, resolver.lookup(fqdn.as_str(), hickory_resolver::proto::rr::RecordType::CNAME)).await {
            Ok(Ok(found)) => {
                let targets: Vec<String> = found
                    .answers()
                    .iter()
                    .filter_map(|r| match &r.data {
                        hickory_resolver::proto::rr::RData::CNAME(c) => Some(normalize(&c.0.to_utf8())),
                        _ => None,
                    })
                    .collect();
                if targets.is_empty() { CnameAnswer::None } else { CnameAnswer::Targets(targets) }
            }
            Ok(Err(e)) if e.is_nx_domain() || e.is_no_records_found() => CnameAnswer::None,
            _ => CnameAnswer::Unknown,
        }
    }
}

/// Lower case, no dot at the end.
pub fn normalize(host: &str) -> String {
    host.trim().trim_end_matches('.').to_ascii_lowercase()
}

/// A resolver for tests: answers from a table.
#[derive(Default)]
pub struct FakeCname {
    table: Mutex<HashMap<String, CnameAnswer>>,
}

impl FakeCname {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
    pub fn set(&self, name: &str, target: &str) {
        self.table.lock().unwrap().insert(name.to_string(), CnameAnswer::Targets(vec![normalize(target)]));
    }
    pub fn remove(&self, name: &str) {
        self.table.lock().unwrap().remove(name);
    }
    pub fn fail(&self, name: &str) {
        self.table.lock().unwrap().insert(name.to_string(), CnameAnswer::Unknown);
    }
}

#[async_trait]
impl CnameResolver for FakeCname {
    async fn cname(&self, name: &str) -> CnameAnswer {
        self.table.lock().unwrap().get(name).cloned().unwrap_or(CnameAnswer::None)
    }
}
