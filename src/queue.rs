//! The play queue: its tracks, the order they play in, and repeat and shuffle.
//!
//! Pure bookkeeping, with no audio and no UI, so every rule here is unit-tested.

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Track {
    pub url: String,
    pub title: String,
    pub duration: Option<f64>,
    /// The uploading channel, usually the artist; empty when unknown.
    #[serde(default)]
    pub channel: String,
    /// Private, deleted or otherwise gone for good; skipped when stepping through the queue.
    pub unavailable: bool,
    /// Failed to load just now (the network, most likely): skipped for the moment, and
    /// tried again once anything plays. Not saved.
    #[serde(skip)]
    pub failed: bool,
}

impl Track {
    /// Whether stepping through the queue should stop here.
    pub fn playable(&self) -> bool {
        !self.unavailable && !self.failed
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Repeat {
    #[default]
    Off,
    All,
    One,
}

impl Repeat {
    pub fn cycled(self) -> Self {
        match self {
            Repeat::Off => Repeat::All,
            Repeat::All => Repeat::One,
            Repeat::One => Repeat::Off,
        }
    }
}

#[derive(Default)]
pub struct Queue {
    tracks: Vec<Track>,
    /// Indices into `tracks` in play order: in sequence, or shuffled.
    order: Vec<usize>,
    /// Where in `order` playback is; `None` before the first track.
    cursor: Option<usize>,
    repeat: Repeat,
    shuffle: bool,
    rng: u64,
}

impl Queue {
    pub fn new() -> Self {
        let seed =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_nanos() as u64);
        Self { rng: seed | 1, ..Self::default() }
    }

    pub fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    pub fn track(&self, index: usize) -> Option<&Track> {
        self.tracks.get(index)
    }

    /// The index of the track at the cursor.
    pub fn current(&self) -> Option<usize> {
        self.cursor.map(|c| self.order[c])
    }

    pub fn repeat(&self) -> Repeat {
        self.repeat
    }

    pub fn set_repeat(&mut self, repeat: Repeat) {
        self.repeat = repeat;
    }

    pub fn shuffle(&self) -> bool {
        self.shuffle
    }

    /// Appends tracks, returning the index of the first one added.
    pub fn add(&mut self, tracks: impl IntoIterator<Item = Track>) -> usize {
        let first = self.tracks.len();
        self.tracks.extend(tracks);
        for index in first..self.tracks.len() {
            if self.shuffle {
                // Somewhere among the tracks still to come.
                let after = self.cursor.map_or(0, |c| c + 1);
                let at = after + self.random(self.order.len() - after + 1);
                self.order.insert(at, index);
            } else {
                self.order.push(index);
            }
        }
        first
    }

    /// Moves the cursor to a track, returning false if there is no such track.
    pub fn select(&mut self, index: usize) -> bool {
        match self.order.iter().position(|&i| i == index) {
            Some(at) => {
                self.cursor = Some(at);
                true
            }
            None => false,
        }
    }

    /// Removes a track. If it was the current one the cursor steps back, so `next`
    /// moves on to the track that followed it.
    pub fn remove(&mut self, index: usize) {
        if index >= self.tracks.len() {
            return;
        }
        self.tracks.remove(index);
        let at = self.order.iter().position(|&i| i == index).expect("every track is in the order");
        self.order.remove(at);
        for i in &mut self.order {
            if *i > index {
                *i -= 1;
            }
        }
        if let Some(cursor) = self.cursor
            && at <= cursor
        {
            self.cursor = cursor.checked_sub(1);
        }
    }

    /// Replaces the track at `index` with `tracks`. If it was the current track, the
    /// cursor moves to `tracks[keep]`: how a playlist fills in around a video that
    /// started playing before the rest of its playlist was listed.
    pub fn splice(&mut self, index: usize, tracks: Vec<Track>, keep: usize) {
        if index >= self.tracks.len() || tracks.is_empty() {
            return;
        }
        let current = self.current();
        let added = tracks.len();
        self.tracks.splice(index..=index, tracks);
        let current = current.map(|c| match c {
            c if c == index => index + keep.min(added - 1),
            c if c > index => c + added - 1,
            c => c,
        });
        self.order = (0..self.tracks.len()).collect();
        self.cursor = current;
        if self.shuffle {
            self.shuffle = false;
            self.set_shuffle(true);
        }
    }

    /// Puts `tracks` right after the current track (at the end when nothing is current),
    /// so they play next, shuffled or not; returns the index of the first. Everything
    /// already queued stays, after them.
    pub fn insert_next(&mut self, tracks: impl IntoIterator<Item = Track>) -> usize {
        let at = self.current().map_or(self.tracks.len(), |c| c + 1);
        let new: Vec<Track> = tracks.into_iter().collect();
        let added = new.len();
        self.tracks.splice(at..at, new);
        for i in &mut self.order {
            if *i >= at {
                *i += added;
            }
        }
        // In play order too, they come right after the current track.
        let place = self.cursor.map_or(self.order.len(), |c| c + 1);
        self.order.splice(place..place, at..at + added);
        at
    }

    /// Puts one track to play right after the current one, shuffled or not, returning
    /// its index: a track already queued moves there rather than being copied. The
    /// current track itself stays put (`None`).
    pub fn play_next(&mut self, track: Track) -> Option<usize> {
        match self.tracks.iter().position(|t| t.url == track.url) {
            Some(index) if Some(index) == self.current() => None,
            Some(index) => {
                let track = self.tracks[index].clone();
                self.remove(index);
                Some(self.insert_next([track]))
            }
            None => Some(self.insert_next([track])),
        }
    }

    pub fn clear(&mut self) {
        self.tracks.clear();
        self.order.clear();
        self.cursor = None;
    }

    /// Moves the track at `from` to `to`, as dragging it in the list does. The same
    /// track stays current; in order, play follows the new list, and shuffled it keeps
    /// to the order it already had.
    pub fn move_track(&mut self, from: usize, to: usize) {
        let len = self.tracks.len();
        if from >= len || to >= len || from == to {
            return;
        }
        let track = self.tracks.remove(from);
        self.tracks.insert(to, track);
        // Where each track's old index lands.
        let moved = |i: usize| match i {
            i if i == from => to,
            i if from < to && (from + 1..=to).contains(&i) => i - 1,
            i if to < from && (to..from).contains(&i) => i + 1,
            i => i,
        };
        if self.shuffle {
            self.order.iter_mut().for_each(|i| *i = moved(*i));
        } else {
            let current = self.current().map(moved);
            self.order = (0..len).collect();
            self.cursor = current;
        }
    }

    /// What was learned of a track since it was queued: its length and channel, where they
    /// were missing. Returns whether anything changed.
    pub fn learn(&mut self, url: &str, duration: Option<f64>, channel: &str) -> bool {
        let mut changed = false;
        for track in self.tracks.iter_mut().filter(|t| t.url == url) {
            if track.duration.is_none() && duration.is_some() {
                track.duration = duration;
                changed = true;
            }
            if track.channel.is_empty() && !channel.is_empty() {
                track.channel = channel.to_string();
                changed = true;
            }
        }
        changed
    }

    pub fn mark_unavailable(&mut self, index: usize) {
        if let Some(track) = self.tracks.get_mut(index) {
            track.unavailable = true;
        }
    }

    pub fn mark_failed(&mut self, index: usize) {
        if let Some(track) = self.tracks.get_mut(index) {
            track.failed = true;
        }
    }

    /// The track at `index` has played: whatever kept it from playing before is gone, and
    /// the others that failed to load (the network was down, most likely) get another go.
    pub fn played(&mut self, index: usize) {
        if let Some(track) = self.tracks.get_mut(index) {
            track.unavailable = false;
        }
        self.tracks.iter_mut().for_each(|t| t.failed = false);
    }

    pub fn set_shuffle(&mut self, on: bool) {
        if on == self.shuffle {
            return;
        }
        self.shuffle = on;
        let current = self.current();
        self.order = (0..self.tracks.len()).collect();
        if on {
            // Fisher–Yates, then the playing track goes first so nothing already heard repeats.
            for i in (1..self.order.len()).rev() {
                let j = self.random(i + 1);
                self.order.swap(i, j);
            }
            if let Some(current) = current {
                let at = self.order.iter().position(|&i| i == current).expect("in order");
                self.order.swap(0, at);
            }
        }
        self.cursor = current.and_then(|c| self.order.iter().position(|&i| i == c));
    }

    /// The track to play when the current one ends (`auto`) or the user skips forward,
    /// moving the cursor to it. `None` at the end of the queue.
    pub fn next(&mut self, auto: bool) -> Option<usize> {
        let target = self.peek_next(auto)?;
        self.select(target);
        Some(target)
    }

    /// What `next` would return, without moving.
    pub fn peek_next(&self, auto: bool) -> Option<usize> {
        if auto
            && self.repeat == Repeat::One
            && let Some(current) = self.current().filter(|&c| self.tracks[c].playable())
        {
            return Some(current);
        }
        let start = self.cursor.map_or(0, |c| c + 1);
        self.step(start, 1)
    }

    /// The track before the current one, moving the cursor to it.
    pub fn previous(&mut self) -> Option<usize> {
        let len = self.order.len();
        let start = match self.cursor {
            Some(0) | None if self.repeat == Repeat::All => len.checked_sub(1)?,
            Some(0) | None => return None,
            Some(c) => c - 1,
        };
        let target = self.step(start, -1)?;
        self.select(target);
        Some(target)
    }

    /// The first playable track walking `order` from `start` in `direction`, wrapping
    /// around only under repeat-all.
    fn step(&self, start: usize, direction: isize) -> Option<usize> {
        let len = self.order.len();
        let wrap = self.repeat == Repeat::All;
        let mut at = start;
        for _ in 0..len {
            if at >= len {
                if !wrap {
                    return None;
                }
                at = if direction > 0 { 0 } else { len - 1 };
            }
            let index = self.order[at];
            if self.tracks[index].playable() {
                return Some(index);
            }
            at = match at.checked_add_signed(direction) {
                Some(next) => next,
                None if wrap => len - 1,
                None => return None,
            };
        }
        None
    }

    /// A number in `0..bound` from xorshift: shuffling needs no better randomness.
    fn random(&mut self, bound: usize) -> usize {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        (self.rng % bound as u64) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(name: &str) -> Track {
        Track {
            url: format!("https://example.com/{name}"),
            title: name.into(),
            duration: None,
            channel: String::new(),
            unavailable: false,
            failed: false,
        }
    }

    fn queue(names: &[&str]) -> Queue {
        let mut q = Queue::new();
        q.add(names.iter().map(|n| track(n)));
        q
    }

    fn walk(q: &mut Queue, auto: bool) -> Vec<usize> {
        std::iter::from_fn(|| q.next(auto)).take(10).collect()
    }

    #[test]
    fn plays_in_order_and_stops_at_the_end() {
        let mut q = queue(&["a", "b", "c"]);
        assert_eq!(q.current(), None);
        assert_eq!(walk(&mut q, true), vec![0, 1, 2]);
        assert_eq!(q.current(), Some(2));
    }

    #[test]
    fn repeat_all_wraps_and_repeat_one_holds() {
        let mut q = queue(&["a", "b"]);
        q.set_repeat(Repeat::All);
        assert_eq!(walk(&mut q, true)[..5], [0, 1, 0, 1, 0]);

        q.set_repeat(Repeat::One);
        q.select(0);
        assert_eq!(q.next(true), Some(0));
        assert_eq!(q.next(true), Some(0));
        // Skipping by hand still moves on under repeat-one.
        assert_eq!(q.next(false), Some(1));
    }

    #[test]
    fn inserted_tracks_play_next_and_the_rest_of_the_queue_follows() {
        let mut q = queue(&["a", "b", "c"]);
        q.select(0);
        let first = q.insert_next([track("x"), track("y")]);
        assert_eq!(first, 1);
        let titles: Vec<&str> = q.tracks().iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, ["a", "x", "y", "b", "c"]);
        assert_eq!(q.current(), Some(0), "a is still playing");
        assert_eq!(walk(&mut q, true), vec![1, 2, 3, 4], "x and y, then b and c");

        let mut empty = Queue::new();
        assert_eq!(empty.insert_next([track("z")]), 0, "nothing current: they go at the end");
    }

    #[test]
    fn inserted_tracks_play_next_even_shuffled() {
        let mut q = queue(&["a", "b", "c", "d"]);
        q.select(2);
        q.set_shuffle(true);
        let first = q.insert_next([track("x"), track("y")]);
        assert_eq!(q.tracks()[q.current().unwrap()].title, "c", "the same track is current");
        assert_eq!(q.next(true), Some(first));
        assert_eq!(q.next(true), Some(first + 1));
        let mut rest = Vec::new();
        while let Some(i) = q.next(true) {
            rest.push(q.tracks()[i].title.clone());
        }
        rest.sort();
        assert_eq!(rest, ["a", "b", "d"], "then the others still to come, each once");
    }

    #[test]
    fn a_moved_track_plays_where_it_was_put() {
        let mut q = queue(&["a", "b", "c", "d"]);
        q.select(1);
        q.move_track(3, 0);
        let titles: Vec<&str> = q.tracks().iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, ["d", "a", "b", "c"]);
        assert_eq!(q.current(), Some(2), "b is still playing, one row further down");
        assert_eq!(q.next(true), Some(3));

        q.move_track(3, 0);
        assert_eq!(q.current(), Some(0), "the playing track itself can be moved");
        assert_eq!(q.tracks()[0].title, "c");
        assert_eq!(walk(&mut q, true), vec![1, 2, 3]);

        q.move_track(0, 9);
        q.move_track(2, 2);
        assert_eq!(q.tracks()[0].title, "c", "a move off the end or onto itself does nothing");
    }

    #[test]
    fn moving_a_track_keeps_the_shuffled_order() {
        let mut q = queue(&["a", "b", "c", "d", "e"]);
        q.select(0);
        q.set_shuffle(true);
        let before: Vec<String> = q.order.iter().map(|&i| q.tracks[i].title.clone()).collect();
        q.move_track(4, 1);
        q.move_track(0, 3);
        let after: Vec<String> = q.order.iter().map(|&i| q.tracks[i].title.clone()).collect();
        assert_eq!(before, after, "the same tracks in the same shuffled order");
        assert_eq!(q.tracks()[q.current().unwrap()].title, "a");
    }

    #[test]
    fn a_track_that_failed_to_load_is_skipped_until_something_plays() {
        let mut q = queue(&["a", "b", "c"]);
        q.mark_failed(1);
        assert_eq!(walk(&mut q, true), vec![0, 2]);
        q.mark_unavailable(0);
        q.played(2);
        assert!(q.tracks()[1].playable(), "the network is back: b gets another go");
        assert!(!q.tracks()[0].playable(), "a is still gone for good");
        q.played(0);
        assert!(q.tracks()[0].playable(), "unless it plays after all");
    }

    #[test]
    fn skips_unavailable_tracks() {
        let mut q = queue(&["a", "b", "c", "d"]);
        q.mark_unavailable(1);
        q.mark_unavailable(3);
        assert_eq!(walk(&mut q, true), vec![0, 2]);
        assert_eq!(q.previous(), Some(0));

        q.set_repeat(Repeat::One);
        q.mark_unavailable(0);
        assert_eq!(q.next(true), Some(2), "repeat-one does not hold an unplayable track");
    }

    #[test]
    fn a_queue_of_nothing_playable_ends() {
        let mut q = queue(&["a", "b"]);
        q.set_repeat(Repeat::All);
        q.mark_unavailable(0);
        q.mark_unavailable(1);
        assert_eq!(q.next(true), None);
        assert_eq!(q.previous(), None);
    }

    #[test]
    fn previous_stops_at_the_start_unless_repeating() {
        let mut q = queue(&["a", "b", "c"]);
        q.select(1);
        assert_eq!(q.previous(), Some(0));
        assert_eq!(q.previous(), None);
        q.set_repeat(Repeat::All);
        assert_eq!(q.previous(), Some(2));
    }

    #[test]
    fn removing_the_current_track_lets_next_continue_after_it() {
        let mut q = queue(&["a", "b", "c"]);
        q.select(1);
        q.remove(1);
        assert_eq!(q.tracks().iter().map(|t| t.title.as_str()).collect::<Vec<_>>(), ["a", "c"]);
        assert_eq!(q.next(true), Some(1));
        assert_eq!(q.track(1).unwrap().title, "c");

        let mut q = queue(&["a", "b", "c"]);
        q.select(2);
        q.remove(0);
        assert_eq!(q.current(), Some(1), "the cursor follows its track when an earlier one goes");
    }

    #[test]
    fn selecting_and_clearing() {
        let mut q = queue(&["a", "b", "c"]);
        assert!(q.select(2));
        assert!(!q.select(9));
        assert_eq!(q.next(true), None);
        q.clear();
        assert!(q.tracks().is_empty());
        assert_eq!(q.next(false), None);
    }

    #[test]
    fn shuffle_plays_every_track_once_starting_from_the_current() {
        let mut q = queue(&["a", "b", "c", "d", "e", "f"]);
        q.select(3);
        q.set_shuffle(true);
        assert_eq!(q.current(), Some(3));
        let mut played = walk(&mut q, true);
        played.push(3);
        played.sort();
        assert_eq!(played, vec![0, 1, 2, 3, 4, 5]);

        // Turning shuffle off keeps the current track and resumes in sequence after it.
        q.select(2);
        q.set_shuffle(false);
        assert_eq!(q.current(), Some(2));
        assert_eq!(q.next(true), Some(3));
    }

    #[test]
    fn tracks_added_while_shuffled_are_still_to_come() {
        let mut q = queue(&["a", "b", "c"]);
        q.next(true);
        q.set_shuffle(true);
        let first = q.add([track("d"), track("e")]);
        assert_eq!(first, 3);
        let mut rest = walk(&mut q, true);
        rest.sort();
        assert_eq!(rest, vec![1, 2, 3, 4]);
    }

    #[test]
    fn splicing_a_playlist_in_around_the_playing_track() {
        let mut q = queue(&["x", "v", "y"]);
        q.select(1);
        q.splice(1, vec![track("a"), track("v"), track("b"), track("c")], 1);
        let titles: Vec<_> = q.tracks().iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, ["x", "a", "v", "b", "c", "y"]);
        assert_eq!(q.current(), Some(2));
        assert_eq!(walk(&mut q, true), vec![3, 4, 5]);

        let mut q = queue(&["v", "y"]);
        q.select(1);
        q.splice(0, vec![track("a"), track("b")], 0);
        assert_eq!(q.current(), Some(2), "a track after the splice keeps its place");

        let mut q = queue(&["v"]);
        q.select(0);
        q.set_shuffle(true);
        q.splice(0, vec![track("a"), track("v"), track("b")], 1);
        assert_eq!(q.current(), Some(1));
        let mut rest = walk(&mut q, true);
        rest.sort();
        assert_eq!(rest, vec![0, 2], "shuffled, the rest of the playlist is still to come");
    }

    #[test]
    fn repeat_cycles_through_its_modes() {
        assert_eq!(Repeat::Off.cycled(), Repeat::All);
        assert_eq!(Repeat::All.cycled(), Repeat::One);
        assert_eq!(Repeat::One.cycled(), Repeat::Off);
    }

    fn titles(q: &Queue) -> Vec<&str> {
        q.tracks().iter().map(|t| t.title.as_str()).collect()
    }

    #[test]
    fn play_next_inserts_after_current() {
        let mut q = queue(&["a", "b", "c"]);
        q.select(1);
        assert_eq!(q.play_next(track("x")), Some(2));
        assert_eq!(titles(&q), ["a", "b", "x", "c"]);
        assert_eq!(q.current(), Some(1), "b is still playing");
        assert_eq!(walk(&mut q, true), vec![2, 3]);
    }

    #[test]
    fn play_next_moves_a_queued_track() {
        let mut q = queue(&["a", "b", "c", "d"]);
        q.select(0);
        assert_eq!(q.play_next(track("d")), Some(1));
        assert_eq!(titles(&q), ["a", "d", "b", "c"], "moved, not copied");
        assert_eq!(q.current(), Some(0));
        // A track from before the current one moves too, and the current one stays current.
        q.select(2);
        assert_eq!(q.play_next(track("a")), Some(2));
        assert_eq!(titles(&q), ["d", "b", "a", "c"]);
        assert_eq!(q.track(q.current().unwrap()).unwrap().title, "b");
        // The current track itself isn't moved.
        assert_eq!(q.play_next(track("b")), None);
        assert_eq!(titles(&q), ["d", "b", "a", "c"]);
    }

    #[test]
    fn play_next_shuffled_plays_next() {
        for _ in 0..20 {
            let mut q = queue(&["a", "b", "c", "d", "e", "f"]);
            q.set_shuffle(true);
            let playing = q.next(true).unwrap();
            q.next(false);
            q.select(playing);
            let index = q.play_next(track("x")).unwrap();
            assert_eq!(q.next(true), Some(index), "x comes next, whatever the shuffle");
            let mut q = queue(&["a", "b", "c", "d", "e", "f"]);
            q.set_shuffle(true);
            let playing = q.next(true).unwrap();
            let other = if playing == 5 { "e" } else { "f" };
            let index = q.play_next(track(other)).unwrap();
            assert_eq!(q.tracks()[index].title, other);
            assert_eq!(q.next(true), Some(index), "a queued track moved next plays next shuffled");
        }
    }

    #[test]
    fn play_next_with_nothing_current_adds_it() {
        let mut q = queue(&["a", "b"]);
        assert_eq!(q.play_next(track("x")), Some(2), "at the end, where play starts from it");
        assert_eq!(titles(&q), ["a", "b", "x"]);
    }

    #[test]
    fn a_track_learns_its_length_once_known() {
        let mut q = queue(&["a", "b"]);
        assert!(q.learn("https://example.com/a", Some(241.0), "Band"));
        assert_eq!(q.track(0).unwrap().duration, Some(241.0));
        assert_eq!(q.track(0).unwrap().channel, "Band");
        assert!(!q.learn("https://example.com/a", Some(300.0), "Other"), "what's known stays");
        assert_eq!(q.track(0).unwrap().duration, Some(241.0));
        assert!(!q.learn("https://example.com/zzz", Some(1.0), ""), "not queued");
    }
}
