//! Times each stage of starting tracks (resolving the link, opening the stream, first
//! sound), silently, to find where a slow start comes from.

use std::time::Instant;

use featherplay::engine::Engine;
use featherplay::extract;

fn main() {
    let engine = Engine::new().expect("audio output");
    let playback = engine.playback();
    playback.set_volume(0.0);
    for link in std::env::args().skip(1) {
        let started = Instant::now();
        let track = match extract::resolve(&link) {
            Ok(track) => track,
            Err(e) => {
                println!("{link}: resolve failed after {:.1} s: {e}", started.elapsed().as_secs_f64());
                continue;
            }
        };
        let resolved = started.elapsed().as_secs_f64();
        let loaded = Instant::now();
        let result = playback.load(&track.stream_url, 0.0);
        let load = loaded.elapsed().as_secs_f64();
        let sounding = Instant::now();
        while playback.position() < 0.05 && sounding.elapsed().as_secs() < 15 {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        println!(
            "{:<40.40} resolve {resolved:.2} s, open {load:.2} s, first sound +{:.2} s, {:?}",
            track.title,
            sounding.elapsed().as_secs_f64(),
            result.err()
        );
    }
    playback.stop();
}
