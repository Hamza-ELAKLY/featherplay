//! A stream that vanishes mid-track comes back at the same place: `cargo run --example dropout_smoke`.

use std::time::{Duration, Instant};

use featherplay::app::{App, Event};
use featherplay::engine::Engine;

fn main() {
    let engine = Engine::new().expect("audio output");
    let app = App::new(engine.playback(), &std::env::temp_dir().join("featherplay-dropout"), |event| {
        if let Event::Status(s) = event
            && !s.is_empty()
        {
            println!("  status: {s}");
        }
    });
    let run = |seconds: f64| {
        let until = Instant::now() + Duration::from_secs_f64(seconds);
        while Instant::now() < until {
            app.tick();
            std::thread::sleep(Duration::from_millis(50));
        }
    };

    app.open_link("https://youtu.be/aqz-KE-bpKQ".into(), true);
    let started = Instant::now();
    while !app.is_playing() {
        assert!(started.elapsed() < Duration::from_secs(30));
        run(0.1);
    }
    app.seek(120.0);
    run(3.0);
    let before = app.playback().position();
    println!("playing at {before:.1}s; cutting the stream");

    app.playback().stop(); // what a dead connection looks like to the app: the track just ends
    let cut = Instant::now();
    run(0.5);
    assert!(!app.is_playing(), "noticed the dropout");
    while !app.is_playing() {
        assert!(cut.elapsed() < Duration::from_secs(30), "never came back");
        run(0.1);
    }
    let after = app.playback().position();
    println!("back after {:.1?} at {after:.1}s, still track #{:?}", cut.elapsed(), app.now_playing().index);
    assert!((after - before).abs() < 2.0, "resumed where it stopped");
    assert_eq!(app.now_playing().index, Some(0), "did not skip the track");
    run(2.0);
    assert!(app.playback().position() > after + 1.0, "and keeps playing");
    println!("OK");
}
