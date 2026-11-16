//! Signing in with Google, the way Google documents it for desktop apps: the browser
//! opens Google's own page, and Google sends the answer back to a one-time address on
//! this computer (`http://127.0.0.1:<port>`). PKCE ties the answer to this sign-in, and a
//! random `state` is checked on return. Featherplay never sees the password.
//!
//! Access is view-only (`youtube.readonly`), and only read requests (`list`) are made.
//! Everything goes to Google over HTTPS; there is no Featherplay server.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read as _, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use ring::rand::SecureRandom;

/// The only access Featherplay asks for: to view the YouTube account.
pub const SCOPE: &str = "https://www.googleapis.com/auth/youtube.readonly";
/// How long the browser has to come back before the sign-in gives up.
pub const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// The app's entry at Google (a "Desktop app" OAuth client). For desktop apps the secret
/// isn't confidential, Google says: it ships inside the app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Client {
    pub id: String,
    pub secret: String,
}

/// Featherplay's own client, in its Google project. The ID is public (every sign-in link
/// shows it). The secret, which Google says isn't confidential for desktop apps, still
/// stays out of the repository, where secret scanning would flag it: it is given at build
/// time (`FEATHERPLAY_GOOGLE_SECRET`; a repository secret for releases). A build without it
/// offers sign-in only with a client of one's own, from settings.json.
const BUILT_IN_ID: &str = "739361184497-vqmit8loikt9isgfjl8r8264c2um8l12.apps.googleusercontent.com";
const BUILT_IN_SECRET: &str = match option_env!("FEATHERPLAY_GOOGLE_SECRET") {
    Some(secret) => secret,
    None => "",
};

impl Client {
    /// The client to sign in with: one's own if settings give one, else Featherplay's.
    pub fn choose(own_id: Option<&str>, own_secret: Option<&str>) -> Option<Self> {
        match own_id.map(str::trim).filter(|id| !id.is_empty()) {
            Some(id) => Some(Self { id: id.into(), secret: own_secret.unwrap_or_default().trim().into() }),
            None if !BUILT_IN_SECRET.is_empty() => {
                Some(Self { id: BUILT_IN_ID.into(), secret: BUILT_IN_SECRET.into() })
            }
            None => None,
        }
    }
}

/// Where Google is: its real addresses, or a stand-in for tests.
#[derive(Clone, Debug)]
pub struct Google {
    auth: String,
    token: String,
    revoke: String,
    api: String,
}

impl Google {
    pub fn real() -> Self {
        Self {
            auth: "https://accounts.google.com/o/oauth2/v2/auth".into(),
            token: "https://oauth2.googleapis.com/token".into(),
            revoke: "https://oauth2.googleapis.com/revoke".into(),
            api: "https://www.googleapis.com".into(),
        }
    }

    /// Everything at one base address, as a test server serves it.
    pub fn at(base: &str) -> Self {
        let base = base.trim_end_matches('/');
        Self {
            auth: format!("{base}/auth"),
            token: format!("{base}/token"),
            revoke: format!("{base}/revoke"),
            api: base.to_string(),
        }
    }

    /// The real Google, unless `FEATHERPLAY_GOOGLE_URL` points somewhere else (for testing).
    pub fn from_env() -> Self {
        std::env::var("FEATHERPLAY_GOOGLE_URL").map_or_else(|_| Self::real(), |base| Self::at(&base))
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    /// The person cancelled in Featherplay.
    Cancelled,
    /// The browser never came back.
    TimedOut,
    /// The person declined on Google's page.
    Declined,
    /// The sign-in is no longer valid: revoked from the Google account, or expired.
    Revoked,
    /// The request didn't get through (offline, say).
    Network(String),
    /// Google's daily allowance for the app, shared by everyone, is used up.
    Quota,
    /// Google answered, but not with what was asked for.
    Refused(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Cancelled => write!(f, "cancelled"),
            Error::TimedOut => write!(f, "the browser didn't come back in time"),
            Error::Declined => write!(f, "declined on Google's page"),
            Error::Revoked => write!(f, "the sign-in is no longer valid"),
            Error::Network(e) => write!(f, "network: {e}"),
            Error::Quota => write!(f, "YouTube's daily limit for the app is reached"),
            Error::Refused(e) => write!(f, "refused: {e}"),
        }
    }
}

/// What a sign-in leaves: the long-lived refresh token, and a first access token.
#[derive(Debug)]
pub struct Tokens {
    pub refresh: String,
    pub access: String,
    pub lasts: Duration,
}

/// A sign-in waiting for the browser.
pub struct SignIn {
    listener: TcpListener,
    redirect: String,
    state: String,
    verifier: String,
    /// Google's sign-in page for this sign-in: what the browser opens.
    pub url: String,
    client: Client,
    google: Google,
}

impl SignIn {
    /// Listens on a free port of 127.0.0.1 (never any other address) and makes the link.
    pub fn start(client: &Client, google: &Google) -> std::io::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let port = listener.local_addr()?.port();
        let redirect = format!("http://127.0.0.1:{port}");
        let state = random_text(16);
        let verifier = random_text(32);
        let url = format!(
            "{}?{}",
            google.auth,
            query(&[
                ("client_id", &client.id),
                ("redirect_uri", &redirect),
                ("response_type", "code"),
                ("scope", SCOPE),
                ("code_challenge", &challenge(&verifier)),
                ("code_challenge_method", "S256"),
                ("state", &state),
                // A refresh token, so the sign-in lasts beyond the hour.
                ("access_type", "offline"),
                ("prompt", "consent"),
            ])
        );
        Ok(Self { listener, redirect, state, verifier, url, client: client.clone(), google: google.clone() })
    }

    pub fn port(&self) -> u16 {
        self.listener.local_addr().map_or(0, |a| a.port())
    }

    /// Waits for the browser's answer, then trades it for tokens. Stops at `cancel` or
    /// after `timeout`; the port closes either way, as the listener is dropped.
    pub fn finish(self, cancel: &AtomicBool, timeout: Duration) -> Result<Tokens, Error> {
        self.listener.set_nonblocking(true).map_err(|e| Error::Network(e.to_string()))?;
        let started = Instant::now();
        loop {
            if cancel.load(Ordering::SeqCst) {
                return Err(Error::Cancelled);
            }
            if started.elapsed() >= timeout {
                return Err(Error::TimedOut);
            }
            let stream = match self.listener.accept() {
                Ok((stream, _)) => stream,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(50));
                    continue;
                }
                Err(e) => return Err(Error::Network(e.to_string())),
            };
            let Some(answer) = read_answer(&stream) else {
                reply(stream, 404, "Not found", "");
                continue;
            };
            // An answer for some other sign-in (an old tab, or anyone else) is refused, and
            // the wait goes on.
            if answer.get("state") != Some(&self.state) {
                reply(stream, 400, "This sign-in link is out of date", "Start again from Featherplay.");
                continue;
            }
            if let Some(code) = answer.get("code") {
                let tokens = exchange(&self.google, &self.client, code, &self.verifier, &self.redirect);
                match &tokens {
                    Ok(_) => reply(stream, 200, "Signed in to Featherplay", "You can close this tab."),
                    Err(_) => reply(stream, 200, "Couldn't sign in", "Featherplay says what went wrong."),
                }
                return tokens;
            }
            reply(stream, 200, "Sign-in cancelled", "You can close this tab.");
            return Err(Error::Declined);
        }
    }
}

/// The query of the one request the browser makes back, if it is to the address's root.
fn read_answer(stream: &TcpStream) -> Option<HashMap<String, String>> {
    stream.set_nonblocking(false).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    let mut line = String::new();
    BufReader::new(stream).take(8192).read_line(&mut line).ok()?;
    let target = line.strip_prefix("GET ")?.split(' ').next()?;
    let query = target.strip_prefix("/?")?;
    Some(query.split('&').filter_map(|pair| pair.split_once('=')).map(|(k, v)| (decode(k), decode(v))).collect())
}

/// A small page for the browser tab, in the app's colours.
fn reply(mut stream: TcpStream, status: u16, title: &str, detail: &str) {
    let page = format!(
        "<!doctype html><meta charset=utf-8><meta name=viewport content='width=device-width'><title>{title}</title>\
         <style>body{{margin:0;height:100vh;display:grid;place-content:center;text-align:center;\
         font:16px system-ui,sans-serif;background:#f4f4f6;color:#17171c}}h1{{font-size:22px;margin:0 0 8px}}\
         p{{margin:0;color:#5d5d68}}@media(prefers-color-scheme:dark){{body{{background:#111114;color:#efeff3}}\
         p{{color:#a9a9b3}}}}</style><h1>{title}</h1><p>{detail}</p>"
    );
    let head = format!(
        "HTTP/1.1 {status} OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        page.len()
    );
    stream.write_all(head.as_bytes()).ok();
    stream.write_all(page.as_bytes()).ok();
}

#[derive(serde::Deserialize)]
struct TokenAnswer {
    access_token: Option<String>,
    expires_in: Option<u64>,
    refresh_token: Option<String>,
    scope: Option<String>,
    error: Option<String>,
}

fn post_form(url: &str, form: &[(&str, &str)]) -> Result<(u16, String), Error> {
    let mut response = crate::net::agent()
        .post(url)
        .config()
        .http_status_as_error(false)
        .build()
        .send_form(form.iter().copied())
        .map_err(|e| Error::Network(e.to_string()))?;
    let status = response.status().as_u16();
    let body = response.body_mut().with_config().limit(1 << 20).read_to_string().unwrap_or_default();
    Ok((status, body))
}

fn token_answer(status: u16, body: &str) -> Result<TokenAnswer, Error> {
    let answer: TokenAnswer = serde_json::from_str(body).map_err(|_| Error::Refused(format!("HTTP {status}")))?;
    match answer.error.as_deref() {
        Some("invalid_grant") => Err(Error::Revoked),
        Some(e) => Err(Error::Refused(e.to_string())),
        None if status != 200 => Err(Error::Refused(format!("HTTP {status}"))),
        None => Ok(answer),
    }
}

fn exchange(google: &Google, client: &Client, code: &str, verifier: &str, redirect: &str) -> Result<Tokens, Error> {
    let (status, body) = post_form(
        &google.token,
        &[
            ("client_id", &client.id),
            ("client_secret", &client.secret),
            ("code", code),
            ("code_verifier", verifier),
            ("grant_type", "authorization_code"),
            ("redirect_uri", redirect),
        ],
    )?;
    let answer = token_answer(status, &body)?;
    // With granular consent the box can be unticked: then there's nothing to work with.
    if !answer.scope.as_deref().unwrap_or_default().split(' ').any(|s| s == SCOPE) {
        return Err(Error::Refused("YouTube access wasn't granted".into()));
    }
    match (answer.refresh_token, answer.access_token) {
        (Some(refresh), Some(access)) => {
            Ok(Tokens { refresh, access, lasts: Duration::from_secs(answer.expires_in.unwrap_or(3600)) })
        }
        _ => Err(Error::Refused("no token in Google's answer".into())),
    }
}

/// A fresh access token, and how long it lasts. `Error::Revoked` when the sign-in is gone.
pub fn refresh(google: &Google, client: &Client, refresh_token: &str) -> Result<(String, Duration), Error> {
    let (status, body) = post_form(
        &google.token,
        &[
            ("client_id", &client.id),
            ("client_secret", &client.secret),
            ("refresh_token", refresh_token),
            ("grant_type", "refresh_token"),
        ],
    )?;
    let answer = token_answer(status, &body)?;
    let access = answer.access_token.ok_or_else(|| Error::Refused("no token in Google's answer".into()))?;
    Ok((access, Duration::from_secs(answer.expires_in.unwrap_or(3600))))
}

/// Asks Google to end the sign-in (an already invalid token counts as ended).
pub fn revoke(google: &Google, token: &str) -> Result<(), Error> {
    let (status, _) = post_form(&google.revoke, &[("token", token)])?;
    match status {
        200 | 400 => Ok(()),
        status => Err(Error::Refused(format!("HTTP {status}"))),
    }
}

/// The only requests made with the sign-in: reading lists, nothing that changes anything.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Read {
    Channels,
    Playlists,
    PlaylistItems,
    Videos,
}

pub const READS: [Read; 4] = [Read::Channels, Read::Playlists, Read::PlaylistItems, Read::Videos];

impl Read {
    /// The API path; every one of them is a `list`, read with GET.
    pub fn path(self) -> &'static str {
        match self {
            Read::Channels => "/youtube/v3/channels",
            Read::Playlists => "/youtube/v3/playlists",
            Read::PlaylistItems => "/youtube/v3/playlistItems",
            Read::Videos => "/youtube/v3/videos",
        }
    }
}

/// Reads a list. `Error::Revoked` when the access token is refused (expired or revoked).
pub fn get(google: &Google, access: &str, read: Read, params: &[(&str, &str)]) -> Result<serde_json::Value, Error> {
    match get_tagged(google, access, read, params, None)? {
        Tagged::Fresh(value, _) => Ok(value),
        Tagged::Unchanged => Err(Error::Refused("unchanged without a tag".into())),
    }
}

/// A list read with the tag of the copy already held: `Unchanged` if it still is (less
/// data, and faster; Google still counts the request against the quota).
pub enum Tagged {
    Unchanged,
    Fresh(serde_json::Value, Option<String>),
}

pub fn get_tagged(
    google: &Google,
    access: &str,
    read: Read,
    params: &[(&str, &str)],
    etag: Option<&str>,
) -> Result<Tagged, Error> {
    let url = format!("{}{}?{}", google.api, read.path(), query(params));
    let mut request = crate::net::agent().get(&url).header("Authorization", &format!("Bearer {access}"));
    if let Some(etag) = etag {
        request = request.header("If-None-Match", etag);
    }
    let mut response =
        request.config().http_status_as_error(false).build().call().map_err(|e| Error::Network(e.to_string()))?;
    let status = response.status().as_u16();
    let tag = response.headers().get("etag").and_then(|v| v.to_str().ok()).map(String::from);
    let body = response.body_mut().with_config().limit(8 << 20).read_to_string().unwrap_or_default();
    match status {
        304 => Ok(Tagged::Unchanged),
        200 => {
            let value: serde_json::Value = serde_json::from_str(&body).map_err(|e| Error::Refused(e.to_string()))?;
            let tag = tag.or_else(|| value["etag"].as_str().map(String::from));
            Ok(Tagged::Fresh(value, tag))
        }
        401 => Err(Error::Revoked),
        403 if body.contains("quotaExceeded") || body.contains("dailyLimitExceeded") => Err(Error::Quota),
        status => Err(Error::Refused(format!("HTTP {status}: {}", body.chars().take(300).collect::<String>()))),
    }
}

/// The signed-in account's YouTube channel: its name and picture.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Channel {
    pub name: String,
    pub picture: Option<String>,
}

/// The account's channel, or `None` for an account without one.
pub fn my_channel(google: &Google, access: &str) -> Result<Option<Channel>, Error> {
    let answer = get(google, access, Read::Channels, &[("part", "snippet"), ("mine", "true")])?;
    let Some(snippet) = answer["items"].get(0).map(|item| &item["snippet"]) else { return Ok(None) };
    let thumbs = &snippet["thumbnails"];
    let picture = ["medium", "default", "high"].iter().find_map(|k| thumbs[k]["url"].as_str()).map(String::from);
    Ok(Some(Channel { name: snippet["title"].as_str().unwrap_or("YouTube").to_string(), picture }))
}

/// The PKCE challenge for a verifier: its SHA-256, in URL-safe base64 (RFC 7636).
pub fn challenge(verifier: &str) -> String {
    base64url(ring::digest::digest(&ring::digest::SHA256, verifier.as_bytes()).as_ref())
}

/// `bytes` random bytes from the system, as URL-safe text.
fn random_text(bytes: usize) -> String {
    let mut buffer = vec![0u8; bytes];
    ring::rand::SystemRandom::new().fill(&mut buffer).expect("the system's random numbers");
    base64url(&buffer)
}

fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
        for i in 0..=chunk.len() {
            out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

fn query(params: &[(&str, &str)]) -> String {
    params.iter().map(|(k, v)| format!("{}={}", encode(k), encode(v))).collect::<Vec<_>>().join("&")
}

fn encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            b => format!("%{b:02X}"),
        })
        .collect()
}

fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(std::str::from_utf8(h).ok()?, 16).ok());
        match (bytes[i], hex) {
            (b'+', _) => out.push(b' '),
            (b'%', Some(b)) => {
                out.push(b);
                i += 2;
            }
            (b, _) => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A stand-in for Google, on 127.0.0.1, for tests: it answers token, revoke and channel
/// requests the way Google does, and records what it was asked.
#[cfg(test)]
pub mod fake {
    use super::*;
    use std::sync::{Arc, Mutex};

    pub const ACCESS: &str = "ya29.fake-access-token";
    pub const REFRESH: &str = "1//fake-refresh-token";

    #[derive(Default)]
    pub struct Behaviour {
        /// Refresh tokens Google no longer accepts.
        pub revoked: bool,
        /// The account has no YouTube channel.
        pub no_channel: bool,
        /// Grants this scope instead of the one asked for.
        pub scope: Option<String>,
        /// Liked videos, playlists and each playlist's length: (id, title, privacy, length).
        pub liked: usize,
        pub playlists: Vec<(String, String, String, usize)>,
        /// Bumped when the library changes: its lists' ETags change with it.
        pub version: u32,
        /// The daily quota is used up.
        pub quota_out: bool,
    }

    impl Behaviour {
        /// A library like a real one: liked videos, and a private, an unlisted and a public
        /// playlist, one of them long.
        pub fn library() -> Self {
            Self {
                liked: 60,
                playlists: vec![
                    ("PLprivate".into(), "Late night".into(), "private".into(), 12),
                    ("PLunlisted".into(), "Road trip".into(), "unlisted".into(), 3),
                    ("PLpublic".into(), "Everything".into(), "public".into(), 120),
                ],
                ..Self::default()
            }
        }
    }

    /// A video id for the `n`th item of a list: 11 characters, as YouTube's are.
    pub fn video(list: &str, n: usize) -> String {
        format!("{:_<7}{n:04}", &list[..list.len().min(7)])
    }

    /// One page (50 at most) of a list of `total`, from `pageToken`, as YouTube pages them.
    fn page(total: usize, token: Option<&str>) -> (std::ops::Range<usize>, Option<String>) {
        let start = token.and_then(|t| t.strip_prefix("p")).and_then(|n| n.parse().ok()).unwrap_or(0);
        let end = (start + 50).min(total);
        (start..end, (end < total).then(|| format!("p{end}")))
    }

    pub struct Server {
        pub base: String,
        pub requests: Arc<Mutex<Vec<String>>>,
        pub behaviour: Arc<Mutex<Behaviour>>,
    }

    pub fn start() -> Server {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let behaviour = Arc::new(Mutex::new(Behaviour::default()));
        let (log, act) = (requests.clone(), behaviour.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (log, act) = (log.clone(), act.clone());
                std::thread::spawn(move || serve(stream, &log, &act));
            }
        });
        Server { base, requests, behaviour }
    }

    fn serve(stream: TcpStream, log: &Mutex<Vec<String>>, act: &Mutex<Behaviour>) {
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let mut length = 0;
        let mut authorization = String::new();
        let mut if_none_match = String::new();
        loop {
            let mut header = String::new();
            reader.read_line(&mut header).unwrap();
            let header = header.trim_end();
            if header.is_empty() {
                break;
            }
            let lower = header.to_lowercase();
            if let Some(v) = lower.strip_prefix("content-length:") {
                length = v.trim().parse().unwrap_or(0);
            }
            if lower.starts_with("authorization:") {
                authorization = header["authorization:".len()..].trim().to_string();
            }
            if lower.starts_with("if-none-match:") {
                if_none_match = header["if-none-match:".len()..].trim().to_string();
            }
        }
        let mut body = vec![0; length];
        reader.read_exact(&mut body).unwrap();
        let body = String::from_utf8(body).unwrap();
        let mut parts = line.split(' ');
        let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
        log.lock().unwrap().push(format!("{method} {target} {body}"));
        let form: HashMap<String, String> =
            body.split('&').filter_map(|p| p.split_once('=')).map(|(k, v)| (decode(k), decode(v))).collect();
        let behaviour = act.lock().unwrap();
        let params: HashMap<String, String> = target
            .split_once('?')
            .map(|(_, q)| q.split('&').filter_map(|p| p.split_once('=')).map(|(k, v)| (decode(k), decode(v))).collect())
            .unwrap_or_default();
        let path = target.split('?').next().unwrap_or("");
        let etag = format!("\"v{}-{}\"", behaviour.version, target.len());
        let api = path.starts_with("/youtube/v3/") && path != "/youtube/v3/channels";
        let refused = authorization != format!("Bearer {ACCESS}") || behaviour.revoked;
        if api && !refused && !behaviour.quota_out && if_none_match == etag {
            log.lock().unwrap().push(format!("304 {target}"));
            let mut stream = stream;
            write!(
                stream,
                "HTTP/1.1 304 Not Modified\r\nETag: {etag}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .ok();
            return;
        }
        let (status, answer) = match (method, path) {
            ("POST", "/token") if form.get("grant_type").map(String::as_str) == Some("authorization_code") => {
                let scope = behaviour.scope.clone().unwrap_or_else(|| SCOPE.into());
                (
                    200,
                    serde_json::json!({"access_token": ACCESS, "expires_in": 3599, "refresh_token": REFRESH,
                        "scope": scope, "token_type": "Bearer"}),
                )
            }
            ("POST", "/token") if behaviour.revoked => (
                400,
                serde_json::json!({"error": "invalid_grant", "error_description": "Token has been expired or revoked."}),
            ),
            ("POST", "/token") => {
                (200, serde_json::json!({"access_token": ACCESS, "expires_in": 3599, "scope": SCOPE}))
            }
            ("POST", "/revoke") => (200, serde_json::json!({})),
            ("GET", "/youtube/v3/channels") if authorization != format!("Bearer {ACCESS}") || behaviour.revoked => {
                (401, serde_json::json!({"error": {"code": 401}}))
            }
            ("GET", "/youtube/v3/channels") if behaviour.no_channel => (200, serde_json::json!({"items": []})),
            ("GET", "/youtube/v3/channels") => (
                200,
                serde_json::json!({"items": [{"snippet": {"title": "Test Channel",
                    "thumbnails": {"default": {"url": "https://yt3.ggpht.com/fake=s88"}}}}]}),
            ),
            (_, _) if api && refused => (401, serde_json::json!({"error": {"code": 401}})),
            (_, _) if api && behaviour.quota_out => {
                (403, serde_json::json!({"error": {"code": 403, "errors": [{"reason": "quotaExceeded"}]}}))
            }
            ("GET", "/youtube/v3/videos") if params.get("myRating").map(String::as_str) == Some("like") => {
                let (range, next) = page(behaviour.liked, params.get("pageToken").map(String::as_str));
                let items: Vec<_> = range
                    .map(|n| {
                        serde_json::json!({"id": video("liked", n), "snippet": {"title": format!("Liked {n}"),
                            "channelTitle": "Artist"}, "contentDetails": {"duration": "PT3M5S"}})
                    })
                    .collect();
                (
                    200,
                    serde_json::json!({"items": items, "nextPageToken": next, "pageInfo": {"totalResults": behaviour.liked}}),
                )
            }
            ("GET", "/youtube/v3/playlists") => {
                let items: Vec<_> = behaviour
                    .playlists
                    .iter()
                    .map(|(id, title, privacy, length)| {
                        serde_json::json!({"id": id, "snippet": {"title": title, "thumbnails": {"medium":
                            {"url": format!("https://i.ytimg.com/vi/{}/mqdefault.jpg", video(id, 0))}}},
                            "status": {"privacyStatus": privacy}, "contentDetails": {"itemCount": length}})
                    })
                    .collect();
                (200, serde_json::json!({"items": items}))
            }
            ("GET", "/youtube/v3/playlistItems") => {
                let list = params.get("playlistId").cloned().unwrap_or_default();
                match behaviour.playlists.iter().find(|p| p.0 == list) {
                    None => {
                        (404, serde_json::json!({"error": {"code": 404, "errors": [{"reason": "playlistNotFound"}]}}))
                    }
                    Some((id, _, _, length)) => {
                        let (range, next) = page(*length, params.get("pageToken").map(String::as_str));
                        let items: Vec<_> = range
                            .map(|n| {
                                // The second item is a private video, the third a deleted one.
                                let (title, privacy) = match n {
                                    1 => ("Private video".to_string(), "private"),
                                    2 => ("Deleted video".to_string(), "privacyStatusUnspecified"),
                                    n => (format!("{id} track {n}"), "public"),
                                };
                                serde_json::json!({"snippet": {"title": title, "videoOwnerChannelTitle": "Artist"},
                                    "contentDetails": {"videoId": video(id, n)}, "status": {"privacyStatus": privacy}})
                            })
                            .collect();
                        (
                            200,
                            serde_json::json!({"items": items, "nextPageToken": next, "pageInfo": {"totalResults": length}}),
                        )
                    }
                }
            }
            _ => (404, serde_json::json!({"error": "not found"})),
        };
        let answer = answer.to_string();
        let mut stream = stream;
        write!(
            stream,
            "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nETag: {etag}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
            answer.len()
        )
        .ok();
    }

    /// Plays the browser: follows the sign-in link to Google's answer, as if the person
    /// agreed, and returns the page the browser was shown.
    pub fn agree(url: &str, state_override: Option<&str>) -> String {
        let params: HashMap<String, String> = url
            .split_once('?')
            .unwrap()
            .1
            .split('&')
            .filter_map(|p| p.split_once('='))
            .map(|(k, v)| (decode(k), decode(v)))
            .collect();
        let state = state_override.unwrap_or(&params["state"]);
        let target =
            format!("{}/?state={}&code=4%2Fgood-code&scope={}", params["redirect_uri"], encode(state), encode(SCOPE));
        crate::net::agent()
            .get(&target)
            .config()
            .http_status_as_error(false)
            .build()
            .call()
            .map(|mut r| r.body_mut().read_to_string().unwrap_or_default())
            .unwrap_or_default()
    }

    pub fn client() -> Client {
        Client { id: "test-client.apps.googleusercontent.com".into(), secret: "test-secret".into() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(url: &str) -> HashMap<String, String> {
        url.split_once('?')
            .unwrap()
            .1
            .split('&')
            .filter_map(|p| p.split_once('='))
            .map(|(k, v)| (decode(k), decode(v)))
            .collect()
    }

    #[test]
    fn pkce_matches_the_rfc_example() {
        // RFC 7636, appendix B.
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        assert_eq!(base64url(b"f"), "Zg");
        assert_eq!(base64url(b"fo"), "Zm8");
        assert_eq!(base64url(b"foo"), "Zm9v");
    }

    #[test]
    fn sign_in_asks_only_for_youtube_readonly() {
        let sign_in = SignIn::start(&fake::client(), &Google::real()).unwrap();
        assert!(sign_in.url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?"));
        let p = params(&sign_in.url);
        assert_eq!(p["scope"], "https://www.googleapis.com/auth/youtube.readonly", "exactly one scope, view-only");
        assert_eq!(p["code_challenge_method"], "S256");
        assert_eq!(p["code_challenge"], challenge(&sign_in.verifier));
        assert!(p["state"].len() >= 20 && p["code_challenge"].len() == 43);
    }

    #[test]
    fn listener_binds_only_to_localhost() {
        let sign_in = SignIn::start(&fake::client(), &Google::real()).unwrap();
        let address = sign_in.listener.local_addr().unwrap();
        assert!(address.ip().is_loopback() && address.ip() == Ipv4Addr::LOCALHOST, "{address}");
        assert_eq!(params(&sign_in.url)["redirect_uri"], format!("http://127.0.0.1:{}", sign_in.port()));
    }

    #[test]
    fn only_read_endpoints_are_called() {
        for read in READS {
            let path = read.path();
            assert!(path.starts_with("/youtube/v3/"), "{path}");
            assert!(["channels", "playlists", "playlistItems", "videos"].contains(&&path["/youtube/v3/".len()..]));
        }
        // The one request that isn't a read is revoking our own sign-in.
        let source = include_str!("google.rs");
        let code = &source[..source.find("#[cfg(test)]").unwrap()];
        let posts = code.matches(".post(").count();
        assert_eq!(code.matches(".get(&").count(), 1, "one GET, in `get_tagged`, for the reads");
        assert_eq!(posts, 1, "only the token and revoke forms are POSTed, through post_form");
    }

    #[test]
    fn signing_in_end_to_end() {
        let server = fake::start();
        let google = Google::at(&server.base);
        let sign_in = SignIn::start(&fake::client(), &google).unwrap();
        let url = sign_in.url.clone();
        let browser = std::thread::spawn(move || fake::agree(&url, None));
        let tokens = sign_in.finish(&AtomicBool::new(false), Duration::from_secs(10)).unwrap();
        assert!(browser.join().unwrap().contains("Signed in to Featherplay"));
        assert_eq!((tokens.access.as_str(), tokens.refresh.as_str()), (fake::ACCESS, fake::REFRESH));
        let channel = my_channel(&google, &tokens.access).unwrap().unwrap();
        assert_eq!(channel.name, "Test Channel");
        let asked = server.requests.lock().unwrap().join("\n");
        assert!(asked.contains("grant_type=authorization_code") && asked.contains("code_verifier="), "{asked}");
    }

    #[test]
    fn answer_with_the_wrong_state_is_refused() {
        let server = fake::start();
        let sign_in = SignIn::start(&fake::client(), &Google::at(&server.base)).unwrap();
        let url = sign_in.url.clone();
        let browser = std::thread::spawn(move || {
            let refused = fake::agree(&url, Some("someone-elses-state"));
            (refused, fake::agree(&url, None))
        });
        let tokens = sign_in.finish(&AtomicBool::new(false), Duration::from_secs(10));
        let (refused, agreed) = browser.join().unwrap();
        assert!(refused.contains("out of date"), "{refused}");
        assert!(agreed.contains("Signed in"));
        assert!(tokens.is_ok());
        // Only the right answer was traded for tokens.
        assert_eq!(server.requests.lock().unwrap().iter().filter(|r| r.contains("/token")).count(), 1);
    }

    #[test]
    fn a_narrower_grant_is_refused() {
        let server = fake::start();
        server.behaviour.lock().unwrap().scope = Some("openid".into());
        let sign_in = SignIn::start(&fake::client(), &Google::at(&server.base)).unwrap();
        let url = sign_in.url.clone();
        std::thread::spawn(move || fake::agree(&url, None));
        let result = sign_in.finish(&AtomicBool::new(false), Duration::from_secs(10));
        assert!(matches!(result, Err(Error::Refused(_))), "{result:?}");
    }

    #[test]
    fn sign_in_times_out_after_five_minutes() {
        assert_eq!(SIGN_IN_TIMEOUT, Duration::from_secs(300));
        let sign_in = SignIn::start(&fake::client(), &Google::real()).unwrap();
        let port = sign_in.port();
        let started = Instant::now();
        assert_eq!(sign_in.finish(&AtomicBool::new(false), Duration::from_millis(300)).unwrap_err(), Error::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(TcpStream::connect((Ipv4Addr::LOCALHOST, port)).is_err(), "the port is closed after");
    }

    #[test]
    fn cancel_stops_listening() {
        let sign_in = SignIn::start(&fake::client(), &Google::real()).unwrap();
        let port = sign_in.port();
        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        let waiting = {
            let cancel = cancel.clone();
            std::thread::spawn(move || sign_in.finish(&cancel, Duration::from_secs(60)))
        };
        std::thread::sleep(Duration::from_millis(100));
        cancel.store(true, Ordering::SeqCst);
        assert_eq!(waiting.join().unwrap().unwrap_err(), Error::Cancelled);
        assert!(TcpStream::connect((Ipv4Addr::LOCALHOST, port)).is_err(), "the port is closed after Cancel");
    }

    #[test]
    fn a_revoked_sign_in_says_so() {
        let server = fake::start();
        server.behaviour.lock().unwrap().revoked = true;
        let google = Google::at(&server.base);
        assert_eq!(refresh(&google, &fake::client(), fake::REFRESH).unwrap_err(), Error::Revoked);
        assert_eq!(my_channel(&google, fake::ACCESS).unwrap_err(), Error::Revoked);
        assert!(revoke(&google, fake::REFRESH).is_ok());
    }

    #[test]
    fn an_account_without_a_channel() {
        let server = fake::start();
        server.behaviour.lock().unwrap().no_channel = true;
        assert_eq!(my_channel(&Google::at(&server.base), fake::ACCESS).unwrap(), None);
    }

    #[test]
    fn query_text_round_trips() {
        for text in ["a b", "4/0AbC-_.~", "http://127.0.0.1:5000", "é&=?"] {
            assert_eq!(decode(&encode(text)), text);
        }
        assert_eq!(decode("a+b%2Fc%zz"), "a b/c%zz");
    }
}
