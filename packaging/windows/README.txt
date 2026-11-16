Featherplay {version}
A light player for the audio of YouTube videos and playlists.

GETTING STARTED

1. Keep these files together in one folder, anywhere you like (Desktop, Documents,
   a USB stick). Nothing needs installing.
2. Double-click featherplay.exe.
3. Paste a YouTube link (a video, a playlist or a Mix) into the box at the top:
   Enter plays it now (your queue carries on after it), Shift+Enter or the + button
   adds it to the end of the queue. Or type words and press Enter to search.

The first time, Windows may say "Windows protected your PC". That's because
Featherplay isn't code-signed yet; getting it signed is under way. Until then, click
"More info", then "Run anyway". You only see this once.

WHAT'S IN THE FOLDER

  featherplay.exe           the player; it looks for a new version once a day,
                            and Settings > Update installs it
  yt-dlp.exe                finds the audio stream of a link; Featherplay keeps it
                            up to date by itself, about once a week
  data\                     your queue, history, pins and settings
  LICENSE.txt               Featherplay's licence (GPL-3.0-or-later)
  THIRD-PARTY-NOTICES.txt   the licences of the software Featherplay is built on
  YT-DLP-THIRD-PARTY-LICENSES.txt
                            the licences of the libraries packed inside yt-dlp.exe

To move Featherplay, move the whole folder. To remove it, delete the folder:
it writes nothing anywhere else.

USING IT

  Click a track             play it
  Drag a track              move it in the queue or the pinned list
  Pin                       keep a track in the Pinned tab, to play again quickly
  Space                     play or pause
  Left / Right              back or forward 5 seconds
  Ctrl+Left / Ctrl+Right    previous or next track
  Up / Down                 volume
  M                         mute
  Ctrl+L or Ctrl+F          go to the search box
  Esc                       close the settings

Closing the window keeps the music playing in the tray (by the clock). Right-click
the tray icon to quit. Media keys work too.

GOOD TO KNOW

- Only public and unlisted playlists can be opened; private ones can't.
- A Mix (radio) link loads its first 100 tracks.
- A track that won't load is retried; one that's really gone (removed, private,
  age-restricted) is marked "Unavailable" and skipped.

This is an early test version. Featherplay is not affiliated with YouTube or Google.
{repository}
