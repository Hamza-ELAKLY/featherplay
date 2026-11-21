# Changelog

## Unreleased

- The Queue keeps the track playing in view: it opens at it, and follows the music
  while you're following it; scrolled elsewhere, it stays put and a Now playing button
  at the list's edge takes you back

## 1.0.0 — first public release

Sign-in with Google is in its testing period: until Google finishes reviewing
Featherplay, only accounts added as testers can sign in. Everything else works for
everyone.

Featherplay is now released under the GNU General Public License, version 3 or later
(it was MIT while private).

- Pinned playlists sit in a row of covers at the top of Pinned that slides with its
  arrows or the mouse wheel; Pinned tracks keep their room below
- Lists sit on a panel with a border, and section headings have a line after them;
  the Library has an account header with Refresh, and an opened playlist shows its
  cover with Pin, Shuffle and Play all buttons
- Search results keep coming as you scroll: 20 at first, then 20 more near the end of
  the list, up to 200
- Updates are signed: the app installs one only if the release's checksums carry a
  valid signature from the project's release key
- When tracks keep failing to load, yt-dlp is updated at once instead of waiting for
  the weekly check
- Screen readers can read and use the window (Windows UI Automation)
- Changing theme wipes the new colours across the window diagonally: from the bottom
  right when going dark, from the top left when going light
- Right-click a track (or the now-playing card) for its menu: Play now, Play next, Add
  to end of queue, Start radio, Pin, Remove, Copy link, Open on YouTube
- Play next puts a track right after the one playing; Start radio plays a song's
  YouTube Mix next, before the rest of the queue
- Lists work from the keyboard: Tab to a list, arrows to move, Enter to play,
  Shift+Enter to queue, Delete to remove, the menu key or Shift+F10 for the menu
- While a list is scrolled down, the now-playing card shrinks to a slim header
- Click the track's length under the waveform to show the time left instead
- Shorter messages that never get cut off; the details of what went wrong are kept in
  `data\featherplay.log`
- The faint grey text is easier to read in both themes (WCAG AA contrast)
- Sign in with Google (view-only), in Settings: Google's own page in the browser, and
  the sign-in kept encrypted for the Windows account (not offered on Linux yet)
- A Library tab: your liked videos and playlists, private ones included, each marked
  Private, Unlisted or Public. Open one to see its tracks, or play, shuffle or queue it
  straight from its row. The last-seen library shows at once and refreshes quietly
- Pasted links to your own private playlists open when signed in (yt-dlp still opens
  public and unlisted ones, with no use of Google's quota)
- Whole playlists can be pinned (from a Library row, its menu or its header), and show
  first in Pinned; playlists have a right-click menu too: Play now, Play next, Add to
  end of queue, Pin, Copy link, Open on YouTube
- Private and deleted videos in a playlist are listed as "can't be played" and skipped

Fixes:

- Tracks from a Library playlist (whose length YouTube's list doesn't give) showed no
  waveform or times while playing; the length is now filled in as the track starts
- On a second screen with a different scale (100 % beside a 125 % main screen), the
  window shrank every time Featherplay started; it now keeps its size
- Seeking after YouTube refused a track's stream link gets a fresh link instead of failing

## 0.1.2 — its own title bar, and a tidier search box

- Featherplay draws its own title bar, in its own colours; the window still moves,
  snaps and resizes as usual
- The hint under the search box (Enter plays now, Shift+Enter or + adds to the queue)
  has its own space, so it never covers the now-playing card; it shows only for links
- The first run opens at a compact 485 × 775; after that the window keeps its size
- In a short window, the empty-list message no longer runs over the footer

## 0.1.1 — self-update, and a safer Enter

- Playing a pasted link (Enter) no longer clears the queue: it plays right after the
  current track, and the rest of the queue follows it
- Shift+Enter adds a link to the end of the queue, like the + button, which now has a
  tooltip; while something is typed, a hint under the search box says what each does
- The message after opening a link says where its tracks went
- Featherplay updates itself: once a day it looks for a newer stable release on GitHub;
  Settings shows it, and Update downloads it, checks it against the release's
  checksums, swaps it in and restarts, carrying on with the queue (and playing, if it was)
- The hint is a small floating card with keycaps; messages show over the settings
  instead of behind them; the last cover fades away when the queue is cleared; Slint's
  logo in About keeps a modest size

## 0.1.0 — first test release (Windows)

A portable Windows build for testing: unzip and run, nothing to install.

- Play the audio of YouTube videos, playlists and Mixes from a pasted link, or search
- A queue with repeat, shuffle, drag-to-reorder, and a video from a playlist starting at once
- Pinned tracks and history, with thumbnails
- A waveform seek bar measured from the whole track, kept for next time
- Soft pause, resume and track changes; a playhead that glides between pixels
- Picks up where it left off; media keys, the Windows media overlay and a tray icon
- Light and dark themes, or follow the system
- Tracks that fail to load are retried; only truly unavailable ones are skipped
- yt-dlp is bundled and keeps itself up to date
- Around 35–45 MB of memory while playing

Known limits: private playlists can't be opened; a Mix loads its first 100 tracks; the
executable isn't code-signed, so Windows SmartScreen asks once before it runs.
