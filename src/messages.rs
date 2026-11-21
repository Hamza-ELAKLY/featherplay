//! Every message the window shows, in one place: short enough to read at a glance and
//! to fit the window at its narrowest. Titles are shortened; the details of what went
//! wrong go to the log (`crate::log`), not here.

/// A title short enough for a message: about 32 characters, cut at a word.
pub fn short(title: &str) -> String {
    const MAX: usize = 32;
    let title = title.trim();
    if title.chars().count() <= MAX {
        return title.to_string();
    }
    let cut: String = title.chars().take(MAX).collect();
    let at = cut.rfind(' ').filter(|&i| i >= MAX / 2).unwrap_or(cut.len());
    format!("{}…", cut[..at].trim_end_matches([' ', '-', '–', '—', ',', '·', '|', ':', '(', '[']))
}

/// "1 track", "12 tracks".
pub fn count_tracks(count: usize) -> String {
    tracks(count)
}

fn tracks(count: usize) -> String {
    if count == 1 { "1 track".into() } else { format!("{count} tracks") }
}

// Playing and queueing.
pub fn playing(title: &str) -> String {
    format!("Playing · {}", short(title))
}
pub fn playing_next(title: &str) -> String {
    format!("Playing next · {}", short(title))
}
pub fn added(title: &str) -> String {
    format!("Added to the queue · {}", short(title))
}
pub fn already_queued(title: &str) -> String {
    format!("Already in the queue · {}", short(title))
}
pub fn already_playing(title: &str) -> String {
    format!("Already playing · {}", short(title))
}
pub fn playing_list(name: &str, count: usize) -> String {
    format!("Playing {} · {}", short(name), tracks(count))
}
pub fn added_list(name: &str, count: usize) -> String {
    format!("Added {} · {}", short(name), tracks(count))
}
pub fn end_of_queue() -> String {
    "End of the queue".into()
}
pub fn looking_up() -> String {
    "Looking up the link…".into()
}

// Radio.
pub fn starting_radio(title: &str) -> String {
    format!("Starting radio from {}…", short(title))
}
pub fn radio(title: &str, count: usize) -> String {
    format!("Radio from {} · {}", short(title), tracks(count))
}
pub fn no_radio(title: &str) -> String {
    format!("No radio for {} · it has no Mix", short(title))
}
pub fn radio_failed(title: &str) -> String {
    format!("Couldn't start radio from {}", short(title))
}

// Loading problems.
pub fn couldnt_load(title: &str) -> String {
    format!("Couldn't load {} · click it to retry", short(title))
}
/// `why` in a word or two: "private", "removed", "age-restricted"…
pub fn skipped(title: &str, why: &str) -> String {
    format!("Skipped {} · {why}", short(title))
}
pub fn link_failed() -> String {
    "Couldn't open that link".into()
}
pub fn private_playlist_sign_in() -> String {
    "Private playlist · sign in from the Library tab to open it".into()
}
pub fn private_elsewhere() -> String {
    "That playlist is private to another account, or gone".into()
}
pub fn list_next(name: &str, count: usize) -> String {
    format!("Up next · {} · {}", short(name), tracks(count))
}
pub fn playlist_failed() -> String {
    "Couldn't open the playlist".into()
}
pub fn reconnecting() -> String {
    "Reconnecting…".into()
}
pub fn connection_lost(title: &str) -> String {
    format!("Connection lost · {}", short(title))
}
pub fn seek_failed() -> String {
    "Couldn't seek there".into()
}

// Search.
pub fn searching(words: &str) -> String {
    format!("Searching for {}…", short(words))
}
pub fn search_failed() -> String {
    "Search failed · check the connection".into()
}
pub fn loading_more_results() -> String {
    "Loading more results…".into()
}
pub fn more_results_failed() -> String {
    "Couldn't load more results · check the connection".into()
}

// yt-dlp and Featherplay itself.
pub fn updating_ytdlp() -> String {
    "Updating yt-dlp…".into()
}
pub fn ytdlp_updated(version: &str) -> String {
    format!("yt-dlp updated to {version}")
}
pub fn ytdlp_current() -> String {
    "yt-dlp is up to date".into()
}
pub fn ytdlp_update_failed() -> String {
    "Couldn't update yt-dlp".into()
}
pub fn ytdlp_rescued() -> String {
    "yt-dlp updated · try again".into()
}
pub fn ytdlp_may_be_old() -> String {
    "Tracks keep failing · updating yt-dlp in Settings may help".into()
}

// The YouTube account.
pub fn signed_in(name: &str) -> String {
    format!("Signed in · {}", short(name))
}
pub fn sign_in_cancelled() -> String {
    "Sign-in cancelled".into()
}
pub fn sign_in_timed_out() -> String {
    "Sign-in timed out · try again".into()
}
pub fn sign_in_failed() -> String {
    "Couldn't sign in · try again".into()
}
pub fn signed_out() -> String {
    "Signed out".into()
}
pub fn signed_out_offline() -> String {
    "Signed out · Google will be told once online".into()
}
pub fn signed_out_by_google() -> String {
    "Signed out by Google · sign in again".into()
}

// The library.
pub fn loading_list(name: &str) -> String {
    format!("Loading {}…", short(name))
}
pub fn library_quota() -> String {
    "YouTube's limit for today is reached · showing your saved library".into()
}
pub fn library_failed() -> String {
    "Couldn't refresh the library · check the connection".into()
}

// Saving.
pub fn couldnt_save(what: &str) -> String {
    format!("Couldn't save the {what}")
}
pub fn link_copied() -> String {
    "Link copied".into()
}

/// Each message, with `title` wherever a title goes: for checking they all fit.
pub fn every(title: &str) -> Vec<String> {
    vec![
        loading_more_results(),
        more_results_failed(),
        playing(title),
        playing_next(title),
        added(title),
        already_queued(title),
        already_playing(title),
        playing_list(title, 100),
        added_list(title, 100),
        end_of_queue(),
        looking_up(),
        starting_radio(title),
        radio(title, 100),
        no_radio(title),
        radio_failed(title),
        couldnt_load(title),
        skipped(title, "blocked in your country"),
        link_failed(),
        private_playlist_sign_in(),
        private_elsewhere(),
        list_next(title, 100),
        playlist_failed(),
        reconnecting(),
        connection_lost(title),
        seek_failed(),
        searching(title),
        search_failed(),
        updating_ytdlp(),
        ytdlp_updated("2026.11.12.123456"),
        ytdlp_current(),
        ytdlp_update_failed(),
        ytdlp_rescued(),
        ytdlp_may_be_old(),
        couldnt_save("settings"),
        signed_in(title),
        sign_in_cancelled(),
        sign_in_timed_out(),
        sign_in_failed(),
        signed_out(),
        signed_out_offline(),
        signed_out_by_google(),
        library_quota(),
        loading_list(title),
        library_failed(),
        link_copied(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_title_cuts_at_a_word() {
        let short = short("Daft Punk - Something About Us (Official Video) [Remastered 2024]");
        assert_eq!(short, "Daft Punk - Something About Us…");
        // No space to cut at: cut where the limit falls.
        assert_eq!(super::short(&"a".repeat(40)), format!("{}…", "a".repeat(32)));
    }

    #[test]
    fn short_title_keeps_short_titles() {
        assert_eq!(short("  Clair de Lune  "), "Clair de Lune");
        assert_eq!(short(&"b".repeat(32)), "b".repeat(32));
    }

    #[test]
    fn counts_read_right() {
        assert_eq!(playing_list("Mix", 1), "Playing Mix · 1 track");
        assert_eq!(added_list("Mix", 12), "Added Mix · 12 tracks");
    }
}
