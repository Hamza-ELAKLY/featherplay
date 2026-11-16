//! Plays a track queued without its length, as Library playlists give them, and checks
//! the player learns it (the waveform, the times and seeking need it):
//! `cargo run --example duration_smoke -- [link]`.

use std::time::{Duration, Instant};

use featherplay::app::App;
use featherplay::engine::Engine;
use featherplay::queue::Track;

fn main() {
    let link = std::env::args().nth(1).unwrap_or_else(|| "https://www.youtube.com/watch?v=dQw4w9WgXcQ".into());
    let engine = Engine::new().expect("audio output");
    let app = App::new(engine.playback(), &std::env::temp_dir().join("featherplay-duration-smoke"), |_| {});
    let track = Track {
        url: link,
        title: "Queued without a length".into(),
        duration: None,
        channel: String::new(),
        unavailable: false,
        failed: false,
    };
    app.add_track(track, true);
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(20) && !app.is_playing() {
        app.tick();
        std::thread::sleep(Duration::from_millis(50));
    }
    let now = app.now_playing();
    println!("playing: {}, length {:.0} s, channel “{}”", app.is_playing(), now.duration, now.channel);
    assert!(app.is_playing(), "it plays");
    assert!(now.duration > 0.0, "the length is known once it plays");
    assert_eq!(app.tracks()[0].duration, Some(now.duration));
    println!("ok");
}
