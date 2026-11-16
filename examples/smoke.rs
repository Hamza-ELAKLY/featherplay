//! Plays a link headlessly and exercises seeking: `cargo run --example smoke -- <link>`.

use std::time::{Duration, Instant};

use featherplay::{engine::Engine, extract};

fn main() {
    let link = std::env::args().nth(1).expect("usage: smoke <link>");
    let started = Instant::now();
    let track = extract::resolve(&link).expect("resolve");
    println!("resolved in {:?}: {} ({:?}s)", started.elapsed(), track.title, track.duration);

    let engine = Engine::new().expect("audio output");
    let playback = engine.playback();
    let started = Instant::now();
    playback.load(&track.stream_url, 0.0).expect("load");
    println!("playing after {:?}", started.elapsed());
    std::thread::sleep(Duration::from_secs(3));
    println!("position after 3s: {:.2}", playback.position());

    for target in [300.0, 47.3, 0.0] {
        let started = Instant::now();
        playback.seek(target).expect("seek");
        let took = started.elapsed();
        std::thread::sleep(Duration::from_secs(2));
        println!("seek to {target}: took {took:?}, position 2s later {:.2}", playback.position());
    }
    println!("finished: {}", playback.finished());
}
