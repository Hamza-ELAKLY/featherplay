//! The signed-in account's library: liked videos and the channel's playlists, private ones
//! included, read through the YouTube API (view-only).
//!
//! The last-seen library is kept in `library.bin`, encrypted like the sign-in, and shows at
//! once; it's refreshed in the background when the Library tab opens and it's over 15
//! minutes old. Lists load 50 tracks at a time. Nothing is read or loaded until the tab is
//! first opened. Google's daily quota is shared by everyone using Featherplay, so requests
//! are few: about one per list page.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::account::{Account, LIBRARY_FILE, Seal};
use crate::google::{self, Error, Google, Read, Tagged};
use crate::log;
use crate::messages as say;
use crate::queue::Track;

/// The liked videos, opened like a playlist.
pub const LIKED: &str = "LL";
/// A library older than this is refreshed when the tab opens.
pub const STALE: Duration = Duration::from_secs(15 * 60);
const PAGE: &str = "50";

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Privacy {
    Public,
    Unlisted,
    Private,
}

impl Privacy {
    pub fn label(self) -> &'static str {
        match self {
            Privacy::Public => "Public",
            Privacy::Unlisted => "Unlisted",
            Privacy::Private => "Private",
        }
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Playlist {
    pub id: String,
    pub title: String,
    pub privacy: Privacy,
    pub count: usize,
    /// The video whose thumbnail stands for the playlist.
    pub thumb: Option<String>,
}

/// A list's tracks loaded so far, where the next page starts, and how many there are.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct List {
    pub tracks: Vec<Track>,
    pub next: Option<String>,
    pub total: usize,
    /// The first page's tag, for asking whether it changed.
    pub etag: Option<String>,
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct Saved {
    /// When it was last refreshed, in seconds since 1970.
    fetched: u64,
    playlists: Vec<Playlist>,
    playlists_etag: Option<String>,
    /// The liked videos and the playlists opened, by id.
    lists: HashMap<String, List>,
}

/// What the window shows of the library.
#[derive(Clone, Debug, Default)]
pub struct View {
    pub playlists: Vec<Playlist>,
    /// How many liked videos (once known).
    pub liked: Option<usize>,
    /// When it was last refreshed (`None`: never).
    pub fetched: Option<SystemTime>,
    pub refreshing: bool,
}

pub enum Note {
    Changed,
    Message(String),
}

/// What reading a private playlist (from a pasted link) through the account gave.
#[derive(Debug, PartialEq)]
pub enum PrivateList {
    /// Not signed in: signing in may open it.
    SignedOut,
    Found {
        name: String,
        tracks: Vec<Track>,
    },
    /// Signed in, but the playlist isn't this account's (or no longer exists).
    Elsewhere,
    Failed,
}

struct State {
    loaded: bool,
    saved: Saved,
    refreshing: bool,
    /// Lists with a page being fetched, so a page is asked for once.
    loading: Vec<String>,
}

pub struct Library {
    data: PathBuf,
    account: Arc<Account>,
    google: Google,
    seal: Option<Seal>,
    state: Mutex<State>,
    notify: Box<dyn Fn(Note) + Send + Sync>,
}

impl Library {
    /// Nothing is read until the tab first opens (`opened`).
    pub fn new(
        data: &Path,
        account: Arc<Account>,
        google: Google,
        seal: Option<Seal>,
        notify: impl Fn(Note) + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            data: data.to_path_buf(),
            account,
            google,
            seal,
            state: Mutex::new(State { loaded: false, saved: Saved::default(), refreshing: false, loading: Vec::new() }),
            notify: Box::new(notify),
        })
    }

    /// The tab opened: the saved library shows at once, and is refreshed in the
    /// background if it's older than 15 minutes.
    pub fn opened(self: &Arc<Self>) {
        let stale = {
            let mut state = self.state.lock().unwrap();
            if !state.loaded {
                state.saved = self.seal.and_then(|seal| load(&self.data, seal)).unwrap_or_default();
                state.loaded = true;
            }
            age(state.saved.fetched).is_none_or(|age| age >= STALE)
        };
        (self.notify)(Note::Changed);
        if stale {
            self.refresh();
        }
    }

    /// Reads the playlists and the first page of liked videos again, now.
    pub fn refresh(self: &Arc<Self>) {
        {
            let mut state = self.state.lock().unwrap();
            if state.refreshing {
                return;
            }
            state.refreshing = true;
        }
        (self.notify)(Note::Changed);
        let library = self.clone();
        std::thread::spawn(move || {
            let result = library.fetch_playlists().and_then(|()| library.fetch_page(LIKED, true));
            match result {
                Ok(()) => {
                    library.state.lock().unwrap().saved.fetched = now();
                    library.save();
                }
                Err(e) => library.failed(&e),
            }
            // Done only once saved: what's on disk is what's shown.
            library.state.lock().unwrap().refreshing = false;
            (library.notify)(Note::Changed);
        });
    }

    pub fn view(&self) -> View {
        let state = self.state.lock().unwrap();
        View {
            playlists: state.saved.playlists.clone(),
            liked: state.saved.lists.get(LIKED).map(|l| l.total),
            fetched: (state.saved.fetched > 0).then(|| UNIX_EPOCH + Duration::from_secs(state.saved.fetched)),
            refreshing: state.refreshing,
        }
    }

    /// A list's tracks as loaded so far (liked videos, or a playlist).
    pub fn list(&self, id: &str) -> Option<List> {
        self.state.lock().unwrap().saved.lists.get(id).cloned()
    }

    /// Opening a list: what's saved shows at once, and its first page is read again in
    /// the background (cheaply, if unchanged).
    pub fn open(self: &Arc<Self>, id: &str) {
        self.in_background(id, true);
    }

    /// The next 50 tracks of a list, when the end of what's loaded comes into view.
    pub fn more(self: &Arc<Self>, id: &str) {
        let has_more = self.state.lock().unwrap().saved.lists.get(id).is_some_and(|l| l.next.is_some());
        if has_more {
            self.in_background(id, false);
        }
    }

    /// Every track of a list (for playing or shuffling all of it): the pages not loaded
    /// yet are read first. Blocks; call it off the window's thread.
    pub fn all(&self, id: &str) -> Result<Vec<Track>, Error> {
        if !self.state.lock().unwrap().saved.lists.contains_key(id) {
            self.fetch_page(id, true)?;
        }
        while self.state.lock().unwrap().saved.lists.get(id).is_some_and(|l| l.next.is_some()) {
            self.fetch_page(id, false)?;
        }
        self.save();
        Ok(self.list(id).map(|l| l.tracks).unwrap_or_default())
    }

    /// A playlist yt-dlp couldn't open (private), read through the account instead.
    /// Blocks; call it off the window's thread.
    pub fn read_private(&self, id: &str) -> PrivateList {
        if !matches!(self.account.status(), crate::account::Status::SignedIn(_)) {
            return PrivateList::SignedOut;
        }
        match self.all(id) {
            Ok(tracks) if !tracks.is_empty() => PrivateList::Found { name: self.name_of(id), tracks },
            Ok(_) => PrivateList::Elsewhere,
            Err(Error::Refused(e)) if e.contains("404") || e.contains("403") => PrivateList::Elsewhere,
            Err(Error::Revoked) => PrivateList::SignedOut,
            Err(e) => {
                log::write(format!("Couldn't read the private playlist {id}: {e}"));
                PrivateList::Failed
            }
        }
    }

    /// A playlist's name: from the library when it's the account's own, else asked of
    /// YouTube (one request).
    fn name_of(&self, id: &str) -> String {
        if let Some(p) = self.state.lock().unwrap().saved.playlists.iter().find(|p| p.id == id) {
            return p.title.clone();
        }
        match self.call(Read::Playlists, &[("part", "snippet"), ("id", id)], None) {
            Ok(Tagged::Fresh(value, _)) => {
                value["items"][0]["snippet"]["title"].as_str().unwrap_or("the playlist").into()
            }
            _ => "the playlist".into(),
        }
    }

    /// Forgets everything (after signing out: the account has deleted the file).
    pub fn clear(&self) {
        let mut state = self.state.lock().unwrap();
        state.saved = Saved::default();
        state.loaded = false;
    }

    fn in_background(self: &Arc<Self>, id: &str, first: bool) {
        {
            let mut state = self.state.lock().unwrap();
            if state.loading.iter().any(|l| l == id) {
                return;
            }
            state.loading.push(id.to_string());
        }
        let (library, id) = (self.clone(), id.to_string());
        std::thread::spawn(move || {
            match library.fetch_page(&id, first) {
                Ok(()) => library.save(),
                Err(e) => library.failed(&e),
            }
            library.state.lock().unwrap().loading.retain(|l| *l != id);
            (library.notify)(Note::Changed);
        });
    }

    /// One request, with the account's access; a refused sign-in signs out.
    fn call(&self, read: Read, params: &[(&str, &str)], etag: Option<&str>) -> Result<Tagged, Error> {
        let access = self.account.access()?;
        let result = google::get_tagged(&self.google, &access, read, params, etag);
        if result.as_ref().is_err_and(|e| *e == Error::Revoked) {
            self.account.signed_out_by_google();
        }
        result
    }

    fn fetch_playlists(&self) -> Result<(), Error> {
        let etag = self.state.lock().unwrap().saved.playlists_etag.clone();
        let mut playlists = Vec::new();
        let mut token: Option<String> = None;
        let mut first_tag = None;
        loop {
            let mut params = vec![("part", "snippet,contentDetails,status"), ("mine", "true"), ("maxResults", PAGE)];
            if let Some(token) = &token {
                params.push(("pageToken", token));
            }
            // Only the first page is asked about by tag: a change anywhere shows there.
            let tag = if token.is_none() { etag.as_deref() } else { None };
            match self.call(Read::Playlists, &params, tag)? {
                Tagged::Unchanged => return Ok(()),
                Tagged::Fresh(value, tag) => {
                    if token.is_none() {
                        first_tag = tag;
                    }
                    let (mut page, next) = parse_playlists(&value);
                    playlists.append(&mut page);
                    match next {
                        Some(next) => token = Some(next),
                        None => break,
                    }
                }
            }
        }
        let mut state = self.state.lock().unwrap();
        // Lists no longer on the channel go; the others keep what's loaded of them.
        let ids: Vec<&str> = playlists.iter().map(|p| p.id.as_str()).collect();
        state.saved.lists.retain(|id, _| id == LIKED || ids.contains(&id.as_str()));
        state.saved.playlists = playlists;
        state.saved.playlists_etag = first_tag;
        Ok(())
    }

    /// The first page of a list (replacing what's loaded, unless it's unchanged), or the
    /// next one.
    fn fetch_page(&self, id: &str, first: bool) -> Result<(), Error> {
        let (token, etag) = {
            let state = self.state.lock().unwrap();
            let list = state.saved.lists.get(id);
            match (first, list) {
                (true, list) => (None, list.and_then(|l| l.etag.clone())),
                (false, Some(List { next: Some(next), .. })) => (Some(next.clone()), None),
                (false, _) => return Ok(()),
            }
        };
        let mut params: Vec<(&str, &str)> = vec![("maxResults", PAGE)];
        let read = if id == LIKED {
            params.extend([("part", "snippet,contentDetails"), ("myRating", "like")]);
            Read::Videos
        } else {
            params.extend([("part", "snippet,contentDetails,status"), ("playlistId", id)]);
            Read::PlaylistItems
        };
        if let Some(token) = &token {
            params.push(("pageToken", token));
        }
        let (value, tag) = match self.call(read, &params, etag.as_deref())? {
            Tagged::Unchanged => return Ok(()),
            Tagged::Fresh(value, tag) => (value, tag),
        };
        let (mut tracks, next, total) = if id == LIKED { parse_liked(&value) } else { parse_items(&value) };
        let mut state = self.state.lock().unwrap();
        let list = state.saved.lists.entry(id.to_string()).or_default();
        if first {
            *list = List { tracks, next, total, etag: tag };
        } else {
            list.tracks.append(&mut tracks);
            list.next = next;
            list.total = total.max(list.tracks.len());
        }
        Ok(())
    }

    fn failed(&self, error: &Error) {
        log::write(format!("Couldn't read the library: {error}"));
        match error {
            // The account said so already.
            Error::Revoked => {}
            Error::Quota => (self.notify)(Note::Message(say::library_quota())),
            _ => (self.notify)(Note::Message(say::library_failed())),
        }
    }

    fn save(&self) {
        let Some(seal) = self.seal else { return };
        // Not after signing out meanwhile: the library goes with the sign-in.
        if !matches!(self.account.status(), crate::account::Status::SignedIn(_)) {
            return;
        }
        let plain = serde_json::to_vec(&self.state.lock().unwrap().saved).unwrap_or_default();
        let Ok(sealed) = (seal.protect)(&plain) else { return };
        let path = self.data.join(LIBRARY_FILE);
        let temp = path.with_extension("tmp");
        if let Err(e) = std::fs::write(&temp, sealed).and_then(|()| std::fs::rename(&temp, &path)) {
            log::write(format!("Couldn't keep the library: {e}"));
        }
    }
}

fn load(data: &Path, seal: Seal) -> Option<Saved> {
    let sealed = std::fs::read(data.join(LIBRARY_FILE)).ok()?;
    serde_json::from_slice(&(seal.unprotect)(&sealed).ok()?).ok()
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// How long ago `fetched` was; `None` for never.
fn age(fetched: u64) -> Option<Duration> {
    (fetched > 0).then(|| Duration::from_secs(now().saturating_sub(fetched)))
}

fn watch(id: &str) -> String {
    format!("https://www.youtube.com/watch?v={id}")
}

fn next_page(value: &serde_json::Value) -> Option<String> {
    value["nextPageToken"].as_str().map(String::from)
}

fn total(value: &serde_json::Value, loaded: usize) -> usize {
    value["pageInfo"]["totalResults"].as_u64().map_or(loaded, |n| n as usize)
}

/// A page of liked videos (`videos.list?myRating=like`).
fn parse_liked(value: &serde_json::Value) -> (Vec<Track>, Option<String>, usize) {
    let tracks: Vec<Track> = value["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let id = item["id"].as_str()?;
            Some(Track {
                url: watch(id),
                title: item["snippet"]["title"].as_str().unwrap_or(id).to_string(),
                duration: item["contentDetails"]["duration"].as_str().and_then(iso_duration),
                channel: item["snippet"]["channelTitle"].as_str().unwrap_or_default().to_string(),
                unavailable: false,
                failed: false,
            })
        })
        .collect();
    let loaded = tracks.len();
    (tracks, next_page(value), total(value, loaded))
}

/// A page of a playlist's tracks (`playlistItems.list`). Private and deleted videos are
/// kept, marked unavailable, so the list matches YouTube's.
fn parse_items(value: &serde_json::Value) -> (Vec<Track>, Option<String>, usize) {
    let tracks: Vec<Track> = value["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let id = item["contentDetails"]["videoId"].as_str()?;
            let title = item["snippet"]["title"].as_str().unwrap_or(id).to_string();
            let privacy = item["status"]["privacyStatus"].as_str().unwrap_or("public");
            let gone = privacy == "private"
                || privacy == "privacyStatusUnspecified"
                || title == "Private video"
                || title == "Deleted video";
            Some(Track {
                url: watch(id),
                title,
                duration: None,
                channel: item["snippet"]["videoOwnerChannelTitle"].as_str().unwrap_or_default().to_string(),
                unavailable: gone,
                failed: false,
            })
        })
        .collect();
    let loaded = tracks.len();
    (tracks, next_page(value), total(value, loaded))
}

/// A page of the channel's playlists (`playlists.list?mine=true`).
fn parse_playlists(value: &serde_json::Value) -> (Vec<Playlist>, Option<String>) {
    let playlists = value["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let id = item["id"].as_str()?.to_string();
            let privacy = match item["status"]["privacyStatus"].as_str() {
                Some("private") => Privacy::Private,
                Some("unlisted") => Privacy::Unlisted,
                _ => Privacy::Public,
            };
            let thumbs = &item["snippet"]["thumbnails"];
            let thumb = ["medium", "default", "high"]
                .iter()
                .find_map(|k| thumbs[k]["url"].as_str())
                .and_then(|url| url.split("/vi/").nth(1))
                .and_then(|rest| rest.split('/').next())
                .map(String::from);
            Some(Playlist {
                id,
                title: item["snippet"]["title"].as_str().unwrap_or("Playlist").to_string(),
                privacy,
                count: item["contentDetails"]["itemCount"].as_u64().unwrap_or(0) as usize,
                thumb,
            })
        })
        .collect();
    (playlists, next_page(value))
}

/// Seconds in an ISO 8601 duration as YouTube gives it: `PT4M1S`, `PT1H2M`, `P1DT2H`.
fn iso_duration(text: &str) -> Option<f64> {
    let text = text.strip_prefix('P')?;
    let mut seconds = 0.0;
    let mut number = String::new();
    let mut time = false;
    for c in text.chars() {
        match c {
            'T' => time = true,
            '0'..='9' | '.' => number.push(c),
            unit => {
                let n: f64 = number.parse().ok()?;
                number.clear();
                seconds += n * match (unit, time) {
                    ('D', false) => 86_400.0,
                    ('W', false) => 604_800.0,
                    ('H', true) => 3600.0,
                    ('M', true) => 60.0,
                    ('S', true) => 1.0,
                    _ => return None,
                };
            }
        }
    }
    (number.is_empty() && seconds > 0.0).then_some(seconds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::testing::{SEAL, signed_in, temp_dir, wait_for};
    use crate::google::fake;

    fn library(dir: &Path, server: &fake::Server) -> (Arc<Library>, Arc<Mutex<Vec<String>>>) {
        let account = signed_in(dir, server);
        let notes = Arc::new(Mutex::new(Vec::new()));
        let seen = notes.clone();
        let library = Library::new(dir, account, Google::at(&server.base), Some(SEAL), move |note| {
            if let Note::Message(m) = note {
                seen.lock().unwrap().push(m);
            }
        });
        (library, notes)
    }

    fn api_requests(server: &fake::Server) -> usize {
        server.requests.lock().unwrap().iter().filter(|r| r.contains("/youtube/v3/") && !r.contains("channels")).count()
    }

    fn settle(library: &Library) {
        wait_for(|| {
            let state = library.state.lock().unwrap();
            !state.refreshing && state.loading.is_empty()
        });
    }

    #[test]
    fn iso_durations() {
        assert_eq!(iso_duration("PT4M1S"), Some(241.0));
        assert_eq!(iso_duration("PT1H2M5S"), Some(3725.0));
        assert_eq!(iso_duration("PT45S"), Some(45.0));
        assert_eq!(iso_duration("P1DT2H"), Some(93_600.0));
        assert_eq!(iso_duration("P0D"), None, "live streams have no length");
        assert_eq!(iso_duration("4:01"), None);
    }

    #[test]
    fn liked_videos_page_parses() {
        // As YouTube answers (shortened; ids and tokens are made up).
        let value = serde_json::json!({
            "kind": "youtube#videoListResponse", "etag": "abc", "nextPageToken": "CDIQAA",
            "pageInfo": {"totalResults": 412, "resultsPerPage": 2},
            "items": [
                {"kind": "youtube#video", "id": "dQw4w9WgXcQ", "snippet": {"title": "Never Gonna Give You Up",
                    "channelTitle": "Rick Astley"}, "contentDetails": {"duration": "PT3M33S"}},
                {"kind": "youtube#video", "id": "jNQXAC9IVRw", "snippet": {"title": "Me at the zoo",
                    "channelTitle": "jawed"}, "contentDetails": {"duration": "PT19S"}}
            ]
        });
        let (tracks, next, total) = parse_liked(&value);
        assert_eq!(tracks.len(), 2);
        assert_eq!(tracks[0].url, "https://www.youtube.com/watch?v=dQw4w9WgXcQ");
        assert_eq!((tracks[0].title.as_str(), tracks[0].channel.as_str()), ("Never Gonna Give You Up", "Rick Astley"));
        assert_eq!(tracks[0].duration, Some(213.0));
        assert_eq!((next.as_deref(), total), (Some("CDIQAA"), 412));
    }

    #[test]
    fn playlists_page_parses_privacy_and_counts() {
        let value = serde_json::json!({"items": [
            {"id": "PL1", "snippet": {"title": "Night", "thumbnails": {"medium": {"url": "https://i.ytimg.com/vi/dQw4w9WgXcQ/mqdefault.jpg"}}},
                "status": {"privacyStatus": "private"}, "contentDetails": {"itemCount": 12}},
            {"id": "PL2", "snippet": {"title": "Drive"}, "status": {"privacyStatus": "unlisted"}, "contentDetails": {"itemCount": 3}},
            {"id": "PL3", "snippet": {"title": "All"}, "status": {"privacyStatus": "public"}, "contentDetails": {"itemCount": 0}}
        ]});
        let (playlists, next) = parse_playlists(&value);
        assert_eq!(next, None);
        let summary: Vec<_> = playlists.iter().map(|p| (p.title.as_str(), p.privacy, p.count)).collect();
        assert_eq!(
            summary,
            [("Night", Privacy::Private, 12), ("Drive", Privacy::Unlisted, 3), ("All", Privacy::Public, 0)]
        );
        assert_eq!(playlists[0].thumb.as_deref(), Some("dQw4w9WgXcQ"));
    }

    #[test]
    fn private_and_deleted_videos_are_marked() {
        let value = serde_json::json!({"items": [
            {"snippet": {"title": "Song", "videoOwnerChannelTitle": "Band"}, "contentDetails": {"videoId": "aaaaaaaaaaa"}, "status": {"privacyStatus": "public"}},
            {"snippet": {"title": "Private video"}, "contentDetails": {"videoId": "bbbbbbbbbbb"}, "status": {"privacyStatus": "private"}},
            {"snippet": {"title": "Deleted video"}, "contentDetails": {"videoId": "ccccccccccc"}, "status": {"privacyStatus": "privacyStatusUnspecified"}}
        ]});
        let (tracks, _, _) = parse_items(&value);
        assert_eq!(tracks.iter().map(|t| t.unavailable).collect::<Vec<_>>(), [false, true, true]);
        assert_eq!(tracks[0].channel, "Band");
    }

    #[test]
    fn nothing_is_read_until_the_tab_opens() {
        let (dir, server) = (temp_dir("library-idle"), fake::start());
        *server.behaviour.lock().unwrap() = fake::Behaviour::library();
        let (library, _) = library(&dir, &server);
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(api_requests(&server), 0);
        assert!(!library.state.lock().unwrap().loaded);
        library.opened();
        settle(&library);
        let view = library.view();
        assert_eq!(view.playlists.len(), 3);
        assert_eq!(view.liked, Some(60));
        assert!(view.fetched.is_some());
        assert_eq!(api_requests(&server), 2, "the playlists and the first page of liked videos");
    }

    #[test]
    fn stale_library_refreshes_fresh_one_doesnt() {
        let (dir, server) = (temp_dir("library-stale"), fake::start());
        *server.behaviour.lock().unwrap() = fake::Behaviour::library();
        let (library, _) = library(&dir, &server);
        library.opened();
        settle(&library);
        let asked = api_requests(&server);
        // Opened again soon after, and in a new run: no requests, from what's saved.
        library.opened();
        let account = library.account.clone();
        let again = Library::new(&dir, account, Google::at(&server.base), Some(SEAL), |_| {});
        again.opened();
        settle(&again);
        assert_eq!(again.view().playlists.len(), 3, "the saved library, at once");
        assert_eq!(api_requests(&server), asked, "fresh: nothing asked");
        // Over 15 minutes old: refreshed, and unchanged lists come back as "unchanged".
        again.state.lock().unwrap().saved.fetched -= STALE.as_secs() + 1;
        again.opened();
        settle(&again);
        assert_eq!(api_requests(&server) - asked, 2 + 2, "two requests, each answered \"unchanged\"");
        let unchanged = server.requests.lock().unwrap().iter().filter(|r| r.starts_with("304 ")).count();
        assert_eq!(unchanged, 2, "both lists were unchanged: no data sent");
        assert!(age(again.state.lock().unwrap().saved.fetched).unwrap() < Duration::from_secs(5));
    }

    #[test]
    fn quota_exceeded_keeps_the_saved_library() {
        let (dir, server) = (temp_dir("library-quota"), fake::start());
        *server.behaviour.lock().unwrap() = fake::Behaviour::library();
        let (library, notes) = library(&dir, &server);
        library.opened();
        settle(&library);
        server.behaviour.lock().unwrap().quota_out = true;
        library.refresh();
        settle(&library);
        assert_eq!(library.view().playlists.len(), 3, "still shown");
        assert_eq!(notes.lock().unwrap().as_slice(), [say::library_quota()]);
    }

    #[test]
    fn lists_load_fifty_at_a_time_and_all_on_demand() {
        let (dir, server) = (temp_dir("library-pages"), fake::start());
        *server.behaviour.lock().unwrap() = fake::Behaviour::library();
        let (library, _) = library(&dir, &server);
        library.opened();
        settle(&library);
        library.open("PLpublic");
        settle(&library);
        let list = library.list("PLpublic").unwrap();
        assert_eq!((list.tracks.len(), list.total), (50, 120));
        library.more("PLpublic");
        settle(&library);
        assert_eq!(library.list("PLpublic").unwrap().tracks.len(), 100);
        let all = library.all("PLpublic").unwrap();
        assert_eq!(all.len(), 120);
        assert!(all[1].unavailable && all[2].unavailable && !all[3].unavailable);
        assert_eq!(all[3].url, format!("https://www.youtube.com/watch?v={}", fake::video("PLpublic", 3)));
        // Liked videos too, with their lengths.
        let liked = library.all(LIKED).unwrap();
        assert_eq!(liked.len(), 60);
        assert_eq!(liked[0].duration, Some(185.0));
    }

    #[test]
    fn the_saved_library_is_not_plain_text_and_goes_with_the_sign_in() {
        let (dir, server) = (temp_dir("library-saved"), fake::start());
        *server.behaviour.lock().unwrap() = fake::Behaviour::library();
        let (library, _) = library(&dir, &server);
        library.opened();
        settle(&library);
        let file = std::fs::read(dir.join(LIBRARY_FILE)).unwrap();
        assert!(!String::from_utf8_lossy(&file).contains("Late night"), "private playlist names stay private");
        library.account.sign_out();
        library.clear();
        assert!(!dir.join(LIBRARY_FILE).exists());
        assert!(library.view().playlists.is_empty());
    }

    #[test]
    fn private_playlist_falls_back_to_the_api_only_when_signed_in() {
        let (dir, server) = (temp_dir("library-private"), fake::start());
        *server.behaviour.lock().unwrap() = fake::Behaviour::library();
        let (library, _) = library(&dir, &server);
        // The account's own private playlist: its tracks, and its name.
        match library.read_private("PLprivate") {
            PrivateList::Found { name, tracks } => {
                assert_eq!(name, "Late night");
                assert_eq!(tracks.len(), 12);
            }
            other => panic!("{other:?}"),
        }
        // Someone else's: not found for this account.
        assert_eq!(library.read_private("PLsomeoneelse"), PrivateList::Elsewhere);
        // Signed out: nothing asked of Google.
        library.account.sign_out();
        let asked = api_requests(&server);
        assert_eq!(library.read_private("PLprivate"), PrivateList::SignedOut);
        assert_eq!(api_requests(&server), asked);
    }
}
