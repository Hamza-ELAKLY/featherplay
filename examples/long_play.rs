//! Plays a URL from `at` seconds and prints the position every 10 s: `long_play <url> <at> <seconds>`.

use std::time::Duration;

use featherplay::engine::Engine;

fn main() {
    let mut args = std::env::args().skip(1);
    let url = args.next().expect("url");
    let at: f64 = args.next().and_then(|a| a.parse().ok()).unwrap_or(0.0);
    let seconds: u64 = args.next().and_then(|a| a.parse().ok()).unwrap_or(60);
    let engine = Engine::new().expect("audio output");
    let playback = engine.playback();
    playback.load(&url, at).expect("load");
    for _ in 0..seconds / 10 {
        std::thread::sleep(Duration::from_secs(10));
        println!("position {:.1}s finished={}", playback.position(), playback.finished());
    }
}
