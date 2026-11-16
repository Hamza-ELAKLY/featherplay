//! Everything that asks yt-dlp about YouTube: resolving a link to an audio stream, and
//! listing the tracks of a playlist.
//!
//! yt-dlp only runs for the few seconds a question takes, then exits.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::RwLock;
use std::time::{Duration, Instant};

pub struct Resolved {
    pub title: String,
    pub duration: Option<f64>,
    /// The uploading channel, usually the artist; empty when YouTube doesn't say.
    pub channel: String,
    pub stream_url: String,
}

pub struct Entry {
    pub url: String,
    pub title: String,
    pub duration: Option<f64>,
    pub channel: String,
}

pub struct Listing {
    pub title: Option<String>,
    pub entries: Vec<Entry>,
    /// The entry the link pointed at, for a video link that also names a playlist.
    pub start: Option<usize>,
}

// AAC in MP4 is the format our decoder supports; YouTube offers it for nearly every video.
// Stereo only: some videos also offer 5.1 AAC, which the decoder can't read and which is
// three times the data for no gain in a music player. `<=?` keeps formats that don't say.
const AUDIO_FORMAT: &str = "bestaudio[ext=m4a][audio_channels<=?2]/bestaudio[acodec^=mp4a][audio_channels<=?2]";
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(45);
const LIST_TIMEOUT: Duration = Duration::from_secs(120);
// Mixes ("RD…" lists) never end; other playlists are read whole up to this many.
const MIX_LIMIT: usize = 100;
const PLAYLIST_LIMIT: usize = 2000;
/// Search results come this many at a time, more as the list is scrolled, up to the limit.
pub const SEARCH_PAGE: usize = 20;
pub const SEARCH_LIMIT: usize = 200;
// One line per playlist entry or search result: id, duration, channel, then the title.
const ENTRY_FORMAT: &str = "E\t%(id)s\t%(duration)s\t%(channel)s\t%(title)s";

/// Where yt-dlp is looked for: `FEATHERPLAY_YTDLP`, then next to our executable, then `PATH`.
pub fn ytdlp_path() -> PathBuf {
    if let Some(path) = std::env::var_os("FEATHERPLAY_YTDLP") {
        return PathBuf::from(path);
    }
    let name = if cfg!(windows) { "yt-dlp.exe" } else { "yt-dlp" };
    if let Some(dir) = std::env::current_exe().ok().and_then(|exe| exe.parent().map(PathBuf::from)) {
        let beside = dir.join(name);
        if beside.is_file() {
            return beside;
        }
    }
    PathBuf::from(name)
}

/// Whether a link names a list of videos (a playlist, a channel's uploads) rather than
/// (or as well as) a single video.
pub fn is_playlist(link: &str) -> bool {
    let path = link.split(['?', '#']).next().unwrap_or_default();
    query_param(link, "list").is_some()
        || ["/playlist", "/@", "/channel/", "/c/", "/user/"].iter().any(|p| path.contains(p))
}

/// The playlist id in a link (`list=`), if it names one.
pub fn list_id(link: &str) -> Option<String> {
    query_param(link, "list").map(String::from)
}

/// The plain video link inside a link to a video within a playlist.
pub fn video_in_playlist(link: &str) -> Option<String> {
    is_playlist(link).then(|| query_param(link, "v")).flatten().map(|v| format!("https://www.youtube.com/watch?v={v}"))
}

/// Which YouTube client yt-dlp asks for a track's stream as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Client {
    /// The visionOS client: its links play at once and need no JavaScript challenge
    /// solved, so they resolve faster. The links yt-dlp picks by default (the TV client,
    /// where a JavaScript runtime is installed) are often refused for a while at first.
    Preferred,
    /// Whatever yt-dlp picks: the fallback when the preferred client fails, as YouTube's
    /// clients come and go.
    Default,
}

/// Resolves a link with the preferred client, falling back to yt-dlp's own choice.
pub fn resolve(link: &str) -> Result<Resolved, String> {
    resolve_with(link, Client::Preferred).or_else(|e| match is_permanent(&e) {
        true => Err(e),
        false => resolve_with(link, Client::Default),
    })
}

pub fn resolve_with(link: &str, client: Client) -> Result<Resolved, String> {
    let preferred: &[&str] = match client {
        Client::Preferred => &["--extractor-args", "youtube:player_client=visionos"],
        Client::Default => &[],
    };
    let stdout = run(
        &[
            preferred,
            &[
                "--no-playlist",
                "-f",
                AUDIO_FORMAT,
                "--print",
                "title",
                "--print",
                "duration",
                "--print",
                "channel",
                "--print",
                "urls",
                "--",
                link,
            ],
        ]
        .concat(),
        RESOLVE_TIMEOUT,
    )?;
    let mut lines = stdout.lines();
    let (Some(title), Some(duration), Some(channel), Some(stream_url)) =
        (lines.next(), lines.next(), lines.next(), lines.next())
    else {
        return Err("yt-dlp returned no audio stream".into());
    };
    Ok(Resolved {
        title: title.into(),
        duration: duration.parse().ok(),
        channel: known(channel),
        stream_url: stream_url.into(),
    })
}

/// Lists a playlist's tracks without resolving any of them, which is fast.
pub fn list(link: &str) -> Result<Listing, String> {
    let list_id = query_param(link, "list").unwrap_or_default();
    let limit = if list_id.starts_with("RD") { MIX_LIMIT } else { PLAYLIST_LIMIT };
    let range = format!(":{limit}");
    let stdout = run(
        &[
            "--flat-playlist",
            "--yes-playlist",
            "-I",
            &range,
            "--print",
            ENTRY_FORMAT,
            "--print",
            "playlist:P\t%(title)s",
            "--",
            link,
        ],
        LIST_TIMEOUT,
    )?;
    let mut listing = parse_listing(&stdout);
    let video = query_param(link, "v");
    listing.start = video.and_then(|v| listing.entries.iter().position(|e| e.url.ends_with(&format!("={v}"))));
    if listing.entries.is_empty() {
        return Err("the playlist is empty or unavailable".into());
    }
    Ok(listing)
}

/// A video's plain YouTube link, for copying or opening; other links stay as they are.
pub fn watch_link(link: &str) -> String {
    match crate::thumbs::video_id(link) {
        Some(id) => format!("https://www.youtube.com/watch?v={id}"),
        None => link.to_string(),
    }
}

/// The link to YouTube's Mix for a video: its "radio".
pub fn radio_link(link: &str) -> Option<String> {
    crate::thumbs::video_id(link).map(|id| format!("https://www.youtube.com/watch?v={id}&list=RD{id}"))
}

/// Whether input is a link to open rather than words to search for.
pub fn is_link(text: &str) -> bool {
    let text = text.trim();
    ["http://", "https://", "www.", "youtu.be/", "youtube.com/", "m.youtube.com/", "music.youtube.com/"]
        .iter()
        .any(|prefix| text.starts_with(prefix))
}

/// YouTube's results for a query from position `from` (0 for the top), a page of them,
/// listed without resolving any. yt-dlp can't start a search part-way, so it asks for
/// everything up to the page's end and prints only the page.
pub fn search(query: &str, from: usize) -> Result<Vec<Entry>, String> {
    let end = from + SEARCH_PAGE;
    let target = format!("ytsearch{end}:{query}");
    let items = format!("{}-{end}", from + 1);
    let stdout = run(&["--flat-playlist", "-I", &items, "--print", ENTRY_FORMAT, "--", &target], RESOLVE_TIMEOUT)?;
    Ok(parse_listing(&stdout).entries)
}

pub fn version() -> Result<String, String> {
    run(&["--version"], RESOLVE_TIMEOUT).map(|out| out.trim().to_string())
}

/// Updates yt-dlp in place, returning what it said. Only the standalone builds can
/// update themselves; one installed by pip or a package manager says how to instead.
/// What an update did.
pub enum Updated {
    /// yt-dlp replaced itself with the version named.
    To(String),
    /// It was already the newest; what it said.
    Current(String),
}

/// Updates yt-dlp in place. It waits for lookups in progress to finish, and new ones
/// wait for it, since a running executable can't be replaced on Windows.
pub fn update() -> Result<Updated, String> {
    let _alone = IN_USE.write().unwrap_or_else(|e| e.into_inner());
    let out = run_unlocked(&["--update"], LIST_TIMEOUT)?;
    let said = out.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("done").trim().to_string();
    // "Updated yt-dlp to stable@2026.11.02 from yt-dlp/yt-dlp"
    Ok(match said.strip_prefix("Updated yt-dlp to ") {
        Some(rest) => Updated::To(rest.split(' ').next().unwrap_or(rest).to_string()),
        None => Updated::Current(said),
    })
}

/// yt-dlp prints "NA" for a field it doesn't have.
fn known(field: &str) -> String {
    if field == "NA" { String::new() } else { field.to_string() }
}

fn parse_listing(stdout: &str) -> Listing {
    let mut listing = Listing { title: None, entries: Vec::new(), start: None };
    for line in stdout.lines() {
        let mut fields = line.split('\t');
        match fields.next() {
            Some("E") => {
                let (Some(id), Some(duration), Some(channel)) = (fields.next(), fields.next(), fields.next()) else {
                    continue;
                };
                // The title comes last, so a tab in it can't shift the other fields.
                let title = fields.collect::<Vec<_>>().join("\t");
                if title.is_empty() {
                    continue;
                }
                listing.entries.push(Entry {
                    url: format!("https://www.youtube.com/watch?v={id}"),
                    title,
                    duration: duration.parse().ok(),
                    channel: known(channel),
                });
            }
            Some("P") => listing.title = fields.next().filter(|t| *t != "NA").map(String::from),
            _ => {}
        }
    }
    listing
}

/// Flat listings name removed videos by placeholder titles instead of leaving them out.
/// Whether yt-dlp's `error` says the video is gone or locked away for good (removed,
/// private, age-gated, members-only, blocked here), rather than that something went
/// wrong just now (the network, a timeout, a refused stream).
pub fn is_permanent(error: &str) -> bool {
    const FOR_GOOD: [&str; 14] = [
        "video unavailable",
        "video is unavailable",
        "video is not available",
        "private video",
        "has been removed",
        "no longer available",
        "account associated with this video has been terminated",
        "members-only",
        "join this channel",
        "sign in to confirm your age",
        "inappropriate for some users",
        "copyright",
        "not available in your country",
        "blocked it in your country",
    ];
    let error = error.to_lowercase();
    FOR_GOOD.iter().any(|phrase| error.contains(phrase))
}

/// Why a track can't play, in a word or two, from yt-dlp's error: for messages.
pub fn reason_gone(error: &str) -> &'static str {
    let error = error.to_lowercase();
    let has = |phrases: &[&str]| phrases.iter().any(|p| error.contains(p));
    if has(&["private video"]) {
        "private"
    } else if has(&["sign in to confirm your age", "inappropriate for some users"]) {
        "age-restricted"
    } else if has(&["members-only", "join this channel"]) {
        "members only"
    } else if has(&["not available in your country", "blocked it in your country"]) {
        "blocked in your country"
    } else if has(&["copyright"]) {
        "removed for copyright"
    } else if has(&["has been removed", "account associated with this video has been terminated"]) {
        "removed"
    } else {
        "unavailable"
    }
}

pub fn is_placeholder_title(title: &str) -> bool {
    matches!(title, "[Private video]" | "[Deleted video]" | "[Unavailable video]")
}

fn query_param<'a>(link: &'a str, name: &str) -> Option<&'a str> {
    let query = link.split_once('?')?.1.split('#').next()?;
    query.split('&').find_map(|pair| pair.strip_prefix(name)?.strip_prefix('=')).filter(|v| !v.is_empty())
}

/// Runs yt-dlp and returns its output, killing it if it outlives `timeout`.
// Lookups share yt-dlp; an update needs it to itself.
static IN_USE: RwLock<()> = RwLock::new(());

fn run(args: &[&str], timeout: Duration) -> Result<String, String> {
    let _shared = IN_USE.read().unwrap_or_else(|e| e.into_inner());
    run_unlocked(args, timeout)
}

fn run_unlocked(args: &[&str], timeout: Duration) -> Result<String, String> {
    let mut cmd = Command::new(ytdlp_path());
    // Every question here only asks; --simulate guarantees nothing is ever downloaded,
    // even where a `--print` of a later stage (like `playlist:`) would switch that off.
    // Piped, yt-dlp would write in the system's code page on Windows, mangling titles.
    cmd.args(["--no-warnings", "--simulate", "--encoding", "utf-8"])
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    hide_console(&mut cmd);
    let mut child = cmd.spawn().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => "yt-dlp was not found next to the app or on PATH".to_string(),
        _ => format!("could not run yt-dlp: {e}"),
    })?;

    // Drain both pipes on their own threads so a full pipe can't stall yt-dlp.
    let drain = |mut pipe: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut out = Vec::new();
            pipe.read_to_end(&mut out).ok();
            String::from_utf8_lossy(&out).into_owned()
        })
    };
    let stdout = drain(Box::new(child.stdout.take().expect("piped stdout")));
    let stderr = drain(Box::new(child.stderr.take().expect("piped stderr")));

    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().ok();
            child.wait().ok();
            return Err("yt-dlp took too long to answer".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let stdout = stdout.join().unwrap_or_default();
    let stderr = stderr.join().unwrap_or_default();
    if !status.success() {
        let reason = stderr.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("unknown error");
        return Err(reason.trim_start_matches("ERROR: ").to_string());
    }
    Ok(stdout)
}

#[cfg(windows)]
fn hide_console(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_console(_cmd: &mut Command) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn radio_link_for_a_video() {
        let mix = Some("https://www.youtube.com/watch?v=dQw4w9WgXcQ&list=RDdQw4w9WgXcQ".to_string());
        assert_eq!(radio_link("https://www.youtube.com/watch?v=dQw4w9WgXcQ"), mix);
        assert_eq!(radio_link("https://youtu.be/dQw4w9WgXcQ?si=abc"), mix);
        assert_eq!(radio_link("https://www.youtube.com/shorts/dQw4w9WgXcQ"), mix);
        assert_eq!(radio_link("https://music.youtube.com/watch?v=dQw4w9WgXcQ&list=PL123"), mix);
        assert_eq!(radio_link("https://example.com/song.mp3"), None);
    }

    #[test]
    fn youtube_link_for_a_track() {
        let plain = "https://www.youtube.com/watch?v=dQw4w9WgXcQ";
        assert_eq!(watch_link("https://youtu.be/dQw4w9WgXcQ"), plain);
        assert_eq!(watch_link("https://www.youtube.com/watch?v=dQw4w9WgXcQ&list=RDdQw4w9WgXcQ&index=3"), plain);
        assert_eq!(watch_link("https://example.com/song.mp3"), "https://example.com/song.mp3");
    }

    #[test]
    fn says_why_a_video_is_gone() {
        assert_eq!(reason_gone("[youtube] abc: Private video. Sign in if you've been granted access"), "private");
        assert_eq!(reason_gone("[youtube] abc: Sign in to confirm your age."), "age-restricted");
        assert_eq!(
            reason_gone("[youtube] abc: Video unavailable. This video has been removed by the uploader"),
            "removed"
        );
        assert_eq!(
            reason_gone("[youtube] abc: Join this channel to get access to members-only content"),
            "members only"
        );
        assert_eq!(
            reason_gone("[youtube] abc: The uploader has not made this video available in your country"),
            "unavailable"
        );
        assert_eq!(
            reason_gone("[youtube] abc: This video is not available in your country"),
            "blocked in your country"
        );
        assert_eq!(
            reason_gone("[youtube] abc: This video contains content from X, who has blocked it on copyright grounds"),
            "removed for copyright"
        );
        assert_eq!(reason_gone("[youtube] aaaaaaaaaaa: This video is unavailable"), "unavailable");
    }

    #[test]
    fn tells_a_video_gone_for_good_from_a_passing_failure() {
        assert!(is_permanent("[youtube] abc: Video unavailable. This video has been removed by the uploader"));
        assert!(is_permanent("[youtube] abc: Private video. Sign in if you've been granted access"));
        assert!(is_permanent(
            "[youtube] abc: Sign in to confirm your age. This video may be inappropriate for some users."
        ));
        assert!(is_permanent("[youtube] aaaaaaaaaaa: This video is unavailable"));
        assert!(!is_permanent("yt-dlp took too long to answer"));
        assert!(!is_permanent("HTTP status 403 Forbidden"));
        assert!(!is_permanent(
            "[youtube] abc: Unable to download API page: <urlopen error [Errno 11001] getaddrinfo failed>"
        ));
        assert!(!is_permanent("could not decode the audio: end of stream"));
    }

    #[test]
    fn finds_query_parameters() {
        let link = "https://www.youtube.com/watch?v=abc&list=RDabc&index=2#t=5";
        assert_eq!(query_param(link, "v"), Some("abc"));
        assert_eq!(query_param(link, "list"), Some("RDabc"));
        assert_eq!(query_param(link, "index"), Some("2"));
        assert_eq!(query_param("https://youtu.be/abc", "v"), None);
        assert_eq!(query_param("https://x.y/?vv=1&v=", "v"), None);
        assert!(is_playlist("https://www.youtube.com/playlist?list=PL123"));
        assert!(!is_playlist("https://www.youtube.com/watch?v=abc"));
        assert!(is_playlist("https://www.youtube.com/@SomeChannel/videos"));
        assert!(is_playlist("https://www.youtube.com/channel/UC123"));
        assert!(!is_playlist("https://youtu.be/abc?si=x/@y"));
        assert!(is_link("https://www.youtube.com/watch?v=abc"));
        assert!(is_link(" youtu.be/abc"));
        assert!(is_link("www.youtube.com/watch?v=abc"));
        assert!(!is_link("daft punk around the world"));
        assert!(!is_link("youth lagoon"), "a search may start like a domain");
        assert_eq!(video_in_playlist(link).as_deref(), Some("https://www.youtube.com/watch?v=abc"));
        assert_eq!(video_in_playlist("https://www.youtube.com/playlist?list=PL1"), None);
        assert_eq!(video_in_playlist("https://www.youtube.com/watch?v=abc"), None);
    }

    #[test]
    fn parses_a_flat_listing() {
        let out =
            "E\tabc\t214\tRick Astley\tFirst\tpart two\nE\tdef\tNA\tNA\t[Deleted video]\nnoise\nE\tbad\nP\tMy list\n";
        let listing = parse_listing(out);
        assert_eq!(listing.title.as_deref(), Some("My list"));
        assert_eq!(listing.entries.len(), 2);
        assert_eq!(listing.entries[0].url, "https://www.youtube.com/watch?v=abc");
        assert_eq!(listing.entries[0].duration, Some(214.0));
        assert_eq!(listing.entries[0].channel, "Rick Astley");
        assert_eq!(listing.entries[0].title, "First\tpart two", "a tab in a title stays in the title");
        assert_eq!(listing.entries[1].duration, None);
        assert_eq!(listing.entries[1].channel, "", "NA means unknown");
        assert!(is_placeholder_title(&listing.entries[1].title));
    }
}
