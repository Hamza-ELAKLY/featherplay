//! Four quick seeks forward add up: `cargo run --example seek_burst`.

use std::time::{Duration, Instant};

use featherplay::app::App;
use featherplay::engine::Engine;

fn main() {
    let engine = Engine::new().expect("audio output");
    let data = std::env::temp_dir().join(format!("featherplay-seek-{}", std::process::id()));
    let app = App::new(engine.playback(), &data, |_| {});
    app.open_link("https://youtu.be/aqz-KE-bpKQ".into(), true);
    let started = Instant::now();
    while !app.is_playing() {
        assert!(started.elapsed() < Duration::from_secs(30));
        app.tick();
        std::thread::sleep(Duration::from_millis(50));
    }
    let before = app.playback().position();
    for _ in 0..4 {
        app.seek_by(5.0);
        std::thread::sleep(Duration::from_millis(30)); // faster than a seek takes
    }
    std::thread::sleep(Duration::from_millis(1500));
    let after = app.playback().position();
    println!("from {before:.1}s, four quick +5s seeks: now {after:.1}s (moved {:.1}s)", after - before);
    assert!((after - before - 20.0).abs() < 2.5, "the four presses add up to about 20 s");
    println!("OK");
    std::fs::remove_dir_all(data).ok();
}
