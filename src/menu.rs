//! The right-click menu on a track: which actions it offers, for which list.
//!
//! Pure rules, unit-tested; the window draws whatever `items` returns.

/// Where the track was right-clicked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum List {
    Queue,
    History,
    Pinned,
    Results,
    NowPlaying,
    /// A playlist (or the liked videos) opened in the Library.
    Library,
}

impl List {
    /// The window's numbering: its tabs, then the now-playing card.
    pub fn from_index(index: i32) -> Option<Self> {
        Some(match index {
            0 => List::Queue,
            1 => List::History,
            2 => List::Pinned,
            3 => List::Results,
            4 => List::NowPlaying,
            5 => List::Library,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    PlayNow,
    PlayNext,
    AddToEnd,
    Radio,
    Pin,
    Unpin,
    Remove,
    CopyLink,
    OpenOnYouTube,
}

const ACTIONS: [Action; 9] = [
    Action::PlayNow,
    Action::PlayNext,
    Action::AddToEnd,
    Action::Radio,
    Action::Pin,
    Action::Unpin,
    Action::Remove,
    Action::CopyLink,
    Action::OpenOnYouTube,
];

impl Action {
    /// How the window names the action back.
    pub fn id(self) -> &'static str {
        match self {
            Action::PlayNow => "play",
            Action::PlayNext => "next",
            Action::AddToEnd => "add",
            Action::Radio => "radio",
            Action::Pin => "pin",
            Action::Unpin => "unpin",
            Action::Remove => "remove",
            Action::CopyLink => "copy",
            Action::OpenOnYouTube => "open",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        ACTIONS.into_iter().find(|a| a.id() == id)
    }

    fn label(self, list: List) -> &'static str {
        match self {
            Action::PlayNow => "Play now",
            Action::PlayNext => "Play next",
            Action::AddToEnd => "Add to end of queue",
            Action::Radio => "Start radio",
            Action::Pin => "Pin",
            Action::Unpin => "Unpin",
            Action::Remove if list == List::History => "Remove from history",
            Action::Remove => "Remove from queue",
            Action::CopyLink => "Copy link",
            Action::OpenOnYouTube => "Open on YouTube",
        }
    }
}

/// What's true of the track, for choosing what the menu offers.
#[derive(Clone, Copy, Debug, Default)]
pub struct Facts {
    /// It's the track playing (or about to resume).
    pub current: bool,
    /// It's in the queue.
    pub queued: bool,
    pub pinned: bool,
    /// It's a YouTube video, so it has a Mix.
    pub video: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub action: Action,
    pub label: &'static str,
    /// A line above it, between groups.
    pub separated: bool,
}

/// The menu for a track, in groups: playing it, keeping it, its link. Actions that don't
/// apply are left out rather than greyed.
pub fn items(list: List, facts: Facts) -> Vec<Item> {
    let play = [
        (!facts.current).then_some(Action::PlayNow),
        (!facts.current).then_some(Action::PlayNext),
        (!facts.queued).then_some(Action::AddToEnd),
        facts.video.then_some(Action::Radio),
    ];
    let keep = [
        Some(if facts.pinned { Action::Unpin } else { Action::Pin }),
        matches!(list, List::Queue | List::History).then_some(Action::Remove),
    ];
    let link = [Some(Action::CopyLink), Some(Action::OpenOnYouTube)];
    let mut out = Vec::new();
    for group in [&play[..], &keep[..], &link[..]] {
        let mut first = !out.is_empty();
        for action in group.iter().flatten() {
            out.push(Item { action: *action, label: action.label(list), separated: first });
            first = false;
        }
    }
    out
}

/// The menu for a whole playlist: playing it now, next or at the end, pinning it, its link.
pub fn playlist_items(pinned: bool) -> Vec<Item> {
    let groups: [&[Action]; 3] = [
        &[Action::PlayNow, Action::PlayNext, Action::AddToEnd],
        &[if pinned { Action::Unpin } else { Action::Pin }],
        &[Action::CopyLink, Action::OpenOnYouTube],
    ];
    let mut out = Vec::new();
    for group in groups {
        for (i, action) in group.iter().enumerate() {
            out.push(Item {
                action: *action,
                label: action.label(List::Library),
                separated: i == 0 && !out.is_empty(),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(list: List, facts: Facts) -> Vec<String> {
        items(list, facts)
            .into_iter()
            .map(|i| if i.separated { format!("| {}", i.label) } else { i.label.to_string() })
            .collect()
    }

    #[test]
    fn a_queued_track_offers_no_add_to_end() {
        let facts = Facts { queued: true, video: true, ..Facts::default() };
        assert_eq!(
            labels(List::Queue, facts),
            ["Play now", "Play next", "Start radio", "| Pin", "Remove from queue", "| Copy link", "Open on YouTube"]
        );
    }

    #[test]
    fn history_offers_everything_and_its_own_remove() {
        let facts = Facts { pinned: true, video: true, ..Facts::default() };
        assert_eq!(
            labels(List::History, facts),
            [
                "Play now",
                "Play next",
                "Add to end of queue",
                "Start radio",
                "| Unpin",
                "Remove from history",
                "| Copy link",
                "Open on YouTube"
            ]
        );
    }

    #[test]
    fn pinned_and_results_have_no_remove() {
        for list in [List::Pinned, List::Results] {
            let all = items(list, Facts { video: true, ..Facts::default() });
            assert!(all.iter().all(|i| i.action != Action::Remove), "{list:?}");
            assert_eq!(all.len(), 7);
        }
    }

    #[test]
    fn the_playing_track_offers_radio_but_not_play() {
        let facts = Facts { current: true, queued: true, video: true, ..Facts::default() };
        assert_eq!(labels(List::NowPlaying, facts), ["Start radio", "| Pin", "| Copy link", "Open on YouTube"]);
    }

    #[test]
    fn a_link_that_isnt_a_youtube_video_has_no_radio() {
        let all = items(List::Results, Facts::default());
        assert!(all.iter().all(|i| i.action != Action::Radio));
    }

    #[test]
    fn a_playlist_offers_play_queue_pin_and_its_link() {
        let labels: Vec<String> = playlist_items(false)
            .into_iter()
            .map(|i| if i.separated { format!("| {}", i.label) } else { i.label.to_string() })
            .collect();
        assert_eq!(labels, ["Play now", "Play next", "Add to end of queue", "| Pin", "| Copy link", "Open on YouTube"]);
        assert!(playlist_items(true).iter().any(|i| i.action == Action::Unpin));
    }

    #[test]
    fn actions_round_trip_through_their_ids() {
        for action in ACTIONS {
            assert_eq!(Action::from_id(action.id()), Some(action));
        }
        assert_eq!(Action::from_id("nonsense"), None);
    }
}
