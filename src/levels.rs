//! The track's loudness, slice by slice, for the waveform seek bar.
//!
//! YouTube doesn't hand out waveforms, so the shape is measured from the audio itself:
//! when a track first plays, a background pass decodes all of it (silently, as fast as
//! it downloads) through `Meter`, which records the loudness of each slice. The bars
//! fill in within seconds and are saved, so a track heard before is shown whole at once.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use rodio::{ChannelCount, Sample, SampleRate, Source};

/// How many slices a track is divided into: one bar each.
pub const SLICES: usize = 100;
// A slot holding this has not been measured.
const UNKNOWN: u32 = u32::MAX;
// The quietest slice's height, and every slice's while they are all as loud.
const FLOOR: f32 = 0.2;
const MID: f32 = 0.7;

/// Loudness per slice, written by the audio thread and read by the UI, without locks.
pub struct Levels {
    slots: Box<[AtomicU32]>,
    duration: f64,
    /// Bumped on every write, so the UI redraws only when something changed.
    version: AtomicU64,
    /// Whether a pass is measuring these levels, so only one does.
    measuring: AtomicBool,
}

impl Levels {
    /// Levels for a track `duration` seconds long, none measured yet.
    pub fn new(duration: f64) -> Arc<Self> {
        Arc::new(Self {
            slots: (0..SLICES).map(|_| AtomicU32::new(UNKNOWN)).collect(),
            duration,
            version: AtomicU64::new(0),
            measuring: AtomicBool::new(false),
        })
    }

    /// Levels saved from an earlier play; anything malformed counts as unmeasured, as does
    /// a shape saved with a different number of slices.
    pub fn from_saved(duration: f64, saved: &[f32]) -> Arc<Self> {
        let levels = Self::new(duration);
        if saved.len() != SLICES {
            return levels;
        }
        for (slot, &value) in levels.slots.iter().zip(saved) {
            if value.is_finite() && value >= 0.0 {
                slot.store(value.to_bits(), Ordering::Relaxed);
            }
        }
        levels
    }

    pub fn duration(&self) -> f64 {
        self.duration
    }

    pub fn version(&self) -> u64 {
        self.version.load(Ordering::Relaxed)
    }

    /// Whether every slice has been measured.
    pub fn complete(&self) -> bool {
        (0..SLICES).all(|i| self.get(i).is_some())
    }

    /// Claims the measuring of these levels; false if a pass already has it.
    pub fn start_measuring(&self) -> bool {
        !self.measuring.swap(true, Ordering::SeqCst)
    }

    pub fn is_measuring(&self) -> bool {
        self.measuring.load(Ordering::SeqCst)
    }

    pub fn stop_measuring(&self) {
        self.measuring.store(false, Ordering::SeqCst);
    }

    fn set(&self, slice: usize, rms: f32) {
        if let Some(slot) = self.slots.get(slice) {
            slot.store(rms.to_bits(), Ordering::Relaxed);
            self.version.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn get(&self, slice: usize) -> Option<f32> {
        let bits = self.slots[slice].load(Ordering::Relaxed);
        (bits != UNKNOWN).then(|| f32::from_bits(bits))
    }

    /// Bar heights from 0 to 1 for the slices measured so far, -1 for the rest.
    ///
    /// Music is mastered loud, so heights span from the quietest slice heard to the
    /// loudest rather than from silence: the shape of the song shows instead of a wall.
    pub fn bars(&self) -> Vec<f32> {
        let raw: Vec<Option<f32>> = (0..SLICES).map(|i| self.get(i)).collect();
        let (quietest, loudest) = raw.iter().flatten().fold((f32::MAX, 0.0f32), |(lo, hi), &v| (lo.min(v), hi.max(v)));
        let range = loudest - quietest;
        raw.iter()
            .map(|level| match level {
                // A steady sound, or one slice so far: an even, medium height.
                Some(_) if range <= loudest * 0.02 => MID,
                Some(level) => FLOOR + (1.0 - FLOOR) * ((level - quietest) / range).clamp(0.0, 1.0),
                None => -1.0,
            })
            .collect()
    }

    /// The raw levels for saving, unmeasured slices as -1; `None` if nothing was measured.
    pub fn saved(&self) -> Option<Vec<f32>> {
        let raw: Vec<f32> = (0..SLICES).map(|i| self.get(i).map_or(-1.0, |v| (v * 1000.0).round() / 1000.0)).collect();
        raw.iter().any(|&v| v >= 0.0).then_some(raw)
    }
}

/// Passes audio through unchanged, measuring the loudness (RMS) of each slice.
pub struct Meter<S> {
    inner: S,
    levels: Arc<Levels>,
    /// Seconds into the track at which `inner` starts.
    start: f64,
    /// Samples per slice, worked out from the first sample's format.
    per_slice: f64,
    slice: usize,
    left: f64,
    sum: f64,
    count: u32,
}

impl<S: Source> Meter<S> {
    pub fn new(inner: S, levels: Arc<Levels>, start: f64) -> Self {
        Self { inner, levels, start, per_slice: 0.0, slice: 0, left: 0.0, sum: 0.0, count: 0 }
    }

    fn begin(&mut self) {
        let duration = self.levels.duration();
        let rate = f64::from(self.inner.sample_rate().get()) * f64::from(self.inner.channels().get());
        if duration <= 0.0 {
            // No length, no slices: pass the audio through and measure nothing.
            self.per_slice = f64::INFINITY;
            self.slice = SLICES;
            return;
        }
        self.per_slice = duration / SLICES as f64 * rate;
        let at = self.start.max(0.0) / duration * SLICES as f64;
        self.slice = at.floor() as usize;
        self.left = (1.0 - at.fract()) * self.per_slice;
    }

    fn flush(&mut self) {
        if self.count > 0 && self.slice < SLICES {
            self.levels.set(self.slice, (self.sum / f64::from(self.count)).sqrt() as f32);
        }
        self.sum = 0.0;
        self.count = 0;
    }
}

impl<S: Source> Iterator for Meter<S> {
    type Item = Sample;

    fn next(&mut self) -> Option<Sample> {
        if self.per_slice == 0.0 {
            self.begin();
        }
        let sample = self.inner.next()?;
        if self.slice < SLICES {
            self.sum += f64::from(sample) * f64::from(sample);
            self.count += 1;
            self.left -= 1.0;
            if self.left <= 0.0 {
                self.flush();
                self.slice += 1;
                self.left += self.per_slice;
            }
        }
        Some(sample)
    }
}

impl<S: Source> Source for Meter<S> {
    fn current_span_len(&self) -> Option<usize> {
        self.inner.current_span_len()
    }

    fn channels(&self) -> ChannelCount {
        self.inner.channels()
    }

    fn sample_rate(&self) -> SampleRate {
        self.inner.sample_rate()
    }

    fn total_duration(&self) -> Option<Duration> {
        self.inner.total_duration()
    }
}

impl<S> Drop for Meter<S> {
    fn drop(&mut self) {
        // The slice cut short (by a seek, a skip, the end) still counts for what it heard.
        if self.count > 0 && self.slice < SLICES {
            self.levels.set(self.slice, (self.sum / f64::from(self.count)).sqrt() as f32);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rodio::source::SineWave;

    /// One second of a tone at `amplitude`, `rate` samples a second, mono.
    fn tone(amplitude: f32, seconds: f32) -> impl Source {
        SineWave::new(440.0).amplify(amplitude).take_duration(Duration::from_secs_f32(seconds))
    }

    #[test]
    fn every_slice_of_a_whole_track_is_measured() {
        let levels = Levels::new(10.0);
        let meter = Meter::new(tone(0.5, 10.0), levels.clone(), 0.0);
        let expected = 48000 * 10;
        assert!(meter.count().abs_diff(expected) < 10, "the audio passes through unchanged in length");
        let bars = levels.bars();
        assert!(bars.iter().all(|&b| b == MID), "a steady tone is the same height everywhere: {bars:?}");
        assert!(levels.version() >= SLICES as u64);
        assert!(levels.complete());
    }

    #[test]
    fn a_seek_measures_from_where_playback_starts() {
        let levels = Levels::new(100.0);
        // From 36 s, ten seconds of audio: slices 36 to 45.
        Meter::new(tone(0.5, 10.0), levels.clone(), 36.0).for_each(drop);
        assert!(!levels.complete());
        let bars = levels.bars();
        assert!(bars[..36].iter().all(|&b| b < 0.0), "nothing before the seek");
        assert!(bars[36..46].iter().all(|&b| b >= FLOOR));
        // The tone runs a few samples long, into slice 46; nothing past that.
        assert!(bars[47..].iter().all(|&b| b < 0.0), "nothing past what played");
    }

    #[test]
    fn heights_span_from_the_quietest_slice_to_the_loudest() {
        let levels = Levels::new(3.0);
        Meter::new(tone(0.8, 1.0), levels.clone(), 0.0).for_each(drop);
        Meter::new(tone(0.5, 1.0), levels.clone(), 1.0).for_each(drop);
        Meter::new(tone(0.2, 1.0), levels.clone(), 2.0).for_each(drop);
        let bars = levels.bars();
        assert!((bars[0] - 1.0).abs() < 0.01, "the loudest is full height");
        assert!((bars[SLICES - 1] - FLOOR).abs() < 0.01, "the quietest is the floor");
        let middle = bars[SLICES / 2];
        assert!(middle > 0.5 && middle < 0.7, "in between, in proportion: {middle}");
    }

    #[test]
    fn saved_levels_round_trip_and_bad_ones_are_ignored() {
        let levels = Levels::new(10.0);
        assert!(levels.saved().is_none(), "nothing measured, nothing to save");
        Meter::new(tone(0.5, 5.0), levels.clone(), 0.0).for_each(drop);
        let saved = levels.saved().unwrap();
        let restored = Levels::from_saved(10.0, &saved);
        for (a, b) in restored.bars().iter().zip(levels.bars()) {
            assert!((a - b).abs() < 0.005, "saved levels are rounded, not changed: {a} vs {b}");
        }

        let mut odd = vec![-1.0; SLICES];
        odd[0] = f32::NAN;
        odd[2] = 0.3;
        let odd = Levels::from_saved(10.0, &odd);
        assert_eq!(&odd.bars()[..3], &[-1.0, -1.0, MID], "one good slice, at the even height");
        assert!(Levels::from_saved(10.0, &[0.3; 72]).saved().is_none(), "an older, coarser shape is measured again");
    }

    #[test]
    fn an_unknown_duration_measures_nothing() {
        let levels = Levels::new(0.0);
        assert!(Meter::new(tone(0.5, 1.0), levels.clone(), 0.0).count().abs_diff(48000) < 10);
        assert!(levels.saved().is_none());
    }
}
