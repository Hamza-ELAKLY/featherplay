# Privacy policy

Featherplay is a desktop app. It has no server, no account of its own, no analytics and
no telemetry. This page says what it reads, where that stays, and how to remove it.

## Without signing in

Featherplay talks only to YouTube (through yt-dlp, to find a link's audio, a playlist's
tracks or search results) and to GitHub (to check for new versions of Featherplay each
time it opens and of yt-dlp once a week; a switch in Settings turns these checks off).
Your queue, pins, history and settings are saved in the app's own folder on your PC and
never sent anywhere.

## Signing in with Google

Signing in is optional; it's only needed for the Library tab.

- **What's accessed:** your YouTube account, view only (the `youtube.readonly` scope):
  your channel's name and picture, your liked videos, and your playlists with their
  tracks. Featherplay can't change, upload, like, comment or delete anything.
- **What it's used for:** only to show your liked videos and playlists in the app and
  play them. Featherplay's use of information received from Google APIs adheres to the
  [Google API Services User Data Policy](https://developers.google.com/terms/api-services-user-data-policy),
  including the Limited Use requirements.
- **Where it's kept:** only on your PC. The sign-in (Google's token) and the copy of
  your library are encrypted for your Windows account. Nothing goes to a Featherplay
  server (there is none), and nothing is shared with, sold to or used by anyone else, or
  used for advertising.
- **How long:** until you sign out. A copy of your library is refreshed when you open
  the Library and deleted when you sign out.
- **Pinned playlists:** the name and picture link of a playlist you pin are saved with
  your other pins, unencrypted, so it still shows after you sign out. Unpin it to remove
  it.

## Removing access and data

- **Sign out** in Settings: Featherplay asks Google to revoke its access and deletes the
  token and the library copy from your PC. If you're offline then, it keeps the token
  (still encrypted) only to revoke it the next time it starts.
- You can also remove Featherplay at any time from your Google account:
  [Third-party connections](https://myaccount.google.com/connections).
- Deleting Featherplay's folder removes everything it saved.

## YouTube and Google

Featherplay uses YouTube API Services. By using Featherplay you agree to the
[YouTube Terms of Service](https://www.youtube.com/t/terms). Google's handling of your
data is described in the [Google Privacy Policy](https://policies.google.com/privacy).

## Contact

Questions about this policy: email
[featherplay-support@googlegroups.com](mailto:featherplay-support@googlegroups.com), or
open a [discussion](https://github.com/Hamza-ELAKLY/featherplay/discussions) or an
[issue](https://github.com/Hamza-ELAKLY/featherplay/issues).

## Changes

This policy changes with the app; its history is in this repository. Last updated:
20 November 2026.
