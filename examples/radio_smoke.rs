//! Lists a video's Mix, the way Start radio does, and checks it's a real radio: many
//! tracks, starting with the song. `cargo run --example radio_smoke -- [link]`.

use std::time::Instant;

use featherplay::extract;
use featherplay::thumbs;

fn main() {
    // A long-standing music video, unless another is given (videos that aren't music
    // mostly have no Mix: it lists only themselves).
    let song = std::env::args().nth(1).unwrap_or_else(|| "https://www.youtube.com/watch?v=dQw4w9WgXcQ".into());
    let link = extract::radio_link(&song).expect("a YouTube video link");
    let started = Instant::now();
    let listing = extract::list(&link).expect("the Mix lists");
    let first = listing.entries.first().map(|e| thumbs::video_id(&e.url));
    println!(
        "{} tracks in {:.1?}; first: {:?}; Mix: {:?}",
        listing.entries.len(),
        started.elapsed(),
        listing.entries.first().map(|e| &e.title),
        listing.title
    );
    assert!(listing.entries.len() >= 50, "a Mix has 50 or more tracks");
    assert_eq!(first, Some(thumbs::video_id(&song)), "the Mix starts with the song");
    println!("ok");
}
