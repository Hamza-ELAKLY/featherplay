//! What the player remembers between runs: settings and recently played tracks.
//!
//! The app is portable, so it keeps its files in a `data` folder beside the executable.
//! Where that can't be written (installed system-wide, say) it falls back to the
//! user's config directory.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::queue::{Repeat, Track};

pub const HISTORY_LIMIT: usize = 30;
pub const WAVES_KEPT: usize = 300;

pub fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("FEATHERPLAY_DATA") {
        return PathBuf::from(dir);
    }
    let beside_exe = std::env::current_exe().ok().and_then(|exe| exe.parent().map(|d| d.join("data")));
    if let Some(dir) = beside_exe.filter(|d| writable(d)) {
        return dir;
    }
    let config = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::home_dir().map(|h| h.join(".config")))
    };
    config.unwrap_or_else(std::env::temp_dir).join("featherplay")
}

fn writable(dir: &Path) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let probe = dir.join(".write-test");
    let ok = std::fs::write(&probe, b"").is_ok();
    std::fs::remove_file(probe).ok();
    ok
}

/// Reads a JSON file, or gives the default if it is missing or unreadable: a damaged
/// settings file should cost the user their settings, not the app.
fn load<T: for<'de> Deserialize<'de> + Default>(path: &Path) -> T {
    std::fs::read(path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default()
}

/// Writes through a temporary file and a rename, so a crash mid-write can't leave half a file.
fn save<T: Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let temp = path.with_extension("tmp");
    std::fs::write(&temp, serde_json::to_vec_pretty(value)?)?;
    std::fs::rename(temp, path)
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub volume: f32,
    pub repeat: Repeat,
    pub shuffle: bool,
    /// Where the window was and how big, to open it there again.
    pub window: Option<WindowPlace>,
    pub theme: ThemeMode,
    /// Thumbnails on list rows; the cover of the track playing shows either way.
    pub thumbnails: bool,
    /// The time left under the waveform, instead of the track's length.
    pub remaining: bool,
    /// Checks for new versions of Featherplay (daily) and yt-dlp (weekly) on its own. Off,
    /// they're checked only when asked, in Settings.
    pub auto_updates: bool,
    /// A Google client of one's own for signing in, instead of Featherplay's (advanced; set
    /// by hand in settings.json).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub google_client_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub google_client_secret: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            volume: 0.8,
            repeat: Repeat::Off,
            shuffle: false,
            window: None,
            theme: ThemeMode::Auto,
            thumbnails: true,
            remaining: false,
            auto_updates: true,
            google_client_id: None,
            google_client_secret: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    /// Follows the system's light or dark mode.
    #[default]
    Auto,
    Dark,
    Light,
}

/// The window's place on screen, in physical pixels, and the scale of the screen it was
/// on (1.25 at 125 %): restoring the size has to undo it, as Windows rescales a window
/// moved between screens of different scales.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowPlace {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub maximized: bool,
    #[serde(default = "unscaled")]
    pub scale: f32,
}

fn unscaled() -> f32 {
    1.0
}

impl WindowPlace {
    /// Whether this looks like a place a window can really be: not one of the far-off
    /// coordinates systems give minimised windows, and of a sensible size. A monitor
    /// unplugged since can still leave it off screen; then it opens where the system
    /// puts new windows instead, once the user moves it.
    pub fn plausible(&self) -> bool {
        (-2000..20000).contains(&self.x)
            && (-2000..20000).contains(&self.y)
            && (200..10000).contains(&self.width)
            && (200..10000).contains(&self.height)
            && (0.5..=4.0).contains(&self.scale)
    }

    /// The size in units independent of the screen's scale, to restore it with: it then
    /// comes out the same whichever screen the window is made on before it moves to its own.
    pub fn logical_size(&self) -> (f32, f32) {
        (self.width as f32 / self.scale, self.height as f32 / self.scale)
    }
}

impl Settings {
    pub fn load(dir: &Path) -> Self {
        let mut settings: Self = load(&dir.join("settings.json"));
        settings.volume = settings.volume.clamp(0.0, 1.0);
        settings.window = settings.window.filter(WindowPlace::plausible);
        settings
    }

    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        save(&dir.join("settings.json"), self)
    }
}

/// When yt-dlp was last checked for an update, if ever.
pub fn last_update_check(dir: &Path) -> Option<std::time::SystemTime> {
    last_check(dir, "ytdlp-checked")
}

pub fn mark_update_check(dir: &Path) -> std::io::Result<()> {
    mark_check(dir, "ytdlp-checked")
}

/// When something was last checked for (an update), as noted in the file `name`.
pub fn last_check(dir: &Path, name: &str) -> Option<std::time::SystemTime> {
    let seconds: u64 = std::fs::read_to_string(dir.join(name)).ok()?.trim().parse().ok()?;
    Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds))
}

pub fn mark_check(dir: &Path, name: &str) -> std::io::Result<()> {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join(name), now.to_string())
}

/// The measured waveforms of recently played tracks, by video id, so a track heard
/// before shows its whole shape at once. Kept to the most recent few hundred.
pub struct Waves {
    path: PathBuf,
    /// Least recently played first.
    entries: Vec<(String, Vec<f32>)>,
}

impl Waves {
    pub fn load(dir: &Path) -> Self {
        let path = dir.join("waves.json");
        let mut entries: Vec<(String, Vec<f32>)> = load(&path);
        let excess = entries.len().saturating_sub(WAVES_KEPT);
        entries.drain(..excess);
        Self { path, entries }
    }

    pub fn get(&self, id: &str) -> Option<&[f32]> {
        self.entries.iter().find(|(key, _)| key == id).map(|(_, levels)| levels.as_slice())
    }

    pub fn put(&mut self, id: &str, levels: Vec<f32>) -> std::io::Result<()> {
        self.entries.retain(|(key, _)| key != id);
        self.entries.push((id.to_string(), levels));
        let excess = self.entries.len().saturating_sub(WAVES_KEPT);
        self.entries.drain(..excess);
        save(&self.path, &self.entries)
    }
}

/// The queue as it was, to pick up where the last run left off.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    pub tracks: Vec<Track>,
    pub current: Option<usize>,
    /// Seconds into the current track.
    pub position: f64,
}

impl Session {
    pub fn load(dir: &Path) -> Self {
        let mut session: Self = load(&dir.join("session.json"));
        // A file edited by hand, or from an older version, must not point past the queue.
        if session.current.is_some_and(|c| c >= session.tracks.len()) {
            session.current = None;
        }
        session.position = session.position.max(0.0);
        session
    }

    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        save(&dir.join("session.json"), self)
    }
}

/// A track as history and pins keep it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedTrack {
    pub url: String,
    pub title: String,
    pub duration: Option<f64>,
    #[serde(default)]
    pub channel: String,
}

impl From<&Track> for SavedTrack {
    fn from(track: &Track) -> Self {
        SavedTrack {
            url: track.url.clone(),
            title: track.title.clone(),
            duration: track.duration,
            channel: track.channel.clone(),
        }
    }
}

impl From<&SavedTrack> for Track {
    fn from(entry: &SavedTrack) -> Self {
        Track {
            url: entry.url.clone(),
            title: entry.title.clone(),
            duration: entry.duration,
            channel: entry.channel.clone(),
            unavailable: false,
            failed: false,
        }
    }
}

/// The most recently played tracks, newest first, each at most once.
pub struct History {
    path: PathBuf,
    entries: Vec<SavedTrack>,
}

impl History {
    pub fn load(dir: &Path) -> Self {
        let path = dir.join("history.json");
        let mut entries: Vec<SavedTrack> = load(&path);
        entries.truncate(HISTORY_LIMIT);
        Self { path, entries }
    }

    pub fn entries(&self) -> &[SavedTrack] {
        &self.entries
    }

    pub fn record(&mut self, track: &Track) -> std::io::Result<()> {
        self.entries.retain(|e| e.url != track.url);
        self.entries.insert(0, SavedTrack::from(track));
        self.entries.truncate(HISTORY_LIMIT);
        save(&self.path, &self.entries)
    }

    pub fn remove(&mut self, url: &str) -> std::io::Result<()> {
        self.entries.retain(|e| e.url != url);
        save(&self.path, &self.entries)
    }

    pub fn clear(&mut self) -> std::io::Result<()> {
        self.entries.clear();
        save(&self.path, &self.entries)
    }
}

/// Tracks the user pinned to play again quickly, in the order they were pinned.
pub struct Pins {
    path: PathBuf,
    entries: Vec<SavedTrack>,
}

impl Pins {
    pub fn load(dir: &Path) -> Self {
        let path = dir.join("pins.json");
        Self { entries: load(&path), path }
    }

    pub fn entries(&self) -> &[SavedTrack] {
        &self.entries
    }

    pub fn contains(&self, url: &str) -> bool {
        self.entries.iter().any(|e| e.url == url)
    }

    /// Pins the track, or unpins it if it already is; returns whether it is now pinned.
    pub fn toggle(&mut self, track: &Track) -> std::io::Result<bool> {
        let pinned = !self.contains(&track.url);
        if pinned {
            self.entries.push(SavedTrack::from(track));
        } else {
            self.entries.retain(|e| e.url != track.url);
        }
        save(&self.path, &self.entries)?;
        Ok(pinned)
    }

    /// Moves the pin at `from` to `to`.
    pub fn move_pin(&mut self, from: usize, to: usize) -> std::io::Result<()> {
        if from >= self.entries.len() || to >= self.entries.len() || from == to {
            return Ok(());
        }
        let entry = self.entries.remove(from);
        self.entries.insert(to, entry);
        save(&self.path, &self.entries)
    }
}

/// A whole playlist pinned, to open or play again quickly.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PinnedList {
    pub id: String,
    pub title: String,
    /// Private lists open only when signed in, through the account.
    #[serde(default)]
    pub private: bool,
    /// The video whose thumbnail stands for it.
    #[serde(default)]
    pub thumb: Option<String>,
}

impl PinnedList {
    pub fn link(&self) -> String {
        format!("https://www.youtube.com/playlist?list={}", self.id)
    }
}

/// Playlists the user pinned, in the order they were pinned.
pub struct PinnedLists {
    path: PathBuf,
    entries: Vec<PinnedList>,
}

impl PinnedLists {
    pub fn load(dir: &Path) -> Self {
        let path = dir.join("pinned-lists.json");
        Self { entries: load(&path), path }
    }

    pub fn entries(&self) -> &[PinnedList] {
        &self.entries
    }

    pub fn contains(&self, id: &str) -> bool {
        self.entries.iter().any(|e| e.id == id)
    }

    /// Pins the playlist, or unpins it if it already is; returns whether it is now pinned.
    pub fn toggle(&mut self, list: &PinnedList) -> std::io::Result<bool> {
        let pinned = !self.contains(&list.id);
        if pinned {
            self.entries.push(list.clone());
        } else {
            self.entries.retain(|e| e.id != list.id);
        }
        save(&self.path, &self.entries)?;
        Ok(pinned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("featherplay-test-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        dir
    }

    fn track(n: usize) -> Track {
        Track {
            url: format!("https://example.com/{n}"),
            title: format!("Track {n}"),
            duration: Some(n as f64),
            channel: String::new(),
            unavailable: false,
            failed: false,
        }
    }

    #[test]
    fn history_keeps_the_newest_once_each_and_survives_a_restart() {
        let dir = temp_dir("history");
        let mut history = History::load(&dir);
        for n in 0..40 {
            history.record(&track(n)).unwrap();
        }
        history.record(&track(35)).unwrap();
        assert_eq!(history.entries().len(), HISTORY_LIMIT);
        assert_eq!(history.entries()[0].title, "Track 35");
        assert_eq!(history.entries()[1].title, "Track 39");
        assert_eq!(history.entries().iter().filter(|e| e.title == "Track 35").count(), 1);

        let reloaded = History::load(&dir);
        assert_eq!(reloaded.entries(), history.entries());

        history.clear().unwrap();
        assert!(History::load(&dir).entries().is_empty());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn history_entries_can_be_removed_one_at_a_time() {
        let dir = temp_dir("history-remove");
        let mut history = History::load(&dir);
        for n in 0..3 {
            history.record(&track(n)).unwrap();
        }
        history.remove("https://example.com/1").unwrap();
        let titles: Vec<_> = History::load(&dir).entries().iter().map(|e| e.title.clone()).collect();
        assert_eq!(titles, ["Track 2", "Track 0"]);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn pins_toggle_keep_their_order_and_survive_a_restart() {
        let dir = temp_dir("pins");
        let mut pins = Pins::load(&dir);
        assert!(pins.toggle(&track(1)).unwrap());
        assert!(pins.toggle(&track(2)).unwrap());
        assert!(pins.toggle(&track(3)).unwrap());
        assert!(!pins.toggle(&track(2)).unwrap(), "pinning again unpins");
        assert!(pins.contains("https://example.com/3") && !pins.contains("https://example.com/2"));

        let reloaded = Pins::load(&dir);
        let titles: Vec<_> = reloaded.entries().iter().map(|e| e.title.as_str()).collect();
        assert_eq!(titles, ["Track 1", "Track 3"]);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_session_round_trips_and_a_bad_one_is_made_safe() {
        let dir = temp_dir("session");
        assert_eq!(Session::load(&dir), Session::default(), "no file yet");

        let session = Session { tracks: vec![track(1), track(2)], current: Some(1), position: 42.5 };
        session.save(&dir).unwrap();
        assert_eq!(Session::load(&dir), session);

        Session { tracks: vec![track(1)], current: Some(5), position: -3.0 }.save(&dir).unwrap();
        let repaired = Session::load(&dir);
        assert_eq!((repaired.current, repaired.position), (None, 0.0));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn the_update_check_time_is_remembered() {
        let dir = temp_dir("update-check");
        assert!(last_update_check(&dir).is_none(), "never checked");
        mark_update_check(&dir).unwrap();
        let age = last_update_check(&dir).unwrap().elapsed().unwrap_or_default();
        assert!(age.as_secs() < 5);
        std::fs::write(dir.join("ytdlp-checked"), "garbage").unwrap();
        assert!(last_update_check(&dir).is_none());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn waves_keep_the_most_recent_and_survive_a_restart() {
        let dir = temp_dir("waves");
        let mut waves = Waves::load(&dir);
        for n in 0..WAVES_KEPT + 5 {
            waves.put(&format!("id{n}"), vec![n as f32]).unwrap();
        }
        waves.put("id10", vec![0.5]).unwrap();
        let reloaded = Waves::load(&dir);
        assert!(reloaded.get("id0").is_none(), "the oldest went");
        assert_eq!(reloaded.get("id10"), Some(&[0.5][..]), "played again, it stays and updates");
        assert_eq!(reloaded.get(&format!("id{}", WAVES_KEPT + 4)), Some(&[(WAVES_KEPT + 4) as f32][..]));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn settings_round_trip_and_damaged_files_fall_back_to_defaults() {
        let dir = temp_dir("settings");
        assert_eq!(Settings::load(&dir), Settings::default(), "no file yet");

        let window = Some(WindowPlace { x: 100, y: 80, width: 650, height: 1025, maximized: false, scale: 1.25 });
        let settings = Settings {
            volume: 0.35,
            repeat: Repeat::One,
            shuffle: true,
            window,
            theme: ThemeMode::Light,
            thumbnails: false,
            remaining: true,
            auto_updates: false,
            google_client_id: Some("mine.apps.googleusercontent.com".into()),
            google_client_secret: None,
        };
        settings.save(&dir).unwrap();
        assert_eq!(Settings::load(&dir), settings);

        std::fs::write(dir.join("settings.json"), b"{ not json").unwrap();
        assert_eq!(Settings::load(&dir), Settings::default());

        std::fs::write(dir.join("settings.json"), br#"{"volume": 7, "shuffle": true}"#).unwrap();
        let partial = Settings::load(&dir);
        assert_eq!((partial.volume, partial.shuffle, partial.repeat), (1.0, true, Repeat::Off));
        assert!(partial.auto_updates, "files from before the switch keep updating on their own");

        std::fs::write(
            dir.join("settings.json"),
            br#"{"window": {"x": -32000, "y": -32000, "width": 160, "height": 28}}"#,
        )
        .unwrap();
        assert_eq!(Settings::load(&dir).window, None, "a minimised window's far-off place is not kept");

        std::fs::write(dir.join("history.json"), b"garbage").unwrap();
        assert!(History::load(&dir).entries().is_empty());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_window_place_restores_its_size_whatever_the_screens_scale() {
        // Saved on a screen at 100 %: 698 × 827 pixels.
        let place = WindowPlace { x: -1091, y: 104, width: 698, height: 827, maximized: false, scale: 1.0 };
        assert_eq!(place.logical_size(), (698.0, 827.0));
        // At 125 %: the same window is 1.25 times as many pixels.
        let place = WindowPlace { scale: 1.25, width: 650, height: 1025, ..place };
        assert_eq!(place.logical_size(), (520.0, 820.0));
        // Places saved before the scale was kept read as unscaled.
        let old: WindowPlace = serde_json::from_str(r#"{"x":1,"y":2,"width":500,"height":800}"#).unwrap();
        assert_eq!((old.scale, old.maximized), (1.0, false));
    }

    #[test]
    fn pinned_playlists_persist() {
        let dir = temp_dir("pinned-lists");
        std::fs::create_dir_all(&dir).unwrap();
        let list = PinnedList { id: "PLx".into(), title: "Late night".into(), private: true, thumb: None };
        let mut lists = PinnedLists::load(&dir);
        assert!(lists.toggle(&list).unwrap());
        let again = PinnedLists::load(&dir);
        assert_eq!(again.entries(), std::slice::from_ref(&list));
        assert_eq!(again.entries()[0].link(), "https://www.youtube.com/playlist?list=PLx");
        assert!(!lists.toggle(&list).unwrap(), "unpinned");
        assert!(PinnedLists::load(&dir).entries().is_empty());
        std::fs::remove_dir_all(dir).ok();
    }
}
