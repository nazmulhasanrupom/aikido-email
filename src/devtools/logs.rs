//! A tracing layer that keeps the last events in memory, for the developer panel.
//!
//! Every event the server writes (to the terminal or not) is kept in a ring of 5000 entries with a growing id, so the
//! panel can ask for "everything after id N". The ring has its own filter that can be changed while the server runs
//! (`PUT /v1/dev/log-filter`), so the panel can show `debug` lines without touching `RUST_LOG`.
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};
use utoipa::ToSchema;

pub const CAPACITY: usize = 5000;
/// The first filter of the panel: our own code at `debug`, everything else at `info`, the noisy crates at `warn`.
pub const DEFAULT_FILTER: &str = "info,aikido_email=debug,sqlx=warn,hyper=warn,rustls=warn,hickory_proto=warn,hickory_resolver=warn,tower_http=warn";

#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct LogEntry {
    pub id: u64,
    pub at: DateTime<Utc>,
    /// `ERROR`, `WARN`, `INFO`, `DEBUG` or `TRACE`.
    pub level: String,
    pub target: String,
    pub message: String,
    /// The other fields of the event: `key=value key=value`.
    pub fields: String,
    /// The request or job the event happened in, for example `http{method=POST path=/v1/campaigns}`.
    pub span: Option<String>,
}

pub fn rank(level: &str) -> u8 {
    match level.to_ascii_uppercase().as_str() {
        "ERROR" => 4,
        "WARN" => 3,
        "INFO" => 2,
        "DEBUG" => 1,
        _ => 0,
    }
}

#[derive(Default)]
struct Inner {
    next_id: u64,
    entries: VecDeque<LogEntry>,
}

pub struct LogBuffer {
    inner: Mutex<Inner>,
    capacity: usize,
}

impl LogBuffer {
    pub fn new(capacity: usize) -> Self {
        Self { inner: Mutex::new(Inner { next_id: 1, entries: VecDeque::new() }), capacity }
    }

    pub fn push(&self, level: &str, target: &str, message: String, fields: String, span: Option<String>) {
        let Ok(mut g) = self.inner.lock() else { return };
        let id = g.next_id;
        g.next_id += 1;
        if g.entries.len() >= self.capacity {
            g.entries.pop_front();
        }
        g.entries.push_back(LogEntry { id, at: Utc::now(), level: level.to_string(), target: target.to_string(), message, fields, span });
    }

    /// Entries after `after` (0 = from the start), oldest first. When `after` is 0 and there are more than `limit`, the newest `limit`.
    pub fn since(&self, after: u64, limit: usize, min_level: Option<&str>, q: Option<&str>) -> (Vec<LogEntry>, u64) {
        let Ok(g) = self.inner.lock() else { return (Vec::new(), after) };
        let last = g.next_id.saturating_sub(1);
        let min = min_level.map(rank).unwrap_or(0);
        let q = q.map(str::to_ascii_lowercase).filter(|q| !q.is_empty());
        let mut out: Vec<LogEntry> = g
            .entries
            .iter()
            .rev()
            .take_while(|e| e.id > after)
            .filter(|e| rank(&e.level) >= min)
            .filter(|e| {
                q.as_ref().is_none_or(|q| {
                    e.message.to_ascii_lowercase().contains(q) || e.fields.to_ascii_lowercase().contains(q) || e.target.to_ascii_lowercase().contains(q) || e.span.as_deref().is_some_and(|s| s.to_ascii_lowercase().contains(q))
                })
            })
            .take(limit)
            .cloned()
            .collect();
        out.reverse();
        (out, last)
    }

    pub fn len(&self) -> usize {
        self.inner.lock().map(|g| g.entries.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn clear(&self) {
        if let Ok(mut g) = self.inner.lock() {
            g.entries.clear();
        }
    }
}

/// What the routes use: the ring, the filter of the ring, and the time the server started.
pub struct Dev {
    pub logs: Arc<LogBuffer>,
    filter: Mutex<String>,
    set_filter: Box<dyn Fn(&str) -> Result<(), String> + Send + Sync>,
    pub started: DateTime<Utc>,
}

impl Dev {
    /// No tracing layer is attached (tests, CLI commands): the ring stays empty and the filter can not change.
    pub fn detached() -> Arc<Self> {
        Arc::new(Self {
            logs: Arc::new(LogBuffer::new(CAPACITY)),
            filter: Mutex::new(DEFAULT_FILTER.to_string()),
            set_filter: Box::new(|_| Ok(())),
            started: Utc::now(),
        })
    }

    pub fn filter(&self) -> String {
        self.filter.lock().map(|f| f.clone()).unwrap_or_default()
    }

    /// Change the filter of the ring while the server runs. A bad filter is refused and nothing changes.
    pub fn set_filter(&self, f: &str) -> Result<(), String> {
        EnvFilter::try_new(f).map_err(|e| e.to_string())?;
        (self.set_filter)(f)?;
        if let Ok(mut cur) = self.filter.lock() {
            *cur = f.to_string();
        }
        Ok(())
    }
}

/// Start logging: the terminal (as before, `RUST_LOG`) and the ring for the panel.
pub fn init_logging() -> Arc<Dev> {
    let buffer = Arc::new(LogBuffer::new(CAPACITY));
    let terminal = EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    let (ring_filter, handle) = tracing_subscriber::reload::Layer::new(EnvFilter::new(DEFAULT_FILTER));
    let ring = RingLayer { buffer: buffer.clone() }.with_filter(ring_filter);
    tracing_subscriber::registry().with(tracing_subscriber::fmt::layer().with_filter(terminal)).with(ring).init();
    Arc::new(Dev {
        logs: buffer,
        filter: Mutex::new(DEFAULT_FILTER.to_string()),
        set_filter: Box::new(move |f| handle.reload(EnvFilter::try_new(f).map_err(|e| e.to_string())?).map_err(|e| e.to_string())),
        started: Utc::now(),
    })
}

struct RingLayer {
    buffer: Arc<LogBuffer>,
}

/// The fields of a span, saved on the span.
struct SpanFields(String);

#[derive(Default)]
struct Fields {
    message: String,
    rest: Vec<String>,
}

impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        } else {
            self.rest.push(format!("{}={}", field.name(), value));
        }
    }
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        } else {
            self.rest.push(format!("{}={:?}", field.name(), value));
        }
    }
}

impl<S> Layer<S> for RingLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &tracing::span::Attributes<'_>, id: &tracing::span::Id, ctx: Context<'_, S>) {
        let mut f = Fields::default();
        attrs.record(&mut f);
        if let Some(span) = ctx.span(id) {
            let text = if f.rest.is_empty() { span.name().to_string() } else { format!("{}{{{}}}", span.name(), f.rest.join(" ")) };
            span.extensions_mut().insert(SpanFields(text));
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let mut f = Fields::default();
        event.record(&mut f);
        let meta = event.metadata();
        let span = ctx.event_scope(event).map(|scope| {
            scope.from_root().filter_map(|s| s.extensions().get::<SpanFields>().map(|f| f.0.clone())).collect::<Vec<_>>().join(" > ")
        });
        let level = match *meta.level() {
            Level::ERROR => "ERROR",
            Level::WARN => "WARN",
            Level::INFO => "INFO",
            Level::DEBUG => "DEBUG",
            Level::TRACE => "TRACE",
        };
        self.buffer.push(level, meta.target(), f.message, f.rest.join(" "), span.filter(|s| !s.is_empty()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf() -> LogBuffer {
        let b = LogBuffer::new(3);
        b.push("INFO", "a", "one".into(), String::new(), None);
        b.push("WARN", "b", "two".into(), "job=7".into(), Some("http{path=/x}".into()));
        b.push("ERROR", "c", "three".into(), String::new(), None);
        b
    }

    #[test]
    fn the_ring_keeps_the_newest_entries() {
        let b = buf();
        b.push("INFO", "d", "four".into(), String::new(), None);
        let (all, last) = b.since(0, 10, None, None);
        assert_eq!(all.iter().map(|e| e.message.as_str()).collect::<Vec<_>>(), ["two", "three", "four"]);
        assert_eq!(last, 4);
    }

    #[test]
    fn since_returns_only_new_entries_and_filters() {
        let b = buf();
        let (new, _) = b.since(2, 10, None, None);
        assert_eq!(new.len(), 1);
        assert_eq!(new[0].message, "three");
        let (warn_up, _) = b.since(0, 10, Some("warn"), None);
        assert_eq!(warn_up.len(), 2);
        let (found, _) = b.since(0, 10, None, Some("JOB=7"));
        assert_eq!(found.len(), 1);
        let (by_span, _) = b.since(0, 10, None, Some("/x}"));
        assert_eq!(by_span[0].message, "two");
        // limit keeps the newest
        let (newest, _) = b.since(0, 1, None, None);
        assert_eq!(newest[0].message, "three");
    }

    #[test]
    fn a_bad_filter_is_refused() {
        let dev = Dev::detached();
        assert!(dev.set_filter("info,aikido_email=debug").is_ok());
        assert_eq!(dev.filter(), "info,aikido_email=debug");
        assert!(dev.set_filter("info,x=notalevel").is_err());
        assert_eq!(dev.filter(), "info,aikido_email=debug");
    }

    #[test]
    fn the_layer_captures_message_fields_and_spans() {
        let dev = Dev::detached();
        let sub = tracing_subscriber::registry().with(RingLayer { buffer: dev.logs.clone() });
        tracing::subscriber::with_default(sub, || {
            let span = tracing::info_span!("http", method = "POST", path = "/v1/x");
            let _g = span.enter();
            tracing::warn!(job_id = 5, error = %"boom", "send failed");
        });
        let (e, _) = dev.logs.since(0, 10, None, None);
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].level, "WARN");
        assert_eq!(e[0].message, "send failed");
        assert!(e[0].fields.contains("job_id=5") && e[0].fields.contains("error=boom"), "{:?}", e[0]);
        assert_eq!(e[0].span.as_deref(), Some("http{method=POST path=/v1/x}"));
    }
}
