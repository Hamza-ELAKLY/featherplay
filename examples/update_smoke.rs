//! Updates the yt-dlp named by FEATHERPLAY_YTDLP and reports what happened.

use featherplay::extract::{self, Updated};

fn main() {
    println!("before: {}", extract::version().expect("version"));
    match extract::update().expect("update") {
        Updated::To(version) => println!("updated to {version}"),
        Updated::Current(said) => println!("already current: {said}"),
    }
    println!("after: {}", extract::version().expect("version"));
}
