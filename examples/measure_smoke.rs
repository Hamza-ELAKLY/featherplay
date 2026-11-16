//! Measures a whole track's waveform without playing it, and reports how long that took.

use std::time::Instant;

use featherplay::{engine, extract, levels::Levels};

fn main() {
    let link = std::env::args().nth(1).unwrap_or("https://youtu.be/djV11Xbc914".into());
    let track = extract::resolve(&link).expect("resolve");
    let levels = Levels::new(track.duration.unwrap_or(0.0));
    let started = Instant::now();
    engine::measure(&track.stream_url, levels.clone(), || true).expect("measure");
    let map: String = levels
        .bars()
        .iter()
        .map(|&b| if b < 0.0 { '.' } else { "▁▂▃▄▅▆▇█".chars().nth((b * 7.0) as usize).unwrap() })
        .collect();
    println!(
        "{:.0} s of audio measured in {:.1} s, complete: {}\n{map}",
        levels.duration(),
        started.elapsed().as_secs_f64(),
        levels.complete()
    );
}
