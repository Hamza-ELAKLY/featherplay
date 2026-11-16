//! Search, history and duplicates, headlessly: `cargo run --example features_smoke`.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use featherplay::app::{App, Event};
use featherplay::engine::Engine;
use featherplay::store::History;

fn main() {
    let data = std::env::temp_dir().join(format!("featherplay-features-{}", std::process::id()));
    let engine = Engine::new().expect("audio output");
    let (searched, search_done) = mpsc::channel();
    let app = App::new(engine.playback(), &data, move |event| {
        if let Event::SearchDone(done) = event {
            searched.send(done).ok();
        }
    });
    let wait_playing = |what: &str| {
        let started = Instant::now();
        while !app.is_playing() {
            app.tick();
            assert!(started.elapsed() < Duration::from_secs(30), "{what}: never started");
            std::thread::sleep(Duration::from_millis(50));
        }
        println!(
            "{what}: playing #{:?} “{}” after {:.2?}",
            app.now_playing().index,
            app.now_playing().title,
            started.elapsed()
        );
    };

    let started = Instant::now();
    app.search("daft punk around the world".into());
    search_done.recv_timeout(Duration::from_secs(30)).expect("search answered").expect("search worked");
    let results = app.search_results();
    println!("search: {} results in {:.2?}; first “{}”", results.len(), started.elapsed(), results[0].title);
    assert!(results.len() >= 5);

    app.add_track(results[0].clone(), true);
    wait_playing("picked the first result");
    app.add_track(results[1].clone(), false);
    std::thread::sleep(Duration::from_secs(5)); // time for its stream URL to prefetch
    assert_eq!(app.tracks().len(), 2, "added the second without playing it");
    assert_eq!(app.now_playing().index, Some(0));

    app.add_track(results[1].clone(), true);
    std::thread::sleep(Duration::from_millis(200));
    wait_playing("picked an already-queued result");
    assert_eq!(app.tracks().len(), 2, "no duplicate");
    assert_eq!(app.now_playing().index, Some(1));

    let history = History::load(&data);
    let titles: Vec<_> = history.entries().iter().map(|e| e.title.as_str()).collect();
    println!("history after a restart: {titles:?}");
    assert_eq!(titles, [results[1].title.as_str(), results[0].title.as_str()]);
    app.clear_history();
    assert!(History::load(&data).entries().is_empty());
    println!("history cleared: OK");
    std::fs::remove_dir_all(data).ok();
}
