//! A small log of what went wrong, in `featherplay.log` in the data folder: messages on
//! screen stay short, and the details (yt-dlp's own words, say) are kept here for bug
//! reports. It only grows when something fails, and is capped in size.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

pub const FILE: &str = "featherplay.log";
// Past this size, the older half is dropped.
const CAP: u64 = 256 * 1024;

static PATH: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Logs into `data` from now on. Until this is called, nothing is written.
pub fn init(data: &Path) {
    *PATH.lock().unwrap() = Some(data.join(FILE));
}

/// Adds a line: the time (UTC), then `what`.
pub fn write(what: impl std::fmt::Display) {
    let guard = PATH.lock().unwrap();
    let Some(path) = guard.as_ref() else { return };
    append(path, &redact(&format!("{}  {what}", timestamp(SystemTime::now()))), CAP);
}

/// Google's tokens never reach the log, whatever an error message quotes: access tokens
/// (`ya29.…`), refresh tokens (`1//…`) and sign-in codes (`4/…`) are blanked out.
fn redact(line: &str) -> String {
    const STARTS: [&str; 3] = ["ya29.", "1//", "4/0"];
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some((at, start)) = STARTS.iter().filter_map(|s| rest.find(s).map(|at| (at, *s))).min() {
        // Only at the start of a word: "v1//x" in a link isn't a token.
        let word_start = at == 0 || !rest[..at].ends_with(|c: char| c.is_ascii_alphanumeric());
        let end = rest[at..]
            .find(|c: char| !(c.is_ascii_alphanumeric() || "-_./+=%".contains(c)))
            .map_or(rest.len(), |e| at + e);
        out.push_str(&rest[..at]);
        out.push_str(if word_start && end - at > start.len() + 4 { "[token]" } else { &rest[at..end] });
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

fn append(path: &Path, line: &str, cap: u64) {
    let line = line.replace(['\n', '\r'], " ");
    if std::fs::metadata(path).is_ok_and(|m| m.len() > cap)
        && let Ok(old) = std::fs::read_to_string(path)
    {
        // Keep the newer half, from a line's start.
        let from = old.len() / 2;
        let from = old[from..].find('\n').map_or(old.len(), |i| from + i + 1);
        std::fs::write(path, &old[from..]).ok();
    }
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        writeln!(file, "{line}").ok();
    }
}

/// `2026-11-12 16:40:01`, in UTC.
fn timestamp(at: SystemTime) -> String {
    let secs = at.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let (days, rest) = (secs / 86_400, secs % 86_400);
    // Days since 1970 to a date (Howard Hinnant's civil_from_days).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}", rest / 3600, rest / 60 % 60, rest % 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn timestamps_are_dates() {
        assert_eq!(timestamp(UNIX_EPOCH), "1970-01-01 00:00:00");
        assert_eq!(timestamp(UNIX_EPOCH + Duration::from_secs(1_794_501_601)), "2026-11-12 16:40:01");
        assert_eq!(timestamp(UNIX_EPOCH + Duration::from_secs(951_782_400)), "2000-02-29 00:00:00");
    }

    #[test]
    fn log_never_contains_tokens() {
        let line = redact(
            "refresh failed for 1//0gAbC-dEf_123 with ya29.a0AfB_byC-xyz.123, code=4/0AeanS0Zx; see https://x.com/v1//y",
        );
        assert!(!line.contains("0gAbC") && !line.contains("a0AfB") && !line.contains("0AeanS"), "{line}");
        assert_eq!(line.matches("[token]").count(), 3, "{line}");
        assert!(line.contains("https://x.com/v1//y"), "not a token: {line}");
        // Every place that logs goes through `write`, and so through `redact`.
        for source in [include_str!("account.rs"), include_str!("google.rs")] {
            assert!(!source.contains("append("), "log::write only");
        }
    }

    #[test]
    fn the_log_keeps_details_and_stays_small() {
        let dir = std::env::temp_dir().join(format!("featherplay-log-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(FILE);
        std::fs::remove_file(&path).ok();
        append(&path, "Couldn't load “Song”: ERROR: [youtube] abc: Sign in to confirm\nsecond line", 1000);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("Sign in to confirm second line"), "one line per entry: {text}");
        for i in 0..200 {
            append(&path, &format!("entry {i:03} padding padding padding"), 1000);
        }
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.len() < 1100, "capped: {} bytes", text.len());
        assert!(text.contains("entry 199"), "the newest stays");
        assert!(text.lines().all(|l| l.starts_with("entry")), "cut at a line's start");
        std::fs::remove_dir_all(dir).ok();
    }
}
