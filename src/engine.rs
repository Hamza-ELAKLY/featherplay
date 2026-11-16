//! Audio playback: decodes a stream and plays it on the default output device.
//!
//! YouTube audio is a fragmented MP4. Its decoder would read every fragment header
//! before playing a seekable file, which downloads the whole track up front, so the
//! decoder is always handed a forward-only stream instead. Seeking reopens the stream
//! at the right fragment, found through the file's own index.
//!
//! Every track and every seek gets a player of its own, so nothing about one (its
//! position above all) can linger into the next. Nothing starts or stops with a cut:
//! a replaced player fades out on its own while the next one starts, and pause and
//! resume ease the volume rather than switching it.

use std::f32::consts::PI;
use std::io::{Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rodio::decoder::DecoderBuilder;
use rodio::mixer::Mixer;
use rodio::{DeviceSinkBuilder, MixerDeviceSink, Player, Source};

use crate::levels::{Levels, Meter};
use crate::mp4::{self, Index, Segment};
use crate::stream::{HttpStream, Spliced};

// The index sits right after the init segment; this comfortably covers both.
const HEAD_BYTES: usize = 128 * 1024;
// How long the volume takes to ease out or in. Short enough not to feel slow, long
// enough that nothing clicks.
const PAUSE_FADE: Duration = Duration::from_millis(150);
const SWITCH_FADE: Duration = Duration::from_millis(200);
const SEEK_FADE: Duration = Duration::from_millis(40);
const FADE_STEPS: u32 = 12;

pub struct Engine {
    // Playback stops when the device sink is dropped, so the engine owns it.
    _sink: MixerDeviceSink,
    playback: Arc<Playback>,
}

impl Engine {
    pub fn new() -> Result<Self, String> {
        let mut sink = DeviceSinkBuilder::open_default_sink().map_err(|e| format!("no audio output: {e}"))?;
        sink.log_on_drop(false);
        let mixer = sink.mixer().clone();
        let output = Output { player: Arc::new(Player::connect_new(&mixer)), base: 0.0, volume: 1.0, paused: false };
        let playback =
            Playback { mixer, output: Mutex::new(output), current: Mutex::new(None), fades: AtomicU64::new(0) };
        Ok(Self { _sink: sink, playback: Arc::new(playback) })
    }

    /// A handle that can be shared with the threads that load and seek.
    pub fn playback(&self) -> Arc<Playback> {
        self.playback.clone()
    }
}

pub struct Playback {
    mixer: Mixer,
    output: Mutex<Output>,
    current: Mutex<Option<Current>>,
    /// Bumped by each pause or resume, so an older fade still running gives way.
    fades: AtomicU64,
}

/// What is sounding now, swapped whole when a track starts or a seek lands.
struct Output {
    player: Arc<Player>,
    /// Where in the track this player's source starts, in seconds.
    base: f64,
    volume: f32,
    /// Paused as far as anyone asking is concerned, even while the sound eases out.
    paused: bool,
}

/// The track loaded, for seeking within it.
struct Current {
    url: String,
    head: Arc<[u8]>,
    index: Option<Index>,
}

impl Playback {
    /// Opens `url` `at` seconds in and replaces whatever is playing. Blocks on the
    /// network: call it off the UI thread.
    pub fn load(&self, url: &str, at: f64) -> Result<(), String> {
        let mut stream = HttpStream::open(url, 0)?;
        let mut head = Vec::with_capacity(HEAD_BYTES);
        (&mut stream).take(HEAD_BYTES as u64).read_to_end(&mut head).map_err(|e| e.to_string())?;
        let index = mp4::parse_index(&head);

        // Without an index the file plays from its first byte, and only decoding reaches `at`.
        let (head, segment): (Arc<[u8]>, Segment) = match &index {
            Some(index) => {
                head.truncate(index.init_len);
                (head.into(), index.segment_at(at.max(0.0)))
            }
            None => (Vec::new().into(), Segment { start: 0.0, offset: 0 }),
        };
        stream.seek(SeekFrom::Start(segment.offset)).map_err(|e| e.to_string())?;
        let skip = Duration::from_secs_f64((at - segment.start).max(0.0));
        let base = segment.start + skip.as_secs_f64();
        let source = decode(Spliced::new(head.clone(), stream))?.skip_duration(skip);
        *self.current.lock().unwrap() = Some(Current { url: url.into(), head, index });
        self.replace(source, base, false, SWITCH_FADE);
        Ok(())
    }

    /// Jumps to `seconds` into the track. Blocks on the network: call it off the UI thread.
    pub fn seek(&self, seconds: f64) -> Result<(), String> {
        let (url, head, segment) = {
            let current = self.current.lock().unwrap();
            let current = current.as_ref().ok_or("nothing is playing")?;
            let segment = current.index.as_ref().map(|index| index.segment_at(seconds.max(0.0)));
            (current.url.clone(), current.head.clone(), segment)
        };
        let Some(segment) = segment else {
            // No index to jump with: start over and decode forward to the spot.
            let paused = self.is_paused();
            self.load(&url, seconds)?;
            if paused {
                // Was paused, stays paused: no fade, nothing was audible.
                let mut output = self.output.lock().unwrap();
                output.player.pause();
                output.paused = true;
            }
            return Ok(());
        };
        let stream = HttpStream::open(&url, segment.offset)?;
        let skip = Duration::from_secs_f64((seconds - segment.start).max(0.0));
        let base = segment.start + skip.as_secs_f64();
        let source = decode(Spliced::new(head, stream))?.skip_duration(skip);
        self.replace(source, base, self.is_paused(), SEEK_FADE);
        Ok(())
    }

    /// Seconds into the track.
    pub fn position(&self) -> f64 {
        let output = self.output.lock().unwrap();
        output.base + output.player.get_pos().as_secs_f64()
    }

    pub fn is_paused(&self) -> bool {
        self.output.lock().unwrap().paused
    }

    /// Whether the track played to its end (or nothing was ever loaded).
    pub fn finished(&self) -> bool {
        self.output.lock().unwrap().player.empty()
    }

    /// Stops playback, fading out what was playing.
    pub fn stop(&self) {
        let (old, volume) = {
            let mut output = self.output.lock().unwrap();
            let fresh = Arc::new(self.player(output.volume));
            let old = std::mem::replace(&mut output.player, fresh);
            output.base = 0.0;
            output.paused = false;
            (old, output.volume)
        };
        *self.current.lock().unwrap() = None;
        fade_out_and_drop(old, volume, SWITCH_FADE);
    }

    /// Eases the sound out, then pauses.
    pub fn pause(self: &Arc<Self>) {
        let player = {
            let mut output = self.output.lock().unwrap();
            if output.paused {
                return;
            }
            output.paused = true;
            output.player.clone()
        };
        let fade = self.fades.fetch_add(1, Ordering::SeqCst) + 1;
        let me = self.clone();
        std::thread::spawn(move || {
            let current = || me.fades.load(Ordering::SeqCst) == fade;
            ease(&player, || me.volume(), false, PAUSE_FADE, current);
            if current() {
                player.pause();
                // Ready at full volume for whatever plays it next.
                player.set_volume(me.volume());
            }
        });
    }

    /// Resumes, easing the sound in.
    pub fn resume(self: &Arc<Self>) {
        let player = {
            let mut output = self.output.lock().unwrap();
            if !output.paused {
                return;
            }
            output.paused = false;
            output.player.clone()
        };
        let fade = self.fades.fetch_add(1, Ordering::SeqCst) + 1;
        player.set_volume(0.0);
        player.play();
        let me = self.clone();
        std::thread::spawn(move || {
            let current = || me.fades.load(Ordering::SeqCst) == fade;
            ease(&player, || me.volume(), true, PAUSE_FADE, current);
            if current() {
                player.set_volume(me.volume());
            }
        });
    }

    pub fn toggle_pause(self: &Arc<Self>) {
        if self.is_paused() { self.resume() } else { self.pause() }
    }

    /// `volume` is the slider's 0–1; loudness is perceived roughly logarithmically.
    pub fn set_volume(&self, volume: f32) {
        let mut output = self.output.lock().unwrap();
        output.volume = volume * volume;
        output.player.set_volume(output.volume);
    }

    fn volume(&self) -> f32 {
        self.output.lock().unwrap().volume
    }

    fn player(&self, volume: f32) -> Player {
        let player = Player::connect_new(&self.mixer);
        player.set_volume(volume);
        player
    }

    /// Plays `source` in place of what is playing, which fades out over `fade`; the new
    /// source starts `base` seconds into the track.
    fn replace<S>(&self, source: S, base: f64, paused: bool, fade: Duration)
    where
        S: Source + Send + 'static,
    {
        let (old, volume) = {
            let mut output = self.output.lock().unwrap();
            let player = self.player(output.volume);
            if paused {
                player.pause();
            }
            player.append(source);
            let old = std::mem::replace(&mut output.player, Arc::new(player));
            output.base = base;
            output.paused = paused;
            (old, output.volume)
        };
        fade_out_and_drop(old, volume, fade);
    }
}

/// Decodes all of `url`, without playing it, to measure its loudness into `levels`.
/// Gives up quietly once `wanted` turns false. Blocks for as long as the download takes.
pub fn measure(url: &str, levels: Arc<Levels>, wanted: impl Fn() -> bool) -> Result<(), String> {
    let stream = HttpStream::open(url, 0)?;
    let mut meter = Meter::new(decode(Spliced::new(Vec::new().into(), stream))?, levels, 0.0);
    // A second or so of stereo audio at a time, with a breath between, so the pass never
    // crowds what is playing.
    while wanted() {
        if meter.by_ref().take(96_000).count() < 96_000 {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    Ok(())
}

/// Eases a player's volume in (`up`) or out over `over`, along half a cosine so it
/// starts and ends gently. Stops early, where it is, once `still` turns false.
fn ease(player: &Player, target: impl Fn() -> f32, up: bool, over: Duration, still: impl Fn() -> bool) {
    for step in 1..=FADE_STEPS {
        if !still() {
            return;
        }
        let progress = step as f32 / FADE_STEPS as f32;
        let gain = 0.5 - 0.5 * (PI * progress).cos();
        player.set_volume(target() * if up { gain } else { 1.0 - gain });
        std::thread::sleep(over / FADE_STEPS);
    }
}

/// Lets a player that has been replaced fade out on its own thread, then stops it.
fn fade_out_and_drop(player: Arc<Player>, volume: f32, over: Duration) {
    if player.empty() || player.is_paused() {
        return;
    }
    std::thread::spawn(move || ease(&player, || volume, false, over, || true));
}

fn decode<R>(reader: R) -> Result<rodio::Decoder<R>, String>
where
    R: Read + Seek + Send + Sync + 'static,
{
    DecoderBuilder::new()
        .with_data(reader)
        .with_seekable(false)
        .with_mime_type("audio/mp4")
        .build()
        .map_err(|e| format!("could not decode the audio: {e}"))
}
