//! Reads three pages of search results, the way scrolling Results does, and checks each
//! page is new. `cargo run --example search_smoke -- [words]`.

use std::collections::HashSet;
use std::time::Instant;

use featherplay::extract;

fn main() {
    let words = std::env::args().nth(1).unwrap_or_else(|| "atmospheric liquid dnb mix".into());
    let mut seen = HashSet::new();
    for page in 0..3 {
        let from = page * extract::SEARCH_PAGE;
        let started = Instant::now();
        let entries = extract::search(&words, from).expect("the search lists");
        let new = entries.iter().filter(|e| seen.insert(e.url.clone())).count();
        println!(
            "results {}-{}: {} listed, {} new, in {:.1?}; first: {:?}",
            from + 1,
            from + extract::SEARCH_PAGE,
            entries.len(),
            new,
            started.elapsed(),
            entries.first().map(|e| &e.title)
        );
        assert_eq!(entries.len(), extract::SEARCH_PAGE);
        assert!(new >= extract::SEARCH_PAGE / 2, "mostly new results");
    }
}
