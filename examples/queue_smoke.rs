//! Drives the whole app headlessly through a playlist: `cargo run --example queue_smoke -- <playlist link>`.

use std::time::{Duration, Instant};

use featherplay::app::{App, Event};
use featherplay::engine::Engine;

fn main() {
    let link = std::env::args().nth(1).expect("usage: queue_smoke <playlist link>");
    let engine = Engine::new().expect("audio output");
    let app = App::new(engine.playback(), &std::env::temp_dir().join("featherplay-smoke"), |event| {
        if let Event::Status(s) = event
            && !s.is_empty()
        {
            println!("  status: {s}");
        }
    });

    let wait_playing = |what: &str| {
        let started = Instant::now();
        while !app.is_playing() {
            app.tick();
            assert!(started.elapsed() < Duration::from_secs(60), "{what}: never started");
            std::thread::sleep(Duration::from_millis(50));
        }
        let now = app.now_playing();
        println!("{what}: #{:?} “{}” after {:.2?}", now.index, now.title, started.elapsed());
        now.index
    };
    let run = |seconds: u64| {
        for _ in 0..seconds * 20 {
            app.tick();
            std::thread::sleep(Duration::from_millis(50));
        }
    };

    let started = Instant::now();
    app.open_link(link, true);
    let first = wait_playing("first track");
    println!("link to sound in {:.2?}", started.elapsed());
    while app.tracks().len() < 2 {
        assert!(started.elapsed() < Duration::from_secs(60), "the playlist never filled in");
        std::thread::sleep(Duration::from_millis(50));
    }
    println!(
        "queue filled in to {} tracks after {:.2?}, still playing #{:?}",
        app.tracks().len(),
        started.elapsed(),
        app.now_playing().index
    );
    assert_eq!(app.now_playing().index, first);

    run(4); // lets the next track's URL prefetch
    app.advance(false);
    assert_eq!(wait_playing("next (prefetched)"), Some(1));

    app.previous();
    assert_eq!(wait_playing("previous"), Some(0));

    let duration = app.now_playing().duration;
    app.seek(duration - 2.0);
    run(1);
    let started = Instant::now();
    while app.now_playing().index == Some(0) {
        app.tick();
        assert!(started.elapsed() < Duration::from_secs(20), "never advanced");
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(wait_playing("auto-advance at the end"), Some(1));

    app.cycle_repeat();
    app.cycle_repeat(); // off -> all -> one
    let duration = app.now_playing().duration;
    app.seek(duration - 2.0);
    run(5);
    assert_eq!(wait_playing("repeat-one replays"), Some(1));
    assert!(app.playback().position() < 5.0, "repeat-one restarted the track");
    app.cycle_repeat(); // back to off

    app.remove(1);
    let index = wait_playing("removed the current track");
    println!("queue now {} tracks, playing #{index:?}", app.tracks().len());

    app.clear();
    run(1);
    assert!(app.tracks().is_empty() && !app.is_playing() && app.playback().finished());
    println!("cleared: OK");
}
