//! The queue and position survive a restart: `cargo run --example session_smoke`.

use std::time::{Duration, Instant};

use featherplay::app::App;
use featherplay::engine::Engine;

fn main() {
    let engine = Engine::new().expect("audio output");
    let data = std::env::temp_dir().join(format!("featherplay-session-{}", std::process::id()));
    let ticks = |app: &std::sync::Arc<App>, seconds: f64| {
        let until = Instant::now() + Duration::from_secs_f64(seconds);
        while Instant::now() < until {
            app.tick();
            std::thread::sleep(Duration::from_millis(50));
        }
    };

    let first = App::new(engine.playback(), &data, |_| {});
    first.open_link("https://www.youtube.com/watch?v=dQw4w9WgXcQ&list=RDdQw4w9WgXcQ".into(), true);
    let started = Instant::now();
    while first.tracks().len() < 2 || !first.is_playing() {
        assert!(started.elapsed() < Duration::from_secs(40), "never got going");
        ticks(&first, 0.1);
    }
    first.advance(false); // the second track, so "current" is not just the first
    while !first.is_playing() {
        ticks(&first, 0.1);
    }
    first.seek(30.0);
    ticks(&first, 1.5);
    let (count, current, position) = (first.tracks().len(), first.now_playing().index, first.playback().position());
    println!("before quitting: {count} tracks, playing #{current:?} at {position:.1}s");
    first.save_session(); // what the window does on exit
    engine.playback().stop();
    drop(first);

    let second = App::new(engine.playback(), &data, |_| {});
    second.restore_session();
    let now = second.now_playing();
    println!(
        "after restarting: {} tracks, #{:?}, paused at {:?}, playing: {}",
        second.tracks().len(),
        now.index,
        now.resume_at,
        second.is_playing()
    );
    assert_eq!(second.tracks().len(), count);
    assert_eq!(now.index, current);
    assert!((now.resume_at.unwrap() - position).abs() < 1.0);
    assert!(!second.is_playing(), "restored paused, not playing by itself");

    second.toggle_pause();
    while !second.is_playing() {
        ticks(&second, 0.1);
    }
    let resumed = second.playback().position();
    println!("play pressed: continuing from {resumed:.1}s");
    assert!((resumed - position).abs() < 1.5);
    println!("OK");
    std::fs::remove_dir_all(data).ok();
}
