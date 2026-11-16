//! The YouTube account: signed in or not, as whom, and the sign-in kept between runs.
//!
//! All that's kept is the refresh token and the channel's name and picture, in
//! `account.bin`, encrypted for this Windows user (`crate::secret`). The short-lived access
//! token stays in memory. Nothing goes anywhere but Google, and nothing happens on the
//! network while signed out, or until the library needs something.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::google::{self, Channel, Client, Error, Google, SignIn};
use crate::log;
use crate::messages as say;

pub const FILE: &str = "account.bin";
/// A sign-out that couldn't reach Google: its token, to revoke once online.
const PENDING_REVOKE: &str = "account-revoke.bin";
/// What the Library keeps of the account (later): deleted on sign-out too.
pub const LIBRARY_FILE: &str = "library.bin";
// An access token is renewed this long before Google says it ends.
const RENEW_EARLY: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// Sign-in isn't possible here: no secret store (Linux, for now), or no Google client
    /// in this build.
    Unavailable,
    SignedOut,
    /// Waiting for the browser; `url` is Google's page, for copying if no browser opened.
    Waiting {
        url: String,
    },
    /// Signed in; `None` for an account without a YouTube channel.
    SignedIn(Option<Channel>),
}

pub enum Note {
    /// The status changed.
    Changed,
    Message(String),
}

/// How the sign-in is kept on disk: encrypted for this user (or, in tests, a stand-in).
#[derive(Clone, Copy)]
pub struct Seal {
    pub protect: fn(&[u8]) -> Result<Vec<u8>, String>,
    pub unprotect: fn(&[u8]) -> Result<Vec<u8>, String>,
}

impl Seal {
    pub fn system() -> Option<Self> {
        crate::secret::AVAILABLE
            .then_some(Self { protect: crate::secret::protect, unprotect: crate::secret::unprotect })
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Saved {
    /// The Google client the sign-in belongs to: another one (a custom client ID) can't use it.
    client: String,
    refresh: String,
    channel: Option<Channel>,
}

struct Inner {
    status: Status,
    refresh: Option<String>,
    access: Option<(String, Instant)>,
}

pub struct Account {
    data: PathBuf,
    google: Google,
    client: Option<Client>,
    seal: Option<Seal>,
    inner: Mutex<Inner>,
    cancel: Arc<AtomicBool>,
    notify: Box<dyn Fn(Note) + Send + Sync>,
}

impl Account {
    /// Reads the kept sign-in, if there is one this user and this client can use. No network.
    pub fn new(
        data: &Path,
        client: Option<Client>,
        google: Google,
        seal: Option<Seal>,
        notify: impl Fn(Note) + Send + Sync + 'static,
    ) -> Arc<Self> {
        let (status, refresh) = match (&client, seal) {
            (Some(client), Some(seal)) => match load(data, seal) {
                Some(saved) if saved.client == client.id => (Status::SignedIn(saved.channel), Some(saved.refresh)),
                Some(_) => {
                    // Another client's sign-in: no use to this one.
                    forget(data);
                    (Status::SignedOut, None)
                }
                None => (Status::SignedOut, None),
            },
            _ => (Status::Unavailable, None),
        };
        let account = Arc::new(Self {
            data: data.to_path_buf(),
            google,
            client,
            seal,
            inner: Mutex::new(Inner { status, refresh, access: None }),
            cancel: Arc::new(AtomicBool::new(false)),
            notify: Box::new(notify),
        });
        account.revoke_pending();
        account
    }

    pub fn status(&self) -> Status {
        self.inner.lock().unwrap().status.clone()
    }

    /// Opens Google's page with `open` (false if no browser could be opened: the page's
    /// link is then shown to copy) and waits for the answer in the background.
    pub fn sign_in(self: &Arc<Self>, open: impl FnOnce(&str) -> bool + Send + 'static) {
        let (Some(client), Some(seal)) = (self.client.clone(), self.seal) else { return };
        if matches!(self.status(), Status::Waiting { .. } | Status::SignedIn(_)) {
            return;
        }
        let sign_in = match SignIn::start(&client, &self.google) {
            Ok(sign_in) => sign_in,
            Err(e) => {
                log::write(format!("Couldn't start signing in: {e}"));
                return self.say(say::sign_in_failed());
            }
        };
        self.cancel.store(false, Ordering::SeqCst);
        self.set(Status::Waiting { url: sign_in.url.clone() });
        let account = self.clone();
        std::thread::spawn(move || {
            if !open(&sign_in.url) {
                log::write("Couldn't open a browser for signing in; the link is shown to copy");
            }
            let result = sign_in.finish(&account.cancel, google::SIGN_IN_TIMEOUT).and_then(|tokens| {
                let channel = google::my_channel(&account.google, &tokens.access)?;
                Ok((tokens, channel))
            });
            match result {
                Ok((tokens, channel)) => {
                    let saved = Saved { client: client.id.clone(), refresh: tokens.refresh.clone(), channel };
                    if let Err(e) = save(&account.data, seal, &saved) {
                        log::write(format!("Couldn't keep the sign-in: {e}"));
                        account.say(say::couldnt_save("sign-in"));
                    }
                    {
                        let mut inner = account.inner.lock().unwrap();
                        inner.refresh = Some(tokens.refresh);
                        inner.access = Some((tokens.access, Instant::now() + tokens.lasts));
                        inner.status = Status::SignedIn(saved.channel.clone());
                    }
                    (account.notify)(Note::Changed);
                    let name = saved.channel.map_or_else(|| "no YouTube channel".to_string(), |c| c.name);
                    account.say(say::signed_in(&name));
                }
                Err(e) => {
                    account.set(Status::SignedOut);
                    match e {
                        Error::Cancelled => {}
                        Error::Declined => account.say(say::sign_in_cancelled()),
                        Error::TimedOut => account.say(say::sign_in_timed_out()),
                        e => {
                            log::write(format!("Couldn't sign in: {e}"));
                            account.say(say::sign_in_failed());
                        }
                    }
                }
            }
        });
    }

    /// Stops waiting for the browser.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    /// Ends the sign-in at Google, then forgets it here (the library with it), even
    /// offline: then Google is told the next time Featherplay starts.
    pub fn sign_out(self: &Arc<Self>) {
        let refresh = {
            let mut inner = self.inner.lock().unwrap();
            inner.access = None;
            inner.status =
                if self.client.is_some() && self.seal.is_some() { Status::SignedOut } else { Status::Unavailable };
            inner.refresh.take()
        };
        forget(&self.data);
        (self.notify)(Note::Changed);
        let Some(refresh) = refresh else { return };
        let account = self.clone();
        std::thread::spawn(move || match google::revoke(&account.google, &refresh) {
            Ok(()) => account.say(say::signed_out()),
            Err(e) => {
                log::write(format!("Couldn't tell Google about the sign-out: {e}"));
                if let Some(sealed) = account.seal.and_then(|seal| (seal.protect)(refresh.as_bytes()).ok()) {
                    std::fs::write(account.data.join(PENDING_REVOKE), sealed).ok();
                }
                account.say(say::signed_out_offline());
            }
        });
    }

    /// An access token for reading the library, renewed when it's about to end. A sign-in
    /// Google no longer accepts (removed from the account, or expired) signs out here too.
    pub fn access(&self) -> Result<String, Error> {
        let (refresh, client) = {
            let inner = self.inner.lock().unwrap();
            if let Some((token, until)) = &inner.access
                && Instant::now() + RENEW_EARLY < *until
            {
                return Ok(token.clone());
            }
            (inner.refresh.clone().ok_or(Error::Revoked)?, self.client.clone().ok_or(Error::Revoked)?)
        };
        match google::refresh(&self.google, &client, &refresh) {
            Ok((token, lasts)) => {
                self.inner.lock().unwrap().access = Some((token.clone(), Instant::now() + lasts));
                Ok(token)
            }
            Err(Error::Revoked) => {
                self.signed_out_by_google();
                Err(Error::Revoked)
            }
            Err(e) => Err(e),
        }
    }

    /// A request was refused with the access token: the sign-in is gone.
    pub fn signed_out_by_google(&self) {
        {
            let mut inner = self.inner.lock().unwrap();
            inner.refresh = None;
            inner.access = None;
            inner.status = Status::SignedOut;
        }
        forget(&self.data);
        log::write("Google no longer accepts the sign-in (removed from the account, or expired): signed out");
        (self.notify)(Note::Changed);
        self.say(say::signed_out_by_google());
    }

    /// A sign-out that couldn't reach Google is finished now, quietly, in the background.
    fn revoke_pending(self: &Arc<Self>) {
        let path = self.data.join(PENDING_REVOKE);
        let (Some(seal), Ok(sealed)) = (self.seal, std::fs::read(&path)) else { return };
        let Some(token) = (seal.unprotect)(&sealed).ok().and_then(|t| String::from_utf8(t).ok()) else {
            std::fs::remove_file(&path).ok();
            return;
        };
        let google = self.google.clone();
        std::thread::spawn(move || {
            if google::revoke(&google, &token).is_ok() {
                std::fs::remove_file(&path).ok();
            }
        });
    }

    fn set(&self, status: Status) {
        self.inner.lock().unwrap().status = status;
        (self.notify)(Note::Changed);
    }

    fn say(&self, message: String) {
        (self.notify)(Note::Message(message));
    }
}

fn load(data: &Path, seal: Seal) -> Option<Saved> {
    let sealed = std::fs::read(data.join(FILE)).ok()?;
    // Another user's or another PC's file doesn't open: that's simply signed out.
    let plain = (seal.unprotect)(&sealed).ok()?;
    serde_json::from_slice(&plain).ok()
}

fn save(data: &Path, seal: Seal, saved: &Saved) -> Result<(), String> {
    let plain = serde_json::to_vec(saved).map_err(|e| e.to_string())?;
    let sealed = (seal.protect)(&plain)?;
    let path = data.join(FILE);
    let temp = path.with_extension("tmp");
    std::fs::write(&temp, sealed).and_then(|()| std::fs::rename(&temp, &path)).map_err(|e| e.to_string())
}

/// Removes the kept sign-in and everything read with it.
fn forget(data: &Path) {
    for file in [FILE, LIBRARY_FILE] {
        std::fs::remove_file(data.join(file)).ok();
    }
}

/// For other modules' tests: a stand-in seal, and an account signed in through a fake Google.
#[cfg(test)]
pub mod testing {
    use super::*;
    use crate::google::fake;

    fn scramble(data: &[u8]) -> Result<Vec<u8>, String> {
        Ok(data.iter().map(|b| b ^ 0x5a).collect())
    }
    pub const SEAL: Seal = Seal { protect: scramble, unprotect: scramble };

    pub fn wait_for(what: impl Fn() -> bool) {
        let start = Instant::now();
        while !what() {
            assert!(start.elapsed() < Duration::from_secs(10), "timed out");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    pub fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("featherplay-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    pub fn signed_in(dir: &Path, server: &fake::Server) -> Arc<Account> {
        let account = Account::new(dir, Some(fake::client()), Google::at(&server.base), Some(SEAL), |_| {});
        account.sign_in(|url| {
            let url = url.to_string();
            std::thread::spawn(move || fake::agree(&url, None));
            true
        });
        wait_for(|| matches!(account.status(), Status::SignedIn(_)));
        account
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::google::fake;

    // A stand-in for DPAPI on systems without it: reversible, and not plain text.
    fn scramble(data: &[u8]) -> Result<Vec<u8>, String> {
        Ok(data.iter().map(|b| b ^ 0x5a).collect())
    }
    fn refuse(_: &[u8]) -> Result<Vec<u8>, String> {
        Err("not this user's".into())
    }
    const SEAL: Seal = Seal { protect: scramble, unprotect: scramble };

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("featherplay-account-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    type Notes = Arc<Mutex<Vec<String>>>;

    fn open(dir: &Path, base: &str, seal: Seal) -> (Arc<Account>, Notes) {
        let notes: Notes = Arc::default();
        let seen = notes.clone();
        let account = Account::new(dir, Some(fake::client()), Google::at(base), Some(seal), move |note| {
            if let Note::Message(m) = note {
                seen.lock().unwrap().push(m);
            }
        });
        (account, notes)
    }

    fn wait_for(what: impl Fn() -> bool) {
        let start = Instant::now();
        while !what() {
            assert!(start.elapsed() < Duration::from_secs(10), "timed out");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Signs in through the fake Google, the way a person agreeing in the browser would.
    fn signed_in(dir: &Path, server: &fake::Server) -> (Arc<Account>, Notes) {
        let (account, notes) = open(dir, &server.base, SEAL);
        account.sign_in(|url| {
            let url = url.to_string();
            std::thread::spawn(move || fake::agree(&url, None));
            true
        });
        wait_for(|| matches!(account.status(), Status::SignedIn(_)));
        (account, notes)
    }

    #[test]
    fn signing_in_keeps_the_account_for_next_time() {
        let (dir, server) = (temp_dir("keep"), fake::start());
        let (account, notes) = signed_in(&dir, &server);
        let channel = Channel { name: "Test Channel".into(), picture: Some("https://yt3.ggpht.com/fake=s88".into()) };
        assert_eq!(account.status(), Status::SignedIn(Some(channel.clone())));
        wait_for(|| !notes.lock().unwrap().is_empty());
        assert_eq!(notes.lock().unwrap()[0], "Signed in · Test Channel");
        // Kept, not as plain text.
        let file = std::fs::read(dir.join(FILE)).unwrap();
        assert!(!String::from_utf8_lossy(&file).contains(fake::REFRESH));
        // The next run is signed in at once, without asking Google anything.
        let asked = server.requests.lock().unwrap().len();
        let (again, _) = open(&dir, &server.base, SEAL);
        assert_eq!(again.status(), Status::SignedIn(Some(channel)));
        assert_eq!(server.requests.lock().unwrap().len(), asked);
        assert_eq!(again.access().unwrap(), fake::ACCESS, "a fresh access token when needed");
    }

    #[test]
    fn sign_out_deletes_token_and_library() {
        let (dir, server) = (temp_dir("out"), fake::start());
        let (account, notes) = signed_in(&dir, &server);
        std::fs::write(dir.join(LIBRARY_FILE), b"saved library").unwrap();
        account.sign_out();
        assert_eq!(account.status(), Status::SignedOut);
        assert!(!dir.join(FILE).exists() && !dir.join(LIBRARY_FILE).exists());
        wait_for(|| notes.lock().unwrap().iter().any(|m| m == "Signed out"));
        assert!(server.requests.lock().unwrap().iter().any(|r| r.starts_with("POST /revoke")), "Google was told");

        // Offline: still forgotten here, and Google is told at the next start.
        let (dir, server) = (temp_dir("out-offline"), fake::start());
        let (online, _) = signed_in(&dir, &server);
        drop(online);
        let dead = "http://127.0.0.1:9";
        let (offline, notes) = open(&dir, dead, SEAL);
        std::fs::write(dir.join(LIBRARY_FILE), b"saved library").unwrap();
        offline.sign_out();
        assert!(!dir.join(FILE).exists() && !dir.join(LIBRARY_FILE).exists());
        wait_for(|| notes.lock().unwrap().iter().any(|m| m.contains("once online")));
        assert!(dir.join(PENDING_REVOKE).exists());
        let (_, _) = open(&dir, &server.base, SEAL);
        wait_for(|| !dir.join(PENDING_REVOKE).exists());
        assert!(server.requests.lock().unwrap().iter().any(|r| r.starts_with("POST /revoke")));
    }

    #[test]
    fn revoked_token_signs_out_cleanly() {
        let (dir, server) = (temp_dir("revoked"), fake::start());
        drop(signed_in(&dir, &server));
        // Access removed from the Google account (or a test sign-in's 7 days over).
        server.behaviour.lock().unwrap().revoked = true;
        let (account, notes) = open(&dir, &server.base, SEAL);
        assert!(matches!(account.status(), Status::SignedIn(_)), "it can't know before asking");
        assert_eq!(account.access().unwrap_err(), Error::Revoked);
        assert_eq!(account.status(), Status::SignedOut);
        assert!(!dir.join(FILE).exists());
        assert_eq!(notes.lock().unwrap().as_slice(), ["Signed out by Google · sign in again"]);
    }

    #[test]
    fn another_users_file_starts_signed_out() {
        let (dir, server) = (temp_dir("other-user"), fake::start());
        let (account, _) = signed_in(&dir, &server);
        drop(account);
        let (account, notes) = open(&dir, &server.base, Seal { protect: scramble, unprotect: refuse });
        assert_eq!(account.status(), Status::SignedOut);
        assert!(notes.lock().unwrap().is_empty(), "no error, just signed out");
    }

    #[test]
    fn custom_client_id_is_used_and_signs_out_on_change() {
        let (dir, server) = (temp_dir("client"), fake::start());
        let (account, _) = signed_in(&dir, &server);
        drop(account);
        let other = Client { id: "someone-elses.apps.googleusercontent.com".into(), secret: "x".into() };
        let account = Account::new(&dir, Some(other), Google::at(&server.base), Some(SEAL), |_| {});
        assert_eq!(account.status(), Status::SignedOut);
        assert!(!dir.join(FILE).exists());
    }

    #[test]
    fn waiting_can_be_cancelled() {
        let (dir, server) = (temp_dir("cancel"), fake::start());
        let (account, notes) = open(&dir, &server.base, SEAL);
        let port = Arc::new(Mutex::new(None));
        let seen = port.clone();
        account.sign_in(move |url| {
            let redirect = url.split("redirect_uri=").nth(1).unwrap().split('&').next().unwrap().to_string();
            *seen.lock().unwrap() = redirect.rsplit("%3A").next().and_then(|p| p.parse::<u16>().ok());
            false
        });
        assert!(matches!(account.status(), Status::Waiting { .. }));
        wait_for(|| port.lock().unwrap().is_some());
        account.cancel();
        wait_for(|| account.status() == Status::SignedOut);
        let port = port.lock().unwrap().unwrap();
        wait_for(|| std::net::TcpStream::connect(("127.0.0.1", port)).is_err());
        assert!(notes.lock().unwrap().is_empty(), "cancelling here needs no message");
    }

    #[test]
    fn without_a_secret_store_or_a_client_sign_in_is_unavailable() {
        let dir = temp_dir("unavailable");
        let account = Account::new(&dir, Some(fake::client()), Google::real(), None, |_| {});
        assert_eq!(account.status(), Status::Unavailable);
        let account = Account::new(&dir, None, Google::real(), Some(SEAL), |_| {});
        assert_eq!(account.status(), Status::Unavailable);
    }

    #[cfg(windows)]
    #[test]
    fn token_survives_a_restart_and_is_not_plain_text() {
        let (dir, server) = (temp_dir("dpapi"), fake::start());
        let seal = Seal::system().expect("DPAPI on Windows");
        let (account, _) = open(&dir, &server.base, seal);
        account.sign_in(|url| {
            let url = url.to_string();
            std::thread::spawn(move || fake::agree(&url, None));
            true
        });
        wait_for(|| matches!(account.status(), Status::SignedIn(_)));
        drop(account);
        let file = std::fs::read(dir.join(FILE)).unwrap();
        let text = String::from_utf8_lossy(&file);
        assert!(!text.contains(fake::REFRESH) && !text.contains("Test Channel"), "encrypted");
        let (again, _) = open(&dir, &server.base, seal);
        assert!(matches!(again.status(), Status::SignedIn(Some(_))), "kept for the next run");
    }
}
