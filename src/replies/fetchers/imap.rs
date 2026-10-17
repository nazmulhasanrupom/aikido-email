//! IMAP reader (`async-imap`). Read only:
//! - folders are opened with `EXAMINE`, never `SELECT`, so no flag can change;
//! - bodies are fetched with `BODY.PEEK`, never `BODY[]`, which would mark the mail as read.
//! Port 993 = implicit TLS. Port 143 = STARTTLS. No encryption only for `localhost`.
use super::*;
use crate::accounts::model::{PasswordSecrets, TlsMode};
use crate::sending::senders::smtp::is_local_host;
use async_imap::{Client, Session};
use chrono::TimeZone;
use futures::TryStreamExt;
use rustls_pki_types::ServerName;
use secrecy::{ExposeSecret, SecretString};
use std::collections::BTreeSet;
use std::fmt::Debug;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_rustls::{TlsConnector, client::TlsStream};

/// One IMAP command may take this long.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(60);
/// Names to try when the server does not mark its spam folder with `\Junk`.
const SPAM_FALLBACKS: &[&str] = &["Spam", "Junk", "Junk E-mail", "[Gmail]/Spam"];
/// UIDs per `FETCH` command.
const FETCH_CHUNK: usize = 50;

#[derive(Clone)]
pub enum ImapAuth {
    Password { username: String, password: SecretString },
    /// OAuth access token (`AUTHENTICATE XOAUTH2`).
    XOauth2 { username: String, token: SecretString },
}

struct XOauth2 {
    response: String,
}

impl async_imap::Authenticator for &XOauth2 {
    type Response = String;
    fn process(&mut self, _challenge: &[u8]) -> Self::Response {
        self.response.clone()
    }
}

enum Conn {
    Tls(Session<TlsStream<TcpStream>>),
    Plain(Session<TcpStream>),
}

struct State {
    conn: Option<Conn>,
    /// The mailbox open right now, and its `UIDVALIDITY` and `UIDNEXT`.
    open: Option<(String, u32, u32)>,
    /// The spam folder name, once found.
    spam: Option<Option<String>>,
}

pub struct ImapFetcher {
    host: String,
    port: u16,
    tls: TlsMode,
    auth: ImapAuth,
    state: tokio::sync::Mutex<State>,
}

fn tls_connector() -> TlsConnector {
    let roots = rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    TlsConnector::from(Arc::new(rustls::ClientConfig::builder().with_root_certificates(roots).with_no_client_auth()))
}

/// IMAP errors to `FetchError`. Never includes a password.
fn map_error(e: async_imap::error::Error) -> FetchError {
    use async_imap::error::Error as E;
    match e {
        E::No(m) | E::Bad(m) => FetchError::Transient(clip(&m)),
        other => FetchError::Transient(clip(&other.to_string())),
    }
}

async fn cmd<T>(fut: impl std::future::Future<Output = Result<T, async_imap::error::Error>>) -> Result<T, FetchError> {
    match tokio::time::timeout(COMMAND_TIMEOUT, fut).await {
        Ok(r) => r.map_err(map_error),
        Err(_) => Err(FetchError::Transient("the IMAP server did not answer in 60 seconds".into())),
    }
}

/// Like `cmd`, for calls that fail with an I/O error (the server greeting).
async fn io_cmd<T>(fut: impl std::future::Future<Output = std::io::Result<T>>) -> Result<T, FetchError> {
    match tokio::time::timeout(COMMAND_TIMEOUT, fut).await {
        Ok(r) => r.map_err(|e| FetchError::Transient(clip(&e.to_string()))),
        Err(_) => Err(FetchError::Transient("the IMAP server did not answer in 60 seconds".into())),
    }
}

impl ImapFetcher {
    pub fn new(host: String, port: u16, tls: TlsMode, auth: ImapAuth) -> Result<Self, FetchError> {
        if tls == TlsMode::Plain && !is_local_host(&host) {
            return Err(FetchError::AuthFailed("IMAP without encryption is allowed only for localhost".into()));
        }
        Ok(Self { host, port, tls, auth, state: tokio::sync::Mutex::new(State { conn: None, open: None, spam: None }) })
    }

    pub fn from_account(account: &EmailAccount, p: PasswordSecrets) -> Result<Self, FetchError> {
        let (Some(host), Some(port), Some(tls)) = (account.imap_host.clone(), account.imap_port, account.imap_tls) else {
            return Err(FetchError::ScopeMissing);
        };
        let username = p.imap_username.unwrap_or_else(|| account.email.clone());
        let password = p.imap_password.ok_or_else(|| FetchError::AuthFailed("no IMAP password is saved".into()))?;
        Self::new(host, port as u16, tls, ImapAuth::Password { username, password })
    }

    async fn login<T>(&self, client: Client<T>) -> Result<Session<T>, FetchError>
    where
        T: AsyncRead + AsyncWrite + Unpin + Debug + Send,
    {
        match &self.auth {
            ImapAuth::Password { username, password } => {
                match tokio::time::timeout(COMMAND_TIMEOUT, client.login(username, password.expose_secret())).await {
                    Ok(Ok(s)) => Ok(s),
                    Ok(Err((e, _))) => Err(FetchError::AuthFailed(clip(&e.to_string()))),
                    Err(_) => Err(FetchError::Transient("the IMAP server did not answer in 60 seconds".into())),
                }
            }
            ImapAuth::XOauth2 { username, token } => {
                let a = XOauth2 { response: format!("user={username}\x01auth=Bearer {}\x01\x01", token.expose_secret()) };
                match tokio::time::timeout(COMMAND_TIMEOUT, client.authenticate("XOAUTH2", &a)).await {
                    Ok(Ok(s)) => Ok(s),
                    Ok(Err((e, _))) => Err(FetchError::AuthFailed(clip(&e.to_string()))),
                    Err(_) => Err(FetchError::Transient("the IMAP server did not answer in 60 seconds".into())),
                }
            }
        }
    }

    async fn connect(&self) -> Result<Conn, FetchError> {
        let transient = |e: std::io::Error| FetchError::Transient(clip(&format!("cannot connect to the IMAP server: {e}")));
        let tcp = tokio::time::timeout(COMMAND_TIMEOUT, TcpStream::connect((self.host.as_str(), self.port)))
            .await
            .map_err(|_| FetchError::Transient("the IMAP server did not answer in 60 seconds".into()))?
            .map_err(transient)?;
        let name = ServerName::try_from(self.host.clone()).map_err(|_| FetchError::Transient("bad IMAP host name".into()))?;
        match self.tls {
            TlsMode::Implicit => {
                let stream = tls_connector().connect(name, tcp).await.map_err(transient)?;
                let mut client = Client::new(stream);
                io_cmd(client.read_response()).await?;
                Ok(Conn::Tls(self.login(client).await?))
            }
            TlsMode::Starttls => {
                let mut client = Client::new(tcp);
                io_cmd(client.read_response()).await?;
                cmd(client.run_command_and_check_ok("STARTTLS", None)).await?;
                let stream = tls_connector().connect(name, client.into_inner()).await.map_err(transient)?;
                Ok(Conn::Tls(self.login(Client::new(stream)).await?))
            }
            TlsMode::Plain => {
                let mut client = Client::new(tcp);
                io_cmd(client.read_response()).await?;
                Ok(Conn::Plain(self.login(client).await?))
            }
        }
    }
}

macro_rules! session {
    ($conn:expr, $s:ident => $body:expr) => {
        match $conn {
            Conn::Tls($s) => $body,
            Conn::Plain($s) => $body,
        }
    };
}

/// `EXAMINE` a mailbox: read only. Returns `(uidvalidity, uidnext)`.
async fn examine<T>(s: &mut Session<T>, name: &str) -> Result<(u32, u32), FetchError>
where
    T: AsyncRead + AsyncWrite + Unpin + Debug + Send,
{
    let mb = cmd(s.examine(name)).await?;
    Ok((mb.uid_validity.unwrap_or(0), mb.uid_next.unwrap_or(1)))
}

async fn find_spam_folder<T>(s: &mut Session<T>) -> Result<Option<String>, FetchError>
where
    T: AsyncRead + AsyncWrite + Unpin + Debug + Send,
{
    let names: Vec<(String, bool)> = {
        let stream = cmd(s.list(Some(""), Some("*"))).await?;
        let names: Vec<_> = stream.try_collect().await.map_err(map_error)?;
        names
            .iter()
            .map(|n| (n.name().to_string(), n.attributes().iter().any(|a| format!("{a:?}").contains("Junk"))))
            .collect()
    };
    if let Some((name, _)) = names.iter().find(|(_, junk)| *junk) {
        return Ok(Some(name.clone()));
    }
    for fallback in SPAM_FALLBACKS {
        if let Some((name, _)) = names.iter().find(|(n, _)| n.eq_ignore_ascii_case(fallback)) {
            return Ok(Some(name.clone()));
        }
    }
    Ok(None)
}

/// IMAP date for `SEARCH SINCE`: `17-Oct-2026`.
fn imap_date(t: DateTime<Utc>) -> String {
    t.format("%d-%b-%Y").to_string()
}

fn uid_set(uids: &[u32]) -> String {
    uids.iter().map(u32::to_string).collect::<Vec<_>>().join(",")
}

impl ImapFetcher {
    /// Open the connection if needed, and `EXAMINE` the folder if it is not open. `None` = the folder does not exist.
    async fn open(&self, st: &mut State, folder: Folder) -> Result<Option<(String, u32, u32)>, FetchError> {
        if st.conn.is_none() {
            st.conn = Some(self.connect().await?);
            st.open = None;
        }
        let conn = st.conn.as_mut().expect("just connected");
        let name = match folder {
            Folder::Inbox | Folder::All => "INBOX".to_string(),
            Folder::Spam => {
                if st.spam.is_none() {
                    st.spam = Some(session!(conn, s => find_spam_folder(s).await)?);
                }
                match st.spam.clone().flatten() {
                    Some(n) => n,
                    None => return Ok(None),
                }
            }
        };
        if let Some(open) = &st.open {
            if open.0 == name {
                return Ok(Some(open.clone()));
            }
        }
        let (validity, next) = session!(conn, s => examine(s, &name).await)?;
        st.open = Some((name.clone(), validity, next));
        Ok(st.open.clone())
    }
}

async fn search<T>(s: &mut Session<T>, query: &str) -> Result<BTreeSet<u32>, FetchError>
where
    T: AsyncRead + AsyncWrite + Unpin + Debug + Send,
{
    Ok(cmd(s.uid_search(query)).await?.into_iter().collect())
}

#[async_trait]
impl MailFetcher for ImapFetcher {
    fn initial_cursor(&self, since: DateTime<Utc>) -> Cursor {
        serde_json::json!({ "since": since.to_rfc3339() })
    }

    fn cursor_after(&self, listing_cursor: &Cursor, last_done: &RemoteHeaders) -> Cursor {
        match (last_done.id.parse::<u32>().ok(), listing_cursor["uidvalidity"].as_u64()) {
            (Some(uid), Some(validity)) => serde_json::json!({ "uidvalidity": validity, "last_uid": uid }),
            _ => listing_cursor.clone(),
        }
    }

    async fn list_new(&self, folder: Folder, cursor: Option<&Cursor>, limit: usize) -> Result<Listing, FetchError> {
        let mut st = self.state.lock().await;
        let Some((_, validity, uid_next)) = self.open(&mut st, folder).await? else {
            // No spam folder on this server: nothing to read.
            return Ok(Listing { ids: vec![], next_cursor: cursor.cloned().unwrap_or(serde_json::json!({})), more: false });
        };
        let conn = st.conn.as_mut().expect("open");
        let (mut uids, from_search): (Vec<u32>, bool) = match cursor {
            Some(c) if c["uidvalidity"].is_u64() => {
                if c["uidvalidity"].as_u64() != Some(validity as u64) {
                    return Err(FetchError::CursorInvalid);
                }
                let last = c["last_uid"].as_u64().unwrap_or(0) as u32;
                // `n:*` always returns the last message, even when nothing is new: drop every UID that is not newer.
                let found = session!(conn, s => search(s, &format!("UID {}:*", last + 1)).await)?;
                (found.into_iter().filter(|u| *u > last).collect(), false)
            }
            other => {
                let since = other
                    .and_then(|c| c["since"].as_str())
                    .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
                    .map(|t| t.with_timezone(&Utc))
                    .unwrap_or_else(|| Utc::now() - chrono::Duration::days(14));
                let found = session!(conn, s => search(s, &format!("SINCE {}", imap_date(since))).await)?;
                (found.into_iter().collect(), true)
            }
        };
        uids.sort_unstable();
        let more = uids.len() > limit;
        uids.truncate(limit);
        let last_uid = match uids.last() {
            Some(u) => *u,
            // Nothing in this search: continue after the newest message of the folder.
            None if from_search => uid_next.saturating_sub(1),
            None => cursor.and_then(|c| c["last_uid"].as_u64()).unwrap_or(0) as u32,
        };
        Ok(Listing {
            ids: uids.iter().map(u32::to_string).collect(),
            next_cursor: serde_json::json!({ "uidvalidity": validity, "last_uid": last_uid }),
            more,
        })
    }

    async fn headers(&self, folder: Folder, ids: &[RemoteId]) -> Result<Vec<RemoteHeaders>, FetchError> {
        let mut st = self.state.lock().await;
        if self.open(&mut st, folder).await?.is_none() {
            return Ok(vec![]);
        }
        let conn = st.conn.as_mut().expect("open");
        let uids: Vec<u32> = ids.iter().filter_map(|i| i.parse().ok()).collect();
        let mut out = Vec::new();
        for chunk in uids.chunks(FETCH_CHUNK) {
            let set = uid_set(chunk);
            let fetched = session!(conn, s => async {
                let stream = cmd(s.uid_fetch(&set, "(UID INTERNALDATE BODY.PEEK[HEADER])")).await?;
                let v: Vec<_> = stream.try_collect().await.map_err(map_error)?;
                Ok::<_, FetchError>(v.into_iter().map(|f| (f.uid, f.internal_date(), f.header().map(<[u8]>::to_vec))).collect::<Vec<_>>())
            }.await)?;
            for (uid, date, header) in fetched {
                let (Some(uid), Some(header)) = (uid, header) else { continue };
                out.push(RemoteHeaders {
                    id: uid.to_string(),
                    thread_id: None,
                    received_at: date.map(|d| Utc.timestamp_opt(d.timestamp(), 0).single().unwrap_or_default()),
                    headers: crate::replies::parse::parse_header_block(&header),
                    labels: vec![],
                });
            }
        }
        Ok(out)
    }

    async fn raw(&self, folder: Folder, id: &RemoteId, max_bytes: usize) -> Result<RawEmail, FetchError> {
        let mut st = self.state.lock().await;
        if self.open(&mut st, folder).await?.is_none() {
            return Err(FetchError::NotFound);
        }
        let conn = st.conn.as_mut().expect("open");
        let uid: u32 = id.parse().map_err(|_| FetchError::NotFound)?;
        // The size first: an over-size mail is not downloaded.
        let size = session!(conn, s => async {
            let stream = cmd(s.uid_fetch(uid.to_string(), "(UID RFC822.SIZE)")).await?;
            let v: Vec<_> = stream.try_collect().await.map_err(map_error)?;
            Ok::<_, FetchError>(v.first().and_then(|f| f.size))
        }.await)?;
        let Some(size) = size else { return Err(FetchError::NotFound) };
        let truncated = size as usize > max_bytes;
        let query = if truncated { "(BODY.PEEK[HEADER])" } else { "(BODY.PEEK[])" };
        let bytes = session!(conn, s => async {
            let stream = cmd(s.uid_fetch(uid.to_string(), query)).await?;
            let v: Vec<_> = stream.try_collect().await.map_err(map_error)?;
            Ok::<_, FetchError>(v.first().and_then(|f| if truncated { f.header() } else { f.body() }).map(<[u8]>::to_vec))
        }.await)?;
        bytes.map(|bytes| RawEmail { bytes, truncated }).ok_or(FetchError::NotFound)
    }

    async fn close(&self) {
        let mut st = self.state.lock().await;
        st.open = None;
        if let Some(conn) = st.conn.take() {
            // LOGOUT. A failure here does not matter: the connection is dropped anyway.
            let _ = tokio::time::timeout(Duration::from_secs(10), async {
                match conn {
                    Conn::Tls(mut s) => s.logout().await.ok(),
                    Conn::Plain(mut s) => s.logout().await.ok(),
                }
            })
            .await;
        }
    }
}
