//! The desktop around the window: media keys and the system's "now playing" display
//! (Windows' media overlay, MPRIS on Linux), and the tray icon.
//!
//! Both turn what the user does outside the window into a `Command`.

use std::ffi::c_void;
use std::sync::Arc;
use std::time::{Duration, Instant};

use souvlaki::{
    MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig, SeekDirection,
};

pub const APP_NAME: &str = "Featherplay";

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    Toggle,
    Play,
    Pause,
    Next,
    Previous,
    Stop,
    /// Seconds into the track.
    SeekTo(f64),
    /// Seconds forward, or back when negative.
    SeekBy(f64),
    /// A link to open, as from `playerctl open <link>`.
    Open(String),
    Show,
    Quit,
}

pub type Handler = Arc<dyn Fn(Command) + Send + Sync>;

/// The media keys, and the track shown by the OS. Updates are cheap no-ops when
/// nothing changed, so they can be sent freely.
pub struct MediaKeys {
    controls: MediaControls,
    track: Option<(String, u64, Option<String>)>,
    state: Option<(bool, bool)>,
    /// When the position was last reported. The OS shows what it was last told, so
    /// it is retold every second while playing.
    reported: Option<Instant>,
}

impl MediaKeys {
    /// `window` is the native window handle Windows ties media controls to.
    pub fn new(window: Option<*mut c_void>, handler: Handler) -> Result<Self, String> {
        // souvlaki panics, rather than failing, without a window on Windows.
        if cfg!(windows) && window.is_none() {
            return Err("the window has no native handle yet".into());
        }
        let config = PlatformConfig { display_name: APP_NAME, dbus_name: "featherplay", hwnd: window };
        let mut controls = MediaControls::new(config).map_err(|e| format!("{e:?}"))?;
        controls
            .attach(move |event| {
                let command = match event {
                    MediaControlEvent::Toggle => Command::Toggle,
                    MediaControlEvent::Play => Command::Play,
                    MediaControlEvent::Pause => Command::Pause,
                    MediaControlEvent::Next => Command::Next,
                    MediaControlEvent::Previous => Command::Previous,
                    MediaControlEvent::Stop => Command::Stop,
                    MediaControlEvent::Seek(SeekDirection::Forward) => Command::SeekBy(10.0),
                    MediaControlEvent::Seek(SeekDirection::Backward) => Command::SeekBy(-10.0),
                    MediaControlEvent::SeekBy(SeekDirection::Forward, by) => Command::SeekBy(by.as_secs_f64()),
                    MediaControlEvent::SeekBy(SeekDirection::Backward, by) => Command::SeekBy(-by.as_secs_f64()),
                    MediaControlEvent::SetPosition(MediaPosition(at)) => Command::SeekTo(at.as_secs_f64()),
                    MediaControlEvent::Raise => Command::Show,
                    MediaControlEvent::Quit => Command::Quit,
                    MediaControlEvent::OpenUri(uri) => Command::Open(uri),
                    // Volume, and the newer events (shuffle, repeat; macOS only so far), are
                    // left to the window.
                    _ => return,
                };
                handler(command);
            })
            .map_err(|e| format!("{e:?}"))?;
        Ok(Self { controls, track: None, state: None, reported: None })
    }

    /// `cover` is a link to the track's picture, which Windows fetches itself to show in its
    /// media overlay.
    pub fn set_track(&mut self, title: &str, duration: f64, cover: Option<&str>) {
        let key = (title.to_string(), duration as u64, cover.map(str::to_string));
        if self.track.as_ref() == Some(&key) {
            return;
        }
        let metadata = MediaMetadata {
            title: Some(title),
            duration: (duration > 0.0).then(|| Duration::from_secs_f64(duration)),
            cover_url: cover,
            ..Default::default()
        };
        if self.controls.set_metadata(metadata).is_ok() {
            self.track = Some(key);
        }
    }

    /// `loaded` is whether there is a track at all. Call `seeked` too after a jump.
    pub fn set_state(&mut self, loaded: bool, playing: bool, position: f64) {
        let stale = playing && self.reported.is_none_or(|at| at.elapsed() >= Duration::from_secs(1));
        if self.state == Some((loaded, playing)) && !stale {
            return;
        }
        let progress = Some(MediaPosition(Duration::from_secs_f64(position.max(0.0))));
        let playback = match (loaded, playing) {
            (false, _) => MediaPlayback::Stopped,
            (true, true) => MediaPlayback::Playing { progress },
            (true, false) => MediaPlayback::Paused { progress },
        };
        if self.controls.set_playback(playback).is_ok() {
            self.state = Some((loaded, playing));
            self.reported = Some(Instant::now());
        }
    }

    /// Reports the position on the next update rather than up to a second later.
    pub fn seeked(&mut self) {
        self.reported = None;
        self.state = None;
    }
}

pub use tray::Tray;

#[cfg(windows)]
mod tray {
    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

    use super::{APP_NAME, Command, Handler};
    use crate::icon;

    pub struct Tray {
        icon: TrayIcon,
        toggle: MenuItem,
    }

    impl Tray {
        /// Must be called on the thread running the window's event loop.
        pub fn new(handler: Handler) -> Option<Self> {
            let show = MenuItem::new("Show", true, None);
            let toggle = MenuItem::new("Play", true, None);
            let next = MenuItem::new("Next", true, None);
            let previous = MenuItem::new("Previous", true, None);
            let quit = MenuItem::new("Quit", true, None);
            let menu = Menu::new();
            menu.append_items(&[
                &show,
                &PredefinedMenuItem::separator(),
                &toggle,
                &next,
                &previous,
                &PredefinedMenuItem::separator(),
                &quit,
            ])
            .ok()?;

            let ids = [
                (show.id().clone(), Command::Show),
                (toggle.id().clone(), Command::Toggle),
                (next.id().clone(), Command::Next),
                (previous.id().clone(), Command::Previous),
                (quit.id().clone(), Command::Quit),
            ];
            let on_menu = handler.clone();
            MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
                if let Some((_, command)) = ids.iter().find(|(id, _)| *id == event.id) {
                    on_menu(command.clone());
                }
            }));
            TrayIconEvent::set_event_handler(Some(move |event| {
                if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } =
                    event
                {
                    handler(Command::Show);
                }
            }));

            let icon = Icon::from_rgba(icon::rgba(32), 32, 32).ok()?;
            let icon = TrayIconBuilder::new()
                .with_icon(icon)
                .with_tooltip(APP_NAME)
                .with_menu(Box::new(menu))
                .with_menu_on_left_click(false)
                .build()
                .ok()?;
            Some(Self { icon, toggle })
        }

        pub fn update(&mut self, title: &str, playing: bool) {
            let tooltip = if title.is_empty() { APP_NAME.to_string() } else { format!("{title} — {APP_NAME}") };
            // Windows caps tray tooltips at 127 characters.
            let tooltip: String = tooltip.chars().take(127).collect();
            self.icon.set_tooltip(Some(tooltip)).ok();
            self.toggle.set_text(if playing { "Pause" } else { "Play" });
        }
    }
}

#[cfg(target_os = "linux")]
mod tray {
    use ksni::blocking::{Handle, TrayMethods};
    use ksni::menu::StandardItem;
    use ksni::{Icon, MenuItem, ToolTip};

    use super::{APP_NAME, Command, Handler};
    use crate::icon;

    pub struct Tray {
        handle: Handle<Item>,
        shown: (String, bool),
    }

    pub struct Item {
        handler: Handler,
        title: String,
        playing: bool,
    }

    impl Tray {
        /// `None` where the desktop has no tray (no StatusNotifier host).
        pub fn new(handler: Handler) -> Option<Self> {
            let item = Item { handler, title: String::new(), playing: false };
            let handle = item.spawn().ok()?;
            Some(Self { handle, shown: (String::new(), false) })
        }

        pub fn update(&mut self, title: &str, playing: bool) {
            let next = (title.to_string(), playing);
            if self.shown == next {
                return;
            }
            self.handle.update(|item| {
                item.title = next.0.clone();
                item.playing = playing;
            });
            self.shown = next;
        }
    }

    impl Item {
        fn entry(label: &str, command: Command) -> MenuItem<Self> {
            StandardItem {
                label: label.into(),
                activate: Box::new(move |item: &mut Self| (item.handler)(command.clone())),
                ..Default::default()
            }
            .into()
        }
    }

    impl ksni::Tray for Item {
        fn id(&self) -> String {
            "featherplay".into()
        }

        fn title(&self) -> String {
            APP_NAME.into()
        }

        fn icon_pixmap(&self) -> Vec<Icon> {
            // StatusNotifier wants ARGB in network byte order.
            let mut data = icon::rgba(32);
            for pixel in data.as_chunks_mut::<4>().0 {
                pixel.rotate_right(1);
            }
            vec![Icon { width: 32, height: 32, data }]
        }

        fn tool_tip(&self) -> ToolTip {
            ToolTip { title: APP_NAME.into(), description: self.title.clone(), ..Default::default() }
        }

        fn activate(&mut self, _x: i32, _y: i32) {
            (self.handler)(Command::Show);
        }

        fn menu(&self) -> Vec<MenuItem<Self>> {
            vec![
                Self::entry("Show", Command::Show),
                MenuItem::Separator,
                Self::entry(if self.playing { "Pause" } else { "Play" }, Command::Toggle),
                Self::entry("Next", Command::Next),
                Self::entry("Previous", Command::Previous),
                MenuItem::Separator,
                Self::entry("Quit", Command::Quit),
            ]
        }
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
mod tray {
    use super::Handler;

    pub struct Tray;

    impl Tray {
        pub fn new(_handler: Handler) -> Option<Self> {
            None
        }

        pub fn update(&mut self, _title: &str, _playing: bool) {}
    }
}
