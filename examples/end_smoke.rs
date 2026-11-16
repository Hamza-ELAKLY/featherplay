//! After the queue runs out, play and seek start the last track again:
//! `cargo run --example end_smoke`.

use std::time::{Duration, Instant};

use featherplay::app::App;
use featherplay::engine::Engine;

fn main() {
    let engine = Engine::new().expect("audio output");
    let data = std::env::temp_dir().join(format!("featherplay-end-{}", std::process::id()));
    let app = App::new(engine.playback(), &data, |_| {});
    let wait = |what: &str, done: &dyn Fn() -> bool| {
        let started = Instant::now();
        while !done() {
            app.tick();
            assert!(started.elapsed() < Duration::from_secs(40), "{what}: timed out");
            std::thread::sleep(Duration::from_millis(50));
        }
        println!("{what} after {:.1?}", started.elapsed());
    };

    app.open_link("https://www.youtube.com/watch?v=jNQXAC9IVRw".into(), true); // 19 seconds
    wait("playing", &|| app.is_playing());
    wait("end of the queue", &|| !app.is_playing() && !app.now_playing().loading);

    app.toggle_pause();
    wait("play pressed at the end: playing again", &|| app.is_playing());
    for _ in 0..6 {
        println!("  position {:.2}s", app.playback().position());
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(app.playback().position() < 3.0, "from the start");
    std::thread::sleep(Duration::from_secs(2));
    assert!(app.playback().position() > 1.0, "and the position moves");

    app.seek(17.5); // WSL's audio relay stalls after ~43 s, so skip ahead rather than wait
    std::thread::sleep(Duration::from_millis(500));
    wait("end of the queue again", &|| !app.is_playing() && !app.now_playing().loading);
    app.seek(12.0);
    wait("seeked while stopped: playing", &|| app.is_playing());
    let at = app.playback().position();
    println!("from {at:.1}s");
    assert!((at - 12.0).abs() < 1.5, "from where the slider was dropped");

    let track = app.tracks()[0].clone();
    app.toggle_pin(&track);
    assert!(app.is_pinned(&track.url));
    let again = App::new(engine.playback(), &data, |_| {});
    assert_eq!(again.pins().len(), 1, "pins survive a restart");
    app.toggle_pin(&track);
    assert!(app.pins().is_empty());
    println!("OK");
    std::fs::remove_dir_all(data).ok();
}
