// No console window behind the app in release builds on Windows.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::c_void;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use slint::{
    CloseRequestResponse, ComponentHandle, Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, Timer, TimerMode,
    VecModel,
};

use featherplay::account::{self, Account, Note};
use featherplay::app::{App, Event, Place};
use featherplay::desktop::{self, Command, MediaKeys, Tray};
use featherplay::engine::Engine;
use featherplay::extract;
use featherplay::google::{self, Google};
use featherplay::instance::{self, Claim};
use featherplay::levels::{self as wave, Levels};
use featherplay::library::{self, Library};
use featherplay::log;
use featherplay::menu::{self, Action, Facts};
use featherplay::messages as say;
use featherplay::queue::{Repeat, Track};
use featherplay::store::{self, PinnedList, PinnedLists, Settings, ThemeMode, WindowPlace};
use featherplay::thumbs::{self, Thumbnail};
use featherplay::update::{self, Release};

slint::include_modules!();

// How long a message stays up.
const TOAST_TIME: Duration = Duration::from_secs(4);
// Slices the waveform divides a track into.
const WAVE_BARS: usize = wave::SLICES;
// How often the playhead may move, at most and at least.
const PLAYHEAD_FASTEST: Duration = Duration::from_millis(33);
const PLAYHEAD_SLOWEST: Duration = Duration::from_millis(250);
// How long a theme change's wipe takes, a little more than its animation.
const THEME_WIPE_TIME: Duration = Duration::from_millis(700);
// After an update the new version is started with the first, while the old one closes,
// and with the second too when music was playing, so it carries on.
const UPDATED_ARG: &str = "--updated";
const PLAYING_ARG: &str = "--playing";
// Featherplay looks for a new version once a day, a little after it starts.
const APP_CHECK_FILE: &str = "app-checked";
const APP_CHECK_EVERY: Duration = Duration::from_secs(24 * 3600);
const APP_CHECK_DELAY: Duration = Duration::from_secs(20);
// Set once an update is in place: the app restarts into it as it quits.
static RESTART: AtomicBool = AtomicBool::new(false);

// The "Sign in with Google" label in Roboto Medium, as outlines (scripts/google_button.py).
const GOOGLE_LABEL: &str = include_str!("google_label.txt");

// Ticks spent waiting for the window before giving up on media keys: five seconds.
const MEDIA_KEY_ATTEMPTS: u32 = 20;

fn main() -> Result<(), slint::PlatformError> {
    let data_dir = store::data_dir();
    log_panics_to(data_dir.join("crash.log"));
    log::init(&data_dir);

    // A link or search given on the command line ("Open with", scripts) starts playing,
    // in the window already open if there is one.
    let args: Vec<String> = std::env::args().skip(1).collect();
    let updated = args.iter().any(|a| a == UPDATED_ARG);
    let arg = args.iter().find(|a| !a.starts_with("--") && !a.trim().is_empty()).cloned();
    // The version an update replaced: removed now, or, right after the update, once it
    // has finished closing (its file stays locked until then), before taking its place.
    if let Ok(exe) = std::env::current_exe() {
        update::remove_old(&exe, if updated { Duration::from_secs(15) } else { Duration::ZERO });
    }
    let instance = match instance::claim(&data_dir, arg.as_deref().unwrap_or("")) {
        Ok(Claim::Handed) => return Ok(()),
        Ok(Claim::First(listener)) => Some(listener),
        // Not being able to listen costs only the hand-over, not the player.
        Err(_) => None,
    };

    let ui = AppWindow::new()?;

    let engine = match Engine::new() {
        Ok(engine) => engine,
        Err(e) => {
            ui.set_status(e.into());
            return ui.run();
        }
    };
    let settings = Settings::load(&data_dir);
    // Where the window was last time, kept current as it moves (see the ticker).
    let place = Rc::new(std::cell::Cell::new(settings.window));
    let playback = engine.playback();
    ui.set_volume(settings.volume);
    ui.set_theme_mode(match settings.theme {
        ThemeMode::Auto => 0,
        ThemeMode::Dark => 1,
        ThemeMode::Light => 2,
    });
    ui.set_show_thumbs(settings.thumbnails);
    ui.set_show_remaining(settings.remaining);
    playback.set_volume(settings.volume);

    let lists = Lists::default();
    let thumbs = Thumbs::new();
    ui.set_queue(ModelRc::from(lists.queue.clone()));
    ui.set_history(ModelRc::from(lists.history.clone()));
    ui.set_results(ModelRc::from(lists.results.clone()));
    ui.set_pins(ModelRc::from(lists.pins.clone()));
    let wave_model = Rc::new(VecModel::from(vec![-1.0f32; WAVE_BARS]));
    ui.set_levels(ModelRc::from(wave_model.clone()));

    // Workers report through events; the UI thread then reads the state it needs.
    let app = App::new(playback, &data_dir, {
        let ui = ui.as_weak();
        move |event| {
            ui.upgrade_in_event_loop(move |ui| match event {
                Event::Status(message) => ui.set_status(message.into()),
                Event::YtDlpVersion(version) => ui.set_ytdlp_version(version.into()),
                Event::QueueChanged | Event::NowPlaying => ui.invoke_refresh(),
                // Pins show in the library's lists too.
                Event::SavedChanged => {
                    ui.invoke_refresh();
                    ui.invoke_library_changed();
                }
                Event::SearchDone(done) => {
                    ui.set_searching(false);
                    match done {
                        Ok(()) => ui.set_status("".into()),
                        Err(e) => {
                            log::write(format!("Search failed: {e}"));
                            ui.set_status(say::search_failed().into());
                        }
                    }
                    ui.invoke_refresh();
                }
                Event::SearchMore(done) => {
                    // The loading message goes, unless something else was said since.
                    if ui.get_status() == say::loading_more_results().as_str() {
                        ui.set_status("".into());
                    }
                    if let Err(e) = done {
                        log::write(format!("Loading more results failed: {e}"));
                        ui.set_status(say::more_results_failed().into());
                    }
                    ui.invoke_refresh();
                }
            })
            .ok();
        }
    });
    app.set_repeat(settings.repeat);
    app.set_shuffle(settings.shuffle);
    ui.set_repeat_mode(repeat_mode(settings.repeat));
    ui.set_shuffle(settings.shuffle);
    // A link given at launch replaces the queue anyway; otherwise pick up the last one.
    if arg.is_none() {
        app.restore_session();
        if updated && args.iter().any(|a| a == PLAYING_ARG) {
            app.resume();
        }
    }

    ui.on_refresh({
        let ui = ui.as_weak();
        let app = app.clone();
        let lists = lists.clone();
        let thumbs = thumbs.clone();
        let shown_track = RefCell::new(None);
        move || {
            if let Some(ui) = ui.upgrade() {
                refresh(&ui, &app, &lists, &thumbs, &shown_track);
            }
        }
    });
    ui.on_want_thumb({
        let thumbs = thumbs.clone();
        move |id| thumbs.want(&id, false)
    });
    ui.on_play_all_pins({
        let ui = ui.as_weak();
        let app = app.clone();
        move || {
            app.play_all(app.pins().iter().map(Track::from).collect());
            if let Some(ui) = ui.upgrade() {
                ui.set_tab(0);
            }
        }
    });

    // Links open; anything else is a search.
    ui.on_is_link(|text| extract::is_link(text.trim()));
    ui.on_open_link({
        let ui = ui.as_weak();
        let app = app.clone();
        move |text, play_now| {
            let text = text.trim().to_string();
            if text.is_empty() {
                return;
            }
            if extract::is_link(&text) {
                app.open_link(text, play_now);
            } else if let Some(ui) = ui.upgrade() {
                ui.set_searching(true);
                ui.set_tab(3);
                ui.set_status(say::searching(&text).into());
                app.search(text);
            }
        }
    });
    ui.on_play_result({
        let app = app.clone();
        move |row| pick(&app, app.search_results(), row, true)
    });
    ui.on_add_result({
        let app = app.clone();
        move |row| pick(&app, app.search_results(), row, false)
    });
    ui.on_play_history({
        let app = app.clone();
        move |row| pick(&app, app.history().iter().map(Track::from).collect(), row, true)
    });
    ui.on_add_history({
        let app = app.clone();
        move |row| pick(&app, app.history().iter().map(Track::from).collect(), row, false)
    });
    ui.on_clear_history({
        let app = app.clone();
        move || app.clear_history()
    });
    ui.on_toggle_pause({
        let app = app.clone();
        move || app.toggle_pause()
    });
    ui.on_previous({
        let app = app.clone();
        move || app.previous()
    });
    ui.on_next({
        let app = app.clone();
        move || app.advance(false)
    });
    ui.on_seek_by({
        let app = app.clone();
        move |delta| app.seek_by(f64::from(delta))
    });
    ui.on_seek({
        let app = app.clone();
        move |seconds| app.seek(f64::from(seconds))
    });
    ui.on_volume_changed({
        let ui = ui.as_weak();
        let app = app.clone();
        move |volume| {
            let muted = ui.upgrade().is_some_and(|ui| ui.get_muted());
            app.playback().set_volume(if muted { 0.0 } else { volume });
        }
    });
    ui.on_cycle_repeat({
        let ui = ui.as_weak();
        let app = app.clone();
        move || {
            let repeat = app.cycle_repeat();
            if let Some(ui) = ui.upgrade() {
                ui.set_repeat_mode(repeat_mode(repeat));
                ui.invoke_settings_changed();
            }
        }
    });
    ui.on_toggle_shuffle({
        let ui = ui.as_weak();
        let app = app.clone();
        move || {
            let on = app.toggle_shuffle();
            if let Some(ui) = ui.upgrade() {
                ui.set_shuffle(on);
                ui.invoke_settings_changed();
            }
        }
    });
    ui.on_play_row({
        let app = app.clone();
        move |row| app.play(row as usize)
    });
    // Pins: from any list, and the pinned list's own buttons.
    ui.on_pin_row({
        let app = app.clone();
        move |row| pin(&app, app.tracks(), row)
    });
    ui.on_pin_history({
        let app = app.clone();
        move |row| pin(&app, app.history().iter().map(Track::from).collect(), row)
    });
    ui.on_pin_result({
        let app = app.clone();
        move |row| pin(&app, app.search_results(), row)
    });
    ui.on_unpin({
        let app = app.clone();
        move |row| pin(&app, app.pins().iter().map(Track::from).collect(), row)
    });
    ui.on_play_pin({
        let app = app.clone();
        move |row| pick(&app, app.pins().iter().map(Track::from).collect(), row, true)
    });
    ui.on_add_pin({
        let app = app.clone();
        move |row| pick(&app, app.pins().iter().map(Track::from).collect(), row, false)
    });
    ui.on_remove_history({
        let app = app.clone();
        move |row| {
            if let Some(entry) = usize::try_from(row).ok().and_then(|row| app.history().into_iter().nth(row)) {
                app.remove_from_history(&entry.url);
            }
        }
    });
    ui.on_remove_row({
        let app = app.clone();
        move |row| app.remove(row as usize)
    });
    ui.on_move_row({
        let app = app.clone();
        move |from, to| app.move_track(from as usize, to as usize)
    });
    ui.on_move_pin({
        let app = app.clone();
        move |from, to| app.move_pin(from as usize, to as usize)
    });
    // The YouTube account.
    let client = google::Client::choose(settings.google_client_id.as_deref(), settings.google_client_secret.as_deref());
    let account = Account::new(&data_dir, client, Google::from_env(), account::Seal::system(), {
        let ui = ui.as_weak();
        move |note| {
            ui.upgrade_in_event_loop(move |ui| match note {
                Note::Changed => ui.invoke_account_changed(),
                Note::Message(message) => ui.set_status(message.into()),
            })
            .ok();
        }
    });
    ui.on_sign_in({
        let account = account.clone();
        move || account.sign_in(open_in_browser)
    });
    ui.on_cancel_sign_in({
        let account = account.clone();
        move || account.cancel()
    });
    ui.on_copy_sign_in_link({
        let ui = ui.as_weak();
        let account = account.clone();
        move || {
            if let (Some(ui), account::Status::Waiting { url }) = (ui.upgrade(), account.status()) {
                ui.invoke_copy_text(url.into());
                ui.set_status(say::link_copied().into());
            }
        }
    });
    ui.on_sign_out({
        let account = account.clone();
        move || account.sign_out()
    });

    // Whole playlists pinned.
    let pinned_lists = Rc::new(RefCell::new(PinnedLists::load(&data_dir)));
    ui.set_pinned_lists(ModelRc::from(lists.pinned_lists.clone()));
    // The account's library, read only once the tab is first opened.
    let library = Library::new(&data_dir, account.clone(), Google::from_env(), account::Seal::system(), {
        let ui = ui.as_weak();
        move |note| {
            ui.upgrade_in_event_loop(move |ui| match note {
                library::Note::Changed => ui.invoke_library_changed(),
                library::Note::Message(message) => ui.set_status(message.into()),
            })
            .ok();
        }
    });
    // Private playlists from pasted links, which yt-dlp can't see, are read through the
    // account when signed in.
    app.read_private_lists_with({
        let library = library.clone();
        move |id| library.read_private(id)
    });
    ui.set_library(ModelRc::from(lists.library.clone()));
    ui.set_library_tracks(ModelRc::from(lists.library_tracks.clone()));
    ui.on_library_changed({
        let ui = ui.as_weak();
        let (app, library, lists, thumbs) = (app.clone(), library.clone(), lists.clone(), thumbs.clone());
        let pinned_lists = pinned_lists.clone();
        move || {
            if let Some(ui) = ui.upgrade() {
                show_library(&ui, &app, &library, &pinned_lists.borrow(), &lists, &thumbs);
            }
        }
    });
    ui.on_library_shown({
        let library = library.clone();
        move || library.opened()
    });
    ui.on_library_refresh({
        let library = library.clone();
        move || library.refresh()
    });
    ui.on_library_open_list({
        let ui = ui.as_weak();
        let library = library.clone();
        move |id| {
            let Some(ui) = ui.upgrade() else { return };
            ui.set_library_open(id.clone());
            library.open(&id);
            ui.invoke_library_changed();
        }
    });
    ui.on_results_more({
        let ui = ui.as_weak();
        let app = app.clone();
        move || {
            if app.search_more()
                && let Some(ui) = ui.upgrade()
            {
                ui.set_status(say::loading_more_results().into());
            }
        }
    });
    ui.on_library_more({
        let ui = ui.as_weak();
        let library = library.clone();
        move || {
            if let Some(ui) = ui.upgrade() {
                library.more(&ui.get_library_open());
            }
        }
    });
    ui.on_library_play({
        let ui = ui.as_weak();
        let (app, library, pinned_lists) = (app.clone(), library.clone(), pinned_lists.clone());
        move |id, shuffle| {
            let list = playlist_info(&library, &pinned_lists.borrow(), &id);
            queue_playlist(&ui, &app, &library, list, Place::Now, shuffle);
        }
    });
    ui.on_library_add({
        let ui = ui.as_weak();
        let (app, library, pinned_lists) = (app.clone(), library.clone(), pinned_lists.clone());
        move |id| {
            let list = playlist_info(&library, &pinned_lists.borrow(), &id);
            queue_playlist(&ui, &app, &library, list, Place::End, false);
        }
    });
    ui.on_pin_list({
        let ui = ui.as_weak();
        let (library, pinned_lists) = (library.clone(), pinned_lists.clone());
        move |id| {
            let list = playlist_info(&library, &pinned_lists.borrow(), &id);
            if let Err(e) = pinned_lists.borrow_mut().toggle(&list) {
                log::write(format!("Couldn't save the pinned playlists: {e}"));
            }
            if let Some(ui) = ui.upgrade() {
                ui.invoke_library_changed();
            }
        }
    });
    // A pinned playlist opens in the Library when signed in; signed out, a public one plays.
    ui.on_open_pinned_list({
        let ui = ui.as_weak();
        let (app, library, account, pinned_lists) =
            (app.clone(), library.clone(), account.clone(), pinned_lists.clone());
        move |id| {
            let Some(ui) = ui.upgrade() else { return };
            let list = playlist_info(&library, &pinned_lists.borrow(), &id);
            if matches!(account.status(), account::Status::SignedIn(_)) {
                ui.set_tab(4);
                ui.invoke_library_open_list(id);
            } else if list.private {
                ui.set_status(say::private_playlist_sign_in().into());
            } else {
                queue_playlist(&ui.as_weak(), &app, &library, list, Place::Now, false);
            }
        }
    });
    ui.on_library_play_track({
        let ui = ui.as_weak();
        let (app, library) = (app.clone(), library.clone());
        move |row| {
            if let Some(ui) = ui.upgrade() {
                pick(&app, opened_tracks(&ui, &library), row, true);
            }
        }
    });
    ui.on_library_add_track({
        let ui = ui.as_weak();
        let (app, library) = (app.clone(), library.clone());
        move |row| {
            if let Some(ui) = ui.upgrade() {
                pick(&app, opened_tracks(&ui, &library), row, false);
            }
        }
    });
    ui.on_library_pin_track({
        let ui = ui.as_weak();
        let (app, library) = (app.clone(), library.clone());
        move |row| {
            if let Some(ui) = ui.upgrade() {
                pin(&app, opened_tracks(&ui, &library), row);
            }
        }
    });

    ui.on_account_changed({
        let ui = ui.as_weak();
        let account = account.clone();
        let library = library.clone();
        // The picture shown, and whose it is.
        let shown: Rc<RefCell<Option<String>>> = Rc::default();
        move || {
            if let Some(ui) = ui.upgrade() {
                let status = account.status();
                show_account(&ui, &status, &shown);
                if !matches!(status, account::Status::SignedIn(_)) {
                    library.clear();
                    ui.set_library_open("".into());
                }
                ui.invoke_library_changed();
            }
        }
    });
    ui.set_google_label(GOOGLE_LABEL.into());
    ui.invoke_account_changed();

    // A track's menu acts on the track it was opened for, whatever the list does meanwhile.
    let menu_target: Rc<RefCell<Option<(menu::List, Track)>>> = Rc::default();
    // A playlist's menu: the Library's rows (6) or the pinned playlists (7).
    let menu_playlist: Rc<RefCell<Option<PinnedList>>> = Rc::default();
    ui.on_open_menu({
        let ui = ui.as_weak();
        let app = app.clone();
        let menu_target = menu_target.clone();
        let (menu_playlist, pinned_lists, library) = (menu_playlist.clone(), pinned_lists.clone(), library.clone());
        move |list, row, x, y| {
            let Some(ui) = ui.upgrade() else { return };
            if list == 6 || list == 7 {
                let pinned = pinned_lists.borrow();
                let id = match (list, usize::try_from(row)) {
                    (6, Ok(0)) => Some(library::LIKED.to_string()),
                    (6, Ok(row)) => library.view().playlists.get(row - 1).map(|p| p.id.clone()),
                    (_, Ok(row)) => pinned.entries().get(row).map(|p| p.id.clone()),
                    _ => None,
                };
                let Some(id) = id else { return };
                let playlist = playlist_info(&library, &pinned, &id);
                let items: Vec<MenuEntry> = menu::playlist_items(pinned.contains(&id))
                    .into_iter()
                    .map(|i| MenuEntry { label: i.label.into(), action: i.action.id().into(), separated: i.separated })
                    .collect();
                ui.set_menu_items(ModelRc::new(VecModel::from(items)));
                ui.set_menu_x(x);
                ui.set_menu_y(y);
                *menu_target.borrow_mut() = None;
                *menu_playlist.borrow_mut() = Some(playlist);
                ui.set_menu_open(true);
                return;
            }
            *menu_playlist.borrow_mut() = None;
            let Some(list) = menu::List::from_index(list) else { return };
            let tracks = match list {
                menu::List::Library => opened_tracks(&ui, &library),
                list => list_tracks(&app, list),
            };
            open_menu(&ui, &menu_target, list, &tracks, row, |track| track_facts(&app, track), (x, y));
        }
    });
    ui.on_menu_chose({
        let ui = ui.as_weak();
        let app = app.clone();
        let menu_target = menu_target.clone();
        let (menu_playlist, library) = (menu_playlist.clone(), library.clone());
        move |id| {
            let (Some(ui), Some(action)) = (ui.upgrade(), Action::from_id(&id)) else { return };
            if let Some(playlist) = menu_playlist.borrow_mut().take() {
                let place = match action {
                    Action::PlayNow => Some(Place::Now),
                    Action::PlayNext => Some(Place::Next),
                    Action::AddToEnd => Some(Place::End),
                    _ => None,
                };
                match (action, place) {
                    (_, Some(place)) => queue_playlist(&ui.as_weak(), &app, &library, playlist, place, false),
                    (Action::Pin | Action::Unpin, _) => ui.invoke_pin_list(playlist.id.into()),
                    (Action::CopyLink, _) => {
                        ui.invoke_copy_text(playlist.link().into());
                        ui.set_status(say::link_copied().into());
                    }
                    (Action::OpenOnYouTube, _) => {
                        open_in_browser(&playlist.link());
                    }
                    _ => {}
                }
                return;
            }
            let target = menu_target.borrow_mut().take();
            if let Some((list, track)) = target {
                act(&ui, &app, list, track, action);
            }
        }
    });
    ui.set_app_version(env!("CARGO_PKG_VERSION").into());
    ui.on_open_website(|| {
        open_in_browser(env!("CARGO_PKG_REPOSITORY"));
    });
    ui.on_open_policy(|which| {
        open_in_browser(&policy_link(which));
    });
    // Updating Featherplay itself.
    let found: Arc<Mutex<Option<Release>>> = Arc::default();
    ui.on_check_app_update({
        let (ui, found, data) = (ui.as_weak(), found.clone(), data_dir.clone());
        move || check_for_update(ui.clone(), found.clone(), data.clone(), false)
    });
    ui.on_install_app_update({
        let (ui, found) = (ui.as_weak(), found.clone());
        move || install_update(ui.clone(), found.clone())
    });
    let due = store::last_check(&data_dir, APP_CHECK_FILE)
        .and_then(|at| at.elapsed().ok())
        .is_none_or(|age| age >= APP_CHECK_EVERY);
    if due {
        let (ui, found, data) = (ui.as_weak(), found.clone(), data_dir.clone());
        Timer::single_shot(APP_CHECK_DELAY, move || check_for_update(ui, found, data, true));
    }

    // A new theme comes in with a diagonal wipe over a picture of the old one.
    let wipes = Rc::new(std::cell::Cell::new(0u32));
    ui.on_pick_theme({
        let ui = ui.as_weak();
        move |choice| {
            let Some(ui) = ui.upgrade() else { return };
            if choice == ui.get_theme_mode() {
                return;
            }
            // Without a picture (a renderer that can't take one) the theme just changes.
            if let Ok(picture) = ui.window().take_snapshot() {
                ui.set_theme_snapshot(Image::from_rgba8(picture));
                ui.set_wiping(true);
            }
            ui.set_theme_mode(choice);
            ui.invoke_settings_changed();
            let wipe = wipes.get() + 1;
            wipes.set(wipe);
            let (ui, wipes) = (ui.as_weak(), wipes.clone());
            Timer::single_shot(THEME_WIPE_TIME, move || {
                // A later change has its own wipe running; leave that one be.
                if let (Some(ui), true) = (ui.upgrade(), wipes.get() == wipe) {
                    ui.set_wiping(false);
                    ui.set_theme_snapshot(Image::default());
                }
            });
        }
    });
    ui.on_clear_queue({
        let app = app.clone();
        move || app.clear()
    });
    ui.on_update_ytdlp({
        let app = app.clone();
        move || app.update_ytdlp()
    });
    app.check_ytdlp();
    app.update_ytdlp_if_due();

    if let Some(place) = settings.window.filter(WindowPlace::plausible) {
        // Placed first, then sized in scale-free units: the window may be made on another
        // screen than its own, and Windows rescales it as it moves there.
        ui.window().set_position(slint::PhysicalPosition::new(place.x, place.y));
        let (width, height) = place.logical_size();
        ui.window().set_size(slint::LogicalSize::new(width, height));
    }
    ui.show()?;
    // The system window exists once the event loop runs: shape it then.
    Timer::single_shot(Duration::ZERO, {
        let ui = ui.as_weak();
        move || {
            if let Some(ui) = ui.upgrade() {
                frameless_window_looks(&ui);
            }
        }
    });
    if settings.window.is_some_and(|p| p.maximized) {
        ui.window().set_maximized(true);
    }

    // Media keys and the tray call in from their own threads; commands run on the UI thread.
    let handler: desktop::Handler = Arc::new({
        let ui = ui.as_weak();
        let app = app.clone();
        move |command| {
            let app = app.clone();
            ui.upgrade_in_event_loop(move |ui| run_command(&ui, &app, command)).ok();
        }
    });
    let mut tray = Tray::new(handler.clone());

    // With a tray, closing the window hides it and playback carries on.
    let has_tray = tray.is_some();
    ui.window().on_close_requested(move || {
        if !has_tray {
            slint::quit_event_loop().ok();
        }
        CloseRequestResponse::HideWindow
    });
    // The window's own title bar.
    ui.on_close_window({
        let ui = ui.as_weak();
        move || {
            let Some(ui) = ui.upgrade() else { return };
            // As the system's close button: to the tray if there is one, else quit.
            if has_tray {
                ui.hide().ok();
            } else {
                slint::quit_event_loop().ok();
            }
        }
    });

    // The UI polls playback a few times a second rather than the audio thread calling in.
    // The playhead moves in eighth-of-a-pixel steps (the waveform draws the part pixel):
    // updated as often as that takes, up to 30 times a second, and no more.
    let playhead = Rc::new(Timer::default());
    playhead.start(TimerMode::Repeated, PLAYHEAD_SLOWEST, {
        let (ui, app) = (ui.as_weak(), app.clone());
        let timer = Rc::downgrade(&playhead);
        move || {
            let (Some(ui), Some(timer)) = (ui.upgrade(), timer.upgrade()) else { return };
            let loaded = app.is_playing();
            if loaded && !ui.get_seeking() {
                ui.set_position((app.playback().position() as f32).min(ui.get_duration()));
            }
            let pixels = ui.get_wave_width() * ui.window().scale_factor();
            let step = match loaded && !app.playback().is_paused() && pixels > 0.0 {
                true => Duration::from_secs_f32(ui.get_duration() / (pixels * 8.0))
                    .clamp(PLAYHEAD_FASTEST, PLAYHEAD_SLOWEST),
                false => PLAYHEAD_SLOWEST,
            };
            if timer.interval().abs_diff(step) > Duration::from_millis(4) {
                timer.set_interval(step);
            }
        }
    });

    let ticker = Timer::default();
    let last_position = std::cell::Cell::new(0.0);
    // The window's place as last seen, and since when: saved once it has settled.
    let moving: RefCell<Option<(WindowPlace, Instant)>> = RefCell::new(None);
    let known_place = place.clone();
    // The waveform shown, by which levels and which version of them.
    let shown_levels: RefCell<Option<(Arc<Levels>, u64)>> = RefCell::new(None);
    // The message showing and since when: each one goes away by itself after a while.
    let toast: RefCell<(slint::SharedString, Instant)> = RefCell::new((Default::default(), Instant::now()));
    let ticker_lists = lists.clone();
    let ticker_thumbs = thumbs.clone();
    // Media keys are tied to the native window, which exists only once the event loop
    // runs, so they are connected from the first ticks rather than up front.
    let mut media_keys: Option<MediaKeys> = None;
    let mut media_key_attempts = 0;
    ticker.start(TimerMode::Repeated, Duration::from_millis(250), {
        let ui = ui.as_weak();
        let app = app.clone();
        move || {
            app.tick();
            let Some(ui) = ui.upgrade() else { return };
            ticker_thumbs.deliver(&ui, &ticker_lists);

            // The waveform fills in as the track is decoded; push it when it moved.
            let levels = app.levels();
            let mut shown = shown_levels.borrow_mut();
            let stale = match (&*shown, &levels) {
                (Some((was, version)), Some(now)) => !Arc::ptr_eq(was, now) || *version != now.version(),
                (None, None) => false,
                _ => true,
            };
            // A shape being measured is shown once it is whole, so it sweeps in at once
            // rather than in the uneven batches the measuring finishes them in.
            let ready = levels.as_ref().is_none_or(|l| l.complete() || !l.is_measuring());
            let switched = match (&*shown, &levels) {
                (Some((was, _)), Some(now)) => !Arc::ptr_eq(was, now),
                (None, None) => false,
                _ => true,
            };
            if stale && (ready || switched) {
                let bars = match &levels {
                    Some(l) if ready => l.bars(),
                    _ => vec![-1.0; WAVE_BARS],
                };
                // Rows are updated in place, so each bar animates to its new height.
                for (row, bar) in bars.into_iter().enumerate() {
                    if wave_model.row_data(row) != Some(bar) {
                        wave_model.set_row_data(row, bar);
                    }
                }
                *shown = levels.map(|l| {
                    // Not ready yet: shown as unknown, and looked at again until it is.
                    let version = if ready { l.version() } else { u64::MAX };
                    (l, version)
                });
            }
            drop(shown);

            // Remember where the window is, once it has stayed put a moment. A minimised
            // or hidden window reports no real place, so it is left alone.
            if let Some(now) = window_place(&ui) {
                let mut seen = moving.borrow_mut();
                match *seen {
                    Some((was, _)) if was == now => {}
                    _ => *seen = Some((now, Instant::now())),
                }
                if let Some((settled, since)) = *seen
                    && since.elapsed() >= Duration::from_secs(1)
                    && known_place.get() != Some(settled)
                {
                    known_place.set(Some(settled));
                    ui.invoke_settings_changed();
                }
            }

            let status = ui.get_status();
            let mut shown = toast.borrow_mut();
            if status != shown.0 {
                *shown = (status, Instant::now());
            } else if !status.is_empty() && shown.1.elapsed() >= TOAST_TIME {
                ui.set_status(Default::default());
            }
            drop(shown);
            let loaded = app.is_playing();
            let playing = loaded && !app.playback().is_paused();
            let position = app.playback().position();
            ui.set_playing(playing);

            if media_keys.is_none() && media_key_attempts < MEDIA_KEY_ATTEMPTS {
                media_key_attempts += 1;
                match MediaKeys::new(native_window(&ui), handler.clone()) {
                    Ok(keys) => media_keys = Some(keys),
                    Err(e) if media_key_attempts == MEDIA_KEY_ATTEMPTS => eprintln!("media keys unavailable: {e}"),
                    Err(_) => {}
                }
            }

            let title = if loaded { ui.get_track_title().to_string() } else { String::new() };
            if let Some(keys) = media_keys.as_mut() {
                if loaded {
                    keys.set_track(&title, f64::from(ui.get_duration()));
                }
                // A jump since the last tick (a seek, a new track) is reported at once.
                if (position - last_position.get()).abs() > 1.5 {
                    keys.seeked();
                }
                keys.set_state(loaded, playing, position);
            }
            last_position.set(position);
            if let Some(tray) = tray.as_mut() {
                tray.update(&title, playing);
            }
        }
    });

    // Saved as they change, not on exit, which a shutdown or a kill never reaches.
    let own_client = (settings.google_client_id.clone(), settings.google_client_secret.clone());
    ui.on_settings_changed({
        let ui = ui.as_weak();
        let app = app.clone();
        let place = place.clone();
        let thumbs = thumbs.clone();
        move || {
            let Some(ui) = ui.upgrade() else { return };
            let theme = match ui.get_theme_mode() {
                1 => ThemeMode::Dark,
                2 => ThemeMode::Light,
                _ => ThemeMode::Auto,
            };
            let thumbnails = ui.get_show_thumbs();
            if !thumbnails {
                // Off means off: let go of the ones held, not just stop showing them.
                thumbs.forget_small();
            }
            let settings = Settings {
                volume: ui.get_volume(),
                repeat: app.repeat(),
                shuffle: app.shuffle(),
                window: place.get(),
                theme,
                thumbnails,
                remaining: ui.get_show_remaining(),
                // Set by hand in the file: kept as they are.
                google_client_id: own_client.0.clone(),
                google_client_secret: own_client.1.clone(),
            };
            if let Err(e) = settings.save(&data_dir) {
                log::write(format!("Couldn't save the settings: {e}"));
                ui.set_status(say::couldnt_save("settings").into());
            }
        }
    });

    if let Some(arg) = arg {
        ui.invoke_open_link(arg.into(), true);
    }
    if let Some(listener) = instance {
        let ui = ui.as_weak();
        listener.serve(move |message| {
            ui.upgrade_in_event_loop(move |ui| {
                ui.show().ok();
                ui.window().set_minimized(false);
                if !message.is_empty() {
                    ui.invoke_open_link(message.into(), true);
                }
            })
            .ok();
        });
    }

    ui.invoke_refresh();
    let result = slint::run_event_loop_until_quit();
    let playing = app.is_playing() && !app.playback().is_paused();
    app.save_session();
    app.keep_levels();
    drop(engine);
    if RESTART.load(Ordering::SeqCst) {
        restart(playing);
    }
    result
}

/// Asks GitHub whether there is a newer version. `quiet` is the daily check: it says
/// nothing unless there is one, and then says so once.
fn check_for_update(
    ui: slint::Weak<AppWindow>,
    found: Arc<Mutex<Option<Release>>>,
    data: std::path::PathBuf,
    quiet: bool,
) {
    if let (false, Some(ui)) = (quiet, ui.upgrade()) {
        ui.set_app_update_state(1);
    }
    std::thread::spawn(move || {
        let result = update::check();
        store::mark_check(&data, APP_CHECK_FILE).ok();
        ui.upgrade_in_event_loop(move |ui| match result {
            Ok(Some(release)) => {
                ui.set_app_update_version(release.version.clone().into());
                ui.set_app_update_state(3);
                if quiet {
                    ui.set_status(say::app_update(&release.version).into());
                }
                *found.lock().unwrap() = Some(release);
            }
            Ok(None) if !quiet => ui.set_app_update_state(2),
            Err(e) if !quiet => {
                ui.set_app_update_error(e.into());
                ui.set_app_update_state(5);
            }
            _ => {}
        })
        .ok();
    });
}

/// Downloads the version found and puts it in place, then quits to restart into it. A
/// release without a build for this system is opened in the browser instead.
fn install_update(ui: slint::Weak<AppWindow>, found: Arc<Mutex<Option<Release>>>) {
    let Some(release) = found.lock().unwrap().clone() else { return };
    if release.download.is_none() {
        open_in_browser(&release.page);
        return;
    }
    let Ok(exe) = std::env::current_exe() else { return };
    if let Some(ui) = ui.upgrade() {
        ui.set_app_update_progress(0.0);
        ui.set_app_update_state(4);
    }
    std::thread::spawn(move || {
        let shown = std::sync::atomic::AtomicU32::new(0);
        let result = update::install(&release, &exe, |fraction| {
            // A percent at a time is plenty for the UI.
            let percent = (fraction * 100.0) as u32;
            if shown.swap(percent, Ordering::Relaxed) != percent {
                ui.upgrade_in_event_loop(move |ui| ui.set_app_update_progress(fraction)).ok();
            }
        });
        ui.upgrade_in_event_loop(move |ui| match result {
            Ok(()) => {
                ui.set_app_update_state(6);
                RESTART.store(true, Ordering::SeqCst);
                slint::quit_event_loop().ok();
            }
            Err(e) => {
                ui.set_app_update_error(e.into());
                ui.set_app_update_state(5);
            }
        })
        .ok();
    });
}

/// Starts the new version, which waits for this one to finish closing.
fn restart(playing: bool) {
    let Ok(exe) = std::env::current_exe() else { return };
    let mut command = std::process::Command::new(exe);
    command.arg(UPDATED_ARG);
    if playing {
        command.arg(PLAYING_ARG);
    }
    if let Err(e) = command.spawn() {
        eprintln!("couldn't restart after the update: {e}");
    }
}

/// A Windows release build has no console, so a panic would vanish without a trace:
/// write it down where a bug report can pick it up.
fn log_panics_to(path: std::path::PathBuf) {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        let report = format!("{} {info}\n{backtrace}\n", env!("CARGO_PKG_VERSION"));
        std::fs::write(&path, report).ok();
        default(info);
    }));
}

/// The window's place on screen, or `None` while it is minimised or hidden in the tray.
fn window_place(ui: &AppWindow) -> Option<WindowPlace> {
    let window = ui.window();
    if window.is_minimized() || !window.is_visible() {
        return None;
    }
    let maximized = window.is_maximized();
    let (position, size) = (window.position(), window.size());
    let place = WindowPlace {
        x: position.x,
        y: position.y,
        width: size.width,
        height: size.height,
        maximized,
        scale: window.scale_factor(),
    };
    place.plausible().then_some(place)
}

fn run_command(ui: &AppWindow, app: &Arc<App>, command: Command) {
    let playback = app.playback();
    match command {
        Command::Toggle => app.toggle_pause(),
        Command::Play => app.resume(),
        Command::Pause | Command::Stop => playback.pause(),
        Command::Next => app.advance(false),
        Command::Previous => app.previous(),
        Command::SeekTo(seconds) => app.seek(seconds),
        Command::SeekBy(delta) => app.seek_by(delta),
        Command::Open(link) => app.open_link(link, true),
        Command::Show => {
            ui.show().ok();
            ui.window().set_minimized(false);
        }
        Command::Quit => {
            slint::quit_event_loop().ok();
        }
    }
}

/// The native window handle, which Windows' media controls attach to.
fn native_window(ui: &AppWindow) -> Option<*mut c_void> {
    #[cfg(windows)]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let handle = ui.window().window_handle();
        match handle.window_handle().ok()?.as_raw() {
            RawWindowHandle::Win32(win32) => Some(win32.hwnd.get() as *mut c_void),
            _ => None,
        }
    }
    #[cfg(not(windows))]
    {
        let _ = ui;
        None
    }
}

/// Without the system's frame a window would lose its shadow and, on Windows 11, its
/// rounded corners: ask for both back.
fn frameless_window_looks(ui: &AppWindow) {
    #[cfg(windows)]
    {
        use slint::winit_030::WinitWindowAccessor;
        use slint::winit_030::winit::platform::windows::WindowExtWindows;
        ui.window().with_winit_window(|window| window.set_undecorated_shadow(true));

        #[link(name = "dwmapi")]
        unsafe extern "system" {
            fn DwmSetWindowAttribute(hwnd: *mut c_void, attribute: u32, value: *const c_void, size: u32) -> i32;
        }
        const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
        let round: u32 = 2; // DWMWCP_ROUND
        if let Some(hwnd) = native_window(ui) {
            // Older Windows doesn't know the attribute and says so; nothing to do then.
            // SAFETY: a valid window handle, and a value of the size given.
            unsafe { DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, (&raw const round).cast(), 4) };
        }
    }
    #[cfg(not(windows))]
    let _ = ui;
}

/// The models behind the lists the window shows.
#[derive(Clone, Default)]
struct Lists {
    queue: Rc<VecModel<QueueRow>>,
    history: Rc<VecModel<QueueRow>>,
    results: Rc<VecModel<QueueRow>>,
    pins: Rc<VecModel<QueueRow>>,
    library: Rc<VecModel<LibraryRow>>,
    library_tracks: Rc<VecModel<QueueRow>>,
    pinned_lists: Rc<VecModel<LibraryRow>>,
}

/// The tracks of the list opened in the Library, as loaded so far.
fn opened_tracks(ui: &AppWindow, library: &Library) -> Vec<Track> {
    library.list(&ui.get_library_open()).map(|l| l.tracks).unwrap_or_default()
}

/// The name of a list in the Library: "Liked videos", or the playlist's.
fn list_name(library: &Library, id: &str) -> String {
    if id == library::LIKED {
        return "Liked videos".into();
    }
    library.view().playlists.into_iter().find(|p| p.id == id).map_or_else(|| "the playlist".into(), |p| p.title)
}

/// A playlist as Featherplay knows it: pinned, in the Library, or just its id.
fn playlist_info(library: &Library, pinned: &PinnedLists, id: &str) -> PinnedList {
    let known = library.view().playlists.into_iter().find(|p| p.id == id);
    if let Some(p) = known {
        return PinnedList {
            id: p.id,
            title: p.title,
            private: p.privacy == library::Privacy::Private,
            thumb: p.thumb,
        };
    }
    if id == library::LIKED {
        return PinnedList { id: id.into(), title: "Liked videos".into(), private: true, thumb: None };
    }
    pinned.entries().iter().find(|p| p.id == id).cloned().unwrap_or_else(|| PinnedList {
        id: id.into(),
        title: "the playlist".into(),
        private: false,
        thumb: None,
    })
}

/// Queues a whole playlist (shuffled, if asked: only its own tracks). Private ones and
/// the liked videos are read through the account; public and unlisted ones through
/// yt-dlp, which costs none of Google's daily quota.
fn queue_playlist(
    ui: &slint::Weak<AppWindow>,
    app: &Arc<App>,
    library: &Arc<Library>,
    list: PinnedList,
    place: Place,
    shuffle: bool,
) {
    if let Some(ui) = ui.upgrade() {
        ui.set_status(say::loading_list(&list.title).into());
    }
    let (ui, app, library) = (ui.clone(), app.clone(), library.clone());
    std::thread::spawn(move || {
        let tracks = if list.private {
            library.all(&list.id).map_err(|e| match e {
                google::Error::Revoked => say::private_playlist_sign_in(),
                google::Error::Quota => say::library_quota(),
                e => {
                    log::write(format!("Couldn't load “{}” to play: {e}", list.title));
                    say::library_failed()
                }
            })
        } else {
            extract::list(&list.link()).map(featherplay::app::playlist_tracks).map_err(|e| {
                log::write(format!("Couldn't list “{}” to play: {e}", list.title));
                say::playlist_failed()
            })
        };
        match tracks {
            Ok(mut tracks) => {
                if shuffle {
                    shuffle_tracks(&mut tracks);
                }
                app.queue_list(tracks, &list.title, place);
            }
            Err(message) => {
                ui.upgrade_in_event_loop(move |ui| ui.set_status(message.into())).ok();
            }
        }
    });
}

/// Fisher–Yates, seeded from the clock: shuffling needs no better randomness.
fn shuffle_tracks(tracks: &mut [Track]) {
    let mut seed =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_nanos() as u64) | 1;
    for i in (1..tracks.len()).rev() {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        tracks.swap(i, (seed % (i as u64 + 1)) as usize);
    }
}

/// Shows the Library: its rows, and the list opened, if any.
fn show_library(
    ui: &AppWindow,
    app: &Arc<App>,
    library: &Library,
    pinned_lists: &PinnedLists,
    lists: &Lists,
    thumbs: &Thumbs,
) {
    let view = library.view();
    let signed_in = ui.get_account_state() >= 3;
    // Pinned playlists, for the Pinned tab.
    lists.pinned_lists.set_vec(
        pinned_lists
            .entries()
            .iter()
            .map(|p| {
                let known = view.playlists.iter().find(|k| k.id == p.id);
                let meta = match (known, p.private, signed_in) {
                    (Some(k), _, true) => format!("{} · {}", k.privacy.label(), say::count_tracks(k.count)),
                    (_, true, false) => "Private · sign in to open".into(),
                    (_, true, true) => "Private".into(),
                    (_, false, _) => "Playlist".into(),
                };
                let thumb_id = p.thumb.clone().unwrap_or_default();
                LibraryRow {
                    id: p.id.as_str().into(),
                    title: p.title.as_str().into(),
                    meta: meta.into(),
                    thumb: thumbs.small(&thumb_id).unwrap_or_default(),
                    thumb_id: thumb_id.into(),
                    liked: p.id == library::LIKED,
                    pinned: true,
                }
            })
            .collect::<Vec<_>>(),
    );
    let pinned: HashSet<String> = app.pins().iter().map(|p| p.url.clone()).collect();
    let mut playlists = vec![LibraryRow {
        id: library::LIKED.into(),
        title: "Liked videos".into(),
        meta: view.liked.map_or_else(|| "Your likes on YouTube".into(), say::count_tracks).into(),
        liked: true,
        pinned: pinned_lists.contains(library::LIKED),
        ..Default::default()
    }];
    playlists.extend(view.playlists.iter().map(|p| {
        let thumb_id = p.thumb.clone().unwrap_or_default();
        LibraryRow {
            id: p.id.as_str().into(),
            title: p.title.as_str().into(),
            meta: format!("{} · {}", p.privacy.label(), say::count_tracks(p.count)).into(),
            thumb: thumbs.small(&thumb_id).unwrap_or_default(),
            thumb_id: thumb_id.into(),
            liked: false,
            pinned: pinned_lists.contains(&p.id),
        }
    }));
    lists.library.set_vec(playlists);
    ui.set_library_refreshing(view.refreshing);
    ui.set_library_updated(
        match view.fetched.and_then(|at| at.elapsed().ok()) {
            _ if view.refreshing => "Refreshing…".to_string(),
            None => String::new(),
            Some(age) if age < Duration::from_secs(60) => "Updated just now".into(),
            Some(age) if age < Duration::from_secs(3600) => format!("Updated {} min ago", age.as_secs() / 60),
            Some(age) => format!("Updated {} h ago", age.as_secs() / 3600),
        }
        .into(),
    );

    let open = ui.get_library_open().to_string();
    ui.set_library_open_pinned(pinned_lists.contains(&open));
    if open.is_empty() {
        return;
    }
    let list = library.list(&open);
    let privacy = view.playlists.iter().find(|p| p.id == open).map(|p| p.privacy.label());
    let total =
        list.as_ref().map(|l| l.total).or_else(|| view.playlists.iter().find(|p| p.id == open).map(|p| p.count));
    let cover = view
        .playlists
        .iter()
        .find(|p| p.id == open)
        .and_then(|p| p.thumb.clone())
        .or_else(|| pinned_lists.entries().iter().find(|p| p.id == open).and_then(|p| p.thumb.clone()));
    ui.set_library_open_cover(cover.and_then(|id| thumbs.small(&id)).unwrap_or_default());
    ui.set_library_title(list_name(library, &open).into());
    ui.set_library_meta(
        [privacy.map(String::from), total.map(say::count_tracks), list.is_none().then(|| "loading…".to_string())]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ")
            .into(),
    );
    let tracks = list.map(|l| l.tracks).unwrap_or_default();
    lists.library_tracks.set_vec(rows(&tracks, None, &pinned, thumbs));
}

/// Shows the account in Settings; the channel's picture is fetched once, in the background.
fn show_account(ui: &AppWindow, status: &account::Status, shown: &Rc<RefCell<Option<String>>>) {
    use account::Status;
    ui.set_account_state(match status {
        Status::Unavailable => 0,
        Status::SignedOut => 1,
        Status::Waiting { .. } => 2,
        Status::SignedIn(Some(_)) => 3,
        Status::SignedIn(None) => 4,
    });
    let channel = match status {
        Status::SignedIn(Some(channel)) => Some(channel),
        _ => None,
    };
    ui.set_account_name(channel.map(|c| c.name.clone()).unwrap_or_default().into());
    let picture = channel.and_then(|c| c.picture.clone());
    if *shown.borrow() == picture {
        return;
    }
    *shown.borrow_mut() = picture.clone();
    ui.set_account_picture(Image::default());
    let Some(url) = picture else { return };
    let ui = ui.as_weak();
    std::thread::spawn(move || {
        // Twice the size shown, for high-DPI screens.
        match thumbs::fetch_picture(&url, 72) {
            Ok(picture) => {
                ui.upgrade_in_event_loop(move |ui| {
                    let pixels =
                        SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&picture.rgba, picture.width, picture.height);
                    ui.set_account_picture(Image::from_rgba8(pixels));
                })
                .ok();
            }
            Err(e) => log::write(format!("Couldn't fetch the channel's picture: {e}")),
        }
    });
}

/// The tracks of a list, as its rows show them.
fn list_tracks(app: &Arc<App>, list: menu::List) -> Vec<Track> {
    match list {
        menu::List::Queue => app.tracks(),
        menu::List::History => app.history().iter().map(Track::from).collect(),
        menu::List::Pinned => app.pins().iter().map(Track::from).collect(),
        menu::List::Results => app.search_results(),
        menu::List::NowPlaying => {
            app.now_playing().index.and_then(|i| app.tracks().get(i).cloned()).into_iter().collect()
        }
        // Read from the library by the caller.
        menu::List::Library => Vec::new(),
    }
}

fn track_facts(app: &Arc<App>, track: &Track) -> Facts {
    let tracks = app.tracks();
    let current = app.now_playing().index.and_then(|i| tracks.get(i)).is_some_and(|t| t.url == track.url);
    Facts {
        current,
        queued: tracks.iter().any(|t| t.url == track.url),
        pinned: app.is_pinned(&track.url),
        video: thumbs::video_id(&track.url).is_some(),
    }
}

/// Opens the menu for the track at `row` of `tracks`, keeping that track as its target.
fn open_menu(
    ui: &AppWindow,
    target: &RefCell<Option<(menu::List, Track)>>,
    list: menu::List,
    tracks: &[Track],
    row: i32,
    facts: impl Fn(&Track) -> Facts,
    (x, y): (f32, f32),
) {
    let Some(track) = usize::try_from(row).ok().and_then(|row| tracks.get(row)) else { return };
    let items: Vec<MenuEntry> = menu::items(list, facts(track))
        .into_iter()
        .map(|item| MenuEntry { label: item.label.into(), action: item.action.id().into(), separated: item.separated })
        .collect();
    ui.set_menu_items(ModelRc::new(VecModel::from(items)));
    ui.set_menu_x(x);
    ui.set_menu_y(y);
    *target.borrow_mut() = Some((list, track.clone()));
    ui.set_menu_open(true);
}

fn act(ui: &AppWindow, app: &Arc<App>, list: menu::List, track: Track, action: Action) {
    match action {
        // A queued track plays where it is; others join the queue.
        Action::PlayNow => app.add_track(track, true),
        Action::PlayNext => app.play_next(track),
        Action::AddToEnd => app.add_track(track, false),
        Action::Radio => app.start_radio(track),
        Action::Pin | Action::Unpin => app.toggle_pin(&track),
        Action::Remove if list == menu::List::History => app.remove_from_history(&track.url),
        Action::Remove => {
            if let Some(index) = app.tracks().iter().position(|t| t.url == track.url) {
                app.remove(index);
            }
        }
        Action::CopyLink => {
            ui.invoke_copy_text(extract::watch_link(&track.url).into());
            ui.set_status(say::link_copied().into());
        }
        Action::OpenOnYouTube => {
            open_in_browser(&extract::watch_link(&track.url));
        }
    }
}

/// Pins the track at `row` of a list, or unpins it.
fn pin(app: &Arc<App>, tracks: Vec<Track>, row: i32) {
    if let Some(track) = usize::try_from(row).ok().and_then(|row| tracks.into_iter().nth(row)) {
        app.toggle_pin(&track);
    }
}

/// Plays or queues the track at `row` of a list.
fn pick(app: &Arc<App>, tracks: Vec<Track>, row: i32, play: bool) {
    if let Some(track) = usize::try_from(row).ok().and_then(|row| tracks.into_iter().nth(row)) {
        app.add_track(track, play);
    }
}

fn rows<'a>(
    tracks: impl IntoIterator<Item = &'a Track>,
    current: Option<usize>,
    pinned: &HashSet<String>,
    thumbs: &Thumbs,
) -> Vec<QueueRow> {
    tracks
        .into_iter()
        .enumerate()
        .map(|(i, track)| {
            let id = thumbs::video_id(&track.url).unwrap_or_default();
            // Private and deleted videos in a Library playlist are listed as YouTube lists them.
            let meta = if track.unavailable && matches!(track.title.as_str(), "Private video" | "Deleted video") {
                "Can't be played · skipped".to_string()
            } else if track.unavailable {
                "Unavailable · skipped".to_string()
            } else if track.failed {
                "Couldn't load · click to try again".to_string()
            } else {
                let duration = track.duration.map(|d| clock(d as f32));
                [Some(track.channel.clone()).filter(|c| !c.is_empty()), duration]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" · ")
            };
            QueueRow {
                title: track.title.as_str().into(),
                meta: meta.into(),
                current: current == Some(i),
                unavailable: !track.playable(),
                pinned: pinned.contains(&track.url),
                thumb: thumbs.small(&id).unwrap_or_default(),
                id: id.into(),
            }
        })
        .collect()
}

/// Reads the queue, the lists and the current track into the window. `shown_track` is
/// the link of the track last shown, so a new one gets its fade-in.
fn refresh(ui: &AppWindow, app: &Arc<App>, lists: &Lists, thumbs: &Thumbs, shown_track: &RefCell<Option<String>>) {
    let now = app.now_playing();
    let pins: Vec<Track> = app.pins().iter().map(Track::from).collect();
    let pinned: HashSet<String> = pins.iter().map(|t| t.url.clone()).collect();
    let tracks = app.tracks();
    lists.queue.set_vec(rows(&tracks, now.index, &pinned, thumbs));
    let history: Vec<Track> = app.history().iter().map(Track::from).collect();
    lists.history.set_vec(rows(&history, None, &pinned, thumbs));
    lists.results.set_vec(rows(&app.search_results(), None, &pinned, thumbs));
    lists.pins.set_vec(rows(&pins, None, &pinned, thumbs));

    let current_url = now.index.and_then(|i| tracks.get(i)).map(|t| t.url.clone());
    if *shown_track.borrow() != current_url {
        if current_url.is_some() {
            ui.set_fresh(true);
        }
        *shown_track.borrow_mut() = current_url;
    }
    let cover_id = now.index.and_then(|i| tracks.get(i)).and_then(|t| thumbs::video_id(&t.url));
    thumbs.set_cover(ui, cover_id);

    ui.set_has_track(now.index.is_some());
    ui.set_loading(now.loading);
    let title = match (now.title.is_empty(), now.loading) {
        (false, _) => now.title,
        (true, true) => "Opening the link…".to_string(),
        (true, false) => "Nothing playing".to_string(),
    };
    ui.set_track_title(title.into());
    ui.set_track_channel(now.channel.into());
    ui.set_up_next(now.up_next.unwrap_or_default().into());
    ui.set_duration(now.duration as f32);
    let shown = match (now.loading, now.index, now.resume_at) {
        (false, Some(_), Some(at)) => Some(at as f32),
        (true, _, _) | (_, None, _) => Some(0.0),
        _ => None,
    };
    if let Some(at) = shown {
        ui.set_position(at);
    }
}

// Thumbnails are kept at twice the size they're shown at, for high-DPI screens.
const THUMB_WIDTH: u32 = 112;
const COVER_WIDTH: u32 = 320;
const THUMBS_KEPT: usize = 100;
const THUMB_WORKERS: usize = 3;
// A thumbnail that failed (offline, say) is tried again after this long.
const THUMB_RETRY: Duration = Duration::from_secs(60);

/// Thumbnails for the rows on screen and the current track. A few downloads run at
/// once, and only the most recently used thumbnails are kept, so memory stays small
/// whatever the length of the queue.
struct Thumbs {
    /// Most recently used last.
    kept: RefCell<VecDeque<(String, Image)>>,
    /// The cover's id, picture and representative colour.
    cover: RefCell<Option<(String, Image, slint::Color)>>,
    cover_wanted: RefCell<Option<String>>,
    /// Whose cover is on screen; it stays there until the next one has arrived.
    cover_shown: RefCell<Option<String>>,
    asked: RefCell<HashSet<(String, bool)>>,
    failed: RefCell<HashMap<(String, bool), Instant>>,
    jobs: mpsc::Sender<(String, bool)>,
    done: mpsc::Receiver<(String, bool, Option<Thumbnail>)>,
}

impl Thumbs {
    fn new() -> Rc<Self> {
        let (jobs, queue) = mpsc::channel::<(String, bool)>();
        let (finished, done) = mpsc::channel();
        let queue = Arc::new(Mutex::new(queue));
        for _ in 0..THUMB_WORKERS {
            let (queue, finished) = (queue.clone(), finished.clone());
            std::thread::spawn(move || {
                while let Ok((id, cover)) = queue.lock().unwrap().recv() {
                    let width = if cover { COVER_WIDTH } else { THUMB_WIDTH };
                    if finished.send((id.clone(), cover, thumbs::fetch(&id, width).ok())).is_err() {
                        return;
                    }
                }
            });
        }
        Rc::new(Self {
            kept: RefCell::default(),
            cover: RefCell::default(),
            cover_wanted: RefCell::default(),
            cover_shown: RefCell::default(),
            asked: RefCell::default(),
            failed: RefCell::default(),
            jobs,
            done,
        })
    }

    fn forget_small(&self) {
        self.kept.borrow_mut().clear();
    }

    fn small(&self, id: &str) -> Option<Image> {
        let mut kept = self.kept.borrow_mut();
        let at = kept.iter().position(|(kept_id, _)| kept_id == id)?;
        let entry = kept.remove(at)?;
        let image = entry.1.clone();
        kept.push_back(entry);
        Some(image)
    }

    fn want(&self, id: &str, cover: bool) {
        let key = (id.to_string(), cover);
        if id.is_empty() || self.asked.borrow().contains(&key) {
            return;
        }
        if self.failed.borrow().get(&key).is_some_and(|at| at.elapsed() < THUMB_RETRY) {
            return;
        }
        self.asked.borrow_mut().insert(key);
        self.jobs.send((id.to_string(), cover)).ok();
    }

    fn set_cover(&self, ui: &AppWindow, id: Option<String>) {
        let found = id.as_ref().and_then(|id| {
            let cover = self.cover.borrow();
            cover.as_ref().filter(|(cover_id, ..)| cover_id == id).map(|(_, image, colour)| (image.clone(), *colour))
        });
        if let (None, Some(id)) = (&found, &id) {
            self.want(id, true);
        }
        match found {
            Some((image, colour)) => self.show_cover(ui, id.clone(), image, Some(colour)),
            // The old cover stays while the new one downloads, rather than a blank between.
            None if id.is_some() => {}
            None => self.show_cover(ui, None, Image::default(), None),
        }
        *self.cover_wanted.borrow_mut() = id;
    }

    /// Puts `id`'s cover on screen, fading it in over the one there before.
    fn show_cover(&self, ui: &AppWindow, id: Option<String>, image: Image, colour: Option<slint::Color>) {
        if *self.cover_shown.borrow() == id {
            return;
        }
        ui.set_previous_cover(ui.get_cover());
        ui.set_cover(image);
        ui.set_cover_changed(true);
        ui.set_tinted(colour.is_some());
        if let Some(colour) = colour {
            ui.set_cover_colour(colour);
        }
        *self.cover_shown.borrow_mut() = id;
    }

    /// Takes in finished downloads and shows them in the rows (and the cover) waiting for them.
    fn deliver(&self, ui: &AppWindow, lists: &Lists) {
        let mut arrived: HashMap<String, Image> = HashMap::new();
        while let Ok((id, cover, thumbnail)) = self.done.try_recv() {
            let key = (id.clone(), cover);
            self.asked.borrow_mut().remove(&key);
            let Some(thumbnail) = thumbnail else {
                self.failed.borrow_mut().insert(key, Instant::now());
                continue;
            };
            let image = Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
                &thumbnail.rgba,
                thumbnail.width,
                thumbnail.height,
            ));
            if cover {
                let [r, g, b] = thumbnail.colour;
                let colour = slint::Color::from_rgb_u8(r, g, b);
                if self.cover_wanted.borrow().as_deref() == Some(id.as_str()) {
                    self.show_cover(ui, Some(id.clone()), image.clone(), Some(colour));
                }
                *self.cover.borrow_mut() = Some((id, image, colour));
            } else {
                let mut kept = self.kept.borrow_mut();
                kept.retain(|(kept_id, _)| *kept_id != id);
                kept.push_back((id.clone(), image.clone()));
                while kept.len() > THUMBS_KEPT {
                    kept.pop_front();
                }
                arrived.insert(id, image);
            }
        }
        if arrived.is_empty() {
            return;
        }
        let open = ui.get_library_open();
        for model in [&lists.library, &lists.pinned_lists] {
            for row in 0..model.row_count() {
                let Some(mut item) = model.row_data(row) else { continue };
                if let Some(image) = arrived.get(item.thumb_id.as_str()) {
                    // The opened playlist's cover, too.
                    if item.id == open {
                        ui.set_library_open_cover(image.clone());
                    }
                    item.thumb = image.clone();
                    model.set_row_data(row, item);
                }
            }
        }
        for model in [&lists.queue, &lists.history, &lists.results, &lists.pins, &lists.library_tracks] {
            for row in 0..model.row_count() {
                let Some(mut item) = model.row_data(row) else { continue };
                if let Some(image) = arrived.get(item.id.as_str()) {
                    item.thumb = image.clone();
                    model.set_row_data(row, item);
                }
            }
        }
    }
}

/// The pages signing in agrees to: Featherplay's privacy policy, YouTube's Terms of
/// Service and Google's Privacy Policy (YouTube's API rules ask apps to link all three).
fn policy_link(which: i32) -> String {
    match which {
        1 => "https://www.youtube.com/t/terms".into(),
        2 => "https://policies.google.com/privacy".into(),
        _ => format!("{}/blob/main/PRIVACY.md", env!("CARGO_PKG_REPOSITORY")),
    }
}

/// Opens `url` in the default browser.
fn open_in_browser(url: &str) -> bool {
    // Not `explorer <url>`: with a query in the link (`watch?v=`) it opens a folder instead.
    let mut command = if cfg!(windows) {
        let mut c = std::process::Command::new("rundll32");
        c.args(["url.dll,FileProtocolHandler", url]);
        c
    } else {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(url);
        c
    };
    match command.spawn() {
        Ok(_) => true,
        Err(e) => {
            log::write(format!("Couldn't open a link in the browser: {e}"));
            false
        }
    }
}

fn repeat_mode(repeat: Repeat) -> i32 {
    match repeat {
        Repeat::Off => 0,
        Repeat::All => 1,
        Repeat::One => 2,
    }
}

fn clock(seconds: f32) -> String {
    let total = seconds.max(0.0) as u64;
    let (h, m, s) = (total / 3600, total / 60 % 60, total % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

#[cfg(test)]
mod showcase;

/// The theme's colours, read from the interface itself, keep text readable: WCAG 2.1 AA.
#[cfg(test)]
mod contrast {
    use std::collections::HashMap;

    /// Each colour token, as (light, dark).
    fn tokens() -> HashMap<String, (String, String)> {
        let source = include_str!("../ui/app.slint");
        let start = source.find("export global Theme").expect("the theme");
        let block = &source[start..start + source[start..].find("\n}").expect("its end")];
        block
            .lines()
            .filter_map(|line| {
                let (name, value) = line.trim().strip_prefix("out property <color> ")?.split_once(':')?;
                let value = value.trim().trim_end_matches(';');
                let (light, dark) = match value.strip_prefix("light ? ") {
                    Some(pair) => pair.split_once(" : ")?,
                    None => (value, value),
                };
                Some((name.to_string(), (light.to_string(), dark.to_string())))
            })
            .collect()
    }

    fn luminance(hex: &str) -> f64 {
        let hex = hex.trim_start_matches('#');
        let channel = |i: usize| {
            let c = f64::from(u8::from_str_radix(&hex[i..i + 2], 16).unwrap()) / 255.0;
            if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * channel(0) + 0.7152 * channel(2) + 0.0722 * channel(4)
    }

    fn ratio(a: &str, b: &str) -> f64 {
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    #[test]
    fn theme_colours_meet_contrast() {
        let tokens = tokens();
        let get = |name: &str| tokens.get(name).unwrap_or_else(|| panic!("no colour {name}")).clone();
        // Text, at any size, on everything it's drawn on: 4.5 : 1.
        let texts = ["text", "muted", "faint", "accent-text"];
        let grounds = ["ground", "surface", "raised", "tint", "card", "hover"];
        let mut pairs: Vec<(&str, &str, f64)> =
            texts.iter().flat_map(|t| grounds.iter().map(move |g| (*t, *g, 4.5))).collect();
        // Icons and large text: 3 : 1. The play button's icon, and the accent as a control.
        pairs.extend([("on-accent", "accent", 3.0), ("accent", "ground", 3.0), ("accent", "surface", 3.0)]);
        let mut short = Vec::new();
        for (fg, bg, needed) in pairs {
            let ((fg_light, fg_dark), (bg_light, bg_dark)) = (get(fg), get(bg));
            for (theme, f, b) in [("light", fg_light, bg_light), ("dark", fg_dark, bg_dark)] {
                let r = ratio(&f, &b);
                if r < needed {
                    short.push(format!("{theme}: {fg} {f} on {bg} {b} is {r:.2}, needs {needed}"));
                }
            }
        }
        assert!(short.is_empty(), "{}", short.join("\n"));
    }

    #[test]
    fn the_contrast_formula_matches_wcag() {
        assert!((ratio("#000000", "#ffffff") - 21.0).abs() < 1e-9);
        assert!((ratio("#777777", "#ffffff") - 4.48).abs() < 0.01);
    }
}

#[cfg(test)]
mod ui_tests {
    use super::*;
    use slint::LogicalPosition;
    use slint::platform::{PointerEventButton, WindowEvent};

    fn row(title: &str) -> QueueRow {
        QueueRow { title: title.into(), meta: "Channel · 3:00".into(), ..Default::default() }
    }

    fn window(rows: usize) -> AppWindow {
        // The real renderer, with the real fonts, so text takes the room it does on screen.
        let options = i_slint_backend_testing::TestingBackendOptions {
            mock_time: false,
            threading: false,
            renderer_name: Some("software".into()),
        };
        // Once per process; the tests that follow reuse it.
        let _ = slint::platform::set_platform(Box::new(i_slint_backend_testing::TestingBackend::new(options)));
        let ui = AppWindow::new().unwrap();
        ui.window().set_size(slint::LogicalSize::new(480.0, 800.0));
        let titles: Vec<QueueRow> = (0..rows).map(|i| row(&format!("Track {i}"))).collect();
        ui.set_queue(ModelRc::new(VecModel::from(titles)));
        ui.show().unwrap();
        settle();
        ui
    }

    /// Saves what the window shows as a PNG (uncompressed: this is for looking at).
    #[allow(dead_code)]
    fn snapshot(ui: &AppWindow, path: &str) {
        let buffer = ui.window().take_snapshot().expect("a snapshot");
        let (width, height) = (buffer.width(), buffer.height());
        let mut raw = Vec::new();
        for row in buffer.as_slice().chunks(width as usize) {
            raw.push(0u8);
            for p in row {
                raw.extend_from_slice(&[p.r, p.g, p.b, p.a]);
            }
        }
        let crc = |data: &[u8]| {
            let mut c = 0xffff_ffffu32;
            for &b in data {
                c ^= u32::from(b);
                for _ in 0..8 {
                    c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
                }
            }
            !c
        };
        let chunk = |out: &mut Vec<u8>, kind: &[u8], data: &[u8]| {
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            let start = out.len();
            out.extend_from_slice(kind);
            out.extend_from_slice(data);
            let sum = crc(&out[start..]);
            out.extend_from_slice(&sum.to_be_bytes());
        };
        let mut zlib = vec![0x78, 0x01];
        let blocks: Vec<&[u8]> = raw.chunks(65_535).collect();
        for (i, block) in blocks.iter().enumerate() {
            zlib.push(u8::from(i + 1 == blocks.len()));
            let len = block.len() as u16;
            zlib.extend_from_slice(&len.to_le_bytes());
            zlib.extend_from_slice(&(!len).to_le_bytes());
            zlib.extend_from_slice(block);
        }
        let (mut a, mut b) = (1u32, 0u32);
        for &byte in &raw {
            a = (a + u32::from(byte)) % 65_521;
            b = (b + a) % 65_521;
        }
        zlib.extend_from_slice(&((b << 16) | a).to_be_bytes());
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut header = Vec::new();
        header.extend_from_slice(&width.to_be_bytes());
        header.extend_from_slice(&height.to_be_bytes());
        header.extend_from_slice(&[8, 6, 0, 0, 0]);
        chunk(&mut png, b"IHDR", &header);
        chunk(&mut png, b"IDAT", &zlib);
        chunk(&mut png, b"IEND", &[]);
        std::fs::write(path, png).unwrap();
    }

    /// Lets animations run out.
    fn settle() {
        for _ in 0..25 {
            tick();
        }
    }

    fn tick() {
        std::thread::sleep(Duration::from_millis(16));
        slint::platform::update_timers_and_animations();
    }

    fn send(ui: &AppWindow, event: WindowEvent) {
        ui.window().dispatch_event(event);
        tick();
    }

    fn pointer(ui: &AppWindow, x: f32, y: f32) {
        send(ui, WindowEvent::PointerMoved { position: LogicalPosition::new(x, y) });
    }

    fn press(ui: &AppWindow, x: f32, y: f32, down: bool) {
        let (position, button) = (LogicalPosition::new(x, y), PointerEventButton::Left);
        send(
            ui,
            if down {
                WindowEvent::PointerPressed { position, button }
            } else {
                WindowEvent::PointerReleased { position, button }
            },
        );
    }

    /// Where the queue's rows are: x, the top of the first and the row height.
    fn rows(ui: &AppWindow) -> (f32, f32, f32) {
        let rows =
            i_slint_backend_testing::ElementHandle::find_by_element_type_name(ui, "TrackRow").collect::<Vec<_>>();
        let first = rows.iter().min_by(|a, b| a.absolute_position().y.total_cmp(&b.absolute_position().y)).unwrap();
        (first.absolute_position().x + first.size().width, first.absolute_position().y, first.size().height)
    }

    #[test]
    fn a_rows_buttons_stay_while_the_pointer_moves_onto_them() {
        let ui = window(3);
        let removed = Rc::new(RefCell::new(Vec::new()));
        ui.on_remove_row({
            let removed = removed.clone();
            move |row| removed.borrow_mut().push(row)
        });
        let (right, top, height) = rows(&ui);
        let y = top + height * 1.5;
        // Across the row and onto its rightmost button (remove), as a hand would.
        for x in [100.0, 250.0, right - 40.0, right - 30.0, right - 24.0] {
            pointer(&ui, x, y);
            settle();
        }
        press(&ui, right - 24.0, y, true);
        press(&ui, right - 24.0, y, false);
        assert_eq!(*removed.borrow(), [1], "the remove button was there to be clicked");
    }

    #[test]
    fn the_scroll_rail_sits_beside_the_rows_and_jumps_where_pressed() {
        let ui = window(40);
        let played = Rc::new(RefCell::new(Vec::new()));
        ui.on_play_row({
            let played = played.clone();
            move |row| played.borrow_mut().push(row)
        });
        let (right, top, height) = rows(&ui);
        let rail = i_slint_backend_testing::ElementHandle::find_by_element_type_name(&ui, "ScrollRail").next().unwrap();
        let (at, size) = (rail.absolute_position(), rail.size());
        assert!(at.x >= right, "the rail has a lane of its own: rows end at {right}, rail starts at {}", at.x);

        // Press near the bottom of the rail: the list jumps towards its end.
        press(&ui, at.x + size.width / 2.0, at.y + size.height - 4.0, true);
        press(&ui, at.x + size.width / 2.0, at.y + size.height - 4.0, false);
        settle();
        press(&ui, 150.0, top + height / 2.0, true);
        press(&ui, 150.0, top + height / 2.0, false);
        let first = played.borrow()[0];
        assert!(first > 25, "the first row showing is now far down the list: {first}");
    }

    #[test]
    fn a_long_link_stays_inside_the_search_box() {
        let ui = window(0);
        let find = |id: &str| i_slint_backend_testing::ElementHandle::find_by_element_id(&ui, id).next().unwrap();
        let (input, box_) = (find("SearchBox::input"), find("AppWindow::search"));
        let edge = box_.absolute_position().x + box_.size().width;
        press(&ui, input.absolute_position().x + 20.0, input.absolute_position().y + input.size().height / 2.0, true);
        press(&ui, input.absolute_position().x + 20.0, input.absolute_position().y + input.size().height / 2.0, false);
        let link = "https://www.youtube.com/watch?v=C8eqFM6TJmg&list=RDC8eqFM6TJmg&start_radio=1&pp=oAcB";
        for c in link.chars() {
            send(&ui, WindowEvent::KeyPressed { text: c.to_string().into() });
            send(&ui, WindowEvent::KeyReleased { text: c.to_string().into() });
        }
        settle();
        let input = find("SearchBox::input");
        assert!(input.accessible_value().is_some_and(|v| v.len() == link.len()), "the link was typed");
        let right = input.absolute_position().x + input.size().width;
        assert!(right < edge - 40.0, "the text field ends before the add button: {right} vs box edge {edge}");
        assert_eq!(box_.size().width, find("AppWindow::search").size().width);
    }

    #[test]
    fn a_pinned_playlist_can_be_unpinned_from_its_corner() {
        let ui = window(0);
        let lists: Vec<LibraryRow> = ["Morning", "Focus", "Evening"]
            .iter()
            .enumerate()
            .map(|(i, t)| LibraryRow {
                id: format!("PL{i}").into(),
                title: (*t).into(),
                meta: "12 tracks".into(),
                pinned: true,
                ..Default::default()
            })
            .collect();
        ui.set_pinned_lists(ModelRc::new(VecModel::from(lists)));
        ui.set_tab(2);
        settle();
        let unpinned = Rc::new(RefCell::new(Vec::new()));
        ui.on_pin_list({
            let unpinned = unpinned.clone();
            move |id| unpinned.borrow_mut().push(id.to_string())
        });
        let button = |title: &str| {
            i_slint_backend_testing::ElementHandle::find_by_accessible_label(&ui, &format!("Unpin {title}"))
                .next()
                .unwrap_or_else(|| panic!("an Unpin button on {title}"))
        };
        for title in ["Morning", "Focus", "Evening"] {
            button(title);
        }
        // On the cover's top right corner.
        let tile =
            i_slint_backend_testing::ElementHandle::find_by_element_type_name(&ui, "PlaylistTile").nth(1).unwrap();
        let corner = button("Focus");
        let (t, c) = (tile.absolute_position(), corner.absolute_position());
        assert!(c.x > t.x + tile.size().width / 2.0 && c.y < t.y + 20.0, "top right: tile {t:?}, button {c:?}");
        if let Ok(dir) = std::env::var("FEATHERPLAY_SNAPSHOTS") {
            pointer(&ui, t.x + 40.0, t.y + 30.0);
            settle();
            snapshot(&ui, &format!("{dir}/unpin-hover.png"));
        }
        corner.invoke_accessible_default_action();
        assert_eq!(*unpinned.borrow(), vec!["PL1".to_string()], "Focus, and only Focus, is unpinned");
    }

    #[test]
    fn about_shows_the_version_and_slints_attribution() {
        let ui = window(0);
        ui.set_app_version(env!("CARGO_PKG_VERSION").into());
        ui.set_settings_open(true);
        settle();
        let sheet = || i_slint_backend_testing::ElementHandle::find_by_element_id(&ui, "AppWindow::about-sheet").next();
        // On screen: found (hidden elements aren't) and its top inside the 800-pixel window.
        let shown = || sheet().is_some_and(|s| s.absolute_position().y < 790.0);
        assert!(!shown(), "About stays closed until asked for");
        let button = i_slint_backend_testing::ElementHandle::find_by_accessible_label(&ui, "About")
            .next()
            .expect("an About button");
        let (at, size) = (button.absolute_position(), button.size());
        press(&ui, at.x + size.width / 2.0, at.y + size.height / 2.0, true);
        press(&ui, at.x + size.width / 2.0, at.y + size.height / 2.0, false);
        settle();
        assert!(shown(), "the About sheet, with Slint's attribution, is on screen");
        send(&ui, WindowEvent::KeyPressed { text: slint::platform::Key::Escape.into() });
        send(&ui, WindowEvent::KeyReleased { text: slint::platform::Key::Escape.into() });
        settle();
        assert!(!shown() && ui.get_settings_open(), "Esc closes About first, leaving the settings open");
    }

    #[test]
    fn enter_plays_a_link_now_and_shift_enter_adds_it() {
        let ui = window(0);
        ui.on_is_link(|text| extract::is_link(text.trim()));
        let submitted = Rc::new(RefCell::new(Vec::new()));
        ui.on_open_link({
            let submitted = submitted.clone();
            move |text, play_now| submitted.borrow_mut().push((text.to_string(), play_now))
        });
        let input = i_slint_backend_testing::ElementHandle::find_by_element_id(&ui, "SearchBox::input").next().unwrap();
        let (x, y) = (input.absolute_position().x + 20.0, input.absolute_position().y + input.size().height / 2.0);
        let typed = |text: &str| {
            press(&ui, x, y, true);
            press(&ui, x, y, false);
            for c in text.chars() {
                send(&ui, WindowEvent::KeyPressed { text: c.to_string().into() });
                send(&ui, WindowEvent::KeyReleased { text: c.to_string().into() });
            }
        };
        // The hint card, if it is showing: what its words say.
        let hint = || {
            i_slint_backend_testing::ElementHandle::find_by_element_id(&ui, "AppWindow::hint")
                .next()
                .filter(|h| h.computed_opacity() > 0.5)
                .and_then(|h| h.accessible_label())
                .map(|l| l.to_string())
        };
        assert_eq!(hint(), None, "no hint while the box is empty");
        let link = "https://youtu.be/djV11Xbc914";
        typed(link);
        settle();
        assert!(hint().is_some_and(|h| h.contains("play now") && h.contains("Shift+Enter")), "{:?}", hint());
        if let Ok(dir) = std::env::var("FEATHERPLAY_SNAPSHOTS") {
            snapshot(&ui, &format!("{dir}/hint.png"));
        }
        send(&ui, WindowEvent::KeyPressed { text: slint::platform::Key::Return.into() });
        send(&ui, WindowEvent::KeyReleased { text: slint::platform::Key::Return.into() });

        typed(link);
        let shift = slint::platform::Key::Shift;
        send(&ui, WindowEvent::KeyPressed { text: shift.into() });
        send(&ui, WindowEvent::KeyPressed { text: slint::platform::Key::Return.into() });
        send(&ui, WindowEvent::KeyReleased { text: slint::platform::Key::Return.into() });
        send(&ui, WindowEvent::KeyReleased { text: shift.into() });

        settle();
        assert_eq!(hint(), None, "gone once the box is empty again");
        typed("calm piano");
        settle();
        assert_eq!(hint(), None, "words to search need no hint");
        assert_eq!(*submitted.borrow(), [(link.to_string(), true), (link.to_string(), false)]);
    }

    #[test]
    fn tab_reaches_every_button_including_settings() {
        let ui = window(2);
        let key = |k: slint::platform::Key| {
            send(&ui, WindowEvent::KeyPressed { text: k.into() });
            send(&ui, WindowEvent::KeyReleased { text: k.into() });
        };
        // Tab on to the next control and press Enter, until Settings opens.
        let mut presses = 0;
        while !ui.get_settings_open() && presses < 40 {
            key(slint::platform::Key::Tab);
            key(slint::platform::Key::Return);
            presses += 1;
        }
        assert!(ui.get_settings_open(), "Settings was never reached by Tab within {presses} presses");
    }

    #[test]
    fn the_empty_queue_message_fits_a_short_window() {
        let ui = window(0);
        for height in [596.0, 640.0, 700.0] {
            ui.window().set_size(slint::LogicalSize::new(480.0, height));
            settle();
            if let Ok(dir) = std::env::var("FEATHERPLAY_SNAPSHOTS") {
                snapshot(&ui, &format!("{dir}/empty-{height}.png"));
            }
            let find =
                |label: &str| i_slint_backend_testing::ElementHandle::find_by_accessible_label(&ui, label).next();
            let settings = find("Settings").expect("the Settings button");
            let words: Vec<_> = ["Nothing queued", "Search, or paste a YouTube link. Ctrl+L gets you there."]
                .into_iter()
                .filter_map(find)
                .collect();
            assert!(!words.is_empty(), "the message shows at {height}");
            for text in words {
                let bottom = text.absolute_position().y + text.size().height;
                assert!(
                    bottom <= settings.absolute_position().y,
                    "at height {height}, text ends at {bottom}, over the footer at {}",
                    settings.absolute_position().y
                );
            }
        }
    }

    #[test]
    fn a_theme_change_wipes_diagonally() {
        let ui = window(3);
        ui.set_theme_mode(1);
        settle();
        // What the handler in main does: a picture of the old theme, then the new theme.
        let brightness = |ui: &AppWindow, x: u32, y: u32| {
            let frame = ui.window().take_snapshot().unwrap();
            let p = frame.as_slice()[(y * frame.width() + x) as usize];
            (u32::from(p.r) + u32::from(p.g) + u32::from(p.b)) / 3
        };
        let dark = brightness(&ui, 4, 60);
        ui.set_theme_snapshot(Image::from_rgba8(ui.window().take_snapshot().unwrap()));
        ui.set_wiping(true);
        ui.set_theme_mode(2);
        for _ in 0..20 {
            tick();
        }
        let size = ui.window().size();
        let (top_left, bottom_right) = (brightness(&ui, 4, 60), brightness(&ui, size.width - 4, size.height - 4));
        if let Ok(dir) = std::env::var("FEATHERPLAY_SNAPSHOTS") {
            snapshot(&ui, &format!("{dir}/wipe-to-light.png"));
        }
        assert!(top_left > dark + 100, "going light, the top left is light already: {top_left} vs dark {dark}");
        assert!(bottom_right < 100, "and the bottom right still dark: {bottom_right}");
        ui.set_wiping(false);
        settle();
        assert!(brightness(&ui, size.width - 4, size.height - 4) > 150, "afterwards, all of it is light");

        // And back: going dark, the new colours come from the bottom right.
        ui.set_theme_snapshot(Image::from_rgba8(ui.window().take_snapshot().unwrap()));
        ui.set_wiping(true);
        ui.set_theme_mode(1);
        for _ in 0..20 {
            tick();
        }
        if let Ok(dir) = std::env::var("FEATHERPLAY_SNAPSHOTS") {
            snapshot(&ui, &format!("{dir}/wipe-to-dark.png"));
        }
        assert!(brightness(&ui, size.width - 4, size.height - 4) < 100, "going dark, the bottom right is dark already");
        assert!(brightness(&ui, 4, 60) > 150, "and the top left still light");
    }

    #[test]
    fn a_new_cover_fades_in_over_the_old_one() {
        let ui = window(1);
        let picture =
            |shade: u8| Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&[shade; 4 * 16], 4, 4));
        ui.set_cover(picture(10));
        settle();
        let cover =
            || i_slint_backend_testing::ElementHandle::find_by_element_id(&ui, "AppWindow::cover-art").next().unwrap();
        assert_eq!(cover().computed_opacity(), 1.0);

        // What the thumbnails do once the next cover has arrived.
        ui.set_previous_cover(ui.get_cover());
        ui.set_cover(picture(200));
        ui.set_cover_changed(true);
        tick();
        assert!(cover().computed_opacity() < 0.2, "the new cover starts hidden, over the old one");
        for _ in 0..8 {
            tick();
        }
        let midway = cover().computed_opacity();
        assert!(midway > 0.0 && midway < 1.0, "and fades in rather than cutting: {midway}");
        settle();
        settle();
        assert_eq!(cover().computed_opacity(), 1.0);

        // The queue cleared: nothing to show, and the last cover fades away with it.
        let old = || i_slint_backend_testing::ElementHandle::find_by_element_id(&ui, "AppWindow::old-cover").next();
        ui.set_previous_cover(ui.get_cover());
        ui.set_cover(Image::default());
        ui.set_cover_changed(true);
        tick();
        assert!(old().is_some_and(|o| o.computed_opacity() > 0.8), "it starts where it was");
        settle();
        settle();
        assert!(old().is_none_or(|o| o.computed_opacity() == 0.0), "and is gone, not left behind");
    }

    #[test]
    fn dragging_a_row_moves_it_and_does_not_play_it() {
        let ui = window(5);
        let moves = Rc::new(RefCell::new(Vec::new()));
        let played = Rc::new(RefCell::new(Vec::new()));
        ui.on_move_row({
            let moves = moves.clone();
            move |from, to| moves.borrow_mut().push((from, to))
        });
        ui.on_play_row({
            let played = played.clone();
            move |row| played.borrow_mut().push(row)
        });
        let (_, top, height) = rows(&ui);
        let y = top + height / 2.0;
        pointer(&ui, 150.0, y);
        press(&ui, 150.0, y, true);
        for step in 1..=10 {
            pointer(&ui, 150.0, y + height * 2.0 * step as f32 / 10.0);
        }
        press(&ui, 150.0, y + height * 2.0, false);
        settle();
        assert_eq!(*moves.borrow(), [(0, 2)]);
        assert!(played.borrow().is_empty(), "a drag is not a click");

        // A plain click still plays.
        press(&ui, 150.0, y, true);
        press(&ui, 150.0, y, false);
        assert_eq!(*played.borrow(), [0]);
        assert_eq!(moves.borrow().len(), 1);
    }

    fn test_track(title: &str) -> Track {
        Track {
            url: format!("https://www.youtube.com/watch?v={:0<11}", title.replace(' ', "")),
            title: title.into(),
            duration: None,
            channel: String::new(),
            unavailable: false,
            failed: false,
        }
    }

    /// A window whose queue shows `titles`, wired to the real menu code: the menu opens
    /// for whatever `tracks` holds when it's asked for, and records what it acts on.
    type Shared<T> = Rc<RefCell<Vec<T>>>;

    fn menu_window(titles: &[&str]) -> (AppWindow, Shared<Track>, Shared<(Action, String)>) {
        let ui = window(0);
        let rows: Vec<QueueRow> = titles.iter().map(|t| row(t)).collect();
        ui.set_queue(ModelRc::new(VecModel::from(rows)));
        let tracks = Rc::new(RefCell::new(titles.iter().map(|t| test_track(t)).collect::<Vec<_>>()));
        let acted = Rc::new(RefCell::new(Vec::new()));
        let target: Rc<RefCell<Option<(menu::List, Track)>>> = Rc::default();
        ui.on_open_menu({
            let ui = ui.as_weak();
            let tracks = tracks.clone();
            let target = target.clone();
            move |list, row, x, y| {
                let ui = ui.upgrade().unwrap();
                let list = menu::List::from_index(list).unwrap();
                let facts = |_: &Track| Facts { queued: true, video: true, ..Facts::default() };
                open_menu(&ui, &target, list, &tracks.borrow(), row, facts, (x, y));
            }
        });
        ui.on_menu_chose({
            let acted = acted.clone();
            move |id| {
                if let Some((_, track)) = target.borrow_mut().take() {
                    acted.borrow_mut().push((Action::from_id(&id).unwrap(), track.title));
                }
            }
        });
        settle();
        (ui, tracks, acted)
    }

    fn right_click(ui: &AppWindow, x: f32, y: f32) {
        let (position, button) = (LogicalPosition::new(x, y), PointerEventButton::Right);
        pointer(ui, x, y);
        send(ui, WindowEvent::PointerPressed { position, button });
        send(ui, WindowEvent::PointerReleased { position, button });
        settle();
    }

    fn menu_labels(ui: &AppWindow) -> Vec<String> {
        ui.get_menu_items().iter().map(|i| i.label.to_string()).collect()
    }

    /// Clicks the menu item with this label, found on screen.
    fn click_item(ui: &AppWindow, label: &str) {
        let item = i_slint_backend_testing::ElementHandle::find_by_element_type_name(ui, "TrackMenu")
            .next()
            .expect("the menu is open");
        let index = menu_labels(ui).iter().position(|l| l == label).expect("the item is offered");
        let (x, y) = (item.absolute_position().x, item.absolute_position().y);
        // Items are 32 px from 5 px down, with 9 px for each line above them.
        let lines = ui.get_menu_items().iter().take(index + 1).filter(|i| i.separated).count() as f32;
        let at = (x + 40.0, y + 5.0 + index as f32 * 32.0 + lines * 9.0 + 16.0);
        pointer(ui, at.0, at.1);
        press(ui, at.0, at.1, true);
        press(ui, at.0, at.1, false);
        settle();
    }

    #[test]
    fn right_click_opens_the_track_menu() {
        let (ui, _, acted) = menu_window(&["One", "Two", "Three"]);
        let (x, top, height) = rows(&ui);
        right_click(&ui, x - 200.0, top + height * 1.5);
        assert!(ui.get_menu_open());
        assert_eq!(
            menu_labels(&ui),
            ["Play now", "Play next", "Start radio", "Pin", "Remove from queue", "Copy link", "Open on YouTube"]
        );
        if let Ok(dir) = std::env::var("FEATHERPLAY_SNAPSHOTS") {
            ui.set_theme_mode(1);
            settle();
            snapshot(&ui, &format!("{dir}/menu-queue-dark.png"));
            ui.set_theme_mode(2);
            settle();
            snapshot(&ui, &format!("{dir}/menu-queue-light.png"));
        }
        // A press outside closes it without acting.
        press(&ui, 20.0, 20.0, true);
        press(&ui, 20.0, 20.0, false);
        settle();
        assert!(!ui.get_menu_open());
        assert!(acted.borrow().is_empty());
    }

    #[test]
    fn right_click_offers_play_next() {
        let (ui, _, acted) = menu_window(&["One", "Two", "Three"]);
        let (x, top, height) = rows(&ui);
        right_click(&ui, x - 200.0, top + height * 2.5);
        click_item(&ui, "Play next");
        assert!(!ui.get_menu_open());
        assert_eq!(*acted.borrow(), [(Action::PlayNext, "Three".to_string())]);
    }

    #[test]
    fn menu_acts_on_the_clicked_row() {
        let (ui, tracks, acted) = menu_window(&["One", "Two", "Three"]);
        let (x, top, height) = rows(&ui);
        right_click(&ui, x - 200.0, top + height * 0.5);
        // The list changes under the open menu: "One" moves to the end.
        tracks.borrow_mut().rotate_left(1);
        ui.set_queue(ModelRc::new(VecModel::from(vec![row("Two"), row("Three"), row("One")])));
        settle();
        click_item(&ui, "Play now");
        assert_eq!(*acted.borrow(), [(Action::PlayNow, "One".to_string())]);
    }

    #[test]
    fn menu_key_opens_it_for_the_focused_row() {
        let (ui, _, acted) = menu_window(&["One", "Two", "Three"]);
        let key = |k: slint::platform::Key| {
            send(&ui, WindowEvent::KeyPressed { text: k.into() });
            send(&ui, WindowEvent::KeyReleased { text: k.into() });
        };
        // Tab on, pressing the menu key at each stop, until the queue has the focus.
        let mut presses = 0;
        while !ui.get_menu_open() && presses < 40 {
            key(slint::platform::Key::Tab);
            key(slint::platform::Key::Menu);
            presses += 1;
        }
        assert!(ui.get_menu_open(), "the menu key opened no menu within {presses} Tab presses");
        // Esc closes it; Down moves to the second row; Shift+F10 opens its menu.
        key(slint::platform::Key::Escape);
        settle();
        assert!(!ui.get_menu_open());
        key(slint::platform::Key::DownArrow);
        send(&ui, WindowEvent::KeyPressed { text: slint::platform::Key::Shift.into() });
        key(slint::platform::Key::F10);
        send(&ui, WindowEvent::KeyReleased { text: slint::platform::Key::Shift.into() });
        settle();
        assert!(ui.get_menu_open(), "Shift+F10 opens it too");
        // The arrows and Enter choose an item: the second is Play next.
        key(slint::platform::Key::DownArrow);
        key(slint::platform::Key::DownArrow);
        key(slint::platform::Key::Return);
        settle();
        assert_eq!(*acted.borrow(), [(Action::PlayNext, "Two".to_string())]);
    }

    #[test]
    fn tab_into_a_scrolled_list_starts_at_the_rows_showing() {
        let ui = window(40);
        let played = Rc::new(RefCell::new(Vec::new()));
        ui.on_play_row({
            let played = played.clone();
            move |row| played.borrow_mut().push(row)
        });
        let key = |k: slint::platform::Key| {
            send(&ui, WindowEvent::KeyPressed { text: k.into() });
            send(&ui, WindowEvent::KeyReleased { text: k.into() });
        };
        // Scroll well down the queue with the wheel.
        let (_, top, height) = rows(&ui);
        let position = LogicalPosition::new(150.0, top + height);
        for _ in 0..10 {
            send(&ui, WindowEvent::PointerScrolled { position, delta_x: 0.0, delta_y: -120.0 });
        }
        settle();
        let (_, top, height) = rows(&ui);
        // Click the first row fully showing, to learn its index.
        let row_top = top + height;
        press(&ui, 150.0, row_top + height / 2.0, true);
        press(&ui, 150.0, row_top + height / 2.0, false);
        let showing = played.borrow_mut().remove(0);
        assert!(showing > 5, "scrolled down: {showing}");
        // Tab until the queue has the focus and the menu key opens a row's menu.
        let menus = Rc::new(RefCell::new(Vec::new()));
        ui.on_open_menu({
            let menus = menus.clone();
            move |list, row, _, _| menus.borrow_mut().push((list, row))
        });
        let mut presses = 0;
        while menus.borrow().is_empty() && presses < 40 {
            key(slint::platform::Key::Tab);
            key(slint::platform::Key::Menu);
            presses += 1;
        }
        let (list, first) = menus.borrow()[0];
        assert_eq!(list, 0);
        assert!(
            (showing - 1..=showing).contains(&first),
            "Tab lands on a row on screen ({showing} or the one above), not row {first}"
        );
    }

    /// The time shown at the right under the waveform.
    fn total_time(ui: &AppWindow) -> (String, i_slint_backend_testing::ElementHandle) {
        let element = i_slint_backend_testing::ElementHandle::find_by_element_id(ui, "AppWindow::total-time")
            .next()
            .expect("the time under the waveform");
        let text = element.accessible_value().or_else(|| element.accessible_label()).unwrap_or_default();
        (text.to_string(), element)
    }

    #[test]
    fn remaining_time_formats() {
        let ui = window(1);
        ui.set_has_track(true);
        ui.set_show_remaining(true);
        for (duration, position, shown) in [
            (241.0, 98.0, "−2:23"),
            (241.0, 0.0, "−4:01"),
            (241.0, 240.4, "−0:01"),
            (241.0, 241.0, "−0:00"),
            (45.0, 3.5, "−0:42"),
            (3725.0, 0.0, "−1:02:05"),
            // A position past the end (a duration a little short) never goes negative.
            (241.0, 250.0, "−0:00"),
        ] {
            ui.set_duration(duration);
            ui.set_position(position);
            tick();
            assert_eq!(total_time(&ui).0, shown, "{position} of {duration}");
        }
        ui.set_show_remaining(false);
        ui.set_duration(3725.0);
        tick();
        assert_eq!(total_time(&ui).0, "1:02:05");
    }

    #[test]
    fn clicking_the_duration_toggles_remaining() {
        let ui = window(1);
        let saved = Rc::new(RefCell::new(0));
        ui.on_settings_changed({
            let saved = saved.clone();
            move || *saved.borrow_mut() += 1
        });
        ui.set_has_track(true);
        ui.set_duration(241.0);
        ui.set_position(98.0);
        tick();
        let (shown, element) = total_time(&ui);
        assert_eq!(shown, "4:01");
        let (at, size) = (element.absolute_position(), element.size());
        let (x, y) = (at.x + size.width / 2.0, at.y + size.height / 2.0);
        press(&ui, x, y, true);
        press(&ui, x, y, false);
        assert!(ui.get_show_remaining());
        assert_eq!(total_time(&ui).0, "−2:23");
        assert_eq!(*saved.borrow(), 1, "the choice is saved");
        press(&ui, x, y, true);
        press(&ui, x, y, false);
        assert!(!ui.get_show_remaining());
        assert_eq!(total_time(&ui).0, "4:01");
        // A click a little beside the text counts too: the target is larger than it.
        let (x, y) = (at.x - 10.0, at.y + size.height + 4.0);
        press(&ui, x, y, true);
        press(&ui, x, y, false);
        assert!(ui.get_show_remaining(), "a click just off the text");
    }

    #[test]
    fn messages_fit_at_minimum_width() {
        let ui = window(3);
        ui.window().set_size(slint::LogicalSize::new(380.0, 700.0));
        settle();
        let toast = || {
            let text = i_slint_backend_testing::ElementHandle::find_by_element_id(&ui, "AppWindow::toast-text")
                .next()
                .expect("the message");
            (text.size(), text.absolute_position())
        };
        ui.set_status(say::link_copied().into());
        settle();
        let line = toast().0.height;
        let title =
            "An extremely long video title, of the kind YouTube allows, with every word you can think of in it…!!";
        assert_eq!(title.chars().count(), 100);
        for message in say::every(title) {
            ui.set_status(message.clone().into());
            tick();
            tick();
            let (size, at) = toast();
            if let Ok(dir) = std::env::var("FEATHERPLAY_SNAPSHOTS")
                && message.starts_with("Couldn't load")
            {
                snapshot(&ui, &format!("{dir}/message-narrow.png"));
            }
            assert!(size.height <= line * 2.0 + 1.0, "two lines at most: “{message}” takes {}", size.height);
            assert!(at.x >= 20.0 && at.x + size.width <= 360.0, "inside the window: “{message}”");
        }
    }

    fn card_height(ui: &AppWindow) -> f32 {
        i_slint_backend_testing::ElementHandle::find_by_element_id(ui, "AppWindow::now-card")
            .next()
            .expect("the now-playing card")
            .size()
            .height
    }

    /// Turns the mouse wheel over the list by `pixels` (positive: down the list).
    fn wheel(ui: &AppWindow, pixels: f32) {
        let (_, top, height) = rows(ui);
        let position = LogicalPosition::new(150.0, top.max(560.0) + height);
        send(ui, WindowEvent::PointerScrolled { position, delta_x: 0.0, delta_y: -pixels });
    }

    #[test]
    fn scrolling_a_list_slims_the_card() {
        let ui = window(40);
        assert_eq!(card_height(&ui), 120.0);
        let list_top = i_slint_backend_testing::ElementHandle::find_by_element_type_name(&ui, "TrackList")
            .next()
            .unwrap()
            .absolute_position()
            .y;
        for _ in 0..10 {
            wheel(&ui, 20.0);
        }
        settle();
        assert!(ui.get_slim());
        assert_eq!(card_height(&ui), 56.0, "the slim header");
        let list = i_slint_backend_testing::ElementHandle::find_by_element_type_name(&ui, "TrackList").next().unwrap();
        // The list's top moved up with the card: 64 px more of it shows.
        assert!(list.absolute_position().y < list_top - 60.0, "the list starts higher");
        if let Ok(dir) = std::env::var("FEATHERPLAY_SNAPSHOTS") {
            snapshot(&ui, &format!("{dir}/slim-header.png"));
        }
    }

    #[test]
    fn scrolling_back_restores_it() {
        let ui = window(40);
        for _ in 0..10 {
            wheel(&ui, 20.0);
        }
        settle();
        assert!(ui.get_slim());
        for _ in 0..30 {
            wheel(&ui, -20.0);
        }
        settle();
        assert!(!ui.get_slim());
        assert_eq!(card_height(&ui), 120.0);

        // A click on the slim header brings the full card back too, as does another tab.
        for _ in 0..10 {
            wheel(&ui, 20.0);
        }
        settle();
        assert!(ui.get_slim());
        let card =
            i_slint_backend_testing::ElementHandle::find_by_element_id(&ui, "AppWindow::now-card").next().unwrap();
        let (x, y) = (card.absolute_position().x + 200.0, card.absolute_position().y + 28.0);
        press(&ui, x, y, true);
        press(&ui, x, y, false);
        settle();
        assert!(!ui.get_slim());
        assert_eq!(card_height(&ui), 120.0);
    }

    #[test]
    fn no_flicker_near_the_threshold() {
        let ui = window(40);
        let mut changes = 0;
        let mut was = ui.get_slim();
        // Down past the line, then small scrolls back and forth around it.
        for step in 0..60 {
            wheel(
                &ui,
                if step < 16 {
                    10.0
                } else if step % 2 == 0 {
                    -10.0
                } else {
                    10.0
                },
            );
            tick();
            if ui.get_slim() != was {
                changes += 1;
                was = ui.get_slim();
            }
        }
        assert!(ui.get_slim());
        assert_eq!(changes, 1, "it slimmed once and stayed");

        // A list with little to scroll never slims, so its end can't fight the header.
        let ui = window(5);
        for _ in 0..20 {
            wheel(&ui, 20.0);
        }
        settle();
        assert!(!ui.get_slim());
    }

    /// How long frames take to draw while the card changes size, with a cover on it.
    #[test]
    #[ignore = "a measurement: cargo test --release -- --ignored slim_frames --nocapture"]
    fn slim_frames() {
        let ui = window(40);
        // The size to measure at, as WIDTHxHEIGHT (default: the tests' 480x800).
        if let Some((w, h)) = std::env::var("FEATHERPLAY_SIZE").ok().and_then(|s| {
            let (w, h) = s.split_once('x')?;
            Some((w.parse().ok()?, h.parse().ok()?))
        }) {
            ui.window().set_size(slint::LogicalSize::new(w, h));
        }
        ui.set_has_track(true);
        ui.set_track_title("A long enough title to wrap onto a second line here".into());
        ui.set_track_channel("Channel".into());
        ui.set_up_next("Something else".into());
        let pixels = SharedPixelBuffer::<Rgba8Pixel>::new(320, 180);
        ui.set_cover(Image::from_rgba8(pixels));
        settle();
        if let Ok(dir) = std::env::var("FEATHERPLAY_SNAPSHOTS") {
            ui.set_slim(true);
            let start = Instant::now();
            for at in [0, 60, 110, 160, 300] {
                while start.elapsed() < Duration::from_millis(at) {
                    slint::platform::update_timers_and_animations();
                }
                snapshot(&ui, &format!("{dir}/slim-{at:03}.png"));
            }
            ui.set_slim(false);
            settle();
        }
        let mut times = Vec::new();
        for round in 0..6 {
            ui.set_slim(round % 2 == 0);
            let start = Instant::now();
            while start.elapsed() < Duration::from_millis(320) {
                slint::platform::update_timers_and_animations();
                let at = Instant::now();
                ui.window().take_snapshot().unwrap();
                times.push(at.elapsed().as_secs_f64() * 1000.0);
            }
        }
        times.sort_by(f64::total_cmp);
        let mean = times.iter().sum::<f64>() / times.len() as f64;
        println!(
            "frames {}: mean {mean:.2} ms, p90 {:.2} ms, max {:.2} ms",
            times.len(),
            times[times.len() * 9 / 10],
            times[times.len() - 1]
        );
    }

    #[test]
    fn rows_stay_still_while_the_card_slims() {
        let ui = window(40);
        for _ in 0..6 {
            wheel(&ui, 20.0);
        }
        settle();
        assert!(!ui.get_slim(), "not yet: 120 px down");
        let tops = |ui: &AppWindow| -> Vec<f32> {
            i_slint_backend_testing::ElementHandle::find_by_element_type_name(ui, "TrackRow")
                .map(|r| r.absolute_position().y)
                .collect()
        };
        let before = tops(&ui);
        // The wheel that crosses the line: from here on only the card moves.
        wheel(&ui, 20.0);
        let reference: Vec<f32> = tops(&ui);
        assert!(ui.get_slim());
        let mut frames = 0;
        let start = Instant::now();
        while start.elapsed() < Duration::from_millis(400) {
            tick();
            let now = tops(&ui);
            for y in reference.iter().filter(|y| **y > 560.0) {
                assert!(now.iter().any(|n| (n - y).abs() < 1.0), "a row at {y} moved (frame {frames}): {now:?}");
            }
            frames += 1;
        }
        assert_eq!(card_height(&ui), 56.0);
        assert!(frames > 10);
        assert_ne!(before, reference, "the wheel did scroll");
    }

    /// Pictures of messages, for looking at: FEATHERPLAY_SNAPSHOTS=<dir>.
    #[test]
    #[ignore = "pictures: FEATHERPLAY_SNAPSHOTS=<dir> cargo test -- --ignored message_examples"]
    fn message_examples() {
        let Ok(dir) = std::env::var("FEATHERPLAY_SNAPSHOTS") else { return };
        let ui = window(3);
        ui.set_theme_mode(1);
        let title = "Madoka Magica - Sis Puella Magica! Vocal Cover by eili";
        let examples = [
            say::playing(title),
            say::added(title),
            say::playing_list("Mix - Apocalyptica - Beyond The Stars (Audio)", 50),
            say::added_list("Apocalyptica - Cell-0 (Full Album)", 9),
            say::already_queued(title),
            say::couldnt_load(title),
            say::skipped(title, "private"),
            say::radio("Apocalyptica - Beyond The Stars (Audio)", 50),
            say::ytdlp_rescued(),
        ];
        for (width, tag) in [(480.0, "wide"), (380.0, "narrow")] {
            ui.window().set_size(slint::LogicalSize::new(width, 700.0));
            for (i, message) in examples.iter().enumerate() {
                ui.set_status(message.clone().into());
                settle();
                snapshot(&ui, &format!("{dir}/msg-{tag}-{i}.png"));
            }
        }
    }

    #[test]
    fn settings_shows_account_states() {
        let ui = window(3);
        ui.set_google_label(GOOGLE_LABEL.into());
        ui.set_settings_open(true);
        let texts = |ui: &AppWindow| -> Vec<String> {
            i_slint_backend_testing::ElementHandle::find_by_element_type_name(ui, "Text")
                .filter_map(|t| t.accessible_label())
                .map(|t| t.to_string())
                .collect()
        };
        let buttons = |ui: &AppWindow| -> Vec<String> {
            i_slint_backend_testing::ElementHandle::find_by_accessible_label(ui, "Sign in with Google")
                .map(|b| b.accessible_label().unwrap_or_default().to_string())
                .collect()
        };
        // Where sign-in isn't possible, nothing about it shows.
        settle();
        assert!(!texts(&ui).iter().any(|t| t.contains("YouTube account")));

        ui.set_account_state(1);
        settle();
        assert!(texts(&ui).iter().any(|t| t == "Not signed in"));
        assert_eq!(buttons(&ui).len(), 1, "Google's button");
        assert!(texts(&ui).iter().any(|t| t.contains("can only view")));
        // The pages signing in agrees to, each opening its own link.
        let opened = Rc::new(RefCell::new(Vec::new()));
        ui.on_open_policy({
            let opened = opened.clone();
            move |which| opened.borrow_mut().push(super::policy_link(which))
        });
        for label in ["Privacy policy", "YouTube Terms", "Google Privacy Policy"] {
            let link = i_slint_backend_testing::ElementHandle::find_by_accessible_label(&ui, label)
                .next()
                .unwrap_or_else(|| panic!("a {label} link"));
            let (at, size) = (link.absolute_position(), link.size());
            press(&ui, at.x + size.width / 2.0, at.y + size.height / 2.0, true);
            press(&ui, at.x + size.width / 2.0, at.y + size.height / 2.0, false);
        }
        assert_eq!(
            *opened.borrow(),
            [
                "https://github.com/Hamza-ELAKLY/featherplay/blob/main/PRIVACY.md",
                "https://www.youtube.com/t/terms",
                "https://policies.google.com/privacy"
            ]
        );
        let signed_out = ui.window().take_snapshot().ok();

        ui.set_account_state(2);
        settle();
        assert!(texts(&ui).iter().any(|t| t.contains("Finish signing in in your browser")));
        assert!(buttons(&ui).is_empty());

        ui.set_account_state(3);
        ui.set_account_name("Test Channel".into());
        settle();
        let shown = texts(&ui);
        assert!(shown.iter().any(|t| t == "Test Channel") && shown.iter().any(|t| t.contains("view only")));

        ui.set_account_state(4);
        settle();
        assert!(texts(&ui).iter().any(|t| t == "Signed in · no YouTube channel"));

        if let Ok(dir) = std::env::var("FEATHERPLAY_SNAPSHOTS") {
            for (theme, name) in [(2, "light"), (1, "dark")] {
                ui.set_theme_mode(theme);
                for (state, label) in [(1, "signed-out"), (2, "waiting"), (3, "signed-in")] {
                    ui.set_account_state(state);
                    settle();
                    snapshot(&ui, &format!("{dir}/account-{label}-{name}.png"));
                }
            }
        }
        drop(signed_out);
    }

    fn library_rows() -> Vec<LibraryRow> {
        let mut rows = vec![LibraryRow {
            id: "LL".into(),
            title: "Liked videos".into(),
            meta: "412 tracks".into(),
            liked: true,
            ..Default::default()
        }];
        for (i, (title, meta)) in [
            ("Late night", "Private · 12 tracks"),
            ("Road trip", "Unlisted · 3 tracks"),
            ("Everything", "Public · 120 tracks"),
        ]
        .into_iter()
        .chain((0..12).map(|i| ("More music", if i % 2 == 0 { "Private · 8 tracks" } else { "Public · 30 tracks" })))
        .enumerate()
        {
            rows.push(LibraryRow {
                id: format!("PL{i}").into(),
                title: title.into(),
                meta: meta.into(),
                ..Default::default()
            });
        }
        rows
    }

    #[test]
    fn library_tab_signed_out_offers_sign_in() {
        let ui = window(3);
        ui.set_google_label(GOOGLE_LABEL.into());
        let shown = Rc::new(RefCell::new(0));
        ui.on_library_shown({
            let shown = shown.clone();
            move || *shown.borrow_mut() += 1
        });
        let signed_in = Rc::new(RefCell::new(0));
        ui.on_sign_in({
            let signed_in = signed_in.clone();
            move || *signed_in.borrow_mut() += 1
        });
        ui.set_account_state(1);
        ui.set_tab(4);
        settle();
        assert_eq!(*shown.borrow(), 1, "the app hears the tab opened");
        let button = i_slint_backend_testing::ElementHandle::find_by_accessible_label(&ui, "Sign in with Google")
            .next()
            .expect("Google's button in the tab");
        let (at, size) = (button.absolute_position(), button.size());
        press(&ui, at.x + size.width / 2.0, at.y + size.height / 2.0, true);
        press(&ui, at.x + size.width / 2.0, at.y + size.height / 2.0, false);
        assert_eq!(*signed_in.borrow(), 1);
        if let Ok(dir) = std::env::var("FEATHERPLAY_SNAPSHOTS") {
            snapshot(&ui, &format!("{dir}/library-signed-out.png"));
        }
    }

    #[test]
    fn library_tab_signed_in_lists_and_opens_a_playlist() {
        let ui = window(3);
        ui.set_account_state(3);
        ui.set_account_name("Test Channel".into());
        ui.set_library_updated("Updated 3 min ago".into());
        ui.set_library(ModelRc::new(VecModel::from(library_rows())));
        let opened = Rc::new(RefCell::new(Vec::new()));
        ui.on_library_open_list({
            let (ui, opened) = (ui.as_weak(), opened.clone());
            move |id| {
                opened.borrow_mut().push(id.to_string());
                let ui = ui.upgrade().unwrap();
                ui.set_library_open(id);
                ui.set_library_title("Late night".into());
                ui.set_library_meta("Private · 12 tracks".into());
                let rows: Vec<QueueRow> = (0..12).map(|i| row(&format!("Night track {i}"))).collect();
                ui.set_library_tracks(ModelRc::new(VecModel::from(rows)));
            }
        });
        let played = Rc::new(RefCell::new(Vec::new()));
        ui.on_library_play({
            let played = played.clone();
            move |id, shuffle| played.borrow_mut().push((id.to_string(), shuffle))
        });
        ui.set_tab(4);
        settle();
        if let Ok(dir) = std::env::var("FEATHERPLAY_SNAPSHOTS") {
            snapshot(&ui, &format!("{dir}/library-signed-in.png"));
        }
        let playlist_rows = || -> Vec<i_slint_backend_testing::ElementHandle> {
            let mut rows: Vec<_> =
                i_slint_backend_testing::ElementHandle::find_by_element_type_name(&ui, "PlaylistRow").collect();
            rows.sort_by(|a, b| a.absolute_position().y.total_cmp(&b.absolute_position().y));
            rows.retain(|r| r.size().height > 0.0);
            rows
        };
        // Scroll the Library down a little, then open the row at the top.
        let first = playlist_rows()[0].absolute_position();
        let position = LogicalPosition::new(150.0, first.y + 30.0);
        for _ in 0..4 {
            send(&ui, WindowEvent::PointerScrolled { position, delta_x: 0.0, delta_y: -30.0 });
        }
        settle();
        let before: Vec<f32> = playlist_rows().iter().map(|r| r.absolute_position().y).collect();
        let target = playlist_rows().into_iter().find(|r| r.absolute_position().y > first.y).unwrap();
        let at = target.absolute_position();
        press(&ui, at.x + 150.0, at.y + 30.0, true);
        press(&ui, at.x + 150.0, at.y + 30.0, false);
        settle();
        assert_eq!(opened.borrow().len(), 1);
        assert_eq!(ui.get_library_open().as_str(), opened.borrow()[0]);
        if let Ok(dir) = std::env::var("FEATHERPLAY_SNAPSHOTS") {
            snapshot(&ui, &format!("{dir}/library-opened.png"));
        }
        // Play all from the opened list's header.
        let play_all = i_slint_backend_testing::ElementHandle::find_by_element_type_name(&ui, "Pill")
            .find(|b| b.accessible_label().is_some_and(|l| l == "Play all"))
            .expect("Play all");
        let (at, size) = (play_all.absolute_position(), play_all.size());
        press(&ui, at.x + size.width / 2.0, at.y + size.height / 2.0, true);
        press(&ui, at.x + size.width / 2.0, at.y + size.height / 2.0, false);
        assert_eq!(*played.borrow(), [(opened.borrow()[0].clone(), false)]);
        // Esc goes back, to the Library where it was.
        send(&ui, WindowEvent::KeyPressed { text: slint::platform::Key::Escape.into() });
        send(&ui, WindowEvent::KeyReleased { text: slint::platform::Key::Escape.into() });
        settle();
        assert_eq!(ui.get_library_open(), "");
        let after: Vec<f32> = playlist_rows().iter().map(|r| r.absolute_position().y).collect();
        assert_eq!(before, after, "the Library kept its scroll position");
    }

    #[test]
    fn private_video_rows_are_marked() {
        let track = |title: &str, unavailable: bool| Track {
            url: "https://www.youtube.com/watch?v=aaaaaaaaaaa".into(),
            title: title.into(),
            duration: None,
            channel: String::new(),
            unavailable,
            failed: false,
        };
        let tracks = [track("Song", false), track("Private video", true), track("Deleted video", true)];
        let shown = super::rows(&tracks, None, &HashSet::new(), &Thumbs::new());
        assert!(!shown[0].unavailable);
        assert!(shown[1].unavailable && shown[2].unavailable, "greyed");
        assert_eq!(shown[1].title, "Private video");
        assert_eq!(shown[1].meta, "Can't be played · skipped");
    }

    #[test]
    fn pinned_playlist_opens() {
        let ui = window(2);
        let pinned = vec![
            LibraryRow {
                id: "PLnight".into(),
                title: "Late night".into(),
                meta: "Private · 12 tracks".into(),
                pinned: true,
                ..Default::default()
            },
            LibraryRow {
                id: "LL".into(),
                title: "Liked videos".into(),
                meta: "Private".into(),
                liked: true,
                pinned: true,
                ..Default::default()
            },
        ];
        ui.set_pinned_lists(ModelRc::new(VecModel::from(pinned)));
        ui.set_pins(ModelRc::new(VecModel::from(vec![row("Pinned track")])));
        let opened = Rc::new(RefCell::new(Vec::new()));
        ui.on_open_pinned_list({
            let opened = opened.clone();
            move |id| opened.borrow_mut().push(id.to_string())
        });
        let played = Rc::new(RefCell::new(Vec::new()));
        ui.on_library_play({
            let played = played.clone();
            move |id, shuffle| played.borrow_mut().push((id.to_string(), shuffle))
        });
        ui.set_tab(2);
        settle();
        if let Ok(dir) = std::env::var("FEATHERPLAY_SNAPSHOTS") {
            snapshot(&ui, &format!("{dir}/pinned-playlists.png"));
        }
        let mut tiles: Vec<_> =
            i_slint_backend_testing::ElementHandle::find_by_element_type_name(&ui, "PlaylistTile").collect();
        tiles.sort_by(|a, b| a.absolute_position().x.total_cmp(&b.absolute_position().x));
        assert_eq!(tiles.len(), 2, "pinned playlists, side by side");
        let track = i_slint_backend_testing::ElementHandle::find_by_element_type_name(&ui, "TrackRow").next().unwrap();
        assert!(track.absolute_position().y > tiles[0].absolute_position().y + 90.0, "above the pinned tracks");
        // Clicking a cover opens it.
        let at = tiles[0].absolute_position();
        press(&ui, at.x + 40.0, at.y + 20.0, true);
        press(&ui, at.x + 40.0, at.y + 20.0, false);
        assert_eq!(*opened.borrow(), ["PLnight"]);
        // Its play button (in the cover's corner, showing on hover) plays it.
        let at = tiles[1].absolute_position();
        let (x, y) = (at.x + 112.0 - 6.0 - 14.0, at.y + 63.0 - 6.0 - 14.0);
        pointer(&ui, x, y);
        settle();
        press(&ui, x, y, true);
        press(&ui, x, y, false);
        assert_eq!(*played.borrow(), [("LL".to_string(), false)]);
        assert_eq!(opened.borrow().len(), 1, "the play button doesn't open it");
    }

    #[test]
    fn many_pinned_playlists_leave_room_for_pinned_tracks() {
        let ui = window(2);
        let lists: Vec<LibraryRow> = (0..8)
            .map(|i| LibraryRow {
                id: format!("PL{i}").into(),
                title: format!("List {i}").into(),
                meta: "Private · 6 tracks".into(),
                pinned: true,
                ..Default::default()
            })
            .collect();
        ui.set_pinned_lists(ModelRc::new(VecModel::from(lists)));
        ui.set_pins(ModelRc::new(VecModel::from((0..6).map(|i| row(&format!("Track {i}"))).collect::<Vec<_>>())));
        ui.set_tab(2);
        settle();
        if let Ok(dir) = std::env::var("FEATHERPLAY_SNAPSHOTS") {
            snapshot(&ui, &format!("{dir}/pinned-many-playlists.png"));
        }
        let visible_tracks = |ui: &AppWindow| {
            let bottom = ui.window().size().height as f32 / ui.window().scale_factor();
            i_slint_backend_testing::ElementHandle::find_by_element_type_name(ui, "TrackRow")
                .filter(|t| t.absolute_position().y + 60.0 < bottom - 40.0)
                .count()
        };
        // The wheel slides the covers sideways.
        let first = |ui: &AppWindow| {
            i_slint_backend_testing::ElementHandle::find_by_element_type_name(ui, "PlaylistTile")
                .map(|t| t.absolute_position())
                .min_by(|a, b| a.x.total_cmp(&b.x))
                .unwrap()
        };
        let start = first(&ui);
        let position = LogicalPosition::new(start.x + 50.0, start.y + 30.0);
        send(&ui, WindowEvent::PointerScrolled { position, delta_x: 0.0, delta_y: -120.0 });
        std::thread::sleep(std::time::Duration::from_millis(300));
        settle();
        assert!(first(&ui).x < start.x - 100.0, "slid left: {} then {}", start.x, first(&ui).x);
        let shown = visible_tracks(&ui);
        assert!(shown >= 1, "pinned tracks stay in view under eight playlists, saw {shown}");
        // Folding the playlists gives the tracks their room.
        assert!(ui.get_pinned_lists_open());
        ui.set_pinned_lists_open(false);
        settle();
        assert_eq!(i_slint_backend_testing::ElementHandle::find_by_element_type_name(&ui, "PlaylistTile").count(), 0);
        assert!(visible_tracks(&ui) > shown);
    }
}
