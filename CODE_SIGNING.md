# Code signing policy

**Status:** Featherplay's releases aren't code-signed yet. This policy takes effect once
SignPath Foundation accepts the project; until then, Windows may warn on first run.

Free code signing provided by [SignPath.io](https://about.signpath.io), certificate by
[SignPath Foundation](https://signpath.org).

## What is signed

- `featherplay.exe`, in each release on the
  [releases page](https://github.com/Hamza-ELAKLY/featherplay/releases), both on its own
  (for the in-app updater) and inside `Featherplay-<version>-windows-x64.zip`.
- Only Featherplay's own program is signed. The zip also contains `yt-dlp.exe`, which is
  [yt-dlp](https://github.com/yt-dlp/yt-dlp)'s own release, included unchanged and not
  signed by this project.

Every signed file is built by the [release workflow](.github/workflows/release.yml) on
GitHub's own runners, from the tagged commit in
[Featherplay's repository](https://github.com/Hamza-ELAKLY/featherplay), and each signing
request is approved by hand. Featherplay's file properties name the product
"Featherplay" and carry the release's version.

## Team and roles

- Committers and reviewers: [Hamza-ELAKLY](https://github.com/Hamza-ELAKLY)
- Approvers: [Hamza-ELAKLY](https://github.com/Hamza-ELAKLY)

Changes from outside contributors are reviewed before they are merged.

## Privacy

Featherplay has no server, no analytics and no telemetry. It connects to YouTube to play
what you ask for, to GitHub to check for new versions of itself (each time it opens) and
of yt-dlp (once a week), and to Google only if you sign in. A switch in Settings turns
the update checks off. See the [privacy policy](PRIVACY.md).

## Removing it

Featherplay installs nothing: delete its folder to remove it (and `%APPDATA%\featherplay`
if it saved its data there because its own folder wasn't writable). If you signed in,
sign out first (Settings), or remove Featherplay from your Google account's
[third-party connections](https://myaccount.google.com/connections).
