//! Opens a link and reports what the queue became: `cargo run --example open_smoke -- <link>`.

use std::time::{Duration, Instant};

use featherplay::app::{App, Event};
use featherplay::engine::Engine;

fn main() {
    let link = std::env::args().nth(1).expect("usage: open_smoke <link>");
    let engine = Engine::new().expect("audio output");
    let app = App::new(engine.playback(), &std::env::temp_dir().join("featherplay-smoke"), |event| {
        if let Event::Status(s) = event
            && !s.is_empty()
            && s != "Buffering…"
        {
            println!("  status: {s}");
        }
    });
    let started = Instant::now();
    app.open_link(link, true);
    while started.elapsed() < Duration::from_secs(15) && !app.is_playing() {
        app.tick();
        std::thread::sleep(Duration::from_millis(50));
    }
    let now = app.now_playing();
    println!(
        "after {:.2?}: {} tracks, playing: {}, “{}” ({:.0}s)",
        started.elapsed(),
        app.tracks().len(),
        app.is_playing(),
        now.title,
        now.duration
    );
}
