//! The README's animated showcase, rendered off-screen with the real renderer and fonts:
//! a link pasted and played, its waveform sweeping in, another added for later, and the
//! theme changed to light and back from the Settings sheet, all at real speed.
//!
//! It fetches real covers and measures real waveforms, so it needs the network and yt-dlp
//! (`FEATHERPLAY_YTDLP` may point at it). Run it alone, and turn the frames into the
//! animation with `scripts/showcase.py`:
//!
//! ```text
//! FEATHERPLAY_UI_DEBUG_INFO=1 FEATHERPLAY_SHOWCASE=<folder> cargo test --release --bin featherplay showcase -- --ignored
//! scripts/showcase.py <folder> assets/showcase.webp
//! ```

use super::*;
use slint::LogicalPosition;
use slint::platform::{Key, WindowEvent};

// The tracks: public-domain recordings, as in the README's screenshots.
const MOZART: (&str, &str, &str, f32) = (
    "Y3H2xsFtXl8",
    "Mozart - Overture to The marriage of Figaro, K 492 (Musopen Symphony)",
    "Only The Classicals",
    241.0,
);
const CHOPIN: (&str, &str, &str, f32) =
    ("e6Kn2sYJZLc", "Musopen - Ballade No. 1 in G Minor, Op. 23", "Aaron Dunn", 121.0);
const BACH_1: (&str, &str, &str, f32) = (
    "EmA5cgOztjI",
    "Johann Sebastian Bach - Goldberg Variations BWV 988: Variation 1 [Classical / Public Domain Music]",
    "The White of Lightning",
    115.0,
);
const BACH_2: (&str, &str, &str, f32) = (
    "82FuqU1x_HM",
    "Johann Sebastian Bach - Goldberg Variations BWV 988: Variation 2 [Classical / Public Domain Music]",
    "The White of Lightning",
    88.0,
);
const BACH_8: (&str, &str, &str, f32) = (
    "UYuvJGK6cKI",
    "Johann Sebastian Bach - Goldberg Variations BWV 988: Variation 8 [Classical / Public Domain Music]",
    "The White of Lightning",
    104.0,
);
const BACH_ARIA: (&str, &str, &str, f32) = (
    "f2Ujm5nJCtE",
    "Johann Sebastian Bach - Goldberg Variations BWV 988: Aria [Classical / Public Domain Music]",
    "The White of Lightning",
    293.0,
);
const BACH_4: (&str, &str, &str, f32) = (
    "Kn5oL5xo0eM",
    "Johann Sebastian Bach - Goldberg Variations BWV 988: Variation 4 [Classical / Public Domain Music]",
    "The White of Lightning",
    54.0,
);
const BACH_5: (&str, &str, &str, f32) = (
    "n-zWtcBvh7A",
    "Johann Sebastian Bach - Goldberg Variations BWV 988: Variation 5 [Classical / Public Domain Music]",
    "The White of Lightning",
    78.0,
);
const BACH_7: (&str, &str, &str, f32) = (
    "Vy6jnaV32Uk",
    "Johann Sebastian Bach - Goldberg Variations BWV 988: Variation 7 [Classical / Public Domain Music]",
    "The White of Lightning",
    175.0,
);
const QUARTET: (&str, &str, &str, f32) = (
    "GUr4sH4uh2s",
    "Wolfgang Amadeus Mozart – Dissonance Quartet, K. 465: II. Andante cantabile | Musopen String Quartet",
    "Timeless Classical Archive",
    474.0,
);
const FPS: f32 = 25.0;
const LENGTH: f32 = 15.5;

struct Track {
    id: &'static str,
    title: &'static str,
    channel: &'static str,
    duration: f32,
    thumb: Image,
    cover: Image,
    colour: slint::Color,
    bars: Vec<f32>,
}

fn picture(t: &thumbs::Thumbnail) -> Image {
    Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&t.rgba, t.width, t.height))
}

fn track((id, title, channel, duration): (&'static str, &'static str, &'static str, f32), measure: bool) -> Track {
    let small = thumbs::fetch(id, THUMB_WIDTH).expect("thumbnail");
    let big = thumbs::fetch(id, COVER_WIDTH).expect("cover");
    let bars = if measure {
        let link = format!("https://www.youtube.com/watch?v={id}");
        let url = extract::resolve(&link).expect("resolve").stream_url;
        // A stream can break off part-way; measure again until every bar is in.
        let levels = Levels::new(f64::from(duration));
        for _ in 0..3 {
            if levels.complete() {
                break;
            }
            featherplay::engine::measure(&url, levels.clone(), || true).expect("measure");
        }
        assert!(levels.complete(), "{title}: the waveform was only partly measured");
        levels.bars()
    } else {
        Vec::new()
    };
    let [r, g, b] = big.colour;
    Track {
        id,
        title,
        channel,
        duration,
        thumb: picture(&small),
        cover: picture(&big),
        colour: slint::Color::from_rgb_u8(r, g, b),
        bars,
    }
}

fn row(t: &Track, current: bool) -> QueueRow {
    QueueRow {
        title: t.title.into(),
        meta: format!("{} · {}", t.channel, clock(t.duration)).into(),
        current,
        id: t.id.into(),
        thumb: t.thumb.clone(),
        ..Default::default()
    }
}

fn key(ui: &AppWindow, text: impl Into<slint::SharedString> + Clone) {
    ui.window().dispatch_event(WindowEvent::KeyPressed { text: text.clone().into() });
    ui.window().dispatch_event(WindowEvent::KeyReleased { text: text.into() });
}

fn click(ui: &AppWindow, (x, y): (f32, f32)) {
    let position = LogicalPosition::new(x, y);
    let button = slint::platform::PointerEventButton::Left;
    ui.window().dispatch_event(WindowEvent::PointerPressed { position, button });
    ui.window().dispatch_event(WindowEvent::PointerReleased { position, button });
}

/// Shows `t` as the track playing, its cover fading in over the last one.
fn now_playing(ui: &AppWindow, t: &Track, up_next: &str) {
    ui.set_previous_cover(ui.get_cover());
    ui.set_cover(t.cover.clone());
    ui.set_cover_changed(true);
    ui.set_tinted(true);
    ui.set_cover_colour(t.colour);
    ui.set_track_title(t.title.into());
    ui.set_track_channel(t.channel.into());
    ui.set_up_next(up_next.into());
    ui.set_duration(t.duration);
    ui.set_has_track(true);
    ui.set_fresh(true);
}

#[test]
#[ignore = "renders the README's showcase: needs the network, and FEATHERPLAY_SHOWCASE"]
fn showcase() {
    let Ok(out) = std::env::var("FEATHERPLAY_SHOWCASE") else { return };
    let out = std::path::PathBuf::from(out);
    std::fs::create_dir_all(&out).unwrap();

    let mozart = track(MOZART, true);
    let chopin = track(CHOPIN, true);
    let (bach_1, bach_2, bach_8) = (track(BACH_1, false), track(BACH_2, false), track(BACH_8, false));
    let more: Vec<Track> = [BACH_ARIA, BACH_4, BACH_5, BACH_7, QUARTET].into_iter().map(|t| track(t, false)).collect();

    let options = i_slint_backend_testing::TestingBackendOptions {
        mock_time: false,
        threading: false,
        renderer_name: Some("software".into()),
    };
    let _ = slint::platform::set_platform(Box::new(i_slint_backend_testing::TestingBackend::new(options)));
    let ui = AppWindow::new().unwrap();
    ui.window().set_size(slint::LogicalSize::new(540.0, 860.0));
    ui.set_theme_mode(1);
    ui.set_ytdlp_version("2026.10.26".into());
    ui.set_app_version(env!("CARGO_PKG_VERSION").into());
    // Signed out: Settings shows the YouTube account row with Google's button.
    ui.set_account_state(1);
    ui.set_google_label(GOOGLE_LABEL.into());
    ui.on_is_link(|text| extract::is_link(text.trim()));

    let mut rows = vec![row(&mozart, true), row(&bach_1, false), row(&bach_2, false)];
    rows.extend(more.iter().map(|t| row(t, false)));
    let queue = Rc::new(VecModel::from(rows));
    ui.set_queue(ModelRc::from(queue.clone()));
    let wave = Rc::new(VecModel::from(mozart.bars.clone()));
    ui.set_levels(ModelRc::from(wave.clone()));
    now_playing(&ui, &mozart, bach_1.title);
    ui.set_cover_changed(false);
    ui.set_fresh(false);
    ui.set_playing(true);
    ui.show().unwrap();

    // The story, by second.
    let pasted = format!("https://www.youtube.com/watch?v={}", chopin.id);
    let added = format!("https://youtu.be/{}", bach_8.id);
    let paste = |ui: &AppWindow, link: &str| {
        // Ctrl+L puts the cursor in the search box; the link arrives as one paste.
        ui.window().dispatch_event(WindowEvent::KeyPressed { text: Key::Control.into() });
        key(ui, "l");
        ui.window().dispatch_event(WindowEvent::KeyReleased { text: Key::Control.into() });
        for c in link.chars() {
            ui.window().dispatch_event(WindowEvent::KeyPressed { text: c.to_string().into() });
        }
    };
    let wiped = |ui: &AppWindow| {
        ui.set_wiping(false);
        ui.set_theme_snapshot(Image::default());
    };
    // Where the pointer goes, and when: it shows up near the footer, opens Settings, picks
    // Light, then Dark, and closes the sheet. Places are read from the window, so they
    // follow the layout.
    let centre = |label: &str| {
        let element = i_slint_backend_testing::ElementHandle::find_by_accessible_label(&ui, label)
            .find(|e| e.size().width > 0.0)
            .expect(label);
        let (at, size) = (element.absolute_position(), element.size());
        (at.x + size.width / 2.0, at.y + size.height / 2.0)
    };
    let settings_button = centre("Settings");
    // The sheet slides in: its places are read once it has come to rest.
    ui.set_settings_open(true);
    for _ in 0..20 {
        slint::platform::update_timers_and_animations();
        ui.window().take_snapshot().unwrap();
        std::thread::sleep(Duration::from_millis(50));
    }
    let choice = {
        let element =
            // The switch, not the "Theme" heading over it (a text is named by its words).
            i_slint_backend_testing::ElementHandle::find_by_accessible_label(&ui, "Theme")
                .max_by(|a, b| a.size().width.total_cmp(&b.size().width))
                .unwrap();
        let (at, size) = (element.absolute_position(), element.size());
        move |i: f32| (at.x + size.width * (i + 0.5) / 3.0, at.y + size.height / 2.0)
    };
    let close = centre("Close settings");
    ui.set_settings_open(false);
    for _ in 0..20 {
        slint::platform::update_timers_and_animations();
        ui.window().take_snapshot().unwrap();
        std::thread::sleep(Duration::from_millis(50));
    }
    let (light, dark) = (choice(2.0), choice(1.0));
    // Each click lands while the pointer rests on its button.
    let stops = [
        (8.2, (settings_button.0 - 90.0, settings_button.1 - 60.0)),
        (8.7, settings_button),
        (9.1, settings_button),
        (9.8, light),
        (10.4, light),
        (11.2, dark),
        (11.8, dark),
        (12.6, close),
        (13.1, close),
        (14.0, (close.0 - 70.0, close.1 + 140.0)),
    ];
    let glide = move |t: f32| -> Option<(f32, f32)> {
        if t < stops[0].0 {
            return None;
        }
        for pair in stops.windows(2) {
            let ((t0, (x0, y0)), (t1, (x1, y1))) = (pair[0], pair[1]);
            if t <= t1 {
                let k = ((t - t0) / (t1 - t0)).clamp(0.0, 1.0);
                let k = k * k * (3.0 - 2.0 * k);
                return Some((x0 + (x1 - x0) * k, y0 + (y1 - y0) * k));
            }
        }
        Some(stops[stops.len() - 1].1)
    };
    let pointer = std::cell::Cell::new(None::<(f32, f32)>);
    // The theme change does what the app does: a picture of the old theme, wiped away.
    ui.on_pick_theme({
        let ui = ui.as_weak();
        move |mode| {
            let ui = ui.upgrade().unwrap();
            ui.set_theme_snapshot(Image::from_rgba8(ui.window().take_snapshot().unwrap()));
            ui.set_wiping(true);
            ui.set_theme_mode(mode);
        }
    });
    let mut done = [false; 14];
    let mut position = 96.0f32;
    let started = std::time::Instant::now();
    let frames = (LENGTH * FPS) as u32;
    for frame in 0..frames {
        let t = frame as f32 / FPS;
        let mut at = |i: usize, when: f32| {
            let due = t >= when && !done[i];
            if due {
                done[i] = true;
            }
            due
        };
        if at(0, 1.0) {
            paste(&ui, &pasted);
        }
        if at(1, 2.4) {
            key(&ui, Key::Return);
            // It plays next: in after the current track, which it replaces.
            queue.insert(1, row(&chopin, true));
            queue.set_row_data(0, row(&mozart, false));
            ui.set_loading(true);
            ui.set_playing(false);
            for i in 0..wave.row_count() {
                wave.set_row_data(i, -1.0);
            }
            position = 0.0;
        }
        if at(2, 3.5) {
            ui.set_loading(false);
            ui.set_playing(true);
            now_playing(&ui, &chopin, bach_1.title);
            // The app's own messages, as it says them.
            ui.set_status(say::playing(chopin.title).into());
        }
        if at(3, 4.2) {
            for (i, &bar) in chopin.bars.iter().enumerate() {
                wave.set_row_data(i, bar);
            }
        }
        if at(4, 5.3) {
            ui.set_status("".into());
        }
        if at(5, 5.8) {
            paste(&ui, &added);
        }
        if at(6, 6.9) {
            ui.window().dispatch_event(WindowEvent::KeyPressed { text: Key::Shift.into() });
            key(&ui, Key::Return);
            ui.window().dispatch_event(WindowEvent::KeyReleased { text: Key::Shift.into() });
            queue.push(row(&bach_8, false));
            ui.set_status(say::added(bach_8.title).into());
        }
        if at(7, 8.1) {
            ui.set_status("".into());
        }
        // The theme: Settings opened, Light picked, then Dark, and the sheet closed, all by
        // the pointer.
        if at(8, 8.9) {
            click(&ui, pointer.get().unwrap());
        }
        if at(9, 10.1) {
            click(&ui, pointer.get().unwrap());
        }
        if at(10, 10.9) {
            wiped(&ui);
        }
        if at(11, 11.5) {
            click(&ui, pointer.get().unwrap());
        }
        if at(12, 12.3) {
            wiped(&ui);
        }
        if at(13, 12.85) {
            click(&ui, pointer.get().unwrap());
        }
        if let Some((x, y)) = glide(t) {
            pointer.set(Some((x, y)));
            ui.window().dispatch_event(WindowEvent::PointerMoved { position: LogicalPosition::new(x, y) });
        }
        // Playback runs at its real speed, like everything else.
        if ui.get_playing() {
            position += 1.0 / FPS;
        }
        ui.set_position(position);

        // Keep to the clock, so animations run at their real speed.
        let due = started + Duration::from_secs_f32(t);
        if let Some(wait) = due.checked_duration_since(std::time::Instant::now()) {
            std::thread::sleep(wait);
        }
        slint::platform::update_timers_and_animations();
        let shot = ui.window().take_snapshot().unwrap();
        let mut raw = Vec::with_capacity(shot.as_slice().len() * 4);
        for p in shot.as_slice() {
            raw.extend_from_slice(&[p.r, p.g, p.b, p.a]);
        }
        std::fs::write(out.join(format!("frame-{frame:04}-{}x{}.rgba", shot.width(), shot.height())), raw).unwrap();
    }
    let late = started.elapsed().as_secs_f32() - LENGTH;
    assert!(late < 1.0, "rendering fell {late:.1} s behind the clock: the animations would look rushed");
}
