//! The player as the UI sees it: a queue of tracks, played one after another.
//!
//! Every slow step (asking yt-dlp, opening a stream) runs on a worker thread, and the
//! UI hears about changes through the `notify` callback. A track's stream URL is
//! resolved just before it plays, and the next track's while the current one plays, so
//! long playlists cost nothing up front and stream URLs never sit around expiring.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::engine::Playback;
use crate::extract::{self, Listing, Updated};
use crate::levels::Levels;
use crate::library::PrivateList;
use crate::log;
use crate::messages as say;
use crate::queue::{Queue, Repeat, Track};
use crate::store::{self, History, Pins, SavedTrack, Session, Waves};
use crate::thumbs;

// YouTube stream URLs last about six hours; re-resolve well before that.
// Tracks failing to load in a row before yt-dlp is updated at once, and how often that
// may happen at most.
const RESCUE_AFTER_FAILURES: usize = 2;
const RESCUE_INTERVAL: Duration = Duration::from_secs(3600);
// Before each attempt at loading a track: at once, again at once with the other client,
// and once more after a pause.
const LOAD_RETRY_DELAYS: [Duration; 3] = [Duration::ZERO, Duration::ZERO, Duration::from_secs(2)];
const STREAM_URL_LIFETIME: Duration = Duration::from_secs(4 * 3600);
// Past this far into a track, "previous" restarts it instead.
const RESTART_THRESHOLD: f64 = 3.0;
const YTDLP_CHECK_INTERVAL: Duration = Duration::from_secs(7 * 24 * 3600);
const YTDLP_CHECK_DELAY: Duration = Duration::from_secs(30);
// While playing, the session is saved at least this often, for the position.
const SESSION_SAVE_INTERVAL: Duration = Duration::from_secs(15);
// A track that stops further than this from its end lost its stream rather than finished.
// How long the waveform waits for playback to settle before it is measured.
const MEASURE_DELAY: Duration = Duration::from_secs(2);
const EARLY_END: f64 = 5.0;
// Waits between attempts to get a lost stream back, in seconds.
const RECONNECT_DELAYS: [u64; 5] = [0, 2, 5, 10, 20];

pub enum Event {
    /// Tracks were added, removed, reordered or marked unavailable.
    QueueChanged,
    /// History or pins changed.
    SavedChanged,
    /// yt-dlp's version, once known or after an update.
    YtDlpVersion(String),
    /// A search finished; its tracks are in `search_results`.
    SearchDone(Result<(), String>),
    /// More results for the search came in (or couldn't).
    SearchMore(Result<(), String>),
    /// The current track, or whether it is loading or playing, changed.
    NowPlaying,
    Status(String),
}

/// The search the results are for, and how far into YouTube's results it has read.
#[derive(Default)]
struct Search {
    query: String,
    /// Bumped by each new search, so a page for an older one is dropped.
    generation: u64,
    /// How many of YouTube's results have been read (repeats included).
    read: usize,
    busy: bool,
    /// YouTube has no more, or the limit is reached.
    done: bool,
}

/// Adds a page of results to those shown, leaving out any already there (YouTube's order
/// can shift a little between requests). Returns how many were new.
fn add_results(shown: &mut Vec<Track>, page: Vec<extract::Entry>) -> usize {
    let before = shown.len();
    for entry in page {
        if shown.len() >= extract::SEARCH_LIMIT {
            break;
        }
        if shown.iter().any(|t| t.url == entry.url) {
            continue;
        }
        shown.push(Track {
            url: entry.url,
            title: entry.title,
            duration: entry.duration,
            channel: entry.channel,
            unavailable: false,
            failed: false,
        });
    }
    shown.len() - before
}

/// The seek the worker should make next, and whether a worker is running.
#[derive(Default)]
struct Seeking {
    target: Option<f64>,
    running: bool,
}

type PrivateLists = Arc<dyn Fn(&str) -> PrivateList + Send + Sync>;

/// Where a list goes in the queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    /// Plays now; the rest of the queue follows it.
    Now,
    /// Right after the current track.
    Next,
    /// At the end.
    End,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Enqueue {
    /// Clear the queue first, then play the new tracks.
    Replace,
    /// Put them right after the current track and play them now; the rest of the queue
    /// follows them.
    PlayNext,
    /// Add to the end; play only if nothing is playing.
    Append,
    /// Put them right after the current track, without playing them (unless nothing is).
    Next,
    /// Add to the end and play the new tracks now.
    AppendAndPlay,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Idle,
    Loading,
    Playing,
}

pub struct NowPlaying {
    pub index: Option<usize>,
    /// Where play will start, for a track restored from the last run and not yet playing.
    pub resume_at: Option<f64>,
    pub title: String,
    pub channel: String,
    pub duration: f64,
    pub loading: bool,
    /// The title of the track that plays after this one, if any.
    pub up_next: Option<String>,
}

pub struct App {
    playback: Arc<Playback>,
    queue: Mutex<Queue>,
    state: Mutex<State>,
    /// Bumped by every request to play something, so a slow load that lost the race
    /// to a newer request can tell and quietly give up.
    generation: AtomicU64,
    streams: Mutex<HashMap<String, (String, Instant)>>,
    history: Mutex<History>,
    pins: Mutex<Pins>,
    results: Mutex<Vec<Track>>,
    search: Mutex<Search>,
    /// Where the current track last was, for picking it up again after a dropout.
    last_position: Mutex<f64>,
    data: std::path::PathBuf,
    /// Where a track restored from the last run starts when play is pressed.
    resume_at: Mutex<Option<f64>>,
    /// Bumped by every change to the queue or the current track, so the session is
    /// saved when it moves and not otherwise.
    version: AtomicU64,
    saved: Mutex<(u64, Instant)>,
    seeking: Mutex<Seeking>,
    waves: Mutex<Waves>,
    /// The waveform being measured, and the link of the track it belongs to.
    levels: Mutex<Option<(String, Arc<Levels>)>>,
    /// Links being looked up to play at once: counted as buffering, as they are the wait.
    opening: AtomicUsize,
    /// Tracks that failed to load in a row, for no reason that lasts: when YouTube has
    /// changed, the cure is a new yt-dlp, fetched at once (see `rescue_ytdlp`).
    failures: AtomicUsize,
    rescued: Mutex<Option<Instant>>,
    /// Whether yt-dlp may be updated without being asked (the Settings switch).
    auto_updates: AtomicBool,
    private_lists: Mutex<Option<PrivateLists>>,
    notify: Box<dyn Fn(Event) + Send + Sync>,
}

impl App {
    /// `data` is where history and pins are kept.
    pub fn new(
        playback: Arc<Playback>,
        data: &std::path::Path,
        notify: impl Fn(Event) + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            playback,
            queue: Mutex::new(Queue::new()),
            state: Mutex::new(State::Idle),
            generation: AtomicU64::new(0),
            streams: Mutex::new(HashMap::new()),
            history: Mutex::new(History::load(data)),
            pins: Mutex::new(Pins::load(data)),
            results: Mutex::new(Vec::new()),
            search: Mutex::new(Search::default()),
            last_position: Mutex::new(0.0),
            data: data.to_path_buf(),
            resume_at: Mutex::new(None),
            version: AtomicU64::new(0),
            saved: Mutex::new((0, Instant::now())),
            seeking: Mutex::new(Seeking::default()),
            waves: Mutex::new(Waves::load(data)),
            levels: Mutex::new(None),
            opening: AtomicUsize::new(0),
            failures: AtomicUsize::new(0),
            rescued: Mutex::new(None),
            auto_updates: AtomicBool::new(true),
            private_lists: Mutex::new(None),
            notify: Box::new(notify),
        })
    }

    pub fn playback(&self) -> &Arc<Playback> {
        &self.playback
    }

    /// Adds a video or a whole playlist. `play_now` replaces the queue and starts it;
    /// otherwise the tracks join the end, and play if nothing else is.
    pub fn open_link(self: &Arc<Self>, link: String, play_now: bool) {
        let app = self.clone();
        if play_now {
            self.opening.fetch_add(1, Ordering::SeqCst);
            self.emit(Event::NowPlaying);
        } else {
            self.status(say::looking_up());
        }
        let link_name = link.clone();
        std::thread::spawn(move || {
            // The wait for a link to play ends once its video is queued (or failed),
            // even while the rest of its playlist is still being listed.
            let opened = || {
                if play_now {
                    app.opening.fetch_sub(1, Ordering::SeqCst);
                    app.emit(Event::NowPlaying);
                }
            };
            // A video inside a playlist starts at once, and the rest of the playlist
            // fills in around it when listed: listing a long one takes seconds.
            if let Some(video) = extract::video_in_playlist(&link).filter(|_| play_now) {
                let listing = std::thread::spawn(move || extract::list(&link));
                let started = app.open_video(&video, true);
                if started {
                    opened();
                }
                match listing.join().unwrap_or_else(|_| Err("listing failed".into())) {
                    Ok(listing) if started => app.splice_listing(&video, listing),
                    Ok(listing) => app.add_listing(listing, true),
                    Err(e) => app.listing_failed(&link_name, &e, started.then_some(video.as_str()), true),
                }
                if !started {
                    opened();
                }
            } else if extract::is_playlist(&link) {
                match extract::list(&link) {
                    Ok(listing) => app.add_listing(listing, play_now),
                    Err(e) => app.listing_failed(&link_name, &e, None, play_now),
                }
                opened();
            } else {
                app.open_video(&link, play_now);
                opened();
            }
        });
    }

    /// Resolves and queues one video, returning whether that worked.
    fn open_video(self: &Arc<Self>, link: &str, play_now: bool) -> bool {
        match extract::resolve(link) {
            Ok(resolved) => {
                self.remember_stream(link, resolved.stream_url);
                let track = Track {
                    url: link.into(),
                    title: resolved.title,
                    duration: resolved.duration,
                    channel: resolved.channel,
                    unavailable: false,
                    failed: false,
                };
                let title = track.title.clone();
                let others = !self.queue.lock().unwrap().tracks().is_empty();
                self.enqueue(vec![track], 0, if play_now { Enqueue::PlayNext } else { Enqueue::Append });
                match (play_now, others) {
                    (true, true) => self.status(say::playing(&title)),
                    (true, false) => {}
                    (false, _) => self.status(say::added(&title)),
                }
                true
            }
            Err(e) => {
                log::write(format!("Couldn't open {link}: {e}"));
                self.status(say::link_failed());
                false
            }
        }
    }

    fn add_listing(self: &Arc<Self>, listing: Listing, play_now: bool) {
        let name = listing.title.clone().unwrap_or_else(|| "the playlist".into());
        let start = listing.start.unwrap_or(0);
        let tracks = playlist_tracks(listing);
        let count = tracks.len();
        self.enqueue(tracks, start, if play_now { Enqueue::PlayNext } else { Enqueue::Append });
        self.status(if play_now { say::playing_list(&name, count) } else { say::added_list(&name, count) });
    }

    /// Puts a playlist in place of the video from it that is already queued.
    fn splice_listing(self: &Arc<Self>, video: &str, listing: Listing) {
        let name = listing.title.clone().unwrap_or_else(|| "the playlist".into());
        let start = listing.start;
        let mut tracks = playlist_tracks(listing);
        let count = tracks.len();
        {
            let mut queue = self.queue.lock().unwrap();
            // The video just started: the current track, unless something else was picked since.
            let current = queue.current().filter(|&i| queue.track(i).is_some_and(|t| t.url == video));
            let Some(index) = current.or_else(|| queue.tracks().iter().position(|t| t.url == video)) else { return };
            // A video the listing leaves out keeps its place, and the playlist follows it.
            if start.is_none() {
                tracks.insert(0, queue.track(index).expect("found above").clone());
            }
            queue.splice(index, tracks, start.unwrap_or(0));
        }
        self.emit(Event::QueueChanged);
        self.status(say::playing_list(&name, count));
        let app = self.clone();
        std::thread::spawn(move || app.prefetch_next());
    }

    /// Queues one track from search results or history. `play` starts it now; a track
    /// already in the queue is played there rather than added twice.
    pub fn add_track(self: &Arc<Self>, track: Track, play: bool) {
        let queued = self.queue.lock().unwrap().tracks().iter().position(|t| t.url == track.url);
        match queued {
            Some(index) if play => self.play(index),
            Some(_) => self.status(say::already_queued(&track.title)),
            None => self.enqueue(vec![track], 0, if play { Enqueue::AppendAndPlay } else { Enqueue::Append }),
        }
    }

    /// Puts a track right after the current one, moving it there if it's already queued;
    /// with nothing playing, it plays now.
    pub fn play_next(self: &Arc<Self>, track: Track) {
        let title = track.title.clone();
        let (index, idle) = {
            let mut queue = self.queue.lock().unwrap();
            let index = queue.play_next(track);
            (index, *self.state.lock().unwrap() == State::Idle)
        };
        let Some(index) = index else {
            self.status(say::already_playing(&title));
            return;
        };
        self.emit(Event::QueueChanged);
        if idle {
            self.status(say::playing(&title));
            self.play(index);
        } else {
            self.status(say::playing_next(&title));
            let app = self.clone();
            std::thread::spawn(move || app.prefetch_next());
        }
    }

    /// Starts YouTube's Mix for a song: the song plays now (or keeps playing, if it is the
    /// current one) and the Mix follows it, before the rest of the queue. If the Mix can't
    /// be listed, nothing changes.
    pub fn start_radio(self: &Arc<Self>, song: Track) {
        let name = song.title.clone();
        let Some(link) = extract::radio_link(&song.url) else {
            self.status(say::radio_failed(&name));
            return;
        };
        let playing = {
            let queue = self.queue.lock().unwrap();
            queue.current().and_then(|c| queue.track(c)).is_some_and(|t| t.url == song.url)
        };
        // Starting a song is a wait like opening a link; extending the one playing isn't.
        if playing {
            self.status(say::starting_radio(&name));
        } else {
            self.opening.fetch_add(1, Ordering::SeqCst);
            self.emit(Event::NowPlaying);
        }
        let app = self.clone();
        std::thread::spawn(move || {
            let listed = extract::list(&link);
            if !playing {
                app.opening.fetch_sub(1, Ordering::SeqCst);
                app.emit(Event::NowPlaying);
            }
            // A video with no Mix (most that aren't music) lists only itself.
            let listed = listed.map(|listing| radio_tracks(playlist_tracks(listing), &song, playing));
            match listed {
                Ok(tracks) if tracks.len() <= usize::from(!playing) => {
                    app.status(say::no_radio(&name));
                }
                Ok(tracks) => {
                    let count = tracks.len() + usize::from(playing);
                    if playing {
                        app.queue.lock().unwrap().insert_next(tracks);
                        app.emit(Event::QueueChanged);
                        let next = app.clone();
                        std::thread::spawn(move || next.prefetch_next());
                    } else {
                        app.enqueue(tracks, 0, Enqueue::PlayNext);
                    }
                    app.status(say::radio(&name, count));
                }
                Err(e) => {
                    log::write(format!("Couldn't start radio from “{name}”: {e}"));
                    app.status(say::radio_failed(&name));
                }
            }
        });
    }

    /// Queues a whole list: playing now with the rest of the queue after it (as a pasted
    /// playlist with Enter), next after the current track, or at the end. `name` is for
    /// the message.
    pub fn queue_list(self: &Arc<Self>, tracks: Vec<Track>, name: &str, place: Place) {
        if tracks.is_empty() {
            return;
        }
        let count = tracks.len();
        let (how, message) = match place {
            Place::Now => (Enqueue::PlayNext, say::playing_list(name, count)),
            Place::Next => (Enqueue::Next, say::list_next(name, count)),
            Place::End => (Enqueue::Append, say::added_list(name, count)),
        };
        self.enqueue(tracks, 0, how);
        self.status(message);
    }

    /// How private playlists are read, where yt-dlp can't: through the signed-in account.
    pub fn read_private_lists_with(&self, read: impl Fn(&str) -> PrivateList + Send + Sync + 'static) {
        *self.private_lists.lock().unwrap() = Some(Arc::new(read));
    }

    /// A pasted playlist yt-dlp couldn't list. A private one is read through the account
    /// instead, when signed in; `started` is the video from the link already playing, if
    /// any, which the playlist then fills in around.
    fn listing_failed(self: &Arc<Self>, link: &str, error: &str, started: Option<&str>, play_now: bool) {
        log::write(format!("Couldn't list the playlist {link}: {error}"));
        let (true, Some(id)) = (is_private(error), extract::list_id(link)) else {
            return self.status(say::playlist_failed());
        };
        let reader = self.private_lists.lock().unwrap().clone();
        match reader.map_or(PrivateList::SignedOut, |read| read(&id)) {
            PrivateList::SignedOut => self.status(say::private_playlist_sign_in()),
            PrivateList::Elsewhere => self.status(say::private_elsewhere()),
            PrivateList::Failed => self.status(say::playlist_failed()),
            PrivateList::Found { name, tracks } => {
                let video_id = started.and_then(thumbs::video_id);
                let start = video_id
                    .as_ref()
                    .and_then(|v| tracks.iter().position(|t| thumbs::video_id(&t.url).as_ref() == Some(v)));
                let listing = Listing {
                    title: Some(name.clone()),
                    start,
                    entries: tracks
                        .into_iter()
                        .map(|t| extract::Entry {
                            url: t.url,
                            title: t.title,
                            duration: t.duration,
                            channel: t.channel,
                        })
                        .collect(),
                };
                match started {
                    Some(video) => self.splice_listing(video, listing),
                    None => self.add_listing(listing, play_now),
                }
            }
        }
    }

    /// Replaces the queue with `tracks` and plays them from the first.
    pub fn play_all(self: &Arc<Self>, tracks: Vec<Track>) {
        if !tracks.is_empty() {
            self.enqueue(tracks, 0, Enqueue::Replace);
        }
    }

    fn enqueue(self: &Arc<Self>, tracks: Vec<Track>, start: usize, how: Enqueue) {
        let to_play = {
            let mut queue = self.queue.lock().unwrap();
            if how == Enqueue::Replace {
                queue.clear();
            }
            let first = match how {
                Enqueue::PlayNext | Enqueue::Next => queue.insert_next(tracks),
                _ => queue.add(tracks),
            };
            let idle = *self.state.lock().unwrap() == State::Idle;
            (!matches!(how, Enqueue::Append | Enqueue::Next) || idle).then_some(first + start)
        };
        self.emit(Event::QueueChanged);
        match to_play {
            Some(index) => self.play(index),
            // What plays next may have changed: have it ready.
            None => {
                let app = self.clone();
                std::thread::spawn(move || app.prefetch_next());
            }
        }
    }

    /// Plays the track at `index` of the queue.
    pub fn play(self: &Arc<Self>, index: usize) {
        self.play_at(index, 0.0);
    }

    /// Plays the track at `index` of the queue, starting `at` seconds in.
    pub fn play_at(self: &Arc<Self>, index: usize, at: f64) {
        *self.resume_at.lock().unwrap() = None;
        let Some(track) = ({
            let mut queue = self.queue.lock().unwrap();
            queue.select(index).then(|| queue.track(index).cloned()).flatten()
        }) else {
            return;
        };
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        *self.state.lock().unwrap() = State::Loading;
        self.playback.stop();
        self.emit(Event::NowPlaying);

        let app = self.clone();
        std::thread::spawn(move || {
            let current = || app.generation.load(Ordering::SeqCst) == generation;
            // Most failures pass (the network, a slow answer, a stream link refused):
            // try again with a fresh link before giving up, unless the video is gone.
            let mut result = Err(String::new());
            for (attempt, delay) in LOAD_RETRY_DELAYS.into_iter().enumerate() {
                std::thread::sleep(delay);
                if !current() {
                    return;
                }
                // A link refused or failing: ask again as yt-dlp's own choice of client.
                let client = if attempt == 0 { extract::Client::Preferred } else { extract::Client::Default };
                result = app.stream_url(&track.url, client).and_then(|url| {
                    if !current() {
                        return Ok(());
                    }
                    app.playback.load(&url, at).map(|()| app.measure(&app.known(&track), &url))
                });
                match &result {
                    Err(e) if !extract::is_permanent(e) => {
                        app.streams.lock().unwrap().remove(&track.url);
                    }
                    _ => break,
                }
            }
            if !current() {
                return;
            }
            // The queue may have been reordered meanwhile: find the track again.
            let index = {
                let queue = app.queue.lock().unwrap();
                queue.current().filter(|&i| queue.track(i).is_some_and(|t| t.url == track.url)).unwrap_or(index)
            };
            match result {
                Ok(()) => {
                    app.queue.lock().unwrap().played(index);
                    app.failures.store(0, Ordering::SeqCst);
                    app.emit(Event::QueueChanged);
                    *app.state.lock().unwrap() = State::Playing;
                    app.emit(Event::NowPlaying);
                    if let Err(e) = app.history.lock().unwrap().record(&app.known(&track)) {
                        app.couldnt_save("history", &e);
                    }
                    app.emit(Event::SavedChanged);
                    app.prefetch_next();
                }
                Err(e) => {
                    let gone = extract::is_permanent(&e);
                    {
                        let mut queue = app.queue.lock().unwrap();
                        if gone { queue.mark_unavailable(index) } else { queue.mark_failed(index) }
                    }
                    *app.state.lock().unwrap() = State::Idle;
                    app.emit(Event::QueueChanged);
                    log::write(format!(
                        "{} “{}” ({}): {e}",
                        if gone { "Skipped" } else { "Couldn't load" },
                        track.title,
                        track.url
                    ));
                    app.status(if gone {
                        say::skipped(&track.title, extract::reason_gone(&e))
                    } else {
                        say::couldnt_load(&track.title)
                    });
                    if !gone && app.failures.fetch_add(1, Ordering::SeqCst) + 1 >= RESCUE_AFTER_FAILURES {
                        app.rescue_ytdlp();
                    }
                    app.advance(true);
                }
            }
        });
    }

    /// Skips forward. `auto` is the end of a track, where repeat-one replays it.
    pub fn advance(self: &Arc<Self>, auto: bool) {
        let next = self.queue.lock().unwrap().next(auto);
        match next {
            Some(index) => self.play(index),
            None if auto => {
                *self.state.lock().unwrap() = State::Idle;
                *self.resume_at.lock().unwrap() = None;
                self.emit(Event::NowPlaying);
                if !self.queue.lock().unwrap().tracks().is_empty() {
                    self.status(say::end_of_queue());
                }
            }
            None => {}
        }
    }

    /// Restarts the track if it is a few seconds in, or else goes to the one before.
    pub fn previous(self: &Arc<Self>) {
        if *self.state.lock().unwrap() == State::Playing && self.playback.position() > RESTART_THRESHOLD {
            self.seek(0.0);
            return;
        }
        let previous = self.queue.lock().unwrap().previous();
        if let Some(index) = previous {
            self.play(index);
        }
    }

    /// Seeks by `delta` seconds from where playback is, staying inside the track.
    pub fn seek_by(self: &Arc<Self>, delta: f64) {
        if !self.is_playing() {
            return;
        }
        let duration = self.now_playing().duration;
        // Presses in quick succession add up: count from a seek still on its way.
        let from = self.seeking.lock().unwrap().target.unwrap_or_else(|| self.playback.position());
        let target = from + delta;
        self.seek(if duration > 0.0 { target.clamp(0.0, duration - 1.0) } else { target.max(0.0) });
    }

    /// Pauses or resumes. Stopped (the queue ran out, or the stream was lost), it plays
    /// the current track again from the start.
    pub fn toggle_pause(self: &Arc<Self>) {
        let state = *self.state.lock().unwrap();
        match state {
            State::Playing => self.playback.toggle_pause(),
            State::Loading => {}
            State::Idle => self.restart(self.resume_point()),
        }
    }

    /// Like `toggle_pause`, but never pauses: for a "play" that must mean play.
    pub fn resume(self: &Arc<Self>) {
        let state = *self.state.lock().unwrap();
        match state {
            State::Playing => self.playback.resume(),
            State::Loading => {}
            State::Idle => self.restart(self.resume_point()),
        }
    }

    fn resume_point(&self) -> f64 {
        self.resume_at.lock().unwrap().unwrap_or(0.0)
    }

    /// Plays the current track from `at`, or the queue from its start if nothing is current.
    fn restart(self: &Arc<Self>, at: f64) {
        let current = self.queue.lock().unwrap().current();
        match current {
            Some(index) => self.play_at(index, at),
            None => self.advance(false),
        }
    }

    /// Seeks within the playing track; stopped, it plays the current track from there.
    pub fn seek(self: &Arc<Self>, seconds: f64) {
        let state = *self.state.lock().unwrap();
        match state {
            State::Playing => {}
            State::Loading => return,
            State::Idle => return self.restart(seconds),
        }
        // One seek runs at a time, always to the latest target: a burst of seeks costs
        // one or two stream openings, and an older one can never land after a newer one.
        let mut seeking = self.seeking.lock().unwrap();
        seeking.target = Some(seconds);
        if seeking.running {
            return;
        }
        seeking.running = true;
        drop(seeking);
        let app = self.clone();
        std::thread::spawn(move || {
            loop {
                let Some(target) = app.seeking.lock().unwrap().target else { break };
                let result = app.playback.seek(target);
                let mut seeking = app.seeking.lock().unwrap();
                if seeking.target == Some(target) {
                    seeking.target = None;
                }
                drop(seeking);
                match result {
                    Ok(()) => {}
                    // YouTube no longer accepts the stream's link (it expires, or is turned
                    // down): a fresh one, at the place sought, rather than failing every
                    // seek from now on.
                    Err(e) if is_refused(&e) => {
                        log::write(format!("Seeking to {target:.1}s was refused ({e}): getting a fresh stream"));
                        app.seeking.lock().unwrap().target = None;
                        app.reconnect(target);
                        break;
                    }
                    Err(e) => {
                        log::write(format!("Couldn't seek to {target:.1}s: {e}"));
                        app.status(say::seek_failed());
                    }
                }
            }
            app.seeking.lock().unwrap().running = false;
        });
    }

    pub fn remove(self: &Arc<Self>, index: usize) {
        let was_current = {
            let mut queue = self.queue.lock().unwrap();
            let was_current = queue.current() == Some(index);
            queue.remove(index);
            was_current
        };
        self.emit(Event::QueueChanged);
        if was_current {
            self.generation.fetch_add(1, Ordering::SeqCst);
            self.playback.stop();
            *self.state.lock().unwrap() = State::Idle;
            self.advance(false);
            self.emit(Event::NowPlaying);
        }
    }

    /// Moves the queue's track at `from` to `to`.
    pub fn move_track(&self, from: usize, to: usize) {
        self.queue.lock().unwrap().move_track(from, to);
        self.emit(Event::QueueChanged);
        self.emit(Event::NowPlaying);
    }

    /// Moves the pin at `from` to `to`.
    pub fn move_pin(&self, from: usize, to: usize) {
        if let Err(e) = self.pins.lock().unwrap().move_pin(from, to) {
            self.couldnt_save("pins", &e);
        }
        self.emit(Event::SavedChanged);
    }

    pub fn clear(&self) {
        self.keep_levels();
        *self.levels.lock().unwrap() = None;
        self.generation.fetch_add(1, Ordering::SeqCst);
        *self.resume_at.lock().unwrap() = None;
        self.playback.stop();
        self.queue.lock().unwrap().clear();
        *self.state.lock().unwrap() = State::Idle;
        self.emit(Event::QueueChanged);
        self.emit(Event::NowPlaying);
    }

    /// Searches YouTube for `query`, then sends `Event::SearchDone`.
    pub fn search(self: &Arc<Self>, query: String) {
        let generation = {
            let mut search = self.search.lock().unwrap();
            *search =
                Search { query: query.clone(), generation: search.generation + 1, busy: true, ..Search::default() };
            search.generation
        };
        let app = self.clone();
        std::thread::spawn(move || {
            let page = extract::search(&query, 0);
            let mut search = app.search.lock().unwrap();
            if search.generation != generation {
                return;
            }
            search.busy = false;
            let done = page.map(|page| {
                search.read = page.len();
                search.done = page.len() < extract::SEARCH_PAGE;
                let mut shown = Vec::new();
                add_results(&mut shown, page);
                *app.results.lock().unwrap() = shown;
            });
            drop(search);
            app.emit(Event::SearchDone(done));
        });
    }

    /// Reads the next page of the search's results, unless one is on its way, there are no
    /// more, or the limit is reached; then sends `Event::SearchMore`. Whether it started.
    pub fn search_more(self: &Arc<Self>) -> bool {
        let (query, from, generation) = {
            let mut search = self.search.lock().unwrap();
            if search.busy || search.done || search.query.is_empty() {
                return false;
            }
            search.busy = true;
            (search.query.clone(), search.read, search.generation)
        };
        let app = self.clone();
        std::thread::spawn(move || {
            let page = extract::search(&query, from);
            let mut search = app.search.lock().unwrap();
            if search.generation != generation {
                return;
            }
            search.busy = false;
            let done = page.map(|page| {
                search.read += page.len();
                let full = page.len() >= extract::SEARCH_PAGE;
                let mut shown = app.results.lock().unwrap();
                let new = add_results(&mut shown, page);
                search.done = !full || new == 0 || shown.len() >= extract::SEARCH_LIMIT;
            });
            drop(search);
            app.emit(Event::SearchMore(done));
        });
        true
    }

    pub fn search_results(&self) -> Vec<Track> {
        self.results.lock().unwrap().clone()
    }

    /// Asks yt-dlp its version, sending `Event::YtDlpVersion`.
    pub fn check_ytdlp(self: &Arc<Self>) {
        let app = self.clone();
        std::thread::spawn(move || {
            let version = extract::version().unwrap_or_else(|_| "not found".into());
            app.emit(Event::YtDlpVersion(version));
        });
    }

    pub fn update_ytdlp(self: &Arc<Self>) {
        self.status(say::updating_ytdlp());
        let app = self.clone();
        std::thread::spawn(move || match app.run_ytdlp_update() {
            Ok(Updated::To(version)) => app.status(say::ytdlp_updated(&version)),
            Ok(Updated::Current(_)) => app.status(say::ytdlp_current()),
            Err(e) => {
                log::write(format!("Couldn't update yt-dlp: {e}"));
                app.status(say::ytdlp_update_failed());
            }
        });
    }

    /// Turns updating yt-dlp without being asked on or off (Settings). Off, it's updated
    /// only from Settings.
    pub fn set_auto_updates(&self, on: bool) {
        self.auto_updates.store(on, Ordering::SeqCst);
    }

    fn auto_updates(&self) -> bool {
        self.auto_updates.load(Ordering::SeqCst)
    }

    /// Updates yt-dlp in the background when it hasn't been checked for a week. YouTube
    /// changes often, and an old yt-dlp is the likeliest reason for links to stop working.
    /// Quiet unless there was something new.
    pub fn update_ytdlp_if_due(self: &Arc<Self>) {
        let due = store::last_update_check(&self.data)
            .is_none_or(|at| at.elapsed().unwrap_or(YTDLP_CHECK_INTERVAL) >= YTDLP_CHECK_INTERVAL);
        if !due || !self.auto_updates() {
            return;
        }
        let app = self.clone();
        std::thread::spawn(move || {
            // Opening the window and its first link come first.
            std::thread::sleep(YTDLP_CHECK_DELAY);
            // Turned off in the meantime.
            if !app.auto_updates() {
                return;
            }
            if let Ok(Updated::To(version)) = app.run_ytdlp_update() {
                app.status(say::ytdlp_updated(&version));
            }
        });
    }

    /// Several tracks in a row failed to load: YouTube has likely changed, and a newer
    /// yt-dlp is the usual cure. Updates it now rather than at the weekly check, at most
    /// once an hour, so a network that's down doesn't keep asking. With automatic updates
    /// off, it only suggests updating.
    fn rescue_ytdlp(self: &Arc<Self>) {
        {
            let mut rescued = self.rescued.lock().unwrap();
            if rescued.is_some_and(|at| at.elapsed() < RESCUE_INTERVAL) {
                return;
            }
            *rescued = Some(Instant::now());
        }
        self.failures.store(0, Ordering::SeqCst);
        if !self.auto_updates() {
            log::write("Tracks kept failing to load; automatic updates are off, so yt-dlp wasn't updated");
            self.status(say::ytdlp_may_be_old());
            return;
        }
        let app = self.clone();
        std::thread::spawn(move || match app.run_ytdlp_update() {
            Ok(Updated::To(version)) => {
                log::write(format!("Tracks kept failing to load: updated yt-dlp to {version}"));
                app.status(say::ytdlp_rescued());
            }
            Ok(Updated::Current(_)) => log::write("Tracks kept failing to load; yt-dlp is already up to date"),
            Err(e) => log::write(format!("Tracks kept failing to load; couldn't update yt-dlp: {e}")),
        });
    }

    fn run_ytdlp_update(self: &Arc<Self>) -> Result<Updated, String> {
        let result = extract::update();
        // Checked is checked: a yt-dlp that can't update itself (installed by pip, say)
        // is not asked again every start.
        store::mark_update_check(&self.data).ok();
        self.check_ytdlp();
        result
    }

    pub fn history(&self) -> Vec<SavedTrack> {
        self.history.lock().unwrap().entries().to_vec()
    }

    pub fn remove_from_history(&self, url: &str) {
        if let Err(e) = self.history.lock().unwrap().remove(url) {
            self.couldnt_save("history", &e);
        }
        self.emit(Event::SavedChanged);
    }

    pub fn pins(&self) -> Vec<SavedTrack> {
        self.pins.lock().unwrap().entries().to_vec()
    }

    pub fn is_pinned(&self, url: &str) -> bool {
        self.pins.lock().unwrap().contains(url)
    }

    pub fn toggle_pin(&self, track: &Track) {
        if let Err(e) = self.pins.lock().unwrap().toggle(track) {
            self.couldnt_save("pins", &e);
        }
        self.emit(Event::SavedChanged);
    }

    pub fn clear_history(&self) {
        if let Err(e) = self.history.lock().unwrap().clear() {
            self.couldnt_save("history", &e);
        }
        self.emit(Event::SavedChanged);
    }

    pub fn repeat(&self) -> Repeat {
        self.queue.lock().unwrap().repeat()
    }

    pub fn set_repeat(&self, repeat: Repeat) {
        self.queue.lock().unwrap().set_repeat(repeat);
    }

    pub fn shuffle(&self) -> bool {
        self.queue.lock().unwrap().shuffle()
    }

    pub fn set_shuffle(&self, on: bool) {
        self.queue.lock().unwrap().set_shuffle(on);
        self.emit(Event::QueueChanged);
    }

    pub fn cycle_repeat(&self) -> Repeat {
        let mut queue = self.queue.lock().unwrap();
        let repeat = queue.repeat().cycled();
        queue.set_repeat(repeat);
        repeat
    }

    pub fn toggle_shuffle(&self) -> bool {
        let mut queue = self.queue.lock().unwrap();
        let on = !queue.shuffle();
        queue.set_shuffle(on);
        on
    }

    /// Called a few times a second by the UI; moves on when a track ends.
    pub fn tick(self: &Arc<Self>) {
        self.save_session_if_due();
        let ended = {
            let mut state = self.state.lock().unwrap();
            if *state != State::Playing {
                return;
            }
            let ended = self.playback.finished();
            if ended {
                *state = State::Idle;
            } else {
                *self.last_position.lock().unwrap() = self.playback.position();
            }
            ended
        };
        if !ended {
            return;
        }
        let at = *self.last_position.lock().unwrap();
        let duration = self.now_playing().duration;
        if duration > 0.0 && at < duration - EARLY_END {
            self.reconnect(at);
        } else {
            self.advance(true);
        }
    }

    /// Gets a stream that stopped early back, at the same place: the network dropped,
    /// or YouTube's stream URL expired. Gives up, stopped, if the network stays away,
    /// rather than skipping through the rest of the queue.
    fn reconnect(self: &Arc<Self>, at: f64) {
        let Some((index, track)) = ({
            let queue = self.queue.lock().unwrap();
            queue.current().and_then(|i| queue.track(i).cloned().map(|t| (i, t)))
        }) else {
            return;
        };
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        // A paused track stays paused once it's back.
        let paused = self.playback.is_paused();
        *self.state.lock().unwrap() = State::Loading;
        self.emit(Event::NowPlaying);
        self.status(say::reconnecting());

        let app = self.clone();
        std::thread::spawn(move || {
            for delay in RECONNECT_DELAYS {
                std::thread::sleep(Duration::from_secs(delay));
                if app.generation.load(Ordering::SeqCst) != generation {
                    return;
                }
                // The old URL may be the problem: always ask for a fresh one.
                app.streams.lock().unwrap().remove(&track.url);
                let resumed = app
                    .stream_url(&track.url, extract::Client::Preferred)
                    .and_then(|url| app.playback.load(&url, at).map(|()| app.measure(&track, &url)));
                if app.generation.load(Ordering::SeqCst) != generation {
                    return;
                }
                if resumed.is_ok() {
                    if paused {
                        app.playback.pause();
                    }
                    *app.state.lock().unwrap() = State::Playing;
                    app.emit(Event::NowPlaying);
                    return;
                }
            }
            app.queue.lock().unwrap().select(index);
            *app.state.lock().unwrap() = State::Idle;
            app.emit(Event::NowPlaying);
            log::write(format!("Lost the connection to “{}” ({}) for good", track.title, track.url));
            app.status(say::connection_lost(&track.title));
        });
    }

    pub fn tracks(&self) -> Vec<Track> {
        self.queue.lock().unwrap().tracks().to_vec()
    }

    pub fn now_playing(&self) -> NowPlaying {
        let queue = self.queue.lock().unwrap();
        let index = queue.current();
        let track = index.and_then(|i| queue.track(i));
        NowPlaying {
            index,
            title: track.map(|t| t.title.clone()).unwrap_or_default(),
            channel: track.map(|t| t.channel.clone()).unwrap_or_default(),
            duration: track.and_then(|t| t.duration).unwrap_or(0.0),
            up_next: queue
                .peek_next(true)
                .filter(|&i| Some(i) != index)
                .and_then(|i| queue.track(i))
                .map(|t| t.title.clone()),
            loading: *self.state.lock().unwrap() == State::Loading || self.opening.load(Ordering::SeqCst) > 0,
            resume_at: *self.resume_at.lock().unwrap(),
        }
    }

    pub fn is_playing(&self) -> bool {
        *self.state.lock().unwrap() == State::Playing
    }

    fn emit(&self, event: Event) {
        if matches!(event, Event::QueueChanged | Event::NowPlaying) {
            self.version.fetch_add(1, Ordering::SeqCst);
        }
        (self.notify)(event);
    }

    /// Puts back the queue the last run left, paused where it was.
    pub fn restore_session(&self) {
        let session = Session::load(&self.data);
        if session.tracks.is_empty() {
            return;
        }
        // Marks saved by earlier versions, which took every failure as final, get one
        // more try; YouTube's own placeholders for deleted and private videos don't.
        let tracks = session.tracks.into_iter().map(|mut track| {
            if track.unavailable && !extract::is_placeholder_title(&track.title) {
                track.unavailable = false;
                track.failed = true;
            }
            track
        });
        {
            let mut queue = self.queue.lock().unwrap();
            queue.add(tracks);
            if let Some(current) = session.current {
                queue.select(current);
            }
        }
        *self.resume_at.lock().unwrap() = session.current.map(|_| session.position);
        self.emit(Event::QueueChanged);
        self.emit(Event::NowPlaying);
        *self.saved.lock().unwrap() = (self.version.load(Ordering::SeqCst), Instant::now());
    }

    /// The waveform of the track playing, as far as it is known.
    pub fn levels(&self) -> Option<Arc<Levels>> {
        self.levels.lock().unwrap().as_ref().map(|(_, levels)| levels.clone())
    }

    /// Measures the waveform of `track`, now playing from `url`, in the background, unless
    /// it is already known or being measured; saves it once it is whole.
    fn measure(self: &Arc<Self>, track: &Track, url: &str) {
        let levels = self.levels_for(track);
        if levels.complete() || !levels.start_measuring() {
            return;
        }
        let (app, url) = (self.clone(), url.to_string());
        std::thread::spawn(move || {
            let current = || app.levels().is_some_and(|now| Arc::ptr_eq(&now, &levels));
            // Playback has just started: leave it the network and the processor for now.
            std::thread::sleep(MEASURE_DELAY);
            // A failure leaves the shape partial; the next play or reconnect tries again.
            let _ = crate::engine::measure(&url, levels.clone(), current);
            levels.stop_measuring();
            if current() && levels.complete() {
                app.keep_levels();
            }
        });
    }

    /// The levels `track` is measured into: the ones already in use when it is the same
    /// track again (a seek, a reconnect, repeat-one), else its saved shape or a new one.
    fn levels_for(&self, track: &Track) -> Arc<Levels> {
        let mut current = self.levels.lock().unwrap();
        if let Some((url, levels)) = current.as_ref()
            && *url == track.url
        {
            return levels.clone();
        }
        drop(current);
        self.keep_levels();
        let duration = track.duration.unwrap_or(0.0);
        let saved =
            thumbs::video_id(&track.url).and_then(|id| self.waves.lock().unwrap().get(&id).map(<[f32]>::to_vec));
        let levels = match saved {
            Some(saved) => Levels::from_saved(duration, &saved),
            None => Levels::new(duration),
        };
        current = self.levels.lock().unwrap();
        *current = Some((track.url.clone(), levels.clone()));
        levels
    }

    /// Saves the measured waveform of the current track, for the next time it plays.
    pub fn keep_levels(&self) {
        let Some((url, levels)) = self.levels.lock().unwrap().clone() else { return };
        let (Some(id), Some(saved)) = (thumbs::video_id(&url), levels.saved()) else { return };
        if let Err(e) = self.waves.lock().unwrap().put(&id, saved) {
            self.couldnt_save("waveform", &e);
        }
    }

    /// Saves the queue and where playback is, for the next run.
    pub fn save_session(&self) {
        let version = self.version.load(Ordering::SeqCst);
        let (tracks, current) = {
            let queue = self.queue.lock().unwrap();
            (queue.tracks().to_vec(), queue.current())
        };
        let position = match *self.state.lock().unwrap() {
            State::Playing => self.playback.position(),
            _ => self.resume_at.lock().unwrap().unwrap_or(0.0),
        };
        if let Err(e) = (Session { tracks, current, position }).save(&self.data) {
            self.couldnt_save("queue", &e);
        }
        *self.saved.lock().unwrap() = (version, Instant::now());
    }

    fn save_session_if_due(&self) {
        let (version, at) = *self.saved.lock().unwrap();
        let changed = version != self.version.load(Ordering::SeqCst);
        let stale = self.is_playing() && at.elapsed() >= SESSION_SAVE_INTERVAL;
        if changed || stale {
            self.save_session();
        }
    }

    fn couldnt_save(&self, what: &str, error: &dyn std::fmt::Display) {
        log::write(format!("Couldn't save the {what}: {error}"));
        self.status(say::couldnt_save(what));
    }

    fn status(&self, message: impl Into<String>) {
        self.emit(Event::Status(message.into()));
    }

    /// The stream for `link`, remembered from earlier or asked of yt-dlp as `client`
    /// (the preferred client falls back to the default one by itself).
    fn stream_url(&self, link: &str, client: extract::Client) -> Result<String, String> {
        if let Some((url, at)) = self.streams.lock().unwrap().get(link)
            && at.elapsed() < STREAM_URL_LIFETIME
        {
            return Ok(url.clone());
        }
        let resolved = match client {
            extract::Client::Preferred => extract::resolve(link),
            extract::Client::Default => extract::resolve_with(link, client),
        }?;
        self.learn(link, &resolved);
        self.remember_stream(link, resolved.stream_url.clone());
        Ok(resolved.stream_url)
    }

    /// Fills in what a queued track didn't know yet from what yt-dlp says of it: tracks
    /// from a Library playlist come without their length (YouTube's playlist answer has
    /// none), and the waveform, the times and seeking all need it.
    fn learn(&self, link: &str, resolved: &extract::Resolved) {
        let changed = {
            let mut queue = self.queue.lock().unwrap();
            queue.learn(link, resolved.duration, &resolved.channel)
        };
        if changed {
            self.emit(Event::QueueChanged);
            self.emit(Event::NowPlaying);
        }
    }

    /// The queue's copy of a track, which may know more than an older copy of it.
    fn known(&self, track: &Track) -> Track {
        let queue = self.queue.lock().unwrap();
        queue.tracks().iter().find(|t| t.url == track.url).cloned().unwrap_or_else(|| track.clone())
    }

    fn remember_stream(&self, link: &str, url: String) {
        let mut streams = self.streams.lock().unwrap();
        streams.retain(|_, (_, at)| at.elapsed() < STREAM_URL_LIFETIME);
        streams.insert(link.to_string(), (url, Instant::now()));
    }

    /// Resolves the track after this one in the background, so it starts without a wait.
    fn prefetch_next(self: &Arc<Self>) {
        let next = {
            let queue = self.queue.lock().unwrap();
            queue.peek_next(true).and_then(|i| queue.track(i).map(|t| t.url.clone()))
        };
        if let Some(link) = next {
            // A failure here is found again, and reported, when the track comes up.
            let _ = self.stream_url(&link, extract::Client::Preferred);
        }
    }
}

/// What to tell the user when a playlist won't open. YouTube says a private playlist
/// "does not exist", so as not to reveal it, which reads as if the link were broken.
/// yt-dlp's error for a playlist it can't see: private, or gone (YouTube says the same).
fn is_private(error: &str) -> bool {
    let error = error.to_lowercase();
    error.contains("does not exist") || error.contains("private") || error.contains("unviewable")
}

/// An error saying the server turned the stream's link down (expired or refused), rather
/// than the network failing.
fn is_refused(error: &str) -> bool {
    ["status: 403", "status: 410", "status: 404", "status 403", "status 410", "status 404"]
        .iter()
        .any(|s| error.contains(s))
}

/// What a radio adds to the queue: the song, then the rest of its Mix (the Mix starts
/// with the song, but that isn't counted on); without the song when it's already playing.
fn radio_tracks(mut mix: Vec<Track>, song: &Track, playing: bool) -> Vec<Track> {
    let id = thumbs::video_id(&song.url);
    mix.retain(|t| thumbs::video_id(&t.url) != id);
    if !playing {
        mix.insert(0, song.clone());
    }
    mix
}

/// A listing's entries as tracks (placeholders for private or deleted videos marked).
pub fn playlist_tracks(listing: Listing) -> Vec<Track> {
    listing
        .entries
        .into_iter()
        .map(|e| Track {
            unavailable: extract::is_placeholder_title(&e.title),
            failed: false,
            url: e.url,
            title: e.title,
            duration: e.duration,
            channel: e.channel,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: &str) -> Track {
        Track {
            url: format!("https://www.youtube.com/watch?v={id}"),
            title: id.into(),
            duration: None,
            channel: String::new(),
            unavailable: false,
            failed: false,
        }
    }

    fn titles(queue: &Queue) -> Vec<&str> {
        queue.tracks().iter().map(|t| t.title.as_str()).collect()
    }

    #[test]
    fn radio_inserts_after_current_and_keeps_the_queue() {
        // The Mix, as listed: the song first (its link in another form), then others.
        let mut song_in_mix = track("song0000000");
        song_in_mix.url = "https://youtu.be/song0000000".into();
        let mix = vec![song_in_mix, track("mixA0000000"), track("mixB0000000")];

        // The song is playing: it keeps playing, the Mix follows it, the queue after that.
        let mut queue = Queue::new();
        queue.add([track("song0000000"), track("next0000000")]);
        queue.select(0);
        queue.insert_next(radio_tracks(mix.clone(), &track("song0000000"), true));
        assert_eq!(titles(&queue), ["song0000000", "mixA0000000", "mixB0000000", "next0000000"]);
        assert_eq!(queue.current(), Some(0));

        // Another song: it comes right after the current track, once, then its Mix.
        let mut queue = Queue::new();
        queue.add([track("play0000000"), track("next0000000")]);
        queue.select(0);
        let first = queue.insert_next(radio_tracks(mix, &track("song0000000"), false));
        assert_eq!(first, 1);
        assert_eq!(titles(&queue), ["play0000000", "song0000000", "mixA0000000", "mixB0000000", "next0000000"]);
    }

    #[test]
    fn a_private_playlist_is_told_from_other_failures() {
        assert!(is_private("ERROR: [youtube:tab] PLx: The playlist does not exist."));
        assert!(is_private("ERROR: [youtube:tab] PLx: This playlist is private"));
        assert!(!is_private("ERROR: unable to download webpage: timed out"));
    }

    #[test]
    fn a_refused_stream_link_is_told_from_a_network_failure() {
        assert!(is_refused("network error: http status: 403"));
        assert!(is_refused("http status: 410"));
        assert!(!is_refused("network error: timed out"));
        assert!(!is_refused("could not decode the audio: end of stream"));
    }

    #[test]
    fn more_results_leave_out_repeats_and_stop_at_the_limit() {
        let entry = |id: usize| extract::Entry {
            url: format!("https://www.youtube.com/watch?v={id:011}"),
            title: format!("Result {id}"),
            duration: Some(60.0),
            channel: String::new(),
        };
        let mut shown = Vec::new();
        assert_eq!(super::add_results(&mut shown, (0..20).map(entry).collect()), 20);
        // The next page overlaps the first by two (YouTube's order shifted).
        assert_eq!(super::add_results(&mut shown, (18..38).map(entry).collect()), 18);
        assert_eq!(shown.len(), 38);
        assert_eq!(shown[20].title, "Result 20");
        let mut many = Vec::new();
        super::add_results(&mut many, (0..250).map(entry).collect());
        assert_eq!(many.len(), extract::SEARCH_LIMIT);
    }
}
