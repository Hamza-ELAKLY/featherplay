//! Fetches one thumbnail and reports its size and how long it took.

use std::time::Instant;

fn main() {
    let started = Instant::now();
    let thumb = featherplay::thumbs::fetch("aqz-KE-bpKQ", 104).expect("thumbnail");
    let distinct: std::collections::HashSet<_> = thumb.rgba.chunks(4).collect();
    println!(
        "{}x{}, {} bytes, {} distinct colours, in {:.0?}",
        thumb.width,
        thumb.height,
        thumb.rgba.len(),
        distinct.len(),
        started.elapsed()
    );
    assert_eq!(thumb.rgba.len(), (thumb.width * thumb.height * 4) as usize);
    assert!(distinct.len() > 100, "a real picture, not a blank");
}
