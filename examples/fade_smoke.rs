//! Pause, resume and stop ease the sound rather than cutting it: `cargo run --example fade_smoke`.

use std::time::{Duration, Instant};

use featherplay::app::App;
use featherplay::engine::Engine;

fn main() {
    let engine = Engine::new().expect("audio output");
    let data = std::env::temp_dir().join(format!("featherplay-fade-{}", std::process::id()));
    let app = App::new(engine.playback(), &data, |_| {});
    let playback = app.playback().clone();
    app.open_link("https://youtu.be/aqz-KE-bpKQ".into(), true);
    let started = Instant::now();
    while !app.is_playing() {
        assert!(started.elapsed() < Duration::from_secs(30));
        app.tick();
        std::thread::sleep(Duration::from_millis(50));
    }
    std::thread::sleep(Duration::from_secs(2));

    playback.pause();
    assert!(playback.is_paused(), "paused the moment it is asked");
    let at_press = playback.position();
    std::thread::sleep(Duration::from_millis(600));
    let settled = playback.position();
    std::thread::sleep(Duration::from_millis(600));
    let later = playback.position();
    println!("pause: pressed at {at_press:.2}s, sound eased out by {settled:.2}s, then held at {later:.2}s");
    assert!(settled - at_press < 0.3, "the fade is short");
    assert!((later - settled).abs() < 0.01, "and then it is really paused");

    playback.resume();
    assert!(!playback.is_paused());
    std::thread::sleep(Duration::from_millis(800));
    let resumed = playback.position();
    println!("resume: playing again, at {resumed:.2}s");
    assert!(resumed > later + 0.5, "playing again");

    playback.pause();
    std::thread::sleep(Duration::from_millis(400));
    app.seek(100.0);
    std::thread::sleep(Duration::from_millis(1500));
    let after_seek = playback.position();
    std::thread::sleep(Duration::from_millis(500));
    println!("seek while paused: at {after_seek:.2}s, still paused: {}", playback.is_paused());
    assert!(
        playback.is_paused() && (after_seek - 100.0).abs() < 1.0 && (playback.position() - after_seek).abs() < 0.01
    );

    playback.resume();
    std::thread::sleep(Duration::from_millis(500));
    playback.stop();
    std::thread::sleep(Duration::from_millis(400));
    assert!(playback.finished(), "stopped");
    println!("OK");
    std::fs::remove_dir_all(data).ok();
}
